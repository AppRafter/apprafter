// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! What every controller's `Err(kube::Error::Api(e)) if e.code == N` arm
//! actually sees.
//!
//! kube 3.0 replaced `ErrorResponse { status, message, reason, code }` with
//! the fuller `Status`, and `Error::Api` now carries it boxed. The operator
//! never matches on reason strings — only on `code` (404 = gone, 409 =
//! conflict/already exists, 403 = RBAC, 422 = invalid) — so what must not
//! move is the CODE a given apiserver answer produces. These tests drive the
//! real client through a scripted apiserver and pin it, for the Status bodies
//! the apiserver sends and for the non-JSON body a missing API group gets.
//!
//! One deliberate non-test: a JSON error body that is NOT a Status and lacks
//! `code`. kube 0.95 required `status`+`code` and fell back to the HTTP code;
//! `Status` defaults every field, so such a body now reads as code 0. The
//! apiserver never sends one (every apiserver error is a `metav1.Status`
//! with `code`), and the only non-apiserver bodies the operator reads — the
//! pod- and node-proxy scrapes — turn any error into a string without
//! looking at the code.

use k8s_openapi::api::core::v1::ConfigMap;
use kube::api::{Api, PostParams};
use kube::client::Body;
use kube::Client;
use serde_json::{json, Value};

/// A client whose every request gets `status` with `body` as JSON.
fn apiserver_answering(status: u16, body: Value) -> Client {
    let service = tower::service_fn(move |_req: http::Request<Body>| {
        let body = body.clone();
        async move {
            Ok::<_, std::convert::Infallible>(
                http::Response::builder()
                    .status(status)
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&body).expect("body")))
                    .expect("response"),
            )
        }
    });
    Client::new(service, "default")
}

/// The apiserver's own `metav1.Status` for a failed request.
fn status(code: u16, reason: &str, message: &str) -> Value {
    json!({
        "kind": "Status", "apiVersion": "v1", "metadata": {},
        "status": "Failure", "message": message, "reason": reason, "code": code,
    })
}

async fn get_error(client: Client) -> kube::Error {
    Api::<ConfigMap>::namespaced(client, "default")
        .get("probe")
        .await
        .expect_err("the scripted apiserver only fails")
}

#[tokio::test]
async fn each_apiserver_failure_keeps_its_code_reason_and_message() {
    for (code, reason, message) in [
        (
            403,
            "Forbidden",
            "configmaps \"probe\" is forbidden: User cannot get",
        ),
        (404, "NotFound", "configmaps \"probe\" not found"),
        (
            409,
            "Conflict",
            "Operation cannot be fulfilled on configmaps \"probe\"",
        ),
        (409, "AlreadyExists", "configmaps \"probe\" already exists"),
        (
            422,
            "Invalid",
            "ConfigMap \"probe\" is invalid: data: Invalid value",
        ),
        (500, "InternalError", "etcdserver: request timed out"),
    ] {
        let err = get_error(apiserver_answering(code, status(code, reason, message))).await;
        match err {
            kube::Error::Api(s) => {
                assert_eq!(s.code, code, "{reason}");
                assert_eq!(s.reason, reason);
                assert_eq!(s.message, message);
                // `Display` is what every ReconcileError logs; it kept the
                // `ErrorResponse` shape "<message>: <reason>".
                assert_eq!(s.to_string(), format!("{message}: {reason}"));
            }
            other => panic!("{code} {reason}: expected Error::Api, got {other:?}"),
        }
    }
}

#[tokio::test]
async fn a_non_json_404_still_carries_the_http_code() {
    // The body a request to an API group the cluster does not serve gets
    // (e.g. a CRD that is not installed): plain text, not a Status.
    let service = tower::service_fn(|_req: http::Request<Body>| async {
        Ok::<_, std::convert::Infallible>(
            http::Response::builder()
                .status(404)
                .header("content-type", "text/plain; charset=utf-8")
                .body(Body::from("404 page not found\n".as_bytes().to_vec()))
                .expect("response"),
        )
    });
    match get_error(Client::new(service, "default")).await {
        kube::Error::Api(s) => assert_eq!(s.code, 404, "{s:?}"),
        other => panic!("expected Error::Api, got {other:?}"),
    }
}

#[tokio::test]
async fn get_opt_reads_an_apiserver_404_as_absent_and_nothing_else() {
    let absent = Api::<ConfigMap>::namespaced(
        apiserver_answering(
            404,
            status(404, "NotFound", "configmaps \"probe\" not found"),
        ),
        "default",
    )
    .get_opt("probe")
    .await
    .expect("a 404 is not an error for get_opt");
    assert!(absent.is_none());

    // A 403 must stay an error: reading "forbidden" as "absent" is how a
    // controller missing an RBAC verb goes on to CREATE what already exists.
    let denied = Api::<ConfigMap>::namespaced(
        apiserver_answering(
            403,
            status(403, "Forbidden", "configmaps \"probe\" is forbidden"),
        ),
        "default",
    )
    .get_opt("probe")
    .await
    .expect_err("a 403 is not an absent object");
    assert!(matches!(denied, kube::Error::Api(s) if s.code == 403));
}

#[tokio::test]
async fn a_create_conflict_is_a_409_the_way_the_reapers_match_it() {
    let err = Api::<ConfigMap>::namespaced(
        apiserver_answering(
            409,
            status(409, "AlreadyExists", "configmaps \"probe\" already exists"),
        ),
        "default",
    )
    .create(&PostParams::default(), &ConfigMap::default())
    .await
    .expect_err("409");
    assert!(
        matches!(&err, kube::Error::Api(e) if e.code == 409 && e.reason == "AlreadyExists"),
        "{err:?}"
    );
}
