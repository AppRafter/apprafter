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
// Pure seams of the entry point
// ---------------------------------------------------------------------------

/// Reject the one flag combination whose step list would quietly do the wrong
/// thing.
///
/// `--reprovision` rebuilds the WHOLE cluster from the backup (its step list
/// has no data-only shortcut); `--data-only` reloads native data into an
/// ALREADY-running cluster. [`restore_steps`] returns the data-only sequence
/// (which has NO `Reprovision` step) whenever `data_only` is set, so the combo
/// would silently skip provisioning and then fail with an unresolved
/// kubeconfig — refuse it up front instead.
fn reject_conflicting_modes(reprovision: bool, data_only: bool) -> Result<()> {
    if reprovision && data_only {
        return Err(CliError::Other(
            "--reprovision and --data-only are mutually exclusive: --reprovision rebuilds the \
             whole cluster from the backup, --data-only reloads data into a running one"
                .into(),
        ));
    }
    Ok(())
}

/// The external binaries a restore must find BEFORE it does anything billable.
///
/// D11 / 2.22a. The credential gate had the right instinct and stopped one rung
/// too high: it refuses to provision on a bad passphrase, and said nothing
/// about a missing binary. `--reprovision` then runs a full billable provision
/// plus bootstrap, and the first `restic` spawn is in the step AFTER it — so an
/// absent restic used to cost a paid, running Hetzner cluster before anything
/// noticed. `helm` is only reachable on the reprovision path (bootstrap
/// installs the charts) so it is demanded only there; restic and kubectl are
/// needed by every mode.
fn tools_for_restore(reprovision: bool) -> Vec<&'static cli_core::tools::Tool> {
    let mut needed: Vec<&'static cli_core::tools::Tool> =
        vec![&cli_core::tools::RESTIC, &cli_core::tools::KUBECTL];
    if reprovision {
        needed.push(&cli_core::tools::HELM);
    }
    needed
}

/// Resolve `(restic password, extra credential env)` for a restore.
///
/// Two credential sources, mirroring the operator maintenance verbs
/// (prune/check/unlock): a REMOTE `s3:`/`b2:`/`gs:`/`azure:`/`rest:` repo (or
/// ANY repo when `--credential-file` is given) needs the operator's full
/// S3-style creds (AWS_* + RESTIC_PASSWORD) — resolved from the dotenv file or
/// the process env, NEVER from the cluster. A LOCAL filesystem repo keeps the
/// legacy RESTIC_PASSWORD-from-flag/env path.
///
/// `env` is the environment lookup and `operator_creds` the operator-credential
/// resolver, both injected so the decision is testable. `operator_creds` stays
/// a parameter rather than a direct call for a second reason: it keeps
/// `resolve_operator_s3_creds(` in `run_restore`'s own body, where
/// `tests/preflight_ordering_test.rs` reads it as the hazard that must come
/// AFTER the binary preflight (D11).
fn resolve_restore_credentials(
    repo: &str,
    credential_file: Option<&Path>,
    passphrase: Option<&str>,
    is_tty: bool,
    env: &dyn Fn(&str) -> Option<String>,
    operator_creds: &dyn Fn() -> Result<BTreeMap<String, String>>,
) -> Result<(String, BTreeMap<String, String>)> {
    if credential_file.is_some() || is_remote_restic_repo(repo) {
        // resolve_operator_s3_creds errors when RESTIC_PASSWORD is absent;
        // surface that in the restore context (which knobs to reach for).
        let creds = operator_creds().map_err(|e| {
            CliError::Other(format!(
                "restore from a remote repo '{repo}' needs S3 + restic credentials \
                 (S3_ACCESS_KEY_ID / S3_SECRET_ACCESS_KEY / RESTIC_PASSWORD; \
                 AWS_ACCESS_KEY_ID / AWS_SECRET_ACCESS_KEY / AWS_DEFAULT_REGION \
                 are accepted as aliases) via --credential-file or the environment: {e}"
            ))
        })?;
        let pass = creds
            .get("RESTIC_PASSWORD")
            .cloned()
            .expect("resolve_operator_s3_creds guarantees RESTIC_PASSWORD is present");
        Ok((pass, creds))
    } else {
        let env_pass = env("RESTIC_PASSWORD");
        let pass = backup_passphrase_or_error(passphrase, env_pass.as_deref(), is_tty)?;
        Ok((pass, BTreeMap::new()))
    }
}

/// The M1 cross-version note: warn (never fail) when the target's live
/// PlatformStack version differs from the backup's, because a cross-version
/// restore may re-render components. Silent when the target version is
/// `"unknown"` (a freshly bootstrapped cluster whose operator has not stamped
/// status yet — nothing to compare against) and on a `--data-only` restore
/// (which replays no CRs, so no component can be re-rendered).
fn cross_version_warning(
    data_only: bool,
    target_version: &str,
    backup_version: &str,
) -> Option<String> {
    if data_only || target_version == "unknown" || target_version == backup_version {
        return None;
    }
    Some(format!(
        "backup is from platform-stack {backup_version} but the target runs {target_version} — \
         a cross-version restore may re-render components; verify after restore"
    ))
}

/// The closing summary, as printable lines. Pure so the report a user reads on
/// their worst day is pinned by tests rather than by a walk.
fn restore_summary(
    manifest: Option<&BackupManifest>,
    target: Option<&str>,
    data_only: bool,
    resumed: usize,
    version_warning: Option<&str>,
) -> Vec<String> {
    let mut lines = vec![format!(
        "✓ Restored backup{} into target '{}'",
        manifest
            .map(|m| format!(" of cluster '{}'", m.cluster_id))
            .unwrap_or_default(),
        target.unwrap_or("<active>")
    )];
    if let Some(m) = manifest {
        lines.push(format!("  namespaces: {}", m.namespaces.join(", ")));
        lines.push(format!(
            "  mode:       {}",
            if data_only { "data-only" } else { "full" }
        ));
    }
    lines.push(format!("  workloads:  {resumed} app(s) resumed"));
    if let Some(w) = version_warning {
        lines.push(format!("  ⚠ {w}"));
    }
    lines
}

/// Unwrap a value the previous step should have produced, naming the step in
/// the error. A `None` here is an internal ordering bug, not user error.
fn produced_by_artifact<'a, T>(value: Option<&'a T>, step: &str) -> Result<&'a T> {
    value.ok_or_else(|| CliError::Other(format!("{step} before artifact")))
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
    reject_conflicting_modes(reprovision, data_only)?;

    // 0. External binaries, before the credential gate below (D11 / 2.22a —
    //    see [`tools_for_restore`]).
    cli_core::tools::preflight_tools(&tools_for_restore(reprovision), "apprafter restore")?;

    // 1. Passphrase + credentials (mandatory — the repo holds decrypted
    //    secrets). Gate FIRST, before touching any cluster/repo — a bad
    //    passphrase must not leave a freshly re-provisioned cluster
    //    half-restored.
    let is_tty = std::io::stdin().is_terminal();
    let env = |k: &str| std::env::var(k).ok();
    let (pass, creds): (String, BTreeMap<String, String>) =
        resolve_restore_credentials(repo, credential_file, passphrase, is_tty, &env, &|| {
            resolve_operator_s3_creds(credential_file, &env)
        })?;

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
                let dd = restore_artifact_tree(
                    &SubprocessRestic {
                        repo,
                        pass: &pass,
                        creds: &creds,
                    },
                    snap,
                    restore_root.path(),
                )?;

                let m = read_backup_manifest(&dd)?;
                // m8: reject a backup written by a newer CLI — guard before
                // any further parsing or cluster writes.
                check_manifest_version(m.manifest_version)?;
                let target_version = read_platform_version(kc.path())?;
                version_warning =
                    cross_version_warning(data_only, &target_version, &m.platform_version);
                data_dir = Some(dd);
                manifest = Some(m);
            }
            RestoreStep::ApplyPlatformStack => {
                let dd = produced_by_artifact(data_dir.as_ref(), "ApplyPlatformStack")?;
                apply_platformstack_from_crs(dd, kc.path())?;
            }
            RestoreStep::EnsureNamespaces => {
                let m = produced_by_artifact(manifest.as_ref(), "EnsureNamespaces")?;
                ensure_namespaces(&m.namespaces, kc.path())?;
            }
            RestoreStep::ApplySourceCredentials => {
                let dd = produced_by_artifact(data_dir.as_ref(), "ApplySourceCredentials")?;
                apply_source_credentials(dd, kc.path())?;
            }
            RestoreStep::ApplyAppsGated => {
                let dd = produced_by_artifact(data_dir.as_ref(), "ApplyAppsGated")?;
                app_replicas = apply_apps_gated(dd, kc.path(), &mut suspended_argo)?;
            }
            RestoreStep::WaitClaimsBound => {
                let m = produced_by_artifact(manifest.as_ref(), "WaitClaimsBound")?;
                wait_claims_bound(m, kc.path())?;
            }
            RestoreStep::LoadData => {
                let dd = produced_by_artifact(data_dir.as_ref(), "LoadData")?;
                let m = produced_by_artifact(manifest.as_ref(), "LoadData")?;
                load_data(dd, m, kc.path())?;
            }
            RestoreStep::ReSealUserSecrets => {
                let dd = produced_by_artifact(data_dir.as_ref(), "ReSealUserSecrets")?;
                reseal_user_secrets(dd, kc.path())?;
            }
            RestoreStep::SuspendWorkloads => {
                // --data-only: scale the running app(s) to 0 + disable Argo
                // auto-sync so the load doesn't race a live pod. We derive the
                // target apps from the backed-up artifact's data/ layout
                // (the namespaces/claims that have data to load).
                let m = produced_by_artifact(manifest.as_ref(), "SuspendWorkloads")?;
                app_replicas = suspend_running_workloads(m, kc.path(), &mut suspended_argo)?;
            }
            RestoreStep::ResumeWorkloads => {
                resume_workloads(&app_replicas, &suspended_argo, kc.path())?;
            }
        }
    }

    for line in restore_summary(
        manifest.as_ref(),
        target,
        data_only,
        app_replicas.len(),
        version_warning.as_deref(),
    ) {
        println!("{line}");
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Restore step implementations (impure — walk-validated)
// ---------------------------------------------------------------------------

/// The two restic reads a restore needs, behind a seam so the run-assembly
/// logic below (the D26 fix) is testable without a repository.
trait ResticFetch {
    /// `restic snapshots --json` for the configured repository.
    fn snapshots_json(&self) -> Result<String>;
    /// `restic restore <snapshot> --target <into>`.
    fn restore_snapshot(&self, snapshot: &str, into: &Path) -> Result<()>;
}

/// The production [`ResticFetch`]: shells out to `restic` with the resolved
/// password + operator credentials on the subprocess environment.
struct SubprocessRestic<'a> {
    repo: &'a str,
    pass: &'a str,
    creds: &'a BTreeMap<String, String>,
}

impl ResticFetch for SubprocessRestic<'_> {
    fn snapshots_json(&self) -> Result<String> {
        restic_stdout(
            &backup_core::restic::restic_snapshots_argv(self.repo),
            self.pass,
            self.creds,
        )
    }

    fn restore_snapshot(&self, snapshot: &str, into: &Path) -> Result<()> {
        run_restic_restore(
            &restic_restore_argv(self.repo, snapshot, &into.to_string_lossy()),
            self.pass,
            self.creds,
        )
    }
}

/// **RestoreArtifact** — materialise the WHOLE requested backup run under
/// `restore_root` and return the `data/` directory the later steps read.
///
/// Resolves the RUN, not a snapshot. A monolithic backup is one snapshot
/// carrying everything; a sequential one is N per-claim snapshots plus a commit
/// point, grouped only by their shared run tag. Fetching just the commit point
/// — which is what this did — yields `crs/`, `secrets/` and `manifest.json` and
/// no `data/`, so the load found nothing and the restore reported success over
/// an empty database (D26).
///
/// Each per-claim snapshot's payload is merged into the same data directory the
/// loader reads: restic restores a snapshot under its own absolute source path,
/// so each lands in its own tree and has to be folded in. The per-claim layout
/// is byte-identical to the monolithic one (the writer reuses `run_extraction`
/// on a one-element slice), so a plain merge is all that is needed.
fn restore_artifact_tree(
    restic: &dyn ResticFetch,
    requested_snapshot: &str,
    restore_root: &Path,
) -> Result<PathBuf> {
    let listing = restic.snapshots_json()?;
    let run = backup_core::restore::resolve_run_snapshots(&listing, requested_snapshot)
        .map_err(CliError::Other)?;

    restic.restore_snapshot(&run.commit, restore_root)?;
    let dd = find_data_dir(restore_root)?;

    if !run.claims.is_empty() {
        println!(
            "  sequential backup: merging {} per-claim snapshot(s)",
            run.claims.len()
        );
    }
    for claim_snap in &run.claims {
        let claim_root = tempfile::tempdir()
            .map_err(|e| CliError::Other(format!("temp dir for a per-claim snapshot: {e}")))?;
        restic.restore_snapshot(claim_snap, claim_root.path())?;
        match find_claim_data_dir(claim_root.path()) {
            Some(src) => merge_data_tree(&src, &dd)?,
            None => {
                // Not fatal: a run can legitimately carry a snapshot with no
                // claim payload. Say so rather than pretending it merged.
                println!("  note: snapshot {claim_snap} carries no claim data — skipped");
            }
        }
    }
    Ok(dd)
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
    let ensured = namespaces_to_ensure(namespaces);
    for ns in &ensured {
        apply_cr(&namespace_object(ns), kubeconfig)?;
    }
    if !ensured.is_empty() {
        println!("  ✓ namespaces ensured: {}", ensured.join(", "));
    }
    Ok(())
}

/// The namespaces [`ensure_namespaces`] actually applies: every non-empty entry
/// of the manifest's namespace list. An empty string would render as a
/// `Namespace` with no name and fail the apply, taking the whole restore with
/// it, so it is dropped here.
fn namespaces_to_ensure(namespaces: &[String]) -> Vec<&str> {
    namespaces
        .iter()
        .map(String::as_str)
        .filter(|n| !n.is_empty())
        .collect()
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
    apply_with_retry(
        PLATFORMSTACK_APPLY_ATTEMPTS,
        std::time::Duration::from_secs(PLATFORMSTACK_APPLY_BACKOFF_SECS),
        &mut |_attempt| kubectl_apply_server_side(&yaml, RESTORE_FIELD_MANAGER, kubeconfig),
    )?;
    println!("  ✓ PlatformStack applied");
    Ok(())
}

/// Retry `apply` up to `attempts` times, sleeping `backoff` between tries, and
/// return the LAST error if none succeeded.
///
/// The `platformstacks.apprafter.io` ValidatingWebhook's backing pod may briefly
/// lack Endpoints right after a bootstrap, so the first apply can race with
/// `no endpoints available for service "admission-webhook"`. The retry budget is
/// the caller's; the sleep never happens after the final attempt, so a failing
/// apply costs `attempts - 1` backoffs, not `attempts`.
fn apply_with_retry(
    attempts: u32,
    backoff: std::time::Duration,
    apply: &mut dyn FnMut(u32) -> Result<()>,
) -> Result<()> {
    let mut attempt = 0u32;
    loop {
        attempt += 1;
        match apply(attempt) {
            Ok(()) => return Ok(()),
            Err(e) if attempt < attempts => {
                eprintln!(
                    "info: PlatformStack apply failed (admission webhook likely not ready yet); \
                     retrying (attempt {attempt}): {e}"
                );
                std::thread::sleep(backoff);
            }
            Err(e) => return Err(e),
        }
    }
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

        for (ns, name, path) in sourcecred_material_files(&sc.cr, &sourcecred_dir) {
            let (data, secret_type) = read_secret_file(&path)?;
            let sealed = reseal_secret(&pub_key, &ns, &name, &secret_type, &data)?;
            apply_cr(&sealed, kubeconfig)?;
        }
    }
    println!("  ✓ {} SourceCredential(s) + material re-sealed", scs.len());
    Ok(())
}

/// The captured material Secrets one `SourceCredential` needs re-sealed:
/// every `(namespace, name)` it references (follow-the-reference, the same walk
/// the backup's capture path did), deduped — the launch default points the git
/// and registry refs at the SAME Secret, so without the dedup it would be
/// sealed and applied twice.
///
/// A reference whose material was never captured (it pointed outside the
/// captured set) is dropped rather than failing the restore: the CR apply above
/// has already surfaced the dangling reference.
fn sourcecred_material_files(sc: &Value, sourcecred_dir: &Path) -> Vec<(String, String, PathBuf)> {
    let mut refs = sourcecred_material_refs(sc);
    refs.sort();
    refs.dedup();
    refs.into_iter()
        .map(|(ns, name)| {
            let path = sourcecred_dir.join(format!("{name}.json"));
            (ns, name, path)
        })
        .filter(|(_, _, path)| path.exists())
        .collect()
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
    let plan = gated_apply_plan(&crs);

    for object in &plan.objects {
        apply_cr(object, kubeconfig)?;
    }
    suspended_argo.extend(plan.argo_apps);

    println!(
        "  ✓ {} app(s) applied gated (replicas=0, Argo auto-sync stripped)",
        plan.app_replicas.len()
    );
    Ok(plan.app_replicas)
}

/// What [`apply_apps_gated`] applies, and what it must remember to undo.
#[derive(Debug, Default, PartialEq)]
struct GatedApplyPlan {
    /// The objects to server-side apply, IN ORDER.
    objects: Vec<Value>,
    /// `((namespace, name), original replicas)` per AppRafter Application, for
    /// `ResumeWorkloads`.
    app_replicas: Vec<((String, String), i64)>,
    /// `(namespace, name)` per user Argo Application whose auto-sync was
    /// stripped and must be re-enabled.
    argo_apps: Vec<(String, String)>,
}

/// Turn the CRs read off the backup into the gated apply plan (H2) — the pure
/// seam of [`apply_apps_gated`].
///
/// Three invariants live here:
/// * `SharedVolume`s go FIRST — an app's `disk.ref` needs the SharedVolume to
///   exist before the Application that references it is applied.
/// * every AppRafter `Application` is applied through [`zero_replicas`], so its
///   claims provision but NO pod comes up on not-yet-loaded data, and its
///   ORIGINAL `spec.base.replicas` (defaulting to the operator's own default of
///   1 when the field is absent) is recorded for the resume.
/// * every user Argo `Application` is applied with `syncPolicy.automated`
///   stripped, so Argo CD cannot re-render the gated Application back to a
///   running workload mid-restore.
fn gated_apply_plan(crs: &[LoadedCr]) -> GatedApplyPlan {
    let mut plan = GatedApplyPlan::default();

    for sv in crs.iter().filter(|c| c.kind == "SharedVolume") {
        plan.objects.push(sv.cr.clone());
    }

    for app in crs.iter().filter(|c| c.kind == "Application") {
        let ns = cr_string(&app.cr, "/metadata/namespace", "");
        let name = cr_string(&app.cr, "/metadata/name", "");
        let replicas = app
            .cr
            .pointer("/spec/base/replicas")
            .and_then(Value::as_i64)
            .unwrap_or(1);
        plan.app_replicas.push(((ns, name), replicas));
        plan.objects.push(zero_replicas(&app.cr));
    }

    for argo in crs.iter().filter(|c| c.kind == "ArgoApplication") {
        let ns = cr_string(&argo.cr, "/metadata/namespace", "argocd");
        let name = cr_string(&argo.cr, "/metadata/name", "");
        plan.objects.push(strip_argo_automated(&argo.cr));
        plan.argo_apps.push((ns, name));
    }

    plan
}

/// Read a string field out of a CR, falling back to `default` when it is absent
/// or not a string.
fn cr_string(cr: &Value, pointer: &str, default: &str) -> String {
    cr.pointer(pointer)
        .and_then(Value::as_str)
        .unwrap_or(default)
        .to_string()
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
    wait_claims_ready_with(
        &claims_to_wait_for(manifest),
        CLAIM_READY_ATTEMPTS,
        std::time::Duration::from_secs(CLAIM_READY_BACKOFF_SECS),
        &mut |ns, name| {
            kubectl_get_json(
                "resourceclaims.apprafter.io",
                Some(name),
                Some(ns),
                kubeconfig,
            )
        },
    )
}

/// The claims a restore must see provision before it loads any data: the
/// `ResourceClaim` entries of the manifest (config CRs and data artifacts share
/// the same list, so the kind filter is what keeps the poll off a Secret).
fn claims_to_wait_for(manifest: &BackupManifest) -> Vec<&ResourceRef> {
    manifest
        .resources
        .iter()
        .filter(|r| r.kind == "ResourceClaim")
        .collect()
}

/// Poll `get` for each claim until `status.ready == true`, at most `attempts`
/// times per claim with `backoff` between polls.
///
/// R1: readiness is the claim's OWN `status.ready`, never PVC `Bound` — the
/// `LoadData` volume helper is the first PVC consumer, so waiting for Bound
/// would deadlock. An absent claim, an absent `status`, or `ready: false` all
/// count as not-ready; only an explicit `true` breaks the poll, so a claim that
/// never provisions fails the restore loudly instead of loading into nothing.
fn wait_claims_ready_with(
    claims: &[&ResourceRef],
    attempts: u32,
    backoff: std::time::Duration,
    get: &mut dyn FnMut(&str, &str) -> Result<Option<Value>>,
) -> Result<()> {
    if claims.is_empty() {
        return Ok(());
    }
    for claim in claims {
        let mut attempt = 0u32;
        loop {
            attempt += 1;
            let json = get(&claim.namespace, &claim.name)?;
            let ready = json
                .as_ref()
                .and_then(|j| j.pointer("/status/ready"))
                .and_then(Value::as_bool)
                .unwrap_or(false);
            if ready {
                break;
            }
            if attempt >= attempts {
                return Err(CliError::Other(format!(
                    "ResourceClaim {}/{} did not become ready within {}s",
                    claim.namespace,
                    claim.name,
                    attempts as u64 * backoff.as_secs()
                )));
            }
            std::thread::sleep(backoff);
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
    // redis: data/redis/<ns>/<claim>/dump.tar → Dragonfly whole-instance snapshot.
    load_redis(data_dir, &k, kubeconfig)?;
    Ok(())
}

/// Restore every `data/pg/<ns>/<claim>.dump` via `pg_restore` over a helper
/// pod, using the FRESH connection Secret (L3 — the post-provision creds, NOT
/// the backed-up ones).
fn load_pg_dumps(data_dir: &Path, k: &dyn KubeExec, kubeconfig: &Path) -> Result<()> {
    let dumps = discover_pg_dumps(data_dir);
    if dumps.is_empty() {
        return Ok(());
    }
    // Resolve the helper image from the dumps' namespaces up front so it can be
    // major-matched to the TARGET cluster's CNPG (reachable via `kubeconfig`).
    // `pg_restore` reading a custom-format archive written by a NEWER `pg_dump`
    // (the export now major-matches the server) fails with "unsupported
    // version" if the helper is the pinned default `postgres:16` while the
    // target runs PG 18 — so resolve the target major the same way export
    // does (spec.imageName / status.image across app-ns + cnpg-system), and
    // only fall back to the pinned default when discovery finds nothing.
    let mut namespaces: Vec<String> = dumps.iter().map(|(ns, _, _)| ns.clone()).collect();
    namespaces.dedup();
    let pg_image = pg_helper_image(first_cnpg_image(&namespaces, kubeconfig).as_deref());

    for (ns, claim, dump_path) in dumps {
        load_one_pg(&ns, &claim, &dump_path, k, kubeconfig, &pg_image)?;
    }
    Ok(())
}

/// Every pg artifact in a restored backup: `data/pg/<ns>/<claim>.dump` →
/// `(namespace, claim, path)`, ordered so a restore is reproducible.
///
/// Only `.dump` files count — the extractor writes nothing else there, but a
/// stray file (an editor swap file, a partially-written `.tmp`) must not be fed
/// to `pg_restore` as if it were an archive. A missing `data/pg` is not an
/// error: a backup with no pg claim simply has none.
fn discover_pg_dumps(data_dir: &Path) -> Vec<(String, String, PathBuf)> {
    let mut out = Vec::new();
    let Ok(ns_entries) = std::fs::read_dir(data_dir.join("pg")) else {
        return out;
    };
    for ns_path in ns_entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
    {
        let Some(ns) = ns_path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
        else {
            continue;
        };
        let Ok(dump_entries) = std::fs::read_dir(&ns_path) else {
            continue;
        };
        for dump_path in dump_entries.flatten().map(|e| e.path()) {
            if dump_path.extension().and_then(|s| s.to_str()) != Some("dump") {
                continue;
            }
            let claim = dump_path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or_default()
                .to_string();
            out.push((ns.clone(), claim, dump_path));
        }
    }
    out.sort();
    out
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
    let secret_name = connection_secret_name(&claim_json, ns, claim)?;

    let (secret, _) = read_secret_data(&secret_name, ns, kubeconfig)?.ok_or_else(|| {
        CliError::Other(format!(
            "connection Secret {ns}/{secret_name} for claim {claim} not found"
        ))
    })?;
    let conn = pg_connection_from_secret(&secret, ns, &secret_name)?;

    run_pg_restore(ns, claim, &conn, dump_path, k, pg_image, &|pod| {
        // Wait for the TARGET database to actually accept a connection before
        // streaming the dump. `WaitClaimsBound` only guarantees the
        // ResourceClaim's `.status.ready` (a control-plane condition); for the
        // FIRST claim that lazily provisions the shared CNPG cluster, the
        // server can still be finishing initdb (connection refused) AND the
        // per-claim database can be uncreated (`FATAL: database "…" does not
        // exist`) when the claim flips ready, so an immediate `pg_restore`
        // aborts the whole restore.
        wait_pg_reachable(pod, ns, &conn, kubeconfig)
    })
}

/// The FRESH connection Secret name of a regenerated claim (L3).
///
/// A claim with no `status.connectionSecretRef` has not been provisioned yet,
/// and loading a dump against the creds embedded in the BACKUP would target the
/// old, gone cluster — so this is a hard error, never a fallback.
fn connection_secret_name(claim_json: &Value, ns: &str, claim: &str) -> Result<String> {
    claim_json
        .pointer("/status/connectionSecretRef")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| {
            CliError::Other(format!(
                "claim {ns}/{claim} has no status.connectionSecretRef (not provisioned)"
            ))
        })
}

/// The connection parameters of a freshly-provisioned pg claim.
struct PgConnection {
    user: String,
    pass: String,
    host: String,
    port: String,
    db: String,
}

/// Read the five connection keys out of a pg claim's connection Secret, naming
/// the missing one when the Secret is incomplete. Every key is REQUIRED: a
/// silently-defaulted host or db would point `pg_restore` at the wrong database
/// and report success.
fn pg_connection_from_secret(
    secret: &BTreeMap<String, Vec<u8>>,
    ns: &str,
    secret_name: &str,
) -> Result<PgConnection> {
    let get = |key: &str| -> Result<String> {
        secret
            .get(key)
            .map(|v| String::from_utf8_lossy(v).into_owned())
            .ok_or_else(|| {
                CliError::Other(format!(
                    "connection Secret {ns}/{secret_name} missing key `{key}`"
                ))
            })
    };
    Ok(PgConnection {
        user: get("user")?,
        pass: get("pass")?,
        host: get("host")?,
        port: get("port")?,
        db: get("db")?,
    })
}

/// The `pg_restore` argv run inside the helper pod.
///
/// `--clean --if-exists` drops the objects the dump recreates (a restore runs
/// against a freshly-provisioned but not necessarily empty database) and
/// `--no-owner` keeps the dump's original role names from being demanded on a
/// cluster where the fresh claim owns a DIFFERENT generated role.
fn pg_restore_argv(conn: &PgConnection) -> Vec<String> {
    vec![
        "pg_restore".into(),
        "--no-owner".into(),
        "--clean".into(),
        "--if-exists".into(),
        "-h".into(),
        conn.host.clone(),
        "-p".into(),
        conn.port.clone(),
        "-U".into(),
        conn.user.clone(),
        "-d".into(),
        conn.db.clone(),
    ]
}

/// The pg helper pod spec (a network pod — the pg_dump image carries
/// `pg_restore`) with `PGPASSWORD` injected so `pg_restore` never prompts for a
/// password and hangs the restore.
fn pg_helper_pod_spec(pod_name: &str, ns: &str, pg_image: &str, pass: &str) -> Value {
    let mut spec = pg_dump_pod_spec(pod_name, ns, pg_image);
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
    spec
}

/// Stand a pg helper pod up, wait for the database behind `probe` to answer,
/// and stream the dump into `pg_restore` on its stdin (L2). The pod is deleted
/// on every return path by [`PodCleanupGuard`].
fn run_pg_restore(
    ns: &str,
    claim: &str,
    conn: &PgConnection,
    dump_path: &Path,
    k: &dyn KubeExec,
    pg_image: &str,
    probe: &dyn Fn(&str) -> Result<()>,
) -> Result<()> {
    let pod_name = truncate_pod_name(&format!("ld-pg-{claim}"));
    let spec = pg_helper_pod_spec(&pod_name, ns, pg_image, &conn.pass);

    let _guard = PodCleanupGuard {
        name: pod_name.clone(),
        namespace: ns.to_string(),
        k,
    };
    k.apply_and_wait_pod_ready(&spec)?;
    probe(&pod_name)?;

    let argv = pg_restore_argv(conn);
    let argv: Vec<&str> = argv.iter().map(String::as_str).collect();
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
fn wait_pg_reachable(pod: &str, ns: &str, conn: &PgConnection, kubeconfig: &Path) -> Result<()> {
    const MAX_ATTEMPTS: u32 = 90; // ~3 min at 2s spacing
    const SPACING: std::time::Duration = std::time::Duration::from_secs(2);
    poll_pg_reachable(MAX_ATTEMPTS, SPACING, conn, &mut || {
        let out = std::process::Command::new("kubectl")
            .args(psql_probe_args(pod, ns, conn))
            .env("KUBECONFIG", kubeconfig)
            .output();
        match out {
            Ok(o) if o.status.success() => Ok(()),
            Ok(o) => Err(format!(
                "{}{}",
                String::from_utf8_lossy(&o.stdout),
                String::from_utf8_lossy(&o.stderr)
            )),
            Err(e) => Err(format!("kubectl exec psql failed to spawn: {e}")),
        }
    })
}

/// The `kubectl exec` argv of one reachability probe.
///
/// The probe is `psql -d <db> -c 'SELECT 1'` and NOT `pg_isready`, because
/// `pg_isready -d <db>` reports "up" as soon as the SERVER accepts connections,
/// whether or not `<db>` exists — and the per-claim database of a lazily
/// provisioned shared CNPG cluster is created strictly after that. `pg_restore`
/// would then fail with `FATAL: database "<db>" does not exist`.
fn psql_probe_args<'a>(pod: &'a str, ns: &'a str, conn: &'a PgConnection) -> Vec<&'a str> {
    vec![
        "exec", pod, "-n", ns, "--", "psql", "-h", &conn.host, "-p", &conn.port, "-U", &conn.user,
        "-d", &conn.db, "-tAc", "SELECT 1",
    ]
}

/// Run `probe` until it succeeds or `attempts` are spent, sleeping `spacing`
/// between tries (never after the last one). The failure carries the LAST
/// probe output — the DB-side reason ("connection refused", "database … does
/// not exist") is the only thing that makes this timeout actionable.
fn poll_pg_reachable(
    attempts: u32,
    spacing: std::time::Duration,
    conn: &PgConnection,
    probe: &mut dyn FnMut() -> std::result::Result<(), String>,
) -> Result<()> {
    let mut last = String::new();
    for attempt in 0..attempts {
        match probe() {
            Ok(()) => return Ok(()),
            Err(e) => last = e,
        }
        if attempt + 1 < attempts {
            std::thread::sleep(spacing);
        }
    }
    Err(CliError::Other(format!(
        "pg database {} on {}:{} was not reachable within {}s (last probe: {})",
        conn.db,
        conn.host,
        conn.port,
        attempts as u64 * spacing.as_secs(),
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
    for (ns, name, tar_path) in discover_nested_artifacts(data_dir, "volumes", "data.tar") {
        let pvc = resolve_volume_pvc(&ns, &name, manifest, kubeconfig)?;
        load_one_volume(&ns, &name, &pvc, &tar_path, k)?;
    }
    Ok(())
}

/// Every `data/<kind>/<ns>/<name>/<file>` artifact a restored backup carries,
/// as `(namespace, name, path)`, ordered so a restore is reproducible.
///
/// Shared by the volume (`volumes`/`data.tar`) and redis (`redis`/`dump.tar`)
/// loaders — both are keyed by claim under a two-level namespace/name tree. A
/// directory without the expected payload file is skipped rather than fed to a
/// helper pod as an empty stream, and a missing `data/<kind>` is not an error:
/// a backup with no claim of that type simply has none.
fn discover_nested_artifacts(
    data_dir: &Path,
    kind: &str,
    file: &str,
) -> Vec<(String, String, PathBuf)> {
    let mut out = Vec::new();
    let Ok(ns_entries) = std::fs::read_dir(data_dir.join(kind)) else {
        return out;
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
            let payload = dir.join(file);
            if !payload.exists() {
                continue;
            }
            let name = name_entry.file_name().to_string_lossy().into_owned();
            out.push((ns.clone(), name, payload));
        }
    }
    out.sort();
    out
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
    claim_status_field(
        &claim_json,
        "/status/volumeClaimRef",
        "volumeClaimRef",
        ns,
        claim,
    )
}

/// Read a REQUIRED `status` field off a regenerated claim, naming the field in
/// the error. A claim missing it has not been provisioned, and every caller
/// needs the fresh, post-provision value — the backed-up one points at the
/// cluster that is gone.
fn claim_status_field(
    claim_json: &Value,
    pointer: &str,
    field: &str,
    ns: &str,
    claim: &str,
) -> Result<String> {
    claim_json
        .pointer(pointer)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| {
            CliError::Other(format!(
                "claim {ns}/{claim} has no status.{field} (not provisioned)"
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

/// Restore each persistent-redis whole-instance snapshot. The backup keys
/// artifacts by CLAIM (`data/redis/<ns>/<claim>/dump.tar`), but a Dragonfly
/// snapshot is whole-INSTANCE (all claims sharing a pool instance are in one
/// snapshot), so we resolve each claim's fresh `status.instance` and restore
/// each unique instance exactly once.
fn load_redis(data_dir: &Path, k: &dyn KubeExec, kubeconfig: &Path) -> Result<()> {
    let mut done: std::collections::HashSet<String> = std::collections::HashSet::new();
    for (ns, claim, tar_path) in discover_nested_artifacts(data_dir, "redis", "dump.tar") {
        let instance = resolve_redis_instance(&ns, &claim, kubeconfig)?;
        if !done.insert(instance.clone()) {
            continue; // another claim already restored this instance's snapshot
        }
        restore_one_redis_instance(&instance, &tar_path, k)?;
    }
    Ok(())
}

/// The Dragonfly pool instance a fresh persistent-redis claim is bound to
/// (`status.instance`), resolved from the regenerated claim.
fn resolve_redis_instance(ns: &str, claim: &str, kubeconfig: &Path) -> Result<String> {
    let claim_json = kubectl_get_json(
        "resourceclaims.apprafter.io",
        Some(claim),
        Some(ns),
        kubeconfig,
    )?
    .ok_or_else(|| {
        CliError::Other(format!(
            "claim {ns}/{claim} not found at redis LoadData — was it gated/applied?"
        ))
    })?;
    claim_status_field(&claim_json, "/status/instance", "instance", ns, claim)
}

/// Restore one Dragonfly instance's whole-instance snapshot by live-loading it
/// into the RUNNING instance: untar the backup into the pod's snapshot dir,
/// then `DFLY LOAD` the latest summary. No scale, so the provisioner never sees
/// the instance vanish and cannot re-provision (FLUSHDB) the claim's DB
/// mid-restore (the failure the scale approach hit on --data-only).
fn restore_one_redis_instance(instance: &str, tar_path: &Path, k: &dyn KubeExec) -> Result<()> {
    // DFLY LOAD works on the data port 6379, which is password-auth'd; read the
    // instance admin password (the admin port 9999 refuses DFLY LOAD).
    let pw = k.get_secret_key(
        &format!("{instance}-admin"),
        DRAGONFLY_NAMESPACE,
        "password",
    )?;

    let script = dfly_load_script(&pw);
    let argv: Vec<&str> = vec!["sh", "-c", &script];
    k.exec_stream_from_file(
        &format!("{instance}-0"),
        DRAGONFLY_NAMESPACE,
        &argv,
        tar_path,
    )?;

    println!("  ✓ redis instance restored: {instance} (DFLY LOAD replayed the snapshot)");
    Ok(())
}

/// The namespace the Dragonfly pool instances live in.
const DRAGONFLY_NAMESPACE: &str = "dragonfly-system";

/// The one-shot `sh -c` script that replays a whole-instance Dragonfly snapshot.
///
/// The tar streams to sh's stdin; `tar x` reads it; then `DFLY LOAD` replays the
/// newest summary file. Two things are load-bearing: `set -e` plus the explicit
/// `[ "$OUT" = OK ]` check, because `redis-cli` exits 0 even when the server
/// answers with an error — without the check a failed load would report a
/// successful restore over an empty instance; and the password goes through
/// [`shell_single_quote`], because it is generated and may contain characters
/// the shell would otherwise interpret.
fn dfly_load_script(password: &str) -> String {
    let pw_q = shell_single_quote(password);
    format!(
        "set -e; \
         rm -f /dragonfly/snapshots/* 2>/dev/null || true; \
         tar x -C /dragonfly/snapshots; \
         SUM=$(ls -1 /dragonfly/snapshots/*summary.dfs | sort | tail -1); \
         OUT=$(redis-cli -a {pw_q} --no-auth-warning -p 6379 DFLY LOAD \"$SUM\"); \
         [ \"$OUT\" = OK ] || {{ echo \"DFLY LOAD failed: $OUT\" >&2; exit 1; }}"
    )
}

/// POSIX-safe single-quote of an arbitrary string for embedding in a `sh -c`
/// script (wraps in single quotes, escaping embedded single quotes).
fn shell_single_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// **ReSealUserSecrets** — re-seal each app user secret under
/// `secrets/<ns>/<name>.json` (NOT `secrets/sourcecred/…`, which
/// `ApplySourceCredentials` already handled) for the TARGET cluster and apply
/// the resulting SealedSecret.
fn reseal_user_secrets(data_dir: &Path, kubeconfig: &Path) -> Result<()> {
    let secrets_dir = data_dir.join("secrets");
    let files = discover_user_secret_files(&secrets_dir);

    let kubectl = KubectlCli;
    let mut pub_key = None;

    for (ns, name, path) in &files {
        // Fetch the target pubkey lazily (only if there's at least one).
        if pub_key.is_none() {
            pub_key = Some(fetch_controller_public_key(&kubectl, kubeconfig)?);
        }
        let key = pub_key.as_ref().expect("pubkey fetched above");
        let (data, secret_type) = read_secret_file(path)?;
        let sealed = reseal_secret(key, ns, name, &secret_type, &data)?;
        apply_cr(&sealed, kubeconfig)?;
    }
    if !files.is_empty() {
        println!("  ✓ {} user secret(s) re-sealed", files.len());
    }
    Ok(())
}

/// The app user secrets to re-seal: `secrets/<ns>/<name>.json` →
/// `(namespace, name, path)`, ordered so a restore is reproducible.
///
/// `secrets/sourcecred/` is deliberately EXCLUDED — `ApplySourceCredentials`
/// has already re-sealed that material into the namespace its
/// `SourceCredential` reference names, and re-sealing it a second time here
/// would seal it under the literal namespace `sourcecred`, where nothing can
/// decrypt it.
fn discover_user_secret_files(secrets_dir: &Path) -> Vec<(String, String, PathBuf)> {
    let mut out = Vec::new();
    let Ok(ns_entries) = std::fs::read_dir(secrets_dir) else {
        return out;
    };
    for ns_entry in ns_entries.flatten() {
        let ns_path = ns_entry.path();
        let ns = ns_entry.file_name().to_string_lossy().into_owned();
        if !ns_path.is_dir() || ns == "sourcecred" {
            continue;
        }
        let Ok(secret_entries) = std::fs::read_dir(&ns_path) else {
            continue;
        };
        for path in secret_entries.flatten().map(|e| e.path()) {
            if path.extension().and_then(|s| s.to_str()) != Some("json") {
                continue;
            }
            let name = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or_default()
                .to_string();
            out.push((ns.clone(), name, path));
        }
    }
    out.sort();
    out
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
    for patch in resume_patches(app_replicas, suspended_argo) {
        kubectl_merge_patch(
            patch.resource,
            &patch.name,
            Some(&patch.namespace),
            None,
            &patch.body,
            kubeconfig,
        )?;
    }
    Ok(())
}

/// One merge-patch a workload step issues.
#[derive(Debug, PartialEq)]
struct MergePatch {
    resource: &'static str,
    namespace: String,
    name: String,
    body: String,
}

/// The patches that bring a restored cluster back up — the pure seam of
/// [`resume_workloads`].
///
/// Replicas are restored BEFORE Argo auto-sync is re-enabled: the recorded
/// count is the truth here, and letting Argo self-heal first would race the
/// gated `replicas: 0` back into the cluster from the config repo before the
/// real count lands.
fn resume_patches(
    app_replicas: &[((String, String), i64)],
    suspended_argo: &[(String, String)],
) -> Vec<MergePatch> {
    let mut out: Vec<MergePatch> = app_replicas
        .iter()
        .map(|((ns, name), replicas)| MergePatch {
            resource: "applications.apprafter.io",
            namespace: ns.clone(),
            name: name.clone(),
            body: replicas_patch_body(*replicas),
        })
        .collect();
    out.extend(suspended_argo.iter().map(|(ns, name)| MergePatch {
        resource: "applications.argoproj.io",
        namespace: ns.clone(),
        name: name.clone(),
        body: argo_autosync_patch_body(true),
    }));
    out
}

/// Merge-patch body setting an AppRafter Application's base replica count.
fn replicas_patch_body(replicas: i64) -> String {
    format!(r#"{{"spec":{{"base":{{"replicas":{replicas}}}}}}}"#)
}

/// Merge-patch body enabling or disabling an Argo Application's auto-sync.
///
/// Disabling writes an explicit `null`, which is how a JSON merge-patch DELETES
/// the key — an empty object would leave auto-sync on and let Argo revert the
/// scale-to-0 mid-load. Enabling restores the platform default (prune +
/// selfHeal).
fn argo_autosync_patch_body(enabled: bool) -> String {
    if enabled {
        r#"{"spec":{"syncPolicy":{"automated":{"prune":true,"selfHeal":true}}}}"#.to_string()
    } else {
        r#"{"spec":{"syncPolicy":{"automated":null}}}"#.to_string()
    }
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
    let claim_namespaces = claim_namespaces(manifest);

    let mut recorded: Vec<((String, String), i64)> = Vec::new();
    for ns in &claim_namespaces {
        let apps = list_items("applications.apprafter.io", Some(ns), kubeconfig)?;
        for (name, replicas) in apps_to_suspend(&apps, ns, &recorded) {
            recorded.push(((ns.to_string(), name.clone()), replicas));

            // Disable Argo auto-sync for this app's Argo Application(s) so the
            // scale-to-0 isn't reverted, then scale to 0.
            for (argo_ns, argo_name) in argo_apps_for(&name, kubeconfig)? {
                kubectl_merge_patch(
                    "applications.argoproj.io",
                    &argo_name,
                    Some(&argo_ns),
                    None,
                    &argo_autosync_patch_body(false),
                    kubeconfig,
                )?;
                suspended_argo.push((argo_ns, argo_name));
            }
            kubectl_merge_patch(
                "applications.apprafter.io",
                &name,
                Some(ns),
                None,
                &replicas_patch_body(0),
                kubeconfig,
            )?;
        }
    }
    if !recorded.is_empty() {
        println!("  ✓ {} app(s) suspended for data-only load", recorded.len());
    }
    Ok(recorded)
}

/// The namespaces a `--data-only` restore must quiesce: the distinct namespaces
/// of the manifest's ResourceClaims (the claims whose data is about to be
/// loaded). Deduped so `list_items` is not called twice for a namespace that
/// appears in several claims (M2).
fn claim_namespaces(manifest: &BackupManifest) -> Vec<String> {
    let mut out: Vec<String> = manifest
        .resources
        .iter()
        .filter(|r| r.kind == "ResourceClaim")
        .map(|r| r.namespace.clone())
        .collect();
    out.sort();
    out.dedup();
    out
}

/// The `(name, current replicas)` of each Application in one namespace that
/// still needs suspending — the pure seam of [`suspend_running_workloads`].
///
/// An Application with no name is skipped (there is nothing to patch), and one
/// already present in `already_recorded` is skipped so a second pass over the
/// same namespace cannot record `replicas: 0` — the value it just wrote — as
/// the count to resume, which would leave the app scaled to zero after a
/// successful restore. An absent `spec.base.replicas` reads as 1, the
/// operator's own default.
fn apps_to_suspend(
    apps: &[Value],
    ns: &str,
    already_recorded: &[((String, String), i64)],
) -> Vec<(String, i64)> {
    let mut out: Vec<(String, i64)> = Vec::new();
    for app in apps {
        let name = cr_string(app, "/metadata/name", "");
        if name.is_empty()
            || already_recorded
                .iter()
                .any(|((n, a), _)| n == ns && a == &name)
            || out.iter().any(|(a, _)| a == &name)
        {
            continue;
        }
        let replicas = app
            .pointer("/spec/base/replicas")
            .and_then(Value::as_i64)
            .unwrap_or(1);
        out.push((name, replicas));
    }
    out
}

/// Find the user Argo Application(s) that manage an AppRafter Application by
/// the `apprafter.io/application=<name>` label. Returns `(namespace, name)`
/// pairs. Used by the data-only suspend path.
fn argo_apps_for(app_name: &str, kubeconfig: &Path) -> Result<Vec<(String, String)>> {
    let items = crate::commands::k8s_helpers::kubectl_get_json_by_selector(
        "applications.argoproj.io",
        &argo_app_selector(app_name),
        None,
        kubeconfig,
    )?;
    Ok(argo_app_refs(&items))
}

/// The label selector that ties a user Argo Application back to the AppRafter
/// Application it manages. Per-environment Argo apps are named
/// `<app>-<env>`, so selecting by NAME would miss them — the label is what
/// makes the match environment-independent.
fn argo_app_selector(app_name: &str) -> String {
    format!("apprafter.io/application={app_name}")
}

/// `(namespace, name)` of each Argo Application item. An item missing either
/// coordinate is dropped rather than patched under a guessed namespace.
fn argo_app_refs(items: &[Value]) -> Vec<(String, String)> {
    items
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
        .collect()
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
/// `restic …` capturing stdout — the listing side of [`run_restic_restore`].
fn restic_stdout(argv: &[String], pass: &str, creds: &BTreeMap<String, String>) -> Result<String> {
    let mut cmd = std::process::Command::new("restic");
    cmd.args(argv).env("RESTIC_PASSWORD", pass);
    crate::commands::backup::apply_creds_to_command(&mut cmd, creds);
    let out = cmd
        .output()
        .map_err(|e| CliError::Other(format!("spawn restic: {e}")))?;
    restic_output_to_result(argv, &out)
}

/// Map a finished `restic` invocation to `Ok(stdout)` / a `CliError` carrying
/// the subcommand, exit code and stderr.
///
/// A non-zero restic exit MUST become an error here: the restore steps that
/// follow read the restored tree off disk, so a swallowed failure would leave
/// an empty tree and report a successful restore over nothing (the shape of
/// D26). stderr is carried verbatim because "wrong password" and "repository
/// is locked" are the two things a user needs to see.
fn restic_output_to_result(argv: &[String], out: &std::process::Output) -> Result<String> {
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

/// The payload directory inside a restored PER-CLAIM snapshot.
///
/// Distinct from [`find_data_dir`], which anchors on `manifest.json` — only the
/// commit point carries one. A per-claim snapshot is recognised by the data
/// kinds the extractor writes.
fn find_claim_data_dir(root: &Path) -> Option<PathBuf> {
    fn search(dir: &Path) -> Option<PathBuf> {
        let mut subdirs = Vec::new();
        let mut has_payload = false;
        for e in std::fs::read_dir(dir).ok()?.flatten() {
            let p = e.path();
            if p.is_dir() {
                if matches!(
                    p.file_name().and_then(|n| n.to_str()),
                    Some("pg") | Some("redis") | Some("disk")
                ) {
                    has_payload = true;
                }
                subdirs.push(p);
            }
        }
        if has_payload {
            return Some(dir.to_path_buf());
        }
        subdirs.iter().find_map(|sd| search(sd))
    }
    search(root)
}

/// Recursively copy `from` into `into`, creating directories as needed.
///
/// Per-claim snapshots hold disjoint trees (one claim each), so a plain merge
/// cannot collide; an existing file is nonetheless left alone rather than
/// overwritten, because silently replacing restored data would be the worst
/// possible way to be wrong here.
fn merge_data_tree(from: &Path, into: &Path) -> Result<()> {
    for e in std::fs::read_dir(from)
        .map_err(|e| CliError::Other(format!("reading {}: {e}", from.display())))?
        .flatten()
    {
        let src = e.path();
        let dst = into.join(e.file_name());
        if src.is_dir() {
            std::fs::create_dir_all(&dst)
                .map_err(|e| CliError::Other(format!("creating {}: {e}", dst.display())))?;
            merge_data_tree(&src, &dst)?;
        } else if !dst.exists() {
            std::fs::copy(&src, &dst).map_err(|e| {
                CliError::Other(format!(
                    "copying {} -> {}: {e}",
                    src.display(),
                    dst.display()
                ))
            })?;
        }
    }
    Ok(())
}

fn run_restic_restore(argv: &[String], pass: &str, creds: &BTreeMap<String, String>) -> Result<()> {
    let mut cmd = std::process::Command::new("restic");
    cmd.args(argv).env("RESTIC_PASSWORD", pass);
    crate::commands::backup::apply_creds_to_command(&mut cmd, creds);
    let out = cmd
        .output()
        .map_err(|e| CliError::Other(format!("spawn restic: {e}")))?;
    restic_output_to_result(argv, &out).map(|_| ())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use backup_core::manifest::MANIFEST_VERSION_CURRENT;
    use cli_core::resolve::resolve_precedence;
    use serde_json::json;
    use std::cell::RefCell;
    use std::io::Write;

    #[test]
    fn shell_single_quote_escapes_embedded_quotes() {
        assert_eq!(super::shell_single_quote("abc123"), "'abc123'");
        assert_eq!(super::shell_single_quote("a'b"), "'a'\\''b'");
    }

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

    // -----------------------------------------------------------------------
    // Task 14: --reprovision resolves server_type via the unified chain
    //
    // The reprovision flow:
    //   restore --reprovision
    //     → bootstrap_all::run(target, false, server_type)
    //     → apply::run(target_override, server_type)
    //     → resolve_precedence(flag, manifest, state, target, env)
    //     → None (when all rungs absent) → ServerTypeNotSelected
    //
    // The end-to-end cannot be unit-tested without live infrastructure;
    // these tests guard the two pure seams: (a) that `resolve_precedence`
    // returns None when all inputs are absent (no silent cpx22 default), and
    // (b) that no `cpx22` fallback exists anywhere in the restore module.
    // -----------------------------------------------------------------------

    /// When every resolution rung (flag / manifest / state / target / env) is
    /// absent, `resolve_precedence` returns `None`. The provider's create path
    /// then fires `CliError::ServerTypeNotSelected` — there is no silent
    /// `cpx22` default anywhere in the restore → bootstrap-all → apply chain.
    #[test]
    fn reprovision_server_type_resolution_returns_none_when_all_rungs_absent() {
        let resolved = resolve_precedence(
            None, // --server-type flag absent
            None, // manifest node kind absent
            None, // state.server_type absent
            None, // target.server_type absent
            None, // APPRAFTER_SERVER_TYPE env absent
        );
        assert!(
            resolved.is_none(),
            "expected None (no silent default) when all server-type rungs are absent, \
             got: {resolved:?}"
        );
    }

    /// The flag rung dominates — when `--server-type cpx22` is passed into
    /// `restore --reprovision`, it propagates through `resolve_precedence` and
    /// reaches `apply`'s provision path.
    #[test]
    fn reprovision_server_type_flag_rung_wins_over_all_others() {
        let resolved = resolve_precedence(
            Some("cpx22"), // --server-type flag
            None,
            None,
            None,
            None,
        );
        assert_eq!(
            resolved.as_deref(),
            Some("cpx22"),
            "flag rung must propagate the chosen type to the provision path"
        );
    }

    // =======================================================================
    // Test doubles
    //
    // The restore drives two outside worlds: `restic` (the repository) and
    // `kubectl` (the target cluster). Both are behind seams, so the decisions
    // between them can be pinned here instead of on a live cluster — which is
    // how D26 (a sequential backup that restored empty and reported success)
    // reached a user in the first place.
    // =======================================================================

    /// A [`ResticFetch`] that "restores" pre-baked trees. `trees` maps a
    /// snapshot id to the relative paths (and contents) it materialises under
    /// the restore target, mirroring restic's habit of recreating the
    /// snapshot's ABSOLUTE source path under `--target`.
    struct FakeRestic {
        listing: String,
        trees: BTreeMap<String, Vec<(String, String)>>,
        restored: RefCell<Vec<String>>,
    }

    impl FakeRestic {
        fn new(listing: &str) -> Self {
            Self {
                listing: listing.to_string(),
                trees: BTreeMap::new(),
                restored: RefCell::new(Vec::new()),
            }
        }

        fn with_tree(mut self, snapshot: &str, files: &[(&str, &str)]) -> Self {
            self.trees.insert(
                snapshot.to_string(),
                files
                    .iter()
                    .map(|(p, c)| (p.to_string(), c.to_string()))
                    .collect(),
            );
            self
        }
    }

    impl ResticFetch for FakeRestic {
        fn snapshots_json(&self) -> Result<String> {
            Ok(self.listing.clone())
        }

        fn restore_snapshot(&self, snapshot: &str, into: &Path) -> Result<()> {
            self.restored.borrow_mut().push(snapshot.to_string());
            for (rel, contents) in self.trees.get(snapshot).into_iter().flatten() {
                let path = into.join(rel);
                std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                std::fs::write(&path, contents).unwrap();
            }
            Ok(())
        }
    }

    /// A recording [`KubeExec`]. Every helper-pod interaction lands in one of
    /// the three logs so a test can assert what the restore actually asked the
    /// cluster to do — including the cleanup that must happen on the error
    /// path.
    /// `(pod, namespace, argv, stdin file)` of one recorded `kubectl exec`.
    type ExecCall = (String, String, Vec<String>, PathBuf);

    #[derive(Default)]
    struct FakeKube {
        applied: RefCell<Vec<Value>>,
        execs: RefCell<Vec<ExecCall>>,
        deleted: RefCell<Vec<(String, String)>>,
        secrets: BTreeMap<String, String>,
        exec_fails: bool,
    }

    impl FakeKube {
        fn with_secret(mut self, secret: &str, ns: &str, key: &str, value: &str) -> Self {
            self.secrets
                .insert(format!("{ns}/{secret}/{key}"), value.to_string());
            self
        }

        fn failing_exec() -> Self {
            Self {
                exec_fails: true,
                ..Self::default()
            }
        }
    }

    impl KubeExec for FakeKube {
        fn apply_and_wait_pod_ready(&self, spec: &Value) -> Result<()> {
            self.applied.borrow_mut().push(spec.clone());
            Ok(())
        }

        fn exec_stream_to_file(
            &self,
            _pod: &str,
            _ns: &str,
            _argv: &[&str],
            _out: &Path,
        ) -> Result<()> {
            unreachable!("restore never streams a pod's stdout to a file")
        }

        fn exec_stream_from_file(
            &self,
            pod: &str,
            ns: &str,
            argv: &[&str],
            input: &Path,
        ) -> Result<()> {
            self.execs.borrow_mut().push((
                pod.to_string(),
                ns.to_string(),
                argv.iter().map(|s| s.to_string()).collect(),
                input.to_path_buf(),
            ));
            if self.exec_fails {
                return Err(CliError::Other("exec failed".into()));
            }
            Ok(())
        }

        fn delete_pod_best_effort(&self, name: &str, ns: &str) {
            self.deleted
                .borrow_mut()
                .push((name.to_string(), ns.to_string()));
        }

        fn get_secret_key(&self, secret: &str, ns: &str, key: &str) -> Result<String> {
            self.secrets
                .get(&format!("{ns}/{secret}/{key}"))
                .cloned()
                .ok_or_else(|| CliError::Other(format!("no secret {ns}/{secret}")))
        }

        fn get_json(&self, _args: &[&str]) -> Result<Option<Value>> {
            unreachable!("restore reads JSON through kubectl_get_json, not KubeExec")
        }
    }

    fn manifest_of(namespaces: &[&str], resources: Vec<ResourceRef>) -> BackupManifest {
        BackupManifest {
            manifest_version: MANIFEST_VERSION_CURRENT,
            cluster_id: "k3d-demo".into(),
            created_at: "2026-09-02T00:00:00Z".into(),
            platform_version: "0.2.40".into(),
            namespaces: namespaces.iter().map(|s| s.to_string()).collect(),
            resources,
        }
    }

    fn resource(kind: &str, ns: &str, name: &str) -> ResourceRef {
        ResourceRef {
            namespace: ns.into(),
            kind: kind.into(),
            name: name.into(),
            claim_type: None,
        }
    }

    fn loaded(kind: &str, cr: Value) -> LoadedCr {
        LoadedCr {
            kind: kind.to_string(),
            cr,
        }
    }

    /// `mkdir -p` + write, for building artifact trees under a tempdir.
    fn write_at(root: &Path, rel: &str, contents: &str) {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }

    // =======================================================================
    // Entry-point seams: mode, tools, credentials
    // =======================================================================

    /// `--reprovision --data-only` must be refused rather than run: the
    /// data-only step list has no `Reprovision` step, so the combination would
    /// skip provisioning and fail later with an unresolved kubeconfig — after
    /// the user believed a rebuild was under way.
    #[test]
    fn reject_conflicting_modes_refuses_only_reprovision_plus_data_only() {
        assert!(reject_conflicting_modes(false, false).is_ok());
        assert!(reject_conflicting_modes(true, false).is_ok());
        assert!(reject_conflicting_modes(false, true).is_ok());
        assert!(reject_conflicting_modes(true, true).is_err());
    }

    /// The preflight demands `helm` ONLY on the reprovision path. Demanding it
    /// everywhere would refuse a perfectly good restore-into-running on a
    /// workstation without helm; not demanding it on `--reprovision` is how a
    /// billable cluster gets provisioned and then fails at bootstrap (D11).
    #[test]
    fn tools_for_restore_demands_helm_only_when_reprovisioning() {
        let names = |reprovision| -> Vec<&'static str> {
            tools_for_restore(reprovision)
                .iter()
                .map(|t| t.name)
                .collect()
        };
        assert_eq!(names(false), vec!["restic", "kubectl"]);
        assert_eq!(names(true), vec!["restic", "kubectl", "helm"]);
    }

    /// A remote (`s3:`) repository resolves the operator's FULL credential set
    /// from the environment, and the restic password comes from that same set —
    /// not from a `--passphrase` flag, which would leave AWS_* unset and make
    /// the repository unreachable.
    #[test]
    fn resolve_restore_credentials_uses_operator_creds_for_a_remote_repo() {
        let env = |k: &str| -> Option<String> {
            match k {
                "S3_ACCESS_KEY_ID" => Some("AKIA".into()),
                "S3_SECRET_ACCESS_KEY" => Some("secret".into()),
                "RESTIC_PASSWORD" => Some("from-env".into()),
                _ => None,
            }
        };
        let (pass, creds) = resolve_restore_credentials(
            "s3:https://s3.example/bucket",
            None,
            Some("flag"),
            false,
            &env,
            &|| resolve_operator_s3_creds(None, &env),
        )
        .unwrap();
        assert_eq!(pass, "from-env");
        assert_eq!(
            creds.get("S3_ACCESS_KEY_ID").map(String::as_str),
            Some("AKIA")
        );
    }

    /// A remote repository with no credentials in the environment fails with
    /// the knobs named — restic would otherwise fail deep inside a subprocess
    /// with an opaque backend error.
    #[test]
    fn resolve_restore_credentials_errors_when_a_remote_repo_has_no_creds() {
        let no_env = |_: &str| None;
        let e = resolve_restore_credentials(
            "s3:https://s3.example/b",
            None,
            None,
            false,
            &no_env,
            &|| resolve_operator_s3_creds(None, &no_env),
        )
        .unwrap_err();
        let msg = format!("{e}");
        assert!(msg.contains("S3_ACCESS_KEY_ID"), "got: {msg}");
        assert!(msg.contains("--credential-file"), "got: {msg}");
    }

    /// A LOCAL filesystem repository keeps the legacy path: RESTIC_PASSWORD
    /// from the flag/env, and NO operator credential map (there is no S3
    /// backend to reach, and demanding AWS_* would break local restores).
    #[test]
    fn resolve_restore_credentials_keeps_the_legacy_passphrase_for_a_local_repo() {
        let unreachable_creds = || -> Result<BTreeMap<String, String>> {
            panic!("a local repo must not resolve operator S3 credentials")
        };
        let (pass, creds) = resolve_restore_credentials(
            "/var/backups/prod",
            None,
            Some("flag-pass"),
            false,
            &|_| None,
            &unreachable_creds,
        )
        .unwrap();
        assert_eq!(pass, "flag-pass");
        assert!(creds.is_empty(), "a local repo needs no S3 credentials");

        let (from_env, _) = resolve_restore_credentials(
            "/var/backups/prod",
            None,
            None,
            false,
            &|k| (k == "RESTIC_PASSWORD").then(|| "env-pass".to_string()),
            &unreachable_creds,
        )
        .unwrap();
        assert_eq!(from_env, "env-pass");
    }

    /// No passphrase anywhere and no TTY to prompt on → refuse. The repository
    /// holds DECRYPTED secrets; continuing with an empty key is never right.
    #[test]
    fn resolve_restore_credentials_refuses_a_local_repo_with_no_passphrase() {
        assert!(resolve_restore_credentials(
            "/var/backups/prod",
            None,
            None,
            false,
            &|_| None,
            &|| Ok(BTreeMap::new()),
        )
        .is_err());
    }

    /// The cross-version note fires only when there is a real, comparable
    /// difference: a `--data-only` restore replays no CRs (nothing to
    /// re-render) and an `unknown` target version means the operator has not
    /// stamped status yet, so neither may raise a false alarm.
    #[test]
    fn cross_version_warning_fires_only_on_a_real_comparable_mismatch() {
        assert!(cross_version_warning(false, "0.2.40", "0.2.41").is_some());
        assert!(cross_version_warning(false, "0.2.40", "0.2.40").is_none());
        assert!(cross_version_warning(false, "unknown", "0.2.41").is_none());
        assert!(cross_version_warning(true, "0.2.40", "0.2.41").is_none());
    }

    /// The closing report names the source cluster, the namespaces it touched,
    /// the mode, and the number of workloads it brought back — with the version
    /// warning last when there is one.
    #[test]
    fn restore_summary_reports_scope_mode_and_workloads() {
        let m = manifest_of(&["demo", "shop"], vec![]);
        let lines = restore_summary(Some(&m), Some("prod"), false, 2, None);
        assert_eq!(
            lines,
            vec![
                "✓ Restored backup of cluster 'k3d-demo' into target 'prod'".to_string(),
                "  namespaces: demo, shop".to_string(),
                "  mode:       full".to_string(),
                "  workloads:  2 app(s) resumed".to_string(),
            ]
        );

        let data_only = restore_summary(Some(&m), None, true, 1, Some("mind the gap"));
        assert_eq!(
            data_only[0],
            "✓ Restored backup of cluster 'k3d-demo' into target '<active>'"
        );
        assert_eq!(data_only[2], "  mode:       data-only");
        assert_eq!(data_only[4], "  ⚠ mind the gap");
    }

    /// With no manifest (the artifact step never completed) the summary still
    /// prints, but claims nothing about the backup's contents.
    #[test]
    fn restore_summary_without_a_manifest_claims_no_scope() {
        let lines = restore_summary(None, Some("prod"), false, 0, None);
        assert_eq!(
            lines,
            vec![
                "✓ Restored backup into target 'prod'".to_string(),
                "  workloads:  0 app(s) resumed".to_string(),
            ]
        );
    }

    // =======================================================================
    // RestoreArtifact — the D26 path
    // =======================================================================

    /// A sequential run's listing: two per-claim snapshots and the commit
    /// point, sharing one run tag (the only thing that groups them).
    fn sequential_listing() -> &'static str {
        r#"[
          {"id":"claimA","short_id":"claimA","time":"2026-09-02T19:11:29Z","tags":["platform-run-1"]},
          {"id":"claimB","short_id":"claimB","time":"2026-09-02T19:11:30Z","tags":["platform-run-1"]},
          {"id":"commit","short_id":"commit","time":"2026-09-02T19:11:31Z","tags":["platform-run-1"]}
        ]"#
    }

    /// D26 REGRESSION GUARD. A sequential backup's payloads live in the
    /// per-claim snapshots, NOT in the commit point — which carries only
    /// `crs/`, `secrets/` and `manifest.json`. Restoring the commit point alone
    /// left `data/pg` empty, so the loader found nothing and the restore
    /// reported success over an empty database. Every snapshot of the run must
    /// be fetched and folded into the one directory the loader reads.
    #[test]
    fn restore_artifact_tree_merges_every_per_claim_snapshot_of_the_run() {
        let root = tempfile::tempdir().unwrap();
        let restic = FakeRestic::new(sequential_listing())
            .with_tree(
                "commit",
                &[
                    ("staging/data/manifest.json", "{}"),
                    ("staging/data/crs/0-PlatformStack-x-default.json", "{}"),
                ],
            )
            .with_tree("claimA", &[("claim-0/data/pg/demo/db.dump", "PGDUMP-A")])
            .with_tree(
                "claimB",
                &[("claim-1/data/redis/demo/cache/dump.tar", "TAR-B")],
            );

        let dd = restore_artifact_tree(&restic, "latest", root.path()).unwrap();

        assert_eq!(
            std::fs::read_to_string(dd.join("pg/demo/db.dump")).unwrap(),
            "PGDUMP-A",
            "the pg payload of a per-claim snapshot must land in the loader's data dir"
        );
        assert_eq!(
            std::fs::read_to_string(dd.join("redis/demo/cache/dump.tar")).unwrap(),
            "TAR-B"
        );
        assert!(
            dd.join("manifest.json").exists(),
            "the commit point's own tree survives the merge"
        );
        assert_eq!(
            *restic.restored.borrow(),
            vec![
                "commit".to_string(),
                "claimA".to_string(),
                "claimB".to_string()
            ],
            "the commit point is restored first, then every claim snapshot of the run"
        );
    }

    /// A monolithic backup is one snapshot carrying everything: nothing else is
    /// fetched, and the data directory is the one the commit point restored.
    #[test]
    fn restore_artifact_tree_fetches_only_the_snapshot_of_a_monolithic_run() {
        let root = tempfile::tempdir().unwrap();
        let listing = r#"[{"id":"solo","short_id":"solo","time":"2026-09-02T19:00:00Z","tags":["platform-run-9"]}]"#;
        let restic = FakeRestic::new(listing).with_tree(
            "solo",
            &[
                ("staging/data/manifest.json", "{}"),
                ("staging/data/pg/demo/db.dump", "PGDUMP"),
            ],
        );

        let dd = restore_artifact_tree(&restic, "latest", root.path()).unwrap();

        assert_eq!(*restic.restored.borrow(), vec!["solo".to_string()]);
        assert_eq!(
            std::fs::read_to_string(dd.join("pg/demo/db.dump")).unwrap(),
            "PGDUMP"
        );
    }

    /// A run snapshot with no claim payload (it carries only config) is
    /// skipped, not merged and not fatal — but it must not silently drag a
    /// non-payload tree into the data directory either.
    #[test]
    fn restore_artifact_tree_skips_a_snapshot_that_carries_no_claim_data() {
        let root = tempfile::tempdir().unwrap();
        let restic = FakeRestic::new(sequential_listing())
            .with_tree("commit", &[("staging/data/manifest.json", "{}")])
            .with_tree("claimA", &[("claim-0/data/pg/demo/db.dump", "PGDUMP-A")])
            .with_tree(
                "claimB",
                &[("claim-1/data/crs/0-Application-demo-web.json", "{}")],
            );

        let dd = restore_artifact_tree(&restic, "latest", root.path()).unwrap();

        assert!(dd.join("pg/demo/db.dump").exists());
        assert!(
            !dd.join("crs/0-Application-demo-web.json").exists(),
            "a payload-less snapshot must not be merged over the commit point's CRs"
        );
    }

    /// A restored tree with no `manifest.json` is not an AppRafter backup — say
    /// so instead of proceeding to apply nothing and calling it a restore.
    #[test]
    fn restore_artifact_tree_errors_when_the_commit_point_has_no_manifest() {
        let root = tempfile::tempdir().unwrap();
        let listing =
            r#"[{"id":"solo","short_id":"solo","time":"2026-09-02T19:00:00Z","tags":["t"]}]"#;
        let restic =
            FakeRestic::new(listing).with_tree("solo", &[("staging/data/pg/demo/db.dump", "x")]);
        let e = restore_artifact_tree(&restic, "latest", root.path()).unwrap_err();
        assert!(format!("{e}").contains("no manifest.json"), "got: {e}");
    }

    // =======================================================================
    // Tree probing + merging
    // =======================================================================

    /// `restic restore --target` recreates the snapshot's ABSOLUTE source path
    /// under the target, so `data/` is nested arbitrarily deep. The manifest is
    /// the anchor, and its PARENT is the data directory.
    #[test]
    fn find_data_dir_anchors_on_the_manifest_however_deep_it_sits() {
        let root = tempfile::tempdir().unwrap();
        write_at(
            root.path(),
            "tmp/apprafter-backup-abc/data/manifest.json",
            "{}",
        );
        let dd = find_data_dir(root.path()).unwrap();
        assert_eq!(dd, root.path().join("tmp/apprafter-backup-abc/data"));
    }

    #[test]
    fn find_data_dir_errors_when_nothing_in_the_tree_is_a_manifest() {
        let root = tempfile::tempdir().unwrap();
        write_at(
            root.path(),
            "tmp/data/crs/0-Application-demo-web.json",
            "{}",
        );
        assert!(find_data_dir(root.path()).is_err());
    }

    /// A per-claim snapshot carries NO manifest, so it is recognised by the
    /// payload kinds the extractor writes (`pg` / `redis` / `disk`). A tree
    /// with only config directories is not a claim payload and must return
    /// None, so the merge is skipped rather than folding CRs in.
    #[test]
    fn find_claim_data_dir_anchors_on_a_payload_kind_directory() {
        let root = tempfile::tempdir().unwrap();
        write_at(root.path(), "claim-0/data/pg/demo/db.dump", "x");
        assert_eq!(
            find_claim_data_dir(root.path()),
            Some(root.path().join("claim-0/data"))
        );

        let no_payload = tempfile::tempdir().unwrap();
        write_at(
            no_payload.path(),
            "claim-0/data/crs/0-Application-demo-web.json",
            "{}",
        );
        assert_eq!(find_claim_data_dir(no_payload.path()), None);
    }

    /// Merging folds a claim tree into the data directory — and NEVER
    /// overwrites a file already there. Silently replacing restored data would
    /// be the worst possible way to be wrong on a restore.
    #[test]
    fn merge_data_tree_adds_new_files_and_leaves_existing_ones_alone() {
        let from = tempfile::tempdir().unwrap();
        let into = tempfile::tempdir().unwrap();
        write_at(from.path(), "pg/demo/db.dump", "NEW");
        write_at(from.path(), "redis/demo/cache/dump.tar", "FRESH");
        write_at(into.path(), "pg/demo/db.dump", "ALREADY-THERE");

        merge_data_tree(from.path(), into.path()).unwrap();

        assert_eq!(
            std::fs::read_to_string(into.path().join("pg/demo/db.dump")).unwrap(),
            "ALREADY-THERE"
        );
        assert_eq!(
            std::fs::read_to_string(into.path().join("redis/demo/cache/dump.tar")).unwrap(),
            "FRESH"
        );
    }

    #[test]
    fn merge_data_tree_errors_when_the_source_is_unreadable() {
        let into = tempfile::tempdir().unwrap();
        assert!(merge_data_tree(&into.path().join("does-not-exist"), into.path()).is_err());
    }

    // =======================================================================
    // Reading the artifact off disk
    // =======================================================================

    /// The backup's kind tag comes from the FILE NAME, not the body: a user
    /// Argo Application is stored with the tag `ArgoApplication` while its body
    /// says `kind: Application`. Reading the body would file it as an AppRafter
    /// Application and gate it to replicas=0 — patching the wrong resource.
    #[test]
    fn read_crs_takes_the_kind_from_the_filename_not_the_body() {
        let dd = tempfile::tempdir().unwrap();
        write_at(
            dd.path(),
            "crs/1-ArgoApplication-argocd-web.json",
            r#"{"kind":"Application","metadata":{"name":"web"}}"#,
        );
        write_at(
            dd.path(),
            "crs/0-PlatformStack-apprafter-system-default.json",
            r#"{"kind":"PlatformStack"}"#,
        );
        write_at(dd.path(), "crs/notes.txt", "ignored");

        let crs = read_crs(dd.path()).unwrap();
        let kinds: Vec<&str> = crs.iter().map(|c| c.kind.as_str()).collect();
        assert_eq!(
            kinds,
            vec!["PlatformStack", "ArgoApplication"],
            "kinds come from the filename segment, in sorted (index) order, and non-JSON is skipped"
        );
        assert_eq!(crs[1].cr["metadata"]["name"], "web");
    }

    /// A data-only export has no `crs/` at all — that is a shape, not a
    /// failure.
    #[test]
    fn read_crs_is_empty_when_the_backup_has_no_crs_directory() {
        let dd = tempfile::tempdir().unwrap();
        assert!(read_crs(dd.path()).unwrap().is_empty());
    }

    #[test]
    fn read_crs_errors_on_a_corrupt_cr_file() {
        let dd = tempfile::tempdir().unwrap();
        write_at(dd.path(), "crs/0-Application-demo-web.json", "{not json");
        assert!(read_crs(dd.path()).is_err());
    }

    #[test]
    fn read_backup_manifest_parses_the_artifact_root_manifest() {
        let dd = tempfile::tempdir().unwrap();
        write_at(
            dd.path(),
            "manifest.json",
            r#"{"clusterId":"k3d-demo","createdAt":"t","platformVersion":"0.2.40",
                "namespaces":["demo"],"resources":[]}"#,
        );
        let m = read_backup_manifest(dd.path()).unwrap();
        assert_eq!(m.cluster_id, "k3d-demo");
        assert_eq!(m.namespaces, vec!["demo".to_string()]);
        assert_eq!(
            m.manifest_version, MANIFEST_VERSION_CURRENT,
            "a shipped v1 manifest carries no manifestVersion and must default, not fail"
        );
    }

    #[test]
    fn read_backup_manifest_errors_when_it_is_missing_or_corrupt() {
        let dd = tempfile::tempdir().unwrap();
        assert!(read_backup_manifest(dd.path()).is_err());
        write_at(dd.path(), "manifest.json", "{oops");
        assert!(read_backup_manifest(dd.path()).is_err());
    }

    /// A pod name must fit the 63-char DNS-1123 label limit, and truncation
    /// must not leave a trailing `-` (which is itself invalid).
    #[test]
    fn truncate_pod_name_fits_the_dns_label_limit_without_a_trailing_dash() {
        let short = truncate_pod_name("ld-pg-cache");
        assert_eq!(short, "ld-pg-cache");

        let long = truncate_pod_name(&format!("ld-pg-{}", "a".repeat(80)));
        assert_eq!(long.len(), 63);

        // 62 chars then a dash lands exactly on the boundary: the dash must go.
        let dashed = truncate_pod_name(&format!("{}-tail", "b".repeat(62)));
        assert_eq!(dashed.len(), 62);
        assert!(!dashed.ends_with('-'));
    }

    // =======================================================================
    // Apply plans
    // =======================================================================

    /// The gated apply is the H2 heart of a restore: claims must provision
    /// while NO pod comes up on not-yet-loaded data. Three things are pinned —
    /// SharedVolumes go first (an app's `disk.ref` needs one to exist), every
    /// Application is applied through `zero_replicas` with its ORIGINAL count
    /// recorded for the resume, and every user Argo Application is applied with
    /// auto-sync stripped so Argo cannot re-render the workload back up.
    #[test]
    fn gated_apply_plan_orders_volumes_first_and_gates_every_workload() {
        let crs = vec![
            loaded(
                "Application",
                json!({"kind":"Application","metadata":{"namespace":"demo","name":"web"},
                       "spec":{"base":{"replicas":3},"environments":{"prod":{"replicas":2}}}}),
            ),
            loaded(
                "ArgoApplication",
                json!({"kind":"Application","metadata":{"namespace":"argocd","name":"web-prod"},
                       "spec":{"syncPolicy":{"automated":{"prune":true},"retry":{"limit":3}}}}),
            ),
            loaded(
                "SharedVolume",
                json!({"kind":"SharedVolume","metadata":{"namespace":"demo","name":"shared"}}),
            ),
        ];

        let plan = gated_apply_plan(&crs);

        let kinds: Vec<&str> = plan
            .objects
            .iter()
            .map(|o| o["kind"].as_str().unwrap_or_default())
            .collect();
        assert_eq!(
            kinds,
            vec!["SharedVolume", "Application", "Application"],
            "SharedVolumes must be applied before the apps that reference them"
        );
        assert_eq!(plan.objects[1]["spec"]["base"]["replicas"], 0);
        assert_eq!(
            plan.objects[1]["spec"]["environments"]["prod"]["replicas"], 0,
            "an environment must not sneak a pod up while the data is still missing"
        );
        assert!(
            plan.objects[2]
                .pointer("/spec/syncPolicy/automated")
                .is_none(),
            "the user Argo app must be applied with auto-sync stripped"
        );
        assert!(
            plan.objects[2].pointer("/spec/syncPolicy/retry").is_some(),
            "stripping auto-sync must not take the rest of the sync policy with it"
        );
        assert_eq!(
            plan.app_replicas,
            vec![(("demo".to_string(), "web".to_string()), 3)],
            "the ORIGINAL replica count is what ResumeWorkloads restores"
        );
        assert_eq!(
            plan.argo_apps,
            vec![("argocd".to_string(), "web-prod".to_string())]
        );
    }

    /// Defaults for a CR that omits them: an absent `spec.base.replicas` reads
    /// as 1 (the operator's own default — recording 0 would leave the app down
    /// after a successful restore) and an Argo Application with no namespace
    /// belongs to `argocd`.
    #[test]
    fn gated_apply_plan_defaults_replicas_to_one_and_argo_namespace_to_argocd() {
        let crs = vec![
            loaded(
                "Application",
                json!({"metadata":{"namespace":"demo","name":"web"},"spec":{"base":{"image":"x"}}}),
            ),
            loaded("ArgoApplication", json!({"metadata":{"name":"web-prod"}})),
        ];
        let plan = gated_apply_plan(&crs);
        assert_eq!(plan.app_replicas[0].1, 1);
        assert_eq!(plan.argo_apps[0].0, "argocd");
    }

    /// A backup with no PlatformStack (an older shape) is not an error: the
    /// plan simply carries nothing to apply.
    #[test]
    fn gated_apply_plan_is_empty_for_a_backup_with_no_workloads() {
        let plan = gated_apply_plan(&[loaded("PlatformStack", json!({"kind":"PlatformStack"}))]);
        assert_eq!(plan, GatedApplyPlan::default());
    }

    /// An empty namespace name would render as a `Namespace` object with no
    /// name, failing the apply and taking the whole restore with it.
    #[test]
    fn namespaces_to_ensure_drops_empty_entries() {
        let namespaces = vec!["demo".to_string(), String::new(), "shop".to_string()];
        assert_eq!(namespaces_to_ensure(&namespaces), vec!["demo", "shop"]);
        assert!(namespaces_to_ensure(&[]).is_empty());
    }

    /// The launch default points a SourceCredential's git and registry refs at
    /// the SAME material Secret, so the refs must be deduped — otherwise it is
    /// sealed and applied twice. A reference whose material was never captured
    /// is dropped rather than failing the restore.
    #[test]
    fn sourcecred_material_files_dedupes_refs_and_drops_uncaptured_material() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("git-creds.json"), "{}").unwrap();
        let sc = json!({
            "metadata": {"namespace": "apprafter-system", "name": "default"},
            "spec": {
                "git": {"backend": {"sealedSecretRef": {"name": "git-creds"}}},
                "registry": {"backend": {"sealedSecretRef": {"name": "git-creds"}}}
            }
        });

        let files = sourcecred_material_files(&sc, dir.path());
        assert_eq!(
            files,
            vec![(
                "apprafter-system".to_string(),
                "git-creds".to_string(),
                dir.path().join("git-creds.json")
            )]
        );

        let dangling = json!({
            "metadata": {"namespace": "apprafter-system", "name": "default"},
            "spec": {"git": {"backend": {"sealedSecretRef": {"name": "never-captured"}}}}
        });
        assert!(
            sourcecred_material_files(&dangling, dir.path()).is_empty(),
            "material that was never captured must be skipped, not read"
        );
    }

    /// Resume patches replicas BEFORE re-enabling Argo auto-sync: letting Argo
    /// self-heal first would race the gated `replicas: 0` back in from the
    /// config repo before the recorded count lands.
    #[test]
    fn resume_patches_restore_replicas_before_re_enabling_autosync() {
        let apps = vec![(("demo".to_string(), "web".to_string()), 3)];
        let argo = vec![("argocd".to_string(), "web-prod".to_string())];
        let patches = resume_patches(&apps, &argo);

        assert_eq!(
            patches.iter().map(|p| p.resource).collect::<Vec<&str>>(),
            vec!["applications.apprafter.io", "applications.argoproj.io"]
        );
        assert_eq!(patches[0].namespace, "demo");
        assert_eq!(patches[0].name, "web");
        let body: Value = serde_json::from_str(&patches[0].body).unwrap();
        assert_eq!(body["spec"]["base"]["replicas"], 3);
        let argo_body: Value = serde_json::from_str(&patches[1].body).unwrap();
        assert_eq!(
            argo_body["spec"]["syncPolicy"]["automated"]["selfHeal"],
            true
        );
    }

    /// Disabling auto-sync writes an explicit `null` — that is how a JSON
    /// merge-patch DELETES the key. An empty object would leave auto-sync on
    /// and let Argo revert the scale-to-0 in the middle of the data load.
    #[test]
    fn argo_autosync_patch_body_nulls_automated_to_delete_it() {
        let off: Value = serde_json::from_str(&argo_autosync_patch_body(false)).unwrap();
        assert!(off["spec"]["syncPolicy"]["automated"].is_null());

        let on: Value = serde_json::from_str(&argo_autosync_patch_body(true)).unwrap();
        assert_eq!(on["spec"]["syncPolicy"]["automated"]["prune"], true);
        assert_eq!(on["spec"]["syncPolicy"]["automated"]["selfHeal"], true);
    }

    #[test]
    fn replicas_patch_body_is_a_valid_merge_patch_on_base_replicas() {
        let body: Value = serde_json::from_str(&replicas_patch_body(0)).unwrap();
        assert_eq!(body["spec"]["base"]["replicas"], 0);
        assert_eq!(body["spec"]["base"].as_object().unwrap().len(), 1);
    }

    /// A `--data-only` restore suspends the apps of the namespaces whose claims
    /// it is about to load. An app already recorded must NOT be recorded again:
    /// the second read would see the `replicas: 0` this step just wrote and
    /// resume the app to zero — a successful restore that leaves the app down.
    #[test]
    fn apps_to_suspend_records_each_app_once_with_its_live_replica_count() {
        let apps = vec![
            json!({"metadata":{"name":"web"},"spec":{"base":{"replicas":2}}}),
            json!({"metadata":{"name":"api"}}),
            json!({"metadata":{"name":"web"},"spec":{"base":{"replicas":0}}}),
            json!({"spec":{"base":{"replicas":9}}}),
        ];
        assert_eq!(
            apps_to_suspend(&apps, "demo", &[]),
            vec![("web".to_string(), 2), ("api".to_string(), 1)],
            "an unnamed app is unpatchable and a repeated one keeps its FIRST count"
        );

        let already = vec![(("demo".to_string(), "web".to_string()), 2)];
        assert_eq!(
            apps_to_suspend(&apps, "demo", &already),
            vec![("api".to_string(), 1)],
            "an app recorded on an earlier pass must not be re-read at replicas 0"
        );
        assert_eq!(
            apps_to_suspend(&apps, "other-ns", &already),
            vec![("web".to_string(), 2), ("api".to_string(), 1)],
            "the record is per (namespace, name) — a same-named app elsewhere still counts"
        );
    }

    /// The namespaces to quiesce come from the manifest's ResourceClaims only,
    /// deduped so a namespace with several claims is listed (and listed for
    /// kubectl) once.
    #[test]
    fn claim_namespaces_dedupes_and_ignores_non_claim_resources() {
        let m = manifest_of(
            &[],
            vec![
                resource("ResourceClaim", "demo", "db"),
                resource("ResourceClaim", "demo", "cache"),
                resource("Application", "other", "web"),
                resource("ResourceClaim", "shop", "db"),
            ],
        );
        assert_eq!(
            claim_namespaces(&m),
            vec!["demo".to_string(), "shop".to_string()]
        );
    }

    /// Per-environment Argo apps are named `<app>-<env>`, so the suspend path
    /// finds them by LABEL — matching on name would miss every environment but
    /// the bare one.
    #[test]
    fn argo_app_selector_matches_by_label_not_by_name() {
        assert_eq!(argo_app_selector("web"), "apprafter.io/application=web");
    }

    /// An Argo item missing either coordinate is dropped: patching it would
    /// need a guessed namespace, and guessing wrong patches someone else's app.
    #[test]
    fn argo_app_refs_drops_items_missing_a_coordinate() {
        let items = vec![
            json!({"metadata":{"namespace":"argocd","name":"web-prod"}}),
            json!({"metadata":{"name":"no-namespace"}}),
            json!({"metadata":{"namespace":"argocd"}}),
        ];
        assert_eq!(
            argo_app_refs(&items),
            vec![("argocd".to_string(), "web-prod".to_string())]
        );
    }

    // =======================================================================
    // Artifact discovery under data/
    // =======================================================================

    /// Only `.dump` files are fed to `pg_restore`; a stray file in the tree
    /// must not be handed to it as if it were an archive. The result is ordered
    /// so a restore replays the same way twice.
    #[test]
    fn discover_pg_dumps_takes_only_dump_files_in_a_stable_order() {
        let dd = tempfile::tempdir().unwrap();
        write_at(dd.path(), "pg/shop/orders.dump", "x");
        write_at(dd.path(), "pg/demo/db.dump", "x");
        write_at(dd.path(), "pg/demo/db.dump.tmp", "x");
        write_at(dd.path(), "pg/demo/README", "x");
        write_at(dd.path(), "pg/loose-file", "x");

        let found: Vec<(String, String)> = discover_pg_dumps(dd.path())
            .into_iter()
            .map(|(ns, claim, _)| (ns, claim))
            .collect();
        assert_eq!(
            found,
            vec![
                ("demo".to_string(), "db".to_string()),
                ("shop".to_string(), "orders".to_string())
            ]
        );
    }

    /// A backup with no pg claim has no `data/pg` — that is a shape, not a
    /// failure, and must not cost a kubectl round-trip either.
    #[test]
    fn discover_pg_dumps_is_empty_without_a_pg_directory() {
        let dd = tempfile::tempdir().unwrap();
        assert!(discover_pg_dumps(dd.path()).is_empty());
    }

    /// Volume and redis artifacts share the `<kind>/<ns>/<name>/<file>` shape.
    /// A directory without the payload file is skipped rather than streamed
    /// into a helper pod as an empty tar.
    #[test]
    fn discover_nested_artifacts_requires_the_payload_file() {
        let dd = tempfile::tempdir().unwrap();
        write_at(dd.path(), "volumes/shop/media/data.tar", "TAR");
        write_at(dd.path(), "volumes/demo/uploads/data.tar", "TAR");
        write_at(dd.path(), "volumes/demo/empty-claim/other.txt", "x");
        write_at(dd.path(), "redis/demo/cache/dump.tar", "RDB");

        let vols = discover_nested_artifacts(dd.path(), "volumes", "data.tar");
        assert_eq!(
            vols,
            vec![
                (
                    "demo".to_string(),
                    "uploads".to_string(),
                    dd.path().join("volumes/demo/uploads/data.tar")
                ),
                (
                    "shop".to_string(),
                    "media".to_string(),
                    dd.path().join("volumes/shop/media/data.tar")
                ),
            ],
            "a claim directory without the payload file is skipped, and the order is stable"
        );

        let redis = discover_nested_artifacts(dd.path(), "redis", "dump.tar");
        assert_eq!(redis.len(), 1);
        assert_eq!(redis[0].1, "cache");
        assert!(discover_nested_artifacts(dd.path(), "pg", "data.tar").is_empty());
    }

    /// `secrets/sourcecred/` is NOT a namespace: `ApplySourceCredentials` has
    /// already re-sealed that material into the namespace its reference names.
    /// Re-sealing it here would seal it under the literal namespace
    /// `sourcecred`, where nothing can decrypt it.
    #[test]
    fn discover_user_secret_files_excludes_the_sourcecred_subtree() {
        let dd = tempfile::tempdir().unwrap();
        write_at(dd.path(), "secrets/demo/app-secrets.json", "{}");
        write_at(dd.path(), "secrets/shop/stripe.json", "{}");
        write_at(dd.path(), "secrets/sourcecred/git-creds.json", "{}");
        write_at(dd.path(), "secrets/demo/notes.txt", "x");
        std::fs::write(dd.path().join("secrets/loose.json"), "{}").unwrap();

        let found: Vec<(String, String)> = discover_user_secret_files(&dd.path().join("secrets"))
            .into_iter()
            .map(|(ns, name, _)| (ns, name))
            .collect();
        assert_eq!(
            found,
            vec![
                ("demo".to_string(), "app-secrets".to_string()),
                ("shop".to_string(), "stripe".to_string())
            ]
        );
    }

    #[test]
    fn discover_user_secret_files_is_empty_without_a_secrets_directory() {
        let dd = tempfile::tempdir().unwrap();
        assert!(discover_user_secret_files(&dd.path().join("secrets")).is_empty());
    }

    // =======================================================================
    // The pg load path
    // =======================================================================

    fn pg_conn() -> PgConnection {
        PgConnection {
            user: "app".into(),
            pass: "s3cret".into(),
            host: "db-rw.demo.svc".into(),
            port: "5432".into(),
            db: "claim_demo_db".into(),
        }
    }

    /// L3: the connection Secret name comes from the REGENERATED claim. A claim
    /// with no `status.connectionSecretRef` has not provisioned, and falling
    /// back to the creds embedded in the backup would aim `pg_restore` at the
    /// cluster that is gone.
    #[test]
    fn connection_secret_name_requires_a_provisioned_claim() {
        let provisioned = json!({"status":{"connectionSecretRef":"db-conn"}});
        assert_eq!(
            connection_secret_name(&provisioned, "demo", "db").unwrap(),
            "db-conn"
        );
        assert!(connection_secret_name(&json!({"status":{}}), "demo", "db").is_err());
    }

    /// Every connection key is REQUIRED, and the missing one is named: a
    /// silently-defaulted host or db would point `pg_restore` at the wrong
    /// database and then report success.
    #[test]
    fn pg_connection_from_secret_requires_every_key_and_names_the_missing_one() {
        let full: BTreeMap<String, Vec<u8>> = [
            ("user", "app"),
            ("pass", "s3cret"),
            ("host", "db-rw"),
            ("port", "5432"),
            ("db", "claim_demo_db"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.as_bytes().to_vec()))
        .collect();
        let conn = pg_connection_from_secret(&full, "demo", "db-conn").unwrap();
        assert_eq!(conn.user, "app");
        assert_eq!(conn.db, "claim_demo_db");

        let mut missing = full.clone();
        missing.remove("db");
        // `.err()` rather than `unwrap_err()`: PgConnection deliberately has no
        // Debug impl, because it carries the database password.
        let e = pg_connection_from_secret(&missing, "demo", "db-conn")
            .err()
            .expect("an incomplete connection Secret must not resolve");
        assert!(format!("{e}").contains("missing key `db`"), "got: {e}");
    }

    /// The restore argv: `--clean --if-exists` because the fresh database is
    /// not necessarily empty, `--no-owner` because the regenerated claim owns a
    /// DIFFERENT generated role than the dump names, and every connection
    /// parameter from the fresh Secret.
    #[test]
    fn pg_restore_argv_is_the_clean_no_owner_restore_of_the_fresh_connection() {
        assert_eq!(
            pg_restore_argv(&pg_conn()),
            vec![
                "pg_restore",
                "--no-owner",
                "--clean",
                "--if-exists",
                "-h",
                "db-rw.demo.svc",
                "-p",
                "5432",
                "-U",
                "app",
                "-d",
                "claim_demo_db",
            ]
        );
    }

    /// `PGPASSWORD` must reach the helper pod's environment, or `pg_restore`
    /// prompts for a password on a pod with no TTY and the restore hangs until
    /// the user gives up.
    #[test]
    fn pg_helper_pod_spec_injects_the_password_into_the_container_env() {
        let spec = pg_helper_pod_spec("ld-pg-db", "demo", "postgres:18", "s3cret");
        assert_eq!(spec["metadata"]["name"], "ld-pg-db");
        assert_eq!(spec["metadata"]["namespace"], "demo");
        assert_eq!(spec["spec"]["containers"][0]["image"], "postgres:18");
        assert_eq!(
            spec["spec"]["containers"][0]["env"][0]["name"],
            "PGPASSWORD"
        );
        assert_eq!(spec["spec"]["containers"][0]["env"][0]["value"], "s3cret");
    }

    /// The reachability probe runs `psql -d <db>`, NOT `pg_isready`: on a
    /// lazily-provisioned shared CNPG cluster `pg_isready` reports "up" before
    /// the per-claim database exists, and `pg_restore` then dies with
    /// `FATAL: database "…" does not exist`.
    #[test]
    fn psql_probe_args_query_the_claim_database_itself() {
        let conn = pg_conn();
        assert_eq!(
            psql_probe_args("ld-pg-db", "demo", &conn),
            vec![
                "exec",
                "ld-pg-db",
                "-n",
                "demo",
                "--",
                "psql",
                "-h",
                "db-rw.demo.svc",
                "-p",
                "5432",
                "-U",
                "app",
                "-d",
                "claim_demo_db",
                "-tAc",
                "SELECT 1",
            ]
        );
    }

    /// The probe retries a bounded number of times and then fails carrying the
    /// LAST probe output — the DB-side reason is the only thing that makes the
    /// timeout actionable.
    #[test]
    fn poll_pg_reachable_retries_then_surfaces_the_last_probe_error() {
        let conn = pg_conn();
        let mut calls = 0;
        poll_pg_reachable(5, std::time::Duration::ZERO, &conn, &mut || {
            calls += 1;
            if calls < 3 {
                Err("connection refused".into())
            } else {
                Ok(())
            }
        })
        .unwrap();
        assert_eq!(calls, 3, "polling must stop at the first success");

        let mut attempts = 0;
        let e = poll_pg_reachable(4, std::time::Duration::ZERO, &conn, &mut || {
            attempts += 1;
            Err(format!("FATAL: database does not exist (try {attempts})"))
        })
        .unwrap_err();
        assert_eq!(attempts, 4, "the whole budget must be spent before failing");
        let msg = format!("{e}");
        assert!(
            msg.contains("FATAL: database does not exist (try 4)"),
            "got: {msg}"
        );
        assert!(msg.contains("claim_demo_db"), "got: {msg}");
    }

    /// The full one-claim pg load: stand the helper up, wait for the database,
    /// stream the dump into `pg_restore` on the pod's stdin, and delete the
    /// helper pod afterwards.
    #[test]
    fn run_pg_restore_streams_the_dump_and_cleans_the_helper_pod_up() {
        let k = FakeKube::default();
        let dump = tempfile::NamedTempFile::new().unwrap();
        let probed = RefCell::new(Vec::new());

        run_pg_restore(
            "demo",
            "db",
            &pg_conn(),
            dump.path(),
            &k,
            "postgres:18",
            &|pod| {
                probed.borrow_mut().push(pod.to_string());
                Ok(())
            },
        )
        .unwrap();

        assert_eq!(
            *probed.borrow(),
            vec!["ld-pg-db".to_string()],
            "the probe runs in the helper pod"
        );
        assert_eq!(k.applied.borrow().len(), 1);
        let execs = k.execs.borrow();
        assert_eq!(execs[0].0, "ld-pg-db");
        assert_eq!(execs[0].1, "demo");
        assert_eq!(execs[0].2[0], "pg_restore");
        assert_eq!(
            execs[0].3,
            dump.path(),
            "the dump is streamed from the artifact on disk"
        );
        assert_eq!(
            *k.deleted.borrow(),
            vec![("ld-pg-db".to_string(), "demo".to_string())]
        );
    }

    /// The helper pod is deleted even when the load FAILS — a leaked pod holds
    /// a PVC and blocks the retry the user is about to attempt.
    #[test]
    fn run_pg_restore_deletes_the_helper_pod_on_the_failure_path_too() {
        let k = FakeKube::failing_exec();
        let dump = tempfile::NamedTempFile::new().unwrap();
        let r = run_pg_restore(
            "demo",
            "db",
            &pg_conn(),
            dump.path(),
            &k,
            "postgres:18",
            &|_| Ok(()),
        );
        assert!(r.is_err());
        assert_eq!(
            *k.deleted.borrow(),
            vec![("ld-pg-db".to_string(), "demo".to_string())]
        );
    }

    /// A probe that never succeeds aborts the load BEFORE `pg_restore` runs —
    /// restoring into a database that is not there would fail halfway and leave
    /// a partial schema behind.
    #[test]
    fn run_pg_restore_does_not_stream_when_the_database_never_answers() {
        let k = FakeKube::default();
        let dump = tempfile::NamedTempFile::new().unwrap();
        let r = run_pg_restore(
            "demo",
            "db",
            &pg_conn(),
            dump.path(),
            &k,
            "postgres:18",
            &|_| Err(CliError::Other("unreachable".into())),
        );
        assert!(r.is_err());
        assert!(
            k.execs.borrow().is_empty(),
            "no dump may be streamed into an unreachable db"
        );
        assert_eq!(k.deleted.borrow().len(), 1);
    }

    // =======================================================================
    // The volume + redis load paths
    // =======================================================================

    /// L1: the load helper mounts the PVC READ-WRITE (a read-only mount cannot
    /// receive the tar) and untars into `/data`.
    #[test]
    fn load_one_volume_mounts_read_write_and_untars_into_the_mount() {
        let k = FakeKube::default();
        let tar = tempfile::NamedTempFile::new().unwrap();

        load_one_volume("demo", "uploads", "pvc-uploads", tar.path(), &k).unwrap();

        let applied = k.applied.borrow();
        let spec = &applied[0];
        assert_eq!(spec["metadata"]["name"], "ld-vol-uploads");
        assert_eq!(
            spec["spec"]["volumes"][0]["persistentVolumeClaim"]["claimName"],
            "pvc-uploads"
        );
        assert_eq!(
            spec["spec"]["volumes"][0]["persistentVolumeClaim"]["readOnly"], false,
            "a read-only mount cannot receive the restored tree"
        );
        let execs = k.execs.borrow();
        assert_eq!(execs[0].2, vec!["tar", "x", "-C", "/data"]);
        assert_eq!(execs[0].3, tar.path());
        assert_eq!(
            *k.deleted.borrow(),
            vec![("ld-vol-uploads".to_string(), "demo".to_string())]
        );
    }

    /// A Dragonfly snapshot is replayed into the RUNNING instance's pod-0 in
    /// `dragonfly-system`, using the instance's ADMIN password on the data port
    /// — the admin port refuses `DFLY LOAD`.
    #[test]
    fn restore_one_redis_instance_replays_into_the_running_pod_zero() {
        let k =
            FakeKube::default().with_secret("pool-a-admin", "dragonfly-system", "password", "pw");
        let tar = tempfile::NamedTempFile::new().unwrap();

        restore_one_redis_instance("pool-a", tar.path(), &k).unwrap();

        let execs = k.execs.borrow();
        assert_eq!(execs[0].0, "pool-a-0");
        assert_eq!(execs[0].1, "dragonfly-system");
        assert_eq!(execs[0].2[0], "sh");
        assert_eq!(execs[0].3, tar.path());
        assert!(
            k.applied.borrow().is_empty(),
            "no helper pod and no scale — the provisioner must not see the instance vanish"
        );
    }

    /// A missing admin Secret must abort the load rather than exec an
    /// unauthenticated `DFLY LOAD` that silently does nothing.
    #[test]
    fn restore_one_redis_instance_fails_when_the_admin_password_is_absent() {
        let k = FakeKube::default();
        let tar = tempfile::NamedTempFile::new().unwrap();
        assert!(restore_one_redis_instance("pool-a", tar.path(), &k).is_err());
        assert!(k.execs.borrow().is_empty());
    }

    /// The replay script must (a) check the reply, because `redis-cli` exits 0
    /// even when the server answers with an error — without the check a failed
    /// load reports a successful restore over an empty instance — and (b) quote
    /// the generated password, which may contain shell metacharacters.
    #[test]
    fn dfly_load_script_checks_the_reply_and_quotes_the_password() {
        let script = dfly_load_script("pa's$word");
        assert!(script.contains("'pa'\\''s$word'"), "got: {script}");
        assert!(script.contains("DFLY LOAD"), "got: {script}");
        assert!(
            script.contains("[ \"$OUT\" = OK ]"),
            "a redis-cli exit code is not evidence the load worked: {script}"
        );
        assert!(script.starts_with("set -e"), "got: {script}");
    }

    // =======================================================================
    // Waiting for the cluster
    // =======================================================================

    /// R1: readiness is the claim's OWN `status.ready`. Waiting for PVC `Bound`
    /// would deadlock, because the load helper is the FIRST consumer of that
    /// PVC. An absent claim or an absent status counts as not-ready.
    #[test]
    fn wait_claims_ready_with_polls_each_claim_until_its_status_says_ready() {
        let claims = [resource("ResourceClaim", "demo", "db")];
        let refs: Vec<&ResourceRef> = claims.iter().collect();
        let mut polls = 0;

        wait_claims_ready_with(&refs, 5, std::time::Duration::ZERO, &mut |ns, name| {
            polls += 1;
            assert_eq!((ns, name), ("demo", "db"));
            Ok(match polls {
                1 => None,
                2 => Some(json!({"status":{}})),
                3 => Some(json!({"status":{"ready":false}})),
                _ => Some(json!({"status":{"ready":true}})),
            })
        })
        .unwrap();
        assert_eq!(polls, 4, "only an explicit ready:true ends the wait");
    }

    /// A claim that never provisions fails the restore LOUDLY — loading into a
    /// backend that is not there is the failure mode this whole step exists to
    /// prevent.
    #[test]
    fn wait_claims_ready_with_gives_up_after_the_budget() {
        let claims = [resource("ResourceClaim", "demo", "db")];
        let refs: Vec<&ResourceRef> = claims.iter().collect();
        let mut polls = 0;
        let e = wait_claims_ready_with(&refs, 3, std::time::Duration::ZERO, &mut |_, _| {
            polls += 1;
            Ok(Some(json!({"status":{"ready":false}})))
        })
        .unwrap_err();
        assert_eq!(polls, 3);
        assert!(format!("{e}").contains("did not become ready"), "got: {e}");
    }

    /// Only ResourceClaims are waited on: the manifest's resource list also
    /// carries the config CRs, and polling those for `status.ready` would spin
    /// until the timeout.
    #[test]
    fn claims_to_wait_for_filters_the_manifest_to_resource_claims() {
        let m = manifest_of(
            &[],
            vec![
                resource("Application", "demo", "web"),
                resource("ResourceClaim", "demo", "db"),
            ],
        );
        let claims = claims_to_wait_for(&m);
        assert_eq!(claims.len(), 1);
        assert_eq!(claims[0].name, "db");
    }

    /// A required `status` field of a regenerated claim, with the field named
    /// in the error — the fresh value is the only correct one, so an absent
    /// field must never fall back to the backed-up coordinate.
    #[test]
    fn claim_status_field_reads_the_fresh_value_or_names_what_is_missing() {
        let claim = json!({"status":{"volumeClaimRef":"pvc-uploads"}});
        assert_eq!(
            claim_status_field(
                &claim,
                "/status/volumeClaimRef",
                "volumeClaimRef",
                "demo",
                "uploads"
            )
            .unwrap(),
            "pvc-uploads"
        );
        let e = claim_status_field(&claim, "/status/instance", "instance", "demo", "cache")
            .unwrap_err();
        assert!(
            format!("{e}").contains("has no status.instance"),
            "got: {e}"
        );
    }

    /// The PlatformStack apply races the admission webhook's Endpoints right
    /// after a bootstrap, so it retries — and when the budget runs out it
    /// surfaces the LAST error rather than a generic timeout.
    #[test]
    fn apply_with_retry_retries_a_failing_apply_then_returns_the_last_error() {
        let mut calls = 0;
        apply_with_retry(5, std::time::Duration::ZERO, &mut |attempt| {
            calls += 1;
            assert_eq!(attempt, calls);
            if calls < 3 {
                Err(CliError::Other("no endpoints available".into()))
            } else {
                Ok(())
            }
        })
        .unwrap();
        assert_eq!(calls, 3, "retrying stops at the first success");

        let mut attempts = 0;
        let e = apply_with_retry(3, std::time::Duration::ZERO, &mut |_| {
            attempts += 1;
            Err(CliError::Other(format!("webhook down ({attempts})")))
        })
        .unwrap_err();
        assert_eq!(attempts, 3);
        assert!(format!("{e}").contains("webhook down (3)"), "got: {e}");
    }

    // =======================================================================
    // restic invocation results
    // =======================================================================

    /// A non-zero restic exit MUST become an error: the steps that follow read
    /// the restored tree off disk, so a swallowed failure leaves an empty tree
    /// and reports a successful restore over nothing.
    #[test]
    fn restic_output_to_result_yields_stdout_or_an_error_carrying_stderr() {
        use std::os::unix::process::ExitStatusExt;
        let argv = vec!["snapshots".to_string(), "--json".to_string()];

        let ok = std::process::Output {
            status: std::process::ExitStatus::from_raw(0),
            stdout: b"[{\"id\":\"abc\"}]".to_vec(),
            stderr: Vec::new(),
        };
        assert_eq!(
            restic_output_to_result(&argv, &ok).unwrap(),
            "[{\"id\":\"abc\"}]"
        );

        let failed = std::process::Output {
            status: std::process::ExitStatus::from_raw(1 << 8), // exit code 1
            stdout: Vec::new(),
            stderr: b"wrong password".to_vec(),
        };
        let e = restic_output_to_result(&argv, &failed).unwrap_err();
        let msg = format!("{e}");
        assert!(
            msg.contains("wrong password"),
            "stderr must reach the user: {msg}"
        );
        assert!(
            msg.contains("snapshots"),
            "the failing subcommand must be named: {msg}"
        );
    }
}
