// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! What the owner is asked to authenticate for, on the OSes whose backend has no action
//! registry of its own. On Linux the same two actions are polkit's
//! ([`crate::linux::polkit::Action`], which also knows their policy ids), and the crate root
//! re-exports whichever this OS has, so the shell names one `apprafter_os_auth::Action`
//! everywhere.

/// Why the OS is asked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Unlock the app.
    Unlock,
    /// Approve or run one destructive operation.
    Confirm,
}
