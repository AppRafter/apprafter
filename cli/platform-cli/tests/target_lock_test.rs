// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Every CLI edit of the target store waits for the store lock, and says
//! so.
//!
//! The test process holds `cli_core::StoreLock` on a seeded store and runs
//! one edit — `target use`, `rename`, `remove`, `machine`, `add --renew` —
//! as a real process, in the cleared environment `target_concurrency_test.rs`
//! uses. The edit must report the wait on stderr and still be running half a
//! second later; released, it must finish, successfully, within 30 s. Each
//! check fails if the command does not take the lock, and none depends on
//! timing when it does: the wait is observed by its stderr line, not by a
//! sleep long enough for an unlocked run to finish.
//!
//! The last test is the other side: a store that cannot be locked at all
//! (read-only) is used without the lock, with a warning, instead of making
//! a command that writes nothing fail.

use std::io::Read;
use std::path::PathBuf;
use std::process::{Child, Command, Output, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use cli_core::target::{
    save_global_config, save_target, GlobalConfig, StoreLock, Target, TargetConfig,
    TargetCredentials, TargetStorePaths, TARGET_STORE_VERSION,
};

/// 64 ASCII alphanumerics: the shape `cli_core::target` accepts.
const TOKEN_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const TOKEN_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
/// How long a command may take to report the wait, and then to finish once
/// released. Either is a few file operations; past this it is stuck.
const BOUND: Duration = Duration::from_secs(30);
/// How long a command that reported the wait must still be waiting.
const STILL_WAITING: Duration = Duration::from_millis(500);

/// A scratch sandbox with a seeded store: targets `a` (active) and `b`.
struct Sandbox {
    _dir: tempfile::TempDir,
    root: PathBuf,
}

impl Sandbox {
    fn new() -> Sandbox {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        for sub in ["home", "config", "cache", "bin"] {
            std::fs::create_dir_all(root.join(sub)).unwrap();
        }
        let sandbox = Sandbox { _dir: dir, root };
        let paths = sandbox.store();
        for name in ["a", "b"] {
            save_target(
                &paths,
                &Target {
                    name: name.into(),
                    config: TargetConfig {
                        provider: "hetzner-cloud".into(),
                        region: Some("nbg1".into()),
                        ..Default::default()
                    },
                    credentials: TargetCredentials {
                        hetzner_token: Some(TOKEN_A.into()),
                    },
                },
            )
            .unwrap();
        }
        save_global_config(
            &paths,
            &GlobalConfig {
                active_target: "a".into(),
                version: TARGET_STORE_VERSION,
            },
        )
        .unwrap();
        sandbox
    }

    fn store(&self) -> TargetStorePaths {
        TargetStorePaths::for_root(self.root.join("store"))
    }

    fn apprafter(&self, args: &[&str]) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_apprafter"));
        c.env_clear()
            .env("PATH", self.root.join("bin"))
            .env("HOME", self.root.join("home"))
            .env("XDG_CONFIG_HOME", self.root.join("config"))
            .env("XDG_CACHE_HOME", self.root.join("cache"))
            .env("APPRAFTER_CONFIG_DIR", self.root.join("store"))
            .env("APPRAFTER_SKIP_STARTUP_CHECKS", "1")
            .env("KUBECONFIG", self.root.join("no-kubeconfig"))
            .env("APPRAFTER_HCLOUD_BASE_URL", "http://127.0.0.1:1")
            .current_dir(self.root.join("home"))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .args(args);
        // Windows cannot start a process without its system root.
        if let Some(root) = std::env::var_os("SYSTEMROOT") {
            c.env("SYSTEMROOT", root);
        }
        c
    }

    /// The line a command prints while it waits for the store lock.
    fn waiting_line(&self) -> String {
        format!(
            "waiting for another AppRafter process to release the target store ({})…",
            self.store().lock_file().display()
        )
    }

    fn read(&self, rel: &str) -> String {
        std::fs::read_to_string(self.root.join("store").join(rel)).unwrap()
    }
}

/// A running command whose stderr can be read while it runs.
struct Running {
    child: Child,
    what: String,
    stderr: Arc<Mutex<Vec<u8>>>,
    stderr_reader: thread::JoinHandle<()>,
    stdout_reader: thread::JoinHandle<Vec<u8>>,
}

impl Running {
    fn spawn(sandbox: &Sandbox, args: &[&str]) -> Running {
        let mut child = sandbox.apprafter(args).spawn().expect("spawn apprafter");
        let stderr = Arc::new(Mutex::new(Vec::new()));
        let mut pipe = child.stderr.take().unwrap();
        let sink = Arc::clone(&stderr);
        let stderr_reader = thread::spawn(move || {
            let mut chunk = [0u8; 4096];
            while let Ok(n) = pipe.read(&mut chunk) {
                if n == 0 {
                    break;
                }
                sink.lock().unwrap().extend_from_slice(&chunk[..n]);
            }
        });
        let mut out = child.stdout.take().unwrap();
        let stdout_reader = thread::spawn(move || {
            let mut bytes = Vec::new();
            let _ = out.read_to_end(&mut bytes);
            bytes
        });
        Running {
            child,
            what: format!("`apprafter {}`", args.join(" ")),
            stderr,
            stderr_reader,
            stdout_reader,
        }
    }

    fn stderr(&self) -> String {
        String::from_utf8_lossy(&self.stderr.lock().unwrap()).into_owned()
    }

    fn exited(&mut self) -> Option<std::process::ExitStatus> {
        self.child.try_wait().expect("try_wait")
    }

    /// Kill the command and fail the test with `why`.
    fn fail(mut self, why: &str) -> ! {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let Running {
            what,
            stderr,
            stderr_reader,
            ..
        } = self;
        let _ = stderr_reader.join();
        let stderr = String::from_utf8_lossy(&stderr.lock().unwrap()).into_owned();
        panic!("{what} {why}; stderr:\n{stderr}");
    }

    /// Wait for the command to end, at most [`BOUND`].
    fn finish(mut self) -> Output {
        let deadline = Instant::now() + BOUND;
        loop {
            if let Some(status) = self.exited() {
                let stdout = self.stdout_reader.join().unwrap_or_default();
                let _ = self.stderr_reader.join();
                let stderr = self.stderr.lock().unwrap().clone();
                return Output {
                    status,
                    stdout,
                    stderr,
                };
            }
            if Instant::now() >= deadline {
                self.fail(&format!(
                    "was still running {}s after the lock was released, so it was killed",
                    BOUND.as_secs()
                ));
            }
            thread::sleep(Duration::from_millis(20));
        }
    }
}

/// Run `args` while the test holds the store lock: it must report the wait
/// and keep waiting; released, it must finish successfully. Returns its
/// output.
fn waits_for_the_lock(sandbox: &Sandbox, args: &[&str]) -> Output {
    waits_for_the_lock_while(sandbox, args, || {})
}

/// [`waits_for_the_lock`], running `meanwhile` while the command waits —
/// another process's edit landing between the command's plan and its
/// execution.
fn waits_for_the_lock_while(sandbox: &Sandbox, args: &[&str], meanwhile: impl FnOnce()) -> Output {
    let held = StoreLock::exclusive(&sandbox.store()).unwrap();
    assert!(held.is_held());
    let mut run = Running::spawn(sandbox, args);
    let waiting = sandbox.waiting_line();

    let deadline = Instant::now() + BOUND;
    while !run.stderr().contains(&waiting) {
        if let Some(status) = run.exited() {
            run.fail(&format!(
                "ended ({status}) while the test held the store lock, without waiting for it"
            ));
        }
        if Instant::now() >= deadline {
            run.fail(&format!(
                "did not report waiting for the store lock within {}s",
                BOUND.as_secs()
            ));
        }
        thread::sleep(Duration::from_millis(20));
    }
    thread::sleep(STILL_WAITING);
    if let Some(status) = run.exited() {
        run.fail(&format!(
            "ended ({status}) while the test still held the store lock"
        ));
    }
    meanwhile();

    drop(held);
    let out = run.finish();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "`apprafter {}` failed once released (exit {:?}):\n{stderr}",
        args.join(" "),
        out.status.code()
    );
    assert_eq!(
        stderr.matches(&waiting).count(),
        1,
        "the wait is reported once:\n{stderr}"
    );
    out
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn target_use_waits_for_the_store_lock() {
    let sb = Sandbox::new();
    let out = waits_for_the_lock(&sb, &["target", "use", "b"]);
    assert!(
        stdout(&out).contains("active target switched: `a` → `b`"),
        "{}",
        stdout(&out)
    );
    assert!(sb.read("config.yaml").contains("active_target: b"));
}

#[test]
fn target_rename_waits_for_the_store_lock() {
    let sb = Sandbox::new();
    let out = waits_for_the_lock(&sb, &["target", "rename", "b", "c"]);
    assert!(
        stdout(&out).contains("target renamed: `b` → `c`"),
        "{}",
        stdout(&out)
    );
    let paths = sb.store();
    assert!(paths.target_dir("c").is_dir());
    assert!(!paths.target_dir("b").exists());
}

#[test]
fn target_remove_waits_for_the_store_lock() {
    let sb = Sandbox::new();
    let out = waits_for_the_lock(&sb, &["target", "remove", "b", "--yes"]);
    assert!(
        stdout(&out).contains("target `b` removed"),
        "{}",
        stdout(&out)
    );
    assert!(!sb.store().target_dir("b").exists());
}

/// R6 under a race: `target remove` warns about the server it leaves running
/// even when the server was recorded after its first look at the state —
/// here while it waits for the lock, as an `apply` in another terminal
/// would (`apply` writes the state without the lock). The warning is the
/// one `remove` prints before it asks, and it is printed once.
#[test]
fn target_remove_warns_about_a_server_recorded_while_it_waited() {
    let sb = Sandbox::new();
    let state = cli_state::StatePaths::for_active_target(&sb.store(), "b");
    let out = waits_for_the_lock_while(&sb, &["target", "remove", "b", "--yes"], || {
        std::fs::create_dir_all(state.state_dir()).unwrap();
        std::fs::write(
            state.state_file(),
            r#"{"hetzner_cloud":{"server_id":42,"server_name":"b-node","server_type":"cx22"}}"#,
        )
        .unwrap();
    });
    assert!(
        stdout(&out).contains("target `b` removed"),
        "{}",
        stdout(&out)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    let warning = "warning: target `b` records server `b-node` (id 42); removing the target does \
                   not delete it";
    assert_eq!(stderr.matches(warning).count(), 1, "{stderr}");
    assert!(!sb.store().state_dir("b").exists());
}

#[test]
fn target_machine_waits_for_the_store_lock() {
    let sb = Sandbox::new();
    let out = waits_for_the_lock(
        &sb,
        &["target", "machine", "--server-type", "cx32", "--no-ping"],
    );
    assert!(
        stdout(&out).contains("server type set to `cx32` on target `a`"),
        "{}",
        stdout(&out)
    );
    assert!(sb
        .read("targets/a/config.yaml")
        .contains("server_type: cx32"));
}

#[test]
fn target_add_renew_waits_for_the_store_lock() {
    let sb = Sandbox::new();
    let out = waits_for_the_lock(
        &sb,
        &[
            "target",
            "add",
            "a",
            "--renew",
            "--token",
            TOKEN_B,
            "--no-ping",
            "--no-interactive",
        ],
    );
    assert!(
        stdout(&out).contains("target `a` credentials rotated"),
        "{}",
        stdout(&out)
    );
    assert!(sb.read("targets/a/credentials.yaml").contains(TOKEN_B));
}

/// Restores a directory's mode on drop, so a failed test still lets its
/// `TempDir` clean up.
#[cfg(unix)]
struct ModeGuard(PathBuf);

#[cfg(unix)]
impl Drop for ModeGuard {
    fn drop(&mut self) {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o755));
    }
}

/// `target use` of the target that is already active writes nothing, so a
/// store this process cannot write — no sentinel in it, and none can be
/// created — must not make it fail: it runs without the lock and says so.
///
/// Skipped — loudly — where the directory mode does not bind this process
/// (running as root): the probe write below then succeeds, and the store is
/// not read-only to it.
#[cfg(unix)]
#[test]
fn target_use_on_a_read_only_store_warns_and_succeeds() {
    use std::os::unix::fs::PermissionsExt;
    let sb = Sandbox::new();
    let root = sb.store().root().to_path_buf();
    assert!(!sb.store().lock_file().exists(), "seeding took no lock");
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o555)).unwrap();
    let _restore = ModeGuard(root.clone());
    if std::fs::write(root.join("probe"), b"").is_ok() {
        eprintln!(
            "SKIPPED target_use_on_a_read_only_store_warns_and_succeeds: a 0555 directory is \
             writable to this process (root?)"
        );
        return;
    }

    let out = sb.apprafter(&["target", "use", "a"]).output().unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "exit {:?}:\n{stderr}",
        out.status.code()
    );
    assert!(
        stdout(&out).contains("target `a` was already the active target"),
        "{}",
        stdout(&out)
    );
    let warning = format!(
        "warning: cannot lock the target store ({}): ",
        sb.store().lock_file().display()
    );
    let line = stderr
        .lines()
        .find(|l| l.starts_with(&warning))
        .unwrap_or_else(|| panic!("no `{warning}…` line:\n{stderr}"));
    assert!(line.ends_with("; continuing without the lock"), "{line}");
    assert!(!sb.store().lock_file().exists());
}
