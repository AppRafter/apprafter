// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Whether this process runs with no_new_privs (WI-545; D.4b B88 (3)). Under it — bubblewrap,
//! Flatpak, a systemd service with `NoNewPrivileges=`, `docker --security-opt
//! no-new-privileges`, Kubernetes' `allowPrivilegeEscalation: false` — a set-uid or set-gid
//! program runs without its privileges: `pam_unix`'s `unix_chkpwd` cannot read `/etc/shadow`
//! and answers `PAM_AUTHINFO_UNAVAIL`, which the shipped stacks (`pam_deny` after `pam_unix`)
//! turn into `PAM_AUTH_ERR` — the right password reads as wrong and is counted (the e1a walk's
//! W3x(a), Debian 13 and Fedora 42). So no check runs there: `Unavailable::NoNewPrivs`.
//!
//! Linux: the `NoNewPrivs:` line of `/proc/self/status`; where that file says nothing (no
//! `/proc` mounted, a kernel before 4.10), the same flag through `prctl(PR_GET_NO_NEW_PRIVS)`,
//! which needs no `/proc` — on a kernel before 3.5 that call fails, and such a kernel cannot set
//! the flag. Other systems have no such flag.

/// Where Linux tells it.
#[cfg(target_os = "linux")]
const PROC_STATUS: &str = "/proc/self/status";

/// This process's flag (module docs).
#[cfg(target_os = "linux")]
pub(crate) fn system_no_new_privs() -> bool {
    let status = std::fs::read_to_string(PROC_STATUS).ok();
    no_new_privs_from(status.as_deref(), prctl_no_new_privs)
}

/// No such flag off Linux.
#[cfg(not(target_os = "linux"))]
pub(crate) fn system_no_new_privs() -> bool {
    false
}

/// The flag from a `/proc/<pid>/status` text when it says, else from `prctl`.
#[cfg(any(target_os = "linux", test))]
pub(crate) fn no_new_privs_from(status: Option<&str>, prctl: impl FnOnce() -> bool) -> bool {
    status.and_then(parse_no_new_privs).unwrap_or_else(prctl)
}

/// The `NoNewPrivs:` line of a `/proc/<pid>/status` text: `Some(true)` for `1`, `Some(false)`
/// for `0`, `None` without the line or with any other value.
#[cfg(any(target_os = "linux", test))]
pub(crate) fn parse_no_new_privs(status: &str) -> Option<bool> {
    status
        .lines()
        .find_map(|line| match line.strip_prefix("NoNewPrivs:")?.trim() {
            "1" => Some(true),
            "0" => Some(false),
            _ => None,
        })
}

/// `prctl(PR_GET_NO_NEW_PRIVS)`: 1 when set, 0 when not, -1 where the kernel has no such flag.
#[cfg(target_os = "linux")]
fn prctl_no_new_privs() -> bool {
    // The kernel refuses this option unless arguments 2 to 5 are zero, and reads each as an
    // unsigned long: pass them so, never as 32-bit literals whose upper halves a variadic call
    // leaves undefined.
    let zero: libc::c_ulong = 0;
    // SAFETY: PR_GET_NO_NEW_PRIVS reads one flag of the calling thread; it takes no pointer and
    // writes no memory.
    unsafe { libc::prctl(libc::PR_GET_NO_NEW_PRIVS, zero, zero, zero, zero) == 1 }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A kernel's `/proc/self/status`, cut to the lines around `flag`.
    fn status(flag: &str) -> String {
        format!(
            "Name:\tapprafter\nUmask:\t0022\nState:\tR (running)\nCapAmb:\t0000000000000000\n\
             {flag}Seccomp:\t2\nSeccomp_filters:\t1\n"
        )
    }

    #[test]
    fn the_status_line_says_whether_the_flag_is_set() {
        assert_eq!(parse_no_new_privs(&status("NoNewPrivs:\t1\n")), Some(true));
        assert_eq!(parse_no_new_privs(&status("NoNewPrivs:\t0\n")), Some(false));
        assert_eq!(parse_no_new_privs(&status("NoNewPrivs:   1\n")), Some(true));
        for unclear in [
            status(""),
            status("NoNewPrivs:\t\n"),
            status("NoNewPrivs:\t2\n"),
            status("NoNewPrivsX:\t1\n"),
            String::new(),
        ] {
            assert_eq!(parse_no_new_privs(&unclear), None, "{unclear:?}");
        }
    }

    #[test]
    fn the_status_text_answers_first_and_prctl_only_where_it_says_nothing() {
        let set = status("NoNewPrivs:\t1\n");
        let clear = status("NoNewPrivs:\t0\n");
        let silent = status("");
        assert!(no_new_privs_from(Some(set.as_str()), || unreachable!(
            "the text said"
        )));
        assert!(!no_new_privs_from(Some(clear.as_str()), || unreachable!(
            "the text said"
        )));
        for prctl in [true, false] {
            assert_eq!(no_new_privs_from(Some(silent.as_str()), || prctl), prctl);
            assert_eq!(no_new_privs_from(None, || prctl), prctl);
        }
    }

    /// The two readings are one flag: whatever this process's is — a test run inside an
    /// agent's sandbox may have it set, CI's has not — `/proc/self/status` and `prctl` agree.
    /// It reads the flag and never sets it.
    #[cfg(target_os = "linux")]
    #[test]
    fn proc_and_prctl_read_the_same_flag() {
        let text = std::fs::read_to_string(PROC_STATUS).expect("Linux has /proc/self/status");
        let from_proc = parse_no_new_privs(&text).expect("Linux 4.10 and later print NoNewPrivs");
        assert_eq!(prctl_no_new_privs(), from_proc);
        assert_eq!(system_no_new_privs(), from_proc);
    }
}
