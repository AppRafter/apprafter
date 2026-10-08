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
//! Every way out — the `quit` command, the last window closing, the OS asking — goes through
//! [`quit`]: [`Shell::begin_quit`] closes the operation manager (nothing new starts), the
//! unlock prompt and every operation prompt, and drops every plan; then a `quit` thread cancels
//! the running operations, waits up to [`STOP_BOUND`] for them to stop, and exits. The run
//! loop's exit request is refused until that thread is done ([`on_exit_requested`]).

use std::io;
use std::panic::{self, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering::SeqCst};
use std::sync::{Arc, Condvar, Mutex, OnceLock, PoisonError};
use std::thread;
use std::time::Duration;

use apprafter_core::Context;
use apprafter_desktop_ipc::{
    AppInfo, LockReason, LockState, OpId, Os, SecretBackend, Settings, SubscriptionId, LOCK_CHANGED,
};
use tauri::ipc::Invoke;
use tauri::webview::PageLoadEvent;
use tauri::{AppHandle, Emitter, ExitRequestApi, Manager, Runtime};

use crate::auth::Authenticator;
use crate::commands;
use crate::errors::DesktopError;
use crate::lock::{LockHook, LockMachine};
use crate::ops::{panic_message, Clock, EventSink, OperationManager};
use crate::settings::SettingsStore;

/// How long a quit waits for cancelled operations to stop before it exits anyway: the CLI's
/// helper-pod stop bound.
pub const STOP_BOUND: Duration = Duration::from_secs(15);

/// How often the idle ticker asks the lock whether the idle time has passed.
pub const IDLE_TICK: Duration = Duration::from_secs(5);

/// How often the output flusher sends tool output that waited long enough (the reporter's
/// flush age).
pub const FLUSH_TICK: Duration = Duration::from_millis(crate::ops::reporter::FLUSH_AGE_MS);

/// What `app_info` says when the OS does not name the account or the host.
const UNKNOWN: &str = "unknown";

/// Everything the commands work on, built once per process.
pub struct Shell {
    pub settings: SettingsStore,
    pub lock: Arc<LockMachine>,
    pub ops: Arc<OperationManager>,
    pub auth: Arc<dyn Authenticator>,
    /// The core's view of this process: the target store and the provider API.
    pub context: Context,
    /// A test build (fake authentication): `app_info` tells the webview to say so.
    pub test_build: bool,
    /// Set by the first [`begin_quit`](Self::begin_quit).
    quitting: AtomicBool,
    /// Set once the quit has waited for the running operations: the exit may go through.
    drained: AtomicBool,
    /// Set when the event loop exits: the tickers stop.
    stop: Stop,
}

/// The tickers' stop signal: a ticker waits on it between ticks, so stopping wakes it at once.
#[derive(Default)]
struct Stop {
    stopped: Mutex<bool>,
    wake: Condvar,
}

impl Stop {
    /// Wait `every`, or until stopped; `true` once stopped.
    fn wait(&self, every: Duration) -> bool {
        let stopped = self.stopped.lock().unwrap_or_else(PoisonError::into_inner);
        let (stopped, _) = self
            .wake
            .wait_timeout_while(stopped, every, |stopped| !*stopped)
            .unwrap_or_else(PoisonError::into_inner);
        *stopped
    }

    fn stop(&self) {
        *self.stopped.lock().unwrap_or_else(PoisonError::into_inner) = true;
        self.wake.notify_all();
    }
}

impl Shell {
    /// The shell over `settings`, its lock starting as they say. Every lock transition — to
    /// locked and to unlocked — drops every pending plan, then ends every operation
    /// subscription, then calls `on_lock_change` with the new state (the app emits
    /// [`LOCK_CHANGED`]). All three run under the lock machine's lock: none may call back
    /// into it.
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
    pub fn new(
        settings: SettingsStore,
        auth: Arc<dyn Authenticator>,
        clock: Arc<dyn Clock>,
        context: Context,
        test_build: bool,
        on_lock_change: impl Fn(&LockState) + Send + Sync + 'static,
    ) -> Arc<Self> {
        let ops = OperationManager::new(clock.clone());
        let hook: LockHook = {
            let ops = ops.clone();
            Box::new(move |state: &LockState| {
                ops.drop_all_plans();
                ops.drop_all_subscribers();
                on_lock_change(state);
            })
        };
        let lock = Arc::new(LockMachine::new(settings.get(), auth.clone(), clock, hook));
        Arc::new(Self {
            settings,
            lock,
            ops,
            auth,
            context,
            test_build,
            quitting: AtomicBool::new(false),
            drained: AtomicBool::new(false),
            stop: Stop::default(),
        })
    }

    /// The `app_info` answer.
    pub fn app_info(&self) -> AppInfo {
        AppInfo {
            os: current_os(),
            desktop_version: env!("CARGO_PKG_VERSION").into(),
            core_version: apprafter_core::VERSION.into(),
            secret_backend: SecretBackend::File,
            account: whoami::username().unwrap_or_else(|_| UNKNOWN.into()),
            host: whoami::hostname().unwrap_or_else(|_| UNKNOWN.into()),
            auth: self.auth.info(),
            test_build: self.test_build,
            settings_notice: self.settings.notice(),
        }
    }

    /// Save `settings` and apply them, through the lock machine (which refuses switching the
    /// lock on with nothing to verify the owner); the settings now in use.
    pub fn set_settings(&self, settings: Settings) -> Result<Settings, DesktopError> {
        self.lock
            .set_settings(settings, |new| self.settings.set(new.clone()))?;
        Ok(self.settings.get())
    }

    /// Lock now; the state that results — unlocked still when the lock is not in effect.
    pub fn lock_now(&self) -> LockState {
        self.lock.lock(LockReason::Manual);
        self.lock.state()
    }

    /// Ask the owner and unlock (blocks while the OS prompt is open); the state that results.
    pub fn unlock(&self) -> Result<LockState, DesktopError> {
        self.lock.unlock()?;
        Ok(self.lock.state())
    }

    /// Run plan `id`, `sink` following it from before the prompt. The subscription it holds,
    /// for `op_unsubscribe`; on an error the sink is unsubscribed again, so a busy prompt's
    /// retry does not leave the first attempt's sink behind.
    pub fn execute(
        &self,
        id: OpId,
        sink: Arc<dyn EventSink>,
    ) -> Result<SubscriptionId, DesktopError> {
        let subscription = self.ops.subscribe(id, sink)?.subscription;
        match self.ops.execute(id, &*self.auth) {
            Ok(_) => Ok(subscription),
            Err(e) => {
                self.ops.unsubscribe(id, subscription);
                Err(e)
            }
        }
    }

    /// The first step of a quit, once: no operation starts from here on (`Closing`), the
    /// unlock prompt and every operation prompt are closed, and every pending plan is
    /// dropped. `false` when a quit had already begun.
    pub fn begin_quit(&self) -> bool {
        if self.quitting.swap(true, SeqCst) {
            return false;
        }
        self.ops.close();
        self.lock.close_prompt();
        self.ops.drop_all_plans();
        true
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
        commands::activity,
        commands::quit,
        commands::op_list,
        commands::op_subscribe,
        commands::op_unsubscribe,
        commands::op_cancel,
        commands::op_discard,
        commands::op_execute,
    ]);
    let gate = cell.clone();
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

fn drain_and_exit<R: Runtime>(app: &AppHandle<R>, shell: &Shell) {
    if !shell.ops.cancel_all_and_wait(STOP_BOUND) {
        tracing::warn!(
            running = shell.ops.running(),
            "operations still running {STOP_BOUND:?} after the quit cancelled them; exiting"
        );
    }
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

/// Start the idle ticker (every [`IDLE_TICK`], the lock's idle check) and the output flusher
/// (every [`FLUSH_TICK`], the operations' waiting output and long-expired plans), each on a
/// `std::thread` of its own — never an async task: the lock hook they can set off drops plans
/// and starts threads. They stop once the event loop exits ([`Shell::stop_tickers`]). An
/// `Err` means no idle lock, so the app must not start.
pub fn start_tickers(shell: &Arc<Shell>) -> io::Result<()> {
    spawn_ticker("idle-ticker", IDLE_TICK, shell, |shell| shell.lock.tick())?;
    spawn_ticker("output-flusher", FLUSH_TICK, shell, |shell| {
        shell.ops.flush_due()
    })
}

fn spawn_ticker(
    name: &str,
    every: Duration,
    shell: &Arc<Shell>,
    tick: fn(&Shell),
) -> io::Result<()> {
    let shell = shell.clone();
    let label = name.to_string();
    thread::Builder::new()
        .name(label.clone())
        .spawn(move || {
            while !shell.stop.wait(every) {
                // One bad tick must not end the ticker, and with it every later auto-lock.
                if let Err(payload) = panic::catch_unwind(AssertUnwindSafe(|| tick(&shell))) {
                    tracing::error!("{label} panicked: {}", panic_message(&*payload));
                }
            }
        })
        .map(drop)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering::SeqCst};
    use std::sync::mpsc;
    use std::sync::{Arc, Mutex};
    use std::thread;
    use std::time::{Duration, Instant};

    use apprafter_core::{CancellationToken, Context, Event, Outcome, PlanClass};
    use apprafter_desktop_ipc::{
        errors, AuthInfo, AuthOutcome, AutoLock, CancelledBy, LockReason, LockState, OpEvent, OpId,
        Settings,
    };
    use serde_json::json;

    use super::Shell;
    use crate::auth::{AuthPurpose, Authenticator, FakeAuthenticator, NoAuthenticator};
    use crate::errors::DesktopError;
    use crate::ops::test_clock::ManualClock;
    use crate::ops::{EventSink, Executor, PlanParts};
    use crate::settings::SettingsStore;

    const T0: u64 = 1_700_000_000_000;
    const LONG: Duration = Duration::from_secs(10);

    struct Rig {
        _dir: tempfile::TempDir,
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
                clock,
                Context::for_desktop(dir.path().join("store"), "http://127.0.0.1:9"),
                false,
                move |state| notified.lock().unwrap().push(state.clone()),
            )
        };
        Rig {
            _dir: dir,
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
        r.shell.execute(id, Arc::new(Sink::default())).unwrap();
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
}
