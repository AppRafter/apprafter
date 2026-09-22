// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The runner's kube client builds its TLS stack without a crypto-provider
//! panic, and the provider it ends up with is ring.
//!
//! A test binary of its own — so a process of its own — on purpose: the rustls
//! process-level provider is global and set-once, and the first assertion
//! below is that NOTHING has installed one yet. In a shared process another
//! test could have, and the rest would then prove nothing.
//!
//! No network: `Client::try_from` builds the rustls config EAGERLY, and that
//! construction is where "Could not automatically determine the process-level
//! CryptoProvider" is raised. The kubeconfig carries a real (throwaway,
//! self-signed) CA, so the builder takes the same root-certificate branch a
//! kubeconfig with `certificate-authority-data` takes in production.

/// A self-signed EC P-256 CA generated for this test alone (valid to 2126).
/// Nothing trusts it; it only has to parse.
const TEST_CA_PEM_B64: &str = concat!(
    "LS0tLS1CRUdJTiBDRVJUSUZJQ0FURS0tLS0tCk1JSUJuRENDQVVPZ0F3SUJBZ0lVZG1zci9UNm5Y",
    "ZktHQnEvZ2NlaGozazVRb3FBd0NnWUlLb1pJemowRUF3SXcKSXpFaE1COEdBMVVFQXd3WVlYQndj",
    "bUZtZEdWeUxXSmhZMnQxY0MxMFpYTjBMV05oTUNBWERUSTJNRGt5TWpFNApNVEExT1ZvWUR6SXhN",
    "all3T0RJNU1UZ3hNRFU1V2pBak1TRXdId1lEVlFRRERCaGhjSEJ5WVdaMFpYSXRZbUZqCmEzVndM",
    "WFJsYzNRdFkyRXdXVEFUQmdjcWhrak9QUUlCQmdncWhrak9QUU1CQndOQ0FBUWJRWXhjSzE4K3Er",
    "OXoKLytHS2s5WXVKTjJBMUdtNUJnclZXMXEvU0FxR2lWV0ZDdmJXN0d6dFQrdndKZ3dGYklKT0lT",
    "STV6R2R0dlZLWgo3NkdUVTZ5em8xTXdVVEFkQmdOVkhRNEVGZ1FVemtJcFhvYUp5aE5QUU5VOWxn",
    "WlFXc3hVb08wd0h3WURWUjBqCkJCZ3dGb0FVemtJcFhvYUp5aE5QUU5VOWxnWlFXc3hVb08wd0R3",
    "WURWUjBUQVFIL0JBVXdBd0VCL3pBS0JnZ3EKaGtqT1BRUURBZ05IQURCRUFpQUZacHFBcU82NFVC",
    "MnhoWnJ4dXdZdlh2SnhDcVJVd05hNXVTakVMaEpEblFJZwpLRjYrWFNCSW53RlNSREV3VXpURU5B",
    "bWZ3TDlkYlNuVDRzTWk5NFZMdXBZPQotLS0tLUVORCBDRVJUSUZJQ0FURS0tLS0tCg==",
);

fn kubeconfig_yaml() -> String {
    format!(
        "apiVersion: v1
kind: Config
clusters:
- name: test
  cluster:
    server: https://127.0.0.1:6443
    certificate-authority-data: {TEST_CA_PEM_B64}
users:
- name: test
  user:
    token: not-a-real-token
contexts:
- name: test
  context:
    cluster: test
    user: test
    namespace: default
current-context: test
"
    )
}

/// The key-exchange groups a provider offers, by name — enough to tell ring's
/// default provider from aws-lc-rs', whose defaults include a post-quantum
/// hybrid group ring does not implement.
fn kx_names(p: &rustls::crypto::CryptoProvider) -> Vec<String> {
    p.kx_groups
        .iter()
        .map(|g| format!("{:?}", g.name()))
        .collect()
}

#[test]
fn the_runner_builds_its_kube_client_with_ring_installed() {
    use rustls::crypto::CryptoProvider;

    assert!(
        CryptoProvider::get_default().is_none(),
        "a crypto provider was installed before the runner's own code ran, so this test \
         could not tell whether `tls::kube_client` installs one"
    );

    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    let kc = kube::config::Kubeconfig::from_yaml(&kubeconfig_yaml()).expect("parse the kubeconfig");
    let config = rt
        .block_on(kube::Config::from_custom_kubeconfig(
            kc,
            &kube::config::KubeConfigOptions::default(),
        ))
        .expect("resolve the kubeconfig into a client config");
    assert!(
        config.cluster_url.scheme_str() == Some("https") && config.root_cert.is_some(),
        "the fixture must take the TLS + root-certificate branch, or the builder is never reached"
    );

    // `Client` wraps a `tower::Buffer`, which spawns — build inside the runtime.
    let client = {
        let _guard = rt.enter();
        apprafter_backup::tls::kube_client(config)
    };
    client.expect("the kube client builds its rustls config");

    let installed = CryptoProvider::get_default().expect("a process-level provider is installed");
    assert_eq!(
        kx_names(installed),
        kx_names(&rustls::crypto::ring::default_provider()),
        "the installed provider must be ring"
    );

    // Idempotent: `main` is not the only possible caller, and a second install
    // must be a no-op, not a panic.
    apprafter_backup::tls::install_rustls_crypto_provider();
}
