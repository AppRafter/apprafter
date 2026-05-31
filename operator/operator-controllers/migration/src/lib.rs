// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! `MigrationController` — third reconciler in the
//! `apprafter-operator` binary (peer to ApplicationController
//! and PlatformController).
//!
//! Drives the FSM for v1alpha1 `MigrationPlan` CRs:
//!
//! ```text
//!     pending-approval
//!       │
//!       │  external: phase=approved   external: phase=rejected
//!       ▼                              (platform scope only)
//!     approved ──→ executing ──→ completed         rejected
//!                                  │
//!                                  ▼
//!                                failed
//! ```
//!
//! See spec.md §3.8 + ADR 0027 + plan.md §1.76.
//!
//! Detection (`detect_destructive`) is NOT here — that lives
//! on each strategy's concrete fn signature and is called by
//! ApplicationController (B.1.77) and PlatformController
//! (B.1.78). This crate ships the trait + impls' execute/reject
//! halves only.

pub mod reconcile;
pub mod strategy;

pub use reconcile::{run, FIELD_MANAGER};
pub use strategy::{
    ApplicationMigrationStrategy, PlatformMigrationStrategy, SourceCredentialMigrationStrategy,
};
