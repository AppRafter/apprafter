// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The process plumbing that exists before the app does: the async runtime commands run on, the
//! one TLS crypto provider, and the log.
//!
//! The core is synchronous and deep (GOTCHA-67), and every command runs it on the runtime's
//! blocking pool, so every runtime thread gets the 8 MiB the operation threads get. Tauri
//! builds a default runtime the first time anything asks for one, which is why
//! [`init_runtime`] runs before the app is built.
//!
//! The log goes to a file in the app's log directory, which follows the data-directory
//! override (`APPRAFTER_DESKTOP_DATA_DIR`), and nowhere else in a release build. The desktop
//! never calls `cli_core::logging::init`: that installs the CLI's own global subscriber.

use std::io;
use std::panic;
use std::path::Path;
use std::sync::OnceLock;

use tracing::Subscriber;
use tracing_appender::rolling::{RollingFileAppender, Rotation};
use tracing_subscriber::filter::{LevelFilter, Targets};
use tracing_subscriber::fmt::format::{DefaultFields, Format};
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::util::SubscriberInitExt;

/// The stack of every async-runtime thread, workers and the blocking pool alike.
pub const THREAD_STACK_BYTES: usize = 8 << 20;

/// Log files are `apprafter-desktop.<yyyy-mm-dd>.log`.
const LOG_PREFIX: &str = "apprafter-desktop";
const LOG_SUFFIX: &str = "log";
/// How many daily files are kept; the oldest goes when a new day starts.
const LOG_FILES_KEPT: usize = 14;

/// The crates whose debug lines a debug build logs: the desktop's own and the core it runs.
/// Every other crate (Tauri, WebKitGTK's bindings, zbus) logs from `info` up in every build.
pub const OWN_CRATES: [&str; 6] = [
    "apprafter_desktop",
    "apprafter_os_auth",
    "apprafter_core",
    "cli_core",
    "cli_providers",
    "backup_core",
];

/// Kept for the process: Tauri holds only a handle.
static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();

/// Build the multi-thread tokio runtime with [`THREAD_STACK_BYTES`] stacks and make it Tauri's
/// async runtime. Must run before anything touches `tauri::async_runtime` — Tauri panics when
/// a runtime is set after its default one exists — so `run` calls it first. A second call
/// does nothing.
pub fn init_runtime() -> io::Result<()> {
    if RUNTIME.get().is_some() {
        return Ok(());
    }
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_name("tokio-runtime")
        .thread_stack_size(THREAD_STACK_BYTES)
        .build()?;
    let handle = runtime.handle().clone();
    if RUNTIME.set(runtime).is_ok() {
        tauri::async_runtime::set(handle);
    }
    Ok(())
}

/// Make ring the process's rustls provider. An `Err` means one is installed already, which is
/// all this wants.
pub fn install_crypto() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

/// Log to a daily file in `log_dir` (created when missing), and in a debug build to stderr
/// too; see [`log_filter`] for the levels. Panics are logged before the previous hook runs.
///
/// An `Err` leaves the process without a log, never without the app: the caller reports it
/// and goes on.
pub fn init_logging(log_dir: &Path) -> Result<(), String> {
    // Before the appender: it prunes old files from the directory before it creates it, and
    // prints an error on a first start.
    std::fs::create_dir_all(log_dir).map_err(|e| format!("{}: {e}", log_dir.display()))?;
    let file = RollingFileAppender::builder()
        .rotation(Rotation::DAILY)
        .filename_prefix(LOG_PREFIX)
        .filename_suffix(LOG_SUFFIX)
        .max_log_files(LOG_FILES_KEPT)
        .build(log_dir)
        .map_err(|e| format!("{}: {e}", log_dir.display()))?;
    let debug = cfg!(debug_assertions);
    let to_file = file_layer(file);
    let to_stderr = debug.then(|| tracing_subscriber::fmt::layer().with_writer(io::stderr));
    tracing_subscriber::registry()
        .with(log_filter(debug))
        .with(to_file)
        .with(to_stderr)
        .try_init()
        .map_err(|e| e.to_string())?;
    log_panics();
    Ok(())
}

/// The log file's lines: plain text, without colour codes. The tests read lines back in this
/// format ([`logged`]).
fn file_layer<S, W>(writer: W) -> tracing_subscriber::fmt::Layer<S, DefaultFields, Format, W>
where
    S: Subscriber + for<'a> LookupSpan<'a>,
    W: for<'w> MakeWriter<'w> + 'static,
{
    tracing_subscriber::fmt::layer()
        .with_writer(writer)
        .with_ansi(false)
}

/// What a release build's log file would hold of the lines `run` logs on this thread: the
/// release filter ([`log_filter`]) and the file's format ([`file_layer`]).
#[cfg(test)]
pub(crate) fn logged(run: impl FnOnce()) -> String {
    use std::sync::{Arc, Mutex, PoisonError};

    /// Appends to the shared buffer.
    struct Lines(Arc<Mutex<Vec<u8>>>);

    impl io::Write for Lines {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            let mut lines = self.0.lock().unwrap_or_else(PoisonError::into_inner);
            lines.extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    let lines = Arc::new(Mutex::new(Vec::new()));
    let writer = {
        let lines = lines.clone();
        move || Lines(lines.clone())
    };
    let subscriber = tracing_subscriber::registry()
        .with(log_filter(false))
        .with(file_layer(writer));
    tracing::subscriber::with_default(subscriber, run);
    let bytes = lines.lock().unwrap_or_else(PoisonError::into_inner).clone();
    String::from_utf8(bytes).expect("the log is UTF-8")
}

/// `info` and up for everything; in a debug build, `debug` and up for [`OWN_CRATES`].
pub fn log_filter(debug: bool) -> Targets {
    let filter = Targets::new().with_default(LevelFilter::INFO);
    if debug {
        filter.with_targets(OWN_CRATES.map(|krate| (krate, LevelFilter::DEBUG)))
    } else {
        filter
    }
}

/// Log every panic (thread name and message) before the previous hook prints it as before.
fn log_panics() {
    let previous = panic::take_hook();
    panic::set_hook(Box::new(move |info| {
        let thread = std::thread::current();
        tracing::error!(
            thread = thread.name().unwrap_or("<unnamed>"),
            "panic: {info}"
        );
        previous(info);
    }));
}

#[cfg(test)]
mod tests {
    use tracing::Level;

    use super::{log_filter, OWN_CRATES};

    #[test]
    fn a_release_logs_info_and_up_for_every_crate() {
        let filter = log_filter(false);
        for target in ["apprafter_desktop", "apprafter_core::ops", "zbus", "tauri"] {
            assert!(filter.would_enable(target, &Level::INFO), "{target}");
            assert!(!filter.would_enable(target, &Level::DEBUG), "{target}");
        }
    }

    #[test]
    fn a_debug_build_adds_debug_lines_for_its_own_crates_only() {
        let filter = log_filter(true);
        for krate in OWN_CRATES {
            let module = format!("{krate}::some::module");
            assert!(filter.would_enable(&module, &Level::DEBUG), "{module}");
            assert!(!filter.would_enable(&module, &Level::TRACE), "{module}");
        }
        for target in ["zbus::connection", "tauri::manager", "tao"] {
            assert!(filter.would_enable(target, &Level::INFO), "{target}");
            assert!(!filter.would_enable(target, &Level::DEBUG), "{target}");
        }
    }

    /// The desktop's own OS-authentication crate is one of its own: its debug lines (a signal
    /// the session watch dropped) show in `just desktop-dev`.
    #[test]
    fn a_debug_build_logs_the_os_auth_crate_s_debug_lines() {
        let target = "apprafter_os_auth::session::linux";
        assert!(log_filter(true).would_enable(target, &Level::DEBUG));
        assert!(!log_filter(false).would_enable(target, &Level::DEBUG));
    }
}
