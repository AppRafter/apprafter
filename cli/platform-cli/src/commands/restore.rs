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
//! Both modes hold every workload at zero replicas for the middle of the run
//! and only resume it in the last step, so a run that STOPS partway leaves a
//! deliberately-down cluster. Two things exist for that: each suspended app
//! carries its pre-restore replica count in the
//! [`PRE_RESTORE_REPLICAS_ANNOTATION`] (so the count outlives the process and
//! a re-run cannot mistake the zero it wrote for the app's real size), and
//! [`interrupted_restore_lines`] names what was left down on the way out.
//!
//! `--reprovision` (mode a) provisions a FRESH cluster in the target first
//! (`bootstrap_all::run`), then replays as restore-into-running.
//!
//! Every mode that replays the `PlatformStack` — that is, every mode except
//! `--data-only` — lands the source's whole `spec.backup` on the target, so the
//! restored cluster can start writing to the SOURCE's repository. Whether it
//! should is a question only the operator can answer, so when the replayed
//! block is enabled the restore ASKS: `--keep-backup-schedule` /
//! `--discard-backup-schedule` answer it up front, a terminal is prompted, and
//! a non-interactive run without either flag is refused. See
//! [`schedule_decision`] and [`apply_backup_schedule_policy`].

use std::collections::BTreeMap;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};

use crate::commands::backup::KubectlExec;
use backup_core::helper_pod::{pg_helper_pod_spec, volume_pod_spec};
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

/// Annotation `SuspendWorkloads` stamps on an AppRafter `Application` carrying
/// the replica count it had BEFORE the restore scaled it to zero — in the SAME
/// merge-patch that scales it, so the record cannot lag the write it describes.
///
/// WHY IT IS ON THE OBJECT (H5). `--data-only` reads the count to resume to off
/// the LIVE Application, and kept it only in a local `Vec`. A run that scaled an
/// app to 0 and then died at `LoadData` — a timeout, a severed connection, a
/// Ctrl-C — left that vec with the process; the obvious re-run then read the
/// live count, which was now the `0` the first run had written, "resumed" to 0
/// and reported success over an application that stayed down. The in-run guard
/// in [`apps_to_suspend`] cannot see across two processes; an annotation can,
/// because it lives where the damage does. It is also readable by hand, which
/// is what makes an ABANDONED restore recoverable at all.
///
/// `pub(crate)` since ADR 0064: `app restart` refuses on a workload at
/// zero replicas and names this annotation as the cause when it is
/// present. Sharing the constant rather than re-typing the key is what
/// keeps the refusal pointing at the annotation this module actually
/// writes — a second spelling would go stale silently, and it would go
/// stale on the one message whose job is to explain an otherwise
/// inexplicable zero.
pub(crate) const PRE_RESTORE_REPLICAS_ANNOTATION: &str = "apprafter.io/pre-restore-replicas";

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
#[derive(Debug)]
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
#[allow(clippy::too_many_arguments)]
fn restore_summary(
    manifest: Option<&BackupManifest>,
    target: Option<&str>,
    data_only: bool,
    resumed: usize,
    version_warning: Option<&str>,
    inherited_cluster_name: Option<&str>,
    schedule: BackupSchedule,
    edge: &EdgeRestore,
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
    lines.extend(backup_inheritance_lines(schedule, inherited_cluster_name));
    lines.extend(edge_inheritance_lines(edge));
    if let Some(w) = version_warning {
        lines.push(format!("  ⚠ {w}"));
    }
    lines
}

/// What a FAILED or interrupted restore says on its way out: which
/// applications it left down, whose Argo CD auto-sync it left off, and what to
/// do about it. Pure — printed to stderr just before the error itself.
///
/// WHY. Both restore modes hold every workload at zero replicas for the middle
/// of the run and only resume it in the last step. A run that dies before that
/// step is therefore a dead cluster BY DESIGN, and the operator staring at it
/// had no way to tell that from a cluster the restore broke. Naming the apps is
/// most of the answer; the rest is saying that re-running is the way forward.
///
/// It says plainly that there is NO resume. The restore has no checkpoints and
/// no `--continue`: a re-run replays every step from the first, re-fetching the
/// snapshot and re-loading the data. Implying otherwise would be worse than
/// saying nothing, because an operator who believes work is being skipped will
/// not budget the time for it.
///
/// Empty when the run wrote nothing — a failure at `RestoreArtifact` has left
/// no state to describe, and the error alone is the whole story.
fn interrupted_restore_lines(
    data_only: bool,
    suspended_apps: &[((String, String), i64)],
    suspended_argo: &[(String, String)],
) -> Vec<String> {
    if suspended_apps.is_empty() && suspended_argo.is_empty() {
        return Vec::new();
    }

    let mut lines = vec![
        String::new(),
        "✗ The restore stopped before it finished, and it did not undo what it had already done."
            .to_string(),
    ];

    if !suspended_apps.is_empty() {
        lines.push(if data_only {
            format!(
                "  {} application(s) were scaled to 0 replicas for the load and are still down:",
                suspended_apps.len()
            )
        } else {
            format!(
                "  {} application(s) were applied from the backup at 0 replicas — a restore \
                 holds every workload down until its last step — and are still down:",
                suspended_apps.len()
            )
        });
        for ((ns, name), replicas) in suspended_apps {
            lines.push(format!(
                "    - {ns}/{name} ({replicas} replica(s) when it comes back up)"
            ));
        }
    }

    if !suspended_argo.is_empty() {
        lines.push(format!(
            "  Argo CD auto-sync is switched OFF on {} Application(s), so GitOps will not put \
             any of this back on its own:",
            suspended_argo.len()
        ));
        for (ns, name) in suspended_argo {
            lines.push(format!("    - {ns}/{name}"));
        }
    }

    lines.push(
        "  Re-running the SAME command is the way to continue: its last step is the one that \
         puts the replica counts and auto-sync back."
            .to_string(),
    );
    lines.push(
        "  There is no resume — the restore has no checkpoints and no --continue, so a re-run \
         replays EVERY step from the first, including re-fetching the snapshot and re-loading \
         the data."
            .to_string(),
    );
    lines.push(if data_only {
        format!(
            "  The counts above are recorded on the applications themselves (the \
             {PRE_RESTORE_REPLICAS_ANNOTATION} annotation), so a re-run resumes each app to its \
             real count and not to the 0 this run left behind."
        )
    } else {
        "  The counts above come from the backup, so a re-run reads them from the artifact again \
         and does not depend on anything this run remembered."
            .to_string()
    });
    lines.push("  To put them back by hand instead, without finishing the restore:".to_string());
    for ((ns, name), replicas) in suspended_apps {
        lines.push(format!(
            "    kubectl -n {ns} patch applications.apprafter.io {name} --type=merge -p '{}'",
            resume_patch_body(*replicas)
        ));
    }
    for (ns, name) in suspended_argo {
        lines.push(format!(
            "    kubectl -n {ns} patch applications.argoproj.io {name} --type=merge -p '{}'",
            argo_autosync_patch_body(true)
        ));
    }
    lines
}

/// What the summary says about the backup configuration this restore
/// inherited: the schedule first, then the name it is written under. Pure.
///
/// The two belong together and are written together. They are ONE fact about
/// the replayed `spec.backup` seen from two sides — whether this cluster will
/// write to the source's repository, and what its rows will be labelled when
/// it does — so the name line is phrased against whatever the schedule line
/// just said rather than restating an inheritance warning from scratch.
fn backup_inheritance_lines(
    schedule: BackupSchedule,
    inherited_cluster_name: Option<&str>,
) -> Vec<String> {
    let mut lines = Vec::new();
    match schedule {
        BackupSchedule::NotInherited => {}
        // Stated as a thing that was DONE, not a thing that was withheld: the
        // schedule is in the cluster, complete, and one command away from
        // running.
        BackupSchedule::Disabled => lines.push(
            "  ⚠ the source's backup schedule came with the restore and was left DISABLED, as \
             you asked. It points at the source's repository, and nothing here can tell whether \
             that cluster is still alive and writing to it. Bucket, credential, schedule, \
             timezone and retention are restored exactly as captured, so `apprafter backup set \
             enabled true` turns it back on unchanged — or restore with `--keep-backup-schedule` \
             to inherit it already enabled."
                .to_string(),
        ),
        // Asked for explicitly — at the prompt or with the flag — and still
        // said out loud: the answer was given before the restore ran, and this
        // is the line that records what it did.
        BackupSchedule::KeptEnabled => lines.push(
            "  ⚠ the source's backup schedule was restored ENABLED, as you asked, so this \
             cluster now backs up to the source's repository on the source's schedule. If the \
             source cluster is still running and still backing up, both are now writing to that \
             one repository. `apprafter backup status` shows where and when; `apprafter backup \
             disable` stops it."
                .to_string(),
        ),
    }
    // The one part of the backup config that is VISIBLE in the repository and
    // is replayed verbatim. Said out loud because the alternative is an
    // operator finding two clusters listed under one name and having no idea
    // why. Its snapshots are still attributed correctly — the identity is the
    // kube-system UID, which this cluster has its own of — so this is a
    // labelling matter, and the line says so and says how to change it.
    if let Some(name) = inherited_cluster_name {
        let when = match schedule {
            BackupSchedule::Disabled => "once you enable it",
            _ => "from now on",
        };
        lines.push(format!(
            "    The backup cluster-name '{name}' came with it, so {when} this cluster's \
             snapshots are listed under it too. They are still told apart by this cluster's own \
             identity — the name is only a label. Rename with `apprafter backup set cluster-name \
             <name>`."
        ));
    }
    lines
}

/// What this restore did about the source cluster's EDGE configuration — the
/// certificate its domains are served from, and its origin firewall.
///
/// The two belong with the backup-inheritance block above and are reported in
/// the same paragraph. They are the same kind of fact: things that came with
/// the snapshot, that the operator did not ask for by name, and that decide
/// whether the restored site actually answers. Both used to be silent, and
/// both failures look identical from outside — after a restore the site does
/// not work.
#[derive(Debug, Default, PartialEq, Eq)]
struct EdgeRestore {
    /// Imported TLS certificates re-applied from the snapshot (A1).
    certs_restored: usize,
    /// Certificate names the restored domains reference that the snapshot did
    /// NOT carry — every backup taken before the certificate was captured has
    /// this shape, and it is exactly the dangling Gateway reference (A1/A2).
    dangling_certs: Vec<String>,
    /// What happened to the source's origin-firewall intent (A4).
    origin_firewall: OriginFirewall,
}

/// What a restore did with the origin-firewall intent the snapshot recorded
/// (A4).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
enum OriginFirewall {
    /// Nothing to carry and nothing to say: the snapshot recorded OFF, or
    /// recorded nothing at all (an in-cluster run, or one taken before the
    /// field existed), or the destination target already has the toggle on.
    #[default]
    Nothing,
    /// Recorded ON, and this restore provisioned the node — so the intent was
    /// written onto the named target and its live firewall reconciled.
    Carried { target: String },
    /// Recorded ON, but the carry did not complete. The ports are still open
    /// and the line says so.
    ///
    /// `recorded` is whether the toggle nonetheless reached the target's
    /// config — the carry persists the intent BEFORE it touches the cloud, so
    /// a failed reconcile usually leaves it on disk, and a failure to resolve
    /// or write the target does not. The distinction is checked rather than
    /// assumed, because "target 'X' records it" decides whether the operator's
    /// next `apprafter apply` fixes this on its own.
    CarriedNotEnforced {
        target: String,
        why: String,
        recorded: bool,
    },
    /// Recorded ON, but this restore provisioned no node. The replayed
    /// `PlatformStack` carries the intent into THIS cluster's CR — so its own
    /// next backup keeps it — but no cloud firewall was touched: a cluster
    /// that was already running has whatever firewall it has, and writing the
    /// target's config here would claim something this restore never made
    /// true.
    Announced,
}

/// What the summary says about the edge configuration. Pure.
///
/// Phrased to continue [`backup_inheritance_lines`] rather than to open a new
/// topic: each line names something of the SOURCE's that came with the restore
/// (or did not), in the same shape, because to the operator they are one
/// paragraph — "here is what you inherited, and here is what it means".
fn edge_inheritance_lines(edge: &EdgeRestore) -> Vec<String> {
    let mut lines = Vec::new();

    // Stated even though it is good news: the certificate is the half of a
    // connected domain that a backup used to drop, and an operator who has
    // been bitten by that needs to see that this restore did not.
    if edge.certs_restored > 0 {
        let n = edge.certs_restored;
        lines.push(format!(
            "  ✓ the source's imported TLS certificate(s) came with the restore — {n} re-applied \
             to apprafter-system, so the domains its PlatformStack brought back are served by the \
             same certificate as before. `apprafter target domain list` shows the zones and the \
             certificate each of them is served from."
        ));
    }

    if !edge.dangling_certs.is_empty() {
        lines.push(format!(
            "  ⚠ the restored domains name certificate(s) this snapshot does not carry: {}. A \
             backup taken before imported certificates were captured looks exactly like this — \
             the material never left the source cluster, so the platform Gateway has nothing to \
             terminate TLS with and the domains will not serve. Re-import with `apprafter target \
             cert import <name> --cert <file> --key <file>`; the domains are already registered, \
             so there is no `target domain add` to repeat. `apprafter target domain list` marks \
             the reference MISSING until then.",
            edge.dangling_certs.join(", ")
        ));
    }

    match &edge.origin_firewall {
        OriginFirewall::Nothing => {}
        OriginFirewall::Carried { target } => lines.push(format!(
            "  ✓ the source's Cloudflare origin firewall came with the restore — target \
             '{target}' records it now, and the node this restore provisioned has its 80/443 \
             restricted to Cloudflare's IP ranges rather than open to the internet. Nothing else \
             on that target changed; `apprafter target firewall cloudflare-origin disable` \
             re-opens them."
        )),
        OriginFirewall::CarriedNotEnforced {
            target,
            why,
            recorded,
        } => lines.push(format!(
            "  ⚠ the source's Cloudflare origin firewall came with the restore, but {}: {why}. \
             The node's 80/443 are still open to the internet.{} To close them now, `apprafter \
             target use {target}` then `apprafter target firewall cloudflare-origin enable`.",
            if *recorded {
                format!(
                    "target '{target}' records it and the live firewall could not be reconciled"
                )
            } else {
                format!("it could not be applied to target '{target}' at all")
            },
            if *recorded {
                " The recorded toggle applies on the next `apprafter apply`."
            } else {
                ""
            },
        )),
        OriginFirewall::Announced => lines.push(
            "  ⚠ the snapshot recorded the source's Cloudflare origin firewall as ON, and the \
             restored PlatformStack now records it here too. This restore provisioned no node, \
             so no firewall was changed — this cluster's 80/443 are whatever they already were. \
             `apprafter target firewall cloudflare-origin enable` restricts them to Cloudflare's \
             IP ranges."
                .to_string(),
        ),
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
    keep_backup_schedule: bool,
    discard_backup_schedule: bool,
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
    // What came back of the source's edge configuration — its imported TLS
    // certificate and its origin firewall (A1/A4). Reported in the summary
    // alongside the backup-config inheritance.
    let mut edge = EdgeRestore::default();
    // The source's `spec.backup.clusterName`, when the replayed PlatformStack
    // carried an enabled schedule — reported in the summary (E1).
    let mut inherited_cluster_name: Option<String> = None;
    // What ApplyPlatformStack did to the replayed schedule (D1/D2). Stays
    // `NotInherited` on `--data-only`, which replays no CR at all — so that
    // mode neither prompts nor refuses, because it never reaches the step that
    // asks.
    let mut schedule = BackupSchedule::NotInherited;
    let schedule_flags = ScheduleFlags {
        keep: keep_backup_schedule,
        discard: discard_backup_schedule,
        is_tty,
    };

    let snap = snapshot.unwrap_or("latest");

    // Every step runs inside this closure so the whole run has ONE exit: the
    // interruption hint below then sees the partial state the steps already
    // wrote into the cluster, whichever of them failed.
    //
    // THE ERROR PATH ONLY — signals are deliberately NOT handled. A default
    // Ctrl-C kills the process without unwinding, so neither this hint nor any
    // `Drop` guard runs. A SIGINT handler could print the same lines, but it
    // could not safely undo a half-issued kubectl from another thread, so it
    // would buy a message and a second exit path. The durable half of the
    // answer is the [`PRE_RESTORE_REPLICAS_ANNOTATION`] instead: it is in the
    // cluster before the scale-to-zero it describes, so the recorded count
    // survives a signal, a severed connection and a kill -9 alike, and the
    // re-run recovers with or without anything having been printed.
    let outcome = (|| -> Result<()> {
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
                    // The TARGET's machine key, so `latest` cannot silently resolve
                    // to a co-tenant cluster's run in a shared repository (E2).
                    // Best-effort: a target that cannot name itself falls back to
                    // the "one cluster in the repo, or refuse" rule, which is the
                    // half that still refuses to guess.
                    let this_uid = crate::commands::backup::read_cluster_uid(kc.path()).ok();
                    let dd = restore_artifact_tree(
                        &SubprocessRestic {
                            repo,
                            pass: &pass,
                            creds: &creds,
                        },
                        snap,
                        restore_root.path(),
                        this_uid.as_deref(),
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
                RestoreStep::ApplyImportedCerts => {
                    let dd = produced_by_artifact(data_dir.as_ref(), "ApplyImportedCerts")?;
                    let (restored, dangling) = apply_imported_certs(dd, kc.path())?;
                    edge.certs_restored = restored;
                    edge.dangling_certs = dangling;
                }
                RestoreStep::ApplyPlatformStack => {
                    let dd = produced_by_artifact(data_dir.as_ref(), "ApplyPlatformStack")?;
                    let replay = apply_platformstack_from_crs(dd, kc.path(), schedule_flags)?;
                    inherited_cluster_name = replay.inherited_cluster_name;
                    schedule = replay.schedule;

                    // A4, at the first moment anything can know: the source's
                    // origin-firewall intent travels in the CR this step just
                    // applied, so it is readable here and nowhere earlier. On
                    // `--reprovision` the node is already up — every minute
                    // between the provision at step 1 and this line is a minute
                    // with 80/443 open to the internet, which is why this sits
                    // immediately after the apply rather than at the end of the
                    // run.
                    edge.origin_firewall =
                        settle_origin_firewall(replay.origin_firewall, reprovision, target);
                }
                RestoreStep::EnsureNamespaces => {
                    let m = produced_by_artifact(manifest.as_ref(), "EnsureNamespaces")?;
                    ensure_namespaces_all(&m.namespaces, &m.secret_namespaces, kc.path())?;
                }
                RestoreStep::ApplySourceCredentials => {
                    let dd = produced_by_artifact(data_dir.as_ref(), "ApplySourceCredentials")?;
                    apply_source_credentials(dd, kc.path())?;
                }
                RestoreStep::ApplyAppsGated => {
                    let dd = produced_by_artifact(data_dir.as_ref(), "ApplyAppsGated")?;
                    apply_apps_gated(dd, kc.path(), &mut suspended_argo, &mut app_replicas)?;
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
                    suspend_running_workloads(
                        m,
                        kc.path(),
                        &mut suspended_argo,
                        &mut app_replicas,
                    )?;
                }
                RestoreStep::ResumeWorkloads => {
                    resume_workloads(&app_replicas, &suspended_argo, kc.path())?;
                }
            }
        }
        Ok(())
    })();

    // A restore that stopped partway leaves the cluster mid-restore, and until
    // now said nothing about it: the operator saw one error and a dead
    // application. Name what is down and what puts it back, on the way out.
    if let Err(err) = outcome {
        for line in interrupted_restore_lines(data_only, &app_replicas, &suspended_argo) {
            eprintln!("{line}");
        }
        return Err(err);
    }

    for line in restore_summary(
        manifest.as_ref(),
        target,
        data_only,
        app_replicas.len(),
        version_warning.as_deref(),
        inherited_cluster_name.as_deref(),
        schedule,
        &edge,
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
    this_cluster_uid: Option<&str>,
) -> Result<PathBuf> {
    let listing = restic.snapshots_json()?;
    let run =
        backup_core::restore::resolve_run_snapshots(&listing, requested_snapshot, this_cluster_uid)
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
///
/// Every object is type-stamped on the way out (see [`stamp_cr_type`]) so the
/// callers that apply it hand `kubectl` a well-formed document — snapshots
/// written by the in-cluster runner before the capture-side fix carry no
/// `apiVersion`/`kind` at all.
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
        let mut cr: Value = serde_json::from_slice(&body)
            .map_err(|e| CliError::Other(format!("parse CR {}: {e}", path.display())))?;
        stamp_cr_type(&mut cr, &kind, &path)?;
        out.push(LoadedCr { kind, cr });
    }
    Ok(out)
}

/// The `(apiVersion, kind)` a staged CR must be applied as, keyed on the
/// backup's own kind tag — the 2nd filename segment `write_crs` puts there.
///
/// `ArgoApplication` is the backup's INTERNAL tag for a user Argo CD
/// `Application`: the tag distinguishes it from an AppRafter `Application` in
/// the same directory, but the object itself is `argoproj.io/v1alpha1`,
/// `kind: Application` — which is how `gated_apply_plan` applies it and how
/// `cluster_bootstrap` writes one. Everything else the capture sweep stages
/// (`capture_non_claim_artifacts`) is an `apprafter.io/v1alpha1` CR whose kind
/// IS the tag.
fn known_cr_type(kind_tag: &str) -> Option<(&'static str, &str)> {
    match kind_tag {
        "ArgoApplication" => Some(("argoproj.io/v1alpha1", "Application")),
        "Application" | "PlatformStack" | "SharedVolume" | "SourceCredential" => {
            Some(("apprafter.io/v1alpha1", kind_tag))
        }
        _ => None,
    }
}

/// True iff `obj` already states its own type — both fields present as
/// non-empty strings.
fn carries_object_type(obj: &Value) -> bool {
    ["apiVersion", "kind"].iter().all(|f| {
        obj.get(f)
            .and_then(Value::as_str)
            .is_some_and(|v| !v.is_empty())
    })
}

/// Fill in whichever of `apiVersion` / `kind` the staged object is missing.
/// Never overwrites one the body carries.
fn fill_object_type(obj: &mut Value, api_version: &str, kind: &str, path: &Path) -> Result<()> {
    let map = obj.as_object_mut().ok_or_else(|| {
        CliError::Other(format!(
            "backup object {} is not a JSON object, so it cannot be applied",
            path.display()
        ))
    })?;
    for (field, value) in [("apiVersion", api_version), ("kind", kind)] {
        let present = map
            .get(field)
            .and_then(Value::as_str)
            .is_some_and(|v| !v.is_empty());
        if !present {
            map.insert(field.to_string(), Value::from(value));
        }
    }
    Ok(())
}

/// Give a staged CR back its `apiVersion` + `kind` when the snapshot does not
/// carry them.
///
/// Snapshots written by the IN-CLUSTER runner before the capture-side fix hold
/// objects with neither field: kube-rs lists return items whose type is implied
/// by the List, and the runner staged them verbatim. Nothing internal noticed —
/// the restore's own routing takes the kind from the FILENAME — but
/// `kubectl apply --server-side` rejects such a body outright
/// (`[apiVersion not set, kind not set]`), so the first `apply_cr` of every
/// restore of such a snapshot died. The type is knowable here, from the same
/// filename tag the routing already uses, so the restore repairs what it reads
/// rather than leaving those snapshots unrestorable.
///
/// A body that already states its type is left alone, tag lookup included — a
/// snapshot from the CLI (`kubectl get -o json` carries the types) or from a
/// fixed runner restores exactly as before, and a future kind this restore has
/// no mapping for still loads.
///
/// A body that needs the repair and whose tag is unknown is an ERROR naming the
/// file. Skipping it would apply part of a cluster and call it a restore.
fn stamp_cr_type(cr: &mut Value, kind_tag: &str, path: &Path) -> Result<()> {
    if carries_object_type(cr) {
        return Ok(());
    }
    let (api_version, kind) = known_cr_type(kind_tag).ok_or_else(|| {
        CliError::Other(format!(
            "backup CR {} states no apiVersion/kind, and its kind tag {kind_tag:?} is not one \
             this restore can type ({}). Refusing to apply an object whose kind is unknown — \
             restoring part of a cluster is worse than not starting.",
            path.display(),
            "Application, ArgoApplication, PlatformStack, SharedVolume, SourceCredential"
        ))
    })?;
    fill_object_type(cr, api_version, kind, path)
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
fn ensure_namespaces_all(
    app_namespaces: &[String],
    secret_namespaces: &[String],
    kubeconfig: &Path,
) -> Result<()> {
    let ensured = namespaces_to_ensure_all(app_namespaces, secret_namespaces);
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

/// Every namespace a restore must create: the app namespaces plus the ones
/// the backup captured SECRETS from.
///
/// Sorted and deduplicated so a restore is reproducible. A backup written
/// before `secretNamespaces` existed passes an empty second list and gets
/// exactly the old behaviour.
fn namespaces_to_ensure_all<'a>(apps: &'a [String], secrets: &'a [String]) -> Vec<&'a str> {
    let mut all: Vec<&str> = namespaces_to_ensure(apps);
    all.extend(namespaces_to_ensure(secrets));
    all.sort_unstable();
    all.dedup();
    all
}

/// What a restore should do with the origin-firewall intent a snapshot
/// recorded (A4). Pure.
///
/// * `recorded` — the replayed `PlatformStack`'s
///   `spec.firewall.cloudflareOrigin`. `None` is UNKNOWN (a snapshot of a
///   cluster from before the field existed, or one whose operator never ran
///   the toggle), and is treated as "say nothing": claiming the source had its
///   80/443 open is a statement about a cluster this snapshot never recorded.
/// * `Some(false)` changes nothing either. The carry is one-directional on
///   purpose — it can only ever RESTRICT ports, so inheriting it can never
///   leave a cluster more exposed than doing nothing, and turning a
///   destination's firewall OFF because the source had none is the one
///   direction that could.
/// * `destination_already_on` — the destination target already records the
///   toggle. Nothing to carry, and nothing worth a line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OriginFirewallAction {
    /// Change nothing, say nothing.
    Nothing,
    /// Write it onto the destination target and reconcile its live firewall —
    /// only on the mode that provisioned the node.
    Carry,
    /// Say the snapshot recorded it; change nothing. The cluster is already
    /// running with whatever firewall it has, so writing the target's config
    /// would be a claim this restore has not made true.
    Announce,
}

fn origin_firewall_action(
    recorded: Option<bool>,
    reprovisioned: bool,
    destination_already_on: bool,
) -> OriginFirewallAction {
    if recorded != Some(true) || destination_already_on {
        return OriginFirewallAction::Nothing;
    }
    if reprovisioned {
        OriginFirewallAction::Carry
    } else {
        OriginFirewallAction::Announce
    }
}

/// Is the Cloudflare origin firewall already recorded on the destination
/// target? A target that cannot be read answers `false`, which downgrades a
/// carry to a warning rather than skipping it silently.
fn target_records_origin_firewall(target: Option<&str>) -> bool {
    crate::commands::state_paths::resolve_state_paths(target)
        .ok()
        .and_then(|r| cli_core::target::load_target(&r.store, &r.target_name).ok())
        .and_then(|t| t.config.firewall)
        .is_some_and(|f| f.cloudflare_origin)
}

/// Carry (or announce) the source's origin-firewall intent, and report what
/// happened for the summary (A4).
///
/// The carry is a side effect on the DESTINATION target's local config plus
/// its live cloud firewall, so it is reported rather than done quietly — and
/// it is scoped to the named target, never the active one.
///
/// A failure here never fails the restore. The data is in the cluster by the
/// time this can be known; refusing to finish over a firewall the operator can
/// set with one command afterwards would trade a working restore for a tidier
/// invariant. It is reported in full instead, ports-still-open and all.
///
/// The cluster-side record is NOT rewritten ([`ClusterRecord::Skip`]): the
/// value being carried was just read out of the `PlatformStack` this restore
/// applied a moment ago, so patching it back would be a round trip whose only
/// possible outcome is a spurious warning about a record that demonstrably
/// exists.
///
/// [`ClusterRecord::Skip`]: crate::commands::target_firewall::ClusterRecord::Skip
fn settle_origin_firewall(
    recorded: Option<bool>,
    reprovision: bool,
    target: Option<&str>,
) -> OriginFirewall {
    use crate::commands::target_firewall::ClusterRecord;
    match origin_firewall_action(
        recorded,
        reprovision,
        target_records_origin_firewall(target),
    ) {
        OriginFirewallAction::Nothing => OriginFirewall::Nothing,
        OriginFirewallAction::Announce => OriginFirewall::Announced,
        OriginFirewallAction::Carry => {
            match crate::commands::target_firewall::apply_cloudflare_origin(
                target,
                true,
                ClusterRecord::Skip,
            ) {
                Ok(applied) => {
                    for line in &applied.lines {
                        println!("  {line}");
                    }
                    match applied.warning {
                        None => OriginFirewall::Carried {
                            target: applied.target,
                        },
                        // The intent IS on disk here: the carry persists it
                        // before it looks for a firewall to reconcile.
                        Some(why) => OriginFirewall::CarriedNotEnforced {
                            target: applied.target,
                            why,
                            recorded: true,
                        },
                    }
                }
                // Anything from "target could not be read" to "the Cloudflare
                // ranges could not be fetched" lands here, and they differ in
                // whether the toggle stuck. Ask the target rather than guess.
                Err(e) => OriginFirewall::CarriedNotEnforced {
                    target: target.unwrap_or("<active>").to_string(),
                    why: format!("{e}"),
                    recorded: target_records_origin_firewall(target),
                },
            }
        }
    }
}

/// **ApplyImportedCerts** (A1) — re-apply the imported TLS certificates the
/// snapshot carries under `certs/`, and report which of the restored domains
/// still have no certificate to point at.
///
/// Applied as the plain `kubernetes.io/tls` Secrets they were captured as —
/// whole objects, labels and annotations included. They are NOT re-sealed like
/// the material under `secrets/`: nothing sealed them at the source (an
/// imported certificate has no SealedSecret behind it, which is exactly why
/// the capture sweep used to miss it), and the import labels have to survive —
/// `target domain add` checks them before it will point a domain at the
/// Secret, and the backup path itself keys on them, so a restored cluster that
/// lost them would drop the certificate from its own next backup.
///
/// Returns `(applied, dangling certificate names)`.
fn apply_imported_certs(data_dir: &Path, kubeconfig: &Path) -> Result<(usize, Vec<String>)> {
    let certs = read_imported_certs(data_dir)?;
    for cert in &certs {
        apply_cr(cert, kubeconfig)?;
    }
    if !certs.is_empty() {
        println!("  ✓ {} imported TLS certificate(s) restored", certs.len());
    }

    let restored: Vec<String> = certs.iter().filter_map(object_name).collect();
    let crs = read_crs(data_dir)?;
    let referenced = crs
        .iter()
        .find(|c| c.kind == "PlatformStack")
        .map(|ps| referenced_cert_names(&ps.cr))
        .unwrap_or_default();
    Ok((certs.len(), dangling_cert_refs(&referenced, &restored)))
}

/// Read every staged certificate under `data/certs/`, ordered so a restore is
/// reproducible. A snapshot with no `certs/` directory — every backup taken
/// before the certificate was captured, and every cluster that never connected
/// a domain — yields an empty list rather than an error.
///
/// Type-stamped on the way out, like [`read_crs`]: an imported certificate is a
/// plain `v1`/`Secret` BY CONSTRUCTION (`target cert import` applies it as one
/// and the capture sweep keys on its label), so there is nothing to look up —
/// but a snapshot from the in-cluster runner before the capture-side fix
/// carries the object with neither field, and `ApplyImportedCerts` is the FIRST
/// step of a restore that applies a captured object, so that snapshot died
/// here.
fn read_imported_certs(data_dir: &Path) -> Result<Vec<Value>> {
    let certs_dir = data_dir.join(backup_core::engine::CERTS_DIR);
    let Ok(entries) = std::fs::read_dir(&certs_dir) else {
        return Ok(Vec::new());
    };
    let mut files: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("json"))
        .collect();
    files.sort();
    let mut out = Vec::new();
    for path in files {
        let body = std::fs::read(&path)
            .map_err(|e| CliError::Other(format!("read cert {}: {e}", path.display())))?;
        let mut cert: Value = serde_json::from_slice(&body)
            .map_err(|e| CliError::Other(format!("parse cert {}: {e}", path.display())))?;
        fill_object_type(&mut cert, "v1", "Secret", &path)?;
        out.push(cert);
    }
    Ok(out)
}

/// `metadata.name` of a captured object.
fn object_name(o: &Value) -> Option<String> {
    o.pointer("/metadata/name")
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// Every certificate name the captured `PlatformStack`'s registered domains
/// point at (`spec.values.gateway.allowedDomains[].importedCertRef`), sorted
/// and deduplicated. Pure.
///
/// This is the reference the chart renders into the Gateway's
/// `tls.certificateRefs`, so it is the exact question "what does this cluster
/// need a certificate for".
fn referenced_cert_names(platformstack: &Value) -> Vec<String> {
    let mut out: Vec<String> = platformstack
        .pointer("/spec/values/gateway/allowedDomains")
        .and_then(Value::as_array)
        .map(|domains| {
            domains
                .iter()
                .filter_map(|d| d.get("importedCertRef").and_then(Value::as_str))
                .filter(|r| !r.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    out.sort();
    out.dedup();
    out
}

/// The certificate references the restore cannot satisfy: named by a restored
/// domain, carried by no captured certificate. Pure.
///
/// This is the A1 defect seen from the restore side, and it is what EVERY
/// backup taken before the certificate was captured produces — so the restore
/// has to be able to name it rather than leave an operator to discover it from
/// a browser error.
fn dangling_cert_refs(referenced: &[String], restored: &[String]) -> Vec<String> {
    referenced
        .iter()
        .filter(|r| !restored.contains(r))
        .cloned()
        .collect()
}

/// What the replayed `PlatformStack` told this restore about the source
/// cluster — everything the CR carries that the summary or a later step needs.
#[derive(Debug, Default, PartialEq, Eq)]
struct PlatformStackReplay {
    /// The source's `spec.backup.clusterName`, when the replayed CR carried an
    /// enabled schedule (E1).
    inherited_cluster_name: Option<String>,
    /// What the schedule policy did to the replayed `spec.backup` (D1/D2).
    schedule: BackupSchedule,
    /// The source's `spec.firewall.cloudflareOrigin` (A4). `None` is UNKNOWN —
    /// a snapshot of a cluster that never recorded one — and must never be
    /// read as "the source had it off".
    origin_firewall: Option<bool>,
}

/// **ApplyPlatformStack** — apply the sanitized `PlatformStack` from `crs/`,
/// mirroring the `cluster_bootstrap` retry loop for the admission-webhook
/// Endpoints race.
///
/// `flags` carries the schedule flags and the terminal state down to
/// [`resolve_schedule_choice`], which ASKS when neither flag answered the
/// question. The question is settled here, BEFORE the apply: the operator
/// reconciles the backup CronJob straight out of `spec.backup`, so by the time
/// the apply returns the answer is already in effect.
///
/// The origin-firewall intent is read off the SAME CR this step applies, so
/// what the caller acts on is exactly what landed in the cluster.
fn apply_platformstack_from_crs(
    data_dir: &Path,
    kubeconfig: &Path,
    flags: ScheduleFlags,
) -> Result<PlatformStackReplay> {
    let crs = read_crs(data_dir)?;
    let Some(ps) = crs.iter().find(|c| c.kind == "PlatformStack") else {
        // A backup without a PlatformStack (older shape) — nothing to apply;
        // the target's own bootstrap PlatformStack stays in place.
        println!("  (no PlatformStack in backup — keeping target's own)");
        return Ok(PlatformStackReplay::default());
    };
    let inherited = inherited_backup_cluster_name(&ps.cr);
    let origin_firewall = crate::commands::target_firewall::recorded_origin_firewall(&ps.cr);
    let (yaml, schedule) = platformstack_apply_payload(&ps.cr, flags)?;
    apply_with_retry(
        PLATFORMSTACK_APPLY_ATTEMPTS,
        std::time::Duration::from_secs(PLATFORMSTACK_APPLY_BACKOFF_SECS),
        &mut |_attempt| kubectl_apply_server_side(&yaml, RESTORE_FIELD_MANAGER, kubeconfig),
    )?;
    println!("  ✓ PlatformStack applied");
    Ok(PlatformStackReplay {
        inherited_cluster_name: inherited,
        schedule,
        origin_firewall,
    })
}

/// The exact bytes `ApplyPlatformStack` hands to `kubectl apply`, and what the
/// schedule question decided. Impure only in the branch that has to ask (see
/// [`resolve_schedule_choice`]); everything else about it is a pure transform.
///
/// The question and the bytes live in ONE function so they are testable
/// together, which is where it matters: a correctly-refusing
/// [`schedule_decision`] next to a payload builder that went on to serialize
/// the captured CR anyway would be the whole defect back, with every
/// decision-level test still green. For the same reason the refusal is
/// returned as an `Err` from here — before the caller has anything to apply.
fn platformstack_apply_payload(
    captured: &Value,
    flags: ScheduleFlags,
) -> Result<(String, BackupSchedule)> {
    let keep_backup_schedule = resolve_schedule_choice(captured, flags)?;
    let (cr, schedule) = apply_backup_schedule_policy(captured, keep_backup_schedule);
    let yaml = serde_json::to_string(&cr)
        .map_err(|e| CliError::Other(format!("serialize PlatformStack: {e}")))?;
    Ok((yaml, schedule))
}

/// What this restore did to the backup schedule it replayed (D1/D2).
///
/// A restore replays the WHOLE `PlatformStack`, so `spec.backup` — bucket,
/// credential, schedule, timezone, retention, `enforce` — migrates with it, and
/// the operator reconciles the CronJob straight out of `spec.backup` outside
/// the upgrade-approval gate. The restored cluster therefore starts writing to
/// the SOURCE's repository, on the source's schedule.
///
/// Whether that is right depends on something no code here can see — see
/// [`schedule_decision`], which is where the question gets asked.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum BackupSchedule {
    /// The backup carried no enabled schedule, so nothing was inherited and
    /// there is nothing to say. Also the default: a `PlatformStackReplay`
    /// built for a snapshot with no PlatformStack in it has inherited
    /// nothing, by construction.
    #[default]
    NotInherited,
    /// The schedule was replayed as captured but forced OFF.
    Disabled,
    /// Replayed enabled, on purpose.
    KeptEnabled,
}

/// What to do about a replayed backup schedule, before anything is applied.
/// Pure (no I/O) — the prompt and the refusal are wired in
/// [`resolve_schedule_choice`]; this type is factored out so all four branches
/// are unit-testable without a terminal, exactly as `secret.rs`'s
/// `overwrite_decision` is.
#[derive(Debug, PartialEq, Eq)]
enum ScheduleChoice {
    /// Nothing to decide: the replayed block carries no enabled schedule.
    NothingToInherit,
    /// Replay it enabled — `--keep-backup-schedule`.
    Inherit,
    /// Replay it switched off — `--discard-backup-schedule`.
    Disable,
    /// Ask on the terminal before applying anything.
    Prompt,
    /// No terminal and no flag: refuse, naming both flags.
    ErrorNonInteractive,
}

/// Decide what a restore does with the backup schedule it is about to replay.
/// Pure: takes whether the captured block is enabled, the two flags and the TTY
/// state, and returns the action.
///
/// There are THREE restore paths and inheriting is right in two of them: a new
/// cluster that should not touch the old destination (off), a move to a bigger
/// machine whose old cluster is retired shortly after (on), and re-provisioning
/// a target whose cluster died (on). Nothing in the artifact distinguishes
/// them. The tempting heuristic — "are there snapshots from this cluster newer
/// than the one being restored, so the source kept writing?" — fails in the
/// unsafe direction on the ordinary move flow, which takes a backup and
/// restores it immediately: no newer snapshot exists and the source is very
/// much alive. So the tool does not guess; it asks, and without a terminal to
/// ask on it stops instead of picking a side.
///
/// `keep` and `discard` are mutually exclusive at the clap layer
/// (`conflicts_with`), so both-at-once cannot reach here from the CLI; should
/// another caller pass both, `discard` wins because it is the reversible half.
fn schedule_decision(enabled: bool, keep: bool, discard: bool, is_tty: bool) -> ScheduleChoice {
    if !enabled {
        return ScheduleChoice::NothingToInherit;
    }
    if discard {
        return ScheduleChoice::Disable;
    }
    if keep {
        return ScheduleChoice::Inherit;
    }
    if is_tty {
        ScheduleChoice::Prompt
    } else {
        ScheduleChoice::ErrorNonInteractive
    }
}

/// The flags and terminal state the schedule question is answered from,
/// carried from the command entry point down to the step that applies the CR.
#[derive(Clone, Copy, Debug)]
struct ScheduleFlags {
    keep: bool,
    discard: bool,
    is_tty: bool,
}

/// How the repository the inherited schedule points at is named to the
/// operator. Pure.
///
/// The bucket is the whole point of the question — "shall this cluster write
/// there" is unanswerable without knowing where "there" is — so it is quoted
/// verbatim from the captured CR. A block with no bucket is a misconfigured
/// source rather than a normal one, and the fallback keeps the sentence
/// readable instead of printing an empty pair of quotes.
fn inherited_repo_label(platformstack: &Value) -> String {
    match platformstack
        .pointer("/spec/backup/bucket")
        .and_then(Value::as_str)
        .filter(|b| !b.is_empty())
    {
        Some(b) => format!("'{b}'"),
        None => "the source's repository (the captured block names no bucket)".to_string(),
    }
}

/// What the terminal prompt says before the schedule is applied. Pure, so the
/// consequence an operator is asked to accept is pinned by a test.
///
/// Names the repository and says what two live writers means, because that is
/// the only part of this an operator cannot reconstruct from the summary
/// afterwards — by then the CronJob is already reconciled.
fn schedule_prompt_text(platformstack: &Value) -> String {
    let repo = inherited_repo_label(platformstack);
    let backup = platformstack.pointer("/spec/backup");
    let when = backup
        .and_then(|b| b.pointer("/schedule"))
        .and_then(Value::as_str)
        .unwrap_or("(no schedule recorded)");
    let zone = backup
        .and_then(|b| b.pointer("/timeZone"))
        .and_then(Value::as_str)
        .unwrap_or("UTC");
    format!(
        "This backup carries the source cluster's backup schedule, and it is ENABLED.\n  \
         repository: {repo}\n  \
         schedule:   {when} ({zone})\n\
         Inheriting it makes THIS cluster back up to that repository on that schedule. If the \
         source cluster is still running and still backing up, both clusters will then be \
         writing to the one repository: they are told apart by identity, but they share its \
         retention, and a prune run by either can remove the other's snapshots.\n\
         Answer no to restore the block exactly as captured but switched off — bucket, \
         credential, schedule, timezone and retention are kept, and `apprafter backup set \
         enabled true` turns it on later."
    )
}

/// What a non-interactive restore is told when it did not answer the question.
/// Pure.
///
/// It names both flags and what each one means, because the operator reading
/// this is reading it out of a CI log with no terminal to explain it: a
/// scripted restore that silently redirected a live cluster's backups would be
/// far worse than one that stops here.
fn non_interactive_schedule_error(platformstack: &Value) -> CliError {
    let repo = inherited_repo_label(platformstack);
    CliError::Other(format!(
        "this restore would inherit the source cluster's backup schedule, pointed at {repo}, and \
         there is no terminal to ask on. Re-run with exactly one of:\n  \
         --keep-backup-schedule     — inherit it ENABLED: this cluster becomes that repository's \
         writer (disaster recovery, or a move whose old cluster is being retired).\n  \
         --discard-backup-schedule  — replay the block exactly as captured but switched off; \
         `apprafter backup set enabled true` turns it on later.\n\
         Neither can be chosen for you: if the source cluster is still running, inheriting \
         silently puts two clusters on one repository."
    ))
}

/// Answer the schedule question — the thin impure half of
/// [`schedule_decision`]. Returns whether to replay the block ENABLED.
///
/// Called from `ApplyPlatformStack`, which is the first moment the captured
/// `spec.backup` is readable and the last moment before it is applied.
fn resolve_schedule_choice(platformstack: &Value, flags: ScheduleFlags) -> Result<bool> {
    let enabled = platformstack
        .pointer("/spec/backup/enabled")
        .and_then(Value::as_bool)
        == Some(true);
    match schedule_decision(enabled, flags.keep, flags.discard, flags.is_tty) {
        ScheduleChoice::NothingToInherit => Ok(false),
        ScheduleChoice::Inherit => Ok(true),
        ScheduleChoice::Disable => Ok(false),
        ScheduleChoice::Prompt => {
            println!("{}", schedule_prompt_text(platformstack));
            inquire::Confirm::new("Inherit the source's backup schedule?")
                .with_default(false)
                .prompt()
                .map_err(|e| CliError::Other(format!("backup-schedule prompt: {e}")))
        }
        ScheduleChoice::ErrorNonInteractive => Err(non_interactive_schedule_error(platformstack)),
    }
}

/// Apply the replayed-schedule policy to a captured `PlatformStack` CR, and
/// say what it did. Pure — the impure caller applies the returned CR.
///
/// ONLY `spec.backup.enabled` is touched. Everything else in the block is
/// restored exactly as captured, so turning the schedule back on afterwards is
/// a one-word `apprafter backup enable` rather than a re-entry of bucket,
/// credential, retention, timezone and schedule.
///
/// `keep == true` returns the CR untouched: when the answer was "inherit", the
/// inheritance is the point. A backup that was not enabled at capture time is
/// [`NotInherited`] in both directions — there is no schedule to disable and
/// none to keep.
///
/// [`NotInherited`]: BackupSchedule::NotInherited
fn apply_backup_schedule_policy(platformstack: &Value, keep: bool) -> (Value, BackupSchedule) {
    let enabled = platformstack
        .pointer("/spec/backup/enabled")
        .and_then(Value::as_bool)
        == Some(true);
    if !enabled {
        return (platformstack.clone(), BackupSchedule::NotInherited);
    }
    if keep {
        return (platformstack.clone(), BackupSchedule::KeptEnabled);
    }
    let mut out = platformstack.clone();
    if let Some(backup) = out
        .pointer_mut("/spec/backup")
        .and_then(Value::as_object_mut)
    {
        backup.insert("enabled".to_string(), Value::Bool(false));
    }
    (out, BackupSchedule::Disabled)
}

/// The backup cluster-name this restore just replayed onto the target, when
/// the backup schedule came with it. Pure.
///
/// `spec.backup.clusterName` is the HUMAN label every snapshot is listed under.
/// It is part of `spec.backup`, so restore replays it verbatim — meaning a
/// clone inherits the source's name and its snapshots appear in the repository
/// under that name. That is cosmetic (every filter keys on the `kube-system`
/// UID, which a clone cannot inherit) but it must not be a SURPRISE, which is
/// why the summary prints it.
///
/// `None` when the backup was not enabled or carried no name: there is nothing
/// inherited to warn about, and a line saying so would be noise on the one
/// screen an operator reads on their worst day.
fn inherited_backup_cluster_name(platformstack: &Value) -> Option<String> {
    let backup = platformstack.pointer("/spec/backup")?;
    if backup.pointer("/enabled").and_then(Value::as_bool) != Some(true) {
        return None;
    }
    backup
        .pointer("/clusterName")
        .and_then(Value::as_str)
        .filter(|n| !n.is_empty())
        .map(str::to_string)
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
/// Both lists are the caller's and are filled in BEFORE the applies run, not
/// after: an apply that fails halfway leaves gated, zero-replica apps behind,
/// and the interruption hint has to be able to name them. The plan is known in
/// full before the first write, so recording it up front is the honest set —
/// an app whose apply had not yet run is named too, which over-lists rather
/// than under-lists, and a re-run fixes both alike.
fn apply_apps_gated(
    data_dir: &Path,
    kubeconfig: &Path,
    suspended_argo: &mut Vec<(String, String)>,
    app_replicas: &mut Vec<((String, String), i64)>,
) -> Result<()> {
    let crs = read_crs(data_dir)?;
    let plan = gated_apply_plan(&crs);

    let gated = plan.app_replicas.len();
    app_replicas.extend(plan.app_replicas);
    record_suspended_argo(suspended_argo, plan.argo_apps);

    for object in &plan.objects {
        apply_cr(object, kubeconfig)?;
    }

    println!("  ✓ {gated} app(s) applied gated (replicas=0, Argo auto-sync stripped)");
    Ok(())
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
    // How long each load helper pod keeps itself alive, and so the most one
    // load may take: the target cluster's backup run deadline, the number that
    // bounds an extraction too (backup_core::helper_pod). It was a fixed hour,
    // which killed any load that needed longer with exit code 137.
    let keep_alive = backup_core::engine::read_run_deadline(&k)?;
    // pg: data/pg/<ns>/<claim>.dump
    load_pg_dumps(data_dir, &k, kubeconfig, keep_alive)?;
    // volumes: data/volumes/<ns>/<name>/data.tar
    load_volumes(data_dir, manifest, &k, kubeconfig, keep_alive)?;
    // redis: data/redis/<ns>/<claim>/dump.tar → Dragonfly whole-instance snapshot.
    load_redis(data_dir, &k, kubeconfig)?;
    // jetstream: data/jetstream/<ns>/<claim>/<stream>.tar → the stream, over
    // the NATS wire, messages and consumers together (2.6d-6).
    load_jetstream(data_dir, &k, kubeconfig, keep_alive)?;
    Ok(())
}

/// One dumped stream on disk: `jetstream/<ns>/<claim>/<stream>.tar`.
struct StreamArtifact {
    namespace: String,
    claim: String,
    /// The stream name, read off the file stem — verbatim, including the `_`
    /// that a pod name could not have carried.
    stream: String,
    path: PathBuf,
}

/// Every stream artifact under `data_dir`, in stable order.
///
/// A sibling of [`discover_nested_artifacts`] rather than a call into it: that
/// one looks for ONE known file name per claim directory, and here the file
/// name is the payload's identity. A missing `data/jetstream` is not an error —
/// a backup with no jetstream claim simply has none.
fn discover_stream_artifacts(data_dir: &Path) -> Vec<StreamArtifact> {
    let mut out = Vec::new();
    let Ok(namespaces) = std::fs::read_dir(data_dir.join("jetstream")) else {
        return out;
    };
    for ns_entry in namespaces.flatten() {
        let ns_path = ns_entry.path();
        if !ns_path.is_dir() {
            continue;
        }
        let namespace = ns_entry.file_name().to_string_lossy().into_owned();
        let Ok(claims) = std::fs::read_dir(&ns_path) else {
            continue;
        };
        for claim_entry in claims.flatten() {
            let claim_dir = claim_entry.path();
            if !claim_dir.is_dir() {
                continue;
            }
            let claim = claim_entry.file_name().to_string_lossy().into_owned();
            let Ok(files) = std::fs::read_dir(&claim_dir) else {
                continue;
            };
            for file in files.flatten() {
                let path = file.path();
                if path.extension().and_then(|e| e.to_str()) != Some("tar") {
                    continue;
                }
                let Some(stream) = path.file_stem().and_then(|s| s.to_str()) else {
                    continue;
                };
                out.push(StreamArtifact {
                    namespace: namespace.clone(),
                    claim: claim.clone(),
                    stream: stream.to_string(),
                    path,
                });
            }
        }
    }
    out.sort_by(|a, b| {
        (&a.namespace, &a.claim, &a.stream).cmp(&(&b.namespace, &b.claim, &b.stream))
    });
    out
}

/// The one-shot `sh -c` script that replays one stream snapshot.
///
/// Three measured facts shape it (nats-server 2.14.3, `nats` CLI v0.2.3):
///
/// * `nats stream restore` takes the DIRECTORY `nats stream backup` wrote, so
///   the tar is unpacked first — it arrives on stdin, which is why `tar x` is
///   the only thing in here that reads it;
/// * the restore REFUSES a stream that exists (`Stream "X" already exist`,
///   exit 1), so the stream is deleted first;
/// * NACK recreates a declared stream empty from its Stream CR on its own
///   loop, so losing that race between the delete and the replay is expected.
///   The loop retries on exactly that message and on nothing else — any other
///   failure is surfaced with the server's own words rather than retried into
///   a timeout.
///
/// What comes back is messages AND consumers with their pending state; a
/// stream restored without its consumers would replay everything to a
/// subscriber that had already processed it.
fn jetstream_restore_script(stream: &str) -> String {
    let stream_q = backup_core::helper_pod::shell_single_quote(stream);
    format!(
        "set -e; \
         rm -rf /tmp/bk; mkdir -p /tmp/bk; \
         tar x -C /tmp/bk; \
         i=0; \
         while :; do \
           nats stream rm {stream_q} -f >/dev/null 2>&1 || true; \
           if nats stream restore /tmp/bk --no-progress >/tmp/nats.err 2>&1; then exit 0; fi; \
           if ! grep -q 'already exist' /tmp/nats.err; then cat /tmp/nats.err >&2; exit 1; fi; \
           i=$((i+1)); \
           if [ \"$i\" -ge 5 ]; then \
             echo 'the stream was recreated faster than it could be restored' >&2; \
             cat /tmp/nats.err >&2; exit 1; \
           fi; \
           sleep 2; \
         done"
    )
}

/// Replay every stream snapshot into the freshly-provisioned account.
///
/// Grouped by claim: the server coordinates and the manager credentials are
/// per-claim, so one helper pod serves all of that claim's streams. The
/// coordinates come from the RESTORED claim's connection Secret (the fresh
/// ones, never the backed-up ones — the same rule the pg loader follows), and
/// the credentials from `nats-mgr-<ns>`, because a claim user is denied the
/// snapshot API by construction (ADR 0061 §4.2).
fn load_jetstream(
    data_dir: &Path,
    k: &dyn KubeExec,
    kubeconfig: &Path,
    keep_alive: std::time::Duration,
) -> Result<()> {
    let artifacts = discover_stream_artifacts(data_dir);
    if artifacts.is_empty() {
        return Ok(());
    }

    let mut by_claim: BTreeMap<(String, String), Vec<StreamArtifact>> = BTreeMap::new();
    for a in artifacts {
        by_claim
            .entry((a.namespace.clone(), a.claim.clone()))
            .or_default()
            .push(a);
    }

    for ((ns, claim), streams) in by_claim {
        let conn = resolve_claim_connection_secret(&ns, &claim, kubeconfig)?;
        let host = k.get_secret_key(&conn, &ns, "host")?;
        let port = k.get_secret_key(&conn, &ns, "port")?;
        let nats_ns = backup_core::extract::nats_namespace_of_host(&host).ok_or_else(|| {
            CliError::Other(format!(
                "cannot tell which namespace NATS runs in from host {host:?} \
                 (claim {ns}/{claim}): expected <service>.<namespace>.svc"
            ))
        })?;
        let mgr = backup_core::extract::mgr_secret_name(&ns);
        let user = k.get_secret_key(&mgr, &nats_ns, "user")?;
        let password = k.get_secret_key(&mgr, &nats_ns, "password")?;

        let server = NatsServer {
            namespace: nats_ns,
            url: format!("nats://{host}:{port}"),
            user,
            password,
        };
        restore_claim_streams(k, &ns, &claim, &server, &streams, keep_alive)?;
    }
    Ok(())
}

/// Where one claim's streams are replayed: the NATS server's namespace and
/// URL, and the manager credentials of the claim's namespace.
struct NatsServer {
    namespace: String,
    url: String,
    user: String,
    password: String,
}

/// Replay one claim's streams through a helper pod beside the server.
///
/// The pod is deleted on every return path by [`PodCleanupGuard`], armed
/// BEFORE the apply like the pg and volume loaders'. It used to be deleted by
/// hand after a failed stream and at the end, which left out a failed Ready
/// wait: the pod stayed, and since a helper pod's spec cannot change in place
/// and a `Completed` one never becomes Ready again, every later jetstream
/// restore of the claim failed on it until someone deleted it.
fn restore_claim_streams(
    k: &dyn KubeExec,
    ns: &str,
    claim: &str,
    server: &NatsServer,
    streams: &[StreamArtifact],
    keep_alive: std::time::Duration,
) -> Result<()> {
    let pod_name = format!("rs-js-{}", backup_core::extract::pod_name_segment(claim));
    let _guard = PodCleanupGuard {
        name: pod_name.clone(),
        namespace: server.namespace.clone(),
        k,
    };
    let spec = backup_core::helper_pod::nats_pod_spec(
        &pod_name,
        &server.namespace,
        backup_core::images::JETSTREAM_IMAGE,
        &server.url,
        &server.user,
        &server.password,
        keep_alive,
    );
    k.apply_and_wait_pod_ready(&spec)?;

    for artifact in streams {
        let script = jetstream_restore_script(&artifact.stream);
        let argv: Vec<&str> = vec!["sh", "-c", &script];
        k.exec_stream_from_file(&pod_name, &server.namespace, &argv, &artifact.path)?;
        println!(
            "  ✓ stream restored: {ns}/{claim} → {} (messages + consumers)",
            artifact.stream
        );
    }
    Ok(())
}

/// The connection Secret a freshly-provisioned claim published
/// (`status.connectionSecretRef`), read off the regenerated claim.
fn resolve_claim_connection_secret(ns: &str, claim: &str, kubeconfig: &Path) -> Result<String> {
    let claim_json = kubectl_get_json(
        "resourceclaims.apprafter.io",
        Some(claim),
        Some(ns),
        kubeconfig,
    )?
    .ok_or_else(|| {
        CliError::Other(format!(
            "claim {ns}/{claim} not found at jetstream LoadData — was it gated/applied?"
        ))
    })?;
    claim_status_field(
        &claim_json,
        "/status/connectionSecretRef",
        "connectionSecretRef",
        ns,
        claim,
    )
}

/// Restore every `data/pg/<ns>/<claim>.dump` via `pg_restore` over a helper
/// pod, using the FRESH connection Secret (L3 — the post-provision creds, NOT
/// the backed-up ones).
fn load_pg_dumps(
    data_dir: &Path,
    k: &dyn KubeExec,
    kubeconfig: &Path,
    keep_alive: std::time::Duration,
) -> Result<()> {
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
        load_one_pg(
            &ns, &claim, &dump_path, k, kubeconfig, &pg_image, keep_alive,
        )?;
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
    keep_alive: std::time::Duration,
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

    run_pg_restore(
        ns,
        claim,
        &conn,
        dump_path,
        k,
        pg_image,
        keep_alive,
        &|pod| {
            // Wait for the TARGET database to actually accept a connection before
            // streaming the dump. `WaitClaimsBound` only guarantees the
            // ResourceClaim's `.status.ready` (a control-plane condition); for the
            // FIRST claim that lazily provisions the shared CNPG cluster, the
            // server can still be finishing initdb (connection refused) AND the
            // per-claim database can be uncreated (`FATAL: database "…" does not
            // exist`) when the claim flips ready, so an immediate `pg_restore`
            // aborts the whole restore.
            wait_pg_reachable(pod, ns, &conn, kubeconfig)
        },
    )
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

/// Stand a pg helper pod up, wait for the database behind `probe` to answer,
/// and stream the dump into `pg_restore` on its stdin (L2). The pod is deleted
/// on every return path by [`PodCleanupGuard`].
#[allow(clippy::too_many_arguments)]
fn run_pg_restore(
    ns: &str,
    claim: &str,
    conn: &PgConnection,
    dump_path: &Path,
    k: &dyn KubeExec,
    pg_image: &str,
    keep_alive: std::time::Duration,
    probe: &dyn Fn(&str) -> Result<()>,
) -> Result<()> {
    let pod_name = truncate_pod_name(&format!("ld-pg-{claim}"));
    // The backup's own pg helper builder (the pg_dump image carries
    // `pg_restore`): `PGPASSWORD` so `pg_restore` never prompts and hangs the
    // restore, and `PGOPTIONS` so a `pg_restore` stopped while its `--clean`
    // waits on a lock does not leave that request queued on the server.
    let spec = pg_helper_pod_spec(&pod_name, ns, pg_image, &conn.pass, keep_alive);

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
    keep_alive: std::time::Duration,
) -> Result<()> {
    for (ns, name, tar_path) in discover_nested_artifacts(data_dir, "volumes", "data.tar") {
        let pvc = resolve_volume_pvc(&ns, &name, manifest, kubeconfig)?;
        load_one_volume(&ns, &name, &pvc, &tar_path, k, keep_alive)?;
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
    keep_alive: std::time::Duration,
) -> Result<()> {
    let pod_name = truncate_pod_name(&format!("ld-vol-{name}"));
    let spec = volume_pod_spec(&pod_name, ns, VOLUME_IMAGE, pvc, false, keep_alive); // L1: RW
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

/// POSIX-safe single-quote, from `backup-core` — the dump side quotes stream
/// names with the same function, and two quoting rules in one repository is
/// how one of them ends up subtly different from the other.
use backup_core::helper_pod::shell_single_quote;

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
            body: resume_patch_body(*replicas),
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

/// The patches that quiesce ONE application for a `--data-only` load — the
/// pure seam of the write half of [`suspend_running_workloads`], mirroring
/// [`resume_patches`] on the way back up.
///
/// Argo CD auto-sync goes off BEFORE the scale-to-zero, because self-heal
/// would otherwise put the replica count straight back and the load would run
/// under a live pod — which is the entire reason this step exists. The
/// application's own patch is [`suspend_patch_body`]: the record and the scale
/// in one write.
fn suspend_patches(
    ns: &str,
    name: &str,
    argo: &[(String, String)],
    replicas: i64,
) -> Vec<MergePatch> {
    let mut out: Vec<MergePatch> = argo
        .iter()
        .map(|(argo_ns, argo_name)| MergePatch {
            resource: "applications.argoproj.io",
            namespace: argo_ns.clone(),
            name: argo_name.clone(),
            body: argo_autosync_patch_body(false),
        })
        .collect();
    out.push(MergePatch {
        resource: "applications.apprafter.io",
        namespace: ns.to_string(),
        name: name.to_string(),
        body: suspend_patch_body(replicas),
    });
    out
}

/// Merge-patch body that SUSPENDS an AppRafter Application: scale to zero and
/// record, in the same write, the count to come back to (H5).
///
/// One patch, not two, and deliberately so — an app at zero replicas with no
/// record of what it was is precisely the state a re-run cannot recover from,
/// and two patches would open a window onto it.
fn suspend_patch_body(pre_restore_replicas: i64) -> String {
    format!(
        r#"{{"metadata":{{"annotations":{{"{PRE_RESTORE_REPLICAS_ANNOTATION}":"{pre_restore_replicas}"}}}},"spec":{{"base":{{"replicas":0}}}}}}"#
    )
}

/// Merge-patch body that RESUMES an AppRafter Application: the recorded replica
/// count back, and the pre-restore annotation removed.
///
/// The `null` is how a JSON merge-patch DELETES a key. Clearing it is the last
/// half of the record's life-cycle: a leftover annotation would win over the
/// live count on the NEXT restore, and pin an app to a number that stopped
/// being true the moment this one finished. Removing a key that was never
/// there is a no-op, so the gated (non-`--data-only`) resume — which reads its
/// counts from the backup artifact and never annotates — uses the same body.
fn resume_patch_body(replicas: i64) -> String {
    format!(
        r#"{{"metadata":{{"annotations":{{"{PRE_RESTORE_REPLICAS_ANNOTATION}":null}}}},"spec":{{"base":{{"replicas":{replicas}}}}}}}"#
    )
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
/// owns a backed-up data artifact, record its pre-restore replica count ON THE
/// OBJECT, disable its Argo auto-sync, and scale it to 0 so the load doesn't
/// race a running pod.
///
/// The set of apps to suspend is derived from the claims recorded in the
/// manifest (those whose data we are about to load): we suspend the Application
/// that lives in the same namespace as each claim. (In practice the data-only
/// flow targets a single app's claim; suspending its namespace's Application is
/// the conservative, correct move.)
///
/// `recorded` is the caller's list and is appended to AS EACH APP IS SUSPENDED,
/// not returned at the end: a failure halfway through this step leaves apps
/// scaled to zero, and the interruption hint can only name them if the caller
/// already holds them. Same reason `suspended_argo` has always been a `&mut`.
///
/// The annotation and the scale-to-zero go out as ONE merge-patch
/// ([`suspend_patch_body`]), so there is no instant in which an app is at zero
/// with no record of what it was.
fn suspend_running_workloads(
    manifest: &BackupManifest,
    kubeconfig: &Path,
    suspended_argo: &mut Vec<(String, String)>,
    recorded: &mut Vec<((String, String), i64)>,
) -> Result<()> {
    let claim_namespaces = claim_namespaces(manifest);

    let before = recorded.len();
    for ns in &claim_namespaces {
        let apps = list_items("applications.apprafter.io", Some(ns), kubeconfig)?;
        for decision in apps_to_suspend(&apps, ns, recorded) {
            let SuspendDecision {
                name,
                replicas,
                source,
            } = decision;
            match &source {
                ReplicaSource::Live => {}
                ReplicaSource::Annotation => println!(
                    "  ↻ {ns}/{name} still carries a pre-restore record of {replicas} replica(s) \
                     from an earlier restore that did not finish — resuming to that, not to its \
                     current count"
                ),
                ReplicaSource::UnusableAnnotation(raw) => eprintln!(
                    "  ⚠ {ns}/{name} has a {PRE_RESTORE_REPLICAS_ANNOTATION} annotation that is \
                     not a replica count ({raw:?}); using its current count ({replicas}) instead \
                     — check it is what you want the app resumed to"
                ),
            }
            // `ns` — the loop's own namespace — is the workload's namespace:
            // `apps_to_suspend` is fed a listing scoped to it. It is NOT
            // carried on `SuspendDecision`, which describes only what to do
            // with ONE already-located Application; copying the loop variable
            // onto every decision would give the same fact two sources of
            // truth that a later edit could let drift.
            let argo = argo_apps_for(&name, ns, kubeconfig)?;

            // Both records go in BEFORE the writes they describe, for the same
            // reason the annotation does: a patch that fails halfway through
            // must still leave the caller able to name what is down.
            recorded.push(((ns.to_string(), name.clone()), replicas));
            record_suspended_argo(suspended_argo, argo.iter().cloned());

            for patch in suspend_patches(ns, &name, &argo, replicas) {
                kubectl_merge_patch(
                    patch.resource,
                    &patch.name,
                    Some(&patch.namespace),
                    None,
                    &patch.body,
                    kubeconfig,
                )?;
            }
        }
    }
    let suspended = recorded.len() - before;
    if suspended > 0 {
        println!("  ✓ {suspended} app(s) suspended for data-only load");
    }
    Ok(())
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

/// One Application a `--data-only` restore is about to quiesce, and the count
/// it must be resumed to.
#[derive(Debug, Clone, PartialEq)]
struct SuspendDecision {
    /// `metadata.name` of the AppRafter Application.
    name: String,
    /// The replica count to record now and patch back at `ResumeWorkloads`.
    replicas: i64,
    /// Where `replicas` came from — reported, because "we are using a count
    /// left by an earlier run" is not something to do silently.
    source: ReplicaSource,
}

/// Where a [`SuspendDecision`]'s replica count was read from.
#[derive(Debug, Clone, PartialEq)]
enum ReplicaSource {
    /// The live object's `spec.base.replicas` — the ordinary first run.
    Live,
    /// The [`PRE_RESTORE_REPLICAS_ANNOTATION`] an earlier, interrupted run
    /// left behind. This is the count that run read BEFORE it scaled the app
    /// to zero, so it is the truth and the live value is the damage.
    Annotation,
    /// The annotation was present but not a usable count (the string is what
    /// it said). The live value was used instead.
    UnusableAnnotation(String),
}

/// Each Application in one namespace that still needs suspending, with the
/// replica count to resume it to — the pure seam of
/// [`suspend_running_workloads`].
///
/// An Application with no name is skipped (there is nothing to patch), and one
/// already present in `already_recorded` is skipped so a second pass over the
/// same namespace cannot record `replicas: 0` — the value it just wrote — as
/// the count to resume, which would leave the app scaled to zero after a
/// successful restore. An absent `spec.base.replicas` reads as 1, the
/// operator's own default.
///
/// # The count a PREVIOUS run left (H5)
///
/// `already_recorded` guards one run against itself and dies with the process,
/// so the identical mistake spanning two runs — read `3`, scale to `0`, fail;
/// re-run, read the `0` we wrote, "resume" to `0` — was wide open. The
/// [`PRE_RESTORE_REPLICAS_ANNOTATION`] is that record made durable, and it
/// WINS over the live field whenever it parses:
///
/// * absent → the live count (first run, nothing to recover from);
/// * a number, INCLUDING `0` → that number. A `0` there is a deliberately
///   scaled-down app faithfully recorded by an earlier run, not damage, and
///   resuming it to 1 would start a workload its operator had stopped;
/// * anything else (hand-edited, truncated) → the live count, and the caller
///   says so out loud rather than guessing a number out of a broken string.
///
/// The annotation winning is unconditional, not "only when the live count is
/// 0": the rule has to be one sentence an operator can hold. The cost is that
/// scaling an app up BY HAND between an interrupted run and its re-run is
/// superseded by the recorded count — remove the annotation first if that is
/// what you meant, or let the restore finish and scale afterwards.
fn apps_to_suspend(
    apps: &[Value],
    ns: &str,
    already_recorded: &[((String, String), i64)],
) -> Vec<SuspendDecision> {
    let mut out: Vec<SuspendDecision> = Vec::new();
    for app in apps {
        let name = cr_string(app, "/metadata/name", "");
        if name.is_empty()
            || already_recorded
                .iter()
                .any(|((n, a), _)| n == ns && a == &name)
            || out.iter().any(|d| d.name == name)
        {
            continue;
        }
        let live = app
            .pointer("/spec/base/replicas")
            .and_then(Value::as_i64)
            .unwrap_or(1);
        let (replicas, source) = match recorded_replicas(app) {
            None => (live, ReplicaSource::Live),
            Some(Ok(n)) => (n, ReplicaSource::Annotation),
            Some(Err(raw)) => (live, ReplicaSource::UnusableAnnotation(raw)),
        };
        out.push(SuspendDecision {
            name,
            replicas,
            source,
        });
    }
    out
}

/// The replica count an earlier run recorded on this Application:
/// `None` when there is no annotation, `Some(Err(raw))` when there is one that
/// is not a count. Annotation values are strings by definition, so a
/// non-string value is as unusable as `"three"` and is reported the same way.
fn recorded_replicas(app: &Value) -> Option<std::result::Result<i64, String>> {
    // Addressed key-by-key, not with a JSON pointer: the annotation key holds a
    // `/`, which a pointer would read as a path separator.
    let raw = app
        .get("metadata")?
        .get("annotations")?
        .get(PRE_RESTORE_REPLICAS_ANNOTATION)?;
    // Only a JSON string is even a candidate: the apiserver's annotations are
    // `map[string]string`, so anything else came from somewhere that was not
    // the API and is reported rather than coerced.
    let Some(text) = raw.as_str() else {
        return Some(Err(raw.to_string()));
    };
    Some(match text.trim().parse::<i64>() {
        Ok(n) if n >= 0 => Ok(n),
        _ => Err(text.to_string()),
    })
}

/// Record the Argo registrations a step has just gated, keeping the
/// accumulator DISTINCT and in first-seen order.
///
/// `suspended_argo` is ONE accumulator with two writers — [`apply_apps_gated`]
/// on the full path and [`suspend_running_workloads`] on `--data-only` — so
/// the distinctness has to be a property of the list, not of one call site.
/// Every consumer wants a set: [`resume_patches`] issues one merge-patch per
/// entry, and [`interrupted_restore_lines`] both COUNTS the list ("auto-sync
/// is switched OFF on N Application(s)") and prints one recovery command per
/// entry. A repeat makes that count wrong and hands an operator who is already
/// mid-incident the same `kubectl` line twice, to wonder how the two differ.
///
/// Duplicates stopped being hypothetical when the suspend path started joining
/// on what a registration DEPLOYS: every workload of a multi-workload bundle
/// in one namespace resolves to the SAME registration, so an N-workload bundle
/// offers it N times. First-seen order is preserved so the recovery lines read
/// the same way on a re-run.
fn record_suspended_argo(
    into: &mut Vec<(String, String)>,
    found: impl IntoIterator<Item = (String, String)>,
) {
    for entry in found {
        if !into.contains(&entry) {
            into.push(entry);
        }
    }
}

/// Find the user Argo Application(s) that deploy one AppRafter Application.
/// Returns `(namespace, name)` pairs. Used by the data-only suspend path.
///
/// One cluster read of the registrations, then the pure join in
/// [`argo_apps_for_cr`]. The registrations all live in the `argocd`
/// namespace — same list `app_index::AppIndex::read` and
/// `app_rollup::ClusterApplications::read` take.
fn argo_apps_for(
    cr_name: &str,
    cr_namespace: &str,
    kubeconfig: &Path,
) -> Result<Vec<(String, String)>> {
    let items = crate::commands::k8s_helpers::kubectl_get_json_by_selector(
        "application.argoproj.io",
        "",
        Some(crate::commands::app::ARGOCD_NAMESPACE),
        kubeconfig,
    )?;
    Ok(argo_apps_for_cr(cr_name, cr_namespace, &items))
}

/// The registrations that deploy a given workload, found by what each
/// one DEPLOYS (`status.resources[]`) rather than by a label.
///
/// The label `apprafter.io/application` on an Argo object carries the
/// REGISTRATION name, and `app.rs:4756-4776` asserts it legitimately
/// differs from the CR's own name. Selecting on it therefore missed —
/// and a miss here is not benign: `suspend_patches` still emits the
/// scale-to-zero, so Argo self-heals the replicas and the data-only
/// load runs under live pods writing to the database.
///
/// EVERY matching registration is returned, not the first: `app add`'s
/// duplicate guard keys on the Argo object name alone, so two
/// registrations can claim one workload, and leaving one of them
/// auto-syncing reproduces the whole defect.
fn argo_apps_for_cr(cr_name: &str, cr_namespace: &str, argo: &[Value]) -> Vec<(String, String)> {
    let deploying: Vec<Value> = argo
        .iter()
        .filter(|a| {
            crate::commands::app_open::apprafter_app_refs(a)
                .iter()
                .any(|r| {
                    // `CrRef::namespace == None` is UNKNOWN, never a wildcard: a
                    // ref we cannot place must NOT match, because matching it
                    // would disable auto-sync on a registration that may deploy a
                    // same-named workload in an entirely different namespace —
                    // quiescing a stranger's app while leaving ours running.
                    r.name == cr_name && r.namespace.as_deref() == Some(cr_namespace)
                })
        })
        .cloned()
        .collect();
    argo_app_refs(&deploying)
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
/// kinds the extractor writes, read from `DataKind::payload_dir` rather than
/// restated: the restatement is what broke it. The list said `disk`, the
/// extractor has always written `volumes`, and the mismatch meant a sequential
/// run of a `needs.disk` claim restored nothing and said so in a note under a
/// successful restore.
fn find_claim_data_dir(root: &Path) -> Option<PathBuf> {
    fn search(dir: &Path) -> Option<PathBuf> {
        let mut subdirs = Vec::new();
        let mut has_payload = false;
        for e in std::fs::read_dir(dir).ok()?.flatten() {
            let p = e.path();
            if p.is_dir() {
                if p.file_name().and_then(|n| n.to_str()).is_some_and(|name| {
                    backup_core::DataKind::ALL
                        .iter()
                        .any(|kind| kind.payload_dir() == name)
                }) {
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
    // That is phase 1 of three, and the chain above stops one frame
    // before the place `--target` used to be dropped. In full:
    //
    //   restore --reprovision --target X
    //     → bootstrap_all::run(target, …)
    //         phase 1 → apply::run(target_override, …)
    //         phase 2 → kubeconfig::fetch_and_cache(true, target_override)
    //         phase 3 → cluster_bootstrap::run(target_override)
    //             → resolve_state_paths(target_override)       // kubeconfig
    //             → load_active_target_config(store, target_override)  // tier
    //
    // Phase 3 took no argument until finding C1: it re-resolved the
    // ACTIVE target, so restoring into X ran helm + server-side apply
    // against whatever target happened to be active — resetting that
    // cluster's channel, autoUpgrade and root targetRevision. Both
    // resolutions inside phase 3 take the override now; the regression
    // guard is `cluster_bootstrap::tests::
    // phase_three_bootstraps_the_overridden_target_not_the_active_one`.
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
        apply_fails: bool,
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

        /// A helper pod that never becomes Ready (the apply-wait fails).
        fn failing_apply() -> Self {
            Self {
                apply_fails: true,
                ..Self::default()
            }
        }
    }

    impl KubeExec for FakeKube {
        fn apply_and_wait_pod_ready(&self, spec: &Value) -> Result<()> {
            self.applied.borrow_mut().push(spec.clone());
            if self.apply_fails {
                return Err(CliError::Other(
                    "pod did not reach Ready within 300s".into(),
                ));
            }
            Ok(())
        }

        fn exec_stream_to_file(
            &self,
            _pod: &str,
            _ns: &str,
            _argv: &[&str],
            _out: &Path,
            _first_output_within: Option<std::time::Duration>,
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
            secret_namespaces: Vec::new(),
            resources,
        }
    }

    fn resource(kind: &str, ns: &str, name: &str) -> ResourceRef {
        ResourceRef {
            namespace: ns.into(),
            kind: kind.into(),
            name: name.into(),
            claim_type: None,
            no_data: false,
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
        let lines = restore_summary(
            Some(&m),
            Some("prod"),
            false,
            2,
            None,
            None,
            BackupSchedule::NotInherited,
            &EdgeRestore::default(),
        );
        assert_eq!(
            lines,
            vec![
                "✓ Restored backup of cluster 'k3d-demo' into target 'prod'".to_string(),
                "  namespaces: demo, shop".to_string(),
                "  mode:       full".to_string(),
                "  workloads:  2 app(s) resumed".to_string(),
            ]
        );

        let data_only = restore_summary(
            Some(&m),
            None,
            true,
            1,
            Some("mind the gap"),
            None,
            BackupSchedule::NotInherited,
            &EdgeRestore::default(),
        );
        assert_eq!(
            data_only[0],
            "✓ Restored backup of cluster 'k3d-demo' into target '<active>'"
        );
        assert_eq!(data_only[2], "  mode:       data-only");
        assert_eq!(data_only[4], "  ⚠ mind the gap");
    }

    /// E1: `spec.backup` is replayed verbatim, so a clone inherits the
    /// source's backup cluster-name and its snapshots appear in the repository
    /// under it. That is cosmetic — attribution is by the cluster's own
    /// kube-system UID — but it must not be a SURPRISE, so the summary says it
    /// and says how to change it.
    #[test]
    fn restore_summary_reports_an_inherited_backup_cluster_name() {
        let m = manifest_of(&["demo"], vec![]);
        let lines = restore_summary(
            Some(&m),
            Some("new"),
            false,
            1,
            None,
            Some("prod"),
            BackupSchedule::KeptEnabled,
            &EdgeRestore::default(),
        );
        let joined = lines.join("\n");
        assert!(joined.contains("cluster-name 'prod'"), "{joined}");
        assert!(
            joined.contains("apprafter backup set cluster-name"),
            "the line must say how to change it: {joined}"
        );
    }

    /// …and says nothing when there is nothing inherited. A warning on every
    /// restore is a warning nobody reads.
    #[test]
    fn restore_summary_is_silent_when_no_cluster_name_was_inherited() {
        let m = manifest_of(&["demo"], vec![]);
        let lines = restore_summary(
            Some(&m),
            Some("new"),
            false,
            1,
            None,
            None,
            BackupSchedule::NotInherited,
            &EdgeRestore::default(),
        );
        assert!(!lines.join("\n").contains("cluster-name"), "{:?}", lines);
    }

    /// The inherited name is read from the replayed CR, and ONLY when the
    /// backup schedule came with it — a disabled or absent `spec.backup`
    /// changes nothing about the repository, so there is nothing to warn about.
    #[test]
    fn the_inherited_cluster_name_comes_from_an_enabled_backup_block_only() {
        let with = serde_json::json!({"spec": {"backup": {
            "enabled": true, "clusterName": "prod", "bucket": "s3:x"}}});
        assert_eq!(
            inherited_backup_cluster_name(&with).as_deref(),
            Some("prod")
        );

        let disabled = serde_json::json!({"spec": {"backup": {
            "enabled": false, "clusterName": "prod"}}});
        assert_eq!(inherited_backup_cluster_name(&disabled), None);

        let unnamed = serde_json::json!({"spec": {"backup": {"enabled": true}}});
        assert_eq!(inherited_backup_cluster_name(&unnamed), None);

        let none = serde_json::json!({"spec": {}});
        assert_eq!(inherited_backup_cluster_name(&none), None);
    }

    /// With no manifest (the artifact step never completed) the summary still
    /// prints, but claims nothing about the backup's contents.
    #[test]
    fn restore_summary_without_a_manifest_claims_no_scope() {
        let lines = restore_summary(
            None,
            Some("prod"),
            false,
            0,
            None,
            None,
            BackupSchedule::NotInherited,
            &EdgeRestore::default(),
        );
        assert_eq!(
            lines,
            vec![
                "✓ Restored backup into target 'prod'".to_string(),
                "  workloads:  0 app(s) resumed".to_string(),
            ]
        );
    }

    // =======================================================================
    // D1 / D2 — the replayed backup schedule
    // =======================================================================

    /// A captured PlatformStack carrying the source's complete, ENABLED
    /// backup configuration: the whole block a restore replays.
    fn captured_with_backup(enabled: bool) -> Value {
        serde_json::json!({
            "apiVersion": "apprafter.io/v1alpha1",
            "kind": "PlatformStack",
            "metadata": {"name": "default", "namespace": "apprafter-system"},
            "spec": {"backup": {
                "enabled": enabled,
                "schedule": "0 3 * * *",
                "timeZone": "Europe/Berlin",
                "bucket": "s3:https://nbg1.your-objectstorage.com/prod-backups",
                "clusterName": "prod",
                "credentialRef": {"name": "apprafter-backup-s3"},
                "stagingMode": "sequential",
                "checkSchedule": "0 6 * * 0",
                "failureWebhook": "https://hooks.example/backup",
                "retention": {"keepDaily": 14, "keepWeekly": 8,
                              "keepMonthly": 12, "enforce": "cluster"}}}
        })
    }

    /// FIRES: a "no" answer forces the replayed schedule off. Without this the
    /// restored cluster starts writing to the SOURCE's repository on the
    /// source's schedule, with the source possibly still alive and writing to
    /// it too (`moving-to-a-bigger-machine`, Route B).
    #[test]
    fn a_declined_backup_schedule_is_replayed_switched_off() {
        let (cr, schedule) = apply_backup_schedule_policy(&captured_with_backup(true), false);
        assert_eq!(schedule, BackupSchedule::Disabled);
        assert_eq!(cr["spec"]["backup"]["enabled"], serde_json::json!(false));
    }

    /// DOES NOT FIRE: a "yes" answer inherits it enabled. This is the
    /// disaster-recovery case — the source is gone and the clone is
    /// legitimately the repository's new writer.
    #[test]
    fn keep_backup_schedule_inherits_the_source_schedule_enabled() {
        let captured = captured_with_backup(true);
        let (cr, schedule) = apply_backup_schedule_policy(&captured, true);
        assert_eq!(schedule, BackupSchedule::KeptEnabled);
        assert_eq!(cr["spec"]["backup"]["enabled"], serde_json::json!(true));
        assert_eq!(cr, captured, "the CR must be replayed byte-for-byte");
    }

    /// ONLY `enabled` is forced. Re-enabling afterwards has to be one word,
    /// not a re-entry of bucket, credential, retention, timezone and schedule
    /// — which an operator restoring at 3am does not have to hand.
    #[test]
    fn disabling_the_schedule_preserves_every_other_backup_field() {
        let captured = captured_with_backup(true);
        let (cr, _) = apply_backup_schedule_policy(&captured, false);
        let before = &captured["spec"]["backup"];
        let after = &cr["spec"]["backup"];
        for key in [
            "schedule",
            "timeZone",
            "bucket",
            "clusterName",
            "credentialRef",
            "stagingMode",
            "checkSchedule",
            "failureWebhook",
            "retention",
        ] {
            assert_eq!(after[key], before[key], "{key} must survive the restore");
        }
        // Including the retention sub-fields, `enforce` above all: a source
        // pruning with `enforce: cluster` hands that to the clone.
        assert_eq!(after["retention"]["enforce"], serde_json::json!("cluster"));
        assert_eq!(after["retention"]["keepDaily"], serde_json::json!(14));
        // …and nothing outside spec.backup is touched.
        assert_eq!(cr["metadata"], captured["metadata"]);
    }

    /// The payload that actually reaches the apiserver carries the answer.
    ///
    /// FIRES on the declining half; the inheriting half below is what
    /// stops this passing on a payload builder that hard-codes `false`. The
    /// pair exists because the policy being right is not the same claim as the
    /// bytes being right — an `apply` of the CAPTURED CR next to a correct
    /// decision is the defect back with every other test green.
    #[test]
    fn the_applied_payload_carries_the_schedule_decision() {
        let captured = captured_with_backup(true);
        let flags = |keep, discard| ScheduleFlags {
            keep,
            discard,
            is_tty: false,
        };

        let (yaml, schedule) = platformstack_apply_payload(&captured, flags(false, true)).unwrap();
        assert_eq!(schedule, BackupSchedule::Disabled);
        let sent: Value = serde_json::from_str(&yaml).expect("the payload is JSON (valid YAML)");
        assert_eq!(sent["spec"]["backup"]["enabled"], serde_json::json!(false));
        assert_eq!(
            sent["spec"]["backup"]["bucket"], captured["spec"]["backup"]["bucket"],
            "the rest of the block still goes to the cluster"
        );

        let (yaml, schedule) = platformstack_apply_payload(&captured, flags(true, false)).unwrap();
        assert_eq!(schedule, BackupSchedule::KeptEnabled);
        let sent: Value = serde_json::from_str(&yaml).unwrap();
        assert_eq!(sent["spec"]["backup"]["enabled"], serde_json::json!(true));
        assert_eq!(sent, captured, "inheriting replays the CR verbatim");
    }

    /// …and an UNANSWERED question yields no payload at all.
    ///
    /// This is the one that stops the refusal being decided correctly and then
    /// ignored: `ApplyPlatformStack` has nothing to send when this errors, so
    /// a non-interactive restore cannot reach the apiserver with either answer
    /// it did not give. The `--data-only` half is the same claim from the
    /// other side — that mode never calls this at all (see
    /// `every_mode_that_replays_the_platformstack_goes_through_the_policy`).
    #[test]
    fn an_unanswered_schedule_question_produces_no_payload() {
        let err = platformstack_apply_payload(
            &captured_with_backup(true),
            ScheduleFlags {
                keep: false,
                discard: false,
                is_tty: false,
            },
        )
        .expect_err("there is nothing safe to apply until the question is answered");
        assert!(err.to_string().contains("--keep-backup-schedule"), "{err}");

        // …while a backup that never had a schedule still builds one, because
        // there was never a question to answer.
        let (yaml, schedule) = platformstack_apply_payload(
            &captured_with_backup(false),
            ScheduleFlags {
                keep: false,
                discard: false,
                is_tty: false,
            },
        )
        .expect("no enabled schedule, no question, no refusal");
        assert_eq!(schedule, BackupSchedule::NotInherited);
        assert!(yaml.contains("\"enabled\":false"), "{yaml}");
    }

    /// A backup that was already off, or absent, is not an inheritance — in
    /// EITHER direction. A flag that invented a warning here would make
    /// `--keep-backup-schedule` look like it did something.
    #[test]
    fn a_backup_that_was_not_enabled_is_never_reported_as_inherited() {
        for keep in [true, false] {
            let (cr, schedule) = apply_backup_schedule_policy(&captured_with_backup(false), keep);
            assert_eq!(schedule, BackupSchedule::NotInherited, "keep={keep}");
            assert_eq!(cr["spec"]["backup"]["enabled"], serde_json::json!(false));

            let bare = serde_json::json!({"spec": {}});
            let (cr, schedule) = apply_backup_schedule_policy(&bare, keep);
            assert_eq!(schedule, BackupSchedule::NotInherited, "keep={keep}");
            assert_eq!(cr, bare, "nothing to rewrite, so nothing is invented");
        }
    }

    /// FIRES: a declined schedule is announced. A schedule silently switched
    /// off is the same class of surprise as one silently switched on — an
    /// operator who believes the clone is backing itself up has an unbacked
    /// cluster.
    #[test]
    fn the_summary_says_the_schedule_was_restored_disabled() {
        let m = manifest_of(&["demo"], vec![]);
        let joined = restore_summary(
            Some(&m),
            Some("new"),
            false,
            1,
            None,
            Some("prod"),
            BackupSchedule::Disabled,
            &EdgeRestore::default(),
        )
        .join("\n");
        assert!(joined.contains("DISABLED"), "{joined}");
        assert!(
            joined.contains("apprafter backup set enabled true"),
            "names the way back on — and the one that changes ONLY this field, since \
             `backup enable` recomposes the whole block from its flags: {joined}"
        );
        assert!(
            joined.contains("--keep-backup-schedule"),
            "names the opt-out: {joined}"
        );
        // …and the cluster-name line is phrased against it, so the two read as
        // one paragraph rather than two unrelated warnings.
        assert!(
            joined.contains("came with it, so once you enable it"),
            "the name line must follow from the schedule line: {joined}"
        );
    }

    /// DOES NOT FIRE THE SAME WAY: when the answer was "inherit", the summary
    /// says the opposite thing. A silent inheritance is exactly what the
    /// question exists to stop, and an answered one is no less worth recording
    /// — including the consequence, which is what the operator was warned
    /// about at the prompt and has to recognise here.
    #[test]
    fn the_summary_says_the_schedule_was_kept_when_it_was_inherited() {
        let m = manifest_of(&["demo"], vec![]);
        let joined = restore_summary(
            Some(&m),
            Some("new"),
            false,
            1,
            None,
            Some("prod"),
            BackupSchedule::KeptEnabled,
            &EdgeRestore::default(),
        )
        .join("\n");
        assert!(joined.contains("ENABLED"), "{joined}");
        assert!(
            joined.contains("as you asked"),
            "an inheritance is only acceptable because it was answered for: {joined}"
        );
        assert!(
            joined.contains("both are now writing to that one repository"),
            "names the two-writer consequence: {joined}"
        );
        assert!(
            joined.contains("apprafter backup disable"),
            "names the way back off: {joined}"
        );
        assert!(
            !joined.contains("DISABLED"),
            "must not also claim it was disabled: {joined}"
        );
        assert!(
            joined.contains("came with it, so from now on"),
            "the name line follows the KEPT phrasing: {joined}"
        );
    }

    /// `--data-only` replays no CR at all, so there is no schedule to disable
    /// and nothing to report. The summary must stay quiet — the mode is
    /// already unaffected by the defect and must not acquire a warning.
    #[test]
    fn a_data_only_restore_says_nothing_about_the_backup_schedule() {
        let m = manifest_of(&["demo"], vec![]);
        let joined = restore_summary(
            Some(&m),
            Some("new"),
            true,
            1,
            None,
            None,
            BackupSchedule::NotInherited,
            &EdgeRestore::default(),
        )
        .join("\n");
        assert!(!joined.contains("schedule"), "{joined}");
        assert!(!joined.contains("DISABLED"), "{joined}");
    }

    /// FIRES on all four combinations of (flag given / not) × (TTY / not),
    /// which is the whole decision. The interesting cell is the last one: a
    /// non-interactive restore with no flag must REFUSE rather than pick a
    /// side, because the side it would pick silently is the one that redirects
    /// a live cluster's backups.
    #[test]
    fn the_schedule_decision_covers_flags_times_tty() {
        // No flag.
        assert_eq!(
            schedule_decision(true, false, false, true),
            ScheduleChoice::Prompt
        );
        assert_eq!(
            schedule_decision(true, false, false, false),
            ScheduleChoice::ErrorNonInteractive
        );
        // A flag — the answer is already given, so neither branch asks, on a
        // terminal or off one.
        for tty in [true, false] {
            assert_eq!(
                schedule_decision(true, true, false, tty),
                ScheduleChoice::Inherit,
                "tty={tty}"
            );
            assert_eq!(
                schedule_decision(true, false, true, tty),
                ScheduleChoice::Disable,
                "tty={tty}"
            );
        }
    }

    /// DOES NOT FIRE: a backup with no enabled schedule asks nothing, refuses
    /// nothing, and says nothing — on a terminal or off one, with or without a
    /// flag. Most clusters have never configured a backup, and a restore that
    /// interrogated them about a schedule that does not exist would be a
    /// question with no right answer.
    #[test]
    fn a_backup_with_no_enabled_schedule_is_never_asked_about() {
        for (keep, discard, tty) in [
            (false, false, true),
            (false, false, false),
            (true, false, false),
            (false, true, true),
        ] {
            assert_eq!(
                schedule_decision(false, keep, discard, tty),
                ScheduleChoice::NothingToInherit,
                "keep={keep} discard={discard} tty={tty}"
            );
        }
    }

    /// Both flags at once cannot come from the CLI (clap `conflicts_with`),
    /// and if it ever did, the reversible half wins. Pinned so a later
    /// re-ordering of the branches cannot silently make "keep" the tiebreak.
    #[test]
    fn both_flags_at_once_resolve_to_the_reversible_half() {
        assert_eq!(
            schedule_decision(true, true, true, false),
            ScheduleChoice::Disable
        );
    }

    /// The prompt has to state the consequence CONCRETELY: which repository,
    /// and what happens if the source cluster is still alive. An operator
    /// answering "is this the DR case or the move case" needs the bucket in
    /// front of them — it is the one fact that distinguishes the two.
    #[test]
    fn the_prompt_names_the_repository_and_the_two_writer_consequence() {
        let text = schedule_prompt_text(&captured_with_backup(true));
        assert!(
            text.contains("'s3:https://nbg1.your-objectstorage.com/prod-backups'"),
            "{text}"
        );
        assert!(text.contains("0 3 * * *"), "{text}");
        assert!(text.contains("Europe/Berlin"), "{text}");
        assert!(
            text.contains("still running") && text.contains("both clusters"),
            "the prompt must name what two live writers means: {text}"
        );
        assert!(
            text.contains("apprafter backup set enabled true"),
            "and what answering no leaves behind: {text}"
        );
    }

    /// A source that recorded no bucket still produces a readable question
    /// rather than an empty pair of quotes.
    #[test]
    fn the_prompt_survives_a_backup_block_with_no_bucket() {
        let bare = serde_json::json!({"spec": {"backup": {"enabled": true}}});
        let text = schedule_prompt_text(&bare);
        assert!(text.contains("the source's repository"), "{text}");
        assert!(!text.contains("''"), "{text}");
    }

    /// The wrapper the apply path actually calls turns each decision into the
    /// answer `apply_backup_schedule_policy` consumes — and turns the refusal
    /// into an ERROR rather than a quiet `false`. Every branch except `Prompt`,
    /// which needs a terminal; a decision function that is right next to a
    /// caller that swallowed it would be the defect back with the four
    /// combinations above still green.
    #[test]
    fn resolve_schedule_choice_answers_every_branch_it_can_without_a_terminal() {
        let enabled = captured_with_backup(true);
        let off = captured_with_backup(false);
        let flags = |keep, discard, is_tty| ScheduleFlags {
            keep,
            discard,
            is_tty,
        };

        assert!(resolve_schedule_choice(&enabled, flags(true, false, false)).unwrap());
        assert!(!resolve_schedule_choice(&enabled, flags(false, true, false)).unwrap());
        assert!(!resolve_schedule_choice(&off, flags(false, false, true)).unwrap());

        let err = resolve_schedule_choice(&enabled, flags(false, false, false))
            .expect_err("no terminal and no flag must STOP the restore, not pick a side");
        assert!(
            err.to_string().contains("--discard-backup-schedule"),
            "{err}"
        );
    }

    /// The refusal names BOTH flags and what each does. It is read out of a CI
    /// log by someone with no terminal to experiment on, so "pass a flag" is
    /// not enough — which flag, and what it means, has to be in the message.
    #[test]
    fn the_non_interactive_refusal_names_both_flags_and_the_repository() {
        let err = non_interactive_schedule_error(&captured_with_backup(true)).to_string();
        assert!(err.contains("--keep-backup-schedule"), "{err}");
        assert!(err.contains("--discard-backup-schedule"), "{err}");
        assert!(
            err.contains("'s3:https://nbg1.your-objectstorage.com/prod-backups'"),
            "{err}"
        );
        assert!(
            err.contains("two clusters on one repository"),
            "says why it will not guess: {err}"
        );
    }

    /// D2: the question covers `--data-only` by exclusion and both replaying
    /// modes by inclusion. `ApplyPlatformStack` is what carries `spec.backup`
    /// AND the only step that asks, and `Reprovision` only PREPENDS a step —
    /// so restore-into-running is exposed to exactly the same inheritance as a
    /// rebuild, and `--data-only` neither prompts nor refuses because it never
    /// reaches the step that would.
    #[test]
    fn every_mode_that_replays_the_platformstack_goes_through_the_policy() {
        for mode in [RestoreMode::IntoRunning, RestoreMode::Reprovision] {
            assert!(
                restore_steps(mode, false).contains(&RestoreStep::ApplyPlatformStack),
                "{mode:?} replays the CR, so it must be gated"
            );
        }
        assert!(
            !restore_steps(RestoreMode::IntoRunning, true)
                .contains(&RestoreStep::ApplyPlatformStack),
            "--data-only replays no CR and must stay that way"
        );
    }

    // =======================================================================
    // A1 / A4 — the edge configuration a restore inherits
    // =======================================================================

    /// A captured PlatformStack with two registered domains, both pointing at
    /// the same imported certificate — the shape `target domain add` writes.
    fn platformstack_with_domains(refs: &[&str]) -> Value {
        let domains: Vec<Value> = refs
            .iter()
            .enumerate()
            .map(|(i, r)| {
                serde_json::json!({
                    "domain": format!("zone{i}.example"),
                    "certMode": "imported",
                    "importedCertRef": r,
                })
            })
            .collect();
        serde_json::json!({"spec": {"values": {"gateway": {"allowedDomains": domains}}}})
    }

    /// The certificate references come from the DOMAINS, which is what the
    /// chart renders into the Gateway's `tls.certificateRefs` — so this is
    /// literally "what does this cluster need a certificate for".
    #[test]
    fn referenced_cert_names_reads_the_domains_deduped() {
        let ps = platformstack_with_domains(&["cf-origin", "cf-origin", "other"]);
        assert_eq!(
            referenced_cert_names(&ps),
            vec!["cf-origin".to_string(), "other".to_string()]
        );

        // A cluster with no domains needs no certificate — and a
        // PlatformStack that predates the gateway block must not panic.
        assert!(referenced_cert_names(&serde_json::json!({"spec": {}})).is_empty());
        assert!(referenced_cert_names(&platformstack_with_domains(&[])).is_empty());
    }

    /// THE A1 detector: a domain that came back with the PlatformStack whose
    /// certificate did NOT come back with the snapshot. Every backup taken
    /// before certificates were captured has exactly this shape.
    #[test]
    fn a_domain_whose_certificate_the_snapshot_lacks_is_reported_dangling() {
        assert_eq!(
            dangling_cert_refs(&["cf-origin".into(), "other".into()], &["other".into()]),
            vec!["cf-origin".to_string()]
        );
        // …and nothing is reported when the certificate came back with it.
        assert!(dangling_cert_refs(&["cf-origin".into()], &["cf-origin".into()]).is_empty());
        // A cluster with no domains has nothing to dangle, whatever was
        // restored.
        assert!(dangling_cert_refs(&[], &["cf-origin".into()]).is_empty());
    }

    /// The staged certificates are read back as whole objects, in a stable
    /// order, and a snapshot that carries no `certs/` directory — every backup
    /// taken before this existed — reads as "none" rather than failing.
    #[test]
    fn read_imported_certs_reads_whole_objects_and_tolerates_no_certs_dir() {
        let dir = tempfile::tempdir().unwrap();
        assert!(read_imported_certs(dir.path()).unwrap().is_empty());

        let certs = dir.path().join("certs");
        std::fs::create_dir_all(&certs).unwrap();
        std::fs::write(
            certs.join("b-cert.json"),
            r#"{"kind":"Secret","type":"kubernetes.io/tls",
                "metadata":{"name":"b-cert","namespace":"apprafter-system",
                            "labels":{"apprafter.io/cert-mode":"imported"}},
                "data":{"tls.crt":"Q1JU"}}"#,
        )
        .unwrap();
        std::fs::write(
            certs.join("a-cert.json"),
            r#"{"metadata":{"name":"a-cert"}}"#,
        )
        .unwrap();
        std::fs::write(certs.join("notes.txt"), "not a cert").unwrap();

        let read = read_imported_certs(dir.path()).unwrap();
        assert_eq!(
            read.iter().filter_map(object_name).collect::<Vec<_>>(),
            vec!["a-cert".to_string(), "b-cert".to_string()],
            "sorted, and only the .json files"
        );
        // The import label survives the round trip — `target domain add`
        // checks it, and the next backup's capture keys on it.
        assert_eq!(
            read[1]["metadata"]["labels"]["apprafter.io/cert-mode"],
            serde_json::json!("imported")
        );
        assert_eq!(read[1]["type"], serde_json::json!("kubernetes.io/tls"));
    }

    /// INVARIANT: a certificate staged WITHOUT its type gets `v1`/`Secret`
    /// back. `ApplyImportedCerts` is the FIRST step of a restore that applies a
    /// captured object, so on a cluster with an imported certificate this is
    /// exactly where a snapshot from the old in-cluster runner died:
    ///
    /// ```text
    /// error validating "STDIN": error validating data:
    /// [apiVersion not set, kind not set]
    /// ```
    ///
    /// Nothing is looked up: an imported certificate is a plain
    /// `kubernetes.io/tls` Secret by construction — `target cert import`
    /// applies it as one and the capture sweep keys on its label.
    #[test]
    fn read_imported_certs_types_a_certificate_a_broken_snapshot_staged_without_one() {
        let dir = tempfile::tempdir().unwrap();
        let certs = dir.path().join("certs");
        std::fs::create_dir_all(&certs).unwrap();
        // Byte-for-byte the shape the old runner staged.
        std::fs::write(
            certs.join("cf-cert.json"),
            r#"{"data":{"tls.crt":"eA=="},
                "metadata":{"name":"cf-cert","namespace":"apprafter-system"},
                "type":"kubernetes.io/tls"}"#,
        )
        .unwrap();
        // …and one that already states its type, which must not be rewritten.
        std::fs::write(
            certs.join("zz-cert.json"),
            r#"{"apiVersion":"v1","kind":"Secret","metadata":{"name":"zz-cert"}}"#,
        )
        .unwrap();

        let read = read_imported_certs(dir.path()).unwrap();
        assert_eq!(read[0]["apiVersion"], serde_json::json!("v1"));
        assert_eq!(read[0]["kind"], serde_json::json!("Secret"));
        // The material and the Secret type are untouched.
        assert_eq!(read[0]["type"], serde_json::json!("kubernetes.io/tls"));
        assert_eq!(read[0]["data"]["tls.crt"], serde_json::json!("eA=="));
        assert_eq!(read[1]["apiVersion"], serde_json::json!("v1"));
        assert_eq!(read[1]["kind"], serde_json::json!("Secret"));
    }

    /// A4: the intent a restore acts on comes out of the replayed
    /// `PlatformStack` — the object every backup mode captures — and NOT out
    /// of the manifest, which only a hand-run `backup create` could fill.
    ///
    /// This asserts the read on the CR shape the capture actually stages
    /// (`sanitize_cr` keeps `spec` whole and drops `status`), including the
    /// two absences that must stay UNKNOWN: a cluster whose operator never
    /// ran the toggle, and a snapshot from the intervening tree that recorded
    /// the intent in the manifest instead — where the CR looks like any other
    /// pre-change CR.
    #[test]
    fn the_replayed_platformstack_is_where_the_origin_firewall_comes_from() {
        use crate::commands::target_firewall::recorded_origin_firewall;

        let captured = serde_json::json!({
            "apiVersion": "apprafter.io/v1alpha1",
            "kind": "PlatformStack",
            "metadata": {"name": "default", "namespace": "apprafter-system"},
            "spec": {
                "channel": "stable",
                "firewall": {"cloudflareOrigin": true},
                "values": {"tier": 1}
            }
        });
        assert_eq!(recorded_origin_firewall(&captured), Some(true));

        let pre_change = serde_json::json!({
            "kind": "PlatformStack",
            "spec": {"channel": "stable", "values": {"tier": 1}}
        });
        assert_eq!(
            recorded_origin_firewall(&pre_change),
            None,
            "a snapshot that never recorded an answer must stay UNKNOWN — \
             `Some(false)` here would have the summary claim the source \
             served its 80/443 open"
        );
    }

    /// The carry hangs off `ApplyPlatformStack`, so the modes that replay no
    /// configuration cannot carry it: `--data-only` exists precisely to make
    /// no config writes, and reconciling a cloud firewall from it would be
    /// one.
    #[test]
    fn a_data_only_restore_has_no_platformstack_step_to_carry_the_firewall_from() {
        let steps = restore_steps(RestoreMode::IntoRunning, true);
        assert!(!steps.contains(&RestoreStep::ApplyPlatformStack));

        for mode in [RestoreMode::IntoRunning, RestoreMode::Reprovision] {
            assert!(restore_steps(mode, false).contains(&RestoreStep::ApplyPlatformStack));
        }
    }

    /// A4, the whole decision table. The carry is one-directional and only on
    /// the mode that provisioned the node; an UNKNOWN intent changes nothing
    /// and says nothing.
    #[test]
    fn the_origin_firewall_is_carried_only_when_recorded_on_and_a_node_was_provisioned() {
        use OriginFirewallAction::*;
        let cases = [
            // (recorded, reprovisioned, destination already on) -> action
            ((Some(true), true, false), Carry),
            ((Some(true), false, false), Announce),
            // A snapshot from before the field existed, or from the
            // in-cluster runner: UNKNOWN, so nothing is claimed either way.
            ((None, true, false), Nothing),
            ((None, false, false), Nothing),
            // Recorded OFF never turns a destination's firewall off — the
            // carry can only ever RESTRICT ports.
            ((Some(false), true, false), Nothing),
            ((Some(false), true, true), Nothing),
            // Already on: nothing to carry, and nothing worth a line.
            ((Some(true), true, true), Nothing),
            ((Some(true), false, true), Nothing),
        ];
        for ((recorded, reprovisioned, already), expected) in cases {
            assert_eq!(
                origin_firewall_action(recorded, reprovisioned, already),
                expected,
                "recorded={recorded:?} reprovisioned={reprovisioned} already_on={already}"
            );
        }
    }

    /// FIRES: the certificate that came back is named, in the same paragraph
    /// as the backup-config inheritance — it is the same kind of fact, and the
    /// operator reading it has the same question.
    #[test]
    fn the_summary_says_the_imported_certificate_came_with_the_restore() {
        let m = manifest_of(&["demo"], vec![]);
        let edge = EdgeRestore {
            certs_restored: 1,
            ..Default::default()
        };
        let joined = restore_summary(
            Some(&m),
            Some("new"),
            false,
            1,
            None,
            Some("prod"),
            BackupSchedule::Disabled,
            &edge,
        )
        .join("\n");
        assert!(
            joined.contains("imported TLS certificate(s) came with the restore"),
            "{joined}"
        );
        assert!(joined.contains("apprafter target domain list"), "{joined}");
        // …and it is not mistaken for a problem.
        assert!(!joined.contains("MISSING"), "{joined}");
    }

    /// FIRES: a pre-A1 snapshot brought the domains back without the
    /// certificate. The summary has to name it and say what to do, because
    /// nothing else in the cluster will — the Gateway just stops serving TLS.
    #[test]
    fn the_summary_names_a_certificate_the_snapshot_did_not_carry() {
        let m = manifest_of(&["demo"], vec![]);
        let edge = EdgeRestore {
            dangling_certs: vec!["cf-origin".into()],
            ..Default::default()
        };
        let joined = restore_summary(
            Some(&m),
            Some("new"),
            false,
            0,
            None,
            None,
            BackupSchedule::NotInherited,
            &edge,
        )
        .join("\n");
        assert!(joined.contains("cf-origin"), "{joined}");
        assert!(
            joined.contains("apprafter target cert import"),
            "names the one command that fixes it: {joined}"
        );
        assert!(
            joined.contains("no `target domain add` to repeat"),
            "A3: the documented runbook's second step HARD-FAILS after a \
             restore, because the domain came back in the snapshot: {joined}"
        );
    }

    /// FIRES: the carried origin firewall is a side effect on ANOTHER target's
    /// local config plus its live cloud firewall. Said out loud, and it names
    /// the target it wrote to.
    #[test]
    fn the_summary_says_the_origin_firewall_was_carried_to_the_destination_target() {
        let m = manifest_of(&["demo"], vec![]);
        let edge = EdgeRestore {
            origin_firewall: OriginFirewall::Carried {
                target: "bigger".into(),
            },
            ..Default::default()
        };
        let joined = restore_summary(
            Some(&m),
            Some("bigger"),
            false,
            1,
            None,
            None,
            BackupSchedule::NotInherited,
            &edge,
        )
        .join("\n");
        assert!(joined.contains("Cloudflare origin firewall"), "{joined}");
        assert!(joined.contains("target 'bigger'"), "{joined}");
        assert!(
            joined.contains("cloudflare-origin disable"),
            "names the way back: {joined}"
        );
    }

    /// DOES NOT FIRE THE SAME WAY: on a mode that provisioned nothing the
    /// summary says the snapshot RECORDED it and that nothing here changed.
    /// Claiming a cluster's ports were restricted when nothing touched them is
    /// the failure this whole line exists to prevent.
    #[test]
    fn a_restore_that_provisioned_nothing_announces_the_firewall_without_claiming_it() {
        let m = manifest_of(&["demo"], vec![]);
        let edge = EdgeRestore {
            origin_firewall: OriginFirewall::Announced,
            ..Default::default()
        };
        let joined = restore_summary(
            Some(&m),
            None,
            false,
            1,
            None,
            None,
            BackupSchedule::NotInherited,
            &edge,
        )
        .join("\n");
        assert!(joined.contains("recorded the source's"), "{joined}");
        assert!(joined.contains("no firewall was changed"), "{joined}");
        assert!(
            joined.contains("cloudflare-origin enable"),
            "names the way to turn it on: {joined}"
        );
    }

    /// A carry whose live reconcile failed must not read like a carry that
    /// worked. The ports are open, and the line says so.
    #[test]
    fn a_carried_but_unenforced_firewall_says_the_ports_are_still_open() {
        let m = manifest_of(&["demo"], vec![]);
        let edge = EdgeRestore {
            origin_firewall: OriginFirewall::CarriedNotEnforced {
                target: "bigger".into(),
                why: "no firewall found for 'platform-1'".into(),
                recorded: true,
            },
            ..Default::default()
        };
        let joined = restore_summary(
            Some(&m),
            Some("bigger"),
            false,
            1,
            None,
            None,
            BackupSchedule::NotInherited,
            &edge,
        )
        .join("\n");
        assert!(joined.contains("still open to the internet"), "{joined}");
        assert!(joined.contains("no firewall found"), "{joined}");
        assert!(joined.contains("apprafter target use bigger"), "{joined}");
        assert!(
            joined.contains("records it"),
            "the toggle IS on disk, so the next apply fixes it: {joined}"
        );
        assert!(
            joined.contains("next `apprafter apply`"),
            "…and the line says so: {joined}"
        );
    }

    /// …and a carry that never reached the target's config must not claim it
    /// did. The two failures differ in whether the operator's next `apprafter
    /// apply` closes the ports on its own, so the line cannot be the same one.
    #[test]
    fn a_carry_that_never_reached_the_target_does_not_claim_it_was_recorded() {
        let m = manifest_of(&["demo"], vec![]);
        let edge = EdgeRestore {
            origin_firewall: OriginFirewall::CarriedNotEnforced {
                target: "bigger".into(),
                why: "target `bigger` not found".into(),
                recorded: false,
            },
            ..Default::default()
        };
        let joined = restore_summary(
            Some(&m),
            Some("bigger"),
            false,
            1,
            None,
            None,
            BackupSchedule::NotInherited,
            &edge,
        )
        .join("\n");
        assert!(joined.contains("could not be applied to target 'bigger' at all"));
        assert!(
            !joined.contains("records it"),
            "nothing was recorded — saying otherwise sends the operator to an \
             `apprafter apply` that will not close the ports: {joined}"
        );
        assert!(!joined.contains("next `apprafter apply`"), "{joined}");
        assert!(joined.contains("still open to the internet"), "{joined}");
    }

    /// …and a restore with nothing to report about the edge says NOTHING. A
    /// paragraph that appears on every restore is a paragraph nobody reads.
    #[test]
    fn a_restore_with_no_edge_configuration_says_nothing_about_it() {
        assert!(edge_inheritance_lines(&EdgeRestore::default()).is_empty());
    }

    // =======================================================================
    // RestoreArtifact — the D26 path
    // =======================================================================

    /// The restore target's `kube-system` UID. The listings below use the
    /// LEGACY tag shape, which by the stated rule counts as this cluster's.
    const TEST_CLUSTER_UID: &str = "11111111-2222-3333-4444-555555555555";

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

        let dd =
            restore_artifact_tree(&restic, "latest", root.path(), Some(TEST_CLUSTER_UID)).unwrap();

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

        let dd =
            restore_artifact_tree(&restic, "latest", root.path(), Some(TEST_CLUSTER_UID)).unwrap();

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

        let dd =
            restore_artifact_tree(&restic, "latest", root.path(), Some(TEST_CLUSTER_UID)).unwrap();

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
        let e = restore_artifact_tree(&restic, "latest", root.path(), Some(TEST_CLUSTER_UID))
            .unwrap_err();
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

    /// INVARIANT: a CR staged WITHOUT `apiVersion`/`kind` gets them back before
    /// anything tries to apply it.
    ///
    /// Every snapshot the in-cluster runner wrote before the capture-side fix
    /// is in this shape — kube-rs list items carry no type, and the runner
    /// staged them verbatim. The restore's own routing never noticed (it takes
    /// the kind from the FILENAME), but `kubectl apply --server-side` rejects
    /// such a body with `[apiVersion not set, kind not set]`, so those
    /// snapshots were unrestorable. New backups being correct does not help
    /// anyone already holding one, so the repair happens on the READ.
    #[test]
    fn read_crs_types_an_object_a_broken_snapshot_staged_without_one() {
        let dd = tempfile::tempdir().unwrap();
        // Exactly what the old runner staged: body, no type.
        write_at(
            dd.path(),
            "crs/0-PlatformStack-apprafter-system-default.json",
            r#"{"metadata":{"name":"default"},"spec":{"channel":"stable"}}"#,
        );
        write_at(
            dd.path(),
            "crs/1-SourceCredential-apprafter-system-gh.json",
            r#"{"metadata":{"name":"gh"}}"#,
        );
        write_at(
            dd.path(),
            "crs/2-Application-demo-web.json",
            r#"{"metadata":{"name":"web"}}"#,
        );
        write_at(
            dd.path(),
            "crs/3-ArgoApplication-argocd-web-prod.json",
            r#"{"metadata":{"name":"web-prod"}}"#,
        );
        write_at(
            dd.path(),
            "crs/4-SharedVolume-demo-assets.json",
            r#"{"metadata":{"name":"assets"}}"#,
        );

        let crs = read_crs(dd.path()).unwrap();
        let typed: Vec<(&str, &str)> = crs
            .iter()
            .map(|c| {
                (
                    c.cr["apiVersion"].as_str().unwrap_or("<missing>"),
                    c.cr["kind"].as_str().unwrap_or("<missing>"),
                )
            })
            .collect();
        assert_eq!(
            typed,
            vec![
                ("apprafter.io/v1alpha1", "PlatformStack"),
                ("apprafter.io/v1alpha1", "SourceCredential"),
                ("apprafter.io/v1alpha1", "Application"),
                // The backup's `ArgoApplication` tag is a FILENAME convention;
                // the object is an argoproj.io `Application` and must be
                // applied as one, or it lands on the wrong CRD.
                ("argoproj.io/v1alpha1", "Application"),
                ("apprafter.io/v1alpha1", "SharedVolume"),
            ]
        );
        // The routing tag is unchanged — the repair must not renumber which
        // step picks the object up.
        assert_eq!(crs[3].kind, "ArgoApplication");
        // …and the body is untouched.
        assert_eq!(crs[0].cr["spec"]["channel"], serde_json::json!("stable"));
    }

    /// INVARIANT: the repair only ever FILLS IN. A snapshot taken by the CLI
    /// (whose `kubectl get -o json` does carry the types) restores byte-for-byte
    /// as before — including a body whose `kind` deliberately differs from the
    /// filename tag, which is the `ArgoApplication` case and the one a
    /// filename-derived overwrite would corrupt.
    #[test]
    fn read_crs_never_overwrites_a_type_the_snapshot_carries() {
        let dd = tempfile::tempdir().unwrap();
        write_at(
            dd.path(),
            "crs/0-ArgoApplication-argocd-web.json",
            r#"{"apiVersion":"argoproj.io/v1alpha1","kind":"Application",
                "metadata":{"name":"web"}}"#,
        );
        // A future kind this restore has no mapping for still LOADS, because a
        // body that states its own type needs no lookup.
        write_at(
            dd.path(),
            "crs/1-FutureThing-demo-x.json",
            r#"{"apiVersion":"apprafter.io/v1beta9","kind":"FutureThing",
                "metadata":{"name":"x"}}"#,
        );

        let crs = read_crs(dd.path()).unwrap();
        assert_eq!(
            crs[0].cr["apiVersion"],
            serde_json::json!("argoproj.io/v1alpha1")
        );
        assert_eq!(crs[0].cr["kind"], serde_json::json!("Application"));
        assert_eq!(
            crs[1].cr["apiVersion"],
            serde_json::json!("apprafter.io/v1beta9")
        );
        assert_eq!(crs[1].cr["kind"], serde_json::json!("FutureThing"));
    }

    /// A HALF-typed body is completed, not left half-applied: `kubectl` rejects
    /// on either field alone.
    #[test]
    fn read_crs_fills_in_only_the_half_of_the_type_that_is_missing() {
        let dd = tempfile::tempdir().unwrap();
        write_at(
            dd.path(),
            "crs/0-Application-demo-web.json",
            r#"{"kind":"Application","metadata":{"name":"web"}}"#,
        );
        write_at(
            dd.path(),
            "crs/1-SharedVolume-demo-assets.json",
            r#"{"apiVersion":"apprafter.io/v1alpha1","metadata":{"name":"assets"}}"#,
        );
        let crs = read_crs(dd.path()).unwrap();
        assert_eq!(
            crs[0].cr["apiVersion"],
            serde_json::json!("apprafter.io/v1alpha1")
        );
        assert_eq!(crs[1].cr["kind"], serde_json::json!("SharedVolume"));
    }

    /// INVARIANT: an untypeable object is LOUD, never skipped. A restore that
    /// quietly dropped a CR it could not name would leave a half-restored
    /// cluster that reported success — the one outcome worse than failing.
    #[test]
    fn read_crs_refuses_an_untyped_object_whose_kind_it_cannot_determine() {
        let dd = tempfile::tempdir().unwrap();
        write_at(
            dd.path(),
            "crs/0-Mystery-demo-thing.json",
            r#"{"metadata":{"name":"thing"}}"#,
        );
        let err = read_crs(dd.path()).expect_err("an unknown kind tag must fail the restore");
        let msg = format!("{err}");
        assert!(
            msg.contains("0-Mystery-demo-thing.json"),
            "the message must name the file: {msg}"
        );
        assert!(
            msg.contains("Mystery"),
            "…and the tag it could not type: {msg}"
        );
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
            m.manifest_version, 1,
            "a shipped v1 manifest carries no manifestVersion and must read as v1 — \
             not as whatever version this build writes"
        );
        assert!(
            m.manifest_version <= MANIFEST_VERSION_CURRENT,
            "…and a version this build can read, so the restore is not refused"
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
    fn a_namespace_that_only_holds_secrets_is_still_created() {
        // Restore applies each captured SealedSecret into its namespace,
        // and `kubectl apply` fails when the namespace is not there. The
        // app namespaces alone are not enough any more: a secret staged
        // ahead of a deployment lives in a namespace that no Application
        // will create, and dropping it would lose exactly the credentials
        // the widened capture exists to keep.
        let apps = vec!["shop".to_string()];
        let secrets = vec!["shop".to_string(), "laundry-assistant".to_string()];
        assert_eq!(
            namespaces_to_ensure_all(&apps, &secrets),
            vec!["laundry-assistant", "shop"]
        );
    }

    #[test]
    fn an_older_backup_without_secret_namespaces_ensures_what_it_always_did() {
        let apps = vec!["shop".to_string(), "blog".to_string()];
        assert_eq!(namespaces_to_ensure_all(&apps, &[]), vec!["blog", "shop"]);
    }

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
        // `contains_key` and not `is_null()`: indexing an ABSENT key also
        // yields null, so the cheap spelling of this assertion passes on a
        // patch that clears nothing at all.
        assert!(
            body["metadata"]["annotations"]
                .as_object()
                .is_some_and(|a| a.get(PRE_RESTORE_REPLICAS_ANNOTATION) == Some(&Value::Null)),
            "the resume also retires the pre-restore record it is acting on"
        );
        let argo_body: Value = serde_json::from_str(&patches[1].body).unwrap();
        assert_eq!(
            argo_body["spec"]["syncPolicy"]["automated"]["selfHeal"],
            true
        );
    }

    // =======================================================================
    // H5: what a restore says when it stops partway
    // =======================================================================

    /// A run that had already suspended workloads must say so. Both halves of
    /// the damage are named — the apps at zero and the Argo Applications whose
    /// auto-sync is off — because an operator looking at a dead cluster cannot
    /// tell an expected mid-restore state from a broken one.
    #[test]
    fn interrupted_restore_names_every_app_and_argo_app_it_left_down() {
        let apps = vec![
            (("demo".to_string(), "web".to_string()), 3),
            (("shop".to_string(), "api".to_string()), 1),
        ];
        let argo = vec![("argocd".to_string(), "web-prod".to_string())];
        let out = interrupted_restore_lines(true, &apps, &argo).join("\n");

        assert!(
            out.contains("demo/web (3 replica(s) when it comes back up)"),
            "{out}"
        );
        assert!(
            out.contains("shop/api (1 replica(s) when it comes back up)"),
            "{out}"
        );
        assert!(out.contains("argocd/web-prod"), "{out}");
        // And the way out, both of them: finish the restore, or patch by hand.
        assert!(out.contains("Re-running the SAME command"), "{out}");
        assert!(
            out.contains("kubectl -n demo patch applications.apprafter.io web --type=merge"),
            "{out}"
        );
        assert!(
            out.contains("kubectl -n argocd patch applications.argoproj.io web-prod"),
            "{out}"
        );
    }

    /// The honest limit, stated rather than implied: a re-run is not a resume.
    /// An operator who thinks the finished steps will be skipped budgets the
    /// wrong amount of time and reads the repeated work as a bug.
    #[test]
    fn interrupted_restore_says_there_is_no_resume() {
        let apps = vec![(("demo".to_string(), "web".to_string()), 3)];
        for data_only in [true, false] {
            let out = interrupted_restore_lines(data_only, &apps, &[]).join("\n");
            assert!(out.contains("There is no resume"), "{out}");
            assert!(out.contains("--continue"), "{out}");
            assert!(out.contains("EVERY step from the first"), "{out}");
        }
    }

    /// The hint applies to BOTH modes, and says the right thing in each about
    /// why a re-run recovers the real counts: `--data-only` reads them off the
    /// annotation it stamped, a full restore off the backup artifact.
    #[test]
    fn interrupted_restore_explains_the_right_recovery_per_mode() {
        let apps = vec![(("demo".to_string(), "web".to_string()), 3)];

        let data_only = interrupted_restore_lines(true, &apps, &[]).join("\n");
        assert!(
            data_only.contains("scaled to 0 replicas for the load"),
            "{data_only}"
        );
        assert!(
            data_only.contains(PRE_RESTORE_REPLICAS_ANNOTATION),
            "the data-only remedy IS the annotation: {data_only}"
        );

        let full = interrupted_restore_lines(false, &apps, &[]).join("\n");
        assert!(
            full.contains("applied from the backup at 0 replicas"),
            "{full}"
        );
        assert!(
            full.contains("come from the backup"),
            "a full restore recovers from the artifact, not from an annotation: {full}"
        );
        assert!(
            !full.contains("recorded on the applications themselves"),
            "the gated path never annotates — promising it would be a lie: {full}"
        );
    }

    /// DOES NOT FIRE: a run that failed before it touched anything (a bad
    /// passphrase, an unreachable repository) has nothing to confess. Printing
    /// a scary "the cluster is left mid-restore" block there would send an
    /// operator hunting for damage that does not exist.
    #[test]
    fn interrupted_restore_says_nothing_when_nothing_was_suspended() {
        assert!(interrupted_restore_lines(true, &[], &[]).is_empty());
        assert!(interrupted_restore_lines(false, &[], &[]).is_empty());
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

    /// The write path of the suspend, asserted where the unit tests of the
    /// bodies cannot reach: the app's patch is the one that CARRIES the record
    /// (a plain scale-to-zero here would leave every body test green and the
    /// defect alive), and auto-sync is switched off before it, or Argo self-heal
    /// undoes the scale while the data loads.
    #[test]
    fn suspend_patches_disable_autosync_before_the_recorded_scale_to_zero() {
        let argo = vec![("argocd".to_string(), "web-prod".to_string())];
        let patches = suspend_patches("demo", "web", &argo, 3);

        assert_eq!(
            patches.iter().map(|p| p.resource).collect::<Vec<&str>>(),
            vec!["applications.argoproj.io", "applications.apprafter.io"],
            "auto-sync off FIRST, then the scale"
        );
        let off: Value = serde_json::from_str(&patches[0].body).unwrap();
        assert!(off["spec"]["syncPolicy"]["automated"].is_null());

        assert_eq!(patches[1].namespace, "demo");
        assert_eq!(patches[1].name, "web");
        let app: Value = serde_json::from_str(&patches[1].body).unwrap();
        assert_eq!(app["spec"]["base"]["replicas"], 0);
        assert_eq!(
            app["metadata"]["annotations"][PRE_RESTORE_REPLICAS_ANNOTATION], "3",
            "the scale-to-zero must carry the count to come back to"
        );
    }

    /// An app with no Argo Application of its own is still suspended — the
    /// record and the scale do not depend on GitOps being involved.
    #[test]
    fn suspend_patches_still_suspend_an_app_with_no_argo_application() {
        let patches = suspend_patches("demo", "web", &[], 2);
        assert_eq!(patches.len(), 1);
        assert_eq!(patches[0].resource, "applications.apprafter.io");
    }

    /// The suspend write carries BOTH halves: the app goes to zero and the
    /// count to come back to is stamped on it, in one patch. Two patches would
    /// leave a window in which the app is down with no record of what it was —
    /// exactly the state a re-run cannot recover from.
    #[test]
    fn suspend_patch_body_records_the_count_in_the_same_patch_that_zeroes_it() {
        let body: Value = serde_json::from_str(&suspend_patch_body(3)).unwrap();
        assert_eq!(body["spec"]["base"]["replicas"], 0);
        assert_eq!(body["spec"]["base"].as_object().unwrap().len(), 1);
        assert_eq!(
            body["metadata"]["annotations"][PRE_RESTORE_REPLICAS_ANNOTATION], "3",
            "annotation values are strings — a bare number would be rejected"
        );
    }

    /// The resume write is the other half of the record's life-cycle: the count
    /// back, and the annotation DELETED (a merge-patch `null`). A leftover
    /// annotation would win over the live count on the next restore and pin the
    /// app to a number that stopped being true when this restore finished.
    #[test]
    fn resume_patch_body_restores_the_count_and_clears_the_record() {
        let body: Value = serde_json::from_str(&resume_patch_body(3)).unwrap();
        assert_eq!(body["spec"]["base"]["replicas"], 3);
        assert_eq!(body["spec"]["base"].as_object().unwrap().len(), 1);
        assert!(
            body["metadata"]["annotations"][PRE_RESTORE_REPLICAS_ANNOTATION].is_null(),
            "null is how a JSON merge-patch removes a key"
        );
        assert!(
            body["metadata"]["annotations"]
                .as_object()
                .unwrap()
                .contains_key(PRE_RESTORE_REPLICAS_ANNOTATION),
            "the key must be PRESENT and null — an absent key removes nothing"
        );
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
            counts(apps_to_suspend(&apps, "demo", &[])),
            vec![("web".to_string(), 2), ("api".to_string(), 1)],
            "an unnamed app is unpatchable and a repeated one keeps its FIRST count"
        );

        let already = vec![(("demo".to_string(), "web".to_string()), 2)];
        assert_eq!(
            counts(apps_to_suspend(&apps, "demo", &already)),
            vec![("api".to_string(), 1)],
            "an app recorded on an earlier pass must not be re-read at replicas 0"
        );
        assert_eq!(
            counts(apps_to_suspend(&apps, "other-ns", &already)),
            vec![("web".to_string(), 2), ("api".to_string(), 1)],
            "the record is per (namespace, name) — a same-named app elsewhere still counts"
        );
    }

    /// `(name, replicas)` of each decision — the shape the pre-H5 seam
    /// returned, so the cases that are only about the COUNT stay readable.
    fn counts(decisions: Vec<SuspendDecision>) -> Vec<(String, i64)> {
        decisions
            .into_iter()
            .map(|d| (d.name, d.replicas))
            .collect()
    }

    /// An app suspended by an earlier run, exactly as that run left it.
    fn interrupted_app(name: &str, live: i64, recorded: Value) -> Value {
        json!({"metadata":{"name":name,
                           "annotations":{PRE_RESTORE_REPLICAS_ANNOTATION: recorded}},
               "spec":{"base":{"replicas":live}}})
    }

    /// THE defect (H5), in the shape it actually happens in: run 1 read 3,
    /// scaled the app to 0 and died at `LoadData`; run 2 reads the live object
    /// and finds the 0 that run 1 wrote. Recording that 0 would "resume" the
    /// app to zero and report success over an application that never came back.
    ///
    /// The in-memory `already_recorded` guard cannot help — it is empty, this
    /// being a different process — so the annotation is the only thing standing
    /// between a second attempt and a permanently dead app.
    #[test]
    fn apps_to_suspend_prefers_an_earlier_runs_record_over_the_live_zero() {
        let apps = vec![interrupted_app("web", 0, json!("3"))];
        let d = apps_to_suspend(&apps, "demo", &[]);

        assert_eq!(
            d,
            vec![SuspendDecision {
                name: "web".to_string(),
                replicas: 3,
                source: ReplicaSource::Annotation,
            }],
            "the live 0 is the previous run's damage, not the app's replica count"
        );
    }

    /// The pair to the above: with no annotation the live value is what counts,
    /// so the rule above is not a resolver that ignores the cluster.
    #[test]
    fn apps_to_suspend_uses_the_live_count_when_no_run_recorded_one() {
        let apps = vec![json!({"metadata":{"name":"web"},"spec":{"base":{"replicas":4}}})];
        assert_eq!(
            apps_to_suspend(&apps, "demo", &[]),
            vec![SuspendDecision {
                name: "web".to_string(),
                replicas: 4,
                source: ReplicaSource::Live,
            }]
        );
    }

    /// A recorded `0` is honoured. It is not damage — it is an app an operator
    /// had deliberately scaled down, faithfully recorded by the run that
    /// suspended it, and resuming it to 1 would start a workload that was
    /// stopped on purpose. The annotation is the record; its value is not
    /// second-guessed.
    #[test]
    fn apps_to_suspend_honours_a_recorded_zero() {
        let apps = vec![interrupted_app("web", 0, json!("0"))];
        assert_eq!(
            apps_to_suspend(&apps, "demo", &[]),
            vec![SuspendDecision {
                name: "web".to_string(),
                replicas: 0,
                source: ReplicaSource::Annotation,
            }]
        );
    }

    /// An annotation that is not a count — hand-edited, truncated, negative,
    /// or written as a number rather than the string an annotation must be —
    /// falls back to the live value and is REPORTED, never parsed into a guess.
    #[test]
    fn apps_to_suspend_falls_back_to_live_for_an_unreadable_record() {
        for bad in [json!("three"), json!(""), json!("-1"), json!("2.5")] {
            let apps = vec![interrupted_app("web", 5, bad.clone())];
            assert_eq!(
                apps_to_suspend(&apps, "demo", &[]),
                vec![SuspendDecision {
                    name: "web".to_string(),
                    replicas: 5,
                    source: ReplicaSource::UnusableAnnotation(
                        bad.as_str().unwrap_or_default().to_string()
                    ),
                }],
                "bad annotation: {bad}"
            );
        }

        // A non-string value cannot be written through the API but can arrive
        // in a hand-crafted object; it is reported the same way, not parsed.
        let apps = vec![interrupted_app("web", 5, json!(3))];
        let d = apps_to_suspend(&apps, "demo", &[]);
        assert_eq!(d[0].replicas, 5);
        assert!(matches!(d[0].source, ReplicaSource::UnusableAnnotation(_)));
    }

    /// Whitespace around an otherwise fine count is not a reason to leave an
    /// app down.
    #[test]
    fn apps_to_suspend_tolerates_whitespace_in_the_record() {
        let apps = vec![interrupted_app("web", 0, json!(" 2 "))];
        assert_eq!(apps_to_suspend(&apps, "demo", &[])[0].replicas, 2);
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

    #[test]
    fn argo_apps_for_cr_finds_the_registration_when_its_name_differs_from_the_cr() {
        // The live defect: the selector compared a CR name against a label
        // whose value is the REGISTRATION name. app.rs:4756-4776 asserts that
        // shape is supported — Argo app "cms" rendering CR "landing-cms" —
        // and on a mismatch suspend emitted the scale-to-zero WITHOUT the
        // auto-sync-disable, so Argo self-healed the replicas and the
        // data-only load ran under live pods writing to the database.
        let argo = vec![json!({
            "metadata": {"name": "cms", "namespace": "argocd",
                         "labels": {"apprafter.io/application": "cms"}},
            "spec": {"destination": {"namespace": "web"}},
            "status": {"resources": [
                {"group": "apprafter.io", "kind": "Application",
                 "name": "landing-cms", "namespace": "web"}
            ]}
        })];
        assert_eq!(
            argo_apps_for_cr("landing-cms", "web", &argo),
            vec![("argocd".to_string(), "cms".to_string())],
            "the registration must be found by what it DEPLOYS, not by a label that names it"
        );
    }

    #[test]
    fn argo_apps_for_cr_ignores_a_registration_that_deploys_a_different_workload() {
        let argo = vec![json!({
            "metadata": {"name": "other", "namespace": "argocd"},
            "status": {"resources": [
                {"group": "apprafter.io", "kind": "Application",
                 "name": "someone-else", "namespace": "web"}
            ]}
        })];
        assert!(argo_apps_for_cr("landing-cms", "web", &argo).is_empty());
    }

    /// A ref whose namespace is UNKNOWN (`CrRef::namespace == None`: the
    /// `status.resources[]` entry names none and the registration has no
    /// `spec.destination.namespace`) must NOT match. Unknown is not a
    /// wildcard — treating it as one would disable auto-sync on a
    /// registration that may deploy a same-named workload somewhere else
    /// entirely, quiescing a stranger's app while leaving ours running
    /// under the load.
    #[test]
    fn argo_apps_for_cr_refuses_a_ref_whose_namespace_is_unknown() {
        let argo = vec![json!({
            "metadata": {"name": "placeless", "namespace": "argocd"},
            "status": {"resources": [
                {"group": "apprafter.io", "kind": "Application", "name": "api"}
            ]}
        })];
        assert!(argo_apps_for_cr("api", "prod", &argo).is_empty());
    }

    #[test]
    fn argo_apps_for_cr_distinguishes_the_same_name_in_two_namespaces() {
        let argo = vec![
            json!({"metadata": {"name": "prod-reg", "namespace": "argocd"},
                   "status": {"resources": [{"group": "apprafter.io", "kind": "Application",
                                             "name": "api", "namespace": "prod"}]}}),
            json!({"metadata": {"name": "stg-reg", "namespace": "argocd"},
                   "status": {"resources": [{"group": "apprafter.io", "kind": "Application",
                                             "name": "api", "namespace": "staging"}]}}),
        ];
        assert_eq!(
            argo_apps_for_cr("api", "prod", &argo),
            vec![("argocd".to_string(), "prod-reg".to_string())]
        );
    }

    /// A bundle's N workloads share ONE registration, so the suspend loop
    /// offers that registration to the accumulator once per workload. It must
    /// land once: `resume_patches` would otherwise emit N identical patches,
    /// and the interruption hint would count wrong and print the same manual
    /// recovery command N times at an operator who is already mid-incident.
    #[test]
    fn a_bundles_shared_registration_is_recorded_once() {
        let argo = vec![json!({
            "metadata": {"name": "shop", "namespace": "argocd"},
            "status": {"resources": [
                {"group": "apprafter.io", "kind": "Application",
                 "name": "shop-web", "namespace": "shop"},
                {"group": "apprafter.io", "kind": "Application",
                 "name": "shop-worker", "namespace": "shop"}
            ]}
        })];

        // Exactly what `suspend_running_workloads` does: one join per
        // workload of the namespace, each recorded as it is suspended.
        let mut suspended_argo: Vec<(String, String)> = Vec::new();
        for workload in ["shop-web", "shop-worker"] {
            record_suspended_argo(
                &mut suspended_argo,
                argo_apps_for_cr(workload, "shop", &argo),
            );
        }
        assert_eq!(
            suspended_argo,
            vec![("argocd".to_string(), "shop".to_string())],
            "one registration, recorded once however many of its workloads are suspended"
        );

        let recovery = interrupted_restore_lines(
            true,
            &[
                (("shop".to_string(), "shop-web".to_string()), 2),
                (("shop".to_string(), "shop-worker".to_string()), 1),
            ],
            &suspended_argo,
        );
        assert_eq!(
            recovery
                .iter()
                .filter(|l| l.contains("patch applications.argoproj.io"))
                .count(),
            1,
            "one registration to re-enable ⇒ exactly one manual recovery command"
        );

        assert_eq!(
            resume_patches(&[], &suspended_argo).len(),
            1,
            "the resume reads the same list and must not re-enable the same app twice"
        );
    }

    #[test]
    fn argo_apps_for_cr_returns_every_registration_that_claims_the_workload() {
        // `app add`'s duplicate guard keys only on the Argo object name, so
        // two registrations CAN claim one CR. Suspending only one of them
        // leaves the other's self-heal live — the whole defect, again.
        let argo = vec![
            json!({"metadata": {"name": "aaa", "namespace": "argocd"},
                   "status": {"resources": [{"group": "apprafter.io", "kind": "Application",
                                             "name": "shop", "namespace": "shop"}]}}),
            json!({"metadata": {"name": "zzz", "namespace": "argocd"},
                   "status": {"resources": [{"group": "apprafter.io", "kind": "Application",
                                             "name": "shop", "namespace": "shop"}]}}),
        ];
        assert_eq!(argo_apps_for_cr("shop", "shop", &argo).len(), 2);
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

    /// Every payload directory the extractor writes has to be recognised, and
    /// `volumes/` is the one that was not: the matcher listed `disk`, which no
    /// version of the extractor has ever written (`extract_volume` has written
    /// `volumes/` since the engine was split behind `KubeExec`). So a
    /// sequential run backing up a `needs.disk` claim produced a snapshot the
    /// restore skipped with a note — and reported success. Same class as D26,
    /// which introduced this function, one directory name later.
    #[test]
    fn every_payload_directory_the_extractor_writes_is_recognised() {
        for (kind, dir) in [
            (backup_core::DataKind::Pg, "pg/demo/db.dump"),
            (
                backup_core::DataKind::Volume,
                "volumes/demo/uploads/data.tar",
            ),
            (backup_core::DataKind::Redis, "redis/demo/cache/dump.tar"),
            (
                backup_core::DataKind::JetStream,
                "jetstream/demo/events/orders.tar",
            ),
        ] {
            let root = tempfile::tempdir().unwrap();
            write_at(root.path(), &format!("claim-0/data/{dir}"), "PAYLOAD");
            assert_eq!(
                find_claim_data_dir(root.path()),
                Some(root.path().join("claim-0/data")),
                "{kind:?} writes {dir} and the matcher did not see it"
            );
        }
    }

    /// A sequential run stages ONE claim per snapshot, so a snapshot holding
    /// only a jetstream payload has to be recognised as a payload directory —
    /// otherwise its data is restored by nothing and the restore reports
    /// success.
    #[test]
    fn a_per_claim_snapshot_holding_only_streams_is_a_payload_directory() {
        let root = tempfile::tempdir().unwrap();
        write_at(
            root.path(),
            "claim-1/data/jetstream/atm/worker-js/orders.tar",
            "TAR",
        );
        assert_eq!(
            find_claim_data_dir(root.path()),
            Some(root.path().join("claim-1/data"))
        );
    }

    /// JetStream artifacts are keyed by STREAM, not by a fixed payload name:
    /// `jetstream/<ns>/<claim>/<stream>.tar`. The volume/redis discovery looks
    /// for one known file name and cannot read this tree at all.
    #[test]
    fn stream_artifacts_are_discovered_by_file_stem() {
        let dd = tempfile::tempdir().unwrap();
        write_at(dd.path(), "jetstream/atm/worker-js/orders.tar", "TAR-A");
        write_at(dd.path(), "jetstream/atm/worker-js/orders_dlq.tar", "TAR-B");
        write_at(dd.path(), "jetstream/atm/api-js/events.tar", "TAR-C");
        // Not an artifact: the dump writes `<stream>.tar` and nothing else.
        write_at(dd.path(), "jetstream/atm/worker-js/notes.txt", "x");

        let found = discover_stream_artifacts(dd.path());
        let seen: Vec<(String, String, String)> = found
            .iter()
            .map(|a| (a.namespace.clone(), a.claim.clone(), a.stream.clone()))
            .collect();
        assert_eq!(
            seen,
            vec![
                (
                    "atm".to_string(),
                    "api-js".to_string(),
                    "events".to_string()
                ),
                (
                    "atm".to_string(),
                    "worker-js".to_string(),
                    "orders".to_string()
                ),
                (
                    "atm".to_string(),
                    "worker-js".to_string(),
                    "orders_dlq".to_string()
                ),
            ],
            "stable order, stream read off the file stem, non-tar ignored"
        );
        assert!(discover_stream_artifacts(dd.path().join("nope").as_path()).is_empty());
    }

    fn nats_server() -> NatsServer {
        NatsServer {
            namespace: "nats".into(),
            url: "nats://nats.nats.svc:4222".into(),
            user: "mgr_atm".into(),
            password: "pw".into(),
        }
    }

    fn stream_artifacts(names: &[&str]) -> (tempfile::TempDir, Vec<StreamArtifact>) {
        let dir = tempfile::tempdir().unwrap();
        let artifacts = names
            .iter()
            .map(|stream| {
                let path = dir.path().join(format!("{stream}.tar"));
                std::fs::write(&path, b"tar").unwrap();
                StreamArtifact {
                    namespace: "atm".into(),
                    claim: "worker-js".into(),
                    stream: stream.to_string(),
                    path,
                }
            })
            .collect();
        (dir, artifacts)
    }

    /// A helper pod that never becomes Ready is deleted too. It used to be
    /// left behind — deleted by hand only after a failed stream and at the
    /// end — and a leftover `rs-js-<claim>` failed every later jetstream
    /// restore of the claim until someone deleted it.
    #[test]
    fn a_jetstream_helper_that_never_becomes_ready_is_deleted() {
        let k = FakeKube::failing_apply();
        let (_dir, streams) = stream_artifacts(&["orders"]);
        let r = restore_claim_streams(
            &k,
            "atm",
            "worker-js",
            &nats_server(),
            &streams,
            backup_core::helper_pod::DEFAULT_RUN_DEADLINE,
        );
        assert!(r.is_err());
        assert!(
            k.execs.borrow().is_empty(),
            "no replay into a pod never Ready"
        );
        assert_eq!(
            *k.deleted.borrow(),
            vec![("rs-js-worker-js".to_string(), "nats".to_string())]
        );
    }

    /// Every stream is replayed through the one pod beside the server, and the
    /// pod is deleted exactly once — on success and when a stream fails.
    #[test]
    fn a_jetstream_helper_replays_every_stream_and_is_deleted_once() {
        let k = FakeKube::default();
        let (_dir, streams) = stream_artifacts(&["orders", "orders_dlq"]);
        restore_claim_streams(
            &k,
            "atm",
            "worker-js",
            &nats_server(),
            &streams,
            backup_core::helper_pod::DEFAULT_RUN_DEADLINE,
        )
        .unwrap();
        let applied = k.applied.borrow();
        assert_eq!(applied.len(), 1);
        assert_eq!(applied[0]["metadata"]["name"], "rs-js-worker-js");
        assert_eq!(applied[0]["metadata"]["namespace"], "nats");
        let execs = k.execs.borrow();
        assert_eq!(execs.len(), 2);
        assert!(execs
            .iter()
            .all(|e| e.0 == "rs-js-worker-js" && e.1 == "nats"));
        assert_eq!(execs[1].3, streams[1].path);
        assert_eq!(
            *k.deleted.borrow(),
            vec![("rs-js-worker-js".to_string(), "nats".to_string())]
        );

        let failing = FakeKube::failing_exec();
        assert!(restore_claim_streams(
            &failing,
            "atm",
            "worker-js",
            &nats_server(),
            &streams,
            backup_core::helper_pod::DEFAULT_RUN_DEADLINE,
        )
        .is_err());
        assert_eq!(
            failing.execs.borrow().len(),
            1,
            "stops at the failed stream"
        );
        assert_eq!(
            *failing.deleted.borrow(),
            vec![("rs-js-worker-js".to_string(), "nats".to_string())]
        );
    }

    /// Measured on a real server (nats 2.14.3 / CLI v0.2.3): `nats stream
    /// restore` refuses a stream that exists — `Stream "X" already exist`,
    /// exit 1 — and NACK recreates a DECLARED stream empty as soon as the
    /// claim provisions. So the replay deletes first, and treats losing that
    /// race as a retry rather than a failure.
    #[test]
    fn the_restore_script_deletes_before_it_replays_and_retries_the_race() {
        let script = jetstream_restore_script("orders");
        let rm = script.find("stream rm").expect("deletes the stream");
        let restore = script.find("stream restore").expect("replays the snapshot");
        assert!(rm < restore, "delete must precede replay: {script}");
        assert!(
            script.contains("already exist"),
            "retries the race: {script}"
        );
        assert!(
            script.contains("tar x -C"),
            "reads the tar on stdin: {script}"
        );
        assert!(
            script.contains("'orders'"),
            "the stream is quoted: {script}"
        );
        assert!(script.starts_with("set -e"), "{script}");
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
    /// the user gives up. `PGOPTIONS` must too: without it a `pg_restore`
    /// stopped while its `--clean` waits for `ACCESS EXCLUSIVE` leaves that
    /// request queued on the server, and every later reader of the table
    /// queues behind it (measured on PostgreSQL 18.6).
    #[test]
    fn pg_helper_pod_spec_injects_the_password_into_the_container_env() {
        let spec = pg_helper_pod_spec(
            "ld-pg-db",
            "demo",
            "postgres:18",
            "s3cret",
            backup_core::helper_pod::DEFAULT_RUN_DEADLINE,
        );
        assert_eq!(spec["metadata"]["name"], "ld-pg-db");
        assert_eq!(spec["metadata"]["namespace"], "demo");
        assert_eq!(spec["spec"]["containers"][0]["image"], "postgres:18");
        // Alive for the run deadline it was given, not a fixed hour: the
        // `sleep` ending kills a `pg_restore` still running in the pod.
        assert_eq!(
            spec["spec"]["containers"][0]["command"],
            serde_json::json!(["sleep", "21600"])
        );
        assert_eq!(
            spec["spec"]["containers"][0]["env"],
            serde_json::json!([
                { "name": "PGPASSWORD", "value": "s3cret" },
                { "name": "PGOPTIONS", "value": "-c client_connection_check_interval=10s" }
            ])
        );
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
            backup_core::helper_pod::DEFAULT_RUN_DEADLINE,
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
        // The pod the load really ran in carries the connection check: a
        // `pg_restore` stopped while its `--clean` waits on a lock must not
        // leave that request queued on the server.
        assert_eq!(
            k.applied.borrow()[0]["spec"]["containers"][0]["env"][1],
            serde_json::json!({
                "name": "PGOPTIONS",
                "value": "-c client_connection_check_interval=10s"
            })
        );
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
            backup_core::helper_pod::DEFAULT_RUN_DEADLINE,
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
            backup_core::helper_pod::DEFAULT_RUN_DEADLINE,
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

        load_one_volume(
            "demo",
            "uploads",
            "pvc-uploads",
            tar.path(),
            &k,
            backup_core::helper_pod::DEFAULT_RUN_DEADLINE,
        )
        .unwrap();

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
