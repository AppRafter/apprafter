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
use super::testutil::{seq, seq_array, tlv, vector_path};
use super::{RecoveryKey, VaultError, VaultFile, VaultResult};
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
