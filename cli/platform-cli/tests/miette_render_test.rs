// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Integration tests for the miette-based error renderer wired
//! into `main.rs` in v0.1.86 (Track A.10). These tests focus on
//! the END-TO-END contract — that a `CliError` reaching `main`
//! actually renders as a miette diagnostic with the `help:` line
//! and the stable `apprafter::…` code, not as `Debug`-styled raw
//! struct output.
//!
//! The unit tests in `cli_core::error::tests` already cover the
//! `.code()` / `.help()` accessor surface; here we verify that
//! the rendered stderr in a real subprocess matches what miette
//! is supposed to produce.

use assert_cmd::Command;
use predicates::str::contains;

fn cli() -> Command {
    Command::cargo_bin("apprafter").unwrap()
}

/// `apply` without any credentials → resolver fails through the
/// 3-step chain → typed `Other` error with the chain mentioned.
/// We assert the miette plumbing renders both the `help:` block
/// and a recognisable diagnostic structure. Independent of which
/// variant the underlying error is, miette's `fancy` reporter
/// always prints a `help:` line when a `#[diagnostic(help = …)]`
/// attribute exists on the variant.
#[test]
fn unhandled_error_renders_with_miette_help_line() {
    let dir = tempfile::tempdir().unwrap();
    let cfg_dir = tempfile::tempdir().unwrap();
    // Seed an active target without a token so the apply below
    // fails at the credential resolver (the `Other` variant with
    // the `apprafter::cli::other` code we're asserting on).
    cli()
        .env("APPRAFTER_CONFIG_DIR", cfg_dir.path())
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
    std::fs::remove_file(cfg_dir.path().join("targets/default/credentials.yaml")).unwrap();
    cli()
        .current_dir(dir.path())
        .env("APPRAFTER_CONFIG_DIR", cfg_dir.path())
        .env_remove("HCLOUD_TOKEN")
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

    let assert = cli()
        .current_dir(dir.path())
        .env("APPRAFTER_CONFIG_DIR", cfg_dir.path())
        .env_remove("HCLOUD_TOKEN")
        .env_remove("APPRAFTER_HCLOUD_BASE_URL")
        // Disable color so the substring matches don't have to
        // navigate ANSI escape codes. miette honours NO_COLOR.
        .env("NO_COLOR", "1")
        .arg("apply")
        .assert()
        .failure();

    // The `help:` line is miette's signature output — its
    // presence is the proof we're going through the fancy
    // reporter, not eyre / Debug. Catch-all `Other` variant
    // ships with the `apprafter::cli::other` code.
    assert
        .stderr(contains("help:"))
        .stderr(contains("apprafter::cli::other"));
}

/// `apply --target ghost` against a populated store → typed
/// `TargetNotFound`. The variant carries its own dedicated code
/// plus multi-line help text; this asserts both reach stderr
/// verbatim with no Debug-style mangling.
#[test]
fn typed_target_not_found_renders_with_dedicated_code_and_help() {
    let dir = tempfile::tempdir().unwrap();
    let cfg_dir = tempfile::tempdir().unwrap();
    cli()
        .current_dir(dir.path())
        .env("APPRAFTER_CONFIG_DIR", cfg_dir.path())
        .env_remove("HCLOUD_TOKEN")
        // Seed a real target so the "available" hint can list it.
        .args([
            "target",
            "add",
            "real",
            "--provider",
            "hetzner-cloud",
            "--token",
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            "--region",
            "nbg1",
            "--tier",
            "solo",
            "--no-ping",
            "--no-interactive",
        ])
        .assert()
        .success();

    // Wizard fails-loud on missing target with TargetNotFound when
    // the operator targets a renew against a non-existent name —
    // simplest path to trigger the typed variant without needing
    // a state file. The `target show ghost` subcommand also
    // surfaces `TargetNotFound`; pick that, it's read-only and
    // doesn't touch the API.
    cli()
        .current_dir(dir.path())
        .env("APPRAFTER_CONFIG_DIR", cfg_dir.path())
        .env_remove("HCLOUD_TOKEN")
        .env("NO_COLOR", "1")
        .args(["target", "show", "ghost"])
        .assert()
        .failure()
        .stderr(contains("apprafter::target::not_found"))
        .stderr(contains("help:"))
        // Help text walks operator through both list + add paths.
        .stderr(contains("apprafter target list"))
        .stderr(contains("apprafter target add"));
}

/// Confirm miette honours `NO_COLOR` — without it, the rendered
/// output carries ANSI escape sequences; with it set, the same
/// substrings show up in plain text. Used as a quick smoke for
/// the env-var pipe-friendliness contract (CLI in a script must
/// produce parseable output).
#[test]
fn no_color_env_yields_ansi_free_stderr() {
    let dir = tempfile::tempdir().unwrap();
    let cfg_dir = tempfile::tempdir().unwrap();
    let assert = cli()
        .current_dir(dir.path())
        .env("APPRAFTER_CONFIG_DIR", cfg_dir.path())
        .env_remove("HCLOUD_TOKEN")
        .env("NO_COLOR", "1")
        .args(["target", "show", "ghost"])
        .assert()
        .failure();
    let stderr_bytes = assert.get_output().stderr.clone();
    let stderr = String::from_utf8_lossy(&stderr_bytes);
    // ANSI CSI sequences start with ESC [ (`\x1b[`). They must
    // not appear when NO_COLOR is honoured.
    assert!(
        !stderr.contains('\x1b'),
        "expected ANSI-free output under NO_COLOR=1, got: {stderr}"
    );
    // But the help / code substrings must still be present.
    assert!(stderr.contains("help:"), "missing help line: {stderr}");
    assert!(
        stderr.contains("apprafter::target::not_found"),
        "missing diagnostic code: {stderr}"
    );
}
