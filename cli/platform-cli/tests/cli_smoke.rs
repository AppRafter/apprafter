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
        .stdout(contains("upgrade-tier"))
        .stdout(contains("destroy"))
        .stdout(contains("import"))
        .stdout(contains("kubeconfig"))
        .stdout(contains("cluster-bootstrap"))
        .stdout(contains("argocd-password"));
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
fn init_prints_would_init_and_writes_state() {
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
    assert!(
        dir.path().join(".apprafter/state.json").exists(),
        "init should write the state file"
    );
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
fn apply_without_token_reports_missing_token() {
    let dir = tempfile::tempdir().unwrap();
    // Run init first so state has provider=hetzner-cloud.
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
        .success();
    cli()
        .current_dir(dir.path())
        .env_remove("HCLOUD_TOKEN")
        .arg("apply")
        .assert()
        .failure()
        .stderr(contains("HCLOUD_TOKEN"));
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

#[test]
fn apply_with_ssh_public_key_env_still_requires_token() {
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
        .success();
    cli()
        .current_dir(dir.path())
        .env_remove("HCLOUD_TOKEN")
        .env("APPRAFTER_SSH_PUBLIC_KEY", "ssh-ed25519 AAAA")
        .arg("apply")
        .assert()
        .failure()
        .stderr(contains("HCLOUD_TOKEN"));
}

#[test]
fn import_without_provider_in_state_errors_clearly() {
    let dir = tempfile::tempdir().unwrap();
    cli()
        .current_dir(dir.path())
        .env_remove("HCLOUD_TOKEN")
        .arg("import")
        .assert()
        .failure()
        .stderr(contains("provider"));
}

#[test]
fn import_without_token_reports_missing_token() {
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
        .success();
    cli()
        .current_dir(dir.path())
        .env_remove("HCLOUD_TOKEN")
        .arg("import")
        .assert()
        .failure()
        .stderr(contains("HCLOUD_TOKEN"));
}

#[test]
fn destroy_with_empty_state_is_noop() {
    let dir = tempfile::tempdir().unwrap();
    cli()
        .current_dir(dir.path())
        .env_remove("HCLOUD_TOKEN")
        .args(["destroy", "--yes"])
        .assert()
        .success()
        .stdout(contains("nothing to destroy"));
}

#[test]
fn cluster_bootstrap_without_hetzner_cloud_state_errors_with_hint() {
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
        .success();
    cli()
        .current_dir(dir.path())
        .arg("cluster-bootstrap")
        .assert()
        .failure()
        .stderr(contains("apply"));
}

#[test]
fn argocd_password_without_hetzner_cloud_state_errors_with_hint() {
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
        .success();
    cli()
        .current_dir(dir.path())
        .arg("argocd-password")
        .assert()
        .failure()
        .stderr(contains("apply"));
}

#[test]
fn tracing_logs_go_to_stderr_not_stdout() {
    // Regression guard: tracing-subscriber must write to stderr so
    // commands whose stdout is consumed downstream (e.g. `kubeconfig
    // | tee /tmp/kc`, `argocd-password | …`) produce clean
    // machine-readable output. The `init` command logs `INFO init
    // invoked …` and prints `would init …` to stdout — after the
    // v0.1.44 fix, only the latter should land on stdout.
    let dir = tempfile::tempdir().unwrap();
    let output = cli()
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
        .output()
        .expect("init runs");
    assert!(output.status.success(), "init should succeed");
    let stdout = String::from_utf8(output.stdout).expect("stdout is utf-8");
    let stderr = String::from_utf8(output.stderr).expect("stderr is utf-8");

    // Match on the tracing message body (`init invoked`) — it is
    // ANSI-color-decoration agnostic. The level prefix (`INFO`)
    // shows up wrapped in ANSI escape sequences, so a literal
    // ` INFO ` substring check is fragile across terminal envs.
    assert!(
        !stdout.contains("init invoked"),
        "stdout must not contain tracing log messages (would corrupt machine-readable command output like kubeconfig).\nSTDOUT:\n{stdout}\nSTDERR:\n{stderr}"
    );
    assert!(
        stdout.contains("would init"),
        "stdout still carries the human-readable program output.\nSTDOUT:\n{stdout}"
    );
    assert!(
        stderr.contains("init invoked"),
        "tracing log messages must land on stderr.\nSTDERR:\n{stderr}"
    );
}
