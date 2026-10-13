// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! `apprafter unlock`, `lock now` and `lock status` as a script or an agent sees them — no
//! terminal (D.4a T60, T65, T66, T69) — and `lock status` at a terminal, which asks nothing.
//! With a terminal and a window: `lock_window_test.rs`.

mod common;

use std::path::Path;

use apprafter_core::secrets::testing::locked_store;
use common::spawn::Spawn;

/// 64 ASCII alphanumerics, the shape cli_core accepts.
const TOKEN: &str = "cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd";

/// stderr with miette's wrapping undone: its `│` gutter dropped, each run of whitespace one space.
fn squeeze(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes)
        .replace('│', " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn run(sp: &Spawn, args: &[&str]) -> (Option<i32>, String, String) {
    let out = sp.cmd(args).output().expect("run apprafter");
    (
        out.status.code(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        squeeze(&out.stderr),
    )
}

/// `prod` in a locked store (one recovery-key slot).
fn locked(sp: &Spawn) {
    locked_store(&sp.store(), &[("prod", Some(TOKEN))]);
}

/// c1's frozen vector: a fast-parameter password slot and a recovery slot, id `0123456789abcdef`.
fn vector() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../apprafter-core/tests/vault_vectors/v1/fast-params")
}

/// T66: `lock status` on a store in files: the fact, exit 0, and no window anywhere.
#[test]
fn lock_status_on_a_store_in_files_says_so() {
    let sp = Spawn::new();
    let (code, out, err) = run(&sp, &["lock", "status"]);
    assert_eq!(code, Some(0), "{err}");
    assert!(
        out.contains("Secrets:        in files — readable by any program running as you; `apprafter lock on` protects them\n"),
        "{out}"
    );
    assert!(
        out.contains("This terminal:  not run from a terminal\n"),
        "{out}"
    );
}

/// T66, T72, O1: a locked store without a terminal: its slots, no window here, this computer's
/// static facts, and what the lock does not cover.
#[test]
fn lock_status_on_a_locked_store_names_its_slots_and_what_it_covers() {
    let sp = Spawn::new();
    locked(&sp);
    let (code, out, err) = run(&sp, &["lock", "status"]);
    assert_eq!(code, Some(0), "{err}");
    for line in [
        "Secrets:        locked — in the vault, opened by the recovery key\n",
        "Slots:          recovery key\n",
        "This terminal:  not run from a terminal\n",
        "  ℹ While a command talks to a cluster, its kubeconfig is a file programs running as you can read\n",
        "  ℹ Hibernation writes memory to disk, an open window's key included\n",
        "The lock protects secrets at rest.",
    ] {
        assert!(out.contains(line), "{line:?} in:\n{out}");
    }
}

/// T22, T23 (L3's spawned twin): the frozen fast-parameter password slot is refused by a
/// release-policy build and named so; a feature build says it is one, and uses the slot.
/// Passes under both policies, as every spawned lock test must (T23).
#[test]
fn lock_status_names_a_slot_by_this_builds_policy() {
    let sp = Spawn::new();
    for f in ["store.yaml", "vault"] {
        std::fs::copy(vector().join(f), sp.store().join(f)).unwrap();
    }
    let out = sp.cmd(&["lock", "status"]).output().unwrap();
    let (stdout, stderr) = (
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    assert_eq!(out.status.code(), Some(0), "{stderr}");
    if stderr.contains("INSECURE-FAST-KDF: this test build derives keys with weak parameters") {
        assert!(
            stdout.contains("Slots:          lock password · recovery key\n"),
            "{stdout}"
        );
    } else {
        assert!(
            stdout.contains("Slots:          lock password (refused by this build: kdf_too_weak) · recovery key\n"),
            "{stdout}"
        );
    }
}

/// T66 at a terminal: `lock status` asks nothing, and still reports.
#[cfg(unix)]
#[test]
fn lock_status_at_a_terminal_asks_nothing() {
    use std::io::Read;
    use std::os::fd::AsRawFd;
    let sp = Spawn::new();
    locked(&sp);
    let (master, slave) = common::spawn::open_pty();
    let out = sp
        .std_at_terminal(&["lock", "status"], slave)
        .stdout(std::process::Stdio::piped())
        .output()
        .unwrap();
    let mut seen = Vec::new();
    let mut m = std::fs::File::from(master);
    // SAFETY: fcntl on the master this test owns: its reads stop at "no more data".
    unsafe {
        let flags = libc::fcntl(m.as_raw_fd(), libc::F_GETFL);
        libc::fcntl(m.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK);
    }
    let _ = m.read_to_end(&mut seen);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&seen)
    );
    assert!(
        stdout.contains("This terminal:  no unlock window\n"),
        "{stdout}"
    );
    assert!(
        !String::from_utf8_lossy(&seen).contains("recovery key:"),
        "lock status asked for a secret"
    );
}

/// T65: `lock now` with no window is no error, on either kind of store.
#[test]
fn lock_now_without_a_window_says_so() {
    for lock in [false, true] {
        let sp = Spawn::new();
        if lock {
            locked(&sp);
        }
        let (code, out, err) = run(&sp, &["lock", "now"]);
        assert_eq!((code, out.as_str()), (Some(0), ""), "{err}");
        assert!(
            err.contains("No unlock window is open in this terminal."),
            "{err}"
        );
    }
}

/// T65, C31: `unlock` on a store in files is no error: there is nothing to unlock.
#[test]
fn unlock_on_a_store_in_files_says_it_is_not_locked() {
    let sp = Spawn::new();
    let (code, _, err) = run(&sp, &["unlock"]);
    assert_eq!(code, Some(0), "{err}");
    assert!(
        err.contains("is not locked: its secrets are in files, readable by any program running as you. `apprafter lock on` protects them."),
        "{err}"
    );
}

/// T61, T69: without a terminal, `unlock` asks nothing and refuses with `store_locked` —
/// before it looks at anything else: here the holder's base is open to others (no window
/// would be possible either), and the answer is still `store_locked`.
#[test]
fn unlock_without_a_terminal_is_store_locked_before_anything_else() {
    let sp = Spawn::new();
    locked(&sp);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let base = sp.path("run").join("apprafter");
        std::fs::create_dir_all(&base).unwrap();
        std::fs::set_permissions(&base, std::fs::Permissions::from_mode(0o777)).unwrap();
    }
    let (code, _, err) = run(&sp, &["unlock"]);
    assert_eq!(code, Some(1), "{err}");
    assert!(err.contains("apprafter::secrets::store_locked"), "{err}");
}

/// T65: an interrupted `lock on` is `lock_incomplete`, never an unlock.
#[test]
fn unlock_on_an_interrupted_lock_is_lock_incomplete() {
    let sp = Spawn::new();
    std::fs::copy(vector().join("vault"), sp.store().join("vault")).unwrap();
    let yaml = std::fs::read_to_string(vector().join("store.yaml"))
        .unwrap()
        .replace("secrets: locked", "secrets: locking");
    std::fs::write(sp.store().join("store.yaml"), yaml).unwrap();
    let (code, _, err) = run(&sp, &["unlock"]);
    assert_eq!(code, Some(1), "{err}");
    assert!(err.contains("apprafter::secrets::lock_incomplete"), "{err}");
}

/// O3: a window is 1 to 1440 minutes; 0 would open nothing.
#[test]
fn unlock_refuses_a_timeout_outside_one_to_a_day() {
    let sp = Spawn::new();
    for t in ["0", "1441"] {
        let (code, _, err) = run(&sp, &["unlock", "--timeout", t]);
        assert_eq!(code, Some(2), "{t}: {err}");
    }
}
