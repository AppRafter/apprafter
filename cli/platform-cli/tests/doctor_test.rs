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

mod common;

/// A `PATH` holding nothing but a stand-in for every tool `doctor` probes.
///
/// A clean run needs the required tools present, so a test that asserts one
/// passed or failed on what the CI image happened to ship: the macOS runners
/// carry no `kubectl`, and every happy-path test here failed on them. What a
/// stand-in is, per platform, is `common::stand_in::tool_stand_ins`'s to say.
fn tools_on_path() -> tempfile::TempDir {
    common::stand_in::tool_stand_ins(cli_core::tools::ALL)
}

/// Decision 2 of the D.3c review: each stand-in answers its tool's version call as the real
/// tool does — a version line and exit 0 (`ssh -V` on stderr) — and anything else with a usage
/// error. The resolver reports a tool that exits non-zero on its version call as having no
/// version (whatever it printed), so a stand-in that did would test a broken tool instead.
/// Unix only: on Windows the stand-ins stay hard links of `apprafter` (GOTCHA-66).
#[cfg(unix)]
#[test]
fn the_stand_ins_answer_their_version_call_like_the_real_tools() {
    let tools = tools_on_path();
    for tool in cli_core::tools::ALL {
        let bin = tools.path().join(tool.name);
        let out = std::process::Command::new(&bin)
            .args(tool.version_args)
            .output()
            .unwrap();
        assert!(out.status.success(), "`{}`: {out:?}", tool.name);
        let answer = if tool.name == "ssh" {
            &out.stderr
        } else {
            &out.stdout
        };
        assert_eq!(
            String::from_utf8_lossy(answer),
            format!("{} stand-in\n", tool.name)
        );
        let wrong = std::process::Command::new(&bin)
            .arg("--no-such-flag")
            .output()
            .unwrap();
        assert_eq!(wrong.status.code(), Some(2), "`{}`: {wrong:?}", tool.name);
    }
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
    for tool in ["kubectl", "helm", "restic", "git", "ssh", "cue"] {
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

    let tools = tools_on_path();
    cli()
        .env("APPRAFTER_CONFIG_DIR", dir.path())
        .env("APPRAFTER_NO_PING", "1")
        .env("PATH", tools.path())
        // Pins the production host: the DNS row names the host of the
        // configured API base, so an inherited override would move it.
        .env_remove("APPRAFTER_HCLOUD_BASE_URL")
        .arg("doctor")
        .assert()
        .success()
        // Target section.
        .stdout(contains("Checking target `default`"))
        .stdout(contains("Config file readable"))
        .stdout(contains("Credentials file"))
        .stdout(contains("Provider `hetzner-cloud` supported"))
        .stdout(contains("Token format valid"))
        // R8: --no-ping means the verification step did not run: a dash and
        // a detail that names no CLI flag, not a WARN.
        .stdout(contains(
            "– Token verified against provider API (not requested)",
        ))
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
fn doctor_resolves_the_host_of_the_configured_api_base() {
    let dir = tempfile::tempdir().unwrap();
    cli()
        .env("APPRAFTER_CONFIG_DIR", dir.path())
        .env("APPRAFTER_SKIP_STARTUP_CHECKS", "1")
        .env("APPRAFTER_HCLOUD_BASE_URL", "http://127.0.0.1:1")
        .env("KUBECONFIG", "/nonexistent")
        .arg("doctor")
        .assert()
        .stdout(contains("DNS resolves `127.0.0.1`"))
        .stdout(contains("api.hetzner.cloud").not());
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

    let tools = tools_on_path();
    cli()
        .env("APPRAFTER_CONFIG_DIR", dir.path())
        .env("APPRAFTER_NO_PING", "1")
        .env("PATH", tools.path())
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

    let tools = tools_on_path();
    cli()
        .env("APPRAFTER_CONFIG_DIR", dir.path())
        .env("APPRAFTER_NO_PING", "1")
        .env("PATH", tools.path())
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
    // Happy path: no FAILs and no warnings. With every tool present and the
    // token check skipped (`--no-ping`, not counted), the summary is "All
    // good" and contains neither "FAIL" nor "warning".
    let dir = tempfile::tempdir().unwrap();
    let key_dir = tempfile::tempdir().unwrap();
    let key_path = key_dir.path().join("id_ed25519.pub");
    std::fs::write(&key_path, "ssh-ed25519 AAAA test@host").unwrap();
    seed_target_with_ssh(dir.path(), Some(&key_path));

    let tools = tools_on_path();
    cli()
        .env("APPRAFTER_CONFIG_DIR", dir.path())
        .env("APPRAFTER_NO_PING", "1")
        .env("PATH", tools.path())
        // The DNS row then resolves `127.0.0.1`, so the run is hermetic.
        .env("APPRAFTER_HCLOUD_BASE_URL", "http://127.0.0.1:1")
        .arg("doctor")
        .assert()
        .success()
        .stdout(predicates::str::contains(" FAIL").not())
        // R8: the token check `--no-ping` skipped is not a warning, so a healthy target is
        // "All good" rather than "review warnings".
        .stdout(contains("warning").not())
        .stdout(contains("All good"));
}
