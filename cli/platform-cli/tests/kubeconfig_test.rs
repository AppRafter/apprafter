// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Integration tests for `apprafter kubeconfig`.
//!
//! The cold-fetch path exercises `ssh` against the live server and
//! is covered in-process by the FakeFetcher unit tests in
//! commands/kubeconfig.rs. These integration tests cover everything
//! else: clap surface, missing-state error, and the cache path
//! (which never touches SSH).
//!
//! v0.1.154 layout: state lives at
//! `<APPRAFTER_CONFIG_DIR>/state/<active-target>/.apprafter/state.json`.
//! Helpers below seed an active target "default" so `init` writes
//! into the per-target slot under an isolated config dir.

mod common;

use common::spawn::Spawn;

use std::path::{Path, PathBuf};

use assert_cmd::Command;
use predicates::str::contains;

fn cli(sp: &Spawn) -> Command {
    sp.cmd(&[])
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
    spawn: Spawn,
}

fn workspace_with_state() -> Workspace {
    let cwd = tempfile::tempdir().unwrap();
    let cfg_dir = tempfile::tempdir().unwrap();
    let spawn = Spawn::new().with_store(cfg_dir.path());

    cli(&spawn)
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

    cli(&spawn)
        .current_dir(cwd.path())
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

    Workspace {
        cwd,
        cfg_dir,
        spawn,
    }
}

#[test]
fn kubeconfig_without_hetzner_cloud_state_errors_with_hint() {
    let ws = workspace_with_state();
    cli(&ws.spawn)
        .current_dir(ws.cwd.path())
        .arg("kubeconfig")
        .assert()
        .failure()
        .stderr(contains("hetzner_cloud"))
        .stderr(contains("apply"));
}

#[test]
fn kubeconfig_prints_cached_yaml_without_touching_ssh() {
    let ws = workspace_with_state();

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
        "kubeconfig_yaml": "apiVersion: v1\nkind: Config\nfrom: cache\n"
    });
    std::fs::write(&path, serde_json::to_vec_pretty(&state).unwrap()).unwrap();

    cli(&ws.spawn)
        .current_dir(ws.cwd.path())
        .arg("kubeconfig")
        .assert()
        .success()
        .stdout(contains("from: cache"));
}

#[test]
fn kubeconfig_decrypts_age_blob_using_identity_at_apprafter_age_key() {
    use age::secrecy::ExposeSecret;
    use std::io::Write;

    let ws = workspace_with_state();

    let identity = age::x25519::Identity::generate();
    let key_path = ws.spawn.path("age.key");
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
        // age 0.11: an iterator of &dyn Recipient, validated eagerly.
        let recipient_ref: &dyn age::Recipient = &recipient;
        let encryptor = age::Encryptor::with_recipients(std::iter::once(recipient_ref)).unwrap();
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
        "kubeconfig_age": armored,
    });
    std::fs::write(&path, serde_json::to_vec_pretty(&state).unwrap()).unwrap();

    cli(&ws.spawn)
        .current_dir(ws.cwd.path())
        .env("APPRAFTER_AGE_KEY", &key_path)
        .arg("kubeconfig")
        .assert()
        .success()
        .stdout(contains("from: age"));
}

/// `plaintext`, sealed to `identity` as the target's cached kubeconfig.
fn cache_sealed_to(ws: &Workspace, identity: &age::x25519::Identity, plaintext: &str) {
    let armored = cli_core::secrets::encrypt_for_recipient(plaintext, &identity.to_public())
        .expect("seal the kubeconfig");
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
        "kubeconfig_age": armored,
    });
    std::fs::write(&path, serde_json::to_vec_pretty(&state).unwrap()).unwrap();
}

/// WI-478: the key file `age-keygen -o` writes — `# created:` and `# public key:` lines, then
/// the key — is read as it is (d4a-h F2: it was the catch-all "invalid Bech32 encoding").
#[test]
fn kubeconfig_reads_a_key_file_in_age_keygens_format() {
    use age::secrecy::ExposeSecret;
    let ws = workspace_with_state();
    let identity = age::x25519::Identity::generate();
    cache_sealed_to(
        &ws,
        &identity,
        "apiVersion: v1\nkind: Config\nfrom: keygen\n",
    );
    let key_path = ws.spawn.path("age.key");
    let text = format!(
        "# created: 2026-10-14T18:00:00+01:00\n# public key: {}\n{}\n",
        identity.to_public(),
        identity.to_string().expose_secret()
    );
    std::fs::write(&key_path, &text).unwrap();
    cli(&ws.spawn)
        .current_dir(ws.cwd.path())
        .env("APPRAFTER_AGE_KEY", &key_path)
        .arg("kubeconfig")
        .assert()
        .success()
        .stdout(contains("from: keygen"));
    assert_eq!(std::fs::read_to_string(&key_path).unwrap(), text);
}

/// WI-478: a key file with no key, or with two, is `age_key_identity_count` naming the file —
/// never the catch-all — and is left as it is.
#[test]
fn kubeconfig_refuses_a_key_file_with_no_key_or_two_by_name() {
    use age::secrecy::ExposeSecret;
    let ws = workspace_with_state();
    let identity = age::x25519::Identity::generate();
    cache_sealed_to(&ws, &identity, "apiVersion: v1\nkind: Config\n");
    let key = identity.to_string();
    let other = age::x25519::Identity::generate().to_string();
    for (name, text, says) in [
        (
            "none.key",
            format!("# public key: {}\n", identity.to_public()),
            "holds no key",
        ),
        (
            "two.key",
            format!("{}\n{}\n", key.expose_secret(), other.expose_secret()),
            "has 2 lines that are neither blank nor a comment",
        ),
    ] {
        let key_path = ws.spawn.path(name);
        std::fs::write(&key_path, &text).unwrap();
        let out = cli(&ws.spawn)
            .current_dir(ws.cwd.path())
            .env("APPRAFTER_AGE_KEY", &key_path)
            .arg("kubeconfig")
            .output()
            .unwrap();
        let stderr: String = String::from_utf8_lossy(&out.stderr)
            .replace('│', " ")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        assert_eq!(out.status.code(), Some(1), "{name}: {stderr}");
        assert!(out.stdout.is_empty(), "{name}");
        assert!(
            stderr.contains("apprafter::secrets::age_key_identity_count")
                && !stderr.contains("apprafter::cli::other")
                && stderr.contains(says),
            "{name}: {stderr}"
        );
        let unwrapped: String = stderr.split_whitespace().collect();
        let shown: String = key_path.display().to_string().split_whitespace().collect();
        assert!(unwrapped.contains(&shown), "{name}: {stderr}");
        assert_eq!(std::fs::read_to_string(&key_path).unwrap(), text, "{name}");
    }
}
