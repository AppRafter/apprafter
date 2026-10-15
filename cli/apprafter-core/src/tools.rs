// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The external tools the core runs (D.3 overview §3.8): which they are, how to install them,
//! where they are ([`ToolResolver`], on the context's tool search path, never the process
//! `PATH`), and what probing them found ([`toolchain`]). D.3c wires them into doctor.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use serde::Serialize;

use crate::context::{Context, PathSource, PATH_ENV};
use crate::error::{CoreError, CoreResult};
use crate::CancellationToken;

/// How long one `--version` probe may take.
pub const TOOL_PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// The most a [`ToolProblem::NoVersionOutput`] detail holds, in characters.
pub const NO_VERSION_DETAIL_MAX_CHARS: usize = 200;

/// A run of at least this many ASCII letters and digits reads as a secret (a Hetzner token is
/// 64, an age key's body 59) and never reaches a detail.
const SECRET_RUN_MIN: usize = 32;

/// One external tool, in the CLI's probe order in [`ToolId::ALL`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum ToolId {
    Kubectl,
    Helm,
    Restic,
    Git,
    Ssh,
    Cue,
}

impl ToolId {
    /// Every tool, in the order doctor and the toolchain panel list them.
    pub const ALL: [ToolId; 6] = [
        ToolId::Restic,
        ToolId::Kubectl,
        ToolId::Helm,
        ToolId::Git,
        ToolId::Ssh,
        ToolId::Cue,
    ];

    /// The CLI's definition of the tool: name, purpose, install lines, version arguments.
    pub fn spec(self) -> &'static cli_core::tools::Tool {
        use cli_core::tools as t;
        match self {
            Self::Restic => &t::RESTIC,
            Self::Kubectl => &t::KUBECTL,
            Self::Helm => &t::HELM,
            Self::Git => &t::GIT,
            Self::Ssh => &t::SSH,
            Self::Cue => &t::CUE,
        }
    }

    /// The executable name, e.g. `kubectl`.
    pub fn name(self) -> &'static str {
        self.spec().name
    }
}

/// Which system an install hint is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum HintOs {
    Windows,
    Macos,
    Debian,
    Arch,
    Nix,
    Other,
}

impl From<cli_core::tools::InstallOs> for HintOs {
    fn from(os: cli_core::tools::InstallOs) -> Self {
        use cli_core::tools::InstallOs as I;
        match os {
            I::Windows => Self::Windows,
            I::Macos => Self::Macos,
            I::Debian => Self::Debian,
            I::Arch => Self::Arch,
            I::Nix => Self::Nix,
            I::Other => Self::Other,
        }
    }
}

/// One install line: a command, a URL, or "preinstalled".
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct InstallHint {
    pub os: HintOs,
    pub command: String,
}

/// Why a tool is not usable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum ToolProblem {
    /// Not on the search path (`path` is `None`).
    NotFound,
    /// Found only as a `.cmd` / `.bat` shim, which cannot be run directly (Windows).
    Unsupported {
        path: String,
    },
    /// It ran but reported no version: it exited non-zero or was killed, whatever it printed
    /// (an error line is not a version). `exit` is its exit code, when it had one; `detail` is
    /// why, in the tool's words: the first line it printed, stderr first, bounded and never a
    /// secret ([`NO_VERSION_DETAIL_MAX_CHARS`]), e.g. a mise shim's "No version is set".
    NoVersionOutput {
        exit: Option<i32>,
        detail: Option<String>,
    },
    TimedOut,
    SpawnFailed {
        error: String,
    },
}

/// What probing one tool found.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct ToolStatus {
    pub tool: ToolId,
    pub required: bool,
    pub purpose: String,
    pub path: Option<String>,
    /// The first line of the tool's version output.
    pub version: Option<String>,
    pub problem: Option<ToolProblem>,
    pub install: Vec<InstallHint>,
}

/// Every tool, and where they were looked for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct ToolchainReport {
    pub tools: Vec<ToolStatus>,
    pub search_path: Vec<String>,
    pub search_path_source: PathSource,
}

impl ToolStatus {
    /// What is known of `tool` before anything is probed.
    fn unprobed(tool: ToolId) -> Self {
        let spec = tool.spec();
        ToolStatus {
            tool,
            required: spec.required,
            purpose: spec.purpose.to_string(),
            path: None,
            version: None,
            problem: None,
            install: spec
                .hints
                .iter()
                .map(|h| InstallHint {
                    os: HintOs::from(h.os),
                    command: h.text.to_string(),
                })
                .collect(),
        }
    }

    /// A probe that could not run at all.
    fn failed(tool: ToolId, error: &str) -> Self {
        ToolStatus {
            problem: Some(ToolProblem::SpawnFailed {
                error: error.to_string(),
            }),
            ..Self::unprobed(tool)
        }
    }
}

/// Finds the tools on one search path (D.3 overview §3.8): the context's
/// ([`Context::tools`]), never the process `PATH`.
#[derive(Debug, Clone, Copy)]
pub struct ToolResolver<'a> {
    search_path: &'a OsStr,
    cue_override: Option<&'a Path>,
    windows: bool,
    needed_by: &'a str,
}

impl<'a> ToolResolver<'a> {
    /// `windows`: look for `<name>.exe` only, and type a `.cmd` / `.bat` shim as
    /// [`CoreError::ToolUnsupported`]. `cue_override` is the CLI's `CUE_BIN`.
    pub fn new(search_path: &'a OsStr, cue_override: Option<&'a Path>, windows: bool) -> Self {
        ToolResolver {
            search_path,
            cue_override,
            windows,
            needed_by: "apprafter",
        }
    }

    /// The command a not-found error names (`"apprafter doctor"`); default `"apprafter"`.
    pub fn needed_by(mut self, command: &'a str) -> Self {
        self.needed_by = command;
        self
    }

    /// The tool's path. Windows: the `.exe` only; a `.cmd` / `.bat` hit is
    /// [`CoreError::ToolUnsupported`]. Cue: the `CUE_BIN` override first, as a path or as a
    /// name looked up on the search path. A miss is `CliError::ExternalToolNotFound`
    /// (`CliError::CueNotFound` for cue).
    pub fn resolve(&self, tool: ToolId) -> CoreResult<PathBuf> {
        self.resolve_in(tool, &cli_core::tools::is_executable_file)
    }

    fn resolve_in(&self, tool: ToolId, is_exe: &dyn Fn(&Path) -> bool) -> CoreResult<PathBuf> {
        use cli_core::tools::{executable_names, find_on_path_with};
        if let (ToolId::Cue, Some(over)) = (tool, self.cue_override) {
            return self.resolve_cue_override(over, is_exe);
        }
        let name = tool.name();
        let names = executable_names(name, self.windows);
        if let Some(found) = find_on_path_with(&names, self.search_path, is_exe) {
            return Ok(found);
        }
        if self.windows {
            let shims = [format!("{name}.cmd"), format!("{name}.bat")];
            if let Some(shim) = find_on_path_with(&shims, self.search_path, is_exe) {
                return Err(CoreError::ToolUnsupported {
                    tool: name.into(),
                    path: shim.display().to_string(),
                });
            }
        }
        Err(self.not_found(tool))
    }

    /// `CUE_BIN` as `cue::cue_bin` reads it: a path (absolute, or with a directory part) is
    /// taken as it is; a bare name is looked up on the search path; empty is "no cue".
    fn resolve_cue_override(
        &self,
        over: &Path,
        is_exe: &dyn Fn(&Path) -> bool,
    ) -> CoreResult<PathBuf> {
        let missing = || CoreError::from(cli_core::CliError::CueNotFound);
        if over.as_os_str().is_empty() {
            return Err(missing());
        }
        if over.is_absolute() || over.components().count() > 1 {
            return if is_exe(over) {
                Ok(over.to_path_buf())
            } else {
                Err(missing())
            };
        }
        let names = cli_core::tools::executable_names(&over.to_string_lossy(), self.windows);
        cli_core::tools::find_on_path_with(&names, self.search_path, is_exe).ok_or_else(missing)
    }

    fn not_found(&self, tool: ToolId) -> CoreError {
        let spec = tool.spec();
        match tool {
            ToolId::Cue => cli_core::CliError::CueNotFound.into(),
            _ => cli_core::CliError::ExternalToolNotFound {
                tool: spec.name.into(),
                needed_by: self.needed_by.into(),
                purpose: spec.purpose.into(),
                install: spec.install.into(),
            }
            .into(),
        }
    }

    /// A `Command` for the resolved path whose child gets `PATH` = the search path (spec §3.1).
    pub fn command(&self, tool: ToolId) -> CoreResult<Command> {
        let mut cmd = Command::new(self.resolve(tool)?);
        cmd.env(PATH_ENV, self.search_path);
        Ok(cmd)
    }

    /// Resolve `tool` and run its version arguments, killed after `timeout` or when `cancel`
    /// trips. Never fails: what went wrong is the status's `problem`. The version is the first
    /// non-empty line of stdout, then stderr (`ssh -V` writes to stderr), of a run that exited
    /// 0. Each of the six version calls exits 0 when the tool works, so a non-zero exit is
    /// `NoVersionOutput` whatever it printed: an asdf / mise shim with no version set, or the
    /// macOS `git` stub without the developer tools, prints an error line and fails.
    pub fn probe(&self, tool: ToolId, timeout: Duration, cancel: &CancellationToken) -> ToolStatus {
        let mut status = ToolStatus::unprobed(tool);
        let path = match self.resolve(tool) {
            Ok(path) => path,
            Err(CoreError::ToolUnsupported { path, .. }) => {
                status.problem = Some(ToolProblem::Unsupported { path });
                return status;
            }
            Err(_) => {
                status.problem = Some(ToolProblem::NotFound);
                return status;
            }
        };
        status.path = Some(path.display().to_string());
        let mut cmd = Command::new(&path);
        cmd.args(tool.spec().version_args)
            .env(PATH_ENV, self.search_path);
        let out = match crate::process::run_bounded(cmd, timeout, cancel) {
            Ok(out) => out,
            // `resolve` found an executable file, so ENOENT from exec is about what the file
            // needs: the interpreter of its `#!` line, or the dynamic loader of a binary built
            // for another libc (NixOS, musl). The tool is installed; `NotFound` would offer
            // install lines for it and hide the cause.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                status.problem = Some(ToolProblem::SpawnFailed {
                    error: format!("{e}: its interpreter (`#!` line) or dynamic loader is missing"),
                });
                return status;
            }
            Err(e) => {
                status.problem = Some(ToolProblem::SpawnFailed {
                    error: e.to_string(),
                });
                return status;
            }
        };
        if out.timed_out {
            status.problem = Some(ToolProblem::TimedOut);
            return status;
        }
        if out.status.is_some_and(|s| s.success()) {
            status.version = first_nonempty_line(&out.stdout, &out.stderr);
        } else {
            status.problem = Some(ToolProblem::NoVersionOutput {
                exit: out.status.and_then(|s| s.code()),
                detail: no_version_detail(&out.stderr, &out.stdout),
            });
        }
        status
    }
}

/// The first non-empty trimmed line of `first`, else of `then`: the version is stdout's, then
/// stderr's (`ssh -V`); a failure's detail is stderr's, then stdout's.
fn first_nonempty_line(first: &[u8], then: &[u8]) -> Option<String> {
    [first, then]
        .iter()
        .map(|b| String::from_utf8_lossy(b))
        .find_map(|text| {
            text.lines()
                .map(str::trim)
                .find(|l| !l.is_empty())
                .map(str::to_string)
        })
}

/// Why a tool reported no version (decision 3 of the D.3a review): its first non-empty line,
/// stderr first, with control characters and terminal escapes dropped (the CLI prints it, the
/// desktop shows it), every run of [`SECRET_RUN_MIN`]+ ASCII letters and digits replaced with
/// `[redacted]` (the child inherits the CLI's environment, a broken shim can print anything),
/// then cut to [`NO_VERSION_DETAIL_MAX_CHARS`] characters — redacted before it is cut, so no
/// secret survives as a shorter run. `None` when it printed nothing.
fn no_version_detail(stderr: &[u8], stdout: &[u8]) -> Option<String> {
    let line = first_nonempty_line(stderr, stdout)?;
    let printable = strip_controls(&line);
    let line = redact_secret_runs(printable.trim());
    let detail: String = line.chars().take(NO_VERSION_DETAIL_MAX_CHARS).collect();
    (!detail.is_empty()).then_some(detail)
}

/// `text` without control characters; an ANSI CSI escape (`ESC [ … final byte`) goes whole.
fn strip_controls(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' && chars.peek() == Some(&'[') {
            chars.next();
            // Parameter and intermediate bytes, then one final byte in `@`..=`~`.
            for c in chars.by_ref() {
                if ('@'..='~').contains(&c) {
                    break;
                }
            }
        } else if !c.is_control() {
            out.push(c);
        }
    }
    out
}

/// `text` with each run of [`SECRET_RUN_MIN`] or more ASCII letters and digits replaced.
fn redact_secret_runs(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut run = String::new();
    let flush = |run: &mut String, out: &mut String| {
        if run.len() >= SECRET_RUN_MIN {
            out.push_str("[redacted]");
        } else {
            out.push_str(run);
        }
        run.clear();
    };
    for c in text.chars() {
        if c.is_ascii_alphanumeric() {
            run.push(c);
        } else {
            flush(&mut run, &mut out);
            out.push(c);
        }
    }
    flush(&mut run, &mut out);
    out
}

/// Every tool probed concurrently, each bounded by [`TOOL_PROBE_TIMEOUT`], in
/// [`ToolId::ALL`] order, with the search path they were looked for on.
pub fn toolchain(ctx: &Context, cancel: &CancellationToken) -> CoreResult<ToolchainReport> {
    cancel.check()?;
    let resolver = ctx.tools();
    let tools = std::thread::scope(|s| {
        let probes: Vec<_> = ToolId::ALL
            .iter()
            .map(|&tool| {
                (
                    tool,
                    s.spawn(move || resolver.probe(tool, TOOL_PROBE_TIMEOUT, cancel)),
                )
            })
            .collect();
        probes
            .into_iter()
            .map(|(tool, h)| {
                h.join()
                    .unwrap_or_else(|_| ToolStatus::failed(tool, "the probe thread panicked"))
            })
            .collect()
    });
    cancel.check()?;
    Ok(ToolchainReport {
        tools,
        search_path: std::env::split_paths(ctx.tool_search_path())
            .filter(|d| !d.as_os_str().is_empty())
            .map(|d| d.display().to_string())
            .collect(),
        search_path_source: ctx.tool_search_path_source(),
    })
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::path::{Path, PathBuf};

    use super::*;
    use crate::error::CoreError;

    #[test]
    fn every_tool_id_has_its_cli_spec_in_probe_order() {
        let names: Vec<_> = ToolId::ALL.iter().map(|t| t.name()).collect();
        assert_eq!(names, ["restic", "kubectl", "helm", "git", "ssh", "cue"]);
        assert!(ToolId::Kubectl.spec().required);
    }

    fn path_of(dirs: &[&str]) -> OsString {
        std::env::join_paths(dirs).unwrap()
    }

    #[test]
    fn a_tool_resolves_to_its_first_hit_in_path_order() {
        let p = path_of(&["/a", "/b"]);
        let r = ToolResolver::new(&p, None, false);
        let got = r
            .resolve_in(ToolId::Kubectl, &|f| f == Path::new("/b/kubectl"))
            .unwrap();
        assert_eq!(got, PathBuf::from("/b/kubectl"));
    }

    #[test]
    fn windows_takes_only_an_exe_and_types_a_shim() {
        let p = path_of(&["/a", "/b"]);
        let r = ToolResolver::new(&p, None, true);
        assert_eq!(
            r.resolve_in(ToolId::Kubectl, &|f| f == Path::new("/a/kubectl.cmd")
                || f == Path::new("/b/kubectl.exe"))
                .unwrap(),
            PathBuf::from("/b/kubectl.exe")
        );
        match r.resolve_in(ToolId::Kubectl, &|f| f == Path::new("/a/kubectl.cmd")) {
            // compared as paths: on CI's windows-latest leg the joined path displays as `/a\kubectl.cmd`
            Err(CoreError::ToolUnsupported { tool, path }) => {
                assert_eq!(tool, "kubectl");
                assert_eq!(Path::new(&path), Path::new("/a/kubectl.cmd"));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_miss_is_the_typed_not_found() {
        let p = path_of(&["/a"]);
        let r = ToolResolver::new(&p, None, false);
        assert!(matches!(r.resolve_in(ToolId::Helm, &|_| false),
            Err(CoreError::Cli(cli_core::CliError::ExternalToolNotFound { ref tool, ref needed_by, .. }))
                if tool == "helm" && needed_by == "apprafter"));
        assert!(matches!(
            r.resolve_in(ToolId::Cue, &|_| false),
            Err(CoreError::Cli(cli_core::CliError::CueNotFound))
        ));
    }

    #[test]
    fn the_cue_override_is_a_path_or_a_name() {
        let p = path_of(&["/a"]);
        let abs = ToolResolver::new(&p, Some(Path::new("/opt/cue")), false);
        assert_eq!(
            abs.resolve_in(ToolId::Cue, &|f| f == Path::new("/opt/cue"))
                .unwrap(),
            PathBuf::from("/opt/cue")
        );
        assert!(abs.resolve_in(ToolId::Cue, &|_| false).is_err());
        let name = ToolResolver::new(&p, Some(Path::new("cue2")), false);
        assert_eq!(
            name.resolve_in(ToolId::Cue, &|f| f == Path::new("/a/cue2"))
                .unwrap(),
            PathBuf::from("/a/cue2")
        );
        let empty = ToolResolver::new(&p, Some(Path::new("")), false);
        assert!(matches!(
            empty.resolve_in(ToolId::Cue, &|_| true),
            Err(CoreError::Cli(cli_core::CliError::CueNotFound))
        ));
    }

    #[cfg(unix)]
    fn script(dir: &Path, name: &str, body: &str) {
        use std::os::unix::fs::PermissionsExt;
        let f = dir.join(name);
        std::fs::write(&f, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    /// `sleep` by its absolute path, from the test process's own `PATH`: a probed child's
    /// `PATH` is the search path (a temp dir here), where a bare `sleep` is not found, and the
    /// shell would exit at once instead of running into the timeout.
    #[cfg(unix)]
    fn sleep_bin() -> String {
        let path = std::env::var_os("PATH").unwrap_or_default();
        cli_core::tools::find_on_path("sleep", &path, cli_core::tools::is_executable_file)
            .expect("a `sleep` on the test's PATH")
            .display()
            .to_string()
    }

    #[cfg(unix)]
    #[test]
    fn a_probe_reads_the_version_and_bounds_the_spawn() {
        let dir = tempfile::tempdir().unwrap();
        script(dir.path(), "git", "echo git version 2.99");
        script(dir.path(), "helm", &format!("exec '{}' 10", sleep_bin()));
        let p = dir.path().as_os_str().to_owned();
        let r = ToolResolver::new(&p, None, false);
        let cancel = CancellationToken::new();
        let git = r.probe(ToolId::Git, Duration::from_secs(5), &cancel);
        assert_eq!(
            (git.version.as_deref(), git.problem.clone()),
            (Some("git version 2.99"), None)
        );
        let helm = r.probe(ToolId::Helm, Duration::from_millis(200), &cancel);
        assert_eq!(helm.problem, Some(ToolProblem::TimedOut));
        let restic = r.probe(ToolId::Restic, Duration::from_secs(1), &cancel);
        assert_eq!(restic.problem, Some(ToolProblem::NotFound));
        assert!(
            !restic.install.is_empty(),
            "a missing tool carries its install lines"
        );
    }

    /// An asdf / mise shim with no version set, or the macOS `git` stub without the developer
    /// tools: one error line on stderr and a non-zero exit. That line is not a version.
    #[cfg(unix)]
    #[test]
    fn a_tool_that_fails_reports_no_version_whatever_it_printed() {
        let dir = tempfile::tempdir().unwrap();
        script(
            dir.path(),
            "kubectl",
            "echo 'mise ERROR No version is set for command kubectl' >&2; exit 1",
        );
        let p = dir.path().as_os_str().to_owned();
        let kubectl = ToolResolver::new(&p, None, false).probe(
            ToolId::Kubectl,
            Duration::from_secs(5),
            &CancellationToken::new(),
        );
        assert_eq!(
            (kubectl.version, kubectl.problem),
            (
                None,
                Some(ToolProblem::NoVersionOutput {
                    exit: Some(1),
                    // Decision 3: the row says why.
                    detail: Some("mise ERROR No version is set for command kubectl".into()),
                })
            )
        );
        assert!(kubectl.path.is_some(), "it was found, and ran");
    }

    #[test]
    fn no_version_output_carries_its_detail_on_the_wire() {
        assert_eq!(
            serde_json::to_value(ToolProblem::NoVersionOutput {
                exit: Some(1),
                detail: Some("why".into()),
            })
            .unwrap(),
            serde_json::json!({"kind": "no_version_output", "exit": 1, "detail": "why"})
        );
    }

    #[test]
    fn the_detail_is_the_first_line_stderr_first_trimmed() {
        assert_eq!(
            no_version_detail(b"\n  \n  shim: no version set  \nmore\n", b"usage: x\n"),
            Some("shim: no version set".into())
        );
        assert_eq!(
            no_version_detail(b"", b"\n usage: x \n"),
            Some("usage: x".into()),
            "stdout when stderr is empty"
        );
        assert_eq!(no_version_detail(b" \n", b""), None, "nothing printed");
    }

    #[test]
    fn the_detail_is_bounded_and_printable() {
        let long = "e".repeat(10) + " " + &"x ".repeat(300);
        let got = no_version_detail(long.as_bytes(), b"").unwrap();
        assert_eq!(got.chars().count(), NO_VERSION_DETAIL_MAX_CHARS);
        // A tool's colours and other control characters never reach a terminal or the GUI.
        assert_eq!(
            no_version_detail(b"\x1b[31mmise ERROR\x1b[0m no\x07 version", b"").as_deref(),
            Some("mise ERROR no version")
        );
    }

    #[test]
    fn the_detail_never_shows_a_token_shaped_run() {
        // The child inherits the CLI's environment (HCLOUD_TOKEN included), and a broken shim can
        // print anything: a run of 32+ ASCII letters and digits — a Hetzner token, an age key —
        // is never shown, wherever it falls against the length bound.
        let token = "a".repeat(64);
        let got = no_version_detail(format!("bad token {token} here").as_bytes(), b"").unwrap();
        assert_eq!(got, "bad token [redacted] here");
        let at_the_edge = format!("{} {token}", "p".repeat(180));
        let got = no_version_detail(at_the_edge.as_bytes(), b"").unwrap();
        assert!(!got.contains("aaaaaaaa"), "{got}");
        // Shorter runs (words, versions, short hashes) stay.
        let short = "v1.31.0 deadbeefdeadbeef";
        assert_eq!(
            no_version_detail(short.as_bytes(), b"").as_deref(),
            Some(short)
        );
    }

    /// A script whose `#!` interpreter is absent (or a binary whose dynamic loader is): the
    /// file was found and is executable, and exec fails with ENOENT. The tool is installed.
    #[cfg(unix)]
    #[test]
    fn a_found_tool_whose_interpreter_is_missing_is_not_reported_missing() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let helm = dir.path().join("helm");
        std::fs::write(&helm, "#!/nonexistent/interpreter\n").unwrap();
        std::fs::set_permissions(&helm, std::fs::Permissions::from_mode(0o755)).unwrap();
        let p = dir.path().as_os_str().to_owned();
        let status = ToolResolver::new(&p, None, false).probe(
            ToolId::Helm,
            Duration::from_secs(5),
            &CancellationToken::new(),
        );
        assert_eq!(status.path, Some(helm.display().to_string()));
        match status.problem {
            Some(ToolProblem::SpawnFailed { ref error }) => {
                assert!(error.contains("interpreter"), "{error}")
            }
            other => panic!("{other:?}"),
        }
    }

    #[cfg(unix)]
    #[test]
    fn the_toolchain_reports_every_tool_in_order_with_its_search_path() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = Context::for_desktop(dir.path().into(), "http://unused")
            .with_tool_search_path(dir.path().as_os_str().to_owned(), PathSource::Explicit);
        let report = toolchain(&ctx, &CancellationToken::new()).unwrap();
        assert_eq!(
            report.tools.iter().map(|t| t.tool).collect::<Vec<_>>(),
            ToolId::ALL
        );
        assert_eq!(report.search_path, vec![dir.path().display().to_string()]);
        assert_eq!(report.search_path_source, PathSource::Explicit);
    }

    #[cfg(unix)]
    #[test]
    fn a_command_runs_with_the_search_path() {
        let dir = tempfile::tempdir().unwrap();
        script(dir.path(), "kubectl", "true");
        let p = dir.path().as_os_str().to_owned();
        let cmd = ToolResolver::new(&p, None, false)
            .command(ToolId::Kubectl)
            .unwrap();
        assert!(cmd
            .get_envs()
            .any(|(k, v)| k == "PATH" && v == Some(p.as_os_str())));
    }
}
