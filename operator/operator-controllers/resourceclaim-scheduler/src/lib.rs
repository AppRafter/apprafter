// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! kube-rs Controller for v1alpha1 `ResourceClaim` (Phase 2.3) —
//! matches each claim to a ServiceProvider by type + label superset and
//! records the winner in `status.provider`. Provisioning is 2.4.

use std::sync::Arc;

use futures::StreamExt;
use kube::api::Api;
use kube::runtime::controller::Controller;
use kube::runtime::watcher;
use kube::Client;
use thiserror::Error;
use tracing::{info, warn};

use operator_core::{Metrics, ResourceClaim};

pub mod reconcile;

pub(crate) const KIND: &str = "ResourceClaim";
pub(crate) const FIELD_MANAGER: &str = "resourceclaim-scheduler";

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
}

/// Spawn the ResourceClaim scheduler Controller.
///
/// Watches `apprafter.io/v1alpha1` `ResourceClaim` resources
/// cluster-wide and reconciles each claim by matching it to a
/// `ServiceProvider` (Phase 2.3) then provisioning a resource
/// (Phase 2.4).
///
/// # ServiceProvider watch omitted — 300 s requeue instead
///
/// When a `ServiceProvider` is created or updated, any Pending claim
/// that now matches should re-evaluate. The idiomatic kube-rs solution
/// is `.watches(Api::<ServiceProvider>, …, mapper)` where `mapper`
/// returns an iterator of `ObjectRef<ResourceClaim>` to re-queue. To
/// enumerate all claims inside that mapper we would need an Arc-wrapped
/// `Store<ResourceClaim>` built from a reflector, which adds meaningful
/// complexity outside the scope of 2.3. Instead, the `reconcile` stub
/// returns `Action::requeue(300s)` so every Pending claim re-evaluates
/// at most 5 minutes after a provider becomes available. A proper
/// provider-watch fan-out is deferred to a future sub-task.
pub async fn run(client: Client, metrics: Arc<Metrics>) -> Result<(), ReconcileError> {
    let claims: Api<ResourceClaim> = Api::all(client.clone());
    let ctx = Arc::new(Context { client, metrics });
    info!(
        field_manager = FIELD_MANAGER,
        "ResourceClaimScheduler starting"
    );
    // No ServiceProvider watch in 2.3 — Pending claims re-evaluate on
    // the 300s requeue; a provider-watch fan-out is future work.
    Controller::new(claims, watcher::Config::default())
        .run(reconcile::reconcile, reconcile::error_policy, ctx)
        .for_each(|res| async move {
            match res {
                Ok((obj_ref, _)) => info!(claim = %obj_ref.name, "reconciled"),
                Err(e) => warn!(error = %e, "reconcile failed"),
            }
        })
        .await;
    info!("ResourceClaimScheduler stream ended");
    Ok(())
}
