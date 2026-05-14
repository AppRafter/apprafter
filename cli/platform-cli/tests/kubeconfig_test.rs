// SPDX-License-Identifier: FSL-1.1-MIT
//! Integration tests for `apprafter kubeconfig`.
//!
//! The cold-fetch path exercises `ssh` against the live server and
//! is covered in-process by the FakeFetcher unit tests in
//! commands/kubeconfig.rs. These integration tests cover everything
//! else: clap surface, missing-state error, and the cache path
//! (which never touches SSH).

use assert_cmd::Command;
use predicates::str::contains;

fn cli() -> Command {
    Command::cargo_bin("apprafter").unwrap()
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

#[test]
fn kubeconfig_decrypts_age_blob_using_identity_at_apprafter_age_key() {
    use age::secrecy::ExposeSecret;
    use std::io::Write;

    let dir = workspace_with_state();

    let identity = age::x25519::Identity::generate();
    let key_path = dir.path().join("age.key");
    {
        let mut f = std::fs::File::create(&key_path).unwrap();
        f.write_all(identity.to_string().expose_secret().as_bytes())
            .unwrap();
        f.write_all(b"\n").unwrap();
    }

    let recipient = identity.to_public();
    let plaintext = "apiVersion: v1\nkind: Config\nfrom: age\n";
    let mut armored_buf: Vec<u8> = Vec::new();
    {
        let recipients: Vec<Box<dyn age::Recipient + Send>> = vec![Box::new(recipient)];
        let encryptor = age::Encryptor::with_recipients(recipients).unwrap();
        let armored = age::armor::ArmoredWriter::wrap_output(
            &mut armored_buf,
            age::armor::Format::AsciiArmor,
        )
        .unwrap();
        let mut writer = encryptor.wrap_output(armored).unwrap();
        writer.write_all(plaintext.as_bytes()).unwrap();
        let armor = writer.finish().unwrap();
        armor.finish().unwrap();
    }
    let armored = String::from_utf8(armored_buf).unwrap();

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
        "kubeconfig_yaml": null,
        "kubeconfig_age": armored,
    });
    std::fs::write(&path, serde_json::to_vec_pretty(&state).unwrap()).unwrap();

    cli()
        .current_dir(dir.path())
        .env("APPRAFTER_AGE_KEY", &key_path)
        .env_remove("HCLOUD_TOKEN")
        .env_remove("APPRAFTER_HCLOUD_BASE_URL")
        .arg("kubeconfig")
        .assert()
        .success()
        .stdout(contains("from: age"));
}
