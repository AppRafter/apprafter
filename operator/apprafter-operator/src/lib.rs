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

/// The client read timeout kube-client applied by default up to 3.x.
pub const CLIENT_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(295);

/// Put back the two kube-client defaults that kube 4.0 moved, so the
/// operator talks to the apiserver exactly as it did on kube 0.95.
///
/// * `read_timeout`: `Some(295s)` → `None`. Upstream dropped it to keep
///   exec/attach/port-forward sessions alive (the operator opens none) and
///   gave watches their own idle timeout instead. Without it, a plain GET
///   or PUT that the apiserver accepts and never answers blocks its caller
///   for ever — including the Lease renewal, which would then hold the
///   `is_leader` gate open while the Lease expires under it.
/// * `default_retry`: off → on. kube 4.0 wraps every request in a retry of
///   429/503/504 with up to 15 attempts and delays growing to minutes. The
///   operator's failure handling is built on seeing those errors: the
///   leader loop counts consecutive renewal failures against a 30s Lease
///   (three misses at a 10s period, then exit), and every controller's
///   `error_policy` requeues on its own backoff. A client that retries
///   inside one call can outlast the Lease without reporting a single
///   failure — two operators reconciling at once.
pub fn with_operator_client_defaults(mut config: kube::Config) -> kube::Config {
    config.read_timeout = Some(CLIENT_READ_TIMEOUT);
    config.default_retry = false;
    config
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

    // -----------------------------------------------------------------
    // with_operator_client_defaults
    // -----------------------------------------------------------------

    fn config_for(url: &str) -> kube::Config {
        kube::Config::new(url.parse().expect("test url"))
    }

    #[test]
    fn the_operator_client_keeps_the_pre_kube4_read_timeout_and_no_retry() {
        let base = config_for("http://127.0.0.1:1");
        // What kube 4.x hands us, so a future upstream default that moves
        // again shows up here rather than on a cluster.
        assert_eq!(base.read_timeout, None, "kube's default moved again");
        assert!(base.default_retry, "kube's default moved again");

        let ours = with_operator_client_defaults(base);
        assert_eq!(ours.read_timeout, Some(std::time::Duration::from_secs(295)));
        assert!(!ours.default_retry);
    }

    /// A 503 on the first attempt, a 200 on any later one; returns the
    /// server's URL and its request counter.
    async fn flaky_apiserver() -> (String, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;
        let hits = Arc::new(AtomicUsize::new(0));
        let counter = hits.clone();
        let app = axum::Router::new().fallback(move || {
            let counter = counter.clone();
            async move {
                let body = if counter.fetch_add(1, Ordering::SeqCst) == 0 {
                    (
                        axum::http::StatusCode::SERVICE_UNAVAILABLE,
                        r#"{"kind":"Status","apiVersion":"v1","status":"Failure","message":"etcd leader changed","reason":"ServiceUnavailable","code":503}"#,
                    )
                } else {
                    (
                        axum::http::StatusCode::OK,
                        r#"{"apiVersion":"v1","kind":"ConfigMap","metadata":{"name":"probe","namespace":"default"}}"#,
                    )
                };
                (
                    body.0,
                    [(axum::http::header::CONTENT_TYPE, "application/json")],
                    body.1,
                )
            }
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve");
        });
        (format!("http://{addr}"), hits)
    }

    /// The property the leader loop depends on, observed through a real
    /// `Client` built from a `Config` (the retry layer lives in that
    /// builder, so a `Client::new(service)` test could not see it): a 503
    /// surfaces to the caller on the FIRST attempt, as it did on kube 0.95.
    #[tokio::test]
    async fn a_503_reaches_the_caller_instead_of_being_retried_inside_the_call() {
        use std::sync::atomic::Ordering;
        let (url, hits) = flaky_apiserver().await;
        let client = kube::Client::try_from(with_operator_client_defaults(config_for(&url)))
            .expect("client");
        let api: kube::Api<k8s_openapi::api::core::v1::ConfigMap> =
            kube::Api::namespaced(client, "default");
        let err = api
            .get("probe")
            .await
            .expect_err("the 503 must reach the caller");
        match err {
            kube::Error::Api(s) => assert_eq!(s.code, 503, "{s:?}"),
            other => panic!("expected an apiserver error, got {other:?}"),
        }
        assert_eq!(
            hits.load(Ordering::SeqCst),
            1,
            "exactly one request on the wire"
        );
    }

    /// The contrast, so the test above cannot pass for the wrong reason (a
    /// server that never answers 200, say): kube 4's own default DOES retry
    /// the same 503 inside the call and hands the caller a success.
    #[tokio::test]
    async fn kube4s_own_default_would_have_hidden_the_503() {
        use std::sync::atomic::Ordering;
        let (url, hits) = flaky_apiserver().await;
        let client = kube::Client::try_from(config_for(&url)).expect("client");
        let api: kube::Api<k8s_openapi::api::core::v1::ConfigMap> =
            kube::Api::namespaced(client, "default");
        api.get("probe")
            .await
            .expect("kube 4 retries the 503 into a success");
        assert_eq!(hits.load(Ordering::SeqCst), 2);
    }
}
