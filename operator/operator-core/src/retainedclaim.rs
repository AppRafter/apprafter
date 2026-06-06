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
    /// Provider `spec.backend` (e.g. `cloudnative-pg` or `dragonfly`).
    pub backend: String,
    /// Shared CNPG `Cluster` name the role + database live in.
    /// CNPG-only (None for dragonfly snapshots).
    #[serde(
        rename = "cnpgCluster",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub cnpg_cluster: Option<String>,
    /// Namespace of the shared CNPG `Cluster` (and the password Secret).
    /// CNPG-only (None for dragonfly snapshots).
    #[serde(
        rename = "cnpgNamespace",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub cnpg_namespace: Option<String>,
    /// Postgres role name (`cnpg::pg_identifier`). CNPG-only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    /// Postgres database name (same identifier as the role). CNPG-only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub database: Option<String>,
    /// DNS-1123 `metadata.name` of the CNPG `Database` CR
    /// (`cnpg::k8s_name`). CNPG-only.
    #[serde(
        rename = "databaseObjectName",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub database_object_name: Option<String>,
    /// `metadata.name` of the basic-auth password Secret in the CNPG
    /// namespace (`{databaseObjectName}-pw`). CNPG-only.
    #[serde(
        rename = "passwordSecretName",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub password_secret_name: Option<String>,

    /// Dragonfly allocation: the shared pool instance this claim's DB
    /// lived on (ADR 0042). None for CNPG snapshots.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instance: Option<String>,
    /// Dragonfly allocation: the numbered logical DB (`$N`) the claim
    /// held on `instance`. None for CNPG snapshots.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dbnum: Option<u16>,
    /// Dragonfly per-claim `$N`-pinned ACL username. None for CNPG.
    #[serde(rename = "aclUser", default, skip_serializing_if = "Option::is_none")]
    pub acl_user: Option<String>,
    /// `metadata.name` of the connection Secret in the claim namespace
    /// (`REDIS_URL` / `REDIS_CHANNEL_PREFIX`). None for CNPG.
    #[serde(
        rename = "connectionSecretRef",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub connection_secret_ref: Option<String>,
    /// Namespace of the connection Secret (the claim's origin namespace).
    /// None for CNPG.
    #[serde(
        rename = "connectionSecretNamespace",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub connection_secret_namespace: Option<String>,

    /// Disk allocation (2.6b): the `metadata.name` of the unowned RWO
    /// PVC the claim provisioned (`status.volumeClaimRef`). The GC
    /// deletes this PVC once `retainUntil` passes. None for CNPG /
    /// dragonfly snapshots.
    #[serde(
        rename = "volumeClaimRef",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub volume_claim_ref: Option<String>,
    /// Disk allocation (2.6b): the namespace of the PVC (the claim's
    /// origin namespace — the PVC is created alongside the workload).
    /// None for CNPG / dragonfly snapshots.
    #[serde(
        rename = "volumeClaimNamespace",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub volume_claim_namespace: Option<String>,
    /// RFC3339 instant after which the GC drops the backend resources +
    /// password/connection Secret (deletion + 7-day grace).
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
        assert_eq!(spec.cnpg_cluster.as_deref(), Some("platform-postgres"));
        assert_eq!(spec.cnpg_namespace.as_deref(), Some("cnpg-system"));
        assert_eq!(
            spec.database_object_name.as_deref(),
            Some("claim-demo-demo-web-pg")
        );
        assert_eq!(
            spec.password_secret_name.as_deref(),
            Some("claim-demo-demo-web-pg-pw")
        );
        assert_eq!(spec.retain_until, "2026-06-10T00:00:00+00:00");
        // Dragonfly fields absent on a CNPG snapshot.
        assert!(spec.instance.is_none());
        assert!(spec.dbnum.is_none());
    }

    #[test]
    fn retained_claim_supports_dragonfly_snapshot() {
        let rc: RetainedClaimSpec = serde_json::from_value(json!({
            "claimRef": {"name": "web-redis", "namespace": "demo"},
            "provider": "redis-integrated",
            "backend": "dragonfly",
            "retainUntil": "2026-06-12T00:00:00Z",
            "instance": "platform-redis-persistent-000",
            "dbnum": 7,
            "aclUser": "claim_demo_web_redis",
            "connectionSecretRef": "web-redis-conn",
            "connectionSecretNamespace": "demo"
        }))
        .unwrap();
        assert_eq!(rc.backend, "dragonfly");
        assert_eq!(rc.dbnum, Some(7));
        assert_eq!(
            rc.instance.as_deref(),
            Some("platform-redis-persistent-000")
        );
        assert_eq!(rc.acl_user.as_deref(), Some("claim_demo_web_redis"));
        assert_eq!(rc.connection_secret_ref.as_deref(), Some("web-redis-conn"));
        assert_eq!(rc.connection_secret_namespace.as_deref(), Some("demo"));
        // CNPG fields are absent on a dragonfly snapshot.
        assert!(rc.cnpg_cluster.is_none());
        assert!(rc.role.is_none());
        assert!(rc.database_object_name.is_none());
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
            cnpg_cluster: Some("platform-postgres".into()),
            cnpg_namespace: Some("cnpg-system".into()),
            role: Some("claim_demo_demo_web_pg".into()),
            database: Some("claim_demo_demo_web_pg".into()),
            database_object_name: Some("claim-demo-demo-web-pg".into()),
            password_secret_name: Some("claim-demo-demo-web-pg-pw".into()),
            instance: None,
            dbnum: None,
            acl_user: None,
            connection_secret_ref: None,
            connection_secret_namespace: None,
            volume_claim_ref: None,
            volume_claim_namespace: None,
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
        // Disk fields are skipped when None (CNPG snapshot).
        assert!(v.get("volumeClaimRef").is_none());
        assert!(v.get("volumeClaimNamespace").is_none());
    }

    #[test]
    fn retained_claim_supports_disk_snapshot() {
        // 2.6b: a disk-backed RetainedClaim carries `volumeClaimRef` +
        // `volumeClaimNamespace` (the unowned RWO PVC the GC deletes) and
        // NONE of the CNPG / dragonfly allocation fields.
        let rc: RetainedClaimSpec = serde_json::from_value(json!({
            "claimRef": {"name": "web-disk-data", "namespace": "demo"},
            "provider": "disk-local",
            "backend": "disk",
            "retainUntil": "2026-06-13T00:00:00Z",
            "volumeClaimRef": "claim-demo-web-disk-data",
            "volumeClaimNamespace": "demo"
        }))
        .unwrap();
        assert_eq!(rc.backend, "disk");
        assert_eq!(
            rc.volume_claim_ref.as_deref(),
            Some("claim-demo-web-disk-data")
        );
        assert_eq!(rc.volume_claim_namespace.as_deref(), Some("demo"));
        // CNPG + dragonfly fields are absent on a disk snapshot.
        assert!(rc.cnpg_cluster.is_none());
        assert!(rc.role.is_none());
        assert!(rc.instance.is_none());
        assert!(rc.acl_user.is_none());

        // Round-trips back with the camelCase renames.
        let v = serde_json::to_value(&rc).unwrap();
        assert_eq!(
            v.get("volumeClaimRef"),
            Some(&json!("claim-demo-web-disk-data"))
        );
        assert_eq!(v.get("volumeClaimNamespace"), Some(&json!("demo")));
        assert!(v.get("volume_claim_ref").is_none());
    }
}
