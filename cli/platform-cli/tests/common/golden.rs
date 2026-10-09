// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The golden harness of `apprafter` output — Phase D, D.1 Part A; shared
//! by `golden.rs` and `golden_doctor.rs` since D.3a.
//!
//! D.1 moves the CLI onto a shared `apprafter-core` crate (ADR 0067). Its
//! invariant is that, on Unix, the CLI prints exactly what it printed
//! before, except for changes made on purpose. The cases pin that byte for
//! byte.
//!
//! Every case runs the shipped binary in a hermetic sandbox:
//! - a root of one fixed length on every machine, `/tmp/.tmpXXXXXX`
//!   ([`SANDBOX_PARENT`]), whatever `TMPDIR` says: miette wraps a `×`
//!   message at 80 columns before the root becomes `<SANDBOX>`, so a path
//!   printed in one would otherwise wrap where the root is longer (macOS's
//!   `/var/folders/…/T/`);
//! - a cleared environment, so the caller's env cannot change the result;
//!   a case may add variables of its own (`Sandbox::with_env`, e.g.
//!   `HCLOUD_TOKEN`);
//! - scratch `HOME`, XDG dirs and `APPRAFTER_CONFIG_DIR`;
//! - `PATH` is one empty directory, so no real tool can run. A case may
//!   replace it (`Sandbox::with_path`); doctor cases use GOTCHA-66
//!   stand-ins (`Sandbox::with_stand_in_tools`): one script per tool
//!   doctor probes that answers its version call as the real tool does,
//!   never the host's tools;
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
//! each block starts with its own `$ apprafter …` line. A case may append
//! store files as the steps left them (`Sandbox::golden_with_files`).
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
//! - a timing `<digits> ms`, where the digits start a token and `ms` ends
//!   one (`182 ms`), becomes `<MS> ms`: timings vary per run;
//! - `(os error N)` becomes `(os error <N>)`: errno values differ by OS
//!   (ECONNREFUSED is 111 on Linux, 61 on macOS);
//! - the ESC byte becomes the text `<ESC>`, so golden files stay printable
//!   and editor-safe.
//!
//! Recording: `APPRAFTER_GOLDEN_UPDATE=1 cargo test -p apprafter --test
//! golden` (or `--test golden_doctor`; the variable must equal `1`) writes
//! every file and then FAILS on purpose, so a run in update mode can never
//! pass CI. Rerun without the variable to verify, review the diff, and
//! commit. A missing golden is a failure, never a silent pass.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Output;
use std::time::Duration;

use assert_cmd::Command;
use tempfile::TempDir;

pub const UPDATE_ENV: &str = "APPRAFTER_GOLDEN_UPDATE";
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
/// Where Hetzner calls go when a case has no mock: a closed local port.
pub const CLOSED_PORT_URL: &str = "http://127.0.0.1:1";
/// Longest a single `apprafter` run may take before it counts as a hang.
pub const RUN_TIMEOUT: Duration = Duration::from_secs(60);
/// Where every sandbox is made, never `TMPDIR`: the root is `/tmp/.tmpXXXXXX`, 15 characters on
/// every Unix machine, the length the goldens were recorded with (see the module docs).
pub const SANDBOX_PARENT: &str = "/tmp";
/// 64 ASCII alphanumerics: the shape `cli_core::target` accepts.
pub const TOKEN_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
pub const TOKEN_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

pub fn golden_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden")
}

/// One hermetic place to run `apprafter`.
pub struct Sandbox {
    dir: TempDir,
    /// Every spelling of the sandbox root (raw, canonical), longest first.
    roots: Vec<String>,
    hcloud: Option<String>,
    /// Replaces the empty `bin` directory as `PATH` (stand-in tools, per-case dirs).
    path_override: Option<PathBuf>,
    /// Extra variables every command of this sandbox gets (e.g. `HCLOUD_TOKEN`).
    extra_env: Vec<(String, String)>,
    /// Keeps the stand-in tools alive as long as the sandbox.
    tools: Option<TempDir>,
}

impl Sandbox {
    pub fn new() -> Self {
        let dir = TempDir::new_in(SANDBOX_PARENT).expect("sandbox root");
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
            path_override: None,
            extra_env: Vec::new(),
            tools: None,
        }
    }

    /// Point every Hetzner call at `url` (a mockito server).
    pub fn with_hcloud(mut self, url: String) -> Self {
        self.hcloud = Some(url);
        self
    }

    pub fn with_path(mut self, dir: PathBuf) -> Self {
        self.path_override = Some(dir);
        self
    }

    pub fn path_override(&self) -> Option<&Path> {
        self.path_override.as_deref()
    }

    pub fn with_env(mut self, key: &str, value: &str) -> Self {
        self.extra_env.push((key.to_string(), value.to_string()));
        self
    }

    /// GOTCHA-66: `PATH` holds only a stand-in per tool doctor probes (`cli_core::tools::ALL`),
    /// each answering its version call as the real tool does
    /// ([`super::stand_in::tool_stand_ins`]).
    pub fn with_stand_in_tools(mut self) -> Self {
        let dir = super::stand_in::tool_stand_ins(cli_core::tools::ALL);
        self.path_override = Some(dir.path().to_path_buf());
        self.tools = Some(dir);
        self
    }

    pub fn path(&self, rel: &str) -> PathBuf {
        self.dir.path().join(rel)
    }

    fn store(&self) -> PathBuf {
        self.path("apprafter-config")
    }

    pub fn seed_store_file(&self, rel: &str, body: &str) {
        let file = self.store().join(rel);
        fs::create_dir_all(file.parent().expect("parent")).expect("store dir");
        fs::write(file, body).expect("seed file");
    }

    pub fn seed_state(&self, target: &str, json: &str) {
        self.seed_store_file(&format!("state/{target}/.apprafter/state.json"), json);
    }

    pub fn seed_config(&self, target: &str, yaml: &str) {
        self.seed_store_file(&format!("targets/{target}/config.yaml"), yaml);
    }

    /// The CLI default pointer, written as `GlobalConfig` serialises.
    pub fn seed_pointer(&self, name: &str) {
        self.seed_store_file(
            "config.yaml",
            &format!("active_target: {name}\nversion: 1\n"),
        );
    }

    pub fn clear_pointer(&self) {
        fs::remove_file(self.store().join("config.yaml")).expect("remove config.yaml");
    }

    /// A readable SSH public key inside the sandbox; its body is never parsed.
    pub fn ssh_key(&self) -> String {
        let p = self.path("home/id_ed25519.pub");
        if !p.exists() {
            fs::write(&p, "ssh-ed25519 AAAA golden test key\n").expect("ssh key");
        }
        p.display().to_string()
    }

    pub fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::cargo_bin("apprafter").expect("apprafter binary");
        c.env_clear()
            .env(
                "PATH",
                self.path_override
                    .clone()
                    .unwrap_or_else(|| self.path("bin")),
            )
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
            .env("USER", "golden");
        for (k, v) in &self.extra_env {
            c.env(k, v);
        }
        c.current_dir(self.path("home"))
            .timeout(RUN_TIMEOUT)
            .args(args);
        c
    }

    /// A setup step: must succeed; its output is not a golden.
    pub fn setup(&self, args: &[&str]) {
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
    pub fn add_target(&self, name: &str) {
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

    pub fn normalize(&self, text: &str) -> String {
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
        let s = mask_timestamps(&s);
        let s = mask_millis(&s);
        mask_os_errors(&s).replace('\x1b', "<ESC>")
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
    pub fn golden(&self, case: &str, args: &[&str]) {
        self.golden_steps(case, &[args]);
    }

    /// Run each step in order and compare all their blocks, concatenated,
    /// against `<family>/<case>.golden`. Each block starts with its own
    /// `$ apprafter …` line, so the file shows the command and then the
    /// state it left behind.
    pub fn golden_steps(&self, case: &str, steps: &[&[&str]]) {
        self.golden_with_files(case, steps, &[]);
    }

    /// Run the steps, then show each store file (relative to the store root) as it is left:
    /// `[file <rel>]` and its normalised body, or `[file <rel>: absent]`.
    pub fn golden_with_files(&self, case: &str, steps: &[&[&str]], files: &[&str]) {
        let mut doc = String::new();
        for args in steps {
            let out = self.cmd(args).output().expect("run apprafter");
            doc.push_str(&self.render(args, &out));
        }
        for rel in files {
            match fs::read_to_string(self.store().join(rel)) {
                Ok(body) => section(&mut doc, &format!("file {rel}"), &self.normalize(&body)),
                Err(_) => doc.push_str(&format!("[file {rel}: absent]\n")),
            }
        }
        self.assert_no_raw_root(case, &doc);
        check(case, &doc);
    }

    /// Fails when `doc` still names the sandbox root's own directory: a spelling of the root
    /// the harness does not know, or a root a wrapped message split before its last component.
    /// Checked before the comparison, so update mode cannot record it either.
    pub fn assert_no_raw_root(&self, case: &str, doc: &str) {
        let name = self.dir.path().file_name().expect("root name");
        let name = name.to_string_lossy();
        assert!(
            !doc.contains(name.as_ref()),
            "golden `{case}`: the sandbox root survived normalisation (`{name}` is still in the \
             output; a spelling of it the harness does not know, or a wrapped line split it):\n{doc}"
        );
    }
}

pub fn section(doc: &mut String, name: &str, body: &str) {
    doc.push_str(&format!("[{name}]\n"));
    doc.push_str(body);
    if !body.is_empty() && !body.ends_with('\n') {
        doc.push_str("\n[no newline at end]\n");
    }
}

/// Replace each `needle` in `s` with `with`, except where the character
/// just before or just after it `joins` it to a longer token.
pub fn replace_isolated(s: &str, needle: &str, with: &str, joins: impl Fn(char) -> bool) -> String {
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
pub fn replace_bounded(s: &str, needle: &str, with: &str) -> String {
    replace_isolated(s, needle, with, |c| c.is_ascii_digit() || c == '.')
}

/// Replace every RFC 3339 UTC timestamp, `YYYY-MM-DDTHH:MM:SS[.fraction]Z`
/// (the shape tracing's default timer prints), with `<TS>`.
pub fn mask_timestamps(s: &str) -> String {
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
pub fn timestamp_len(bytes: &[u8]) -> Option<usize> {
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

/// `<digits> ms` → `<MS> ms` where the digits start a token (start of text, or after a byte
/// that is neither alphanumeric nor `.`) and `ms` ends one: timings (`182 ms`) vary per run.
pub fn mask_millis(s: &str) -> String {
    let bytes = s.as_bytes();
    let (mut out, mut copied, mut at) = (String::with_capacity(s.len()), 0, 0);
    while at < bytes.len() {
        let starts = bytes[at].is_ascii_digit()
            && (at == 0 || !(bytes[at - 1].is_ascii_alphanumeric() || bytes[at - 1] == b'.'));
        if !starts {
            at += 1;
            continue;
        }
        let digits = bytes[at..]
            .iter()
            .take_while(|b| b.is_ascii_digit())
            .count();
        let rest = &bytes[at + digits..];
        if rest.starts_with(b" ms") && rest.get(3).is_none_or(|b| !b.is_ascii_alphanumeric()) {
            out.push_str(&s[copied..at]);
            out.push_str("<MS> ms");
            at += digits + 3;
            copied = at;
        } else {
            at += digits;
        }
    }
    out.push_str(&s[copied..]);
    out
}

/// `(os error N)` → `(os error <N>)`: errno values differ by OS (ECONNREFUSED is 111 on Linux,
/// 61 on macOS) and the golden suite runs on both.
pub fn mask_os_errors(s: &str) -> String {
    const OPEN: &str = "(os error ";
    let (mut out, mut rest) = (String::with_capacity(s.len()), s);
    while let Some(at) = rest.find(OPEN) {
        let after = &rest[at + OPEN.len()..];
        let digits = after.bytes().take_while(u8::is_ascii_digit).count();
        if digits > 0 && after[digits..].starts_with(')') {
            out.push_str(&rest[..at]);
            out.push_str("(os error <N>)");
            rest = &after[digits + 1..];
        } else {
            out.push_str(&rest[..at + OPEN.len()]);
            rest = after;
        }
    }
    out.push_str(rest);
    out
}

/// Why a golden comparison did not pass; each carries the full message.
#[derive(Debug)]
pub enum CheckError {
    /// Verify mode and no file: a missing golden fails, never passes.
    Missing(String),
    /// Verify mode and the file differs from the output.
    Differs(String),
    /// Update mode wrote the file; the run fails on purpose.
    Written(String),
}

impl CheckError {
    pub fn message(&self) -> &str {
        match self {
            CheckError::Missing(m) | CheckError::Differs(m) | CheckError::Written(m) => m,
        }
    }
}

/// Update mode is on only when the variable is exactly `1`.
pub fn update_mode() -> bool {
    std::env::var(UPDATE_ENV).as_deref() == Ok("1")
}

pub fn check(case: &str, actual: &str) {
    let path = golden_root().join(format!("{case}.golden"));
    if let Err(e) = check_at(&path, actual, update_mode()) {
        panic!("golden `{case}` {}", e.message());
    }
}

/// Compare `actual` with the golden at `path`, or (in update mode) write it
/// there and fail on purpose.
pub fn check_at(path: &Path, actual: &str, update: bool) -> Result<(), CheckError> {
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
                 apprafter --test {}, then review and commit",
                path.display(),
                env!("CARGO_CRATE_NAME")
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
// Hetzner fixtures (mockito)
// ---------------------------------------------------------------------

pub const LOCATIONS_OK: &str = r#"{"locations":[
  {"id":1,"name":"fsn1","description":"Falkenstein DC Park 1","country":"DE","city":"Falkenstein","network_zone":"eu-central"},
  {"id":2,"name":"nbg1","description":"Nuremberg DC Park 1","country":"DE","city":"Nuremberg","network_zone":"eu-central"}
]}"#;

pub const UNAUTHORIZED: &str =
    r#"{"error":{"code":"unauthorized","message":"unable to authenticate"}}"#;

/// Two SKUs in nbg1: cx22 (recommended) and cx32; cx32 is also sold in fsn1.
pub const SERVER_TYPES: &str = r#"{"server_types":[
  {"id":104,"name":"cx22","architecture":"x86","cpu_type":"shared","cores":2,"memory":4.0,"disk":40,"deprecation":null,
   "locations":[{"name":"nbg1","available":true,"recommended":true}],
   "prices":[{"location":"nbg1","price_monthly":{"net":"3.7900","gross":"4.5101"},"price_hourly":{"net":"0.0060","gross":"0.0071"}}]},
  {"id":105,"name":"cx32","architecture":"x86","cpu_type":"shared","cores":4,"memory":8.0,"disk":80,"deprecation":null,
   "locations":[{"name":"nbg1","available":true,"recommended":false},{"name":"fsn1","available":true,"recommended":false}],
   "prices":[{"location":"nbg1","price_monthly":{"net":"6.8000","gross":"8.0920"},"price_hourly":{"net":"0.0109","gross":"0.0130"}},
             {"location":"fsn1","price_monthly":{"net":"6.8000","gross":"8.0920"},"price_hourly":{"net":"0.0109","gross":"0.0130"}}]}
],"meta":{"pagination":{"next_page":null}}}"#;

/// `state.json` of a target whose server Hetzner knows as 42.
pub const PROVISIONED_STATE: &str =
    r#"{"hetzner_cloud":{"server_id":42,"server_name":"prod-node","server_type":"cx22"}}"#;
/// `GET /v1/servers/42`, the read `target ip` makes: the server `PROVISIONED_STATE` records.
pub const SERVER_42_BODY: &str = r#"{"server":{"id":42,"name":"prod-node","status":"running","labels":{},"public_net":{"ipv4":{"ip":"203.0.113.10"},"ipv6":{"ip":"2001:db8:1::/64"}}}}"#;
/// Hetzner's 404 for a server id it does not know.
pub const SERVER_NOT_FOUND: &str =
    r#"{"error":{"code":"not_found","message":"server with ID '42' not found"}}"#;

/// A JSON `GET path` route that answers only requests carrying
/// `Bearer {token}`; not yet created, so a case can add expectations.
pub fn json_route(
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

pub fn json_mock(
    server: &mut mockito::Server,
    path: &str,
    status: usize,
    body: &str,
    token: &str,
) -> mockito::Mock {
    json_route(server, path, status, body, token).create()
}
