// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! A client of this terminal's holder (D.4a §3.10, T53, T59). One request per connection: a
//! client is spent after its request. 2 s per step — the connect, each read, each write — and
//! 5 s in all; past them the holder "does not answer" and the command goes on without it (T59).
//! The server's uid must be this process's (a shared base under `/tmp` could hold another
//! user's socket).

use std::io;
#[cfg(unix)]
use std::io::{Read, Write};
use std::time::Duration;

use apprafter_core::Zeroizing;
use cli_core::secrets::CachedSecret;

#[cfg(unix)]
use super::endpoint::Endpoint;
use super::protocol::{
    read_reply, write_request, FrameError, HolderItem, HolderStatus, HolderUnavailable, Refusal,
    Reply, Request, StoreBinding,
};
use super::window::{HolderClock, SystemClock};

#[cfg(unix)]
use std::os::unix::net::UnixStream;

/// Per step: the connect, a read, a write.
pub const IO_TIMEOUT: Duration = Duration::from_secs(2);
/// For the whole request, the connect included.
pub const OVERALL: Duration = Duration::from_secs(5);

#[derive(Debug)]
pub enum HolderError {
    Refused(Refusal),
    /// Timed out, or the connection broke.
    Io(io::Error),
    Protocol(FrameError),
    /// This client already made its one request.
    Spent,
    /// A reply of another kind than the request's.
    Unexpected,
}

pub struct HolderClient {
    #[cfg(unix)]
    conn: UnixStream,
    #[cfg(windows)]
    pipe: super::os::windows::Handle,
    server_pid: Option<u32>,
    #[cfg(unix)]
    server_started: Option<apprafter_core::process::ProcStart>,
    spent: bool,
    deadline: Duration,
}

/// A connection read and written under the request's deadline.
#[cfg(unix)]
struct Timed<'a> {
    conn: &'a UnixStream,
    deadline: Duration,
}

#[cfg(unix)]
impl Timed<'_> {
    fn left(&self) -> io::Result<Duration> {
        let left = self.deadline.saturating_sub(SystemClock.with_sleep());
        if left.is_zero() {
            return Err(io::ErrorKind::TimedOut.into());
        }
        Ok(left.min(IO_TIMEOUT))
    }
}

#[cfg(unix)]
impl Read for Timed<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.conn.set_read_timeout(Some(self.left()?))?;
        (&*self.conn).read(buf)
    }
}

#[cfg(unix)]
impl Write for Timed<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.conn.set_write_timeout(Some(self.left()?))?;
        (&*self.conn).write(buf)
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn from_frame(e: FrameError) -> HolderError {
    match e {
        FrameError::Io(e) => HolderError::Io(e),
        other => HolderError::Protocol(other),
    }
}

impl HolderClient {
    /// This terminal's holder of `store`, under the context's base (T49, T50): `Ok(None)` when no
    /// holder serves here (no socket, a dead one's socket, another user's); `Err` when no window
    /// is possible here at all.
    #[cfg(unix)]
    pub fn find(
        ctx: &apprafter_core::Context,
        store: &StoreBinding,
    ) -> Result<Option<HolderClient>, HolderUnavailable> {
        let endpoint = super::endpoint::endpoint_for(ctx, store)?;
        Ok(HolderClient::at(&endpoint))
    }

    /// A client of the holder at `endpoint`, if one listens there and runs as this user.
    #[cfg(unix)]
    pub fn at(endpoint: &Endpoint) -> Option<HolderClient> {
        let Endpoint::Socket { path, .. } = endpoint else {
            return None;
        };
        let deadline = SystemClock.with_sleep() + OVERALL;
        // ENOENT or ECONNREFUSED: no holder (holder §2.4). A connect past its step — a holder
        // that hung with its queue full — is none either: the command goes on without it (T59).
        let conn = super::os::connect_within(path, IO_TIMEOUT).ok()?;
        let server = super::os::current::peer_cred(&conn).ok()?;
        // SAFETY: geteuid has no failure mode.
        if server.uid != unsafe { libc::geteuid() } {
            return None;
        }
        use super::binding::ProcLookup;
        Some(HolderClient {
            server_pid: u32::try_from(server.pid).ok().filter(|p| *p > 0),
            server_started: super::os::OsProcs.started(server.pid),
            conn,
            spent: false,
            deadline,
        })
    }

    /// The holder's pid, as the connection reports it.
    pub fn server_pid(&self) -> Option<u32> {
        self.server_pid
    }

    /// Tests (T59, S9): whether the holder let go of this connection within `limit` — the
    /// fact [`HolderClient::terminate_server`] checks once, waited for, so a test reaches that
    /// check after the holder closed its end and never in the instant before (a loop of 10 ms
    /// naps: HG1 keeps `Instant` out of `holder/`).
    #[cfg(unix)]
    #[doc(hidden)]
    pub fn closed_within(&self, limit: Duration) -> bool {
        for _ in 0..=limit.as_millis() / 10 {
            if super::os::hung_up(&self.conn).unwrap_or(false) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        false
    }

    fn ask(&mut self, req: &Request) -> Result<Reply, HolderError> {
        if std::mem::replace(&mut self.spent, true) {
            return Err(HolderError::Spent);
        }
        #[cfg(unix)]
        let mut io = Timed {
            conn: &self.conn,
            deadline: self.deadline,
        };
        #[cfg(windows)]
        let mut io = super::os::windows::PipeIo::new(self.pipe.0, IO_TIMEOUT, Some(self.deadline))
            .map_err(HolderError::Io)?;
        // A holder that refused before reading the whole request has still sent its reply.
        let _ = write_request(&mut io, req);
        read_reply(&mut io).map_err(from_frame)
    }

    pub fn status(&mut self) -> Result<HolderStatus, HolderError> {
        match self.ask(&Request::Status)? {
            Reply::Status(s) => Ok(s),
            Reply::Refused(r) => Err(HolderError::Refused(r)),
            _ => Err(HolderError::Unexpected),
        }
    }

    /// The item, `None` when the authenticated vault holds none.
    pub fn open_item(
        &mut self,
        vault: &[u8],
        item: &HolderItem,
    ) -> Result<Option<Zeroizing<Vec<u8>>>, HolderError> {
        let req = Request::OpenItem {
            vault: vault.to_vec(),
            item: item.clone(),
        };
        match self.ask(&req)? {
            Reply::Item(v) => Ok(Some(v)),
            Reply::Absent => Ok(None),
            Reply::Refused(r) => Err(HolderError::Refused(r)),
            _ => Err(HolderError::Unexpected),
        }
    }

    pub fn open_cache(
        &mut self,
        vault: &[u8],
        secret: CachedSecret,
        target: &str,
        armored: &str,
    ) -> Result<Zeroizing<Vec<u8>>, HolderError> {
        let req = Request::OpenCache {
            vault: vault.to_vec(),
            secret,
            target: target.into(),
            armored: armored.as_bytes().to_vec(),
        };
        match self.ask(&req)? {
            Reply::Item(v) => Ok(v),
            Reply::Refused(r) => Err(HolderError::Refused(r)),
            _ => Err(HolderError::Unexpected),
        }
    }

    /// The store's recipient (public), for a new cache.
    pub fn recipient(&mut self, vault: &[u8]) -> Result<String, HolderError> {
        match self.ask(&Request::Recipient {
            vault: vault.to_vec(),
        })? {
            Reply::Recipient(r) => Ok(r),
            Reply::Refused(r) => Err(HolderError::Refused(r)),
            _ => Err(HolderError::Unexpected),
        }
    }

    /// `End`: the holder zeroes its key and exits.
    pub fn end(&mut self) -> Result<(), HolderError> {
        match self.ask(&Request::End)? {
            Reply::Ended => Ok(()),
            Reply::Refused(r) => Err(HolderError::Refused(r)),
            _ => Err(HolderError::Unexpected),
        }
    }

    /// `lock now` on a holder that does not answer (T59, S9): end the process this connection's
    /// server is, and no other.
    #[cfg(unix)]
    pub fn terminate_server(self) -> Result<(), HolderError> {
        let pid = self
            .server_pid
            .and_then(|p| i32::try_from(p).ok())
            .ok_or(HolderError::Unexpected)?;
        super::os::current::kill_server(&self.conn, pid, self.server_started)
            .map_err(HolderError::Io)
    }

    #[cfg(windows)]
    pub fn find(
        _ctx: &apprafter_core::Context,
        store: &StoreBinding,
    ) -> Result<Option<HolderClient>, HolderUnavailable> {
        HolderClient::on_console(store)
    }

    /// The holder of `store` on this process's console (T51, T50): one pipe name per process the
    /// console lists, all tried within one request's 5 s (T59); `Err` when this process has no
    /// console (no window is possible here).
    #[cfg(windows)]
    pub fn on_console(store: &StoreBinding) -> Result<Option<HolderClient>, HolderUnavailable> {
        let user =
            super::os::windows::user_sid_string().map_err(|e| HolderUnavailable::NoConsole {
                code: e.raw_os_error().unwrap_or(0) as u32,
            })?;
        let id = super::endpoint::EndpointId::for_user(&user, store);
        let console = super::os::windows::console_processes();
        if console.is_empty() {
            return Err(HolderUnavailable::NoConsole { code: 0 });
        }
        let me = std::process::id();
        let deadline = SystemClock.with_sleep() + OVERALL;
        Ok(console.into_iter().filter(|p| *p != me).find_map(|pid| {
            HolderClient::pipe_by(&super::endpoint::pipe_name(&id, pid), pid, deadline)
        }))
    }

    /// A client of the pipe `name` (the holder `pid`'s), if it opens.
    #[cfg(windows)]
    pub fn at_pipe(name: &str, pid: u32) -> Option<HolderClient> {
        HolderClient::pipe_by(name, pid, SystemClock.with_sleep() + OVERALL)
    }

    /// [`HolderClient::at_pipe`] within a request's `deadline`, the open included (T59).
    #[cfg(windows)]
    fn pipe_by(name: &str, pid: u32, deadline: Duration) -> Option<HolderClient> {
        Some(HolderClient {
            pipe: open_pipe(name, deadline)?,
            server_pid: Some(pid),
            spent: false,
            deadline,
        })
    }

    /// `lock now` on a holder that does not answer (T59, S9): the process is opened first — a
    /// handle pins it — then the pipe's server is checked to be still that pid, then it ends.
    #[cfg(windows)]
    pub fn terminate_server(self) -> Result<(), HolderError> {
        let pid = self.server_pid.ok_or(HolderError::Unexpected)?;
        super::os::windows::kill_server(&self.pipe, pid).map_err(HolderError::Io)
    }
}

/// Open the holder's pipe at identification level (T53): a squatting server learns who
/// connected and can act as no one. A busy pipe is waited for once, within the step's 2 s and
/// the request's `deadline` (T59): its open is a step like a read or a write.
#[cfg(windows)]
fn open_pipe(name: &str, deadline: Duration) -> Option<super::os::windows::Handle> {
    use windows_sys::Win32::Foundation::{
        GetLastError, ERROR_PIPE_BUSY, GENERIC_READ, GENERIC_WRITE, INVALID_HANDLE_VALUE,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FILE_FLAG_OVERLAPPED, OPEN_EXISTING, SECURITY_IDENTIFICATION,
        SECURITY_SQOS_PRESENT,
    };
    use windows_sys::Win32::System::Pipes::WaitNamedPipeW;
    let wide: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
    for attempt in 0..2 {
        // SAFETY: a NUL-terminated name; the handle is owned below.
        let h = unsafe {
            CreateFileW(
                wide.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                0,
                std::ptr::null(),
                OPEN_EXISTING,
                FILE_FLAG_OVERLAPPED | SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION,
                std::ptr::null_mut(),
            )
        };
        if h != INVALID_HANDLE_VALUE {
            return Some(super::os::windows::Handle(h));
        }
        // SAFETY: the error of the call above.
        if attempt == 0 && unsafe { GetLastError() } == ERROR_PIPE_BUSY {
            let step = deadline
                .saturating_sub(SystemClock.with_sleep())
                .min(IO_TIMEOUT);
            if step.is_zero() {
                return None;
            }
            // 0 would be the pipe's own default wait: at least 1 ms.
            let ms = u32::try_from(step.as_millis()).unwrap_or(u32::MAX).max(1);
            // SAFETY: a NUL-terminated name; waits at most `ms`.
            unsafe { WaitNamedPipeW(wide.as_ptr(), ms) };
            continue;
        }
        return None;
    }
    None
}
