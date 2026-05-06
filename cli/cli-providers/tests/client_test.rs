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
        networks: None,
        firewalls: None,
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

#[test]
fn list_networks_returns_filtered_list() {
    let mut server = mockito::Server::new();
    let m = server
        .mock("GET", "/v1/networks")
        .match_header("Authorization", "Bearer test-token")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(
            r#"{"networks":[{"id":11,"name":"n1","ip_range":"10.0.0.0/16","subnets":[],"labels":{"apprafter":"true"}}]}"#,
        )
        .create();
    let client = HetznerCloudClient::new(server.url(), "test-token");
    let resp = client.list_networks().expect("list_networks");
    assert_eq!(resp.networks.len(), 1);
    assert_eq!(resp.networks[0].id, 11);
    m.assert();
}

#[test]
fn create_network_posts_json_with_subnet_and_returns_id() {
    let mut server = mockito::Server::new();
    let m = server
        .mock("POST", "/v1/networks")
        .match_header("Content-Type", "application/json")
        .match_body(mockito::Matcher::PartialJson(serde_json::json!({
            "name": "platform-1-net",
            "ip_range": "10.0.0.0/16",
            "subnets": [{"type":"cloud","ip_range":"10.0.0.0/24","network_zone":"eu-central"}]
        })))
        .with_status(201)
        .with_header("content-type", "application/json")
        .with_body(
            r#"{"network":{"id":11,"name":"platform-1-net","ip_range":"10.0.0.0/16","subnets":[{"type":"cloud","ip_range":"10.0.0.0/24","network_zone":"eu-central"}],"labels":{"apprafter":"true"}}}"#,
        )
        .create();

    use cli_providers::hetzner_cloud::{NetworkCreateRequest, Subnet};
    use std::collections::BTreeMap;
    let client = HetznerCloudClient::new(server.url(), "test-token");
    let mut labels = BTreeMap::new();
    labels.insert("apprafter".into(), "true".into());
    let req = NetworkCreateRequest {
        name: "platform-1-net".into(),
        ip_range: "10.0.0.0/16".into(),
        subnets: vec![Subnet {
            kind: "cloud".into(),
            ip_range: "10.0.0.0/24".into(),
            network_zone: "eu-central".into(),
        }],
        labels,
    };
    let resp = client.create_network(&req).expect("create_network");
    assert_eq!(resp.network.id, 11);
    m.assert();
}

#[test]
fn delete_network_returns_unit_on_success() {
    let mut server = mockito::Server::new();
    let m = server
        .mock("DELETE", "/v1/networks/11")
        .with_status(204)
        .create();
    let client = HetznerCloudClient::new(server.url(), "test-token");
    client.delete_network(11).expect("delete_network");
    m.assert();
}

#[test]
fn delete_network_404_is_treated_as_already_gone() {
    let mut server = mockito::Server::new();
    server
        .mock("DELETE", "/v1/networks/999")
        .with_status(404)
        .with_body(r#"{"error":{"code":"not_found","message":"x"}}"#)
        .create();
    let client = HetznerCloudClient::new(server.url(), "test-token");
    client
        .delete_network(999)
        .expect("delete_network is idempotent");
}

#[test]
fn list_firewalls_returns_filtered_list() {
    let mut server = mockito::Server::new();
    let m = server
        .mock("GET", "/v1/firewalls")
        .with_status(200)
        .with_body(
            r#"{"firewalls":[{"id":21,"name":"f1","rules":[],"labels":{"apprafter":"true"}}]}"#,
        )
        .create();
    let client = HetznerCloudClient::new(server.url(), "test-token");
    let resp = client.list_firewalls().expect("list_firewalls");
    assert_eq!(resp.firewalls.len(), 1);
    assert_eq!(resp.firewalls[0].id, 21);
    m.assert();
}

#[test]
fn create_firewall_posts_json_with_rules_and_returns_id() {
    let mut server = mockito::Server::new();
    let m = server
        .mock("POST", "/v1/firewalls")
        .match_body(mockito::Matcher::PartialJson(serde_json::json!({
            "name": "platform-1-fw",
            "rules": [{
                "direction": "in",
                "port": "22",
                "protocol": "tcp",
                "source_ips": ["0.0.0.0/0", "::/0"]
            }]
        })))
        .with_status(201)
        .with_body(
            r#"{"firewall":{"id":21,"name":"platform-1-fw","rules":[{"direction":"in","port":"22","protocol":"tcp","source_ips":["0.0.0.0/0","::/0"],"destination_ips":[]}],"labels":{"apprafter":"true"}}}"#,
        )
        .create();

    use cli_providers::hetzner_cloud::{FirewallCreateRequest, FirewallRule};
    use std::collections::BTreeMap;
    let client = HetznerCloudClient::new(server.url(), "test-token");
    let mut labels = BTreeMap::new();
    labels.insert("apprafter".into(), "true".into());
    let req = FirewallCreateRequest {
        name: "platform-1-fw".into(),
        rules: vec![FirewallRule {
            direction: "in".into(),
            port: Some("22".into()),
            protocol: "tcp".into(),
            source_ips: vec!["0.0.0.0/0".into(), "::/0".into()],
            destination_ips: vec![],
            description: None,
        }],
        labels,
    };
    let resp = client.create_firewall(&req).expect("create_firewall");
    assert_eq!(resp.firewall.id, 21);
    m.assert();
}

#[test]
fn delete_firewall_returns_unit_on_success() {
    let mut server = mockito::Server::new();
    let m = server
        .mock("DELETE", "/v1/firewalls/21")
        .with_status(204)
        .create();
    let client = HetznerCloudClient::new(server.url(), "test-token");
    client.delete_firewall(21).expect("delete_firewall");
    m.assert();
}

#[test]
fn delete_firewall_404_is_treated_as_already_gone() {
    let mut server = mockito::Server::new();
    server
        .mock("DELETE", "/v1/firewalls/999")
        .with_status(404)
        .with_body(r#"{"error":{"code":"not_found","message":"x"}}"#)
        .create();
    let client = HetznerCloudClient::new(server.url(), "test-token");
    client
        .delete_firewall(999)
        .expect("delete_firewall idempotent");
}

#[test]
fn list_floating_ips_returns_filtered_list() {
    let mut server = mockito::Server::new();
    let m = server
        .mock("GET", "/v1/floating_ips")
        .with_status(200)
        .with_body(
            r#"{"floating_ips":[{"id":31,"type":"ipv4","ip":"1.2.3.4","name":"egress","server":42,"home_location":{"name":"nbg1"},"labels":{"apprafter":"true"}}]}"#,
        )
        .create();
    let client = HetznerCloudClient::new(server.url(), "test-token");
    let resp = client.list_floating_ips().expect("list_floating_ips");
    assert_eq!(resp.floating_ips.len(), 1);
    assert_eq!(resp.floating_ips[0].id, 31);
    m.assert();
}

#[test]
fn create_floating_ip_posts_json_with_server_and_returns_id() {
    let mut server = mockito::Server::new();
    let m = server
        .mock("POST", "/v1/floating_ips")
        .match_body(mockito::Matcher::PartialJson(serde_json::json!({
            "type": "ipv4",
            "home_location": "nbg1",
            "server": 42,
            "name": "platform-1-egress"
        })))
        .with_status(201)
        .with_body(
            r#"{"floating_ip":{"id":31,"type":"ipv4","ip":"1.2.3.4","name":"platform-1-egress","server":42,"home_location":{"name":"nbg1"},"labels":{"apprafter":"true"}}}"#,
        )
        .create();

    use cli_providers::hetzner_cloud::FloatingIpCreateRequest;
    use std::collections::BTreeMap;
    let client = HetznerCloudClient::new(server.url(), "test-token");
    let mut labels = BTreeMap::new();
    labels.insert("apprafter".into(), "true".into());
    let req = FloatingIpCreateRequest {
        kind: "ipv4".into(),
        home_location: "nbg1".into(),
        server: Some(42),
        name: Some("platform-1-egress".into()),
        labels,
    };
    let resp = client.create_floating_ip(&req).expect("create_floating_ip");
    assert_eq!(resp.floating_ip.id, 31);
    assert_eq!(resp.floating_ip.ip, "1.2.3.4");
    m.assert();
}

#[test]
fn delete_floating_ip_returns_unit_on_success() {
    let mut server = mockito::Server::new();
    let m = server
        .mock("DELETE", "/v1/floating_ips/31")
        .with_status(204)
        .create();
    let client = HetznerCloudClient::new(server.url(), "test-token");
    client.delete_floating_ip(31).expect("delete_floating_ip");
    m.assert();
}

#[test]
fn delete_floating_ip_404_is_treated_as_already_gone() {
    let mut server = mockito::Server::new();
    server
        .mock("DELETE", "/v1/floating_ips/999")
        .with_status(404)
        .with_body(r#"{"error":{"code":"not_found","message":"x"}}"#)
        .create();
    let client = HetznerCloudClient::new(server.url(), "test-token");
    client
        .delete_floating_ip(999)
        .expect("delete_floating_ip is idempotent");
}
