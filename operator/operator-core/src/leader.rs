// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Lease-based leader election for the AppRafter operator.
//!
//! Tier-1 single-replica scope: the operator creates (or takes over
//! a stale) `coordination.k8s.io/v1` Lease in the operator's
//! namespace, then renews it on every `renew_period`. A leader that
//! has not renewed for `renew_deadline` steps down, and so does one
//! that finds the Lease held by someone else; the binary exits on
//! either, so the Deployment restart policy takes over.
//!
//! The step-down is decided by TIME, not by a count of failures, and
//! every request is bounded — see [`LeaderConfig`] for why both.
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
use tokio::time::Instant;
use tracing::{info, warn};

use crate::k8s_time::{from_micro_time, micro_time};

/// How long after the last `renewTime` another replica may take the Lease.
const DEFAULT_LEASE_DURATION: Duration = Duration::from_secs(30);
/// How often a healthy leader renews.
const DEFAULT_RENEW_PERIOD: Duration = Duration::from_secs(10);
/// How long a leader may go without a successful renewal before it steps
/// down: two renew periods, which leaves the last third of the Lease as the
/// margin between this process stopping and anyone else being allowed to
/// start.
const DEFAULT_RENEW_DEADLINE: Duration = Duration::from_secs(20);
/// How soon a leader retries after a failed renewal.
const DEFAULT_RETRY_PERIOD: Duration = Duration::from_secs(2);
/// The longest a Lease READ may take before it is cut off and counted as a
/// failed step. Half a renew period, so a leader whose renewal read hangs is
/// cut off with half its window left: enough to try again.
const DEFAULT_READ_TIMEOUT: Duration = Duration::from_secs(5);

/// Where the Lease lives, who we are, and the timings.
///
/// The timings are four numbers with one job: a leader must STOP before any
/// other replica may START. Another replica takes the Lease once its
/// `renewTime` is older than `lease_duration`. This process stops leading
/// once it has gone `renew_deadline` without a successful renewal, measured
/// from the instant it stamped the `renewTime` it last wrote. So
/// `renew_deadline < lease_duration`, and the gap between them is the room
/// for clock skew between nodes and for the process to exit.
///
/// Counting failures cannot give that guarantee, which is why this module
/// no longer does. Three strikes at a 10s renew period put the third strike
/// at the thirtieth second — the moment the Lease becomes takeable — even
/// when every failure is instant. And a request is only a strike once it
/// returns: with the client's 295s read timeout, one GET that the apiserver
/// accepted and never answered kept `is_leader` true while the Lease
/// expired under it (the frozen `renewTime` the wave-1 upgrade walk saw).
/// Hence the bounds on each request, and the deadline caps every one of
/// them: nothing is still in flight past the moment this process must stop.
///
/// The bounds are per REQUEST, not per step, and only a read has one of its
/// own. A renewal is a read and then a write, and a healthy apiserver in
/// front of a slow disk answers both late. Bounded together at five
/// seconds, a 3s read plus a 3s write failed every renewal after its write
/// had already committed: a working leader stepped down, and the process
/// that replaced it wrote itself into the Lease on every attempt without
/// ever opening its gate, so nobody reconciled and nobody else could take
/// over. So a read that has not answered within `read_timeout` is cut off —
/// a retry still fits before the deadline — while a write gets everything
/// left in its step. A write's latency is the storage's, and one cut short
/// may already be committed: ending it early buys a retry that has to start
/// with a fresh read anyway.
#[derive(Debug, Clone)]
pub struct LeaderConfig {
    pub namespace: String,
    pub name: String,
    pub holder_id: String,
    /// What every other replica measures staleness against.
    pub lease_duration: Duration,
    /// A healthy leader's renewal cadence; also a standby's polling cadence.
    pub renew_period: Duration,
    /// No successful renewal for this long → step down. Must be shorter
    /// than `lease_duration`.
    pub renew_deadline: Duration,
    /// Delay before a leader retries a failed renewal.
    pub retry_period: Duration,
    /// Upper bound on one Lease read. A read that hits it fails its step,
    /// exactly like an error response. Writes have no bound of their own:
    /// they run until the step's window closes.
    pub read_timeout: Duration,
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
            renew_deadline: DEFAULT_RENEW_DEADLINE,
            retry_period: DEFAULT_RETRY_PERIOD,
            read_timeout: DEFAULT_READ_TIMEOUT,
        }
    }
}

#[derive(Debug, Error)]
pub enum LeaderError {
    #[error("kube error: {0}")]
    Kube(#[from] kube::Error),

    /// One Lease request was not answered within its bound.
    #[error(
        "the apiserver did not answer the Lease {request} within {:.1}s",
        .after.as_secs_f64()
    )]
    Timeout {
        request: &'static str,
        after: Duration,
    },

    /// The leader went `renew_deadline` without a successful renewal.
    #[error(
        "lost leadership: no successful Lease renewal for {:.1}s ({failures} consecutive failures)",
        .since.as_secs_f64()
    )]
    LostLeadership { failures: u32, since: Duration },

    /// The leader read the Lease and found another holder in it.
    #[error("lost leadership: the Lease is held by another holder")]
    Deposed,
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

    /// Run the leader-election loop. It never returns `Ok`: it returns an
    /// error once this process has held the Lease and must stop acting on
    /// it — `LostLeadership` when no renewal succeeded for `renew_deadline`,
    /// `Deposed` when the Lease turned out to be held by someone else. The
    /// gate is closed before it returns, but the controllers do not watch
    /// the gate once they have started, so the caller must end the process.
    ///
    /// A replica that never led keeps retrying through any failure: it is in
    /// no race with a Lease it does not hold.
    pub async fn run(self) -> Result<(), LeaderError> {
        let api: Api<Lease> = Api::namespaced(self.client.clone(), &self.config.namespace);
        let mut consecutive_failures: u32 = 0;
        // When this process stamped the `renewTime` it last wrote, on the
        // monotonic clock (a wall-clock step must not stretch a tenure).
        // `Some` exactly while we lead.
        let mut renewed_at: Option<Instant> = None;
        loop {
            let started = Instant::now();
            let since_renewal = renewed_at.map(|at| started.saturating_duration_since(at));
            let Some(window) = step_window(&self.config, since_renewal) else {
                return Err(self.step_down(LeaderError::LostLeadership {
                    failures: consecutive_failures,
                    since: since_renewal.unwrap_or_default(),
                }));
            };
            // `now` is stamped into `renewTime` and `started` is the same
            // instant on the monotonic clock: the deadline counts from what
            // the other replicas will count from.
            let step = self
                .acquire_or_renew(&api, Utc::now(), started + window)
                .await;
            let next_attempt = match step {
                Ok(true) => {
                    consecutive_failures = 0;
                    renewed_at = Some(started);
                    if !self.is_leader.swap(true, Ordering::SeqCst) {
                        info!(
                            holder = %self.config.holder_id,
                            namespace = %self.config.namespace,
                            name = %self.config.name,
                            "became leader"
                        );
                    }
                    started + self.config.renew_period
                }
                Ok(false) if renewed_at.is_some() => {
                    // Nobody may take a Lease its holder is still renewing,
                    // so reaching here means the clocks disagree by more than
                    // the margin, or a human edited the Lease. Either way the
                    // other holder is acting, so this one must not.
                    return Err(self.step_down(LeaderError::Deposed));
                }
                Ok(false) => {
                    consecutive_failures = 0;
                    started + self.config.renew_period
                }
                Err(err) => {
                    consecutive_failures = consecutive_failures.saturating_add(1);
                    warn!(
                        %err,
                        consecutive_failures,
                        leading = renewed_at.is_some(),
                        "leader election step failed"
                    );
                    match renewed_at {
                        // Retry soon, but never later than the deadline:
                        // the check at the top of the loop steps down there.
                        Some(at) => (started + self.config.retry_period)
                            .min(at + self.config.renew_deadline),
                        None => started + self.config.renew_period,
                    }
                }
            };
            tokio::time::sleep_until(next_attempt).await;
        }
    }

    /// Close the controller gate and hand back why.
    fn step_down(&self, why: LeaderError) -> LeaderError {
        self.is_leader.store(false, Ordering::SeqCst);
        warn!(holder = %self.config.holder_id, reason = %why, "stepping down as leader");
        why
    }

    /// Try to acquire (create / take over a stale Lease) or renew.
    /// Returns `Ok(true)` if we hold the Lease at the end of the
    /// call, `Ok(false)` if another holder owns it and is fresh.
    ///
    /// Every request is over by `until`, the end of this step's window; see
    /// [`request_bound`] for how much of it each one gets.
    async fn acquire_or_renew(
        &self,
        api: &Api<Lease>,
        now: DateTime<Utc>,
        until: Instant,
    ) -> Result<bool, LeaderError> {
        let read = api.get_opt(&self.config.name);
        match self.bounded(Request::Read, until, read).await? {
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
                    let params = PostParams::default();
                    let write = api.replace(&self.config.name, &params, &updated);
                    self.bounded(Request::Write, until, write).await?;
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
                let params = PostParams::default();
                let write = api.create(&params, &lease);
                self.bounded(Request::Write, until, write).await?;
                Ok(true)
            }
        }
    }

    /// Run one Lease request under its bound, a request that hits the bound
    /// being a failure like any other.
    async fn bounded<T>(
        &self,
        request: Request,
        until: Instant,
        call: impl std::future::Future<Output = Result<T, kube::Error>>,
    ) -> Result<T, LeaderError> {
        let left = until.saturating_duration_since(Instant::now());
        let bound = request_bound(&self.config, request, left);
        match tokio::time::timeout(bound, call).await {
            Ok(answer) => Ok(answer?),
            Err(_elapsed) => Err(LeaderError::Timeout {
                request: request.as_str(),
                after: bound,
            }),
        }
    }
}

/// The two kinds of Lease request, which are bounded differently.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Request {
    Read,
    Write,
}

impl Request {
    fn as_str(self) -> &'static str {
        match self {
            Request::Read => "read",
            Request::Write => "write",
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
        .unwrap_or_else(|| micro_time(now));
    LeaseSpec {
        holder_identity: Some(config.holder_id.clone()),
        lease_duration_seconds: Some(config.lease_duration.as_secs() as i32),
        acquire_time: Some(acquire_time),
        renew_time: Some(micro_time(now)),
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

/// How long the step starting now may run, given how long ago this process
/// stamped its last successful renewal (`since_renewal`, which is `None`
/// while it does not lead).
///
/// A LEADER's step runs until its renew deadline, so no request can still be
/// in flight — and, if it succeeded late, re-open the gate — after the
/// moment this process has to stop. `None` means that moment has come: step
/// down without trying again.
///
/// A replica that does not lead has no deadline. It is in no race with a
/// Lease it does not hold, and stepping down there would turn an apiserver
/// blip into a crash-looping standby, which is noise on top of an outage. Its
/// step gets one renew period, the cadence it polls on anyway: room for a
/// slow read and a slow write, and no request outlives the next poll.
fn step_window(config: &LeaderConfig, since_renewal: Option<Duration>) -> Option<Duration> {
    match since_renewal {
        None => Some(config.renew_period),
        Some(since) => config
            .renew_deadline
            .checked_sub(since)
            .filter(|left| !left.is_zero()),
    }
}

/// How long one request may take when `left` remains in its step's window:
/// a read at most `read_timeout`, so a hung read is retried inside the
/// window rather than using it up; a write all of it (see [`LeaderConfig`]).
fn request_bound(config: &LeaderConfig, request: Request, left: Duration) -> Duration {
    match request {
        Request::Read => left.min(config.read_timeout),
        Request::Write => left,
    }
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
            let elapsed = now.signed_duration_since(from_micro_time(t));
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
        assert_eq!(cfg.renew_deadline, DEFAULT_RENEW_DEADLINE);
        assert_eq!(cfg.retry_period, DEFAULT_RETRY_PERIOD);
        assert_eq!(cfg.read_timeout, DEFAULT_READ_TIMEOUT);
    }

    #[test]
    fn the_defaults_stop_a_leader_before_anyone_may_take_its_lease() {
        // The one relation the whole module rests on: the process stops
        // acting as leader BEFORE the Lease it last renewed becomes takeable,
        // with room left over for clock skew and for the process to exit.
        let cfg = LeaderConfig::for_apprafter_operator("operator-a");
        assert!(cfg.renew_deadline < cfg.lease_duration);
        assert_eq!(
            cfg.lease_duration - cfg.renew_deadline,
            Duration::from_secs(10),
            "the margin between stepping down and a possible takeover"
        );
        // A healthy leader renews at least once inside its own deadline…
        assert!(cfg.renew_period < cfg.renew_deadline);
        // …and a renewal whose read hangs is cut off with time left in its
        // window, so it is retried before the deadline rather than being the
        // last attempt.
        assert!(cfg.read_timeout < cfg.renew_deadline - cfg.renew_period);
        assert!(cfg.retry_period < cfg.renew_deadline - cfg.renew_period);
    }

    #[test]
    fn lease_with_no_renew_time_is_stale() {
        let now = Utc::now();
        assert!(is_lease_stale(None, Duration::from_secs(30), now));
    }

    #[test]
    fn fresh_lease_is_not_stale() {
        let now = Utc::now();
        let renew = micro_time(now);
        // 0 seconds elapsed — not stale.
        assert!(!is_lease_stale(Some(&renew), Duration::from_secs(30), now));
    }

    #[test]
    fn lease_older_than_lease_duration_is_stale() {
        let now = Utc::now();
        let earlier = now - chrono::Duration::seconds(31);
        let renew = micro_time(earlier);
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
        let renew = micro_time(now - chrono::Duration::seconds(30));
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
    // step_window / request_bound — how long a step and each request in
    // it may take
    // -----------------------------------------------------------------

    #[test]
    fn a_leaders_step_runs_until_its_deadline_and_never_past_it() {
        // A renewal due on schedule has the ten seconds left before its
        // deadline; three seconds before the deadline it has three. A
        // request still in flight at the deadline could succeed late and
        // re-open the gate after the process should have stopped.
        let cfg = LeaderConfig::for_apprafter_operator("operator-a");
        assert_eq!(
            step_window(&cfg, Some(cfg.renew_period)),
            Some(cfg.renew_deadline - cfg.renew_period)
        );
        let since = cfg.renew_deadline - Duration::from_secs(3);
        assert_eq!(step_window(&cfg, Some(since)), Some(Duration::from_secs(3)));
    }

    #[test]
    fn at_or_past_the_deadline_a_leader_does_not_try_again() {
        let cfg = LeaderConfig::for_apprafter_operator("operator-a");
        assert_eq!(step_window(&cfg, Some(cfg.renew_deadline)), None);
        assert_eq!(step_window(&cfg, Some(cfg.lease_duration)), None);
    }

    #[test]
    fn a_standbys_step_gets_one_renew_period() {
        // No deadline, so the cadence it polls on: room for a slow read and
        // a slow write, and nothing outlives the next poll.
        let cfg = LeaderConfig::for_apprafter_operator("operator-a");
        assert_eq!(step_window(&cfg, None), Some(cfg.renew_period));
    }

    #[test]
    fn a_read_is_cut_at_the_read_timeout_and_a_write_gets_whatever_is_left() {
        // Not the client's 295s: a read that hangs is cut off and retried.
        // A write is slow for the storage's reasons and may commit anyway,
        // so it keeps the rest of the window. Neither outlives the window.
        let cfg = LeaderConfig::for_apprafter_operator("operator-a");
        let ten = Duration::from_secs(10);
        let two = Duration::from_secs(2);
        assert_eq!(request_bound(&cfg, Request::Read, ten), cfg.read_timeout);
        assert_eq!(request_bound(&cfg, Request::Write, ten), ten);
        assert_eq!(request_bound(&cfg, Request::Read, two), two);
        assert_eq!(request_bound(&cfg, Request::Write, two), two);
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
            acquire_time: Some(micro_time(acquired)),
            renew_time: Some(micro_time(acquired)),
            ..LeaseSpec::default()
        };
        let now = Utc::now();
        let spec = lease_spec(&cfg, now, Some(&prior));
        assert_eq!(spec.acquire_time, Some(micro_time(acquired)));
        assert_eq!(spec.renew_time, Some(micro_time(now)));
    }

    #[test]
    fn a_takeover_from_a_holder_with_no_acquire_time_stamps_one_now() {
        // Taking over a Lease whose acquireTime is absent must produce one,
        // not propagate the absence: the field is what the next holder reads
        // to decide the tenure it is displacing.
        let cfg = LeaderConfig::for_apprafter_operator("operator-a");
        let now = Utc::now();
        let spec = lease_spec(&cfg, now, Some(&LeaseSpec::default()));
        assert_eq!(spec.acquire_time, Some(micro_time(now)));
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

    /// The written LeaseSpec, byte for byte. `acquireTime`/`renewTime` are
    /// `metav1.MicroTime`, which the apiserver parses with Go's fixed-width
    /// RFC3339Micro: exactly six fractional digits, `Z`. A whole-second
    /// `now` is the case a lenient formatter breaks (it would drop the
    /// `.000000` and every renewal on that second would be a 400). Pinning
    /// the whole object also pins that no new optional LeaseSpec field
    /// (`strategy`, `preferredHolder`) leaks onto the wire as a null.
    #[test]
    fn the_written_lease_spec_is_pinned_byte_for_byte() {
        let cfg = LeaderConfig::for_apprafter_operator("operator-a");
        let at = |s: &str| DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc);
        let prior = LeaseSpec {
            holder_identity: Some("operator-a".to_string()),
            acquire_time: Some(micro_time(at("2026-09-22T17:43:31.5Z"))),
            renew_time: Some(micro_time(at("2026-09-22T17:53:21.123456Z"))),
            ..LeaseSpec::default()
        };
        let renewal = lease_spec(&cfg, at("2026-09-22T17:53:31Z"), Some(&prior));
        assert_eq!(
            serde_json::to_string(&renewal).unwrap(),
            r#"{"acquireTime":"2026-09-22T17:43:31.500000Z","holderIdentity":"operator-a","leaseDurationSeconds":30,"renewTime":"2026-09-22T17:53:31.000000Z"}"#
        );
        let first = lease_spec(&cfg, at("2026-09-22T17:53:31.987654321Z"), None);
        assert_eq!(
            serde_json::to_string(&first).unwrap(),
            r#"{"acquireTime":"2026-09-22T17:53:31.987654Z","holderIdentity":"operator-a","leaseDurationSeconds":30,"renewTime":"2026-09-22T17:53:31.987654Z"}"#
        );
    }

    #[test]
    fn stepping_down_says_why_the_process_exited() {
        // These messages are the only record of why the process exited. The
        // time and the count tell "the apiserver went away" apart from "it
        // answered and said no", and both apart from "another replica took
        // over".
        let err = LeaderError::LostLeadership {
            failures: 2,
            since: Duration::from_secs(20),
        };
        assert_eq!(
            err.to_string(),
            "lost leadership: no successful Lease renewal for 20.0s (2 consecutive failures)"
        );
        assert_eq!(
            LeaderError::Deposed.to_string(),
            "lost leadership: the Lease is held by another holder"
        );
        assert_eq!(
            LeaderError::Timeout {
                request: Request::Read.as_str(),
                after: Duration::from_millis(5000),
            }
            .to_string(),
            "the apiserver did not answer the Lease read within 5.0s"
        );
        assert_eq!(
            LeaderError::Timeout {
                request: Request::Write.as_str(),
                after: Duration::from_millis(7000),
            }
            .to_string(),
            "the apiserver did not answer the Lease write within 7.0s"
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

    /// The status a responder returns to make the apiserver accept the
    /// request and never answer it — the hang the client's read timeout only
    /// ends after 295s.
    const NEVER_ANSWER: u16 = 0;

    fn hang() -> (u16, Value) {
        (NEVER_ANSWER, Value::Null)
    }

    /// A `Client` that answers from `respond`, plus the ordered log of every
    /// request it was asked to serve (a request that is never answered is
    /// logged when it arrives).
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
                if code == NEVER_ANSWER {
                    std::future::pending::<()>().await;
                }
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
                "acquireTime": micro_time(acquired),
                "renewTime": micro_time(renewed),
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

    /// A step window no scripted answer comes near, for the tests that call
    /// `acquire_or_renew` directly to see which requests it makes.
    fn unhurried() -> Instant {
        Instant::now() + Duration::from_secs(60)
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
        // A whole-second clock: the value a lenient MicroTime formatter
        // would write without its `.000000`, which the apiserver rejects.
        let now = DateTime::parse_from_rfc3339("2026-09-22T17:53:31Z")
            .unwrap()
            .with_timezone(&Utc);

        assert!(le
            .acquire_or_renew(&lease_api(&client), now, unhurried())
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
        // The bytes the real client put on the wire for both MicroTimes.
        for field in ["/spec/acquireTime", "/spec/renewTime"] {
            assert_eq!(
                calls[1].body.pointer(field).and_then(Value::as_str),
                Some("2026-09-22T17:53:31.000000Z"),
                "{field} must be RFC3339Micro on the wire"
            );
        }
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
            .acquire_or_renew(&lease_api(&client), Utc::now(), unhurried())
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
            .acquire_or_renew(&lease_api(&client), Utc::now(), unhurried())
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
            .acquire_or_renew(&lease_api(&client), Utc::now(), unhurried())
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
            .acquire_or_renew(&lease_api(&client), Utc::now(), unhurried())
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
    /// the flag must CLOSE and the loop must END. Closing the gate alone is
    /// not stepping down — the controllers only wait on it to START and never
    /// look at it again — so a leader that carried on looping after seeing
    /// another holder would keep reconciling beside it for as long as the
    /// process lived.
    #[tokio::test]
    async fn losing_the_lease_to_a_fresh_holder_ends_the_leaders_loop() {
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
        let ended = tokio::time::timeout(Duration::from_secs(2), task).await;
        assert!(
            closed,
            "kept reconciling after the Lease moved to another holder"
        );
        let err = ended
            .expect("a deposed leader must end its loop, not keep polling")
            .expect("the loop must not panic")
            .expect_err("a deposed leader must not report success");
        assert!(matches!(err, LeaderError::Deposed), "{err}");
    }

    // -----------------------------------------------------------------
    // The timing of a step-down, on tokio's paused clock
    //
    // These run the real loop at the real 30s/10s/20s timings: a paused
    // runtime jumps its clock to the next timer whenever every task is idle,
    // so a request that never answers costs no wall time, and the instant at
    // which the loop gives up is exact rather than sampled.
    // -----------------------------------------------------------------

    /// A leader's first step: no Lease yet, so it creates one. Every request
    /// after those two is answered by `then`.
    fn leader_then<F>(mut then: F) -> impl FnMut(&Call) -> (u16, Value) + Send + 'static
    where
        F: FnMut(&Call) -> (u16, Value) + Send + 'static,
    {
        let mut served = 0usize;
        move |call| {
            served += 1;
            match served {
                1 => lease_not_found(),
                2 => (201, lease_json("operator-a", Utc::now(), Utc::now())),
                _ => then(call),
            }
        }
    }

    /// THE bug this loop was rewritten for. The leader holds the Lease, then
    /// every request it makes is accepted and never answered. It must stop
    /// leading while the Lease it last renewed is still its own — i.e. less
    /// than `lease_duration` after it stamped that renewal, which is the
    /// earliest any other replica may take it. Before the per-step bound, the
    /// loop sat in the first hung GET with the gate open: a leader in its own
    /// eyes for as long as the read timeout, and a takeable Lease in
    /// everyone else's after thirty seconds.
    #[tokio::test(start_paused = true)]
    async fn a_leader_whose_requests_hang_steps_down_before_its_lease_is_takeable() {
        let (client, log) = scripted_apiserver(leader_then(|_| hang()));
        let cfg = LeaderConfig::for_apprafter_operator("operator-a");
        let le = LeaderElection::new(client, cfg.clone());
        let flag = le.is_leader_handle();
        // The clock is paused and the first two answers are immediate, so the
        // acquisition is stamped at exactly this instant.
        let renewed_at = Instant::now();

        let outcome = tokio::time::timeout(cfg.lease_duration * 3, le.run()).await;
        let stopped_after = renewed_at.elapsed();

        let err = outcome
            .expect("a leader whose renewals hang must step down, not wait on the request")
            .expect_err("a leader that cannot renew must not report success");
        assert!(
            stopped_after < cfg.lease_duration,
            "stepped down {stopped_after:?} after its last renewal; another replica may take \
             the Lease after {:?}",
            cfg.lease_duration
        );
        assert!(
            stopped_after >= cfg.renew_deadline,
            "gave up at {stopped_after:?}, before its own deadline"
        );
        // Each hung step was cut off and counted as a failed renewal.
        match err {
            LeaderError::LostLeadership { failures, since } => {
                assert_eq!(since, stopped_after);
                assert!(failures >= 1, "the hung steps were never counted");
            }
            other => panic!("expected LostLeadership, got {other}"),
        }
        assert!(!flag.load(Ordering::SeqCst), "the gate must be closed");
        let calls = log.lock().expect("log").clone();
        assert!(
            calls.len() >= 3,
            "a hung renewal must be retried inside the deadline, not be the last attempt: \
             {calls:?}"
        );
    }

    /// One hung renewal is a failed step, not the end of the process and not
    /// the end of the loop: the step is cut off, the retry succeeds, and the
    /// leader keeps both the Lease and the gate. Without the bound the loop
    /// is still inside that first request a minute later.
    #[tokio::test(start_paused = true)]
    async fn one_hung_renewal_is_retried_and_the_leader_keeps_the_lease() {
        let mut after = 0usize;
        let (client, log) = scripted_apiserver(leader_then(move |_| {
            after += 1;
            if after == 1 {
                hang()
            } else {
                // Our own, freshly renewed Lease — for the GET and the PUT.
                (200, lease_json("operator-a", Utc::now(), ago(600)))
            }
        }));
        let cfg = LeaderConfig::for_apprafter_operator("operator-a");
        let le = LeaderElection::new(client, cfg.clone());
        let flag = le.is_leader_handle();

        let outcome = tokio::time::timeout(cfg.lease_duration * 2, le.run()).await;
        assert!(
            outcome.is_err(),
            "a single hung request must not end the loop: {outcome:?}"
        );
        assert!(flag.load(Ordering::SeqCst), "the leader lost its gate");
        let calls = log.lock().expect("log").clone();
        let renewals_after_the_hang = calls[3..].iter().filter(|c| c.method == "PUT").count();
        assert!(
            renewals_after_the_hang >= 3,
            "the leader must go on renewing after the hung request: {calls:?}"
        );
    }

    /// A LEADER whose apiserver keeps answering 500 retries every
    /// `retry_period` and steps down at its deadline — not at a count of
    /// failures. Three strikes at the renew period land on the thirtieth
    /// second, the very moment another replica may take the Lease. The gate
    /// must be closed before the loop returns, or the in-process controllers
    /// keep reconciling all the way to exit.
    #[tokio::test(start_paused = true)]
    async fn a_leader_whose_renewals_fail_steps_down_at_its_deadline() {
        let (client, _log) = scripted_apiserver(leader_then(|_| apiserver_unavailable()));
        let cfg = LeaderConfig::for_apprafter_operator("operator-a");
        let le = LeaderElection::new(client, cfg.clone());
        let flag = le.is_leader_handle();
        let renewed_at = Instant::now();

        let err = tokio::time::timeout(cfg.lease_duration * 3, le.run())
            .await
            .expect("the loop must exit rather than spin on a dying Lease")
            .expect_err("a leader that cannot renew must not report success");
        let stopped_after = renewed_at.elapsed();
        assert_eq!(stopped_after, cfg.renew_deadline);
        // The first renewal is due one renew period in; from then on it is
        // retried every retry period until the deadline.
        let retries =
            ((cfg.renew_deadline - cfg.renew_period).as_secs() / cfg.retry_period.as_secs()) as u32;
        match err {
            LeaderError::LostLeadership { failures, since } => {
                assert_eq!(failures, retries);
                assert_eq!(since, cfg.renew_deadline);
            }
            other => panic!("expected LostLeadership, got {other}"),
        }
        assert!(
            !flag.load(Ordering::SeqCst),
            "the gate must close before the process exits"
        );
    }

    /// A replica that never led is in no race with an expiring Lease, so no
    /// run of failures ends it — it keeps retrying. Exiting here turns an
    /// apiserver outage into a crash-looping standby, stacking restart noise
    /// on top of the real failure and leaving nothing ready to take over
    /// when the apiserver comes back.
    #[tokio::test(start_paused = true)]
    async fn a_standby_keeps_retrying_through_any_run_of_failures() {
        let (client, log) = scripted_apiserver(|_| apiserver_unavailable());
        let cfg = LeaderConfig::for_apprafter_operator("operator-a");
        let le = LeaderElection::new(client, cfg.clone());
        let flag = le.is_leader_handle();

        let outcome = tokio::time::timeout(cfg.lease_duration * 3, le.run()).await;
        assert!(
            outcome.is_err(),
            "a standby must not exit on apiserver failures: {outcome:?}"
        );
        assert!(!flag.load(Ordering::SeqCst), "a standby never leads");
        assert!(
            log.lock().expect("log").len() >= 9,
            "it must keep polling once per renew period"
        );
    }

    /// The same for a standby whose requests hang: each is cut off at the
    /// per-step bound and it polls again on its cadence, instead of sitting
    /// in one request and never noticing when the Lease is free.
    #[tokio::test(start_paused = true)]
    async fn a_standby_whose_requests_hang_keeps_polling() {
        let (client, log) = scripted_apiserver(|_| hang());
        let cfg = LeaderConfig::for_apprafter_operator("operator-a");
        let le = LeaderElection::new(client, cfg.clone());

        let outcome = tokio::time::timeout(cfg.lease_duration * 3, le.run()).await;
        assert!(outcome.is_err(), "a standby must not exit: {outcome:?}");
        assert!(
            log.lock().expect("log").len() >= 9,
            "every hung request must be cut off and the next one made"
        );
    }

    // -----------------------------------------------------------------
    // A slow but HEALTHY apiserver, on tokio's paused clock
    //
    // Everything above either answers at once or never. What a busy
    // single-node control plane actually does is answer every request
    // correctly and late: kine on sqlite on a small VDS can take seconds to
    // commit a write under disk pressure. Slowness is not a failure, and
    // the loop must not turn it into one: a leader that steps down over it
    // restarts into a standby that never acquires, and while that standby
    // keeps its own write landing, nobody else may take the Lease either.
    // -----------------------------------------------------------------

    use std::sync::atomic::AtomicU64;

    /// An apiserver that keeps ONE Lease the way the real one does, and
    /// answers late. A write is checked against `resourceVersion` (the
    /// apiserver's optimistic concurrency) and COMMITTED the moment it
    /// arrives; a read returns what is committed. Only the answer waits, for
    /// the delay currently set for its kind of request.
    #[derive(Default)]
    struct SlowStore {
        lease: Mutex<Option<Value>>,
        version: AtomicU64,
        delays: Mutex<(Duration, Duration)>,
        /// When each write committed, on the tokio clock (the paused clock
        /// does not move `Utc::now()`, so `renewTime` cannot be used).
        commits: Mutex<Vec<Instant>>,
    }

    impl SlowStore {
        fn with_delays(read: Duration, write: Duration) -> Arc<Self> {
            let store = Arc::new(Self::default());
            store.set_delays(read, write);
            store
        }

        fn set_delays(&self, read: Duration, write: Duration) {
            *self.delays.lock().expect("delays") = (read, write);
        }

        fn holder(&self) -> Option<String> {
            self.lease
                .lock()
                .expect("lease")
                .as_ref()
                .and_then(|l| l.pointer("/spec/holderIdentity"))
                .and_then(Value::as_str)
                .map(str::to_string)
        }

        fn commits_since(&self, since: Instant) -> Vec<Instant> {
            let commits = self.commits.lock().expect("commits");
            commits.iter().copied().filter(|t| *t >= since).collect()
        }

        fn commit(&self, mut lease: Value, code: u16) -> (u16, Value) {
            let version = self.version.fetch_add(1, Ordering::SeqCst) + 1;
            lease["metadata"]["resourceVersion"] = json!(version.to_string());
            *self.lease.lock().expect("lease") = Some(lease.clone());
            self.commits.lock().expect("commits").push(Instant::now());
            (code, lease)
        }

        /// The answer to one request, and how long it waits before it is
        /// sent.
        fn serve(&self, method: &str, body: Value) -> ((u16, Value), Duration) {
            let (read, write) = *self.delays.lock().expect("delays");
            let stored = self.lease.lock().expect("lease").clone();
            match (method, stored) {
                ("GET", Some(lease)) => ((200, lease), read),
                ("GET", None) => (lease_not_found(), read),
                ("POST", None) => (self.commit(body, 201), write),
                ("POST", Some(_)) => (conflict("AlreadyExists", 409), write),
                ("PUT", Some(lease)) => {
                    let rv = |v: &Value| v.pointer("/metadata/resourceVersion").cloned();
                    if rv(&body) == rv(&lease) {
                        (self.commit(body, 200), write)
                    } else {
                        (conflict("Conflict", 409), write)
                    }
                }
                ("PUT", None) => (lease_not_found(), write),
                (other, _) => panic!("unexpected {other} on the Lease"),
            }
        }
    }

    /// The apiserver's 409, for a create that finds the Lease already there
    /// (`AlreadyExists`) or a replace carrying a stale `resourceVersion`
    /// (`Conflict`).
    fn conflict(reason: &str, code: u16) -> (u16, Value) {
        (
            code,
            json!({
                "kind": "Status", "apiVersion": "v1", "status": "Failure",
                "message": format!("leases.coordination.k8s.io \"apprafter-operator\": {reason}"),
                "reason": reason, "code": code,
            }),
        )
    }

    fn slow_apiserver(store: Arc<SlowStore>) -> Client {
        let service = tower::service_fn(move |req: http::Request<Body>| {
            let store = store.clone();
            async move {
                let method = req.method().to_string();
                let bytes = req.into_body().collect_bytes().await.expect("request body");
                let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
                let ((code, payload), delay) = store.serve(&method, body);
                tokio::time::sleep(delay).await;
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
        Client::new(service, "apprafter-system")
    }

    /// How long until `flag` reads `want`, polled on the tokio clock for up
    /// to `within`; `None` if it never does.
    async fn reached_within(
        flag: &Arc<AtomicBool>,
        want: bool,
        within: Duration,
    ) -> Option<Duration> {
        let begun = Instant::now();
        while begun.elapsed() <= within {
            if flag.load(Ordering::SeqCst) == want {
                return Some(begun.elapsed());
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        None
    }

    /// Every request answered 3s late; and reads quick while every write
    /// takes 7s, which is longer than a read may take and still well inside
    /// the ten seconds a renewal has before the deadline.
    const SLOW_BUT_HEALTHY: [(Duration, Duration); 2] = [
        (Duration::from_secs(3), Duration::from_secs(3)),
        (Duration::from_millis(200), Duration::from_secs(7)),
    ];

    /// A leader already holding the Lease when the apiserver slows down must
    /// keep holding it — and keep it FRESH — for as long as the slowness
    /// lasts. The five-second bound on a whole GET-plus-PUT step cut every
    /// 3s+3s renewal off after the PUT had already committed, so the Lease
    /// was being renewed while the process counted failures and exited at
    /// its deadline.
    #[tokio::test(start_paused = true)]
    async fn a_leader_keeps_its_lease_through_a_slow_but_healthy_apiserver() {
        for (read, write) in SLOW_BUT_HEALTHY {
            let store = SlowStore::with_delays(Duration::ZERO, Duration::ZERO);
            let cfg = LeaderConfig::for_apprafter_operator("operator-a");
            let le = LeaderElection::new(slow_apiserver(store.clone()), cfg.clone());
            let flag = le.is_leader_handle();
            let task = tokio::spawn(le.run());
            assert!(reaches(&flag, true).await, "never became leader");

            store.set_delays(read, write);
            let slowed = Instant::now();
            tokio::time::sleep(Duration::from_secs(120)).await;

            if task.is_finished() {
                panic!(
                    "read {read:?} / write {write:?}: the leader stopped: {:?}",
                    task.await
                );
            }
            task.abort();
            assert!(flag.load(Ordering::SeqCst), "the leader closed its gate");
            let renewals = store.commits_since(slowed);
            assert!(
                renewals.len() >= 11,
                "read {read:?} / write {write:?}: {} renewals in 120s",
                renewals.len()
            );
            // Never a gap anywhere near the deadline, let alone the Lease.
            let mut last = slowed;
            for at in renewals {
                assert!(
                    at - last < cfg.renew_deadline,
                    "read {read:?} / write {write:?}: {:?} between two renewals",
                    at - last
                );
                last = at;
            }
        }
    }

    /// The restart that follows: a fresh process against the same slow
    /// apiserver must acquire the Lease on its first attempt and go on
    /// leading. Bounded as a whole at five seconds, the 3s+3s acquisition
    /// was cut off after its write had committed: the Lease named this
    /// process, the gate never opened, and every later attempt refreshed the
    /// Lease again — so for as long as the slowness lasted no operator
    /// reconciled and no other replica could take over.
    #[tokio::test(start_paused = true)]
    async fn a_standby_acquires_through_a_slow_but_healthy_apiserver() {
        for (read, write) in SLOW_BUT_HEALTHY {
            let store = SlowStore::with_delays(read, write);
            let cfg = LeaderConfig::for_apprafter_operator("operator-a");
            let le = LeaderElection::new(slow_apiserver(store.clone()), cfg.clone());
            let flag = le.is_leader_handle();
            let task = tokio::spawn(le.run());

            let opened = reached_within(&flag, true, Duration::from_secs(120)).await;
            assert!(
                opened.is_some_and(|after| after <= read + write + Duration::from_secs(1)),
                "read {read:?} / write {write:?}: the gate opened after {opened:?} \
                 (the Lease names {:?})",
                store.holder()
            );
            tokio::time::sleep(Duration::from_secs(120)).await;
            if task.is_finished() {
                panic!(
                    "read {read:?} / write {write:?}: the new leader stopped: {:?}",
                    task.await
                );
            }
            task.abort();
            assert!(flag.load(Ordering::SeqCst));
        }
    }

    /// A write that never answers gets the rest of the leader's window and
    /// not a moment more: cut at the deadline, the leader steps down there
    /// exactly — with the margin before the Lease is takeable intact, and
    /// with nothing in flight that could succeed late and re-open the gate.
    /// Here the renewal's first read hangs too, so the write only starts
    /// five seconds before the deadline, and a bound measured from the start
    /// of the write (rather than from the last renewal) would overrun it.
    #[tokio::test(start_paused = true)]
    async fn a_hung_write_is_cut_at_the_leaders_deadline_and_not_later() {
        let mut reads = 0usize;
        let (client, log) =
            scripted_apiserver(leader_then(move |call| match call.method.as_str() {
                "GET" => {
                    reads += 1;
                    if reads == 1 {
                        hang()
                    } else {
                        (200, lease_json("operator-a", Utc::now(), ago(600)))
                    }
                }
                _ => hang(),
            }));
        let cfg = LeaderConfig::for_apprafter_operator("operator-a");
        let le = LeaderElection::new(client, cfg.clone());
        let flag = le.is_leader_handle();
        let renewed_at = Instant::now();

        let err = tokio::time::timeout(cfg.lease_duration * 3, le.run())
            .await
            .expect("a leader whose write hangs must step down, not wait on it")
            .expect_err("a leader that cannot renew must not report success");
        assert_eq!(renewed_at.elapsed(), cfg.renew_deadline);
        assert!(matches!(err, LeaderError::LostLeadership { .. }), "{err}");
        assert!(!flag.load(Ordering::SeqCst), "the gate must be closed");
        let calls = log.lock().expect("log").clone();
        let writes = calls[2..].iter().filter(|c| c.method == "PUT").count();
        assert_eq!(writes, 1, "{calls:?}");
    }
}
