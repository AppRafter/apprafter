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
//!     Secret carrying `DATABASE_URL` into the claim's namespace;
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
//! ## Cleanup scope (SKELETON — 2.4f grafts the real GC)
//!
//! The connection Secret in the claim's namespace carries an
//! `ownerReference` to the `ResourceClaim`, so it cascades on claim
//! delete with no finalizer logic of its own. The per-claim role +
//! database in the shared cluster are NOT dropped on delete — they are
//! retained until 2.4f wires the RetainedClaim snapshot + 7-day grace
//! GC. The provisioner finalizer this controller installs only logs
//! "role/DB retained pending 2.4f GC" on delete and removes itself,
//! establishing the wiring 2.4f grafts onto.

use std::sync::Arc;

use futures::StreamExt;
use kube::api::Api;
use kube::runtime::controller::Controller;
use kube::runtime::watcher;
use kube::Client;
use thiserror::Error;
use tracing::{info, warn};

use operator_core::{Metrics, ResourceClaim};

pub mod cnpg;
pub mod grace;
pub mod reconcile;

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
    let ctx = Arc::new(Context { client, metrics });
    info!(
        field_manager = FIELD_MANAGER,
        "ResourceClaimProvisioner starting"
    );
    Controller::new(claims, watcher::Config::default())
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
