// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The one way a test in `cli/` runs the `apprafter` binary or the `platform-cli` shim (D.4a T1,
//! ADR 0068 §9); `tests/spawn_guard.rs` keeps every other way out.
//!
//! A [`Spawn`] is a scratch machine for the child: one temporary root holding a home, the four
//! XDG directories, a runtime directory (0700 on Unix), a temp directory, a target store and an
//! empty `bin`, all gone when the `Spawn` drops. The child's environment is built from nothing
//! (D.4a overview §3.16):
//!
//! - inherited: nothing on Unix; on Windows only [`WINDOWS_INHERITED`], without which a Windows
//!   process cannot start its runtime or open a socket;
//! - `HOME`, and on Unix `XDG_CONFIG_HOME`, `XDG_CACHE_HOME`, `XDG_DATA_HOME`, `XDG_STATE_HOME`,
//!   `XDG_RUNTIME_DIR` and `TMPDIR`, on Windows `TMP`, `TEMP`, `USERPROFILE`, `APPDATA` and
//!   `LOCALAPPDATA`, all inside the root. Windows resolves the profile through Known Folders,
//!   which ignore the environment, so there `APPRAFTER_AGE_KEY` and `APPRAFTER_SSH_PRIVATE_KEY`
//!   name files in the root too; on Unix `HOME` moves both, and they stay unset (a test of a
//!   locked store must not meet the override refusal of ADR 0068 §6);
//! - `APPRAFTER_CONFIG_DIR` (the root's `apprafter-config`, or [`Spawn::with_store`]),
//!   `APPRAFTER_SANDBOX` (the root and every store a test adds), `APPRAFTER_SKIP_STARTUP_CHECKS=1`
//!   unless [`Spawn::with_startup_checks`], `KUBECONFIG` naming a file that does not exist,
//!   `APPRAFTER_HCLOUD_BASE_URL` on the test's mock or [`CLOSED_PORT_URL`], and `TZ=UTC`,
//!   `LANG=C`, `USER=golden` (the goldens' values).
//!
//! The child has no controlling terminal (Unix: it starts a session of its own) and no console
//! (Windows: `DETACHED_PROCESS`), so a prompt can never read the keyboard of whoever runs the
//! tests (GOTCHA-179). A test that needs a terminal gives the child its own pseudo-terminal
//! ([`Spawn::std_at_terminal`]); the Windows Ctrl-Break test shares the console
//! ([`Spawn::sharing_console`]); the spawn point's self-tests run other programs under the same
//! rules ([`Spawn::probe_program`]). Each opt-in is allow-listed in `tests/spawn_guard.rs`.
//!
//! Keep the `Spawn` alive while its commands run: bind it (`let sp = Spawn::new();`) and let a
//! helper build its command from a `&Spawn` it is given, never from one it makes and drops — the
//! root would be gone, and the binary refuses a sandbox root that does not exist.

use std::ffi::{OsStr, OsString};
#[cfg(unix)]
use std::os::fd::OwnedFd;
use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;
use std::time::Duration;

use tempfile::TempDir;

/// The fence `apprafter` honours (`src/sandbox.rs`).
pub const SANDBOX_ENV: &str = "APPRAFTER_SANDBOX";
/// Longest a single `apprafter` run may take before it counts as a hang.
pub const RUN_TIMEOUT: Duration = Duration::from_secs(60);
/// Where Hetzner calls go when a test has no mock: a closed local port.
pub const CLOSED_PORT_URL: &str = "http://127.0.0.1:1";
/// Where every root is made on Unix, never `TMPDIR`: `/tmp/.tmpXXXXXX` is 15 characters on every
/// machine — the length the goldens were recorded with (miette wraps a path before the harness
/// masks it, GOTCHA-100) — and a holder socket under it fits `sun_path`.
#[cfg(unix)]
pub const SANDBOX_PARENT: &str = "/tmp";
/// What a Windows child inherits from the test: without these it cannot start its runtime or
/// open a socket.
#[cfg(windows)]
pub const WINDOWS_INHERITED: [&str; 5] =
    ["SystemRoot", "SystemDrive", "windir", "ComSpec", "PATHEXT"];

/// The directories every root holds; `run` is 0700 on Unix.
const DIRS: [&str; 9] = [
    "home",
    "config",
    "cache",
    "data",
    "state",
    "run",
    "tmp",
    "apprafter-config",
    "bin",
];

/// Which binary a [`Spawn`] runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bin {
    /// `apprafter`.
    Apprafter,
    /// The deprecated `platform-cli` shim, which runs the `apprafter` beside it.
    PlatformCliShim,
}

impl Bin {
    /// The path cargo built it at.
    pub fn path(self) -> &'static str {
        match self {
            Bin::Apprafter => env!("CARGO_BIN_EXE_apprafter"),
            Bin::PlatformCliShim => env!("CARGO_BIN_EXE_platform-cli"),
        }
    }
}

/// How a child meets a terminal.
enum Terminal {
    /// None: a session of its own (Unix), no console (Windows, unless `sharing_console`).
    None,
    /// The test's pseudo-terminal, as the child's controlling terminal, on stdin and stderr.
    #[cfg(unix)]
    Pty(OwnedFd),
}

/// A scratch machine for one test's children (module docs).
pub struct Spawn {
    root: TempDir,
    /// `with_store`'s directory; the root's `apprafter-config` otherwise.
    store: Option<PathBuf>,
    /// Directories outside the root that join `APPRAFTER_SANDBOX` (stores a test made itself).
    extra_roots: Vec<PathBuf>,
    bin: Bin,
    hcloud: Option<String>,
    /// Replaces the empty `bin` as `PATH` (stand-in tools).
    search_path: Option<PathBuf>,
    extra_env: Vec<(OsString, OsString)>,
    removed: Vec<OsString>,
    cwd: Option<PathBuf>,
    startup_checks: bool,
    #[cfg(windows)]
    shared_console: bool,
}

impl Default for Spawn {
    fn default() -> Self {
        Self::new()
    }
}

impl Spawn {
    /// A fresh root, running `apprafter`.
    pub fn new() -> Self {
        Self::of(Bin::Apprafter)
    }

    /// A fresh root, running the `platform-cli` shim.
    pub fn shim() -> Self {
        Self::of(Bin::PlatformCliShim)
    }

    fn of(bin: Bin) -> Self {
        #[cfg(unix)]
        let root = TempDir::new_in(SANDBOX_PARENT).expect("sandbox root");
        #[cfg(windows)]
        let root = TempDir::new().expect("sandbox root");
        for dir in DIRS {
            std::fs::create_dir_all(root.path().join(dir)).expect("sandbox dir");
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(
                root.path().join("run"),
                std::fs::Permissions::from_mode(0o700),
            )
            .expect("runtime dir 0700");
        }
        Spawn {
            root,
            store: None,
            extra_roots: Vec::new(),
            bin,
            hcloud: None,
            search_path: None,
            extra_env: Vec::new(),
            removed: Vec::new(),
            cwd: None,
            startup_checks: false,
            #[cfg(windows)]
            shared_console: false,
        }
    }

    /// The root every scratch directory lives in.
    pub fn root(&self) -> &Path {
        self.root.path()
    }

    /// `rel` under the root.
    pub fn path(&self, rel: &str) -> PathBuf {
        self.root.path().join(rel)
    }

    /// The child's `HOME` (and, on Windows, its `USERPROFILE`).
    pub fn home(&self) -> PathBuf {
        self.path("home")
    }

    /// The child's target store (`APPRAFTER_CONFIG_DIR`).
    pub fn store(&self) -> PathBuf {
        self.store
            .clone()
            .unwrap_or_else(|| self.path("apprafter-config"))
    }

    /// Where the child resolves its age key on every OS: under `HOME` on Unix, the file
    /// `APPRAFTER_AGE_KEY` names on Windows.
    pub fn age_key(&self) -> PathBuf {
        self.home()
            .join(".config")
            .join("apprafter")
            .join("age.key")
    }

    /// Use `dir` — a temporary directory the test made — as the store; it joins the sandbox.
    /// Panics on anything else: a test never hands the CLI a real store.
    pub fn with_store(mut self, dir: &Path) -> Self {
        assert_scratch(dir);
        self.store = Some(dir.to_path_buf());
        self.extra_roots.push(dir.to_path_buf());
        self
    }

    /// Point every Hetzner call at `url` (a mockito server).
    pub fn with_hcloud(mut self, url: String) -> Self {
        self.hcloud = Some(url);
        self
    }

    /// `PATH` is `dir` alone (stand-in tools) instead of the empty `bin`.
    pub fn with_path(mut self, dir: PathBuf) -> Self {
        self.search_path = Some(dir);
        self
    }

    /// One more variable, or one of the above replaced. A location the fence checks that lands
    /// outside the sandbox makes the binary refuse (`src/sandbox.rs`). Replacing
    /// `APPRAFTER_SANDBOX` itself is a fence test's alone (G1).
    pub fn with_env(mut self, key: &str, value: impl AsRef<OsStr>) -> Self {
        self.extra_env
            .push((OsString::from(key), value.as_ref().to_os_string()));
        self
    }

    /// One of the above left unset. Removing `HOME`, `APPRAFTER_CONFIG_DIR`, `XDG_RUNTIME_DIR` or
    /// `APPRAFTER_SANDBOX` is a fence test's alone (G1).
    pub fn without_env(mut self, key: &str) -> Self {
        self.removed.push(OsString::from(key));
        self
    }

    /// The child's working directory (default: its home).
    pub fn current_dir(mut self, dir: &Path) -> Self {
        self.cwd = Some(dir.to_path_buf());
        self
    }

    /// The startup checks stay on (no `APPRAFTER_SKIP_STARTUP_CHECKS`): `startup_checks_test.rs`
    /// alone (G1).
    pub fn with_startup_checks(mut self) -> Self {
        self.startup_checks = true;
        self
    }

    /// The child shares this test's console, in a process group of its own
    /// (`CREATE_NEW_PROCESS_GROUP`), so a console event reaches it alone: the Windows Ctrl-Break
    /// test of `doctor_test.rs` alone (G1).
    #[cfg(windows)]
    pub fn sharing_console(mut self) -> Self {
        self.shared_console = true;
        self
    }

    /// `apprafter <args>` (or the shim) as a `std::process::Command`, for a test that wires the
    /// child's stdio or waits for it itself. No timeout.
    pub fn std(&self, args: &[&str]) -> StdCommand {
        self.command(OsStr::new(self.bin.path()), args, Terminal::None)
    }

    /// [`Spawn::std`] as an `assert_cmd::Command`, bounded by [`RUN_TIMEOUT`].
    pub fn cmd(&self, args: &[&str]) -> assert_cmd::Command {
        let mut c = assert_cmd::Command::from_std(self.std(args));
        c.timeout(RUN_TIMEOUT);
        c
    }

    /// [`Spawn::std`] with stdin and stderr on `slave`, made the child's controlling terminal
    /// (`setsid` + `TIOCSCTTY`); stdout is the caller's to wire. The child leads its session, so
    /// a process it starts ends with it (D.4a T3: prompt tests only).
    #[cfg(unix)]
    pub fn std_at_terminal(&self, args: &[&str], slave: OwnedFd) -> StdCommand {
        self.command(OsStr::new(self.bin.path()), args, Terminal::Pty(slave))
    }

    /// Another program under the same environment and terminal rules: the spawn point's
    /// self-tests alone (G1).
    pub fn probe_program(&self, program: &Path, args: &[&str]) -> StdCommand {
        self.command(program.as_os_str(), args, Terminal::None)
    }

    /// [`Spawn::probe_program`] at `slave`, as [`Spawn::std_at_terminal`] does it.
    #[cfg(unix)]
    pub fn probe_program_at_terminal(
        &self,
        program: &Path,
        args: &[&str],
        slave: OwnedFd,
    ) -> StdCommand {
        self.command(program.as_os_str(), args, Terminal::Pty(slave))
    }

    /// `text` with every spelling of the root (as made, and canonical: macOS `/tmp` is
    /// `/private/tmp`) replaced by `<SANDBOX>`, longest first, so two runs compare.
    pub fn mask_root(&self, text: &str) -> String {
        let mut spellings = vec![self.root().display().to_string()];
        if let Ok(canonical) = self.root().canonicalize() {
            let canonical = canonical.display().to_string();
            if !spellings.contains(&canonical) {
                spellings.push(canonical);
            }
        }
        spellings.sort_by_key(|s| std::cmp::Reverse(s.len()));
        spellings.iter().fold(text.to_string(), |s, root| {
            s.replace(root.as_str(), "<SANDBOX>")
        })
    }

    fn command(&self, program: &OsStr, args: &[&str], terminal: Terminal) -> StdCommand {
        let mut c = StdCommand::new(program);
        c.env_clear();
        #[cfg(windows)]
        for name in WINDOWS_INHERITED {
            if let Some(value) = std::env::var_os(name) {
                c.env(name, value);
            }
        }
        c.env(
            "PATH",
            self.search_path.clone().unwrap_or_else(|| self.path("bin")),
        )
        .env("HOME", self.home())
        .env("APPRAFTER_CONFIG_DIR", self.store())
        .env(SANDBOX_ENV, self.sandbox_value())
        .env("KUBECONFIG", self.path("no-kubeconfig"))
        .env(
            "APPRAFTER_HCLOUD_BASE_URL",
            self.hcloud.as_deref().unwrap_or(CLOSED_PORT_URL),
        )
        .env("TZ", "UTC")
        .env("LANG", "C")
        .env("USER", "golden");
        #[cfg(unix)]
        c.env("XDG_CONFIG_HOME", self.path("config"))
            .env("XDG_CACHE_HOME", self.path("cache"))
            .env("XDG_DATA_HOME", self.path("data"))
            .env("XDG_STATE_HOME", self.path("state"))
            .env("XDG_RUNTIME_DIR", self.path("run"))
            .env("TMPDIR", self.path("tmp"));
        #[cfg(windows)]
        c.env("TMP", self.path("tmp"))
            .env("TEMP", self.path("tmp"))
            .env("USERPROFILE", self.home())
            .env("APPDATA", self.path("config"))
            .env("LOCALAPPDATA", self.path("data"))
            .env("APPRAFTER_AGE_KEY", self.age_key())
            .env(
                "APPRAFTER_SSH_PRIVATE_KEY",
                self.home().join(".ssh").join("id_ed25519"),
            );
        if !self.startup_checks {
            c.env("APPRAFTER_SKIP_STARTUP_CHECKS", "1");
        }
        for (key, value) in &self.extra_env {
            c.env(key, value);
        }
        for key in &self.removed {
            c.env_remove(key);
        }
        c.current_dir(self.cwd.clone().unwrap_or_else(|| self.home()))
            .args(args);
        self.detach(&mut c, terminal);
        c
    }

    #[cfg(unix)]
    fn detach(&self, c: &mut StdCommand, terminal: Terminal) {
        use std::os::unix::process::CommandExt as _;
        match terminal {
            Terminal::None => {
                // SAFETY: setsid(2) is async-signal-safe and the closure touches no memory of
                // this process. Never with `process_group(0)`: a group leader's setsid fails.
                unsafe {
                    c.pre_exec(|| {
                        if libc::setsid() == -1 {
                            return Err(std::io::Error::last_os_error());
                        }
                        Ok(())
                    });
                }
            }
            Terminal::Pty(slave) => {
                let stdin = slave.try_clone().expect("dup the pty slave");
                c.stdin(std::process::Stdio::from(stdin))
                    .stderr(std::process::Stdio::from(slave));
                // SAFETY: as above, and ioctl(2) is async-signal-safe; std puts the slave on fd 0
                // before it runs pre_exec closures.
                unsafe {
                    c.pre_exec(|| {
                        if libc::setsid() == -1 || libc::ioctl(0, libc::TIOCSCTTY as _, 0) == -1 {
                            return Err(std::io::Error::last_os_error());
                        }
                        Ok(())
                    });
                }
            }
        }
    }

    #[cfg(windows)]
    fn detach(&self, c: &mut StdCommand, terminal: Terminal) {
        use std::os::windows::process::CommandExt as _;
        use windows_sys::Win32::System::Threading::{CREATE_NEW_PROCESS_GROUP, DETACHED_PROCESS};
        match terminal {
            Terminal::None => {}
        }
        c.creation_flags(if self.shared_console {
            CREATE_NEW_PROCESS_GROUP
        } else {
            DETACHED_PROCESS
        });
    }

    fn sandbox_value(&self) -> OsString {
        let roots =
            std::iter::once(self.root().to_path_buf()).chain(self.extra_roots.iter().cloned());
        std::env::join_paths(roots).expect("the sandbox roots join as a path list")
    }
}

/// Panics unless `dir` lies, canonically, below the system temp directory or (Unix)
/// [`SANDBOX_PARENT`]: a store a test hands its child is scratch, never a real one.
fn assert_scratch(dir: &Path) {
    let canonical = dir
        .canonicalize()
        .unwrap_or_else(|e| panic!("with_store({}): {e}", dir.display()));
    // Arrays, one per OS: a `vec!` only iterated is clippy's `useless_vec` on Windows, where
    // nothing is pushed.
    #[cfg(unix)]
    let parents = [std::env::temp_dir(), PathBuf::from(SANDBOX_PARENT)];
    #[cfg(windows)]
    let parents = [std::env::temp_dir()];
    let inside = parents
        .iter()
        .filter_map(|p| p.canonicalize().ok())
        .any(|p| canonical.starts_with(&p) && canonical != p);
    assert!(
        inside,
        "with_store({}): a store a test hands the CLI must be a temporary directory",
        dir.display()
    );
}

/// A pseudo-terminal of 24x100 as (master, slave), both close-on-exec: `Stdio` dups the slave
/// onto a child's own fds, which clears the flag there, and neither may leak into a child another
/// test spawns meanwhile. `openpty` sets the size on the open slave on Linux (glibc, musl) and
/// macOS alike; TIOCSWINSZ on the master before the slave is open works on Linux only. The two
/// platforms declare the pointers `*const` (Linux) and `*mut` (macOS): a `*mut` fits both.
#[cfg(unix)]
pub fn open_pty() -> (OwnedFd, OwnedFd) {
    use std::os::fd::FromRawFd;
    let mut ws = libc::winsize {
        ws_row: 24,
        ws_col: 100,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    let (mut m, mut s) = (-1, -1);
    // SAFETY: `openpty` writes two fds into `m` and `s` and reads `ws`; nothing else is passed.
    let opened = unsafe {
        libc::openpty(
            &mut m,
            &mut s,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &raw mut ws,
        )
    };
    assert_eq!(opened, 0, "openpty: {}", std::io::Error::last_os_error());
    for fd in [m, s] {
        // SAFETY: fcntl on an fd `openpty` just returned.
        let set = unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) };
        assert_eq!(set, 0, "FD_CLOEXEC: {}", std::io::Error::last_os_error());
    }
    // SAFETY: both fds came from `openpty` and are owned here alone.
    unsafe { (OwnedFd::from_raw_fd(m), OwnedFd::from_raw_fd(s)) }
}
