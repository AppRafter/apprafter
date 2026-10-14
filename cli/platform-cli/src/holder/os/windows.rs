// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Windows calls of the holder (D.4a T51–T56; holder §1.4, §2.3, §4).

use std::io;
use std::path::Path;
use std::ptr;
use std::time::Duration;

use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, LocalFree, ERROR_BROKEN_PIPE, ERROR_IO_PENDING, HANDLE, WAIT_TIMEOUT,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SetSecurityInfo,
    SDDL_REVISION_1, SE_KERNEL_OBJECT,
};
use windows_sys::Win32::Security::{
    GetSecurityDescriptorDacl, GetTokenInformation, TokenLogonSid, TokenUser, ACL,
    DACL_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR,
    TOKEN_GROUPS, TOKEN_QUERY, TOKEN_USER,
};
use windows_sys::Win32::Storage::FileSystem::{ReadFile, WriteFile};
use windows_sys::Win32::System::Console::GetConsoleProcessList;
use windows_sys::Win32::System::Pipes::GetNamedPipeServerProcessId;
use windows_sys::Win32::System::SystemInformation::{
    GetSystemDirectoryW, GetSystemWindowsDirectoryW, GetTickCount64,
};
use windows_sys::Win32::System::Threading::{
    CreateEventW, GetCurrentProcess, OpenProcess, OpenProcessToken, QueryFullProcessImageNameW,
    TerminateProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_TERMINATE,
};
use windows_sys::Win32::System::WindowsProgramming::QueryUnbiasedInterruptTime;
use windows_sys::Win32::System::IO::{
    CancelIoEx, GetOverlappedResult, GetOverlappedResultEx, OVERLAPPED,
};

/// Counts suspend (milliseconds since boot): the window's clock.
pub(crate) fn clock_with_sleep() -> Duration {
    // SAFETY: no arguments.
    Duration::from_millis(unsafe { GetTickCount64() })
}

/// Excludes suspend ("the time the system has spent in the working state", 100 ns units).
pub(crate) fn clock_awake() -> Duration {
    let mut t = 0u64;
    // SAFETY: writes one u64.
    unsafe { QueryUnbiasedInterruptTime(&mut t) };
    Duration::from_nanos(t.saturating_mul(100))
}

/// An owned kernel handle, closed on drop.
pub(crate) struct Handle(pub(crate) HANDLE);

impl Drop for Handle {
    fn drop(&mut self) {
        if !self.0.is_null() && self.0 != windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE {
            // SAFETY: a handle this value owns, closed once.
            unsafe { CloseHandle(self.0) };
        }
    }
}

// SAFETY: a kernel handle may be used from any thread.
unsafe impl Send for Handle {}

/// A security descriptor made from SDDL, freed with `LocalFree`.
pub(crate) struct Sd(pub(crate) PSECURITY_DESCRIPTOR);

impl Sd {
    pub(crate) fn from_sddl(sddl: &str) -> io::Result<Sd> {
        let wide: Vec<u16> = sddl.encode_utf16().chain(Some(0)).collect();
        let mut sd: PSECURITY_DESCRIPTOR = ptr::null_mut();
        // SAFETY: a NUL-terminated string in, one descriptor out (LocalAlloc'd).
        let ok = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                wide.as_ptr(),
                SDDL_REVISION_1,
                &mut sd,
                ptr::null_mut(),
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Sd(sd))
    }

    /// The descriptor's DACL, owned by the descriptor.
    pub(crate) fn dacl(&self) -> io::Result<*mut ACL> {
        let (mut present, mut defaulted) = (0, 0);
        let mut dacl: *mut ACL = ptr::null_mut();
        // SAFETY: reads the descriptor this value owns.
        let ok =
            unsafe { GetSecurityDescriptorDacl(self.0, &mut present, &mut dacl, &mut defaulted) };
        if ok == 0 || present == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(dacl)
    }
}

impl Drop for Sd {
    fn drop(&mut self) {
        // SAFETY: the descriptor ConvertStringSecurityDescriptorToSecurityDescriptorW allocated.
        unsafe { LocalFree(self.0) };
    }
}

/// This process token's user SID ("S-1-5-21-…"), or its logon SID when `logon` (the pipe's
/// DACL, T53: another logon session of the same user — an SSH logon, a scheduled task — carries
/// another one).
pub(crate) fn token_sid(logon: bool) -> io::Result<String> {
    let mut token: HANDLE = ptr::null_mut();
    // SAFETY: this process's pseudo handle; the token handle is owned below.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let token = Handle(token);
    let class = if logon { TokenLogonSid } else { TokenUser };
    let mut len = 0u32;
    // SAFETY: a size query.
    unsafe { GetTokenInformation(token.0, class, ptr::null_mut(), 0, &mut len) };
    if len == 0 {
        return Err(io::Error::last_os_error());
    }
    // u64s keep the TOKEN_* structures aligned.
    let mut buf = vec![0u64; (len as usize).div_ceil(8)];
    // SAFETY: `buf` holds `len` bytes.
    if unsafe { GetTokenInformation(token.0, class, buf.as_mut_ptr().cast(), len, &mut len) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the buffer holds the structure the class names.
    let sid = unsafe {
        if logon {
            let groups = &*buf.as_ptr().cast::<TOKEN_GROUPS>();
            if groups.GroupCount == 0 {
                return Err(io::Error::from(io::ErrorKind::NotFound));
            }
            groups.Groups[0].Sid
        } else {
            (*buf.as_ptr().cast::<TOKEN_USER>()).User.Sid
        }
    };
    let mut text: *mut u16 = ptr::null_mut();
    // SAFETY: a SID in `buf`, one LocalAlloc'd string out.
    if unsafe { ConvertSidToStringSidW(sid, &mut text) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: a NUL-terminated string from the call above, freed once.
    let s = unsafe {
        let n = (0..).take_while(|i| *text.add(*i) != 0).count();
        let s = String::from_utf16_lossy(std::slice::from_raw_parts(text, n));
        LocalFree(text.cast());
        s
    };
    Ok(s)
}

pub(crate) fn user_sid_string() -> io::Result<String> {
    token_sid(false)
}

/// T54's process DACL: the user may wait on, query and terminate the holder
/// (`SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_TERMINATE`); `OWNER RIGHTS` only
/// reads its security, which removes the owner's implicit `WRITE_DAC`. No `PROCESS_VM_READ`.
pub(crate) fn process_sddl(user: &str) -> String {
    format!("D:P(A;;0x00101001;;;{user})(A;;RC;;;OW)")
}

/// The primary thread's: `SYNCHRONIZE | THREAD_QUERY_LIMITED_INFORMATION`; no
/// `THREAD_GET_CONTEXT`.
pub(crate) fn thread_sddl(user: &str) -> String {
    format!("D:P(A;;0x00100800;;;{user})(A;;RC;;;OW)")
}

/// T54 for threads (critic-d4a-e finding 12): a same-user process that opens a thread of the
/// holder with `THREAD_GET_CONTEXT | THREAD_SET_CONTEXT | THREAD_SUSPEND_RESUME` can make it run
/// code that reads the key page, whatever the process DACL says. So [`thread_sddl`]'s DACL goes
/// on every thread this process has now — the loader's workers got the token's default — and
/// becomes the token's default, so a thread this process makes later (a thread-pool worker)
/// gets it too. The thread running this uses its pseudo handle, which needs no access right (its
/// own DACL, set at creation, grants the owner no `WRITE_DAC`). A thread that is gone between
/// the snapshot and the open is skipped. A thread another process makes in the holder (the
/// console's control handler) takes its creator's default; `CTRL_CLOSE_EVENT`'s handler wipes
/// the key page first.
pub(crate) fn restrict_threads() -> io::Result<()> {
    use windows_sys::Win32::Foundation::{ERROR_INVALID_PARAMETER, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Security::{
        SetTokenInformation, TokenDefaultDacl, TOKEN_ADJUST_DEFAULT, TOKEN_DEFAULT_DACL,
    };
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Thread32First, Thread32Next, TH32CS_SNAPTHREAD, THREADENTRY32,
    };
    use windows_sys::Win32::System::Threading::{
        GetCurrentProcessId, GetCurrentThread, GetCurrentThreadId, OpenThread, THREAD_WRITE_DAC,
    };
    let sd = Sd::from_sddl(&thread_sddl(&user_sid_string()?))?;
    let dacl = sd.dacl()?;
    let apply = |thread: HANDLE| {
        // SAFETY: a thread handle with WRITE_DAC (or the pseudo handle) and a DACL `sd` owns.
        let rc = unsafe {
            SetSecurityInfo(
                thread,
                SE_KERNEL_OBJECT,
                DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                ptr::null_mut(),
                ptr::null_mut(),
                dacl,
                ptr::null(),
            )
        };
        if rc == 0 {
            Ok(())
        } else {
            Err(io::Error::from_raw_os_error(rc as i32))
        }
    };
    let mut token: HANDLE = ptr::null_mut();
    // SAFETY: this process's pseudo handle; the token handle is owned below.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_ADJUST_DEFAULT, &mut token) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let token = Handle(token);
    let default = TOKEN_DEFAULT_DACL { DefaultDacl: dacl };
    // SAFETY: a TOKEN_DEFAULT_DACL whose DACL `sd` owns, for the call's length.
    let set = unsafe {
        SetTokenInformation(
            token.0,
            TokenDefaultDacl,
            (&raw const default).cast(),
            std::mem::size_of::<TOKEN_DEFAULT_DACL>() as u32,
        )
    };
    if set == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: a snapshot of the system's threads; owned below.
    let snap = Handle(unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) });
    if snap.0 == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: no arguments.
    let (me, this_thread) = unsafe { (GetCurrentProcessId(), GetCurrentThreadId()) };
    // SAFETY: an all-zero THREADENTRY32 is valid; dwSize is set before use.
    let mut e: THREADENTRY32 = unsafe { std::mem::zeroed() };
    e.dwSize = std::mem::size_of::<THREADENTRY32>() as u32;
    // SAFETY: the snapshot above and an entry of the size it says.
    let mut more = unsafe { Thread32First(snap.0, &mut e) } != 0;
    while more {
        if e.th32OwnerProcessID == me {
            if e.th32ThreadID == this_thread {
                // SAFETY: no arguments; a pseudo handle, never closed.
                apply(unsafe { GetCurrentThread() })?;
            } else {
                // SAFETY: one of this process's threads; the handle is owned below.
                let t = unsafe { OpenThread(THREAD_WRITE_DAC, 0, e.th32ThreadID) };
                if t.is_null() {
                    // SAFETY: this thread's last error.
                    if unsafe { windows_sys::Win32::Foundation::GetLastError() }
                        != ERROR_INVALID_PARAMETER
                    {
                        return Err(io::Error::last_os_error());
                    }
                } else {
                    let t = Handle(t);
                    apply(t.0)?;
                }
            }
        }
        // SAFETY: as above.
        more = unsafe { Thread32Next(snap.0, &mut e) } != 0;
    }
    Ok(())
}

/// Apply [`process_sddl`] to this process (the spawn set it at creation too; holder §1.4).
pub(crate) fn restrict_process() -> io::Result<()> {
    let sd = Sd::from_sddl(&process_sddl(&user_sid_string()?))?;
    let dacl = sd.dacl()?;
    // SAFETY: this process's pseudo handle and a DACL the descriptor owns.
    let rc = unsafe {
        SetSecurityInfo(
            GetCurrentProcess(),
            SE_KERNEL_OBJECT,
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            ptr::null_mut(),
            ptr::null_mut(),
            dacl,
            ptr::null(),
        )
    };
    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::from_raw_os_error(rc as i32))
    }
}

impl Handle {
    /// The raw handle, no longer closed by this value.
    pub(crate) fn into_raw(self) -> HANDLE {
        let h = self.0;
        std::mem::forget(self);
        h
    }
}

/// The processes attached to this process's console; empty when it has none (T51, T52).
pub(crate) fn console_processes() -> Vec<u32> {
    let mut pids = vec![0u32; 64];
    loop {
        // SAFETY: writes at most `pids.len()` pids and returns how many there are.
        let n = unsafe { GetConsoleProcessList(pids.as_mut_ptr(), pids.len() as u32) } as usize;
        if n == 0 {
            return Vec::new();
        }
        if n <= pids.len() {
            pids.truncate(n);
            return pids;
        }
        pids.resize(n, 0);
    }
}

/// A manual-reset event, unsignalled.
pub(crate) fn event() -> io::Result<Handle> {
    // SAFETY: an unnamed event, owned below.
    let h = unsafe { CreateEventW(ptr::null(), 1, 0, ptr::null()) };
    if h.is_null() {
        return Err(io::Error::last_os_error());
    }
    Ok(Handle(h))
}

/// `%SystemRoot%` from the system, not the environment (the holder's only variable).
pub(crate) fn windows_dir() -> io::Result<Vec<u16>> {
    dir(|buf, len| unsafe { GetSystemWindowsDirectoryW(buf, len) })
}

/// `System32`, the holder's working directory (it pins no user directory).
pub(crate) fn system_dir() -> io::Result<Vec<u16>> {
    dir(|buf, len| unsafe { GetSystemDirectoryW(buf, len) })
}

fn dir(get: impl Fn(*mut u16, u32) -> u32) -> io::Result<Vec<u16>> {
    let mut buf = vec![0u16; 260];
    let n = get(buf.as_mut_ptr(), buf.len() as u32) as usize;
    if n == 0 || n >= buf.len() {
        return Err(io::Error::last_os_error());
    }
    buf.truncate(n);
    Ok(buf)
}

/// The image path of process `pid` (T53's identity check; `lock now`'s fallback).
pub(crate) fn image_path(pid: u32) -> io::Result<std::path::PathBuf> {
    use std::os::windows::ffi::OsStringExt;
    // SAFETY: a query-only handle, owned below.
    let h = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if h.is_null() {
        return Err(io::Error::last_os_error());
    }
    let h = Handle(h);
    let mut buf = vec![0u16; 1024];
    let mut len = buf.len() as u32;
    // SAFETY: writes at most `len` UTF-16 units.
    if unsafe { QueryFullProcessImageNameW(h.0, PROCESS_NAME_WIN32, buf.as_mut_ptr(), &mut len) }
        == 0
    {
        return Err(io::Error::last_os_error());
    }
    buf.truncate(len as usize);
    Ok(std::path::PathBuf::from(std::ffi::OsString::from_wide(
        &buf,
    )))
}

/// `lock now` on a hung holder (T59, S9): open the process first — a handle pins it — then
/// check the pipe's server is still that pid, then terminate through the handle.
pub(crate) fn kill_server(pipe: &Handle, pid: u32) -> io::Result<()> {
    // SAFETY: PROCESS_TERMINATE is the user's on a holder (T54's DACL); owned below.
    let h = unsafe {
        OpenProcess(
            PROCESS_TERMINATE | PROCESS_QUERY_LIMITED_INFORMATION,
            0,
            pid,
        )
    };
    if h.is_null() {
        return Err(io::Error::last_os_error());
    }
    let h = Handle(h);
    let mut now = 0u32;
    // SAFETY: the client's pipe handle; writes one pid (Task 6 P2).
    if unsafe { GetNamedPipeServerProcessId(pipe.0, &mut now) } == 0 || now != pid {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "the holder already let go of this pipe",
        ));
    }
    // SAFETY: the handle above.
    if unsafe { TerminateProcess(h.0, 1) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// The pid of the server at the other end of a client's pipe (Task 6 P2).
pub(crate) fn server_pid(pipe: &Handle) -> Option<u32> {
    let mut pid = 0u32;
    // SAFETY: the client's pipe handle; writes one pid.
    (unsafe { GetNamedPipeServerProcessId(pipe.0, &mut pid) } != 0).then_some(pid)
}

/// Overlapped reads and writes on a pipe, each bounded by the per-operation timeout and, when
/// set, a deadline on the window's clock.
pub(crate) struct PipeIo {
    pipe: HANDLE,
    event: Handle,
    per_op: Duration,
    deadline: Option<Duration>,
}

impl PipeIo {
    pub(crate) fn new(
        pipe: HANDLE,
        per_op: Duration,
        deadline: Option<Duration>,
    ) -> io::Result<PipeIo> {
        Ok(PipeIo {
            pipe,
            event: event()?,
            per_op,
            deadline,
        })
    }

    fn timeout_ms(&self) -> io::Result<u32> {
        let mut t = self.per_op;
        if let Some(d) = self.deadline {
            let left = d.saturating_sub(clock_with_sleep());
            if left.is_zero() {
                return Err(io::ErrorKind::TimedOut.into());
            }
            t = t.min(left);
        }
        Ok(u32::try_from(t.as_millis()).unwrap_or(u32::MAX))
    }

    fn op(&mut self, read: bool, buf: *mut u8, len: u32) -> io::Result<usize> {
        let ms = self.timeout_ms()?;
        // SAFETY: an all-zero OVERLAPPED is valid; it lives until the operation completes or is
        // cancelled and waited for below.
        let mut ov: OVERLAPPED = unsafe { std::mem::zeroed() };
        ov.hEvent = self.event.0;
        // SAFETY: `buf` holds `len` bytes for this call's duration.
        let ok = unsafe {
            if read {
                ReadFile(self.pipe, buf, len, ptr::null_mut(), &mut ov)
            } else {
                WriteFile(self.pipe, buf, len, ptr::null_mut(), &mut ov)
            }
        };
        if ok == 0 {
            // SAFETY: the error of the call above.
            match unsafe { GetLastError() } {
                ERROR_IO_PENDING => {}
                ERROR_BROKEN_PIPE => return Ok(0),
                e => return Err(io::Error::from_raw_os_error(e as i32)),
            }
        }
        let mut n = 0u32;
        // SAFETY: the operation above, waited for at most `ms`.
        if unsafe { GetOverlappedResultEx(self.pipe, &ov, &mut n, ms, 0) } == 0 {
            // SAFETY: the error of the call above.
            let e = unsafe { GetLastError() };
            if e == WAIT_TIMEOUT {
                // SAFETY: cancel, then wait for the cancellation so `ov` may go.
                unsafe {
                    CancelIoEx(self.pipe, &ov);
                    GetOverlappedResult(self.pipe, &ov, &mut n, 1);
                }
                return Err(io::ErrorKind::TimedOut.into());
            }
            if e == ERROR_BROKEN_PIPE {
                return Ok(0);
            }
            return Err(io::Error::from_raw_os_error(e as i32));
        }
        Ok(n as usize)
    }
}

impl io::Read for PipeIo {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let len = buf.len().min(u32::MAX as usize) as u32;
        self.op(true, buf.as_mut_ptr(), len)
    }
}

impl io::Write for PipeIo {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let len = buf.len().min(u32::MAX as usize) as u32;
        self.op(false, buf.as_ptr().cast_mut(), len)
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// A holder's record of itself where `Spawn`'s `Drop` and the CI step look (G8), removed when
/// it ends.
pub(crate) struct PidFile(std::path::PathBuf);

impl PidFile {
    /// Records `pid` in `base`, the runtime dir (`<store>\run`, or one under Local AppData),
    /// made with the owner-only DACL when it is missing (WI-479), its parents as they inherit.
    pub(crate) fn write(base: &Path, id: &impl std::fmt::Display, pid: u32) -> io::Result<PidFile> {
        cli_core::owner_only::create_dir_all_from(base, base)?;
        let path = base.join(format!("h-{id}-{pid}.pid"));
        std::fs::write(&path, format!("{pid}\n"))?;
        Ok(PidFile(path))
    }
}

impl Drop for PidFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// The number of ACEs in this process's DACL when it is protected (tests: the DACL took).
#[cfg(test)]
pub(crate) fn own_dacl_aces() -> io::Result<u32> {
    // Test-only names, imported here: at the module's top they would be unused in the lib
    // build and fail the Windows clippy (`-D warnings`).
    use windows_sys::Win32::Security::Authorization::GetSecurityInfo;
    use windows_sys::Win32::Security::{
        AclSizeInformation, GetAclInformation, GetSecurityDescriptorControl, ACL_SIZE_INFORMATION,
        SE_DACL_PROTECTED,
    };
    let mut dacl: *mut ACL = ptr::null_mut();
    let mut sd: PSECURITY_DESCRIPTOR = ptr::null_mut();
    // SAFETY: reads this process's DACL into a LocalAlloc'd descriptor, freed below.
    let rc = unsafe {
        GetSecurityInfo(
            GetCurrentProcess(),
            SE_KERNEL_OBJECT,
            DACL_SECURITY_INFORMATION,
            ptr::null_mut(),
            ptr::null_mut(),
            &mut dacl,
            ptr::null_mut(),
            &mut sd,
        )
    };
    if rc != 0 {
        return Err(io::Error::from_raw_os_error(rc as i32));
    }
    let owned = Sd(sd);
    let (mut control, mut revision) = (0u16, 0u32);
    // SAFETY: the descriptor above.
    unsafe { GetSecurityDescriptorControl(owned.0, &mut control, &mut revision) };
    let mut info: ACL_SIZE_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: the DACL inside `owned`.
    let ok = unsafe {
        GetAclInformation(
            dacl,
            (&raw mut info).cast(),
            std::mem::size_of::<ACL_SIZE_INFORMATION>() as u32,
            AclSizeInformation,
        )
    };
    if ok == 0 || control & SE_DACL_PROTECTED == 0 {
        return Ok(0);
    }
    Ok(info.AceCount)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// WI-479: a runtime dir the pid file makes has the owner-only DACL, its parents inherit
    /// theirs, and the file goes with the holder.
    #[test]
    fn a_pid_file_makes_its_runtime_dir_owner_only() {
        use cli_core::owner_only::testing;
        let scratch = tempfile::tempdir().unwrap();
        let base = scratch.path().join("apprafter").join("run");
        let file = PidFile::write(&base, &"h", 7).unwrap();
        testing::of_path(&base)
            .unwrap()
            .assert_owner_only_dir("the runtime dir");
        assert!(
            !testing::of_path(&scratch.path().join("apprafter"))
                .unwrap()
                .protected
        );
        let path = base.join("h-h-7.pid");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "7\n");
        drop(file);
        assert!(!path.exists());
    }
}
