// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! This account (`getpwuid_r(getuid())`, never a name from input and never `$HOME`) and what a
//! process that is not root can read about its password (D.4b B16, B41, B75; GOTCHA-328).
//!
//! Linux: `passwd -S <account>` works for one's own account on Debian, Fedora and NixOS (d4b-a's
//! probe) through `passwd`'s set-uid bit: `<name> P|L|NP <date> …` — the password set, locked or
//! none (`PS`, `LK` on older Fedora and RHEL too) and its last change as a day, `YYYY-MM-DD` or
//! `MM/DD/YYYY`. A day of 1970-01-02 or earlier is unknown: NixOS writes day 1 for a password
//! set in its configuration, and day 0 means "change at next login". Without its privileges —
//! no_new_privs, the set-uid bit dropped, a nosuid mount, NixOS's profile `passwd` — shadow's
//! `passwd` cannot read `/etc/shadow` and prints `<name> P` from `/etc/passwd`'s `x`
//! placeholder, whatever the password is: a line without the change-day field is unknown
//! (WI-545).
//!
//! macOS: `dscl . -read /Users/<account> AuthenticationAuthority` — "No such key" means the
//! account has no password record, so no password; anything else is unknown, since an empty
//! password reads like a set one; `accountPolicyData`'s `passwordLastSetTime` is the change time
//! in seconds.
//!
//! Each reader runs a fixed program path with a cleared environment (`LC_ALL=C`), its input
//! closed, and is killed after [`READ_LIMIT`]; whatever it does not say clearly is `Unknown`.

use std::fmt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::outcome::{PasswordChanged, PasswordState, PasswordStatus};

/// How long a status reader may run before it is killed and its answer is unknown.
pub const READ_LIMIT: Duration = Duration::from_secs(2);

/// Where `passwd` is, in the order tried: NixOS's set-uid wrapper first (its profile's `passwd`
/// is not set-uid), then the usual places.
pub const PASSWD: [&str; 3] = ["/run/wrappers/bin/passwd", "/usr/bin/passwd", "/bin/passwd"];

/// macOS's directory service tool.
pub const DSCL: &str = "/usr/bin/dscl";

/// The account a check is for: the running uid's passwd entry.
#[derive(Clone, PartialEq, Eq)]
pub struct Account {
    /// Its name, as the passwd entry has it.
    pub name: Vec<u8>,
    pub uid: u32,
    /// Its passwd home directory — never `$HOME`, which any caller can set.
    pub home: PathBuf,
}

impl Account {
    /// The name, lossily as text (for labels and messages).
    pub fn name_lossy(&self) -> String {
        String::from_utf8_lossy(&self.name).into_owned()
    }
}

impl fmt::Debug for Account {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Account")
            .field("name", &self.name_lossy())
            .field("uid", &self.uid)
            .field("home", &self.home)
            .finish()
    }
}

/// The running uid's passwd entry (`getpwuid_r(getuid())`); `None` without one.
#[cfg(unix)]
pub fn current_account() -> Option<Account> {
    use std::ffi::{CStr, OsStr};
    use std::mem::MaybeUninit;
    use std::os::unix::ffi::OsStrExt;

    /// glibc asks for 1 KiB; a directory service can need more, but not without bound.
    const FIRST: usize = 1024;
    const LAST: usize = 1 << 20;
    // SAFETY: getuid cannot fail and touches no memory.
    let uid = unsafe { libc::getuid() };
    let mut buffer = vec![0_u8; FIRST];
    loop {
        let mut entry = MaybeUninit::<libc::passwd>::uninit();
        let mut found: *mut libc::passwd = std::ptr::null_mut();
        // SAFETY: `entry` and `buffer` are valid for writes of their sizes, and getpwuid_r
        // writes no further; it points `found` at `entry` or leaves it null.
        let rc = unsafe {
            libc::getpwuid_r(
                uid,
                entry.as_mut_ptr(),
                buffer.as_mut_ptr().cast(),
                buffer.len(),
                &mut found,
            )
        };
        if rc == libc::ERANGE && buffer.len() < LAST {
            buffer.resize(buffer.len() * 2, 0);
            continue;
        }
        if rc != 0 || found.is_null() {
            return None;
        }
        // SAFETY: getpwuid_r succeeded: `found` points at the initialised `entry`, whose
        // `pw_name` and `pw_dir` are null or NUL-terminated strings inside `buffer`, still alive.
        let (name, home) = unsafe {
            let entry = &*found;
            let field = |p: *const libc::c_char| {
                (!p.is_null()).then(|| CStr::from_ptr(p).to_bytes().to_vec())
            };
            (field(entry.pw_name), field(entry.pw_dir))
        };
        let name = name.filter(|n| !n.is_empty())?;
        let home = PathBuf::from(OsStr::from_bytes(&home.unwrap_or_default()));
        return Some(Account { name, uid, home });
    }
}

/// No passwd entries here (Windows).
#[cfg(not(unix))]
pub fn current_account() -> Option<Account> {
    None
}

/// What this account's password status reads (module docs).
pub fn password_status(account: &Account) -> PasswordStatus {
    let name = account.name_lossy();
    if cfg!(target_os = "macos") {
        let user = format!("/Users/{name}");
        let read = |key: &str| run_bounded(Path::new(DSCL), &[".", "-read", user.as_str(), key]);
        PasswordStatus {
            state: read("AuthenticationAuthority")
                .map_or(PasswordState::Unknown, |out| parse_dscl_authority(&out)),
            changed: read("accountPolicyData")
                .map_or(PasswordChanged::Unknown, |out| parse_dscl_policy(&out)),
        }
    } else if cfg!(target_os = "linux") {
        PASSWD
            .iter()
            .map(Path::new)
            .find(|program| program.is_file())
            .and_then(|program| run_bounded(program, &["-S", name.as_str()]))
            .map_or(PasswordStatus::UNKNOWN, |out| {
                parse_passwd_status(&name, &out)
            })
    } else {
        PasswordStatus::UNKNOWN
    }
}

/// `passwd -S`'s line for `account` (module docs).
pub(crate) fn parse_passwd_status(account: &str, out: &str) -> PasswordStatus {
    let Some(fields) = out
        .lines()
        .map(|line| line.split_whitespace().collect::<Vec<_>>())
        .find(|f| f.first() == Some(&account))
    else {
        return PasswordStatus::UNKNOWN;
    };
    // WI-545: shadow's `passwd` prints the change day and the ages only when it read
    // /etc/shadow; without its privileges it prints `<name> P` from /etc/passwd's `x`
    // placeholder, whatever the password is. A line without the change-day field says nothing.
    if fields.len() < 3 {
        return PasswordStatus::UNKNOWN;
    }
    let state = match fields.get(1).copied() {
        Some("P" | "PS") => PasswordState::Usable,
        Some("L" | "LK") => PasswordState::Locked,
        Some("NP") => PasswordState::Empty,
        _ => PasswordState::Unknown,
    };
    let changed = fields
        .get(2)
        .copied()
        .and_then(parse_day)
        .filter(|&day| day > 1)
        .map_or(PasswordChanged::Unknown, |days_since_epoch| {
            PasswordChanged::Day { days_since_epoch }
        });
    PasswordStatus { state, changed }
}

/// `YYYY-MM-DD` or `MM/DD/YYYY` as days since 1970-01-01.
pub(crate) fn parse_day(text: &str) -> Option<i64> {
    let num = |part: &str| part.parse::<i64>().ok();
    let iso: Vec<&str> = text.split('-').collect();
    let us: Vec<&str> = text.split('/').collect();
    let (y, m, d) = if let [y, m, d] = iso[..] {
        (num(y)?, num(m)?, num(d)?)
    } else if let [m, d, y] = us[..] {
        (num(y)?, num(m)?, num(d)?)
    } else {
        return None;
    };
    days_from_civil(y, m, d)
}

/// Days since 1970-01-01 of a proleptic Gregorian date (Howard Hinnant's `days_from_civil`).
/// A day past its month's end is not refused; `passwd` prints none.
fn days_from_civil(y: i64, m: i64, d: i64) -> Option<i64> {
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    let y = if m <= 2 { y - 1 } else { y };
    let era = (if y >= 0 { y } else { y - 399 }) / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some(era * 146_097 + doe - 719_468)
}

/// `dscl . -read /Users/<me> AuthenticationAuthority`: "No such key" is no password record.
pub(crate) fn parse_dscl_authority(out: &str) -> PasswordState {
    if out
        .lines()
        .any(|line| line.trim() == "No such key: AuthenticationAuthority")
    {
        PasswordState::Empty
    } else {
        PasswordState::Unknown
    }
}

/// `accountPolicyData`'s `passwordLastSetTime` (a plist `<real>` or `<integer>`), in whole
/// seconds.
pub(crate) fn parse_dscl_policy(out: &str) -> PasswordChanged {
    let compact: String = out.split_whitespace().collect();
    let Some(after) = compact.split("<key>passwordLastSetTime</key>").nth(1) else {
        return PasswordChanged::Unknown;
    };
    ["<real>", "<integer>"]
        .iter()
        .find_map(|open| after.strip_prefix(open))
        .and_then(|rest| rest.split('<').next())
        .and_then(|value| value.parse::<f64>().ok())
        .filter(|seconds| seconds.is_finite() && *seconds > 0.0)
        .map_or(PasswordChanged::Unknown, |seconds| PasswordChanged::At {
            unix_seconds: seconds as i64,
        })
}

/// `program args` with a cleared environment and closed input; its stdout then stderr, or
/// `None` when it cannot start or runs past [`READ_LIMIT`] (then it is killed).
fn run_bounded(program: &Path, args: &[&str]) -> Option<String> {
    let mut child = Command::new(program)
        .args(args)
        .env_clear()
        .env("LC_ALL", "C")
        .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .ok()?;
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if started.elapsed() < READ_LIMIT => {
                std::thread::sleep(Duration::from_millis(20));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
    let output = child.wait_with_output().ok()?;
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    Some(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use PasswordState::{Empty, Locked, Unknown, Usable};

    /// 2026-10-15, the probe's day.
    const PROBE_DAY: i64 = 20_741;

    #[test]
    fn civil_days_match_known_dates() {
        for (y, m, d, days) in [
            (1970, 1, 1, 0),
            (1970, 1, 2, 1),
            (1969, 12, 31, -1),
            (2000, 1, 1, 10_957),
            (2024, 2, 29, 19_782),
            (2026, 2, 1, 20_485),
            (2026, 10, 15, PROBE_DAY),
        ] {
            assert_eq!(days_from_civil(y, m, d), Some(days), "{y}-{m}-{d}");
        }
        assert_eq!(days_from_civil(2026, 13, 1), None);
        assert_eq!(days_from_civil(2026, 0, 1), None);
        assert_eq!(days_from_civil(2026, 1, 0), None);
    }

    #[test]
    fn passwd_status_reads_every_state() {
        for (line, state) in [
            ("walker P 2026-10-15 0 99999 7 -1", Usable),
            (
                "walker PS 2026-10-15 0 99999 7 -1 (Password set, SHA512 crypt.)",
                Usable,
            ),
            ("walker L 2026-10-15 0 99999 7 -1", Locked),
            (
                "walker LK 2026-10-15 0 99999 7 -1 (Password locked.)",
                Locked,
            ),
            ("walker NP 2026-10-15 0 99999 7 -1", Empty),
            ("walker ?? 2026-10-15", Unknown),
        ] {
            let status = parse_passwd_status("walker", line);
            assert_eq!(status.state, state, "{line}");
            let day = PasswordChanged::Day {
                days_since_epoch: PROBE_DAY,
            };
            assert_eq!(status.changed, day, "{line}");
        }
    }

    #[test]
    fn both_date_forms_read_alike() {
        let iso = parse_passwd_status("walker", "walker P 2026-10-15 0 99999 7 -1");
        let us = parse_passwd_status("walker", "walker P 10/15/2026 0 99999 7 -1");
        assert_eq!(iso, us);
        let february = parse_passwd_status("walker", "walker P 02/01/2026 0 99999 7 -1");
        assert_eq!(
            february.changed,
            PasswordChanged::Day {
                days_since_epoch: 20_485
            }
        );
    }

    #[test]
    fn a_change_date_on_or_before_1970_01_02_is_unknown() {
        for date in ["1970-01-02", "1970-01-01", "01/01/1970", "never", ""] {
            let status = parse_passwd_status("walker", &format!("walker P {date} -1 -1 -1 -1"));
            let unknown = PasswordStatus {
                state: Usable,
                changed: PasswordChanged::Unknown,
            };
            assert_eq!(status, unknown, "{date:?}");
        }
        let third = parse_passwd_status("walker", "walker P 1970-01-03 0 99999 7 -1");
        assert_eq!(
            third.changed,
            PasswordChanged::Day {
                days_since_epoch: 2
            }
        );
    }

    #[test]
    fn a_line_for_another_account_is_not_read() {
        let out = "root P 2026-10-15 0 99999 7 -1\nwalker L 2026-10-14 0 99999 7 -1\n";
        assert_eq!(parse_passwd_status("walker", out).state, Locked);
        assert_eq!(parse_passwd_status("walk", out), PasswordStatus::UNKNOWN);
        let refused = "passwd: You may not view or modify password information for root.";
        assert_eq!(
            parse_passwd_status("walker", refused),
            PasswordStatus::UNKNOWN
        );
    }

    /// WI-545: shadow's `passwd` that cannot read /etc/shadow — no_new_privs, its set-uid bit
    /// dropped, a nosuid mount, NixOS's profile `passwd` — prints `<name> P` (the `x`
    /// placeholder), whatever the password is: unknown, never usable, so `lock on` offers the
    /// empty entry. The full lines of Debian, Fedora and NixOS read as before; the change-day
    /// field alone is enough.
    #[test]
    fn a_line_without_the_change_day_field_is_unknown() {
        for out in [
            "walker P\n",
            "walker P",
            "walker NP\n",
            "walker L\n",
            "walker PS\n",
        ] {
            let status = parse_passwd_status("walker", out);
            assert_eq!(status, PasswordStatus::UNKNOWN, "{out:?}");
        }
        let day = |days_since_epoch| PasswordChanged::Day { days_since_epoch };
        for (out, state, changed) in [
            // Debian 13 and Fedora 42 through their set-uid passwd (the e1a walk's W3xdiag).
            (
                "walker P 2026-10-16 0 99999 7 -1\n",
                Usable,
                day(PROBE_DAY + 1),
            ),
            (
                "walker NP 2026-10-16 0 99999 7 -1\n",
                Empty,
                day(PROBE_DAY + 1),
            ),
            (
                "walker L 2026-10-16 0 99999 7 -1\n",
                Locked,
                day(PROBE_DAY + 1),
            ),
            // NixOS's set-uid wrapper, /run/wrappers/bin/passwd (the walk's VM).
            (
                "walker P 2026-10-20 -1 -1 -1 -1\n",
                Usable,
                day(PROBE_DAY + 5),
            ),
            // NixOS, a password set in its configuration: day 1 (GOTCHA-328).
            (
                "walker P 1970-01-02 -1 -1 -1 -1\n",
                Usable,
                PasswordChanged::Unknown,
            ),
            // The rule asks for the change-day field, not for all seven.
            ("walker P 2026-10-15\n", Usable, day(PROBE_DAY)),
        ] {
            let status = parse_passwd_status("walker", out);
            assert_eq!(status, PasswordStatus { state, changed }, "{out:?}");
        }
    }

    #[test]
    fn dscl_without_an_authentication_authority_is_no_password() {
        assert_eq!(
            parse_dscl_authority("No such key: AuthenticationAuthority\n"),
            Empty
        );
        let set = "AuthenticationAuthority: ;ShadowHash;HASHLIST:<SALTED-SHA512-PBKDF2> ;Kerberosv5;;walker@LKDC:SHA1.B7;\n";
        assert_eq!(parse_dscl_authority(set), Unknown);
        assert_eq!(parse_dscl_authority(""), Unknown);
    }

    #[test]
    fn the_password_change_time_is_read_from_its_key() {
        let out = "dsAttrTypeNative:accountPolicyData:\n <?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<plist version=\"1.0\">\n<dict>\n\t<key>creationTime</key>\n\t<real>1792091542.7738051</real>\n\t<key>failedLoginCount</key>\n\t<integer>1</integer>\n\t<key>passwordLastSetTime</key>\n\t<real>1792091543.5475512</real>\n</dict>\n</plist>\n";
        let at = PasswordChanged::At {
            unix_seconds: 1_792_091_543,
        };
        assert_eq!(parse_dscl_policy(out), at);
        let integer = out.replace(
            "<real>1792091543.5475512</real>",
            "<integer>1792091543</integer>",
        );
        assert_eq!(parse_dscl_policy(&integer), at);
        let other = out.replace("passwordLastSetTime", "somethingElse");
        assert_eq!(parse_dscl_policy(&other), PasswordChanged::Unknown);
        assert_eq!(
            parse_dscl_policy("No such key: accountPolicyData"),
            PasswordChanged::Unknown
        );
    }

    #[cfg(unix)]
    #[test]
    fn this_account_is_the_running_uid() {
        let account = current_account().expect("the test runner has a passwd entry");
        // SAFETY: getuid cannot fail and touches no memory.
        assert_eq!(account.uid, unsafe { libc::getuid() });
        assert!(!account.name.is_empty());
        assert!(format!("{account:?}").contains(&account.name_lossy()));
    }

    #[cfg(unix)]
    #[test]
    fn a_reader_that_runs_too_long_or_is_missing_says_nothing() {
        let started = Instant::now();
        // A shell busy forever (/bin/sh is on every Unix, NixOS included; `sleep` is not on
        // NixOS's /bin).
        assert_eq!(
            run_bounded(Path::new("/bin/sh"), &["-c", "while :; do :; done"]),
            None
        );
        assert!(
            started.elapsed() < READ_LIMIT + Duration::from_secs(2),
            "killed at the limit"
        );
        assert_eq!(run_bounded(Path::new("/nonexistent/passwd"), &["-S"]), None);
    }
}
