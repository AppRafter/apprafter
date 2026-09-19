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
    apply_and_wait_pod_ready, delete_pod_best_effort, exec_stream_to_file, nats_pod_spec,
    volume_pod_spec,
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
/// **Empty since 2.6d-6**, and that is a claim about the product rather than a
/// retired mechanism: `jetstream` was the only entry, and the helper pod this
/// module now drives (`nats stream backup`, through the `mgr_<ns>` identity
/// ADR 0061 §4.2 reserved for exactly this) captures it. `clickhouse`, `s3`
/// and `notifications` still fall through [`plan_extraction`]'s catch-all and
/// are still deliberately absent here: none of them ships, so a claim of those
/// types provisions nothing and there is no data to miss — announcing them
/// would be a warning about a situation that cannot occur.
///
/// The machinery stays wired end to end (manifest marker, `backup create`,
/// `backup show`, `export`) because it is the guard for the NEXT type that
/// ships without a capture path, and a guard nothing can reach is a guard that
/// is already broken. [`claims_without_data_capture_in`] is what keeps it
/// exercised while this table is empty.
pub const CLAIM_TYPES_WITHOUT_DATA_CAPTURE: &[&str] = &[];

/// Whether a claim of this `spec.type` has NO data in the backup. Pure.
///
/// The one place the rule lives: the manifest marker, the `backup create`
/// summary and `backup show` all read it, so they cannot come to different
/// conclusions about the same claim.
pub fn claim_type_has_no_data_capture(claim_type: &str) -> bool {
    CLAIM_TYPES_WITHOUT_DATA_CAPTURE.contains(&claim_type)
}

/// When each type's capture landed, by manifest format version.
///
/// A snapshot written before that version cannot hold the type's data no
/// matter what this build can capture today, and nothing inside such a
/// snapshot says so — the `no_data` marker did not exist for it to carry, and
/// an empty `jetstream/` directory is indistinguishable from a claim that had
/// no streams.
///
/// This is the half of A6 that a capture path does not close: had the table
/// above simply been emptied, every snapshot taken before 2.6d-6 would have
/// gone quiet about its jetstream claims the day the code shipped — the exact
/// "reads as complete while it is not" the finding named, pointed at the
/// archive instead of at the run.
pub const CAPTURE_LANDED_IN_MANIFEST_VERSION: &[(&str, u32)] = &[("jetstream", 2)];

/// Whether a claim of this type has no data in a snapshot of this manifest
/// version — the shipped table, OR a capture that postdates the snapshot.
pub fn claim_type_has_no_data_in_manifest(claim_type: &str, manifest_version: u32) -> bool {
    claim_type_has_no_data_capture(claim_type)
        || CAPTURE_LANDED_IN_MANIFEST_VERSION
            .iter()
            .any(|(ty, since)| *ty == claim_type && manifest_version < *since)
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
    claims_without_data_capture_in(claims, CLAIM_TYPES_WITHOUT_DATA_CAPTURE)
}

/// [`claims_without_data_capture`] against an explicit table.
///
/// Exists so the reporting path stays testable while the shipped table is
/// empty: the behaviour under test is "a claim of an announced type is named",
/// which cannot be exercised through a table with nothing in it.
fn claims_without_data_capture_in(claims: &[Value], types: &[&str]) -> Vec<UncapturedClaim> {
    claims
        .iter()
        .filter_map(|c| {
            let ty = c.pointer("/spec/type").and_then(Value::as_str)?;
            if !types.contains(&ty) {
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
    /// `JetStream` → the STREAM name; one item per stream the claim owns.
    pub source: String,
    /// `JetStream` only: the claim's `status.connectionSecretRef`.
    ///
    /// A field rather than a delimiter inside `source`, because a `:`-joined
    /// pair is a parser nobody asked for — and one that would look safe right
    /// up until a name carried the delimiter. The dump reads `host`/`port` off
    /// this Secret to reach the server; the credentials it authenticates with
    /// are the namespace's manager user, which lives elsewhere.
    pub connection: Option<String>,
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
                        connection: None,
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
                            connection: None,
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
                        connection: None,
                    });
                }
            }
            "jetstream" => {
                // One item per stream, not one per claim: a claim owns N
                // streams, each is dumped and replayed on its own, and a
                // single artifact for the claim would make one stream's
                // failure the whole claim's — and hide which one it was.
                let Some(conn) = c
                    .pointer("/status/connectionSecretRef")
                    .and_then(Value::as_str)
                else {
                    // Never finished provisioning: no account, no server
                    // coordinates, and nothing to dump.
                    continue;
                };
                for stream in owned_streams(c) {
                    items.push(ExtractItem {
                        namespace: ns.clone(),
                        claim_name: name.clone(),
                        kind: DataKind::JetStream,
                        source: stream,
                        connection: Some(conn.to_string()),
                    });
                }
            }
            _ => {
                // clickhouse / s3 / notifications — no extraction path, and
                // none of them ships. NOT silent for anything that does:
                // `claims_without_data_capture` reports it, the manifest
                // marks it `no_data`, and both `backup create` and
                // `backup show` say so.
            }
        }
    }
    items
}

/// The streams a jetstream claim OWNS, in `status.streams` (ADR 0061 §9).
///
/// `declared` and `dynamic` — the ones the provisioner attributed to this
/// application — and never `unattributed`, which that inventory defines as
/// "streams touching `<app>.` that no declaration in the namespace accounts
/// for … reported, never claimed". Dumping one would copy a neighbour's data
/// into this claim's artifact and restore it under this claim's name.
fn owned_streams(claim: &Value) -> Vec<String> {
    ["declared", "dynamic"]
        .iter()
        .filter_map(|key| claim.pointer(&format!("/status/streams/{key}")))
        .filter_map(Value::as_array)
        .flatten()
        .filter_map(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

/// The namespace NATS runs in, read off a claim connection Secret's `host`.
///
/// The provisioner writes `host` as `<statefulset>.<namespace>.svc`
/// (`reconcile.rs`), and that is the only place a backup can learn where the
/// lazily-enabled NATS component was installed: the claim carries its account
/// and its own user, never the component's namespace. `None` for a host that
/// is not a service FQDN — a caller cannot guess, and guessing wrong would
/// send the manager-credential read at some unrelated namespace.
pub fn nats_namespace_of_host(host: &str) -> Option<String> {
    let mut parts = host.split('.');
    let _statefulset = parts.next()?;
    let namespace = parts.next().filter(|ns| !ns.is_empty())?;
    Some(namespace.to_string())
}

/// The per-namespace NATS manager Secret, by the provisioner's convention.
///
/// Restated rather than imported: `nats::mgr_secret_name` lives in the
/// `operator/` workspace, which this crate does not depend on — the same
/// cross-workspace restatement `df-<instance>-0` already makes for Dragonfly.
/// `the_manager_secret_name_matches_the_provisioners_convention` is what keeps
/// the restatement checkable.
pub fn mgr_secret_name(namespace: &str) -> String {
    format!("nats-mgr-{namespace}")
}

/// The one-shot `sh -c` script that dumps one stream and tars it to stdout.
///
/// `nats` writes a progress line and a summary to stdout; both would land in
/// front of the tar and corrupt the artifact, so its output is discarded
/// wholesale and only `tar` writes to the stream. `set -e` is what makes a
/// failed dump a failed exec instead of an empty-but-successful tar — the
/// shape the redis loader's `[ "$OUT" = OK ]` check exists for.
fn jetstream_dump_script(stream: &str) -> String {
    let stream_q = crate::helper_pod::shell_single_quote(stream);
    format!(
        "set -e; \
         rm -rf /tmp/bk; mkdir -p /tmp/bk; \
         nats stream backup {stream_q} /tmp/bk --no-progress >/dev/null 2>&1; \
         tar c -C /tmp/bk ."
    )
}

/// A pod-name segment from an arbitrary NATS name.
///
/// Stream names are not pod names: NATS allows `_`, which DNS-1123 does not,
/// so `orders_dlq` would produce a pod the apiserver rejects. Anything outside
/// `[a-z0-9]` folds to `-`; the ARTIFACT keeps the real stream name, because
/// that is what the restore reads back.
pub fn pod_name_segment(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect()
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
            DataKind::JetStream => extract_jetstream(k, item, out_dir)?,
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

/// Extract one JetStream stream (2.6d-6).
///
/// `item.source` is the STREAM; `item.connection` is the claim's connection
/// Secret, which is where the server coordinates come from. The credentials
/// are NOT that Secret's: a claim user is denied `$JS.API.STREAM.SNAPSHOT` by
/// construction (ADR 0061 §4.2 — its delivery subject is caller-chosen, which
/// made it a read bypass), and the snapshot is the manager user's job. So this
/// reads `nats-mgr-<ns>` from the namespace NATS runs in, which the `host`
/// names and nothing else does.
///
/// The artifact is a tar of what `nats stream backup` writes — `backup.json`
/// plus `stream.tar.s2` — because `nats stream restore` takes that DIRECTORY,
/// not a single file. Consumers ride along: the CLI includes them unless told
/// otherwise, and a stream restored without its consumers would replay every
/// message to a subscriber that had already processed it.
fn extract_jetstream(k: &dyn KubeExec, item: &ExtractItem, out_dir: &Path) -> Result<()> {
    let ns = &item.namespace;
    let claim = &item.claim_name;
    let stream = &item.source;
    let conn = item.connection.as_deref().ok_or_else(|| {
        CliError::Other(format!(
            "jetstream item {ns}/{claim} stream {stream} has no connection Secret — \
             the planner must set it"
        ))
    })?;

    // 1. Server coordinates from the claim's own connection Secret.
    let host = k.get_secret_key(conn, ns, "host")?;
    let port = k.get_secret_key(conn, ns, "port")?;
    let nats_ns = nats_namespace_of_host(&host).ok_or_else(|| {
        CliError::Other(format!(
            "cannot tell which namespace NATS runs in from host {host:?} \
             (claim {ns}/{claim}): expected <service>.<namespace>.svc"
        ))
    })?;

    // 2. Manager credentials from that namespace.
    let mgr = mgr_secret_name(ns);
    let user = k.get_secret_key(&mgr, &nats_ns, "user")?;
    let password = k.get_secret_key(&mgr, &nats_ns, "password")?;
    let url = format!("nats://{host}:{port}");

    // 3. Helper pod beside the server, guard armed before apply-wait.
    let pod_name = truncate_pod_name(&format!(
        "bk-js-{}-{}",
        pod_name_segment(claim),
        pod_name_segment(stream)
    ));
    let _guard = HelperPodGuard {
        name: pod_name.clone(),
        namespace: &nats_ns,
        k,
    };
    let spec = nats_pod_spec(
        &pod_name,
        &nats_ns,
        images::JETSTREAM_IMAGE,
        &url,
        &user,
        &password,
    );
    apply_and_wait_pod_ready(k, &spec)?;

    // 4. Dump + tar to `jetstream/<ns>/<claim>/<stream>.tar`. The file NAME is
    //    the stream, verbatim, because that is what the restore reads back.
    let dest = out_dir.join("jetstream").join(ns).join(claim);
    fs::create_dir_all(&dest)
        .map_err(|e| CliError::Other(format!("create dir {}: {e}", dest.display())))?;
    let tar_path = dest.join(format!("{stream}.tar"));

    let script = jetstream_dump_script(stream);
    let argv: Vec<&str> = vec!["sh", "-c", &script];
    exec_stream_to_file(k, &pod_name, &nats_ns, &argv, &tar_path)
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
    // 2.6d-6 — JetStream capture
    // =======================================================================

    #[test]
    fn a_jetstream_claim_yields_one_item_per_stream_it_owns() {
        let claims = vec![json!({
            "spec": {"type": "jetstream"},
            "metadata": {"name": "atm-worker-jetstream", "namespace": "atm"},
            "status": {
                "ready": true,
                "connectionSecretRef": "atm-worker-jetstream-conn",
                "streams": {
                    "declared": ["orders"],
                    "dynamic": ["orders_dlq"],
                    "unattributed": ["someone-elses"],
                    "observedAt": "2026-09-19T00:00:00Z"
                }
            }
        })];
        let plan = plan_extraction(&claims);
        let streams: Vec<&str> = plan.iter().map(|i| i.source.as_str()).collect();
        // `unattributed` is reported, never claimed (ADR 0061 §9): dumping it
        // would copy a neighbour's data into this claim's artifact.
        assert_eq!(streams, vec!["orders", "orders_dlq"]);
        assert!(plan.iter().all(|i| i.kind == DataKind::JetStream));
        assert!(plan.iter().all(|i| i.claim_name == "atm-worker-jetstream"));
        assert!(plan.iter().all(|i| i.namespace == "atm"));
    }

    #[test]
    fn a_jetstream_item_carries_the_connection_secret_the_dump_authenticates_through() {
        // `source` is the stream, so the server coordinates need a field of
        // their own: the dump reads `host`/`port` off this Secret and then the
        // manager credentials from the NATS namespace it names.
        let claims = vec![json!({
            "spec": {"type": "jetstream"},
            "metadata": {"name": "c", "namespace": "atm"},
            "status": {"connectionSecretRef": "c-conn", "streams": {"declared": ["s"]}}
        })];
        let plan = plan_extraction(&claims);
        assert_eq!(plan.len(), 1);
        assert_eq!(plan[0].connection.as_deref(), Some("c-conn"));
    }

    #[test]
    fn a_jetstream_claim_with_no_observed_streams_yields_nothing() {
        // Consume-only, or never provisioned: either way there is no stream of
        // this claim's own to dump, and an item would send a helper pod after
        // a stream that does not exist.
        for status in [
            json!({"connectionSecretRef": "c-conn"}),
            json!({"connectionSecretRef": "c-conn", "streams": {"unattributed": ["x"]}}),
            json!({"streams": {"declared": ["s"]}}),
        ] {
            let claims = vec![json!({
                "spec": {"type": "jetstream"},
                "metadata": {"name": "c", "namespace": "atm"},
                "status": status
            })];
            assert!(
                plan_extraction(&claims).is_empty(),
                "must yield nothing: {claims:?}"
            );
        }
    }

    #[test]
    fn the_nats_namespace_comes_off_the_connection_host() {
        // The provisioner writes `host` as `<statefulset>.<ns>.svc`; the
        // manager Secret lives in that namespace, and nothing else a backup
        // can read says where the NATS component was installed.
        assert_eq!(
            nats_namespace_of_host("nats.nats-system.svc"),
            Some("nats-system".to_string())
        );
        assert_eq!(
            nats_namespace_of_host("nats.messaging.svc.cluster.local"),
            Some("messaging".to_string())
        );
        assert_eq!(nats_namespace_of_host("nats"), None);
        assert_eq!(nats_namespace_of_host(""), None);
    }

    #[test]
    fn the_manager_secret_name_matches_the_provisioners_convention() {
        // `nats::mgr_secret_name` lives in the operator workspace, which this
        // crate cannot depend on. The convention is restated here, and this
        // test is what makes the restatement checkable rather than folklore.
        assert_eq!(mgr_secret_name("atm"), "nats-mgr-atm");
        assert_eq!(mgr_secret_name("a-b"), "nats-mgr-a-b");
    }

    #[test]
    fn the_dump_script_leaves_only_the_tar_on_stdout() {
        let script = jetstream_dump_script("orders");
        // `nats` prints a progress line and a summary; anything it writes to
        // stdout lands in front of the tar and corrupts the artifact.
        assert!(script.contains(">/dev/null 2>&1"), "{script}");
        assert!(script.contains("tar c -C"), "{script}");
        assert!(script.starts_with("set -e"), "{script}");
        assert!(
            script.contains("'orders'"),
            "stream must be quoted: {script}"
        );
    }

    // =======================================================================
    // A6 — the claims a backup lists but holds no data for
    // =======================================================================

    /// FIRES: a claim of an announced type produces no extraction item AND is
    /// reported by name, which together are the finding — producing no item is
    /// the old behaviour; being reported is what stops the snapshot reading as
    /// complete.
    ///
    /// Driven through the table-taking half rather than the shipped table,
    /// because 2.6d-6 emptied the shipped one: `jetstream` was the only entry
    /// and its data is captured now. The reporting path must stay exercised
    /// for the next type that ships without a capture path — a guard that
    /// nothing can reach is a guard that is already broken.
    #[test]
    fn a_claim_of_an_announced_type_is_extracted_by_nothing_and_reported_by_name() {
        let claims = vec![json!({
            "spec": {"type": "clickhouse"},
            "metadata": {"name": "events", "namespace": "demo"},
            "status": {"ready": true}
        })];
        assert!(plan_extraction(&claims).is_empty());
        assert_eq!(
            claims_without_data_capture_in(&claims, &["clickhouse"]),
            vec![UncapturedClaim {
                namespace: "demo".into(),
                name: "events".into(),
                claim_type: "clickhouse".into(),
            }]
        );
    }

    /// The shipped table is empty, and that is a claim about the product: every
    /// `needs` type that ships has a capture path. Asserted on its own so the
    /// two tests below, which iterate it, are visibly vacuous today rather than
    /// silently so.
    #[test]
    fn no_shipped_need_type_is_announced_as_dataless_today() {
        assert!(
            CLAIM_TYPES_WITHOUT_DATA_CAPTURE.is_empty(),
            "a type is announced as dataless: {CLAIM_TYPES_WITHOUT_DATA_CAPTURE:?} — if it \
             ships, capture it; if it does not, it does not belong here"
        );
    }

    #[test]
    fn jetstream_is_no_longer_announced_because_its_data_is_captured() {
        assert!(!claim_type_has_no_data_capture("jetstream"));
    }

    /// The archive half of A6: capture closes the gap for snapshots taken
    /// AFTER it, and says nothing about the ones already in the repository.
    /// Emptying the table alone would have silenced those — the finding's own
    /// failure mode, pointed at the archive.
    #[test]
    fn a_snapshot_older_than_the_capture_still_reports_the_type_as_dataless() {
        assert!(claim_type_has_no_data_in_manifest("jetstream", 1));
        assert!(!claim_type_has_no_data_in_manifest("jetstream", 2));
        // A type that was never in the table and never gained a capture path
        // is not retroactively announced by the version rule.
        assert!(!claim_type_has_no_data_in_manifest("pg", 1));
    }

    #[test]
    fn every_capture_era_entry_names_a_version_this_build_can_write() {
        // An entry whose version exceeds what `capture_non_claim_artifacts`
        // stamps would mark EVERY snapshot this build writes as dataless for
        // that type — a warning on data that is right there.
        assert!(!CAPTURE_LANDED_IN_MANIFEST_VERSION.is_empty());
        for (ty, since) in CAPTURE_LANDED_IN_MANIFEST_VERSION {
            assert!(
                *since <= crate::manifest::MANIFEST_VERSION_CURRENT,
                "{ty} claims to be captured from manifest v{since}, but this build writes v{}",
                crate::manifest::MANIFEST_VERSION_CURRENT
            );
        }
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

    /// The narrowing, stated as a test: `clickhouse`, `s3` and `notifications`
    /// fall through the planner's catch-all and are deliberately NOT announced
    /// — none of them ships, so such a claim provisions nothing and there is no
    /// data to miss. A warning about them would be a warning about an
    /// impossible situation.
    #[test]
    fn an_unshipped_type_is_neither_extracted_nor_announced() {
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
    /// is in the snapshot — the same dishonesty pointing the other way. This
    /// is what forced 2.6d-6's two halves into one commit.
    ///
    /// Vacuous while the table is empty, which
    /// `no_shipped_need_type_is_announced_as_dataless_today` states out loud.
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
