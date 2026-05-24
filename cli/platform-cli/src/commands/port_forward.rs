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
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::Duration;

use cli_core::{CliError, Result};

/// Cap on captured stderr lines в `spawn_capturing_drainer`.
/// kubectl typically emits 1–3 stderr lines on the failure
/// paths we care about (no Endpoints, unauthorized,
/// unreachable kubeconfig); а small buffer keeps memory
/// bounded for long-running success paths где stderr might
/// stream forever.
const STDERR_CAPTURE_LIMIT: usize = 20;

/// Brief wait after `rx.recv()` resolves Err — gives the
/// stderr drainer thread time to push its last line into the
/// shared buffer before we read it. kubectl is already
/// exiting когда we get the Err, the drainer is just finishing
/// its read syscall. 100ms is plenty in practice.
const STDERR_FLUSH_GRACE_MS: u64 = 100;

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
    let stderr_buf = spawn_capturing_drainer(stderr);

    match rx.recv() {
        Ok(()) => Ok(()),
        Err(_) => {
            thread::sleep(Duration::from_millis(STDERR_FLUSH_GRACE_MS));
            Err(format_early_exit_error(&stderr_buf))
        }
    }
}

/// Pure helper — render the «kubectl exited early» error
/// message, attaching captured stderr lines когда available.
/// Walk-fix #2 post-B.1.79b: previous silent drainer
/// swallowed kubectl's diagnostic stderr, leaving operators
/// staring at а generic «exited before binding local port»
/// with no clue why (image pull error? RBAC? no Endpoints?).
fn format_early_exit_error(stderr_buf: &Arc<Mutex<Vec<String>>>) -> CliError {
    let captured = stderr_buf.lock().unwrap();
    if captured.is_empty() {
        CliError::Other(
            "kubectl port-forward exited before binding local port — and \
             kubectl produced no stderr output. This is unusual; kubectl may \
             have been killed by the OS, or the binary is broken. Try \
             `kubectl port-forward svc/<name> -n <ns> 8080:80` manually to see \
             the raw error."
                .into(),
        )
    } else {
        let text = captured.join("\n");
        CliError::Other(format!(
            "kubectl port-forward exited before binding local port. \
             kubectl stderr:\n  {}",
            text.replace('\n', "\n  ")
        ))
    }
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

/// Spawn а thread that drains а pipe to EOF AND captures
/// the last `STDERR_CAPTURE_LIMIT` lines in а shared buffer.
/// Walk-fix #2 post-B.1.79b — replaced the silent drainer
/// whose only job was «keep the pipe alive»; we now also
/// remember what was on it, so `wait_ready` can surface
/// kubectl's actual error when the ready banner never lands.
///
/// The buffer is bounded by а ring-eviction rule: when full,
/// the oldest line is dropped on each new push. Practical
/// memory ceiling: `STDERR_CAPTURE_LIMIT × longest-stderr-
/// line ≈ 20 × 256 bytes = 5 KiB`. Effectively zero для а
/// command that spends most of its time blocked on
/// `child.wait()`.
fn spawn_capturing_drainer<R: Read + Send + 'static>(reader: R) -> Arc<Mutex<Vec<String>>> {
    let buf = Arc::new(Mutex::new(Vec::with_capacity(STDERR_CAPTURE_LIMIT)));
    let buf_clone = Arc::clone(&buf);
    thread::spawn(move || {
        let reader = BufReader::new(reader);
        for line in reader.lines() {
            let Ok(line) = line else { break };
            let mut guard = buf_clone.lock().unwrap();
            if guard.len() >= STDERR_CAPTURE_LIMIT {
                guard.remove(0);
            }
            guard.push(line);
        }
    });
    buf
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
    fn capturing_drainer_records_all_lines_under_limit() {
        // Three lines, well under STDERR_CAPTURE_LIMIT (20).
        // Buffer must end up with all three в order.
        let stream = b"error: foo\nerror: bar\nerror: baz\n".to_vec();
        let buf = spawn_capturing_drainer(Cursor::new(stream));

        // Wait for the drainer thread to finish reading EOF.
        for _ in 0..100 {
            if buf.lock().unwrap().len() >= 3 {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        let captured = buf.lock().unwrap().clone();
        assert_eq!(captured.len(), 3);
        assert_eq!(captured[0], "error: foo");
        assert_eq!(captured[1], "error: bar");
        assert_eq!(captured[2], "error: baz");
    }

    #[test]
    fn capturing_drainer_evicts_oldest_when_over_limit() {
        // Feed STDERR_CAPTURE_LIMIT + 3 lines; buffer keeps
        // the last STDERR_CAPTURE_LIMIT — oldest 3 dropped.
        let mut stream = Vec::new();
        for i in 0..(STDERR_CAPTURE_LIMIT + 3) {
            stream.extend(format!("line {i}\n").bytes());
        }
        let buf = spawn_capturing_drainer(Cursor::new(stream));

        for _ in 0..100 {
            if buf.lock().unwrap().len() >= STDERR_CAPTURE_LIMIT {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        // Drainer may briefly hold more than limit during
        // push-then-evict; give it а moment to settle.
        thread::sleep(Duration::from_millis(50));

        let captured = buf.lock().unwrap().clone();
        assert_eq!(
            captured.len(),
            STDERR_CAPTURE_LIMIT,
            "buffer must stay bounded; got {} lines",
            captured.len()
        );
        // Oldest line ("line 0") dropped; newest must be
        // present.
        assert_eq!(captured.first().unwrap(), "line 3");
        assert_eq!(
            captured.last().unwrap(),
            &format!("line {}", STDERR_CAPTURE_LIMIT + 2)
        );
    }

    #[test]
    fn format_early_exit_error_uses_no_output_message_when_buffer_empty() {
        // No stderr captured — operator sees the generic
        // hint pointing к manual port-forward. Walk-fix #2
        // covers the case where the buffer carries content;
        // this test pins the empty path so future refactors
        // don't conflate it.
        let buf = Arc::new(Mutex::new(Vec::<String>::new()));
        let err = format_early_exit_error(&buf);
        let msg = err.to_string();
        assert!(
            msg.contains("no stderr output"),
            "empty-buffer error must say so explicitly; got: {msg}"
        );
        assert!(
            msg.contains("kubectl port-forward svc/"),
            "must hint at manual repro command; got: {msg}"
        );
    }

    #[test]
    fn format_early_exit_error_includes_captured_stderr() {
        // Walk-fix #2 regression guard. The diagnostic must
        // surface kubectl's actual error verbatim — operator
        // shouldn't have to rerun manually to find out why.
        let buf = Arc::new(Mutex::new(vec![
            "error: unable to forward port because pod is not running".to_string(),
            "current status=ContainerCreating".to_string(),
        ]));
        let err = format_early_exit_error(&buf);
        let msg = err.to_string();
        assert!(
            msg.contains("pod is not running"),
            "captured first stderr line must appear in error; got: {msg}"
        );
        assert!(
            msg.contains("ContainerCreating"),
            "captured second stderr line must appear in error; got: {msg}"
        );
        // Indented continuation lines are easier to scan when
        // the error renders в multi-line `miette` format.
        assert!(
            msg.contains("\n  "),
            "multi-line error should indent continuation lines for readability; \
             got: {msg}"
        );
    }
}
