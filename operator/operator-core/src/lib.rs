// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Shared types and helpers for the AppRafter operator.

pub mod application;
pub mod leader;
pub mod metrics;
pub mod migration;
pub mod migration_plan;
pub mod platform_stack;
pub mod resourceclaim;
pub mod serviceprovider;
pub mod sourcecredential;

pub use application::{
    Application, ApplicationBaseSpec, ApplicationCondition, ApplicationExpose, ApplicationSpec,
    ApplicationStatus, COND_MIGRATION_PENDING, PHASE_AWAITING_MIGRATION_APPROVAL,
};
pub use leader::{LeaderConfig, LeaderElection, LeaderError};
pub use metrics::Metrics;
pub use migration::{DestructiveChange, MigrationError, MigrationStrategy, StepOutcome};
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
pub use resourceclaim::{
    ResourceClaim, ResourceClaimCondition, ResourceClaimSpec, ResourceClaimStatus,
};
pub use serviceprovider::{ServiceProvider, ServiceProviderSpec, ServiceProviderStatus};
pub use sourcecredential::{
    SealedSecretRef, SourceBackend, SourceCredential, SourceCredentialCondition,
    SourceCredentialSpec, SourceCredentialStatus, SourceGit, SourceRegistry, COND_GIT_PRESENT,
    COND_GIT_VALID, COND_REGISTRY_PRESENT, COND_REGISTRY_VALID, REASON_AUTH_REJECTED,
    REASON_REACHABLE, REASON_UNVERIFIED,
};
