// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The types AppRafter Desktop sends between Rust and the webview (ADR 0067 §3).
//!
//! Tauri-free on purpose: the TypeScript bindings are exported from this crate's tests,
//! which then run on any OS without WebKitGTK. The wire shape of every type here is the
//! contract with the frontend, and each module's tests pin it.

mod app_info;
mod auth;
mod lock;
mod ops;
mod quit;
mod settings;

/// The command names. Plain Rust with no imports: `src-tauri/build.rs` `include!`s it.
pub mod commands;
pub mod errors;

pub use app_info::{AppInfo, Os, SecretBackend};
pub use auth::{AuthInfo, AuthMethod, AuthOutcome, CancelledBy, UnavailableReason};
pub use commands::{ALLOWED_WHILE_LOCKED, COMMANDS};
pub use lock::{LockReason, LockState, LOCK_CHANGED};
pub use ops::{
    OpEvent, OpId, OpState, OpSummary, OutputStream, PlanView, Subscribed, SubscriptionId,
};
pub use quit::{Quitting, QUITTING};
pub use settings::{AutoLock, Refresh, Settings, Theme};

/// What every failed command returns: the core's serialisable error projection.
pub use apprafter_core::UiError;

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::{ALLOWED_WHILE_LOCKED, COMMANDS};

    #[test]
    fn command_names_are_unique() {
        let unique: BTreeSet<_> = COMMANDS.iter().collect();
        assert_eq!(unique.len(), COMMANDS.len(), "{COMMANDS:?}");
    }

    #[test]
    fn what_a_locked_app_answers_is_a_unique_subset_of_the_commands() {
        let unique: BTreeSet<_> = ALLOWED_WHILE_LOCKED.iter().collect();
        assert_eq!(unique.len(), ALLOWED_WHILE_LOCKED.len());
        for name in ALLOWED_WHILE_LOCKED {
            assert!(COMMANDS.contains(name), "{name} is not a command");
        }
    }

    /// What src-tauri/build.rs does with the file. At module level: inside a function body
    /// `include!` parses an expression and rejects the `pub const` items.
    mod included_as_build_rs_does {
        include!("commands.rs");
    }

    #[test]
    fn commands_rs_compiles_when_included_elsewhere() {
        assert_eq!(included_as_build_rs_does::COMMANDS, COMMANDS);
        assert_eq!(
            included_as_build_rs_does::ALLOWED_WHILE_LOCKED,
            ALLOWED_WHILE_LOCKED
        );
    }
}
