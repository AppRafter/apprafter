// SPDX-License-Identifier: FSL-1.1-MIT
//! Integration tests for `apprafter bootstrap-all`.
//!
//! The wet path (`apply` → `kubeconfig` poll → `cluster-bootstrap`)
//! is exercised by `e2e/mvp.sh` against a live Hetzner Cloud token;
//! these tests cover the dry-run plan output and the clap surface
//! contract.

use assert_cmd::Command;
use predicates::prelude::PredicateBooleanExt;
use predicates::str::contains;

fn cli() -> Command {
    Command::cargo_bin("apprafter").unwrap()
}

#[test]
fn bootstrap_all_dry_run_prints_three_phase_plan_without_provider_calls() {
    // No state file, no HCLOUD_TOKEN, no APPRAFTER_HCLOUD_BASE_URL —
    // a dry-run must succeed regardless because it never reaches
    // apply / cluster_bootstrap / kubeconfig.
    let dir = tempfile::tempdir().unwrap();
    cli()
        .current_dir(dir.path())
        .env_remove("HCLOUD_TOKEN")
        .env_remove("APPRAFTER_HCLOUD_BASE_URL")
        .args(["bootstrap-all", "--dry-run"])
        .assert()
        .success()
        .stdout(contains("DRY RUN"))
        .stdout(contains("[1/3] apply"))
        .stdout(contains("[2/3] kubeconfig"))
        .stdout(contains("[3/3] cluster-bootstrap"));
}

#[test]
fn bootstrap_all_dry_run_echoes_target_override() {
    let dir = tempfile::tempdir().unwrap();
    cli()
        .current_dir(dir.path())
        .env_remove("HCLOUD_TOKEN")
        .args(["bootstrap-all", "--dry-run", "--target", "work"])
        .assert()
        .success()
        .stdout(contains("--target work"));
}

#[test]
fn bootstrap_all_help_documents_dry_run_and_target_flags() {
    cli()
        .args(["bootstrap-all", "--help"])
        .assert()
        .success()
        .stdout(contains("--dry-run"))
        .stdout(contains("--target"));
}

#[test]
fn bootstrap_all_rejects_unknown_flag() {
    cli()
        .args(["bootstrap-all", "--no-such-flag"])
        .assert()
        .failure()
        .stderr(contains("unexpected argument").or(contains("error")));
}
