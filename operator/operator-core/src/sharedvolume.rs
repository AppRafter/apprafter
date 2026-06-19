// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! kube-rs CRD types for v1alpha1 `SharedVolume`.
//!
//! Mirrors the OpenAPI v3 CRD shipped in
//! `operator/charts/apprafter-operator/templates/crd-sharedvolume.yaml`
//! and `schemas/v1alpha1/sharedvolume.cue`. Namespaced; cross-app shared
//! storage (2.6c) — multiple Applications may reference one SharedVolume
//! and the operator mounts the backing PVC into each consumer workload.

use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub const COND_READY: &str = "Ready";
pub const COND_CAPACITY_WARNING: &str = "CapacityWarning";

#[derive(CustomResource, Clone, Debug, Default, Deserialize, Serialize, JsonSchema, PartialEq)]
#[kube(
    group = "apprafter.io",
    version = "v1alpha1",
    kind = "SharedVolume",
    namespaced,
    status = "SharedVolumeStatus",
    shortname = "sv"
)]
#[serde(rename_all = "camelCase")]
pub struct SharedVolumeSpec {
    pub size: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub class: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, JsonSchema, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SharedVolumeStatus {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ready: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pvc_ref: Option<String>,
    // JSON integer; always >= 0, defaults to 0 in the CUE schema
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ref_count: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capacity: Option<SharedVolumeCapacity>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conditions: Option<Vec<SharedVolumeCondition>>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, JsonSchema, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SharedVolumeCapacity {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub used_bytes: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capacity_bytes: Option<i64>,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq)]
pub struct SharedVolumeCondition {
    #[serde(rename = "type")]
    pub type_: String,
    pub status: String,
    #[serde(rename = "lastTransitionTime")]
    pub last_transition_time: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use kube::Resource;
    use serde_json::json;

    #[test]
    fn spec_round_trips_with_size_and_default_class() {
        let spec: SharedVolumeSpec = serde_json::from_value(json!({ "size": "5Gi" })).unwrap();
        assert_eq!(spec.size, "5Gi");
        assert_eq!(spec.class.as_deref(), None);
        let out = serde_json::to_value(&spec).unwrap();
        assert!(out.get("class").is_none());
    }

    #[test]
    fn status_round_trips_camelcase() {
        let status: SharedVolumeStatus = serde_json::from_value(json!({
            "ready": true,
            "pvcRef": "sv-demo-shared",
            "refCount": 2,
            "conditions": [{
                "type": "Ready", "status": "True",
                "lastTransitionTime": "2026-06-19T00:00:00Z"
            }]
        }))
        .unwrap();
        assert_eq!(status.pvc_ref.as_deref(), Some("sv-demo-shared"));
        assert_eq!(status.ref_count, Some(2));
        assert_eq!(status.conditions.as_ref().unwrap()[0].type_, "Ready");
    }

    #[test]
    fn resource_metadata_is_apprafter_namespaced() {
        assert_eq!(SharedVolume::group(&()), "apprafter.io");
        assert_eq!(SharedVolume::kind(&()), "SharedVolume");
        assert_eq!(SharedVolume::version(&()), "v1alpha1");
        assert_eq!(SharedVolume::plural(&()), "sharedvolumes");
    }
}
