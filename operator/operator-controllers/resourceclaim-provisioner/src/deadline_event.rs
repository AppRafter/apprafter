// SPDX-License-Identifier: FSL-1.1-Apache-2.0

//! The Warning Event a deadline-abandoned pass leaves on its object (WI-400).
//!
//! When `operator_core::deadline::within` abandons a reconcile, the only
//! surfaces that cannot hurt the object are a WARN, a metric and an Event.
//! A status write is ruled out: every controller in this crate owns its
//! status under [`crate::FIELD_MANAGER`] with full-body server-side apply,
//! so a "timed out" body would PRUNE whatever it omits — a claim's
//! `instance`/`dbnum`/`connectionSecretRef`, a volume's `refCount`, a
//! database's backing — and a `Ready=False` would make a live claim
//! provision again or refuse every new consumer of a live database. An
//! Event touches none of that, and it is what `kubectl describe` shows
//! beside the object.
//!
//! Publishing is bounded too: the apiserver that stalled the pass may stall
//! this POST, and the controller's only slot stays held until the wrapper
//! that calls this returns.

use std::time::Duration;

use k8s_openapi::api::core::v1::ObjectReference;
use kube::runtime::events::{Event, EventType, Reporter};
use kube::Client;
use operator_core::deadline::ReconcileTimedOut;
use operator_core::events::ObjectRecorder;
use tracing::warn;

/// The Event reason. Stable: it is what `kubectl get events
/// --field-selector reason=ReconcileTimedOut` and a walk would match on.
pub(crate) const REASON: &str = "ReconcileTimedOut";

/// How long the publish may take before it is given up.
pub(crate) const PUBLISH_BOUND: Duration = Duration::from_secs(10);

/// The reporter every Event from this crate carries — the same one the
/// SharedVolume `CapacityWarning` Event uses.
const REPORTER_CONTROLLER: &str = "apprafter-resourceclaim-provisioner";

/// The Event's note: what happened, and what did NOT.
pub(crate) fn note(kind: &str, timed_out: ReconcileTimedOut) -> String {
    format!(
        "{kind} {timed_out}: the controller abandoned this pass and will retry it; \
         nothing was written to this object's status"
    )
}

/// Publish the [`REASON`] Warning Event about `regarding`. Best-effort and
/// bounded by [`PUBLISH_BOUND`]: a failure or a stall is logged, never
/// returned.
pub(crate) async fn publish(
    client: &Client,
    regarding: ObjectReference,
    kind: &str,
    timed_out: ReconcileTimedOut,
) {
    let name = regarding.name.clone().unwrap_or_default();
    let namespace = regarding.namespace.clone().unwrap_or_default();
    let reporter = Reporter {
        controller: REPORTER_CONTROLLER.into(),
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
    use crate::route_apiserver::{apiserver, route, Reply};
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
        let (client, log) = apiserver(vec![route(
            "POST",
            EVENTS,
            Reply::Json(
                201,
                json!({
                    "apiVersion": "events.k8s.io/v1", "kind": "Event",
                    "metadata": { "name": "web-redis.1", "namespace": "apps" },
                }),
            ),
        )]);

        publish(&client, claim_reference(), "ResourceClaim", TIMED_OUT).await;

        let log = log.lock().expect("log").clone();
        assert_eq!(log.len(), 1, "{log:#?}");
        assert_eq!(log[0].method, "POST");
        assert_eq!(log[0].path, EVENTS);
        let body = &log[0].body;
        assert_eq!(body["type"], json!("Warning"));
        assert_eq!(body["reason"], json!("ReconcileTimedOut"));
        assert_eq!(body["action"], json!("Reconcile"));
        assert_eq!(body["regarding"]["kind"], json!("ResourceClaim"));
        assert_eq!(body["regarding"]["name"], json!("web-redis"));
        assert_eq!(
            body["note"],
            json!(
                "ResourceClaim reconcile did not finish within 120s: the controller abandoned \
                 this pass and will retry it; nothing was written to this object's status"
            )
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_publish_that_never_answers_is_given_up_at_its_bound() {
        let started = tokio::time::Instant::now();
        tokio::time::timeout(
            PUBLISH_BOUND * 10,
            publish(
                &operator_core::testing::stalled_client(),
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
