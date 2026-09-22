// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Shared types and helpers for the AppRafter operator.

pub mod application;
pub mod capacity;
pub mod events;
pub mod k8s_time;
pub mod leader;
pub mod matching;
pub mod metrics;
pub mod migration;
pub mod migration_plan;
pub mod migration_state;
pub mod platform_stack;
pub mod problems;
pub mod promscrape;
pub mod resourceclaim;
pub mod retainedclaim;
pub mod serviceprovider;
pub mod shareddatabase;
pub mod sharedvolume;
pub mod sourcecredential;

pub use application::{
    image_repo, AppResources, Application, ApplicationBaseSpec, ApplicationCondition,
    ApplicationEnvOverride, ApplicationExpose, ApplicationSpec, ApplicationStatus,
    ContainerRecommendation, DiskClaim, EnvConfigStatus, EnvRef, EnvValue, ExposeOverride,
    ImagePolicy, JetStreamConsume, JetStreamConsumerLimits, JetStreamDeadLetter, JetStreamNeed,
    JetStreamStream, NeedEntry, Needs, OneOrMany, Probe, Probes, RecommendedResources, ServiceNeed,
    StatusImage, StatusImagePin, StatusImagePrevious, ANN_IMAGE_PIN, ANN_IMAGE_PINNED_AT,
    COND_IMAGE_RESOLVED, COND_PUBLIC_ROUTE_READY, COND_RESOURCE_CLAIM_PENDING,
    PHASE_AWAITING_RESOURCE_CLAIM, PHASE_ENV_SECRET_MISSING, PHASE_INVALID_EFFECTIVE_SPEC,
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
    resolve_egress_profile, AutoscaleConfig, AutoscaleMode, EdgeFirewallConfig, EgressConfig,
    EgressProfile, NetworkConfig, PlatformStack, PlatformStackComponent,
    PlatformStackComponentOverride, PlatformStackCondition, PlatformStackSource, PlatformStackSpec,
    PlatformStackStatus, PlatformStackValues, PlatformStackVersionHistoryEntry,
    ResourceGovernanceConfig, ResourceQuantities,
};
pub use resourceclaim::{
    ClaimCapacity, ClaimSize, ClaimStreamInventory, ResourceClaim, ResourceClaimCondition,
    ResourceClaimJetStream, ResourceClaimSpec, ResourceClaimStatus,
};
pub use retainedclaim::{ClaimRef, RetainedClaim, RetainedClaimSpec};
pub use serviceprovider::{ServiceProvider, ServiceProviderSpec, ServiceProviderStatus};
// `COND_READY` is re-exported from `sharedvolume` (both modules define the
// same "Ready" literal); SharedDatabase's is reached as
// `shareddatabase::COND_READY` where the distinction matters.
pub use shareddatabase::{
    PgExtension, SharedDatabase, SharedDatabaseCondition, SharedDatabaseSpec, SharedDatabaseStatus,
    COND_EXTENSION_UNAVAILABLE,
};
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
