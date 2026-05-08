// SPDX-License-Identifier: FSL-1.1-MIT
//! Prometheus metrics for the AppRafter operator.
//!
//! Three signal-grade metrics:
//!   - apprafter_reconcile_total{kind,namespace,result} — every
//!     reconcile call increments one of {ok, error}.
//!   - apprafter_reconcile_duration_seconds{kind} — histogram of
//!     wall-time per reconcile.
//!   - apprafter_reconcile_errors_total{kind} — error-only counter
//!     for quick "errors per minute" alerts.
//!
//! Metrics are registered into a single `Registry` that the HTTP
//! `/metrics` handler in `apprafter-operator` encodes.

use prometheus::{histogram_opts, opts, CounterVec, Encoder, HistogramVec, Registry, TextEncoder};

pub struct Metrics {
    pub registry: Registry,
    pub reconcile_total: CounterVec,
    pub reconcile_duration: HistogramVec,
    pub reconcile_errors: CounterVec,
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

        registry
            .register(Box::new(reconcile_total.clone()))
            .expect("reconcile_total registers cleanly");
        registry
            .register(Box::new(reconcile_duration.clone()))
            .expect("reconcile_duration registers cleanly");
        registry
            .register(Box::new(reconcile_errors.clone()))
            .expect("reconcile_errors registers cleanly");

        Self {
            registry,
            reconcile_total,
            reconcile_duration,
            reconcile_errors,
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
    fn metrics_registry_lists_all_three_metric_families() {
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
        m.reconcile_errors
            .with_label_values(&["Application"])
            .inc();
        let body = String::from_utf8(m.encode()).unwrap();
        assert!(body.contains("apprafter_reconcile_total"), "{body}");
        assert!(
            body.contains("apprafter_reconcile_duration_seconds"),
            "{body}"
        );
        assert!(body.contains("apprafter_reconcile_errors_total"), "{body}");
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
        m.reconcile_errors
            .with_label_values(&["Application"])
            .inc();
        let body = String::from_utf8(m.encode()).unwrap();
        // The error counter has its own family — visible even when
        // reconcile_total is empty.
        assert!(
            body.contains("apprafter_reconcile_errors_total{kind=\"Application\"}"),
            "{body}"
        );
    }
}
