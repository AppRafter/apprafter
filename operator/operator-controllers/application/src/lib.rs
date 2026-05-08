// SPDX-License-Identifier: FSL-1.1-MIT
//! kube-rs Controller for the v1alpha1 `Application` CRD.
//!
//! v0.1.26 ships only the reconcile + error_policy + Context types;
//! the actual `run()` that spins up the Controller and watches the
//! API lands in v0.1.27 along with the apprafter-operator binary.
//! Phase 1.9 replaces the reconcile stub with the real render →
//! server-side-apply flow.

use std::sync::Arc;
use std::time::Duration;

use kube::runtime::controller::Action;
use kube::Client;
use kube::ResourceExt;
use thiserror::Error;
use tracing::{info, warn};

use operator_core::Application;

/// Per-controller reconcile context. v0.1.26 only carries the kube
/// client; v0.1.27 adds a Metrics handle, and phase 1.9 adds a
/// server-side-apply field manager string.
pub struct Context {
    pub client: Client,
}

#[derive(Debug, Error)]
pub enum ReconcileError {
    #[error("kube-rs error: {0}")]
    Kube(#[from] kube::Error),
}

/// Reconcile fn — v0.1.26 stub: logs the observed Application and
/// requeues every 60 seconds. Phase 1.9 replaces this with the
/// actual render → server-side-apply logic.
pub async fn reconcile(
    app: Arc<Application>,
    _ctx: Arc<Context>,
) -> Result<Action, ReconcileError> {
    let name = app.name_any();
    let namespace = app.namespace().unwrap_or_default();
    info!(%name, %namespace, "reconciling Application");
    Ok(Action::requeue(Duration::from_secs(60)))
}

/// Error policy — v0.1.26 stub: logs the error and requeues with a
/// fixed 30s delay. Phase 1.9 will distinguish transient vs terminal
/// errors and wire up backoff.
pub fn error_policy(
    app: Arc<Application>,
    err: &ReconcileError,
    _ctx: Arc<Context>,
) -> Action {
    let name = app.name_any();
    let namespace = app.namespace().unwrap_or_default();
    warn!(%name, %namespace, %err, "reconcile error");
    Action::requeue(Duration::from_secs(30))
}
