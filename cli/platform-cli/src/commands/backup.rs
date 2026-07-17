// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! `apprafter export` / `apprafter backup` — 2.6d export + backup command
//! logic.
//!
//! Two kinds of data pull, sharing the same native-extraction engine
//! (`cli_providers::backup`):
//!
//! * **`export`** (Kind 1) — pull native data (pg dumps, volume tars, redis
//!   snapshots) to a plain local folder + a `manifest.json`. No CRs, no
//!   secrets, no encryption. A debugging / one-off-recovery convenience.
//!
//! * **`backup`** (Kind 2) — the same extraction PLUS the serialized config
//!   and app CRs, PLUS the decrypted user secrets, all staged and then
//!   wrapped into an encrypted `restic` repository. This is the
//!   disaster-recovery artifact [`crate::commands::restore::run_restore`]
//!   consumes.
//!
//! ## Default scope = WHOLE CLUSTER
//!
//! Both commands default to every namespace that hosts an AppRafter
//! `Application` — the *app-namespace set*, derived from
//! `kubectl get applications.apprafter.io -A`, NOT `kubectl get ns` (the
//! latter would sweep in platform/system namespaces we must never replay).
//! `--namespace <ns>` (repeatable) / `--select` narrows the set.
//!
//! ## User vs platform discrimination (H1 — load-bearing)
//!
//! A restore must NOT clobber the bootstrap's own platform objects, so the
//! backup captures user material only:
//!
//! * **Argo `Application`s** are filtered to those carrying the
//!   `apprafter.io/managed-by=apprafter` label ([`is_user_argo_app`]). The
//!   platform umbrella + component Argo Applications LACK it → never
//!   serialized (else restore double-owns them against bootstrap).
//! * **Config CRs** are captured by KIND (`PlatformStack/default`,
//!   `SourceCredential` cluster-wide), not by an namespace sweep. There is no
//!   in-cluster `Infrastructure` CR (M2: it is the local manifest) — its
//!   topology rides `manifest.platform_version`, and a missing
//!   `infrastructures.apprafter.io` listing is expected, never an error.
//!
//! ## SourceCredential material — follow-the-reference (rev-5)
//!
//! `SourceCredential` CRs and their sealed material live in
//! `apprafter-system`, OUTSIDE the app-namespace set, so the app-ns secret
//! sweep MISSES them. Instead, for each `SourceCredential` we resolve its
//! `spec.git.backend.sealedSecretRef` + `spec.registry.backend.sealedSecretRef`
//! ([`sourcecred_material_refs`]) and read the underlying
//! controller-unsealed Secret directly. Two distinct secret-capture paths:
//! (a) app user secrets — SealedSecret-backed sweep scoped to the app-ns set;
//! (b) SourceCredential material — follow-the-reference, cluster-wide.

use std::collections::BTreeMap;
use std::io::{self, BufRead, BufReader, IsTerminal, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use backup_core::engine::BackupOpts;
use backup_core::extract::plan_extraction;
use backup_core::{KubeExec, ResticRunner, StagingMode};
use base64::Engine as _;
use cli_core::{CliError, Result};
use cli_providers::backup::extract::run_extraction;
use cli_providers::backup::images::pg_helper_image;
use cli_providers::backup::manifest::BackupManifest;
use cli_providers::backup::restic::restic_snapshots_argv;
use cli_providers::backup::ResourceRef;
use serde_json::Value;

use crate::commands::k8s_helpers::{
    ensure_kubeconfig_tempfile, kubectl_get_json, kubectl_get_json_cluster_wide,
};
use crate::commands::state_paths::resolve_state_paths;

/// Namespace the `PlatformStack` singleton + `SourceCredential`s + their sealed
/// material live in. Mirrors `repo_creds::SOURCECRED_NAMESPACE` /
/// `platform::PLATFORMSTACK_NAMESPACE` (both private to their modules).
/// Exported `pub(crate)` so `restore.rs` can use it without re-declaring.
pub(crate) const APPRAFTER_SYSTEM_NAMESPACE: &str = "apprafter-system";
pub(crate) const PLATFORMSTACK_NAME: &str = "default";

// ---------------------------------------------------------------------------
// Pure helpers (the tested core — some exported pub(crate) for restore.rs)
// ---------------------------------------------------------------------------

/// Distinct, sorted namespaces of the AppRafter `Application` CRs. When
/// `select` is non-empty the result is intersected with it (the operator
/// asked for a subset).
///
/// `apprafter_apps` is the `.items[]` array of
/// `kubectl get applications.apprafter.io -A -o json`. The app-namespace set
/// derives from THESE, never from `kubectl get ns` — that distinction is the
/// whole point of the H1 review (platform/system namespaces must never enter
/// the backup scope).
pub fn app_namespaces(apprafter_apps: &[Value], select: &[String]) -> Vec<String> {
    let mut set: Vec<String> = apprafter_apps
        .iter()
        .filter_map(|a| {
            a.pointer("/metadata/namespace")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .collect();
    set.sort();
    set.dedup();
    if select.is_empty() {
        set
    } else {
        set.into_iter().filter(|ns| select.contains(ns)).collect()
    }
}

/// Resolve the backup passphrase: explicit `--passphrase` → `RESTIC_PASSWORD`
/// env → (on a TTY) an interactive masked prompt. The repository holds
/// DECRYPTED secrets, so an empty / absent passphrase is NEVER allowed: when
/// neither source is set and we're not on a TTY, this errors instead of
/// silently producing an unencrypted-by-empty-key repo.
pub fn backup_passphrase_or_error(
    arg: Option<&str>,
    env: Option<&str>,
    is_tty: bool,
) -> Result<String> {
    if let Some(p) = arg.or(env) {
        if p.is_empty() {
            return Err(CliError::Other(
                "empty backup passphrase — the repository holds decrypted secrets and must be \
                 encrypted; pass a non-empty `--passphrase` or set RESTIC_PASSWORD"
                    .into(),
            ));
        }
        return Ok(p.to_string());
    }
    if !is_tty {
        return Err(CliError::Other(
            "no backup passphrase — pass `--passphrase <value>`, set RESTIC_PASSWORD, or run from \
             an interactive shell for a prompt (the repo holds decrypted secrets and must be \
             encrypted)"
                .into(),
        ));
    }
    let pass = inquire::Password::new("Backup passphrase:")
        .with_display_mode(inquire::PasswordDisplayMode::Masked)
        .with_help_message("encrypts the restic repository; you'll need it to restore")
        .prompt()
        .map_err(|e| CliError::Other(format!("passphrase prompt: {e}")))?;
    if pass.is_empty() {
        return Err(CliError::Other(
            "passphrase cannot be empty — the repo holds decrypted secrets".into(),
        ));
    }
    Ok(pass)
}

/// `(namespace, name)` of each sealed material Secret a `SourceCredential`
/// references via `spec.git.backend.sealedSecretRef` +
/// `spec.registry.backend.sealedSecretRef`. The `namespace` field of each ref
/// is optional and DEFAULTS to the SourceCredential's own namespace
/// (`apprafter-system`) — matching the operator's resolution
/// (`operator-core::sourcecredential::SealedSecretRef`). The launch default
/// points both refs at the same material Secret, so the result may contain
/// duplicates; the caller dedups.
pub fn sourcecred_material_refs(sc: &Value) -> Vec<(String, String)> {
    let own_ns = sc
        .pointer("/metadata/namespace")
        .and_then(Value::as_str)
        .unwrap_or(APPRAFTER_SYSTEM_NAMESPACE);
    let mut refs = Vec::new();
    for ptr in [
        "/spec/git/backend/sealedSecretRef",
        "/spec/registry/backend/sealedSecretRef",
    ] {
        if let Some(r) = sc.pointer(ptr) {
            if let Some(name) = r.pointer("/name").and_then(Value::as_str) {
                let ns = r
                    .pointer("/namespace")
                    .and_then(Value::as_str)
                    .unwrap_or(own_ns)
                    .to_string();
                refs.push((ns, name.to_string()));
            }
        }
    }
    refs
}

// ---------------------------------------------------------------------------
// Impure helpers (walk-validated)
// ---------------------------------------------------------------------------

/// `.items[]` of `kubectl get <resource> [-n ns | -A] -o json`, or an empty
/// `Vec` when the resource lists nothing / its CRD is absent (e.g.
/// `infrastructures.apprafter.io`, which legitimately has no instances — M2).
/// Exported `pub(crate)` for use by `restore.rs`.
pub(crate) fn list_items(
    resource: &str,
    namespace: Option<&str>,
    kubeconfig: &Path,
) -> Result<Vec<Value>> {
    match kubectl_get_json_cluster_wide(resource, namespace, kubeconfig) {
        Ok(Some(v)) => Ok(v
            .get("items")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()),
        Ok(None) => Ok(Vec::new()),
        Err(e) => {
            // A missing CRD (no `infrastructures` kind) is not a backup failure.
            let msg = format!("{e}");
            if msg.contains("the server doesn't have a resource type")
                || msg.contains("doesn't have a resource type")
            {
                Ok(Vec::new())
            } else {
                Err(e)
            }
        }
    }
}

/// Read the full `.data` of a Secret (all keys), base64-decoding each value,
/// as a `name → bytes` map, plus the secret's `.type` field (defaulting to
/// `"Opaque"` when absent). Returns `Ok(None)` when the Secret is absent.
///
/// Exported `pub(crate)` for use by `restore.rs` (which needs raw connection
/// creds for the pg load path — it ignores the returned type for that use).
#[allow(clippy::type_complexity)]
pub(crate) fn read_secret_data(
    name: &str,
    namespace: &str,
    kubeconfig: &Path,
) -> Result<Option<(BTreeMap<String, Vec<u8>>, String)>> {
    let json = kubectl_get_json("secret", Some(name), Some(namespace), kubeconfig)?;
    let Some(json) = json else { return Ok(None) };
    let secret_type = json
        .pointer("/type")
        .and_then(Value::as_str)
        .unwrap_or("Opaque")
        .to_string();
    let mut out = BTreeMap::new();
    if let Some(data) = json.pointer("/data").and_then(Value::as_object) {
        for (k, v) in data {
            let b64 = v.as_str().unwrap_or("");
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(b64)
                .map_err(|e| {
                    CliError::Other(format!("decode secret {namespace}/{name} key {k}: {e}"))
                })?;
            out.insert(k.clone(), bytes);
        }
    }
    Ok(Some((out, secret_type)))
}

/// Read `PlatformStack/default.status.currentVersion` (the live platform-stack
/// version) so `restore --reprovision` bootstraps the target at the same
/// version. Falls back to `"unknown"` when the field is unset (a freshly
/// bootstrapped cluster whose operator hasn't stamped status yet).
/// Exported `pub(crate)` for use by `restore.rs`.
pub(crate) fn read_platform_version(kubeconfig: &Path) -> Result<String> {
    let ps = kubectl_get_json(
        "platformstack",
        Some(PLATFORMSTACK_NAME),
        Some(APPRAFTER_SYSTEM_NAMESPACE),
        kubeconfig,
    )?;
    Ok(ps
        .as_ref()
        .and_then(|p| p.pointer("/status/currentVersion"))
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_string())
}

/// Build the `ResourceRef`s recorded in `manifest.json` from the captured
/// config CRs + app CRs + claims. Pure-ish (operates on already-fetched
/// JSON), kept private since it just shapes the manifest body.
fn resource_refs(crs: &[(&str, &Value)], claims: &[Value]) -> Vec<ResourceRef> {
    let mut refs = Vec::new();
    for (kind, cr) in crs {
        refs.push(ResourceRef {
            namespace: cr
                .pointer("/metadata/namespace")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            kind: (*kind).to_string(),
            name: cr
                .pointer("/metadata/name")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            claim_type: None,
        });
    }
    for c in claims {
        refs.push(ResourceRef {
            namespace: c
                .pointer("/metadata/namespace")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            kind: "ResourceClaim".to_string(),
            name: c
                .pointer("/metadata/name")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            claim_type: c
                .pointer("/spec/type")
                .and_then(Value::as_str)
                .map(str::to_string),
        });
    }
    refs
}

/// The CNPG operator's own namespace, where the lazily-provisioned shared
/// integrated `platform-postgres` Cluster lives (never in an app namespace).
const CNPG_OPERATOR_NS: &str = "cnpg-system";

/// The CNPG operand image of the first CNPG Cluster found across the app
/// namespaces AND `cnpg-system`, used to pick a major-matched `pg_dump` helper
/// image. Falls back to the default pg image when none is found.
///
/// The `cnpg-system` scan is load-bearing: integrated-tier claims all share the
/// `platform-postgres` Cluster there (see the resourceclaim-provisioner), which
/// never appears in an app namespace, so an app-ns-only scan structurally
/// misses it and always falls back to the default major. For each Cluster CR
/// the image is read from `spec.imageName` first and, when that is unset (CNPG
/// derives the operand image from its own default or an ImageCatalogRef — the
/// common case, so `spec.imageName` is typically EMPTY), from `status.image`
/// (the resolved operand image CNPG stamps once the Cluster is running).
/// Without the `status.image` fallback a modern CNPG (PG 18) would silently
/// mismatch a `postgres:16` `pg_dump` (`pg_dump: server version mismatch`).
pub(crate) fn first_cnpg_image(namespaces: &[String], kubeconfig: &Path) -> Option<String> {
    // App namespaces first (per-claim owned clusters, if any), then the shared
    // integrated cluster's namespace. Dedup so `cnpg-system` isn't scanned
    // twice when it is already an app namespace.
    let mut scan: Vec<&str> = namespaces.iter().map(String::as_str).collect();
    if !scan.contains(&CNPG_OPERATOR_NS) {
        scan.push(CNPG_OPERATOR_NS);
    }
    for ns in scan {
        if let Ok(items) = list_items("clusters.postgresql.cnpg.io", Some(ns), kubeconfig) {
            if let Some(img) = items.iter().find_map(cnpg_cluster_image) {
                return Some(img);
            }
        }
    }
    None
}

/// Resolve a CNPG `Cluster`'s operand image: `spec.imageName` when set, else the
/// resolved `status.image` (populated once the Cluster is running, even when the
/// image comes from a default/ImageCatalogRef rather than an explicit spec).
fn cnpg_cluster_image(c: &Value) -> Option<String> {
    c.pointer("/spec/imageName")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .or_else(|| {
            c.pointer("/status/image")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
        })
        .map(str::to_string)
}

/// Enumerate ResourceClaims across the given namespaces (cluster-wide when the
/// set is the whole cluster), flattened into a single `Vec`.
fn claims_in_namespaces(namespaces: &[String], kubeconfig: &Path) -> Result<Vec<Value>> {
    let mut all = Vec::new();
    for ns in namespaces {
        all.extend(list_items(
            "resourceclaims.apprafter.io",
            Some(ns),
            kubeconfig,
        )?);
    }
    Ok(all)
}

/// Resolve the default output directory for `export`: `<cwd>/apprafter-export`.
fn default_export_dir() -> PathBuf {
    std::env::current_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join("apprafter-export")
}

/// Default restic repo path for a target: `<config>/backups/<target>`.
fn default_backup_repo(target_name: &str) -> Result<PathBuf> {
    let root = cli_core::target::default_config_root()?;
    Ok(root.join("backups").join(target_name))
}

// ---------------------------------------------------------------------------
// Concrete KubeExec impl — subprocess kubectl
// ---------------------------------------------------------------------------

/// Maximum stderr lines to retain for error reporting.
const STDERR_CAPTURE_LIMIT: usize = 20;

/// Grace period after `child.wait()` to let the stderr-drainer thread flush.
const STDERR_FLUSH_GRACE_MS: u64 = 100;

/// CLI's concrete implementation of [`backup_core::KubeExec`]: shells out to
/// `kubectl` with `KUBECONFIG=<path>`.
pub(crate) struct KubectlExec {
    pub kubeconfig: PathBuf,
}

impl KubectlExec {
    pub(crate) fn new(kubeconfig: PathBuf) -> Self {
        Self { kubeconfig }
    }
}

/// Spawn a thread that drains `reader` to EOF, retaining the last
/// `STDERR_CAPTURE_LIMIT` lines in a shared buffer for error reporting.
fn spawn_capturing_drainer<R: Read + Send + 'static>(reader: R) -> Arc<Mutex<Vec<String>>> {
    let buf: Arc<Mutex<Vec<String>>> =
        Arc::new(Mutex::new(Vec::with_capacity(STDERR_CAPTURE_LIMIT)));
    let buf_clone = Arc::clone(&buf);
    thread::spawn(move || {
        let reader = BufReader::new(reader);
        for line in reader.lines() {
            let Ok(line) = line else { break };
            let mut guard = buf_clone.lock().unwrap();
            if guard.len() >= STDERR_CAPTURE_LIMIT {
                guard.remove(0);
            }
            guard.push(line);
        }
    });
    buf
}

fn format_exec_error(
    context: &str,
    status: std::process::ExitStatus,
    stderr_buf: &Arc<Mutex<Vec<String>>>,
) -> CliError {
    thread::sleep(Duration::from_millis(STDERR_FLUSH_GRACE_MS));
    let captured = stderr_buf.lock().unwrap();
    if captured.is_empty() {
        CliError::Other(format!(
            "{context}: kubectl exec exited with {status} and produced no stderr output"
        ))
    } else {
        let text = captured.join("\n");
        CliError::Other(format!(
            "{context}: kubectl exec exited with {status}.\nkubectl stderr:\n  {}",
            text.replace('\n', "\n  ")
        ))
    }
}

impl KubeExec for KubectlExec {
    fn apply_and_wait_pod_ready(&self, spec: &serde_json::Value) -> Result<()> {
        let name = spec["metadata"]["name"]
            .as_str()
            .ok_or_else(|| CliError::Other("pod spec missing metadata.name".into()))?;
        let ns = spec["metadata"]["namespace"]
            .as_str()
            .ok_or_else(|| CliError::Other("pod spec missing metadata.namespace".into()))?;

        let json_bytes = serde_json::to_vec(spec)
            .map_err(|e| CliError::Other(format!("serialize pod spec: {e}")))?;

        let mut apply_child = Command::new("kubectl")
            .args(["apply", "-f", "-", "-n", ns])
            .env("KUBECONFIG", &self.kubeconfig)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| CliError::Other(format!("spawn kubectl apply: {e}")))?;

        {
            let mut stdin = apply_child
                .stdin
                .take()
                .ok_or_else(|| CliError::Other("kubectl apply has no stdin".into()))?;
            stdin
                .write_all(&json_bytes)
                .map_err(|e| CliError::Other(format!("write pod spec to kubectl apply: {e}")))?;
        }

        let apply_stderr = apply_child
            .stderr
            .take()
            .ok_or_else(|| CliError::Other("kubectl apply has no stderr".into()))?;
        let apply_stderr_buf = spawn_capturing_drainer(apply_stderr);
        let apply_status = apply_child
            .wait()
            .map_err(|e| CliError::Other(format!("wait kubectl apply: {e}")))?;
        if !apply_status.success() {
            return Err(format_exec_error(
                "apply_and_wait_pod_ready(apply)",
                apply_status,
                &apply_stderr_buf,
            ));
        }

        let wait_status = Command::new("kubectl")
            .args([
                "wait",
                "--for=condition=Ready",
                &format!("pod/{name}"),
                "-n",
                ns,
                "--timeout=300s",
            ])
            .env("KUBECONFIG", &self.kubeconfig)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map_err(|e| CliError::Other(format!("spawn kubectl wait: {e}")))?;

        if wait_status.success() {
            Ok(())
        } else {
            Err(CliError::Other(format!(
                "pod {name} in {ns} did not reach Ready within 300s (kubectl wait exited {wait_status})"
            )))
        }
    }

    fn exec_stream_to_file(
        &self,
        pod: &str,
        ns: &str,
        argv: &[&str],
        out_path: &Path,
    ) -> Result<()> {
        let mut cmd = Command::new("kubectl");
        cmd.arg("exec")
            .arg(pod)
            .arg("-n")
            .arg(ns)
            .arg("--")
            .args(argv)
            .env("KUBECONFIG", &self.kubeconfig)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let mut child = cmd
            .spawn()
            .map_err(|e| CliError::Other(format!("spawn kubectl exec (stream-to-file): {e}")))?;

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| CliError::Other("kubectl exec has no stdout".into()))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| CliError::Other("kubectl exec has no stderr".into()))?;

        let stderr_buf = spawn_capturing_drainer(stderr);

        let mut out_file = std::fs::File::create(out_path).map_err(|e| {
            CliError::Other(format!("create output file {}: {e}", out_path.display()))
        })?;
        let mut reader = BufReader::new(stdout);
        io::copy(&mut reader, &mut out_file)
            .map_err(|e| CliError::Other(format!("copy kubectl exec stdout → file: {e}")))?;

        let status = child
            .wait()
            .map_err(|e| CliError::Other(format!("wait kubectl exec: {e}")))?;

        if status.success() {
            Ok(())
        } else {
            Err(format_exec_error(
                "exec_stream_to_file",
                status,
                &stderr_buf,
            ))
        }
    }

    fn exec_stream_from_file(
        &self,
        pod: &str,
        ns: &str,
        argv: &[&str],
        in_path: &Path,
    ) -> Result<()> {
        let mut cmd = Command::new("kubectl");
        cmd.arg("exec")
            .arg("-i")
            .arg(pod)
            .arg("-n")
            .arg(ns)
            .arg("--")
            .args(argv)
            .env("KUBECONFIG", &self.kubeconfig)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());

        let mut child = cmd
            .spawn()
            .map_err(|e| CliError::Other(format!("spawn kubectl exec (stream-from-file): {e}")))?;

        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| CliError::Other("kubectl exec has no stdin".into()))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| CliError::Other("kubectl exec has no stderr".into()))?;

        let stderr_buf = spawn_capturing_drainer(stderr);

        let mut in_file = std::fs::File::open(in_path)
            .map_err(|e| CliError::Other(format!("open input file {}: {e}", in_path.display())))?;
        match io::copy(&mut in_file, &mut stdin) {
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::BrokenPipe => {}
            Err(e) => {
                return Err(CliError::Other(format!(
                    "copy file → kubectl exec stdin: {e}"
                )));
            }
        }
        drop(stdin);

        let status = child
            .wait()
            .map_err(|e| CliError::Other(format!("wait kubectl exec: {e}")))?;

        if status.success() {
            Ok(())
        } else {
            Err(format_exec_error(
                "exec_stream_from_file",
                status,
                &stderr_buf,
            ))
        }
    }

    fn delete_pod_best_effort(&self, name: &str, ns: &str) {
        let _ = Command::new("kubectl")
            .args([
                "delete",
                "pod",
                name,
                "-n",
                ns,
                "--ignore-not-found",
                "--wait=false",
            ])
            .env("KUBECONFIG", &self.kubeconfig)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }

    fn get_secret_key(&self, secret: &str, ns: &str, key: &str) -> Result<String> {
        let out = Command::new("kubectl")
            .args([
                "get",
                "secret",
                secret,
                "-n",
                ns,
                "-o",
                &format!("jsonpath={{.data.{key}}}"),
            ])
            .env("KUBECONFIG", &self.kubeconfig)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .map_err(|e| CliError::Other(format!("spawn kubectl get secret: {e}")))?;

        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
            return Err(CliError::Other(format!(
                "kubectl get secret {secret} -n {ns} -o jsonpath={{.data.{key}}} \
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
                    "decode secret {secret}/{key} (value was not valid base64): {e}"
                ))
            })?;

        String::from_utf8(decoded)
            .map_err(|e| CliError::Other(format!("secret {secret}/{key} is not utf-8: {e}")))
    }

    fn get_json(&self, args: &[&str]) -> Result<Option<serde_json::Value>> {
        let mut c = Command::new("kubectl");
        c.args(args).env("KUBECONFIG", &self.kubeconfig);

        let out = c
            .output()
            .map_err(|e| CliError::Other(format!("spawn kubectl: {e}")))?;

        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            if stderr.contains("NotFound") || stderr.contains("not found") {
                return Ok(None);
            }
            return Err(CliError::Other(format!(
                "kubectl {:?} failed (exit {:?}): {stderr}",
                args.first().unwrap_or(&"?"),
                out.status.code()
            )));
        }

        let value: serde_json::Value = serde_json::from_slice(&out.stdout)
            .map_err(|e| CliError::Other(format!("kubectl JSON parse: {e}")))?;
        Ok(Some(value))
    }
}

// ---------------------------------------------------------------------------
// Concrete ResticRunner impl — subprocess restic
// ---------------------------------------------------------------------------

/// CLI's concrete implementation of [`backup_core::ResticRunner`]: shells out
/// to `restic` subprocess.
pub(crate) struct SubprocessRestic;

impl ResticRunner for SubprocessRestic {
    fn run(&self, argv: &[String], pass: &str) -> Result<()> {
        let out = Command::new("restic")
            .args(argv)
            .env("RESTIC_PASSWORD", pass)
            .output()
            .map_err(|e| CliError::Other(format!("spawn restic: {e}")))?;
        if !out.status.success() {
            return Err(CliError::Other(format!(
                "restic {} failed (exit {:?}): {}",
                argv.first().map(String::as_str).unwrap_or("?"),
                out.status.code(),
                String::from_utf8_lossy(&out.stderr)
            )));
        }
        Ok(())
    }

    fn run_stdout(&self, argv: &[String], pass: &str) -> Result<String> {
        let out = Command::new("restic")
            .args(argv)
            .env("RESTIC_PASSWORD", pass)
            .output()
            .map_err(|e| CliError::Other(format!("spawn restic: {e}")))?;
        if !out.status.success() {
            return Err(CliError::Other(format!(
                "restic {} failed (exit {:?}): {}",
                argv.first().map(String::as_str).unwrap_or("?"),
                out.status.code(),
                String::from_utf8_lossy(&out.stderr)
            )));
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    fn run_backup(&self, argv: &[String], pass: &str) -> Result<Option<String>> {
        let stdout = self.run_stdout(argv, pass)?;
        let snapshot_id = stdout.lines().find_map(|line| {
            let obj: Value = serde_json::from_str(line.trim()).ok()?;
            if obj.pointer("/message_type").and_then(Value::as_str) == Some("summary") {
                obj.pointer("/snapshot_id")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            } else {
                None
            }
        });
        Ok(snapshot_id)
    }
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

/// `apprafter export` — pull native data (Kind 1) to a plain local folder.
///
/// Scope: the app-namespace set (whole cluster by default), narrowed by
/// `namespaces` when `select` is set. Writes `<out>/{pg,volumes,redis}/…`
/// plus a `<out>/manifest.json`. No CRs, no secrets, no encryption.
pub fn run_export(namespaces: &[String], select: bool, out: Option<&str>) -> Result<()> {
    let resolved = resolve_state_paths(None)?;
    let cluster_id = resolved.target_name.clone();
    let kc = ensure_kubeconfig_tempfile()?;

    let subset: &[String] = if select { namespaces } else { &[] };
    let apps = list_items("applications.apprafter.io", None, kc.path())?;
    let ns_set = app_namespaces(&apps, subset);
    if ns_set.is_empty() {
        return Err(CliError::Other(
            "no AppRafter Applications found — nothing to export. (Scope derives from \
             `kubectl get applications.apprafter.io -A`.)"
                .into(),
        ));
    }

    let out_dir = match out {
        Some(p) => PathBuf::from(p),
        None => default_export_dir(),
    };
    std::fs::create_dir_all(&out_dir)
        .map_err(|e| CliError::Other(format!("create export dir {}: {e}", out_dir.display())))?;

    let k = KubectlExec::new(kc.path().to_path_buf());
    let claims = claims_in_namespaces(&ns_set, kc.path())?;
    let plan = plan_extraction(&claims);
    let pg_image = pg_helper_image(first_cnpg_image(&ns_set, kc.path()).as_deref());
    run_extraction(&k, &plan, &out_dir, &pg_image)?;

    let platform_version = read_platform_version(kc.path())?;
    let manifest = BackupManifest {
        manifest_version: backup_core::manifest::MANIFEST_VERSION_CURRENT,
        cluster_id: cluster_id.clone(),
        created_at: now_rfc3339(),
        platform_version,
        namespaces: ns_set.clone(),
        resources: resource_refs(&[], &claims),
    };
    write_manifest(&manifest, &out_dir)?;

    println!(
        "✓ Exported {} namespace(s) from cluster '{cluster_id}' → {}",
        ns_set.len(),
        out_dir.display()
    );
    println!("  namespaces: {}", ns_set.join(", "));
    println!(
        "  claims:     {} ({} extractable)",
        claims.len(),
        plan.len()
    );
    Ok(())
}

/// `apprafter backup` — full encrypted backup (Kind 2): native extraction +
/// serialized config/app CRs + decrypted user secrets, wrapped into a restic
/// repository.
pub fn run_backup(
    namespaces: &[String],
    select: bool,
    repo: Option<&str>,
    passphrase: Option<&str>,
) -> Result<()> {
    let resolved = resolve_state_paths(None)?;
    let cluster_id = resolved.target_name.clone();

    let env_pass = std::env::var("RESTIC_PASSWORD").ok();
    let is_tty = std::io::stdin().is_terminal();
    let pass = backup_passphrase_or_error(passphrase, env_pass.as_deref(), is_tty)?;

    // Keep the kubeconfig tempfile alive for the WHOLE sequence (every kubectl
    // shell-out below depends on it; dropping it deletes the file).
    let kc = ensure_kubeconfig_tempfile()?;

    // Resolve ns_set BEFORE handing off to the engine (the engine's list_items
    // uses KubeExec which doesn't know about the "app-namespace set" concept —
    // that's a CLI-layer concern).
    let k = KubectlExec::new(kc.path().to_path_buf());
    let subset: &[String] = if select { namespaces } else { &[] };
    let apps = list_items("applications.apprafter.io", None, kc.path())?;
    let ns_set = app_namespaces(&apps, subset);
    if ns_set.is_empty() {
        return Err(CliError::Other(
            "no AppRafter Applications found — nothing to back up. (Scope derives from \
             `kubectl get applications.apprafter.io -A`.)"
                .into(),
        ));
    }

    let repo_path = match repo {
        Some(r) => PathBuf::from(r),
        None => default_backup_repo(&cluster_id)?,
    };
    let repo_str = repo_path.to_string_lossy().to_string();

    let pg_image = pg_helper_image(first_cnpg_image(&ns_set, kc.path()).as_deref());
    let platform_version = read_platform_version(kc.path())?;

    // Stage everything under a tempdir; the engine writes data/ under this root.
    let staging = tempfile::Builder::new()
        .prefix("apprafter-backup-")
        .tempdir()
        .map_err(|e| CliError::Other(format!("create staging dir: {e}")))?;

    let opts = BackupOpts {
        repo: repo_str.clone(),
        passphrase: pass,
        cluster_id: cluster_id.clone(),
        created_at: now_rfc3339(),
        platform_version,
        namespaces: ns_set.clone(),
        is_subset: select,
        staging_root: staging.path().to_path_buf(),
        pg_image,
        staging_mode: StagingMode::Monolithic,
        // CLI local-pull: keep the machine's hostname as the restic group
        // (correct per-operator-station grouping). The in-cluster runner
        // (chunk 2) will set Some("apprafter-backup") for a stable pod-agnostic
        // host (spec §Retention M-r3-1a).
        backup_host: None,
    };

    let r = SubprocessRestic;
    let summary = backup_core::engine::run_backup_with_summary(&k, &r, &opts)?;

    println!("✓ Backed up cluster '{cluster_id}' → {repo_str}");
    println!("  namespaces: {}", ns_set.join(", "));
    println!(
        "  captured:   {} CR(s), {} secret(s), {} claim(s) ({} extracted)",
        summary.cr_count, summary.secret_count, summary.claim_count, summary.extracted_count,
    );
    println!("  tag:        {}", summary.tag);
    if let Some(id) = summary.snapshot_id {
        println!("  snapshot:   {id}");
    }
    Ok(())
}

/// `apprafter backup list` — list the snapshots in a restic repo.
pub fn run_backup_list(repo: Option<&str>, passphrase: Option<&str>) -> Result<()> {
    let resolved = resolve_state_paths(None)?;
    let env_pass = std::env::var("RESTIC_PASSWORD").ok();
    let is_tty = std::io::stdin().is_terminal();
    let pass = backup_passphrase_or_error(passphrase, env_pass.as_deref(), is_tty)?;

    let repo_path = match repo {
        Some(r) => PathBuf::from(r),
        None => default_backup_repo(&resolved.target_name)?,
    };
    let repo_str = repo_path.to_string_lossy().to_string();

    let r = SubprocessRestic;
    let json = r.run_stdout(&restic_snapshots_argv(&repo_str), &pass)?;
    let snapshots: Value = serde_json::from_str(&json)
        .map_err(|e| CliError::Other(format!("parse restic snapshots JSON: {e}")))?;

    let arr = snapshots.as_array().cloned().unwrap_or_default();
    if arr.is_empty() {
        println!("No snapshots in {repo_str}.");
        return Ok(());
    }

    println!("Snapshots in {repo_str}:");
    println!("{:<12}  {:<25}  TAGS", "ID", "TIME");
    for s in &arr {
        let id = s
            .pointer("/short_id")
            .or_else(|| s.pointer("/id"))
            .and_then(Value::as_str)
            .map(|i| i.chars().take(8).collect::<String>())
            .unwrap_or_else(|| "?".to_string());
        let time = s.pointer("/time").and_then(Value::as_str).unwrap_or("?");
        let tags = s
            .pointer("/tags")
            .and_then(Value::as_array)
            .map(|t| {
                t.iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default();
        println!("{id:<12}  {time:<25}  {tags}");
    }
    Ok(())
}

fn write_manifest(manifest: &BackupManifest, dir: &Path) -> Result<()> {
    let body = serde_json::to_vec_pretty(manifest)
        .map_err(|e| CliError::Other(format!("serialize manifest: {e}")))?;
    std::fs::write(dir.join("manifest.json"), body)
        .map_err(|e| CliError::Other(format!("write manifest.json: {e}")))
}

/// Current time as an RFC3339 string (manifest `created_at` + tag timestamp).
fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339()
}

// ---------------------------------------------------------------------------
// Tests (pure helpers — the tested core)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn app_namespaces_derive_from_apprafter_applications_not_all_ns() {
        let apps = vec![
            json!({"metadata":{"name":"alpha","namespace":"demo"}}),
            json!({"metadata":{"name":"beta","namespace":"demo"}}),
            json!({"metadata":{"name":"shop","namespace":"prod"}}),
        ];
        assert_eq!(app_namespaces(&apps, &[]), vec!["demo", "prod"]);
        assert_eq!(app_namespaces(&apps, &["prod".to_string()]), vec!["prod"]);
    }

    #[test]
    fn backup_requires_passphrase() {
        assert!(backup_passphrase_or_error(None, None, false).is_err());
        assert!(backup_passphrase_or_error(Some("p"), None, false).is_ok());
    }

    #[test]
    fn cnpg_cluster_image_prefers_spec_then_status() {
        // Explicit spec.imageName wins.
        let spec = json!({"spec":{"imageName":"ghcr.io/cloudnative-pg/postgresql:16.2"}});
        assert_eq!(
            cnpg_cluster_image(&spec).as_deref(),
            Some("ghcr.io/cloudnative-pg/postgresql:16.2")
        );
        // Absent/empty spec.imageName falls back to the resolved status.image —
        // the integrated shared cluster path (CNPG derives PG 18 from its own
        // default, leaving spec.imageName empty). This is the regression: an
        // app-ns-only, spec-only lookup returned None → default postgres:16 →
        // `pg_dump: server version mismatch` against the PG 18 server.
        let status_only =
            json!({"spec":{},"status":{"image":"ghcr.io/cloudnative-pg/postgresql:18.3-1"}});
        assert_eq!(
            cnpg_cluster_image(&status_only).as_deref(),
            Some("ghcr.io/cloudnative-pg/postgresql:18.3-1")
        );
        let empty_spec = json!({"spec":{"imageName":""},"status":{"image":"postgres:17"}});
        assert_eq!(
            cnpg_cluster_image(&empty_spec).as_deref(),
            Some("postgres:17")
        );
        // The chosen image drives the helper major — 18.3-1 → postgres:18-alpine.
        assert_eq!(
            pg_helper_image(cnpg_cluster_image(&status_only).as_deref()),
            "postgres:18-alpine"
        );
        // Neither present → None (caller uses the pinned default).
        assert_eq!(cnpg_cluster_image(&json!({"spec":{}})), None);
    }

    #[test]
    fn sourcecred_material_refs_follow_git_and_registry() {
        // a SourceCredential CR with both git + registry sealedSecretRefs
        let sc = json!({"metadata":{"name":"ghcr","namespace":"apprafter-system"},
            "spec":{"git":{"backend":{"sealedSecretRef":{"name":"ghcr-git"}}},
                    "registry":{"backend":{"sealedSecretRef":{"name":"ghcr-reg"}}}}});
        let refs = sourcecred_material_refs(&sc);
        // each ref defaults ns to the CR's own namespace (apprafter-system)
        assert!(refs
            .iter()
            .any(|(ns, n)| ns == "apprafter-system" && n == "ghcr-git"));
        assert!(refs
            .iter()
            .any(|(ns, n)| ns == "apprafter-system" && n == "ghcr-reg"));
    }
}
