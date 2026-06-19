// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Reconcile loop for the `ResourceClaim` scheduler controller.
//!
//! Matches each claim to a `ServiceProvider` by `spec.type` + label
//! superset and records the winner in `status.provider` + a `Scheduled`
//! condition.  When no provider matches, emits a Kubernetes Warning Event
//! and increments the `apprafter_claim_unmatched_total` metric.
//!
//! Provisioning (2.4) reads `status.provider` — this controller never
//! touches `status.ready` or `status.connectionSecretRef`.

use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use k8s_openapi::api::core::v1::ObjectReference;
use kube::api::{Api, Patch, PatchParams};
use kube::runtime::controller::Action;
use kube::runtime::events::{Event as KubeEvent, EventType, Recorder, Reporter};
use kube::{Client, Resource, ResourceExt};
use serde_json::{json, Value};
use tracing::{info, warn};

use operator_core::{ResourceClaim, ResourceClaimCondition, ServiceProvider};

use crate::{Context, ReconcileError, FIELD_MANAGER, KIND};

/// Condition type written by this controller.
const COND_SCHEDULED: &str = "Scheduled";

/// Reporter identity stamped onto every Kubernetes Event this controller
/// publishes.  Visible in `kubectl describe resourceclaim <name>` under
/// the Events section.
const EVENT_REPORTER_CONTROLLER: &str = "apprafter-resourceclaim-scheduler";

// ---------------------------------------------------------------------------
// Public reconcile + error_policy
// ---------------------------------------------------------------------------

/// Reconcile a single `ResourceClaim`:
///
/// 1. List all `ServiceProvider` CRs cluster-wide.
/// 2. Run the pure `select_provider` matcher.
/// 3. On match   → SSA-patch `status.provider` + `Scheduled=True`.
/// 4. On no-match → SSA-patch `status.conditions[Scheduled=False]`,
///    emit a Warning Kubernetes Event, increment `claim_unmatched_total`.
pub async fn reconcile(
    claim: Arc<ResourceClaim>,
    ctx: Arc<Context>,
) -> Result<Action, ReconcileError> {
    let ns = claim.namespace().unwrap_or_default();
    let name = claim.name_any();
    let _timer = ctx
        .metrics
        .reconcile_duration
        .with_label_values(&[KIND])
        .start_timer();

    info!(%name, %ns, "reconciling ResourceClaim");

    // 1. List all ServiceProviders cluster-wide.
    let providers: Vec<ServiceProvider> = Api::<ServiceProvider>::all(ctx.client.clone())
        .list(&Default::default())
        .await?
        .items;

    // 2. Project into Candidate structs for the pure matcher.
    let candidates: Vec<operator_core::matching::Candidate> = providers
        .iter()
        .map(operator_core::matching::Candidate::from_provider)
        .collect();

    // 3. Run the matcher.
    let chosen = operator_core::matching::select_provider(
        &claim.spec.type_,
        &claim.spec.selector,
        &candidates,
    );

    // 4. Read prior conditions for the timestamp-preservation guard.
    let prior: Vec<ResourceClaimCondition> = claim
        .status
        .as_ref()
        .and_then(|s| s.conditions.clone())
        .unwrap_or_default();

    match chosen {
        Some(ref provider_name) => {
            // 5a. MATCH — build Scheduled=True condition and SSA-patch.
            let cond = condition(
                COND_SCHEDULED,
                "True",
                "MatchFound",
                &format!("matched ServiceProvider {provider_name}"),
                &prior,
            );
            let body = build_status_patch(&name, Some(provider_name), cond);
            let api: Api<ResourceClaim> = Api::namespaced(ctx.client.clone(), &ns);
            api.patch_status(
                &name,
                &PatchParams::apply(FIELD_MANAGER).force(),
                &Patch::Apply(&body),
            )
            .await?;

            info!(%name, %ns, provider = %provider_name, "ResourceClaim scheduled");
            ctx.metrics
                .reconcile_total
                .with_label_values(&[KIND, &ns, "ok"])
                .inc();
        }
        None => {
            // 5b. NO MATCH — build Scheduled=False, patch status, emit event.
            let message = format!(
                "no ServiceProvider matches type={:?} selector={:?}",
                claim.spec.type_, claim.spec.selector
            );
            let cond = condition(
                COND_SCHEDULED,
                "False",
                "NoMatchingProvider",
                &message,
                &prior,
            );
            // provider key OMITTED — build_status_patch(name, None, cond)
            let body = build_status_patch(&name, None, cond);
            let api: Api<ResourceClaim> = Api::namespaced(ctx.client.clone(), &ns);
            api.patch_status(
                &name,
                &PatchParams::apply(FIELD_MANAGER).force(),
                &Patch::Apply(&body),
            )
            .await?;

            // Emit a best-effort Kubernetes Warning Event.
            let recorder = build_recorder(&ctx.client, &claim);
            let ev = KubeEvent {
                type_: EventType::Warning,
                reason: "NoMatchingServiceProvider".into(),
                note: Some(message),
                action: "Schedule".into(),
                secondary: None,
            };
            if let Err(e) = recorder.publish(ev).await {
                warn!(error = %e, "failed to publish NoMatchingServiceProvider event (continuing)");
            }

            warn!(%name, %ns, "ResourceClaim: no matching ServiceProvider found");
            ctx.metrics
                .claim_unmatched_total
                .with_label_values(&[KIND, &ns, "no_matching_provider"])
                .inc();
            ctx.metrics
                .reconcile_total
                .with_label_values(&[KIND, &ns, "pending"])
                .inc();
        }
    }

    // Re-evaluate every 5 minutes so a Pending claim picks up a newly
    // created ServiceProvider without requiring a watch fan-out.
    Ok(Action::requeue(Duration::from_secs(300)))
}

/// Error policy: increment error metrics and requeue after 30 seconds.
pub fn error_policy(claim: Arc<ResourceClaim>, err: &ReconcileError, ctx: Arc<Context>) -> Action {
    let name = claim.name_any();
    let namespace = claim.namespace().unwrap_or_default();
    warn!(%name, %namespace, %err, "resourceclaim reconcile error");
    ctx.metrics
        .reconcile_total
        .with_label_values(&[KIND, &namespace, "error"])
        .inc();
    ctx.metrics
        .reconcile_errors
        .with_label_values(&[KIND])
        .inc();
    Action::requeue(Duration::from_secs(30))
}

// ---------------------------------------------------------------------------
// Pure helpers (unit-tested without a cluster)
// ---------------------------------------------------------------------------

/// Build a `Recorder` that publishes events against the given
/// `ResourceClaim`.  Constructing per-reconcile keeps the reconcile
/// function pure; `Recorder::new` is cheap.
fn build_recorder(client: &Client, claim: &ResourceClaim) -> Recorder {
    let reporter = Reporter {
        controller: EVENT_REPORTER_CONTROLLER.into(),
        instance: std::env::var("POD_NAME").ok(),
    };
    let reference: ObjectReference = claim.object_ref(&());
    Recorder::new(client.clone(), reporter, reference)
}

/// Build a `ResourceClaimCondition`, preserving `lastTransitionTime`
/// when the `(type, status)` pair is unchanged (hot-loop guard: same
/// `(type, status)` ⇒ byte-equal condition ⇒ no-op SSA patch ⇒ no
/// self-triggered re-reconcile).
fn condition(
    type_: &str,
    status: &str,
    reason: &str,
    message: &str,
    previous: &[ResourceClaimCondition],
) -> ResourceClaimCondition {
    let last_transition_time = previous
        .iter()
        .find(|c| c.type_ == type_ && c.status == status)
        .map(|c| c.last_transition_time.clone())
        .unwrap_or_else(|| Utc::now().to_rfc3339());
    ResourceClaimCondition {
        type_: type_.to_string(),
        status: status.to_string(),
        last_transition_time,
        reason: Some(reason.to_string()),
        message: Some(message.to_string()),
    }
}

/// Build the SSA patch body for `ResourceClaim` status.
///
/// When `provider` is `Some` (match found), the body includes
/// `status.provider`.  When `None` (no match), the `provider` key is
/// absent so 2.4 — which sets `provider` once provisioning is wired —
/// is never inadvertently cleared by a scheduler no-match pass.
///
/// The body **never** includes `status.ready` or
/// `status.connectionSecretRef`; those are owned by the 2.4
/// provisioner.
fn build_status_patch(name: &str, provider: Option<&str>, cond: ResourceClaimCondition) -> Value {
    let status = match provider {
        Some(p) => json!({
            "provider": p,
            "conditions": [cond],
        }),
        None => json!({
            "conditions": [cond],
        }),
    };
    json!({
        "apiVersion": "apprafter.io/v1alpha1",
        "kind": "ResourceClaim",
        "metadata": { "name": name },
        "status": status,
    })
}

// ---------------------------------------------------------------------------
// Unit tests — pure helpers only, no kube client
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn prev_cond(type_: &str, status: &str, ts: &str) -> ResourceClaimCondition {
        ResourceClaimCondition {
            type_: type_.to_string(),
            status: status.to_string(),
            last_transition_time: ts.to_string(),
            reason: Some("Reason".to_string()),
            message: Some("msg".to_string()),
        }
    }

    // --- condition() timestamp guard ---

    #[test]
    fn condition_reuses_timestamp_when_status_unchanged() {
        let ts = "2026-01-01T00:00:00+00:00";
        let prev = vec![prev_cond(COND_SCHEDULED, "True", ts)];
        let c = condition(COND_SCHEDULED, "True", "MatchFound", "msg", &prev);
        assert_eq!(c.last_transition_time, ts);
    }

    #[test]
    fn condition_bumps_timestamp_when_status_changes() {
        let ts = "2026-01-01T00:00:00+00:00";
        let prev = vec![prev_cond(COND_SCHEDULED, "False", ts)];
        let c = condition(COND_SCHEDULED, "True", "MatchFound", "msg", &prev);
        assert_ne!(c.last_transition_time, ts);
    }

    // --- build_status_patch() shape assertions ---

    #[test]
    fn build_status_patch_includes_provider_and_condition_on_match() {
        let cond = condition(COND_SCHEDULED, "True", "MatchFound", "matched pg-a", &[]);
        let patch = build_status_patch("my-claim", Some("pg-integrated"), cond);

        // Required SSA fields.
        assert_eq!(
            patch.pointer("/apiVersion").and_then(Value::as_str),
            Some("apprafter.io/v1alpha1")
        );
        assert_eq!(
            patch.pointer("/kind").and_then(Value::as_str),
            Some("ResourceClaim")
        );
        assert_eq!(
            patch.pointer("/metadata/name").and_then(Value::as_str),
            Some("my-claim")
        );

        // Provider must be present.
        assert_eq!(
            patch.pointer("/status/provider").and_then(Value::as_str),
            Some("pg-integrated")
        );

        // Scheduled=True condition must be present.
        let cond_type = patch
            .pointer("/status/conditions/0/type")
            .and_then(Value::as_str);
        assert_eq!(cond_type, Some(COND_SCHEDULED));
        let cond_status = patch
            .pointer("/status/conditions/0/status")
            .and_then(Value::as_str);
        assert_eq!(cond_status, Some("True"));

        // Must NOT include ready or connectionSecretRef (owned by 2.4).
        assert!(patch.pointer("/status/ready").is_none());
        assert!(patch.pointer("/status/connectionSecretRef").is_none());
    }

    #[test]
    fn build_status_patch_omits_provider_on_no_match() {
        let cond = condition(
            COND_SCHEDULED,
            "False",
            "NoMatchingProvider",
            "no match",
            &[],
        );
        let patch = build_status_patch("my-claim", None, cond);

        // Provider key must be absent.
        assert!(patch.pointer("/status/provider").is_none());

        // Scheduled=False condition must be present.
        let cond_type = patch
            .pointer("/status/conditions/0/type")
            .and_then(Value::as_str);
        assert_eq!(cond_type, Some(COND_SCHEDULED));
        let cond_status = patch
            .pointer("/status/conditions/0/status")
            .and_then(Value::as_str);
        assert_eq!(cond_status, Some("False"));

        // Must NOT include ready or connectionSecretRef (owned by 2.4).
        assert!(patch.pointer("/status/ready").is_none());
        assert!(patch.pointer("/status/connectionSecretRef").is_none());
    }
}
