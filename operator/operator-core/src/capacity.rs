// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Capacity-signal via the kubelet Summary API (Phase 2.6c, Task 11).
//!
//! # Why this lives in `operator-core` (2.22d / D8)
//!
//! It used to live in the ResourceClaim provisioner, which meant the NODE's
//! disk-pressure signal was computed only while reconciling a
//! `SharedVolume` — an optional, application-scoped object. A cluster with
//! none got no warning at all, though everything on a Tier-1 node shares
//! that filesystem: owned disks, CNPG data, Dragonfly snapshots, container
//! images and logs. A node-scoped fact needs a node-scoped carrier, so the
//! PlatformStack controller now computes it too and both import from here
//! rather than one of them owning it.
//!
//! A node's local-path filesystem can fill up; when it does, the
//! local-path PVCs backing a `SharedVolume` (and owned disks) silently
//! stop accepting writes. To surface this BEFORE it bites, the provisioner
//! samples the kubelet Summary API per-reconcile and stamps a
//! `CapacityWarning` condition + `status.capacity` on the `SharedVolume`,
//! plus an edge-triggered Warning `Event`.
//!
//! ## Best-effort (load-bearing safety property)
//!
//! Capacity is **decorative**: a Summary-API failure (RBAC, kubelet
//! unreachable, parse error, no node) MUST NEVER fail a reconcile. Every
//! fetch path returns `Option` — `None` means "capacity simply absent this
//! cycle", and the reconcile proceeds. ("decorative lookups must be
//! best-effort" — operator RBAC/anchor lesson.)
//!
//! ## Pure core
//!
//! The Summary-JSON parsers ([`node_free_fraction`], [`pvc_usage`]), the
//! threshold predicate ([`is_capacity_warning`]), and the edge-trigger
//! ([`should_emit_event`]) are pure functions over `serde_json::Value` /
//! scalars, unit-tested without a cluster. Only the [`CapacityCache`] fetch
//! touches I/O, and it is validated on the T13 walk.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::Value;
use tracing::debug;

/// Node-free fraction below which a `CapacityWarning` fires. 0.15 ⇒ warn
/// when the node filesystem is more than 85% full.
pub const DEFAULT_NODE_FREE_THRESHOLD: f64 = 0.15;

/// How long a cached kubelet Summary sample is considered fresh. The two
/// controllers share one [`CapacityCache`] (via `Context`), so a single
/// node's kubelet is hit at most once per TTL across all reconciles in the
/// window — keeping the per-reconcile poll cheap.
const CACHE_TTL: Duration = Duration::from_secs(30);

// ---------------------------------------------------------------------------
// Pure parsers + predicates (unit-tested without a cluster)
// ---------------------------------------------------------------------------

/// Free fraction (`availableBytes / capacityBytes`) of the node's root
/// filesystem from a kubelet Summary document, or `None` if either field
/// is missing/non-numeric or capacity is non-positive.
pub fn node_free_fraction(summary: &Value) -> Option<f64> {
    let avail = summary.pointer("/node/fs/availableBytes")?.as_f64()?;
    let cap = summary.pointer("/node/fs/capacityBytes")?.as_f64()?;
    if cap <= 0.0 {
        return None;
    }
    Some(avail / cap)
}

/// The node root filesystem's `capacityBytes`, or `None` when absent.
///
/// Read from the same Summary document the per-volume figures come from, so a
/// caller can tell whether a volume's numbers are the volume's own — see
/// [`capacity_scope`].
pub fn node_fs_capacity(summary: &Value) -> Option<i64> {
    summary.pointer("/node/fs/capacityBytes")?.as_i64()
}

/// Whether a sampled volume figure describes the VOLUME or the HOST DISK
/// underneath it (D29).
///
/// # The problem this names
///
/// The kubelet reports volume statistics per PVC, and for a local-path
/// (hostPath-backed) PersistentVolume those statistics are the BACKING
/// FILESYSTEM's: a directory on a shared filesystem has no quota to report
/// against, so `capacityBytes` comes back as the whole node disk.
///
/// Measured on real hardware: a claim requesting 1Gi reported
/// `capacityBytes = 80279486464`, byte-identical to the node's root
/// filesystem, and `usedBytes` was the node's used space. Presenting that as
/// the claim's own capacity tells an operator their 1Gi volume holds 80GB —
/// the node's fact wearing the claim's name.
///
/// Nothing in the sampling is wrong; the composition is. So instead of
/// discarding the figure (it is genuinely useful — "this volume shares an 80GB
/// disk that is 12% full" is the actionable fact on a single-node tier) or
/// dressing it up, record WHICH THING WAS MEASURED and let the reader be told
/// the truth.
///
/// Equality against the node's capacity is the detector. A CSI backend that
/// enforces a real quota reports that quota, and matching the root
/// filesystem's byte count exactly would be a coincidence with no plausible
/// mechanism behind it.
pub fn capacity_scope(
    volume_capacity_bytes: i64,
    node_capacity_bytes: Option<i64>,
) -> &'static str {
    match node_capacity_bytes {
        Some(node) if node > 0 && node == volume_capacity_bytes => SCOPE_HOST,
        _ => SCOPE_VOLUME,
    }
}

/// The figure is the host disk's, not this volume's.
pub const SCOPE_HOST: &str = "host";
/// The figure is the volume's own — a backend with a real quota.
pub const SCOPE_VOLUME: &str = "volume";

/// `(usedBytes, capacityBytes)` for the named PVC, scanning every pod's
/// `volume[]` for a matching `pvcRef.name`. `None` when the PVC is not
/// mounted by any pod the kubelet reports, or either byte field is absent.
pub fn pvc_usage(summary: &Value, pvc_name: &str) -> Option<(i64, i64)> {
    for pod in summary.get("pods")?.as_array()? {
        if let Some(vols) = pod.get("volume").and_then(Value::as_array) {
            for v in vols {
                if v.pointer("/pvcRef/name").and_then(Value::as_str) == Some(pvc_name) {
                    return Some((
                        v.get("usedBytes").and_then(Value::as_i64)?,
                        v.get("capacityBytes").and_then(Value::as_i64)?,
                    ));
                }
            }
        }
    }
    None
}

/// Every PVC name the kubelet summary carries volume statistics for.
///
/// Used only to turn "no figure" into a sentence a human can act on: an EMPTY
/// list means this kubelet reports no volume metrics at all (its volume plugin
/// has no metrics provider — `hostPath`-backed volumes never report), while a
/// non-empty one that omits the PVC in question means something more specific
/// is wrong.
pub fn reported_pvc_names(summary: &Value) -> Vec<String> {
    let mut out = Vec::new();
    let Some(pods) = summary.get("pods").and_then(Value::as_array) else {
        return out;
    };
    for pod in pods {
        let Some(vols) = pod.get("volume").and_then(Value::as_array) else {
            continue;
        };
        for v in vols {
            if let Some(n) = v.pointer("/pvcRef/name").and_then(Value::as_str) {
                out.push(n.to_string());
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

/// Whether the node-free fraction is below the warning threshold.
pub fn is_capacity_warning(node_free_fraction: f64, threshold: f64) -> bool {
    node_free_fraction < threshold
}

/// Fraction of a volume's own capacity above which it is worth warning.
/// 0.85 ⇒ warn once the volume is more than 85% full.
pub const DEFAULT_VOLUME_FULL_THRESHOLD: f64 = 0.85;

/// Whether a volume's OWN usage is above the warning threshold (2.22d / D8).
///
/// Distinct from [`is_capacity_warning`], which reads the node's filesystem.
/// The platform sampled both from the beginning and thresholded only the
/// node, so a volume at 99% of its own request on a healthy node said
/// nothing — while the condition it would have set was, confusingly, called
/// `CapacityWarning` and reported the node instead.
///
/// `None` when the numbers cannot support a judgement: a non-positive
/// capacity is not "0% full", it is unknown, and returning `false` there
/// would be an assertion the data does not carry.
pub fn is_volume_warning(used_bytes: i64, capacity_bytes: i64, threshold: f64) -> Option<bool> {
    if capacity_bytes <= 0 || used_bytes < 0 {
        return None;
    }
    Some(used_bytes as f64 / capacity_bytes as f64 > threshold)
}

/// Edge-trigger: emit a Warning Event only on an OK→warning transition
/// (anti-spam). warn→warn and any →OK transition are suppressed.
pub fn should_emit_event(was_warning: bool, is_warning: bool) -> bool {
    is_warning && !was_warning
}

// ---------------------------------------------------------------------------
// TTL cache + best-effort kubelet fetch (I/O — validated on the T13 walk)
// ---------------------------------------------------------------------------

/// Per-node TTL cache of kubelet Summary documents.
///
/// Keyed by node name; a sample is reused while younger than [`CACHE_TTL`],
/// else a fresh fetch is made through the apiserver node-proxy subresource
/// (`GET /api/v1/nodes/{node}/proxy/stats/summary`, RBAC verb `get` on
/// `nodes/proxy`). Every fetch is **best-effort**: any error (request fails,
/// non-JSON body, RBAC denial) is logged at debug and yields `None`.
#[derive(Default)]
pub struct CapacityCache {
    entries: Mutex<HashMap<String, (Instant, Value)>>,
}

impl CapacityCache {
    /// Construct an empty cache.
    pub fn new() -> Self {
        Self::default()
    }

    /// Return a kubelet Summary document for `node`, served from cache when
    /// fresh else fetched. Returns `None` on any error — NEVER an `Err`, so
    /// a caller can treat capacity as simply absent and continue.
    pub async fn summary_for_node(&self, client: &kube::Client, node: &str) -> Option<Value> {
        self.summary_for_node_with(node, CACHE_TTL, || Self::fetch(client, node))
            .await
    }

    /// The cache policy behind [`Self::summary_for_node`], with the kubelet
    /// fetch injected.
    ///
    /// Two invariants live here and neither is visible from outside: a fresh
    /// sample must suppress the fetch ENTIRELY — both controllers share one
    /// cache precisely so a node's kubelet is polled at most once per TTL no
    /// matter how many claims reconcile in the window — and a FAILED fetch
    /// must not be stored, or one RBAC hiccup would suppress capacity for
    /// every reconcile in the next 30 seconds.
    async fn summary_for_node_with<F, Fut>(
        &self,
        node: &str,
        ttl: Duration,
        fetch: F,
    ) -> Option<Value>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Option<Value>>,
    {
        if let Some(v) = self.cached_fresh(node, ttl) {
            return Some(v);
        }
        let value = fetch().await?;
        self.store(node, value.clone());
        Some(value)
    }

    /// Return the cached Summary for `node` iff it is younger than `ttl`.
    ///
    /// The window is a parameter rather than a read of [`CACHE_TTL`] purely
    /// so the expiry boundary is reachable from a test: `Instant` cannot be
    /// constructed in the past portably, so the comparison has to be driven
    /// from its other side. The single production caller passes `CACHE_TTL`.
    fn cached_fresh(&self, node: &str, ttl: Duration) -> Option<Value> {
        let entries = self.entries.lock().ok()?;
        let (at, value) = entries.get(node)?;
        if at.elapsed() < ttl {
            Some(value.clone())
        } else {
            None
        }
    }

    /// Insert/replace the cached sample for `node` stamped at `now`.
    fn store(&self, node: &str, value: Value) {
        if let Ok(mut entries) = self.entries.lock() {
            entries.insert(node.to_string(), (Instant::now(), value));
        }
    }

    /// Best-effort raw fetch of the kubelet Summary API through the
    /// apiserver node-proxy subresource. Any failure ⇒ `None` (logged).
    async fn fetch(client: &kube::Client, node: &str) -> Option<Value> {
        let path = node_summary_path(node);
        let req = match http::Request::get(&path).body(Vec::new()) {
            Ok(r) => r,
            Err(e) => {
                debug!(%node, error = %e, "capacity: failed to build kubelet Summary request");
                return None;
            }
        };
        match client.request::<Value>(req).await {
            Ok(v) => Some(v),
            Err(e) => {
                // RBAC denial / kubelet unreachable / non-JSON body all land
                // here — capacity is decorative, so swallow and continue.
                debug!(%node, error = %e, "capacity: kubelet Summary fetch failed (continuing without capacity)");
                None
            }
        }
    }
}

/// The apiserver path that proxies to a node's kubelet Summary API.
///
/// Split out of [`CapacityCache::fetch`] so the one non-I/O part of it can be
/// pinned. `nodes/{name}/proxy` is a subresource: an RBAC rule is granted on
/// `nodes/proxy` and the request must address exactly that path, and because
/// every failure here is swallowed by design, a wrong path produces silence
/// rather than an error — capacity would simply never appear anywhere.
fn node_summary_path(node: &str) -> String {
    format!("/api/v1/nodes/{node}/proxy/stats/summary")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_volume_past_the_threshold_warns_and_one_below_does_not() {
        assert_eq!(is_volume_warning(90, 100, 0.85), Some(true));
        assert_eq!(is_volume_warning(50, 100, 0.85), Some(false));
    }

    #[test]
    fn exactly_at_the_threshold_does_not_warn() {
        // Strictly greater: a volume sitting on the line is not yet a
        // problem, and a threshold that fires AT its own value makes the
        // number in the message look wrong to whoever reads it.
        assert_eq!(is_volume_warning(85, 100, 0.85), Some(false));
    }

    #[test]
    fn unusable_numbers_are_unknown_rather_than_healthy() {
        // A non-positive capacity is not "0% full" — it is a sample that
        // cannot support a judgement. Returning false there would assert
        // something the data does not carry, which is how a monitoring
        // surface starts lying quietly.
        assert_eq!(is_volume_warning(10, 0, 0.85), None);
        assert_eq!(is_volume_warning(10, -1, 0.85), None);
        assert_eq!(is_volume_warning(-1, 100, 0.85), None);
    }

    #[test]
    fn parses_node_free_fraction_from_summary() {
        let s = json!({ "node": { "fs": { "availableBytes": 15_u64, "capacityBytes": 100_u64 } } });
        assert!((node_free_fraction(&s).unwrap() - 0.15).abs() < 1e-9);
    }

    #[test]
    fn node_free_fraction_none_on_missing_or_zero_capacity() {
        assert!(node_free_fraction(&json!({})).is_none());
        let zero = json!({ "node": { "fs": { "availableBytes": 0, "capacityBytes": 0 } } });
        assert!(node_free_fraction(&zero).is_none());
    }

    #[test]
    fn parses_pvc_used_capacity_by_name() {
        let s = json!({ "pods": [ { "volume": [
            { "pvcRef": { "name": "sv-demo-shared" }, "usedBytes": 40_u64, "capacityBytes": 50_u64 } ] } ] });
        assert_eq!(pvc_usage(&s, "sv-demo-shared"), Some((40, 50)));
    }

    #[test]
    fn pvc_usage_none_when_pvc_absent() {
        let s = json!({ "pods": [ { "volume": [
            { "pvcRef": { "name": "other" }, "usedBytes": 1, "capacityBytes": 2 } ] } ] });
        assert_eq!(pvc_usage(&s, "sv-demo-shared"), None);
        assert_eq!(pvc_usage(&json!({}), "sv-demo-shared"), None);
    }

    #[test]
    fn warning_fires_below_threshold_only() {
        assert!(is_capacity_warning(0.10, 0.15));
        assert!(!is_capacity_warning(0.20, 0.15));
    }

    #[test]
    fn edge_triggered_event_only_on_transition() {
        assert!(should_emit_event(false, true)); // OK→warn → emit
        assert!(!should_emit_event(true, true)); // warn→warn → suppress
        assert!(!should_emit_event(true, false)); // recovered → no emit
        assert!(!should_emit_event(false, false)); // OK→OK → no emit
    }

    // -----------------------------------------------------------------
    // D29 — a figure must say which thing it measured
    // -----------------------------------------------------------------

    #[test]
    fn a_volume_figure_matching_the_node_disk_is_the_hosts() {
        // The measured case, from e2e/node-disk-pressure-hetzner.sh: a claim
        // that asked for 1Gi reported the node's whole root filesystem,
        // because local-path has no quota for the kubelet to report against.
        assert_eq!(
            capacity_scope(80_279_486_464, Some(80_279_486_464)),
            SCOPE_HOST
        );
    }

    #[test]
    fn a_real_quota_reports_itself_and_stays_volume_scoped() {
        // A CSI backend enforcing 1Gi reports 1Gi; it does not coincide with
        // the node byte count, and must not be relabelled.
        assert_eq!(
            capacity_scope(1_073_741_824, Some(80_279_486_464)),
            SCOPE_VOLUME
        );
    }

    #[test]
    fn an_unknown_node_capacity_never_claims_host_scope() {
        // Silence about the node is not evidence about the volume. Defaulting
        // to `host` on a missing figure would relabel every correct volume
        // number on a cluster whose kubelet stopped reporting node.fs.
        assert_eq!(capacity_scope(1_073_741_824, None), SCOPE_VOLUME);
        // And a nonsense node figure is treated the same way.
        assert_eq!(capacity_scope(0, Some(0)), SCOPE_VOLUME);
    }

    #[test]
    fn node_fs_capacity_reads_the_same_document_the_volume_figures_come_from() {
        let s = json!({ "node": { "fs": { "availableBytes": 15_u64, "capacityBytes": 100_u64 } } });
        assert_eq!(node_fs_capacity(&s), Some(100));
        assert_eq!(node_fs_capacity(&json!({})), None);
    }

    // -----------------------------------------------------------------
    // reported_pvc_names — the diagnostic that explains a missing figure
    // -----------------------------------------------------------------

    /// Two pods. The second mounts a PVC the first does not (so a scan that
    /// stops early loses it) as well as one they share (an RWX volume, or the
    /// same PVC seen through two pods), plus a volume that is not a PVC.
    fn two_pod_summary() -> Value {
        json!({ "pods": [
            { "volume": [
                { "pvcRef": { "name": "sv-demo-shared" }, "usedBytes": 40_u64, "capacityBytes": 50_u64 },
                { "name": "tmp" }
            ] },
            { "volume": [
                { "pvcRef": { "name": "sv-demo-shared" }, "usedBytes": 40_u64, "capacityBytes": 50_u64 },
                { "pvcRef": { "name": "claim-demo-web-disk" }, "usedBytes": 1_u64, "capacityBytes": 9_u64 }
            ] }
        ]})
    }

    #[test]
    fn reported_pvc_names_lists_every_pvc_across_every_pod_once() {
        // Sorted and deduplicated: this list is rendered to a human as "the
        // kubelet reports these", so the same volume appearing twice because
        // two pods mount it reads as two volumes.
        assert_eq!(
            reported_pvc_names(&two_pod_summary()),
            vec![
                "claim-demo-web-disk".to_string(),
                "sv-demo-shared".to_string()
            ]
        );
    }

    #[test]
    fn a_kubelet_publishing_no_volume_metrics_reports_an_empty_list() {
        // Emptiness is the whole signal here — it is what separates "this
        // kubelet has no volume metrics provider at all" (hostPath-backed
        // volumes never report) from "it reports volumes, just not yours".
        // A non-PVC volume must not be counted as a nameless PVC, which is
        // what any `unwrap_or_default()` on the pvcRef would produce.
        let no_volumes =
            json!({ "pods": [ { "name": "p1" }, { "volume": [ { "name": "tmp" } ] } ] });
        assert!(reported_pvc_names(&no_volumes).is_empty());
        assert!(reported_pvc_names(&json!({})).is_empty());
        // …and the same function does find them when they are there.
        assert!(!reported_pvc_names(&two_pod_summary()).is_empty());
    }

    // -----------------------------------------------------------------
    // pvc_usage — scanning, and refusing to invent numbers
    // -----------------------------------------------------------------

    #[test]
    fn pvc_usage_scans_past_the_first_pod() {
        // The claim's own pod is rarely first in the kubelet's list; stopping
        // at the first pod would report "no capacity" for every volume but
        // one, intermittently, depending on pod ordering.
        let s = json!({ "pods": [
            { "volume": [ { "pvcRef": { "name": "other" }, "usedBytes": 1_u64, "capacityBytes": 2_u64 } ] },
            { "volume": [ { "pvcRef": { "name": "wanted" }, "usedBytes": 7_u64, "capacityBytes": 11_u64 } ] }
        ]});
        assert_eq!(pvc_usage(&s, "wanted"), Some((7, 11)));
    }

    #[test]
    fn a_matched_volume_missing_its_byte_fields_is_unknown_not_zero() {
        // The volume is present but the kubelet did not report its bytes.
        // Defaulting either field to 0 would render a full disk as an empty
        // one — the same "unmeasured is not zero" rule the size scrape keeps.
        let no_used = json!({ "pods": [ { "volume": [
            { "pvcRef": { "name": "v" }, "capacityBytes": 50_u64 } ] } ] });
        assert_eq!(pvc_usage(&no_used, "v"), None);
        let no_capacity = json!({ "pods": [ { "volume": [
            { "pvcRef": { "name": "v" }, "usedBytes": 40_u64 } ] } ] });
        assert_eq!(pvc_usage(&no_capacity, "v"), None);
    }

    // -----------------------------------------------------------------
    // node_free_fraction / is_capacity_warning — boundaries
    // -----------------------------------------------------------------

    #[test]
    fn a_non_numeric_summary_field_is_unknown_rather_than_zero_free() {
        // A kubelet that answers with a string (or an error document shaped
        // like a Summary) must not read as "0% free", which would fire a
        // disk-pressure warning on a node nothing is known about.
        let s = json!({ "node": { "fs": { "availableBytes": "15", "capacityBytes": 100_u64 } } });
        assert!(node_free_fraction(&s).is_none());
        let missing_avail = json!({ "node": { "fs": { "capacityBytes": 100_u64 } } });
        assert!(node_free_fraction(&missing_avail).is_none());
    }

    #[test]
    fn a_node_exactly_at_the_threshold_does_not_warn() {
        // Strictly below, matching `is_volume_warning`'s strictly-above: a
        // node sitting exactly on 15% free is the boundary, not past it, and
        // a threshold that fires at its own value makes the number in the
        // Event look wrong to whoever reads it.
        assert!(!is_capacity_warning(0.15, 0.15));
        assert!(is_capacity_warning(0.149, 0.15));
    }

    #[test]
    fn a_negative_node_capacity_never_claims_host_scope() {
        // `capacity_scope` guards on `node > 0`; a negative pair would
        // otherwise compare equal and relabel a volume's own figure.
        assert_eq!(capacity_scope(-1, Some(-1)), SCOPE_VOLUME);
    }

    // -----------------------------------------------------------------
    // CapacityCache — key isolation and the TTL boundary
    // -----------------------------------------------------------------

    #[test]
    fn the_summary_url_addresses_the_node_proxy_subresource() {
        assert_eq!(
            node_summary_path("node-1"),
            "/api/v1/nodes/node-1/proxy/stats/summary"
        );
    }

    #[test]
    fn a_stored_summary_is_served_back_per_node_while_it_is_fresh() {
        // Per NODE: on a multi-node cluster one node's disk-pressure sample
        // standing in for another's is a warning attached to the wrong
        // machine, which sends an operator to drain a healthy node.
        let cache = CapacityCache::new();
        cache.store(
            "node-a",
            json!({ "node": { "fs": { "capacityBytes": 1_u64 } } }),
        );
        cache.store(
            "node-b",
            json!({ "node": { "fs": { "capacityBytes": 2_u64 } } }),
        );
        assert_eq!(
            cache
                .cached_fresh("node-a", CACHE_TTL)
                .and_then(|v| node_fs_capacity(&v)),
            Some(1)
        );
        assert_eq!(
            cache
                .cached_fresh("node-b", CACHE_TTL)
                .and_then(|v| node_fs_capacity(&v)),
            Some(2)
        );
        assert!(cache.cached_fresh("node-c", CACHE_TTL).is_none());
    }

    /// A kubelet fetch that records how often it ran, so "did not poll the
    /// kubelet" is an assertion rather than an assumption.
    struct CountingFetch {
        calls: std::cell::Cell<u32>,
        result: Option<Value>,
    }

    impl CountingFetch {
        fn new(result: Option<Value>) -> Self {
            Self {
                calls: std::cell::Cell::new(0),
                result,
            }
        }
        async fn run(&self) -> Option<Value> {
            self.calls.set(self.calls.get() + 1);
            self.result.clone()
        }
    }

    #[tokio::test]
    async fn a_fresh_sample_suppresses_the_kubelet_poll_entirely() {
        // Both controllers share one cache so that a node's kubelet is polled
        // at most once per TTL however many claims reconcile in the window. A
        // cache that returned the right answer but polled anyway would look
        // perfectly correct while multiplying kubelet load by the claim count.
        let cache = CapacityCache::new();
        cache.store(
            "node-a",
            json!({ "node": { "fs": { "capacityBytes": 1_u64 } } }),
        );
        let fetch = CountingFetch::new(Some(
            json!({ "node": { "fs": { "capacityBytes": 2_u64 } } }),
        ));
        let got = cache
            .summary_for_node_with("node-a", CACHE_TTL, || fetch.run())
            .await;
        assert_eq!(got.as_ref().and_then(node_fs_capacity), Some(1));
        assert_eq!(fetch.calls.get(), 0);
    }

    #[tokio::test]
    async fn a_failed_kubelet_poll_is_not_cached() {
        // Capacity is decorative and every failure is swallowed, so a
        // remembered failure would be indistinguishable from a node that
        // simply has no capacity to report — for the whole TTL, on every
        // reconcile, with nothing in the logs to say why.
        let cache = CapacityCache::new();
        let failing = CountingFetch::new(None);
        assert!(cache
            .summary_for_node_with("node-a", CACHE_TTL, || failing.run())
            .await
            .is_none());
        assert!(cache.cached_fresh("node-a", CACHE_TTL).is_none());

        // The next reconcile gets a real attempt, and that one IS cached.
        let ok = CountingFetch::new(Some(
            json!({ "node": { "fs": { "capacityBytes": 7_u64 } } }),
        ));
        let got = cache
            .summary_for_node_with("node-a", CACHE_TTL, || ok.run())
            .await;
        assert_eq!(got.as_ref().and_then(node_fs_capacity), Some(7));
        assert_eq!(ok.calls.get(), 1);
        assert_eq!(
            cache
                .cached_fresh("node-a", CACHE_TTL)
                .as_ref()
                .and_then(node_fs_capacity),
            Some(7)
        );
    }

    #[test]
    fn a_summary_older_than_the_ttl_is_not_served() {
        // Driven from the TTL side because `Instant` has no portable past.
        let cache = CapacityCache::new();
        cache.store("node-a", json!({ "node": {} }));
        assert!(cache.cached_fresh("node-a", Duration::ZERO).is_none());
    }
}
