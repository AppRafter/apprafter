// SPDX-License-Identifier: FSL-1.1-Apache-2.0
use std::path::{Path, PathBuf};

use assert_cmd::Command;
use predicates::prelude::PredicateBooleanExt;
use predicates::str::contains;

fn cli() -> Command {
    Command::cargo_bin("apprafter").unwrap()
}

/// Path to the per-target state file under the supplied config
/// dir (v0.1.154 layout). Equivalent to
/// `cli_state::StatePaths::for_active_target` minus the type
/// wrapper.
fn state_path(cfg_dir: &Path, target: &str) -> PathBuf {
    cfg_dir
        .join("state")
        .join(target)
        .join(".apprafter")
        .join("state.json")
}

/// Create an active target named `default` under the supplied
/// config dir. The non-interactive `target add` invocation seeds
/// `GlobalConfig.active_target = "default"` on the first call so
/// subsequent commands (`init`, `apply`, `kubeconfig`, …)
/// resolve to the same target without an explicit `--target`
/// flag. Pin used by every test that needs a working
/// `apprafter init` / `apprafter apply` path under v0.1.154's
/// per-target state layout.
fn seed_active_target(cfg_dir: &Path) {
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
    let cfg_dir = tempfile::tempdir().unwrap();
    seed_active_target(cfg_dir.path());
    cli()
        .current_dir(dir.path())
        .env("APPRAFTER_CONFIG_DIR", cfg_dir.path())
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
    // v0.1.154: state lives at
    // <cfg_dir>/state/<active-target>/.apprafter/state.json
    // rather than <cwd>/.apprafter/state.json.
    assert!(
        state_path(cfg_dir.path(), "default").exists(),
        "init should write the per-target state file"
    );
}

#[test]
fn plan_on_empty_state_says_no_changes() {
    let dir = tempfile::tempdir().unwrap();
    let cfg_dir = tempfile::tempdir().unwrap();
    seed_active_target(cfg_dir.path());
    cli()
        .current_dir(dir.path())
        .env("APPRAFTER_CONFIG_DIR", cfg_dir.path())
        .arg("plan")
        .assert()
        .success()
        .stdout(contains("no changes"));
}

#[test]
fn apply_without_token_reports_missing_token() {
    // Active target exists (so state resolution passes the "no
    // active target" gate) but `credentials.yaml` is empty by
    // virtue of being hand-removed below — the token resolver
    // then walks the full chain and surfaces the operator-facing
    // hint mentioning all three remediation paths.
    let dir = tempfile::tempdir().unwrap();
    let cfg_dir = tempfile::tempdir().unwrap();
    seed_active_target(cfg_dir.path());
    // Wipe out the token Hetzner-side. `target add` refused to
    // create the target with a missing `--token` (the wizard /
    // CLI requires one), so we run the seed first and remove
    // afterwards. Equivalent to an operator who edited
    // credentials.yaml by hand.
    std::fs::remove_file(cfg_dir.path().join("targets/default/credentials.yaml"))
        .expect("credentials.yaml must exist after target add");
    cli()
        .current_dir(dir.path())
        .env("APPRAFTER_CONFIG_DIR", cfg_dir.path())
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
        .env("APPRAFTER_CONFIG_DIR", cfg_dir.path())
        .env_remove("HCLOUD_TOKEN")
        .arg("apply")
        .assert()
        .failure()
        // Error mentions every path the resolver tried. Miette's
        // terminal renderer wraps long lines so a substring
        // straddling a wrap point doesn't match — keep each
        // assertion to a single word that survives the wrap.
        .stderr(contains("--token"))
        .stderr(contains("HCLOUD_TOKEN"))
        .stderr(contains("apprafter target"));
}

#[test]
fn status_prints_would_show() {
    let dir = tempfile::tempdir().unwrap();
    let cfg_dir = tempfile::tempdir().unwrap();
    seed_active_target(cfg_dir.path());
    cli()
        .current_dir(dir.path())
        .env("APPRAFTER_CONFIG_DIR", cfg_dir.path())
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
    let cfg_dir = tempfile::tempdir().unwrap();
    seed_active_target(cfg_dir.path());
    // Remove the token Hetzner-side so the resolver chain fails
    // through to the "target has no token stored" branch even
    // though the active target exists.
    std::fs::remove_file(cfg_dir.path().join("targets/default/credentials.yaml")).unwrap();
    cli()
        .current_dir(dir.path())
        .env("APPRAFTER_CONFIG_DIR", cfg_dir.path())
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
        .env("APPRAFTER_CONFIG_DIR", cfg_dir.path())
        .env_remove("HCLOUD_TOKEN")
        .env("APPRAFTER_SSH_PUBLIC_KEY", "ssh-ed25519 AAAA")
        .arg("apply")
        .assert()
        .failure()
        .stderr(contains("HCLOUD_TOKEN"));
}

#[test]
fn import_without_provider_in_state_errors_clearly() {
    // v0.1.154: an empty config dir now fails at state
    // resolution with "no active target" instead of at the
    // provider-resolution step. Both error paths are friendly
    // — they nudge the operator to `apprafter target add`. We
    // pin the same hint-substring expectations so the test
    // still proves the operator gets a non-cryptic message.
    let dir = tempfile::tempdir().unwrap();
    let cfg_dir = tempfile::tempdir().unwrap();
    cli()
        .current_dir(dir.path())
        .env("APPRAFTER_CONFIG_DIR", cfg_dir.path())
        .env_remove("HCLOUD_TOKEN")
        .arg("import")
        .assert()
        .failure()
        // Either the state-resolution error ("no active target")
        // or the legacy provider-resolution error must mention
        // `apprafter target add` so the operator knows the
        // remediation path.
        .stderr(contains("apprafter target add").or(contains("apprafter init")));
}

#[test]
fn import_without_token_reports_missing_token() {
    let dir = tempfile::tempdir().unwrap();
    let cfg_dir = tempfile::tempdir().unwrap();
    seed_active_target(cfg_dir.path());
    // Strip the stored token so the resolver chain fails through
    // to the "target has no token stored" branch.
    std::fs::remove_file(cfg_dir.path().join("targets/default/credentials.yaml")).unwrap();
    cli()
        .current_dir(dir.path())
        .env("APPRAFTER_CONFIG_DIR", cfg_dir.path())
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
        .env("APPRAFTER_CONFIG_DIR", cfg_dir.path())
        .env_remove("HCLOUD_TOKEN")
        .arg("import")
        .assert()
        .failure()
        .stderr(contains("HCLOUD_TOKEN"));
}

#[test]
fn apply_without_init_uses_active_target_config_for_provider() {
    // Regression-guard for the v0.1.83 fix: after `target add`
    // the operator should be able to run `apprafter apply`
    // directly without first running `apprafter init`. v0.1.82
    // wired credential resolution but provider/region still
    // required state.json, so the user hit "state has no
    // provider — run init first" even when target had it all.
    let cwd = tempfile::tempdir().unwrap();
    let cfg_dir = tempfile::tempdir().unwrap();

    // Seed target store with full config but NO `init` in cwd.
    // Use an obviously-invalid token so apply fails fast on the
    // first API call — we don't want to actually hit Hetzner;
    // we just need to verify that apply got PAST the
    // "no provider" gate.
    cli()
        .env("APPRAFTER_CONFIG_DIR", cfg_dir.path())
        .env("APPRAFTER_NO_PING", "1")
        .env_remove("HCLOUD_TOKEN")
        .args([
            "target",
            "add",
            "active",
            "--provider",
            "hetzner-cloud",
            "--token",
            &"a".repeat(64),
            "--region",
            "nbg1",
            "--tier",
            "solo",
            "--cluster-name",
            "from-target",
        ])
        .assert()
        .success();

    // Apply with NO `init`. Should NOT fail with "state has no
    // provider"; should fail (or proceed to API call) after
    // pulling provider/region/cluster_name from the target.
    let output = cli()
        .current_dir(cwd.path())
        .env("APPRAFTER_CONFIG_DIR", cfg_dir.path())
        .env_remove("HCLOUD_TOKEN")
        .env_remove("APPRAFTER_NO_PING")
        // Point Hetzner at a closed port so apply fails on the
        // FIRST network attempt instead of hanging or actually
        // creating resources. The point of the test is that
        // apply got past provider resolution.
        .env("APPRAFTER_HCLOUD_BASE_URL", "http://127.0.0.1:1")
        .arg("apply")
        .output()
        .expect("apply runs");

    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    // Failure is expected (closed-port transport). What MUST
    // NOT appear is the "state has no provider" / "run init"
    // gate.
    assert!(
        !stderr.contains("state has no provider"),
        "apply should fall back to active target's provider, not require init.\nSTDERR:\n{stderr}\nSTDOUT:\n{stdout}"
    );
    assert!(
        !stderr.contains("run `apprafter init"),
        "init must be optional when an active target is configured.\nSTDERR:\n{stderr}"
    );
}

#[test]
fn apply_target_flag_routes_resolution_at_named_target_and_surfaces_not_found() {
    // Track A.8 plumbing smoke: `--target <name>` reaches the
    // credential resolver and errors with the canonical
    // "target not found, available: …" hint when the name is
    // unknown. Verifies the clap wiring + resolver dispatch
    // without requiring any real Hetzner endpoint.
    let dir = tempfile::tempdir().unwrap();
    let cfg_dir = tempfile::tempdir().unwrap();

    // Seed the target store with one valid target so the
    // resolver's "available" hint has something to recommend.
    cli()
        .env("APPRAFTER_CONFIG_DIR", cfg_dir.path())
        .env("APPRAFTER_NO_PING", "1")
        .env_remove("HCLOUD_TOKEN")
        .args([
            "target",
            "add",
            "real",
            "--provider",
            "hetzner-cloud",
            "--token",
            &"a".repeat(64),
        ])
        .assert()
        .success();

    cli()
        .current_dir(dir.path())
        .env("APPRAFTER_CONFIG_DIR", cfg_dir.path())
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
        .env("APPRAFTER_CONFIG_DIR", cfg_dir.path())
        .env_remove("HCLOUD_TOKEN")
        .args(["apply", "--target", "ghost"])
        .assert()
        .failure()
        .stderr(contains("ghost"))
        .stderr(contains("not found"))
        // The "available" hint enumerates the seeded target so
        // the operator knows the right name to retry with.
        .stderr(contains("real"));
}

#[test]
fn destroy_with_empty_state_is_noop() {
    let dir = tempfile::tempdir().unwrap();
    let cfg_dir = tempfile::tempdir().unwrap();
    seed_active_target(cfg_dir.path());
    cli()
        .current_dir(dir.path())
        .env("APPRAFTER_CONFIG_DIR", cfg_dir.path())
        .env_remove("HCLOUD_TOKEN")
        .args(["destroy", "--yes"])
        .assert()
        .success()
        .stdout(contains("nothing to destroy"));
}

#[test]
fn cluster_bootstrap_without_hetzner_cloud_state_errors_with_hint() {
    let dir = tempfile::tempdir().unwrap();
    let cfg_dir = tempfile::tempdir().unwrap();
    seed_active_target(cfg_dir.path());
    cli()
        .current_dir(dir.path())
        .env("APPRAFTER_CONFIG_DIR", cfg_dir.path())
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
        .env("APPRAFTER_CONFIG_DIR", cfg_dir.path())
        .arg("cluster-bootstrap")
        .assert()
        .failure()
        .stderr(contains("apply"));
}

#[test]
fn argocd_password_without_hetzner_cloud_state_errors_with_hint() {
    let dir = tempfile::tempdir().unwrap();
    let cfg_dir = tempfile::tempdir().unwrap();
    seed_active_target(cfg_dir.path());
    cli()
        .current_dir(dir.path())
        .env("APPRAFTER_CONFIG_DIR", cfg_dir.path())
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
        .env("APPRAFTER_CONFIG_DIR", cfg_dir.path())
        .arg("argocd-password")
        .assert()
        .failure()
        .stderr(contains("apply"));
}

#[test]
fn platform_cli_shim_warns_and_forwards() {
    // v0.1.69 ships the binary rename `platform-cli` → `apprafter`
    // with a deprecation shim. The shim must:
    //   1. print a warning to stderr that mentions both names + the
    //      v0.2.0 removal window,
    //   2. forward args + exit code to the real `apprafter` binary
    //      living next to it,
    //   3. let the forwarded command's stdout pass through unmodified
    //      so pipelines like `platform-cli kubeconfig | kubectl …`
    //      keep working during the deprecation cycle.
    let dir = tempfile::tempdir().unwrap();
    let cfg_dir = tempfile::tempdir().unwrap();
    seed_active_target(cfg_dir.path());
    let output = Command::cargo_bin("platform-cli")
        .unwrap()
        .current_dir(dir.path())
        .env("APPRAFTER_CONFIG_DIR", cfg_dir.path())
        .arg("plan")
        .output()
        .expect("shim runs");
    assert!(output.status.success(), "shim must forward exit code");
    let stderr = String::from_utf8(output.stderr).expect("stderr utf-8");
    let stdout = String::from_utf8(output.stdout).expect("stdout utf-8");
    assert!(
        stderr.contains("`platform-cli` has been renamed to `apprafter`"),
        "shim must announce the rename. STDERR:\n{stderr}"
    );
    assert!(
        stderr.contains("v0.2.0"),
        "shim must mention the removal window. STDERR:\n{stderr}"
    );
    assert!(
        stdout.contains("no changes"),
        "forwarded `plan` output must reach stdout untouched. STDOUT:\n{stdout}"
    );
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
    let cfg_dir = tempfile::tempdir().unwrap();
    seed_active_target(cfg_dir.path());
    let output = cli()
        .current_dir(dir.path())
        .env("APPRAFTER_CONFIG_DIR", cfg_dir.path())
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
