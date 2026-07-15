// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! `apprafter export` / `apprafter backup` — 2.6d export + backup command
//! logic.
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
//!   disaster-recovery artifact [`crate::commands::restore::run_restore`]
//!   consumes.
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
use cli_providers::backup::images::pg_helper_image;
use cli_providers::backup::manifest::BackupManifest;
use cli_providers::backup::restic::{restic_backup_argv, restic_init_argv, restic_snapshots_argv};
use cli_providers::backup::sanitize::sanitize_cr;
use cli_providers::backup::ResourceRef;
use serde_json::Value;

use crate::commands::k8s_helpers::{
    ensure_kubeconfig_tempfile, kubectl_get_json, kubectl_get_json_cluster_wide,
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
/// Exported `pub(crate)` so `restore.rs` can use it without re-declaring.
pub(crate) const APPRAFTER_SYSTEM_NAMESPACE: &str = "apprafter-system";
pub(crate) const PLATFORMSTACK_NAME: &str = "default";

// ---------------------------------------------------------------------------
// Pure helpers (the tested core — some exported pub(crate) for restore.rs)
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
/// Exported `pub(crate)` for use by `restore.rs`.
pub(crate) fn list_items(
    resource: &str,
    namespace: Option<&str>,
    kubeconfig: &Path,
) -> Result<Vec<Value>> {
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
/// as a `name → bytes` map, plus the secret's `.type` field (defaulting to
/// `"Opaque"` when absent). Returns `Ok(None)` when the Secret is absent.
///
/// Exported `pub(crate)` for use by `restore.rs` (which needs raw connection
/// creds for the pg load path — it ignores the returned type for that use).
#[allow(clippy::type_complexity)]
pub(crate) fn read_secret_data(
    name: &str,
    namespace: &str,
    kubeconfig: &Path,
) -> Result<Option<(BTreeMap<String, Vec<u8>>, String)>> {
    let json = kubectl_get_json("secret", Some(name), Some(namespace), kubeconfig)?;
    let Some(json) = json else { return Ok(None) };
    let secret_type = json
        .pointer("/type")
        .and_then(Value::as_str)
        .unwrap_or("Opaque")
        .to_string();
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
    Ok(Some((out, secret_type)))
}

/// Read `PlatformStack/default.status.currentVersion` (the live platform-stack
/// version) so `restore --reprovision` bootstraps the target at the same
/// version. Falls back to `"unknown"` when the field is unset (a freshly
/// bootstrapped cluster whose operator hasn't stamped status yet).
/// Exported `pub(crate)` for use by `restore.rs`.
pub(crate) fn read_platform_version(kubeconfig: &Path) -> Result<String> {
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

/// The CNPG operator's own namespace, where the lazily-provisioned shared
/// integrated `platform-postgres` Cluster lives (never in an app namespace).
const CNPG_OPERATOR_NS: &str = "cnpg-system";

/// The CNPG operand image of the first CNPG Cluster found across the app
/// namespaces AND `cnpg-system`, used to pick a major-matched `pg_dump` helper
/// image. Falls back to the default pg image when none is found.
///
/// The `cnpg-system` scan is load-bearing: integrated-tier claims all share the
/// `platform-postgres` Cluster there (see the resourceclaim-provisioner), which
/// never appears in an app namespace, so an app-ns-only scan structurally
/// misses it and always falls back to the default major. For each Cluster CR
/// the image is read from `spec.imageName` first and, when that is unset (CNPG
/// derives the operand image from its own default or an ImageCatalogRef — the
/// common case, so `spec.imageName` is typically EMPTY), from `status.image`
/// (the resolved operand image CNPG stamps once the Cluster is running).
/// Without the `status.image` fallback a modern CNPG (PG 18) would silently
/// mismatch a `postgres:16` `pg_dump` (`pg_dump: server version mismatch`).
pub(crate) fn first_cnpg_image(namespaces: &[String], kubeconfig: &Path) -> Option<String> {
    // App namespaces first (per-claim owned clusters, if any), then the shared
    // integrated cluster's namespace. Dedup so `cnpg-system` isn't scanned
    // twice when it is already an app namespace.
    let mut scan: Vec<&str> = namespaces.iter().map(String::as_str).collect();
    if !scan.contains(&CNPG_OPERATOR_NS) {
        scan.push(CNPG_OPERATOR_NS);
    }
    for ns in scan {
        if let Ok(items) = list_items("clusters.postgresql.cnpg.io", Some(ns), kubeconfig) {
            if let Some(img) = items.iter().find_map(cnpg_cluster_image) {
                return Some(img);
            }
        }
    }
    None
}

/// Resolve a CNPG `Cluster`'s operand image: `spec.imageName` when set, else the
/// resolved `status.image` (populated once the Cluster is running, even when the
/// image comes from a default/ImageCatalogRef rather than an explicit spec).
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
            if let Some((data, secret_type)) = read_secret_data(name, ns, kc.path())? {
                write_secret_json(&ns_dir, name, &data, &secret_type)?;
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
        if let Some((data, secret_type)) = read_secret_data(name, ns, kc.path())? {
            write_secret_json(&sourcecred_dir, name, &data, &secret_type)?;
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

/// Write one secret's decrypted `key → bytes` map under `<dir>/<name>.json`,
/// including the Kubernetes secret type for a faithful roundtrip.
///
/// ## On-disk format
///
/// ```json
/// { "type": "<secret-type>", "data": { "key": "<base64-value>", … } }
/// ```
///
/// `restore.rs::read_secret_file` reads this shape and falls back gracefully
/// to `"Opaque"` when the `type` field is absent (old backups).
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
    fn cnpg_cluster_image_prefers_spec_then_status() {
        // Explicit spec.imageName wins.
        let spec = json!({"spec":{"imageName":"ghcr.io/cloudnative-pg/postgresql:16.2"}});
        assert_eq!(
            cnpg_cluster_image(&spec).as_deref(),
            Some("ghcr.io/cloudnative-pg/postgresql:16.2")
        );
        // Absent/empty spec.imageName falls back to the resolved status.image —
        // the integrated shared cluster path (CNPG derives PG 18 from its own
        // default, leaving spec.imageName empty). This is the regression: an
        // app-ns-only, spec-only lookup returned None → default postgres:16 →
        // `pg_dump: server version mismatch` against the PG 18 server.
        let status_only =
            json!({"spec":{},"status":{"image":"ghcr.io/cloudnative-pg/postgresql:18.3-1"}});
        assert_eq!(
            cnpg_cluster_image(&status_only).as_deref(),
            Some("ghcr.io/cloudnative-pg/postgresql:18.3-1")
        );
        let empty_spec = json!({"spec":{"imageName":""},"status":{"image":"postgres:17"}});
        assert_eq!(
            cnpg_cluster_image(&empty_spec).as_deref(),
            Some("postgres:17")
        );
        // The chosen image drives the helper major — 18.3-1 → postgres:18-alpine.
        assert_eq!(
            pg_helper_image(cnpg_cluster_image(&status_only).as_deref()),
            "postgres:18-alpine"
        );
        // Neither present → None (caller uses the pinned default).
        assert_eq!(cnpg_cluster_image(&json!({"spec":{}})), None);
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
