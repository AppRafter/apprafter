// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Reconcile loop for the `ResourceClaim` provisioner (Phase 2.4c).
//!
//! Stub — the real flow is implemented in Task 3.

use std::sync::Arc;
use std::time::Duration;

use kube::runtime::controller::Action;
use kube::ResourceExt;
use tracing::warn;

use operator_core::ResourceClaim;

use crate::{Context, ReconcileError, KIND};

/// Reconcile a single `ResourceClaim` (stub — Task 3 fills this in).
pub async fn reconcile(
    _claim: Arc<ResourceClaim>,
    _ctx: Arc<Context>,
) -> Result<Action, ReconcileError> {
    Ok(Action::requeue(Duration::from_secs(60)))
}

/// Error policy: increment error metrics and requeue after 30 seconds.
pub fn error_policy(claim: Arc<ResourceClaim>, err: &ReconcileError, ctx: Arc<Context>) -> Action {
    let name = claim.name_any();
    let namespace = claim.namespace().unwrap_or_default();
    warn!(%name, %namespace, %err, "resourceclaim provisioner reconcile error");
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
