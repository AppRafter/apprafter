// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Integration tests for `apprafter volume` — help text and
//! argument parsing without a cluster (offline-safe).

use assert_cmd::Command;
use predicates::prelude::PredicateBooleanExt;
use predicates::str::contains;

fn cli() -> Command {
    Command::cargo_bin("apprafter").unwrap()
}

#[test]
fn volume_help_lists_subcommands() {
    cli()
        .args(["volume", "--help"])
        .assert()
        .success()
        .stdout(contains("create"))
        .stdout(contains("rm"))
        .stdout(contains("status"));
}

#[test]
fn volume_create_requires_size_flag() {
    cli()
        .args(["volume", "create", "my-vol"])
        .assert()
        .failure()
        .stderr(contains("--size"));
}

#[test]
fn volume_list_help_shows_namespace_flag() {
    cli()
        .args(["volume", "list", "--help"])
        .assert()
        .success()
        .stdout(contains("namespace"));
}

#[test]
fn volume_rm_help_shows_yes_flag() {
    cli()
        .args(["volume", "rm", "--help"])
        .assert()
        .success()
        .stdout(contains("yes"));
}

#[test]
fn volume_status_help_shows_name() {
    cli()
        .args(["volume", "status", "--help"])
        .assert()
        .success()
        .stdout(contains("name").or(contains("NAME")));
}
