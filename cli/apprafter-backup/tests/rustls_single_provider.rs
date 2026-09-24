// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Exactly ONE rustls crypto provider is compiled into the CLI workspace, and
//! it is ring.
//!
//! rustls chooses a provider from its own crate features only when exactly one
//! of `ring` / `aws-lc-rs` is enabled. With both — or neither — every
//! `rustls::ClientConfig::builder()` made before a provider is installed
//! explicitly panics with "Could not automatically determine the process-level
//! CryptoProvider from Rustls crate features". kube-rs builds its TLS config
//! exactly that way. The backup runner installs ring itself
//! (`apprafter_backup::tls`), so it would survive a second provider; any other
//! code in the graph that relies on crate-feature selection would not, and a
//! second provider also links a second native crypto library into every binary.
//!
//! The usual way to get two is a dependency that enables rustls' DEFAULT
//! features, which include aws-lc-rs (kube >= 0.99 splits the provider into
//! its own `ring` / `aws-lc-rs` feature for the same reason, kube-rs#1717).
//!
//! Feature unification makes the workspace test build a SUPERSET of each
//! shipped binary's own build, so one provider here means at most one there;
//! and the runner names ring itself, so never zero.
//!
//! A test binary of its own — so a process of its own — because the provider is
//! global and set-once: nothing may have installed one before this runs.

fn kx_names(p: &rustls::crypto::CryptoProvider) -> Vec<String> {
    p.kx_groups
        .iter()
        .map(|g| format!("{:?}", g.name()))
        .collect()
}

#[test]
fn exactly_one_rustls_provider_is_compiled_in_and_it_is_ring() {
    use rustls::crypto::CryptoProvider;

    assert!(
        CryptoProvider::get_default().is_none(),
        "a crypto provider was already installed, so crate-feature selection cannot be observed"
    );

    // The implicit path: no provider installed, rustls picks from its features.
    let built = std::panic::catch_unwind(|| {
        rustls::ClientConfig::builder()
            .with_root_certificates(rustls::RootCertStore::empty())
            .with_no_client_auth()
    });
    assert!(
        built.is_ok(),
        "rustls could not choose a crypto provider from its crate features: both ring and \
         aws-lc-rs (or neither) are compiled in. `cargo tree -e features -i rustls` shows who \
         enabled which."
    );

    let chosen = CryptoProvider::get_default().expect("builder() installs the provider it chose");
    assert_eq!(
        kx_names(chosen),
        kx_names(&rustls::crypto::ring::default_provider()),
        "the one compiled-in provider must be ring"
    );
}
