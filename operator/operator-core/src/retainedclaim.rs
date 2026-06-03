// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! kube-rs CRD type for v1alpha1 `RetainedClaim` (Phase 2.4f).
//!
//! Mirrors the OpenAPI v3 CRD shipped in
//! `operator/charts/apprafter-operator/templates/crd-retainedclaim.yaml`
//! and `schemas/v1alpha1/retainedclaim.cue`. Namespaced (always lives
//! in `apprafter-system` — a platform namespace that outlives tenant
//! namespaces so the GC always fires even if the app's namespace is
//! torn down).
//!
//! A `RetainedClaim` is the immutable snapshot the
//! `resourceclaim-provisioner` finalizer writes when a `ResourceClaim`
//! is deleted, BEFORE it removes its finalizer. It carries everything
//! the 7-day-grace GC controller needs to drop the per-claim Postgres
//! role + database + password Secret once `retainUntil` passes:
//! lineage (`claimRef`), the CNPG target (`cnpgCluster` /
//! `cnpgNamespace`), the derived Postgres + Kubernetes object names,
//! and the grace deadline.
//!
//! Operator-only + immutable: the admission webhook gates CREATE to
//! the operator SA / cluster-admin and rejects any spec mutation on
//! UPDATE (mirrored by an `x-kubernetes-validations` CEL rule on the
//! CRD). No `status` subresource — the GC reads `spec` only.

use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(CustomResource, Clone, Debug, Default, Deserialize, Serialize, JsonSchema, PartialEq)]
#[kube(
    group = "apprafter.io",
    version = "v1alpha1",
    kind = "RetainedClaim",
    namespaced
)]
pub struct RetainedClaimSpec {
    /// Lineage of the deleted `ResourceClaim` this snapshot stands in
    /// for. The snapshot itself lives in `apprafter-system`; the
    /// `claimRef` preserves the original `(name, namespace)`.
    #[serde(rename = "claimRef")]
    pub claim_ref: ClaimRef,
    /// `ServiceProvider` name the deleted claim was matched to.
    pub provider: String,
    /// Provider `spec.backend` (e.g. `cloudnative-pg`).
    pub backend: String,
    /// Shared CNPG `Cluster` name the role + database live in.
    #[serde(rename = "cnpgCluster")]
    pub cnpg_cluster: String,
    /// Namespace of the shared CNPG `Cluster` (and the password Secret).
    #[serde(rename = "cnpgNamespace")]
    pub cnpg_namespace: String,
    /// Postgres role name (`cnpg::pg_identifier`).
    pub role: String,
    /// Postgres database name (same identifier as the role).
    pub database: String,
    /// DNS-1123 `metadata.name` of the CNPG `Database` CR
    /// (`cnpg::k8s_name`).
    #[serde(rename = "databaseObjectName")]
    pub database_object_name: String,
    /// `metadata.name` of the basic-auth password Secret in the CNPG
    /// namespace (`{databaseObjectName}-pw`).
    #[serde(rename = "passwordSecretName")]
    pub password_secret_name: String,
    /// RFC3339 instant after which the GC drops the role + database +
    /// password Secret (deletion + 7-day grace).
    #[serde(rename = "retainUntil")]
    pub retain_until: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, JsonSchema, PartialEq)]
pub struct ClaimRef {
    pub name: String,
    pub namespace: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn deserializes_spec_with_claim_ref_rename_and_retain_until() {
        let spec: RetainedClaimSpec = serde_json::from_value(json!({
            "claimRef": { "name": "demo-web-pg", "namespace": "demo" },
            "provider": "pg-integrated",
            "backend": "cloudnative-pg",
            "cnpgCluster": "platform-postgres",
            "cnpgNamespace": "cnpg-system",
            "role": "claim_demo_demo_web_pg",
            "database": "claim_demo_demo_web_pg",
            "databaseObjectName": "claim-demo-demo-web-pg",
            "passwordSecretName": "claim-demo-demo-web-pg-pw",
            "retainUntil": "2026-06-10T00:00:00+00:00"
        }))
        .expect("valid spec");
        assert_eq!(spec.claim_ref.name, "demo-web-pg");
        assert_eq!(spec.claim_ref.namespace, "demo");
        assert_eq!(spec.cnpg_cluster, "platform-postgres");
        assert_eq!(spec.cnpg_namespace, "cnpg-system");
        assert_eq!(spec.database_object_name, "claim-demo-demo-web-pg");
        assert_eq!(spec.password_secret_name, "claim-demo-demo-web-pg-pw");
        assert_eq!(spec.retain_until, "2026-06-10T00:00:00+00:00");
    }

    #[test]
    fn spec_serializes_back_with_camel_case_renames() {
        let spec = RetainedClaimSpec {
            claim_ref: ClaimRef {
                name: "demo-web-pg".into(),
                namespace: "demo".into(),
            },
            provider: "pg-integrated".into(),
            backend: "cloudnative-pg".into(),
            cnpg_cluster: "platform-postgres".into(),
            cnpg_namespace: "cnpg-system".into(),
            role: "claim_demo_demo_web_pg".into(),
            database: "claim_demo_demo_web_pg".into(),
            database_object_name: "claim-demo-demo-web-pg".into(),
            password_secret_name: "claim-demo-demo-web-pg-pw".into(),
            retain_until: "2026-06-10T00:00:00+00:00".into(),
        };
        let v = serde_json::to_value(&spec).unwrap();
        // camelCase renames land on the wire.
        assert_eq!(
            v.get("claimRef").unwrap().get("name"),
            Some(&json!("demo-web-pg"))
        );
        assert_eq!(v.get("cnpgCluster"), Some(&json!("platform-postgres")));
        assert_eq!(v.get("cnpgNamespace"), Some(&json!("cnpg-system")));
        assert_eq!(
            v.get("databaseObjectName"),
            Some(&json!("claim-demo-demo-web-pg"))
        );
        assert_eq!(
            v.get("passwordSecretName"),
            Some(&json!("claim-demo-demo-web-pg-pw"))
        );
        assert_eq!(
            v.get("retainUntil"),
            Some(&json!("2026-06-10T00:00:00+00:00"))
        );
        // No snake_case keys leak.
        assert!(v.get("claim_ref").is_none());
        assert!(v.get("retain_until").is_none());
    }
}
