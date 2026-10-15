// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The frozen v1 vectors (D.4a T30; ADR 0068 §2 "golden vectors freeze each version").
//!
//! `tests/vault_vectors/v1/` was written once by an implementation independent of this one —
//! `tests/vault_vectors/gen_v1_vectors.py`: libsodium (PyNaCl) for XChaCha20-Poly1305,
//! libargon2 (argon2-cffi) for Argon2id, Python's `hmac` for HKDF-SHA256 and the BIP-350
//! reference code — and is never regenerated. This build must open each vector, reproduce it
//! byte for byte from its inputs, and agree with every intermediate value, so a failure says
//! which step of the schedule moved. A later format version adds `v2/` beside `v1/`; every
//! reader keeps opening `v1/`.

use std::fs;

use serde_json::Value;
use sha2::{Digest, Sha256};

use super::kdf::{derive_password_material, slot_keys, KdfPolicy, HEAVY_KDF};
use super::keys::SlotSecret;
use super::payload::{EntryKey, VaultPayload};
use super::rng::{OsRandom, ScriptedRandom};
use super::seal::{create, slot_aad, NewSlot};
use super::testutil::{hex, unhex, vector_path};
use super::{Argon2idParams, RecoveryKey, SlotBody, SlotKind, VaultFile};
use crate::context::SecretString;

/// SHA-256 of every file of `v1/`, pinned here: an accidental regeneration fails this test,
/// not only review.
const V1_SHA256: &[(&str, &str)] = &[
    (
        "fast-params/inputs.json",
        "7f660d17b66b7dab8435a5edecbef93c9388af36cf7ad9e0ad78f673e3f65281",
    ),
    (
        "fast-params/store.yaml",
        "9949e4c4851f9475686b5671b29149787e17f52dee37e4ce71929a9ff1763a30",
    ),
    (
        "fast-params/vault",
        "ac2a0a30bb39155bb605cde1455aa7c89517675f64e448409570f3ec40d26c79",
    ),
    (
        "password-recovery/inputs.json",
        "610363685342d0e04255af4f0098528928f91de07747318e0c35718336293d17",
    ),
    (
        "password-recovery/vault",
        "cbfd48b251f06872564203b0e0165d4460cd6f452a538a35f4ceac9aa2b428b5",
    ),
    (
        "recovery-key.json",
        "8e8baeea07d44cda8f66d16ca530e42d450de5c318d7d414ada190dc9acbd5f6",
    ),
    (
        "unknown-entry/inputs.json",
        "6147ef04eb16c44b419b0dbac6f247fb50e2672496281bbc2e1a92fa94ae0083",
    ),
    (
        "unknown-entry/vault",
        "fd8664dff9c78bfdfef3d94b565a209713c8bf2c1006c6b84e9400221620c8c4",
    ),
    (
        "unknown-slot/inputs.json",
        "84f88f56fe60ccc1804ffe6462f7d646ed0b83d1d38cbcdf219f7ddf0664d130",
    ),
    (
        "unknown-slot/vault",
        "b25efca87b642aa85e17674697b13bd79bfa15a88f07b3793546f30862e41387",
    ),
];

fn read(rel: &str) -> Vec<u8> {
    fs::read(vector_path(rel)).unwrap_or_else(|e| panic!("tests/vault_vectors/v1/{rel}: {e}"))
}

fn inputs(name: &str) -> Value {
    serde_json::from_slice(&read(&format!("{name}/inputs.json"))).unwrap()
}

fn text<'a>(v: &'a Value, path: &[&str]) -> &'a str {
    path.iter()
        .fold(v, |v, key| &v[*key])
        .as_str()
        .unwrap_or_else(|| panic!("{path:?} is not a string"))
}

fn params(v: &Value) -> Argon2idParams {
    let n = |key: &str| v["params"][key].as_u64().unwrap() as u32;
    Argon2idParams {
        m_cost_kib: n("m_cost_kib"),
        t_cost: n("t_cost"),
        p_cost: n("p_cost"),
    }
}

fn draws(v: &Value) -> Vec<Vec<u8>> {
    v["random"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| unhex(c.as_str().unwrap()))
        .collect()
}

/// The recovery key of every vector: bytes `00 … 1f`, through its recorded text.
fn recovery_key() -> RecoveryKey {
    let doc: Value = serde_json::from_slice(&read("recovery-key.json")).unwrap();
    RecoveryKey::parse(text(&doc, &["canonical"])).unwrap()
}

/// The payload `inputs.json` lists, built through the payload's own setters.
fn payload_of(v: &Value) -> VaultPayload {
    let entries = v["entries"].as_object().unwrap();
    let value = |key: &str| unhex(entries[key].as_str().unwrap());
    let identity = String::from_utf8(value("age-identity")).unwrap();
    let mut payload = VaultPayload::new(&identity.parse().unwrap());
    for key in entries.keys() {
        match EntryKey::parse(key) {
            EntryKey::AgeIdentity => {}
            EntryKey::HetznerToken { target } => payload
                .set_hetzner_token(
                    &target,
                    SecretString::new(String::from_utf8(value(key)).unwrap()),
                )
                .unwrap(),
            EntryKey::UnlockTimeoutMin => payload
                .set_unlock_timeout_min(String::from_utf8(value(key)).unwrap().parse().unwrap())
                .unwrap(),
            EntryKey::Other(other) => panic!("{other}: a create vector holds known entries only"),
        }
    }
    payload
}

fn new_slots(v: &Value) -> Vec<NewSlot> {
    vec![
        NewSlot::Password {
            secret: SecretString::new(text(v, &["password"])),
            label: "lock password".into(),
        },
        NewSlot::RecoveryKey {
            key: recovery_key(),
            label: "recovery key".into(),
        },
    ]
}

/// Every key and value of an opened payload, values hex-encoded.
fn entries_of(payload: &VaultPayload) -> Vec<(String, String)> {
    payload
        .keys()
        .map(|key| (key.to_string(), hex(payload.raw(key).unwrap())))
        .collect()
}

fn entries_listed(v: &Value) -> Vec<(String, String)> {
    v["entries"]
        .as_object()
        .unwrap()
        .iter()
        .map(|(key, value)| (key.clone(), value.as_str().unwrap().to_string()))
        .collect()
}

/// (a)–(e) of the crypto brief §12.2 for one create vector under `policy`.
fn check_create_vector(name: &str, policy: &KdfPolicy) {
    let v = inputs(name);
    let bytes = read(&format!("{name}/vault"));
    assert_eq!(hex(&Sha256::digest(&bytes)), text(&v, &["vault_sha256"]));
    let file = VaultFile::parse(&bytes).unwrap();
    let slot = |i: usize| &file.header().slots[i];
    let SlotBody::Password { salt, .. } = &slot(0).body else {
        panic!("slot 0 is the password slot");
    };

    // (d) every intermediate, step by step.
    let material = derive_password_material(
        &SecretString::new(text(&v, &["password"])),
        salt,
        &params(&v),
        policy,
    )
    .unwrap();
    assert_eq!(hex(&material[..]), text(&v, &["argon2_output"]), "Argon2id");
    for (i, trace, material) in [
        (0, "password_slot", *material),
        (1, "recovery_slot", *recovery_key().key_bytes()),
    ] {
        let (salt, wrapped) = match &slot(i).body {
            SlotBody::Password {
                salt, wrapped_dk, ..
            }
            | SlotBody::RecoveryKey {
                salt, wrapped_dk, ..
            } => (salt, wrapped_dk),
            SlotBody::Opaque(_) => unreachable!(),
        };
        let (prk, _) = hkdf::Hkdf::<sha2::Sha256>::extract(Some(&salt[..]), &material);
        assert_eq!(hex(&prk), text(&v, &[trace, "prk"]), "{trace} PRK");
        let keys = slot_keys(&material, salt, slot(i).kind.byte());
        assert_eq!(hex(&keys.kek[..]), text(&v, &[trace, "kek"]), "{trace} KEK");
        assert_eq!(
            hex(&keys.key_check[..]),
            text(&v, &[trace, "key_check"]),
            "{trace} key check"
        );
        let aad = slot_aad(
            &file.header().vault_id,
            &slot(i).id,
            slot(i).kind,
            &slot(i).body,
        );
        assert_eq!(hex(&aad), text(&v, &[trace, "slot_aad"]), "{trace} AAD");
        assert_eq!(
            hex(wrapped),
            text(&v, &[trace, "wrapped_dk"]),
            "{trace} wrapped key"
        );
    }
    assert_eq!(
        hex(&file.payload_aad),
        text(&v, &["payload_aad"]),
        "payload AAD"
    );
    assert_eq!(
        hex(&payload_of(&v).encode().unwrap()),
        text(&v, &["payload_plaintext"]),
        "payload plaintext"
    );

    // (c) create, fed the recorded draws, writes the vault byte for byte.
    let mut rng = ScriptedRandom::new(draws(&v));
    let (created, _) = create(
        text(&v, &["store_id"]),
        text(&v, &["writer"]),
        payload_of(&v),
        new_slots(&v),
        policy,
        &mut rng,
    )
    .unwrap();
    assert_eq!(hex(&created), hex(&bytes), "create reproduces {name}/vault");

    // (a), (b) each secret opens it to the recorded entries and the recorded data key.
    let by_password = file
        .open(
            &SlotSecret::Password(SecretString::new(text(&v, &["password"]))),
            policy,
        )
        .unwrap();
    let by_recovery = file
        .open(&SlotSecret::RecoveryKey(recovery_key()), policy)
        .unwrap();
    for opened in [&by_password, &by_recovery] {
        assert_eq!(entries_of(opened.payload()), entries_listed(&v));
        assert_eq!(&opened.data_key().expose()[..], &draws(&v)[1][..]);
    }

    // (e) two seals draw two nonces, and both open.
    let first = VaultFile::parse(
        &by_password
            .seal("apprafter 0.0.0-vector", &mut OsRandom)
            .unwrap(),
    )
    .unwrap();
    let second = VaultFile::parse(
        &by_password
            .seal("apprafter 0.0.0-vector", &mut OsRandom)
            .unwrap(),
    )
    .unwrap();
    assert_ne!(first.payload_nonce, second.payload_nonce);
    for file in [&first, &second] {
        assert!(file
            .open_with_key(by_password.data_key(), file.header().vault_id)
            .is_ok());
    }
}

#[test]
fn the_v1_files_are_the_frozen_ones() {
    for (rel, sum) in V1_SHA256 {
        assert_eq!(
            hex(&Sha256::digest(read(rel))),
            *sum,
            "{rel} changed: the v1 vectors are written once and never regenerated"
        );
    }
    let listed: String = V1_SHA256
        .iter()
        .map(|(rel, sum)| format!("{sum}  {rel}\n"))
        .collect();
    assert_eq!(String::from_utf8(read("SHA256SUMS")).unwrap(), listed);
    let mut files = 0;
    let mut dirs = vec![vector_path("")];
    while let Some(dir) = dirs.pop() {
        for entry in fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                dirs.push(path);
            } else {
                files += 1;
            }
        }
    }
    assert_eq!(
        files,
        V1_SHA256.len() + 1,
        "a file in v1/ that SHA256SUMS does not pin"
    );
}

/// One of the two release-strength derivation tests (T23), behind HEAVY_KDF: three derivations.
#[test]
fn password_recovery_vector() {
    let _heavy = HEAVY_KDF.lock().unwrap_or_else(|e| e.into_inner());
    // Frozen with today's release default; the vector keeps these whatever the default becomes.
    assert_eq!(
        params(&inputs("password-recovery")),
        Argon2idParams {
            m_cost_kib: 262_144,
            t_cost: 3,
            p_cost: 4
        }
    );
    check_create_vector("password-recovery", &KdfPolicy::RELEASE);
}

#[test]
fn fast_params_vector() {
    check_create_vector("fast-params", &KdfPolicy::INSECURE_FAST);
    // L1 on a file: the release policy refuses its password slot, its recovery key opens it.
    let file = VaultFile::parse(&read("fast-params/vault")).unwrap();
    let verdicts = file.slot_policy(&KdfPolicy::RELEASE);
    assert!(matches!(
        verdicts[0].1,
        Err(super::VaultError::KdfTooWeak { .. })
    ));
    assert!(verdicts[1].1.is_ok());
    assert!(matches!(
        file.open(
            &SlotSecret::Password(SecretString::new("correct horse battery staple")),
            &KdfPolicy::RELEASE
        ),
        Err(super::VaultError::KdfTooWeak { .. })
    ));
    assert!(file
        .open(
            &SlotSecret::RecoveryKey(recovery_key()),
            &KdfPolicy::RELEASE
        )
        .is_ok());
    // The fixture store beside it names the vault's store (c2 parses it; L3 in d4a-e reads it).
    let store = String::from_utf8(read("fast-params/store.yaml")).unwrap();
    assert!(store.contains(&format!("id: {}\n", file.header().store_id)));
}

/// An unknown slot kind is listed, skipped, and written back byte for byte.
#[test]
fn unknown_slot_vector() {
    let bytes = read("unknown-slot/vault");
    let file = VaultFile::parse(&bytes).unwrap();
    let unknown = &file.header().slots[2];
    assert_eq!(unknown.kind, SlotKind::Unknown(0x7f));
    assert_eq!(unknown.body, SlotBody::Opaque((0u8..40).collect()));
    assert_eq!(unknown.label, "future slot");
    let opened = file
        .open(
            &SlotSecret::Password(SecretString::new("correct horse battery staple")),
            &KdfPolicy::INSECURE_FAST,
        )
        .unwrap();
    let nonce = file.payload_nonce.to_vec();
    let resealed = opened
        .seal(
            "apprafter 0.0.0-vector",
            &mut ScriptedRandom::new(vec![nonce]),
        )
        .unwrap();
    assert_eq!(hex(&resealed), hex(&bytes));
    assert!(VaultFile::parse(&resealed)
        .unwrap()
        .open(
            &SlotSecret::RecoveryKey(recovery_key()),
            &KdfPolicy::INSECURE_FAST
        )
        .is_ok());
}

/// Unknown entries are kept byte for byte and move or go with their target.
#[test]
fn unknown_entry_vector() {
    let bytes = read("unknown-entry/vault");
    let file = VaultFile::parse(&bytes).unwrap();
    let mut opened = file
        .open(
            &SlotSecret::RecoveryKey(recovery_key()),
            &KdfPolicy::INSECURE_FAST,
        )
        .unwrap();
    assert_eq!(
        entries_of(opened.payload()),
        entries_listed(&inputs("unknown-entry"))
    );
    let nonce = file.payload_nonce.to_vec();
    let resealed = opened
        .seal(
            "apprafter 0.0.0-vector",
            &mut ScriptedRandom::new(vec![nonce]),
        )
        .unwrap();
    assert_eq!(hex(&resealed), hex(&bytes));
    assert_eq!(opened.payload_mut().remove_target("prod"), 2);
    let after = VaultFile::parse(
        &opened
            .seal("apprafter 0.0.0-vector", &mut OsRandom)
            .unwrap(),
    )
    .unwrap()
    .open_with_key(opened.data_key(), file.header().vault_id)
    .unwrap();
    assert_eq!(
        after.payload().keys().collect::<Vec<_>>(),
        [
            "age-identity",
            "settings/unlock-timeout-min",
            "target/staging-1/hetzner-token",
            "x-future/key"
        ]
    );
    assert_eq!(
        after.payload().raw("x-future/key"),
        Some(&[0xffu8, 0xfe, 0x00, 0x01][..])
    );
}
