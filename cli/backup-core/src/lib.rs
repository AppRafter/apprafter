// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Execution-agnostic backup engine shared by the CLI (kubectl+restic
//! subprocess) and the in-cluster runner (kube-rs). See
//! docs/superpowers/specs/2026-07-16-2-6d-4-s3-push-design.md.

pub mod cluster;
pub mod engine;
pub mod extract;
pub mod helper_pod;
pub mod images;
pub mod kube;
pub mod manifest;
pub mod prune;
pub mod reseal;
pub mod restic;
pub mod restic_runner;
pub mod restore;
pub mod sanitize;
/// Pure crypto helpers for sealed-secrets reseal (no kubectl dependency).
pub mod sealing;

pub use engine::StagingMode;
pub use kube::KubeExec;
pub use restic_runner::ResticRunner;
pub use restic_runner::SubprocessRestic;

/// The native data kinds an extraction pulls.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DataKind {
    Pg,
    Redis,
    Volume,
    /// One JetStream stream, dumped over the NATS wire (2.6d-6).
    JetStream,
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
    /// A6. `true` marks a ResourceClaim that IS in this backup while its DATA
    /// is not: the claim comes back on a restore, empty.
    ///
    /// The claim is listed rather than dropped because the restore genuinely
    /// replays it — what would be dishonest is listing it with no way to tell
    /// it apart from a claim whose data is there. Written by
    /// [`extract::claim_type_has_no_data_capture`], so the manifest carries the
    /// statement rather than leaving every reader to know the rule.
    ///
    /// Absent (= `false`) on config CRs and on every claim whose data is
    /// captured, which is also how a manifest written before this field reads.
    #[serde(default, skip_serializing_if = "is_false")]
    pub no_data: bool,
}

/// `skip_serializing_if` for a `bool` that means "not the ordinary case": keeps
/// the manifest free of a key on every resource that has nothing to declare.
fn is_false(b: &bool) -> bool {
    !*b
}
