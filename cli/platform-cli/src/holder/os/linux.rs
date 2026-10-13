// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Linux calls of the holder (D.4a T52, T56).

use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::net::UnixStream;
use std::time::Duration;

use apprafter_core::process::ProcStart;

use super::{hung_up, leader_alive, Wake};
use crate::holder::binding::PeerCred;

/// Counts suspend: the window's clock. `Instant` reads `CLOCK_MONOTONIC`, which does not
/// (holder finding 5), so a lid closed for the night would leave a window its minutes.
pub(crate) const WITH_SLEEP: libc::clockid_t = libc::CLOCK_BOOTTIME;
/// Stops in suspend: the sleep detector's second clock (O4).
pub(crate) const AWAKE: libc::clockid_t = libc::CLOCK_MONOTONIC;

pub(crate) fn clock_with_sleep() -> Duration {
    super::clock(WITH_SLEEP)
}

pub(crate) fn clock_awake() -> Duration {
    super::clock(AWAKE)
}

/// The peer's uid and pid (`SO_PEERCRED`): on an accepted socket the client's, on a client's
/// socket the server's at `listen` time (unix(7)).
pub(crate) fn peer_cred(s: &UnixStream) -> io::Result<PeerCred> {
    let mut cred = libc::ucred {
        pid: 0,
        uid: 0,
        gid: 0,
    };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: SO_PEERCRED writes one ucred.
    let rc = unsafe {
        libc::getsockopt(
            s.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&raw mut cred).cast(),
            &mut len,
        )
    };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(PeerCred {
        uid: cred.uid,
        pid: cred.pid,
    })
}

fn pidfd_open(pid: i32) -> io::Result<OwnedFd> {
    // SAFETY: pidfd_open(2) (Linux 5.3) through syscall: no glibc symbol needed.
    let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: a new descriptor this process owns.
    Ok(unsafe { OwnedFd::from_raw_fd(fd as RawFd) })
}

enum Leader {
    /// Readable when the leader exits (T52).
    Pidfd(OwnedFd),
    /// `pidfd_open` refused (`ENOSYS` before 5.3, `EPERM` under a seccomp filter: risk 6), or a
    /// test forced it: a look every second, with the start-time check (sudo's rule).
    Poll { sid: i32, started: ProcStart },
}

pub(crate) struct Waiter {
    listener: RawFd,
    signal: RawFd,
    leader: Leader,
}

impl Waiter {
    pub(crate) fn new(
        listener: RawFd,
        signal: RawFd,
        sid: i32,
        started: ProcStart,
        force_poll: bool,
    ) -> io::Result<Waiter> {
        let leader = match (!force_poll).then(|| pidfd_open(sid)) {
            Some(Ok(fd)) => Leader::Pidfd(fd),
            _ => Leader::Poll { sid, started },
        };
        Ok(Waiter {
            listener,
            signal,
            leader,
        })
    }

    pub(crate) fn wait(&mut self, timeout: Duration) -> io::Result<Wake> {
        let fd = |fd: RawFd| libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        let mut fds = vec![fd(self.listener), fd(self.signal)];
        let nap = match &self.leader {
            Leader::Pidfd(p) => {
                fds.push(fd(p.as_raw_fd()));
                timeout
            }
            Leader::Poll { .. } => timeout.min(Duration::from_secs(1)),
        };
        let ms = i32::try_from(nap.as_millis()).unwrap_or(i32::MAX);
        // SAFETY: `fds` holds `fds.len()` pollfds.
        let n = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, ms) };
        if n < 0 {
            let e = io::Error::last_os_error();
            return if e.kind() == io::ErrorKind::Interrupted {
                Ok(Wake::Timeout)
            } else {
                Err(e)
            };
        }
        if fds[1].revents != 0 {
            return Ok(Wake::Signal);
        }
        if fds.get(2).is_some_and(|f| f.revents != 0) {
            return Ok(Wake::Leader);
        }
        if let Leader::Poll { sid, started } = &self.leader {
            if !leader_alive(*sid, *started) {
                return Ok(Wake::Leader);
            }
        }
        if fds[0].revents & libc::POLLIN != 0 {
            return Ok(Wake::Listener);
        }
        Ok(Wake::Timeout)
    }
}

/// `lock now` on a hung holder (T59, S9): a pidfd for the pid the connection names, opened while
/// the connection still proves its server alive — the server holds the other end, so the pid
/// cannot have been reused between the two — then SIGKILL through that pidfd.
pub(crate) fn kill_server(
    conn: &UnixStream,
    pid: i32,
    _started: Option<ProcStart>,
) -> io::Result<()> {
    let pidfd = pidfd_open(pid)?;
    if hung_up(conn)? {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "the holder already let go of this connection",
        ));
    }
    // SAFETY: pidfd_send_signal(2) (Linux 5.1) on the pidfd above; no siginfo.
    let rc = unsafe {
        libc::syscall(
            libc::SYS_pidfd_send_signal,
            pidfd.as_raw_fd(),
            libc::SIGKILL,
            std::ptr::null::<libc::siginfo_t>(),
            0u32,
        )
    };
    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_window_clocks_are_the_ones_that_count_suspend() {
        assert_eq!(
            (WITH_SLEEP, AWAKE),
            (libc::CLOCK_BOOTTIME, libc::CLOCK_MONOTONIC)
        );
    }
}
