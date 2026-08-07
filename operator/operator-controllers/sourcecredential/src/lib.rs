// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! kube-rs Controller for the v1alpha1 `SourceCredential` CRD (1.79c /
//! ADR 0039).
//!
//! A `SourceCredential` is a config-only reference object. This
//! controller is the single owner of every derived materialisation:
//!
//!   - **git half:** for each `repoPrefixes` entry, a prefix-matched
//!     Argo CD `repo-creds` Secret in the `argocd` namespace, so Argo
//!     CD can clone the private repo.
//!   - **registry half:** a canonical `dockerconfigjson` Secret in the
//!     SourceCredential's namespace; the Application controller projects
//!     a per-workload copy and attaches it to `imagePullSecrets`
//!     (Seam A).
//!
//! The credential material is read from the Secret the sealed-secrets
//! controller unsealed (`spec.git.backend.sealedSecretRef.name`,
//! defaulting to the SourceCredential's own namespace). The material
//! Secret carries two keys: `username` and `password` (the PAT). The
//! controller never holds the sealed blob — only the controller's
//! private key unseals, and that already happened by the time this
//! reconcile reads the materialised Secret.
//!
//! Validity is probed live (S5): for each covered prefix the controller
//! finds a representative object actually in use — a git repo from a
//! matching Argo CD `Application`, an image from a matching AppRafter
//! `Application` — and probes it (git smart-HTTP / a scoped registry v2
//! token exchange). The verdict lands in `GitValid` / `RegistryValid`
//! (`True` Reachable / `False` AuthRejected / `Unknown` Unverified) with
//! `status.lastValidated`. When no application references a covered
//! prefix yet, or egress is blocked, the half stays `Unverified` — the
//! state the `present` coverage gate accepts by design (ADR 0039
//! §Validation and status). See `validity.rs`.
//!
//! Derived Secrets are garbage-collected on delete via a finalizer
//! (`apprafter.io/derived-secrets-cleanup`): cross-namespace
//! ownerReferences are disallowed by Kubernetes, so the controller GCs
//! the Argo `repo-creds` (in `argocd`) and the canonical
//! `dockerconfigjson` (selected by the `apprafter.io/source-credential`
//! label) itself before releasing the finalizer. Per-workload
//! pull-secret copies are owned by the Application controller (Seam A)
//! and cascade with their app. (Reclaiming a single derived Secret when
//! only one `repoPrefix` is *removed* — coverage narrowing, distinct
//! from full deletion — is the destructive change the MigrationPlan gate
//! classifies; see `operator-controllers-migration`.)

use std::sync::Arc;
use std::time::Duration;

use base64::Engine;
use chrono::Utc;
use futures::StreamExt;
use k8s_openapi::api::core::v1::Secret;
use kube::api::{Api, DeleteParams, ListParams, Patch, PatchParams};
use kube::runtime::controller::{Action, Controller};
use kube::runtime::watcher;
use kube::{Client, ResourceExt};
use serde_json::{json, Value};
use thiserror::Error;
use tracing::{info, warn};

use operator_core::{
    Metrics, SourceCredential, SourceCredentialCondition, SourceCredentialStatus, COND_GIT_PRESENT,
    COND_GIT_VALID, COND_REGISTRY_PRESENT, COND_REGISTRY_VALID, REASON_UNVERIFIED,
};

mod validity;
use validity::Validity;

const KIND: &str = "SourceCredential";

/// SSA field manager for everything this controller owns (status +
/// derived Secrets). Distinct from `apprafter-operator` so the
/// Application controller and this one never fight over fields.
pub const FIELD_MANAGER: &str = "apprafter-sourcecredential";

/// Namespace Argo CD reads `repo-creds` Secrets from.
const ARGOCD_NAMESPACE: &str = "argocd";

/// Finalizer that blocks SourceCredential deletion until the derived
/// Secrets (cross-namespace, so no ownerReference cascade is possible)
/// are garbage-collected.
const DERIVED_SECRETS_FINALIZER: &str = "apprafter.io/derived-secrets-cleanup";

/// Label every derived Secret carries, keyed to the owning
/// SourceCredential — the GC selector and the human "who made this".
const SOURCE_CREDENTIAL_LABEL: &str = "apprafter.io/source-credential";

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

/// Reconcile: derive the git half's Argo `repo-creds` Secret(s) and the
/// registry half's canonical `dockerconfigjson` from the unsealed
/// material, and report per-half status. Requeues every 60s so material
/// rotation is picked up (15s while material is still unsealing).
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

    // Finalizer-gated cleanup: derived Secrets live in other namespaces
    // (argocd repo-creds, apprafter-system dockerconfigjson), so a
    // cross-namespace ownerReference cascade is impossible (k8s forbids
    // it). A finalizer lets this controller GC them on delete instead.
    let finalizers = cred.metadata.finalizers.clone().unwrap_or_default();
    if cred.metadata.deletion_timestamp.is_some() {
        gc_derived_secrets(&ctx.client, &namespace, &name).await?;
        if finalizers.iter().any(|f| f == DERIVED_SECRETS_FINALIZER) {
            set_finalizers(
                &ctx.client,
                &namespace,
                &name,
                without_finalizer(&finalizers),
            )
            .await?;
        }
        info!(%name, %namespace, "SourceCredential derived Secrets garbage-collected");
        return Ok(Action::await_change());
    }
    if !finalizers.iter().any(|f| f == DERIVED_SECRETS_FINALIZER) {
        set_finalizers(&ctx.client, &namespace, &name, with_finalizer(&finalizers)).await?;
        // The patch re-triggers reconcile; derivation proceeds below
        // anyway so the first pass is not wasted.
    }

    let previous = cred
        .status
        .as_ref()
        .and_then(|s| s.conditions.as_deref())
        .unwrap_or(&[]);

    let mut conditions: Vec<SourceCredentialCondition> = Vec::new();
    let mut covered_prefixes: Vec<String> = Vec::new();
    let mut covered_hosts: Vec<String> = Vec::new();
    let mut last_validated: Option<String> = None;
    let mut pending = false;
    let pp = PatchParams::apply(FIELD_MANAGER).force();

    // ---- git half → Argo repo-creds Secret(s) in `argocd` ----
    if let Some(git) = &cred.spec.git {
        match &git.backend.sealed_secret_ref {
            None => conditions.push(condition(
                COND_GIT_PRESENT,
                "Unknown",
                REASON_UNVERIFIED,
                "git backend uses openBaoPath; not derivable on Tier 1",
                previous,
            )),
            Some(sealed_ref) => {
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
                        pending = true;
                    }
                    Some((username, password)) => {
                        let api: Api<Secret> =
                            Api::namespaced(ctx.client.clone(), ARGOCD_NAMESPACE);
                        for (idx, prefix) in git.repo_prefixes.iter().enumerate() {
                            let url = normalize_repo_url(prefix);
                            let secret_name = repo_cred_secret_name(&name, idx);
                            let payload =
                                repo_cred_payload(&secret_name, &url, &username, &password, &name);
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
                        // Live reachability probe (S5): find a representative
                        // repo covered by each prefix (from a matching Argo
                        // Application) and probe it over git smart-HTTP.
                        let normalized: Vec<String> = git
                            .repo_prefixes
                            .iter()
                            .map(|p| normalize_repo_url(p))
                            .collect();
                        let (verdict, message) = validity::probe_git_half(
                            &ctx.client,
                            &normalized,
                            &username,
                            &password,
                        )
                        .await;
                        if verdict != Validity::Unverified {
                            last_validated = Some(Utc::now().to_rfc3339());
                        }
                        let (status, reason) = verdict.condition_parts();
                        conditions.push(condition(
                            COND_GIT_VALID,
                            status,
                            reason,
                            &message,
                            previous,
                        ));
                    }
                }
            }
        }
    }

    // ---- registry half → canonical dockerconfigjson in this ns ----
    if let Some(registry) = &cred.spec.registry {
        match &registry.backend.sealed_secret_ref {
            None => conditions.push(condition(
                COND_REGISTRY_PRESENT,
                "Unknown",
                REASON_UNVERIFIED,
                "registry backend uses openBaoPath; not derivable on Tier 1",
                previous,
            )),
            Some(sealed_ref) => {
                let material_ns = sealed_ref.namespace.clone().unwrap_or(namespace.clone());
                match read_material(&ctx.client, &material_ns, &sealed_ref.name).await? {
                    None => {
                        conditions.push(condition(
                            COND_REGISTRY_PRESENT,
                            "False",
                            "MaterialMissing",
                            &format!(
                                "unsealed material Secret {material_ns}/{} not found yet",
                                sealed_ref.name
                            ),
                            previous,
                        ));
                        pending = true;
                    }
                    Some((username, password)) => {
                        let dockercfg = dockerconfigjson(&registry.hosts, &username, &password);
                        let secret_name = pull_secret_name(&name);
                        let payload =
                            dockercfg_payload(&secret_name, &namespace, &dockercfg, &name);
                        let api: Api<Secret> = Api::namespaced(ctx.client.clone(), &namespace);
                        api.patch(&secret_name, &pp, &Patch::Apply(&payload))
                            .await?;
                        covered_hosts.extend(registry.hosts.iter().cloned());
                        conditions.push(condition(
                            COND_REGISTRY_PRESENT,
                            "True",
                            "Derived",
                            "dockerconfigjson pull-secret derived from sealed material",
                            previous,
                        ));
                        // Live reachability probe (S5): find a representative
                        // image covered by each host (from a matching
                        // AppRafter Application) and probe it via a scoped
                        // registry v2 token exchange.
                        let (verdict, message) = validity::probe_registry_half(
                            &ctx.client,
                            &registry.hosts,
                            &username,
                            &password,
                        )
                        .await;
                        if verdict != Validity::Unverified {
                            last_validated = Some(Utc::now().to_rfc3339());
                        }
                        let (status, reason) = verdict.condition_parts();
                        conditions.push(condition(
                            COND_REGISTRY_VALID,
                            status,
                            reason,
                            &message,
                            previous,
                        ));
                    }
                }
            }
        }
    }

    patch_status(
        &ctx.client,
        &namespace,
        &name,
        &build_status(conditions, covered_prefixes, covered_hosts, last_validated),
    )
    .await?;
    let outcome = if pending { "pending" } else { "ok" };
    ctx.metrics
        .reconcile_total
        .with_label_values(&[KIND, &namespace, outcome])
        .inc();
    Ok(Action::requeue(Duration::from_secs(if pending {
        15
    } else {
        60
    })))
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

/// Delete every derived Secret owned by this SourceCredential across
/// the namespaces it writes to (Argo `repo-creds` in `argocd`, the
/// canonical `dockerconfigjson` in the credential's own namespace),
/// selected by the `apprafter.io/source-credential` label. Per-workload
/// pull-secret copies are owned by the Application controller (Seam A)
/// and cascade with their app, so they are not GC'd here. Idempotent:
/// a missing Secret is not an error.
async fn gc_derived_secrets(
    client: &Client,
    cred_namespace: &str,
    name: &str,
) -> Result<(), ReconcileError> {
    let selector = format!("{SOURCE_CREDENTIAL_LABEL}={name}");
    let mut namespaces = vec![ARGOCD_NAMESPACE];
    if cred_namespace != ARGOCD_NAMESPACE {
        namespaces.push(cred_namespace);
    }
    for ns in namespaces {
        let api: Api<Secret> = Api::namespaced(client.clone(), ns);
        let lp = ListParams::default().labels(&selector);
        for secret in api.list(&lp).await? {
            if let Some(secret_name) = secret.metadata.name {
                // ignore "already gone" so re-runs after a partial
                // delete still converge
                let _ = api.delete(&secret_name, &DeleteParams::default()).await;
            }
        }
    }
    Ok(())
}

/// Merge-patch the SourceCredential's `metadata.finalizers` to `list`.
async fn set_finalizers(
    client: &Client,
    namespace: &str,
    name: &str,
    list: Vec<String>,
) -> Result<(), ReconcileError> {
    let api: Api<SourceCredential> = Api::namespaced(client.clone(), namespace);
    let patch = json!({ "metadata": { "finalizers": list } });
    api.patch(name, &PatchParams::default(), &Patch::Merge(&patch))
        .await?;
    Ok(())
}

/// `current` with our finalizer appended (idempotent — caller checks
/// absence first, but kept pure for testing).
fn with_finalizer(current: &[String]) -> Vec<String> {
    let mut out = current.to_vec();
    if !out.iter().any(|f| f == DERIVED_SECRETS_FINALIZER) {
        out.push(DERIVED_SECRETS_FINALIZER.to_string());
    }
    out
}

/// `current` without our finalizer.
fn without_finalizer(current: &[String]) -> Vec<String> {
    current
        .iter()
        .filter(|f| *f != DERIVED_SECRETS_FINALIZER)
        .cloned()
        .collect()
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

/// Registry hostname (the `dockerconfigjson` auths key) from a host
/// prefix: the first path segment. `"ghcr.io/myorg/"` → `"ghcr.io"`.
fn registry_hostname(host_prefix: &str) -> String {
    host_prefix
        .split('/')
        .next()
        .unwrap_or(host_prefix)
        .to_string()
}

/// Deterministic name for the canonical derived pull-secret (lives in
/// the SourceCredential's namespace). The Application controller reads
/// it by this name to project per-workload copies (Seam A).
pub fn pull_secret_name(cred_name: &str) -> String {
    format!("srccred-{cred_name}-dockercfg")
}

/// Build a `.dockerconfigjson` body: one `auths` entry per distinct
/// registry hostname covered by `hosts`, all using the same material.
fn dockerconfigjson(hosts: &[String], username: &str, password: &str) -> String {
    let auth = base64::engine::general_purpose::STANDARD.encode(format!("{username}:{password}"));
    let mut auths = serde_json::Map::new();
    for host in hosts {
        let hostname = registry_hostname(host);
        auths
            .entry(hostname)
            .or_insert_with(|| json!({ "username": username, "password": password, "auth": auth }));
    }
    serde_json::to_string(&json!({ "auths": auths })).expect("serialise dockerconfigjson")
}

/// SSA payload for a `kubernetes.io/dockerconfigjson` Secret.
fn dockercfg_payload(
    secret_name: &str,
    namespace: &str,
    dockercfg: &str,
    cred_name: &str,
) -> Value {
    json!({
        "apiVersion": "v1",
        "kind": "Secret",
        "metadata": {
            "name": secret_name,
            "namespace": namespace,
            "labels": {
                "apprafter.io/managed-by": "apprafter",
                "apprafter.io/source-credential": cred_name,
            }
        },
        "type": "kubernetes.io/dockerconfigjson",
        "stringData": { ".dockerconfigjson": dockercfg }
    })
}

fn build_status(
    conditions: Vec<SourceCredentialCondition>,
    covered_prefixes: Vec<String>,
    covered_hosts: Vec<String>,
    last_validated: Option<String>,
) -> SourceCredentialStatus {
    SourceCredentialStatus {
        conditions: Some(conditions),
        covered_repo_prefixes: if covered_prefixes.is_empty() {
            None
        } else {
            Some(covered_prefixes)
        },
        covered_hosts: if covered_hosts.is_empty() {
            None
        } else {
            Some(covered_hosts)
        },
        last_validated,
        // 2.16b-sc migration baseline + pause phase are managed by the gate
        // (Task 7), not the coverage-derivation status writer; default them to
        // absent so this SSA patch never clobbers a gate-written value.
        ..SourceCredentialStatus::default()
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
    fn build_status_omits_empty_covered_lists() {
        let s = build_status(vec![], vec![], vec![], None);
        assert!(s.covered_repo_prefixes.is_none());
        assert!(s.covered_hosts.is_none());
        assert!(s.last_validated.is_none());
        let s = build_status(
            vec![],
            vec!["github.com/acme/".to_string()],
            vec!["ghcr.io/acme/".to_string()],
            Some("2026-05-31T00:00:00+00:00".to_string()),
        );
        assert_eq!(s.covered_repo_prefixes.unwrap(), vec!["github.com/acme/"]);
        assert_eq!(s.covered_hosts.unwrap(), vec!["ghcr.io/acme/"]);
        assert!(s.last_validated.is_some());
    }

    #[test]
    fn finalizer_add_is_idempotent_and_preserves_others() {
        let none: Vec<String> = vec![];
        assert_eq!(
            with_finalizer(&none),
            vec![DERIVED_SECRETS_FINALIZER.to_string()]
        );
        // already present → unchanged (no duplicate)
        let present = vec![DERIVED_SECRETS_FINALIZER.to_string()];
        assert_eq!(with_finalizer(&present), present);
        // foreign finalizers are preserved
        let mixed = vec!["other.io/keep".to_string()];
        let added = with_finalizer(&mixed);
        assert_eq!(
            added,
            vec![
                "other.io/keep".to_string(),
                DERIVED_SECRETS_FINALIZER.to_string()
            ]
        );
    }

    #[test]
    fn finalizer_remove_drops_only_ours() {
        let list = vec![
            "other.io/keep".to_string(),
            DERIVED_SECRETS_FINALIZER.to_string(),
        ];
        assert_eq!(without_finalizer(&list), vec!["other.io/keep".to_string()]);
        let only_ours = vec![DERIVED_SECRETS_FINALIZER.to_string()];
        assert!(without_finalizer(&only_ours).is_empty());
    }

    #[test]
    fn registry_hostname_takes_the_first_path_segment() {
        assert_eq!(registry_hostname("ghcr.io/myorg/"), "ghcr.io");
        assert_eq!(registry_hostname("ghcr.io/myorg"), "ghcr.io");
        assert_eq!(registry_hostname("ghcr.io"), "ghcr.io");
        assert_eq!(
            registry_hostname("registry.gitlab.com/grp/proj"),
            "registry.gitlab.com"
        );
    }

    #[test]
    fn pull_secret_name_is_deterministic() {
        assert_eq!(pull_secret_name("acme"), "srccred-acme-dockercfg");
    }

    #[test]
    fn dockerconfigjson_dedups_hosts_and_encodes_auth() {
        let body = dockerconfigjson(
            &["ghcr.io/org1/".to_string(), "ghcr.io/org2/".to_string()],
            "git",
            "ghp_x",
        );
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        // Both prefixes collapse to the single ghcr.io host.
        assert_eq!(v["auths"].as_object().unwrap().len(), 1);
        assert_eq!(v["auths"]["ghcr.io"]["username"], "git");
        assert_eq!(v["auths"]["ghcr.io"]["password"], "ghp_x");
        let expected_auth = base64::engine::general_purpose::STANDARD.encode("git:ghp_x");
        assert_eq!(v["auths"]["ghcr.io"]["auth"], expected_auth);
    }

    #[test]
    fn dockercfg_payload_is_a_dockerconfigjson_secret() {
        let p = dockercfg_payload(
            "srccred-acme-dockercfg",
            "apprafter-system",
            "{\"auths\":{}}",
            "acme",
        );
        assert_eq!(p["type"], "kubernetes.io/dockerconfigjson");
        assert_eq!(p["metadata"]["namespace"], "apprafter-system");
        assert_eq!(
            p["metadata"]["labels"]["apprafter.io/source-credential"],
            "acme"
        );
        assert_eq!(p["stringData"][".dockerconfigjson"], "{\"auths\":{}}");
    }
}
