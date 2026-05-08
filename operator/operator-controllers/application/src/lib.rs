// SPDX-License-Identifier: FSL-1.1-MIT
//! kube-rs Controller for the v1alpha1 `Application` CRD.
//!
//! v0.1.27 wires up `run()` which spawns the Controller against the
//! supplied `kube::Client` + `Metrics`. The reconcile fn still
//! delegates to a stub renderer (phase 1.9 fills in the real
//! Deployment / Service / HTTPRoute logic).

use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use kube::runtime::controller::{Action, Controller};
use kube::runtime::watcher;
use kube::{Api, Client, ResourceExt};
use thiserror::Error;
use tracing::{info, warn};

use operator_core::{Application, Metrics};

/// Resource kind label used for every metric tagged with `kind`.
const KIND: &str = "Application";

/// Per-controller reconcile context.
pub struct Context {
    pub client: Client,
    pub metrics: Arc<Metrics>,
}

#[derive(Debug, Error)]
pub enum ReconcileError {
    #[error("kube-rs error: {0}")]
    Kube(#[from] kube::Error),
}

/// Spawn the Application Controller. Watches `apprafter.io/v1alpha1`
/// `Application` resources cluster-wide and reconciles them through
/// [`reconcile`]. Errors from individual reconcile calls go through
/// [`error_policy`].
///
/// Returns when the Controller's stream completes (typically only
/// on cluster-side disconnects); v0.1.27 callers keep it running
/// inside a tokio task.
pub async fn run(client: Client, metrics: Arc<Metrics>) -> Result<(), ReconcileError> {
    let apps: Api<Application> = Api::all(client.clone());
    let context = Arc::new(Context { client, metrics });

    Controller::new(apps, watcher::Config::default())
        .run(reconcile, error_policy, context)
        .for_each(|res| async move {
            match res {
                Ok((obj_ref, _action)) => {
                    info!(?obj_ref, "controller step ok");
                }
                Err(err) => {
                    warn!(%err, "controller step error");
                }
            }
        })
        .await;
    Ok(())
}

/// Reconcile fn — v0.1.27 stub instrumented with metrics. Phase 1.9
/// replaces the no-op body with the real render → server-side-apply
/// flow.
pub async fn reconcile(
    app: Arc<Application>,
    ctx: Arc<Context>,
) -> Result<Action, ReconcileError> {
    let name = app.name_any();
    let namespace = app.namespace().unwrap_or_default();
    let _timer = ctx
        .metrics
        .reconcile_duration
        .with_label_values(&[KIND])
        .start_timer();

    info!(%name, %namespace, "reconciling Application");

    // Phase 1.9 fills in the actual logic here.

    ctx.metrics
        .reconcile_total
        .with_label_values(&[KIND, &namespace, "ok"])
        .inc();
    Ok(Action::requeue(Duration::from_secs(60)))
}

/// Error policy — logs the error, increments the error counters,
/// and requeues with a fixed 30s delay. Phase 1.9 will distinguish
/// transient vs terminal errors and wire up exponential backoff.
pub fn error_policy(
    app: Arc<Application>,
    err: &ReconcileError,
    ctx: Arc<Context>,
) -> Action {
    let name = app.name_any();
    let namespace = app.namespace().unwrap_or_default();
    warn!(%name, %namespace, %err, "reconcile error");
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
