// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The runner's restic, run so that a stopped run can stop it cleanly.
//!
//! The runner is PID 1 of its pod, and restic is its child. When Kubernetes
//! stops a run (the Job's deadline, a deleted or evicted pod), only the runner
//! receives SIGTERM; restic, mid-`backup` or mid-`prune`, receives nothing.
//! When the runner then exits, the kernel ends every other process in the
//! pod's PID namespace with SIGKILL, and a SIGKILLed restic leaves its
//! repository lock behind. restic treats a lock as stale only after 30
//! minutes, and a prune's lock is exclusive, so for that half hour a check or
//! a manual run fails on it. (The next scheduled run's `restic unlock` clears
//! it, but only once it is stale.)
//!
//! [`ForwardingRestic`] runs restic the way `backup_core::SubprocessRestic`
//! does, and records each child in a [`LiveResticChildren`] the stop holds.
//! The stop passes its signal on ([`LiveResticChildren::close_and_signal`]):
//! restic 0.18 handles SIGTERM and SIGINT alike — it prints `signal
//! terminated received, cleaning up`, removes its lock and exits — and the
//! stop gives it a bounded moment to do so ([`release_restic`]).
//!
//! # Signalling a pid safely
//!
//! A pid names a process only until it is reaped; after that the kernel may
//! give it to another. So the waiting side never reaps a child the stop could
//! still signal: it waits for the exit with `waitid(WNOWAIT)`, which leaves
//! the child a zombie whose pid stays reserved, removes it from the live set
//! under the same lock the stop signals under, and only then reaps it.

use std::collections::BTreeSet;
use std::io::Read;
use std::path::PathBuf;
use std::process::{Child, Command, Output, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use backup_core::restic_runner::{backup_summary_snapshot_id, restic_error};
use backup_core::ResticRunner;
use cli_core::{CliError, Result};

/// The restic children a run has running, by pid. Shared between
/// [`ForwardingRestic`], which adds and removes them, and the stop, which
/// signals what is there.
#[derive(Clone, Debug, Default)]
pub struct LiveResticChildren(Arc<Mutex<ChildrenState>>);

#[derive(Debug, Default)]
struct ChildrenState {
    /// Children started and not yet exited — never reaped while listed here.
    pids: BTreeSet<u32>,
    /// Set by the stop; no restic starts after it.
    stopping: bool,
}

impl LiveResticChildren {
    /// Refuse every later start, and send `signal` to every child running
    /// now. Returns how many were signalled.
    pub fn close_and_signal(&self, signal: libc::c_int) -> usize {
        let mut state = self.lock();
        state.stopping = true;
        for pid in &state.pids {
            // SAFETY: `kill(2)` takes plain integers and touches no memory of
            // ours. The pid is a child that has not been reaped (see the
            // module docs), so it is still that child's.
            unsafe { libc::kill(*pid as libc::pid_t, signal) };
        }
        state.pids.len()
    }

    /// How many children are running.
    pub fn running(&self) -> usize {
        self.lock().pids.len()
    }

    /// SIGKILL every child still running; returns how many.
    pub fn kill_remaining(&self) -> usize {
        self.close_and_signal(libc::SIGKILL)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, ChildrenState> {
        // Every operation is one step on the state, so a poisoned lock still
        // guards a good one.
        self.0.lock().unwrap_or_else(|p| p.into_inner())
    }
}

/// The in-cluster runner's [`ResticRunner`]: restic run as a child the stop
/// can signal (see the module docs). Otherwise it behaves as
/// `backup_core::SubprocessRestic`: `RESTIC_PASSWORD` in the environment,
/// never on argv; stdout captured; a failure classified by `restic_error`.
pub struct ForwardingRestic {
    program: PathBuf,
    live: LiveResticChildren,
}

impl ForwardingRestic {
    /// Run `program` (`restic` in production, found through `PATH`).
    pub fn new(program: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
            live: LiveResticChildren::default(),
        }
    }

    /// The children this runner has running — a handle that stays current.
    pub fn live_children(&self) -> LiveResticChildren {
        self.live.clone()
    }

    /// Run restic to completion and collect its output, keeping the child in
    /// the live set, unreaped, for exactly as long as it runs.
    fn output(&self, argv: &[String], pass: &str) -> Result<Output> {
        let verb = argv.first().map(String::as_str).unwrap_or("?");
        let mut child: Child = {
            // Checked and recorded under one lock, so a child is either
            // refused or signalled by a stop that has begun — never neither.
            let mut state = self.live.lock();
            if state.stopping {
                return Err(CliError::Other(format!(
                    "the run is being stopped, so restic {verb} was not started"
                )));
            }
            let child = Command::new(&self.program)
                .args(argv)
                .env("RESTIC_PASSWORD", pass)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .map_err(|e| CliError::Other(format!("spawn restic: {e}")))?;
            state.pids.insert(child.id());
            child
        };
        let pid = child.id();

        // Both pipes are read to the end on their own threads, so a full pipe
        // never blocks restic.
        let stdout = read_to_end_on_a_thread(child.stdout.take());
        let stderr = read_to_end_on_a_thread(child.stderr.take());

        let exited = wait_for_exit_unreaped(pid);
        self.live.lock().pids.remove(&pid);
        let status = child
            .wait()
            .map_err(|e| CliError::Other(format!("wait restic {verb}: {e}")))?;
        exited.map_err(|e| CliError::Other(format!("wait restic {verb}: {e}")))?;
        let collect = |reader: std::thread::JoinHandle<Vec<u8>>| reader.join().unwrap_or_default();
        Ok(Output {
            status,
            stdout: collect(stdout),
            stderr: collect(stderr),
        })
    }
}

/// Read `pipe` to its end on a thread of its own.
fn read_to_end_on_a_thread<R: Read + Send + 'static>(
    pipe: Option<R>,
) -> std::thread::JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        if let Some(mut pipe) = pipe {
            let _ = pipe.read_to_end(&mut bytes);
        }
        bytes
    })
}

/// Block until child `pid` has exited, leaving it unreaped (`WNOWAIT`): its
/// pid stays reserved until [`Child::wait`] reaps it.
fn wait_for_exit_unreaped(pid: u32) -> std::io::Result<()> {
    loop {
        // SAFETY: `siginfo_t` is plain old data, valid all-zero; `waitid`
        // writes into it and keeps no reference past the call.
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        let rc = unsafe {
            libc::waitid(
                libc::P_PID,
                pid as libc::id_t,
                &mut info,
                libc::WEXITED | libc::WNOWAIT,
            )
        };
        if rc == 0 {
            return Ok(());
        }
        let err = std::io::Error::last_os_error();
        if err.kind() != std::io::ErrorKind::Interrupted {
            return Err(err);
        }
    }
}

impl ResticRunner for ForwardingRestic {
    fn run(&self, argv: &[String], pass: &str) -> Result<()> {
        self.run_stdout(argv, pass).map(|_| ())
    }

    fn run_stdout(&self, argv: &[String], pass: &str) -> Result<String> {
        let out = self.output(argv, pass)?;
        if !out.status.success() {
            return Err(restic_error(argv, out.status.code(), &out.stderr));
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    fn run_backup(&self, argv: &[String], pass: &str) -> Result<Option<String>> {
        Ok(backup_summary_snapshot_id(&self.run_stdout(argv, pass)?))
    }
}

/// What [`release_restic`] saw.
#[derive(Debug, PartialEq, Eq)]
pub enum Released {
    /// No restic was running.
    NoneRunning,
    /// Every restic signalled exited within the bound.
    Exited(usize),
    /// Some were still running at the bound and were killed.
    Killed(usize),
}

/// The stop's restic step: refuse every later start, pass `signal` on to
/// each restic running, and wait up to `bound` for them to remove their locks
/// and exit; SIGKILL whatever is left.
pub async fn release_restic(
    live: &LiveResticChildren,
    signal: libc::c_int,
    bound: Duration,
) -> Released {
    let signalled = live.close_and_signal(signal);
    if signalled == 0 {
        return Released::NoneRunning;
    }
    eprintln!(
        "stop: passed the signal on to {signalled} restic process(es); waiting up to {}s for \
         restic to remove its repository lock",
        bound.as_secs()
    );
    let deadline = tokio::time::Instant::now() + bound;
    while live.running() > 0 {
        if tokio::time::Instant::now() >= deadline {
            let killed = live.kill_remaining();
            eprintln!(
                "stop: restic still running after {}s; killed it. Its repository lock stays \
                 until it is stale (30 minutes) and the next run's `restic unlock` removes it",
                bound.as_secs()
            );
            return Released::Killed(killed);
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    eprintln!("stop: restic exited");
    Released::Exited(signalled)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt as _;
    use std::path::Path;
    use std::time::Instant;

    /// A fake restic: a shell script. It writes `started` once running, so
    /// a test can signal it only after it has installed its traps.
    fn fake_restic(dir: &Path, body: &str) -> PathBuf {
        let path = dir.join("restic");
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        // A just-written executable can be briefly busy (ETXTBSY) for exec.
        for _ in 0..200 {
            match Command::new(&path).arg("__probe").status() {
                Err(e) if e.raw_os_error() == Some(26) => {
                    std::thread::sleep(Duration::from_millis(5))
                }
                _ => break,
            }
        }
        path
    }

    fn wait_for(path: &Path) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !path.exists() {
            assert!(
                Instant::now() < deadline,
                "{} never appeared",
                path.display()
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn args(argv: &[&str]) -> Vec<String> {
        argv.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn restic_runs_as_before_password_in_its_env_and_stdout_returned() {
        let dir = tempfile::tempdir().unwrap();
        let bin = fake_restic(
            dir.path(),
            "[ \"$1\" = __probe ] && exit 0\n\
             echo \"$@\"\n\
             echo \"pw=$RESTIC_PASSWORD\"\n\
             echo '{\"message_type\":\"summary\",\"snapshot_id\":\"ab12\"}'",
        );
        let r = ForwardingRestic::new(&bin);
        let out = r
            .run_stdout(&args(&["snapshots", "--json"]), "s3cret")
            .unwrap();
        assert!(out.starts_with("snapshots --json\npw=s3cret\n"), "{out}");
        assert_eq!(
            r.run_backup(&args(&["backup"]), "s3cret")
                .unwrap()
                .as_deref(),
            Some("ab12")
        );
        assert_eq!(r.live_children().running(), 0, "nothing left listed");
    }

    #[test]
    fn a_failing_restic_is_classified_as_before() {
        let dir = tempfile::tempdir().unwrap();
        let bin = fake_restic(
            dir.path(),
            "[ \"$1\" = __probe ] && exit 0\n\
             echo 'Fatal: wrong password or no key found' >&2\nexit 1",
        );
        let err = ForwardingRestic::new(&bin)
            .run(&args(&["unlock"]), "pw")
            .unwrap_err();
        assert!(
            matches!(&err, CliError::Restic { verb, exit: Some(1), .. } if verb == "unlock"),
            "{err:?}"
        );
        assert!(err.to_string().contains("wrong password"), "{err}");
    }

    /// Output larger than a pipe's buffer does not block restic: both pipes
    /// are read while it runs.
    #[test]
    fn a_restic_that_writes_a_lot_is_read_while_it_runs() {
        let dir = tempfile::tempdir().unwrap();
        let bin = fake_restic(
            dir.path(),
            "[ \"$1\" = __probe ] && exit 0\n\
             i=0; while [ $i -lt 20000 ]; do echo \"status line $i\"; \
             echo \"stderr line $i\" >&2; i=$((i+1)); done",
        );
        let out = ForwardingRestic::new(&bin)
            .run_stdout(&args(&["backup"]), "pw")
            .unwrap();
        assert!(out.ends_with("status line 19999\n"));
    }

    /// The stop's signal reaches a running restic, which then cleans up and
    /// exits: the call returns its failure promptly, and nothing is left
    /// listed.
    #[test]
    fn the_stops_signal_reaches_a_running_restic() {
        let dir = tempfile::tempdir().unwrap();
        let started = dir.path().join("started");
        let got = dir.path().join("got");
        let bin = fake_restic(
            dir.path(),
            &format!(
                "[ \"$1\" = __probe ] && exit 0\n\
                 trap 'echo TERM > {got}; echo \"signal terminated received, cleaning up\" >&2; \
                 exit 1' TERM\n\
                 trap 'echo INT > {got}; exit 1' INT\n\
                 touch {started}\n\
                 while :; do sleep 0.05; done",
                got = got.display(),
                started = started.display()
            ),
        );
        let r = ForwardingRestic::new(&bin);
        let live = r.live_children();
        let run = std::thread::spawn(move || r.run(&args(&["forget", "--prune"]), "pw"));
        wait_for(&started);
        assert_eq!(live.running(), 1);

        let rt = tokio::runtime::Runtime::new().unwrap();
        let t0 = Instant::now();
        let released = rt.block_on(release_restic(&live, libc::SIGTERM, Duration::from_secs(5)));
        assert_eq!(released, Released::Exited(1));
        assert!(t0.elapsed() < Duration::from_secs(2), "{:?}", t0.elapsed());
        assert_eq!(std::fs::read_to_string(&got).unwrap(), "TERM\n");
        let err = run.join().unwrap().unwrap_err();
        assert!(err.to_string().contains("cleaning up"), "{err}");
        assert_eq!(live.running(), 0);
    }

    /// The signal passed on is the one given: SIGINT stays SIGINT.
    #[test]
    fn sigint_is_passed_on_as_sigint() {
        let dir = tempfile::tempdir().unwrap();
        let started = dir.path().join("started");
        let got = dir.path().join("got");
        let bin = fake_restic(
            dir.path(),
            &format!(
                "[ \"$1\" = __probe ] && exit 0\n\
                 trap 'echo TERM > {got}; exit 1' TERM\n\
                 trap 'echo INT > {got}; exit 1' INT\n\
                 touch {started}\n\
                 while :; do sleep 0.05; done",
                got = got.display(),
                started = started.display()
            ),
        );
        let r = ForwardingRestic::new(&bin);
        let live = r.live_children();
        let run = std::thread::spawn(move || r.run(&args(&["backup"]), "pw"));
        wait_for(&started);
        let rt = tokio::runtime::Runtime::new().unwrap();
        let released = rt.block_on(release_restic(&live, libc::SIGINT, Duration::from_secs(5)));
        assert_eq!(released, Released::Exited(1));
        assert_eq!(std::fs::read_to_string(&got).unwrap(), "INT\n");
        assert!(run.join().unwrap().is_err());
    }

    /// A restic that does not exit within the bound is killed, and the stop
    /// goes on: the grace period is not spent waiting for it.
    #[test]
    fn a_restic_that_ignores_the_signal_is_killed_at_the_bound() {
        let dir = tempfile::tempdir().unwrap();
        let started = dir.path().join("started");
        // `exec` keeps it one process (an ignored signal survives exec), so
        // the SIGKILL ends it and nothing else holds its pipes open.
        let bin = fake_restic(
            dir.path(),
            &format!(
                "[ \"$1\" = __probe ] && exit 0\n\
                 trap '' TERM\n\
                 touch {started}\n\
                 exec sleep 30",
                started = started.display()
            ),
        );
        let r = ForwardingRestic::new(&bin);
        let live = r.live_children();
        let run = std::thread::spawn(move || r.run(&args(&["backup"]), "pw"));
        wait_for(&started);
        let rt = tokio::runtime::Runtime::new().unwrap();
        let t0 = Instant::now();
        let released = rt.block_on(release_restic(
            &live,
            libc::SIGTERM,
            Duration::from_millis(300),
        ));
        let took = t0.elapsed();
        assert_eq!(released, Released::Killed(1));
        assert!(took < Duration::from_secs(3), "{took:?}");
        let err = run.join().unwrap().unwrap_err();
        assert!(
            matches!(err, CliError::Restic { exit: None, .. }),
            "{err:?}"
        );
        assert_eq!(live.running(), 0);
    }

    /// Once the stop has begun, restic is not started again: whatever the
    /// main thread moves on to (a prune after the backup) is refused.
    #[test]
    fn no_restic_starts_once_the_stop_has_begun() {
        let dir = tempfile::tempdir().unwrap();
        let ran = dir.path().join("ran");
        let bin = fake_restic(
            dir.path(),
            &format!(
                "[ \"$1\" = __probe ] && exit 0\ntouch {ran}",
                ran = ran.display()
            ),
        );
        let r = ForwardingRestic::new(&bin);
        let rt = tokio::runtime::Runtime::new().unwrap();
        assert_eq!(
            rt.block_on(release_restic(
                &r.live_children(),
                libc::SIGTERM,
                Duration::from_secs(1)
            )),
            Released::NoneRunning
        );
        let err = r.run(&args(&["forget", "--prune"]), "pw").unwrap_err();
        assert!(err.to_string().contains("being stopped"), "{err}");
        assert!(err.to_string().contains("restic forget"), "{err}");
        assert!(!ran.exists(), "restic was started after the stop began");
    }
}
