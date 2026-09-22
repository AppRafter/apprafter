// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! kube-rs CRD types for v1alpha1 `PlatformStack`.
//!
//! Mirrors the OpenAPI v3 CRD shipped in
//! `operator/charts/apprafter-operator/templates/crd-platformstack.yaml`
//! and `schemas/v1alpha1/platformstack.cue`. The `kube::CustomResource`
//! derive macro generates the wrapper struct `PlatformStack` with the
//! standard apiVersion / kind / metadata / spec / status layout.

use std::collections::BTreeMap;

use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(CustomResource, Clone, Debug, Default, Deserialize, Serialize, JsonSchema, PartialEq)]
#[kube(
    group = "apprafter.io",
    version = "v1alpha1",
    kind = "PlatformStack",
    namespaced,
    status = "PlatformStackStatus"
)]
pub struct PlatformStackSpec {
    #[serde(default = "default_channel")]
    pub channel: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pin: Option<String>,
    #[serde(default, rename = "autoUpgrade")]
    pub auto_upgrade: bool,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "defaultEnvironment"
    )]
    pub default_environment: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network: Option<NetworkConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub firewall: Option<EdgeFirewallConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backup: Option<BackupConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resources: Option<ResourceGovernanceConfig>,
    pub source: PlatformStackSource,
    pub values: PlatformStackValues,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overrides: Option<BTreeMap<String, PlatformStackComponentOverride>>,
}

fn default_channel() -> String {
    "stable".to_string()
}

/// Cluster-wide egress posture for app-derived CiliumNetworkPolicies (2.10).
/// Gates which baseline rules the renderer emits; needs-derived rules are
/// always emitted regardless of profile. See ADR 0045 §Decision #3.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum EgressProfile {
    /// DNS + same-ns + `world` + needs — internet open; the documented default.
    #[default]
    Internet,
    /// DNS + same-ns + needs — in-cluster only, no internet.
    Internal,
    /// DNS + needs — maximal; even same-namespace egress is denied.
    Strict,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
pub struct NetworkConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub egress: Option<EgressConfig>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
pub struct EgressConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<EgressProfile>,
}

/// Edge-firewall posture of the node this cluster runs on (A4). Mirrors
/// `schemas/v1alpha1/platformstack.cue#PlatformStackSpec`'s `firewall?` block.
///
/// No operator code reads this: the firewall is a CLOUD object the CLI
/// reconciles out-of-cluster. The field exists so the intent — which used to
/// live ONLY in the operator's local target store — rides the CR into every
/// backup, including the scheduled in-cluster runner's, and can be replayed
/// onto the target a restore provisions.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
pub struct EdgeFirewallConfig {
    /// Is the node's 80/443 restricted to Cloudflare's IP ranges? `None` is
    /// UNKNOWN — a CR written before this field existed — and must never be
    /// read as "off".
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "cloudflareOrigin"
    )]
    pub cloudflare_origin: Option<bool>,
}

/// Resolve the effective egress profile from a PlatformStack spec. Field
/// absent (or `network`/`egress` unset) → the documented default `Internet`.
pub fn resolve_egress_profile(spec: &PlatformStackSpec) -> EgressProfile {
    spec.network
        .as_ref()
        .and_then(|n| n.egress.as_ref())
        .and_then(|e| e.profile)
        .unwrap_or_default()
}

/// Opt-in automated off-site backup (2.6d-4). Absent (`spec.backup` unset) =
/// disabled. Mirrors `schemas/v1alpha1/platformstack.cue#PlatformStackSpec`'s
/// `backup?` block. Credentials never live here — only a `credentialRef`
/// name pointing at a Secret the operator resolves.
#[derive(Clone, Debug, Default, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
pub struct BackupConfig {
    #[serde(default)]
    pub enabled: bool,
    pub schedule: String,
    /// `activeDeadlineSeconds` of each scheduled backup Job. `None` on a CR
    /// that does not set it, which the chart then fills with its own
    /// six-hour default — optional rather than defaulted here, like
    /// `checkReadDataSubset`, so the default lives in one place.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "activeDeadlineSeconds"
    )]
    pub active_deadline_seconds: Option<i64>,
    pub bucket: String,
    /// Human name this cluster's snapshots are listed under (the restic
    /// `--host`). `None` on a CR written before the field existed, which the
    /// chart then fills with the fixed `apprafter-backup` host every cluster
    /// used before. NOT an identity: it is replayed by restore, so a clone
    /// inherits it — snapshots are attributed by the `kube-system` UID.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "clusterName"
    )]
    pub cluster_name: Option<String>,
    #[serde(rename = "credentialRef")]
    pub credential_ref: CredentialRef,
    #[serde(rename = "stagingMode")]
    pub staging_mode: String,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "stagingSizeLimit"
    )]
    pub staging_size_limit: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retention: Option<RetentionConfig>,
    #[serde(rename = "checkSchedule")]
    pub check_schedule: String,
    /// `activeDeadlineSeconds` of each weekly check Job; `None` = the chart's
    /// six-hour default.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "checkActiveDeadlineSeconds"
    )]
    pub check_active_deadline_seconds: Option<i64>,
    /// IANA timezone for both schedules → `CronJob.spec.timeZone` (2.22g).
    /// Absent = the kube-controller-manager's zone, which is what every
    /// cluster did before this field and is the trap the field closes.
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "timeZone")]
    pub time_zone: Option<String>,
    #[serde(default, rename = "checkReadData")]
    pub check_read_data: bool,
    /// Deep-verify a random subset each weekly check
    /// (`--read-data-subset`). `None` on a CR written before the field
    /// existed, which the chart then fills with its own default —
    /// optional rather than defaulted here so an absent value stays
    /// distinguishable from an explicit "structure only" (`""`).
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "checkReadDataSubset"
    )]
    pub check_read_data_subset: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "failureWebhook"
    )]
    pub failure_webhook: Option<String>,
}

/// Name-only reference to a Secret carrying the backup destination
/// credentials. Kept opaque here (the operator resolves it) so the spec
/// never embeds secret material.
#[derive(Clone, Debug, Default, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
pub struct CredentialRef {
    pub name: String,
}

/// Snapshot retention policy. Absent → the platform-stack `backup`
/// component's built-in defaults apply. `enforce` selects whether the
/// operator or the in-cluster backup component prunes.
#[derive(Clone, Debug, Default, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
pub struct RetentionConfig {
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "keepDaily")]
    pub keep_daily: Option<i64>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "keepWeekly"
    )]
    pub keep_weekly: Option<i64>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "keepMonthly"
    )]
    pub keep_monthly: Option<i64>,
    pub enforce: String,
}

/// A resource-name → quantity map (cpu/memory → "25m"/"32Mi"). Same shape as
/// the app-side `AppResources` maps; used for VPA min/maxAllowed floors.
#[derive(Clone, Debug, Default, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
pub struct ResourceQuantities(pub std::collections::BTreeMap<String, String>);

impl<const N: usize> From<[(&str, &str); N]> for ResourceQuantities {
    fn from(pairs: [(&str, &str); N]) -> Self {
        ResourceQuantities(
            pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        )
    }
}

/// Cluster-wide VPA enforcement posture (2.16e / ADR 0054). Read-with-fallback
/// by the operator — NEVER written by the operator (PlatformController owns
/// spec). `full` is the shipped default: bidirectional in-place right-sizing.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum AutoscaleMode {
    #[default]
    Full,
    UpOnly,
    Off,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
pub struct AutoscaleConfig {
    #[serde(default)]
    pub mode: AutoscaleMode,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "minAllowed"
    )]
    pub min_allowed: Option<ResourceQuantities>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "maxAllowed"
    )]
    pub max_allowed: Option<ResourceQuantities>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
pub struct ResourceGovernanceConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub autoscale: Option<AutoscaleConfig>,
}

/// Resolve the effective backup staging mode from a PlatformStack spec.
/// Field absent (`backup` unset) → the documented default `"monolithic"`.
pub fn resolve_backup_staging_mode(spec: &PlatformStackSpec) -> &str {
    spec.backup
        .as_ref()
        .map(|b| b.staging_mode.as_str())
        .unwrap_or("monolithic")
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq)]
pub struct PlatformStackSource {
    pub upstream: String,
    #[serde(rename = "repoURL")]
    pub repo_url: String,
    #[serde(rename = "checkInterval")]
    pub check_interval: String,
}

impl Default for PlatformStackSource {
    fn default() -> Self {
        Self {
            upstream: "oci://ghcr.io/apprafter/platform-stack".into(),
            repo_url: "oci://ghcr.io/apprafter/platform-stack".into(),
            check_interval: "6h".into(),
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, JsonSchema, PartialEq)]
pub struct PlatformStackValues {
    pub tier: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,
    // Tier-specific extra fields preserved as a raw map; the CRD
    // declares `x-kubernetes-preserve-unknown-fields: true` on
    // `spec.values`.
    #[serde(flatten)]
    pub extras: BTreeMap<String, serde_json::Value>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, JsonSchema, PartialEq)]
pub struct PlatformStackComponentOverride {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pin: Option<String>,
    // `schema_with`, not the derived schema: schemars 1.x renders
    // `serde_json::Value` as the boolean schema `true`, and the openapi3
    // `ReplaceBoolSchemas` transform does not descend into
    // `additionalProperties` — so under the `overrides` map this node stays a
    // bare `true`, which kube-derive cannot read as a `JSONSchemaProps`, and
    // `PlatformStack::crd()` panics (crdgen's assertion B is its only caller).
    // `{"nullable": true}` is byte-for-byte what schemars 0.8 emitted here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(schema_with = "untyped_nullable")]
    pub values: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
}

/// An untyped, nullable schema node — what schemars 0.8 derived for an
/// `Option<serde_json::Value>`. See `PlatformStackComponentOverride.values`.
fn untyped_nullable(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({ "nullable": true })
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, JsonSchema, PartialEq)]
pub struct PlatformStackStatus {
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "currentVersion"
    )]
    pub current_version: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "targetVersion"
    )]
    pub target_version: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "availableVersion"
    )]
    pub available_version: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "lastUpstreamCheck"
    )]
    pub last_upstream_check: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub components: Option<Vec<PlatformStackComponent>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "versionHistory"
    )]
    pub version_history: Option<Vec<PlatformStackVersionHistoryEntry>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conditions: Option<Vec<PlatformStackCondition>>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, JsonSchema, PartialEq)]
pub struct PlatformStackComponent {
    pub name: String,
    pub version: String,
    pub ready: bool,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, JsonSchema, PartialEq)]
pub struct PlatformStackVersionHistoryEntry {
    pub version: String,
    #[serde(rename = "appliedAt")]
    pub applied_at: String,
    pub outcome: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq)]
pub struct PlatformStackCondition {
    #[serde(rename = "type")]
    pub type_: String,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(rename = "lastTransitionTime")]
    pub last_transition_time: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `PlatformStack::crd()` panicked under schemars 1.x ("valid
    /// JSONSchemaProps from schemars schema: invalid type: boolean `true`")
    /// because the untyped `overrides.*.values` node, nested under
    /// `additionalProperties`, survived as the boolean schema `true`. crdgen's
    /// assertion B is its only caller, so the panic would surface as a red
    /// `crd-check` with no hint of the cause — pin the node here instead.
    #[test]
    fn the_derived_crd_builds_with_override_values_as_an_untyped_node() {
        use kube::CustomResourceExt;
        let crd = serde_json::to_value(PlatformStack::crd()).expect("derived CRD");
        let values = &crd["spec"]["versions"][0]["schema"]["openAPIV3Schema"]["properties"]["spec"]
            ["properties"]["overrides"]["additionalProperties"]["properties"]["values"];
        assert_eq!(values, &serde_json::json!({ "nullable": true }));
    }

    #[test]
    fn default_environment_round_trips_camel_case() {
        let spec: PlatformStackSpec = serde_json::from_value(serde_json::json!({
            "source": {
                "upstream": "oci://ghcr.io/apprafter/platform-stack",
                "repoURL": "oci://ghcr.io/apprafter/platform-stack",
                "checkInterval": "6h"
            },
            "values": { "tier": 1 },
            "defaultEnvironment": "prod"
        }))
        .unwrap();
        assert_eq!(spec.default_environment.as_deref(), Some("prod"));
        let v = serde_json::to_value(&spec).unwrap();
        assert_eq!(
            v.get("defaultEnvironment").and_then(|x| x.as_str()),
            Some("prod")
        );
    }

    #[test]
    fn egress_profile_resolves_with_internet_default_when_absent() {
        let spec: PlatformStackSpec = serde_json::from_value(serde_json::json!({
            "source": {
                "upstream": "oci://ghcr.io/apprafter/platform-stack",
                "repoURL": "oci://ghcr.io/apprafter/platform-stack",
                "checkInterval": "6h"
            },
            "values": { "tier": 1 }
        }))
        .unwrap();
        assert!(spec.network.is_none());
        assert_eq!(resolve_egress_profile(&spec), EgressProfile::Internet);
    }

    #[test]
    fn egress_profile_defaults_to_internet_when_network_present_but_egress_absent() {
        // `network` object present but `egress` (and thus `profile`) unset →
        // still resolves to the documented default `Internet`.
        let spec: PlatformStackSpec = serde_json::from_value(serde_json::json!({
            "source": {
                "upstream": "oci://ghcr.io/apprafter/platform-stack",
                "repoURL": "oci://ghcr.io/apprafter/platform-stack",
                "checkInterval": "6h"
            },
            "values": { "tier": 1 },
            "network": {}
        }))
        .unwrap();
        assert!(spec.network.is_some());
        assert!(spec.network.as_ref().unwrap().egress.is_none());
        assert_eq!(resolve_egress_profile(&spec), EgressProfile::Internet);
    }

    #[test]
    fn egress_profile_reads_explicit_strict() {
        let spec: PlatformStackSpec = serde_json::from_value(serde_json::json!({
            "source": {
                "upstream": "oci://ghcr.io/apprafter/platform-stack",
                "repoURL": "oci://ghcr.io/apprafter/platform-stack",
                "checkInterval": "6h"
            },
            "values": { "tier": 1 },
            "network": { "egress": { "profile": "strict" } }
        }))
        .unwrap();
        assert_eq!(resolve_egress_profile(&spec), EgressProfile::Strict);
        // round-trips back to camel/lowercase JSON
        let v = serde_json::to_value(&spec).unwrap();
        assert_eq!(
            v.pointer("/network/egress/profile")
                .and_then(|x| x.as_str()),
            Some("strict")
        );
    }

    #[test]
    fn backup_absent_leaves_spec_backup_none() {
        let spec: PlatformStackSpec = serde_json::from_value(serde_json::json!({
            "source": {
                "upstream": "oci://ghcr.io/apprafter/platform-stack",
                "repoURL": "oci://ghcr.io/apprafter/platform-stack",
                "checkInterval": "6h"
            },
            "values": { "tier": 1 }
        }))
        .unwrap();
        assert!(spec.backup.is_none());
    }

    #[test]
    fn backup_full_block_parses_every_field() {
        let spec: PlatformStackSpec = serde_json::from_value(serde_json::json!({
            "source": {
                "upstream": "oci://ghcr.io/apprafter/platform-stack",
                "repoURL": "oci://ghcr.io/apprafter/platform-stack",
                "checkInterval": "6h"
            },
            "values": { "tier": 1 },
            "backup": {
                "enabled": true,
                "schedule": "0 3 * * *",
                "bucket": "s3://apprafter-backups/cluster-a",
                "credentialRef": { "name": "backup-s3-creds" },
                "stagingMode": "sequential",
                "stagingSizeLimit": "20Gi",
                "retention": {
                    "keepDaily": 7,
                    "keepWeekly": 4,
                    "keepMonthly": 6,
                    "enforce": "cluster"
                },
                "checkSchedule": "0 6 * * 0",
                "checkReadData": true,
                "failureWebhook": "https://hooks.example.com/backup-failed"
            }
        }))
        .unwrap();
        let b = spec.backup.as_ref().expect("backup present");
        assert!(b.enabled);
        assert_eq!(b.schedule, "0 3 * * *");
        assert_eq!(b.bucket, "s3://apprafter-backups/cluster-a");
        assert_eq!(b.credential_ref.name, "backup-s3-creds");
        assert_eq!(b.staging_mode, "sequential");
        assert_eq!(b.staging_size_limit.as_deref(), Some("20Gi"));
        let r = b.retention.as_ref().expect("retention present");
        assert_eq!(r.keep_daily, Some(7));
        assert_eq!(r.keep_weekly, Some(4));
        assert_eq!(r.keep_monthly, Some(6));
        assert_eq!(r.enforce, "cluster");
        assert_eq!(b.check_schedule, "0 6 * * 0");
        assert!(b.check_read_data);
        assert_eq!(
            b.failure_webhook.as_deref(),
            Some("https://hooks.example.com/backup-failed")
        );

        // camelCase round-trip: the serde renames land back on the wire keys.
        let v = serde_json::to_value(&spec).unwrap();
        assert_eq!(
            v.pointer("/backup/credentialRef/name")
                .and_then(|x| x.as_str()),
            Some("backup-s3-creds")
        );
        assert_eq!(
            v.pointer("/backup/stagingMode").and_then(|x| x.as_str()),
            Some("sequential")
        );
        assert_eq!(
            v.pointer("/backup/retention/keepMonthly")
                .and_then(|x| x.as_i64()),
            Some(6)
        );
        assert_eq!(
            v.pointer("/backup/checkReadData").and_then(|x| x.as_bool()),
            Some(true)
        );
    }

    #[test]
    fn backup_minimal_block_omits_optionals() {
        // Only the required (defaulted) fields present; the three optional
        // sub-fields (`stagingSizeLimit`, `retention`, `failureWebhook`) are
        // absent and must round-trip as absent, not serialize as null.
        let spec: PlatformStackSpec = serde_json::from_value(serde_json::json!({
            "source": {
                "upstream": "oci://ghcr.io/apprafter/platform-stack",
                "repoURL": "oci://ghcr.io/apprafter/platform-stack",
                "checkInterval": "6h"
            },
            "values": { "tier": 1 },
            "backup": {
                "enabled": false,
                "schedule": "0 3 * * *",
                "bucket": "s3://apprafter-backups/cluster-a",
                "credentialRef": { "name": "backup-s3-creds" },
                "stagingMode": "monolithic",
                "checkSchedule": "0 6 * * 0",
                "checkReadData": false
            }
        }))
        .unwrap();
        let b = spec.backup.as_ref().expect("backup present");
        assert!(b.staging_size_limit.is_none());
        assert!(b.retention.is_none());
        assert!(b.failure_webhook.is_none());
        let v = serde_json::to_value(&spec).unwrap();
        assert!(v.pointer("/backup/stagingSizeLimit").is_none());
        assert!(v.pointer("/backup/retention").is_none());
        assert!(v.pointer("/backup/failureWebhook").is_none());
    }

    #[test]
    fn staging_mode_resolves_to_monolithic_when_backup_absent() {
        let spec: PlatformStackSpec = serde_json::from_value(serde_json::json!({
            "source": {
                "upstream": "oci://ghcr.io/apprafter/platform-stack",
                "repoURL": "oci://ghcr.io/apprafter/platform-stack",
                "checkInterval": "6h"
            },
            "values": { "tier": 1 }
        }))
        .unwrap();
        assert!(spec.backup.is_none());
        assert_eq!(resolve_backup_staging_mode(&spec), "monolithic");
    }

    #[test]
    fn autoscale_mode_serde_lowercase() {
        assert_eq!(
            serde_json::to_string(&AutoscaleMode::Full).unwrap(),
            "\"full\""
        );
        assert_eq!(
            serde_json::to_string(&AutoscaleMode::UpOnly).unwrap(),
            "\"up-only\""
        );
        assert_eq!(
            serde_json::to_string(&AutoscaleMode::Off).unwrap(),
            "\"off\""
        );
        assert_eq!(AutoscaleMode::default(), AutoscaleMode::Full);
    }

    #[test]
    fn resources_autoscale_roundtrips_and_is_optional() {
        let bare: PlatformStackSpec =
            serde_json::from_value(serde_json::json!({ "source": {}, "values": { "tier": 1 } }))
                .unwrap_or_default();
        assert!(bare.resources.is_none());
        let ac = AutoscaleConfig {
            mode: AutoscaleMode::Full,
            min_allowed: Some(ResourceQuantities::from([
                ("cpu", "25m"),
                ("memory", "32Mi"),
            ])),
            max_allowed: Some(ResourceQuantities::from([
                ("memory", "512Mi"),
                ("cpu", "1"),
            ])),
        };
        let back: AutoscaleConfig =
            serde_json::from_value(serde_json::to_value(&ac).unwrap()).unwrap();
        assert_eq!(back, ac);
    }

    #[test]
    fn staging_mode_reads_explicit_sequential() {
        let spec: PlatformStackSpec = serde_json::from_value(serde_json::json!({
            "source": {
                "upstream": "oci://ghcr.io/apprafter/platform-stack",
                "repoURL": "oci://ghcr.io/apprafter/platform-stack",
                "checkInterval": "6h"
            },
            "values": { "tier": 1 },
            "backup": {
                "enabled": true,
                "schedule": "0 3 * * *",
                "bucket": "s3://b",
                "credentialRef": { "name": "c" },
                "stagingMode": "sequential",
                "checkSchedule": "0 6 * * 0",
                "checkReadData": false
            }
        }))
        .unwrap();
        assert_eq!(resolve_backup_staging_mode(&spec), "sequential");
    }
}
