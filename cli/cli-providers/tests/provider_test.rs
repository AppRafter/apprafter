// SPDX-License-Identifier: FSL-1.1-MIT
//! Provider-level tests: plan/apply/destroy diff state vs reality.

use std::collections::BTreeMap;

use cli_providers::hetzner_cloud::{
    HetznerCloudClient, HetznerCloudProvider, ServerSpec, SshKeySpec,
};
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

fn ssh_spec(name: &str) -> SshKeySpec {
    SshKeySpec {
        name: name.into(),
        public_key: "ssh-ed25519 AAAA".into(),
    }
}

#[test]
fn plan_creates_when_server_missing() {
    let mut srv = mockito::Server::new();
    let _list_keys = srv
        .mock("GET", "/v1/ssh_keys")
        .with_status(200)
        .with_body(r#"{"ssh_keys":[]}"#)
        .create();
    let _list_servers = srv
        .mock("GET", "/v1/servers")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(r#"{"servers":[]}"#)
        .create();

    let provider = HetznerCloudProvider {
        client: HetznerCloudClient::new(srv.url(), "tok"),
        spec: spec("platform-1"),
        ssh_keys: vec![],
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
    let _list_keys = srv
        .mock("GET", "/v1/ssh_keys")
        .with_status(200)
        .with_body(r#"{"ssh_keys":[]}"#)
        .create();
    let body = r#"{"servers":[{"id":42,"name":"platform-1","status":"running","labels":{"apprafter":"true"}}]}"#;
    let _list_servers = srv
        .mock("GET", "/v1/servers")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(body)
        .create();

    let provider = HetznerCloudProvider {
        client: HetznerCloudClient::new(srv.url(), "tok"),
        spec: spec("platform-1"),
        ssh_keys: vec![],
    };

    let plan = provider.plan().unwrap();
    assert!(plan.actions.is_empty(), "expected noop, got {plan:?}");
}

#[test]
fn apply_creates_then_second_apply_is_noop() {
    let mut srv = mockito::Server::new();
    // First apply: empty ssh-keys + empty servers, then create.
    let _list_keys_first = srv
        .mock("GET", "/v1/ssh_keys")
        .with_status(200)
        .with_body(r#"{"ssh_keys":[]}"#)
        .expect(1)
        .create();
    let _list_servers_empty = srv
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
        ssh_keys: vec![],
    };

    let outcome = provider.apply().unwrap();
    assert_eq!(outcome.applied, 1);

    // Second apply: list returns the existing server; no POST.
    let _list_keys_second = srv
        .mock("GET", "/v1/ssh_keys")
        .with_status(200)
        .with_body(r#"{"ssh_keys":[]}"#)
        .expect(1)
        .create();
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
    let _list_keys = srv
        .mock("GET", "/v1/ssh_keys")
        .with_status(200)
        .with_body(r#"{"ssh_keys":[]}"#)
        .create();

    let provider = HetznerCloudProvider {
        client: HetznerCloudClient::new(srv.url(), "tok"),
        spec: spec("platform-1"),
        ssh_keys: vec![],
    };

    let outcome = provider.destroy().unwrap();
    assert_eq!(outcome.destroyed, 1);
}

#[test]
fn plan_creates_ssh_key_then_server_when_both_missing() {
    let mut srv = mockito::Server::new();
    let _list_keys = srv
        .mock("GET", "/v1/ssh_keys")
        .with_status(200)
        .with_body(r#"{"ssh_keys":[]}"#)
        .create();
    let _list_servers = srv
        .mock("GET", "/v1/servers")
        .with_status(200)
        .with_body(r#"{"servers":[]}"#)
        .create();

    let provider = HetznerCloudProvider {
        client: HetznerCloudClient::new(srv.url(), "tok"),
        spec: spec("platform-1"),
        ssh_keys: vec![ssh_spec("platform-1-key")],
    };

    let plan = provider.plan().unwrap();
    assert_eq!(
        plan.actions,
        vec![
            Action::CreateSshKey("platform-1-key".into()),
            Action::CreateServer("platform-1".into()),
        ]
    );
}

#[test]
fn plan_noop_when_ssh_key_and_server_present() {
    let mut srv = mockito::Server::new();
    let _list_keys = srv
        .mock("GET", "/v1/ssh_keys")
        .with_status(200)
        .with_body(
            r#"{"ssh_keys":[{"id":7,"name":"platform-1-key","public_key":"ssh-ed25519 AAAA","fingerprint":"f","labels":{"apprafter":"true"}}]}"#,
        )
        .create();
    let _list_servers = srv
        .mock("GET", "/v1/servers")
        .with_status(200)
        .with_body(
            r#"{"servers":[{"id":42,"name":"platform-1","status":"running","labels":{"apprafter":"true"}}]}"#,
        )
        .create();

    let provider = HetznerCloudProvider {
        client: HetznerCloudClient::new(srv.url(), "tok"),
        spec: spec("platform-1"),
        ssh_keys: vec![ssh_spec("platform-1-key")],
    };

    let plan = provider.plan().unwrap();
    assert!(plan.actions.is_empty(), "got {plan:?}");
}

#[test]
fn apply_creates_ssh_key_then_server_with_ssh_keys_attached() {
    let mut srv = mockito::Server::new();
    let _list_keys_empty = srv
        .mock("GET", "/v1/ssh_keys")
        .with_status(200)
        .with_body(r#"{"ssh_keys":[]}"#)
        .expect(1)
        .create();
    let _list_servers_empty = srv
        .mock("GET", "/v1/servers")
        .with_status(200)
        .with_body(r#"{"servers":[]}"#)
        .expect(1)
        .create();
    let _create_key = srv
        .mock("POST", "/v1/ssh_keys")
        .with_status(201)
        .with_body(
            r#"{"ssh_key":{"id":7,"name":"platform-1-key","public_key":"ssh-ed25519 AAAA","fingerprint":"f","labels":{"apprafter":"true"}}}"#,
        )
        .expect(1)
        .create();
    let _create_server = srv
        .mock("POST", "/v1/servers")
        .match_body(mockito::Matcher::PartialJson(serde_json::json!({
            "name": "platform-1",
            "ssh_keys": [7]
        })))
        .with_status(201)
        .with_body(
            r#"{"server":{"id":42,"name":"platform-1","status":"initializing","labels":{"apprafter":"true"}}}"#,
        )
        .expect(1)
        .create();

    let provider = HetznerCloudProvider {
        client: HetznerCloudClient::new(srv.url(), "tok"),
        spec: spec("platform-1"),
        ssh_keys: vec![ssh_spec("platform-1-key")],
    };

    let outcome = provider.apply().unwrap();
    assert_eq!(outcome.applied, 2);
}

#[test]
fn destroy_deletes_server_then_ssh_key() {
    let mut srv = mockito::Server::new();
    let _list_servers = srv
        .mock("GET", "/v1/servers")
        .with_status(200)
        .with_body(
            r#"{"servers":[{"id":42,"name":"platform-1","status":"running","labels":{"apprafter":"true"}}]}"#,
        )
        .create();
    let _delete_server = srv
        .mock("DELETE", "/v1/servers/42")
        .with_status(200)
        .with_body(r#"{"action":{"id":1,"status":"success"}}"#)
        .expect(1)
        .create();
    let _list_keys = srv
        .mock("GET", "/v1/ssh_keys")
        .with_status(200)
        .with_body(
            r#"{"ssh_keys":[{"id":7,"name":"platform-1-key","public_key":"k","fingerprint":"f","labels":{"apprafter":"true"}}]}"#,
        )
        .create();
    let _delete_key = srv
        .mock("DELETE", "/v1/ssh_keys/7")
        .with_status(204)
        .expect(1)
        .create();

    let provider = HetznerCloudProvider {
        client: HetznerCloudClient::new(srv.url(), "tok"),
        spec: spec("platform-1"),
        ssh_keys: vec![ssh_spec("platform-1-key")],
    };

    let outcome = provider.destroy().unwrap();
    assert_eq!(outcome.destroyed, 2);
}
