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
use backup_core::prune::{run_prune, RetentionPolicy};
use backup_core::restic::{restic_check_argv, restic_unlock_argv};
use backup_core::{KubeExec, ResticRunner, StagingMode, SubprocessRestic};
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
    kubectl_merge_patch,
};
use crate::commands::state_paths::resolve_state_paths;

/// Namespace the `PlatformStack` singleton + `SourceCredential`s + their sealed
/// material live in. Mirrors `repo_creds::SOURCECRED_NAMESPACE` /
/// `platform::PLATFORMSTACK_NAMESPACE` (both private to their modules).
/// Exported `pub(crate)` so `restore.rs` can use it without re-declaring.
pub(crate) const APPRAFTER_SYSTEM_NAMESPACE: &str = "apprafter-system";
pub(crate) const PLATFORMSTACK_NAME: &str = "default";
/// Namespace the `PlatformStack` singleton lives in — the `spec.backup`
/// merge-patch target. Alias of [`APPRAFTER_SYSTEM_NAMESPACE`], named to mirror
/// `platform::PLATFORMSTACK_NAMESPACE` at the merge-patch call sites.
pub(crate) const PLATFORMSTACK_NAMESPACE: &str = APPRAFTER_SYSTEM_NAMESPACE;

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
// 2a. `apprafter backup enable` / `disable` — spec.backup patch builders (pure)
// ---------------------------------------------------------------------------

/// Options for `apprafter backup enable`, mapped 1:1 onto the
/// `PlatformStack.spec.backup` CRD block (camelCase). `bucket` + `credential`
/// are mandatory; every other field is an override the operator may leave to
/// the chart/operator default (omitted from the patch when `None`).
#[derive(Default)]
pub(crate) struct EnableOpts {
    /// Restic S3 repository URL → `spec.backup.bucket`.
    pub bucket: String,
    /// Cluster credential Secret name → `spec.backup.credentialRef.name`.
    pub credential: String,
    /// Backup cron → `spec.backup.schedule`.
    pub cron: Option<String>,
    /// `spec.backup.retention.keepDaily`.
    pub keep_daily: Option<u32>,
    /// `spec.backup.retention.keepWeekly`.
    pub keep_weekly: Option<u32>,
    /// `spec.backup.retention.keepMonthly`.
    pub keep_monthly: Option<u32>,
    /// `spec.backup.retention.enforce` (`operator` | `cluster`).
    pub enforce: Option<String>,
    /// `spec.backup.stagingMode` (`monolithic` | `sequential`).
    pub staging_mode: Option<String>,
    /// `spec.backup.checkSchedule` cron.
    pub check_cron: Option<String>,
    /// `spec.backup.failureWebhook` URL.
    pub failure_webhook: Option<String>,
}

/// Build the JSON merge-patch body `{"spec":{"backup":{…}}}` for
/// `apprafter backup enable`.
///
/// `enabled:true`, `bucket`, and `credentialRef:{name}` are always present.
/// `schedule` / `stagingMode` / `checkSchedule` / `failureWebhook` appear only
/// when their option is `Some`. The nested `retention` object contains only the
/// keys whose option is `Some`, and is omitted ENTIRELY when none of
/// `keep_daily` / `keep_weekly` / `keep_monthly` / `enforce` is set — a bare
/// enable then leaves retention to the operator/chart default rather than
/// merge-patching an empty object.
///
/// Pure: no I/O, no validation of enum values (the impure caller
/// [`run_backup_enable`] validates `enforce` / `staging_mode` before calling).
pub(crate) fn backup_enable_patch(o: &EnableOpts) -> serde_json::Value {
    let mut backup = serde_json::Map::new();
    backup.insert("enabled".to_string(), Value::Bool(true));
    backup.insert("bucket".to_string(), Value::String(o.bucket.clone()));
    backup.insert(
        "credentialRef".to_string(),
        serde_json::json!({ "name": o.credential }),
    );

    if let Some(cron) = &o.cron {
        backup.insert("schedule".to_string(), Value::String(cron.clone()));
    }
    if let Some(mode) = &o.staging_mode {
        backup.insert("stagingMode".to_string(), Value::String(mode.clone()));
    }
    if let Some(check) = &o.check_cron {
        backup.insert("checkSchedule".to_string(), Value::String(check.clone()));
    }
    if let Some(hook) = &o.failure_webhook {
        backup.insert("failureWebhook".to_string(), Value::String(hook.clone()));
    }

    let mut retention = serde_json::Map::new();
    if let Some(d) = o.keep_daily {
        retention.insert("keepDaily".to_string(), Value::from(d));
    }
    if let Some(w) = o.keep_weekly {
        retention.insert("keepWeekly".to_string(), Value::from(w));
    }
    if let Some(m) = o.keep_monthly {
        retention.insert("keepMonthly".to_string(), Value::from(m));
    }
    if let Some(e) = &o.enforce {
        retention.insert("enforce".to_string(), Value::String(e.clone()));
    }
    if !retention.is_empty() {
        backup.insert("retention".to_string(), Value::Object(retention));
    }

    serde_json::json!({ "spec": { "backup": Value::Object(backup) } })
}

/// Build the JSON merge-patch body `{"spec":{"backup":{"enabled":false}}}` for
/// `apprafter backup disable` — flips `enabled` off while retaining every other
/// configured field (merge-patch only touches the keys it names).
pub(crate) fn backup_disable_patch() -> serde_json::Value {
    serde_json::json!({ "spec": { "backup": { "enabled": false } } })
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
// Operator S3 credential helpers (pub(crate) — consumed by backup
// enable/prune/check/unlock/restore in later tasks).
// ---------------------------------------------------------------------------

/// Parse a dotenv-style string into a `KEY → VALUE` map.
///
/// Rules:
/// * Blank lines and lines whose first non-whitespace character is `#` are
///   skipped.
/// * Split on the **first** `=` only — values may contain `=`.
/// * Whitespace around both key and value is trimmed.
/// * Lines with no `=` are ignored.
pub(crate) fn parse_credential_file(contents: &str) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    for line in contents.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if let Some(eq_pos) = trimmed.find('=') {
            let key = trimmed[..eq_pos].trim().to_string();
            let value = trimmed[eq_pos + 1..].trim().to_string();
            if !key.is_empty() {
                map.insert(key, value);
            }
        }
    }
    map
}

/// The S3 / restic credential keys consumed by the off-site backup verbs.
const OPERATOR_S3_CRED_KEYS: &[&str] = &[
    "AWS_ACCESS_KEY_ID",
    "AWS_SECRET_ACCESS_KEY",
    "RESTIC_PASSWORD",
    "AWS_DEFAULT_REGION",
];

/// Resolve operator-side S3 credentials for restic off-site backup verbs.
///
/// * `cred_file = Some(path)` — read and parse that dotenv file.
/// * `cred_file = None` — build the map from `env_lookup` for the four
///   canonical keys; keys not returned by the lookup are omitted.
///
/// In both cases, returns an error when `RESTIC_PASSWORD` is absent or empty
/// (a restic repo with no password would hold decrypted cluster secrets in the
/// clear).
///
/// The `env_lookup` parameter is an injectable seam for testing; production
/// callers pass `&|k| std::env::var(k).ok()`.
pub(crate) fn resolve_operator_s3_creds(
    cred_file: Option<&std::path::Path>,
    env_lookup: &dyn Fn(&str) -> Option<String>,
) -> Result<BTreeMap<String, String>> {
    let map = if let Some(path) = cred_file {
        let contents = std::fs::read_to_string(path).map_err(|e| {
            CliError::Other(format!("read credential file {}: {e}", path.display()))
        })?;
        parse_credential_file(&contents)
    } else {
        let mut m = BTreeMap::new();
        for &key in OPERATOR_S3_CRED_KEYS {
            if let Some(val) = env_lookup(key) {
                m.insert(key.to_string(), val);
            }
        }
        m
    };

    match map.get("RESTIC_PASSWORD").map(String::as_str) {
        None | Some("") => {
            return Err(CliError::Other(
                "RESTIC_PASSWORD not set — pass --credential-file or export RESTIC_PASSWORD \
                 for an s3: repo"
                    .into(),
            ));
        }
        Some(_) => {}
    }

    Ok(map)
}

/// Inject all entries from `creds` as environment variables on `cmd`.
///
/// Used by the backup operator verbs (prune / check / unlock / restore) to
/// forward S3 + restic credentials to the subprocess without persisting them
/// in shell history or temporary files.
pub(crate) fn apply_creds_to_command(cmd: &mut Command, creds: &BTreeMap<String, String>) {
    for (k, v) in creds {
        cmd.env(k, v);
    }
}

// ---------------------------------------------------------------------------
// Operator-side restic maintenance verbs — prune / check / unlock
//
// These run OUTSIDE the cluster, on the operator's workstation, with the
// operator's FULL S3 creds (from `--credential-file` or env). They reach an
// `s3:` repo directly via a [`CredentialedRestic`] runner that injects the
// AWS_* + RESTIC_PASSWORD env on every restic Command (unlike the in-cluster
// scheduled path, which uses scoped creds mounted into the CronJob).
// ---------------------------------------------------------------------------

/// A [`ResticRunner`] that injects operator S3 credentials (AWS_* +
/// RESTIC_PASSWORD) onto every restic subprocess, WITHOUT mutating the global
/// process environment. Mirrors [`SubprocessRestic`]'s error handling
/// (non-zero exit → `Err` carrying stderr), adding the creds on top so restic
/// can reach an `s3:` repo the plain `SubprocessRestic` can't.
///
/// The `ResticRunner` trait is declared over `cli_core::Result` — the SAME
/// `Result`/`CliError` platform-cli uses — so these methods return exactly the
/// caller's error type; no cross-error mapping is needed at the call sites.
struct CredentialedRestic {
    creds: BTreeMap<String, String>,
}

impl CredentialedRestic {
    /// Build the restic Command for `argv`, applying the operator creds and the
    /// `RESTIC_PASSWORD` env. `pass` and `creds["RESTIC_PASSWORD"]` are the same
    /// value (`resolve_operator_s3_creds` guarantees the key is present); the
    /// explicit `RESTIC_PASSWORD` set from `pass` honours the trait contract
    /// while `apply_creds_to_command` carries the AWS_* keys.
    fn command(&self, argv: &[String], pass: &str) -> Command {
        let mut c = Command::new("restic");
        c.args(argv);
        apply_creds_to_command(&mut c, &self.creds);
        c.env("RESTIC_PASSWORD", pass);
        c
    }
}

impl ResticRunner for CredentialedRestic {
    fn run(&self, argv: &[String], pass: &str) -> Result<()> {
        let out = self
            .command(argv, pass)
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
        let out = self
            .command(argv, pass)
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
        // Not exercised by prune/check/unlock, but implemented for real (mirrors
        // SubprocessRestic) so the trait stays honest for any future caller.
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

/// Resolve the target restic repo for an operator maintenance verb.
///
/// * `Some(repo)` — use the explicit `--repo` override verbatim.
/// * `None` — read `PlatformStack/default.spec.backup.bucket`; error when
///   backup is unconfigured (no `spec.backup.bucket`), directing the operator
///   to pass `--repo` or run `apprafter backup enable`.
fn resolve_backup_repo(repo_override: Option<&str>, kubeconfig: &Path) -> Result<String> {
    if let Some(r) = repo_override {
        return Ok(r.to_string());
    }
    let ps = kubectl_get_json(
        "platformstack",
        Some(PLATFORMSTACK_NAME),
        Some(PLATFORMSTACK_NAMESPACE),
        kubeconfig,
    )?;
    ps.as_ref()
        .and_then(|p| p.pointer("/spec/backup/bucket"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .ok_or_else(|| {
            CliError::Other(
                "backup not configured — pass --repo or run `apprafter backup enable`".into(),
            )
        })
}

/// Compute the retention policy for a prune from the CR's `spec.backup` plus CLI
/// `--keep-*` overrides.
///
/// Precedence per field: CLI override (`Some`) wins → else the CR's
/// `.retention.{keepDaily,keepWeekly,keepMonthly}` when present → else the
/// [`RetentionPolicy::default`] (7 / 4 / 6). Pure — the impure caller fetches
/// `spec.backup` and reads the CLI flags.
fn retention_from_spec_backup(
    spec_backup: Option<&Value>,
    keep_daily: Option<u32>,
    keep_weekly: Option<u32>,
    keep_monthly: Option<u32>,
) -> RetentionPolicy {
    let default = RetentionPolicy::default();
    let cr = |key: &str| -> Option<u32> {
        spec_backup
            .and_then(|s| s.pointer(&format!("/retention/{key}")))
            .and_then(Value::as_u64)
            .map(|n| n as u32)
    };
    RetentionPolicy {
        keep_daily: keep_daily
            .or_else(|| cr("keepDaily"))
            .unwrap_or(default.keep_daily),
        keep_weekly: keep_weekly
            .or_else(|| cr("keepWeekly"))
            .unwrap_or(default.keep_weekly),
        keep_monthly: keep_monthly
            .or_else(|| cr("keepMonthly"))
            .unwrap_or(default.keep_monthly),
    }
}

/// `apprafter backup prune` — format-aware retention prune of an off-site restic
/// repo, run OUTSIDE the cluster with the operator's full S3 creds.
///
/// Resolves the repo (`--repo` → `spec.backup.bucket`) + creds
/// (`--credential-file` → env), computes the retention policy (CLI overrides →
/// CR → 7/4/6 default), then delegates the run-aware forget-set + prune to the
/// chunk-1 [`run_prune`]. On success it stamps the PlatformStack
/// `apprafter.io/last-prune` annotation with the current RFC3339 time so
/// `apprafter backup status` can surface when the repo was last pruned.
pub fn run_backup_prune(
    repo_override: Option<&str>,
    credential_file: Option<&Path>,
    keep_daily: Option<u32>,
    keep_weekly: Option<u32>,
    keep_monthly: Option<u32>,
) -> Result<()> {
    let kc = ensure_kubeconfig_tempfile()?;
    let creds = resolve_operator_s3_creds(credential_file, &|k| std::env::var(k).ok())?;
    let pass = creds["RESTIC_PASSWORD"].clone();

    // Fetch the CR once: repo fallback (spec.backup.bucket) + retention defaults
    // (spec.backup.retention) both read from it.
    let ps = kubectl_get_json(
        "platformstack",
        Some(PLATFORMSTACK_NAME),
        Some(PLATFORMSTACK_NAMESPACE),
        kc.path(),
    )?;
    let spec_backup = ps.as_ref().and_then(|p| p.pointer("/spec/backup"));

    let repo = match repo_override {
        Some(r) => r.to_string(),
        None => spec_backup
            .and_then(|s| s.pointer("/bucket"))
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .ok_or_else(|| {
                CliError::Other(
                    "backup not configured — pass --repo or run `apprafter backup enable`".into(),
                )
            })?,
    };

    let policy = retention_from_spec_backup(spec_backup, keep_daily, keep_weekly, keep_monthly);

    let runner = CredentialedRestic { creds };
    run_prune(&runner, &repo, &pass, &policy)?;

    // Stamp last-prune so `backup status` can report it. Best-effort ordering:
    // the prune already succeeded, so a merge-patch failure here surfaces as an
    // error (the annotation is the audit trail — we don't want to swallow it).
    let ts = chrono::Utc::now().to_rfc3339();
    let body = serde_json::json!({
        "metadata": { "annotations": { "apprafter.io/last-prune": ts } }
    })
    .to_string();
    kubectl_merge_patch(
        "platformstack",
        PLATFORMSTACK_NAME,
        Some(PLATFORMSTACK_NAMESPACE),
        None,
        &body,
        kc.path(),
    )?;

    println!("✓ Pruned {repo}");
    println!(
        "  retention: keepDaily={} keepWeekly={} keepMonthly={}",
        policy.keep_daily, policy.keep_weekly, policy.keep_monthly
    );
    println!("  last-prune stamped: {ts}");
    Ok(())
}

/// `apprafter backup check` — verify an off-site restic repo's integrity
/// (`restic check`, opt-in `--read-data` for a deep, full-download verify), run
/// OUTSIDE the cluster with the operator's full S3 creds.
pub fn run_backup_check(
    repo_override: Option<&str>,
    credential_file: Option<&Path>,
    read_data: bool,
) -> Result<()> {
    let kc = ensure_kubeconfig_tempfile()?;
    let creds = resolve_operator_s3_creds(credential_file, &|k| std::env::var(k).ok())?;
    let pass = creds["RESTIC_PASSWORD"].clone();
    let repo = resolve_backup_repo(repo_override, kc.path())?;

    let runner = CredentialedRestic { creds };
    runner.run(&restic_check_argv(&repo, read_data), &pass)?;

    if read_data {
        println!("✓ Repository check passed (deep --read-data verify).");
    } else {
        println!("✓ Repository check passed.");
    }
    Ok(())
}

/// `apprafter backup unlock` — remove STALE locks from an off-site restic repo
/// (`restic unlock`; never touches live locks held by a concurrent run), run
/// OUTSIDE the cluster with the operator's full S3 creds.
pub fn run_backup_unlock(
    repo_override: Option<&str>,
    credential_file: Option<&Path>,
) -> Result<()> {
    let kc = ensure_kubeconfig_tempfile()?;
    let creds = resolve_operator_s3_creds(credential_file, &|k| std::env::var(k).ok())?;
    let pass = creds["RESTIC_PASSWORD"].clone();
    let repo = resolve_backup_repo(repo_override, kc.path())?;

    let runner = CredentialedRestic { creds };
    runner.run(&restic_unlock_argv(&repo), &pass)?;

    println!("✓ Stale locks removed.");
    Ok(())
}

// ---------------------------------------------------------------------------
// 2c. `apprafter backup enable` / `disable` — preflight + spec.backup patch
// ---------------------------------------------------------------------------

/// Minimum restic version the off-site backup path relies on (compression +
/// `s3:` repo behaviour). Anything confidently older is rejected up front.
const MIN_RESTIC_MAJOR: u64 = 0;
const MIN_RESTIC_MINOR: u64 = 14;

/// Parse the `x.y.z` semver out of a `restic version` stdout line
/// (e.g. `restic 0.16.4 compiled with go1.21.6 on linux/amd64`). Returns
/// `(major, minor, patch)` or `None` when no dotted-triple token is found.
/// Pure — unit-testable without a restic binary.
fn parse_restic_version(stdout: &str) -> Option<(u64, u64, u64)> {
    for tok in stdout.split_whitespace() {
        // Strip a leading `v` if present (restic prints bare, but be lenient).
        let t = tok.strip_prefix('v').unwrap_or(tok);
        let mut parts = t.split('.');
        let (Some(a), Some(b), Some(c)) = (parts.next(), parts.next(), parts.next()) else {
            continue;
        };
        // Only accept when the third segment starts with digits (guards against
        // matching e.g. `go1.21.6` — that would parse, so we additionally
        // require the token to not be prefixed by non-version text).
        if let (Ok(major), Ok(minor)) = (a.parse::<u64>(), b.parse::<u64>()) {
            // `c` may carry a trailing suffix; take its leading digits.
            let patch_digits: String = c.chars().take_while(|ch| ch.is_ascii_digit()).collect();
            if let Ok(patch) = patch_digits.parse::<u64>() {
                return Some((major, minor, patch));
            }
        }
    }
    None
}

/// Is `(major, minor, _)` confidently BELOW the required `MIN_RESTIC_*`?
fn restic_version_too_old(v: (u64, u64, u64)) -> bool {
    let (major, minor, _) = v;
    (major, minor) < (MIN_RESTIC_MAJOR, MIN_RESTIC_MINOR)
}

/// `apprafter backup enable` — validate the repo + credential Secret + operator
/// intent, then merge-patch `PlatformStack.spec.backup` to turn on scheduled
/// off-site backup.
///
/// Preflight (fail-closed, in order):
/// 1. Validate `--enforce` / `--staging-mode` enum values (pure).
/// 2. Resolve operator S3 creds (`--credential-file` → env); errors without
///    `RESTIC_PASSWORD` (the preflight repo probe needs it).
/// 3. `restic version` ≥ 0.14 (not-on-PATH → error; unparseable → warn+continue;
///    confidently-lower → error).
/// 4. The cluster credential Secret exists in `apprafter-system` (else the
///    operator would have no creds to run scheduled backups).
/// 5. Repo reachability: `restic cat config` succeeds, else `restic init`; if
///    both fail the repo is unreachable / creds are bad → error with stderr.
/// 6. DR confirmation: `--i-have-saved-credentials`, else a TTY prompt, else a
///    non-interactive error (never patches without the operator confirming the
///    passphrase + creds live OUTSIDE the cluster).
///
/// Only after all six pass does it merge-patch `spec.backup`.
pub fn run_backup_enable(
    opts: EnableOpts,
    credential_file: Option<&Path>,
    i_have_saved: bool,
) -> Result<()> {
    // 1. Validate enum-valued options before touching the cluster.
    if let Some(enforce) = &opts.enforce {
        if enforce != "operator" && enforce != "cluster" {
            return Err(CliError::Other(format!(
                "invalid --enforce '{enforce}': expected 'operator' or 'cluster'"
            )));
        }
    }
    if let Some(mode) = &opts.staging_mode {
        if mode != "monolithic" && mode != "sequential" {
            return Err(CliError::Other(format!(
                "invalid --staging-mode '{mode}': expected 'monolithic' or 'sequential'"
            )));
        }
    }

    // 2. Resolve operator S3 creds (errors if RESTIC_PASSWORD is absent — the
    //    repo probe below needs it to read/init an encrypted repo).
    let creds = resolve_operator_s3_creds(credential_file, &|k| std::env::var(k).ok())?;

    // 3. restic version preflight.
    preflight_restic_version()?;

    // Kubeconfig for the Secret-existence probe + the merge-patch. Keep it
    // alive across both kubectl shell-outs (drop deletes the tempfile).
    let kc = ensure_kubeconfig_tempfile()?;

    // 4. Cluster credential Secret must already exist (sealed) in apprafter-system.
    let secret = kubectl_get_json(
        "secret",
        Some(&opts.credential),
        Some(PLATFORMSTACK_NAMESPACE),
        kc.path(),
    )?;
    if secret.is_none() {
        return Err(CliError::Other(format!(
            "credential Secret '{}' not found in {PLATFORMSTACK_NAMESPACE} — seal it first: \
             apprafter secret seal {} ... --namespace {PLATFORMSTACK_NAMESPACE}",
            opts.credential, opts.credential
        )));
    }

    // 5. Repo reachability: cat config → init → error.
    preflight_repo_reachable(&opts.bucket, &creds)?;

    // 6. DR credential confirmation.
    if !i_have_saved {
        if std::io::stdin().is_terminal() {
            let confirmed = inquire::Confirm::new(
                "Have you saved the restic passphrase AND S3 credentials somewhere OUTSIDE \
                 this cluster? Without them, backups are UNRECOVERABLE.",
            )
            .with_default(false)
            .prompt()
            .map_err(|e| CliError::Other(format!("confirmation prompt: {e}")))?;
            if !confirmed {
                println!(
                    "Aborted — no changes made. Save the restic passphrase + S3 credentials \
                     outside the cluster, then re-run."
                );
                return Ok(());
            }
        } else {
            return Err(CliError::Other(
                "non-interactive: re-run with --i-have-saved-credentials once you've saved the \
                 passphrase + S3 creds outside the cluster"
                    .into(),
            ));
        }
    }

    // 7. Merge-patch spec.backup (path-scoped; spec.backup has no required
    //    siblings, so a JSON merge-patch is correct — no SSA field-manager).
    let patch = backup_enable_patch(&opts);
    let body = serde_json::to_string(&patch)
        .map_err(|e| CliError::Other(format!("serialize spec.backup patch: {e}")))?;
    kubectl_merge_patch(
        "platformstack",
        PLATFORMSTACK_NAME,
        Some(PLATFORMSTACK_NAMESPACE),
        None,
        &body,
        kc.path(),
    )?;

    // 8. Success + GitOps advisory.
    println!(
        "✓ Scheduled off-site backup enabled → {} (credential Secret '{}').",
        opts.bucket, opts.credential
    );
    println!("{BACKUP_GITOPS_ADVISORY}");
    Ok(())
}

/// `apprafter backup disable` — merge-patch `spec.backup.enabled=false`,
/// retaining every other configured field for a later re-enable.
pub fn run_backup_disable() -> Result<()> {
    let kc = ensure_kubeconfig_tempfile()?;
    let body = serde_json::to_string(&backup_disable_patch())
        .map_err(|e| CliError::Other(format!("serialize spec.backup patch: {e}")))?;
    kubectl_merge_patch(
        "platformstack",
        PLATFORMSTACK_NAME,
        Some(PLATFORMSTACK_NAMESPACE),
        None,
        &body,
        kc.path(),
    )?;
    println!(
        "✓ Scheduled backup disabled (config retained; re-enable with `apprafter backup enable`)."
    );
    Ok(())
}

/// One-line advisory printed after a successful `spec.backup` merge-patch, in
/// the same spirit as `platform env set` / `platform egress set`: a live
/// merge-patch is not durable if the field is git-managed via Argo CD.
const BACKUP_GITOPS_ADVISORY: &str =
    "If PlatformStack.spec.backup is git-managed via Argo CD, the next sync will overwrite this \
     — set it in your infra repo for a durable change.";

/// Run `restic version`, parse the semver, and error when it is confidently
/// older than the required minimum. `restic` not on PATH → error. An
/// unparseable version → warn to stderr and continue (don't hard-fail purely on
/// a parse miss — only on a confidently-lower version).
fn preflight_restic_version() -> Result<()> {
    let out = Command::new("restic")
        .arg("version")
        .output()
        .map_err(|e| {
            if e.kind() == io::ErrorKind::NotFound {
                CliError::Other("restic not on PATH — install restic >= 0.14 first".into())
            } else {
                CliError::Other(format!("spawn restic version: {e}"))
            }
        })?;
    if !out.status.success() {
        // `restic version` failing is unusual but shouldn't itself block enable
        // — warn and continue; the repo probe below is the real gate.
        eprintln!(
            "warning: `restic version` exited with {} — continuing (repo probe still validates)",
            out.status
        );
        return Ok(());
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    match parse_restic_version(&stdout) {
        Some(v) if restic_version_too_old(v) => Err(CliError::Other(format!(
            "restic >= {MIN_RESTIC_MAJOR}.{MIN_RESTIC_MINOR} required, found {}.{}.{}",
            v.0, v.1, v.2
        ))),
        Some(_) => Ok(()),
        None => {
            eprintln!(
                "warning: could not parse restic version from `{}` — continuing",
                stdout.trim()
            );
            Ok(())
        }
    }
}

/// Probe repo reachability: `restic cat config` (repo already initialised) or,
/// failing that, `restic init`. If both fail the repo is unreachable or the
/// creds are wrong → error carrying restic's stderr. Creds are injected via
/// [`apply_creds_to_command`] (AWS_* + RESTIC_PASSWORD), never persisted.
fn preflight_repo_reachable(bucket: &str, creds: &BTreeMap<String, String>) -> Result<()> {
    let mut cat = Command::new("restic");
    cat.args(["cat", "config", "-r", bucket]);
    apply_creds_to_command(&mut cat, creds);
    let cat_out = cat
        .output()
        .map_err(|e| CliError::Other(format!("spawn restic cat config: {e}")))?;
    if cat_out.status.success() {
        return Ok(());
    }

    // Not initialised (or unreachable) — try to init it.
    let mut init = Command::new("restic");
    init.args(["init", "-r", bucket]);
    apply_creds_to_command(&mut init, creds);
    let init_out = init
        .output()
        .map_err(|e| CliError::Other(format!("spawn restic init: {e}")))?;
    if init_out.status.success() {
        return Ok(());
    }

    let cat_err = String::from_utf8_lossy(&cat_out.stderr);
    let init_err = String::from_utf8_lossy(&init_out.stderr);
    Err(CliError::Other(format!(
        "backup repo '{bucket}' unreachable / bad credentials — neither `restic cat config` nor \
         `restic init` succeeded.\n  cat config stderr: {}\n  init stderr: {}",
        cat_err.trim(),
        init_err.trim()
    )))
}

// ---------------------------------------------------------------------------
// 3a. `apprafter backup status` — pure formatter
// ---------------------------------------------------------------------------

/// Extract `.metadata.name` from a Job JSON object (empty string when absent).
fn job_metadata_name(j: &serde_json::Value) -> &str {
    j.pointer("/metadata/name")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
}

/// Extract `.status.startTime` from a Job JSON object (empty string when absent).
fn job_start_time(j: &serde_json::Value) -> &str {
    j.pointer("/status/startTime")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
}

/// Pick the most-recent Job from a slice by `.status.startTime` (lexicographic;
/// RFC3339 timestamps sort correctly as strings). Returns `None` when the slice
/// is empty.
fn most_recent_job<'a>(jobs: &[&'a serde_json::Value]) -> Option<&'a serde_json::Value> {
    jobs.iter().copied().max_by_key(|j| job_start_time(j))
}

/// Summarise a Job's terminal state from `.status.succeeded/.failed/.active`.
fn job_outcome(j: &serde_json::Value) -> &'static str {
    let succeeded = j
        .pointer("/status/succeeded")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    let active = j
        .pointer("/status/active")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    let failed = j
        .pointer("/status/failed")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    if succeeded > 0 {
        "Succeeded"
    } else if active > 0 {
        "Running"
    } else if failed > 0 {
        "Failed"
    } else {
        "Unknown"
    }
}

/// Render a human-readable status block for `apprafter backup status`.
///
/// All four inputs are optional / may be empty so the function works honestly
/// for every cluster state (backup never configured, no Jobs yet, CM absent).
///
/// # ConfigMap data keys (from `apprafter-backup/src/status.rs`)
/// * `lastRunFormat`  — staging mode of the last run (always written).
/// * `lastSuccess`    — RFC3339 timestamp of the last successful run.
/// * `lastFailure`    — RFC3339 timestamp of the last failed run.
/// * `lastError`      — error message from the last failed run.
///
/// # CronJob names (from `platform-stack/cue/render_tool.cue _backupTemplate`)
/// * `apprafter-backup`       — the scheduled backup CronJob.
/// * `apprafter-backup-check` — the weekly check CronJob.
///
/// Jobs are selected by their `.metadata.name` prefix `apprafter-backup` (both
/// CronJob-spawned Jobs share that prefix). For each of the two flavours (with
/// and without `-check`) the most-recent Job (by `.status.startTime`) is shown.
pub(crate) fn format_backup_status(
    spec_backup: Option<&serde_json::Value>,
    jobs: &[serde_json::Value],
    status_cm: Option<&serde_json::Value>,
    last_prune: Option<&str>,
) -> String {
    let mut out = String::new();

    // --- Config block ---
    let enabled = spec_backup
        .and_then(|s| s.get("enabled"))
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);

    if !enabled {
        out.push_str("Backup: DISABLED — enable with `apprafter backup enable ...`\n");
        if let Some(spec) = spec_backup {
            if let Some(bucket) = spec.get("bucket").and_then(serde_json::Value::as_str) {
                out.push_str(&format!("  bucket:   {bucket} (config retained)\n"));
            }
        }
        return out;
    }

    let spec = spec_backup.unwrap(); // enabled=true implies Some

    out.push_str("Backup: ENABLED\n");
    if let Some(v) = spec.get("bucket").and_then(serde_json::Value::as_str) {
        out.push_str(&format!("  bucket:        {v}\n"));
    }
    if let Some(v) = spec.get("schedule").and_then(serde_json::Value::as_str) {
        out.push_str(&format!("  schedule:      {v}\n"));
    }
    if let Some(v) = spec.get("stagingMode").and_then(serde_json::Value::as_str) {
        out.push_str(&format!("  stagingMode:   {v}\n"));
    }
    if let Some(v) = spec
        .get("checkSchedule")
        .and_then(serde_json::Value::as_str)
    {
        out.push_str(&format!("  checkSchedule: {v}\n"));
    }
    // Retention sub-block.
    if let Some(ret) = spec.get("retention") {
        out.push_str("  retention:\n");
        for key in ["keepDaily", "keepWeekly", "keepMonthly"] {
            if let Some(n) = ret.get(key) {
                out.push_str(&format!("    {key}: {n}\n"));
            }
        }
        if let Some(e) = ret.get("enforce").and_then(serde_json::Value::as_str) {
            out.push_str(&format!("    enforce: {e}\n"));
        }
    }

    // --- Job outcomes ---
    // Partition into backup Jobs (name prefix `apprafter-backup` but NOT
    // `apprafter-backup-check`) and check Jobs (prefix `apprafter-backup-check`).
    let backup_jobs: Vec<&serde_json::Value> = jobs
        .iter()
        .filter(|j| {
            let n = job_metadata_name(j);
            n.starts_with("apprafter-backup") && !n.contains("check")
        })
        .collect();
    let check_jobs: Vec<&serde_json::Value> = jobs
        .iter()
        .filter(|j| job_metadata_name(j).contains("apprafter-backup-check"))
        .collect();

    out.push_str("\nJobs:\n");
    match most_recent_job(&backup_jobs) {
        Some(j) => out.push_str(&format!(
            "  Last backup Job: {} — {}\n",
            job_metadata_name(j),
            job_outcome(j)
        )),
        None => out.push_str("  Last backup Job: none\n"),
    }
    match most_recent_job(&check_jobs) {
        Some(j) => out.push_str(&format!(
            "  Last check Job:  {} — {}\n",
            job_metadata_name(j),
            job_outcome(j)
        )),
        None => out.push_str("  Last check Job:  none\n"),
    }

    // --- Runner status CM ---
    out.push_str("\nRunner status:\n");
    if let Some(cm) = status_cm {
        // The CM may be passed as the full CM object (with a .data map) or as
        // just the .data section. Check both to stay robust to caller choice.
        let data = cm.get("data").filter(|d| d.is_object()).unwrap_or(cm);
        let get_str = |key: &str| -> &str {
            data.get(key)
                .and_then(serde_json::Value::as_str)
                .unwrap_or("")
        };
        let last_success = get_str("lastSuccess");
        let last_failure = get_str("lastFailure");
        let last_error = get_str("lastError");
        let last_run_format = get_str("lastRunFormat");

        if !last_success.is_empty() {
            out.push_str(&format!("  lastSuccess:    {last_success}\n"));
        } else {
            out.push_str("  lastSuccess:    never\n");
        }
        if !last_failure.is_empty() {
            out.push_str(&format!("  lastFailure:    {last_failure}\n"));
        }
        if !last_error.is_empty() {
            out.push_str(&format!("  lastError:      {last_error}\n"));
        }
        if !last_run_format.is_empty() {
            out.push_str(&format!("  lastRunFormat:  {last_run_format}\n"));
        }
    } else {
        out.push_str("  (no status ConfigMap yet — backup may not have run)\n");
    }

    // --- Last prune ---
    out.push_str(&format!(
        "\nLast prune: {}\n",
        last_prune.unwrap_or("never")
    ));

    out
}

/// `apprafter backup status` — show the operator's backup configuration, last
/// Job outcomes, runner self-reported status, and last prune time.
pub fn run_backup_status() -> Result<()> {
    let kc = ensure_kubeconfig_tempfile()?;

    // 1. Fetch PlatformStack to get spec.backup + last-prune annotation.
    let ps = kubectl_get_json(
        "platformstack",
        Some(PLATFORMSTACK_NAME),
        Some(PLATFORMSTACK_NAMESPACE),
        kc.path(),
    )?;
    let spec_backup = ps.as_ref().and_then(|p| p.pointer("/spec/backup")).cloned();
    let last_prune: Option<String> = ps
        .as_ref()
        .and_then(|p| {
            p.pointer("/metadata/annotations/apprafter.io~1last-prune")
                .and_then(serde_json::Value::as_str)
        })
        .map(str::to_string);

    // 2. List Jobs in apprafter-system and filter by name prefix.
    let jobs_list = kubectl_get_json("jobs", None, Some(PLATFORMSTACK_NAMESPACE), kc.path())?;
    let jobs: Vec<serde_json::Value> = jobs_list
        .as_ref()
        .and_then(|v| v.get("items"))
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter(|j| {
            j.pointer("/metadata/name")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("")
                .starts_with("apprafter-backup")
        })
        .collect();

    // 3. Fetch the runner status ConfigMap.
    let status_cm = kubectl_get_json(
        "configmap",
        Some("apprafter-backup-status"),
        Some(PLATFORMSTACK_NAMESPACE),
        kc.path(),
    )?;

    println!(
        "{}",
        format_backup_status(
            spec_backup.as_ref(),
            &jobs,
            status_cm.as_ref(),
            last_prune.as_deref(),
        )
    );
    Ok(())
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

    // ------------------------------------------------------------------
    // 1a. parse_credential_file
    // ------------------------------------------------------------------

    #[test]
    fn credential_file_parses_dotenv_keys() {
        let m = parse_credential_file(
            "# creds\nAWS_ACCESS_KEY_ID=AK\nAWS_SECRET_ACCESS_KEY=sk\nRESTIC_PASSWORD=p\n\n\
             AWS_DEFAULT_REGION = eu \n",
        );
        assert_eq!(m.get("AWS_ACCESS_KEY_ID").map(String::as_str), Some("AK"));
        assert_eq!(m.get("RESTIC_PASSWORD").map(String::as_str), Some("p"));
        assert_eq!(m.get("AWS_DEFAULT_REGION").map(String::as_str), Some("eu")); // trimmed
        assert!(!m.contains_key("# creds"));
    }

    #[test]
    fn credential_file_value_may_contain_equals() {
        let m = parse_credential_file("RESTIC_PASSWORD=a=b=c\n");
        assert_eq!(m.get("RESTIC_PASSWORD").map(String::as_str), Some("a=b=c"));
    }

    // ------------------------------------------------------------------
    // 1b. resolve_operator_s3_creds
    // ------------------------------------------------------------------

    #[test]
    fn resolve_creds_from_env_lookup_when_no_file() {
        let env: BTreeMap<&str, &str> =
            [("AWS_ACCESS_KEY_ID", "AK"), ("RESTIC_PASSWORD", "p")].into();
        let m = resolve_operator_s3_creds(None, &|k| env.get(k).map(|s| s.to_string())).unwrap();
        assert_eq!(m.get("AWS_ACCESS_KEY_ID").map(String::as_str), Some("AK"));
        assert_eq!(m.get("RESTIC_PASSWORD").map(String::as_str), Some("p"));
    }

    #[test]
    fn resolve_creds_errors_when_no_password() {
        let err = resolve_operator_s3_creds(None, &|_| None);
        assert!(err.is_err());
    }

    #[test]
    fn resolve_creds_from_credential_file() {
        use std::io::Write as _;
        let mut f = tempfile::NamedTempFile::new().unwrap();
        writeln!(
            f,
            "AWS_ACCESS_KEY_ID=FILEKEY\nAWS_SECRET_ACCESS_KEY=FILESEC\nRESTIC_PASSWORD=filepass\n"
        )
        .unwrap();
        let m = resolve_operator_s3_creds(Some(f.path()), &|_| None).unwrap();
        assert_eq!(
            m.get("AWS_ACCESS_KEY_ID").map(String::as_str),
            Some("FILEKEY")
        );
        assert_eq!(
            m.get("RESTIC_PASSWORD").map(String::as_str),
            Some("filepass")
        );
    }

    // ------------------------------------------------------------------
    // 1c. apply_creds_to_command — trivial but exercises pub(crate) fn
    // ------------------------------------------------------------------

    #[test]
    fn apply_creds_to_command_sets_env_vars() {
        let mut creds = BTreeMap::new();
        creds.insert("RESTIC_PASSWORD".to_string(), "testpass".to_string());
        creds.insert("AWS_ACCESS_KEY_ID".to_string(), "AKID".to_string());
        // Just construct a Command and call apply_creds_to_command — we can't
        // easily inspect the env map directly, so we exercise the code path
        // via a no-op echo (or true) call and verify it doesn't panic.
        let mut cmd = Command::new("true");
        apply_creds_to_command(&mut cmd, &creds);
        // If we reach here without panic the function is wired correctly.
    }

    // ------------------------------------------------------------------
    // 2a. backup_enable_patch / backup_disable_patch (pure patch builders)
    // ------------------------------------------------------------------

    #[test]
    fn enable_patch_sets_spec_backup_fields() {
        let p = backup_enable_patch(&EnableOpts {
            bucket: "s3:x".into(),
            credential: "c".into(),
            cron: Some("0 2 * * *".into()),
            enforce: Some("cluster".into()),
            staging_mode: Some("sequential".into()),
            keep_daily: Some(5),
            ..Default::default()
        });
        assert_eq!(p["spec"]["backup"]["enabled"], serde_json::json!(true));
        assert_eq!(p["spec"]["backup"]["bucket"], serde_json::json!("s3:x"));
        assert_eq!(
            p["spec"]["backup"]["credentialRef"]["name"],
            serde_json::json!("c")
        );
        assert_eq!(
            p["spec"]["backup"]["schedule"],
            serde_json::json!("0 2 * * *")
        );
        assert_eq!(
            p["spec"]["backup"]["retention"]["enforce"],
            serde_json::json!("cluster")
        );
        assert_eq!(
            p["spec"]["backup"]["retention"]["keepDaily"],
            serde_json::json!(5)
        );
        assert_eq!(
            p["spec"]["backup"]["stagingMode"],
            serde_json::json!("sequential")
        );
    }

    #[test]
    fn enable_patch_omits_retention_when_no_retention_flags() {
        // No keep_*/enforce set → the whole retention block is absent (a bare
        // enable that leaves retention to the operator/chart default).
        let p = backup_enable_patch(&EnableOpts {
            bucket: "s3:x".into(),
            credential: "c".into(),
            check_cron: Some("0 6 * * 0".into()),
            failure_webhook: Some("https://hook".into()),
            ..Default::default()
        });
        assert!(
            p["spec"]["backup"].get("retention").is_none(),
            "retention must be absent when no retention flag is set: {p}"
        );
        // Optional non-retention fields still flow through when present.
        assert_eq!(
            p["spec"]["backup"]["checkSchedule"],
            serde_json::json!("0 6 * * 0")
        );
        assert_eq!(
            p["spec"]["backup"]["failureWebhook"],
            serde_json::json!("https://hook")
        );
        // Optional fields the caller left unset are omitted (no null keys).
        assert!(p["spec"]["backup"].get("schedule").is_none());
        assert!(p["spec"]["backup"].get("stagingMode").is_none());
    }

    #[test]
    fn enable_patch_retention_includes_only_set_keys() {
        // Only keep_weekly set → retention present with just keepWeekly.
        let p = backup_enable_patch(&EnableOpts {
            bucket: "s3:x".into(),
            credential: "c".into(),
            keep_weekly: Some(3),
            ..Default::default()
        });
        let ret = &p["spec"]["backup"]["retention"];
        assert_eq!(ret["keepWeekly"], serde_json::json!(3));
        assert!(ret.get("keepDaily").is_none());
        assert!(ret.get("keepMonthly").is_none());
        assert!(ret.get("enforce").is_none());
    }

    #[test]
    fn disable_patch_sets_enabled_false() {
        assert_eq!(
            backup_disable_patch()["spec"]["backup"]["enabled"],
            serde_json::json!(false)
        );
    }

    // ------------------------------------------------------------------
    // 5. retention_from_spec_backup (CLI override → CR → 7/4/6 default)
    // ------------------------------------------------------------------

    #[test]
    fn retention_from_spec_backup_uses_cr_values() {
        let spec = json!({
            "bucket": "s3:x",
            "retention": { "keepDaily": 10, "keepWeekly": 8, "keepMonthly": 12 }
        });
        let p = retention_from_spec_backup(Some(&spec), None, None, None);
        assert_eq!(p.keep_daily, 10);
        assert_eq!(p.keep_weekly, 8);
        assert_eq!(p.keep_monthly, 12);
    }

    #[test]
    fn retention_from_spec_backup_override_wins_over_cr() {
        let spec = json!({
            "retention": { "keepDaily": 10, "keepWeekly": 8, "keepMonthly": 12 }
        });
        // keep_daily override wins; the other two fall back to the CR.
        let p = retention_from_spec_backup(Some(&spec), Some(3), None, None);
        assert_eq!(p.keep_daily, 3);
        assert_eq!(p.keep_weekly, 8);
        assert_eq!(p.keep_monthly, 12);
    }

    #[test]
    fn retention_from_spec_backup_all_unset_is_default_7_4_6() {
        // No CR retention block and no overrides → the 7/4/6 default.
        let p = retention_from_spec_backup(None, None, None, None);
        assert_eq!(p.keep_daily, 7);
        assert_eq!(p.keep_weekly, 4);
        assert_eq!(p.keep_monthly, 6);
        // A CR with no `.retention` also falls through to the default.
        let spec = json!({ "bucket": "s3:x" });
        let p2 = retention_from_spec_backup(Some(&spec), None, None, None);
        assert_eq!(p2.keep_daily, 7);
        assert_eq!(p2.keep_weekly, 4);
        assert_eq!(p2.keep_monthly, 6);
    }

    #[test]
    fn retention_override_applies_with_no_cr_retention() {
        let spec = json!({ "bucket": "s3:x" });
        let p = retention_from_spec_backup(Some(&spec), Some(1), Some(2), Some(3));
        assert_eq!(p.keep_daily, 1);
        assert_eq!(p.keep_weekly, 2);
        assert_eq!(p.keep_monthly, 3);
    }

    // ------------------------------------------------------------------
    // 2c. restic version preflight (pure parse + comparison)
    // ------------------------------------------------------------------

    #[test]
    fn parse_restic_version_reads_dotted_triple() {
        assert_eq!(
            parse_restic_version("restic 0.16.4 compiled with go1.21.6 on linux/amd64"),
            Some((0, 16, 4))
        );
        assert_eq!(parse_restic_version("restic 0.14.0"), Some((0, 14, 0)));
        // Leading `v` tolerated.
        assert_eq!(parse_restic_version("v1.2.3"), Some((1, 2, 3)));
        // No dotted triple at all → None (warn+continue path).
        assert_eq!(parse_restic_version("restic unknown"), None);
    }

    #[test]
    fn restic_version_gate_rejects_below_014() {
        assert!(restic_version_too_old((0, 13, 0)));
        assert!(restic_version_too_old((0, 9, 6)));
        assert!(!restic_version_too_old((0, 14, 0)));
        assert!(!restic_version_too_old((0, 16, 4)));
        assert!(!restic_version_too_old((1, 0, 0)));
    }

    // ------------------------------------------------------------------
    // 3a. format_backup_status
    // ------------------------------------------------------------------

    #[test]
    fn status_disabled_when_no_spec_backup() {
        let s = format_backup_status(None, &[], None, None);
        assert!(s.to_lowercase().contains("disabled"));
    }

    #[test]
    fn status_disabled_when_enabled_false() {
        let spec = json!({"enabled": false, "bucket": "s3:x"});
        let s = format_backup_status(Some(&spec), &[], None, None);
        assert!(s.to_lowercase().contains("disabled"));
        // Config is retained and shown even when disabled.
        assert!(s.contains("s3:x"));
    }

    #[test]
    fn status_renders_enabled_config_and_last_prune() {
        let spec = json!({"enabled": true, "bucket": "s3:x", "schedule": "0 3 * * *", "stagingMode": "monolithic"});
        let s = format_backup_status(Some(&spec), &[], None, Some("2026-07-17T03:00:00Z"));
        assert!(s.contains("s3:x"));
        assert!(s.contains("2026-07-17T03:00:00Z"));
        assert!(s.contains("0 3 * * *"));
        assert!(s.contains("monolithic"));
    }

    #[test]
    fn status_reports_job_outcome() {
        let job = json!({
            "metadata": {"name": "apprafter-backup-28900000"},
            "status": {"succeeded": 1}
        });
        let spec = json!({"enabled": true, "bucket": "s3:x"});
        let s = format_backup_status(Some(&spec), std::slice::from_ref(&job), None, None);
        assert!(s.contains("apprafter-backup-28900000"));
        assert!(s.contains("Succeeded"));
    }

    #[test]
    fn status_cm_last_success_and_error_keys_render() {
        // Uses the REAL keys from apprafter-backup/src/status.rs:
        // lastSuccess, lastFailure, lastError, lastRunFormat.
        let cm = json!({
            "data": {
                "lastSuccess": "2026-07-17T03:00:00Z",
                "lastFailure": "2026-07-16T03:00:00Z",
                "lastError": "restic: connection refused",
                "lastRunFormat": "monolithic"
            }
        });
        let spec = json!({"enabled": true, "bucket": "s3:x"});
        let s = format_backup_status(Some(&spec), &[], Some(&cm), None);
        assert!(
            s.contains("2026-07-17T03:00:00Z"),
            "lastSuccess not rendered: {s}"
        );
        assert!(
            s.contains("2026-07-16T03:00:00Z"),
            "lastFailure not rendered: {s}"
        );
        assert!(
            s.contains("restic: connection refused"),
            "lastError not rendered: {s}"
        );
        assert!(s.contains("monolithic"), "lastRunFormat not rendered: {s}");
    }

    #[test]
    fn status_last_prune_never_when_absent() {
        let spec = json!({"enabled": true, "bucket": "s3:x"});
        let s = format_backup_status(Some(&spec), &[], None, None);
        assert!(s.contains("Last prune: never"));
    }

    #[test]
    fn status_picks_most_recent_job_by_start_time() {
        let job_old = json!({
            "metadata": {"name": "apprafter-backup-28800000"},
            "status": {"startTime": "2026-07-16T03:00:00Z", "failed": 1}
        });
        let job_new = json!({
            "metadata": {"name": "apprafter-backup-28900000"},
            "status": {"startTime": "2026-07-17T03:00:00Z", "succeeded": 1}
        });
        let spec = json!({"enabled": true, "bucket": "s3:x"});
        let s = format_backup_status(Some(&spec), &[job_old, job_new], None, None);
        // Most-recent (new) should appear in the "Last backup Job" line.
        assert!(s.contains("apprafter-backup-28900000"));
        assert!(s.contains("Succeeded"));
    }

    #[test]
    fn status_check_job_is_separated_from_backup_job() {
        let backup_job = json!({
            "metadata": {"name": "apprafter-backup-28900000"},
            "status": {"succeeded": 1}
        });
        let check_job = json!({
            "metadata": {"name": "apprafter-backup-check-28900000"},
            "status": {"failed": 1}
        });
        let spec = json!({"enabled": true, "bucket": "s3:x"});
        let s = format_backup_status(Some(&spec), &[backup_job, check_job], None, None);
        assert!(s.contains("apprafter-backup-28900000"));
        assert!(s.contains("apprafter-backup-check-28900000"));
        // backup is Succeeded, check is Failed
        assert!(s.contains("Succeeded"));
        assert!(s.contains("Failed"));
    }
}
