// SPDX-License-Identifier: FSL-1.1-MIT
//! kube-rs CRD types for v1alpha1 `Application`.
//!
//! Mirrors the OpenAPI v3 CRD shipped in `cli-providers::k8s::application_crd`
//! and `schemas/v1alpha1/application.cue`. The `kube::CustomResource`
//! derive macro generates the wrapper struct `Application` with the
//! standard apiVersion / kind / metadata / spec / status layout —
//! possible because v0.1.25 wrapped the field tree under `spec`.

use std::collections::BTreeMap;

use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(CustomResource, Clone, Debug, Default, Deserialize, Serialize, JsonSchema, PartialEq)]
#[kube(
    group = "apprafter.io",
    version = "v1alpha1",
    kind = "Application",
    namespaced,
    status = "ApplicationStatus"
)]
pub struct ApplicationSpec {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base: Option<ApplicationBaseSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environments: Option<BTreeMap<String, ApplicationBaseSpec>>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, JsonSchema, PartialEq)]
pub struct ApplicationBaseSpec {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replicas: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expose: Option<ApplicationExpose>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<BTreeMap<String, String>>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, JsonSchema, PartialEq)]
pub struct ApplicationExpose {
    pub port: i32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub public: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, JsonSchema, PartialEq)]
pub struct ApplicationStatus {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "observedGeneration"
    )]
    pub observed_generation: Option<i64>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "endpointURL"
    )]
    pub endpoint_url: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use kube::Resource;
    use serde_json::json;

    #[test]
    fn application_kind_and_apiversion_match_crd() {
        // The kube derive macro wires <Application as Resource>::kind()
        // and api_version() to "Application" / "apprafter.io/v1alpha1".
        assert_eq!(Application::kind(&()), "Application");
        assert_eq!(Application::api_version(&()), "apprafter.io/v1alpha1");
        assert_eq!(Application::group(&()), "apprafter.io");
        assert_eq!(Application::version(&()), "v1alpha1");
    }

    #[test]
    fn application_round_trips_through_serde_json() {
        let json_obj = json!({
            "apiVersion": "apprafter.io/v1alpha1",
            "kind": "Application",
            "metadata": { "name": "web", "namespace": "default" },
            "spec": {
                "base": {
                    "image": "ghcr.io/acme/web:1.0",
                    "replicas": 3,
                    "expose": { "port": 8080, "public": false, "network": "internal" },
                    "env": { "LOG_LEVEL": "info" }
                },
                "environments": {
                    "prod": { "replicas": 5 }
                }
            }
        });
        let app: Application = serde_json::from_value(json_obj).unwrap();

        let base = app.spec.base.as_ref().expect("base decoded");
        assert_eq!(base.image.as_deref(), Some("ghcr.io/acme/web:1.0"));
        assert_eq!(base.replicas, Some(3));
        let expose = base.expose.as_ref().expect("expose decoded");
        assert_eq!(expose.port, 8080);
        assert_eq!(expose.network.as_deref(), Some("internal"));
        let env = base.env.as_ref().expect("env decoded");
        assert_eq!(env.get("LOG_LEVEL").map(String::as_str), Some("info"));

        let envs = app.spec.environments.as_ref().expect("environments decoded");
        let prod = envs.get("prod").expect("prod decoded");
        assert_eq!(prod.replicas, Some(5));

        // Round-trip serialize → deserialize.
        let serialized = serde_json::to_value(&app).unwrap();
        let deserialized: Application = serde_json::from_value(serialized).unwrap();
        assert_eq!(deserialized.spec, app.spec);
    }

    #[test]
    fn status_subresource_is_optional() {
        let json_obj = json!({
            "apiVersion": "apprafter.io/v1alpha1",
            "kind": "Application",
            "metadata": { "name": "x" },
            "spec": {}
        });
        let app: Application = serde_json::from_value(json_obj).unwrap();
        assert!(app.status.is_none());
    }
}
