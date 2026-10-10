// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The only codes the desktop itself raises, as `UiError.code`; core codes pass through.
//!
//! An authentication error from the app's own password field (`unlock_with_password`,
//! `op_execute` with a password) may carry `fields.messages`: what the OS said while it checked
//! the password — PAM's "Password expired", say — as a list of strings, never the password.

/// A command other than [`ALLOWED_WHILE_LOCKED`](crate::ALLOWED_WHILE_LOCKED) while locked.
pub const LOCKED: &str = "apprafter::desktop::locked";
/// No pending plan has this op id (`fields.opId`): executed already, discarded, or dropped on
/// lock.
pub const PLAN_NOT_FOUND: &str = "apprafter::desktop::plan_not_found";
/// The plan (`fields.opId`) outlived its time to live; plan again.
pub const PLAN_EXPIRED: &str = "apprafter::desktop::plan_expired";
/// The OS prompt was closed before it answered.
pub const AUTH_CANCELLED: &str = "apprafter::desktop::auth_cancelled";
/// The OS did not verify the device owner.
pub const AUTH_FAILED: &str = "apprafter::desktop::auth_failed";
/// The OS cannot authenticate here; `fields.reason` says why.
pub const AUTH_UNAVAILABLE: &str = "apprafter::desktop::auth_unavailable";
/// Another authentication prompt was already open; nothing was asked, and the plan waits.
pub const AUTH_BUSY: &str = "apprafter::desktop::auth_busy";
/// `settings.json` could not be written.
pub const SETTINGS_IO: &str = "apprafter::desktop::settings_io";
/// A bug in the desktop, e.g. a panicked operation.
pub const INTERNAL: &str = "apprafter::desktop::internal";
/// The app is quitting: nothing new starts, and a plan that would have is dropped.
pub const CLOSING: &str = "apprafter::desktop::closing";
/// No token draft has this id (`fields.draftId`): used by its plan, discarded, or dropped on lock.
pub const DRAFT_NOT_FOUND: &str = "apprafter::desktop::draft_not_found";
/// The draft (`fields.draftId`) outlived its ten minutes; verify the token again.
pub const DRAFT_EXPIRED: &str = "apprafter::desktop::draft_expired";
/// A typed path (`fields.path`, as typed) is not a full path: it would resolve against the
/// app's working directory, which means nothing to whoever typed it. A `~/` path is a full one.
pub const RELATIVE_PATH: &str = "apprafter::desktop::relative_path";

/// Every code above, once.
pub const ALL: &[&str] = &[
    LOCKED,
    PLAN_NOT_FOUND,
    PLAN_EXPIRED,
    AUTH_CANCELLED,
    AUTH_FAILED,
    AUTH_UNAVAILABLE,
    AUTH_BUSY,
    SETTINGS_IO,
    INTERNAL,
    CLOSING,
    DRAFT_NOT_FOUND,
    DRAFT_EXPIRED,
    RELATIVE_PATH,
];

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    #[test]
    fn every_code_is_listed_once_under_the_desktop_prefix() {
        assert_eq!(ALL.len(), 13);
        let unique: BTreeSet<_> = ALL.iter().collect();
        assert_eq!(unique.len(), ALL.len(), "{ALL:?}");
        for code in ALL {
            assert!(code.starts_with("apprafter::desktop::"), "{code}");
        }
        for code in [
            LOCKED,
            PLAN_NOT_FOUND,
            PLAN_EXPIRED,
            AUTH_CANCELLED,
            AUTH_FAILED,
            AUTH_UNAVAILABLE,
            AUTH_BUSY,
            SETTINGS_IO,
            INTERNAL,
            CLOSING,
            DRAFT_NOT_FOUND,
            DRAFT_EXPIRED,
            RELATIVE_PATH,
        ] {
            assert!(ALL.contains(&code), "{code} is not in ALL");
        }
    }
}
