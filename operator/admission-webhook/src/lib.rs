// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Library surface of the AppRafter admission webhook.

pub mod server;
pub mod validator;
pub mod validator_migrationplan;
pub mod validator_platformstack;
pub mod validator_resourceclaim;
pub mod validator_retainedclaim;
pub mod validator_serviceprovider;
pub mod validator_sourcecredential;

pub use server::build_router;
pub use validator::{validate_application_spec, ValidationError};
pub use validator_sourcecredential::validate_sourcecredential;

/// Install `aws-lc-rs` as the process-level rustls
/// `CryptoProvider`. Mirrors `apprafter_operator::install_rustls_crypto_provider`
/// — rustls 0.23+ removed the auto-default provider, so any
/// rustls-using API (axum-server's TLS init, kube's
/// `Client::try_default`) panics at runtime without this call.
///
/// Walk-found bug v0.1.105 → v0.1.106: the webhook code never
/// actually ran before walk #8 because
/// `ghcr.io/apprafter/apprafter-admission-webhook:v0.1.91` was a
/// broken image with the binary missing. The v0.1.105 image
/// (cargo-chef Dockerfile) produces a working binary; first run
/// surfaced the rustls panic that the operator already had a
/// fix for since v0.1.61. This adds the same fix to the webhook
/// crate.
///
/// Idempotent — if a provider is already installed, the second
/// call is a no-op rather than a panic (matches the operator's
/// behaviour).
pub fn install_rustls_crypto_provider() {
    // `install_default` returns `Err(CryptoProvider)` if one is
    // already installed; we ignore the Err and don't replace —
    // any installed provider satisfies the rustls requirement.
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_rustls_crypto_provider_sets_a_process_level_default() {
        // Regression guard mirroring the operator's. Without
        // this call, axum-server's RustlsConfig::from_pem_file
        // panics at startup with `Could not automatically
        // determine the process-level CryptoProvider`.
        install_rustls_crypto_provider();
        assert!(
            rustls::crypto::CryptoProvider::get_default().is_some(),
            "after install_rustls_crypto_provider() the process-level CryptoProvider \
             must be set; otherwise the webhook pod panics at TLS init"
        );
    }

    #[test]
    fn install_rustls_crypto_provider_is_idempotent() {
        // Second call must not panic — install_default returns
        // Err (not panic) on a re-install, and we explicitly
        // swallow that Err.
        install_rustls_crypto_provider();
        install_rustls_crypto_provider();
        assert!(rustls::crypto::CryptoProvider::get_default().is_some());
    }
}
