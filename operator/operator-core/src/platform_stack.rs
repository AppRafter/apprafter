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

/// Resolve the effective egress profile from a PlatformStack spec. Field
/// absent (or `network`/`egress` unset) → the documented default `Internet`.
pub fn resolve_egress_profile(spec: &PlatformStackSpec) -> EgressProfile {
    spec.network
        .as_ref()
        .and_then(|n| n.egress.as_ref())
        .and_then(|e| e.profile)
        .unwrap_or_default()
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub values: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
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
}
