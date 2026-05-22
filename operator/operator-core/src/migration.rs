// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Shared `MigrationStrategy` trait + supporting types for the
//! MigrationController (B.1.76).
//!
//! Two strategies live in the
//! `operator-controllers/migration` crate: `ApplicationMigrationStrategy`
//! and `PlatformMigrationStrategy`. They share the
//! per-plan-step execution + reject paths through this trait
//! so MigrationController can dispatch on `scope.type` at
//! runtime via `Box<dyn MigrationStrategy>`.
//!
//! Per-scope detection (`detect_destructive`) is NOT in the
//! trait — Application detection takes an `ApplicationSpec`
//! diff, platform detection takes version strings +
//! compatibility metadata. Forcing both through a single
//! `&dyn Context` would either erase the type information
//! callers need or introduce an associated type that breaks
//! trait-object dispatch. Detection therefore lives as
//! concrete fns on each strategy struct; B.1.77 and B.1.78
//! wire them in at the call sites.

use async_trait::async_trait;
use thiserror::Error;

use crate::{MigrationPlan, MigrationStep};

/// Outcome of running a single `MigrationStep`. The
/// MigrationController appends these to `status.executedSteps`
/// after each call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StepOutcome {
    Succeeded,
    Failed { message: String },
    Skipped { reason: String },
}

#[derive(Debug, Error)]
pub enum MigrationError {
    #[error("kube API error: {0}")]
    Kube(#[from] kube::Error),
    #[error(
        "MigrationPlan {0:?} has no spec.previousSpecSnapshot — \
             reject cannot revert without a snapshot"
    )]
    NoSnapshot(String),
    #[error("malformed previousSpecSnapshot in MigrationPlan {0:?}: {1}")]
    SnapshotShape(String, String),
    #[error("strategy backend error: {0}")]
    Backend(String),
}

/// Per-scope behaviour shared between application + platform
/// strategies. Trait-object friendly (no associated types, no
/// generics).
///
/// `execute_step` runs ONE step from `plan.spec.plan[]`. Steps
/// are free-form text in 1.75 / 1.76 — no machine semantics —
/// so the default strategy impl just marks them `Succeeded`.
/// B.1.77 and beyond can replace the impls with real action
/// runners.
///
/// `reject` is invoked when MigrationController observes
/// `status.phase = rejected`. Application-scope MUST be a
/// no-op per ADR 0027 (the user reverts the Git commit
/// instead); platform-scope reverts `PlatformStack.spec.pin`
/// from `plan.spec.previousSpecSnapshot`.
#[async_trait]
pub trait MigrationStrategy: Send + Sync {
    async fn execute_step(
        &self,
        plan: &MigrationPlan,
        step: &MigrationStep,
    ) -> Result<StepOutcome, MigrationError>;

    async fn reject(&self, plan: &MigrationPlan) -> Result<(), MigrationError>;
}
