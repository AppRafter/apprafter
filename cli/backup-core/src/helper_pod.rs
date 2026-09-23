// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Ephemeral helper-pod spec builders (pure) + `KubeExec`-forwarding stream
//! helpers (impure) for 2.6d backup/restore data extraction and load.
//!
//! # Pure pod-spec builders
//!
//! `pg_dump_pod_spec` / `volume_pod_spec` / `nats_pod_spec` return
//! serde_json::Value Pod specs that are applied via `apply_and_wait_pod_ready`
//! and then exec'd into. Each runs [`keep_alive_command`] — `sleep` for the
//! run's deadline — as its container command, so the pod is there to exec
//! into. `restartPolicy: Never` ensures a single attempt; the caller tears
//! down with `delete_pod_best_effort` after the stream completes.
//!
//! # How long a helper pod lives
//!
//! When the `sleep` ends, the container exits and every exec still running in
//! it dies with exit code 137. So the keep-alive is a hard cap on each single
//! extraction or load, whatever else allows it — and it used to be a fixed
//! `sleep 3600`, so no one claim could take longer than an hour, even under a
//! Job deadline of six. It is now the run's deadline ([`run_deadline_of`]):
//! the scheduled runner passes its Job's `activeDeadlineSeconds`, and the CLI
//! the same cluster setting, so one number decides how long a backup may
//! take.
//!
//! For a helper pod nobody deleted (a runner or a CLI killed before its
//! cleanup ran), the keep-alive ends the pod's process, not the Pod: with
//! `restartPolicy: Never` the object stays behind, `Completed`, until
//! something deletes it. The next run that applies a helper pod of that name
//! (the same step for the same claim) fails on it: an unchanged spec applies,
//! and the pod never becomes Ready within the five-minute wait. A backup's
//! cleanup deletes it then, so it costs one failed run.
//!
//! # Impure forwarding helpers
//!
//! `apply_and_wait_pod_ready`, `exec_stream_to_file`, `exec_stream_from_file`,
//! `delete_pod_best_effort` delegate to the [`KubeExec`] trait so the engine
//! is portable across the CLI subprocess path and the future in-cluster runner.

use cli_core::Result;
use serde_json::{json, Value};
use std::path::Path;
use std::time::Duration;

use crate::kube::KubeExec;

// ---------------------------------------------------------------------------
// The run's deadline and the keep-alive it sets
// ---------------------------------------------------------------------------

/// The deadline of a backup run when `spec.backup.activeDeadlineSeconds` is
/// unset: the chart's default for the backup Job's `activeDeadlineSeconds`,
/// six hours. `scripts/check-backup-render.sh` asserts the two are the same
/// number.
pub const DEFAULT_RUN_DEADLINE: Duration = Duration::from_secs(21600);

/// The cluster's backup run deadline, read off `PlatformStack/default`:
/// `spec.backup.activeDeadlineSeconds`, else [`DEFAULT_RUN_DEADLINE`] — the
/// same resolution the chart makes for the backup Job. Pure.
///
/// A value that is not a positive whole number reads as unset: the CRD holds it
/// to 600 or more, so anything else is not a setting anyone made.
pub fn run_deadline_of(platformstack: Option<&Value>) -> Duration {
    platformstack
        .and_then(|ps| ps.pointer("/spec/backup/activeDeadlineSeconds"))
        .and_then(Value::as_u64)
        .filter(|secs| *secs > 0)
        .map(Duration::from_secs)
        .unwrap_or(DEFAULT_RUN_DEADLINE)
}

/// The container command that keeps a helper pod alive for `keep_alive`, the
/// run's deadline (see the module docs for why that is the right length).
pub fn keep_alive_command(keep_alive: Duration) -> Value {
    json!(["sleep", keep_alive.as_secs().max(1).to_string()])
}

// ---------------------------------------------------------------------------
// Pure pod-spec builders
// ---------------------------------------------------------------------------

/// Build a Pod spec for pg_dump extraction.
///
/// No PVC mount — the container runs `pg_dump` over a TCP connection to the
/// CNPG cluster Service. The keep-alive command ([`keep_alive_command`]) lets
/// the caller exec in and run the tool after the pod reaches Running.
pub fn pg_dump_pod_spec(name: &str, ns: &str, image: &str, keep_alive: Duration) -> Value {
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
                "command": keep_alive_command(keep_alive)
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
pub fn volume_pod_spec(
    name: &str,
    ns: &str,
    image: &str,
    pvc: &str,
    read_only: bool,
    keep_alive: Duration,
) -> Value {
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
                "command": keep_alive_command(keep_alive),
                "volumeMounts": [{
                    "name": "data",
                    "mountPath": "/data",
                    "readOnly": read_only
                }]
            }]
        }
    })
}

/// Build a Pod spec that runs the `nats` CLI against one server (2.6d-6).
///
/// No volumes: a JetStream stream is read over the NATS wire, not off a disk,
/// so this pod needs coordinates and credentials and nothing else. Both go in
/// the container ENV rather than on the command line — the `nats` CLI reads
/// `NATS_URL` / `NATS_USER` / `NATS_PASSWORD` natively, and an argv carrying
/// the manager password would show up in `ps` inside the pod and in any
/// `kubectl exec` audit entry. Same reasoning as `PGPASSWORD` above.
///
/// The pod belongs in the NAMESPACE NATS runs in: the manager Secret lives
/// there, and the default-deny NetworkPolicy bundle (`default` namespace only)
/// leaves same-namespace traffic to the server alone.
pub fn nats_pod_spec(
    name: &str,
    ns: &str,
    image: &str,
    url: &str,
    user: &str,
    password: &str,
    keep_alive: Duration,
) -> Value {
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
                "command": keep_alive_command(keep_alive),
                "env": [
                    { "name": "NATS_URL", "value": url },
                    { "name": "NATS_USER", "value": user },
                    { "name": "NATS_PASSWORD", "value": password }
                ]
            }]
        }
    })
}

/// POSIX-safe single-quote of an arbitrary string for embedding in a `sh -c`
/// script (wraps in single quotes, escaping embedded single quotes).
///
/// Shared by both sides of the backup: `extract` quotes stream names into the
/// dump script, `restore` quotes generated passwords into the Dragonfly load
/// script. It was private to the restore side until 2.6d-6 needed the same
/// rule — and two quoting functions in one repository is how one of them ends
/// up subtly different from the other.
pub fn shell_single_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
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
/// Delegates to [`KubeExec::exec_stream_to_file`], which says what
/// `first_output_within` bounds.
pub fn exec_stream_to_file(
    k: &dyn KubeExec,
    pod: &str,
    ns: &str,
    argv: &[&str],
    out_path: &Path,
    first_output_within: Option<std::time::Duration>,
) -> Result<()> {
    k.exec_stream_to_file(pod, ns, argv, out_path, first_output_within)
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
        let p = pg_dump_pod_spec(
            "bk-pg-alpha",
            "demo",
            "postgres:16-alpine",
            DEFAULT_RUN_DEADLINE,
        );
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
            DEFAULT_RUN_DEADLINE,
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
        let p = volume_pod_spec(
            "ld-vol-data",
            "demo",
            "busybox:1.36",
            "claim-x",
            false,
            DEFAULT_RUN_DEADLINE,
        );
        assert_eq!(
            p["spec"]["volumes"][0]["persistentVolumeClaim"]["readOnly"],
            false
        );
        assert_eq!(
            p["spec"]["containers"][0]["volumeMounts"][0]["readOnly"],
            false
        );
    }

    #[test]
    fn every_helper_pod_keeps_itself_alive_for_the_run_deadline_it_is_given() {
        // Not an hour: the `sleep` ending kills every exec in the pod, so a
        // fixed hour capped every single extraction at an hour, whatever the
        // Job deadline said.
        let twelve_hours = Duration::from_secs(43200);
        let want = json!(["sleep", "43200"]);
        for spec in [
            pg_dump_pod_spec("bk-pg-db", "demo", "postgres:18-alpine", twelve_hours),
            volume_pod_spec(
                "bk-vol-v",
                "demo",
                "busybox:1.36",
                "pvc",
                true,
                twelve_hours,
            ),
            nats_pod_spec(
                "bk-js-s",
                "nats",
                "nats:2",
                "nats://n:4222",
                "u",
                "p",
                twelve_hours,
            ),
        ] {
            assert_eq!(spec["spec"]["containers"][0]["command"], want, "{spec}");
        }
    }

    #[test]
    fn the_run_deadline_is_the_clusters_setting_or_six_hours() {
        assert_eq!(DEFAULT_RUN_DEADLINE, Duration::from_secs(6 * 3600));
        let set = json!({"spec": {"backup": {"activeDeadlineSeconds": 43200}}});
        assert_eq!(run_deadline_of(Some(&set)), Duration::from_secs(43200));
        for unset in [
            json!({"spec": {"backup": {"enabled": true}}}),
            json!({"spec": {}}),
            json!({"spec": {"backup": {"activeDeadlineSeconds": 0}}}),
            json!({"spec": {"backup": {"activeDeadlineSeconds": "12h"}}}),
        ] {
            assert_eq!(
                run_deadline_of(Some(&unset)),
                DEFAULT_RUN_DEADLINE,
                "{unset}"
            );
        }
        assert_eq!(run_deadline_of(None), DEFAULT_RUN_DEADLINE);
    }
}
