// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Windows calls of the holder (D.4a T51–T56; holder §1.4, §2.3, §4).

use std::io;
use std::ptr;
use std::time::Duration;

use windows_sys::Win32::Foundation::{CloseHandle, LocalFree, HANDLE};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SetSecurityInfo,
    SDDL_REVISION_1, SE_KERNEL_OBJECT,
};
use windows_sys::Win32::Security::{
    GetSecurityDescriptorDacl, GetTokenInformation, TokenLogonSid, TokenUser, ACL,
    DACL_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR,
    TOKEN_GROUPS, TOKEN_QUERY, TOKEN_USER,
};
use windows_sys::Win32::System::SystemInformation::GetTickCount64;
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
use windows_sys::Win32::System::WindowsProgramming::QueryUnbiasedInterruptTime;

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
