// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Shared `MigrationStrategy` trait + supporting types for the
//! MigrationController (B.1.76).
//!
//! Two strategies live in the
//! `operator-controllers/migration` crate: `ApplicationMigrationStrategy`
//! and `PlatformMigrationStrategy`. They share the
//! per-plan-step execution + reject paths through this trait
//! so MigrationController can dispatch on `scope.type` at
//! runtime via `Box<dyn MigrationStrategy>`.
//!
//! Per-scope detection (`detect_destructive`) is NOT in the
//! trait — Application detection takes an `ApplicationSpec`
//! diff, platform detection takes version strings +
//! compatibility metadata. Forcing both through a single
//! `&dyn Context` would either erase the type information
//! callers need or introduce an associated type that breaks
//! trait-object dispatch. Detection therefore lives as
//! concrete fns on each strategy struct; B.1.77 and B.1.78
//! wire them in at the call sites.

use async_trait::async_trait;
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{MigrationPlan, MigrationStep};

/// Outcome of running a single `MigrationStep`. The
/// MigrationController appends these to `status.executedSteps`
/// after each call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StepOutcome {
    Succeeded,
    Failed { message: String },
    Skipped { reason: String },
}

/// What detection produced when it found a destructive change.
/// Mirrors the structured fields the MigrationPlan CRD's
/// `spec.trigger` + `spec.risks.classification` carry — the
/// strategy's `create_plan_for` helper rolls a Plan out of this.
///
/// Detection (`detect_destructive`) lives as a concrete fn on
/// each strategy struct, NOT in the `MigrationStrategy` trait —
/// per-scope signatures differ (Application takes an
/// `ApplicationSpec` diff; platform takes version strings +
/// compatibility data). See `migration::MigrationStrategy` doc
/// comment for the rationale.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DestructiveChange {
    /// Free-form trigger kind. Examples: `"selector-change"`,
    /// `"storage-class-change"`, `"major-version-upgrade"`,
    /// `"platform-classification"`.
    pub trigger_type: String,
    /// JSON Pointer-ish path of the field whose change triggered
    /// the plan. Example: `"needs.pg.selector"`, `"spec.pin"`.
    pub field: String,
    /// Old value, serialised as JSON. Free-form because trigger
    /// kinds span heterogeneous types.
    pub from: Option<serde_json::Value>,
    /// New value, serialised as JSON.
    pub to: Option<serde_json::Value>,
    /// One of `"safe" | "requires-restart" | "data-migration" | "breaking"
    /// | "security-boundary"`. Mirrors the platform-stack compatibility
    /// classification vocabulary (plus the app-scope `security-boundary`
    /// class, 2.16b S1.1) so users see consistent terminology across scopes.
    pub classification: String,
}

#[derive(Debug, Error)]
pub enum MigrationError {
    #[error("kube API error: {0}")]
    Kube(#[from] kube::Error),
    #[error(
        "MigrationPlan {0:?} has no spec.previousSpecSnapshot — \
             reject cannot revert without a snapshot"
    )]
    NoSnapshot(String),
    #[error("malformed previousSpecSnapshot in MigrationPlan {0:?}: {1}")]
    SnapshotShape(String, String),
    #[error("strategy backend error: {0}")]
    Backend(String),
}

/// Ordinal severity of a MigrationPlan classification. Higher = more
/// destructive. Used to pick ONE primary DestructiveChange when an edit
/// carries several destructive ops (2.16b spec: highest-severity wins).
pub fn classification_severity(classification: &str) -> u8 {
    match classification {
        // 2.16b S1.1: a change that alters the app's security boundary (e.g.
        // moving the image to a different repository / pull source) is the most
        // severe class — it OUTRANKS a data-migration so it wins primary in a
        // multi-op edit.
        "security-boundary" => 4,
        "data-migration" => 3,
        "breaking" => 2,
        "requires-restart" => 1,
        _ => 0, // "safe" / unknown
    }
}

/// 2.16b S-4: a stable, content-sensitive hash of the destructive
/// change(s) a spec edit produced. Used to BIND an app-scope
/// MigrationPlan approval to the *exact* change it was approved for,
/// so an approval is never transferable across a different spec edit.
///
/// Without this binding, `plan_state`'s trigger-match compared only the
/// `(trigger_type, field)` tuple — never the `from`/`to` CONTENT — so an
/// approval for `replicas 2->0` would consume against a DIFFERENT
/// `replicas 1->0`. For a security-boundary op that fully defeats the
/// gate (approve a benign `from->to`, swap the payload before consume).
/// The stamped hash is re-verified at consume time; a mismatch demotes
/// the completed plan to a relic and re-gates the edit as a fresh
/// pending-approval plan.
///
/// Determinism & collision-freedom: each change is canonicalised to a
/// JSON array `[trigger_type, field, from, to]` where `from`/`to` are the
/// RAW `Option<Value>` (NOT flattened to a string) — this preserves the
/// JSON TYPE (a string `"2"` and a number `2` encode distinctly) and lets
/// serde escape every special character, so no `from`/`to` payload can
/// smuggle a separator to collide two distinct changes (e.g. from=`a`,
/// to=`b|c` vs from=`a|b`, to=`c`). The per-change arrays are collected,
/// SORTED by their own `serde_json::to_string()` form (so the hash is
/// independent of candidate discovery/push order), wrapped in a JSON
/// array, serialised once, and SHA-256'd to a lowercase hex string.
pub fn change_hash(changes: &[DestructiveChange]) -> String {
    let mut items: Vec<serde_json::Value> = changes
        .iter()
        .map(|c| serde_json::json!([c.trigger_type, c.field, c.from, c.to]))
        .collect();
    // Sort deterministically by the canonical JSON form of each element so
    // candidate order does not affect the hash. `to_string` on a
    // serde_json::Value is infallible.
    items.sort_by_key(|v| v.to_string());
    let array = serde_json::Value::Array(items);
    // Serialising a `Value` never fails.
    let canonical = serde_json::to_string(&array).expect("serialising a JSON array cannot fail");
    // Hex by hand rather than `format!("{:x}", …)`: sha2 0.11 moved to
    // hybrid-array, whose `Array` does not implement `LowerHex`. The BYTES are
    // unchanged — this is still SHA-256 of the same canonical JSON — and the
    // rendering stays lowercase, unseparated, 64 chars, because this string is
    // the ADR 0052 approval content hash that a human compares by eye and that
    // migration_state matches a completed plan against.
    let digest = Sha256::digest(canonical.as_bytes());
    let mut hex = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write as _;
        // Writing to a String is infallible.
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

/// Per-scope behaviour shared between application + platform
/// strategies. Trait-object friendly (no associated types, no
/// generics).
///
/// `execute_step` runs ONE step from `plan.spec.plan[]`. Steps
/// are free-form text in 1.75 / 1.76 — no machine semantics —
/// so the default strategy impl just marks them `Succeeded`.
/// B.1.77 and beyond can replace the impls with real action
/// runners.
///
/// `reject` is invoked when MigrationController observes
/// `status.phase = rejected`. Application-scope MUST be a
/// no-op per ADR 0027 (the user reverts the Git commit
/// instead); platform-scope reverts `PlatformStack.spec.pin`
/// from `plan.spec.previousSpecSnapshot`.
#[async_trait]
pub trait MigrationStrategy: Send + Sync {
    async fn execute_step(
        &self,
        plan: &MigrationPlan,
        step: &MigrationStep,
    ) -> Result<StepOutcome, MigrationError>;

    async fn reject(&self, plan: &MigrationPlan) -> Result<(), MigrationError>;
}

#[cfg(test)]
mod tests {
    use super::classification_severity;

    // ---- change_hash: the ADR 0052 approval content hash ----
    //
    // Added during the 2026-09 sha2 0.10 -> 0.11 bump. sha2 0.11 dropped
    // `LowerHex` on the digest output, so the hex rendering had to be
    // rewritten by hand — and NOTHING pinned this value, even though a change
    // to it silently invalidates every in-flight approval: migration_state
    // matches a completed plan against the hash a human approved
    // (`completed_plan_consumes_only_on_matching_content_hash`).
    //
    // The expected literal is NOT whatever this code happens to emit. It was
    // computed independently, outside Rust:
    //
    //   printf '%s' '[["selector-change","needs.pg.selector","old","new"]]' \
    //     | sha256sum
    //
    // so the assertion covers BOTH halves at once — that the canonical JSON is
    // what the doc comment says it is, and that the hex encoding is unchanged.

    #[test]
    fn change_hash_matches_an_independently_computed_sha256() {
        let changes = vec![super::DestructiveChange {
            trigger_type: "selector-change".to_string(),
            field: "needs.pg.selector".to_string(),
            from: Some(serde_json::Value::String("old".to_string())),
            to: Some(serde_json::Value::String("new".to_string())),
            // NOT part of the canonical form — the hash covers
            // [trigger_type, field, from, to] only.
            classification: "data-migration".to_string(),
        }];
        assert_eq!(
            super::change_hash(&changes),
            "317d6715ef9137b8c113d5c6cce330ead87aab9a63fddc3d88aa00f121720030",
            "the approval content hash moved — every in-flight approval a human \
             already signed off is invalidated by this"
        );
    }

    #[test]
    fn change_hash_is_lowercase_hex_of_exactly_64_chars() {
        let changes = vec![super::DestructiveChange {
            trigger_type: "t".to_string(),
            field: "f".to_string(),
            from: None,
            to: None,
            classification: "safe".to_string(),
        }];
        let h = super::change_hash(&changes);
        assert_eq!(h.len(), 64, "expected 64 hex chars, got {h:?}");
        assert!(
            h.chars()
                .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c)),
            "the hash must render as LOWERCASE hex with no separators: {h:?}"
        );
    }

    #[test]
    fn severity_orders_data_migration_above_requires_restart() {
        assert!(
            classification_severity("data-migration") > classification_severity("requires-restart")
        );
        assert!(classification_severity("requires-restart") > classification_severity("safe"));
        // unknown classifications sort lowest (defensive)
        assert_eq!(classification_severity("bogus"), 0);
    }

    // 2.16b S1.1: the `security-boundary` class is the most severe — a change
    // that alters the app's security boundary (e.g. moving the image to a
    // different repository / pull source) OUTRANKS a data-migration, so in a
    // multi-op edit it becomes the plan's primary.
    #[test]
    fn security_boundary_outranks_data_migration() {
        assert_eq!(classification_severity("security-boundary"), 4);
        assert!(
            classification_severity("security-boundary")
                > classification_severity("data-migration")
        );
    }
}
