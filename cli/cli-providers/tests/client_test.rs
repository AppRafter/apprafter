// SPDX-License-Identifier: FSL-1.1-MIT
//! HTTP-level tests for HetznerCloudClient using mockito.

use cli_core::CliError;
use cli_providers::hetzner_cloud::HetznerCloudClient;

#[test]
fn get_servers_includes_bearer_header_and_decodes_list() {
    let mut server = mockito::Server::new();
    let m = server
        .mock("GET", "/v1/servers")
        .match_header("Authorization", "Bearer test-token")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(r#"{"servers":[{"id":1,"name":"a","status":"running","labels":{}}]}"#)
        .create();

    let client = HetznerCloudClient::new(server.url(), "test-token");
    let resp = client.list_servers().expect("list_servers should succeed");
    assert_eq!(resp.servers.len(), 1);
    assert_eq!(resp.servers[0].id, 1);
    m.assert();
}

#[test]
fn http_error_maps_to_cli_error_hetzner() {
    let mut server = mockito::Server::new();
    server
        .mock("GET", "/v1/servers")
        .with_status(401)
        .with_header("content-type", "application/json")
        .with_body(r#"{"error":{"code":"unauthorized","message":"bad token"}}"#)
        .create();

    let client = HetznerCloudClient::new(server.url(), "wrong");
    let err = client.list_servers().unwrap_err();
    match err {
        CliError::Hetzner {
            status,
            code,
            message,
            ..
        } => {
            assert_eq!(status, 401);
            assert_eq!(code, "unauthorized");
            assert_eq!(message, "bad token");
        }
        other => panic!("expected Hetzner error, got {other:?}"),
    }
}

#[test]
fn create_server_posts_json_and_returns_id() {
    let mut server = mockito::Server::new();
    let m = server
        .mock("POST", "/v1/servers")
        .match_header("Authorization", "Bearer test-token")
        .match_header("Content-Type", "application/json")
        .match_body(mockito::Matcher::PartialJson(serde_json::json!({
            "name": "platform-1",
            "server_type": "cx22",
            "image": "ubuntu-24.04",
            "location": "nbg1",
            "start_after_create": true
        })))
        .with_status(201)
        .with_header("content-type", "application/json")
        .with_body(
            r#"{
              "server": {
                "id": 12345,
                "name": "platform-1",
                "status": "initializing",
                "labels": {"apprafter": "true"}
              },
              "root_password": "rootpw"
            }"#,
        )
        .create();

    use cli_providers::hetzner_cloud::ServerCreateRequest;
    use std::collections::BTreeMap;

    let client = HetznerCloudClient::new(server.url(), "test-token");
    let mut labels = BTreeMap::new();
    labels.insert("apprafter".into(), "true".into());
    let req = ServerCreateRequest {
        name: "platform-1".into(),
        server_type: "cx22".into(),
        image: "ubuntu-24.04".into(),
        location: "nbg1".into(),
        labels,
        start_after_create: true,
        ssh_keys: None,
    };

    let resp = client.create_server(&req).expect("create_server");
    assert_eq!(resp.server.id, 12345);
    assert_eq!(resp.server.name, "platform-1");
    assert_eq!(resp.root_password.as_deref(), Some("rootpw"));
    m.assert();
}

#[test]
fn delete_server_returns_unit_on_success() {
    let mut server = mockito::Server::new();
    let m = server
        .mock("DELETE", "/v1/servers/12345")
        .match_header("Authorization", "Bearer test-token")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(r#"{"action":{"id":99,"status":"success"}}"#)
        .create();

    let client = HetznerCloudClient::new(server.url(), "test-token");
    client.delete_server(12345).expect("delete_server");
    m.assert();
}

#[test]
fn delete_server_404_is_treated_as_already_gone() {
    let mut server = mockito::Server::new();
    server
        .mock("DELETE", "/v1/servers/99999")
        .with_status(404)
        .with_header("content-type", "application/json")
        .with_body(r#"{"error":{"code":"not_found","message":"server not found"}}"#)
        .create();

    let client = HetznerCloudClient::new(server.url(), "test-token");
    // Idempotent delete: 404 is not an error from our perspective.
    client
        .delete_server(99999)
        .expect("delete should be idempotent");
}

#[test]
fn list_ssh_keys_returns_filtered_list() {
    let mut server = mockito::Server::new();
    let m = server
        .mock("GET", "/v1/ssh_keys")
        .match_header("Authorization", "Bearer test-token")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(
            r#"{"ssh_keys":[{"id":7,"name":"k1","public_key":"ssh-ed25519 AAAA","fingerprint":"a","labels":{"apprafter":"true"}}]}"#,
        )
        .create();

    let client = HetznerCloudClient::new(server.url(), "test-token");
    let resp = client.list_ssh_keys().expect("list_ssh_keys");
    assert_eq!(resp.ssh_keys.len(), 1);
    assert_eq!(resp.ssh_keys[0].id, 7);
    m.assert();
}

#[test]
fn create_ssh_key_posts_json_and_returns_id() {
    let mut server = mockito::Server::new();
    let m = server
        .mock("POST", "/v1/ssh_keys")
        .match_header("Authorization", "Bearer test-token")
        .match_header("Content-Type", "application/json")
        .match_body(mockito::Matcher::PartialJson(serde_json::json!({
            "name": "platform-1-key",
            "public_key": "ssh-ed25519 AAAA"
        })))
        .with_status(201)
        .with_header("content-type", "application/json")
        .with_body(
            r#"{"ssh_key":{"id":42,"name":"platform-1-key","public_key":"ssh-ed25519 AAAA","fingerprint":"f","labels":{"apprafter":"true"}}}"#,
        )
        .create();

    use cli_providers::hetzner_cloud::SshKeyCreateRequest;
    use std::collections::BTreeMap;

    let client = HetznerCloudClient::new(server.url(), "test-token");
    let mut labels = BTreeMap::new();
    labels.insert("apprafter".into(), "true".into());
    let req = SshKeyCreateRequest {
        name: "platform-1-key".into(),
        public_key: "ssh-ed25519 AAAA".into(),
        labels,
    };
    let resp = client.create_ssh_key(&req).expect("create_ssh_key");
    assert_eq!(resp.ssh_key.id, 42);
    assert_eq!(resp.ssh_key.name, "platform-1-key");
    m.assert();
}

#[test]
fn delete_ssh_key_returns_unit_on_success() {
    let mut server = mockito::Server::new();
    let m = server
        .mock("DELETE", "/v1/ssh_keys/42")
        .match_header("Authorization", "Bearer test-token")
        .with_status(204)
        .create();
    let client = HetznerCloudClient::new(server.url(), "test-token");
    client.delete_ssh_key(42).expect("delete_ssh_key");
    m.assert();
}

#[test]
fn delete_ssh_key_404_is_treated_as_already_gone() {
    let mut server = mockito::Server::new();
    server
        .mock("DELETE", "/v1/ssh_keys/9999")
        .with_status(404)
        .with_header("content-type", "application/json")
        .with_body(r#"{"error":{"code":"not_found","message":"ssh key not found"}}"#)
        .create();
    let client = HetznerCloudClient::new(server.url(), "test-token");
    client
        .delete_ssh_key(9999)
        .expect("delete should be idempotent");
}
