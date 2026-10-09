// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! `apprafter doctor` — the CLI over `apprafter_core::doctor` (ADR 0067). The checks, their
//! order and their outcomes live in the core and are shared with the desktop's doctor overlay;
//! this module logs the invocation, builds the context, prints the report through
//! `render::doctor` and owns the exit-code policy: 1 when any check FAILs, else 0 (a CI gate
//! can run `apprafter doctor` directly).
//!
//! # Ctrl-C
//!
//! The core starts every tool probe and every `kubectl` in a session (Unix) or Job Object
//! (Windows) of its own, so the terminal's Ctrl-C reaches none of them: only cancelling the
//! run's token kills them. So the arm turns the first SIGINT or SIGTERM (Windows: Ctrl-C,
//! Ctrl-Break, the console closing) into `CancellationToken::cancel`, from a thread of its own
//! and never from inside a signal handler; the core, which checks the token between its steps
//! and every 50 ms while it waits on a probe or a DNS lookup, then kills what it started,
//! removes its kubeconfig copy and returns `Cancelled`, and doctor exits with 128 + the signal
//! (130 for Ctrl-C; 130 on Windows) without a report. A second signal ends the process at once,
//! as in `helper_interrupt`. A signal the process was started with ignored (a background job's
//! SIGINT) stays ignored. The console closing on Windows is held while the run unwinds
//! (`windows_console`).

use std::io::Write as _;
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::Arc;

use apprafter_core::doctor::{self, DoctorArgs, DoctorTarget};
use apprafter_core::{CancellationToken, Context, CoreError};
use cli_core::CliError;
use tracing::info;

use crate::commands::state_paths::resolve_state_paths;
use crate::context::cli_context;
use crate::render;

pub fn run(target_override: Option<&str>, no_ping: bool) -> miette::Result<()> {
    info!(target_override, no_ping, "doctor invoked");
    let cancel = CancellationToken::new();
    let interrupt = cancel_on_interrupt(&cancel);
    let ctx = cli_context()?.with_no_ping(no_ping);
    migrate_legacy_state(&ctx, target_override).map_err(miette::Report::new)?;
    let target = match target_override {
        Some(name) => DoctorTarget::Named(name.to_string()),
        None => DoctorTarget::CliDefault,
    };
    let report = match doctor::run(
        &ctx,
        DoctorArgs { target },
        &render::reporter::CliReporter,
        &cancel,
    ) {
        Ok(report) => report,
        Err(CoreError::Cancelled) => {
            // The same rendering `main` gives an error, then the signal's exit code.
            let _ = writeln!(
                std::io::stderr(),
                "Error: {:?}",
                render::core_error::report(CoreError::Cancelled)
            );
            std::process::exit(interrupt.code());
        }
        Err(e) => return Err(render::core_error::report(e)),
    };
    print!("{}", render::doctor::render(&report));
    let _ = std::io::stdout().flush();
    if report.has_failures() {
        std::process::exit(1);
    }
    Ok(())
}

/// The one-shot `<cwd>/.apprafter/state.json` migration every state-reading command runs first
/// (CLI-only, spec §3.1; overview §3.11): the core reads state and never runs it. Only for a
/// target that exists: doctor reports a missing one as a row and must not create a state
/// directory for it (deviation 3).
fn migrate_legacy_state(ctx: &Context, target_override: Option<&str>) -> cli_core::Result<()> {
    let Some(name) = cli_core::resolve_active_target_name(&ctx.store(), target_override)? else {
        return Ok(());
    };
    // `Some(name)`: the existence check runs for the CLI default too, so a dangling pointer
    // migrates nothing.
    match resolve_state_paths(Some(&name)) {
        Ok(_) | Err(CliError::TargetNotFound { .. }) => Ok(()),
        Err(e) => Err(e),
    }
}

/// The exit code of the interrupt that cancelled the run.
struct Interrupt(Arc<AtomicI32>);

impl Interrupt {
    /// 128 + the signal; 130 when none was recorded (a console event on Windows).
    fn code(&self) -> i32 {
        match self.0.load(Ordering::SeqCst) {
            0 => INTERRUPTED_EXIT,
            code => code,
        }
    }
}

/// What Ctrl-C exits with: 128 + SIGINT on Unix, and the same on Windows.
const INTERRUPTED_EXIT: i32 = 130;

/// Cancel `cancel` on the first SIGINT or SIGTERM (see the module docs). A failure to install
/// the handler is a warning: doctor still runs, and a Ctrl-C then ends it the default way.
#[cfg(unix)]
fn cancel_on_interrupt(cancel: &CancellationToken) -> Interrupt {
    let code = Arc::new(AtomicI32::new(0));
    if let Err(e) = register(cancel, &code) {
        let _ = writeln!(
            std::io::stderr(),
            "warning: cannot handle Ctrl-C ({e}); an interrupted run leaves the tool it was \
             probing running until its own timeout"
        );
    }
    Interrupt(code)
}

#[cfg(unix)]
fn register(cancel: &CancellationToken, code: &Arc<AtomicI32>) -> std::io::Result<()> {
    use signal_hook::consts::{SIGINT, SIGTERM};
    use std::sync::atomic::AtomicBool;

    let signals: Vec<i32> = [SIGINT, SIGTERM]
        .into_iter()
        .filter(|&s| !crate::commands::helper_interrupt::ignored_at_start(s))
        .collect();
    if signals.is_empty() {
        return Ok(());
    }
    let seen = Arc::new(AtomicBool::new(false));
    for &signal in &signals {
        // In this order: the first signal finds `seen` unset and only sets it; a second finds it
        // set and ends the process from the handler, whatever the run is doing.
        signal_hook::flag::register_conditional_shutdown(signal, 128 + signal, Arc::clone(&seen))?;
        signal_hook::flag::register(signal, Arc::clone(&seen))?;
    }
    let mut iterator = signal_hook::iterator::Signals::new(&signals)?;
    let (cancel, code) = (cancel.clone(), Arc::clone(code));
    std::thread::Builder::new()
        .name("doctor-interrupt".into())
        .spawn(move || {
            if let Some(signal) = iterator.forever().next() {
                code.store(128 + signal, Ordering::SeqCst);
                cancel.cancel();
            }
        })?;
    Ok(())
}

/// Windows: a console control handler (see `windows_console`).
#[cfg(windows)]
fn cancel_on_interrupt(cancel: &CancellationToken) -> Interrupt {
    let code = Arc::new(AtomicI32::new(0));
    if let Err(e) = windows_console::register(cancel, &code) {
        let _ = writeln!(
            std::io::stderr(),
            "warning: cannot handle Ctrl-C ({e}); an interrupted run leaves the tool it was \
             probing running until its own timeout"
        );
    }
    Interrupt(code)
}

/// Windows: one console control handler, running on a thread the system creates per event.
///
/// - Ctrl-C and Ctrl-Break cancel the run and return: the process carries on, the core sees the
///   token within 50 ms, ends the probe's Job Object and removes its kubeconfig copy, and doctor
///   exits 130. A second one ends the process at once.
/// - The console closing (and logoff and shutdown, which reach a console process only when it
///   runs as a service) cancel the run too, but Windows ends the process as soon as the handler
///   returns. So the handler holds the event for [`CLOSE_HOLD`] while the run unwinds; the
///   arm's own exit ends the process, and the hold with it, once the run has unwound. Whatever
///   is still running when Windows ends the process dies with it (the core's jobs are
///   KILL_ON_JOB_CLOSE); a kubeconfig copy of a run that did not unwind within the hold is
///   swept by a later run (overview R9).
#[cfg(windows)]
mod windows_console {
    use std::sync::atomic::{AtomicI32, AtomicU32, Ordering};
    use std::sync::{Arc, OnceLock};
    use std::time::Duration;

    use apprafter_core::CancellationToken;
    use windows_sys::core::BOOL;
    use windows_sys::Win32::Foundation::{FALSE, TRUE};
    use windows_sys::Win32::System::Console::{
        SetConsoleCtrlHandler, CTRL_BREAK_EVENT, CTRL_CLOSE_EVENT, CTRL_C_EVENT, CTRL_LOGOFF_EVENT,
        CTRL_SHUTDOWN_EVENT,
    };

    use super::INTERRUPTED_EXIT;

    /// How long a closing console is held for the run to unwind: well past an unwind (the
    /// token seen within 50 ms, the job ended, the copy removed), and well inside the 5 s
    /// Windows gives a handler before it ends the process regardless.
    pub(super) const CLOSE_HOLD: Duration = Duration::from_secs(2);

    /// What the handler acts on (a plain function, no state of its own).
    pub(super) struct Run {
        cancel: CancellationToken,
        code: Arc<AtomicI32>,
        /// Ctrl-C and Ctrl-Break so far: the first cancels, a later one ends the process.
        interrupts: AtomicU32,
    }

    static RUN: OnceLock<Run> = OnceLock::new();

    pub(super) fn register(
        cancel: &CancellationToken,
        code: &Arc<AtomicI32>,
    ) -> std::io::Result<()> {
        let _ = RUN.set(Run::new(cancel, code));
        // SAFETY: registers a plain `extern "system"` function for the life of the process;
        // it is never removed.
        if unsafe { SetConsoleCtrlHandler(Some(handler), TRUE) } == 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }

    impl Run {
        pub(super) fn new(cancel: &CancellationToken, code: &Arc<AtomicI32>) -> Run {
            Run {
                cancel: cancel.clone(),
                code: Arc::clone(code),
                interrupts: AtomicU32::new(0),
            }
        }
    }

    /// What an event does (module docs).
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(super) enum Action {
        /// Not one of ours: the next handler, or the default one, takes it.
        Pass,
        /// Cancel the run and return; the process carries on.
        Cancel,
        /// Cancel the run, then hold the event while it unwinds, at most [`CLOSE_HOLD`].
        CancelAndHold,
        /// End the process at once.
        ExitAtOnce,
    }

    /// The action for `ctrl_type`, `earlier` Ctrl-C or Ctrl-Break events having come before it.
    pub(super) fn action(ctrl_type: u32, earlier: u32) -> Action {
        match ctrl_type {
            CTRL_C_EVENT | CTRL_BREAK_EVENT if earlier == 0 => Action::Cancel,
            CTRL_C_EVENT | CTRL_BREAK_EVENT => Action::ExitAtOnce,
            CTRL_CLOSE_EVENT | CTRL_LOGOFF_EVENT | CTRL_SHUTDOWN_EVENT => Action::CancelAndHold,
            _ => Action::Pass,
        }
    }

    unsafe extern "system" fn handler(ctrl_type: u32) -> BOOL {
        match RUN.get() {
            Some(run) => respond(run, ctrl_type),
            None => FALSE,
        }
    }

    /// Act on `ctrl_type` for `run`: what [`handler`] returns to Windows.
    pub(super) fn respond(run: &Run, ctrl_type: u32) -> BOOL {
        let earlier = if matches!(ctrl_type, CTRL_C_EVENT | CTRL_BREAK_EVENT) {
            run.interrupts.fetch_add(1, Ordering::SeqCst)
        } else {
            0
        };
        match action(ctrl_type, earlier) {
            Action::Pass => FALSE,
            Action::ExitAtOnce => exit_at_once(INTERRUPTED_EXIT),
            Action::Cancel => {
                cancel(run);
                TRUE
            }
            Action::CancelAndHold => {
                cancel(run);
                // Returning lets Windows end the process: hold it while the run unwinds. The
                // arm exits once it has, which ends this thread too.
                std::thread::sleep(CLOSE_HOLD);
                TRUE
            }
        }
    }

    fn cancel(run: &Run) {
        run.code.store(INTERRUPTED_EXIT, Ordering::SeqCst);
        // A panicking cancel callback must not unwind out of an `extern "system"` fn.
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run.cancel.cancel()));
    }

    /// End the process at once, running nothing else (the second event).
    fn exit_at_once(code: i32) -> ! {
        use windows_sys::Win32::System::Threading::{GetCurrentProcess, TerminateProcess};
        // SAFETY: GetCurrentProcess returns a pseudo-handle that needs no closing;
        // TerminateProcess on it does not return when it succeeds.
        unsafe {
            TerminateProcess(GetCurrentProcess(), code as u32);
        }
        std::process::exit(code)
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::time::Instant;

        /// Review findings 1 and 6: every event's action. The console closing, logoff and
        /// shutdown hold (Windows ends the process when the handler returns), whatever came
        /// before; Ctrl-C and Ctrl-Break cancel once and end the process the second time.
        #[test]
        fn each_console_event_maps_to_its_action() {
            for (ctrl_type, earlier, expected) in [
                (CTRL_C_EVENT, 0, Action::Cancel),
                (CTRL_BREAK_EVENT, 0, Action::Cancel),
                (CTRL_C_EVENT, 1, Action::ExitAtOnce),
                (CTRL_BREAK_EVENT, 1, Action::ExitAtOnce),
                (CTRL_BREAK_EVENT, 7, Action::ExitAtOnce),
                (CTRL_CLOSE_EVENT, 0, Action::CancelAndHold),
                (CTRL_CLOSE_EVENT, 1, Action::CancelAndHold),
                (CTRL_LOGOFF_EVENT, 0, Action::CancelAndHold),
                (CTRL_SHUTDOWN_EVENT, 0, Action::CancelAndHold),
                (3, 0, Action::Pass),
                (4, 0, Action::Pass),
                (99, 0, Action::Pass),
            ] {
                assert_eq!(
                    action(ctrl_type, earlier),
                    expected,
                    "event {ctrl_type} after {earlier} interrupt(s)"
                );
            }
        }

        /// The console closing: the run is cancelled at once (the core starts unwinding while
        /// the event is held), and the handler returns only after [`CLOSE_HOLD`], by which time
        /// the arm's own exit would have ended the process.
        #[test]
        fn a_closing_console_cancels_at_once_and_is_held_while_the_run_unwinds() {
            let cancel = CancellationToken::new();
            let code = Arc::new(AtomicI32::new(0));
            let run = Run::new(&cancel, &code);
            let started = Instant::now();
            let watch = cancel.clone();
            let seen = std::thread::spawn(move || {
                while !watch.is_cancelled() {
                    std::thread::sleep(Duration::from_millis(5));
                }
                Instant::now()
            });
            assert_eq!(respond(&run, CTRL_CLOSE_EVENT), TRUE);
            let held = started.elapsed();
            let cancelled_after = seen.join().unwrap() - started;
            assert!(
                cancelled_after < Duration::from_millis(500),
                "cancelled only after {cancelled_after:?}"
            );
            assert!(held >= CLOSE_HOLD, "returned after {held:?}");
            assert_eq!(code.load(Ordering::SeqCst), INTERRUPTED_EXIT);
        }

        /// Ctrl-C (and Ctrl-Break): the run is cancelled and the handler returns at once, so
        /// the process carries on to exit 130 itself.
        #[test]
        fn a_first_interrupt_cancels_and_returns_at_once() {
            for ctrl_type in [CTRL_C_EVENT, CTRL_BREAK_EVENT] {
                let cancel = CancellationToken::new();
                let code = Arc::new(AtomicI32::new(0));
                let run = Run::new(&cancel, &code);
                let started = Instant::now();
                assert_eq!(respond(&run, ctrl_type), TRUE);
                assert!(started.elapsed() < Duration::from_millis(500));
                assert!(cancel.is_cancelled());
                assert_eq!(code.load(Ordering::SeqCst), INTERRUPTED_EXIT);
            }
        }

        /// An event that is not ours is passed on, and touches nothing.
        #[test]
        fn another_event_is_passed_on() {
            let cancel = CancellationToken::new();
            let run = Run::new(&cancel, &Arc::new(AtomicI32::new(0)));
            assert_eq!(respond(&run, 99), FALSE);
            assert!(!cancel.is_cancelled());
        }
    }
}
