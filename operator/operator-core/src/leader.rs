// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Lease-based leader election for the AppRafter operator.
//!
//! Tier-1 single-replica scope: the operator creates (or takes over
//! a stale) `coordination.k8s.io/v1` Lease in the operator's
//! namespace, then renews it on every `renew_period`. Three
//! consecutive renewal failures exit the process so the Deployment
//! restart policy takes over.
//!
//! Multi-replica preemption with full leader-elector semantics
//! (jitter, backoff, fast handoff) lands in a tier-2/3 HA cycle.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use k8s_openapi::api::coordination::v1::{Lease, LeaseSpec};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::MicroTime;
use kube::api::{Api, ObjectMeta, PostParams};
use kube::Client;
use thiserror::Error;
use tracing::{info, warn};

/// Default Lease duration. The renew interval is one-third of this
/// value so we get three renewal attempts before a takeover window
/// opens.
const DEFAULT_LEASE_DURATION: Duration = Duration::from_secs(30);
const DEFAULT_RENEW_PERIOD: Duration = Duration::from_secs(10);
const RENEWAL_FAILURE_BUDGET: u32 = 3;

#[derive(Debug, Clone)]
pub struct LeaderConfig {
    pub namespace: String,
    pub name: String,
    pub holder_id: String,
    pub lease_duration: Duration,
    pub renew_period: Duration,
}

impl LeaderConfig {
    /// Reasonable defaults for the AppRafter operator.
    pub fn for_apprafter_operator(holder_id: impl Into<String>) -> Self {
        Self {
            namespace: "apprafter-system".to_string(),
            name: "apprafter-operator".to_string(),
            holder_id: holder_id.into(),
            lease_duration: DEFAULT_LEASE_DURATION,
            renew_period: DEFAULT_RENEW_PERIOD,
        }
    }
}

#[derive(Debug, Error)]
pub enum LeaderError {
    #[error("kube error: {0}")]
    Kube(#[from] kube::Error),

    #[error("lost leadership after {0} consecutive renewal failures")]
    LostLeadership(u32),
}

pub struct LeaderElection {
    client: Client,
    config: LeaderConfig,
    is_leader: Arc<AtomicBool>,
}

impl LeaderElection {
    pub fn new(client: Client, config: LeaderConfig) -> Self {
        Self {
            client,
            config,
            is_leader: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Returns a clone of the `is_leader` flag — set to true while
    /// we hold the Lease. Other tasks (the Controller in
    /// `apprafter-operator`) block on this becoming `true` before
    /// they start.
    pub fn is_leader_handle(&self) -> Arc<AtomicBool> {
        self.is_leader.clone()
    }

    /// Run the leader-election loop forever. Returns
    /// `LeaderError::LostLeadership` after `RENEWAL_FAILURE_BUDGET`
    /// consecutive renewal failures so the caller can exit the
    /// process cleanly.
    pub async fn run(self) -> Result<(), LeaderError> {
        let api: Api<Lease> = Api::namespaced(self.client.clone(), &self.config.namespace);
        let mut consecutive_failures: u32 = 0;
        loop {
            match self.acquire_or_renew(&api, Utc::now()).await {
                Ok(true) => {
                    consecutive_failures = 0;
                    if !self.is_leader.swap(true, Ordering::SeqCst) {
                        info!(
                            holder = %self.config.holder_id,
                            namespace = %self.config.namespace,
                            name = %self.config.name,
                            "became leader"
                        );
                    }
                }
                Ok(false) => {
                    consecutive_failures = 0;
                    if self.is_leader.swap(false, Ordering::SeqCst) {
                        warn!(
                            holder = %self.config.holder_id,
                            "lost leadership (Lease held by another holder)"
                        );
                    }
                }
                Err(err) => {
                    consecutive_failures = consecutive_failures.saturating_add(1);
                    warn!(?err, consecutive_failures, "leader election step failed");
                    if must_step_down(consecutive_failures, self.is_leader.load(Ordering::SeqCst)) {
                        self.is_leader.store(false, Ordering::SeqCst);
                        return Err(LeaderError::LostLeadership(consecutive_failures));
                    }
                }
            }
            tokio::time::sleep(self.config.renew_period).await;
        }
    }

    /// Try to acquire (create / take over a stale Lease) or renew.
    /// Returns `Ok(true)` if we hold the Lease at the end of the
    /// call, `Ok(false)` if another holder owns it and is fresh.
    async fn acquire_or_renew(
        &self,
        api: &Api<Lease>,
        now: DateTime<Utc>,
    ) -> Result<bool, LeaderError> {
        match api.get_opt(&self.config.name).await? {
            Some(existing) => {
                let holder = existing
                    .spec
                    .as_ref()
                    .and_then(|s| s.holder_identity.as_deref());
                let stale = is_lease_stale(
                    existing.spec.as_ref().and_then(|s| s.renew_time.as_ref()),
                    self.config.lease_duration,
                    now,
                );
                if may_take_lease(holder, stale, &self.config.holder_id) {
                    let mut updated = existing.clone();
                    updated.spec = Some(lease_spec(&self.config, now, existing.spec.as_ref()));
                    api.replace(&self.config.name, &PostParams::default(), &updated)
                        .await?;
                    Ok(true)
                } else {
                    Ok(false)
                }
            }
            None => {
                let lease = Lease {
                    metadata: ObjectMeta {
                        name: Some(self.config.name.clone()),
                        namespace: Some(self.config.namespace.clone()),
                        ..Default::default()
                    },
                    spec: Some(lease_spec(&self.config, now, None)),
                };
                api.create(&PostParams::default(), &lease).await?;
                Ok(true)
            }
        }
    }
}

/// The Lease body this holder writes when it acquires or renews.
///
/// A free function over the config rather than a method so it is reachable
/// without a `Client`: everything it decides is a pure function of the
/// config, the clock, and whatever spec was already there.
///
/// `acquireTime` is carried over from `prior` — it records when the CURRENT
/// tenure began, so a renewal that reset it would make a leader that has held
/// the Lease for a week look like it took over a second ago, and would erase
/// the one field a human uses to tell "stable" from "flapping".
fn lease_spec(config: &LeaderConfig, now: DateTime<Utc>, prior: Option<&LeaseSpec>) -> LeaseSpec {
    let acquire_time = prior
        .and_then(|p| p.acquire_time.clone())
        .unwrap_or(MicroTime(now));
    LeaseSpec {
        holder_identity: Some(config.holder_id.clone()),
        lease_duration_seconds: Some(config.lease_duration.as_secs() as i32),
        acquire_time: Some(acquire_time),
        renew_time: Some(MicroTime(now)),
        ..LeaseSpec::default()
    }
}

/// Whether this holder may write itself into an EXISTING Lease.
///
/// Two ways in, and only two: the Lease is already ours (a renewal), or its
/// holder has stopped renewing long enough to be considered gone (a
/// takeover). A fresh Lease held by somebody else is the whole point of the
/// mechanism — taking it would put two operators in the same reconcile loop,
/// both server-side-applying the same objects.
fn may_take_lease(holder: Option<&str>, stale: bool, me: &str) -> bool {
    holder == Some(me) || stale
}

/// Whether repeated apiserver failures must end the process.
///
/// Only a HOLDER steps down. Once we cannot renew, our Lease is expiring on a
/// clock we no longer control, so the safe move is to exit and let the
/// Deployment restart us. A replica that never became leader is in no such
/// race: exiting there would turn an apiserver blip into a crash-looping
/// standby, which is noise on top of an outage.
fn must_step_down(consecutive_failures: u32, is_leader: bool) -> bool {
    consecutive_failures >= RENEWAL_FAILURE_BUDGET && is_leader
}

/// Pure staleness check — extracted for testability. A Lease is
/// stale if its `renewTime` is older than `lease_duration` from the
/// supplied `now`.
fn is_lease_stale(
    renew_time: Option<&MicroTime>,
    lease_duration: Duration,
    now: DateTime<Utc>,
) -> bool {
    match renew_time {
        Some(t) => {
            let elapsed = now.signed_duration_since(t.0);
            elapsed.num_seconds() > lease_duration.as_secs() as i64
        }
        None => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_defaults_for_apprafter_operator() {
        let cfg = LeaderConfig::for_apprafter_operator("test-holder");
        assert_eq!(cfg.namespace, "apprafter-system");
        assert_eq!(cfg.name, "apprafter-operator");
        assert_eq!(cfg.holder_id, "test-holder");
        assert_eq!(cfg.lease_duration, DEFAULT_LEASE_DURATION);
        assert_eq!(cfg.renew_period, DEFAULT_RENEW_PERIOD);
    }

    #[test]
    fn lease_with_no_renew_time_is_stale() {
        let now = Utc::now();
        assert!(is_lease_stale(None, Duration::from_secs(30), now));
    }

    #[test]
    fn fresh_lease_is_not_stale() {
        let now = Utc::now();
        let renew = MicroTime(now);
        // 0 seconds elapsed — not stale.
        assert!(!is_lease_stale(Some(&renew), Duration::from_secs(30), now));
    }

    #[test]
    fn lease_older_than_lease_duration_is_stale() {
        let now = Utc::now();
        let earlier = now - chrono::Duration::seconds(31);
        let renew = MicroTime(earlier);
        // 31s elapsed > 30s lease duration — stale.
        assert!(is_lease_stale(Some(&renew), Duration::from_secs(30), now));
    }

    #[test]
    fn a_lease_renewed_exactly_one_duration_ago_is_not_yet_stale() {
        // The boundary, and it belongs on the incumbent's side: the renew
        // period is a third of the duration, so at exactly one duration the
        // holder has already missed two renewals and a third is in flight.
        // Declaring it stale one tick early is how two operators end up
        // applying the same objects at the same time.
        let now = Utc::now();
        let renew = MicroTime(now - chrono::Duration::seconds(30));
        assert!(!is_lease_stale(Some(&renew), Duration::from_secs(30), now));
    }

    // -----------------------------------------------------------------
    // may_take_lease — who is allowed to write into an existing Lease
    // -----------------------------------------------------------------

    #[test]
    fn a_fresh_lease_held_by_someone_else_is_left_alone() {
        // The one case the whole mechanism exists for. Taking it would put
        // two operators in the same reconcile loop, both server-side-applying
        // the same objects with the same field manager.
        assert!(!may_take_lease(Some("operator-b"), false, "operator-a"));
        // …and an existing Lease with no holder recorded is not an invitation
        // either, until it goes stale.
        assert!(!may_take_lease(None, false, "operator-a"));
    }

    #[test]
    fn our_own_lease_is_renewable_and_a_stale_one_is_takeable() {
        assert!(may_take_lease(Some("operator-a"), false, "operator-a"));
        assert!(may_take_lease(Some("operator-b"), true, "operator-a"));
    }

    // -----------------------------------------------------------------
    // must_step_down — when losing the apiserver must end the process
    // -----------------------------------------------------------------

    #[test]
    fn a_leader_steps_down_only_after_the_whole_failure_budget() {
        // Three attempts, because the renew period is a third of the lease
        // duration: exiting earlier would restart the operator over a single
        // blip that the next renewal would have covered.
        assert!(!must_step_down(RENEWAL_FAILURE_BUDGET - 1, true));
        assert!(must_step_down(RENEWAL_FAILURE_BUDGET, true));
    }

    #[test]
    fn a_replica_that_never_led_does_not_exit_on_apiserver_failures() {
        // A standby is in no race with an expiring Lease it does not hold.
        // Exiting here turns an apiserver outage into a crash-looping pod —
        // noise stacked on top of the real failure.
        assert!(!must_step_down(RENEWAL_FAILURE_BUDGET, false));
        assert!(!must_step_down(RENEWAL_FAILURE_BUDGET * 10, false));
    }

    // -----------------------------------------------------------------
    // lease_spec — what a renewal writes
    // -----------------------------------------------------------------

    #[test]
    fn a_renewal_moves_renew_time_but_keeps_the_tenures_acquire_time() {
        // `acquireTime` records when THIS tenure began. Resetting it on every
        // renewal would make a leader that has held the Lease for a week look
        // like it took over a second ago, erasing the only field that tells a
        // human "stable" from "flapping".
        let cfg = LeaderConfig::for_apprafter_operator("operator-a");
        let acquired = Utc::now() - chrono::Duration::seconds(600);
        let prior = LeaseSpec {
            holder_identity: Some("operator-a".to_string()),
            acquire_time: Some(MicroTime(acquired)),
            renew_time: Some(MicroTime(acquired)),
            ..LeaseSpec::default()
        };
        let now = Utc::now();
        let spec = lease_spec(&cfg, now, Some(&prior));
        assert_eq!(spec.acquire_time, Some(MicroTime(acquired)));
        assert_eq!(spec.renew_time, Some(MicroTime(now)));
    }

    #[test]
    fn a_takeover_from_a_holder_with_no_acquire_time_stamps_one_now() {
        // Taking over a Lease whose acquireTime is absent must produce one,
        // not propagate the absence: the field is what the next holder reads
        // to decide the tenure it is displacing.
        let cfg = LeaderConfig::for_apprafter_operator("operator-a");
        let now = Utc::now();
        let spec = lease_spec(&cfg, now, Some(&LeaseSpec::default()));
        assert_eq!(spec.acquire_time, Some(MicroTime(now)));
    }

    #[test]
    fn the_written_lease_carries_this_holders_identity_and_duration() {
        // The identity is what `may_take_lease` compares on the next pass, and
        // the duration is what every OTHER replica measures staleness against
        // — a Lease written with someone else's identity, or with a duration
        // that disagrees with the one this process renews on, hands the Lease
        // away while we still think we hold it.
        let cfg = LeaderConfig::for_apprafter_operator("operator-a");
        let spec = lease_spec(&cfg, Utc::now(), None);
        assert_eq!(spec.holder_identity.as_deref(), Some("operator-a"));
        assert_eq!(
            spec.lease_duration_seconds,
            Some(cfg.lease_duration.as_secs() as i32)
        );
    }

    #[test]
    fn stepping_down_reports_how_many_renewals_were_lost() {
        // This message is the only record of why the process exited; the
        // count is what tells a reader "the apiserver went away" apart from
        // "another replica took over".
        let err = LeaderError::LostLeadership(3);
        assert_eq!(
            err.to_string(),
            "lost leadership after 3 consecutive renewal failures"
        );
    }

    // -----------------------------------------------------------------
    // A scripted in-process apiserver
    //
    // The pure helpers above pin every DECISION leader election makes. What
    // they cannot reach is the part that actually causes split brain: which
    // requests the loop puts on the wire, and — in the one case that
    // matters — that it puts none there at all. `kube::Client` is a thin
    // wrapper over a `tower::Service`, so handing it a service that answers
    // from a script exercises the real client (real URL construction, real
    // serialisation, real 404/5xx mapping) without a cluster.
    // -----------------------------------------------------------------

    use std::sync::atomic::AtomicUsize;
    use std::sync::Mutex;

    use kube::client::Body;
    use serde_json::{json, Value};

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

    /// The apiserver's own 404 for a Lease that was never created.
    fn lease_not_found() -> (u16, Value) {
        (
            404,
            json!({
                "kind": "Status", "apiVersion": "v1", "status": "Failure",
                "message": "leases.coordination.k8s.io \"apprafter-operator\" not found",
                "reason": "NotFound", "code": 404,
            }),
        )
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

    fn lease_json(holder: &str, renewed: DateTime<Utc>, acquired: DateTime<Utc>) -> Value {
        json!({
            "apiVersion": "coordination.k8s.io/v1",
            "kind": "Lease",
            "metadata": { "name": "apprafter-operator", "namespace": "apprafter-system" },
            "spec": {
                "holderIdentity": holder,
                "leaseDurationSeconds": 30,
                "acquireTime": MicroTime(acquired),
                "renewTime": MicroTime(renewed),
            },
        })
    }

    fn election(client: Client, renew: Duration) -> LeaderElection {
        let mut config = LeaderConfig::for_apprafter_operator("operator-a");
        config.renew_period = renew;
        LeaderElection::new(client, config)
    }

    fn lease_api(client: &Client) -> Api<Lease> {
        Api::namespaced(client.clone(), "apprafter-system")
    }

    fn ago(secs: i64) -> DateTime<Utc> {
        Utc::now() - chrono::Duration::seconds(secs)
    }

    fn renew_time_of(body: &Value) -> DateTime<Utc> {
        let raw = body
            .pointer("/spec/renewTime")
            .and_then(Value::as_str)
            .expect("a written Lease must carry a renewTime");
        DateTime::parse_from_rfc3339(raw)
            .expect("renewTime must be RFC 3339")
            .with_timezone(&Utc)
    }

    /// Poll `flag` until it reads `want`, for up to two seconds. Returns
    /// whether it got there — the caller asserts on that rather than hanging.
    async fn reaches(flag: &Arc<AtomicBool>, want: bool) -> bool {
        for _ in 0..2000 {
            if flag.load(Ordering::SeqCst) == want {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        false
    }

    /// A cluster with no Lease yet gets one CREATED, carrying this holder's
    /// identity and the duration every other replica will measure staleness
    /// against. A create that omitted either would hand the Lease straight
    /// back out: an empty `holderIdentity` matches nobody, and a missing
    /// duration makes `is_lease_stale` read the Lease as expired forever.
    #[tokio::test]
    async fn an_absent_lease_is_created_carrying_this_holders_identity_and_duration() {
        let (client, log) = scripted_apiserver(|call| match call.method.as_str() {
            "GET" => lease_not_found(),
            _ => (201, lease_json("operator-a", Utc::now(), Utc::now())),
        });
        let le = election(client.clone(), Duration::from_millis(1));

        assert!(le
            .acquire_or_renew(&lease_api(&client), Utc::now())
            .await
            .expect("creating the first Lease must succeed"));

        let calls = log.lock().expect("log").clone();
        assert_eq!(calls.len(), 2, "one probe then one create: {calls:?}");
        assert_eq!(calls[0].method, "GET");
        assert_eq!(calls[1].method, "POST");
        assert!(
            calls[1]
                .uri
                .ends_with("/namespaces/apprafter-system/leases?"),
            "the create must POST the collection, not a name: {}",
            calls[1].uri
        );
        assert_eq!(
            calls[1]
                .body
                .pointer("/spec/holderIdentity")
                .and_then(Value::as_str),
            Some("operator-a")
        );
        assert_eq!(
            calls[1]
                .body
                .pointer("/spec/leaseDurationSeconds")
                .and_then(Value::as_i64),
            Some(30)
        );
    }

    /// THE invariant of the whole module: a Lease held by another operator
    /// that is still being renewed must not be written to AT ALL. Not a
    /// no-op update, not a conditional replace — no request. Two operators
    /// in the same reconcile loop server-side-apply the same objects under
    /// the same field manager, and the cluster has no way to notice.
    #[tokio::test]
    async fn a_fresh_lease_held_by_another_operator_is_never_written_to() {
        let (client, log) =
            scripted_apiserver(|_| (200, lease_json("operator-b", Utc::now(), ago(600))));
        let le = election(client.clone(), Duration::from_millis(1));

        assert!(!le
            .acquire_or_renew(&lease_api(&client), Utc::now())
            .await
            .expect("reading someone else's Lease is not an error"));

        let calls = log.lock().expect("log").clone();
        assert_eq!(
            calls.iter().map(|c| c.method.as_str()).collect::<Vec<_>>(),
            vec!["GET"],
            "the standby must read and stop: {calls:?}"
        );
    }

    /// A holder that stopped renewing is gone, and its Lease is taken over by
    /// name — a REPLACE of the existing object, not a second Lease. The
    /// takeover has to move `renewTime` forward too: a write that copied the
    /// dead holder's timestamp would leave the Lease stale on its own terms,
    /// so the next replica along would take it from us immediately.
    #[tokio::test]
    async fn a_stale_lease_is_taken_over_in_place_with_a_fresh_renew_time() {
        let stale_renew = ago(120);
        let (client, log) = scripted_apiserver(move |call| match call.method.as_str() {
            "GET" => (200, lease_json("operator-b", stale_renew, ago(600))),
            _ => (200, lease_json("operator-a", Utc::now(), ago(600))),
        });
        let le = election(client.clone(), Duration::from_millis(1));

        assert!(le
            .acquire_or_renew(&lease_api(&client), Utc::now())
            .await
            .expect("a stale Lease is takeable"));

        let calls = log.lock().expect("log").clone();
        assert_eq!(calls.len(), 2, "{calls:?}");
        assert_eq!(calls[1].method, "PUT");
        assert!(
            calls[1]
                .uri
                .contains("/namespaces/apprafter-system/leases/apprafter-operator"),
            "the takeover must replace the existing Lease: {}",
            calls[1].uri
        );
        assert_eq!(
            calls[1]
                .body
                .pointer("/spec/holderIdentity")
                .and_then(Value::as_str),
            Some("operator-a")
        );
        assert!(
            renew_time_of(&calls[1].body) > stale_renew,
            "the takeover must stamp its own renewTime"
        );
    }

    /// A renewal of our OWN Lease pushes `renewTime` forward. This is the
    /// only thing keeping the Lease alive: a renewal that re-sent the
    /// timestamp it read would let every other replica declare us stale one
    /// lease-duration later while we still believed we were leader.
    #[tokio::test]
    async fn renewing_our_own_lease_pushes_renew_time_forward() {
        let previously_renewed = ago(5);
        let (client, log) = scripted_apiserver(move |call| match call.method.as_str() {
            "GET" => (200, lease_json("operator-a", previously_renewed, ago(600))),
            _ => (200, lease_json("operator-a", Utc::now(), ago(600))),
        });
        let le = election(client.clone(), Duration::from_millis(1));

        assert!(le
            .acquire_or_renew(&lease_api(&client), Utc::now())
            .await
            .expect("renewing our own Lease must succeed"));

        let calls = log.lock().expect("log").clone();
        assert_eq!(calls[1].method, "PUT");
        assert!(
            renew_time_of(&calls[1].body) > previously_renewed,
            "a renewal that does not move renewTime is not a renewal"
        );
    }

    /// An apiserver failure must PROPAGATE, never degrade to "no Lease
    /// there". `acquire_or_renew` reporting `Ok(true)` — or the caller
    /// treating a read failure as an absent Lease and creating one — is
    /// exactly how a partitioned replica joins a live leader in the same
    /// reconcile loop.
    #[tokio::test]
    async fn an_apiserver_failure_is_not_mistaken_for_a_free_lease() {
        let (client, log) = scripted_apiserver(|_| apiserver_unavailable());
        let le = election(client.clone(), Duration::from_millis(1));

        let err = le
            .acquire_or_renew(&lease_api(&client), Utc::now())
            .await
            .expect_err("a 500 must not look like an acquirable Lease");
        assert!(matches!(err, LeaderError::Kube(_)), "{err}");
        assert_eq!(
            log.lock().expect("log").len(),
            1,
            "nothing may be written after a failed read"
        );
    }

    /// The `is_leader` flag is the gate every controller in the process waits
    /// on. It starts closed and opens only once the Lease is actually held —
    /// a flag set before the write lands would start reconciling on a Lease
    /// another operator still owns.
    #[tokio::test]
    async fn the_controller_gate_opens_only_after_the_lease_is_held() {
        let (client, _log) = scripted_apiserver(|call| match call.method.as_str() {
            "GET" => lease_not_found(),
            _ => (201, lease_json("operator-a", Utc::now(), Utc::now())),
        });
        let le = election(client, Duration::from_millis(1));
        let flag = le.is_leader_handle();
        assert!(
            !flag.load(Ordering::SeqCst),
            "the gate must be closed before the loop has acquired anything"
        );

        let task = tokio::spawn(le.run());
        let opened = reaches(&flag, true).await;
        task.abort();
        assert!(opened, "the gate never opened for the elected leader");
    }

    /// The other half of the gate: when the Lease moves to another holder,
    /// the flag must CLOSE. A leader that keeps the gate open after losing
    /// the Lease is the split-brain the Lease exists to prevent, and nothing
    /// downstream re-checks.
    #[tokio::test]
    async fn losing_the_lease_to_a_fresh_holder_closes_the_controller_gate() {
        let handover = Arc::new(AtomicBool::new(false));
        let script = handover.clone();
        let (client, _log) = scripted_apiserver(move |call| {
            match (call.method.as_str(), script.load(Ordering::SeqCst)) {
                ("GET", true) => (200, lease_json("operator-b", Utc::now(), Utc::now())),
                ("GET", false) => lease_not_found(),
                _ => (201, lease_json("operator-a", Utc::now(), Utc::now())),
            }
        });
        let le = election(client, Duration::from_millis(1));
        let flag = le.is_leader_handle();
        let task = tokio::spawn(le.run());

        assert!(reaches(&flag, true).await, "never became leader");
        handover.store(true, Ordering::SeqCst);
        let closed = reaches(&flag, false).await;
        task.abort();
        assert!(
            closed,
            "kept reconciling after the Lease moved to another holder"
        );
    }

    /// A LEADER that has burned the whole renewal budget ends the process:
    /// its Lease is expiring on a clock it no longer controls, so the safe
    /// move is to exit and let the Deployment restart it. The error carries
    /// the failure count, and the gate must be closed before it returns —
    /// returning with the gate still open would leave the in-process
    /// controllers reconciling all the way to exit.
    #[tokio::test]
    async fn a_leader_that_burns_the_renewal_budget_exits_with_the_gate_closed() {
        let gets = AtomicUsize::new(0);
        let (client, _log) = scripted_apiserver(move |call| match call.method.as_str() {
            "GET" if gets.fetch_add(1, Ordering::SeqCst) == 0 => lease_not_found(),
            "GET" => apiserver_unavailable(),
            _ => (201, lease_json("operator-a", Utc::now(), Utc::now())),
        });
        let le = election(client, Duration::from_millis(1));
        let flag = le.is_leader_handle();

        let err = tokio::time::timeout(Duration::from_secs(5), le.run())
            .await
            .expect("the loop must exit rather than spin on a dying Lease")
            .expect_err("a leader that cannot renew must not report success");
        assert!(
            matches!(err, LeaderError::LostLeadership(RENEWAL_FAILURE_BUDGET)),
            "{err}"
        );
        assert!(
            !flag.load(Ordering::SeqCst),
            "the gate must close before the process exits"
        );
    }

    /// A replica that never led is in no race with an expiring Lease, so the
    /// same run of failures must NOT end it — it keeps retrying. Exiting here
    /// turns an apiserver outage into a crash-looping standby, stacking
    /// restart noise on top of the real failure and leaving nothing ready to
    /// take over when the apiserver comes back.
    #[tokio::test]
    async fn a_standby_keeps_retrying_through_the_same_run_of_failures() {
        let (client, log) = scripted_apiserver(|_| apiserver_unavailable());
        let le = election(client, Duration::from_millis(1));
        let flag = le.is_leader_handle();

        let outcome = tokio::time::timeout(Duration::from_millis(200), le.run()).await;
        assert!(
            outcome.is_err(),
            "a standby must not exit on apiserver failures: {outcome:?}"
        );
        assert!(!flag.load(Ordering::SeqCst), "a standby never leads");
        assert!(
            log.lock().expect("log").len() > RENEWAL_FAILURE_BUDGET as usize,
            "it must have kept trying past the budget a leader would step down on"
        );
    }
}
