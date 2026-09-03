// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! In-cluster KubeExec via kube-rs. Integration-verified by the chunk-6
//! real-Hetzner walk; unit-verified below against a stub apiserver (see the
//! `tests` module for exactly which parts a cluster is still required for —
//! in short, the two WebSocket `exec` streams and nothing else).
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
use k8s_openapi::apimachinery::pkg::apis::meta::v1::Status;
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
            let (name, ns) = pod_identity(spec)?;

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
// Tests
// ---------------------------------------------------------------------------

/// # What a cluster is still required for
///
/// `exec_stream_to_file` / `exec_stream_from_file` open the apiserver's
/// `pods/exec` **WebSocket** subresource. `Api::exec` returns an
/// [`AttachedProcess`] whose stdin/stdout handles and status channel only exist
/// once that upgrade has completed against a real, *running* pod; the type has
/// no public constructor and no in-memory transport. Their bodies are therefore
/// covered by the real-Hetzner walk and the kind-based `e2e/backup-*` scripts,
/// NOT here. What *is* covered here is the verdict those two methods end on —
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
            // neither `-A` nor `-n`: scope is undefined
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
    #[test]
    fn is_not_found_is_true_for_404_and_nothing_else() {
        let api = |code| {
            kube::Error::Api(kube::core::ErrorResponse {
                status: "Failure".into(),
                message: "boom".into(),
                reason: "Whatever".into(),
                code,
            })
        };
        assert!(is_not_found(&api(404)));
        assert!(!is_not_found(&api(403)));
        assert!(!is_not_found(&api(410)));
        assert!(!is_not_found(&api(500)));
        assert!(!is_not_found(&kube::Error::TlsRequired));
    }

    /// INVARIANT: the value handed to `engine::list_items` must be the kubectl
    /// `{"items":[…]}` envelope AND each item must survive the round-trip with
    /// its arbitrary CR body intact — the engine reads `/spec/type`,
    /// `/status/connectionSecretRef` and friends straight off these objects.
    #[test]
    fn list_to_value_wraps_items_and_preserves_each_object_verbatim() {
        let list: ObjectList<DynamicObject> = serde_json::from_value(json!({
            "apiVersion": "apprafter.io/v1alpha1",
            "kind": "ResourceClaimList",
            "items": [{
                "apiVersion": "apprafter.io/v1alpha1",
                "kind": "ResourceClaim",
                "metadata": {"name": "pg-0", "namespace": "demo"},
                "spec": {"type": "pg"},
                "status": {"connectionSecretRef": "pg-0-conn"}
            }]
        }))
        .expect("decode a claim list");

        let v = list_to_value(list).expect("serialize list");
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

    #[test]
    fn list_to_value_of_an_empty_list_is_an_empty_items_array() {
        let list: ObjectList<DynamicObject> =
            serde_json::from_value(json!({"apiVersion": "v1", "kind": "List", "items": []}))
                .expect("decode an empty list");
        assert_eq!(list_to_value(list).unwrap(), json!({"items": []}));
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
    // Stub apiserver — a `tower::Service` handed to `kube::Client::new`
    // -----------------------------------------------------------------------

    /// One canned apiserver reply, matched on METHOD + exact request PATH
    /// (query strings are recorded but not matched, so a test can assert the
    /// server-side-apply parameters without encoding them twice).
    struct Route {
        method: &'static str,
        path: String,
        status: u16,
        body: String,
    }

    fn ok_route(method: &'static str, path: &str, body: Value) -> Route {
        Route {
            method,
            path: path.to_string(),
            status: 200,
            body: body.to_string(),
        }
    }

    fn err_route(method: &'static str, path: &str, code: u16, reason: &str) -> Route {
        Route {
            method,
            path: path.to_string(),
            status: code,
            body: json!({
                "kind": "Status", "apiVersion": "v1", "status": "Failure",
                "reason": reason, "message": "stub apiserver rejection", "code": code
            })
            .to_string(),
        }
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
                Some(r) => (r.status, r.body.clone()),
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

    #[test]
    fn get_json_lists_cluster_wide_in_the_kubectl_items_shape() {
        let mut routes = apprafter_discovery_routes();
        routes.push(ok_route(
            "GET",
            "/apis/apprafter.io/v1alpha1/applications",
            json!({
                "apiVersion": "apprafter.io/v1alpha1", "kind": "ApplicationList",
                "items": [{
                    "apiVersion": "apprafter.io/v1alpha1", "kind": "Application",
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
                    "apiVersion": "v1", "kind": "Secret",
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
}
