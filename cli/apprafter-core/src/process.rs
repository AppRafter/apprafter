// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Child processes with a deadline (D.3 overview §3.4): every pipe drained on its own thread,
//! the child polled, and on timeout or cancellation killed with everything it started.
//!
//! The child gets a process tree of its own, so a kill reaches the grandchildren too (a login
//! profile's hung network call, a tool's shim):
//! - Unix: a new session (`setsid` before exec), whose process group the kill signals. A new
//!   session rather than only a new process group (`process_group(0)`), because a session has
//!   no controlling terminal: an interactive shell (the macOS login-shell probe runs `-i`)
//!   started in a background process group of the terminal the app was launched from takes
//!   that terminal's foreground away from the app (zsh) or stops itself on SIGTTIN until the
//!   timeout (bash). Without one, it has no terminal to take, and a tool that would prompt on
//!   `/dev/tty` fails at once instead of waiting out the timeout.
//! - Windows: a Job Object, the child assigned while still suspended so nothing it starts can
//!   escape, then `TerminateJobObject`. `CREATE_NO_WINDOW` too, so the desktop (a GUI-subsystem
//!   process with no console) never flashes a console window per console tool it runs.
//!
//! A child that exits leaves its tree alone: only a timeout or a cancellation kills it.

use std::io::{self, Read};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::{mpsc, Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use crate::CancellationToken;

const POLL: Duration = Duration::from_millis(50);
/// How long the pipes may stay open after the child is gone (something it started can hold
/// them): one deadline for both pipes, after which whatever arrived is returned.
const DRAIN_GRACE: Duration = Duration::from_secs(1);

/// What a bounded child left behind.
#[derive(Debug)]
pub struct BoundedOutput {
    /// `None` when the child was killed (timeout or cancellation).
    pub status: Option<ExitStatus>,
    /// Everything read from the pipe by the end of the drain grace, even when something the
    /// child started still holds it open.
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    /// Killed because the timeout passed (not because `cancel` tripped).
    pub timed_out: bool,
}

/// Spawn `cmd` (stdin null, both outputs piped) in a process tree of its own (module docs),
/// wait at most `timeout`, and kill the tree on timeout or when `cancel` trips (`status: None`;
/// `timed_out` only for the timeout). A missing program is the spawn's `ErrorKind::NotFound`,
/// for the caller to type.
///
/// `run_bounded` owns how the child is started: on Unix it runs `setsid` before exec (a
/// `process_group` set on `cmd` makes that fail), on Windows it sets the creation flags,
/// replacing any set on `cmd`.
pub fn run_bounded(
    mut cmd: Command,
    timeout: Duration,
    cancel: &CancellationToken,
) -> io::Result<BoundedOutput> {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    tree::isolate(&mut cmd);
    let mut child = cmd.spawn()?;
    let tree = tree::Tree::adopt(&mut child)?;
    let (done, finished) = mpsc::channel();
    let stdout = drain(child.stdout.take(), &done);
    let stderr = drain(child.stderr.take(), &done);
    drop(done);
    let deadline = Instant::now() + timeout;
    let (status, timed_out) = loop {
        if let Some(status) = child.try_wait()? {
            break (Some(status), false);
        }
        if cancel.is_cancelled() || Instant::now() >= deadline {
            tree.kill(&mut child);
            let _ = child.wait();
            break (None, !cancel.is_cancelled());
        }
        std::thread::sleep(POLL);
    };
    // One grace for both pipes: each drain reports its end of file, or the deadline passes.
    let grace = Instant::now() + DRAIN_GRACE;
    for _ in 0..2 {
        let left = grace.saturating_duration_since(Instant::now());
        if finished.recv_timeout(left).is_err() {
            break;
        }
    }
    Ok(BoundedOutput {
        status,
        stdout: take(&stdout),
        stderr: take(&stderr),
        timed_out,
    })
}

type Buffer = Arc<Mutex<Vec<u8>>>;

/// Read `pipe` chunk by chunk into a shared buffer on a thread of its own; `done` hears when it
/// reaches end of file. A missing pipe is an empty buffer, done at once.
fn drain(pipe: Option<impl Read + Send + 'static>, done: &mpsc::Sender<()>) -> Buffer {
    let buf = Buffer::default();
    let done = done.clone();
    let Some(mut pipe) = pipe else {
        let _ = done.send(());
        return buf;
    };
    let sink = Arc::clone(&buf);
    std::thread::spawn(move || {
        let mut chunk = [0u8; 8192];
        loop {
            match pipe.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => sink
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .extend_from_slice(&chunk[..n]),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(_) => break,
            }
        }
        let _ = done.send(());
    });
    buf
}

/// What `buf` holds now; a drain still reading appends to the emptied buffer, unseen.
fn take(buf: &Buffer) -> Vec<u8> {
    std::mem::take(&mut *buf.lock().unwrap_or_else(PoisonError::into_inner))
}

#[cfg(unix)]
mod tree {
    use std::io;
    use std::os::unix::process::CommandExt;
    use std::process::{Child, Command};

    /// A new session (module docs), so the child leads a process group of its own.
    pub(super) fn isolate(cmd: &mut Command) {
        // SAFETY: `setsid` is async-signal-safe and touches no memory of this process.
        unsafe {
            cmd.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }

    /// The child's process group: its pid, as the session leader.
    pub(super) struct Tree {
        group: libc::pid_t,
    }

    impl Tree {
        pub(super) fn adopt(child: &mut Child) -> io::Result<Tree> {
            let group = libc::pid_t::try_from(child.id())
                .map_err(|_| io::Error::other("child pid out of range"))?;
            Ok(Tree { group })
        }

        /// SIGKILL to the whole group. The child is not reaped yet, so its pid (the group id)
        /// cannot have been reused.
        pub(super) fn kill(&self, child: &mut Child) {
            // SAFETY: a plain signal to a process group this call created.
            unsafe { libc::kill(-self.group, libc::SIGKILL) };
            let _ = child.kill();
        }
    }
}

#[cfg(windows)]
mod tree {
    use std::io;
    use std::os::windows::io::AsRawHandle;
    use std::os::windows::process::CommandExt;
    use std::process::{Child, Command};

    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Thread32First, Thread32Next, TH32CS_SNAPTHREAD, THREADENTRY32,
    };
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, TerminateJobObject,
    };
    use windows_sys::Win32::System::Threading::{
        OpenThread, ResumeThread, CREATE_NO_WINDOW, CREATE_SUSPENDED, THREAD_SUSPEND_RESUME,
    };

    /// No console window; suspended until it is in the job ([`Tree::adopt`]).
    pub(super) fn isolate(cmd: &mut Command) {
        cmd.creation_flags(CREATE_NO_WINDOW | CREATE_SUSPENDED);
    }

    /// The Job Object the child (and everything it starts) belongs to; `None` when Windows
    /// would not make or assign one, and then a kill reaches the child alone.
    pub(super) struct Tree {
        job: Option<HANDLE>,
    }

    impl Tree {
        /// Put the suspended child in a new job, then resume it. A child that cannot be
        /// resumed is killed: it would only wait out the timeout.
        pub(super) fn adopt(child: &mut Child) -> io::Result<Tree> {
            let tree = Tree { job: assign(child) };
            if let Err(e) = resume(child.id()) {
                tree.kill(child);
                let _ = child.wait();
                return Err(e);
            }
            Ok(tree)
        }

        pub(super) fn kill(&self, child: &mut Child) {
            if let Some(job) = self.job {
                // SAFETY: `job` is a live job handle this tree owns.
                unsafe { TerminateJobObject(job, 1) };
            }
            let _ = child.kill();
        }
    }

    impl Drop for Tree {
        fn drop(&mut self) {
            if let Some(job) = self.job {
                // SAFETY: closed once, here. The job lives on while processes are in it, and
                // without KILL_ON_JOB_CLOSE closing it kills nothing.
                unsafe { CloseHandle(job) };
            }
        }
    }

    fn assign(child: &Child) -> Option<HANDLE> {
        // SAFETY: an unnamed job with default security; no pointer outlives the call.
        let job = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if job.is_null() {
            return None;
        }
        // SAFETY: both handles are live; the child's is borrowed from `child`.
        if unsafe { AssignProcessToJobObject(job, child.as_raw_handle() as HANDLE) } == 0 {
            // SAFETY: the job was made above and is closed once.
            unsafe { CloseHandle(job) };
            return None;
        }
        Some(job)
    }

    /// Resume every thread of the suspended process `pid` (std keeps no thread handle).
    fn resume(pid: u32) -> io::Result<()> {
        // SAFETY: a snapshot of the system's threads, closed below.
        let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
        if snapshot == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        let mut entry = THREADENTRY32 {
            dwSize: std::mem::size_of::<THREADENTRY32>() as u32,
            cntUsage: 0,
            th32ThreadID: 0,
            th32OwnerProcessID: 0,
            tpBasePri: 0,
            tpDeltaPri: 0,
            dwFlags: 0,
        };
        let mut resumed = 0;
        // SAFETY: `entry` is a correctly sized THREADENTRY32 owned by this frame.
        let mut more = unsafe { Thread32First(snapshot, &mut entry) } != 0;
        while more {
            if entry.th32OwnerProcessID == pid {
                // SAFETY: a handle opened, used and closed right here.
                unsafe {
                    let thread = OpenThread(THREAD_SUSPEND_RESUME, 0, entry.th32ThreadID);
                    if !thread.is_null() {
                        if ResumeThread(thread) != u32::MAX {
                            resumed += 1;
                        }
                        CloseHandle(thread);
                    }
                }
            }
            // SAFETY: as for Thread32First.
            more = unsafe { Thread32Next(snapshot, &mut entry) } != 0;
        }
        // SAFETY: the snapshot is closed once.
        unsafe { CloseHandle(snapshot) };
        if resumed == 0 {
            return Err(io::Error::other(format!(
                "could not resume the suspended child process {pid}"
            )));
        }
        Ok(())
    }
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

    /// (a) A background job keeps the pipes open after the child exits (a login profile's
    /// `x &`): the bytes the child wrote before are kept, and the call ends after one grace.
    #[test]
    fn output_written_before_a_background_job_holds_the_pipe_is_kept() {
        let started = Instant::now();
        let out = run_bounded(
            sh("printf out; sleep 5 &"),
            Duration::from_secs(5),
            &CancellationToken::new(),
        )
        .unwrap();
        let took = started.elapsed();
        assert_eq!(out.stdout, b"out");
        assert_eq!(out.status.and_then(|s| s.code()), Some(0));
        assert!(took < DRAIN_GRACE + Duration::from_millis(700), "{took:?}");
    }

    /// (b) A timeout kills the child's whole session: a hung grandchild does not outlive it.
    #[test]
    fn a_timeout_kills_the_grandchildren_too() {
        let dir = tempfile::tempdir().unwrap();
        let pid_file = dir.path().join("grandchild.pid");
        let mut cmd = sh(r#"sleep 30 & echo $! > "$0"; wait"#);
        cmd.arg(&pid_file);
        let timeout = Duration::from_millis(500);
        let started = Instant::now();
        let out = run_bounded(cmd, timeout, &CancellationToken::new()).unwrap();
        let took = started.elapsed();
        assert!(out.timed_out && out.status.is_none());
        let pid: libc::pid_t = std::fs::read_to_string(&pid_file)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        // Killed, then reaped by init: give the reaper a moment.
        let gone = (0..60).any(|_| {
            // SAFETY: signal 0 only checks that `pid` exists.
            let alive = unsafe { libc::kill(pid, 0) } == 0;
            if alive {
                std::thread::sleep(Duration::from_millis(50));
            }
            !alive
        });
        if !gone {
            // SAFETY: the grandchild this test started; do not leave it running.
            unsafe { libc::kill(pid, libc::SIGKILL) };
        }
        assert!(gone, "grandchild {pid} outlived the timeout");
        assert!(took < timeout + DRAIN_GRACE, "{took:?}");
    }

    /// (c) Both pipes held open after the child is gone wait out ONE shared grace, not one each.
    #[test]
    fn both_held_pipes_share_one_grace() {
        let started = Instant::now();
        let out = run_bounded(
            sh("printf out; printf err >&2; sleep 5 &"),
            Duration::from_secs(5),
            &CancellationToken::new(),
        )
        .unwrap();
        let took = started.elapsed();
        assert!(took < DRAIN_GRACE + DRAIN_GRACE / 2, "{took:?}");
        assert_eq!(
            (out.stdout.as_slice(), out.stderr.as_slice()),
            (&b"out"[..], &b"err"[..])
        );
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

/// Windows: the child's Job Object. The helper processes are this test binary started again
/// (no shell needed, so the test runs under wine too); a helper does nothing unless a test
/// started it, so `--ignored` runs stay quick.
#[cfg(all(test, windows))]
mod windows_tests {
    use super::*;
    use std::io::Write;
    use std::process::Stdio;

    use windows_sys::Win32::Foundation::{CloseHandle, WAIT_OBJECT_0};
    use windows_sys::Win32::System::Threading::{
        OpenProcess, TerminateProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE,
    };

    use crate::CancellationToken;

    const HELPER: &str = "APPRAFTER_PROCESS_TEST_HELPER";

    fn helper(name: &str) -> Command {
        let mut c = Command::new(std::env::current_exe().unwrap());
        c.args([
            "--exact",
            &format!("process::windows_tests::{name}"),
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(HELPER, "1");
        c
    }

    #[test]
    #[ignore = "a helper process of a_timeout_kills_the_grandchildren_too"]
    fn helper_sleeps() {
        if std::env::var_os(HELPER).is_some() {
            std::thread::sleep(Duration::from_secs(60));
        }
    }

    #[test]
    #[ignore = "a helper process of a_timeout_kills_the_grandchildren_too"]
    fn helper_starts_a_sleeper_and_hangs() {
        if std::env::var_os(HELPER).is_none() {
            return;
        }
        let mut sleeper = helper("helper_sleeps")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let mut out = std::io::stdout();
        writeln!(out, "grandchild {}", sleeper.id()).unwrap();
        out.flush().unwrap();
        let _ = sleeper.wait();
    }

    /// The timeout terminates the job: the grandchild the child started dies with it.
    #[test]
    fn a_timeout_kills_the_grandchildren_too() {
        let out = run_bounded(
            helper("helper_starts_a_sleeper_and_hangs"),
            Duration::from_secs(5),
            &CancellationToken::new(),
        )
        .unwrap();
        assert!(out.timed_out && out.status.is_none());
        let text = String::from_utf8_lossy(&out.stdout);
        // libtest prints the test's name on the same line, before the helper's own output.
        let pid: u32 = text
            .split_once("grandchild ")
            .and_then(|(_, rest)| rest.split_whitespace().next())
            .unwrap_or_else(|| panic!("no grandchild pid in {text:?}"))
            .parse()
            .unwrap();
        // SAFETY: a handle to the grandchild, waited on and closed here.
        let gone = unsafe {
            let process = OpenProcess(PROCESS_SYNCHRONIZE | PROCESS_TERMINATE, 0, pid);
            if process.is_null() {
                true // already gone, its pid released
            } else {
                let gone = WaitForSingleObject(process, 3000) == WAIT_OBJECT_0;
                if !gone {
                    TerminateProcess(process, 1); // do not leave it running
                }
                CloseHandle(process);
                gone
            }
        };
        assert!(gone, "grandchild {pid} outlived the timeout");
    }
}
