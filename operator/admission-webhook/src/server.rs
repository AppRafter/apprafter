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
    let user_info = request
        .get("userInfo")
        .cloned()
        .unwrap_or_else(|| Value::Object(Default::default()));
    let operation = request
        .get("operation")
        .and_then(Value::as_str)
        .unwrap_or("");

    // The targeted subresource, if any. A status-subresource write
    // (`kubectl patch --subresource=status`, or a direct
    // `patch <resource>/status` API call) sets `request.subResource` to
    // `"status"`; a write to the main object leaves it absent.
    let sub_resource = request.get("subResource").and_then(Value::as_str);

    // 2.16b (S-1): `Application.status` is the root of trust for the
    // app-migration gate (`status.lastAppliedSpec` is the baseline every
    // destructive-change detection diffs against). The ONLY legitimate
    // writer is the operator's SSA field manager (`apprafter-operator`);
    // any other subject with `patch applications/status` RBAC could zero
    // `lastAppliedSpec` and silently disarm the gate. Reject every
    // non-operator write to the `Application/status` subresource. This runs
    // BEFORE the per-kind spec validation `match` below — a status write
    // carries no meaningful `spec` change to validate, only the
    // ownership/writer check.
    if kind == "Application" && sub_resource == Some("status") {
        let field_manager = request_field_manager(request);
        if !crate::validator::status_write_allowed(field_manager) {
            let fm = field_manager.unwrap_or("<none>");
            let message = format!(
                "Application.status is operator-owned; direct status writes are rejected (fieldManager={fm}). The migration gate's baseline (status.lastAppliedSpec) must not be overwritten."
            );
            warn!(
                target: "admission_webhook",
                fieldManager = %fm,
                "rejected non-operator Application.status write"
            );
            return (
                StatusCode::OK,
                Json(json!({
                    "apiVersion": api_version,
                    "kind": "AdmissionReview",
                    "response": {
                        "uid": uid,
                        "allowed": false,
                        "status": {
                            "code": 403,
                            "message": message
                        }
                    }
                })),
            );
        }
    }

    let errors = match kind {
        // A status-subresource write carries no meaningful spec change to
        // validate — the operator-ownership guard above is the whole check
        // for `Application/status`, and the operator's own status object
        // has no `spec`. Skip spec validation for it (allow); the main
        // Application resource still runs the full spec validator.
        "Application" if sub_resource == Some("status") => Vec::new(),
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
        "SourceCredential" => crate::validator_sourcecredential::validate_sourcecredential(&object),
        "ServiceProvider" => crate::validator_serviceprovider::validate_serviceprovider(&object),
        "ResourceClaim" => {
            crate::validator_resourceclaim::validate_resourceclaim(&object, &user_info, operation)
        }
        "RetainedClaim" => crate::validator_retainedclaim::validate_retainedclaim(
            &object,
            old_object.as_ref(),
            &user_info,
            operation,
        ),
        "SharedVolume" => crate::validator::validate_sharedvolume(&object),
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

/// 2.16b (S-1): the `fieldManager` that issued this write, or `None`.
///
/// The apiserver forwards the operation's options object in
/// `request.options` — a `PatchOptions` for PATCH, a `CreateOptions` for
/// CREATE, an `UpdateOptions` for UPDATE — and the SSA/Apply field manager
/// is its `fieldManager` field. The operator's status writes are SSA Apply
/// patches under `PatchParams::apply("apprafter-operator")`, so the value
/// arrives at `request.options.fieldManager`. Returns `None` when the
/// options object is absent or carries no `fieldManager` (e.g. a
/// client-side write that never set one) — the caller treats an absent
/// fieldManager as "not the operator" and denies, which is the safe
/// default since `Application.status` has no external writer.
fn request_field_manager(request: &serde_json::Map<String, Value>) -> Option<&str> {
    request
        .get("options")
        .and_then(|o| o.get("fieldManager"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
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

    /// Like `admission_review_for_kind` but also sets `request.userInfo`
    /// and `request.operation` — required for the ResourceClaim identity gate.
    fn admission_review_with_user(
        kind: &str,
        object: serde_json::Value,
        username: &str,
        groups: &[&str],
        operation: &str,
    ) -> serde_json::Value {
        json!({
            "apiVersion": "admission.k8s.io/v1",
            "kind": "AdmissionReview",
            "request": {
                "uid": "deadbeef",
                "kind": { "group": "apprafter.io", "version": "v1alpha1", "kind": kind },
                "object": object,
                "operation": operation,
                "userInfo": {
                    "username": username,
                    "groups": groups,
                },
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
    async fn accepts_valid_serviceprovider() {
        let router = build_router();
        let body = admission_review_for_kind(
            "ServiceProvider",
            json!({
                "metadata": { "name": "pg-integrated", "namespace": "apprafter-system" },
                "spec": { "type": "pg", "backend": "cloudnative-pg" }
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
    async fn rejects_serviceprovider_with_unknown_type() {
        let router = build_router();
        let body = admission_review_for_kind(
            "ServiceProvider",
            json!({
                "metadata": { "name": "kafka-provider", "namespace": "apprafter-system" },
                "spec": { "type": "kafka", "backend": "strimzi" }
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
        assert!(msg.starts_with("ServiceProvider is invalid: "));
    }

    // ── 2.16b S-1: Application.status ownership guard (end-to-end) ────────

    /// AdmissionReview for a status-subresource write, carrying the SSA
    /// `options.fieldManager` the apiserver forwards for an Apply patch.
    fn admission_status_write(
        kind: &str,
        field_manager: Option<&str>,
        operation: &str,
    ) -> serde_json::Value {
        let options = match field_manager {
            Some(fm) => json!({
                "apiVersion": "meta.k8s.io/v1",
                "kind": "PatchOptions",
                "fieldManager": fm,
            }),
            None => json!({
                "apiVersion": "meta.k8s.io/v1",
                "kind": "PatchOptions",
            }),
        };
        json!({
            "apiVersion": "admission.k8s.io/v1",
            "kind": "AdmissionReview",
            "request": {
                "uid": "deadbeef",
                "kind": { "group": "apprafter.io", "version": "v1alpha1", "kind": kind },
                "subResource": "status",
                "operation": operation,
                "object": {
                    "metadata": { "name": "web", "namespace": "demo" },
                    "status": { "lastAppliedSpec": null }
                },
                "options": options,
            }
        })
    }

    async fn post_review(body: serde_json::Value) -> serde_json::Value {
        let router = build_router();
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
        serde_json::from_slice(&bytes).unwrap()
    }

    #[tokio::test]
    async fn allows_application_status_write_from_operator_manager() {
        let parsed = post_review(admission_status_write(
            "Application",
            Some("apprafter-operator"),
            "UPDATE",
        ))
        .await;
        assert_eq!(
            parsed["response"]["allowed"],
            json!(true),
            "the operator's own SSA status write must pass, or the reconcile breaks"
        );
    }

    #[tokio::test]
    async fn rejects_application_status_write_from_other_manager() {
        let parsed = post_review(admission_status_write(
            "Application",
            Some("kubectl-client-side-apply"),
            "UPDATE",
        ))
        .await;
        assert_eq!(parsed["response"]["allowed"], json!(false));
        let msg = parsed["response"]["status"]["message"].as_str().unwrap();
        assert!(
            msg.contains("operator-owned"),
            "expected ownership message, got {msg:?}"
        );
        assert!(
            msg.contains("kubectl-client-side-apply"),
            "expected the offending fieldManager in the message, got {msg:?}"
        );
        assert!(
            msg.contains("lastAppliedSpec"),
            "expected the baseline reference in the message, got {msg:?}"
        );
    }

    #[tokio::test]
    async fn rejects_application_status_write_with_no_field_manager() {
        let parsed = post_review(admission_status_write("Application", None, "UPDATE")).await;
        assert_eq!(
            parsed["response"]["allowed"],
            json!(false),
            "an absent fieldManager must be denied (only the operator writes Application.status)"
        );
    }

    #[tokio::test]
    async fn allows_application_main_resource_write_regardless_of_manager() {
        // A write to the MAIN Application resource (no subResource) is NOT
        // status-gated — a valid spec from any writer passes as before.
        let body = admission_review_for_kind(
            "Application",
            json!({ "spec": { "base": { "image": "ghcr.io/acme/web:1.0" } } }),
        );
        let parsed = post_review(body).await;
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

    // ── ResourceClaim identity-gate end-to-end tests ──────────────────────

    fn valid_claim_object() -> serde_json::Value {
        json!({
            "metadata": { "name": "demo-web-pg", "namespace": "demo" },
            "spec": { "type": "pg", "selector": { "tier": "integrated" } }
        })
    }

    #[tokio::test]
    async fn allows_resourceclaim_create_from_operator_sa() {
        let router = build_router();
        let body = admission_review_with_user(
            "ResourceClaim",
            valid_claim_object(),
            "system:serviceaccount:apprafter-system:apprafter-operator",
            &["system:serviceaccounts"],
            "CREATE",
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
        assert_eq!(parsed["response"]["allowed"], json!(true));
    }

    #[tokio::test]
    async fn rejects_resourceclaim_create_from_user() {
        let router = build_router();
        let body = admission_review_with_user(
            "ResourceClaim",
            valid_claim_object(),
            "alice",
            &["system:authenticated"],
            "CREATE",
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
        assert!(
            msg.starts_with("ResourceClaim is invalid: "),
            "expected prefix 'ResourceClaim is invalid: ', got {msg:?}"
        );
        assert!(
            msg.contains("apprafter-operator"),
            "expected 'apprafter-operator' in message, got {msg:?}"
        );
    }

    #[tokio::test]
    async fn allows_resourceclaim_update_from_user() {
        let router = build_router();
        let body = admission_review_with_user(
            "ResourceClaim",
            valid_claim_object(),
            "alice",
            &["system:authenticated"],
            "UPDATE",
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
        assert_eq!(
            parsed["response"]["allowed"],
            json!(true),
            "UPDATE should not be identity-gated"
        );
    }

    #[tokio::test]
    async fn rejects_resourceclaim_create_from_operator_with_bad_type() {
        let router = build_router();
        let bad_claim = json!({
            "metadata": { "name": "demo-web-kafka", "namespace": "demo" },
            "spec": { "type": "kafka", "selector": { "tier": "integrated" } }
        });
        let body = admission_review_with_user(
            "ResourceClaim",
            bad_claim,
            "system:serviceaccount:apprafter-system:apprafter-operator",
            &["system:serviceaccounts"],
            "CREATE",
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
        assert!(
            msg.starts_with("ResourceClaim is invalid: "),
            "expected prefix 'ResourceClaim is invalid: ', got {msg:?}"
        );
        assert!(
            msg.contains("spec.type"),
            "expected 'spec.type' in message, got {msg:?}"
        );
    }
}
