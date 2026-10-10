// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Golden snapshots of `apprafter` output — Phase D, D.1 Part A.
//!
//! D.1 moves the CLI onto a shared `apprafter-core` crate (ADR 0067). Its
//! invariant is that, on Unix, the CLI prints exactly what it printed
//! before, except for changes made on purpose. These cases pin that
//! byte for byte. The harness — the sandbox, the normalisations and how
//! to record — and its rules are in `common/golden.rs`.
#![cfg(unix)]

mod common;
use common::golden::*;

use std::fs;
use std::path::{Path, PathBuf};

use tempfile::TempDir;

// ---------------------------------------------------------------------
// The harness's own guarantees
// ---------------------------------------------------------------------

#[test]
fn harness_normalizes_sandbox_hcloud_and_version() {
    let sb = Sandbox::new().with_hcloud("http://127.0.0.1:41234".to_string());
    let raw = format!(
        "{} at http://127.0.0.1:41234 v{VERSION}",
        sb.path("apprafter-config").display()
    );
    assert_eq!(
        sb.normalize(&raw),
        "<SANDBOX>/apprafter-config at <HCLOUD> v<VERSION>"
    );
}

#[test]
fn harness_normalizes_the_closed_port_url_without_a_mock() {
    let sb = Sandbox::new();
    assert_eq!(
        sb.normalize("GET http://127.0.0.1:1/v1/locations failed."),
        "GET <HCLOUD>/v1/locations failed."
    );
}

#[test]
fn harness_never_half_replaces_a_mock_url_that_starts_like_the_closed_port() {
    let sb = Sandbox::new().with_hcloud("http://127.0.0.1:12345".to_string());
    assert_eq!(
        sb.normalize("GET http://127.0.0.1:12345/v1 and http://127.0.0.1:1/v1"),
        "GET <HCLOUD>/v1 and <HCLOUD>/v1"
    );
    let unmocked = Sandbox::new();
    assert_eq!(
        unmocked.normalize("http://127.0.0.1:12345/v1"),
        "http://127.0.0.1:12345/v1"
    );
}

#[test]
fn harness_masks_tracing_timestamps_and_escapes() {
    let sb = Sandbox::new();
    assert_eq!(
        sb.normalize("\x1b[2m2026-10-08T05:08:01.068034Z\x1b[0m \x1b[32m INFO\x1b[0m hi\n"),
        "<ESC>[2m<TS><ESC>[0m <ESC>[32m INFO<ESC>[0m hi\n"
    );
}

#[test]
fn harness_marks_a_missing_trailing_newline() {
    let mut doc = String::new();
    section(&mut doc, "stdout", "no newline");
    assert_eq!(doc, "[stdout]\nno newline\n[no newline at end]\n");
}

#[test]
fn harness_masks_a_timestamp_with_a_fraction() {
    assert_eq!(
        mask_timestamps("at 2026-10-08T05:08:01.068034Z done"),
        "at <TS> done"
    );
}

#[test]
fn harness_masks_a_timestamp_without_a_fraction() {
    assert_eq!(mask_timestamps("at 2026-10-08T05:08:01Z"), "at <TS>");
}

#[test]
fn harness_masks_two_timestamps_on_one_line() {
    assert_eq!(
        mask_timestamps("2026-10-08T05:08:01Z..2026-10-09T23:59:59.5Z\n"),
        "<TS>..<TS>\n"
    );
}

#[test]
fn harness_leaves_non_timestamps_alone() {
    for s in [
        "2026-10-08 05:08:01Z",
        "2026-10-08T05:08:01+02:00",
        "2026-10-08T05:08:01.Z",
        "2026-10-08T05:08Z",
        "12026-10-08T05:08:01Z",
        "v0.2.80 build 20261008",
    ] {
        assert_eq!(mask_timestamps(s), s, "{s}");
    }
}

#[test]
fn harness_replace_bounded_replaces_a_standalone_version() {
    assert_eq!(
        replace_bounded("apprafter 0.2.80\n(v0.2.80)", "0.2.80", "<V>"),
        "apprafter <V>\n(v<V>)"
    );
}

#[test]
fn harness_replace_bounded_leaves_longer_numbers_alone() {
    for s in ["10.2.80", "0.2.801", "1.0.2.80", "0.2.80.1"] {
        assert_eq!(replace_bounded(s, "0.2.80", "<V>"), s, "{s}");
    }
}

#[test]
fn harness_check_at_reports_a_missing_golden() {
    let dir = TempDir::new().expect("tempdir");
    let path = dir.path().join("family/case.golden");
    let res = check_at(&path, "anything", false);
    assert!(matches!(res, Err(CheckError::Missing(_))), "{res:?}");
    assert!(!path.exists(), "verify mode must not write");
}

#[test]
fn harness_check_at_accepts_an_equal_golden() {
    let dir = TempDir::new().expect("tempdir");
    let path = dir.path().join("case.golden");
    fs::write(&path, "same\n").expect("write");
    let res = check_at(&path, "same\n", false);
    assert!(res.is_ok(), "{res:?}");
}

#[test]
fn harness_check_at_reports_a_differing_golden() {
    let dir = TempDir::new().expect("tempdir");
    let path = dir.path().join("case.golden");
    fs::write(&path, "old\n").expect("write");
    let res = check_at(&path, "new\n", false);
    assert!(matches!(res, Err(CheckError::Differs(_))), "{res:?}");
    assert_eq!(fs::read_to_string(&path).expect("read"), "old\n");
}

#[test]
fn harness_check_at_update_mode_writes_and_fails() {
    let dir = TempDir::new().expect("tempdir");
    let path = dir.path().join("family/case.golden");
    let res = check_at(&path, "fresh\n", true);
    assert!(matches!(res, Err(CheckError::Written(_))), "{res:?}");
    assert_eq!(fs::read_to_string(&path).expect("read"), "fresh\n");
}

#[test]
fn harness_masks_millisecond_timings() {
    assert_eq!(
        mask_millis("Hetzner Cloud /v1/locations, 182 ms)"),
        "Hetzner Cloud /v1/locations, <MS> ms)"
    );
    assert_eq!(mask_millis("0 ms"), "<MS> ms");
    assert_eq!(mask_millis("a 1 ms, b 22 ms."), "a <MS> ms, b <MS> ms.");
}

#[test]
fn harness_leaves_other_ms_text_alone() {
    for s in ["182ms", "x12 ms", "12 msgs", "v1.5 ms", "ms 12", "12  ms"] {
        assert_eq!(mask_millis(s), s, "{s}");
    }
}

#[test]
fn harness_masks_os_error_numbers() {
    assert_eq!(
        mask_os_errors("Connection refused (os error 111) / (os error 61)"),
        "Connection refused (os error <N>) / (os error <N>)"
    );
    for s in ["(os error )", "(os error x)", "(os error 2", "os error 2)"] {
        assert_eq!(mask_os_errors(s), s, "{s}");
    }
}

#[test]
fn harness_normalize_applies_the_new_masks() {
    let sb = Sandbox::new();
    assert_eq!(
        sb.normalize("ping 7 ms; refused (os error 111)"),
        "ping <MS> ms; refused (os error <N>)"
    );
}

#[test]
fn harness_seed_helpers_write_into_the_store() {
    let sb = Sandbox::new();
    sb.seed_state("prod", "{}");
    assert!(sb
        .path("apprafter-config/state/prod/.apprafter/state.json")
        .exists());
    sb.seed_config("prod", "provider: hetzner-cloud\n");
    assert!(sb
        .path("apprafter-config/targets/prod/config.yaml")
        .exists());
    sb.seed_pointer("gone");
    assert_eq!(
        fs::read_to_string(sb.path("apprafter-config/config.yaml")).unwrap(),
        "active_target: gone\nversion: 1\n"
    );
    sb.clear_pointer();
    assert!(!sb.path("apprafter-config/config.yaml").exists());
}

#[test]
fn harness_stand_in_tools_link_every_probed_tool() {
    let sb = Sandbox::new().with_stand_in_tools();
    let dir = sb.path_override().expect("stand-ins replace PATH");
    for name in cli_core::tools::ALL.iter().map(|t| t.name) {
        let file = dir.join(format!("{name}{}", std::env::consts::EXE_SUFFIX));
        assert!(file.is_file(), "{}", file.display());
    }
}

/// miette wraps a `×` message at 80 columns, breaking inside a word, before the harness swaps
/// the sandbox root for `<SANDBOX>`; so a case that prints a sandbox path in one matches only
/// while the root keeps the length it was recorded with. macOS's `TMPDIR`
/// (`/var/folders/<2>/<30>/T/`) is about 45 characters longer than Linux's `/tmp`. This re-runs
/// such a case in a child of this test binary with a deliberately long `TMPDIR`: it must match.
#[test]
fn harness_a_long_tmpdir_changes_no_golden() {
    let scratch = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).expect("scratch dir");
    let long = scratch
        .path()
        .join("a-temporary-directory-as-long-as-the-one-macos-hands-every-process");
    fs::create_dir_all(&long).expect("long TMPDIR");
    let case = "target_add_with_a_missing_ssh_key";
    let out = std::process::Command::new(std::env::current_exe().expect("this test binary"))
        .args(["--exact", case, "--test-threads=1"])
        .env("TMPDIR", &long)
        .env_remove(UPDATE_ENV)
        .output()
        .expect("re-run the case");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success() && stdout.contains("1 passed"),
        "`{case}` under TMPDIR={} failed:\n{stdout}\n{}",
        long.display(),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A spelling of the sandbox root the harness does not know, or a root a wrapped message split
/// before its last component, leaves the root's own name in the normalised output: that fails
/// with a clear message on every OS, in update mode too.
#[test]
#[should_panic(expected = "the sandbox root survived normalisation")]
fn harness_refuses_output_that_still_names_the_sandbox_root() {
    let sb = Sandbox::new();
    let root = sb.path("").display().to_string();
    let (head, tail) = root.split_at(3);
    sb.assert_no_raw_root("harness/split", &format!("× {head}\n  │ {tail}/home\n"));
}

/// Every recorded file must belong to a case in this file or in
/// `golden_doctor.rs`; otherwise a renamed or deleted case would leave a
/// golden that nothing checks.
#[test]
fn every_golden_file_has_a_case() {
    fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
        for entry in fs::read_dir(dir).expect("read golden dir") {
            let path = entry.expect("golden dir entry").path();
            if path.is_dir() {
                collect(&path, out);
            } else if path.extension().is_some_and(|e| e == "golden") {
                out.push(path);
            }
        }
    }
    let sources = [include_str!("golden.rs"), include_str!("golden_doctor.rs")];
    let root = golden_root();
    let mut files = Vec::new();
    collect(&root, &mut files);
    assert!(
        !files.is_empty(),
        "no golden files under {}",
        root.display()
    );
    let orphans: Vec<String> = files
        .iter()
        .map(|p| {
            p.strip_prefix(&root)
                .expect("under the golden root")
                .with_extension("")
                .display()
                .to_string()
        })
        .filter(|id| {
            let quoted = format!("\"{id}\"");
            !sources.iter().any(|source| source.contains(&quoted))
        })
        .collect();
    assert!(
        orphans.is_empty(),
        "golden files with no case in golden.rs or golden_doctor.rs (restore the case or \
         delete the file): {orphans:?}"
    );
}

// ---------------------------------------------------------------------
// target — local (no network)
// ---------------------------------------------------------------------

#[test]
fn target_list_empty_store() {
    Sandbox::new().golden("target/list_empty", &["target", "list"]);
}

#[test]
fn target_add_first_becomes_active() {
    let sb = Sandbox::new();
    let key = sb.ssh_key();
    sb.golden(
        "target/add_first",
        &[
            "target",
            "add",
            "prod",
            "--provider",
            "hetzner-cloud",
            "--token",
            TOKEN_A,
            "--ssh-key",
            &key,
            "--region",
            "nbg1",
            "--tier",
            "team",
            "--cluster-name",
            "platform-1",
            "--no-ping",
            "--no-interactive",
        ],
    );
}

#[test]
fn target_add_second_keeps_active() {
    let sb = Sandbox::new();
    sb.add_target("prod");
    let key = sb.ssh_key();
    sb.golden(
        "target/add_second",
        &[
            "target",
            "add",
            "staging",
            "--provider",
            "hetzner-cloud",
            "--token",
            TOKEN_B,
            "--ssh-key",
            &key,
            "--region",
            "fsn1",
            "--tier",
            "solo",
            "--no-ping",
            "--no-interactive",
        ],
    );
}

#[test]
fn target_add_existing_without_force_is_refused() {
    let sb = Sandbox::new();
    sb.add_target("prod");
    let key = sb.ssh_key();
    sb.golden(
        "target/add_existing_refused",
        &[
            "target",
            "add",
            "prod",
            "--provider",
            "hetzner-cloud",
            "--token",
            TOKEN_B,
            "--ssh-key",
            &key,
            "--no-ping",
            "--no-interactive",
        ],
    );
}

#[test]
fn target_add_force_overwrites() {
    let sb = Sandbox::new();
    sb.add_target("prod");
    let key = sb.ssh_key();
    sb.golden_steps(
        "target/add_force",
        &[
            &[
                "target",
                "add",
                "prod",
                "--provider",
                "hetzner-cloud",
                "--token",
                TOKEN_B,
                "--ssh-key",
                &key,
                "--region",
                "hel1",
                "--force",
                "--no-ping",
                "--no-interactive",
            ],
            &["target", "show"],
        ],
    );
    // Bug 8: the tier the setup stored survives a `--force` that did not pass `--tier`.
    let cfg = fs::read_to_string(sb.path("apprafter-config/targets/prod/config.yaml")).unwrap();
    assert!(cfg.contains("default_tier: solo"), "{cfg}");
}

#[test]
fn target_add_invalid_name_is_refused() {
    let sb = Sandbox::new();
    sb.golden(
        "target/add_invalid_name",
        &[
            "target",
            "add",
            "bad_name",
            "--provider",
            "hetzner-cloud",
            "--token",
            TOKEN_A,
            "--no-ping",
            "--no-interactive",
        ],
    );
}

#[test]
fn target_add_name_with_dash_edges_is_refused() {
    let sb = Sandbox::new();
    sb.golden(
        "target/add_name_dash_edges",
        &[
            "target",
            "add",
            "--provider",
            "hetzner-cloud",
            "--token",
            TOKEN_A,
            "--no-ping",
            "--no-interactive",
            "--",
            "-bad-",
        ],
    );
}

#[test]
fn target_add_renew_rotates_token() {
    // `whoami` pings with the stored token; the mock answers only TOKEN_B,
    // so a verified ping proves the renew replaced TOKEN_A.
    let mut server = mockito::Server::new();
    let _loc = json_mock(&mut server, "/v1/locations", 200, LOCATIONS_OK, TOKEN_B);
    let sb = Sandbox::new().with_hcloud(server.url());
    sb.add_target("prod");
    sb.golden_steps(
        "target/add_renew",
        &[
            &[
                "target",
                "add",
                "prod",
                "--renew",
                "--token",
                TOKEN_B,
                "--no-ping",
                "--no-interactive",
            ],
            &["whoami"],
        ],
    );
}

#[test]
fn target_add_renew_identical_token_is_refused() {
    let sb = Sandbox::new();
    sb.add_target("prod");
    sb.golden(
        "target/add_renew_identical",
        &[
            "target",
            "add",
            "prod",
            "--renew",
            "--token",
            TOKEN_A,
            "--no-ping",
            "--no-interactive",
        ],
    );
}

#[test]
fn target_list_two_marks_active() {
    let sb = Sandbox::new();
    sb.add_target("prod");
    sb.add_target("staging");
    sb.golden("target/list_two", &["target", "list"]);
}

#[test]
fn target_show_active() {
    let sb = Sandbox::new();
    sb.add_target("prod");
    sb.golden("target/show_active", &["target", "show"]);
}

#[test]
fn target_info_alias_matches_show() {
    let sb = Sandbox::new();
    sb.add_target("prod");
    sb.golden("target/info_alias", &["target", "info"]);
}

#[test]
fn target_show_named_unknown() {
    let sb = Sandbox::new();
    sb.add_target("prod");
    sb.golden("target/show_unknown", &["target", "show", "ghost"]);
}

#[test]
fn target_use_switches_active() {
    let sb = Sandbox::new();
    sb.add_target("prod");
    sb.add_target("staging");
    sb.golden("target/use_staging", &["target", "use", "staging"]);
}

/// WI-458 review #4: `use` refuses a target it cannot load — credentials or config that do not
/// parse — with that file's error, and the default stays. The refusal moved from the CLI into
/// the core, which the desktop's Make default calls too; recorded before the move, this pins
/// that the CLI prints what it printed.
#[test]
fn target_use_refuses_a_target_whose_files_cannot_be_read() {
    let sb = Sandbox::new();
    sb.add_target("prod");
    sb.add_target("staging");
    sb.add_target("test");
    sb.seed_store_file(
        "targets/staging/credentials.yaml",
        "hetzner_token: [unclosed\n",
    );
    sb.seed_store_file("targets/test/config.yaml", "provider: [unclosed\n");
    sb.golden_steps(
        "target/use_unreadable",
        &[
            &["target", "use", "staging"],
            &["target", "use", "test"],
            &["target", "list"],
        ],
    );
}

#[test]
fn target_use_unknown_is_refused() {
    let sb = Sandbox::new();
    sb.add_target("prod");
    sb.golden("target/use_unknown", &["target", "use", "ghost"]);
}

#[test]
fn target_rename_active_keeps_it_active() {
    let sb = Sandbox::new();
    sb.add_target("prod");
    sb.add_target("staging");
    sb.golden_steps(
        "target/rename_active",
        &[
            &["target", "rename", "prod", "production"],
            &["target", "list"],
        ],
    );
}

#[test]
fn target_rename_inactive() {
    let sb = Sandbox::new();
    sb.add_target("prod");
    sb.add_target("staging");
    sb.golden(
        "target/rename_inactive",
        &["target", "rename", "staging", "stage"],
    );
}

#[test]
fn target_remove_active_moves_pointer_to_next() {
    let sb = Sandbox::new();
    sb.add_target("prod");
    sb.add_target("staging");
    sb.golden_steps(
        "target/remove_active",
        &[&["target", "remove", "prod", "--yes"], &["target", "list"]],
    );
}

#[test]
fn target_remove_inactive() {
    let sb = Sandbox::new();
    sb.add_target("prod");
    sb.add_target("staging");
    sb.golden(
        "target/remove_inactive",
        &["target", "remove", "staging", "--yes"],
    );
}

#[test]
fn target_remove_last_clears_pointer() {
    let sb = Sandbox::new();
    sb.add_target("prod");
    sb.golden_steps(
        "target/remove_last",
        &[&["target", "remove", "prod", "--yes"], &["target", "list"]],
    );
}

#[test]
fn target_remove_without_yes_non_interactive() {
    let sb = Sandbox::new();
    sb.add_target("prod");
    sb.golden("target/remove_no_yes", &["target", "remove", "prod"]);
}

#[test]
fn target_remove_unknown_is_refused() {
    let sb = Sandbox::new();
    sb.add_target("prod");
    sb.golden(
        "target/remove_unknown",
        &["target", "remove", "ghost", "--yes"],
    );
}

#[test]
fn target_machine_no_ping_records_unvalidated() {
    let sb = Sandbox::new();
    sb.add_target("prod");
    sb.golden_steps(
        "target/machine_no_ping",
        &[
            &["target", "machine", "--server-type", "cx32", "--no-ping"],
            &["target", "show"],
        ],
    );
}

/// bug 4: exit 1 with `apprafter::target::not_provisioned`, nothing on stdout.
#[test]
fn target_ip_without_server() {
    let sb = Sandbox::new();
    sb.add_target("prod");
    sb.golden("target/ip_no_server", &["target", "ip"]);
}

#[test]
fn target_add_server_type_no_ping() {
    let sb = Sandbox::new();
    let key = sb.ssh_key();
    sb.golden(
        "target/add_server_type_no_ping",
        &[
            "target",
            "add",
            "prod",
            "--provider",
            "hetzner-cloud",
            "--token",
            TOKEN_A,
            "--ssh-key",
            &key,
            "--region",
            "nbg1",
            "--server-type",
            "cx32",
            "--no-ping",
            "--no-interactive",
        ],
    );
}

// ---------------------------------------------------------------------
// Hetzner-backed (mockito)
// ---------------------------------------------------------------------

#[test]
fn target_add_with_ping_verifies_token() {
    let mut server = mockito::Server::new();
    let _loc = json_mock(&mut server, "/v1/locations", 200, LOCATIONS_OK, TOKEN_A);
    let sb = Sandbox::new().with_hcloud(server.url());
    let key = sb.ssh_key();
    sb.golden(
        "target/add_ping_ok",
        &[
            "target",
            "add",
            "prod",
            "--provider",
            "hetzner-cloud",
            "--token",
            TOKEN_A,
            "--ssh-key",
            &key,
            "--region",
            "nbg1",
            "--tier",
            "solo",
            "--no-interactive",
        ],
    );
}

#[test]
fn target_add_with_rejected_token() {
    let mut server = mockito::Server::new();
    let _loc = json_mock(&mut server, "/v1/locations", 401, UNAUTHORIZED, TOKEN_A);
    let sb = Sandbox::new().with_hcloud(server.url());
    let key = sb.ssh_key();
    sb.golden(
        "target/add_ping_rejected",
        &[
            "target",
            "add",
            "prod",
            "--provider",
            "hetzner-cloud",
            "--token",
            TOKEN_A,
            "--ssh-key",
            &key,
            "--region",
            "nbg1",
            "--no-interactive",
        ],
    );
}

#[test]
fn target_add_with_validated_server_type() {
    let mut server = mockito::Server::new();
    let _loc = json_mock(&mut server, "/v1/locations", 200, LOCATIONS_OK, TOKEN_A);
    let st = json_route(&mut server, "/v1/server_types", 200, SERVER_TYPES, TOKEN_A)
        .expect(1)
        .create();
    let sb = Sandbox::new().with_hcloud(server.url());
    let key = sb.ssh_key();
    sb.golden(
        "target/add_server_type_ok",
        &[
            "target",
            "add",
            "prod",
            "--provider",
            "hetzner-cloud",
            "--token",
            TOKEN_A,
            "--ssh-key",
            &key,
            "--region",
            "nbg1",
            "--server-type",
            "cx32",
            "--no-interactive",
        ],
    );
    st.assert();
}

#[test]
fn target_add_with_unknown_server_type() {
    let mut server = mockito::Server::new();
    let _loc = json_mock(&mut server, "/v1/locations", 200, LOCATIONS_OK, TOKEN_A);
    let st = json_route(&mut server, "/v1/server_types", 200, SERVER_TYPES, TOKEN_A)
        .expect(1)
        .create();
    let sb = Sandbox::new().with_hcloud(server.url());
    let key = sb.ssh_key();
    sb.golden(
        "target/add_server_type_unknown",
        &[
            "target",
            "add",
            "prod",
            "--provider",
            "hetzner-cloud",
            "--token",
            TOKEN_A,
            "--ssh-key",
            &key,
            "--region",
            "nbg1",
            "--server-type",
            "cx99",
            "--no-interactive",
        ],
    );
    st.assert();
}

#[test]
fn target_machine_validated_server_type() {
    let mut server = mockito::Server::new();
    let _st = json_mock(&mut server, "/v1/server_types", 200, SERVER_TYPES, TOKEN_A);
    let sb = Sandbox::new().with_hcloud(server.url());
    sb.add_target("prod");
    sb.golden(
        "target/machine_validated",
        &["target", "machine", "--server-type", "cx22"],
    );
}

#[test]
fn target_machine_unknown_server_type() {
    let mut server = mockito::Server::new();
    let _st = json_mock(&mut server, "/v1/server_types", 200, SERVER_TYPES, TOKEN_A);
    let sb = Sandbox::new().with_hcloud(server.url());
    sb.add_target("prod");
    sb.golden(
        "target/machine_unknown_sku",
        &["target", "machine", "--server-type", "cx99"],
    );
}

#[test]
fn whoami_with_ping() {
    let mut server = mockito::Server::new();
    let _loc = json_mock(&mut server, "/v1/locations", 200, LOCATIONS_OK, TOKEN_A);
    let sb = Sandbox::new().with_hcloud(server.url());
    sb.add_target("prod");
    sb.golden("session/whoami_ping_ok", &["whoami"]);
}

#[test]
fn whoami_with_rejected_token() {
    let mut server = mockito::Server::new();
    let _loc = json_mock(&mut server, "/v1/locations", 401, UNAUTHORIZED, TOKEN_A);
    let sb = Sandbox::new().with_hcloud(server.url());
    sb.add_target("prod");
    sb.golden("session/whoami_ping_rejected", &["whoami"]);
}

#[test]
fn whoami_no_ping() {
    let sb = Sandbox::new();
    sb.add_target("prod");
    sb.golden("session/whoami_no_ping", &["whoami", "--no-ping"]);
}

#[test]
fn whoami_empty_store() {
    Sandbox::new().golden("session/whoami_empty", &["whoami", "--no-ping"]);
}

// ---------------------------------------------------------------------
// Target resolution, init, version
// ---------------------------------------------------------------------

#[test]
fn version_flag() {
    Sandbox::new().golden("session/version", &["--version"]);
}

#[test]
fn init_reports_its_state_write() {
    let sb = Sandbox::new();
    sb.add_target("prod");
    sb.golden(
        "session/init",
        &[
            "init",
            "--provider",
            "hetzner-cloud",
            "--tier",
            "solo",
            "--region",
            "nbg1",
        ],
    );
}

#[test]
fn init_without_target() {
    Sandbox::new().golden(
        "session/init_no_target",
        &[
            "init",
            "--provider",
            "hetzner-cloud",
            "--tier",
            "solo",
            "--region",
            "nbg1",
        ],
    );
}

#[test]
fn kubeconfig_with_no_target() {
    Sandbox::new().golden("resolve/kubeconfig_no_target", &["kubeconfig"]);
}

#[test]
fn kubeconfig_with_unknown_target() {
    let sb = Sandbox::new();
    sb.add_target("prod");
    sb.golden(
        "resolve/kubeconfig_unknown_target",
        &["kubeconfig", "--target", "ghost"],
    );
}

#[test]
fn status_with_no_target() {
    Sandbox::new().golden("resolve/status_no_target", &["status"]);
}

#[test]
fn app_list_with_no_target() {
    Sandbox::new().golden("resolve/app_list_no_target", &["app", "list"]);
}

// ---------------------------------------------------------------------
// target family — D.3 baselines
//
// Recorded on the binary as it was before D.3 moved any of these paths
// onto `apprafter-core`, bugs included: every later change to one of
// these files is deliberate and reviewed in its own commit.
// ---------------------------------------------------------------------

/// Two targets (`prod` active, then `staging`) and a pointer naming
/// `gone`, which has no target directory.
fn dangling_pointer_sandbox() -> Sandbox {
    let sb = Sandbox::new();
    sb.add_target("prod");
    sb.add_target("staging");
    sb.seed_pointer("gone");
    sb
}

#[test]
fn target_list_with_a_dangling_pointer() {
    let sb = Sandbox::new();
    sb.add_target("prod");
    sb.add_target("staging");
    sb.seed_pointer("gone");
    sb.golden("target/list_dangling", &["target", "list"]);
}

/// D.3d review #5: a hand edit with no space after the colon (`hetzner_token:<token>`) makes
/// the whole line one YAML scalar, which serde quotes when it refuses it. The error names the
/// file and where in it, never what it holds.
#[test]
fn target_show_and_whoami_with_credentials_missing_the_space_after_the_colon() {
    let sb = Sandbox::new();
    sb.add_target("prod");
    sb.seed_store_file(
        "targets/prod/credentials.yaml",
        &format!("hetzner_token:{TOKEN_A}\n"),
    );
    let steps: &[&[&str]] = &[&["target", "show"], &["whoami", "--no-ping"]];
    sb.assert_steps_never_print(steps, TOKEN_A);
    sb.golden_steps("target/show_credentials_no_space", steps);
}

/// The same for a credentials file that holds only the token, as `echo $TOKEN > …` leaves it.
#[test]
fn target_show_with_credentials_that_hold_only_the_token() {
    let sb = Sandbox::new();
    sb.add_target("prod");
    sb.seed_store_file("targets/prod/credentials.yaml", &format!("{TOKEN_A}\n"));
    let steps: &[&[&str]] = &[&["target", "show"]];
    sb.assert_steps_never_print(steps, TOKEN_A);
    sb.golden_steps("target/show_credentials_bare_token", steps);
}

/// D.3d follow-up: the store's own `config.yaml` belongs to no target, so no removal or re-add
/// repairs it. The help names it, says to fix it by hand or restore it, and that deleting it and
/// choosing the default again (`target use`, which then writes it) works too.
#[test]
fn target_list_and_use_with_an_unreadable_store_config() {
    let sb = Sandbox::new();
    sb.add_target("prod");
    sb.seed_store_file("config.yaml", "active_target: [prod\n");
    sb.golden_steps(
        "target/list_store_config_unreadable",
        &[&["target", "list"], &["target", "use", "prod"]],
    );
    // What the help says works: with the file deleted, `target use` writes it again.
    sb.clear_pointer();
    sb.cmd(&["target", "use", "prod"]).assert().success();
    assert_eq!(
        fs::read_to_string(sb.path("apprafter-config/config.yaml")).unwrap(),
        "active_target: prod\nversion: 1\n"
    );
}

#[test]
fn target_show_with_a_dangling_pointer() {
    dangling_pointer_sandbox().golden("target/show_dangling", &["target", "show"]);
}

#[test]
fn target_use_with_a_dangling_pointer() {
    dangling_pointer_sandbox().golden_steps(
        "target/use_dangling",
        &[&["target", "use", "prod"], &["target", "list"]],
    );
}

#[test]
fn target_remove_with_a_dangling_pointer() {
    dangling_pointer_sandbox().golden_steps(
        "target/remove_dangling",
        &[&["target", "remove", "prod", "--yes"], &["target", "list"]],
    );
}

#[test]
fn target_machine_with_a_dangling_pointer() {
    dangling_pointer_sandbox().golden(
        "target/machine_dangling",
        &["target", "machine", "--server-type", "cx32", "--no-ping"],
    );
}

#[test]
fn whoami_with_a_dangling_pointer() {
    dangling_pointer_sandbox().golden("session/whoami_dangling", &["whoami", "--no-ping"]);
}

#[test]
fn target_ip_with_a_dangling_pointer() {
    dangling_pointer_sandbox().golden("target/ip_dangling", &["target", "ip"]);
}

#[test]
fn target_use_without_a_config_file() {
    let sb = Sandbox::new();
    sb.add_target("prod");
    sb.clear_pointer();
    sb.golden_steps(
        "target/use_no_config",
        &[&["target", "use", "prod"], &["target", "list"]],
    );
}

#[test]
fn target_use_of_the_active_target() {
    let sb = Sandbox::new();
    sb.add_target("prod");
    sb.golden("target/use_already_active", &["target", "use", "prod"]);
}

#[test]
fn target_add_with_the_token_from_the_environment() {
    let sb = Sandbox::new().with_env("HCLOUD_TOKEN", TOKEN_A);
    let key = sb.ssh_key();
    sb.golden_steps(
        "target/add_env_token",
        &[
            &[
                "target",
                "add",
                "prod",
                "--provider",
                "hetzner-cloud",
                "--ssh-key",
                &key,
                "--no-ping",
                "--no-interactive",
            ],
            &["target", "show"],
        ],
    );
}

#[test]
fn target_add_with_the_api_unreachable() {
    let sb = Sandbox::new();
    let key = sb.ssh_key();
    sb.golden(
        "target/add_api_unreachable",
        &[
            "target",
            "add",
            "prod",
            "--provider",
            "hetzner-cloud",
            "--token",
            TOKEN_A,
            "--ssh-key",
            &key,
            "--no-interactive",
        ],
    );
}

#[test]
fn target_add_with_a_malformed_token() {
    Sandbox::new().golden(
        "target/add_malformed_token",
        &[
            "target",
            "add",
            "prod",
            "--provider",
            "hetzner-cloud",
            "--token",
            "short",
            "--no-ping",
            "--no-interactive",
        ],
    );
}

#[test]
fn target_add_with_an_unknown_provider() {
    Sandbox::new().golden(
        "target/add_unknown_provider",
        &[
            "target",
            "add",
            "prod",
            "--provider",
            "aws",
            "--token",
            TOKEN_A,
            "--no-ping",
            "--no-interactive",
        ],
    );
}

#[test]
fn target_add_with_a_missing_ssh_key() {
    let sb = Sandbox::new();
    let missing = sb.path("home/missing.pub").display().to_string();
    sb.golden(
        "target/add_missing_ssh_key",
        &[
            "target",
            "add",
            "prod",
            "--provider",
            "hetzner-cloud",
            "--token",
            TOKEN_A,
            "--ssh-key",
            &missing,
            "--no-ping",
            "--no-interactive",
        ],
    );
}

#[test]
fn target_add_server_type_without_a_region() {
    let mut server = mockito::Server::new();
    let _loc = json_mock(&mut server, "/v1/locations", 200, LOCATIONS_OK, TOKEN_A);
    let _st = json_mock(&mut server, "/v1/server_types", 200, SERVER_TYPES, TOKEN_A);
    let sb = Sandbox::new().with_hcloud(server.url());
    let key = sb.ssh_key();
    sb.golden_steps(
        "target/add_server_type_default_region",
        &[
            &[
                "target",
                "add",
                "prod",
                "--provider",
                "hetzner-cloud",
                "--token",
                TOKEN_A,
                "--ssh-key",
                &key,
                "--server-type",
                "cx32",
                "--no-interactive",
            ],
            &["target", "show"],
        ],
    );
}

#[test]
fn target_add_force_on_an_inactive_target() {
    let sb = Sandbox::new();
    sb.add_target("prod");
    sb.add_target("staging");
    sb.golden_steps(
        "target/add_force_inactive",
        &[
            &[
                "target",
                "add",
                "staging",
                "--force",
                "--provider",
                "hetzner-cloud",
                "--token",
                TOKEN_B,
                "--no-ping",
                "--no-interactive",
            ],
            &["target", "show", "staging"],
        ],
    );
}

/// Bug 8: `--force` reset the Cloudflare origin firewall toggle (no `target add` flag sets it),
/// so the next `apply` reopened 80/443. It is carried over now, with every field not passed.
#[test]
fn target_add_force_keeps_a_seeded_firewall_toggle() {
    let sb = Sandbox::new();
    sb.add_target("prod");
    sb.seed_config(
        "prod",
        "provider: hetzner-cloud\nregion: nbg1\ndefault_tier: solo\nfirewall:\n  cloudflare_origin: true\n",
    );
    sb.golden_with_files(
        "target/add_force_firewall",
        &[&[
            "target",
            "add",
            "prod",
            "--force",
            "--provider",
            "hetzner-cloud",
            "--token",
            TOKEN_B,
            "--no-ping",
            "--no-interactive",
        ]],
        &["targets/prod/config.yaml"],
    );
    let cfg = fs::read_to_string(sb.path("apprafter-config/targets/prod/config.yaml")).unwrap();
    assert!(cfg.contains("cloudflare_origin: true"), "{cfg}");
    assert!(cfg.contains("default_tier: solo"), "{cfg}");
}

/// Bug 8: on a target whose state records a server, `--force --region <other>` is refused
/// (the guard `target machine` uses); nothing is written.
#[test]
fn target_add_force_region_on_a_provisioned_target() {
    let sb = Sandbox::new();
    sb.add_target("prod");
    sb.seed_state("prod", PROVISIONED_STATE);
    sb.golden_with_files(
        "target/add_force_provisioned",
        &[
            &[
                "target",
                "add",
                "prod",
                "--force",
                "--provider",
                "hetzner-cloud",
                "--token",
                TOKEN_B,
                "--region",
                "hel1",
                "--no-ping",
                "--no-interactive",
            ],
            &["target", "show"],
        ],
        &[
            "targets/prod/config.yaml",
            "state/prod/.apprafter/state.json",
        ],
    );
}

/// Bug 8: a `--force` that changes only the token is allowed on a provisioned target.
#[test]
fn target_add_force_keeps_a_provisioned_target_when_only_the_token_changes() {
    let sb = Sandbox::new();
    sb.add_target("prod");
    sb.seed_state("prod", PROVISIONED_STATE);
    sb.golden_steps(
        "target/add_force_provisioned_token_only",
        &[
            &[
                "target",
                "add",
                "prod",
                "--provider",
                "hetzner-cloud",
                "--token",
                TOKEN_B,
                "--force",
                "--no-ping",
                "--no-interactive",
            ],
            &["target", "show"],
        ],
    );
}

#[test]
fn target_renew_with_a_verified_token() {
    let mut server = mockito::Server::new();
    let _loc = json_mock(&mut server, "/v1/locations", 200, LOCATIONS_OK, TOKEN_B);
    let sb = Sandbox::new().with_hcloud(server.url());
    sb.add_target("prod");
    sb.golden(
        "target/renew_ping_ok",
        &[
            "target",
            "add",
            "prod",
            "--renew",
            "--token",
            TOKEN_B,
            "--no-interactive",
        ],
    );
}

#[test]
fn target_renew_with_a_rejected_token() {
    let mut server = mockito::Server::new();
    let _loc = json_mock(&mut server, "/v1/locations", 401, UNAUTHORIZED, TOKEN_B);
    let sb = Sandbox::new().with_hcloud(server.url());
    sb.add_target("prod");
    sb.golden(
        "target/renew_ping_rejected",
        &[
            "target",
            "add",
            "prod",
            "--renew",
            "--token",
            TOKEN_B,
            "--no-interactive",
        ],
    );
}

#[test]
fn target_renew_of_a_missing_target() {
    let sb = Sandbox::new();
    sb.add_target("prod");
    sb.golden(
        "target/renew_missing",
        &[
            "target",
            "add",
            "ghost",
            "--renew",
            "--token",
            TOKEN_B,
            "--no-ping",
            "--no-interactive",
        ],
    );
}

#[test]
fn target_renew_with_config_flags() {
    let sb = Sandbox::new();
    sb.add_target("prod");
    sb.golden(
        "target/renew_config_flags",
        &[
            "target",
            "add",
            "prod",
            "--renew",
            "--token",
            TOKEN_B,
            "--region",
            "hel1",
            "--no-ping",
            "--no-interactive",
        ],
    );
}

#[test]
fn target_renew_with_a_server_type() {
    let sb = Sandbox::new();
    sb.add_target("prod");
    sb.golden_steps(
        "target/renew_server_type",
        &[
            &[
                "target",
                "add",
                "prod",
                "--renew",
                "--token",
                TOKEN_B,
                "--server-type",
                "cx32",
                "--no-ping",
                "--no-interactive",
            ],
            &["target", "show"],
        ],
    );
}

/// A second readable public key in the sandbox, other than the one `add_target` stores.
fn work_key(sb: &Sandbox) -> String {
    let p = sb.path("home/work.pub");
    fs::write(&p, "ssh-ed25519 AAAA golden work key\n").expect("ssh key");
    p.display().to_string()
}

/// `prod`'s credentials file, hand-written: a comment the CLI never writes, so a renewal that
/// rewrote the file would show.
fn hand_written_credentials(sb: &Sandbox) -> String {
    let creds = format!("# pasted by hand\nhetzner_token: {TOKEN_A}\n");
    sb.seed_store_file("targets/prod/credentials.yaml", &creds);
    creds
}

fn credentials_of_prod(sb: &Sandbox) -> String {
    fs::read_to_string(sb.path("apprafter-config/targets/prod/credentials.yaml")).unwrap()
}

/// WI-452: `--renew --ssh-key` with no token changes only the key: no ping (the provider is a
/// closed port and the ping is not skipped), the credentials file untouched, byte for byte.
#[test]
fn target_renew_with_only_an_ssh_key() {
    let sb = Sandbox::new();
    sb.add_target("prod");
    let creds = hand_written_credentials(&sb);
    let key = work_key(&sb);
    sb.golden_with_files(
        "target/renew_ssh_key_only",
        &[&[
            "target",
            "add",
            "prod",
            "--renew",
            "--ssh-key",
            &key,
            "--no-interactive",
        ]],
        &["targets/prod/config.yaml"],
    );
    assert_eq!(credentials_of_prod(&sb), creds);
}

/// WI-452: an `HCLOUD_TOKEN` holding the stored token is no new token: beside `--ssh-key` it is
/// the key-only renewal, not the unchanged-token refusal.
#[test]
fn target_renew_with_an_ssh_key_and_the_stored_token_in_the_env() {
    let sb = Sandbox::new().with_env("HCLOUD_TOKEN", TOKEN_A);
    sb.add_target("prod");
    let creds = hand_written_credentials(&sb);
    let key = work_key(&sb);
    sb.golden_with_files(
        "target/renew_ssh_key_env_token_unchanged",
        &[&[
            "target",
            "add",
            "prod",
            "--renew",
            "--ssh-key",
            &key,
            "--no-interactive",
        ]],
        &["targets/prod/config.yaml"],
    );
    assert_eq!(credentials_of_prod(&sb), creds);
}

/// WI-452: no token and the stored key: nothing would change, and that is the refusal.
#[test]
fn target_renew_that_would_change_nothing() {
    let sb = Sandbox::new();
    sb.add_target("prod");
    let key = sb.ssh_key();
    sb.golden(
        "target/renew_nothing_to_change",
        &[
            "target",
            "add",
            "prod",
            "--renew",
            "--ssh-key",
            &key,
            "--no-interactive",
        ],
    );
}

/// WI-452: a typed `--token` other than the stored one beside `--ssh-key`: the token is
/// verified (the mock answers only it) and saved, and the key changes with it.
#[test]
fn target_renew_rotates_the_token_and_changes_the_ssh_key() {
    let mut server = mockito::Server::new();
    let _loc = json_mock(&mut server, "/v1/locations", 200, LOCATIONS_OK, TOKEN_B);
    let sb = Sandbox::new().with_hcloud(server.url());
    sb.add_target("prod");
    let key = work_key(&sb);
    sb.golden_with_files(
        "target/renew_token_and_ssh_key",
        &[&[
            "target",
            "add",
            "prod",
            "--renew",
            "--token",
            TOKEN_B,
            "--ssh-key",
            &key,
            "--no-interactive",
        ]],
        &["targets/prod/config.yaml"],
    );
    assert!(credentials_of_prod(&sb).contains(TOKEN_B));
}

/// D.3d review #0/#8: `HCLOUD_TOKEN` is the hcloud CLI's own variable and may hold another
/// project's token. Beside a typed `--ssh-key` with no typed `--token` it is not used: only the
/// key changes (no ping — the provider is a closed port), the credentials stay byte for byte,
/// and one note says the env token was not used and how to rotate as well.
#[test]
fn target_renew_with_an_ssh_key_ignores_another_token_in_the_env() {
    let sb = Sandbox::new().with_env("HCLOUD_TOKEN", TOKEN_B);
    sb.add_target("prod");
    let creds = hand_written_credentials(&sb);
    let key = work_key(&sb);
    sb.golden_with_files(
        "target/renew_ssh_key_env_token_differs",
        &[&[
            "target",
            "add",
            "prod",
            "--renew",
            "--ssh-key",
            &key,
            "--no-interactive",
        ]],
        &["targets/prod/config.yaml"],
    );
    assert_eq!(credentials_of_prod(&sb), creds);
}

/// D.3d review #3: a key from `APPRAFTER_SSH_PUBLIC_KEY_PATH` (set for every command in CI)
/// never makes a renewal key-only by itself: with no token, it is today's refusal, and the
/// stored key stays.
#[test]
fn target_renew_with_only_an_env_ssh_key_still_needs_a_token() {
    let sb = Sandbox::new();
    sb.add_target("prod");
    let key = work_key(&sb);
    let sb = sb.with_env("APPRAFTER_SSH_PUBLIC_KEY_PATH", &key);
    sb.golden_with_files(
        "target/renew_env_ssh_key_no_token",
        &[&["target", "add", "prod", "--renew", "--no-interactive"]],
        &["targets/prod/config.yaml"],
    );
}

/// An env key is applied alongside an env token's rotation, as before WI-452.
#[test]
fn target_renew_with_an_env_token_applies_the_env_ssh_key_too() {
    let mut server = mockito::Server::new();
    let _loc = json_mock(&mut server, "/v1/locations", 200, LOCATIONS_OK, TOKEN_B);
    let sb = Sandbox::new()
        .with_hcloud(server.url())
        .with_env("HCLOUD_TOKEN", TOKEN_B);
    sb.add_target("prod");
    let key = work_key(&sb);
    let sb = sb.with_env("APPRAFTER_SSH_PUBLIC_KEY_PATH", &key);
    sb.golden_with_files(
        "target/renew_env_token_and_env_ssh_key",
        &[&["target", "add", "prod", "--renew", "--no-interactive"]],
        &["targets/prod/config.yaml"],
    );
    assert!(credentials_of_prod(&sb).contains(TOKEN_B));
}

/// D.3d review #3, rule 3: a key from `APPRAFTER_SSH_PUBLIC_KEY_PATH` rides only beside a real
/// rotation. An `HCLOUD_TOKEN` holding the stored token rotates nothing, so the env key does not
/// turn the renewal into a key-only one: it is the unchanged-token refusal, as before key-only
/// renewals existed, and the stored key path and the credentials stay byte for byte.
#[test]
fn target_renew_with_the_stored_token_in_the_env_and_an_env_ssh_key() {
    let sb = Sandbox::new().with_env("HCLOUD_TOKEN", TOKEN_A);
    sb.add_target("prod");
    let creds = hand_written_credentials(&sb);
    let key = work_key(&sb);
    let sb = sb.with_env("APPRAFTER_SSH_PUBLIC_KEY_PATH", &key);
    sb.golden_with_files(
        "target/renew_env_token_unchanged_env_ssh_key",
        &[&["target", "add", "prod", "--renew", "--no-interactive"]],
        &["targets/prod/config.yaml"],
    );
    assert_env_key_not_applied(&sb, &creds);
}

/// The same with the stored token typed: `--token` alone is typed, so the env key still rides
/// only beside a rotation (a typed `--token <stored> --ssh-key <new>` stays key-only).
#[test]
fn target_renew_with_the_stored_token_typed_and_an_env_ssh_key() {
    let sb = Sandbox::new();
    sb.add_target("prod");
    let creds = hand_written_credentials(&sb);
    let key = work_key(&sb);
    let sb = sb.with_env("APPRAFTER_SSH_PUBLIC_KEY_PATH", &key);
    sb.golden_with_files(
        "target/renew_typed_token_unchanged_env_ssh_key",
        &[&[
            "target",
            "add",
            "prod",
            "--renew",
            "--token",
            TOKEN_A,
            "--no-interactive",
        ]],
        &["targets/prod/config.yaml"],
    );
    assert_env_key_not_applied(&sb, &creds);
}

/// Typed, the stored token with a new `--ssh-key` is the key-only renewal (WI-452), whatever the
/// env key says: the typed key changes, the credentials stay byte for byte, nothing is pinged.
#[test]
fn target_renew_with_the_stored_token_and_an_ssh_key_both_typed() {
    let sb = Sandbox::new();
    sb.add_target("prod");
    let creds = hand_written_credentials(&sb);
    let key = work_key(&sb);
    let other = key_file(&sb, "env.pub", "ssh-ed25519 AAAA golden env key\n");
    let sb = sb.with_env("APPRAFTER_SSH_PUBLIC_KEY_PATH", &other);
    sb.golden_with_files(
        "target/renew_typed_token_unchanged_typed_ssh_key",
        &[&[
            "target",
            "add",
            "prod",
            "--renew",
            "--token",
            TOKEN_A,
            "--ssh-key",
            &key,
            "--no-interactive",
        ]],
        &["targets/prod/config.yaml"],
    );
    assert_eq!(credentials_of_prod(&sb), creds);
}

/// `prod` keeps the key it was added with, and its credentials byte for byte.
fn assert_env_key_not_applied(sb: &Sandbox, creds: &str) {
    let config = fs::read_to_string(sb.path("apprafter-config/targets/prod/config.yaml")).unwrap();
    assert!(
        config.contains("id_ed25519.pub") && !config.contains("work.pub"),
        "{config}"
    );
    assert_eq!(credentials_of_prod(sb), creds);
}

/// `--renew` with neither a token nor `--ssh-key` (and no terminal) is today's refusal.
#[test]
fn target_renew_without_a_token_or_a_key() {
    let sb = Sandbox::new();
    sb.add_target("prod");
    sb.golden(
        "target/renew_no_token",
        &["target", "add", "prod", "--renew", "--no-interactive"],
    );
}

/// A private key as `ssh-keygen` writes it next to the `.pub` (OpenSSH's PEM format); the
/// body is a placeholder.
const PRIVATE_KEY: &str = "-----BEGIN OPENSSH PRIVATE KEY-----\nb3BlbnNzaC1rZXktdjEAAAAABG5vbmUAAAAEbm9uZQ==\n-----END OPENSSH PRIVATE KEY-----\n";
/// The private key's body, letters and digits only (`assert_steps_never_print` searches so).
const PRIVATE_KEY_BODY: &str = "b3BlbnNzaC1rZXktdjEAAAAABG5vbmUAAAAEbm9uZQ";

/// `body` at `<sandbox>/home/<name>`.
fn key_file(sb: &Sandbox, name: &str, body: &str) -> String {
    let p = sb.path(&format!("home/{name}"));
    fs::write(&p, body).expect("key file");
    p.display().to_string()
}

/// GOTCHA-149: `--ssh-key` names the file `apply` sends the provider as the target's key, so a
/// private key (the file next to the `.pub`, one dropped suffix away) is refused by name, its
/// text never shown, and nothing is saved.
#[test]
fn target_add_refuses_a_private_key_as_the_ssh_key() {
    let sb = Sandbox::new();
    let key = key_file(&sb, "id_ed25519", PRIVATE_KEY);
    let steps: &[&[&str]] = &[&[
        "target",
        "add",
        "prod",
        "--provider",
        "hetzner-cloud",
        "--token",
        TOKEN_A,
        "--ssh-key",
        &key,
        "--no-ping",
        "--no-interactive",
    ]];
    sb.assert_steps_never_print(steps, PRIVATE_KEY_BODY);
    sb.golden_with_files(
        "target/add_ssh_key_private",
        steps,
        &["targets/prod/config.yaml"],
    );
}

/// The same for a key-only renewal onto an RSA private key in the classic PEM format: the stored
/// key stays.
#[test]
fn target_renew_refuses_a_pem_private_key_as_the_ssh_key() {
    let sb = Sandbox::new();
    sb.add_target("prod");
    let key = key_file(
        &sb,
        "id_rsa",
        "-----BEGIN RSA PRIVATE KEY-----\nMIIEowIBAAKCAQEAgoldenplaceholder\n-----END RSA PRIVATE KEY-----\n",
    );
    let steps: &[&[&str]] = &[&[
        "target",
        "add",
        "prod",
        "--renew",
        "--ssh-key",
        &key,
        "--no-interactive",
    ]];
    sb.assert_steps_never_print(steps, "MIIEowIBAAKCAQEAgoldenplaceholder");
    sb.golden_with_files(
        "target/renew_ssh_key_pem",
        steps,
        &["targets/prod/config.yaml"],
    );
}

/// A file that is not an OpenSSH public key line at all is refused too.
#[test]
fn target_add_refuses_a_file_that_is_not_a_public_key() {
    let sb = Sandbox::new();
    let key = key_file(&sb, "notes.pub", "not-a-key\n");
    sb.golden_with_files(
        "target/add_ssh_key_not_public",
        &[&[
            "target",
            "add",
            "prod",
            "--provider",
            "hetzner-cloud",
            "--token",
            TOKEN_A,
            "--ssh-key",
            &key,
            "--no-ping",
            "--no-interactive",
        ]],
        &["targets/prod/config.yaml"],
    );
}

#[test]
fn target_rename_to_an_existing_name() {
    let sb = Sandbox::new();
    sb.add_target("prod");
    sb.add_target("staging");
    sb.golden(
        "target/rename_to_existing",
        &["target", "rename", "prod", "staging"],
    );
}

#[test]
fn target_rename_to_an_invalid_name() {
    let sb = Sandbox::new();
    sb.add_target("prod");
    sb.golden(
        "target/rename_invalid",
        &["target", "rename", "prod", "bad.name"],
    );
}

#[test]
fn target_rename_to_the_same_name() {
    let sb = Sandbox::new();
    sb.add_target("prod");
    sb.golden(
        "target/rename_identical",
        &["target", "rename", "prod", "prod"],
    );
}

#[test]
fn target_remove_of_a_provisioned_target() {
    let sb = Sandbox::new();
    sb.add_target("prod");
    sb.seed_state("prod", PROVISIONED_STATE);
    sb.golden_with_files(
        "target/remove_provisioned",
        &[&["target", "remove", "prod", "--yes"]],
        &["state/prod/.apprafter/state.json"],
    );
}

/// WI-458: a target whose `config.yaml` cannot be read is removed with `--yes`. The warnings say
/// what cannot be read, and that the server its state records keeps running and cannot be
/// checked or destroyed through it; its directory and state go, and the default moves to the
/// target that can be read.
#[test]
fn target_remove_of_a_target_whose_config_cannot_be_read() {
    let sb = Sandbox::new();
    sb.add_target("prod");
    sb.add_target("staging");
    sb.seed_config("prod", "provider: [hetzner-cloud\n");
    sb.seed_state("prod", PROVISIONED_STATE);
    sb.golden_with_files(
        "target/remove_unreadable",
        &[&["target", "remove", "prod", "--yes"], &["target", "list"]],
        &[
            "targets/prod/config.yaml",
            "targets/prod/credentials.yaml",
            "state/prod/.apprafter/state.json",
        ],
    );
}

/// WI-458: a good `config.yaml` beside a `credentials.yaml` that cannot be read takes the same
/// path, and its text (the token) is never printed: only where it stopped parsing.
#[test]
fn target_remove_of_a_target_whose_credentials_cannot_be_read() {
    let seeded = || {
        let sb = Sandbox::new();
        sb.add_target("prod");
        sb.add_target("staging");
        sb.seed_store_file(
            "targets/prod/credentials.yaml",
            &format!("hetzner_token:{TOKEN_A}\n"),
        );
        sb
    };
    let steps: &[&[&str]] = &[&["target", "remove", "prod", "--yes"]];
    seeded().assert_steps_never_print(steps, TOKEN_A);
    seeded().golden_with_files(
        "target/remove_unreadable_credentials",
        steps,
        &["targets/prod/credentials.yaml"],
    );
}

/// WI-458 (from D.3d): removing the default moves it to the alphabetically first target that
/// can be read — both files — passing over one whose config cannot be read and one whose
/// credentials cannot, and the line names them.
#[test]
fn target_remove_of_the_default_passes_over_targets_that_cannot_be_read() {
    let sb = Sandbox::new();
    for name in ["alpha", "beta", "staging", "prod"] {
        sb.add_target(name);
    }
    sb.seed_pointer("prod");
    sb.seed_config("alpha", "provider: [hetzner-cloud\n");
    sb.seed_store_file("targets/beta/credentials.yaml", "hetzner_token: [\n");
    sb.golden_with_files(
        "target/remove_skips_unreadable",
        &[&["target", "remove", "prod", "--yes"], &["target", "list"]],
        &["config.yaml"],
    );
}

/// WI-458: when no target that can be read is left, the default is cleared and the line says
/// why; the one left can then be removed too.
#[test]
fn target_remove_of_the_default_with_no_readable_target_left() {
    let sb = Sandbox::new();
    sb.add_target("prod");
    sb.add_target("broken");
    sb.seed_config("broken", "provider: [hetzner-cloud\n");
    sb.golden_with_files(
        "target/remove_none_readable",
        &[
            &["target", "remove", "prod", "--yes"],
            &["target", "remove", "broken", "--yes"],
            &["target", "list"],
        ],
        &["config.yaml", "targets/broken/config.yaml"],
    );
}

#[test]
fn target_show_of_a_provisioned_target() {
    let sb = Sandbox::new();
    sb.add_target("prod");
    sb.seed_state("prod", PROVISIONED_STATE);
    sb.golden("target/show_provisioned", &["target", "show"]);
}

#[test]
fn target_machine_on_a_provisioned_target() {
    let sb = Sandbox::new();
    sb.add_target("prod");
    sb.seed_state("prod", PROVISIONED_STATE);
    sb.golden(
        "target/machine_provisioned",
        &["target", "machine", "--server-type", "cx32", "--no-ping"],
    );
}

#[test]
fn target_machine_no_ping_without_a_server_type() {
    let sb = Sandbox::new();
    sb.add_target("prod");
    sb.golden(
        "target/machine_no_ping_no_sku",
        &["target", "machine", "--no-ping"],
    );
}

#[test]
fn target_machine_without_a_tty_or_a_server_type() {
    // stdin is null under `output()`: not a TTY.
    let sb = Sandbox::new();
    sb.add_target("prod");
    sb.golden("target/machine_non_tty_no_sku", &["target", "machine"]);
}

#[test]
fn target_machine_of_a_non_active_target() {
    let mut server = mockito::Server::new();
    let _st = json_mock(&mut server, "/v1/server_types", 200, SERVER_TYPES, TOKEN_A);
    let sb = Sandbox::new().with_hcloud(server.url());
    sb.add_target("prod");
    sb.add_target("staging");
    sb.golden_steps(
        "target/machine_other_target",
        &[
            &[
                "target",
                "machine",
                "--target",
                "staging",
                "--server-type",
                "cx32",
            ],
            &["target", "show", "staging"],
        ],
    );
}

#[test]
fn target_machine_with_the_token_from_the_environment() {
    // The mock answers only TOKEN_B (the environment's); the stored
    // token is TOKEN_A.
    let mut server = mockito::Server::new();
    let _st = json_mock(&mut server, "/v1/server_types", 200, SERVER_TYPES, TOKEN_B);
    let sb = Sandbox::new()
        .with_hcloud(server.url())
        .with_env("HCLOUD_TOKEN", TOKEN_B);
    sb.add_target("prod");
    sb.golden(
        "target/machine_env_token",
        &["target", "machine", "--server-type", "cx32"],
    );
}

#[test]
fn target_ip_with_a_server() {
    let mut server = mockito::Server::new();
    let _server = json_mock(&mut server, "/v1/servers/42", 200, SERVER_42_BODY, TOKEN_A);
    let sb = Sandbox::new().with_hcloud(server.url());
    sb.add_target("prod");
    sb.seed_state("prod", PROVISIONED_STATE);
    sb.golden("target/ip_with_server", &["target", "ip"]);
}

#[test]
fn target_ip_with_the_server_absent() {
    let mut server = mockito::Server::new();
    let _server = json_mock(
        &mut server,
        "/v1/servers/42",
        404,
        SERVER_NOT_FOUND,
        TOKEN_A,
    );
    let sb = Sandbox::new().with_hcloud(server.url());
    sb.add_target("prod");
    sb.seed_state("prod", PROVISIONED_STATE);
    sb.golden("target/ip_server_absent", &["target", "ip"]);
}

#[test]
fn target_ip_without_a_stored_token() {
    let sb = Sandbox::new();
    sb.add_target("prod");
    sb.seed_state("prod", PROVISIONED_STATE);
    sb.seed_store_file("targets/prod/credentials.yaml", "{}\n");
    sb.golden("target/ip_no_token", &["target", "ip"]);
}

#[test]
fn whoami_with_the_api_unreachable() {
    let sb = Sandbox::new();
    sb.add_target("prod");
    sb.golden("session/whoami_unreachable", &["whoami"]);
}

#[test]
fn whoami_with_the_ssh_key_file_missing() {
    let sb = Sandbox::new();
    sb.add_target("prod");
    fs::remove_file(sb.path("home/id_ed25519.pub")).expect("remove the ssh key");
    sb.golden("session/whoami_ssh_key_missing", &["whoami", "--no-ping"]);
}

// ---------------------------------------------------------------------
// kubeconfig — a lost age key (WI-457, GOTCHA-120, GOTCHA-121)
//
// `prod` and `staging` cache their kubeconfig and Argo CD password under a key that is gone
// (made here, never written). The node is a stand-in `ssh` on an otherwise empty `PATH` that
// logs its arguments and prints a k3s kubeconfig; the server's address comes from the mocked
// Hetzner API. Nothing reaches a real node, cluster or provider.
// ---------------------------------------------------------------------

/// What the stand-in node serves at `/etc/rancher/k3s/k3s.yaml`.
const NODE_KUBECONFIG: &str =
    "apiVersion: v1\nclusters:\n- cluster:\n    server: https://127.0.0.1:6443\n  name: default\n";

/// `GET /v1/servers`: the server `prod`'s state records, labelled and addressed.
const SERVERS_42: &str = r#"{"servers":[{"id":42,"name":"prod-node","status":"running","labels":{"apprafter":"true"},"public_net":{"ipv4":{"ip":"203.0.113.10"}}}]}"#;

/// `prod` (active) and `staging`, each caching both secrets under `lost`, which is on no disk.
fn lost_key_sandbox(server: &mockito::Server) -> (Sandbox, age::x25519::Identity) {
    let lost = age::x25519::Identity::generate();
    let enc = |text: &str| {
        cli_core::secrets::encrypt_for_recipient(text, &lost.to_public()).expect("encrypt")
    };
    let sb = Sandbox::new().with_hcloud(server.url());
    sb.add_target("prod");
    sb.add_target("staging");
    for (target, id) in [("prod", 42), ("staging", 43)] {
        sb.seed_state(
            target,
            &serde_json::json!({"hetzner_cloud": {
                "server_id": id, "server_name": format!("{target}-node"),
                "kubeconfig_age": enc("from: the lost key\n"),
                "argocd_admin_password_age": enc("lost-password"),
            }})
            .to_string(),
        );
    }
    (sb, lost)
}

/// The stand-ins, alone on `PATH`: the node — `ssh … root@203.0.113.10 cat
/// /etc/rancher/k3s/k3s.yaml` prints [`NODE_KUBECONFIG`] — and the cluster — `kubectl get secret
/// argocd-initial-admin-secret -n argocd …` prints the password `new-password`, base64 as the
/// API holds it. Each logs its calls to `<sandbox>/<tool>.log`; anything else exits non-zero.
fn stand_in_tools(sb: &Sandbox) -> PathBuf {
    let bin = sb.path("tools-bin");
    fs::create_dir_all(&bin).expect("tools bin");
    let body: String = NODE_KUBECONFIG.lines().map(|l| format!(" '{l}'")).collect();
    for (tool, call, answer) in [
        (
            "ssh",
            "*' root@203.0.113.10 cat /etc/rancher/k3s/k3s.yaml'",
            format!("printf '%s\\n'{body}"),
        ),
        (
            "kubectl",
            "'get secret argocd-initial-admin-secret -n argocd -o jsonpath={.data.password}'",
            "printf '%s' 'bmV3LXBhc3N3b3Jk'".to_string(),
        ),
    ] {
        common::stand_in::script(
            &bin.join(tool),
            &format!(
                "echo \"$*\" >> '{log}'\n\
                 case \"$*\" in\n\
                 {call}) {answer} ;;\n\
                 *) echo \"{tool} stand-in: unexpected arguments: $*\" >&2; exit 2 ;;\n\
                 esac",
                log = sb.path(&format!("{tool}.log")).display()
            ),
        );
    }
    bin
}

fn age_key(sb: &Sandbox) -> PathBuf {
    sb.path("home/.config/apprafter/age.key")
}

fn hetzner_state(sb: &Sandbox, target: &str) -> serde_json::Value {
    let raw = fs::read_to_string(sb.path(&format!(
        "apprafter-config/state/{target}/.apprafter/state.json"
    )))
    .expect("state");
    serde_json::from_str::<serde_json::Value>(&raw).expect("json")["hetzner_cloud"].clone()
}

/// GOTCHA-120: a read with the key gone names the way back, and never creates a key.
#[test]
fn kubeconfig_with_the_age_key_lost() {
    let server = mockito::Server::new();
    let (sb, _lost) = lost_key_sandbox(&server);
    let before = hetzner_state(&sb, "prod");
    sb.golden("kubeconfig/age_key_lost", &["kubeconfig"]);
    assert!(!age_key(&sb).exists(), "a read never creates a key");
    assert_eq!(hetzner_state(&sb, "prod"), before);
}

/// Without a terminal and without `--yes`, `--refresh` lists what a new key leaves unreadable
/// and refuses: no key, no read of the node, no write. The node is reachable — the address is
/// mocked and the stand-in `ssh` answers — so a bypassed consent would read it, and both the
/// address lookup and the `ssh` call are asserted absent on their own (review #4).
#[test]
fn kubeconfig_refresh_with_the_age_key_lost_needs_yes() {
    let mut server = mockito::Server::new();
    let servers = json_route(&mut server, "/v1/servers", 200, SERVERS_42, TOKEN_A)
        .expect(0)
        .create();
    let (sb, _lost) = lost_key_sandbox(&server);
    let tools = stand_in_tools(&sb);
    let sb = sb.with_path(tools);
    let before = hetzner_state(&sb, "prod");
    sb.golden(
        "kubeconfig/age_key_lost_refresh_needs_yes",
        &["kubeconfig", "--refresh"],
    );
    servers.assert();
    assert!(!age_key(&sb).exists(), "no key without consent");
    assert!(!sb.path("ssh.log").exists(), "the node is not read");
    assert_eq!(hetzner_state(&sb, "prod"), before);
}

/// Review #5/#12: at a terminal, no, Esc and Ctrl-C at the new-key question are one refusal: exit
/// 1 with `apprafter::secrets::new_key_declined`, nothing on stdout — the kubeconfig's channel,
/// redirected to a file as in `apprafter kubeconfig > kc && …` — and nothing created, read or
/// written. stdin and stderr are a pseudo-terminal, as the question needs; stdout is a pipe.
#[test]
fn a_declined_new_age_key_exits_non_zero_with_nothing_on_stdout() {
    let mut server = mockito::Server::new();
    let servers = json_route(&mut server, "/v1/servers", 200, SERVERS_42, TOKEN_A)
        .expect(0)
        .create();
    let (sb, _lost) = lost_key_sandbox(&server);
    let tools = stand_in_tools(&sb);
    let sb = sb.with_path(tools);
    let before = hetzner_state(&sb, "prod");
    for (how, keys) in [("no", "n\r"), ("Esc", "\x1b"), ("Ctrl-C", "\x03")] {
        let run = run_at_a_terminal(
            sb.std_cmd(&["kubeconfig", "--refresh"]),
            "Create a new age key?",
            keys.as_bytes(),
        );
        assert_eq!(run.status.code(), Some(1), "{how}:\n{}", run.terminal);
        assert_eq!(run.stdout, "", "{how}: stdout is the kubeconfig's");
        assert!(
            run.terminal
                .contains("apprafter::secrets::new_key_declined"),
            "{how}:\n{}",
            run.terminal
        );
    }
    servers.assert();
    assert!(!age_key(&sb).exists(), "no key after a decline");
    assert!(!sb.path("ssh.log").exists(), "the node is not read");
    assert_eq!(hetzner_state(&sb, "prod"), before);
}

/// What a child run at a pseudo-terminal left: its exit, its stdout (a pipe), and everything it
/// drew on the terminal (stderr, the prompt included).
struct TerminalRun {
    status: std::process::ExitStatus,
    stdout: String,
    terminal: String,
}

/// Run `cmd` with stdin and stderr on a pseudo-terminal of 24x100 and stdout on a pipe; once
/// the terminal shows `prompt`, type `keys`. Panics if the prompt or the exit takes over 30 s.
fn run_at_a_terminal(mut cmd: std::process::Command, prompt: &str, keys: &[u8]) -> TerminalRun {
    use std::io::{Read, Write};
    use std::os::fd::{FromRawFd, OwnedFd};
    use std::process::Stdio;
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    // posix_openpt, not openpty: it is in every libc without -lutil.
    // SAFETY: plain libc calls on fds this function owns; `ptsname`'s static buffer is copied
    // before anything else runs on this thread.
    let (master, slave) = unsafe {
        let m = libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY);
        assert!(m >= 0, "posix_openpt: {}", std::io::Error::last_os_error());
        assert_eq!(libc::grantpt(m), 0, "grantpt");
        assert_eq!(libc::unlockpt(m), 0, "unlockpt");
        let name = libc::ptsname(m);
        assert!(!name.is_null(), "ptsname");
        let name = std::ffi::CStr::from_ptr(name).to_owned();
        let ws = libc::winsize {
            ws_row: 24,
            ws_col: 100,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        assert_eq!(libc::ioctl(m, libc::TIOCSWINSZ, &ws), 0, "TIOCSWINSZ");
        let s = libc::open(name.as_ptr(), libc::O_RDWR | libc::O_NOCTTY);
        assert!(
            s >= 0,
            "open the slave: {}",
            std::io::Error::last_os_error()
        );
        (OwnedFd::from_raw_fd(m), OwnedFd::from_raw_fd(s))
    };
    let mut child = cmd
        .stdin(Stdio::from(slave.try_clone().expect("dup slave")))
        .stderr(Stdio::from(slave))
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn apprafter");
    // The command holds the slave's fds until it drops: drop it, so the terminal closes when
    // the child exits and the reader below sees the end.
    drop(cmd);
    let mut stdout = child.stdout.take().expect("stdout");
    let out_reader = std::thread::spawn(move || {
        let mut out = String::new();
        stdout.read_to_string(&mut out).expect("read stdout");
        out
    });
    let mut writer = std::fs::File::from(master.try_clone().expect("dup master"));
    let mut reader = std::fs::File::from(master);
    let seen = Arc::new(Mutex::new(Vec::<u8>::new()));
    let sink = Arc::clone(&seen);
    let (closed, drained) = std::sync::mpsc::channel::<()>();
    // Reads until the child's side closes (EIO on Linux, EOF elsewhere).
    std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        while let Ok(n) = reader.read(&mut buf) {
            if n == 0 {
                break;
            }
            sink.lock().unwrap().extend_from_slice(&buf[..n]);
        }
        let _ = closed.send(());
    });
    let text =
        |seen: &Arc<Mutex<Vec<u8>>>| String::from_utf8_lossy(&seen.lock().unwrap()).into_owned();
    let deadline = Instant::now() + Duration::from_secs(30);
    while !text(&seen).contains(prompt) {
        if let Some(status) = child.try_wait().expect("try_wait") {
            panic!("exited {status} before asking:\n{}", text(&seen));
        }
        assert!(
            Instant::now() < deadline,
            "no prompt within 30 s:\n{}",
            text(&seen)
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    writer.write_all(keys).expect("type");
    let status = loop {
        if let Some(status) = child.try_wait().expect("try_wait") {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            panic!("no exit within 30 s:\n{}", text(&seen));
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let stdout = out_reader.join().expect("stdout reader");
    drained
        .recv_timeout(Duration::from_secs(10))
        .expect("the terminal closes once the child has exited");
    TerminalRun {
        status,
        stdout,
        terminal: text(&seen),
    }
}

/// GOTCHA-121: `--refresh --yes` reads the node over SSH (only that: `cat` of the kubeconfig),
/// caches it under a new key, drops `prod`'s password cached under the lost key, and says what
/// is left: `staging`'s caches, each with its way back. The cache then reads under the new key,
/// `staging`'s names its refetch, and `argocd-password` fetches the dropped password from the
/// cluster again, caching it under the new key.
#[test]
fn kubeconfig_refresh_yes_recovers_from_a_lost_age_key() {
    let mut server = mockito::Server::new();
    let servers = json_route(&mut server, "/v1/servers", 200, SERVERS_42, TOKEN_A)
        .expect(1)
        .create();
    let (sb, _lost) = lost_key_sandbox(&server);
    let tools = stand_in_tools(&sb);
    let sb = sb.with_path(tools);
    let staging_before = hetzner_state(&sb, "staging");
    sb.golden_steps(
        "kubeconfig/age_key_lost_refresh_yes",
        &[
            &["kubeconfig", "--refresh", "--yes"],
            &["kubeconfig"],
            &["kubeconfig", "--target", "staging"],
            &["argocd-password"],
        ],
    );
    servers.assert();
    let calls = |tool: &str| fs::read_to_string(sb.path(&format!("{tool}.log"))).expect(tool);
    let ssh = calls("ssh");
    assert_eq!(ssh.lines().count(), 1, "{ssh}");
    assert!(
        ssh.trim_end()
            .ends_with(" root@203.0.113.10 cat /etc/rancher/k3s/k3s.yaml"),
        "{ssh}"
    );
    assert_eq!(
        calls("kubectl").lines().count(),
        1,
        "the password is read once"
    );
    let key = cli_core::secrets::load_identity(&age_key(&sb))
        .expect("readable")
        .expect("a new key");
    let prod = hetzner_state(&sb, "prod");
    let open = |slot: &str| {
        cli_core::secrets::decrypt_with_identity(prod[slot].as_str().expect(slot), &key)
            .expect("cached under the new key")
    };
    assert_eq!(
        open("kubeconfig_age"),
        NODE_KUBECONFIG.replace("127.0.0.1", "203.0.113.10")
    );
    assert_eq!(open("argocd_admin_password_age"), "new-password");
    assert_eq!(hetzner_state(&sb, "staging"), staging_before);
}

/// A legacy plaintext kubeconfig needs no key, but the password fetched with it does: with
/// another target's cache under a lost key, `argocd-password` refuses to create one (only
/// `kubeconfig --refresh` does, after asking), before it writes anything.
#[test]
fn argocd_password_beside_a_lost_age_key_creates_none() {
    let server = mockito::Server::new();
    let (sb, _lost) = lost_key_sandbox(&server);
    sb.seed_state(
        "prod",
        &serde_json::json!({"hetzner_cloud": {
            "server_id": 42, "server_name": "prod-node", "kubeconfig_yaml": NODE_KUBECONFIG,
        }})
        .to_string(),
    );
    let tools = stand_in_tools(&sb);
    let sb = sb.with_path(tools);
    let before = hetzner_state(&sb, "prod");
    sb.golden(
        "kubeconfig/age_key_lost_password_with_plaintext_kubeconfig",
        &["argocd-password"],
    );
    assert!(!age_key(&sb).exists(), "no key without asking");
    assert_eq!(hetzner_state(&sb, "prod"), before);
}

/// GOTCHA-120 on every other command that reads a cache: each fails naming the way back, and
/// none creates a key — not the password's read, not `--refresh` (which still needs the
/// kubeconfig), not a command that talks to the cluster.
#[test]
fn reads_of_a_cache_with_the_age_key_lost() {
    let server = mockito::Server::new();
    let (sb, _lost) = lost_key_sandbox(&server);
    let before = hetzner_state(&sb, "prod");
    sb.golden_steps(
        "kubeconfig/age_key_lost_reads",
        &[
            &["argocd-password"],
            &["argocd-password", "--refresh"],
            &["app", "list"],
            &["cluster-bootstrap"],
        ],
    );
    assert!(!age_key(&sb).exists(), "a read never creates a key");
    assert_eq!(hetzner_state(&sb, "prod"), before);
}
