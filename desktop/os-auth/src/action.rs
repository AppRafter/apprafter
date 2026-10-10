// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! What the owner is asked to authenticate for: one `Action` on every OS, so the shell names one
//! `apprafter_os_auth::Action` everywhere. On Linux the polkit backend also gives each action its
//! policy id (`crate::linux::polkit`).

/// Why the OS is asked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Unlock the app.
    Unlock,
    /// Approve or run one destructive operation.
    Confirm,
}
