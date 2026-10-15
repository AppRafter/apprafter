// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The app lock, decided in Rust (design spec §4.3): the webview only renders
//! [`LockState`], and every command passes [`LockMachine::guard`] first.
//!
//! The lock is in effect when the settings switch it on **and** the [`Authenticator`] can
//! verify the owner — as its last answer says: the machine reads the [`AuthCache`], never the
//! OS, so an OS slow to answer or hung holds up no tick, lock or state read. Without an
//! authenticator it fails closed in the only safe direction: a lock nobody could open is no
//! lock, so it is off (the webview shows a persistent banner from `AuthInfo`), switching it on
//! is refused with `AuthUnavailable`, and destructive operations are refused by the
//! [`OperationManager`](crate::ops::OperationManager) for the same reason.
//!
//! Settings reach the machine as a copy: [`LockMachine::new`] takes the loaded settings and
//! only [`LockMachine::set_settings`] replaces them. It checks the change, then calls the
//! caller's `persist` (in the app, `|s| store.set(s.clone())`) and applies the change only if
//! that succeeded, so the file and the machine change together or not at all. The machine
//! knows nothing of the [`SettingsStore`](crate::settings::SettingsStore) and the store nothing
//! of the machine.
//!
//! Every transition between locked and unlocked calls the hook exactly once, with the new
//! state, under the machine's lock (so hooks run in transition order). So does every other
//! change to what the state reports — its idle time, after a settings change or a new answer
//! from the authenticator — as an update ([`LockChange::Update`]): the same lock, reported
//! anew. The state at construction is neither: nothing is listening yet. Each of them numbers
//! the state it enters, under that same lock ([`LockState::seq`], 0 at construction): every
//! state the hook is told of and every [`LockMachine::state`] carries the number of the change
//! it follows, which is the order the page keeps — its time (`since_ms`) is the wall clock's,
//! which can step back — and two states with one number are the same state. The idle time is
//! measured with [`elapsed_ms`](crate::ops::elapsed_ms): a suspend counts, a wall clock stepped
//! back does not.
//!
//! # The start
//!
//! The start needs the authenticator's answer only to lock at start (`lock_on_start`, the lock
//! on); otherwise it starts unlocked and asks nothing, the first answer coming in the
//! background. When it needs it, it waits up to [`STARTUP_WITHIN`], for a question the app
//! already asked (the shell's, with the settings) if one is out. An answer later than that
//! starts it locked provisionally — a slow answer never starts it unlocked — and when that
//! answer comes, it unlocks through a normal transition only if it says nothing can verify the
//! owner here (rule 3: a lock nobody could open is no lock) and nothing has changed since the
//! start; otherwise the lock stands.
//!
//! # What waits for what
//!
//! Tauri runs the invoke handler on the main thread, and every command passes
//! [`guard`](LockMachine::guard) there, so the guard reads an atomic and never waits. The
//! machine's own lock is held for bookkeeping and the hook only — never across a prompt or a
//! disk write. [`set_settings`](LockMachine::set_settings) holds a lock of its own across
//! `persist` and the apply, which orders concurrent changes and holds up nothing but the next
//! change.
//!
//! The locks are taken in one order: the settings write, then the machine, then the
//! [`OperationManager`](crate::ops::OperationManager) — the hook drops pending plans, which
//! takes the manager's lock and sends their pages a final event under it. Nothing may take
//! them the other way round, so no `LockMachine` method may be called from an
//! [`EventSink::send`](crate::ops::EventSink::send), from a Rust-side `lock-changed`
//! listener, from the hook itself, or from `persist`. The [`AuthCache`]'s own lock comes last:
//! the machine reads the cache under its lock, and the cache tells the machine of an answer on
//! its asking thread, holding none of its own. The lock on the tick's back-off is taken alone.
//!
//! # A protected store (D.4a T74)
//!
//! While the target store's secrets are protected ([`LockMachine::set_protected`], from the
//! shell's re-read of `store.yaml`) the lock is in effect whatever the saved switch or the OS,
//! the machine follows the pinned settings ([`pinned`]), a save that would switch a pin off is
//! refused `LockRequired`, and only the store's secret unlocks
//! ([`LockMachine::unlock_with_store_secret`]), its key kept under the machine's lock just
//! before the transition. The key's holder (`vault_access`) is told of every lock by the hook.

use std::panic::{self, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use apprafter_core::CancellationToken;
use apprafter_desktop_ipc::{
    AuthInfo, AutoLock, LockReason, LockState, Settings, UnavailableReason, ALLOWED_WHILE_LOCKED,
};
use zeroize::Zeroizing;

use crate::auth::{AuthPurpose, Authenticator, PasswordAnswer};
use crate::auth_cache::AuthCache;
use crate::errors::{DesktopError, Refusal};
use crate::ops::{panic_message, Clock, ClockPair, Stamp};

const MINUTE_MS: u64 = 60_000;

/// How long the start waits for the authenticator's first answer, when it needs one (see the
/// module docs).
pub const STARTUP_WITHIN: Duration = Duration::from_millis(500);

/// The first pause between the idle tick's own questions to an authenticator whose answer is
/// unavailable, and the longest; it doubles in between ([`LockMachine::tick`]).
pub const RE_ASK_FIRST: Duration = Duration::from_secs(5);
pub const RE_ASK_LAST: Duration = Duration::from_secs(5 * 60);

/// Called with the new state on every transition between locked and unlocked, on which the app
/// drops pending plans and ends every operation subscription, and on every update; on both it
/// emits `lock-changed`. It runs under the machine's lock, so it must be quick and must never
/// call back into the machine (see the module docs for the lock order). A panic in it is caught
/// and logged: the change stands, and whoever made it — the idle ticker among them — goes on.
pub type LockHook = Box<dyn Fn(&LockState, LockChange) + Send + Sync>;

/// What a numbered change to the state was.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockChange {
    /// Locked, or unlocked.
    Transition,
    /// The same lock, reported anew: its idle time changed.
    Update,
}

pub struct LockMachine {
    auth: Arc<AuthCache>,
    clock: Arc<dyn Clock>,
    hook: LockHook,
    inner: Mutex<Inner>,
    /// `inner.reason.is_some()`, for [`guard`](Self::guard). Every transition writes it under
    /// `inner`, before the hook hears of it, so it is never ahead of the state.
    locked: AtomicBool,
    /// Held by [`set_settings`](Self::set_settings) across `persist` and the apply.
    settings_write: Mutex<()>,
    /// When the tick next asks an unavailable authenticator again. Its lock is taken alone.
    re_ask: Mutex<ReAsk>,
}

/// The tick's own questions while the authenticator's answer is unavailable: when the next is
/// due on the monotonic clock (`None`: at the next tick), and the pause after it.
struct ReAsk {
    due_ms: Option<u64>,
    pause: Duration,
}

impl Default for ReAsk {
    fn default() -> Self {
        Self {
            due_ms: None,
            pause: RE_ASK_FIRST,
        }
    }
}

struct Inner {
    settings: Settings,
    /// `Some` while locked.
    reason: Option<LockReason>,
    /// When the current state began, on the wall clock: what the webview shows.
    since_ms: u64,
    /// The numbered changes so far: the current state's [`LockState::seq`].
    seq: u64,
    /// The idle time the current state reports ([`LockState::auto_lock_minutes`]).
    minutes: Option<u32>,
    /// The last activity (or unlock): the idle time runs from it.
    last_activity: Stamp,
    /// The unlock prompt while it is open; a second unlock is `AuthBusy` meanwhile.
    prompt: Option<Prompt>,
    /// Set for good by [`LockMachine::close`]: no unlock prompt opens from then on.
    closing: bool,
    /// Locked at start for want of the authenticator's answer, which has not come yet.
    provisional: bool,
    /// The store's secrets are protected (D.4a T74): the lock is in effect whatever the
    /// settings and the OS say, the pinned settings apply, and only the store's secret
    /// unlocks.
    protected: bool,
    /// O4's two clocks at the last look (a tick, a secret's use, an unlock): the sleep gap is
    /// measured from here.
    last_tick: ClockPair,
}

struct Prompt {
    cancel: CancellationToken,
    /// Set, under the machine's lock, when a lock or a quit closed the prompt: its answer no
    /// longer counts. The token trips on another thread, so this flag — not the token — decides,
    /// and a `Verified` that arrives before the token has tripped does not unlock either.
    closed: bool,
}

impl LockMachine {
    /// Locked with [`LockReason::Startup`] when the lock is in effect and `lock_on_start` is
    /// set; unlocked otherwise; locked provisionally when it would need the authenticator's
    /// answer and that does not come within [`STARTUP_WITHIN`] (see the module docs).
    ///
    /// The app builds the machine on its main thread, where no OS backend may be asked (Windows
    /// answers Hello's availability through a COM apartment the main thread does not pump): the
    /// cache asks on a thread of its own, and this waits at most the bound.
    pub fn new(
        settings: Settings,
        auth: Arc<AuthCache>,
        clock: Arc<dyn Clock>,
        hook: LockHook,
    ) -> Arc<Self> {
        Self::new_protected(settings, auth, clock, hook, false)
    }

    /// [`new`](Self::new) over a store whose secrets are `protected` (D.4a T74): then it
    /// starts locked whatever the settings say, asking the authenticator nothing first — the
    /// vault's own field opens it on every OS.
    pub fn new_protected(
        settings: Settings,
        auth: Arc<AuthCache>,
        clock: Arc<dyn Clock>,
        hook: LockHook,
        protected: bool,
    ) -> Arc<Self> {
        let now = Stamp::now(&*clock);
        let last_tick = ClockPair::now(&*clock);
        let (reason, provisional) = if protected {
            auth.refresh();
            (Some(LockReason::Startup), false)
        } else if settings.lock_enabled && settings.lock_on_start {
            match auth.fresh(STARTUP_WITHIN) {
                Some(info) => (
                    in_effect(&settings, &info, false).then_some(LockReason::Startup),
                    false,
                ),
                None => {
                    tracing::warn!(
                        "the authenticator did not answer within {STARTUP_WITHIN:?}: locked \
                         until it does"
                    );
                    (Some(LockReason::Startup), true)
                }
            }
        } else {
            auth.refresh();
            (None, false)
        };
        let minutes = reported_minutes(&settings, &auth.info(), protected);
        let machine = Arc::new(Self {
            auth,
            clock,
            hook,
            inner: Mutex::new(Inner {
                settings,
                reason,
                since_ms: now.wall_ms,
                seq: 0,
                minutes,
                last_activity: now,
                prompt: None,
                closing: false,
                provisional,
                protected,
                last_tick,
            }),
            locked: AtomicBool::new(reason.is_some()),
            settings_write: Mutex::new(()),
            re_ask: Mutex::default(),
        });
        let weak = Arc::downgrade(&machine);
        machine.auth.on_answer(move || {
            if let Some(machine) = weak.upgrade() {
                machine.auth_changed();
            }
        });
        // An answer that came in before the listener did.
        machine.auth_changed();
        machine
    }

    /// The authenticator answered again: a start locked provisionally learns whether it may
    /// stay locked (see the module docs), and a changed idle time is reported.
    fn auth_changed(&self) {
        let Some(info) = self.auth.known() else {
            return;
        };
        if info.available {
            // Any answer that the owner can be verified starts the tick's back-off over.
            *self.re_ask.lock().unwrap_or_else(|p| p.into_inner()) = ReAsk::default();
        }
        let close = {
            let mut inner = self.lock_inner();
            let provisional = std::mem::take(&mut inner.provisional);
            // Not on a protected store: the vault's own field opens it, whatever the OS says
            // (D.4a T75), so the lock stands.
            if provisional && !info.available && inner.seq == 0 && !inner.protected {
                tracing::info!(
                    "the authenticator's late answer: nothing can verify the owner here, so the \
                     start's lock is lifted"
                );
                let close = close_open_prompt(&mut inner);
                let now = self.clock.now_ms();
                self.enter(&mut inner, &info, None, now);
                close
            } else {
                self.report(&mut inner, &info);
                None
            }
        };
        trip_prompt(close);
    }

    /// The state now. Should the authenticator's answer have changed what it reports (its idle
    /// time), that is numbered first, as an update.
    pub fn state(&self) -> LockState {
        let info = self.auth.info();
        let mut inner = self.lock_inner();
        self.report(&mut inner, &info);
        state_of(&inner)
    }

    /// `Locked` for any command not in [`ALLOWED_WHILE_LOCKED`] while locked. It never
    /// waits — not for an open prompt, a transition's hook or a settings write: every command
    /// passes it on the main thread.
    pub fn guard(&self, command: &str) -> Result<(), DesktopError> {
        if self.locked.load(Ordering::SeqCst) && !ALLOWED_WHILE_LOCKED.contains(&command) {
            return Err(DesktopError::Locked);
        }
        Ok(())
    }

    /// The owner did something: the idle time starts again.
    pub fn activity(&self) {
        let now = Stamp::now(&*self.clock);
        self.lock_inner().last_activity = now;
    }

    /// Lock with [`LockReason::Idle`] once `auto_lock` minutes have passed since the last
    /// activity (or unlock), on either clock: a suspend counts, a wall clock stepped back does
    /// not. Does nothing while locked, with the lock not in effect, or with `auto_lock` set to
    /// never.
    ///
    /// While the authenticator has not answered, or its last answer is that nothing can verify
    /// the owner, it is asked again ([`AuthCache::refresh`]: off this thread, and one question
    /// however many ticks ask): a lock, a prompt or a save would ask, but none comes while the
    /// lock is off, so a first answer that was wrong for a moment (an OS still starting) would
    /// keep it off for good. An answer that stays unavailable (an administrator's refusal) is
    /// asked less and less often: [`RE_ASK_FIRST`] after the first question, doubling up to
    /// [`RE_ASK_LAST`], on the monotonic clock; any available answer starts that over. Once
    /// the answer is that the owner can be verified, a tick asks nothing. Only the tick backs
    /// off: a lock, a prompt, a save and `app_info` ask at once.
    pub fn tick(&self) {
        if self.auth.known().is_none_or(|info| !info.available) && self.re_ask_due() {
            self.auth.refresh();
        }
        let info = self.auth.info();
        let mut inner = self.lock_inner();
        // D.4a O4: a sleep the OS did not report, as the holder sees one ([`Self::look`]).
        let slept = self.look(&mut inner);
        self.report(&mut inner, &info);
        if inner.reason.is_some() || !in_effect(&inner.settings, &info, inner.protected) {
            return;
        }
        if slept && inner.protected {
            let now = self.clock.now_ms();
            self.enter(&mut inner, &info, Some(LockReason::OsSession), now);
            return;
        }
        let Some(minutes) = pinned(&inner.settings, inner.protected).auto_lock.minutes() else {
            return;
        };
        if inner.last_activity.elapsed_ms(&*self.clock) >= u64::from(minutes) * MINUTE_MS {
            let now = self.clock.now_ms();
            self.enter(&mut inner, &info, Some(LockReason::Idle), now);
        }
    }

    /// Whether the tick's next question is due; if so, the one after is due a pause later, and
    /// the pause doubles, up to [`RE_ASK_LAST`].
    fn re_ask_due(&self) -> bool {
        let now = self.clock.monotonic_ms();
        let mut re_ask = self.re_ask.lock().unwrap_or_else(|p| p.into_inner());
        if re_ask.due_ms.is_some_and(|due| now < due) {
            return false;
        }
        let pause = u64::try_from(re_ask.pause.as_millis()).unwrap_or(u64::MAX);
        re_ask.due_ms = Some(now.saturating_add(pause));
        re_ask.pause = (re_ask.pause * 2).min(RE_ASK_LAST);
        true
    }

    /// Lock for `reason`. Already locked, it keeps the reason and time it locked with, and
    /// closes an open unlock prompt (the session locked or slept while it was open). With
    /// the lock not in effect it does nothing: there would be no way to unlock.
    pub fn lock(&self, reason: LockReason) {
        self.lock_if(reason, |_| true);
    }

    /// The OS session locked or is going to sleep (`apprafter_os_auth::SessionEvent`): with the
    /// `lock_on_sleep` setting on, [`lock`](Self::lock) with [`LockReason::OsSession`] — the hook
    /// tells the page as it does of Lock now. With it off, nothing at all: the owner asked the
    /// app not to follow the OS, and an open unlock prompt stays open.
    pub fn os_session(&self) {
        self.lock_if(LockReason::OsSession, |settings| settings.lock_on_sleep);
    }

    /// [`lock`](Self::lock), when `wanted` says so of the settings in use, asked under the
    /// machine's lock so a settings change cannot fall between the two.
    fn lock_if(&self, reason: LockReason, wanted: impl FnOnce(&Settings) -> bool) {
        let info = self.auth.info();
        let close = {
            let mut inner = self.lock_inner();
            if !wanted(&pinned(&inner.settings, inner.protected)) {
                return;
            }
            if inner.reason.is_some() {
                // Already locked, an open unlock prompt is still closed: with lock-on-sleep on,
                // the app is locked after every session lock or sleep until an unlock that
                // began after it, so the yes of a prompt opened before must not count (the
                // owner may have left, or typed just as the lid closed). A prompt exists only
                // while the app is locked, so leaving it alone here would mean no session event
                // ever closed one. Each event closes only the prompt it finds open. That a
                // stream of events would close every prompt is why the watch hears only the
                // OS's own senders (on Linux it drops a signal sent to the app alone); anything
                // else that can send them runs as the owner, and could do worse.
                close_open_prompt(&mut inner)
            } else {
                if in_effect(&inner.settings, &info, inner.protected) {
                    // Read under the lock, as every transition reads it: a time read before
                    // could be older than a transition that took the lock first.
                    let now = self.clock.now_ms();
                    self.enter(&mut inner, &info, Some(reason), now);
                }
                None
            }
        };
        trip_prompt(close);
    }

    /// Quit: close an open unlock prompt, as the [`Authenticator`] promises the app does, so
    /// that a `Verified` arriving after this returns — even before the dialog has closed —
    /// does not unlock (its `unlock` answers `AuthCancelled`), and open none from here on:
    /// every later `unlock` of a locked app is refused with `Closing`, decided under the same
    /// lock hold that would open the prompt. Nothing else changes: the state stays as it is,
    /// and a lock still locks.
    pub fn close(&self) {
        let close = {
            let mut inner = self.lock_inner();
            inner.closing = true;
            close_open_prompt(&mut inner)
        };
        trip_prompt(close);
    }

    /// Ask the owner, and unlock if the OS verifies them. Blocks until the prompt answers
    /// (it can take minutes), without holding the machine's lock: meanwhile every other
    /// call answers at once, and a second `unlock` is `AuthBusy`. Unlocked already, it asks
    /// nothing; once the quit has [`close`](Self::close)d the machine, it asks nothing and
    /// answers `Closing`. On a protected store the OS opens nothing (D.4a T75): it is not
    /// asked, and the answer is `AuthUnavailable` with `UsePasswordField`.
    pub fn unlock(&self) -> Result<(), DesktopError> {
        self.unlock_by(
            Route::Os,
            |auth, cancel| {
                PasswordAnswer::from(auth.verify(&AuthPurpose::Unlock, cancel)).into_result()
            },
            |()| {},
        )
        .map_err(|refusal| *refusal.error)
    }

    /// [`unlock`](Self::unlock) with the password from the app's own field
    /// ([`Authenticator::verify_password`]) in place of the OS's prompt, by the same rules: one
    /// check at a time, with any `unlock` (`AuthBusy`), closed by a lock or a quit, a `Verified`
    /// that comes after that refused. A refusal carries what the OS said with it (PAM's
    /// messages). The password goes to the authenticator, or is wiped unasked. Refused, as
    /// `unlock` is, on a protected store.
    pub fn unlock_with_password(&self, password: Zeroizing<String>) -> Result<(), Refusal> {
        self.unlock_by(
            Route::Os,
            move |auth, cancel| {
                auth.verify_password(&AuthPurpose::Unlock, password, cancel)
                    .into_result()
            },
            |()| {},
        )
    }

    /// Unlock with the store's own secret (D.4a T74, T75): `open` opens the vault with what
    /// the person typed (the lock password or the recovery key: blocking, a derivation), and
    /// `commit` keeps what it opened — called under the machine's lock just before the
    /// transition, so the key is held exactly when the app is unlocked. By the unlock's
    /// rules: one check at a time with any unlock, and a lock, a session event or a quit
    /// during the check refuses its yes (`commit` is then never called, and what `open`
    /// opened is dropped).
    pub fn unlock_with_store_secret<T>(
        &self,
        open: impl FnOnce(&CancellationToken) -> Result<T, Refusal>,
        commit: impl FnOnce(T),
    ) -> Result<(), Refusal> {
        self.unlock_by(Route::StoreSecret, |_, cancel| open(cancel), commit)
    }

    /// The one unlock: `ask` is the check, run once a prompt slot is had; `commit` runs
    /// under the machine's lock right before the app unlocks. D.4b's gesture slots are routes
    /// here.
    fn unlock_by<T>(
        &self,
        route: Route,
        ask: impl FnOnce(&dyn Authenticator, &CancellationToken) -> Result<T, Refusal>,
        commit: impl FnOnce(T),
    ) -> Result<(), Refusal> {
        let cancel = {
            let mut inner = self.lock_inner();
            if inner.reason.is_none() {
                return Ok(());
            }
            // The quit's drain may take seconds: no prompt may open meanwhile, and none
            // between this check and the insert below, which is this same lock hold.
            if inner.closing {
                return Err(DesktopError::Closing.into());
            }
            if route == Route::Os && inner.protected {
                return Err(DesktopError::AuthUnavailable {
                    reason: UnavailableReason::UsePasswordField,
                }
                .into());
            }
            if inner.prompt.is_some() {
                return Err(DesktopError::AuthBusy.into());
            }
            let cancel = CancellationToken::new();
            inner.prompt = Some(Prompt {
                cancel: cancel.clone(),
                closed: false,
            });
            // O4's count for this check starts with it: a sleep while locked, before the
            // person typed, is no sleep of this unlock's.
            let started = ClockPair::now(&*self.clock);
            inner.last_tick = started;
            cancel
        };
        let answer = panic::catch_unwind(AssertUnwindSafe(|| ask(&*self.auth, &cancel)));
        let info = self.auth.info();
        let mut inner = self.lock_inner();
        let closed = inner.prompt.take().is_none_or(|prompt| prompt.closed);
        let answer = match answer {
            Ok(answer) => answer,
            // The prompt is closed above, so the next unlock can ask again.
            Err(payload) => {
                drop(inner);
                panic::resume_unwind(payload)
            }
        };
        match answer {
            // A lock or a quit closed it, or the store became protected while the OS asked:
            // the yes does not count, and says nothing worth showing.
            Ok(_) if closed || (route == Route::Os && inner.protected) => {
                Err(DesktopError::AuthCancelled.into())
            }
            Ok(value) => {
                // A sleep the OS did not report while the secret was checked voids the
                // check, as a reported one does (the final verifier's finding 9): nobody may
                // be there to see the app open. The look re-arms O4's count either way.
                if self.look(&mut inner) && inner.protected {
                    return Err(DesktopError::AuthCancelled.into());
                }
                commit(value);
                let now = Stamp::now(&*self.clock);
                inner.last_activity = now;
                self.enter(&mut inner, &info, None, now.wall_ms);
                Ok(())
            }
            Err(refusal) => Err(refusal),
        }
    }

    /// The store's secrets became protected or not (the shell's re-read, D.4a T74): the pins
    /// apply or lift, and a changed idle time is reported. Protected, nothing locks here: the
    /// shell locks with `SecretsLocked` when it holds no key ([`lock_unlocked`]). No longer
    /// protected while locked, with the lock not in effect by the saved settings and the
    /// authenticator, the app unlocks through a transition — a lock nobody could open is no
    /// lock (rule 3), and the secrets are in files again.
    ///
    /// [`lock_unlocked`]: Self::lock_unlocked
    pub fn set_protected(&self, protected: bool) {
        let info = self.auth.info();
        let close = {
            let mut inner = self.lock_inner();
            if inner.protected == protected {
                return;
            }
            inner.protected = protected;
            if !protected && inner.reason.is_some() && !in_effect(&inner.settings, &info, false) {
                let close = close_open_prompt(&mut inner);
                let now = self.clock.now_ms();
                self.enter(&mut inner, &info, None, now);
                close
            } else {
                self.report(&mut inner, &info);
                None
            }
        };
        trip_prompt(close);
    }

    /// Whether the store's secrets are protected, as the machine last heard.
    pub fn protected(&self) -> bool {
        self.lock_inner().protected
    }

    /// Whether the app is locked now; it never waits.
    pub fn is_locked(&self) -> bool {
        self.locked.load(Ordering::SeqCst)
    }

    /// Lock for `reason` when unlocked and the lock is in effect; a locked app, and an open
    /// unlock prompt, are left as they are. The shell's re-read: a store locked under an app
    /// that holds no key (`SecretsLocked`).
    pub fn lock_unlocked(&self, reason: LockReason) {
        self.lock_unlocked_unless(reason, || false);
    }

    /// [`lock_unlocked`](Self::lock_unlocked), unless `keep` says the app may stay
    /// unlocked. `keep` is asked under the machine's lock, which an unlock's `commit` holds
    /// too, so the shell's re-read never decides on a key an unlock is keeping that moment,
    /// nor locks the app that unlock just opened (the final verifier's finding 8). `keep`
    /// runs on a locked app too (it drops a key that is not the store's).
    pub fn lock_unlocked_unless(&self, reason: LockReason, keep: impl FnOnce() -> bool) {
        let info = self.auth.info();
        let mut inner = self.lock_inner();
        let stays = keep();
        if !stays && inner.reason.is_none() && in_effect(&inner.settings, &info, inner.protected) {
            let now = self.clock.now_ms();
            self.enter(&mut inner, &info, Some(reason), now);
        }
    }

    /// O4's look ([`ClockPair::slept_since`]): whether the machine slept since the last look,
    /// which this one becomes. On a protected store a sleep it finds while an unlock's check
    /// is open voids that check (its yes does not count: `closed`), as a reported sleep
    /// does — the tick may take the look before the check's own (final verifier 9).
    fn look(&self, inner: &mut Inner) -> bool {
        let now = ClockPair::now(&*self.clock);
        let slept = now.slept_since(inner.last_tick);
        inner.last_tick = now;
        if slept && inner.protected {
            if let Some(prompt) = inner.prompt.as_mut() {
                prompt.closed = true;
            }
        }
        slept
    }

    /// O4 between ticks: the vault access asks before each secret's use (Tasks 13, 15), so
    /// the first read after a wake the OS did not report already finds the app locked, as
    /// the holder refuses at the first request after one. On a protected, unlocked app a
    /// sleep locks it with `OsSession` (the lock hook drops the key). Whether it locked.
    pub fn check_sleep(&self) -> bool {
        let info = self.auth.info();
        let mut inner = self.lock_inner();
        let slept = self.look(&mut inner);
        if slept
            && inner.protected
            && inner.reason.is_none()
            && in_effect(&inner.settings, &info, true)
        {
            let now = self.clock.now_ms();
            self.enter(&mut inner, &info, Some(LockReason::OsSession), now);
            return true;
        }
        false
    }

    /// Replace the settings the machine follows, once `persist` has saved them.
    ///
    /// Switching the lock on is refused with `AuthUnavailable` when nothing can verify the
    /// owner; keeping it on is not (the defaults have it on, and every other setting must
    /// stay changeable). Switching it off never unlocks. A new idle time applies from the
    /// next tick, and a change to what the state reports is numbered, as an update.
    ///
    /// `persist` runs outside the machine's lock, so a slow disk holds up no command, lock or
    /// tick; a lock of its own, held across `persist` and the apply, makes concurrent changes
    /// reach the file and the machine in the same order. `persist` must not call back into
    /// the machine.
    pub fn set_settings(
        &self,
        settings: Settings,
        persist: impl FnOnce(&Settings) -> Result<(), DesktopError>,
    ) -> Result<(), DesktopError> {
        let _writing = self
            .settings_write
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let info = self.auth.info();
        // Only this function changes the settings, one call at a time: what is checked here
        // is still what the change applies over once `persist` returns.
        // D.4a T74, O7: while protected no pinned setting is switched off, nor Auto-lock set
        // to Never; a value already saved off stays (the person's own, back when protection
        // ends).
        {
            let inner = self.lock_inner();
            if inner.protected && turns_a_pin_off(&inner.settings, &settings) {
                return Err(DesktopError::LockRequired);
            }
        }
        let switching_on = settings.lock_enabled && !self.lock_inner().settings.lock_enabled;
        if switching_on && !info.available {
            return Err(DesktopError::AuthUnavailable {
                reason: info.unavailable.unwrap_or(UnavailableReason::NoBackend),
            });
        }
        persist(&settings)?;
        let mut inner = self.lock_inner();
        inner.settings = settings;
        self.report(&mut inner, &info);
        Ok(())
    }

    /// Move to `reason` (`None` = unlocked), numbered as the next change, and tell the hook;
    /// a panic in the hook is logged, and the transition stands. The one place the lock
    /// changes, always under the machine's lock. A lock first tells the authenticator
    /// ([`Authenticator::locked`]), so what the lock screen reads of it is fresh.
    fn enter(&self, inner: &mut Inner, info: &AuthInfo, reason: Option<LockReason>, now: u64) {
        inner.reason = reason;
        inner.since_ms = now;
        inner.seq += 1;
        inner.minutes = reported_minutes(&inner.settings, info, inner.protected);
        self.locked.store(reason.is_some(), Ordering::SeqCst);
        if reason.is_some() {
            if let Err(payload) = panic::catch_unwind(AssertUnwindSafe(|| self.auth.locked())) {
                tracing::error!(
                    "the authenticator's lock notice panicked: {}",
                    panic_message(&*payload)
                );
            }
        }
        self.tell(&state_of(inner), LockChange::Transition);
    }

    /// When the idle time the state reports is no longer what the settings and `info` make it,
    /// report it anew: an update, numbered as the next change. The other place the state
    /// changes, also under the machine's lock.
    fn report(&self, inner: &mut Inner, info: &AuthInfo) {
        let minutes = reported_minutes(&inner.settings, info, inner.protected);
        if minutes != inner.minutes {
            inner.minutes = minutes;
            inner.seq += 1;
            self.tell(&state_of(inner), LockChange::Update);
        }
    }

    /// The hook, with `state`; a panic in it is logged.
    fn tell(&self, state: &LockState, change: LockChange) {
        if let Err(payload) = panic::catch_unwind(AssertUnwindSafe(|| (self.hook)(state, change))) {
            tracing::error!(
                locked = state.locked,
                ?change,
                "the lock hook panicked: {}",
                panic_message(&*payload)
            );
        }
    }

    fn lock_inner(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }
}

/// Mark the open unlock prompt closed, under the machine's lock: from here its answer does not
/// count. Its token, to close the dialog, unless it was closed already.
fn close_open_prompt(inner: &mut Inner) -> Option<CancellationToken> {
    inner.prompt.as_mut().and_then(|prompt| {
        (!prompt.closed).then(|| {
            prompt.closed = true;
            prompt.cancel.clone()
        })
    })
}

/// Close the dialog, off this thread: `cancel` runs the backend's callbacks on the calling
/// thread and re-raises the first one's panic, and the caller may be the main thread.
fn trip_prompt(cancel: Option<CancellationToken>) {
    if let Some(cancel) = cancel {
        crate::ops::trip(cancel, "lock-cancel");
    }
}

/// How an unlock asks: the OS (its prompt, or its password through the app's field), or the
/// store's own secret (the vault's field, D.4a T75).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Route {
    Os,
    StoreSecret,
}

/// The settings the machine follows: the saved ones, or while the store is protected the
/// pinned ones (D.4a T74, O4, O7) — the lock on, at start and on sleep, and an idle time (a
/// saved `Never` reads as 30 min). The file keeps the person's own values.
pub fn pinned(settings: &Settings, protected: bool) -> Settings {
    if !protected {
        return settings.clone();
    }
    Settings {
        lock_enabled: true,
        lock_on_start: true,
        lock_on_sleep: true,
        auto_lock: if settings.auto_lock == AutoLock::Never {
            AutoLock::Min30
        } else {
            settings.auto_lock
        },
        ..settings.clone()
    }
}

/// Whether `new` switches off a pinned setting `old` has on, or moves Auto-lock to Never.
fn turns_a_pin_off(old: &Settings, new: &Settings) -> bool {
    (old.lock_enabled && !new.lock_enabled)
        || (old.lock_on_start && !new.lock_on_start)
        || (old.lock_on_sleep && !new.lock_on_sleep)
        || (old.auto_lock != AutoLock::Never && new.auto_lock == AutoLock::Never)
}

/// On a protected store the vault's own field always opens the app (D.4a T74): the lock
/// is in effect whatever the switch or the OS.
fn in_effect(settings: &Settings, info: &AuthInfo, protected: bool) -> bool {
    protected || (settings.lock_enabled && info.available)
}

/// The idle time a state reports: none while the lock is not in effect or set to never.
fn reported_minutes(settings: &Settings, info: &AuthInfo, protected: bool) -> Option<u32> {
    in_effect(settings, info, protected)
        .then(|| pinned(settings, protected).auto_lock.minutes())
        .flatten()
}

fn state_of(inner: &Inner) -> LockState {
    LockState {
        locked: inner.reason.is_some(),
        reason: inner.reason,
        since_ms: inner.since_ms,
        auto_lock_minutes: inner.minutes,
        seq: inner.seq,
    }
}

#[cfg(test)]
mod tests {
    use std::panic::{self, AssertUnwindSafe};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::SeqCst};
    use std::sync::mpsc::{self, RecvTimeoutError};
    use std::sync::{Arc, Mutex};
    use std::thread;
    use std::time::{Duration, Instant};

    use apprafter_core::CancellationToken;
    use apprafter_desktop_ipc::{
        errors, AuthInfo, AuthOutcome, AutoLock, CancelledBy, LockReason, LockState, Settings,
        Theme, UnavailableReason, ALLOWED_WHILE_LOCKED, COMMANDS,
    };
    use apprafter_os_auth::Action;

    use zeroize::Zeroizing;

    use super::{pinned, LockChange, LockMachine, STARTUP_WITHIN};
    use crate::auth::test_os::{self, Call, ScriptedOs};
    use crate::auth::{
        AuthPurpose, Authenticator, FakeAuthenticator, NoAuthenticator, PasswordAnswer,
    };
    use crate::auth_cache::AuthCache;
    use crate::errors::{DesktopError, Refusal};
    use crate::ops::test_clock::ManualClock;
    use crate::ops::test_trips;

    const T0: u64 = 1_700_000_000_000;
    const MIN: u64 = 60_000;
    const DAY: u64 = 24 * 60 * MIN;
    /// What a test waits before it calls something stuck.
    const LONG: Duration = Duration::from_secs(10);

    /// Every state the hook was called with, and why.
    #[derive(Default)]
    struct Hooked(Mutex<Vec<(LockState, LockChange)>>);

    impl Hooked {
        /// The states of every call, transitions and updates.
        fn calls(&self) -> Vec<LockState> {
            self.0
                .lock()
                .unwrap()
                .iter()
                .map(|(s, _)| s.clone())
                .collect()
        }

        /// What each call was.
        fn changes(&self) -> Vec<LockChange> {
            self.0.lock().unwrap().iter().map(|(_, c)| *c).collect()
        }
    }

    struct Rig {
        clock: Arc<ManualClock>,
        hooked: Arc<Hooked>,
        machine: Arc<LockMachine>,
        cache: Arc<AuthCache>,
    }

    /// The machine over `auth`, once the authenticator's first answer is in.
    fn rig(settings: Settings, auth: Arc<dyn Authenticator>) -> Rig {
        let r = rig_unsettled(settings, auth);
        assert!(r.cache.settled(LONG), "the authenticator never answered");
        r
    }

    /// The machine over `auth`, as it returns from its start.
    fn rig_unsettled(settings: Settings, auth: Arc<dyn Authenticator>) -> Rig {
        let clock = Arc::new(ManualClock::at(T0));
        let hooked = Arc::new(Hooked::default());
        let cache = AuthCache::new(auth);
        let machine = {
            let hooked = hooked.clone();
            LockMachine::new(
                settings,
                cache.clone(),
                clock.clone(),
                Box::new(move |state, change| {
                    hooked.0.lock().unwrap().push((state.clone(), change))
                }),
            )
        };
        Rig {
            clock,
            hooked,
            machine,
            cache,
        }
    }

    fn fake() -> Arc<FakeAuthenticator> {
        Arc::new(FakeAuthenticator::new())
    }

    /// Lock on, no lock at start, 10 minutes idle.
    fn unlocked_at_start() -> Settings {
        Settings {
            lock_on_start: false,
            ..Settings::default()
        }
    }

    /// Locked for `reason`, by transition `seq` (0: as constructed).
    fn locked(
        reason: LockReason,
        since_ms: u64,
        auto_lock_minutes: Option<u32>,
        seq: u64,
    ) -> LockState {
        LockState {
            locked: true,
            reason: Some(reason),
            since_ms,
            auto_lock_minutes,
            seq,
        }
    }

    /// Unlocked, by transition `seq` (0: as constructed).
    fn unlocked(since_ms: u64, auto_lock_minutes: Option<u32>, seq: u64) -> LockState {
        LockState {
            locked: false,
            reason: None,
            since_ms,
            auto_lock_minutes,
            seq,
        }
    }

    /// Cannot authenticate, for `reason` (or for no reason given).
    struct Unavailable(Option<UnavailableReason>);

    impl Authenticator for Unavailable {
        fn info(&self) -> AuthInfo {
            AuthInfo {
                available: false,
                unavailable: self.0,
                ..NoAuthenticator.info()
            }
        }

        fn verify(&self, _purpose: &AuthPurpose, _cancel: &CancellationToken) -> AuthOutcome {
            AuthOutcome::Unavailable {
                reason: self.0.unwrap_or(UnavailableReason::NoBackend),
            }
        }
    }

    /// An OS prompt that stays open until the test answers it. It says when it opened and
    /// when its token tripped (the app closing it).
    struct HeldPrompt {
        opened: Mutex<mpsc::Sender<()>>,
        tripped: Mutex<mpsc::Sender<()>>,
        answer: Mutex<mpsc::Receiver<AuthOutcome>>,
    }

    struct HeldPromptEnds {
        opened: mpsc::Receiver<()>,
        tripped: mpsc::Receiver<()>,
        answer: mpsc::Sender<AuthOutcome>,
    }

    impl HeldPrompt {
        fn new() -> (Arc<Self>, HeldPromptEnds) {
            let (opened_tx, opened) = mpsc::channel();
            let (tripped_tx, tripped) = mpsc::channel();
            let (answer, answer_rx) = mpsc::channel();
            let prompt = Arc::new(Self {
                opened: Mutex::new(opened_tx),
                tripped: Mutex::new(tripped_tx),
                answer: Mutex::new(answer_rx),
            });
            let ends = HeldPromptEnds {
                opened,
                tripped,
                answer,
            };
            (prompt, ends)
        }
    }

    impl Authenticator for HeldPrompt {
        fn info(&self) -> AuthInfo {
            FakeAuthenticator::new().info()
        }

        fn verify(&self, _purpose: &AuthPurpose, cancel: &CancellationToken) -> AuthOutcome {
            let tripped = self.tripped.lock().unwrap().clone();
            let _registration = cancel.on_cancel(move || {
                let _ = tripped.send(());
            });
            let _ = self.opened.lock().unwrap().send(());
            // A test that failed drops the sender: answer at once rather than hang.
            self.answer
                .lock()
                .unwrap()
                .recv_timeout(LONG)
                .unwrap_or(AuthOutcome::Cancelled {
                    by: CancelledBy::App,
                })
        }

        /// The password check stays open the same way.
        fn verify_password(
            &self,
            purpose: &AuthPurpose,
            _password: Zeroizing<String>,
            cancel: &CancellationToken,
        ) -> PasswordAnswer {
            self.verify(purpose, cancel).into()
        }
    }

    /// Runs `f` on a thread of its own, so a deadlock fails the test after `LONG` instead
    /// of hanging it; a panic in `f` is re-raised here.
    fn within<T: Send + 'static>(what: &str, f: impl FnOnce() -> T + Send + 'static) -> T {
        let (tx, rx) = mpsc::channel();
        let handle = thread::spawn(move || {
            let _ = tx.send(f());
        });
        match rx.recv_timeout(LONG) {
            Ok(value) => value,
            Err(RecvTimeoutError::Timeout) => {
                panic!("{what} did not return within {LONG:?}: blocked behind the prompt")
            }
            Err(RecvTimeoutError::Disconnected) => match handle.join() {
                Err(panic) => panic::resume_unwind(panic),
                Ok(()) => panic!("{what} ended without a result"),
            },
        }
    }

    /// Starts `machine.unlock()` on a thread; its result arrives on the receiver.
    fn unlock_in_background(
        machine: &Arc<LockMachine>,
    ) -> mpsc::Receiver<Result<(), DesktopError>> {
        let (tx, rx) = mpsc::channel();
        let machine = machine.clone();
        thread::spawn(move || {
            let _ = tx.send(machine.unlock());
        });
        rx
    }

    /// What the owner typed into the lock screen's field.
    fn typed(password: &str) -> Zeroizing<String> {
        Zeroizing::new(password.to_owned())
    }

    /// Starts `machine.unlock_with_password(password)` on a thread; its error, if any, arrives
    /// on the receiver.
    fn password_unlock_in_background(
        machine: &Arc<LockMachine>,
        password: &str,
    ) -> mpsc::Receiver<Result<(), DesktopError>> {
        let (tx, rx) = mpsc::channel();
        let (machine, password) = (machine.clone(), typed(password));
        thread::spawn(move || {
            let _ = tx.send(
                machine
                    .unlock_with_password(password)
                    .map_err(|refusal| *refusal.error),
            );
        });
        rx
    }

    /// The OS's own authenticator ([`crate::auth::SystemAuthenticator`]) over an OS that
    /// reports `info` and answers every prompt `verify`.
    fn os(info: AuthInfo, verify: AuthOutcome) -> (Arc<dyn Authenticator>, test_os::Calls) {
        let (auth, calls) = test_os::system(ScriptedOs::new(info, verify));
        (auth, calls)
    }

    /// An OS that can prompt (polkit, say).
    fn os_available() -> AuthInfo {
        AuthInfo {
            available: true,
            method: Some(apprafter_desktop_ipc::AuthMethod::Polkit),
            unavailable: None,
            biometrics_choice: false,
            password_field: false,
        }
    }

    /// An OS that cannot verify the owner here, for `reason`.
    fn os_unavailable(reason: UnavailableReason) -> AuthInfo {
        AuthInfo {
            available: false,
            method: None,
            unavailable: Some(reason),
            biometrics_choice: false,
            password_field: false,
        }
    }

    // 1–3. Whether the lock is in effect, and the state at start.

    /// Says which thread asked it for its info.
    struct NamesItsAsker(Mutex<Vec<thread::ThreadId>>);

    impl Authenticator for NamesItsAsker {
        fn info(&self) -> AuthInfo {
            self.0.lock().unwrap().push(thread::current().id());
            FakeAuthenticator::new().info()
        }

        fn verify(&self, _purpose: &AuthPurpose, _cancel: &CancellationToken) -> AuthOutcome {
            AuthOutcome::Verified
        }
    }

    #[test]
    fn the_start_asks_the_authenticator_off_the_building_thread() {
        // The app builds the machine on the main thread, where an OS backend must never be
        // asked (Windows answers through a COM apartment the main thread does not pump).
        let auth = Arc::new(NamesItsAsker(Mutex::new(Vec::new())));
        let r = rig(Settings::default(), auth.clone());
        assert_eq!(
            r.machine.state(),
            locked(LockReason::Startup, T0, Some(10), 0),
            "its answer still decides the start"
        );
        r.machine.tick();
        r.machine.lock(LockReason::OsSession);
        let askers = auth.0.lock().unwrap().clone();
        assert!(!askers.is_empty());
        assert!(
            askers.iter().all(|asker| *asker != thread::current().id()),
            "asked on this thread"
        );
    }

    // I-2, I-3. The machine reads the kept answer, never the OS, and starts within its bound.

    /// An OS whose every `info` waits for the test to answer it, for as long as the test holds
    /// its sender (dropped, it answers "no authenticator" at once); prompts verify at once.
    /// Never on its own: a call that waits for the OS's answer waits until `within`'s guard
    /// fails it, so no test tells "waited" by the clock (GOTCHA-332).
    struct Gated {
        answers: Mutex<mpsc::Receiver<AuthInfo>>,
        asked: AtomicUsize,
    }

    impl Gated {
        fn new() -> (Arc<Self>, mpsc::Sender<AuthInfo>) {
            let (send, answers) = mpsc::channel();
            let gated = Arc::new(Self {
                answers: Mutex::new(answers),
                asked: AtomicUsize::new(0),
            });
            (gated, send)
        }
    }

    impl Authenticator for Gated {
        fn info(&self) -> AuthInfo {
            self.asked.fetch_add(1, SeqCst);
            self.answers
                .lock()
                .unwrap()
                .recv()
                .unwrap_or_else(|_| NoAuthenticator.info())
        }

        fn verify(&self, _purpose: &AuthPurpose, _cancel: &CancellationToken) -> AuthOutcome {
            AuthOutcome::Verified
        }
    }

    /// The machine over `auth`, started on a thread of its own, so a start that waits for ever
    /// fails the test instead of hanging it.
    fn started(settings: Settings, auth: Arc<dyn Authenticator>) -> Rig {
        within("the start", move || rig_unsettled(settings, auth))
    }

    #[test]
    fn the_lock_reads_the_kept_answer_and_an_os_that_stops_answering_holds_nothing_up() {
        let (os, answer) = Gated::new();
        let r = started(unlocked_at_start(), os.clone());
        answer.send(FakeAuthenticator::new().info()).unwrap();
        assert!(r.cache.settled(LONG));
        // From here every question hangs: a call that waited for it would never return.
        r.cache.refresh();
        {
            let (machine, clock) = (r.machine.clone(), r.clock.clone());
            within("the lock's calls", move || {
                machine.tick();
                let _ = machine.state();
                machine
                    .set_settings(unlocked_at_start(), |_| Ok(()))
                    .unwrap();
                machine.os_session();
                assert!(machine.state().locked, "the session event locked it");
                machine.unlock().unwrap();
                clock.set(T0 + 10 * MIN);
                machine.tick();
            });
        }
        assert_eq!(r.cache.waits(), [LONG], "the calls waited for nothing");
        assert_eq!(
            r.machine.state().reason,
            Some(LockReason::Idle),
            "the idle lock still fired"
        );
        drop(answer);
    }

    #[test]
    fn without_lock_on_start_the_start_asks_nothing_and_waits_for_nothing() {
        let (os, answer) = Gated::new();
        let r = started(unlocked_at_start(), os.clone());
        assert!(
            r.cache.waits().is_empty(),
            "it waited: {:?}",
            r.cache.waits()
        );
        assert_eq!(r.machine.state(), unlocked(T0, Some(10), 0));
        drop(answer);
    }

    #[test]
    fn a_slow_answer_starts_locked_within_the_bound() {
        let (os, answer) = Gated::new();
        let r = started(Settings::default(), os.clone());
        // It waited with its bound, and only that, and the answer never comes on its own: the
        // start gave up at the bound (GOTCHA-332: the record, not the clock).
        assert_eq!(r.cache.waits(), [STARTUP_WITHIN]);
        assert_eq!(
            r.machine.state(),
            locked(LockReason::Startup, T0, Some(10), 0)
        );
        assert!(matches!(
            r.machine.guard("op_list"),
            Err(DesktopError::Locked)
        ));
        drop(answer);
    }

    #[test]
    fn a_late_unavailable_unlocks_through_a_transition_the_page_hears() {
        let (os, answer) = Gated::new();
        let r = started(Settings::default(), os.clone());
        assert!(r.machine.state().locked);
        r.clock.set(T0 + MIN);
        answer.send(NoAuthenticator.info()).unwrap();
        assert!(r.cache.settled(LONG));
        let open = unlocked(T0 + MIN, None, 1);
        assert_eq!(
            r.hooked.calls(),
            std::slice::from_ref(&open),
            "the page heard it"
        );
        assert_eq!(r.hooked.changes(), [LockChange::Transition]);
        assert_eq!(r.machine.state(), open, "rule 3's end state");
        assert!(r.machine.guard("op_list").is_ok());
    }

    #[test]
    fn a_late_available_keeps_it_locked() {
        let (os, answer) = Gated::new();
        let r = started(Settings::default(), os.clone());
        answer.send(FakeAuthenticator::new().info()).unwrap();
        assert!(r.cache.settled(LONG));
        assert_eq!(
            r.machine.state(),
            locked(LockReason::Startup, T0, Some(10), 0)
        );
        assert!(r.hooked.calls().is_empty());
    }

    #[test]
    fn a_late_answer_after_the_owner_acted_changes_nothing() {
        let (os, answer) = Gated::new();
        let r = started(Settings::default(), os.clone());
        // The owner unlocks (the prompt is the OS's own, asked afresh) and locks again.
        r.machine.unlock().unwrap();
        r.clock.set(T0 + MIN);
        r.machine.lock(LockReason::Manual);
        answer.send(NoAuthenticator.info()).unwrap();
        // The first answer, then the question the unlock and the lock asked meanwhile.
        answer.send(NoAuthenticator.info()).unwrap();
        assert!(r.cache.settled(LONG));
        let state = r.machine.state();
        assert!(state.locked, "{state:?}");
        assert_eq!(state.reason, Some(LockReason::Manual));
        assert_eq!(
            r.hooked.changes(),
            [
                LockChange::Transition,
                LockChange::Transition,
                LockChange::Update
            ],
            "unlock, lock, then only the idle time it no longer has"
        );
        assert_eq!(state.auto_lock_minutes, None);
    }

    #[test]
    fn a_late_unavailable_closes_an_open_unlock_prompt() {
        let (prompt, ends) = HeldPrompt::new();
        let (os, answer) = Gated::new();
        struct Both(Arc<Gated>, Arc<HeldPrompt>);
        impl Authenticator for Both {
            fn info(&self) -> AuthInfo {
                self.0.info()
            }
            fn verify(&self, purpose: &AuthPurpose, cancel: &CancellationToken) -> AuthOutcome {
                self.1.verify(purpose, cancel)
            }
        }
        let r = started(Settings::default(), Arc::new(Both(os, prompt)));
        let first = unlock_in_background(&r.machine);
        ends.opened.recv_timeout(LONG).expect("the prompt opened");
        answer.send(NoAuthenticator.info()).unwrap();
        ends.tripped
            .recv_timeout(LONG)
            .expect("the unlock closed the prompt");
        ends.answer.send(AuthOutcome::Verified).unwrap();
        let result = first.recv_timeout(LONG).expect("the unlock returned");
        assert!(
            matches!(result, Err(DesktopError::AuthCancelled)),
            "{result:?}"
        );
        assert_eq!(
            r.hooked.changes(),
            [LockChange::Transition],
            "unlocked once"
        );
        assert!(!r.machine.state().locked);
        drop(answer);
    }

    #[test]
    fn with_an_authenticator_the_default_settings_start_locked() {
        let r = rig(Settings::default(), fake());
        assert_eq!(
            r.machine.state(),
            locked(LockReason::Startup, T0, Some(10), 0)
        );
        assert!(
            r.hooked.calls().is_empty(),
            "the state at start is not a transition"
        );
    }

    #[test]
    fn without_lock_on_start_or_with_the_lock_off_it_starts_unlocked() {
        let r = rig(unlocked_at_start(), fake());
        assert_eq!(r.machine.state(), unlocked(T0, Some(10), 0));
        let off = Settings {
            lock_enabled: false,
            ..Settings::default()
        };
        let r = rig(off, fake());
        assert_eq!(
            r.machine.state(),
            unlocked(T0, None, 0),
            "no idle time when off"
        );
    }

    #[test]
    fn without_an_authenticator_the_lock_is_off_even_when_the_settings_say_on() {
        let r = rig(Settings::default(), Arc::new(NoAuthenticator));
        assert_eq!(r.machine.state(), unlocked(T0, None, 0));
        // Nothing locks it: there would be no way back.
        r.machine.lock(LockReason::Manual);
        r.clock.set(T0 + 1000 * MIN);
        r.machine.tick();
        assert_eq!(r.machine.state(), unlocked(T0, None, 0));
        assert!(r.hooked.calls().is_empty());
    }

    #[test]
    fn the_lock_cannot_be_switched_on_without_an_authenticator() {
        for (auth, reason) in [
            (
                Arc::new(NoAuthenticator) as Arc<dyn Authenticator>,
                UnavailableReason::NoBackend,
            ),
            (
                Arc::new(Unavailable(Some(UnavailableReason::PolicyMissing))),
                UnavailableReason::PolicyMissing,
            ),
            (Arc::new(Unavailable(None)), UnavailableReason::NoBackend),
        ] {
            let off = Settings {
                lock_enabled: false,
                ..Settings::default()
            };
            let r = rig(off.clone(), auth);
            let persisted = AtomicUsize::new(0);
            let on = Settings {
                lock_enabled: true,
                ..off.clone()
            };
            let err = r
                .machine
                .set_settings(on, |_| {
                    persisted.fetch_add(1, SeqCst);
                    Ok(())
                })
                .unwrap_err();
            assert!(
                matches!(err, DesktopError::AuthUnavailable { reason: got } if got == reason),
                "{err:?}"
            );
            assert_eq!(persisted.load(SeqCst), 0, "a refused change is not saved");
        }
    }

    #[test]
    fn a_lock_already_on_can_stay_on_without_an_authenticator() {
        // The defaults have the lock on; refusing every save that keeps it on would make
        // every other setting unchangeable on a machine without an authenticator.
        let r = rig(Settings::default(), Arc::new(NoAuthenticator));
        let light = Settings {
            theme: Theme::Light,
            ..Settings::default()
        };
        r.machine.set_settings(light, |_| Ok(())).unwrap();
        assert_eq!(r.machine.state(), unlocked(T0, None, 0));
    }

    // 3a. The same rules with the OS's own authenticator: in effect where the OS can verify the
    // owner, failing closed where it cannot.

    #[test]
    fn with_an_os_that_can_verify_the_lock_is_in_effect() {
        let (auth, calls) = os(os_available(), AuthOutcome::Verified);
        let r = rig(Settings::default(), auth);
        assert_eq!(
            r.machine.state(),
            locked(LockReason::Startup, T0, Some(10), 0)
        );
        r.machine.unlock().unwrap();
        assert!(!r.machine.state().locked);
        assert!(
            calls
                .lock()
                .unwrap()
                .contains(&Call::Verify(Action::Unlock)),
            "{calls:?}"
        );
        // Off, it can be switched on again.
        let off = Settings {
            lock_enabled: false,
            ..Settings::default()
        };
        let (auth, _) = os(os_available(), AuthOutcome::Verified);
        let r = rig(off, auth);
        r.machine
            .set_settings(Settings::default(), |_| Ok(()))
            .unwrap();
        r.machine.lock(LockReason::Manual);
        assert!(r.machine.state().locked);
    }

    #[test]
    fn with_an_os_that_cannot_verify_the_lock_still_fails_closed() {
        for reason in [
            UnavailableReason::NotPermittedHere,
            UnavailableReason::NoPamService,
            UnavailableReason::NotConfigured,
        ] {
            let (auth, _) = os(os_unavailable(reason), AuthOutcome::Unavailable { reason });
            let r = rig(Settings::default(), auth);
            assert_eq!(r.machine.state(), unlocked(T0, None, 0), "{reason:?}");
            r.machine.lock(LockReason::Manual);
            r.machine.lock(LockReason::OsSession);
            r.clock.set(T0 + 1000 * MIN);
            r.machine.tick();
            assert_eq!(r.machine.state(), unlocked(T0, None, 0), "{reason:?}");
            assert!(r.hooked.calls().is_empty(), "{reason:?}");

            let (auth, _) = os(os_unavailable(reason), AuthOutcome::Unavailable { reason });
            let off = Settings {
                lock_enabled: false,
                ..Settings::default()
            };
            let r = rig(off, auth);
            let err = r
                .machine
                .set_settings(Settings::default(), |_| Ok(()))
                .unwrap_err();
            assert!(
                matches!(err, DesktopError::AuthUnavailable { reason: got } if got == reason),
                "{err:?}"
            );
        }
    }

    // 4. Idle.

    #[test]
    fn it_locks_after_the_idle_time_without_activity() {
        let r = rig(unlocked_at_start(), fake());
        r.clock.set(T0 + 10 * MIN - 1);
        r.machine.tick();
        assert!(!r.machine.state().locked);
        r.clock.set(T0 + 10 * MIN);
        r.machine.tick();
        let idle = locked(LockReason::Idle, T0 + 10 * MIN, Some(10), 1);
        assert_eq!(r.machine.state(), idle);
        assert_eq!(r.hooked.calls(), vec![idle.clone()]);
        // While locked, ticks do nothing.
        r.clock.set(T0 + 100 * MIN);
        r.machine.tick();
        assert_eq!(r.machine.state(), idle);
        assert_eq!(r.hooked.calls().len(), 1);
    }

    #[test]
    fn activity_restarts_the_idle_time() {
        let r = rig(unlocked_at_start(), fake());
        r.clock.set(T0 + 9 * MIN);
        r.machine.activity();
        r.clock.set(T0 + 10 * MIN);
        r.machine.tick();
        r.clock.set(T0 + 19 * MIN - 1);
        r.machine.tick();
        assert!(!r.machine.state().locked);
        r.clock.set(T0 + 19 * MIN);
        r.machine.tick();
        assert_eq!(
            r.machine.state(),
            locked(LockReason::Idle, T0 + 19 * MIN, Some(10), 1)
        );
    }

    #[test]
    fn never_never_idles_and_neither_does_a_lock_switched_off() {
        let never = Settings {
            auto_lock: AutoLock::Never,
            ..unlocked_at_start()
        };
        let off = Settings {
            lock_enabled: false,
            ..Settings::default()
        };
        for settings in [never, off] {
            let r = rig(settings.clone(), fake());
            r.clock.set(T0 + 1000 * 24 * 60 * MIN);
            r.machine.tick();
            assert!(!r.machine.state().locked, "{settings:?}");
            assert_eq!(r.machine.state().auto_lock_minutes, None, "{settings:?}");
            assert!(r.hooked.calls().is_empty());
        }
    }

    #[test]
    fn a_changed_idle_time_applies_at_once() {
        let r = rig(unlocked_at_start(), fake());
        let five = Settings {
            auto_lock: AutoLock::Min5,
            ..unlocked_at_start()
        };
        r.machine.set_settings(five, |_| Ok(())).unwrap();
        assert_eq!(r.machine.state().auto_lock_minutes, Some(5));
        r.clock.set(T0 + 5 * MIN);
        r.machine.tick();
        assert_eq!(r.machine.state().reason, Some(LockReason::Idle));
    }

    #[test]
    fn a_wall_clock_stepped_back_does_not_postpone_the_idle_lock() {
        let r = rig(unlocked_at_start(), fake());
        r.clock.advance(5 * MIN);
        // A time sync, or the owner changing the date: the wall clock goes back a day.
        r.clock.set_wall(T0 - DAY);
        r.clock.advance(5 * MIN);
        r.machine.tick();
        assert_eq!(
            r.machine.state(),
            locked(LockReason::Idle, T0 - DAY + 5 * MIN, Some(10), 1),
            "ten minutes passed, whatever the wall clock says"
        );
    }

    #[test]
    fn a_suspend_past_the_idle_time_locks_on_the_next_tick() {
        let r = rig(unlocked_at_start(), fake());
        // Asleep for 11 minutes: the monotonic clock stood still, the wall clock did not.
        r.clock.set_wall(T0 + 11 * MIN);
        r.machine.tick();
        assert_eq!(
            r.machine.state(),
            locked(LockReason::Idle, T0 + 11 * MIN, Some(10), 1)
        );
    }

    /// Answers that nothing can verify the owner its first `unavailable` times (an OS not ready
    /// yet), then that something can; counts the questions.
    struct UnavailableAtFirst {
        unavailable: usize,
        asked: AtomicUsize,
    }

    impl Authenticator for UnavailableAtFirst {
        fn info(&self) -> AuthInfo {
            if self.asked.fetch_add(1, SeqCst) < self.unavailable {
                NoAuthenticator.info()
            } else {
                FakeAuthenticator::new().info()
            }
        }

        fn verify(&self, _purpose: &AuthPurpose, _cancel: &CancellationToken) -> AuthOutcome {
            AuthOutcome::Verified
        }
    }

    /// Nothing but a lock, a prompt, a save or `app_info` asks the OS again, and none of them
    /// comes while the lock is off: an "unavailable" that was wrong for a moment must not keep
    /// it off for good. Each tick asks again while that is the answer — and only then.
    #[test]
    fn each_tick_asks_again_while_the_answer_is_unavailable_and_only_then() {
        let auth = Arc::new(UnavailableAtFirst {
            unavailable: 2,
            asked: AtomicUsize::new(0),
        });
        let r = rig(unlocked_at_start(), auth.clone());
        assert_eq!(r.machine.state().auto_lock_minutes, None, "not in effect");
        for (tick, in_effect) in [(1, false), (2, true)] {
            // The idle ticker's interval, which is also the first pause between re-asks.
            r.clock.advance(5_000);
            r.machine.tick();
            assert!(r.cache.settled(LONG));
            assert_eq!(auth.asked.load(SeqCst), tick + 1, "tick {tick} asked again");
            assert_eq!(
                r.machine.state().auto_lock_minutes.is_some(),
                in_effect,
                "after tick {tick}"
            );
        }
        // In effect: a tick asks nothing.
        for _ in 0..3 {
            r.machine.tick();
        }
        assert!(r.cache.settled(LONG));
        assert_eq!(auth.asked.load(SeqCst), 3, "ticks asked nothing more");
        r.clock.set(T0 + 10 * MIN);
        r.machine.tick();
        assert_eq!(r.machine.state().reason, Some(LockReason::Idle));
    }

    /// An OS whose answer the test switches; counts the questions.
    #[derive(Default)]
    struct Switched {
        available: AtomicBool,
        asked: AtomicUsize,
    }

    impl Authenticator for Switched {
        fn info(&self) -> AuthInfo {
            self.asked.fetch_add(1, SeqCst);
            if self.available.load(SeqCst) {
                FakeAuthenticator::new().info()
            } else {
                NoAuthenticator.info()
            }
        }

        fn verify(&self, _purpose: &AuthPurpose, _cancel: &CancellationToken) -> AuthOutcome {
            AuthOutcome::Verified
        }
    }

    /// An answer that stays "unavailable" (an administrator's `rules.d` NO, say) is not asked
    /// again every 5 s for good: the tick's own re-asks wait 5 s, then 10, 20 … up to 5
    /// minutes, on the monotonic clock, and any available answer starts them over.
    #[test]
    fn the_tick_s_re_asks_back_off_to_five_minutes_and_an_available_answer_resets_them() {
        let os = Arc::new(Switched::default());
        let r = rig(unlocked_at_start(), os.clone());
        let asked = || {
            assert!(r.cache.settled(LONG));
            os.asked.load(SeqCst)
        };
        // A tick every 5 s, from `from` to `to` seconds after T0: the seconds it asked at.
        let ticks = |from: u64, to: u64| {
            let mut asked_at = Vec::new();
            for second in (from..=to).step_by(5) {
                r.clock.set(T0 + second * 1000);
                let before = asked();
                r.machine.tick();
                if asked() > before {
                    asked_at.push(second);
                }
            }
            asked_at
        };
        assert_eq!(ticks(5, 900), [5, 10, 20, 40, 80, 160, 320, 620]);
        // A wall clock stepped forward is no time passing.
        let before = asked();
        r.clock.set_wall(T0 + 900_000 + DAY);
        r.machine.tick();
        assert_eq!(asked(), before, "the wall clock moved the re-ask");
        r.clock.set_wall(T0 + 900_000);
        assert_eq!(ticks(905, 1220), [920, 1220], "five minutes apart at most");

        // An available answer, from anything that asks (here as `app_info` would).
        os.available.store(true, SeqCst);
        assert!(r.cache.fresh(LONG).is_some_and(|info| info.available));
        os.available.store(false, SeqCst);
        r.cache.refresh();
        assert!(r.cache.settled(LONG));
        assert_eq!(
            r.machine.state().auto_lock_minutes,
            None,
            "unavailable again"
        );
        assert_eq!(ticks(1225, 1400), [1225, 1230, 1240, 1260, 1300, 1380]);
    }

    #[test]
    fn an_unlock_counts_as_activity() {
        let r = rig(Settings::default(), fake());
        r.clock.set(T0 + 30 * MIN);
        r.machine.unlock().unwrap();
        r.clock.set(T0 + 30 * MIN + 1);
        r.machine.tick();
        assert_eq!(r.machine.state(), unlocked(T0 + 30 * MIN, Some(10), 1));
    }

    // 5. Lock and unlock.

    #[test]
    fn lock_now_locks_and_a_verified_owner_unlocks() {
        let auth = fake();
        let r = rig(unlocked_at_start(), auth.clone());
        r.clock.set(T0 + MIN);
        r.machine.lock(LockReason::Manual);
        assert_eq!(
            r.machine.state(),
            locked(LockReason::Manual, T0 + MIN, Some(10), 1)
        );
        r.clock.set(T0 + 2 * MIN);
        r.machine.unlock().unwrap();
        assert_eq!(r.machine.state(), unlocked(T0 + 2 * MIN, Some(10), 2));
        assert_eq!(auth.asked(), vec![AuthPurpose::Unlock]);
    }

    #[test]
    fn locking_a_locked_app_keeps_why_and_since() {
        let r = rig(Settings::default(), fake());
        r.clock.set(T0 + MIN);
        r.machine.lock(LockReason::Manual);
        r.machine.lock(LockReason::OsSession);
        assert_eq!(
            r.machine.state(),
            locked(LockReason::Startup, T0, Some(10), 0)
        );
        assert!(r.hooked.calls().is_empty(), "no transition happened");
    }

    #[test]
    fn unlocking_an_unlocked_app_asks_nothing() {
        let auth = fake();
        let r = rig(unlocked_at_start(), auth.clone());
        r.machine.unlock().unwrap();
        assert!(auth.asked().is_empty());
        assert!(r.hooked.calls().is_empty());
    }

    #[test]
    fn anything_but_verified_keeps_it_locked() {
        for (outcome, expected) in [
            (
                AuthOutcome::Cancelled {
                    by: CancelledBy::User,
                },
                DesktopError::AuthCancelled,
            ),
            (
                AuthOutcome::Failed {
                    exhausted: true,
                    retry_in_ms: Some(29_000),
                },
                DesktopError::AuthFailed {
                    exhausted: true,
                    retry_in_ms: Some(29_000),
                },
            ),
            (
                AuthOutcome::Failed {
                    exhausted: true,
                    retry_in_ms: None,
                },
                DesktopError::AuthFailed {
                    exhausted: true,
                    retry_in_ms: None,
                },
            ),
            (
                AuthOutcome::Failed {
                    exhausted: false,
                    retry_in_ms: None,
                },
                DesktopError::AuthFailed {
                    exhausted: false,
                    retry_in_ms: None,
                },
            ),
            (
                AuthOutcome::Unavailable {
                    reason: UnavailableReason::NoAgent,
                },
                DesktopError::AuthUnavailable {
                    reason: UnavailableReason::NoAgent,
                },
            ),
            (AuthOutcome::Busy, DesktopError::AuthBusy),
        ] {
            let auth = fake();
            auth.then(outcome);
            let r = rig(Settings::default(), auth.clone());
            let err = r.machine.unlock().unwrap_err();
            assert_eq!(
                err.to_ui(),
                expected.to_ui(),
                "{outcome:?} gave {err:?}, not {expected:?}"
            );
            assert_eq!(
                r.machine.state(),
                locked(LockReason::Startup, T0, Some(10), 0),
                "{outcome:?}"
            );
            assert!(r.hooked.calls().is_empty(), "{outcome:?}");
            // The prompt is closed: the next attempt asks again.
            r.machine.unlock().unwrap();
            assert_eq!(auth.asked().len(), 2, "{outcome:?}");
        }
    }

    #[test]
    fn a_second_unlock_while_the_prompt_is_open_is_busy_and_nothing_waits_for_the_prompt() {
        let (prompt, ends) = HeldPrompt::new();
        let r = rig(Settings::default(), prompt);
        let first = unlock_in_background(&r.machine);
        ends.opened.recv_timeout(LONG).expect("the prompt opened");
        let (second, state, guard) = within("a second unlock", {
            let machine = r.machine.clone();
            move || (machine.unlock(), machine.state(), machine.guard("op_list"))
        });
        assert!(matches!(second, Err(DesktopError::AuthBusy)), "{second:?}");
        assert!(state.locked);
        assert!(matches!(guard, Err(DesktopError::Locked)), "{guard:?}");
        assert!(
            ends.opened.try_recv().is_err(),
            "the second unlock opened no prompt"
        );
        ends.answer.send(AuthOutcome::Verified).unwrap();
        first
            .recv_timeout(LONG)
            .expect("the first unlock returned")
            .unwrap();
        assert!(!r.machine.state().locked);
        assert_eq!(r.hooked.calls().len(), 1);
    }

    #[test]
    fn a_lock_while_the_prompt_is_open_closes_it_and_a_late_yes_does_not_unlock() {
        let (prompt, ends) = HeldPrompt::new();
        let r = rig(Settings::default(), prompt);
        let first = unlock_in_background(&r.machine);
        ends.opened.recv_timeout(LONG).expect("the prompt opened");
        r.machine.lock(LockReason::OsSession);
        ends.tripped
            .recv_timeout(LONG)
            .expect("the lock closed the prompt");
        // The OS says yes anyway, as the session locks.
        ends.answer.send(AuthOutcome::Verified).unwrap();
        let result = first.recv_timeout(LONG).expect("the unlock returned");
        assert!(
            matches!(result, Err(DesktopError::AuthCancelled)),
            "{result:?}"
        );
        assert_eq!(
            r.machine.state(),
            locked(LockReason::Startup, T0, Some(10), 0)
        );
        assert!(r.hooked.calls().is_empty());
        // Closed is closed: the next unlock opens a new prompt.
        let again = unlock_in_background(&r.machine);
        ends.opened.recv_timeout(LONG).expect("a new prompt opened");
        ends.answer.send(AuthOutcome::Verified).unwrap();
        again.recv_timeout(LONG).expect("it returned").unwrap();
        assert!(!r.machine.state().locked);
    }

    #[test]
    fn a_yes_that_beats_the_closing_thread_does_not_unlock_either() {
        let (prompt, ends) = HeldPrompt::new();
        let r = rig(Settings::default(), prompt);
        let first = unlock_in_background(&r.machine);
        ends.opened.recv_timeout(LONG).expect("the prompt opened");
        // A lock closes the prompt by tripping its token on a thread of its own. Holding that
        // thread back makes the OS answer yes after `lock` returned and before the dialog
        // closed — the window a scheduler opens only sometimes — so only the machine's own
        // record that the prompt was closed can refuse the answer.
        let ((), held) = {
            let machine = r.machine.clone();
            within("lock", move || {
                test_trips::held(|| machine.lock(LockReason::OsSession))
            })
        };
        assert_eq!(held.len(), 1, "the lock closes the prompt");
        assert!(
            ends.tripped.try_recv().is_err(),
            "held back, the token has not tripped"
        );
        ends.answer.send(AuthOutcome::Verified).unwrap();
        let result = first.recv_timeout(LONG).expect("the unlock returned");
        assert!(
            matches!(result, Err(DesktopError::AuthCancelled)),
            "{result:?}"
        );
        assert_eq!(
            r.machine.state(),
            locked(LockReason::Startup, T0, Some(10), 0)
        );
        assert!(r.hooked.calls().is_empty());
        for token in held {
            token.cancel();
        }
    }

    #[test]
    fn a_quit_closes_the_open_prompt_and_a_late_yes_does_not_unlock() {
        let (prompt, ends) = HeldPrompt::new();
        let r = rig(Settings::default(), prompt);
        let first = unlock_in_background(&r.machine);
        ends.opened.recv_timeout(LONG).expect("the prompt opened");
        r.machine.close();
        ends.tripped
            .recv_timeout(LONG)
            .expect("the quit closed the prompt");
        ends.answer.send(AuthOutcome::Verified).unwrap();
        let result = first.recv_timeout(LONG).expect("the unlock returned");
        assert!(
            matches!(result, Err(DesktopError::AuthCancelled)),
            "{result:?}"
        );
        assert_eq!(
            r.machine.state(),
            locked(LockReason::Startup, T0, Some(10), 0)
        );
        assert!(r.hooked.calls().is_empty());
    }

    #[test]
    fn a_yes_that_beats_the_quits_closing_thread_does_not_unlock_either() {
        let (prompt, ends) = HeldPrompt::new();
        let r = rig(Settings::default(), prompt);
        let first = unlock_in_background(&r.machine);
        ends.opened.recv_timeout(LONG).expect("the prompt opened");
        // As with a lock: the token trips on a thread of its own, held back here so the OS
        // answers yes after `close` returned and before the dialog closed.
        let ((), held) = {
            let machine = r.machine.clone();
            within("close", move || test_trips::held(|| machine.close()))
        };
        assert_eq!(held.len(), 1, "the quit closes the prompt");
        assert!(
            ends.tripped.try_recv().is_err(),
            "held back, the token has not tripped"
        );
        ends.answer.send(AuthOutcome::Verified).unwrap();
        let result = first.recv_timeout(LONG).expect("the unlock returned");
        assert!(
            matches!(result, Err(DesktopError::AuthCancelled)),
            "{result:?}"
        );
        assert_eq!(
            r.machine.state(),
            locked(LockReason::Startup, T0, Some(10), 0)
        );
        assert!(r.hooked.calls().is_empty());
        for token in held {
            token.cancel();
        }
    }

    #[test]
    fn once_the_quit_closed_the_machine_no_unlock_opens_a_prompt() {
        let auth = fake();
        let r = rig(Settings::default(), auth.clone());
        r.machine.close();
        let result = r.machine.unlock();
        assert!(matches!(result, Err(DesktopError::Closing)), "{result:?}");
        assert!(auth.asked().is_empty(), "no prompt opened during the quit");
        assert_eq!(
            r.machine.state(),
            locked(LockReason::Startup, T0, Some(10), 0)
        );
        // Sticky: a later try is refused the same way, and an unlocked app asks nothing.
        assert!(matches!(r.machine.unlock(), Err(DesktopError::Closing)));
        let r = rig(unlocked_at_start(), auth.clone());
        r.machine.close();
        r.machine.unlock().unwrap();
        assert!(auth.asked().is_empty());
    }

    #[test]
    fn a_quit_that_closes_the_machine_after_a_prompt_opened_still_refuses_its_yes() {
        // The other order: the prompt opened before the quit, which closes it; the next try is
        // refused without a prompt.
        let (prompt, ends) = HeldPrompt::new();
        let r = rig(Settings::default(), prompt);
        let first = unlock_in_background(&r.machine);
        ends.opened.recv_timeout(LONG).expect("the prompt opened");
        r.machine.close();
        ends.tripped.recv_timeout(LONG).expect("the quit closed it");
        let again = within("a second unlock", {
            let machine = r.machine.clone();
            move || machine.unlock()
        });
        assert!(matches!(again, Err(DesktopError::Closing)), "{again:?}");
        ends.answer.send(AuthOutcome::Verified).unwrap();
        let result = first.recv_timeout(LONG).expect("the unlock returned");
        assert!(
            matches!(result, Err(DesktopError::AuthCancelled)),
            "{result:?}"
        );
        assert!(r.machine.state().locked);
        assert!(
            ends.opened.try_recv().is_err(),
            "the second unlock opened no prompt"
        );
    }

    #[test]
    fn closing_with_no_prompt_open_leaves_the_state_as_it_was() {
        for settings in [Settings::default(), unlocked_at_start()] {
            let r = rig(settings, fake());
            let before = r.machine.state();
            let ((), held) = test_trips::held(|| r.machine.close());
            assert!(held.is_empty(), "no prompt, nothing to close");
            assert_eq!(r.machine.state(), before);
            assert!(r.hooked.calls().is_empty());
        }
        // A prompt closed once is not closed again (its token trips once).
        let (prompt, ends) = HeldPrompt::new();
        let r = rig(Settings::default(), prompt);
        let first = unlock_in_background(&r.machine);
        ends.opened.recv_timeout(LONG).expect("the prompt opened");
        let ((), held) = test_trips::held(|| {
            r.machine.close();
            r.machine.close();
            r.machine.lock(LockReason::OsSession);
        });
        assert_eq!(held.len(), 1);
        ends.answer.send(AuthOutcome::Verified).unwrap();
        assert!(matches!(
            first.recv_timeout(LONG).expect("the unlock returned"),
            Err(DesktopError::AuthCancelled)
        ));
        for token in held {
            token.cancel();
        }
    }

    #[test]
    fn a_panicking_prompt_leaves_the_next_unlock_free_to_ask() {
        struct PanicsOnce(AtomicBool);
        impl Authenticator for PanicsOnce {
            fn info(&self) -> AuthInfo {
                FakeAuthenticator::new().info()
            }
            fn verify(&self, _purpose: &AuthPurpose, _cancel: &CancellationToken) -> AuthOutcome {
                if !self.0.swap(true, SeqCst) {
                    panic!("the OS prompt broke");
                }
                AuthOutcome::Verified
            }
        }
        let r = rig(
            Settings::default(),
            Arc::new(PanicsOnce(AtomicBool::new(false))),
        );
        let machine = r.machine.clone();
        assert!(panic::catch_unwind(AssertUnwindSafe(|| machine.unlock())).is_err());
        assert!(r.machine.state().locked);
        r.machine
            .unlock()
            .expect("not busy behind the broken prompt");
        assert!(!r.machine.state().locked);
    }

    // 5a. The lock screen's own password field.

    #[test]
    fn the_right_password_unlocks_and_a_wrong_one_is_refused_with_what_pam_said() {
        let auth = Arc::new(FakeAuthenticator::new().with_password("open sesame".to_owned()));
        auth.saying(&["Authentication failure"]);
        let r = rig(Settings::default(), auth.clone());
        let refusal = r
            .machine
            .unlock_with_password(typed("open sesame!"))
            .unwrap_err();
        assert!(
            matches!(
                *refusal.error,
                DesktopError::AuthFailed {
                    exhausted: false,
                    retry_in_ms: None
                }
            ),
            "{refusal:?}"
        );
        assert_eq!(refusal.messages, ["Authentication failure"]);
        assert_eq!(
            r.machine.state(),
            locked(LockReason::Startup, T0, Some(10), 0)
        );
        assert!(r.hooked.calls().is_empty());
        // The check is over: the next one runs.
        r.clock.set(T0 + MIN);
        r.machine
            .unlock_with_password(typed("open sesame"))
            .unwrap();
        let open = unlocked(T0 + MIN, Some(10), 1);
        assert_eq!(r.machine.state(), open);
        assert_eq!(r.hooked.calls(), vec![open]);
        assert_eq!(auth.asked(), vec![AuthPurpose::Unlock, AuthPurpose::Unlock]);
        // Unlocked, nothing is checked.
        r.machine.unlock_with_password(typed("guess")).unwrap();
        assert_eq!(auth.asked().len(), 2);
    }

    /// The back-off refuses: the page is told how long it still will, with what PAM said.
    #[test]
    fn a_password_refused_by_the_back_off_says_how_long_it_still_refuses() {
        let auth = Arc::new(FakeAuthenticator::new().with_password("open sesame".to_owned()));
        auth.saying(&["Authentication failure"])
            .then(AuthOutcome::Failed {
                exhausted: true,
                retry_in_ms: Some(17_000),
            });
        let r = rig(Settings::default(), auth.clone());
        let refusal = r
            .machine
            .unlock_with_password(typed("open sesame"))
            .unwrap_err();
        let ui = refusal.to_ui();
        assert_eq!(ui.code.as_deref(), Some(errors::AUTH_FAILED), "{ui:?}");
        assert_eq!(ui.fields["exhausted"], serde_json::json!(true));
        assert_eq!(ui.fields["retryInMs"], serde_json::json!(17_000));
        assert_eq!(
            ui.fields["messages"],
            serde_json::json!(["Authentication failure"])
        );
        assert!(r.machine.state().locked);
    }

    #[test]
    fn where_the_os_prompts_itself_the_field_unlocks_nothing() {
        let auth = fake();
        let r = rig(Settings::default(), auth.clone());
        let refusal = r
            .machine
            .unlock_with_password(typed("anything"))
            .unwrap_err();
        assert!(
            matches!(
                *refusal.error,
                DesktopError::AuthUnavailable {
                    reason: UnavailableReason::UseSystemPrompt
                }
            ),
            "{refusal:?}"
        );
        assert!(r.machine.state().locked);
        r.machine.unlock().unwrap();
        assert!(!r.machine.state().locked, "the slot was given back");
    }

    #[test]
    fn a_password_check_and_an_os_prompt_never_run_together() {
        // The OS's prompt is open: the field is busy, and checks nothing.
        let (prompt, ends) = HeldPrompt::new();
        let r = rig(Settings::default(), prompt);
        let first = unlock_in_background(&r.machine);
        ends.opened.recv_timeout(LONG).expect("the prompt opened");
        let second = within("a password unlock", {
            let machine = r.machine.clone();
            move || machine.unlock_with_password(typed("pw"))
        });
        assert!(
            matches!(second, Err(ref refusal) if matches!(*refusal.error, DesktopError::AuthBusy)),
            "{second:?}"
        );
        assert!(ends.opened.try_recv().is_err(), "nothing else was asked");
        ends.answer.send(AuthOutcome::Verified).unwrap();
        first.recv_timeout(LONG).expect("it returned").unwrap();

        // The field is being checked: the OS's prompt is busy.
        r.machine.lock(LockReason::Manual);
        let first = password_unlock_in_background(&r.machine, "pw");
        ends.opened.recv_timeout(LONG).expect("the check began");
        let second = within("an unlock", {
            let machine = r.machine.clone();
            move || machine.unlock()
        });
        assert!(matches!(second, Err(DesktopError::AuthBusy)), "{second:?}");
        assert!(ends.opened.try_recv().is_err(), "nothing else was asked");
        ends.answer.send(AuthOutcome::Verified).unwrap();
        first.recv_timeout(LONG).expect("it returned").unwrap();
        assert!(!r.machine.state().locked);
    }

    #[test]
    fn a_lock_during_the_password_check_refuses_its_yes() {
        let (prompt, ends) = HeldPrompt::new();
        let r = rig(Settings::default(), prompt);
        let checking = password_unlock_in_background(&r.machine, "pw");
        ends.opened.recv_timeout(LONG).expect("the check began");
        r.machine.lock(LockReason::OsSession);
        ends.tripped
            .recv_timeout(LONG)
            .expect("the lock cancelled the check");
        ends.answer.send(AuthOutcome::Verified).unwrap();
        let result = checking.recv_timeout(LONG).expect("it returned");
        assert!(
            matches!(result, Err(DesktopError::AuthCancelled)),
            "{result:?}"
        );
        assert_eq!(
            r.machine.state(),
            locked(LockReason::Startup, T0, Some(10), 0)
        );
    }

    #[test]
    fn once_the_quit_closed_the_machine_no_password_is_checked() {
        let auth = Arc::new(FakeAuthenticator::new().with_password("pw".to_owned()));
        let r = rig(Settings::default(), auth.clone());
        r.machine.close();
        let refusal = r.machine.unlock_with_password(typed("pw")).unwrap_err();
        assert!(
            matches!(*refusal.error, DesktopError::Closing),
            "{refusal:?}"
        );
        assert!(auth.asked().is_empty());
        assert!(r.machine.state().locked);
    }

    /// Counts the lock notices it is given.
    #[derive(Default)]
    struct CountsLocks {
        fake: FakeAuthenticator,
        locks: AtomicUsize,
    }

    impl Authenticator for CountsLocks {
        fn info(&self) -> AuthInfo {
            self.fake.info()
        }

        fn verify(&self, purpose: &AuthPurpose, cancel: &CancellationToken) -> AuthOutcome {
            self.fake.verify(purpose, cancel)
        }

        fn locked(&self) {
            self.locks.fetch_add(1, SeqCst);
        }
    }

    #[test]
    fn every_lock_tells_the_authenticator_before_the_page_hears_of_it() {
        let auth = Arc::new(CountsLocks::default());
        let heard = Arc::new(Mutex::new(Vec::new()));
        let clock = Arc::new(ManualClock::at(T0));
        let machine = {
            let (auth, heard) = (auth.clone(), heard.clone());
            LockMachine::new(
                unlocked_at_start(),
                AuthCache::new(auth.clone()),
                clock.clone(),
                Box::new(move |state, _| {
                    heard
                        .lock()
                        .unwrap()
                        .push((state.locked, auth.locks.load(SeqCst)));
                }),
            )
        };
        machine.lock(LockReason::Manual);
        machine.lock(LockReason::OsSession);
        machine.unlock().unwrap();
        clock.set(T0 + 10 * MIN);
        machine.tick();
        assert_eq!(auth.locks.load(SeqCst), 2, "a lock each, not the unlock");
        assert_eq!(
            *heard.lock().unwrap(),
            [(true, 1), (false, 1), (true, 2)],
            "told before the hook ran"
        );
    }

    #[test]
    fn a_panicking_lock_notice_neither_undoes_the_lock_nor_skips_the_hook() {
        struct Panics;
        impl Authenticator for Panics {
            fn info(&self) -> AuthInfo {
                FakeAuthenticator::new().info()
            }
            fn verify(&self, _purpose: &AuthPurpose, _cancel: &CancellationToken) -> AuthOutcome {
                AuthOutcome::Verified
            }
            fn locked(&self) {
                panic!("the notice broke");
            }
        }
        let r = rig(unlocked_at_start(), Arc::new(Panics));
        r.machine.lock(LockReason::Manual);
        assert!(r.machine.state().locked);
        assert_eq!(r.hooked.calls().len(), 1);
        assert!(matches!(
            r.machine.guard("op_list"),
            Err(DesktopError::Locked)
        ));
    }

    // 5b. The OS session locked or went to sleep.

    #[test]
    fn an_os_session_event_locks_when_lock_on_sleep_is_on() {
        let r = rig(unlocked_at_start(), fake());
        r.clock.set(T0 + MIN);
        r.machine.os_session();
        let session = locked(LockReason::OsSession, T0 + MIN, Some(10), 1);
        assert_eq!(r.machine.state(), session);
        assert_eq!(
            r.hooked.calls(),
            vec![session.clone()],
            "the page hears it as it hears Lock now"
        );
        // Locked already: it keeps why and since, as a lock does.
        r.machine.os_session();
        assert_eq!(r.machine.state(), session);
        assert_eq!(r.hooked.calls().len(), 1);
    }

    #[test]
    fn without_lock_on_sleep_or_with_the_lock_not_in_effect_an_os_session_event_does_nothing() {
        let no_sleep = Settings {
            lock_on_sleep: false,
            ..unlocked_at_start()
        };
        let off = Settings {
            lock_enabled: false,
            ..unlocked_at_start()
        };
        for (settings, auth) in [
            (no_sleep, fake() as Arc<dyn Authenticator>),
            (off, fake()),
            (unlocked_at_start(), Arc::new(NoAuthenticator)),
        ] {
            let r = rig(settings.clone(), auth);
            let before = r.hooked.calls();
            r.machine.os_session();
            assert!(!r.machine.state().locked, "{settings:?}");
            // Without an authenticator the start reported an idle time until it answered,
            // and then an update without one: no transition, before or after the event.
            assert!(
                !r.hooked.changes().contains(&LockChange::Transition),
                "{settings:?}"
            );
            assert_eq!(
                r.hooked.calls(),
                before,
                "{settings:?}: the event changed nothing"
            );
        }
    }

    #[test]
    fn an_os_session_event_closes_the_open_unlock_prompt_only_when_lock_on_sleep_is_on() {
        // On: the session locked while the prompt was open, so its yes no longer counts.
        let (prompt, ends) = HeldPrompt::new();
        let r = rig(Settings::default(), prompt);
        let first = unlock_in_background(&r.machine);
        ends.opened.recv_timeout(LONG).expect("the prompt opened");
        r.machine.os_session();
        ends.tripped
            .recv_timeout(LONG)
            .expect("the event closed it");
        ends.answer.send(AuthOutcome::Verified).unwrap();
        let result = first.recv_timeout(LONG).expect("the unlock returned");
        assert!(
            matches!(result, Err(DesktopError::AuthCancelled)),
            "{result:?}"
        );

        // Off: the owner asked for no reaction to the OS, so the prompt stays open.
        let (prompt, ends) = HeldPrompt::new();
        let no_sleep = Settings {
            lock_on_sleep: false,
            ..Settings::default()
        };
        let r = rig(no_sleep, prompt);
        let first = unlock_in_background(&r.machine);
        ends.opened.recv_timeout(LONG).expect("the prompt opened");
        r.machine.os_session();
        assert!(
            ends.tripped
                .recv_timeout(Duration::from_millis(100))
                .is_err(),
            "the prompt was left open"
        );
        ends.answer.send(AuthOutcome::Verified).unwrap();
        first
            .recv_timeout(LONG)
            .expect("the unlock returned")
            .unwrap();
        assert!(!r.machine.state().locked);
    }

    /// The decision in `lock_if`: a session event on a locked app closes the unlock prompt it
    /// finds open, so that prompt's yes does not unlock after the session locked or slept, and
    /// only that one: a prompt opened after the event is the owner answering anew, and unlocks.
    #[test]
    fn a_session_event_closes_the_open_prompt_and_not_the_next() {
        let (prompt, ends) = HeldPrompt::new();
        let r = rig(Settings::default(), prompt);
        let before = unlock_in_background(&r.machine);
        ends.opened.recv_timeout(LONG).expect("the prompt opened");
        r.machine.os_session();
        ends.tripped
            .recv_timeout(LONG)
            .expect("the event closed the prompt it found");
        ends.answer.send(AuthOutcome::Verified).unwrap();
        let result = before.recv_timeout(LONG).expect("the unlock returned");
        assert!(
            matches!(result, Err(DesktopError::AuthCancelled)),
            "{result:?}"
        );
        assert!(r.machine.state().locked);

        let after = unlock_in_background(&r.machine);
        ends.opened.recv_timeout(LONG).expect("a new prompt opened");
        assert!(
            ends.tripped
                .recv_timeout(Duration::from_millis(100))
                .is_err(),
            "the event closed a prompt opened after it"
        );
        ends.answer.send(AuthOutcome::Verified).unwrap();
        after
            .recv_timeout(LONG)
            .expect("the unlock returned")
            .unwrap();
        assert!(!r.machine.state().locked);
    }

    #[test]
    fn lock_on_sleep_follows_the_saved_settings() {
        let r = rig(unlocked_at_start(), fake());
        let no_sleep = Settings {
            lock_on_sleep: false,
            ..unlocked_at_start()
        };
        r.machine.set_settings(no_sleep, |_| Ok(())).unwrap();
        r.machine.os_session();
        assert!(!r.machine.state().locked);
        r.machine
            .set_settings(unlocked_at_start(), |_| Ok(()))
            .unwrap();
        r.machine.os_session();
        assert_eq!(r.machine.state().reason, Some(LockReason::OsSession));
    }

    // 6. The hook.

    #[test]
    fn every_transition_calls_the_hook_once_with_the_new_state() {
        let r = rig(unlocked_at_start(), fake());
        r.clock.set(T0 + MIN);
        r.machine.lock(LockReason::Manual);
        r.machine.lock(LockReason::Manual);
        r.clock.set(T0 + 2 * MIN);
        r.machine.unlock().unwrap();
        r.machine.unlock().unwrap();
        r.clock.set(T0 + 12 * MIN);
        r.machine.tick();
        r.clock.set(T0 + 13 * MIN);
        r.machine.tick();
        r.clock.set(T0 + 14 * MIN);
        r.machine.unlock().unwrap();
        assert_eq!(
            r.hooked.calls(),
            vec![
                locked(LockReason::Manual, T0 + MIN, Some(10), 1),
                unlocked(T0 + 2 * MIN, Some(10), 2),
                locked(LockReason::Idle, T0 + 12 * MIN, Some(10), 3),
                unlocked(T0 + 14 * MIN, Some(10), 4),
            ]
        );
    }

    #[test]
    fn transitions_on_two_threads_are_numbered_in_the_order_they_happen() {
        // An OS session lock and the owner's unlock land on two threads, here all within one
        // millisecond (the clock stands still): every state the hook is told of, and every
        // state read back, carries the number of the transition it follows, one more each time.
        // Each thread keeps at its own act until the two together made enough transitions, so
        // the count does not depend on how the scheduler runs them.
        const TRANSITIONS: usize = 100;
        const READS_KEPT: usize = 1_000;
        let r = rig(unlocked_at_start(), fake());
        let spin = |act: fn(&LockMachine)| {
            let (machine, hooked) = (r.machine.clone(), r.hooked.clone());
            thread::spawn(move || {
                let deadline = Instant::now() + LONG;
                let mut read = Vec::new();
                while hooked.calls().len() < TRANSITIONS && Instant::now() < deadline {
                    act(&machine);
                    if read.len() < READS_KEPT {
                        read.push(machine.state());
                    }
                }
                read
            })
        };
        let locking = spin(|machine| machine.lock(LockReason::OsSession));
        // A lock that lands while the prompt is open closes it: AuthCancelled.
        let unlocking = spin(|machine| drop(machine.unlock()));
        let mut read = locking.join().unwrap();
        read.extend(unlocking.join().unwrap());
        let hooked = r.hooked.calls();
        assert!(hooked.len() >= TRANSITIONS, "{} transitions", hooked.len());
        for (i, state) in hooked.iter().enumerate() {
            assert_eq!(state.seq, i as u64 + 1, "{hooked:?}");
            assert_eq!(state.locked, i % 2 == 0, "locks and unlocks alternate");
            assert_eq!(
                state.since_ms, T0,
                "one millisecond: only the number orders them"
            );
        }
        // A state read back is the one its number entered: the two never come apart.
        for state in read {
            let entered = match state.seq {
                0 => unlocked(T0, Some(10), 0),
                seq => hooked[usize::try_from(seq).unwrap() - 1].clone(),
            };
            assert_eq!(state, entered);
        }
        assert_eq!(r.machine.state().seq, hooked.len() as u64);
    }

    #[test]
    fn a_panicking_hook_neither_undoes_the_transition_nor_stops_later_ticks() {
        let clock = Arc::new(ManualClock::at(T0));
        let calls = Arc::new(AtomicUsize::new(0));
        let machine = {
            let calls = calls.clone();
            LockMachine::new(
                unlocked_at_start(),
                AuthCache::new(fake()),
                clock.clone(),
                Box::new(move |_, _| {
                    if calls.fetch_add(1, SeqCst) == 0 {
                        panic!("the hook broke");
                    }
                }),
            )
        };
        // The idle ticker calls this: a panic out of it would end the ticker, and with it
        // every later auto-lock.
        clock.set(T0 + 10 * MIN);
        machine.tick();
        assert_eq!(
            machine.state(),
            locked(LockReason::Idle, T0 + 10 * MIN, Some(10), 1)
        );
        assert!(matches!(
            machine.guard("op_list"),
            Err(DesktopError::Locked)
        ));
        clock.set(T0 + 11 * MIN);
        machine.unlock().unwrap();
        clock.set(T0 + 21 * MIN);
        machine.tick();
        assert_eq!(
            machine.state(),
            locked(LockReason::Idle, T0 + 21 * MIN, Some(10), 3)
        );
        assert_eq!(calls.load(SeqCst), 3, "the hook heard every transition");
    }

    // 7. The guard.

    #[test]
    fn locked_it_answers_only_the_allowed_commands() {
        let r = rig(Settings::default(), fake());
        for command in COMMANDS {
            let result = r.machine.guard(command);
            if ALLOWED_WHILE_LOCKED.contains(command) {
                assert!(result.is_ok(), "{command}: {result:?}");
            } else {
                assert!(
                    matches!(result, Err(DesktopError::Locked)),
                    "{command}: {result:?}"
                );
            }
        }
        assert!(matches!(
            r.machine.guard("not_a_command"),
            Err(DesktopError::Locked)
        ));
        assert!(
            COMMANDS.len() > ALLOWED_WHILE_LOCKED.len(),
            "the test needs refused commands"
        );
    }

    #[test]
    fn unlocked_it_answers_every_command() {
        let r = rig(unlocked_at_start(), fake());
        for command in COMMANDS {
            assert!(r.machine.guard(command).is_ok(), "{command}");
        }
    }

    #[test]
    fn the_guard_answers_while_a_transition_holds_the_machine() {
        // The hook runs under the machine's lock; every command passes the guard, on the main
        // thread, so the guard must never wait for that lock.
        let (entered_tx, entered) = mpsc::channel();
        let (release, release_rx) = mpsc::channel::<()>();
        let gate = Mutex::new((entered_tx, release_rx));
        let machine = LockMachine::new(
            unlocked_at_start(),
            AuthCache::new(fake()),
            Arc::new(ManualClock::at(T0)),
            Box::new(move |_, _| {
                let gate = gate.lock().unwrap();
                let _ = gate.0.send(());
                // Until the test releases it, or fails and drops the sender.
                let _ = gate.1.recv();
            }),
        );
        let locking = {
            let machine = machine.clone();
            thread::spawn(move || machine.lock(LockReason::Manual))
        };
        entered.recv_timeout(LONG).expect("the hook ran");
        let guard = {
            let machine = machine.clone();
            within("the guard", move || machine.guard("op_list"))
        };
        assert!(
            matches!(guard, Err(DesktopError::Locked)),
            "the state the hook is told of is the one the guard enforces: {guard:?}"
        );
        release.send(()).unwrap();
        locking.join().unwrap();
        assert!(machine.state().locked);
    }

    // Settings.

    #[test]
    fn a_failed_save_leaves_the_settings_in_use_as_they_were() {
        let r = rig(unlocked_at_start(), fake());
        let never = Settings {
            auto_lock: AutoLock::Never,
            ..unlocked_at_start()
        };
        let err = r
            .machine
            .set_settings(never, |_| Err(DesktopError::SettingsIo("disk full".into())))
            .unwrap_err();
        assert!(matches!(err, DesktopError::SettingsIo(_)), "{err:?}");
        assert_eq!(r.machine.state().auto_lock_minutes, Some(10));
        r.clock.set(T0 + 10 * MIN);
        r.machine.tick();
        assert!(r.machine.state().locked);
    }

    #[test]
    fn the_saved_settings_are_the_ones_applied() {
        let r = rig(unlocked_at_start(), fake());
        let never = Settings {
            auto_lock: AutoLock::Never,
            ..unlocked_at_start()
        };
        let saved = Mutex::new(None);
        r.machine
            .set_settings(never.clone(), |s| {
                *saved.lock().unwrap() = Some(s.clone());
                Ok(())
            })
            .unwrap();
        assert_eq!(saved.into_inner().unwrap(), Some(never));
        assert_eq!(r.machine.state().auto_lock_minutes, None);
    }

    #[test]
    fn a_slow_save_holds_up_no_command() {
        let r = rig(unlocked_at_start(), fake());
        let (saving_tx, saving) = mpsc::channel();
        let (saved, saved_rx) = mpsc::channel::<()>();
        let five = Settings {
            auto_lock: AutoLock::Min5,
            ..unlocked_at_start()
        };
        let setting = {
            let machine = r.machine.clone();
            thread::spawn(move || {
                machine.set_settings(five, move |_| {
                    let _ = saving_tx.send(());
                    // Until the test lets the disk finish, or fails and drops the sender.
                    let _ = saved_rx.recv();
                    Ok(())
                })
            })
        };
        saving.recv_timeout(LONG).expect("the save started");
        let (guard, state) = {
            let machine = r.machine.clone();
            within("commands during a save", move || {
                let guard = machine.guard("op_list");
                machine.activity();
                machine.tick();
                machine.lock(LockReason::Manual);
                (guard, machine.state())
            })
        };
        assert!(guard.is_ok(), "{guard:?}");
        assert!(state.locked, "a lock does not wait for the disk");
        assert_eq!(
            state.auto_lock_minutes,
            Some(10),
            "a change applies once it is saved"
        );
        saved.send(()).unwrap();
        setting.join().unwrap().unwrap();
        assert_eq!(r.machine.state().auto_lock_minutes, Some(5));
    }

    #[test]
    fn a_second_save_waits_until_the_first_is_saved_and_applied() {
        let r = rig(unlocked_at_start(), fake());
        let (saving_tx, saving) = mpsc::channel();
        let (saved, saved_rx) = mpsc::channel::<()>();
        let five = Settings {
            auto_lock: AutoLock::Min5,
            ..unlocked_at_start()
        };
        let never = Settings {
            auto_lock: AutoLock::Never,
            ..unlocked_at_start()
        };
        let first = {
            let machine = r.machine.clone();
            thread::spawn(move || {
                machine.set_settings(five, move |_| {
                    let _ = saving_tx.send(());
                    let _ = saved_rx.recv();
                    Ok(())
                })
            })
        };
        saving.recv_timeout(LONG).expect("the first save started");
        let (seen_tx, seen) = mpsc::channel();
        let second = {
            let machine = r.machine.clone();
            thread::spawn(move || {
                let reader = machine.clone();
                machine.set_settings(never, move |_| {
                    let _ = seen_tx.send(reader.state().auto_lock_minutes);
                    Ok(())
                })
            })
        };
        // The first is on the disk: the second must not reach its own save meanwhile, or
        // the two could reach the file and the machine in different orders.
        assert!(
            matches!(
                seen.recv_timeout(Duration::from_millis(20)),
                Err(RecvTimeoutError::Timeout)
            ),
            "the second save started while the first was on the disk"
        );
        saved.send(()).unwrap();
        assert_eq!(
            seen.recv_timeout(LONG).expect("the second save ran"),
            Some(5),
            "the first change was applied before the second was saved"
        );
        first.join().unwrap().unwrap();
        second.join().unwrap().unwrap();
        assert_eq!(
            r.machine.state().auto_lock_minutes,
            None,
            "the last one wins"
        );
    }

    #[test]
    fn switching_the_lock_off_never_unlocks() {
        let r = rig(Settings::default(), fake());
        let off = Settings {
            lock_enabled: false,
            ..Settings::default()
        };
        r.clock.set(T0 + MIN);
        r.machine.set_settings(off, |_| Ok(())).unwrap();
        let still = locked(LockReason::Startup, T0, None, 1);
        // Told by the save itself, before anything reads the state.
        assert_eq!(
            r.hooked.calls(),
            std::slice::from_ref(&still),
            "reported, as an update"
        );
        assert_eq!(r.hooked.changes(), [LockChange::Update]);
        assert_eq!(
            r.machine.state(),
            still,
            "locked since the start, no idle time now"
        );
    }

    // M-8. Whatever changes what the state reports is numbered, so the page's order holds.

    #[test]
    fn a_new_idle_time_is_reported_as_a_numbered_update() {
        let r = rig(unlocked_at_start(), fake());
        r.machine.lock(LockReason::Manual);
        r.clock.set(T0 + MIN);
        let five = Settings {
            auto_lock: AutoLock::Min5,
            ..unlocked_at_start()
        };
        r.machine.set_settings(five, |_| Ok(())).unwrap();
        let updated = locked(LockReason::Manual, T0, Some(5), 2);
        // Told by the save itself, before anything reads the state.
        assert_eq!(
            r.hooked.changes(),
            [LockChange::Transition, LockChange::Update]
        );
        assert_eq!(r.hooked.calls()[1], updated);
        assert_eq!(
            r.machine.state(),
            updated,
            "the same lock, since its own time"
        );
        let never = Settings {
            auto_lock: AutoLock::Never,
            ..unlocked_at_start()
        };
        r.machine.set_settings(never, |_| Ok(())).unwrap();
        assert_eq!(r.machine.state(), locked(LockReason::Manual, T0, None, 3));
    }

    #[test]
    fn a_save_that_changes_nothing_the_state_reports_is_not_numbered() {
        let r = rig(unlocked_at_start(), fake());
        for settings in [
            Settings {
                theme: Theme::Light,
                ..unlocked_at_start()
            },
            Settings {
                lock_on_sleep: false,
                ..unlocked_at_start()
            },
            unlocked_at_start(),
        ] {
            r.machine.set_settings(settings, |_| Ok(())).unwrap();
        }
        assert_eq!(r.machine.state(), unlocked(T0, Some(10), 0));
        assert!(r.hooked.calls().is_empty());
    }

    /// The tie the page resolves by taking the state that arrives last: two states with one
    /// number are the same state, idle time included.
    #[test]
    fn two_states_with_one_number_are_the_same_state() {
        let r = rig(unlocked_at_start(), fake());
        let mut seen = vec![r.machine.state()];
        let settings = [
            AutoLock::Min5,
            AutoLock::Min30,
            AutoLock::Never,
            AutoLock::Min10,
        ];
        for (i, auto_lock) in settings.into_iter().enumerate() {
            let s = Settings {
                auto_lock,
                ..unlocked_at_start()
            };
            r.machine.set_settings(s, |_| Ok(())).unwrap();
            seen.push(r.machine.state());
            if i == 1 {
                r.machine.lock(LockReason::Manual);
                seen.push(r.machine.state());
                r.machine.unlock().unwrap();
                seen.push(r.machine.state());
            }
        }
        seen.extend(r.hooked.calls());
        for a in &seen {
            for b in &seen {
                if a.seq == b.seq {
                    assert_eq!(a, b, "one number, two states");
                }
            }
        }
        assert_eq!(r.machine.state().seq, 6);
    }

    // D.4a T74, T75, O4, O7: the lock over a protected store.

    /// The machine over `auth` on a protected store, once the authenticator has answered.
    fn rig_protected(settings: Settings, auth: Arc<dyn Authenticator>) -> Rig {
        let clock = Arc::new(ManualClock::at(T0));
        let hooked = Arc::new(Hooked::default());
        let cache = AuthCache::new(auth);
        let machine = {
            let hooked = hooked.clone();
            LockMachine::new_protected(
                settings,
                cache.clone(),
                clock.clone(),
                Box::new(move |state, change| {
                    hooked.0.lock().unwrap().push((state.clone(), change))
                }),
                true,
            )
        };
        assert!(cache.settled(LONG), "the authenticator never answered");
        Rig {
            clock,
            hooked,
            machine,
            cache,
        }
    }

    /// The person's own settings with every lock setting off.
    fn all_off() -> Settings {
        Settings {
            lock_enabled: false,
            lock_on_start: false,
            lock_on_sleep: false,
            auto_lock: AutoLock::Never,
            ..Settings::default()
        }
    }

    fn opens(machine: &LockMachine) {
        machine
            .unlock_with_store_secret(|_| Ok(()), |()| {})
            .unwrap();
    }

    #[test]
    fn pinned_follows_protection() {
        let own = all_off();
        assert_eq!(pinned(&own, false), own);
        let p = pinned(&own, true);
        assert!(p.lock_enabled && p.lock_on_start && p.lock_on_sleep);
        assert_eq!(p.auto_lock, AutoLock::Min30);
        assert_eq!(p.theme, own.theme);
        let five = Settings {
            auto_lock: AutoLock::Min5,
            ..own
        };
        assert_eq!(pinned(&five, true).auto_lock, AutoLock::Min5);
    }

    #[test]
    fn a_protected_store_starts_locked_whatever_the_settings_or_the_os() {
        for auth in [Arc::new(NoAuthenticator) as Arc<dyn Authenticator>, fake()] {
            let r = rig_protected(all_off(), auth);
            let state = r.machine.state();
            assert!(
                state.locked && state.reason == Some(LockReason::Startup),
                "{state:?}"
            );
            assert!(r.machine.protected() && r.machine.is_locked());
            // A saved Never reads as 30 minutes while protected (O7).
            assert_eq!(state.auto_lock_minutes, Some(30));
        }
    }

    #[test]
    fn while_protected_the_idle_and_the_sleep_locks_apply() {
        let r = rig_protected(all_off(), Arc::new(NoAuthenticator));
        opens(&r.machine);
        assert!(!r.machine.is_locked());
        r.clock.advance(30 * MIN);
        r.machine.tick();
        assert_eq!(r.machine.state().reason, Some(LockReason::Idle));
        opens(&r.machine);
        r.machine.os_session();
        assert_eq!(r.machine.state().reason, Some(LockReason::OsSession));
    }

    #[test]
    fn while_protected_a_save_never_turns_a_pin_off() {
        let r = rig_protected(Settings::default(), fake());
        let persisted = Arc::new(AtomicUsize::new(0));
        let persist = |_: &Settings| {
            persisted.fetch_add(1, SeqCst);
            Ok(())
        };
        for off in [
            Settings {
                lock_enabled: false,
                ..Settings::default()
            },
            Settings {
                lock_on_start: false,
                ..Settings::default()
            },
            Settings {
                lock_on_sleep: false,
                ..Settings::default()
            },
            Settings {
                auto_lock: AutoLock::Never,
                ..Settings::default()
            },
        ] {
            let e = r.machine.set_settings(off.clone(), persist).unwrap_err();
            assert!(matches!(e, DesktopError::LockRequired), "{off:?}: {e:?}");
        }
        assert_eq!(
            persisted.load(SeqCst),
            0,
            "nothing refused reaches the file"
        );
        r.machine
            .set_settings(
                Settings {
                    theme: Theme::Light,
                    ..Settings::default()
                },
                persist,
            )
            .unwrap();
        assert_eq!(persisted.load(SeqCst), 1);
    }

    /// What the person saved off before protecting stays saved — protection off brings it
    /// back — while the pin holds: Lock now still locks.
    #[test]
    fn a_value_saved_off_stays_saved_and_the_pin_holds() {
        let r = rig_protected(all_off(), fake());
        let saved = Mutex::new(None);
        r.machine
            .set_settings(
                Settings {
                    theme: Theme::Light,
                    ..all_off()
                },
                |s| {
                    *saved.lock().unwrap() = Some(s.clone());
                    Ok(())
                },
            )
            .unwrap();
        assert!(!saved.lock().unwrap().as_ref().unwrap().lock_enabled);
        opens(&r.machine);
        r.machine.lock(LockReason::Manual);
        assert!(r.machine.is_locked());
    }

    #[test]
    fn while_protected_the_os_unlocks_nothing_and_is_not_asked() {
        let (auth, calls) = os(os_available(), AuthOutcome::Verified);
        let r = rig_protected(unlocked_at_start(), auth);
        let e = r.machine.unlock().unwrap_err();
        assert!(
            matches!(
                e,
                DesktopError::AuthUnavailable {
                    reason: UnavailableReason::UsePasswordField
                }
            ),
            "{e:?}"
        );
        let e = r
            .machine
            .unlock_with_password(typed("hunter2"))
            .unwrap_err();
        assert!(
            matches!(
                *e.error,
                DesktopError::AuthUnavailable {
                    reason: UnavailableReason::UsePasswordField
                }
            ),
            "{e:?}"
        );
        assert!(r.machine.is_locked());
        assert!(
            calls
                .lock()
                .unwrap()
                .iter()
                .all(|c| !matches!(c, Call::Verify(_) | Call::Password(..))),
            "the OS was asked"
        );
    }

    /// The key is kept under the machine's lock before the transition: whoever hears of the
    /// unlock finds it held.
    #[test]
    fn the_store_secret_s_key_is_kept_before_anyone_hears_of_the_unlock() {
        let clock = Arc::new(ManualClock::at(T0));
        let cache = AuthCache::new(Arc::new(NoAuthenticator));
        let kept = Arc::new(AtomicBool::new(false));
        let heard = Arc::new(Mutex::new(Vec::new()));
        let machine = {
            let (kept, heard) = (kept.clone(), heard.clone());
            LockMachine::new_protected(
                unlocked_at_start(),
                cache.clone(),
                clock,
                Box::new(move |state, _| {
                    heard
                        .lock()
                        .unwrap()
                        .push((state.locked, kept.load(SeqCst)))
                }),
                true,
            )
        };
        assert!(cache.settled(LONG));
        machine
            .unlock_with_store_secret(
                |_| Ok(7),
                |key: i32| {
                    assert_eq!(key, 7);
                    kept.store(true, SeqCst);
                },
            )
            .unwrap();
        assert_eq!(*heard.lock().unwrap(), vec![(false, true)]);
    }

    #[test]
    fn a_refused_store_secret_keeps_it_locked_and_keeps_nothing() {
        let r = rig_protected(unlocked_at_start(), Arc::new(NoAuthenticator));
        let kept = AtomicBool::new(false);
        let e = r
            .machine
            .unlock_with_store_secret(
                |_| {
                    Err::<(), _>(Refusal::new(
                        DesktopError::AuthFailed {
                            exhausted: false,
                            retry_in_ms: None,
                        },
                        vec!["said".into()],
                    ))
                },
                |()| kept.store(true, SeqCst),
            )
            .unwrap_err();
        assert!(matches!(*e.error, DesktopError::AuthFailed { .. }), "{e:?}");
        assert_eq!(e.messages, ["said"]);
        assert!(r.machine.is_locked() && !kept.load(SeqCst));
    }

    /// A session event while the secret is checked closes the check: its yes does not count,
    /// and nothing is kept.
    #[test]
    fn a_lock_during_the_store_secret_check_refuses_its_yes() {
        let r = rig_protected(unlocked_at_start(), Arc::new(NoAuthenticator));
        let machine = r.machine.clone();
        let kept = AtomicBool::new(false);
        let e = r
            .machine
            .unlock_with_store_secret(
                |_| {
                    machine.os_session();
                    Ok(())
                },
                |()| kept.store(true, SeqCst),
            )
            .unwrap_err();
        assert!(matches!(*e.error, DesktopError::AuthCancelled), "{e:?}");
        assert!(r.machine.is_locked() && !kept.load(SeqCst));
    }

    #[test]
    fn protection_ending_while_locked_unlocks_only_where_the_lock_is_not_in_effect() {
        // The lock off in the saved settings: nothing could open the app once it is gone.
        let r = rig_protected(all_off(), fake());
        r.machine.set_protected(false);
        assert!(!r.machine.is_locked() && !r.machine.protected());
        // The lock on, and an OS that can verify: it stays locked, for the OS to open.
        let r = rig_protected(Settings::default(), fake());
        r.machine.set_protected(false);
        assert!(r.machine.is_locked());
    }

    #[test]
    fn protection_on_pins_without_locking_and_lock_unlocked_leaves_a_locked_app() {
        let r = rig(
            Settings {
                lock_enabled: false,
                ..unlocked_at_start()
            },
            fake(),
        );
        assert!(!r.machine.is_locked());
        assert_eq!(r.machine.state().auto_lock_minutes, None);
        r.machine.set_protected(true);
        assert!(
            !r.machine.is_locked(),
            "the shell decides, by the key it holds"
        );
        assert_eq!(r.machine.state().auto_lock_minutes, Some(10));
        r.machine.lock_unlocked(LockReason::SecretsLocked);
        assert_eq!(r.machine.state().reason, Some(LockReason::SecretsLocked));
        r.machine.lock_unlocked(LockReason::Idle);
        assert_eq!(
            r.machine.state().reason,
            Some(LockReason::SecretsLocked),
            "a locked app is left as it is"
        );
    }

    /// O4, as the holder has it: a sleep the OS did not report — the clock that counts suspend
    /// more than 5 s ahead of the one that does not, since the last look — locks a protected
    /// app as a reported sleep does (the lock hook then drops the key). Exactly 5 s is no sleep.
    #[test]
    fn while_protected_an_unreported_sleep_locks_on_the_next_tick() {
        let r = rig_protected(all_off(), Arc::new(NoAuthenticator));
        opens(&r.machine);
        r.machine.tick();
        r.clock.sleep(5_000);
        r.machine.tick();
        assert!(!r.machine.is_locked(), "{:?}", r.machine.state());
        r.clock.sleep(5_001);
        r.machine.tick();
        assert_eq!(r.machine.state().reason, Some(LockReason::OsSession));
    }

    /// Between ticks, a secret's use looks first (`check_sleep`, the vault access's hook): the
    /// first read after the wake already finds the app locked.
    #[test]
    fn a_look_between_ticks_finds_the_sleep_too() {
        let r = rig_protected(all_off(), Arc::new(NoAuthenticator));
        opens(&r.machine);
        assert!(!r.machine.check_sleep());
        r.clock.sleep(60_000);
        assert!(r.machine.check_sleep());
        assert_eq!(r.machine.state().reason, Some(LockReason::OsSession));
    }

    /// A wall clock that steps (a time sync, the date changed) is no sleep: O4 never reads it
    /// (the Rust verifier's finding 4).
    #[test]
    fn a_wall_clock_step_is_no_sleep() {
        let r = rig_protected(all_off(), Arc::new(NoAuthenticator));
        opens(&r.machine);
        r.machine.tick();
        r.clock.set_wall(T0 + 10 * MIN);
        r.machine.tick();
        assert!(!r.machine.is_locked(), "{:?}", r.machine.state());
    }

    /// A store in files keeps D.2's behaviour: no lock on a gap the OS did not report.
    #[test]
    fn an_unreported_sleep_locks_only_a_protected_app() {
        let r = rig(unlocked_at_start(), fake());
        r.machine.tick();
        r.clock.sleep(MIN);
        r.machine.tick();
        assert!(!r.machine.check_sleep());
        assert!(!r.machine.is_locked(), "{:?}", r.machine.state());
    }

    /// A sleep while the app is locked is not the next unlock's to end.
    #[test]
    fn a_sleep_while_locked_does_not_lock_the_unlock_that_follows() {
        let r = rig_protected(all_off(), Arc::new(NoAuthenticator));
        r.machine.tick();
        r.clock.sleep(10 * MIN);
        opens(&r.machine);
        r.machine.tick();
        assert!(!r.machine.is_locked(), "{:?}", r.machine.state());
    }

    /// The final verifier's finding 9: a sleep the OS does not report, while the store's
    /// secret is being checked (the lid closed on a submitted password), voids the check —
    /// whether the check's own look finds it or a tick took the look first.
    #[test]
    fn a_sleep_during_the_check_voids_the_unlock() {
        for tick_first in [false, true] {
            let r = rig_protected(all_off(), Arc::new(NoAuthenticator));
            let kept = std::cell::Cell::new(false);
            let e = r
                .machine
                .unlock_with_store_secret(
                    |_| {
                        r.clock.sleep(6_000);
                        if tick_first {
                            r.machine.tick();
                        }
                        Ok(())
                    },
                    |()| kept.set(true),
                )
                .unwrap_err();
            assert!(
                matches!(*e.error, DesktopError::AuthCancelled),
                "{tick_first}: {e:?}"
            );
            assert!(!kept.get() && r.machine.is_locked(), "{tick_first}");
            // The next check, with no sleep in it, opens.
            opens(&r.machine);
            assert!(!r.machine.is_locked(), "{tick_first}");
        }
    }

    /// The final verifier's finding 8: the shell's re-read decides under the machine's lock —
    /// `keep` yes leaves the app unlocked; no locks it; a locked app is left locked.
    #[test]
    fn lock_unlocked_unless_asks_keep_under_the_machine_s_lock() {
        let r = rig_protected(all_off(), Arc::new(NoAuthenticator));
        opens(&r.machine);
        let asked = std::cell::Cell::new(0);
        r.machine
            .lock_unlocked_unless(LockReason::SecretsLocked, || {
                asked.set(asked.get() + 1);
                // The machine's lock is held while `keep` runs: nothing else can take it now.
                assert!(r.machine.inner.try_lock().is_err());
                true
            });
        assert!(!r.machine.is_locked());
        r.machine
            .lock_unlocked_unless(LockReason::SecretsLocked, || false);
        assert_eq!(r.machine.state().reason, Some(LockReason::SecretsLocked));
        r.machine
            .lock_unlocked_unless(LockReason::SecretsLocked, || {
                asked.set(asked.get() + 1);
                true
            });
        assert!(r.machine.is_locked() && asked.get() == 2);
    }

    /// Correction (run 2): a late answer that nothing can verify the owner lifts a provisional
    /// start's lock (rule 3) — but not once the store is protected, where the vault's own field
    /// opens the app and the OS decides nothing (T75). The protection came before the answer,
    /// and changed nothing the page hears (the pinned idle time is the one in use), so only the
    /// protection keeps the lock.
    #[test]
    fn a_late_unavailable_lifts_nothing_once_the_store_is_protected() {
        let (os, answer) = Gated::new();
        let r = started(Settings::default(), os.clone());
        r.machine.set_protected(true);
        assert_eq!(r.machine.state().seq, 0, "nothing the page hears moved");
        answer.send(NoAuthenticator.info()).unwrap();
        assert!(r.cache.settled(LONG));
        let state = r.machine.state();
        assert!(
            state.locked && state.reason == Some(LockReason::Startup),
            "{state:?}"
        );
        assert!(
            r.hooked.calls().iter().all(|s| s.locked),
            "{:?}",
            r.hooked.calls()
        );
    }
}
