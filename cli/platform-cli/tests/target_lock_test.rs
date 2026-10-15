// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Every CLI edit of the target store waits for the store lock, and says
//! so.
//!
//! The test process holds `cli_core::StoreLock` on a seeded store and runs
//! one edit — `target use`, `rename`, `remove`, `machine`, `add --renew` —
//! as a real process, through the spawn point (`tests/common/spawn.rs`).
//! The edit must report the wait on stderr and still be running half a
//! second later; released, it must finish, successfully, within 30 s. Each
//! check fails if the command does not take the lock, and none depends on
//! timing when it does: the wait is observed by its stderr line, not by a
//! sleep long enough for an unlocked run to finish.
//!
//! The last test is the other side: a store that cannot be locked at all
//! (read-only) is used without the lock, with a warning, instead of making
//! a command that writes nothing fail.

mod common;

use std::io::Read;
use std::process::{Child, Command, Output, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use cli_core::target::{
    save_global_config, save_target, GlobalConfig, StoreLock, Target, TargetConfig,
    TargetCredentials, TargetStorePaths, TARGET_STORE_VERSION,
};
use common::spawn::Spawn;

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
    spawn: Spawn,
}

impl Sandbox {
    fn new() -> Sandbox {
        Sandbox::on(Spawn::new())
    }

    /// The seeded store in `spawn`'s sandbox: a mock API or a working directory of the test's.
    fn on(spawn: Spawn) -> Sandbox {
        let sandbox = Sandbox { spawn };
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
        TargetStorePaths::for_root(self.spawn.store())
    }

    fn apprafter(&self, args: &[&str]) -> Command {
        let mut c = self.spawn.std(args);
        c.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        c
    }

    /// The line a command prints while it waits for the store lock.
    fn waiting_line(&self) -> String {
        format!(
            "waiting for another AppRafter process to finish writing the target store ({})…",
            self.store().lock_file().display()
        )
    }

    fn read(&self, rel: &str) -> String {
        std::fs::read_to_string(self.spawn.store().join(rel)).unwrap()
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
    fn spawn(sandbox: &Sandbox, args: &[&str], env: &[(&str, &str)]) -> Running {
        let mut command = sandbox.apprafter(args);
        command.envs(env.iter().copied());
        let mut child = command.spawn().expect("spawn apprafter");
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

/// [`waits_for_the_lock`], the command given `env` besides the sandbox's.
fn waits_for_the_lock_env(sandbox: &Sandbox, args: &[&str], env: &[(&str, &str)]) -> Output {
    waits_for_the_lock_with(sandbox, args, env, || {})
}

/// [`waits_for_the_lock`], running `meanwhile` while the command waits —
/// another process's edit landing between the command's plan and its
/// execution.
fn waits_for_the_lock_while(sandbox: &Sandbox, args: &[&str], meanwhile: impl FnOnce()) -> Output {
    waits_for_the_lock_with(sandbox, args, &[], meanwhile)
}

fn waits_for_the_lock_with(
    sandbox: &Sandbox,
    args: &[&str],
    env: &[(&str, &str)],
    meanwhile: impl FnOnce(),
) -> Output {
    let held = StoreLock::exclusive(&sandbox.store()).unwrap();
    assert!(held.is_held());
    let mut run = Running::spawn(sandbox, args, env);
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
/// here while it waits for the lock, as an `apply` in another terminal that
/// took the lock first would. The warning is the one `remove` prints before
/// it asks, and it is printed once.
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

// ── WI-487: every writer of a target's state.json waits for the store lock ──

/// The five listings `destroy` and `import` read, on a mock of the Hetzner API: `servers` as
/// given, every other kind empty.
fn hetzner_with(servers: &str) -> mockito::ServerGuard {
    let mut api = mockito::Server::new();
    for (path, body) in [
        ("/v1/servers", format!(r#"{{"servers":[{servers}]}}"#)),
        ("/v1/ssh_keys", r#"{"ssh_keys":[]}"#.to_string()),
        ("/v1/networks", r#"{"networks":[]}"#.to_string()),
        ("/v1/firewalls", r#"{"firewalls":[]}"#.to_string()),
        ("/v1/floating_ips", r#"{"floating_ips":[]}"#.to_string()),
    ] {
        api.mock("GET", path)
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(body)
            .create();
    }
    api
}

/// `a`'s state.json, as the store holds it.
const A_STATE: &str = "state/a/.apprafter/state.json";

fn write_a_state(sb: &Sandbox, body: &str) {
    let state = cli_state::StatePaths::for_active_target(&sb.store(), "a");
    std::fs::create_dir_all(state.state_dir()).unwrap();
    std::fs::write(state.state_file(), body).unwrap();
}

#[test]
fn init_waits_for_the_store_lock() {
    let sb = Sandbox::new();
    let out = waits_for_the_lock(
        &sb,
        &[
            "init",
            "--provider",
            "hetzner-cloud",
            "--tier",
            "team",
            "--region",
            "fsn1",
        ],
    );
    assert!(stdout(&out).contains("would init"), "{}", stdout(&out));
    let state = sb.read(A_STATE);
    assert!(
        state.contains("\"fsn1\"") && state.contains("\"team\""),
        "{state}"
    );
}

#[test]
fn destroy_waits_for_the_store_lock() {
    let api = hetzner_with("");
    let sb = Sandbox::on(Spawn::new().with_hcloud(api.url()));
    write_a_state(
        &sb,
        r#"{"provider":"hetzner-cloud","cluster_name":"platform-1","region":"nbg1",
            "hetzner_cloud":{"server_id":42,"server_name":"platform-1"}}"#,
    );
    let out = waits_for_the_lock_env(&sb, &["destroy", "--yes"], &[("HCLOUD_TOKEN", TOKEN_A)]);
    assert!(
        stdout(&out).contains("destroy complete: 0 resource(s) removed"),
        "{}",
        stdout(&out)
    );
    let state = sb.read(A_STATE);
    assert!(state.contains("\"hetzner_cloud\": null"), "{state}");
    assert!(state.contains("\"nbg1\""), "{state}");
}

#[test]
fn import_waits_for_the_store_lock() {
    let api = hetzner_with(
        r#"{"id":42,"name":"platform-1","status":"running","labels":{"apprafter":"true"}}"#,
    );
    let sb = Sandbox::on(Spawn::new().with_hcloud(api.url()));
    let out = waits_for_the_lock_env(&sb, &["import"], &[("HCLOUD_TOKEN", TOKEN_A)]);
    assert!(
        stdout(&out).contains("import: wrote state for cluster `platform-1`"),
        "{}",
        stdout(&out)
    );
    assert!(sb.read(A_STATE).contains("\"server_id\": 42"));
}

/// The one-shot move of a legacy `<cwd>/.apprafter/state.json` writes the target's state, so
/// it waits for the store lock too — here under `plan`, a command that writes nothing else.
#[test]
fn a_legacy_state_moves_in_under_the_store_lock() {
    let project = tempfile::tempdir().unwrap();
    let legacy = project.path().join(".apprafter").join("state.json");
    std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
    std::fs::write(&legacy, r#"{"cluster_name":"legacy"}"#).unwrap();
    let sb = Sandbox::on(Spawn::new().current_dir(project.path()));
    waits_for_the_lock(&sb, &["plan"]);
    assert!(!legacy.exists(), "the legacy state stayed");
    assert!(sb.read(A_STATE).contains("\"legacy\""));
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
    let sb = Sandbox::new();
    let root = sb.store().root().to_path_buf();
    assert!(!sb.store().lock_file().exists(), "seeding took no lock");
    let _restore = cli_core::owner_only::testing::ModeGuard::set(&root, 0o555);
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
        "warning: cannot take the target store's write lock ({}): ",
        sb.store().lock_file().display()
    );
    let line = stderr
        .lines()
        .find(|l| l.starts_with(&warning))
        .unwrap_or_else(|| panic!("no `{warning}…` line:\n{stderr}"));
    assert!(line.ends_with("; continuing without it"), "{line}");
    assert!(!sb.store().lock_file().exists());
}
