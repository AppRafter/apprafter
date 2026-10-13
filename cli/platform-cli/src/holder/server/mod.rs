// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The holder process (`apprafter __holder`), per OS.

#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;

/// The holder's exit code.
pub(super) fn run() -> i32 {
    #[cfg(unix)]
    return unix::run();
    #[cfg(windows)]
    return windows::run();
}
