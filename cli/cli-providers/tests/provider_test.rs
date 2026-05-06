// SPDX-License-Identifier: FSL-1.1-MIT
//! Provider-level tests: plan/apply/destroy diff state vs reality.

use std::collections::BTreeMap;

use cli_providers::hetzner_cloud::{HetznerCloudClient, HetznerCloudProvider, ServerSpec};
use cli_providers::{Action, Provider};

fn spec(name: &str) -> ServerSpec {
    let mut labels = BTreeMap::new();
    labels.insert("apprafter".into(), "true".into());
    ServerSpec {
        name: name.into(),
        server_type: "cx22".into(),
        image: "ubuntu-24.04".into(),
        location: "nbg1".into(),
        labels,
    }
}

#[test]
fn plan_creates_when_server_missing() {
    let mut srv = mockito::Server::new();
    let _list = srv
        .mock("GET", "/v1/servers")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(r#"{"servers":[]}"#)
        .create();

    let provider = HetznerCloudProvider {
        client: HetznerCloudClient::new(srv.url(), "tok"),
        spec: spec("platform-1"),
    };

    let plan = provider.plan().unwrap();
    assert_eq!(
        plan.actions,
        vec![Action::CreateServer("platform-1".into())]
    );
}

#[test]
fn plan_noop_when_server_already_present() {
    let mut srv = mockito::Server::new();
    let body = r#"{"servers":[{"id":42,"name":"platform-1","status":"running","labels":{"apprafter":"true"}}]}"#;
    let _list = srv
        .mock("GET", "/v1/servers")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(body)
        .create();

    let provider = HetznerCloudProvider {
        client: HetznerCloudClient::new(srv.url(), "tok"),
        spec: spec("platform-1"),
    };

    let plan = provider.plan().unwrap();
    assert!(plan.actions.is_empty(), "expected noop, got {plan:?}");
}

#[test]
fn apply_creates_then_second_apply_is_noop() {
    let mut srv = mockito::Server::new();
    // First apply: list returns empty, then create succeeds.
    let _list_empty = srv
        .mock("GET", "/v1/servers")
        .with_status(200)
        .with_body(r#"{"servers":[]}"#)
        .expect(1)
        .create();
    let _create = srv
        .mock("POST", "/v1/servers")
        .with_status(201)
        .with_body(
            r#"{"server":{"id":42,"name":"platform-1","status":"initializing","labels":{"apprafter":"true"}}}"#,
        )
        .expect(1)
        .create();

    let provider = HetznerCloudProvider {
        client: HetznerCloudClient::new(srv.url(), "tok"),
        spec: spec("platform-1"),
    };

    let outcome = provider.apply().unwrap();
    assert_eq!(outcome.applied, 1);

    // Second apply: list returns the existing server; no POST.
    let _list_present = srv
        .mock("GET", "/v1/servers")
        .with_status(200)
        .with_body(
            r#"{"servers":[{"id":42,"name":"platform-1","status":"running","labels":{"apprafter":"true"}}]}"#,
        )
        .expect(1)
        .create();

    let outcome2 = provider.apply().unwrap();
    assert_eq!(outcome2.applied, 0);
}

#[test]
fn destroy_deletes_each_tagged_server() {
    let mut srv = mockito::Server::new();
    let _list = srv
        .mock("GET", "/v1/servers")
        .with_status(200)
        .with_body(
            r#"{"servers":[{"id":42,"name":"platform-1","status":"running","labels":{"apprafter":"true"}}]}"#,
        )
        .create();
    let _delete = srv
        .mock("DELETE", "/v1/servers/42")
        .with_status(200)
        .with_body(r#"{"action":{"id":1,"status":"success"}}"#)
        .expect(1)
        .create();

    let provider = HetznerCloudProvider {
        client: HetznerCloudClient::new(srv.url(), "tok"),
        spec: spec("platform-1"),
    };

    let outcome = provider.destroy().unwrap();
    assert_eq!(outcome.destroyed, 1);
}
