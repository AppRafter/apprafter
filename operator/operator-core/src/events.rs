// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Kubernetes Events bound to one object, the way every controller here
//! publishes them.
//!
//! Up to kube 0.97 a `Recorder` was constructed FOR an object
//! (`Recorder::new(client, reporter, reference)`) and `publish(event)` always
//! CREATEd a new `events.k8s.io/v1` Event. From kube 0.98 the `Recorder` is
//! object-agnostic (`publish(&event, &reference)`) and keeps a cache: a second
//! publish of the same (type, reason, action, reporter, regarding, related)
//! within six minutes on the SAME `Recorder` becomes a merge-PATCH that bumps
//! `series.count` instead of a new Event.
//!
//! Every controller builds a fresh recorder per publish (see each
//! `build_recorder`), so that cache is always empty and every publish is
//! still a CREATE — the pre-0.98 behaviour. [`ObjectRecorder`] keeps the
//! bound-to-one-object call shape so that fact stays visible at the call
//! sites, and the tests below pin it.
//!
//! One thing the library changed that no call shape can preserve: the Event's
//! NAME. Before 0.98 it was server-generated from `generateName:
//! "<reporting controller>-"`; now it is client-chosen, `<regarding
//! name>.<nanoseconds since epoch, hex>` — the same scheme client-go's
//! recorder uses. Nothing in this repository addresses an Event by name.

use k8s_openapi::api::core::v1::ObjectReference;
use kube::runtime::events::{Event, Recorder, Reporter};
use kube::Client;

/// A kube-runtime [`Recorder`] bound to the one object its Events regard.
#[derive(Clone)]
pub struct ObjectRecorder {
    inner: Recorder,
    reference: ObjectReference,
}

impl ObjectRecorder {
    /// A recorder for Events about `reference`. Build one per publish (or
    /// per reconcile) — see the module docs for why that matters.
    #[must_use]
    pub fn new(client: Client, reporter: Reporter, reference: ObjectReference) -> Self {
        Self {
            inner: Recorder::new(client, reporter),
            reference,
        }
    }

    /// Publish `ev` about the bound object. The Event lands in the object's
    /// namespace (every object this operator publishes about is namespaced).
    pub async fn publish(&self, ev: Event) -> Result<(), kube::Error> {
        self.inner.publish(&ev, &self.reference).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kube::client::Body;
    use kube::runtime::events::EventType;
    use serde_json::{json, Value};
    use std::sync::{Arc, Mutex};

    #[derive(Clone, Debug)]
    struct Call {
        method: String,
        uri: String,
        body: Value,
    }

    /// A `Client` that answers every request 201 with the posted body and
    /// logs what it was asked.
    fn recording_apiserver() -> (Client, Arc<Mutex<Vec<Call>>>) {
        let log = Arc::new(Mutex::new(Vec::<Call>::new()));
        let sink = log.clone();
        let service = tower::service_fn(move |req: http::Request<Body>| {
            let sink = sink.clone();
            async move {
                let method = req.method().to_string();
                let uri = req.uri().to_string();
                let bytes = req.into_body().collect_bytes().await.expect("request body");
                let body: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
                sink.lock().expect("log").push(Call {
                    method,
                    uri,
                    body: body.clone(),
                });
                Ok::<_, std::convert::Infallible>(
                    http::Response::builder()
                        .status(201)
                        .header("content-type", "application/json")
                        .body(Body::from(serde_json::to_vec(&body).expect("echo")))
                        .expect("response"),
                )
            }
        });
        (Client::new(service, "default"), log)
    }

    fn app_reference() -> ObjectReference {
        ObjectReference {
            api_version: Some("apprafter.io/v1alpha1".into()),
            kind: Some("Application".into()),
            name: Some("web".into()),
            namespace: Some("shop".into()),
            uid: Some("0b8f7d1e-0000-4000-8000-000000000001".into()),
            ..ObjectReference::default()
        }
    }

    fn reporter() -> Reporter {
        Reporter {
            controller: "apprafter-application-controller".into(),
            instance: Some("apprafter-operator-7d9f-abcde".into()),
        }
    }

    fn event() -> Event {
        Event {
            type_: EventType::Warning,
            reason: "SoftDestructiveChange".into(),
            note: Some("replicas 3 -> 1".into()),
            action: "Reconcile".into(),
            secondary: None,
        }
    }

    /// `metav1.MicroTime` on the wire: exactly six fractional digits and `Z`.
    fn is_rfc3339_micro(s: &str) -> bool {
        let b = s.as_bytes();
        b.len() == 27
            && b.iter().enumerate().all(|(i, c)| match i {
                4 | 7 => *c == b'-',
                10 => *c == b'T',
                13 | 16 => *c == b':',
                19 => *c == b'.',
                26 => *c == b'Z',
                _ => c.is_ascii_digit(),
            })
    }

    #[test]
    fn the_micro_time_shape_check_rejects_what_the_apiserver_rejects() {
        assert!(is_rfc3339_micro("2026-09-22T17:53:31.000000Z"));
        assert!(!is_rfc3339_micro("2026-09-22T17:53:31Z"));
        assert!(!is_rfc3339_micro("2026-09-22T17:53:31.123Z"));
        assert!(!is_rfc3339_micro("2026-09-22T17:53:31.123456+00:00"));
    }

    /// One publish is one CREATE of an events.k8s.io/v1 Event in the
    /// object's namespace, carrying the reporter, the regarding object and a
    /// MicroTime `eventTime` the apiserver's RFC3339Micro parse accepts.
    #[tokio::test]
    async fn a_publish_creates_one_event_in_the_objects_namespace() {
        let (client, log) = recording_apiserver();
        ObjectRecorder::new(client, reporter(), app_reference())
            .publish(event())
            .await
            .expect("publish");

        let calls = log.lock().expect("log").clone();
        assert_eq!(calls.len(), 1, "{calls:?}");
        assert_eq!(calls[0].method, "POST");
        assert!(
            calls[0]
                .uri
                .starts_with("/apis/events.k8s.io/v1/namespaces/shop/events"),
            "{}",
            calls[0].uri
        );
        let body = &calls[0].body;
        assert_eq!(body["type"], json!("Warning"));
        assert_eq!(body["reason"], json!("SoftDestructiveChange"));
        assert_eq!(body["action"], json!("Reconcile"));
        assert_eq!(body["note"], json!("replicas 3 -> 1"));
        assert_eq!(
            body["reportingController"],
            json!("apprafter-application-controller")
        );
        assert_eq!(
            body["reportingInstance"],
            json!("apprafter-operator-7d9f-abcde")
        );
        assert_eq!(body["regarding"]["name"], json!("web"));
        assert_eq!(body["regarding"]["kind"], json!("Application"));
        assert_eq!(body["metadata"]["namespace"], json!("shop"));
        assert!(
            body.get("series").is_none(),
            "a first publish carries no series"
        );
        let event_time = body["eventTime"].as_str().expect("eventTime is a string");
        assert!(is_rfc3339_micro(event_time), "eventTime {event_time:?}");
        // The kube >= 0.98 client-chosen name: `<regarding>.<hex nanos>`.
        let name = body["metadata"]["name"]
            .as_str()
            .expect("a client-chosen name");
        let suffix = name
            .strip_prefix("web.")
            .expect("named after the regarding object");
        assert!(
            !suffix.is_empty() && suffix.chars().all(|c| c.is_ascii_hexdigit()),
            "{name}"
        );
    }

    /// The property every controller relies on: a FRESH recorder per
    /// publish means the same Event twice is two CREATEs, never a
    /// series PATCH — the pre-kube-0.98 behaviour.
    #[tokio::test]
    async fn a_fresh_recorder_per_publish_never_aggregates() {
        let (client, log) = recording_apiserver();
        for _ in 0..2 {
            ObjectRecorder::new(client.clone(), reporter(), app_reference())
                .publish(event())
                .await
                .expect("publish");
        }
        let methods: Vec<String> = log
            .lock()
            .expect("log")
            .iter()
            .map(|c| c.method.clone())
            .collect();
        assert_eq!(methods, vec!["POST", "POST"]);
    }

    /// …and the library behaviour that property sidesteps, pinned so a
    /// refactor that starts reusing one recorder sees the change: the second
    /// identical publish on the SAME recorder is a merge-PATCH with a series.
    #[tokio::test]
    async fn one_reused_recorder_turns_a_repeat_into_a_series_patch() {
        let (client, log) = recording_apiserver();
        let recorder = ObjectRecorder::new(client, reporter(), app_reference());
        recorder.publish(event()).await.expect("first");
        recorder.publish(event()).await.expect("second");
        let calls = log.lock().expect("log").clone();
        assert_eq!(calls.len(), 2, "{calls:?}");
        assert_eq!(calls[0].method, "POST");
        assert_eq!(calls[1].method, "PATCH");
        assert_eq!(calls[1].body["series"]["count"], json!(2));
        let observed = calls[1].body["series"]["lastObservedTime"]
            .as_str()
            .expect("lastObservedTime");
        assert!(is_rfc3339_micro(observed), "{observed:?}");
    }
}
