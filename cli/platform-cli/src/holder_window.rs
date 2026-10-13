// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! This terminal's unlock window, as the CLI sees it (ADR 0068 §4; D.4a T49–T59): where its
//! holder lives, the store a holder binds, whether one serves here, and starting one. The **d
//! seam** at the end holds the only functions of D.4a-e that name d4a-d's code: a rename of
//! d's touches nothing else (Task 10 Step 5).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use apprafter_core::lock::{HolderUnavailableReason, UnlockWindow};
use apprafter_core::secrets::StoreId;
use apprafter_core::vault::{DataKey, VaultId};
use apprafter_core::{Context, CoreResult};

use crate::holder::client::{HolderClient, HolderError, HolderToEnd, Reached};
use crate::holder::protocol::{HolderUnavailable, InitHeader, StoreBinding};
use crate::holder::spawn::{start_holder, HolderStarted};

/// Where this terminal's holder lives, taken from the context once: on Unix its socket's base
/// (`Context::holder_base`), on Windows the directory of its pid file (`Context::runtime_dir`).
#[derive(Debug, Clone)]
pub(crate) struct HolderPlace {
    base: Option<PathBuf>,
    runtime_dir: PathBuf,
}

impl HolderPlace {
    pub(crate) fn of(ctx: &Context) -> HolderPlace {
        HolderPlace {
            base: ctx.holder_base().map(|b| PathBuf::from(&b.dir)),
            runtime_dir: ctx.runtime_dir().to_path_buf(),
        }
    }

    /// What looking for this terminal's holder of `store` finds (T50, T51, T53, T59): its
    /// client, none, or a holder that lives here and takes no connection — never read as "no
    /// holder" by `lock status` (rule 1); `Err` when no window is possible here at all.
    pub(crate) fn reach(
        &self,
        store: &StoreBinding,
    ) -> Result<Reached<HolderClient>, HolderUnavailable> {
        #[cfg(unix)]
        {
            let base = self.base.as_ref().ok_or(HolderUnavailable::NoRuntimeDir)?;
            let endpoint = crate::holder::endpoint::session_endpoint(base, store)?;
            Ok(HolderClient::reach(&endpoint))
        }
        #[cfg(windows)]
        {
            HolderClient::reach_console(store)
        }
    }

    /// This terminal's holder of `store`, to end it (`lock now`, `unlock`, `lock off`): what
    /// [`HolderPlace::reach`] finds, and also a holder an upgrade left behind at another path of
    /// this binary, which a reading client refuses (Task 8's `HolderToEnd`, critic-d4a-e
    /// finding 13).
    pub(crate) fn reach_to_end(
        &self,
        store: &StoreBinding,
    ) -> Result<Reached<HolderToEnd>, HolderUnavailable> {
        #[cfg(unix)]
        {
            let base = self.base.as_ref().ok_or(HolderUnavailable::NoRuntimeDir)?;
            let endpoint = crate::holder::endpoint::session_endpoint(base, store)?;
            Ok(HolderToEnd::reach(&endpoint))
        }
        #[cfg(windows)]
        {
            HolderToEnd::reach_console(store)
        }
    }

    /// [`HolderPlace::reach_to_end`]'s holder, when one took the connection.
    pub(crate) fn find_to_end(
        &self,
        store: &StoreBinding,
    ) -> Result<Option<HolderToEnd>, HolderUnavailable> {
        self.reach_to_end(store).map(Reached::holder)
    }

    /// Whether a window is possible here, asked before `unlock` asks anything (T59): on Unix
    /// the base and the socket path, connecting to nothing; on Windows a console to bind to.
    pub(crate) fn possible(&self, store: &StoreBinding) -> Result<(), HolderUnavailable> {
        #[cfg(unix)]
        {
            let base = self.base.as_ref().ok_or(HolderUnavailable::NoRuntimeDir)?;
            crate::holder::endpoint::session_endpoint(base, store).map(|_| ())
        }
        #[cfg(windows)]
        {
            HolderClient::on_console(store).map(|_| ())
        }
    }

    /// Where a new holder records itself (`InitHeader::base`).
    fn init_base(&self) -> Option<PathBuf> {
        if cfg!(windows) {
            Some(self.runtime_dir.clone())
        } else {
            self.base.clone()
        }
    }
}

/// The store a holder binds (T50): `store.yaml`'s id and the canonical root; `None` for a store
/// without an id (in files: `read_store_file` refuses a locked one without it).
pub(crate) fn store_binding(ctx: &Context) -> CoreResult<Option<StoreBinding>> {
    let Some(id) = ctx.secrets().store_file()?.and_then(|f| f.id) else {
        return Ok(None);
    };
    Ok(Some(binding(ctx.config_root(), id.as_str())))
}

/// `id` at `root`, canonical — as every holder of the store is keyed (a symlinked root is one
/// store).
pub(crate) fn binding(root: &Path, id: &str) -> StoreBinding {
    StoreBinding {
        id: id.to_string(),
        root: std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf()),
    }
}

/// This terminal's window on the store, for `lock status` (T56, T59, T66): asked of its holder
/// by `Status`, which restarts nothing; never a prompt, never the vault. Only a holder whose
/// `Status` names this store counts (its test, `window_state_believes_only_its_own_stores_holder`,
/// lands in Task 13 beside the fake holder it needs). A holder that does not answer — it took
/// the connection and said nothing, or it lives here and takes no connection — is
/// `not_responding`, never a closed window (T59, rule 1).
pub(crate) fn window_state(
    ctx: &Context,
    place: &HolderPlace,
    terminal: bool,
) -> CoreResult<UnlockWindow> {
    let every_command = ctx
        .secrets()
        .store_file()?
        .and_then(|f| f.unlock_timeout_min)
        == Some(0);
    let quiet = if !terminal {
        UnlockWindow::NoTerminal
    } else if every_command {
        UnlockWindow::EveryCommand
    } else {
        UnlockWindow::Closed
    };
    let Some(store) = store_binding(ctx)? else {
        return Ok(quiet);
    };
    Ok(match place.reach(&store) {
        Ok(Reached::Holder(mut client)) => match client.status() {
            // A holder at this store's endpoint that names another store is no window of this
            // store — as `Window::session` decides (Task 13; d4a-f's critic B, minor 13).
            Ok(s) if s.store_id == store.id => UnlockWindow::Open {
                left_secs: s.left_secs,
                minutes: s.minutes,
                holder_version: s.version,
                hardening: s.hardening,
            },
            Ok(_) => quiet,
            // It took the connection and said nothing (T59).
            Err(e) if silent(&e) => UnlockWindow::Unavailable {
                reason: HolderUnavailableReason::NotResponding,
            },
            // `NotThisTerminal`, or `WindowEnded` (it exits now): no window for this terminal.
            Err(_) => quiet,
        },
        // It lives here and takes no connection (T59): a full listen queue, a busy pipe.
        Ok(Reached::NotResponding { .. }) => UnlockWindow::Unavailable {
            reason: HolderUnavailableReason::NotResponding,
        },
        Ok(Reached::Nobody) => quiet,
        Err(u) if terminal => UnlockWindow::Unavailable { reason: u.reason() },
        Err(_) => quiet,
    })
}

/// Start this terminal's holder of `store` for `minutes` with a key a prompt opened in this
/// process (T48, T56). A holder that has just answered `End` may hold its lock a moment
/// longer: it is waited for up to a second.
pub(crate) fn start_window(
    place: &HolderPlace,
    store: &StoreBinding,
    key: &DataKey,
    vault_id: VaultId,
    minutes: u32,
) -> Result<HolderStarted, HolderUnavailable> {
    let init = InitHeader {
        store: store.clone(),
        base: place.init_base().ok_or(HolderUnavailable::NoRuntimeDir)?,
        vault_id,
        minutes,
        window_secs: None,
        poll_leader: false,
        client_pid: std::process::id(),
    };
    for _ in 0..10 {
        match start_holder(&init, key)? {
            HolderStarted::AlreadyRunning { .. } => std::thread::sleep(Duration::from_millis(100)),
            started => return Ok(started),
        }
    }
    start_holder(&init, key)
}

/// What ending this terminal's window came to (`lock now`, `lock off`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Ended {
    /// Its holder answered `End`.
    Closed,
    /// Its holder did not answer and was stopped (S9: exactly the process the connection's
    /// server is).
    Stopped,
    /// Its holder does not answer and could not be stopped from here — it takes no connection,
    /// or the OS refused to stop it — so it still holds the key (`pid`, when known). Never
    /// reported as no window (rule 1).
    StillOpen { pid: Option<u32> },
    /// No window was open in this terminal.
    None,
}

/// End this terminal's window on the store `ctx` names (T56, T59, T65, S9): `End` to its
/// holder — an older install's too ([`HolderPlace::reach_to_end`]) — and a holder that does not
/// answer is stopped through its connection. Asks for nothing, reads no secret.
pub(crate) fn end_window(ctx: &Context) -> CoreResult<Ended> {
    let Some(store) = store_binding(ctx)? else {
        return Ok(Ended::None);
    };
    Ok(end_at(&HolderPlace::of(ctx), &store))
}

/// [`end_window`] at `place`.
fn end_at(place: &HolderPlace, store: &StoreBinding) -> Ended {
    match place.reach_to_end(store) {
        Ok(Reached::Holder(mut holder)) => match holder.end() {
            Ok(()) => Ended::Closed,
            Err(e) if silent(&e) => {
                let pid = holder.server_pid();
                stopped(holder.terminate_server(), pid)
            }
            Err(_) => Ended::None,
        },
        // It lives and takes no connection: nothing proves which process to stop (S9).
        Ok(Reached::NotResponding { pid }) => Ended::StillOpen { pid },
        Ok(Reached::Nobody) | Err(_) => Ended::None,
    }
}

/// What stopping a holder that did not answer came to: stopped; gone already (it let go of the
/// connection, or no such process is left); or still there.
fn stopped(r: Result<(), HolderError>, pid: Option<u32>) -> Ended {
    match r {
        Ok(()) => Ended::Stopped,
        Err(HolderError::Io(e)) if gone(&e) => Ended::None,
        Err(_) => Ended::StillOpen { pid },
    }
}

/// The holder a stop was aimed at is gone: it let go of the connection, or its process is.
fn gone(e: &std::io::Error) -> bool {
    #[cfg(unix)]
    let no_such_process = e.raw_os_error() == Some(libc::ESRCH);
    // ERROR_INVALID_PARAMETER: `OpenProcess` names no process of that pid.
    #[cfg(windows)]
    let no_such_process = e.raw_os_error() == Some(87);
    e.kind() == std::io::ErrorKind::NotFound || no_such_process
}

/// C30: what opening a window says — `unlock`'s result, and a command's own prompt's once its
/// window started (O6: everything started from this terminal can use it).
pub(crate) fn opened_note(minutes: u32) -> String {
    format!(
        "Unlocked in this terminal for {minutes} min, restarted by each command that uses it. \
         Everything started from this terminal can use it, an AI agent included; `apprafter \
         lock now` ends it."
    )
}

/// A holder that took the connection and said nothing within the client's bounds (T59), as
/// against one that is gone (its connection closed: it exited).
pub(crate) fn silent(e: &HolderError) -> bool {
    matches!(e, HolderError::Io(io) if matches!(io.kind(), std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock))
}

/// This CLI leads its own session (`exec apprafter unlock`, `ssh -t host apprafter …`): a
/// window it opens ends with it (T52).
pub(crate) fn leads_own_session() -> bool {
    #[cfg(unix)]
    {
        // SAFETY: getsid(0) and getpid() cannot fail for this process.
        unsafe { libc::getsid(0) == libc::getpid() }
    }
    #[cfg(windows)]
    {
        false
    }
}

// ---------------------------------------------------------------------------------------------
// The d seam (Task 10 Step 5): the only functions of D.4a-e that name d4a-d's code. Task 13
// adds the fourth, `own_session`.
// ---------------------------------------------------------------------------------------------

/// A vault this process opened at its prompt: its key, which vault it opens, and the vault's
/// own window (T33, T56).
pub(crate) struct Opened {
    pub(crate) key: Arc<DataKey>,
    pub(crate) vault_id: VaultId,
    pub(crate) default_minutes: u32,
}

/// D2 (T61): stdin is this process's controlling terminal (Windows: its console).
pub(crate) fn terminal_present() -> bool {
    crate::prompt::secret::terminal_present()
}

/// C27's question; the window's length is in the line before it.
const UNLOCK_PROMPT: &str = "Lock password or recovery key: ";

/// D3: ask at this terminal — `intro` once (C27's first line, ending in a newline), then C27's
/// question, d's three attempts, `unlock_failed`, its 5-minute `store_locked{timed_out}` and
/// Ctrl-C — and open the vault of the store `id` names in this process (D1, inside d's loop).
pub(crate) fn ask_and_open(ctx: &Context, id: &StoreId, intro: &str) -> miette::Result<Opened> {
    let opened = crate::vault_access::ask_and_open(
        &crate::prompt::Terminal,
        ctx.config_root(),
        id,
        intro,
        UNLOCK_PROMPT,
        apprafter_core::vault::KdfPolicy::active(),
    )
    .map_err(miette::Report::new)?;
    Ok(Opened {
        key: opened.key,
        vault_id: opened.vault_id,
        default_minutes: opened.unlock_timeout_min,
    })
}

/// D6 (T23): a feature build says so on `unlock` and `lock status`, as d's `lock on` does,
/// with d's helper. No golden runs either leaf (Deviations 14).
pub(crate) fn fast_kdf_note() {
    crate::vault_access::insecure_kdf_note();
}

#[cfg(all(test, unix))]
mod tests {
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::net::UnixListener;

    use apprafter_core::secrets::testing::locked_store;

    use super::*;
    use crate::holder::endpoint::{session_endpoint, Endpoint};

    /// A locked scratch store, a base under `/tmp` (`sun_path`) and this test's endpoint there,
    /// where a fake holder plays each case.
    struct Here {
        _store: tempfile::TempDir,
        _base: tempfile::TempDir,
        ctx: Context,
        place: HolderPlace,
        store: StoreBinding,
        socket: PathBuf,
        lock: PathBuf,
    }

    fn here() -> Here {
        let store_dir = tempfile::tempdir().unwrap();
        locked_store(store_dir.path(), &[("prod", None)]);
        let base_dir = tempfile::TempDir::new_in("/tmp").unwrap();
        let base = base_dir.path().join("apprafter");
        let ctx = Context::for_desktop(store_dir.path().to_path_buf(), "http://127.0.0.1:1");
        let store = store_binding(&ctx)
            .unwrap()
            .expect("a locked store names its id");
        let Endpoint::Socket { path, lock } = session_endpoint(&base, &store).unwrap() else {
            unreachable!("Unix")
        };
        Here {
            place: HolderPlace {
                base: Some(base),
                runtime_dir: store_dir.path().join("run"),
            },
            _store: store_dir,
            _base: base_dir,
            ctx,
            store,
            socket: path,
            lock,
        }
    }

    /// The endpoint's lock taken as a living holder takes it (`LOCK_EX`, its pid written), held
    /// while the returned file is open.
    fn hold_lock(lock: &Path, pid: u32) -> std::fs::File {
        let f = std::fs::File::create(lock).unwrap();
        // SAFETY: flock on the file just made, held while it stays open.
        assert_eq!(
            unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
            0
        );
        std::fs::write(lock, format!("{pid}\n")).unwrap();
        f
    }

    /// A connect that never waits: queued, or turned away at once (a full queue: `EAGAIN` on
    /// Linux, `ECONNREFUSED` on macOS).
    fn queue_one(path: &Path) -> std::io::Result<OwnedFd> {
        // SAFETY: a new socket, owned below.
        let fd = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0) };
        assert!(fd >= 0);
        // SAFETY: a descriptor this function opened and nothing else owns.
        let fd = unsafe { OwnedFd::from_raw_fd(fd) };
        // SAFETY: fcntl on that descriptor.
        unsafe {
            let flags = libc::fcntl(fd.as_raw_fd(), libc::F_GETFL);
            libc::fcntl(fd.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK);
        }
        // SAFETY: an all-zero sockaddr_un is valid; the path is copied in below.
        let mut addr: libc::sockaddr_un = unsafe { std::mem::zeroed() };
        addr.sun_family = libc::AF_UNIX as libc::sa_family_t;
        for (d, s) in addr.sun_path.iter_mut().zip(path.as_os_str().as_bytes()) {
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

    fn not_responding() -> UnlockWindow {
        UnlockWindow::Unavailable {
            reason: HolderUnavailableReason::NotResponding,
        }
    }

    /// T59, rule 1 (found in Part 1's run C): a holder that hung and whose listen queue filled
    /// takes no connection — Linux makes the connect wait past its step, macOS turns it away at
    /// once while the holder still holds its lock. `lock status` says it does not answer, never
    /// that no window is open; `lock now` says it could not end it, naming its pid.
    #[test]
    fn a_holder_whose_queue_is_full_is_not_responding_never_closed() {
        let h = here();
        let _held = hold_lock(&h.lock, 4242);
        let listener = UnixListener::bind(&h.socket).unwrap();
        // SAFETY: the listener's queue cut to the smallest one the OS keeps; it never accepts.
        assert_eq!(unsafe { libc::listen(listener.as_raw_fd(), 0) }, 0);
        let mut queued = Vec::new();
        while let Ok(fd) = queue_one(&h.socket) {
            queued.push(fd);
            assert!(queued.len() < 1024, "the listen queue never filled");
        }
        assert_eq!(
            window_state(&h.ctx, &h.place, true).unwrap(),
            not_responding()
        );
        assert_eq!(
            window_state(&h.ctx, &h.place, false).unwrap(),
            not_responding(),
            "a holder of this session, asked without a terminal"
        );
        assert_eq!(
            end_at(&h.place, &h.store),
            Ended::StillOpen { pid: Some(4242) }
        );
        drop(queued);
    }

    /// A dead holder's socket (no listener) is no window — unless a process still holds the
    /// endpoint's lock: then a holder lives there and takes no connection.
    #[test]
    fn a_dead_holders_socket_is_no_window_and_a_held_lock_is_a_living_holder() {
        let h = here();
        drop(UnixListener::bind(&h.socket).unwrap());
        assert!(h.socket.exists(), "the dead holder's socket stays");
        std::fs::write(&h.lock, "4242\n").unwrap();
        assert_eq!(
            window_state(&h.ctx, &h.place, true).unwrap(),
            UnlockWindow::Closed
        );
        assert_eq!(end_at(&h.place, &h.store), Ended::None);
        let held = hold_lock(&h.lock, 4343);
        assert_eq!(
            window_state(&h.ctx, &h.place, true).unwrap(),
            not_responding()
        );
        assert_eq!(
            end_at(&h.place, &h.store),
            Ended::StillOpen { pid: Some(4343) }
        );
        drop(held);
        assert_eq!(
            window_state(&h.ctx, &h.place, true).unwrap(),
            UnlockWindow::Closed,
            "once its lock is let go"
        );
    }

    /// S9, rule 1: a holder that did not answer and was stopped; one that is gone already (it
    /// let go of the connection, or no such process is left); and one the OS would not stop,
    /// which still holds the key — never "no window".
    #[test]
    fn a_holder_that_could_not_be_stopped_is_not_ended() {
        let io = |e: std::io::Error| Err(HolderError::Io(e));
        assert_eq!(stopped(Ok(()), Some(7)), Ended::Stopped);
        assert_eq!(
            stopped(io(std::io::ErrorKind::NotFound.into()), Some(7)),
            Ended::None
        );
        assert_eq!(
            stopped(io(std::io::Error::from_raw_os_error(libc::ESRCH)), Some(7)),
            Ended::None
        );
        assert_eq!(
            stopped(io(std::io::Error::from_raw_os_error(libc::ENOSYS)), Some(7)),
            Ended::StillOpen { pid: Some(7) }
        );
        assert_eq!(
            stopped(Err(HolderError::Unexpected), None),
            Ended::StillOpen { pid: None }
        );
    }
}
