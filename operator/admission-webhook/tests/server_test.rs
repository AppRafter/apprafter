// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Integration tests for the admission webhook router.

use admission_webhook::build_router;
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;

async fn post_validate(payload: Value) -> (StatusCode, Value) {
    let router = build_router();
    let body = Body::from(serde_json::to_vec(&payload).unwrap());
    let response = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/validate")
                .header("content-type", "application/json")
                .body(body)
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let json: Value = serde_json::from_slice(&bytes).unwrap();
    (status, json)
}

#[tokio::test]
async fn healthz_returns_200_ok() {
    let router = build_router();
    let response = router
        .oneshot(
            Request::builder()
                .uri("/healthz")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(&bytes[..], b"ok");
}

#[tokio::test]
async fn readyz_returns_200_ready() {
    let router = build_router();
    let response = router
        .oneshot(
            Request::builder()
                .uri("/readyz")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(&bytes[..], b"ready");
}

#[tokio::test]
async fn validate_allows_valid_application() {
    let payload = json!({
        "apiVersion": "admission.k8s.io/v1",
        "kind": "AdmissionReview",
        "request": {
            "uid": "abc-123",
            "kind": { "group": "apprafter.io", "version": "v1alpha1", "kind": "Application" },
            "operation": "CREATE",
            "object": {
                "apiVersion": "apprafter.io/v1alpha1",
                "kind": "Application",
                "metadata": { "name": "web", "namespace": "default" },
                "spec": {
                    "base": { "image": "ghcr.io/acme/web:1.0" }
                }
            }
        }
    });
    let (status, body) = post_validate(payload).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["response"]["uid"], "abc-123");
    assert_eq!(body["response"]["allowed"], true);
}

#[tokio::test]
async fn validate_rejects_application_missing_image() {
    let payload = json!({
        "apiVersion": "admission.k8s.io/v1",
        "kind": "AdmissionReview",
        "request": {
            "uid": "u-1",
            "kind": { "group": "apprafter.io", "version": "v1alpha1", "kind": "Application" },
            "operation": "CREATE",
            "object": {
                "apiVersion": "apprafter.io/v1alpha1",
                "kind": "Application",
                "metadata": { "name": "web" },
                "spec": { "base": {} }
            }
        }
    });
    let (status, body) = post_validate(payload).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["response"]["allowed"], false);
    let message = body["response"]["status"]["message"].as_str().unwrap();
    assert!(message.contains("spec.base.image"), "{message}");
    assert!(message.contains("Application is invalid"), "{message}");
}

#[tokio::test]
async fn validate_passes_through_unknown_kinds() {
    // Webhook should never reject non-Application kinds — it's
    // configured only for our own CRD, but defensively we allow
    // anything else through.
    let payload = json!({
        "apiVersion": "admission.k8s.io/v1",
        "kind": "AdmissionReview",
        "request": {
            "uid": "u-2",
            "kind": { "group": "", "version": "v1", "kind": "ConfigMap" },
            "operation": "CREATE",
            "object": { "apiVersion": "v1", "kind": "ConfigMap", "metadata": { "name": "x" } }
        }
    });
    let (status, body) = post_validate(payload).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["response"]["allowed"], true);
}

#[tokio::test]
async fn validate_returns_400_on_missing_request_field() {
    let payload = json!({
        "apiVersion": "admission.k8s.io/v1",
        "kind": "AdmissionReview"
    });
    let (status, body) = post_validate(payload).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["response"]["allowed"], false);
}

#[tokio::test]
async fn validate_echoes_apiversion_back() {
    let payload = json!({
        "apiVersion": "admission.k8s.io/v1beta1",
        "kind": "AdmissionReview",
        "request": {
            "uid": "u-v1beta",
            "kind": { "group": "apprafter.io", "version": "v1alpha1", "kind": "Application" },
            "operation": "CREATE",
            "object": {
                "spec": { "base": { "image": "x" } }
            }
        }
    });
    let (_status, body) = post_validate(payload).await;
    assert_eq!(body["apiVersion"], "admission.k8s.io/v1beta1");
}

// ---------------- MigrationPlan dispatch (B.1.75) ----------------
//
// These tests pin the router's third-kind dispatch path. The
// validator_migrationplan unit tests already cover the
// validation logic itself; here we just confirm
// kind="MigrationPlan" reaches the validator at all + that
// `oldObject` on UPDATE flows through into the immutability
// check.

#[tokio::test]
async fn validate_allows_valid_application_scope_migrationplan() {
    let payload = json!({
        "apiVersion": "admission.k8s.io/v1",
        "kind": "AdmissionReview",
        "request": {
            "uid": "mp-1",
            "kind": { "group": "apprafter.io", "version": "v1alpha1", "kind": "MigrationPlan" },
            "operation": "CREATE",
            "object": {
                "apiVersion": "apprafter.io/v1alpha1",
                "kind": "MigrationPlan",
                "metadata": { "name": "parser-pg-2026", "namespace": "apprafter-system" },
                "spec": {
                    "scope": {
                        "type": "application",
                        "application": {
                            "ref": { "name": "parser", "namespace": "demo" },
                            "environment": "prod"
                        }
                    },
                    "trigger": { "type": "selector-change", "field": "needs.pg.selector" },
                    "approvers": ["alice@company.com"]
                }
            }
        }
    });
    let (status, body) = post_validate(payload).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["response"]["uid"], "mp-1");
    assert_eq!(body["response"]["allowed"], true);
}

#[tokio::test]
async fn validate_rejects_platform_migrationplan_without_components() {
    let payload = json!({
        "apiVersion": "admission.k8s.io/v1",
        "kind": "AdmissionReview",
        "request": {
            "uid": "mp-2",
            "kind": { "group": "apprafter.io", "version": "v1alpha1", "kind": "MigrationPlan" },
            "operation": "CREATE",
            "object": {
                "apiVersion": "apprafter.io/v1alpha1",
                "kind": "MigrationPlan",
                "metadata": { "name": "p-empty", "namespace": "apprafter-system" },
                "spec": {
                    "scope": { "type": "platform", "platform": { "components": [] } },
                    "trigger": { "type": "platform-classification", "field": "spec.pin" }
                }
            }
        }
    });
    let (status, body) = post_validate(payload).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["response"]["allowed"], false);
    let message = body["response"]["status"]["message"].as_str().unwrap();
    assert!(
        message.contains("spec.scope.platform.components"),
        "{message}"
    );
    assert!(message.contains("MigrationPlan is invalid"), "{message}");
}

#[tokio::test]
async fn validate_rejects_application_scope_phase_transition_to_rejected() {
    // Acceptance test #4 for B.1.76: application-scope plans
    // cannot transition to phase=rejected. The admission
    // webhook is the load-bearing guard (no controller-side
    // enforcement needed, since the controller never sees
    // the request).
    let app_scope = json!({
        "type": "application",
        "application": {
            "ref": { "name": "parser", "namespace": "demo" },
            "environment": "prod"
        }
    });
    let payload = json!({
        "apiVersion": "admission.k8s.io/v1",
        "kind": "AdmissionReview",
        "request": {
            "uid": "mp-reject-app",
            "kind": { "group": "apprafter.io", "version": "v1alpha1", "kind": "MigrationPlan" },
            "operation": "UPDATE",
            "object": {
                "apiVersion": "apprafter.io/v1alpha1",
                "kind": "MigrationPlan",
                "metadata": { "name": "app-reject", "namespace": "apprafter-system" },
                "spec": {
                    "scope": app_scope.clone(),
                    "trigger": { "type": "t", "field": "f" }
                },
                "status": { "phase": "rejected" }
            },
            "oldObject": {
                "apiVersion": "apprafter.io/v1alpha1",
                "kind": "MigrationPlan",
                "metadata": { "name": "app-reject", "namespace": "apprafter-system" },
                "spec": {
                    "scope": app_scope,
                    "trigger": { "type": "t", "field": "f" }
                },
                "status": { "phase": "pending-approval" }
            }
        }
    });
    let (status, body) = post_validate(payload).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["response"]["allowed"], false);
    let message = body["response"]["status"]["message"].as_str().unwrap();
    assert!(
        message.contains("application-scope") && message.contains("ADR 0027"),
        "rejection message must explain ADR 0027 rationale; got {message}"
    );
}

#[tokio::test]
async fn validate_allows_platform_scope_phase_transition_to_rejected() {
    let platform_scope = json!({
        "type": "platform",
        "platform": { "components": ["apprafter-operator"] }
    });
    let payload = json!({
        "apiVersion": "admission.k8s.io/v1",
        "kind": "AdmissionReview",
        "request": {
            "uid": "mp-reject-plat",
            "kind": { "group": "apprafter.io", "version": "v1alpha1", "kind": "MigrationPlan" },
            "operation": "UPDATE",
            "object": {
                "apiVersion": "apprafter.io/v1alpha1",
                "kind": "MigrationPlan",
                "metadata": { "name": "platform-reject", "namespace": "apprafter-system" },
                "spec": {
                    "scope": platform_scope.clone(),
                    "trigger": { "type": "t", "field": "f" }
                },
                "status": { "phase": "rejected" }
            },
            "oldObject": {
                "apiVersion": "apprafter.io/v1alpha1",
                "kind": "MigrationPlan",
                "metadata": { "name": "platform-reject", "namespace": "apprafter-system" },
                "spec": {
                    "scope": platform_scope,
                    "trigger": { "type": "t", "field": "f" }
                },
                "status": { "phase": "pending-approval" }
            }
        }
    });
    let (status, body) = post_validate(payload).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["response"]["allowed"], true);
}

#[tokio::test]
async fn validate_rejects_migrationplan_scope_mutation_on_update() {
    // Server-level guard for `oldObject` flow: UPDATE request
    // with a different `spec.scope` shape must reach
    // `validate_migrationplan` with the old value and be
    // rejected.
    let app_scope = json!({
        "type": "application",
        "application": {
            "ref": { "name": "parser", "namespace": "demo" },
            "environment": "prod"
        }
    });
    let platform_scope = json!({
        "type": "platform",
        "platform": { "components": ["apprafter-operator"] }
    });
    let payload = json!({
        "apiVersion": "admission.k8s.io/v1",
        "kind": "AdmissionReview",
        "request": {
            "uid": "mp-3",
            "kind": { "group": "apprafter.io", "version": "v1alpha1", "kind": "MigrationPlan" },
            "operation": "UPDATE",
            "object": {
                "apiVersion": "apprafter.io/v1alpha1",
                "kind": "MigrationPlan",
                "metadata": { "name": "scope-flip", "namespace": "apprafter-system" },
                "spec": {
                    "scope": platform_scope,
                    "trigger": { "type": "t", "field": "f" }
                }
            },
            "oldObject": {
                "apiVersion": "apprafter.io/v1alpha1",
                "kind": "MigrationPlan",
                "metadata": { "name": "scope-flip", "namespace": "apprafter-system" },
                "spec": {
                    "scope": app_scope,
                    "trigger": { "type": "t", "field": "f" }
                }
            }
        }
    });
    let (status, body) = post_validate(payload).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["response"]["allowed"], false);
    let message = body["response"]["status"]["message"].as_str().unwrap();
    assert!(message.contains("spec.scope"), "{message}");
    assert!(message.contains("immutable"), "{message}");
}

#[tokio::test]
async fn validate_allows_single_pat_sourcecredential() {
    let payload = json!({
        "apiVersion": "admission.k8s.io/v1",
        "kind": "AdmissionReview",
        "request": {
            "uid": "sc-1",
            "kind": { "group": "apprafter.io", "version": "v1alpha1", "kind": "SourceCredential" },
            "operation": "CREATE",
            "object": {
                "apiVersion": "apprafter.io/v1alpha1",
                "kind": "SourceCredential",
                "metadata": { "name": "acme", "namespace": "apprafter-system" },
                "spec": {
                    "git": {
                        "backend": { "sealedSecretRef": { "name": "srccred-acme-material" } },
                        "repoPrefixes": ["github.com/acme/"]
                    },
                    "registry": {
                        "backend": { "sealedSecretRef": { "name": "srccred-acme-material" } },
                        "hosts": ["ghcr.io/acme/"]
                    }
                }
            }
        }
    });
    let (status, body) = post_validate(payload).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["response"]["uid"], "sc-1");
    assert_eq!(body["response"]["allowed"], true);
}

#[tokio::test]
async fn validate_rejects_sourcecredential_with_neither_half() {
    let payload = json!({
        "apiVersion": "admission.k8s.io/v1",
        "kind": "AdmissionReview",
        "request": {
            "uid": "sc-2",
            "kind": { "group": "apprafter.io", "version": "v1alpha1", "kind": "SourceCredential" },
            "operation": "CREATE",
            "object": {
                "apiVersion": "apprafter.io/v1alpha1",
                "kind": "SourceCredential",
                "metadata": { "name": "empty", "namespace": "apprafter-system" },
                "spec": {}
            }
        }
    });
    let (status, body) = post_validate(payload).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["response"]["allowed"], false);
    let message = body["response"]["status"]["message"].as_str().unwrap();
    assert!(
        message.contains("at least one of git or registry"),
        "{message}"
    );
    assert!(message.contains("SourceCredential is invalid"), "{message}");
}
