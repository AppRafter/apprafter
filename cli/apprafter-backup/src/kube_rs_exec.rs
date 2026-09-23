// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! In-cluster KubeExec via kube-rs. Integration-verified by the chunk-6
//! real-Hetzner walk; unit-verified below against a stub apiserver (see the
//! `tests` module for exactly which parts a cluster is still required for —
//! in short, the two WebSocket `exec` streams and nothing else).
//!
//! [`KubeRsExec`] implements [`backup_core::KubeExec`] using kube-rs so the
//! in-cluster scheduled-backup runner drives the SAME portable backup engine
//! (`backup_core::engine`) the CLI does — but through the apiserver directly,
//! not by shelling out to `kubectl`. Every method mirrors the semantics of the
//! CLI's `KubectlExec` (`platform_cli::commands::backup`):
//!
//! * `apply_and_wait_pod_ready` — server-side-apply the helper Pod, then poll
//!   until it is `Running` with a `True` `Ready` condition (mirrors
//!   `kubectl apply` + `kubectl wait --for=condition=Ready`).
//! * `exec_stream_to_file` / `exec_stream_from_file` — pod command execution via
//!   the WebSocket attach subresource, streaming the process stdout to a file
//!   (`pg_dump`, `tar c`) / a file into the process stdin (restore load path).
//! * `delete_pod_best_effort` — best-effort helper-pod teardown.
//! * `get_secret_key` — read one decoded Secret key (connection creds).
//! * `get_json` — the "run kubectl get and return parsed JSON" method the
//!   engine's list/get sweep relies on, resolved through API discovery so the
//!   kubectl-style resource strings (`applications.apprafter.io`, `secrets`,
//!   `platformstack`, …) map to the right GVK without hardcoding third-party
//!   CRD versions.
//!
//! Every trait method is synchronous (the engine is sync) and drives its async
//! kube-rs body via `self.rt.block_on(...)` on the caller-supplied Tokio
//! runtime handle.

use std::path::Path;

// `is_backup_helper`: whether a Pod spec is a backup helper pod — the label
// every `backup_core::helper_pod` builder stamps. Only those are the run's to
// delete when it is stopped.
use backup_core::helper_pod::is_backup_helper;
use backup_core::KubeExec;
use cli_core::{CliError, Result};
use k8s_openapi::api::core::v1::{Pod, Secret};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::Status;
use kube::api::{
    Api, AttachParams, AttachedProcess, DeleteParams, DynamicObject, ListParams, ObjectList, Patch,
    PatchParams, TypeMeta,
};
use kube::discovery::{self, ApiResource};
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Field manager used for the server-side apply of helper Pods. Distinct from
/// the operator's `apprafter-operator` and the CLI's other managers so its
/// ownership never collides with a real workload's.
const FIELD_MANAGER: &str = "apprafter-backup";

/// Interval between pod-Ready polls. The budget for the whole wait is
/// [`backup_core::helper_pod::POD_READY_TIMEOUT`], the CLI's too.
const POD_READY_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(2);

/// In-cluster [`KubeExec`] backed by kube-rs.
///
/// Owns a [`kube::Client`] and a Tokio runtime
/// [`Handle`](tokio::runtime::Handle); each synchronous trait method blocks on
/// that handle to run its async body. The client is expected to be built from
/// the pod's in-cluster service-account (`kube::Client::try_default()` resolves
/// the mounted token + CA), so no kubeconfig file is involved.
pub struct KubeRsExec {
    client: kube::Client,
    rt: tokio::runtime::Handle,
    /// Every helper pod applied and not yet deleted, for the stop to delete
    /// when Kubernetes ends the run first (`crate::stop`).
    live_helpers: crate::stop::LiveHelperPods,
}

impl KubeRsExec {
    /// Construct a runner over `client`, blocking on `rt` for every call.
    pub fn new(client: kube::Client, rt: tokio::runtime::Handle) -> Self {
        Self {
            client,
            rt,
            live_helpers: crate::stop::LiveHelperPods::default(),
        }
    }

    /// The helper pods this exec has applied and not yet deleted — a handle
    /// that stays current as the run goes on.
    pub fn live_helper_pods(&self) -> crate::stop::LiveHelperPods {
        self.live_helpers.clone()
    }

    /// Resolve a kubectl-style resource string (`applications.apprafter.io`,
    /// `secrets`, `platformstack`, …) to its dynamic [`ApiResource`] via API
    /// discovery.
    ///
    /// * `<plural>.<group>` (a dot present) → discover the named API group and
    ///   match the resource by plural name (e.g. `applications.apprafter.io`,
    ///   `clusters.postgresql.cnpg.io`).
    /// * a bare token with NO dot → either a core resource (`secrets`) or an
    ///   apprafter short name (`platformstack`). We map the known bare names the
    ///   engine passes to a `(group, plural)` pair, then discover that group.
    ///
    /// Discovery (rather than a static GVK table) keeps third-party CRD versions
    /// — CNPG's `postgresql.cnpg.io/v1`, sealed-secrets' `bitnami.com/v1alpha1`
    /// — out of this code: the apiserver reports the served version.
    async fn resolve_resource(&self, resource: &str) -> Result<ApiResource> {
        let (group, plural) = split_resource(resource);
        let apigroup = discovery::group(&self.client, &group).await.map_err(|e| {
            CliError::Other(format!(
                "discover API group {group:?} for resource {resource:?}: {e}"
            ))
        })?;
        // Match the served resource by plural name at the group's preferred
        // version. `versioned_resources` returns `(ApiResource, caps)`.
        let ver = apigroup.preferred_version_or_latest();
        for (ar, _caps) in apigroup.versioned_resources(ver) {
            if resource_matches(&ar, &plural) {
                return Ok(ar);
            }
        }
        Err(CliError::Other(format!(
            "resource {resource:?} (plural {plural:?}) not found in API group {group:?} at \
             version {ver:?}"
        )))
    }
}

/// True iff the discovered `ar` is what kubectl would resolve `plural` to.
///
/// Match the PLURAL (`secrets`, `applications`) OR the singular kind
/// (`secret`, `application`) — kubectl accepts both, so the shared engine may
/// pass either; be as lenient as `KubectlExec` here.
///
/// Extracted from [`KubeRsExec::resolve_resource`] (its only caller) so the
/// matching RULE is unit-testable: the surrounding discovery round-trip is pure
/// apiserver I/O, this predicate is the whole decision.
fn resource_matches(ar: &ApiResource, plural: &str) -> bool {
    ar.plural == plural || ar.kind.eq_ignore_ascii_case(plural)
}

/// Split a kubectl resource string into `(group, plural)`.
///
/// * `applications.apprafter.io` → `("apprafter.io", "applications")`.
/// * `secrets` → `("", "secrets")` (core group).
/// * `platformstack` → `("apprafter.io", "platformstacks")` — the one bare
///   apprafter short name the engine passes (`get_platformstack`). The engine's
///   only other bare token is `secrets`, so a tiny alias table suffices; any
///   other bare name falls through to the core group and, if absent there,
///   surfaces a clear discovery error rather than silently misbehaving.
fn split_resource(resource: &str) -> (String, String) {
    // Known bare (dotless) apprafter short name the engine uses.
    if matches!(resource, "platformstack" | "platformstacks") {
        return ("apprafter.io".to_string(), "platformstacks".to_string());
    }
    match resource.split_once('.') {
        Some((plural, group)) => (group.to_string(), plural.to_string()),
        None => (String::new(), resource.to_string()),
    }
}

/// `(name, namespace)` of a helper-Pod spec.
///
/// Extracted from [`KubeRsExec::apply_and_wait_pod_ready`] (its only caller) so
/// the two "spec missing …" rejections are unit-testable without an apiserver.
/// Both fields are REQUIRED: `Api::namespaced` would otherwise silently target
/// the client's default namespace, applying a backup helper pod into the wrong
/// place.
fn pod_identity(spec: &Value) -> Result<(String, String)> {
    let name = spec["metadata"]["name"]
        .as_str()
        .ok_or_else(|| CliError::Other("pod spec missing metadata.name".into()))?
        .to_string();
    let ns = spec["metadata"]["namespace"]
        .as_str()
        .ok_or_else(|| CliError::Other("pod spec missing metadata.namespace".into()))?
        .to_string();
    Ok((name, ns))
}

/// Parsed shape of a `get_json` args vector.
#[derive(Debug)]
enum GetShape {
    /// `get <resource> -A` (cluster-wide list).
    ListAll { resource: String },
    /// `get <resource> -n <ns>` (namespaced list, no name).
    ListNs { resource: String, ns: String },
    /// `get <resource> -n <ns> <name>` (single object get).
    GetNamed {
        resource: String,
        ns: String,
        name: String,
    },
    /// `get <resource> <name>` with NO `-n` — a CLUSTER-SCOPED single get.
    /// The engine's one use is `get namespaces kube-system`, the cluster's
    /// machine key (`engine::read_cluster_uid`).
    GetClusterScoped { resource: String, name: String },
}

/// Parse the kubectl-style `args` vector into a [`GetShape`].
///
/// Handles exactly the shapes the engine / extract layer pass (confirmed by
/// grepping every `get_json` call site):
///
/// * `["get", "<resource>", "-A", "-o", "json"]`
/// * `["get", "<resource>", "-n", "<ns>", "-o", "json"]`
/// * `["get", "<resource>", "<name>", "-n", "<ns>", "-o", "json"]`
/// * `["get", "<resource>", "<name>", "-o", "json"]` — cluster-scoped get
///   (`namespaces kube-system`, the cluster's machine key)
///
/// The `-o json` tail is ignored (kube-rs returns typed objects we serialize
/// ourselves). Any other shape is an error — we never silently misbehave.
fn parse_get_args(args: &[&str]) -> Result<GetShape> {
    let mut it = args.iter().copied();
    match it.next() {
        Some("get") => {}
        other => {
            return Err(CliError::Other(format!(
                "unsupported get_json args (expected leading `get`): {args:?} (got {other:?})"
            )));
        }
    }
    let resource = it
        .next()
        .ok_or_else(|| {
            CliError::Other(format!("unsupported get_json args (no resource): {args:?}"))
        })?
        .to_string();

    let mut ns: Option<String> = None;
    let mut all = false;
    let mut name: Option<String> = None;

    while let Some(tok) = it.next() {
        match tok {
            "-A" | "--all-namespaces" => all = true,
            "-n" | "--namespace" => {
                let v = it.next().ok_or_else(|| {
                    CliError::Other(format!(
                        "unsupported get_json args (`-n` without value): {args:?}"
                    ))
                })?;
                ns = Some(v.to_string());
            }
            "-o" => {
                // Skip the output format value (`json`); the engine always asks
                // for json and we return json regardless.
                let _ = it.next();
            }
            // A bare positional after the resource is the object name.
            other if !other.starts_with('-') => {
                if name.is_some() {
                    return Err(CliError::Other(format!(
                        "unsupported get_json args (two positionals): {args:?}"
                    )));
                }
                name = Some(other.to_string());
            }
            other => {
                return Err(CliError::Other(format!(
                    "unsupported get_json args (unrecognized flag {other:?}): {args:?}"
                )));
            }
        }
    }

    match (all, ns, name) {
        (true, _, None) => Ok(GetShape::ListAll { resource }),
        (false, Some(ns), None) => Ok(GetShape::ListNs { resource, ns }),
        (false, Some(ns), Some(name)) => Ok(GetShape::GetNamed { resource, ns, name }),
        // No `-n`, no `-A`, one positional: a cluster-scoped object. kubectl
        // reads it the same way, and a namespaced kind asked for like this
        // fails at the apiserver rather than silently resolving somewhere.
        (false, None, Some(name)) => Ok(GetShape::GetClusterScoped { resource, name }),
        (_, _, _) => Err(CliError::Other(format!(
            "unsupported get_json args (need `-A` for a cluster list, `-n <ns>` for a namespaced \
             list, `-n <ns> <name>` for a single get, or `<name>` for a cluster-scoped get): \
             {args:?}"
        ))),
    }
}

/// Classify a kube-rs error as "not found" (→ `Ok(None)` for `get_json`) vs a
/// real error. Mirrors `KubectlExec::get_json`, which returns `Ok(None)` when
/// kubectl's stderr carries `NotFound` / `not found`.
fn is_not_found(err: &kube::Error) -> bool {
    matches!(err, kube::Error::Api(ae) if ae.code == 404)
}

/// Stamp `apiVersion` + `kind` onto an object the apiserver returned WITHOUT
/// them, taking the identity from the [`ApiResource`] the `Api` was built with.
///
/// A List response does not repeat the type on its items — they are implied by
/// the List's own kind — so `DynamicObject::types` decodes as `None` and a
/// plain `serde_json::to_value` emits neither field. Everything inside this
/// process reads such an object fine (the engine goes at `/spec`, `/metadata`;
/// the restore recovers the kind from the staged FILENAME), so it stayed
/// invisible until a snapshot's objects were piped to `kubectl apply
/// --server-side`, which needs the real fields and answers
/// `[apiVersion not set, kind not set]`.
///
/// The identity comes from the `ApiResource` rather than from the requested
/// resource string: it is the exact GVK the request was issued at — discovered,
/// so a third-party CRD's served version is right by construction.
///
/// Only ever FILLS IN. An item that carried its own type (a mixed `List`)
/// keeps it, or every object in such a list would be applied as the wrong kind.
fn stamp_types(mut o: DynamicObject, ar: &ApiResource) -> DynamicObject {
    if o.types.is_none() {
        o.types = Some(TypeMeta {
            api_version: ar.api_version.clone(),
            kind: ar.kind.clone(),
        });
    }
    o
}

/// Serialize ONE `DynamicObject` the way the engine stages it: type-stamped
/// (see [`stamp_types`]), so what lands in a snapshot is a document
/// `kubectl apply` accepts.
fn object_to_value(o: DynamicObject, ar: &ApiResource) -> Result<Value> {
    serde_json::to_value(stamp_types(o, ar)).map_err(CliError::from)
}

/// Serialize a kube-rs `ObjectList<DynamicObject>` into the `{"items":[...]}`
/// shape kubectl emits, so `engine::list_items` can read `.items[]` unchanged.
///
/// `ar` is the resource the list was issued against; every item is stamped with
/// its type (see [`stamp_types`]).
fn list_to_value(list: ObjectList<DynamicObject>, ar: &ApiResource) -> Result<Value> {
    let items = list
        .items
        .into_iter()
        .map(|o| object_to_value(o, ar))
        .collect::<Result<Vec<Value>>>()?;
    Ok(serde_json::json!({ "items": items }))
}

impl KubeRsExec {
    /// Server-side apply `spec` (mirrors `kubectl apply -f -`), replacing a
    /// pod of the same name left from an earlier run (see
    /// [`backup_core::helper_pod::stale_helper_reason`]): one whose spec
    /// cannot be applied over, and one the apply shows has ended, is going
    /// away, or has used more of its keep-alive than a reused helper may.
    /// Each is deleted, waited out, and the apply made again, once.
    ///
    /// A backup helper pod's every PATCH is recorded in the live set first
    /// ([`Self::begin_helper_apply`]), which refuses it once the run is being
    /// stopped: the re-apply after a replacement included.
    async fn apply_replacing_stale(
        &self,
        api: &Api<Pod>,
        name: &str,
        ns: &str,
        spec: &Value,
    ) -> Result<()> {
        let pp = PatchParams::apply(FIELD_MANAGER).force();
        let apply_error =
            |e: kube::Error| CliError::Other(format!("apply pod {name} in {ns}: {e}"));
        let in_flight = self.begin_helper_apply(spec, ns, name)?;
        let applied = api.patch(name, &pp, &Patch::Apply(spec)).await;
        drop(in_flight);
        let stale = match applied {
            Ok(pod) => {
                let pod = serde_json::to_value(&pod).map_err(CliError::from)?;
                match backup_core::helper_pod::stale_helper_reason(&pod, spec, chrono::Utc::now()) {
                    Some(why) => why,
                    None => return Ok(()),
                }
            }
            Err(kube::Error::Api(ae))
                if ae.code == 422
                    && backup_core::helper_pod::is_immutable_pod_update(&ae.message) =>
            {
                "was created with a spec this run's cannot be applied over (an earlier run of \
                 another version, or with another keep-alive)"
                    .to_string()
            }
            Err(e) => return Err(apply_error(e)),
        };
        eprintln!(
            "{}",
            backup_core::helper_pod::replacing_stale_helper_note(ns, name, &stale)
        );
        self.delete_and_wait_gone(api, name, ns).await?;
        let _in_flight = self.begin_helper_apply(spec, ns, name)?;
        api.patch(name, &pp, &Patch::Apply(spec))
            .await
            .map_err(apply_error)?;
        Ok(())
    }

    /// Record `spec` in the live set as an apply under way, when it is a
    /// backup helper pod (only those are the stop's to delete); refused once
    /// the stop has begun ([`crate::stop::LiveHelperPods`]).
    fn begin_helper_apply(
        &self,
        spec: &Value,
        ns: &str,
        name: &str,
    ) -> Result<Option<crate::stop::ApplyInFlight>> {
        if is_backup_helper(spec) {
            self.live_helpers.begin_apply(ns, name).map(Some)
        } else {
            Ok(None)
        }
    }

    /// Delete a stale helper pod and wait until it is gone, within
    /// [`backup_core::helper_pod::STALE_POD_GONE_WITHIN`].
    async fn delete_and_wait_gone(&self, api: &Api<Pod>, name: &str, ns: &str) -> Result<()> {
        let dp = DeleteParams {
            grace_period_seconds: Some(backup_core::helper_pod::STALE_POD_DELETE_GRACE_SECONDS),
            ..DeleteParams::default()
        };
        match api.delete(name, &dp).await {
            Ok(_) => {}
            Err(e) if is_not_found(&e) => {}
            Err(e) => {
                return Err(CliError::Other(format!(
                    "delete the stale helper pod {name} in {ns}: {e}"
                )))
            }
        }
        let bound = backup_core::helper_pod::STALE_POD_GONE_WITHIN;
        let deadline = tokio::time::Instant::now() + bound;
        loop {
            let present = api.get_opt(name).await.map_err(|e| {
                CliError::Other(format!(
                    "get pod {name} in {ns} while waiting for it to be deleted: {e}"
                ))
            })?;
            if present.is_none() {
                return Ok(());
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(CliError::Other(format!(
                    "the stale helper pod {name} in {ns} was still there {}s after it was \
                     deleted (its node may be unreachable); delete it with `kubectl delete pod \
                     {name} -n {ns} --force --grace-period=0` and run again",
                    bound.as_secs()
                )));
            }
            tokio::time::sleep(STALE_POD_GONE_POLL_INTERVAL).await;
        }
    }
}

impl KubeRsExec {
    /// Poll pod `name` until it is Running + Ready, for up to `timeout`, every
    /// `poll` — the CLI's `KubectlExec` waits the same way. A container the
    /// kubelet cannot configure for `grace` without a break (a credential
    /// Secret or key missing: [`backup_core::helper_pod::container_config_error`])
    /// ends the wait at once with the kubelet's words, rather than after the
    /// whole `timeout` with none.
    async fn wait_pod_ready(
        &self,
        api: &Api<Pod>,
        name: &str,
        ns: &str,
        timeout: std::time::Duration,
        poll: std::time::Duration,
        grace: std::time::Duration,
    ) -> Result<()> {
        let deadline = tokio::time::Instant::now() + timeout;
        let mut config_error = backup_core::helper_pod::ConfigErrorWatch::default();
        loop {
            let pod = api.get(name).await.map_err(|e| {
                CliError::Other(format!("get pod {name} in {ns} while waiting Ready: {e}"))
            })?;
            if pod_is_ready(&pod) {
                return Ok(());
            }
            let seen = serde_json::to_value(&pod).map_err(CliError::from)?;
            // Tokio's clock, so the grace can be driven in a test like the
            // rest of this loop.
            let now = tokio::time::Instant::now().into_std();
            if let Some(why) = config_error.observe(&seen, now, grace) {
                return Err(backup_core::helper_pod::container_config_error_message(
                    ns, name, &why,
                ));
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(CliError::Other(format!(
                    "pod {name} in {ns} did not reach Ready within {}s",
                    timeout.as_secs()
                )));
            }
            tokio::time::sleep(poll).await;
        }
    }
}

/// Interval between the polls that wait for a stale helper pod to be gone.
const STALE_POD_GONE_POLL_INTERVAL: tokio::time::Duration = tokio::time::Duration::from_secs(1);

impl KubeExec for KubeRsExec {
    fn apply_and_wait_pod_ready(&self, spec: &Value) -> Result<()> {
        self.rt.block_on(async {
            let (name, ns) = pod_identity(spec)?;
            let api: Api<Pod> = Api::namespaced(self.client.clone(), &ns);

            // Server-side apply (mirrors `kubectl apply -f -`), over a stale
            // leftover of the same name if there is one. A helper pod is
            // tracked from BEFORE its apply: one whose apply went through and
            // whose Ready wait is still running is the run's to delete too.
            self.apply_replacing_stale(&api, &name, &ns, spec).await?;

            self.wait_pod_ready(
                &api,
                &name,
                &ns,
                backup_core::helper_pod::POD_READY_TIMEOUT,
                POD_READY_POLL_INTERVAL,
                backup_core::helper_pod::CONTAINER_CONFIG_ERROR_GRACE,
            )
            .await
        })
    }

    fn exec_stream_to_file(
        &self,
        pod: &str,
        ns: &str,
        argv: &[&str],
        out: &Path,
        first_output_within: Option<std::time::Duration>,
    ) -> Result<()> {
        self.rt.block_on(async {
            let api: Api<Pod> = Api::namespaced(self.client.clone(), ns);
            let ap = AttachParams::default()
                .stdin(false)
                .stdout(true)
                .stderr(true);

            let mut attached = api
                .exec(pod, argv.iter().copied(), &ap)
                .await
                .map_err(|e| {
                    CliError::Other(format!(
                        "exec_stream_to_file: start command {argv:?} in {ns}/{pod}: {e}"
                    ))
                })?;

            // Before anything else can block: see `StderrDrain`.
            let stderr = StderrDrain::start(&mut attached);

            let mut proc_stdout = attached.stdout().ok_or_else(|| {
                CliError::Other("exec_stream_to_file: attached process exposed no stdout".into())
            })?;

            let mut file = tokio::fs::File::create(out).await.map_err(|e| {
                CliError::Other(format!(
                    "exec_stream_to_file: create output file {}: {e}",
                    out.display()
                ))
            })?;

            let copy_error = |e: std::io::Error| {
                CliError::Other(format!(
                    "exec_stream_to_file: copy command stdout to {}: {e}",
                    out.display()
                ))
            };

            // The first read is timed when the caller bounded it: see
            // `KubeExec::exec_stream_to_file`. Abandoning the exec ends this
            // side only — the command keeps running in its pod until the pod
            // goes, which is the caller's helper-pod guard, straight after.
            if let Some(bound) = first_output_within {
                let mut first = vec![0u8; 64 * 1024];
                match tokio::time::timeout(bound, proc_stdout.read(&mut first)).await {
                    Err(_elapsed) => {
                        attached.abort();
                        return Err(backup_core::kube::no_output_error(
                            "exec_stream_to_file",
                            argv,
                            ns,
                            pod,
                            bound,
                        ));
                    }
                    Ok(read) => {
                        let n = read.map_err(copy_error)?;
                        file.write_all(&first[..n]).await.map_err(copy_error)?;
                    }
                }
            }

            // Stream the (rest of the) process stdout to the file.
            tokio::io::copy(&mut proc_stdout, &mut file)
                .await
                .map_err(copy_error)?;
            file.flush().await.map_err(|e| {
                CliError::Other(format!(
                    "exec_stream_to_file: flush output file {}: {e}",
                    out.display()
                ))
            })?;

            let status =
                check_exec_status(&mut attached, "exec_stream_to_file", argv, ns, pod).await;
            let captured = stderr.finish().await;
            status.map_err(|e| with_stderr(e, captured.as_ref()))
        })
    }

    fn exec_stream_from_file(
        &self,
        pod: &str,
        ns: &str,
        argv: &[&str],
        input: &Path,
    ) -> Result<()> {
        self.rt.block_on(async {
            let api: Api<Pod> = Api::namespaced(self.client.clone(), ns);
            let ap = AttachParams::default()
                .stdin(true)
                .stdout(false)
                .stderr(true);

            let mut attached = api
                .exec(pod, argv.iter().copied(), &ap)
                .await
                .map_err(|e| {
                    CliError::Other(format!(
                        "exec_stream_from_file: start command {argv:?} in {ns}/{pod}: {e}"
                    ))
                })?;

            // Before anything else can block: see `StderrDrain`.
            let stderr = StderrDrain::start(&mut attached);

            let mut proc_stdin = attached.stdin().ok_or_else(|| {
                CliError::Other("exec_stream_from_file: attached process exposed no stdin".into())
            })?;

            let mut file = tokio::fs::File::open(input).await.map_err(|e| {
                CliError::Other(format!(
                    "exec_stream_from_file: open input file {}: {e}",
                    input.display()
                ))
            })?;

            // Feed the input file's bytes into the process stdin, then close it
            // (EOF) so the remote command sees end-of-input.
            //
            // Neither result is propagated here, for the reason spelled out on
            // `KubectlExec::apply_and_wait_pod_ready` in platform-cli's
            // `backup.rs`. `check_exec_status` below reads the remote
            // command's exit status, and `StderrDrain` holds its stderr;
            // returning early skips both and reports a transport error instead
            // of the command's own explanation. The transport is a websocket rather than an OS
            // pipe, so there is no literal SIGPIPE — the apiserver closes the
            // stdin channel when the remote command exits, and the write comes
            // back `BrokenPipe`/`ConnectionReset`. The structure is identical.
            //
            // And unlike a pipe, size makes it certain rather than rare: the
            // callers here stream a `pg_dump` file into `psql` and a tar
            // archive into `tar` (`restore.rs`), megabytes at least. A `psql`
            // that exits on a bad password used to surface as "copy … to
            // command stdin: Broken pipe" with psql's actual complaint thrown
            // away, on the restore path, where a wrong diagnosis costs most.
            let copy_result = tokio::io::copy(&mut file, &mut proc_stdin).await;
            let shutdown_result = proc_stdin.shutdown().await;
            drop(proc_stdin);

            let status =
                check_exec_status(&mut attached, "exec_stream_from_file", argv, ns, pod).await;
            let captured = stderr.finish().await;
            status.map_err(|e| with_stderr(e, captured.as_ref()))?;

            // The command claimed success. That is its claim about what it did
            // with its input, not evidence the input arrived — a dump that was
            // never fully delivered must never read as a completed restore.
            copy_result.map_err(|e| {
                CliError::Other(format!(
                    "exec_stream_from_file: copy {} to command stdin: {e}",
                    input.display()
                ))
            })?;
            shutdown_result.map_err(|e| {
                CliError::Other(format!("exec_stream_from_file: close command stdin: {e}"))
            })?;
            Ok(())
        })
    }

    fn delete_pod_best_effort(&self, name: &str, ns: &str) {
        // Best-effort: swallow every error (mirrors `kubectl delete pod
        // --ignore-not-found`). Never panics.
        let _ = self.rt.block_on(async {
            let api: Api<Pod> = Api::namespaced(self.client.clone(), ns);
            api.delete(name, &DeleteParams::default()).await
        });
        // Forgotten whatever the delete answered: it is the only delete this
        // run makes, and a stop that retried it would find the same answer.
        self.live_helpers.remove(ns, name);
    }

    fn get_secret_key(&self, secret: &str, ns: &str, key: &str) -> Result<String> {
        self.rt.block_on(async {
            let api: Api<Secret> = Api::namespaced(self.client.clone(), ns);
            let obj = api
                .get(secret)
                .await
                .map_err(|e| CliError::Other(format!("get secret {secret} in {ns}: {e}")))?;
            // `Secret.data` is `BTreeMap<String, ByteString>` — already
            // base64-DECODED bytes (k8s-openapi handles the transport decode),
            // so no further base64 step here (unlike KubectlExec, which decodes
            // the raw jsonpath value itself).
            let data = obj
                .data
                .ok_or_else(|| CliError::Other(format!("secret {ns}/{secret} has no data")))?;
            let bytes = data
                .get(key)
                .ok_or_else(|| CliError::Other(format!("secret {ns}/{secret} has no key {key}")))?;
            String::from_utf8(bytes.0.clone()).map_err(|e| {
                CliError::Other(format!("secret {ns}/{secret} key {key} is not utf-8: {e}"))
            })
        })
    }

    fn get_json(&self, args: &[&str]) -> Result<Option<Value>> {
        let shape = parse_get_args(args)?;
        self.rt.block_on(async {
            match shape {
                GetShape::ListAll { resource } => {
                    let ar = self.resolve_resource(&resource).await?;
                    let api: Api<DynamicObject> = Api::all_with(self.client.clone(), &ar);
                    match api.list(&ListParams::default()).await {
                        Ok(list) => Ok(Some(list_to_value(list, &ar)?)),
                        Err(e) if is_not_found(&e) => Ok(None),
                        Err(e) => Err(CliError::Other(format!("list {resource} -A: {e}"))),
                    }
                }
                GetShape::ListNs { resource, ns } => {
                    let ar = self.resolve_resource(&resource).await?;
                    let api: Api<DynamicObject> =
                        Api::namespaced_with(self.client.clone(), &ns, &ar);
                    match api.list(&ListParams::default()).await {
                        Ok(list) => Ok(Some(list_to_value(list, &ar)?)),
                        Err(e) if is_not_found(&e) => Ok(None),
                        Err(e) => Err(CliError::Other(format!("list {resource} -n {ns}: {e}"))),
                    }
                }
                GetShape::GetNamed { resource, ns, name } => {
                    let ar = self.resolve_resource(&resource).await?;
                    let api: Api<DynamicObject> =
                        Api::namespaced_with(self.client.clone(), &ns, &ar);
                    match api.get(&name).await {
                        Ok(obj) => Ok(Some(object_to_value(obj, &ar)?)),
                        Err(e) if is_not_found(&e) => Ok(None),
                        Err(e) => Err(CliError::Other(format!(
                            "get {resource} {name} -n {ns}: {e}"
                        ))),
                    }
                }
                GetShape::GetClusterScoped { resource, name } => {
                    let ar = self.resolve_resource(&resource).await?;
                    let api: Api<DynamicObject> = Api::all_with(self.client.clone(), &ar);
                    match api.get(&name).await {
                        Ok(obj) => Ok(Some(object_to_value(obj, &ar)?)),
                        Err(e) if is_not_found(&e) => Ok(None),
                        Err(e) => Err(CliError::Other(format!("get {resource} {name}: {e}"))),
                    }
                }
            }
        })
    }
}

/// True iff the Pod is `Running` AND carries a `Ready` condition with
/// `status == "True"`: [`backup_core::helper_pod::pod_is_ready`], the rule the
/// CLI's wait applies too, on the typed Pod.
fn pod_is_ready(pod: &Pod) -> bool {
    serde_json::to_value(pod).is_ok_and(|pod| backup_core::helper_pod::pod_is_ready(&pod))
}

/// Await the attached process's terminal status and translate a non-`Success`
/// status into an `Err` (mirrors `KubectlExec`'s "command exited non-zero"
/// path).
///
/// The k8s remotecommand protocol reports a `metav1.Status` on stream close:
/// `status: Some("Success")` on exit-0, else `status: Some("Failure")` with a
/// `reason` (`NonZeroExitCode`) and a human-readable `message`.
///
/// **Fail-closed**: after streaming, the terminal `metav1.Status` MUST arrive
/// and MUST be `Success`. A missing status channel (`take_status()` → `None`)
/// or an empty status future (`status_fut.await` → `None`) means we cannot
/// verify the command's exit code — a truncated or failed `pg_dump`/`tar` must
/// never be recorded as a successful backup, so we return `Err` in both cases.
async fn check_exec_status(
    attached: &mut AttachedProcess,
    context: &str,
    argv: &[&str],
    ns: &str,
    pod: &str,
) -> Result<()> {
    // Awaiting the status channel is the only part that needs a live
    // WebSocket; the VERDICT it feeds is in `classify_exec_status`.
    let terminal = match attached.take_status() {
        Some(status_fut) => Some(status_fut.await),
        None => None,
    };
    classify_exec_status(terminal, context, argv, ns, pod)
}

/// Turn an awaited remotecommand terminal status into the engine's verdict.
///
/// Extracted from [`check_exec_status`] (its only caller) so the fail-closed
/// rule is unit-testable — an `AttachedProcess` cannot be constructed without a
/// live WebSocket to an apiserver, but the decision it drives is pure.
///
/// `terminal` layers the two ways the status can be absent:
/// * `None` — `take_status()` gave no channel at all;
/// * `Some(None)` — the channel closed without ever yielding a status;
/// * `Some(Some(s))` — a `metav1.Status` arrived.
///
/// INVARIANT: ONLY `Some(Some(status))` with `status == "Success"` is `Ok`.
/// Both absent-status cases are errors, because a truncated `pg_dump`/`tar`
/// whose exit code we could not read must never be recorded as a successful
/// backup.
fn classify_exec_status(
    terminal: Option<Option<Status>>,
    context: &str,
    argv: &[&str],
    ns: &str,
    pod: &str,
) -> Result<()> {
    match terminal {
        Some(Some(status)) if status.status.as_deref() == Some("Success") => Ok(()),
        Some(Some(status)) => Err(CliError::Other(format!(
            "{context}: exec {argv:?} in {ns}/{pod} failed (status={:?}, reason={}): {}",
            status.status,
            status.reason.clone().unwrap_or_default(),
            status.message.clone().unwrap_or_default(),
        ))),
        Some(None) => Err(CliError::Other(format!(
            "{context}: exec {argv:?} in {ns}/{pod} returned no terminal status — cannot \
             verify exit code (failing closed to avoid a truncated backup)",
        ))),
        None => Err(CliError::Other(format!(
            "{context}: exec {argv:?} in {ns}/{pod} exposed no status channel — cannot verify \
             exit code (failing closed)",
        ))),
    }
}

// ---------------------------------------------------------------------------
// The remote command's stderr
// ---------------------------------------------------------------------------

/// Bytes of a remote command's stderr kept from its start (where `pg_dump`
/// and `psql` put the error) and from its end (where `tar` does).
const STDERR_HEAD_BYTES: usize = 2048;
const STDERR_TAIL_BYTES: usize = 2048;

/// Reads an exec'd command's stderr to EOF on its own task, for as long as the
/// command runs.
///
/// Two reasons, and the first is a hang. kube-rs hands stderr over through an
/// in-memory pipe of 1 KiB (`AttachParams::max_stderr_buf_size`'s default),
/// written by the SAME task that reads the WebSocket. Asking for stderr and
/// never reading it — what this module did up to 0.2.77 — means the first
/// command to write more than 1 KiB there stalls that task: no more stdout, no
/// terminal status, no pings, and no read on the socket for the client's read
/// timeout to count. The run waits for ever. `pg_dump` 18 hitting its
/// `--lock-wait-timeout` writes one `LOCK TABLE` statement naming every table
/// in the database — 2 KiB for 62 tables.
///
/// The second: that text is the only explanation of a failure. The apiserver's
/// terminal status says `exit code 1` and nothing about why.
///
/// Dropping the drain aborts its task, so an early `?` return leaves nothing
/// behind.
struct StderrDrain(Option<tokio::task::JoinHandle<StderrCapture>>);

impl StderrDrain {
    /// Take `attached`'s stderr and start reading it. Must run inside the
    /// runtime (it spawns), which every `KubeRsExec` method body does.
    fn start(attached: &mut AttachedProcess) -> Self {
        Self(attached.stderr().map(|r| tokio::spawn(drain_stderr(r))))
    }

    /// What the command wrote to stderr. Call once the terminal status has
    /// arrived: kube-rs closes the pipe as its message loop ends, so the task
    /// is at, or moments from, EOF by then.
    async fn finish(mut self) -> Option<StderrCapture> {
        self.0.take()?.await.ok()
    }
}

impl Drop for StderrDrain {
    fn drop(&mut self) {
        if let Some(task) = &self.0 {
            task.abort();
        }
    }
}

/// A command's stderr, read to EOF but kept bounded: the first
/// [`STDERR_HEAD_BYTES`], the last [`STDERR_TAIL_BYTES`], and how many bytes
/// fell between. Bounded because it ends up in the status ConfigMap and the
/// failure webhook.
#[derive(Debug, Default)]
struct StderrCapture {
    head: Vec<u8>,
    tail: std::collections::VecDeque<u8>,
    total: u64,
}

impl StderrCapture {
    fn push(&mut self, bytes: &[u8]) {
        self.total += bytes.len() as u64;
        let into_head = STDERR_HEAD_BYTES
            .saturating_sub(self.head.len())
            .min(bytes.len());
        self.head.extend_from_slice(&bytes[..into_head]);
        self.tail.extend(&bytes[into_head..]);
        let excess = self.tail.len().saturating_sub(STDERR_TAIL_BYTES);
        self.tail.drain(..excess);
    }

    /// The captured text, `None` when the command wrote nothing (or only
    /// whitespace).
    fn render(&self) -> Option<String> {
        let kept = (self.head.len() + self.tail.len()) as u64;
        let mut text = String::from_utf8_lossy(&self.head).into_owned();
        if self.total > kept {
            text.push_str(&format!(
                "\n[… {} bytes of stderr not kept …]\n",
                self.total - kept
            ));
        }
        let tail: Vec<u8> = self.tail.iter().copied().collect();
        text.push_str(&String::from_utf8_lossy(&tail));
        let text = text.trim_end();
        (!text.trim().is_empty()).then(|| text.to_string())
    }
}

/// Read `reader` to EOF (or its first error) into a [`StderrCapture`].
async fn drain_stderr<R: tokio::io::AsyncRead + Unpin>(mut reader: R) -> StderrCapture {
    let mut capture = StderrCapture::default();
    let mut buf = [0u8; 8192];
    loop {
        match reader.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => capture.push(&buf[..n]),
        }
    }
    capture
}

/// Append the command's own stderr to an exec failure; the error unchanged
/// when there is none.
fn with_stderr(err: CliError, stderr: Option<&StderrCapture>) -> CliError {
    match stderr.and_then(StderrCapture::render) {
        Some(text) => CliError::Other(format!("{err}\ncommand stderr:\n{text}")),
        None => err,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// # What a cluster is still required for
///
/// `exec_stream_to_file` / `exec_stream_from_file` open the apiserver's
/// `pods/exec` **WebSocket** subresource. `Api::exec` returns an
/// [`AttachedProcess`] whose stdin/stdout handles and status channel only exist
/// once that upgrade has completed against a real, *running* pod; the type has
/// no public constructor and no in-memory transport. `exec_stream_to_file` is
/// therefore covered by the real-Hetzner walk, the kind-based `e2e/backup-*`
/// scripts and `tests/kind_smoke_test.rs`, NOT here; `exec_stream_from_file`
/// has no real-cluster coverage at all (see that test's module docs for why).
/// What *is* covered here is the verdict those two methods end on —
/// [`classify_exec_status`], the fail-closed rule extracted out of
/// [`check_exec_status`] for exactly that reason.
///
/// Everything else in this file is exercised below. The HTTP methods run
/// against a hand-written stub apiserver handed to `kube::Client::new` (the
/// kube-rs project's own test idiom), so the production request builder, the
/// real response decoder and the real `kube::Error` mapping all run — only the
/// socket is replaced.
///
/// The runtime handling is under test throughout, implicitly but strictly:
/// every stub-backed test calls the *synchronous* trait method from a plain
/// test thread that is NOT inside a runtime, exactly as `main` does. A method
/// that reached for `Handle::current()` instead of the caller-supplied `self.rt`
/// would panic there, and one that tried to `block_on` from inside the runtime
/// would panic too.
#[cfg(test)]
mod tests {
    use super::*;

    use std::convert::Infallible;
    use std::future::{ready, Ready};
    use std::sync::{Arc, Mutex};
    use std::task::{Context, Poll};

    use http::{Request, Response};
    use kube::client::Body;
    use serde_json::json;
    use tower_service::Service;

    // -----------------------------------------------------------------------
    // Pure helpers
    // -----------------------------------------------------------------------

    #[test]
    fn split_resource_separates_a_dotted_plural_from_its_group() {
        assert_eq!(
            split_resource("applications.apprafter.io"),
            ("apprafter.io".to_string(), "applications".to_string())
        );
        assert_eq!(
            split_resource("clusters.postgresql.cnpg.io"),
            ("postgresql.cnpg.io".to_string(), "clusters".to_string())
        );
        assert_eq!(
            split_resource("sealedsecrets.bitnami.com"),
            ("bitnami.com".to_string(), "sealedsecrets".to_string())
        );
    }

    #[test]
    fn split_resource_puts_a_bare_name_in_the_core_group() {
        assert_eq!(
            split_resource("secrets"),
            (String::new(), "secrets".to_string())
        );
    }

    /// INVARIANT: `platformstack` is the one DOTLESS non-core name the engine
    /// passes (`engine::get_platformstack`). Without the alias it would be
    /// looked up in the CORE group, where it does not exist, and every backup
    /// would lose its `PlatformStack` CR + `platform_version`.
    #[test]
    fn split_resource_aliases_the_bare_platformstack_short_name_to_its_group() {
        assert_eq!(
            split_resource("platformstack"),
            ("apprafter.io".to_string(), "platformstacks".to_string())
        );
        assert_eq!(
            split_resource("platformstacks"),
            ("apprafter.io".to_string(), "platformstacks".to_string())
        );
    }

    #[test]
    fn resource_matches_accepts_the_plural_or_the_kind_case_insensitively() {
        let ar = ApiResource {
            group: "apprafter.io".into(),
            version: "v1alpha1".into(),
            api_version: "apprafter.io/v1alpha1".into(),
            kind: "Application".into(),
            plural: "applications".into(),
        };
        assert!(resource_matches(&ar, "applications"));
        assert!(resource_matches(&ar, "application"));
        assert!(resource_matches(&ar, "APPLICATION"));
        assert!(!resource_matches(&ar, "applicationsets"));
        assert!(!resource_matches(&ar, "resourceclaims"));
    }

    fn shape_of(args: &[&str]) -> GetShape {
        parse_get_args(args).expect("supported get_json shape")
    }

    #[test]
    fn parse_get_args_reads_the_three_shapes_the_engine_emits() {
        match shape_of(&["get", "applications.apprafter.io", "-A", "-o", "json"]) {
            GetShape::ListAll { resource } => assert_eq!(resource, "applications.apprafter.io"),
            other => panic!("expected a cluster-wide list, got {other:?}"),
        }
        match shape_of(&["get", "secrets", "-n", "demo", "-o", "json"]) {
            GetShape::ListNs { resource, ns } => {
                assert_eq!(resource, "secrets");
                assert_eq!(ns, "demo");
            }
            other => panic!("expected a namespaced list, got {other:?}"),
        }
        match shape_of(&[
            "get",
            "platformstack",
            "default",
            "-n",
            "apprafter-system",
            "-o",
            "json",
        ]) {
            GetShape::GetNamed { resource, ns, name } => {
                assert_eq!(resource, "platformstack");
                assert_eq!(ns, "apprafter-system");
                assert_eq!(name, "default");
            }
            other => panic!("expected a single get, got {other:?}"),
        }
        // …plus the cluster-scoped get the identity read needs (E1).
        match shape_of(&["get", "namespaces", "kube-system", "-o", "json"]) {
            GetShape::GetClusterScoped { resource, name } => {
                assert_eq!(resource, "namespaces");
                assert_eq!(name, "kube-system");
            }
            other => panic!("expected a cluster-scoped get, got {other:?}"),
        }
    }

    /// The name is a bare positional and may sit on EITHER side of `-n <ns>`
    /// (kubectl accepts both orders, and `engine::read_secret_data` writes it
    /// before the flag while a hand-built call may not).
    #[test]
    fn parse_get_args_finds_the_name_on_either_side_of_the_namespace_flag() {
        for args in [
            ["get", "secrets", "app-creds", "-n", "demo"].as_slice(),
            ["get", "secrets", "-n", "demo", "app-creds"].as_slice(),
        ] {
            match shape_of(args) {
                GetShape::GetNamed { ns, name, .. } => {
                    assert_eq!((ns.as_str(), name.as_str()), ("demo", "app-creds"));
                }
                other => panic!("expected a single get for {args:?}, got {other:?}"),
            }
        }
    }

    #[test]
    fn parse_get_args_accepts_the_long_flag_spellings() {
        match shape_of(&["get", "pods", "--all-namespaces"]) {
            GetShape::ListAll { resource } => assert_eq!(resource, "pods"),
            other => panic!("expected a cluster-wide list, got {other:?}"),
        }
        match shape_of(&["get", "pods", "--namespace", "kube-system"]) {
            GetShape::ListNs { ns, .. } => assert_eq!(ns, "kube-system"),
            other => panic!("expected a namespaced list, got {other:?}"),
        }
    }

    /// INVARIANT: an args vector this parser does not understand is an ERROR,
    /// never a silently-wrong query. Each of these would otherwise resolve to a
    /// *different* set of objects than the caller asked for — a backup that
    /// quietly captured the wrong namespace, or nothing at all.
    #[test]
    fn parse_get_args_rejects_every_shape_it_cannot_serve() {
        for args in [
            // no leading `get`
            ["describe", "pods", "-A"].as_slice(),
            // no resource
            ["get"].as_slice(),
            // `-n` with nothing after it
            ["get", "secrets", "-n"].as_slice(),
            // two positionals — which one is the name?
            ["get", "secrets", "a", "b", "-n", "demo"].as_slice(),
            // Flags the parser does not model. Silently DROPPING them would
            // hand back a wider result set than the caller asked for: the first
            // as a plain cluster-wide list, the second with the selector's
            // value swallowed as an object name.
            ["get", "pods", "-A", "--show-labels"].as_slice(),
            [
                "get",
                "pods",
                "-n",
                "demo",
                "--field-selector",
                "status.phase=Running",
            ]
            .as_slice(),
            // neither `-A` nor `-n` and NO name: scope is undefined. (With a
            // name it is a cluster-scoped get, which IS a shape — see above.)
            ["get", "pods", "-o", "json"].as_slice(),
            // a name with `-A`: kubectl has no such shape
            ["get", "pods", "mypod", "-A"].as_slice(),
        ] {
            assert!(
                parse_get_args(args).is_err(),
                "must refuse to guess at {args:?}"
            );
        }
    }

    /// `-o <fmt>` is consumed WITH its value; if the value leaked back into the
    /// token stream it would be mistaken for the object name and turn a list
    /// into a single get.
    #[test]
    fn parse_get_args_swallows_the_output_format_value() {
        match shape_of(&["get", "pods", "-o", "json", "-n", "demo"]) {
            GetShape::ListNs { ns, .. } => assert_eq!(ns, "demo"),
            other => panic!("`-o json` must not become the object name; got {other:?}"),
        }
    }

    /// INVARIANT: only a 404 becomes `Ok(None)`. A 403 or a 500 must stay an
    /// error — reading "forbidden" as "absent" would let a backup silently skip
    /// resources the service account cannot see and still report success.
    ///
    /// The rule is the HTTP CODE, not the reason string: the reason here is
    /// deliberately not `NotFound`, and kube's own `Status::is_not_found()`
    /// would answer by reason alone (Go-client semantics). kube 3 folded
    /// `ErrorResponse` into `Status`; the classification did not move with it.
    #[test]
    fn is_not_found_is_true_for_404_and_nothing_else() {
        let api = |code| {
            kube::Error::Api(
                kube::core::Status::failure("boom", "Whatever")
                    .with_code(code)
                    .boxed(),
            )
        };
        assert!(is_not_found(&api(404)));
        assert!(!is_not_found(&api(403)));
        assert!(!is_not_found(&api(409)));
        assert!(!is_not_found(&api(410)));
        assert!(!is_not_found(&api(500)));
        assert!(!is_not_found(&kube::Error::TlsRequired));
    }

    /// The `ApiResource` an `Api` is built with — the type identity kube-rs
    /// already holds for every list it issues, and the one this file stamps
    /// back onto the items the apiserver returns without it.
    fn claim_resource() -> ApiResource {
        ApiResource {
            group: "apprafter.io".into(),
            version: "v1alpha1".into(),
            api_version: "apprafter.io/v1alpha1".into(),
            kind: "ResourceClaim".into(),
            plural: "resourceclaims".into(),
        }
    }

    /// INVARIANT: the value handed to `engine::list_items` must be the kubectl
    /// `{"items":[…]}` envelope AND each item must survive the round-trip with
    /// its arbitrary CR body intact — the engine reads `/spec/type`,
    /// `/status/connectionSecretRef` and friends straight off these objects.
    ///
    /// The fixture is the REAL apiserver List shape: `apiVersion`/`kind` appear
    /// once, on the List, and NOT on each item — they are implied by the List's
    /// own kind. A fixture that repeated them per item would pre-supply exactly
    /// what [`list_to_value`] is responsible for adding, and could never fail.
    #[test]
    fn list_to_value_wraps_items_and_preserves_each_object_verbatim() {
        let list: ObjectList<DynamicObject> = serde_json::from_value(json!({
            "apiVersion": "apprafter.io/v1alpha1",
            "kind": "ResourceClaimList",
            "items": [{
                "metadata": {"name": "pg-0", "namespace": "demo"},
                "spec": {"type": "pg"},
                "status": {"connectionSecretRef": "pg-0-conn"}
            }]
        }))
        .expect("decode a claim list");

        let v = list_to_value(list, &claim_resource()).expect("serialize list");
        let items = v["items"].as_array().expect("an items array");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0]["metadata"]["name"], json!("pg-0"));
        assert_eq!(items[0]["metadata"]["namespace"], json!("demo"));
        assert_eq!(items[0]["spec"]["type"], json!("pg"));
        assert_eq!(
            items[0]["status"]["connectionSecretRef"],
            json!("pg-0-conn")
        );
    }

    /// INVARIANT (the D-restore defect): every item leaves here carrying
    /// `apiVersion` + `kind`.
    ///
    /// A real apiserver List does NOT repeat the type on its items, so
    /// `DynamicObject::types` decodes as `None` and a plain
    /// `serde_json::to_value` emits neither field. Objects staged from such a
    /// list reached `kubectl apply --server-side` during a restore as
    /// `{"data":…,"metadata":…}` and were rejected with
    /// `[apiVersion not set, kind not set]` — every snapshot the in-cluster
    /// runner ever wrote was unrestorable. The identity is not re-derived from
    /// the requested resource string: it comes from the `ApiResource` the `Api`
    /// was constructed with, which is the same GVK the request was issued at.
    #[test]
    fn list_to_value_stamps_the_type_the_apiserver_left_off_its_items() {
        let list: ObjectList<DynamicObject> = serde_json::from_value(json!({
            "apiVersion": "v1",
            "kind": "SecretList",
            "items": [{
                "metadata": {"name": "cf-cert", "namespace": "apprafter-system"},
                "type": "kubernetes.io/tls",
                "data": {"tls.crt": "eA=="}
            }]
        }))
        .expect("decode a secret list");

        let secrets = ApiResource {
            group: String::new(),
            version: "v1".into(),
            api_version: "v1".into(),
            kind: "Secret".into(),
            plural: "secrets".into(),
        };
        let v = list_to_value(list, &secrets).expect("serialize list");
        assert_eq!(v["items"][0]["apiVersion"], json!("v1"));
        assert_eq!(v["items"][0]["kind"], json!("Secret"));
        // …and the body is untouched.
        assert_eq!(v["items"][0]["type"], json!("kubernetes.io/tls"));
        assert_eq!(v["items"][0]["data"]["tls.crt"], json!("eA=="));
    }

    /// INVARIANT: stamping only ever FILLS IN. An item that carried its own
    /// type keeps it verbatim — a `List` of mixed kinds (kubectl's `-o json`
    /// over several resources) would otherwise be rewritten to the collection's
    /// kind and every object applied as the wrong type.
    #[test]
    fn list_to_value_never_overwrites_a_type_the_item_already_carries() {
        let list: ObjectList<DynamicObject> = serde_json::from_value(json!({
            "apiVersion": "v1",
            "kind": "List",
            "items": [{
                "apiVersion": "cert-manager.io/v1",
                "kind": "Certificate",
                "metadata": {"name": "star", "namespace": "apprafter-system"}
            }]
        }))
        .expect("decode a mixed list");

        let v = list_to_value(list, &claim_resource()).expect("serialize list");
        assert_eq!(v["items"][0]["apiVersion"], json!("cert-manager.io/v1"));
        assert_eq!(v["items"][0]["kind"], json!("Certificate"));
    }

    #[test]
    fn list_to_value_of_an_empty_list_is_an_empty_items_array() {
        let list: ObjectList<DynamicObject> =
            serde_json::from_value(json!({"apiVersion": "v1", "kind": "List", "items": []}))
                .expect("decode an empty list");
        assert_eq!(
            list_to_value(list, &claim_resource()).unwrap(),
            json!({"items": []})
        );
    }

    // --- Kubernetes timestamps on the wire -----------------------------------
    //
    // k8s-openapi 0.23 held `Time`/`MicroTime` as `chrono::DateTime<Utc>`;
    // 0.28 (with kube 3+) holds a `jiff::Timestamp` and formats it with its own
    // strftime pattern. Every object the runner stages is re-serialized through
    // those types (`metadata.creationTimestamp`, `managedFields[].time`, …), so
    // the formatter swap reaches the snapshot bytes directly. The strings below
    // are what 0.23 produced — verified against it, not copied from 0.28 — so a
    // difference here is a change in what a backup writes.

    /// `Time` into and back out of its wire form.
    fn time_wire(s: &str) -> String {
        use k8s_openapi::apimachinery::pkg::apis::meta::v1::Time;
        let t: Time = serde_json::from_value(json!(s)).expect("parse a metav1.Time");
        serde_json::to_string(&t).expect("serialize a metav1.Time")
    }

    /// `MicroTime` into and back out of its wire form.
    fn micro_time_wire(s: &str) -> String {
        use k8s_openapi::apimachinery::pkg::apis::meta::v1::MicroTime;
        let t: MicroTime = serde_json::from_value(json!(s)).expect("parse a metav1.MicroTime");
        serde_json::to_string(&t).expect("serialize a metav1.MicroTime")
    }

    /// INVARIANT: `metav1.Time` is whole seconds, UTC, `Z`-suffixed. Sub-second
    /// input is TRUNCATED (never rounded up into the next second) and an offset
    /// is normalized to UTC.
    #[test]
    fn metav1_time_keeps_its_exact_wire_form() {
        assert_eq!(
            time_wire("2026-09-22T17:53:30Z"),
            r#""2026-09-22T17:53:30Z""#
        );
        assert_eq!(
            time_wire("2026-09-22T17:53:30.999999999Z"),
            r#""2026-09-22T17:53:30Z""#
        );
        assert_eq!(
            time_wire("2026-09-22T20:53:30+03:00"),
            r#""2026-09-22T17:53:30Z""#
        );
        assert_eq!(
            time_wire("1970-01-01T00:00:00Z"),
            r#""1970-01-01T00:00:00Z""#
        );
    }

    /// INVARIANT: `metav1.MicroTime` always carries EXACTLY six fractional
    /// digits — zeros included, which is the case a formatter that elides a
    /// zero fraction would break — truncated from nanoseconds, UTC, `Z`.
    #[test]
    fn metav1_micro_time_keeps_its_exact_wire_form() {
        assert_eq!(
            micro_time_wire("2026-09-22T17:53:30.123456Z"),
            r#""2026-09-22T17:53:30.123456Z""#
        );
        assert_eq!(
            micro_time_wire("2026-09-22T17:53:30Z"),
            r#""2026-09-22T17:53:30.000000Z""#
        );
        assert_eq!(
            micro_time_wire("2026-09-22T17:53:30.1Z"),
            r#""2026-09-22T17:53:30.100000Z""#
        );
        assert_eq!(
            micro_time_wire("2026-09-22T17:53:30.123456789Z"),
            r#""2026-09-22T17:53:30.123456Z""#
        );
        assert_eq!(
            micro_time_wire("2026-09-22T20:53:30.000001+03:00"),
            r#""2026-09-22T17:53:30.000001Z""#
        );
    }

    /// INVARIANT: a staged object's metadata timestamps reach the snapshot
    /// byte-for-byte. This is the path the runner actually takes — the object is
    /// decoded as a `DynamicObject` (whose metadata is the typed `ObjectMeta`)
    /// and re-serialized by [`list_to_value`] — asserted on the serialized
    /// bytes, not on a parsed value that would compare equal across formats.
    #[test]
    fn staged_object_metadata_timestamps_survive_byte_for_byte() {
        let list: ObjectList<DynamicObject> = serde_json::from_value(json!({
            "apiVersion": "v1",
            "kind": "SecretList",
            "items": [{
                "metadata": {
                    "name": "cf-cert",
                    "namespace": "apprafter-system",
                    "creationTimestamp": "2026-09-22T17:53:30Z",
                    "deletionTimestamp": "2026-09-22T18:00:00Z",
                    "managedFields": [{
                        "manager": "apprafter",
                        "operation": "Apply",
                        "apiVersion": "v1",
                        "time": "2026-09-22T17:53:31Z",
                        "fieldsType": "FieldsV1",
                        "fieldsV1": {"f:data": {"f:tls.crt": {}}}
                    }]
                },
                "type": "kubernetes.io/tls"
            }]
        }))
        .expect("decode a secret list with timestamps");

        let secrets = ApiResource {
            group: String::new(),
            version: "v1".into(),
            api_version: "v1".into(),
            kind: "Secret".into(),
            plural: "secrets".into(),
        };
        let v = list_to_value(list, &secrets).expect("serialize list");
        assert_eq!(
            serde_json::to_string(&v["items"][0]["metadata"]).unwrap(),
            concat!(
                r#"{"creationTimestamp":"2026-09-22T17:53:30Z","#,
                r#""deletionTimestamp":"2026-09-22T18:00:00Z","#,
                r#""managedFields":[{"apiVersion":"v1","fieldsType":"FieldsV1","#,
                r#""fieldsV1":{"f:data":{"f:tls.crt":{}}},"manager":"apprafter","#,
                r#""operation":"Apply","time":"2026-09-22T17:53:31Z"}],"#,
                r#""name":"cf-cert","namespace":"apprafter-system"}"#,
            )
        );
    }

    fn pod_from(status: Value) -> Pod {
        serde_json::from_value(json!({
            "apiVersion": "v1",
            "kind": "Pod",
            "metadata": {"name": "bk-pg-alpha", "namespace": "demo"},
            "status": status,
        }))
        .expect("decode pod")
    }

    /// INVARIANT: BOTH halves are required. A `Running` pod whose container is
    /// still starting has no listening socket yet, so exec'ing `pg_dump` at
    /// that moment fails; a `Ready` condition left over on a `Succeeded` pod is
    /// equally useless. This mirrors `kubectl wait --for=condition=Ready`.
    #[test]
    fn pod_is_ready_requires_running_phase_and_a_true_ready_condition() {
        let ready_cond = json!([{"type": "Ready", "status": "True"}]);
        assert!(pod_is_ready(&pod_from(
            json!({"phase": "Running", "conditions": ready_cond})
        )));

        // Running, but the Ready condition is missing / not yet True.
        assert!(!pod_is_ready(&pod_from(json!({"phase": "Running"}))));
        assert!(!pod_is_ready(&pod_from(
            json!({"phase": "Running", "conditions": []})
        )));
        assert!(!pod_is_ready(&pod_from(json!({
            "phase": "Running",
            "conditions": [{"type": "Ready", "status": "False"}]
        }))));
        assert!(!pod_is_ready(&pod_from(json!({
            "phase": "Running",
            "conditions": [{"type": "Initialized", "status": "True"}]
        }))));

        // Ready condition present, but the pod is not Running.
        for phase in ["Pending", "Succeeded", "Failed", "Unknown"] {
            assert!(
                !pod_is_ready(&pod_from(json!({"phase": phase, "conditions": ready_cond}))),
                "phase {phase} must not count as Ready"
            );
        }

        // No status block at all (freshly created).
        let bare: Pod = serde_json::from_value(json!({
            "apiVersion": "v1", "kind": "Pod",
            "metadata": {"name": "bk-pg-alpha", "namespace": "demo"}
        }))
        .unwrap();
        assert!(!pod_is_ready(&bare));
    }

    #[test]
    fn pod_identity_reads_the_name_and_namespace_out_of_the_spec() {
        let (name, ns) = pod_identity(&json!({
            "metadata": {"name": "bk-vol-data", "namespace": "demo"}
        }))
        .expect("a complete spec");
        assert_eq!(name, "bk-vol-data");
        assert_eq!(ns, "demo");
    }

    /// INVARIANT: neither field may be defaulted. `Api::namespaced` would
    /// happily fall back to the client's default namespace, applying a backup
    /// helper pod — with the database password in its env — into the wrong one.
    #[test]
    fn pod_identity_refuses_a_spec_missing_either_field() {
        assert!(pod_identity(&json!({"metadata": {"namespace": "demo"}})).is_err());
        assert!(pod_identity(&json!({"metadata": {"name": "bk-vol-data"}})).is_err());
        assert!(pod_identity(&json!({})).is_err());
        // Present but not a string is just as unusable as absent.
        assert!(pod_identity(&json!({"metadata": {"name": 7, "namespace": "demo"}})).is_err());
    }

    fn status(kind: &str) -> Status {
        Status {
            status: Some(kind.to_string()),
            reason: Some("NonZeroExitCode".to_string()),
            message: Some("command terminated with exit code 1".to_string()),
            ..Default::default()
        }
    }

    /// INVARIANT (fail-closed): a dump whose exit code we could not read is a
    /// FAILED dump. `pg_dump` writing a truncated file and the stream closing
    /// without a terminal status must never be recorded as a good backup — that
    /// is the difference between "restore is empty" and "backup failed loudly".
    #[test]
    fn classify_exec_status_accepts_only_an_explicit_success() {
        let argv = ["pg_dump", "-Fc"];
        let go = |t| classify_exec_status(t, "exec_stream_to_file", &argv, "demo", "bk-pg-alpha");

        assert!(go(Some(Some(status("Success")))).is_ok());

        // An explicit failure.
        assert!(go(Some(Some(status("Failure")))).is_err());
        // A status object with no `status` field at all.
        assert!(go(Some(Some(Status::default()))).is_err());
        // The channel existed but closed without yielding a status.
        assert!(go(Some(None)).is_err());
        // There was no status channel to begin with.
        assert!(go(None).is_err());
    }

    /// The failure message must carry the apiserver's own `reason` + `message`
    /// — they are the only place the remote command's exit code is reported,
    /// and a scheduled backup's log is all an operator gets.
    #[test]
    fn classify_exec_status_reports_the_reason_and_message_from_the_apiserver() {
        let err = classify_exec_status(
            Some(Some(status("Failure"))),
            "exec_stream_to_file",
            &["pg_dump"],
            "demo",
            "bk-pg-alpha",
        )
        .expect_err("a Failure status must be an error");
        let msg = format!("{err}");
        assert!(msg.contains("NonZeroExitCode"), "lost the reason: {msg}");
        assert!(
            msg.contains("command terminated with exit code 1"),
            "lost the message: {msg}"
        );
    }

    // -----------------------------------------------------------------------
    // The remote command's stderr
    // -----------------------------------------------------------------------

    /// kube-rs's stderr pipe, as `AttachedProcess` builds it: a
    /// `tokio::io::duplex` of `AttachParams::max_stderr_buf_size`'s default.
    const KUBE_STDERR_PIPE_BYTES: usize = 1024;

    /// The drain must keep reading however much the command writes — a
    /// reader that stopped once it had "enough" would stall kube-rs's message
    /// loop exactly as an unread pipe does — and it must keep the start and
    /// the end of what it read.
    #[test]
    fn drain_stderr_reads_far_past_the_kube_pipe_and_keeps_both_ends() {
        use tokio::io::AsyncWriteExt;

        let rt = tokio::runtime::Runtime::new().expect("runtime");
        let capture = rt.block_on(async {
            let (mut writer, reader) = tokio::io::duplex(KUBE_STDERR_PIPE_BYTES);
            // The writer is what kube-rs's message loop does with stderr
            // frames: `write_all`, which parks while the pipe is full.
            let written = tokio::spawn(async move {
                writer
                    .write_all(b"pg_dump: error: query failed: ERROR:  canceling statement\n")
                    .await?;
                for i in 0..2000 {
                    writer
                        .write_all(format!("public.app_orders_line_items_{i}, ").as_bytes())
                        .await?;
                }
                writer.write_all(b"\nTHE LAST LINE\n").await?;
                std::io::Result::Ok(())
            });
            let capture =
                tokio::time::timeout(std::time::Duration::from_secs(10), drain_stderr(reader))
                    .await
                    .expect("the drain stopped reading: the writer is parked on a full pipe");
            written.await.expect("join").expect("write");
            capture
        });

        assert!(capture.total > 60_000, "total: {}", capture.total);
        let text = capture.render().expect("stderr was written");
        assert!(
            text.starts_with("pg_dump: error: query failed"),
            "the head is where pg_dump puts the error: {text}"
        );
        assert!(text.ends_with("THE LAST LINE"), "the tail: {text}");
        assert!(text.contains("bytes of stderr not kept"), "{text}");
        assert!(
            text.len() < STDERR_HEAD_BYTES + STDERR_TAIL_BYTES + 100,
            "bounded: {} bytes",
            text.len()
        );
    }

    #[test]
    fn a_short_stderr_is_kept_whole() {
        let mut capture = StderrCapture::default();
        capture.push(b"tar: ./x: Cannot open: Permission denied\n");
        capture.push(b"tar: Exiting with failure status\n");
        assert_eq!(
            capture.render().as_deref(),
            Some(
                "tar: ./x: Cannot open: Permission denied\n\
                 tar: Exiting with failure status"
            )
        );
    }

    #[test]
    fn an_exec_failure_carries_the_commands_stderr_and_only_when_there_is_some() {
        let failure = || {
            classify_exec_status(
                Some(Some(status("Failure"))),
                "exec_stream_to_file",
                &["pg_dump"],
                "demo",
                "bk-pg-alpha",
            )
            .expect_err("a Failure status")
        };

        let mut said = StderrCapture::default();
        said.push(b"pg_dump: error: connection refused\n");
        let msg = with_stderr(failure(), Some(&said)).to_string();
        assert!(msg.contains("NonZeroExitCode"), "{msg}");
        assert!(
            msg.ends_with("command stderr:\npg_dump: error: connection refused"),
            "{msg}"
        );

        let bare = failure().to_string();
        let mut blank = StderrCapture::default();
        blank.push(b"  \n");
        assert_eq!(with_stderr(failure(), Some(&blank)).to_string(), bare);
        assert_eq!(with_stderr(failure(), None).to_string(), bare);
    }

    // -----------------------------------------------------------------------
    // Stub apiserver — a `tower::Service` handed to `kube::Client::new`
    // -----------------------------------------------------------------------

    /// One canned apiserver reply, matched on METHOD + exact request PATH
    /// (query strings are recorded but not matched, so a test can assert the
    /// server-side-apply parameters without encoding them twice).
    ///
    /// A route answers its replies in order, one per request, and repeats the
    /// last one from then on — so a route with one reply always gives it, and
    /// a test can script a pod that changes between two requests.
    struct Route {
        method: &'static str,
        path: String,
        replies: Vec<(u16, String)>,
        hits: std::sync::atomic::AtomicUsize,
        /// Run on every request the route answers, before it answers: what a
        /// test needs to happen at exactly that point of the exchange.
        on_hit: Option<Box<dyn Fn() + Send + Sync>>,
    }

    impl Route {
        fn next_reply(&self) -> (u16, String) {
            if let Some(hook) = &self.on_hit {
                hook();
            }
            let n = self.hits.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            self.replies[n.min(self.replies.len() - 1)].clone()
        }

        fn on_hit(mut self, hook: impl Fn() + Send + Sync + 'static) -> Self {
            self.on_hit = Some(Box::new(hook));
            self
        }
    }

    fn seq_route(method: &'static str, path: &str, replies: Vec<(u16, Value)>) -> Route {
        Route {
            method,
            path: path.to_string(),
            replies: replies
                .into_iter()
                .map(|(code, body)| (code, body.to_string()))
                .collect(),
            hits: std::sync::atomic::AtomicUsize::new(0),
            on_hit: None,
        }
    }

    fn ok_route(method: &'static str, path: &str, body: Value) -> Route {
        seq_route(method, path, vec![(200, body)])
    }

    fn status_body(code: u16, reason: &str, message: &str) -> Value {
        json!({
            "kind": "Status", "apiVersion": "v1", "status": "Failure",
            "reason": reason, "message": message, "code": code
        })
    }

    fn err_route(method: &'static str, path: &str, code: u16, reason: &str) -> Route {
        seq_route(
            method,
            path,
            vec![(code, status_body(code, reason, "stub apiserver rejection"))],
        )
    }

    /// A `tower::Service` that answers from a fixed route table and records
    /// every request as `"<METHOD> <uri>"`. Anything unrouted gets a genuine
    /// 404 `Status` body, so the not-found paths run through the real kube-rs
    /// error decoder rather than a hand-made `kube::Error`.
    #[derive(Clone)]
    struct StubApiServer {
        routes: Arc<Vec<Route>>,
        seen: Arc<Mutex<Vec<String>>>,
    }

    impl Service<Request<Body>> for StubApiServer {
        type Response = Response<Body>;
        type Error = Infallible;
        type Future = Ready<std::result::Result<Response<Body>, Infallible>>;

        fn poll_ready(
            &mut self,
            _cx: &mut Context<'_>,
        ) -> Poll<std::result::Result<(), Infallible>> {
            Poll::Ready(Ok(()))
        }

        fn call(&mut self, req: Request<Body>) -> Self::Future {
            let method = req.method().as_str().to_string();
            self.seen
                .lock()
                .unwrap()
                .push(format!("{method} {}", req.uri()));
            let path = req.uri().path();
            let (status, body) = match self
                .routes
                .iter()
                .find(|r| r.method == method && r.path == path)
            {
                Some(r) => r.next_reply(),
                None => (
                    404,
                    json!({
                        "kind": "Status", "apiVersion": "v1", "status": "Failure",
                        "reason": "NotFound", "message": format!("{path} not found"), "code": 404
                    })
                    .to_string(),
                ),
            };
            ready(Ok(Response::builder()
                .status(status)
                .header("content-type", "application/json")
                .body(Body::from(body.into_bytes()))
                .expect("build stub response")))
        }
    }

    /// A [`KubeRsExec`] wired to a stub apiserver, plus the runtime it blocks
    /// on. Mirrors `main`: the runtime is built up front and the SYNC trait
    /// methods are called from this (non-runtime) test thread via its handle.
    struct Harness {
        _rt: tokio::runtime::Runtime,
        exec: KubeRsExec,
        seen: Arc<Mutex<Vec<String>>>,
    }

    impl Harness {
        fn new(routes: Vec<Route>) -> Self {
            let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
            let seen = Arc::new(Mutex::new(Vec::new()));
            let svc = StubApiServer {
                routes: Arc::new(routes),
                seen: Arc::clone(&seen),
            };
            // `Client::new` wraps the service in a `tower::Buffer`, which
            // spawns its worker task — so it must be built inside the runtime.
            let client = {
                let _guard = rt.enter();
                kube::Client::new(svc, "default")
            };
            let exec = KubeRsExec::new(client, rt.handle().clone());
            Self {
                _rt: rt,
                exec,
                seen,
            }
        }

        fn seen(&self) -> Vec<String> {
            self.seen.lock().unwrap().clone()
        }
    }

    // --- discovery fixtures -------------------------------------------------

    fn apprafter_group_list() -> Value {
        json!({
            "kind": "APIGroupList", "apiVersion": "v1",
            "groups": [{
                "name": "apprafter.io",
                "versions": [{"groupVersion": "apprafter.io/v1alpha1", "version": "v1alpha1"}],
                "preferredVersion": {"groupVersion": "apprafter.io/v1alpha1", "version": "v1alpha1"}
            }]
        })
    }

    fn apprafter_resource_list() -> Value {
        json!({
            "kind": "APIResourceList", "apiVersion": "v1",
            "groupVersion": "apprafter.io/v1alpha1",
            "resources": [
                {"name": "applications", "singularName": "application", "namespaced": true,
                 "kind": "Application", "verbs": ["get", "list"]},
                {"name": "platformstacks", "singularName": "platformstack", "namespaced": true,
                 "kind": "PlatformStack", "verbs": ["get", "list"]}
            ]
        })
    }

    fn core_version_list() -> Value {
        json!({"kind": "APIVersions", "versions": ["v1"], "serverAddressByClientCIDRs": []})
    }

    fn core_resource_list() -> Value {
        json!({
            "kind": "APIResourceList", "apiVersion": "v1", "groupVersion": "v1",
            "resources": [
                {"name": "secrets", "singularName": "secret", "namespaced": true,
                 "kind": "Secret", "verbs": ["get", "list"]},
                {"name": "pods", "singularName": "pod", "namespaced": true,
                 "kind": "Pod", "verbs": ["get", "list"]}
            ]
        })
    }

    fn apprafter_discovery_routes() -> Vec<Route> {
        vec![
            ok_route("GET", "/apis", apprafter_group_list()),
            ok_route(
                "GET",
                "/apis/apprafter.io/v1alpha1",
                apprafter_resource_list(),
            ),
        ]
    }

    fn core_discovery_routes() -> Vec<Route> {
        vec![
            ok_route("GET", "/api", core_version_list()),
            ok_route("GET", "/api/v1", core_resource_list()),
        ]
    }

    // --- apply_and_wait_pod_ready ------------------------------------------

    fn helper_pod_spec() -> Value {
        json!({
            "apiVersion": "v1", "kind": "Pod",
            "metadata": {"name": "bk-pg-alpha", "namespace": "demo"},
            "spec": {"containers": [{"name": "dump", "image": "postgres:16-alpine"}]}
        })
    }

    fn running_ready_pod() -> Value {
        json!({
            "apiVersion": "v1", "kind": "Pod",
            "metadata": {"name": "bk-pg-alpha", "namespace": "demo"},
            "status": {"phase": "Running", "conditions": [{"type": "Ready", "status": "True"}]}
        })
    }

    /// INVARIANT: the helper pod is created by SERVER-SIDE APPLY under the
    /// `apprafter-backup` field manager, forced. The literals are spelled out
    /// here on purpose: the field manager is an ownership identity, so changing
    /// it silently orphans the fields a previous run owns, and dropping `force`
    /// makes a re-run fail on conflict against its own leftovers.
    #[test]
    fn apply_and_wait_pod_ready_server_side_applies_then_polls_the_pod() {
        let path = "/api/v1/namespaces/demo/pods/bk-pg-alpha";
        let h = Harness::new(vec![
            ok_route("PATCH", path, running_ready_pod()),
            ok_route("GET", path, running_ready_pod()),
        ]);

        h.exec
            .apply_and_wait_pod_ready(&helper_pod_spec())
            .expect("apply + wait");

        let seen = h.seen();
        assert_eq!(seen.len(), 2, "expected one apply then one poll: {seen:?}");
        assert!(
            seen[0].starts_with(&format!("PATCH {path}?")),
            "the pod must be applied at its own namespaced URL: {seen:?}"
        );
        assert!(
            seen[0].contains("fieldManager=apprafter-backup"),
            "server-side apply must claim the apprafter-backup manager: {seen:?}"
        );
        assert!(
            seen[0].contains("force=true"),
            "the apply must be forced so a re-run wins its own conflicts: {seen:?}"
        );
        assert_eq!(
            seen[1],
            format!("GET {path}"),
            "readiness must be polled by GETting the same pod: {seen:?}"
        );
    }

    /// The helper pod as the kubelet leaves it when a Secret its container
    /// reads a credential from is missing.
    fn config_blocked_pod() -> Value {
        json!({
            "apiVersion": "v1", "kind": "Pod",
            "metadata": {"name": "bk-pg-alpha", "namespace": "demo"},
            "status": {"phase": "Pending", "containerStatuses": [{
                "name": "dump", "ready": false, "restartCount": 0, "image": "postgres:18-alpine",
                "imageID": "",
                "state": {"waiting": {"reason": "CreateContainerConfigError",
                                      "message": "secret \"db-conn\" not found"}}
            }]}
        })
    }

    /// Wait for `bk-pg-alpha` in `demo` with bounds short enough for a test.
    fn wait_ready_briefly(h: &Harness, grace: std::time::Duration) -> Result<()> {
        let api: Api<Pod> = Api::namespaced(h.exec.client.clone(), "demo");
        h.exec.rt.block_on(h.exec.wait_pod_ready(
            &api,
            "bk-pg-alpha",
            "demo",
            std::time::Duration::from_secs(20),
            std::time::Duration::from_millis(50),
            grace,
        ))
    }

    /// WI-383: a helper reads its credentials from a Secret by reference, so
    /// a missing Secret or key leaves its container unable to start. The wait
    /// says so with the kubelet's words once that has held for the grace —
    /// not after the whole five-minute timeout, with none.
    #[test]
    fn a_helper_whose_credential_secret_is_missing_fails_the_wait_with_the_kubelets_words() {
        let path = "/api/v1/namespaces/demo/pods/bk-pg-alpha";
        let h = Harness::new(vec![ok_route("GET", path, config_blocked_pod())]);
        let started = std::time::Instant::now();
        let msg = wait_ready_briefly(&h, std::time::Duration::from_millis(300))
            .expect_err("a container that cannot be configured never becomes Ready")
            .to_string();
        assert!(
            started.elapsed() < std::time::Duration::from_secs(5),
            "gave up after {:?}, not at the grace",
            started.elapsed()
        );
        assert!(
            msg.starts_with(
                "helper pod demo/bk-pg-alpha cannot start its container: secret \"db-conn\" \
                 not found (CreateContainerConfigError)"
            ),
            "{msg}"
        );
    }

    /// One that clears within the grace — a Secret the kubelet had not yet
    /// synced — is waited out like any other start.
    #[test]
    fn a_config_error_that_clears_within_the_grace_is_waited_out() {
        let path = "/api/v1/namespaces/demo/pods/bk-pg-alpha";
        let h = Harness::new(vec![seq_route(
            "GET",
            path,
            vec![
                (200, config_blocked_pod()),
                (200, config_blocked_pod()),
                (200, running_ready_pod()),
            ],
        )]);
        wait_ready_briefly(&h, std::time::Duration::from_secs(10)).expect("Ready after all");
    }

    fn pod_in_phase(phase: &str) -> Value {
        json!({
            "apiVersion": "v1", "kind": "Pod",
            "metadata": {"name": "bk-pg-alpha", "namespace": "demo"},
            "status": {"phase": phase}
        })
    }

    fn not_found() -> (u16, Value) {
        (
            404,
            status_body(404, "NotFound", "pods \"bk-pg-alpha\" not found"),
        )
    }

    /// A helper pod left behind by an earlier run, ended (`Completed`): the
    /// apply answers with it unchanged, and it would never become Ready. It is
    /// deleted, waited out, and the pod created again — not waited on for five
    /// minutes and failed.
    #[test]
    fn an_ended_leftover_of_the_same_name_is_deleted_and_created_again() {
        let path = "/api/v1/namespaces/demo/pods/bk-pg-alpha";
        let h = Harness::new(vec![
            seq_route(
                "PATCH",
                path,
                vec![
                    (200, pod_in_phase("Succeeded")),
                    (200, pod_in_phase("Pending")),
                ],
            ),
            ok_route("DELETE", path, pod_in_phase("Succeeded")),
            // The wait for the delete, then the Ready poll of the new pod.
            seq_route("GET", path, vec![not_found(), (200, running_ready_pod())]),
        ]);

        h.exec
            .apply_and_wait_pod_ready(&helper_pod_spec())
            .expect("the leftover is replaced");

        let seen: Vec<String> = h
            .seen()
            .iter()
            .map(|r| r.split('?').next().unwrap().to_string())
            .collect();
        assert_eq!(
            seen,
            vec![
                format!("PATCH {path}"),
                format!("DELETE {path}"),
                format!("GET {path}"),
                format!("PATCH {path}"),
                format!("GET {path}"),
            ]
        );
    }

    /// A helper kept alive for six hours, as the builders shape one.
    fn six_hour_helper_spec() -> Value {
        let mut spec = helper_pod_spec();
        spec["spec"]["containers"][0]["command"] = json!(["sleep", "21600"]);
        spec
    }

    /// The same helper as the apply returns it: running, Ready, its container
    /// started `ago` before now.
    fn six_hour_helper_running_for(ago: std::time::Duration) -> Value {
        let started = chrono::Utc::now() - chrono::Duration::from_std(ago).unwrap();
        let mut pod = running_ready_pod();
        pod["spec"] = six_hour_helper_spec()["spec"].clone();
        pod["status"]["containerStatuses"] = json!([{"name": "dump", "state": {"running": {
            "startedAt": started.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
        }}}]);
        pod
    }

    /// A helper left running by a command stopped before its cleanup — an
    /// earlier run's, five hours into its six-hour keep-alive — would give
    /// this run's dump one hour. It is replaced, like an ended one.
    #[test]
    fn a_running_leftover_with_hours_of_its_keep_alive_used_is_replaced() {
        let path = "/api/v1/namespaces/demo/pods/bk-pg-alpha";
        let h = Harness::new(vec![
            seq_route(
                "PATCH",
                path,
                vec![
                    (
                        200,
                        six_hour_helper_running_for(std::time::Duration::from_secs(5 * 3600)),
                    ),
                    (200, pod_in_phase("Pending")),
                ],
            ),
            ok_route("DELETE", path, pod_in_phase("Running")),
            seq_route("GET", path, vec![not_found(), (200, running_ready_pod())]),
        ]);

        h.exec
            .apply_and_wait_pod_ready(&six_hour_helper_spec())
            .expect("the leftover is replaced");

        let seen: Vec<String> = h
            .seen()
            .iter()
            .map(|r| r.split('?').next().unwrap().to_string())
            .collect();
        assert_eq!(
            seen,
            vec![
                format!("PATCH {path}"),
                format!("DELETE {path}"),
                format!("GET {path}"),
                format!("PATCH {path}"),
                format!("GET {path}"),
            ]
        );
    }

    /// One another run created moments ago is used as it is: deleting it
    /// would kill that run's command, and it has its keep-alive nearly whole.
    #[test]
    fn a_running_helper_started_moments_ago_is_used_as_it_is() {
        let path = "/api/v1/namespaces/demo/pods/bk-pg-alpha";
        let fresh = six_hour_helper_running_for(std::time::Duration::from_secs(10));
        let h = Harness::new(vec![
            ok_route("PATCH", path, fresh.clone()),
            ok_route("GET", path, fresh),
        ]);
        h.exec
            .apply_and_wait_pod_ready(&six_hour_helper_spec())
            .expect("apply + wait");
        let seen: Vec<String> = h
            .seen()
            .iter()
            .map(|r| r.split('?').next().unwrap().to_string())
            .collect();
        assert_eq!(seen, vec![format!("PATCH {path}"), format!("GET {path}")]);
    }

    /// A leftover with a spec this run's cannot be applied over — an older
    /// runner's `sleep 3600`, say — is refused by the apiserver, and replaced.
    #[test]
    fn a_leftover_whose_spec_cannot_change_in_place_is_replaced() {
        let path = "/api/v1/namespaces/demo/pods/bk-pg-alpha";
        let immutable = status_body(
            422,
            "Invalid",
            "Pod \"bk-pg-alpha\" is invalid: spec: Forbidden: pod updates may not change \
             fields other than `spec.containers[*].image`",
        );
        let h = Harness::new(vec![
            seq_route(
                "PATCH",
                path,
                vec![(422, immutable), (200, pod_in_phase("Pending"))],
            ),
            ok_route("DELETE", path, pod_in_phase("Running")),
            seq_route(
                "GET",
                path,
                vec![
                    (200, pod_in_phase("Running")),
                    not_found(),
                    (200, running_ready_pod()),
                ],
            ),
        ]);

        h.exec
            .apply_and_wait_pod_ready(&helper_pod_spec())
            .expect("the leftover is replaced");

        let seen: Vec<String> = h
            .seen()
            .iter()
            .map(|r| r.split('?').next().unwrap().to_string())
            .collect();
        assert_eq!(
            seen,
            vec![
                format!("PATCH {path}"),
                format!("DELETE {path}"),
                // Still there once (terminating), then gone.
                format!("GET {path}"),
                format!("GET {path}"),
                format!("PATCH {path}"),
                format!("GET {path}"),
            ]
        );
    }

    /// Any other refusal is the run's error, and nothing is deleted: a 422 for
    /// another reason is not a leftover.
    #[test]
    fn another_invalid_apply_deletes_nothing() {
        let path = "/api/v1/namespaces/demo/pods/bk-pg-alpha";
        let h = Harness::new(vec![
            seq_route(
                "PATCH",
                path,
                vec![(
                    422,
                    status_body(
                        422,
                        "Invalid",
                        "Pod \"bk-pg-alpha\" is invalid: metadata.name",
                    ),
                )],
            ),
            ok_route("DELETE", path, pod_in_phase("Running")),
        ]);
        let err = h
            .exec
            .apply_and_wait_pod_ready(&helper_pod_spec())
            .expect_err("an invalid spec fails the step");
        assert!(err.to_string().starts_with("apply pod "), "{err}");
        assert!(
            h.seen().iter().all(|r| !r.starts_with("DELETE")),
            "{:?}",
            h.seen()
        );
    }

    /// The stop deletes what is in the live set, so a helper pod must be in it
    /// from its apply until its delete — and nothing else ever is.
    #[test]
    fn a_helper_pod_is_live_from_its_apply_until_its_delete_and_other_pods_never() {
        let path = "/api/v1/namespaces/demo/pods/bk-pg-alpha";
        let h = Harness::new(vec![
            ok_route("PATCH", path, running_ready_pod()),
            ok_route("GET", path, running_ready_pod()),
            ok_route("DELETE", path, running_ready_pod()),
        ]);
        let live = h.exec.live_helper_pods();
        let mut spec = helper_pod_spec();
        spec["metadata"]["labels"] = json!({"apprafter.io/backup-helper": "true"});

        h.exec
            .apply_and_wait_pod_ready(&spec)
            .expect("apply + wait");
        assert_eq!(
            live.snapshot(),
            vec![("demo".to_string(), "bk-pg-alpha".to_string())]
        );
        h.exec.delete_pod_best_effort("bk-pg-alpha", "demo");
        assert!(live.snapshot().is_empty(), "{:?}", live.snapshot());

        // A pod without the helper label — a test's own server, say — is not
        // the stop's to delete.
        h.exec
            .apply_and_wait_pod_ready(&helper_pod_spec())
            .expect("apply + wait");
        assert!(live.snapshot().is_empty(), "{:?}", live.snapshot());
    }

    /// Once the stop has begun, no helper pod is applied — not a single
    /// request goes out — while a pod without the helper label still is.
    #[test]
    fn no_helper_pod_is_applied_once_the_run_is_being_stopped() {
        let path = "/api/v1/namespaces/demo/pods/bk-pg-alpha";
        let h = Harness::new(vec![
            ok_route("PATCH", path, running_ready_pod()),
            ok_route("GET", path, running_ready_pod()),
        ]);
        h.exec.live_helper_pods().close();
        let mut spec = helper_pod_spec();
        spec["metadata"]["labels"] = json!({"apprafter.io/backup-helper": "true"});

        let err = h
            .exec
            .apply_and_wait_pod_ready(&spec)
            .expect_err("a helper pod is not applied once the run is being stopped");
        assert!(err.to_string().contains("being stopped"), "{err}");
        assert!(h.seen().is_empty(), "{:?}", h.seen());
        assert!(h.exec.live_helper_pods().snapshot().is_empty());

        h.exec
            .apply_and_wait_pod_ready(&helper_pod_spec())
            .expect("a pod that is not a backup helper is not the stop's");
    }

    /// The re-apply after a leftover's replacement is refused too, when the
    /// stop begins while the leftover is being deleted.
    #[test]
    fn a_leftovers_replacement_is_not_created_once_the_run_is_being_stopped() {
        let path = "/api/v1/namespaces/demo/pods/bk-pg-alpha";
        let live = Arc::new(Mutex::new(None::<crate::stop::LiveHelperPods>));
        let stop = Arc::clone(&live);
        let h = Harness::new(vec![
            seq_route(
                "PATCH",
                path,
                vec![
                    (200, pod_in_phase("Succeeded")),
                    (200, pod_in_phase("Pending")),
                ],
            ),
            // The stop begins as the leftover is deleted.
            ok_route("DELETE", path, pod_in_phase("Succeeded")).on_hit(move || {
                if let Some(live) = stop.lock().unwrap().as_ref() {
                    live.close();
                }
            }),
            seq_route("GET", path, vec![not_found(), (200, running_ready_pod())]),
        ]);
        *live.lock().unwrap() = Some(h.exec.live_helper_pods());
        let mut spec = helper_pod_spec();
        spec["metadata"]["labels"] = json!({"apprafter.io/backup-helper": "true"});

        let err = h
            .exec
            .apply_and_wait_pod_ready(&spec)
            .expect_err("the replacement is not created once the stop has begun");
        assert!(err.to_string().contains("being stopped"), "{err}");
        let seen: Vec<String> = h
            .seen()
            .iter()
            .map(|r| r.split('?').next().unwrap().to_string())
            .collect();
        assert_eq!(
            seen,
            vec![
                format!("PATCH {path}"),
                format!("DELETE {path}"),
                format!("GET {path}"),
            ],
            "no second PATCH"
        );
    }

    /// A rejected apply must abort immediately — never fall through into the
    /// readiness poll, where a stale pod of the same name from an earlier run
    /// could report Ready and let the backup dump the WRONG database.
    #[test]
    fn apply_and_wait_pod_ready_stops_at_a_rejected_apply() {
        let path = "/api/v1/namespaces/demo/pods/bk-pg-alpha";
        let h = Harness::new(vec![
            err_route("PATCH", path, 403, "Forbidden"),
            ok_route("GET", path, running_ready_pod()),
        ]);

        let err = h
            .exec
            .apply_and_wait_pod_ready(&helper_pod_spec())
            .expect_err("a 403 apply must fail the backup");
        assert!(
            format!("{err}").starts_with("apply pod "),
            "the error must name the apply step: {err}"
        );
        assert_eq!(
            h.seen().len(),
            1,
            "a failed apply must not be followed by a readiness poll: {:?}",
            h.seen()
        );
    }

    /// The spec is validated BEFORE any request goes out — a malformed spec is
    /// a programming error, not an apiserver round-trip.
    #[test]
    fn apply_and_wait_pod_ready_rejects_a_spec_without_a_namespace_offline() {
        let h = Harness::new(vec![]);
        assert!(h
            .exec
            .apply_and_wait_pod_ready(&json!({"metadata": {"name": "bk-pg-alpha"}}))
            .is_err());
        assert!(
            h.seen().is_empty(),
            "no request may be issued for an unusable spec: {:?}",
            h.seen()
        );
    }

    // --- get_secret_key ----------------------------------------------------

    fn secret_with(data: Value) -> Value {
        json!({
            "apiVersion": "v1", "kind": "Secret",
            "metadata": {"name": "pg-0-conn", "namespace": "demo"},
            "data": data
        })
    }

    /// INVARIANT: `Secret.data` reaches us as ALREADY-decoded bytes
    /// (k8s-openapi's `ByteString` does the transport base64), unlike the CLI's
    /// `KubectlExec`, which decodes a raw jsonpath value itself. A second
    /// decode here would corrupt every password.
    #[test]
    fn get_secret_key_returns_the_transport_decoded_value() {
        // "s3cr3t/pw" base64-encodes to "czNjcjN0L3B3".
        let h = Harness::new(vec![ok_route(
            "GET",
            "/api/v1/namespaces/demo/secrets/pg-0-conn",
            secret_with(json!({"pass": "czNjcjN0L3B3", "user": "YXBw"})),
        )]);

        assert_eq!(
            h.exec.get_secret_key("pg-0-conn", "demo", "pass").unwrap(),
            "s3cr3t/pw"
        );
        assert_eq!(
            h.exec.get_secret_key("pg-0-conn", "demo", "user").unwrap(),
            "app"
        );
        assert_eq!(
            h.seen()[0],
            "GET /api/v1/namespaces/demo/secrets/pg-0-conn",
            "the secret must be read from its own namespace: {:?}",
            h.seen()
        );
    }

    /// Every one of these is a hard error, not an empty string: `pg_dump` given
    /// an empty password/host silently produces a useless dump.
    #[test]
    fn get_secret_key_fails_loudly_on_an_absent_secret_key_or_data_block() {
        let h = Harness::new(vec![
            ok_route(
                "GET",
                "/api/v1/namespaces/demo/secrets/pg-0-conn",
                secret_with(json!({"pass": "czNjcjN0L3B3"})),
            ),
            ok_route(
                "GET",
                "/api/v1/namespaces/demo/secrets/empty",
                json!({
                    "apiVersion": "v1", "kind": "Secret",
                    "metadata": {"name": "empty", "namespace": "demo"}
                }),
            ),
            ok_route(
                "GET",
                "/api/v1/namespaces/demo/secrets/binary",
                // 0xFF is not valid UTF-8.
                secret_with(json!({"pass": "/w=="})),
            ),
        ]);

        // Present secret, absent key.
        assert!(h.exec.get_secret_key("pg-0-conn", "demo", "host").is_err());
        // Secret with no `data` block at all.
        assert!(h.exec.get_secret_key("empty", "demo", "pass").is_err());
        // Value that is not UTF-8.
        assert!(h.exec.get_secret_key("binary", "demo", "pass").is_err());
        // Secret that does not exist (the stub's default 404).
        assert!(h.exec.get_secret_key("nope", "demo", "pass").is_err());
    }

    // --- delete_pod_best_effort --------------------------------------------

    /// INVARIANT: teardown is BEST-EFFORT but not optional. It must actually
    /// issue the DELETE (a leaked `bk-pg-*` pod holds the database password in
    /// its env indefinitely) and must never propagate a failure — it runs from
    /// a `Drop` guard, where a panic would abort the process mid-backup.
    #[test]
    fn delete_pod_best_effort_issues_the_delete_and_swallows_a_failure() {
        let path = "/api/v1/namespaces/demo/pods/bk-pg-alpha";
        let h = Harness::new(vec![err_route("DELETE", path, 500, "InternalError")]);

        h.exec.delete_pod_best_effort("bk-pg-alpha", "demo");

        assert_eq!(
            h.seen(),
            vec![format!("DELETE {path}?")],
            "the helper pod must be deleted in its own namespace"
        );
    }

    #[test]
    fn delete_pod_best_effort_tolerates_an_already_gone_pod() {
        // No routes at all => the stub's default 404.
        let h = Harness::new(vec![]);
        h.exec.delete_pod_best_effort("bk-vol-data", "demo");
        assert_eq!(
            h.seen(),
            vec!["DELETE /api/v1/namespaces/demo/pods/bk-vol-data?".to_string()]
        );
    }

    // --- get_json ----------------------------------------------------------

    /// INVARIANT: the served version comes from DISCOVERY, never from a
    /// hardcoded GVK table — that is what keeps third-party CRD versions out of
    /// this file. The stub serves `apprafter.io` only at `v1alpha1`, so a
    /// hardcoded `v1` would miss the route and 404.
    #[test]
    fn get_json_resolves_a_named_cr_through_discovery() {
        let ps = json!({
            "apiVersion": "apprafter.io/v1alpha1", "kind": "PlatformStack",
            "metadata": {"name": "default", "namespace": "apprafter-system"},
            "status": {"currentVersion": "0.2.37"}
        });
        let mut routes = apprafter_discovery_routes();
        routes.push(ok_route(
            "GET",
            "/apis/apprafter.io/v1alpha1/namespaces/apprafter-system/platformstacks/default",
            ps.clone(),
        ));
        let h = Harness::new(routes);

        let got = h
            .exec
            .get_json(&[
                "get",
                "platformstack",
                "default",
                "-n",
                "apprafter-system",
                "-o",
                "json",
            ])
            .expect("get platformstack")
            .expect("the object is present");

        assert_eq!(got["status"]["currentVersion"], json!("0.2.37"));
        assert!(
            h.seen().iter().any(|r| r
                == "GET /apis/apprafter.io/v1alpha1/namespaces/apprafter-system/platformstacks/default"),
            "the discovered version must build the object URL: {:?}",
            h.seen()
        );
    }

    /// INVARIANT: an absent object is `Ok(None)`, not `Err` —
    /// `engine::read_secret_data` and `read_platform_version` both treat
    /// "missing" as a normal, skippable condition.
    #[test]
    fn get_json_returns_none_when_the_object_is_absent() {
        // Discovery succeeds; the object itself falls through to the 404.
        let h = Harness::new(apprafter_discovery_routes());
        let got = h
            .exec
            .get_json(&[
                "get",
                "platformstack",
                "default",
                "-n",
                "apprafter-system",
                "-o",
                "json",
            ])
            .expect("a 404 must not be an error");
        assert!(got.is_none(), "expected Ok(None), got {got:?}");
    }

    /// A LIST against something that is gone — a deleted namespace, a CRD
    /// removed between discovery and the read — 404s too. Both list shapes must
    /// read that as "nothing here", not as a failed backup.
    #[test]
    fn get_json_reads_a_404_list_as_absent_in_both_list_shapes() {
        // Discovery resolves both groups; neither collection URL is routed, so
        // the stub answers its default 404.
        let mut routes = apprafter_discovery_routes();
        routes.extend(core_discovery_routes());
        let h = Harness::new(routes);

        assert!(h
            .exec
            .get_json(&["get", "applications.apprafter.io", "-A", "-o", "json"])
            .expect("a 404 list must not be an error")
            .is_none());
        assert!(h
            .exec
            .get_json(&["get", "secrets", "-n", "gone", "-o", "json"])
            .expect("a 404 list must not be an error")
            .is_none());
    }

    /// A 404 whose body is NOT a `Status` — a proxy's plain `404 page not
    /// found` — is absent too. kube-rs only builds the error from the HTTP code
    /// when the body fails to decode, and [`is_not_found`] classifies by code,
    /// so this pins the one path where the code does not come from the body.
    /// kube 3 made every `Status` field optional (`ErrorResponse` required
    /// `status` + `code`), which moved what "fails to decode" means: only a
    /// non-object body still takes this path.
    #[test]
    fn get_json_reads_a_non_status_404_body_as_absent() {
        let path = "/apis/apprafter.io/v1alpha1/namespaces/apprafter-system/platformstacks/default";
        let mut routes = apprafter_discovery_routes();
        routes.push(Route {
            method: "GET",
            path: path.to_string(),
            replies: vec![(404, "404 page not found\n".to_string())],
            hits: std::sync::atomic::AtomicUsize::new(0),
            on_hit: None,
        });
        let h = Harness::new(routes);

        let got = h
            .exec
            .get_json(&[
                "get",
                "platformstack",
                "default",
                "-n",
                "apprafter-system",
                "-o",
                "json",
            ])
            .expect("a 404 must not be an error, whatever its body");
        assert!(got.is_none(), "expected Ok(None), got {got:?}");
        assert!(
            h.seen().iter().any(|r| r == &format!("GET {path}")),
            "the plain-text 404 must come from the object route: {:?}",
            h.seen()
        );
    }

    /// A forbidden read is NOT "absent": swallowing it would let a backup skip
    /// every CR the service account cannot see and still report success.
    #[test]
    fn get_json_propagates_a_non_404_rejection() {
        let mut routes = apprafter_discovery_routes();
        routes.push(err_route(
            "GET",
            "/apis/apprafter.io/v1alpha1/applications",
            403,
            "Forbidden",
        ));
        let h = Harness::new(routes);

        assert!(h
            .exec
            .get_json(&["get", "applications.apprafter.io", "-A", "-o", "json"])
            .is_err());
    }

    /// The list body is the REAL apiserver shape — the type on the List, not on
    /// each item — so the round-trip proves what actually reaches the engine,
    /// discovered GVK and all.
    #[test]
    fn get_json_lists_cluster_wide_in_the_kubectl_items_shape() {
        let mut routes = apprafter_discovery_routes();
        routes.push(ok_route(
            "GET",
            "/apis/apprafter.io/v1alpha1/applications",
            json!({
                "apiVersion": "apprafter.io/v1alpha1", "kind": "ApplicationList",
                "items": [{
                    "metadata": {"name": "alpha", "namespace": "demo"},
                    "spec": {"base": {"image": "nginx:1"}}
                }]
            }),
        ));
        let h = Harness::new(routes);

        let got = h
            .exec
            .get_json(&["get", "applications.apprafter.io", "-A", "-o", "json"])
            .expect("list applications")
            .expect("a list is always present");

        let items = got["items"].as_array().expect("an items array");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0]["metadata"]["name"], json!("alpha"));
        assert_eq!(items[0]["spec"]["base"]["image"], json!("nginx:1"));
        // The type the apiserver left off the item, restored from the
        // DISCOVERED resource — without it the object is unappliable.
        assert_eq!(items[0]["apiVersion"], json!("apprafter.io/v1alpha1"));
        assert_eq!(items[0]["kind"], json!("Application"));

        // `-A` must hit the CLUSTER-WIDE collection URL, not a namespaced one.
        assert!(
            h.seen()
                .iter()
                .any(|r| r.starts_with("GET /apis/apprafter.io/v1alpha1/applications")),
            "expected a cluster-wide list URL: {:?}",
            h.seen()
        );
    }

    /// `secrets` is dotless AND core: it must resolve through `/api` + `/api/v1`
    /// (core discovery), not `/apis`, and list under `/api/v1/namespaces/<ns>/`.
    #[test]
    fn get_json_lists_a_core_namespaced_resource_through_core_discovery() {
        let mut routes = core_discovery_routes();
        routes.push(ok_route(
            "GET",
            "/api/v1/namespaces/demo/secrets",
            json!({
                "apiVersion": "v1", "kind": "SecretList",
                "items": [{
                    "metadata": {"name": "stripe", "namespace": "demo"}
                }]
            }),
        ));
        let h = Harness::new(routes);

        let got = h
            .exec
            .get_json(&["get", "secrets", "-n", "demo", "-o", "json"])
            .expect("list secrets")
            .expect("a list is always present");
        assert_eq!(got["items"][0]["metadata"]["name"], json!("stripe"));
        assert_eq!(got["items"][0]["apiVersion"], json!("v1"));
        assert_eq!(got["items"][0]["kind"], json!("Secret"));

        let seen = h.seen();
        assert!(
            seen.iter().any(|r| r == "GET /api"),
            "core discovery starts at /api, not /apis: {seen:?}"
        );
        assert!(
            seen.iter()
                .any(|r| r.starts_with("GET /api/v1/namespaces/demo/secrets")),
            "expected the namespaced core collection URL: {seen:?}"
        );
    }

    /// A resource the group does not serve must be a clear error, NOT an empty
    /// list — `engine::list_items` only treats a *missing CRD* message as
    /// empty, so a silent `Ok(None)` here would drop real data from a backup.
    #[test]
    fn get_json_errors_when_the_resource_is_absent_from_its_group() {
        let h = Harness::new(apprafter_discovery_routes());
        let err = h
            .exec
            .get_json(&["get", "resourceclaims.apprafter.io", "-A", "-o", "json"])
            .expect_err("an unserved resource must be an error");
        let msg = format!("{err}");
        assert!(
            msg.contains("not found in API group"),
            "expected a discovery error, got: {msg}"
        );
    }

    #[test]
    fn get_json_errors_when_the_api_group_itself_is_missing() {
        // Discovery answers, but the group is not in the list.
        let h = Harness::new(vec![ok_route(
            "GET",
            "/apis",
            json!({"kind": "APIGroupList", "apiVersion": "v1", "groups": []}),
        )]);
        assert!(h
            .exec
            .get_json(&["get", "sealedsecrets.bitnami.com", "-A", "-o", "json"])
            .is_err());
    }

    /// An args vector the parser rejects must fail BEFORE any apiserver call —
    /// otherwise a malformed query would be paid for with a round-trip and,
    /// worse, could resolve to something plausible.
    #[test]
    fn get_json_rejects_unsupported_args_without_touching_the_apiserver() {
        let h = Harness::new(apprafter_discovery_routes());
        assert!(h.exec.get_json(&["describe", "pods", "-A"]).is_err());
        assert!(h.exec.get_json(&["get", "pods", "-o", "json"]).is_err());
        assert!(
            h.seen().is_empty(),
            "unparseable args must not reach the apiserver: {:?}",
            h.seen()
        );
    }

    // -----------------------------------------------------------------------
    // End-to-end: the bytes a scheduled run actually stages
    // -----------------------------------------------------------------------
    //
    // Every test above stops at `get_json`'s return value. The defect it
    // missed lived one layer further out: the engine wrote those values to
    // disk, restic snapshotted them, and a restore piped them to
    // `kubectl apply --server-side` — which is the ONLY consumer that needs
    // `apiVersion`/`kind`, and the only one that was not in any test. So the
    // gate below runs the REAL engine over `KubeRsExec` against a stub
    // apiserver serving REAL list bodies, and reads the staged files back.

    /// A [`ResticRunner`] that runs nothing. The gate is about the staged
    /// TREE, which is complete before restic is invoked; `run_stdout` answering
    /// `Ok` makes `ensure_repo` treat the repository as already present.
    struct NoopRestic;

    impl backup_core::ResticRunner for NoopRestic {
        fn run(&self, _argv: &[String], _pass: &str) -> Result<()> {
            Ok(())
        }
        fn run_stdout(&self, _argv: &[String], _pass: &str) -> Result<String> {
            Ok("[]".to_string())
        }
        fn run_backup(&self, _argv: &[String], _pass: &str) -> Result<Option<String>> {
            Ok(Some("stub-snapshot".to_string()))
        }
    }

    /// Discovery + collection routes for a one-namespace cluster carrying one
    /// of every CR the capture sweep stages, plus one imported certificate.
    ///
    /// The LIST bodies are the real apiserver shape: the type appears once, on
    /// the List, and NOT on the items. The single-object GET (`PlatformStack`)
    /// carries its own type, because a real single GET does — which is also why
    /// that one path was never broken.
    fn full_capture_routes() -> Vec<Route> {
        let groups = json!({
            "kind": "APIGroupList", "apiVersion": "v1",
            "groups": [
                {"name": "apprafter.io",
                 "versions": [{"groupVersion": "apprafter.io/v1alpha1", "version": "v1alpha1"}],
                 "preferredVersion": {"groupVersion": "apprafter.io/v1alpha1", "version": "v1alpha1"}},
                {"name": "argoproj.io",
                 "versions": [{"groupVersion": "argoproj.io/v1alpha1", "version": "v1alpha1"}],
                 "preferredVersion": {"groupVersion": "argoproj.io/v1alpha1", "version": "v1alpha1"}},
                {"name": "bitnami.com",
                 "versions": [{"groupVersion": "bitnami.com/v1alpha1", "version": "v1alpha1"}],
                 "preferredVersion": {"groupVersion": "bitnami.com/v1alpha1", "version": "v1alpha1"}}
            ]
        });
        let apprafter_resources = json!({
            "kind": "APIResourceList", "apiVersion": "v1",
            "groupVersion": "apprafter.io/v1alpha1",
            "resources": [
                {"name": "applications", "singularName": "application", "namespaced": true,
                 "kind": "Application", "verbs": ["get", "list"]},
                {"name": "platformstacks", "singularName": "platformstack", "namespaced": true,
                 "kind": "PlatformStack", "verbs": ["get", "list"]},
                {"name": "resourceclaims", "singularName": "resourceclaim", "namespaced": true,
                 "kind": "ResourceClaim", "verbs": ["get", "list"]},
                {"name": "sharedvolumes", "singularName": "sharedvolume", "namespaced": true,
                 "kind": "SharedVolume", "verbs": ["get", "list"]},
                {"name": "sourcecredentials", "singularName": "sourcecredential",
                 "namespaced": true, "kind": "SourceCredential", "verbs": ["get", "list"]}
            ]
        });
        let argo_resources = json!({
            "kind": "APIResourceList", "apiVersion": "v1",
            "groupVersion": "argoproj.io/v1alpha1",
            "resources": [{"name": "applications", "singularName": "application",
                           "namespaced": true, "kind": "Application", "verbs": ["get", "list"]}]
        });
        let sealed_resources = json!({
            "kind": "APIResourceList", "apiVersion": "v1",
            "groupVersion": "bitnami.com/v1alpha1",
            "resources": [{"name": "sealedsecrets", "singularName": "sealedsecret",
                           "namespaced": true, "kind": "SealedSecret", "verbs": ["get", "list"]}]
        });

        // `list_of` builds a real List body: typed collection, UNTYPED items.
        let list_of = |api_version: &str, kind: &str, items: Value| json!({"apiVersion": api_version, "kind": kind, "items": items});

        vec![
            ok_route("GET", "/apis", groups),
            ok_route("GET", "/apis/apprafter.io/v1alpha1", apprafter_resources),
            ok_route("GET", "/apis/argoproj.io/v1alpha1", argo_resources),
            ok_route("GET", "/apis/bitnami.com/v1alpha1", sealed_resources),
            ok_route("GET", "/api", core_version_list()),
            ok_route("GET", "/api/v1", core_resource_list()),
            // No claims — the extraction plan is empty, so no helper pod (and
            // therefore no WebSocket) is needed for the sweep to complete.
            ok_route(
                "GET",
                "/apis/apprafter.io/v1alpha1/namespaces/demo/resourceclaims",
                list_of("apprafter.io/v1alpha1", "ResourceClaimList", json!([])),
            ),
            // PlatformStack: a single GET, which DOES carry its own type.
            ok_route(
                "GET",
                "/apis/apprafter.io/v1alpha1/namespaces/apprafter-system/platformstacks/default",
                json!({
                    "apiVersion": "apprafter.io/v1alpha1", "kind": "PlatformStack",
                    "metadata": {"name": "default", "namespace": "apprafter-system"},
                    "spec": {"channel": "stable"}
                }),
            ),
            ok_route(
                "GET",
                "/apis/apprafter.io/v1alpha1/sourcecredentials",
                list_of(
                    "apprafter.io/v1alpha1",
                    "SourceCredentialList",
                    json!([{
                        "metadata": {"name": "gh", "namespace": "apprafter-system"},
                        "spec": {"git": {"backend": {"type": "github"}}}
                    }]),
                ),
            ),
            ok_route(
                "GET",
                "/apis/apprafter.io/v1alpha1/applications",
                list_of(
                    "apprafter.io/v1alpha1",
                    "ApplicationList",
                    json!([{
                        "metadata": {"name": "web", "namespace": "demo"},
                        "spec": {"base": {"image": "nginx:1", "replicas": 2}}
                    }]),
                ),
            ),
            ok_route(
                "GET",
                "/apis/argoproj.io/v1alpha1/applications",
                list_of(
                    "argoproj.io/v1alpha1",
                    "ApplicationList",
                    json!([{
                        "metadata": {
                            "name": "web-prod", "namespace": "argocd",
                            "labels": {"apprafter.io/managed-by": "apprafter"}
                        },
                        "spec": {"syncPolicy": {"automated": {}}}
                    }]),
                ),
            ),
            ok_route(
                "GET",
                "/apis/apprafter.io/v1alpha1/namespaces/demo/sharedvolumes",
                list_of(
                    "apprafter.io/v1alpha1",
                    "SharedVolumeList",
                    json!([{"metadata": {"name": "assets", "namespace": "demo"},
                            "spec": {"size": "5Gi"}}]),
                ),
            ),
            // No sealed secrets — the sealed sweep has nothing to read, which
            // keeps this gate on the CR + certificate paths.
            ok_route(
                "GET",
                "/apis/bitnami.com/v1alpha1/sealedsecrets",
                list_of("bitnami.com/v1alpha1", "SealedSecretList", json!([])),
            ),
            // The imported TLS certificate, captured off a LIST — the object
            // that reached `kubectl apply` as `{"data":…,"metadata":…,"type":…}`
            // and blew up `ApplyImportedCerts` on the operator's restore.
            ok_route(
                "GET",
                "/api/v1/namespaces/apprafter-system/secrets",
                list_of(
                    "v1",
                    "SecretList",
                    json!([{
                        "metadata": {
                            "name": "cf-cert", "namespace": "apprafter-system",
                            "labels": {"apprafter.io/cert-mode": "imported"}
                        },
                        "type": "kubernetes.io/tls",
                        "data": {"tls.crt": "eA==", "tls.key": "eQ=="}
                    }]),
                ),
            ),
        ]
    }

    /// Every `*.json` staged under `dir`, as `(relative path, parsed body)`.
    fn staged_objects(dir: &Path) -> Vec<(String, Value)> {
        let mut out = Vec::new();
        let Ok(entries) = std::fs::read_dir(dir) else {
            return out;
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.extension().and_then(|s| s.to_str()) != Some("json") {
                continue;
            }
            let body = std::fs::read(&p).expect("read a staged object");
            out.push((
                p.file_name().unwrap().to_string_lossy().into_owned(),
                serde_json::from_slice(&body).expect("a staged object is JSON"),
            ));
        }
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }

    /// INVARIANT (the gate that did not exist): every object the IN-CLUSTER
    /// runner stages is a document `kubectl apply` will accept — it carries a
    /// non-empty `apiVersion` AND `kind`.
    ///
    /// This is the whole defect, end to end. `restore` pipes these exact bytes
    /// to `kubectl apply --server-side`, which rejects a body without them:
    ///
    /// ```text
    /// error validating "STDIN": error validating data:
    /// [apiVersion not set, kind not set]
    /// ```
    ///
    /// Nothing inside the process ever needed the fields — the engine reads
    /// `/spec` and `/metadata`, and the restore recovers the kind from the
    /// staged FILENAME — so the whole pipeline was green while every snapshot
    /// the scheduled CronJob wrote was unrestorable. The assertion is therefore
    /// made on the FILES, not on a return value.
    #[test]
    fn every_object_the_runner_stages_carries_its_apiversion_and_kind() {
        let h = Harness::new(full_capture_routes());
        let staging = tempfile::tempdir().expect("staging tempdir");

        let summary = backup_core::engine::run_backup_with_summary(
            &h.exec,
            &NoopRestic,
            &backup_core::engine::BackupOpts {
                repo: "/tmp/does-not-matter/repo".to_string(),
                passphrase: "pw".to_string(),
                cluster_id: "demo-cluster".to_string(),
                cluster_uid: "11111111-2222-3333-4444-555555555555".to_string(),
                created_at: "2026-09-14T00:00:00Z".to_string(),
                platform_version: "0.2.65".to_string(),
                namespaces: vec!["demo".to_string()],
                is_subset: false,
                staging_root: staging.path().to_path_buf(),
                pg_image: "postgres:16-alpine".to_string(),
                helper_keep_alive: backup_core::helper_pod::DEFAULT_RUN_DEADLINE,
                staging_mode: backup_core::StagingMode::Monolithic,
                backup_host: Some("apprafter-backup".to_string()),
            },
        )
        .expect("the capture sweep completes against the stub apiserver");

        // The sweep really did capture everything, or an empty tree would pass
        // the invariant vacuously.
        assert_eq!(
            (summary.cr_count, summary.cert_count),
            (5, 1),
            "expected PlatformStack + SourceCredential + Application + \
             ArgoApplication + SharedVolume, and one imported certificate"
        );

        let data = staging.path().join("data");
        let crs = staged_objects(&data.join("crs"));
        let certs = staged_objects(&data.join(backup_core::engine::CERTS_DIR));
        assert_eq!(crs.len(), 5, "staged CRs: {crs:?}");
        assert_eq!(certs.len(), 1, "staged certificates: {certs:?}");

        for (file, obj) in crs.iter().chain(certs.iter()) {
            let api_version = obj.get("apiVersion").and_then(Value::as_str);
            let kind = obj.get("kind").and_then(Value::as_str);
            assert!(
                api_version.is_some_and(|v| !v.is_empty()),
                "staged {file} has no apiVersion — `kubectl apply` rejects it: {obj}"
            );
            assert!(
                kind.is_some_and(|v| !v.is_empty()),
                "staged {file} has no kind — `kubectl apply` rejects it: {obj}"
            );
        }

        // And the identities are the RIGHT ones, not merely present: a wrong
        // apiVersion is applied to the wrong CRD (or to nothing).
        let typed: Vec<(&str, &str, &str)> = crs
            .iter()
            .chain(certs.iter())
            .map(|(f, o)| {
                (
                    f.as_str(),
                    o["apiVersion"].as_str().unwrap(),
                    o["kind"].as_str().unwrap(),
                )
            })
            .collect();
        assert_eq!(
            typed,
            vec![
                (
                    "000-PlatformStack-apprafter-system-default.json",
                    "apprafter.io/v1alpha1",
                    "PlatformStack"
                ),
                (
                    "001-SourceCredential-apprafter-system-gh.json",
                    "apprafter.io/v1alpha1",
                    "SourceCredential"
                ),
                (
                    "002-Application-demo-web.json",
                    "apprafter.io/v1alpha1",
                    "Application"
                ),
                (
                    // The backup's own `ArgoApplication` tag is a FILENAME
                    // convention; the object itself is an argoproj.io
                    // `Application`, and that is what must be applied.
                    "003-ArgoApplication-argocd-web-prod.json",
                    "argoproj.io/v1alpha1",
                    "Application"
                ),
                (
                    "004-SharedVolume-demo-assets.json",
                    "apprafter.io/v1alpha1",
                    "SharedVolume"
                ),
                ("cf-cert.json", "v1", "Secret"),
            ]
        );
    }
}
