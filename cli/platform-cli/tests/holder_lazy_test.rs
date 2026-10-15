// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! T-lazy-2 (D.4a T7, G5; safety-net §2.5): the commands that need no secret never contact a
//! holder and never read the vault. In a session of its own, a tripwire listens at the very
//! endpoint this terminal's holder of the store would have, and counts every connection. The
//! commands run in two stores that differ only in their `vault` — a directory in the second,
//! so any read of it fails — and each must exit and print the same in both, with no
//! connection. The controls: `lock status` asks for the window (one connection; it reads the
//! vault's header, so it runs in the first store only), and `target ip` asks in the first store
//! and, in the second, fails reading the vault before it asks anyone.
#![cfg(unix)]

mod common;

use std::ffi::OsString;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::DirBuilderExt;
use std::os::unix::net::UnixListener;
use std::process::{Child, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use apprafter::holder::endpoint::{socket_endpoint, Endpoint, EndpointId};
use apprafter::holder::protocol::StoreBinding;
use apprafter_core::secrets::testing::locked_store;
use common::golden::PROVISIONED_STATE;
use common::mask::{mask_millis, mask_timestamps};
use common::spawn::Spawn;

const TOKEN: &str = "d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1";

/// The commands of rule 2 (T7), in order; each sees what the ones before it left.
const NO_SECRET: &[&[&str]] = &[
    &["--help"],
    &["--version"],
    &["completion", "bash"],
    &["target", "list"],
    &["target", "show", "prod"],
    &["target", "use", "prod"],
    &["whoami", "--no-ping"],
    &["plan"],
    &[
        "app",
        "scaffold",
        "--runtime",
        "blank",
        "--name",
        "lazy",
        "--path",
        "scaffold",
    ],
    &["app", "validate", "scaffold/apprafter"],
];

/// A `cue` for `app validate`, as `laziness_test.rs` finds it.
fn cue() -> OsString {
    std::env::var_os("CUE_BIN")
        .filter(|v| !v.is_empty())
        .or_else(|| {
            let path = std::env::var_os("PATH").unwrap_or_default();
            cli_core::tools::find_on_path("cue", &path, cli_core::tools::is_executable_file)
                .map(|p| p.into_os_string())
        })
        .expect("`app validate` needs cue: set CUE_BIN or put cue on PATH, as the CLI build does")
}

/// A locked store with `prod` (its token in the vault, a server recorded) and the CLI's default
/// on it; in the trapped one, `vault` is a directory.
fn store(trapped: bool) -> (Spawn, StoreBinding) {
    let sp = Spawn::new().with_env("CUE_BIN", cue());
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
    if trapped {
        std::fs::remove_file(root.join("vault")).unwrap();
        std::fs::create_dir(root.join("vault")).unwrap();
    }
    let binding = StoreBinding {
        id: locked.store_id.as_str().into(),
        root: std::fs::canonicalize(&root).unwrap(),
    };
    (sp, binding)
}

/// One session: `/bin/sh` leads it; it waits for a line on stdin before each phase.
struct Session {
    child: Child,
    out: mpsc::Receiver<String>,
}

/// The script: wait; the rule-2 commands, each one's exit and output kept in `<dir>/<n>.*`;
/// `step: quiet`; wait; then each control, with its step. Builtins and `$1` only: the
/// session's `PATH` is the sandbox's empty `bin`.
fn script(dir: &std::path::Path, controls: &[&[&str]]) -> String {
    let mut s = format!("D='{}'; read _\n", dir.display());
    for (n, args) in NO_SECRET.iter().enumerate() {
        s.push_str(&format!(
            "\"$1\" {} >\"$D/{n}.out\" 2>\"$D/{n}.err\"; echo $? >\"$D/{n}.code\"\n",
            args.join(" ")
        ));
    }
    s.push_str("echo \"step: quiet\"; read _\n");
    for (n, args) in controls.iter().enumerate() {
        s.push_str(&format!(
            "\"$1\" {} >\"$D/c{n}.out\" 2>\"$D/c{n}.err\"; echo \"step: c{n}\"; read _\n",
            args.join(" ")
        ));
    }
    s
}

fn start(sp: &Spawn, script: &str) -> Session {
    let mut c = sp.in_session(script, None);
    c.stdin(Stdio::piped()).stdout(Stdio::piped());
    let mut child = c.spawn().expect("start the session");
    let lines = BufReader::new(child.stdout.take().unwrap());
    let (tx, out) = mpsc::channel();
    std::thread::spawn(move || {
        for line in lines.lines().map_while(Result::ok) {
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    Session { child, out }
}

impl Session {
    fn go(&mut self) {
        self.child.stdin.as_mut().unwrap().write_all(b"\n").unwrap();
    }

    fn step(&self) -> String {
        self.out
            .recv_timeout(Duration::from_secs(120))
            .expect("the session's next step within 120 s")
    }

    fn finish(mut self) {
        drop(self.child.stdin.take());
        self.child.wait().unwrap();
    }
}

/// A listener at the endpoint the holder of `b` for the session `sid` would have. It never
/// answers: a connection waits in its backlog (a client's `connect` returns once it is queued)
/// until [`Tripwire::hits`] drains and counts it — so a count read after a command ended sees
/// every connection that command made, with no race against an accepting thread.
struct Tripwire {
    listener: UnixListener,
    count: usize,
}

impl Tripwire {
    fn at(sp: &Spawn, b: &StoreBinding, sid: i32) -> Tripwire {
        let base = sp.path("run").join("apprafter");
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&base)
            .unwrap();
        // SAFETY: geteuid has no failure mode.
        let uid = unsafe { libc::geteuid() };
        let path = match socket_endpoint(&base, &EndpointId::for_session(uid, b, sid)) {
            Ok(Endpoint::Socket { path, .. }) => path,
            other => panic!("{other:?}"),
        };
        let listener = UnixListener::bind(&path).unwrap();
        listener.set_nonblocking(true).unwrap();
        Tripwire { listener, count: 0 }
    }

    /// Every connection made so far.
    fn hits(&mut self) -> usize {
        loop {
            match self.listener.accept() {
                Ok(_) => self.count += 1,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => return self.count,
                Err(e) => panic!("the tripwire: {e}"),
            }
        }
    }
}

/// Each rule-2 step's exit and output in `sp`, the sandbox's root masked, then tracing's
/// timestamps and timings, as `laziness_test.rs` normalises them.
fn results(sp: &Spawn) -> Vec<(String, String, String)> {
    let read = |f: String| {
        let text = std::fs::read_to_string(sp.path(&format!("lazy/{f}"))).unwrap();
        mask_millis(&mask_timestamps(&sp.mask_root(&text)))
    };
    (0..NO_SECRET.len())
        .map(|n| {
            (
                read(format!("{n}.code")),
                read(format!("{n}.out")),
                read(format!("{n}.err")),
            )
        })
        .collect()
}

#[test]
fn commands_that_need_no_secret_never_contact_a_holder_nor_read_the_vault() {
    let (plain_sp, plain_b) = store(false);
    let (trap_sp, trap_b) = store(true);
    assert!(trap_sp.store().join("vault").is_dir(), "the trap is set");
    for sp in [&plain_sp, &trap_sp] {
        std::fs::create_dir(sp.path("lazy")).unwrap();
    }

    let mut plain = start(
        &plain_sp,
        &script(
            &plain_sp.path("lazy"),
            &[&["lock", "status"], &["target", "ip"]],
        ),
    );
    let mut plain_wire = Tripwire::at(&plain_sp, &plain_b, plain.child.id() as i32);
    let mut trap = start(
        &trap_sp,
        &script(&trap_sp.path("lazy"), &[&["target", "ip"]]),
    );
    let mut trap_wire = Tripwire::at(&trap_sp, &trap_b, trap.child.id() as i32);

    plain.go();
    trap.go();
    assert_eq!(plain.step(), "step: quiet");
    assert_eq!(trap.step(), "step: quiet");
    assert_eq!(
        (plain_wire.hits(), trap_wire.hits()),
        (0, 0),
        "a command that needs no secret contacted a holder"
    );
    let (p, t) = (results(&plain_sp), results(&trap_sp));
    for (n, args) in NO_SECRET.iter().enumerate() {
        assert_eq!(
            p[n],
            t[n],
            "`apprafter {}` behaves differently when the vault cannot be read: it read the vault",
            args.join(" ")
        );
    }

    // The controls: `lock status` asks for the window once; `target ip` asks in the store
    // whose vault reads, and fails before asking in the other.
    plain.go();
    assert_eq!(plain.step(), "step: c0");
    assert_eq!(
        plain_wire.hits(),
        1,
        "lock status did not ask for the window"
    );
    plain.go();
    assert_eq!(plain.step(), "step: c1");
    assert_eq!(plain_wire.hits(), 2, "target ip did not ask for the window");
    trap.go();
    assert_eq!(trap.step(), "step: c0");
    assert_eq!(
        trap_wire.hits(),
        0,
        "target ip asked before it read the vault"
    );
    let (pe, te) = (
        std::fs::read_to_string(plain_sp.path("lazy/c1.err")).unwrap(),
        std::fs::read_to_string(trap_sp.path("lazy/c0.err")).unwrap(),
    );
    assert_ne!(
        plain_sp.mask_root(&pe),
        trap_sp.mask_root(&te),
        "target ip reads the vault, so the trap must show in its output; it did not"
    );
    plain.go();
    trap.go();
    plain.finish();
    trap.finish();
}

/// A fake holder at the endpoint the holder of `b` for the session `sid` would have, which
/// serves another store: each `Status` it reads is answered with another store's id, so no
/// command takes it as this store's window. Every connection is counted — those still queued
/// when it stops too, so a count read after a command ended has no race with its thread.
struct Elsewhere {
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    thread: std::thread::JoinHandle<usize>,
}

impl Elsewhere {
    fn at(sp: &Spawn, b: &StoreBinding, sid: i32) -> Elsewhere {
        use apprafter::holder::protocol::{
            read_request, write_reply, HolderStatus, Reply, Request, PROTOCOL, VERSION,
        };
        let base = sp.path("run").join("apprafter");
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&base)
            .unwrap();
        // SAFETY: geteuid has no failure mode.
        let uid = unsafe { libc::geteuid() };
        let path = match socket_endpoint(&base, &EndpointId::for_session(uid, b, sid)) {
            Ok(Endpoint::Socket { path, .. }) => path,
            other => panic!("{other:?}"),
        };
        let listener = UnixListener::bind(&path).unwrap();
        listener.set_nonblocking(true).unwrap();
        let other = if b.id == "0123456789abcdef" {
            "fedcba9876543210"
        } else {
            "0123456789abcdef"
        };
        let status = HolderStatus {
            protocol: PROTOCOL,
            version: VERSION.into(),
            store_id: other.into(),
            vault_id: apprafter_core::vault::VaultId([7; 16]),
            left_secs: 240,
            minutes: 5,
            hardening: apprafter_core::lock::HolderHardening {
                non_dumpable: Some(true),
                deny_attach: None,
                core_off: true,
                memory_locked: true,
                process_dacl: None,
            },
        };
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stopped = stop.clone();
        let thread = std::thread::spawn(move || {
            let mut count = 0;
            loop {
                match listener.accept() {
                    Ok((mut conn, _)) => {
                        count += 1;
                        // macOS hands the listener's O_NONBLOCK to the connection.
                        conn.set_nonblocking(false).unwrap();
                        conn.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
                        // A macOS client that finds this server is not its binary sends
                        // nothing and lets go: nothing to answer.
                        if let Ok(Request::Status) = read_request(&mut conn) {
                            let _ = write_reply(&mut conn, &Reply::Status(status.clone()));
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        if stopped.load(std::sync::atomic::Ordering::SeqCst) {
                            return count;
                        }
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Err(e) => panic!("the fake holder: {e}"),
                }
            }
        });
        Elsewhere { stop, thread }
    }

    /// Every connection made so far; the fake stops.
    fn stop(self) -> usize {
        self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
        self.thread.join().expect("the fake holder")
    }
}

/// T-lazy-2 and d4a-h F3: a command with no terminal looks for this terminal's window once —
/// its session's probe — and asks no holder again before a prompt it cannot show
/// (`store_locked` follows). F3's ask before a prompt runs only where a prompt is shown: the
/// macOS leg of the control above ("target ip did not ask for the window", 3 connections for
/// 2) found it there, where a server that is not this binary reads as no holder at all. Here
/// the holder serves another store, which reads the same on every OS.
#[test]
fn a_command_with_no_terminal_looks_for_its_window_once() {
    let (sp, b) = store(false);
    std::fs::create_dir(sp.path("lazy")).unwrap();
    let mut session = start(
        &sp,
        &format!(
            "D='{}'; read _\n\"$1\" target ip >\"$D/ip.out\" 2>\"$D/ip.err\"; echo \"step: ip $?\"; read _\n",
            sp.path("lazy").display()
        ),
    );
    let fake = Elsewhere::at(&sp, &b, session.child.id() as i32);
    session.go();
    assert_eq!(session.step(), "step: ip 1");
    let connections = fake.stop();
    let err = std::fs::read_to_string(sp.path("lazy/ip.err")).unwrap();
    assert!(err.contains("apprafter::secrets::store_locked"), "{err}");
    assert_eq!(
        connections, 1,
        "a command with no terminal looked for its window more than once:\n{err}"
    );
    session.go();
    session.finish();
}
