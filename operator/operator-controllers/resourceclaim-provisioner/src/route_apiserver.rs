// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! A route-table apiserver for reconcile-level unit tests (WI-400).
//!
//! Every answer is fixed up front, per exact method and path; nothing is
//! stored, so a write is never read back. That is all a test of ONE pass
//! needs, and it keeps each test's expected traffic in one list.
//!
//! `kube::Client` is a thin wrapper over a `tower::Service`, so a service that
//! answers from a route table exercises the REAL client — URL construction,
//! status-code mapping, (de)serialisation — with no cluster. Each test lists
//! the requests its reconcile is expected to make; anything else is answered
//! with a 500 naming the request, so an unexpected call fails the reconcile
//! loudly instead of being silently satisfied.
//!
//! [`Reply::Never`] is a request that never answers: the hung kubelet, the
//! black-holed apiserver. The bound tests drive it on a paused clock
//! (`#[tokio::test(start_paused = true)]`), where it costs no wall time and
//! the moment a bound fires is exact.
//!
//! A unit-test module rather than `tests/`: the reconcile under test needs
//! the crate's `#[cfg(test)]` fakes (`FakePg`, `FakeRedis`), which an
//! integration test cannot see.

use std::sync::{Arc, Mutex};

use kube::client::Body;
use serde_json::{json, Value};

/// One request, as the apiserver saw it.
#[derive(Clone, Debug)]
pub(crate) struct Call {
    pub method: String,
    /// The path WITHOUT its query string.
    pub path: String,
    /// The request body as JSON, `Null` when empty or not JSON.
    pub body: Value,
}

/// How a route answers.
#[derive(Clone, Debug)]
pub(crate) enum Reply {
    /// This status code with this JSON body.
    Json(u16, Value),
    /// Never — the response future stays pending forever.
    Never,
}

/// One scripted request: an exact method and an exact path.
#[derive(Clone, Debug)]
pub(crate) struct Route {
    method: &'static str,
    path: String,
    reply: Reply,
}

pub(crate) fn route(method: &'static str, path: impl Into<String>, reply: Reply) -> Route {
    Route {
        method,
        path: path.into(),
        reply,
    }
}

/// A `Client` that answers from `routes`, plus the ordered log of every
/// request it was asked to serve (logged before it is answered, so a request
/// that never answers is in the log too).
pub(crate) fn apiserver(routes: Vec<Route>) -> (kube::Client, Arc<Mutex<Vec<Call>>>) {
    let log = Arc::new(Mutex::new(Vec::<Call>::new()));
    let sink = log.clone();
    let routes = Arc::new(routes);
    let service = tower::service_fn(move |req: http::Request<Body>| {
        let sink = sink.clone();
        let routes = routes.clone();
        async move {
            let method = req.method().to_string();
            let path = req.uri().path().to_string();
            let bytes = req.into_body().collect_bytes().await.expect("request body");
            sink.lock().expect("log").push(Call {
                method: method.clone(),
                path: path.clone(),
                body: serde_json::from_slice(&bytes).unwrap_or(Value::Null),
            });
            let reply = routes
                .iter()
                .find(|r| r.method == method && r.path == path)
                .map(|r| r.reply.clone())
                .unwrap_or_else(|| {
                    Reply::Json(
                        500,
                        json!({
                            "kind": "Status", "apiVersion": "v1", "status": "Failure",
                            "reason": "InternalError", "code": 500,
                            "message": format!("unscripted request: {method} {path}"),
                        }),
                    )
                });
            match reply {
                Reply::Json(code, payload) => Ok::<_, std::convert::Infallible>(
                    http::Response::builder()
                        .status(code)
                        .header("content-type", "application/json")
                        .body(Body::from(serde_json::to_vec(&payload).expect("payload")))
                        .expect("response"),
                ),
                Reply::Never => std::future::pending().await,
            }
        }
    });
    (kube::Client::new(service, "apprafter-system"), log)
}

/// The calls made with `method` to `path`, in order.
pub(crate) fn calls_to(log: &Arc<Mutex<Vec<Call>>>, method: &str, path: &str) -> Vec<Call> {
    log.lock()
        .expect("log")
        .iter()
        .filter(|c| c.method == method && c.path == path)
        .cloned()
        .collect()
}
