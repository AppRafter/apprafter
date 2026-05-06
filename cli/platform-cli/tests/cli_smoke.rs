// SPDX-License-Identifier: FSL-1.1-MIT
use assert_cmd::Command;
use predicates::str::contains;

fn cli() -> Command {
    Command::cargo_bin("platform-cli").unwrap()
}

#[test]
fn help_lists_all_subcommands() {
    cli()
        .arg("--help")
        .assert()
        .success()
        .stdout(contains("init"))
        .stdout(contains("plan"))
        .stdout(contains("apply"))
        .stdout(contains("status"))
        .stdout(contains("login"))
        .stdout(contains("upgrade-tier"));
}

#[test]
fn version_flag_works() {
    cli()
        .arg("--version")
        .assert()
        .success()
        .stdout(contains(env!("CARGO_PKG_VERSION")));
}

#[test]
fn init_prints_would_init() {
    let dir = tempfile::tempdir().unwrap();
    cli()
        .current_dir(dir.path())
        .args([
            "init",
            "--provider",
            "hetzner-cloud",
            "--tier",
            "solo",
            "--region",
            "nbg1",
        ])
        .assert()
        .success()
        .stdout(contains("would init"))
        .stdout(contains("hetzner-cloud"))
        .stdout(contains("solo"))
        .stdout(contains("nbg1"));
}

#[test]
fn plan_on_empty_state_says_no_changes() {
    let dir = tempfile::tempdir().unwrap();
    cli()
        .current_dir(dir.path())
        .arg("plan")
        .assert()
        .success()
        .stdout(contains("no changes"));
}

#[test]
fn apply_prints_would_apply() {
    let dir = tempfile::tempdir().unwrap();
    cli()
        .current_dir(dir.path())
        .arg("apply")
        .assert()
        .success()
        .stdout(contains("would apply"));
}

#[test]
fn status_prints_would_show() {
    let dir = tempfile::tempdir().unwrap();
    cli()
        .current_dir(dir.path())
        .arg("status")
        .assert()
        .success()
        .stdout(contains("would show status"));
}

#[test]
fn login_prints_would_login() {
    let dir = tempfile::tempdir().unwrap();
    cli()
        .current_dir(dir.path())
        .arg("login")
        .assert()
        .success()
        .stdout(contains("would login"));
}

#[test]
fn upgrade_tier_prints_target() {
    let dir = tempfile::tempdir().unwrap();
    cli()
        .current_dir(dir.path())
        .args(["upgrade-tier", "--to", "team"])
        .assert()
        .success()
        .stdout(contains("would upgrade tier"))
        .stdout(contains("team"));
}
