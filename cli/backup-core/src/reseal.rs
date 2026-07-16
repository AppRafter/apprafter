// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! 2.6d re-seal: rebuild a `SealedSecret` CR from decrypted backup bytes,
//! scoped to the TARGET cluster's sealed-secrets public key and the secret's
//! strict `namespace/name` label.
//!
//! This is a thin wrapper over [`crate::sealing::build_sealed_secret`],
//! which applies the strict scope internally via [`crate::sealing::seal_encrypted_data`].

use std::collections::BTreeMap;

use cli_core::Result;
use rsa::RsaPublicKey;
use serde_json::Value;

use crate::sealing::build_sealed_secret;

/// Build a bitnami `SealedSecret` CR from the backed-up (decrypted) bytes of
/// a secret, sealed for the TARGET cluster.
///
/// The strict `namespace/name` scope is applied inside
/// [`build_sealed_secret`] → [`crate::sealing::seal_encrypted_data`],
/// so the resulting blob can only be decrypted by the target cluster's
/// sealed-secrets controller when it finds the SealedSecret in the given
/// namespace with the given name.
///
/// # Arguments
/// * `target_pub_key` — RSA public key fetched from the TARGET cluster's
///   sealed-secrets controller (via
///   [`cli_providers::k8s::sealing::fetch_controller_public_key`]).
/// * `namespace` — namespace in the target cluster where the restored Secret
///   will live (and where the SealedSecret will be applied).
/// * `name` — name of the restored Secret.
/// * `secret_type` — Kubernetes secret type (e.g. `"Opaque"`).
/// * `data` — raw (decrypted) key→value pairs from the backup.
pub fn reseal_secret(
    target_pub_key: &RsaPublicKey,
    namespace: &str,
    name: &str,
    secret_type: &str,
    data: &BTreeMap<String, Vec<u8>>,
) -> Result<Value> {
    build_sealed_secret(target_pub_key, namespace, name, data, secret_type)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsa::RsaPrivateKey;
    use std::collections::BTreeMap;

    #[test]
    fn reseal_builds_sealedsecret_for_target_scope() {
        // Mirror sealing.rs test keygen: rand::thread_rng(), 2048-bit.
        let mut rng = rand::thread_rng();
        let priv_key = RsaPrivateKey::new(&mut rng, 2048).unwrap();
        let pub_key = priv_key.to_public_key();

        let mut data = BTreeMap::new();
        data.insert("api-key".to_string(), b"super-secret".to_vec());

        let cr = reseal_secret(&pub_key, "demo", "stripe", "Opaque", &data).unwrap();
        assert_eq!(cr["apiVersion"], "bitnami.com/v1alpha1");
        assert_eq!(cr["kind"], "SealedSecret");
        assert_eq!(cr["metadata"]["namespace"], "demo");
        assert_eq!(cr["metadata"]["name"], "stripe");
        assert!(cr["spec"]["encryptedData"]["api-key"].is_string());
    }
}
