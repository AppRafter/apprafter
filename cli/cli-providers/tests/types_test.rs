// SPDX-License-Identifier: FSL-1.1-MIT
//! Round-trip tests for the Hetzner Cloud wire types.

use cli_providers::hetzner_cloud::types::{
    Server, ServerCreateRequest, ServerListResponse, ServerStatus,
};

#[test]
fn deserialize_server_status() {
    assert_eq!(
        serde_json::from_str::<ServerStatus>("\"running\"").unwrap(),
        ServerStatus::Running
    );
    assert_eq!(
        serde_json::from_str::<ServerStatus>("\"off\"").unwrap(),
        ServerStatus::Off
    );
    assert_eq!(
        serde_json::from_str::<ServerStatus>("\"initializing\"").unwrap(),
        ServerStatus::Initializing
    );
}

#[test]
fn deserialize_server_list_response_filters_by_label() {
    let json = r#"{
        "servers": [
            {
                "id": 42,
                "name": "platform-1",
                "status": "running",
                "labels": { "apprafter": "true", "tier": "solo" }
            },
            {
                "id": 43,
                "name": "other",
                "status": "running",
                "labels": {}
            }
        ]
    }"#;

    let parsed: ServerListResponse = serde_json::from_str(json).unwrap();
    assert_eq!(parsed.servers.len(), 2);
    let ours: Vec<&Server> = parsed
        .servers
        .iter()
        .filter(|s| s.labels.get("apprafter").map(String::as_str) == Some("true"))
        .collect();
    assert_eq!(ours.len(), 1);
    assert_eq!(ours[0].id, 42);
    assert_eq!(ours[0].name, "platform-1");
}

#[test]
fn server_create_request_serialises_required_fields() {
    let req = ServerCreateRequest {
        name: "platform-1".into(),
        server_type: "cx22".into(),
        image: "ubuntu-24.04".into(),
        location: "nbg1".into(),
        labels: [("apprafter".to_string(), "true".to_string())]
            .into_iter()
            .collect(),
        start_after_create: true,
        ssh_keys: None,
        networks: None,
        firewalls: None,
    };

    let json = serde_json::to_value(&req).unwrap();
    assert_eq!(json["name"], "platform-1");
    assert_eq!(json["server_type"], "cx22");
    assert_eq!(json["image"], "ubuntu-24.04");
    assert_eq!(json["location"], "nbg1");
    assert_eq!(json["labels"]["apprafter"], "true");
    assert_eq!(json["start_after_create"], true);
}

#[test]
fn deserialize_ssh_key_list_response() {
    let json = r#"{
      "ssh_keys": [
        {
          "id": 7,
          "name": "platform-1-key",
          "public_key": "ssh-ed25519 AAAA",
          "fingerprint": "aa:bb:cc",
          "labels": {"apprafter": "true"}
        }
      ]
    }"#;
    use cli_providers::hetzner_cloud::types::SshKeyListResponse;
    let parsed: SshKeyListResponse = serde_json::from_str(json).unwrap();
    assert_eq!(parsed.ssh_keys.len(), 1);
    assert_eq!(parsed.ssh_keys[0].id, 7);
    assert_eq!(parsed.ssh_keys[0].name, "platform-1-key");
}

#[test]
fn ssh_key_create_request_serialises_required_fields() {
    use cli_providers::hetzner_cloud::types::SshKeyCreateRequest;
    let req = SshKeyCreateRequest {
        name: "platform-1-key".into(),
        public_key: "ssh-ed25519 AAAA".into(),
        labels: [("apprafter".to_string(), "true".to_string())]
            .into_iter()
            .collect(),
    };
    let json = serde_json::to_value(&req).unwrap();
    assert_eq!(json["name"], "platform-1-key");
    assert_eq!(json["public_key"], "ssh-ed25519 AAAA");
    assert_eq!(json["labels"]["apprafter"], "true");
}

#[test]
fn server_create_request_ssh_keys_optional_serialisation() {
    let mut req = ServerCreateRequest {
        name: "platform-1".into(),
        server_type: "cx22".into(),
        image: "ubuntu-24.04".into(),
        location: "nbg1".into(),
        labels: Default::default(),
        start_after_create: true,
        ssh_keys: None,
        networks: None,
        firewalls: None,
    };
    let json_none = serde_json::to_value(&req).unwrap();
    assert!(
        json_none.get("ssh_keys").is_none(),
        "ssh_keys should be omitted when None"
    );

    req.ssh_keys = Some(vec![7, 9]);
    let json_some = serde_json::to_value(&req).unwrap();
    assert_eq!(json_some["ssh_keys"], serde_json::json!([7, 9]));
}

#[test]
fn deserialize_network_list_response() {
    let json = r#"{
      "networks": [
        {
          "id": 11,
          "name": "platform-1-net",
          "ip_range": "10.0.0.0/16",
          "subnets": [
            { "type": "cloud", "ip_range": "10.0.0.0/24", "network_zone": "eu-central" }
          ],
          "labels": {"apprafter": "true"}
        }
      ]
    }"#;
    use cli_providers::hetzner_cloud::types::NetworkListResponse;
    let parsed: NetworkListResponse = serde_json::from_str(json).unwrap();
    assert_eq!(parsed.networks.len(), 1);
    assert_eq!(parsed.networks[0].id, 11);
    assert_eq!(parsed.networks[0].subnets.len(), 1);
    assert_eq!(parsed.networks[0].subnets[0].network_zone, "eu-central");
}

#[test]
fn network_create_request_serialises_with_subnet() {
    use cli_providers::hetzner_cloud::types::{NetworkCreateRequest, Subnet};
    let req = NetworkCreateRequest {
        name: "platform-1-net".into(),
        ip_range: "10.0.0.0/16".into(),
        subnets: vec![Subnet {
            kind: "cloud".into(),
            ip_range: "10.0.0.0/24".into(),
            network_zone: "eu-central".into(),
        }],
        labels: [("apprafter".to_string(), "true".to_string())]
            .into_iter()
            .collect(),
    };
    let json = serde_json::to_value(&req).unwrap();
    assert_eq!(json["name"], "platform-1-net");
    assert_eq!(json["ip_range"], "10.0.0.0/16");
    assert_eq!(json["subnets"][0]["type"], "cloud");
    assert_eq!(json["subnets"][0]["ip_range"], "10.0.0.0/24");
    assert_eq!(json["subnets"][0]["network_zone"], "eu-central");
}

#[test]
fn deserialize_firewall_list_response() {
    let json = r#"{
      "firewalls": [
        {
          "id": 21,
          "name": "platform-1-fw",
          "rules": [
            {"direction":"in","port":"22","protocol":"tcp","source_ips":["0.0.0.0/0","::/0"],"destination_ips":[]}
          ],
          "labels": {"apprafter": "true"}
        }
      ]
    }"#;
    use cli_providers::hetzner_cloud::types::FirewallListResponse;
    let parsed: FirewallListResponse = serde_json::from_str(json).unwrap();
    assert_eq!(parsed.firewalls.len(), 1);
    assert_eq!(parsed.firewalls[0].id, 21);
    assert_eq!(parsed.firewalls[0].rules[0].direction, "in");
    assert_eq!(parsed.firewalls[0].rules[0].port.as_deref(), Some("22"));
}

#[test]
fn server_create_request_networks_and_firewalls_serialise_optionally() {
    let mut req = ServerCreateRequest {
        name: "platform-1".into(),
        server_type: "cx22".into(),
        image: "ubuntu-24.04".into(),
        location: "nbg1".into(),
        labels: Default::default(),
        start_after_create: true,
        ssh_keys: None,
        networks: None,
        firewalls: None,
    };
    let json_none = serde_json::to_value(&req).unwrap();
    assert!(json_none.get("networks").is_none());
    assert!(json_none.get("firewalls").is_none());

    use cli_providers::hetzner_cloud::types::FirewallReference;
    req.networks = Some(vec![11]);
    req.firewalls = Some(vec![FirewallReference { firewall: 21 }]);
    let json_some = serde_json::to_value(&req).unwrap();
    assert_eq!(json_some["networks"], serde_json::json!([11]));
    assert_eq!(json_some["firewalls"], serde_json::json!([{"firewall":21}]));
}
