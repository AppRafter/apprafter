// SPDX-License-Identifier: FSL-1.1-MIT
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
