// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! What a holder does to itself before it holds a key (ADR 0068 §4; D.4a T54, holder §4). Each
//! measure is best effort and reported in `Status`, never fatal: `lock status` and doctor's
//! `window_hardening` row say when one failed (rule 1). `memory_locked` is the key page's, set
//! by the caller once the page exists.
//!
//! On Windows the DACL also goes on every thread of the holder's own and becomes the token's
//! default for threads it makes later; a thread another process makes in it takes its creator's
//! default (the console's control handler, whose `CTRL_CLOSE_EVENT` wipes the key page first).
//!
//! It does not stop root, a process with `CAP_SYS_PTRACE` or `SeDebugPrivilege`, or a same-user
//! process with the macOS debugging entitlement (the `macos_task_port` limit).

use apprafter_core::lock::HolderHardening;

/// Harden this process (module docs). On Unix also `umask(0o077)`: every file it makes is
/// private.
pub fn harden_self() -> HolderHardening {
    #[cfg(unix)]
    // SAFETY: umask cannot fail.
    unsafe {
        libc::umask(0o077);
    }
    HolderHardening {
        non_dumpable: non_dumpable(),
        deny_attach: deny_attach(),
        core_off: core_off(),
        memory_locked: false,
        process_dacl: process_dacl(),
    }
}

/// Linux: no same-uid `ptrace` attach, no `/proc/<pid>/mem`, `/proc/<pid>/*` owned by root.
/// Every exec resets it, so the holder sets it itself (holder finding 6).
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

/// macOS: refuse every later `ptrace` attach (KeePassXC does the same).
#[cfg(target_os = "macos")]
fn deny_attach() -> Option<bool> {
    // SAFETY: PT_DENY_ATTACH takes no address or data.
    Some(unsafe { libc::ptrace(libc::PT_DENY_ATTACH, 0, std::ptr::null_mut(), 0) } == 0)
}

#[cfg(not(target_os = "macos"))]
fn deny_attach() -> Option<bool> {
    None
}

#[cfg(unix)]
fn core_off() -> bool {
    let zero = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: setrlimit reads one rlimit.
    unsafe { libc::setrlimit(libc::RLIMIT_CORE, &zero) == 0 }
}

/// Windows: no fault dialog, no critical-error box (Windows Error Reporting may still collect a
/// minidump: holder §4, UNVERIFIED whether one could hold the key page).
#[cfg(windows)]
fn core_off() -> bool {
    use windows_sys::Win32::System::Diagnostics::Debug::{
        SetErrorMode, SEM_FAILCRITICALERRORS, SEM_NOGPFAULTERRORBOX,
    };
    // SAFETY: SetErrorMode only sets this process's mode.
    unsafe { SetErrorMode(SEM_FAILCRITICALERRORS | SEM_NOGPFAULTERRORBOX) };
    true
}

#[cfg(windows)]
fn process_dacl() -> Option<bool> {
    let process = super::os::windows::restrict_process().is_ok();
    Some(process && super::os::windows::restrict_threads().is_ok())
}

#[cfg(not(windows))]
fn process_dacl() -> Option<bool> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const INNER: &str = "APPRAFTER_HARDEN_INNER";

    /// T54: in a child (the measures change the process they run in), the report is what the
    /// OS then says: Linux not dumpable, macOS denying attach, no core dumps, a private umask;
    /// Windows its protected DACL. A failed measure is reported, never hidden.
    #[test]
    fn a_hardened_process_reports_what_the_os_then_says() {
        // Forced: a core soft limit above 0 to start from (a runner may already have 0), so a
        // holder that skips setrlimit shows it. The child inherits it. Only the soft limit moves,
        // under the hard one this process has (raising the hard limit needs privilege: critic-d4a-e
        // finding 20); a hard limit of 0 fails the test by name, never skips it.
        #[cfg(unix)]
        {
            let mut now = libc::rlimit {
                rlim_cur: 0,
                rlim_max: 0,
            };
            // SAFETY: getrlimit writes one rlimit.
            assert_eq!(unsafe { libc::getrlimit(libc::RLIMIT_CORE, &mut now) }, 0);
            assert!(
                now.rlim_max > 0,
                "the core hard limit is 0 here: a skipped setrlimit could not show"
            );
            let one = libc::rlimit {
                rlim_cur: 1,
                rlim_max: now.rlim_max,
            };
            // SAFETY: setrlimit reads one rlimit.
            let raised = unsafe { libc::setrlimit(libc::RLIMIT_CORE, &one) } == 0;
            assert!(
                raised,
                "the core soft limit can be set to 1 under the hard limit"
            );
        }
        // The child's environment is the test's own, nothing of whoever runs it (critic-d4a-e
        // finding 24); Windows keeps `SystemRoot`, as the holder's own start does.
        let mut child = std::process::Command::new(std::env::current_exe().unwrap());
        child.env_clear().env(INNER, "1");
        #[cfg(windows)]
        if let Some(root) = std::env::var_os("SystemRoot") {
            child.env("SystemRoot", root);
        }
        let out = child
            .args([
                "--exact",
                "holder::harden::tests::inner_report",
                "--ignored",
                "--nocapture",
                "--test-threads=1",
            ])
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);
        // libtest prints `test <name> ... ` before the test runs and its result after, so with
        // --nocapture the report shares a line with that prefix: look for it within each line.
        let line = stdout
            .lines()
            .find_map(|l| l.split_once("harden: ").map(|(_, report)| report))
            .unwrap_or_else(|| panic!("the inner test ran: {stdout}"));
        #[cfg(target_os = "linux")]
        assert_eq!(
            line,
            "nd=Some(true) da=None core=true dacl=None real_dumpable=0 real_core=0 umask=77"
        );
        #[cfg(target_os = "macos")]
        assert_eq!(
            line,
            "nd=None da=Some(true) core=true dacl=None real_core=0 umask=77"
        );
        #[cfg(windows)]
        assert_eq!(
            line,
            "nd=None da=None core=true dacl=Some(true) protected_aces=2"
        );
    }

    #[test]
    #[ignore = "runs only inside a_hardened_process_reports_what_the_os_then_says"]
    fn inner_report() {
        assert!(
            std::env::var_os(INNER).is_some(),
            "run only by a_hardened_process_reports_what_the_os_then_says"
        );
        let h = harden_self();
        let head = format!(
            "nd={:?} da={:?} core={} dacl={:?}",
            h.non_dumpable, h.deny_attach, h.core_off, h.process_dacl
        );
        #[cfg(unix)]
        {
            let mut rl = libc::rlimit {
                rlim_cur: 1,
                rlim_max: 1,
            };
            // SAFETY: getrlimit writes one rlimit; umask returns the old mask and sets a new one,
            // which the next call puts back.
            let (core, old) = unsafe {
                libc::getrlimit(libc::RLIMIT_CORE, &mut rl);
                let old = libc::umask(0o022);
                libc::umask(old);
                (rl.rlim_cur, old)
            };
            #[cfg(target_os = "linux")]
            // SAFETY: PR_GET_DUMPABLE reads this process's flag.
            let dumpable = unsafe { libc::prctl(libc::PR_GET_DUMPABLE, 0, 0, 0, 0) };
            #[cfg(target_os = "linux")]
            println!("harden: {head} real_dumpable={dumpable} real_core={core} umask={old:o}");
            #[cfg(target_os = "macos")]
            println!("harden: {head} real_core={core} umask={old:o}");
        }
        #[cfg(windows)]
        println!(
            "harden: {head} protected_aces={}",
            super::super::os::windows::own_dacl_aces().unwrap_or(0)
        );
    }
}
