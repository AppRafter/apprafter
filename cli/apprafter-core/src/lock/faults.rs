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
    /// `lock off`: step 2 is done (the shared age key is there when a cache needs one).
    AfterSharedKey,
    /// `lock off`: target `n`'s token and caches are in files.
    AfterFilesTarget(usize),
    /// `lock off`: every file was read back.
    AfterFilesVerified,
    /// `lock off`: the vault is deleted.
    AfterVaultDeleted,
    /// `lock off`: `store.yaml` says `files`.
    AfterFiles,
    /// `lock slot add` / `remove` (D.4b c1): the new vault is sealed, not yet written.
    BeforeSlotWrite,
    /// `lock slot add` / `remove`: the new vault is on disk, not yet read back nor mirrored.
    AfterSlotWrite,
    /// A key change (D.4b B63): `store.yaml` says `rotating` (R1).
    AfterStoreFileRotating,
    /// A key change: the rotated vault is on disk (R2), not yet read back.
    AfterVaultRotated,
    /// A key change: the rotated vault was read back and opened with every slot it holds.
    AfterRotatedVerified,
    /// A key change: state folder `n`'s caches are re-sealed (R3).
    AfterCachesRekeyed(usize),
    /// A key change: the retiring store key is out of the vault (R4).
    AfterRetiringDropped,
    /// A key change: this computer's Windows Hello keys of dropped slots were handed over
    /// (R4b).
    AfterHelloKeysHandedOver,
    /// A key change: `store.yaml` says `locked` (R5).
    AfterRotated,
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
