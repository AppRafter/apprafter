// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! axum router for the operator's HTTP listener — `/healthz`,
//! `/readyz`, and `/metrics`.

use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::get;
use axum::Router;
use operator_core::Metrics;

/// Build the axum router. Used by `main.rs` (binding to a port) and
/// by integration tests (`tower::ServiceExt::oneshot`).
pub fn build_router(metrics: Arc<Metrics>) -> Router {
    Router::new()
        .route("/healthz", get(healthz_handler))
        .route("/readyz", get(readyz_handler))
        .route("/metrics", get(metrics_handler))
        .with_state(metrics)
}

async fn healthz_handler() -> &'static str {
    "ok"
}

async fn readyz_handler() -> &'static str {
    "ready"
}

async fn metrics_handler(State(metrics): State<Arc<Metrics>>) -> impl IntoResponse {
    let body = metrics.encode();
    (
        StatusCode::OK,
        [("content-type", "text/plain; version=0.0.4")],
        body,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    async fn get_path(metrics: Arc<Metrics>, path: &str) -> (StatusCode, Vec<u8>) {
        let router = build_router(metrics);
        let response = router
            .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        (status, bytes.to_vec())
    }

    #[tokio::test]
    async fn healthz_returns_200_ok() {
        let metrics = Arc::new(Metrics::new());
        let (status, body) = get_path(metrics, "/healthz").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, b"ok");
    }

    #[tokio::test]
    async fn readyz_returns_200_ready() {
        let metrics = Arc::new(Metrics::new());
        let (status, body) = get_path(metrics, "/readyz").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, b"ready");
    }

    #[tokio::test]
    async fn metrics_returns_prometheus_text_with_three_families() {
        let metrics = Arc::new(Metrics::new());
        // Drive each metric family once so the text encoder emits
        // them — empty families are skipped.
        metrics
            .reconcile_total
            .with_label_values(&["Application", "default", "ok"])
            .inc();
        metrics
            .reconcile_duration
            .with_label_values(&["Application"])
            .observe(0.0);
        metrics
            .reconcile_errors
            .with_label_values(&["Application"])
            .inc();
        let (status, body) = get_path(metrics, "/metrics").await;
        assert_eq!(status, StatusCode::OK);
        let text = String::from_utf8(body).unwrap();
        assert!(text.contains("apprafter_reconcile_total"), "{text}");
        assert!(
            text.contains("apprafter_reconcile_duration_seconds"),
            "{text}"
        );
        assert!(text.contains("apprafter_reconcile_errors_total"), "{text}");
    }
}
