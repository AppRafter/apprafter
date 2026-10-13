// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The holder process (`apprafter __holder`), per OS.

#[cfg(unix)]
mod unix;

/// The holder's exit code.
pub(super) fn run() -> i32 {
    unix::run()
}
