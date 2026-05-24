// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Integration tests for `apprafter import` against a mockito
//! Hetzner Cloud server.
//!
//! ## v0.1.154 layout
//!
//! State now lives at
//! `<APPRAFTER_CONFIG_DIR>/state/<active-target>/.apprafter/state.json`
//! instead of `<cwd>/.apprafter/state.json`. The test helpers
//! below seed a target named `default` (which becomes active on
//! creation) and inspect the per-target file. Independence from
//! the developer's real `~/.config/apprafter/` is enforced by
//! always supplying `APPRAFTER_CONFIG_DIR` to the spawned
//! processes.

use std::path::{Path, PathBuf};

use assert_cmd::Command;
use predicates::str::contains;

fn cli() -> Command {
    Command::cargo_bin("apprafter").unwrap()
}

/// Path to the per-target `state.json` under a given config dir.
/// Mirrors `cli_state::StatePaths::for_active_target`'s layout.
fn state_path(cfg_dir: &Path, target: &str) -> PathBuf {
    cfg_dir
        .join("state")
        .join(target)
        .join(".apprafter")
        .join("state.json")
}

/// Workspace + isolated config dir + an active target seeded
/// with the per-target state that `init` would write. Returns
/// both temp dirs so callers can keep them alive for the test
/// lifetime (drop = remove); returns the config dir explicitly
/// since the state path lives under it, not under cwd.
struct Workspace {
    cwd: tempfile::TempDir,
    cfg_dir: tempfile::TempDir,
}

fn workspace_with_state(cluster: &str) -> Workspace {
    let cwd = tempfile::tempdir().unwrap();
    let cfg_dir = tempfile::tempdir().unwrap();

    // Create an active target. `target add` writes
    // `GlobalConfig.active_target = "default"` on the first
    // call so subsequent `init` / `apply` resolve to it.
    cli()
        .env("APPRAFTER_CONFIG_DIR", cfg_dir.path())
        .env_remove("HCLOUD_TOKEN")
        .args([
            "target",
            "add",
            "default",
            "--provider",
            "hetzner-cloud",
            "--token",
            &"a".repeat(64),
            "--region",
            "nbg1",
            "--tier",
            "solo",
            "--no-ping",
            "--no-interactive",
        ])
        .assert()
        .success();

    // Init now writes to <cfg_dir>/state/default/.apprafter/state.json.
    cli()
        .current_dir(cwd.path())
        .env("APPRAFTER_CONFIG_DIR", cfg_dir.path())
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
        let path = state_path(cfg_dir.path(), "default");
        let raw = std::fs::read_to_string(&path).unwrap();
        let mut state: serde_json::Value = serde_json::from_str(&raw).unwrap();
        state["cluster_name"] = serde_json::Value::String(cluster.into());
        std::fs::write(path, serde_json::to_vec_pretty(&state).unwrap()).unwrap();
    }
    Workspace { cwd, cfg_dir }
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
    let ws = workspace_with_state("platform-1");

    cli()
        .current_dir(ws.cwd.path())
        .env("APPRAFTER_CONFIG_DIR", ws.cfg_dir.path())
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

    let raw = std::fs::read_to_string(state_path(ws.cfg_dir.path(), "default")).unwrap();
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
    let ws = workspace_with_state("platform-1");

    cli()
        .current_dir(ws.cwd.path())
        .env("APPRAFTER_CONFIG_DIR", ws.cfg_dir.path())
        .env("HCLOUD_TOKEN", "test-token")
        .env("APPRAFTER_HCLOUD_BASE_URL", server.url())
        .args(["import", "--dry-run"])
        .assert()
        .success()
        .stdout(contains("would write"))
        .stdout(contains("server: platform-1 (id 42)"));

    let raw = std::fs::read_to_string(state_path(ws.cfg_dir.path(), "default")).unwrap();
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
    let ws = workspace_with_state("platform-1");

    cli()
        .current_dir(ws.cwd.path())
        .env("APPRAFTER_CONFIG_DIR", ws.cfg_dir.path())
        .env("HCLOUD_TOKEN", "test-token")
        .env("APPRAFTER_HCLOUD_BASE_URL", server.url())
        .arg("import")
        .assert()
        .success()
        .stdout(contains("no matching"))
        .stdout(contains("platform-1"));

    let raw = std::fs::read_to_string(state_path(ws.cfg_dir.path(), "default")).unwrap();
    let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert!(v["hetzner_cloud"].is_null());
}

#[test]
fn import_refuses_to_overwrite_existing_state_without_force() {
    let mut server = mockito::Server::new();
    mock_all_lists(&mut server, "platform-1", 0);
    let ws = workspace_with_state("platform-1");

    let path = state_path(ws.cfg_dir.path(), "default");
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
        .current_dir(ws.cwd.path())
        .env("APPRAFTER_CONFIG_DIR", ws.cfg_dir.path())
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
    let ws = workspace_with_state("platform-1");

    let path = state_path(ws.cfg_dir.path(), "default");
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
        .current_dir(ws.cwd.path())
        .env("APPRAFTER_CONFIG_DIR", ws.cfg_dir.path())
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
    let ws = workspace_with_state("platform-1");

    cli()
        .current_dir(ws.cwd.path())
        .env("APPRAFTER_CONFIG_DIR", ws.cfg_dir.path())
        .env("HCLOUD_TOKEN", "test-token")
        .env("APPRAFTER_HCLOUD_BASE_URL", server.url())
        .arg("import")
        .assert()
        .success()
        .stdout(contains("no matching"));
}
