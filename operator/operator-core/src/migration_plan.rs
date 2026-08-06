// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! kube-rs CRD types for v1alpha1 `MigrationPlan`.
//!
//! Mirrors the OpenAPI v3 CRD shipped in
//! `operator/charts/apprafter-operator/templates/crd-migrationplan.yaml`
//! and `schemas/v1alpha1/migrationplan.cue`.
//!
//! B.1.75 ships the type only (admission webhook reads it from
//! AdmissionReview JSON via `serde_json::Value`, not via this
//! type). B.1.76 lands the MigrationController reconciler,
//! which is where these structs will actually flow through
//! reconcile signatures.

use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(CustomResource, Clone, Debug, Default, Deserialize, Serialize, JsonSchema, PartialEq)]
#[kube(
    group = "apprafter.io",
    version = "v1alpha1",
    kind = "MigrationPlan",
    namespaced,
    status = "MigrationPlanStatus",
    shortname = "mp",
    shortname = "migplan"
)]
pub struct MigrationPlanSpec {
    pub scope: MigrationPlanScope,
    pub trigger: MigrationTrigger,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub risks: Option<MigrationRisks>,
    /// 2.16b S1.2: EVERY destructive candidate this spec edit produced —
    /// not just the `pick_primary` headline carried by `spec.trigger`. An
    /// approver reads this to see the FULL blast radius so a dangerous op
    /// can't be laundered behind a benign-looking primary. The approval
    /// content hash (`spec.trigger.approvedSpecHash`) covers this WHOLE set
    /// (S-4 gap close), so attaching a lower-severity destructive op that
    /// rides along changes the hash and re-gates the edit. Sorted the same
    /// way `pick_primary` orders candidates (severity desc, then
    /// trigger/field asc) so the list is deterministic.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub changes: Option<Vec<MigrationChange>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<Vec<MigrationStep>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approvers: Option<Vec<String>>,
    /// Previous spec snapshot for platform-scope reject flow.
    /// Free-form JSON; the controller round-trips it without
    /// interpretation.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "previousSpecSnapshot"
    )]
    pub previous_spec_snapshot: Option<serde_json::Value>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, JsonSchema, PartialEq)]
pub struct MigrationPlanScope {
    /// `"application"` or `"platform"`. The webhook enforces
    /// that the matching sub-object is populated.
    #[serde(rename = "type")]
    pub type_: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub application: Option<MigrationApplicationScope>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platform: Option<MigrationPlatformScope>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, JsonSchema, PartialEq)]
pub struct MigrationApplicationScope {
    // CRD field is `ref` (Kubernetes convention) but `ref` is
    // a reserved word in Rust — rename via serde.
    #[serde(rename = "ref")]
    pub ref_: MigrationApplicationRef,
    pub environment: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, JsonSchema, PartialEq)]
pub struct MigrationApplicationRef {
    pub name: String,
    pub namespace: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, JsonSchema, PartialEq)]
pub struct MigrationPlatformScope {
    /// Platform-stack component names this migration affects.
    /// Webhook rejects the platform-scope plan when this list
    /// is empty.
    pub components: Vec<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, JsonSchema, PartialEq)]
pub struct MigrationTrigger {
    #[serde(rename = "type")]
    pub type_: String,
    pub field: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<serde_json::Value>,
    /// 2.16b S-4: SHA-256 of the destructive change(s) this plan was cut
    /// (and thus approved) for. Binds an app-scope approval to the exact
    /// `from`/`to` CONTENT — not just the `(type, field)` tuple — so an
    /// approval is never transferable across a different spec edit. The
    /// Application reconciler re-verifies it at consume time; a mismatch
    /// demotes the completed plan to a relic and re-gates the edit as a
    /// fresh pending-approval plan. Absent (`None`) on legacy plans
    /// cut before S-4 — those still consume (the reconciler treats a
    /// missing hash as "don't break existing plans").
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "approvedSpecHash"
    )]
    pub approved_spec_hash: Option<String>,
}

/// 2.16b S1.2: one detected destructive candidate. `spec.changes[]` carries
/// every candidate an edit produced (`spec.trigger` carries only the primary
/// `pick_primary` picks). `severity` is the ordinal from
/// `migration::classification_severity` so an approval UI can sort/threshold
/// without re-deriving the vocabulary; `from`/`to` mirror the trigger's
/// free-form JSON payload.
#[derive(Clone, Debug, Default, Deserialize, Serialize, JsonSchema, PartialEq)]
pub struct MigrationChange {
    #[serde(rename = "type")]
    pub trigger: String,
    pub field: String,
    pub classification: String,
    pub severity: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<serde_json::Value>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, JsonSchema, PartialEq)]
pub struct MigrationRisks {
    /// Mirrors `compatibility.cue` change-class vocabulary.
    pub classification: String,
    /// 2.16b S1.2: the DISTINCT classification vocabulary across every
    /// detected candidate (`spec.changes[]`), sorted severity desc then
    /// name asc. `classification` above is the primary's (the max); this
    /// list surfaces the full set so an approver sees that e.g. a
    /// `data-migration` primary also carries a `requires-restart` op.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub classifications: Option<Vec<String>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "estimatedDowntime"
    )]
    pub estimated_downtime: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "dataVolume"
    )]
    pub data_volume: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reversible: Option<bool>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "requiresFullBackup"
    )]
    pub requires_full_backup: Option<bool>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, JsonSchema, PartialEq)]
pub struct MigrationStep {
    pub step: u32,
    pub action: String,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "estimatedDuration"
    )]
    pub estimated_duration: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reversible: Option<bool>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, JsonSchema, PartialEq)]
pub struct MigrationPlanStatus {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "approvedAt"
    )]
    pub approved_at: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "approvedBy"
    )]
    pub approved_by: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "executedSteps"
    )]
    pub executed_steps: Option<Vec<ExecutedStep>>,
    /// RFC3339 timestamp recording when MigrationController
    /// successfully applied `strategy.reject()` to a
    /// `phase=rejected` plan. Walk-fix #3 post-B.1.77 marker:
    /// rejected plans are otherwise re-reconciled on every
    /// operator pod restart (cache replay → watcher fires on
    /// existing rejected plans), each replay re-invoking
    /// `strategy.reject()` which patches
    /// `PlatformStack.spec.pin` back to the snapshot value —
    /// overriding any operator action taken since the
    /// original reject. Once `rejectedAt` is set,
    /// MigrationController treats the plan as sealed and
    /// skips `strategy.reject()` on subsequent reconciles.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "rejectedAt"
    )]
    pub rejected_at: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, JsonSchema, PartialEq)]
pub struct ExecutedStep {
    pub step: u32,
    #[serde(rename = "startedAt")]
    pub started_at: String,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "finishedAt"
    )]
    pub finished_at: Option<String>,
    pub outcome: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}
