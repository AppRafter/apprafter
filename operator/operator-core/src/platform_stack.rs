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
    pub source: PlatformStackSource,
    pub values: PlatformStackValues,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overrides: Option<BTreeMap<String, PlatformStackComponentOverride>>,
}

fn default_channel() -> String {
    "stable".to_string()
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
}
