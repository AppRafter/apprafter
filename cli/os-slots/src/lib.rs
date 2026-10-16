// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The OS side of the target-store lock's everyday slots (D.4b; ADR 0069 §3, §9).
//!
//! - The account-password check, `AccountPassword`: the account password checked with the
//!   system once, when its slot is added (Linux: PAM, `libpam.so.0` loaded at run time; macOS:
//!   the PAM service `chkpasswd`, GOTCHA-327), under a back-off across runs; and what this
//!   account's password status reads without privileges.
//! - [`HostFacts`]: this computer's machine id and name, and whether it looks like a container
//!   or WSL ([`machine`]).
//! - A real account-password backend is made only by `fence::admit` (B16, B82): given
//!   `APPRAFTER_SANDBOX`'s roots, it admits only an account whose passwd home lies strictly
//!   inside one and holds the test-account marker. The caller passes the roots; this crate
//!   reads no environment variable.
//! - `stand_in::ScriptedAccountPassword` (feature `test-support`) stands in for both in tests.
//!
//! Nothing here depends on AppRafter's other crates, and `apprafter-core` never names this one
//! (ADR 0067 §2): `platform-cli` and the desktop map its answers to the core's types.

pub mod account;
pub mod backoff;
mod check;
pub mod fence;
pub mod machine;
#[cfg(target_os = "macos")]
mod macos;
pub mod outcome;
mod pam;

use std::time::SystemTime;

use zeroize::Zeroizing;

pub use machine::{Environment, MachineId, NoMachineId, SystemHost};
pub use outcome::{
    BackoffState, BackoffWait, CheckOutcome, InputRefused, NotCheckedWhy, Os, PasswordChanged,
    PasswordCheck, PasswordState, PasswordStatus, Unavailable, WaitWhy,
};

/// This computer's facts a slot is bound to or labelled with (D.4b B54, B76, B77).
pub trait HostFacts: Send + Sync {
    /// Linux: the 32 characters of `/etc/machine-id`; macOS: `gethostuuid`'s 16 bytes.
    fn machine_id(&self) -> Result<MachineId, NoMachineId>;
    /// Whether this computer looks like a container or WSL (B76).
    fn environment(&self) -> Environment;
    /// This computer's name (`gethostname`), for a slot's default label (B77).
    fn host_name(&self) -> Option<String>;
}

/// The account-password check and the facts a plan reads before it (D.4b B55, B74, B75, B88).
/// Each fact is its own call, so the caller orders the fallback reasons (B88). The real one is
/// `fence::SystemAccountPassword`, made only by `fence::admit`.
pub trait AccountPassword: Send + Sync {
    /// The OS this backend checks on (a stand-in plays one).
    fn os(&self) -> Os;
    /// The account a check is for (`getpwuid_r(getuid())`, never a name from input), for labels.
    fn account_name(&self) -> Option<String>;
    /// Whether a check could run here: the account, libpam and its service. Asks PAM nothing.
    fn backend(&self) -> Result<(), Unavailable>;
    /// The back-off as it stands; `Unavailable::NoBackoffRecord` when its record has no place.
    /// Takes no lock and writes nothing (a plan's read, B74). The real backend reads the system
    /// clock whatever `now` says, so no caller moves the back-off; a stand-in answers relative to
    /// `now` (critic round).
    fn backoff(&self, now: SystemTime) -> Result<BackoffState, Unavailable>;
    /// What this account's password status reads, without privileges (B75).
    fn password_status(&self) -> PasswordStatus;
    /// One check of `password` (B10–B12): refused before PAM when it is empty or holds a NUL, or
    /// while a back-off is in force; counted toward the back-off unless PAM reported success.
    /// `now` as in [`AccountPassword::backoff`].
    fn check(&self, password: &Zeroizing<String>, now: SystemTime) -> PasswordCheck;
}
