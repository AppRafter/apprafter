// SPDX-License-Identifier: FSL-1.1-MIT
//! Provider-level tests: plan/apply/destroy diff state vs reality.

use std::collections::BTreeMap;

use cli_providers::hetzner_cloud::{
    FirewallRuleSpec, FirewallSpec, HetznerCloudClient, HetznerCloudProvider, NetworkSpec,
    ServerSpec, SshKeySpec,
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

fn net_spec(name: &str) -> NetworkSpec {
    NetworkSpec {
        name: name.into(),
        ip_range: "10.0.0.0/16".into(),
        subnet_ip_range: "10.0.0.0/24".into(),
        network_zone: "eu-central".into(),
    }
}

fn fw_spec(name: &str) -> FirewallSpec {
    FirewallSpec {
        name: name.into(),
        rules: vec![FirewallRuleSpec {
            direction: "in".into(),
            port: Some("22".into()),
            protocol: "tcp".into(),
            source_ips: vec!["0.0.0.0/0".into(), "::/0".into()],
            destination_ips: vec![],
        }],
    }
}

#[test]
fn plan_creates_when_server_missing() {
    let mut srv = mockito::Server::new();
    let _ssh = srv
        .mock("GET", "/v1/ssh_keys")
        .with_status(200)
        .with_body(r#"{"ssh_keys":[]}"#)
        .create();
    let _net = srv
        .mock("GET", "/v1/networks")
        .with_status(200)
        .with_body(r#"{"networks":[]}"#)
        .create();
    let _fw = srv
        .mock("GET", "/v1/firewalls")
        .with_status(200)
        .with_body(r#"{"firewalls":[]}"#)
        .create();
    let _list = srv
        .mock("GET", "/v1/servers")
        .with_status(200)
        .with_body(r#"{"servers":[]}"#)
        .create();

    let provider = HetznerCloudProvider {
        client: HetznerCloudClient::new(srv.url(), "tok"),
        spec: spec("platform-1"),
        ssh_keys: vec![],
        networks: vec![],
        firewalls: vec![],
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
    let _ssh = srv
        .mock("GET", "/v1/ssh_keys")
        .with_status(200)
        .with_body(r#"{"ssh_keys":[]}"#)
        .create();
    let _net = srv
        .mock("GET", "/v1/networks")
        .with_status(200)
        .with_body(r#"{"networks":[]}"#)
        .create();
    let _fw = srv
        .mock("GET", "/v1/firewalls")
        .with_status(200)
        .with_body(r#"{"firewalls":[]}"#)
        .create();
    let _list = srv
        .mock("GET", "/v1/servers")
        .with_status(200)
        .with_body(
            r#"{"servers":[{"id":42,"name":"platform-1","status":"running","labels":{"apprafter":"true"}}]}"#,
        )
        .create();

    let provider = HetznerCloudProvider {
        client: HetznerCloudClient::new(srv.url(), "tok"),
        spec: spec("platform-1"),
        ssh_keys: vec![],
        networks: vec![],
        firewalls: vec![],
    };

    let plan = provider.plan().unwrap();
    assert!(plan.actions.is_empty(), "got {plan:?}");
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
    let _list_nets = srv
        .mock("GET", "/v1/networks")
        .with_status(200)
        .with_body(r#"{"networks":[]}"#)
        .create();
    let _list_fws = srv
        .mock("GET", "/v1/firewalls")
        .with_status(200)
        .with_body(r#"{"firewalls":[]}"#)
        .create();

    let provider = HetznerCloudProvider {
        client: HetznerCloudClient::new(srv.url(), "tok"),
        spec: spec("platform-1"),
        ssh_keys: vec![],
        networks: vec![],
        firewalls: vec![],
    };

    let outcome = provider.destroy().unwrap();
    assert_eq!(outcome.destroyed, 1);
}

#[test]
fn plan_creates_network_firewall_and_server_when_all_missing() {
    let mut srv = mockito::Server::new();
    let _ssh = srv
        .mock("GET", "/v1/ssh_keys")
        .with_status(200)
        .with_body(r#"{"ssh_keys":[]}"#)
        .create();
    let _net = srv
        .mock("GET", "/v1/networks")
        .with_status(200)
        .with_body(r#"{"networks":[]}"#)
        .create();
    let _fw = srv
        .mock("GET", "/v1/firewalls")
        .with_status(200)
        .with_body(r#"{"firewalls":[]}"#)
        .create();
    let _list = srv
        .mock("GET", "/v1/servers")
        .with_status(200)
        .with_body(r#"{"servers":[]}"#)
        .create();

    let provider = HetznerCloudProvider {
        client: HetznerCloudClient::new(srv.url(), "tok"),
        spec: spec("platform-1"),
        ssh_keys: vec![ssh_spec("platform-1-key")],
        networks: vec![net_spec("platform-1-net")],
        firewalls: vec![fw_spec("platform-1-fw")],
    };

    let plan = provider.plan().unwrap();
    assert_eq!(
        plan.actions,
        vec![
            Action::CreateSshKey("platform-1-key".into()),
            Action::CreateNetwork("platform-1-net".into()),
            Action::CreateFirewall("platform-1-fw".into()),
            Action::CreateServer("platform-1".into()),
        ]
    );
}

#[test]
fn apply_creates_all_resources_in_order_and_attaches_to_server() {
    let mut srv = mockito::Server::new();
    let _list_keys = srv
        .mock("GET", "/v1/ssh_keys")
        .with_status(200)
        .with_body(r#"{"ssh_keys":[]}"#)
        .create();
    let _list_nets = srv
        .mock("GET", "/v1/networks")
        .with_status(200)
        .with_body(r#"{"networks":[]}"#)
        .create();
    let _list_fws = srv
        .mock("GET", "/v1/firewalls")
        .with_status(200)
        .with_body(r#"{"firewalls":[]}"#)
        .create();
    let _list_servers = srv
        .mock("GET", "/v1/servers")
        .with_status(200)
        .with_body(r#"{"servers":[]}"#)
        .create();
    let _create_key = srv
        .mock("POST", "/v1/ssh_keys")
        .with_status(201)
        .with_body(
            r#"{"ssh_key":{"id":7,"name":"k","public_key":"x","fingerprint":"f","labels":{"apprafter":"true"}}}"#,
        )
        .expect(1)
        .create();
    let _create_net = srv
        .mock("POST", "/v1/networks")
        .with_status(201)
        .with_body(
            r#"{"network":{"id":11,"name":"platform-1-net","ip_range":"10.0.0.0/16","subnets":[],"labels":{"apprafter":"true"}}}"#,
        )
        .expect(1)
        .create();
    let _create_fw = srv
        .mock("POST", "/v1/firewalls")
        .with_status(201)
        .with_body(
            r#"{"firewall":{"id":21,"name":"platform-1-fw","rules":[],"labels":{"apprafter":"true"}}}"#,
        )
        .expect(1)
        .create();
    let _create_server = srv
        .mock("POST", "/v1/servers")
        .match_body(mockito::Matcher::PartialJson(serde_json::json!({
            "name": "platform-1",
            "ssh_keys": [7],
            "networks": [11],
            "firewalls": [{"firewall": 21}]
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
        networks: vec![net_spec("platform-1-net")],
        firewalls: vec![fw_spec("platform-1-fw")],
    };

    let outcome = provider.apply().unwrap();
    assert_eq!(outcome.applied, 4);
}

#[test]
fn destroy_removes_in_order_server_firewall_network_ssh() {
    let mut srv = mockito::Server::new();
    let _list_servers = srv
        .mock("GET", "/v1/servers")
        .with_status(200)
        .with_body(
            r#"{"servers":[{"id":42,"name":"platform-1","status":"running","labels":{"apprafter":"true"}}]}"#,
        )
        .create();
    let _del_server = srv
        .mock("DELETE", "/v1/servers/42")
        .with_status(200)
        .with_body(r#"{"action":{"id":1,"status":"success"}}"#)
        .expect(1)
        .create();
    let _list_fws = srv
        .mock("GET", "/v1/firewalls")
        .with_status(200)
        .with_body(
            r#"{"firewalls":[{"id":21,"name":"platform-1-fw","rules":[],"labels":{"apprafter":"true"}}]}"#,
        )
        .create();
    let _del_fw = srv
        .mock("DELETE", "/v1/firewalls/21")
        .with_status(204)
        .expect(1)
        .create();
    let _list_nets = srv
        .mock("GET", "/v1/networks")
        .with_status(200)
        .with_body(
            r#"{"networks":[{"id":11,"name":"platform-1-net","ip_range":"10.0.0.0/16","subnets":[],"labels":{"apprafter":"true"}}]}"#,
        )
        .create();
    let _del_net = srv
        .mock("DELETE", "/v1/networks/11")
        .with_status(204)
        .expect(1)
        .create();
    let _list_keys = srv
        .mock("GET", "/v1/ssh_keys")
        .with_status(200)
        .with_body(
            r#"{"ssh_keys":[{"id":7,"name":"k","public_key":"x","fingerprint":"f","labels":{"apprafter":"true"}}]}"#,
        )
        .create();
    let _del_key = srv
        .mock("DELETE", "/v1/ssh_keys/7")
        .with_status(204)
        .expect(1)
        .create();

    let provider = HetznerCloudProvider {
        client: HetznerCloudClient::new(srv.url(), "tok"),
        spec: spec("platform-1"),
        ssh_keys: vec![ssh_spec("k")],
        networks: vec![net_spec("platform-1-net")],
        firewalls: vec![fw_spec("platform-1-fw")],
    };

    let outcome = provider.destroy().unwrap();
    assert_eq!(outcome.destroyed, 4);
}
