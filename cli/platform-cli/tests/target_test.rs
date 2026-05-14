// SPDX-License-Identifier: FSL-1.1-MIT
//! Integration tests for `apprafter target add` (Track A.3 /
//! v0.1.73).
//!
//! All tests redirect the target store at a per-test `tempfile::
//! TempDir` via the `APPRAFTER_CONFIG_DIR` env-var override
//! (recognised by `cli_core::target::default_config_root`). Each
//! invocation runs in a fresh dir so tests are independent and
//! parallel-safe.
//!
//! Cold-fetch interactive wizard path lives in Track A.4 and is
//! out of scope here — Track A.3 ships pure flag-driven mode only.

use assert_cmd::Command;
use predicates::str::contains;
use std::path::Path;

fn cli() -> Command {
    Command::cargo_bin("apprafter").unwrap()
}

/// 67-char synthetic Hetzner token (`hcloud_` + 60 `a`s) that
/// satisfies the format validator without being a real credential.
/// Tests stay 100 % offline — no Hetzner API ping happens until
/// Track A.4 validator integration.
fn synthetic_hcloud_token() -> String {
    let body = "a".repeat(60);
    format!("hcloud_{body}")
}

/// Drop a fake SSH public key in the supplied dir and return its
/// path. The body content is irrelevant — A.3 only checks the file
/// is readable; OpenSSH-format parsing arrives in Track A.4.
fn write_fake_ssh_pubkey(dir: &Path) -> std::path::PathBuf {
    let path = dir.join("id_ed25519.pub");
    std::fs::write(&path, "ssh-ed25519 AAAA fake test key").unwrap();
    path
}

#[test]
fn target_add_writes_config_and_credentials_and_promotes_first_target_to_active() {
    let dir = tempfile::tempdir().unwrap();
    let token = synthetic_hcloud_token();
    cli()
        .env("APPRAFTER_CONFIG_DIR", dir.path())
        .env_remove("HCLOUD_TOKEN")
        .args([
            "target",
            "add",
            "default",
            "--provider",
            "hetzner-cloud",
            "--token",
            &token,
            "--region",
            "nbg1",
            "--tier",
            "solo",
        ])
        .assert()
        .success()
        .stdout(contains("saved and set as active"));

    // Spec §4 file layout: config.yaml + targets/<name>/config.yaml
    // + targets/<name>/credentials.yaml + auth/.keep.
    assert!(dir.path().join("config.yaml").exists());
    assert!(dir.path().join("targets/default/config.yaml").exists());
    assert!(dir.path().join("targets/default/credentials.yaml").exists());
    assert!(dir.path().join("auth/.keep").exists());

    // Active pointer in the global config matches the just-saved
    // target name.
    let global = std::fs::read_to_string(dir.path().join("config.yaml")).unwrap();
    assert!(
        global.contains("active_target: default"),
        "expected `active_target: default`, got:\n{global}"
    );

    // Credentials file actually contains the token body.
    let creds =
        std::fs::read_to_string(dir.path().join("targets/default/credentials.yaml")).unwrap();
    assert!(creds.contains(&token), "creds:\n{creds}");
}

#[cfg(unix)]
#[test]
fn target_add_credentials_file_is_mode_0600() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    cli()
        .env("APPRAFTER_CONFIG_DIR", dir.path())
        .env_remove("HCLOUD_TOKEN")
        .args([
            "target",
            "add",
            "default",
            "--provider",
            "hetzner-cloud",
            "--token",
            &synthetic_hcloud_token(),
        ])
        .assert()
        .success();

    let mode = std::fs::metadata(dir.path().join("targets/default/credentials.yaml"))
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(
        mode, 0o600,
        "credentials.yaml landed at mode {mode:o} — atomic_write must enforce 0600"
    );
}

#[test]
fn target_add_uses_hcloud_token_env_var_as_fallback() {
    let dir = tempfile::tempdir().unwrap();
    let token = synthetic_hcloud_token();
    cli()
        .env("APPRAFTER_CONFIG_DIR", dir.path())
        .env("HCLOUD_TOKEN", &token)
        .args([
            "target",
            "add",
            "ci",
            "--provider",
            "hetzner-cloud",
            // No --token; clap reads HCLOUD_TOKEN via env attribute.
        ])
        .assert()
        .success()
        .stdout(contains("saved and set as active"));
}

#[test]
fn target_add_errors_when_token_missing_entirely() {
    let dir = tempfile::tempdir().unwrap();
    cli()
        .env("APPRAFTER_CONFIG_DIR", dir.path())
        .env_remove("HCLOUD_TOKEN")
        .args(["target", "add", "ci", "--provider", "hetzner-cloud"])
        .assert()
        .failure()
        .stderr(contains("--token` is required"));
}

#[test]
fn target_add_errors_on_unknown_provider() {
    let dir = tempfile::tempdir().unwrap();
    cli()
        .env("APPRAFTER_CONFIG_DIR", dir.path())
        .env_remove("HCLOUD_TOKEN")
        .args([
            "target",
            "add",
            "ci",
            "--provider",
            "aws-bedrock",
            "--token",
            &synthetic_hcloud_token(),
        ])
        .assert()
        .failure()
        .stderr(contains("not supported"));
}

#[test]
fn target_add_errors_on_malformed_hetzner_token() {
    let dir = tempfile::tempdir().unwrap();
    cli()
        .env("APPRAFTER_CONFIG_DIR", dir.path())
        .env_remove("HCLOUD_TOKEN")
        .args([
            "target",
            "add",
            "ci",
            "--provider",
            "hetzner-cloud",
            "--token",
            "not_a_token",
        ])
        .assert()
        .failure()
        .stderr(contains("invalid Hetzner Cloud token"));
}

#[test]
fn target_add_errors_on_invalid_target_name() {
    let dir = tempfile::tempdir().unwrap();
    cli()
        .env("APPRAFTER_CONFIG_DIR", dir.path())
        .env_remove("HCLOUD_TOKEN")
        .args([
            "target",
            "add",
            "with space",
            "--provider",
            "hetzner-cloud",
            "--token",
            &synthetic_hcloud_token(),
        ])
        .assert()
        .failure()
        .stderr(contains("is invalid"));
}

#[test]
fn target_add_refuses_to_overwrite_existing_target_without_force() {
    let dir = tempfile::tempdir().unwrap();
    let token = synthetic_hcloud_token();
    for _ in 0..1 {
        cli()
            .env("APPRAFTER_CONFIG_DIR", dir.path())
            .env_remove("HCLOUD_TOKEN")
            .args([
                "target",
                "add",
                "work",
                "--provider",
                "hetzner-cloud",
                "--token",
                &token,
            ])
            .assert()
            .success();
    }
    // Second invocation: must refuse.
    cli()
        .env("APPRAFTER_CONFIG_DIR", dir.path())
        .env_remove("HCLOUD_TOKEN")
        .args([
            "target",
            "add",
            "work",
            "--provider",
            "hetzner-cloud",
            "--token",
            &token,
        ])
        .assert()
        .failure()
        .stderr(contains("already exists"))
        .stderr(contains("--force"))
        .stderr(contains("--renew"));
}

#[test]
fn target_add_force_overwrites_existing_target_and_keeps_active_pointer() {
    let dir = tempfile::tempdir().unwrap();
    let token1 = synthetic_hcloud_token();
    // 60 'b's to differentiate clearly from token1.
    let body = "b".repeat(60);
    let token2 = format!("hcloud_{body}");

    cli()
        .env("APPRAFTER_CONFIG_DIR", dir.path())
        .env_remove("HCLOUD_TOKEN")
        .args([
            "target",
            "add",
            "work",
            "--provider",
            "hetzner-cloud",
            "--token",
            &token1,
            "--region",
            "nbg1",
        ])
        .assert()
        .success();

    cli()
        .env("APPRAFTER_CONFIG_DIR", dir.path())
        .env_remove("HCLOUD_TOKEN")
        .args([
            "target",
            "add",
            "work",
            "--provider",
            "hetzner-cloud",
            "--token",
            &token2,
            "--region",
            "fsn1",
            "--force",
        ])
        .assert()
        .success()
        // Second save is not the first target → message must NOT
        // claim it became active. Pre-existing global config is
        // preserved.
        .stdout(contains("active target unchanged"));

    // Region flag in the overwrite must land in the on-disk config.
    let cfg = std::fs::read_to_string(dir.path().join("targets/work/config.yaml")).unwrap();
    assert!(cfg.contains("region: fsn1"), "{cfg}");
    // Token in the overwrite is the new one.
    let creds = std::fs::read_to_string(dir.path().join("targets/work/credentials.yaml")).unwrap();
    assert!(creds.contains(&token2));
    assert!(!creds.contains(&token1));
}

#[test]
fn target_add_renew_rotates_credentials_without_touching_config() {
    let dir = tempfile::tempdir().unwrap();
    let token1 = synthetic_hcloud_token();
    let body = "c".repeat(60);
    let token2 = format!("hcloud_{body}");

    cli()
        .env("APPRAFTER_CONFIG_DIR", dir.path())
        .env_remove("HCLOUD_TOKEN")
        .args([
            "target",
            "add",
            "prod",
            "--provider",
            "hetzner-cloud",
            "--token",
            &token1,
            "--region",
            "fsn1",
            "--cluster-name",
            "edge-1",
        ])
        .assert()
        .success();

    cli()
        .env("APPRAFTER_CONFIG_DIR", dir.path())
        .env_remove("HCLOUD_TOKEN")
        .args(["target", "add", "prod", "--token", &token2, "--renew"])
        .assert()
        .success()
        .stdout(contains("credentials rotated"));

    let cfg = std::fs::read_to_string(dir.path().join("targets/prod/config.yaml")).unwrap();
    assert!(cfg.contains("region: fsn1"), "{cfg}");
    assert!(cfg.contains("cluster_name: edge-1"), "{cfg}");
    let creds = std::fs::read_to_string(dir.path().join("targets/prod/credentials.yaml")).unwrap();
    assert!(creds.contains(&token2));
    assert!(!creds.contains(&token1));
}

#[test]
fn target_add_renew_on_missing_target_errors_with_hint() {
    let dir = tempfile::tempdir().unwrap();
    cli()
        .env("APPRAFTER_CONFIG_DIR", dir.path())
        .env_remove("HCLOUD_TOKEN")
        .args([
            "target",
            "add",
            "ghost",
            "--token",
            &synthetic_hcloud_token(),
            "--renew",
        ])
        .assert()
        .failure()
        .stderr(contains("does not exist"))
        .stderr(contains("drop `--renew`"));
}

#[test]
fn target_add_renew_rejects_config_flags() {
    let dir = tempfile::tempdir().unwrap();
    let token = synthetic_hcloud_token();
    cli()
        .env("APPRAFTER_CONFIG_DIR", dir.path())
        .env_remove("HCLOUD_TOKEN")
        .args([
            "target",
            "add",
            "live",
            "--provider",
            "hetzner-cloud",
            "--token",
            &token,
        ])
        .assert()
        .success();
    cli()
        .env("APPRAFTER_CONFIG_DIR", dir.path())
        .env_remove("HCLOUD_TOKEN")
        .args([
            "target", "add", "live", "--token", &token, "--region", "hel1", "--renew",
        ])
        .assert()
        .failure()
        .stderr(contains("only updates credentials"));
}

#[test]
fn target_add_force_and_renew_are_mutually_exclusive() {
    // clap's `conflicts_with` should reject both flags up front
    // before the handler runs.
    let dir = tempfile::tempdir().unwrap();
    cli()
        .env("APPRAFTER_CONFIG_DIR", dir.path())
        .env_remove("HCLOUD_TOKEN")
        .args([
            "target",
            "add",
            "any",
            "--provider",
            "hetzner-cloud",
            "--token",
            &synthetic_hcloud_token(),
            "--force",
            "--renew",
        ])
        .assert()
        .failure()
        .stderr(contains("cannot be used"));
}

#[test]
fn target_add_with_ssh_key_path_verifies_file_exists() {
    let dir = tempfile::tempdir().unwrap();
    let token = synthetic_hcloud_token();
    let pubkey = write_fake_ssh_pubkey(dir.path());

    cli()
        .env("APPRAFTER_CONFIG_DIR", dir.path())
        .env_remove("HCLOUD_TOKEN")
        .env_remove("APPRAFTER_SSH_PUBLIC_KEY_PATH")
        .args([
            "target",
            "add",
            "ops",
            "--provider",
            "hetzner-cloud",
            "--token",
            &token,
            "--ssh-key",
            pubkey.to_str().unwrap(),
        ])
        .assert()
        .success();

    let cfg = std::fs::read_to_string(dir.path().join("targets/ops/config.yaml")).unwrap();
    assert!(cfg.contains(pubkey.to_str().unwrap()), "{cfg}");
}

#[test]
fn target_add_errors_when_ssh_key_path_missing() {
    let dir = tempfile::tempdir().unwrap();
    cli()
        .env("APPRAFTER_CONFIG_DIR", dir.path())
        .env_remove("HCLOUD_TOKEN")
        .env_remove("APPRAFTER_SSH_PUBLIC_KEY_PATH")
        .args([
            "target",
            "add",
            "ops",
            "--provider",
            "hetzner-cloud",
            "--token",
            &synthetic_hcloud_token(),
            "--ssh-key",
            "/does/not/exist/key.pub",
        ])
        .assert()
        .failure()
        .stderr(contains("does not exist"));
}

#[test]
fn second_target_save_keeps_first_as_active_and_reports_so() {
    let dir = tempfile::tempdir().unwrap();
    let token = synthetic_hcloud_token();

    cli()
        .env("APPRAFTER_CONFIG_DIR", dir.path())
        .env_remove("HCLOUD_TOKEN")
        .args([
            "target",
            "add",
            "first",
            "--provider",
            "hetzner-cloud",
            "--token",
            &token,
        ])
        .assert()
        .success()
        .stdout(contains("saved and set as active"));

    cli()
        .env("APPRAFTER_CONFIG_DIR", dir.path())
        .env_remove("HCLOUD_TOKEN")
        .args([
            "target",
            "add",
            "second",
            "--provider",
            "hetzner-cloud",
            "--token",
            &token,
        ])
        .assert()
        .success()
        .stdout(contains("active target unchanged"));

    // Global config still points at the first target — Track A.5
    // brings `apprafter target use <name>` for switching.
    let global = std::fs::read_to_string(dir.path().join("config.yaml")).unwrap();
    assert!(
        global.contains("active_target: first"),
        "expected active to remain `first`, got:\n{global}"
    );
}

#[test]
fn target_alias_t_subcommand_resolves_to_target() {
    // Smoke for the `apprafter t add …` alias declared in clap.
    let dir = tempfile::tempdir().unwrap();
    cli()
        .env("APPRAFTER_CONFIG_DIR", dir.path())
        .env_remove("HCLOUD_TOKEN")
        .args([
            "t",
            "add",
            "via-alias",
            "--provider",
            "hetzner-cloud",
            "--token",
            &synthetic_hcloud_token(),
        ])
        .assert()
        .success();
    assert!(dir.path().join("targets/via-alias/config.yaml").exists());
}
