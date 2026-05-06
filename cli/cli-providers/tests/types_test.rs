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
    };

    let json = serde_json::to_value(&req).unwrap();
    assert_eq!(json["name"], "platform-1");
    assert_eq!(json["server_type"], "cx22");
    assert_eq!(json["image"], "ubuntu-24.04");
    assert_eq!(json["location"], "nbg1");
    assert_eq!(json["labels"]["apprafter"], "true");
    assert_eq!(json["start_after_create"], true);
}
