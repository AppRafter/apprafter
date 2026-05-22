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
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, JsonSchema, PartialEq)]
pub struct MigrationRisks {
    /// Mirrors `compatibility.cue` change-class vocabulary.
    pub classification: String,
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
