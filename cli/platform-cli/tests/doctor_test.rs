// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Integration tests for `apprafter doctor` (Track A.7 /
//! v0.1.81).
//!
//! Each test points the target store at a fresh tempdir and uses
//! `APPRAFTER_NO_PING=1` to keep the run offline (the API ping
//! path itself is exercised by the `whoami_auth_test.rs`
//! mockito-driven scenarios already; doctor reuses the same
//! `HetznerCloudValidator` plumbing).

use assert_cmd::Command;
use predicates::prelude::PredicateBooleanExt;
use predicates::str::contains;

fn cli() -> Command {
    Command::cargo_bin("apprafter").unwrap()
}

fn synthetic_hetzner_token() -> String {
    "a".repeat(64)
}

fn seed_target_with_ssh(dir: &std::path::Path, ssh_key: Option<&std::path::Path>) {
    let token = synthetic_hetzner_token();
    let mut args: Vec<String> = vec![
        "target".into(),
        "add".into(),
        "default".into(),
        "--provider".into(),
        "hetzner-cloud".into(),
        "--token".into(),
        token,
        "--region".into(),
        "nbg1".into(),
        "--tier".into(),
        "solo".into(),
    ];
    if let Some(k) = ssh_key {
        args.push("--ssh-key".into());
        args.push(k.to_string_lossy().into_owned());
    }
    cli()
        .env("APPRAFTER_CONFIG_DIR", dir)
        .env("APPRAFTER_NO_PING", "1")
        .env_remove("HCLOUD_TOKEN")
        .args(args)
        .assert()
        .success();
}

#[test]
fn doctor_on_empty_store_still_reports_the_environment() {
    // D11 / 2.22a. This test previously asserted `.failure()` with "no
    // active target" on stderr — the exact behaviour the audit called
    // wrong. `doctor`'s own docstring names first-run users as its
    // audience, and a first-run user has no target by definition, so
    // aborting there told them nothing about kubectl, helm, ssh or DNS:
    // precisely what they opened the command to learn.
    //
    // The exit status is deliberately NOT asserted. It now depends on
    // whether a REQUIRED tool is present on the machine running the
    // test, which is not a property of this code path. The exit-code
    // rule is unit-tested instead, in
    // `doctor::tests::a_missing_required_tool_makes_the_whole_run_fail`.
    let dir = tempfile::tempdir().unwrap();
    let out = cli()
        .env("APPRAFTER_CONFIG_DIR", dir.path())
        .env("APPRAFTER_NO_PING", "1")
        .arg("doctor")
        .output()
        .expect("doctor runs");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();

    // The environment half reached the reader.
    for tool in ["kubectl", "helm", "restic", "git", "ssh"] {
        assert!(
            stdout.contains(tool),
            "`{tool}` missing from the report a first-run user sees:\n{stdout}"
        );
    }
    // And they are told what to do next, as a check rather than an abort.
    assert!(stdout.contains("active target"), "{stdout}");
    assert!(stdout.contains("apprafter target add"), "{stdout}");
}

#[test]
fn doctor_renders_target_and_env_checks_with_summary() {
    let dir = tempfile::tempdir().unwrap();
    let key_dir = tempfile::tempdir().unwrap();
    let key_path = key_dir.path().join("id_ed25519.pub");
    std::fs::write(&key_path, "ssh-ed25519 AAAA test@host").unwrap();

    seed_target_with_ssh(dir.path(), Some(&key_path));

    cli()
        .env("APPRAFTER_CONFIG_DIR", dir.path())
        .env("APPRAFTER_NO_PING", "1")
        .arg("doctor")
        .assert()
        .success()
        // Target section.
        .stdout(contains("Checking target `default`"))
        .stdout(contains("Config file readable"))
        .stdout(contains("Credentials file"))
        .stdout(contains("Provider `hetzner-cloud` supported"))
        .stdout(contains("Token format valid"))
        // --no-ping means the verification step is WARN with
        // skipped detail, not PASS.
        .stdout(contains("Token verified against provider API"))
        .stdout(contains("skipped"))
        .stdout(contains("--no-ping"))
        .stdout(contains("SSH key readable"))
        .stdout(contains("ssh-ed25519"))
        // Environment section.
        .stdout(contains("Checking environment"))
        .stdout(contains("DNS resolves"))
        .stdout(contains("api.hetzner.cloud"))
        // Summary line includes both the target name and the
        // overall verdict.
        .stdout(contains("checks for target `default`"));
}

#[test]
fn doctor_target_flag_inspects_non_active_target() {
    let dir = tempfile::tempdir().unwrap();
    seed_target_with_ssh(dir.path(), None);
    // Add a second target that isn't active.
    cli()
        .env("APPRAFTER_CONFIG_DIR", dir.path())
        .env("APPRAFTER_NO_PING", "1")
        .env_remove("HCLOUD_TOKEN")
        .args([
            "target",
            "add",
            "secondary",
            "--provider",
            "hetzner-cloud",
            "--token",
            &synthetic_hetzner_token(),
        ])
        .assert()
        .success();

    cli()
        .env("APPRAFTER_CONFIG_DIR", dir.path())
        .env("APPRAFTER_NO_PING", "1")
        .args(["doctor", "--target", "secondary"])
        .assert()
        .success()
        .stdout(contains("Checking target `secondary`"));
}

#[test]
fn doctor_ssh_key_missing_path_fails_the_run_with_exit_1() {
    // Configure a target with an ssh-key path, then delete the
    // file so the doctor's ssh-key check trips into FAIL.
    let dir = tempfile::tempdir().unwrap();
    let key_dir = tempfile::tempdir().unwrap();
    let key_path = key_dir.path().join("id_ed25519.pub");
    std::fs::write(&key_path, "ssh-ed25519 AAAA test@host").unwrap();
    seed_target_with_ssh(dir.path(), Some(&key_path));

    // Wipe the key file — its path stays in the target config,
    // doctor must surface this as a FAIL (stale config).
    std::fs::remove_file(&key_path).unwrap();

    cli()
        .env("APPRAFTER_CONFIG_DIR", dir.path())
        .env("APPRAFTER_NO_PING", "1")
        .arg("doctor")
        .assert()
        .failure()
        .stdout(contains("SSH key readable"))
        .stdout(contains("file does not exist"))
        .stdout(contains("FAIL"));
}

#[test]
fn doctor_target_not_found_fails_with_available_hint() {
    let dir = tempfile::tempdir().unwrap();
    seed_target_with_ssh(dir.path(), None);

    cli()
        .env("APPRAFTER_CONFIG_DIR", dir.path())
        .env("APPRAFTER_NO_PING", "1")
        .args(["doctor", "--target", "ghost"])
        .assert()
        .failure()
        .stdout(contains("Target `ghost`"))
        .stdout(contains("available targets"))
        .stdout(contains("default"));
}

#[test]
fn doctor_summary_line_phrases_outcomes_clearly() {
    // Happy path: no FAILs. Summary should NOT contain "FAIL".
    let dir = tempfile::tempdir().unwrap();
    let key_dir = tempfile::tempdir().unwrap();
    let key_path = key_dir.path().join("id_ed25519.pub");
    std::fs::write(&key_path, "ssh-ed25519 AAAA test@host").unwrap();
    seed_target_with_ssh(dir.path(), Some(&key_path));

    cli()
        .env("APPRAFTER_CONFIG_DIR", dir.path())
        .env("APPRAFTER_NO_PING", "1")
        .arg("doctor")
        .assert()
        .success()
        .stdout(predicates::str::contains(" FAIL").not())
        // With --no-ping, the token-verified check is a WARN —
        // so the summary mentions "warning(s)".
        .stdout(contains("warning"));
}
