// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Shared types and helpers for the AppRafter operator.

pub mod application;
pub mod leader;
pub mod matching;
pub mod metrics;
pub mod migration;
pub mod migration_plan;
pub mod migration_state;
pub mod platform_stack;
pub mod resourceclaim;
pub mod retainedclaim;
pub mod serviceprovider;
pub mod sharedvolume;
pub mod sourcecredential;

pub use application::{
    image_repo, Application, ApplicationBaseSpec, ApplicationCondition, ApplicationEnvOverride,
    ApplicationExpose, ApplicationSpec, ApplicationStatus, DiskClaim, EnvRef, EnvValue,
    ExposeOverride, ImagePolicy, NeedEntry, Needs, OneOrMany, ServiceNeed, StatusImage,
    COND_IMAGE_RESOLVED, COND_PUBLIC_ROUTE_READY, COND_RESOURCE_CLAIM_PENDING,
    PHASE_AWAITING_RESOURCE_CLAIM, PHASE_ENV_SECRET_MISSING,
};
pub use leader::{LeaderConfig, LeaderElection, LeaderError};
pub use matching::{matches, select_provider, Candidate};
pub use metrics::Metrics;
pub use migration::{DestructiveChange, MigrationError, MigrationStrategy, StepOutcome};
pub use migration_plan::{
    ExecutedStep, MigrationApplicationRef, MigrationApplicationScope, MigrationChange,
    MigrationPlan, MigrationPlanScope, MigrationPlanSpec, MigrationPlanStatus,
    MigrationPlatformScope, MigrationRisks, MigrationSourceCredentialRef,
    MigrationSourceCredentialScope, MigrationStep, MigrationTrigger,
};
pub use migration_state::{
    decide, plan_state, plan_state_no_change, MigrationDecision, PlanState, COND_MIGRATION_PENDING,
    PHASE_AWAITING_MIGRATION_APPROVAL,
};
pub use platform_stack::{
    resolve_egress_profile, EgressConfig, EgressProfile, NetworkConfig, PlatformStack,
    PlatformStackComponent, PlatformStackComponentOverride, PlatformStackCondition,
    PlatformStackSource, PlatformStackSpec, PlatformStackStatus, PlatformStackValues,
    PlatformStackVersionHistoryEntry,
};
pub use resourceclaim::{
    ResourceClaim, ResourceClaimCondition, ResourceClaimSpec, ResourceClaimStatus,
};
pub use retainedclaim::{ClaimRef, RetainedClaim, RetainedClaimSpec};
pub use serviceprovider::{ServiceProvider, ServiceProviderSpec, ServiceProviderStatus};
pub use sharedvolume::{
    SharedVolume, SharedVolumeCapacity, SharedVolumeCondition, SharedVolumeSpec,
    SharedVolumeStatus, COND_CAPACITY_WARNING, COND_READY,
};
pub use sourcecredential::{
    SealedSecretRef, SourceBackend, SourceCredential, SourceCredentialCondition,
    SourceCredentialSpec, SourceCredentialStatus, SourceGit, SourceRegistry, COND_GIT_PRESENT,
    COND_GIT_VALID, COND_REGISTRY_PRESENT, COND_REGISTRY_VALID, REASON_AUTH_REJECTED,
    REASON_REACHABLE, REASON_UNVERIFIED,
};
