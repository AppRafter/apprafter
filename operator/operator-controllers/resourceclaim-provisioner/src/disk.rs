// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Pure `disk` backend builders (Phase 2.6b / ADR 0043).
//!
//! A `needs.disk` entry generates a `type: disk` ResourceClaim that the
//! `Backend::Disk` provisioner (`reconcile.rs`) backs with a standalone
//! `ReadWriteOnce` PersistentVolumeClaim. The PVC is **unowned** — no
//! `ownerReferences` back to the Application or claim — so it survives an
//! app delete + redeploy. Its lifecycle is INTENDED to be GC-managed via
//! the 2.4f RetainedClaim + grace, exactly like the CNPG role/DB
//! (retention); the disk GC arm (`gc_drop_disk` + the disk snapshot
//! branch) lands in 2.6b-5 and is not wired yet.
//!
//! Every function here is pure (`-> serde_json::Value`), so the whole
//! module is unit-testable without a cluster. The live SSA-apply +
//! terminal-status path is exercised by the 2.6b e2e walk.

use serde_json::{json, Value};

/// Build a v1 `PersistentVolumeClaim` SSA-apply body for a disk claim.
///
/// `accessModes: [ReadWriteOnce]` (single-replica at launch; per-replica
/// RWX is a deferred T2 feature), `resources.requests.storage = size`
/// (a Kubernetes quantity string, e.g. `"1Gi"`), and
/// `storageClassName = storage_class` (the matched ServiceProvider's
/// `config.storageClass`, tier-mapped).
///
/// **NO `ownerReferences`** — the PVC must outlive an app delete so data
/// survives a delete+redeploy within the RetainedClaim grace window; the
/// intent is for the GC to drop it after grace (like the pg role/DB). The
/// disk GC arm (`gc_drop_disk` + the disk snapshot branch) lands in
/// 2.6b-5 and is not wired yet. `metadata` carries only `name` +
/// `namespace` + the inventory label.
pub fn pvc_object(name: &str, ns: &str, size: &str, storage_class: &str) -> Value {
    json!({
        "apiVersion": "v1",
        "kind": "PersistentVolumeClaim",
        "metadata": {
            "name": name,
            "namespace": ns,
            "labels": {
                "apprafter.io/managed-by": "apprafter",
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
    fn pvc_object_is_rwo_unowned_with_class_and_size() {
        let p = pvc_object("claim-demo-app-disk-data", "demo", "1Gi", "local-path");
        assert_eq!(p["apiVersion"], "v1");
        assert_eq!(p["kind"], "PersistentVolumeClaim");
        assert_eq!(p["metadata"]["name"], "claim-demo-app-disk-data");
        assert_eq!(p["metadata"]["namespace"], "demo");
        // Single-replica RWO at launch.
        assert_eq!(p["spec"]["accessModes"], json!(["ReadWriteOnce"]));
        // Requested storage carries the claim's quantity verbatim.
        assert_eq!(p["spec"]["resources"]["requests"]["storage"], "1Gi");
        // Storage class from the matched provider's config.
        assert_eq!(p["spec"]["storageClassName"], "local-path");
        // Inventory label, like the other managed objects.
        assert_eq!(
            p["metadata"]["labels"]["apprafter.io/managed-by"],
            "apprafter"
        );
        // CRITICAL (retention): the PVC is UNOWNED — no ownerReferences —
        // so an app delete does NOT cascade-delete it; the 2.4f GC is
        // INTENDED to drop it after the grace window (like the pg role/DB),
        // once the disk GC arm lands in 2.6b-5.
        assert!(
            p["metadata"].get("ownerReferences").is_none(),
            "disk PVC must be unowned so it survives an app delete (retention)"
        );
    }
}
