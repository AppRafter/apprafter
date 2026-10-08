// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Quitting on a signal, on Linux and macOS: `SIGTERM` (a logout, a shutdown, `kill`), `SIGHUP`
//! (the terminal the app was started from closed) and `SIGINT` (Ctrl-C in it) run the quit
//! sequence ([`app::quit`](crate::app::quit)) as the app's own Quit does — without it the
//! process would die at once, its operations' helper pods and tool processes left running.
//!
//! As in the CLI: the first signal starts the quit; a second one, while the quit waits for the
//! running operations, ends the process at once with `128 + signal`, from the signal handler
//! itself, whatever any thread is doing. A signal the process started with ignored (`nohup`,
//! a background job of a non-interactive shell) stays ignored.
//!
//! Windows has no such signals for a GUI app: a logoff or a shutdown reaches it as
//! `WM_ENDSESSION`, which [`Shell::on_exit`](crate::app::Shell::on_exit) handles.

#[cfg(unix)]
pub use unix::{on_quit_signals, QUIT_SIGNALS};

#[cfg(unix)]
mod unix {
    use std::io;
    use std::os::raw::c_int;
    use std::sync::atomic::{AtomicBool, Ordering::SeqCst};
    use std::sync::Arc;
    use std::thread;

    use signal_hook::consts::{SIGHUP, SIGINT, SIGTERM};
    use signal_hook::iterator::Signals;
    use signal_hook::low_level;

    /// The signals that quit the app.
    pub const QUIT_SIGNALS: [c_int; 3] = [SIGTERM, SIGHUP, SIGINT];

    /// Handle `signals` (the app passes [`QUIT_SIGNALS`]), each one the process did not start
    /// with ignored: the first to arrive calls `on_quit` with it, on a thread of its own
    /// (`quit-signal`); any later one ends the process at once (see the module docs). Call it
    /// once. An `Err` leaves those signals as they were: the app runs on without them, and a
    /// signal kills it as before.
    pub fn on_quit_signals(
        signals: &[c_int],
        on_quit: impl FnOnce(c_int) + Send + 'static,
    ) -> io::Result<()> {
        let signals: Vec<c_int> = signals
            .iter()
            .copied()
            .filter(|&signal| !ignored(signal))
            .collect();
        if signals.is_empty() {
            return Ok(());
        }
        let seen = Arc::new(AtomicBool::new(false));
        for &signal in &signals {
            let seen = Arc::clone(&seen);
            // SAFETY: the action runs inside the signal handler and does only what is
            // async-signal-safe there: an atomic swap, and `_exit(2)`.
            unsafe {
                low_level::register(signal, move || {
                    on_signal(&seen, signal, |code| low_level::exit(code))
                })?;
            }
        }
        // Registered after the actions above, so it runs after them (signal-hook keeps the
        // order): a second signal never reaches it, the process has ended by then.
        let mut arrivals = Signals::new(&signals)?;
        thread::Builder::new()
            .name("quit-signal".into())
            .spawn(move || {
                if let Some(signal) = arrivals.forever().next() {
                    tracing::info!(signal, "quitting on a signal");
                    on_quit(signal);
                }
            })?;
        Ok(())
    }

    /// In the handler, for every quit signal: the first only marks that one came (the quit
    /// thread takes it from there); a later one calls `exit` with `128 + signal`, the code a
    /// shell reports for a death by that signal.
    pub(super) fn on_signal(seen: &AtomicBool, signal: c_int, exit: impl FnOnce(c_int)) {
        if seen.swap(true, SeqCst) {
            exit(128 + signal);
        }
    }

    /// Whether `signal` is ignored in this process right now — before the handlers are
    /// installed, that is how it was started.
    fn ignored(signal: c_int) -> bool {
        // SAFETY: sigaction with a null new action only reads the current one into `old`,
        // a plain, zero-initialised struct.
        unsafe {
            let mut old: libc::sigaction = std::mem::zeroed();
            libc::sigaction(signal, std::ptr::null(), &mut old) == 0
                && old.sa_sigaction == libc::SIG_IGN
        }
    }

    #[cfg(test)]
    mod tests {
        use std::sync::atomic::AtomicBool;
        use std::sync::{mpsc, Mutex};
        use std::time::Duration;

        use signal_hook::consts::{SIGHUP, SIGINT, SIGTERM, SIGUSR1, SIGUSR2};
        use signal_hook::low_level;

        use super::{ignored, on_quit_signals, on_signal, QUIT_SIGNALS};

        #[test]
        fn the_quit_signals_are_term_hup_and_int() {
            assert_eq!(QUIT_SIGNALS, [SIGTERM, SIGHUP, SIGINT]);
        }

        #[test]
        fn the_first_signal_only_marks_itself_and_a_second_of_any_kind_exits() {
            let seen = AtomicBool::new(false);
            let exits = Mutex::new(Vec::new());
            on_signal(&seen, SIGTERM, |code| exits.lock().unwrap().push(code));
            assert!(
                exits.lock().unwrap().is_empty(),
                "the first starts the quit"
            );
            on_signal(&seen, SIGINT, |code| exits.lock().unwrap().push(code));
            on_signal(&seen, SIGHUP, |code| exits.lock().unwrap().push(code));
            assert_eq!(*exits.lock().unwrap(), vec![128 + SIGINT, 128 + SIGHUP]);
            assert_eq!(128 + SIGINT, 130, "as a shell reports Ctrl-C");
        }

        /// A real signal, raised in this test process only, and once: a second would end it.
        /// SIGUSR1 is one nothing else in the test binary uses.
        #[test]
        fn a_signal_raised_once_reaches_on_quit_on_its_own_thread() {
            let (tx, rx) = mpsc::channel();
            on_quit_signals(&[SIGUSR1], move |signal| {
                let name = std::thread::current().name().map(str::to_owned);
                let _ = tx.send((signal, name));
            })
            .unwrap();
            low_level::raise(SIGUSR1).unwrap();
            let (signal, thread) = rx
                .recv_timeout(Duration::from_secs(10))
                .expect("on_quit was called");
            assert_eq!(signal, SIGUSR1);
            assert_eq!(thread.as_deref(), Some("quit-signal"));
        }

        #[test]
        fn a_signal_the_process_ignores_stays_ignored() {
            // SIGUSR2, which nothing else in the test binary uses, set to ignored as `nohup`
            // leaves SIGHUP.
            // SAFETY: setting a disposition to SIG_IGN installs no handler.
            unsafe {
                libc::signal(SIGUSR2, libc::SIG_IGN);
            }
            assert!(ignored(SIGUSR2));
            assert!(!ignored(SIGUSR1), "SIGUSR1 is not ignored");
            let (tx, rx) = mpsc::channel();
            on_quit_signals(&[SIGUSR2], move |signal| {
                let _ = tx.send(signal);
            })
            .unwrap();
            // Still ignored: raising it does not reach on_quit. Once only: were it handled,
            // a second would end the test process.
            low_level::raise(SIGUSR2).unwrap();
            assert!(rx.recv_timeout(Duration::from_millis(200)).is_err());
            assert!(ignored(SIGUSR2));
        }
    }
}
