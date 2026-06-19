// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! kube-rs Controller for v1alpha1 `ResourceClaim` provisioning
//! (Phase 2.4c).
//!
//! The 2.3 scheduler matches each claim to a `ServiceProvider` and
//! records the winner in `status.provider` + a `Scheduled=True`
//! condition. This controller — the provisioner — picks up every
//! claim the scheduler marked `Scheduled=True` and materialises the
//! backing resource:
//!
//!   - dispatch on the matched provider's `spec.backend`; only
//!     `cloudnative-pg` is wired today (2.5/2.6 add jetstream / redis);
//!   - for `cloudnative-pg`: lazily SSA-apply the shared
//!     `platform-postgres` CNPG `Cluster` (created on the first claim,
//!     so a solo cluster with no pg apps pays no Postgres-pod cost),
//!     provision a per-claim Postgres role + database + a basic-auth
//!     password Secret (in the CNPG namespace), and write a connection
//!     Secret with decomposed keys (`url`/`user`/`pass`/…) into the
//!     claim's namespace (2.12 — ADR 0046);
//!   - write `status.ready` + `status.connectionSecretRef` + a `Ready`
//!     condition under its OWN field manager.
//!
//! ## SSA field-manager split (CRITICAL)
//!
//! This controller writes ONLY `status.ready`,
//! `status.connectionSecretRef`, and the `Ready` condition, under field
//! manager `resourceclaim-provisioner`. It NEVER touches
//! `status.provider` or the `Scheduled` condition — those are owned by
//! the scheduler (`resourceclaim-scheduler`). Patching them would fight
//! the scheduler over the same fields. The status patch this controller
//! sends therefore contains ONLY the provisioner's own keys.
//!
//! ## Cleanup scope (2.4f — RetainedClaim snapshot + 7-day grace GC)
//!
//! The connection Secret in the claim's namespace carries an
//! `ownerReference` to the `ResourceClaim`, so it cascades on claim
//! delete with no finalizer logic of its own. The per-claim role +
//! database + password Secret in the shared cluster are NOT dropped
//! immediately: on delete the provisioner finalizer ([`reconcile`])
//! snapshots the claim into an immutable `RetainedClaim` in
//! `apprafter-system` (`retainUntil = deletion + 7d`) BEFORE removing
//! itself, and the [`gc`] Controller (the 7th controller) drops the
//! role (RMW `spec.managed.roles` via `cnpg::remove_role`), the
//! database (`spec.ensure: absent` — CNPG drops it; deleting the CR
//! would not, because the Postgres reclaim default is `retain`), the
//! password Secret, and finally the snapshot once `retainUntil` passes.
//! The 7-day timer is exercised via injected-clock unit tests
//! ([`grace`]) — no real wait.

use std::sync::Arc;

use futures::StreamExt;
use kube::api::Api;
use kube::runtime::controller::{Config as ControllerConfig, Controller};
use kube::runtime::watcher;
use kube::Client;
use thiserror::Error;
use tracing::{info, warn};

use operator_core::{Metrics, ResourceClaim};

pub mod acl_reconcile;
pub mod cnpg;
pub mod disk;
pub mod dragonfly;
pub mod gc;
pub mod grace;
pub mod reconcile;
pub mod redis_client;
pub mod shared_volume;

use redis_client::{RedisAdmin, RedisClient};

pub(crate) const KIND: &str = "ResourceClaim";

/// SSA field manager for everything this controller owns
/// (`status.ready` / `status.connectionSecretRef` / the `Ready`
/// condition, plus the connection Secret + the lazily-created CNPG
/// `Cluster` / `Database`). Distinct from `resourceclaim-scheduler` so
/// the scheduler and this controller never fight over `ResourceClaim`
/// status fields.
pub const FIELD_MANAGER: &str = "resourceclaim-provisioner";

/// Finalizer the provisioner installs so deletes are observed. On
/// delete it only logs (role/DB retained for 2.4f) and self-removes.
pub(crate) const PROVISIONER_FINALIZER: &str = "apprafter.io/resourceclaim-provisioner";

/// Per-controller reconcile context.
pub struct Context {
    pub client: Client,
    pub metrics: Arc<Metrics>,
    /// Imperative Redis admin seam for the dragonfly backend (per-claim
    /// `$N` ACL users + `FLUSHDB`). Injected so the reconcile + GC logic
    /// is testable with a fake; production is [`RedisClient`]. Unused by
    /// the CNPG path. ADR 0042 §2/§4.
    pub redis: Arc<dyn RedisAdmin>,
}

impl Context {
    /// Construct a [`Context`] with the production [`RedisClient`] seam.
    /// Both controllers ([`run`] + [`gc::run`]) share this so the redis
    /// admin path is wired identically.
    pub fn new(client: Client, metrics: Arc<Metrics>) -> Self {
        Self {
            client,
            metrics,
            redis: Arc::new(RedisClient),
        }
    }
}

#[derive(Debug, Error)]
pub enum ReconcileError {
    #[error("kube api error: {0}")]
    Kube(#[from] kube::Error),
    #[error("serde error: {0}")]
    Serde(#[from] serde_json::Error),
    #[error("provisioning: {0}")]
    Provisioning(String),
}

/// Spawn the ResourceClaim provisioner Controller.
///
/// Watches `apprafter.io/v1alpha1` `ResourceClaim` resources
/// cluster-wide and provisions each one the scheduler marked
/// `Scheduled=True` into its matched backend.
///
/// # No `.watches` on the backend CRs
///
/// The reconcile re-evaluates every claim on a 300s requeue (60s while
/// waiting for the scheduler), which is sufficient to pick up a claim
/// once `status.provider` lands without a watch fan-out over the CNPG
/// `Cluster` / `Database` CRs. A proper watch is future work.
pub async fn run(client: Client, metrics: Arc<Metrics>) -> Result<(), ReconcileError> {
    let claims: Api<ResourceClaim> = Api::all(client.clone());
    let ctx = Arc::new(Context::new(client, metrics));
    info!(
        field_manager = FIELD_MANAGER,
        "ResourceClaimProvisioner starting"
    );
    // Serialize reconciles (concurrency = 1). The dbnum allocator does a
    // read-allocate-write (list live claims → pick the lowest free dbnum →
    // patch status) that is NOT atomic: with the default unbounded
    // concurrency, two claims provisioning onto the same pool instance at the
    // same time both list before either writes its dbnum, so both pick the
    // same number → two tenants pinned to one logical DB (isolation breach).
    // One in-flight reconcile at a time makes each claim's allocation visible
    // (a fresh apiserver list) before the next claim allocates. Leader
    // election already guarantees a single active controller, so this fully
    // serializes allocation. Throughput is a non-issue at claim volumes.
    Controller::new(claims, watcher::Config::default())
        .with_config(ControllerConfig::default().concurrency(1))
        .run(reconcile::reconcile, reconcile::error_policy, ctx)
        .for_each(|res| async move {
            match res {
                Ok((obj_ref, _)) => info!(claim = %obj_ref.name, "provisioned"),
                Err(e) => warn!(error = %e, "provision failed"),
            }
        })
        .await;
    info!("ResourceClaimProvisioner stream ended");
    Ok(())
}

/// Spawn the dragonfly ACL re-pin loop (Phase 2.6-5, ADR 0042 §4).
///
/// Per-claim `$N` ACL users are runtime state on a Dragonfly instance —
/// wiped on a pod restart. This loop periodically re-asserts every live
/// ready dragonfly claim's user (idempotent `ACL SETUSER`, password
/// recovered from the claim's connection-Secret DSN) so an app reconnects
/// without `WRONGPASS`/`NOPERM` after the instance churns. Shares the
/// production [`RedisClient`] seam via [`Context::new`], identical to the
/// provisioner + GC controllers. Unlike them it is NOT a kube-rs
/// `Controller` (there is no clean per-object trigger for "the instance
/// restarted"); it is a simple interval task in the same crate.
pub async fn run_acl_reconcile(
    client: Client,
    metrics: Arc<Metrics>,
) -> Result<(), ReconcileError> {
    let ctx = Arc::new(Context::new(client, metrics));
    acl_reconcile::run(ctx).await
}
