// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! A conversation with `apprafter` at a pseudo-terminal that is its controlling terminal (D.4a
//! d; GOTCHA-179, GOTCHA-180). The child's stdin and stderr are the pty's slave, made its
//! controlling terminal by the spawn point ([`Spawn::std_at_terminal`]); stdout is a pipe. Each
//! step waits for the text the terminal must show before it types — a key typed earlier would
//! meet the terminal before the prompt made it raw — and the end reads back, through the
//! master, whether the terminal was put back (ECHO and ICANON set). Unix only.

use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::process::{Command, ExitStatus, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::spawn::{open_pty, Spawn};

/// How long one step may wait: a plain build derives release keys (seconds per `lock on`).
pub const STEP_TIMEOUT: Duration = Duration::from_secs(120);

/// One step of a conversation.
pub enum Step {
    /// Wait until the terminal shows this text, after everything an earlier step matched.
    Expect(&'static str),
    /// Type these bytes.
    Type(Vec<u8>),
    /// Type what this makes of everything the terminal showed so far (a recovery key).
    TypeFrom(fn(&str) -> Vec<u8>),
    /// Send this signal to the child.
    Signal(libc::c_int),
}

/// `text`, then Enter.
pub fn line(text: &str) -> Step {
    Step::Type(format!("{text}\r").into_bytes())
}

/// How a conversation ended.
pub struct Ended {
    pub status: ExitStatus,
    pub stdout: String,
    /// Everything the terminal showed: the child's stderr and its prompts.
    pub terminal: String,
    /// ECHO and ICANON are set on the terminal again once the child ended.
    pub restored: bool,
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

pub fn converse(sp: &Spawn, args: &[&str], steps: &[Step]) -> Ended {
    converse_with(sp, args, steps, |_| {})
}

/// [`converse`] with `wire` given the command before it starts (e.g. stderr to null).
pub fn converse_with(
    sp: &Spawn,
    args: &[&str],
    steps: &[Step],
    wire: impl FnOnce(&mut Command),
) -> Ended {
    converse_on(sp, args, steps, false, wire)
}

/// [`converse`] with stdout at the terminal too, as a shell runs a command: what asks only
/// where stdin and stdout are both terminals — the `target add` and `repo creds add` wizards —
/// asks here (WI-478). `Ended::stdout` is then empty: everything shows in `terminal`.
pub fn converse_at_terminal(sp: &Spawn, args: &[&str], steps: &[Step]) -> Ended {
    converse_on(sp, args, steps, true, |_| {})
}

/// The conversation: stdin and stderr at the pty, and stdout too when `stdout_at_terminal`,
/// else a pipe read on its own thread.
fn converse_on(
    sp: &Spawn,
    args: &[&str],
    steps: &[Step],
    stdout_at_terminal: bool,
    wire: impl FnOnce(&mut Command),
) -> Ended {
    let (master, slave) = open_pty();
    let out = stdout_at_terminal.then(|| slave.try_clone().expect("dup the slave"));
    let mut cmd = sp.std_at_terminal(args, slave);
    match out {
        Some(slave) => cmd.stdout(Stdio::from(slave)),
        None => cmd.stdout(Stdio::piped()),
    };
    wire(&mut cmd);
    let mut child = cmd.spawn().expect("spawn apprafter");
    // The command holds the slave until it drops: the terminal must close with the child.
    drop(cmd);
    let out_reader = child.stdout.take().map(|mut stdout| {
        std::thread::spawn(move || {
            let mut out = String::new();
            let _ = stdout.read_to_string(&mut out);
            out
        })
    });
    let mut writer = std::fs::File::from(master.try_clone().expect("dup the master"));
    let mut reader = std::fs::File::from(master);
    let seen = Arc::new(Mutex::new(Vec::<u8>::new()));
    let sink = Arc::clone(&seen);
    let (closed, drained) = std::sync::mpsc::channel::<()>();
    // Until the child's side closes (EIO on Linux, EOF elsewhere).
    std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        while let Ok(n) = reader.read(&mut buf) {
            if n == 0 {
                break;
            }
            sink.lock().unwrap().extend_from_slice(&buf[..n]);
        }
        let _ = closed.send(());
    });
    let text =
        |seen: &Arc<Mutex<Vec<u8>>>| String::from_utf8_lossy(&seen.lock().unwrap()).into_owned();
    let mut matched = 0usize;
    for step in steps {
        match step {
            Step::Expect(want) => {
                let deadline = Instant::now() + STEP_TIMEOUT;
                loop {
                    let at = {
                        let shown = seen.lock().unwrap();
                        shown
                            .get(matched..)
                            .and_then(|rest| find(rest, want.as_bytes()))
                    };
                    if let Some(at) = at {
                        matched += at + want.len();
                        break;
                    }
                    if let Some(status) = child.try_wait().expect("try_wait") {
                        panic!("exited {status} before showing {want:?}:\n{}", text(&seen));
                    }
                    assert!(
                        Instant::now() < deadline,
                        "{want:?} not shown within {STEP_TIMEOUT:?}:\n{}",
                        text(&seen)
                    );
                    std::thread::sleep(Duration::from_millis(20));
                }
            }
            Step::Type(bytes) => writer.write_all(bytes).expect("type"),
            Step::TypeFrom(make) => writer.write_all(&make(&text(&seen))).expect("type"),
            Step::Signal(sig) => {
                // SAFETY: a signal to our own child, by its pid.
                let sent = unsafe { libc::kill(child.id() as libc::pid_t, *sig) };
                assert_eq!(sent, 0, "kill: {}", std::io::Error::last_os_error());
            }
        }
    }
    let deadline = Instant::now() + STEP_TIMEOUT;
    let status = loop {
        if let Some(status) = child.try_wait().expect("try_wait") {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            panic!("no exit within {STEP_TIMEOUT:?}:\n{}", text(&seen));
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let restored = {
        let mut t = std::mem::MaybeUninit::<libc::termios>::uninit();
        // Through the master, which reads the slave's termios on Linux and macOS alike. Not
        // through a handle on the slave: the child led the terminal's session, and on macOS a
        // session leader's exit revokes every handle on its controlling terminal, so a read
        // there fails once the child has ended, put back or not.
        // SAFETY: tcgetattr fills `t` when it returns 0; `writer` is our handle on the master.
        let read = unsafe { libc::tcgetattr(writer.as_raw_fd(), t.as_mut_ptr()) } == 0;
        // SAFETY: filled when `read` is true.
        read && {
            let flags = unsafe { t.assume_init() }.c_lflag;
            flags & (libc::ECHO | libc::ICANON) == (libc::ECHO | libc::ICANON)
        }
    };
    let stdout = out_reader
        .map(|r| r.join().expect("stdout reader"))
        .unwrap_or_default();
    drained
        .recv_timeout(Duration::from_secs(10))
        .expect("the terminal closes once the child has exited");
    Ended {
        status,
        stdout,
        terminal: text(&seen),
        restored,
    }
}

/// The recovery key `lock on` showed, as a person types it back: its two lines, a space between.
pub fn shown_recovery_key(terminal: &str) -> String {
    let mut lines = terminal
        .lines()
        .map(str::trim)
        .skip_while(|l| !l.starts_with("APPRAFTER-RECOVERY-KEY-1"));
    let first = lines.next().expect("a recovery key was shown");
    let second = lines.find(|l| !l.is_empty()).expect("its second line");
    format!("{first} {second}")
}
