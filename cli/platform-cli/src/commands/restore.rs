// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! `apprafter restore` — 2.6d restore orchestration (Task 11).
//!
//! Replays a full encrypted backup (produced by `apprafter backup`) into a
//! RUNNING, already-bootstrapped target cluster. Two modes:
//!
//! * **Full restore** (`--data-only == false`):
//!   `RestoreArtifact` (restic → tempdir) → `ApplyPlatformStack` →
//!   `EnsureNamespaces` (create the backup's app namespaces before any
//!   namespaced apply — a fresh target lacks them) →
//!   `ApplySourceCredentials` → `ApplyAppsGated` (H2: claims provision,
//!   NO pod — [`zero_replicas`] + Argo auto-sync stripped) →
//!   `WaitClaimsBound` (poll `status.ready`, NOT PVC Bound — R1) →
//!   `LoadData` (pg uses the FRESH connection Secret — L3) →
//!   `ReSealUserSecrets` → `ResumeWorkloads` (replicas + auto-sync back).
//!
//! * **Data-only restore** (`--data-only == true`):
//!   `RestoreArtifact` → `SuspendWorkloads` (scale the existing app to 0 +
//!   disable its Argo auto-sync) → `LoadData` → `ResumeWorkloads`.
//!   No CR/secret replay.
//!
//! `--reprovision` (mode a) provisions a FRESH cluster in the target first
//! (`bootstrap_all::run`), then replays as restore-into-running.

use std::collections::BTreeMap;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};

use crate::commands::backup::KubectlExec;
use backup_core::helper_pod::{pg_dump_pod_spec, volume_pod_spec};
use backup_core::KubeExec;
use base64::Engine as _;
use cli_core::{CliError, Result};
use cli_providers::backup::images::{pg_helper_image, VOLUME_IMAGE};
use cli_providers::backup::manifest::BackupManifest;
use cli_providers::backup::reseal::reseal_secret;
use cli_providers::backup::restic::restic_restore_argv;
use cli_providers::backup::restore::{restore_steps, zero_replicas, RestoreMode, RestoreStep};
use cli_providers::backup::ResourceRef;
use cli_providers::k8s::kubectl::KubectlCli;
use cli_providers::k8s::sealing::fetch_controller_public_key;
use serde_json::Value;

use crate::commands::backup::{
    backup_passphrase_or_error, first_cnpg_image, list_items, read_platform_version,
    read_secret_data, resolve_operator_s3_creds, sourcecred_material_refs,
};
use crate::commands::k8s_helpers::{
    ensure_kubeconfig_tempfile_for_target, kubectl_apply_server_side, kubectl_get_json,
    kubectl_merge_patch,
};

// ---------------------------------------------------------------------------
// Restore-only constants
// ---------------------------------------------------------------------------

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
// Internal types
// ---------------------------------------------------------------------------

/// A captured CR read back off disk during restore.
struct LoadedCr {
    /// The backup's kind tag (`PlatformStack` / `SourceCredential` /
    /// `Application` / `ArgoApplication` / `SharedVolume`).
    kind: String,
    cr: Value,
}

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/// `apprafter restore` — replay a backup into a RUNNING, bootstrapped target
/// cluster (modes a-into-running / b). Drives the pure step list from
/// [`cli_providers::backup::restore::restore_steps`].
///
/// `--reprovision` (mode a) provisions a fresh cluster in the target first, then
/// replays; topology + cloud token come from the target's local config.
// 2.16h: server_type added the 8th argument — allow here instead of introducing
// an intermediate struct just to satisfy clippy (the function is a direct
// command entrypoint, not a deeply-called helper).
#[allow(clippy::too_many_arguments)]
pub fn run_restore(
    repo: &str,
    target: Option<&str>,
    reprovision: bool,
    snapshot: Option<&str>,
    data_only: bool,
    passphrase: Option<&str>,
    credential_file: Option<&Path>,
    server_type: Option<&str>,
) -> Result<()> {
    // Mutually-exclusive modes: `--reprovision` rebuilds the WHOLE cluster from
    // the backup (its step list has no data-only shortcut); `--data-only` reloads
    // native data into an ALREADY-running cluster. `restore_steps` returns the
    // data-only sequence (no Reprovision step) whenever `data_only`, so the combo
    // would silently skip provisioning and then fail with an unresolved
    // kubeconfig — reject it up front instead.
    if reprovision && data_only {
        return Err(CliError::Other(
            "--reprovision and --data-only are mutually exclusive: --reprovision rebuilds the \
             whole cluster from the backup, --data-only reloads data into a running one"
                .into(),
        ));
    }

    // 1. Passphrase + credentials (mandatory — the repo holds decrypted
    //    secrets). Gate FIRST, before touching any cluster/repo — a bad
    //    passphrase must not leave a freshly re-provisioned cluster
    //    half-restored.
    //
    //    Two credential sources, mirroring the operator maintenance verbs
    //    (prune/check/unlock): a REMOTE `s3:`/`b2:`/`gs:`/`azure:`/`rest:` repo
    //    (or ANY repo when `--credential-file` is given) needs the operator's
    //    full S3-style creds (AWS_* + RESTIC_PASSWORD) — resolved from the
    //    dotenv file or the process env, NEVER from the cluster. A LOCAL
    //    filesystem repo keeps the legacy RESTIC_PASSWORD-from-flag/env path.
    let is_tty = std::io::stdin().is_terminal();
    let (pass, creds): (String, BTreeMap<String, String>) =
        if credential_file.is_some() || is_remote_restic_repo(repo) {
            // resolve_operator_s3_creds errors when RESTIC_PASSWORD is absent;
            // surface that in the restore context (which knobs to reach for).
            let creds = resolve_operator_s3_creds(credential_file, &|k| std::env::var(k).ok())
                .map_err(|e| {
                    CliError::Other(format!(
                        "restore from a remote repo '{repo}' needs the operator's S3 \
                         credentials (AWS_ACCESS_KEY_ID / AWS_SECRET_ACCESS_KEY / \
                         RESTIC_PASSWORD) via --credential-file or the environment: {e}"
                    ))
                })?;
            let pass = creds
                .get("RESTIC_PASSWORD")
                .cloned()
                .expect("resolve_operator_s3_creds guarantees RESTIC_PASSWORD is present");
            (pass, creds)
        } else {
            let env_pass = std::env::var("RESTIC_PASSWORD").ok();
            let pass = backup_passphrase_or_error(passphrase, env_pass.as_deref(), is_tty)?;
            (pass, BTreeMap::new())
        };

    let mode = if reprovision {
        RestoreMode::Reprovision
    } else {
        RestoreMode::IntoRunning
    };
    let steps = restore_steps(mode, data_only);

    // Kubeconfig resolution is LAZY. For `--reprovision` the target cluster does
    // not exist yet — the Reprovision step provisions it (and caches its
    // kubeconfig into state), after which we resolve kc. For restore-into-running
    // / --data-only the cluster is already up, so resolve it now. Held alive for
    // the WHOLE flow: every kubectl/restic-adjacent shell-out below depends on it.
    let mut kc: Option<tempfile::NamedTempFile> = if reprovision {
        None
    } else {
        Some(ensure_kubeconfig_tempfile_for_target(target)?)
    };

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
        // The Reprovision step provisions + bootstraps a fresh cluster in the
        // target (topology + cloud token come from the target's local config,
        // exactly as `apprafter up` — R2), then resolves the now-cached
        // kubeconfig. It is always first, and every later step needs kc.
        if let RestoreStep::Reprovision = step {
            println!(
                "→ --reprovision: provisioning a fresh cluster in target '{}' before replay",
                target.unwrap_or("<active>")
            );
            crate::commands::bootstrap_all::run(target, false, server_type)?;
            kc = Some(ensure_kubeconfig_tempfile_for_target(target)?);
            continue;
        }
        let kc = kc.as_ref().ok_or_else(|| {
            CliError::Other("internal: kubeconfig unresolved before a restore step".into())
        })?;
        match step {
            RestoreStep::Reprovision => unreachable!("Reprovision handled before the match"),
            RestoreStep::RestoreArtifact => {
                run_restic_restore(
                    &restic_restore_argv(repo, snap, &restore_root.path().to_string_lossy()),
                    &pass,
                    &creds,
                )?;
                let dd = find_data_dir(restore_root.path())?;
                let m = read_backup_manifest(&dd)?;
                // m8: reject a backup written by a newer CLI — guard before
                // any further parsing or cluster writes.
                check_manifest_version(m.manifest_version)?;
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
            RestoreStep::EnsureNamespaces => {
                let m = manifest
                    .as_ref()
                    .ok_or_else(|| CliError::Other("EnsureNamespaces before artifact".into()))?;
                ensure_namespaces(&m.namespaces, kc.path())?;
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

/// Read one backed-up secret JSON and return the decoded `key → bytes` map
/// plus the secret type.
///
/// ## On-disk format (written by `backup.rs::write_secret_json`)
///
/// New backups write:
/// ```json
/// { "type": "Opaque", "data": { "key": "<base64>" } }
/// ```
///
/// Old backups (pre-FIX3) wrote a flat object:
/// ```json
/// { "key": "<base64>" }
/// ```
///
/// Both shapes are handled: when a `"data"` key is present the wrapper form is
/// used; otherwise the entire object is treated as the flat data map and the
/// type defaults to `"Opaque"` (all pre-FIX3 user secrets + SourceCredential
/// material are Opaque, so this is backward-compatible).
fn read_secret_file(path: &Path) -> Result<(BTreeMap<String, Vec<u8>>, String)> {
    let body = std::fs::read(path)
        .map_err(|e| CliError::Other(format!("read secret {}: {e}", path.display())))?;
    let envelope: Value = serde_json::from_slice(&body)
        .map_err(|e| CliError::Other(format!("parse secret {}: {e}", path.display())))?;

    // Detect new wrapper shape vs. legacy flat shape.
    let (secret_type, data_map) = if let Some(data_obj) = envelope.get("data") {
        // New shape: { "type": "...", "data": { ... } }
        let t = envelope
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("Opaque")
            .to_string();
        let obj = data_obj.as_object().ok_or_else(|| {
            CliError::Other(format!(
                "secret {} has a `data` key but it is not an object",
                path.display()
            ))
        })?;
        let mut map: BTreeMap<String, String> = BTreeMap::new();
        for (k, v) in obj {
            map.insert(k.clone(), v.as_str().unwrap_or("").to_string());
        }
        (t, map)
    } else {
        // Legacy flat shape: { "key": "<base64>", ... }
        let flat: BTreeMap<String, String> = serde_json::from_value(envelope).map_err(|e| {
            CliError::Other(format!(
                "parse secret {} (legacy flat): {e}",
                path.display()
            ))
        })?;
        ("Opaque".to_string(), flat)
    };

    let mut data = BTreeMap::new();
    for (k, b64) in data_map {
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(&b64)
            .map_err(|e| {
                CliError::Other(format!("decode secret {} key {k}: {e}", path.display()))
            })?;
        data.insert(k, bytes);
    }
    Ok((data, secret_type))
}

/// Apply a CR `Value` via server-side apply with the restore field manager.
/// JSON is valid YAML, so we serialize and pipe it on stdin.
fn apply_cr(cr: &Value, kubeconfig: &Path) -> Result<()> {
    let yaml = serde_json::to_string(cr)
        .map_err(|e| CliError::Other(format!("serialize CR for apply: {e}")))?;
    kubectl_apply_server_side(&yaml, RESTORE_FIELD_MANAGER, kubeconfig)
}

/// Build a bare `Namespace` object for `name` — the pure, unit-tested seam of
/// [`ensure_namespaces`].
fn namespace_object(name: &str) -> Value {
    serde_json::json!({
        "apiVersion": "v1",
        "kind": "Namespace",
        "metadata": { "name": name },
    })
}

/// **EnsureNamespaces** — SSA-apply a bare `Namespace` for every namespace the
/// backup captured, so the namespaced applies that follow (source credentials,
/// apps, re-sealed user secrets, data-load helper pods) land in an existing
/// namespace. Without this, a fresh restore target — which carries only the
/// platform namespaces from bootstrap — fails the first namespaced apply with
/// `namespaces "<ns>" not found`. Idempotent: a server-side apply of an
/// already-present namespace (including the platform ones) is a no-op.
fn ensure_namespaces(namespaces: &[String], kubeconfig: &Path) -> Result<()> {
    let ensured: Vec<&String> = namespaces.iter().filter(|n| !n.is_empty()).collect();
    for ns in &ensured {
        apply_cr(&namespace_object(ns), kubeconfig)?;
    }
    if !ensured.is_empty() {
        let names: Vec<&str> = ensured.iter().map(|s| s.as_str()).collect();
        println!("  ✓ namespaces ensured: {}", names.join(", "));
    }
    Ok(())
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
    let k = KubectlExec::new(kubeconfig.to_path_buf());
    // pg: data/pg/<ns>/<claim>.dump
    load_pg_dumps(data_dir, &k, kubeconfig)?;
    // volumes: data/volumes/<ns>/<name>/data.tar
    load_volumes(data_dir, manifest, &k, kubeconfig)?;
    // redis: documented skeleton (T12 verifies Dragonfly RDB).
    load_redis_skeleton(data_dir);
    Ok(())
}

/// Restore every `data/pg/<ns>/<claim>.dump` via `pg_restore` over a helper
/// pod, using the FRESH connection Secret (L3 — the post-provision creds, NOT
/// the backed-up ones).
fn load_pg_dumps(data_dir: &Path, k: &dyn KubeExec, kubeconfig: &Path) -> Result<()> {
    let pg_root = data_dir.join("pg");
    let Ok(ns_entries) = std::fs::read_dir(&pg_root) else {
        return Ok(());
    };
    // Collect the dump namespaces up front so the pg helper image can be
    // major-matched to the TARGET cluster's CNPG (reachable via `kubeconfig`).
    // `pg_restore` reading a custom-format archive written by a NEWER `pg_dump`
    // (the export now major-matches the server) fails with "unsupported
    // version" if the helper is the pinned default `postgres:16` while the
    // target runs PG 18 — so resolve the target major the same way export
    // does (spec.imageName / status.image across app-ns + cnpg-system), and
    // only fall back to the pinned default when discovery finds nothing.
    let ns_dirs: Vec<(String, PathBuf)> = ns_entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .filter_map(|p| {
            p.file_name()
                .map(|n| (n.to_string_lossy().into_owned(), p.clone()))
        })
        .collect();
    let namespaces: Vec<String> = ns_dirs.iter().map(|(ns, _)| ns.clone()).collect();
    let pg_image = pg_helper_image(first_cnpg_image(&namespaces, kubeconfig).as_deref());

    for (ns, ns_path) in ns_dirs {
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
            load_one_pg(&ns, &claim, &dump_path, k, kubeconfig, &pg_image)?;
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
fn load_one_pg(
    ns: &str,
    claim: &str,
    dump_path: &Path,
    k: &dyn KubeExec,
    kubeconfig: &Path,
    pg_image: &str,
) -> Result<()> {
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

    let (secret, _) = read_secret_data(secret_name, ns, kubeconfig)?.ok_or_else(|| {
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
    let mut spec = pg_dump_pod_spec(&pod_name, ns, pg_image);
    if let Some(container) = spec
        .pointer_mut("/spec/containers/0")
        .and_then(Value::as_object_mut)
    {
        // replaces env — pg_dump_pod_spec has no env; keep in sync if that changes
        container.insert(
            "env".to_string(),
            serde_json::json!([{ "name": "PGPASSWORD", "value": pass }]),
        );
    }

    let _guard = PodCleanupGuard {
        name: pod_name.clone(),
        namespace: ns.to_string(),
        k,
    };
    k.apply_and_wait_pod_ready(&spec)?;

    // Wait for the TARGET database to actually accept a connection before
    // streaming the dump. `WaitClaimsBound` only guarantees the ResourceClaim's
    // `.status.ready` (a control-plane condition); for the FIRST claim that
    // lazily provisions the shared CNPG cluster, the server can still be
    // finishing initdb (connection refused) AND the per-claim database can be
    // uncreated (`FATAL: database "…" does not exist`) when the claim flips
    // ready, so an immediate `pg_restore` aborts the whole restore. Probe the
    // db with `psql SELECT 1` (bounded) — the same "claim-ready ≠ TCP/DB-ready"
    // gap the export side and the app entrypoint already tolerate.
    wait_pg_reachable(&pod_name, ns, &host, &port, &user, &db, kubeconfig)?;

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
    k.exec_stream_from_file(&pod_name, ns, &argv, dump_path)?;
    println!("  ✓ pg restored: {ns}/{claim}");
    Ok(())
}

/// Poll a real connection to the TARGET database inside an already-running
/// helper pod until it succeeds, or a bounded timeout elapses. `PGPASSWORD` is
/// already in the pod env (set by the caller).
///
/// A `pg_isready` probe is NOT enough: for a lazily-provisioned shared CNPG
/// cluster, the SERVER accepts connections (to `postgres`) well before the
/// per-claim database (`claim_<ns>_<name>`) is created — `pg_isready -d <db>`
/// reports "up" regardless of whether `<db>` exists, so `pg_restore` would then
/// fail with `FATAL: database "<db>" does not exist`. Probe with `psql -d <db>
/// -c 'SELECT 1'`, which only succeeds once the database itself is reachable.
fn wait_pg_reachable(
    pod: &str,
    ns: &str,
    host: &str,
    port: &str,
    user: &str,
    db: &str,
    kubeconfig: &Path,
) -> Result<()> {
    const MAX_ATTEMPTS: u32 = 90; // ~3 min at 2s spacing
    const SPACING: std::time::Duration = std::time::Duration::from_secs(2);
    let mut last = String::new();
    for attempt in 0..MAX_ATTEMPTS {
        let out = std::process::Command::new("kubectl")
            .args([
                "exec", pod, "-n", ns, "--", "psql", "-h", host, "-p", port, "-U", user, "-d", db,
                "-tAc", "SELECT 1",
            ])
            .env("KUBECONFIG", kubeconfig)
            .output();
        match out {
            Ok(o) if o.status.success() => return Ok(()),
            Ok(o) => {
                last = format!(
                    "{}{}",
                    String::from_utf8_lossy(&o.stdout),
                    String::from_utf8_lossy(&o.stderr)
                );
            }
            Err(e) => last = format!("kubectl exec psql failed to spawn: {e}"),
        }
        if attempt + 1 < MAX_ATTEMPTS {
            std::thread::sleep(SPACING);
        }
    }
    Err(CliError::Other(format!(
        "pg database {db} on {host}:{port} was not reachable within {}s (last probe: {})",
        MAX_ATTEMPTS as u64 * SPACING.as_secs(),
        last.trim()
    )))
}

/// Restore every `data/volumes/<ns>/<name>/data.tar` into its fresh PVC via a
/// busybox helper pod mounted READ-WRITE (L1). The PVC to mount is the claim's
/// regenerated `status.volumeClaimRef` (or, for a SharedVolume, the SV's bound
/// PVC) — resolved from the live claim/SharedVolume by name.
fn load_volumes(
    data_dir: &Path,
    manifest: &BackupManifest,
    k: &dyn KubeExec,
    kubeconfig: &Path,
) -> Result<()> {
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
            load_one_volume(&ns, &name, &pvc, &tar_path, k)?;
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
    k: &dyn KubeExec,
) -> Result<()> {
    let pod_name = truncate_pod_name(&format!("ld-vol-{name}"));
    let spec = volume_pod_spec(&pod_name, ns, VOLUME_IMAGE, pvc, false); // L1: RW
    let _guard = PodCleanupGuard {
        name: pod_name.clone(),
        namespace: ns.to_string(),
        k,
    };
    k.apply_and_wait_pod_ready(&spec)?;
    let argv: Vec<&str> = vec!["tar", "x", "-C", "/data"];
    k.exec_stream_from_file(&pod_name, ns, &argv, tar_path)?;
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
    // Deduped so list_items is not called twice for a namespace that appears
    // in multiple claims (M2).
    let mut claim_namespaces: Vec<String> = manifest
        .resources
        .iter()
        .filter(|r| r.kind == "ResourceClaim")
        .map(|r| r.namespace.clone())
        .collect();
    claim_namespaces.sort();
    claim_namespaces.dedup();

    let mut recorded: Vec<((String, String), i64)> = Vec::new();
    for ns in &claim_namespaces {
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

/// Deletes a helper pod on drop — guarantees cleanup on every return path of a
/// LoadData step (mirrors `extract::HelperPodGuard`).
struct PodCleanupGuard<'a> {
    name: String,
    namespace: String,
    k: &'a dyn KubeExec,
}

impl Drop for PodCleanupGuard<'_> {
    fn drop(&mut self) {
        self.k.delete_pod_best_effort(&self.name, &self.namespace);
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
// Manifest version guard (m8)
// ---------------------------------------------------------------------------

/// Return `Ok(())` when `v <= MANIFEST_VERSION_CURRENT`, otherwise an error
/// that contains "unsupported backup format" so callers and tests can match on
/// it. The message also hints at upgrading the CLI so the user knows what to do.
fn check_manifest_version(v: u32) -> cli_core::Result<()> {
    let current = backup_core::manifest::MANIFEST_VERSION_CURRENT;
    if v > current {
        return Err(CliError::Other(format!(
            "unsupported backup format: manifest version {v} is newer than this CLI supports \
             (max {current}) — upgrade the CLI to restore this backup"
        )));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// restic runner
// ---------------------------------------------------------------------------

/// True when `repo` names a REMOTE restic backend (`s3:` / `b2:` / `gs:` /
/// `azure:` / `rest:`) that needs the operator's full S3-style credentials —
/// as opposed to a local filesystem path. Used to decide whether restore must
/// resolve operator creds (`--credential-file` / env) before shelling out to
/// restic: a local repo keeps the legacy RESTIC_PASSWORD-only path.
pub(crate) fn is_remote_restic_repo(repo: &str) -> bool {
    const REMOTE_PREFIXES: &[&str] = &["s3:", "b2:", "gs:", "azure:", "rest:"];
    REMOTE_PREFIXES.iter().any(|p| repo.starts_with(p))
}

/// Run a restic command, capturing output — silent on success, surfaces stderr
/// on failure. Used only for `restic restore` here; the backup-side runners
/// live in `backup.rs`.
///
/// `pass` sets `RESTIC_PASSWORD` explicitly (the trait/legacy contract), and
/// `creds` layers the operator's full S3 credentials (AWS_* + RESTIC_PASSWORD)
/// on top so an `s3:` (or other remote) repo is reachable — for a local repo it
/// is empty and only `RESTIC_PASSWORD` is set. `pass` and any
/// `creds["RESTIC_PASSWORD"]` are the same value at the call sites.
fn run_restic_restore(argv: &[String], pass: &str, creds: &BTreeMap<String, String>) -> Result<()> {
    let mut cmd = std::process::Command::new("restic");
    cmd.args(argv).env("RESTIC_PASSWORD", pass);
    crate::commands::backup::apply_creds_to_command(&mut cmd, creds);
    let out = cmd
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

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use backup_core::manifest::MANIFEST_VERSION_CURRENT;
    use serde_json::json;
    use std::io::Write;

    #[test]
    fn restore_rejects_a_newer_manifest_version_with_a_clear_error() {
        assert!(check_manifest_version(MANIFEST_VERSION_CURRENT).is_ok());
        let e = check_manifest_version(MANIFEST_VERSION_CURRENT + 1).unwrap_err();
        assert!(
            format!("{e}").contains("unsupported backup format"),
            "got: {e}"
        );
    }

    // -----------------------------------------------------------------------
    // FIX 2 (M3): strip_argo_automated
    // -----------------------------------------------------------------------

    #[test]
    fn strip_argo_automated_removes_key_and_leaves_siblings() {
        let argo = json!({
            "spec": {
                "syncPolicy": {
                    "automated": { "prune": true },
                    "retry": { "limit": 3 }
                }
            }
        });
        let s = strip_argo_automated(&argo);
        assert!(
            s.pointer("/spec/syncPolicy/automated").is_none(),
            "automated should be stripped"
        );
        assert!(
            s.pointer("/spec/syncPolicy/retry").is_some(),
            "sibling retry should survive"
        );
    }

    #[test]
    fn strip_argo_automated_noop_when_absent() {
        let argo = json!({
            "spec": {
                "syncPolicy": {
                    "retry": { "limit": 3 }
                }
            }
        });
        let s = strip_argo_automated(&argo);
        assert!(
            s.pointer("/spec/syncPolicy/retry").is_some(),
            "retry should be present when automated was never there"
        );
    }

    // -----------------------------------------------------------------------
    // FIX 3: read_secret_file new + legacy shapes
    // -----------------------------------------------------------------------

    fn write_tmpfile(content: &[u8]) -> (tempfile::NamedTempFile, std::path::PathBuf) {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(content).unwrap();
        let p = f.path().to_path_buf();
        (f, p)
    }

    #[test]
    fn read_secret_file_new_shape_roundtrips_type() {
        // New shape: { "type": "kubernetes.io/tls", "data": { "tls.crt": "<b64>" } }
        let tls_crt = base64::engine::general_purpose::STANDARD.encode(b"CERTBYTES");
        let content = format!(r#"{{"type":"kubernetes.io/tls","data":{{"tls.crt":"{tls_crt}"}}}}"#);
        let (_f, path) = write_tmpfile(content.as_bytes());
        let (data, secret_type) = read_secret_file(&path).unwrap();
        assert_eq!(secret_type, "kubernetes.io/tls");
        assert_eq!(data.get("tls.crt").unwrap(), b"CERTBYTES");
    }

    #[test]
    fn read_secret_file_legacy_flat_defaults_to_opaque() {
        // Legacy flat shape: { "key": "<b64>" }
        let val = base64::engine::general_purpose::STANDARD.encode(b"myvalue");
        let content = format!(r#"{{"key":"{val}"}}"#);
        let (_f, path) = write_tmpfile(content.as_bytes());
        let (data, secret_type) = read_secret_file(&path).unwrap();
        assert_eq!(secret_type, "Opaque");
        assert_eq!(data.get("key").unwrap(), b"myvalue");
    }

    #[test]
    fn read_secret_file_new_shape_missing_type_defaults_opaque() {
        // New shape with "data" but no "type" key.
        let val = base64::engine::general_purpose::STANDARD.encode(b"abc");
        let content = format!(r#"{{"data":{{"k":"{val}"}}}}"#);
        let (_f, path) = write_tmpfile(content.as_bytes());
        let (data, secret_type) = read_secret_file(&path).unwrap();
        assert_eq!(secret_type, "Opaque");
        assert_eq!(data.get("k").unwrap(), b"abc");
    }

    // -----------------------------------------------------------------------
    // is_remote_restic_repo — remote-backend detection for the creds contract
    // -----------------------------------------------------------------------

    #[test]
    fn is_remote_restic_repo_matches_remote_backends() {
        assert!(is_remote_restic_repo("s3:s3.amazonaws.com/bucket/prefix"));
        assert!(is_remote_restic_repo("b2:bucketname/path"));
        assert!(is_remote_restic_repo("gs:bucket/path"));
        assert!(is_remote_restic_repo("azure:container/path"));
        assert!(is_remote_restic_repo("rest:https://host/repo"));
    }

    #[test]
    fn is_remote_restic_repo_rejects_local_paths() {
        assert!(!is_remote_restic_repo("/var/lib/apprafter/backups/prod"));
        assert!(!is_remote_restic_repo("./relative/repo"));
        assert!(!is_remote_restic_repo("backups/prod"));
        // A local path that merely CONTAINS a scheme-like segment is still local.
        assert!(!is_remote_restic_repo("/tmp/s3-backup"));
    }

    #[test]
    fn namespace_object_has_the_apply_shape() {
        let ns = namespace_object("apprafter");
        assert_eq!(ns["apiVersion"], "v1");
        assert_eq!(ns["kind"], "Namespace");
        assert_eq!(ns["metadata"]["name"], "apprafter");
        // Cluster-scoped: no metadata.namespace on a Namespace object.
        assert!(ns["metadata"].get("namespace").is_none());
    }
}
