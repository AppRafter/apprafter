// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! What a check and the facts around it answer (D.4b B11, B12, B74, B75, B88). The CLI and the
//! desktop map them to the core's refusals and fallbacks; nothing here is text for a person.

use std::path::PathBuf;
use std::time::SystemTime;

/// The operating system a backend checks on. A stand-in plays one (B15): the caller reads this,
/// never `cfg!`, so every OS's flow runs on every CI leg.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Os {
    Linux,
    Macos,
    Windows,
}

impl Os {
    /// The OS this build runs on.
    pub const fn this() -> Os {
        if cfg!(target_os = "macos") {
            Os::Macos
        } else if cfg!(windows) {
            Os::Windows
        } else {
            Os::Linux
        }
    }
}

/// Why no check can run here: a fact, read without asking PAM anything.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unavailable {
    /// libpam does not load; `tried` is every path tried, in order.
    NoLibpam { tried: Vec<String> },
    /// Linux: none of the services a check may use exists (`common-auth`, `system-auth`,
    /// `login`).
    NoService,
    /// macOS: `/etc/pam.d/chkpasswd`, the one service that checks the typed text, is missing.
    NoCheckRoute,
    /// The running uid has no passwd entry with a name.
    NoAccount,
    /// Linux: this process runs with no_new_privs (a sandbox such as Flatpak or bubblewrap, a
    /// service started with `NoNewPrivileges=`): `pam_unix`'s set-uid helper cannot read the
    /// password database, so the stack would refuse even the right password and may count it
    /// (WI-545; `privileges`).
    NoNewPrivs,
    /// The back-off record has no place that is this account's alone: Linux `/run/user/<uid>`
    /// missing — no login session: a container, WSL 1, `su`, `sudo`, ssh without logind — or a
    /// directory another account owns or others can write.
    NoBackoffRecord { dir: PathBuf },
    /// This OS has no account-password slot (Windows; ADR 0068 §3).
    NotOnThisOs,
}

/// Why the next check must wait.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitWhy {
    /// A failed check less than ten minutes ago.
    RecentFailure,
    /// Two failed checks in the last hour.
    TwoInAnHour,
    /// Another AppRafter process is checking this account's password right now.
    AnotherCheckRunning,
}

/// A back-off in force: the earliest time the next check may run (`None` while another check
/// runs), and why.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BackoffWait {
    pub until: Option<SystemTime>,
    pub why: WaitWhy,
}

/// The back-off as it stands, read without changing it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackoffState {
    Clear,
    Wait(BackoffWait),
}

/// Why a check that reached PAM did not check the typed text (B11).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotCheckedWhy {
    /// The stack never asked a hidden prompt: a fingerprint module, `pam_permit`, a smartcard,
    /// or a module that refused before the password module asked.
    NoPasswordAsked,
    /// The stack asked something besides one hidden prompt — a second one (a one-time code), one
    /// that echoes, Linux-PAM's radio or binary kinds — and the conversation refused it.
    OtherPrompt,
    /// The stack succeeded for another account (a module changed `PAM_USER`).
    UserChanged,
}

/// An entry refused before PAM is asked; never counted toward the back-off.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputRefused {
    Empty,
    ContainsNul,
}

/// How one check ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckOutcome {
    /// PAM succeeded, its one hidden prompt was answered with the typed text, for this account.
    Verified,
    /// PAM refused the typed text: `PAM_AUTH_ERR`, `PAM_PERM_DENIED` (Debian's `pam_faillock`
    /// `[default=die]`, a disabled macOS account) or `PAM_MAXTRIES`.
    Wrong,
    NotChecked(NotCheckedWhy),
    /// PAM failed for another reason; `name` is its constant's name (`PAM_AUTHINFO_UNAVAIL`).
    SystemError {
        code: i32,
        name: &'static str,
    },
    Refused(InputRefused),
    Backoff(BackoffWait),
    Unavailable(Unavailable),
}

/// A check's outcome with what the stack said on the way: its info and error messages, in
/// order, at most 16, each cut at 512 characters (e.g. `pam_faillock`'s "The account is locked
/// due to 3 failed logins").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PasswordCheck {
    pub outcome: CheckOutcome,
    pub messages: Vec<String>,
}

/// What a process that is not root can read about its own account's password (B75).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PasswordState {
    /// A password is set and not locked.
    Usable,
    /// Locked (`passwd -l`): no password opens the account.
    Locked,
    /// No password, or an empty one.
    Empty,
    /// Not readable here (macOS tells an empty password from a set one only at a check; Linux's
    /// `passwd` says nothing clear without its privileges, WI-545).
    Unknown,
}

/// When the account's password last changed, as precisely as this OS tells it (B41).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PasswordChanged {
    /// Linux `passwd -S`: a day, as days since 1970-01-01.
    Day { days_since_epoch: i64 },
    /// macOS `passwordLastSetTime`: whole seconds since the epoch.
    At { unix_seconds: i64 },
    /// Not told — or told as 1970-01-02 or earlier: NixOS writes day 1 for a password set in its
    /// configuration, and day 0 means "change at next login" (GOTCHA-328).
    Unknown,
}

/// [`PasswordState`] and [`PasswordChanged`] together.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PasswordStatus {
    pub state: PasswordState,
    pub changed: PasswordChanged,
}

impl PasswordStatus {
    /// Nothing read.
    pub const UNKNOWN: PasswordStatus = PasswordStatus {
        state: PasswordState::Unknown,
        changed: PasswordChanged::Unknown,
    };
}
