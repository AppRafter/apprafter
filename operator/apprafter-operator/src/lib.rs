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

// WI-400: the Application reconcile's deadline must fire before the read
// timeout above. Otherwise a pass held by one silent socket ends as a read
// error at 295s, filed as `ReconcileFailed`, instead of as
// `ReconcileTimedOut` at its own deadline, and the deadline bounds nothing.
const _: () = assert!(
    operator_controllers_application::RECONCILE_DEADLINE.as_secs() < CLIENT_READ_TIMEOUT.as_secs(),
    "the Application controller's RECONCILE_DEADLINE must be shorter than CLIENT_READ_TIMEOUT"
);

// WI-400: the same holds for every other controller's deadline, one assert
// each. `tests/reconcile_deadline_coverage_test.rs` fails when a controller
// has a deadline without its assert here.
const _: () = assert!(
    operator_controllers_migration::reconcile::RECONCILE_DEADLINE.as_secs()
        < CLIENT_READ_TIMEOUT.as_secs(),
    "the MigrationPlan controller's RECONCILE_DEADLINE must be shorter than CLIENT_READ_TIMEOUT"
);
const _: () = assert!(
    operator_controllers_platform_stack::reconcile::RECONCILE_DEADLINE.as_secs()
        < CLIENT_READ_TIMEOUT.as_secs(),
    "the PlatformStack controller's RECONCILE_DEADLINE must be shorter than CLIENT_READ_TIMEOUT"
);
const _: () = assert!(
    operator_controllers_resourceclaim_provisioner::reconcile::RECONCILE_DEADLINE.as_secs()
        < CLIENT_READ_TIMEOUT.as_secs(),
    "the ResourceClaim provisioner's RECONCILE_DEADLINE must be shorter than CLIENT_READ_TIMEOUT"
);
const _: () = assert!(
    operator_controllers_resourceclaim_provisioner::gc::RECONCILE_DEADLINE.as_secs()
        < CLIENT_READ_TIMEOUT.as_secs(),
    "the RetainedClaim GC's RECONCILE_DEADLINE must be shorter than CLIENT_READ_TIMEOUT"
);
const _: () = assert!(
    operator_controllers_resourceclaim_provisioner::shared_database::RECONCILE_DEADLINE.as_secs()
        < CLIENT_READ_TIMEOUT.as_secs(),
    "the SharedDatabase controller's RECONCILE_DEADLINE must be shorter than CLIENT_READ_TIMEOUT"
);
const _: () = assert!(
    operator_controllers_resourceclaim_provisioner::shared_volume::RECONCILE_DEADLINE.as_secs()
        < CLIENT_READ_TIMEOUT.as_secs(),
    "the SharedVolume controller's RECONCILE_DEADLINE must be shorter than CLIENT_READ_TIMEOUT"
);
const _: () = assert!(
    operator_controllers_resourceclaim_scheduler::RECONCILE_DEADLINE.as_secs()
        < CLIENT_READ_TIMEOUT.as_secs(),
    "the ResourceClaim scheduler's RECONCILE_DEADLINE must be shorter than CLIENT_READ_TIMEOUT"
);
const _: () = assert!(
    operator_controllers_sourcecredential::RECONCILE_DEADLINE.as_secs()
        < CLIENT_READ_TIMEOUT.as_secs(),
    "the SourceCredential controller's RECONCILE_DEADLINE must be shorter than CLIENT_READ_TIMEOUT"
);

/// Put back the two kube-client defaults that kube 4.0 moved, so the
/// operator talks to the apiserver exactly as it did on kube 0.95.
///
/// * `read_timeout`: `Some(295s)` → `None`. Upstream dropped it to keep
///   exec/attach/port-forward sessions alive (the operator opens none) and
///   gave watches their own idle timeout instead. Without it, a plain GET
///   or PUT that the apiserver accepts and never answers blocks its caller
///   for ever. 295s is still far too long for the Lease, so the leader loop
///   bounds each of its own steps (`operator_core::leader`); this is the
///   bound for everything else.
/// * `default_retry`: off → on. kube 4.0 wraps every request in a retry of
///   429/503/504 with up to 15 attempts and delays growing to minutes. The
///   operator's failure handling is built on seeing those errors: the
///   leader loop retries a failed renewal on its own schedule and steps down
///   20s after the last one that succeeded, well inside the 30s Lease, and
///   every controller's `error_policy` requeues on its own backoff. A client
///   that retries inside one call spends that time without reporting a
///   single failure.
pub fn with_operator_client_defaults(mut config: kube::Config) -> kube::Config {
    config.read_timeout = Some(CLIENT_READ_TIMEOUT);
    config.default_retry = false;
    config
}

/// Build the operator's kube [`Client`](kube::Client): kube 4.2's own client
/// stack (`ClientBuilder::try_from`, kube-client's `client/builder.rs`) with
/// ONE change. The hyper-util connection pool keeps no idle connection, so
/// every request is sent on a connection of its own.
///
/// What is kept, in kube's order:
/// * the rustls HTTPS connector (`ConfigExt::rustls_https_connector`) inside
///   a `hyper_timeout::TimeoutConnector` with the config's connect, read and
///   write timeouts;
/// * the base-URI layer, kube's retry layer when `default_retry` asks for it
///   (the operator switches it off, see [`with_operator_client_defaults`]),
///   the auth layer and the extra-headers layer;
/// * kube's `TraceLayer`, with the same span and hooks under kube's tracing
///   target, so each request still gets its `HTTP` span;
/// * kube's handling of `proxy_url`. kube's `http-proxy` and `socks5`
///   features are off in this build, so kube's builder refuses every proxy,
///   and so does this one, with the same error.
///
/// Two parts of kube's builder do not appear here:
/// * gzip decompression, behind kube's `gzip` feature, which is off in this
///   build;
/// * `Client::valid_until`, the expiry of a client certificate returned by an
///   exec plugin. kube computes it with a crate-private helper. The operator
///   authenticates with its ServiceAccount token, which carries no such
///   expiry, and never reads `valid_until`.
pub fn build_client(config: kube::Config) -> kube::Result<kube::Client> {
    use hyper::body::Incoming;
    use hyper::{HeaderMap, Request, Response};
    use kube::client::{retry::RetryPolicy, Body, ConfigExt};
    use std::time::Duration;
    use tower::{retry::RetryLayer, ServiceBuilder};
    use tower_http::classify::ServerErrorsFailureClass;
    use tower_http::trace::TraceLayer;
    use tracing::{debug, debug_span, error, Span};

    // kube's builder installs the same provider when none is set, first.
    install_rustls_crypto_provider();
    if let Some(proxy_url) = config.proxy_url.as_ref() {
        return Err(proxy_refused(proxy_url));
    }

    let default_ns = config.default_namespace.clone();
    let auth_layer = config.auth_layer()?;

    let client: hyper_util::client::legacy::Client<_, Body> = {
        let mut connector = hyper_timeout::TimeoutConnector::new(config.rustls_https_connector()?);
        connector.set_connect_timeout(config.connect_timeout);
        connector.set_read_timeout(config.read_timeout);
        connector.set_write_timeout(config.write_timeout);
        hyper_util::client::legacy::Builder::new(hyper_util::rt::TokioExecutor::new())
            // WI-417 (ATM GOTCHA-57): never reuse a connection. hyper 1.11.1 and
            // hyper-util 0.1.20 can hand a request to an HTTP/1.1 connection
            // that is still streaming a watch. hyper's `Sender::try_send` calls
            // `giver.give()` and only then sends (`client/dispatch.rs:93-115`),
            // and the dispatcher, woken by the previous body read, can re-arm
            // WANT in between (`poll_recv` → `taker.want()`, :188). So a
            // watcher's LIST → WATCH on a connection that has just gone idle can
            // leave a stale WANT. hyper-util then puts the connection back in the
            // pool at response-head time (`client.rs:358`, `is_ready()`) while
            // the watch body still streams. The next request checked out on it
            // is queued in the connection's channel and never written until the
            // watch ends, up to its 290s `timeoutSeconds`. The read timeout
            // cannot cut it, because a queued request reads no socket. Right
            // after the operator takes the Lease, about twenty watchers LIST
            // then WATCH at once, which is how first reconciles stalled
            // (WI-400). With no idle connection kept, the pool is off. The cost
            // is one TCP and TLS handshake per request.
            .pool_max_idle_per_host(0)
            .build(connector)
    };

    let service = ServiceBuilder::new()
        .layer(config.base_uri_layer())
        .option_layer(
            config
                .default_retry
                .then_some(RetryLayer::new(RetryPolicy::server_retry())),
        )
        .option_layer(auth_layer)
        .layer(config.extra_headers_layer()?)
        .layer(
            // kube's own trace layer (kube-client 4.2 `client/builder.rs`),
            // verbatim but for one thing: every span and event names kube's
            // target, so a filter such as `RUST_LOG=kube_client=debug` still
            // selects them. Attribute names follow the OpenTelemetry HTTP
            // semantic conventions.
            TraceLayer::new_for_http()
                .make_span_with(|req: &Request<Body>| {
                    debug_span!(
                        target: KUBE_TRACE_TARGET,
                        "HTTP",
                         http.method = %req.method(),
                         http.url = %req.uri(),
                         http.status_code = tracing::field::Empty,
                         otel.name = req.extensions().get::<&'static str>().unwrap_or(&"HTTP"),
                         otel.kind = "client",
                         otel.status_code = tracing::field::Empty,
                    )
                })
                .on_request(|_req: &Request<Body>, _span: &Span| {
                    debug!(target: KUBE_TRACE_TARGET, "requesting");
                })
                .on_response(
                    |res: &Response<Incoming>, _latency: Duration, span: &Span| {
                        let status = res.status();
                        span.record("http.status_code", status.as_u16());
                        if status.is_client_error() || status.is_server_error() {
                            span.record("otel.status_code", "ERROR");
                        }
                    },
                )
                // Explicitly disable `on_body_chunk`. The default does nothing.
                .on_body_chunk(())
                .on_eos(|_: Option<&HeaderMap>, _duration: Duration, _span: &Span| {
                    debug!(target: KUBE_TRACE_TARGET, "stream closed");
                })
                .on_failure(
                    |ec: ServerErrorsFailureClass, _latency: Duration, span: &Span| {
                        // Called when
                        // - Calling the inner service errored
                        // - Polling `Body` errored
                        // - the response was classified as failure (5xx)
                        // - End of stream was classified as failure
                        span.record("otel.status_code", "ERROR");
                        match ec {
                            ServerErrorsFailureClass::StatusCode(status) => {
                                span.record("http.status_code", status.as_u16());
                                error!(target: KUBE_TRACE_TARGET, "failed with status {status}")
                            }
                            ServerErrorsFailureClass::Error(err) => {
                                error!(target: KUBE_TRACE_TARGET, "failed with error {err}")
                            }
                        }
                    },
                ),
        )
        .map_err(tower::BoxError::from)
        .service(client);

    Ok(kube::Client::new(service, default_ns))
}

/// The tracing target of kube's own request spans and events: the module of
/// kube-client 4.2 that emits them.
const KUBE_TRACE_TARGET: &str = "kube_client::client::builder";

/// The error kube's builder returns for a configured proxy when its
/// `socks5` and `http-proxy` features are off, as they are in this build
/// (kube-client 4.2 `client/builder.rs`, `TryFrom<Config>`).
fn proxy_refused(proxy_url: &hyper::Uri) -> kube::Error {
    match proxy_url.scheme_str() {
        Some("socks5") => kube::Error::ProxyProtocolDisabled {
            proxy_url: proxy_url.clone(),
            protocol_feature: "kube/socks5",
        },
        Some("http") | Some("https") => kube::Error::ProxyProtocolDisabled {
            proxy_url: proxy_url.clone(),
            protocol_feature: "kube/http-proxy",
        },
        _ => kube::Error::ProxyProtocolUnsupported {
            proxy_url: proxy_url.clone(),
        },
    }
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

    /// The property the leader loop depends on, observed through the
    /// operator's real client, built from a `Config` by `build_client` (the
    /// retry layer lives in that builder, so a `Client::new(service)` test
    /// could not see it): a 503 surfaces to the caller on the FIRST attempt,
    /// as it did on kube 0.95.
    #[tokio::test]
    async fn a_503_reaches_the_caller_instead_of_being_retried_inside_the_call() {
        use std::sync::atomic::Ordering;
        let (url, hits) = flaky_apiserver().await;
        let client = build_client(with_operator_client_defaults(config_for(&url))).expect("client");
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

    // -----------------------------------------------------------------
    // build_client
    // -----------------------------------------------------------------

    const PROBE_CONFIGMAP: &str = r#"{"apiVersion":"v1","kind":"ConfigMap","metadata":{"name":"probe","namespace":"default"}}"#;

    /// An apiserver that answers every request with one ConfigMap and keeps
    /// the connection open for the next request (HTTP/1.1 keep-alive), so a
    /// client that reuses connections can. Returns its URL and the number of
    /// TCP connections it has accepted.
    async fn connection_counting_apiserver(
    ) -> (String, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let accepted = Arc::new(AtomicUsize::new(0));
        let counter = accepted.clone();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        tokio::spawn(async move {
            loop {
                let (mut socket, _) = listener.accept().await.expect("accept");
                counter.fetch_add(1, Ordering::SeqCst);
                tokio::spawn(async move {
                    let mut buf = Vec::new();
                    let mut chunk = [0u8; 4096];
                    loop {
                        // A GET has no body: the request ends with its head.
                        let end = loop {
                            if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                                break i + 4;
                            }
                            match socket.read(&mut chunk).await {
                                Ok(0) | Err(_) => return,
                                Ok(n) => buf.extend_from_slice(&chunk[..n]),
                            }
                        };
                        buf.drain(..end);
                        let response = format!(
                            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{PROBE_CONFIGMAP}",
                            PROBE_CONFIGMAP.len()
                        );
                        if socket.write_all(response.as_bytes()).await.is_err() {
                            return;
                        }
                    }
                });
            }
        });
        (format!("http://{addr}"), accepted)
    }

    /// WI-417: the operator's client hands no request to a connection that an
    /// earlier request used, so none can be queued behind a watch that is
    /// still streaming on it (see `build_client`). Three sequential GETs,
    /// which kube's stock client sends on one kept-alive connection, open
    /// three connections.
    #[tokio::test]
    async fn the_operator_client_opens_a_connection_for_every_request() {
        use std::sync::atomic::Ordering;
        let (url, accepted) = connection_counting_apiserver().await;
        let client = build_client(config_for(&url)).expect("client");
        let api: kube::Api<k8s_openapi::api::core::v1::ConfigMap> =
            kube::Api::namespaced(client, "default");
        for _ in 0..3 {
            api.get("probe").await.expect("the probe ConfigMap");
        }
        assert_eq!(
            accepted.load(Ordering::SeqCst),
            3,
            "three sequential requests must open three connections: a request \
             sent on a reused connection can queue behind a streaming watch"
        );
    }

    /// `build_client` keeps kube's in-call retry for a `Config` that asks for
    /// it, as kube's own builder does (the contrast test above): only the
    /// connection pool differs.
    #[tokio::test]
    async fn the_operator_client_keeps_kubes_retry_when_the_config_asks_for_it() {
        use std::sync::atomic::Ordering;
        let (url, hits) = flaky_apiserver().await;
        let client = build_client(config_for(&url)).expect("client");
        let api: kube::Api<k8s_openapi::api::core::v1::ConfigMap> =
            kube::Api::namespaced(client, "default");
        api.get("probe")
            .await
            .expect("kube's retry turns the 503 into a success");
        assert_eq!(hits.load(Ordering::SeqCst), 2);
    }

    /// A configured proxy is refused with the error kube's own builder
    /// returns in this build. Should a kube feature ever make the stock
    /// builder accept a proxy, this fails instead of the two drifting apart.
    #[test]
    fn a_proxy_is_refused_with_kubes_own_error() {
        for proxy in [
            "socks5://127.0.0.1:1080",
            "http://127.0.0.1:3128",
            "https://127.0.0.1:3128",
            "ftp://127.0.0.1:21",
        ] {
            let mut config = config_for("https://127.0.0.1:6443");
            config.proxy_url = Some(proxy.parse().expect("proxy url"));
            let Err(stock) = kube::Client::try_from(config.clone()) else {
                panic!("kube's builder accepted the proxy {proxy}");
            };
            let Err(ours) = build_client(config) else {
                panic!("build_client accepted the proxy {proxy}");
            };
            assert!(
                matches!(
                    ours,
                    kube::Error::ProxyProtocolDisabled { .. }
                        | kube::Error::ProxyProtocolUnsupported { .. }
                ),
                "{proxy}: {ours:?}"
            );
            assert_eq!(format!("{ours:?}"), format!("{stock:?}"), "{proxy}");
        }
    }
}
