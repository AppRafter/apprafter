// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! kube-rs CRD types for v1alpha1 `ServiceProvider`.
//!
//! Mirrors the OpenAPI v3 CRD shipped in
//! `operator/charts/apprafter-operator/templates/crd-serviceprovider.yaml`
//! and `schemas/v1alpha1/serviceprovider.cue`. Namespaced; the
//! `kube::CustomResource` derive generates the wrapper struct
//! `ServiceProvider` with the standard apiVersion / kind / metadata /
//! spec / status layout.

use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(CustomResource, Clone, Debug, Default, Deserialize, Serialize, JsonSchema, PartialEq)]
#[kube(
    group = "apprafter.io",
    version = "v1alpha1",
    kind = "ServiceProvider",
    namespaced,
    status = "ServiceProviderStatus"
)]
pub struct ServiceProviderSpec {
    /// Built-in service type (closed enum; see `schemas/v1alpha1`).
    #[serde(rename = "type")]
    pub type_: String,
    /// Implementation backend identifier.
    pub backend: String,
    /// Backend-specific configuration, opaque to the platform.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config: Option<serde_json::Value>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, JsonSchema, PartialEq)]
pub struct ServiceProviderStatus {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub health: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn deserializes_spec_with_type_rename_and_raw_config() {
        let spec: ServiceProviderSpec = serde_json::from_value(json!({
            "type": "pg",
            "backend": "cloudnative-pg",
            "config": { "cluster": "platform-postgres", "nodes": 3 }
        }))
        .expect("valid spec");
        assert_eq!(spec.type_, "pg");
        assert_eq!(spec.backend, "cloudnative-pg");
        assert_eq!(
            spec.config.as_ref().unwrap().pointer("/nodes"),
            Some(&json!(3))
        );
    }

    #[test]
    fn serializes_type_back_to_json_key_type() {
        let spec = ServiceProviderSpec {
            type_: "redis".into(),
            backend: "dragonfly".into(),
            config: None,
        };
        let v = serde_json::to_value(&spec).unwrap();
        assert_eq!(v.get("type"), Some(&json!("redis")));
        assert!(v.get("config").is_none());
    }

    #[test]
    fn config_is_optional() {
        let spec: ServiceProviderSpec =
            serde_json::from_value(json!({ "type": "s3", "backend": "minio" })).unwrap();
        assert!(spec.config.is_none());
    }
}
