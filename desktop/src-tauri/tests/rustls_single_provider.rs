// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Exactly ONE rustls crypto provider is compiled into the desktop workspace, and it is ring —
//! the same rule, and the same test, as the CLI workspace's
//! (`cli/apprafter-backup/tests/rustls_single_provider.rs`).
//!
//! rustls chooses a provider from its own crate features only when exactly one of `ring` /
//! `aws-lc-rs` is enabled. With both — or neither — every `rustls::ClientConfig::builder()`
//! made before a provider is installed explicitly panics with "Could not automatically
//! determine the process-level CryptoProvider from Rustls crate features". The app installs
//! ring itself first thing (`runtime::install_crypto`, called by `run`), so it would survive a
//! second provider; any other code in the graph that relies on crate-feature selection — a
//! test, a client a plugin builds before that install — would not, and a second provider
//! also links a second native crypto library (aws-lc-sys, a C build of its own) into the app.
//!
//! The usual way to get two is a dependency that enables rustls' DEFAULT features, which
//! include aws-lc-rs. Tauri plugins bring HTTP clients and rustls with them — the updater
//! (D.13) is the first — and a plugin's default TLS features are exactly that: this test is
//! what keeps aws-lc-rs out when one lands.
//!
//! So it never calls `runtime::install_crypto`: an installed provider would hide what the
//! crate features select. Feature unification makes the workspace test build a SUPERSET of the
//! shipped binary's own build, so one provider here means at most one there; and the app names
//! ring itself, so never zero.
//!
//! A test binary of its own — so a process of its own — because the provider is global and
//! set-once: nothing may have installed one before this runs.

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
         aws-lc-rs (or neither) are compiled in. `cargo tree -e features -i rustls` (in \
         desktop/) shows who enabled which."
    );

    let chosen = CryptoProvider::get_default().expect("builder() installs the provider it chose");
    assert_eq!(
        kx_names(chosen),
        kx_names(&rustls::crypto::ring::default_provider()),
        "the one compiled-in provider must be ring"
    );
}
