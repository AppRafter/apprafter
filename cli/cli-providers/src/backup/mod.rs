// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! 2.6d backup/restore: native export + restic-wrapped local-pull backup +
//! restore. CLI-orchestrated; cluster-scoped by default.

pub mod images;
pub mod manifest;
pub mod restic;
pub mod sanitize;

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
