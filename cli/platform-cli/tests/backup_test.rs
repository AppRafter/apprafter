// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Integration tests for `apprafter export` / `apprafter backup` /
//! `apprafter restore` — help text and argument parsing without a cluster
//! (offline-safe). 2.6d T10.

use assert_cmd::Command;
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
fn restore_stub_errors_clearly() {
    // Invoking `apprafter restore` with a repo should fail with the
    // T11/T13 stub message (not a panic / clap parse error).
    cli()
        .args(["restore", "/tmp/fake-repo"])
        .assert()
        .failure()
        .stderr(contains("T11/T13"));
}
