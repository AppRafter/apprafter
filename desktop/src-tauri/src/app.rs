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

use std::io;
use std::panic::{self, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering::SeqCst};
use std::sync::{Arc, Condvar, Mutex, OnceLock, PoisonError};
use std::thread;
use std::time::Duration;

use apprafter_core::Context;
use apprafter_desktop_ipc::{
    AppInfo, LockReason, LockState, OpId, Os, Quitting, SecretBackend, Settings, SubscriptionId,
    LOCK_CHANGED, QUITTING,
};
use tauri::ipc::Invoke;
use tauri::webview::PageLoadEvent;
use tauri::{AppHandle, Emitter, ExitRequestApi, Manager, Runtime};

use zeroize::Zeroizing;

use crate::auth::Authenticator;
use crate::commands;
use crate::errors::{DesktopError, Refusal};
use crate::lock::{LockHook, LockMachine};
use crate::ops::{panic_message, Clock, EventSink, OperationManager};
use crate::settings::SettingsStore;

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
    ///
    /// `auth` is given the settings first ([`Authenticator::apply_settings`]: Windows' `hello`),
    /// and again after every save.
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
        auth.apply_settings(&settings.get());
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

    /// The `app_info` answer. Its `auth` is the authenticator's answer now, never one kept from
    /// the start: it changes (a polkit dialog that found no agent moves Linux to the password
    /// field, a lock moves it back).
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
        self.lock.unlock()?;
        Ok(self.lock.state())
    }

    /// Unlock with the password from the lock screen's own field (blocks while it is checked);
    /// the state that results.
    pub fn unlock_with_password(&self, password: Zeroizing<String>) -> Result<LockState, Refusal> {
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

    /// The first step of a quit, once: no operation starts and no unlock prompt opens from
    /// here on (`Closing`), the open unlock prompt and every operation prompt are closed, and
    /// every pending plan is dropped. `false` when a quit had already begun.
    pub fn begin_quit(&self) -> bool {
        if self.quitting.swap(true, SeqCst) {
            return false;
        }
        self.ops.close();
        self.lock.close();
        self.ops.drop_all_plans();
        true
    }

    /// The event loop is exiting (`RunEvent::Exit`, on the main thread): the process ends once
    /// this returns.
    ///
    /// When the quit has not waited for the running operations, the exit never went through
    /// [`quit`]: the OS ended the app without asking (see the module docs). The quit begins
    /// here, every subscription ends (the webview goes with the process), and this cancels the
    /// running operations and waits for them up to [`FORCED_STOP_BOUND`]. Either way the
    /// tickers stop.
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

/// The opener plugin as the app uses it: `open_url`, which the capability scopes to the three
/// links the page shows, exactly as written. No injected script: the plugin's default would
/// also open any `<a target="_blank">`, or a link clicked with Ctrl or Shift, in the browser on
/// its own, a way out the page never needs (its links go through `open_url`) and an injected
/// link could use.
pub fn opener_plugin<R: Runtime>() -> impl tauri::plugin::Plugin<R> {
    tauri_plugin_opener::Builder::new()
        .open_js_links_on_click(false)
        .build()
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

/// The idle ticker's work: lock once the idle time has passed, and drop the plans one time to
/// live past their expiry.
fn idle_tick(shell: &Shell) {
    shell.lock.tick();
    shell.ops.sweep();
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

    use apprafter_core::{CancellationToken, Context, Event, Outcome, PlanClass};
    use apprafter_desktop_ipc::{
        errors, AuthInfo, AuthOutcome, AutoLock, CancelledBy, LockReason, LockState, OpEvent, OpId,
        Settings, UnavailableReason,
    };
    use serde_json::json;

    use zeroize::Zeroizing;

    use super::Shell;
    use crate::auth::test_os::{self, Call, ScriptedOs};
    use crate::auth::{AuthPurpose, Authenticator, FakeAuthenticator, NoAuthenticator};
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
                false,
                move |state| notified.lock().unwrap().push(state.clone()),
            )
        };
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
        let auth = Arc::new(FakeAuthenticator::new().with_password("open sesame"));
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
}
