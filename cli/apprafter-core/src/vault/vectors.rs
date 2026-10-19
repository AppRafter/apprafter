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

use age::secrecy::ExposeSecret;
use cli_core::secrets::decrypt_with_identity;
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::kdf::{
    derive_password_material, machine_secret, machine_tag, os_password_material, slot_keys,
    KdfPolicy, DERIVATIONS, HEAVY_KDF,
};
use super::keys::{MachineId, SlotSecret};
use super::payload::{EntryKey, VaultPayload};
use super::rng::{OsRandom, ScriptedRandom};
use super::seal::{
    create, create_with, rotate, slot_aad, KeptMaterial, NewSlot, RotationIds, VaultIds,
};
use super::testutil::{hex, seq, unhex, vector_path};
use super::{
    Argon2idParams, RecoveryKey, SlotBody, SlotId, SlotKind, VaultError, VaultFile, VaultId,
};
use crate::context::SecretString;

/// SHA-256 of every file of `v1/`, pinned here: an accidental regeneration fails this test,
/// not only review.
const V1_SHA256: &[(&str, &str)] = &[
    (
        "created-with/inputs.json",
        "f25a4ed80a25c9a3de0e595f9038815195cd46ad160e1eb6f90b937e935b416c",
    ),
    (
        "created-with/vault",
        "16be7d470ff0c9be8bd35c3c234cfa87aa997f0e4320cd91f6de8088dc69af03",
    ),
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
        "os-password/inputs.json",
        "aae7ae822a97279f004642d6fd98ac00ceb74cc5391e3826acb965a477336107",
    ),
    (
        "os-password/vault",
        "722052a95da29523755b1ebb9e9c6d8d878e475028c780e05d38a04283752d76",
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
        "rotated/after/vault",
        "5b0fefc4f15e8d2cbe74160fa398c43d3951280a35969917da119fb69784c214",
    ),
    (
        "rotated/before/vault",
        "60013b7a5713cbe65733517ec83c8ede0ae45ee4b87fcc1e644786f6d6fcfbea",
    ),
    (
        "rotated/cache-new.age",
        "cef9eea076c3b8ab53b7e8fef200a7744111f8532b5d4e974b67a54971ed0064",
    ),
    (
        "rotated/cache-old.age",
        "825f7a4a043ad86bb19f75e5401dc9e90942f1aa4fef2c3ae071ef4917152008",
    ),
    (
        "rotated/inputs.json",
        "2d5f6eccbb73c65412875e996cd50ce56e989594cc4ed1fd3a5cdf5bb840d6e8",
    ),
    (
        "rotated/mid/vault",
        "313296b3f1b83fb0bda41f172204e59bedbe55668fa602cf4b67034e253bf075",
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
            EntryKey::AgeIdentityRetiring => {
                panic!("a create vector holds no retiring store key")
            }
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
            SlotBody::OsPassword { .. } | SlotBody::Opaque(_) => unreachable!(),
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

    // (c') create_with, given the recorded ids and the other draws in order, writes it too
    // (B57: `create` is the same writer, its ids drawn in place).
    let d = draws(&v);
    let id = |i: usize| -> [u8; 16] { d[i].clone().try_into().unwrap() };
    let ids = VaultIds {
        vault_id: VaultId(id(0)),
        slots: vec![SlotId(id(2)), SlotId(id(5))],
    };
    let rest = [1, 3, 4, 6, 7, 8].iter().map(|&i| d[i].clone()).collect();
    let (made, _) = create_with(
        &ids,
        text(&v, &["store_id"]),
        text(&v, &["writer"]),
        payload_of(&v),
        new_slots(&v),
        policy,
        &mut ScriptedRandom::new(rest),
    )
    .unwrap();
    assert_eq!(
        hex(&made),
        hex(&bytes),
        "create_with reproduces {name}/vault"
    );

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

/// One of the two release-strength derivation tests (T23), behind HEAVY_KDF: four derivations.
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

/// A machine id from `inputs.json`: Linux's as its text, the Mac's 16 bytes as hex (B76).
fn machine_id(v: &Value, which: &str) -> MachineId {
    let id = text(v, &["machine_ids", which]);
    match which {
        "linux" => MachineId::new(id.as_bytes()).unwrap(),
        "mac" => MachineId::new(&unhex(id)).unwrap(),
        other => panic!("{other}: no such machine id in the vector"),
    }
}

fn account(v: &Value) -> SecretString {
    SecretString::new(text(v, &["account_password"]))
}

/// Slot `i`'s wrap from `material`, step by step against `trace` (B7: a failure names the step
/// that moved).
fn check_wrap(file: &VaultFile, i: usize, material: &[u8], trace: &Value) {
    let slot = &file.header().slots[i];
    let (salt, wrapped) = match &slot.body {
        SlotBody::Password {
            salt, wrapped_dk, ..
        }
        | SlotBody::OsPassword {
            salt, wrapped_dk, ..
        }
        | SlotBody::RecoveryKey {
            salt, wrapped_dk, ..
        } => (salt, wrapped_dk),
        SlotBody::Opaque(_) => panic!("slot {i} is opaque"),
    };
    assert_eq!(
        u64::from(slot.kind.byte()),
        trace["kind"].as_u64().unwrap(),
        "slot {i} kind"
    );
    assert_eq!(slot.label, text(trace, &["label"]), "slot {i} label");
    let (prk, _) = hkdf::Hkdf::<sha2::Sha256>::extract(Some(&salt[..]), material);
    assert_eq!(hex(&prk), text(trace, &["prk"]), "slot {i} PRK");
    let keys = slot_keys(material, salt, slot.kind.byte());
    assert_eq!(hex(&keys.kek[..]), text(trace, &["kek"]), "slot {i} KEK");
    assert_eq!(
        hex(&keys.key_check[..]),
        text(trace, &["key_check"]),
        "slot {i} key check"
    );
    let aad = slot_aad(&file.header().vault_id, &slot.id, slot.kind, &slot.body);
    assert_eq!(hex(&aad), text(trace, &["slot_aad"]), "slot {i} AAD");
    assert_eq!(
        hex(wrapped),
        text(trace, &["wrapped_dk"]),
        "slot {i} wrapped key"
    );
}

/// Slot `i`, an account-password slot (B54, B76): its fields, its tag and machine secret,
/// its material, then its wrap — against the generator's trace.
fn check_account_slot(file: &VaultFile, i: usize, v: &Value) {
    let trace = &v["slots"][i];
    let slot = &file.header().slots[i];
    let SlotBody::OsPassword {
        kdf,
        salt,
        enrolled_at,
        machine_tag: tag,
        ..
    } = &slot.body
    else {
        panic!("slot {i} is an account-password slot");
    };
    assert_eq!(*kdf, params(v), "slot {i} costs");
    assert_eq!(
        *enrolled_at,
        trace["enrolled_at"].as_u64().unwrap(),
        "slot {i} enrolled_at"
    );
    let machine = machine_id(v, text(trace, &["machine"]));
    assert_eq!(
        hex(tag),
        text(trace, &["machine_tag"]),
        "slot {i} tag as written"
    );
    assert_eq!(
        hex(&machine_tag(&slot.id, &machine)),
        text(trace, &["machine_tag"]),
        "slot {i} tag"
    );
    assert_eq!(
        hex(&machine_secret(&machine)[..]),
        text(trace, &["machine_secret"]),
        "slot {i} machine secret"
    );
    let fast = &KdfPolicy::INSECURE_FAST;
    let argon = derive_password_material(&account(v), salt, kdf, fast).unwrap();
    assert_eq!(
        hex(&argon[..]),
        text(trace, &["argon2_output"]),
        "slot {i} Argon2id"
    );
    let material = os_password_material(&account(v), salt, kdf, &machine, fast).unwrap();
    assert_eq!(
        hex(&material[..]),
        text(trace, &["material"]),
        "slot {i} material"
    );
    check_wrap(file, i, &material[..], trace);
}

/// The new slot `trace` records (an account-password slot of a vector).
fn account_new_slot(v: &Value, trace: &Value) -> NewSlot {
    NewSlot::OsPassword {
        secret: account(v),
        label: text(trace, &["label"]).to_string(),
        enrolled_at: trace["enrolled_at"].as_u64().unwrap(),
        machine: machine_id(v, text(trace, &["machine"])),
    }
}

fn recovery_new_slot() -> NewSlot {
    NewSlot::RecoveryKey {
        key: recovery_key(),
        label: "recovery key".into(),
    }
}

fn derived() -> u32 {
    DERIVATIONS.with(std::cell::Cell::get)
}

/// B7, B54, B76: two account-password slots — one bound to a Linux machine id (its 32 hex
/// characters), one to a macOS one (16 raw bytes) — and the recovery slot.
#[test]
fn os_password_vector() {
    let v = inputs("os-password");
    let bytes = read("os-password/vault");
    assert_eq!(hex(&Sha256::digest(&bytes)), text(&v, &["vault_sha256"]));
    let file = VaultFile::parse(&bytes).unwrap();
    check_account_slot(&file, 0, &v);
    check_account_slot(&file, 1, &v);
    check_wrap(&file, 2, recovery_key().key_bytes(), &v["slots"][2]);
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
    let fast = &KdfPolicy::INSECURE_FAST;

    // `create`, fed the recorded draws, writes it byte for byte.
    let slots = vec![
        account_new_slot(&v, &v["slots"][0]),
        account_new_slot(&v, &v["slots"][1]),
        recovery_new_slot(),
    ];
    let (made, _) = create(
        text(&v, &["store_id"]),
        text(&v, &["writer"]),
        payload_of(&v),
        slots,
        fast,
        &mut ScriptedRandom::new(draws(&v)),
    )
    .unwrap();
    assert_eq!(
        hex(&made),
        hex(&bytes),
        "create reproduces os-password/vault"
    );

    // Each computer's account password opens it, deriving its own slot only, and the
    // recovery key too, to one data key.
    let dk = seq(0x40, 32);
    for which in ["linux", "mac"] {
        let before = derived();
        let secret = SlotSecret::AccountPassword(account(&v), machine_id(&v, which));
        let opened = file.open(&secret, fast).unwrap();
        assert_eq!(
            derived(),
            before + 1,
            "{which}: only its own slot is derived"
        );
        assert_eq!(entries_of(opened.payload()), entries_listed(&v));
        assert_eq!(&opened.data_key().expose()[..], &dk[..]);
    }
    let opened = file
        .open(&SlotSecret::RecoveryKey(recovery_key()), fast)
        .unwrap();
    assert_eq!(&opened.data_key().expose()[..], &dk[..]);

    // Neither a lock password of the same text nor a third computer's id tries a slot.
    let before = derived();
    let third = MachineId::new(&[0x77; 16]).unwrap();
    for secret in [
        SlotSecret::Password(account(&v)),
        SlotSecret::AccountPassword(account(&v), third),
    ] {
        assert!(
            matches!(
                file.open(&secret, fast),
                Err(VaultError::NoUsableSlot { .. })
            ),
            "{secret:?}"
        );
    }
    assert_eq!(derived(), before);

    // L1 on an account-password slot: the release policy refuses its fast costs, unhashed.
    let verdicts = file.slot_policy(&KdfPolicy::RELEASE);
    assert!(matches!(verdicts[0].1, Err(VaultError::KdfTooWeak { .. })));
    assert!(matches!(verdicts[1].1, Err(VaultError::KdfTooWeak { .. })));
    assert!(verdicts[2].1.is_ok());
    let linux = SlotSecret::AccountPassword(account(&v), machine_id(&v, "linux"));
    assert!(matches!(
        file.open(&linux, &KdfPolicy::RELEASE),
        Err(VaultError::KdfTooWeak { .. })
    ));
    assert_eq!(derived(), before);

    // One encoding: resealed with its own nonce, the same bytes.
    let resealed = opened
        .seal(
            text(&v, &["writer"]),
            &mut ScriptedRandom::new(vec![file.payload_nonce.to_vec()]),
        )
        .unwrap();
    assert_eq!(hex(&resealed), hex(&bytes));
}

/// B57: the vault `lock on` writes from ids minted at plan time — an account-password first
/// slot (Linux's machine id) and the recovery slot.
#[test]
fn created_with_vector() {
    let v = inputs("created-with");
    let bytes = read("created-with/vault");
    assert_eq!(hex(&Sha256::digest(&bytes)), text(&v, &["vault_sha256"]));
    let file = VaultFile::parse(&bytes).unwrap();
    check_account_slot(&file, 0, &v);
    check_wrap(&file, 1, recovery_key().key_bytes(), &v["slots"][1]);
    assert_eq!(
        hex(&file.payload_aad),
        text(&v, &["payload_aad"]),
        "payload AAD"
    );
    let id = |hex_text: &str| -> [u8; 16] { unhex(hex_text).try_into().unwrap() };
    let ids = VaultIds {
        vault_id: VaultId(id(text(&v, &["ids", "vault_id"]))),
        slots: v["ids"]["slots"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| SlotId(id(s.as_str().unwrap())))
            .collect(),
    };
    assert_eq!(file.header().vault_id, ids.vault_id);
    let in_file: Vec<SlotId> = file.header().slots.iter().map(|s| s.id).collect();
    assert_eq!(in_file, ids.slots);
    let fast = &KdfPolicy::INSECURE_FAST;
    let slots = vec![account_new_slot(&v, &v["slots"][0]), recovery_new_slot()];
    let (made, _) = create_with(
        &ids,
        text(&v, &["store_id"]),
        text(&v, &["writer"]),
        payload_of(&v),
        slots,
        fast,
        &mut ScriptedRandom::new(draws(&v)),
    )
    .unwrap();
    assert_eq!(
        hex(&made),
        hex(&bytes),
        "create_with reproduces created-with/vault"
    );
    for secret in [
        SlotSecret::AccountPassword(account(&v), machine_id(&v, "linux")),
        SlotSecret::RecoveryKey(recovery_key()),
    ] {
        let opened = file.open(&secret, fast).unwrap();
        assert_eq!(entries_of(opened.payload()), entries_listed(&v));
        assert_eq!(&opened.data_key().expose()[..], &seq(0x40, 32)[..]);
    }
}

/// B6, B7, B63, acceptance 1 and 3: a rotation as `lock rekey` runs it, from the independent
/// generator — a store of every slot shape (a lock password, this computer's account password,
/// another computer's, an unknown slot, the recovery key) rotated to a new data key, vault id
/// and store key; the account password and the recovery key kept under their own ids, a new
/// lock password under its minted id, the rest dropped; the old store key retiring beside the
/// new one. rage sealed one cache to each store key.
#[test]
fn rotated_vector() {
    let v = inputs("rotated");
    let before = read("rotated/before/vault");
    let mid = read("rotated/mid/vault");
    assert_eq!(
        hex(&Sha256::digest(&before)),
        text(&v, &["before", "vault_sha256"])
    );
    assert_eq!(
        hex(&Sha256::digest(&mid)),
        text(&v, &["mid", "vault_sha256"])
    );
    let fast = &KdfPolicy::INSECURE_FAST;
    let b = VaultFile::parse(&before).unwrap();
    assert_eq!(
        b.header().slots.iter().map(|s| s.kind).collect::<Vec<_>>(),
        [
            SlotKind::Password,
            SlotKind::OsPassword,
            SlotKind::OsPassword,
            SlotKind::Unknown(0x7f),
            SlotKind::RecoveryKey
        ]
    );
    let current = b
        .open(&SlotSecret::RecoveryKey(recovery_key()), fast)
        .unwrap();
    assert_eq!(entries_of(current.payload()), entries_listed(&v["before"]));
    // before opens with each of its own slots, the lock password and both computers'
    // account passwords (each with its own machine id), to its recorded data key; the
    // rotation's new lock password opens nothing in it (B7; critic round, 7).
    assert_eq!(
        hex(current.data_key().expose()),
        text(&v, &["before", "data_key"])
    );
    for secret in [
        SlotSecret::Password(SecretString::new(text(&v, &["password"]))),
        SlotSecret::AccountPassword(account(&v), machine_id(&v, "linux")),
        SlotSecret::AccountPassword(account(&v), machine_id(&v, "mac")),
    ] {
        let opened = b.open(&secret, fast).unwrap();
        assert_eq!(
            &opened.data_key().expose()[..],
            &current.data_key().expose()[..],
            "{secret:?}"
        );
    }
    let typed_new = SlotSecret::Password(SecretString::new(text(&v, &["new_password"])));
    assert!(matches!(
        b.open(&typed_new, fast),
        Err(VaultError::WrongSecret { .. })
    ));
    let id = |hex_text: &str| -> [u8; 16] { unhex(hex_text).try_into().unwrap() };
    let ids_at = |name: &str| -> Vec<SlotId> {
        v["rotation"][name]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| SlotId(id(s.as_str().unwrap())))
            .collect()
    };
    let kept = ids_at("kept");
    let ids = RotationIds {
        vault_id: VaultId(id(text(&v, &["rotation", "vault_id"]))),
        new_slots: ids_at("new_slots"),
    };
    let new_password = || SecretString::new(text(&v, &["new_password"]));

    // `rotate`, fed the recorded draws, writes mid byte for byte.
    let linux = KeptMaterial::AccountPassword(account(&v), machine_id(&v, "linux"));
    let keep = vec![
        current.keep(kept[0], linux, fast).unwrap(),
        current
            .keep(kept[1], KeptMaterial::RecoveryKey(recovery_key()), fast)
            .unwrap(),
    ];
    let new = vec![NewSlot::Password {
        secret: new_password(),
        label: "lock password".into(),
    }];
    let mut rng = ScriptedRandom::new(draws(&v["rotation"]));
    let rotated = rotate(
        &current,
        &ids,
        keep,
        new,
        text(&v, &["writer"]),
        fast,
        &mut rng,
    )
    .unwrap();
    assert_eq!(
        hex(&rotated.bytes),
        hex(&mid),
        "rotate reproduces rotated/mid/vault"
    );
    assert_eq!(rotated.dropped, ids_at("dropped"));

    // Every step of mid's slots against the generator's trace; its ids.
    let m = VaultFile::parse(&mid).unwrap();
    assert_eq!(m.header().store_id, b.header().store_id);
    assert_eq!(m.header().vault_id, ids.vault_id);
    assert_eq!(
        m.header().slots.iter().map(|s| s.id).collect::<Vec<_>>(),
        [kept.clone(), ids.new_slots.clone()].concat()
    );
    check_account_slot(&m, 0, &v);
    check_wrap(&m, 1, recovery_key().key_bytes(), &v["slots"][1]);
    let SlotBody::Password { kdf, salt, .. } = &m.header().slots[2].body else {
        panic!("slot 2 is the new lock password");
    };
    let material = derive_password_material(&new_password(), salt, kdf, fast).unwrap();
    assert_eq!(
        hex(&material[..]),
        v["slots"][2]["argon2_output"].as_str().unwrap(),
        "slot 2 Argon2id"
    );
    check_wrap(&m, 2, &material[..], &v["slots"][2]);
    assert_eq!(
        hex(&m.payload_aad),
        text(&v, &["mid", "payload_aad"]),
        "payload AAD"
    );

    // Each kept and new slot opens mid, to the new data key; the old lock password, the
    // other computer's account password and the old data key open nothing.
    let new_dk = draws(&v["rotation"])[0].clone();
    for secret in [
        SlotSecret::AccountPassword(account(&v), machine_id(&v, "linux")),
        SlotSecret::RecoveryKey(recovery_key()),
        SlotSecret::Password(new_password()),
    ] {
        let opened = m.open(&secret, fast).unwrap();
        assert_eq!(&opened.data_key().expose()[..], &new_dk[..]);
        assert_eq!(entries_of(opened.payload()), entries_listed(&v["mid"]));
    }
    let old_password = SlotSecret::Password(SecretString::new(text(&v, &["password"])));
    assert!(matches!(
        m.open(&old_password, fast),
        Err(VaultError::WrongSecret { .. })
    ));
    let mac = SlotSecret::AccountPassword(account(&v), machine_id(&v, "mac"));
    assert!(matches!(
        m.open(&mac, fast),
        Err(VaultError::NoUsableSlot { .. })
    ));
    assert!(matches!(
        m.open_with_key(current.data_key(), ids.vault_id),
        Err(VaultError::Tampered)
    ));

    // The store keys: the generator's texts and recipients.
    let payload = rotated.vault.payload();
    let new_key = payload.age_identity().unwrap();
    let retiring = payload.retiring_identity().unwrap().unwrap();
    assert_eq!(
        new_key.to_string().expose_secret(),
        text(&v, &["identities", "new"])
    );
    assert_eq!(
        retiring.to_string().expose_secret(),
        text(&v, &["identities", "old"])
    );
    assert_eq!(
        new_key.to_public().to_string(),
        text(&v, &["recipients", "new"])
    );
    assert_eq!(
        retiring.to_public().to_string(),
        text(&v, &["recipients", "old"])
    );

    // rage's caches: the old store key — before's, and mid's retiring one — opens the old
    // cache only; the new store key the new cache only.
    let plain = text(&v, &["cache", "plaintext"]);
    let cache = |name: &str| String::from_utf8(read(&format!("rotated/{name}"))).unwrap();
    let old_key = current.payload().age_identity().unwrap();
    for key in [&old_key, &retiring] {
        assert_eq!(
            decrypt_with_identity(&cache("cache-old.age"), key).unwrap(),
            plain
        );
        assert!(decrypt_with_identity(&cache("cache-new.age"), key).is_err());
    }
    assert_eq!(
        decrypt_with_identity(&cache("cache-new.age"), &new_key).unwrap(),
        plain
    );
    assert!(decrypt_with_identity(&cache("cache-old.age"), &new_key).is_err());
}

/// B63, R4, acceptance 2: `retire_identity` on the rotated vector writes after byte for
/// byte — the same data key, vault id and slots, the payload without
/// `age-identity-retiring` — and only the new store key is left, which opens the new cache.
#[test]
fn rotated_vector_retired() {
    let v = inputs("rotated");
    let mid = read("rotated/mid/vault");
    let after = read("rotated/after/vault");
    assert_eq!(
        hex(&Sha256::digest(&after)),
        text(&v, &["after", "vault_sha256"])
    );
    let fast = &KdfPolicy::INSECURE_FAST;
    let m = VaultFile::parse(&mid).unwrap();
    let opened = m
        .open(&SlotSecret::RecoveryKey(recovery_key()), fast)
        .unwrap();
    let mut rng = ScriptedRandom::new(draws(&v["after"]));
    let retired = opened
        .retire_identity(text(&v, &["writer"]), &mut rng)
        .unwrap();
    assert_eq!(
        hex(&retired),
        hex(&after),
        "retire_identity reproduces rotated/after/vault"
    );
    let a = VaultFile::parse(&after).unwrap();
    assert_eq!(a.header(), m.header());
    assert_eq!(
        hex(&a.payload_aad),
        text(&v, &["after", "payload_aad"]),
        "payload AAD"
    );
    let by_key = a
        .open_with_key(opened.data_key(), a.header().vault_id)
        .unwrap();
    assert_eq!(entries_of(by_key.payload()), entries_listed(&v["after"]));
    assert!(by_key.payload().retiring_identity().unwrap().is_none());
    for secret in [
        SlotSecret::AccountPassword(account(&v), machine_id(&v, "linux")),
        SlotSecret::RecoveryKey(recovery_key()),
        SlotSecret::Password(SecretString::new(text(&v, &["new_password"]))),
    ] {
        assert!(a.open(&secret, fast).is_ok(), "{secret:?}");
    }
    let key = by_key.payload().age_identity().unwrap();
    assert_eq!(
        key.to_string().expose_secret(),
        text(&v, &["identities", "new"])
    );
    let cache = |name: &str| String::from_utf8(read(&format!("rotated/{name}"))).unwrap();
    assert_eq!(
        decrypt_with_identity(&cache("cache-new.age"), &key).unwrap(),
        text(&v, &["cache", "plaintext"])
    );
    assert!(decrypt_with_identity(&cache("cache-old.age"), &key).is_err());
}
