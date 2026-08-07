// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! kube-rs CRD types for v1alpha1 `SourceCredential` (1.79c / ADR 0039).
//!
//! Mirrors the OpenAPI v3 CRD shipped in the operator chart
//! (`templates/crd-sourcecredential.yaml`) and
//! `schemas/v1alpha1/sourcecredential.cue`. A `SourceCredential` is a
//! config-only reference object carrying zero secret material: each
//! `backend` points at sealed material (a SealedSecret on Tier 1, an
//! OpenBao path on Tier 2). The operator derives a prefix-matched Argo
//! `repo-creds` Secret and a host-matched `dockerconfigjson` pull-secret
//! from the one CR + its material.
//!
//! `backend` is modelled as a struct with two optional fields rather
//! than a Rust enum: the structural CRD schema cannot express `oneOf`,
//! so both are optional at the apiserver layer and the admission webhook
//! enforces exactly-one — the same pattern MigrationPlan uses for its
//! scope discriminator.

use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(CustomResource, Clone, Debug, Default, Deserialize, Serialize, JsonSchema, PartialEq)]
#[kube(
    group = "apprafter.io",
    version = "v1alpha1",
    kind = "SourceCredential",
    plural = "sourcecredentials",
    shortname = "srccred",
    namespaced,
    status = "SourceCredentialStatus"
)]
pub struct SourceCredentialSpec {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git: Option<SourceGit>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub registry: Option<SourceRegistry>,
}

/// git-read half: the backend material + the repo URL prefixes Argo CD
/// matches a clone against.
#[derive(Clone, Debug, Default, Deserialize, Serialize, JsonSchema, PartialEq)]
pub struct SourceGit {
    pub backend: SourceBackend,
    #[serde(rename = "repoPrefixes")]
    pub repo_prefixes: Vec<String>,
}

/// registry-pull half: the backend material + the registry-host prefixes
/// the operator matches a rendered image against.
#[derive(Clone, Debug, Default, Deserialize, Serialize, JsonSchema, PartialEq)]
pub struct SourceRegistry {
    pub backend: SourceBackend,
    pub hosts: Vec<String>,
}

/// Where the sealed material lives. Exactly one of `sealed_secret_ref` /
/// `open_bao_path` is set (webhook-enforced).
#[derive(Clone, Debug, Default, Deserialize, Serialize, JsonSchema, PartialEq)]
pub struct SourceBackend {
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "sealedSecretRef"
    )]
    pub sealed_secret_ref: Option<SealedSecretRef>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "openBaoPath"
    )]
    pub open_bao_path: Option<String>,
}

/// Reference to a SealedSecret (and therefore the controller-unsealed
/// Secret of the same name). `namespace` defaults to the
/// SourceCredential's own namespace when omitted.
#[derive(Clone, Debug, Default, Deserialize, Serialize, JsonSchema, PartialEq)]
pub struct SealedSecretRef {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub namespace: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, JsonSchema, PartialEq)]
pub struct SourceCredentialStatus {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conditions: Option<Vec<SourceCredentialCondition>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "coveredRepoPrefixes"
    )]
    pub covered_repo_prefixes: Option<Vec<String>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "coveredHosts"
    )]
    pub covered_hosts: Option<Vec<String>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "lastValidated"
    )]
    pub last_validated: Option<String>,
    /// 2.16b-sc: the last spec the controller successfully DERIVED from
    /// (repo-creds + pull-secret shipped for this coverage). The reconciler
    /// diffs an incoming spec against this baseline to decide whether a
    /// coverage change is safe or needs a gating MigrationPlan — mirrors
    /// `ApplicationStatus.lastAppliedSpec`. Operator-owned; lives in `status`
    /// so Argo (which ignores status) never sees it as drift. The CUE-derived
    /// CRD carries the whole `status` node as
    /// `x-kubernetes-preserve-unknown-fields`, but a bare `type: object` child
    /// under that root still PRUNES its own children at the apiserver — so the
    /// crdmeta `statusSchemaPatches` explicitly re-marks THIS node
    /// preserve-unknown (same walk-found bug class as the 2.16b app baseline).
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "lastAppliedSpec"
    )]
    pub last_applied_spec: Option<SourceCredentialSpec>,
    /// 2.16b-sc: `AwaitingMigrationApproval` while a coverage-removal
    /// MigrationPlan gates this credential; otherwise absent. Shares the
    /// hoisted `PHASE_AWAITING_MIGRATION_APPROVAL` phase string with the
    /// Application scope.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase: Option<String>,
}

/// Condition `type` values. Per-half presence + validity. `status:
/// "Unknown"` with reason `Unverified` marks "could not reach the host
/// to validate" (air-gapped / restricted egress), distinct from an
/// `Invalid` auth failure (`status: "False"`).
pub const COND_GIT_PRESENT: &str = "GitPresent";
pub const COND_GIT_VALID: &str = "GitValid";
pub const COND_REGISTRY_PRESENT: &str = "RegistryPresent";
pub const COND_REGISTRY_VALID: &str = "RegistryValid";
/// `status: "Unknown"` — the host could not be reached to validate
/// (restricted egress / air-gapped), OR no application references the
/// covered prefix yet so there is nothing concrete to probe. The
/// `present` coverage gate accepts this state by design (ADR 0039).
pub const REASON_UNVERIFIED: &str = "Unverified";
/// `status: "True"` — a representative repo / image covered by this
/// half was reached and the credential authenticated (1.79c S5 live
/// validity probe).
pub const REASON_REACHABLE: &str = "Reachable";
/// `status: "False"` — the host actively rejected the credential
/// (HTTP 401/403 on git, auth failure on the registry token exchange).
pub const REASON_AUTH_REJECTED: &str = "AuthRejected";

/// k8s-style condition (mirrors `meta/v1.Condition`).
#[derive(Clone, Debug, Default, Deserialize, Serialize, JsonSchema, PartialEq)]
pub struct SourceCredentialCondition {
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
    fn crd_metadata_matches_the_apiserver_contract() {
        assert_eq!(SourceCredential::group(&()), "apprafter.io");
        assert_eq!(SourceCredential::version(&()), "v1alpha1");
        assert_eq!(SourceCredential::kind(&()), "SourceCredential");
        assert_eq!(SourceCredential::plural(&()), "sourcecredentials");
    }

    #[test]
    fn deserializes_single_pat_spec() {
        let v = json!({
            "apiVersion": "apprafter.io/v1alpha1",
            "kind": "SourceCredential",
            "metadata": {"name": "acme", "namespace": "apprafter-system"},
            "spec": {
                "git": {
                    "backend": {"sealedSecretRef": {"name": "srccred-acme-material"}},
                    "repoPrefixes": ["github.com/acme/"]
                },
                "registry": {
                    "backend": {"sealedSecretRef": {"name": "srccred-acme-material"}},
                    "hosts": ["ghcr.io/acme/"]
                }
            }
        });
        let sc: SourceCredential = serde_json::from_value(v).unwrap();
        let git = sc.spec.git.unwrap();
        assert_eq!(git.repo_prefixes, vec!["github.com/acme/"]);
        assert_eq!(
            git.backend.sealed_secret_ref.unwrap().name,
            "srccred-acme-material"
        );
        let registry = sc.spec.registry.unwrap();
        assert_eq!(registry.hosts, vec!["ghcr.io/acme/"]);
    }

    #[test]
    fn round_trips_registry_only_with_openbao_backend() {
        let sc = SourceCredential::new(
            "vault-cred",
            SourceCredentialSpec {
                git: None,
                registry: Some(SourceRegistry {
                    backend: SourceBackend {
                        sealed_secret_ref: None,
                        open_bao_path: Some("secret/data/ghcr".to_string()),
                    },
                    hosts: vec!["ghcr.io/acme/".to_string()],
                }),
            },
        );
        let v = serde_json::to_value(&sc).unwrap();
        assert_eq!(
            v["spec"]["registry"]["backend"]["openBaoPath"],
            "secret/data/ghcr"
        );
        assert!(v["spec"]["git"].is_null());
        // The generated wrapper does not derive PartialEq; compare the
        // re-serialised JSON instead to prove the round-trip is lossless.
        let back: SourceCredential = serde_json::from_value(v.clone()).unwrap();
        assert_eq!(serde_json::to_value(&back).unwrap(), v);
    }

    // 2.16b-sc: `SourceCredentialStatus.lastAppliedSpec` carries a FULL
    // `SourceCredentialSpec` snapshot. This proves the nested spec survives a
    // serde round-trip through the status (a Rust-side sanity check) AND — via
    // the crdmeta `statusSchemaPatches` preserve-unknown patch (the CRD gate)
    // — that the apiserver won't prune the nested node. It's the SAME
    // walk-found bug class as the 2.16b app-baseline: a bare `type: object`
    // child under the preserve-unknown status root prunes its children unless
    // explicitly re-marked preserve-unknown.
    #[test]
    fn status_last_applied_spec_round_trips_with_nested_spec_and_phase() {
        let baseline = SourceCredentialSpec {
            git: Some(SourceGit {
                backend: SourceBackend {
                    sealed_secret_ref: Some(SealedSecretRef {
                        name: "srccred-acme-material".to_string(),
                        namespace: Some("team-a".to_string()),
                    }),
                    open_bao_path: None,
                },
                repo_prefixes: vec!["github.com/acme/".to_string()],
            }),
            registry: Some(SourceRegistry {
                backend: SourceBackend {
                    sealed_secret_ref: None,
                    open_bao_path: Some("secret/data/ghcr".to_string()),
                },
                hosts: vec!["ghcr.io/acme/".to_string()],
            }),
        };
        let status = SourceCredentialStatus {
            last_applied_spec: Some(baseline.clone()),
            phase: Some(
                super::super::migration_state::PHASE_AWAITING_MIGRATION_APPROVAL.to_string(),
            ),
            ..SourceCredentialStatus::default()
        };
        let v = serde_json::to_value(&status).unwrap();
        // The nested spec must serialise under the `lastAppliedSpec` key with
        // its own children intact (the pruning bug would drop these).
        assert_eq!(
            v["lastAppliedSpec"]["git"]["repoPrefixes"][0],
            "github.com/acme/"
        );
        assert_eq!(
            v["lastAppliedSpec"]["registry"]["backend"]["openBaoPath"],
            "secret/data/ghcr"
        );
        assert_eq!(v["phase"], "AwaitingMigrationApproval");
        // Full round-trip: the nested spec deserialises back identically.
        let back: SourceCredentialStatus = serde_json::from_value(v).unwrap();
        assert_eq!(back.last_applied_spec.as_ref(), Some(&baseline));
        assert_eq!(back.phase.as_deref(), Some("AwaitingMigrationApproval"));
    }
}
