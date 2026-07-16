// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Execution-agnostic backup engine shared by the CLI (kubectl+restic
//! subprocess) and the in-cluster runner (kube-rs). See
//! docs/superpowers/specs/2026-07-16-2-6d-4-s3-push-design.md.

pub mod images;
pub mod manifest;
pub mod reseal;
pub mod restic;
pub mod restore;
pub mod sanitize;
/// Pure crypto helpers for sealed-secrets reseal (no kubectl dependency).
pub mod sealing;

/// The native data kinds an extraction pulls.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DataKind {
    Pg,
    Redis,
    Volume,
}

/// A resource captured into the backup manifest.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ResourceRef {
    pub namespace: String,
    pub kind: String,
    pub name: String,
    /// For ResourceClaims / data artifacts: the claim type (pg/redis/disk/shared-disk). None for config CRs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claim_type: Option<String>,
}
