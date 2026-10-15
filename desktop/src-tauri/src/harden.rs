// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! What the app does to its own process before it holds any secret (D.4a T77). Whatever the
//! store's state — a store can be locked from a terminal while the app runs — and in builds
//! without `debug_assertions` only: a developer attaches a debugger to a debug build, which
//! reports every measure not taken ([`not_applied`]). Each measure is best effort and reported,
//! never fatal: the Security page says which did not take (rule 1, `HolderHardening`).
//!
//! - Linux: `PR_SET_DUMPABLE` 0 — no same-user `ptrace` attach, `/proc/<pid>/mem` closed;
//! - macOS: `PT_DENY_ATTACH` — no later debugger attach;
//! - Unix: no core file (`RLIMIT_CORE` 0), claimed only where it holds ([`no_core`]: a piped
//!   `core_pattern` decides for itself, unless the process is non-dumpable); Windows: no fault
//!   dialog (`SetErrorMode`), and `core_off` stays false — Windows Error Reporting or
//!   LocalDumps may still write a dump;
//! - Windows: the holder's restricted process DACL (T54) — the account may wait on, query and
//!   end the app, nothing more, no `PROCESS_VM_READ`. The process object only: the threads keep
//!   the token's default DACL, so a same-user program can still take one over — the
//!   `windows_same_user` row says so. Whether WebView2 still works under it is a Windows walk
//!   row (overview §8 risk 8);
//! - the data key's page is locked in memory while the app holds it ([`lock_page`]), so it is
//!   not paged out — under the OS's limit; `memory_locked` is that page's state, which
//!   `lock_info` measures (this report carries `false`).
//!
//! It runs before any log subscriber exists: [`log`] says what it did once logging is up.
//!
//! None of it stops root, a process with `CAP_SYS_PTRACE` or `SeDebugPrivilege`, or a same-user
//! program with the macOS debugging entitlement (the `macos_task_port` row); hibernation writes
//! memory to disk regardless (the `hibernation` row). WebKit's web process, where the lock
//! password is typed, is a child the app does not harden (T78, the `ptrace_scope` and Windows
//! rows).

use apprafter_core::lock::HolderHardening;

/// What [`harden_process`] did: the report the Security page shows, and why a measure did not
/// take, for the log (the Rust verifier's finding 13: no subscriber exists while it runs).
#[derive(Debug, Clone)]
pub struct Hardened {
    pub hardening: HolderHardening,
    pub why_not: Vec<String>,
}

/// Harden this process (module docs); what took. A debug build hardens nothing. Logs nothing:
/// no subscriber exists yet ([`log`]).
pub fn harden_process() -> Hardened {
    let mut why_not = Vec::new();
    if cfg!(debug_assertions) {
        return Hardened {
            hardening: not_applied(),
            why_not,
        };
    }
    let non_dumpable = non_dumpable();
    let hardening = HolderHardening {
        non_dumpable,
        deny_attach: deny_attach(),
        core_off: core_off(non_dumpable),
        // The key's page, measured when lock_info asks (vault_access); none is held yet.
        memory_locked: false,
        process_dacl: process_dacl(&mut why_not),
    };
    Hardened { hardening, why_not }
}

/// What [`harden_process`] did, and why a measure did not take, logged once `run` has started
/// the log.
pub fn log(hardened: &Hardened) {
    if cfg!(debug_assertions) {
        tracing::warn!("a debug build: the process is not hardened (D.4a T77)");
        return;
    }
    let hardening = &hardened.hardening;
    tracing::info!(?hardening, "the process is hardened (D.4a T77)");
    for why in &hardened.why_not {
        tracing::warn!("{why}");
    }
}

/// Every measure this OS has, not taken: a debug build's report, and the tests' shells'.
pub fn not_applied() -> HolderHardening {
    HolderHardening {
        non_dumpable: cfg!(target_os = "linux").then_some(false),
        deny_attach: cfg!(target_os = "macos").then_some(false),
        core_off: false,
        memory_locked: false,
        process_dacl: cfg!(windows).then_some(false),
    }
}

#[cfg(target_os = "linux")]
fn non_dumpable() -> Option<bool> {
    // SAFETY: PR_SET_DUMPABLE and PR_GET_DUMPABLE change and read this process's flag only.
    let set = unsafe { libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0) } == 0;
    // SAFETY: as above.
    Some(set && unsafe { libc::prctl(libc::PR_GET_DUMPABLE, 0, 0, 0, 0) } == 0)
}

#[cfg(not(target_os = "linux"))]
fn non_dumpable() -> Option<bool> {
    None
}

#[cfg(target_os = "macos")]
fn deny_attach() -> Option<bool> {
    // SAFETY: PT_DENY_ATTACH takes no address or data.
    Some(unsafe { libc::ptrace(libc::PT_DENY_ATTACH, 0, std::ptr::null_mut(), 0) } == 0)
}

#[cfg(not(target_os = "macos"))]
fn deny_attach() -> Option<bool> {
    None
}

/// `RLIMIT_CORE` 0; whether the OS took it. Async-signal-safe (the forked test calls it).
#[cfg(unix)]
fn limit_core() -> bool {
    let zero = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: setrlimit reads one rlimit.
    unsafe { libc::setrlimit(libc::RLIMIT_CORE, &zero) == 0 }
}

/// Whether a crash leaves no core: a non-dumpable process is never dumped (the kernel checks
/// the flag before `core_pattern`; `suid_dumpable` concerns processes a credential change
/// marked, not one that cleared its own flag); otherwise the zero limit holds only for a file
/// pattern — a piped one (systemd-coredump, apport) gets the dump and decides for itself.
#[cfg_attr(not(unix), allow(dead_code))]
fn no_core(limit_zero: bool, piped_pattern: bool, non_dumpable: Option<bool>) -> bool {
    non_dumpable == Some(true) || (limit_zero && !piped_pattern)
}

/// Whether `kernel.core_pattern` hands the dump to a program.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn piped(pattern: &str) -> bool {
    pattern.trim_start().starts_with('|')
}

#[cfg(unix)]
fn core_off(non_dumpable: Option<bool>) -> bool {
    let limit_zero = limit_core();
    // Linux: an unreadable pattern counts as piped (claim nothing unknown); macOS has none.
    #[cfg(target_os = "linux")]
    let piped_pattern = match std::fs::read_to_string("/proc/sys/kernel/core_pattern") {
        Ok(pattern) => piped(&pattern),
        Err(_) => true,
    };
    #[cfg(not(target_os = "linux"))]
    let piped_pattern = false;
    no_core(limit_zero, piped_pattern, non_dumpable)
}

/// Hides the fault dialog, and claims nothing: Windows Error Reporting or LocalDumps may still
/// write a dump, so a crash may write the app's memory to disk (rule 1).
#[cfg(windows)]
fn core_off(_non_dumpable: Option<bool>) -> bool {
    use windows_sys::Win32::System::Diagnostics::Debug::{
        SetErrorMode, SEM_FAILCRITICALERRORS, SEM_NOGPFAULTERRORBOX,
    };
    // SAFETY: SetErrorMode only sets this process's mode.
    unsafe { SetErrorMode(SEM_FAILCRITICALERRORS | SEM_NOGPFAULTERRORBOX) };
    false
}

#[cfg(windows)]
fn process_dacl(why_not: &mut Vec<String>) -> Option<bool> {
    Some(match windows::restrict_process() {
        Ok(()) => true,
        Err(e) => {
            why_not.push(format!("the process DACL was not restricted: {e}"));
            false
        }
    })
}

#[cfg(not(windows))]
fn process_dacl(_why_not: &mut Vec<String>) -> Option<bool> {
    None
}

/// Lock the page(s) under `bytes` in memory; whether it took (best effort, under the OS's
/// limit). The data key's, while the app holds it (`vault_access`).
#[cfg(unix)]
pub fn lock_page(bytes: &[u8]) -> bool {
    // SAFETY: an address range this process owns, for its length.
    unsafe { libc::mlock(bytes.as_ptr().cast(), bytes.len()) == 0 }
}

#[cfg(windows)]
pub fn lock_page(bytes: &[u8]) -> bool {
    // SAFETY: an address range this process owns, for its length.
    unsafe {
        windows_sys::Win32::System::Memory::VirtualLock(bytes.as_ptr().cast(), bytes.len()) != 0
    }
}

/// Undo [`lock_page`] for `bytes`, before the key goes.
#[cfg(unix)]
pub fn unlock_page(bytes: &[u8]) {
    // SAFETY: as in lock_page.
    unsafe {
        libc::munlock(bytes.as_ptr().cast(), bytes.len());
    }
}

#[cfg(windows)]
pub fn unlock_page(bytes: &[u8]) {
    // SAFETY: as in lock_page.
    unsafe {
        windows_sys::Win32::System::Memory::VirtualUnlock(bytes.as_ptr().cast(), bytes.len());
    }
}

/// Windows: the holder's process DACL (T54), applied to the app.
#[cfg(windows)]
mod windows {
    use std::io;
    use std::ptr;

    use windows_sys::Win32::Foundation::{CloseHandle, LocalFree, HANDLE};
    use windows_sys::Win32::Security::Authorization::{
        ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
        SetSecurityInfo, SDDL_REVISION_1, SE_KERNEL_OBJECT,
    };
    use windows_sys::Win32::Security::{
        GetSecurityDescriptorDacl, GetTokenInformation, TokenUser, ACL, DACL_SECURITY_INFORMATION,
        PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, TOKEN_QUERY, TOKEN_USER,
    };
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    /// The user may wait on, query and end the app (`SYNCHRONIZE |
    /// PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_TERMINATE`); `OWNER RIGHTS` only reads its
    /// security, which removes the owner's implicit `WRITE_DAC`. No `PROCESS_VM_READ`.
    pub(super) fn process_sddl(user: &str) -> String {
        format!("D:P(A;;0x00101001;;;{user})(A;;RC;;;OW)")
    }

    pub(super) fn restrict_process() -> io::Result<()> {
        let wide: Vec<u16> = process_sddl(&user_sid()?)
            .encode_utf16()
            .chain(Some(0))
            .collect();
        let mut sd: PSECURITY_DESCRIPTOR = ptr::null_mut();
        // SAFETY: a NUL-terminated string; the descriptor is LocalAlloc'd and freed below.
        let made = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                wide.as_ptr(),
                SDDL_REVISION_1,
                &mut sd,
                ptr::null_mut(),
            )
        };
        if made == 0 {
            return Err(io::Error::last_os_error());
        }
        let applied = apply(sd);
        // SAFETY: allocated by the conversion above, freed once.
        unsafe { LocalFree(sd.cast()) };
        applied
    }

    fn apply(sd: PSECURITY_DESCRIPTOR) -> io::Result<()> {
        let (mut present, mut defaulted) = (0, 0);
        let mut dacl: *mut ACL = ptr::null_mut();
        // SAFETY: a descriptor the caller owns; the DACL points into it.
        if unsafe { GetSecurityDescriptorDacl(sd, &mut present, &mut dacl, &mut defaulted) } == 0
            || present == 0
        {
            return Err(io::Error::last_os_error());
        }
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

    /// This process's user, as an SDDL SID string.
    fn user_sid() -> io::Result<String> {
        let mut token: HANDLE = ptr::null_mut();
        // SAFETY: this process's pseudo handle; the token handle is closed below.
        if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let sid = token_user_sid(token);
        // SAFETY: the handle opened above, closed once.
        unsafe { CloseHandle(token) };
        sid
    }

    fn token_user_sid(token: HANDLE) -> io::Result<String> {
        let mut len = 0u32;
        // SAFETY: asks for the size only.
        unsafe { GetTokenInformation(token, TokenUser, ptr::null_mut(), 0, &mut len) };
        // u64s: TOKEN_USER holds a pointer, so the buffer is pointer-aligned.
        let mut buf = vec![0u64; (len as usize).div_ceil(8).max(1)];
        // SAFETY: a buffer of at least `len` bytes, aligned for TOKEN_USER.
        if unsafe { GetTokenInformation(token, TokenUser, buf.as_mut_ptr().cast(), len, &mut len) }
            == 0
        {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: GetTokenInformation wrote a TOKEN_USER at the buffer's start.
        let sid = unsafe { (*buf.as_ptr().cast::<TOKEN_USER>()).User.Sid };
        let mut text: *mut u16 = ptr::null_mut();
        // SAFETY: a SID inside `buf`; the string is LocalAlloc'd, freed below.
        if unsafe { ConvertSidToStringSidW(sid, &mut text) } == 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: a NUL-terminated string from the call above, read then freed once.
        let s = unsafe {
            let n = (0..).take_while(|i| *text.add(*i) != 0).count();
            let s = String::from_utf16_lossy(std::slice::from_raw_parts(text, n));
            LocalFree(text.cast());
            s
        };
        Ok(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A debug build hardens nothing and says so, measure by measure, for this OS.
    #[cfg(debug_assertions)]
    #[test]
    fn a_debug_build_reports_every_measure_not_taken() {
        let h = harden_process().hardening;
        assert_eq!(h, not_applied());
        assert!(!h.core_off && !h.memory_locked);
        assert_eq!(h.non_dumpable, cfg!(target_os = "linux").then_some(false));
        assert_eq!(h.deny_attach, cfg!(target_os = "macos").then_some(false));
        assert_eq!(h.process_dacl, cfg!(windows).then_some(false));
    }

    /// The measures themselves, each in a forked child (they cannot be undone, and the test
    /// process must stay debuggable): the child applies one and exits with what the OS reads
    /// back. `fork` in a threaded process: the child calls only `prctl`/`setrlimit`/`getrlimit`
    /// and `_exit`, all async-signal-safe.
    #[cfg(unix)]
    fn in_child(measure: fn() -> bool) -> bool {
        // SAFETY: see above; the child never returns from this block.
        unsafe {
            match libc::fork() {
                0 => libc::_exit(if measure() { 0 } else { 1 }),
                -1 => panic!("fork: {}", std::io::Error::last_os_error()),
                pid => {
                    let mut status = 0;
                    assert_eq!(libc::waitpid(pid, &mut status, 0), pid);
                    libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0
                }
            }
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn the_process_turns_non_dumpable() {
        assert!(in_child(|| {
            let said = non_dumpable() == Some(true);
            // SAFETY: reads this process's flag.
            let read = unsafe { libc::prctl(libc::PR_GET_DUMPABLE, 0, 0, 0, 0) };
            said && read == 0
        }));
    }

    #[cfg(unix)]
    #[test]
    fn no_core_file_is_written() {
        assert!(in_child(|| {
            // `limit_core` alone: async-signal-safe (`core_off` also reads a file).
            let set = limit_core();
            let mut now = libc::rlimit {
                rlim_cur: 1,
                rlim_max: 1,
            };
            // SAFETY: writes one rlimit.
            let read = unsafe { libc::getrlimit(libc::RLIMIT_CORE, &mut now) } == 0;
            // Both limits: where the soft one is 0 already (systemd's `LimitCORE=0:infinity`, a CI
            // runner), the hard one still tells whether the measure ran (the Rust verifier's
            // finding 12).
            set && read && now.rlim_cur == 0 && now.rlim_max == 0
        }));
    }

    /// `core_off` claims only what holds (critic B finding 20): a non-dumpable process is never
    /// dumped; otherwise the zero limit holds only for a file pattern, and a piped one decides
    /// for itself.
    #[test]
    fn a_crash_leaves_no_core_only_where_the_measures_hold() {
        assert!(no_core(true, false, None));
        assert!(!no_core(true, true, None));
        assert!(no_core(false, true, Some(true)));
        assert!(!no_core(true, true, Some(false)));
        assert!(!no_core(false, false, Some(false)));
    }

    #[test]
    fn a_core_pattern_that_starts_with_a_pipe_is_piped() {
        assert!(piped(
            "|/usr/lib/systemd/systemd-coredump %P %u %g %s %t %c %h"
        ));
        assert!(piped("  |/usr/share/apport/apport -p%p"));
        assert!(!piped("core"));
        assert!(!piped("/var/crash/core.%e.%p"));
    }

    /// The key's page locks under the OS's limit, and unlocks again: in a child whose
    /// `RLIMIT_MEMLOCK` is 64 KiB (root's too), a 32-byte key's page locks.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_page_locks_under_the_os_limit_and_unlocks() {
        assert!(in_child(|| {
            let limit = libc::rlimit {
                rlim_cur: 65_536,
                rlim_max: 65_536,
            };
            // SAFETY: setrlimit reads one rlimit.
            let set = unsafe { libc::setrlimit(libc::RLIMIT_MEMLOCK, &limit) } == 0;
            let key = [7u8; 32];
            let locked = lock_page(&key[..]);
            unlock_page(&key[..]);
            set && locked
        }));
    }

    #[cfg(windows)]
    #[test]
    fn the_process_dacl_is_the_holder_s() {
        assert_eq!(
            windows::process_sddl("S-1-5-21-1"),
            "D:P(A;;0x00101001;;;S-1-5-21-1)(A;;RC;;;OW)"
        );
    }
}
