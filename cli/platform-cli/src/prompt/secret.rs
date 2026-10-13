// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The controlling terminal, for a secret (D.4a T61; ADR 0068 §4; critic G7, S10).
//!
//! A secret is asked only where a person sits: stdin is a terminal AND it is this process's
//! controlling terminal ([`terminal_present`]). An agent's tool that spawns `apprafter` without a
//! session of its own still has the person's terminal as its controlling one, and is refused
//! instead of drawing a prompt on the person's screen. The prompt reads and writes that device
//! itself (`/dev/tty`; `CONIN$` / `CONOUT$`), never stdin, stdout or stderr: `2>/dev/null` cannot
//! hide it.
//!
//! While it reads, nothing typed is shown (no echo, and no mask: a mask shows the length), and
//! Ctrl-C arrives as a byte, not a signal (`ISIG` cleared; Windows: processed input off), which
//! ends the prompt as [`PromptError::Cancelled`]. Escape sequences (arrow keys) are skipped;
//! Backspace erases a character, Ctrl-U the line. The terminal is put back as it was on every way
//! out the process can see: the read's return (a drop guard), SIGTERM, SIGHUP, SIGINT, SIGQUIT
//! and SIGTSTP (a handler restores it, then raises the signal again with the action it had
//! before the prompt — by default the process ends, or stops), and a panic (a hook). A signal
//! taken while a prompt reads also ends the read: the read polls in slices of at most 250 ms and
//! answers [`PromptError::Cancelled`] once one arrived, so a process that goes on (a handler of
//! its own, a SIGTSTP the shell continues) never reads on at a terminal that echoes again.
//! Windows: a console control handler, registered while a prompt reads, puts the console mode
//! back on Ctrl-Break and a close, and lets the handler before it act. SIGKILL cannot be
//! handled: `stty sane` or `reset` is the way back then. A prompt nobody answers for
//! [`PROMPT_TIMEOUT`] gives up.
//!
//! [`parent_process`] names the program that started this one, for the prompt's "started by"
//! line (critic S10): a person can tell their own command from an agent harness that runs
//! commands on their terminal. A name is whatever that program set (`prctl(PR_SET_NAME)`), so
//! Linux and macOS add its executable's path.

use std::io;
use std::time::{Duration, Instant};

use apprafter_core::{SecretString, Zeroizing};

/// How long a prompt waits (sudo's `passwd_timeout`); then the store is `store_locked{timed_out}`.
pub(crate) const PROMPT_TIMEOUT: Duration = Duration::from_secs(5 * 60);

/// The longest line kept, in bytes. The buffer is allocated at this size and never grows, so no
/// reallocation leaves a copy of a secret behind; the vault refuses a password over 1024 bytes.
const LINE_MAX: usize = 4096;

/// Why a prompt returned no answer.
#[derive(Debug)]
pub(crate) enum PromptError {
    /// No terminal to ask at (`Asker::present` was false).
    NoTerminal,
    /// Ctrl-C (or Ctrl-D on an empty line), or a signal while it read.
    Cancelled,
    /// Nobody answered within the limit.
    TimedOut,
    Io(io::Error),
}

fn not_utf8() -> PromptError {
    PromptError::Io(io::Error::new(
        io::ErrorKind::InvalidData,
        "the terminal sent text that is not UTF-8",
    ))
}

/// The program that started this process (critic S10).
#[derive(Debug)]
pub(crate) struct Parent {
    /// Its name as it set it.
    pub(crate) name: String,
    pub(crate) pid: u32,
    /// Its executable, where the OS says (Linux, macOS): a name alone is whatever the program
    /// chose (critic finding 14).
    pub(crate) exe: Option<std::path::PathBuf>,
}

/// The program that started this process; `None` when the OS does not say.
pub(crate) fn parent_process() -> Option<Parent> {
    parent()
}

#[cfg(target_os = "linux")]
fn parent() -> Option<Parent> {
    // SAFETY: getppid takes nothing and cannot fail.
    let pid = u32::try_from(unsafe { libc::getppid() }).ok()?;
    let name = std::fs::read_to_string(format!("/proc/{pid}/comm")).ok()?;
    Some(Parent {
        name: name.trim_end().to_string(),
        pid,
        exe: std::fs::read_link(format!("/proc/{pid}/exe")).ok(),
    })
}

#[cfg(target_os = "macos")]
fn parent() -> Option<Parent> {
    // SAFETY: getppid takes nothing and cannot fail.
    let ppid = unsafe { libc::getppid() };
    let mut buf = [0u8; 256];
    // SAFETY: proc_name writes at most `buf.len()` bytes into `buf`.
    let n = unsafe { libc::proc_name(ppid, buf.as_mut_ptr().cast(), buf.len() as u32) };
    let n = usize::try_from(n).ok().filter(|n| *n > 0)?;
    let mut path = [0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
    // SAFETY: proc_pidpath writes at most `path.len()` bytes into `path`.
    let len = unsafe { libc::proc_pidpath(ppid, path.as_mut_ptr().cast(), path.len() as u32) };
    let exe = usize::try_from(len).ok().filter(|l| *l > 0).map(|l| {
        use std::os::unix::ffi::OsStrExt;
        std::path::PathBuf::from(std::ffi::OsStr::from_bytes(&path[..l]))
    });
    Some(Parent {
        name: String::from_utf8_lossy(&buf[..n]).into_owned(),
        pid: u32::try_from(ppid).ok()?,
        exe,
    })
}

#[cfg(all(unix, not(any(target_os = "linux", target_os = "macos"))))]
fn parent() -> Option<Parent> {
    None
}

#[cfg(windows)]
fn parent() -> Option<Parent> {
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
        TH32CS_SNAPPROCESS,
    };
    let name_of = |wide: &[u16]| {
        let end = wide.iter().position(|c| *c == 0).unwrap_or(wide.len());
        String::from_utf16_lossy(&wide[..end])
    };
    let me = std::process::id();
    let mut entries = Vec::new();
    // SAFETY: a snapshot of the process list, closed below; each entry is ours to fill, its
    // size set as the API requires.
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snap == INVALID_HANDLE_VALUE {
            return None;
        }
        let mut e: PROCESSENTRY32W = std::mem::zeroed();
        e.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        let mut more = Process32FirstW(snap, &mut e) != 0;
        while more {
            entries.push((
                e.th32ProcessID,
                e.th32ParentProcessID,
                name_of(&e.szExeFile),
            ));
            more = Process32NextW(snap, &mut e) != 0;
        }
        CloseHandle(snap);
    }
    let parent = entries.iter().find(|(pid, _, _)| *pid == me)?.1;
    let name = entries.iter().find(|(pid, _, _)| *pid == parent)?.2.clone();
    // The snapshot names the executable file (`szExeFile`), not its path.
    Some(Parent {
        name,
        pid: parent,
        exe: None,
    })
}

// ---- Unix -------------------------------------------------------------------------------

/// T61: stdin is a terminal, and it is this process's controlling terminal.
#[cfg(unix)]
pub(crate) fn terminal_present() -> bool {
    // SAFETY: each call takes an fd or a pid and touches no memory of ours.
    unsafe {
        if libc::isatty(0) != 1 {
            return false;
        }
        let session = libc::tcgetsid(0);
        session != -1 && session == libc::getsid(0)
    }
}

/// The controlling terminal, open for reading and writing.
#[cfg(unix)]
pub(crate) struct Tty {
    file: std::fs::File,
}

#[cfg(unix)]
impl Tty {
    /// `/dev/tty`. Opening it can fail although [`terminal_present`] said yes (it raced away):
    /// that is an I/O error, never "no terminal".
    pub(crate) fn open() -> Result<Tty, PromptError> {
        use std::os::unix::fs::OpenOptionsExt;
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NOCTTY)
            .open("/dev/tty")
            .map_err(PromptError::Io)?;
        Ok(Tty { file })
    }

    pub(crate) fn say(&mut self, text: &str) -> io::Result<()> {
        use std::io::Write;
        (&self.file).write_all(text.as_bytes())?;
        (&self.file).flush()
    }

    pub(crate) fn read_secret(
        &mut self,
        prompt: &str,
        timeout: Duration,
    ) -> Result<SecretString, PromptError> {
        self.read(prompt, false, timeout)
    }

    pub(crate) fn read_answer(
        &mut self,
        prompt: &str,
        timeout: Duration,
    ) -> Result<String, PromptError> {
        self.read(prompt, true, timeout)
            .map(|s| s.expose().to_string())
    }

    fn read(
        &mut self,
        prompt: &str,
        echo: bool,
        timeout: Duration,
    ) -> Result<SecretString, PromptError> {
        use std::os::fd::AsRawFd;
        let _one = restore::PROMPT
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // Raw before the prompt shows: a key typed once it shows is never echoed (GOTCHA-179).
        let guard = TermGuard::raw(self.file.as_raw_fd())?;
        self.say(prompt).map_err(PromptError::Io)?;
        let line = read_line(&self.file, echo, Instant::now() + timeout);
        drop(guard);
        let _ = self.say("\n");
        line?.into_secret()
    }
}

/// The line a prompt collects, zeroed when it drops.
#[cfg(unix)]
struct Line {
    bytes: Zeroizing<Vec<u8>>,
}

#[cfg(unix)]
impl Line {
    fn new() -> Self {
        Line {
            bytes: Zeroizing::new(Vec::with_capacity(LINE_MAX)),
        }
    }

    /// Appends `b`; false (and nothing kept) once the line is full.
    fn push(&mut self, b: u8) -> bool {
        if self.bytes.len() < LINE_MAX {
            self.bytes.push(b);
            true
        } else {
            false
        }
    }

    /// Removes the last character (all its UTF-8 bytes, each zeroed); false on an empty line.
    fn erase(&mut self) -> bool {
        if self.bytes.is_empty() {
            return false;
        }
        while let Some(last) = self.bytes.len().checked_sub(1) {
            let b = self.bytes[last];
            self.bytes[last] = 0;
            self.bytes.truncate(last);
            if b & 0xC0 != 0x80 {
                break;
            }
        }
        true
    }

    /// How many characters it holds (for erasing an echoed answer).
    fn chars(&self) -> usize {
        self.bytes.iter().filter(|b| **b & 0xC0 != 0x80).count()
    }

    fn clear(&mut self) {
        self.bytes.iter_mut().for_each(|b| *b = 0);
        self.bytes.clear();
    }

    fn into_secret(self) -> Result<SecretString, PromptError> {
        std::str::from_utf8(&self.bytes)
            .map(SecretString::new)
            .map_err(|_| not_utf8())
    }
}

/// Reads one line from `file` until Enter, editing as the module docs say.
#[cfg(unix)]
fn read_line(mut file: &std::fs::File, echo: bool, deadline: Instant) -> Result<Line, PromptError> {
    use std::io::Write;
    let mut line = Line::new();
    loop {
        match next_byte(file, deadline)? {
            b'\r' | b'\n' => return Ok(line),
            0x03 => return Err(PromptError::Cancelled),
            0x04 if line.bytes.is_empty() => return Err(PromptError::Cancelled),
            0x7f | 0x08 => {
                if line.erase() && echo {
                    file.write_all(b"\x08 \x08").map_err(PromptError::Io)?;
                }
            }
            0x15 => {
                let shown = line.chars();
                line.clear();
                if echo {
                    for _ in 0..shown {
                        file.write_all(b"\x08 \x08").map_err(PromptError::Io)?;
                    }
                }
            }
            0x1b => skip_escape(file, deadline)?,
            b if b < 0x20 => {}
            b => {
                if line.push(b) && echo {
                    file.write_all(&[b]).map_err(PromptError::Io)?;
                }
            }
        }
    }
}

/// An escape sequence after its ESC: CSI (`ESC [ … final`), SS3 (`ESC O x`), or a lone Escape
/// and the key after it — all skipped.
#[cfg(unix)]
fn skip_escape(file: &std::fs::File, deadline: Instant) -> Result<(), PromptError> {
    match next_byte(file, deadline)? {
        b'[' => loop {
            if (0x40..=0x7e).contains(&next_byte(file, deadline)?) {
                return Ok(());
            }
        },
        b'O' => next_byte(file, deadline).map(|_| ()),
        _ => Ok(()),
    }
}

/// The longest a read waits before it looks again for a signal another thread took (critic
/// finding 15), in milliseconds.
#[cfg(unix)]
const SLICE_MS: i32 = 250;

/// The next byte typed, waiting at most until `deadline`; `Cancelled` once a signal arrived
/// while the prompt read, whichever thread took it.
#[cfg(unix)]
fn next_byte(file: &std::fs::File, deadline: Instant) -> Result<u8, PromptError> {
    use std::os::fd::AsRawFd;
    let fd = file.as_raw_fd();
    loop {
        if restore::interrupted() {
            return Err(PromptError::Cancelled);
        }
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(PromptError::TimedOut);
        }
        let mut pfd = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        let ms = i32::try_from(left.as_millis())
            .unwrap_or(i32::MAX)
            .clamp(1, SLICE_MS);
        // SAFETY: one pollfd of ours, for the call's duration.
        let n = unsafe { libc::poll(&mut pfd, 1, ms) };
        if n < 0 {
            let e = io::Error::last_os_error();
            if e.kind() == io::ErrorKind::Interrupted {
                if restore::interrupted() {
                    return Err(PromptError::Cancelled);
                }
                continue;
            }
            return Err(PromptError::Io(e));
        }
        if n == 0 {
            continue;
        }
        let mut b = 0u8;
        // SAFETY: reads at most one byte into `b`.
        let r = unsafe { libc::read(fd, (&raw mut b).cast(), 1) };
        match r {
            1 => return Ok(b),
            0 => {
                return Err(PromptError::Io(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "the terminal closed",
                )))
            }
            _ => {
                let e = io::Error::last_os_error();
                match e.kind() {
                    io::ErrorKind::Interrupted if restore::interrupted() => {
                        return Err(PromptError::Cancelled)
                    }
                    io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock => continue,
                    _ => return Err(PromptError::Io(e)),
                }
            }
        }
    }
}

/// The terminal raw while a prompt reads (no echo, no canonical line, no signals), put back on
/// drop; while it lives the handler and the hook of [`restore`] can put it back too.
#[cfg(unix)]
struct TermGuard {
    fd: libc::c_int,
    saved: libc::termios,
}

#[cfg(unix)]
impl TermGuard {
    fn raw(fd: libc::c_int) -> Result<TermGuard, PromptError> {
        let mut saved = std::mem::MaybeUninit::<libc::termios>::uninit();
        // SAFETY: tcgetattr fills `saved` when it returns 0.
        if unsafe { libc::tcgetattr(fd, saved.as_mut_ptr()) } != 0 {
            return Err(PromptError::Io(io::Error::last_os_error()));
        }
        // SAFETY: filled by the successful call above.
        let saved = unsafe { saved.assume_init() };
        restore::arm(fd, &saved);
        let mut raw = saved;
        raw.c_lflag &= !(libc::ECHO | libc::ECHONL | libc::ICANON | libc::ISIG | libc::IEXTEN);
        raw.c_cc[libc::VMIN] = 1;
        raw.c_cc[libc::VTIME] = 0;
        // SAFETY: `raw` is a termios read from this fd, changed in flags only.
        if unsafe { libc::tcsetattr(fd, libc::TCSANOW, &raw) } != 0 {
            let e = io::Error::last_os_error();
            restore::disarm();
            return Err(PromptError::Io(e));
        }
        Ok(TermGuard { fd, saved })
    }
}

#[cfg(unix)]
impl Drop for TermGuard {
    fn drop(&mut self) {
        // MUTANT: the guard does not put the terminal back.
        let _ = (self.fd, &self.saved);
        restore::disarm();
    }
}

/// What puts the terminal back when the guard cannot: a signal handler and a panic hook, over
/// the termios the guard saved. Written only under [`PROMPT`](restore::PROMPT), before the
/// handlers are installed; read by them only while armed.
#[cfg(unix)]
mod restore {
    use std::cell::UnsafeCell;
    use std::mem::MaybeUninit;
    use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
    use std::sync::{Mutex, Once};

    /// The signals that put the terminal back before they act (SIGKILL and SIGSTOP cannot be
    /// caught). SIGQUIT ends the process, SIGTSTP from outside stops it (with `ISIG` cleared
    /// Ctrl-\ and Ctrl-Z are bytes the prompt skips); either would leave the terminal raw.
    const SIGNALS: [libc::c_int; 5] = [
        libc::SIGTERM,
        libc::SIGHUP,
        libc::SIGINT,
        libc::SIGQUIT,
        libc::SIGTSTP,
    ];

    /// One prompt at a time in this process.
    pub(super) static PROMPT: Mutex<()> = Mutex::new(());

    struct Saved {
        termios: UnsafeCell<MaybeUninit<libc::termios>>,
        old: UnsafeCell<[MaybeUninit<libc::sigaction>; 5]>,
        installed: [AtomicBool; 5],
    }

    // SAFETY: `termios` and `old` are written only by `arm`, under PROMPT: `termios` before
    // ARMED is set, each `old[i]` before its handler is installed; the handler and the hook
    // read them only after that.
    unsafe impl Sync for Saved {}

    static SAVED: Saved = Saved {
        termios: UnsafeCell::new(MaybeUninit::uninit()),
        old: UnsafeCell::new([const { MaybeUninit::uninit() }; 5]),
        installed: [const { AtomicBool::new(false) }; 5],
    };
    static ARMED: AtomicBool = AtomicBool::new(false);
    static FD: AtomicI32 = AtomicI32::new(-1);
    static INTERRUPTED: AtomicI32 = AtomicI32::new(0);
    static HOOK: Once = Once::new();

    /// Saves `saved` for `fd` and arms the restore, then installs the handlers (a signal this
    /// process ignores stays ignored) and, once, the panic hook. Everything a handler reads is
    /// in place before the first handler is (critic finding 15): a signal that arrives while
    /// they are installed is recorded, never cleared after the fact.
    pub(super) fn arm(fd: libc::c_int, saved: &libc::termios) {
        HOOK.call_once(|| {
            let previous = std::panic::take_hook();
            std::panic::set_hook(Box::new(move |info| {
                restore_now();
                previous(info);
            }));
        });
        // SAFETY: under PROMPT and not armed: nothing reads SAVED.termios meanwhile.
        unsafe { (*SAVED.termios.get()).write(*saved) };
        FD.store(fd, Ordering::SeqCst);
        INTERRUPTED.store(0, Ordering::SeqCst);
        ARMED.store(true, Ordering::SeqCst);
        // SAFETY: under PROMPT: each `old[i]` is written before its handler is installed, and
        // only that handler reads it; sigaction's pointers are to our own storage.
        unsafe {
            for (i, sig) in SIGNALS.iter().enumerate() {
                let old = &mut (*SAVED.old.get())[i];
                libc::sigaction(*sig, std::ptr::null(), old.as_mut_ptr());
                if old.assume_init_ref().sa_sigaction == libc::SIG_IGN {
                    SAVED.installed[i].store(false, Ordering::SeqCst);
                    continue;
                }
                let mut new: libc::sigaction = std::mem::zeroed();
                new.sa_sigaction = on_signal as extern "C" fn(libc::c_int) as libc::sighandler_t;
                libc::sigemptyset(&mut new.sa_mask);
                SAVED.installed[i].store(true, Ordering::SeqCst);
                libc::sigaction(*sig, &new, std::ptr::null_mut());
            }
        }
    }

    /// The prompt is over: the handlers go, the signals get their own actions back.
    pub(super) fn disarm() {
        ARMED.store(false, Ordering::SeqCst);
        for (i, sig) in SIGNALS.iter().enumerate() {
            if SAVED.installed[i].swap(false, Ordering::SeqCst) {
                // SAFETY: the action `arm` saved for this signal.
                unsafe {
                    libc::sigaction(*sig, (*SAVED.old.get())[i].as_ptr(), std::ptr::null_mut())
                };
            }
        }
        FD.store(-1, Ordering::SeqCst);
    }

    /// Puts the terminal back if a prompt has it raw: what the hook and the handler call.
    /// Whether it did. Async-signal-safe (atomics and `tcsetattr`).
    pub(super) fn restore_now() -> bool {
        if !ARMED.swap(false, Ordering::SeqCst) {
            return false;
        }
        // MUTANT: the handler does not put the terminal back.
        true
    }

    /// A signal arrived while a prompt read.
    pub(super) fn interrupted() -> bool {
        INTERRUPTED.load(Ordering::SeqCst) != 0
    }

    /// Only async-signal-safe calls: atomics, `tcsetattr`, `sigaction`, `raise`.
    extern "C" fn on_signal(sig: libc::c_int) {
        restore_now();
        if let Some(i) = SIGNALS.iter().position(|s| *s == sig) {
            if SAVED.installed[i].swap(false, Ordering::SeqCst) {
                // SAFETY: the action saved before this handler was installed.
                unsafe {
                    libc::sigaction(sig, (*SAVED.old.get())[i].as_ptr(), std::ptr::null_mut())
                };
            }
        }
        INTERRUPTED.store(sig, Ordering::SeqCst);
        // SAFETY: raise is async-signal-safe. The signal is blocked until this handler
        // returns, then delivered with the action it had before the prompt (by default: the
        // process ends; a handler of its own, e.g. `helper_interrupt`'s, runs).
        unsafe { libc::raise(sig) };
    }

    #[cfg(test)]
    pub(super) fn armed() -> bool {
        ARMED.load(Ordering::SeqCst)
    }

    /// Tests: what `on_signal` records when another thread takes the signal and the process
    /// goes on — the reading thread is told only through `interrupted()`.
    #[cfg(test)]
    pub(super) fn mark_interrupted(sig: libc::c_int) {
        restore_now();
        INTERRUPTED.store(sig, Ordering::SeqCst);
    }
}

// ---- Windows ----------------------------------------------------------------------------

/// T61: stdin is a console, and the console input can be opened.
#[cfg(windows)]
pub(crate) fn terminal_present() -> bool {
    use windows_sys::Win32::System::Console::{GetConsoleMode, GetStdHandle, STD_INPUT_HANDLE};
    let mut mode = 0;
    // SAFETY: GetStdHandle returns a handle (perhaps invalid); GetConsoleMode only writes `mode`.
    let console = unsafe { GetConsoleMode(GetStdHandle(STD_INPUT_HANDLE), &mut mode) } != 0;
    console
        && std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open("CONIN$")
            .is_ok()
}

/// The console's input and output, opened by name (never stdin / stderr).
#[cfg(windows)]
pub(crate) struct Tty {
    input: std::fs::File,
    output: std::fs::File,
}

#[cfg(windows)]
static PROMPT: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(windows)]
impl Tty {
    pub(crate) fn open() -> Result<Tty, PromptError> {
        let open = |name: &str| {
            std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(name)
                .map_err(PromptError::Io)
        };
        Ok(Tty {
            input: open("CONIN$")?,
            output: open("CONOUT$")?,
        })
    }

    pub(crate) fn say(&mut self, text: &str) -> io::Result<()> {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::System::Console::WriteConsoleW;
        let wide: Vec<u16> = text.encode_utf16().collect();
        let mut done = 0;
        while done < wide.len() {
            let chunk = &wide[done..];
            let mut written = 0u32;
            // SAFETY: writes from our buffer to the console output we hold.
            let ok = unsafe {
                WriteConsoleW(
                    self.output.as_raw_handle(),
                    chunk.as_ptr(),
                    u32::try_from(chunk.len()).unwrap_or(u32::MAX),
                    &mut written,
                    std::ptr::null(),
                )
            };
            if ok == 0 {
                return Err(io::Error::last_os_error());
            }
            done += written as usize;
        }
        Ok(())
    }

    pub(crate) fn read_secret(
        &mut self,
        prompt: &str,
        timeout: Duration,
    ) -> Result<SecretString, PromptError> {
        self.read(prompt, false, timeout)
    }

    pub(crate) fn read_answer(
        &mut self,
        prompt: &str,
        timeout: Duration,
    ) -> Result<String, PromptError> {
        self.read(prompt, true, timeout)
            .map(|s| s.expose().to_string())
    }

    fn read(
        &mut self,
        prompt: &str,
        echo: bool,
        timeout: Duration,
    ) -> Result<SecretString, PromptError> {
        use std::os::windows::io::AsRawHandle;
        let _one = PROMPT
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let guard = ConsoleGuard::raw(self.input.as_raw_handle())?;
        self.say(prompt).map_err(PromptError::Io)?;
        let line = self.read_line(echo, Instant::now() + timeout);
        drop(guard);
        let _ = self.say("\r\n");
        line
    }

    fn read_line(&mut self, echo: bool, deadline: Instant) -> Result<SecretString, PromptError> {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::Foundation::{WAIT_OBJECT_0, WAIT_TIMEOUT};
        use windows_sys::Win32::System::Console::ReadConsoleW;
        use windows_sys::Win32::System::Threading::WaitForSingleObject;
        let input = self.input.as_raw_handle();
        let mut units: Zeroizing<Vec<u16>> = Zeroizing::new(Vec::with_capacity(LINE_MAX));
        loop {
            // A console event (Ctrl-Break, a close) put the mode back: read no further.
            if console_restore::interrupted() {
                return Err(PromptError::Cancelled);
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(PromptError::TimedOut);
            }
            let ms = u32::try_from(left.as_millis()).unwrap_or(u32::MAX).min(250);
            // SAFETY: waits on the console input we hold.
            match unsafe { WaitForSingleObject(input, ms) } {
                WAIT_OBJECT_0 => {}
                WAIT_TIMEOUT => continue,
                _ => return Err(PromptError::Io(io::Error::last_os_error())),
            }
            let mut unit = 0u16;
            let mut read = 0u32;
            // SAFETY: reads at most one UTF-16 unit into `unit`.
            let ok = unsafe {
                ReadConsoleW(
                    input,
                    (&raw mut unit).cast(),
                    1,
                    &mut read,
                    std::ptr::null(),
                )
            };
            if ok == 0 {
                return Err(PromptError::Io(io::Error::last_os_error()));
            }
            if read == 0 {
                continue;
            }
            match unit {
                0x0d | 0x0a => {
                    return String::from_utf16(&units)
                        .map(SecretString::new)
                        .map_err(|_| not_utf8())
                }
                0x03 => return Err(PromptError::Cancelled),
                0x08 => {
                    // A character is one unit, or a surrogate pair.
                    if let Some(low) = units.pop() {
                        if (0xDC00..=0xDFFF).contains(&low) {
                            units.pop();
                        }
                        if echo {
                            let _ = self.say("\x08 \x08");
                        }
                    }
                }
                u if u < 0x20 => {}
                u => {
                    if units.len() < LINE_MAX {
                        units.push(u);
                        if echo {
                            let _ = self.say(&String::from_utf16_lossy(&[u]));
                        }
                    }
                }
            }
        }
    }
}

/// The console input with echo, line input and processed input off while a prompt reads, put
/// back on drop; while it lives [`console_restore`]'s handler and the panic hook can put it
/// back too.
#[cfg(windows)]
struct ConsoleGuard {
    handle: windows_sys::Win32::Foundation::HANDLE,
    saved: windows_sys::Win32::System::Console::CONSOLE_MODE,
}

#[cfg(windows)]
impl ConsoleGuard {
    fn raw(handle: windows_sys::Win32::Foundation::HANDLE) -> Result<ConsoleGuard, PromptError> {
        use windows_sys::Win32::System::Console::{
            GetConsoleMode, SetConsoleMode, ENABLE_ECHO_INPUT, ENABLE_LINE_INPUT,
            ENABLE_PROCESSED_INPUT,
        };
        let mut saved = 0;
        // SAFETY: reads the mode of the console input we hold.
        if unsafe { GetConsoleMode(handle, &mut saved) } == 0 {
            return Err(PromptError::Io(io::Error::last_os_error()));
        }
        console_restore::arm(handle, saved);
        let raw = saved & !(ENABLE_ECHO_INPUT | ENABLE_LINE_INPUT | ENABLE_PROCESSED_INPUT);
        // SAFETY: sets the mode of the console input we hold, from the one read above.
        if unsafe { SetConsoleMode(handle, raw) } == 0 {
            let e = io::Error::last_os_error();
            console_restore::disarm();
            return Err(PromptError::Io(e));
        }
        Ok(ConsoleGuard { handle, saved })
    }
}

#[cfg(windows)]
impl Drop for ConsoleGuard {
    fn drop(&mut self) {
        use windows_sys::Win32::System::Console::SetConsoleMode;
        // SAFETY: the mode this guard read from this handle.
        unsafe { SetConsoleMode(self.handle, self.saved) };
        console_restore::disarm();
    }
}

/// What puts the console back when the guard cannot (G7, T61; critic finding 8): a console
/// control handler, registered only while a prompt reads, for Ctrl-Break and the console
/// closing (processed input is off, so Ctrl-C is a byte the prompt reads), and the panic hook.
/// The handler puts the saved mode back and returns FALSE, so the handler registered before
/// it — `helper_interrupt`'s, or the system's, which ends the process — still acts; a read
/// that goes on sees [`interrupted`](console_restore::interrupted) and ends as cancelled.
#[cfg(windows)]
mod console_restore {
    use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicU32, Ordering};
    use std::sync::Once;

    use windows_sys::core::BOOL;
    use windows_sys::Win32::Foundation::{FALSE, HANDLE, TRUE};
    use windows_sys::Win32::System::Console::{
        SetConsoleCtrlHandler, SetConsoleMode, CONSOLE_MODE,
    };

    static ARMED: AtomicBool = AtomicBool::new(false);
    static INTERRUPTED: AtomicBool = AtomicBool::new(false);
    static INPUT: AtomicPtr<core::ffi::c_void> = AtomicPtr::new(std::ptr::null_mut());
    static MODE: AtomicU32 = AtomicU32::new(0);
    static HOOK: Once = Once::new();

    /// Saves `mode` for `input` and arms the restore, then registers the handler and, once,
    /// the panic hook.
    pub(super) fn arm(input: HANDLE, mode: CONSOLE_MODE) {
        HOOK.call_once(|| {
            let previous = std::panic::take_hook();
            std::panic::set_hook(Box::new(move |info| {
                restore_now();
                previous(info);
            }));
        });
        INPUT.store(input, Ordering::SeqCst);
        MODE.store(mode, Ordering::SeqCst);
        INTERRUPTED.store(false, Ordering::SeqCst);
        ARMED.store(true, Ordering::SeqCst);
        // SAFETY: registers a plain `extern "system"` function; `disarm` removes it.
        unsafe { SetConsoleCtrlHandler(Some(on_event), TRUE) };
    }

    /// The prompt is over: the handler goes.
    pub(super) fn disarm() {
        ARMED.store(false, Ordering::SeqCst);
        // SAFETY: removes the function `arm` registered.
        unsafe { SetConsoleCtrlHandler(Some(on_event), FALSE) };
        INPUT.store(std::ptr::null_mut(), Ordering::SeqCst);
    }

    /// Puts the saved mode back if a prompt has the console raw; whether it did.
    pub(super) fn restore_now() -> bool {
        if !ARMED.swap(false, Ordering::SeqCst) {
            return false;
        }
        // SAFETY: armed: the mode `arm` saved, for the console input it saved.
        unsafe { SetConsoleMode(INPUT.load(Ordering::SeqCst), MODE.load(Ordering::SeqCst)) };
        true
    }

    /// A console event arrived while a prompt read.
    pub(super) fn interrupted() -> bool {
        INTERRUPTED.load(Ordering::SeqCst)
    }

    /// Runs on a thread the system creates for the event: the console back, and the next
    /// handler decides what the event does.
    unsafe extern "system" fn on_event(_ctrl_type: u32) -> BOOL {
        restore_now();
        INTERRUPTED.store(true, Ordering::SeqCst);
        FALSE
    }
}

// ---- Tests ------------------------------------------------------------------------------

/// On a pseudo-terminal this test process opens itself and never makes its controlling one:
/// nothing here reads `/dev/tty` or the keyboard of whoever runs the tests (rule 6).
#[cfg(all(test, unix))]
mod tests {
    use std::io::{Read, Write};
    use std::os::fd::{AsRawFd, FromRawFd};

    use super::*;

    /// (master, the slave as a `Tty`, a second handle on the slave to read its flags with).
    fn pty() -> (std::fs::File, Tty, std::fs::File) {
        let (mut m, mut s) = (-1, -1);
        // SAFETY: openpty writes two fds; no name, termios or size is passed.
        let rc = unsafe {
            libc::openpty(
                &mut m,
                &mut s,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        assert_eq!(rc, 0, "openpty: {}", io::Error::last_os_error());
        // SAFETY: the two fds openpty just returned, each owned once.
        let (master, slave) =
            unsafe { (std::fs::File::from_raw_fd(m), std::fs::File::from_raw_fd(s)) };
        let probe = slave.try_clone().unwrap();
        (master, Tty { file: slave }, probe)
    }

    fn lflag(file: &std::fs::File) -> libc::tcflag_t {
        let mut t = std::mem::MaybeUninit::<libc::termios>::uninit();
        // SAFETY: tcgetattr fills `t` when it returns 0.
        assert_eq!(
            unsafe { libc::tcgetattr(file.as_raw_fd(), t.as_mut_ptr()) },
            0
        );
        // SAFETY: filled above.
        unsafe { t.assume_init() }.c_lflag
    }

    struct Typed {
        result: Result<String, String>,
        shown: String,
        restored: bool,
    }

    /// Asks `Q: ` (a secret, or an `echo`ed answer), types `keys` once the prompt shows, and
    /// returns what was read, what the terminal showed, and whether ECHO and ICANON are back.
    fn typed(echo: bool, keys: &[u8], timeout: Duration) -> Typed {
        let (mut master, mut tty, probe) = pty();
        let before = lflag(&probe);
        let reader = std::thread::spawn(move || {
            let r = if echo {
                tty.read_answer("Q: ", timeout)
            } else {
                tty.read_secret("Q: ", timeout)
                    .map(|s| s.expose().to_string())
            };
            r.map_err(|e| format!("{e:?}"))
        });
        let mut shown = Vec::new();
        let mut buf = [0u8; 512];
        while !String::from_utf8_lossy(&shown).contains("Q: ") {
            let n = master.read(&mut buf).unwrap();
            shown.extend_from_slice(&buf[..n]);
        }
        master.write_all(keys).unwrap();
        let result = reader.join().unwrap();
        // SAFETY: fcntl on our own master fd.
        unsafe { libc::fcntl(master.as_raw_fd(), libc::F_SETFL, libc::O_NONBLOCK) };
        while let Ok(n) = master.read(&mut buf) {
            if n == 0 {
                break;
            }
            shown.extend_from_slice(&buf[..n]);
        }
        let after = lflag(&probe);
        let back = libc::ECHO | libc::ICANON;
        Typed {
            result,
            shown: String::from_utf8_lossy(&shown).into_owned(),
            restored: before & back == back && after & back == back,
        }
    }

    const LONG: Duration = Duration::from_secs(10);

    /// One test at a time: what `restore` keeps — armed or not, a signal taken, the handlers
    /// installed, the panic hook — is the process's, and a test reads it after its prompt
    /// ended, when another test's prompt (or a failing test's panic hook) could change it.
    static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn serial() -> std::sync::MutexGuard<'static, ()> {
        SERIAL
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    #[test]
    fn a_secret_is_read_without_echo_and_the_terminal_is_put_back() {
        let _serial = serial();
        let t = typed(false, b"s3cret phrase\r", LONG);
        assert_eq!(t.result.as_deref(), Ok("s3cret phrase"));
        assert!(!t.shown.contains("s3cret"), "{:?}", t.shown);
        assert!(t.restored);
        assert!(!restore::armed());
    }

    #[test]
    fn backspace_and_ctrl_u_edit_the_line() {
        let _serial = serial();
        let t = typed(false, b"ab\x7fc\x15xyz\x7f\r", LONG);
        assert_eq!(t.result.as_deref(), Ok("xy"));
    }

    #[test]
    fn a_multibyte_character_is_erased_whole() {
        let _serial = serial();
        let t = typed(false, "é\u{1F600}x\x7f\x7f\x7fy\r".as_bytes(), LONG);
        assert_eq!(t.result.as_deref(), Ok("y"));
    }

    /// G7: Ctrl-C is a byte (ISIG cleared) that cancels the prompt, and the terminal comes back.
    #[test]
    fn ctrl_c_cancels_and_the_terminal_is_put_back() {
        let _serial = serial();
        let t = typed(false, b"ab\x03", LONG);
        assert_eq!(t.result, Err("Cancelled".to_string()));
        assert!(t.restored);
    }

    #[test]
    fn ctrl_d_cancels_only_an_empty_line() {
        let _serial = serial();
        assert_eq!(
            typed(false, b"\x04", LONG).result,
            Err("Cancelled".to_string())
        );
        assert_eq!(typed(false, b"a\x04b\r", LONG).result.as_deref(), Ok("ab"));
    }

    #[test]
    fn an_arrow_key_is_skipped() {
        let _serial = serial();
        assert_eq!(
            typed(false, b"a\x1b[Db\x1bOAc\r", LONG).result.as_deref(),
            Ok("abc")
        );
    }

    #[test]
    fn nobody_answering_times_out_and_the_terminal_is_put_back() {
        let _serial = serial();
        let started = Instant::now();
        let t = typed(false, b"", Duration::from_millis(300));
        assert_eq!(t.result, Err("TimedOut".to_string()));
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "{:?}",
            started.elapsed()
        );
        assert!(t.restored);
    }

    #[test]
    fn an_answer_is_shown_as_it_is_typed() {
        let _serial = serial();
        let t = typed(true, b"yes\r", LONG);
        assert_eq!(t.result.as_deref(), Ok("yes"));
        assert!(t.shown.contains("yes"), "{:?}", t.shown);
    }

    #[test]
    fn a_line_never_grows_past_its_limit() {
        let _serial = serial();
        let mut keys = vec![b'a'; LINE_MAX + 10];
        keys.push(b'\r');
        assert_eq!(
            typed(false, &keys, LONG).result.map(|s| s.len()),
            Ok(LINE_MAX)
        );
    }

    /// G7: the panic hook's path puts back a terminal a prompt left raw.
    #[test]
    fn the_hook_path_puts_back_a_raw_terminal() {
        let _serial = serial();
        let (_master, tty, probe) = pty();
        let _one = restore::PROMPT
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let guard = TermGuard::raw(tty.file.as_raw_fd()).unwrap();
        assert_eq!(lflag(&probe) & libc::ECHO, 0, "raw while the prompt reads");
        assert!(restore::restore_now(), "armed");
        assert_ne!(lflag(&probe) & libc::ECHO, 0, "echo back");
        assert!(!restore::restore_now(), "once");
        drop(guard);
    }

    /// Critic finding 15: a signal another thread took (its handler put the terminal back, the
    /// process goes on) ends the read as cancelled within a slice — the prompt never reads on
    /// at a terminal that echoes again.
    #[test]
    fn a_signal_taken_by_another_thread_ends_the_read() {
        let _serial = serial();
        let (mut master, mut tty, probe) = pty();
        let started = Instant::now();
        let reader = std::thread::spawn(move || {
            tty.read_secret("Q: ", LONG)
                .map(|s| s.expose().to_string())
                .map_err(|e| format!("{e:?}"))
        });
        let mut shown = Vec::new();
        let mut buf = [0u8; 512];
        while !String::from_utf8_lossy(&shown).contains("Q: ") {
            let n = master.read(&mut buf).unwrap();
            shown.extend_from_slice(&buf[..n]);
        }
        // The prompt shows only while the read holds PROMPT and is armed: no other test's
        // read is running, so the mark reaches this one alone.
        restore::mark_interrupted(libc::SIGTERM);
        assert_eq!(reader.join().unwrap(), Err("Cancelled".to_string()));
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "{:?}",
            started.elapsed()
        );
        assert_ne!(lflag(&probe) & libc::ECHO, 0, "echo back");
        assert!(!restore::armed());
    }

    /// Critic S10 and finding 14: the parent by its name, its pid and, on Linux and macOS, its
    /// executable's path.
    #[test]
    fn the_parent_is_named() {
        let parent = parent_process().unwrap();
        assert!(!parent.name.is_empty());
        // SAFETY: getppid takes nothing and cannot fail.
        assert_eq!(parent.pid as libc::pid_t, unsafe { libc::getppid() });
        if cfg!(any(target_os = "linux", target_os = "macos")) {
            let exe = parent
                .exe
                .expect("Linux and macOS name the parent's executable");
            assert!(exe.is_absolute(), "{exe:?}");
        }
    }
}
