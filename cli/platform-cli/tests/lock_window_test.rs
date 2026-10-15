// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! A person's terminal and its unlock window (D.4a T48–T59, T61, T66; holder §9.2). Each test
//! is a session of its own whose controlling terminal is a pseudo-terminal the test reads and
//! types into (`Spawn::in_session`), running the real `apprafter` as a person would. The
//! Hetzner mock answers only the store's own token: a command that gets past it was served
//! that token — by its own prompt, or by the window. Linux and macOS (`macos-latest`); the
//! Windows console's window is the owner's walk (h).
#![cfg(unix)]

mod common;

use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use apprafter::holder::endpoint::{socket_endpoint, Endpoint, EndpointId};
use apprafter::holder::protocol::StoreBinding;
use apprafter_core::secrets::testing::locked_store;
use common::golden::{json_route, LOCATIONS_OK, PROVISIONED_STATE, SERVER_42_BODY};
use common::spawn::{open_pty, Spawn};

/// 64 ASCII alphanumerics: the store's token, the one the mock accepts.
const TOKEN: &str = "abababababababababababababababababababababababababababababababab";
/// The copy deck's prompt (C27, C28, C24): what the terminal shows when a command asks.
const PROMPT: &str = "Lock password or recovery key";
/// How long a session may take to show what a test waits for.
const LIMIT: Duration = Duration::from_secs(30);

/// A locked store with `prod` (its token in the vault, server 42 recorded) and the CLI's
/// default on it, the Hetzner API at `api`. Returns the recovery key's line a person types and
/// the store's binding (what its holders are keyed by).
fn store(api: &str) -> (Spawn, String, StoreBinding) {
    let sp = Spawn::new().with_hcloud(api.to_string());
    let root = sp.store();
    let locked = locked_store(&root, &[("prod", Some(TOKEN))]);
    let paths = cli_core::TargetStorePaths::for_root(root.clone());
    cli_core::save_global_config(
        &paths,
        &cli_core::GlobalConfig {
            active_target: "prod".into(),
            version: cli_core::TARGET_STORE_VERSION,
        },
    )
    .unwrap();
    let state = cli_state::StatePaths::for_active_target(&paths, "prod");
    std::fs::create_dir_all(state.state_dir()).unwrap();
    std::fs::write(state.state_file(), PROVISIONED_STATE).unwrap();
    let key = locked
        .recovery_key()
        .expose()
        .lines()
        .nth(1)
        .expect("the recovery key's second line")
        .to_string();
    let binding = StoreBinding {
        id: locked.store_id.as_str().into(),
        root: std::fs::canonicalize(&root).unwrap(),
    };
    (sp, key, binding)
}

/// A session the test drives as a person at a terminal: `/bin/sh -c <script>` leads it, with a
/// pseudo-terminal as its controlling terminal, stdin and stderr; the script's stdout is
/// read line by line (`step: <name> <exit>` marks each command's end).
struct Terminal {
    child: Child,
    keys: std::fs::File,
    screen: Arc<Mutex<String>>,
    /// Reads the terminal into `screen` until it closes.
    reader: std::thread::JoinHandle<()>,
    out: mpsc::Receiver<String>,
    stdout: Vec<String>,
}

impl Terminal {
    fn start(sp: &Spawn, script: &str) -> Terminal {
        Terminal::start_reading_late(sp, script, Duration::ZERO)
    }

    /// [`Terminal::start`] with the terminal's reader starting `late`: the harness's own test
    /// puts it behind a session that has already ended.
    fn start_reading_late(sp: &Spawn, script: &str, late: Duration) -> Terminal {
        let (master, slave) = open_pty();
        let mut c = sp.in_session(script, Some(slave));
        c.stdout(Stdio::piped());
        let mut child = c.spawn().expect("start the session");
        drop(c);
        let keys = std::fs::File::from(master);
        let mut from = keys.try_clone().unwrap();
        let screen = Arc::new(Mutex::new(String::new()));
        let shown = screen.clone();
        let reader = std::thread::spawn(move || {
            std::thread::sleep(late);
            let mut buf = [0u8; 4096];
            // EIO once no process has the terminal open any more: the session is over.
            while let Ok(n @ 1..) = from.read(&mut buf) {
                shown
                    .lock()
                    .unwrap()
                    .push_str(&String::from_utf8_lossy(&buf[..n]));
            }
        });
        let lines = BufReader::new(child.stdout.take().unwrap());
        let (tx, out) = mpsc::channel();
        std::thread::spawn(move || {
            for line in lines.lines().map_while(Result::ok) {
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        Terminal {
            child,
            keys,
            screen,
            reader,
            out,
            stdout: Vec::new(),
        }
    }

    fn screen(&self) -> String {
        self.screen.lock().unwrap().clone()
    }

    /// Wait until the terminal has shown `text` at least `n` times.
    fn shows(&self, text: &str, n: usize) {
        let until = Instant::now() + LIMIT;
        while Instant::now() < until {
            if self.screen().matches(text).count() >= n {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!(
            "the terminal did not show {text:?} ×{n} within 30 s:\n{}",
            self.screen()
        );
    }

    /// Type a line, as a person does.
    fn type_line(&mut self, line: &str) {
        self.keys.write_all(format!("{line}\n").as_bytes()).unwrap();
    }

    /// The next `step:` line of the script — `<name> <exit code>` — keeping every line before it.
    fn step(&mut self) -> String {
        loop {
            let line = self.out.recv_timeout(LIMIT).unwrap_or_else(|_| {
                panic!(
                    "no step within 30 s; the terminal shows:\n{}",
                    self.screen()
                )
            });
            self.stdout.push(line.clone());
            if let Some(step) = line.strip_prefix("step: ") {
                return step.to_string();
            }
        }
    }

    fn stdout(&self) -> String {
        self.stdout.join("\n")
    }

    /// Wait for the shell's end, then for the reader to read the terminal until it closes (no
    /// process has it open any more): what the session wrote last can still be unread when the
    /// shell is reaped (GOTCHA-259). What the terminal showed.
    fn finish(self) -> String {
        let Terminal {
            mut child,
            screen,
            reader,
            ..
        } = self;
        child.wait().unwrap();
        let until = Instant::now() + LIMIT;
        while !reader.is_finished() {
            assert!(
                Instant::now() < until,
                "the terminal did not close within 30 s of the session's end:\n{}",
                screen.lock().unwrap()
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        reader.join().expect("the terminal's reader");
        Arc::try_unwrap(screen)
            .expect("the reader has let go of the screen")
            .into_inner()
            .unwrap()
    }
}

/// The lock file the holder of `b` for the session `sid` keeps: held while that holder lives.
fn holder_lock(sp: &Spawn, b: &StoreBinding, sid: i32) -> PathBuf {
    // SAFETY: geteuid has no failure mode.
    let uid = unsafe { libc::geteuid() };
    match socket_endpoint(
        &sp.path("run").join("apprafter"),
        &EndpointId::for_session(uid, b, sid),
    ) {
        Ok(Endpoint::Socket { lock, .. }) => lock,
        other => panic!("{other:?}"),
    }
}

/// A holder holds its lock file for its whole life: free means gone.
fn holder_alive(lock: &Path) -> bool {
    use std::os::fd::AsRawFd;
    let Ok(f) = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(lock)
    else {
        return false;
    };
    // SAFETY: flock on a file this function opened; released when it closes.
    unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) != 0 }
}

/// The harness itself (GOTCHA-259): the screen `finish` returns holds what the session wrote
/// last, even when the reader is behind. Forced: the reader starts 2 s after a session that
/// writes one line to its terminal and ends at once, so the shell is reaped before anything
/// was read.
#[test]
fn finish_returns_what_the_session_wrote_last_when_the_reader_is_behind() {
    let sp = Spawn::new();
    let mut t = Terminal::start_reading_late(
        &sp,
        r#"echo "the session's last line" >&2; echo "step: wrote $?""#,
        Duration::from_secs(2),
    );
    assert_eq!(t.step(), "wrote 0");
    let screen = t.finish();
    assert!(screen.contains("the session's last line"), "{screen:?}");
}

/// T56, T48, T66, O6: `unlock` asks once; then every command of this terminal — one with no
/// terminal of its own among them, as an agent's tool would run — is served by the window
/// without asking: the token (`target ip`), whoami's ping, `lock status`'s window line. `lock
/// now` ends it: the next command without a terminal is `store_locked`, and `whoami` at the
/// terminal still asks nothing (T66).
#[test]
fn unlock_opens_a_window_the_next_commands_use() {
    let mut api = mockito::Server::new();
    let ip = json_route(&mut api, "/v1/servers/42", 200, SERVER_42_BODY, TOKEN)
        .expect(1)
        .create();
    let ping = json_route(&mut api, "/v1/locations", 200, LOCATIONS_OK, TOKEN)
        .expect(1)
        .create();
    let (sp, key, _) = store(&api.url());
    let mut t = Terminal::start(
        &sp,
        r#""$1" unlock; echo "step: unlock $?"
"$1" target ip </dev/null; echo "step: ip $?"
"$1" whoami </dev/null; echo "step: whoami $?"
"$1" lock status </dev/null; echo "step: status $?"
"$1" lock now; echo "step: now $?"
"$1" whoami; echo "step: whoami-locked $?"
"$1" target ip </dev/null; echo "step: after $?""#,
    );
    t.shows(PROMPT, 1);
    t.type_line(&key);
    assert_eq!(t.step(), "unlock 0", "{}", t.screen());
    // The terminal's bytes arrive on their own reader: waited for, never read once.
    t.shows("Unlocked in this terminal for 5 min", 1);
    assert_eq!(t.step(), "ip 0", "{}", t.screen());
    assert!(t.stdout().contains("203.0.113.10"), "{}", t.stdout());
    assert_eq!(t.step(), "whoami 0", "{}", t.screen());
    assert!(t.stdout().contains("verified ✓"), "{}", t.stdout());
    assert_eq!(t.step(), "status 0", "{}", t.screen());
    assert!(
        t.stdout()
            .contains("This terminal:  unlocked, 5 min left (a window lasts 5 min"),
        "{}",
        t.stdout()
    );
    assert_eq!(t.step(), "now 0", "{}", t.screen());
    t.shows("The unlock window in this terminal is closed.", 1);
    assert_eq!(t.step(), "whoami-locked 0", "{}", t.screen());
    assert!(
        t.stdout()
            .contains("verification skipped — the target store is locked"),
        "{}",
        t.stdout()
    );
    assert_eq!(t.step(), "after 1", "{}", t.screen());
    let screen = t.finish();
    assert!(
        screen.contains("apprafter::secrets::store_locked"),
        "{screen}"
    );
    assert_eq!(
        screen.matches(PROMPT).count(),
        1,
        "asked more than once:\n{screen}"
    );
    ip.assert();
    ping.assert();
}

/// T56, C28, C30, O6: a command's own prompt says the window its answer opens, starts it, and
/// says what it opened and who can use it — the next command, with no terminal of its own, is
/// served by it (critic-d4a-e finding 7).
#[test]
fn the_inline_prompt_starts_a_window() {
    let mut api = mockito::Server::new();
    let ip = json_route(&mut api, "/v1/servers/42", 200, SERVER_42_BODY, TOKEN)
        .expect(2)
        .create();
    let (sp, key, _) = store(&api.url());
    let mut t = Terminal::start(
        &sp,
        r#""$1" target ip; echo "step: first $?"
"$1" target ip </dev/null; echo "step: second $?"
"$1" lock now; echo "step: now $?""#,
    );
    t.shows(
        "Lock password or recovery key (unlocks this terminal for 5 min): ",
        1,
    );
    t.type_line(&key);
    assert_eq!(t.step(), "first 0", "{}", t.screen());
    t.shows(
        "Unlocked in this terminal for 5 min, restarted by each command that uses it. \
         Everything started from this terminal can use it, an AI agent included",
        1,
    );
    assert_eq!(t.step(), "second 0", "{}", t.screen());
    assert_eq!(t.step(), "now 0", "{}", t.screen());
    let screen = t.finish();
    assert!(
        screen.contains("The unlock window in this terminal is closed."),
        "{screen}"
    );
    assert!(
        !screen.contains("asked again by the next command"),
        "{screen}"
    );
    assert_eq!(
        screen.matches(PROMPT).count(),
        1,
        "asked more than once:\n{screen}"
    );
    ip.assert();
}

/// d4a-h F3 (matrix C-10): a change to the store inside this terminal's open window —
/// `lock on --timeout 15` — asks for the lock password anyway, with a question that promises no
/// window, and once answered says nothing of the window: its holder still serves, so it is
/// never called one that "does not answer". The window goes on as it was: `lock status` reports
/// it, and the next command with no terminal of its own is served by it.
#[test]
fn a_change_inside_the_window_asks_and_leaves_the_window_alone() {
    let mut api = mockito::Server::new();
    let ip = json_route(&mut api, "/v1/servers/42", 200, SERVER_42_BODY, TOKEN)
        .expect(1)
        .create();
    let (sp, key, _) = store(&api.url());
    let mut t = Terminal::start(
        &sp,
        r#""$1" unlock; echo "step: unlock $?"
"$1" lock on --timeout 15; echo "step: on $?"
"$1" lock status </dev/null; echo "step: status $?"
"$1" target ip </dev/null; echo "step: ip $?"
"$1" lock now; echo "step: now $?""#,
    );
    t.shows(PROMPT, 1);
    t.type_line(&key);
    assert_eq!(t.step(), "unlock 0", "{}", t.screen());
    t.shows(PROMPT, 2);
    t.type_line(&key);
    assert_eq!(t.step(), "on 0", "{}", t.screen());
    assert!(t.stdout().contains("now last 15 min"), "{}", t.stdout());
    assert_eq!(t.step(), "status 0", "{}", t.screen());
    assert!(
        t.stdout().contains("This terminal:  unlocked, "),
        "{}",
        t.stdout()
    );
    assert_eq!(t.step(), "ip 0", "{}", t.screen());
    assert!(t.stdout().contains("203.0.113.10"), "{}", t.stdout());
    assert_eq!(t.step(), "now 0", "{}", t.screen());
    let screen = t.finish();
    assert!(!screen.contains("does not answer"), "{screen}");
    assert!(!screen.contains("(unlocks this terminal for"), "{screen}");
    assert!(!screen.contains("No unlock window"), "{screen}");
    assert!(
        screen.contains(
            "Lock password or recovery key (a change to the store asks even in an unlocked \
             terminal): "
        ),
        "{screen}"
    );
    assert_eq!(
        screen.matches(PROMPT).count(),
        2,
        "asked other than once by unlock and once by the change:\n{screen}"
    );
    ip.assert();
}

/// T51, T69: a process of another session — an agent's tool that is not this terminal's — gets
/// `store_locked` and nothing from the window, which goes on serving this terminal. T35: inside
/// the window, `HCLOUD_TOKEN` on the locked store is still refused before anything is read.
#[test]
fn another_session_is_store_locked_while_the_window_stays() {
    let mut api = mockito::Server::new();
    let ip = json_route(&mut api, "/v1/servers/42", 200, SERVER_42_BODY, TOKEN)
        .expect(1)
        .create();
    let (sp, key, _) = store(&api.url());
    let mut t = Terminal::start(
        &sp,
        r#""$1" unlock; echo "step: unlock $?"
HCLOUD_TOKEN=dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd "$1" target ip </dev/null; echo "step: override $?"
read _
"$1" target ip </dev/null; echo "step: mine $?"
"$1" lock now; echo "step: now $?""#,
    );
    t.shows(PROMPT, 1);
    t.type_line(&key);
    assert_eq!(t.step(), "unlock 0", "{}", t.screen());
    // T35: an override on a locked store is refused inside the window too.
    assert_eq!(t.step(), "override 1", "{}", t.screen());
    t.shows("apprafter::secrets::override_refused", 1);
    let other = sp.cmd(&["target", "ip"]).output().unwrap();
    assert_eq!(other.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&other.stderr).contains("apprafter::secrets::store_locked"));
    t.type_line("");
    assert_eq!(t.step(), "mine 0", "{}", t.screen());
    assert_eq!(t.step(), "now 0", "{}", t.screen());
    t.finish();
    ip.assert();
}

/// T52 (critic G6): a CLI that leads its own session (`exec apprafter unlock`) says so, and its
/// window ends with it.
#[test]
fn a_cli_that_leads_its_session_takes_its_window_with_it() {
    let (sp, key, binding) = store("http://127.0.0.1:1");
    let mut t = Terminal::start(&sp, r#"exec "$1" unlock"#);
    let lock = holder_lock(&sp, &binding, t.child.id() as i32);
    t.shows(PROMPT, 1);
    t.type_line(&key);
    t.shows("Unlocked in this terminal for 5 min", 1);
    let screen = t.finish();
    assert!(
        screen.contains("This command leads its own session: the window ends with it."),
        "{screen}"
    );
    let until = Instant::now() + Duration::from_secs(2);
    while holder_alive(&lock) && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(25));
    }
    assert!(
        !holder_alive(&lock),
        "the holder outlived the session its CLI led"
    );
}

/// T56 (critic-d4a-e finding 14): `lock off` ends this terminal's window before the vault goes:
/// after it no holder of this terminal is left to serve a copy of the vault until its clock
/// runs out, and `lock now` would no longer find it (the store has no id any more).
#[test]
fn lock_off_ends_this_terminals_window() {
    let (sp, key, binding) = store("http://127.0.0.1:1");
    let mut t = Terminal::start(
        &sp,
        r#""$1" unlock; echo "step: unlock $?"
"$1" lock off; echo "step: off $?""#,
    );
    let lock = holder_lock(&sp, &binding, t.child.id() as i32);
    t.shows(PROMPT, 1);
    t.type_line(&key);
    assert_eq!(t.step(), "unlock 0", "{}", t.screen());
    assert!(holder_alive(&lock), "unlock opened no window");
    t.shows(PROMPT, 2);
    t.type_line(&key);
    assert_eq!(t.step(), "off 0", "{}", t.screen());
    t.shows("The unlock window in this terminal is closed.", 1);
    let until = Instant::now() + Duration::from_secs(2);
    while holder_alive(&lock) && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(25));
    }
    assert!(
        !holder_alive(&lock),
        "a holder of this terminal outlived lock off"
    );
    t.finish();
}

/// The pid of `parent`'s child named `apprafter`, once it runs (Linux `/proc`).
#[cfg(target_os = "linux")]
fn cli_child(parent: u32) -> u32 {
    let until = Instant::now() + LIMIT;
    while Instant::now() < until {
        for entry in std::fs::read_dir("/proc").unwrap().flatten() {
            let Ok(pid) = entry.file_name().to_string_lossy().parse::<u32>() else {
                continue;
            };
            let Ok(stat) = std::fs::read_to_string(entry.path().join("stat")) else {
                continue;
            };
            // "<pid> (<comm>) <state> <ppid> …": a comm may hold spaces, so split at its ')'.
            let Some((head, rest)) = stat.rsplit_once(')') else {
                continue;
            };
            let ppid = rest
                .split_whitespace()
                .nth(1)
                .and_then(|p| p.parse::<u32>().ok());
            if ppid == Some(parent) && head.ends_with("(apprafter") {
                return pid;
            }
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("no apprafter under pid {parent} within 30 s");
}

/// T57 (Linux): while a command's own prompt, `unlock` and `lock off` wait for a secret, the
/// CLI is not dumpable — its `/proc/<pid>/mem` belongs to root, as every file under
/// `/proc/<pid>` of a non-dumpable process does (the directory itself keeps the process's uid:
/// the kernel's `task_dump_owner`) — while its shell's is still this user's (the control); the
/// inline prompt is the one every ordinary command shows (critic-d4a-e finding 6). The forced
/// condition: the test does not run as root.
#[cfg(target_os = "linux")]
#[test]
fn the_cli_is_not_dumpable_while_its_prompt_waits() {
    use std::os::unix::fs::MetadataExt;

    // SAFETY: geteuid has no failure mode.
    assert_ne!(
        unsafe { libc::geteuid() },
        0,
        "as root, /proc/<pid>/mem is root's anyway"
    );
    let (sp, key, _) = store("http://127.0.0.1:1");
    let mut t = Terminal::start(
        &sp,
        r#""$1" target ip; echo "step: ip $?"
"$1" lock now; echo "step: closed $?"
"$1" unlock; echo "step: unlock $?"
"$1" lock off; echo "step: off $?"
"$1" lock now; echo "step: now $?""#,
    );
    let owner = |pid: u32| std::fs::metadata(format!("/proc/{pid}/mem")).unwrap().uid();
    assert_ne!(owner(t.child.id()), 0, "the shell is this user's");
    t.shows(PROMPT, 1);
    assert_eq!(
        owner(cli_child(t.child.id())),
        0,
        "a command is dumpable at its own prompt"
    );
    t.type_line(&key);
    t.step();
    assert_eq!(t.step(), "closed 0", "{}", t.screen());
    t.shows(PROMPT, 2);
    assert_eq!(
        owner(cli_child(t.child.id())),
        0,
        "unlock is dumpable at its prompt"
    );
    t.type_line(&key);
    assert_eq!(t.step(), "unlock 0", "{}", t.screen());
    t.shows(PROMPT, 3);
    let off = cli_child(t.child.id());
    assert_eq!(owner(off), 0, "lock off is dumpable at its prompt");
    // SAFETY: SIGTERM to the `lock off` this test started; it restores the terminal and ends.
    unsafe { libc::kill(off as i32, libc::SIGTERM) };
    assert_ne!(t.step(), "off 0", "{}", t.screen());
    assert_eq!(t.step(), "now 0", "{}", t.screen());
    t.finish();
}

/// D.4a T66, T56 (d4a-f): `doctor` in an unlocked terminal reads the token through the window
/// for its ping — asking nothing, with no terminal of its own — and its `secrets` row names
/// the window; `doctor --no-ping` reads no token through it (every served item restarts the
/// window), only says where the token is; after `lock now` the same doctor skips the token,
/// saying the store is locked, and pings nothing. No run shows the token, and nothing typed is
/// shown.
#[test]
fn doctor_reads_through_the_window_and_asks_nothing() {
    let mut api = mockito::Server::new();
    let ping = json_route(&mut api, "/v1/locations", 200, LOCATIONS_OK, TOKEN)
        .expect(1)
        .create();
    let (sp, key, _) = store(&api.url());
    let mut t = Terminal::start(
        &sp,
        r#""$1" unlock; echo "step: unlock $?"
"$1" doctor </dev/null; echo "step: doctor $?"
"$1" doctor --no-ping </dev/null; echo "step: doctor-no-ping $?"
"$1" lock now; echo "step: now $?"
"$1" doctor </dev/null; echo "step: doctor-locked $?""#,
    );
    t.shows(PROMPT, 1);
    t.type_line(&key);
    assert_eq!(t.step(), "unlock 0", "{}", t.screen());
    assert!(t.step().starts_with("doctor "), "{}", t.screen());
    assert!(t.step().starts_with("doctor-no-ping "), "{}", t.screen());
    assert!(t.step().starts_with("now 0"), "{}", t.screen());
    assert!(t.step().starts_with("doctor-locked "), "{}", t.screen());
    let out = t.stdout();
    assert_eq!(
        out.matches("Token format valid (64 chars, alphanumeric)")
            .count(),
        1,
        "{out}"
    );
    assert!(
        out.contains("Secrets locked (1 slot: recovery key) (unlocked in this terminal, "),
        "{out}"
    );
    assert_eq!(
        out.matches("Token format valid (in the vault; read only with the ping)")
            .count(),
        1,
        "{out}"
    );
    assert_eq!(
        out.matches("Token format valid (the target store is locked)")
            .count(),
        1,
        "{out}"
    );
    assert!(!out.contains(TOKEN), "the token reached stdout:\n{out}");
    let screen = t.finish();
    assert_eq!(screen.matches(PROMPT).count(), 1, "doctor asked:\n{screen}");
    assert!(
        !screen.contains(TOKEN),
        "the token reached the terminal:\n{screen}"
    );
    let typed = key.split_once(' ').map_or(key.as_str(), |(_, rest)| rest);
    assert!(
        !screen.contains(typed),
        "the typed key was shown:\n{screen}"
    );
    ping.assert();
}
