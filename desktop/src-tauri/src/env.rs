// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! What the desktop reads from its process environment: an allow-list, nothing more (ADR 0067
//! §2).
//!
//! A desktop started from a terminal inherits that terminal's whole environment —
//! `HCLOUD_TOKEN`, `APPRAFTER_AGE_KEY`, the `APPRAFTER_HCLOUD_BASE_URL` a developer exported for
//! the CLI — and every cluster tab would then act on that one provider project. So the app reads
//! its environment only through an [`AllowListEnv`], which answers:
//!
//! - `APPRAFTER_CONFIG_DIR`: the target store root, the one the CLI opens too;
//! - `APPRAFTER_DESKTOP_DATA_DIR`: the desktop's own data directory, so a walk never touches the
//!   owner's settings and logs ([`data_dir_override`]), and never finds the owner's running
//!   instance ([`instance_identifier`]). It fails closed: set but empty or not Unicode, the app
//!   refuses to start rather than fall back on the owner's own files;
//! - on Linux and Windows, `PATH`: where the tools the app runs (`kubectl`, `helm`, `restic`,
//!   `git`, `ssh`, `cue`) are looked for, and the `PATH` they get ([`tool_search_path`]). An
//!   inherited `PATH` decides which `kubectl` runs: a recorded trust decision (spec rev 4 §10).
//!   macOS does not read it — an app started from Finder has launchd's bare `PATH` — and asks
//!   the account's interactive login shell for its `PATH` instead (five-second limit), falling
//!   back to [`MACOS_FALLBACK_PATH`]. It asks on a thread of its own from the start, so the
//!   window never waits for a slow profile; the first lookup of a tool waits for the answer,
//!   at most [`TOOL_PATH_WAIT`] from the start ([`ToolSearchPath`]);
//! - in a test build only (cargo feature `test-build`), `APPRAFTER_HCLOUD_BASE_URL`, which the
//!   core then accepts only as a loopback `http://` URL ([`desktop_context`]), and
//!   `APPRAFTER_DESKTOP_TEST_PASSWORD`, the password the fake authenticator's own field accepts,
//!   so a walk can use the lock screen's password field ([`crate::auth::choice`]);
//!
//! and `None` for every other name, however it is set. [`AllowListEnv::from_process`] is where the
//! app reads its own variables.
//!
//! On Linux this file also holds the one workaround the app applies through the environment:
//! WebKitGTK's DMA-BUF renderer closes the window with a Wayland protocol error on NVIDIA's
//! driver, so there [`turn_off_dmabuf_renderer_on_nvidia_wayland`] restarts the app at once
//! with `WEBKIT_DISABLE_DMABUF_RENDERER=1`, unless the user set it — a restart, not a
//! `set_var`, since a thread of WebKitGTK's runs before `main`. It reads the display variables
//! GTK reads, that one and the restart's mark, which counts only in the process whose ID it
//! names ([`GraphicsFacts::from_process`]): none of them is a setting of the app, and none
//! reaches the core.
//!
//! Nowhere else in `src/` reads or writes `std::env`: `tests/env_guard.rs` fails on any other
//! file.

use std::ffi::OsString;
use std::fmt;
use std::io;
use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering::SeqCst};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use apprafter_core::context::{HCLOUD_BASE_URL_ENV, PATH_ENV};
use apprafter_core::{Context, CoreResult, DesktopHost, DesktopPolicy, EnvSource, PathSource};
use cli_core::CONFIG_DIR_ENV;

/// Points the desktop's own files (settings, logs) at another directory.
pub const DATA_DIR_ENV: &str = "APPRAFTER_DESKTOP_DATA_DIR";

/// A test build's fake authenticator gets a password field accepting this password. Never read
/// in a release: no owner's password is ever in the environment.
pub const TEST_PASSWORD_ENV: &str = "APPRAFTER_DESKTOP_TEST_PASSWORD";

/// How an [`AllowListEnv`] reads a name it allows: the raw value, as the OS holds it.
type Lookup = dyn Fn(&str) -> Option<OsString> + Send + Sync;

/// The process environment, seen through the desktop's allow-list (see the module docs).
pub struct AllowListEnv {
    test_build: bool,
    lookup: Box<Lookup>,
}

impl AllowListEnv {
    /// The process environment. The app passes `cfg!(feature = "test-build")`.
    ///
    /// Through [`EnvSource::var`] a value that is not valid Unicode reads as unset, as
    /// [`EnvSource`] specifies (like `std::env::var(..).ok()`): the core takes every one of
    /// these values as a `String`. [`var_os`](Self::var_os) has the raw value, for the one
    /// name that must tell set-but-unreadable from unset ([`data_dir_override`]).
    pub fn from_process(test_build: bool) -> Self {
        Self::with_lookup(test_build, |key| std::env::var_os(key))
    }

    /// The same allow-list over `lookup` in place of the process environment, so a test never
    /// mutates the real one. `lookup` is asked only for a name the allow-list lets through.
    pub fn with_lookup(
        test_build: bool,
        lookup: impl Fn(&str) -> Option<OsString> + Send + Sync + 'static,
    ) -> Self {
        AllowListEnv {
            test_build,
            lookup: Box::new(lookup),
        }
    }

    /// Whether this is a test build's view: it then also answers `APPRAFTER_HCLOUD_BASE_URL` and
    /// `APPRAFTER_DESKTOP_TEST_PASSWORD`, and [`desktop_context`] builds under
    /// [`DesktopPolicy::TEST_BUILD`].
    pub fn test_build(&self) -> bool {
        self.test_build
    }

    /// The raw value of `key` when it is on the allow-list and set, Unicode or not; `None`
    /// for every other name.
    pub fn var_os(&self, key: &str) -> Option<OsString> {
        if self.allows(key) {
            (self.lookup)(key)
        } else {
            None
        }
    }

    /// Whether `key` is on this view's allow-list.
    pub fn allows(&self, key: &str) -> bool {
        allows_on(key, self.test_build, cfg!(target_os = "macos"))
    }

    fn policy(&self) -> DesktopPolicy {
        if self.test_build {
            DesktopPolicy::TEST_BUILD
        } else {
            DesktopPolicy::RELEASE
        }
    }
}

/// Whether `key` is on the allow-list for a view with `test_build`, on macOS or not. `PATH` is
/// read on Linux and Windows (the tool search path, a recorded trust decision: spec rev 4
/// §10); macOS asks the account's login shell instead ([`desktop_host`]).
fn allows_on(key: &str, test_build: bool, macos: bool) -> bool {
    key == CONFIG_DIR_ENV
        || key == DATA_DIR_ENV
        || (key == PATH_ENV && !macos)
        || (test_build && (key == HCLOUD_BASE_URL_ENV || key == TEST_PASSWORD_ENV))
}

impl EnvSource for AllowListEnv {
    fn var(&self, key: &str) -> Option<String> {
        self.var_os(key)?.into_string().ok()
    }

    /// The raw value, so a `PATH` that is not valid Unicode survives.
    fn var_os(&self, key: &str) -> Option<OsString> {
        AllowListEnv::var_os(self, key)
    }
}

impl fmt::Debug for AllowListEnv {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AllowListEnv")
            .field("test_build", &self.test_build)
            .finish_non_exhaustive()
    }
}

/// What only this process knows about where the core may look and write: the tool search path
/// as it is known now — on macOS, before the login shell answered, its fallback
/// ([`ToolSearchPath::now`]: this never waits) — and the runtime dir (`<app data dir>/run`).
/// What runs tools takes the path from [`ToolSearchPath::get`] instead
/// ([`Shell::tool_context`](crate::app::Shell::tool_context)).
pub fn desktop_host(tools: &ToolSearchPath, runtime_dir: PathBuf) -> DesktopHost {
    let (tool_search_path, tool_search_path_source) = tools.now();
    DesktopHost {
        runtime_dir,
        tool_search_path,
        tool_search_path_source,
    }
}

/// Linux and Windows: the allow-listed `PATH`, raw, known at once.
#[cfg(not(target_os = "macos"))]
pub fn tool_search_path(env: &AllowListEnv) -> ToolSearchPath {
    ToolSearchPath::known(
        env.var_os(PATH_ENV).unwrap_or_default(),
        PathSource::Environment,
    )
}

/// macOS: the account's login shell's `PATH` (an app started from Finder has launchd's), asked
/// from now on a thread of its own; [`MACOS_FALLBACK_PATH`] when it gives none.
#[cfg(target_os = "macos")]
pub fn tool_search_path(_env: &AllowListEnv) -> ToolSearchPath {
    ToolSearchPath::probe(
        || {
            let path =
                account_shell().and_then(|shell| probe_login_shell(login_shell_command(&shell)));
            if path.is_none() {
                tracing::warn!(
                    "the login shell gave no PATH; tools are looked for in {MACOS_FALLBACK_PATH}"
                );
            }
            path
        },
        PathSource::LoginShell,
        (OsString::from(MACOS_FALLBACK_PATH), PathSource::Fallback),
        TOOL_PATH_WAIT,
    )
}

/// How long the first lookup of a tool waits for the login shell's answer, counted from when
/// the app asked: the probe's own five-second bound and a second's grace. Past it the lookup
/// takes the fallback.
pub const TOOL_PATH_WAIT: Duration = Duration::from_secs(6);

/// The tool search path and where it came from, as the app learns it. Off macOS it is known at
/// once ([`known`](Self::known)). On macOS the login shell is asked on a thread of its own,
/// started with the app ([`probe`](Self::probe)), so nothing on the way to the window waits
/// for a slow profile: the app's context is built with what is known by then
/// ([`now`](Self::now)), and only what needs a tool waits for the answer ([`get`](Self::get)),
/// at most until its wait has passed since the app asked; past it, the fallback, until the
/// answer comes after all. Clones share the one answer.
#[derive(Debug, Clone)]
pub struct ToolSearchPath(Arc<ToolPathState>);

#[derive(Debug)]
struct ToolPathState {
    /// Set once, by the probe's thread (or at once by [`ToolSearchPath::known`]).
    answer: Mutex<Option<(OsString, PathSource)>>,
    answered: Condvar,
    /// [`ToolSearchPath::get`] never waits past it.
    deadline: Instant,
    /// The path before the answer, and in its place when there is none.
    fallback: (OsString, PathSource),
    /// The fallback taken for want of an answer in time is logged once.
    warned: AtomicBool,
}

impl ToolPathState {
    fn new(
        answer: Option<(OsString, PathSource)>,
        fallback: (OsString, PathSource),
        wait: Duration,
    ) -> Self {
        Self {
            answer: Mutex::new(answer),
            answered: Condvar::new(),
            deadline: Instant::now() + wait,
            fallback,
            warned: AtomicBool::new(false),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Option<(OsString, PathSource)>> {
        self.answer.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn answer(&self, answer: (OsString, PathSource)) {
        *self.lock() = Some(answer);
        self.answered.notify_all();
    }
}

impl ToolSearchPath {
    /// A path known now.
    pub fn known(path: OsString, source: PathSource) -> Self {
        let known = (path, source);
        Self(Arc::new(ToolPathState::new(
            Some(known.clone()),
            known,
            Duration::ZERO,
        )))
    }

    /// `probe`'s path (from `source`), asked from now on a thread of its own (`path-probe`), and
    /// `fallback` when it gives none, panics, or no thread can be had. [`get`](Self::get) waits
    /// for it until `wait` has passed from now.
    pub fn probe(
        probe: impl FnOnce() -> Option<OsString> + Send + 'static,
        source: PathSource,
        fallback: (OsString, PathSource),
        wait: Duration,
    ) -> Self {
        let state = Arc::new(ToolPathState::new(None, fallback, wait));
        let answering = Arc::clone(&state);
        let spawned = thread::Builder::new()
            .name("path-probe".into())
            .spawn(move || {
                let found = panic::catch_unwind(AssertUnwindSafe(probe)).ok().flatten();
                let answer = match found {
                    Some(path) => (path, source),
                    None => answering.fallback.clone(),
                };
                answering.answer(answer);
            });
        if let Err(e) = spawned {
            tracing::warn!(
                "no thread to ask for the tool search path on ({e}); the fallback holds"
            );
            state.answer(state.fallback.clone());
        }
        Self(state)
    }

    /// What is known now, never waiting: the answer, or the fallback while there is none.
    pub fn now(&self) -> (OsString, PathSource) {
        let answer = self.0.lock().clone();
        answer.unwrap_or_else(|| self.0.fallback.clone())
    }

    /// The answer, waiting for it until the wait given at the start has passed; the fallback
    /// past that, until the answer comes.
    pub fn get(&self) -> (OsString, PathSource) {
        let state = &*self.0;
        let remaining = state.deadline.saturating_duration_since(Instant::now());
        let (answer, _) = state
            .answered
            .wait_timeout_while(state.lock(), remaining, |answer| answer.is_none())
            .unwrap_or_else(PoisonError::into_inner);
        if let Some(answer) = &*answer {
            return answer.clone();
        }
        drop(answer);
        if !state.warned.swap(true, SeqCst) {
            tracing::warn!(
                "the tool search path was not known in time; tools are looked for in {:?} until it is",
                state.fallback.0
            );
        }
        state.fallback.clone()
    }
}

/// The marker line, one literal for both [`PATH_MARKER`] and [`LOGIN_SHELL_SCRIPT`], so the
/// script never prints a marker the parser does not look for.
macro_rules! path_marker {
    () => {
        "__APPRAFTER_PATH__"
    };
}

/// The line before the `PATH` in the login shell's output.
const PATH_MARKER: &str = path_marker!();

/// `printenv` prints the exported, `:`-joined `PATH` in every shell (fish included, which
/// expands a quoted `"$PATH"` to a space-joined list).
#[cfg(any(target_os = "macos", all(test, unix)))]
const LOGIN_SHELL_SCRIPT: &str = concat!("echo ", path_marker!(), "; /usr/bin/printenv PATH");

/// How the account's shell is asked for its `PATH`: as an interactive (`-i`) login (`-l`)
/// shell, as VS Code's shell-environment resolver asks it. A login shell that is not
/// interactive reads `.zprofile` but never `.zshrc` (bash: `.bash_profile`, not `.bashrc`), and
/// `.zshrc` is where many users, and the installers they ran, extend `PATH` (nvm, pyenv, krew,
/// mise, the Google Cloud SDK, Homebrew lines): without `-i` the app would miss tools a
/// Terminal window finds, and still report the login shell as the source. Whatever an rc file
/// prints before the marker is skipped ([`parse_login_shell_output`]); stdin is null, so an rc
/// file that prompts reads end of file; and `run_bounded` starts the shell in a session of its
/// own (no terminal to take over or to stop on) and kills it after [`LOGIN_SHELL_TIMEOUT`].
#[cfg(any(target_os = "macos", all(test, unix)))]
const LOGIN_SHELL_ARGS: [&str; 4] = ["-i", "-l", "-c", LOGIN_SHELL_SCRIPT];

/// How long the login shell may take before the probe gives up on it.
#[cfg(any(target_os = "macos", all(test, unix)))]
const LOGIN_SHELL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// The search path when the login shell gives none: the system's own directories.
pub const MACOS_FALLBACK_PATH: &str = "/usr/bin:/bin:/usr/sbin:/sbin";

/// The first non-empty line after the last marker line of a login shell's stdout (a profile may
/// print before it).
pub fn parse_login_shell_output(stdout: &[u8]) -> Option<OsString> {
    let text = String::from_utf8_lossy(stdout);
    let (_, after) = text.rsplit_once(PATH_MARKER)?;
    let line = after.lines().find(|l| !l.trim().is_empty())?.trim();
    Some(OsString::from(line))
}

/// `shell`, asked for its `PATH` with [`LOGIN_SHELL_ARGS`].
#[cfg(any(target_os = "macos", all(test, unix)))]
fn login_shell_command(shell: &Path) -> std::process::Command {
    let mut cmd = std::process::Command::new(shell);
    cmd.args(LOGIN_SHELL_ARGS);
    cmd
}

/// Run a [`login_shell_command`] (stdin null, killed with everything it started after
/// [`LOGIN_SHELL_TIMEOUT`]): the `PATH` it printed after the marker, or `None`.
#[cfg(any(target_os = "macos", all(test, unix)))]
fn probe_login_shell(cmd: std::process::Command) -> Option<OsString> {
    match apprafter_core::process::run_bounded(
        cmd,
        LOGIN_SHELL_TIMEOUT,
        &apprafter_core::CancellationToken::new(),
    ) {
        Ok(out) if !out.timed_out => parse_login_shell_output(&out.stdout),
        _ => None,
    }
}

/// The account's login shell from the password database (`SHELL` is an environment read).
#[cfg(target_os = "macos")]
fn account_shell() -> Option<PathBuf> {
    // SAFETY: `passwd` is a plain C struct; all-zero is a valid (empty) value.
    let mut pwd: libc::passwd = unsafe { std::mem::zeroed() };
    let mut buf = vec![0 as libc::c_char; 4096];
    let mut result: *mut libc::passwd = std::ptr::null_mut();
    // SAFETY: every pointer refers to a live, correctly sized buffer owned by this frame.
    let rc = unsafe {
        libc::getpwuid_r(
            libc::getuid(),
            &mut pwd,
            buf.as_mut_ptr(),
            buf.len(),
            &mut result,
        )
    };
    if rc != 0 || result.is_null() || pwd.pw_shell.is_null() {
        return None;
    }
    // SAFETY: `pw_shell` points into `buf`, NUL-terminated by getpwuid_r.
    let shell = unsafe { std::ffi::CStr::from_ptr(pwd.pw_shell) }
        .to_str()
        .ok()?;
    (!shell.is_empty()).then(|| PathBuf::from(shell))
}

/// The core's [`Context`] for this process: the store root from `APPRAFTER_CONFIG_DIR` (else the
/// platform default the CLI uses too), and the real Hetzner API — or, in a test build, the
/// loopback mock `APPRAFTER_HCLOUD_BASE_URL` names; any other value there is refused with
/// [`CoreError::UnsafeOverride`](apprafter_core::CoreError::UnsafeOverride). Every CLI override
/// the process inherited (`HCLOUD_TOKEN` first) is ignored. The tool search path and the
/// runtime dir come from `host` ([`desktop_host`]).
///
/// The policy follows `env`'s own build flag ([`AllowListEnv::test_build`]), so a release view
/// can never be paired with the test-build policy, nor the other way round.
pub fn desktop_context(env: &AllowListEnv, host: DesktopHost) -> CoreResult<Context> {
    Context::from_desktop_env(env, env.policy(), host)
}

/// Why the data-directory override cannot be used: the app refuses to start (exit code 2)
/// rather than run on the owner's own files.
#[derive(Debug, thiserror::Error)]
pub enum DataDirError {
    #[error("{DATA_DIR_ENV} is set but empty: unset it, or point it at a directory")]
    Empty,
    #[error("{DATA_DIR_ENV} is not valid Unicode ({0:?}): point it at a directory whose path is")]
    NotUnicode(OsString),
    #[error("{DATA_DIR_ENV} names {}, which cannot be used: {error}", dir.display())]
    Unusable { dir: PathBuf, error: io::Error },
}

/// Where the desktop keeps its own files in place of the platform's app-data directory:
/// `APPRAFTER_DESKTOP_DATA_DIR` when set; `Ok(None)` when unset. A walk points it at its
/// scratch directory, and [`prepare_data_dir`] makes it the one path every use agrees on.
///
/// It fails closed: set but empty, or set to a value that is not valid Unicode, is an error,
/// never "unset" — a walk whose variable broke must not run on the owner's settings and logs,
/// nor focus the owner's running app.
pub fn data_dir_override(env: &AllowListEnv) -> Result<Option<PathBuf>, DataDirError> {
    let Some(raw) = env.var_os(DATA_DIR_ENV) else {
        return Ok(None);
    };
    if raw.is_empty() {
        return Err(DataDirError::Empty);
    }
    let dir = raw.into_string().map_err(DataDirError::NotUnicode)?;
    Ok(Some(PathBuf::from(dir)))
}

/// The override `dir` as every use of it must see it: created when missing, then canonical —
/// absolute (a relative one against the working directory, as the CLI reads
/// `APPRAFTER_CONFIG_DIR`), with no `.`, `..`, trailing separator or symbolic link left. So
/// `x`, `x/`, `x/./` and `y/../x` are one directory, one instance ([`instance_identifier`])
/// and one set of app directories. On Windows the path keeps its usual `C:\…` form, not the
/// verbatim `\\?\` one `std::fs::canonicalize` gives (`dunce`, as Tauri resolves its own).
pub fn prepare_data_dir(dir: &Path) -> Result<PathBuf, DataDirError> {
    let unusable = |error| DataDirError::Unusable {
        dir: dir.to_path_buf(),
        error,
    };
    std::fs::create_dir_all(dir).map_err(unusable)?;
    dunce::canonicalize(dir).map_err(unusable)
}

/// The identifier the single-instance lock is keyed on: `base` itself, or, with a data-dir
/// override, `base.t<16 lowercase hex digits>` — so a walk's instance never finds (and focuses)
/// the owner's, nor two walks on different directories each other.
///
/// The digits are FNV-1a 64 over the directory as given — the app passes it through
/// [`prepare_data_dir`] first — as raw OS bytes: the bytes themselves on Unix, the UTF-16 code
/// units in little-endian order on Windows. FNV is fixed by its definition, unlike
/// `DefaultHasher`, so the identifier stays the same across runs and Rust releases. The `t`
/// keeps the new element a valid D-Bus name element (`[A-Za-z_][A-Za-z0-9_]*`, never a leading
/// digit): on Linux the single-instance plugin registers the identifier on the session bus.
pub fn instance_identifier(base: &str, data_dir: Option<&Path>) -> String {
    match data_dir {
        None => base.to_string(),
        Some(dir) => format!("{base}.t{:016x}", instance_hash(dir)),
    }
}

/// The webview's data store with a data-dir override, on macOS (see
/// [`window::build_main`](crate::window::build_main)): the 16 hex digits of
/// [`instance_identifier`]'s suffix, as bytes — so the store changes exactly when the instance
/// does, and stays the same across runs.
pub fn data_store_identifier(data_dir: &Path) -> [u8; 16] {
    let hash = instance_hash(data_dir);
    std::array::from_fn(|i| HEX_DIGITS[((hash >> (60 - 4 * i)) & 0xf) as usize])
}

const HEX_DIGITS: &[u8; 16] = b"0123456789abcdef";

/// FNV-1a 64 over `dir`'s raw OS bytes.
fn instance_hash(dir: &Path) -> u64 {
    fnv1a64(os_bytes(dir))
}

const FNV_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

/// FNV-1a, 64 bits.
fn fnv1a64(bytes: impl IntoIterator<Item = u8>) -> u64 {
    bytes.into_iter().fold(FNV_OFFSET_BASIS, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(FNV_PRIME)
    })
}

/// A path's raw OS bytes: the bytes themselves on Unix.
#[cfg(unix)]
fn os_bytes(path: &Path) -> impl Iterator<Item = u8> + '_ {
    use std::os::unix::ffi::OsStrExt;
    path.as_os_str().as_bytes().iter().copied()
}

/// A path's raw OS bytes: its UTF-16 code units, each little-endian, on Windows.
#[cfg(windows)]
fn os_bytes(path: &Path) -> impl Iterator<Item = u8> + '_ {
    use std::os::windows::ffi::OsStrExt;
    path.as_os_str().encode_wide().flat_map(u16::to_le_bytes)
}

/// WebKitGTK's own switch for its DMA-BUF renderer, read once, when the first web view needs a
/// renderer: set to anything but `0` (an empty value included) the renderer is off; set to `0`,
/// or unset, it is on (WebKitGTK 2.52, `UIProcess/gtk/AcceleratedBackingStore.cpp`:
/// `if (disableDMABuf && g_strcmp0(disableDMABuf, "0")) return;`).
#[cfg(target_os = "linux")]
pub const DMABUF_RENDERER_ENV: &str = "WEBKIT_DISABLE_DMABUF_RENDERER";

/// Set, to the process's ID, in the environment of the process the app restarts into with the
/// renderer off ([`restart_with_dmabuf_renderer_off`]; `exec` keeps the ID): in the process it
/// names, the `1` in [`DMABUF_RENDERER_ENV`] is the app's, not the user's. Neither can be taken
/// out of the environment again (the thread `set_var` would race is running), so the programs
/// the app starts inherit both; to them the mark names another process, and the variable reads
/// as one the user set ([`names_this_process`]).
#[cfg(target_os = "linux")]
pub const DMABUF_RESTARTED_ENV: &str = "APPRAFTER_DESKTOP_DMABUF_RESTARTED";

/// Whether the restart's `mark` names the process `pid`: its ID in decimal, exactly as the
/// restart writes it.
#[cfg(target_os = "linux")]
pub fn names_this_process(mark: Option<&std::ffi::OsStr>, pid: u32) -> bool {
    mark.is_some_and(|mark| mark == pid.to_string().as_str())
}

/// The facts [`dmabuf_renderer`] decides on, as [`GraphicsFacts::from_process`] finds them.
#[cfg(target_os = "linux")]
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GraphicsFacts {
    /// `WAYLAND_DISPLAY`: the compositor's socket libwayland connects to.
    pub wayland_display: Option<OsString>,
    /// `XDG_SESSION_TYPE`: `wayland` in a Wayland session, whose socket is libwayland's default
    /// when `WAYLAND_DISPLAY` is unset.
    pub xdg_session_type: Option<OsString>,
    /// `GDK_BACKEND`: the display backends GTK tries, in order.
    pub gdk_backend: Option<OsString>,
    /// NVIDIA's kernel driver is loaded (`/sys/module/nvidia`, `/proc/driver/nvidia/version`).
    pub nvidia_driver: bool,
    /// [`DMABUF_RENDERER_ENV`] as the process has it.
    pub dmabuf_renderer: Option<OsString>,
    /// [`DMABUF_RESTARTED_ENV`] names this process: it is the app's restart.
    pub restarted: bool,
}

#[cfg(target_os = "linux")]
impl GraphicsFacts {
    /// This process's facts. The one place besides [`AllowListEnv::from_process`] that reads
    /// the environment: three names GTK reads to pick its display, the one WebKitGTK reads for
    /// its renderer, and the app's mark of its own restart; none of them a setting of the app.
    pub fn from_process() -> Self {
        let exists = |path: &str| Path::new(path).exists();
        let mark = std::env::var_os(DMABUF_RESTARTED_ENV);
        GraphicsFacts {
            wayland_display: std::env::var_os("WAYLAND_DISPLAY"),
            xdg_session_type: std::env::var_os("XDG_SESSION_TYPE"),
            gdk_backend: std::env::var_os("GDK_BACKEND"),
            nvidia_driver: exists("/sys/module/nvidia") || exists("/proc/driver/nvidia/version"),
            dmabuf_renderer: std::env::var_os(DMABUF_RENDERER_ENV),
            restarted: names_this_process(mark.as_deref(), std::process::id()),
        }
    }

    /// Whether GTK 3 opens a Wayland display. It tries the backends `GDK_BACKEND` lists in
    /// order, all of them (`wayland` first) when it is unset: so the first entry must be
    /// `wayland` or `*`. Its Wayland backend connects where libwayland does: `WAYLAND_DISPLAY`
    /// (set but empty, nowhere — GTK goes on to X11), or `wayland-0` when that is unset, which
    /// is the socket of the Wayland session `XDG_SESSION_TYPE` names.
    pub fn is_wayland(&self) -> bool {
        let backend_allows = match &self.gdk_backend {
            None => true,
            Some(list) => {
                let list = list.to_string_lossy();
                matches!(list.split(',').next(), Some("wayland" | "*"))
            }
        };
        let socket = match &self.wayland_display {
            Some(display) => !display.is_empty(),
            None => self.xdg_session_type.as_deref() == Some("wayland".as_ref()),
        };
        backend_allows && socket
    }
}

/// What the app does with WebKitGTK's DMA-BUF renderer at start ([`dmabuf_renderer`]).
#[cfg(target_os = "linux")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DmabufRenderer {
    /// Not NVIDIA's driver under Wayland: WebKitGTK decides as it always does.
    Untouched,
    /// NVIDIA's driver under Wayland, where WebKitGTK's DMA-BUF renderer closes the window with
    /// a Wayland protocol error (`Error 71`), and the variable unset: the app restarts with
    /// [`DMABUF_RENDERER_ENV`] set to `1`. The decision only: the restart replaces the process.
    Restart,
    /// This process is that restart (the mark names it): the app turned the renderer off.
    TurnedOff,
    /// NVIDIA's driver under Wayland, and the user has set [`DMABUF_RENDERER_ENV`] (to this
    /// value): theirs stands.
    UserSet(OsString),
    /// The restart failed (why): the renderer stays on.
    RestartFailed(String),
}

#[cfg(target_os = "linux")]
impl DmabufRenderer {
    /// One line in the log for what the app did: nothing when it left the renderer alone. Called
    /// once the log has started, long after the decision.
    pub fn log(&self) {
        match self {
            // `turn_off_dmabuf_renderer_on_nvidia_wayland` returns `Restart` never: the restart
            // replaces the process, or failed.
            Self::Untouched | Self::Restart => {}
            Self::TurnedOff => tracing::info!(
                "WebKitGTK's DMA-BUF renderer is off: the NVIDIA driver is loaded and the window \
                 is on Wayland, where the renderer closes the app with a Wayland protocol error \
                 (Error 71). The app restarted itself with {DMABUF_RENDERER_ENV}=1; starting it \
                 with {DMABUF_RENDERER_ENV}=0 keeps the renderer on"
            ),
            Self::UserSet(value) => tracing::info!(
                "{DMABUF_RENDERER_ENV}={value:?} is set: the app leaves WebKitGTK's DMA-BUF \
                 renderer as that says, although the NVIDIA driver is loaded under Wayland"
            ),
            Self::RestartFailed(error) => tracing::warn!(
                "WebKitGTK's DMA-BUF renderer stays on although the NVIDIA driver is loaded \
                 under Wayland: the app could not restart itself with it off ({error}). If the \
                 window closes with a Wayland protocol error (Error 71), start the app with \
                 {DMABUF_RENDERER_ENV}=1"
            ),
        }
    }
}

/// The decision, on `facts` alone: under Wayland with NVIDIA's driver loaded, restart with the
/// renderer off — unless this is that restart, or the user set the variable. A restarted
/// process never restarts again, even without the variable: that restart would be the same.
#[cfg(target_os = "linux")]
pub fn dmabuf_renderer(facts: &GraphicsFacts) -> DmabufRenderer {
    if !facts.nvidia_driver || !facts.is_wayland() {
        return DmabufRenderer::Untouched;
    }
    match &facts.dmabuf_renderer {
        None if facts.restarted => DmabufRenderer::RestartFailed(format!(
            "the restarted process has no {DMABUF_RENDERER_ENV}"
        )),
        None => DmabufRenderer::Restart,
        Some(value) if facts.restarted && value == "1" => DmabufRenderer::TurnedOff,
        Some(value) => DmabufRenderer::UserSet(value.clone()),
    }
}

/// Turn WebKitGTK's DMA-BUF renderer off when [`dmabuf_renderer`] says so for this process,
/// and say what was decided, for the log once it starts ([`DmabufRenderer::log`]). When it
/// says so, this does not return: the process restarts with the renderer off
/// ([`restart_with_dmabuf_renderer_off`]), unless that fails.
///
/// `run` calls it as its first statement, before the app starts anything: a restart then
/// throws nothing away. The variable has to be in the environment before WebKitGTK reads it,
/// and the process is never on one thread here — WebKitGTK's own library constructor starts
/// its allocator's scavenger thread before `main` (libpas, `pas_scavenger`) — so a `set_var`
/// could change the environment under a thread that reads it. The restart puts the variable in
/// the new image's initial environment instead: nothing is written while anything runs.
#[cfg(target_os = "linux")]
pub fn turn_off_dmabuf_renderer_on_nvidia_wayland() -> DmabufRenderer {
    apply_dmabuf_renderer(&GraphicsFacts::from_process())
}

/// [`turn_off_dmabuf_renderer_on_nvidia_wayland`] on `facts`: the decision, carried out. Does
/// not return when it restarts.
#[cfg(target_os = "linux")]
pub fn apply_dmabuf_renderer(facts: &GraphicsFacts) -> DmabufRenderer {
    match dmabuf_renderer(facts) {
        DmabufRenderer::Restart => {
            DmabufRenderer::RestartFailed(restart_with_dmabuf_renderer_off().to_string())
        }
        decision => decision,
    }
}

/// Replace this process with this program again — same process, same arguments and name, the
/// same environment plus [`DMABUF_RENDERER_ENV`]`=1` and [`DMABUF_RESTARTED_ENV`] set to the
/// process's ID. Returns only when that failed, or cannot be done ([`restart_command`]), with
/// why.
#[cfg(target_os = "linux")]
pub fn restart_with_dmabuf_renderer_off() -> io::Error {
    use std::os::unix::process::CommandExt;

    let restart = std::env::current_exe()
        .and_then(|exe| restart_command(&exe, std::env::args_os(), std::process::id()));
    match restart {
        Ok(mut command) => command.exec(),
        Err(error) => error,
    }
}

/// The restart's command: `exe`, given `args` (the first one the program's name, as it was
/// started), in the inherited environment plus the variable and the mark naming the process
/// `pid`. None when `exe` is the dynamic loader ([`is_dynamic_loader`]): the process was
/// started as `ld.so <program> …`, and the loader took its options and the program's path out
/// of the arguments, so the restart would run the loader with nothing to load.
#[cfg(target_os = "linux")]
fn restart_command(
    exe: &Path,
    args: impl IntoIterator<Item = OsString>,
    pid: u32,
) -> io::Result<std::process::Command> {
    use std::os::unix::process::CommandExt;

    if is_dynamic_loader(exe) {
        return Err(io::Error::other(format!(
            "it was started through the dynamic loader {}, which a restart cannot repeat",
            exe.display()
        )));
    }
    let mut args = args.into_iter();
    let mut command = std::process::Command::new(exe);
    if let Some(name) = args.next() {
        command.arg0(name);
    }
    command
        .args(args)
        .env(DMABUF_RENDERER_ENV, "1")
        .env(DMABUF_RESTARTED_ENV, pid.to_string());
    Ok(command)
}

/// Whether `exe`, this process's image, is the dynamic loader rather than a program: glibc's
/// `ld-linux*.so.*` (`ld64.so.*` on ppc64 and s390x, `ld.so.*` on some others) or musl's
/// `ld-musl-*.so.*`.
#[cfg(target_os = "linux")]
fn is_dynamic_loader(exe: &Path) -> bool {
    exe.file_name().is_some_and(|name| {
        let name = name.to_string_lossy();
        ["ld-linux", "ld-musl", "ld.so", "ld64.so"]
            .iter()
            .any(|loader| name.starts_with(loader))
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::{Arc, Mutex};

    use apprafter_core::{CliOverrides, CoreError, MapEnv};

    use super::*;

    /// Everything a terminal might hand the app: the names on the list (`PATH` off macOS) and
    /// some it must never see.
    const AMBIENT: &[(&str, &str)] = &[
        ("APPRAFTER_CONFIG_DIR", "/tmp/store"),
        ("APPRAFTER_DESKTOP_DATA_DIR", "/tmp/walk"),
        ("APPRAFTER_HCLOUD_BASE_URL", "http://127.0.0.1:9"),
        ("APPRAFTER_DESKTOP_TEST_PASSWORD", "open sesame"),
        ("PATH", "/usr/bin:/bin"),
        ("HCLOUD_TOKEN", "inherited-token"),
        ("APPRAFTER_AGE_KEY", "/tmp/age.key"),
        ("APPRAFTER_SSH_PRIVATE_KEY", "/tmp/id"),
        ("APPRAFTER_SERVER_TYPE", "cx99"),
        ("KUBECONFIG", "/tmp/kubeconfig"),
        ("HOME", "/home/someone"),
    ];

    fn env_of(test_build: bool, pairs: &[(&str, &str)]) -> AllowListEnv {
        let pairs: Vec<(&str, OsString)> = pairs.iter().map(|(k, v)| (*k, v.into())).collect();
        env_of_os(test_build, &pairs)
    }

    /// [`env_of`] with raw values, Unicode or not.
    fn env_of_os(test_build: bool, pairs: &[(&str, OsString)]) -> AllowListEnv {
        let map: BTreeMap<String, OsString> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect();
        AllowListEnv::with_lookup(test_build, move |k| map.get(k).cloned())
    }

    /// Whether this platform's allow-list carries `PATH` (Linux and Windows; macOS asks the
    /// login shell instead).
    const PATH_LISTED: bool = !cfg!(target_os = "macos");

    #[test]
    fn a_release_build_answers_only_the_store_root_the_data_dir_and_path() {
        let env = env_of(false, AMBIENT);
        for (key, value) in AMBIENT {
            let expected = (matches!(*key, "APPRAFTER_CONFIG_DIR" | "APPRAFTER_DESKTOP_DATA_DIR")
                || (*key == "PATH" && PATH_LISTED))
                .then(|| value.to_string());
            assert_eq!(env.var(key), expected, "{key}");
        }
        assert!(!env.test_build());
    }

    #[test]
    fn a_test_build_also_answers_the_api_base_and_the_test_password_and_nothing_else() {
        let env = env_of(true, AMBIENT);
        for (key, value) in AMBIENT {
            let expected = (matches!(
                *key,
                "APPRAFTER_CONFIG_DIR"
                    | "APPRAFTER_DESKTOP_DATA_DIR"
                    | "APPRAFTER_HCLOUD_BASE_URL"
                    | "APPRAFTER_DESKTOP_TEST_PASSWORD"
            ) || (*key == "PATH" && PATH_LISTED))
                .then(|| value.to_string());
            assert_eq!(env.var(key), expected, "{key}");
        }
        assert!(env.test_build());
    }

    #[test]
    fn path_is_on_the_list_except_on_macos() {
        for test_build in [false, true] {
            assert!(
                allows_on("PATH", test_build, false),
                "Linux/Windows read PATH"
            );
            assert!(
                !allows_on("PATH", test_build, true),
                "macOS asks the login shell instead"
            );
        }
    }

    #[test]
    fn the_login_shell_output_yields_the_path_after_the_last_marker() {
        let out = b"Last login: x\n__APPRAFTER_PATH__\n/old\n__APPRAFTER_PATH__\n/opt/homebrew/bin:/usr/bin\n";
        assert_eq!(
            parse_login_shell_output(out),
            Some(OsString::from("/opt/homebrew/bin:/usr/bin"))
        );
        assert_eq!(parse_login_shell_output(b"no marker\n"), None);
        assert_eq!(parse_login_shell_output(b"__APPRAFTER_PATH__\n\n"), None);
    }

    /// A stand-in login shell in `dir`: it fails (64) unless asked exactly `-i -l -c <the
    /// probe's script>` (the real `/usr/bin/printenv` runs in the macOS tests), prints what a
    /// chatty profile might (a stray marker included), starts a job that keeps stdout open past
    /// its own exit, then answers as the script would. It is run once with `__probe` first, to
    /// wait out `ETXTBSY`: a sibling test thread that forks while the file is still open for
    /// writing holds a write handle to it until it execs.
    #[cfg(unix)]
    fn fake_login_shell(dir: &Path) -> PathBuf {
        // The script, spelled out rather than taken from LOGIN_SHELL_SCRIPT: it is the contract
        // with the shell, so a change to it must change this test too.
        const EXPECTED_SCRIPT: &str = "echo __APPRAFTER_PATH__; /usr/bin/printenv PATH";
        let shell = dir.join("fake-login-shell");
        install_script(
            &shell,
            &format!(
                r#"#!/bin/sh
case "$1" in __probe) exit 0;; esac
if [ "$#" != 4 ] || [ "$1 $2 $3" != "-i -l -c" ] || [ "$4" != '{EXPECTED_SCRIPT}' ]; then
    echo "unexpected argv: $*" >&2
    exit 64
fi
echo 'Last login: Thu Oct  9 10:00:00 on ttys000'
echo '{PATH_MARKER}'
echo '/from/a/profile/that/printed/the/marker'
sleep 3 &
echo '{PATH_MARKER}'
echo '/opt/homebrew/bin:/usr/bin:/bin'
"#
            ),
        );
        shell
    }

    /// Write `body` to `script`, executable, and run it once with `__probe` (it must exit 0 on
    /// that) to wait out `ETXTBSY`: a sibling test thread that forks while the file is still
    /// open for writing holds a write handle to it until it execs.
    #[cfg(unix)]
    fn install_script(script: &Path, body: &str) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::write(script, body).unwrap();
        std::fs::set_permissions(script, std::fs::Permissions::from_mode(0o755)).unwrap();
        for _ in 0..200 {
            match std::process::Command::new(script).arg("__probe").status() {
                Err(e) if e.raw_os_error() == Some(libc::ETXTBSY) => {
                    std::thread::sleep(std::time::Duration::from_millis(5))
                }
                _ => break,
            }
        }
    }

    /// A login shell with a slow profile: it answers as the probe's script would, `/slow/bin`,
    /// after `secs` seconds.
    #[cfg(unix)]
    fn slow_login_shell(dir: &Path, secs: u32) -> PathBuf {
        let shell = dir.join("slow-login-shell");
        install_script(
            &shell,
            &format!(
                "#!/bin/sh\ncase \"$1\" in __probe) exit 0;; esac\nsleep {secs}\necho '{PATH_MARKER}'\necho /slow/bin\n"
            ),
        );
        shell
    }

    fn fallback() -> (OsString, PathSource) {
        (OsString::from(MACOS_FALLBACK_PATH), PathSource::Fallback)
    }

    /// The macOS start, on every Unix against a slow stand-in shell: asking it holds up nothing
    /// — not the host the context is built from, which has the fallback meanwhile — and the
    /// first lookup of a tool waits for its answer.
    #[cfg(unix)]
    #[test]
    fn a_slow_login_shell_holds_up_the_first_tool_lookup_and_nothing_before_it() {
        let dir = tempfile::tempdir().unwrap();
        let shell = slow_login_shell(dir.path(), 2);
        let started = Instant::now();
        let tools = ToolSearchPath::probe(
            move || probe_login_shell(login_shell_command(&shell)),
            PathSource::LoginShell,
            fallback(),
            TOOL_PATH_WAIT,
        );
        let host = desktop_host(&tools, PathBuf::from("/data/run"));
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "the start waited {:?} for the login shell",
            started.elapsed()
        );
        assert_eq!(
            (host.tool_search_path, host.tool_search_path_source),
            fallback()
        );
        assert_eq!(
            tools.get(),
            (OsString::from("/slow/bin"), PathSource::LoginShell)
        );
        assert!(
            started.elapsed() >= Duration::from_secs(2),
            "it waited for the answer"
        );
        assert_eq!(
            tools.clone().now().1,
            PathSource::LoginShell,
            "clones share it"
        );
    }

    /// A shell slower than the wait: the lookup takes the fallback at the wait's end, later
    /// lookups take it at once, and the answer still counts once it comes.
    #[test]
    fn past_its_wait_a_lookup_takes_the_fallback_until_the_answer_comes() {
        let (release, released) = std::sync::mpsc::channel::<()>();
        let started = Instant::now();
        let tools = ToolSearchPath::probe(
            move || {
                let _ = released.recv_timeout(Duration::from_secs(30));
                Some(OsString::from("/late/bin"))
            },
            PathSource::LoginShell,
            fallback(),
            Duration::from_millis(200),
        );
        assert_eq!(tools.get(), fallback());
        let waited = started.elapsed();
        assert!(
            waited >= Duration::from_millis(200) && waited < Duration::from_secs(5),
            "{waited:?}"
        );
        let again = Instant::now();
        assert_eq!(tools.get(), fallback());
        assert!(
            again.elapsed() < Duration::from_millis(100),
            "no second wait"
        );
        release.send(()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while tools.now().1 != PathSource::LoginShell {
            assert!(Instant::now() < deadline, "the late answer never counted");
            thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(
            tools.get(),
            (OsString::from("/late/bin"), PathSource::LoginShell)
        );
    }

    /// No answer, or a probe that panicked: the fallback, without waiting out the wait.
    #[test]
    fn a_probe_that_finds_nothing_or_panics_gives_the_fallback_at_once() {
        let nothing = ToolSearchPath::probe(|| None, PathSource::LoginShell, fallback(), LONG_WAIT);
        let broken = ToolSearchPath::probe(
            || panic!("the probe broke"),
            PathSource::LoginShell,
            fallback(),
            LONG_WAIT,
        );
        let started = Instant::now();
        assert_eq!(nothing.get(), fallback());
        assert_eq!(broken.get(), fallback());
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "{:?}",
            started.elapsed()
        );
    }

    /// Longer than any test waits.
    const LONG_WAIT: Duration = Duration::from_secs(60);

    #[test]
    fn a_known_path_is_known_at_once() {
        let tools = ToolSearchPath::known("/a:/b".into(), PathSource::Environment);
        assert_eq!(tools.now(), ("/a:/b".into(), PathSource::Environment));
        assert_eq!(tools.get(), ("/a:/b".into(), PathSource::Environment));
    }

    /// The wait covers the probe's own bound, and a second more.
    #[cfg(unix)]
    #[test]
    fn the_wait_is_the_probe_s_bound_and_a_second() {
        assert_eq!(TOOL_PATH_WAIT, LOGIN_SHELL_TIMEOUT + Duration::from_secs(1));
    }

    /// The probe on every Unix, against a stand-in shell: asked interactive and login, it takes
    /// the `PATH` after the last marker, past the profile's noise and its background job.
    #[cfg(unix)]
    #[test]
    fn the_probe_asks_an_interactive_login_shell_and_takes_its_path() {
        let dir = tempfile::tempdir().unwrap();
        let shell = fake_login_shell(dir.path());
        assert_eq!(
            probe_login_shell(login_shell_command(&shell)),
            Some(OsString::from("/opt/homebrew/bin:/usr/bin:/bin"))
        );
    }

    /// macOS: the password database names the account's shell, a file on disk.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_account_shell_is_a_file_on_disk() {
        let shell = account_shell().expect("the password database names a login shell");
        assert!(
            shell.is_absolute() && shell.is_file(),
            "{}",
            shell.display()
        );
    }

    /// macOS: the real probe against `/bin/sh`, with a scratch `HOME` and `ZDOTDIR` and no
    /// `ENV`, so no rc file of the machine's own account is read. It must answer (`None` is
    /// what falls back to [`MACOS_FALLBACK_PATH`]) with a `PATH` that holds `/usr/bin`.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_real_probe_against_bin_sh_answers_with_usr_bin() {
        let home = tempfile::tempdir().unwrap();
        let mut cmd = login_shell_command(Path::new("/bin/sh"));
        cmd.env("HOME", home.path())
            .env("ZDOTDIR", home.path())
            .env_remove("ENV"); // an interactive `sh` reads the file it names
        let path = probe_login_shell(cmd).expect("the login shell printed a PATH");
        assert!(
            std::env::split_paths(&path).any(|dir| dir == Path::new("/usr/bin")),
            "{path:?}"
        );
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn the_host_takes_the_allow_listed_path() {
        let env = env_of(false, &[("PATH", "/usr/bin:/bin")]);
        let host = desktop_host(&tool_search_path(&env), PathBuf::from("/data/run"));
        assert_eq!(host.tool_search_path, OsString::from("/usr/bin:/bin"));
        assert_eq!(host.tool_search_path_source, PathSource::Environment);
        assert_eq!(host.runtime_dir, PathBuf::from("/data/run"));
    }

    /// The host the context tests pass: a fixed search path and runtime dir.
    fn test_host() -> DesktopHost {
        DesktopHost {
            runtime_dir: "/tmp/desk/run".into(),
            tool_search_path: "/opt/bin".into(),
            tool_search_path_source: PathSource::Explicit,
        }
    }

    #[test]
    fn the_lookup_is_never_asked_for_a_name_off_the_list() {
        for test_build in [false, true] {
            let asked = Arc::new(Mutex::new(Vec::<String>::new()));
            let env = AllowListEnv::with_lookup(test_build, {
                let asked = asked.clone();
                move |k| {
                    asked.lock().unwrap().push(k.to_string());
                    Some("set".into())
                }
            });
            for (key, _) in AMBIENT {
                let _ = env.var(key);
            }
            let asked = asked.lock().unwrap().clone();
            assert!(
                asked.iter().all(|k| env.allows(k)),
                "test_build={test_build}: {asked:?}"
            );
            let mut expected = vec!["APPRAFTER_CONFIG_DIR", "APPRAFTER_DESKTOP_DATA_DIR"];
            if test_build {
                expected.extend([
                    "APPRAFTER_DESKTOP_TEST_PASSWORD",
                    "APPRAFTER_HCLOUD_BASE_URL",
                ]);
            }
            if PATH_LISTED {
                expected.push("PATH");
            }
            let mut asked_sorted = asked;
            asked_sorted.sort();
            assert_eq!(asked_sorted, expected, "test_build={test_build}");
        }
    }

    /// The real environment, read and never written: whatever this test process inherited.
    #[test]
    fn from_process_reads_the_real_environment_through_the_list() {
        for test_build in [false, true] {
            let env = AllowListEnv::from_process(test_build);
            assert_eq!(env.test_build(), test_build);
            for key in [CONFIG_DIR_ENV, DATA_DIR_ENV] {
                assert_eq!(env.var(key), std::env::var(key).ok(), "{key}");
            }
            let base = test_build
                .then(|| std::env::var(HCLOUD_BASE_URL_ENV).ok())
                .flatten();
            assert_eq!(env.var(HCLOUD_BASE_URL_ENV), base);
            let password = test_build
                .then(|| std::env::var(TEST_PASSWORD_ENV).ok())
                .flatten();
            assert_eq!(env.var(TEST_PASSWORD_ENV), password);
            assert_eq!(
                env.var("PATH"),
                PATH_LISTED.then(|| std::env::var("PATH").ok()).flatten()
            );
            // `HOME` is set in any test process, so its `None` shows the list at work.
            assert!(std::env::var_os("HOME").is_some() || cfg!(windows));
            assert_eq!(env.var("HOME"), None);
            assert_eq!(env.var("HCLOUD_TOKEN"), None);
        }
    }

    #[test]
    fn a_name_on_the_list_reads_as_the_lookup_has_it() {
        let env = env_of(true, &[("APPRAFTER_CONFIG_DIR", "")]);
        assert_eq!(
            env.var("APPRAFTER_CONFIG_DIR").as_deref(),
            Some(""),
            "empty passes through: each consumer decides what empty means"
        );
        assert_eq!(env.var("APPRAFTER_DESKTOP_DATA_DIR"), None);
        assert_eq!(env.var("APPRAFTER_HCLOUD_BASE_URL"), None);
    }

    #[test]
    fn the_names_are_the_ones_the_core_and_the_cli_use() {
        assert_eq!(CONFIG_DIR_ENV, "APPRAFTER_CONFIG_DIR");
        assert_eq!(HCLOUD_BASE_URL_ENV, "APPRAFTER_HCLOUD_BASE_URL");
        assert_eq!(DATA_DIR_ENV, "APPRAFTER_DESKTOP_DATA_DIR");
        assert_eq!(TEST_PASSWORD_ENV, "APPRAFTER_DESKTOP_TEST_PASSWORD");
    }

    /// The API base a context gets when nothing redirects it.
    fn real_api_base() -> String {
        let env = MapEnv::new().with("APPRAFTER_CONFIG_DIR", "/tmp/store");
        let base = Context::from_desktop_env(&env, DesktopPolicy::RELEASE, test_host())
            .unwrap()
            .hcloud_base_url()
            .to_string();
        assert!(base.starts_with("https://"), "{base}");
        base
    }

    /// WI-430's acceptance: an ambient `HCLOUD_TOKEN` is ignored, and so is any API base.
    #[test]
    fn a_release_context_ignores_the_inherited_token_and_any_api_base() {
        for base in ["http://evil.example", "http://127.0.0.1:9"] {
            let env = env_of(
                false,
                &[
                    ("APPRAFTER_CONFIG_DIR", "/tmp/store"),
                    ("HCLOUD_TOKEN", "inherited-token"),
                    ("APPRAFTER_HCLOUD_BASE_URL", base),
                ],
            );
            let ctx = desktop_context(&env, test_host()).unwrap();
            assert_eq!(ctx.config_root(), Path::new("/tmp/store"));
            assert_eq!(ctx.overrides(), &CliOverrides::default());
            assert_eq!(ctx.hcloud_base_url(), real_api_base(), "{base}");
        }
    }

    #[test]
    fn a_test_build_context_honours_a_loopback_api_base_and_still_ignores_the_token() {
        let env = env_of(
            true,
            &[
                ("APPRAFTER_CONFIG_DIR", "/tmp/store"),
                ("HCLOUD_TOKEN", "inherited-token"),
                ("APPRAFTER_HCLOUD_BASE_URL", "http://127.0.0.1:9"),
            ],
        );
        let ctx = desktop_context(&env, test_host()).unwrap();
        assert_eq!(ctx.config_root(), Path::new("/tmp/store"));
        assert_eq!(ctx.hcloud_base_url(), "http://127.0.0.1:9");
        assert_eq!(ctx.overrides(), &CliOverrides::default());
    }

    #[test]
    fn a_test_build_context_refuses_a_non_loopback_api_base() {
        let env = env_of(
            true,
            &[
                ("APPRAFTER_CONFIG_DIR", "/tmp/store"),
                ("APPRAFTER_HCLOUD_BASE_URL", "http://evil.example"),
            ],
        );
        let err = desktop_context(&env, test_host()).unwrap_err();
        assert!(
            matches!(err, CoreError::UnsafeOverride { var, .. } if var == HCLOUD_BASE_URL_ENV),
            "{err:?}"
        );
    }

    #[test]
    fn a_test_build_context_without_an_api_base_uses_the_real_one() {
        let env = env_of(true, &[("APPRAFTER_CONFIG_DIR", "/tmp/store")]);
        assert_eq!(
            desktop_context(&env, test_host())
                .unwrap()
                .hcloud_base_url(),
            real_api_base()
        );
    }

    #[test]
    fn the_data_dir_override_is_a_path_when_set_and_nothing_when_unset() {
        for test_build in [false, true] {
            let set = env_of(test_build, &[("APPRAFTER_DESKTOP_DATA_DIR", "/tmp/walk")]);
            assert_eq!(
                data_dir_override(&set).unwrap(),
                Some(PathBuf::from("/tmp/walk"))
            );
            assert_eq!(data_dir_override(&env_of(test_build, &[])).unwrap(), None);
        }
    }

    /// A broken variable must not read as unset: the walk would run on the owner's files.
    #[test]
    fn a_data_dir_override_set_but_empty_or_not_unicode_is_refused() {
        for test_build in [false, true] {
            let empty = env_of(test_build, &[("APPRAFTER_DESKTOP_DATA_DIR", "")]);
            let err = data_dir_override(&empty).unwrap_err();
            assert!(matches!(err, DataDirError::Empty), "{err:?}");
            assert_eq!(
                err.to_string(),
                "APPRAFTER_DESKTOP_DATA_DIR is set but empty: unset it, or point it at a directory"
            );
            // Bytes that are no UTF-8 on Unix, an unpaired surrogate on Windows.
            #[cfg(unix)]
            let raw = {
                use std::os::unix::ffi::OsStringExt;
                OsString::from_vec(b"/tmp/walk-\xff".to_vec())
            };
            #[cfg(windows)]
            let raw = {
                use std::os::windows::ffi::OsStringExt;
                OsString::from_wide(&[u16::from(b'w'), 0xd800])
            };
            let env = env_of_os(test_build, &[("APPRAFTER_DESKTOP_DATA_DIR", raw.clone())]);
            assert_eq!(env.var(DATA_DIR_ENV), None, "unset, read as a String");
            let err = data_dir_override(&env).unwrap_err();
            assert!(
                matches!(&err, DataDirError::NotUnicode(got) if *got == raw),
                "{err:?}"
            );
            assert!(err.to_string().contains("not valid Unicode"), "{err}");
        }
    }

    #[test]
    fn a_prepared_data_dir_is_one_path_however_it_is_spelled() {
        let root = tempfile::tempdir().unwrap();
        // Canonical already, so the comparison below is not thrown by a symlinked temp dir.
        let root = dunce::canonicalize(root.path()).unwrap();
        std::fs::create_dir(root.join("y")).unwrap();
        let x = root.join("x");
        let spellings = [
            x.clone(),
            PathBuf::from(format!("{}/", x.display())),
            x.join("."),
            root.join("y").join("..").join("x"),
        ];
        for spelling in &spellings {
            let prepared = prepare_data_dir(spelling).unwrap();
            assert_eq!(prepared, x, "{spelling:?}");
            assert!(prepared.is_dir(), "{spelling:?}: created");
            assert_eq!(
                instance_identifier(BASE, Some(&prepared)),
                instance_identifier(BASE, Some(&x)),
                "{spelling:?}"
            );
            assert_eq!(
                data_store_identifier(&prepared),
                data_store_identifier(&x),
                "{spelling:?}"
            );
        }
        // Different spellings, hashed as given, would have been different instances.
        assert_ne!(
            instance_identifier(BASE, Some(&spellings[0])),
            instance_identifier(BASE, Some(&spellings[1]))
        );
    }

    #[test]
    fn a_data_dir_that_cannot_be_created_is_refused() {
        let root = tempfile::tempdir().unwrap();
        let file = root.path().join("a-file");
        std::fs::write(&file, b"").unwrap();
        let err = prepare_data_dir(&file.join("walk")).unwrap_err();
        assert!(matches!(err, DataDirError::Unusable { .. }), "{err:?}");
        assert!(err.to_string().contains("cannot be used"), "{err}");
    }

    #[test]
    fn the_data_store_is_the_identifiers_hex_digits_as_bytes() {
        let dir = Path::new("/tmp/walk");
        let id = instance_identifier(BASE, Some(dir));
        let digits = id.rsplit_once(".t").unwrap().1;
        assert_eq!(&data_store_identifier(dir), digits.as_bytes());
        assert_ne!(
            data_store_identifier(dir),
            data_store_identifier(Path::new("/tmp/walk2"))
        );
    }

    const BASE: &str = "dev.apprafter.desktop";

    /// Paths that differ in a trailing slash, a case, a byte, relativity — and one whose hash
    /// starts with a zero digit on both Unix and Windows, so the padding is exercised.
    const PATHS: &[&str] = &[
        "/tmp/walk",
        "/tmp/walk/",
        "/tmp/Walk",
        "/tmp/walk2",
        "walk",
        "/tmp/walk-60",
        "",
    ];

    #[test]
    fn without_an_override_the_identifier_is_the_base() {
        assert_eq!(instance_identifier(BASE, None), BASE);
    }

    /// Pinned: a changed hash would let a walk started by an older build and one started by a
    /// newer build both run, and is caught here first.
    #[test]
    fn the_identifier_suffix_is_pinned() {
        let expected = if cfg!(windows) {
            "dev.apprafter.desktop.t3776a7a6fa698a4d"
        } else {
            "dev.apprafter.desktop.t8c5108fba7cbf86d"
        };
        assert_eq!(
            instance_identifier(BASE, Some(Path::new("/tmp/walk"))),
            expected
        );
    }

    #[test]
    fn fnv1a64_matches_the_published_vectors() {
        assert_eq!(fnv1a64(*b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a64(*b"a"), 0xaf63_dc4c_8601_ec8c);
        assert_eq!(fnv1a64(*b"foobar"), 0x8594_4171_f739_67e8);
    }

    #[test]
    fn the_identifier_is_deterministic_distinct_and_a_valid_dbus_name() {
        let ids: Vec<String> = PATHS
            .iter()
            .map(|p| instance_identifier(BASE, Some(Path::new(p))))
            .collect();
        for (p, id) in PATHS.iter().zip(&ids) {
            assert_eq!(&instance_identifier(BASE, Some(Path::new(p))), id);
            let suffix = id
                .strip_prefix("dev.apprafter.desktop.t")
                .unwrap_or_else(|| panic!("{p:?}: {id}"));
            assert_eq!(suffix.len(), 16, "{p:?}: {id}");
            assert!(
                suffix
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
                "{p:?}: {id}"
            );
            for element in id.split('.') {
                assert!(is_dbus_name_element(element), "{p:?}: {element:?} in {id}");
            }
        }
        let mut distinct = ids.clone();
        distinct.sort();
        distinct.dedup();
        assert_eq!(distinct.len(), ids.len(), "{ids:?}");
        assert!(ids.iter().any(|id| id.contains(".t0")), "{ids:?}");
    }

    fn is_dbus_name_element(s: &str) -> bool {
        let mut bytes = s.bytes();
        bytes
            .next()
            .is_some_and(|b| b.is_ascii_alphabetic() || b == b'_')
            && bytes.all(|b| b.is_ascii_alphanumeric() || b == b'_')
    }

    /// The DMA-BUF renderer decision, on facts the test writes: never the process's own.
    #[cfg(target_os = "linux")]
    mod graphics {
        use std::ffi::OsStr;

        use super::*;
        use crate::runtime::logged;

        /// NVIDIA's driver in a Wayland session, the variable unset, before any restart.
        fn nvidia_wayland() -> GraphicsFacts {
            GraphicsFacts {
                wayland_display: Some("wayland-0".into()),
                xdg_session_type: Some("wayland".into()),
                gdk_backend: None,
                nvidia_driver: true,
                dmabuf_renderer: None,
                restarted: false,
            }
        }

        fn os(value: Option<&str>) -> Option<OsString> {
            value.map(OsString::from)
        }

        #[test]
        fn nvidia_s_driver_under_wayland_restarts_the_app_with_the_renderer_off() {
            assert_eq!(dmabuf_renderer(&nvidia_wayland()), DmabufRenderer::Restart);
        }

        #[test]
        fn without_nvidia_s_driver_or_without_wayland_nothing_changes() {
            let no_driver = GraphicsFacts {
                nvidia_driver: false,
                ..nvidia_wayland()
            };
            assert_eq!(dmabuf_renderer(&no_driver), DmabufRenderer::Untouched);
            let x11 = GraphicsFacts {
                wayland_display: None,
                xdg_session_type: Some("x11".into()),
                ..nvidia_wayland()
            };
            assert_eq!(dmabuf_renderer(&x11), DmabufRenderer::Untouched);
        }

        /// `WAYLAND_DISPLAY`, `XDG_SESSION_TYPE`, `GDK_BACKEND`, and whether that is Wayland.
        type Case = (
            Option<&'static str>,
            Option<&'static str>,
            Option<&'static str>,
            bool,
        );

        /// Wayland is the display GTK 3 opens: `GDK_BACKEND`'s first entry, `wayland` by default,
        /// and libwayland's socket — `WAYLAND_DISPLAY`, or `wayland-0` when it is unset, which a
        /// Wayland session (`XDG_SESSION_TYPE`) has.
        #[test]
        fn wayland_is_the_display_gtk_opens() {
            #[rustfmt::skip]
            let cases: &[Case] = &[
                // WAYLAND_DISPLAY, XDG_SESSION_TYPE, GDK_BACKEND
                (Some("wayland-0"), Some("wayland"), None, true),
                (Some("wayland-1"), None, None, true),
                // A compositor nested in an X11 session: GTK opens the compositor.
                (Some("wayland-1"), Some("x11"), None, true),
                // Unset: libwayland connects to wayland-0, the session's own.
                (None, Some("wayland"), None, true),
                (None, Some("x11"), None, false),
                (None, Some("tty"), None, false),
                (None, None, None, false),
                // Set but empty: libwayland finds no socket and GTK goes on to X11.
                (Some(""), Some("wayland"), None, false),
                (Some("wayland-0"), Some("wayland"), Some("x11"), false),
                (Some("wayland-0"), Some("wayland"), Some("x11,wayland"), false),
                (Some("wayland-0"), Some("wayland"), Some("broadway"), false),
                (Some("wayland-0"), Some("wayland"), Some("wayland"), true),
                (Some("wayland-0"), Some("wayland"), Some("wayland,x11"), true),
                (Some("wayland-0"), Some("wayland"), Some("*"), true),
                (None, Some("wayland"), Some("x11"), false),
            ];
            for &(display, session, backend, expected) in cases {
                let facts = GraphicsFacts {
                    wayland_display: os(display),
                    xdg_session_type: os(session),
                    gdk_backend: os(backend),
                    ..nvidia_wayland()
                };
                assert_eq!(
                    facts.is_wayland(),
                    expected,
                    "WAYLAND_DISPLAY={display:?} XDG_SESSION_TYPE={session:?} GDK_BACKEND={backend:?}"
                );
                let decision = dmabuf_renderer(&facts);
                let restart = decision == DmabufRenderer::Restart;
                assert_eq!(restart, expected, "{facts:?}: {decision:?}");
            }
        }

        /// WebKitGTK reads any value but `0` as off, an empty one too, and `0` as on: whatever
        /// the user set is theirs.
        #[test]
        fn a_value_the_user_set_stands_whatever_it_is() {
            for value in ["1", "0", "", "yes"] {
                let facts = GraphicsFacts {
                    dmabuf_renderer: Some(value.into()),
                    ..nvidia_wayland()
                };
                assert_eq!(
                    dmabuf_renderer(&facts),
                    DmabufRenderer::UserSet(value.into()),
                    "{value:?}"
                );
            }
        }

        /// The restarted process finds the variable it was given, `1`, and the mark: the app
        /// turned the renderer off. Any other value is the user's. A mark without the variable
        /// never restarts again: a restart that lost it would restart for ever.
        #[test]
        fn after_the_restart_the_app_knows_it_turned_the_renderer_off() {
            let restarted = |value: Option<&str>| GraphicsFacts {
                dmabuf_renderer: os(value),
                restarted: true,
                ..nvidia_wayland()
            };
            assert_eq!(
                dmabuf_renderer(&restarted(Some("1"))),
                DmabufRenderer::TurnedOff
            );
            assert_eq!(
                dmabuf_renderer(&restarted(Some("0"))),
                DmabufRenderer::UserSet("0".into())
            );
            let again = dmabuf_renderer(&restarted(None));
            assert!(
                matches!(&again, DmabufRenderer::RestartFailed(why) if why.contains(DMABUF_RENDERER_ENV)),
                "{again:?}"
            );
        }

        /// The restarted process finds its own ID in the mark, and only it does: a copy that a
        /// program inherited from the restarted app names another process.
        #[test]
        fn the_mark_names_the_restarted_process_alone() {
            let mark = |value: &str| Some(OsString::from(value));
            assert!(names_this_process(mark("4242").as_deref(), 4242));
            for other in ["4243", "1", "04242", "+4242", "4242 ", ""] {
                assert!(
                    !names_this_process(mark(other).as_deref(), 4242),
                    "{other:?}"
                );
            }
            assert!(!names_this_process(None, 4242));
        }

        /// The restart runs this program, with its own arguments and its own name, in the
        /// environment it has, plus the variable set to `1` and the mark naming the process
        /// (`exec` keeps its ID): nothing else changes.
        #[test]
        fn the_restart_is_this_program_with_its_arguments_and_the_renderer_off() {
            let args = ["apprafter-desktop", "--flag", "two words"].map(OsString::from);
            let command =
                restart_command(Path::new("/opt/bin/apprafter-desktop"), args, 4242).unwrap();
            assert_eq!(command.get_program(), "/opt/bin/apprafter-desktop");
            let given: Vec<_> = command.get_args().collect();
            assert_eq!(given, ["--flag", "two words"]);
            let mut envs: Vec<_> = command
                .get_envs()
                .map(|(k, v)| (k.to_owned(), v.map(OsStr::to_owned)))
                .collect();
            envs.sort();
            assert_eq!(
                envs,
                [
                    (
                        OsString::from(DMABUF_RESTARTED_ENV),
                        Some(OsString::from("4242"))
                    ),
                    (
                        OsString::from(DMABUF_RENDERER_ENV),
                        Some(OsString::from("1"))
                    ),
                ]
            );
        }

        /// Started as `ld-linux-x86-64.so.2 ./apprafter-desktop`, the process's image is the
        /// loader, and its arguments no longer hold the program or the loader's options: a
        /// restart would run the loader with nothing to load, and die before any window or log.
        /// So there is none, and the failure's warning says to set the variable. A program that
        /// is not a loader restarts, whatever its name or directory.
        #[test]
        fn a_process_started_through_the_dynamic_loader_is_not_restarted() {
            let args = || ["./apprafter-desktop"].map(OsString::from);
            for loader in [
                "/lib64/ld-linux-x86-64.so.2",
                "/lib/ld-linux-aarch64.so.1",
                "/lib/ld-linux.so.2",
                "/lib/ld-musl-x86_64.so.1",
                "/lib/ld.so.1",
                "/usr/lib/ld.so",
                "/lib64/ld64.so.2",
                "/nix/store/0000-glibc-2.40/lib/ld-linux-x86-64.so.2",
            ] {
                let error = restart_command(Path::new(loader), args(), 4242)
                    .expect_err(loader)
                    .to_string();
                assert!(
                    error.contains("dynamic loader") && error.contains(loader),
                    "{loader}: {error}"
                );
                let warning = logged(|| DmabufRenderer::RestartFailed(error.clone()).log());
                assert!(warning.contains(loader), "{warning}");
                assert!(
                    warning.contains("start the app with WEBKIT_DISABLE_DMABUF_RENDERER=1"),
                    "{warning}"
                );
            }
            for program in [
                "/opt/bin/apprafter-desktop",
                "/usr/bin/ld",
                "/usr/bin/ldd",
                "/tmp/ld.so.d/apprafter-desktop",
            ] {
                assert!(
                    restart_command(Path::new(program), args(), 4242).is_ok(),
                    "{program}"
                );
            }
        }

        #[test]
        fn the_names_are_webkit_s_and_the_app_s_own() {
            assert_eq!(DMABUF_RENDERER_ENV, "WEBKIT_DISABLE_DMABUF_RENDERER");
            assert_eq!(DMABUF_RESTARTED_ENV, "APPRAFTER_DESKTOP_DMABUF_RESTARTED");
        }

        /// One line says what the app did, why, and how to keep the renderer on.
        #[test]
        fn the_log_says_what_the_app_did_why_and_how_to_keep_the_renderer() {
            let off = logged(|| DmabufRenderer::TurnedOff.log());
            assert_eq!(off.lines().count(), 1, "{off}");
            assert!(off.contains(" INFO "), "{off}");
            for needle in [
                "NVIDIA",
                "Wayland",
                "Error 71",
                "WEBKIT_DISABLE_DMABUF_RENDERER=1",
                "WEBKIT_DISABLE_DMABUF_RENDERER=0 keeps",
            ] {
                assert!(off.contains(needle), "{needle:?} in {off}");
            }
            assert_eq!(logged(|| DmabufRenderer::Untouched.log()), "");
            assert_eq!(logged(|| DmabufRenderer::Restart.log()), "");
            let user = logged(|| DmabufRenderer::UserSet("0".into()).log());
            assert_eq!(user.lines().count(), 1, "{user}");
            assert!(user.contains(" INFO "), "{user}");
            assert!(
                user.contains("WEBKIT_DISABLE_DMABUF_RENDERER=\"0\""),
                "{user}"
            );
            let failed = logged(|| DmabufRenderer::RestartFailed("no exe".into()).log());
            assert_eq!(failed.lines().count(), 1, "{failed}");
            assert!(failed.contains(" WARN "), "{failed}");
            assert!(failed.contains("no exe"), "{failed}");
            assert!(
                failed.contains("WEBKIT_DISABLE_DMABUF_RENDERER=1"),
                "{failed}"
            );
        }
    }
}
