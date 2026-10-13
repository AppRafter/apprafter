// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Where a test interrupts a lock flow (D.4a §3.9; critic G12): each step boundary of `lock on`
//! and `lock off` passes a [`FaultPoint`], and a context armed with that point fails there as if
//! the process had died. Only `Context::with_fault` (`test-support`) arms one: no environment
//! variable and no argv flag does, in any build, and guard L6 keeps `platform-cli` and the
//! desktop from naming it.

use crate::context::Context;
use crate::error::CoreResult;

/// A step boundary of a lock flow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FaultPoint {
    /// `lock on`: `store.yaml` says `locking`.
    AfterStoreFileLocking,
    /// `lock on`: the vault is on disk.
    AfterVaultWritten,
    /// `lock on`: the vault was read back and opened with both slots.
    AfterVaultVerified,
    /// `lock on`: target `n`'s caches are re-sealed.
    AfterCachesTarget(usize),
    /// `lock on`: target `n`'s credentials file is a marker.
    AfterMarkerTarget(usize),
    /// `lock on`: `store.yaml` says `locked`.
    AfterLocked,
    /// `lock off`: `store.yaml` says `unlocking`.
    AfterStoreFileUnlocking,
    /// `lock off`: the shared age key is there.
    AfterSharedKey,
    /// `lock off`: target `n`'s token and caches are in files.
    AfterFilesTarget(usize),
    /// `lock off`: every file was read back.
    AfterFilesVerified,
    /// `lock off`: the vault is deleted.
    AfterVaultDeleted,
    /// `lock off`: `store.yaml` says `files`.
    AfterFiles,
}

/// An injected failure when `ctx` is armed with `at`; nothing in a build without `test-support`.
pub(crate) fn hit(ctx: &Context, at: FaultPoint) -> CoreResult<()> {
    #[cfg(any(test, feature = "test-support"))]
    if ctx.fault() == Some(at) {
        return Err(crate::error::CoreError::Cli(cli_core::CliError::Other(
            format!("injected fault at {at:?}"),
        )));
    }
    #[cfg(not(any(test, feature = "test-support")))]
    let _ = (ctx, at);
    Ok(())
}
