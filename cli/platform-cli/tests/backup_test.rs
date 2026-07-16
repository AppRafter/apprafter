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
    // Post-T11, `apprafter restore <repo>` drives the real restore-into-running
    // flow. With no RESTIC_PASSWORD set and no TTY (assert_cmd has no terminal),
    // the mandatory-passphrase gate fires first — a REAL precondition error, not
    // the removed "not yet implemented" stub.
    cli()
        .args(["restore", "/tmp/fake-repo"])
        .env_remove("RESTIC_PASSWORD")
        .assert()
        .failure()
        .stderr(contains("passphrase"))
        .stderr(contains("not yet implemented").not());
}
