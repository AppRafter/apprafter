// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! A client of this terminal's holder (D.4a §3.10, T53, T59). One request per connection: a
//! client is spent after its request. 2 s per read or write and 5 s in all; past them the
//! holder "does not answer" and the command goes on without it (T59). The server's uid must be
//! this process's (a shared base under `/tmp` could hold another user's socket).

use std::io::{self, Read, Write};
use std::time::Duration;

use apprafter_core::Zeroizing;
use cli_core::secrets::CachedSecret;

use super::endpoint::Endpoint;
use super::protocol::{
    read_reply, write_request, FrameError, HolderItem, HolderStatus, HolderUnavailable, Refusal,
    Reply, Request, StoreBinding,
};
use super::window::{HolderClock, SystemClock};

#[cfg(unix)]
use std::os::unix::net::UnixStream;

/// Per read or write.
pub const IO_TIMEOUT: Duration = Duration::from_secs(2);
/// For the whole request.
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
        // ENOENT or ECONNREFUSED: no holder (holder §2.4).
        let conn = UnixStream::connect(path).ok()?;
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
            deadline: SystemClock.with_sleep() + OVERALL,
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
        let mut io = Timed {
            conn: &self.conn,
            deadline: self.deadline,
        };
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
}
