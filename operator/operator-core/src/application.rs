// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! kube-rs CRD types for v1alpha1 `Application`.
//!
//! Mirrors the OpenAPI v3 CRD shipped by the `apprafter-operator`
//! Helm chart (`templates/crd-application.yaml`) and
//! `schemas/v1alpha1/application.cue`. The `kube::CustomResource`
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub needs: Option<BTreeMap<String, ServiceNeed>>,
    /// Image resolution policy (ADR 0040). Absent => default `digest`
    /// (the controller resolves the tag to a registry digest each
    /// reconcile). Mirrors `#ImagePolicy` in application.cue + the CRD.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "imagePolicy"
    )]
    pub image_policy: Option<ImagePolicy>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, JsonSchema, PartialEq)]
pub struct ApplicationExpose {
    pub port: i32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub public: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network: Option<String>,
}

/// One declared platform-service dependency under
/// `Application.spec.*.needs`, keyed by service type. The 2.4d
/// controller turns each entry into a `ResourceClaim`; the 2.3
/// scheduler routes it via `selector`. Mirrors `#ServiceNeed` in
/// `schemas/v1alpha1/application.cue` and the `needs` block of the
/// OpenAPI v3 CRD.
#[derive(Clone, Debug, Default, Deserialize, Serialize, JsonSchema, PartialEq)]
pub struct ServiceNeed {
    /// Label selector matched against `ServiceProvider.metadata.labels`.
    /// Optional — the controller injects `{tier: integrated}` when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selector: Option<BTreeMap<String, String>>,
    /// Requested size class (`nano|small|medium|large|xlarge`).
    /// Optional — tier defaults fill it. Enforced as an enum by the
    /// CRD; a plain `String` here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, JsonSchema, PartialEq)]
pub struct ImagePolicy {
    /// `"digest"` (default) resolves the tag to `repo@sha256:…`; `"off"`
    /// renders the reference verbatim and performs no registry poll.
    /// Enforced as an enum by the CRD; a plain `String` here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolve: Option<String>,
}

/// `status.image` — the resolved-image truth (ADR 0040). `tag` is the
/// reference as written in `spec`; `resolved` is `repo@sha256:…`
/// actually rendered into the Deployment; `resolvedAt` is the RFC3339
/// time of the last successful resolution.
#[derive(Clone, Debug, Default, Deserialize, Serialize, JsonSchema, PartialEq)]
pub struct StatusImage {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tag: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "resolvedAt"
    )]
    pub resolved_at: Option<String>,
}

/// `ImageResolved=False` when tag→digest resolution failed this cycle
/// (registry unreachable, no covering credential for a private image,
/// malformed reference) — the Deployment falls back to the verbatim
/// tag; resolution NEVER blocks the rollout (ADR 0040). `True` after a
/// successful resolution; absent when `imagePolicy.resolve: off`.
pub const COND_IMAGE_RESOLVED: &str = "ImageResolved";

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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conditions: Option<Vec<ApplicationCondition>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "endpointURL"
    )]
    pub endpoint_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<StatusImage>,
}

/// Reserved phase: Application reconciler is paused awaiting
/// approval of a MigrationPlan that gates the destructive
/// change observed on this Application. Walk-fix B.1.77 / ADR
/// 0027.
pub const PHASE_AWAITING_MIGRATION_APPROVAL: &str = "AwaitingMigrationApproval";

/// Condition type emitted alongside the
/// `AwaitingMigrationApproval` phase. `condition.message`
/// carries the MigrationPlan name so operators can `kubectl
/// describe` straight from the Application status.
pub const COND_MIGRATION_PENDING: &str = "MigrationPending";

/// Reserved phase: the Application reconciler is paused awaiting a
/// generated `ResourceClaim` (from `spec.*.needs`) to be provisioned
/// (`status.ready` + `connectionSecretRef`). Phase 2.4d.
pub const PHASE_AWAITING_RESOURCE_CLAIM: &str = "AwaitingResourceClaim";

/// Condition emitted alongside `AwaitingResourceClaim`; `message`
/// carries the unready claim name(s). Phase 2.4d.
pub const COND_RESOURCE_CLAIM_PENDING: &str = "ResourceClaimPending";

/// k8s-style condition (mirrors `meta/v1.Condition`). Operator
/// emits `Ready` of `True` after a successful reconcile.
#[derive(Clone, Debug, Default, Deserialize, Serialize, JsonSchema, PartialEq)]
pub struct ApplicationCondition {
    #[serde(rename = "type")]
    pub type_: String,
    pub status: String,
    #[serde(rename = "lastTransitionTime")]
    pub last_transition_time: String,
    pub reason: String,
    pub message: String,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "observedGeneration"
    )]
    pub observed_generation: Option<i64>,
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

        let envs = app
            .spec
            .environments
            .as_ref()
            .expect("environments decoded");
        let prod = envs.get("prod").expect("prod decoded");
        assert_eq!(prod.replicas, Some(5));

        // Round-trip serialize → deserialize.
        let serialized = serde_json::to_value(&app).unwrap();
        let deserialized: Application = serde_json::from_value(serialized).unwrap();
        assert_eq!(deserialized.spec, app.spec);
    }

    #[test]
    fn needs_round_trips_through_serde_json() {
        let json_obj = json!({
            "apiVersion": "apprafter.io/v1alpha1",
            "kind": "Application",
            "metadata": { "name": "web", "namespace": "default" },
            "spec": {
                "base": {
                    "image": "ghcr.io/acme/web:1.0",
                    "needs": {
                        "pg": { "selector": { "tier": "integrated" } }
                    }
                },
                "environments": {
                    "prod": {
                        "needs": { "pg": { "selector": { "tier": "managed-aws" }, "size": "small" } }
                    }
                }
            }
        });
        let app: Application = serde_json::from_value(json_obj).unwrap();

        let base_needs = app
            .spec
            .base
            .as_ref()
            .unwrap()
            .needs
            .as_ref()
            .expect("base needs");
        let pg = base_needs.get("pg").expect("pg need");
        assert_eq!(
            pg.selector
                .as_ref()
                .and_then(|s| s.get("tier"))
                .map(String::as_str),
            Some("integrated")
        );
        assert_eq!(pg.size, None);

        let prod = app.spec.environments.as_ref().unwrap().get("prod").unwrap();
        let prod_pg = prod.needs.as_ref().unwrap().get("pg").unwrap();
        assert_eq!(prod_pg.size.as_deref(), Some("small"));

        // Round-trip serialize → deserialize.
        let serialized = serde_json::to_value(&app).unwrap();
        let deserialized: Application = serde_json::from_value(serialized).unwrap();
        assert_eq!(deserialized.spec, app.spec);
    }

    #[test]
    fn image_policy_and_status_image_round_trip() {
        let json_obj = json!({
            "apiVersion": "apprafter.io/v1alpha1",
            "kind": "Application",
            "metadata": { "name": "web", "namespace": "default" },
            "spec": { "base": {
                "image": "ghcr.io/acme/web:latest",
                "imagePolicy": { "resolve": "off" }
            }},
            "status": {
                "image": {
                    "tag": "ghcr.io/acme/web:latest",
                    "resolved": "ghcr.io/acme/web@sha256:abc",
                    "resolvedAt": "2026-06-05T00:00:00Z"
                }
            }
        });
        let app: Application = serde_json::from_value(json_obj).unwrap();
        assert_eq!(
            app.spec
                .base
                .unwrap()
                .image_policy
                .unwrap()
                .resolve
                .as_deref(),
            Some("off")
        );
        let img = app.status.unwrap().image.unwrap();
        assert_eq!(img.resolved.as_deref(), Some("ghcr.io/acme/web@sha256:abc"));
        assert_eq!(img.tag.as_deref(), Some("ghcr.io/acme/web:latest"));
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
