// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Portable backup orchestration engine. Drives the full Kind-2 backup
//! (native extraction + CR serialisation + secret capture + restic snapshot)
//! via the [`KubeExec`] and [`ResticRunner`] traits so the same logic is
//! reusable by the CLI subprocess path and a future in-cluster runner.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use base64::Engine as _;
use cli_core::{CliError, Result};
use serde_json::Value;

use crate::extract::{plan_extraction, run_extraction};
use crate::kube::KubeExec;
use crate::manifest::BackupManifest;
use crate::restic::{restic_backup_argv, restic_init_argv, restic_snapshots_argv};
use crate::restic_runner::ResticRunner;
use crate::sanitize::sanitize_cr;
use crate::ResourceRef;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Staging behaviour.  For now only `Monolithic` is implemented (identical to
/// the CLI's original single-pass behaviour: extract + CRs + secrets all land
/// in one staging dir before a single restic snapshot).  `Sequential` is
/// reserved for a later task and returns a real error when selected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StagingMode {
    /// All artifacts staged together, then a single restic snapshot.
    Monolithic,
    /// Each namespace staged and snapshotted independently (later task).
    Sequential,
}

/// Inputs the backup engine needs.  Constructed by the caller (CLI command
/// or in-cluster runner) and passed to [`run_backup`].
pub struct BackupOpts {
    /// The restic repository path / URL string.
    pub repo: String,
    /// The restic encryption passphrase.
    pub passphrase: String,
    /// Cluster identifier (embedded in the manifest + tag).
    pub cluster_id: String,
    /// RFC-3339 timestamp for the manifest `created_at` field and the tag.
    pub created_at: String,
    /// The live `PlatformStack.status.currentVersion` (embedded in the manifest).
    pub platform_version: String,
    /// App namespaces to back up (already resolved from the app-namespace set).
    pub namespaces: Vec<String>,
    /// The app-namespace subset flag: when `true` the namespaces were narrowed
    /// by `--namespace`/`--select`; used to decorate the restic tag.
    pub is_subset: bool,
    /// Root of the caller-owned staging tempdir. The engine writes to a `data/`
    /// subdirectory under this path.
    pub staging_root: PathBuf,
    /// The `postgres:<major>-alpine` image to use for pg_dump helper pods.
    pub pg_image: String,
    /// Staging / snapshotting behaviour.
    pub staging_mode: StagingMode,
}

// ---------------------------------------------------------------------------
// Engine
// ---------------------------------------------------------------------------

/// Run a full Kind-2 backup (extraction + CRs + secrets + restic snapshot).
///
/// Returns the restic snapshot id on success, or `None` when the summary
/// JSON line is absent (a restic version difference — the backup still
/// succeeded).
///
/// # StagingMode
///
/// * `Monolithic` — everything staged in one pass, then a single snapshot
///   (the original CLI behaviour, byte-for-byte equivalent).
/// * `Sequential` — per-namespace staging (not yet implemented); returns
///   `Err` with a descriptive message when selected.
pub fn run_backup(
    k: &dyn KubeExec,
    r: &dyn ResticRunner,
    opts: &BackupOpts,
) -> Result<Option<String>> {
    Ok(run_backup_with_summary(k, r, opts)?.snapshot_id)
}

// ---------------------------------------------------------------------------
// Pure helpers (mirrors of the helpers in backup.rs)
// ---------------------------------------------------------------------------

pub const APPRAFTER_MANAGED_LABEL: &str = "apprafter.io/managed-by=apprafter";
const APPRAFTER_SYSTEM_NAMESPACE: &str = "apprafter-system";
const PLATFORMSTACK_NAME: &str = "default";
const CNPG_OPERATOR_NS: &str = "cnpg-system";

/// True iff this Argo `Application` JSON carries the
/// `apprafter.io/managed-by: apprafter` label.
pub fn is_user_argo_app(argo_app: &Value) -> bool {
    let (key, value) = APPRAFTER_MANAGED_LABEL
        .split_once('=')
        .expect("APPRAFTER_MANAGED_LABEL is a key=value literal");
    argo_app
        .pointer("/metadata/labels")
        .and_then(Value::as_object)
        .and_then(|labels| labels.get(key))
        .and_then(Value::as_str)
        == Some(value)
}

/// Of the Secret names present in `ns`, keep only those that have a matching
/// SealedSecret.
pub fn secrets_to_back_up(
    secret_names: &[String],
    sealed_secrets: &[Value],
    ns: &str,
) -> Vec<String> {
    let sealed_in_ns: Vec<&str> = sealed_secrets
        .iter()
        .filter(|s| {
            s.pointer("/metadata/namespace").and_then(Value::as_str) == Some(ns)
                || s.pointer("/metadata/namespace").is_none()
        })
        .filter_map(|s| s.pointer("/metadata/name").and_then(Value::as_str))
        .collect();
    secret_names
        .iter()
        .filter(|n| sealed_in_ns.contains(&n.as_str()))
        .cloned()
        .collect()
}

/// `(namespace, name)` of each sealed material Secret a `SourceCredential`
/// references via `spec.git.backend.sealedSecretRef` +
/// `spec.registry.backend.sealedSecretRef`.
fn sourcecred_material_refs(sc: &Value) -> Vec<(String, String)> {
    let own_ns = sc
        .pointer("/metadata/namespace")
        .and_then(Value::as_str)
        .unwrap_or(APPRAFTER_SYSTEM_NAMESPACE);
    let mut refs = Vec::new();
    for ptr in [
        "/spec/git/backend/sealedSecretRef",
        "/spec/registry/backend/sealedSecretRef",
    ] {
        if let Some(r) = sc.pointer(ptr) {
            if let Some(name) = r.pointer("/name").and_then(Value::as_str) {
                let ns = r
                    .pointer("/namespace")
                    .and_then(Value::as_str)
                    .unwrap_or(own_ns)
                    .to_string();
                refs.push((ns, name.to_string()));
            }
        }
    }
    refs
}

/// Build the `ResourceRef`s recorded in `manifest.json`.
fn resource_refs(crs: &[(&str, &Value)], claims: &[Value]) -> Vec<ResourceRef> {
    let mut refs = Vec::new();
    for (kind, cr) in crs {
        refs.push(ResourceRef {
            namespace: cr
                .pointer("/metadata/namespace")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            kind: (*kind).to_string(),
            name: cr
                .pointer("/metadata/name")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            claim_type: None,
        });
    }
    for c in claims {
        refs.push(ResourceRef {
            namespace: c
                .pointer("/metadata/namespace")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            kind: "ResourceClaim".to_string(),
            name: c
                .pointer("/metadata/name")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            claim_type: c
                .pointer("/spec/type")
                .and_then(Value::as_str)
                .map(str::to_string),
        });
    }
    refs
}

/// The restic snapshot tag.
pub fn backup_tag(cluster_id: &str, created_at: &str, subset_namespaces: &[String]) -> String {
    let base = format!("{cluster_id}-{created_at}");
    if subset_namespaces.is_empty() {
        base
    } else {
        format!("{base}-ns-{}", subset_namespaces.join("_"))
    }
}

// ---------------------------------------------------------------------------
// KubeExec-backed helpers (mirrors of the helpers in backup.rs)
// ---------------------------------------------------------------------------

/// `.items[]` of a kubectl list call.  Missing CRDs are silently treated as
/// empty lists (mirrors `backup.rs::list_items`).
pub fn list_items(k: &dyn KubeExec, resource: &str, namespace: Option<&str>) -> Result<Vec<Value>> {
    // Build args: get <resource> [-n <ns> | -A] -o json
    let mut args: Vec<&str> = vec!["get", resource];
    let ns_owned;
    match namespace {
        Some(ns) => {
            ns_owned = ns.to_string();
            args.push("-n");
            args.push(&ns_owned);
        }
        None => args.push("-A"),
    }
    args.push("-o");
    args.push("json");

    match k.get_json(&args) {
        Ok(Some(v)) => Ok(v
            .get("items")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()),
        Ok(None) => Ok(Vec::new()),
        Err(e) => {
            let msg = format!("{e}");
            if msg.contains("the server doesn't have a resource type")
                || msg.contains("doesn't have a resource type")
            {
                Ok(Vec::new())
            } else {
                Err(e)
            }
        }
    }
}

/// Get `PlatformStack/default`.
fn get_platformstack(k: &dyn KubeExec) -> Result<Option<Value>> {
    let args = vec![
        "get",
        "platformstack",
        PLATFORMSTACK_NAME,
        "-n",
        APPRAFTER_SYSTEM_NAMESPACE,
        "-o",
        "json",
    ];
    k.get_json(&args)
}

/// Read the full `.data` of a Secret, base64-decoding each value.
/// Returns `Ok(None)` when the Secret is absent.
#[allow(clippy::type_complexity)]
pub fn read_secret_data(
    k: &dyn KubeExec,
    name: &str,
    namespace: &str,
) -> Result<Option<(BTreeMap<String, Vec<u8>>, String)>> {
    let args = vec!["get", "secret", name, "-n", namespace, "-o", "json"];
    let Some(json) = k.get_json(&args)? else {
        return Ok(None);
    };
    let secret_type = json
        .pointer("/type")
        .and_then(Value::as_str)
        .unwrap_or("Opaque")
        .to_string();
    let mut out = BTreeMap::new();
    if let Some(data) = json.pointer("/data").and_then(Value::as_object) {
        for (k_name, v) in data {
            let b64 = v.as_str().unwrap_or("");
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(b64)
                .map_err(|e| {
                    CliError::Other(format!(
                        "decode secret {namespace}/{name} key {k_name}: {e}"
                    ))
                })?;
            out.insert(k_name.clone(), bytes);
        }
    }
    Ok(Some((out, secret_type)))
}

/// Read `PlatformStack/default.status.currentVersion`.
pub fn read_platform_version(k: &dyn KubeExec) -> Result<String> {
    let ps = get_platformstack(k)?;
    Ok(ps
        .as_ref()
        .and_then(|p| p.pointer("/status/currentVersion"))
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_string())
}

/// The CNPG operand image of the first CNPG Cluster found, for major-matched
/// pg_dump helper image selection.
pub fn first_cnpg_image(k: &dyn KubeExec, namespaces: &[String]) -> Option<String> {
    let mut scan: Vec<&str> = namespaces.iter().map(String::as_str).collect();
    if !scan.contains(&CNPG_OPERATOR_NS) {
        scan.push(CNPG_OPERATOR_NS);
    }
    for ns in scan {
        if let Ok(items) = list_items(k, "clusters.postgresql.cnpg.io", Some(ns)) {
            if let Some(img) = items.iter().find_map(cnpg_cluster_image) {
                return Some(img);
            }
        }
    }
    None
}

fn cnpg_cluster_image(c: &Value) -> Option<String> {
    c.pointer("/spec/imageName")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .or_else(|| {
            c.pointer("/status/image")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
        })
        .map(str::to_string)
}

/// Enumerate ResourceClaims across the given namespaces.
fn claims_in_namespaces(k: &dyn KubeExec, namespaces: &[String]) -> Result<Vec<Value>> {
    let mut all = Vec::new();
    for ns in namespaces {
        all.extend(list_items(k, "resourceclaims.apprafter.io", Some(ns))?);
    }
    Ok(all)
}

/// Probe whether a restic repository already exists (runs `snapshots`).
fn restic_repo_exists(r: &dyn ResticRunner, repo: &str, pass: &str) -> Result<bool> {
    Ok(r.run_stdout(&restic_snapshots_argv(repo), pass).is_ok())
}

// ---------------------------------------------------------------------------
// File writers (pure, identical to backup.rs)
// ---------------------------------------------------------------------------

fn write_manifest(manifest: &BackupManifest, dir: &Path) -> Result<()> {
    let body = serde_json::to_vec_pretty(manifest)
        .map_err(|e| CliError::Other(format!("serialize manifest: {e}")))?;
    std::fs::write(dir.join("manifest.json"), body)
        .map_err(|e| CliError::Other(format!("write manifest.json: {e}")))
}

fn write_crs(crs: &[(String, Value)], crs_dir: &Path) -> Result<()> {
    for (i, (kind, cr)) in crs.iter().enumerate() {
        let name = cr
            .pointer("/metadata/name")
            .and_then(Value::as_str)
            .unwrap_or("unnamed");
        let ns = cr
            .pointer("/metadata/namespace")
            .and_then(Value::as_str)
            .unwrap_or("cluster");
        let sanitized = sanitize_cr(cr);
        let body = serde_json::to_vec_pretty(&sanitized)
            .map_err(|e| CliError::Other(format!("serialize CR {kind}/{name}: {e}")))?;
        let file = format!("{i:03}-{kind}-{ns}-{name}.json");
        std::fs::write(crs_dir.join(&file), body)
            .map_err(|e| CliError::Other(format!("write CR {file}: {e}")))?;
    }
    Ok(())
}

fn write_secret_json(
    dir: &Path,
    name: &str,
    data: &BTreeMap<String, Vec<u8>>,
    secret_type: &str,
) -> Result<()> {
    std::fs::create_dir_all(dir)
        .map_err(|e| CliError::Other(format!("create secret dir {}: {e}", dir.display())))?;
    let encoded: BTreeMap<String, String> = data
        .iter()
        .map(|(k, v)| {
            (
                k.clone(),
                base64::engine::general_purpose::STANDARD.encode(v),
            )
        })
        .collect();
    let envelope = serde_json::json!({ "type": secret_type, "data": encoded });
    let body = serde_json::to_vec_pretty(&envelope)
        .map_err(|e| CliError::Other(format!("serialize secret {name}: {e}")))?;
    std::fs::write(dir.join(format!("{name}.json")), body)
        .map_err(|e| CliError::Other(format!("write secret {name}.json: {e}")))
}

// ---------------------------------------------------------------------------
// Re-export summary info so the CLI caller can print it
// ---------------------------------------------------------------------------

/// Summary counts returned alongside the snapshot id.
pub struct BackupSummary {
    pub snapshot_id: Option<String>,
    pub cr_count: usize,
    pub secret_count: usize,
    pub claim_count: usize,
    pub extracted_count: usize,
    pub tag: String,
}

/// Like [`run_backup`] but also returns the summary counts the CLI uses for
/// its output line.
pub fn run_backup_with_summary(
    k: &dyn KubeExec,
    r: &dyn ResticRunner,
    opts: &BackupOpts,
) -> Result<BackupSummary> {
    match opts.staging_mode {
        StagingMode::Monolithic => run_backup_monolithic_with_summary(k, r, opts),
        StagingMode::Sequential => Err(CliError::Other(
            "sequential staging not yet implemented".into(),
        )),
    }
}

fn run_backup_monolithic_with_summary(
    k: &dyn KubeExec,
    r: &dyn ResticRunner,
    opts: &BackupOpts,
) -> Result<BackupSummary> {
    let data_dir = opts.staging_root.join("data");
    std::fs::create_dir_all(&data_dir)
        .map_err(|e| CliError::Other(format!("create staging data dir: {e}")))?;

    // 1. Native data extraction.
    let claims = claims_in_namespaces(k, &opts.namespaces)?;
    let plan = plan_extraction(&claims);
    run_extraction(k, &plan, &data_dir, &opts.pg_image)?;

    // 2. CRs.
    let crs_dir = data_dir.join("crs");
    std::fs::create_dir_all(&crs_dir)
        .map_err(|e| CliError::Other(format!("create crs dir: {e}")))?;

    let mut captured_crs: Vec<(String, Value)> = Vec::new();

    if let Some(ps) = get_platformstack(k)? {
        captured_crs.push(("PlatformStack".to_string(), ps));
    }

    let source_creds = list_items(k, "sourcecredentials.apprafter.io", None)?;
    for sc in &source_creds {
        captured_crs.push(("SourceCredential".to_string(), sc.clone()));
    }

    let apps = list_items(k, "applications.apprafter.io", None)?;
    let app_crs: Vec<Value> = apps
        .iter()
        .filter(|a| {
            a.pointer("/metadata/namespace")
                .and_then(Value::as_str)
                .map(|ns| opts.namespaces.iter().any(|n| n == ns))
                .unwrap_or(false)
        })
        .cloned()
        .collect();
    for a in &app_crs {
        captured_crs.push(("Application".to_string(), a.clone()));
    }

    let argo_apps = list_items(k, "applications.argoproj.io", None)?;
    let user_argo: Vec<Value> = argo_apps
        .iter()
        .filter(|a| is_user_argo_app(a))
        .cloned()
        .collect();
    for a in &user_argo {
        captured_crs.push(("ArgoApplication".to_string(), a.clone()));
    }

    let mut shared_volumes = Vec::new();
    for ns in &opts.namespaces {
        shared_volumes.extend(list_items(k, "sharedvolumes.apprafter.io", Some(ns))?);
    }
    for sv in &shared_volumes {
        captured_crs.push(("SharedVolume".to_string(), sv.clone()));
    }

    write_crs(&captured_crs, &crs_dir)?;

    // 3. Secrets.
    let secrets_dir = data_dir.join("secrets");
    std::fs::create_dir_all(&secrets_dir)
        .map_err(|e| CliError::Other(format!("create secrets dir: {e}")))?;

    let mut secret_count = 0usize;
    for ns in &opts.namespaces {
        let sealed = list_items(k, "sealedsecrets.bitnami.com", Some(ns))?;
        let secret_names: Vec<String> = list_items(k, "secrets", Some(ns))?
            .iter()
            .filter_map(|s| {
                s.pointer("/metadata/name")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .collect();
        let to_back_up = secrets_to_back_up(&secret_names, &sealed, ns);
        let ns_dir = secrets_dir.join(ns);
        for name in &to_back_up {
            if let Some((data, secret_type)) = read_secret_data(k, name, ns)? {
                write_secret_json(&ns_dir, name, &data, &secret_type)?;
                secret_count += 1;
            }
        }
    }

    let sourcecred_dir = secrets_dir.join("sourcecred");
    let mut sc_refs: Vec<(String, String)> = source_creds
        .iter()
        .flat_map(sourcecred_material_refs)
        .collect();
    sc_refs.sort();
    sc_refs.dedup();
    for (ns, name) in &sc_refs {
        if let Some((data, secret_type)) = read_secret_data(k, name, ns)? {
            write_secret_json(&sourcecred_dir, name, &data, &secret_type)?;
            secret_count += 1;
        }
    }

    // 4. manifest.json.
    let manifest_crs: Vec<(&str, &Value)> =
        captured_crs.iter().map(|(k, v)| (k.as_str(), v)).collect();
    let manifest = BackupManifest {
        cluster_id: opts.cluster_id.clone(),
        created_at: opts.created_at.clone(),
        platform_version: opts.platform_version.clone(),
        namespaces: opts.namespaces.clone(),
        resources: resource_refs(&manifest_crs, &claims),
    };
    write_manifest(&manifest, &data_dir)?;

    // 5. restic.
    let repo = &opts.repo;
    let pass = &opts.passphrase;

    if !restic_repo_exists(r, repo, pass)? {
        if let Some(parent) = Path::new(repo).parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(|e| {
                    CliError::Other(format!("create repo parent {}: {e}", parent.display()))
                })?;
            }
        }
        r.run(&restic_init_argv(repo), pass)?;
    }

    let subset_ns: Vec<String> = if opts.is_subset {
        opts.namespaces.clone()
    } else {
        vec![]
    };
    let tag = backup_tag(&opts.cluster_id, &opts.created_at, &subset_ns);
    let snapshot_id = r.run_backup(
        &restic_backup_argv(repo, &data_dir.to_string_lossy(), &tag),
        pass,
    )?;

    let extracted_count = plan.len();
    let claim_count = claims.len();
    let cr_count = captured_crs.len();

    Ok(BackupSummary {
        snapshot_id,
        cr_count,
        secret_count,
        claim_count,
        extracted_count,
        tag,
    })
}

// ---------------------------------------------------------------------------
// Tests (pure engine helpers)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn is_user_argo_app_checks_managed_by_label() {
        let user = json!({"metadata":{"labels":{"apprafter.io/managed-by":"apprafter"}}});
        let platform = json!({"metadata":{"labels":{"app.kubernetes.io/part-of":"argocd"}}});
        let nolabels = json!({"metadata":{"name":"x"}});
        assert!(is_user_argo_app(&user));
        assert!(!is_user_argo_app(&platform));
        assert!(!is_user_argo_app(&nolabels));
    }

    #[test]
    fn secrets_to_back_up_are_those_with_a_sealedsecret() {
        let sealed = vec![json!({"metadata":{"name":"stripe","namespace":"demo"}})];
        let secrets = vec!["stripe".to_string(), "alpha-pg-conn".to_string()];
        assert_eq!(
            secrets_to_back_up(&secrets, &sealed, "demo"),
            vec!["stripe"]
        );
    }

    #[test]
    fn backup_tag_is_cluster_id_and_timestamp_not_namespace() {
        let t = backup_tag("k3d-demo", "2026-06-20T00:00:00Z", &[]);
        assert!(t.contains("k3d-demo"));
        assert!(t.contains("2026-06-20"));
        let sub = backup_tag("k3d-demo", "2026-06-20T00:00:00Z", &["prod".to_string()]);
        assert!(sub.contains("prod"));
    }
}
