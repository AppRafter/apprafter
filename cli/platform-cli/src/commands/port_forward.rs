// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Shared kubectl port-forward + browser-open helpers.
//!
//! Two callers consume this module:
//!
//!   * `commands::open::argocd` — `apprafter open argocd`
//!     (well-known target svc/argocd-server in `argocd`
//!     namespace).
//!   * `commands::app_open::open` — `apprafter app open <name>`
//!     (dynamic target resolved from the Application CR's
//!     destination namespace and child Service).
//!
//! Walk-fix #1 post-B.1.79 is baked into `wait_ready`:
//! kubectl is a Go binary, and Go's default SIGPIPE handler
//! terminates the process on the next write to a closed
//! stdout pipe. The drainer threads must outlive
//! `wait_ready`'s return and keep reading until the child
//! exits naturally — closing the read end too early kills
//! the port-forward.
//!
//! All helpers are blocking; the caller's main thread is
//! expected to `child.wait()` after `wait_ready` returns,
//! tearing down on Ctrl+C through the process group.

use std::io::{BufRead, BufReader, Read};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::thread;

use cli_core::{CliError, Result};

/// Spawn `kubectl port-forward <target> -n <namespace>
/// <local>:<remote>` with piped stdout/stderr (the drainer
/// threads in `wait_ready` consume both).
///
/// `target` is the kubectl resource selector verbatim — e.g.
/// `"svc/argocd-server"`, `"svc/landing-web"`,
/// `"deployment/foo"`. The caller composes the right shape;
/// this helper just shells out.
pub fn spawn_kubectl_port_forward(
    target: &str,
    namespace: &str,
    local_port: u16,
    remote_port: u16,
    kubeconfig_path: &Path,
) -> Result<Child> {
    let child = Command::new("kubectl")
        .arg("port-forward")
        .arg(target)
        .arg("-n")
        .arg(namespace)
        .arg(format!("{local_port}:{remote_port}"))
        .env("KUBECONFIG", kubeconfig_path)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| CliError::Other(format!("spawn kubectl port-forward: {e}")))?;
    Ok(child)
}

/// Wait for kubectl's `Forwarding from …` ready banner on
/// stdout AND keep both pipes drained for the lifetime of
/// the child.
///
/// **Walk-fix #1 post-B.1.79.** The first implementation
/// closed stdout the moment it saw the ready line (BufReader
/// dropped + ChildStdout dropped + read-end of the pipe
/// closed). kubectl is a Go binary; Go's default SIGPIPE
/// handler terminates the process on the next write to a
/// closed stdout pipe, so kubectl exited within milliseconds
/// — well before the operator could use the forward.
///
/// Fix: spawn one drainer thread per pipe. The stdout drainer
/// signals readiness through a one-shot channel when it sees
/// the banner and continues reading until EOF, throwing the
/// bytes away. The stderr drainer just reads and discards.
/// Both threads outlive this function's return; they exit
/// naturally when the child closes its pipes on shutdown.
pub fn wait_ready(child: &mut Child) -> Result<()> {
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| CliError::Other("kubectl port-forward has no stdout".into()))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| CliError::Other("kubectl port-forward has no stderr".into()))?;

    let rx = spawn_ready_drainer(stdout);
    spawn_silent_drainer(stderr);

    rx.recv().map_err(|_| {
        CliError::Other("kubectl port-forward exited before binding local port".into())
    })
}

/// Spawn a thread that reads stdout line-by-line, signals
/// readiness on the first `Forwarding from …` line, then
/// keeps draining silently until EOF.
fn spawn_ready_drainer<R: Read + Send + 'static>(reader: R) -> mpsc::Receiver<()> {
    let (tx, rx) = mpsc::sync_channel::<()>(1);
    thread::spawn(move || {
        let buf = BufReader::new(reader);
        let mut signaled = false;
        for line in buf.lines() {
            let Ok(line) = line else { break };
            if !signaled && line.contains("Forwarding from") {
                let _ = tx.send(());
                signaled = true;
            }
        }
    });
    rx
}

/// Spawn a thread that silently drains a pipe to EOF.
fn spawn_silent_drainer<R: Read + Send + 'static>(reader: R) {
    thread::spawn(move || {
        let mut buf = BufReader::new(reader);
        let mut sink = Vec::with_capacity(256);
        loop {
            sink.clear();
            match buf.read_until(b'\n', &mut sink) {
                Ok(0) => break,
                Ok(_) => continue,
                Err(_) => break,
            }
        }
    });
}

/// Open `url` in the operator's default browser. Cross-
/// platform shellout — `xdg-open` on Linux, `open` on
/// macOS, `cmd /c start` on Windows. Failures fall through
/// quietly: the URL is already printed to stdout, so the
/// operator can paste it manually.
pub fn open_in_browser(url: &str) -> Result<()> {
    let (program, args): (&str, Vec<String>) = if cfg!(target_os = "macos") {
        ("open", vec![url.to_string()])
    } else if cfg!(target_os = "windows") {
        ("cmd", vec!["/c".into(), "start".into(), url.to_string()])
    } else {
        ("xdg-open", vec![url.to_string()])
    };
    Command::new(program)
        .args(&args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| CliError::Other(format!("spawn browser: {e}")))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    #[test]
    fn ready_drainer_signals_on_forwarding_line() {
        let stream = b"Forwarding from 127.0.0.1:8080 -> 443\n".to_vec();
        let rx = spawn_ready_drainer(Cursor::new(stream));
        rx.recv_timeout(Duration::from_secs(1))
            .expect("ready signal must arrive");
    }

    #[test]
    fn ready_drainer_continues_draining_after_signal() {
        // Real kubectl emits stdout after the ready banner
        // (IPv6 binding line, per-connection lines). If we
        // close the pipe right after signaling, kubectl
        // SIGPIPEs and exits. Test asserts the drainer reads
        // ALL bytes — guards against future "close on first
        // match" regressions.
        struct Tracker {
            inner: Cursor<Vec<u8>>,
            consumed: Arc<Mutex<usize>>,
        }
        impl Read for Tracker {
            fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                let n = self.inner.read(buf)?;
                *self.consumed.lock().unwrap() += n;
                Ok(n)
            }
        }

        let stream = b"Forwarding from 127.0.0.1:8080 -> 443\n\
                       Forwarding from [::1]:8080 -> 443\n\
                       Handling connection for 8080\n"
            .to_vec();
        let total = stream.len();
        let consumed = Arc::new(Mutex::new(0usize));
        let tracker = Tracker {
            inner: Cursor::new(stream),
            consumed: Arc::clone(&consumed),
        };

        let rx = spawn_ready_drainer(tracker);
        rx.recv_timeout(Duration::from_secs(1))
            .expect("ready signal must arrive");

        for _ in 0..100 {
            if *consumed.lock().unwrap() >= total {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(
            *consumed.lock().unwrap(),
            total,
            "drainer must read ALL bytes (including those after the ready banner) \
             to keep kubectl's stdout pipe alive — otherwise Go SIGPIPE-exits"
        );
    }

    #[test]
    fn ready_drainer_yields_recv_err_when_eof_before_banner() {
        let stream = b"error: unable to forward port because pod is not running\n".to_vec();
        let rx = spawn_ready_drainer(Cursor::new(stream));
        let err = rx.recv_timeout(Duration::from_secs(1));
        assert!(
            err.is_err(),
            "recv must Err when EOF arrives before the ready banner; got Ok"
        );
    }

    #[test]
    fn silent_drainer_reads_to_eof() {
        let stream = b"warning A\nwarning B\nwarning C\n".to_vec();
        let total = stream.len();
        let consumed = Arc::new(Mutex::new(0usize));

        struct Counter {
            inner: Cursor<Vec<u8>>,
            consumed: Arc<Mutex<usize>>,
        }
        impl Read for Counter {
            fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                let n = self.inner.read(buf)?;
                *self.consumed.lock().unwrap() += n;
                Ok(n)
            }
        }

        spawn_silent_drainer(Counter {
            inner: Cursor::new(stream),
            consumed: Arc::clone(&consumed),
        });

        for _ in 0..100 {
            if *consumed.lock().unwrap() >= total {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(*consumed.lock().unwrap(), total);
    }
}
