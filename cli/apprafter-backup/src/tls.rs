// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The runner's kube-rs client, built with its rustls crypto provider in place.
//!
//! kube-rs builds its TLS config with `rustls::ClientConfig::builder()`, which
//! needs a PROCESS-LEVEL `CryptoProvider`. When none has been installed, rustls
//! picks one from its own crate features — and that works only while exactly
//! ONE provider (`ring` or `aws-lc-rs`) is compiled in. With none, or with both,
//! the first client construction panics:
//!
//! ```text
//! Could not automatically determine the process-level CryptoProvider from
//! Rustls crate features.
//! ```
//!
//! The operator hit exactly this in v0.1.61 (CrashLoopBackOff on its first real
//! cluster, because no test ever built a TLS client). Here the workspace
//! compiles ring alone today, so crate-feature selection would work — but that
//! is a property of the whole dependency graph, and one new dependency that
//! enables rustls' default features would bring aws-lc-rs in beside it and turn
//! the scheduled backup into a panic on start. Installing ring explicitly makes
//! the runner independent of that: an explicitly installed provider wins over
//! crate-feature selection, however many providers are compiled in.
//!
//! [`kube_client`] is the ONLY way `main` builds its client, so the install
//! cannot be forgotten on the path that ships.

/// Install `ring` as the process-level rustls `CryptoProvider`. Idempotent.
///
/// `install_default` errs only when a provider is already installed — which is
/// the state this function exists to reach — so that error is not a failure.
pub fn install_rustls_crypto_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

/// Build the kube client from `config`, with the crypto provider installed
/// first.
///
/// Must be called inside a Tokio runtime context: the client wraps its service
/// in a `tower::Buffer`, which spawns a worker task.
pub fn kube_client(config: kube::Config) -> kube::Result<kube::Client> {
    install_rustls_crypto_provider();
    kube::Client::try_from(config)
}
