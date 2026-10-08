// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The Warning Event a deadline-abandoned pass leaves on its object (WI-400).
//!
//! When [`crate::deadline::within`] abandons a reconcile, the only surfaces
//! that cannot hurt the object are a WARN, a metric and an Event. A status
//! write is ruled out for every controller that publishes this Event: each
//! owns its status under its own field manager with full-body server-side
//! apply, so a "timed out" body would PRUNE whatever it omits — a claim's
//! `provider` (the resourceclaim-scheduler) or its
//! `instance`/`dbnum`/`connectionSecretRef`, a volume's `refCount`, a
//! database's backing (the resourceclaim-provisioner) — and a `Ready=False`
//! would make a live claim provision again or refuse every new consumer of a
//! live database. An Event touches none of that, and it is what `kubectl
//! describe` shows beside the object.
//!
//! Every publisher sends the same Event, so a reader matches one contract:
//! `type: Warning`, `reason:` [`REASON`], `action: Reconcile`, `regarding`
//! the object (its `object_ref`, with its `uid`), and a note that starts
//! `<Kind> reconcile did not finish within <N>s:` — the Display of
//! [`ReconcileTimedOut`]. Only `reportingController` differs: it names the
//! controller whose pass was cut, which is how a reader tells a scheduler
//! timeout from a provisioner one on the same claim.
//!
//! Publishing is bounded too: the apiserver that stalled the pass may stall
//! this POST, and the controller's slot for the object stays held until the
//! wrapper that calls this returns.

use std::time::Duration;

use k8s_openapi::api::core::v1::ObjectReference;
use kube::runtime::events::{Event, EventType, Reporter};
use kube::Client;
use tracing::warn;

use crate::deadline::ReconcileTimedOut;
use crate::events::ObjectRecorder;

/// The Event reason. Stable: it is what `kubectl get events
/// --field-selector reason=ReconcileTimedOut` and a walk would match on.
pub const REASON: &str = "ReconcileTimedOut";

/// How long the publish may take before it is given up.
pub const PUBLISH_BOUND: Duration = Duration::from_secs(10);

/// The Event's note: what happened, and what did NOT.
fn note(kind: &str, timed_out: ReconcileTimedOut) -> String {
    format!(
        "{kind} {timed_out}: the controller abandoned this pass and will retry it; \
         nothing was written to this object's status"
    )
}

/// Publish the [`REASON`] Warning Event about `regarding`, reported by
/// `reporting_controller` (the controller whose pass was cut). Best-effort
/// and bounded by [`PUBLISH_BOUND`]: a failure or a stall is logged, never
/// returned.
pub async fn publish(
    client: &Client,
    reporting_controller: &str,
    regarding: ObjectReference,
    kind: &str,
    timed_out: ReconcileTimedOut,
) {
    let name = regarding.name.clone().unwrap_or_default();
    let namespace = regarding.namespace.clone().unwrap_or_default();
    let reporter = Reporter {
        controller: reporting_controller.into(),
        instance: std::env::var("POD_NAME").ok(),
    };
    let recorder = ObjectRecorder::new(client.clone(), reporter, regarding);
    let event = Event {
        type_: EventType::Warning,
        reason: REASON.into(),
        note: Some(note(kind, timed_out)),
        action: "Reconcile".into(),
        secondary: None,
    };
    match tokio::time::timeout(PUBLISH_BOUND, recorder.publish(event)).await {
        Ok(Ok(())) => {}
        Ok(Err(e)) => warn!(
            %kind, %name, %namespace, error = %e,
            "could not publish the ReconcileTimedOut event"
        ),
        Err(_) => warn!(
            %kind, %name, %namespace, bound_secs = PUBLISH_BOUND.as_secs(),
            "the ReconcileTimedOut event publish did not answer within its bound"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::tests::recording_apiserver;
    use serde_json::json;

    const EVENTS: &str = "/apis/events.k8s.io/v1/namespaces/apps/events";

    fn claim_reference() -> ObjectReference {
        ObjectReference {
            api_version: Some("apprafter.io/v1alpha1".into()),
            kind: Some("ResourceClaim".into()),
            name: Some("web-redis".into()),
            namespace: Some("apps".into()),
            uid: Some("u-1".into()),
            ..ObjectReference::default()
        }
    }

    const TIMED_OUT: ReconcileTimedOut = ReconcileTimedOut {
        after: Duration::from_secs(120),
    };

    #[tokio::test]
    async fn a_timed_out_pass_leaves_one_warning_event_on_its_object() {
        let (client, log) = recording_apiserver();

        publish(
            &client,
            "apprafter-resourceclaim-provisioner",
            claim_reference(),
            "ResourceClaim",
            TIMED_OUT,
        )
        .await;

        let log = log.lock().expect("log").clone();
        assert_eq!(log.len(), 1, "{log:#?}");
        assert_eq!(log[0].method, "POST");
        assert!(log[0].uri.starts_with(EVENTS), "{}", log[0].uri);
        let body = &log[0].body;
        assert_eq!(body["type"], json!("Warning"));
        assert_eq!(body["reason"], json!(REASON));
        assert_eq!(body["reason"], json!("ReconcileTimedOut"));
        assert_eq!(body["action"], json!("Reconcile"));
        assert_eq!(body["regarding"]["kind"], json!("ResourceClaim"));
        assert_eq!(body["regarding"]["name"], json!("web-redis"));
        assert_eq!(body["regarding"]["uid"], json!("u-1"));
        assert_eq!(
            body["note"],
            json!(
                "ResourceClaim reconcile did not finish within 120s: the controller abandoned \
                 this pass and will retry it; nothing was written to this object's status"
            )
        );
    }

    /// The reporter is the caller's: it is the one field that tells which
    /// controller's pass was cut, so it must not be fixed here.
    #[tokio::test]
    async fn the_event_names_the_controller_whose_pass_was_cut() {
        for controller in [
            "apprafter-resourceclaim-provisioner",
            "apprafter-resourceclaim-scheduler",
        ] {
            let (client, log) = recording_apiserver();
            publish(
                &client,
                controller,
                claim_reference(),
                "ResourceClaim",
                TIMED_OUT,
            )
            .await;
            let log = log.lock().expect("log").clone();
            assert_eq!(log.len(), 1, "{log:#?}");
            assert_eq!(log[0].body["reportingController"], json!(controller));
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_publish_that_never_answers_is_given_up_at_its_bound() {
        let started = tokio::time::Instant::now();
        tokio::time::timeout(
            PUBLISH_BOUND * 10,
            publish(
                &crate::testing::stalled_client(),
                "apprafter-resourceclaim-provisioner",
                claim_reference(),
                "ResourceClaim",
                TIMED_OUT,
            ),
        )
        .await
        .expect("the publish must give up on its own");
        assert_eq!(started.elapsed(), PUBLISH_BOUND);
    }
}
