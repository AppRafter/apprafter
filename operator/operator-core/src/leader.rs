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
}
