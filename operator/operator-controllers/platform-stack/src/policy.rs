// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Future-version policy hooks. PlatformController calls these
//! during reconcile so 1.74 (MigrationPlan auto-create) can
//! drop in a concrete implementation without touching the
//! reconcile loop.
//!
//! Yank handling (B.1.74a) is NOT a hook — the reconcile loop
//! pulls `compatibility.yaml` once per OCI poll cycle and
//! consumes it inline for two purposes: filtering yanked
//! versions out of channel-latest resolution
//! (`resolve_non_yanked_latest`) and surfacing the
//! `YankedVersion` condition. Adding a hook on top would
//! either duplicate work (N tarball pulls per resolve) or
//! force the hook to take a `&CompatibilityDoc` argument that
//! is a thin wrapper over the standard `BTreeMap` lookup —
//! neither shape adds value.
//!
//! `NoOpHooks` remains as the default test fixture: it
//! returns success on every MigrationPlan request without
//! creating any resource. 1.74 lands the real impl.

use async_trait::async_trait;
use thiserror::Error;

use crate::compatibility::ChangeClass;

#[derive(Debug, Error)]
pub enum PolicyError {
    #[error("policy hook backend error: {0}")]
    Backend(String),
}

/// Hooks the reconciler calls into.
#[async_trait]
pub trait PolicyHooks: Send + Sync {
    /// Request a MigrationPlan be created for the breaking
    /// transition from `from_version` to `to_version`. 1.73's
    /// stub returns Ok(()) without creating any resource — the
    /// `MigrationPending=True` condition pushed onto the
    /// PlatformStack status is the only signal in 1.73.
    /// Concrete impl in 1.74 creates the MigrationPlan CR.
    async fn request_migration_plan(
        &self,
        from_version: &str,
        to_version: &str,
        change_class: ChangeClass,
    ) -> Result<(), PolicyError>;
}

#[derive(Debug, Default, Clone, Copy)]
pub struct NoOpHooks;

#[async_trait]
impl PolicyHooks for NoOpHooks {
    async fn request_migration_plan(
        &self,
        _: &str,
        _: &str,
        _: ChangeClass,
    ) -> Result<(), PolicyError> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn no_op_hooks_succeed_on_migration_plan_request() {
        let hooks = NoOpHooks;
        hooks
            .request_migration_plan("0.1.0", "0.2.0", ChangeClass::Breaking)
            .await
            .unwrap();
    }
}
