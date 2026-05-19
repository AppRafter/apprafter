// SPDX-License-Identifier: FSL-1.1-Apache-2.0
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
use predicates::prelude::PredicateBooleanExt;
use predicates::str::contains;
use std::path::Path;

fn cli() -> Command {
    Command::cargo_bin("apprafter").unwrap()
}

/// 64-char synthetic Hetzner token (canonical alphanumeric, no
/// prefix — matches what Hetzner Cloud Console actually issues
/// per `cli-dx-task.md` §11 as amended in v0.1.74). Lets tests
/// stay 100 % offline — no Hetzner API ping happens until Track
/// A.4 validator integration.
fn synthetic_hetzner_token() -> String {
    "a".repeat(64)
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
    let token = synthetic_hetzner_token();
    cli()
        .env("APPRAFTER_CONFIG_DIR", dir.path())
        .env("APPRAFTER_NO_PING", "1")
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
        .env("APPRAFTER_NO_PING", "1")
        .env_remove("HCLOUD_TOKEN")
        .args([
            "target",
            "add",
            "default",
            "--provider",
            "hetzner-cloud",
            "--token",
            &synthetic_hetzner_token(),
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
    let token = synthetic_hetzner_token();
    cli()
        .env("APPRAFTER_CONFIG_DIR", dir.path())
        .env("APPRAFTER_NO_PING", "1")
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
        .env("APPRAFTER_NO_PING", "1")
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
        .env("APPRAFTER_NO_PING", "1")
        .env_remove("HCLOUD_TOKEN")
        .args([
            "target",
            "add",
            "ci",
            "--provider",
            "aws-bedrock",
            "--token",
            &synthetic_hetzner_token(),
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
        .env("APPRAFTER_NO_PING", "1")
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
        .env("APPRAFTER_NO_PING", "1")
        .env_remove("HCLOUD_TOKEN")
        .args([
            "target",
            "add",
            "with space",
            "--provider",
            "hetzner-cloud",
            "--token",
            &synthetic_hetzner_token(),
        ])
        .assert()
        .failure()
        .stderr(contains("is invalid"));
}

#[test]
fn target_add_refuses_to_overwrite_existing_target_without_force() {
    let dir = tempfile::tempdir().unwrap();
    let token = synthetic_hetzner_token();
    for _ in 0..1 {
        cli()
            .env("APPRAFTER_CONFIG_DIR", dir.path())
            .env("APPRAFTER_NO_PING", "1")
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
        .env("APPRAFTER_NO_PING", "1")
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
    let token1 = synthetic_hetzner_token();
    // 64 'b's so it passes the strict alphanumeric validator and
    // differs byte-for-byte from token1 (all 'a's) so the rotation
    // assertion can detect either-or.
    let token2 = "b".repeat(64);

    cli()
        .env("APPRAFTER_CONFIG_DIR", dir.path())
        .env("APPRAFTER_NO_PING", "1")
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
        .env("APPRAFTER_NO_PING", "1")
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
    let token1 = synthetic_hetzner_token();
    // 64 'c's — different from token1 (all 'a's) so the rotation
    // assertion can prove old credentials were really overwritten.
    let token2 = "c".repeat(64);

    cli()
        .env("APPRAFTER_CONFIG_DIR", dir.path())
        .env("APPRAFTER_NO_PING", "1")
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
        .env("APPRAFTER_NO_PING", "1")
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
        .env("APPRAFTER_NO_PING", "1")
        .env_remove("HCLOUD_TOKEN")
        .args([
            "target",
            "add",
            "ghost",
            "--token",
            &synthetic_hetzner_token(),
            "--renew",
        ])
        .assert()
        .failure()
        .stderr(contains("does not exist"))
        .stderr(contains("drop `--renew`"));
}

#[test]
fn target_add_renew_rejects_identical_token_with_rotation_hint() {
    // The v0.1.77 walk surfaced that `--renew` happily "rotated"
    // a target to the exact same token bytes — green checkmark,
    // zero actual change in Hetzner. v0.1.78 makes that case
    // fail loudly so the operator hits the Hetzner Cloud Console
    // and generates a fresh token instead of having a silent
    // no-op.
    let dir = tempfile::tempdir().unwrap();
    let token = synthetic_hetzner_token();
    cli()
        .env("APPRAFTER_CONFIG_DIR", dir.path())
        .env("APPRAFTER_NO_PING", "1")
        .env_remove("HCLOUD_TOKEN")
        .args([
            "target",
            "add",
            "rotate-me",
            "--provider",
            "hetzner-cloud",
            "--token",
            &token,
        ])
        .assert()
        .success();

    // Same token supplied to --renew → reject.
    cli()
        .env("APPRAFTER_CONFIG_DIR", dir.path())
        .env("APPRAFTER_NO_PING", "1")
        .env_remove("HCLOUD_TOKEN")
        .args(["target", "add", "rotate-me", "--token", &token, "--renew"])
        .assert()
        .failure()
        .stderr(contains("requires a NEW token"))
        .stderr(contains("Hetzner Cloud Console"));

    // On-disk credentials must still reflect the original token,
    // not be wiped or corrupted by the failed renew attempt.
    let creds =
        std::fs::read_to_string(dir.path().join("targets/rotate-me/credentials.yaml")).unwrap();
    assert!(
        creds.contains(&token),
        "original token must remain on disk after a rejected renew, got:\n{creds}"
    );
}

#[test]
fn target_add_renew_accepts_genuinely_new_token() {
    let dir = tempfile::tempdir().unwrap();
    let old = synthetic_hetzner_token();
    let new = "b".repeat(64);
    assert_ne!(old, new, "test fixtures must differ");

    cli()
        .env("APPRAFTER_CONFIG_DIR", dir.path())
        .env("APPRAFTER_NO_PING", "1")
        .env_remove("HCLOUD_TOKEN")
        .args([
            "target",
            "add",
            "rotate-me",
            "--provider",
            "hetzner-cloud",
            "--token",
            &old,
        ])
        .assert()
        .success();

    cli()
        .env("APPRAFTER_CONFIG_DIR", dir.path())
        .env("APPRAFTER_NO_PING", "1")
        .env_remove("HCLOUD_TOKEN")
        .args(["target", "add", "rotate-me", "--token", &new, "--renew"])
        .assert()
        .success()
        .stdout(contains("credentials rotated"));

    let creds =
        std::fs::read_to_string(dir.path().join("targets/rotate-me/credentials.yaml")).unwrap();
    assert!(creds.contains(&new), "new token must land on disk");
    assert!(!creds.contains(&old), "old token must be replaced");
}

#[test]
fn target_add_renew_rejects_config_flags() {
    let dir = tempfile::tempdir().unwrap();
    let token = synthetic_hetzner_token();
    cli()
        .env("APPRAFTER_CONFIG_DIR", dir.path())
        .env("APPRAFTER_NO_PING", "1")
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
        .env("APPRAFTER_NO_PING", "1")
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
        .env("APPRAFTER_NO_PING", "1")
        .env_remove("HCLOUD_TOKEN")
        .args([
            "target",
            "add",
            "any",
            "--provider",
            "hetzner-cloud",
            "--token",
            &synthetic_hetzner_token(),
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
    let token = synthetic_hetzner_token();
    let pubkey = write_fake_ssh_pubkey(dir.path());

    cli()
        .env("APPRAFTER_CONFIG_DIR", dir.path())
        .env("APPRAFTER_NO_PING", "1")
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
        .env("APPRAFTER_NO_PING", "1")
        .env_remove("HCLOUD_TOKEN")
        .env_remove("APPRAFTER_SSH_PUBLIC_KEY_PATH")
        .args([
            "target",
            "add",
            "ops",
            "--provider",
            "hetzner-cloud",
            "--token",
            &synthetic_hetzner_token(),
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
    let token = synthetic_hetzner_token();

    cli()
        .env("APPRAFTER_CONFIG_DIR", dir.path())
        .env("APPRAFTER_NO_PING", "1")
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
        .env("APPRAFTER_NO_PING", "1")
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

// ---------------------------------------------------------------
// CRUD commands (Track A.5 / v0.1.79) — list / use / show /
// rename / remove. All run against a per-test tempdir + skip the
// API ping (APPRAFTER_NO_PING=1) since they touch only the on-disk
// store.
// ---------------------------------------------------------------

/// Seed `dir` with two targets so list/show/rename/remove have
/// something to operate on. The first one becomes active by the
/// `first-on-fresh-store` rule.
fn seed_two_targets(dir: &Path) {
    let token = synthetic_hetzner_token();
    for (name, region) in &[("first", "nbg1"), ("second", "fsn1")] {
        cli()
            .env("APPRAFTER_CONFIG_DIR", dir)
            .env("APPRAFTER_NO_PING", "1")
            .env_remove("HCLOUD_TOKEN")
            .args([
                "target",
                "add",
                name,
                "--provider",
                "hetzner-cloud",
                "--token",
                &token,
                "--region",
                region,
                "--tier",
                "solo",
            ])
            .assert()
            .success();
    }
}

#[test]
fn target_list_on_empty_store_prints_onboarding_hint() {
    let dir = tempfile::tempdir().unwrap();
    cli()
        .env("APPRAFTER_CONFIG_DIR", dir.path())
        .env("APPRAFTER_NO_PING", "1")
        .args(["target", "list"])
        .assert()
        .success()
        .stdout(contains("No targets configured"))
        .stdout(contains("apprafter target add"));
}

#[test]
fn target_list_renders_table_with_active_marker_and_columns() {
    let dir = tempfile::tempdir().unwrap();
    seed_two_targets(dir.path());

    cli()
        .env("APPRAFTER_CONFIG_DIR", dir.path())
        .env("APPRAFTER_NO_PING", "1")
        .args(["target", "list"])
        .assert()
        .success()
        // Column headers from `tabled` derive.
        .stdout(contains("Active"))
        .stdout(contains("Name"))
        .stdout(contains("Provider"))
        .stdout(contains("Region"))
        .stdout(contains("Tier"))
        // Both targets present.
        .stdout(contains("first"))
        .stdout(contains("second"))
        .stdout(contains("nbg1"))
        .stdout(contains("fsn1"))
        // Active is `first` (created first → auto-promoted).
        .stdout(contains("Active: 'first'"));
}

#[test]
fn target_use_switches_active_pointer_and_reports_the_swap() {
    let dir = tempfile::tempdir().unwrap();
    seed_two_targets(dir.path());

    cli()
        .env("APPRAFTER_CONFIG_DIR", dir.path())
        .env("APPRAFTER_NO_PING", "1")
        .args(["target", "use", "second"])
        .assert()
        .success()
        .stdout(contains("first"))
        .stdout(contains("second"));

    // On-disk global config now points at `second`.
    let global = std::fs::read_to_string(dir.path().join("config.yaml")).unwrap();
    assert!(global.contains("active_target: second"), "{global}");
}

#[test]
fn target_use_on_already_active_is_a_polite_noop() {
    let dir = tempfile::tempdir().unwrap();
    seed_two_targets(dir.path());

    cli()
        .env("APPRAFTER_CONFIG_DIR", dir.path())
        .env("APPRAFTER_NO_PING", "1")
        .args(["target", "use", "first"])
        .assert()
        .success()
        .stdout(contains("was already the active target"));
}

#[test]
fn target_use_on_missing_target_surfaces_available_hint() {
    let dir = tempfile::tempdir().unwrap();
    seed_two_targets(dir.path());

    cli()
        .env("APPRAFTER_CONFIG_DIR", dir.path())
        .env("APPRAFTER_NO_PING", "1")
        .args(["target", "use", "ghost"])
        .assert()
        .failure()
        // Canonical TargetNotFound message includes `available: …`.
        .stderr(contains("target `ghost` not found"))
        .stderr(contains("first"))
        .stderr(contains("second"));
}

#[test]
fn target_show_with_no_args_renders_active_target_with_masked_token() {
    let dir = tempfile::tempdir().unwrap();
    seed_two_targets(dir.path());

    cli()
        .env("APPRAFTER_CONFIG_DIR", dir.path())
        .env("APPRAFTER_NO_PING", "1")
        .args(["target", "show"])
        .assert()
        .success()
        .stdout(contains("Target: first (active)"))
        .stdout(contains("hetzner-cloud"))
        .stdout(contains("nbg1"))
        .stdout(contains("Hetzner token: set"))
        // Never echo the actual token body in show output.
        .stdout(predicates::str::contains(synthetic_hetzner_token()).not());
}

#[test]
fn target_show_with_explicit_name_renders_named_target_without_active_marker() {
    let dir = tempfile::tempdir().unwrap();
    seed_two_targets(dir.path());

    cli()
        .env("APPRAFTER_CONFIG_DIR", dir.path())
        .env("APPRAFTER_NO_PING", "1")
        .args(["target", "show", "second"])
        .assert()
        .success()
        .stdout(contains("Target: second"))
        .stdout(predicates::str::contains("(active)").not());
}

#[test]
fn target_show_on_empty_store_errors_with_onboarding_hint() {
    let dir = tempfile::tempdir().unwrap();
    cli()
        .env("APPRAFTER_CONFIG_DIR", dir.path())
        .env("APPRAFTER_NO_PING", "1")
        .args(["target", "show"])
        .assert()
        .failure()
        .stderr(contains("no active target"))
        .stderr(contains("apprafter target add"));
}

#[test]
fn target_rename_moves_files_and_updates_active_pointer() {
    let dir = tempfile::tempdir().unwrap();
    seed_two_targets(dir.path());

    cli()
        .env("APPRAFTER_CONFIG_DIR", dir.path())
        .env("APPRAFTER_NO_PING", "1")
        .args(["target", "rename", "first", "primary"])
        .assert()
        .success()
        .stdout(contains("first"))
        .stdout(contains("primary"))
        .stdout(contains("active pointer updated"));

    assert!(!dir.path().join("targets/first").exists());
    assert!(dir.path().join("targets/primary/config.yaml").exists());

    let global = std::fs::read_to_string(dir.path().join("config.yaml")).unwrap();
    assert!(global.contains("active_target: primary"), "{global}");
}

#[test]
fn target_rename_non_active_target_leaves_active_pointer_alone() {
    let dir = tempfile::tempdir().unwrap();
    seed_two_targets(dir.path());

    cli()
        .env("APPRAFTER_CONFIG_DIR", dir.path())
        .env("APPRAFTER_NO_PING", "1")
        .args(["target", "rename", "second", "backup"])
        .assert()
        .success()
        // No "active pointer updated" message when the renamed
        // target wasn't active.
        .stdout(predicates::str::contains("active pointer updated").not());

    let global = std::fs::read_to_string(dir.path().join("config.yaml")).unwrap();
    assert!(global.contains("active_target: first"), "{global}");
}

#[test]
fn target_rename_refuses_when_destination_exists() {
    let dir = tempfile::tempdir().unwrap();
    seed_two_targets(dir.path());

    cli()
        .env("APPRAFTER_CONFIG_DIR", dir.path())
        .env("APPRAFTER_NO_PING", "1")
        .args(["target", "rename", "first", "second"])
        .assert()
        .failure()
        .stderr(contains("already exists"));

    // Both targets remain intact.
    assert!(dir.path().join("targets/first/config.yaml").exists());
    assert!(dir.path().join("targets/second/config.yaml").exists());
}

#[test]
fn target_rename_rejects_invalid_destination_name() {
    let dir = tempfile::tempdir().unwrap();
    seed_two_targets(dir.path());

    cli()
        .env("APPRAFTER_CONFIG_DIR", dir.path())
        .env("APPRAFTER_NO_PING", "1")
        .args(["target", "rename", "first", "with space"])
        .assert()
        .failure()
        .stderr(contains("is invalid"));
}

#[test]
fn target_rename_refuses_identical_source_and_destination() {
    let dir = tempfile::tempdir().unwrap();
    seed_two_targets(dir.path());

    cli()
        .env("APPRAFTER_CONFIG_DIR", dir.path())
        .env("APPRAFTER_NO_PING", "1")
        .args(["target", "rename", "first", "first"])
        .assert()
        .failure()
        .stderr(contains("identical"));
}

#[test]
fn target_remove_with_yes_flag_deletes_and_reassigns_active_alphabetically() {
    let dir = tempfile::tempdir().unwrap();
    seed_two_targets(dir.path());

    // Remove the active target (`first`); active should move to
    // the alphabetically next remaining target (`second`).
    cli()
        .env("APPRAFTER_CONFIG_DIR", dir.path())
        .env("APPRAFTER_NO_PING", "1")
        .args(["target", "remove", "first", "--yes"])
        .assert()
        .success()
        .stdout(contains("removed"))
        .stdout(contains("active switched to `second`"));

    assert!(!dir.path().join("targets/first").exists());
    let global = std::fs::read_to_string(dir.path().join("config.yaml")).unwrap();
    assert!(global.contains("active_target: second"), "{global}");
}

#[test]
fn target_remove_last_target_clears_active_pointer() {
    let dir = tempfile::tempdir().unwrap();
    let token = synthetic_hetzner_token();
    cli()
        .env("APPRAFTER_CONFIG_DIR", dir.path())
        .env("APPRAFTER_NO_PING", "1")
        .env_remove("HCLOUD_TOKEN")
        .args([
            "target",
            "add",
            "only-one",
            "--provider",
            "hetzner-cloud",
            "--token",
            &token,
        ])
        .assert()
        .success();
    cli()
        .env("APPRAFTER_CONFIG_DIR", dir.path())
        .env("APPRAFTER_NO_PING", "1")
        .args(["target", "remove", "only-one", "--yes"])
        .assert()
        .success()
        .stdout(contains("active pointer cleared"));

    // Global config file dropped; next `target add` should go
    // through the fresh-store path again.
    assert!(!dir.path().join("config.yaml").exists());
}

#[test]
fn target_remove_non_active_target_keeps_active_pointer_intact() {
    let dir = tempfile::tempdir().unwrap();
    seed_two_targets(dir.path());

    cli()
        .env("APPRAFTER_CONFIG_DIR", dir.path())
        .env("APPRAFTER_NO_PING", "1")
        .args(["target", "remove", "second", "--yes"])
        .assert()
        .success();

    let global = std::fs::read_to_string(dir.path().join("config.yaml")).unwrap();
    assert!(global.contains("active_target: first"), "{global}");
}

#[test]
fn target_remove_non_interactive_without_yes_refuses() {
    let dir = tempfile::tempdir().unwrap();
    seed_two_targets(dir.path());

    cli()
        .env("APPRAFTER_CONFIG_DIR", dir.path())
        .env("APPRAFTER_NO_PING", "1")
        .args(["target", "remove", "second"])
        .assert()
        .failure()
        .stderr(contains("non-interactive"))
        .stderr(contains("--yes"));

    // Target intact.
    assert!(dir.path().join("targets/second/config.yaml").exists());
}

#[test]
fn target_remove_on_missing_target_surfaces_available_hint() {
    let dir = tempfile::tempdir().unwrap();
    seed_two_targets(dir.path());

    cli()
        .env("APPRAFTER_CONFIG_DIR", dir.path())
        .env("APPRAFTER_NO_PING", "1")
        .args(["target", "remove", "ghost", "--yes"])
        .assert()
        .failure()
        .stderr(contains("target `ghost` not found"));
}

#[test]
fn target_alias_t_subcommand_resolves_to_target() {
    // Smoke for the `apprafter t add …` alias declared in clap.
    let dir = tempfile::tempdir().unwrap();
    cli()
        .env("APPRAFTER_CONFIG_DIR", dir.path())
        .env("APPRAFTER_NO_PING", "1")
        .env_remove("HCLOUD_TOKEN")
        .args([
            "t",
            "add",
            "via-alias",
            "--provider",
            "hetzner-cloud",
            "--token",
            &synthetic_hetzner_token(),
        ])
        .assert()
        .success();
    assert!(dir.path().join("targets/via-alias/config.yaml").exists());
}

// ---------------------------------------------------------------
// Provider API ping (Track A.4a / v0.1.75)
//
// These tests redirect the Hetzner client at a `mockito::Server`
// via APPRAFTER_HCLOUD_BASE_URL (honoured by hcloud_base_url()).
// They MUST NOT set APPRAFTER_NO_PING — the whole point is to
// exercise the ping path end-to-end.
// ---------------------------------------------------------------

const LOCATIONS_OK: &str = r#"{
    "locations": [
        {
            "id": 1,
            "name": "fsn1",
            "description": "Falkenstein DC Park 1",
            "country": "DE",
            "city": "Falkenstein",
            "network_zone": "eu-central"
        }
    ]
}"#;

#[test]
fn target_add_pings_provider_by_default_and_announces_verified_status() {
    let mut server = mockito::Server::new();
    let _m = server
        .mock("GET", "/v1/locations")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(LOCATIONS_OK)
        .expect(1)
        .create();

    let dir = tempfile::tempdir().unwrap();
    cli()
        .env("APPRAFTER_CONFIG_DIR", dir.path())
        .env("APPRAFTER_HCLOUD_BASE_URL", server.url())
        .env_remove("HCLOUD_TOKEN")
        .env_remove("APPRAFTER_NO_PING")
        .args([
            "target",
            "add",
            "primary",
            "--provider",
            "hetzner-cloud",
            "--token",
            &synthetic_hetzner_token(),
        ])
        .assert()
        .success()
        .stdout(contains("verified against Hetzner Cloud"));

    // Mock expectation is asserted on Drop via `expect(1)` — the
    // ping must have been called exactly once.
    drop(_m);
    drop(server);

    assert!(dir.path().join("targets/primary/config.yaml").exists());
}

#[test]
fn target_add_surfaces_typed_error_on_hetzner_401() {
    let mut server = mockito::Server::new();
    let _m = server
        .mock("GET", "/v1/locations")
        .with_status(401)
        .with_header("content-type", "application/json")
        .with_body(r#"{"error":{"code":"unauthorized","message":"unable to authenticate"}}"#)
        .expect(1)
        .create();

    let dir = tempfile::tempdir().unwrap();
    cli()
        .env("APPRAFTER_CONFIG_DIR", dir.path())
        .env("APPRAFTER_HCLOUD_BASE_URL", server.url())
        .env_remove("HCLOUD_TOKEN")
        .env_remove("APPRAFTER_NO_PING")
        .args([
            "target",
            "add",
            "bad-token",
            "--provider",
            "hetzner-cloud",
            "--token",
            &synthetic_hetzner_token(),
        ])
        .assert()
        .failure()
        // v0.1.87 — 401 now surfaces as the typed
        // `ProviderTokenRejected` variant. Miette renders both the
        // top-level summary AND the underlying Hetzner API envelope
        // via the `#[diagnostic_source]` cause chain. Pin both:
        // diagnostic code, rotation hint from help, raw status from
        // chained cause.
        .stderr(contains("apprafter::target::token_rejected"))
        .stderr(contains("rejected the supplied token"))
        .stderr(contains("--renew"))
        // The chained `Hetzner` cause's diagnostic code is the
        // signal that the cause chain rendered — miette line-wraps
        // the raw error message at 80 cols so a `(status 401)`
        // substring isn't reliable, but the code string itself
        // never wraps.
        .stderr(contains("apprafter::provider::hetzner_api_error"));

    // Failed ping must NOT save the target. The user reruns with a
    // good token rather than getting half-state on disk.
    assert!(
        !dir.path().join("targets/bad-token").exists(),
        "target dir must not exist after a failed ping"
    );
}

#[test]
fn target_add_surfaces_helpful_error_when_api_is_unreachable() {
    // No mockito server — point at a known-closed port so ureq
    // returns a transport-class error.
    let dir = tempfile::tempdir().unwrap();
    cli()
        .env("APPRAFTER_CONFIG_DIR", dir.path())
        .env("APPRAFTER_HCLOUD_BASE_URL", "http://127.0.0.1:1")
        .env_remove("HCLOUD_TOKEN")
        .env_remove("APPRAFTER_NO_PING")
        .args([
            "target",
            "add",
            "offline",
            "--provider",
            "hetzner-cloud",
            "--token",
            &synthetic_hetzner_token(),
        ])
        .assert()
        .failure()
        // v0.1.87 — non-401 transport errors surface as the typed
        // `ProviderApiUnreachable` variant. The help block mentions
        // `apprafter doctor` and `--no-ping` as next steps. The
        // chained cause carries the original transport message.
        .stderr(contains("apprafter::target::provider_unreachable"))
        .stderr(contains("API was unreachable"))
        .stderr(contains("--no-ping"))
        .stderr(contains("apprafter doctor"));

    assert!(!dir.path().join("targets/offline").exists());
}

#[test]
fn target_add_no_ping_flag_skips_validator_and_announces_unverified() {
    // No mockito server set up — would fail any actual ping. The
    // --no-ping flag must short-circuit before that.
    let dir = tempfile::tempdir().unwrap();
    cli()
        .env("APPRAFTER_CONFIG_DIR", dir.path())
        .env("APPRAFTER_HCLOUD_BASE_URL", "http://127.0.0.1:1")
        .env_remove("HCLOUD_TOKEN")
        .env_remove("APPRAFTER_NO_PING")
        .args([
            "target",
            "add",
            "ci",
            "--provider",
            "hetzner-cloud",
            "--token",
            &synthetic_hetzner_token(),
            "--no-ping",
        ])
        .assert()
        .success()
        .stdout(contains("token NOT verified"));

    assert!(dir.path().join("targets/ci/config.yaml").exists());
}

#[test]
fn target_add_no_ping_env_var_also_skips_validator() {
    // Equivalent to the --no-ping flag via env-var binding.
    let dir = tempfile::tempdir().unwrap();
    cli()
        .env("APPRAFTER_CONFIG_DIR", dir.path())
        .env("APPRAFTER_HCLOUD_BASE_URL", "http://127.0.0.1:1")
        .env("APPRAFTER_NO_PING", "1")
        .env_remove("HCLOUD_TOKEN")
        .args([
            "target",
            "add",
            "ci-env",
            "--provider",
            "hetzner-cloud",
            "--token",
            &synthetic_hetzner_token(),
        ])
        .assert()
        .success()
        .stdout(contains("token NOT verified"));
}
