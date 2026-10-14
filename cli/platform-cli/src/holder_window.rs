// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! This terminal's unlock window, as the CLI sees it (ADR 0068 §4; D.4a T49–T59): where its
//! holder lives, the store a holder binds, whether one serves here, and starting one. The **d
//! seam** at the end holds the only functions of D.4a-e that name d4a-d's code: a rename of
//! d's touches nothing else (Task 10 Step 5).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use apprafter_core::lock::{HolderUnavailableReason, UnlockWindow};
use apprafter_core::secrets::{SlotInfo, StoreId, VaultRequest, VaultSession};
use apprafter_core::vault::{DataKey, VaultId, DEFAULT_UNLOCK_TIMEOUT_MIN};
use apprafter_core::{Context, CoreResult, SecretString, Zeroizing};
use cli_core::secrets::{CachedSecret, Recipient};
use cli_core::{CliError, LockedWhy};

use crate::holder::client::{HolderClient, HolderError, HolderToEnd, Reached};
use crate::holder::protocol::{HolderItem, HolderUnavailable, InitHeader, Refusal, StoreBinding};
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

    /// [`HolderPlace::reach`]'s holder, when one took the connection: a reading command asks
    /// without one either way, after naming one that does not answer (`Window::probe`).
    pub(crate) fn find(
        &self,
        store: &StoreBinding,
    ) -> Result<Option<HolderClient>, HolderUnavailable> {
        self.reach(store).map(Reached::holder)
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
// A command's reads through this terminal's window (§3.4; T48, T53, T56, T59, C3, rule 9)
// ---------------------------------------------------------------------------------------------

/// T59's one note when no window can be had: the command goes on; the next one asks. (Added
/// here, with its two callers, not in Task 12: an uncalled `pub(crate)` function fails clippy.)
pub(crate) fn no_window_note(u: &HolderUnavailable) -> String {
    format!(
        "No unlock window in this terminal: {}. The next command asks again.",
        u.describe()
    )
}

/// What a client does about a holder that did not serve (T53, critic C3; T59).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Reaction {
    /// No holder serves this terminal after all — another terminal's, one whose window just
    /// ended, one that is gone: open here, and send no `End` (the endpoint may not be ours).
    Prompt,
    /// End it, then open here: the in-process open decides what is wrong, never the holder's
    /// word — on the vault or on a cache (only an in-process open says either, rule 3).
    EndThenPrompt,
    /// It took the request and said nothing: name it once (T59), then open here.
    Hung,
}

pub(crate) fn reaction(e: &HolderError) -> Reaction {
    match e {
        HolderError::Refused(Refusal::NotThisTerminal | Refusal::WindowEnded) => Reaction::Prompt,
        HolderError::Refused(_) => Reaction::EndThenPrompt,
        e if silent(e) => Reaction::Hung,
        HolderError::Io(_) => Reaction::Prompt,
        HolderError::Protocol(_) | HolderError::Spent | HolderError::Unexpected => {
            Reaction::EndThenPrompt
        }
    }
}

/// C3 and T59, carried out: from now on this process opens the vault itself; a holder to end
/// is looked for with `find` (only then); the note to print, if any.
fn react(
    e: &HolderError,
    pid: Option<u32>,
    find: &dyn Fn() -> Option<HolderClient>,
    no_holder: &AtomicBool,
) -> Option<String> {
    no_holder.store(true, Ordering::SeqCst);
    match reaction(e) {
        Reaction::Prompt => None,
        Reaction::EndThenPrompt => {
            if let Some(mut holder) = find() {
                let _ = holder.end();
            }
            None
        }
        Reaction::Hung => Some(no_window_note(&HolderUnavailable::NotResponding { pid })),
    }
}

/// Rule 9 (critic S1): `find`, unless this process may no longer ask a holder.
fn holder_for<S>(no_holder: &AtomicBool, find: impl FnOnce() -> Option<S>) -> Option<S> {
    if no_holder.load(Ordering::SeqCst) {
        None
    } else {
        find()
    }
}

/// What one request to the holder came to.
enum Answer<T> {
    Served(T),
    /// This process opens the item itself, now and for the rest of its life.
    Own,
}

type Finder = Box<dyn Fn() -> Option<HolderClient> + Send + Sync>;
type OwnPath = Box<dyn Fn() -> cli_core::Result<Arc<dyn VaultSession>> + Send + Sync>;

/// A read session through this terminal's holder (T48, T53, C3). It holds no key: each item is
/// one connection. A refusal or a silent holder is handled as C3 and T59 say, and the item is
/// then opened by this process's own path (`own`: its key while the secret phase lasts, else
/// a prompt) — as is every later one: no holder is asked again (rule 9). A diagnostic's session
/// has no own path (T66): there the item is `store_locked`, which `open_token` reports as
/// `Locked` (critic-d4a-e finding 5).
pub(crate) struct HolderSession {
    find: Finder,
    own: Option<OwnPath>,
    root: PathBuf,
    no_holder: Arc<AtomicBool>,
    /// The window's own flag ([`Window`]'s `hung`): set when this holder falls silent at an
    /// item, which names it (T59) — so the prompt that follows promises no window and starts
    /// none, and the holder, which still holds the place, is not named a second time.
    hung: Arc<AtomicBool>,
}

impl std::fmt::Debug for HolderSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HolderSession")
            .field("root", &self.root)
            .finish()
    }
}

impl HolderSession {
    /// The session of `store`'s holder in this terminal, for the read `req` asked: a command's
    /// (`own`: its own path after a refusal) or a diagnostic's (none).
    fn for_store(
        place: HolderPlace,
        store: StoreBinding,
        req: &VaultRequest<'_>,
        no_holder: Arc<AtomicBool>,
        hung: Arc<AtomicBool>,
        own: bool,
    ) -> HolderSession {
        let root = req.store_root.to_path_buf();
        let (own_root, id, slots): (PathBuf, StoreId, Vec<SlotInfo>) =
            (root.clone(), req.store_id.clone(), req.slots.to_vec());
        let own: Option<OwnPath> = own.then(move || -> OwnPath {
            Box::new(move || {
                own_session(&VaultRequest {
                    store_root: &own_root,
                    store_id: &id,
                    slots: &slots,
                })
            })
        });
        HolderSession {
            find: Box::new(move || place.find(&store).ok().flatten()),
            own,
            root,
            no_holder,
            hung,
        }
    }

    /// This process's own path for an item; a diagnostic's session has none (T66).
    fn own(&self) -> cli_core::Result<Arc<dyn VaultSession>> {
        match &self.own {
            Some(own) => own(),
            None => Err(CliError::StoreLocked {
                store: self.root.clone(),
                reason: LockedWhy::NoTerminal,
                stopped_before: None,
            }),
        }
    }

    fn ask<T>(
        &self,
        request: impl FnOnce(&mut HolderClient) -> Result<T, HolderError>,
    ) -> Answer<T> {
        if self.no_holder.load(Ordering::SeqCst) {
            return Answer::Own;
        }
        let Some(mut client) = (self.find)() else {
            // Gone since the probe (its window ended, `lock now`): as `WindowEnded`.
            self.no_holder.store(true, Ordering::SeqCst);
            return Answer::Own;
        };
        let pid = client.server_pid();
        match request(&mut client) {
            Ok(v) => Answer::Served(v),
            Err(e) => {
                if let Some(note) = react(&e, pid, &*self.find, &self.no_holder) {
                    self.hung.store(true, Ordering::SeqCst);
                    eprintln!("{note}");
                }
                Answer::Own
            }
        }
    }
}

/// An item's bytes as text; anything else is a garbled reply.
fn text_of(bytes: &[u8]) -> Result<&str, HolderError> {
    std::str::from_utf8(bytes).map_err(|_| HolderError::Unexpected)
}

impl VaultSession for HolderSession {
    fn hetzner_token(&self, vault: &[u8], target: &str) -> cli_core::Result<Option<SecretString>> {
        let item = HolderItem::HetznerToken {
            target: target.into(),
        };
        let asked = self.ask(|c| {
            c.open_item(vault, &item)?
                .map(|v| text_of(&v).map(SecretString::new))
                .transpose()
        });
        match asked {
            Answer::Served(token) => Ok(token),
            Answer::Own => self.own()?.hetzner_token(vault, target),
        }
    }

    fn open_cache(
        &self,
        vault: &[u8],
        secret: CachedSecret,
        target: &str,
        armored: &str,
    ) -> cli_core::Result<Zeroizing<String>> {
        let asked = self.ask(|c| {
            let v = c.open_cache(vault, secret, target, armored)?;
            text_of(&v).map(|s| Zeroizing::new(s.to_string()))
        });
        match asked {
            Answer::Served(text) => Ok(text),
            // A holder's `CacheRefused` included: this process's own open says whether the
            // cache is undecryptable (rule 3, critic-d4a-e finding 16).
            Answer::Own => self.own()?.open_cache(vault, secret, target, armored),
        }
    }

    fn recipient(&self, vault: &[u8]) -> cli_core::Result<Recipient> {
        let asked = self.ask(|c| {
            c.recipient(vault)?
                .parse::<Recipient>()
                .map_err(|_| HolderError::Unexpected)
        });
        match asked {
            Answer::Served(r) => Ok(r),
            Answer::Own => self.own()?.recipient(vault),
        }
    }
}

type StartFn = fn(
    &HolderPlace,
    &StoreBinding,
    &DataKey,
    VaultId,
    u32,
) -> Result<HolderStarted, HolderUnavailable>;

/// What this CLI process knows of its terminal's window (one command per process).
struct Window {
    place: HolderPlace,
    /// Set once this process prompted, or found its holder refusing or silent: from then on it
    /// opens the vault itself and never asks a holder again (rule 9, critic S1).
    no_holder: Arc<AtomicBool>,
    /// Set once this process named a holder of this terminal that does not answer (T59) — at
    /// its connect, its `Status` or an item (its [`HolderSession`] shares the flag): it still
    /// holds the place, so a prompt here promises no window and starts none — a start could
    /// only meet it again, and name it twice.
    hung: Arc<AtomicBool>,
    /// How a holder is started ([`start_window`]; a counter in the unit tests).
    start: StartFn,
    /// Whether this CLI leads its own session ([`leads_own_session`]; a constant in the unit
    /// tests): a window it started would end with this very command (T52), so a command's own
    /// prompt starts none — `unlock` alone does, and says so.
    leads_session: fn() -> bool,
}

static WINDOW: OnceLock<Window> = OnceLock::new();

impl Window {
    /// §3.4's holder step: this terminal's holder of the store, when this process may ask one
    /// and one serves that store here — checked with `Status`, which restarts nothing. `own`:
    /// a command's session (its own path after a refusal); else a diagnostic's (none, T66).
    fn session(&self, req: &VaultRequest<'_>, own: bool) -> Option<Arc<dyn VaultSession>> {
        holder_for(&self.no_holder, || {
            let store = binding(req.store_root, req.store_id.as_str());
            let mut probe = match self.probe(&store) {
                Ok(found) => found?,
                Err(note) => {
                    eprintln!("{note}");
                    return None;
                }
            };
            let pid = probe.server_pid();
            match probe.status() {
                Ok(s) if s.store_id == store.id => {
                    let session = HolderSession::for_store(
                        self.place.clone(),
                        store,
                        req,
                        self.no_holder.clone(),
                        self.hung.clone(),
                        own,
                    );
                    Some(Arc::new(session) as Arc<dyn VaultSession>)
                }
                // Another store's holder at this store's endpoint is no holder of ours.
                Ok(_) => None,
                Err(e) => {
                    let (place, again) = (self.place.clone(), store.clone());
                    let find = move || place.find(&again).ok().flatten();
                    if let Some(note) = react(&e, pid, &find, &self.no_holder) {
                        self.hung.store(true, Ordering::SeqCst);
                        eprintln!("{note}");
                    }
                    None
                }
            }
        })
    }

    /// This terminal's holder of `store`, when one takes the connection. One that lives here
    /// and takes none (T59: a full listen queue, a busy pipe) is named once — the note is the
    /// `Err` — and from then on this process opens the vault itself (a command no
    /// longer meets it as no window, rule 1).
    fn probe(&self, store: &StoreBinding) -> Result<Option<HolderClient>, String> {
        match self.place.reach(store) {
            Ok(Reached::Holder(client)) => Ok(Some(client)),
            Ok(Reached::NotResponding { pid }) => {
                self.no_holder.store(true, Ordering::SeqCst);
                self.hung.store(true, Ordering::SeqCst);
                Err(no_window_note(&HolderUnavailable::NotResponding { pid }))
            }
            Ok(Reached::Nobody) | Err(_) => Ok(None),
        }
    }

    /// The minutes a command's own prompt here would open a window for (C28, T52, T56): the
    /// store's mirror of the vault's (5 without one), unless they are 0, this CLI leads its own
    /// session, or this CLI has nowhere to put a holder (no base). Looked at before the prompt
    /// with no I/O beyond `store.yaml`: a start that then fails says so in T59's note.
    fn would_open(&self, root: &Path) -> Option<u32> {
        if (self.leads_session)() || self.place.init_base().is_none() {
            return None;
        }
        // T59: a holder of this terminal named as not answering holds the place.
        if self.hung.load(Ordering::SeqCst) {
            return None;
        }
        let minutes = apprafter_core::secrets::read_store_file(root)
            .ok()
            .flatten()
            .and_then(|f| f.unlock_timeout_min)
            .unwrap_or(DEFAULT_UNLOCK_TIMEOUT_MIN);
        (minutes != 0).then_some(minutes)
    }

    /// After a prompt opened the vault: no holder from now on (rule 9), and a window for the
    /// vault's own minutes unless they are 0 or this CLI leads its own session (T56, T52). What
    /// to say: C30 once a holder started (O6); T59's note when none could start, or when a
    /// holder of this terminal this process could not use holds the place (`AlreadyRunning`:
    /// an older install's, or a silent one — `lock now` ends it, critic-d4a-e finding 13);
    /// nothing when no window was meant.
    fn after_prompt(
        &self,
        root: &Path,
        id: &StoreId,
        key: &DataKey,
        vault_id: VaultId,
        default_minutes: u32,
    ) -> Option<String> {
        self.no_holder.store(true, Ordering::SeqCst);
        if default_minutes == 0 || (self.leads_session)() {
            return None;
        }
        // T59: the holder already named as not answering holds the place.
        if self.hung.load(Ordering::SeqCst) {
            return None;
        }
        let store = binding(root, id.as_str());
        Some(
            match (self.start)(&self.place, &store, key, vault_id, default_minutes) {
                Ok(HolderStarted::Started { .. }) => opened_note(default_minutes),
                Ok(HolderStarted::AlreadyRunning { pid }) => {
                    no_window_note(&HolderUnavailable::NotResponding { pid: Some(pid) })
                }
                Err(u) => no_window_note(&u),
            },
        )
    }
}

/// `context.rs`'s `build()`: this process's window, once (a later call keeps the first). Never
/// in this crate's unit-test binary: some of its tests run command code whose context comes from
/// the process environment (`cluster_bootstrap`'s phase three), and a window installed there
/// would reach every later test of that binary — d's `vault_access` tests would ask the window's
/// question, and start a holder (the test binary itself, `current_exe`) after each prompt. The
/// window's unit tests build a [`Window`] of their own; `lock_window_test` runs the real CLI.
pub(crate) fn install(place: HolderPlace) {
    if cfg!(test) {
        return;
    }
    let _ = WINDOW.set(Window {
        place,
        no_holder: Arc::new(AtomicBool::new(false)),
        hung: Arc::new(AtomicBool::new(false)),
        start: start_window,
        leads_session: leads_own_session,
    });
}

/// Hook A (the top of d's `CliVaultAccess::session`, when this process holds no key of its
/// own): this terminal's holder of the store, if this process may ask one and one serves here.
pub(crate) fn window_session(req: &VaultRequest<'_>) -> Option<Arc<dyn VaultSession>> {
    WINDOW.get()?.session(req, true)
}

/// `CliVaultAccess::open_session` (T66): this terminal's window with no own path — a window that
/// ends, refuses or falls silent under a diagnostic is `store_locked`, never a prompt.
pub(crate) fn diagnostic_session(req: &VaultRequest<'_>) -> Option<Arc<dyn VaultSession>> {
    WINDOW.get()?.session(req, false)
}

/// The question of a command's own prompt (C28; d's `Inner::key_for` asks it): "(unlocks this
/// terminal for <n> min)" when its answer will open a window here, else `asked_again`, d's
/// "(asked again by the next command)" (critic-d4a-e finding 7).
pub(crate) fn inline_prompt(root: &Path, asked_again: &str) -> String {
    match WINDOW.get().and_then(|w| w.would_open(root)) {
        Some(n) => format!("Lock password or recovery key (unlocks this terminal for {n} min): "),
        None => asked_again.to_string(),
    }
}

/// Hook B (d's `Inner::key_for`, right after its prompt opened the vault — for a read or a
/// write key): no holder from now on, this terminal's window, and what it opened (C30) or why
/// none (T59), on stderr.
pub(crate) fn after_prompt(
    root: &Path,
    id: &StoreId,
    key: &DataKey,
    vault_id: VaultId,
    default_minutes: u32,
) {
    if let Some(said) = WINDOW
        .get()
        .and_then(|w| w.after_prompt(root, id, key, vault_id, default_minutes))
    {
        eprintln!("{said}");
    }
}

/// T57 (D9: the first line of d's `Terminal::ask`, so before any prompt of the CLI reads —
/// `unlock`'s, a command's own, `lock on`'s and `lock off`'s): on Linux this process stops being
/// dumpable until it exits (no same-user `ptrace`, no `/proc/<pid>/mem`, no core file); its
/// children reset the flag at `exec`, so `kubectl` and `ssh` are unaffected. macOS gets no
/// `PT_DENY_ATTACH` here (it cannot be undone and ends a CLI run under a debugger, holder
/// H-Q11); Windows nothing.
pub(crate) fn harden_for_secrets() {
    #[cfg(target_os = "linux")]
    // SAFETY: PR_SET_DUMPABLE takes an integer and changes only this process's flag.
    unsafe {
        libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0);
    }
}

// ---------------------------------------------------------------------------------------------
// The d seam (Task 10 Step 5): the only functions of D.4a-e that name d4a-d's code (Task 13
// added the fourth, `own_session`).
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

/// D4: this process's own read path once it may not use a holder — d's `CliSession` for the
/// store: its key while the secret phase lasts, else a new prompt (which ends in hook B).
pub(crate) fn own_session(req: &VaultRequest<'_>) -> cli_core::Result<Arc<dyn VaultSession>> {
    crate::vault_access::process().own_session(req)
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
    /// endpoint's lock: then a holder lives there and takes no connection. A lock let go of
    /// stays held while any process keeps a descriptor of it: a child another test thread forks
    /// keeps one until its `exec` (GOTCHA-240). So the closed window is asked for once the lock
    /// reads free, never before — and a child that keeps it 200 ms plays that race every run.
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
        let mut child = keeps_it_a_moment(&held);
        drop(held);
        let_go_within(&h.lock, Duration::from_secs(2));
        assert_eq!(
            window_state(&h.ctx, &h.place, true).unwrap(),
            UnlockWindow::Closed,
            "once its lock is let go"
        );
        child.wait().unwrap();
    }

    /// A child that inherits `lock`'s descriptor and keeps it 200 ms: the lock stays held that
    /// long after this process lets go of it, as a forked child keeps it until its `exec`.
    fn keeps_it_a_moment(lock: &std::fs::File) -> std::process::Child {
        use std::os::unix::process::CommandExt;
        let fd = lock.as_raw_fd();
        let mut keeper = std::process::Command::new("sleep");
        keeper.arg("0.2");
        // SAFETY: between fork and exec the child only clears close-on-exec on its own copy of a
        // descriptor this process has open (async-signal-safe `fcntl`); the parent's keeps it.
        unsafe {
            keeper.pre_exec(move || {
                if libc::fcntl(fd, libc::F_SETFD, 0) == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        keeper.spawn().unwrap()
    }

    /// Wait, at most `limit`, until no process holds `lock` — failing if one still does.
    fn let_go_within(lock: &Path, limit: Duration) {
        let deadline = std::time::Instant::now() + limit;
        while crate::holder::os::lock_held(lock) {
            assert!(
                std::time::Instant::now() < deadline,
                "{} is still held after {limit:?}",
                lock.display()
            );
            std::thread::sleep(Duration::from_millis(10));
        }
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

    /// T59: a holder that lives here and takes no connection is named once, with its
    /// pid, as one that took the connection and said nothing is. From then on this process
    /// opens the vault itself, its question promises no window, and its prompt starts none:
    /// the place is held, so a start could only meet that holder again and name it twice.
    #[test]
    fn a_holder_that_takes_no_connection_is_named_once_and_promises_no_window() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let h = here();
        drop(UnixListener::bind(&h.socket).unwrap());
        let _held = hold_lock(&h.lock, 4343);
        let w = Window {
            place: h.place.clone(),
            no_holder: Arc::new(AtomicBool::new(false)),
            hung: Arc::new(AtomicBool::new(false)),
            start: |_, _, _, _, _| unreachable!("a holder started over one that holds the place"),
            leads_session: || false,
        };
        let root = h.ctx.config_root();
        assert_eq!(
            w.would_open(root),
            Some(5),
            "the forced condition: a window would open"
        );
        assert_eq!(
            w.probe(&h.store).err(),
            Some(no_window_note(&HolderUnavailable::NotResponding {
                pid: Some(4343)
            }))
        );
        assert!(
            w.no_holder.load(Ordering::SeqCst),
            "a holder is asked again"
        );
        assert_eq!(w.would_open(root), None, "the question promises a window");
        let id = StoreId::parse(&h.store.id).unwrap();
        let key = DataKey::from_bytes(&[7; 32]);
        assert_eq!(w.after_prompt(root, &id, &key, VaultId([0; 16]), 5), None);
    }
}

#[cfg(test)]
mod window_tests {
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

    use apprafter_core::secrets::testing::locked_store;

    use super::*;
    use crate::holder::protocol::Refusal;

    fn io(kind: std::io::ErrorKind) -> HolderError {
        HolderError::Io(kind.into())
    }

    /// C3 and T59: the client's answer to each way a holder can fail to serve.
    #[test]
    fn every_refusal_has_its_reaction() {
        for (e, want) in [
            (
                HolderError::Refused(Refusal::NotThisTerminal),
                Reaction::Prompt,
            ),
            (HolderError::Refused(Refusal::WindowEnded), Reaction::Prompt),
            (
                HolderError::Refused(Refusal::WrongStore),
                Reaction::EndThenPrompt,
            ),
            (
                HolderError::Refused(Refusal::KeyMismatch),
                Reaction::EndThenPrompt,
            ),
            (
                HolderError::Refused(Refusal::Malformed),
                Reaction::EndThenPrompt,
            ),
            (
                HolderError::Refused(Refusal::Unsupported { holder_protocol: 2 }),
                Reaction::EndThenPrompt,
            ),
            (
                HolderError::Refused(Refusal::VaultDamaged),
                Reaction::EndThenPrompt,
            ),
            (
                HolderError::Refused(Refusal::CacheRefused {
                    reason: cli_core::secrets::UndecryptableWhy::NotForThisKey,
                    detail: String::new(),
                }),
                Reaction::EndThenPrompt,
            ),
            (HolderError::Unexpected, Reaction::EndThenPrompt),
            (HolderError::Spent, Reaction::EndThenPrompt),
            (io(std::io::ErrorKind::TimedOut), Reaction::Hung),
            (io(std::io::ErrorKind::WouldBlock), Reaction::Hung),
            (io(std::io::ErrorKind::UnexpectedEof), Reaction::Prompt),
            (io(std::io::ErrorKind::ConnectionReset), Reaction::Prompt),
        ] {
            assert_eq!(reaction(&e), want, "{e:?}");
        }
    }

    /// Rule 9 (critic S1): once this process may no longer ask a holder, `find` is not even
    /// called.
    #[test]
    fn a_process_that_prompted_never_asks_a_holder() {
        let flag = AtomicBool::new(false);
        assert_eq!(holder_for(&flag, || Some(7)), Some(7));
        flag.store(true, Ordering::SeqCst);
        assert_eq!(
            holder_for(&flag, || -> Option<u8> {
                panic!("a holder was asked after a prompt")
            }),
            None
        );
    }

    /// A unit-test binary never installs the window: its tests that run command code (with a
    /// context of the process environment) would otherwise leave it to every later test.
    #[test]
    fn a_unit_test_binary_installs_no_window() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = Context::for_desktop(dir.path().to_path_buf(), "http://127.0.0.1:1");
        install(HolderPlace::of(&ctx));
        assert!(WINDOW.get().is_none());
    }

    /// T59: a silent holder is named once, with its pid; a refusal that needs no `End` sends
    /// none (its client is never looked for again); either way the process is done with
    /// holders.
    #[test]
    fn a_silent_holder_is_named_and_a_foreign_one_is_not_ended() {
        let flag = AtomicBool::new(false);
        assert_eq!(
            react(
                &io(std::io::ErrorKind::TimedOut),
                Some(4242),
                &|| None,
                &flag
            )
            .as_deref(),
            Some(
                "No unlock window in this terminal: its holder (pid 4242) does not answer; \
                 `apprafter lock now` ends it. The next command asks again."
            )
        );
        assert!(flag.load(Ordering::SeqCst));
        let flag = AtomicBool::new(false);
        let no_end =
            || -> Option<HolderClient> { panic!("`End` sent to another terminal's holder") };
        assert_eq!(
            react(
                &HolderError::Refused(Refusal::NotThisTerminal),
                None,
                &no_end,
                &flag
            ),
            None
        );
        assert!(flag.load(Ordering::SeqCst));
    }

    static STARTS: AtomicU32 = AtomicU32::new(0);
    static MINUTES: AtomicU32 = AtomicU32::new(0);

    fn counted_start(
        _: &HolderPlace,
        _: &StoreBinding,
        _: &DataKey,
        _: VaultId,
        minutes: u32,
    ) -> Result<HolderStarted, HolderUnavailable> {
        STARTS.fetch_add(1, Ordering::SeqCst);
        MINUTES.store(minutes, Ordering::SeqCst);
        Ok(HolderStarted::Started { pid: 1 })
    }

    /// Rule 9, T56, T52, C30, T59: a prompt marks the process; a window of 0 minutes, or a CLI
    /// that leads its own session (the window would end with it), starts no holder and says
    /// nothing; 5 minutes start one and say C30's line; a holder of this terminal this process
    /// could not use (an older install's, a silent one) still holds the place, and is named once.
    #[test]
    fn a_prompt_marks_the_process_and_starts_a_window_only_where_one_lasts() {
        let dir = tempfile::tempdir().unwrap();
        let store = locked_store(dir.path(), &[("prod", None)]);
        let ctx = Context::for_desktop(dir.path().to_path_buf(), "http://127.0.0.1:1");
        let window = |leads: fn() -> bool, start: StartFn| Window {
            place: HolderPlace::of(&ctx),
            no_holder: Arc::new(AtomicBool::new(false)),
            hung: Arc::new(AtomicBool::new(false)),
            start,
            leads_session: leads,
        };
        let opened = |w: &Window, minutes: u32| {
            w.after_prompt(
                dir.path(),
                &store.store_id,
                store.data_key(),
                store.vault_id,
                minutes,
            )
        };
        let w = window(|| false, counted_start);
        assert_eq!(opened(&w, 0), None);
        assert!(w.no_holder.load(Ordering::SeqCst));
        assert_eq!(
            STARTS.load(Ordering::SeqCst),
            0,
            "0 minutes started a holder"
        );
        let led = window(|| true, counted_start);
        assert_eq!(opened(&led, 5), None);
        assert!(led.no_holder.load(Ordering::SeqCst));
        assert_eq!(
            STARTS.load(Ordering::SeqCst),
            0,
            "a CLI leading its session started a holder"
        );
        assert_eq!(opened(&w, 5), Some(opened_note(5)));
        assert_eq!(
            (
                STARTS.load(Ordering::SeqCst),
                MINUTES.load(Ordering::SeqCst)
            ),
            (1, 5)
        );
        let taken = window(
            || false,
            |_, _, _, _, _| Ok(HolderStarted::AlreadyRunning { pid: 4242 }),
        );
        assert_eq!(
            opened(&taken, 5),
            Some(no_window_note(&HolderUnavailable::NotResponding {
                pid: Some(4242)
            }))
        );
    }

    #[cfg(target_os = "linux")]
    const INNER: &str = "APPRAFTER_WINDOW_INNER";

    /// T57 (Linux): `harden_for_secrets` — which d's `Terminal::ask` calls before any prompt of
    /// the CLI reads — leaves the process not dumpable: in a child, since the flag cannot be set
    /// back, started with an environment of its own (critic-d4a-e finding 24). The forced
    /// condition: the child starts dumpable.
    #[cfg(target_os = "linux")]
    #[test]
    fn the_process_is_not_dumpable_once_hardened() {
        let out = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "holder_window::window_tests::inner_dumpable",
                "--ignored",
                "--nocapture",
                "--test-threads=1",
            ])
            .env_clear()
            .env(INNER, "1")
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(stdout.contains("dumpable: before=1 after=0"), "{stdout}");
    }

    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "runs only inside the_process_is_not_dumpable_once_hardened"]
    fn inner_dumpable() {
        assert!(
            std::env::var_os(INNER).is_some(),
            "run only by the_process_is_not_dumpable_once_hardened"
        );
        // SAFETY: PR_GET_DUMPABLE reads this process's flag.
        let get = || unsafe { libc::prctl(libc::PR_GET_DUMPABLE, 0, 0, 0, 0) };
        let before = get();
        harden_for_secrets();
        println!("dumpable: before={before} after={}", get());
    }
}

/// `HolderSession` against a fake holder on a socket of its own (C3, T48, T59).
#[cfg(all(test, unix))]
mod session_tests {
    use std::os::unix::net::UnixListener;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Mutex;

    use apprafter_core::lock::HolderHardening;
    use apprafter_core::secrets::read_store_file;
    use apprafter_core::secrets::testing::{locked_store, LockedTestStore};
    use apprafter_core::secrets::{OpenToken, VaultAccess};
    use apprafter_core::Zeroizing;
    use cli_core::secrets::{
        encrypt_for_recipient, CacheKeyKind, CachedSecret, Identity, UndecryptableWhy,
    };
    use cli_core::CliError;

    use super::*;
    use crate::holder::endpoint::{session_endpoint, Endpoint};
    use crate::holder::protocol::{
        read_request, write_reply, HolderStatus, Refusal, Reply, Request, PROTOCOL, VERSION,
    };

    /// The store's token: what this process opens itself.
    const TOKEN: &str = "fafafafafafafafafafafafafafafafafafafafafafafafafafafafafafafafa";
    /// What the fake holder serves: never the store's, so a served item shows where it came from.
    const SERVED: &str = "5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e";

    type Seen = Arc<Mutex<Vec<&'static str>>>;

    /// A fake holder in a directory of its own (`fake_at`).
    fn fake(replies: Vec<Option<Reply>>) -> (tempfile::TempDir, Endpoint, Seen) {
        let dir = tempfile::TempDir::new_in("/tmp").unwrap();
        let path = dir.path().join("h.sock");
        let seen = fake_at(&path, replies);
        (
            dir,
            Endpoint::Socket {
                path,
                lock: PathBuf::new(),
            },
            seen,
        )
    }

    /// A fake holder listening at `path`: connection N gets `replies[N]` (`None`: it takes the
    /// request and says nothing for 10 s); each request's kind is recorded before the reply
    /// goes out.
    fn fake_at(path: &Path, replies: Vec<Option<Reply>>) -> Seen {
        let listener = UnixListener::bind(path).unwrap();
        let seen: Seen = Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        std::thread::spawn(move || {
            for reply in replies {
                let Ok((mut conn, _)) = listener.accept() else {
                    return;
                };
                let kind = match read_request(&mut conn) {
                    Ok(Request::Status) => "status",
                    Ok(Request::OpenItem { .. }) => "item",
                    Ok(Request::OpenCache { .. }) => "cache",
                    Ok(Request::Recipient { .. }) => "recipient",
                    Ok(Request::End) => "end",
                    Err(_) => "garbage",
                };
                log.lock().unwrap().push(kind);
                match reply {
                    Some(r) => {
                        let _ = write_reply(&mut conn, &r);
                    }
                    None => std::thread::sleep(Duration::from_secs(10)),
                }
            }
        });
        seen
    }

    struct Fixture {
        _dir: tempfile::TempDir,
        _store: LockedTestStore,
        vault: Vec<u8>,
        owns: Arc<AtomicUsize>,
        no_holder: Arc<AtomicBool>,
        hung: Arc<AtomicBool>,
    }

    /// A session over the holder at `endpoint` whose own path opens the store with its key,
    /// counted.
    fn session(endpoint: Endpoint) -> (HolderSession, Fixture) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let store = locked_store(&root, &[("prod", Some(TOKEN))]);
        let vault = std::fs::read(root.join("vault")).unwrap();
        let owns = Arc::new(AtomicUsize::new(0));
        let no_holder = Arc::new(AtomicBool::new(false));
        let hung = Arc::new(AtomicBool::new(false));
        let file = read_store_file(&root).unwrap().unwrap();
        let access = store.access();
        let counted = owns.clone();
        let own_root = root.clone();
        let s = HolderSession {
            find: Box::new(move || HolderClient::at(&endpoint)),
            own: Some(Box::new(move || {
                counted.fetch_add(1, Ordering::SeqCst);
                access.session(&VaultRequest {
                    store_root: &own_root,
                    store_id: file.id.as_ref().unwrap(),
                    slots: &file.slots,
                })
            })),
            root,
            no_holder: no_holder.clone(),
            hung: hung.clone(),
        };
        (
            s,
            Fixture {
                _dir: dir,
                _store: store,
                vault,
                owns,
                no_holder,
                hung,
            },
        )
    }

    fn kinds(seen: &Seen) -> Vec<&'static str> {
        seen.lock().unwrap().clone()
    }

    /// T48: an item the holder serves is the holder's; this process opens nothing.
    #[test]
    fn a_served_item_is_the_holders() {
        let (_d, ep, seen) = fake(vec![Some(Reply::Item(Zeroizing::new(
            SERVED.as_bytes().to_vec(),
        )))]);
        let (s, f) = session(ep);
        assert_eq!(
            s.hetzner_token(&f.vault, "prod").unwrap().unwrap().expose(),
            SERVED
        );
        assert_eq!(kinds(&seen), ["item"]);
        assert_eq!(f.owns.load(Ordering::SeqCst), 0);
        assert!(!f.no_holder.load(Ordering::SeqCst));
    }

    /// C3: a holder that refuses for its own reasons is ended, and the item is opened here —
    /// its word on the vault is never believed; no holder is asked again (rule 9).
    #[test]
    fn a_refusing_holder_is_ended_and_the_item_opens_here() {
        for refusal in [
            Refusal::WrongStore,
            Refusal::KeyMismatch,
            Refusal::Malformed,
            Refusal::VaultDamaged,
            Refusal::Unsupported { holder_protocol: 2 },
        ] {
            let (_d, ep, seen) = fake(vec![
                Some(Reply::Refused(refusal.clone())),
                Some(Reply::Ended),
            ]);
            let (s, f) = session(ep);
            assert_eq!(
                s.hetzner_token(&f.vault, "prod").unwrap().unwrap().expose(),
                TOKEN
            );
            assert_eq!(
                s.hetzner_token(&f.vault, "prod").unwrap().unwrap().expose(),
                TOKEN
            );
            assert_eq!(kinds(&seen), ["item", "end"], "{refusal:?}");
            assert_eq!(f.owns.load(Ordering::SeqCst), 2, "{refusal:?}");
            assert!(f.no_holder.load(Ordering::SeqCst));
            assert!(
                !f.hung.load(Ordering::SeqCst),
                "an ended holder is taken as still holding the place: {refusal:?}"
            );
        }
    }

    /// C3: another terminal's holder, or one whose window just ended, is sent no `End`.
    #[test]
    fn not_this_terminal_and_an_ended_window_send_no_end() {
        for refusal in [Refusal::NotThisTerminal, Refusal::WindowEnded] {
            let (_d, ep, seen) = fake(vec![Some(Reply::Refused(refusal.clone()))]);
            let (s, f) = session(ep);
            assert_eq!(
                s.hetzner_token(&f.vault, "prod").unwrap().unwrap().expose(),
                TOKEN
            );
            assert_eq!(kinds(&seen), ["item"], "{refusal:?}");
            assert_eq!(f.owns.load(Ordering::SeqCst), 1);
        }
    }

    /// C3, rule 3 (critic-d4a-e finding 16): a holder's word that a cache is undecryptable is
    /// not believed — it may be a squatter's, steering the person to refresh a cache sealed to
    /// the squatter's recipient. The holder is ended and the cache opened here: a cache the
    /// store's key does not open is then this process's own verdict.
    #[test]
    fn a_holder_that_refuses_a_cache_is_ended_and_the_cache_opens_here() {
        let (_d, ep, seen) = fake(vec![
            Some(Reply::Refused(Refusal::CacheRefused {
                reason: UndecryptableWhy::NotForThisKey,
                detail: "no identity matched any of the recipients".into(),
            })),
            Some(Reply::Ended),
        ]);
        let (s, f) = session(ep);
        let foreign =
            encrypt_for_recipient("apiVersion: v1", &Identity::generate().to_public()).unwrap();
        match s.open_cache(&f.vault, CachedSecret::Kubeconfig, "prod", &foreign) {
            Err(CliError::CacheUndecryptable {
                reason,
                key,
                target,
                ..
            }) => assert_eq!(
                (reason, key, target.as_str()),
                (
                    UndecryptableWhy::NotForThisKey,
                    CacheKeyKind::StoreVault,
                    "prod"
                )
            ),
            Err(other) => panic!("{other:?}"),
            Ok(_) => panic!("an undecryptable cache opened"),
        }
        assert_eq!(kinds(&seen), ["cache", "end"]);
        assert_eq!(f.owns.load(Ordering::SeqCst), 1);
        assert!(f.no_holder.load(Ordering::SeqCst));
    }

    /// T66 (critic-d4a-e finding 5): a diagnostic never prompts. This terminal's window answers
    /// `Status`, then has ended by the time the item is asked (`WindowEnded`): `open_token` is
    /// `Locked`, and the diagnostic's session has no own path to take.
    #[test]
    fn a_diagnostic_whose_window_ends_under_it_is_locked_and_asks_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let locked = locked_store(&root, &[("prod", Some(TOKEN))]);
        let base_dir = tempfile::TempDir::new_in("/tmp").unwrap();
        let base = base_dir.path().join("apprafter");
        let store = binding(&root, locked.store_id.as_str());
        let Endpoint::Socket { path, .. } = session_endpoint(&base, &store).unwrap() else {
            unreachable!("Unix")
        };
        let status = HolderStatus {
            protocol: PROTOCOL,
            version: VERSION.into(),
            store_id: store.id.clone(),
            vault_id: locked.vault_id,
            left_secs: 60,
            minutes: 5,
            hardening: HolderHardening {
                non_dumpable: Some(true),
                deny_attach: None,
                core_off: true,
                memory_locked: true,
                process_dacl: None,
            },
        };
        let seen = fake_at(
            &path,
            vec![
                Some(Reply::Status(status)),
                Some(Reply::Refused(Refusal::WindowEnded)),
            ],
        );
        /// The CLI's `open_session`, over a window whose place is the fake's.
        struct Diagnostic(Window);
        impl std::fmt::Debug for Diagnostic {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("Diagnostic")
            }
        }
        impl VaultAccess for Diagnostic {
            fn session(&self, _: &VaultRequest<'_>) -> cli_core::Result<Arc<dyn VaultSession>> {
                panic!("a diagnostic asked for the vault's key (D.4a T66)")
            }
            fn write_key(
                &self,
                _: &VaultRequest<'_>,
            ) -> cli_core::Result<apprafter_core::secrets::KeySession> {
                panic!("a diagnostic asked for a write key (D.4a T66)")
            }
            fn open_session(
                &self,
                req: &VaultRequest<'_>,
            ) -> cli_core::Result<Option<Arc<dyn VaultSession>>> {
                Ok(self.0.session(req, false))
            }
        }
        let place = HolderPlace {
            base: Some(base),
            runtime_dir: root.join("run"),
        };
        let window = Window {
            place: place.clone(),
            no_holder: Arc::new(AtomicBool::new(false)),
            hung: Arc::new(AtomicBool::new(false)),
            start: |_, _, _, _, _| unreachable!("a diagnostic starts no holder"),
            leads_session: || false,
        };
        let ctx = Context::for_desktop(root.clone(), "http://127.0.0.1:1")
            .with_vault_access(Arc::new(Diagnostic(window)));
        assert!(matches!(
            ctx.secrets().open_token("prod").unwrap(),
            OpenToken::Locked
        ));
        assert_eq!(kinds(&seen), ["status", "item"]);
        // The two kinds of session, side by side: only a command's has an own path.
        let req = VaultRequest {
            store_root: &root,
            store_id: &locked.store_id,
            slots: &[],
        };
        let flag = || Arc::new(AtomicBool::new(false));
        let diagnostic =
            HolderSession::for_store(place.clone(), store.clone(), &req, flag(), flag(), false);
        assert!(
            diagnostic.own.is_none(),
            "a diagnostic's session could prompt"
        );
        let command = HolderSession::for_store(place, store, &req, flag(), flag(), true);
        assert!(command.own.is_some());
    }

    /// T66 (d4a-f's critic B, minor 13): a holder at this store's endpoint whose `Status` names
    /// another store is no window of this store — `window_state` reports the terminal's quiet
    /// state, as `Window::session` refuses it; one that names this store is the window.
    #[test]
    fn window_state_believes_only_its_own_stores_holder() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let locked = locked_store(&root, &[("prod", Some(TOKEN))]);
        let base_dir = tempfile::TempDir::new_in("/tmp").unwrap();
        let base = base_dir.path().join("apprafter");
        let store = binding(&root, locked.store_id.as_str());
        let Endpoint::Socket { path, .. } = session_endpoint(&base, &store).unwrap() else {
            unreachable!("Unix")
        };
        let status = |store_id: &str| HolderStatus {
            protocol: PROTOCOL,
            version: VERSION.into(),
            store_id: store_id.into(),
            vault_id: locked.vault_id,
            left_secs: 60,
            minutes: 5,
            hardening: HolderHardening {
                non_dumpable: Some(true),
                deny_attach: None,
                core_off: true,
                memory_locked: true,
                process_dacl: None,
            },
        };
        let other = if store.id == "0123456789abcdef" {
            "fedcba9876543210"
        } else {
            "0123456789abcdef"
        };
        let seen = fake_at(
            &path,
            vec![
                Some(Reply::Status(status(other))),
                Some(Reply::Status(status(&store.id))),
            ],
        );
        let place = HolderPlace {
            base: Some(base),
            runtime_dir: root.join("run"),
        };
        let ctx = Context::for_desktop(root.clone(), "http://127.0.0.1:1");
        assert_eq!(
            window_state(&ctx, &place, true).unwrap(),
            UnlockWindow::Closed,
            "another store's holder"
        );
        assert!(matches!(
            window_state(&ctx, &place, true).unwrap(),
            UnlockWindow::Open { left_secs: 60, .. }
        ));
        assert_eq!(kinds(&seen), ["status", "status"]);
    }

    /// T59: a holder that takes the request and says nothing is given up after the client's
    /// bounds, and the item is opened here.
    #[test]
    fn a_silent_holder_is_given_up_and_the_item_opens_here() {
        let (_d, ep, _seen) = fake(vec![None]);
        let (s, f) = session(ep);
        assert_eq!(
            s.hetzner_token(&f.vault, "prod").unwrap().unwrap().expose(),
            TOKEN
        );
        assert_eq!(f.owns.load(Ordering::SeqCst), 1);
        assert!(f.no_holder.load(Ordering::SeqCst));
        assert!(
            f.hung.load(Ordering::SeqCst),
            "a silent holder is not marked as holding the place"
        );
    }

    /// T59: a holder that takes the connection and says nothing at its `Status` is
    /// named, and from then on this process asks no holder, its question promises no window and
    /// its prompt starts none (the place is still held).
    #[test]
    fn a_holder_silent_at_its_status_is_named_and_promises_no_window() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let locked = locked_store(&root, &[("prod", Some(TOKEN))]);
        let base_dir = tempfile::TempDir::new_in("/tmp").unwrap();
        let base = base_dir.path().join("apprafter");
        let store = binding(&root, locked.store_id.as_str());
        let Endpoint::Socket { path, .. } = session_endpoint(&base, &store).unwrap() else {
            unreachable!("Unix")
        };
        let seen = fake_at(&path, vec![None]);
        let w = Window {
            place: HolderPlace {
                base: Some(base),
                runtime_dir: root.join("run"),
            },
            no_holder: Arc::new(AtomicBool::new(false)),
            hung: Arc::new(AtomicBool::new(false)),
            start: |_, _, _, _, _| unreachable!("a holder started over one that holds the place"),
            leads_session: || false,
        };
        assert_eq!(
            w.would_open(&root),
            Some(5),
            "the forced condition: a window would open"
        );
        let req = VaultRequest {
            store_root: &root,
            store_id: &locked.store_id,
            slots: &[],
        };
        assert!(w.session(&req, true).is_none());
        assert_eq!(kinds(&seen), ["status"]);
        assert!(
            w.no_holder.load(Ordering::SeqCst),
            "a holder is asked again"
        );
        assert_eq!(w.would_open(&root), None, "the question promises a window");
        assert_eq!(
            w.after_prompt(
                &root,
                &locked.store_id,
                locked.data_key(),
                locked.vault_id,
                5
            ),
            None
        );
    }

    /// T48, T53: a command's window is only the holder whose `Status` names this store:
    /// another store's holder at this store's endpoint serves nothing here; one that names this
    /// store is the window.
    #[test]
    fn a_window_is_only_the_holder_of_its_own_store() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let locked = locked_store(&root, &[("prod", Some(TOKEN))]);
        let base_dir = tempfile::TempDir::new_in("/tmp").unwrap();
        let base = base_dir.path().join("apprafter");
        let store = binding(&root, locked.store_id.as_str());
        let Endpoint::Socket { path, .. } = session_endpoint(&base, &store).unwrap() else {
            unreachable!("Unix")
        };
        let status = |store_id: &str| HolderStatus {
            protocol: PROTOCOL,
            version: VERSION.into(),
            store_id: store_id.into(),
            vault_id: locked.vault_id,
            left_secs: 60,
            minutes: 5,
            hardening: HolderHardening {
                non_dumpable: Some(true),
                deny_attach: None,
                core_off: true,
                memory_locked: true,
                process_dacl: None,
            },
        };
        let other = if store.id == "0123456789abcdef" {
            "fedcba9876543210"
        } else {
            "0123456789abcdef"
        };
        let seen = fake_at(
            &path,
            vec![
                Some(Reply::Status(status(other))),
                Some(Reply::Status(status(&store.id))),
            ],
        );
        let w = Window {
            place: HolderPlace {
                base: Some(base),
                runtime_dir: root.join("run"),
            },
            no_holder: Arc::new(AtomicBool::new(false)),
            hung: Arc::new(AtomicBool::new(false)),
            start: |_, _, _, _, _| unreachable!("a probe starts no holder"),
            leads_session: || false,
        };
        let req = VaultRequest {
            store_root: &root,
            store_id: &locked.store_id,
            slots: &[],
        };
        assert!(
            w.session(&req, true).is_none(),
            "another store's holder is this store's window"
        );
        assert!(w.session(&req, true).is_some());
        assert_eq!(kinds(&seen), ["status", "status"]);
    }

    /// T59, one naming per command (run E's residual): a holder that answers `Status` and then
    /// falls silent at an item is named by the item's session — and from then on the window
    /// knows it, as it knows one silent at its `Status`: the question promises no window and the
    /// prompt that follows starts none, so its start never meets that holder (`AlreadyRunning`)
    /// to name it a second time. A diagnostic's session, so the item is `store_locked` here
    /// rather than a prompt.
    #[test]
    fn a_holder_silent_at_an_item_is_named_once_and_promises_no_window() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let locked = locked_store(&root, &[("prod", Some(TOKEN))]);
        let base_dir = tempfile::TempDir::new_in("/tmp").unwrap();
        let base = base_dir.path().join("apprafter");
        let store = binding(&root, locked.store_id.as_str());
        let Endpoint::Socket { path, .. } = session_endpoint(&base, &store).unwrap() else {
            unreachable!("Unix")
        };
        let seen = fake_at(
            &path,
            vec![
                Some(Reply::Status(HolderStatus {
                    protocol: PROTOCOL,
                    version: VERSION.into(),
                    store_id: store.id.clone(),
                    vault_id: locked.vault_id,
                    left_secs: 60,
                    minutes: 5,
                    hardening: HolderHardening {
                        non_dumpable: Some(true),
                        deny_attach: None,
                        core_off: true,
                        memory_locked: true,
                        process_dacl: None,
                    },
                })),
                None,
            ],
        );
        let w = Window {
            place: HolderPlace {
                base: Some(base),
                runtime_dir: root.join("run"),
            },
            no_holder: Arc::new(AtomicBool::new(false)),
            hung: Arc::new(AtomicBool::new(false)),
            start: |_, _, _, _, _| unreachable!("a holder started over one that holds the place"),
            leads_session: || false,
        };
        let req = VaultRequest {
            store_root: &root,
            store_id: &locked.store_id,
            slots: &[],
        };
        let session = w
            .session(&req, false)
            .expect("the holder answers its Status");
        assert_eq!(
            w.would_open(&root),
            Some(5),
            "the forced condition: a window would open"
        );
        let vault = std::fs::read(root.join("vault")).unwrap();
        assert!(matches!(
            session.hetzner_token(&vault, "prod"),
            Err(CliError::StoreLocked { .. })
        ));
        assert_eq!(kinds(&seen), ["status", "item"]);
        assert!(
            w.no_holder.load(Ordering::SeqCst),
            "a holder is asked again"
        );
        assert_eq!(w.would_open(&root), None, "the question promises a window");
        assert_eq!(
            w.after_prompt(
                &root,
                &locked.store_id,
                locked.data_key(),
                locked.vault_id,
                5
            ),
            None,
            "the prompt names the holder again"
        );
    }
}
