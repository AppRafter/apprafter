// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The only codes the desktop itself raises, as `UiError.code`; core codes pass through.

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
];

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    #[test]
    fn every_code_is_listed_once_under_the_desktop_prefix() {
        assert_eq!(ALL.len(), 10);
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
        ] {
            assert!(ALL.contains(&code), "{code} is not in ALL");
        }
    }
}
