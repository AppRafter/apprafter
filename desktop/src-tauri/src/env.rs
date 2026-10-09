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
//!   `git`, `ssh`, `cue`) are looked for, and the `PATH` they get ([`desktop_host`]). An
//!   inherited `PATH` decides which `kubectl` runs: a recorded trust decision (spec rev 4 §10).
//!   macOS does not read it — an app started from Finder has launchd's bare `PATH` — and asks
//!   the account's interactive login shell for its `PATH` instead (five-second limit), falling
//!   back to [`MACOS_FALLBACK_PATH`];
//! - in a test build only (cargo feature `test-build`), `APPRAFTER_HCLOUD_BASE_URL`, which the
//!   core then accepts only as a loopback `http://` URL ([`desktop_context`]), and
//!   `APPRAFTER_DESKTOP_TEST_PASSWORD`, the password the fake authenticator's own field accepts,
//!   so a walk can use the lock screen's password field ([`crate::auth::choice`]);
//!
//! and `None` for every other name, however it is set. [`AllowListEnv::from_process`] is the one
//! place in `src/` that reads `std::env`: `tests/env_guard.rs` fails on any other.

use std::ffi::OsString;
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

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
/// and the runtime dir (`<app data dir>/run`).
pub fn desktop_host(env: &AllowListEnv, runtime_dir: PathBuf) -> DesktopHost {
    let (tool_search_path, tool_search_path_source) = tool_search_path(env);
    DesktopHost {
        runtime_dir,
        tool_search_path,
        tool_search_path_source,
    }
}

/// Linux and Windows: the allow-listed `PATH`, raw.
#[cfg(not(target_os = "macos"))]
fn tool_search_path(env: &AllowListEnv) -> (OsString, PathSource) {
    (
        env.var_os(PATH_ENV).unwrap_or_default(),
        PathSource::Environment,
    )
}

/// macOS: the account's login shell's `PATH` (an app started from Finder has launchd's).
#[cfg(target_os = "macos")]
fn tool_search_path(_env: &AllowListEnv) -> (OsString, PathSource) {
    login_shell_path()
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

/// The account's login shell's `PATH` ([`probe_login_shell`]); [`MACOS_FALLBACK_PATH`] when
/// that gives none.
#[cfg(target_os = "macos")]
fn login_shell_path() -> (OsString, PathSource) {
    match account_shell().and_then(|shell| probe_login_shell(login_shell_command(&shell))) {
        Some(path) => (path, PathSource::LoginShell),
        None => {
            tracing::warn!(
                "the login shell gave no PATH; tools are looked for in {MACOS_FALLBACK_PATH}"
            );
            (OsString::from(MACOS_FALLBACK_PATH), PathSource::Fallback)
        }
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
        use std::os::unix::fs::PermissionsExt;
        // The script, spelled out rather than taken from LOGIN_SHELL_SCRIPT: it is the contract
        // with the shell, so a change to it must change this test too.
        const EXPECTED_SCRIPT: &str = "echo __APPRAFTER_PATH__; /usr/bin/printenv PATH";
        let shell = dir.join("fake-login-shell");
        std::fs::write(
            &shell,
            format!(
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
        )
        .unwrap();
        std::fs::set_permissions(&shell, std::fs::Permissions::from_mode(0o755)).unwrap();
        for _ in 0..200 {
            match std::process::Command::new(&shell).arg("__probe").status() {
                Err(e) if e.raw_os_error() == Some(libc::ETXTBSY) => {
                    std::thread::sleep(std::time::Duration::from_millis(5))
                }
                _ => break,
            }
        }
        shell
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
        let host = desktop_host(&env, PathBuf::from("/data/run"));
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
}
