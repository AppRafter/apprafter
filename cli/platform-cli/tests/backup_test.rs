// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Integration tests for `apprafter export` / `apprafter backup` /
//! `apprafter restore` — help text and argument parsing without a cluster
//! (offline-safe). 2.6d T10.

use assert_cmd::Command;
use predicates::prelude::PredicateBooleanExt;
use predicates::str::contains;

fn cli() -> Command {
    Command::cargo_bin("apprafter").unwrap()
}

#[test]
fn export_backup_restore_help_lists_commands() {
    cli()
        .args(["export", "--help"])
        .assert()
        .success()
        .stdout(contains("--namespace"));
    cli()
        .args(["backup", "--help"])
        .assert()
        .success()
        .stdout(contains("create"))
        .stdout(contains("list"));
    cli()
        .args(["restore", "--help"])
        .assert()
        .success()
        .stdout(contains("--reprovision"))
        .stdout(contains("--data-only"));
}

#[test]
fn export_help_shows_select_and_out_flags() {
    cli()
        .args(["export", "--help"])
        .assert()
        .success()
        .stdout(contains("--select"))
        .stdout(contains("--out"));
}

#[test]
fn backup_create_help_shows_repo_and_passphrase() {
    cli()
        .args(["backup", "create", "--help"])
        .assert()
        .success()
        .stdout(contains("--repo"))
        .stdout(contains("--passphrase"));
}

#[test]
fn backup_list_help_shows_repo_flag() {
    cli()
        .args(["backup", "list", "--help"])
        .assert()
        .success()
        .stdout(contains("--repo"));
}

#[test]
fn restore_requires_repo_positional() {
    // `apprafter restore` without a repo positional arg should fail
    // (missing required argument).
    cli().args(["restore"]).assert().failure();
}

#[test]
fn restore_reprovision_is_no_longer_a_t13_stub() {
    // 2.6d T13 wired `--reprovision`. It must NO LONGER fail with the old
    // "lands in 2.6d T13" stub; with no passphrase + no TTY (assert_cmd has no
    // terminal) the mandatory-passphrase gate fires first — a REAL precondition,
    // proving the stub is gone and the real flow is entered.
    cli()
        .args(["restore", "/tmp/fake-repo", "--reprovision"])
        .assert()
        .failure()
        .stderr(contains("T13").not());
}

#[test]
fn restore_reprovision_and_data_only_are_mutually_exclusive() {
    // `--reprovision` (rebuild the whole cluster) and `--data-only` (reload data
    // into a running one) are contradictory; the combo must be rejected up front
    // with a clear message, never silently degrade to an unresolved-kubeconfig.
    cli()
        .args(["restore", "/tmp/fake-repo", "--reprovision", "--data-only"])
        .assert()
        .failure()
        .stderr(contains("mutually exclusive"));
}

#[test]
fn restore_into_running_errors_for_a_real_reason_not_a_stub() {
    // Post-T11, `apprafter restore <repo>` drives the real
    // restore-into-running flow, so a REAL precondition must fire rather
    // than the removed "not yet implemented" stub.
    //
    // 2.22a moved WHICH precondition fires, deliberately: the binary
    // check now runs before the credential gate (D11). Both are real, so
    // the stub assertion is unchanged — but rather than accepting either
    // message and weakening the test, it now asserts the ORDERING, which
    // is the thing 2.22a changed. The environment decides which branch
    // applies, so the test asks the same question the code does.
    let restic_present = cli_core::tools::preflight_tool(&cli_core::tools::RESTIC, "test").is_ok();

    let assertion = cli()
        .args(["restore", "/tmp/fake-repo"])
        .env_remove("RESTIC_PASSWORD")
        .assert()
        .failure()
        .stderr(contains("not yet implemented").not());

    if restic_present {
        // Binary available → the credential gate is the first thing that
        // can fail, exactly as before.
        assertion.stderr(contains("passphrase"));
    } else {
        // Binary missing → the preflight fires FIRST. The second half is
        // the actual fix: the command must not have asked for a
        // passphrase it could never have used. Without this, moving the
        // preflight back down would still pass.
        assertion
            .stderr(contains("restic"))
            .stderr(contains("not on PATH"))
            .stderr(contains("passphrase").not());
    }
}
