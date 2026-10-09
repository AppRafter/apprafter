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
    for name in cli_core::tools::ALL.iter().map(|t| t.name).chain(["cue"]) {
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

#[test]
fn target_add_force_drops_a_seeded_firewall_toggle() {
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
}

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
