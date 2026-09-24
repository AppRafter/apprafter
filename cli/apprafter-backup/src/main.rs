// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! In-cluster scheduled-backup runner (2.6d-4). See
//! docs/superpowers/specs/2026-07-16-2-6d-4-s3-push-design.md.
//!
//! `main` assembles the pieces the prior chunks built into one run:
//! [`RunnerConfig::from_env`] → an in-cluster kube-rs [`KubeRsExec`] +
//! [`ForwardingRestic`] → [`backup_core::engine::run_backup`] (+ optional
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
//! * SIGTERM — Kubernetes stopping the run at its Job deadline, or deleting
//!   its pod — or SIGINT → the run is recorded as a `Failure` by
//!   [`stop::stop_run`] instead of by the run itself, its helper pods deleted
//!   and the signal passed on to a restic it has running, exit **1**. Only one
//!   of the two ever records ([`OutcomeClaim`]).
//! * the staging volume holding more than its size limit → the run is
//!   stopped the same way, by the runner itself, and recorded as a `Failure`
//!   that names the limit ([`apprafter_backup::staging`]), exit **1**. The
//!   check run too.
//!
//! # `apprafter-backup check` — the weekly check Job
//!
//! With the argument `check` the runner does not back up: it runs `restic
//! check`, then — under `retention.enforce: check`, and only after a check
//! that passed — the prune, then reads the repository's figures
//! ([`apprafter_backup::check`]). Exit **1** when the check did not pass,
//! **0** otherwise, whatever the prune did: a prune the cluster's key may not
//! run, or one that fails, is recorded in the status ConfigMap and reported
//! by the operator's `BackupRetention` condition, not as a failed check. The
//! same stop handles its SIGTERM, and records the failure against the step
//! it was in. Any other argument is a precondition error (exit **2**).

use apprafter_backup::check::{run_check, CheckPlan, PhaseCell};
use apprafter_backup::config::{Enforce, RunnerConfig};
use apprafter_backup::kube_rs_exec::KubeRsExec;
use apprafter_backup::orchestrate::{resolve_namespaces, RunOutcome};
use apprafter_backup::restic_child::ForwardingRestic;
use apprafter_backup::staging;
use apprafter_backup::status::{prune_record, status_configmap, write_status_data, PruneRecord};
use apprafter_backup::stop::{self, OutcomeClaim, StopContext, StopKind, StopSignal};
use apprafter_backup::webhook::post_failure;

use backup_core::engine::{run_backup, BackupOpts};
use backup_core::prune::{run_prune, PruneOutcome};
use backup_core::restic::restic_unlock_argv;
use backup_core::{KubeExec, ResticRunner, StagingMode};

use cli_core::{CliError, Result};

/// Which Job this process is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    /// The nightly backup (no argument): the CronJob, and `apprafter backup
    /// run`, which copies its Job template.
    Backup,
    /// The weekly check Job (`check`).
    Check,
}

impl Mode {
    fn from_args(args: &[String]) -> Result<Self> {
        match args {
            [] => Ok(Mode::Backup),
            [one] if one == "check" => Ok(Mode::Check),
            other => Err(CliError::Other(format!(
                "unknown arguments {other:?}: the runner takes none (a backup) or `check` (the \
                 weekly check and the prune after it)"
            ))),
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = match Mode::from_args(&args) {
        Ok(mode) => run(mode),
        Err(e) => {
            eprintln!("{e}");
            2
        }
    };
    std::process::exit(code);
}

/// The whole run, returning the process exit code. NEVER panics: every error is
/// funnelled into an exit code (see the module-level error contract).
fn run(mode: Mode) -> i32 {
    // Before anything else, so a run stopped at its deadline reports how long
    // it had been running rather than how long its setup took.
    let started = std::time::Instant::now();

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
    //    Built through `tls::kube_client` (NOT `Client::try_default`) so the
    //    rustls crypto provider is installed before kube-rs builds its TLS
    //    config, and so the client carries the runner's read timeout and retry
    //    settings — see `apprafter_backup::tls`. Inference order is unchanged:
    //    `try_default` is exactly `Config::infer` + `Client::try_from`.
    let client = match rt.block_on(async {
        let config = kube::Config::infer()
            .await
            .map_err(kube::Error::InferConfig)?;
        apprafter_backup::tls::kube_client(config)
    }) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("kube client: {e}");
            return 2;
        }
    };

    let k = KubeRsExec::new(client.clone(), rt.handle().clone());
    // restic as a child the stop can pass its signal on to (`restic_child`).
    let r = ForwardingRestic::new("restic");

    // The status ConfigMap records which staging format this run used.
    let format = match cfg.staging_mode {
        StagingMode::Sequential => "sequential",
        StagingMode::Monolithic => "monolithic",
    };

    // 2b. SIGTERM: at the Job's deadline (or when its pod is deleted)
    //     Kubernetes sends it, then SIGKILL after the pod's grace period. As
    //     PID 1 with no handler the runner would ignore the first and die by
    //     the second with nothing recorded; see `stop`. SIGINT, from a run
    //     started by hand, is handled the same way. A handler that cannot be
    //     installed leaves the run exactly as it was before, so it is reported
    //     and the backup goes ahead.
    let claim = OutcomeClaim::default();
    let phase = PhaseCell::default();
    let stop_ctx = StopContext {
        client: client.clone(),
        live_helpers: k.live_helper_pods(),
        restic: r.live_children(),
        started,
        deadline: cfg.deadline,
        kind: match mode {
            Mode::Backup => StopKind::Backup { format },
            Mode::Check => StopKind::Check {
                phase: phase.clone(),
            },
        },
        cluster_id: cfg.cluster_id.clone(),
        failure_webhook: cfg.failure_webhook.clone(),
    };
    match rt.block_on(async {
        use tokio::signal::unix::{signal, SignalKind};
        Ok::<_, std::io::Error>((
            signal(SignalKind::terminate())?,
            signal(SignalKind::interrupt())?,
        ))
    }) {
        Ok((mut sigterm, mut sigint)) => {
            let ctx = stop_ctx.clone();
            let claim = claim.clone();
            rt.spawn(async move {
                let received = tokio::select! {
                    Some(()) = sigterm.recv() => StopSignal::Terminate,
                    Some(()) = sigint.recv() => StopSignal::Interrupt,
                    else => return,
                };
                if !claim.claim() {
                    // The run finished first and is recording its own outcome;
                    // it exits well inside the grace period.
                    eprintln!(
                        "{} received while the run was already ending",
                        received.name()
                    );
                    return;
                }
                let outcome = stop::stop_run(&ctx, received).await;
                std::process::exit(outcome.exit_code());
            });
        }
        Err(e) => eprintln!(
            "warning: cannot handle SIGTERM/SIGINT ({e}); a run stopped at its deadline will \
             not record lastFailure"
        ),
    }

    // 2c. The staging volume's size limit. The run stages under TMPDIR, which
    //     the chart sets to the staging volume, so that directory is the
    //     volume the limit applies to. Measured here every few seconds: a run
    //     that outgrows it is stopped and recorded with a message that names
    //     the limit, instead of being evicted late and recorded, if at all,
    //     as stopped by Kubernetes. See `staging`. The check Job mounts the
    //     same volume for restic's cache and temporary files, and is watched
    //     the same way.
    if let Some(limit) = cfg.staging_limit {
        let ctx = stop_ctx.clone();
        let claim = claim.clone();
        let root = std::env::temp_dir();
        let staging_mode = cfg.staging_mode;
        rt.spawn(async move {
            let used = staging::wait_for_overrun(root, limit, staging::POLL).await;
            if !claim.claim() {
                // The run is already ending, and records its own outcome.
                return;
            }
            let error = match mode {
                Mode::Backup => staging::overrun_message(used, limit, staging_mode),
                Mode::Check => staging::check_overrun_message(used, limit),
            };
            let outcome = stop::stop_run_with(&ctx, error, libc::SIGTERM).await;
            std::process::exit(outcome.exit_code());
        });
    }

    if mode == Mode::Check {
        return check(&k, &r, &cfg, &client, &rt, &phase, &claim);
    }

    // 3. The backup itself, wrapped so ANY error becomes a Failure outcome
    //    (never a panic, never a bare exit) — the engine ran, so the outcome is
    //    recorded in the status CM and the exit code is 1.
    let mut pruned: Option<PruneRecord> = None;
    let result = do_backup(&k, &r, &cfg, &mut pruned);
    if !claim.claim() {
        // Kubernetes stopped the run, or its staging passed the limit, and the
        // stop is recording it; an error here is the stop's own doing (it
        // deleted the helper pod this run was reading from, or signalled its
        // restic). Wait for the stop to exit the process.
        loop {
            std::thread::park();
        }
    }
    let outcome = match result {
        Ok(snapshot) => RunOutcome::Success { snapshot },
        Err(e) => {
            // Surface the failure reason on stderr so `kubectl logs <runner-pod>`
            // shows WHY the backup failed. The status ConfigMap (below) is
            // best-effort and may be unwritable (RBAC / apiserver), so stderr is
            // the reliable operator-facing signal — never swallow the error.
            eprintln!("backup failed: {e}");
            RunOutcome::Failure {
                error: format!("{e}"),
            }
        }
    };

    // 4. Status ConfigMap — BEST-EFFORT. A write failure is logged but does NOT
    //    change the run's exit code (the backup's success/failure is what the
    //    exit code reflects, not our ability to record it).
    //    Under `enforce: cluster` the run's prune is recorded with it, so
    //    that retention is reported whatever became of the backup.
    let now = chrono::Utc::now().to_rfc3339();
    let mut data = status_configmap(&outcome, format, &now)["data"].clone();
    if let (Some(p), Some(fields)) = (&pruned, data.as_object_mut()) {
        if let Some(prune) = prune_record(p, "backup", &now).as_object() {
            fields.extend(prune.clone());
        }
    }
    if let Err(e) = rt.block_on(write_status_data(&client, &data)) {
        eprintln!("warning: status ConfigMap write failed (non-fatal): {e}");
    }

    // 5. Failure webhook — BEST-EFFORT (fire-and-forget; post_failure never
    //    errors or panics). Only posts on a Failure outcome with a configured URL.
    if let (RunOutcome::Failure { error }, Some(url)) = (&outcome, &cfg.failure_webhook) {
        post_failure(url, &cfg.cluster_id, "backup", error);
    }

    outcome.exit_code()
}

/// The weekly check Job: check, prune after a check that passed, figures
/// ([`apprafter_backup::check`]). Each step is recorded in the status
/// ConfigMap as it ends — best-effort, like the backup's record — and a check
/// that did not pass, or a prune that failed, is posted to the failure
/// webhook.
fn check(
    k: &dyn KubeExec,
    r: &dyn ResticRunner,
    cfg: &RunnerConfig,
    client: &kube::Client,
    rt: &tokio::runtime::Runtime,
    phase: &PhaseCell,
    claim: &OutcomeClaim,
) -> i32 {
    let plan = CheckPlan::of(cfg);
    // Nothing more once a stop has claimed the outcome: it records the step
    // it stopped, with its reason (`OutcomeClaim::unless_claimed`).
    let mut record = claim.unless_claimed(|data: serde_json::Value| {
        if let Err(e) = rt.block_on(write_status_data(client, &data)) {
            eprintln!("warning: status ConfigMap write failed (non-fatal): {e}");
        }
    });
    let run = run_check(
        r,
        &plan,
        phase,
        // The same identity read the backup makes (E1): the prune forgets
        // this cluster's snapshots only.
        &mut || backup_core::engine::read_cluster_uid(k),
        &chrono::Utc::now,
        &mut record,
    );
    if !claim.claim() {
        // Stopped: the stop records the step it was in and exits.
        loop {
            std::thread::park();
        }
    }
    if let (Some((phase, error)), Some(url)) = (run.failure(), &cfg.failure_webhook) {
        post_failure(url, &cfg.cluster_id, phase, &error);
    }
    run.exit_code()
}

/// Run one backup end-to-end, returning the restic snapshot id (or `None` when
/// restic emitted no summary line). Every fallible step propagates its error up
/// to [`run`], which turns it into a [`RunOutcome::Failure`]. Under `enforce:
/// cluster`, what became of the prune is left in `pruned` for the record.
fn do_backup(
    k: &dyn KubeExec,
    r: &dyn ResticRunner,
    cfg: &RunnerConfig,
    pruned: &mut Option<PruneRecord>,
) -> Result<Option<String>> {
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

    // b2. THIS cluster's machine key: the `kube-system` namespace UID (E1).
    //     It leads every snapshot tag, and it is the only thing that tells
    //     this cluster's runs from a co-tenant's in a repository two clusters
    //     share. Read before anything is staged so an RBAC gap fails the run
    //     cheaply rather than after a pg_dump. A hard error on purpose: a
    //     snapshot with no identity would land as "legacy" and be attributed
    //     to whichever cluster prunes next.
    let cluster_uid = backup_core::engine::read_cluster_uid(k)?;

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
        cluster_uid: cluster_uid.clone(),
        created_at: chrono::Utc::now().to_rfc3339(),
        platform_version,
        namespaces,
        // The in-cluster runner always backs up the full app-namespace set
        // (there is no `--namespace`/`--select` subset flag), so the tag is
        // never namespace-decorated.
        is_subset: false,
        staging_root: staging.path().to_path_buf(),
        pg_image,
        // Each helper pod lives at least as long as this run may: the Job's
        // deadline, never less than six hours — the rule the CLI's helper
        // pods follow too, so that both build one spec for a name.
        helper_keep_alive: cfg.helper_keep_alive(),
        staging_mode: cfg.staging_mode,
        // Stable restic `--host` — the operator-chosen cluster NAME when there
        // is one, else the fixed `apprafter-backup`. Never the pod name, which
        // is ephemeral (spec §Retention M-r3-1a). This is the human label that
        // makes a listing legible; the identity a filter keys on is the UID in
        // the tag above, because the name is replayed by restore and a clone
        // inherits it.
        backup_host: Some(cfg.backup_host.clone()),
    };

    // f. The backup.
    let snapshot = run_backup(k, r, &opts)?;

    // g. Retention prune — only when this runner is the enforcing (in-cluster)
    //    one. UNLIKE the best-effort status/webhook steps, a prune failure DOES
    //    fail the run: a repo whose retention isn't being enforced grows without
    //    bound, and that is a real backup-subsystem fault worth surfacing.
    //
    //    Scoped to THIS cluster's snapshots (E3). The repository can be shared,
    //    and the prune runs immediately after this run's own backup — so our
    //    snapshot is always the newest in its day, and an unscoped planner
    //    would make the co-tenant lose every bucket, structurally.
    //
    //    A key that may not delete fails the run too, as it always has under
    //    `cluster`: this mode promises a prune after every backup. Nothing is
    //    deleted — `run_prune` stops at the first refused delete — and the
    //    record says `not-permitted`.
    if cfg.enforce == Enforce::Cluster {
        // This run's own snapshots are complete by now. A run with no
        // manifest that another backup may still be writing — `apprafter
        // backup create` into the same repository — is left alone.
        match run_prune(
            r,
            &cfg.repo,
            &cfg.passphrase,
            &cfg.retention,
            &cluster_uid,
            chrono::Utc::now(),
            cfg.prune_run_deadline(),
        ) {
            Ok(outcome @ PruneOutcome::NotPermitted { .. }) => {
                let error = format!(
                    "retention.enforce is cluster, and the prune after this backup was {}. \
                     The backup itself was taken ({}). Give the cluster a key that may delete, \
                     or set `apprafter backup set enforce check` (prune after the weekly check, \
                     as far as the key allows) or `operator` (prune from outside the cluster)",
                    outcome.describe(),
                    snapshot
                        .as_deref()
                        .unwrap_or("its snapshot id was not reported")
                );
                *pruned = Some(PruneRecord::Done(outcome));
                return Err(CliError::Other(error));
            }
            Ok(outcome) => *pruned = Some(PruneRecord::Done(outcome)),
            Err(e) => {
                *pruned = Some(PruneRecord::Failed(e.to_string()));
                return Err(e);
            }
        }
    }

    // Keep `staging` alive until here (all restic snapshots are committed).
    drop(staging);

    Ok(snapshot)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn no_argument_is_a_backup_and_check_is_the_check() {
        assert_eq!(Mode::from_args(&[]).unwrap(), Mode::Backup);
        assert_eq!(Mode::from_args(&args(&["check"])).unwrap(), Mode::Check);
    }

    /// An argument this runner does not know must not fall through to a
    /// backup: a Job that asked for something else would take one instead.
    #[test]
    fn any_other_argument_is_refused() {
        for bad in [&["prune"][..], &["check", "now"], &["--check"], &["Check"]] {
            let err = Mode::from_args(&args(bad)).unwrap_err();
            assert!(
                err.to_string().contains("unknown arguments"),
                "{bad:?}: {err}"
            );
        }
    }
}
