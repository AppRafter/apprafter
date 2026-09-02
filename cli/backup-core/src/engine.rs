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

/// Staging behaviour.
///
/// * `Monolithic` — extract + CRs + secrets all land in one staging dir before
///   a single restic snapshot (the CLI's original single-pass behaviour). Peak
///   staged disk = `sum(claims)`.
/// * `Sequential` — each claim is staged, snapshotted, and deleted one at a
///   time (peak = `max(claim)`), then the non-claim components go into a final
///   commit-point manifest snapshot written LAST. Every snapshot shares one
///   `run-<id>` tag. See [`run_backup`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StagingMode {
    /// All artifacts staged together, then a single restic snapshot.
    Monolithic,
    /// Each claim staged + snapshotted + deleted independently (peak =
    /// `max(claim)`), then a final commit-point manifest snapshot.
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
    /// Fixed `--host` passed to every `restic backup` invocation for this run.
    ///
    /// When `Some(h)`, restic groups snapshots under `h` so `restic forget`
    /// retention policies apply across runs (spec §Retention M-r3-1a). Set to
    /// `Some("apprafter-backup")` in the in-cluster runner (where the pod name
    /// is ephemeral). Leave as `None` for the CLI local-pull path, which keeps
    /// the machine's own hostname as the group (correct for a per-operator
    /// station grouping).
    pub backup_host: Option<String>,
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
/// * `Sequential` — stage → snapshot → delete each claim in turn (own snapshot,
///   peak disk = `max(claim)`), then a final commit-point snapshot carrying
///   `crs/` + `secrets/` + `manifest.json` written LAST. Every snapshot shares
///   the same `run-<id>` tag (== the monolithic tag); a run that dies before
///   the final snapshot leaves no manifest, so restore ignores the set.
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
    // Canonical PLURAL `secrets`: kubectl (the CLI's `KubectlExec`) accepts the
    // singular, but the in-cluster `KubeRsExec` resolves resources by exact
    // plural via API discovery, so `secret` fails there. `secrets` works for both.
    let args = vec!["get", "secrets", name, "-n", namespace, "-o", "json"];
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
        StagingMode::Sequential => run_backup_sequential_with_summary(k, r, opts),
    }
}

/// The CRs + secrets a run captures, plus the manifest resources. Written into
/// the "non-claim" staging dir by [`capture_non_claim_artifacts`] — that dir is
/// `<staging>/data` for `Monolithic` (colocated with the dumps) and the final
/// commit-point dir for `Sequential`.
struct NonClaimArtifacts {
    cr_count: usize,
    secret_count: usize,
}

/// Write the non-claim components (`crs/` serialized CRs + `secrets/` +
/// `manifest.json`) into `dest_dir`. Shared verbatim by both staging modes —
/// in `Monolithic` `dest_dir == <staging>/data`, in `Sequential` it is the
/// final commit-point dir written LAST. Returns the CR / secret counts.
fn capture_non_claim_artifacts(
    k: &dyn KubeExec,
    opts: &BackupOpts,
    claims: &[Value],
    dest_dir: &Path,
) -> Result<NonClaimArtifacts> {
    // CRs.
    let crs_dir = dest_dir.join("crs");
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

    // Secrets.
    let secrets_dir = dest_dir.join("secrets");
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

    // manifest.json (the commit point in `Sequential`).
    let manifest_crs: Vec<(&str, &Value)> =
        captured_crs.iter().map(|(k, v)| (k.as_str(), v)).collect();
    let manifest = BackupManifest {
        manifest_version: crate::manifest::MANIFEST_VERSION_CURRENT,
        cluster_id: opts.cluster_id.clone(),
        created_at: opts.created_at.clone(),
        platform_version: opts.platform_version.clone(),
        namespaces: opts.namespaces.clone(),
        resources: resource_refs(&manifest_crs, claims),
    };
    write_manifest(&manifest, dest_dir)?;

    Ok(NonClaimArtifacts {
        cr_count: captured_crs.len(),
        secret_count,
    })
}

/// The directory that must exist before `restic init` can create a repository
/// there — `None` for every backend that is not a local filesystem path.
///
/// A restic repository reference is NOT a path. `s3:https://host/bucket`,
/// `rest:`, `sftp:`, `b2:`, `gs:` and friends name a remote, and asking for the
/// "parent directory" of one yields a nonsense relative path built out of the
/// URL. Creating it is wrong in two different ways depending on who runs it:
///
///   * in the in-cluster runner, whose container is not root and whose cwd is
///     `/`, it fails with `Permission denied (os error 13)` and takes the whole
///     backup down with a message naming an S3 URL as a directory;
///   * in the CLI, where the cwd is writable, it SUCCEEDS — silently creating a
///     junk tree like `./s3:https:/minio.example.com:9000/backups` next to
///     whatever the operator happened to be standing in.
///
/// It stayed invisible because the branch only runs when the repository does
/// not exist yet, and `apprafter backup enable` initialises it host-side first.
/// It fires on a genuinely fresh repo — a new bucket, a wiped one, or a
/// `PlatformStack` edited by hand without the CLI preflight — i.e. on somebody's
/// FIRST scheduled backup. Found by `e2e/backup-s3-sequential-kind.sh` on the
/// first run after that walk started executing the runner it builds (D24).
fn repo_parent_to_create(repo: &str) -> Option<&Path> {
    // `local:` is restic's explicit spelling of a filesystem repo; a bare path
    // is the implicit one. Everything else carrying a `<scheme>:` is a remote.
    let path = repo.strip_prefix("local:").unwrap_or(repo);
    let looks_remote = path
        .split_once(':')
        .is_some_and(|(scheme, _)| {
            !scheme.is_empty()
                && scheme.starts_with(|c: char| c.is_ascii_alphabetic())
                && scheme
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '.' | '-'))
        });
    if looks_remote {
        return None;
    }
    Path::new(path)
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
}

/// Init the restic repo iff it does not already exist (probe = `snapshots`).
fn ensure_repo(r: &dyn ResticRunner, repo: &str, pass: &str) -> Result<()> {
    if !restic_repo_exists(r, repo, pass)? {
        if let Some(parent) = repo_parent_to_create(repo) {
            std::fs::create_dir_all(parent).map_err(|e| {
                CliError::Other(format!("create repo parent {}: {e}", parent.display()))
            })?;
        }
        r.run(&restic_init_argv(repo), pass)?;
    }
    Ok(())
}

/// The `run-<id>` tag every snapshot in a run shares. IS the monolithic tag
/// (`<cluster_id>-<created_at>[…-ns-…]`) — the run id and the restic tag are
/// the same value.
fn run_tag(opts: &BackupOpts) -> String {
    let subset_ns: Vec<String> = if opts.is_subset {
        opts.namespaces.clone()
    } else {
        vec![]
    };
    backup_tag(&opts.cluster_id, &opts.created_at, &subset_ns)
}

fn run_backup_monolithic_with_summary(
    k: &dyn KubeExec,
    r: &dyn ResticRunner,
    opts: &BackupOpts,
) -> Result<BackupSummary> {
    let data_dir = opts.staging_root.join("data");
    std::fs::create_dir_all(&data_dir)
        .map_err(|e| CliError::Other(format!("create staging data dir: {e}")))?;

    // 1. Native data extraction — the WHOLE plan into <staging>/data.
    let claims = claims_in_namespaces(k, &opts.namespaces)?;
    let plan = plan_extraction(&claims);
    run_extraction(k, &plan, &data_dir, &opts.pg_image)?;

    // 2-4. CRs + secrets + manifest, colocated in <staging>/data.
    let non_claim = capture_non_claim_artifacts(k, opts, &claims, &data_dir)?;

    // 5. restic — ONE snapshot over the whole staged tree.
    let repo = &opts.repo;
    let pass = &opts.passphrase;
    ensure_repo(r, repo, pass)?;

    let tag = run_tag(opts);
    let snapshot_id = r.run_backup(
        &restic_backup_argv(
            repo,
            &data_dir.to_string_lossy(),
            &tag,
            opts.backup_host.as_deref(),
        ),
        pass,
    )?;

    Ok(BackupSummary {
        snapshot_id,
        cr_count: non_claim.cr_count,
        secret_count: non_claim.secret_count,
        claim_count: claims.len(),
        extracted_count: plan.len(),
        tag,
    })
}

/// `Sequential` staging: peak disk = `max(claim)`, not `sum`.
///
/// For each data claim: stage ONLY that claim's artifact into a per-claim
/// staging dir → `restic backup --tag run-<id>` it (its own snapshot) → delete
/// the staged dir → next claim. Then the non-claim components (`crs/` +
/// `secrets/` + `manifest.json`) go into a FINAL snapshot written LAST — the
/// COMMIT POINT: a run that dies before it leaves no manifest, so restore
/// ignores the incomplete set (its orphan snapshots are reaped by the next
/// prune). Every snapshot in the run shares the SAME `run-<id>` tag.
fn run_backup_sequential_with_summary(
    k: &dyn KubeExec,
    r: &dyn ResticRunner,
    opts: &BackupOpts,
) -> Result<BackupSummary> {
    let repo = &opts.repo;
    let pass = &opts.passphrase;
    let tag = run_tag(opts);

    let claims = claims_in_namespaces(k, &opts.namespaces)?;
    let plan = plan_extraction(&claims);

    ensure_repo(r, repo, pass)?;

    let mut snapshot_id: Option<String> = None;

    // Per-claim: stage one → back it up (run-tagged) → delete → next.
    // Each claim becomes its own restic snapshot, so peak staged disk is a
    // single claim's dump/tar rather than the sum of them all.
    for (i, item) in plan.iter().enumerate() {
        let claim_dir = opts.staging_root.join(format!("claim-{i}"));
        std::fs::create_dir_all(&claim_dir)
            .map_err(|e| CliError::Other(format!("create per-claim staging dir {i}: {e}")))?;

        // Extract exactly THIS claim — reuses run_extraction (and thus the same
        // extract_pg / extract_volume) on a one-element slice, so the per-claim
        // path is byte-identical to the monolithic per-claim layout.
        run_extraction(k, std::slice::from_ref(item), &claim_dir, &opts.pg_image)?;

        snapshot_id = r.run_backup(
            &restic_backup_argv(
                repo,
                &claim_dir.to_string_lossy(),
                &tag,
                opts.backup_host.as_deref(),
            ),
            pass,
        )?;

        std::fs::remove_dir_all(&claim_dir)
            .map_err(|e| CliError::Other(format!("remove per-claim staging dir {i}: {e}")))?;
    }

    // FINAL snapshot = the commit point: crs/ + secrets/ + manifest.json,
    // written LAST. Same run-<id> tag. No manifest ⇒ restore ignores the run.
    let commit_dir = opts.staging_root.join("commit");
    std::fs::create_dir_all(&commit_dir)
        .map_err(|e| CliError::Other(format!("create commit staging dir: {e}")))?;
    let non_claim = capture_non_claim_artifacts(k, opts, &claims, &commit_dir)?;

    let manifest_snapshot_id = r.run_backup(
        &restic_backup_argv(
            repo,
            &commit_dir.to_string_lossy(),
            &tag,
            opts.backup_host.as_deref(),
        ),
        pass,
    )?;
    // The manifest snapshot is the run's representative id (its presence marks a
    // complete run); prefer it over the last per-claim id.
    snapshot_id = manifest_snapshot_id.or(snapshot_id);

    Ok(BackupSummary {
        snapshot_id,
        cr_count: non_claim.cr_count,
        secret_count: non_claim.secret_count,
        claim_count: claims.len(),
        extracted_count: plan.len(),
        tag,
    })
}

// ---------------------------------------------------------------------------
// Tests (pure engine helpers)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::path::Path;

    use serde_json::json;

    // -----------------------------------------------------------------------
    // repo_parent_to_create — a repository reference is not a path
    // -----------------------------------------------------------------------

    #[test]
    fn no_directory_is_created_for_a_remote_repository() {
        // The case that took a backup Job down: the runner is not root, its cwd
        // is `/`, and this used to resolve to a relative directory built out of
        // an S3 URL.
        for repo in [
            "s3:http://minio.minio-e2e.svc.cluster.local:9000/apprafter-backups/seq",
            "s3:https://fsn1.your-objectstorage.com/bucket/prefix",
            "rest:https://backup.example.com/repo",
            "sftp:user@host:/srv/restic",
            "b2:bucket:path",
            "gs:bucket:/prefix",
            "rclone:remote:path",
        ] {
            assert!(
                repo_parent_to_create(repo).is_none(),
                "a parent directory must never be created for the remote repo {repo}"
            );
        }
    }

    #[test]
    fn a_local_repository_still_gets_its_parent_created() {
        assert_eq!(
            repo_parent_to_create("/var/backups/apprafter/repo"),
            Some(Path::new("/var/backups/apprafter"))
        );
        // restic's explicit spelling of the same thing.
        assert_eq!(
            repo_parent_to_create("local:/var/backups/apprafter/repo"),
            Some(Path::new("/var/backups/apprafter"))
        );
        assert_eq!(
            repo_parent_to_create("relative/repo"),
            Some(Path::new("relative"))
        );
    }

    #[test]
    fn a_bare_repository_name_has_no_parent_to_create() {
        assert!(repo_parent_to_create("repo").is_none());
    }

    // -----------------------------------------------------------------------
    // In-crate test doubles — drive the engine end-to-end with NO cluster and
    // NO real restic, capturing the restic `backup` argv of every snapshot.
    // -----------------------------------------------------------------------

    /// Records every `run_backup` argv so a test can assert the snapshot count,
    /// the per-call `--tag`, and the staged path of each snapshot. `run` /
    /// `run_stdout` are no-ops. `run_stdout` returning `Ok` makes
    /// `restic_repo_exists` report the repo already exists, so the engine skips
    /// `init` (keeping the recorded calls to just the `backup` snapshots).
    #[derive(Default)]
    struct RecordingRestic {
        calls: RefCell<Vec<Vec<String>>>,
    }

    impl RecordingRestic {
        fn calls(&self) -> Vec<Vec<String>> {
            self.calls.borrow().clone()
        }
    }

    impl ResticRunner for RecordingRestic {
        fn run(&self, _argv: &[String], _passphrase: &str) -> Result<()> {
            Ok(())
        }
        fn run_stdout(&self, _argv: &[String], _passphrase: &str) -> Result<String> {
            // Ok(..) => restic_repo_exists() == true => engine skips `init`.
            Ok(String::new())
        }
        fn run_backup(&self, argv: &[String], _passphrase: &str) -> Result<Option<String>> {
            self.calls.borrow_mut().push(argv.to_vec());
            Ok(Some("snap".into()))
        }
    }

    /// Extract the value that follows `--tag` in a recorded restic argv.
    fn tag_of(argv: &[String]) -> Option<String> {
        argv.windows(2)
            .find(|w| w[0] == "--tag")
            .map(|w| w[1].clone())
    }

    /// The staged path a `restic backup` argv points at — the last positional
    /// (the argv is `backup --repo <r> --tag <t> --json <staging_dir>`).
    fn staged_path_of(argv: &[String]) -> Option<String> {
        argv.last().cloned()
    }

    /// A `KubeExec` that returns a fixed set of pg ResourceClaims for the
    /// claim-list probe and empty (`Ok(None)`) for every other list, so the
    /// engine's CR/secret sweep produces an empty-but-valid artifact. Its
    /// `exec_stream_to_file` CREATES the `out` file (a few bytes) so the
    /// sequential stage→backup→delete loop sees a real staged file.
    struct FakeKube {
        /// The pg claim JSONs returned for the `resourceclaims.apprafter.io` list.
        claims: Vec<Value>,
    }

    impl FakeKube {
        /// `n` fake, fully-provisioned pg claims in namespace `demo`.
        fn with_pg_claims(n: usize) -> Self {
            let claims = (0..n)
                .map(|i| {
                    json!({
                        "spec": { "type": "pg" },
                        "metadata": { "name": format!("pg-{i}"), "namespace": "demo" },
                        "status": { "connectionSecretRef": format!("pg-{i}-conn") }
                    })
                })
                .collect();
            Self { claims }
        }
    }

    impl KubeExec for FakeKube {
        fn apply_and_wait_pod_ready(&self, _spec: &Value) -> Result<()> {
            Ok(())
        }

        fn exec_stream_to_file(
            &self,
            _pod: &str,
            _ns: &str,
            _argv: &[&str],
            out: &Path,
        ) -> Result<()> {
            if let Some(parent) = out.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            // A real staged file so the stage→backup→delete loop has something
            // to delete and the backup argv points at a non-empty tree.
            std::fs::write(out, b"DUMP").unwrap();
            Ok(())
        }

        fn exec_stream_from_file(
            &self,
            _pod: &str,
            _ns: &str,
            _argv: &[&str],
            _input: &Path,
        ) -> Result<()> {
            Ok(())
        }

        fn delete_pod_best_effort(&self, _name: &str, _ns: &str) {}

        fn get_secret_key(&self, _secret: &str, _ns: &str, _key: &str) -> Result<String> {
            Ok("x".into())
        }

        fn get_json(&self, args: &[&str]) -> Result<Option<Value>> {
            // The only list the engine needs populated to exercise the
            // per-claim path is the ResourceClaim list; everything else
            // (platformstack, sourcecredentials, applications, argo apps,
            // sharedvolumes, sealedsecrets, secrets) reports empty so the CR /
            // secret sweep yields an empty-but-valid artifact.
            if args.iter().any(|a| a.starts_with("resourceclaims")) {
                Ok(Some(json!({ "items": self.claims })))
            } else {
                Ok(None)
            }
        }
    }

    fn opts_for(mode: StagingMode, staging_root: PathBuf) -> BackupOpts {
        BackupOpts {
            repo: "/tmp/does-not-matter-repo".into(),
            passphrase: "pw".into(),
            cluster_id: "k3d-demo".into(),
            created_at: "2026-06-20T00:00:00Z".into(),
            platform_version: "0.2.37".into(),
            namespaces: vec!["demo".into()],
            is_subset: false,
            staging_root,
            pg_image: "postgres:16-alpine".into(),
            staging_mode: mode,
            backup_host: None,
        }
    }

    #[test]
    fn monolithic_writes_exactly_one_snapshot() {
        let staging = tempfile::tempdir().unwrap();
        let k = FakeKube::with_pg_claims(2);
        let r = RecordingRestic::default();
        let opts = opts_for(StagingMode::Monolithic, staging.path().to_path_buf());

        let out = run_backup(&k, &r, &opts).expect("monolithic backup");
        assert_eq!(out, Some("snap".to_string()));

        let calls = r.calls();
        assert_eq!(
            calls.len(),
            1,
            "monolithic must write exactly ONE snapshot, got {calls:?}"
        );
        // The single snapshot is over <staging>/data.
        let staged = staged_path_of(&calls[0]).unwrap();
        let expected = staging.path().join("data");
        assert_eq!(staged, expected.to_string_lossy());
    }

    #[test]
    fn sequential_writes_a_snapshot_per_claim_plus_a_final_manifest_snapshot_all_run_tagged() {
        let staging = tempfile::tempdir().unwrap();
        let k = FakeKube::with_pg_claims(2);
        let r = RecordingRestic::default();
        let opts = opts_for(StagingMode::Sequential, staging.path().to_path_buf());

        run_backup(&k, &r, &opts).expect("sequential backup");

        let calls = r.calls();
        // 2 per-claim snapshots + 1 final manifest/commit snapshot.
        assert_eq!(
            calls.len(),
            3,
            "sequential(2 claims) must write 3 snapshots (2 claim + 1 manifest), got {calls:?}"
        );

        // Every snapshot shares the SAME run-id tag (== the monolithic tag).
        let run_id = backup_tag(&opts.cluster_id, &opts.created_at, &[]);
        for c in &calls {
            assert_eq!(
                tag_of(c).as_deref(),
                Some(run_id.as_str()),
                "every sequential snapshot must be run-tagged {run_id}: {c:?}"
            );
        }

        // The LAST snapshot is the manifest/commit-point snapshot: its staged
        // path holds manifest.json (and crs/ + secrets/), distinct from the
        // per-claim dump paths.
        let last = &calls[calls.len() - 1];
        let manifest_dir = PathBuf::from(staged_path_of(last).unwrap());
        assert!(
            manifest_dir.join("manifest.json").exists(),
            "final snapshot staged dir must hold manifest.json: {manifest_dir:?}"
        );
        assert!(
            manifest_dir.join("crs").exists() && manifest_dir.join("secrets").exists(),
            "final snapshot staged dir must hold crs/ + secrets/: {manifest_dir:?}"
        );

        // The per-claim snapshots point at distinct, non-manifest paths.
        for c in &calls[..calls.len() - 1] {
            let p = PathBuf::from(staged_path_of(c).unwrap());
            assert_ne!(
                p, manifest_dir,
                "claim snapshot path must differ from manifest snapshot"
            );
            assert!(
                !p.join("manifest.json").exists(),
                "a per-claim snapshot must NOT carry manifest.json: {p:?}"
            );
            // Peak-disk = max(claim): the per-claim staging dir is deleted after
            // its backup, before the next claim is staged (not merely emptied).
            assert!(
                !p.exists(),
                "per-claim staging dir must be deleted after backup (peak disk = max claim): {p:?}"
            );
        }
    }

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
