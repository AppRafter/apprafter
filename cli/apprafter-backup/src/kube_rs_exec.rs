// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! In-cluster KubeExec via kube-rs. Cluster-only — integration-verified by the
//! chunk-6 real-Hetzner walk (no unit tests possible without an apiserver +
//! running pods).
//!
//! [`KubeRsExec`] implements [`backup_core::KubeExec`] using kube-rs 0.95 so the
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

use backup_core::KubeExec;
use cli_core::{CliError, Result};
use k8s_openapi::api::core::v1::{Pod, Secret};
use kube::api::{
    Api, AttachParams, AttachedProcess, DeleteParams, DynamicObject, ListParams, ObjectList, Patch,
    PatchParams,
};
use kube::discovery::{self, ApiResource};
use serde_json::Value;
use tokio::io::AsyncWriteExt;

/// Field manager used for the server-side apply of helper Pods. Distinct from
/// the operator's `apprafter-operator` and the CLI's other managers so its
/// ownership never collides with a real workload's.
const FIELD_MANAGER: &str = "apprafter-backup";

/// Total budget for the pod-Ready poll (mirrors `kubectl wait --timeout=300s`
/// used by `KubectlExec`).
const POD_READY_TIMEOUT_SECS: u64 = 300;

/// Interval between pod-Ready polls.
const POD_READY_POLL_INTERVAL_SECS: u64 = 2;

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
}

impl KubeRsExec {
    /// Construct a runner over `client`, blocking on `rt` for every call.
    pub fn new(client: kube::Client, rt: tokio::runtime::Handle) -> Self {
        Self { client, rt }
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
            if ar.plural == plural {
                return Ok(ar);
            }
        }
        Err(CliError::Other(format!(
            "resource {resource:?} (plural {plural:?}) not found in API group {group:?} at \
             version {ver:?}"
        )))
    }
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

/// Parsed shape of a `get_json` args vector.
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
}

/// Parse the kubectl-style `args` vector into a [`GetShape`].
///
/// Handles exactly the shapes the engine / extract layer pass (confirmed by
/// grepping every `get_json` call site):
///
/// * `["get", "<resource>", "-A", "-o", "json"]`
/// * `["get", "<resource>", "-n", "<ns>", "-o", "json"]`
/// * `["get", "<resource>", "<name>", "-n", "<ns>", "-o", "json"]`
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
        (_, _, _) => Err(CliError::Other(format!(
            "unsupported get_json args (need `-A` for a cluster list, `-n <ns>` for a namespaced \
             list, or `-n <ns> <name>` for a single get): {args:?}"
        ))),
    }
}

/// Classify a kube-rs error as "not found" (→ `Ok(None)` for `get_json`) vs a
/// real error. Mirrors `KubectlExec::get_json`, which returns `Ok(None)` when
/// kubectl's stderr carries `NotFound` / `not found`.
fn is_not_found(err: &kube::Error) -> bool {
    matches!(err, kube::Error::Api(ae) if ae.code == 404)
}

/// Serialize a kube-rs `ObjectList<DynamicObject>` into the `{"items":[...]}`
/// shape kubectl emits, so `engine::list_items` can read `.items[]` unchanged.
fn list_to_value(list: ObjectList<DynamicObject>) -> Result<Value> {
    let items = list
        .items
        .into_iter()
        .map(|o| serde_json::to_value(o).map_err(CliError::from))
        .collect::<Result<Vec<Value>>>()?;
    Ok(serde_json::json!({ "items": items }))
}

impl KubeExec for KubeRsExec {
    fn apply_and_wait_pod_ready(&self, spec: &Value) -> Result<()> {
        self.rt.block_on(async {
            let name = spec["metadata"]["name"]
                .as_str()
                .ok_or_else(|| CliError::Other("pod spec missing metadata.name".into()))?
                .to_string();
            let ns = spec["metadata"]["namespace"]
                .as_str()
                .ok_or_else(|| CliError::Other("pod spec missing metadata.namespace".into()))?
                .to_string();

            let api: Api<Pod> = Api::namespaced(self.client.clone(), &ns);

            // Server-side apply (mirrors `kubectl apply -f -`).
            let pp = PatchParams::apply(FIELD_MANAGER).force();
            api.patch(&name, &pp, &Patch::Apply(spec))
                .await
                .map_err(|e| CliError::Other(format!("apply pod {name} in {ns}: {e}")))?;

            // Poll until Running + Ready (mirrors `kubectl wait
            // --for=condition=Ready --timeout=300s`).
            let deadline = tokio::time::Instant::now()
                + tokio::time::Duration::from_secs(POD_READY_TIMEOUT_SECS);
            loop {
                let pod = api.get(&name).await.map_err(|e| {
                    CliError::Other(format!("get pod {name} in {ns} while waiting Ready: {e}"))
                })?;
                if pod_is_ready(&pod) {
                    return Ok(());
                }
                if tokio::time::Instant::now() >= deadline {
                    return Err(CliError::Other(format!(
                        "pod {name} in {ns} did not reach Ready within {POD_READY_TIMEOUT_SECS}s"
                    )));
                }
                tokio::time::sleep(tokio::time::Duration::from_secs(
                    POD_READY_POLL_INTERVAL_SECS,
                ))
                .await;
            }
        })
    }

    fn exec_stream_to_file(&self, pod: &str, ns: &str, argv: &[&str], out: &Path) -> Result<()> {
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

            let mut proc_stdout = attached.stdout().ok_or_else(|| {
                CliError::Other("exec_stream_to_file: attached process exposed no stdout".into())
            })?;

            let mut file = tokio::fs::File::create(out).await.map_err(|e| {
                CliError::Other(format!(
                    "exec_stream_to_file: create output file {}: {e}",
                    out.display()
                ))
            })?;

            // Stream the whole process stdout to the file.
            tokio::io::copy(&mut proc_stdout, &mut file)
                .await
                .map_err(|e| {
                    CliError::Other(format!(
                        "exec_stream_to_file: copy command stdout to {}: {e}",
                        out.display()
                    ))
                })?;
            file.flush().await.map_err(|e| {
                CliError::Other(format!(
                    "exec_stream_to_file: flush output file {}: {e}",
                    out.display()
                ))
            })?;

            check_exec_status(&mut attached, "exec_stream_to_file", argv, ns, pod).await
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
            tokio::io::copy(&mut file, &mut proc_stdin)
                .await
                .map_err(|e| {
                    CliError::Other(format!(
                        "exec_stream_from_file: copy {} to command stdin: {e}",
                        input.display()
                    ))
                })?;
            proc_stdin.shutdown().await.map_err(|e| {
                CliError::Other(format!("exec_stream_from_file: close command stdin: {e}"))
            })?;
            drop(proc_stdin);

            check_exec_status(&mut attached, "exec_stream_from_file", argv, ns, pod).await
        })
    }

    fn delete_pod_best_effort(&self, name: &str, ns: &str) {
        // Best-effort: swallow every error (mirrors `kubectl delete pod
        // --ignore-not-found`). Never panics.
        let _ = self.rt.block_on(async {
            let api: Api<Pod> = Api::namespaced(self.client.clone(), ns);
            api.delete(name, &DeleteParams::default()).await
        });
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
                        Ok(list) => Ok(Some(list_to_value(list)?)),
                        Err(e) if is_not_found(&e) => Ok(None),
                        Err(e) => Err(CliError::Other(format!("list {resource} -A: {e}"))),
                    }
                }
                GetShape::ListNs { resource, ns } => {
                    let ar = self.resolve_resource(&resource).await?;
                    let api: Api<DynamicObject> =
                        Api::namespaced_with(self.client.clone(), &ns, &ar);
                    match api.list(&ListParams::default()).await {
                        Ok(list) => Ok(Some(list_to_value(list)?)),
                        Err(e) if is_not_found(&e) => Ok(None),
                        Err(e) => Err(CliError::Other(format!("list {resource} -n {ns}: {e}"))),
                    }
                }
                GetShape::GetNamed { resource, ns, name } => {
                    let ar = self.resolve_resource(&resource).await?;
                    let api: Api<DynamicObject> =
                        Api::namespaced_with(self.client.clone(), &ns, &ar);
                    match api.get(&name).await {
                        Ok(obj) => Ok(Some(serde_json::to_value(obj).map_err(CliError::from)?)),
                        Err(e) if is_not_found(&e) => Ok(None),
                        Err(e) => Err(CliError::Other(format!(
                            "get {resource} {name} -n {ns}: {e}"
                        ))),
                    }
                }
            }
        })
    }
}

/// True iff the Pod is `Running` AND carries a `Ready` condition with
/// `status == "True"` (the kube-rs analogue of
/// `kubectl wait --for=condition=Ready`).
fn pod_is_ready(pod: &Pod) -> bool {
    let Some(status) = pod.status.as_ref() else {
        return false;
    };
    if status.phase.as_deref() != Some("Running") {
        return false;
    }
    status
        .conditions
        .as_ref()
        .map(|conds| {
            conds
                .iter()
                .any(|c| c.type_ == "Ready" && c.status == "True")
        })
        .unwrap_or(false)
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
    match attached.take_status() {
        Some(status_fut) => match status_fut.await {
            Some(status) if status.status.as_deref() == Some("Success") => Ok(()),
            Some(status) => Err(CliError::Other(format!(
                "{context}: exec {argv:?} in {ns}/{pod} failed (status={:?}, reason={}): {}",
                status.status,
                status.reason.clone().unwrap_or_default(),
                status.message.clone().unwrap_or_default(),
            ))),
            None => Err(CliError::Other(format!(
                "{context}: exec {argv:?} in {ns}/{pod} returned no terminal status — cannot \
                 verify exit code (failing closed to avoid a truncated backup)",
            ))),
        },
        None => Err(CliError::Other(format!(
            "{context}: exec {argv:?} in {ns}/{pod} exposed no status channel — cannot verify \
             exit code (failing closed)",
        ))),
    }
}
