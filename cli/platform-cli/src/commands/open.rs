// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! `apprafter open <ui>` — local port-forward, fetch admin
//! credentials, open default browser. Track B.1.79.
//!
//! Currently supports `argocd` only; `backstage` / `grafana`
//! / `hubble` deferred к later phases (those UIs aren't tier-
//! 1 resident yet).

use std::io::{BufRead, BufReader, Read};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::thread;

use cli_core::{CliError, Result};

use crate::commands::argocd_password;
use crate::commands::k8s_helpers::ensure_kubeconfig_tempfile;

/// Open Argo CD's web UI:
///
///   1. Decrypt the cached kubeconfig.
///   2. Resolve the admin password (cached age-encrypted в
///      state via `apprafter argocd-password`).
///   3. Spawn `kubectl port-forward svc/argocd-server -n argocd
///      8080:443` в background; wait until the bind message
///      lands on stdout.
///   4. Print URL (с optional `?proj=<filter>` AppProject
///      filter) + username + password; copy password to
///      clipboard когда возможно; open browser.
///   5. Block on the port-forward child; both die when the
///      operator Ctrl+C's the parent process (kubectl
///      inherits the terminal's SIGINT через the process
///      group, Rust's default).
///
/// `project_filter` controls the URL's `?proj=<name>` query
/// parameter. `None` drops the filter entirely (renders
/// `apprafter open argocd --all-projects`); `Some("apps")` is
/// the CLI default per Track B.1.79a — operators land on
/// their own user-apps view first, since platform components
/// are mostly hands-off after bootstrap.
pub fn argocd(project_filter: Option<&str>) -> Result<()> {
    let kc = ensure_kubeconfig_tempfile()?;

    // Resolve password through the existing cached path. This
    // also seeds the encrypted cache on first run.
    let password =
        argocd_password::compute_argocd_password(&cli_providers::k8s::KubectlCli, kc.path())?;

    let local_port = 8080;
    let mut child = spawn_port_forward("argocd-server", "argocd", local_port, 443, kc.path())?;

    wait_port_forward_ready(&mut child)?;

    let url = build_argocd_url(local_port, project_filter);
    let clipboard_status = copy_password_to_clipboard(&password);

    println!();
    println!("Opening Argo CD UI...");
    println!("  URL:       {url}");
    println!("  Username:  admin");
    print!("  Password:  {password}");
    match clipboard_status {
        ClipboardStatus::Copied => println!("  (copied к clipboard)"),
        ClipboardStatus::Failed => println!("  (clipboard unavailable — copy manually)"),
    }
    println!();

    let browser_status = open_in_browser(&url);
    match browser_status {
        Ok(()) => println!("✓ Browser opened"),
        Err(_) => println!("ℹ Browser open failed — paste the URL into your browser"),
    }
    println!("ℹ Press Ctrl+C к stop port-forward");

    // Block until the port-forward child exits — which happens
    // when the operator Ctrl+C's this process (kubectl receives
    // SIGINT via the process group и tears down cleanly).
    let _ = child.wait();
    Ok(())
}

/// Build the Argo CD UI URL, optionally narrowing к
/// а specific AppProject via Argo CD's `?proj=<name>` filter.
///
/// Pure fn (no IO, no globals) so tests can exhaustively
/// cover the default / explicit / drop-filter shapes.
fn build_argocd_url(local_port: u16, project_filter: Option<&str>) -> String {
    let base = format!("https://localhost:{local_port}");
    match project_filter {
        Some(proj) if !proj.is_empty() => format!("{base}/applications?proj={proj}"),
        _ => base,
    }
}

/// Outcome of the optional clipboard copy step. Distinct
/// variants so the printed banner can distinguish "we copied
/// for you" from "we tried и failed" — the second case warns
/// the operator that they need to manually copy from terminal
/// before the buffer flushes.
#[derive(Debug, PartialEq, Eq)]
enum ClipboardStatus {
    Copied,
    Failed,
}

/// Copy `password` к the system clipboard via `arboard`.
/// Fail-quiet — environment without а clipboard daemon
/// (headless servers, sandboxed shells, fresh SSH session
/// without X11 forwarding) returns `Failed` и the caller
/// surfaces а hint, без поднятия error через `?` chain.
fn copy_password_to_clipboard(password: &str) -> ClipboardStatus {
    match arboard::Clipboard::new() {
        Ok(mut cb) => match cb.set_text(password.to_string()) {
            Ok(()) => ClipboardStatus::Copied,
            Err(_) => ClipboardStatus::Failed,
        },
        Err(_) => ClipboardStatus::Failed,
    }
}

fn spawn_port_forward(
    service: &str,
    namespace: &str,
    local_port: u16,
    remote_port: u16,
    kubeconfig_path: &Path,
) -> Result<Child> {
    let child = Command::new("kubectl")
        .arg("port-forward")
        .arg(format!("svc/{service}"))
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

/// Waits для kubectl's `Forwarding from …` ready banner on
/// stdout AND keeps both pipes drained for the lifetime of the
/// child.
///
/// **Walk-fix #1 post-B.1.79.** The first implementation
/// closed stdout the moment it saw the ready line (BufReader
/// dropped + ChildStdout dropped + read-end of the pipe
/// closed). kubectl is а Go binary; Go's default SIGPIPE
/// handler terminates the process on the next write к а closed
/// stdout pipe, so kubectl exited within milliseconds — well
/// before the operator could use the forward. Symptom from the
/// v0.1.135 walk: `apprafter open argocd` printed the
/// credentials banner и returned к the shell immediately.
///
/// Fix: spawn one drainer thread per pipe. The stdout drainer
/// signals readiness через а one-shot channel когда it sees
/// the banner и continues reading until EOF, throwing the
/// bytes away. The stderr drainer just reads and discards.
/// Both threads outlive `wait_port_forward_ready`'s return;
/// they exit naturally when the child closes its pipes on
/// shutdown.
fn wait_port_forward_ready(child: &mut Child) -> Result<()> {
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| CliError::Other("kubectl port-forward без stdout".into()))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| CliError::Other("kubectl port-forward без stderr".into()))?;

    let rx = spawn_ready_drainer(stdout);
    spawn_silent_drainer(stderr);

    rx.recv().map_err(|_| {
        CliError::Other("kubectl port-forward exited before binding local port".into())
    })
}

/// Spawn а thread that reads stdout line-by-line, signals
/// readiness on the first `Forwarding from …` line, then keeps
/// draining silently until EOF. The sender drops when the
/// thread exits — if EOF lands before the ready banner, the
/// channel's `recv()` returns `Err` so the caller can surface
/// "exited early."
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
            // Continue reading (and discarding) bytes to keep
            // the pipe open — otherwise Go's default SIGPIPE
            // handler kills kubectl on the next stdout write.
        }
        // EOF — drop tx implicitly so a never-signaled recv()
        // resolves as Err and the caller propagates "exited
        // early".
    });
    rx
}

/// Spawn а thread that silently drains а pipe to EOF. Used for
/// stderr — we don't surface kubectl's diagnostic chatter
/// upward (the operator sees "exited early" if the ready
/// banner never lands; debugging beyond that is out of scope),
/// but we MUST read so the pipe's kernel buffer never fills
/// up, which would block the child on its next stderr write.
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

/// Open `url` в the operator's default browser. Cross-platform
/// shellout — `xdg-open` on Linux, `open` on macOS, `cmd /c
/// start` on Windows. Failures fall through quietly: the URL
/// is already printed к stdout, so the operator can paste
/// manually.
fn open_in_browser(url: &str) -> Result<()> {
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
        // Single "Forwarding from" line followed by EOF —
        // the receiver should yield Ok once and the thread
        // should exit cleanly.
        let stream = b"Forwarding from 127.0.0.1:8080 -> 443\n".to_vec();
        let rx = spawn_ready_drainer(Cursor::new(stream));
        rx.recv_timeout(Duration::from_secs(1))
            .expect("ready signal must arrive");
    }

    #[test]
    fn ready_drainer_continues_draining_after_signal() {
        // Real kubectl emits more stdout after the ready
        // banner ("Forwarding from [::1]:8080 -> 443\n",
        // followed by per-connection lines). If we close the
        // pipe right after signaling, kubectl SIGPIPEs and
        // exits. This test feeds extra bytes after the
        // banner and asserts the drainer thread reads ALL
        // of them — i.e. it doesn't shut down on the first
        // match.
        //
        // Wraps the input in a tracker so the test can
        // measure how many bytes were consumed.
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

        // Give the drainer a beat to finish reading the
        // remaining bytes. The thread reads to EOF on its
        // own; we just need to observe the result.
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
        // EOF before the banner means kubectl exited early
        // (bad kubeconfig, port collision, etc.). The
        // sender drops, and recv() must surface Err so the
        // caller can render "exited before binding local
        // port".
        let stream = b"error: unable to forward port because pod is not running\n".to_vec();
        let rx = spawn_ready_drainer(Cursor::new(stream));
        let err = rx.recv_timeout(Duration::from_secs(1));
        assert!(
            err.is_err(),
            "recv must Err when EOF arrives before the ready banner; got Ok"
        );
    }

    #[test]
    fn build_argocd_url_defaults_no_filter() {
        // Без project filter — голый base URL, никаких query
        // params'ов. Matches the --all-projects code path.
        assert_eq!(build_argocd_url(8080, None), "https://localhost:8080");
    }

    #[test]
    fn build_argocd_url_appends_proj_filter() {
        // Standard B.1.79a default — `apps` filter narrows the
        // UI к user-application view. Regression: ensure the
        // path is `/applications?proj=<name>` exactly (Argo CD's
        // documented filter URL shape).
        assert_eq!(
            build_argocd_url(8080, Some("apps")),
            "https://localhost:8080/applications?proj=apps"
        );
        assert_eq!(
            build_argocd_url(9999, Some("platform")),
            "https://localhost:9999/applications?proj=platform"
        );
    }

    #[test]
    fn build_argocd_url_treats_empty_filter_as_no_filter() {
        // Defensive: an empty string filter (e.g. caller
        // passed `Some("")`) renders как if --all-projects
        // was set — а bare `?proj=` would mean "filter к
        // project with empty name", which is nonsense.
        assert_eq!(build_argocd_url(8080, Some("")), "https://localhost:8080");
    }

    #[test]
    fn silent_drainer_reads_to_eof() {
        // Stderr drainer must NOT block the calling thread
        // и must read every byte so kubectl's stderr pipe
        // doesn't fill up — а full pipe deadlocks the
        // child's next stderr write.
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
