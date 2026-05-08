// SPDX-License-Identifier: FSL-1.1-MIT
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
                    if consecutive_failures >= RENEWAL_FAILURE_BUDGET
                        && self.is_leader.load(Ordering::SeqCst)
                    {
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
                if holder == Some(self.config.holder_id.as_str()) || stale {
                    let mut updated = existing.clone();
                    updated.spec = Some(self.lease_spec(now, existing.spec.as_ref()));
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
                    spec: Some(self.lease_spec(now, None)),
                };
                api.create(&PostParams::default(), &lease).await?;
                Ok(true)
            }
        }
    }

    fn lease_spec(&self, now: DateTime<Utc>, prior: Option<&LeaseSpec>) -> LeaseSpec {
        let acquire_time = prior
            .and_then(|p| p.acquire_time.clone())
            .unwrap_or(MicroTime(now));
        LeaseSpec {
            holder_identity: Some(self.config.holder_id.clone()),
            lease_duration_seconds: Some(self.config.lease_duration.as_secs() as i32),
            acquire_time: Some(acquire_time),
            renew_time: Some(MicroTime(now)),
            ..LeaseSpec::default()
        }
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
}
