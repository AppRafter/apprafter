// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! 2.6d extraction orchestration: enumerate which `(ns, claim, DataKind,
//! source)` to dump from a list of ResourceClaim JSONs, then drive the
//! ephemeral helper pods (from `helper_pod`) to pull each artifact out.
//!
//! # Structure
//!
//! * `plan_extraction` — **pure** enumerator: walks the claim list and emits
//!   an `ExtractItem` per claim that has dumpable state.  The only piece that
//!   is unit-tested.
//! * `run_extraction` — **impure** driver: applies helper pods, reads
//!   connection Secrets via `KubeExec`, execs `pg_dump` / `tar`, streams the
//!   output to local files, tears down pods.  Walk-validated; not unit-tested.
//!
//! # Pg credential delivery (`PGPASSWORD`)
//!
//! The connection Secret (written by the provisioner at `status.connectionSecretRef`)
//! carries decomposed keys `user`, `pass`, `host`, `port`, `db` — all
//! `stringData` fields, stored base64 under `data`.  `run_extraction` reads
//! each key via `KubeExec::get_secret_key` (which returns the base64-decoded
//! string).  The password is then injected as the `PGPASSWORD` environment
//! variable INTO THE HELPER POD SPEC (not via `kubectl exec --env`), so
//! `pg_dump` reads it from the env and suppresses the interactive prompt.

use std::fs;
use std::path::Path;

use cli_core::{CliError, Result};
use serde_json::{json, Value};

use crate::helper_pod::{
    apply_and_wait_pod_ready, delete_pod_best_effort, exec_stream_to_file, volume_pod_spec,
};
use crate::images;
use crate::kube::KubeExec;
use crate::DataKind;

// ---------------------------------------------------------------------------
// Pure data types
// ---------------------------------------------------------------------------

/// The claim types this build stores NO data for, even though the claim itself
/// is captured and replayed (A6).
///
/// `jetstream` only, and deliberately only. `clickhouse` and `s3` fall through
/// the same arm of [`plan_extraction`], but neither ships, so a claim of those
/// types provisions nothing and there is no data to miss — announcing them
/// would be a warning about a situation that cannot occur. JetStream ships, so
/// a cluster can hold real streams today, and a snapshot that listed the claim
/// without saying this reads as complete while being short of the data.
///
/// Capturing the streams is separate, larger work (a helper pod running `nats
/// stream backup`) and is NOT in progress — nothing here should be read as a
/// promise of it.
pub const CLAIM_TYPES_WITHOUT_DATA_CAPTURE: &[&str] = &["jetstream"];

/// Whether a claim of this `spec.type` has NO data in the backup. Pure.
///
/// The one place the rule lives: the manifest marker, the `backup create`
/// summary and `backup show` all read it, so they cannot come to different
/// conclusions about the same claim.
pub fn claim_type_has_no_data_capture(claim_type: &str) -> bool {
    CLAIM_TYPES_WITHOUT_DATA_CAPTURE.contains(&claim_type)
}

/// A claim a run captures as configuration only — the counterpart of an
/// [`ExtractItem`], and what the summaries name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UncapturedClaim {
    pub namespace: String,
    pub name: String,
    pub claim_type: String,
}

/// The claims in scope whose native data this build does not capture. Pure.
///
/// Walks the same list as [`plan_extraction`] and reports what that planner
/// passes over, so the two cannot drift: a type added to the planner and left
/// in [`CLAIM_TYPES_WITHOUT_DATA_CAPTURE`] would be reported as empty while
/// carrying data, and the test pair asserts they stay disjoint.
pub fn claims_without_data_capture(claims: &[Value]) -> Vec<UncapturedClaim> {
    claims
        .iter()
        .filter_map(|c| {
            let ty = c.pointer("/spec/type").and_then(Value::as_str)?;
            if !claim_type_has_no_data_capture(ty) {
                return None;
            }
            Some(UncapturedClaim {
                namespace: c
                    .pointer("/metadata/namespace")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                name: c
                    .pointer("/metadata/name")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                claim_type: ty.to_string(),
            })
        })
        .collect()
}

/// One unit of extraction: the data to pull from a single claim.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExtractItem {
    pub namespace: String,
    pub claim_name: String,
    pub kind: DataKind,
    /// `Pg`     → `status.connectionSecretRef` (the connection Secret name).
    /// `Volume` → `status.volumeClaimRef` (the PVC name).
    /// `Redis`  → `status.instance` (the Dragonfly pool instance; snapshot PVC is `df-<instance>-0`).
    pub source: String,
}

// ---------------------------------------------------------------------------
// Pure extraction planner (unit-tested)
// ---------------------------------------------------------------------------

/// Plan which claims to extract and how.
///
/// Rules per `spec.type`:
/// * `pg` → always emit a `Pg` item; source = `status.connectionSecretRef`.
/// * `redis` → only when `spec.persistent == true` AND provisioned (has
///   `status.instance`); source = `status.instance` (the Dragonfly pool
///   instance whose snapshot PVC is `df-<instance>-0`).
/// * `disk` / `shared-disk` → emit a `Volume` item; source = `status.volumeClaimRef`.
/// * everything else → no item; see [`claims_without_data_capture`], which
///   reports the shipped ones so the omission is stated rather than silent.
///
/// Claims missing the required `status` reference are also skipped: they were
/// never provisioned successfully and have no data to dump.
pub fn plan_extraction(claims: &[Value]) -> Vec<ExtractItem> {
    let mut items = Vec::new();
    for c in claims {
        let ns = c
            .pointer("/metadata/namespace")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let name = c
            .pointer("/metadata/name")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let ty = c
            .pointer("/spec/type")
            .and_then(Value::as_str)
            .unwrap_or("");

        match ty {
            "pg" => {
                if let Some(src) = c
                    .pointer("/status/connectionSecretRef")
                    .and_then(Value::as_str)
                {
                    items.push(ExtractItem {
                        namespace: ns,
                        claim_name: name,
                        kind: DataKind::Pg,
                        source: src.to_string(),
                    });
                }
            }
            "redis" => {
                let persistent = c
                    .pointer("/spec/persistent")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                if persistent {
                    // The Dragonfly pool instance this claim is bound to (status.instance);
                    // its snapshot lives on PVC df-<instance>-0. A persistent claim not yet
                    // provisioned has no status.instance and no data to dump — skip it.
                    if let Some(instance) = c.pointer("/status/instance").and_then(Value::as_str) {
                        items.push(ExtractItem {
                            namespace: ns,
                            claim_name: name,
                            kind: DataKind::Redis,
                            source: instance.to_string(),
                        });
                    }
                }
                // Non-persistent redis (ephemeral): no snapshot worth keeping.
            }
            "disk" | "shared-disk" => {
                if let Some(src) = c.pointer("/status/volumeClaimRef").and_then(Value::as_str) {
                    items.push(ExtractItem {
                        namespace: ns,
                        claim_name: name,
                        kind: DataKind::Volume,
                        source: src.to_string(),
                    });
                }
            }
            _ => {
                // jetstream / clickhouse / s3 / notifications — no extraction
                // path. NOT silent for the ones that ship:
                // `claims_without_data_capture` reports them, the manifest
                // marks them `no_data`, and both `backup create` and
                // `backup show` say so.
            }
        }
    }
    items
}

// ---------------------------------------------------------------------------
// Impure extraction driver (walk-validated; not unit-tested)
// ---------------------------------------------------------------------------

/// Build a `pg_dump` helper Pod spec with `PGPASSWORD` injected into the
/// container environment so `pg_dump` never needs an interactive prompt.
/// All other fields mirror `helper_pod::pg_dump_pod_spec`.
pub(crate) fn pg_dump_pod_spec_with_password(
    name: &str,
    ns: &str,
    image: &str,
    password: &str,
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
                "command": ["sleep", "3600"],
                "env": [{
                    "name": "PGPASSWORD",
                    "value": password
                }]
            }]
        }
    })
}

/// Drive all extraction items to completion, writing each artifact under
/// `out_dir`:
///
/// ```text
/// <out_dir>/pg/<ns>/<claim>.dump           (pg_dump custom format, -Fc)
/// <out_dir>/volumes/<ns>/<claim>/data.tar  (tar c -C /data .)
/// <out_dir>/redis/<ns>/<claim>/dump.tar    (tar c -C /dragonfly/snapshots .)
/// ```
///
/// For each item:
///
/// * **Pg** — reads the connection Secret (decomposed keys `user`, `pass`,
///   `host`, `port`, `db`) via `k.get_secret_key`; applies a helper pod with
///   `PGPASSWORD` injected into its container env; execs `pg_dump -Fc -h
///   <host> -U <user> -p <port> <db>` and streams stdout to
///   `pg/<ns>/<claim>.dump`; deletes the pod (best-effort).
///
/// * **Volume** — applies a busybox pod that mounts `item.source` (the PVC
///   name) at `/data` read-only; execs `tar c -C /data .` and streams to
///   `volumes/<ns>/<claim>/data.tar`; deletes the pod.
///
/// * **Redis** — persistent Dragonfly snapshot; `item.source` is the claim's
///   `status.instance` (the Dragonfly pool instance) whose pod is
///   `<instance>-0` in `dragonfly-system`. Execs a synchronous `SAVE` on the
///   admin port 9999 (no password) so the snapshot is current, then `tar`s the
///   whole `/dragonfly/snapshots` dir (the snapshot is a multi-file `.dfs`
///   set) to `redis/<ns>/<claim>/dump.tar`. No helper pod — the Dragonfly pod
///   is already running with the PVC mounted and carries `redis-cli` + `tar`.
///
/// The helper-pod names are deterministic (`bk-<kind>-<claim>`) and include
/// a truncation to stay under the 63-char DNS-1123 limit.
///
/// `pg_image` controls the `postgres:<major>-alpine` image used for the pg
/// dump helper.  Pass `images::DEFAULT_PG_IMAGE` for the tier-1 default
/// (pg 16); `images::pg_helper_image(Some(server_image_name))` when the CNPG
/// Cluster's `spec.imageName` is known.
pub fn run_extraction(
    k: &dyn KubeExec,
    items: &[ExtractItem],
    out_dir: &Path,
    pg_image: &str,
) -> Result<()> {
    for item in items {
        match item.kind {
            DataKind::Pg => extract_pg(k, item, out_dir, pg_image)?,
            DataKind::Volume => extract_volume(k, item, out_dir)?,
            DataKind::Redis => extract_redis(k, item, out_dir)?,
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Drop guard — guarantees helper-pod cleanup on every return path
// ---------------------------------------------------------------------------

/// Deletes a helper pod on drop — guarantees cleanup on every return path
/// (apply-wait failure, exec failure, success, or panic).
///
/// Construct the guard RIGHT AFTER the pod name is known and BEFORE
/// `apply_and_wait_pod_ready`, so even an apply-wait timeout can't leave a
/// stalled `bk-pg-*`/`bk-vol-*` pod behind.
struct HelperPodGuard<'a> {
    name: String,
    namespace: &'a str,
    k: &'a dyn KubeExec,
}

impl Drop for HelperPodGuard<'_> {
    fn drop(&mut self) {
        delete_pod_best_effort(self.k, &self.name, self.namespace);
    }
}

// ---------------------------------------------------------------------------
// Per-kind extraction helpers (private)
// ---------------------------------------------------------------------------

/// Extract a single `Pg` claim.
///
/// 1. Read connection creds from the connection Secret via `k.get_secret_key`.
/// 2. Apply a pg-dump helper pod with `PGPASSWORD` in its container env.
/// 3. `k.exec_stream_to_file` → `pg_dump -Fc` → stream to
///    `out_dir/pg/<ns>/<claim>.dump`.
/// 4. Delete the pod (best-effort, via `HelperPodGuard` drop).
fn extract_pg(k: &dyn KubeExec, item: &ExtractItem, out_dir: &Path, pg_image: &str) -> Result<()> {
    let secret_name = &item.source; // status.connectionSecretRef
    let ns = &item.namespace;
    let claim = &item.claim_name;

    // 1. Read the decomposed connection Secret keys.
    let user = k.get_secret_key(secret_name, ns, "user")?;
    let pass = k.get_secret_key(secret_name, ns, "pass")?;
    let host = k.get_secret_key(secret_name, ns, "host")?;
    let port = k.get_secret_key(secret_name, ns, "port")?;
    let db = k.get_secret_key(secret_name, ns, "db")?;

    // 2. Build the pod name and arm the cleanup guard BEFORE apply-wait,
    //    so cleanup runs even if apply-wait times out or errors.
    let pod_name = truncate_pod_name(&format!("bk-pg-{claim}"));
    let _guard = HelperPodGuard {
        name: pod_name.clone(),
        namespace: ns,
        k,
    };
    let spec = pg_dump_pod_spec_with_password(&pod_name, ns, pg_image, &pass);
    apply_and_wait_pod_ready(k, &spec)?;

    // 3. Stream pg_dump output to disk.
    let dest = out_dir.join("pg").join(ns);
    fs::create_dir_all(&dest)
        .map_err(|e| CliError::Other(format!("create dir {}: {e}", dest.display())))?;
    let dump_path = dest.join(format!("{claim}.dump"));

    let owned = pg_dump_argv(&db, &user, &host, &port);
    let argv: Vec<&str> = owned.iter().map(String::as_str).collect();
    exec_stream_to_file(k, &pod_name, ns, &argv, &dump_path)
    // _guard drops here (or on any earlier return) → delete_pod_best_effort called.
}

/// Extract a single `Volume` (disk / shared-disk) claim.
///
/// 1. Apply a busybox pod that mounts `item.source` (the PVC) at `/data`
///    read-only.
/// 2. `k.exec_stream_to_file` → `tar c -C /data .` → stream to
///    `out_dir/volumes/<ns>/<claim>/data.tar`.
/// 3. Delete the pod (best-effort, via `HelperPodGuard` drop).
fn extract_volume(k: &dyn KubeExec, item: &ExtractItem, out_dir: &Path) -> Result<()> {
    let pvc_name = &item.source; // status.volumeClaimRef
    let ns = &item.namespace;
    let claim = &item.claim_name;

    // Arm the cleanup guard BEFORE apply-wait so cleanup runs even if the
    // pod never reaches Ready.
    let pod_name = truncate_pod_name(&format!("bk-vol-{claim}"));
    let _guard = HelperPodGuard {
        name: pod_name.clone(),
        namespace: ns,
        k,
    };
    let spec = volume_pod_spec(
        &pod_name,
        ns,
        images::VOLUME_IMAGE,
        pvc_name,
        true, // read-only for backup
    );
    apply_and_wait_pod_ready(k, &spec)?;

    let dest = out_dir.join("volumes").join(ns).join(claim);
    fs::create_dir_all(&dest)
        .map_err(|e| CliError::Other(format!("create dir {}: {e}", dest.display())))?;
    let tar_path = dest.join("data.tar");

    let argv: Vec<&str> = vec!["tar", "c", "-C", "/data", "."];
    exec_stream_to_file(k, &pod_name, ns, &argv, &tar_path)
    // _guard drops here (or on any earlier return) → delete_pod_best_effort called.
}

/// Extract a persistent Redis (Dragonfly) claim's whole-instance snapshot.
///
/// `item.source` is the claim's `status.instance` (the Dragonfly pool
/// instance). Its pod is `<instance>-0` in `dragonfly-system`, with the
/// snapshot PVC mounted at `/dragonfly/snapshots` and an admin port 9999
/// (no password). We exec a synchronous `SAVE` (so the snapshot is current,
/// not up-to-30-min stale) and then `tar` the whole snapshot dir — the
/// snapshot is a multi-file `.dfs` set, so the directory is the unit. No
/// helper pod: the Dragonfly pod is already running with the PVC mounted and
/// carries both `redis-cli` and `tar`.
fn extract_redis(k: &dyn KubeExec, item: &ExtractItem, out_dir: &Path) -> Result<()> {
    let instance = &item.source; // status.instance
    let ns = &item.namespace; // the app/claim namespace — the backup key
    let claim = &item.claim_name;

    let pod = format!("{instance}-0");
    let df_ns = "dragonfly-system";

    let dest = out_dir.join("redis").join(ns).join(claim);
    fs::create_dir_all(&dest)
        .map_err(|e| CliError::Other(format!("create dir {}: {e}", dest.display())))?;
    let tar_path = dest.join("dump.tar");

    // SAVE (admin port 9999, nopass) then tar the snapshot dir. Discard SAVE's
    // stdout so only the tarball reaches the file.
    let argv: Vec<&str> = vec![
        "sh",
        "-c",
        "redis-cli -p 9999 SAVE >/dev/null 2>&1 && tar c -C /dragonfly/snapshots .",
    ];
    exec_stream_to_file(k, &pod, df_ns, &argv, &tar_path)
}

/// Build the `pg_dump` argument vector for a custom-format dump.
///
/// Returns a `Vec<String>` so the caller is not constrained by the lifetime of
/// local credential variables.  The caller converts to `Vec<&str>` immediately
/// before passing to `exec_stream_to_file`.
pub fn pg_dump_argv(db: &str, user: &str, host: &str, port: &str) -> Vec<String> {
    vec![
        "pg_dump".to_string(),
        "-Fc".to_string(),
        "--compress=0".to_string(),
        "-h".to_string(),
        host.to_string(),
        "-U".to_string(),
        user.to_string(),
        "-p".to_string(),
        port.to_string(),
        db.to_string(),
    ]
}

/// Truncate a pod name to 63 chars (DNS-1123 label limit) and strip any
/// trailing `-` left by the truncation.
pub fn truncate_pod_name(name: &str) -> String {
    let mut s = name.to_string();
    s.truncate(63);
    while s.ends_with('-') {
        s.pop();
    }
    s
}

// ---------------------------------------------------------------------------
// Tests (pure planner only — impure fns require a live cluster)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn plans_pg_volume_and_persistent_redis_only() {
        let claims = vec![
            json!({"spec":{"type":"pg"},"metadata":{"name":"alpha-pg","namespace":"demo"},
                   "status":{"connectionSecretRef":"alpha-pg-conn","ready":true}}),
            json!({"spec":{"type":"disk"},"metadata":{"name":"alpha-disk","namespace":"demo"},
                   "status":{"volumeClaimRef":"claim-demo-alpha-disk","ready":true}}),
            json!({"spec":{"type":"redis","persistent":false},"metadata":{"name":"a-r","namespace":"demo"},
                   "status":{"ready":true}}),
        ];
        let plan = plan_extraction(&claims);
        let kinds: Vec<_> = plan.iter().map(|i| i.kind).collect();
        assert!(kinds.contains(&DataKind::Pg));
        assert!(kinds.contains(&DataKind::Volume));
        assert!(!kinds.contains(&DataKind::Redis)); // non-persistent redis skipped
    }

    #[test]
    fn persistent_redis_is_planned() {
        let claims = vec![json!({"spec":{"type":"redis","persistent":true},
            "metadata":{"name":"r","namespace":"demo"},
            "status":{"ready":true,"instance":"platform-redis-persistent-000"}})];
        let plan = plan_extraction(&claims);
        assert!(plan.iter().any(|i| i.kind == DataKind::Redis));
    }

    #[test]
    fn pg_source_is_connection_secret_ref_disk_is_volume_claim_ref() {
        let claims = vec![
            json!({"spec":{"type":"pg"},"metadata":{"name":"p","namespace":"demo"},
                   "status":{"connectionSecretRef":"p-conn"}}),
            json!({"spec":{"type":"shared-disk"},"metadata":{"name":"d","namespace":"demo"},
                   "status":{"volumeClaimRef":"sv-demo-shared"}}),
        ];
        let plan = plan_extraction(&claims);
        let pg = plan.iter().find(|i| i.kind == DataKind::Pg).unwrap();
        assert_eq!(pg.source, "p-conn");
        let vol = plan.iter().find(|i| i.kind == DataKind::Volume).unwrap();
        assert_eq!(vol.source, "sv-demo-shared");
    }

    #[test]
    fn pg_missing_connection_secret_ref_is_skipped() {
        // A claim that provisioning never completed has no connectionSecretRef;
        // it should not appear in the plan (no data to dump).
        let claims = vec![json!({
            "spec": { "type": "pg" },
            "metadata": { "name": "p", "namespace": "demo" },
            "status": {}
        })];
        let plan = plan_extraction(&claims);
        assert!(
            plan.is_empty(),
            "unprovisioned pg claim must be skipped: {plan:?}"
        );
    }

    #[test]
    fn disk_missing_volume_claim_ref_is_skipped() {
        let claims = vec![json!({
            "spec": { "type": "disk" },
            "metadata": { "name": "d", "namespace": "demo" },
            "status": {}
        })];
        assert!(plan_extraction(&claims).is_empty());
    }

    #[test]
    fn unknown_type_is_silently_skipped() {
        let claims = vec![json!({
            "spec": { "type": "s3" },
            "metadata": { "name": "s", "namespace": "demo" },
            "status": {}
        })];
        assert!(plan_extraction(&claims).is_empty());
    }

    #[test]
    fn persistent_redis_source_is_status_instance() {
        // The source is the Dragonfly pool instance (status.instance) whose
        // snapshot PVC is df-<instance>-0; the claim name remains the backup key.
        let claims = vec![json!({
            "spec": { "type": "redis", "persistent": true },
            "metadata": { "name": "my-cache", "namespace": "prod" },
            "status": { "ready": true, "instance": "platform-redis-persistent-000" }
        })];
        let plan = plan_extraction(&claims);
        assert_eq!(plan.len(), 1);
        assert_eq!(plan[0].source, "platform-redis-persistent-000");
        assert_eq!(plan[0].claim_name, "my-cache");
        assert_eq!(plan[0].namespace, "prod");
    }

    #[test]
    fn persistent_redis_without_status_instance_is_skipped() {
        // A persistent redis claim that never provisioned has no
        // status.instance — no Dragonfly snapshot exists, so skip it.
        let claims = vec![json!({
            "spec": { "type": "redis", "persistent": true },
            "metadata": { "name": "my-cache", "namespace": "prod" },
            "status": { "ready": true }
        })];
        assert!(
            plan_extraction(&claims).is_empty(),
            "unprovisioned persistent redis (no status.instance) must be skipped"
        );
    }

    #[test]
    fn pg_dump_argv_disables_dump_compression_so_restic_dedups() {
        let argv = pg_dump_argv("appdb", "approle", "hostx", "5432");
        assert!(
            argv.iter().any(|a| a == "--compress=0"),
            "argv missing --compress=0: {argv:?}"
        );
        assert!(argv.iter().any(|a| a == "-Fc"), "argv: {argv:?}");
    }

    #[test]
    fn truncate_pod_name_stays_under_64_chars() {
        let long = "bk-pg-".to_string() + &"a".repeat(100);
        let t = truncate_pod_name(&long);
        assert!(t.len() <= 63, "pod name too long: {}", t.len());
        assert!(!t.ends_with('-'), "trailing dash: {t}");
    }

    // =======================================================================
    // A6 — the claims a backup lists but holds no data for
    // =======================================================================

    /// FIRES: a jetstream claim produces no extraction item AND is reported,
    /// which together are the finding. Producing no item is the old behaviour;
    /// being reported is what stops the snapshot reading as complete.
    #[test]
    fn a_jetstream_claim_is_extracted_by_nothing_and_reported_by_name() {
        let claims = vec![json!({
            "spec": {"type": "jetstream"},
            "metadata": {"name": "events", "namespace": "demo"},
            "status": {"ready": true, "account": "demo"}
        })];
        assert!(plan_extraction(&claims).is_empty());
        assert_eq!(
            claims_without_data_capture(&claims),
            vec![UncapturedClaim {
                namespace: "demo".into(),
                name: "events".into(),
                claim_type: "jetstream".into(),
            }]
        );
    }

    /// DOES NOT FIRE for the types whose data IS captured — a warning on a pg
    /// claim would be a lie, and one on every claim is a warning nobody reads.
    #[test]
    fn a_claim_whose_data_is_captured_is_never_reported_as_dataless() {
        let claims = vec![
            json!({"spec": {"type": "pg"}, "metadata": {"name": "db", "namespace": "demo"},
                   "status": {"connectionSecretRef": "db-conn"}}),
            json!({"spec": {"type": "disk"}, "metadata": {"name": "vol", "namespace": "demo"},
                   "status": {"volumeClaimRef": "pvc"}}),
            json!({"spec": {"type": "redis", "persistent": true},
                   "metadata": {"name": "cache", "namespace": "demo"},
                   "status": {"instance": "platform-redis-persistent-000"}}),
        ];
        assert_eq!(plan_extraction(&claims).len(), 3);
        assert!(claims_without_data_capture(&claims).is_empty());
    }

    /// The narrowing, stated as a test: `clickhouse` and `s3` fall through the
    /// same arm, and are deliberately NOT announced — neither ships, so such a
    /// claim provisions nothing and there is no data to miss. A warning about
    /// them would be a warning about an impossible situation.
    #[test]
    fn only_the_shipped_dataless_type_is_announced() {
        assert_eq!(CLAIM_TYPES_WITHOUT_DATA_CAPTURE, &["jetstream"]);
        for ty in ["clickhouse", "s3", "notifications"] {
            let claims = vec![json!({
                "spec": {"type": ty},
                "metadata": {"name": "x", "namespace": "demo"}
            })];
            assert!(plan_extraction(&claims).is_empty(), "{ty}");
            assert!(claims_without_data_capture(&claims).is_empty(), "{ty}");
        }
    }

    /// The two lists must stay disjoint: a type the planner learns to extract
    /// while it is still named here would be reported as empty while its data
    /// is in the snapshot — the same dishonesty pointing the other way.
    #[test]
    fn the_dataless_types_are_disjoint_from_what_the_planner_extracts() {
        for ty in CLAIM_TYPES_WITHOUT_DATA_CAPTURE {
            let claims = vec![json!({
                "spec": {"type": ty, "persistent": true},
                "metadata": {"name": "x", "namespace": "demo"},
                // Every source reference the planner could possibly key on,
                // so the claim fails to extract because of its TYPE and not
                // because it looks unprovisioned.
                "status": {"connectionSecretRef": "c", "volumeClaimRef": "p", "instance": "i"}
            })];
            assert!(
                plan_extraction(&claims).is_empty(),
                "{ty} is announced as dataless but the planner extracts it"
            );
        }
    }

    /// An unprovisioned claim of a captured type is still skipped silently,
    /// and that is NOT what this reports: it is a different case (the claim
    /// has no data yet, rather than no capture path) and it is out of scope
    /// here. Pinned so the two are not later conflated.
    #[test]
    fn an_unprovisioned_claim_is_not_reported_as_dataless() {
        let claims = vec![json!({
            "spec": {"type": "pg"},
            "metadata": {"name": "db", "namespace": "demo"}
        })];
        assert!(plan_extraction(&claims).is_empty());
        assert!(claims_without_data_capture(&claims).is_empty());
    }
}
