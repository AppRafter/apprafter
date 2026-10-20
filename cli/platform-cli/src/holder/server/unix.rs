// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The holder process on Linux and macOS (D.4a T51–T56; holder §1.2, §1.3, §2.4, §3.4).

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::mem::ManuallyDrop;
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

use apprafter_core::lock::HolderHardening;
use apprafter_core::process::ProcStart;

use crate::holder::binding::{admit, Admission, ProcLookup, SessionBinding};
use crate::holder::endpoint::{check_base, socket_endpoint, Endpoint, EndpointId};
use crate::holder::harden::harden_self;
use crate::holder::memory::LockedKey;
use crate::holder::os::{self, current, OsProcs, Signals, Wake};
use crate::holder::protocol::{
    read_init, read_request, write_ack, write_reply, Ack, FrameError, HolderUnavailable,
    InitHeader, Refusal, Reply, PROTOCOL, VERSION,
};
use crate::holder::serve::{handle, Held};
use crate::holder::watch::StoreWatch;
use crate::holder::window::{SystemClock, Window, WindowState};

/// How long one client may take to send its request or read the reply.
const IO_TIMEOUT: Duration = Duration::from_secs(2);
/// The longest the loop waits between two looks at the window (the store's watch looks more
/// often: `watch::STORE_LOOK`).
const MAX_NAP: Duration = Duration::from_secs(30);

pub(super) fn run() -> i32 {
    // SAFETY: isatty only reads fd 0's state.
    if unsafe { libc::isatty(0) } == 1 {
        eprintln!("{}", crate::holder::NOT_A_COMMAND);
        return 2;
    }
    // fd 1 is the ack pipe until the handshake ends; never closed through a `File`.
    // SAFETY: fd 1 exists in any process std started.
    let mut ack = ManuallyDrop::new(unsafe { File::from_raw_fd(1) });
    let running = match prepare(&mut OsSteps).and_then(bind) {
        Ok(r) => r,
        Err(Stop::Ack(a)) => {
            let _ = write_ack(&mut *ack, &a);
            return 1;
        }
        Err(Stop::Quiet) => return 1,
    };
    let ready = Ack::Ready {
        pid: std::process::id(),
        protocol: PROTOCOL,
        version: VERSION.into(),
    };
    if write_ack(&mut *ack, &ready).is_err() {
        return 1;
    }
    os::null_stdio();
    serve(running)
}

/// Why a holder stops before it serves: with an ack the CLI reads, or quietly (no init frame).
pub(crate) enum Stop {
    Ack(Ack),
    Quiet,
}

/// The six first steps, in T54's order (`prepare`).
pub(crate) trait StartSteps {
    fn record_session(&mut self) -> (u32, i32);
    fn detach(&mut self) -> io::Result<()>;
    fn harden(&mut self) -> HolderHardening;
    fn close_inherited(&mut self);
    fn lock_page(&mut self) -> io::Result<LockedKey>;
    fn read_init(&mut self, key: &mut [u8; 32]) -> Result<InitHeader, FrameError>;
}

pub(crate) struct Prepared {
    uid: u32,
    sid: i32,
    hardening: HolderHardening,
    key: LockedKey,
    init: InitHeader,
}

/// Record the session the holder was born in (the unlocking terminal's: the binding is not
/// something the init frame claims), leave it, harden, close every inherited descriptor, lock a
/// page, and only then read the key into it (T54; holder finding 6).
pub(crate) fn prepare(s: &mut impl StartSteps) -> Result<Prepared, Stop> {
    let (uid, sid) = s.record_session();
    s.detach()
        .map_err(|_| Stop::Ack(Ack::Unavailable(HolderUnavailable::NoSessionLeader)))?;
    let mut hardening = s.harden();
    s.close_inherited();
    let mut key = s.lock_page().map_err(|_| Stop::Quiet)?;
    hardening.memory_locked = key.locked();
    let init = s.read_init(key.bytes_mut()).map_err(|_| Stop::Quiet)?;
    Ok(Prepared {
        uid,
        sid,
        hardening,
        key,
        init,
    })
}

struct OsSteps;

impl StartSteps for OsSteps {
    fn record_session(&mut self) -> (u32, i32) {
        // SAFETY: neither call can fail for this process.
        unsafe { (libc::geteuid(), libc::getsid(0)) }
    }
    fn detach(&mut self) -> io::Result<()> {
        // SAFETY: setsid has no memory effects; it fails only for a group leader.
        if unsafe { libc::setsid() } == -1 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
    fn harden(&mut self) -> HolderHardening {
        harden_self()
    }
    fn close_inherited(&mut self) {
        os::close_inherited();
    }
    fn lock_page(&mut self) -> io::Result<LockedKey> {
        LockedKey::new()
    }
    fn read_init(&mut self, key: &mut [u8; 32]) -> Result<InitHeader, FrameError> {
        // fd 0 is the init pipe; `null_stdio` replaces it once the handshake is over.
        // SAFETY: fd 0 exists in any process std started.
        let mut pipe = ManuallyDrop::new(unsafe { File::from_raw_fd(0) });
        read_init(&mut *pipe, key)
    }
}

struct Running {
    binding: SessionBinding<ProcStart>,
    window: Window,
    /// Its store, re-read every `STORE_LOOK` (team decision 2026-10-13).
    watch: StoreWatch,
    listener: UnixListener,
    socket: PathBuf,
    _lock: File,
    waiter: current::Waiter,
    _signals: Signals,
    key: LockedKey,
    init: InitHeader,
    hardening: HolderHardening,
}

impl Drop for Running {
    /// While the lock is still held no newer holder exists: the socket is this holder's.
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.socket);
    }
}

/// Bind the session, take the endpoint, listen (holder §1.2 steps 6–7, §2.4).
fn bind(p: Prepared) -> Result<Running, Stop> {
    let unavailable = |u| Stop::Ack(Ack::Unavailable(u));
    // The session the CLI ran in must still have its leader, still leading it.
    // SAFETY: getsid answers for any pid.
    let leads = unsafe { libc::getsid(p.sid) } == p.sid;
    let leader_started = leads
        .then(|| OsProcs.started(p.sid))
        .flatten()
        .ok_or(unavailable(HolderUnavailable::NoSessionLeader))?;
    let binding = SessionBinding {
        uid: p.uid,
        sid: p.sid,
        leader_started,
    };
    check_base(&p.init.base).map_err(unavailable)?;
    let id = EndpointId::for_session(p.uid, &p.init.store, p.sid);
    let Endpoint::Socket { path, lock } =
        socket_endpoint(&p.init.base, &id).map_err(unavailable)?
    else {
        return Err(Stop::Quiet);
    };
    let refuse = |path: &PathBuf, e: io::Error| {
        unavailable(HolderUnavailable::UnsafeRuntimeDir {
            path: path.clone(),
            reason: e.to_string(),
        })
    };
    let mut lock_file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&lock)
        .map_err(|e| refuse(&lock, e))?;
    // One holder per terminal and store: a held lock is a live holder.
    // SAFETY: flock on the file just opened; held for the holder's life.
    if unsafe { libc::flock(lock_file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        let mut text = String::new();
        let _ = lock_file.read_to_string(&mut text);
        let pid = text.trim().parse().unwrap_or(0);
        return Err(Stop::Ack(Ack::AlreadyRunning { pid }));
    }
    // The lock is this holder's, so a socket at `path` is a dead holder's.
    let _ = std::fs::remove_file(&path);
    let listener = UnixListener::bind(&path).map_err(|e| refuse(&path, e))?;
    restrict_socket(&path).map_err(|e| refuse(&path, e))?;
    lock_file.set_len(0).map_err(|e| refuse(&lock, e))?;
    lock_file
        .seek(SeekFrom::Start(0))
        .map_err(|e| refuse(&lock, e))?;
    writeln!(lock_file, "{}", std::process::id()).map_err(|e| refuse(&lock, e))?;
    let signals = Signals::new().map_err(|_| Stop::Quiet)?;
    let waiter = current::Waiter::new(
        listener.as_raw_fd(),
        signals.fd(),
        p.sid,
        leader_started,
        p.init.poll_leader,
    )
    .map_err(|_| Stop::Quiet)?;
    let window = Window::open(
        Window::length(p.init.minutes, p.init.window_secs),
        &SystemClock,
    );
    let watch = StoreWatch::new(&p.init.store);
    Ok(Running {
        binding,
        window,
        watch,
        listener,
        socket: path,
        _lock: lock_file,
        waiter,
        _signals: signals,
        key: p.key,
        init: p.init,
        hardening: p.hardening,
    })
}

/// The socket's mode (T49): 01600, the sticky bit keeping an `$XDG_RUNTIME_DIR` cleaner away
/// (basedir spec). A chmod(2) that refuses that bit on a non-directory to anyone but root
/// (`EFTYPE`, as the BSDs' does; macOS's is decided on CI) leaves it 0600. The 0700 base is what
/// keeps other users out either way.
fn restrict_socket(path: &Path) -> io::Result<()> {
    let set = |mode| std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode));
    match set(0o1600) {
        #[cfg(target_os = "macos")]
        Err(e) if e.raw_os_error() == Some(libc::EFTYPE) => set(0o600),
        other => other,
    }
}

/// Serve until the window ends, the session ends, the store is no longer locked with this
/// holder's id (its watch: at once, then every `STORE_LOOK`; T56, team decision 2026-10-13), a
/// signal or `End` (holder §3.4). The key page is zeroed as `Running` drops.
fn serve(mut r: Running) -> i32 {
    loop {
        let left = match r.window.check(&SystemClock) {
            WindowState::Open { left } => left,
            WindowState::Expired | WindowState::Slept => return 0,
        };
        if !r.watch.holds(&SystemClock) {
            return 0;
        }
        let nap = left.min(MAX_NAP).min(r.watch.until_next(&SystemClock));
        match r.waiter.wait(nap) {
            Ok(Wake::Listener) => {
                if r.accept_one() {
                    return 0;
                }
            }
            Ok(Wake::Timeout) => {}
            Ok(Wake::Leader | Wake::Signal) | Err(_) => return 0,
        }
    }
}

impl Running {
    /// One connection, one request. True when the holder ends.
    fn accept_one(&mut self) -> bool {
        let Ok((stream, _)) = self.listener.accept() else {
            return false;
        };
        let _ = stream.set_read_timeout(Some(IO_TIMEOUT));
        let _ = stream.set_write_timeout(Some(IO_TIMEOUT));
        let Ok(peer) = current::peer_cred(&stream) else {
            return false;
        };
        let send = |s: &UnixStream, reply: &Reply| {
            let _ = write_reply(&mut &*s, reply);
        };
        // §3.10's order — accept, peer check, window, one request: the peer is judged before a
        // byte of its request is read, so a process of another session cannot hold this
        // one-at-a-time loop for the 2 s read (critic-d4a-e finding 21).
        match admit(&self.binding, peer, &OsProcs) {
            Admission::EndNow => return true,
            Admission::Refuse(r) => {
                send(&stream, &Reply::Refused(r));
                return false;
            }
            Admission::Serve => {}
        }
        let req = match read_request(&mut &stream) {
            Ok(req) => req,
            Err(FrameError::Version(_)) => {
                send(
                    &stream,
                    &Reply::Refused(Refusal::Unsupported {
                        holder_protocol: PROTOCOL,
                    }),
                );
                return false;
            }
            Err(_) => {
                send(&stream, &Reply::Refused(Refusal::Malformed));
                return false;
            }
        };
        let held = Held {
            key: self.key.bytes(),
            init: &self.init,
            hardening: self.hardening,
        };
        let served = handle(req, &held, &mut self.window, &SystemClock);
        if served.end {
            // Off the endpoint before the reply: a command that runs once its client has read
            // `Ended` or `window_ended` finds no holder here, never one that is about to exit.
            let _ = std::fs::remove_file(&self.socket);
        }
        send(&stream, &served.reply);
        served.end
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::holder::protocol::StoreBinding;

    /// The steps, recorded instead of run.
    struct Recorded(Vec<&'static str>);

    impl StartSteps for Recorded {
        fn record_session(&mut self) -> (u32, i32) {
            self.0.push("record_session");
            (1000, 77)
        }
        fn detach(&mut self) -> io::Result<()> {
            self.0.push("detach");
            Ok(())
        }
        fn harden(&mut self) -> HolderHardening {
            self.0.push("harden");
            HolderHardening {
                non_dumpable: Some(true),
                deny_attach: None,
                core_off: true,
                memory_locked: false,
                process_dacl: None,
            }
        }
        fn close_inherited(&mut self) {
            self.0.push("close_inherited");
        }
        fn lock_page(&mut self) -> io::Result<LockedKey> {
            self.0.push("lock_page");
            LockedKey::new()
        }
        fn read_init(&mut self, key: &mut [u8; 32]) -> Result<InitHeader, FrameError> {
            self.0.push("read_init");
            key.copy_from_slice(&[3; 32]);
            Ok(InitHeader {
                store: StoreBinding {
                    id: "0123456789abcdef".into(),
                    root: "/s".into(),
                },
                base: "/r".into(),
                vault_id: apprafter_core::vault::VaultId([0; 16]),
                minutes: 5,
                window_secs: None,
                poll_leader: false,
                client_pid: 1,
            })
        }
    }

    /// T54: the session is recorded before the holder leaves it; it hardens before it locks a
    /// page and reads the key only into that page, after every inherited descriptor is closed
    /// (`PR_SET_DUMPABLE` is reset by every exec: holder finding 6).
    #[test]
    fn a_holder_hardens_and_locks_its_page_before_it_reads_the_key() {
        let mut steps = Recorded(Vec::new());
        let Ok(p) = prepare(&mut steps) else {
            panic!("prepare")
        };
        assert_eq!(
            steps.0,
            [
                "record_session",
                "detach",
                "harden",
                "close_inherited",
                "lock_page",
                "read_init"
            ]
        );
        assert_eq!((p.uid, p.sid, p.key.bytes()), (1000, 77, &[3; 32]));
        assert_eq!(
            p.hardening.memory_locked,
            p.key.locked(),
            "the page's own report"
        );
    }
}
