// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Future-version policy hooks. PlatformController calls these
//! during reconcile so 1.74 (MigrationPlan) and 1.74a (yanking)
//! can drop in concrete implementations without touching the
//! reconcile loop.
//!
//! In 1.73 we ship a `NoOpHooks` default impl: every version is
//! considered non-yanked; MigrationPlan creation is a no-op
//! returning success (the breaking-diff condition is still
//! pushed onto the PlatformStack status from
//! `reconcile::reconcile`).

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
    /// Should the reconciler skip `version` when picking the
    /// channel-latest? Concrete impl in 1.74a reads
    /// `compatibility.yaml#<version>.yanked` (the yanked field
    /// does not yet exist in 1.73 — schema bump in 1.74a).
    async fn is_yanked(&self, upstream_url: &str, version: &str) -> Result<bool, PolicyError>;

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
    async fn is_yanked(&self, _: &str, _: &str) -> Result<bool, PolicyError> {
        Ok(false)
    }

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
    async fn no_op_hooks_report_not_yanked() {
        let hooks = NoOpHooks;
        assert!(!hooks.is_yanked("oci://example", "0.1.0").await.unwrap());
    }

    #[tokio::test]
    async fn no_op_hooks_succeed_on_migration_plan_request() {
        let hooks = NoOpHooks;
        hooks
            .request_migration_plan("0.1.0", "0.2.0", ChangeClass::Breaking)
            .await
            .unwrap();
    }
}
