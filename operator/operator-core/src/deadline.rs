// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! A deadline on every reconcile (WI-400, GOTCHA-51).
//!
//! kube-runtime runs at most one reconcile per object at a time and HOLDS
//! every later trigger for that object until the running one returns; with
//! `concurrency(1)` it holds every trigger for every object of the kind. Its
//! controller `Config` has a debounce and a concurrency, no timeout, so a
//! reconcile that never returns freezes its object — no status, no Event, no
//! log line — while its siblings carry on. The kube client's 295s read
//! timeout bounds only a socket that stops answering, and only one I/O at a
//! time.
//!
//! [`within`] bounds the WHOLE reconcile. Expiry is an error, not a quiet
//! cancel: [`ReconcileTimedOut`] converts into the controller's own error type
//! (each controller enum carries `#[error(transparent)] TimedOut(#[from]
//! ReconcileTimedOut)`), so it takes the path every other failure takes —
//! `error_policy` logs it and requeues — and kube-runtime releases the held
//! triggers the moment the slot frees.
//!
//! Wire it at the `Controller::run` call, not inside `reconcile`, so the
//! direct `reconcile()` unit tests stay as they are:
//!
//! ```text
//! .run(
//!     |obj, ctx| operator_core::deadline::within(RECONCILE_DEADLINE, reconcile(obj, ctx)),
//!     error_policy,
//!     ctx,
//! )
//! ```
//!
//! Each controller crate owns its `RECONCILE_DEADLINE` next to its `run()`,
//! because the honest bound is that controller's worst legitimate pass.
//!
//! What it can and cannot do:
//!
//! - It cuts at the next `.await` — every apiserver, registry and database
//!   call. A thread blocked synchronously inside a reconcile is not
//!   interrupted.
//! - Dropping a request does not un-send it. A write the apiserver already
//!   accepted may still commit after the reconcile was abandoned, so the
//!   reconcile that follows must start from what is there, not from what
//!   the abandoned one last sent — which each controller's wiring has to
//!   check for its own writes.
//! - It writes nothing, and a timeout must never become a status write: an
//!   SSA under the controller's own field manager that omits a field PRUNES
//!   it. A controller surfaces the timeout only through paths that cannot
//!   prune — a WARN and `Metrics::reconcile_timeouts` from its
//!   `error_policy`, the ProblemLedger behind Application's
//!   `status.recentProblems`, an Event, or one listMap condition written
//!   under a field manager of its own.

use std::future::Future;
use std::time::Duration;

use thiserror::Error;

/// A reconcile ran past its deadline and was abandoned.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error("reconcile did not finish within {}s", .after.as_secs())]
pub struct ReconcileTimedOut {
    /// The deadline the reconcile ran past.
    pub after: Duration,
}

/// Run `reconcile`, or give up on it once `deadline` has passed.
///
/// A reconcile that finishes in time — including one that finishes on the
/// very tick the deadline expires, because the inner future is polled before
/// the timer — comes back exactly as it finished, `Ok` or `Err`. One that
/// does not is dropped at its pending `.await` and comes back as
/// `Err(ReconcileTimedOut { after: deadline }.into())`.
pub async fn within<T, E: From<ReconcileTimedOut>>(
    deadline: Duration,
    reconcile: impl Future<Output = Result<T, E>>,
) -> Result<T, E> {
    match tokio::time::timeout(deadline, reconcile).await {
        Ok(finished) => finished,
        Err(_elapsed) => Err(ReconcileTimedOut { after: deadline }.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    use futures::StreamExt;
    use k8s_openapi::api::core::v1::ConfigMap;
    use kube::api::ObjectMeta;
    use kube::runtime::controller::{Action, Config};
    use kube::runtime::reflector::{self, ObjectRef};
    use kube::runtime::{applier, watcher};
    use tokio::time::Instant;

    /// A controller error shaped like the ones in `operator-controllers`.
    #[derive(Debug, Error, PartialEq)]
    enum TestError {
        #[error("the apiserver said no")]
        Refused,
        #[error(transparent)]
        TimedOut(#[from] ReconcileTimedOut),
    }

    const DEADLINE: Duration = Duration::from_secs(120);
    const ONE_MS: Duration = Duration::from_millis(1);

    #[tokio::test(start_paused = true)]
    async fn a_reconcile_that_never_returns_is_cut_at_exactly_the_deadline() {
        let started = Instant::now();
        let mut cut = tokio::spawn(within::<(), TestError>(DEADLINE, std::future::pending()));

        tokio::time::sleep(DEADLINE - ONE_MS).await;
        // Let anything woken on this same tick run before looking.
        tokio::task::yield_now().await;
        assert!(
            !cut.is_finished(),
            "within() gave up before the deadline (at {:?})",
            started.elapsed()
        );

        // Bounded from outside too, so a helper that never cuts fails this
        // test instead of hanging it.
        let outcome = tokio::time::timeout(DEADLINE, &mut cut)
            .await
            .expect("within() must give up on a reconcile that never returns")
            .expect("the task must not panic");
        assert_eq!(started.elapsed(), DEADLINE);
        assert_eq!(
            outcome,
            Err(TestError::TimedOut(ReconcileTimedOut { after: DEADLINE }))
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_reconcile_that_finishes_in_time_is_returned_unchanged() {
        let ok: Result<u32, TestError> = within(DEADLINE, async {
            tokio::time::sleep(DEADLINE - ONE_MS).await;
            Ok(7)
        })
        .await;
        assert_eq!(ok, Ok(7));

        let started = Instant::now();
        let err: Result<u32, TestError> = within(DEADLINE, async {
            tokio::time::sleep(Duration::from_secs(5)).await;
            Err(TestError::Refused)
        })
        .await;
        assert_eq!(err, Err(TestError::Refused), "its own error, not a timeout");
        assert_eq!(
            started.elapsed(),
            Duration::from_secs(5),
            "an early error must not wait for the deadline"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_reconcile_that_finishes_on_the_deadline_is_kept() {
        let at_the_line: Result<u32, TestError> = within(DEADLINE, async {
            tokio::time::sleep(DEADLINE).await;
            Ok(1)
        })
        .await;
        assert_eq!(at_the_line, Ok(1));
    }

    #[test]
    fn the_timeout_reads_through_a_controller_error_unchanged() {
        let err = TestError::from(ReconcileTimedOut { after: DEADLINE });
        assert_eq!(err.to_string(), "reconcile did not finish within 120s");
    }

    /// GOTCHA-51 end to end, through kube-runtime's own `applier` (the loop
    /// `Controller::run` drives): the first reconcile of an object hangs, and
    /// a second trigger for the SAME object arrives while it does. kube-runtime
    /// holds that trigger for as long as the hang lasts. With the deadline,
    /// `error_policy` sees the timeout at the deadline and the held trigger
    /// runs at that same instant — not after the policy's requeue.
    #[tokio::test(start_paused = true)]
    async fn a_hung_reconcile_reaches_error_policy_and_releases_the_held_trigger() {
        let (store, mut writer) = reflector::store::<ConfigMap>();
        let web = ConfigMap {
            metadata: ObjectMeta {
                name: Some("web".into()),
                namespace: Some("demo".into()),
                ..Default::default()
            },
            ..Default::default()
        };
        // `InitDone` swaps the init buffer INTO the store, so it goes first.
        writer.apply_watcher_event(&watcher::Event::InitDone);
        writer.apply_watcher_event(&watcher::Event::Apply(web.clone()));

        let calls = Arc::new(AtomicUsize::new(0));
        let reconciled_at = Arc::new(Mutex::new(Vec::<Duration>::new()));
        let policy_saw = Arc::new(Mutex::new(Vec::<(Duration, String)>::new()));
        let t0 = Instant::now();

        let (queue_tx, queue_rx) = futures::channel::mpsc::unbounded::<ObjectRef<ConfigMap>>();
        let applier = applier(
            {
                let calls = calls.clone();
                let reconciled_at = reconciled_at.clone();
                move |_obj: Arc<ConfigMap>, _ctx: Arc<()>| {
                    let first = calls.fetch_add(1, Ordering::SeqCst) == 0;
                    let reconciled_at = reconciled_at.clone();
                    Box::pin(within(DEADLINE, async move {
                        if first {
                            // The stall: a request the apiserver accepted
                            // and never answered.
                            std::future::pending::<()>().await;
                        }
                        reconciled_at.lock().unwrap().push(t0.elapsed());
                        Ok::<_, TestError>(Action::await_change())
                    }))
                }
            },
            {
                let policy_saw = policy_saw.clone();
                move |_obj: Arc<ConfigMap>, err: &TestError, _ctx: Arc<()>| {
                    policy_saw
                        .lock()
                        .unwrap()
                        .push((t0.elapsed(), err.to_string()));
                    // A requeue far beyond the test window, so the only way
                    // the second reconcile can run is the held trigger.
                    Action::requeue(Duration::from_secs(3600))
                }
            },
            Arc::new(()),
            store,
            queue_rx.map(Ok::<_, std::convert::Infallible>),
            Config::default(),
        );
        let driven = tokio::spawn(applier.for_each(|_| async {}));

        queue_tx.unbounded_send(ObjectRef::from_obj(&web)).unwrap();
        tokio::time::sleep(Duration::from_secs(1)).await;
        // A sibling's progress re-fires the owner while it is still stuck.
        queue_tx.unbounded_send(ObjectRef::from_obj(&web)).unwrap();
        tokio::time::sleep(DEADLINE * 2).await;

        assert_eq!(
            *policy_saw.lock().unwrap(),
            vec![(DEADLINE, "reconcile did not finish within 120s".to_string())],
            "error_policy must see the timeout, at the deadline"
        );
        assert_eq!(
            *reconciled_at.lock().unwrap(),
            vec![DEADLINE],
            "the held trigger must run the moment the hung reconcile is abandoned"
        );
        assert_eq!(calls.load(Ordering::SeqCst), 2);

        drop(queue_tx);
        driven.abort();
    }
}
