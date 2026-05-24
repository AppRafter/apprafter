// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Integration tests for `apprafter argocd-password`.
//!
//! The cold-fetch path goes through real `kubectl` against a live
//! cluster — covered by manual smoke (cluster_smoke_test.rs sister
//! suite) when needed. The cached path doesn't need a cluster, so
//! we pin its end-to-end behaviour here.
//!
//! v0.1.154: state lives at
//! `<APPRAFTER_CONFIG_DIR>/state/<active-target>/.apprafter/state.json`.

use std::path::{Path, PathBuf};

use age::secrecy::ExposeSecret;
use assert_cmd::Command;
use predicates::str::contains;
use std::io::Write;

fn cli() -> Command {
    Command::cargo_bin("apprafter").unwrap()
}

fn state_path(cfg_dir: &Path, target: &str) -> PathBuf {
    cfg_dir
        .join("state")
        .join(target)
        .join(".apprafter")
        .join("state.json")
}

struct Workspace {
    cwd: tempfile::TempDir,
    cfg_dir: tempfile::TempDir,
}

fn workspace_with_state() -> Workspace {
    let cwd = tempfile::tempdir().unwrap();
    let cfg_dir = tempfile::tempdir().unwrap();

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

    Workspace { cwd, cfg_dir }
}

#[test]
fn argocd_password_decrypts_age_blob_using_identity_at_apprafter_age_key() {
    let ws = workspace_with_state();

    // Fresh identity in a temp file.
    let identity = age::x25519::Identity::generate();
    let key_path = ws.cwd.path().join("age.key");
    {
        let mut f = std::fs::File::create(&key_path).unwrap();
        f.write_all(identity.to_string().expose_secret().as_bytes())
            .unwrap();
        f.write_all(b"\n").unwrap();
    }

    // Encrypt a known password with the recipient.
    let recipient = identity.to_public();
    let plaintext = "argo-admin-hunter2";
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

    // Pre-populate state with the encrypted password.
    let path = state_path(ws.cfg_dir.path(), "default");
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
        "kubeconfig_age": null,
        "argocd_admin_password_age": armored,
    });
    std::fs::write(&path, serde_json::to_vec_pretty(&state).unwrap()).unwrap();

    cli()
        .current_dir(ws.cwd.path())
        .env("APPRAFTER_CONFIG_DIR", ws.cfg_dir.path())
        .env("APPRAFTER_AGE_KEY", &key_path)
        .arg("argocd-password")
        .assert()
        .success()
        .stdout(contains("argo-admin-hunter2"));
}
