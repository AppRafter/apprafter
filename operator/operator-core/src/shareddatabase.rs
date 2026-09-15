// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! kube-rs CRD types for v1alpha1 `SharedDatabase` (2.29 / ADR 0066).
//!
//! Mirrors `schemas/v1alpha1/shareddatabase.cue` and the generated
//! `operator/charts/apprafter-operator/templates/crd-shareddatabase.yaml`.
//! Namespaced; one database several Applications may bind, with its own
//! lifecycle independent of any of them.
//!
//! **No `connection_secret_ref` in the status, deliberately.** `SharedVolume`
//! publishes a `pvc_ref` because every consumer mounts the identical object;
//! here every consumer gets its OWN credential (ADR 0066 §3), so a shared
//! Secret in the status would be the thing this CRD exists to avoid. The
//! status says what was provisioned, not how to reach it.

use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub const COND_READY: &str = "Ready";
/// A declared extension the running operand image does not provide (ADR 0066
/// §4.2). Raised from the provisioner's own `pg_available_extensions` read as
/// well as CNPG's `Database.status.extensions[]`, because neither alone
/// should leave a claim `Ready=False` with nothing saying why.
pub const COND_EXTENSION_UNAVAILABLE: &str = "ExtensionUnavailable";

#[derive(CustomResource, Clone, Debug, Default, Deserialize, Serialize, JsonSchema, PartialEq)]
#[kube(
    group = "apprafter.io",
    version = "v1alpha1",
    kind = "SharedDatabase",
    namespaced,
    status = "SharedDatabaseStatus",
    shortname = "shdb"
)]
#[serde(rename_all = "camelCase")]
pub struct SharedDatabaseSpec {
    /// `pg` | `redis`. A plain `String` here and an enum in the CRD, matching
    /// `ServiceNeed.size`'s convention in this crate.
    #[serde(rename = "type")]
    pub type_: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selector: Option<std::collections::BTreeMap<String, String>>,
    /// redis only; rejected on `pg` by the webhook.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub persistent: Option<bool>,
    /// pg only. Bounded by the provider seed's allow list — a refusal, not a
    /// preference: the shared cluster serves every tenant and CNPG runs
    /// `CREATE EXTENSION` as superuser.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extensions: Option<Vec<PgExtension>>,
}

/// One PostgreSQL extension to create in a database. Maps 1:1 onto CNPG's
/// `Database.spec.extensions[]`; `ensure` is not exposed, because removing an
/// entry from the list is how you say absent.
#[derive(Clone, Debug, Default, Deserialize, Serialize, JsonSchema, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PgExtension {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, JsonSchema, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SharedDatabaseStatus {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ready: Option<bool>,
    /// DERIVED from the live claims that reference this database and
    /// recomputed on reconcile — never incremented and decremented. That is
    /// the counter that drifts, and this one gates a destructive `rm`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ref_count: Option<i64>,
    /// pg: the database name in the shared cluster.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub database: Option<String>,
    /// redis: the pool instance and the `$N` every consumer is pinned to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instance: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dbnum: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conditions: Option<Vec<SharedDatabaseCondition>>,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq)]
pub struct SharedDatabaseCondition {
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
    fn spec_round_trips_with_type_and_extensions() {
        let spec: SharedDatabaseSpec = serde_json::from_value(json!({
            "type": "pg",
            "size": "small",
            "extensions": [{"name": "vector"}, {"name": "pg_trgm", "version": "1.6"}]
        }))
        .unwrap();
        assert_eq!(spec.type_, "pg");
        let ext = spec.extensions.as_ref().expect("extensions");
        assert_eq!(ext[0].name, "vector");
        assert_eq!(ext[0].version, None);
        assert_eq!(ext[1].version.as_deref(), Some("1.6"));
        // An absent field must not serialize: `persistent` unset is not the
        // same statement as `persistent: false`, which is rejected on pg.
        let out = serde_json::to_value(&spec).unwrap();
        assert!(out.get("persistent").is_none());
    }

    #[test]
    fn the_status_publishes_no_connection_secret() {
        // The absence is the design (ADR 0066 §1): every consumer has its own
        // credential, so a shared Secret here would be the thing this CRD
        // exists to avoid. Asserted so a later "convenience" addition has to
        // delete a test that says why.
        let status = SharedDatabaseStatus {
            ready: Some(true),
            ref_count: Some(2),
            database: Some("shd_apps_orders".into()),
            ..Default::default()
        };
        let out = serde_json::to_value(&status).unwrap();
        assert!(out.get("connectionSecretRef").is_none());
        assert_eq!(out["refCount"], 2);
        assert_eq!(out["database"], "shd_apps_orders");
    }

    #[test]
    fn resource_metadata_is_apprafter_namespaced() {
        assert_eq!(SharedDatabase::group(&()), "apprafter.io");
        assert_eq!(SharedDatabase::kind(&()), "SharedDatabase");
        assert_eq!(SharedDatabase::version(&()), "v1alpha1");
        assert_eq!(SharedDatabase::plural(&()), "shareddatabases");
    }
}
