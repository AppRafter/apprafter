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
//! 1. stops the work under way, two things side by side:
//!    - deletes the helper pods it has applied and not yet deleted — which
//!      also ends the exec the run was blocked in — after refusing every later
//!      apply and waiting out the one under way, so that the main thread,
//!      still running until that exec fails, cannot leave a pod behind the
//!      list;
//!    - passes the signal on to a restic it has running (`restic backup`,
//!      the check Job's `restic check`, or the prune's `restic forget` or
//!      `restic prune`) and gives it a moment to remove its repository lock
//!      and exit ([`crate::restic_child`]). Otherwise restic would be
//!      SIGKILLed with the runner and leave the lock for 30 minutes — an
//!      exclusive one, when it was checking or pruning;
//! 2. records the failure in the status ConfigMap (`lastFailure`, `lastError`);
//! 3. posts the failure webhook;
//! 4. exits 1.
//!
//! Each step is bounded (see the `*_BOUND` constants), and together they fit
//! in the grace period, so the runner always exits on its own before the
//! SIGKILL. The Job itself still fails with reason `DeadlineExceeded`: the
//! Job controller has decided that before it sends the signal.
//!
//! SIGINT — a run started by hand in a terminal, or `kill -INT 1` — is handled
//! the same way, and passed on to restic as SIGINT.
//!
//! SIGTERM is the trigger rather than a timer of the runner's own because only
//! the Job controller knows when the deadline is. It counts from the Job's
//! start, across every retry of the pod, and a timer started by the runner
//! would run late on any pod but the first. The same path also records a run
//! stopped for another reason — its pod deleted or evicted.
//!
//! The runner also stops a run itself, through the same steps
//! ([`stop_run_with`]), when its staging volume holds more than its size
//! limit ([`crate::staging`]); the failure it records then says so.
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
/// The most a restic the stop has signalled may take to remove its lock and
/// exit before it is killed. Removing the lock is one DELETE of `locks/<id>`
/// in the repository, which a reachable S3 endpoint answers in well under a
/// second: fifteen leaves room for a slow one and restic's own retry of it. A
/// restic still running at the bound is killed, and its lock stays as it
/// would have without the signal.
///
/// The bound assumes restic's own rate limits (`--limit-upload`,
/// `--limit-download`) are not used; the runner passes none. The signal does
/// not cancel a pack transfer restic is throttling: restic finishes it at the
/// throttled rate, and only then removes its lock and exits. So the wait is
/// up to one pack's size over the rate. Both Jobs in the chart set
/// `RESTIC_PACK_SIZE=4`, so a pack restic uploads is about 4 to 5 MiB (83 for
/// 400 MB in the WI-386 measurement): about 15 s at 300 KiB/s. A pack it
/// downloads has the size in use when it was written, restic's default
/// 16 MiB before the chart set the smaller one: about a minute at the same
/// rate. Measured with restic 0.18.1 on a local repository, a
/// `forget --prune` signalled two seconds into its repack at 300 KiB/s took
/// 9.2 s to exit (SIGTERM and SIGINT alike, 30 MB of data). Past the bound,
/// restic is killed and its exclusive prune lock stays.
pub const RESTIC_RELEASE_BOUND: Duration = Duration::from_secs(15);
/// Step 1, stopping the work: the helper deletes and restic's release run
/// side by side, so it takes the longer of the two bounds, not their sum.
pub const STOP_WORK_BOUND: Duration =
    if HELPER_DELETE_BOUND.as_millis() > RESTIC_RELEASE_BOUND.as_millis() {
        HELPER_DELETE_BOUND
    } else {
        RESTIC_RELEASE_BOUND
    };
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
///
/// The stop's view of them has to be complete, and the run's main thread
/// keeps going while the stop runs: it ends only once the stop has made the
/// exec it is blocked in fail. Two things close that race:
///
/// * [`LiveHelperPods::close`] — the stop's first step — makes every later
///   [`LiveHelperPods::begin_apply`] fail, so no helper pod is applied after
///   the stop has started, whatever the main thread moves on to (the next
///   claim's dump, a leftover's replacement).
/// * An apply already under way when the stop starts is waited for
///   ([`LiveHelperPods::applies_in_flight`]) before the stop deletes: a delete
///   that reached the apiserver before that apply's PATCH would answer 404,
///   and the PATCH would then create the pod after all.
#[derive(Clone, Debug, Default)]
pub struct LiveHelperPods(Arc<Mutex<LiveHelperState>>);

#[derive(Debug, Default)]
struct LiveHelperState {
    pods: BTreeSet<(String, String)>,
    /// Applies begun and not yet answered.
    applying: usize,
    /// Set by the stop; no apply begins after it.
    stopping: bool,
}

/// An apply of a helper pod under way: held for exactly as long as its
/// PATCH, which the stop waits out before it deletes.
#[must_use = "the apply counts as under way only while this is held"]
pub struct ApplyInFlight(LiveHelperPods);

impl Drop for ApplyInFlight {
    fn drop(&mut self) {
        let mut state = self.0.lock();
        state.applying = state.applying.saturating_sub(1);
    }
}

impl LiveHelperPods {
    /// Record a helper pod about to be applied, and count its apply as under
    /// way until the returned guard drops. Fails once the stop has begun: the
    /// run is over, and a pod applied now would outlive it.
    pub fn begin_apply(&self, namespace: &str, name: &str) -> cli_core::Result<ApplyInFlight> {
        let mut state = self.lock();
        if state.stopping {
            return Err(cli_core::CliError::Other(format!(
                "the run is being stopped, so helper pod {namespace}/{name} was not applied"
            )));
        }
        state.pods.insert((namespace.to_string(), name.to_string()));
        state.applying += 1;
        Ok(ApplyInFlight(self.clone()))
    }

    /// Forget a helper pod that has been deleted.
    pub fn remove(&self, namespace: &str, name: &str) {
        self.lock()
            .pods
            .remove(&(namespace.to_string(), name.to_string()));
    }

    /// The helper pods live right now.
    pub fn snapshot(&self) -> Vec<(String, String)> {
        self.lock().pods.iter().cloned().collect()
    }

    /// Refuse every apply from now on ([`Self::begin_apply`]).
    pub fn close(&self) {
        self.lock().stopping = true;
    }

    /// How many applies have begun and not yet been answered.
    pub fn applies_in_flight(&self) -> usize {
        self.lock().applying
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, LiveHelperState> {
        // A panic while holding this lock cannot leave the state half-updated
        // (every operation is one step on it), so a poisoned lock is still a
        // good state.
        self.0.lock().unwrap_or_else(|p| p.into_inner())
    }
}

/// The most the stop waits for applies already under way to be answered
/// before it deletes; part of [`HELPER_DELETE_BOUND`]. A PATCH takes
/// milliseconds; one that takes longer than this is not waited for, and the
/// pod it may yet create is the one this cannot catch.
pub const HELPER_APPLY_SETTLE_BOUND: Duration = Duration::from_secs(5);

/// The helper pods the stop must delete: refuse every later apply, wait up to
/// `settle` for the ones under way, then take what is live.
pub async fn helper_pods_to_delete(
    live: &LiveHelperPods,
    settle: Duration,
) -> Vec<(String, String)> {
    live.close();
    let deadline = tokio::time::Instant::now() + settle;
    while live.applies_in_flight() > 0 {
        if tokio::time::Instant::now() >= deadline {
            eprintln!(
                "stop: {} helper-pod apply(s) still unanswered after {}s; deleting what is known",
                live.applies_in_flight(),
                settle.as_secs()
            );
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    live.snapshot()
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

    /// Whether the outcome has been claimed, without claiming it.
    pub fn is_claimed(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }

    /// `record`, made to write nothing once the outcome is claimed. For the
    /// check run, which records each step as it ends rather than one outcome
    /// at the end: a stop claims the outcome before it signals restic, and
    /// then records the step it stopped, with its reason. What the run would
    /// record after that is only what the stop did to it (restic's `context
    /// canceled`, a restic start refused), and written last it replaced the
    /// reason: on kind, a staging overrun during the prune was recorded as
    /// `restic forget failed … context canceled`.
    pub fn unless_claimed<'a>(
        &'a self,
        mut record: impl FnMut(serde_json::Value) + 'a,
    ) -> impl FnMut(serde_json::Value) + 'a {
        move |data| {
            if !self.is_claimed() {
                record(data)
            }
        }
    }
}

/// `6h`, `90m`, `45s`, `5h59m50s`: a duration as `apprafter backup set
/// deadline` takes one. Shared with backup-core's keep-alive explanation.
pub use backup_core::helper_pod::human_duration;

/// The signal that stopped the run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StopSignal {
    /// SIGTERM: Kubernetes, at the Job's deadline or on deleting the pod.
    Terminate,
    /// SIGINT: someone interrupting a run started by hand.
    Interrupt,
}

impl StopSignal {
    /// `SIGTERM` / `SIGINT`.
    pub fn name(self) -> &'static str {
        match self {
            StopSignal::Terminate => "SIGTERM",
            StopSignal::Interrupt => "SIGINT",
        }
    }

    /// The signal's number, to pass it on to restic unchanged.
    pub fn number(self) -> libc::c_int {
        match self {
            StopSignal::Terminate => libc::SIGTERM,
            StopSignal::Interrupt => libc::SIGINT,
        }
    }
}

/// Which Job a stopped run belongs to: it decides what the stop records,
/// and which knob its message names.
#[derive(Clone, Debug)]
pub enum StopKind {
    /// A backup run. The stop records `lastFailure` / `lastError`, with the
    /// staging format the run used.
    Backup { format: &'static str },
    /// The weekly check run. The stop records its failure against the step
    /// under way ([`crate::check::CheckPhase`]): the check, or the prune
    /// after it.
    Check { phase: crate::check::PhaseCell },
}

impl StopKind {
    /// What a message calls the Job and its work, the field that sets its
    /// deadline, and the command that raises it.
    fn deadline_knob(&self) -> (&'static str, &'static str, &'static str, &'static str) {
        match self {
            StopKind::Backup { .. } => (
                "a backup Job",
                "the backup needs",
                "spec.backup.activeDeadlineSeconds",
                "`apprafter backup set deadline`",
            ),
            StopKind::Check { .. } => (
                "a check Job",
                "the check and the prune after it need",
                "spec.backup.checkActiveDeadlineSeconds",
                "`apprafter backup set check-deadline`",
            ),
        }
    }
}

/// The `lastError` and webhook text for a backup run stopped by `signal`,
/// after `ran_for` of a run whose deadline is `deadline` (`None`: not known).
pub fn stopped_message(
    ran_for: Duration,
    deadline: Option<Duration>,
    signal: StopSignal,
) -> String {
    stopped_message_for(&StopKind::Backup { format: "" }, ran_for, deadline, signal)
}

/// [`stopped_message`] for a run of either Job.
pub fn stopped_message_for(
    kind: &StopKind,
    ran_for: Duration,
    deadline: Option<Duration>,
    signal: StopSignal,
) -> String {
    let ran = human_duration(ran_for);
    if signal == StopSignal::Interrupt {
        return format!("run was interrupted (SIGINT) after {ran}");
    }
    let (job, work, field, raise) = kind.deadline_knob();
    match deadline {
        Some(d) if ran_for + DEADLINE_START_ALLOWANCE >= d => format!(
            "run exceeded its deadline of {} and was stopped after {ran}: {job} is stopped \
             when its deadline passes ({field}). If {work} longer, raise it with \
             {raise}; if it should not have taken this long, the run was stuck{}",
            human_duration(d),
            match kind {
                StopKind::Backup { .. } => " — the helper pods it was using have been deleted",
                StopKind::Check { .. } => "",
            }
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

/// What the stop records for `kind`, and the phase its failure webhook names:
/// `(status fields, phase)`. An empty object records nothing: a check run
/// stopped while reading the repository's figures has its check and prune
/// recorded already.
pub fn stop_record(kind: &StopKind, error: &str, now: &str) -> (serde_json::Value, &'static str) {
    use crate::check::CheckPhase;
    use crate::status::{check_record, prune_record, CheckResult, PruneRecord};
    match kind {
        StopKind::Backup { format } => (
            crate::status::status_configmap(
                &RunOutcome::Failure {
                    error: error.to_string(),
                },
                format,
                now,
            )["data"]
                .clone(),
            "backup",
        ),
        StopKind::Check { phase } => match phase.get() {
            CheckPhase::Check => (
                check_record(&CheckResult::Failed(error.to_string()), now),
                "check",
            ),
            CheckPhase::Prune => (
                prune_record(&PruneRecord::Failed(error.to_string()), "check", now),
                "prune",
            ),
            CheckPhase::Stats => (serde_json::json!({}), "check"),
        },
    }
}

/// Everything the stop needs, captured when the run starts. Cloned for each
/// thing that can stop the run: a signal, or the staging volume passing its
/// limit ([`crate::staging`]).
#[derive(Clone)]
pub struct StopContext {
    pub client: kube::Client,
    pub live_helpers: LiveHelperPods,
    /// The restic processes the run has running.
    pub restic: crate::restic_child::LiveResticChildren,
    pub started: Instant,
    pub deadline: Option<Duration>,
    /// Which Job this is, and so what the stop records.
    pub kind: StopKind,
    pub cluster_id: String,
    pub failure_webhook: Option<String>,
}

/// Record a run `signal` is stopping (see the module docs). Returns once
/// every step has finished or run out of time; the caller then exits.
pub async fn stop_run(ctx: &StopContext, signal: StopSignal) -> RunOutcome {
    let error = stopped_message_for(&ctx.kind, ctx.started.elapsed(), ctx.deadline, signal);
    stop_run_with(ctx, error, signal.number()).await
}

/// Stop the run for the reason `error` gives, the same way whatever stopped
/// it: the work under way stopped (helper pods deleted, `restic_signal`
/// passed on to a running restic so it removes its lock), then `error`
/// recorded as the run's failure and posted to the failure webhook. Each step
/// is bounded (the `*_BOUND` constants). Returns once every step has finished
/// or run out of time; the caller then exits.
///
/// The caller must hold the run's [`OutcomeClaim`]: exactly one of the run
/// and its stops records the outcome.
pub async fn stop_run_with(
    ctx: &StopContext,
    error: String,
    restic_signal: libc::c_int,
) -> RunOutcome {
    eprintln!("run stopped: {error}");

    // 1. Stop the work under way: the helper pods and restic, side by side.
    //
    //    Helper pods: deleting one ends the exec the run is blocked in.
    //    Grace 0, because the work in them is abandoned. A dump killed now has
    //    its database session, and the table locks that session holds, ended
    //    within seconds, even while it waits on a lock: the helper connects with
    //    `client_connection_check_interval`
    //    (`backup_core::extract::PG_HELPER_PGOPTIONS`). Without that, the
    //    session would outlive the kill.
    //    First every apply is refused and the ones under way are waited out,
    //    so the set is complete (see `LiveHelperPods`).
    let delete_helpers = async {
        let pods = helper_pods_to_delete(&ctx.live_helpers, HELPER_APPLY_SETTLE_BOUND).await;
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
        futures::future::join_all(deletes).await;
    };
    let delete_helpers = async {
        if tokio::time::timeout(HELPER_DELETE_BOUND, delete_helpers)
            .await
            .is_err()
        {
            eprintln!(
                "stop: helper-pod deletes still running after {}s; going on",
                HELPER_DELETE_BOUND.as_secs()
            );
        }
    };
    //    restic: the signal passed on, so that it removes its lock itself.
    let release_restic =
        crate::restic_child::release_restic(&ctx.restic, restic_signal, RESTIC_RELEASE_BOUND);
    tokio::join!(delete_helpers, release_restic);

    // 2. The status ConfigMap: the backup's failure, or the check run's
    //    against the step it was in.
    let now = chrono::Utc::now().to_rfc3339();
    let (data, phase) = stop_record(&ctx.kind, &error, &now);
    let outcome = RunOutcome::Failure { error };
    if data.as_object().is_some_and(|d| !d.is_empty()) {
        match tokio::time::timeout(
            STATUS_WRITE_BOUND,
            crate::status::write_status_data(&ctx.client, &data),
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
    }

    // 3. The failure webhook. ureq blocks, so it runs off the runtime's
    //    workers; the outer bound covers name resolution, which ureq's own
    //    cannot.
    if let (RunOutcome::Failure { error }, Some(url)) = (&outcome, &ctx.failure_webhook) {
        let (url, cluster, error) = (url.clone(), ctx.cluster_id.clone(), error.clone());
        let post = tokio::task::spawn_blocking(move || {
            crate::webhook::post_failure(&url, &cluster, phase, &error)
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

    /// A stub apiserver that answers every request 200 with an empty object
    /// and records `"<METHOD> <path>"`: enough for the stop's deletes and its
    /// status write to go through.
    #[derive(Clone, Default)]
    struct RecordingApiServer(Arc<Mutex<Vec<String>>>);

    impl tower_service::Service<http::Request<kube::client::Body>> for RecordingApiServer {
        type Response = http::Response<kube::client::Body>;
        type Error = std::convert::Infallible;
        type Future = std::future::Ready<Result<Self::Response, Self::Error>>;

        fn poll_ready(
            &mut self,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Result<(), Self::Error>> {
            std::task::Poll::Ready(Ok(()))
        }

        fn call(&mut self, req: http::Request<kube::client::Body>) -> Self::Future {
            self.0
                .lock()
                .unwrap()
                .push(format!("{} {}", req.method(), req.uri().path()));
            std::future::ready(Ok(http::Response::builder()
                .status(200)
                .header("content-type", "application/json")
                .body(kube::client::Body::from(
                    br#"{"apiVersion":"v1","kind":"ConfigMap","metadata":{"name":"x"}}"#.to_vec(),
                ))
                .unwrap()))
        }
    }

    /// The stop's first step reaches both halves of the work: the helper pod
    /// is deleted, and the signal it received is passed on to the restic the
    /// run has running, which exits — and only then is the run recorded.
    #[test]
    fn the_stop_deletes_the_helpers_and_passes_its_signal_on_to_restic() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let (started, got) = (dir.path().join("started"), dir.path().join("got"));
        let bin = dir.path().join("restic");
        std::fs::write(
            &bin,
            format!(
                "#!/bin/sh\n[ \"$1\" = __probe ] && exit 0\n\
                 trap 'echo TERM > {got}; exit 1' TERM\n\
                 trap 'echo INT > {got}; exit 1' INT\n\
                 touch {started}\nwhile :; do sleep 0.05; done\n",
                got = got.display(),
                started = started.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        for _ in 0..200 {
            match std::process::Command::new(&bin).arg("__probe").status() {
                Err(e) if e.raw_os_error() == Some(26) => {
                    std::thread::sleep(Duration::from_millis(5))
                }
                _ => break,
            }
        }

        for (signal, name) in [
            (StopSignal::Terminate, "TERM"),
            (StopSignal::Interrupt, "INT"),
        ] {
            let _ = std::fs::remove_file(&started);
            let rt = tokio::runtime::Runtime::new().unwrap();
            let seen = RecordingApiServer::default();
            let client = {
                let _guard = rt.enter();
                kube::Client::new(seen.clone(), "default")
            };
            let restic = crate::restic_child::ForwardingRestic::new(&bin);
            let live_restic = restic.live_children();
            let run = std::thread::spawn(move || {
                use backup_core::ResticRunner as _;
                restic.run(&["backup".to_string()], "pw")
            });
            let deadline = Instant::now() + Duration::from_secs(10);
            while !started.exists() {
                assert!(Instant::now() < deadline, "the fake restic never started");
                std::thread::sleep(Duration::from_millis(10));
            }
            let live_helpers = LiveHelperPods::default();
            drop(live_helpers.begin_apply("shop", "bk-pg-db").unwrap());

            let ctx = StopContext {
                client,
                live_helpers,
                restic: live_restic.clone(),
                started: Instant::now(),
                deadline: None,
                kind: StopKind::Backup {
                    format: "sequential",
                },
                cluster_id: "test".into(),
                failure_webhook: None,
            };
            let t0 = Instant::now();
            let outcome = rt.block_on(stop_run(&ctx, signal));
            assert!(t0.elapsed() < Duration::from_secs(5), "{:?}", t0.elapsed());
            assert_eq!(outcome.exit_code(), 1);
            assert_eq!(std::fs::read_to_string(&got).unwrap(), format!("{name}\n"));
            assert!(run.join().unwrap().is_err());
            assert_eq!(live_restic.running(), 0);
            let seen = seen.0.lock().unwrap().clone();
            assert!(
                seen.contains(&"DELETE /api/v1/namespaces/shop/pods/bk-pg-db".to_string()),
                "{seen:?}"
            );
            assert!(
                seen.iter()
                    .any(|r| r.contains("configmaps/apprafter-backup-status")),
                "the run is recorded: {seen:?}"
            );
        }
    }

    /// A stop for a reason of the runner's own (the staging volume past its
    /// limit) takes the same steps, records the reason it is given rather
    /// than a signal's, and passes on the signal it is told to.
    #[test]
    fn a_stop_for_the_runners_own_reason_records_that_reason() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let (started, got) = (dir.path().join("started"), dir.path().join("got"));
        let bin = dir.path().join("restic");
        std::fs::write(
            &bin,
            format!(
                "#!/bin/sh\n[ \"$1\" = __probe ] && exit 0\n\
                 trap 'echo TERM > {got}; exit 1' TERM\n\
                 trap 'echo INT > {got}; exit 1' INT\n\
                 touch {started}\nwhile :; do sleep 0.05; done\n",
                got = got.display(),
                started = started.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        for _ in 0..200 {
            match std::process::Command::new(&bin).arg("__probe").status() {
                Err(e) if e.raw_os_error() == Some(26) => {
                    std::thread::sleep(Duration::from_millis(5))
                }
                _ => break,
            }
        }
        let rt = tokio::runtime::Runtime::new().unwrap();
        let seen = RecordingApiServer::default();
        let client = {
            let _guard = rt.enter();
            kube::Client::new(seen.clone(), "default")
        };
        let restic = crate::restic_child::ForwardingRestic::new(&bin);
        let live_restic = restic.live_children();
        let run = std::thread::spawn(move || {
            use backup_core::ResticRunner as _;
            restic.run(&["backup".to_string()], "pw")
        });
        let deadline = Instant::now() + Duration::from_secs(10);
        while !started.exists() {
            assert!(Instant::now() < deadline, "the fake restic never started");
            std::thread::sleep(Duration::from_millis(10));
        }
        let live_helpers = LiveHelperPods::default();
        drop(live_helpers.begin_apply("shop", "bk-pg-db").unwrap());
        let ctx = StopContext {
            client,
            live_helpers,
            restic: live_restic.clone(),
            started: Instant::now(),
            deadline: Some(Duration::from_secs(21600)),
            kind: StopKind::Backup {
                format: "monolithic",
            },
            cluster_id: "test".into(),
            failure_webhook: None,
        };

        let reason = "the staging volume held 1.5Gi, more than its limit of 1.0Gi";
        let outcome = rt.block_on(stop_run_with(&ctx, reason.to_string(), libc::SIGINT));
        match &outcome {
            RunOutcome::Failure { error } => assert_eq!(error, reason),
            RunOutcome::Success { .. } => panic!("a stopped run recorded success"),
        }
        assert_eq!(outcome.exit_code(), 1);
        assert_eq!(std::fs::read_to_string(&got).unwrap(), "INT\n");
        assert!(run.join().unwrap().is_err());
        assert_eq!(live_restic.running(), 0);
        let seen = seen.0.lock().unwrap().clone();
        assert!(
            seen.contains(&"DELETE /api/v1/namespaces/shop/pods/bk-pg-db".to_string()),
            "{seen:?}"
        );
        assert!(
            seen.iter()
                .any(|r| r.contains("configmaps/apprafter-backup-status")),
            "the run is recorded: {seen:?}"
        );
    }

    #[test]
    fn the_stop_fits_in_the_grace_period_it_is_given() {
        // Every step at its bound still leaves the runner time to exit before
        // the SIGKILL; otherwise the record is lost exactly when it matters.
        // Step 1's two halves run side by side: the longer one counts.
        assert_eq!(
            STOP_WORK_BOUND,
            HELPER_DELETE_BOUND.max(RESTIC_RELEASE_BOUND)
        );
        let worst = STOP_WORK_BOUND + STATUS_WRITE_BOUND + WEBHOOK_BOUND;
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
            StopSignal::Terminate,
        );
        assert!(
            msg.starts_with("run exceeded its deadline of 6h and was stopped after 5h59m40s"),
            "{msg}"
        );
        assert!(msg.contains("apprafter backup set deadline"), "{msg}");
    }

    #[test]
    fn a_check_run_stopped_at_its_deadline_names_the_checks_own_knob() {
        let kind = StopKind::Check {
            phase: crate::check::PhaseCell::default(),
        };
        let msg = stopped_message_for(
            &kind,
            Duration::from_secs(6 * 3600 - 20),
            Some(Duration::from_secs(6 * 3600)),
            StopSignal::Terminate,
        );
        assert!(msg.starts_with("run exceeded its deadline of 6h"), "{msg}");
        assert!(msg.contains("checkActiveDeadlineSeconds"), "{msg}");
        assert!(msg.contains("apprafter backup set check-deadline"), "{msg}");
        assert!(!msg.contains("helper pods"), "a check has none: {msg}");
    }

    /// A stopped check run records its failure against the step it was in:
    /// a stop during the check is a check that did not pass; one during the
    /// prune leaves the check's pass alone and fails the prune.
    #[test]
    fn a_stopped_check_run_records_the_step_it_was_in() {
        use crate::check::PhaseCell;
        let backup = StopKind::Backup {
            format: "monolithic",
        };
        let (data, phase) = stop_record(&backup, "stopped", "t");
        assert_eq!(phase, "backup");
        assert_eq!(data["lastFailure"], "t");
        assert_eq!(data["lastError"], "stopped");

        let cell = PhaseCell::default();
        let check = StopKind::Check {
            phase: cell.clone(),
        };
        let (data, phase) = stop_record(&check, "stopped", "t");
        assert_eq!(phase, "check");
        assert_eq!(data["lastCheckResult"], "failed");
        assert_eq!(data["lastCheckError"], "stopped");
        assert!(data.get("lastFailure").is_none(), "{data}");

        // Moved on by the run itself, through the same cell.
        let mut records = Vec::new();
        let r = StuckInPrune;
        let depth = backup_core::restic::CheckDepth::Structure;
        let retention = backup_core::prune::RetentionPolicy::default();
        let plan = crate::check::CheckPlan {
            repo: "s3:x",
            passphrase: "pw",
            depth: &depth,
            enforce: crate::config::Enforce::Check,
            retention: &retention,
            backup_run_deadline: backup_core::helper_pod::DEFAULT_RUN_DEADLINE,
        };
        let mut at_prune = None;
        crate::check::run_check(
            &r,
            &plan,
            &cell,
            &mut || {
                at_prune = Some(stop_record(&check, "stopped", "t"));
                Ok("11111111-2222-3333-4444-555555555555".into())
            },
            &chrono::Utc::now,
            &mut |v| records.push(v),
        );
        let (data, phase) = at_prune.expect("the prune began");
        assert_eq!(phase, "prune");
        assert_eq!(data["lastPruneResult"], "failed");
        assert!(data.get("lastCheck").is_none(), "{data}");
        let (data, _) = stop_record(&check, "stopped", "t");
        assert_eq!(
            data,
            serde_json::json!({}),
            "the figures step records nothing"
        );
    }

    /// A restic whose every command succeeds with an empty repository.
    struct StuckInPrune;

    impl backup_core::ResticRunner for StuckInPrune {
        fn run(&self, _: &[String], _: &str) -> cli_core::Result<()> {
            Ok(())
        }
        fn run_stdout(&self, argv: &[String], _: &str) -> cli_core::Result<String> {
            Ok(if argv[0] == "snapshots" { "[]" } else { "{}" }.into())
        }
        fn run_backup(&self, _: &[String], _: &str) -> cli_core::Result<Option<String>> {
            Ok(None)
        }
        fn run_capture(
            &self,
            argv: &[String],
            p: &str,
        ) -> cli_core::Result<backup_core::ResticOutput> {
            Ok(backup_core::ResticOutput {
                stdout: self.run_stdout(argv, p)?,
                stderr: String::new(),
            })
        }
    }

    #[test]
    fn a_slow_start_still_counts_as_the_deadline() {
        // The pod started four minutes after its Job.
        let msg = stopped_message(
            Duration::from_secs(3600 - 240),
            Some(Duration::from_secs(3600)),
            StopSignal::Terminate,
        );
        assert!(msg.starts_with("run exceeded its deadline of 1h"), "{msg}");
    }

    #[test]
    fn a_run_stopped_well_before_its_deadline_is_not_blamed_on_it() {
        let msg = stopped_message(
            Duration::from_secs(2 * 3600 + 13 * 60),
            Some(Duration::from_secs(6 * 3600)),
            StopSignal::Terminate,
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
        let msg = stopped_message(Duration::from_secs(90), None, StopSignal::Terminate);
        assert!(msg.contains("after 1m30s"), "{msg}");
        assert!(msg.contains("deadline passed"), "{msg}");
        assert!(msg.contains("evicted"), "{msg}");
    }

    #[test]
    fn a_run_interrupted_by_hand_is_not_blamed_on_kubernetes() {
        let msg = stopped_message(
            Duration::from_secs(6 * 3600 - 20),
            Some(Duration::from_secs(6 * 3600)),
            StopSignal::Interrupt,
        );
        assert_eq!(msg, "run was interrupted (SIGINT) after 5h59m40s");
    }

    #[test]
    fn the_signal_passed_on_is_the_one_received() {
        assert_eq!(StopSignal::Terminate.number(), libc::SIGTERM);
        assert_eq!(StopSignal::Interrupt.number(), libc::SIGINT);
        assert_eq!(StopSignal::Terminate.name(), "SIGTERM");
        assert_eq!(StopSignal::Interrupt.name(), "SIGINT");
    }

    #[test]
    fn only_the_first_claim_wins() {
        let claim = OutcomeClaim::default();
        let other = claim.clone();
        assert!(claim.claim());
        assert!(!other.claim());
        assert!(!claim.claim());
    }

    /// A step the check run records goes through until a stop claims the
    /// outcome, and none after it: the stop's record of the step it stopped
    /// is the last word.
    #[test]
    fn a_step_recorded_after_the_stop_has_claimed_is_not_written() {
        let claim = OutcomeClaim::default();
        let stop = claim.clone();
        let mut written = Vec::new();
        {
            let mut record = claim.unless_claimed(|v| written.push(v));
            record(serde_json::json!({"lastCheckResult": "passed"}));
            assert!(!claim.is_claimed(), "recording does not claim");
            assert!(stop.claim(), "the stop claims first");
            record(serde_json::json!({"lastPruneResult": "failed",
                "lastPruneError": "restic forget failed: context canceled"}));
        }
        assert_eq!(
            written,
            vec![serde_json::json!({"lastCheckResult": "passed"})]
        );
        // The run's own claim at its end then fails, as before.
        assert!(!claim.claim());
    }

    #[test]
    fn the_live_set_forgets_a_deleted_pod() {
        let live = LiveHelperPods::default();
        drop(live.begin_apply("demo", "bk-pg-db").unwrap());
        drop(live.begin_apply("demo", "bk-vol-uploads").unwrap());
        live.remove("demo", "bk-pg-db");
        assert_eq!(
            live.snapshot(),
            vec![("demo".to_string(), "bk-vol-uploads".to_string())]
        );
    }

    #[test]
    fn no_helper_pod_is_applied_once_the_stop_has_begun() {
        let live = LiveHelperPods::default();
        let first = live.begin_apply("demo", "bk-pg-a").unwrap();
        assert_eq!(live.applies_in_flight(), 1);
        drop(first);
        assert_eq!(live.applies_in_flight(), 0);

        live.close();
        let err = live
            .begin_apply("demo", "bk-pg-b")
            .err()
            .expect("an apply after the stop has begun is refused");
        assert!(err.to_string().contains("being stopped"), "{err}");
        assert!(err.to_string().contains("demo/bk-pg-b"), "{err}");
        // …and it was never recorded as live, nor as under way.
        assert_eq!(
            live.snapshot(),
            vec![("demo".to_string(), "bk-pg-a".to_string())]
        );
        assert_eq!(live.applies_in_flight(), 0);
    }

    /// The stop takes its list only once an apply that was under way has been
    /// answered: deleting before that PATCH lands answers 404, and the PATCH
    /// then creates the pod after all.
    #[test]
    fn the_stop_waits_for_an_apply_under_way_before_it_lists_the_pods() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let live = LiveHelperPods::default();
        let in_flight = live.begin_apply("demo", "bk-pg-db").unwrap();
        let answered = Arc::new(AtomicBool::new(false));
        let apply = {
            let answered = Arc::clone(&answered);
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(300));
                answered.store(true, Ordering::SeqCst);
                drop(in_flight);
            })
        };
        let started = Instant::now();
        let pods = rt.block_on(helper_pods_to_delete(&live, Duration::from_secs(5)));
        assert!(
            answered.load(Ordering::SeqCst),
            "the list was taken before the apply under way was answered"
        );
        assert!(started.elapsed() >= Duration::from_millis(300));
        assert_eq!(pods, vec![("demo".to_string(), "bk-pg-db".to_string())]);
        apply.join().unwrap();
        assert!(live.begin_apply("demo", "bk-pg-next").is_err());
    }

    /// An apply that is never answered does not hold the stop past its bound.
    #[test]
    fn an_unanswered_apply_holds_the_stop_only_for_its_bound() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let live = LiveHelperPods::default();
        let _stuck = live.begin_apply("demo", "bk-pg-db").unwrap();
        let started = Instant::now();
        let pods = rt.block_on(helper_pods_to_delete(&live, Duration::from_millis(200)));
        let took = started.elapsed();
        assert!(
            took >= Duration::from_millis(200) && took < Duration::from_secs(2),
            "{took:?}"
        );
        assert_eq!(pods, vec![("demo".to_string(), "bk-pg-db".to_string())]);
    }

    #[test]
    fn waiting_out_applies_is_part_of_the_helper_delete_bound() {
        assert!(HELPER_APPLY_SETTLE_BOUND < HELPER_DELETE_BOUND);
    }
}
