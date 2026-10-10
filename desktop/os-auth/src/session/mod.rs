// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The operating system's lock and sleep signals, on which the shell locks the app when the
//! `lockOnSleep` setting is on.
//!
//! [`watch`] starts the OS's sources and calls `on_event` with a [`SessionEvent`] for each
//! signal, until the [`SessionWatch`] it returns is dropped:
//! - Linux: logind on the system bus, whose `PrepareForSleep(true)` is `Sleeping` and whose
//!   `Lock` on the app's own session is `Locked` (the session is found as polkitd finds it,
//!   [`crate::linux::session`]); on the session bus, `ActiveChanged(true)` from
//!   `org.freedesktop.ScreenSaver` (KDE and others) or from `org.gnome.ScreenSaver` (GNOME, which
//!   locks without logind's `Lock`) is `Locked`.
//! - Windows: a message-only window on a thread of its own. `WM_WTSSESSION_CHANGE` with
//!   `WTS_SESSION_LOCK` (`WTSRegisterSessionNotification`) is `Locked`; `WM_POWERBROADCAST` with
//!   `PBT_APMSUSPEND` is `Sleeping`, through `RegisterSuspendResumeNotification`, since a
//!   message-only window receives no broadcast.
//! - macOS: `NSWorkspace`'s `willSleep` and `screensDidSleep` notifications are `Sleeping`, and
//!   the distributed `com.apple.screenIsLocked` is `Locked`, delivered at once even while the
//!   app is not the active one. Both centres deliver on the main thread's run loop, which the
//!   app's event loop runs: a process without one (a test binary) hears nothing.
//!
//! Every source is optional. One the OS does not offer here (no session bus, no logind, a
//! registration the OS refuses) is logged at info and skipped, never an error; the others
//! still report. Once set up, the watch says which signals it hears ([`Listening`]): a lock
//! source, a sleep source, both or neither (WSL or a container without a bus) — so the app can
//! tell the owner when lock-on-sleep has nothing to follow. On Linux a source counts only when
//! its sender is there (a window manager without a screen saver hears no lock, a system without
//! logind no sleep), and logind's count for nothing under WSL, whose logind never sends a sleep
//! and never hears the Windows screen lock; every source is listened to all the same, so a
//! sender that starts later is heard.
//!
//! An OS signal becomes an event through [`event`], a pure function tested on every OS. Events
//! reach `on_event` in order on one thread of the watch's own, never on the OS's (the main
//! thread on macOS), so a slow callback delays no OS delivery.

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(windows)]
mod windows;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::Duration;

/// Why the app should lock now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionEvent {
    /// The OS locked the session.
    Locked,
    /// The machine, or its screens, are going to sleep.
    Sleeping,
}

/// What a source saw, before [`event`] maps it: one variant per OS signal the watch listens to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Signal {
    /// logind's `org.freedesktop.login1.Manager.PrepareForSleep(start)`: `start` before the
    /// sleep, `false` after the wake.
    PrepareForSleep { start: bool },
    /// logind's `org.freedesktop.login1.Session.Lock` on the app's own session.
    SessionLock,
    /// A screen saver's `ActiveChanged(active)`.
    ScreenSaverActive { active: bool },
    /// A message the Windows source's window received, with its `wParam`.
    WindowsMessage { message: u32, wparam: usize },
    /// `NSWorkspaceWillSleepNotification`.
    WillSleep,
    /// `NSWorkspaceScreensDidSleepNotification`.
    ScreensDidSleep,
    /// The distributed `com.apple.screenIsLocked`.
    ScreenIsLocked,
}

/// `WM_WTSSESSION_CHANGE` (`winuser.h`).
pub const WM_WTSSESSION_CHANGE: u32 = 0x02B1;
/// `WTS_SESSION_LOCK`, a `WM_WTSSESSION_CHANGE` `wParam` (`winuser.h`).
pub const WTS_SESSION_LOCK: usize = 0x7;
/// `WM_POWERBROADCAST` (`winuser.h`).
pub const WM_POWERBROADCAST: u32 = 0x0218;
/// `PBT_APMSUSPEND`, a `WM_POWERBROADCAST` `wParam` (`winuser.h`): the system is suspending.
pub const PBT_APMSUSPEND: usize = 0x4;

/// The event a signal stands for, if any: the end of a sleep, a screen saver going away and
/// every other session change are none.
pub fn event(signal: Signal) -> Option<SessionEvent> {
    match signal {
        Signal::PrepareForSleep { start } => start.then_some(SessionEvent::Sleeping),
        Signal::SessionLock | Signal::ScreenIsLocked => Some(SessionEvent::Locked),
        Signal::ScreenSaverActive { active } => active.then_some(SessionEvent::Locked),
        Signal::WillSleep | Signal::ScreensDidSleep => Some(SessionEvent::Sleeping),
        Signal::WindowsMessage { message, wparam } => match (message, wparam) {
            (WM_WTSSESSION_CHANGE, WTS_SESSION_LOCK) => Some(SessionEvent::Locked),
            (WM_POWERBROADCAST, PBT_APMSUSPEND) => Some(SessionEvent::Sleeping),
            _ => None,
        },
    }
}

/// What the dispatcher thread is told.
enum Message {
    Event(SessionEvent),
    Stop,
}

/// Which of the OS's signals a watch hears: what its sources could set up.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Listening {
    /// A source of the session's locks listens: logind's `Lock` (not under WSL) or a running
    /// screen saver on Linux, the session notification on Windows, the distributed screen-lock
    /// notification on macOS.
    pub lock: bool,
    /// A source of sleeps listens: logind's `PrepareForSleep` (logind running, not under WSL),
    /// the suspend notification, or `NSWorkspace`'s sleep notifications.
    pub sleep: bool,
}

impl Listening {
    /// Nothing listens.
    pub const NONE: Listening = Listening {
        lock: false,
        sleep: false,
    };

    /// Whether anything listens.
    pub fn any(self) -> bool {
        self.lock || self.sleep
    }
}

/// Set once a watch's sources are set up, with what listens.
#[derive(Default)]
struct Ready(Mutex<Option<Listening>>, Condvar);

/// Where a source sends what it sees: cheap to clone, `Send + Sync`, never blocks.
#[derive(Clone)]
pub(crate) struct Emitter {
    sender: Sender<Message>,
    ready: Arc<Ready>,
}

impl Emitter {
    /// Reports `signal`, if it stands for an event. A watch that has stopped hears nothing.
    pub(crate) fn signal(&self, signal: Signal) {
        if let Some(event) = event(signal) {
            let _ = self.sender.send(Message::Event(event));
        }
    }

    /// Every source that could be set up listens now; `listening` says which kinds there are.
    pub(crate) fn ready(&self, listening: Listening) {
        *self.ready.0.lock().unwrap_or_else(PoisonError::into_inner) = Some(listening);
        self.ready.1.notify_all();
    }
}

/// The OS's sources while a watch runs; dropping it stops them.
type Sources = Box<dyn Send>;

/// A running watch: dropping it stops the sources and the callbacks.
///
/// Dropping waits for a callback in progress to return, so never drop the watch while holding
/// anything `on_event` takes. No callback starts once the drop has begun.
pub struct SessionWatch {
    /// Dropped first: stops the OS's sources.
    sources: Option<Sources>,
    sender: Sender<Message>,
    stopped: Arc<AtomicBool>,
    ready: Arc<Ready>,
    dispatcher: Option<JoinHandle<()>>,
}

/// Starts watching the OS's lock and sleep signals (the module docs list them); `on_event` runs
/// on a thread of the watch's own. Never fails: a source the OS does not offer is skipped.
#[cfg(any(target_os = "linux", target_os = "macos", windows))]
pub fn watch(on_event: impl Fn(SessionEvent) + Send + 'static) -> SessionWatch {
    #[cfg(target_os = "linux")]
    use self::linux::start;
    #[cfg(target_os = "macos")]
    use self::macos::start;
    #[cfg(windows)]
    use self::windows::start;

    SessionWatch::start(on_event, |emitter| Box::new(start(emitter)))
}

impl SessionWatch {
    /// The dispatcher, then the sources `start` sets up with an [`Emitter`] of the watch's.
    fn start(
        on_event: impl Fn(SessionEvent) + Send + 'static,
        start: impl FnOnce(Emitter) -> Sources,
    ) -> Self {
        let (sender, messages) = mpsc::channel();
        let stopped = Arc::new(AtomicBool::new(false));
        let ready = Arc::new(Ready::default());
        let dispatcher = {
            let stopped = Arc::clone(&stopped);
            #[cfg(test)]
            if test_dispatcher::fails() {
                return Self::without_dispatcher(sender, stopped, ready);
            }
            thread::Builder::new()
                .name("session-watch".to_owned())
                .spawn(move || {
                    for message in messages {
                        match message {
                            Message::Event(_) if stopped.load(Ordering::SeqCst) => {}
                            Message::Event(event) => on_event(event),
                            Message::Stop => break,
                        }
                    }
                })
        };
        let dispatcher = match dispatcher {
            Ok(dispatcher) => dispatcher,
            Err(error) => {
                tracing::warn!("no thread for the session watch ({error}): nothing is watched");
                return Self::without_dispatcher(sender, stopped, ready);
            }
        };
        let emitter = Emitter {
            sender: sender.clone(),
            ready: Arc::clone(&ready),
        };
        Self {
            sources: Some(start(emitter)),
            sender,
            stopped,
            ready,
            dispatcher: Some(dispatcher),
        }
    }

    /// A watch with nothing to call back on: no source starts, and it hears nothing.
    fn without_dispatcher(
        sender: Sender<Message>,
        stopped: Arc<AtomicBool>,
        ready: Arc<Ready>,
    ) -> Self {
        let emitter = Emitter {
            sender: sender.clone(),
            ready: Arc::clone(&ready),
        };
        emitter.ready(Listening::NONE);
        Self {
            sources: None,
            sender,
            stopped,
            ready,
            dispatcher: None,
        }
    }

    /// Waits up to `timeout` until every source that could be set up listens; which signals
    /// they hear, or `None` while they are still being set up. Signals before then may be
    /// missed. `Duration::ZERO` asks without waiting.
    pub fn listening(&self, timeout: Duration) -> Option<Listening> {
        let (ready, woken) = (&self.ready.0, &self.ready.1);
        let ready = ready.lock().unwrap_or_else(PoisonError::into_inner);
        let (ready, _) = woken
            .wait_timeout_while(ready, timeout, |ready| ready.is_none())
            .unwrap_or_else(PoisonError::into_inner);
        *ready
    }
}

/// Making a watch started inside [`failing`](test_dispatcher::failing) find no thread for its
/// dispatcher, as an exhausted system would.
#[cfg(test)]
mod test_dispatcher {
    use std::cell::Cell;

    thread_local! {
        static FAIL: Cell<bool> = const { Cell::new(false) };
    }

    /// Runs `f`; a watch it starts on this thread gets no dispatcher.
    pub(super) fn failing<T>(f: impl FnOnce() -> T) -> T {
        FAIL.with(|fail| fail.set(true));
        let value = f();
        FAIL.with(|fail| fail.set(false));
        value
    }

    pub(super) fn fails() -> bool {
        FAIL.with(Cell::get)
    }
}

impl Drop for SessionWatch {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::SeqCst);
        drop(self.sources.take());
        let _ = self.sender.send(Message::Stop);
        if let Some(dispatcher) = self.dispatcher.take() {
            // Dropped from a callback: the dispatcher ends once it returns.
            if dispatcher.thread().id() != thread::current().id() {
                let _ = dispatcher.join();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc::Receiver;
    use std::time::Instant;

    use super::test_dispatcher;
    use super::*;
    use SessionEvent::{Locked, Sleeping};

    /// Longer than any wait a passing test makes; a failing one panics instead of hanging.
    const PATIENCE: Duration = Duration::from_secs(10);
    /// How long a test listens for an event that must not come.
    const QUIET: Duration = Duration::from_millis(200);

    #[test]
    fn every_signal_has_its_event() {
        for (signal, expected) in [
            (Signal::PrepareForSleep { start: true }, Some(Sleeping)),
            (Signal::PrepareForSleep { start: false }, None),
            (Signal::SessionLock, Some(Locked)),
            (Signal::ScreenSaverActive { active: true }, Some(Locked)),
            (Signal::ScreenSaverActive { active: false }, None),
            (Signal::WillSleep, Some(Sleeping)),
            (Signal::ScreensDidSleep, Some(Sleeping)),
            (Signal::ScreenIsLocked, Some(Locked)),
        ] {
            assert_eq!(event(signal), expected, "{signal:?}");
        }
    }

    #[test]
    fn only_a_session_lock_and_a_suspend_are_windows_events() {
        let message = |message, wparam| Signal::WindowsMessage { message, wparam };
        for (signal, expected) in [
            (
                message(WM_WTSSESSION_CHANGE, WTS_SESSION_LOCK),
                Some(Locked),
            ),
            (message(WM_POWERBROADCAST, PBT_APMSUSPEND), Some(Sleeping)),
            // WTS_SESSION_UNLOCK, WTS_CONSOLE_DISCONNECT, WTS_REMOTE_DISCONNECT, a logoff.
            (message(WM_WTSSESSION_CHANGE, 0x8), None),
            (message(WM_WTSSESSION_CHANGE, 0x2), None),
            (message(WM_WTSSESSION_CHANGE, 0x4), None),
            (message(WM_WTSSESSION_CHANGE, 0x6), None),
            // PBT_APMRESUMESUSPEND, PBT_APMRESUMEAUTOMATIC, PBT_APMPOWERSTATUSCHANGE.
            (message(WM_POWERBROADCAST, 0x7), None),
            (message(WM_POWERBROADCAST, 0x12), None),
            (message(WM_POWERBROADCAST, 0xA), None),
            // The values crossed, and other messages with them.
            (message(WM_WTSSESSION_CHANGE, PBT_APMSUSPEND), None),
            (message(WM_POWERBROADCAST, WTS_SESSION_LOCK), None),
            (message(0x0010, WTS_SESSION_LOCK), None),
            (message(0x0002, PBT_APMSUSPEND), None),
        ] {
            assert_eq!(event(signal), expected, "{signal:?}");
        }
        assert_eq!(
            (WM_WTSSESSION_CHANGE, WM_POWERBROADCAST),
            (689, 536),
            "winuser.h"
        );
    }

    const BOTH: Listening = Listening {
        lock: true,
        sleep: true,
    };

    /// A watch whose one source is the returned emitter, and the events it reports.
    fn fake_watch() -> (SessionWatch, Emitter, Receiver<SessionEvent>) {
        let (events, received) = mpsc::channel();
        let (emitters, emitter) = mpsc::channel();
        let watch = SessionWatch::start(
            move |event| {
                let _ = events.send(event);
            },
            move |emitter: Emitter| {
                emitters.send(emitter.clone()).unwrap();
                emitter.ready(BOTH);
                Box::new(())
            },
        );
        (watch, emitter.recv().unwrap(), received)
    }

    #[test]
    fn a_source_s_signals_reach_the_callback_in_order() {
        let (watch, source, events) = fake_watch();
        assert_eq!(watch.listening(PATIENCE), Some(BOTH));
        source.signal(Signal::SessionLock);
        source.signal(Signal::PrepareForSleep { start: false });
        source.signal(Signal::PrepareForSleep { start: true });
        source.signal(Signal::ScreenSaverActive { active: true });
        assert_eq!(events.recv_timeout(PATIENCE), Ok(Locked));
        assert_eq!(events.recv_timeout(PATIENCE), Ok(Sleeping));
        assert_eq!(events.recv_timeout(PATIENCE), Ok(Locked));
        assert!(events.recv_timeout(QUIET).is_err(), "the wake is no event");
    }

    #[test]
    fn the_callback_runs_on_the_watch_s_own_thread() {
        let (threads, names) = mpsc::channel();
        let (emitters, emitter) = mpsc::channel();
        let _watch = SessionWatch::start(
            move |_| {
                let _ = threads.send(thread::current().name().map(str::to_owned));
            },
            move |emitter: Emitter| {
                emitters.send(emitter).unwrap();
                Box::new(())
            },
        );
        let source: Emitter = emitter.recv().unwrap();
        thread::spawn(move || source.signal(Signal::SessionLock))
            .join()
            .unwrap();
        assert_eq!(
            names.recv_timeout(PATIENCE),
            Ok(Some("session-watch".to_owned()))
        );
    }

    #[test]
    fn a_dropped_watch_calls_back_no_more_and_stops_its_sources() {
        struct Stopped(mpsc::Sender<()>);
        impl Drop for Stopped {
            fn drop(&mut self) {
                let _ = self.0.send(());
            }
        }
        let (events, received) = mpsc::channel();
        let (emitters, emitter) = mpsc::channel();
        let (stopped, sources_stopped) = mpsc::channel();
        let watch = SessionWatch::start(
            move |event| {
                let _ = events.send(event);
            },
            move |emitter: Emitter| {
                emitters.send(emitter).unwrap();
                Box::new(Stopped(stopped))
            },
        );
        let source: Emitter = emitter.recv().unwrap();
        source.signal(Signal::SessionLock);
        assert_eq!(received.recv_timeout(PATIENCE), Ok(Locked));
        drop(watch);
        assert_eq!(
            sources_stopped.try_recv(),
            Ok(()),
            "the sources were dropped"
        );
        source.signal(Signal::SessionLock);
        assert!(
            received.recv_timeout(QUIET).is_err(),
            "no callback after the drop"
        );
    }

    /// Events still queued when the drop begins are discarded, not delivered during it.
    #[test]
    fn events_queued_at_the_drop_are_discarded() {
        let (entered_tx, entered) = mpsc::channel();
        let (release, release_rx) = mpsc::channel::<()>();
        let (events, received) = mpsc::channel();
        let (emitters, emitter) = mpsc::channel();
        let watch = SessionWatch::start(
            move |event| {
                if entered_tx.send(()).is_ok() {
                    let _ = release_rx.recv_timeout(PATIENCE);
                }
                let _ = events.send(event);
            },
            move |emitter: Emitter| {
                emitters.send(emitter).unwrap();
                Box::new(())
            },
        );
        let source: Emitter = emitter.recv().unwrap();
        source.signal(Signal::SessionLock);
        entered.recv_timeout(PATIENCE).unwrap();
        source.signal(Signal::WillSleep);
        source.signal(Signal::ScreenIsLocked);
        drop(entered);
        let dropping = thread::spawn(move || drop(watch));
        thread::sleep(QUIET);
        release.send(()).unwrap();
        dropping.join().unwrap();
        assert_eq!(received.try_recv(), Ok(Locked), "the callback in progress");
        assert!(received.try_recv().is_err(), "nothing queued after it");
    }

    /// A callback may drop its own watch without waiting for itself.
    #[test]
    fn a_callback_can_drop_the_watch() {
        let slot: Arc<Mutex<Option<SessionWatch>>> = Arc::default();
        let (done, finished) = mpsc::channel();
        let (emitters, emitter) = mpsc::channel();
        let watch = {
            let slot = Arc::clone(&slot);
            SessionWatch::start(
                move |_| {
                    drop(slot.lock().unwrap().take());
                    let _ = done.send(());
                },
                move |emitter: Emitter| {
                    emitters.send(emitter).unwrap();
                    Box::new(())
                },
            )
        };
        *slot.lock().unwrap() = Some(watch);
        let source: Emitter = emitter.recv().unwrap();
        source.signal(Signal::SessionLock);
        assert_eq!(finished.recv_timeout(PATIENCE), Ok(()));
    }

    #[test]
    fn listening_waits_for_the_sources_and_no_longer_than_asked() {
        let (emitters, emitter) = mpsc::channel();
        let watch = SessionWatch::start(
            |_| {},
            move |emitter: Emitter| {
                emitters.send(emitter).unwrap();
                Box::new(())
            },
        );
        let started = Instant::now();
        assert_eq!(watch.listening(Duration::from_millis(50)), None);
        assert!(started.elapsed() >= Duration::from_millis(50));
        assert_eq!(
            watch.listening(Duration::ZERO),
            None,
            "asked without waiting"
        );
        let source: Emitter = emitter.recv().unwrap();
        thread::spawn(move || {
            thread::sleep(Duration::from_millis(30));
            source.ready(BOTH);
        });
        assert_eq!(watch.listening(PATIENCE), Some(BOTH));
        assert_eq!(
            watch.listening(Duration::ZERO),
            Some(BOTH),
            "and from then on"
        );
    }

    /// What the sources set up is what the watch says it hears: nothing, either half, or both.
    #[test]
    fn a_watch_says_which_signals_its_sources_hear() {
        for listening in [
            Listening::NONE,
            Listening {
                lock: true,
                sleep: false,
            },
            Listening {
                lock: false,
                sleep: true,
            },
            BOTH,
        ] {
            let watch = SessionWatch::start(
                |_| {},
                move |emitter: Emitter| {
                    emitter.ready(listening);
                    Box::new(())
                },
            );
            assert_eq!(watch.listening(PATIENCE), Some(listening));
            assert_eq!(listening.any(), listening.lock || listening.sleep);
        }
        assert!(!Listening::NONE.any());
    }

    /// Without a thread to call back on, no source starts, and the watch says it hears nothing
    /// rather than leave the question open.
    #[test]
    fn without_a_dispatcher_nothing_listens() {
        let started = Arc::new(AtomicBool::new(false));
        let watch = test_dispatcher::failing(|| {
            let started = Arc::clone(&started);
            SessionWatch::start(
                |_| {},
                move |emitter: Emitter| {
                    started.store(true, Ordering::SeqCst);
                    emitter.ready(BOTH);
                    Box::new(())
                },
            )
        });
        assert_eq!(watch.listening(Duration::ZERO), Some(Listening::NONE));
        assert!(!started.load(Ordering::SeqCst), "no source started");
    }

    #[test]
    fn a_watch_can_move_to_another_thread() {
        fn send<T: Send>() {}
        send::<SessionWatch>();
    }
}
