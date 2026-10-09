// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Child processes with a deadline (D.3 overview §3.4): every pipe drained on its own thread,
//! the child polled and killed on timeout or cancellation.

use std::io::{self, Read};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use crate::CancellationToken;

const POLL: Duration = Duration::from_millis(50);
/// How long the pipes may stay open after the child is gone (a grandchild can hold them).
const DRAIN_GRACE: Duration = Duration::from_secs(1);

/// What a bounded child left behind.
#[derive(Debug)]
pub struct BoundedOutput {
    /// `None` when the child was killed (timeout or cancellation).
    pub status: Option<ExitStatus>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    /// Killed because the timeout passed (not because `cancel` tripped).
    pub timed_out: bool,
}

/// Spawn `cmd` (stdin null, both outputs piped), wait at most `timeout`, kill it on timeout or
/// when `cancel` trips (`status: None`; `timed_out` only for the timeout). A missing program is
/// the spawn's `ErrorKind::NotFound`, for the caller to type.
pub fn run_bounded(
    mut cmd: Command,
    timeout: Duration,
    cancel: &CancellationToken,
) -> io::Result<BoundedOutput> {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn()?;
    let stdout = drain(child.stdout.take());
    let stderr = drain(child.stderr.take());
    let deadline = Instant::now() + timeout;
    let (status, timed_out) = loop {
        if let Some(status) = child.try_wait()? {
            break (Some(status), false);
        }
        if cancel.is_cancelled() || Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            break (None, !cancel.is_cancelled());
        }
        std::thread::sleep(POLL);
    };
    Ok(BoundedOutput {
        status,
        stdout: stdout.recv_timeout(DRAIN_GRACE).unwrap_or_default(),
        stderr: stderr.recv_timeout(DRAIN_GRACE).unwrap_or_default(),
        timed_out,
    })
}

fn drain(pipe: Option<impl Read + Send + 'static>) -> mpsc::Receiver<Vec<u8>> {
    let (tx, rx) = mpsc::channel();
    if let Some(mut pipe) = pipe {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = pipe.read_to_end(&mut buf);
            let _ = tx.send(buf);
        });
    }
    rx
}

/// `#[cfg(unix)]`: they need `sh` (listed in the plan's Gate for the Windows check).
#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::io;
    use std::process::Command;
    use std::time::{Duration, Instant};

    use crate::CancellationToken;

    fn sh(script: &str) -> Command {
        let mut c = Command::new("sh");
        c.args(["-c", script]);
        c
    }

    #[test]
    fn output_and_status_are_captured() {
        let out = run_bounded(
            sh("printf out; printf err >&2; exit 3"),
            Duration::from_secs(5),
            &CancellationToken::new(),
        )
        .unwrap();
        assert_eq!(
            (out.stdout.as_slice(), out.stderr.as_slice()),
            (&b"out"[..], &b"err"[..])
        );
        assert_eq!(out.status.and_then(|s| s.code()), Some(3));
        assert!(!out.timed_out);
    }

    #[test]
    fn a_child_past_the_timeout_is_killed() {
        let started = Instant::now();
        let out = run_bounded(
            sh("sleep 10"),
            Duration::from_millis(200),
            &CancellationToken::new(),
        )
        .unwrap();
        assert!(out.timed_out && out.status.is_none());
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "{:?}",
            started.elapsed()
        );
    }

    #[test]
    fn a_cancelled_child_is_killed_and_not_timed_out() {
        let cancel = CancellationToken::new();
        let trip = cancel.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            trip.cancel();
        });
        let out = run_bounded(sh("sleep 10"), Duration::from_secs(10), &cancel).unwrap();
        assert!(!out.timed_out && out.status.is_none());
    }

    #[test]
    fn a_missing_program_is_not_found() {
        let err = run_bounded(
            Command::new("definitely-not-a-binary-9f3a"),
            Duration::from_secs(1),
            &CancellationToken::new(),
        )
        .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
    }
}
