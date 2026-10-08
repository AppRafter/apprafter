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
//! - `KUBECONFIG` names a sandbox file that does not exist, so no cluster
//!   can be reached;
//! - startup checks off;
//! - `APPRAFTER_HCLOUD_BASE_URL` is always set: to the case's `mockito`
//!   server when it has one, else to a closed local port, so no call can
//!   reach the real Hetzner API;
//! - `TZ=UTC`. Code that asks the OS for its zone through `iana_time_zone`
//!   (`backup.rs`) reads `/etc/localtime` and ignores `TZ`; no current case
//!   reaches it;
//! - a 60-second timeout per command, so a hang fails instead of blocking
//!   the suite.
//!
//! `RUST_LOG` is deliberately left unset: the default tracing filter's
//! INFO/WARN lines reach stderr exactly as users see them (with ANSI
//! colour — tracing turns it off only for `NO_COLOR`). A refactor that
//! drops one, for example by moving code into a crate the filter does not
//! name, fails here.
//!
//! stdout and stderr are captured separately (non-TTY) and rendered with
//! the command and exit code into one `tests/golden/<family>/<case>.golden`.
//! A multi-step case renders every step's block, in order, into one file;
//! each block starts with its own `$ apprafter …` line.
//!
//! The only normalisations are deterministic substitutions:
//! - the sandbox path becomes `<SANDBOX>` (its raw and canonical spellings,
//!   the longer one first);
//! - the Hetzner base URL (the mockito URL, or the closed-port URL) becomes
//!   `<HCLOUD>`;
//! - the CLI version becomes `<VERSION>`, so a release bump does not
//!   rewrite every file — only where it is not part of a longer number;
//! - an RFC 3339 UTC timestamp `YYYY-MM-DDTHH:MM:SS[.fraction]Z` (tracing's
//!   default timer) becomes `<TS>`;
//! - the ESC byte becomes the text `<ESC>`, so golden files stay printable
//!   and editor-safe.
//!
//! Recording: `APPRAFTER_GOLDEN_UPDATE=1 cargo test -p apprafter --test
//! golden` (the variable must equal `1`) writes every file and then FAILS on
//! purpose, so a run in update mode can never pass CI. Rerun without the
//! variable to verify, review the diff, and commit. A missing golden is a
//! failure, never a silent pass.
#![cfg(unix)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Output;
use std::time::Duration;

use assert_cmd::Command;
use tempfile::TempDir;

const UPDATE_ENV: &str = "APPRAFTER_GOLDEN_UPDATE";
const VERSION: &str = env!("CARGO_PKG_VERSION");
/// Where Hetzner calls go when a case has no mock: a closed local port.
const CLOSED_PORT_URL: &str = "http://127.0.0.1:1";
/// Longest a single `apprafter` run may take before it counts as a hang.
const RUN_TIMEOUT: Duration = Duration::from_secs(60);
/// 64 ASCII alphanumerics: the shape `cli_core::target` accepts.
const TOKEN_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const TOKEN_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

fn golden_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden")
}

/// One hermetic place to run `apprafter`.
struct Sandbox {
    dir: TempDir,
    /// Every spelling of the sandbox root (raw, canonical), longest first.
    roots: Vec<String>,
    hcloud: Option<String>,
}

impl Sandbox {
    fn new() -> Self {
        let dir = TempDir::new().expect("tempdir");
        for sub in ["home", "config", "cache", "apprafter-config", "bin"] {
            fs::create_dir_all(dir.path().join(sub)).expect("sandbox dir");
        }
        // On some systems the tempdir is reached through a symlink and a
        // command may print either spelling. Longest first, so a spelling
        // that contains the other is never half-replaced.
        let mut roots = vec![dir.path().display().to_string()];
        if let Ok(canon) = dir.path().canonicalize() {
            let canon = canon.display().to_string();
            if !roots.contains(&canon) {
                roots.push(canon);
            }
        }
        roots.sort_by_key(|r| std::cmp::Reverse(r.len()));
        Sandbox {
            dir,
            roots,
            hcloud: None,
        }
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
            .env("KUBECONFIG", self.path("no-kubeconfig"))
            .env(
                "APPRAFTER_HCLOUD_BASE_URL",
                self.hcloud.as_deref().unwrap_or(CLOSED_PORT_URL),
            )
            .env("TZ", "UTC")
            .env("LANG", "C")
            .env("USER", "golden")
            .current_dir(self.path("home"))
            .timeout(RUN_TIMEOUT)
            .args(args);
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
        for root in &self.roots {
            s = s.replace(root.as_str(), "<SANDBOX>");
        }
        // The mock URL first: the closed-port URL is a prefix of a mock URL
        // whose port starts with `1`. Neither may be continued by a digit,
        // so one is never half-replaced either way.
        if let Some(url) = &self.hcloud {
            s = replace_isolated(&s, url, "<HCLOUD>", |c| c.is_ascii_digit());
        }
        s = replace_isolated(&s, CLOSED_PORT_URL, "<HCLOUD>", |c| c.is_ascii_digit());
        s = replace_bounded(&s, VERSION, "<VERSION>");
        mask_timestamps(&s).replace('\x1b', "<ESC>")
    }

    fn render(&self, args: &[&str], out: &Output) -> String {
        let stdout = std::str::from_utf8(&out.stdout).expect("stdout is UTF-8");
        let stderr = std::str::from_utf8(&out.stderr).expect("stderr is UTF-8");
        let mut doc = String::new();
        doc.push_str(&format!(
            "$ apprafter {}\n",
            self.normalize(&args.join(" "))
        ));
        match out.status.code() {
            Some(code) => doc.push_str(&format!("[exit {code}]\n")),
            None => doc.push_str("[exit signal]\n"),
        }
        section(&mut doc, "stdout", &self.normalize(stdout));
        section(&mut doc, "stderr", &self.normalize(stderr));
        doc
    }

    /// Run `args` and compare against `<family>/<case>.golden`.
    fn golden(&self, case: &str, args: &[&str]) {
        self.golden_steps(case, &[args]);
    }

    /// Run each step in order and compare all their blocks, concatenated,
    /// against `<family>/<case>.golden`. Each block starts with its own
    /// `$ apprafter …` line, so the file shows the command and then the
    /// state it left behind.
    fn golden_steps(&self, case: &str, steps: &[&[&str]]) {
        let mut doc = String::new();
        for args in steps {
            let out = self.cmd(args).output().expect("run apprafter");
            doc.push_str(&self.render(args, &out));
        }
        check(case, &doc);
    }
}

fn section(doc: &mut String, name: &str, body: &str) {
    doc.push_str(&format!("[{name}]\n"));
    doc.push_str(body);
    if !body.is_empty() && !body.ends_with('\n') {
        doc.push_str("\n[no newline at end]\n");
    }
}

/// Replace each `needle` in `s` with `with`, except where the character
/// just before or just after it `joins` it to a longer token.
fn replace_isolated(s: &str, needle: &str, with: &str, joins: impl Fn(char) -> bool) -> String {
    let mut out = String::with_capacity(s.len());
    // Bytes of `s` already copied (or replaced) into `out`.
    let mut copied = 0;
    for (at, _) in s.match_indices(needle) {
        let before = s[..at].chars().next_back();
        let after = s[at + needle.len()..].chars().next();
        if before.is_some_and(&joins) || after.is_some_and(&joins) {
            continue;
        }
        out.push_str(&s[copied..at]);
        out.push_str(with);
        copied = at + needle.len();
    }
    out.push_str(&s[copied..]);
    out
}

/// Replace `needle` (a version) only where it is not part of a longer
/// number: no ASCII digit or `.` right before or after it.
fn replace_bounded(s: &str, needle: &str, with: &str) -> String {
    replace_isolated(s, needle, with, |c| c.is_ascii_digit() || c == '.')
}

/// Replace every RFC 3339 UTC timestamp, `YYYY-MM-DDTHH:MM:SS[.fraction]Z`
/// (the shape tracing's default timer prints), with `<TS>`.
fn mask_timestamps(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    // Bytes of `s` already copied (or masked) into `out`.
    let mut copied = 0;
    let mut at = 0;
    while at < bytes.len() {
        // A timestamp never continues a longer digit run.
        let starts_token = at == 0 || !bytes[at - 1].is_ascii_digit();
        match timestamp_len(&bytes[at..]) {
            Some(len) if starts_token => {
                out.push_str(&s[copied..at]);
                out.push_str("<TS>");
                at += len;
                copied = at;
            }
            _ => at += 1,
        }
    }
    out.push_str(&s[copied..]);
    out
}

/// The length of the `YYYY-MM-DDTHH:MM:SS[.fraction]Z` timestamp that
/// `bytes` starts with, if it starts with one.
fn timestamp_len(bytes: &[u8]) -> Option<usize> {
    // `9` stands for any ASCII digit; everything else is literal.
    const SHAPE: &[u8] = b"9999-99-99T99:99:99";
    let head = bytes.get(..SHAPE.len())?;
    let fits = head.iter().zip(SHAPE).all(|(b, want)| match want {
        b'9' => b.is_ascii_digit(),
        literal => b == literal,
    });
    if !fits {
        return None;
    }
    let mut len = SHAPE.len();
    if bytes.get(len) == Some(&b'.') {
        let digits = bytes[len + 1..]
            .iter()
            .take_while(|b| b.is_ascii_digit())
            .count();
        if digits == 0 {
            return None;
        }
        len += 1 + digits;
    }
    (bytes.get(len) == Some(&b'Z')).then_some(len + 1)
}

/// Why a golden comparison did not pass; each carries the full message.
#[derive(Debug)]
enum CheckError {
    /// Verify mode and no file: a missing golden fails, never passes.
    Missing(String),
    /// Verify mode and the file differs from the output.
    Differs(String),
    /// Update mode wrote the file; the run fails on purpose.
    Written(String),
}

impl CheckError {
    fn message(&self) -> &str {
        match self {
            CheckError::Missing(m) | CheckError::Differs(m) | CheckError::Written(m) => m,
        }
    }
}

/// Update mode is on only when the variable is exactly `1`.
fn update_mode() -> bool {
    std::env::var(UPDATE_ENV).as_deref() == Ok("1")
}

fn check(case: &str, actual: &str) {
    let path = golden_root().join(format!("{case}.golden"));
    if let Err(e) = check_at(&path, actual, update_mode()) {
        panic!("golden `{case}` {}", e.message());
    }
}

/// Compare `actual` with the golden at `path`, or (in update mode) write it
/// there and fail on purpose.
fn check_at(path: &Path, actual: &str, update: bool) -> Result<(), CheckError> {
    if update {
        fs::create_dir_all(path.parent().expect("golden parent")).expect("golden dir");
        fs::write(path, actual).expect("write golden");
        return Err(CheckError::Written(format!(
            "written to {} — rerun without {UPDATE_ENV} to verify, then review and commit it",
            path.display()
        )));
    }
    let expected = match fs::read_to_string(path) {
        Ok(expected) => expected,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(CheckError::Missing(format!(
                "is missing at {} ({e}). Record it deliberately: {UPDATE_ENV}=1 cargo test -p \
                 apprafter --test golden, then review and commit",
                path.display()
            )));
        }
        Err(e) => panic!("cannot read golden {}: {e}", path.display()),
    };
    if expected != actual {
        return Err(CheckError::Differs(format!(
            "differs from the binary's output.\n--- expected ({})\n{expected}\n--- actual\n{actual}",
            path.display()
        )));
    }
    Ok(())
}

// ---------------------------------------------------------------------
// The harness's own guarantees
// ---------------------------------------------------------------------

#[test]
fn harness_normalizes_sandbox_hcloud_and_version() {
    let sb = Sandbox::new().with_hcloud("http://127.0.0.1:41234".to_string());
    let raw = format!(
        "{}/apprafter-config at http://127.0.0.1:41234 v{VERSION}",
        sb.dir.path().display()
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

/// Every recorded file must belong to a case in this file; otherwise a
/// renamed or deleted case would leave a golden that nothing checks.
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
    let source = include_str!("golden.rs");
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
        .filter(|id| !source.contains(&format!("\"{id}\"")))
        .collect();
    assert!(
        orphans.is_empty(),
        "golden files with no case in golden.rs (restore the case or delete the file): \
         {orphans:?}"
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

const LOCATIONS_OK: &str = r#"{"locations":[
  {"id":1,"name":"fsn1","description":"Falkenstein DC Park 1","country":"DE","city":"Falkenstein","network_zone":"eu-central"},
  {"id":2,"name":"nbg1","description":"Nuremberg DC Park 1","country":"DE","city":"Nuremberg","network_zone":"eu-central"}
]}"#;

const UNAUTHORIZED: &str =
    r#"{"error":{"code":"unauthorized","message":"unable to authenticate"}}"#;

/// Two SKUs in nbg1: cx22 (recommended) and cx32; cx32 is also sold in fsn1.
const SERVER_TYPES: &str = r#"{"server_types":[
  {"id":104,"name":"cx22","architecture":"x86","cpu_type":"shared","cores":2,"memory":4.0,"disk":40,"deprecation":null,
   "locations":[{"name":"nbg1","available":true,"recommended":true}],
   "prices":[{"location":"nbg1","price_monthly":{"net":"3.7900","gross":"4.5101"},"price_hourly":{"net":"0.0060","gross":"0.0071"}}]},
  {"id":105,"name":"cx32","architecture":"x86","cpu_type":"shared","cores":4,"memory":8.0,"disk":80,"deprecation":null,
   "locations":[{"name":"nbg1","available":true,"recommended":false},{"name":"fsn1","available":true,"recommended":false}],
   "prices":[{"location":"nbg1","price_monthly":{"net":"6.8000","gross":"8.0920"},"price_hourly":{"net":"0.0109","gross":"0.0130"}},
             {"location":"fsn1","price_monthly":{"net":"6.8000","gross":"8.0920"},"price_hourly":{"net":"0.0109","gross":"0.0130"}}]}
],"meta":{"pagination":{"next_page":null}}}"#;

/// A JSON `GET path` route that answers only requests carrying
/// `Bearer {token}`; not yet created, so a case can add expectations.
fn json_route(
    server: &mut mockito::Server,
    path: &str,
    status: usize,
    body: &str,
    token: &str,
) -> mockito::Mock {
    server
        .mock("GET", path)
        .match_query(mockito::Matcher::Any)
        .match_header("authorization", format!("Bearer {token}").as_str())
        .with_status(status)
        .with_header("content-type", "application/json")
        .with_body(body)
}

fn json_mock(
    server: &mut mockito::Server,
    path: &str,
    status: usize,
    body: &str,
    token: &str,
) -> mockito::Mock {
    json_route(server, path, status, body, token).create()
}

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
