// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Library surface of the AppRafter operator binary.
//!
//! Exposes the axum router builder so integration tests can drive
//! it via `tower::ServiceExt::oneshot`, and the rustls crypto-provider
//! installer that `main()` must call before any TLS-using kube
//! client construction.

pub mod server;

pub use server::build_router;

/// Install `aws-lc-rs` as the process-level rustls
/// `CryptoProvider`. Idempotent — if a provider is already
/// installed (e.g. by a previous call in the same process during
/// tests), the second call is a no-op rather than a panic.
///
/// rustls 0.23+ removed the auto-default provider, so callers of
/// any rustls-using API (including kube's `Client::try_default`)
/// must install one explicitly at startup. Without this, the
/// operator pod panicked at runtime on:
///
/// ```text
/// thread 'main' panicked at rustls-0.23.40/src/crypto/mod.rs:249:14:
///   Could not automatically determine the process-level
///   CryptoProvider from Rustls crate features.
/// ```
///
/// Manifested only at pod-startup in a real cluster (v0.1.60
/// integration), invisible to unit tests + `cargo run` against a
/// local kubeconfig where TLS sometimes short-circuits before
/// the crypto provider is needed. The regression-guard test in
/// this module proves the install succeeds in CI; the
/// helm-deployable image carries the call in `main.rs`.
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
        // Regression guard for the v0.1.61 fix: without this call,
        // rustls 0.23 panics inside Client::try_default with
        // "Could not automatically determine the process-level
        // CryptoProvider". Calling our helper must result in
        // `CryptoProvider::get_default()` returning `Some`.
        install_rustls_crypto_provider();
        assert!(
            rustls::crypto::CryptoProvider::get_default().is_some(),
            "after install_rustls_crypto_provider() the process-level CryptoProvider \
             must be set; otherwise the operator pod panics at startup deep inside \
             kube::Client::try_default()"
        );
    }

    #[test]
    fn install_rustls_crypto_provider_is_idempotent() {
        // Tests in the same crate run in a shared process, so an
        // earlier test may have installed a provider already. The
        // second call must not panic — install_default returns Err
        // (not panic) on a re-install, and we explicitly swallow
        // that Err. Without this property, a second main() call
        // (or an init-then-test pattern) would crash.
        install_rustls_crypto_provider();
        install_rustls_crypto_provider();
        assert!(rustls::crypto::CryptoProvider::get_default().is_some());
    }
}
