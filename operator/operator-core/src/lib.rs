// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Shared types and helpers for the AppRafter operator.

pub mod application;
pub mod leader;
pub mod metrics;
pub mod migration;
pub mod migration_plan;
pub mod platform_stack;

pub use application::{
    Application, ApplicationBaseSpec, ApplicationCondition, ApplicationExpose, ApplicationSpec,
    ApplicationStatus,
};
pub use leader::{LeaderConfig, LeaderElection, LeaderError};
pub use metrics::Metrics;
pub use migration::{MigrationError, MigrationStrategy, StepOutcome};
pub use migration_plan::{
    ExecutedStep, MigrationApplicationRef, MigrationApplicationScope, MigrationPlan,
    MigrationPlanScope, MigrationPlanSpec, MigrationPlanStatus, MigrationPlatformScope,
    MigrationRisks, MigrationStep, MigrationTrigger,
};
pub use platform_stack::{
    PlatformStack, PlatformStackComponent, PlatformStackComponentOverride, PlatformStackCondition,
    PlatformStackSource, PlatformStackSpec, PlatformStackStatus, PlatformStackValues,
    PlatformStackVersionHistoryEntry,
};
