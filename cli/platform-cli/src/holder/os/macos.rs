// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! macOS calls of the holder (D.4a T52, T56).

use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::net::UnixStream;
use std::time::Duration;

use apprafter_core::process::ProcStart;

use super::{hung_up, leader_alive, OsProcs, Wake};
use crate::holder::binding::{PeerCred, ProcLookup};

/// Counts sleep (`clock_gettime(3)`: `mach_continuous_time`); `Instant` reads
/// `CLOCK_UPTIME_RAW`, which does not (holder finding 5). Raw, as [`AWAKE`] is: the sleep
/// detector compares the two (O4), and `CLOCK_MONOTONIC` is slewed by time adjustments, so its
/// readings drift from a raw clock's and the two are not even ordered (CI, 2026-10-13: 0.57 s
/// behind `CLOCK_UPTIME_RAW` after ten minutes of uptime).
pub(crate) const WITH_SLEEP: libc::clockid_t = libc::CLOCK_MONOTONIC_RAW;
/// Stops in sleep (`mach_absolute_time`): the sleep detector's second clock (O4).
pub(crate) const AWAKE: libc::clockid_t = libc::CLOCK_UPTIME_RAW;

pub(crate) fn clock_with_sleep() -> Duration {
    super::clock(WITH_SLEEP)
}

pub(crate) fn clock_awake() -> Duration {
    super::clock(AWAKE)
}

/// The peer's uid (`getpeereid`) and pid (`LOCAL_PEERPID`).
pub(crate) fn peer_cred(s: &UnixStream) -> io::Result<PeerCred> {
    let fd = s.as_raw_fd();
    let (mut uid, mut gid) = (0, 0);
    // SAFETY: writes two ids.
    if unsafe { libc::getpeereid(fd, &mut uid, &mut gid) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let mut pid: libc::pid_t = 0;
    let mut len = std::mem::size_of::<libc::pid_t>() as libc::socklen_t;
    // SAFETY: LOCAL_PEERPID writes one pid_t.
    let rc = unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_LOCAL,
            libc::LOCAL_PEERPID,
            (&raw mut pid).cast(),
            &mut len,
        )
    };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(PeerCred { uid, pid })
}

fn kev(ident: usize, filter: i16, fflags: u32) -> libc::kevent {
    libc::kevent {
        ident,
        filter,
        flags: libc::EV_ADD,
        fflags,
        data: 0,
        udata: std::ptr::null_mut(),
    }
}

pub(crate) struct Waiter {
    kq: OwnedFd,
    listener: RawFd,
    signal: RawFd,
    sid: i32,
    started: ProcStart,
    /// `EVFILT_PROC` refused, or a test forced it: a look every second.
    poll: bool,
}

impl Waiter {
    pub(crate) fn new(
        listener: RawFd,
        signal: RawFd,
        sid: i32,
        started: ProcStart,
        force_poll: bool,
    ) -> io::Result<Waiter> {
        // SAFETY: kqueue has no arguments; the descriptor is owned below.
        let kq = unsafe { libc::kqueue() };
        if kq < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: a new descriptor this process owns.
        let kq = unsafe { OwnedFd::from_raw_fd(kq) };
        let register = |changes: &[libc::kevent]| {
            // SAFETY: a changelist of `changes.len()` events, no eventlist.
            unsafe {
                libc::kevent(
                    kq.as_raw_fd(),
                    changes.as_ptr(),
                    changes.len() as i32,
                    std::ptr::null_mut(),
                    0,
                    std::ptr::null(),
                )
            }
        };
        let reads = [
            kev(listener as usize, libc::EVFILT_READ, 0),
            kev(signal as usize, libc::EVFILT_READ, 0),
        ];
        if register(&reads) < 0 {
            return Err(io::Error::last_os_error());
        }
        // NOTE_EXIT alone needs no permission (XNU filt_procattach): only `| NOTE_EXITSTATUS` does.
        let poll =
            force_poll || register(&[kev(sid as usize, libc::EVFILT_PROC, libc::NOTE_EXIT)]) < 0;
        Ok(Waiter {
            kq,
            listener,
            signal,
            sid,
            started,
            poll,
        })
    }

    pub(crate) fn wait(&mut self, timeout: Duration) -> io::Result<Wake> {
        let nap = if self.poll {
            timeout.min(Duration::from_secs(1))
        } else {
            timeout
        };
        let ts = libc::timespec {
            tv_sec: nap.as_secs() as libc::time_t,
            tv_nsec: libc::c_long::from(nap.subsec_nanos()),
        };
        // SAFETY: an all-zero kevent is a valid value to be overwritten.
        let mut out: [libc::kevent; 4] = unsafe { std::mem::zeroed() };
        // SAFETY: an eventlist of 4.
        let n = unsafe {
            libc::kevent(
                self.kq.as_raw_fd(),
                std::ptr::null(),
                0,
                out.as_mut_ptr(),
                out.len() as i32,
                &ts,
            )
        };
        if n < 0 {
            let e = io::Error::last_os_error();
            return if e.kind() == io::ErrorKind::Interrupted {
                Ok(Wake::Timeout)
            } else {
                Err(e)
            };
        }
        let got = &out[..n as usize];
        let read_on = |fd: RawFd| {
            got.iter()
                .any(|e| e.filter == libc::EVFILT_READ && e.ident == fd as usize)
        };
        if read_on(self.signal) {
            return Ok(Wake::Signal);
        }
        if got.iter().any(|e| e.filter == libc::EVFILT_PROC) {
            return Ok(Wake::Leader);
        }
        if self.poll && !leader_alive(self.sid, self.started) {
            return Ok(Wake::Leader);
        }
        if read_on(self.listener) {
            return Ok(Wake::Listener);
        }
        Ok(Wake::Timeout)
    }
}

/// `lock now` on a hung holder (T59, S9): the server's start, recorded when the client
/// connected, read again just before the signal, with the connection still held by it.
pub(crate) fn kill_server(
    conn: &UnixStream,
    pid: i32,
    started: Option<ProcStart>,
) -> io::Result<()> {
    let gone = || {
        io::Error::new(
            io::ErrorKind::NotFound,
            "the holder already let go of this connection",
        )
    };
    let started = started.ok_or_else(gone)?;
    if hung_up(conn)? || OsProcs.started(pid) != Some(started) {
        return Err(gone());
    }
    // SAFETY: SIGKILL to the pid just checked.
    if unsafe { libc::kill(pid, libc::SIGKILL) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// A process's image path (`proc_pidpath`; Task 6 P5: it reads a `PT_DENY_ATTACH` process).
pub(crate) fn image_path(pid: i32) -> io::Result<std::path::PathBuf> {
    let mut buf = vec![0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
    // SAFETY: writes at most `buf.len()` bytes.
    let n = unsafe { libc::proc_pidpath(pid, buf.as_mut_ptr().cast(), buf.len() as u32) };
    if n <= 0 {
        return Err(io::Error::last_os_error());
    }
    buf.truncate(n as usize);
    use std::os::unix::ffi::OsStringExt;
    Ok(std::path::PathBuf::from(std::ffi::OsString::from_vec(buf)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_window_clocks_are_the_ones_that_count_suspend() {
        assert_eq!(
            (WITH_SLEEP, AWAKE),
            (libc::CLOCK_MONOTONIC_RAW, libc::CLOCK_UPTIME_RAW)
        );
    }
}
