// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The holder's OS calls, one module per OS (`std` + `libc` + `windows-sys`; no `nix`,
//! `rustix` or `interprocess`, holder §7). `current` is the running OS's.

#[cfg(target_os = "linux")]
pub(crate) mod linux;
#[cfg(target_os = "macos")]
pub(crate) mod macos;
#[cfg(windows)]
pub(crate) mod windows;

#[cfg(target_os = "linux")]
pub(crate) use linux as current;
#[cfg(target_os = "macos")]
pub(crate) use macos as current;
#[cfg(windows)]
pub(crate) use windows as current;

/// A clock since boot, as a `Duration`.
#[cfg(unix)]
pub(crate) fn clock(id: libc::clockid_t) -> std::time::Duration {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: clock_gettime writes one timespec; the clocks used here always exist.
    unsafe { libc::clock_gettime(id, &mut ts) };
    std::time::Duration::new(ts.tv_sec as u64, ts.tv_nsec as u32)
}

#[cfg(unix)]
pub(crate) use unix_common::*;

/// What every Unix holder and client shares.
#[cfg(unix)]
mod unix_common {
    use std::io;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
    use std::os::unix::net::UnixStream;
    use std::path::Path;
    use std::time::Duration;

    use apprafter_core::process::ProcStart;

    use crate::holder::binding::ProcLookup;

    /// Why a holder's loop woke.
    pub(crate) enum Wake {
        Listener,
        Leader,
        Signal,
        Timeout,
    }

    /// The OS's process facts.
    pub(crate) struct OsProcs;

    impl ProcLookup for OsProcs {
        type Start = ProcStart;
        fn sid_of(&self, pid: i32) -> Option<i32> {
            // SAFETY: getsid answers for any pid, -1 for one that is gone.
            let sid = unsafe { libc::getsid(pid) };
            (sid > 0).then_some(sid)
        }
        /// `pid`'s start while it runs. A process that exited is gone though its parent has not
        /// reaped it yet: Linux keeps a zombie's `/proc/<pid>/stat`, its start included, until
        /// the reap, so its state is read first (a pid reused between the two reads shows
        /// another start); macOS's `proc_pidinfo` answers nothing for an exited process.
        fn started(&self, pid: i32) -> Option<ProcStart> {
            #[cfg(target_os = "linux")]
            if super::current::exited(pid) {
                return None;
            }
            apprafter_core::process::started(u32::try_from(pid).ok()?)
        }
    }

    /// The bound leader still runs and is still the one bound (T52): the polling look's test.
    pub(crate) fn leader_alive(sid: i32, started: ProcStart) -> bool {
        OsProcs.started(sid) == Some(started)
    }

    /// Close every descriptor above 2 (holder §1.2 step 3): a pipe an agent harness or a shell's
    /// `3>` left the CLI must not stay open for the window. Linux: `close_range` through
    /// `syscall` (no glibc symbol: the release binary still starts on a glibc without it), else
    /// — before Linux 5.9, or under a seccomp filter that refuses it — every descriptor
    /// `/proc/self/fd` lists (libc has no `getdtablesize` on Linux). macOS: every descriptor
    /// below the table size.
    pub(crate) fn close_inherited() {
        #[cfg(target_os = "linux")]
        {
            // SAFETY: closes descriptors this process opened none of.
            if unsafe { libc::syscall(libc::SYS_close_range, 3u32, u32::MAX, 0u32) } == 0 {
                return;
            }
            // Listed whole before any is closed: the listing's own descriptor is among them,
            // and already closed when its turn comes (EBADF).
            let listed: Option<Vec<RawFd>> = std::fs::read_dir("/proc/self/fd").ok().map(|dir| {
                dir.flatten()
                    .filter_map(|e| e.file_name().to_str()?.parse().ok())
                    .collect()
            });
            // No /proc: every descriptor below the soft limit (bounded: some containers allow
            // a billion).
            let open = listed.unwrap_or_else(|| {
                // SAFETY: sysconf has no preconditions.
                let max = unsafe { libc::sysconf(libc::_SC_OPEN_MAX) }.clamp(3, 1 << 20);
                (3..RawFd::try_from(max).unwrap_or(1 << 20)).collect()
            });
            for fd in open.into_iter().filter(|fd| *fd > 2) {
                // SAFETY: as above; closing a descriptor that is not open is harmless.
                unsafe { libc::close(fd) };
            }
        }
        #[cfg(target_os = "macos")]
        {
            // SAFETY: getdtablesize has no preconditions; closing an unused descriptor is harmless.
            let max = unsafe { libc::getdtablesize() }.max(3);
            for fd in 3..max {
                // SAFETY: as above.
                unsafe { libc::close(fd) };
            }
        }
    }

    /// Put `/dev/null` on stdin and stdout once the handshake is over (holder §1.2 step 8).
    pub(crate) fn null_stdio() {
        // SAFETY: open, dup2 and close on descriptors this process owns.
        unsafe {
            let null = libc::open(c"/dev/null".as_ptr(), libc::O_RDWR | libc::O_CLOEXEC);
            if null >= 0 {
                libc::dup2(null, 0);
                libc::dup2(null, 1);
                libc::close(null);
            }
        }
    }

    /// Connect to the socket at `path`, waiting at most `limit` on the window's clock (T59: the
    /// connect is a step like a read or a write). The socket is non-blocking until it is
    /// connected, so a listener whose queue is full — a holder that hung, then other connects
    /// reached it — costs at most `limit`, never a command blocked for good: Linux answers such
    /// a connect `EAGAIN`, tried again every 10 ms until `limit`; macOS turns it away
    /// (`ECONNREFUSED`, as when no one listens). An `EINPROGRESS`, which neither gives for a
    /// local socket, is waited for in `poll`. Past `limit`: `TimedOut`.
    pub(crate) fn connect_within(path: &Path, limit: Duration) -> io::Result<UnixStream> {
        let until = super::current::clock_with_sleep() + limit;
        let left = || {
            let left = until.saturating_sub(super::current::clock_with_sleep());
            if left.is_zero() {
                Err(io::Error::from(io::ErrorKind::TimedOut))
            } else {
                Ok(left)
            }
        };
        let fd = local_socket()?;
        let (addr, len) = socket_address(path)?;
        loop {
            // SAFETY: the socket above and an address of `len` bytes.
            if unsafe { libc::connect(fd.as_raw_fd(), (&raw const addr).cast(), len) } == 0 {
                break;
            }
            let e = io::Error::last_os_error();
            match e.raw_os_error() {
                Some(libc::EINTR) => {}
                Some(libc::EISCONN) => break,
                Some(libc::EAGAIN) => std::thread::sleep(left()?.min(Duration::from_millis(10))),
                Some(libc::EINPROGRESS | libc::EALREADY) => {
                    let ms = i32::try_from(left()?.as_millis())
                        .unwrap_or(i32::MAX)
                        .max(1);
                    let mut p = libc::pollfd {
                        fd: fd.as_raw_fd(),
                        events: libc::POLLOUT,
                        revents: 0,
                    };
                    // SAFETY: one pollfd.
                    match unsafe { libc::poll(&mut p, 1, ms) } {
                        0 => return Err(io::ErrorKind::TimedOut.into()),
                        n if n < 0 => {
                            let e = io::Error::last_os_error();
                            if e.kind() != io::ErrorKind::Interrupted {
                                return Err(e);
                            }
                        }
                        _ => {
                            let mut err: libc::c_int = 0;
                            let mut len = std::mem::size_of::<libc::c_int>() as libc::socklen_t;
                            // SAFETY: SO_ERROR writes one int.
                            let rc = unsafe {
                                libc::getsockopt(
                                    fd.as_raw_fd(),
                                    libc::SOL_SOCKET,
                                    libc::SO_ERROR,
                                    (&raw mut err).cast(),
                                    &mut len,
                                )
                            };
                            if rc != 0 {
                                return Err(io::Error::last_os_error());
                            }
                            if err != 0 {
                                return Err(io::Error::from_raw_os_error(err));
                            }
                            break;
                        }
                    }
                }
                _ => return Err(e),
            }
        }
        let conn = UnixStream::from(fd);
        conn.set_nonblocking(false)?;
        Ok(conn)
    }

    /// A local stream socket as `std` makes one — close-on-exec, and on macOS no `SIGPIPE` from
    /// a write to a closed peer (`SO_NOSIGPIPE`) — but non-blocking.
    fn local_socket() -> io::Result<OwnedFd> {
        #[cfg(target_os = "linux")]
        let kind = libc::SOCK_STREAM | libc::SOCK_CLOEXEC | libc::SOCK_NONBLOCK;
        #[cfg(not(target_os = "linux"))]
        let kind = libc::SOCK_STREAM;
        // SAFETY: socket(2) with plain values; the descriptor is owned below.
        let fd = unsafe { libc::socket(libc::AF_UNIX, kind, 0) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: a new descriptor this process owns.
        let fd = unsafe { OwnedFd::from_raw_fd(fd) };
        #[cfg(not(target_os = "linux"))]
        {
            let one: libc::c_int = 1;
            // SAFETY: fcntl and setsockopt on the descriptor above.
            let ok = unsafe {
                let flags = libc::fcntl(fd.as_raw_fd(), libc::F_GETFL);
                flags >= 0
                    && libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) == 0
                    && libc::fcntl(fd.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) == 0
                    && libc::setsockopt(
                        fd.as_raw_fd(),
                        libc::SOL_SOCKET,
                        libc::SO_NOSIGPIPE,
                        (&raw const one).cast(),
                        std::mem::size_of::<libc::c_int>() as libc::socklen_t,
                    ) == 0
            };
            if !ok {
                return Err(io::Error::last_os_error());
            }
        }
        Ok(fd)
    }

    /// `path` as a `sockaddr_un` and its length (`std`'s: the path and its NUL).
    fn socket_address(path: &Path) -> io::Result<(libc::sockaddr_un, libc::socklen_t)> {
        use std::os::unix::ffi::OsStrExt;
        // SAFETY: an all-zero sockaddr_un is valid; the path is copied in below.
        let mut addr: libc::sockaddr_un = unsafe { std::mem::zeroed() };
        addr.sun_family = libc::AF_UNIX as libc::sa_family_t;
        let bytes = path.as_os_str().as_bytes();
        if bytes.len() >= addr.sun_path.len() || bytes.contains(&0) {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        for (d, s) in addr.sun_path.iter_mut().zip(bytes) {
            *d = *s as libc::c_char;
        }
        let len = std::mem::offset_of!(libc::sockaddr_un, sun_path) + bytes.len() + 1;
        Ok((addr, len as libc::socklen_t))
    }

    /// The other end of `conn` is closed: its server has ended or let the connection go.
    pub(crate) fn hung_up(conn: &UnixStream) -> io::Result<bool> {
        let mut p = libc::pollfd {
            fd: conn.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: one pollfd, no wait.
        if unsafe { libc::poll(&mut p, 1, 0) } < 0 {
            return Err(io::Error::last_os_error());
        }
        if p.revents & (libc::POLLHUP | libc::POLLERR) != 0 {
            return Ok(true);
        }
        if p.revents & libc::POLLIN != 0 {
            let mut b = [0u8; 1];
            // SAFETY: a one-byte peek into `b`.
            let n = unsafe {
                libc::recv(
                    conn.as_raw_fd(),
                    b.as_mut_ptr().cast(),
                    1,
                    libc::MSG_PEEK | libc::MSG_DONTWAIT,
                )
            };
            return Ok(n == 0);
        }
        Ok(false)
    }

    /// SIGTERM and SIGHUP, delivered as a readable byte (`signal_hook`'s self-pipe).
    pub(crate) struct Signals {
        read: UnixStream,
    }

    impl Signals {
        pub(crate) fn new() -> io::Result<Signals> {
            let (read, write) = UnixStream::pair()?;
            read.set_nonblocking(true)?;
            let hup = write.try_clone()?;
            signal_hook::low_level::pipe::register(libc::SIGTERM, write)?;
            signal_hook::low_level::pipe::register(libc::SIGHUP, hup)?;
            Ok(Signals { read })
        }

        pub(crate) fn fd(&self) -> RawFd {
            self.read.as_raw_fd()
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::os::fd::AsRawFd;
    use std::os::unix::net::UnixStream;
    use std::process::{Command, Stdio};
    use std::time::Duration;

    use super::{current, Wake};

    /// T52, risk 6: with no pidfd (Linux) and no `EVFILT_PROC` (macOS) the holder looks at its
    /// session's leader every second, and a leader that exited is gone even while its parent
    /// has not reaped it. Linux keeps a zombie's `/proc/<pid>/stat`, its start included, until
    /// the reap: by its start alone it would read as alive, and the window would outlive its
    /// terminal for as long as the parent waits. macOS's `proc_pidinfo` answers nothing for a
    /// process that exited (XNU marks it dead before it is a zombie; only a call that asks for
    /// zombies finds it), so there the start alone already says gone.
    #[test]
    fn the_polling_look_counts_an_exited_unreaped_leader_gone() {
        let mut child = Command::new("/bin/sh")
            .args(["-c", "read _"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let pid = child.id();
        let start = apprafter_core::process::started(pid).expect("a running child's start");
        // The shell's `read` meets the end of its input: it exits.
        drop(child.stdin.take());
        // SAFETY: an all-zero siginfo_t is a valid value to be overwritten.
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        // SAFETY: waits for this test's own child to exit and leaves it unreaped (WNOWAIT).
        let waited = unsafe {
            libc::waitid(
                libc::P_PID,
                pid as libc::id_t,
                &mut info,
                libc::WEXITED | libc::WNOWAIT,
            )
        };
        assert_eq!(waited, 0, "{}", std::io::Error::last_os_error());
        let zombie_start = apprafter_core::process::started(pid);
        let (listener, _peer) = UnixStream::pair().unwrap();
        let (signal, _writer) = UnixStream::pair().unwrap();
        let mut waiter = current::Waiter::new(
            listener.as_raw_fd(),
            signal.as_raw_fd(),
            pid as i32,
            start,
            true,
        )
        .unwrap();
        let woke = waiter.wait(Duration::from_secs(3)).unwrap();
        child.wait().unwrap();
        #[cfg(target_os = "linux")]
        assert_eq!(
            zombie_start,
            Some(start),
            "a zombie keeps its /proc/<pid>/stat"
        );
        #[cfg(target_os = "macos")]
        assert_eq!(
            zombie_start, None,
            "proc_pidinfo answers nothing for an exited process"
        );
        assert!(
            matches!(woke, Wake::Leader),
            "the polling look took an exited, unreaped leader for alive"
        );
    }
}
