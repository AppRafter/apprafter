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
    // apply / cluster_bootstrap / kubeconfig. Point the config dir
    // at a fresh tempdir so the test is not coupled to the
    // developer's real `~/.config/apprafter/` target store.
    let dir = tempfile::tempdir().unwrap();
    let config_dir = tempfile::tempdir().unwrap();
    cli()
        .current_dir(dir.path())
        .env("APPRAFTER_CONFIG_DIR", config_dir.path())
        .env_remove("HCLOUD_TOKEN")
        .env_remove("APPRAFTER_HCLOUD_BASE_URL")
        .args(["bootstrap-all", "--dry-run"])
        .assert()
        .success()
        .stdout(contains("DRY RUN"))
        .stdout(contains("[1/3] apply"))
        .stdout(contains("[2/3] kubeconfig"))
        .stdout(contains("[3/3] cluster-bootstrap"))
        .stdout(contains("Phases:"));
}

#[test]
fn bootstrap_all_dry_run_with_empty_store_prints_onboarding_hint() {
    // Empty store → no active target name resolvable. The plan
    // must still print, but with a clear "no active target" hint
    // pointing at `apprafter target add`.
    let dir = tempfile::tempdir().unwrap();
    let config_dir = tempfile::tempdir().unwrap();
    cli()
        .current_dir(dir.path())
        .env("APPRAFTER_CONFIG_DIR", config_dir.path())
        .env_remove("HCLOUD_TOKEN")
        .args(["bootstrap-all", "--dry-run"])
        .assert()
        .success()
        .stdout(contains("no active target"))
        .stdout(contains("apprafter target add"));
}

#[test]
fn bootstrap_all_dry_run_with_target_override_labels_it_clearly() {
    // `--target work` overrides the resolution chain. Even with
    // no matching target file, the dry-run plan must echo the
    // override label.
    let dir = tempfile::tempdir().unwrap();
    let config_dir = tempfile::tempdir().unwrap();
    cli()
        .current_dir(dir.path())
        .env("APPRAFTER_CONFIG_DIR", config_dir.path())
        .env_remove("HCLOUD_TOKEN")
        .args(["bootstrap-all", "--dry-run", "--target", "work"])
        .assert()
        .success()
        .stdout(contains("Target: work"))
        .stdout(contains("via --target override"));
}

#[test]
fn bootstrap_all_dry_run_with_active_target_resolves_name_and_config() {
    // Seed a real target via `apprafter target add` so the dry-run
    // plan can pick up its name + provider/region/tier from disk.
    let dir = tempfile::tempdir().unwrap();
    let config_dir = tempfile::tempdir().unwrap();
    cli()
        .current_dir(dir.path())
        .env("APPRAFTER_CONFIG_DIR", config_dir.path())
        .env_remove("HCLOUD_TOKEN")
        .args([
            "target",
            "add",
            "myprod",
            "--provider",
            "hetzner-cloud",
            "--token",
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            "--region",
            "fsn1",
            "--tier",
            "team",
            "--no-ping",
            "--no-interactive",
        ])
        .assert()
        .success();

    cli()
        .current_dir(dir.path())
        .env("APPRAFTER_CONFIG_DIR", config_dir.path())
        .env_remove("HCLOUD_TOKEN")
        .args(["bootstrap-all", "--dry-run"])
        .assert()
        .success()
        // Active target name + status surfaces in the header.
        .stdout(contains("Target: myprod"))
        .stdout(contains("(active)"))
        // Provider / region / tier from config.yaml propagate.
        .stdout(contains("Provider:"))
        .stdout(contains("hetzner-cloud"))
        .stdout(contains("Region:"))
        .stdout(contains("fsn1"))
        .stdout(contains("Tier:"))
        .stdout(contains("team"));
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
