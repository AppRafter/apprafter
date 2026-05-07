// SPDX-License-Identifier: FSL-1.1-MIT
//! Integration tests for `platform-cli kubeconfig`.
//!
//! The cold-fetch path exercises `ssh` against the live server and
//! is covered in-process by the FakeFetcher unit tests in
//! commands/kubeconfig.rs. These integration tests cover everything
//! else: clap surface, missing-state error, and the cache path
//! (which never touches SSH).

use assert_cmd::Command;
use predicates::str::contains;

fn cli() -> Command {
    Command::cargo_bin("platform-cli").unwrap()
}

fn workspace_with_state() -> tempfile::TempDir {
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
    dir
}

#[test]
fn kubeconfig_without_hetzner_cloud_state_errors_with_hint() {
    let dir = workspace_with_state();
    cli()
        .current_dir(dir.path())
        .arg("kubeconfig")
        .assert()
        .failure()
        .stderr(contains("hetzner_cloud"))
        .stderr(contains("apply"));
}

#[test]
fn kubeconfig_prints_cached_yaml_without_touching_ssh() {
    let dir = workspace_with_state();

    let path = dir.path().join(".apprafter/state.json");
    let raw = std::fs::read_to_string(&path).unwrap();
    let mut state: serde_json::Value = serde_json::from_str(&raw).unwrap();
    state["hetzner_cloud"] = serde_json::json!({
        "server_id": 1,
        "server_name": "platform-1",
        "ssh_key_ids": [],
        "network_id": null,
        "firewall_id": null,
        "floating_ip_ids": [],
        "kubeconfig_yaml": "apiVersion: v1\nkind: Config\nfrom: cache\n"
    });
    std::fs::write(&path, serde_json::to_vec_pretty(&state).unwrap()).unwrap();

    cli()
        .current_dir(dir.path())
        .env_remove("HCLOUD_TOKEN")
        .env_remove("APPRAFTER_HCLOUD_BASE_URL")
        .arg("kubeconfig")
        .assert()
        .success()
        .stdout(contains("from: cache"));
}
