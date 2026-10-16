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

pub mod machine;
#[cfg(target_os = "macos")]
mod macos;
pub mod outcome;

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
