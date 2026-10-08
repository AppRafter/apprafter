// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Golden snapshots of `apprafter` output — Phase D, D.1 Part A.
//!
//! D.1 moves the CLI onto a shared `apprafter-core` crate (ADR 0067). Its
//! invariant is that, on Unix, the CLI prints exactly what it printed
//! before, except for changes made on purpose. These cases pin that
//! byte for byte.
//!
//! Every case runs the shipped binary in a hermetic sandbox:
//! - a cleared environment, so the caller's env cannot change the result;
//! - scratch `HOME`, XDG dirs and `APPRAFTER_CONFIG_DIR`;
//! - `PATH` is one empty directory, so no real tool can run;
//! - startup checks off and `RUST_LOG=off`, because tracing lines carry
//!   timestamps;
//! - a `mockito` server for every Hetzner call, so nothing reaches the
//!   network.
//!
//! stdout and stderr are captured separately (non-TTY) and rendered with
//! the command and exit code into one `tests/golden/<family>/<case>.golden`.
//!
//! The only normalisations are deterministic substitutions:
//! - the sandbox path becomes `<SANDBOX>`;
//! - the mockito URL becomes `<HCLOUD>`;
//! - the CLI version becomes `<VERSION>`, so a release bump does not
//!   rewrite every file.
//!
//! Recording: `APPRAFTER_GOLDEN_UPDATE=1 cargo test -p apprafter --test
//! golden` writes every file and then FAILS on purpose, so a run in update
//! mode can never pass CI. Rerun without the variable to verify, review
//! the diff, and commit. A missing golden is a failure, never a silent
//! pass.
#![cfg(unix)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Output;

use assert_cmd::Command;
use tempfile::TempDir;

const UPDATE_ENV: &str = "APPRAFTER_GOLDEN_UPDATE";
const VERSION: &str = env!("CARGO_PKG_VERSION");
/// 64 ASCII alphanumerics: the shape `cli_core::target` accepts.
const TOKEN_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const TOKEN_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

fn golden_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden")
}

/// One hermetic place to run `apprafter`.
struct Sandbox {
    dir: TempDir,
    hcloud: Option<String>,
}

impl Sandbox {
    fn new() -> Self {
        let dir = TempDir::new().expect("tempdir");
        for sub in ["home", "config", "cache", "apprafter-config", "bin"] {
            fs::create_dir_all(dir.path().join(sub)).expect("sandbox dir");
        }
        Sandbox { dir, hcloud: None }
    }

    /// Point every Hetzner call at `url` (a mockito server).
    fn with_hcloud(mut self, url: String) -> Self {
        self.hcloud = Some(url);
        self
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.dir.path().join(rel)
    }

    /// A readable SSH public key inside the sandbox; its body is never parsed.
    fn ssh_key(&self) -> String {
        let p = self.path("home/id_ed25519.pub");
        if !p.exists() {
            fs::write(&p, "ssh-ed25519 AAAA golden test key\n").expect("ssh key");
        }
        p.display().to_string()
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::cargo_bin("apprafter").expect("apprafter binary");
        c.env_clear()
            .env("PATH", self.path("bin"))
            .env("HOME", self.path("home"))
            .env("XDG_CONFIG_HOME", self.path("config"))
            .env("XDG_CACHE_HOME", self.path("cache"))
            .env("APPRAFTER_CONFIG_DIR", self.path("apprafter-config"))
            .env("APPRAFTER_SKIP_STARTUP_CHECKS", "1")
            .env("RUST_LOG", "off")
            .env("TZ", "UTC")
            .env("LANG", "C")
            .env("USER", "golden")
            .current_dir(self.path("home"))
            .args(args);
        if let Some(url) = &self.hcloud {
            c.env("APPRAFTER_HCLOUD_BASE_URL", url);
        }
        c
    }

    /// A setup step: must succeed; its output is not a golden.
    fn setup(&self, args: &[&str]) {
        let out = self.cmd(args).output().expect("run apprafter");
        assert!(
            out.status.success(),
            "setup `apprafter {}` failed (exit {:?}):\n{}",
            args.join(" "),
            out.status.code(),
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// The local, no-ping `target add` most cases start from.
    fn add_target(&self, name: &str) {
        let key = self.ssh_key();
        self.setup(&[
            "target",
            "add",
            name,
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
            "--no-ping",
            "--no-interactive",
        ]);
    }

    fn normalize(&self, text: &str) -> String {
        let mut s = text.to_string();
        // The canonical form first: on some systems the tempdir is reached
        // through a symlink and a command may print either spelling.
        if let Ok(canon) = self.dir.path().canonicalize() {
            s = s.replace(&canon.display().to_string(), "<SANDBOX>");
        }
        s = s.replace(&self.dir.path().display().to_string(), "<SANDBOX>");
        if let Some(url) = &self.hcloud {
            s = s.replace(url.as_str(), "<HCLOUD>");
        }
        s.replace(VERSION, "<VERSION>")
    }

    fn render(&self, args: &[&str], out: &Output) -> String {
        let stdout = String::from_utf8(out.stdout.clone()).expect("stdout is UTF-8");
        let stderr = String::from_utf8(out.stderr.clone()).expect("stderr is UTF-8");
        let mut doc = String::new();
        doc.push_str(&format!(
            "$ apprafter {}\n",
            self.normalize(&args.join(" "))
        ));
        match out.status.code() {
            Some(code) => doc.push_str(&format!("[exit {code}]\n")),
            None => doc.push_str("[exit signal]\n"),
        }
        section(&mut doc, "stdout", &self.normalize(&stdout));
        section(&mut doc, "stderr", &self.normalize(&stderr));
        doc
    }

    /// Run `args` and compare against `<family>/<case>.golden`.
    fn golden(&self, case: &str, args: &[&str]) {
        let out = self.cmd(args).output().expect("run apprafter");
        check(case, &self.render(args, &out));
    }
}

fn section(doc: &mut String, name: &str, body: &str) {
    doc.push_str(&format!("[{name}]\n"));
    doc.push_str(body);
    if !body.is_empty() && !body.ends_with('\n') {
        doc.push_str("\n[no newline at end]\n");
    }
}

fn check(case: &str, actual: &str) {
    let path = golden_root().join(format!("{case}.golden"));
    if std::env::var_os(UPDATE_ENV).is_some() {
        fs::create_dir_all(path.parent().expect("golden parent")).expect("golden dir");
        fs::write(&path, actual).expect("write golden");
        panic!(
            "golden `{case}` written to {} — rerun without {UPDATE_ENV} to verify, then review \
             and commit it",
            path.display()
        );
    }
    let expected = fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "golden `{case}` is missing at {} ({e}). Record it deliberately: \
             {UPDATE_ENV}=1 cargo test -p apprafter --test golden, then review and commit",
            path.display()
        )
    });
    assert!(
        expected == actual,
        "golden `{case}` differs from the binary's output.\n--- expected ({})\n{expected}\n--- \
         actual\n{actual}",
        path.display()
    );
}

// ---------------------------------------------------------------------
// The harness's own guarantees
// ---------------------------------------------------------------------

#[test]
fn harness_normalizes_sandbox_hcloud_and_version() {
    let sb = Sandbox::new().with_hcloud("http://127.0.0.1:41234".to_string());
    let raw = format!(
        "{}/apprafter-config at http://127.0.0.1:41234 v{VERSION}",
        sb.path("x").parent().unwrap().display()
    );
    assert_eq!(
        sb.normalize(&raw),
        "<SANDBOX>/apprafter-config at <HCLOUD> v<VERSION>"
    );
}

#[test]
fn harness_marks_a_missing_trailing_newline() {
    let mut doc = String::new();
    section(&mut doc, "stdout", "no newline");
    assert_eq!(doc, "[stdout]\nno newline\n[no newline at end]\n");
}

#[test]
#[should_panic(expected = "is missing")]
fn harness_fails_on_a_missing_golden() {
    if std::env::var_os(UPDATE_ENV).is_some() {
        // Update mode writes instead of failing on absence; this guard is
        // about verify mode, so make the expectation hold either way.
        panic!("golden `harness/never-recorded` is missing (update mode)");
    }
    check("harness/never-recorded", "anything");
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
    sb.golden(
        "target/add_force",
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
    let sb = Sandbox::new();
    sb.add_target("prod");
    sb.golden(
        "target/add_renew",
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
    sb.golden(
        "target/rename_active",
        &["target", "rename", "prod", "production"],
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
    sb.golden(
        "target/remove_active",
        &["target", "remove", "prod", "--yes"],
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
    sb.golden("target/remove_last", &["target", "remove", "prod", "--yes"]);
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
    sb.golden(
        "target/machine_no_ping",
        &["target", "machine", "--server-type", "cx32", "--no-ping"],
    );
}

#[test]
fn target_ip_without_server() {
    let sb = Sandbox::new();
    sb.add_target("prod");
    sb.golden("target/ip_no_server", &["target", "ip"]);
}
