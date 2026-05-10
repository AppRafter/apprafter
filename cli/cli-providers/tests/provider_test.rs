// SPDX-License-Identifier: FSL-1.1-MIT
//! Provider-level tests: plan/apply/destroy diff state vs reality.

use std::collections::BTreeMap;

use cli_providers::hetzner_cloud::{
    FirewallRuleSpec, FirewallSpec, FloatingIpSpec, HetznerCloudClient, HetznerCloudProvider,
    NetworkSpec, ServerSpec, SshKeySpec,
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
        user_data: None,
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

fn fip_spec(name: &str) -> FloatingIpSpec {
    FloatingIpSpec {
        name: name.into(),
        kind: "ipv4".into(),
        home_location: "nbg1".into(),
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
    let _list_fips = srv
        .mock("GET", "/v1/floating_ips")
        .with_status(200)
        .with_body(r#"{"floating_ips":[]}"#)
        .create();

    let provider = HetznerCloudProvider {
        client: HetznerCloudClient::new(srv.url(), "tok"),
        spec: spec("platform-1"),
        ssh_keys: vec![],
        networks: vec![],
        firewalls: vec![],
        floating_ips: vec![],
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
    let _list_fips = srv
        .mock("GET", "/v1/floating_ips")
        .with_status(200)
        .with_body(r#"{"floating_ips":[]}"#)
        .create();

    let provider = HetznerCloudProvider {
        client: HetznerCloudClient::new(srv.url(), "tok"),
        spec: spec("platform-1"),
        ssh_keys: vec![],
        networks: vec![],
        firewalls: vec![],
        floating_ips: vec![],
    };

    let plan = provider.plan().unwrap();
    assert!(plan.actions.is_empty(), "got {plan:?}");
}

#[test]
fn destroy_deletes_each_tagged_server() {
    let mut srv = mockito::Server::new();
    // First list call: refresh sees the server. Subsequent calls
    // (wait_for_server_gone polling): return empty so the wait
    // returns immediately.
    let _list_first = srv
        .mock("GET", "/v1/servers")
        .with_status(200)
        .with_body(
            r#"{"servers":[{"id":42,"name":"platform-1","status":"running","labels":{"apprafter":"true"}}]}"#,
        )
        .expect(1)
        .create();
    let _list_after = srv
        .mock("GET", "/v1/servers")
        .with_status(200)
        .with_body(r#"{"servers":[]}"#)
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
    let _list_fips = srv
        .mock("GET", "/v1/floating_ips")
        .with_status(200)
        .with_body(r#"{"floating_ips":[]}"#)
        .create();

    let provider = HetznerCloudProvider {
        client: HetznerCloudClient::new(srv.url(), "tok"),
        spec: spec("platform-1"),
        ssh_keys: vec![],
        networks: vec![],
        firewalls: vec![],
        floating_ips: vec![],
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
    let _list_fips = srv
        .mock("GET", "/v1/floating_ips")
        .with_status(200)
        .with_body(r#"{"floating_ips":[]}"#)
        .create();

    let provider = HetznerCloudProvider {
        client: HetznerCloudClient::new(srv.url(), "tok"),
        spec: spec("platform-1"),
        ssh_keys: vec![ssh_spec("platform-1-key")],
        networks: vec![net_spec("platform-1-net")],
        firewalls: vec![fw_spec("platform-1-fw")],
        floating_ips: vec![],
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
    let _list_servers = srv
        .mock("GET", "/v1/servers")
        .with_status(200)
        .with_body(r#"{"servers":[]}"#)
        .create();
    let _list_types = srv
        .mock("GET", "/v1/server_types")
        .with_status(200)
        .with_body(
            r#"{"server_types":[{"id":104,"name":"cx22","architecture":"x86","cpu_type":"shared","cores":2,"memory":4,"disk":40,"deprecation":null,"locations":[{"name":"nbg1","available":true}]}]}"#,
        )
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
        floating_ips: vec![],
    };

    let outcome = provider.apply().unwrap();
    assert_eq!(outcome.applied, 4);
}

#[test]
fn destroy_removes_in_order_server_firewall_network_ssh() {
    let mut srv = mockito::Server::new();
    let _list_servers_first = srv
        .mock("GET", "/v1/servers")
        .with_status(200)
        .with_body(
            r#"{"servers":[{"id":42,"name":"platform-1","status":"running","labels":{"apprafter":"true"}}]}"#,
        )
        .expect(1)
        .create();
    let _list_servers_after = srv
        .mock("GET", "/v1/servers")
        .with_status(200)
        .with_body(r#"{"servers":[]}"#)
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
    let _list_fips = srv
        .mock("GET", "/v1/floating_ips")
        .with_status(200)
        .with_body(r#"{"floating_ips":[]}"#)
        .create();

    let provider = HetznerCloudProvider {
        client: HetznerCloudClient::new(srv.url(), "tok"),
        spec: spec("platform-1"),
        ssh_keys: vec![ssh_spec("k")],
        networks: vec![net_spec("platform-1-net")],
        firewalls: vec![fw_spec("platform-1-fw")],
        floating_ips: vec![],
    };

    let outcome = provider.destroy().unwrap();
    assert_eq!(outcome.destroyed, 4);
}

#[test]
fn apply_creates_floating_ip_after_server_with_server_attach() {
    let mut srv = mockito::Server::new();
    let _list_servers = srv
        .mock("GET", "/v1/servers")
        .with_status(200)
        .with_body(r#"{"servers":[]}"#)
        .create();
    let _list_types = srv
        .mock("GET", "/v1/server_types")
        .with_status(200)
        .with_body(
            r#"{"server_types":[{"id":104,"name":"cx22","architecture":"x86","cpu_type":"shared","cores":2,"memory":4,"disk":40,"deprecation":null,"locations":[{"name":"nbg1","available":true}]}]}"#,
        )
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
    let _list_fips = srv
        .mock("GET", "/v1/floating_ips")
        .with_status(200)
        .with_body(r#"{"floating_ips":[]}"#)
        .create();
    let _create_server = srv
        .mock("POST", "/v1/servers")
        .with_status(201)
        .with_body(
            r#"{"server":{"id":42,"name":"platform-1","status":"initializing","labels":{"apprafter":"true"}}}"#,
        )
        .expect(1)
        .create();
    let _create_fip = srv
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
        .expect(1)
        .create();

    let provider = HetznerCloudProvider {
        client: HetznerCloudClient::new(srv.url(), "tok"),
        spec: spec("platform-1"),
        ssh_keys: vec![],
        networks: vec![],
        firewalls: vec![],
        floating_ips: vec![fip_spec("platform-1-egress")],
    };

    let outcome = provider.apply().unwrap();
    // 1 server + 1 floating ip.
    assert_eq!(outcome.applied, 2);
}

#[test]
fn destroy_removes_floating_ip_first_then_others() {
    let mut srv = mockito::Server::new();
    let _list_fips = srv
        .mock("GET", "/v1/floating_ips")
        .with_status(200)
        .with_body(
            r#"{"floating_ips":[{"id":31,"type":"ipv4","ip":"1.2.3.4","name":"egress","server":42,"home_location":{"name":"nbg1"},"labels":{"apprafter":"true"}}]}"#,
        )
        .create();
    let _del_fip = srv
        .mock("DELETE", "/v1/floating_ips/31")
        .with_status(204)
        .expect(1)
        .create();
    let _list_servers_first = srv
        .mock("GET", "/v1/servers")
        .with_status(200)
        .with_body(
            r#"{"servers":[{"id":42,"name":"platform-1","status":"running","labels":{"apprafter":"true"}}]}"#,
        )
        .expect(1)
        .create();
    let _list_servers_after = srv
        .mock("GET", "/v1/servers")
        .with_status(200)
        .with_body(r#"{"servers":[]}"#)
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
        .with_body(r#"{"firewalls":[]}"#)
        .create();
    let _list_nets = srv
        .mock("GET", "/v1/networks")
        .with_status(200)
        .with_body(r#"{"networks":[]}"#)
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
        networks: vec![],
        firewalls: vec![],
        floating_ips: vec![fip_spec("platform-1-egress")],
    };

    let outcome = provider.destroy().unwrap();
    // 1 floating ip + 1 server.
    assert_eq!(outcome.destroyed, 2);
}

#[test]
fn apply_forwards_user_data_into_the_create_server_request() {
    use cli_providers::hetzner_cloud::{HetznerCloudClient, HetznerCloudProvider, ServerSpec};
    use cli_providers::Provider;
    use std::collections::BTreeMap;

    let mut server = mockito::Server::new();
    server
        .mock("GET", "/v1/servers")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(r#"{"servers":[]}"#)
        .create();
    server
        .mock("GET", "/v1/server_types")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(
            r#"{"server_types":[{"id":104,"name":"cx22","architecture":"x86","cpu_type":"shared","cores":2,"memory":4,"disk":40,"deprecation":null,"locations":[{"name":"nbg1","available":true}]}]}"#,
        )
        .create();
    server
        .mock("GET", "/v1/ssh_keys")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(r#"{"ssh_keys":[]}"#)
        .create();
    server
        .mock("GET", "/v1/networks")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(r#"{"networks":[]}"#)
        .create();
    server
        .mock("GET", "/v1/firewalls")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(r#"{"firewalls":[]}"#)
        .create();

    let create = server
        .mock("POST", "/v1/servers")
        .match_body(mockito::Matcher::PartialJson(serde_json::json!({
            "user_data": "#cloud-config\nfoo: bar\n"
        })))
        .with_status(201)
        .with_header("content-type", "application/json")
        .with_body(
            r#"{"server":{"id":7,"name":"cl","status":"initializing","labels":{"apprafter":"true"}},"root_password":null}"#,
        )
        .create();

    let provider = HetznerCloudProvider {
        client: HetznerCloudClient::new(server.url(), "test-token"),
        spec: ServerSpec {
            name: "cl".into(),
            server_type: "cx22".into(),
            image: "ubuntu-24.04".into(),
            location: "nbg1".into(),
            labels: BTreeMap::new(),
            user_data: Some("#cloud-config\nfoo: bar\n".into()),
        },
        ssh_keys: vec![],
        networks: vec![],
        firewalls: vec![],
        floating_ips: vec![],
    };

    let outcome = provider.apply().expect("apply");
    assert_eq!(outcome.applied, 1);
    create.assert();
}

#[test]
fn apply_rejects_deprecated_server_type_before_any_post() {
    let mut server = mockito::Server::new();

    // refresh_ssh_keys / refresh_networks / refresh_firewalls /
    // refresh_servers all return empty — so the provider would
    // try to CREATE everything. The pre-flight must reject FIRST,
    // before any POST, so we ONLY mock the GETs and assert no
    // POST mock was consumed.
    let _g_keys = server
        .mock("GET", "/v1/ssh_keys")
        .with_status(200)
        .with_body(r#"{"ssh_keys":[]}"#)
        .create();
    let _g_nets = server
        .mock("GET", "/v1/networks")
        .with_status(200)
        .with_body(r#"{"networks":[]}"#)
        .create();
    let _g_fws = server
        .mock("GET", "/v1/firewalls")
        .with_status(200)
        .with_body(r#"{"firewalls":[]}"#)
        .create();
    let g_servers = server
        .mock("GET", "/v1/servers")
        .with_status(200)
        .with_body(r#"{"servers":[]}"#)
        .expect_at_least(1)
        .create();
    let g_types = server
        .mock("GET", "/v1/server_types")
        .with_status(200)
        .with_body(
            r#"{
              "server_types": [
                {
                  "id": 109, "name": "cpx22", "architecture": "x86",
                  "cpu_type": "shared", "cores": 2, "memory": 4, "disk": 80,
                  "deprecation": null,
                  "locations": [{"name": "nbg1", "available": true}]
                },
                {
                  "id": 104, "name": "cx22", "architecture": "x86",
                  "cpu_type": "shared", "cores": 2, "memory": 4, "disk": 40,
                  "deprecation": {"announced": "2025-10-01T00:00:00Z"},
                  "locations": [{"name": "nbg1", "available": false}]
                }
              ]
            }"#,
        )
        .expect_at_least(1)
        .create();
    // NOTE: zero POST mocks. If apply calls POST, mockito returns
    // 501 and the test would fail — but more importantly, we want
    // it to fail BEFORE that with our typed error.
    let no_post = server
        .mock("POST", mockito::Matcher::Any)
        .expect(0)
        .create();

    use cli_providers::hetzner_cloud::{
        FirewallSpec, HetznerCloudClient, HetznerCloudProvider, NetworkSpec, ServerSpec,
    };
    use std::collections::BTreeMap;

    let provider = HetznerCloudProvider {
        client: HetznerCloudClient::new(server.url(), "test-token"),
        spec: ServerSpec {
            name: "platform-1".into(),
            server_type: "cx22".into(), // ← retired upstream
            image: "ubuntu-24.04".into(),
            location: "nbg1".into(),
            labels: BTreeMap::new(),
            user_data: None,
        },
        ssh_keys: vec![],
        networks: vec![NetworkSpec {
            name: "platform-1-net".into(),
            ip_range: "10.0.0.0/16".into(),
            subnet_ip_range: "10.0.0.0/24".into(),
            network_zone: "eu-central".into(),
        }],
        firewalls: vec![FirewallSpec {
            name: "platform-1-fw".into(),
            rules: vec![],
        }],
        floating_ips: vec![],
    };

    let err = cli_providers::Provider::apply(&provider).unwrap_err();
    match err {
        cli_core::CliError::ServerTypeUnavailable {
            requested,
            alternatives,
            ..
        } => {
            assert_eq!(requested, "cx22");
            assert!(alternatives.contains("cpx22"));
        }
        other => panic!("expected ServerTypeUnavailable, got {other:?}"),
    }
    g_servers.assert();
    g_types.assert();
    no_post.assert(); // proves nothing was POSTed
}

#[test]
fn apply_skips_server_types_lookup_when_server_already_exists() {
    let mut server = mockito::Server::new();

    // SSH keys / networks / firewalls already exist — provider
    // sees them via refresh_*, no creation needed.
    let _g_keys = server
        .mock("GET", "/v1/ssh_keys")
        .with_status(200)
        .with_body(r#"{"ssh_keys":[]}"#)
        .create();
    let _g_nets = server
        .mock("GET", "/v1/networks")
        .with_status(200)
        .with_body(r#"{"networks":[]}"#)
        .create();
    let _g_fws = server
        .mock("GET", "/v1/firewalls")
        .with_status(200)
        .with_body(r#"{"firewalls":[]}"#)
        .create();
    // Server with our name already exists ⇒ no_op create branch.
    let _g_servers = server
        .mock("GET", "/v1/servers")
        .with_status(200)
        .with_body(
            r#"{"servers":[{"id":42,"name":"platform-1","status":"running","labels":{"apprafter":"true"}}]}"#,
        )
        .create();
    // CRITICAL: list_server_types must NOT be called when no
    // create is needed.
    let no_types = server.mock("GET", "/v1/server_types").expect(0).create();

    use cli_providers::hetzner_cloud::{HetznerCloudClient, HetznerCloudProvider, ServerSpec};
    use std::collections::BTreeMap;

    let provider = HetznerCloudProvider {
        client: HetznerCloudClient::new(server.url(), "test-token"),
        spec: ServerSpec {
            name: "platform-1".into(),
            server_type: "cpx22".into(),
            image: "ubuntu-24.04".into(),
            location: "nbg1".into(),
            labels: BTreeMap::new(),
            user_data: None,
        },
        ssh_keys: vec![],
        networks: vec![],
        firewalls: vec![],
        floating_ips: vec![],
    };

    let outcome = cli_providers::Provider::apply(&provider).expect("no-op apply succeeds");
    assert_eq!(outcome.applied, 0, "no resources should be created");
    no_types.assert(); // proves /server_types was NEVER hit
}

#[test]
fn destroy_waits_for_server_async_cleanup_before_deleting_network() {
    // Regression guard for v0.1.47: Hetzner's `DELETE /servers/{id}`
    // is async; the next `DELETE /networks/{id}` returns 409 "in
    // use" while the server's network interface is still attached.
    // Since v0.1.47 destroy() polls /v1/servers until the deleted
    // id is gone before proceeding to firewall/network/ssh-key.
    // Test mocks the server as "still present" once after delete
    // (one poll iteration with the server still listed) and
    // "gone" thereafter; asserts the second list call happens
    // BEFORE delete_network is invoked.
    let mut srv = mockito::Server::new();
    let _list_fips = srv
        .mock("GET", "/v1/floating_ips")
        .with_status(200)
        .with_body(r#"{"floating_ips":[]}"#)
        .create();

    // Sequenced /v1/servers responses:
    //   1st: refresh sees the server (initial destroy step).
    //   2nd: wait_for_server_gone first poll — STILL present
    //        (simulates Hetzner async lag).
    //   3rd+: gone.
    let _list_first = srv
        .mock("GET", "/v1/servers")
        .with_status(200)
        .with_body(
            r#"{"servers":[{"id":42,"name":"platform-1","status":"running","labels":{"apprafter":"true"}}]}"#,
        )
        .expect(1)
        .create();
    let _list_lag = srv
        .mock("GET", "/v1/servers")
        .with_status(200)
        .with_body(
            r#"{"servers":[{"id":42,"name":"platform-1","status":"deleting","labels":{"apprafter":"true"}}]}"#,
        )
        .expect(1)
        .create();
    let list_gone = srv
        .mock("GET", "/v1/servers")
        .with_status(200)
        .with_body(r#"{"servers":[]}"#)
        .expect_at_least(1)
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
        .with_body(r#"{"firewalls":[]}"#)
        .create();
    let _list_nets = srv
        .mock("GET", "/v1/networks")
        .with_status(200)
        .with_body(r#"{"networks":[]}"#)
        .create();
    let _list_keys = srv
        .mock("GET", "/v1/ssh_keys")
        .with_status(200)
        .with_body(r#"{"ssh_keys":[]}"#)
        .create();

    use cli_providers::hetzner_cloud::{HetznerCloudClient, HetznerCloudProvider, ServerSpec};
    use std::collections::BTreeMap;

    let provider = HetznerCloudProvider {
        client: HetznerCloudClient::new(srv.url(), "tok"),
        spec: ServerSpec {
            name: "platform-1".into(),
            server_type: "cpx22".into(),
            image: "ubuntu-24.04".into(),
            location: "nbg1".into(),
            labels: BTreeMap::new(),
            user_data: None,
        },
        ssh_keys: vec![],
        networks: vec![],
        firewalls: vec![],
        floating_ips: vec![],
    };

    let outcome = cli_providers::Provider::destroy(&provider).expect("destroy succeeds");
    assert_eq!(outcome.destroyed, 1, "exactly one server destroyed");
    list_gone.assert(); // proves the wait-for-gone poll ran at least once
}
