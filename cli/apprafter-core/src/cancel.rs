// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Per-operation cancellation (ADR 0067 §2). Completed in Task B7.

/// Returned by [`CancellationToken::check`] once the token has tripped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cancelled;

impl std::fmt::Display for Cancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("operation cancelled")
    }
}

impl std::error::Error for Cancelled {}
