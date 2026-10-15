// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Integration tests for `apprafter export` / `apprafter backup` /
//! `apprafter restore` — help text and argument parsing without a cluster
//! (offline-safe). 2.6d T10.

mod common;

use common::spawn::Spawn;

use assert_cmd::Command;
use predicates::prelude::PredicateBooleanExt;
use predicates::str::contains;

fn cli(sp: &Spawn) -> Command {
    sp.cmd(&[])
}

#[test]
fn export_backup_restore_help_lists_commands() {
    let sp = Spawn::new();
    cli(&sp)
        .args(["export", "--help"])
        .assert()
        .success()
        .stdout(contains("--namespace"));
    cli(&sp)
        .args(["backup", "--help"])
        .assert()
        .success()
        .stdout(contains("create"))
        .stdout(contains("list"));
    cli(&sp)
        .args(["restore", "--help"])
        .assert()
        .success()
        .stdout(contains("--reprovision"))
        .stdout(contains("--data-only"));
}

#[test]
fn export_help_shows_select_and_out_flags() {
    let sp = Spawn::new();
    cli(&sp)
        .args(["export", "--help"])
        .assert()
        .success()
        .stdout(contains("--select"))
        .stdout(contains("--out"));
}

#[test]
fn backup_create_help_shows_repo_and_passphrase() {
    let sp = Spawn::new();
    cli(&sp)
        .args(["backup", "create", "--help"])
        .assert()
        .success()
        .stdout(contains("--repo"))
        .stdout(contains("--passphrase"));
}

#[test]
fn backup_list_help_shows_repo_flag() {
    let sp = Spawn::new();
    cli(&sp)
        .args(["backup", "list", "--help"])
        .assert()
        .success()
        .stdout(contains("--repo"));
}

#[test]
fn restore_requires_repo_positional() {
    // `apprafter restore` without a repo positional arg should fail
    // (missing required argument).
    let sp = Spawn::new();
    cli(&sp).args(["restore"]).assert().failure();
}

#[test]
fn restore_reprovision_is_no_longer_a_t13_stub() {
    // 2.6d T13 wired `--reprovision`. It must NO LONGER fail with the old
    // "lands in 2.6d T13" stub: a REAL precondition fires instead, proving the stub is gone
    // and the real flow is entered. In the sandbox it is the tool preflight (its `PATH` holds
    // no restic; 2.22a D11 runs the preflight before the mandatory-passphrase gate, which fires
    // when the tools are there: `restore_into_running_errors_for_a_real_reason_not_a_stub`).
    let sp = Spawn::new();
    cli(&sp)
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
    let sp = Spawn::new();
    cli(&sp)
        .args(["restore", "/tmp/fake-repo", "--reprovision", "--data-only"])
        .assert()
        .failure()
        .stderr(contains("mutually exclusive"));
}

#[test]
fn restore_into_running_errors_for_a_real_reason_not_a_stub() {
    // Post-T11, `apprafter restore <repo>` drives the real restore-into-running flow, so a REAL
    // precondition must fire rather than the removed "not yet implemented" stub. 2.22a moved
    // WHICH one fires: the binary check runs before the credential gate (D11). The child's
    // `PATH` is the sandbox's, so the test sets up each branch instead of reading one off the
    // machine running it.

    // Binary missing → the preflight fires FIRST, and the command does not ask for a passphrase
    // it could never have used.
    let sp = Spawn::new();
    sp.cmd(&["restore", "/tmp/fake-repo"])
        .assert()
        .failure()
        .stderr(contains("not yet implemented").not())
        .stderr(contains("restic"))
        .stderr(contains("not on PATH"))
        .stderr(contains("passphrase").not());

    // Binaries present → the credential gate is the first thing that can fail. `restore`
    // preflights restic AND kubectl (`tools_for_restore(false)`, commands/restore.rs), and the
    // preflight reports the first one missing: both stand in, or kubectl's absence fires first.
    let tools =
        common::stand_in::tool_stand_ins([&cli_core::tools::RESTIC, &cli_core::tools::KUBECTL]);
    let sp = Spawn::new().with_path(tools.path().to_path_buf());
    sp.cmd(&["restore", "/tmp/fake-repo"])
        .assert()
        .failure()
        .stderr(contains("not yet implemented").not())
        .stderr(contains("passphrase"));
}
