// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Ephemeral helper-pod spec builders (pure) + `KubeExec`-forwarding stream
//! helpers (impure) for 2.6d backup/restore data extraction and load.
//!
//! # Pure pod-spec builders
//!
//! `pg_dump_pod_spec` / `volume_pod_spec` return serde_json::Value Pod specs
//! that are applied via `apply_and_wait_pod_ready` and then exec'd into.
//! Both use `["sleep", "3600"]` as the container command — a keep-alive so
//! the CLI can exec before the tool needs to run.  `restartPolicy: Never`
//! ensures a single attempt; the caller tears down with
//! `delete_pod_best_effort` after the stream completes.
//!
//! # Impure forwarding helpers
//!
//! `apply_and_wait_pod_ready`, `exec_stream_to_file`, `exec_stream_from_file`,
//! `delete_pod_best_effort` delegate to the [`KubeExec`] trait so the engine
//! is portable across the CLI subprocess path and the future in-cluster runner.

use cli_core::Result;
use serde_json::{json, Value};
use std::path::Path;

use crate::kube::KubeExec;

// ---------------------------------------------------------------------------
// Pure pod-spec builders
// ---------------------------------------------------------------------------

/// Build a Pod spec for pg_dump extraction.
///
/// No PVC mount — the container runs `pg_dump` over a TCP connection to the
/// CNPG cluster Service.  The keep-alive command (`sleep 3600`) lets the CLI
/// exec in and run the tool after the pod reaches Running.
pub fn pg_dump_pod_spec(name: &str, ns: &str, image: &str) -> Value {
    json!({
        "apiVersion": "v1",
        "kind": "Pod",
        "metadata": {
            "name": name,
            "namespace": ns,
            "labels": { "apprafter.io/backup-helper": "true" }
        },
        "spec": {
            "restartPolicy": "Never",
            "containers": [{
                "name": "dump",
                "image": image,
                "command": ["sleep", "3600"]
            }]
        }
    })
}

/// Build a Pod spec that mounts a PersistentVolumeClaim at `/data`.
///
/// `read_only` = `true` for backup extraction (read the volume tree out into
/// a tar stream); `false` for `LoadData` restore (write the tree into a
/// freshly-provisioned PVC).  The distinction matters for the
/// `persistentVolumeClaim.readOnly` field and the matching `volumeMounts`
/// entry — Kubernetes enforces the readOnly flag on the mount, and a
/// RWO PVC mounted read-write on restore allows `tar x` to write files (L1).
pub fn volume_pod_spec(name: &str, ns: &str, image: &str, pvc: &str, read_only: bool) -> Value {
    json!({
        "apiVersion": "v1",
        "kind": "Pod",
        "metadata": {
            "name": name,
            "namespace": ns,
            "labels": { "apprafter.io/backup-helper": "true" }
        },
        "spec": {
            "restartPolicy": "Never",
            "volumes": [{
                "name": "data",
                "persistentVolumeClaim": {
                    "claimName": pvc,
                    "readOnly": read_only
                }
            }],
            "containers": [{
                "name": "dump",
                "image": image,
                "command": ["sleep", "3600"],
                "volumeMounts": [{
                    "name": "data",
                    "mountPath": "/data",
                    "readOnly": read_only
                }]
            }]
        }
    })
}

// ---------------------------------------------------------------------------
// Impure forwarding helpers — delegate to KubeExec
// ---------------------------------------------------------------------------

/// Apply a Pod spec JSON and block until the pod reaches `Ready`.
/// Delegates to [`KubeExec::apply_and_wait_pod_ready`].
pub fn apply_and_wait_pod_ready(k: &dyn KubeExec, spec: &Value) -> Result<()> {
    k.apply_and_wait_pod_ready(spec)
}

/// Stream data DOWN from a pod to a local file (backup extraction).
/// Delegates to [`KubeExec::exec_stream_to_file`].
pub fn exec_stream_to_file(
    k: &dyn KubeExec,
    pod: &str,
    ns: &str,
    argv: &[&str],
    out_path: &Path,
) -> Result<()> {
    k.exec_stream_to_file(pod, ns, argv, out_path)
}

/// Stream data UP from a local file into a pod (restore / LoadData L2).
/// Delegates to [`KubeExec::exec_stream_from_file`].
pub fn exec_stream_from_file(
    k: &dyn KubeExec,
    pod: &str,
    ns: &str,
    argv: &[&str],
    in_path: &Path,
) -> Result<()> {
    k.exec_stream_from_file(pod, ns, argv, in_path)
}

/// Delete the helper pod, ignoring errors (best-effort cleanup guard).
/// Delegates to [`KubeExec::delete_pod_best_effort`].
pub fn delete_pod_best_effort(k: &dyn KubeExec, name: &str, ns: &str) {
    k.delete_pod_best_effort(name, ns)
}

// ---------------------------------------------------------------------------
// Tests (pure pod-spec builders only — impure fns require a live cluster)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pg_dump_pod_uses_pg_image_and_no_pvc_mount() {
        let p = pg_dump_pod_spec("bk-pg-alpha", "demo", "postgres:16-alpine");
        assert_eq!(p["spec"]["containers"][0]["image"], "postgres:16-alpine");
        assert_eq!(p["metadata"]["namespace"], "demo");
        assert!(p["spec"]["containers"][0]["command"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c == "sleep"));
        assert!(p["spec"].get("volumes").is_none());
        assert_eq!(p["spec"]["restartPolicy"], "Never");
    }

    #[test]
    fn volume_pod_mounts_pvc_read_only_for_backup() {
        let p = volume_pod_spec(
            "bk-vol-data",
            "demo",
            "busybox:1.36",
            "sv-demo-shared",
            true,
        );
        let vol = &p["spec"]["volumes"][0];
        assert_eq!(vol["persistentVolumeClaim"]["claimName"], "sv-demo-shared");
        assert_eq!(vol["persistentVolumeClaim"]["readOnly"], true);
        assert_eq!(
            p["spec"]["containers"][0]["volumeMounts"][0]["readOnly"],
            true
        );
    }

    #[test]
    fn volume_pod_rw_for_restore_load() {
        // L1: LoadData must WRITE the tree into the fresh PVC → read_only=false
        let p = volume_pod_spec("ld-vol-data", "demo", "busybox:1.36", "claim-x", false);
        assert_eq!(
            p["spec"]["volumes"][0]["persistentVolumeClaim"]["readOnly"],
            false
        );
        assert_eq!(
            p["spec"]["containers"][0]["volumeMounts"][0]["readOnly"],
            false
        );
    }
}
