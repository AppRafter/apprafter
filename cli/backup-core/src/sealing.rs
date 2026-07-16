// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Pure sealed-secrets crypto helpers used by `reseal.rs`.
//!
//! The bitnami hybrid scheme (strict-scope RSA-OAEP-SHA256 + AES-256-GCM) —
//! the pure subset of `cli_providers::k8s::sealing` that has no kubectl
//! dependency and can therefore live in the execution-agnostic backup-core
//! crate. The impure `fetch_controller_public_key` remains in cli-providers.

use std::collections::BTreeMap;

use aes_gcm::aead::Aead;
use aes_gcm::{Aes256Gcm, KeyInit, Nonce};
use base64::Engine;
use cli_core::{CliError, Result};
use rand::RngCore;
use rsa::{Oaep, RsaPublicKey};
use serde_json::{json, Value};
use sha2::Sha256;

/// Strict-scope RSA-OAEP label, byte-identical to bitnami sealed-secrets'
/// `EncryptionLabel`: `fmt.Sprintf("%s/%s", namespace, name)`.
pub fn strict_label(namespace: &str, name: &str) -> Vec<u8> {
    format!("{namespace}/{name}").into_bytes()
}

/// Seal one value into the bitnami wire format. `label` is the scope —
/// use [`strict_label`] for the strict-scope default.
pub fn seal_value(pub_key: &RsaPublicKey, label: &[u8], plaintext: &[u8]) -> Result<Vec<u8>> {
    let mut rng = rand::thread_rng();

    // (1) random single-use AES-256 session key.
    let mut session_key = [0u8; 32];
    rng.fill_bytes(&mut session_key);

    // (2) AES-256-GCM with a zero nonce + empty AAD.
    let cipher = Aes256Gcm::new_from_slice(&session_key)
        .map_err(|e| CliError::Other(format!("aes-256-gcm init: {e}")))?;
    let nonce = Nonce::from_slice(&[0u8; 12]);
    let gcm_ciphertext = cipher
        .encrypt(nonce, plaintext)
        .map_err(|e| CliError::Other(format!("aes-256-gcm encrypt: {e}")))?;

    // (3) RSA-OAEP-SHA256 wrap of the session key, label = scope bytes.
    let label = String::from_utf8(label.to_vec())
        .map_err(|e| CliError::Other(format!("oaep label is not utf-8: {e}")))?;
    let padding = Oaep::new_with_label::<Sha256, _>(label);
    let rsa_block = pub_key
        .encrypt(&mut rng, padding, &session_key)
        .map_err(|e| CliError::Other(format!("rsa-oaep encrypt: {e}")))?;

    // (4) wire = 2-byte BE length || RSA block || GCM ciphertext.
    let rsa_len = u16::try_from(rsa_block.len())
        .map_err(|_| CliError::Other("rsa block exceeds u16 length prefix".to_string()))?;
    let mut wire = Vec::with_capacity(2 + rsa_block.len() + gcm_ciphertext.len());
    wire.extend_from_slice(&rsa_len.to_be_bytes());
    wire.extend_from_slice(&rsa_block);
    wire.extend_from_slice(&gcm_ciphertext);
    Ok(wire)
}

/// Seal every entry of `data` under the strict scope for
/// `(namespace, name)` and return the base64-encoded `encryptedData` map.
pub fn seal_encrypted_data(
    pub_key: &RsaPublicKey,
    namespace: &str,
    name: &str,
    data: &BTreeMap<String, Vec<u8>>,
) -> Result<BTreeMap<String, String>> {
    let label = strict_label(namespace, name);
    let b64 = base64::engine::general_purpose::STANDARD;
    let mut out = BTreeMap::new();
    for (k, v) in data {
        let wire = seal_value(pub_key, &label, v)?;
        out.insert(k.clone(), b64.encode(wire));
    }
    Ok(out)
}

/// Build a bitnami `SealedSecret` CR (as a `serde_json::Value`) that seals
/// every entry of `data` under strict scope for `(namespace, name)`.
pub fn build_sealed_secret(
    pub_key: &RsaPublicKey,
    namespace: &str,
    name: &str,
    data: &BTreeMap<String, Vec<u8>>,
    secret_type: &str,
) -> Result<Value> {
    let encrypted = seal_encrypted_data(pub_key, namespace, name, data)?;
    Ok(json!({
        "apiVersion": "bitnami.com/v1alpha1",
        "kind": "SealedSecret",
        "metadata": { "name": name, "namespace": namespace },
        "spec": {
            "encryptedData": encrypted,
            "template": {
                "metadata": { "name": name, "namespace": namespace },
                "type": secret_type,
            }
        }
    }))
}
