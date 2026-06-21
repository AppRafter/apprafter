// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! `apprafter export` / `apprafter backup` — 2.6d backup/restore command
//! logic (Task 9).
//!
//! Two kinds of data pull, sharing the same native-extraction engine
//! (`cli_providers::backup`):
//!
//! * **`export`** (Kind 1) — pull native data (pg dumps, volume tars, redis
//!   snapshots) to a plain local folder + a `manifest.json`. No CRs, no
//!   secrets, no encryption. A debugging / one-off-recovery convenience.
//!
//! * **`backup`** (Kind 2) — the same extraction PLUS the serialized config
//!   and app CRs, PLUS the decrypted user secrets, all staged and then
//!   wrapped into an encrypted `restic` repository. This is the
//!   disaster-recovery artifact `restore` consumes.
//!
//! ## Default scope = WHOLE CLUSTER
//!
//! Both commands default to every namespace that hosts an AppRafter
//! `Application` — the *app-namespace set*, derived from
//! `kubectl get applications.apprafter.io -A`, NOT `kubectl get ns` (the
//! latter would sweep in platform/system namespaces we must never replay).
//! `--namespace <ns>` (repeatable) / `--select` narrows the set.
//!
//! ## User vs platform discrimination (H1 — load-bearing)
//!
//! A restore must NOT clobber the bootstrap's own platform objects, so the
//! backup captures user material only:
//!
//! * **Argo `Application`s** are filtered to those carrying the
//!   `apprafter.io/managed-by=apprafter` label ([`is_user_argo_app`]). The
//!   platform umbrella + component Argo Applications LACK it → never
//!   serialized (else restore double-owns them against bootstrap).
//! * **Config CRs** are captured by KIND (`PlatformStack/default`,
//!   `SourceCredential` cluster-wide), not by an namespace sweep. There is no
//!   in-cluster `Infrastructure` CR (M2: it is the local manifest) — its
//!   topology rides `manifest.platform_version`, and a missing
//!   `infrastructures.apprafter.io` listing is expected, never an error.
//!
//! ## SourceCredential material — follow-the-reference (rev-5)
//!
//! `SourceCredential` CRs and their sealed material live in
//! `apprafter-system`, OUTSIDE the app-namespace set, so the app-ns secret
//! sweep MISSES them. Instead, for each `SourceCredential` we resolve its
//! `spec.git.backend.sealedSecretRef` + `spec.registry.backend.sealedSecretRef`
//! ([`sourcecred_material_refs`]) and read the underlying
//! controller-unsealed Secret directly. Two distinct secret-capture paths:
//! (a) app user secrets — SealedSecret-backed sweep scoped to the app-ns set;
//! (b) SourceCredential material — follow-the-reference, cluster-wide.

use std::collections::BTreeMap;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::process::Command;

use base64::Engine as _;
use cli_core::{CliError, Result};
use cli_providers::backup::extract::{plan_extraction, run_extraction};
use cli_providers::backup::helper_pod::{
    apply_and_wait_pod_ready, delete_pod_best_effort, exec_stream_from_file, pg_dump_pod_spec,
    volume_pod_spec,
};
use cli_providers::backup::images::{pg_helper_image, VOLUME_IMAGE};
use cli_providers::backup::manifest::BackupManifest;
use cli_providers::backup::reseal::reseal_secret;
use cli_providers::backup::restic::{
    restic_backup_argv, restic_init_argv, restic_restore_argv, restic_snapshots_argv,
};
use cli_providers::backup::restore::{restore_steps, zero_replicas, RestoreMode, RestoreStep};
use cli_providers::backup::sanitize::sanitize_cr;
use cli_providers::backup::ResourceRef;
use cli_providers::k8s::kubectl::KubectlCli;
use cli_providers::k8s::sealing::fetch_controller_public_key;
use serde_json::Value;

use crate::commands::k8s_helpers::{
    ensure_kubeconfig_tempfile, ensure_kubeconfig_tempfile_for_target, kubectl_apply_server_side,
    kubectl_get_json, kubectl_get_json_cluster_wide, kubectl_merge_patch,
};
use crate::commands::state_paths::resolve_state_paths;

/// The `apprafter.io/managed-by=apprafter` label that marks a user-owned Argo
/// `Application`. Mirrors `app::APPRAFTER_MANAGED_LABEL` (kept in sync as a
/// literal — `app`'s const is private to that module). The `key=value` form
/// is convenient for `kubectl -l`; here we split it for a JSON label lookup.
const APPRAFTER_MANAGED_LABEL: &str = "apprafter.io/managed-by=apprafter";

/// Namespace the `PlatformStack` singleton + `SourceCredential`s + their sealed
/// material live in. Mirrors `repo_creds::SOURCECRED_NAMESPACE` /
/// `platform::PLATFORMSTACK_NAMESPACE` (both private to their modules).
const APPRAFTER_SYSTEM_NAMESPACE: &str = "apprafter-system";
const PLATFORMSTACK_NAME: &str = "default";

/// Field manager for every restore-time apply. A dedicated manager keeps the
/// restored objects' ownership distinct from the bootstrap loader
/// (`apprafter-cli`) and the operator (`apprafter-operator`), so a later Argo
/// self-heal / operator reconcile can cooperate on the fields it owns.
const RESTORE_FIELD_MANAGER: &str = "apprafter-restore";

/// Retry budget for the restored-PlatformStack apply — mirrors
/// `cluster_bootstrap::PLATFORMSTACK_APPLY_ATTEMPTS`. The
/// `platformstacks.apprafter.io` ValidatingWebhook's backing pod may briefly
/// lack Endpoints, so the first apply can race with `no endpoints available
/// for service "admission-webhook"`. 30 × 10s = 5 min.
const PLATFORMSTACK_APPLY_ATTEMPTS: u32 = 30;
const PLATFORMSTACK_APPLY_BACKOFF_SECS: u64 = 10;

/// Poll budget for `WaitClaimsBound`: wait for every regenerated ResourceClaim
/// to report `status.ready == true` (NOT PVC Bound — R1: 2.6b marks disk ready
/// on `volumeClaimRef` set, and the `LoadData` helper is the first PVC
/// consumer, so waiting for Bound would deadlock). 60 × 10s = 10 min.
const CLAIM_READY_ATTEMPTS: u32 = 60;
const CLAIM_READY_BACKOFF_SECS: u64 = 10;

// ---------------------------------------------------------------------------
// Pure helpers (the tested core)
// ---------------------------------------------------------------------------

/// Distinct, sorted namespaces of the AppRafter `Application` CRs. When
/// `select` is non-empty the result is intersected with it (the operator
/// asked for a subset).
///
/// `apprafter_apps` is the `.items[]` array of
/// `kubectl get applications.apprafter.io -A -o json`. The app-namespace set
/// derives from THESE, never from `kubectl get ns` — that distinction is the
/// whole point of the H1 review (platform/system namespaces must never enter
/// the backup scope).
pub fn app_namespaces(apprafter_apps: &[Value], select: &[String]) -> Vec<String> {
    let mut set: Vec<String> = apprafter_apps
        .iter()
        .filter_map(|a| {
            a.pointer("/metadata/namespace")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .collect();
    set.sort();
    set.dedup();
    if select.is_empty() {
        set
    } else {
        set.into_iter().filter(|ns| select.contains(ns)).collect()
    }
}

/// True iff this Argo `Application` JSON carries the
/// `apprafter.io/managed-by: apprafter` label. The platform umbrella and the
/// per-component Argo Applications lack it; only user apps registered via
/// `apprafter app add` set it. Filtering on it keeps the backup from
/// double-owning the bootstrap's own Applications on restore.
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

/// Of the Secret names present in `ns`, keep only those that have a
/// matching SealedSecret (same name, same namespace). A SealedSecret-backed
/// Secret is a user secret we sealed; a plain Secret with no SealedSecret is
/// a system/derived artifact (e.g. connection Secrets, dockerconfig) that the
/// operator re-creates on restore — we don't carry it.
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

/// Resolve the backup passphrase: explicit `--passphrase` → `RESTIC_PASSWORD`
/// env → (on a TTY) an interactive masked prompt. The repository holds
/// DECRYPTED secrets, so an empty / absent passphrase is NEVER allowed: when
/// neither source is set and we're not on a TTY, this errors instead of
/// silently producing an unencrypted-by-empty-key repo.
pub fn backup_passphrase_or_error(
    arg: Option<&str>,
    env: Option<&str>,
    is_tty: bool,
) -> Result<String> {
    if let Some(p) = arg.or(env) {
        if p.is_empty() {
            return Err(CliError::Other(
                "empty backup passphrase — the repository holds decrypted secrets and must be \
                 encrypted; pass a non-empty `--passphrase` or set RESTIC_PASSWORD"
                    .into(),
            ));
        }
        return Ok(p.to_string());
    }
    if !is_tty {
        return Err(CliError::Other(
            "no backup passphrase — pass `--passphrase <value>`, set RESTIC_PASSWORD, or run from \
             an interactive shell for a prompt (the repo holds decrypted secrets and must be \
             encrypted)"
                .into(),
        ));
    }
    let pass = inquire::Password::new("Backup passphrase:")
        .with_display_mode(inquire::PasswordDisplayMode::Masked)
        .with_help_message("encrypts the restic repository; you'll need it to restore")
        .prompt()
        .map_err(|e| CliError::Other(format!("passphrase prompt: {e}")))?;
    if pass.is_empty() {
        return Err(CliError::Other(
            "passphrase cannot be empty — the repo holds decrypted secrets".into(),
        ));
    }
    Ok(pass)
}

/// The restic snapshot tag: `<cluster_id>-<created_at>`, plus a
/// `-ns-<joined>` marker when the backup is a namespace subset. Identifies the
/// SOURCE CLUSTER + WHEN, NOT a single namespace (a whole-cluster backup spans
/// many namespaces, so a namespace-based tag would be wrong / ambiguous).
pub fn backup_tag(cluster_id: &str, created_at: &str, subset_namespaces: &[String]) -> String {
    let base = format!("{cluster_id}-{created_at}");
    if subset_namespaces.is_empty() {
        base
    } else {
        format!("{base}-ns-{}", subset_namespaces.join("_"))
    }
}

/// `(namespace, name)` of each sealed material Secret a `SourceCredential`
/// references via `spec.git.backend.sealedSecretRef` +
/// `spec.registry.backend.sealedSecretRef`. The `namespace` field of each ref
/// is optional and DEFAULTS to the SourceCredential's own namespace
/// (`apprafter-system`) — matching the operator's resolution
/// (`operator-core::sourcecredential::SealedSecretRef`). The launch default
/// points both refs at the same material Secret, so the result may contain
/// duplicates; the caller dedups.
pub fn sourcecred_material_refs(sc: &Value) -> Vec<(String, String)> {
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

// ---------------------------------------------------------------------------
// Impure helpers (walk-validated)
// ---------------------------------------------------------------------------

/// `.items[]` of `kubectl get <resource> [-n ns | -A] -o json`, or an empty
/// `Vec` when the resource lists nothing / its CRD is absent (e.g.
/// `infrastructures.apprafter.io`, which legitimately has no instances — M2).
fn list_items(resource: &str, namespace: Option<&str>, kubeconfig: &Path) -> Result<Vec<Value>> {
    match kubectl_get_json_cluster_wide(resource, namespace, kubeconfig) {
        Ok(Some(v)) => Ok(v
            .get("items")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()),
        Ok(None) => Ok(Vec::new()),
        Err(e) => {
            // A missing CRD (no `infrastructures` kind) is not a backup failure.
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

/// Read the full `.data` of a Secret (all keys), base64-decoding each value,
/// as a `name → bytes` map. Returns `Ok(None)` when the Secret is absent.
fn read_secret_data(
    name: &str,
    namespace: &str,
    kubeconfig: &Path,
) -> Result<Option<BTreeMap<String, Vec<u8>>>> {
    let json = kubectl_get_json("secret", Some(name), Some(namespace), kubeconfig)?;
    let Some(json) = json else { return Ok(None) };
    let mut out = BTreeMap::new();
    if let Some(data) = json.pointer("/data").and_then(Value::as_object) {
        for (k, v) in data {
            let b64 = v.as_str().unwrap_or("");
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(b64)
                .map_err(|e| {
                    CliError::Other(format!("decode secret {namespace}/{name} key {k}: {e}"))
                })?;
            out.insert(k.clone(), bytes);
        }
    }
    Ok(Some(out))
}

/// Read `PlatformStack/default.status.currentVersion` (the live platform-stack
/// version) so `restore --reprovision` bootstraps the target at the same
/// version. Falls back to `"unknown"` when the field is unset (a freshly
/// bootstrapped cluster whose operator hasn't stamped status yet).
fn read_platform_version(kubeconfig: &Path) -> Result<String> {
    let ps = kubectl_get_json(
        "platformstack",
        Some(PLATFORMSTACK_NAME),
        Some(APPRAFTER_SYSTEM_NAMESPACE),
        kubeconfig,
    )?;
    Ok(ps
        .as_ref()
        .and_then(|p| p.pointer("/status/currentVersion"))
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_string())
}

/// Build the `ResourceRef`s recorded in `manifest.json` from the captured
/// config CRs + app CRs + claims. Pure-ish (operates on already-fetched
/// JSON), kept private since it just shapes the manifest body.
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

/// The CNPG `Cluster.spec.imageName` of the first CNPG Cluster found in the
/// app namespaces, used to pick a major-matched `pg_dump` helper image. Falls
/// back to the default pg image when none is found.
fn first_cnpg_image(namespaces: &[String], kubeconfig: &Path) -> Option<String> {
    for ns in namespaces {
        if let Ok(items) = list_items("clusters.postgresql.cnpg.io", Some(ns), kubeconfig) {
            if let Some(img) = items
                .iter()
                .find_map(|c| c.pointer("/spec/imageName").and_then(Value::as_str))
            {
                return Some(img.to_string());
            }
        }
    }
    None
}

/// Enumerate ResourceClaims across the given namespaces (cluster-wide when the
/// set is the whole cluster), flattened into a single `Vec`.
fn claims_in_namespaces(namespaces: &[String], kubeconfig: &Path) -> Result<Vec<Value>> {
    let mut all = Vec::new();
    for ns in namespaces {
        all.extend(list_items(
            "resourceclaims.apprafter.io",
            Some(ns),
            kubeconfig,
        )?);
    }
    Ok(all)
}

/// Resolve the default output directory for `export`: `<cwd>/apprafter-export`.
fn default_export_dir() -> PathBuf {
    std::env::current_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join("apprafter-export")
}

/// Default restic repo path for a target: `<config>/backups/<target>`.
fn default_backup_repo(target_name: &str) -> Result<PathBuf> {
    let root = cli_core::target::default_config_root()?;
    Ok(root.join("backups").join(target_name))
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

/// `apprafter export` — pull native data (Kind 1) to a plain local folder.
///
/// Scope: the app-namespace set (whole cluster by default), narrowed by
/// `namespaces` when `select` is set. Writes `<out>/{pg,volumes,redis}/…`
/// plus a `<out>/manifest.json`. No CRs, no secrets, no encryption.
pub fn run_export(namespaces: &[String], select: bool, out: Option<&str>) -> Result<()> {
    let resolved = resolve_state_paths(None)?;
    let cluster_id = resolved.target_name.clone();
    let kc = ensure_kubeconfig_tempfile()?;

    let subset: &[String] = if select { namespaces } else { &[] };
    let apps = list_items("applications.apprafter.io", None, kc.path())?;
    let ns_set = app_namespaces(&apps, subset);
    if ns_set.is_empty() {
        return Err(CliError::Other(
            "no AppRafter Applications found — nothing to export. (Scope derives from \
             `kubectl get applications.apprafter.io -A`.)"
                .into(),
        ));
    }

    let out_dir = match out {
        Some(p) => PathBuf::from(p),
        None => default_export_dir(),
    };
    std::fs::create_dir_all(&out_dir)
        .map_err(|e| CliError::Other(format!("create export dir {}: {e}", out_dir.display())))?;

    let claims = claims_in_namespaces(&ns_set, kc.path())?;
    let plan = plan_extraction(&claims);
    let pg_image = pg_helper_image(first_cnpg_image(&ns_set, kc.path()).as_deref());
    run_extraction(&plan, &out_dir, kc.path(), &pg_image)?;

    let platform_version = read_platform_version(kc.path())?;
    let manifest = BackupManifest {
        cluster_id: cluster_id.clone(),
        created_at: now_rfc3339(),
        platform_version,
        namespaces: ns_set.clone(),
        resources: resource_refs(&[], &claims),
    };
    write_manifest(&manifest, &out_dir)?;

    println!(
        "✓ Exported {} namespace(s) from cluster '{cluster_id}' → {}",
        ns_set.len(),
        out_dir.display()
    );
    println!("  namespaces: {}", ns_set.join(", "));
    println!(
        "  claims:     {} ({} extractable)",
        claims.len(),
        plan.len()
    );
    Ok(())
}

/// `apprafter backup` — full encrypted backup (Kind 2): native extraction +
/// serialized config/app CRs + decrypted user secrets, wrapped into a restic
/// repository.
pub fn run_backup(
    namespaces: &[String],
    select: bool,
    repo: Option<&str>,
    passphrase: Option<&str>,
) -> Result<()> {
    let resolved = resolve_state_paths(None)?;
    let cluster_id = resolved.target_name.clone();

    let env_pass = std::env::var("RESTIC_PASSWORD").ok();
    let is_tty = std::io::stdin().is_terminal();
    let pass = backup_passphrase_or_error(passphrase, env_pass.as_deref(), is_tty)?;

    // Keep the kubeconfig tempfile alive for the WHOLE sequence (every kubectl
    // shell-out below depends on it; dropping it deletes the file).
    let kc = ensure_kubeconfig_tempfile()?;

    let subset: &[String] = if select { namespaces } else { &[] };
    let apps = list_items("applications.apprafter.io", None, kc.path())?;
    let ns_set = app_namespaces(&apps, subset);
    if ns_set.is_empty() {
        return Err(CliError::Other(
            "no AppRafter Applications found — nothing to back up. (Scope derives from \
             `kubectl get applications.apprafter.io -A`.)"
                .into(),
        ));
    }

    // Stage everything under a tempdir; restic snapshots `data/`.
    let staging = tempfile::Builder::new()
        .prefix("apprafter-backup-")
        .tempdir()
        .map_err(|e| CliError::Other(format!("create staging dir: {e}")))?;
    let data_dir = staging.path().join("data");
    std::fs::create_dir_all(&data_dir)
        .map_err(|e| CliError::Other(format!("create staging data dir: {e}")))?;

    // 1. Native data extraction.
    let claims = claims_in_namespaces(&ns_set, kc.path())?;
    let plan = plan_extraction(&claims);
    let pg_image = pg_helper_image(first_cnpg_image(&ns_set, kc.path()).as_deref());
    run_extraction(&plan, &data_dir, kc.path(), &pg_image)?;

    // 2. Serialize CRs (sanitized) into data/crs/.
    let crs_dir = data_dir.join("crs");
    std::fs::create_dir_all(&crs_dir)
        .map_err(|e| CliError::Other(format!("create crs dir: {e}")))?;

    let mut captured_crs: Vec<(String, Value)> = Vec::new();

    // 2a. PlatformStack/default (config CR, by kind).
    if let Some(ps) = kubectl_get_json(
        "platformstack",
        Some(PLATFORMSTACK_NAME),
        Some(APPRAFTER_SYSTEM_NAMESPACE),
        kc.path(),
    )? {
        captured_crs.push(("PlatformStack".to_string(), ps));
    }

    // 2b. SourceCredential (config CRs, cluster-wide by kind).
    let source_creds = list_items("sourcecredentials.apprafter.io", None, kc.path())?;
    for sc in &source_creds {
        captured_crs.push(("SourceCredential".to_string(), sc.clone()));
    }

    // 2c. AppRafter Application CRs (app-ns set).
    let app_crs: Vec<Value> = apps
        .iter()
        .filter(|a| {
            a.pointer("/metadata/namespace")
                .and_then(Value::as_str)
                .map(|ns| ns_set.iter().any(|n| n == ns))
                .unwrap_or(false)
        })
        .cloned()
        .collect();
    for a in &app_crs {
        captured_crs.push(("Application".to_string(), a.clone()));
    }

    // 2d. USER Argo Applications (managed-by filter). These live in the
    //     `argocd` namespace (outside the app-ns set), so they are listed
    //     cluster-wide and filtered by label — the platform umbrella +
    //     component Argo Apps lack the label and are never captured.
    let argo_apps = list_items("applications.argoproj.io", None, kc.path())?;
    let user_argo: Vec<Value> = argo_apps
        .iter()
        .filter(|a| is_user_argo_app(a))
        .cloned()
        .collect();
    for a in &user_argo {
        captured_crs.push(("ArgoApplication".to_string(), a.clone()));
    }

    // 2e. SharedVolume CRs (app-ns set).
    let mut shared_volumes = Vec::new();
    for ns in &ns_set {
        shared_volumes.extend(list_items(
            "sharedvolumes.apprafter.io",
            Some(ns),
            kc.path(),
        )?);
    }
    for sv in &shared_volumes {
        captured_crs.push(("SharedVolume".to_string(), sv.clone()));
    }

    write_crs(&captured_crs, &crs_dir)?;

    // 3. Secrets — TWO distinct paths.
    let secrets_dir = data_dir.join("secrets");
    std::fs::create_dir_all(&secrets_dir)
        .map_err(|e| CliError::Other(format!("create secrets dir: {e}")))?;

    // 3a. App user secrets — SealedSecret-backed sweep, app-ns scoped.
    let mut secret_count = 0usize;
    for ns in &ns_set {
        let sealed = list_items("sealedsecrets.bitnami.com", Some(ns), kc.path())?;
        let secret_names: Vec<String> = list_items("secrets", Some(ns), kc.path())?
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
            if let Some(data) = read_secret_data(name, ns, kc.path())? {
                write_secret_json(&ns_dir, name, &data)?;
                secret_count += 1;
            }
        }
    }

    // 3b. SourceCredential material — follow-the-reference, cluster-wide
    //     (the app-ns sweep above MISSES it: it lives in apprafter-system).
    let sourcecred_dir = secrets_dir.join("sourcecred");
    let mut sc_refs: Vec<(String, String)> = source_creds
        .iter()
        .flat_map(sourcecred_material_refs)
        .collect();
    sc_refs.sort();
    sc_refs.dedup();
    for (ns, name) in &sc_refs {
        if let Some(data) = read_secret_data(name, ns, kc.path())? {
            write_secret_json(&sourcecred_dir, name, &data)?;
            secret_count += 1;
        }
    }

    // 4. manifest.json.
    let manifest_crs: Vec<(&str, &Value)> =
        captured_crs.iter().map(|(k, v)| (k.as_str(), v)).collect();
    let manifest = BackupManifest {
        cluster_id: cluster_id.clone(),
        created_at: now_rfc3339(),
        platform_version: read_platform_version(kc.path())?,
        namespaces: ns_set.clone(),
        resources: resource_refs(&manifest_crs, &claims),
    };
    write_manifest(&manifest, &data_dir)?;

    // 5. restic init (only if the repo doesn't already exist) + backup.
    let repo_path = match repo {
        Some(r) => PathBuf::from(r),
        None => default_backup_repo(&cluster_id)?,
    };
    let repo_str = repo_path.to_string_lossy().to_string();

    if !restic_repo_exists(&repo_str, &pass)? {
        if let Some(parent) = repo_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                CliError::Other(format!("create repo parent {}: {e}", parent.display()))
            })?;
        }
        run_restic(&restic_init_argv(&repo_str), &pass)?;
    }

    let tag = backup_tag(&cluster_id, &manifest.created_at, subset);
    let snapshot_id = run_restic_backup(
        &restic_backup_argv(&repo_str, &data_dir.to_string_lossy(), &tag),
        &pass,
    )?;

    println!("✓ Backed up cluster '{cluster_id}' → {repo_str}");
    println!("  namespaces: {}", ns_set.join(", "));
    println!(
        "  captured:   {} CR(s), {} secret(s), {} claim(s) ({} extracted)",
        captured_crs.len(),
        secret_count,
        claims.len(),
        plan.len()
    );
    println!("  tag:        {tag}");
    if let Some(id) = snapshot_id {
        println!("  snapshot:   {id}");
    }
    Ok(())
}

/// `apprafter backup list` — list the snapshots in a restic repo.
pub fn run_backup_list(repo: Option<&str>, passphrase: Option<&str>) -> Result<()> {
    let resolved = resolve_state_paths(None)?;
    let env_pass = std::env::var("RESTIC_PASSWORD").ok();
    let is_tty = std::io::stdin().is_terminal();
    let pass = backup_passphrase_or_error(passphrase, env_pass.as_deref(), is_tty)?;

    let repo_path = match repo {
        Some(r) => PathBuf::from(r),
        None => default_backup_repo(&resolved.target_name)?,
    };
    let repo_str = repo_path.to_string_lossy().to_string();

    let json = run_restic_stdout(&restic_snapshots_argv(&repo_str), &pass)?;
    let snapshots: Value = serde_json::from_str(&json)
        .map_err(|e| CliError::Other(format!("parse restic snapshots JSON: {e}")))?;

    let arr = snapshots.as_array().cloned().unwrap_or_default();
    if arr.is_empty() {
        println!("No snapshots in {repo_str}.");
        return Ok(());
    }

    println!("Snapshots in {repo_str}:");
    println!("{:<12}  {:<25}  TAGS", "ID", "TIME");
    for s in &arr {
        let id = s
            .pointer("/short_id")
            .or_else(|| s.pointer("/id"))
            .and_then(Value::as_str)
            .map(|i| i.chars().take(8).collect::<String>())
            .unwrap_or_else(|| "?".to_string());
        let time = s.pointer("/time").and_then(Value::as_str).unwrap_or("?");
        let tags = s
            .pointer("/tags")
            .and_then(Value::as_array)
            .map(|t| {
                t.iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default();
        println!("{id:<12}  {time:<25}  {tags}");
    }
    Ok(())
}

/// `apprafter restore` — replay a backup into a RUNNING, bootstrapped target
/// cluster (modes a-into-running / b). Drives the pure step list from
/// [`cli_providers::backup::restore::restore_steps`].
///
/// The flow (`--data-only == false`):
/// `RestoreArtifact` (restic → tempdir) → `ApplyPlatformStack` → `ApplySourceCredentials`
/// → `ApplyAppsGated` (H2: claims provision, NO pod — [`zero_replicas`] + Argo
/// auto-sync stripped) → `WaitClaimsBound` (poll `status.ready`, NOT PVC Bound
/// — R1) → `LoadData` (pg uses the FRESH connection Secret — L3) →
/// `ReSealUserSecrets` → `ResumeWorkloads` (replicas + auto-sync back).
///
/// `--data-only == true`: `RestoreArtifact` → `SuspendWorkloads` (scale the
/// existing app to 0 + disable its Argo auto-sync) → `LoadData` →
/// `ResumeWorkloads`. No CR/secret replay.
///
/// `--reprovision` (fresh cluster in the target first) lands in 2.6d T13.
pub fn run_restore(
    repo: &str,
    target: Option<&str>,
    reprovision: bool,
    snapshot: Option<&str>,
    data_only: bool,
    passphrase: Option<&str>,
) -> Result<()> {
    if reprovision {
        return Err(CliError::Other(
            "restore --reprovision (fresh-cluster re-provision then replay) lands in 2.6d T13; \
             for now restore into an already-bootstrapped target (drop --reprovision)"
                .into(),
        ));
    }

    // 1. Passphrase (mandatory — the repo holds decrypted secrets) + TARGET
    //    kubeconfig. Hold the kubeconfig tempfile alive for the WHOLE flow:
    //    every kubectl/restic-adjacent shell-out below depends on it.
    let env_pass = std::env::var("RESTIC_PASSWORD").ok();
    let is_tty = std::io::stdin().is_terminal();
    let pass = backup_passphrase_or_error(passphrase, env_pass.as_deref(), is_tty)?;
    let kc = ensure_kubeconfig_tempfile_for_target(target)?;

    let mode = RestoreMode::IntoRunning;
    let steps = restore_steps(mode, data_only);

    // Held across the whole restore: the restic restore unpacks the decrypted
    // secrets here and it must NOT be cleaned up until every step that reads
    // `data/` is done (rev-5: tempfile::tempdir auto-cleanup on drop).
    let restore_root = tempfile::Builder::new()
        .prefix("apprafter-restore-")
        .tempdir()
        .map_err(|e| CliError::Other(format!("create restore tempdir: {e}")))?;

    // Lazily-populated after RestoreArtifact: the `data/` dir under the restic
    // restore target, plus the parsed manifest.
    let mut data_dir: Option<PathBuf> = None;
    let mut manifest: Option<BackupManifest> = None;
    // Recorded original replica counts per AppRafter Application
    // (`(namespace, name) → replicas`) for ResumeWorkloads (H2).
    let mut app_replicas: Vec<((String, String), i64)> = Vec::new();
    // The logical names of user Argo Applications whose auto-sync we stripped
    // (ApplyAppsGated / SuspendWorkloads) and must re-enable in ResumeWorkloads.
    let mut suspended_argo: Vec<(String, String)> = Vec::new();
    let mut version_warning: Option<String> = None;

    let snap = snapshot.unwrap_or("latest");

    for step in &steps {
        match step {
            RestoreStep::Reprovision => {
                // Unreachable for IntoRunning (guarded above); T13 owns this.
                return Err(CliError::Other(
                    "restore reprovision step requires --reprovision (2.6d T13)".into(),
                ));
            }
            RestoreStep::RestoreArtifact => {
                run_restic(
                    &restic_restore_argv(repo, snap, &restore_root.path().to_string_lossy()),
                    &pass,
                )?;
                let dd = find_data_dir(restore_root.path())?;
                let m = read_backup_manifest(&dd)?;
                // M1 version note: warn (don't fail) when the target's live
                // PlatformStack version differs from the backup's — a
                // cross-version restore may re-render components.
                let target_version = read_platform_version(kc.path())?;
                if !data_only && target_version != "unknown" && target_version != m.platform_version
                {
                    version_warning = Some(format!(
                        "backup is from platform-stack {} but the target runs {} — \
                         a cross-version restore may re-render components; verify after restore",
                        m.platform_version, target_version
                    ));
                }
                data_dir = Some(dd);
                manifest = Some(m);
            }
            RestoreStep::ApplyPlatformStack => {
                let dd = data_dir
                    .as_ref()
                    .ok_or_else(|| CliError::Other("ApplyPlatformStack before artifact".into()))?;
                apply_platformstack_from_crs(dd, kc.path())?;
            }
            RestoreStep::ApplySourceCredentials => {
                let dd = data_dir.as_ref().ok_or_else(|| {
                    CliError::Other("ApplySourceCredentials before artifact".into())
                })?;
                apply_source_credentials(dd, kc.path())?;
            }
            RestoreStep::ApplyAppsGated => {
                let dd = data_dir
                    .as_ref()
                    .ok_or_else(|| CliError::Other("ApplyAppsGated before artifact".into()))?;
                app_replicas = apply_apps_gated(dd, kc.path(), &mut suspended_argo)?;
            }
            RestoreStep::WaitClaimsBound => {
                let m = manifest
                    .as_ref()
                    .ok_or_else(|| CliError::Other("WaitClaimsBound before artifact".into()))?;
                wait_claims_bound(m, kc.path())?;
            }
            RestoreStep::LoadData => {
                let dd = data_dir
                    .as_ref()
                    .ok_or_else(|| CliError::Other("LoadData before artifact".into()))?;
                let m = manifest
                    .as_ref()
                    .ok_or_else(|| CliError::Other("LoadData before artifact".into()))?;
                load_data(dd, m, kc.path())?;
            }
            RestoreStep::ReSealUserSecrets => {
                let dd = data_dir
                    .as_ref()
                    .ok_or_else(|| CliError::Other("ReSealUserSecrets before artifact".into()))?;
                reseal_user_secrets(dd, kc.path())?;
            }
            RestoreStep::SuspendWorkloads => {
                // --data-only: scale the running app(s) to 0 + disable Argo
                // auto-sync so the load doesn't race a live pod. We derive the
                // target apps from the backed-up artifact's data/ layout
                // (the namespaces/claims that have data to load).
                let m = manifest
                    .as_ref()
                    .ok_or_else(|| CliError::Other("SuspendWorkloads before artifact".into()))?;
                app_replicas = suspend_running_workloads(m, kc.path(), &mut suspended_argo)?;
            }
            RestoreStep::ResumeWorkloads => {
                resume_workloads(&app_replicas, &suspended_argo, kc.path())?;
            }
        }
    }

    // Summary.
    let m = manifest.as_ref();
    println!(
        "✓ Restored backup{} into target '{}'",
        m.map(|m| format!(" of cluster '{}'", m.cluster_id))
            .unwrap_or_default(),
        target.unwrap_or("<active>")
    );
    if let Some(m) = m {
        println!("  namespaces: {}", m.namespaces.join(", "));
        println!(
            "  mode:       {}",
            if data_only { "data-only" } else { "full" }
        );
    }
    println!("  workloads:  {} app(s) resumed", app_replicas.len());
    if let Some(w) = &version_warning {
        println!("  ⚠ {w}");
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Restore step implementations (impure — walk-validated)
// ---------------------------------------------------------------------------

/// A captured CR read back off disk during restore.
struct LoadedCr {
    /// The backup's kind tag (`PlatformStack` / `SourceCredential` /
    /// `Application` / `ArgoApplication` / `SharedVolume`).
    kind: String,
    cr: Value,
}

/// Locate the `data/` directory inside the restic restore target.
///
/// `restic restore --target <out>` recreates the snapshot's ABSOLUTE source
/// path under `<out>` (the backup snapshotted `<staging>/data`), so the
/// artifacts land at `<out>/<staging-path>/data/…`, not directly under
/// `<out>/data`. We find the directory by locating the unique `manifest.json`
/// (written at the root of `data/`) and returning its parent.
fn find_data_dir(restore_root: &Path) -> Result<PathBuf> {
    fn search(dir: &Path) -> Option<PathBuf> {
        let entries = std::fs::read_dir(dir).ok()?;
        let mut subdirs = Vec::new();
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                subdirs.push(p);
            } else if p.file_name().and_then(|n| n.to_str()) == Some("manifest.json") {
                return Some(dir.to_path_buf());
            }
        }
        for sd in subdirs {
            if let Some(found) = search(&sd) {
                return Some(found);
            }
        }
        None
    }
    search(restore_root).ok_or_else(|| {
        CliError::Other(format!(
            "restored artifact has no manifest.json under {} — is this an AppRafter backup repo?",
            restore_root.display()
        ))
    })
}

/// Parse `data/manifest.json`.
fn read_backup_manifest(data_dir: &Path) -> Result<BackupManifest> {
    let path = data_dir.join("manifest.json");
    let body = std::fs::read(&path)
        .map_err(|e| CliError::Other(format!("read {}: {e}", path.display())))?;
    serde_json::from_slice(&body).map_err(|e| CliError::Other(format!("parse manifest.json: {e}")))
}

/// Read every CR JSON file under `data/crs/`, returning `(kind, value)` pairs.
/// File names are `<idx>-<Kind>-<ns>-<name>.json` (see `write_crs`); the kind
/// is read from the file BODY's `kind` field is unreliable for the backup's
/// internal `ArgoApplication` tag (the on-disk CR has `kind: Application`), so
/// we recover the backup's kind tag from the FILENAME segment instead.
fn read_crs(data_dir: &Path) -> Result<Vec<LoadedCr>> {
    let crs_dir = data_dir.join("crs");
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(&crs_dir) else {
        // No crs/ dir (data-only export shape) — nothing to replay.
        return Ok(out);
    };
    let mut files: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("json"))
        .collect();
    files.sort();
    for path in files {
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or_default();
        // `<idx>-<Kind>-<ns>-<name>` — the kind tag is the 2nd dash segment.
        let kind = stem.split('-').nth(1).unwrap_or_default().to_string();
        let body = std::fs::read(&path)
            .map_err(|e| CliError::Other(format!("read CR {}: {e}", path.display())))?;
        let cr: Value = serde_json::from_slice(&body)
            .map_err(|e| CliError::Other(format!("parse CR {}: {e}", path.display())))?;
        out.push(LoadedCr { kind, cr });
    }
    Ok(out)
}

/// Read one backed-up secret JSON (`{ key: base64-value }`) as a decoded
/// `key → bytes` map, plus the secret type. The backup writes only `.data`
/// (no type), so the type defaults to `Opaque` — the launch user-secrets +
/// SourceCredential material are all `Opaque`.
fn read_secret_file(path: &Path) -> Result<(BTreeMap<String, Vec<u8>>, String)> {
    let body = std::fs::read(path)
        .map_err(|e| CliError::Other(format!("read secret {}: {e}", path.display())))?;
    let encoded: BTreeMap<String, String> = serde_json::from_slice(&body)
        .map_err(|e| CliError::Other(format!("parse secret {}: {e}", path.display())))?;
    let mut data = BTreeMap::new();
    for (k, b64) in encoded {
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(&b64)
            .map_err(|e| {
                CliError::Other(format!("decode secret {} key {k}: {e}", path.display()))
            })?;
        data.insert(k, bytes);
    }
    Ok((data, "Opaque".to_string()))
}

/// Apply a CR `Value` via server-side apply with the restore field manager.
/// JSON is valid YAML, so we serialize and pipe it on stdin.
fn apply_cr(cr: &Value, kubeconfig: &Path) -> Result<()> {
    let yaml = serde_json::to_string(cr)
        .map_err(|e| CliError::Other(format!("serialize CR for apply: {e}")))?;
    kubectl_apply_server_side(&yaml, RESTORE_FIELD_MANAGER, kubeconfig)
}

/// **ApplyPlatformStack** — apply the sanitized `PlatformStack` from `crs/`,
/// mirroring the `cluster_bootstrap` retry loop for the admission-webhook
/// Endpoints race.
fn apply_platformstack_from_crs(data_dir: &Path, kubeconfig: &Path) -> Result<()> {
    let crs = read_crs(data_dir)?;
    let Some(ps) = crs.iter().find(|c| c.kind == "PlatformStack") else {
        // A backup without a PlatformStack (older shape) — nothing to apply;
        // the target's own bootstrap PlatformStack stays in place.
        println!("  (no PlatformStack in backup — keeping target's own)");
        return Ok(());
    };
    let yaml = serde_json::to_string(&ps.cr)
        .map_err(|e| CliError::Other(format!("serialize PlatformStack: {e}")))?;
    let mut attempt = 0u32;
    loop {
        attempt += 1;
        match kubectl_apply_server_side(&yaml, RESTORE_FIELD_MANAGER, kubeconfig) {
            Ok(()) => break,
            Err(e) if attempt < PLATFORMSTACK_APPLY_ATTEMPTS => {
                eprintln!(
                    "info: PlatformStack apply failed (admission webhook likely not ready yet); \
                     retrying (attempt {attempt}): {e}"
                );
                std::thread::sleep(std::time::Duration::from_secs(
                    PLATFORMSTACK_APPLY_BACKOFF_SECS,
                ));
            }
            Err(e) => return Err(e),
        }
    }
    println!("  ✓ PlatformStack applied");
    Ok(())
}

/// **ApplySourceCredentials** — apply each `SourceCredential` CR, then re-seal
/// its material from `secrets/sourcecred/<name>.json` for the TARGET cluster
/// and apply the SealedSecret. Runs BEFORE apps so a config-repo / registry
/// reference is resolvable by the time the app reconciles.
fn apply_source_credentials(data_dir: &Path, kubeconfig: &Path) -> Result<()> {
    let crs = read_crs(data_dir)?;
    let scs: Vec<&LoadedCr> = crs
        .iter()
        .filter(|c| c.kind == "SourceCredential")
        .collect();
    if scs.is_empty() {
        return Ok(());
    }

    let kubectl = KubectlCli;
    let pub_key = fetch_controller_public_key(&kubectl, kubeconfig)?;
    let sourcecred_dir = data_dir.join("secrets").join("sourcecred");

    for sc in &scs {
        apply_cr(&sc.cr, kubeconfig)?;

        // Re-seal each material Secret this SourceCredential references
        // (follow-the-reference, same as the backup's capture path).
        let mut refs = sourcecred_material_refs(&sc.cr);
        refs.sort();
        refs.dedup();
        for (ns, name) in refs {
            let path = sourcecred_dir.join(format!("{name}.json"));
            if !path.exists() {
                // Material was never captured (a SourceCredential whose ref
                // pointed outside the captured set) — skip, the CR apply
                // already surfaced the reference.
                continue;
            }
            let (data, secret_type) = read_secret_file(&path)?;
            let sealed = reseal_secret(&pub_key, &ns, &name, &secret_type, &data)?;
            apply_cr(&sealed, kubeconfig)?;
        }
    }
    println!("  ✓ {} SourceCredential(s) + material re-sealed", scs.len());
    Ok(())
}

/// **ApplyAppsGated** (H2) — apply user Argo `Application` CRs with
/// `syncPolicy.automated` STRIPPED (so Argo won't re-render / overwrite the
/// gated AppRafter Application), AppRafter `Application` CRs with
/// [`zero_replicas`] applied (claims provision, NO pod), and `SharedVolume`
/// CRs. Records the original replica count per AppRafter Application
/// (`(ns, name) → replicas`) for `ResumeWorkloads`, and the logical name of
/// each gated user Argo Application (`(ns, name)`) whose auto-sync to re-enable.
///
/// Returns the recorded `((ns, name), replicas)` list.
#[allow(clippy::type_complexity)]
fn apply_apps_gated(
    data_dir: &Path,
    kubeconfig: &Path,
    suspended_argo: &mut Vec<(String, String)>,
) -> Result<Vec<((String, String), i64)>> {
    let crs = read_crs(data_dir)?;
    let mut recorded = Vec::new();

    // SharedVolumes first (an app's disk.ref needs the SharedVolume to exist).
    for sv in crs.iter().filter(|c| c.kind == "SharedVolume") {
        apply_cr(&sv.cr, kubeconfig)?;
    }

    // AppRafter Applications — gated to replicas=0.
    for app in crs.iter().filter(|c| c.kind == "Application") {
        let ns = app
            .cr
            .pointer("/metadata/namespace")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let name = app
            .cr
            .pointer("/metadata/name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        // Record the ORIGINAL base replicas (the count to restore on resume).
        // Default 1 when the field is absent (the operator's own default).
        let replicas = app
            .cr
            .pointer("/spec/base/replicas")
            .and_then(Value::as_i64)
            .unwrap_or(1);
        recorded.push(((ns, name), replicas));

        let gated = zero_replicas(&app.cr);
        apply_cr(&gated, kubeconfig)?;
    }

    // User Argo Applications — strip auto-sync so Argo holds off re-rendering.
    for argo in crs.iter().filter(|c| c.kind == "ArgoApplication") {
        let ns = argo
            .cr
            .pointer("/metadata/namespace")
            .and_then(Value::as_str)
            .unwrap_or("argocd")
            .to_string();
        let name = argo
            .cr
            .pointer("/metadata/name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let stripped = strip_argo_automated(&argo.cr);
        apply_cr(&stripped, kubeconfig)?;
        suspended_argo.push((ns, name));
    }

    println!(
        "  ✓ {} app(s) applied gated (replicas=0, Argo auto-sync stripped)",
        recorded.len()
    );
    Ok(recorded)
}

/// Strip `spec.syncPolicy.automated` from an Argo `Application` CR so Argo CD
/// will not auto-sync (and thus re-render the gated AppRafter Application) until
/// `ResumeWorkloads` re-enables it. Returns a fresh Value.
fn strip_argo_automated(argo: &Value) -> Value {
    let mut out = argo.clone();
    if let Some(sp) = out
        .pointer_mut("/spec/syncPolicy")
        .and_then(Value::as_object_mut)
    {
        sp.remove("automated");
    }
    out
}

/// **WaitClaimsBound** — poll each ResourceClaim recorded in the manifest until
/// `status.ready == true`. R1: do NOT wait for PVC Bound (the `LoadData` volume
/// helper is the first PVC consumer; waiting for Bound would deadlock).
fn wait_claims_bound(manifest: &BackupManifest, kubeconfig: &Path) -> Result<()> {
    let claims: Vec<&ResourceRef> = manifest
        .resources
        .iter()
        .filter(|r| r.kind == "ResourceClaim")
        .collect();
    if claims.is_empty() {
        return Ok(());
    }
    for claim in &claims {
        let mut attempt = 0u32;
        loop {
            attempt += 1;
            let json = kubectl_get_json(
                "resourceclaims.apprafter.io",
                Some(&claim.name),
                Some(&claim.namespace),
                kubeconfig,
            )?;
            let ready = json
                .as_ref()
                .and_then(|j| j.pointer("/status/ready"))
                .and_then(Value::as_bool)
                .unwrap_or(false);
            if ready {
                break;
            }
            if attempt >= CLAIM_READY_ATTEMPTS {
                return Err(CliError::Other(format!(
                    "ResourceClaim {}/{} did not become ready within {}s",
                    claim.namespace,
                    claim.name,
                    CLAIM_READY_ATTEMPTS as u64 * CLAIM_READY_BACKOFF_SECS
                )));
            }
            std::thread::sleep(std::time::Duration::from_secs(CLAIM_READY_BACKOFF_SECS));
        }
    }
    println!("  ✓ {} claim(s) ready", claims.len());
    Ok(())
}

/// **LoadData** — inject each native artifact under `data/{pg,volumes,redis}/`
/// into its freshly-provisioned backend.
fn load_data(data_dir: &Path, manifest: &BackupManifest, kubeconfig: &Path) -> Result<()> {
    // pg: data/pg/<ns>/<claim>.dump
    load_pg_dumps(data_dir, kubeconfig)?;
    // volumes: data/volumes/<ns>/<name>/data.tar
    load_volumes(data_dir, manifest, kubeconfig)?;
    // redis: documented skeleton (T12 verifies Dragonfly RDB).
    load_redis_skeleton(data_dir);
    Ok(())
}

/// Restore every `data/pg/<ns>/<claim>.dump` via `pg_restore` over a helper
/// pod, using the FRESH connection Secret (L3 — the post-provision creds, NOT
/// the backed-up ones).
fn load_pg_dumps(data_dir: &Path, kubeconfig: &Path) -> Result<()> {
    let pg_root = data_dir.join("pg");
    let Ok(ns_entries) = std::fs::read_dir(&pg_root) else {
        return Ok(());
    };
    for ns_entry in ns_entries.flatten() {
        let ns_path = ns_entry.path();
        if !ns_path.is_dir() {
            continue;
        }
        let ns = ns_entry.file_name().to_string_lossy().into_owned();
        let Ok(dump_entries) = std::fs::read_dir(&ns_path) else {
            continue;
        };
        for dump_entry in dump_entries.flatten() {
            let dump_path = dump_entry.path();
            if dump_path.extension().and_then(|s| s.to_str()) != Some("dump") {
                continue;
            }
            let claim = dump_path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or_default()
                .to_string();
            load_one_pg(&ns, &claim, &dump_path, kubeconfig)?;
        }
    }
    Ok(())
}

/// Load one pg dump into a freshly-provisioned claim.
///
/// L3: resolve the claim's CURRENT `status.connectionSecretRef` and read
/// user/pass/host/port/db from THAT (the post-provision Secret), never the
/// creds embedded in the backup. L2: `pg_restore` reads the dump on stdin via
/// `exec_stream_from_file`; `PGPASSWORD` is injected into the helper pod env.
fn load_one_pg(ns: &str, claim: &str, dump_path: &Path, kubeconfig: &Path) -> Result<()> {
    // Resolve the FRESH connection Secret name from the regenerated claim.
    let claim_json = kubectl_get_json(
        "resourceclaims.apprafter.io",
        Some(claim),
        Some(ns),
        kubeconfig,
    )?
    .ok_or_else(|| {
        CliError::Other(format!(
            "claim {ns}/{claim} not found at LoadData — was it gated/applied?"
        ))
    })?;
    let secret_name = claim_json
        .pointer("/status/connectionSecretRef")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            CliError::Other(format!(
                "claim {ns}/{claim} has no status.connectionSecretRef (not provisioned)"
            ))
        })?;

    let secret = read_secret_data(secret_name, ns, kubeconfig)?.ok_or_else(|| {
        CliError::Other(format!(
            "connection Secret {ns}/{secret_name} for claim {claim} not found"
        ))
    })?;
    let get = |k: &str| -> Result<String> {
        secret
            .get(k)
            .map(|v| String::from_utf8_lossy(v).into_owned())
            .ok_or_else(|| {
                CliError::Other(format!(
                    "connection Secret {ns}/{secret_name} missing key `{k}`"
                ))
            })
    };
    let user = get("user")?;
    let pass = get("pass")?;
    let host = get("host")?;
    let port = get("port")?;
    let db = get("db")?;

    // Helper pod (network pod — pg_dump image carries pg_restore). Inject
    // PGPASSWORD into the env so pg_restore never prompts.
    let pod_name = truncate_pod_name(&format!("ld-pg-{claim}"));
    let mut spec = pg_dump_pod_spec(&pod_name, ns, images_default_pg());
    if let Some(container) = spec
        .pointer_mut("/spec/containers/0")
        .and_then(Value::as_object_mut)
    {
        container.insert(
            "env".to_string(),
            serde_json::json!([{ "name": "PGPASSWORD", "value": pass }]),
        );
    }

    let _guard = PodCleanupGuard {
        name: pod_name.clone(),
        namespace: ns.to_string(),
        kubeconfig,
    };
    apply_and_wait_pod_ready(&spec, kubeconfig)?;

    let argv: Vec<&str> = vec![
        "pg_restore",
        "--no-owner",
        "--clean",
        "--if-exists",
        "-h",
        &host,
        "-p",
        &port,
        "-U",
        &user,
        "-d",
        &db,
    ];
    exec_stream_from_file(&pod_name, ns, &argv, dump_path, kubeconfig)?;
    println!("  ✓ pg restored: {ns}/{claim}");
    Ok(())
}

/// Restore every `data/volumes/<ns>/<name>/data.tar` into its fresh PVC via a
/// busybox helper pod mounted READ-WRITE (L1). The PVC to mount is the claim's
/// regenerated `status.volumeClaimRef` (or, for a SharedVolume, the SV's bound
/// PVC) — resolved from the live claim/SharedVolume by name.
fn load_volumes(data_dir: &Path, manifest: &BackupManifest, kubeconfig: &Path) -> Result<()> {
    let vol_root = data_dir.join("volumes");
    let Ok(ns_entries) = std::fs::read_dir(&vol_root) else {
        return Ok(());
    };
    for ns_entry in ns_entries.flatten() {
        let ns_path = ns_entry.path();
        if !ns_path.is_dir() {
            continue;
        }
        let ns = ns_entry.file_name().to_string_lossy().into_owned();
        let Ok(name_entries) = std::fs::read_dir(&ns_path) else {
            continue;
        };
        for name_entry in name_entries.flatten() {
            let dir = name_entry.path();
            if !dir.is_dir() {
                continue;
            }
            let tar_path = dir.join("data.tar");
            if !tar_path.exists() {
                continue;
            }
            let name = name_entry.file_name().to_string_lossy().into_owned();
            let pvc = resolve_volume_pvc(&ns, &name, manifest, kubeconfig)?;
            load_one_volume(&ns, &name, &pvc, &tar_path, kubeconfig)?;
        }
    }
    Ok(())
}

/// Resolve the fresh PVC name a volume artifact should be loaded into. The
/// backup keys volume artifacts by CLAIM name (`data/volumes/<ns>/<claim>/`),
/// so we read the regenerated claim's `status.volumeClaimRef`.
fn resolve_volume_pvc(
    ns: &str,
    claim: &str,
    _manifest: &BackupManifest,
    kubeconfig: &Path,
) -> Result<String> {
    let claim_json = kubectl_get_json(
        "resourceclaims.apprafter.io",
        Some(claim),
        Some(ns),
        kubeconfig,
    )?
    .ok_or_else(|| {
        CliError::Other(format!(
            "claim {ns}/{claim} not found at volume LoadData — was it gated/applied?"
        ))
    })?;
    claim_json
        .pointer("/status/volumeClaimRef")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| {
            CliError::Other(format!(
                "claim {ns}/{claim} has no status.volumeClaimRef (not provisioned)"
            ))
        })
}

/// Load one volume tar into its PVC (L1: read-write mount).
fn load_one_volume(
    ns: &str,
    name: &str,
    pvc: &str,
    tar_path: &Path,
    kubeconfig: &Path,
) -> Result<()> {
    let pod_name = truncate_pod_name(&format!("ld-vol-{name}"));
    let spec = volume_pod_spec(&pod_name, ns, VOLUME_IMAGE, pvc, false); // L1: RW
    let _guard = PodCleanupGuard {
        name: pod_name.clone(),
        namespace: ns.to_string(),
        kubeconfig,
    };
    apply_and_wait_pod_ready(&spec, kubeconfig)?;
    let argv: Vec<&str> = vec!["tar", "x", "-C", "/data"];
    exec_stream_from_file(&pod_name, ns, &argv, tar_path, kubeconfig)?;
    println!("  ✓ volume restored: {ns}/{name} → pvc {pvc}");
    Ok(())
}

/// Redis restore skeleton — the Dragonfly RDB injection path (snapshot PVC +
/// whether a `DEBUG RELOAD` is needed) is verified on the live walk in T12.
/// For now, log a note per persisted redis artifact so the operator knows it
/// was not loaded.
fn load_redis_skeleton(data_dir: &Path) {
    let redis_root = data_dir.join("redis");
    if let Ok(ns_entries) = std::fs::read_dir(&redis_root) {
        for ns_entry in ns_entries.flatten() {
            let ns = ns_entry.file_name().to_string_lossy().into_owned();
            eprintln!(
                "info: redis restore for namespace {ns} deferred to T12 (Dragonfly RDB) — skipping"
            );
        }
    }
}

/// **ReSealUserSecrets** — re-seal each app user secret under
/// `secrets/<ns>/<name>.json` (NOT `secrets/sourcecred/…`, which
/// `ApplySourceCredentials` already handled) for the TARGET cluster and apply
/// the resulting SealedSecret.
fn reseal_user_secrets(data_dir: &Path, kubeconfig: &Path) -> Result<()> {
    let secrets_dir = data_dir.join("secrets");
    let Ok(ns_entries) = std::fs::read_dir(&secrets_dir) else {
        return Ok(());
    };

    let kubectl = KubectlCli;
    let mut pub_key = None;
    let mut count = 0usize;

    for ns_entry in ns_entries.flatten() {
        let ns_path = ns_entry.path();
        let ns = ns_entry.file_name().to_string_lossy().into_owned();
        // Skip the sourcecred subtree (handled in ApplySourceCredentials) and
        // any non-directory entries.
        if !ns_path.is_dir() || ns == "sourcecred" {
            continue;
        }
        let Ok(secret_entries) = std::fs::read_dir(&ns_path) else {
            continue;
        };
        for secret_entry in secret_entries.flatten() {
            let path = secret_entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("json") {
                continue;
            }
            let name = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or_default()
                .to_string();
            // Fetch the target pubkey lazily (only if there's at least one).
            if pub_key.is_none() {
                pub_key = Some(fetch_controller_public_key(&kubectl, kubeconfig)?);
            }
            let key = pub_key.as_ref().expect("pubkey fetched above");
            let (data, secret_type) = read_secret_file(&path)?;
            let sealed = reseal_secret(key, &ns, &name, &secret_type, &data)?;
            apply_cr(&sealed, kubeconfig)?;
            count += 1;
        }
    }
    if count > 0 {
        println!("  ✓ {count} user secret(s) re-sealed");
    }
    Ok(())
}

/// **ResumeWorkloads** — patch each AppRafter `Application`'s replicas back to
/// the recorded value (merge-patch on `spec.base.replicas`) and re-enable Argo
/// `syncPolicy.automated` on the user Argo Applications we stripped (H2). After
/// this the workloads come up on already-loaded data.
fn resume_workloads(
    app_replicas: &[((String, String), i64)],
    suspended_argo: &[(String, String)],
    kubeconfig: &Path,
) -> Result<()> {
    for ((ns, name), replicas) in app_replicas {
        let body = format!(r#"{{"spec":{{"base":{{"replicas":{replicas}}}}}}}"#);
        kubectl_merge_patch(
            "applications.apprafter.io",
            name,
            Some(ns),
            None,
            &body,
            kubeconfig,
        )?;
    }
    // Re-enable Argo auto-sync (prune + selfHeal — the platform default).
    for (ns, name) in suspended_argo {
        let body =
            r#"{"spec":{"syncPolicy":{"automated":{"prune":true,"selfHeal":true}}}}"#.to_string();
        kubectl_merge_patch(
            "applications.argoproj.io",
            name,
            Some(ns),
            None,
            &body,
            kubeconfig,
        )?;
    }
    Ok(())
}

/// **SuspendWorkloads** (`--data-only`) — for each AppRafter Application that
/// owns a backed-up data artifact, read its CURRENT replica count, disable its
/// Argo auto-sync, and scale it to 0 so the load doesn't race a running pod.
/// Returns the recorded `((ns, name), replicas)` list for `ResumeWorkloads`.
///
/// The set of apps to suspend is derived from the claims recorded in the
/// manifest (those whose data we are about to load): we suspend the Application
/// that lives in the same namespace as each claim. (In practice the data-only
/// flow targets a single app's claim; suspending its namespace's Application is
/// the conservative, correct move.)
#[allow(clippy::type_complexity)]
fn suspend_running_workloads(
    manifest: &BackupManifest,
    kubeconfig: &Path,
    suspended_argo: &mut Vec<(String, String)>,
) -> Result<Vec<((String, String), i64)>> {
    // Distinct Application names in the namespaces that have a claim.
    let claim_namespaces: Vec<&str> = manifest
        .resources
        .iter()
        .filter(|r| r.kind == "ResourceClaim")
        .map(|r| r.namespace.as_str())
        .collect();

    let mut recorded: Vec<((String, String), i64)> = Vec::new();
    for ns in claim_namespaces {
        let apps = list_items("applications.apprafter.io", Some(ns), kubeconfig)?;
        for app in &apps {
            let name = app
                .pointer("/metadata/name")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            if name.is_empty() || recorded.iter().any(|((n, a), _)| n == ns && a == &name) {
                continue;
            }
            let replicas = app
                .pointer("/spec/base/replicas")
                .and_then(Value::as_i64)
                .unwrap_or(1);
            recorded.push(((ns.to_string(), name.clone()), replicas));

            // Disable Argo auto-sync for this app's Argo Application(s) so the
            // scale-to-0 isn't reverted, then scale to 0.
            for (argo_ns, argo_name) in argo_apps_for(&name, kubeconfig)? {
                let body = r#"{"spec":{"syncPolicy":{"automated":null}}}"#.to_string();
                kubectl_merge_patch(
                    "applications.argoproj.io",
                    &argo_name,
                    Some(&argo_ns),
                    None,
                    &body,
                    kubeconfig,
                )?;
                suspended_argo.push((argo_ns, argo_name));
            }
            let body = r#"{"spec":{"base":{"replicas":0}}}"#.to_string();
            kubectl_merge_patch(
                "applications.apprafter.io",
                &name,
                Some(ns),
                None,
                &body,
                kubeconfig,
            )?;
        }
    }
    if !recorded.is_empty() {
        println!("  ✓ {} app(s) suspended for data-only load", recorded.len());
    }
    Ok(recorded)
}

/// Find the user Argo Application(s) that manage an AppRafter Application by
/// the `apprafter.io/application=<name>` label. Returns `(namespace, name)`
/// pairs. Used by the data-only suspend path.
fn argo_apps_for(app_name: &str, kubeconfig: &Path) -> Result<Vec<(String, String)>> {
    let items = crate::commands::k8s_helpers::kubectl_get_json_by_selector(
        "applications.argoproj.io",
        &format!("apprafter.io/application={app_name}"),
        None,
        kubeconfig,
    )?;
    Ok(items
        .iter()
        .filter_map(|a| {
            let ns = a
                .pointer("/metadata/namespace")
                .and_then(Value::as_str)?
                .to_string();
            let name = a
                .pointer("/metadata/name")
                .and_then(Value::as_str)?
                .to_string();
            Some((ns, name))
        })
        .collect())
}

/// The default pg helper image for restore (pg_restore runs in the same image
/// family as pg_dump). Restore can't always see the source CNPG image, so it
/// uses the pinned default; a pg-major mismatch surfaces as a `pg_restore`
/// error on the live walk (T12).
fn images_default_pg() -> &'static str {
    cli_providers::backup::images::DEFAULT_PG_IMAGE
}

/// Deletes a helper pod on drop — guarantees cleanup on every return path of a
/// LoadData step (mirrors `extract::HelperPodGuard`).
struct PodCleanupGuard<'a> {
    name: String,
    namespace: String,
    kubeconfig: &'a Path,
}

impl Drop for PodCleanupGuard<'_> {
    fn drop(&mut self) {
        delete_pod_best_effort(&self.name, &self.namespace, self.kubeconfig);
    }
}

/// Truncate a pod name to the 63-char DNS-1123 label limit, stripping any
/// trailing `-` left by truncation (mirrors `extract::truncate_pod_name`).
fn truncate_pod_name(name: &str) -> String {
    let mut s = name.to_string();
    s.truncate(63);
    while s.ends_with('-') {
        s.pop();
    }
    s
}

// ---------------------------------------------------------------------------
// restic runners + file writers
// ---------------------------------------------------------------------------

/// Probe whether a restic repository already exists at `repo`. `restic init`
/// errors on an existing repo, so we run `snapshots` (which succeeds on any
/// initialized repo, even an empty one) and treat success as "exists".
fn restic_repo_exists(repo: &str, pass: &str) -> Result<bool> {
    let out = Command::new("restic")
        .args(restic_snapshots_argv(repo))
        .env("RESTIC_PASSWORD", pass)
        .output()
        .map_err(|e| CliError::Other(format!("spawn restic snapshots (probe): {e}")))?;
    Ok(out.status.success())
}

/// Run a restic command, capturing output — silent on success, surfaces stderr on failure.
fn run_restic(argv: &[String], pass: &str) -> Result<()> {
    let out = Command::new("restic")
        .args(argv)
        .env("RESTIC_PASSWORD", pass)
        .output()
        .map_err(|e| CliError::Other(format!("spawn restic: {e}")))?;
    if !out.status.success() {
        return Err(CliError::Other(format!(
            "restic {} failed (exit {:?}): {}",
            argv.first().map(String::as_str).unwrap_or("?"),
            out.status.code(),
            String::from_utf8_lossy(&out.stderr)
        )));
    }
    Ok(())
}

/// Run a restic command and return its stdout verbatim. Used for
/// `snapshots --json` (the caller parses the JSON).
fn run_restic_stdout(argv: &[String], pass: &str) -> Result<String> {
    let out = Command::new("restic")
        .args(argv)
        .env("RESTIC_PASSWORD", pass)
        .output()
        .map_err(|e| CliError::Other(format!("spawn restic: {e}")))?;
    if !out.status.success() {
        return Err(CliError::Other(format!(
            "restic {} failed (exit {:?}): {}",
            argv.first().map(String::as_str).unwrap_or("?"),
            out.status.code(),
            String::from_utf8_lossy(&out.stderr)
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Run `restic backup --json` and return the snapshot id from the structured
/// summary line. `restic backup --json` emits one JSON object per line; the
/// final summary object has `"message_type": "summary"` and carries
/// `"snapshot_id"`. Returns `None` when the summary object is not found (a
/// restic version difference) — the backup still succeeded, we just can't echo
/// the id.
fn run_restic_backup(argv: &[String], pass: &str) -> Result<Option<String>> {
    let stdout = run_restic_stdout(argv, pass)?;
    let snapshot_id = stdout.lines().find_map(|line| {
        let obj: Value = serde_json::from_str(line.trim()).ok()?;
        if obj.pointer("/message_type").and_then(Value::as_str) == Some("summary") {
            obj.pointer("/snapshot_id")
                .and_then(Value::as_str)
                .map(str::to_string)
        } else {
            None
        }
    });
    Ok(snapshot_id)
}

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
        // Prefix with an index so two same-named CRs of different kinds/ns
        // never collide on disk.
        let file = format!("{i:03}-{kind}-{ns}-{name}.json");
        std::fs::write(crs_dir.join(&file), body)
            .map_err(|e| CliError::Other(format!("write CR {file}: {e}")))?;
    }
    Ok(())
}

/// Write one secret's decrypted `key → bytes` map as a JSON object of
/// base64-encoded values under `<dir>/<name>.json`. (Re-encoding to base64
/// keeps non-UTF-8 secret bytes representable in JSON; restore decodes back.)
fn write_secret_json(dir: &Path, name: &str, data: &BTreeMap<String, Vec<u8>>) -> Result<()> {
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
    let body = serde_json::to_vec_pretty(&encoded)
        .map_err(|e| CliError::Other(format!("serialize secret {name}: {e}")))?;
    std::fs::write(dir.join(format!("{name}.json")), body)
        .map_err(|e| CliError::Other(format!("write secret {name}.json: {e}")))
}

/// Current time as an RFC3339 string (manifest `created_at` + tag timestamp).
fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339()
}

// ---------------------------------------------------------------------------
// Tests (pure helpers — the tested core)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn app_namespaces_derive_from_apprafter_applications_not_all_ns() {
        let apps = vec![
            json!({"metadata":{"name":"alpha","namespace":"demo"}}),
            json!({"metadata":{"name":"beta","namespace":"demo"}}),
            json!({"metadata":{"name":"shop","namespace":"prod"}}),
        ];
        assert_eq!(app_namespaces(&apps, &[]), vec!["demo", "prod"]);
        assert_eq!(app_namespaces(&apps, &["prod".to_string()]), vec!["prod"]);
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
    fn backup_requires_passphrase() {
        assert!(backup_passphrase_or_error(None, None, false).is_err());
        assert!(backup_passphrase_or_error(Some("p"), None, false).is_ok());
    }

    #[test]
    fn backup_tag_is_cluster_id_and_timestamp_not_namespace() {
        let t = backup_tag("k3d-demo", "2026-06-20T00:00:00Z", &[]);
        assert!(t.contains("k3d-demo"));
        assert!(t.contains("2026-06-20"));
        let sub = backup_tag("k3d-demo", "2026-06-20T00:00:00Z", &["prod".to_string()]);
        assert!(sub.contains("prod"));
    }

    #[test]
    fn sourcecred_material_refs_follow_git_and_registry() {
        // a SourceCredential CR with both git + registry sealedSecretRefs
        let sc = json!({"metadata":{"name":"ghcr","namespace":"apprafter-system"},
            "spec":{"git":{"backend":{"sealedSecretRef":{"name":"ghcr-git"}}},
                    "registry":{"backend":{"sealedSecretRef":{"name":"ghcr-reg"}}}}});
        let refs = sourcecred_material_refs(&sc);
        // each ref defaults ns to the CR's own namespace (apprafter-system)
        assert!(refs
            .iter()
            .any(|(ns, n)| ns == "apprafter-system" && n == "ghcr-git"));
        assert!(refs
            .iter()
            .any(|(ns, n)| ns == "apprafter-system" && n == "ghcr-reg"));
    }
}
