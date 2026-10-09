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
//! and never from inside a signal handler; the core then kills what it started and returns
//! `Cancelled`, and doctor exits with 128 + the signal (130 for Ctrl-C; 130 on Windows) without
//! a report. A second signal ends the process at once, as in `helper_interrupt`. A signal the
//! process was started with ignored (a background job's SIGINT) stays ignored.

use std::io::Write as _;
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::Arc;

use apprafter_core::doctor::{self, DoctorArgs, DoctorTarget};
use apprafter_core::{CancellationToken, CoreError};
use tracing::info;

use crate::context::cli_context;
use crate::render;

pub fn run(target_override: Option<&str>, no_ping: bool) -> miette::Result<()> {
    info!(target_override, no_ping, "doctor invoked");
    let cancel = CancellationToken::new();
    let interrupt = cancel_on_interrupt(&cancel);
    let ctx = cli_context()?.with_no_ping(no_ping);
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

/// Windows: a console control handler for Ctrl-C, Ctrl-Break and the console closing. It runs
/// on a thread the system creates for the event, so it cancels the token itself: the core's
/// cancel callbacks terminate the Job Objects of the children it started, at once.
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

#[cfg(windows)]
mod windows_console {
    use std::sync::atomic::{AtomicI32, AtomicU32, Ordering};
    use std::sync::{Arc, OnceLock};

    use apprafter_core::CancellationToken;
    use windows_sys::core::BOOL;
    use windows_sys::Win32::Foundation::{FALSE, TRUE};
    use windows_sys::Win32::System::Console::{
        SetConsoleCtrlHandler, CTRL_BREAK_EVENT, CTRL_CLOSE_EVENT, CTRL_C_EVENT,
    };

    use super::INTERRUPTED_EXIT;

    /// The run's token and exit code, for the handler (a plain function, no state of its own).
    static RUN: OnceLock<(CancellationToken, Arc<AtomicI32>)> = OnceLock::new();
    /// Console events so far: the first cancels, a later one ends the process at once.
    static EVENTS: AtomicU32 = AtomicU32::new(0);

    pub(super) fn register(
        cancel: &CancellationToken,
        code: &Arc<AtomicI32>,
    ) -> std::io::Result<()> {
        let _ = RUN.set((cancel.clone(), Arc::clone(code)));
        // SAFETY: registers a plain `extern "system"` function for the life of the process;
        // it is never removed.
        if unsafe { SetConsoleCtrlHandler(Some(handler), TRUE) } == 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }

    /// Logoff and shutdown are left to the default handling (they reach services only).
    unsafe extern "system" fn handler(ctrl_type: u32) -> BOOL {
        if !matches!(
            ctrl_type,
            CTRL_C_EVENT | CTRL_BREAK_EVENT | CTRL_CLOSE_EVENT
        ) {
            return FALSE;
        }
        if EVENTS.fetch_add(1, Ordering::SeqCst) > 0 {
            exit_at_once(INTERRUPTED_EXIT);
        }
        if let Some((cancel, code)) = RUN.get() {
            code.store(INTERRUPTED_EXIT, Ordering::SeqCst);
            // A panicking cancel callback must not unwind out of an `extern "system"` fn.
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| cancel.cancel()));
        }
        // Ctrl-C and Ctrl-Break: handled, the run returns `Cancelled` and exits 130. The
        // console closing: Windows ends the process when this returns, after the cancel above
        // has terminated the children's Job Objects.
        TRUE
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
}
