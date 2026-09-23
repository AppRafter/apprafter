// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! What the runner does when Kubernetes stops it.
//!
//! A backup Job carries `activeDeadlineSeconds`. When a run is still going at
//! that deadline, the Job controller deletes its pod, and the kubelet sends the
//! runner — PID 1 of the pod — SIGTERM, then SIGKILL once the pod's
//! `terminationGracePeriodSeconds` (90 s in the chart) have passed. A PID 1
//! without a SIGTERM handler ignores the signal, so before this module the
//! runner was SIGKILLed with nothing recorded: no `lastFailure`, no failure
//! webhook, and its helper pod left running.
//!
//! On SIGTERM the runner now, within that grace period:
//!
//! 1. deletes the helper pods it has applied and not yet deleted — which also
//!    ends the exec the run was blocked in;
//! 2. records the failure in the status ConfigMap (`lastFailure`, `lastError`);
//! 3. posts the failure webhook;
//! 4. exits 1.
//!
//! Each step is bounded (see the `*_BOUND` constants), and together they fit
//! in the grace period, so the runner always exits on its own before the
//! SIGKILL. The Job itself still fails with reason `DeadlineExceeded`: the
//! Job controller has decided that before it sends the signal.
//!
//! SIGTERM is the trigger rather than a timer of the runner's own because only
//! the Job controller knows when the deadline is. It counts from the Job's
//! start, across every retry of the pod, and a timer started by the runner
//! would run late on any pod but the first. The same path also records a run
//! stopped for another reason — its pod deleted or evicted.
//!
//! Exactly one of the run and the stop records the outcome: [`OutcomeClaim`].

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use k8s_openapi::api::core::v1::Pod;
use kube::api::{Api, DeleteParams};

use crate::orchestrate::RunOutcome;

/// The most the helper-pod deletes may take, all of them together.
pub const HELPER_DELETE_BOUND: Duration = Duration::from_secs(10);
/// The most the status ConfigMap write may take.
pub const STATUS_WRITE_BOUND: Duration = Duration::from_secs(20);
/// The most the failure webhook may take: its own 30 s bound, plus room for
/// the name resolution that bound cannot interrupt (`webhook::post_failure`).
pub const WEBHOOK_BOUND: Duration = Duration::from_secs(45);

/// The pod's `terminationGracePeriodSeconds` in the chart's backup CronJob:
/// the time between SIGTERM and SIGKILL.
pub const TERMINATION_GRACE: Duration = Duration::from_secs(90);

/// How long before its deadline a stopped run still counts as stopped BY the
/// deadline. The runner starts after its Job does — scheduling, an image pull
/// — so it sees less of the deadline than the Job controller counts; five
/// minutes covers a slow start, and a pod deleted in that window is at most
/// misattributed to a deadline it was about to meet anyway.
pub const DEADLINE_START_ALLOWANCE: Duration = Duration::from_secs(300);

/// The helper pods a run has applied and not yet deleted, as
/// `(namespace, name)`. Shared between the exec layer, which adds and removes
/// them, and the stop, which deletes what is left.
#[derive(Clone, Debug, Default)]
pub struct LiveHelperPods(Arc<Mutex<BTreeSet<(String, String)>>>);

impl LiveHelperPods {
    /// Record a helper pod about to be applied.
    pub fn insert(&self, namespace: &str, name: &str) {
        self.lock()
            .insert((namespace.to_string(), name.to_string()));
    }

    /// Forget a helper pod that has been deleted.
    pub fn remove(&self, namespace: &str, name: &str) {
        self.lock()
            .remove(&(namespace.to_string(), name.to_string()));
    }

    /// The helper pods live right now.
    pub fn snapshot(&self) -> Vec<(String, String)> {
        self.lock().iter().cloned().collect()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, BTreeSet<(String, String)>> {
        // A panic while holding this lock cannot leave the set half-updated
        // (every operation is one call on it), so a poisoned lock is still a
        // good set.
        self.0.lock().unwrap_or_else(|p| p.into_inner())
    }
}

/// Which of the two writers records the run's outcome: the run, when it ends
/// on its own, or the stop, when Kubernetes ends it first. The first to claim
/// it records; the other records nothing.
#[derive(Clone, Debug, Default)]
pub struct OutcomeClaim(Arc<AtomicBool>);

impl OutcomeClaim {
    /// `true` for the first caller, `false` for every later one.
    pub fn claim(&self) -> bool {
        !self.0.swap(true, Ordering::SeqCst)
    }
}

/// `6h`, `90m`, `45s`, `5h59m50s`: a duration as `apprafter backup set
/// deadline` takes one, whole units and no zero parts.
pub fn human_duration(d: Duration) -> String {
    let total = d.as_secs();
    let (h, m, s) = (total / 3600, total % 3600 / 60, total % 60);
    let mut out = String::new();
    if h > 0 {
        out.push_str(&format!("{h}h"));
    }
    if m > 0 {
        out.push_str(&format!("{m}m"));
    }
    if s > 0 || out.is_empty() {
        out.push_str(&format!("{s}s"));
    }
    out
}

/// The `lastError` and webhook text for a run Kubernetes stopped, after
/// `ran_for` of a run whose deadline is `deadline` (`None`: not known).
pub fn stopped_message(ran_for: Duration, deadline: Option<Duration>) -> String {
    let ran = human_duration(ran_for);
    match deadline {
        Some(d) if ran_for + DEADLINE_START_ALLOWANCE >= d => format!(
            "run exceeded its deadline of {} and was stopped after {ran}: a backup Job is \
             stopped when its deadline passes (spec.backup.activeDeadlineSeconds). If the \
             backup needs longer, raise it with `apprafter backup set deadline`; if it should \
             not have taken this long, the run was stuck — the helper pods it was using have \
             been deleted",
            human_duration(d)
        ),
        Some(d) => format!(
            "run was stopped by Kubernetes (SIGTERM) after {ran}, before its deadline of {}: \
             its pod was deleted or evicted, or this was a retry of a failed attempt and the \
             deadline counts from the Job's first one",
            human_duration(d)
        ),
        None => format!(
            "run was stopped by Kubernetes (SIGTERM) after {ran}: its Job's deadline passed, \
             or its pod was deleted or evicted"
        ),
    }
}

/// Everything the stop needs, captured when the run starts.
pub struct StopContext {
    pub client: kube::Client,
    pub live_helpers: LiveHelperPods,
    pub started: Instant,
    pub deadline: Option<Duration>,
    /// The staging format the status ConfigMap records.
    pub format: &'static str,
    pub cluster_id: String,
    pub failure_webhook: Option<String>,
}

/// Record a run Kubernetes is stopping (see the module docs). Returns once
/// every step has finished or run out of time; the caller then exits.
pub async fn stop_run(ctx: &StopContext) -> RunOutcome {
    let error = stopped_message(ctx.started.elapsed(), ctx.deadline);
    eprintln!("backup stopped: {error}");

    // 1. Helper pods first: deleting one ends the exec the run is blocked in.
    //    Grace 0, because the work in them is abandoned. A dump killed now has
    //    its database session, and the table locks that session holds, ended
    //    within seconds, even while it waits on a lock: the helper connects with
    //    `client_connection_check_interval`
    //    (`backup_core::extract::PG_DUMP_PGOPTIONS`). Without that, the
    //    session would outlive the kill.
    let pods = ctx.live_helpers.snapshot();
    let deletes = pods.iter().map(|(ns, name)| {
        let api: Api<Pod> = Api::namespaced(ctx.client.clone(), ns);
        async move {
            let dp = DeleteParams {
                grace_period_seconds: Some(0),
                ..DeleteParams::default()
            };
            match api.delete(name, &dp).await {
                Ok(_) => eprintln!("stop: deleted helper pod {ns}/{name}"),
                Err(e) => eprintln!("stop: delete helper pod {ns}/{name}: {e}"),
            }
        }
    });
    if tokio::time::timeout(HELPER_DELETE_BOUND, futures::future::join_all(deletes))
        .await
        .is_err()
    {
        eprintln!(
            "stop: helper-pod deletes still running after {}s; going on",
            HELPER_DELETE_BOUND.as_secs()
        );
    }

    // 2. The status ConfigMap.
    let outcome = RunOutcome::Failure { error };
    let now = chrono::Utc::now().to_rfc3339();
    match tokio::time::timeout(
        STATUS_WRITE_BOUND,
        crate::status::write_status(&ctx.client, &outcome, ctx.format, &now),
    )
    .await
    {
        Ok(Ok(())) => {}
        Ok(Err(e)) => eprintln!("warning: status ConfigMap write failed (non-fatal): {e}"),
        Err(_) => eprintln!(
            "warning: status ConfigMap write still running after {}s (non-fatal)",
            STATUS_WRITE_BOUND.as_secs()
        ),
    }

    // 3. The failure webhook. ureq blocks, so it runs off the runtime's
    //    workers; the outer bound covers name resolution, which ureq's own
    //    cannot.
    if let (RunOutcome::Failure { error }, Some(url)) = (&outcome, &ctx.failure_webhook) {
        let (url, cluster, error) = (url.clone(), ctx.cluster_id.clone(), error.clone());
        let post = tokio::task::spawn_blocking(move || {
            crate::webhook::post_failure(&url, &cluster, "backup", &error)
        });
        if tokio::time::timeout(WEBHOOK_BOUND, post).await.is_err() {
            eprintln!(
                "warning: failure webhook still running after {}s (non-fatal)",
                WEBHOOK_BOUND.as_secs()
            );
        }
    }

    outcome
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_stop_fits_in_the_grace_period_it_is_given() {
        // Every step at its bound still leaves the runner time to exit before
        // the SIGKILL; otherwise the record is lost exactly when it matters.
        let worst = HELPER_DELETE_BOUND + STATUS_WRITE_BOUND + WEBHOOK_BOUND;
        assert!(
            worst + Duration::from_secs(10) <= TERMINATION_GRACE,
            "{worst:?} of steps in a {TERMINATION_GRACE:?} grace period"
        );
        assert!(
            WEBHOOK_BOUND > crate::webhook::WEBHOOK_TIMEOUT,
            "the outer webhook bound must leave the inner one room to fire first"
        );
    }

    #[test]
    fn the_grace_period_is_the_one_the_chart_renders() {
        // The chart's backup CronJob; `scripts/check-backup-render.sh` asserts
        // the rendered value is 90 as well.
        assert_eq!(TERMINATION_GRACE, Duration::from_secs(90));
    }

    #[test]
    fn a_run_stopped_at_its_deadline_says_so_and_names_the_knob() {
        let msg = stopped_message(
            Duration::from_secs(6 * 3600 - 20),
            Some(Duration::from_secs(6 * 3600)),
        );
        assert!(
            msg.starts_with("run exceeded its deadline of 6h and was stopped after 5h59m40s"),
            "{msg}"
        );
        assert!(msg.contains("apprafter backup set deadline"), "{msg}");
    }

    #[test]
    fn a_slow_start_still_counts_as_the_deadline() {
        // The pod started four minutes after its Job.
        let msg = stopped_message(
            Duration::from_secs(3600 - 240),
            Some(Duration::from_secs(3600)),
        );
        assert!(msg.starts_with("run exceeded its deadline of 1h"), "{msg}");
    }

    #[test]
    fn a_run_stopped_well_before_its_deadline_is_not_blamed_on_it() {
        let msg = stopped_message(
            Duration::from_secs(2 * 3600 + 13 * 60),
            Some(Duration::from_secs(6 * 3600)),
        );
        assert!(
            msg.starts_with(
                "run was stopped by Kubernetes (SIGTERM) after 2h13m, before its deadline of 6h"
            ),
            "{msg}"
        );
        assert!(msg.contains("evicted"), "{msg}");
    }

    #[test]
    fn a_run_with_no_known_deadline_names_both_causes() {
        let msg = stopped_message(Duration::from_secs(90), None);
        assert!(msg.contains("after 1m30s"), "{msg}");
        assert!(msg.contains("deadline passed"), "{msg}");
        assert!(msg.contains("evicted"), "{msg}");
    }

    #[test]
    fn human_duration_writes_whole_units_only() {
        for (secs, want) in [
            (0, "0s"),
            (45, "45s"),
            (90, "1m30s"),
            (600, "10m"),
            (3600, "1h"),
            (21600, "6h"),
            (5400, "1h30m"),
            (21590, "5h59m50s"),
            (43200, "12h"),
        ] {
            assert_eq!(human_duration(Duration::from_secs(secs)), want, "{secs}s");
        }
    }

    #[test]
    fn only_the_first_claim_wins() {
        let claim = OutcomeClaim::default();
        let other = claim.clone();
        assert!(claim.claim());
        assert!(!other.claim());
        assert!(!claim.claim());
    }

    #[test]
    fn the_live_set_forgets_a_deleted_pod() {
        let live = LiveHelperPods::default();
        live.insert("demo", "bk-pg-db");
        live.insert("demo", "bk-vol-uploads");
        live.remove("demo", "bk-pg-db");
        assert_eq!(
            live.snapshot(),
            vec![("demo".to_string(), "bk-vol-uploads".to_string())]
        );
    }
}
