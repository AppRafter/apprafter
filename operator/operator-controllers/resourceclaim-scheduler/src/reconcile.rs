// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Reconcile loop for the `ResourceClaim` scheduler controller.
//!
//! **Task 2.3 stub** — enough wiring to compile and link.  The full
//! matching + status-patch logic lands in Task 4.

use std::sync::Arc;
use std::time::Duration;

use kube::runtime::controller::Action;
use kube::ResourceExt;
use tracing::{info, warn};

use operator_core::ResourceClaim;

use crate::{Context, ReconcileError, KIND};

/// Reconcile a single `ResourceClaim`.
///
/// Currently a stub: reads the claim name/namespace for logging and
/// returns a 300-second requeue so Pending claims re-evaluate
/// periodically while we wait for a matching `ServiceProvider` to
/// appear.  The full match→status-patch body is implemented in Task 4.
pub async fn reconcile(
    claim: Arc<ResourceClaim>,
    ctx: Arc<Context>,
) -> Result<Action, ReconcileError> {
    let name = claim.name_any();
    let namespace = claim.namespace().unwrap_or_default();
    let _timer = ctx
        .metrics
        .reconcile_duration
        .with_label_values(&[KIND])
        .start_timer();

    info!(%name, %namespace, "reconciling ResourceClaim (2.3 stub)");

    ctx.metrics
        .reconcile_total
        .with_label_values(&[KIND, &namespace, "ok"])
        .inc();

    // Re-evaluate every 5 minutes so a Pending claim picks up a newly
    // created ServiceProvider without requiring a watch fan-out.
    // No ServiceProvider watch in 2.3 — Pending claims re-evaluate on
    // the 300 s requeue; a provider-watch fan-out is future work.
    Ok(Action::requeue(Duration::from_secs(300)))
}

/// Error policy: increment error metrics and requeue after 30 seconds.
pub fn error_policy(claim: Arc<ResourceClaim>, err: &ReconcileError, ctx: Arc<Context>) -> Action {
    let name = claim.name_any();
    let namespace = claim.namespace().unwrap_or_default();
    warn!(%name, %namespace, %err, "resourceclaim reconcile error");
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
