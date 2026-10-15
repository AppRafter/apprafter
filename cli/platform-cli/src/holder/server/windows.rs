// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The holder process on Windows (D.4a T51–T54, T56; holder §1.4, §1.5, §2.3, §3.4).

use std::fs::File;
use std::os::windows::io::{FromRawHandle, RawHandle};
use std::ptr;
use std::sync::atomic::{AtomicPtr, Ordering};
use std::time::Duration;

use apprafter_core::lock::HolderHardening;
use windows_sys::core::BOOL;
use windows_sys::Win32::Foundation::{
    GetLastError, ERROR_IO_PENDING, ERROR_PIPE_CONNECTED, INVALID_HANDLE_VALUE, WAIT_OBJECT_0,
    WAIT_TIMEOUT,
};
use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
use windows_sys::Win32::Storage::FileSystem::{
    FlushFileBuffers, GetFileType, FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED,
    FILE_TYPE_CHAR, PIPE_ACCESS_DUPLEX,
};
use windows_sys::Win32::System::Console::{
    AttachConsole, FreeConsole, GetStdHandle, SetConsoleCtrlHandler, ATTACH_PARENT_PROCESS,
    CTRL_BREAK_EVENT, CTRL_CLOSE_EVENT, CTRL_C_EVENT, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
};
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, GetNamedPipeClientProcessId,
    PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_WAIT,
};
use windows_sys::Win32::System::Threading::{ExitProcess, ResetEvent, WaitForSingleObject};
use windows_sys::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};

use crate::holder::binding::{admit_console, console_alone, Admission};
use crate::holder::endpoint::{pipe_name, EndpointId};
use crate::holder::harden::harden_self;
use crate::holder::memory::{wipe, LockedKey};
use crate::holder::os::windows::{
    console_processes, event, token_sid, user_sid_string, Handle, PidFile, PipeIo, Sd,
};
use crate::holder::protocol::{
    read_init, read_request, write_ack, write_reply, Ack, FrameError, HolderUnavailable,
    InitHeader, Refusal, Reply, PROTOCOL, VERSION,
};
use crate::holder::serve::{handle, Held};
use crate::holder::watch::StoreWatch;
use crate::holder::window::{SystemClock, Window, WindowState};

const IO_TIMEOUT: Duration = Duration::from_secs(2);

/// The key page, for the console handler: zeroed on `CTRL_CLOSE_EVENT` before the process exits.
static KEY_PAGE: AtomicPtr<u8> = AtomicPtr::new(ptr::null_mut());

/// Ctrl+C and Ctrl+Break never end a window (holder §1.4); the console closing does, the key
/// zeroed first (the system allows 5 s).
unsafe extern "system" fn on_console_event(event: u32) -> BOOL {
    match event {
        CTRL_C_EVENT | CTRL_BREAK_EVENT => 1,
        CTRL_CLOSE_EVENT => {
            let page = KEY_PAGE.swap(ptr::null_mut(), Ordering::SeqCst);
            if !page.is_null() {
                // SAFETY: the key page, still mapped: the main thread frees it only after
                // clearing KEY_PAGE.
                unsafe { wipe(page, 32) };
            }
            // SAFETY: ends this process.
            unsafe { ExitProcess(0) }
        }
        _ => 0,
    }
}

pub(super) fn run() -> i32 {
    // SAFETY: GetStdHandle and GetFileType read this process's handles.
    let stdin = unsafe { GetStdHandle(STD_INPUT_HANDLE) };
    if unsafe { GetFileType(stdin) } == FILE_TYPE_CHAR {
        eprintln!("{}", crate::holder::NOT_A_COMMAND);
        return 2;
    }
    // T54: the DACL re-applied (it was set at creation) and the error mode, then a locked page,
    // then the key into it.
    let mut hardening = harden_self();
    let Ok(mut key) = LockedKey::new() else {
        return 1;
    };
    hardening.memory_locked = key.locked();
    // SAFETY: the inherited init pipe, owned from here, closed once read.
    let mut init_pipe = unsafe { File::from_raw_handle(stdin as RawHandle) };
    let Ok(init) = read_init(&mut init_pipe, key.bytes_mut()) else {
        return 1;
    };
    drop(init_pipe);
    // SAFETY: the inherited ack pipe, owned from here.
    let mut ack = unsafe { File::from_raw_handle(GetStdHandle(STD_OUTPUT_HANDLE) as RawHandle) };
    let bound = match bind(&init, &key) {
        Ok(b) => b,
        Err(u) => {
            let _ = write_ack(&mut ack, &Ack::Unavailable(u));
            return 1;
        }
    };
    let ready = Ack::Ready {
        pid: bound.me,
        protocol: PROTOCOL,
        version: VERSION.into(),
    };
    if write_ack(&mut ack, &ready).is_err() {
        return 1;
    }
    drop(ack);
    let code = serve(&bound, &key, &init, hardening);
    KEY_PAGE.store(ptr::null_mut(), Ordering::SeqCst);
    code
}

struct Bound {
    pipe: Handle,
    _pid_file: PidFile,
    me: u32,
}

/// Attach to the starter's console, check it lists the starter, create the pipe, record the pid
/// (holder §1.4 steps 4–7).
fn bind(init: &InitHeader, key: &LockedKey) -> Result<Bound, HolderUnavailable> {
    // SAFETY: attaches this detached process to its parent's console.
    if unsafe { AttachConsole(ATTACH_PARENT_PROCESS) } == 0 {
        // SAFETY: the error of the call above.
        return Err(HolderUnavailable::NoConsole {
            code: unsafe { GetLastError() },
        });
    }
    KEY_PAGE.store(key.page(), Ordering::SeqCst);
    // SAFETY: Ctrl+C ignored (also by CREATE_NEW_PROCESS_GROUP), and this process's handler.
    unsafe {
        SetConsoleCtrlHandler(None, 1);
        SetConsoleCtrlHandler(Some(on_console_event), 1);
    }
    if !console_processes().contains(&init.client_pid) {
        return Err(HolderUnavailable::NoConsole { code: 0 });
    }
    let user = user_sid_string().map_err(|e| HolderUnavailable::NoConsole {
        code: e.raw_os_error().unwrap_or(0) as u32,
    })?;
    let id = EndpointId::for_user(&user, &init.store);
    let me = std::process::id();
    let pipe = create_pipe(&pipe_name(&id, me)).map_err(|_| HolderUnavailable::Timeout)?;
    let pid_file =
        PidFile::write(&init.base, &id, me).map_err(|e| HolderUnavailable::UnsafeRuntimeDir {
            path: init.base.clone(),
            reason: e.to_string(),
        })?;
    Ok(Bound {
        pipe,
        _pid_file: pid_file,
        me,
    })
}

/// The holder's pipe (T53): the first and only instance of its name, closed to remote clients,
/// open to this logon session alone (its logon SID; the user's when the token has none).
/// `OWNER RIGHTS` reads its security and nothing more, as on the holder's process and threads:
/// without it the owner — this user, in any logon session — would hold the implicit
/// `WRITE_DAC`, open the pipe with it, and grant itself the rest.
fn create_pipe(name: &str) -> std::io::Result<Handle> {
    let sid = token_sid(true).or_else(|_| token_sid(false))?;
    let sd = Sd::from_sddl(&format!("D:P(A;;GRGW;;;{sid})(A;;RC;;;OW)"))?;
    let sa = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: sd.0,
        bInheritHandle: 0,
    };
    let wide: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
    // SAFETY: a NUL-terminated name and the attributes above, which outlive the call.
    let h = unsafe {
        CreateNamedPipeW(
            wide.as_ptr(),
            PIPE_ACCESS_DUPLEX | FILE_FLAG_FIRST_PIPE_INSTANCE | FILE_FLAG_OVERLAPPED,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
            1,
            4096,
            4096,
            0,
            &sa,
        )
    };
    if h == INVALID_HANDLE_VALUE {
        return Err(std::io::Error::last_os_error());
    }
    Ok(Handle(h))
}

/// T52, O4, T56: the 1 s look — alone on the console (or no console), the window over, or the
/// store no longer locked with this holder's id (its watch re-reads `store.yaml` every
/// `STORE_LOOK`; team decision 2026-10-13).
fn leave(window: &mut Window, watch: &mut StoreWatch, me: u32) -> bool {
    console_alone(&console_processes(), me)
        || !matches!(window.check(&SystemClock), WindowState::Open { .. })
        || !watch.holds(&SystemClock)
}

/// One instance, reused: a pending connect, a look every second, one request per client.
fn serve(b: &Bound, key: &LockedKey, init: &InitHeader, hardening: HolderHardening) -> i32 {
    let mut window = Window::open(Window::length(init.minutes, init.window_secs), &SystemClock);
    let mut watch = StoreWatch::new(&init.store);
    let Ok(event) = event() else {
        return 1;
    };
    loop {
        // SAFETY: an all-zero OVERLAPPED is valid; it outlives the connect (waited for, or
        // cancelled and waited for).
        let mut ov: OVERLAPPED = unsafe { std::mem::zeroed() };
        ov.hEvent = event.0;
        // SAFETY: the event above; the pipe this holder owns.
        unsafe { ResetEvent(event.0) };
        let connected = unsafe { ConnectNamedPipe(b.pipe.0, &mut ov) } != 0;
        if !connected {
            // SAFETY: the error of ConnectNamedPipe.
            match unsafe { GetLastError() } {
                ERROR_PIPE_CONNECTED => {}
                ERROR_IO_PENDING => loop {
                    // SAFETY: the event above.
                    match unsafe { WaitForSingleObject(event.0, 1000) } {
                        WAIT_OBJECT_0 => {
                            let mut n = 0u32;
                            // SAFETY: the completed connect.
                            if unsafe { GetOverlappedResult(b.pipe.0, &ov, &mut n, 0) } == 0 {
                                // SAFETY: drop a failed client and listen again.
                                unsafe { DisconnectNamedPipe(b.pipe.0) };
                            }
                            break;
                        }
                        WAIT_TIMEOUT => {
                            if leave(&mut window, &mut watch, b.me) {
                                let mut n = 0u32;
                                // SAFETY: cancel the pending connect and wait for it.
                                unsafe {
                                    CancelIoEx(b.pipe.0, &ov);
                                    GetOverlappedResult(b.pipe.0, &ov, &mut n, 1);
                                    FreeConsole();
                                }
                                return 0;
                            }
                        }
                        _ => return 1,
                    }
                },
                _ => return 1,
            }
        }
        // The one instance stays this client's until it is disconnected, after the reply: a
        // command that runs once its client has read `Ended` finds the pipe busy, then gone with
        // this process — never a holder that is about to exit (the Unix server removes its socket
        // before the reply for the same end).
        let end = serve_client(b, key, init, hardening, &mut window);
        // SAFETY: the pipe this holder owns.
        unsafe {
            FlushFileBuffers(b.pipe.0);
            DisconnectNamedPipe(b.pipe.0);
        }
        if end {
            // SAFETY: leave the console before exiting, so a classic window can close.
            unsafe { FreeConsole() };
            return 0;
        }
    }
}

/// One client: admit it by the console list (T51), then read its request and serve — §3.10's
/// order, so a refused client never holds the loop for a read (critic-d4a-e finding 21). True:
/// the holder ends.
fn serve_client(
    b: &Bound,
    key: &LockedKey,
    init: &InitHeader,
    hardening: HolderHardening,
    window: &mut Window,
) -> bool {
    let Ok(mut io) = PipeIo::new(b.pipe.0, IO_TIMEOUT, None) else {
        return false;
    };
    let mut client = 0u32;
    // SAFETY: the connected pipe; writes one pid (0 when it cannot).
    unsafe { GetNamedPipeClientProcessId(b.pipe.0, &mut client) };
    match admit_console(client, &console_processes(), b.me) {
        Admission::Serve => {}
        Admission::Refuse(r) => {
            let _ = write_reply(&mut io, &Reply::Refused(r));
            return false;
        }
        Admission::EndNow => return true,
    }
    let req = match read_request(&mut io) {
        Ok(req) => req,
        Err(FrameError::Version(_)) => {
            let _ = write_reply(
                &mut io,
                &Reply::Refused(Refusal::Unsupported {
                    holder_protocol: PROTOCOL,
                }),
            );
            return false;
        }
        Err(_) => {
            let _ = write_reply(&mut io, &Reply::Refused(Refusal::Malformed));
            return false;
        }
    };
    let held = Held {
        key: key.bytes(),
        init,
        hardening,
    };
    let served = handle(req, &held, window, &SystemClock);
    let _ = write_reply(&mut io, &served.reply);
    served.end
}
