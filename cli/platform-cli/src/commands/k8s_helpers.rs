// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Thin `kubectl` shellout helpers shared by the B.1.79 thin-
//! wrapper CLI subcommands (`apprafter platform …`, `apprafter
//! migration …`, `apprafter open …`). The CLI already depends
//! on `kubectl` being on PATH through other commands
//! (`argocd-password`, `cluster-bootstrap`), so spawning it
//! here keeps the wire format consistent and avoids pulling
//! in kube-rs's Tokio runtime for the synchronous CLI binary.

use std::io::Write;
use std::path::Path;
use std::process::Command;

use cli_core::secrets::{decrypt_with_identity, default_age_key_path, load_or_create_identity};
use cli_core::{CliError, Result};
use cli_state::{State, StatePaths};
use tempfile::NamedTempFile;

/// Decrypt the cached kubeconfig from state and write it to a
/// `NamedTempFile`. Callers MUST keep the returned file alive
/// for the duration of the `kubectl` invocation — when the
/// `NamedTempFile` drops, the file is deleted.
///
/// Centralises the boilerplate that
/// `commands::argocd_password` carries; B.1.79 commands reuse
/// it instead of re-implementing the chain three times.
pub fn ensure_kubeconfig_tempfile() -> Result<NamedTempFile> {
    let cwd = std::env::current_dir()?;
    let paths = StatePaths::for_root(&cwd);
    let state = State::load_or_default(&paths)?;
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
