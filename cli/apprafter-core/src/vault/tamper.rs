// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The tamper suite (D.4a T30; crypto brief §13) on the frozen fast-params vector: every bit of
//! the file flipped, every truncation, slots swapped, removed, transplanted and duplicated, a
//! planted Argon2 cost, and authenticated payloads that break a payload rule. No change ever
//! opens the vault, none reads as an absent secret, and each lands on the refusal its region
//! calls for.

use std::ops::Range;
use std::time::{Duration, Instant};

use chacha20poly1305::aead::{AeadInPlace, KeyInit};
use chacha20poly1305::{Key, XChaCha20Poly1305, XNonce};

use super::error::{MalformedWhat, PayloadWhat};
use super::kdf::{KdfPolicy, DERIVATIONS};
use super::keys::SlotSecret;
use super::seal::UnlockedVault;
use super::test_alloc;
use super::testutil::{seq, seq_array, tlv, unhex, vector_path};
use super::{
    MachineId, RecoveryKey, SlotBody, SlotId, SlotKind, SlotRecord, VaultError, VaultFile,
    VaultHeader, VaultResult,
};
use crate::context::SecretString;

const FAST: &KdfPolicy = &KdfPolicy::INSECURE_FAST;

/// Byte ranges of `fast-params/vault` (writer 22 bytes, store id 16, labels 13 and 12).
const MAGIC: Range<usize> = 0..8;
const VERSION: Range<usize> = 8..10;
const WRITER: Range<usize> = 11..33;
const FLAGS: Range<usize> = 33..35;
const VAULT_ID: Range<usize> = 35..51;
const STORE_ID: Range<usize> = 52..68;
const SLOT1: Range<usize> = 69..237;
const S1_ID: Range<usize> = 71..87;
const S1_KIND: usize = 87;
const S1_KDF_ID: usize = 90;
const S1_M_COST: Range<usize> = 91..95;
const S1_T_P: Range<usize> = 95..103;
const S1_SALT: Range<usize> = 103..119;
const S1_NONCE: Range<usize> = 119..143;
const S1_CHECK: Range<usize> = 143..175;
const S1_WRAPPED: Range<usize> = 175..223;
const S1_LABEL: Range<usize> = 224..237;
const SLOT2: Range<usize> = 237..392;
const S2_ID: Range<usize> = 239..255;
const S2_KIND: usize = 255;
const S2_KDF_ID: usize = 258;
const S2_SALT: Range<usize> = 259..275;
const S2_NONCE: Range<usize> = 275..299;
const S2_CHECK: Range<usize> = 299..331;
const S2_WRAPPED: Range<usize> = 331..379;
const S2_LABEL: Range<usize> = 380..392;
const PAYLOAD_NONCE: Range<usize> = 392..416;
const PAYLOAD_LEN: Range<usize> = 416..420;
const PAYLOAD_CT: Range<usize> = 420..748;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Seen {
    NotAVault,
    Unsupported,
    Malformed,
    WrongSecret,
    SlotCorrupt,
    Tampered,
    NoUsableSlot,
    KdfTooWeak,
    KdfOutOfRange,
    PayloadInvalid,
}

fn seen(result: VaultResult<UnlockedVault>) -> Seen {
    match result {
        Ok(_) => panic!("a changed vault opened"),
        Err(VaultError::NotAVault) => Seen::NotAVault,
        Err(VaultError::UnsupportedVersion { .. }) => Seen::Unsupported,
        Err(VaultError::Malformed { .. }) => Seen::Malformed,
        Err(VaultError::WrongSecret { .. }) => Seen::WrongSecret,
        Err(VaultError::SlotCorrupt { .. }) => Seen::SlotCorrupt,
        Err(VaultError::Tampered) => Seen::Tampered,
        Err(VaultError::NoUsableSlot { .. }) => Seen::NoUsableSlot,
        Err(VaultError::KdfTooWeak { .. }) => Seen::KdfTooWeak,
        Err(VaultError::KdfOutOfRange { .. }) => Seen::KdfOutOfRange,
        Err(VaultError::PayloadInvalid { .. }) => Seen::PayloadInvalid,
        Err(other) => panic!("an outcome no change should have: {other:?}"),
    }
}

fn open_as(bytes: &[u8], secret: &SlotSecret, policy: &KdfPolicy) -> Seen {
    match VaultFile::parse(bytes) {
        Err(e) => seen(Err(e)),
        Ok(file) => seen(file.open(secret, policy)),
    }
}

fn vector() -> Vec<u8> {
    std::fs::read(vector_path("fast-params/vault"))
        .expect("tests/vault_vectors/v1/fast-params/vault")
}

fn password() -> SlotSecret {
    SlotSecret::Password(SecretString::new("correct horse battery staple"))
}

fn recovery() -> SlotSecret {
    SlotSecret::RecoveryKey(
        RecoveryKey::generate(&mut super::ScriptedRandom::new(vec![seq(0x00, 32)])).unwrap(),
    )
}

/// What a one-bit change at `at` may give, opened with the password and with the recovery key.
fn allowed(at: usize) -> (&'static [Seen], &'static [Seen]) {
    use Seen::*;
    let within = |r: &Range<usize>| r.contains(&at);
    match at {
        _ if within(&MAGIC) => (&[NotAVault], &[NotAVault]),
        _ if within(&VERSION) => (&[Unsupported], &[Unsupported]),
        _ if within(&FLAGS) => (&[Malformed], &[Malformed]),
        // In each slot's associated data: its key check still passes, its unwrap fails.
        _ if within(&VAULT_ID) => (&[SlotCorrupt], &[SlotCorrupt]),
        _ if within(&S1_ID) => (&[SlotCorrupt], &[Tampered]),
        _ if within(&S2_ID) => (&[Tampered], &[SlotCorrupt]),
        // Printable or not: authenticated by the payload, refused by the parser.
        _ if within(&WRITER) || within(&STORE_ID) || within(&S1_LABEL) || within(&S2_LABEL) => {
            (&[Malformed, Tampered], &[Malformed, Tampered])
        }
        // A kind byte this build does not open makes the slot opaque.
        S1_KIND => (&[NoUsableSlot], &[Tampered]),
        S2_KIND => (&[Tampered], &[NoUsableSlot]),
        S1_KDF_ID | S2_KDF_ID => (&[Malformed], &[Malformed]),
        // A changed cost changes the key, or is out of range; the recovery slot's payload sees it.
        _ if within(&S1_M_COST) || within(&S1_T_P) => {
            (&[WrongSecret, KdfOutOfRange, KdfTooWeak], &[Tampered])
        }
        _ if within(&S1_SALT) || within(&S1_CHECK) => (&[WrongSecret], &[Tampered]),
        _ if within(&S1_NONCE) || within(&S1_WRAPPED) => (&[SlotCorrupt], &[Tampered]),
        _ if within(&S2_SALT) || within(&S2_CHECK) => (&[Tampered], &[WrongSecret]),
        _ if within(&S2_NONCE) || within(&S2_WRAPPED) => (&[Tampered], &[SlotCorrupt]),
        _ if within(&PAYLOAD_NONCE) || within(&PAYLOAD_CT) => (&[Tampered], &[Tampered]),
        _ if within(&PAYLOAD_LEN) => (&[Malformed], &[Malformed]),
        // Every length and count: the layout breaks.
        _ => (&[Malformed], &[Malformed]),
    }
}

#[test]
fn every_bit_flipped_is_refused_as_its_region_says() {
    let original = vector();
    assert_eq!(original.len(), PAYLOAD_CT.end);
    let (password, recovery) = (password(), recovery());
    assert!(VaultFile::parse(&original)
        .unwrap()
        .open(&password, FAST)
        .is_ok());
    for at in 0..original.len() {
        let (by_password, by_recovery) = allowed(at);
        for bit in 0..8 {
            let mut bytes = original.clone();
            bytes[at] ^= 1 << bit;
            // A flipped high bit of m asks for up to 2 GiB of Argon2 memory, which the policy
            // allows: the low bits show the password slot's refusal, the recovery slot's
            // payload check shows every one.
            let m_cost = u32::from_le_bytes(bytes[S1_M_COST].try_into().unwrap());
            if !(S1_M_COST.contains(&at) && m_cost > 4096) {
                let got = open_as(&bytes, &password, FAST);
                assert!(
                    by_password.contains(&got),
                    "byte {at} bit {bit}, password: {got:?}"
                );
            }
            let got = open_as(&bytes, &recovery, FAST);
            assert!(
                by_recovery.contains(&got),
                "byte {at} bit {bit}, recovery key: {got:?}"
            );
        }
    }
}

#[test]
fn every_truncation_is_refused_and_one_more_byte_too() {
    let original = vector();
    for len in 0..original.len() {
        let got = open_as(&original[..len], &recovery(), FAST);
        let expected = if len < MAGIC.end {
            Seen::NotAVault
        } else {
            Seen::Malformed
        };
        assert_eq!(got, expected, "length {len}");
    }
    let mut longer = original.clone();
    longer.push(0);
    assert!(matches!(
        VaultFile::parse(&longer),
        Err(VaultError::Malformed {
            what: MalformedWhat::TrailingBytes,
            ..
        })
    ));
}

#[test]
fn swapped_slots_still_unwrap_and_the_payload_sees_the_order() {
    let v = vector();
    let swapped = [
        &v[..SLOT1.start],
        &v[SLOT2.clone()],
        &v[SLOT1.clone()],
        &v[SLOT2.end..],
    ]
    .concat();
    assert_eq!(open_as(&swapped, &password(), FAST), Seen::Tampered);
    assert_eq!(open_as(&swapped, &recovery(), FAST), Seen::Tampered);
}

#[test]
fn a_silently_removed_recovery_slot_is_detected() {
    let v = vector();
    let mut removed = [&v[..SLOT2.start], &v[SLOT2.end..]].concat();
    removed[SLOT1.start - 1] = 1;
    assert_eq!(open_as(&removed, &password(), FAST), Seen::Tampered);
    assert_eq!(open_as(&removed, &recovery(), FAST), Seen::NoUsableSlot);
}

#[test]
fn a_duplicated_slot_id_is_malformed() {
    let mut v = vector();
    let id = v[S1_ID].to_vec();
    v[S2_ID].copy_from_slice(&id);
    assert!(matches!(
        VaultFile::parse(&v),
        Err(VaultError::Malformed {
            what: MalformedWhat::DuplicateSlotId,
            ..
        })
    ));
}

#[test]
fn a_vault_from_another_store_is_store_mismatch() {
    let file = VaultFile::parse(&vector()).unwrap();
    assert!(file.header().check_store_id("0123456789abcdef").is_ok());
    assert!(matches!(
        file.header().check_store_id("fedcba9876543210"),
        Err(VaultError::StoreMismatch { .. })
    ));
}

#[test]
fn a_planted_memory_cost_is_refused_at_once() {
    let mut v = vector();
    v[S1_M_COST].copy_from_slice(&u32::MAX.to_le_bytes());
    let file = VaultFile::parse(&v).unwrap();
    let derived = DERIVATIONS.with(std::cell::Cell::get);
    let mut fastest = Duration::MAX;
    for policy in [&KdfPolicy::RELEASE, FAST] {
        test_alloc::reset_largest();
        let start = Instant::now();
        assert_eq!(seen(file.open(&password(), policy)), Seen::KdfOutOfRange);
        fastest = fastest.min(start.elapsed());
        assert!(
            test_alloc::largest() < 1024 * 1024,
            "{} bytes asked for",
            test_alloc::largest()
        );
    }
    assert!(fastest < Duration::from_millis(10), "took {fastest:?}");
    assert_eq!(DERIVATIONS.with(std::cell::Cell::get), derived);
    assert_eq!(seen(file.open(&recovery(), FAST)), Seen::Tampered);
}

/// The vector re-sealed under its own data key (bytes `40 … 5f`) around `plain`.
fn with_plaintext(plain: &[u8]) -> Vec<u8> {
    let v = vector();
    let mut out = v[..PAYLOAD_NONCE.start].to_vec();
    out.extend_from_slice(&seq(0xe0, 24));
    out.extend_from_slice(&((plain.len() + 16) as u32).to_le_bytes());
    let mut buf = plain.to_vec();
    let key: [u8; 32] = seq_array(0x40);
    let tag = XChaCha20Poly1305::new(Key::from_slice(&key))
        .encrypt_in_place_detached(XNonce::from_slice(&seq(0xe0, 24)), &out, &mut buf)
        .unwrap();
    out.extend_from_slice(&buf);
    out.extend_from_slice(&tag);
    out
}

#[test]
fn an_authenticated_payload_that_breaks_a_rule_is_payload_invalid() {
    const ID: &str = "AGE-SECRET-KEY-1YQSJYGEYY5NZW2PF9G4JCTFW9UCRZV3NXS6NVDEC8YARK0PA8CLSWXGG7Q";
    let id = ("age-identity", ID.as_bytes());
    // The helper is right: the vector's own entries re-sealed open.
    let fine = with_plaintext(&tlv(&[id, ("settings/unlock-timeout-min", &b"15"[..])]));
    assert!(VaultFile::parse(&fine)
        .unwrap()
        .open(&recovery(), FAST)
        .is_ok());
    for (plain, what) in [
        (tlv(&[("b", &b"v"[..]), id]), PayloadWhat::KeyOrder),
        (tlv(&[id, id]), PayloadWhat::KeyOrder),
        (tlv(&[("x", &b"v"[..])]), PayloadWhat::AgeIdentityMissing),
        (
            tlv(&[id, ("settings/unlock-timeout-min", &b"1441"[..])]),
            PayloadWhat::UnlockTimeout,
        ),
        (tlv(&[]), PayloadWhat::EntryCount),
    ] {
        let file = VaultFile::parse(&with_plaintext(&plain)).unwrap();
        match file.open(&recovery(), FAST) {
            Err(VaultError::PayloadInvalid { what: w }) => assert_eq!(w, what),
            other => panic!("{what:?}: {:?}", other.map(|_| "opened")),
        }
    }
}

// ---------------------------------------------------------------------
// The account-password slot (D.4b-b1; B1, B54, B77) on `os-password/vault`
// ---------------------------------------------------------------------

const ACCOUNT_PASSWORD: &str = "hunter2 is not a password";
const MACHINE_LINUX: &[u8] = b"5f0d6a3e0c2b4f6a8d9e1b2c3d4e5f60";
const MACHINE_MAC: &str = "3c1e9a7b2d4f4e8a9b0c1d2e3f405162";

/// Byte ranges of `os-password/vault` (writer 22 bytes, store id 16; Linux's account-password
/// slot, the Mac's, the recovery slot; labels 35, 35 and 12 bytes).
const OS_SLOT_L: Range<usize> = 69..275;
const OS_L_ID: Range<usize> = 71..87;
const OS_L_KIND: usize = 87;
const OS_L_KDF_ID: usize = 90;
const OS_L_M_COST: Range<usize> = 91..95;
const OS_L_T_P: Range<usize> = 95..103;
const OS_L_SALT: Range<usize> = 103..119;
const OS_L_ENROLLED: Range<usize> = 119..127;
const OS_L_TAG: Range<usize> = 127..135;
const OS_L_NONCE: Range<usize> = 135..159;
const OS_L_CHECK: Range<usize> = 159..191;
const OS_L_WRAPPED: Range<usize> = 191..239;
const OS_L_LABEL: Range<usize> = 240..275;
const OS_M_ID: Range<usize> = 277..293;
const OS_M_KIND: usize = 293;
const OS_M_KDF_ID: usize = 296;
const OS_M_M_COST: Range<usize> = 297..301;
const OS_M_T_P: Range<usize> = 301..309;
const OS_M_SALT: Range<usize> = 309..325;
const OS_M_ENROLLED: Range<usize> = 325..333;
const OS_M_TAG: Range<usize> = 333..341;
const OS_M_NONCE: Range<usize> = 341..365;
const OS_M_CHECK: Range<usize> = 365..397;
const OS_M_WRAPPED: Range<usize> = 397..445;
const OS_M_LABEL: Range<usize> = 446..481;
const OS_R_ID: Range<usize> = 483..499;
const OS_R_KIND: usize = 499;
const OS_R_KDF_ID: usize = 502;
const OS_R_SALT: Range<usize> = 503..519;
const OS_R_NONCE: Range<usize> = 519..543;
const OS_R_CHECK: Range<usize> = 543..575;
const OS_R_WRAPPED: Range<usize> = 575..623;
const OS_R_LABEL: Range<usize> = 624..636;
const OS_PAYLOAD_NONCE: Range<usize> = 636..660;
const OS_PAYLOAD_LEN: Range<usize> = 660..664;
const OS_PAYLOAD_CT: Range<usize> = 664..992;

fn os_vector() -> Vec<u8> {
    std::fs::read(vector_path("os-password/vault"))
        .expect("tests/vault_vectors/v1/os-password/vault")
}

fn linux() -> MachineId {
    MachineId::new(MACHINE_LINUX).unwrap()
}

fn mac() -> MachineId {
    MachineId::new(&unhex(MACHINE_MAC)).unwrap()
}

fn account(machine: MachineId) -> SlotSecret {
    SlotSecret::AccountPassword(SecretString::new(ACCOUNT_PASSWORD), machine)
}

/// The vector's payload plaintext, as its recovery key opens it.
fn os_plaintext() -> Vec<u8> {
    VaultFile::parse(&os_vector())
        .unwrap()
        .open(&recovery(), FAST)
        .unwrap()
        .payload()
        .encode()
        .unwrap()
        .to_vec()
}

/// `header` sealed around `plain` under the vectors' data key (`40 … 5f`) and payload nonce
/// (`e0 … f7`): a vault whose header a test changed, its payload authentic for that header.
fn resealed(header: &VaultHeader, plain: &[u8]) -> Vec<u8> {
    let nonce: [u8; 24] = seq_array(0xe0);
    let mut out = header.encode_aad(&nonce, plain.len() + 16).unwrap();
    let mut buf = plain.to_vec();
    let key: [u8; 32] = seq_array(0x40);
    let tag = XChaCha20Poly1305::new(Key::from_slice(&key))
        .encrypt_in_place_detached(XNonce::from_slice(&nonce), &out, &mut buf)
        .unwrap();
    out.extend_from_slice(&buf);
    out.extend_from_slice(&tag);
    out
}

/// The same outcomes for all three secrets.
fn all(seen: &'static [Seen]) -> [&'static [Seen]; 3] {
    [seen; 3]
}

/// What a one-bit change at `at` of `os-password/vault` may give, opened with Linux's account
/// password, the Mac's and the recovery key.
fn os_allowed(at: usize) -> [&'static [Seen]; 3] {
    use Seen::*;
    const T: &[Seen] = &[Seen::Tampered];
    let within = |r: &Range<usize>| r.contains(&at);
    match at {
        _ if within(&MAGIC) => all(&[NotAVault]),
        _ if within(&VERSION) => all(&[Unsupported]),
        _ if within(&FLAGS) => all(&[Malformed]),
        // In every slot's associated data: each key check passes, each unwrap fails.
        _ if within(&VAULT_ID) => all(&[SlotCorrupt]),
        _ if within(&WRITER)
            || within(&STORE_ID)
            || within(&OS_L_LABEL)
            || within(&OS_M_LABEL)
            || within(&OS_R_LABEL) =>
        {
            all(&[Malformed, Tampered])
        }
        // A slot id or tag that is not this computer's for the slot: the slot is not tried.
        _ if within(&OS_L_ID) || within(&OS_L_TAG) => [&[NoUsableSlot], T, T],
        _ if within(&OS_M_ID) || within(&OS_M_TAG) => [T, &[NoUsableSlot], T],
        // 0x01 / 0x02 parse strictly and refuse the length; any other kind keeps it opaque.
        OS_L_KIND => [
            &[Malformed, NoUsableSlot],
            &[Malformed, Tampered],
            &[Malformed, Tampered],
        ],
        OS_M_KIND => [
            &[Malformed, Tampered],
            &[Malformed, NoUsableSlot],
            &[Malformed, Tampered],
        ],
        // Another key-derivation id: a newer layout, kept opaque (B1, newer_kind).
        OS_L_KDF_ID => [&[NoUsableSlot], T, T],
        OS_M_KDF_ID => [T, &[NoUsableSlot], T],
        _ if within(&OS_L_M_COST) || within(&OS_L_T_P) => {
            [&[WrongSecret, KdfOutOfRange, KdfTooWeak], T, T]
        }
        _ if within(&OS_M_M_COST) || within(&OS_M_T_P) => {
            [T, &[WrongSecret, KdfOutOfRange, KdfTooWeak], T]
        }
        _ if within(&OS_L_SALT) || within(&OS_L_CHECK) => [&[WrongSecret], T, T],
        _ if within(&OS_M_SALT) || within(&OS_M_CHECK) => [T, &[WrongSecret], T],
        // `enrolled_at` is bound by the wrap's associated data, not by the key (B54).
        _ if within(&OS_L_ENROLLED) || within(&OS_L_NONCE) || within(&OS_L_WRAPPED) => {
            [&[SlotCorrupt], T, T]
        }
        _ if within(&OS_M_ENROLLED) || within(&OS_M_NONCE) || within(&OS_M_WRAPPED) => {
            [T, &[SlotCorrupt], T]
        }
        _ if within(&OS_R_ID) => [T, T, &[SlotCorrupt]],
        OS_R_KIND => [T, T, &[NoUsableSlot]],
        OS_R_KDF_ID => all(&[Malformed]),
        _ if within(&OS_R_SALT) || within(&OS_R_CHECK) => [T, T, &[WrongSecret]],
        _ if within(&OS_R_NONCE) || within(&OS_R_WRAPPED) => [T, T, &[SlotCorrupt]],
        _ if within(&OS_PAYLOAD_NONCE) || within(&OS_PAYLOAD_CT) => all(T),
        _ if within(&OS_PAYLOAD_LEN) => all(&[Malformed]),
        // Every length and count: the layout breaks.
        _ => all(&[Malformed]),
    }
}

#[test]
fn every_bit_of_an_account_password_vault_flipped_is_refused_as_its_region_says() {
    let original = os_vector();
    assert_eq!(original.len(), OS_PAYLOAD_CT.end);
    assert_eq!(
        (
            original[OS_L_KIND],
            original[OS_M_KIND],
            original[OS_R_KIND]
        ),
        (0x03, 0x03, 0x02)
    );
    assert_eq!(
        &original[OS_L_LABEL],
        b"account password of walker on box-a"
    );
    assert_eq!(
        &original[OS_M_LABEL],
        b"account password of walker on box-b"
    );
    assert_eq!(&original[OS_R_LABEL], b"recovery key");
    assert_eq!(
        &original[OS_SLOT_L.end..OS_SLOT_L.end + 2],
        &204u16.to_le_bytes()
    );
    let secrets = [account(linux()), account(mac()), recovery()];
    let file = VaultFile::parse(&original).unwrap();
    for secret in &secrets {
        assert!(
            file.open(secret, FAST).is_ok(),
            "{secret:?} opens the vector"
        );
    }
    for at in 0..original.len() {
        let allowed = os_allowed(at);
        for bit in 0..8 {
            let mut bytes = original.clone();
            bytes[at] ^= 1 << bit;
            let m_cost =
                |r: &Range<usize>| u32::from_le_bytes(bytes[r.clone()].try_into().unwrap());
            for (who, secret) in secrets.iter().enumerate() {
                // A flipped high bit of a slot's m asks Argon2 for up to 2 GiB, which the
                // policy allows: that slot's own secret skips it; the others' payload checks
                // see every one.
                let huge = (who == 0 && OS_L_M_COST.contains(&at) && m_cost(&OS_L_M_COST) > 4096)
                    || (who == 1 && OS_M_M_COST.contains(&at) && m_cost(&OS_M_M_COST) > 4096);
                if huge {
                    continue;
                }
                let got = open_as(&bytes, secret, FAST);
                assert!(
                    allowed[who].contains(&got),
                    "byte {at} bit {bit}, secret {who}: {got:?}"
                );
            }
        }
    }
}

#[test]
fn every_truncation_of_an_account_password_vault_is_refused() {
    let original = os_vector();
    for len in 0..original.len() {
        let expected = if len < MAGIC.end {
            Seen::NotAVault
        } else {
            Seen::Malformed
        };
        assert_eq!(
            open_as(&original[..len], &recovery(), FAST),
            expected,
            "length {len}"
        );
    }
}

/// B1: a `0x03` body this build cannot read — 148 or 150 bytes, another key-derivation id — and
/// an unread `0x04` body are kept byte for byte, never tried, and refuse only their slot: the
/// other computer's slot and the recovery key open the vault.
#[test]
fn an_unread_body_is_kept_and_refuses_only_its_slot() {
    let original = os_vector();
    let file = VaultFile::parse(&original).unwrap();
    let plain = os_plaintext();
    let mut other_kdf = original[OS_L_KDF_ID..OS_L_WRAPPED.end].to_vec();
    other_kdf[0] = 0x02;
    let hello = SlotRecord {
        id: SlotId([0x44; 16]),
        kind: SlotKind::WindowsHello,
        body: SlotBody::Opaque(seq(0x01, 193)),
        label: "Windows Hello on BOX-C".into(),
    };
    for body in [seq(0x01, 148), seq(0x01, 150), other_kdf] {
        let mut header = file.header().clone();
        header.slots[0].body = SlotBody::Opaque(body.clone());
        header.slots.push(hello.clone());
        let bytes = resealed(&header, &plain);
        let parsed = VaultFile::parse(&bytes).unwrap();
        assert_eq!(
            parsed.header().slots[0].body,
            SlotBody::Opaque(body),
            "kept as read"
        );
        assert_eq!(parsed.header().slots[3], hello);
        assert_eq!(
            parsed.to_bytes().unwrap(),
            bytes,
            "written back byte for byte"
        );
        let derived = DERIVATIONS.with(std::cell::Cell::get);
        assert_eq!(
            seen(parsed.open(&account(linux()), FAST)),
            Seen::NoUsableSlot
        );
        assert_eq!(
            DERIVATIONS.with(std::cell::Cell::get),
            derived,
            "an unread body is never tried"
        );
        assert!(
            parsed.open(&account(mac()), FAST).is_ok(),
            "the other computer's opens"
        );
        assert!(
            parsed.open(&recovery(), FAST).is_ok(),
            "the recovery key opens"
        );
    }
}

/// A slot is bound to its vault: created-with's account-password slot (the same password, salt
/// and machine) moved into os-password passes its key check and does not unwrap.
#[test]
fn an_account_password_slot_is_bound_to_its_vault() {
    let os = os_vector();
    let cw = std::fs::read(vector_path("created-with/vault")).unwrap();
    let moved_slot = &cw[OS_SLOT_L];
    assert_eq!(moved_slot[OS_L_KIND - OS_SLOT_L.start], 0x03);
    let mut moved = os.clone();
    moved[OS_SLOT_L].copy_from_slice(moved_slot);
    let derived = DERIVATIONS.with(std::cell::Cell::get);
    assert_eq!(open_as(&moved, &account(linux()), FAST), Seen::SlotCorrupt);
    assert_eq!(
        DERIVATIONS.with(std::cell::Cell::get),
        derived + 1,
        "its key check passed"
    );
    assert_eq!(open_as(&moved, &account(mac()), FAST), Seen::Tampered);
    assert_eq!(open_as(&moved, &recovery(), FAST), Seen::Tampered);
}

/// B54: the right account password with another machine id fails the key check — a copy whose
/// tag is rewritten for a guessed machine id tests no guess of the password.
#[test]
fn the_right_password_without_this_machine_id_fails_the_key_check() {
    let file = VaultFile::parse(&os_vector()).unwrap();
    let guess = MachineId::new(&[0x77; 16]).unwrap();
    let mut header = file.header().clone();
    let id = header.slots[0].id;
    if let SlotBody::OsPassword {
        machine_tag: tag, ..
    } = &mut header.slots[0].body
    {
        *tag = super::kdf::machine_tag(&id, &guess);
    }
    let bytes = resealed(&header, &os_plaintext());
    assert_eq!(open_as(&bytes, &account(guess), FAST), Seen::WrongSecret);
}
