// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The unlock holder on Linux and macOS (D.4a T47–T56, T59, S9; holder §9.2). Each "terminal" is
//! a session a test owns — `Spawn::in_session` runs `/bin/sh -c <script>` as a session leader,
//! and the script's commands (this test binary re-run for one inner test, as `apprafter` itself
//! would run) are members of it. The holder is started the way the CLI starts it (`start_with`,
//! the store's key opened from its recovery key: no prompt, no KDF) and asked through its client.
//! Every holder is keyed by a scratch store's id and root: no test can name a person's holder.
#![cfg(unix)]

mod common;

use std::io::{BufRead, BufReader, Write};
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use apprafter::holder::client::{HolderClient, HolderError, HolderToEnd, IO_TIMEOUT};
use apprafter::holder::endpoint::{session_endpoint, socket_endpoint, Endpoint, EndpointId};
use apprafter::holder::protocol::{read_reply, HolderItem, InitHeader, Reply, StoreBinding};
use apprafter::holder::spawn::{start_with, HolderStarted};
use apprafter::holder::watch::STORE_LOOK;
use apprafter_core::secrets::testing::{locked_store, LockedTestStore};
use apprafter_core::secrets::{
    read_store_file, write_store_file, SecretsMode, StoreFile, StoreId, STORE_FILE,
};
use apprafter_core::vault::{
    KdfPolicy, OsRandom, RecoveryKey, SlotSecret, VaultFile, VaultId, VAULT_FILE,
};
use common::spawn::{Bin, Spawn};

/// 64 ASCII alphanumerics, the shape cli_core accepts.
const TOKEN: &str = "efefefefefefefefefefefefefefefefefefefefefefefefefefefefefefefef";
/// Set for the inner tests: run only inside a session an outer test started.
const INNER: &str = "APPRAFTER_HOLDER_INNER";
/// `t <inner test>` runs one inner test of this binary as a command of the session; `ask <what>`
/// asks the session's holder.
const PRELUDE: &str = "B=\"$2\"\n\
    t() { \"$B\" --exact \"$1\" --ignored --nocapture --test-threads=1; }\n\
    ask() { HOLDER_TEST_ASK=\"$1\"; export HOLDER_TEST_ASK; t inner_ask; }\n";
/// How long a session may take to print its next line.
const LINE_TIMEOUT: Duration = Duration::from_secs(30);

struct Store {
    sp: Spawn,
    binding: StoreBinding,
    _locked: LockedTestStore,
}

/// A locked scratch store in `sp`'s store: `prod` with a token, `staging` without one.
fn store(sp: Spawn) -> Store {
    let locked = locked_store(&sp.store(), &[("prod", Some(TOKEN)), ("staging", None)]);
    let key_line = locked
        .recovery_key()
        .expose()
        .lines()
        .nth(1)
        .unwrap()
        .to_string();
    let sp = sp
        .with_env(INNER, "1")
        .with_env("HOLDER_TEST_RECOVERY", key_line);
    let binding = StoreBinding {
        id: locked.store_id.as_str().into(),
        root: std::fs::canonicalize(sp.store()).unwrap(),
    };
    Store {
        sp,
        binding,
        _locked: locked,
    }
}

/// What an inner test said on `line`: the text after "holder: ". libtest prints `test <name> ... `
/// before a test runs and its result after, so with one thread and `--nocapture` an inner test's
/// first line starts with that prefix (GOTCHA-217): the marker is looked for within the line.
fn said(line: &str) -> Option<&str> {
    line.split_once("holder: ").map(|(_, said)| said)
}

/// The "holder: " lines of a session's output.
fn lines(out: &[u8]) -> Vec<String> {
    String::from_utf8_lossy(out)
        .lines()
        .filter_map(said)
        .map(str::to_string)
        .collect()
}

/// Run `script` as a session of its own, to its end.
fn session(sp: &Spawn, script: &str) -> Vec<String> {
    let out = sp
        .in_session(&format!("{PRELUDE}{script}"), None)
        .output()
        .expect("run the session");
    lines(&out.stdout)
}

/// A session the test reads line by line and lets go on (`go`) past each `read _`.
struct Live {
    child: Child,
    rx: mpsc::Receiver<String>,
}

impl Live {
    fn start(sp: &Spawn, script: &str) -> Live {
        let mut c = sp.in_session(&format!("{PRELUDE}{script}"), None);
        c.stdin(Stdio::piped()).stdout(Stdio::piped());
        let mut child = c.spawn().expect("start the session");
        let out = BufReader::new(child.stdout.take().unwrap());
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            for line in out.lines().map_while(Result::ok) {
                if let Some(l) = said(&line) {
                    if tx.send(l.to_string()).is_err() {
                        break;
                    }
                }
            }
        });
        Live { child, rx }
    }

    /// The session's id: its shell leads it.
    fn sid(&self) -> i32 {
        self.child.id() as i32
    }

    fn next(&self) -> String {
        self.rx
            .recv_timeout(LINE_TIMEOUT)
            .expect("the session printed its next line within 30 s")
    }

    fn go(&mut self) {
        self.child.stdin.as_mut().unwrap().write_all(b"\n").unwrap();
    }

    /// Close the shell's stdin (a `read _` returns, the script ends) and collect what is left.
    fn finish(mut self) -> Vec<String> {
        drop(self.child.stdin.take());
        self.child.wait().unwrap();
        self.rx.try_iter().collect()
    }
}

fn started_pid(line: &str) -> i32 {
    line.strip_prefix("started ")
        .unwrap_or_else(|| panic!("a holder started: {line}"))
        .parse()
        .unwrap()
}

/// The lock file of the holder of `b` for the session `sid`.
fn lock_of(sp: &Spawn, b: &StoreBinding, sid: i32) -> PathBuf {
    let base = sp.path("run").join("apprafter");
    // SAFETY: geteuid has no failure mode.
    let uid = unsafe { libc::geteuid() };
    match socket_endpoint(&base, &EndpointId::for_session(uid, b, sid)).unwrap() {
        Endpoint::Socket { lock, .. } => lock,
        Endpoint::Pipe { .. } => unreachable!("Unix"),
    }
}

/// A holder holds its lock file for its whole life: free means gone (a zombie holds no lock).
fn holder_alive(lock: &Path) -> bool {
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

fn gone_within(lock: &Path, limit: Duration) -> bool {
    let until = Instant::now() + limit;
    while Instant::now() < until {
        if !holder_alive(lock) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    !holder_alive(lock)
}

/// `root`'s `store.yaml` as `change` leaves it (team decision 2026-10-13): `files` is `lock
/// off`'s last two steps (the vault deleted, then `files`); `another id` another store's
/// `store.yaml` copied over it; `removed` no file at all. (`watch.rs`'s unit helper, which an
/// integration test cannot reach.)
fn change_store(root: &Path, change: &str) {
    if change == "removed" {
        std::fs::remove_file(root.join(STORE_FILE)).unwrap();
        return;
    }
    let mut file = read_store_file(root).unwrap().expect("a locked store");
    match change {
        "files" => {
            std::fs::remove_file(root.join(VAULT_FILE)).unwrap();
            file = StoreFile {
                secrets: SecretsMode::Files,
                id: None,
                slots: Vec::new(),
                unlock_timeout_min: None,
                ..file
            };
        }
        "another id" => file.id = Some(StoreId::generate(&mut OsRandom).unwrap()),
        other => panic!("no change {other}"),
    }
    let paths = cli_core::TargetStorePaths::for_root(root.to_path_buf());
    let held = cli_core::StoreLock::exclusive(&paths).unwrap();
    write_store_file(root, &file, &held).unwrap();
}

/// T47, T48, T56: a command of the unlocking terminal's session gets the token, an absent one
/// is `absent`, `Status` reports the window and the protections, and `End` stops the holder:
/// its lock is free and the next ask finds no holder.
#[test]
fn a_command_of_the_session_is_served_and_end_stops_the_holder() {
    let st = store(Spawn::new());
    let got = session(
        &st.sp,
        "t inner_start; ask item:prod; ask item:staging; ask status; ask end; ask status",
    );
    assert_eq!(got.len(), 6, "{got:?}");
    started_pid(&got[0]);
    assert_eq!(got[1], format!("item {TOKEN}"));
    assert_eq!(got[2], "absent");
    #[cfg(target_os = "linux")]
    assert!(
        got[3].starts_with("status left=")
            && got[3].ends_with(" minutes=5 nd=Some(true) da=None core=true mem=true"),
        "{}",
        got[3]
    );
    #[cfg(target_os = "macos")]
    assert!(
        got[3].starts_with("status left=")
            && got[3].ends_with(" minutes=5 nd=None da=Some(true) core=true mem=true"),
        "{}",
        got[3]
    );
    assert_eq!(&got[4..], ["ended", "none"]);
}

/// T51: another terminal — a process of another session, here at this session's very socket —
/// is refused, before a byte of its request is read (one that sends nothing is refused at once),
/// and the holder goes on serving its own. T49: the socket is 0600 (Linux: 01600, the sticky bit
/// keeping an `$XDG_RUNTIME_DIR` cleaner away; macOS refuses that bit on a non-directory).
#[test]
fn another_session_is_not_this_terminal() {
    use std::os::unix::fs::PermissionsExt;
    let st = store(Spawn::new());
    let mut s1 = Live::start(&st.sp, "t inner_start; read _; ask item:prod; ask end");
    started_pid(&s1.next());
    let lock = lock_of(&st.sp, &st.binding, s1.sid());
    let socket = lock.with_extension("sock");
    let mode = std::fs::symlink_metadata(&socket)
        .unwrap()
        .permissions()
        .mode()
        & 0o7777;
    #[cfg(target_os = "linux")]
    assert_eq!(mode, 0o1600, "{mode:o}");
    #[cfg(not(target_os = "linux"))]
    assert!(mode & 0o777 == 0o600, "{mode:o}");
    let sp2 = Spawn::new()
        .with_store(&st.sp.store())
        .with_env(INNER, "1")
        .with_env("XDG_RUNTIME_DIR", st.sp.path("run"))
        .with_env("HOLDER_TEST_SOCKET", &socket);
    let got = session(&sp2, "ask item:prod; ask say-nothing");
    assert_eq!(
        got,
        ["refused not_this_terminal", "refused not_this_terminal"]
    );
    s1.go();
    assert_eq!(s1.next(), format!("item {TOKEN}"));
    assert_eq!(s1.next(), "ended");
    s1.finish();
}

/// T52: the terminal's end — its session leader's exit — ends the holder within 2 s; through
/// the pidfd (Linux) or `NOTE_EXIT` (macOS), and through the 1 s look when they are not used.
#[test]
fn the_holder_ends_with_its_session() {
    for poll in [false, true] {
        let st = store(Spawn::new());
        let sp = if poll {
            st.sp.with_env("HOLDER_TEST_POLL", "1")
        } else {
            st.sp
        };
        let s = Live::start(&sp, "t inner_start; read _");
        started_pid(&s.next());
        let lock = lock_of(&sp, &st.binding, s.sid());
        assert!(holder_alive(&lock));
        s.finish();
        assert!(gone_within(&lock, Duration::from_secs(2)), "poll={poll}");
    }
}

/// T56: a window of 2 s (the test-only init field) ends on its own — the holder's loop wakes at
/// the window's end — and a later command finds no holder. (A request that arrives in the
/// instant before the holder leaves gets `window_ended`: `serve`'s unit test.)
#[test]
fn an_expired_window_ends_the_holder() {
    let st = store(Spawn::new());
    let sp = st.sp.with_env("HOLDER_TEST_WINDOW_SECS", "2");
    let mut s = Live::start(&sp, "t inner_start; read _; ask item:prod");
    started_pid(&s.next());
    let lock = lock_of(&sp, &st.binding, s.sid());
    assert!(holder_alive(&lock));
    assert!(
        gone_within(&lock, Duration::from_secs(4)),
        "the window was 2 s"
    );
    s.go();
    assert_eq!(s.next(), "none");
    s.finish();
}

/// T56, team decision 2026-10-13: a holder watches its store. `lock off` in any terminal (the
/// vault deleted, then `files`), another store's `store.yaml` copied over it, or the file
/// removed ends the holder within its look ([`STORE_LOOK`]), while its window (5 min) and its
/// session go on — no `End` from anywhere.
#[test]
fn a_holder_ends_within_its_look_once_its_store_is_not_locked_with_its_id() {
    for change in ["files", "another id", "removed"] {
        let st = store(Spawn::new());
        let s = Live::start(&st.sp, "t inner_start; read _");
        started_pid(&s.next());
        let lock = lock_of(&st.sp, &st.binding, s.sid());
        assert!(holder_alive(&lock), "{change}: the holder serves");
        change_store(&st.binding.root, change);
        assert!(
            gone_within(&lock, STORE_LOOK + Duration::from_secs(3)),
            "{change}: the holder outlived its store's lock by more than its look"
        );
        s.finish();
    }
}

/// Holder §2.4: one holder per terminal and store — a second start finds the first, which goes
/// on serving.
#[test]
fn a_second_holder_finds_the_first() {
    let st = store(Spawn::new());
    let got = session(
        &st.sp,
        "t inner_start; t inner_start; ask item:prod; ask end",
    );
    let first = started_pid(&got[0]);
    assert_eq!(got[1], format!("already {first}"));
    assert_eq!(&got[2..], [format!("item {TOKEN}"), "ended".to_string()]);
}

/// Holder §2.4: a holder killed outright leaves its socket; a client finds no holder there,
/// and the next holder replaces it.
#[test]
fn a_stale_socket_is_replaced() {
    let st = store(Spawn::new());
    let mut s = Live::start(
        &st.sp,
        "t inner_start; read _; ask status; t inner_start; ask item:prod; ask end",
    );
    let pid = started_pid(&s.next());
    let lock = lock_of(&st.sp, &st.binding, s.sid());
    // SAFETY: SIGKILL to the holder this test started.
    unsafe { libc::kill(pid, libc::SIGKILL) };
    assert!(gone_within(&lock, Duration::from_secs(2)));
    assert!(
        lock.with_extension("sock").exists(),
        "a killed holder leaves its socket"
    );
    s.go();
    assert_eq!(s.next(), "none");
    let second = started_pid(&s.next());
    assert_ne!(second, pid);
    assert_eq!(s.next(), format!("item {TOKEN}"));
    assert_eq!(s.next(), "ended");
    s.finish();
}

/// T53: a holder serves only its store — another store's vault is `wrong_store` — and only its
/// vault's key: started with another vault's id (a rekeyed vault), it answers `key_mismatch`.
#[test]
fn each_holder_serves_only_its_store_and_its_key() {
    let st = store(Spawn::new());
    let other_dir = tempfile::TempDir::new_in("/tmp").unwrap();
    locked_store(other_dir.path(), &[("prod", Some(TOKEN))]);
    let sp = st
        .sp
        .with_store(other_dir.path())
        .with_env("HOLDER_TEST_VAULT", other_dir.path().join("vault"));
    // with_store moved APPRAFTER_CONFIG_DIR: point the holder back at the first store.
    let sp = sp.with_env("HOLDER_TEST_STORE", &st.binding.root);
    let got = session(&sp, "t inner_start; ask item:prod; ask end");
    assert_eq!(&got[1..], ["refused wrong_store", "ended"]);
    let st = store(Spawn::new());
    let sp = st.sp.with_env("HOLDER_TEST_VAULT_ID", "09".repeat(16));
    let got = session(&sp, "t inner_start; ask item:prod; ask end");
    assert_eq!(&got[1..], ["refused key_mismatch", "ended"]);
}

/// Holder §1.2 step 3: a descriptor the starting command holds without close-on-exec does not
/// stay open in the holder — the test's read of that pipe sees its end once the command is gone,
/// while the holder lives.
#[test]
fn the_holder_keeps_no_descriptor_its_starter_had() {
    let st = store(Spawn::new());
    let fifo = st.sp.path("tmp").join("fifo");
    let c = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
    // SAFETY: mkfifo on a fresh path under the sandbox.
    assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
    let reader_path = fifo.clone();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        // Blocks until the inner test opens the write end; returns when every writer closed it.
        let mut f = std::fs::File::open(&reader_path).unwrap();
        let mut sink = Vec::new();
        std::io::Read::read_to_end(&mut f, &mut sink).unwrap();
        let _ = tx.send(());
    });
    let sp = st.sp.with_env("HOLDER_TEST_FIFO", &fifo);
    let mut s = Live::start(&sp, "t inner_start; read _; ask end");
    started_pid(&s.next());
    let lock = lock_of(&sp, &st.binding, s.sid());
    assert!(
        rx.recv_timeout(Duration::from_secs(2)).is_ok(),
        "the pipe's write end is still open: the holder kept a descriptor it inherited"
    );
    assert!(holder_alive(&lock));
    s.go();
    assert_eq!(s.next(), "ended");
    s.finish();
}

/// T49: a base open to others is refused before anything binds, and the CLI is told why.
#[test]
fn an_open_base_is_refused_before_bind() {
    use std::os::unix::fs::PermissionsExt;
    let st = store(Spawn::new());
    let base = st.sp.path("run").join("apprafter");
    std::fs::create_dir(&base).unwrap();
    std::fs::set_permissions(&base, std::fs::Permissions::from_mode(0o755)).unwrap();
    let got = session(&st.sp, "t inner_start");
    assert_eq!(got, ["unavailable UnsafeRuntimeDir"]);
    assert_eq!(
        std::fs::read_dir(&base).unwrap().count(),
        0,
        "nothing was bound"
    );
}

/// T54 (Linux): the holder is not dumpable — its `/proc` files are root's — and refuses a
/// ptrace attach even from its reaper, which Yama's scope 1 would allow.
#[cfg(target_os = "linux")]
#[test]
fn linux_the_holder_is_not_dumpable_and_refuses_ptrace() {
    use std::os::unix::fs::MetadataExt;
    // SAFETY: geteuid has no failure mode.
    assert_ne!(
        unsafe { libc::geteuid() },
        0,
        "run as a non-root user: root may ptrace anything"
    );
    // SAFETY: this test process becomes the reaper of its orphaned descendants: the holder.
    assert_eq!(
        unsafe { libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0) },
        0
    );
    let st = store(Spawn::new());
    let mut s = Live::start(&st.sp, "t inner_start; read _; ask end");
    let pid = started_pid(&s.next());
    let meta = std::fs::metadata(format!("/proc/{pid}/environ")).unwrap();
    assert_eq!(
        meta.uid(),
        0,
        "a non-dumpable process's /proc files belong to root"
    );
    let null = std::ptr::null_mut::<libc::c_void>();
    // SAFETY: an attach attempt on the holder; detached at once should it ever succeed.
    let rc = unsafe { libc::ptrace(libc::PTRACE_ATTACH, pid, null, null) };
    let errno = std::io::Error::last_os_error().raw_os_error();
    if rc == 0 {
        // SAFETY: as above.
        unsafe { libc::ptrace(libc::PTRACE_DETACH, pid, null, null) };
    }
    assert_eq!((rc, errno), (-1, Some(libc::EPERM)));
    s.go();
    assert_eq!(s.next(), "ended");
    s.finish();
    // Reap the holder this reaper inherited — that pid only: another test's children are its own.
    let until = Instant::now() + Duration::from_secs(2);
    // SAFETY: waitpid on the holder, now this process's child.
    while unsafe { libc::waitpid(pid, std::ptr::null_mut(), libc::WNOHANG) } == 0
        && Instant::now() < until
    {
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// A connect that never waits, to the socket at `path`: queued, or turned away at once — the
/// listener's queue full is `EAGAIN` on Linux and `ECONNREFUSED` on macOS.
fn queue_one(path: &Path) -> std::io::Result<std::os::fd::OwnedFd> {
    use std::os::fd::{FromRawFd, OwnedFd};
    // SAFETY: a new socket, owned below.
    let fd = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0) };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: a descriptor this function opened and nothing else owns.
    let fd = unsafe { OwnedFd::from_raw_fd(fd) };
    // SAFETY: fcntl on that descriptor.
    unsafe {
        let flags = libc::fcntl(fd.as_raw_fd(), libc::F_GETFL);
        assert_eq!(
            libc::fcntl(fd.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK),
            0
        );
    }
    // SAFETY: an all-zero sockaddr_un is valid; the path is copied in below.
    let mut addr: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    addr.sun_family = libc::AF_UNIX as libc::sa_family_t;
    let bytes = path.as_os_str().as_bytes();
    assert!(bytes.len() < addr.sun_path.len(), "{path:?} fits sun_path");
    for (d, s) in addr.sun_path.iter_mut().zip(bytes) {
        *d = *s as libc::c_char;
    }
    // SAFETY: connect with the address above and its size.
    let rc = unsafe {
        libc::connect(
            fd.as_raw_fd(),
            (&raw const addr).cast(),
            std::mem::size_of::<libc::sockaddr_un>() as libc::socklen_t,
        )
    };
    if rc != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(fd)
}

/// T59: the client's connect is a step like its read and its write. A holder that never
/// accepts, its listen queue full (a hung holder that other connects reached first), holds the
/// client no longer than a step: it finds no holder there, and the command goes on without one.
/// Linux makes a connect to a full queue wait (`EAGAIN` when it may not); macOS turns it away.
#[test]
fn a_holder_whose_queue_is_full_holds_no_client_past_its_step() {
    let dir = tempfile::TempDir::new_in("/tmp").unwrap();
    let path = dir.path().join("h.sock");
    let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
    // SAFETY: the listener's queue cut to the smallest one the OS keeps; it never accepts.
    assert_eq!(unsafe { libc::listen(listener.as_raw_fd(), 0) }, 0);
    let mut queued = Vec::new();
    let full = loop {
        match queue_one(&path) {
            Ok(fd) => queued.push(fd),
            Err(e) => break e,
        }
        assert!(queued.len() < 1024, "the listen queue never filled");
    };
    assert!(
        !queued.is_empty(),
        "the queue takes a connect first: {full}"
    );
    #[cfg(target_os = "linux")]
    assert_eq!(full.kind(), std::io::ErrorKind::WouldBlock, "{full}");
    #[cfg(target_os = "macos")]
    assert_eq!(full.kind(), std::io::ErrorKind::ConnectionRefused, "{full}");
    let endpoint = Endpoint::Socket {
        path,
        lock: PathBuf::new(),
    };
    let (tx, rx) = mpsc::channel();
    let asked = Instant::now();
    std::thread::spawn(move || {
        let _ = tx.send(HolderClient::at(&endpoint).is_some());
    });
    let found = rx
        .recv_timeout(Duration::from_secs(5))
        .expect("the client's connect gave up within its step, never past the 5 s of a request");
    let waited = asked.elapsed();
    assert!(!found, "a holder that never accepts is no holder");
    assert!(
        waited < IO_TIMEOUT + Duration::from_secs(1),
        "the connect took {waited:?}, past its {IO_TIMEOUT:?} step"
    );
    drop(queued);
    drop(listener);
}

/// T59, S9: `lock now` ends a holder that does not answer — exactly the process the
/// connection's server is — and never one that answered and closed.
#[test]
fn a_hung_holder_is_ended_and_a_live_one_never() {
    let st = store(Spawn::new());
    let mut s = Live::start(
        &st.sp,
        "t inner_start; read _; ask terminate-after-reply; read _; ask terminate-hung; ask status",
    );
    let pid = started_pid(&s.next());
    let lock = lock_of(&st.sp, &st.binding, s.sid());
    s.go();
    assert_eq!(
        s.next(),
        "error NotFound",
        "a holder that answered and closed is never killed"
    );
    assert!(holder_alive(&lock));
    // SAFETY: SIGSTOP to the holder this test started: it accepts, and answers nothing.
    unsafe { libc::kill(pid, libc::SIGSTOP) };
    s.go();
    assert_eq!(s.next(), "terminated");
    assert!(gone_within(&lock, Duration::from_secs(2)));
    assert_eq!(s.next(), "none");
    s.finish();
}

/// T47: `apprafter __holder` typed at a terminal is no command: it says so and exits 2. The
/// terminal is read while the child runs (`converse`): macOS drains a session leader's
/// controlling terminal as the leader exits (XNU's `proc_exit` waits in `ttywait` until the
/// master has read what the slave wrote), so a terminal nobody reads holds that exit for good.
#[test]
fn the_holder_refuses_a_terminal() {
    let ended = common::terminal::converse(&Spawn::new(), &["__holder"], &[]);
    assert_eq!(ended.status.code(), Some(2), "{}", ended.terminal);
    assert!(
        ended.terminal.contains("not a command"),
        "{}",
        ended.terminal
    );
}

/// T47: a holder whose init pipe carries no init frame writes nothing and exits.
#[test]
fn a_holder_without_an_init_frame_writes_nothing() {
    let sp = Spawn::new();
    let out = sp
        .in_session(
            "printf 'not a frame' | \"$1\" __holder; echo \"holder: exit $?\"",
            None,
        )
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stdout), "holder: exit 1\n");
}

/// G8: a holder a test leaves behind fails that test when its `Spawn` drops — here the session
/// (and so its holder) is still open as the `Spawn` goes.
#[test]
fn a_leaked_holder_fails_its_test() {
    let leaked = std::panic::catch_unwind(|| {
        let st = store(Spawn::new());
        let s = Live::start(&st.sp, "t inner_start; read _");
        started_pid(&s.next());
        drop(st);
        s
    });
    assert!(
        leaked.is_err(),
        "Spawn's Drop must fail a test that leaks a holder"
    );
}

/// T53 (macOS): a process that took the holder's socket — here this test binary, listening
/// where this session's holder would — is no holder: the client finds none and the command
/// would prompt. Without the check it would send its request there and wait. The client asks
/// only once the squatter says it listens (shell builtins only: `Spawn`'s `PATH` is an empty
/// directory, so there is no `sleep`), so the answer is the identity check's, never timing's.
#[cfg(target_os = "macos")]
#[test]
fn macos_a_squatter_at_the_holders_socket_is_no_holder() {
    let st = store(Spawn::new());
    let mut s = Live::start(&st.sp, "t inner_squat & read _; ask status; wait");
    assert_eq!(s.next(), "squatting");
    s.go();
    assert_eq!(s.next(), "none");
    assert!(s.finish().is_empty());
}

/// T53, T58 (macOS; critic-d4a-e finding 13): an upgrade that moved the binary leaves a holder
/// of the old path. No command reads through it — its image is not this binary — but a client
/// that only ends a holder (`lock now`, `unlock`, `lock off`) accepts an image of this binary's
/// file name, so it is ended.
#[cfg(target_os = "macos")]
#[test]
fn macos_a_holder_an_upgrade_moved_is_read_by_none_and_ended() {
    let st = store(Spawn::new());
    let old = st.sp.path("old");
    std::fs::create_dir_all(&old).unwrap();
    let moved = old.join("apprafter");
    std::fs::copy(Bin::Apprafter.path(), &moved).unwrap();
    let script = format!(
        "HOLDER_TEST_PROGRAM='{}'; export HOLDER_TEST_PROGRAM; t inner_start; \
         unset HOLDER_TEST_PROGRAM; ask status; ask end-old; ask status",
        moved.display()
    );
    let s = Live::start(&st.sp, &script);
    started_pid(&s.next());
    let lock = lock_of(&st.sp, &st.binding, s.sid());
    assert_eq!(
        s.next(),
        "none",
        "a command read through another install's holder"
    );
    assert_eq!(s.next(), "ended");
    assert!(gone_within(&lock, Duration::from_secs(2)));
    assert_eq!(s.next(), "none");
    assert!(s.finish().is_empty());
}

/// T53 (Linux): no identity check is possible — the holder's image is unreadable to the same
/// user (non-dumpable) — pinned so a kernel that allows it is noticed.
#[cfg(target_os = "linux")]
#[test]
fn linux_the_holders_image_is_unreadable() {
    let st = store(Spawn::new());
    let mut s = Live::start(&st.sp, "t inner_start; read _; ask end");
    let pid = started_pid(&s.next());
    let err = std::fs::read_link(format!("/proc/{pid}/exe")).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::PermissionDenied);
    s.go();
    assert_eq!(s.next(), "ended");
    s.finish();
}

// ---------------------------------------------------------------------
// Inner tests: the commands of a session
// ---------------------------------------------------------------------

struct Inner {
    root: PathBuf,
    base: PathBuf,
}

fn inner() -> Inner {
    assert!(
        std::env::var_os(INNER).is_some(),
        "run only as a command of a session holder_unix's tests start: an `--ignored` run \
         reaches it alone and fails here on purpose"
    );
    let root = std::env::var_os("HOLDER_TEST_STORE")
        .or_else(|| std::env::var_os("APPRAFTER_CONFIG_DIR"))
        .unwrap();
    Inner {
        root: std::fs::canonicalize(root).unwrap(),
        base: PathBuf::from(std::env::var_os("XDG_RUNTIME_DIR").unwrap()).join("apprafter"),
    }
}

fn binding_of(root: &Path) -> StoreBinding {
    let file = read_store_file(root).unwrap().expect("a locked store");
    StoreBinding {
        id: file.id.expect("an id").as_str().into(),
        root: root.to_path_buf(),
    }
}

#[test]
#[ignore = "a command of a session holder_unix's tests start"]
fn inner_start() {
    let w = inner();
    let bytes = std::fs::read(w.root.join("vault")).unwrap();
    let key = RecoveryKey::parse(&std::env::var("HOLDER_TEST_RECOVERY").unwrap()).unwrap();
    let opened = VaultFile::parse(&bytes)
        .unwrap()
        .open(&SlotSecret::RecoveryKey(key), KdfPolicy::active())
        .unwrap();
    // A descriptor this command holds without close-on-exec, as a shell's `3>` would leave it.
    let _kept = std::env::var_os("HOLDER_TEST_FIFO").map(|p| {
        let c = std::ffi::CString::new(p.as_bytes()).unwrap();
        // SAFETY: open(2) of the test's FIFO for writing; deliberately no O_CLOEXEC.
        let fd = unsafe { libc::open(c.as_ptr(), libc::O_WRONLY) };
        assert!(fd > 2);
        fd
    });
    let vault_id = match std::env::var("HOLDER_TEST_VAULT_ID") {
        Ok(hex) => VaultId(std::array::from_fn(|i| {
            u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).unwrap()
        })),
        Err(_) => opened.header().vault_id,
    };
    let init = InitHeader {
        store: binding_of(&w.root),
        base: w.base,
        vault_id,
        minutes: 5,
        window_secs: std::env::var("HOLDER_TEST_WINDOW_SECS")
            .ok()
            .map(|s| s.parse().unwrap()),
        poll_leader: std::env::var_os("HOLDER_TEST_POLL").is_some(),
        client_pid: std::process::id(),
    };
    // The program a moved install would run (`HOLDER_TEST_PROGRAM`), else this build's.
    let program = std::env::var_os("HOLDER_TEST_PROGRAM")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(Bin::Apprafter.path()));
    match start_with(&program, &init, opened.data_key()) {
        Ok(HolderStarted::Started { pid }) => println!("holder: started {pid}"),
        Ok(HolderStarted::AlreadyRunning { pid }) => println!("holder: already {pid}"),
        Err(u) => println!("holder: unavailable {:?}", u.reason()),
    }
}

#[test]
#[ignore = "a command of a session holder_unix's tests start"]
fn inner_ask() {
    let w = inner();
    let store = binding_of(&w.root);
    let endpoint = match std::env::var_os("HOLDER_TEST_SOCKET") {
        Some(p) => Endpoint::Socket {
            path: PathBuf::from(p),
            lock: PathBuf::new(),
        },
        None => session_endpoint(&w.base, &store).unwrap(),
    };
    let vault_path = std::env::var_os("HOLDER_TEST_VAULT")
        .map(PathBuf::from)
        .unwrap_or_else(|| w.root.join("vault"));
    let vault = std::fs::read(vault_path).unwrap();
    // A peer that connects and sends nothing: §3.10's order judges it before a byte of its
    // request is read, so another session's refusal comes at once, never after the holder's 2 s
    // read (critic-d4a-e finding 21).
    if std::env::var("HOLDER_TEST_ASK").as_deref() == Ok("say-nothing") {
        let Endpoint::Socket { path, .. } = &endpoint else {
            unreachable!("Unix")
        };
        let mut conn = std::os::unix::net::UnixStream::connect(path).unwrap();
        conn.set_read_timeout(Some(Duration::from_secs(1))).unwrap();
        let said = match read_reply(&mut conn) {
            Ok(Reply::Refused(r)) => format!("refused {}", r.as_str()),
            Ok(other) => format!("reply {other:?}"),
            Err(e) => format!("error {e:?}"),
        };
        println!("holder: {said}");
        return;
    }
    if std::env::var("HOLDER_TEST_ASK").as_deref() == Ok("end-old") {
        let said = match HolderToEnd::at_expecting(&endpoint, Path::new(Bin::Apprafter.path())) {
            Some(mut old) => match old.end() {
                Ok(()) => "ended".to_string(),
                Err(e) => format!("error {e:?}"),
            },
            None => "none".to_string(),
        };
        println!("holder: {said}");
        return;
    }
    let Some(mut client) = HolderClient::at_expecting(&endpoint, Path::new(Bin::Apprafter.path()))
    else {
        println!("holder: none");
        return;
    };
    let ask = std::env::var("HOLDER_TEST_ASK").unwrap();
    let said: Result<String, HolderError> = match ask.as_str() {
        "status" => client.status().map(|s| {
            format!(
                "status left={} minutes={} nd={:?} da={:?} core={} mem={}",
                s.left_secs,
                s.minutes,
                s.hardening.non_dumpable,
                s.hardening.deny_attach,
                s.hardening.core_off,
                s.hardening.memory_locked
            )
        }),
        "end" => client.end().map(|()| "ended".to_string()),
        "terminate-after-reply" => match client.status() {
            // S9: once the holder has let go of this connection (it closes it after its reply),
            // `terminate_server` must refuse. Waited for, so the check meets the close and the
            // result is never decided by timing (critic-d4a-e finding 11).
            Ok(_) => {
                assert!(
                    client.closed_within(Duration::from_secs(2)),
                    "the holder let go of the connection after its reply"
                );
                client.terminate_server().map(|()| "terminated".to_string())
            }
            Err(e) => Err(e),
        },
        "terminate-hung" => match client.status() {
            Err(HolderError::Io(_)) => client.terminate_server().map(|()| "terminated".to_string()),
            Ok(_) => Ok("answered".to_string()),
            Err(e) => Err(e),
        },
        item => {
            let target = item.strip_prefix("item:").expect("item:<target>");
            client
                .open_item(
                    &vault,
                    &HolderItem::HetznerToken {
                        target: target.into(),
                    },
                )
                .map(|got| match got {
                    Some(v) => format!("item {}", String::from_utf8_lossy(&v)),
                    None => "absent".to_string(),
                })
        }
    };
    let said = said.unwrap_or_else(|e| match e {
        HolderError::Refused(r) => format!("refused {}", r.as_str()),
        HolderError::Io(e) => format!("error {:?}", e.kind()),
        other => format!("error {other:?}"),
    });
    println!("holder: {said}");
}

#[cfg(target_os = "macos")]
#[test]
#[ignore = "a squatter in a session holder_unix's tests start"]
fn inner_squat() {
    let w = inner();
    let Endpoint::Socket { path, .. } = session_endpoint(&w.base, &binding_of(&w.root)).unwrap()
    else {
        unreachable!()
    };
    let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
    println!("holder: squatting");
    std::io::stdout().flush().unwrap();
    listener.set_nonblocking(true).unwrap();
    let until = Instant::now() + Duration::from_secs(4);
    let mut held = Vec::new();
    while Instant::now() < until {
        if let Ok((c, _)) = listener.accept() {
            held.push(c);
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let _ = std::fs::remove_file(&path);
}
