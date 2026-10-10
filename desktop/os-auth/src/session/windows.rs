// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Windows: a message-only window on a thread of its own, which Windows tells of the session's
//! lock (`WTSRegisterSessionNotification`) and of a suspend (`RegisterSuspendResumeNotification`:
//! a message-only window receives no broadcast, so `WM_POWERBROADCAST` reaches it only through
//! that registration).
//!
//! The window's thread runs its message loop until the window is destroyed. Dropping the
//! sources posts `WM_CLOSE`, which unregisters both notifications and destroys the window; its
//! `WM_DESTROY` ends the loop, and the drop joins the thread. Either registration may fail (a
//! Windows without the service behind it): it is logged and the other still reports.

use std::cell::RefCell;
use std::sync::mpsc;
use std::thread::{self, JoinHandle};

use ::windows::core::{w, PCWSTR};
use ::windows::Win32::Foundation::{HANDLE, HWND, LPARAM, LRESULT, WPARAM};
use ::windows::Win32::System::LibraryLoader::GetModuleHandleW;
use ::windows::Win32::System::Power::{
    RegisterSuspendResumeNotification, UnregisterSuspendResumeNotification, HPOWERNOTIFY,
};
use ::windows::Win32::System::RemoteDesktop::{
    WTSRegisterSessionNotification, WTSUnRegisterSessionNotification, NOTIFY_FOR_THIS_SESSION,
};
use ::windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW, PostMessageW,
    PostQuitMessage, RegisterClassExW, DEVICE_NOTIFY_WINDOW_HANDLE, HWND_MESSAGE, MSG,
    WINDOW_EX_STYLE, WINDOW_STYLE, WM_CLOSE, WM_DESTROY, WNDCLASSEXW,
};

use super::{Emitter, Listening, Signal, WM_POWERBROADCAST, WM_WTSSESSION_CHANGE};

/// The window class every watch's window is created from.
const CLASS: PCWSTR = w!("AppRafterDesktopSessionWatch");

/// What the window's thread keeps for its window procedure: one window per thread.
struct Window {
    emitter: Emitter,
    /// Registered for session changes.
    session: bool,
    /// The suspend and resume registration.
    power: Option<HPOWERNOTIFY>,
}

thread_local! {
    static WINDOW: RefCell<Option<Window>> = const { RefCell::new(None) };
}

/// The running window: dropping it closes the window and waits for its thread.
pub(super) struct Sources {
    /// The window's `HWND`, as an integer so the sources can move between threads.
    window: isize,
    thread: Option<JoinHandle<()>>,
}

impl Drop for Sources {
    fn drop(&mut self) {
        let Some(thread) = self.thread.take() else {
            return;
        };
        // SAFETY: posting only queues a message; a handle that no longer names a window is
        // refused with an error, never undefined behaviour.
        let closed = unsafe {
            PostMessageW(
                Some(HWND(self.window as *mut _)),
                WM_CLOSE,
                WPARAM(0),
                LPARAM(0),
            )
        };
        match closed {
            Ok(()) => {
                let _ = thread.join();
            }
            // Joining would wait for ever; the thread is left to the process's end.
            Err(error) => tracing::warn!("the session watch's window cannot be closed ({error})"),
        }
    }
}

pub(super) fn start(emitter: Emitter) -> Sources {
    let (created, window) = mpsc::channel();
    let thread = {
        let emitter = emitter.clone();
        thread::Builder::new()
            .name("session-watch-window".to_owned())
            .spawn(move || run(emitter, &created))
    };
    let sources = match thread {
        Err(error) => {
            tracing::warn!("no thread for the session watch's window ({error}): not watched");
            None
        }
        Ok(thread) => match window.recv() {
            Ok(Some((window, listening))) => Some((
                Sources {
                    window,
                    thread: Some(thread),
                },
                listening,
            )),
            // No window: the thread has ended or is ending.
            Ok(None) | Err(_) => {
                let _ = thread.join();
                None
            }
        },
    };
    let (sources, listening) = sources.unwrap_or((
        Sources {
            window: 0,
            thread: None,
        },
        Listening::NONE,
    ));
    emitter.ready(listening);
    sources
}

/// What the window hears: the session's locks once registered for session changes, sleeps
/// once registered for suspends.
fn listening(session: bool, power: bool) -> Listening {
    Listening {
        lock: session,
        sleep: power,
    }
}

/// The window's thread: creates the window, registers it, says so through `created` with what
/// it hears, and runs the message loop until the window is destroyed.
fn run(emitter: Emitter, created: &mpsc::Sender<Option<(isize, Listening)>>) {
    let Some(window) = create_window() else {
        let _ = created.send(None);
        return;
    };
    // SAFETY: `window` is the window this thread just created, which lives until its WM_CLOSE.
    let session = match unsafe { WTSRegisterSessionNotification(window, NOTIFY_FOR_THIS_SESSION) } {
        Ok(()) => true,
        Err(error) => {
            tracing::info!(
                "no session notifications ({error}): the session's locks are not watched"
            );
            false
        }
    };
    // SAFETY: as above; a window handle is what DEVICE_NOTIFY_WINDOW_HANDLE says the recipient
    // is.
    let power = match unsafe {
        RegisterSuspendResumeNotification(HANDLE(window.0), DEVICE_NOTIFY_WINDOW_HANDLE)
    } {
        Ok(power) => Some(power),
        Err(error) => {
            tracing::info!("no suspend notifications ({error}): sleeps are not watched");
            None
        }
    };
    WINDOW.with(|slot| {
        *slot.borrow_mut() = Some(Window {
            emitter,
            session,
            power,
        });
    });
    let _ = created.send(Some((
        window.0 as isize,
        listening(session, power.is_some()),
    )));
    let mut message = MSG::default();
    loop {
        // SAFETY: `message` is a valid MSG for the call to fill; no window filter, so it reads
        // this thread's queue.
        let got = unsafe { GetMessageW(&mut message, None, 0, 0) };
        // 0: WM_QUIT, after WM_DESTROY. -1: an error, which would repeat for ever.
        if got.0 <= 0 {
            break;
        }
        // SAFETY: `message` is the one GetMessageW just filled.
        unsafe { DispatchMessageW(&message) };
    }
    WINDOW.with(|slot| slot.borrow_mut().take());
}

/// A message-only window of [`CLASS`], or `None`, logged.
fn create_window() -> Option<HWND> {
    // SAFETY: `None` asks for the module of the running executable, which is never freed.
    let instance = match unsafe { GetModuleHandleW(None) } {
        Ok(module) => module.into(),
        Err(error) => {
            tracing::info!("no module handle ({error}): the session is not watched");
            return None;
        }
    };
    let class = WNDCLASSEXW {
        cbSize: u32::try_from(std::mem::size_of::<WNDCLASSEXW>()).ok()?,
        lpfnWndProc: Some(window_procedure),
        hInstance: instance,
        lpszClassName: CLASS,
        ..Default::default()
    };
    // SAFETY: `class` is a complete WNDCLASSEXW whose name is a static string. A second watch
    // finds the class registered already (0, ERROR_CLASS_ALREADY_EXISTS), which is no error:
    // creating the window says whether the class is there.
    unsafe { RegisterClassExW(&class) };
    // SAFETY: the class is registered for this module; HWND_MESSAGE makes the window
    // message-only, with no size, style or menu.
    let created = unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            CLASS,
            PCWSTR::null(),
            WINDOW_STYLE(0),
            0,
            0,
            0,
            0,
            Some(HWND_MESSAGE),
            None,
            Some(instance),
            None,
        )
    };
    match created {
        Ok(window) => Some(window),
        Err(error) => {
            tracing::info!("no message-only window ({error}): the session is not watched");
            None
        }
    }
}

/// The window's procedure, on the window's thread. Never panics: it is called from Windows.
extern "system" fn window_procedure(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_WTSSESSION_CHANGE | WM_POWERBROADCAST => {
            WINDOW.with(|slot| {
                if let Ok(slot) = slot.try_borrow() {
                    if let Some(state) = slot.as_ref() {
                        state.emitter.signal(Signal::WindowsMessage {
                            message,
                            wparam: wparam.0,
                        });
                    }
                }
            });
            // WM_POWERBROADCAST asks for TRUE; WM_WTSSESSION_CHANGE's result is ignored.
            LRESULT(1)
        }
        WM_CLOSE => {
            close(window);
            LRESULT(0)
        }
        WM_DESTROY => {
            // SAFETY: on the window's own thread, which runs the loop this ends.
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        // SAFETY: the arguments are the ones Windows passed for this window.
        _ => unsafe { DefWindowProcW(window, message, wparam, lparam) },
    }
}

/// Unregisters both notifications, which must happen before the window goes, then destroys it.
fn close(window: HWND) {
    let registered = WINDOW.with(|slot| {
        slot.try_borrow()
            .ok()
            .and_then(|slot| slot.as_ref().map(|state| (state.session, state.power)))
    });
    if let Some((session, power)) = registered {
        if session {
            // SAFETY: `window` is registered and not yet destroyed.
            let _ = unsafe { WTSUnRegisterSessionNotification(window) };
        }
        if let Some(power) = power {
            // SAFETY: the handle RegisterSuspendResumeNotification returned, unregistered once.
            let _ = unsafe { UnregisterSuspendResumeNotification(power) };
        }
    }
    // SAFETY: on the thread that created `window`, as DestroyWindow requires.
    let _ = unsafe { DestroyWindow(window) };
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use ::windows::Win32::UI::WindowsAndMessaging as win32;

    use super::super::{SessionEvent, SessionWatch, PBT_APMSUSPEND, WTS_SESSION_LOCK};
    use super::*;

    /// Longer than any wait a passing test makes; a failing one panics instead of hanging.
    const PATIENCE: Duration = Duration::from_secs(10);
    const QUIET: Duration = Duration::from_millis(300);

    #[test]
    fn the_constants_are_windows_own() {
        assert_eq!(WM_WTSSESSION_CHANGE, win32::WM_WTSSESSION_CHANGE);
        assert_eq!(WM_POWERBROADCAST, win32::WM_POWERBROADCAST);
        assert_eq!(WTS_SESSION_LOCK, win32::WTS_SESSION_LOCK as usize);
        assert_eq!(PBT_APMSUSPEND, win32::PBT_APMSUSPEND as usize);
    }

    fn post(window: isize, message: u32, wparam: usize) -> ::windows::core::Result<()> {
        // SAFETY: as in `Sources::drop`.
        unsafe {
            PostMessageW(
                Some(HWND(window as *mut _)),
                message,
                WPARAM(wparam),
                LPARAM(0),
            )
        }
    }

    /// The window's procedure, through a real message-only window and its message loop: what
    /// Windows would send for a lock and a suspend becomes the events, and nothing else does.
    #[test]
    fn the_window_reports_a_lock_and_a_suspend() {
        let (events, received) = mpsc::channel();
        let (windows, window) = mpsc::channel();
        let watch = SessionWatch::start(
            move |event| {
                let _ = events.send(event);
            },
            move |emitter| {
                let sources = start(emitter);
                windows.send(sources.window).unwrap();
                Box::new(sources)
            },
        );
        let listening = watch.listening(PATIENCE).expect("the watch was set up");
        // Wine registers both; a Windows without the services behind them would say so here.
        assert_eq!(
            listening,
            Listening {
                lock: true,
                sleep: true
            },
            "both registrations"
        );
        let window = window.recv().unwrap();
        assert_ne!(window, 0, "a window was created");

        post(window, WM_WTSSESSION_CHANGE, WTS_SESSION_LOCK).unwrap();
        assert_eq!(received.recv_timeout(PATIENCE), Ok(SessionEvent::Locked));
        post(window, WM_POWERBROADCAST, PBT_APMSUSPEND).unwrap();
        assert_eq!(received.recv_timeout(PATIENCE), Ok(SessionEvent::Sleeping));
        // WTS_SESSION_UNLOCK and PBT_APMRESUMEAUTOMATIC.
        post(window, WM_WTSSESSION_CHANGE, 0x8).unwrap();
        post(window, WM_POWERBROADCAST, 0x12).unwrap();
        assert!(received.recv_timeout(QUIET).is_err());

        drop(watch);
        assert!(
            post(window, WM_WTSSESSION_CHANGE, WTS_SESSION_LOCK).is_err(),
            "the window is gone"
        );
    }

    #[test]
    fn each_registration_is_its_half() {
        assert_eq!(listening(false, false), Listening::NONE);
        assert_eq!(
            listening(true, false),
            Listening {
                lock: true,
                sleep: false
            }
        );
        assert_eq!(
            listening(false, true),
            Listening {
                lock: false,
                sleep: true
            }
        );
    }

    /// Two watches at once: the class is registered once, and each has its own window.
    #[test]
    fn two_watches_have_a_window_each() {
        let first = SessionWatch::start(|_| {}, |emitter| Box::new(start(emitter)));
        let (events, received) = mpsc::channel();
        let (windows, window) = mpsc::channel();
        let second = SessionWatch::start(
            move |event| {
                let _ = events.send(event);
            },
            move |emitter| {
                let sources = start(emitter);
                windows.send(sources.window).unwrap();
                Box::new(sources)
            },
        );
        let window = window.recv().unwrap();
        assert_ne!(window, 0);
        drop(first);
        post(window, WM_WTSSESSION_CHANGE, WTS_SESSION_LOCK).unwrap();
        assert_eq!(received.recv_timeout(PATIENCE), Ok(SessionEvent::Locked));
        drop(second);
    }
}
