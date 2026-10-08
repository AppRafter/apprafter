// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! kube-rs Controller for v1alpha1 `ResourceClaim` (Phase 2.3) —
//! matches each claim to a ServiceProvider by type + label superset and
//! records the winner in `status.provider`. Provisioning is 2.4.

use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use kube::api::Api;
use kube::runtime::controller::{Action, Controller};
use kube::runtime::watcher;
use kube::{Client, Resource};
use thiserror::Error;
use tracing::{info, warn};

use operator_core::{Metrics, ResourceClaim};

pub mod reconcile;

pub(crate) const KIND: &str = "ResourceClaim";
pub(crate) const FIELD_MANAGER: &str = "resourceclaim-scheduler";

/// Per-controller reconcile context.
pub struct Context {
    pub client: Client,
    pub metrics: Arc<Metrics>,
}

#[derive(Debug, Error)]
pub enum ReconcileError {
    #[error("kube api error: {0}")]
    Kube(#[from] kube::Error),
    #[error("serde error: {0}")]
    Serde(#[from] serde_json::Error),
    /// The pass ran past [`RECONCILE_DEADLINE`] and was abandoned (WI-400).
    #[error(transparent)]
    TimedOut(#[from] operator_core::deadline::ReconcileTimedOut),
}

/// How long one scheduling pass may run before it is abandoned (WI-400).
///
/// A pass is at most three sequential apiserver requests (the ServiceProvider
/// LIST, the status apply and, on a no-match, one best-effort Event) and
/// normally takes well under a second. The apiserver bounds each request at
/// its own 60s request timeout, APF queueing included, so an answered LIST
/// and apply can legitimately take about 120s. A pass cut past 120s is
/// therefore either in the best-effort Event leg after the status apply
/// committed (harmless: one more Warning Event and a 30s requeue instead of
/// 300s), or both critical calls were already within seconds of the
/// apiserver's own 504. 120s ends an accepted-and-never-answered request
/// about 2.5x sooner than the client's 295s read timeout, which does nothing
/// at all for a hang above the socket, and stays under the 300s success
/// requeue.
///
/// Nothing tighter is needed: this controller runs at the default, unbounded
/// concurrency, so a stuck pass holds only its own claim. Abandoning one
/// converges: every pass recomputes its decision from the watched claim and
/// one LIST, and its only write is a forced apply. A PATCH abandoned
/// mid-flight may still commit after the next pass's, carrying an older
/// snapshot and `lastTransitionTime`. That late status change re-triggers
/// this controller (the watch has no predicate). A stale no-match body that
/// prunes `status.provider` errs in the reaper's safe direction, because an
/// unresolved claim vetoes reaping (`resourceclaim-provisioner`'s
/// `reaper::is_intent_for`). The timeout itself writes nothing to the claim's
/// status: [`reconcile_with_deadline`] leaves a `ReconcileTimedOut` Warning
/// Event on it, and `error_policy` warns, counts it and requeues.
pub const RECONCILE_DEADLINE: Duration = Duration::from_secs(120);

/// [`reconcile::reconcile`], abandoned once it runs past
/// [`RECONCILE_DEADLINE`]. This — never the unbounded reconcile — is what
/// [`run`] hands kube-runtime, which holds every later trigger for a claim
/// while a pass for it is in flight (WI-400).
///
/// A cut pass leaves the same `ReconcileTimedOut` Warning Event on the claim
/// that the provisioner's controllers leave on theirs
/// (`operator_core::deadline_event`), reported as
/// `apprafter-resourceclaim-scheduler`: a reader of the claim's Events sees a
/// scheduling stall too, and the reporter tells it from a provisioning one.
/// It writes nothing to the claim's status: an apply under
/// `resourceclaim-scheduler` that omits `provider` would prune it. The publish
/// is bounded, so the wrapper returns at most `deadline_event::PUBLISH_BOUND`
/// after the deadline.
pub async fn reconcile_with_deadline(
    claim: Arc<ResourceClaim>,
    ctx: Arc<Context>,
) -> Result<Action, ReconcileError> {
    let outcome = operator_core::deadline::within(
        RECONCILE_DEADLINE,
        reconcile::reconcile(claim.clone(), ctx.clone()),
    )
    .await;
    if let Err(ReconcileError::TimedOut(timed_out)) = &outcome {
        operator_core::deadline_event::publish(
            &ctx.client,
            reconcile::EVENT_REPORTER_CONTROLLER,
            claim.object_ref(&()),
            KIND,
            *timed_out,
        )
        .await;
    }
    outcome
}

/// Spawn the ResourceClaim scheduler Controller.
///
/// Watches `apprafter.io/v1alpha1` `ResourceClaim` resources
/// cluster-wide and reconciles each claim by matching it to a
/// `ServiceProvider` (Phase 2.3) then provisioning a resource
/// (Phase 2.4).
///
/// # ServiceProvider watch omitted — 300 s requeue instead
///
/// When a `ServiceProvider` is created or updated, any Pending claim
/// that now matches should re-evaluate. The idiomatic kube-rs solution
/// is `.watches(Api::<ServiceProvider>, …, mapper)` where `mapper`
/// returns an iterator of `ObjectRef<ResourceClaim>` to re-queue. To
/// enumerate all claims inside that mapper we would need an Arc-wrapped
/// `Store<ResourceClaim>` built from a reflector, which adds meaningful
/// complexity outside the scope of 2.3. Instead, the `reconcile` stub
/// returns `Action::requeue(300s)` so every Pending claim re-evaluates
/// at most 5 minutes after a provider becomes available. A proper
/// provider-watch fan-out is deferred to a future sub-task.
pub async fn run(client: Client, metrics: Arc<Metrics>) -> Result<(), ReconcileError> {
    let claims: Api<ResourceClaim> = Api::all(client.clone());
    let ctx = Arc::new(Context { client, metrics });
    info!(
        field_manager = FIELD_MANAGER,
        "ResourceClaimScheduler starting"
    );
    // No ServiceProvider watch in 2.3 — Pending claims re-evaluate on
    // the 300s requeue; a provider-watch fan-out is future work.
    Controller::new(claims, watcher::Config::default())
        .run(reconcile_with_deadline, reconcile::error_policy, ctx)
        .for_each(|res| async move {
            match res {
                Ok((obj_ref, _)) => info!(claim = %obj_ref.name, "reconciled"),
                Err(e) => warn!(error = %e, "reconcile failed"),
            }
        })
        .await;
    info!("ResourceClaimScheduler stream ended");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A wrapped apiserver error must still carry the apiserver's own words.
    ///
    /// `run`'s stream handler and `error_policy` both render a
    /// `ReconcileError` with nothing but `%err`, so whatever `Display` omits
    /// is gone from the operator log for good. An RBAC denial is the case
    /// that matters: "kube api error" alone is indistinguishable between a
    /// missing verb and an unreachable apiserver, and this repository has
    /// twice shipped a controller whose RBAC did not match its code.
    #[test]
    fn a_wrapped_apiserver_error_keeps_the_apiservers_own_message() {
        let denial = kube::Error::Api(
            kube::core::Status::failure("resourceclaims.apprafter.io is forbidden", "Forbidden")
                .with_code(403)
                .boxed(),
        );
        let shown = ReconcileError::from(denial).to_string();
        assert!(shown.contains("is forbidden"), "{shown}");
        assert!(shown.contains("Forbidden"), "{shown}");
    }

    /// The same rule for a deserialization failure: the position and the
    /// reason come from serde, and dropping them leaves "serde error" on a
    /// controller that cannot read one specific object.
    #[test]
    fn a_wrapped_serde_error_keeps_the_underlying_reason() {
        let broken = serde_json::from_str::<serde_json::Value>("{oops").unwrap_err();
        let reason = broken.to_string();
        let shown = ReconcileError::from(broken).to_string();
        assert!(shown.contains(&reason), "{shown} does not carry {reason}");
    }

    // ---- WI-400: the reconcile deadline ----

    use operator_core::deadline::ReconcileTimedOut;
    use operator_core::ResourceClaimSpec;

    /// The same rule once more, for the error a deadline produces: the WARN
    /// line is the one place the operator log reports a timed-out claim
    /// (nothing is written to its status; the claim gets only a Warning
    /// Event), so it has to say that the reconcile was cut and after how long
    /// — not "kube api error", which would send an operator after RBAC or the
    /// network.
    #[test]
    fn a_timed_out_reconcile_names_the_deadline_it_ran_past() {
        let shown = ReconcileError::from(ReconcileTimedOut {
            after: RECONCILE_DEADLINE,
        })
        .to_string();
        assert_eq!(shown, "reconcile did not finish within 120s");
    }

    /// A claim as the watch delivers it: namespaced, already Scheduled or
    /// not — the deadline does not care which arm the pass would take.
    fn claim_in_flight() -> Arc<ResourceClaim> {
        let mut claim = ResourceClaim::new(
            "web-pg",
            ResourceClaimSpec {
                type_: "pg".to_string(),
                ..Default::default()
            },
        );
        claim.metadata.namespace = Some("apps".to_string());
        Arc::new(claim)
    }

    /// The WI-400 hang, on this controller: an apiserver that accepts the
    /// ServiceProvider LIST and never answers held the reconcile — and with
    /// it every later trigger for the claim — until the client's 295s read
    /// timeout, or forever above the socket. The deadline-bounded reconcile
    /// gives up at exactly [`RECONCILE_DEADLINE`] and returns the timeout as
    /// this controller's own error, so `error_policy` sees it and kube-runtime
    /// releases the held triggers. Every request hangs here, the
    /// `ReconcileTimedOut` Event publish included, so the wrapper returns one
    /// publish bound after the deadline, and no later.
    #[tokio::test(start_paused = true)]
    async fn a_reconcile_whose_apiserver_never_answers_is_abandoned_at_the_deadline() {
        let ctx = Arc::new(Context {
            client: operator_core::testing::stalled_client(),
            metrics: Arc::new(Metrics::new()),
        });
        let started = tokio::time::Instant::now();

        // Bounded from outside as well, so a reconcile that is no longer cut
        // fails this test instead of hanging it.
        let outcome = tokio::time::timeout(
            RECONCILE_DEADLINE * 2,
            reconcile_with_deadline(claim_in_flight(), ctx),
        )
        .await
        .expect("the deadline must cut a reconcile whose apiserver never answers");

        assert_eq!(
            started.elapsed(),
            RECONCILE_DEADLINE + operator_core::deadline_event::PUBLISH_BOUND
        );
        match outcome {
            Err(ReconcileError::TimedOut(timed_out)) => {
                assert_eq!(timed_out.after, RECONCILE_DEADLINE)
            }
            other => panic!("expected ReconcileError::TimedOut, got {other:?}"),
        }
    }

    /// The deadline only exists if `run` hands kube-runtime the BOUNDED
    /// reconcile. Nothing else can see the run site — a `Controller` needs a
    /// live watch — so this reads it: everything above this module, which is
    /// the production code, must wire `reconcile_with_deadline`. The needle is
    /// assembled with `concat!` so this test's own text can never satisfy it.
    #[test]
    fn the_controller_runs_the_deadline_bounded_reconcile() {
        let production = include_str!("lib.rs")
            .split("#[cfg(test)]")
            .next()
            .expect("lib.rs has production code above its test module");
        let wired = concat!(
            ".run(reconcile_with_",
            "deadline, reconcile::error_policy, ctx)"
        );
        assert!(
            production.contains(wired),
            "run() must drive reconcile_with_deadline, not the unbounded reconcile::reconcile"
        );
    }
}
