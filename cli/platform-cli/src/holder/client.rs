// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! A client of this terminal's holder (D.4a §3.10, T53, T59). One request per connection: a
//! client is spent after its request. 2 s per step — the connect, each read, each write — and
//! 5 s in all; past them the holder "does not answer" and the command goes on without it (T59).
//! The server's uid must be this process's (a shared base under `/tmp` could hold another
//! user's socket). The server must run as this user, and on macOS and Windows be this very
//! binary (T53); Linux cannot check that (the holder is non-dumpable), and T48 states what a
//! squatter's replies can do. A holder that lives at the endpoint and takes no connection is
//! told apart from none ([`Reached`]): a command then asks as if no window were open, while
//! `lock status` and `lock now` say it does not answer (rule 1).

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

/// What looking for this terminal's holder found (T59). A holder that lives here and takes no
/// connection is not "no holder": `lock status` says it does not answer, and `lock now` that it
/// could not end it, never that no window is open (rule 1).
#[derive(Debug)]
pub enum Reached<T> {
    /// A holder took the connection.
    Holder(T),
    /// No holder serves here: no endpoint, a dead holder's, another user's, not this binary.
    Nobody,
    /// A holder lives here and takes no connection — Unix: its listen queue is full (Linux makes
    /// the connect wait past its step; macOS turns it away at once, while the holder still holds
    /// its lock); Windows: its one pipe instance stays busy past the wait. `pid` as its lock file
    /// or its pipe's name gives it.
    NotResponding { pid: Option<u32> },
}

impl<T> Reached<T> {
    /// The holder, when one took the connection: a reading command asks either way.
    pub fn holder(self) -> Option<T> {
        match self {
            Reached::Holder(t) => Some(t),
            _ => None,
        }
    }

    fn map<U>(self, f: impl FnOnce(T) -> U) -> Reached<U> {
        match self {
            Reached::Holder(t) => Reached::Holder(f(t)),
            Reached::Nobody => Reached::Nobody,
            Reached::NotResponding { pid } => Reached::NotResponding { pid },
        }
    }
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

    /// Bound the next read or write by what is left of its step and of the request (`set`:
    /// `set_read_timeout` or `set_write_timeout`). macOS refuses every socket option once the
    /// socket is shut down both ways (`EINVAL`: XNU's `sosetoptlock`), which a holder that has
    /// answered and closed leaves it in — before this client read the reply, or even sent its
    /// request. Nothing blocks on such a socket, and the step's bound set before the connect
    /// (`os::connect_within`) stays on it: the call goes on and reads what is there.
    fn bound(&self, set: fn(&UnixStream, Option<Duration>) -> io::Result<()>) -> io::Result<()> {
        match set(self.conn, Some(self.left()?)) {
            Err(e) if e.kind() != io::ErrorKind::InvalidInput => Err(e),
            _ => Ok(()),
        }
    }
}

#[cfg(unix)]
impl Read for Timed<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.bound(UnixStream::set_read_timeout)?;
        (&*self.conn).read(buf)
    }
}

#[cfg(unix)]
impl Write for Timed<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.bound(UnixStream::set_write_timeout)?;
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

    /// A client of the holder at `endpoint`, if one listens there as this user — and, on macOS,
    /// runs this very binary (T53).
    #[cfg(unix)]
    pub fn at(endpoint: &Endpoint) -> Option<HolderClient> {
        HolderClient::reach(endpoint).holder()
    }

    /// [`HolderClient::at`], a holder that lives at `endpoint` and takes no connection told
    /// apart from none (T59).
    #[cfg(unix)]
    pub fn reach(endpoint: &Endpoint) -> Reached<HolderClient> {
        match std::env::current_exe() {
            Ok(me) => HolderClient::connect(endpoint, &me, same_file),
            Err(_) => Reached::Nobody,
        }
    }

    /// [`HolderClient::at`] with the holder's expected image named (tests: their client is a test
    /// binary). Linux checks no image: the holder is non-dumpable, so `/proc/<pid>/exe` is closed
    /// to the same user (`linux_the_holders_image_is_unreadable`).
    #[cfg(unix)]
    pub fn at_expecting(endpoint: &Endpoint, image: &std::path::Path) -> Option<HolderClient> {
        HolderClient::connect(endpoint, image, same_file).holder()
    }

    /// The Unix connect, the server's image judged by `accept` against `image` (macOS).
    #[cfg(unix)]
    fn connect(
        endpoint: &Endpoint,
        image: &std::path::Path,
        accept: fn(&std::path::Path, &std::path::Path) -> bool,
    ) -> Reached<HolderClient> {
        let Endpoint::Socket { path, lock } = endpoint else {
            return Reached::Nobody;
        };
        let deadline = SystemClock.with_sleep() + OVERALL;
        // ENOENT or ECONNREFUSED: no holder (holder §2.4), unless one still holds the lock. A
        // connect past its step — a holder that hung with its queue full — is a holder that does
        // not answer: the command goes on without it (T59), `lock status` names it.
        let conn = match super::os::connect_within(path, IO_TIMEOUT) {
            Ok(conn) => conn,
            Err(e) => return missed(&e, lock),
        };
        let Ok(server) = super::os::current::peer_cred(&conn) else {
            return Reached::Nobody;
        };
        // SAFETY: geteuid has no failure mode.
        if server.uid != unsafe { libc::geteuid() } {
            return Reached::Nobody;
        }
        #[cfg(target_os = "macos")]
        if !super::os::current::image_path(server.pid).is_ok_and(|p| accept(&p, image)) {
            return Reached::Nobody;
        }
        #[cfg(not(target_os = "macos"))]
        let _ = (image, accept);
        use super::binding::ProcLookup;
        Reached::Holder(HolderClient {
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
    /// console lists, all tried within one request's 5 s (T59), and only a server that is this
    /// very binary (T53); `Err` when this process has no console (no window is possible here).
    #[cfg(windows)]
    pub fn on_console(store: &StoreBinding) -> Result<Option<HolderClient>, HolderUnavailable> {
        HolderClient::reach_console(store).map(Reached::holder)
    }

    /// [`HolderClient::on_console`], a holder whose pipe stays busy told apart from none (T59).
    #[cfg(windows)]
    pub fn reach_console(store: &StoreBinding) -> Result<Reached<HolderClient>, HolderUnavailable> {
        let me = std::env::current_exe().map_err(|_| HolderUnavailable::NoConsole { code: 0 })?;
        HolderClient::reach_console_expecting(store, &me)
    }

    /// [`HolderClient::on_console`] with the holder's expected image named (tests). A pipe whose
    /// server is not the pid its name carries, or whose image is not this binary, is skipped (T53).
    #[cfg(windows)]
    pub fn on_console_expecting(
        store: &StoreBinding,
        image: &std::path::Path,
    ) -> Result<Option<HolderClient>, HolderUnavailable> {
        HolderClient::reach_console_expecting(store, image).map(Reached::holder)
    }

    /// [`HolderClient::reach_console`] with the holder's expected image named (tests).
    #[cfg(windows)]
    pub fn reach_console_expecting(
        store: &StoreBinding,
        image: &std::path::Path,
    ) -> Result<Reached<HolderClient>, HolderUnavailable> {
        HolderClient::console_with(store, image, same_file)
    }

    /// The Windows search of this console's holders, each server's image judged by `accept`. A
    /// pipe of this store's name that stays busy past its wait is a holder that does not answer,
    /// unless another pipe's holder answers (T59).
    #[cfg(windows)]
    fn console_with(
        store: &StoreBinding,
        image: &std::path::Path,
        accept: fn(&std::path::Path, &std::path::Path) -> bool,
    ) -> Result<Reached<HolderClient>, HolderUnavailable> {
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
        let mut busy = None;
        for pid in console.into_iter().filter(|p| *p != me) {
            match open_pipe(&super::endpoint::pipe_name(&id, pid), deadline) {
                PipeOpen::Open(pipe) => {
                    let ours = super::os::windows::server_pid(&pipe) == Some(pid)
                        && super::os::windows::image_path(pid).is_ok_and(|p| accept(&p, image));
                    if ours {
                        return Ok(Reached::Holder(HolderClient::of_pipe(pipe, pid, deadline)));
                    }
                }
                PipeOpen::Busy => busy = busy.or(Some(pid)),
                PipeOpen::Absent => {}
            }
        }
        Ok(match busy {
            Some(pid) => Reached::NotResponding { pid: Some(pid) },
            None => Reached::Nobody,
        })
    }

    /// A client of the pipe `name` (the holder `pid`'s), if it opens.
    #[cfg(windows)]
    pub fn at_pipe(name: &str, pid: u32) -> Option<HolderClient> {
        let deadline = SystemClock.with_sleep() + OVERALL;
        match open_pipe(name, deadline) {
            PipeOpen::Open(pipe) => Some(HolderClient::of_pipe(pipe, pid, deadline)),
            PipeOpen::Busy | PipeOpen::Absent => None,
        }
    }

    /// A client of the holder `pid` over `pipe`, within a request's `deadline` (T59).
    #[cfg(windows)]
    fn of_pipe(pipe: super::os::windows::Handle, pid: u32, deadline: Duration) -> HolderClient {
        HolderClient {
            pipe,
            server_pid: Some(pid),
            spent: false,
            deadline,
        }
    }

    /// `lock now` on a holder that does not answer (T59, S9): the process is opened first — a
    /// handle pins it — then the pipe's server is checked to be still that pid, then it ends.
    #[cfg(windows)]
    pub fn terminate_server(self) -> Result<(), HolderError> {
        let pid = self.server_pid.ok_or(HolderError::Unexpected)?;
        super::os::windows::kill_server(&self.pipe, pid).map_err(HolderError::Io)
    }
}

/// A client that may only end a holder (`lock now`, `unlock`'s replacement, `lock off`; T56,
/// T58): what [`HolderClient::at`] finds, and also — on macOS and Windows — a holder whose image
/// has this binary's file name at another path, an older install an upgrade moved, which `at`
/// refuses so that no command reads through it (critic-d4a-e finding 13). `End` carries no
/// secret and its reply none; this type asks for nothing else.
pub struct HolderToEnd(HolderClient);

impl HolderToEnd {
    #[cfg(unix)]
    pub fn at(endpoint: &Endpoint) -> Option<HolderToEnd> {
        HolderToEnd::reach(endpoint).holder()
    }

    /// [`HolderToEnd::at`], a holder that takes no connection told apart from none (T59).
    #[cfg(unix)]
    pub fn reach(endpoint: &Endpoint) -> Reached<HolderToEnd> {
        match std::env::current_exe() {
            Ok(me) => HolderClient::connect(endpoint, &me, same_program).map(HolderToEnd),
            Err(_) => Reached::Nobody,
        }
    }

    /// [`HolderToEnd::at`] with the expected image named (tests).
    #[cfg(unix)]
    pub fn at_expecting(endpoint: &Endpoint, image: &std::path::Path) -> Option<HolderToEnd> {
        HolderClient::connect(endpoint, image, same_program)
            .map(HolderToEnd)
            .holder()
    }

    #[cfg(windows)]
    pub fn on_console(store: &StoreBinding) -> Result<Option<HolderToEnd>, HolderUnavailable> {
        HolderToEnd::reach_console(store).map(Reached::holder)
    }

    /// [`HolderToEnd::on_console`], a holder whose pipe stays busy told apart from none (T59).
    #[cfg(windows)]
    pub fn reach_console(store: &StoreBinding) -> Result<Reached<HolderToEnd>, HolderUnavailable> {
        let me = std::env::current_exe().map_err(|_| HolderUnavailable::NoConsole { code: 0 })?;
        Ok(HolderClient::console_with(store, &me, same_program)?.map(HolderToEnd))
    }

    pub fn server_pid(&self) -> Option<u32> {
        self.0.server_pid()
    }

    /// `End`: the holder zeroes its key and exits.
    pub fn end(&mut self) -> Result<(), HolderError> {
        self.0.end()
    }

    /// [`HolderClient::terminate_server`]: a holder that does not answer, ended through this
    /// connection (S9).
    pub fn terminate_server(self) -> Result<(), HolderError> {
        self.0.terminate_server()
    }
}

/// A connect that failed (T59): past its step — a listener whose queue is full, which Linux makes
/// the connect wait on — or turned away while a process holds the endpoint's lock — macOS
/// refuses a connect to a full queue at once, as it does one to no listener — is a holder that
/// does not answer; anything else is no holder.
#[cfg(unix)]
fn missed<T>(e: &io::Error, lock: &std::path::Path) -> Reached<T> {
    let lives = match e.kind() {
        io::ErrorKind::TimedOut => true,
        io::ErrorKind::ConnectionRefused => super::os::lock_held(lock),
        _ => false,
    };
    if lives {
        Reached::NotResponding {
            pid: super::os::lock_pid(lock),
        }
    } else {
        Reached::Nobody
    }
}

/// What opening a holder's pipe came to.
#[cfg(windows)]
enum PipeOpen {
    Open(super::os::windows::Handle),
    /// Its one instance stayed busy past the wait: a server holds it and does not let go.
    Busy,
    /// No such pipe, or one this client may not open.
    Absent,
}

/// Open the holder's pipe at identification level (T53): a squatting server learns who
/// connected and can act as no one. A busy pipe is waited for once, within the step's 2 s and
/// the request's `deadline` (T59): its open is a step like a read or a write.
#[cfg(windows)]
fn open_pipe(name: &str, deadline: Duration) -> PipeOpen {
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
            return PipeOpen::Open(super::os::windows::Handle(h));
        }
        // SAFETY: the error of the call above.
        if unsafe { GetLastError() } != ERROR_PIPE_BUSY {
            return PipeOpen::Absent;
        }
        let step = deadline
            .saturating_sub(SystemClock.with_sleep())
            .min(IO_TIMEOUT);
        if attempt == 1 || step.is_zero() {
            return PipeOpen::Busy;
        }
        // 0 would be the pipe's own default wait: at least 1 ms.
        let ms = u32::try_from(step.as_millis()).unwrap_or(u32::MAX).max(1);
        // SAFETY: a NUL-terminated name; waits at most `ms`.
        unsafe { WaitNamedPipeW(wide.as_ptr(), ms) };
    }
    PipeOpen::Busy
}

/// The same file, by canonical path (a symlinked install, `/private/tmp` on macOS). Named on
/// every OS (Linux passes it and never calls it: no image check there).
fn same_file(a: &std::path::Path, b: &std::path::Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// This binary or another install of it: the same file, or a file of the same name (only
/// [`HolderToEnd`] accepts that).
fn same_program(a: &std::path::Path, b: &std::path::Path) -> bool {
    same_file(a, b) || (a.file_name().is_some() && a.file_name() == b.file_name())
}

#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::net::{UnixListener, UnixStream};

    use super::*;
    use crate::holder::protocol::write_reply;

    /// macOS refuses every socket option once a socket is shut down both ways (`EINVAL`: XNU's
    /// `sosetoptlock`), which a holder that has answered and closed leaves it in. Such a refusal
    /// is no failure of the request — the reply is still there to read, and nothing blocks on
    /// that socket — while any other refusal, and a request past its 5 s, still is.
    #[test]
    fn a_bound_the_os_refuses_on_a_shut_down_socket_is_no_failure() {
        let (a, _b) = UnixStream::pair().unwrap();
        let t = Timed {
            conn: &a,
            deadline: SystemClock.with_sleep() + OVERALL,
        };
        assert!(t
            .bound(|_, _| Err(io::ErrorKind::InvalidInput.into()))
            .is_ok());
        assert_eq!(
            t.bound(|_, _| Err(io::ErrorKind::PermissionDenied.into()))
                .unwrap_err()
                .kind(),
            io::ErrorKind::PermissionDenied
        );
        let spent = Timed {
            conn: &a,
            deadline: Duration::ZERO,
        };
        assert_eq!(
            spent.bound(|_, _| Ok(())).unwrap_err().kind(),
            io::ErrorKind::TimedOut
        );
    }

    /// T53, T59 (found on macos-latest): a holder answers at once and closes — a refusal comes
    /// before a byte of the request is read — and its reply is read, though the connection was
    /// closed before the client asked anything. macOS then refuses the client's per-step bound;
    /// the reply must still be read, as on Linux.
    #[test]
    fn a_reply_left_by_a_holder_that_already_closed_is_read() {
        let dir = tempfile::TempDir::new_in("/tmp").unwrap();
        let path = dir.path().join("h.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let server = std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            write_reply(&mut s, &Reply::Refused(Refusal::NotThisTerminal)).unwrap();
        });
        let endpoint = Endpoint::Socket {
            path,
            lock: std::path::PathBuf::new(),
        };
        let mut client = HolderClient::at(&endpoint).expect("a client of the listener");
        server.join().unwrap();
        assert!(
            client.closed_within(Duration::from_secs(2)),
            "the fake holder closed its end"
        );
        let got = client.status();
        assert!(
            matches!(got, Err(HolderError::Refused(Refusal::NotThisTerminal))),
            "{got:?}"
        );
    }
}
