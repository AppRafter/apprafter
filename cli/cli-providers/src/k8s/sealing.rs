// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Native client-side sealing for the bitnami-labs sealed-secrets
//! controller (1.79c S0 / ADR 0039) — no external `kubeseal` binary,
//! so the CLI stays a single static binary.
//!
//! The bitnami hybrid scheme (see `bitnami-labs/sealed-secrets`
//! `docs/developer/crypto.md`):
//!
//!   1. Generate a random 32-byte AES-256 session key.
//!   2. AES-256-GCM encrypt the value with a **zero 12-byte nonce**
//!      (safe — the session key is single-use) and **empty AAD**; the
//!      16-byte GCM tag is appended to the ciphertext.
//!   3. RSA-OAEP (SHA-256 for both the hash and the MGF1 mask) encrypt
//!      the session key, with the OAEP **label = scope bytes**. Strict
//!      scope (the default, and what we use for platform material in a
//!      fixed namespace) is `namespace || name`.
//!   4. Wire bytes = big-endian `u16` length of the RSA block, then the
//!      RSA block, then the AES-GCM ciphertext.
//!   5. `SealedSecret.spec.encryptedData[key]` = standard-base64(wire).
//!
//! Only the controller's in-cluster private key can decrypt; the sealed
//! blob is safe in transit, at rest, and in Git.

use std::collections::BTreeMap;
use std::path::Path;

use aes_gcm::aead::Aead;
use aes_gcm::{Aes256Gcm, KeyInit, Nonce};
use base64::Engine;
use cli_core::{CliError, Result};
use rand::RngCore;
use rsa::pkcs8::DecodePublicKey;
use rsa::{Oaep, RsaPublicKey};
use serde_json::{json, Value};
use sha2::Sha256;
use x509_cert::der::{DecodePem, Encode};
use x509_cert::Certificate;

/// Strict-scope RSA-OAEP label per bitnami sealed-secrets: the namespace
/// concatenated with the name, no separator.
pub fn strict_label(namespace: &str, name: &str) -> Vec<u8> {
    let mut label = namespace.as_bytes().to_vec();
    label.extend_from_slice(name.as_bytes());
    label
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
/// `(namespace, name)` and return the base64-encoded `encryptedData` map
/// for a `SealedSecret` CR.
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

/// Parse the controller's PEM X.509 certificate and extract its RSA public
/// key. The cert is fetched over the TLS-authenticated kube API (see
/// [`fetch_controller_public_key`]), which is what protects it against
/// substitution.
pub fn public_key_from_cert_pem(pem: &str) -> Result<RsaPublicKey> {
    let cert = Certificate::from_pem(pem.as_bytes())
        .map_err(|e| CliError::Other(format!("parse controller cert pem: {e}")))?;
    let spki_der = cert
        .tbs_certificate
        .subject_public_key_info
        .to_der()
        .map_err(|e| CliError::Other(format!("encode subjectPublicKeyInfo: {e}")))?;
    RsaPublicKey::from_public_key_der(&spki_der)
        .map_err(|e| CliError::Other(format!("controller key is not RSA: {e}")))
}

/// kube-API service-proxy path for the sealed-secrets controller's public
/// cert. Routed through the apiserver (TLS-authenticated) rather than the
/// pod network, so the default-deny NetworkPolicy does not block it and the
/// cert cannot be substituted by an on-path actor. The Service name is
/// pinned via `fullnameOverride` in the platform-stack component.
pub fn controller_cert_raw_path() -> String {
    "/api/v1/namespaces/apprafter-system/services/http:sealed-secrets-controller:http/proxy/v1/cert.pem"
        .to_string()
}

/// Fetch the controller's public key over the kube API and parse it.
pub fn fetch_controller_public_key(
    kubectl: &dyn crate::k8s::kubectl::KubectlRunner,
    kubeconfig_path: &Path,
) -> Result<RsaPublicKey> {
    let pem = kubectl.get_raw(&controller_cert_raw_path(), kubeconfig_path)?;
    public_key_from_cert_pem(&pem)
}

/// Build a bitnami `SealedSecret` CR (as a `serde_json::Value`) that seals
/// every entry of `data` under strict scope for `(namespace, name)`. The
/// `template` carries the resulting `Secret`'s metadata + type so the
/// controller materialises it in place.
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

#[cfg(test)]
mod tests {
    use super::*;
    use aes_gcm::aead::Aead;
    use rsa::traits::PublicKeyParts;
    use rsa::{Oaep, RsaPrivateKey};
    use sha2::Sha256;

    fn test_keypair() -> (RsaPrivateKey, RsaPublicKey) {
        // 2048 keeps the unit test fast; production uses the controller's
        // 4096-bit cert.
        let mut rng = rand::thread_rng();
        let priv_key = RsaPrivateKey::new(&mut rng, 2048).expect("gen rsa key");
        let pub_key = priv_key.to_public_key();
        (priv_key, pub_key)
    }

    /// Decrypt the bitnami wire format with the private key — the inverse
    /// of `seal_value`, so a round-trip proves the envelope is exactly the
    /// scheme the controller implements.
    fn unseal(priv_key: &RsaPrivateKey, label: &[u8], wire: &[u8]) -> Vec<u8> {
        let rsa_len = u16::from_be_bytes([wire[0], wire[1]]) as usize;
        let rsa_block = &wire[2..2 + rsa_len];
        let gcm_ciphertext = &wire[2 + rsa_len..];

        let label = String::from_utf8(label.to_vec()).unwrap();
        let padding = Oaep::new_with_label::<Sha256, _>(label);
        let session_key = priv_key.decrypt(padding, rsa_block).expect("rsa decrypt");

        let cipher = Aes256Gcm::new_from_slice(&session_key).unwrap();
        let nonce = Nonce::from_slice(&[0u8; 12]);
        cipher.decrypt(nonce, gcm_ciphertext).expect("aes decrypt")
    }

    #[test]
    fn strict_label_is_namespace_then_name() {
        assert_eq!(strict_label("ns", "nm"), b"nsnm".to_vec());
        assert_eq!(
            strict_label("apprafter-system", "srccred-demo-material"),
            b"apprafter-systemsrccred-demo-material".to_vec()
        );
    }

    #[test]
    fn seal_round_trips_under_strict_scope() {
        let (priv_key, pub_key) = test_keypair();
        let label = strict_label("apprafter-system", "srccred-demo-material");
        let wire = seal_value(&pub_key, &label, b"ghp_exampletoken123").unwrap();
        let back = unseal(&priv_key, &label, &wire);
        assert_eq!(back, b"ghp_exampletoken123");
    }

    #[test]
    fn wrong_scope_label_fails_to_unseal() {
        let (priv_key, pub_key) = test_keypair();
        let sealed_label = strict_label("apprafter-system", "srccred-demo-material");
        let wire = seal_value(&pub_key, &sealed_label, b"secret").unwrap();
        // A different namespace/name yields a different OAEP label; RSA-OAEP
        // decryption must reject it (this is the strict-scope binding).
        let wrong_label = strict_label("default", "srccred-demo-material");
        let rsa_len = u16::from_be_bytes([wire[0], wire[1]]) as usize;
        let padding = Oaep::new_with_label::<Sha256, _>(String::from_utf8(wrong_label).unwrap());
        assert!(priv_key.decrypt(padding, &wire[2..2 + rsa_len]).is_err());
    }

    #[test]
    fn seal_encrypted_data_emits_base64_per_key() {
        let (_priv, pub_key) = test_keypair();
        let mut data = BTreeMap::new();
        data.insert("token".to_string(), b"ghp_x".to_vec());
        data.insert("ca".to_string(), b"----".to_vec());
        let out = seal_encrypted_data(&pub_key, "apprafter-system", "srccred-acme-material", &data)
            .unwrap();
        assert_eq!(out.len(), 2);
        // base64 of (2 + 256 RSA block + GCM) is well over 300 chars for 2048-bit.
        assert!(out["token"].len() > 300);
        assert!(base64::engine::general_purpose::STANDARD
            .decode(&out["token"])
            .is_ok());
    }

    // A throwaway self-signed RSA-2048 cert (CN=sealed-secrets), shaped
    // exactly like the controller's `/v1/cert.pem`. Used only to exercise
    // the X.509 → RSA-public-key extraction path.
    const TEST_CERT_PEM: &str = "-----BEGIN CERTIFICATE-----\n\
MIIDEzCCAfugAwIBAgIUEGwALXIt6VVjL69wEPgNvw6fy4IwDQYJKoZIhvcNAQEL\n\
BQAwGTEXMBUGA1UEAwwOc2VhbGVkLXNlY3JldHMwHhcNMjYwNTMwMTA1ODI2WhcN\n\
MzYwNTI3MTA1ODI2WjAZMRcwFQYDVQQDDA5zZWFsZWQtc2VjcmV0czCCASIwDQYJ\n\
KoZIhvcNAQEBBQADggEPADCCAQoCggEBAL+Baum+/DU1rtKMEQjmiHG5HUQSFfEz\n\
FffxQUdQFty5TCUxXZRDCQF/p19xow9j7wzv94XdpNEsdaxkFhPc3GqrazwJCTtN\n\
eRsaIFBbOQcfWAR5tozb5hEY0Acg9/qjNUpqIGyFd+ckrVd5PM4khLAgO+DJPWV7\n\
8xPDDt60ftx35zre2EJ/QaqY3x5P7rp1NXtUe3Djag/wN8Zfd9vW/uErL2vPAoQk\n\
DDwUh6CwzWYDZ6LP2WELWzg5la4GcMDQqunswjiIya8EdLn8tJegUofjKhSK4ItK\n\
KfQ/M2k6Fr4IT45+pjIpP8hchZSU3dQESZY9j4GOir5JsoMhVsImCEMCAwEAAaNT\n\
MFEwHQYDVR0OBBYEFPPlsGrc8nfLIsJTLHu/TlFKFdNqMB8GA1UdIwQYMBaAFPPl\n\
sGrc8nfLIsJTLHu/TlFKFdNqMA8GA1UdEwEB/wQFMAMBAf8wDQYJKoZIhvcNAQEL\n\
BQADggEBAJqdsHhxK1bDHYq2WEip+rbNm12FEowVYeNpKXHzz7iQ01ChyxnWfZB3\n\
HoNFzrWVovjS0rgkVdp8na1UCH7OSDVS5ZNA5MI69kKJDArersx1XJoT+XefO5pM\n\
3+gTqrbwkpxV8i6nYGgPcnbv4iCgPTXMVXY9scNmnH/qPI0cR3w5XmZ13M45GSCa\n\
GxF0B2cjA2v6+pxPGCi24U9Me4SZjPQIoya20GsMDvja337Etp46uSUWiQbVadJ5\n\
S76U0erGx//AVPudbq9NXMPpWtHO+uywxVP+TCpipJjkINNq27apueEQzlFdljJL\n\
vE/RXfo1pEArgO4rXBaZPr9LyWQ0ayM=\n\
-----END CERTIFICATE-----\n";

    #[test]
    fn public_key_parses_from_pem_cert() {
        let key = public_key_from_cert_pem(TEST_CERT_PEM).unwrap();
        // 2048-bit modulus = 256 bytes.
        assert_eq!(key.size(), 256);
    }

    #[test]
    fn cert_round_trips_seal_against_extracted_key() {
        // Extract the public key from the cert, seal with it — proves the
        // cert-derived key is usable by the envelope.
        let key = public_key_from_cert_pem(TEST_CERT_PEM).unwrap();
        let label = strict_label("apprafter-system", "srccred-acme-material");
        let wire = seal_value(&key, &label, b"ghp_x").unwrap();
        assert!(wire.len() > 256);
    }

    #[test]
    fn cert_raw_path_is_the_service_proxy_for_the_pinned_controller() {
        assert_eq!(
            controller_cert_raw_path(),
            "/api/v1/namespaces/apprafter-system/services/http:sealed-secrets-controller:http/proxy/v1/cert.pem"
        );
    }

    #[test]
    fn builds_sealed_secret_cr_with_template() {
        let key = public_key_from_cert_pem(TEST_CERT_PEM).unwrap();
        let mut data = BTreeMap::new();
        data.insert("token".to_string(), b"ghp_x".to_vec());
        let cr = build_sealed_secret(
            &key,
            "apprafter-system",
            "srccred-acme-material",
            &data,
            "Opaque",
        )
        .unwrap();
        assert_eq!(cr["apiVersion"], "bitnami.com/v1alpha1");
        assert_eq!(cr["kind"], "SealedSecret");
        assert_eq!(cr["metadata"]["name"], "srccred-acme-material");
        assert_eq!(cr["metadata"]["namespace"], "apprafter-system");
        assert!(cr["spec"]["encryptedData"]["token"].as_str().unwrap().len() > 40);
        assert_eq!(
            cr["spec"]["template"]["metadata"]["name"],
            "srccred-acme-material"
        );
        assert_eq!(cr["spec"]["template"]["type"], "Opaque");
    }
}
