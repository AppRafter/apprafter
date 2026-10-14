// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The Tauri layer: the [`Shell`] every command works on, the one lock gate, the page-load
//! hook, and the process lifecycle (tickers, quit).
//!
//! # The lock gate
//!
//! [`builder`] wraps the generated invoke handler: before any command runs, the wrapper asks
//! [`LockMachine::guard`] — an atomic read, never a wait — and refuses with
//! `apprafter::desktop::locked`. It is the only place the lock is enforced, so no command can
//! forget it. Plugin commands (`plugin:*`) never reach an app's invoke handler in Tauri 2;
//! the capability therefore grants no plugin command beyond `core:event:allow-listen` and
//! `core:event:allow-unlisten`, which carry no data out.
//!
//! # Where the shell comes from
//!
//! The settings live in the app's config directory, which only the built app resolves (it
//! follows the identifier and the data-directory override). So the app is built first with a
//! [`ShellCell`], and [`install`] puts the shell in it and in the app's state before the
//! event loop starts — before any window, so before any command. The gate reads the cell
//! without a lock.
//!
//! # Quitting
//!
//! Every way out the app sees coming — the `quit` command, the last window closing, the macOS
//! app menu's Quit ([`crate::menu`]), a quit signal ([`crate::signals`]), an exit request
//! from the OS — goes through [`quit`]: [`Shell::begin_quit`] closes the operation manager
//! (nothing new starts) and the lock machine (no unlock prompt opens), closes every open
//! prompt, and drops every plan; then a `quit` thread cancels the running operations, waits up
//! to [`STOP_BOUND`] for them to stop, and exits. The run loop's exit request is refused until
//! that thread is done ([`on_exit_requested`]). While it waits, the window stays, and the page
//! says that it is stopping them (`quitting`, with how many and that bound).
//!
//! Some exits the OS starts never ask: macOS ends an app with `terminate:` (the Dock's Quit, a
//! logout, a shutdown), Windows ends a session's apps with `WM_ENDSESSION`, and the event loop
//! exits with no exit request. [`Shell::on_exit`] runs the same quit there, with a wait of
//! [`FORCED_STOP_BOUND`] on the main thread: the process ends as it returns. A process killed
//! outright (`SIGKILL`, the Task Manager) runs nothing at all.
//!
//! # The OS session watch
//!
//! [`Shell::watch_session`] keeps the OS's lock and sleep signals (`apprafter_os_auth::watch`):
//! each one locks the app when `lockOnSleep` says so ([`LockMachine::os_session`]), on the
//! watch's own thread. The shell keeps the watch until a quit drops it, once the quit has waited
//! for the running operations ([`quit`]'s thread, or [`Shell::on_exit`]). Dropping it waits for
//! a callback in progress, and a callback takes the lock machine's lock and the operation
//! manager's: so it is dropped holding nothing, not even the shell's own hold on it.
//!
//! What the OS reports to the app (`AppInfo.sessionEvents`: the session's locks, sleeps, both or
//! neither) is known once the watch is set up, which can take a moment (on Linux it connects to
//! two buses). A thread asks the watch every [`SESSION_READY_POLL`] until it says, never
//! blocking the main thread; `app_info` (on the blocking pool) waits for that answer for up to
//! [`SESSION_READY_WITHIN`] from the watch's start rather than answer early, so the page's first
//! `app_info` normally has it and no event is needed. A watch that says later is still asked,
//! every [`SESSION_LATE_POLL`], and counts once it has said: a later `app_info` reports it.
//!
//! # A protected store (D.4a T74)
//!
//! The shell's context carries [`DesktopVaultAccess`]: on a protected store the app is unlocked
//! exactly while it holds the store's key. The lock hook drops the key on every lock, and so
//! does a quit. [`Shell::recheck`] re-reads `store.yaml` on every `app_info` and unlock, on
//! window focus, before `lock_info` and doctor, when a read finds no key, and when the idle
//! tick sees the file change ([`Shell::recheck_if_changed`]), and follows it: protected
//! without the key held, the app locks with `SecretsLocked`; back in files, the key goes and the
//! lock's settings are no longer pinned. The lock screen's field opens the vault there.

use std::io;
use std::panic::{self, AssertUnwindSafe};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::SeqCst};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock, PoisonError, Weak};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use apprafter_core::lock::limits::{
    DisplayKind, LimitClient, SessionEvents as CoreSessionEvents, SshFacts,
};
use apprafter_core::lock::{HolderHardening, UnlockWindow};
use apprafter_core::secrets::{VaultAccess, STORE_FILE};
use apprafter_core::{Context, CoreError, SecretString};
use apprafter_desktop_ipc::{
    AppInfo, LockReason, LockState, OpId, Os, PlanView, Quitting, SessionEvents, Settings,
    SubscriptionId, UnavailableReason, LOCK_CHANGED, QUITTING,
};
use apprafter_os_auth::{Listening, SessionEvent, SessionWatch};
use tauri::ipc::Invoke;
use tauri::webview::PageLoadEvent;
use tauri::{AppHandle, Emitter, ExitRequestApi, Manager, Runtime};

use zeroize::Zeroizing;

use crate::auth::Authenticator;
use crate::auth_cache::AuthCache;
use crate::commands;
use crate::drafts::DraftStore;
use crate::env::ToolSearchPath;
use crate::errors::{DesktopError, Refusal};
use crate::lock::{LockChange, LockHook, LockMachine};
use crate::lock_ops::LockOnTable;
use crate::ops::{
    panic_message, Clock, EventSink, Executor, OperationManager, PlanParts, SecretGesture,
};
use crate::protection::Protection;
use crate::secret_clip::{ClipboardMaker, SecretClipboard};
use crate::settings::SettingsStore;
use crate::theme::Appearance;
use crate::vault_access::{DesktopVaultAccess, OpenedSlot, OsPager, Pager, StoreSecretGesture};

/// How long a quit waits for cancelled operations to stop before it exits anyway: the CLI's
/// helper-pod stop bound.
pub const STOP_BOUND: Duration = Duration::from_secs(15);

/// How long an exit the OS forced waits for cancelled operations ([`Shell::on_exit`]): short,
/// since the OS is ending the process anyway and the wait blocks the main thread — Windows
/// gives a session's apps about five seconds once the session ends.
pub const FORCED_STOP_BOUND: Duration = Duration::from_secs(3);

/// How often the idle ticker asks the lock whether the idle time has passed.
pub const IDLE_TICK: Duration = Duration::from_secs(5);

/// How often the output flusher sends tool output that waited long enough (the reporter's
/// flush age).
pub const FLUSH_TICK: Duration = Duration::from_millis(crate::ops::reporter::FLUSH_AGE_MS);

/// What `app_info` says when the OS does not name the account or the host.
const UNKNOWN: &str = "unknown";

/// How long `app_info` waits for the authenticator's fresh answer before it gives the last one.
pub const APP_INFO_AUTH_WITHIN: Duration = Duration::from_secs(1);

/// How long `app_info` waits, from the OS session watch's start, for the watch to say what it
/// hears (`AppInfo.sessionEvents`); after it, `app_info` says what is known.
pub const SESSION_READY_WITHIN: Duration = Duration::from_secs(2);

/// How often the readiness thread asks the watch what it hears, within
/// [`SESSION_READY_WITHIN`].
pub const SESSION_READY_POLL: Duration = Duration::from_millis(20);

/// How often it asks a watch that has not said within [`SESSION_READY_WITHIN`].
pub const SESSION_LATE_POLL: Duration = Duration::from_millis(250);

/// What the OS session watch calls with each signal.
pub type SessionCallback = Box<dyn Fn(SessionEvent) + Send>;

/// What only the running app knows of itself, given to [`Shell::new_with`] (`lib.rs`); every
/// test's shell takes the default — nothing hardened, so nothing claimed.
pub struct ShellExtras {
    /// What the process did to protect itself before any secret (D.4a T77, `harden`).
    pub hardening: HolderHardening,
    /// The display the app's window is on, as GDK says on Linux (D.4a T72; `display`): what
    /// the limit rows say of what other programs see of the app's keystrokes.
    pub display: DisplayKind,
    /// The ssh-agent the limit rows ask (D.4a O1; `env::ssh_agent`).
    pub ssh_agent: Option<PathBuf>,
    /// Opens the OS clipboard for the recovery key's Copy (D.4a O12; `secret_clip`); `None`:
    /// no Copy here.
    pub clipboard: Option<ClipboardMaker>,
    /// Locks the held key's page in memory (D.4a T77; `vault_access`): the OS's, a test's.
    pub pager: Arc<dyn Pager>,
}

impl Default for ShellExtras {
    fn default() -> Self {
        ShellExtras {
            hardening: crate::harden::not_applied(),
            display: DisplayKind::None,
            ssh_agent: None,
            clipboard: None,
            pager: Arc::new(OsPager),
        }
    }
}

/// Held by a lock flow of the app's own — Protect, Finish, Stop protecting — while it runs:
/// taken by its executor's first statement, dropped at its end (D.4a T74). The store changes
/// as it runs, so the shell's re-read only reports meanwhile; the run's end re-reads. Never
/// from the plan: a confirm can stay open until the plans' sweep (20 minutes), and the
/// re-read must not be off for all of it (critic B finding 4). A run that panics skips its
/// end's re-read, so the guard's drop asks for one while the thread unwinds (the Rust
/// verifier's finding 11).
pub struct FlowGuard {
    flows: Arc<AtomicUsize>,
    shell: Weak<Shell>,
}

impl Drop for FlowGuard {
    fn drop(&mut self) {
        self.flows.fetch_sub(1, SeqCst);
        if thread::panicking() {
            if let Some(shell) = self.shell.upgrade() {
                shell.recheck_later();
            }
        }
    }
}

/// What the idle tick compares to notice the store changed under the app (D.4a T74; critic
/// B finding 19): `store.yaml`'s length and modification time, or its absence. A `stat`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreStamp(Option<(u64, Option<SystemTime>)>);

impl StoreStamp {
    pub fn read(root: &std::path::Path) -> StoreStamp {
        StoreStamp(
            std::fs::metadata(root.join(STORE_FILE))
                .ok()
                .map(|m| (m.len(), m.modified().ok())),
        )
    }
}

/// The OS session watch as the shell keeps it: `apprafter_os_auth::SessionWatch` in the app, a
/// stand-in in the tests. Dropping it stops the watch, waiting for a callback in progress.
pub trait SessionSource: Send {
    /// What the watch hears once it is set up, asked without waiting; `None` until then.
    fn listening(&self) -> Option<Listening>;
}

impl SessionSource for SessionWatch {
    fn listening(&self) -> Option<Listening> {
        SessionWatch::listening(self, Duration::ZERO)
    }
}

/// Where [`Shell`] keeps the session watch, and what it knows of it.
#[derive(Default)]
struct Session {
    state: Mutex<SessionState>,
    /// Notified when `readiness` leaves `Pending`.
    known: Condvar,
}

#[derive(Default)]
struct SessionState {
    watch: Option<Box<dyn SessionSource>>,
    readiness: Readiness,
    /// Until when `app_info` waits while `readiness` is `Pending`.
    wait_until: Option<Instant>,
    /// A quit dropped the watch: a later one is dropped at once.
    stopped: bool,
}

/// What the session watch hears.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum Readiness {
    /// No watch: none started, or the quit dropped it.
    #[default]
    None,
    /// Started; not set up yet.
    Pending,
    /// Set up, hearing this.
    Known(Listening),
}

impl Readiness {
    /// As the page is told.
    fn events(self) -> SessionEvents {
        match self {
            Readiness::Known(listening) => SessionEvents {
                lock: listening.lock,
                sleep: listening.sleep,
            },
            Readiness::None | Readiness::Pending => SessionEvents::default(),
        }
    }
}

impl Session {
    fn lock(&self) -> MutexGuard<'_, SessionState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Keeps `watch`, `app_info` waiting for what it hears until `wait_until`; or drops it,
    /// after a quit.
    fn keep(&self, watch: Box<dyn SessionSource>, wait_until: Instant) -> bool {
        let mut state = self.lock();
        if state.stopped {
            drop(state);
            drop(watch);
            return false;
        }
        state.watch = Some(watch);
        state.readiness = Readiness::Pending;
        state.wait_until = Some(wait_until);
        true
    }

    /// Asks the watch once, under the lock, held for the asking only so a quit's drop never
    /// waits for the readiness thread; `true` once there is nothing more to ask (it said, or it
    /// is gone).
    fn ask(&self) -> bool {
        let mut state = self.lock();
        if state.readiness != Readiness::Pending {
            return true;
        }
        let Some(listening) = state.watch.as_ref().and_then(|watch| watch.listening()) else {
            return false;
        };
        if listening.any() {
            tracing::info!(?listening, "the OS session watch listens");
        } else {
            tracing::warn!(
                "the OS reports neither the session's locks nor sleeps to the app here: \
                 lock-on-sleep has nothing to follow"
            );
        }
        state.readiness = Readiness::Known(listening);
        self.known.notify_all();
        true
    }

    /// The readiness thread's work: ask the watch every [`SESSION_READY_POLL`] until it says or
    /// it is dropped, and once `app_info` has stopped waiting for it (`wait_until`), every
    /// [`SESSION_LATE_POLL`].
    fn settle(&self) {
        let mut said_late = false;
        while !self.ask() {
            let late = self
                .lock()
                .wait_until
                .is_some_and(|until| Instant::now() >= until);
            if late && !said_late {
                said_late = true;
                tracing::warn!(
                    "the OS session watch is not set up after {SESSION_READY_WITHIN:?}: \
                     lock-on-sleep is shown as unavailable until it is"
                );
            }
            thread::sleep(if late {
                SESSION_LATE_POLL
            } else {
                SESSION_READY_POLL
            });
        }
    }

    /// `AppInfo.session_events`: while the watch has not said, waits for it until its
    /// `wait_until`; after that, what is known now.
    fn events(&self) -> SessionEvents {
        let mut state = self.lock();
        while state.readiness == Readiness::Pending {
            let left = state
                .wait_until
                .and_then(|until| until.checked_duration_since(Instant::now()))
                .unwrap_or_default();
            if left.is_zero() {
                break;
            }
            state = self
                .known
                .wait_timeout(state, left)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
        state.readiness.events()
    }

    /// Drops the watch, holding nothing: a callback in progress may take any of the shell's
    /// locks. No watch is kept from here on.
    fn stop(&self) {
        let watch = {
            let mut state = self.lock();
            state.stopped = true;
            state.readiness = Readiness::None;
            self.known.notify_all();
            state.watch.take()
        };
        if watch.is_some() {
            tracing::info!("the OS session watch stops");
        }
        drop(watch);
    }
}

/// Everything the commands work on, built once per process.
pub struct Shell {
    pub settings: SettingsStore,
    pub lock: Arc<LockMachine>,
    pub ops: Arc<OperationManager>,
    /// Verified tokens waiting for their add plan; dropped on every lock transition and on
    /// quit.
    pub drafts: Arc<DraftStore>,
    /// The authenticator, its answer about what it can do kept ([`AuthCache`]).
    pub auth: Arc<AuthCache>,
    /// The core's view of this process: the target store and the provider API. Its tool search
    /// path is what was known when the app started; what runs tools takes
    /// [`tool_context`](Self::tool_context) instead.
    pub context: Context,
    /// The tool search path as the app learns it: on macOS, the login shell's answer, asked from
    /// the start on a thread of its own ([`crate::env::tool_search_path`]).
    tools: ToolSearchPath,
    /// A test build (fake authentication): `app_info` tells the webview to say so.
    pub test_build: bool,
    /// Set by the first [`begin_quit`](Self::begin_quit).
    quitting: AtomicBool,
    /// Set once the quit has waited for the running operations: the exit may go through.
    drained: AtomicBool,
    /// Set when the event loop exits: the tickers stop.
    stop: Stop,
    /// The OS session watch ([`Shell::watch_session`]).
    session: Arc<Session>,
    /// The native window's theme ([`crate::theme`]): `theme_apply`'s, and on Linux the
    /// desktop's colour scheme under System.
    pub appearance: Arc<Appearance>,
    /// What only the running app knows of itself ([`ShellExtras`]).
    extras: ShellExtras,
    /// The store's key while the app holds it (D.4a T74, `vault_access`); the context's
    /// `VaultAccess` too.
    pub vault: Arc<DesktopVaultAccess>,
    /// The new recovery keys of the Protect plans waiting to run (D.4a T25, `lock_ops`).
    pub lock_ons: LockOnTable,
    /// The recovery key's Copy (D.4a O12, `secret_clip`).
    pub clipboard: Arc<SecretClipboard>,
    /// Lock flows of the app's own while they run ([`FlowGuard`]).
    flows: Arc<AtomicUsize>,
    /// Re-reads asked of [`Shell::recheck_later`] that its thread has not answered yet; 0
    /// while none runs.
    recheck_asks: AtomicUsize,
    /// One re-read acts at a time ([`Shell::recheck`]): a stale read never acts after a newer
    /// one (the Rust verifier's finding 10).
    rechecks: Mutex<()>,
    /// `store.yaml` as the idle tick last saw it ([`Shell::recheck_if_changed`]).
    store_seen: Mutex<StoreStamp>,
}

/// The tickers' stop signal: a ticker waits on it between ticks, so stopping wakes it at once.
/// An idle ticker waits on it too, for work ([`nudge`](Self::nudge)).
///
/// Its lock comes before the [`OperationManager`]'s: a waiter's `ready` may take the
/// manager's lock under it, so nothing may nudge or stop while holding the manager's lock.
#[derive(Default)]
struct Stop {
    stopped: Mutex<bool>,
    wake: Condvar,
}

impl Stop {
    /// Wait `every`, or until stopped; `true` once stopped.
    fn wait(&self, every: Duration) -> bool {
        self.wait_until(every, || false)
    }

    /// Wait until `ready` holds or `backstop` has passed, or until stopped; `true` once
    /// stopped. `ready` is asked under this lock, first and on every wake, so a
    /// [`nudge`](Self::nudge) after a change it reads is never missed.
    fn wait_until(&self, backstop: Duration, ready: impl Fn() -> bool) -> bool {
        let stopped = self.stopped.lock().unwrap_or_else(PoisonError::into_inner);
        let (stopped, _) = self
            .wake
            .wait_timeout_while(stopped, backstop, |stopped| !*stopped && !ready())
            .unwrap_or_else(PoisonError::into_inner);
        *stopped
    }

    /// Something a waiter's `ready` reads changed: every waiter asks again. Under the lock,
    /// so the wake cannot fall between a waiter's asking and its waiting.
    fn nudge(&self) {
        let _held = self.stopped.lock().unwrap_or_else(PoisonError::into_inner);
        self.wake.notify_all();
    }

    fn stop(&self) {
        *self.stopped.lock().unwrap_or_else(PoisonError::into_inner) = true;
        self.wake.notify_all();
    }
}

impl Shell {
    /// The shell over `settings`, its lock starting as they say. Every lock transition — to
    /// locked and to unlocked — drops every pending plan, cancels every running read, drops
    /// every verified token waiting as a draft, then ends every operation subscription, then
    /// calls `on_lock_change` with the new state (the app emits [`LOCK_CHANGED`]). All five run
    /// under the lock machine's lock: none may call back into it. An update — the same lock
    /// reported anew, its idle time changed — only calls `on_lock_change`: the page hears of
    /// it, and its plans, reads, drafts and subscriptions stay.
    ///
    /// A read stops on a lock where a confirmed plan runs on: nobody confirmed it, and a
    /// locked page cannot follow it. A draft goes for the reason a plan does: no token verified
    /// before the lock may be used after it.
    ///
    /// A lock ends the subscriptions because a locked page must receive nothing, and it
    /// cannot unsubscribe (the gate refuses `op_unsubscribe`): the shell unmounts on a lock and
    /// subscribes again after the unlock, from the replay, so a sink kept across the lock
    /// would show every later event twice. The operations themselves run on.
    ///
    /// Both go on an unlock too: a plan registered, or a subscription made, by a command that
    /// passed the guard just before a lock lands after the lock's sweep, and would otherwise
    /// survive it — a plan executable after the lock, a sink that doubles the page's output.
    /// Nothing else can subscribe while locked, and the page subscribes again only once it
    /// hears of the unlock, after this ran.
    ///
    /// `auth` is kept behind an [`AuthCache`], which the lock reads instead of the OS. It is
    /// given the settings first ([`Authenticator::apply_settings`]: Windows' `hello`), and again
    /// after every save. `tools` is where the tools are looked for ([`tool_context`]).
    ///
    /// [`tool_context`]: Self::tool_context
    pub fn new(
        settings: SettingsStore,
        auth: Arc<dyn Authenticator>,
        clock: Arc<dyn Clock>,
        context: Context,
        tools: ToolSearchPath,
        test_build: bool,
        on_lock_change: impl Fn(&LockState) + Send + Sync + 'static,
    ) -> Arc<Self> {
        Self::new_with(
            settings,
            auth,
            clock,
            context,
            tools,
            test_build,
            ShellExtras::default(),
            on_lock_change,
        )
    }

    /// [`new`](Self::new), with what only the running app knows of itself (`extras`).
    #[allow(clippy::too_many_arguments)]
    pub fn new_with(
        settings: SettingsStore,
        auth: Arc<dyn Authenticator>,
        clock: Arc<dyn Clock>,
        context: Context,
        tools: ToolSearchPath,
        test_build: bool,
        extras: ShellExtras,
        on_lock_change: impl Fn(&LockState) + Send + Sync + 'static,
    ) -> Arc<Self> {
        let ops = OperationManager::new(clock.clone());
        let drafts = Arc::new(DraftStore::new(clock.clone()));
        // D.4a T74: the key the app holds, behind the core's door to the secrets.
        let vault = DesktopVaultAccess::with_pager(clock.clone(), extras.pager.clone());
        let clipboard = SecretClipboard::new(extras.clipboard.clone(), clock.clone());
        let context = context.with_vault_access(vault.clone());
        let protection = Protection::read(context.config_root(), context.home_dir());
        let store_seen = Mutex::new(StoreStamp::read(context.config_root()));
        let hook: LockHook = {
            let (ops, drafts, vault, clipboard) = (
                ops.clone(),
                drafts.clone(),
                vault.clone(),
                clipboard.clone(),
            );
            Box::new(move |state: &LockState, change| {
                if change == LockChange::Transition {
                    // Every lock drops the store's key, before the page hears of it.
                    if state.locked {
                        vault.forget();
                        // The recovery key, if it is still on the clipboard (O12).
                        clipboard.clear_soon();
                    }
                    ops.drop_all_plans();
                    ops.cancel_reads();
                    drafts.drop_all();
                    ops.drop_all_subscribers();
                }
                on_lock_change(state);
            })
        };
        let auth = AuthCache::new(auth);
        auth.apply_settings(&settings.get());
        let lock = LockMachine::new_protected(
            settings.get(),
            auth.clone(),
            clock,
            hook,
            protection.protected,
        );
        let appearance = Arc::new(Appearance::new(settings.get().theme));
        let shell = Arc::new(Self {
            settings,
            lock,
            ops,
            drafts,
            auth,
            context,
            tools,
            test_build,
            quitting: AtomicBool::new(false),
            drained: AtomicBool::new(false),
            stop: Stop::default(),
            session: Arc::default(),
            appearance,
            extras,
            store_seen,
            vault,
            lock_ons: LockOnTable::default(),
            clipboard,
            flows: Arc::default(),
            recheck_asks: AtomicUsize::new(0),
            rechecks: Mutex::new(()),
        });
        // A read that finds no key: the store may have been locked under the app.
        let weak = Arc::downgrade(&shell);
        shell.vault.on_missing(move || {
            if let Some(shell) = weak.upgrade() {
                shell.recheck();
            }
        });
        // O4 between ticks (Task 14): every secret's use looks for a sleep the OS did not
        // report first, so the first read after the wake is refused.
        let weak = Arc::downgrade(&shell);
        shell.vault.before_use(move || {
            if let Some(shell) = weak.upgrade() {
                shell.lock.check_sleep();
            }
        });
        // A change waiting for the store's lock goes on while the app is unlocked, and is
        // abandoned, nothing written, once it locks or quits (the final verifier's finding
        // 12; Tasks 10a, 13).
        let weak = Arc::downgrade(&shell);
        shell.vault.may_wait(move || {
            weak.upgrade()
                .is_some_and(|shell| !shell.quitting.load(SeqCst) && !shell.lock.is_locked())
        });
        shell
    }

    /// The `app_info` answer. Its `auth` is asked of the authenticator afresh, waiting up to
    /// [`APP_INFO_AUTH_WITHIN`] for it and else the last answer: it changes (a polkit dialog
    /// that found no agent moves Linux to the password field, a lock moves it back), and before
    /// any answer it is [`UNANSWERED`](crate::auth_cache::UNANSWERED). Its `session_events`
    /// waits, up to [`SESSION_READY_WITHIN`] from the watch's start, while the watch has not said
    /// what it hears (see the module docs): call it on a blocking worker, never on the main
    /// thread.
    pub fn app_info(&self) -> AppInfo {
        AppInfo {
            os: current_os(),
            desktop_version: env!("CARGO_PKG_VERSION").into(),
            core_version: apprafter_core::VERSION.into(),
            // D.4a T74: the shell's re-read, which also pins, locks and forgets.
            secrets: self.recheck().info(),
            account: whoami::username().unwrap_or_else(|_| UNKNOWN.into()),
            host: whoami::hostname().unwrap_or_else(|_| UNKNOWN.into()),
            auth: self
                .auth
                .fresh(APP_INFO_AUTH_WITHIN)
                .unwrap_or_else(|| self.auth.info()),
            session_events: self.session.events(),
            test_build: self.test_build,
            settings_notice: self.settings.notice(),
        }
    }

    /// Re-read where the store keeps its secrets, and follow it (D.4a T74): protected
    /// without the key held — locked from a terminal under the app, or re-locked with a new
    /// id — the key goes and the app locks with `SecretsLocked`; back in files, the key goes
    /// and the pins lift. Protection itself is followed even while a lock flow of the app's
    /// own runs ([`FlowGuard`]): the pins and the unlock's route then match the store, so the
    /// OS cannot open a store the flow just protected (the final verifier's finding 10); only
    /// the key's drop and the lock wait for the run, which re-reads as it ends. On a
    /// protected store the decision is taken under the machine's lock
    /// (`lock_unlocked_unless`), which an unlock's commit holds too, and the key is kept or
    /// dropped in one step (`keep_only`): a key an unlock is keeping is never dropped by a
    /// re-read, nor the app it opened re-locked (finding 8). On `app_info`, on window focus,
    /// before `lock_info` and doctor, before Protect is planned, and when a read finds no
    /// key; a file read, never on the main thread. One at a time. A key goes only when one is
    /// held: a re-read of an unchanged store moves nothing, so a Protect plan waiting for its
    /// confirm keeps its key at the end (T64).
    pub fn recheck(&self) -> Protection {
        let _one = self.rechecks.lock().unwrap_or_else(PoisonError::into_inner);
        let now = Protection::read(self.context.config_root(), self.context.home_dir());
        self.lock.set_protected(now.protected);
        if self.flows.load(SeqCst) > 0 {
            return now;
        }
        if now.protected {
            let id = now.id.clone();
            self.lock
                .lock_unlocked_unless(LockReason::SecretsLocked, || {
                    id.as_ref().is_some_and(|id| self.vault.keep_only(id))
                });
        } else {
            self.vault.drop_key();
        }
        now
    }

    /// [`recheck`](Self::recheck) on a thread of its own: the window gained the focus, or a
    /// lock flow's run panicked ([`FlowGuard`]). One thread at a time, and no ask is lost: an
    /// ask that lands while the thread re-reads makes it read once more. (A panicking run's ask
    /// can land just after a focus re-read found the run still going and acted on nothing; a
    /// flag that dropped it would leave a store the run protected unfollowed.)
    pub fn recheck_later(self: &Arc<Self>) {
        if self.recheck_asks.fetch_add(1, SeqCst) > 0 {
            return;
        }
        let shell = self.clone();
        let spawned = thread::Builder::new()
            .name("store-recheck".into())
            .spawn(move || loop {
                let asked = shell.recheck_asks.load(SeqCst);
                if let Err(payload) = panic::catch_unwind(AssertUnwindSafe(|| shell.recheck())) {
                    tracing::error!(
                        "the target store's re-read panicked: {}",
                        panic_message(&*payload)
                    );
                }
                // Done only when no ask came in since this re-read began.
                if shell
                    .recheck_asks
                    .compare_exchange(asked, 0, SeqCst, SeqCst)
                    .is_ok()
                {
                    break;
                }
            });
        if let Err(e) = spawned {
            self.recheck_asks.store(0, SeqCst);
            tracing::warn!("no thread to re-read the target store on ({e})");
        }
    }

    /// A lock flow of the app's own starts to run (its executor's first statement): the
    /// re-read only reports until the guard goes.
    pub fn flow(self: &Arc<Self>) -> FlowGuard {
        self.flows.fetch_add(1, SeqCst);
        FlowGuard {
            flows: self.flows.clone(),
            shell: Arc::downgrade(self),
        }
    }

    /// Keep a plan ([`OperationManager::register_plan`]). On a protected store with no OS
    /// authenticator a destructive plan's gesture is the store's secret (D.4a T76), typed into
    /// the confirm dialog's field and opened against the vault, where D.2 refused the plan. A
    /// plan that only `requires_gesture` keeps D.2's rule until its slice decides (critic B
    /// finding 24): the manager gives the store's secret to a destructive plan only
    /// ([`OperationManager::register_plan_with`]), so that rule has one place.
    pub fn register_plan(&self, parts: PlanParts, exec: Executor) -> PlanView {
        let secret = if self.lock.protected() && !self.auth.info().available {
            self.store_secret_gesture(None)
        } else {
            None
        };
        self.ops.register_plan_with(parts, exec, secret)
    }

    /// Keep a plan whose gesture, when it needs one, is `secret` ([`register_plan`]).
    ///
    /// [`register_plan`]: Self::register_plan
    pub fn register_plan_with(
        &self,
        parts: PlanParts,
        exec: Executor,
        secret: Option<Arc<dyn SecretGesture>>,
    ) -> PlanView {
        self.ops.register_plan_with(parts, exec, secret)
    }

    /// The store's secret as a gesture over the store as it is now; what it opens goes to
    /// `slot` when given. `None` when there is no vault to open (in files, or `store.yaml`
    /// unreadable).
    pub fn store_secret_gesture(&self, slot: Option<OpenedSlot>) -> Option<Arc<dyn SecretGesture>> {
        let now = Protection::read(self.context.config_root(), self.context.home_dir());
        let id = now.id?;
        Some(Arc::new(StoreSecretGesture::new(
            self.vault.clone(),
            self.context.config_root().to_path_buf(),
            id,
            slot,
        )))
    }

    /// The app's unlock window (D.4a T73): `App` while it holds the protected store's key,
    /// else `Closed`.
    pub fn window(&self, now: &Protection) -> UnlockWindow {
        let held = now.id.as_ref().is_some_and(|id| self.vault.holds(id));
        if now.protected && held {
            UnlockWindow::App
        } else {
            UnlockWindow::Closed
        }
    }

    /// What the lock's limit rows are told of this app (D.4a T72, T73), made later — on a
    /// read's own thread: the session watch's answer may wait. The fetch key is the default
    /// `~/.ssh/id_ed25519` (the desktop reads no `APPRAFTER_SSH_PRIVATE_KEY`, ADR 0067 §2).
    pub fn limit_client_later(&self) -> impl FnOnce() -> LimitClient + Send + 'static {
        let events = self.session_events_later();
        let display = self.extras.display;
        let ssh = SshFacts {
            key: self
                .context
                .home_dir()
                .map(|home| home.join(".ssh").join("id_ed25519")),
            agent: self.extras.ssh_agent.clone(),
        };
        move || {
            let events = events();
            LimitClient::Desktop {
                session_events: CoreSessionEvents {
                    lock: events.lock,
                    sleep: events.sleep,
                },
                display,
                ssh,
            }
        }
    }

    /// The idle tick's look at the store (critic B finding 19): when `store.yaml` changed
    /// since the last look — `lock on`, `lock off` or a re-lock from a terminal — a full
    /// [`recheck`](Self::recheck). A `stat` every [`IDLE_TICK`].
    pub fn recheck_if_changed(&self) {
        let now = StoreStamp::read(self.context.config_root());
        let changed = {
            let mut seen = self
                .store_seen
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            let changed = *seen != now;
            *seen = now;
            changed
        };
        if changed {
            self.recheck();
        }
    }

    /// The lock screen's field on a protected store (D.4a T75): the lock password or the
    /// recovery key opens the vault through the gate, and its key is kept under the lock
    /// machine's lock just before the unlock. `now` is the re-read the caller just made.
    /// Back in files, the field's secret opens nothing (`use_system_prompt`: the OS is the
    /// way again); an unreadable `store.yaml` is refused with why.
    fn unlock_with_store_secret(
        &self,
        now: Protection,
        secret: Zeroizing<String>,
    ) -> Result<LockState, Refusal> {
        let Some(id) = now.id.clone() else {
            let root = self.context.config_root();
            let error = match apprafter_core::secrets::read_store_file(root) {
                Err(e) => DesktopError::Core(CoreError::Cli(e)),
                Ok(_) => DesktopError::AuthUnavailable {
                    reason: UnavailableReason::UseSystemPrompt,
                },
            };
            return Err(error.into());
        };
        let root = self.context.config_root().to_path_buf();
        let secret = SecretString::from(secret);
        let kept = id.clone();
        self.lock.unlock_with_store_secret(
            |_| self.vault.open_typed(&root, &id, secret),
            |opened| self.vault.keep(kept, opened),
        )?;
        // `id` came from a re-read made before a 0.7 s derivation: a terminal's re-lock
        // meanwhile would leave a key for an id the store no longer has. Read again (the
        // Rust verifier's finding 10).
        self.recheck();
        Ok(self.lock.state())
    }

    /// What the app's own process did to protect the store's key (D.4a T77), for `lock_info`.
    pub fn app_hardening(&self) -> HolderHardening {
        self.extras.hardening
    }

    /// What the OS reports of the session (`AppInfo.sessionEvents`), for doctor's
    /// `sleep_unreported` row (D.4a T72, G11), as a call to make later: it waits as `app_info`
    /// does while the session watch has not said (up to `SESSION_READY_WITHIN`), so a read
    /// makes it on its worker, never on the command's thread.
    pub fn session_events_later(&self) -> impl FnOnce() -> SessionEvents + Send + 'static {
        let session = self.session.clone();
        move || session.events()
    }

    /// Save `settings` and apply them, through the lock machine (which refuses switching the
    /// lock on with nothing to verify the owner), and give the authenticator the saved ones;
    /// the settings now in use. The authenticator hears of them within the machine's settings
    /// write, so concurrent saves reach it in the order they reach the file.
    pub fn set_settings(&self, settings: Settings) -> Result<Settings, DesktopError> {
        self.lock.set_settings(settings, |new| {
            self.settings.set(new.clone())?;
            self.auth.apply_settings(new);
            Ok(())
        })?;
        Ok(self.settings.get())
    }

    /// Lock now; the state that results — unlocked still when the lock is not in effect.
    pub fn lock_now(&self) -> LockState {
        self.lock.lock(LockReason::Manual);
        self.lock.state()
    }

    /// Ask the owner and unlock (blocks while the OS prompt is open); the state that results.
    pub fn unlock(&self) -> Result<LockState, DesktopError> {
        // D.4a T74 (critic B finding 19): the store may have been locked from a terminal
        // while the app was locked; then the OS opens nothing.
        self.recheck();
        self.lock.unlock()?;
        Ok(self.lock.state())
    }

    /// Unlock with the password from the lock screen's own field (blocks while it is checked);
    /// the state that results. On a protected store the field's text is the lock password or
    /// the recovery key, opened against the vault (D.4a T75).
    pub fn unlock_with_password(&self, password: Zeroizing<String>) -> Result<LockState, Refusal> {
        // D.4a T74, T75: re-read first (the store may have changed while the app was
        // locked); on a protected store the field takes the store's secret, on every OS.
        let now = self.recheck();
        if self.lock.protected() {
            return self.unlock_with_store_secret(now, password);
        }
        self.lock.unlock_with_password(password)?;
        Ok(self.lock.state())
    }

    /// Run plan `id`, `sink` following it from before the prompt. The subscription it holds,
    /// for `op_unsubscribe`; on an error the sink is unsubscribed again, so a busy prompt's
    /// retry does not leave the first attempt's sink behind. A started operation wakes the
    /// output flusher, which sleeps while nothing runs.
    pub fn execute(
        &self,
        id: OpId,
        sink: Arc<dyn EventSink>,
    ) -> Result<SubscriptionId, DesktopError> {
        self.execute_with(id, sink, None)
            .map_err(|refusal| *refusal.error)
    }

    /// [`execute`](Self::execute), the gesture checking `password` from the confirm dialog's
    /// own field when it is given ([`OperationManager::execute_with`]).
    pub fn execute_with(
        &self,
        id: OpId,
        sink: Arc<dyn EventSink>,
        password: Option<Zeroizing<String>>,
    ) -> Result<SubscriptionId, Refusal> {
        let subscription = self.ops.subscribe(id, sink)?.subscription;
        match self.ops.execute_with(id, &*self.auth, password) {
            Ok(_) => {
                self.stop.nudge();
                Ok(subscription)
            }
            Err(e) => {
                self.ops.unsubscribe(id, subscription);
                Err(e)
            }
        }
    }

    /// The core's context for work that runs tools (the toolchain, doctor): the tool search path
    /// as the app knows it, waiting the first time for the macOS login shell's answer — at most
    /// until [`TOOL_PATH_WAIT`](crate::env::TOOL_PATH_WAIT) has passed since the app asked
    /// ([`ToolSearchPath::get`]). Call it on a blocking worker or an operation's thread.
    pub fn tool_context(&self) -> Context {
        self.tool_context_later()()
    }

    /// [`tool_context`](Self::tool_context), made later by the closure this returns — on the
    /// thread of the work that runs the tools (a doctor read), so whoever starts that work never
    /// waits for the tool search path.
    pub fn tool_context_later(&self) -> impl FnOnce() -> Context + Send + 'static {
        let (context, tools) = (self.context.clone(), self.tools.clone());
        move || {
            let (path, source) = tools.get();
            context.with_tool_search_path(path, source)
        }
    }

    /// Start a read ([`OperationManager::start`]) and wake the output flusher, which sleeps
    /// while nothing runs. `Closing` once a quit began.
    pub fn start_read(
        &self,
        title: impl Into<String>,
        target: Option<String>,
        exec: Executor,
    ) -> Result<OpId, DesktopError> {
        let id = self.ops.start(title, target, exec)?;
        self.stop.nudge();
        Ok(id)
    }

    /// The first step of a quit, once: no operation starts and no unlock prompt opens from
    /// here on (`Closing`), the open unlock prompt and every operation prompt are closed, and
    /// every pending plan and every draft is dropped. `false` when a quit had already begun.
    pub fn begin_quit(&self) -> bool {
        if self.quitting.swap(true, SeqCst) {
            return false;
        }
        self.ops.close();
        self.lock.close();
        self.ops.drop_all_plans();
        self.drafts.drop_all();
        // D.4a T74: the quit drops the store's key too.
        self.vault.forget();
        // The recovery key off the clipboard, never waited for here (the main thread).
        self.clipboard.clear_soon();
        true
    }

    /// Watch the OS's lock and sleep signals: `start` (`apprafter_os_auth::watch` in the app)
    /// is called here with the callback, which locks as `lockOnSleep` says
    /// ([`LockMachine::os_session`]); the shell keeps what it returns until a quit drops it, and
    /// a thread of its own (`session-ready`) learns whether it listens (see the module docs).
    /// Call it once. After a quit the watch is dropped at once.
    pub fn watch_session(&self, start: impl FnOnce(SessionCallback) -> Box<dyn SessionSource>) {
        self.watch_session_within(start, SESSION_READY_WITHIN);
    }

    /// [`watch_session`](Self::watch_session), `app_info` waiting for the watch for up to
    /// `within`.
    fn watch_session_within(
        &self,
        start: impl FnOnce(SessionCallback) -> Box<dyn SessionSource>,
        within: Duration,
    ) {
        let lock = self.lock.clone();
        let wait_until = Instant::now() + within;
        let watch = start(Box::new(move |event| {
            tracing::info!(?event, "the OS session locked or is going to sleep");
            lock.os_session();
        }));
        if !self.session.keep(watch, wait_until) {
            return;
        }
        let session = self.session.clone();
        let spawned = thread::Builder::new()
            .name("session-ready".into())
            .spawn(move || session.settle());
        if let Err(e) = spawned {
            // No waiting here (this may be the main thread): what the watch says now is all
            // the app learns.
            tracing::warn!("no thread to learn what the OS session watch hears ({e})");
            self.session.ask();
        }
    }

    /// Drop the OS session watch (see the module docs): the quit calls it once the running
    /// operations have stopped. Never while holding a lock the watch's callback takes.
    pub fn stop_session_watch(&self) {
        self.session.stop();
    }

    /// The event loop is exiting (`RunEvent::Exit`, on the main thread): the process ends once
    /// this returns.
    ///
    /// When the quit has not waited for the running operations, the exit never went through
    /// [`quit`]: the OS ended the app without asking (see the module docs). The quit begins
    /// here, every subscription ends (the webview goes with the process), and this cancels the
    /// running operations and waits for them up to [`FORCED_STOP_BOUND`]. Either way the OS
    /// session watch is dropped, if the quit has not dropped it yet, and the tickers stop.
    pub fn on_exit(&self) {
        if !self.drained.load(SeqCst) {
            tracing::info!("the OS is ending the app: stopping the running operations");
            self.begin_quit();
            self.ops.drop_all_subscribers();
            if !self.ops.cancel_all_and_wait(FORCED_STOP_BOUND) {
                tracing::warn!(
                    running = self.ops.running(),
                    "operations still running {FORCED_STOP_BOUND:?} after the exit cancelled \
                     them; exiting"
                );
            }
        }
        // The recovery key off the clipboard: on Windows and macOS it would outlive the
        // process. On a thread of its own, waited for at most its bound.
        self.clipboard
            .clear_within(crate::secret_clip::CLEAR_ON_QUIT);
        self.stop_session_watch();
        self.stop_tickers();
    }

    /// The event loop exited: the tickers stop now.
    pub fn stop_tickers(&self) {
        self.stop.stop();
    }
}

fn current_os() -> Os {
    if cfg!(target_os = "windows") {
        Os::Windows
    } else if cfg!(target_os = "macos") {
        Os::Macos
    } else {
        Os::Linux
    }
}

/// Where the gate and the page-load hook find the shell: set once by [`install`], read
/// without a lock.
#[derive(Clone, Default)]
pub struct ShellCell(Arc<OnceLock<Arc<Shell>>>);

impl ShellCell {
    fn get(&self) -> Option<&Arc<Shell>> {
        self.0.get()
    }
}

/// `base` (`tauri::Builder::default()` in the app, the mock builder in tests) with every
/// command behind the lock gate, and the page-load hook that drops a reloaded page's
/// subscriptions. The shell comes later, through `cell` ([`install`]); until then every
/// command is refused as an internal error.
pub fn builder<R: Runtime>(base: tauri::Builder<R>, cell: ShellCell) -> tauri::Builder<R> {
    let handler: Box<dyn Fn(Invoke<R>) -> bool + Send + Sync> = Box::new(tauri::generate_handler![
        commands::app_info,
        commands::settings_get,
        commands::settings_set,
        commands::lock_status,
        commands::lock_now,
        commands::unlock,
        commands::unlock_with_password,
        commands::activity,
        commands::quit,
        commands::op_list,
        commands::op_subscribe,
        commands::op_unsubscribe,
        commands::op_cancel,
        commands::op_discard,
        commands::op_execute,
        commands::window_ready,
        commands::theme_apply,
        commands::target_list,
        commands::target_show,
        commands::ssh_key_candidates,
        commands::ssh_key_inspect,
        commands::toolchain_status,
        commands::whoami,
        commands::op_start_verify_token,
        commands::op_start_machine_catalogue,
        commands::op_start_region_latencies,
        commands::op_start_doctor,
        commands::op_start_whoami,
        commands::op_plan_target_add,
        commands::op_plan_target_renew,
        commands::op_plan_target_use,
        commands::op_plan_target_rename,
        commands::op_plan_target_remove,
        commands::op_plan_target_machine,
        commands::target_draft_discard,
        commands::lock_info,
        commands::op_plan_lock_on,
        commands::op_confirm_recovery_key,
        commands::copy_recovery_key,
        commands::op_recovery_key_saved,
        commands::op_plan_lock_finish,
        commands::op_plan_lock_off,
    ]);
    let gate = cell.clone();
    let focus = cell.clone();
    base.invoke_handler(move |invoke| {
        // The one place the lock is enforced: no command can forget it.
        if let Err(e) = guard(&gate, invoke.message.command()) {
            invoke.resolver.reject(e.to_ui());
            return true;
        }
        handler(invoke)
    })
    .on_page_load(move |webview, payload| {
        // A reload or a navigation: the old page's channels are dead.
        if payload.event() == PageLoadEvent::Started {
            if let Some(shell) = cell.get() {
                shell.ops.drop_subscribers_of(webview.label());
            }
        }
    })
    .on_window_event(move |_window, event| {
        // D.4a T74: the store may have been locked or put back in files from a terminal
        // while the window was away; the re-read runs off the main thread.
        if matches!(event, tauri::WindowEvent::Focused(true)) {
            if let Some(shell) = focus.get() {
                shell.recheck_later();
            }
        }
    })
}

/// The opener plugin as the app uses it: `open_url`, which the capability scopes to the app's
/// links and the install pages the core's tool specs name, exactly as written (pinned in
/// tests/ipc_mock.rs). No injected script: the plugin's default would also open any
/// `<a target="_blank">`, or a link clicked with Ctrl or Shift, in the browser on its own, a way
/// out the page never needs (its links go through `open_url`) and an injected link could use.
pub fn opener_plugin<R: Runtime>() -> impl tauri::plugin::Plugin<R> {
    tauri_plugin_opener::Builder::new()
        .open_js_links_on_click(false)
        .build()
}

/// The clipboard plugin. The capability grants `write_text` alone: the app copies a report or a
/// command out, and never reads what another program put on the clipboard (D.3 overview R11).
pub fn clipboard_plugin<R: Runtime>() -> impl tauri::plugin::Plugin<R> {
    tauri_plugin_clipboard_manager::init()
}

fn guard(cell: &ShellCell, command: &str) -> Result<(), DesktopError> {
    match cell.get() {
        Some(shell) => shell.lock.guard(command),
        None => Err(DesktopError::Internal(
            "the shell has not started yet".into(),
        )),
    }
}

/// Make `shell` the one the gate (through `cell`) and the commands (as state) use. Call it
/// once, after the app is built and before it runs.
pub fn install<R: Runtime>(
    app: &tauri::App<R>,
    cell: &ShellCell,
    shell: Arc<Shell>,
) -> Result<(), DesktopError> {
    cell.0
        .set(shell.clone())
        .map_err(|_| DesktopError::Internal("the shell was installed twice".into()))?;
    app.manage(shell);
    Ok(())
}

/// The lock hook's notice to the webview. No Rust-side listener exists for it, and none may
/// call into the lock machine: Tauri runs Rust listeners here, under the machine's lock.
pub fn emit_lock_changed<R: Runtime>(app: &AppHandle<R>, state: &LockState) {
    if let Err(e) = app.emit(LOCK_CHANGED, state.clone()) {
        tracing::warn!("{LOCK_CHANGED} was not delivered: {e}");
    }
}

/// Quit (see the module docs): returns at once; a `quit` thread waits for the running
/// operations and then exits. Only the first call does anything.
pub fn quit<R: Runtime>(app: &AppHandle<R>, shell: &Arc<Shell>) {
    if !shell.begin_quit() {
        return;
    }
    tracing::info!("quitting");
    tell_quitting(app, shell.ops.running());
    let spawned = thread::Builder::new().name("quit".into()).spawn({
        let (app, shell) = (app.clone(), shell.clone());
        move || drain_and_exit(&app, &shell)
    });
    if let Err(e) = spawned {
        tracing::error!("no thread to wait for running operations on ({e}); exiting at once");
        shell.drained.store(true, SeqCst);
        app.exit(0);
    }
}

/// Quit ([`quit`]) on the first of `signals` to arrive — the app passes
/// [`QUIT_SIGNALS`](crate::signals::QUIT_SIGNALS); [`crate::signals`] has the rules.
#[cfg(unix)]
pub fn quit_on_signals<R: Runtime>(
    app: &AppHandle<R>,
    shell: &Arc<Shell>,
    signals: &[std::os::raw::c_int],
) -> io::Result<()> {
    let (app, shell) = (app.clone(), shell.clone());
    crate::signals::on_quit_signals(signals, move |_| quit(&app, &shell))
}

/// A quit waiting for `running` operations tells the page ([`QUITTING`]), which shows that it
/// is stopping them rather than a window whose every command is refused. With none running it
/// exits at once, and nothing is said.
fn tell_quitting<R: Runtime>(app: &AppHandle<R>, running: usize) {
    if running == 0 {
        return;
    }
    let quitting = Quitting {
        running: u32::try_from(running).unwrap_or(u32::MAX),
        wait_ms: u64::try_from(STOP_BOUND.as_millis()).unwrap_or(u64::MAX),
    };
    if let Err(e) = app.emit(QUITTING, quitting) {
        tracing::warn!("{QUITTING} was not delivered: {e}");
    }
}

fn drain_and_exit<R: Runtime>(app: &AppHandle<R>, shell: &Shell) {
    if !shell.ops.cancel_all_and_wait(STOP_BOUND) {
        tracing::warn!(
            running = shell.ops.running(),
            "operations still running {STOP_BOUND:?} after the quit cancelled them; exiting"
        );
    }
    // On this thread, which holds nothing the watch's callback takes.
    shell.stop_session_watch();
    // The recovery key off the clipboard before the exit, waited for at most its bound.
    shell
        .clipboard
        .clear_within(crate::secret_clip::CLEAR_ON_QUIT);
    shell.drained.store(true, SeqCst);
    app.exit(0);
}

/// The run loop wants to exit (the last window closed, the OS asked, or [`quit`]'s own exit):
/// refused until the quit has waited for the running operations, then let through.
pub fn on_exit_requested<R: Runtime>(app: &AppHandle<R>, shell: &Arc<Shell>, api: &ExitRequestApi) {
    if shell.drained.load(SeqCst) {
        return;
    }
    api.prevent_exit();
    quit(app, shell);
}

/// Start the idle ticker (every [`IDLE_TICK`]: the lock's idle check and the sweep of
/// long-expired plans) and the output flusher (every [`FLUSH_TICK`] while an operation runs:
/// the output that waited long enough), each on a `std::thread` of its own — never an async
/// task: the lock hook they can set off drops plans and starts threads. While nothing runs the
/// flusher sleeps, so an idle app does not wake twenty times a second; [`Shell::execute`] wakes
/// it, and [`IDLE_TICK`] is its backstop. They stop once the event loop exits
/// ([`Shell::stop_tickers`]). An `Err` means no idle lock, so the app must not start.
pub fn start_tickers(shell: &Arc<Shell>) -> io::Result<()> {
    spawn_ticker("idle-ticker", IDLE_TICK, shell, |_| true, idle_tick)?;
    spawn_ticker("output-flusher", FLUSH_TICK, shell, running, |shell| {
        shell.ops.flush_due()
    })
}

/// The idle ticker's work: lock once the idle time has passed, drop the plans one time to live
/// past their expiry, and the drafts past theirs, and re-read the store when `store.yaml` changed
/// (D.4a T74).
fn idle_tick(shell: &Shell) {
    shell.lock.tick();
    shell.recheck_if_changed();
    shell.ops.sweep();
    shell.drafts.sweep();
}

/// Remove the `kubeconfig-*` copies a gone process left in `context`'s runtime dir (by owner,
/// never by age: D.4a T44); how many went. A failure is a logged warning, never fatal: the next
/// start, and every doctor run, tries again.
pub fn sweep_runtime_dir(context: &Context) -> usize {
    use apprafter_core::runtime::sweep_stale;
    match sweep_stale(context) {
        Ok(removed) => {
            if removed > 0 {
                tracing::info!(removed, "removed stale kubeconfigs from the runtime dir");
            }
            removed
        }
        Err(e) => {
            tracing::warn!("the runtime dir could not be swept: {e}");
            0
        }
    }
}

/// Whether an operation runs: the output flusher has work.
fn running(shell: &Shell) -> bool {
    shell.ops.running() > 0
}

/// A ticker that calls `tick` every `every` while `active` holds, and otherwise sleeps until
/// it does (asked on a nudge, and at least every [`IDLE_TICK`]).
fn spawn_ticker(
    name: &str,
    every: Duration,
    shell: &Arc<Shell>,
    active: fn(&Shell) -> bool,
    tick: fn(&Shell),
) -> io::Result<()> {
    let shell = shell.clone();
    let label = name.to_string();
    thread::Builder::new()
        .name(label.clone())
        .spawn(move || loop {
            if shell.stop.wait_until(IDLE_TICK, || active(&shell)) {
                break;
            }
            if !active(&shell) {
                continue;
            }
            if shell.stop.wait(every) {
                break;
            }
            // One bad tick must not end the ticker, and with it every later auto-lock.
            if let Err(payload) = panic::catch_unwind(AssertUnwindSafe(|| tick(&shell))) {
                tracing::error!("{label} panicked: {}", panic_message(&*payload));
            }
        })
        .map(drop)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::SeqCst};
    use std::sync::mpsc;
    use std::sync::{Arc, Mutex};
    use std::thread;
    use std::time::{Duration, Instant};

    use apprafter_core::lock::limits::{DisplayKind, LimitClient};
    use apprafter_core::lock::UnlockWindow;
    use apprafter_core::secrets::testing::{locked_store, LockedTestStore};
    use apprafter_core::secrets::VaultRequest;
    use apprafter_core::vault::VAULT_FILE;
    use apprafter_core::{
        CancellationToken, Context, CoreError, Event, Outcome, PathSource, PlanClass, SecretString,
    };
    use apprafter_desktop_ipc::SessionEvents;
    use apprafter_desktop_ipc::{
        errors, AuthInfo, AuthOutcome, AutoLock, CancelledBy, LockReason, LockState, OpEvent, OpId,
        OpState, Settings, UnavailableReason,
    };
    use apprafter_os_auth::{Listening, SessionEvent};
    use serde_json::json;

    use zeroize::Zeroizing;

    use super::Shell;
    use crate::auth::test_os::{self, Call, ScriptedOs};
    use crate::auth::{AuthPurpose, Authenticator, FakeAuthenticator, NoAuthenticator};
    use crate::drafts::DRAFT_TTL_MS;
    use crate::env::ToolSearchPath;
    use crate::errors::DesktopError;
    use crate::ops::test_clock::ManualClock;
    use crate::ops::{EventSink, Executor, PlanParts, PLAN_TTL_MS};
    use crate::settings::SettingsStore;

    const T0: u64 = 1_700_000_000_000;
    const LONG: Duration = Duration::from_secs(10);

    struct Rig {
        _dir: tempfile::TempDir,
        clock: Arc<ManualClock>,
        shell: Arc<Shell>,
        notified: Arc<Mutex<Vec<LockState>>>,
    }

    fn rig(settings: Settings, auth: Arc<dyn Authenticator>) -> Rig {
        let dir = tempfile::tempdir().unwrap();
        let clock = Arc::new(ManualClock::at(T0));
        let store = SettingsStore::load(dir.path(), &*clock);
        store.set(settings).unwrap();
        let notified = Arc::new(Mutex::new(Vec::new()));
        let shell = {
            let notified = notified.clone();
            Shell::new(
                store,
                auth,
                clock.clone(),
                Context::for_desktop(dir.path().join("store"), "http://127.0.0.1:9"),
                ToolSearchPath::known(Default::default(), PathSource::Explicit),
                false,
                move |state| notified.lock().unwrap().push(state.clone()),
            )
        };
        assert!(shell.auth.settled(LONG), "the authenticator never answered");
        Rig {
            _dir: dir,
            clock,
            shell,
            notified,
        }
    }

    fn unlocked_at_start() -> Settings {
        Settings {
            lock_on_start: false,
            ..Settings::default()
        }
    }

    fn fake() -> Arc<dyn Authenticator> {
        Arc::new(FakeAuthenticator::new())
    }

    fn plan(shell: &Shell, ran: &Arc<AtomicBool>) -> OpId {
        let ran = ran.clone();
        let exec: Executor = Box::new(move |_, _| {
            ran.store(true, SeqCst);
            Ok(Outcome::Completed { result: json!(0) })
        });
        shell
            .ops
            .register_plan(
                PlanParts::new(PlanClass::Bounded, "Upgrade", "upgrade"),
                exec,
            )
            .op_id
    }

    #[derive(Default)]
    struct Sink(Mutex<Vec<OpEvent>>);

    impl EventSink for Sink {
        fn send(&self, event: &OpEvent) -> bool {
            self.0.lock().unwrap().push(event.clone());
            true
        }

        fn webview(&self) -> &str {
            "main"
        }
    }

    #[test]
    fn locking_and_unlocking_each_drop_every_plan_and_notify_the_webview() {
        let r = rig(unlocked_at_start(), Arc::new(FakeAuthenticator::new()));
        let ran = Arc::new(AtomicBool::new(false));
        let before_lock = plan(&r.shell, &ran);
        let state = r.shell.lock_now();
        assert!(state.locked);
        assert_eq!(state.reason, Some(LockReason::Manual));
        assert!(matches!(
            r.shell.ops.execute(before_lock, &FakeAuthenticator::new()),
            Err(DesktopError::PlanNotFound { .. })
        ));
        // A plan registered while locked (a command that passed the guard just before the
        // lock) goes with the unlock.
        let during_lock = plan(&r.shell, &ran);
        let state = r.shell.unlock().unwrap();
        assert!(!state.locked);
        assert!(matches!(
            r.shell.ops.execute(during_lock, &FakeAuthenticator::new()),
            Err(DesktopError::PlanNotFound { .. })
        ));
        assert!(!ran.load(SeqCst));
        let notified = r.notified.lock().unwrap().clone();
        assert_eq!(notified.len(), 2, "{notified:?}");
        assert!(notified[0].locked && !notified[1].locked);
    }

    fn stage(index: u32) -> OpEvent {
        OpEvent::Stage {
            index,
            total: 2,
            title: format!("step {index}"),
        }
    }

    /// Waits until `done` holds; a test that waits longer than `LONG` fails.
    fn wait_until(what: &str, done: impl Fn() -> bool) {
        let deadline = Instant::now() + LONG;
        while !done() {
            assert!(Instant::now() < deadline, "{what} never happened");
            thread::sleep(Duration::from_millis(2));
        }
    }

    /// A plan whose operation reports stage 1 at once, then stage 2 and its end once the
    /// sender returned with its id sends.
    fn two_stages(shell: &Shell) -> (OpId, mpsc::Sender<()>) {
        let (next, wait) = mpsc::channel::<()>();
        let exec: Executor = Box::new(move |r, _| {
            r.report(Event::Stage {
                index: 1,
                total: 2,
                title: "step 1".into(),
            });
            let _ = wait.recv_timeout(LONG);
            r.report(Event::Stage {
                index: 2,
                total: 2,
                title: "step 2".into(),
            });
            Ok(Outcome::Completed { result: json!(2) })
        });
        let id = shell
            .ops
            .register_plan(
                PlanParts::new(PlanClass::Bounded, "Upgrade", "upgrade"),
                exec,
            )
            .op_id;
        (id, next)
    }

    fn finished() -> OpEvent {
        OpEvent::Finished {
            outcome: Outcome::Completed { result: json!(2) },
        }
    }

    #[test]
    fn a_lock_ends_every_subscription_and_a_page_catches_up_after_the_unlock() {
        let r = rig(unlocked_at_start(), Arc::new(FakeAuthenticator::new()));
        let (id, next) = two_stages(&r.shell);
        let page = Arc::new(Sink::default());
        r.shell.execute(id, page.clone()).unwrap();
        wait_until("stage 1 reached the page", || {
            page.0.lock().unwrap().contains(&stage(1))
        });

        assert!(r.shell.lock_now().locked);
        // The operation runs on, and ends, while the app is locked.
        next.send(()).unwrap();
        wait_until("the operation ended", || r.shell.ops.running() == 0);
        assert_eq!(
            *page.0.lock().unwrap(),
            vec![stage(1)],
            "a locked page receives nothing"
        );

        r.shell.unlock().unwrap();
        // The page subscribes again, once: what it missed is in the replay.
        let again = Arc::new(Sink::default());
        let subscribed = r.shell.ops.subscribe(id, again).unwrap();
        assert_eq!(subscribed.replay, vec![stage(1), stage(2), finished()]);
        assert_eq!(*page.0.lock().unwrap(), vec![stage(1)], "never twice");
    }

    #[test]
    fn a_subscription_that_lands_while_locked_ends_with_the_unlock() {
        // `op_subscribe` passed the gate just before a lock, and its subscription landed after
        // the lock had ended every other: the unlock ends it, as the page subscribes afresh.
        let r = rig(unlocked_at_start(), Arc::new(FakeAuthenticator::new()));
        let (id, next) = two_stages(&r.shell);
        let first = Arc::new(Sink::default());
        r.shell.execute(id, first.clone()).unwrap();
        // Step 1 is reported on the operation's own thread: wait for it, or it could land
        // between the raced subscription and the unlock and reach that sink legitimately.
        wait_until("step 1 was reported", || {
            !first.0.lock().unwrap().is_empty()
        });
        assert!(r.shell.lock_now().locked);
        let raced = Arc::new(Sink::default());
        r.shell.ops.subscribe(id, raced.clone()).unwrap();
        r.shell.unlock().unwrap();
        next.send(()).unwrap();
        wait_until("the operation ended", || r.shell.ops.running() == 0);
        assert!(raced.0.lock().unwrap().is_empty(), "{:?}", raced.0);
    }

    #[test]
    fn lock_now_with_the_lock_not_in_effect_reports_unlocked() {
        let r = rig(Settings::default(), Arc::new(NoAuthenticator));
        let state = r.shell.lock_now();
        assert!(!state.locked, "a lock nobody could open is no lock");
        assert!(r.notified.lock().unwrap().is_empty());
    }

    #[test]
    fn a_new_idle_time_tells_the_page_and_keeps_its_plans_and_subscriptions() {
        let r = rig(unlocked_at_start(), Arc::new(FakeAuthenticator::new()));
        let ran = Arc::new(AtomicBool::new(false));
        let pending = plan(&r.shell, &ran);
        let page = Arc::new(Sink::default());
        r.shell.ops.subscribe(pending, page.clone()).unwrap();
        let five = Settings {
            auto_lock: AutoLock::Min5,
            ..unlocked_at_start()
        };
        r.shell.set_settings(five).unwrap();
        let heard = r.notified.lock().unwrap().clone();
        assert_eq!(
            heard.len(),
            1,
            "the page heard of it from the save: {heard:?}"
        );
        assert_eq!((heard[0].auto_lock_minutes, heard[0].seq), (Some(5), 1));
        assert_eq!(r.shell.lock.state(), heard[0]);
        assert!(page.0.lock().unwrap().is_empty(), "nothing ended");
        r.shell.execute(pending, page.clone()).unwrap();
        wait_until("the plan ran", || ran.load(SeqCst));
    }

    #[test]
    fn settings_go_through_the_lock_machine_and_the_file() {
        let r = rig(unlocked_at_start(), Arc::new(FakeAuthenticator::new()));
        let never = Settings {
            auto_lock: AutoLock::Never,
            ..unlocked_at_start()
        };
        assert_eq!(r.shell.set_settings(never.clone()).unwrap(), never);
        assert_eq!(r.shell.settings.get(), never);
        assert_eq!(r.shell.lock.state().auto_lock_minutes, None);

        // Without an authenticator the lock cannot be switched on, and nothing is saved.
        let r = rig(
            Settings {
                lock_enabled: false,
                ..Settings::default()
            },
            Arc::new(NoAuthenticator),
        );
        let err = r.shell.set_settings(Settings::default()).unwrap_err();
        assert!(
            matches!(err, DesktopError::AuthUnavailable { .. }),
            "{err:?}"
        );
        assert!(!r.shell.settings.get().lock_enabled);
    }

    #[test]
    fn app_info_names_this_build_and_its_authenticator() {
        let r = rig(unlocked_at_start(), Arc::new(NoAuthenticator));
        let info = r.shell.app_info();
        assert_eq!(info.desktop_version, env!("CARGO_PKG_VERSION"));
        assert_eq!(info.core_version, apprafter_core::VERSION);
        assert_eq!(info.auth, NoAuthenticator.info());
        assert!(!info.test_build);
        assert!(!info.account.is_empty() && !info.host.is_empty());
        assert_eq!(info.settings_notice, None);
        // The rig's store is in files (D.4a §3.14, T74).
        assert_eq!(
            info.secrets,
            apprafter_desktop_ipc::SecretsInfo {
                mode: apprafter_core::secrets::SecretsMode::Files,
                slots: Vec::new(),
                protected: false,
                store: r.shell.context.config_root().display().to_string(),
                problem: None,
            }
        );
    }

    /// D.4a T74: app_info reads store.yaml — a locked store's mode, slots and protection.
    #[test]
    fn app_info_reads_where_the_store_keeps_its_secrets() {
        let r = rig(unlocked_at_start(), Arc::new(NoAuthenticator));
        apprafter_core::secrets::testing::locked_store(r.shell.context.config_root(), &[]);
        let info = r.shell.app_info();
        assert_eq!(
            info.secrets.mode,
            apprafter_core::secrets::SecretsMode::Locked
        );
        assert_eq!(
            info.secrets.slots,
            vec![apprafter_core::secrets::LockSlotKind::RecoveryKey]
        );
        assert!(info.secrets.protected && info.secrets.problem.is_none());
    }

    #[test]
    fn the_settings_reach_the_authenticator_at_start_and_after_every_save() {
        let os = ScriptedOs::new(FakeAuthenticator::new().info(), AuthOutcome::Verified);
        let (auth, calls) = test_os::system(os);
        let no_hello = Settings {
            hello: false,
            ..unlocked_at_start()
        };
        let r = rig(no_hello.clone(), auth);
        let hellos = || -> Vec<bool> {
            calls
                .lock()
                .unwrap()
                .iter()
                .filter_map(|call| match call {
                    Call::Hello(on) => Some(*on),
                    _ => None,
                })
                .collect()
        };
        assert_eq!(hellos(), [false], "at start");
        r.shell.set_settings(unlocked_at_start()).unwrap();
        assert_eq!(hellos(), [false, true], "after the save");

        // A save the lock machine refuses reaches nothing.
        let reason = UnavailableReason::NotConfigured;
        let (auth, calls) = test_os::system(ScriptedOs::new(
            AuthInfo {
                available: false,
                method: None,
                unavailable: Some(reason),
                biometrics_choice: false,
                password_field: false,
            },
            AuthOutcome::Unavailable { reason },
        ));
        let off = Settings {
            lock_enabled: false,
            ..no_hello
        };
        let r = rig(off, auth);
        assert!(r.shell.set_settings(Settings::default()).is_err());
        let said: Vec<Call> = calls
            .lock()
            .unwrap()
            .iter()
            .filter(|call| matches!(call, Call::Hello(_)))
            .cloned()
            .collect();
        assert_eq!(said, [Call::Hello(false)], "only the start's");
    }

    /// Answers `info` with a password field once `field` is set.
    struct Changes {
        field: AtomicBool,
    }

    impl Authenticator for Changes {
        fn info(&self) -> AuthInfo {
            AuthInfo {
                password_field: self.field.load(SeqCst),
                ..FakeAuthenticator::new().info()
            }
        }

        fn verify(&self, _purpose: &AuthPurpose, _cancel: &CancellationToken) -> AuthOutcome {
            AuthOutcome::Verified
        }
    }

    #[test]
    fn app_info_says_what_the_authenticator_says_now() {
        // A polkit dialog that found no agent moves Linux to the password field: the page reads
        // app_info again and must see it.
        let auth = Arc::new(Changes {
            field: AtomicBool::new(false),
        });
        let r = rig(unlocked_at_start(), auth.clone());
        assert!(!r.shell.app_info().auth.password_field);
        auth.field.store(true, SeqCst);
        assert!(r.shell.app_info().auth.password_field);
    }

    #[test]
    fn the_password_unlock_answers_the_state_it_left() {
        let auth = Arc::new(FakeAuthenticator::new().with_password("open sesame".to_owned()));
        auth.saying(&["Authentication failure"]);
        let r = rig(Settings::default(), auth);
        let refusal = r
            .shell
            .unlock_with_password(Zeroizing::new("guess".into()))
            .unwrap_err();
        assert_eq!(
            refusal.to_ui().fields["messages"],
            json!(["Authentication failure"])
        );
        let state = r
            .shell
            .unlock_with_password(Zeroizing::new("open sesame".into()))
            .unwrap();
        assert!(!state.locked);
        assert_eq!(state, r.shell.lock.state());
        assert_eq!(r.notified.lock().unwrap().clone(), [state]);
    }

    #[test]
    fn execute_follows_the_plan_and_a_failed_try_leaves_no_sink_behind() {
        let auth = Arc::new(FakeAuthenticator::new());
        let r = rig(unlocked_at_start(), auth.clone());
        let ran = Arc::new(AtomicBool::new(false));
        let view = r.shell.ops.register_plan(
            PlanParts::new(PlanClass::Destructive, "Remove", "delete"),
            {
                let ran = ran.clone();
                Box::new(move |_, _| {
                    ran.store(true, SeqCst);
                    Ok(Outcome::Completed { result: json!(1) })
                })
            },
        );
        auth.then(AuthOutcome::Busy);
        let first = Arc::new(Sink::default());
        let err = r.shell.execute(view.op_id, first.clone()).unwrap_err();
        assert!(matches!(err, DesktopError::AuthBusy), "{err:?}");
        let second = Arc::new(Sink::default());
        r.shell.execute(view.op_id, second.clone()).unwrap();
        let deadline = Instant::now() + LONG;
        while second.0.lock().unwrap().is_empty() {
            assert!(Instant::now() < deadline, "the op never ended");
            thread::sleep(Duration::from_millis(2));
        }
        assert!(ran.load(SeqCst));
        assert!(
            first.0.lock().unwrap().is_empty(),
            "the busy try's sink was unsubscribed"
        );
        assert!(matches!(
            second.0.lock().unwrap().as_slice(),
            [OpEvent::Finished { .. }]
        ));
    }

    /// An unlock prompt that stays open until the test answers it, and says when it opened.
    struct HeldPrompt {
        opened: Mutex<mpsc::Sender<()>>,
        answer: Mutex<mpsc::Receiver<AuthOutcome>>,
    }

    impl Authenticator for HeldPrompt {
        fn info(&self) -> AuthInfo {
            FakeAuthenticator::new().info()
        }

        fn verify(&self, _purpose: &AuthPurpose, _cancel: &CancellationToken) -> AuthOutcome {
            let _ = self.opened.lock().unwrap().send(());
            self.answer
                .lock()
                .unwrap()
                .recv_timeout(LONG)
                .unwrap_or(AuthOutcome::Cancelled {
                    by: CancelledBy::App,
                })
        }
    }

    #[test]
    fn a_quit_closes_the_unlock_prompt_drops_plans_and_starts_nothing_new() {
        let (opened_tx, opened) = mpsc::channel();
        let (answer, answer_rx) = mpsc::channel();
        let prompt = Arc::new(HeldPrompt {
            opened: Mutex::new(opened_tx),
            answer: Mutex::new(answer_rx),
        });
        let r = rig(Settings::default(), prompt);
        assert!(r.shell.lock.state().locked);
        let ran = Arc::new(AtomicBool::new(false));
        let pending = plan(&r.shell, &ran);
        let sink = Arc::new(Sink::default());
        r.shell.ops.subscribe(pending, sink.clone()).unwrap();
        let unlocking = {
            let shell = r.shell.clone();
            thread::spawn(move || shell.unlock())
        };
        opened.recv_timeout(LONG).expect("the unlock prompt opened");

        assert!(r.shell.begin_quit());
        assert!(!r.shell.begin_quit(), "a quit begins once");
        answer.send(AuthOutcome::Verified).unwrap();
        let unlocked = unlocking.join().unwrap();
        assert!(
            matches!(unlocked, Err(DesktopError::AuthCancelled)),
            "a yes after the quit began does not unlock: {unlocked:?}"
        );
        assert!(r.shell.lock.state().locked);
        assert_eq!(
            *sink.0.lock().unwrap(),
            vec![OpEvent::Failed {
                error: DesktopError::Closing.to_ui()
            }],
            "the plan's page heard why"
        );
        assert!(matches!(
            r.shell.ops.execute(pending, &FakeAuthenticator::new()),
            Err(DesktopError::PlanNotFound { .. })
        ));
        let after = plan(&r.shell, &ran);
        let err = r
            .shell
            .ops
            .execute(after, &FakeAuthenticator::new())
            .unwrap_err();
        assert_eq!(err.to_ui().code.as_deref(), Some(errors::CLOSING));
        assert!(!ran.load(SeqCst));
    }

    /// A running operation that ignores its token until `release` sends; `tripped` hears when
    /// the token trips.
    struct Stubborn {
        id: OpId,
        tripped: mpsc::Receiver<()>,
        release: mpsc::Sender<()>,
    }

    fn stubborn(shell: &Shell, page: Arc<Sink>) -> Stubborn {
        let (tripped_tx, tripped) = mpsc::channel();
        let (release, released) = mpsc::channel::<()>();
        let exec: Executor = Box::new(move |_, token| {
            let _registration = token.on_cancel(move || {
                let _ = tripped_tx.send(());
            });
            let _ = released.recv_timeout(LONG);
            Ok(Outcome::Completed {
                result: json!("ignored the token"),
            })
        });
        let id = shell
            .ops
            .register_plan(
                PlanParts::new(PlanClass::Bounded, "Upgrade", "upgrade"),
                exec,
            )
            .op_id;
        shell.execute(id, page).unwrap();
        Stubborn {
            id,
            tripped,
            release,
        }
    }

    #[test]
    fn an_exit_the_os_forced_quits_and_waits_its_short_bound_for_a_stubborn_op() {
        let r = rig(unlocked_at_start(), Arc::new(FakeAuthenticator::new()));
        let page = Arc::new(Sink::default());
        let op = stubborn(&r.shell, page.clone());
        let ran = Arc::new(AtomicBool::new(false));
        let pending = plan(&r.shell, &ran);

        let started = Instant::now();
        r.shell.on_exit();
        let waited = started.elapsed();
        assert!(
            waited >= super::FORCED_STOP_BOUND,
            "returned after {waited:?}"
        );
        assert!(
            waited < super::FORCED_STOP_BOUND + Duration::from_secs(2),
            "returned after {waited:?}"
        );
        assert_eq!(super::FORCED_STOP_BOUND, Duration::from_secs(3));
        op.tripped
            .recv_timeout(LONG)
            .expect("the exit tripped the operation's token");

        // The quit began: the plans are gone, nothing new starts, no unlock prompt opens.
        assert!(matches!(
            r.shell.ops.execute(pending, &FakeAuthenticator::new()),
            Err(DesktopError::PlanNotFound { .. })
        ));
        let after = plan(&r.shell, &ran);
        let err = r
            .shell
            .ops
            .execute(after, &FakeAuthenticator::new())
            .unwrap_err();
        assert!(matches!(err, DesktopError::Closing), "{err:?}");
        assert!(!ran.load(SeqCst));
        assert!(r.shell.stop.wait(LONG), "the tickers stopped");

        // Nothing reaches the departing webview, the operation's end included.
        op.release.send(()).unwrap();
        wait_until("the operation ended", || r.shell.ops.running() == 0);
        assert!(page.0.lock().unwrap().is_empty(), "{:?}", page.0);
        assert!(r.shell.lock_now().locked, "a lock still locks");
        assert!(matches!(r.shell.unlock(), Err(DesktopError::Closing)));
    }

    #[test]
    fn an_exit_after_the_quit_drained_waits_for_nothing() {
        let r = rig(unlocked_at_start(), Arc::new(FakeAuthenticator::new()));
        let op = stubborn(&r.shell, Arc::new(Sink::default()));
        // As the quit thread leaves it, once its own wait is over.
        r.shell.drained.store(true, SeqCst);
        let started = Instant::now();
        r.shell.on_exit();
        assert!(started.elapsed() < Duration::from_secs(1));
        assert!(
            op.tripped.recv_timeout(Duration::from_millis(100)).is_err(),
            "the exit cancelled nothing more"
        );
        assert!(r.shell.stop.wait(LONG), "the tickers stopped");
        op.release.send(()).unwrap();
        wait_until("the operation ended", || r.shell.ops.running() == 0);
        assert!(r.shell.ops.list().iter().any(|op_| op_.op_id == op.id));
    }

    #[test]
    fn the_idle_tick_sweeps_the_plans_long_expired() {
        // No auto-lock: a lock would drop the plan first.
        let never = Settings {
            auto_lock: AutoLock::Never,
            ..unlocked_at_start()
        };
        let r = rig(never, Arc::new(FakeAuthenticator::new()));
        let ran = Arc::new(AtomicBool::new(false));
        let abandoned = plan(&r.shell, &ran);
        let page = Arc::new(Sink::default());
        r.shell.ops.subscribe(abandoned, page.clone()).unwrap();
        r.clock.advance(2 * PLAN_TTL_MS);
        super::idle_tick(&r.shell);
        assert!(
            page.0.lock().unwrap().is_empty(),
            "kept one TTL past its expiry"
        );
        r.clock.advance(1);
        r.shell.ops.flush_due();
        assert!(
            page.0.lock().unwrap().is_empty(),
            "the flusher sweeps nothing"
        );
        super::idle_tick(&r.shell);
        assert_eq!(
            *page.0.lock().unwrap(),
            vec![OpEvent::Failed {
                error: DesktopError::PlanExpired { op_id: abandoned }.to_ui()
            }],
            "swept, and its page told why"
        );
    }

    /// Counts the test flusher's ticks.
    static FLUSHES: AtomicUsize = AtomicUsize::new(0);

    #[test]
    fn the_output_flusher_sleeps_while_nothing_runs() {
        let r = rig(unlocked_at_start(), Arc::new(FakeAuthenticator::new()));
        super::spawn_ticker(
            "test-flusher",
            Duration::from_millis(1),
            &r.shell,
            super::running,
            |_| {
                FLUSHES.fetch_add(1, SeqCst);
            },
        )
        .unwrap();
        thread::sleep(Duration::from_millis(100));
        assert_eq!(FLUSHES.load(SeqCst), 0, "idle: not a tick");

        let (id, next) = two_stages(&r.shell);
        let started = Instant::now();
        r.shell.execute(id, Arc::new(Sink::default())).unwrap();
        wait_until("the flusher ticked", || FLUSHES.load(SeqCst) > 2);
        assert!(
            started.elapsed() < super::IDLE_TICK / 2,
            "the start woke it, not the backstop: {:?}",
            started.elapsed()
        );

        next.send(()).unwrap();
        wait_until("the operation ended", || r.shell.ops.running() == 0);
        // A tick already past its check may still land, however slow the machine; no other.
        let after = FLUSHES.load(SeqCst);
        thread::sleep(Duration::from_millis(200));
        assert!(FLUSHES.load(SeqCst) <= after + 1, "idle again: asleep");
        r.shell.stop_tickers();
    }

    #[test]
    fn the_tickers_stop_as_soon_as_the_event_loop_exits() {
        // The ticker threads hold the shell; stopped, they let it go — without waiting out
        // the idle ticker's five seconds.
        let r = rig(unlocked_at_start(), Arc::new(NoAuthenticator));
        let weak = Arc::downgrade(&r.shell);
        super::start_tickers(&r.shell).unwrap();
        let started = Instant::now();
        r.shell.stop_tickers();
        drop(r);
        while weak.strong_count() > 0 {
            assert!(
                started.elapsed() < super::IDLE_TICK,
                "a ticker still holds the shell after {:?}",
                started.elapsed()
            );
            thread::sleep(Duration::from_millis(5));
        }
    }

    /// The OS session watch as a test drives it: what it hears once set up (`None`: not set up
    /// yet), and what its drop does.
    struct FakeWatch {
        listening: Arc<Mutex<Option<Listening>>>,
        /// When it was asked what it hears.
        asked: Arc<Mutex<Vec<Instant>>>,
        /// Run by the drop, as the real watch's drop waits for a callback in progress.
        on_drop: Option<Box<dyn FnOnce() + Send>>,
        dropped: mpsc::Sender<thread::ThreadId>,
    }

    impl super::SessionSource for FakeWatch {
        fn listening(&self) -> Option<Listening> {
            self.asked.lock().unwrap().push(Instant::now());
            *self.listening.lock().unwrap()
        }
    }

    impl Drop for FakeWatch {
        fn drop(&mut self) {
            if let Some(on_drop) = self.on_drop.take() {
                on_drop();
            }
            let _ = self.dropped.send(thread::current().id());
        }
    }

    /// The callback the shell gave the watch, what it hears, and what tells of its drop.
    struct Watched {
        callback: Arc<Mutex<Option<super::SessionCallback>>>,
        listening: Arc<Mutex<Option<Listening>>>,
        asked: Arc<Mutex<Vec<Instant>>>,
        dropped: mpsc::Receiver<thread::ThreadId>,
    }

    impl Watched {
        fn signal(&self, event: SessionEvent) {
            (self.callback.lock().unwrap().as_ref().expect("a watch"))(event);
        }

        /// The watch is set up a moment from now, hearing `listening`.
        fn set_up_after(&self, after: Duration, listening: Listening) {
            let slot = self.listening.clone();
            thread::spawn(move || {
                thread::sleep(after);
                *slot.lock().unwrap() = Some(listening);
            });
        }
    }

    const BOTH: Listening = Listening {
        lock: true,
        sleep: true,
    };

    /// Starts a fake watch on `shell`, set up at once with `listening` (`None`: not yet), whose
    /// answer `app_info` waits for up to `within`; `on_drop` runs in its drop.
    fn watch(
        shell: &Shell,
        listening: Option<Listening>,
        within: Duration,
        on_drop: Option<Box<dyn FnOnce() + Send>>,
    ) -> Watched {
        let callback = Arc::new(Mutex::new(None));
        let listening = Arc::new(Mutex::new(listening));
        let asked = Arc::new(Mutex::new(Vec::new()));
        let (dropped_tx, dropped) = mpsc::channel();
        shell.watch_session_within(
            {
                let (callback, listening, asked) =
                    (callback.clone(), listening.clone(), asked.clone());
                move |on_event| {
                    *callback.lock().unwrap() = Some(on_event);
                    Box::new(FakeWatch {
                        listening,
                        asked,
                        on_drop,
                        dropped: dropped_tx,
                    })
                }
            },
            within,
        );
        Watched {
            callback,
            listening,
            asked,
            dropped,
        }
    }

    fn events(lock: bool, sleep: bool) -> SessionEvents {
        SessionEvents { lock, sleep }
    }

    #[test]
    fn an_os_lock_or_sleep_locks_as_lock_on_sleep_says_and_the_page_hears_it() {
        for event in [SessionEvent::Locked, SessionEvent::Sleeping] {
            let r = rig(unlocked_at_start(), Arc::new(FakeAuthenticator::new()));
            let watched = watch(&r.shell, Some(BOTH), LONG, None);
            watched.signal(event);
            let state = r.shell.lock.state();
            assert_eq!(state.reason, Some(LockReason::OsSession), "{event:?}");
            assert_eq!(*r.notified.lock().unwrap(), [state], "{event:?}");
        }
        let no_sleep = Settings {
            lock_on_sleep: false,
            ..unlocked_at_start()
        };
        let r = rig(no_sleep, Arc::new(FakeAuthenticator::new()));
        watch(&r.shell, Some(BOTH), LONG, None).signal(SessionEvent::Locked);
        assert!(!r.shell.lock.state().locked);
        assert!(r.notified.lock().unwrap().is_empty());
    }

    #[test]
    fn app_info_says_what_the_watch_hears_and_waits_for_it_within_its_bound() {
        let r = rig(unlocked_at_start(), Arc::new(FakeAuthenticator::new()));
        let started = Instant::now();
        assert_eq!(
            r.shell.app_info().session_events,
            events(false, false),
            "no watch"
        );
        assert!(started.elapsed() < Duration::from_millis(500), "no wait");

        // Set up a moment after it starts: the first answer waits for it.
        let watched = watch(&r.shell, None, LONG, None);
        watched.set_up_after(Duration::from_millis(60), BOTH);
        let started = Instant::now();
        assert_eq!(r.shell.app_info().session_events, events(true, true));
        let waited = started.elapsed();
        assert!(
            waited >= Duration::from_millis(40) && waited < LONG,
            "{waited:?}"
        );
    }

    /// A watch that sets up nothing (no bus: WSL, a container) says so, and so does one that
    /// hears only the locks or only the sleeps.
    #[test]
    fn app_info_says_which_half_listens_and_nothing_when_nothing_does() {
        for (listening, expected) in [
            (Listening::NONE, events(false, false)),
            (
                Listening {
                    lock: true,
                    sleep: false,
                },
                events(true, false),
            ),
            (
                Listening {
                    lock: false,
                    sleep: true,
                },
                events(false, true),
            ),
            (BOTH, events(true, true)),
        ] {
            let r = rig(unlocked_at_start(), Arc::new(FakeAuthenticator::new()));
            let _watched = watch(&r.shell, Some(listening), LONG, None);
            let started = Instant::now();
            assert_eq!(r.shell.app_info().session_events, expected, "{listening:?}");
            assert!(
                started.elapsed() < LONG / 2,
                "{listening:?}: answered once known"
            );
        }
    }

    /// Past its bound `app_info` waits no more and says what is known now; a watch set up later
    /// still counts from then on.
    #[test]
    fn a_watch_set_up_after_the_bound_still_counts_once_it_is() {
        let r = rig(unlocked_at_start(), Arc::new(FakeAuthenticator::new()));
        let bound = Duration::from_millis(150);
        let started = Instant::now();
        let watched = watch(&r.shell, None, bound, None);
        assert_eq!(r.shell.app_info().session_events, events(false, false));
        assert!(started.elapsed() >= bound - Duration::from_millis(50));
        let asked = Instant::now();
        assert_eq!(r.shell.app_info().session_events, events(false, false));
        assert!(
            asked.elapsed() < Duration::from_millis(100),
            "past the bound, no wait"
        );
        // Set up only once the watch was asked past the bound, when the readiness thread knows
        // it is late: from then on only a thread that keeps asking can learn it.
        let deadline = Instant::now() + LONG;
        while !watched
            .asked
            .lock()
            .unwrap()
            .iter()
            .any(|at| at.duration_since(started) > bound)
        {
            assert!(Instant::now() < deadline, "never asked past the bound");
            thread::sleep(Duration::from_millis(2));
        }
        *watched.listening.lock().unwrap() = Some(BOTH);
        let deadline = Instant::now() + LONG;
        while r.shell.app_info().session_events != events(true, true) {
            assert!(Instant::now() < deadline, "the late watch never counted");
            thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn an_exit_drops_the_watch_holding_nothing_its_callback_takes() {
        // The real watch's drop waits for a callback in progress, which takes the lock
        // machine's lock and the operation manager's: the stand-in runs one on a thread of its
        // own and waits for it. Holding either while dropping would never return.
        let r = rig(unlocked_at_start(), Arc::new(FakeAuthenticator::new()));
        let callback: Arc<Mutex<Option<super::SessionCallback>>> = Arc::default();
        let on_drop: Box<dyn FnOnce() + Send> = Box::new({
            let callback = callback.clone();
            move || {
                let callback = callback.lock().unwrap().take().expect("a watch");
                thread::spawn(move || callback(SessionEvent::Sleeping))
                    .join()
                    .unwrap();
            }
        });
        let watched = watch(&r.shell, Some(BOTH), LONG, Some(on_drop));
        *callback.lock().unwrap() = watched.callback.lock().unwrap().take();
        assert_eq!(r.shell.app_info().session_events, events(true, true));
        let ran = Arc::new(AtomicBool::new(false));
        let pending = plan(&r.shell, &ran);
        let page = Arc::new(Sink::default());
        r.shell.ops.subscribe(pending, page.clone()).unwrap();

        let exiting = {
            let shell = r.shell.clone();
            thread::spawn(move || shell.on_exit())
        };
        let dropper = watched
            .dropped
            .recv_timeout(LONG)
            .expect("the exit dropped the watch, without waiting for itself");
        assert_eq!(dropper, exiting.thread().id(), "dropped by the exit itself");
        exiting.join().unwrap();
        assert_eq!(
            r.shell.lock.state().reason,
            Some(LockReason::OsSession),
            "the callback in progress ran to its end"
        );
        assert_eq!(
            r.shell.app_info().session_events,
            events(false, false),
            "no watch any more"
        );
        // A watch started after the quit is dropped at once.
        let late = watch(&r.shell, Some(BOTH), LONG, None);
        late.dropped
            .recv_timeout(LONG)
            .expect("dropped at once after the quit");
        assert_eq!(r.shell.app_info().session_events, events(false, false));
    }

    #[test]
    fn a_nudge_wakes_a_waiter_once_its_condition_holds_and_the_backstop_bounds_it() {
        let stop = Arc::new(super::Stop::default());
        let ready = Arc::new(AtomicBool::new(false));
        let waiter = {
            let (stop, ready) = (stop.clone(), ready.clone());
            thread::spawn(move || stop.wait_until(LONG, || ready.load(SeqCst)))
        };
        thread::sleep(Duration::from_millis(20));
        ready.store(true, SeqCst);
        let started = Instant::now();
        stop.nudge();
        assert!(!waiter.join().unwrap(), "woken, not stopped");
        assert!(started.elapsed() < LONG);
        let started = Instant::now();
        assert!(!stop.wait_until(Duration::from_millis(20), || false));
        assert!(started.elapsed() >= Duration::from_millis(20));
        stop.stop();
        assert!(stop.wait_until(LONG, || false), "stopped");
    }

    #[test]
    fn a_stop_signal_waits_out_its_interval_until_stopped() {
        let stop = super::Stop::default();
        let started = Instant::now();
        assert!(!stop.wait(Duration::from_millis(20)));
        assert!(started.elapsed() >= Duration::from_millis(20));
        stop.stop();
        let started = Instant::now();
        assert!(stop.wait(LONG), "stopped");
        assert!(started.elapsed() < LONG);
    }

    /// The opener as the app builds it puts no script in the page: the plugin's own
    /// (`init-iife.js`, added with `open_js_links_on_click`) would open links in the browser
    /// without `open_url`'s scope.
    #[test]
    fn the_opener_plugin_injects_no_script() {
        use tauri::plugin::Plugin;
        let opener = super::opener_plugin::<tauri::test::MockRuntime>();
        assert!(opener.initialization_script().is_none());
        assert!(opener.initialization_script_2().is_none());
    }

    // Drafts and reads: a lock or a quit drops the drafts and stops the reads; the idle tick
    // sweeps the drafts past their time; stale kubeconfigs go at start.

    /// A read that waits for its token, then ends cancelled — or, when it never trips, ends
    /// completed once `LONG` has passed, so a cancel that never came is the wrong end.
    fn waits_for_cancel() -> Executor {
        Box::new(|_, cancel| {
            let deadline = Instant::now() + LONG;
            while !cancel.is_cancelled() {
                if Instant::now() >= deadline {
                    return Ok(Outcome::Completed {
                        result: json!("never cancelled"),
                    });
                }
                thread::sleep(Duration::from_millis(1));
            }
            Err(CoreError::Cancelled)
        })
    }

    fn a_token(c: char) -> SecretString {
        SecretString::new(c.to_string().repeat(64))
    }

    #[test]
    fn a_lock_drops_the_drafts_cancels_the_reads_and_a_late_verify_keeps_nothing() {
        let r = rig(unlocked_at_start(), Arc::new(FakeAuthenticator::new()));
        let before = r.shell.drafts.epoch();
        let kept = r
            .shell
            .drafts
            .insert(before, "hetzner-cloud".into(), a_token('k'))
            .unwrap();
        let read = r
            .shell
            .start_read("Doctor · prod", Some("prod".into()), waits_for_cancel())
            .unwrap();
        assert!(r.shell.lock_now().locked);
        assert!(matches!(
            r.shell.drafts.get(kept),
            Err(DesktopError::DraftNotFound { .. })
        ));
        wait_until("the read ended", || {
            r.shell
                .ops
                .list()
                .iter()
                .any(|s| s.op_id == read && s.state != OpState::Running)
        });
        let state = r.shell.ops.list().into_iter().find(|s| s.op_id == read);
        assert_eq!(state.map(|s| s.state), Some(OpState::Cancelled));
        assert_eq!(
            r.shell
                .drafts
                .insert(before, "hetzner-cloud".into(), a_token('m')),
            None,
            "a verify that began before the lock keeps nothing"
        );
    }

    #[test]
    fn a_quit_drops_the_drafts_and_starts_no_read() {
        let r = rig(unlocked_at_start(), Arc::new(FakeAuthenticator::new()));
        let id = r
            .shell
            .drafts
            .insert(r.shell.drafts.epoch(), "hetzner-cloud".into(), a_token('k'))
            .unwrap();
        assert!(r.shell.begin_quit());
        assert!(matches!(
            r.shell.drafts.get(id),
            Err(DesktopError::DraftNotFound { .. })
        ));
        assert!(matches!(
            r.shell.start_read("Doctor", None, waits_for_cancel()),
            Err(DesktopError::Closing)
        ));
        assert!(r.shell.ops.list().is_empty());
    }

    #[test]
    fn the_idle_tick_sweeps_the_expired_drafts() {
        // No auto-lock: a lock would drop the draft first.
        let never = Settings {
            auto_lock: AutoLock::Never,
            ..unlocked_at_start()
        };
        let r = rig(never, Arc::new(FakeAuthenticator::new()));
        let id = r
            .shell
            .drafts
            .insert(r.shell.drafts.epoch(), "hetzner-cloud".into(), a_token('k'))
            .unwrap();
        r.clock.advance(DRAFT_TTL_MS + 1);
        super::idle_tick(&r.shell);
        assert!(!r.shell.lock.state().locked);
        assert!(
            matches!(
                r.shell.drafts.get(id),
                Err(DesktopError::DraftNotFound { .. })
            ),
            "swept, not merely expired"
        );
    }

    #[test]
    fn a_started_read_wakes_the_output_flusher() {
        let r = rig(unlocked_at_start(), Arc::new(FakeAuthenticator::new()));
        let asked = Arc::new(AtomicUsize::new(0));
        let (woke_tx, woke) = mpsc::channel();
        {
            let (shell, asked) = (r.shell.clone(), asked.clone());
            thread::spawn(move || {
                let started = Instant::now();
                shell.stop.wait_until(LONG, || {
                    asked.fetch_add(1, SeqCst);
                    super::running(&shell)
                });
                let _ = woke_tx.send(started.elapsed());
            });
        }
        // Forced, not sampled: the flusher asked once under the stop lock and found nothing, so
        // it waits; the nudge takes that lock, so it lands only once the flusher is waiting.
        wait_until("the flusher asked", || asked.load(SeqCst) > 0);
        let id = r
            .shell
            .start_read("Doctor", None, waits_for_cancel())
            .unwrap();
        let waited = woke.recv_timeout(2 * LONG).expect("the flusher returned");
        assert!(
            waited < LONG / 2,
            "the flusher slept through the read: {waited:?}"
        );
        r.shell.ops.cancel(id).unwrap();
        wait_until("the read ended", || r.shell.ops.running() == 0);
    }

    #[test]
    fn the_runtime_sweep_removes_only_what_a_gone_process_left() {
        let dir = tempfile::tempdir().unwrap();
        let run = dir.path().join("run");
        std::fs::create_dir_all(&run).unwrap();
        // Names without a recorded start, as a pre-D.4a client wrote them: judged by the pid
        // alone (the core's tests cover the recorded start, `runtime.rs`).
        let gone = format!("kubeconfig-{}-1.yaml", 0x3FFF_FFF0_u32);
        let mine = format!("kubeconfig-{}-1.yaml", std::process::id());
        for name in [gone.as_str(), mine.as_str(), "notes.txt"] {
            std::fs::write(run.join(name), "x").unwrap();
        }
        let ctx = Context::for_desktop(dir.path().join("store"), "http://127.0.0.1:9")
            .with_runtime_dir(run.clone());
        assert_eq!(super::sweep_runtime_dir(&ctx), 1);
        assert!(!run.join(&gone).exists());
        assert!(run.join(&mine).exists() && run.join("notes.txt").exists());
        // A runtime dir that cannot be read is a warning, never an error: none removed.
        let unreadable = Context::for_desktop(dir.path().join("store"), "http://127.0.0.1:9")
            .with_runtime_dir(run.join("notes.txt"));
        assert_eq!(super::sweep_runtime_dir(&unreadable), 0);
    }

    /// The rig over a store `prepare` made before the app started (D.4a T74).
    fn rig_with_store<T>(
        settings: Settings,
        auth: Arc<dyn Authenticator>,
        prepare: impl FnOnce(&std::path::Path) -> T,
    ) -> (Rig, T) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("store");
        std::fs::create_dir_all(&root).unwrap();
        let made = prepare(&root);
        let clock = Arc::new(ManualClock::at(T0));
        let store = SettingsStore::load(dir.path(), &*clock);
        store.set(settings).unwrap();
        let notified = Arc::new(Mutex::new(Vec::new()));
        let shell = {
            let notified = notified.clone();
            Shell::new(
                store,
                auth,
                clock.clone(),
                Context::for_desktop(root, "http://127.0.0.1:9"),
                ToolSearchPath::known(Default::default(), PathSource::Explicit),
                false,
                move |state| notified.lock().unwrap().push(state.clone()),
            )
        };
        assert!(shell.auth.settled(LONG), "the authenticator never answered");
        (
            Rig {
                _dir: dir,
                clock,
                shell,
                notified,
            },
            made,
        )
    }

    fn locked_rig() -> (Rig, LockedTestStore) {
        rig_with_store(unlocked_at_start(), Arc::new(NoAuthenticator), |root| {
            locked_store(root, &[])
        })
    }

    fn secret(store: &LockedTestStore) -> Zeroizing<String> {
        Zeroizing::new(store.recovery_key().expose().to_string())
    }

    #[test]
    fn a_protected_store_starts_the_app_locked_and_its_secret_unlocks_it() {
        let (r, store) = locked_rig();
        assert!(r.shell.lock.state().locked && r.shell.lock.protected());
        let wrong = r
            .shell
            .unlock_with_password(Zeroizing::new("not it".into()))
            .unwrap_err();
        assert!(
            matches!(*wrong.error, DesktopError::AuthFailed { .. }),
            "{wrong:?}"
        );
        assert!(!r.shell.vault.holds(&store.store_id));
        let state = r.shell.unlock_with_password(secret(&store)).unwrap();
        assert!(!state.locked);
        assert!(r.shell.vault.holds(&store.store_id));
    }

    #[test]
    fn every_lock_and_the_quit_drop_the_key() {
        let (r, store) = locked_rig();
        r.shell.unlock_with_password(secret(&store)).unwrap();
        r.shell.lock_now();
        assert!(!r.shell.vault.holds(&store.store_id));
        r.shell.unlock_with_password(secret(&store)).unwrap();
        assert!(r.shell.begin_quit());
        assert!(!r.shell.vault.holds(&store.store_id));
    }

    #[test]
    fn the_os_opens_nothing_on_a_protected_store() {
        let (r, _store) =
            rig_with_store(unlocked_at_start(), fake(), |root| locked_store(root, &[]));
        let e = r.shell.unlock().unwrap_err();
        assert!(
            matches!(
                e,
                DesktopError::AuthUnavailable {
                    reason: UnavailableReason::UsePasswordField
                }
            ),
            "{e:?}"
        );
    }

    #[test]
    fn a_store_locked_under_the_app_locks_it_on_the_next_read() {
        let r = rig(unlocked_at_start(), Arc::new(NoAuthenticator));
        assert!(!r.shell.lock.state().locked);
        locked_store(r.shell.context.config_root(), &[]);
        let info = r.shell.app_info();
        assert!(info.secrets.protected);
        let state = r.shell.lock.state();
        assert!(
            state.locked && state.reason == Some(LockReason::SecretsLocked),
            "{state:?}"
        );
    }

    /// A secret read with no key for the store (the core asks the access) re-reads the store
    /// and locks the app, before the read is refused.
    #[test]
    fn a_read_that_finds_no_key_locks_the_app() {
        let r = rig(unlocked_at_start(), Arc::new(NoAuthenticator));
        let store = locked_store(r.shell.context.config_root(), &[]);
        let req = VaultRequest {
            store_root: r.shell.context.config_root(),
            store_id: &store.store_id,
            slots: &[],
        };
        assert!(r.shell.context.vault_access().session(&req).is_err());
        assert_eq!(r.shell.lock.state().reason, Some(LockReason::SecretsLocked));
    }

    #[test]
    fn a_store_back_in_files_drops_the_key_and_unpins() {
        let (r, store) = locked_rig();
        r.shell.unlock_with_password(secret(&store)).unwrap();
        let root = r.shell.context.config_root().to_path_buf();
        std::fs::remove_file(root.join(VAULT_FILE)).unwrap();
        std::fs::remove_file(root.join("store.yaml")).unwrap();
        let info = r.shell.app_info();
        assert!(!info.secrets.protected);
        assert!(!r.shell.vault.holds(&store.store_id) && !r.shell.lock.protected());
        assert!(!r.shell.lock.state().locked);
    }

    /// A lock flow of the app's own changes the store as it runs: the re-read only reports
    /// until its guard goes (the run takes it: Task 17).
    #[test]
    fn during_a_lock_flow_the_re_read_neither_locks_nor_forgets() {
        let r = rig(unlocked_at_start(), Arc::new(NoAuthenticator));
        let flow = r.shell.flow();
        locked_store(r.shell.context.config_root(), &[]);
        assert!(r.shell.app_info().secrets.protected);
        assert!(!r.shell.lock.state().locked);
        drop(flow);
        r.shell.app_info();
        assert_eq!(r.shell.lock.state().reason, Some(LockReason::SecretsLocked));
    }

    #[test]
    fn a_focus_re_reads_the_store_off_the_calling_thread() {
        let r = rig(unlocked_at_start(), Arc::new(NoAuthenticator));
        locked_store(r.shell.context.config_root(), &[]);
        r.shell.recheck_later();
        let deadline = Instant::now() + LONG;
        while !r.shell.lock.state().locked && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(r.shell.lock.state().reason, Some(LockReason::SecretsLocked));
    }

    /// Critic B finding 19: a store locked from a terminal while the app was already locked
    /// (by the person, in files) is re-read by the unlock: the OS no longer opens it.
    #[test]
    fn an_unlock_re_reads_the_store_first() {
        let r = rig(unlocked_at_start(), fake());
        r.shell.lock_now();
        assert!(r.shell.lock.state().locked);
        locked_store(r.shell.context.config_root(), &[]);
        let e = r.shell.unlock().unwrap_err();
        assert!(
            matches!(
                e,
                DesktopError::AuthUnavailable {
                    reason: UnavailableReason::UsePasswordField
                }
            ),
            "{e:?}"
        );
        assert!(r.shell.lock.state().locked);
    }

    /// A store locked from a terminal under the unlocked app opens again in the app on every
    /// route the shell has (controller, d4a-g run 5): the OS's own prompt (Windows Hello,
    /// Touch ID, polkit), Linux's PAM field, and no system authentication at all. The app
    /// locks (`SecretsLocked`), the OS opens nothing, the system password opens nothing, and
    /// the store's secret, typed into the lock screen's field, does (D.4a T75).
    #[test]
    fn a_store_locked_from_a_terminal_opens_with_its_secret_on_every_route() {
        let routes: [(&str, Arc<dyn Authenticator>); 3] = [
            ("the OS's prompt", fake()),
            (
                "PAM's field",
                Arc::new(FakeAuthenticator::new().with_password("system password".to_string())),
            ),
            ("no system authentication", Arc::new(NoAuthenticator)),
        ];
        for (route, auth) in routes {
            let r = rig(unlocked_at_start(), auth);
            assert!(!r.shell.lock.state().locked, "{route}: unlocked at start");
            let store = locked_store(r.shell.context.config_root(), &[]);
            r.shell.app_info();
            let state = r.shell.lock.state();
            assert!(
                state.locked && state.reason == Some(LockReason::SecretsLocked),
                "{route}: {state:?}"
            );
            let e = r.shell.unlock().unwrap_err();
            assert!(
                matches!(
                    e,
                    DesktopError::AuthUnavailable {
                        reason: UnavailableReason::UsePasswordField
                    }
                ),
                "{route}: {e:?}"
            );
            let wrong = r
                .shell
                .unlock_with_password(Zeroizing::new("system password".into()))
                .unwrap_err();
            assert!(
                matches!(*wrong.error, DesktopError::AuthFailed { .. }),
                "{route}: {wrong:?}"
            );
            assert!(r.shell.lock.state().locked, "{route}: still locked");
            let state = r.shell.unlock_with_password(secret(&store)).unwrap();
            assert!(!state.locked, "{route}: unlocked");
            assert!(
                r.shell.vault.holds(&store.store_id),
                "{route}: the key held"
            );
        }
    }

    /// Critic B finding 19: a terminal's `lock off` (or `lock on`) under an unlocked app is
    /// followed by no secret read; the idle tick sees `store.yaml` change and re-reads.
    #[test]
    fn the_idle_tick_follows_a_store_changed_from_a_terminal() {
        let (r, store) = locked_rig();
        r.shell.unlock_with_password(secret(&store)).unwrap();
        let root = r.shell.context.config_root().to_path_buf();
        std::fs::remove_file(root.join(VAULT_FILE)).unwrap();
        std::fs::remove_file(root.join("store.yaml")).unwrap();
        super::idle_tick(&r.shell);
        assert!(!r.shell.vault.holds(&store.store_id) && !r.shell.lock.protected());
        // And the other way: locked from a terminal under the unlocked app.
        locked_store(&root, &[]);
        super::idle_tick(&r.shell);
        assert_eq!(r.shell.lock.state().reason, Some(LockReason::SecretsLocked));
    }

    /// T64 (the Rust verifier's finding 2): re-reads of an unchanged store — app_info, a
    /// re-read, the idle tick — move no epoch, in files or with the key held, so a Protect
    /// plan waiting for its confirm keeps its key at the end.
    #[test]
    fn a_re_read_of_an_unchanged_store_moves_no_epoch() {
        let r = rig(unlocked_at_start(), Arc::new(NoAuthenticator));
        let before = r.shell.vault.epoch();
        r.shell.app_info();
        r.shell.recheck();
        super::idle_tick(&r.shell);
        assert_eq!(r.shell.vault.epoch(), before);
        let (r, store) = locked_rig();
        r.shell.unlock_with_password(secret(&store)).unwrap();
        let before = r.shell.vault.epoch();
        r.shell.app_info();
        r.shell.recheck();
        assert_eq!(r.shell.vault.epoch(), before);
        assert!(r.shell.vault.holds(&store.store_id));
    }

    /// O4 between ticks (the Rust verifier's finding 4): the first secret read after a sleep
    /// the OS did not report finds the app locked and the key gone.
    #[test]
    fn the_first_read_after_an_unreported_sleep_is_refused() {
        let (r, store) = locked_rig();
        r.shell.unlock_with_password(secret(&store)).unwrap();
        let root = r.shell.context.config_root().to_path_buf();
        let req = VaultRequest {
            store_root: &root,
            store_id: &store.store_id,
            slots: &[],
        };
        let session = r.shell.context.vault_access().session(&req).unwrap();
        let vault = std::fs::read(root.join(VAULT_FILE)).unwrap();
        assert!(session.recipient(&vault).is_ok());
        r.clock.sleep(60_000);
        assert!(session.recipient(&vault).is_err());
        assert_eq!(r.shell.lock.state().reason, Some(LockReason::OsSession));
        assert!(!r.shell.vault.holds(&store.store_id));
    }

    /// A lock flow's run that panics still re-reads as it ends (the Rust verifier's finding
    /// 11): its guard asks for it while the thread unwinds.
    #[test]
    fn a_flow_that_panics_still_re_reads() {
        let r = rig(unlocked_at_start(), Arc::new(NoAuthenticator));
        let shell = r.shell.clone();
        let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = shell.flow();
            let _ = locked_store(shell.context.config_root(), &[]);
            panic!("the run broke");
        }));
        assert!(ran.is_err());
        let deadline = Instant::now() + LONG;
        while !r.shell.lock.state().locked && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(r.shell.lock.state().reason, Some(LockReason::SecretsLocked));
    }

    /// The final verifier's finding 10: during a lock flow the re-read still follows
    /// protection — only the lock and the key's drop wait for the run — so after a Lock now
    /// during the run the OS cannot open the store the flow just protected.
    #[test]
    fn during_a_lock_flow_the_os_opens_no_store_it_protected() {
        let r = rig(unlocked_at_start(), fake());
        let flow = r.shell.flow();
        locked_store(r.shell.context.config_root(), &[]);
        r.shell.lock_now();
        let e = r.shell.unlock().unwrap_err();
        assert!(
            matches!(
                e,
                DesktopError::AuthUnavailable {
                    reason: UnavailableReason::UsePasswordField
                }
            ),
            "{e:?}"
        );
        assert!(r.shell.lock.state().locked && r.shell.lock.protected());
        drop(flow);
    }

    /// The final verifier's finding 12 (Tasks 10a, 13): a change that holds a key and waits
    /// for the store's lock goes on while the app is unlocked, and is abandoned once it locks.
    #[test]
    fn after_a_lock_a_change_waiting_for_the_store_is_abandoned() {
        let r = rig(unlocked_at_start(), fake());
        let access = r.shell.context.vault_access().clone();
        let root = r.shell.context.config_root().to_path_buf();
        assert!(access.store_wait_slice().is_some());
        assert!(access.keep_waiting(&root).is_ok());
        r.shell.lock_now();
        assert!(r.shell.lock.state().locked);
        let e = access.keep_waiting(&root).unwrap_err();
        assert!(matches!(e, cli_core::CliError::StoreLocked { .. }), "{e:?}");
    }

    /// D.4a T76: on a protected store with no OS authenticator a destructive plan takes the
    /// store's secret as its gesture (D.2 refused it there); with one, the OS's. A plan that
    /// only `requires_gesture` (an approval) keeps the OS's there (critic B finding 24).
    #[test]
    fn on_a_protected_store_without_an_os_authenticator_a_destructive_plan_takes_the_store_s_secret(
    ) {
        let (r, store) = locked_rig();
        r.shell.unlock_with_password(secret(&store)).unwrap();
        let ran = Arc::new(AtomicBool::new(false));
        let exec: Executor = {
            let ran = ran.clone();
            Box::new(move |_, _| {
                ran.store(true, SeqCst);
                Ok(Outcome::Completed { result: json!(0) })
            })
        };
        let view = r.shell.register_plan(
            PlanParts::new(PlanClass::Destructive, "Remove target prod", "remove"),
            exec,
        );
        assert_eq!(view.gesture, apprafter_desktop_ipc::Gesture::StoreSecret);
        // Checked against the vault through the gate: a wrong secret and a mistyped recovery
        // key are refused, nothing runs, and the plan waits for the next try.
        let tries = Arc::new(Sink::default());
        let wrong = r
            .shell
            .execute_with(
                view.op_id,
                tries.clone(),
                Some(Zeroizing::new("not it".into())),
            )
            .unwrap_err();
        assert!(
            matches!(*wrong.error, DesktopError::AuthFailed { .. }),
            "{wrong:?}"
        );
        let shown = secret(&store);
        let mut chars: Vec<char> = shown.chars().collect();
        let at = shown.rfind('\n').map_or(0, |i| i + 3);
        chars[at] = if chars[at] == 'Q' { 'P' } else { 'Q' };
        let mistyped = Zeroizing::new(chars.into_iter().collect::<String>());
        let mistyped = r
            .shell
            .execute_with(view.op_id, tries, Some(mistyped))
            .unwrap_err();
        assert!(
            matches!(
                &*mistyped.error,
                DesktopError::Core(CoreError::RecoveryKeyInvalid { .. })
            ),
            "{mistyped:?}"
        );
        assert!(!ran.load(SeqCst));
        let sink = Arc::new(Sink::default());
        r.shell
            .execute_with(view.op_id, sink, Some(secret(&store)))
            .unwrap();
        let deadline = Instant::now() + LONG;
        while !ran.load(SeqCst) && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(5));
        }
        assert!(ran.load(SeqCst));
        // target_ops's plans are registered through the shell too.
        let via_ops = crate::target_ops::register(
            &r.shell,
            apprafter_core::Plan {
                class: PlanClass::Destructive,
                title: "Remove target prod".into(),
                changes: Vec::new(),
                payload: (),
            },
            "prod".into(),
            "remove",
            |_, _, _, _| Ok(Outcome::Completed { result: 0 }),
        );
        assert_eq!(via_ops.gesture, apprafter_desktop_ipc::Gesture::StoreSecret);
        let mut approval = PlanParts::new(PlanClass::Bounded, "Approve the plan", "approve");
        approval.requires_gesture = true;
        let approval = r.shell.register_plan(
            approval,
            Box::new(|_, _| Ok(Outcome::Completed { result: json!(0) })),
        );
        assert_eq!(approval.gesture, apprafter_desktop_ipc::Gesture::Os);

        let (r, _store) =
            rig_with_store(unlocked_at_start(), fake(), |root| locked_store(root, &[]));
        let view = r.shell.register_plan(
            PlanParts::new(PlanClass::Destructive, "Remove target prod", "remove"),
            Box::new(|_, _| Ok(Outcome::Completed { result: json!(0) })),
        );
        assert_eq!(view.gesture, apprafter_desktop_ipc::Gesture::Os);
    }

    /// D.4a T72, T73: the app's facts reach the limit rows as given; its window is `App` only
    /// while it holds the protected store's key.
    #[test]
    fn the_limit_client_carries_the_app_s_facts_and_the_window_follows_the_key() {
        let dir = tempfile::tempdir().unwrap();
        let clock = Arc::new(ManualClock::at(T0));
        let ctx = Context::for_desktop(dir.path().join("store"), "http://127.0.0.1:9")
            .with_home_dir(Some(dir.path().to_path_buf()));
        let shell = Shell::new_with(
            SettingsStore::load(dir.path(), &*clock),
            Arc::new(NoAuthenticator),
            clock,
            ctx,
            ToolSearchPath::known(Default::default(), PathSource::Explicit),
            false,
            super::ShellExtras {
                display: DisplayKind::X11,
                ssh_agent: Some("/tmp/agent.sock".into()),
                ..super::ShellExtras::default()
            },
            |_| {},
        );
        let LimitClient::Desktop { display, ssh, .. } = shell.limit_client_later()() else {
            panic!("not the desktop's client")
        };
        assert_eq!(display, DisplayKind::X11);
        assert_eq!(ssh.key, Some(dir.path().join(".ssh").join("id_ed25519")));
        assert_eq!(ssh.agent, Some(std::path::PathBuf::from("/tmp/agent.sock")));
        let now = shell.recheck();
        assert_eq!(shell.window(&now), UnlockWindow::Closed);
    }

    /// D.4a O12: a lock clears the recovery key from the clipboard before its 60 s.
    #[test]
    fn a_lock_clears_the_recovery_key_from_the_clipboard() {
        let board = Arc::new(Mutex::new(None::<String>));
        struct Board(Arc<Mutex<Option<String>>>);
        impl crate::secret_clip::ClipboardBackend for Board {
            fn set_concealed(&mut self, text: &str) -> Result<(), String> {
                *self.0.lock().unwrap() = Some(text.into());
                Ok(())
            }
            fn text(&mut self) -> Result<Option<Zeroizing<String>>, String> {
                Ok(self.0.lock().unwrap().clone().map(Zeroizing::new))
            }
            fn clear(&mut self) -> Result<(), String> {
                *self.0.lock().unwrap() = None;
                Ok(())
            }
        }
        let maker: crate::secret_clip::ClipboardMaker = {
            let board = board.clone();
            Arc::new(move || {
                Ok(Box::new(Board(board.clone())) as Box<dyn crate::secret_clip::ClipboardBackend>)
            })
        };
        let dir = tempfile::tempdir().unwrap();
        let clock = Arc::new(ManualClock::at(T0));
        let store = SettingsStore::load(dir.path(), &*clock);
        store.set(unlocked_at_start()).unwrap();
        let shell = Shell::new_with(
            store,
            Arc::new(FakeAuthenticator::new()),
            clock,
            Context::for_desktop(dir.path().join("store"), "http://127.0.0.1:9"),
            ToolSearchPath::known(Default::default(), PathSource::Explicit),
            false,
            super::ShellExtras {
                clipboard: Some(maker),
                ..super::ShellExtras::default()
            },
            |_| {},
        );
        assert!(shell.auth.settled(LONG));
        shell
            .clipboard
            .copy(&SecretString::new("APPRAFTER-RECOVERY-KEY-1\nQQQSY"))
            .unwrap();
        assert!(board.lock().unwrap().is_some());
        assert!(shell.lock_now().locked);
        let deadline = Instant::now() + LONG;
        while board.lock().unwrap().is_some() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(
            *board.lock().unwrap(),
            None,
            "the lock left the key on the clipboard"
        );
    }
}
