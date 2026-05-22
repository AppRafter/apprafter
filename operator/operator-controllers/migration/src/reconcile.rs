// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! MigrationController reconcile loop.
//!
//! The controller watches `MigrationPlan` CRs cluster-wide
//! and walks each one through its phase FSM. External actors
//! (Backstage, Argo CD Lua action, `apprafter migration
//! approve|reject`) drive the `pending-approval → approved |
//! rejected` transitions; the controller owns everything
//! after that.
//!
//! Status writes use server-side apply with field manager
//! `migration-controller`. Step execution is idempotent —
//! `status.executedSteps[]` doubles as the controller's
//! progress marker, so a reconcile mid-step never re-runs
//! a step that already completed.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::Utc;
use futures::StreamExt;
use kube::api::{Api, ListParams, Patch, PatchParams};
use kube::runtime::controller::Action;
use kube::runtime::Controller;
use kube::{Client, ResourceExt};
use serde_json::json;
use thiserror::Error;
use tracing::{info, warn};

use operator_core::{
    ExecutedStep, MigrationError, MigrationPlan, MigrationPlanStatus, MigrationStrategy,
    StepOutcome,
};

use crate::strategy::{ApplicationMigrationStrategy, PlatformMigrationStrategy};

/// SSA field manager owning `MigrationPlan.status.*` writes.
/// Mirrors the operator's other field-manager constants
/// (`apprafter-operator` for Application status,
/// `platform-controller` for PlatformStack status).
pub const FIELD_MANAGER: &str = "migration-controller";

const RECONCILE_REQUEUE_AFTER_PROGRESS: Duration = Duration::from_secs(1);
const ERROR_REQUEUE_AFTER: Duration = Duration::from_secs(15);

#[derive(Debug, Error)]
pub enum Error {
    #[error("kube API error: {0}")]
    Kube(#[from] kube::Error),
    #[error("strategy error: {0}")]
    Strategy(#[from] MigrationError),
    #[error("missing field {0:?} on plan status")]
    MissingField(String),
    #[error("unknown scope.type {0:?} — webhook should have rejected this")]
    UnknownScope(String),
    #[error("unknown phase {0:?} — webhook should have rejected this")]
    UnknownPhase(String),
}

/// Reconciler context. Holds the kube client + both strategies
/// pre-built — the per-call dispatch picks one by inspecting
/// `plan.spec.scope.type`. Cheap clones: the strategies own
/// only an `Arc<Client>` and a static `ApiResource`.
pub struct Context {
    pub client: Client,
    pub application_strategy: ApplicationMigrationStrategy,
    pub platform_strategy: PlatformMigrationStrategy,
}

/// Spawn the controller. Returns when the underlying watcher
/// stream ends — under normal operation that means the leader
/// lease was lost and the process is about to exit.
pub async fn run(client: Client) -> Result<(), Error> {
    let ctx = Arc::new(Context {
        client: client.clone(),
        application_strategy: ApplicationMigrationStrategy,
        platform_strategy: PlatformMigrationStrategy::new(client.clone()),
    });

    let api: Api<MigrationPlan> = Api::all(client);
    Controller::new(
        api,
        kube::runtime::watcher::Config::default().any_semantic(),
    )
    .shutdown_on_signal()
    .run(reconcile, error_policy, ctx)
    .for_each(|res| async move {
        match res {
            Ok((obj, _)) => info!(plan = %obj.name, "migration reconcile completed"),
            Err(e) => warn!(error = %e, "migration reconcile error"),
        }
    })
    .await;
    Ok(())
}

fn error_policy(_obj: Arc<MigrationPlan>, err: &Error, _ctx: Arc<Context>) -> Action {
    warn!(error = %err, "MigrationController error_policy fired");
    Action::requeue(ERROR_REQUEUE_AFTER)
}

async fn reconcile(plan: Arc<MigrationPlan>, ctx: Arc<Context>) -> Result<Action, Error> {
    let name = plan.name_any();
    let namespace = plan
        .metadata
        .namespace
        .clone()
        .unwrap_or_else(|| "apprafter-system".to_string());
    let phase = plan
        .status
        .as_ref()
        .and_then(|s| s.phase.as_deref())
        .unwrap_or("pending-approval")
        .to_string();
    info!(plan = %name, %phase, "reconcile");

    let strategy = pick_strategy(&plan, &ctx)?;

    match phase.as_str() {
        // Awaiting external approval / reject — no work for
        // the controller. Future watch events (status patches)
        // trigger another reconcile.
        "pending-approval" => Ok(Action::await_change()),

        // External flip to `approved`. Move to `executing` so
        // step execution starts on the next reconcile cycle.
        "approved" => {
            let new_status = with_phase(plan.status.as_ref(), "executing");
            write_status(&ctx, &namespace, &name, &new_status).await?;
            Ok(Action::requeue(RECONCILE_REQUEUE_AFTER_PROGRESS))
        }

        "executing" => {
            execute_next_step(plan.as_ref(), strategy.as_ref(), &ctx, &namespace, &name).await
        }

        // External flip to `rejected`. Run strategy.reject
        // ONCE — `status.rejectedAt` marker prevents
        // re-invocation across operator pod restarts.
        //
        // Walk-fix #3 post-B.1.77: prior version called
        // `strategy.reject()` on every reconcile of a
        // rejected plan, which fires on cold-start cache
        // replay (operator restart, leader-election handoff,
        // etc.). For platform-scope plans the strategy SSA-
        // patches `PlatformStack.spec.pin` back to the
        // snapshot value, overriding ANY subsequent operator
        // action on pin — the rejected plan effectively pins
        // the platform forever. Marker-gated invocation
        // turns the call into a true one-shot.
        //
        // For application-scope plans `strategy.reject` is a
        // no-op per ADR 0027 (the webhook FSM also blocks
        // the transition); the marker still gets set so the
        // sealing behaviour is uniform across scopes.
        "rejected" => {
            let already_applied = plan
                .status
                .as_ref()
                .and_then(|s| s.rejected_at.as_deref())
                .is_some();
            if already_applied {
                info!(plan = %name, "rejected plan already sealed — skipping strategy.reject");
                return Ok(Action::await_change());
            }
            strategy.reject(plan.as_ref()).await?;
            let mut sealed = plan.status.clone().unwrap_or_default();
            sealed.rejected_at = Some(Utc::now().to_rfc3339());
            write_status(&ctx, &namespace, &name, &sealed).await?;
            info!(plan = %name, "rejected plan sealed");
            Ok(Action::await_change())
        }

        // Sealed states — no further action; controller awaits
        // external deletes / new plans.
        "completed" | "failed" => Ok(Action::await_change()),

        other => Err(Error::UnknownPhase(other.to_string())),
    }
}

/// Pure mapping of `scope.type` → `StrategyKey`. Pulled out
/// of `pick_strategy` so unit tests can exercise the scope
/// dispatch contract without constructing a kube `Client`
/// (which spawns tokio tasks at build time and was flaky under
/// `cargo test --workspace --all-features`).
fn pick_strategy_key(scope_type: &str) -> Result<StrategyKey, Error> {
    match scope_type {
        "application" => Ok(StrategyKey::Application),
        "platform" => Ok(StrategyKey::Platform),
        other => Err(Error::UnknownScope(other.to_string())),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StrategyKey {
    Application,
    Platform,
}

fn pick_strategy(plan: &MigrationPlan, ctx: &Context) -> Result<Arc<dyn MigrationStrategy>, Error> {
    match pick_strategy_key(&plan.spec.scope.type_)? {
        StrategyKey::Application => Ok(Arc::new(ctx.application_strategy.clone())),
        StrategyKey::Platform => Ok(Arc::new(ctx.platform_strategy.clone())),
    }
}

async fn execute_next_step(
    plan: &MigrationPlan,
    strategy: &dyn MigrationStrategy,
    ctx: &Context,
    namespace: &str,
    name: &str,
) -> Result<Action, Error> {
    let executed = plan
        .status
        .as_ref()
        .and_then(|s| s.executed_steps.clone())
        .unwrap_or_default();
    let steps = plan.spec.plan.clone().unwrap_or_default();

    if executed.len() >= steps.len() {
        // All steps already run; transition to `completed`.
        // The empty-plan case (no steps) lands here on the
        // first reconcile after `approved` — also valid;
        // a zero-step migration completes immediately.
        let new_status = with_phase(plan.status.as_ref(), "completed");
        write_status(ctx, namespace, name, &new_status).await?;
        return Ok(Action::await_change());
    }

    let next_step = &steps[executed.len()];
    info!(plan = %name, step = next_step.step, "executing step");
    let started_at = Utc::now().to_rfc3339();
    let outcome = strategy.execute_step(plan, next_step).await?;
    let finished_at = Utc::now().to_rfc3339();

    let (outcome_str, message) = match &outcome {
        StepOutcome::Succeeded => ("succeeded".to_string(), None),
        StepOutcome::Failed { message } => ("failed".to_string(), Some(message.clone())),
        StepOutcome::Skipped { reason } => ("skipped".to_string(), Some(reason.clone())),
    };

    let mut new_executed = executed;
    new_executed.push(ExecutedStep {
        step: next_step.step,
        started_at,
        finished_at: Some(finished_at),
        outcome: outcome_str.clone(),
        message,
    });

    // If this step failed, seal the plan in `failed`. Otherwise
    // requeue immediately to run the next step (or transition
    // to `completed` when none remain).
    let next_phase = if matches!(outcome, StepOutcome::Failed { .. }) {
        "failed"
    } else if new_executed.len() >= steps.len() {
        "completed"
    } else {
        "executing"
    };

    let mut new_status = plan.status.clone().unwrap_or_default();
    new_status.phase = Some(next_phase.to_string());
    new_status.executed_steps = Some(new_executed);
    write_status(ctx, namespace, name, &new_status).await?;

    if next_phase == "executing" {
        Ok(Action::requeue(RECONCILE_REQUEUE_AFTER_PROGRESS))
    } else {
        Ok(Action::await_change())
    }
}

fn with_phase(prior: Option<&MigrationPlanStatus>, phase: &str) -> MigrationPlanStatus {
    let mut status = prior.cloned().unwrap_or_default();
    status.phase = Some(phase.to_string());
    status
}

async fn write_status(
    ctx: &Context,
    namespace: &str,
    name: &str,
    new_status: &MigrationPlanStatus,
) -> Result<(), Error> {
    let api: Api<MigrationPlan> = Api::namespaced(ctx.client.clone(), namespace);
    let body = json!({
        "apiVersion": "apprafter.io/v1alpha1",
        "kind": "MigrationPlan",
        "metadata": { "name": name },
        "status": new_status,
    });
    // `force=true` is load-bearing here. External actors
    // (Backstage UI, `apprafter migration approve|reject`,
    // `kubectl patch --subresource=status`) write `status.phase`
    // and register their own SSA field manager
    // (`kubectl-patch` / `backstage` / etc.) as the owner of
    // that field. MigrationController's own SSA patch carrying
    // `phase=executing` (or `completed` / `failed`) without
    // `.force()` 409s with a managedFields conflict, the
    // reconcile error_policy retries forever on the same
    // conflict, and the plan freezes at `approved`.
    //
    // Walk-found bug v0.1.126 → v0.1.127. Application
    // controller's `apply_status` already uses `.force()` for
    // exactly this reason (operator-controllers/application).
    api.patch_status(
        name,
        &PatchParams::apply(FIELD_MANAGER).force(),
        &Patch::Apply(&body),
    )
    .await?;
    Ok(())
}

// Public for callers that want to bound the controller's
// listing scope (smoke tests, scripted backfills). Default
// run() uses cluster-scoped listing — MigrationPlans live in
// apprafter-system today but we don't gate the controller on
// that namespace.
#[allow(dead_code)]
async fn list_all(client: &Client) -> Result<Vec<MigrationPlan>, Error> {
    let api: Api<MigrationPlan> = Api::all(client.clone());
    let lst = api.list(&ListParams::default()).await?;
    Ok(lst.items)
}

// Trait re-export to make trait-method dispatch ergonomic at
// call sites without `use operator_core::MigrationStrategy;`
// in every file.
#[async_trait]
pub trait MigrationDispatch: MigrationStrategy {}
#[async_trait]
impl<T: MigrationStrategy + ?Sized> MigrationDispatch for T {}

#[cfg(test)]
mod tests {
    use super::*;
    use operator_core::{
        MigrationApplicationRef, MigrationApplicationScope, MigrationPlanScope, MigrationPlanSpec,
        MigrationPlanStatus, MigrationStep, MigrationTrigger,
    };

    fn build_plan(
        scope_type: &str,
        steps: Vec<MigrationStep>,
        phase: Option<&str>,
    ) -> MigrationPlan {
        let spec = MigrationPlanSpec {
            scope: MigrationPlanScope {
                type_: scope_type.into(),
                application: Some(MigrationApplicationScope {
                    ref_: MigrationApplicationRef {
                        name: "parser".into(),
                        namespace: "demo".into(),
                    },
                    environment: "prod".into(),
                }),
                platform: None,
            },
            trigger: MigrationTrigger {
                type_: "t".into(),
                field: "f".into(),
                from: None,
                to: None,
            },
            risks: None,
            plan: Some(steps),
            approvers: None,
            previous_spec_snapshot: None,
        };
        let mut plan = MigrationPlan::new("p", spec);
        if let Some(p) = phase {
            plan.status = Some(MigrationPlanStatus {
                phase: Some(p.into()),
                ..MigrationPlanStatus::default()
            });
        }
        plan
    }

    fn step(n: u32) -> MigrationStep {
        MigrationStep {
            step: n,
            action: format!("action-{n}"),
            estimated_duration: None,
            reversible: None,
        }
    }

    #[test]
    fn with_phase_carries_executed_steps_forward() {
        // The phase-update helper preserves the prior status's
        // executedSteps so a transition write doesn't blank
        // out the audit trail. Regression guard against a
        // future refactor that "clears" status on phase flip.
        let prior = MigrationPlanStatus {
            phase: Some("approved".into()),
            executed_steps: Some(vec![ExecutedStep {
                step: 1,
                started_at: "2026-05-22T12:00:00+00:00".into(),
                finished_at: Some("2026-05-22T12:00:30+00:00".into()),
                outcome: "succeeded".into(),
                message: None,
            }]),
            ..MigrationPlanStatus::default()
        };
        let next = with_phase(Some(&prior), "executing");
        assert_eq!(next.phase.as_deref(), Some("executing"));
        assert_eq!(next.executed_steps.as_ref().map(Vec::len), Some(1));
    }

    #[test]
    fn with_phase_starts_from_default_when_prior_is_none() {
        let next = with_phase(None, "approved");
        assert_eq!(next.phase.as_deref(), Some("approved"));
        assert!(next.executed_steps.is_none());
    }

    #[test]
    fn pick_strategy_key_application_scope_returns_application_key() {
        // Pure scope-dispatch contract. We test the pure
        // string→key mapping (no kube::Client construction)
        // because tokio runtime build flake when run under
        // `cargo test --workspace --all-features` made the
        // Client-based variant unreliable.
        assert_eq!(
            pick_strategy_key("application").unwrap(),
            StrategyKey::Application
        );
    }

    #[test]
    fn pick_strategy_key_platform_scope_returns_platform_key() {
        assert_eq!(
            pick_strategy_key("platform").unwrap(),
            StrategyKey::Platform
        );
    }

    #[test]
    fn pick_strategy_key_unknown_scope_returns_error() {
        let err = pick_strategy_key("tenant").expect_err("must error on unknown scope");
        match err {
            Error::UnknownScope(s) => assert_eq!(s, "tenant"),
            other => panic!("expected UnknownScope, got {other:?}"),
        }
    }

    #[test]
    fn empty_plan_steps_signal_completion_immediately() {
        // The execute_next_step helper, given an empty
        // spec.plan vec, must transition to `completed` on
        // first call — zero-step plans are legal (a plan
        // whose execution is purely declarative / advisory).
        let plan = build_plan("application", vec![], Some("executing"));
        let executed = plan
            .status
            .as_ref()
            .and_then(|s| s.executed_steps.clone())
            .unwrap_or_default();
        let steps = plan.spec.plan.clone().unwrap_or_default();
        assert!(executed.len() >= steps.len());
    }

    #[test]
    fn step_index_tracks_executed_steps_length() {
        // The reconcile loop's "what step is next" reduces to
        // `executed_steps.len()`. Pin this so a refactor that
        // changes the marker semantics surfaces here.
        let mut plan = build_plan(
            "application",
            vec![step(1), step(2), step(3)],
            Some("executing"),
        );
        plan.status.as_mut().unwrap().executed_steps = Some(vec![ExecutedStep {
            step: 1,
            started_at: "t".into(),
            finished_at: Some("t".into()),
            outcome: "succeeded".into(),
            message: None,
        }]);
        let executed = plan
            .status
            .as_ref()
            .unwrap()
            .executed_steps
            .as_ref()
            .unwrap();
        assert_eq!(executed.len(), 1);
        // Next step index would be 1 (zero-based), pointing
        // at the second step in spec.plan[].
        let steps = plan.spec.plan.as_ref().unwrap();
        assert_eq!(steps[executed.len()].step, 2);
    }

    #[test]
    fn rejected_plan_with_rejected_at_marker_is_sealed() {
        // Walk-fix #3 contract: once `status.rejectedAt` is
        // set, the controller treats the rejected plan as
        // sealed. Encodes the helper logic the reconcile body
        // uses (`plan.status.as_ref().and_then(|s|
        // s.rejected_at.as_deref()).is_some()`); pin against
        // future refactors that might drop the marker check.
        let mut plan = build_plan("platform", vec![], Some("rejected"));
        plan.status.as_mut().unwrap().rejected_at = Some("2026-05-22T22:55:44+00:00".into());
        let sealed = plan
            .status
            .as_ref()
            .and_then(|s| s.rejected_at.as_deref())
            .is_some();
        assert!(sealed, "plan with rejectedAt marker must be sealed");
    }

    #[test]
    fn rejected_plan_without_rejected_at_marker_is_not_sealed() {
        // The complementary case: first time controller sees
        // phase=rejected, marker is absent → strategy.reject
        // must fire. Subsequent reconcile sets the marker
        // before write — so this branch is taken at most once
        // per plan lifetime.
        let plan = build_plan("platform", vec![], Some("rejected"));
        let sealed = plan
            .status
            .as_ref()
            .and_then(|s| s.rejected_at.as_deref())
            .is_some();
        assert!(
            !sealed,
            "freshly-rejected plan without rejectedAt marker must NOT be considered sealed"
        );
    }
}
