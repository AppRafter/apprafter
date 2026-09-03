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

use operator_core::matching::Candidate;
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

    // 3. Decide — matcher + condition + patch body, all pure.
    let decision = decide(&name, &claim, &candidates);

    // 4. Carry the decision out: one status patch either way.
    let api: Api<ResourceClaim> = Api::namespaced(ctx.client.clone(), &ns);
    api.patch_status(
        &name,
        &PatchParams::apply(FIELD_MANAGER).force(),
        &Patch::Apply(&decision.patch),
    )
    .await?;

    match (&decision.provider, &decision.warning) {
        (Some(provider_name), _) => {
            info!(%name, %ns, provider = %provider_name, "ResourceClaim scheduled");
            ctx.metrics
                .reconcile_total
                .with_label_values(&[KIND, &ns, "ok"])
                .inc();
        }
        (None, warning) => {
            // Emit a best-effort Kubernetes Warning Event.
            let recorder = build_recorder(&ctx.client, &claim);
            let ev = KubeEvent {
                type_: EventType::Warning,
                reason: "NoMatchingServiceProvider".into(),
                note: warning.clone(),
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

/// Everything one reconcile decides about a claim, before any API call.
struct Decision {
    /// The winning ServiceProvider, or `None` when nothing matched.
    provider: Option<String>,
    /// The SSA body to send to `patch_status` — the only status write this
    /// controller makes, on either path.
    patch: Value,
    /// The note for the Warning Event, present only on the no-match path.
    /// It is the same sentence the `Scheduled=False` condition carries, so
    /// `kubectl describe` and the condition cannot disagree about why a
    /// claim is stuck.
    warning: Option<String>,
}

/// Match a claim against the visible providers and build what should be
/// written about it.
///
/// Split out of [`reconcile`] so the decision is reachable without an
/// apiserver: which provider wins, whether `status.provider` is touched at
/// all, and whether `lastTransitionTime` moves are the parts that can be
/// wrong in ways a cluster would only reveal slowly (a hot reconcile loop, a
/// provider silently unset). The two API calls around it are not.
fn decide(name: &str, claim: &ResourceClaim, candidates: &[Candidate]) -> Decision {
    let chosen = operator_core::matching::select_provider(
        &claim.spec.type_,
        &claim.spec.selector,
        candidates,
    );

    // Prior conditions feed the timestamp-preservation guard.
    let prior: Vec<ResourceClaimCondition> = claim
        .status
        .as_ref()
        .and_then(|s| s.conditions.clone())
        .unwrap_or_default();

    match chosen {
        Some(provider_name) => {
            let cond = condition(
                COND_SCHEDULED,
                "True",
                "MatchFound",
                &format!("matched ServiceProvider {provider_name}"),
                &prior,
            );
            Decision {
                patch: build_status_patch(name, Some(&provider_name), cond),
                provider: Some(provider_name),
                warning: None,
            }
        }
        None => {
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
            Decision {
                provider: None,
                patch: build_status_patch(name, None, cond),
                warning: Some(message),
            }
        }
    }
}

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

    use std::collections::BTreeMap;

    use operator_core::{ResourceClaimSpec, ResourceClaimStatus};

    fn labels(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    fn candidate(name: &str, type_: &str, labels_: &[(&str, &str)]) -> Candidate {
        Candidate {
            name: name.to_string(),
            type_: type_.to_string(),
            labels: labels(labels_),
        }
    }

    fn claim(type_: &str, selector: &[(&str, &str)]) -> ResourceClaim {
        ResourceClaim::new(
            "demo-web-pg",
            ResourceClaimSpec {
                type_: type_.to_string(),
                selector: labels(selector),
                ..Default::default()
            },
        )
    }

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

    // -----------------------------------------------------------------
    // decide() — the whole reconcile decision, without an apiserver
    // -----------------------------------------------------------------

    #[test]
    fn a_matching_provider_is_recorded_and_nothing_is_warned_about() {
        let c = claim("pg", &[("tier", "shared")]);
        let d = decide(
            "demo-web-pg",
            &c,
            &[candidate("pg-integrated", "pg", &[("tier", "shared")])],
        );
        assert_eq!(d.provider.as_deref(), Some("pg-integrated"));
        assert_eq!(
            d.patch.pointer("/status/provider").and_then(Value::as_str),
            Some("pg-integrated")
        );
        assert_eq!(
            d.patch
                .pointer("/status/conditions/0/status")
                .and_then(Value::as_str),
            Some("True")
        );
        // A scheduled claim has nothing to warn about; a note here would put
        // a Warning Event on a claim that is working.
        assert_eq!(d.warning, None);
    }

    #[test]
    fn a_provider_of_the_right_type_but_the_wrong_labels_is_not_a_match() {
        // The selector has to survive the trip into the matcher. Dropping it
        // schedules a `tier: dedicated` claim onto the shared backend — a
        // successful-looking reconcile that puts a tenant on the wrong
        // machine, which no status field would then contradict.
        let c = claim("pg", &[("tier", "dedicated")]);
        let d = decide(
            "demo-web-pg",
            &c,
            &[candidate("pg-integrated", "pg", &[("tier", "shared")])],
        );
        assert_eq!(d.provider, None);
        assert_eq!(
            d.patch
                .pointer("/status/conditions/0/status")
                .and_then(Value::as_str),
            Some("False")
        );
    }

    #[test]
    fn a_no_match_never_clears_a_provider_a_later_phase_already_set() {
        // The provisioner (2.4) owns `status.provider` once provisioning is
        // wired. A scheduler pass that finds nothing — because the
        // ServiceProvider list came back empty during a blip, say — must omit
        // the key rather than write null over it: a forced SSA apply carrying
        // `provider: null` would strip a provisioned claim's binding and send
        // it back to Pending.
        let mut c = claim("pg", &[("tier", "shared")]);
        c.status = Some(ResourceClaimStatus {
            provider: Some("pg-integrated".to_string()),
            ready: Some(true),
            ..Default::default()
        });
        let d = decide("demo-web-pg", &c, &[]);
        assert_eq!(d.provider, None);
        assert!(d.patch.pointer("/status/provider").is_none());
        assert!(d.patch.pointer("/status/ready").is_none());
    }

    #[test]
    fn the_warning_names_the_type_and_selector_that_matched_nothing() {
        // This note is the whole of what an operator sees in `kubectl
        // describe` for a stuck claim. Without the selector in it, every
        // unmatched `pg` claim produces the same sentence and the actual
        // reason — a label nothing carries — stays invisible.
        let c = claim("pg", &[("tier", "dedicated")]);
        let d = decide("demo-web-pg", &c, &[]);
        let warning = d.warning.expect("a no-match must carry a note");
        assert!(warning.contains("pg"), "{warning}");
        assert!(warning.contains("tier"), "{warning}");
        assert!(warning.contains("dedicated"), "{warning}");
        // …and the condition says the same thing, so the two cannot drift.
        assert_eq!(
            d.patch
                .pointer("/status/conditions/0/message")
                .and_then(Value::as_str),
            Some(warning.as_str())
        );
    }

    #[test]
    fn a_claim_that_still_matches_keeps_its_transition_timestamp() {
        // The hot-loop guard, end to end. Every status write bumps
        // `resourceVersion` and wakes this controller again, so a timestamp
        // that moves on an unchanged decision makes the controller reconcile
        // one claim forever at whatever rate the apiserver will take.
        let ts = "2026-01-01T00:00:00+00:00";
        let mut c = claim("pg", &[("tier", "shared")]);
        c.status = Some(ResourceClaimStatus {
            conditions: Some(vec![prev_cond(COND_SCHEDULED, "True", ts)]),
            ..Default::default()
        });
        let d = decide(
            "demo-web-pg",
            &c,
            &[candidate("pg-integrated", "pg", &[("tier", "shared")])],
        );
        assert_eq!(
            d.patch
                .pointer("/status/conditions/0/lastTransitionTime")
                .and_then(Value::as_str),
            Some(ts)
        );
    }

    #[test]
    fn a_claim_that_stops_matching_gets_a_fresh_transition_timestamp() {
        // The other side of the same guard: `Scheduled` really did change, so
        // the timestamp must move. Reusing it would date the failure to
        // whenever the claim was last healthy.
        let ts = "2026-01-01T00:00:00+00:00";
        let mut c = claim("pg", &[("tier", "shared")]);
        c.status = Some(ResourceClaimStatus {
            conditions: Some(vec![prev_cond(COND_SCHEDULED, "True", ts)]),
            ..Default::default()
        });
        let d = decide("demo-web-pg", &c, &[]);
        assert_ne!(
            d.patch
                .pointer("/status/conditions/0/lastTransitionTime")
                .and_then(Value::as_str),
            Some(ts)
        );
    }

    #[test]
    fn the_alphabetically_first_of_several_matching_providers_wins() {
        // Two equally valid providers must not make the winner depend on the
        // order the apiserver happened to list them in: an unstable choice
        // would move a claim between backends on nothing but a relist.
        let c = claim("pg", &[]);
        let candidates = [
            candidate("pg-zeta", "pg", &[]),
            candidate("pg-alpha", "pg", &[]),
        ];
        assert_eq!(
            decide("demo-web-pg", &c, &candidates).provider.as_deref(),
            Some("pg-alpha")
        );
        // Reversed input, same winner.
        let reversed = [
            candidate("pg-alpha", "pg", &[]),
            candidate("pg-zeta", "pg", &[]),
        ];
        assert_eq!(
            decide("demo-web-pg", &c, &reversed).provider.as_deref(),
            Some("pg-alpha")
        );
    }

    // -----------------------------------------------------------------
    // A scripted in-process apiserver
    //
    // `decide` above pins what the controller concludes. What it cannot
    // reach is what the controller DOES with that conclusion: which calls
    // go on the wire, in which order, and — for a failed ServiceProvider
    // list — that none do. `kube::Client` is a thin wrapper over a
    // `tower::Service`, so a service answering from a script exercises the
    // real client without a cluster.
    // -----------------------------------------------------------------

    use std::sync::{Arc, Mutex};

    use kube::client::Body;
    use operator_core::Metrics;

    /// One request, as the apiserver saw it.
    #[derive(Clone, Debug)]
    struct Call {
        method: String,
        uri: String,
        body: Value,
    }

    /// A `Client` that answers from `respond`, plus the ordered log of every
    /// request it was asked to serve.
    fn scripted_apiserver<F>(respond: F) -> (Client, Arc<Mutex<Vec<Call>>>)
    where
        F: FnMut(&Call) -> (u16, Value) + Send + 'static,
    {
        let log = Arc::new(Mutex::new(Vec::<Call>::new()));
        let sink = log.clone();
        let respond = Arc::new(Mutex::new(respond));
        let service = tower::service_fn(move |req: http::Request<Body>| {
            let sink = sink.clone();
            let respond = respond.clone();
            async move {
                let method = req.method().to_string();
                let uri = req.uri().to_string();
                let bytes = req.into_body().collect_bytes().await.expect("request body");
                let call = Call {
                    method,
                    uri,
                    body: serde_json::from_slice(&bytes).unwrap_or(Value::Null),
                };
                let (code, payload) = (respond.lock().expect("responder"))(&call);
                sink.lock().expect("log").push(call);
                Ok::<_, std::convert::Infallible>(
                    http::Response::builder()
                        .status(code)
                        .header("content-type", "application/json")
                        .body(Body::from(
                            serde_json::to_vec(&payload).expect("canned response"),
                        ))
                        .expect("canned response"),
                )
            }
        });
        (Client::new(service, "apprafter-system"), log)
    }

    /// A 500 the way the apiserver renders one when etcd is unavailable.
    fn apiserver_unavailable() -> (u16, Value) {
        (
            500,
            json!({
                "kind": "Status", "apiVersion": "v1", "status": "Failure",
                "message": "etcdserver: request timed out",
                "reason": "InternalError", "code": 500,
            }),
        )
    }

    /// A `ServiceProviderList` carrying zero or more providers.
    fn provider_list(providers: Vec<Value>) -> (u16, Value) {
        (
            200,
            json!({
                "apiVersion": "apprafter.io/v1alpha1",
                "kind": "ServiceProviderList",
                "metadata": { "resourceVersion": "1" },
                "items": providers,
            }),
        )
    }

    fn provider_json(name: &str, type_: &str, labels: &[(&str, &str)]) -> Value {
        json!({
            "apiVersion": "apprafter.io/v1alpha1",
            "kind": "ServiceProvider",
            "metadata": {
                "name": name,
                "namespace": "apprafter-system",
                "labels": labels.iter().copied().collect::<BTreeMap<_, _>>(),
            },
            "spec": { "type": type_, "backend": "cloudnative-pg" },
        })
    }

    /// The claim as it arrives from the watch: namespaced, with a uid (the
    /// Event's `regarding` reference is built from it).
    fn live_claim(type_: &str, selector: &[(&str, &str)]) -> Arc<ResourceClaim> {
        let mut c = claim(type_, selector);
        c.metadata.namespace = Some("landing".to_string());
        c.metadata.uid = Some("11111111-2222-3333-4444-555555555555".to_string());
        Arc::new(c)
    }

    fn context(client: Client) -> Arc<Context> {
        Arc::new(Context {
            client,
            metrics: Arc::new(Metrics::new()),
        })
    }

    /// The claim echoed back as the apiserver would after a status patch.
    fn patched_claim() -> (u16, Value) {
        (
            200,
            json!({
                "apiVersion": "apprafter.io/v1alpha1",
                "kind": "ResourceClaim",
                "metadata": { "name": "demo-web-pg", "namespace": "landing" },
                "spec": { "type": "pg", "selector": {} },
            }),
        )
    }

    /// A matched claim is written back as a status patch that names the
    /// winner, and NOTHING else happens — in particular no Warning Event.
    /// An Event on a claim that scheduled cleanly puts a permanent red mark
    /// in `kubectl describe` for a working resource.
    #[tokio::test]
    async fn a_scheduled_claim_gets_its_provider_patched_and_no_warning_event() {
        let (client, log) = scripted_apiserver(|call| match call.method.as_str() {
            "GET" => provider_list(vec![provider_json(
                "pg-integrated",
                "pg",
                &[("tier", "shared")],
            )]),
            _ => patched_claim(),
        });
        let ctx = context(client);

        let action = reconcile(live_claim("pg", &[("tier", "shared")]), ctx.clone())
            .await
            .expect("a matched claim reconciles cleanly");
        assert_eq!(
            format!("{action:?}"),
            format!("{:?}", Action::requeue(Duration::from_secs(300)))
        );

        let calls = log.lock().expect("log").clone();
        assert_eq!(
            calls.iter().map(|c| c.method.as_str()).collect::<Vec<_>>(),
            vec!["GET", "PATCH"],
            "one provider list, one status patch, nothing else: {calls:?}"
        );
        assert!(
            calls[1]
                .uri
                .contains("/namespaces/landing/resourceclaims/demo-web-pg/status"),
            "the write must land on the status subresource: {}",
            calls[1].uri
        );
        assert!(
            calls[1]
                .uri
                .contains("fieldManager=resourceclaim-scheduler"),
            "a shared field manager would let this controller fight the provisioner: {}",
            calls[1].uri
        );
        assert_eq!(
            calls[1]
                .body
                .pointer("/status/provider")
                .and_then(Value::as_str),
            Some("pg-integrated")
        );
        assert_eq!(
            ctx.metrics
                .reconcile_total
                .with_label_values(&[KIND, "landing", "ok"])
                .get(),
            1.0
        );
    }

    /// The provider list really is read from the cluster and really is fed
    /// to the matcher: a provider whose labels do not satisfy the claim's
    /// selector must leave the claim unscheduled. If the list were dropped
    /// on the floor (or the selector lost in transit) this claim would be
    /// bound to the wrong backend and nothing downstream would contradict
    /// it.
    #[tokio::test]
    async fn a_provider_that_fails_the_selector_leaves_the_claim_unscheduled() {
        let (client, log) = scripted_apiserver(|call| match call.method.as_str() {
            "GET" => provider_list(vec![provider_json(
                "pg-integrated",
                "pg",
                &[("tier", "shared")],
            )]),
            _ => patched_claim(),
        });
        let ctx = context(client);

        reconcile(live_claim("pg", &[("tier", "dedicated")]), ctx.clone())
            .await
            .expect("an unmatched claim is not an error");

        let calls = log.lock().expect("log").clone();
        let patch = calls
            .iter()
            .find(|c| c.method == "PATCH")
            .expect("the verdict must still be recorded");
        assert!(
            patch.body.pointer("/status/provider").is_none(),
            "an unmatched claim must not be bound to anything: {}",
            patch.body
        );
        assert_eq!(
            patch
                .body
                .pointer("/status/conditions/0/status")
                .and_then(Value::as_str),
            Some("False")
        );
    }

    /// An unmatched claim is announced as a Kubernetes Warning Event on the
    /// claim itself. `kubectl describe resourceclaim` is where an operator
    /// looks first, and the Event has to carry the claim's own reference —
    /// an Event pointing at the wrong object is invisible where it matters.
    #[tokio::test]
    async fn an_unmatched_claim_raises_a_warning_event_against_that_claim() {
        let (client, log) = scripted_apiserver(|call| match call.method.as_str() {
            "GET" => provider_list(vec![]),
            "PATCH" => patched_claim(),
            _ => (
                201,
                json!({ "apiVersion": "events.k8s.io/v1", "kind": "Event" }),
            ),
        });
        let ctx = context(client);

        reconcile(live_claim("pg", &[("tier", "dedicated")]), ctx.clone())
            .await
            .expect("an unmatched claim is not an error");

        let calls = log.lock().expect("log").clone();
        let event = calls
            .iter()
            .find(|c| c.method == "POST")
            .expect("an unmatched claim must be announced");
        assert!(
            event
                .uri
                .contains("/apis/events.k8s.io/v1/namespaces/landing/events"),
            "the Event must be written in the claim's namespace: {}",
            event.uri
        );
        assert_eq!(
            event.body.pointer("/type").and_then(Value::as_str),
            Some("Warning")
        );
        assert_eq!(
            event.body.pointer("/reason").and_then(Value::as_str),
            Some("NoMatchingServiceProvider")
        );
        assert_eq!(
            event
                .body
                .pointer("/regarding/name")
                .and_then(Value::as_str),
            Some("demo-web-pg")
        );
        assert_eq!(
            event
                .body
                .pointer("/regarding/kind")
                .and_then(Value::as_str),
            Some("ResourceClaim")
        );
        assert_eq!(
            event.body.pointer("/regarding/uid").and_then(Value::as_str),
            Some("11111111-2222-3333-4444-555555555555")
        );
        assert_eq!(
            event
                .body
                .pointer("/reportingController")
                .and_then(Value::as_str),
            Some(EVENT_REPORTER_CONTROLLER)
        );
        // …and the note says the same thing the condition does, so the two
        // places an operator can read cannot disagree about why it is stuck.
        let patch = calls
            .iter()
            .find(|c| c.method == "PATCH")
            .expect("the verdict must be recorded");
        assert_eq!(
            event.body.pointer("/note"),
            patch.body.pointer("/status/conditions/0/message")
        );
        assert_eq!(
            ctx.metrics
                .claim_unmatched_total
                .with_label_values(&[KIND, "landing", "no_matching_provider"])
                .get(),
            1.0
        );
        assert_eq!(
            ctx.metrics
                .reconcile_total
                .with_label_values(&[KIND, "landing", "pending"])
                .get(),
            1.0
        );
    }

    /// Events are best-effort. A cluster that rejects the Event (RBAC, a
    /// full event store) must not fail the reconcile: the status patch —
    /// the thing the provisioner and `apprafter app status` actually read —
    /// already landed, and erroring here would retry it forever and count a
    /// working reconcile as a failure.
    #[tokio::test]
    async fn a_rejected_event_does_not_fail_a_reconcile_whose_status_landed() {
        let (client, _log) = scripted_apiserver(|call| match call.method.as_str() {
            "GET" => provider_list(vec![]),
            "PATCH" => patched_claim(),
            _ => apiserver_unavailable(),
        });
        let ctx = context(client);

        reconcile(live_claim("pg", &[]), ctx.clone())
            .await
            .expect("a rejected Event must not fail the reconcile");
        assert_eq!(
            ctx.metrics
                .reconcile_total
                .with_label_values(&[KIND, "landing", "pending"])
                .get(),
            1.0
        );
    }

    /// A ServiceProvider list that FAILS must abort before any write. An
    /// empty list and a failed list look identical to the matcher, so
    /// swallowing the error would publish `Scheduled=False` — and a Warning
    /// Event — for every claim in the cluster on any apiserver blip, then
    /// flap them all back on the next pass.
    #[tokio::test]
    async fn a_failed_provider_list_writes_nothing_at_all() {
        let (client, log) = scripted_apiserver(|_| apiserver_unavailable());
        let ctx = context(client);

        let err = reconcile(live_claim("pg", &[]), ctx.clone())
            .await
            .expect_err("an unlistable cluster must not look like an empty one");
        assert!(matches!(err, ReconcileError::Kube(_)), "{err}");

        let calls = log.lock().expect("log").clone();
        assert_eq!(
            calls.iter().map(|c| c.method.as_str()).collect::<Vec<_>>(),
            vec!["GET"],
            "no status may be written off a failed list: {calls:?}"
        );
        assert_eq!(
            ctx.metrics
                .reconcile_total
                .with_label_values(&[KIND, "landing", "pending"])
                .get(),
            0.0,
            "a failed list is not a pending claim"
        );
    }

    /// When the status patch itself fails, the reconcile fails — and the
    /// Warning Event is NOT published. Announcing a verdict that was never
    /// recorded leaves `kubectl describe` insisting a claim is unmatched
    /// while its status still says otherwise.
    #[tokio::test]
    async fn a_failed_status_patch_aborts_before_announcing_the_verdict() {
        let (client, log) = scripted_apiserver(|call| match call.method.as_str() {
            "GET" => provider_list(vec![]),
            _ => apiserver_unavailable(),
        });
        let ctx = context(client);

        let err = reconcile(live_claim("pg", &[]), ctx.clone())
            .await
            .expect_err("a failed status write must surface");
        assert!(matches!(err, ReconcileError::Kube(_)), "{err}");

        let calls = log.lock().expect("log").clone();
        assert!(
            !calls.iter().any(|c| c.method == "POST"),
            "no Event may be published for a verdict that was not recorded: {calls:?}"
        );
    }

    /// Every reconcile error must be visible on BOTH metrics — the
    /// per-kind/namespace outcome counter (which claim is failing) and the
    /// error-only counter alerts fire on — and must be retried rather than
    /// dropped on the floor.
    #[tokio::test]
    async fn error_policy_counts_the_error_on_both_metrics_and_retries() {
        let (client, log) = scripted_apiserver(|_| apiserver_unavailable());
        let ctx = context(client);
        let err = ReconcileError::Kube(kube::Error::Api(kube::core::ErrorResponse {
            status: "Failure".into(),
            message: "serviceproviders.apprafter.io is forbidden".into(),
            reason: "Forbidden".into(),
            code: 403,
        }));

        let action = error_policy(live_claim("pg", &[]), &err, ctx.clone());

        assert_eq!(
            ctx.metrics
                .reconcile_errors
                .with_label_values(&[KIND])
                .get(),
            1.0
        );
        assert_eq!(
            ctx.metrics
                .reconcile_total
                .with_label_values(&[KIND, "landing", "error"])
                .get(),
            1.0
        );
        assert_eq!(
            format!("{action:?}"),
            format!("{:?}", Action::requeue(Duration::from_secs(30)))
        );
        assert!(
            log.lock().expect("log").is_empty(),
            "the error path must not talk to the apiserver"
        );
    }
}
