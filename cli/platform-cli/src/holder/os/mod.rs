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
    use std::os::fd::{AsRawFd, RawFd};
    use std::os::unix::net::UnixStream;

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
        fn started(&self, pid: i32) -> Option<ProcStart> {
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
