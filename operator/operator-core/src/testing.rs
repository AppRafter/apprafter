// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Test fixtures for the controller crates (feature `testing`).
//!
//! A controller crate turns the feature on from its `[dev-dependencies]`:
//!
//! ```toml
//! [dev-dependencies]
//! operator-core = { path = "../../operator-core", features = ["testing"] }
//! ```
//!
//! `--all-features` in `just lint`/`just test` and CI turns it on as well.
//! Neither reaches the release image build
//! (`cargo build --release --locked --bin ...`), and resolver 2 does not
//! unify a dev-dependency's features into that build, so this module never
//! reaches a shipped binary.
//!
//! The feature also turns on tokio's `test-util`, which
//! `#[tokio::test(start_paused = true)]` needs: a stalled client is only
//! useful on a paused clock, where waiting out a 120s deadline costs no wall
//! time.

use kube::client::Body;
use kube::Client;

/// A `kube::Client` whose every request is accepted and never answered.
///
/// This is the GOTCHA-51 stall: an apiserver that took the request and went
/// quiet. Nothing in the client bounds it — a `Client` built over a service
/// has none of the read/connect timeouts `Client::try_default` layers in — so
/// a reconcile driven with this client returns only if something above it
/// gives up, which is what a controller's deadline test asserts. On a paused
/// clock (`#[tokio::test(start_paused = true)]`) the wait costs no wall time.
///
/// Its default namespace is `"default"`.
///
/// # Panics
///
/// Outside a Tokio runtime: `Client::new` spawns the client's request buffer
/// onto the current one.
pub fn stalled_client() -> Client {
    let service = tower::service_fn(|_request: http::Request<Body>| {
        std::future::pending::<Result<http::Response<Body>, std::convert::Infallible>>()
    });
    Client::new(service, "default")
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::time::Duration;

    use k8s_openapi::api::core::v1::ConfigMap;
    use kube::api::{Api, Patch, PatchParams};
    use tokio::time::Instant;

    const AN_HOUR: Duration = Duration::from_secs(3600);

    #[tokio::test(start_paused = true)]
    async fn a_get_through_the_stalled_client_never_completes() {
        let client = stalled_client();
        assert_eq!(client.default_namespace(), "default");
        let config_maps: Api<ConfigMap> = Api::default_namespaced(client);

        let started = Instant::now();
        let get = tokio::time::timeout(AN_HOUR, config_maps.get("web")).await;
        assert!(
            get.is_err(),
            "a GET through the stalled client completed: {get:?}"
        );
        assert_eq!(started.elapsed(), AN_HOUR);
    }

    #[tokio::test(start_paused = true)]
    async fn every_request_hangs_not_only_the_first() {
        let config_maps: Api<ConfigMap> = Api::default_namespaced(stalled_client());
        let ssa = PatchParams::apply("apprafter-operator");
        let body = serde_json::json!({ "apiVersion": "v1", "kind": "ConfigMap" });
        let patch = Patch::Apply(&body);

        let (get, apply) = tokio::join!(
            tokio::time::timeout(AN_HOUR, config_maps.get("web")),
            tokio::time::timeout(AN_HOUR, config_maps.patch("web", &ssa, &patch)),
        );
        assert!(get.is_err(), "the GET completed: {get:?}");
        assert!(apply.is_err(), "the server-side apply completed: {apply:?}");
    }
}
