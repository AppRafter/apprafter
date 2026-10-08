// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! WI-417: the operator's client (`apprafter_operator::build_client`) keeps
//! kube's per-request `HTTP` span, under kube's tracing target, so a filter
//! that selects kube's spans (`kube_client=debug`) selects the operator's.
//!
//! This test has a binary of its own. tracing caches each callsite's interest
//! for the whole process. A test running at the same time in the same binary
//! that reaches these callsites first, with no subscriber of its own, could
//! cache "never" for them. That happened in 3 of 4 runs of the lib suite.

use std::sync::{Arc, Mutex};

use apprafter_operator::build_client;
use k8s_openapi::api::core::v1::ConfigMap;
use kube::Api;

const PROBE_CONFIGMAP: &str =
    r#"{"apiVersion":"v1","kind":"ConfigMap","metadata":{"name":"probe","namespace":"default"}}"#;

#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for Captured {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().expect("lock").extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// An apiserver that answers every request with one ConfigMap.
async fn apiserver() -> String {
    let app = axum::Router::new().fallback(|| async {
        (
            [(axum::http::header::CONTENT_TYPE, "application/json")],
            PROBE_CONFIGMAP,
        )
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("serve");
    });
    format!("http://{addr}")
}

#[tokio::test]
async fn every_request_still_gets_kubes_http_span() {
    let captured = Captured::default();
    let writer = captured.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::new("kube_client=debug"))
        .with_span_events(tracing_subscriber::fmt::format::FmtSpan::CLOSE)
        .with_ansi(false)
        .with_writer(move || writer.clone())
        .finish();
    // A current-thread runtime: kube's buffer worker and hyper's tasks run on
    // this thread, where this subscriber is the default.
    let _default = tracing::subscriber::set_default(subscriber);

    let url = apiserver().await;
    let config = kube::Config::new(url.parse().expect("url"));
    let api: Api<ConfigMap> = Api::namespaced(build_client(config).expect("client"), "default");
    api.get("probe").await.expect("the probe ConfigMap");

    let log = String::from_utf8(captured.0.lock().expect("lock").clone()).expect("utf-8");
    let span = log
        .lines()
        .find(|line| line.contains("HTTP{http.method=GET") && line.contains("close"))
        .unwrap_or_else(|| panic!("no closed HTTP span for the GET in:\n{log}"));
    assert!(
        span.contains("/api/v1/namespaces/default/configmaps/probe"),
        "{span}"
    );
    assert!(span.contains("http.status_code=200"), "{span}");
    assert!(span.contains("otel.kind=\"client\""), "{span}");
    assert!(
        log.lines().any(|line| line.contains("requesting")),
        "no on_request event in:\n{log}"
    );
}
