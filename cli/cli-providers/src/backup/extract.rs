// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! 2.6d extraction orchestration: enumerate which `(ns, claim, DataKind,
//! source)` to dump from a list of ResourceClaim JSONs, then drive the
//! ephemeral helper pods (from `helper_pod.rs`) to pull each artifact out.
//!
//! # Structure
//!
//! * `plan_extraction` — **pure** enumerator: walks the claim list and emits
//!   an `ExtractItem` per claim that has dumpable state.  The only piece that
//!   is unit-tested.
//! * `run_extraction` — **impure** driver: applies helper pods, reads
//!   connection Secrets via `kubectl`, execs `pg_dump` / `tar`, streams the
//!   output to local files, tears down pods.  Walk-validated; not unit-tested.
//!
//! # Pg credential delivery (`PGPASSWORD`)
//!
//! The connection Secret (written by the provisioner at `status.connectionSecretRef`)
//! carries decomposed keys `user`, `pass`, `host`, `port`, `db` — all
//! `stringData` fields, stored base64 under `data`.  `run_extraction` reads
//! each key via `kubectl get secret … -o jsonpath={.data.<key>}` (which
//! returns the base64-encoded value) and decodes it.  The password is then
//! injected as the `PGPASSWORD` environment variable INTO THE HELPER POD SPEC
//! (not via `kubectl exec --env`), so `pg_dump` never has to pass it on the
//! command line.  `pg_dump` reads `PGPASSWORD` from the env and suppresses
//! the interactive password prompt.

use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};

use base64::Engine as _;
use cli_core::{CliError, Result};
use serde_json::{json, Value};

use crate::backup::helper_pod::{
    apply_and_wait_pod_ready, delete_pod_best_effort, exec_stream_to_file, volume_pod_spec,
};
use crate::backup::images;
use crate::backup::DataKind;

// ---------------------------------------------------------------------------
// Pure data types
// ---------------------------------------------------------------------------

/// One unit of extraction: the data to pull from a single claim.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExtractItem {
    pub namespace: String,
    pub claim_name: String,
    pub kind: DataKind,
    /// `Pg`     → `status.connectionSecretRef` (the connection Secret name).
    /// `Volume` → `status.volumeClaimRef` (the PVC name).
    /// `Redis`  → the claim name itself (snapshot PVC resolved at run-time).
    pub source: String,
}

// ---------------------------------------------------------------------------
// Pure extraction planner (unit-tested)
// ---------------------------------------------------------------------------

/// Plan which claims to extract and how.
///
/// Rules per `spec.type`:
/// * `pg` → always emit a `Pg` item; source = `status.connectionSecretRef`.
/// * `redis` → only when `spec.persistent == true`; source = claim name
///   (the exact snapshot PVC path is resolved at run-time, T12).
/// * `disk` / `shared-disk` → emit a `Volume` item; source = `status.volumeClaimRef`.
/// * everything else → silently skipped (out of scope this cycle).
///
/// Claims missing the required `status` reference are also silently skipped:
/// they were never provisioned successfully and have no data to dump.
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
                    // The exact Dragonfly snapshot PVC name is resolved at
                    // run-time in T12; the claim name is sufficient as the
                    // plan-level identifier.
                    items.push(ExtractItem {
                        namespace: ns,
                        claim_name: name.clone(),
                        kind: DataKind::Redis,
                        source: name,
                    });
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
                // jetstream / clickhouse / s3 / notifications — out of scope
                // this cycle; skip silently.
            }
        }
    }
    items
}

// ---------------------------------------------------------------------------
// Impure extraction driver (walk-validated; not unit-tested)
// ---------------------------------------------------------------------------

/// Read a single `data.<key>` field from a Kubernetes Secret via `kubectl`
/// and return it base64-decoded as a UTF-8 string.
///
/// The Secret's `stringData` fields are stored under `data` as standard
/// base64 values; `kubectl get secret … -o jsonpath={.data.<key>}` returns
/// the raw base64 token (no trailing newline guarantee — we trim before
/// decoding).
fn read_secret_key(secret_name: &str, ns: &str, key: &str, kubeconfig: &Path) -> Result<String> {
    let out = Command::new("kubectl")
        .args([
            "get",
            "secret",
            secret_name,
            "-n",
            ns,
            "-o",
            &format!("jsonpath={{.data.{key}}}"),
        ])
        .env("KUBECONFIG", kubeconfig)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| CliError::Other(format!("spawn kubectl get secret: {e}")))?;

    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        return Err(CliError::Other(format!(
            "kubectl get secret {secret_name} -n {ns} -o jsonpath={{.data.{key}}} \
             failed (exit {:?}): {stderr}",
            out.status.code()
        )));
    }

    let b64 = String::from_utf8(out.stdout)
        .map_err(|e| CliError::Other(format!("kubectl get secret stdout not utf-8: {e}")))?;

    let decoded = base64::engine::general_purpose::STANDARD
        .decode(b64.trim())
        .map_err(|e| {
            CliError::Other(format!(
                "decode secret {secret_name}/{key} (value was not valid base64): {e}"
            ))
        })?;

    String::from_utf8(decoded)
        .map_err(|e| CliError::Other(format!("secret {secret_name}/{key} is not utf-8: {e}")))
}

/// Build a `pg_dump` helper Pod spec with `PGPASSWORD` injected into the
/// container environment so `pg_dump` never needs an interactive prompt.
/// All other fields mirror `helper_pod::pg_dump_pod_spec`.
fn pg_dump_pod_spec_with_password(name: &str, ns: &str, image: &str, password: &str) -> Value {
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
/// <out_dir>/redis/<ns>/<claim>/            (documented skeleton; T12)
/// ```
///
/// For each item:
///
/// * **Pg** — reads the connection Secret (decomposed keys `user`, `pass`,
///   `host`, `port`, `db`) via `kubectl get secret -o jsonpath`; applies a
///   helper pod with `PGPASSWORD` injected into its container env; execs
///   `pg_dump -Fc -h <host> -U <user> -p <port> <db>` and streams stdout
///   to `pg/<ns>/<claim>.dump`; deletes the pod (best-effort).
///
/// * **Volume** — applies a busybox pod that mounts `item.source` (the PVC
///   name) at `/data` read-only; execs `tar c -C /data .` and streams to
///   `volumes/<ns>/<claim>/data.tar`; deletes the pod.
///
/// * **Redis** — persistent Dragonfly snapshot; the exact snapshot PVC path
///   (and whether to trigger `BGSAVE` first) is verified on the live walk
///   in T12.  This cycle emits a log note and skips, preserving the item in
///   the returned plan so callers can surface a "not yet extracted" warning.
///
/// The helper-pod names are deterministic (`bk-<kind>-<claim>`) and include
/// a truncation to stay under the 63-char DNS-1123 limit.
///
/// `pg_image` controls the `postgres:<major>-alpine` image used for the pg
/// dump helper.  Pass `images::DEFAULT_PG_IMAGE` for the tier-1 default
/// (pg 16); `images::pg_helper_image(Some(server_image_name))` when the CNPG
/// Cluster's `spec.imageName` is known.
pub fn run_extraction(
    items: &[ExtractItem],
    out_dir: &Path,
    kubeconfig: &Path,
    pg_image: &str,
) -> Result<()> {
    for item in items {
        match item.kind {
            DataKind::Pg => extract_pg(item, out_dir, kubeconfig, pg_image)?,
            DataKind::Volume => extract_volume(item, out_dir, kubeconfig)?,
            DataKind::Redis => {
                // Persistent Dragonfly snapshot extraction is verified on the
                // live walk in T12 (the exact snapshot PVC naming + whether a
                // BGSAVE is needed before the tar).  Skip with a log note for
                // now so the plan remains actionable once T12 lands.
                eprintln!(
                    "info: redis extraction for claim {}/{} deferred to T12 — skipping",
                    item.namespace, item.claim_name
                );
            }
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
    kubeconfig: &'a Path,
}

impl Drop for HelperPodGuard<'_> {
    fn drop(&mut self) {
        delete_pod_best_effort(&self.name, self.namespace, self.kubeconfig);
    }
}

// ---------------------------------------------------------------------------
// Per-kind extraction helpers (private)
// ---------------------------------------------------------------------------

/// Extract a single `Pg` claim.
///
/// 1. Read connection creds from the connection Secret.
/// 2. Apply a pg-dump helper pod with `PGPASSWORD` in its container env.
/// 3. `kubectl exec` → `pg_dump -Fc` → stream to `out_dir/pg/<ns>/<claim>.dump`.
/// 4. Delete the pod (best-effort, via `HelperPodGuard` drop).
fn extract_pg(item: &ExtractItem, out_dir: &Path, kubeconfig: &Path, pg_image: &str) -> Result<()> {
    let secret_name = &item.source; // status.connectionSecretRef
    let ns = &item.namespace;
    let claim = &item.claim_name;

    // 1. Read the decomposed connection Secret keys.
    let user = read_secret_key(secret_name, ns, "user", kubeconfig)?;
    let pass = read_secret_key(secret_name, ns, "pass", kubeconfig)?;
    let host = read_secret_key(secret_name, ns, "host", kubeconfig)?;
    let port = read_secret_key(secret_name, ns, "port", kubeconfig)?;
    let db = read_secret_key(secret_name, ns, "db", kubeconfig)?;

    // 2. Build the pod name and arm the cleanup guard BEFORE apply-wait,
    //    so cleanup runs even if apply-wait times out or errors.
    let pod_name = truncate_pod_name(&format!("bk-pg-{claim}"));
    let _guard = HelperPodGuard {
        name: pod_name.clone(),
        namespace: ns,
        kubeconfig,
    };
    let spec = pg_dump_pod_spec_with_password(&pod_name, ns, pg_image, &pass);
    apply_and_wait_pod_ready(&spec, kubeconfig)?;

    // 3. Stream pg_dump output to disk.
    let dest = out_dir.join("pg").join(ns);
    fs::create_dir_all(&dest)
        .map_err(|e| CliError::Other(format!("create dir {}: {e}", dest.display())))?;
    let dump_path = dest.join(format!("{claim}.dump"));

    let argv: Vec<&str> = vec!["pg_dump", "-Fc", "-h", &host, "-U", &user, "-p", &port, &db];
    exec_stream_to_file(&pod_name, ns, &argv, &dump_path, kubeconfig)
    // _guard drops here (or on any earlier return) → delete_pod_best_effort called.
}

/// Extract a single `Volume` (disk / shared-disk) claim.
///
/// 1. Apply a busybox pod that mounts `item.source` (the PVC) at `/data`
///    read-only.
/// 2. `kubectl exec` → `tar c -C /data .` → stream to
///    `out_dir/volumes/<ns>/<claim>/data.tar`.
/// 3. Delete the pod (best-effort, via `HelperPodGuard` drop).
fn extract_volume(item: &ExtractItem, out_dir: &Path, kubeconfig: &Path) -> Result<()> {
    let pvc_name = &item.source; // status.volumeClaimRef
    let ns = &item.namespace;
    let claim = &item.claim_name;

    // Arm the cleanup guard BEFORE apply-wait so cleanup runs even if the
    // pod never reaches Ready.
    let pod_name = truncate_pod_name(&format!("bk-vol-{claim}"));
    let _guard = HelperPodGuard {
        name: pod_name.clone(),
        namespace: ns,
        kubeconfig,
    };
    let spec = volume_pod_spec(
        &pod_name,
        ns,
        images::VOLUME_IMAGE,
        pvc_name,
        true, // read-only for backup
    );
    apply_and_wait_pod_ready(&spec, kubeconfig)?;

    let dest = out_dir.join("volumes").join(ns).join(claim);
    fs::create_dir_all(&dest)
        .map_err(|e| CliError::Other(format!("create dir {}: {e}", dest.display())))?;
    let tar_path = dest.join("data.tar");

    let argv: Vec<&str> = vec!["tar", "c", "-C", "/data", "."];
    exec_stream_to_file(&pod_name, ns, &argv, &tar_path, kubeconfig)
    // _guard drops here (or on any earlier return) → delete_pod_best_effort called.
}

/// Truncate a pod name to 63 chars (DNS-1123 label limit) and strip any
/// trailing `-` left by the truncation.
fn truncate_pod_name(name: &str) -> String {
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
            "metadata":{"name":"r","namespace":"demo"},"status":{"ready":true}})];
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
    fn persistent_redis_source_is_claim_name() {
        // The snapshot PVC path is resolved at run-time (T12); the plan
        // uses the claim name as the source key.
        let claims = vec![json!({
            "spec": { "type": "redis", "persistent": true },
            "metadata": { "name": "my-cache", "namespace": "prod" },
            "status": { "ready": true }
        })];
        let plan = plan_extraction(&claims);
        assert_eq!(plan.len(), 1);
        assert_eq!(plan[0].source, "my-cache");
        assert_eq!(plan[0].claim_name, "my-cache");
        assert_eq!(plan[0].namespace, "prod");
    }

    #[test]
    fn truncate_pod_name_stays_under_64_chars() {
        let long = "bk-pg-".to_string() + &"a".repeat(100);
        let t = truncate_pod_name(&long);
        assert!(t.len() <= 63, "pod name too long: {}", t.len());
        assert!(!t.ends_with('-'), "trailing dash: {t}");
    }
}
