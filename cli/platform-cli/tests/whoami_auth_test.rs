// SPDX-License-Identifier: FSL-1.1-MIT
//! Integration tests for `apprafter whoami` + `apprafter auth …`
//! stubs (Track A.6 / v0.1.80).
//!
//! Mirrors the test scaffolding from `target_test.rs`: each test
//! redirects the target store at a fresh `tempfile::TempDir` via
//! the `APPRAFTER_CONFIG_DIR` env override, and the ones that
//! exercise the API ping point `APPRAFTER_HCLOUD_BASE_URL` at a
//! mockito server.

use assert_cmd::Command;
use predicates::prelude::PredicateBooleanExt;
use predicates::str::contains;

fn cli() -> Command {
    Command::cargo_bin("apprafter").unwrap()
}

fn synthetic_hetzner_token() -> String {
    "a".repeat(64)
}

fn seed_target(dir: &std::path::Path) {
    let token = synthetic_hetzner_token();
    cli()
        .env("APPRAFTER_CONFIG_DIR", dir)
        .env("APPRAFTER_NO_PING", "1")
        .env_remove("HCLOUD_TOKEN")
        .args([
            "target",
            "add",
            "primary",
            "--provider",
            "hetzner-cloud",
            "--token",
            &token,
            "--region",
            "nbg1",
            "--tier",
            "solo",
        ])
        .assert()
        .success();
}

// ---------------------------------------------------------------
// whoami
// ---------------------------------------------------------------

#[test]
fn whoami_on_empty_store_prints_onboarding_hint() {
    let dir = tempfile::tempdir().unwrap();
    cli()
        .env("APPRAFTER_CONFIG_DIR", dir.path())
        .env("APPRAFTER_NO_PING", "1")
        .arg("whoami")
        .assert()
        .success()
        .stdout(contains("Identity:"))
        .stdout(contains("anonymous (self-hosted"))
        .stdout(contains("No active target"))
        .stdout(contains("apprafter target add"));
}

#[test]
fn whoami_with_active_target_renders_summary_and_honours_no_ping() {
    let dir = tempfile::tempdir().unwrap();
    seed_target(dir.path());

    cli()
        .env("APPRAFTER_CONFIG_DIR", dir.path())
        .env("APPRAFTER_NO_PING", "1")
        .arg("whoami")
        .assert()
        .success()
        .stdout(contains("Identity:"))
        .stdout(contains("anonymous (self-hosted"))
        .stdout(contains("Target:"))
        .stdout(contains("primary (active)"))
        .stdout(contains("Provider:"))
        .stdout(contains("hetzner-cloud"))
        .stdout(contains("verification skipped"))
        .stdout(contains("--no-ping"))
        .stdout(contains("Region:"))
        .stdout(contains("nbg1"))
        .stdout(contains("Default tier:"))
        .stdout(contains("solo"))
        // No token bytes ever appear in the whoami output —
        // verification status is just `verified ✓` / failed
        // string, never the credential body.
        .stdout(predicates::str::contains(synthetic_hetzner_token()).not());
}

#[test]
fn whoami_with_real_ping_reports_verified_on_mockito_200() {
    let mut server = mockito::Server::new();
    let _m = server
        .mock("GET", "/v1/locations")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(r#"{"locations":[]}"#)
        .expect(1)
        .create();

    let dir = tempfile::tempdir().unwrap();
    seed_target(dir.path());

    cli()
        .env("APPRAFTER_CONFIG_DIR", dir.path())
        .env("APPRAFTER_HCLOUD_BASE_URL", server.url())
        .env_remove("HCLOUD_TOKEN")
        .env_remove("APPRAFTER_NO_PING")
        .arg("whoami")
        .assert()
        .success()
        .stdout(contains("verified ✓"));
}

#[test]
fn whoami_with_real_ping_reports_failure_hint_on_mockito_401() {
    let mut server = mockito::Server::new();
    let _m = server
        .mock("GET", "/v1/locations")
        .with_status(401)
        .with_header("content-type", "application/json")
        .with_body(r#"{"error":{"code":"unauthorized","message":"bad token"}}"#)
        .create();

    let dir = tempfile::tempdir().unwrap();
    seed_target(dir.path());

    // 401 must NOT fail the whoami command — info is still useful
    // for the operator. Surface the verdict in the Provider line.
    cli()
        .env("APPRAFTER_CONFIG_DIR", dir.path())
        .env("APPRAFTER_HCLOUD_BASE_URL", server.url())
        .env_remove("HCLOUD_TOKEN")
        .env_remove("APPRAFTER_NO_PING")
        .arg("whoami")
        .assert()
        .success()
        .stdout(contains("verification failed"))
        .stdout(contains("HTTP 401"))
        .stdout(contains("--renew"));
}

#[test]
fn whoami_with_real_ping_reports_failure_when_provider_unreachable() {
    let dir = tempfile::tempdir().unwrap();
    seed_target(dir.path());

    cli()
        .env("APPRAFTER_CONFIG_DIR", dir.path())
        .env("APPRAFTER_HCLOUD_BASE_URL", "http://127.0.0.1:1")
        .env_remove("HCLOUD_TOKEN")
        .env_remove("APPRAFTER_NO_PING")
        .arg("whoami")
        .assert()
        .success()
        .stdout(contains("verification failed"))
        // Closed-port transport error is collapsed into the
        // "provider unreachable" branch (or surfaces as the
        // generic synthesized 5xx on some sandboxes).
        .stdout(contains("unreachable").or(contains("HTTP")));
}

// ---------------------------------------------------------------
// auth stubs
// ---------------------------------------------------------------

#[test]
fn auth_login_prints_friendly_redirect_to_target_add() {
    cli()
        .args(["auth", "login"])
        .assert()
        .success()
        .stdout(contains("AppRafter Cloud is not yet available"))
        .stdout(contains("apprafter target add"))
        .stdout(contains("https://apprafter.dev"));
}

#[test]
fn auth_logout_prints_friendly_redirect_with_nothing_to_logout_phrasing() {
    cli()
        .args(["auth", "logout"])
        .assert()
        .success()
        .stdout(contains("nothing to log out"))
        .stdout(contains("apprafter target add"));
}

#[test]
fn auth_status_explains_self_hosted_mode_and_points_at_whoami() {
    cli()
        .args(["auth", "status"])
        .assert()
        .success()
        .stdout(contains("not available yet"))
        .stdout(contains("Self-hosted mode active"))
        .stdout(contains("apprafter whoami"))
        .stdout(contains("https://apprafter.dev"));
}

#[test]
fn auth_group_is_hidden_from_top_level_help() {
    // `apprafter --help` must not list `auth` so the empty
    // namespace doesn't crowd the new-user discovery surface.
    // The subcommand itself still works — see the three tests
    // above — so power users + future docs can reach it.
    cli()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicates::str::contains("auth").not());
}

#[test]
fn auth_subcommand_help_is_still_reachable() {
    // The hide flag is for the top-level help only; deliberate
    // `apprafter auth --help` still renders the subcommand tree.
    cli()
        .args(["auth", "--help"])
        .assert()
        .success()
        .stdout(contains("login"))
        .stdout(contains("logout"))
        .stdout(contains("status"));
}
