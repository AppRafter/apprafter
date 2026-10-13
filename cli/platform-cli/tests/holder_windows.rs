// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The unlock holder on Windows (D.4a T51–T54, T59; holder §9.3). CI only (`windows-latest`:
//! agents cannot build msvc, GOTCHA-75). A "terminal" is a console this test makes with
//! `CREATE_NEW_CONSOLE` for its inner `leader` (this test binary re-run), which starts the holder
//! as the CLI will and runs the scenario's commands on its console (a child inherits it) or on a
//! console of their own. No `apprafter` command runs: only `apprafter __holder`, keyed by a
//! scratch store's id and root, so no test can reach a person's holder.
#![cfg(windows)]

mod common;

use std::io::{BufRead, BufReader, Write};
use std::os::windows::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use apprafter::holder::client::{HolderClient, HolderError, IO_TIMEOUT};
use apprafter::holder::endpoint::{pipe_name, EndpointId};
use apprafter::holder::protocol::{HolderItem, InitHeader, StoreBinding};
use apprafter::holder::spawn::{start_with, HolderStarted};
use apprafter::holder::watch::STORE_LOOK;
use apprafter_core::secrets::testing::locked_store;
use apprafter_core::secrets::{read_store_file, write_store_file, SecretsMode, StoreFile};
use apprafter_core::vault::{KdfPolicy, RecoveryKey, SlotSecret, VaultFile};
use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, SetHandleInformation, ERROR_ACCESS_DENIED, ERROR_PIPE_BUSY,
    GENERIC_READ, GENERIC_WRITE, HANDLE_FLAG_INHERIT, INVALID_HANDLE_VALUE, WAIT_OBJECT_0,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, OPEN_EXISTING, PIPE_ACCESS_DUPLEX, READ_CONTROL, WRITE_DAC,
};
use windows_sys::Win32::System::Console::{
    AttachConsole, FreeConsole, GenerateConsoleCtrlEvent, GetStdHandle, SetConsoleCtrlHandler,
    CTRL_C_EVENT, STD_ERROR_HANDLE, STD_OUTPUT_HANDLE,
};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Thread32First, Thread32Next, TH32CS_SNAPTHREAD, THREADENTRY32,
};
use windows_sys::Win32::System::Pipes::{
    CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_TYPE_BYTE, PIPE_WAIT,
};
use windows_sys::Win32::System::Threading::{
    OpenProcess, OpenThread, TerminateProcess, WaitForSingleObject, CREATE_NEW_CONSOLE,
    PROCESS_SYNCHRONIZE, PROCESS_TERMINATE, PROCESS_VM_READ, THREAD_GET_CONTEXT,
    THREAD_SET_CONTEXT, THREAD_SUSPEND_RESUME,
};

use common::spawn::Bin;

const TOKEN: &str = "a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1";
const INNER: &str = "APPRAFTER_HOLDER_INNER";

struct Scratch {
    _dir: tempfile::TempDir,
    root: PathBuf,
    recovery: String,
}

fn scratch() -> Scratch {
    let dir = tempfile::tempdir().unwrap();
    let locked = locked_store(dir.path(), &[("prod", Some(TOKEN))]);
    let recovery = locked
        .recovery_key()
        .expose()
        .lines()
        .nth(1)
        .unwrap()
        .to_string();
    Scratch {
        root: std::fs::canonicalize(dir.path()).unwrap(),
        _dir: dir,
        recovery,
    }
}

/// This test binary, re-run for `inner` in the scenario's environment.
fn rerun(inner: &str, s: &Scratch, scenario: &str) -> Command {
    let mut c = Command::new(std::env::current_exe().unwrap());
    c.args([
        "--exact",
        inner,
        "--ignored",
        "--nocapture",
        "--test-threads=1",
    ])
    .env(INNER, "1")
    .env("HOLDER_W_STORE", &s.root)
    .env("HOLDER_W_RECOVERY", &s.recovery)
    .env("HOLDER_W_SCENARIO", scenario);
    c
}

/// What an inner test said on `line`: the text after "holder: ". libtest prints `test <name> ... `
/// before a test runs and its result after, so with one thread and `--nocapture` an inner test's
/// first line starts with that prefix (GOTCHA-217): the marker is looked for within the line.
fn said(line: &str) -> Option<&str> {
    line.trim_end().split_once("holder: ").map(|(_, said)| said)
}

fn lines(out: &[u8]) -> Vec<String> {
    String::from_utf8_lossy(out)
        .lines()
        .filter_map(said)
        .map(str::to_string)
        .collect()
}

/// The scenario, played by a leader on a console of its own, to its end.
fn on_a_console(s: &Scratch, scenario: &str) -> Vec<String> {
    let out = rerun("inner_leader", s, scenario)
        .creation_flags(CREATE_NEW_CONSOLE)
        .stdout(Stdio::piped())
        .output()
        .expect("a leader with a console of its own");
    lines(&out.stdout)
}

/// A leader the test reads line by line and lets go on (a line on its stdin).
fn live(s: &Scratch, scenario: &str) -> (Child, mpsc::Receiver<String>) {
    let mut child = rerun("inner_leader", s, scenario)
        .creation_flags(CREATE_NEW_CONSOLE)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let out = BufReader::new(child.stdout.take().unwrap());
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for l in out.lines().map_while(Result::ok) {
            if let Some(x) = said(&l) {
                let _ = tx.send(x.to_string());
            }
        }
    });
    (child, rx)
}

fn next(rx: &mpsc::Receiver<String>) -> String {
    rx.recv_timeout(Duration::from_secs(30))
        .expect("the leader's next line")
}

fn started_pid(line: &str) -> u32 {
    line.strip_prefix("started ")
        .unwrap_or_else(|| panic!("{line}"))
        .parse()
        .unwrap()
}

fn exits_within(pid: u32, ms: u32) -> bool {
    // SAFETY: a wait-only handle on the holder, closed below.
    let h = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, pid) };
    if h.is_null() {
        return true;
    }
    // SAFETY: the handle above.
    let r = unsafe { WaitForSingleObject(h, ms) };
    // SAFETY: as above.
    unsafe { CloseHandle(h) };
    r == WAIT_OBJECT_0
}

/// T51: a command on the holder's console gets the token; `Status` reports the window and the
/// protections (the process DACL and the locked page); `End` stops the holder.
#[test]
fn windows_a_command_on_the_holders_console_is_served() {
    let s = scratch();
    let got = on_a_console(&s, "serve");
    started_pid(&got[0]);
    assert_eq!(got[1], format!("item {TOKEN}"));
    assert!(
        got[2].starts_with("status left=")
            && got[2].ends_with(" nd=None da=None core=true mem=true dacl=Some(true)"),
        "{}",
        got[2]
    );
    assert_eq!(got[3], "ended");
}

/// T51: a process on another console, at the holder's very pipe, is refused.
#[test]
fn windows_another_console_is_not_this_terminal() {
    let s = scratch();
    let got = on_a_console(&s, "other_console");
    assert_eq!(&got[1..], ["refused not_this_terminal", "ended"]);
}

/// ADR 0068 Risks "The Windows console binding", pinned (T51): any process of the same user can
/// attach itself to the unlocked console and is then served. A fix flips this test on purpose.
#[test]
fn windows_console_binding_admits_a_process_that_attaches_itself() {
    let s = scratch();
    let got = on_a_console(&s, "attach");
    assert_eq!(&got[1..], [format!("item {TOKEN}"), "ended".to_string()]);
}

/// T52, holder finding 7: once every other process left its console the holder leaves too, so a
/// classic console window closes after its shell exits.
#[test]
fn windows_the_holder_leaves_a_console_it_is_alone_on() {
    let s = scratch();
    let got = on_a_console(&s, "alone");
    let pid = started_pid(&got[0]);
    assert!(
        exits_within(pid, 3000),
        "the holder outlived its console's last other process"
    );
}

/// T56, team decision 2026-10-13: the holder watches its store on Windows too — `lock off`'s
/// last two steps (the vault deleted, then `files`) end it within its look (`STORE_LOOK`),
/// while its console and its window go on.
#[test]
fn windows_the_holder_ends_within_its_look_once_its_store_is_not_locked() {
    let s = scratch();
    let (mut leader, rx) = live(&s, "pause");
    let pid = started_pid(&next(&rx));
    next(&rx);
    assert!(!exits_within(pid, 0), "the holder serves");
    let file = read_store_file(&s.root).unwrap().expect("a locked store");
    std::fs::remove_file(s.root.join("vault")).unwrap();
    let files = StoreFile {
        secrets: SecretsMode::Files,
        id: None,
        slots: Vec::new(),
        unlock_timeout_min: None,
        ..file
    };
    let paths = cli_core::TargetStorePaths::for_root(s.root.clone());
    let held = cli_core::StoreLock::exclusive(&paths).unwrap();
    write_store_file(&s.root, &files, &held).unwrap();
    drop(held);
    let ms = u32::try_from((STORE_LOOK + Duration::from_secs(3)).as_millis()).unwrap();
    assert!(
        exits_within(pid, ms),
        "the holder outlived its store's lock by more than its look"
    );
    leader.kill().unwrap();
    leader.wait().unwrap();
}

/// T53: the pipe's name cannot be taken while the holder serves (first instance, one instance).
#[test]
fn windows_a_squatter_cannot_take_the_holders_pipe() {
    let s = scratch();
    let (mut leader, rx) = live(&s, "pause");
    started_pid(&next(&rx));
    let name = next(&rx).strip_prefix("pipe ").unwrap().to_string();
    let wide: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
    // SAFETY: a squatter's CreateNamedPipeW on the holder's name.
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
    // SAFETY: the error of the call above.
    let err = unsafe { GetLastError() };
    if h != INVALID_HANDLE_VALUE {
        // SAFETY: the squatter's handle.
        unsafe { CloseHandle(h) };
    }
    // The owner's implicit WRITE_DAC is gone (`OWNER RIGHTS` reads only): no process of this
    // user, in this logon session or another, opens the pipe to rewrite its DACL.
    // SAFETY: an open of the holder's pipe asking for WRITE_DAC alone.
    let dac = unsafe {
        CreateFileW(
            wide.as_ptr(),
            WRITE_DAC,
            0,
            std::ptr::null(),
            OPEN_EXISTING,
            0,
            std::ptr::null_mut(),
        )
    };
    // SAFETY: the error of the call above.
    let dac_err = unsafe { GetLastError() };
    if dac != INVALID_HANDLE_VALUE {
        // SAFETY: closed if it ever opened.
        unsafe { CloseHandle(dac) };
    }
    leader.stdin.as_mut().unwrap().write_all(b"\n").unwrap();
    assert_eq!(next(&rx), "ended");
    leader.wait().unwrap();
    assert_eq!(h, INVALID_HANDLE_VALUE);
    assert!(
        err == ERROR_ACCESS_DENIED || err == ERROR_PIPE_BUSY,
        "error {err}"
    );
    assert!(
        dac == INVALID_HANDLE_VALUE && dac_err == ERROR_ACCESS_DENIED,
        "WRITE_DAC on the holder's pipe: error {dac_err}"
    );
}

/// T59 (Windows): the client's open is a step like its reads and writes. A pipe whose one
/// instance another client holds and never lets go keeps the client no longer than a step: it
/// waits for the pipe once, within its 2 s, and finds no holder there.
#[test]
fn windows_a_busy_pipe_holds_no_client_past_its_step() {
    let name = format!(r"\\.\pipe\apprafter-busy-{}", std::process::id());
    let wide: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
    // SAFETY: a pipe of this test's own, one instance.
    let server = unsafe {
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
    assert_ne!(server, INVALID_HANDLE_VALUE);
    // SAFETY: a first client takes the one instance and holds it.
    let first = unsafe {
        CreateFileW(
            wide.as_ptr(),
            GENERIC_READ | GENERIC_WRITE,
            0,
            std::ptr::null(),
            OPEN_EXISTING,
            0,
            std::ptr::null_mut(),
        )
    };
    assert_ne!(
        first,
        INVALID_HANDLE_VALUE,
        "{}",
        std::io::Error::last_os_error()
    );
    let (tx, rx) = mpsc::channel();
    let asked = Instant::now();
    let pipe = name.clone();
    std::thread::spawn(move || {
        let _ = tx.send(HolderClient::at_pipe(&pipe, std::process::id()).is_some());
    });
    let found = rx.recv_timeout(Duration::from_secs(5));
    let waited = asked.elapsed();
    // SAFETY: the two handles above, each closed once.
    unsafe {
        CloseHandle(first);
        CloseHandle(server);
    }
    let found =
        found.expect("the client's open gave up within its step, never past the 5 s of a request");
    assert!(
        !found,
        "a pipe some other client holds is no holder of this one's"
    );
    assert!(
        waited < IO_TIMEOUT + Duration::from_secs(1),
        "the open took {waited:?}, past its {IO_TIMEOUT:?} step"
    );
}

/// The holder's DACLs, as the person's tests can read them (Task 6's P3: the hosted runner's
/// token has `SeDebugPrivilege` enabled, so there a DACL refuses no `OpenProcess` and the refusal
/// itself is the owner's walk's): `object`'s DACL, opened with `READ_CONTROL` (which `OWNER
/// RIGHTS` grants), as whether it is protected and each ACE's access mask, sorted.
fn dacl_of(object: windows_sys::Win32::Foundation::HANDLE) -> (bool, Vec<u32>) {
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Authorization::{GetSecurityInfo, SE_KERNEL_OBJECT};
    use windows_sys::Win32::Security::{
        AclSizeInformation, GetAce, GetAclInformation, GetSecurityDescriptorControl,
        ACCESS_ALLOWED_ACE, ACL, ACL_SIZE_INFORMATION, DACL_SECURITY_INFORMATION,
        PSECURITY_DESCRIPTOR, SE_DACL_PROTECTED,
    };
    let mut dacl: *mut ACL = std::ptr::null_mut();
    let mut sd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
    // SAFETY: reads `object`'s DACL into a LocalAlloc'd descriptor, freed below.
    let rc = unsafe {
        GetSecurityInfo(
            object,
            SE_KERNEL_OBJECT,
            DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut dacl,
            std::ptr::null_mut(),
            &mut sd,
        )
    };
    assert_eq!(
        rc,
        0,
        "GetSecurityInfo: {}",
        std::io::Error::from_raw_os_error(rc as i32)
    );
    let (mut control, mut revision) = (0u16, 0u32);
    // SAFETY: the descriptor above.
    unsafe { GetSecurityDescriptorControl(sd, &mut control, &mut revision) };
    // SAFETY: an all-zero ACL_SIZE_INFORMATION is valid; filled by the call.
    let mut info: ACL_SIZE_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: the DACL inside the descriptor, one ACL_SIZE_INFORMATION out.
    let ok = !dacl.is_null()
        && unsafe {
            GetAclInformation(
                dacl,
                (&raw mut info).cast(),
                std::mem::size_of::<ACL_SIZE_INFORMATION>() as u32,
                AclSizeInformation,
            )
        } != 0;
    let mut masks = Vec::new();
    for i in 0..if ok { info.AceCount } else { 0 } {
        let mut ace: *mut std::ffi::c_void = std::ptr::null_mut();
        // SAFETY: ACE `i` of the DACL above; every ACE of these DACLs is an allowing one
        // (their SDDL), whose mask sits where ACCESS_ALLOWED_ACE says.
        if unsafe { GetAce(dacl, i, &mut ace) } != 0 {
            masks.push(unsafe { (*ace.cast::<ACCESS_ALLOWED_ACE>()).Mask });
        }
    }
    // SAFETY: the descriptor GetSecurityInfo allocated, freed once.
    unsafe { LocalFree(sd) };
    masks.sort_unstable();
    (control & SE_DACL_PROTECTED != 0, masks)
}

/// `READ_CONTROL`, `OWNER RIGHTS`' one right on the holder's process and threads (T54).
const RC: u32 = 0x0002_0000;

/// T54 (Task 6's P3 alternative): the holder's process carries its DACL — protected, the user's
/// `SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_TERMINATE` (no `PROCESS_VM_READ`)
/// and `OWNER RIGHTS`' `READ_CONTROL` (no implicit `WRITE_DAC`) — so a same-user process that is
/// not elevated cannot open it to read its memory (the owner's Windows walk asserts the refusal
/// itself: the hosted runner's token has `SeDebugPrivilege` enabled). The user may stop it.
#[test]
fn windows_no_other_process_reads_the_holders_memory() {
    let s = scratch();
    let (mut leader, rx) = live(&s, "pause");
    let pid = started_pid(&next(&rx));
    next(&rx);
    // SAFETY: a handle to read the holder's DACL (READ_CONTROL), closed below.
    let h = unsafe { OpenProcess(READ_CONTROL, 0, pid) };
    let read = (!h.is_null()).then(|| dacl_of(h));
    if !h.is_null() {
        // SAFETY: the handle above, closed once.
        unsafe { CloseHandle(h) };
    }
    // SAFETY: the user's right to end it (T54's DACL).
    let t = unsafe { OpenProcess(PROCESS_TERMINATE, 0, pid) };
    let can_terminate = !t.is_null();
    if can_terminate {
        // SAFETY: as above.
        unsafe { CloseHandle(t) };
    }
    leader.stdin.as_mut().unwrap().write_all(b"\n").unwrap();
    assert_eq!(next(&rx), "ended");
    leader.wait().unwrap();
    let (protected, masks) = read.expect("READ_CONTROL on the holder, which OWNER RIGHTS grants");
    assert!(protected, "the holder's process DACL is protected");
    assert_eq!(
        masks,
        [RC, 0x0010_1001],
        "the holder's process DACL, sorted"
    );
    assert!(masks.iter().all(|m| m & PROCESS_VM_READ == 0));
    assert!(can_terminate);
}

/// T54 (critic-d4a-e finding 12; Task 6's P3 alternative): no thread of the holder may be opened
/// by another process of the same user to read, set or stop its context — a thread so opened
/// could be made to read the key page whatever the process DACL says. Each thread's DACL holds
/// the user's `SYNCHRONIZE | THREAD_QUERY_LIMITED_INFORMATION` and `OWNER RIGHTS`'
/// `READ_CONTROL`, nothing more: the primary thread's set at creation, every other's, and the
/// token's default for threads made later, by `restrict_threads` before the key is read. (The
/// refusal itself, from a non-elevated shell, is the owner's walk's.)
#[test]
fn windows_no_other_process_drives_a_holders_thread() {
    let s = scratch();
    let (mut leader, rx) = live(&s, "pause");
    let pid = started_pid(&next(&rx));
    next(&rx);
    let threads = threads_of(pid);
    let read: Vec<(u32, Option<Vec<u32>>)> = threads
        .iter()
        .map(|&tid| {
            // SAFETY: a handle to read a holder thread's DACL (READ_CONTROL), closed below.
            let h = unsafe { OpenThread(READ_CONTROL, 0, tid) };
            if h.is_null() {
                return (tid, None);
            }
            let (_, masks) = dacl_of(h);
            // SAFETY: the handle above, closed once.
            unsafe { CloseHandle(h) };
            (tid, Some(masks))
        })
        .collect();
    leader.stdin.as_mut().unwrap().write_all(b"\n").unwrap();
    assert_eq!(next(&rx), "ended");
    leader.wait().unwrap();
    assert!(!threads.is_empty(), "the holder has threads");
    let drive = THREAD_GET_CONTEXT | THREAD_SET_CONTEXT | THREAD_SUSPEND_RESUME;
    for (tid, masks) in &read {
        let masks = masks
            .as_ref()
            .unwrap_or_else(|| panic!("thread {tid}: READ_CONTROL, which OWNER RIGHTS grants"));
        assert_eq!(
            masks,
            &[RC, 0x0010_0800],
            "thread {tid}'s DACL, sorted, of {threads:?}"
        );
        assert!(masks.iter().all(|m| m & drive == 0), "thread {tid}");
    }
}

/// Every thread of process `pid`, from a Toolhelp32 snapshot.
fn threads_of(pid: u32) -> Vec<u32> {
    // SAFETY: a snapshot of the system's threads; closed below.
    let snap = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
    assert_ne!(snap, INVALID_HANDLE_VALUE);
    // SAFETY: an all-zero THREADENTRY32 is valid; dwSize is set before use.
    let mut e: THREADENTRY32 = unsafe { std::mem::zeroed() };
    e.dwSize = std::mem::size_of::<THREADENTRY32>() as u32;
    let mut out = Vec::new();
    // SAFETY: the snapshot above and an entry of the size it says.
    let mut more = unsafe { Thread32First(snap, &mut e) } != 0;
    while more {
        if e.th32OwnerProcessID == pid {
            out.push(e.th32ThreadID);
        }
        // SAFETY: as above.
        more = unsafe { Thread32Next(snap, &mut e) } != 0;
    }
    // SAFETY: the snapshot, closed once.
    unsafe { CloseHandle(snap) };
    out
}

/// Holder §1.4: the holder holds no handle of its starter's — the leader's stdout pipe reaches
/// its end as the leader exits, while the holder lives on (a keeper holds the console).
#[test]
fn windows_the_holder_keeps_no_handle_of_its_starter() {
    let s = scratch();
    let (tx, rx) = mpsc::channel();
    let mut cmd = rerun("inner_leader", &s, "keeper");
    std::thread::spawn(move || {
        let out = cmd
            .creation_flags(CREATE_NEW_CONSOLE)
            .stdout(Stdio::piped())
            .output()
            .unwrap();
        let _ = tx.send(lines(&out.stdout));
    });
    let got = rx
        .recv_timeout(Duration::from_secs(20))
        .expect("the leader's stdout ended when it exited: the holder kept no handle to it");
    let pid = started_pid(&got[0]);
    let keeper: u32 = got[1].strip_prefix("keeper ").unwrap().parse().unwrap();
    assert!(
        !exits_within(pid, 0),
        "the holder lives while the keeper holds the console"
    );
    for p in [pid, keeper] {
        // SAFETY: ends the holder and the keeper this test started.
        unsafe {
            let h = OpenProcess(PROCESS_TERMINATE, 0, p);
            if !h.is_null() {
                TerminateProcess(h, 1);
                CloseHandle(h);
            }
        }
    }
}

/// Holder §1.4: Ctrl+C typed on the console does not end the window.
#[test]
fn windows_ctrl_c_on_the_console_keeps_the_window() {
    let s = scratch();
    let got = on_a_console(&s, "ctrl_c");
    assert!(got[1].starts_with("status left="), "{got:?}");
    assert_eq!(got[2], "ended");
}

/// G8: the holder records its pid where `Spawn`'s `Drop` and the CI step find it, and removes the
/// record when it ends.
#[test]
fn windows_the_holders_pid_file_lives_as_long_as_it() {
    let s = scratch();
    let (mut leader, rx) = live(&s, "pause");
    let pid = started_pid(&next(&rx));
    next(&rx);
    let files = |dir: &Path| -> Vec<String> {
        std::fs::read_dir(dir)
            .map(|d| {
                d.flatten()
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default()
    };
    let run = s.root.join("run");
    let during = files(&run);
    assert!(
        during
            .iter()
            .any(|f| f.starts_with("h-") && f.ends_with(&format!("-{pid}.pid"))),
        "{during:?}"
    );
    leader.stdin.as_mut().unwrap().write_all(b"\n").unwrap();
    assert_eq!(next(&rx), "ended");
    leader.wait().unwrap();
    assert!(exits_within(pid, 3000));
    assert!(
        !files(&run).iter().any(|f| f.ends_with(".pid")),
        "{:?}",
        files(&run)
    );
}

// ---------------------------------------------------------------------
// Inner tests
// ---------------------------------------------------------------------

struct Inner {
    root: PathBuf,
    recovery: String,
    scenario: String,
}

fn inner() -> Inner {
    assert!(
        std::env::var_os(INNER).is_some(),
        "run only by holder_windows's outer tests: an `--ignored` run reaches it alone and fails \
         here on purpose"
    );
    Inner {
        root: PathBuf::from(std::env::var_os("HOLDER_W_STORE").unwrap()),
        recovery: std::env::var("HOLDER_W_RECOVERY").unwrap(),
        scenario: std::env::var("HOLDER_W_SCENARIO").unwrap(),
    }
}

fn binding_of(root: &Path) -> StoreBinding {
    let file = read_store_file(root).unwrap().expect("a locked store");
    StoreBinding {
        id: file.id.expect("an id").as_str().into(),
        root: root.to_path_buf(),
    }
}

/// Run this test binary's `inner_ask` as a command (it inherits this leader's environment): on
/// this console, or on one of its own.
fn ask(what: &str, own_console: bool, extra: &[(&str, String)]) {
    let mut c = Command::new(std::env::current_exe().unwrap());
    c.args([
        "--exact",
        "inner_ask",
        "--ignored",
        "--nocapture",
        "--test-threads=1",
    ])
    .env("HOLDER_W_ASK", what);
    for (k, v) in extra {
        c.env(k, v);
    }
    if own_console {
        c.creation_flags(CREATE_NEW_CONSOLE);
    }
    c.status().expect("the command runs");
}

fn wait_for_go() {
    let mut l = String::new();
    let _ = std::io::stdin().read_line(&mut l);
}

#[test]
#[ignore = "the leader of a console holder_windows's tests start"]
fn inner_leader() {
    let w = inner();
    // A Ctrl+C on this console (the ctrl_c scenario) must not end this leader.
    // SAFETY: ignore Ctrl+C in this process.
    unsafe { SetConsoleCtrlHandler(None, 1) };
    let bytes = std::fs::read(w.root.join("vault")).unwrap();
    let opened = VaultFile::parse(&bytes)
        .unwrap()
        .open(
            &SlotSecret::RecoveryKey(RecoveryKey::parse(&w.recovery).unwrap()),
            KdfPolicy::active(),
        )
        .unwrap();
    let store = binding_of(&w.root);
    let init = InitHeader {
        store: store.clone(),
        base: w.root.join("run"),
        vault_id: opened.header().vault_id,
        minutes: 5,
        window_secs: None,
        poll_leader: false,
        client_pid: std::process::id(),
    };
    let pid = match start_with(Path::new(Bin::Apprafter.path()), &init, opened.data_key()) {
        Ok(HolderStarted::Started { pid }) => pid,
        other => {
            println!("holder: start {other:?}");
            return;
        }
    };
    println!("holder: started {pid}");
    std::io::stdout().flush().unwrap();
    let user = apprafter::holder::os_user_sid().unwrap();
    let pipe = pipe_name(&EndpointId::for_user(&user, &store), pid);
    match w.scenario.as_str() {
        "serve" => {
            ask("item:prod", false, &[]);
            ask("status", false, &[]);
            ask("end", false, &[]);
        }
        "other_console" => {
            ask(
                "item:prod",
                true,
                &[
                    ("HOLDER_W_PIPE", pipe.clone()),
                    ("HOLDER_W_PIPE_PID", pid.to_string()),
                ],
            );
            ask("end", false, &[]);
        }
        "attach" => {
            ask(
                "item:prod",
                true,
                &[("HOLDER_W_ATTACH", std::process::id().to_string())],
            );
            ask("end", false, &[]);
        }
        "alone" => {}
        "pause" => {
            println!("holder: pipe {pipe}");
            std::io::stdout().flush().unwrap();
            wait_for_go();
            ask("end", false, &[]);
        }
        "keeper" => {
            // `std` passes every inheritable handle to a child (rust#73281): the leader's own
            // stdout and stderr, the test's pipes, would stay open in the keeper for its 20 s and
            // hide what this scenario asks of the holder.
            for std_handle in [STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
                // SAFETY: one of this process's standard handles, its inherit flag cleared.
                unsafe { SetHandleInformation(GetStdHandle(std_handle), HANDLE_FLAG_INHERIT, 0) };
            }
            // Never waited for: it must outlive this leader, and the outer test ends it by its
            // pid (Windows keeps no zombie of a process no one waits for).
            #[allow(clippy::zombie_processes)]
            let keeper = Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "inner_keeper", "--ignored", "--test-threads=1"])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap();
            println!("holder: keeper {}", keeper.id());
        }
        "ctrl_c" => {
            // SAFETY: Ctrl+C to every process of this console (group 0).
            unsafe { GenerateConsoleCtrlEvent(CTRL_C_EVENT, 0) };
            std::thread::sleep(Duration::from_millis(300));
            ask("status", false, &[]);
            ask("end", false, &[]);
        }
        other => panic!("no scenario {other}"),
    }
}

#[test]
#[ignore = "a process that keeps a holder_windows console populated"]
fn inner_keeper() {
    inner();
    std::thread::sleep(Duration::from_secs(20));
}

#[test]
#[ignore = "a command of a console holder_windows's tests start"]
fn inner_ask() {
    let w = inner();
    if let Ok(pid) = std::env::var("HOLDER_W_ATTACH") {
        // SAFETY: leave this process's console and attach to the leader's (the pinned limit).
        unsafe {
            FreeConsole();
            assert_ne!(
                AttachConsole(pid.parse().unwrap()),
                0,
                "{}",
                std::io::Error::last_os_error()
            );
        }
    }
    let store = binding_of(&w.root);
    let vault = std::fs::read(w.root.join("vault")).unwrap();
    let client = match std::env::var("HOLDER_W_PIPE") {
        Ok(name) => HolderClient::at_pipe(
            &name,
            std::env::var("HOLDER_W_PIPE_PID").unwrap().parse().unwrap(),
        ),
        Err(_) => HolderClient::on_console(&store).unwrap(),
    };
    let Some(mut client) = client else {
        println!("holder: none");
        return;
    };
    let said = match std::env::var("HOLDER_W_ASK").unwrap().as_str() {
        "status" => client.status().map(|s| {
            format!(
                "status left={} minutes={} nd={:?} da={:?} core={} mem={} dacl={:?}",
                s.left_secs,
                s.minutes,
                s.hardening.non_dumpable,
                s.hardening.deny_attach,
                s.hardening.core_off,
                s.hardening.memory_locked,
                s.hardening.process_dacl
            )
        }),
        "end" => client.end().map(|()| "ended".to_string()),
        item => {
            let target = item.strip_prefix("item:").unwrap();
            client
                .open_item(
                    &vault,
                    &HolderItem::HetznerToken {
                        target: target.into(),
                    },
                )
                .map(|v| {
                    v.map_or("absent".into(), |v| {
                        format!("item {}", String::from_utf8_lossy(&v))
                    })
                })
        }
    };
    println!(
        "holder: {}",
        said.unwrap_or_else(|e| match e {
            HolderError::Refused(r) => format!("refused {}", r.as_str()),
            other => format!("error {other:?}"),
        })
    );
}
