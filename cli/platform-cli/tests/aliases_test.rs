// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Integration tests for the subcommand aliases shipped in
//! v0.1.88 (Track A.11). Each test verifies that the alias
//! routes to the same handler as the canonical name — same
//! help output, same exit behaviour for the obvious failure
//! mode.

use assert_cmd::Command;
use predicates::str::contains;

fn cli() -> Command {
    Command::cargo_bin("apprafter").unwrap()
}

#[test]
fn target_ls_alias_routes_to_target_list() {
    // Empty store: both commands print the same onboarding hint.
    let cfg_dir = tempfile::tempdir().unwrap();
    let canonical = cli()
        .env("APPRAFTER_CONFIG_DIR", cfg_dir.path())
        .args(["target", "list"])
        .output()
        .unwrap();
    let alias = cli()
        .env("APPRAFTER_CONFIG_DIR", cfg_dir.path())
        .args(["target", "ls"])
        .output()
        .unwrap();
    assert_eq!(
        canonical.stdout, alias.stdout,
        "target list and target ls must produce identical stdout"
    );
    assert_eq!(canonical.status.success(), alias.status.success());
}

#[test]
fn target_rm_alias_routes_to_target_remove() {
    // Both reject "no such target" with the same error path.
    let cfg_dir = tempfile::tempdir().unwrap();
    cli()
        .env("APPRAFTER_CONFIG_DIR", cfg_dir.path())
        .args(["target", "rm", "ghost", "--yes"])
        .assert()
        .failure()
        .stderr(contains("apprafter::target::not_found"));
}

#[test]
fn target_info_alias_routes_to_target_show() {
    let cfg_dir = tempfile::tempdir().unwrap();
    cli()
        .env("APPRAFTER_CONFIG_DIR", cfg_dir.path())
        .args(["target", "info", "ghost"])
        .assert()
        .failure()
        .stderr(contains("apprafter::target::not_found"));
}

/// Seed an active target named `default` in the supplied config
/// dir so subsequent state-reading commands resolve past the
/// "no active target" gate and exercise the "no hetzner_cloud
/// state" path the aliases are supposed to share with their
/// canonical verbs. v0.1.154 layout: state location pivots on
/// the active target rather than cwd.
fn seed_active_target(cfg_dir: &std::path::Path) {
    cli()
        .env("APPRAFTER_CONFIG_DIR", cfg_dir)
        .env_remove("HCLOUD_TOKEN")
        .args([
            "target",
            "add",
            "default",
            "--provider",
            "hetzner-cloud",
            "--token",
            &"a".repeat(64),
            "--region",
            "nbg1",
            "--tier",
            "solo",
            "--no-ping",
            "--no-interactive",
        ])
        .assert()
        .success();
}

#[test]
fn kc_alias_routes_to_kubeconfig() {
    let dir = tempfile::tempdir().unwrap();
    let cfg_dir = tempfile::tempdir().unwrap();
    seed_active_target(cfg_dir.path());
    // `apprafter kc` against an empty per-target state surfaces
    // the same "no hetzner_cloud state" error as `apprafter
    // kubeconfig` — both routes go through the canonical handler.
    cli()
        .current_dir(dir.path())
        .env("APPRAFTER_CONFIG_DIR", cfg_dir.path())
        .arg("kc")
        .assert()
        .failure()
        .stderr(contains("hetzner_cloud"))
        .stderr(contains("apply"));
}

#[test]
fn cb_alias_routes_to_cluster_bootstrap() {
    let dir = tempfile::tempdir().unwrap();
    let cfg_dir = tempfile::tempdir().unwrap();
    seed_active_target(cfg_dir.path());
    cli()
        .current_dir(dir.path())
        .env("APPRAFTER_CONFIG_DIR", cfg_dir.path())
        .arg("cb")
        .assert()
        .failure()
        .stderr(contains("hetzner_cloud"))
        .stderr(contains("apply"));
}

#[test]
fn up_alias_routes_to_bootstrap_all_dry_run() {
    // `up --dry-run` MUST succeed in an empty cwd / empty store
    // exactly like `bootstrap-all --dry-run`. Pinning this proves
    // the alias hits the same dispatch branch in main.rs.
    let dir = tempfile::tempdir().unwrap();
    let cfg_dir = tempfile::tempdir().unwrap();
    cli()
        .current_dir(dir.path())
        .env("APPRAFTER_CONFIG_DIR", cfg_dir.path())
        .args(["up", "--dry-run"])
        .assert()
        .success()
        .stdout(contains("DRY RUN"))
        .stdout(contains("[1/3] apply"));
}

#[test]
fn t_alias_for_target_still_works_alongside_new_alias_chain() {
    // `apprafter t ls` chains the existing `t` → `target` alias
    // with the new `ls` → `list` alias on the subcommand. This
    // is the most common chain operators will type from muscle
    // memory (kubectl-style); pin it so a future cli.rs refactor
    // doesn't quietly break the combination.
    let cfg_dir = tempfile::tempdir().unwrap();
    cli()
        .env("APPRAFTER_CONFIG_DIR", cfg_dir.path())
        .args(["t", "ls"])
        .assert()
        .success()
        .stdout(contains("apprafter target add"));
}
