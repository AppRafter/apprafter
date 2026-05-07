// SPDX-License-Identifier: FSL-1.1-MIT
//! Integration tests for `platform-cli import` against a mockito
//! Hetzner Cloud server.

use assert_cmd::Command;
use predicates::str::contains;

fn cli() -> Command {
    Command::cargo_bin("platform-cli").unwrap()
}

/// Build a workspace dir with `state.json` already containing
/// `provider=hetzner-cloud`, `cluster_name=<cluster>`. Returns the
/// `tempfile::TempDir` (dropping it removes the dir).
fn workspace_with_state(cluster: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    cli()
        .current_dir(dir.path())
        .args([
            "init",
            "--provider",
            "hetzner-cloud",
            "--tier",
            "solo",
            "--region",
            "nbg1",
        ])
        .assert()
        .success();
    if cluster != "platform-1" {
        let path = dir.path().join(".apprafter/state.json");
        let raw = std::fs::read_to_string(&path).unwrap();
        let mut state: serde_json::Value = serde_json::from_str(&raw).unwrap();
        state["cluster_name"] = serde_json::Value::String(cluster.into());
        std::fs::write(path, serde_json::to_vec_pretty(&state).unwrap()).unwrap();
    }
    dir
}

/// Stand up mocks for every list endpoint `import` queries.
fn mock_all_lists(server: &mut mockito::ServerGuard, cluster: &str, fip_count: usize) {
    server
        .mock("GET", "/v1/servers")
        .match_header("authorization", "Bearer test-token")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(format!(
            r#"{{"servers":[{{"id":42,"name":"{cluster}","status":"running",
                "labels":{{"apprafter":"true"}}}}]}}"#
        ))
        .create();
    server
        .mock("GET", "/v1/ssh_keys")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(
            r#"{"ssh_keys":[
                {"id":7,"name":"k1","public_key":"ssh-ed25519 AAA",
                 "fingerprint":"aa:aa","labels":{"apprafter":"true"}},
                {"id":8,"name":"k2","public_key":"ssh-ed25519 BBB",
                 "fingerprint":"bb:bb","labels":{"apprafter":"true"}}
            ]}"#,
        )
        .create();
    server
        .mock("GET", "/v1/networks")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(
            r#"{"networks":[
                {"id":100,"name":"net","ip_range":"10.0.0.0/16",
                 "subnets":[],"labels":{"apprafter":"true"}}
            ]}"#,
        )
        .create();
    server
        .mock("GET", "/v1/firewalls")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(
            r#"{"firewalls":[
                {"id":200,"name":"fw","rules":[],"labels":{"apprafter":"true"}}
            ]}"#,
        )
        .create();
    let fips_json: String = (0..fip_count)
        .map(|i| {
            format!(
                r#"{{"id":{},"type":"ipv4","ip":"1.2.3.{i}","name":"fip-{i}",
                "home_location":{{"name":"nbg1"}},"labels":{{"apprafter":"true"}}}}"#,
                300 + i
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    server
        .mock("GET", "/v1/floating_ips")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(format!(r#"{{"floating_ips":[{fips_json}]}}"#))
        .create();
}

#[test]
fn import_writes_state_for_matching_cluster() {
    let mut server = mockito::Server::new();
    mock_all_lists(&mut server, "platform-1", 1);
    let dir = workspace_with_state("platform-1");

    cli()
        .current_dir(dir.path())
        .env("HCLOUD_TOKEN", "test-token")
        .env("APPRAFTER_HCLOUD_BASE_URL", server.url())
        .arg("import")
        .assert()
        .success()
        .stdout(contains("server: platform-1 (id 42)"))
        .stdout(contains("ssh keys: 2"))
        .stdout(contains("network: 100"))
        .stdout(contains("firewall: 200"))
        .stdout(contains("floating IPs: 1"));

    let raw = std::fs::read_to_string(dir.path().join(".apprafter/state.json")).unwrap();
    let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(v["hetzner_cloud"]["server_id"], 42);
    assert_eq!(v["hetzner_cloud"]["server_name"], "platform-1");
    assert_eq!(
        v["hetzner_cloud"]["ssh_key_ids"].as_array().unwrap().len(),
        2
    );
    assert_eq!(v["hetzner_cloud"]["network_id"], 100);
    assert_eq!(v["hetzner_cloud"]["firewall_id"], 200);
    assert_eq!(
        v["hetzner_cloud"]["floating_ip_ids"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn import_dry_run_prints_summary_but_does_not_write_state() {
    let mut server = mockito::Server::new();
    mock_all_lists(&mut server, "platform-1", 0);
    let dir = workspace_with_state("platform-1");

    cli()
        .current_dir(dir.path())
        .env("HCLOUD_TOKEN", "test-token")
        .env("APPRAFTER_HCLOUD_BASE_URL", server.url())
        .args(["import", "--dry-run"])
        .assert()
        .success()
        .stdout(contains("would write"))
        .stdout(contains("server: platform-1 (id 42)"));

    let raw = std::fs::read_to_string(dir.path().join(".apprafter/state.json")).unwrap();
    let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert!(v["hetzner_cloud"].is_null());
}

#[test]
fn import_with_no_matching_server_says_so_and_writes_nothing() {
    let mut server = mockito::Server::new();
    server
        .mock("GET", "/v1/servers")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(r#"{"servers":[]}"#)
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
    server
        .mock("GET", "/v1/floating_ips")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(r#"{"floating_ips":[]}"#)
        .create();
    let dir = workspace_with_state("platform-1");

    cli()
        .current_dir(dir.path())
        .env("HCLOUD_TOKEN", "test-token")
        .env("APPRAFTER_HCLOUD_BASE_URL", server.url())
        .arg("import")
        .assert()
        .success()
        .stdout(contains("no matching"))
        .stdout(contains("platform-1"));

    let raw = std::fs::read_to_string(dir.path().join(".apprafter/state.json")).unwrap();
    let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert!(v["hetzner_cloud"].is_null());
}

#[test]
fn import_refuses_to_overwrite_existing_state_without_force() {
    let mut server = mockito::Server::new();
    mock_all_lists(&mut server, "platform-1", 0);
    let dir = workspace_with_state("platform-1");

    let path = dir.path().join(".apprafter/state.json");
    let raw = std::fs::read_to_string(&path).unwrap();
    let mut state: serde_json::Value = serde_json::from_str(&raw).unwrap();
    state["hetzner_cloud"] = serde_json::json!({
        "server_id": 1,
        "server_name": "stale",
        "ssh_key_ids": [],
        "network_id": null,
        "firewall_id": null,
        "floating_ip_ids": []
    });
    std::fs::write(&path, serde_json::to_vec_pretty(&state).unwrap()).unwrap();

    cli()
        .current_dir(dir.path())
        .env("HCLOUD_TOKEN", "test-token")
        .env("APPRAFTER_HCLOUD_BASE_URL", server.url())
        .arg("import")
        .assert()
        .failure()
        .stderr(contains("--force"));

    let raw_after = std::fs::read_to_string(&path).unwrap();
    let v: serde_json::Value = serde_json::from_str(&raw_after).unwrap();
    assert_eq!(v["hetzner_cloud"]["server_id"], 1);
    assert_eq!(v["hetzner_cloud"]["server_name"], "stale");
}

#[test]
fn import_force_overwrites_existing_state() {
    let mut server = mockito::Server::new();
    mock_all_lists(&mut server, "platform-1", 0);
    let dir = workspace_with_state("platform-1");

    let path = dir.path().join(".apprafter/state.json");
    let raw = std::fs::read_to_string(&path).unwrap();
    let mut state: serde_json::Value = serde_json::from_str(&raw).unwrap();
    state["hetzner_cloud"] = serde_json::json!({
        "server_id": 1,
        "server_name": "stale",
        "ssh_key_ids": [],
        "network_id": null,
        "firewall_id": null,
        "floating_ip_ids": []
    });
    std::fs::write(&path, serde_json::to_vec_pretty(&state).unwrap()).unwrap();

    cli()
        .current_dir(dir.path())
        .env("HCLOUD_TOKEN", "test-token")
        .env("APPRAFTER_HCLOUD_BASE_URL", server.url())
        .args(["import", "--force"])
        .assert()
        .success()
        .stdout(contains("server: platform-1 (id 42)"));

    let raw_after = std::fs::read_to_string(&path).unwrap();
    let v: serde_json::Value = serde_json::from_str(&raw_after).unwrap();
    assert_eq!(v["hetzner_cloud"]["server_id"], 42);
    assert_eq!(v["hetzner_cloud"]["server_name"], "platform-1");
}

#[test]
fn import_skips_servers_without_apprafter_label() {
    let mut server = mockito::Server::new();
    server
        .mock("GET", "/v1/servers")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(
            r#"{"servers":[
                {"id":99,"name":"platform-1","status":"running","labels":{}}
            ]}"#,
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
    server
        .mock("GET", "/v1/floating_ips")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(r#"{"floating_ips":[]}"#)
        .create();
    let dir = workspace_with_state("platform-1");

    cli()
        .current_dir(dir.path())
        .env("HCLOUD_TOKEN", "test-token")
        .env("APPRAFTER_HCLOUD_BASE_URL", server.url())
        .arg("import")
        .assert()
        .success()
        .stdout(contains("no matching"));
}
