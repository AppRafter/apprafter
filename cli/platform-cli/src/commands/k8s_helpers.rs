// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Thin `kubectl` shellout helpers shared by the B.1.79 thin-
//! wrapper CLI subcommands (`apprafter platform …`, `apprafter
//! migration …`, `apprafter open …`, `apprafter app …`,
//! `apprafter repo creds …`). The CLI already depends on
//! `kubectl` being on PATH through other commands
//! (`argocd-password`, `cluster-bootstrap`), so spawning it
//! here keeps the wire format consistent and avoids pulling
//! in kube-rs's Tokio runtime for the synchronous CLI binary.
//!
//! ## State location — v0.1.154
//!
//! `ensure_kubeconfig_tempfile` reads the cached kubeconfig
//! from the per-target state directory
//! (`<config>/state/<active-target>/.apprafter/state.json`).
//! Up through v0.1.153 the lookup was anchored to cwd, which
//! broke the most common operator flow: `apprafter apply` from
//! the project root, then `apprafter app add` from
//! `landing/cms/` — the second invocation's cwd had its own
//! (empty) `.apprafter/` directory and the kubeconfig wasn't
//! found. Per-target state pins the cache to the deployment
//! target the operator selected with `apprafter target use`,
//! independent of cwd.
//!
//! These thin-wrappers don't expose a `--target <name>`
//! override of their own (yet — Track A.9 will add it
//! uniformly across the CLI). The helper therefore always
//! resolves the active target.

use std::io::Write;
use std::path::Path;
use std::process::Command;

use cli_core::secrets::{decrypt_with_identity, default_age_key_path, load_or_create_identity};
use cli_core::{CliError, Result};
use cli_state::State;
use tempfile::NamedTempFile;

use crate::commands::state_paths::resolve_state_paths;

/// Decrypt the cached kubeconfig from state and write it to a
/// `NamedTempFile`. Callers MUST keep the returned file alive
/// for the duration of the `kubectl` invocation — when the
/// `NamedTempFile` drops, the file is deleted.
///
/// Centralises the boilerplate that
/// `commands::argocd_password` carries; the thin-wrapper
/// commands reuse it instead of re-implementing the chain
/// across five modules.
pub fn ensure_kubeconfig_tempfile() -> Result<NamedTempFile> {
    ensure_kubeconfig_tempfile_for_target(None)
}

/// Like [`ensure_kubeconfig_tempfile`] but resolves the state of a SPECIFIC
/// target (the `--target <name>` override) rather than always the active one.
/// Used by `apprafter restore`, which replays a backup into a target cluster
/// that may not be the active deployment target. `None` resolves the active
/// target, identical to [`ensure_kubeconfig_tempfile`].
pub fn ensure_kubeconfig_tempfile_for_target(
    target_override: Option<&str>,
) -> Result<NamedTempFile> {
    let resolved = resolve_state_paths(target_override)?;
    let state = State::load_or_default(&resolved.paths)?;
    let hetzner = state.hetzner_cloud.clone().ok_or_else(|| {
        CliError::Other(
            "state has no hetzner_cloud section; run `apprafter apply` first".to_string(),
        )
    })?;
    let identity = load_or_create_identity(&default_age_key_path())?;
    let kubeconfig = if let Some(armored) = &hetzner.kubeconfig_age {
        decrypt_with_identity(armored, &identity)?
    } else if let Some(plain) = &hetzner.kubeconfig_yaml {
        plain.clone()
    } else {
        return Err(CliError::Other(
            "no cached kubeconfig in state; run `apprafter kubeconfig` first".to_string(),
        ));
    };

    let mut f = tempfile::Builder::new()
        .prefix("apprafter-kubeconfig-")
        .tempfile()
        .map_err(|e| CliError::Other(format!("create kubeconfig tempfile: {e}")))?;
    f.write_all(kubeconfig.as_bytes())
        .map_err(|e| CliError::Other(format!("write kubeconfig tempfile: {e}")))?;
    Ok(f)
}

/// Run `kubectl get -o json ...` and return the parsed JSON
/// value. Returns `Ok(None)` when the resource is 404 — each
/// caller decides whether absence is an error.
pub fn kubectl_get_json(
    resource: &str,
    name: Option<&str>,
    namespace: Option<&str>,
    kubeconfig_path: &Path,
) -> Result<Option<serde_json::Value>> {
    let mut c = Command::new("kubectl");
    c.arg("get").arg(resource);
    if let Some(n) = name {
        c.arg(n);
    }
    if let Some(ns) = namespace {
        c.arg("-n").arg(ns);
    }
    c.arg("-o").arg("json").env("KUBECONFIG", kubeconfig_path);

    let out = c
        .output()
        .map_err(|e| CliError::Other(format!("spawn kubectl: {e}")))?;

    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        if stderr.contains("NotFound") || stderr.contains("not found") {
            return Ok(None);
        }
        return Err(CliError::Other(format!(
            "kubectl get {resource} failed (exit {:?}): {stderr}",
            out.status.code()
        )));
    }

    let value: serde_json::Value = serde_json::from_slice(&out.stdout)
        .map_err(|e| CliError::Other(format!("kubectl JSON parse: {e}")))?;
    Ok(Some(value))
}

/// Build the args for `kubectl get <resource> -l <selector> -n <ns> -o json`.
/// Factored out so the arg shape is unit-testable without spawning kubectl.
/// When `namespace` is `None` the listing is cluster-wide (`-A`).
pub(crate) fn kubectl_list_args(
    resource: &str,
    selector: &str,
    namespace: Option<&str>,
) -> Vec<String> {
    let mut a = vec![
        "get".to_string(),
        resource.to_string(),
        "-l".to_string(),
        selector.to_string(),
    ];
    match namespace {
        Some(ns) => {
            a.push("-n".to_string());
            a.push(ns.to_string());
        }
        None => a.push("-A".to_string()),
    }
    a.push("-o".to_string());
    a.push("json".to_string());
    a
}

/// `kubectl get <resource> -l <selector> [-n ns | -A] -o json` → the
/// `.items[]` array. Mirrors `kubectl_get_json`'s spawn/error handling;
/// returns an empty `Vec` when the listing matches nothing (a successful
/// list of zero items, NOT a 404 — selector lists never 404).
pub(crate) fn kubectl_get_json_by_selector(
    resource: &str,
    selector: &str,
    namespace: Option<&str>,
    kubeconfig_path: &Path,
) -> Result<Vec<serde_json::Value>> {
    let mut c = Command::new("kubectl");
    c.args(kubectl_list_args(resource, selector, namespace))
        .env("KUBECONFIG", kubeconfig_path);

    let out = c
        .output()
        .map_err(|e| CliError::Other(format!("spawn kubectl: {e}")))?;

    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(CliError::Other(format!(
            "kubectl get {resource} -l {selector} failed (exit {:?}): {stderr}",
            out.status.code()
        )));
    }

    let value: serde_json::Value = serde_json::from_slice(&out.stdout)
        .map_err(|e| CliError::Other(format!("kubectl JSON parse: {e}")))?;
    Ok(value
        .get("items")
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default())
}

/// Build the args for `kubectl get <resource> [-n <ns> | -A] -o json`.
/// Factored out so the arg shape is unit-testable without spawning kubectl.
/// When `namespace` is `None` the listing is cluster-wide (`-A`).
pub(crate) fn kubectl_get_cluster_wide_args(
    resource: &str,
    namespace: Option<&str>,
) -> Vec<String> {
    let mut a = vec!["get".to_string(), resource.to_string()];
    match namespace {
        Some(ns) => {
            a.push("-n".to_string());
            a.push(ns.to_string());
        }
        None => a.push("-A".to_string()),
    }
    a.push("-o".to_string());
    a.push("json".to_string());
    a
}

/// Like [`kubectl_get_json`] but accepts `namespace: Option<&str>` and passes
/// `-A` (all-namespaces) when it is `None`, instead of falling through to the
/// kubeconfig default namespace.  Use this for listing verbs where the user
/// omitting `--namespace` should mean cluster-wide, not current-context ns.
pub(crate) fn kubectl_get_json_cluster_wide(
    resource: &str,
    namespace: Option<&str>,
    kubeconfig_path: &Path,
) -> Result<Option<serde_json::Value>> {
    let mut c = Command::new("kubectl");
    c.args(kubectl_get_cluster_wide_args(resource, namespace))
        .env("KUBECONFIG", kubeconfig_path);

    let out = c
        .output()
        .map_err(|e| CliError::Other(format!("spawn kubectl: {e}")))?;

    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        if stderr.contains("NotFound") || stderr.contains("not found") {
            return Ok(None);
        }
        return Err(CliError::Other(format!(
            "kubectl get {resource} failed (exit {:?}): {stderr}",
            out.status.code()
        )));
    }

    let value: serde_json::Value = serde_json::from_slice(&out.stdout)
        .map_err(|e| CliError::Other(format!("kubectl JSON parse: {e}")))?;
    Ok(Some(value))
}

/// Apply a manifest from a `serde_json::Value` via `kubectl apply -f <tempfile>`.
/// Serialises the value to a temporary JSON file and passes the path; the temp
/// file is removed when this function returns.  Simple client-side apply —
/// equivalent to `kubectl apply -f manifest.json`.  Use
/// `kubectl_apply_server_side` when SSA field-manager ownership is required.
pub fn kubectl_apply_json(manifest: &serde_json::Value, kubeconfig_path: &Path) -> Result<()> {
    use std::io::Write as _;

    let mut file = tempfile::Builder::new()
        .prefix("apprafter-apply-")
        .suffix(".json")
        .tempfile()
        .map_err(|e| CliError::Other(format!("create apply tempfile: {e}")))?;
    let body = serde_json::to_vec_pretty(manifest)
        .map_err(|e| CliError::Other(format!("serialise manifest: {e}")))?;
    file.write_all(&body)
        .map_err(|e| CliError::Other(format!("write apply tempfile: {e}")))?;
    file.flush()
        .map_err(|e| CliError::Other(format!("flush apply tempfile: {e}")))?;

    let out = Command::new("kubectl")
        .arg("apply")
        .arg("-f")
        .arg(file.path())
        .env("KUBECONFIG", kubeconfig_path)
        .output()
        .map_err(|e| CliError::Other(format!("spawn kubectl apply: {e}")))?;
    if !out.status.success() {
        return Err(CliError::Other(format!(
            "kubectl apply failed (exit {:?}): {}",
            out.status.code(),
            String::from_utf8_lossy(&out.stderr)
        )));
    }
    Ok(())
}

/// Run `kubectl delete <resource> <name> -n <namespace> --ignore-not-found`.
/// Idempotent — absent objects are a no-op, not an error.
pub fn kubectl_delete(
    resource: &str,
    name: &str,
    namespace: &str,
    kubeconfig_path: &Path,
) -> Result<()> {
    let out = Command::new("kubectl")
        .args([
            "delete",
            resource,
            name,
            "-n",
            namespace,
            "--ignore-not-found",
        ])
        .env("KUBECONFIG", kubeconfig_path)
        .output()
        .map_err(|e| CliError::Other(format!("spawn kubectl delete: {e}")))?;
    if !out.status.success() {
        return Err(CliError::Other(format!(
            "kubectl delete {resource}/{name} failed (exit {:?}): {}",
            out.status.code(),
            String::from_utf8_lossy(&out.stderr)
        )));
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    let trimmed = stdout.trim();
    if !trimmed.is_empty() {
        println!("  {trimmed}");
    }
    Ok(())
}

/// Run `kubectl patch ... --type=merge -p <body>` against a
/// namespaced or cluster-scoped resource. When `subresource`
/// is `Some(name)` (`status`, `scale`), the patch routes
/// through the `<resource>/<subresource>` endpoint — required
/// for `status.phase` writes that bypass `spec`-only webhook
/// rules.
pub fn kubectl_merge_patch(
    resource: &str,
    name: &str,
    namespace: Option<&str>,
    subresource: Option<&str>,
    body_json: &str,
    kubeconfig_path: &Path,
) -> Result<()> {
    let mut c = Command::new("kubectl");
    c.arg("patch").arg(resource).arg(name);
    if let Some(ns) = namespace {
        c.arg("-n").arg(ns);
    }
    if let Some(sub) = subresource {
        c.arg(format!("--subresource={sub}"));
    }
    c.arg("--type=merge")
        .arg("-p")
        .arg(body_json)
        .env("KUBECONFIG", kubeconfig_path);

    let out = c
        .output()
        .map_err(|e| CliError::Other(format!("spawn kubectl: {e}")))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(CliError::Other(format!(
            "kubectl patch {resource}/{name} failed (exit {:?}): {stderr}",
            out.status.code()
        )));
    }
    Ok(())
}

/// Apply a manifest via **server-side apply** with the supplied
/// field manager, piping the YAML on stdin
/// (`kubectl apply --server-side --field-manager=<fm>
/// --force-conflicts -f -`). Unlike `kubectl_merge_patch`, SSA
/// tracks ownership in `metadata.managedFields`: a field whose
/// path no co-owner declares (e.g.
/// `spec.network.egress.profile`, which the platform-stack chart
/// does not template) persists across Argo CD self-heal because
/// there is no conflicting owner to revert it. `--force-conflicts`
/// migrates a pre-existing client-side-owned object cleanly to
/// the new field manager. Used by `apprafter platform egress set`
/// (ADR 0045 §Decision #4).
pub fn kubectl_apply_server_side(
    manifest_yaml: &str,
    field_manager: &str,
    kubeconfig_path: &Path,
) -> Result<()> {
    use std::process::Stdio;

    let mut child = Command::new("kubectl")
        .arg("apply")
        .arg("--server-side")
        .arg(format!("--field-manager={field_manager}"))
        .arg("--force-conflicts")
        .arg("-f")
        .arg("-")
        .env("KUBECONFIG", kubeconfig_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| CliError::Other(format!("spawn kubectl apply --server-side: {e}")))?;

    child
        .stdin
        .take()
        .ok_or_else(|| CliError::Other("kubectl stdin unavailable".to_string()))?
        .write_all(manifest_yaml.as_bytes())
        .map_err(|e| CliError::Other(format!("write manifest to kubectl stdin: {e}")))?;

    let out = child
        .wait_with_output()
        .map_err(|e| CliError::Other(format!("wait for kubectl apply: {e}")))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(CliError::Other(format!(
            "kubectl apply --server-side failed (exit {:?}): {stderr}",
            out.status.code()
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_by_selector_builds_label_args() {
        let args = kubectl_list_args(
            "application.argoproj.io",
            "apprafter.io/application=web",
            Some("argocd"),
        );
        assert!(args
            .windows(2)
            .any(|w| w == ["-l", "apprafter.io/application=web"]));
        assert!(args.windows(2).any(|w| w == ["-n", "argocd"]));
        let all = kubectl_list_args("x", "y=z", None);
        assert!(all.iter().any(|a| a == "-A"));
    }

    #[test]
    fn cluster_wide_args_all_namespaces_when_none() {
        // When no namespace is given, -A must appear (cluster-wide listing).
        let args = kubectl_get_cluster_wide_args("sharedvolume.apprafter.io", None);
        assert!(args.iter().any(|a| a == "-A"), "expected -A flag: {args:?}");
        assert!(
            !args.iter().any(|a| a == "-n"),
            "-n must be absent: {args:?}"
        );
        assert!(
            args.iter().any(|a| a == "json"),
            "expected -o json: {args:?}"
        );
    }

    #[test]
    fn cluster_wide_args_namespaced_when_some() {
        // When a namespace is given, -n <ns> must appear and -A must not.
        let args =
            kubectl_get_cluster_wide_args("sharedvolume.apprafter.io", Some("apprafter-system"));
        assert!(
            args.windows(2).any(|w| w == ["-n", "apprafter-system"]),
            "expected -n apprafter-system: {args:?}"
        );
        assert!(
            !args.iter().any(|a| a == "-A"),
            "-A must be absent: {args:?}"
        );
    }
}
