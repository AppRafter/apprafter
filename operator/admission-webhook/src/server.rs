// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! HTTP server for the AppRafter admission webhook.
//!
//! Exposes three routes:
//!   - POST /validate — accepts AdmissionReview JSON, validates the
//!     embedded Application object's spec, and returns a corresponding
//!     AdmissionReview response.
//!   - GET /healthz   — liveness probe.
//!   - GET /readyz    — readiness probe.
//!
//! The AdmissionReview shape is hand-rolled via `serde_json::Value`
//! to avoid pulling in the heavy `kube` crate; the request /
//! response wire format follows admission.k8s.io/v1.

use axum::{
    extract::Json,
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Router,
};
use serde_json::{json, Value};
use tracing::warn;

use crate::validator::{validate_application_spec, ValidationError};

/// Build the axum router. Used by main.rs (binding to a port) and
/// by integration tests (`tower::ServiceExt::oneshot`).
pub fn build_router() -> Router {
    Router::new()
        .route("/validate", post(validate_handler))
        .route("/healthz", get(healthz_handler))
        .route("/readyz", get(readyz_handler))
}

async fn healthz_handler() -> &'static str {
    "ok"
}

async fn readyz_handler() -> &'static str {
    "ready"
}

async fn validate_handler(Json(review): Json<Value>) -> impl IntoResponse {
    let api_version = review
        .get("apiVersion")
        .and_then(Value::as_str)
        .unwrap_or("admission.k8s.io/v1")
        .to_string();
    let request = match review.get("request").and_then(Value::as_object) {
        Some(r) => r,
        None => {
            warn!("AdmissionReview missing request field");
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({
                    "apiVersion": api_version,
                    "kind": "AdmissionReview",
                    "response": {
                        "uid": "",
                        "allowed": false,
                        "status": {
                            "code": 400,
                            "message": "AdmissionReview is missing the request field"
                        }
                    }
                })),
            );
        }
    };

    let uid = request
        .get("uid")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();

    let kind = request
        .get("kind")
        .and_then(|v| v.get("kind"))
        .and_then(Value::as_str)
        .unwrap_or("");

    let object = request
        .get("object")
        .cloned()
        .unwrap_or(Value::Object(Default::default()));
    // `oldObject` is only present on UPDATE operations. The
    // MigrationPlan validator uses it for spec.scope
    // immutability; other validators ignore it.
    let old_object = request.get("oldObject").cloned();

    let errors = match kind {
        "Application" => {
            let spec = object
                .get("spec")
                .cloned()
                .unwrap_or(Value::Object(Default::default()));
            validate_application_spec(&spec)
        }
        "PlatformStack" => crate::validator_platformstack::validate_platformstack(&object),
        "MigrationPlan" => {
            crate::validator_migrationplan::validate_migrationplan(&object, old_object.as_ref())
        }
        _ => {
            // Webhook registered for an unrecognised kind — allow,
            // log once for operator visibility. The
            // ValidatingWebhookConfiguration's rules list is the
            // source of truth for which kinds reach this handler;
            // unknown kinds should not surface in production.
            warn!(target: "admission_webhook", "AdmissionReview for unrecognised kind {kind:?}; allowing");
            return (
                StatusCode::OK,
                Json(json!({
                    "apiVersion": api_version,
                    "kind": "AdmissionReview",
                    "response": {
                        "uid": uid,
                        "allowed": true,
                    }
                })),
            );
        }
    };

    if errors.is_empty() {
        return (
            StatusCode::OK,
            Json(json!({
                "apiVersion": api_version,
                "kind": "AdmissionReview",
                "response": {
                    "uid": uid,
                    "allowed": true,
                }
            })),
        );
    }

    let message = render_error_message(kind, &errors);
    (
        StatusCode::OK,
        Json(json!({
            "apiVersion": api_version,
            "kind": "AdmissionReview",
            "response": {
                "uid": uid,
                "allowed": false,
                "status": {
                    "code": 400,
                    "message": message
                }
            }
        })),
    )
}

fn render_error_message(kind: &str, errors: &[ValidationError]) -> String {
    let mut out = format!("{kind} is invalid: ");
    for (i, e) in errors.iter().enumerate() {
        if i > 0 {
            out.push_str("; ");
        }
        out.push_str(&e.field);
        out.push_str(": ");
        out.push_str(&e.message);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use serde_json::json;
    use tower::ServiceExt;

    #[test]
    fn renders_single_error() {
        let msg = render_error_message(
            "Application",
            &[ValidationError::new("spec.base.image", "is required")],
        );
        assert!(msg.starts_with("Application is invalid: "));
        assert!(msg.contains("spec.base.image: is required"));
    }

    #[test]
    fn renders_multiple_errors_with_separator() {
        let msg = render_error_message(
            "Application",
            &[
                ValidationError::new("a", "x"),
                ValidationError::new("b", "y"),
            ],
        );
        assert!(msg.contains("a: x; b: y"));
    }

    #[test]
    fn renders_platformstack_kind_in_message() {
        let msg = render_error_message(
            "PlatformStack",
            &[ValidationError::new("metadata.name", "must be default")],
        );
        assert!(msg.starts_with("PlatformStack is invalid: "));
        assert!(msg.contains("metadata.name: must be default"));
    }

    fn admission_review_for_kind(kind: &str, object: serde_json::Value) -> serde_json::Value {
        json!({
            "apiVersion": "admission.k8s.io/v1",
            "kind": "AdmissionReview",
            "request": {
                "uid": "deadbeef",
                "kind": { "group": "apprafter.io", "version": "v1alpha1", "kind": kind },
                "object": object,
            }
        })
    }

    #[tokio::test]
    async fn rejects_platformstack_with_wrong_name() {
        let router = build_router();
        let body = admission_review_for_kind(
            "PlatformStack",
            json!({
                "metadata": { "name": "other", "namespace": "apprafter-system" },
                "spec": {
                    "source": {
                        "upstream": "oci://ghcr.io/apprafter/platform-stack",
                        "repoURL": "oci://ghcr.io/apprafter/platform-stack",
                        "checkInterval": "6h"
                    },
                    "values": { "tier": 1 }
                }
            }),
        );

        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/validate")
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(parsed["response"]["allowed"], json!(false));
        let msg = parsed["response"]["status"]["message"].as_str().unwrap();
        assert!(msg.contains("PlatformStack is invalid"));
        assert!(msg.contains("metadata.name"));
    }

    #[tokio::test]
    async fn accepts_valid_platformstack() {
        let router = build_router();
        let body = admission_review_for_kind(
            "PlatformStack",
            json!({
                "metadata": { "name": "default", "namespace": "apprafter-system" },
                "spec": {
                    "source": {
                        "upstream": "oci://ghcr.io/apprafter/platform-stack",
                        "repoURL": "oci://ghcr.io/apprafter/platform-stack",
                        "checkInterval": "6h"
                    },
                    "values": { "tier": 1 }
                }
            }),
        );

        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/validate")
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();

        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(parsed["response"]["allowed"], json!(true));
    }

    #[tokio::test]
    async fn allows_unrecognised_kind() {
        let router = build_router();
        let body = admission_review_for_kind("SomethingElse", json!({}));
        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/validate")
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(parsed["response"]["allowed"], json!(true));
    }
}
