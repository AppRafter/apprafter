// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! What the holder relies on that only the CI runners can show (D.4a §9; holder §12). Each
//! probe asserts its fact; a red probe is a decision for the plan (`2026-10-13-d4a-e-holder-
//! unlock.md`, Task 6), never a skip. The probes run no `apprafter`: they are this test binary
//! re-run for an inner probe, so they need no sandbox and reach no store.
#![cfg(any(windows, target_os = "macos"))]

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

const INNER: &str = "APPRAFTER_PROBE_INNER";

/// This test binary, re-run for one inner probe.
fn rerun(inner: &str) -> Command {
    let mut c = Command::new(std::env::current_exe().expect("this test binary"));
    c.args([
        "--exact",
        inner,
        "--ignored",
        "--nocapture",
        "--test-threads=1",
    ])
    .env(INNER, "1");
    c
}

/// A child's next "probe: " line, the text after the marker. libtest prints `test <name> ... `
/// before a test runs, so with one thread and `--nocapture` an inner probe's first line starts
/// with that prefix (GOTCHA-217): the marker is looked for within the line.
fn next_line(r: &mut impl BufRead) -> String {
    let mut l = String::new();
    loop {
        l.clear();
        assert!(
            r.read_line(&mut l).unwrap() > 0,
            "the inner probe ended early"
        );
        if let Some((_, x)) = l.trim_end().split_once("probe: ") {
            return x.to_string();
        }
    }
}

fn inner() {
    assert!(
        std::env::var_os(INNER).is_some(),
        "run only by holder_probe's outer tests: an `--ignored` run reaches it alone and fails \
         here on purpose"
    );
}

/// A child the test holds by its stdin and reads by its stdout.
fn held(mut c: Command) -> (Child, BufReader<std::process::ChildStdout>) {
    let mut child = c
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("start the inner probe");
    let out = BufReader::new(child.stdout.take().unwrap());
    (child, out)
}

fn release(mut child: Child) {
    drop(child.stdin.take());
    let _ = child.wait_timeout_or_kill();
}

trait WaitTimeout {
    fn wait_timeout_or_kill(&mut self) -> std::io::Result<()>;
}

impl WaitTimeout for Child {
    /// Wait up to 10 s, then kill: a probe never hangs the suite.
    fn wait_timeout_or_kill(&mut self) -> std::io::Result<()> {
        for _ in 0..200 {
            if self.try_wait()?.is_some() {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        self.kill()?;
        self.wait().map(|_| ())
    }
}

// ---------------------------------------------------------------------
// Windows
// ---------------------------------------------------------------------

/// P1: a console of its own works in the runner's session, and a detached child attaches to
/// it and is listed on it.
#[cfg(windows)]
#[test]
fn p1_windows_a_console_of_its_own_and_a_detached_child_attached_to_it() {
    use std::os::windows::process::CommandExt as _;
    use windows_sys::Win32::System::Threading::CREATE_NEW_CONSOLE;
    let out = rerun("inner_console_owner")
        .creation_flags(CREATE_NEW_CONSOLE)
        .stdout(Stdio::piped())
        .output()
        .expect("a process with a console of its own");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success() && stdout.contains("probe: attached, both listed"),
        "{stdout}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[cfg(windows)]
#[test]
#[ignore = "runs only inside p1_windows_a_console_of_its_own_and_a_detached_child_attached_to_it"]
fn inner_console_owner() {
    use std::os::windows::process::CommandExt as _;
    use windows_sys::Win32::System::Threading::DETACHED_PROCESS;
    inner();
    let mine = console_list();
    assert!(
        mine.contains(&std::process::id()),
        "a console of its own lists this process: {mine:?}"
    );
    let out = rerun("inner_attacher")
        .creation_flags(DETACHED_PROCESS)
        .env("PROBE_PARENT", std::process::id().to_string())
        .stdout(Stdio::piped())
        .output()
        .unwrap();
    print!("{}", String::from_utf8_lossy(&out.stdout));
}

#[cfg(windows)]
#[test]
#[ignore = "runs only inside inner_console_owner"]
fn inner_attacher() {
    use windows_sys::Win32::System::Console::{AttachConsole, ATTACH_PARENT_PROCESS};
    inner();
    // SAFETY: attaches this detached process to its parent's console.
    let attached = unsafe { AttachConsole(ATTACH_PARENT_PROCESS) } != 0;
    let parent: u32 = std::env::var("PROBE_PARENT").unwrap().parse().unwrap();
    let list = console_list();
    if attached && list.contains(&parent) && list.contains(&std::process::id()) {
        println!("probe: attached, both listed");
    } else {
        println!("probe: attached={attached} list={list:?} parent={parent}");
    }
}

#[cfg(windows)]
fn console_list() -> Vec<u32> {
    use windows_sys::Win32::System::Console::GetConsoleProcessList;
    let mut pids = vec![0u32; 64];
    // SAFETY: writes at most `pids.len()` pids.
    let n = unsafe { GetConsoleProcessList(pids.as_mut_ptr(), pids.len() as u32) } as usize;
    pids.truncate(n.min(pids.len()));
    pids
}

/// P2: `GetNamedPipeServerProcessId` on a client's handle names the server's pid.
#[cfg(windows)]
#[test]
fn p2_windows_the_server_pid_from_a_client_handle() {
    use windows_sys::Win32::Foundation::{
        CloseHandle, GENERIC_READ, GENERIC_WRITE, INVALID_HANDLE_VALUE,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FILE_FLAG_OVERLAPPED, OPEN_EXISTING, SECURITY_IDENTIFICATION,
        SECURITY_SQOS_PRESENT,
    };
    use windows_sys::Win32::System::Pipes::GetNamedPipeServerProcessId;
    let (child, mut out) = held(rerun("inner_pipe_server"));
    let name = next_line(&mut out);
    let wide: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
    // SAFETY: opens the probe's pipe as the holder's client will (Task 7).
    let h = unsafe {
        CreateFileW(
            wide.as_ptr(),
            GENERIC_READ | GENERIC_WRITE,
            0,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_OVERLAPPED | SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION,
            std::ptr::null_mut(),
        )
    };
    assert_ne!(
        h,
        INVALID_HANDLE_VALUE,
        "{}",
        std::io::Error::last_os_error()
    );
    let mut server = 0u32;
    // SAFETY: the client handle above; writes one pid.
    let ok = unsafe { GetNamedPipeServerProcessId(h, &mut server) } != 0;
    let err = std::io::Error::last_os_error();
    // SAFETY: the handle above, closed once.
    unsafe { CloseHandle(h) };
    let pid = child.id();
    release(child);
    assert!(
        ok && server == pid,
        "server pid {server} (ok={ok}, {err}) for the server {pid}"
    );
}

#[cfg(windows)]
#[test]
#[ignore = "runs only inside p2_windows_the_server_pid_from_a_client_handle"]
fn inner_pipe_server() {
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::PIPE_ACCESS_DUPLEX;
    use windows_sys::Win32::System::Pipes::{
        ConnectNamedPipe, CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_TYPE_BYTE, PIPE_WAIT,
    };
    inner();
    let name = format!(r"\\.\pipe\apprafter-probe-{}", std::process::id());
    let wide: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
    // SAFETY: a pipe of this probe's own; the default DACL serves a same-user probe.
    let h = unsafe {
        CreateNamedPipeW(
            wide.as_ptr(),
            PIPE_ACCESS_DUPLEX,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
            1,
            512,
            512,
            0,
            std::ptr::null(),
        )
    };
    assert_ne!(h, INVALID_HANDLE_VALUE);
    println!("probe: {name}");
    std::io::stdout().flush().unwrap();
    // SAFETY: blocks until the outer test connects (or connected first).
    unsafe { ConnectNamedPipe(h, std::ptr::null_mut()) };
    // Held until the outer test closes this probe's stdin.
    let _ = std::io::stdin().lines().count();
    // SAFETY: the pipe above.
    unsafe { CloseHandle(h) };
}

/// P3, pinned to what `windows-latest` showed (Task 6 Step 6; Deviations 9's pattern): the
/// hosted runner's token is elevated, with `SeDebugPrivilege` enabled, so there a process DACL
/// cannot keep a test out of the holder. The Windows holder tests therefore read the holder's
/// process and thread DACLs back (`READ_CONTROL`, which `OWNER RIGHTS` grants) rather than
/// assert that `PROCESS_VM_READ` and `THREAD_GET_CONTEXT` are refused; the refusals, from a
/// non-elevated shell, are rows of the owner's Windows walk. A runner whose token has it
/// disabled fails this probe on purpose: the refusals can then be asserted here.
#[cfg(windows)]
#[test]
fn p3_windows_se_debug_privilege_is_enabled_on_the_runner() {
    use windows_sys::Win32::Foundation::{CloseHandle, LUID};
    use windows_sys::Win32::Security::{
        GetTokenInformation, LookupPrivilegeValueW, TokenPrivileges, SE_PRIVILEGE_ENABLED,
        TOKEN_PRIVILEGES, TOKEN_QUERY,
    };
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
    let mut token = std::ptr::null_mut();
    // SAFETY: this process's token, closed below.
    assert_ne!(
        unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) },
        0
    );
    let mut len = 0u32;
    // SAFETY: a size query, then the read into a buffer of that size.
    unsafe { GetTokenInformation(token, TokenPrivileges, std::ptr::null_mut(), 0, &mut len) };
    let mut buf = vec![0u64; (len as usize).div_ceil(8)];
    assert_ne!(
        unsafe {
            GetTokenInformation(
                token,
                TokenPrivileges,
                buf.as_mut_ptr().cast(),
                len,
                &mut len,
            )
        },
        0
    );
    let name: Vec<u16> = "SeDebugPrivilege".encode_utf16().chain(Some(0)).collect();
    let mut luid = LUID {
        LowPart: 0,
        HighPart: 0,
    };
    // SAFETY: looks the privilege's LUID up on this machine.
    assert_ne!(
        unsafe { LookupPrivilegeValueW(std::ptr::null(), name.as_ptr(), &mut luid) },
        0
    );
    // SAFETY: the buffer holds a TOKEN_PRIVILEGES with `PrivilegeCount` entries.
    let enabled = unsafe {
        let tp = &*buf.as_ptr().cast::<TOKEN_PRIVILEGES>();
        let all = std::slice::from_raw_parts(tp.Privileges.as_ptr(), tp.PrivilegeCount as usize);
        all.iter().any(|p| {
            p.Luid.LowPart == luid.LowPart
                && p.Luid.HighPart == luid.HighPart
                && p.Attributes & SE_PRIVILEGE_ENABLED != 0
        })
    };
    // SAFETY: the token above.
    unsafe { CloseHandle(token) };
    assert!(
        enabled,
        "SeDebugPrivilege is not enabled here: a DACL keeps this process out, so the holder \
         tests may assert the refusals themselves (Task 6 Step 6, P3)"
    );
}

// ---------------------------------------------------------------------
// macOS
// ---------------------------------------------------------------------

/// P4: `LOCAL_PEERPID` on the client's socket names the server's pid.
#[cfg(target_os = "macos")]
#[test]
fn p4_macos_local_peerpid_on_a_client_socket_names_the_server() {
    use std::os::fd::AsRawFd;
    use std::os::unix::net::UnixStream;
    let dir = tempfile::TempDir::new_in("/tmp").unwrap();
    let path = dir.path().join("p.sock");
    let (child, mut out) = held({
        let mut c = rerun("inner_unix_server");
        c.env("PROBE_SOCKET", &path);
        c
    });
    assert_eq!(next_line(&mut out), "listening");
    let conn = UnixStream::connect(&path).unwrap();
    let mut pid: libc::pid_t = 0;
    let mut len = std::mem::size_of::<libc::pid_t>() as libc::socklen_t;
    // SAFETY: LOCAL_PEERPID writes one pid_t.
    let rc = unsafe {
        libc::getsockopt(
            conn.as_raw_fd(),
            libc::SOL_LOCAL,
            libc::LOCAL_PEERPID,
            (&raw mut pid).cast(),
            &mut len,
        )
    };
    let server = child.id();
    drop(conn);
    release(child);
    assert_eq!((rc, pid as u32), (0, server));
}

#[cfg(target_os = "macos")]
#[test]
#[ignore = "runs only inside p4_macos_local_peerpid_on_a_client_socket_names_the_server"]
fn inner_unix_server() {
    inner();
    let l =
        std::os::unix::net::UnixListener::bind(std::env::var_os("PROBE_SOCKET").unwrap()).unwrap();
    println!("probe: listening");
    std::io::stdout().flush().unwrap();
    let _held = l.accept().unwrap();
    let _ = std::io::stdin().lines().count();
}

/// P5: `proc_pidpath` reads the image path of a process that called `PT_DENY_ATTACH`.
#[cfg(target_os = "macos")]
#[test]
fn p5_macos_proc_pidpath_reads_a_process_that_denies_attach() {
    let (child, mut out) = held(rerun("inner_denier"));
    assert_eq!(next_line(&mut out), "denied");
    let mut buf = vec![0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
    // SAFETY: proc_pidpath writes at most `buf.len()` bytes.
    let n =
        unsafe { libc::proc_pidpath(child.id() as i32, buf.as_mut_ptr().cast(), buf.len() as u32) };
    release(child);
    assert!(n > 0, "{}", std::io::Error::last_os_error());
    let got = std::path::PathBuf::from(String::from_utf8_lossy(&buf[..n as usize]).into_owned());
    assert_eq!(
        std::fs::canonicalize(got).unwrap(),
        std::fs::canonicalize(std::env::current_exe().unwrap()).unwrap()
    );
}

#[cfg(target_os = "macos")]
#[test]
#[ignore = "runs only inside p5_macos_proc_pidpath_reads_a_process_that_denies_attach"]
fn inner_denier() {
    inner();
    // SAFETY: PT_DENY_ATTACH on this probe process alone.
    let rc = unsafe { libc::ptrace(libc::PT_DENY_ATTACH, 0, std::ptr::null_mut(), 0) };
    assert_eq!(rc, 0);
    println!("probe: denied");
    std::io::stdout().flush().unwrap();
    let _ = std::io::stdin().lines().count();
}
