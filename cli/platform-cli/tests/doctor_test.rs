// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Integration tests for `apprafter doctor` (Track A.7 /
//! v0.1.81).
//!
//! Each test points the target store at a fresh tempdir and uses
//! `APPRAFTER_NO_PING=1` to keep the run offline. The checks
//! themselves are `apprafter_core::doctor`'s and are unit-tested
//! there (the token ping against mockito); these tests cover what
//! the CLI adds: the rendering, the exit code and Ctrl-C.

use assert_cmd::Command;
use predicates::prelude::PredicateBooleanExt;
use predicates::str::contains;

/// `doctor` reaches beyond this machine (`startup.rs`), so without the bypass the startup
/// checks would call the network from a test (overview §6.2).
fn cli() -> Command {
    let mut cmd = Command::cargo_bin("apprafter").unwrap();
    cmd.env("APPRAFTER_SKIP_STARTUP_CHECKS", "1");
    cmd
}

mod common;

/// A `PATH` holding nothing but a stand-in for every tool `doctor` probes.
///
/// A clean run needs the required tools present, so a test that asserts one
/// passed or failed on what the CI image happened to ship: the macOS runners
/// carry no `kubectl`, and every happy-path test here failed on them. What a
/// stand-in is, per platform, is `common::stand_in::tool_stand_ins`'s to say.
fn tools_on_path() -> tempfile::TempDir {
    common::stand_in::tool_stand_ins(cli_core::tools::ALL)
}

/// Decision 2 of the D.3c review: each stand-in answers its tool's version call as the real
/// tool does — a version line and exit 0 (`ssh -V` on stderr) — and anything else with a usage
/// error. The resolver reports a tool that exits non-zero on its version call as having no
/// version (whatever it printed), so a stand-in that did would test a broken tool instead.
/// On every platform: the Unix scripts and the Windows `apprafter-tool-stand-in` keep the
/// same contract.
#[test]
fn the_stand_ins_answer_their_version_call_like_the_real_tools() {
    let tools = tools_on_path();
    for tool in cli_core::tools::ALL {
        let bin = tools
            .path()
            .join(format!("{}{}", tool.name, std::env::consts::EXE_SUFFIX));
        let out = std::process::Command::new(&bin)
            .args(tool.version_args)
            .output()
            .unwrap();
        assert!(out.status.success(), "`{}`: {out:?}", tool.name);
        let answer = if tool.name == "ssh" {
            &out.stderr
        } else {
            &out.stdout
        };
        assert_eq!(
            String::from_utf8_lossy(answer),
            format!("{} stand-in\n", tool.name)
        );
        let wrong = std::process::Command::new(&bin)
            .arg("--no-such-flag")
            .output()
            .unwrap();
        assert_eq!(wrong.status.code(), Some(2), "`{}`: {wrong:?}", tool.name);
    }
}

fn synthetic_hetzner_token() -> String {
    "a".repeat(64)
}

fn seed_target_with_ssh(dir: &std::path::Path, ssh_key: Option<&std::path::Path>) {
    let token = synthetic_hetzner_token();
    let mut args: Vec<String> = vec![
        "target".into(),
        "add".into(),
        "default".into(),
        "--provider".into(),
        "hetzner-cloud".into(),
        "--token".into(),
        token,
        "--region".into(),
        "nbg1".into(),
        "--tier".into(),
        "solo".into(),
    ];
    if let Some(k) = ssh_key {
        args.push("--ssh-key".into());
        args.push(k.to_string_lossy().into_owned());
    }
    cli()
        .env("APPRAFTER_CONFIG_DIR", dir)
        .env("APPRAFTER_NO_PING", "1")
        .env_remove("HCLOUD_TOKEN")
        .args(args)
        .assert()
        .success();
}

#[test]
fn doctor_on_empty_store_still_reports_the_environment() {
    // D11 / 2.22a. This test previously asserted `.failure()` with "no
    // active target" on stderr — the exact behaviour the audit called
    // wrong. `doctor`'s own docstring names first-run users as its
    // audience, and a first-run user has no target by definition, so
    // aborting there told them nothing about kubectl, helm, ssh or DNS:
    // precisely what they opened the command to learn.
    //
    // The exit status is deliberately NOT asserted. It now depends on
    // whether a REQUIRED tool is present on the machine running the
    // test, which is not a property of this code path. The exit-code
    // rule is pinned instead by the empty-PATH goldens
    // (`golden/doctor/*_empty_path.golden`: a missing kubectl is a FAIL
    // and `[exit 1]`).
    let dir = tempfile::tempdir().unwrap();
    let out = cli()
        .env("APPRAFTER_CONFIG_DIR", dir.path())
        .env("APPRAFTER_NO_PING", "1")
        .arg("doctor")
        .output()
        .expect("doctor runs");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();

    // The environment half reached the reader.
    for tool in ["kubectl", "helm", "restic", "git", "ssh", "cue"] {
        assert!(
            stdout.contains(tool),
            "`{tool}` missing from the report a first-run user sees:\n{stdout}"
        );
    }
    // And they are told what to do next, as a check rather than an abort.
    assert!(stdout.contains("active target"), "{stdout}");
    assert!(stdout.contains("apprafter target add"), "{stdout}");
}

#[test]
fn doctor_renders_target_and_env_checks_with_summary() {
    let dir = tempfile::tempdir().unwrap();
    let key_dir = tempfile::tempdir().unwrap();
    let key_path = key_dir.path().join("id_ed25519.pub");
    std::fs::write(&key_path, "ssh-ed25519 AAAA test@host").unwrap();

    seed_target_with_ssh(dir.path(), Some(&key_path));

    let tools = tools_on_path();
    cli()
        .env("APPRAFTER_CONFIG_DIR", dir.path())
        .env("APPRAFTER_NO_PING", "1")
        .env("PATH", tools.path())
        // Pins the production host: the DNS row names the host of the
        // configured API base, so an inherited override would move it.
        .env_remove("APPRAFTER_HCLOUD_BASE_URL")
        .arg("doctor")
        .assert()
        .success()
        // Target section.
        .stdout(contains("Checking target `default`"))
        .stdout(contains("Config file readable"))
        .stdout(contains("Credentials file"))
        .stdout(contains("Provider `hetzner-cloud` supported"))
        .stdout(contains("Token format valid"))
        // R8: --no-ping means the verification step did not run: a dash and
        // a detail that names no CLI flag, not a WARN.
        .stdout(contains(
            "– Token verified against provider API (not requested)",
        ))
        .stdout(contains("SSH key readable"))
        .stdout(contains("ssh-ed25519"))
        // Environment section.
        .stdout(contains("Checking environment"))
        .stdout(contains("DNS resolves"))
        .stdout(contains("api.hetzner.cloud"))
        // Summary line includes both the target name and the
        // overall verdict.
        .stdout(contains("checks for target `default`"));
}

#[test]
fn doctor_resolves_the_host_of_the_configured_api_base() {
    let dir = tempfile::tempdir().unwrap();
    cli()
        .env("APPRAFTER_CONFIG_DIR", dir.path())
        .env("APPRAFTER_SKIP_STARTUP_CHECKS", "1")
        .env("APPRAFTER_HCLOUD_BASE_URL", "http://127.0.0.1:1")
        .env("KUBECONFIG", "/nonexistent")
        .arg("doctor")
        .assert()
        .stdout(contains("DNS resolves `127.0.0.1`"))
        .stdout(contains("api.hetzner.cloud").not());
}

#[test]
fn doctor_target_flag_inspects_non_active_target() {
    let dir = tempfile::tempdir().unwrap();
    seed_target_with_ssh(dir.path(), None);
    // Add a second target that isn't active.
    cli()
        .env("APPRAFTER_CONFIG_DIR", dir.path())
        .env("APPRAFTER_NO_PING", "1")
        .env_remove("HCLOUD_TOKEN")
        .args([
            "target",
            "add",
            "secondary",
            "--provider",
            "hetzner-cloud",
            "--token",
            &synthetic_hetzner_token(),
        ])
        .assert()
        .success();

    let tools = tools_on_path();
    cli()
        .env("APPRAFTER_CONFIG_DIR", dir.path())
        .env("APPRAFTER_NO_PING", "1")
        .env("PATH", tools.path())
        .args(["doctor", "--target", "secondary"])
        .assert()
        .success()
        .stdout(contains("Checking target `secondary`"));
}

#[test]
fn doctor_ssh_key_missing_path_fails_the_run_with_exit_1() {
    // Configure a target with an ssh-key path, then delete the
    // file so the doctor's ssh-key check trips into FAIL.
    let dir = tempfile::tempdir().unwrap();
    let key_dir = tempfile::tempdir().unwrap();
    let key_path = key_dir.path().join("id_ed25519.pub");
    std::fs::write(&key_path, "ssh-ed25519 AAAA test@host").unwrap();
    seed_target_with_ssh(dir.path(), Some(&key_path));

    // Wipe the key file — its path stays in the target config,
    // doctor must surface this as a FAIL (stale config).
    std::fs::remove_file(&key_path).unwrap();

    let tools = tools_on_path();
    cli()
        .env("APPRAFTER_CONFIG_DIR", dir.path())
        .env("APPRAFTER_NO_PING", "1")
        .env("PATH", tools.path())
        .arg("doctor")
        .assert()
        .failure()
        .stdout(contains("SSH key readable"))
        .stdout(contains("file does not exist"))
        .stdout(contains("FAIL"));
}

#[test]
fn doctor_target_not_found_fails_with_available_hint() {
    let dir = tempfile::tempdir().unwrap();
    seed_target_with_ssh(dir.path(), None);

    cli()
        .env("APPRAFTER_CONFIG_DIR", dir.path())
        .env("APPRAFTER_NO_PING", "1")
        .args(["doctor", "--target", "ghost"])
        .assert()
        .failure()
        .stdout(contains("Target `ghost`"))
        .stdout(contains("available targets"))
        .stdout(contains("default"));
}

#[test]
fn doctor_summary_line_phrases_outcomes_clearly() {
    // Happy path: no FAILs and no warnings. With every tool present and the
    // token check skipped (`--no-ping`, not counted), the summary is "All
    // good" and contains neither "FAIL" nor "warning".
    let dir = tempfile::tempdir().unwrap();
    let key_dir = tempfile::tempdir().unwrap();
    let key_path = key_dir.path().join("id_ed25519.pub");
    std::fs::write(&key_path, "ssh-ed25519 AAAA test@host").unwrap();
    seed_target_with_ssh(dir.path(), Some(&key_path));

    let tools = tools_on_path();
    cli()
        .env("APPRAFTER_CONFIG_DIR", dir.path())
        .env("APPRAFTER_NO_PING", "1")
        .env("PATH", tools.path())
        // The DNS row then resolves `127.0.0.1`, so the run is hermetic.
        .env("APPRAFTER_HCLOUD_BASE_URL", "http://127.0.0.1:1")
        .arg("doctor")
        .assert()
        .success()
        .stdout(predicates::str::contains(" FAIL").not())
        // R8: the token check `--no-ping` skipped is not a warning, so a healthy target is
        // "All good" rather than "review warnings".
        .stdout(contains("warning").not())
        .stdout(contains("All good"));
}

/// WI-451 decision 1: the core starts every tool probe in a session of its own, which the
/// terminal's Ctrl-C does not reach, so doctor turns SIGINT into cancelling its run. The probe
/// it was waiting on is then killed, and doctor exits 130 at once rather than at the probe's
/// timeout. Without that, the default SIGINT ends doctor and leaves the probe running.
#[cfg(unix)]
#[test]
fn ctrl_c_stops_doctor_and_kills_the_tool_it_was_probing() {
    use std::time::{Duration, Instant};

    use apprafter_core::tools::TOOL_PROBE_TIMEOUT;

    fn alive(pid: i32) -> bool {
        // SAFETY: signal 0 only asks whether the process exists.
        unsafe { libc::kill(pid, 0) == 0 }
    }

    let dir = tempfile::tempdir().unwrap();
    let tools = tools_on_path();
    let pid_file = tools.path().join("restic.pid");
    let host_path = std::env::var_os("PATH").unwrap_or_default();
    let sleep =
        cli_core::tools::find_on_path("sleep", &host_path, cli_core::tools::is_executable_file)
            .expect("a `sleep` on the test's PATH");
    // A restic that never answers its version call: it records its pid and becomes `sleep`.
    common::stand_in::script(
        &tools.path().join("restic"),
        &format!(
            "echo $$ > '{}'\nexec '{}' 60",
            pid_file.display(),
            sleep.display()
        ),
    );
    let started = Instant::now();
    let mut doctor = std::process::Command::new(env!("CARGO_BIN_EXE_apprafter"))
        .env("APPRAFTER_CONFIG_DIR", dir.path())
        .env("APPRAFTER_SKIP_STARTUP_CHECKS", "1")
        .env("APPRAFTER_NO_PING", "1")
        .env("APPRAFTER_HCLOUD_BASE_URL", "http://127.0.0.1:1")
        .env("KUBECONFIG", "/nonexistent")
        .env("PATH", tools.path())
        .arg("doctor")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let probe = loop {
        if let Some(pid) = std::fs::read_to_string(&pid_file)
            .ok()
            .and_then(|s| s.trim().parse::<i32>().ok())
        {
            break pid;
        }
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "the hung probe never started"
        );
        std::thread::sleep(Duration::from_millis(20));
    };
    // About a second in, the probe hung and its timeout still four seconds away.
    std::thread::sleep(Duration::from_secs(1).saturating_sub(started.elapsed()));
    let signalled = Instant::now();
    // SAFETY: a plain kill(2) of the child this test started.
    assert_eq!(unsafe { libc::kill(doctor.id() as i32, libc::SIGINT) }, 0);
    let exited = loop {
        if doctor.try_wait().unwrap().is_some() {
            break Some(signalled.elapsed());
        }
        if signalled.elapsed() > TOOL_PROBE_TIMEOUT * 3 {
            break None;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let gone = (0..100).any(|_| {
        let gone = !alive(probe);
        if !gone {
            std::thread::sleep(Duration::from_millis(20));
        }
        gone
    });
    if !gone {
        // Do not leak it into the rest of the run.
        // SAFETY: as above.
        unsafe { libc::kill(probe, libc::SIGKILL) };
    }
    if exited.is_none() {
        let _ = doctor.kill();
    }
    let out = doctor.wait_with_output().unwrap();
    let (stdout, stderr) = (
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    let exited = exited.unwrap_or_else(|| panic!("doctor did not exit after SIGINT:\n{stderr}"));
    assert_eq!(out.status.code(), Some(130), "{stdout}\n{stderr}");
    assert!(
        exited < TOOL_PROBE_TIMEOUT - Duration::from_secs(1),
        "doctor took {exited:?} to stop: it waited for the probe's own timeout"
    );
    assert!(gone, "the probed tool (pid {probe}) outlived doctor");
    assert!(stderr.contains("interrupted"), "{stderr}");
    assert!(
        !stdout.contains("checks"),
        "an interrupted run prints no report:\n{stdout}"
    );
}

#[test]
fn doctor_prints_the_cluster_group_for_an_existing_target() {
    let dir = tempfile::tempdir().unwrap();
    seed_target_with_ssh(dir.path(), None);
    let tools = tools_on_path();
    let out = cli()
        .env("APPRAFTER_CONFIG_DIR", dir.path())
        .env("APPRAFTER_NO_PING", "1")
        .env("APPRAFTER_HCLOUD_BASE_URL", "http://127.0.0.1:1")
        .env("KUBECONFIG", "/nonexistent")
        .env("PATH", tools.path())
        .arg("doctor")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("Checking cluster...\n"), "{stdout}");
    for row in [
        "  – Kubeconfig cached (no provisioned server)",
        "  – Kube API reachable (no provisioned server)",
        "  – Node reachable over SSH (no provisioned server)",
    ] {
        assert!(stdout.contains(row), "missing `{row}`:\n{stdout}");
    }
}

/// Overview §3.11: the CLI moves a v0.1.153 `<cwd>/.apprafter/state.json` into the store
/// before the core reads state, as every state-reading command does.
#[test]
fn doctor_migrates_a_legacy_cwd_state_before_reading_it() {
    let dir = tempfile::tempdir().unwrap();
    seed_target_with_ssh(dir.path(), None);
    let cwd = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(cwd.path().join(".apprafter")).unwrap();
    std::fs::write(
        cwd.path().join(".apprafter/state.json"),
        r#"{"hetzner_cloud":{"server_id":9,"server_name":"legacy"}}"#,
    )
    .unwrap();
    let tools = tools_on_path();
    let out = cli()
        .env("APPRAFTER_CONFIG_DIR", dir.path())
        .env("APPRAFTER_NO_PING", "1")
        .env("APPRAFTER_HCLOUD_BASE_URL", "http://127.0.0.1:1")
        .env("KUBECONFIG", "/nonexistent")
        .env("PATH", tools.path())
        .current_dir(cwd.path())
        .arg("doctor")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("✗ Kubeconfig cached (none cached for server `legacy` (id 9))"),
        "{stdout}"
    );
    assert!(dir
        .path()
        .join("state/default/.apprafter/state.json")
        .exists());
}

/// Deviation 3: a missing target is a row, and doctor creates no state directory for it.
#[test]
fn doctor_of_a_missing_target_creates_no_state_dir() {
    let dir = tempfile::tempdir().unwrap();
    seed_target_with_ssh(dir.path(), None);
    let cwd = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(cwd.path().join(".apprafter")).unwrap();
    std::fs::write(
        cwd.path().join(".apprafter/state.json"),
        r#"{"hetzner_cloud":{"server_id":9,"server_name":"legacy"}}"#,
    )
    .unwrap();
    cli()
        .env("APPRAFTER_CONFIG_DIR", dir.path())
        .env("APPRAFTER_NO_PING", "1")
        .env("APPRAFTER_HCLOUD_BASE_URL", "http://127.0.0.1:1")
        .env("KUBECONFIG", "/nonexistent")
        .current_dir(cwd.path())
        .args(["doctor", "--target", "ghost"])
        .assert()
        .failure()
        .stdout(contains("Target `ghost` exists"));
    assert!(!dir.path().join("state/ghost").exists());
    assert!(
        cwd.path().join(".apprafter/state.json").exists(),
        "the legacy file is left for a command on an existing target"
    );
}
