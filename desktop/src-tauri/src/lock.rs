// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The app lock, decided in Rust (design spec §4.3): the webview only renders
//! [`LockState`], and every command passes [`LockMachine::guard`] first.
//!
//! The lock is in effect when the settings switch it on **and** the [`Authenticator`] can
//! verify the owner. Without an authenticator it fails closed in the only safe direction: a
//! lock nobody could open is no lock, so it is off (the webview shows a persistent banner from
//! `AuthInfo`), switching it on is refused with `AuthUnavailable`, and destructive operations
//! are refused by the [`OperationManager`](crate::ops::OperationManager) for the same reason.
//!
//! Settings reach the machine as a copy: [`LockMachine::new`] takes the loaded settings and
//! only [`LockMachine::set_settings`] replaces them. It checks the change, then calls the
//! caller's `persist` (in the app, `|s| store.set(s.clone())`) and applies the change only if
//! that succeeded, so the file and the machine change together or not at all. The machine
//! knows nothing of the [`SettingsStore`](crate::settings::SettingsStore) and the store nothing
//! of the machine.
//!
//! Every transition between locked and unlocked calls the hook exactly once, with the new
//! state, under the machine's lock (so hooks run in transition order). The state at
//! construction is not a transition: nothing is listening yet. The idle time is measured with
//! [`elapsed_ms`](crate::ops::elapsed_ms): a suspend counts, a wall clock stepped back does not.
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
//! listener, from the hook itself, or from `persist`.

use std::panic::{self, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use apprafter_core::CancellationToken;
use apprafter_desktop_ipc::{
    AuthInfo, AuthOutcome, LockReason, LockState, Settings, UnavailableReason, ALLOWED_WHILE_LOCKED,
};

use crate::auth::{AuthPurpose, Authenticator};
use crate::errors::DesktopError;
use crate::ops::{panic_message, Clock, Stamp};

const MINUTE_MS: u64 = 60_000;

/// Called on every transition between locked and unlocked, with the new state; the app drops
/// pending plans, ends every operation subscription and emits `lock-changed`. It runs under
/// the machine's lock, so it must be quick and must never call back into the machine (see the
/// module docs for the lock order).
/// A panic in it is caught and logged: the transition stands, and whoever made it — the idle
/// ticker among them — goes on.
pub type LockHook = Box<dyn Fn(&LockState) + Send + Sync>;

pub struct LockMachine {
    auth: Arc<dyn Authenticator>,
    clock: Arc<dyn Clock>,
    hook: LockHook,
    inner: Mutex<Inner>,
    /// `inner.reason.is_some()`, for [`guard`](Self::guard). Every transition writes it under
    /// `inner`, before the hook hears of it, so it is never ahead of the state.
    locked: AtomicBool,
    /// Held by [`set_settings`](Self::set_settings) across `persist` and the apply.
    settings_write: Mutex<()>,
}

struct Inner {
    settings: Settings,
    /// `Some` while locked.
    reason: Option<LockReason>,
    /// When the current state began, on the wall clock: what the webview shows.
    since_ms: u64,
    /// The last activity (or unlock): the idle time runs from it.
    last_activity: Stamp,
    /// The unlock prompt while it is open; a second unlock is `AuthBusy` meanwhile.
    prompt: Option<Prompt>,
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
    /// set; unlocked otherwise.
    pub fn new(
        settings: Settings,
        auth: Arc<dyn Authenticator>,
        clock: Arc<dyn Clock>,
        hook: LockHook,
    ) -> Self {
        let now = Stamp::now(&*clock);
        let reason = (in_effect(&settings, &auth.info()) && settings.lock_on_start)
            .then_some(LockReason::Startup);
        Self {
            auth,
            clock,
            hook,
            inner: Mutex::new(Inner {
                settings,
                reason,
                since_ms: now.wall_ms,
                last_activity: now,
                prompt: None,
            }),
            locked: AtomicBool::new(reason.is_some()),
            settings_write: Mutex::new(()),
        }
    }

    pub fn state(&self) -> LockState {
        let info = self.auth.info();
        state_of(&self.lock_inner(), &info)
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
    pub fn tick(&self) {
        let info = self.auth.info();
        let mut inner = self.lock_inner();
        if inner.reason.is_some() || !in_effect(&inner.settings, &info) {
            return;
        }
        let Some(minutes) = inner.settings.auto_lock.minutes() else {
            return;
        };
        if inner.last_activity.elapsed_ms(&*self.clock) >= u64::from(minutes) * MINUTE_MS {
            let now = self.clock.now_ms();
            self.enter(&mut inner, &info, Some(LockReason::Idle), now);
        }
    }

    /// Lock for `reason`. Already locked, it keeps the reason and time it locked with, and
    /// closes an open unlock prompt (the session locked or slept while it was open). With
    /// the lock not in effect it does nothing: there would be no way to unlock.
    pub fn lock(&self, reason: LockReason) {
        let info = self.auth.info();
        let now = self.clock.now_ms();
        let close = {
            let mut inner = self.lock_inner();
            if inner.reason.is_some() {
                close_open_prompt(&mut inner)
            } else {
                if in_effect(&inner.settings, &info) {
                    self.enter(&mut inner, &info, Some(reason), now);
                }
                None
            }
        };
        trip_prompt(close);
    }

    /// Quit: close an open unlock prompt, as the [`Authenticator`] promises the app does, so
    /// that a `Verified` arriving after this returns — even before the dialog has closed —
    /// does not unlock (its `unlock` answers `AuthCancelled`). Nothing else changes; with no
    /// prompt open it does nothing.
    pub fn close_prompt(&self) {
        let close = close_open_prompt(&mut self.lock_inner());
        trip_prompt(close);
    }

    /// Ask the owner, and unlock if the OS verifies them. Blocks until the prompt answers
    /// (it can take minutes), without holding the machine's lock: meanwhile every other
    /// call answers at once, and a second `unlock` is `AuthBusy`. Unlocked already, it asks
    /// nothing.
    pub fn unlock(&self) -> Result<(), DesktopError> {
        let cancel = {
            let mut inner = self.lock_inner();
            if inner.reason.is_none() {
                return Ok(());
            }
            if inner.prompt.is_some() {
                return Err(DesktopError::AuthBusy);
            }
            let cancel = CancellationToken::new();
            inner.prompt = Some(Prompt {
                cancel: cancel.clone(),
                closed: false,
            });
            cancel
        };
        let outcome = panic::catch_unwind(AssertUnwindSafe(|| {
            self.auth.verify(&AuthPurpose::Unlock, &cancel)
        }));
        let info = self.auth.info();
        let mut inner = self.lock_inner();
        let closed = inner.prompt.take().is_none_or(|prompt| prompt.closed);
        let outcome = match outcome {
            Ok(outcome) => outcome,
            // The prompt is closed above, so the next unlock can ask again.
            Err(payload) => {
                drop(inner);
                panic::resume_unwind(payload)
            }
        };
        match outcome {
            AuthOutcome::Verified if closed => Err(DesktopError::AuthCancelled),
            AuthOutcome::Verified => {
                let now = Stamp::now(&*self.clock);
                inner.last_activity = now;
                self.enter(&mut inner, &info, None, now.wall_ms);
                Ok(())
            }
            AuthOutcome::Cancelled { .. } => Err(DesktopError::AuthCancelled),
            AuthOutcome::Failed { exhausted } => Err(DesktopError::AuthFailed { exhausted }),
            AuthOutcome::Unavailable { reason } => Err(DesktopError::AuthUnavailable { reason }),
            AuthOutcome::Busy => Err(DesktopError::AuthBusy),
        }
    }

    /// Replace the settings the machine follows, once `persist` has saved them.
    ///
    /// Switching the lock on is refused with `AuthUnavailable` when nothing can verify the
    /// owner; keeping it on is not (the defaults have it on, and every other setting must
    /// stay changeable). Switching it off never unlocks. A new idle time applies from the
    /// next tick.
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
        let switching_on = settings.lock_enabled && !self.lock_inner().settings.lock_enabled;
        if switching_on && !info.available {
            return Err(DesktopError::AuthUnavailable {
                reason: info.unavailable.unwrap_or(UnavailableReason::NoBackend),
            });
        }
        persist(&settings)?;
        self.lock_inner().settings = settings;
        Ok(())
    }

    /// Move to `reason` (`None` = unlocked) and tell the hook; a panic in the hook is logged,
    /// and the transition stands.
    fn enter(&self, inner: &mut Inner, info: &AuthInfo, reason: Option<LockReason>, now: u64) {
        inner.reason = reason;
        inner.since_ms = now;
        self.locked.store(reason.is_some(), Ordering::SeqCst);
        let state = state_of(inner, info);
        if let Err(payload) = panic::catch_unwind(AssertUnwindSafe(|| (self.hook)(&state))) {
            tracing::error!(
                locked = state.locked,
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

fn in_effect(settings: &Settings, info: &AuthInfo) -> bool {
    settings.lock_enabled && info.available
}

fn state_of(inner: &Inner, info: &AuthInfo) -> LockState {
    LockState {
        locked: inner.reason.is_some(),
        reason: inner.reason,
        since_ms: inner.since_ms,
        auto_lock_minutes: in_effect(&inner.settings, info)
            .then(|| inner.settings.auto_lock.minutes())
            .flatten(),
    }
}

#[cfg(test)]
mod tests {
    use std::panic::{self, AssertUnwindSafe};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::SeqCst};
    use std::sync::mpsc::{self, RecvTimeoutError};
    use std::sync::{Arc, Mutex};
    use std::thread;
    use std::time::Duration;

    use apprafter_core::CancellationToken;
    use apprafter_desktop_ipc::{
        AuthInfo, AuthOutcome, AutoLock, CancelledBy, LockReason, LockState, Settings, Theme,
        UnavailableReason, ALLOWED_WHILE_LOCKED, COMMANDS,
    };

    use super::LockMachine;
    use crate::auth::{AuthPurpose, Authenticator, FakeAuthenticator, NoAuthenticator};
    use crate::errors::DesktopError;
    use crate::ops::test_clock::ManualClock;
    use crate::ops::test_trips;

    const T0: u64 = 1_700_000_000_000;
    const MIN: u64 = 60_000;
    const DAY: u64 = 24 * 60 * MIN;
    /// What a test waits before it calls something stuck.
    const LONG: Duration = Duration::from_secs(10);

    /// Every state the hook was called with.
    #[derive(Default)]
    struct Hooked(Mutex<Vec<LockState>>);

    impl Hooked {
        fn calls(&self) -> Vec<LockState> {
            self.0.lock().unwrap().clone()
        }
    }

    struct Rig {
        clock: Arc<ManualClock>,
        hooked: Arc<Hooked>,
        machine: Arc<LockMachine>,
    }

    fn rig(settings: Settings, auth: Arc<dyn Authenticator>) -> Rig {
        let clock = Arc::new(ManualClock::at(T0));
        let hooked = Arc::new(Hooked::default());
        let machine = {
            let hooked = hooked.clone();
            Arc::new(LockMachine::new(
                settings,
                auth,
                clock.clone(),
                Box::new(move |state| hooked.0.lock().unwrap().push(state.clone())),
            ))
        };
        Rig {
            clock,
            hooked,
            machine,
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

    fn locked(reason: LockReason, since_ms: u64, auto_lock_minutes: Option<u32>) -> LockState {
        LockState {
            locked: true,
            reason: Some(reason),
            since_ms,
            auto_lock_minutes,
        }
    }

    fn unlocked(since_ms: u64, auto_lock_minutes: Option<u32>) -> LockState {
        LockState {
            locked: false,
            reason: None,
            since_ms,
            auto_lock_minutes,
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

    // 1–3. Whether the lock is in effect, and the state at start.

    #[test]
    fn with_an_authenticator_the_default_settings_start_locked() {
        let r = rig(Settings::default(), fake());
        assert_eq!(r.machine.state(), locked(LockReason::Startup, T0, Some(10)));
        assert!(
            r.hooked.calls().is_empty(),
            "the state at start is not a transition"
        );
    }

    #[test]
    fn without_lock_on_start_or_with_the_lock_off_it_starts_unlocked() {
        let r = rig(unlocked_at_start(), fake());
        assert_eq!(r.machine.state(), unlocked(T0, Some(10)));
        let off = Settings {
            lock_enabled: false,
            ..Settings::default()
        };
        let r = rig(off, fake());
        assert_eq!(
            r.machine.state(),
            unlocked(T0, None),
            "no idle time when off"
        );
    }

    #[test]
    fn without_an_authenticator_the_lock_is_off_even_when_the_settings_say_on() {
        let r = rig(Settings::default(), Arc::new(NoAuthenticator));
        assert_eq!(r.machine.state(), unlocked(T0, None));
        // Nothing locks it: there would be no way back.
        r.machine.lock(LockReason::Manual);
        r.clock.set(T0 + 1000 * MIN);
        r.machine.tick();
        assert_eq!(r.machine.state(), unlocked(T0, None));
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
        assert_eq!(r.machine.state(), unlocked(T0, None));
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
        let idle = locked(LockReason::Idle, T0 + 10 * MIN, Some(10));
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
            locked(LockReason::Idle, T0 + 19 * MIN, Some(10))
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
            locked(LockReason::Idle, T0 - DAY + 5 * MIN, Some(10)),
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
            locked(LockReason::Idle, T0 + 11 * MIN, Some(10))
        );
    }

    #[test]
    fn an_unlock_counts_as_activity() {
        let r = rig(Settings::default(), fake());
        r.clock.set(T0 + 30 * MIN);
        r.machine.unlock().unwrap();
        r.clock.set(T0 + 30 * MIN + 1);
        r.machine.tick();
        assert_eq!(r.machine.state(), unlocked(T0 + 30 * MIN, Some(10)));
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
            locked(LockReason::Manual, T0 + MIN, Some(10))
        );
        r.clock.set(T0 + 2 * MIN);
        r.machine.unlock().unwrap();
        assert_eq!(r.machine.state(), unlocked(T0 + 2 * MIN, Some(10)));
        assert_eq!(auth.asked(), vec![AuthPurpose::Unlock]);
    }

    #[test]
    fn locking_a_locked_app_keeps_why_and_since() {
        let r = rig(Settings::default(), fake());
        r.clock.set(T0 + MIN);
        r.machine.lock(LockReason::Manual);
        r.machine.lock(LockReason::OsSession);
        assert_eq!(r.machine.state(), locked(LockReason::Startup, T0, Some(10)));
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
                AuthOutcome::Failed { exhausted: true },
                DesktopError::AuthFailed { exhausted: true },
            ),
            (
                AuthOutcome::Failed { exhausted: false },
                DesktopError::AuthFailed { exhausted: false },
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
                locked(LockReason::Startup, T0, Some(10)),
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
        assert_eq!(r.machine.state(), locked(LockReason::Startup, T0, Some(10)));
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
        assert_eq!(r.machine.state(), locked(LockReason::Startup, T0, Some(10)));
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
        r.machine.close_prompt();
        ends.tripped
            .recv_timeout(LONG)
            .expect("the quit closed the prompt");
        ends.answer.send(AuthOutcome::Verified).unwrap();
        let result = first.recv_timeout(LONG).expect("the unlock returned");
        assert!(
            matches!(result, Err(DesktopError::AuthCancelled)),
            "{result:?}"
        );
        assert_eq!(r.machine.state(), locked(LockReason::Startup, T0, Some(10)));
        assert!(r.hooked.calls().is_empty());
    }

    #[test]
    fn a_yes_that_beats_the_quits_closing_thread_does_not_unlock_either() {
        let (prompt, ends) = HeldPrompt::new();
        let r = rig(Settings::default(), prompt);
        let first = unlock_in_background(&r.machine);
        ends.opened.recv_timeout(LONG).expect("the prompt opened");
        // As with a lock: the token trips on a thread of its own, held back here so the OS
        // answers yes after `close_prompt` returned and before the dialog closed.
        let ((), held) = {
            let machine = r.machine.clone();
            within("close_prompt", move || {
                test_trips::held(|| machine.close_prompt())
            })
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
        assert_eq!(r.machine.state(), locked(LockReason::Startup, T0, Some(10)));
        assert!(r.hooked.calls().is_empty());
        for token in held {
            token.cancel();
        }
    }

    #[test]
    fn closing_the_prompt_with_none_open_changes_nothing() {
        for settings in [Settings::default(), unlocked_at_start()] {
            let r = rig(settings, fake());
            let before = r.machine.state();
            let ((), held) = test_trips::held(|| r.machine.close_prompt());
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
            r.machine.close_prompt();
            r.machine.close_prompt();
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
                locked(LockReason::Manual, T0 + MIN, Some(10)),
                unlocked(T0 + 2 * MIN, Some(10)),
                locked(LockReason::Idle, T0 + 12 * MIN, Some(10)),
                unlocked(T0 + 14 * MIN, Some(10)),
            ]
        );
    }

    #[test]
    fn a_panicking_hook_neither_undoes_the_transition_nor_stops_later_ticks() {
        let clock = Arc::new(ManualClock::at(T0));
        let calls = Arc::new(AtomicUsize::new(0));
        let machine = {
            let calls = calls.clone();
            LockMachine::new(
                unlocked_at_start(),
                fake(),
                clock.clone(),
                Box::new(move |_| {
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
            locked(LockReason::Idle, T0 + 10 * MIN, Some(10))
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
            locked(LockReason::Idle, T0 + 21 * MIN, Some(10))
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
        let machine = Arc::new(LockMachine::new(
            unlocked_at_start(),
            fake(),
            Arc::new(ManualClock::at(T0)),
            Box::new(move |_| {
                let gate = gate.lock().unwrap();
                let _ = gate.0.send(());
                // Until the test releases it, or fails and drops the sender.
                let _ = gate.1.recv();
            }),
        ));
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
        r.machine.set_settings(off, |_| Ok(())).unwrap();
        assert!(r.machine.state().locked);
        assert!(r.hooked.calls().is_empty());
    }
}
