// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The Windows start (holder §1.4, finding 11): raw `CreateProcessW`, so the holder inherits the
//! two pipe ends and nothing else (`std::process::Command` passes every inheritable handle,
//! rust#73281), and its process and thread carry T54's security descriptors from creation.

use std::fs::File;
use std::mem::size_of;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{FromRawHandle, RawHandle};
use std::path::Path;
use std::ptr;

use apprafter_core::vault::DataKey;
use windows_sys::Win32::Foundation::{SetHandleInformation, HANDLE, HANDLE_FLAG_INHERIT};
use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
use windows_sys::Win32::System::Pipes::CreatePipe;
use windows_sys::Win32::System::Threading::{
    CreateProcessW, DeleteProcThreadAttributeList, InitializeProcThreadAttributeList,
    TerminateProcess, UpdateProcThreadAttribute, CREATE_NEW_PROCESS_GROUP,
    CREATE_UNICODE_ENVIRONMENT, DETACHED_PROCESS, EXTENDED_STARTUPINFO_PRESENT,
    LPPROC_THREAD_ATTRIBUTE_LIST, PROCESS_INFORMATION, PROC_THREAD_ATTRIBUTE_HANDLE_LIST,
    STARTF_USESTDHANDLES, STARTUPINFOEXW,
};

use super::{read_ack_within, HolderStarted, ACK_TIMEOUT};
use crate::holder::os::windows::{
    process_sddl, system_dir, thread_sddl, user_sid_string, windows_dir, Handle, Sd,
};
use crate::holder::protocol::{write_init, Ack, HolderUnavailable, InitHeader};

/// A pipe, neither end inheritable yet: (read, write). The child's end is made inheritable
/// for the `CreateProcessW` call alone ([`inheritable`]).
fn pipe() -> std::io::Result<(Handle, Handle)> {
    let sa = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: ptr::null_mut(),
        bInheritHandle: 0,
    };
    let (mut r, mut w): (HANDLE, HANDLE) = (ptr::null_mut(), ptr::null_mut());
    // SAFETY: two handles out, owned below.
    if unsafe { CreatePipe(&mut r, &mut w, &sa, 0) } == 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok((Handle(r), Handle(w)))
}

/// Make the holder's two ends inheritable (`on`), or not again. They are inheritable for the
/// `CreateProcessW` call alone (critic-d4a-e finding 17): while they are, a
/// `std::process::Command` another thread of this process starts would inherit them too —
/// `std` passes every inheritable handle (rust#73281) and its own lock against this race is
/// private to `std`. So `start_holder` must not run beside another spawn of this process; its
/// callers are `unlock`, a leaf that starts nothing else, and hook B, on the thread that has
/// just prompted (**Deviations** 30).
fn inheritable(handles: [HANDLE; 2], on: bool) -> std::io::Result<()> {
    let flag = if on { HANDLE_FLAG_INHERIT } else { 0 };
    for h in handles {
        // SAFETY: a handle this process owns.
        if unsafe { SetHandleInformation(h, HANDLE_FLAG_INHERIT, flag) } == 0 {
            return Err(std::io::Error::last_os_error());
        }
    }
    Ok(())
}

/// The attribute list naming the only handles the holder inherits.
struct HandleList {
    buf: Vec<usize>,
    handles: Box<[HANDLE; 2]>,
}

impl HandleList {
    fn new(handles: [HANDLE; 2]) -> std::io::Result<HandleList> {
        let mut size = 0usize;
        // SAFETY: a size query.
        unsafe { InitializeProcThreadAttributeList(ptr::null_mut(), 1, 0, &mut size) };
        let mut list = HandleList {
            buf: vec![0usize; size.div_ceil(size_of::<usize>())],
            handles: Box::new(handles),
        };
        // SAFETY: a buffer of at least `size` bytes, pointer-aligned.
        if unsafe { InitializeProcThreadAttributeList(list.ptr(), 1, 0, &mut size) } == 0 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: the list above; the handle array lives as long as the list (boxed: stable).
        let ok = unsafe {
            UpdateProcThreadAttribute(
                list.ptr(),
                0,
                PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
                list.handles.as_ptr().cast(),
                size_of::<[HANDLE; 2]>(),
                ptr::null_mut(),
                ptr::null(),
            )
        };
        if ok == 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(list)
    }

    fn ptr(&mut self) -> LPPROC_THREAD_ATTRIBUTE_LIST {
        self.buf.as_mut_ptr().cast()
    }
}

impl Drop for HandleList {
    fn drop(&mut self) {
        // SAFETY: the list `new` initialised.
        unsafe { DeleteProcThreadAttributeList(self.ptr()) };
    }
}

pub(super) fn start(
    program: &Path,
    arg: &str,
    init: &InitHeader,
    key: &DataKey,
) -> Result<HolderStarted, HolderUnavailable> {
    let failed = |_: std::io::Error| HolderUnavailable::Timeout;
    let (init_read, init_write) = pipe().map_err(failed)?;
    let (ack_read, ack_write) = pipe().map_err(failed)?;
    let user = user_sid_string().map_err(failed)?;
    let process_sd = Sd::from_sddl(&process_sddl(&user)).map_err(failed)?;
    let thread_sd = Sd::from_sddl(&thread_sddl(&user)).map_err(failed)?;
    let attrs = |sd: &Sd| SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: sd.0,
        bInheritHandle: 0,
    };
    let (process_attrs, thread_attrs) = (attrs(&process_sd), attrs(&thread_sd));
    let mut list = HandleList::new([init_read.0, ack_write.0]).map_err(failed)?;
    // SAFETY: an all-zero STARTUPINFOEXW is valid; the fields set below.
    let mut si: STARTUPINFOEXW = unsafe { std::mem::zeroed() };
    si.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
    si.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
    si.StartupInfo.hStdInput = init_read.0;
    si.StartupInfo.hStdOutput = ack_write.0;
    si.StartupInfo.hStdError = ptr::null_mut();
    si.lpAttributeList = list.ptr();
    let app: Vec<u16> = program.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut cmd: Vec<u16> = Vec::new();
    cmd.push(u16::from(b'"'));
    cmd.extend(program.as_os_str().encode_wide());
    cmd.extend("\" ".encode_utf16());
    cmd.extend(arg.encode_utf16());
    cmd.push(0);
    // The holder reads no environment (T47): SystemRoot alone keeps the Windows runtime working.
    let mut env: Vec<u16> = "SystemRoot=".encode_utf16().collect();
    env.extend(windows_dir().map_err(failed)?);
    env.extend([0, 0]);
    let mut cwd = system_dir().map_err(failed)?;
    cwd.push(0);
    // SAFETY: an all-zero PROCESS_INFORMATION is valid; filled by the call.
    let mut pi: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
    let ends = [init_read.0, ack_write.0];
    inheritable(ends, true).map_err(failed)?;
    // SAFETY: every pointer is to a NUL-terminated buffer or a structure that outlives the call.
    let created = unsafe {
        CreateProcessW(
            app.as_ptr(),
            cmd.as_mut_ptr(),
            &process_attrs,
            &thread_attrs,
            1,
            DETACHED_PROCESS
                | CREATE_NEW_PROCESS_GROUP
                | CREATE_UNICODE_ENVIRONMENT
                | EXTENDED_STARTUPINFO_PRESENT,
            env.as_ptr().cast(),
            cwd.as_ptr(),
            &si.StartupInfo,
            &mut pi,
        )
    } != 0;
    // Closed right below; not inheritable a moment longer than the call (finding 17).
    let _ = inheritable(ends, false);
    drop(list);
    drop(init_read);
    drop(ack_write);
    if !created {
        return Err(HolderUnavailable::Timeout);
    }
    let process = Handle(pi.hProcess);
    drop(Handle(pi.hThread));
    // SAFETY: this process's end of the init pipe, owned by the File from here.
    let mut to_holder = unsafe { File::from_raw_handle(init_write.into_raw() as RawHandle) };
    // The key's one trip (T48): from its buffer into the pipe, which is unbuffered.
    let wrote = write_init(&mut to_holder, init, key);
    drop(to_holder);
    let kill = || {
        // SAFETY: the holder's process handle (full access: its creator's).
        unsafe { TerminateProcess(process.0, 1) };
    };
    if wrote.is_err() {
        kill();
        return Err(HolderUnavailable::Timeout);
    }
    // SAFETY: this process's end of the ack pipe, owned by the File from here.
    let from_holder = unsafe { File::from_raw_handle(ack_read.into_raw() as RawHandle) };
    match read_ack_within(from_holder, ACK_TIMEOUT) {
        Some(Ok(Ack::Ready { pid, .. })) => Ok(HolderStarted::Started { pid }),
        Some(Ok(Ack::AlreadyRunning { pid })) => Ok(HolderStarted::AlreadyRunning { pid }),
        Some(Ok(Ack::Unavailable(u))) => Err(u),
        Some(Err(_)) | None => {
            kill();
            Err(HolderUnavailable::Timeout)
        }
    }
}
