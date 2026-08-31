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

/// Whether the node-free fraction is below the warning threshold.
pub fn is_capacity_warning(node_free_fraction: f64, threshold: f64) -> bool {
    node_free_fraction < threshold
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
        if let Some(v) = self.cached_fresh(node) {
            return Some(v);
        }
        match Self::fetch(client, node).await {
            Some(v) => {
                self.store(node, v.clone());
                Some(v)
            }
            None => None,
        }
    }

    /// Return the cached Summary for `node` iff it is younger than the TTL.
    fn cached_fresh(&self, node: &str) -> Option<Value> {
        let entries = self.entries.lock().ok()?;
        let (at, value) = entries.get(node)?;
        if at.elapsed() < CACHE_TTL {
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
        let path = format!("/api/v1/nodes/{node}/proxy/stats/summary");
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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
}
