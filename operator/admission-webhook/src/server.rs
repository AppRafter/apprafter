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

    if kind != "Application" {
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

    let spec = request
        .get("object")
        .and_then(|o| o.get("spec"))
        .cloned()
        .unwrap_or(Value::Object(Default::default()));

    let errors = validate_application_spec(&spec);
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

    let message = render_error_message(&errors);
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

fn render_error_message(errors: &[ValidationError]) -> String {
    let mut out = String::from("Application is invalid: ");
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

    #[test]
    fn renders_single_error() {
        let msg = render_error_message(&[ValidationError::new("spec.base.image", "is required")]);
        assert!(msg.starts_with("Application is invalid: "));
        assert!(msg.contains("spec.base.image: is required"));
    }

    #[test]
    fn renders_multiple_errors_with_separator() {
        let msg = render_error_message(&[
            ValidationError::new("a", "x"),
            ValidationError::new("b", "y"),
        ]);
        assert!(msg.contains("a: x; b: y"));
    }
}
