// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! kube-rs Controller for the v1alpha1 `SourceCredential` CRD (1.79c /
//! ADR 0039).
//!
//! A `SourceCredential` is a config-only reference object. This
//! controller is the single owner of every derived materialisation:
//!
//!   - **git half (S2, here):** for each `repoPrefixes` entry, a
//!     prefix-matched Argo CD `repo-creds` Secret in the `argocd`
//!     namespace, so Argo CD can clone the private repo.
//!   - **registry half (S3):** a `dockerconfigjson` pull-secret —
//!     derivation lands in the next slice.
//!
//! The credential material is read from the Secret the sealed-secrets
//! controller unsealed (`spec.git.backend.sealedSecretRef.name`,
//! defaulting to the SourceCredential's own namespace). The material
//! Secret carries two keys: `username` and `password` (the PAT). The
//! controller never holds the sealed blob — only the controller's
//! private key unseals, and that already happened by the time this
//! reconcile reads the materialised Secret.
//!
//! Validity (a live `git ls-remote` / registry probe) is NOT performed
//! yet: the git half is reported `GitPresent=True` + `GitValid=Unknown`
//! (reason `Unverified`). This is exactly the restricted-egress state
//! the coverage gate's `present` default is designed for (ADR 0039
//! §Validation and status); the live probe is a defined follow-on.
//!
//! Known limitations (deferred, tracked for S5): derived `repo-creds`
//! Secrets are not garbage-collected when a `repoPrefix` is removed or
//! the SourceCredential is deleted — cross-namespace ownerReferences are
//! disallowed by Kubernetes, so cleanup needs a finalizer. Coverage
//! removal is the destructive change the MigrationPlan gate handles.

use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use futures::StreamExt;
use k8s_openapi::api::core::v1::Secret;
use kube::api::{Api, Patch, PatchParams};
use kube::runtime::controller::{Action, Controller};
use kube::runtime::watcher;
use kube::{Client, ResourceExt};
use serde_json::{json, Value};
use thiserror::Error;
use tracing::{info, warn};

use operator_core::{
    Metrics, SourceCredential, SourceCredentialCondition, SourceCredentialStatus, COND_GIT_PRESENT,
    COND_GIT_VALID, REASON_UNVERIFIED,
};

const KIND: &str = "SourceCredential";

/// SSA field manager for everything this controller owns (status +
/// derived Secrets). Distinct from `apprafter-operator` so the
/// Application controller and this one never fight over fields.
pub const FIELD_MANAGER: &str = "apprafter-sourcecredential";

/// Namespace Argo CD reads `repo-creds` Secrets from.
const ARGOCD_NAMESPACE: &str = "argocd";

/// Keys in the unsealed material Secret.
const MATERIAL_USERNAME_KEY: &str = "username";
const MATERIAL_PASSWORD_KEY: &str = "password";

/// Username used when the material Secret omits `username` — GitHub
/// accepts any non-empty username with a PAT password.
const DEFAULT_GIT_USERNAME: &str = "git";

#[derive(Debug, Error)]
pub enum ReconcileError {
    #[error("kube-rs error: {0}")]
    Kube(#[from] kube::Error),

    #[error("serde_json error: {0}")]
    Serde(#[from] serde_json::Error),
}

/// Per-controller reconcile context.
pub struct Context {
    pub client: Client,
    pub metrics: Arc<Metrics>,
}

/// Spawn the SourceCredential Controller. Watches
/// `apprafter.io/v1alpha1` `SourceCredential` resources cluster-wide.
pub async fn run(client: Client, metrics: Arc<Metrics>) -> Result<(), ReconcileError> {
    let creds: Api<SourceCredential> = Api::all(client.clone());
    let context = Arc::new(Context { client, metrics });

    Controller::new(creds, watcher::Config::default())
        .run(reconcile, error_policy, context)
        .for_each(|res| async move {
            match res {
                Ok((obj_ref, _action)) => info!(?obj_ref, "sourcecredential step ok"),
                Err(err) => warn!(%err, "sourcecredential step error"),
            }
        })
        .await;
    Ok(())
}

/// Reconcile: derive the git half's Argo `repo-creds` Secret(s) from the
/// unsealed material and report status. Requeues every 60s so material
/// rotation is picked up.
pub async fn reconcile(
    cred: Arc<SourceCredential>,
    ctx: Arc<Context>,
) -> Result<Action, ReconcileError> {
    let name = cred.name_any();
    let namespace = cred.namespace().unwrap_or_default();
    let _timer = ctx
        .metrics
        .reconcile_duration
        .with_label_values(&[KIND])
        .start_timer();

    info!(%name, %namespace, "reconciling SourceCredential");

    let previous = cred
        .status
        .as_ref()
        .and_then(|s| s.conditions.as_deref())
        .unwrap_or(&[]);

    let mut conditions: Vec<SourceCredentialCondition> = Vec::new();
    let mut covered_prefixes: Vec<String> = Vec::new();

    if let Some(git) = &cred.spec.git {
        let Some(sealed_ref) = &git.backend.sealed_secret_ref else {
            // OpenBao backend (Tier 2) — not derivable on a Tier-1
            // SealedSecrets cluster. Surface, do not fail.
            conditions.push(condition(
                COND_GIT_PRESENT,
                "Unknown",
                REASON_UNVERIFIED,
                "git backend uses openBaoPath; not derivable on Tier 1",
                previous,
            ));
            return finish(&ctx, &name, &namespace, &cred, conditions, covered_prefixes).await;
        };

        let material_ns = sealed_ref.namespace.clone().unwrap_or(namespace.clone());
        match read_material(&ctx.client, &material_ns, &sealed_ref.name).await? {
            None => {
                conditions.push(condition(
                    COND_GIT_PRESENT,
                    "False",
                    "MaterialMissing",
                    &format!(
                        "unsealed material Secret {material_ns}/{} not found yet",
                        sealed_ref.name
                    ),
                    previous,
                ));
                // Material may still be unsealing — requeue sooner.
                patch_status(
                    &ctx.client,
                    &namespace,
                    &name,
                    &build_status(conditions, covered_prefixes),
                )
                .await?;
                ctx.metrics
                    .reconcile_total
                    .with_label_values(&[KIND, &namespace, "pending"])
                    .inc();
                return Ok(Action::requeue(Duration::from_secs(15)));
            }
            Some((username, password)) => {
                let pp = PatchParams::apply(FIELD_MANAGER).force();
                for (idx, prefix) in git.repo_prefixes.iter().enumerate() {
                    let url = normalize_repo_url(prefix);
                    let secret_name = repo_cred_secret_name(&name, idx);
                    let payload =
                        repo_cred_payload(&secret_name, &url, &username, &password, &name);
                    let api: Api<Secret> = Api::namespaced(ctx.client.clone(), ARGOCD_NAMESPACE);
                    api.patch(&secret_name, &pp, &Patch::Apply(&payload))
                        .await?;
                    covered_prefixes.push(prefix.clone());
                }
                conditions.push(condition(
                    COND_GIT_PRESENT,
                    "True",
                    "Derived",
                    "Argo repo-creds Secret(s) derived from sealed material",
                    previous,
                ));
                // Live reachability probe not implemented — report
                // Unverified, which the `present` coverage gate accepts.
                conditions.push(condition(
                    COND_GIT_VALID,
                    "Unknown",
                    REASON_UNVERIFIED,
                    "validity probe not yet implemented; coverage gated by presence",
                    previous,
                ));
            }
        }
    }

    finish(&ctx, &name, &namespace, &cred, conditions, covered_prefixes).await
}

/// Patch status + record metric + requeue 60s.
async fn finish(
    ctx: &Arc<Context>,
    name: &str,
    namespace: &str,
    _cred: &Arc<SourceCredential>,
    conditions: Vec<SourceCredentialCondition>,
    covered_prefixes: Vec<String>,
) -> Result<Action, ReconcileError> {
    patch_status(
        &ctx.client,
        namespace,
        name,
        &build_status(conditions, covered_prefixes),
    )
    .await?;
    ctx.metrics
        .reconcile_total
        .with_label_values(&[KIND, namespace, "ok"])
        .inc();
    Ok(Action::requeue(Duration::from_secs(60)))
}

pub fn error_policy(
    cred: Arc<SourceCredential>,
    err: &ReconcileError,
    ctx: Arc<Context>,
) -> Action {
    let name = cred.name_any();
    let namespace = cred.namespace().unwrap_or_default();
    warn!(%name, %namespace, %err, "sourcecredential reconcile error");
    ctx.metrics
        .reconcile_total
        .with_label_values(&[KIND, &namespace, "error"])
        .inc();
    ctx.metrics
        .reconcile_errors
        .with_label_values(&[KIND])
        .inc();
    Action::requeue(Duration::from_secs(30))
}

/// Read `(username, password)` from the unsealed material Secret.
/// Returns `None` if the Secret does not exist yet.
async fn read_material(
    client: &Client,
    namespace: &str,
    name: &str,
) -> Result<Option<(String, String)>, ReconcileError> {
    let api: Api<Secret> = Api::namespaced(client.clone(), namespace);
    let Some(secret) = api.get_opt(name).await? else {
        return Ok(None);
    };
    let data = secret.data.unwrap_or_default();
    let username = data
        .get(MATERIAL_USERNAME_KEY)
        .map(|b| String::from_utf8_lossy(&b.0).into_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| DEFAULT_GIT_USERNAME.to_string());
    let password = data
        .get(MATERIAL_PASSWORD_KEY)
        .map(|b| String::from_utf8_lossy(&b.0).into_owned())
        .unwrap_or_default();
    Ok(Some((username, password)))
}

async fn patch_status(
    client: &Client,
    namespace: &str,
    name: &str,
    status: &SourceCredentialStatus,
) -> Result<(), ReconcileError> {
    let api: Api<SourceCredential> = Api::namespaced(client.clone(), namespace);
    let pp = PatchParams::apply(FIELD_MANAGER).force();
    let payload = json!({
        "apiVersion": "apprafter.io/v1alpha1",
        "kind": "SourceCredential",
        "metadata": { "name": name },
        "status": status,
    });
    api.patch_status(name, &pp, &Patch::Apply(&payload)).await?;
    Ok(())
}

// ---------------- pure helpers (unit-tested without kube) ----------------

/// Normalise a repo prefix into an Argo-CD `repo-creds` URL: ensure a
/// scheme (`https://` by default) and strip the trailing slash so the
/// prefix match against `Application.spec.source.repoURL` is clean.
fn normalize_repo_url(prefix: &str) -> String {
    let with_scheme = if prefix.contains("://") {
        prefix.to_string()
    } else {
        format!("https://{prefix}")
    };
    with_scheme.trim_end_matches('/').to_string()
}

/// Deterministic name for the derived `repo-creds` Secret of one prefix.
fn repo_cred_secret_name(cred_name: &str, idx: usize) -> String {
    format!("srccred-{cred_name}-repo-{idx}")
}

/// SSA payload for a derived Argo CD `repo-creds` Secret.
fn repo_cred_payload(
    secret_name: &str,
    url: &str,
    username: &str,
    password: &str,
    cred_name: &str,
) -> Value {
    json!({
        "apiVersion": "v1",
        "kind": "Secret",
        "metadata": {
            "name": secret_name,
            "namespace": ARGOCD_NAMESPACE,
            "labels": {
                "argocd.argoproj.io/secret-type": "repo-creds",
                "apprafter.io/managed-by": "apprafter",
                "apprafter.io/source-credential": cred_name,
            }
        },
        "type": "Opaque",
        "stringData": {
            "url": url,
            "username": username,
            "password": password,
        }
    })
}

fn build_status(
    conditions: Vec<SourceCredentialCondition>,
    covered_prefixes: Vec<String>,
) -> SourceCredentialStatus {
    SourceCredentialStatus {
        conditions: Some(conditions),
        covered_repo_prefixes: if covered_prefixes.is_empty() {
            None
        } else {
            Some(covered_prefixes)
        },
        covered_hosts: None,
        last_validated: None,
    }
}

/// Build a condition, preserving `lastTransitionTime` when the
/// `(type, status)` pair is unchanged — the same hot-loop guard the
/// Application controller uses (identical status ⇒ no-op SSA ⇒ no
/// self-triggered re-reconcile).
fn condition(
    type_: &str,
    status: &str,
    reason: &str,
    message: &str,
    previous: &[SourceCredentialCondition],
) -> SourceCredentialCondition {
    let last_transition_time = previous
        .iter()
        .find(|c| c.type_ == type_ && c.status == status)
        .map(|c| c.last_transition_time.clone())
        .unwrap_or_else(|| Utc::now().to_rfc3339());
    SourceCredentialCondition {
        type_: type_.to_string(),
        status: status.to_string(),
        last_transition_time,
        reason: Some(reason.to_string()),
        message: Some(message.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_repo_url_adds_https_and_strips_trailing_slash() {
        assert_eq!(
            normalize_repo_url("github.com/acme/"),
            "https://github.com/acme"
        );
        assert_eq!(
            normalize_repo_url("github.com/acme"),
            "https://github.com/acme"
        );
    }

    #[test]
    fn normalize_repo_url_keeps_existing_scheme() {
        assert_eq!(
            normalize_repo_url("https://github.com/acme/"),
            "https://github.com/acme"
        );
        assert_eq!(
            normalize_repo_url("ssh://git@github.com/acme"),
            "ssh://git@github.com/acme"
        );
    }

    #[test]
    fn repo_cred_secret_name_is_deterministic_per_prefix() {
        assert_eq!(repo_cred_secret_name("acme", 0), "srccred-acme-repo-0");
        assert_eq!(repo_cred_secret_name("acme", 1), "srccred-acme-repo-1");
    }

    #[test]
    fn repo_cred_payload_is_an_argo_repo_creds_secret() {
        let p = repo_cred_payload(
            "srccred-acme-repo-0",
            "https://github.com/acme",
            "git",
            "ghp_x",
            "acme",
        );
        assert_eq!(p["kind"], "Secret");
        assert_eq!(p["metadata"]["namespace"], "argocd");
        assert_eq!(
            p["metadata"]["labels"]["argocd.argoproj.io/secret-type"],
            "repo-creds"
        );
        assert_eq!(
            p["metadata"]["labels"]["apprafter.io/source-credential"],
            "acme"
        );
        assert_eq!(p["type"], "Opaque");
        assert_eq!(p["stringData"]["url"], "https://github.com/acme");
        assert_eq!(p["stringData"]["username"], "git");
        assert_eq!(p["stringData"]["password"], "ghp_x");
    }

    #[test]
    fn condition_reuses_timestamp_when_status_unchanged() {
        let prev = vec![SourceCredentialCondition {
            type_: COND_GIT_PRESENT.to_string(),
            status: "True".to_string(),
            last_transition_time: "2026-01-01T00:00:00+00:00".to_string(),
            reason: Some("Derived".to_string()),
            message: Some("x".to_string()),
        }];
        let c = condition(COND_GIT_PRESENT, "True", "Derived", "y", &prev);
        assert_eq!(c.last_transition_time, "2026-01-01T00:00:00+00:00");
    }

    #[test]
    fn condition_bumps_timestamp_when_status_changes() {
        let prev = vec![SourceCredentialCondition {
            type_: COND_GIT_PRESENT.to_string(),
            status: "False".to_string(),
            last_transition_time: "2026-01-01T00:00:00+00:00".to_string(),
            reason: Some("MaterialMissing".to_string()),
            message: Some("x".to_string()),
        }];
        let c = condition(COND_GIT_PRESENT, "True", "Derived", "y", &prev);
        assert_ne!(c.last_transition_time, "2026-01-01T00:00:00+00:00");
    }

    #[test]
    fn build_status_omits_empty_covered_prefixes() {
        let s = build_status(vec![], vec![]);
        assert!(s.covered_repo_prefixes.is_none());
        let s = build_status(vec![], vec!["github.com/acme/".to_string()]);
        assert_eq!(s.covered_repo_prefixes.unwrap(), vec!["github.com/acme/"]);
    }
}
