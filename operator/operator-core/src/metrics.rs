// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Prometheus metrics for the AppRafter operator.
//!
//! Four signal-grade metrics:
//!   - apprafter_reconcile_total{kind,namespace,result} — every
//!     reconcile call increments one of {ok, error}.
//!   - apprafter_reconcile_duration_seconds{kind} — histogram of
//!     wall-time per reconcile.
//!   - apprafter_reconcile_errors_total{kind} — error-only counter
//!     for quick "errors per minute" alerts.
//!   - apprafter_claim_unmatched_total{kind,namespace,reason} —
//!     ResourceClaims with no matching ServiceProvider.
//!   - apprafter_claim_gc_total{result,namespace} — RetainedClaim
//!     GC sweeps (role/DB/Secret drop after the 7-day grace), by
//!     result {success, error}.
//!   - apprafter_soft_destructive_total{trigger,namespace} — soft
//!     (un-gated but potentially-disruptive) Application edits that
//!     rolled through, by trigger label (2.16b S7.1).
//!   - apprafter_claim_retained_total{backend,namespace} —
//!     RetainedClaim snapshots created on claim delete, by backend
//!     (2.16b S3 — ends the silent RetainedClaim creation).
//!   - apprafter_migration_regate_total{reason,namespace} — app-scope
//!     MigrationPlans re-gated at consume time because their approval
//!     hash failed, by reason {HashMissing, HashMismatch} (2.16b-sec
//!     N-3 — surfaces a stale/forged approval re-gate + measures any
//!     hashless tail in the wild).
//!   - apprafter_shared_backend_reap_total{backend,result} —
//!     shared-backend reaper decisions (ADR 0042 §9). `backend` ∈
//!     {dragonfly-ephemeral, dragonfly-persistent, cnpg, unknown};
//!     `result` ∈ {reaped, dwelling, veto_live, veto_intent,
//!     veto_retained, veto_uid_conflict, veto_owner_reattached,
//!     veto_unverified, error}. The full label domain is declared here
//!     even though the Dragonfly and CNPG arms each emit only part of
//!     it — `veto_owner_reattached` and `veto_unverified` are CNPG-only
//!     (its PVC ownerReference strip, §9.3) and `unknown`/`error` mark
//!     a sweep that failed before it could attribute a backend.
//!     The two strip results are DELIBERATELY separate and must not be
//!     merged back: `veto_owner_reattached` means the reference was
//!     observed BACK on the PVC after the strip — a real claim about
//!     CNPG's behaviour, and the signal that §9.3's measured "CNPG does
//!     not re-add it" has stopped holding on some chart version.
//!     `veto_unverified` means the reaper could not READ well enough to
//!     say either way (a truncated LIST, or a LIST/PATCH that errored).
//!     Both block the delete identically, but only the first is
//!     evidence about the database operator; counting a read failure as
//!     a reattachment would make the metric lie in exactly the
//!     debugging session where the difference matters.
//!     NOTE the results have MIXED semantics: `reaped` and
//!     `veto_uid_conflict` count EVENTS, while `dwelling` and every
//!     `veto_*` are per-tick SAMPLES — the sweep increments one per
//!     instance per tick regardless of whether anything changed. So
//!     `rate(...{result="veto_live"}[5m])` measures tick-rate ×
//!     instance-count, not a rate of vetoes; alert on `reaped`, and
//!     read the sample series as "how many instances were in this
//!     state", not "how often this happened".
//!
//! Metrics are registered into a single `Registry` that the HTTP
//! `/metrics` handler in `apprafter-operator` encodes.

use prometheus::{histogram_opts, opts, CounterVec, Encoder, HistogramVec, Registry, TextEncoder};

pub struct Metrics {
    pub registry: Registry,
    pub reconcile_total: CounterVec,
    pub reconcile_duration: HistogramVec,
    pub reconcile_errors: CounterVec,
    pub claim_unmatched_total: CounterVec,
    pub claim_provisioned_total: CounterVec,
    pub claim_gc_total: CounterVec,
    pub image_resolve_total: CounterVec,
    pub soft_destructive_total: CounterVec,
    pub claim_retained_total: CounterVec,
    pub migration_regate_total: CounterVec,
    pub shared_backend_reap_total: CounterVec,
}

impl Default for Metrics {
    fn default() -> Self {
        Self::new()
    }
}

impl Metrics {
    pub fn new() -> Self {
        let registry = Registry::new();

        let reconcile_total = CounterVec::new(
            opts!(
                "apprafter_reconcile_total",
                "Total reconcile calls by kind, namespace, and result"
            ),
            &["kind", "namespace", "result"],
        )
        .expect("CounterVec must build with a non-empty name");

        let reconcile_duration = HistogramVec::new(
            histogram_opts!(
                "apprafter_reconcile_duration_seconds",
                "Reconcile call duration by kind"
            ),
            &["kind"],
        )
        .expect("HistogramVec must build with a non-empty name");

        let reconcile_errors = CounterVec::new(
            opts!(
                "apprafter_reconcile_errors_total",
                "Reconcile error counter by kind"
            ),
            &["kind"],
        )
        .expect("CounterVec must build with a non-empty name");

        let claim_unmatched_total = CounterVec::new(
            opts!(
                "apprafter_claim_unmatched_total",
                "ResourceClaims with no matching ServiceProvider, by kind, namespace, and reason"
            ),
            &["kind", "namespace", "reason"],
        )
        .expect("CounterVec must build with a non-empty name");

        let claim_provisioned_total = CounterVec::new(
            opts!(
                "apprafter_claim_provisioned_total",
                "ResourceClaims provisioned to a backend"
            ),
            &["backend", "namespace"],
        )
        .expect("CounterVec must build with a non-empty name");

        let claim_gc_total = CounterVec::new(
            opts!(
                "apprafter_claim_gc_total",
                "RetainedClaim GC sweeps (role/DB/Secret drop after the 7-day grace), by result and namespace"
            ),
            &["result", "namespace"],
        )
        .expect("CounterVec must build with a non-empty name");

        let image_resolve_total = CounterVec::new(
            opts!(
                "apprafter_image_resolve_total",
                "Application image tag->digest resolutions (ADR 0040), by result (ok|cached|failed)"
            ),
            &["result"],
        )
        .expect("CounterVec must build with a non-empty name");

        let soft_destructive_total = CounterVec::new(
            opts!(
                "apprafter_soft_destructive_total",
                "Soft (un-gated but potentially-disruptive) Application edits that rolled through, by trigger and namespace"
            ),
            &["trigger", "namespace"],
        )
        .expect("CounterVec must build with a non-empty name");

        let claim_retained_total = CounterVec::new(
            opts!(
                "apprafter_claim_retained_total",
                "RetainedClaim snapshots created on claim delete, by backend and namespace"
            ),
            &["backend", "namespace"],
        )
        .expect("CounterVec must build with a non-empty name");

        let migration_regate_total = CounterVec::new(
            opts!(
                "apprafter_migration_regate_total",
                "App-scope MigrationPlans re-gated at consume time on an approval-hash failure, by reason (HashMissing|HashMismatch) and namespace"
            ),
            &["reason", "namespace"],
        )
        .expect("CounterVec must build with a non-empty name");

        let shared_backend_reap_total = CounterVec::new(
            opts!(
                "apprafter_shared_backend_reap_total",
                "Shared-backend reaper decisions (ADR 0042 §9), by backend and result"
            ),
            &["backend", "result"],
        )
        .expect("CounterVec must build with a non-empty name");

        registry
            .register(Box::new(reconcile_total.clone()))
            .expect("reconcile_total registers cleanly");
        registry
            .register(Box::new(reconcile_duration.clone()))
            .expect("reconcile_duration registers cleanly");
        registry
            .register(Box::new(reconcile_errors.clone()))
            .expect("reconcile_errors registers cleanly");
        registry
            .register(Box::new(claim_unmatched_total.clone()))
            .expect("claim_unmatched_total registers cleanly");
        registry
            .register(Box::new(claim_provisioned_total.clone()))
            .expect("claim_provisioned_total registers cleanly");
        registry
            .register(Box::new(claim_gc_total.clone()))
            .expect("claim_gc_total registers cleanly");
        registry
            .register(Box::new(image_resolve_total.clone()))
            .expect("image_resolve_total registers cleanly");
        registry
            .register(Box::new(soft_destructive_total.clone()))
            .expect("soft_destructive_total registers cleanly");
        registry
            .register(Box::new(claim_retained_total.clone()))
            .expect("claim_retained_total registers cleanly");
        registry
            .register(Box::new(migration_regate_total.clone()))
            .expect("migration_regate_total registers cleanly");
        registry
            .register(Box::new(shared_backend_reap_total.clone()))
            .expect("shared_backend_reap_total registers cleanly");

        Self {
            registry,
            reconcile_total,
            reconcile_duration,
            reconcile_errors,
            claim_unmatched_total,
            claim_provisioned_total,
            claim_gc_total,
            image_resolve_total,
            soft_destructive_total,
            claim_retained_total,
            migration_regate_total,
            shared_backend_reap_total,
        }
    }

    /// Encode the registered metrics in Prometheus text format.
    pub fn encode(&self) -> Vec<u8> {
        let encoder = TextEncoder::new();
        let mfs = self.registry.gather();
        let mut buf = Vec::new();
        encoder
            .encode(&mfs, &mut buf)
            .expect("encode never fails on the in-process buffer");
        buf
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metrics_registry_lists_all_metric_families() {
        // Prometheus' TextEncoder skips empty metric families, so
        // we drive each counter once (to emit at least one sample
        // per family) and then verify the family names appear in
        // the encoded text.
        let m = Metrics::new();
        m.reconcile_total
            .with_label_values(&["Application", "default", "ok"])
            .inc();
        m.reconcile_duration
            .with_label_values(&["Application"])
            .observe(0.0);
        m.reconcile_errors.with_label_values(&["Application"]).inc();
        m.claim_unmatched_total
            .with_label_values(&["ResourceClaim", "demo", "no_matching_provider"])
            .inc();
        m.claim_provisioned_total
            .with_label_values(&["cloudnative-pg", "demo"])
            .inc();
        m.claim_gc_total
            .with_label_values(&["success", "apprafter-system"])
            .inc();
        m.image_resolve_total.with_label_values(&["ok"]).inc();
        m.soft_destructive_total
            .with_label_values(&["scale-down", "demo"])
            .inc();
        m.claim_retained_total
            .with_label_values(&["cloudnative-pg", "demo"])
            .inc();
        m.migration_regate_total
            .with_label_values(&["HashMismatch", "demo"])
            .inc();
        m.shared_backend_reap_total
            .with_label_values(&["dragonfly-ephemeral", "reaped"])
            .inc();
        let body = String::from_utf8(m.encode()).unwrap();
        assert!(body.contains("apprafter_reconcile_total"), "{body}");
        assert!(
            body.contains("apprafter_reconcile_duration_seconds"),
            "{body}"
        );
        assert!(body.contains("apprafter_reconcile_errors_total"), "{body}");
        assert!(body.contains("apprafter_claim_unmatched_total"), "{body}");
        assert!(body.contains("apprafter_claim_provisioned_total"), "{body}");
        assert!(body.contains("apprafter_claim_gc_total"), "{body}");
        assert!(body.contains("apprafter_image_resolve_total"), "{body}");
        assert!(body.contains("apprafter_soft_destructive_total"), "{body}");
        assert!(body.contains("apprafter_claim_retained_total"), "{body}");
        assert!(body.contains("apprafter_migration_regate_total"), "{body}");
        assert!(
            body.contains("apprafter_shared_backend_reap_total"),
            "{body}"
        );
    }

    #[test]
    fn reconcile_total_counter_increments_with_labels() {
        let m = Metrics::new();
        m.reconcile_total
            .with_label_values(&["Application", "default", "ok"])
            .inc();
        m.reconcile_total
            .with_label_values(&["Application", "default", "error"])
            .inc_by(2.0);
        let body = String::from_utf8(m.encode()).unwrap();
        assert!(body.contains("result=\"ok\""), "{body}");
        assert!(body.contains("result=\"error\""), "{body}");
    }

    #[test]
    fn reconcile_errors_counter_independent_from_total() {
        let m = Metrics::new();
        m.reconcile_errors.with_label_values(&["Application"]).inc();
        let body = String::from_utf8(m.encode()).unwrap();
        // The error counter has its own family — visible even when
        // reconcile_total is empty.
        assert!(
            body.contains("apprafter_reconcile_errors_total{kind=\"Application\"}"),
            "{body}"
        );
    }
}
