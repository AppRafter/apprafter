// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! In-cluster scheduled-backup runner (2.6d-4). See
//! docs/superpowers/specs/2026-07-16-2-6d-4-s3-push-design.md.
//!
//! `main` assembles the pieces the prior chunks built into one run:
//! [`RunnerConfig::from_env`] → an in-cluster kube-rs [`KubeRsExec`] +
//! [`SubprocessRestic`] → [`backup_core::engine::run_backup`] (+ optional
//! [`backup_core::prune::run_prune`]) → a status ConfigMap + optional failure
//! webhook.
//!
//! # Runtime shape (why NOT `#[tokio::main]`)
//!
//! The portable backup engine is SYNCHRONOUS: its `KubeExec` trait methods are
//! `fn`, and [`KubeRsExec`] drives each one's async kube-rs body by
//! `Handle::block_on`-ing onto a Tokio runtime. If `main` were `#[tokio::main]`,
//! the engine's sync methods would run ON a runtime worker thread and their
//! internal `block_on` would be a nested `block_on` on the same worker — which
//! Tokio panics on. So we build a manual [`Runtime`](tokio::runtime::Runtime),
//! run the sync engine on the MAIN thread, and let `KubeRsExec` `block_on` onto
//! the runtime's [`Handle`](tokio::runtime::Handle). The only things `main`
//! itself `block_on`s directly (kube client construction, the status-CM write)
//! run on the main thread too, never nested inside the engine.
//!
//! # Error contract (NO panic — every path becomes an exit code)
//!
//! * config / runtime / kube-client construction failure → print + exit **2**
//!   (a precondition error, not a backup that ran and failed).
//! * any error from the backup itself → a [`RunOutcome::Failure`] → status CM
//!   records `lastFailure`/`lastError`, exit **1**.
//! * a successful backup → [`RunOutcome::Success`] → status CM records
//!   `lastSuccess`, exit **0**.
//! * the status-CM write and the failure webhook are BEST-EFFORT: a failure in
//!   either is logged but NEVER changes the run's exit code.

use apprafter_backup::config::RunnerConfig;
use apprafter_backup::kube_rs_exec::KubeRsExec;
use apprafter_backup::orchestrate::{resolve_namespaces, RunOutcome};
use apprafter_backup::status::write_status;
use apprafter_backup::webhook::post_failure;

use backup_core::engine::{run_backup, BackupOpts};
use backup_core::prune::run_prune;
use backup_core::restic::restic_unlock_argv;
use backup_core::{KubeExec, ResticRunner, StagingMode, SubprocessRestic};

use cli_core::{CliError, Result};

fn main() {
    let code = run();
    std::process::exit(code);
}

/// The whole run, returning the process exit code. NEVER panics: every error is
/// funnelled into an exit code (see the module-level error contract).
fn run() -> i32 {
    // 1. Config. A missing/invalid env is a PRECONDITION error — the backup
    //    never even started, so this is exit 2, not a Failure outcome.
    let cfg = match RunnerConfig::from_env() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("config error: {e}");
            return 2;
        }
    };

    // 2. Tokio runtime + in-cluster kube client. Both are preconditions (exit 2).
    //    The runtime is manual (NOT #[tokio::main]) — see the module docs.
    let rt = match tokio::runtime::Runtime::new() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("runtime: {e}");
            return 2;
        }
    };
    let client = match rt.block_on(kube::Client::try_default()) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("kube client: {e}");
            return 2;
        }
    };

    let k = KubeRsExec::new(client.clone(), rt.handle().clone());
    let r = SubprocessRestic;

    // The status ConfigMap records which staging format this run used.
    let format = match cfg.staging_mode {
        StagingMode::Sequential => "sequential",
        StagingMode::Monolithic => "monolithic",
    };

    // 3. The backup itself, wrapped so ANY error becomes a Failure outcome
    //    (never a panic, never a bare exit) — the engine ran, so the outcome is
    //    recorded in the status CM and the exit code is 1.
    let outcome = match do_backup(&k, &r, &cfg) {
        Ok(snapshot) => RunOutcome::Success { snapshot },
        Err(e) => RunOutcome::Failure {
            error: format!("{e}"),
        },
    };

    // 4. Status ConfigMap — BEST-EFFORT. A write failure is logged but does NOT
    //    change the run's exit code (the backup's success/failure is what the
    //    exit code reflects, not our ability to record it).
    let now = chrono::Utc::now().to_rfc3339();
    if let Err(e) = rt.block_on(write_status(&client, &outcome, format, &now)) {
        eprintln!("warning: status ConfigMap write failed (non-fatal): {e}");
    }

    // 5. Failure webhook — BEST-EFFORT (fire-and-forget; post_failure never
    //    errors or panics). Only posts on a Failure outcome with a configured URL.
    if let (RunOutcome::Failure { error }, Some(url)) = (&outcome, &cfg.failure_webhook) {
        post_failure(url, &cfg.cluster_id, "backup", error);
    }

    outcome.exit_code()
}

/// Run one backup end-to-end, returning the restic snapshot id (or `None` when
/// restic emitted no summary line). Every fallible step propagates its error up
/// to [`run`], which turns it into a [`RunOutcome::Failure`].
fn do_backup(k: &dyn KubeExec, r: &dyn ResticRunner, cfg: &RunnerConfig) -> Result<Option<String>> {
    // a. Unlock a stale lock left by a previous crashed run. NON-FATAL: a fresh
    //    repo (or one restic can't reach yet) has no lock — log and continue so
    //    a spurious unlock error can never fail an otherwise-fine backup.
    if let Err(e) = r.run(&restic_unlock_argv(&cfg.repo), &cfg.passphrase) {
        eprintln!("unlock (non-fatal): {e}");
    }

    // b. Resolve the app-namespace set the SAME way the CLI does: list every
    //    AppRafter Application cluster-wide and take the (deduped, sorted) set of
    //    their namespaces — NOT `kubectl get ns` (the backup scope is the set of
    //    namespaces that host an Application, spec §backup-scope). A missing CRD
    //    (`Ok(None)`) or zero Applications means there is nothing to back up.
    let apps = k
        .get_json(&["get", "applications.apprafter.io", "-A", "-o", "json"])?
        .unwrap_or_else(|| serde_json::json!({ "items": [] }));
    let namespaces = resolve_namespaces(&apps);
    if namespaces.is_empty() {
        return Err(CliError::Other(
            "no AppRafter Applications found — nothing to back up. (Scope derives from \
             `applications.apprafter.io` across all namespaces.)"
                .into(),
        ));
    }

    // c. Platform version stamped into the manifest — read straight from
    //    `PlatformStack/default.status.currentVersion` (the engine helper the
    //    CLI also uses), falling back to `"unknown"` internally when absent.
    let platform_version = backup_core::engine::read_platform_version(k)?;

    // d. pg_dump helper image: major-matched to the live CNPG server image when a
    //    CNPG Cluster exists (mirrors the CLI's
    //    `pg_helper_image(first_cnpg_image(...))`), else the pinned default. This
    //    is the SAME resolution the CLI backup path uses, so an in-cluster run and
    //    a CLI local-pull pick an identical pg_dump image.
    let pg_image = backup_core::images::pg_helper_image(
        backup_core::engine::first_cnpg_image(k, &namespaces).as_deref(),
    );

    // e. Staging tempdir — the engine writes its `data/` (and per-claim /
    //    commit) subtrees under this root. KEEP the guard alive for the WHOLE
    //    backup: dropping it removes the directory (and thus every staged dump)
    //    before restic has snapshotted it.
    let staging = tempfile::Builder::new()
        .prefix("apprafter-backup-")
        .tempdir()
        .map_err(|e| CliError::Other(format!("create staging dir: {e}")))?;

    let opts = BackupOpts {
        repo: cfg.repo.clone(),
        passphrase: cfg.passphrase.clone(),
        cluster_id: cfg.cluster_id.clone(),
        created_at: chrono::Utc::now().to_rfc3339(),
        platform_version,
        namespaces,
        // The in-cluster runner always backs up the full app-namespace set
        // (there is no `--namespace`/`--select` subset flag), so the tag is
        // never namespace-decorated.
        is_subset: false,
        staging_root: staging.path().to_path_buf(),
        pg_image,
        staging_mode: cfg.staging_mode,
        // Fixed restic `--host` so `forget` retention policies group across runs
        // even though the pod name is ephemeral (spec §Retention M-r3-1a).
        backup_host: Some("apprafter-backup".into()),
    };

    // f. The backup.
    let snapshot = run_backup(k, r, &opts)?;

    // g. Retention prune — only when this runner is the enforcing (in-cluster)
    //    one. UNLIKE the best-effort status/webhook steps, a prune failure DOES
    //    fail the run: a repo whose retention isn't being enforced grows without
    //    bound, and that is a real backup-subsystem fault worth surfacing.
    if cfg.enforce_in_cluster {
        run_prune(r, &cfg.repo, &cfg.passphrase, &cfg.retention)?;
    }

    // Keep `staging` alive until here (all restic snapshots are committed).
    drop(staging);

    Ok(snapshot)
}
