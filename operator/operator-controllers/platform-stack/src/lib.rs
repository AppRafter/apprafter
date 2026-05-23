// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! kube-rs Controller for the v1alpha1 `PlatformStack` CRD.
//!
//! Lives in the same `apprafter-operator` binary as the
//! Application controller (per Track B.1.73 design note, session
//! 2026-05-20). Watches the singleton `PlatformStack/default` CR
//! in `apprafter-system`, computes the desired Application
//! `spec.source.{targetRevision, helm.valuesObject}`, and SSA-
//! patches the parent `platform` Application in `argocd` with
//! field manager `platform-controller`.
//!
//! Cascade to child Applications is Argo CD's responsibility —
//! the controller never touches children directly.

use std::sync::Arc;

use kube::Client;
use operator_core::Metrics;
use tracing::info;

pub mod compatibility;
pub mod desired;
pub mod oci;
pub mod reconcile;
pub mod status;

/// SSA field manager for every patch this controller emits. The
/// Argo CD application-controller uses a different field manager
/// (`argocd-application-controller`) for writes to `.status`, so
/// the two writers do not conflict on disjoint subtrees. Any
/// non-`platform-controller` write to `spec.source.targetRevision`
/// or `spec.source.helm.valuesObject` is treated as an unauthorized
/// modification (see `reconcile::detect_outside_writer`).
pub const FIELD_MANAGER: &str = "platform-controller";

/// PlatformStack singleton coordinates per webhook contract
/// (1.72). The reconciler ignores any other CR.
pub const SINGLETON_NAME: &str = "default";
pub const SINGLETON_NAMESPACE: &str = "apprafter-system";

/// Spawn the PlatformStack controller. Returns when the
/// underlying kube-rs Controller stream ends (which happens on
/// API-server disconnect or process shutdown).
pub async fn run(client: Client, metrics: Arc<Metrics>) -> Result<(), reconcile::Error> {
    info!(
        ns = SINGLETON_NAMESPACE,
        name = SINGLETON_NAME,
        field_manager = FIELD_MANAGER,
        "PlatformController starting"
    );
    let result = reconcile::run(client, metrics).await;
    info!("PlatformController stream ended");
    result
}
