// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Pure `SharedVolume` PVC builder (Phase 2.6c).
//!
//! A `SharedVolume` is backed by an unowned `ReadWriteOnce`
//! `PersistentVolumeClaim` SSA-applied by the SharedVolume reconciler
//! (T6). Every function here is pure (`-> serde_json::Value`), so the
//! module is unit-testable without a cluster.

use serde_json::{json, Value};

/// Deterministic unowned-PVC name for a SharedVolume.
///
/// The `sv-` prefix avoids any collision with owned-disk PVC names
/// (which are named `claim-<ns>-<app>-disk-<claim>`).
pub fn sv_pvc_name(ns: &str, name: &str) -> String {
    format!("sv-{ns}-{name}")
}

/// Pure SSA-apply body for the unowned backing PVC.
///
/// `accessModes: [ReadWriteOnce]` — on a single node N pods all
/// schedule onto the same node so RWO allows concurrent mounts.
///
/// **NO `ownerReferences`** — the PVC lifecycle is owned by the
/// `SharedVolume` CR (reaped by `volume rm` via finalizer, not by
/// an app delete). A second label `apprafter.io/shared-volume=<name>`
/// makes the PVC inventory-queryable by name without a full label scan.
pub fn sv_pvc_object(
    pvc_name: &str,
    ns: &str,
    size: &str,
    storage_class: &str,
    sv_name: &str,
) -> Value {
    json!({
        "apiVersion": "v1",
        "kind": "PersistentVolumeClaim",
        "metadata": {
            "name": pvc_name,
            "namespace": ns,
            "labels": {
                "apprafter.io/managed-by": "apprafter",
                "apprafter.io/shared-volume": sv_name,
            },
        },
        "spec": {
            "accessModes": ["ReadWriteOnce"],
            "storageClassName": storage_class,
            "resources": {
                "requests": {
                    "storage": size,
                },
            },
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn sv_pvc_object_is_rwo_unowned_labelled() {
        let p = sv_pvc_object("sv-demo-shared", "demo", "5Gi", "local-path", "shared");
        assert_eq!(p["spec"]["accessModes"], json!(["ReadWriteOnce"]));
        assert_eq!(p["spec"]["storageClassName"], "local-path");
        assert_eq!(p["spec"]["resources"]["requests"]["storage"], "5Gi");
        assert_eq!(
            p["metadata"]["labels"]["apprafter.io/shared-volume"],
            "shared"
        );
        assert!(p["metadata"].get("ownerReferences").is_none());
    }

    #[test]
    fn sv_pvc_name_is_deterministic_and_prefixed() {
        assert_eq!(sv_pvc_name("demo", "shared"), "sv-demo-shared");
    }
}
