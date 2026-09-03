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

use age::x25519::Identity;
use cli_core::secrets::{decrypt_with_identity, default_age_key_path, load_or_create_identity};
use cli_core::{CliError, Result};
use cli_state::State;
use tempfile::NamedTempFile;

use crate::commands::state_paths::resolve_state_paths;

/// Build a classified [`CliError::Kubectl`] from a failed invocation.
///
/// One place, so every `kubectl` failure in this module gets the same
/// treatment: the stderr is classified (unreachable / forbidden / kind
/// not served) and the remedy for that class becomes the diagnostic's
/// help. Anything unrecognised carries an empty hint and renders
/// verbatim — a confident wrong classification would send the reader
/// somewhere else, which is worse than the raw text.
///
/// D11 / 2.22a. These were 39 spawn sites pasting raw stderr into the
/// catch-all; classifying at the choke points rather than at every call
/// site is the point — the entry is explicit that 584 is not a number to
/// drive to zero.
fn kubectl_error(verb: &str, resource: &str, exit: Option<i32>, stderr: &str) -> CliError {
    let hint = cli_core::diagnose::classify_kubectl(stderr)
        .hint()
        .unwrap_or_default()
        .to_string();
    CliError::Kubectl {
        verb: verb.to_string(),
        resource: resource.to_string(),
        exit,
        stderr: stderr.to_string(),
        hint,
    }
}

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
    let kubeconfig = select_cached_kubeconfig(
        hetzner.kubeconfig_age.as_deref(),
        hetzner.kubeconfig_yaml.as_deref(),
        &identity,
    )?;
    write_kubeconfig_tempfile(&kubeconfig)
}

/// Pick the cached kubeconfig body out of the state's two slots, decrypting
/// the age-armored one when present. Extracted from
/// [`ensure_kubeconfig_tempfile_for_target`] (which is otherwise pure IO) so
/// the branch order is unit-testable without a state file on disk.
///
/// PRECEDENCE IS LOAD-BEARING: `kubeconfig_age` wins over `kubeconfig_yaml`.
/// The plaintext slot is a legacy/plaintext fallback that older states may
/// still carry alongside a freshly written encrypted one; preferring the
/// plaintext there would silently hand back a STALE kubeconfig for a cluster
/// that has since been re-provisioned. Neither slot set is a hard error with
/// the `apprafter kubeconfig` remedy — never an empty config.
fn select_cached_kubeconfig(
    kubeconfig_age: Option<&str>,
    kubeconfig_yaml: Option<&str>,
    identity: &Identity,
) -> Result<String> {
    if let Some(armored) = kubeconfig_age {
        decrypt_with_identity(armored, identity)
    } else if let Some(plain) = kubeconfig_yaml {
        Ok(plain.to_string())
    } else {
        Err(CliError::Other(
            "no cached kubeconfig in state; run `apprafter kubeconfig` first".to_string(),
        ))
    }
}

/// Write `kubeconfig` to a fresh `NamedTempFile`. Extracted from
/// [`ensure_kubeconfig_tempfile_for_target`] so the round-trip (the bytes
/// `kubectl` will actually read through `KUBECONFIG`) is testable.
fn write_kubeconfig_tempfile(kubeconfig: &str) -> Result<NamedTempFile> {
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
    kubectl_get_json_inner(resource, name, namespace, kubeconfig_path, false)
}

/// Same as [`kubectl_get_json`], but asks for `metadata.managedFields`.
///
/// **kubectl STRIPS `managedFields` from `get -o json` by default** (it has
/// since 1.21, to keep output readable). Any caller that inspects field
/// ownership therefore sees an empty list and concludes nobody owns anything
/// — so an ownership guard built on the plain getter cannot fire, ever. Two
/// shipped guards were dead for exactly this reason before it was found on a
/// live cluster; both now call this.
pub fn kubectl_get_json_showing_managed_fields(
    resource: &str,
    name: Option<&str>,
    namespace: Option<&str>,
    kubeconfig_path: &Path,
) -> Result<Option<serde_json::Value>> {
    kubectl_get_json_inner(resource, name, namespace, kubeconfig_path, true)
}

/// Build the args for `kubectl get <resource> [--show-managed-fields] [name]
/// [-n <ns>] -o json`. Extracted from [`kubectl_get_json_inner`] so the argv
/// shape — in particular that `--show-managed-fields` is present ONLY for the
/// ownership-inspecting getter — is unit-testable without spawning kubectl.
pub(crate) fn kubectl_get_args(
    resource: &str,
    name: Option<&str>,
    namespace: Option<&str>,
    show_managed_fields: bool,
) -> Vec<String> {
    let mut a = vec!["get".to_string(), resource.to_string()];
    if show_managed_fields {
        a.push("--show-managed-fields".to_string());
    }
    if let Some(n) = name {
        a.push(n.to_string());
    }
    if let Some(ns) = namespace {
        a.push("-n".to_string());
        a.push(ns.to_string());
    }
    a.push("-o".to_string());
    a.push("json".to_string());
    a
}

/// Turn a finished `kubectl get -o json` invocation into the caller-facing
/// result. Extracted (and shared by the single-object and cluster-wide
/// getters, which had byte-identical copies) so the two decisions that matter
/// are testable without a cluster:
///
/// 1. A 404 — recognised by `NotFound` / `not found` anywhere in stderr — is
///    `Ok(None)`, NOT an error: each caller decides whether absence is fatal.
/// 2. Any other non-zero exit becomes a CLASSIFIED [`CliError::Kubectl`], so
///    an unreachable apiserver or an RBAC refusal does not get swallowed as
///    "absent" and reported as a missing object.
fn interpret_get_json_output(
    resource: &str,
    success: bool,
    exit: Option<i32>,
    stdout: &[u8],
    stderr: &str,
) -> Result<Option<serde_json::Value>> {
    if !success {
        if stderr.contains("NotFound") || stderr.contains("not found") {
            return Ok(None);
        }
        return Err(kubectl_error("get", resource, exit, stderr));
    }
    let value: serde_json::Value = serde_json::from_slice(stdout)
        .map_err(|e| CliError::Other(format!("kubectl JSON parse: {e}")))?;
    Ok(Some(value))
}

fn kubectl_get_json_inner(
    resource: &str,
    name: Option<&str>,
    namespace: Option<&str>,
    kubeconfig_path: &Path,
    show_managed_fields: bool,
) -> Result<Option<serde_json::Value>> {
    let mut c = Command::new("kubectl");
    c.args(kubectl_get_args(
        resource,
        name,
        namespace,
        show_managed_fields,
    ))
    .env("KUBECONFIG", kubeconfig_path);

    let out = c
        .output()
        .map_err(|e| CliError::Other(format!("spawn kubectl: {e}")))?;

    interpret_get_json_output(
        resource,
        out.status.success(),
        out.status.code(),
        &out.stdout,
        &String::from_utf8_lossy(&out.stderr),
    )
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

    interpret_list_json_output(
        resource,
        selector,
        out.status.success(),
        out.status.code(),
        &out.stdout,
        &String::from_utf8_lossy(&out.stderr),
    )
}

/// Turn a finished `kubectl get … -l <selector> -o json` invocation into the
/// `.items[]` array. Extracted from [`kubectl_get_json_by_selector`] so the
/// two divergences from [`interpret_get_json_output`] are pinned:
///
/// 1. A selector list NEVER 404s — an empty match is a SUCCESSFUL list of zero
///    items. So a `not found` stderr here is a genuine failure (the KIND is
///    not served) and must surface as an error, not as "no items".
/// 2. A payload with no `items` key degrades to an empty `Vec` rather than
///    erroring, so a shape surprise cannot panic a listing command.
fn interpret_list_json_output(
    resource: &str,
    selector: &str,
    success: bool,
    exit: Option<i32>,
    stdout: &[u8],
    stderr: &str,
) -> Result<Vec<serde_json::Value>> {
    if !success {
        return Err(kubectl_error(
            "get",
            &format!("{resource} -l {selector}"),
            exit,
            stderr,
        ));
    }

    let value: serde_json::Value = serde_json::from_slice(stdout)
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

    interpret_get_json_output(
        resource,
        out.status.success(),
        out.status.code(),
        &out.stdout,
        &String::from_utf8_lossy(&out.stderr),
    )
}

/// Apply a manifest from a `serde_json::Value` via `kubectl apply -f <tempfile>`.
/// Serialises the value to a temporary JSON file and passes the path; the temp
/// file is removed when this function returns.  Simple client-side apply —
/// equivalent to `kubectl apply -f manifest.json`.  Use
/// `kubectl_apply_server_side` when SSA field-manager ownership is required.
pub fn kubectl_apply_json(manifest: &serde_json::Value, kubeconfig_path: &Path) -> Result<()> {
    let file = write_apply_tempfile(manifest)?;

    let out = Command::new("kubectl")
        .arg("apply")
        .arg("-f")
        .arg(file.path())
        .env("KUBECONFIG", kubeconfig_path)
        .output()
        .map_err(|e| CliError::Other(format!("spawn kubectl apply: {e}")))?;
    if !out.status.success() {
        return Err(kubectl_error(
            "apply",
            "manifest",
            out.status.code(),
            &String::from_utf8_lossy(&out.stderr),
        ));
    }
    Ok(())
}

/// Serialise `manifest` to a temporary `.json` file for `kubectl apply -f`.
/// Extracted from [`kubectl_apply_json`] so the bytes that actually reach the
/// apiserver are testable — the manifest must survive the round-trip intact,
/// which a silently-truncated or unflushed write would break.
fn write_apply_tempfile(manifest: &serde_json::Value) -> Result<NamedTempFile> {
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
    Ok(file)
}

/// Build the args for `kubectl delete <resource> <name> -n <ns>
/// --ignore-not-found`. Extracted so the `--ignore-not-found` flag — the whole
/// reason [`kubectl_delete`] is idempotent — is pinned without a cluster.
pub(crate) fn kubectl_delete_args(resource: &str, name: &str, namespace: &str) -> Vec<String> {
    vec![
        "delete".to_string(),
        resource.to_string(),
        name.to_string(),
        "-n".to_string(),
        namespace.to_string(),
        "--ignore-not-found".to_string(),
    ]
}

/// The line [`kubectl_delete`] echoes for a delete, or `None` when kubectl
/// said nothing. `--ignore-not-found` prints NOTHING for an absent object, so
/// "empty stdout" is the signal that the delete was a no-op and there is
/// nothing to report. Extracted so that mapping is testable without stdout
/// capture.
fn delete_progress_line(stdout: &str) -> Option<&str> {
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    }
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
        .args(kubectl_delete_args(resource, name, namespace))
        .env("KUBECONFIG", kubeconfig_path)
        .output()
        .map_err(|e| CliError::Other(format!("spawn kubectl delete: {e}")))?;
    if !out.status.success() {
        return Err(kubectl_error(
            "delete",
            &format!("{resource}/{name}"),
            out.status.code(),
            &String::from_utf8_lossy(&out.stderr),
        ));
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    if let Some(line) = delete_progress_line(&stdout) {
        println!("  {line}");
    }
    Ok(())
}

/// Build the args for `kubectl patch <resource> <name> [-n <ns>]
/// [--subresource=<sub>] --type=merge -p <body>`. Extracted from
/// [`kubectl_merge_patch`] so two things are pinned without a cluster: the
/// `--subresource=` routing (a `status.phase` write MUST go through the
/// subresource endpoint or a `spec`-only webhook rejects it), and that both
/// `-n` and `--subresource` are omitted entirely when not asked for — a
/// cluster-scoped patch must not grow a namespace flag.
pub(crate) fn kubectl_merge_patch_args(
    resource: &str,
    name: &str,
    namespace: Option<&str>,
    subresource: Option<&str>,
    body_json: &str,
) -> Vec<String> {
    let mut a = vec!["patch".to_string(), resource.to_string(), name.to_string()];
    if let Some(ns) = namespace {
        a.push("-n".to_string());
        a.push(ns.to_string());
    }
    if let Some(sub) = subresource {
        a.push(format!("--subresource={sub}"));
    }
    a.push("--type=merge".to_string());
    a.push("-p".to_string());
    a.push(body_json.to_string());
    a
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
    c.args(kubectl_merge_patch_args(
        resource,
        name,
        namespace,
        subresource,
        body_json,
    ))
    .env("KUBECONFIG", kubeconfig_path);

    let out = c
        .output()
        .map_err(|e| CliError::Other(format!("spawn kubectl: {e}")))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(kubectl_error(
            "patch",
            &format!("{resource}/{name}"),
            out.status.code(),
            &stderr,
        ));
    }
    Ok(())
}

/// Build the args for `kubectl apply --server-side
/// --field-manager=<fm> --force-conflicts -f -`. Extracted from
/// [`kubectl_apply_server_side`] so the three flags that make SSA ownership
/// work are pinned: `--server-side` (without it kubectl does a client-side
/// apply and records NO field ownership, so Argo CD self-heal reverts the
/// write), the supplied `--field-manager` (the owner name the ADR 0045 egress
/// write depends on), and `-f -` so the manifest is read from the stdin pipe
/// this function writes to.
pub(crate) fn kubectl_server_side_apply_args(field_manager: &str) -> Vec<String> {
    vec![
        "apply".to_string(),
        "--server-side".to_string(),
        format!("--field-manager={field_manager}"),
        "--force-conflicts".to_string(),
        "-f".to_string(),
        "-".to_string(),
    ]
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
        .args(kubectl_server_side_apply_args(field_manager))
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
        return Err(kubectl_error(
            "apply --server-side",
            "manifest",
            out.status.code(),
            &stderr,
        ));
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

    // ---- error classification -------------------------------------------

    /// Unwraps a [`CliError::Kubectl`], failing loudly on any other variant.
    fn kubectl_parts(err: CliError) -> (String, String, Option<i32>, String, String) {
        match err {
            CliError::Kubectl {
                verb,
                resource,
                exit,
                stderr,
                hint,
            } => (verb, resource, exit, stderr, hint),
            other => panic!("expected CliError::Kubectl, got {other:?}"),
        }
    }

    #[test]
    fn kubectl_error_carries_the_remedy_for_the_classified_failure() {
        // INVARIANT: the diagnostic's `help` is the remedy for THAT failure
        // class, not a generic one. An unreachable apiserver and an RBAC
        // refusal need different actions, so their hints must differ and each
        // must name its own remedy. This is the whole point of routing every
        // spawn site through one classifier (D11 / 2.22a).
        let (verb, resource, exit, stderr, unreachable_hint) = kubectl_parts(kubectl_error(
            "get",
            "pods",
            Some(1),
            "Unable to connect to the server: dial tcp 10.0.0.1:6443: i/o timeout",
        ));
        assert_eq!(verb, "get");
        assert_eq!(resource, "pods");
        assert_eq!(exit, Some(1));
        assert!(stderr.contains("i/o timeout"), "stderr verbatim: {stderr}");
        assert!(
            unreachable_hint.contains("6443"),
            "unreachable hint must point at the apiserver port: {unreachable_hint}"
        );

        let (.., forbidden_hint) = kubectl_parts(kubectl_error(
            "patch",
            "platformstack/apprafter",
            Some(1),
            "Error from server (Forbidden): platformstacks.apprafter.io is forbidden",
        ));
        assert!(
            forbidden_hint.contains("RBAC"),
            "forbidden hint must name the RBAC remedy: {forbidden_hint}"
        );
        assert_ne!(
            unreachable_hint, forbidden_hint,
            "the two failure classes must not share one hint"
        );
    }

    #[test]
    fn kubectl_error_leaves_an_unrecognised_stderr_hintless() {
        // INVARIANT: an unrecognised stderr gets an EMPTY hint and renders
        // verbatim. A confident wrong classification would send the reader
        // somewhere else, which is worse than the raw text.
        let (.., stderr, hint) = kubectl_parts(kubectl_error(
            "apply",
            "manifest",
            Some(1),
            "error: the ClusterQuota admission plugin ate your homework",
        ));
        assert!(
            hint.is_empty(),
            "unclassified failure must carry no hint: {hint}"
        );
        assert!(stderr.contains("ate your homework"), "{stderr}");
    }

    // ---- get argv --------------------------------------------------------

    #[test]
    fn get_args_ask_for_managed_fields_only_when_requested() {
        // INVARIANT: kubectl STRIPS metadata.managedFields from `get -o json`
        // unless `--show-managed-fields` is passed. Two shipped ownership
        // guards were dead because the flag was missing, so its presence on
        // the managed-fields getter (and ABSENCE on the plain one) is the
        // thing to pin.
        let plain = kubectl_get_args("secret", Some("creds"), Some("argocd"), false);
        assert!(
            !plain.iter().any(|a| a == "--show-managed-fields"),
            "the plain getter must not ask for managedFields: {plain:?}"
        );
        let owned = kubectl_get_args("secret", Some("creds"), Some("argocd"), true);
        assert!(
            owned.iter().any(|a| a == "--show-managed-fields"),
            "the ownership getter MUST ask for managedFields: {owned:?}"
        );
        // The flag goes before the object name, so kubectl parses it as a flag
        // rather than a second positional resource.
        let flag = owned
            .iter()
            .position(|a| a == "--show-managed-fields")
            .unwrap();
        let name = owned.iter().position(|a| a == "creds").unwrap();
        assert!(flag < name, "flag must precede the object name: {owned:?}");
    }

    #[test]
    fn get_args_omit_name_and_namespace_when_absent() {
        // A whole-kind get in the kubeconfig's default namespace: no stray
        // positional, no `-n`.
        let args = kubectl_get_args("nodes", None, None, false);
        assert_eq!(
            args,
            vec!["get", "nodes", "-o", "json"],
            "unqualified get must be exactly `get <kind> -o json`"
        );
    }

    // ---- get / list output interpretation --------------------------------

    #[test]
    fn get_output_maps_a_404_to_none_but_classifies_everything_else() {
        // INVARIANT: only a genuine 404 becomes `Ok(None)`. An unreachable
        // apiserver must NOT be reported as "the object is absent" — callers
        // treat `None` as "safe to create", so misreading a connection failure
        // as absence would drive a wrong write.
        let absent = interpret_get_json_output(
            "application",
            false,
            Some(1),
            b"",
            "Error from server (NotFound): applications.argoproj.io \"web\" not found",
        )
        .expect("a 404 is not an error");
        assert_eq!(absent, None);

        let unreachable = interpret_get_json_output(
            "application",
            false,
            Some(1),
            b"",
            "Unable to connect to the server: dial tcp 10.0.0.1:6443: connect: connection refused",
        );
        assert!(
            matches!(unreachable, Err(CliError::Kubectl { .. })),
            "an unreachable apiserver must surface as an error, not as absence: {unreachable:?}"
        );
    }

    #[test]
    fn get_output_parses_the_payload_on_success() {
        let v = interpret_get_json_output(
            "application",
            true,
            Some(0),
            br#"{"metadata":{"name":"web"},"spec":{"replicas":2}}"#,
            "",
        )
        .expect("valid JSON parses")
        .expect("success is Some");
        assert_eq!(v["metadata"]["name"], "web");
        assert_eq!(v["spec"]["replicas"], 2);

        // Truncated stdout is an error, never a silent empty object.
        assert!(interpret_get_json_output("application", true, Some(0), b"{\"a\":", "").is_err());
    }

    #[test]
    fn list_output_treats_not_found_as_a_failure_unlike_the_single_getter() {
        // INVARIANT (the divergence from `interpret_get_json_output`): a
        // selector list NEVER 404s — zero matches is a successful empty list.
        // So the very stderr the single-object getter flattens to `Ok(None)`
        // means something else here: the KIND is not served. It must NOT be
        // flattened into "no items", or a missing CRD reads as an empty
        // cluster and the caller happily reports nothing to clean up.
        let not_found =
            "Error from server (NotFound): the server could not find the requested resource";
        assert_eq!(
            interpret_get_json_output("sharedvolume.apprafter.io", false, Some(1), b"", not_found)
                .expect("the single getter flattens this to absence"),
            None
        );

        let err = interpret_list_json_output(
            "sharedvolume.apprafter.io",
            "apprafter.io/application=web",
            false,
            Some(1),
            b"",
            not_found,
        );
        assert!(
            matches!(err, Err(CliError::Kubectl { .. })),
            "the list path must NOT flatten a not-found stderr into zero items: {err:?}"
        );
        let (_, resource, ..) = kubectl_parts(err.unwrap_err());
        assert!(
            resource.contains("-l apprafter.io/application=web"),
            "the failure must name the selector that was listed: {resource}"
        );
    }

    #[test]
    fn list_output_extracts_items_and_degrades_a_shapeless_payload_to_empty() {
        let items = interpret_list_json_output(
            "application",
            "k=v",
            true,
            Some(0),
            br#"{"items":[{"metadata":{"name":"a"}},{"metadata":{"name":"b"}}]}"#,
            "",
        )
        .expect("valid list parses");
        assert_eq!(items.len(), 2);
        assert_eq!(items[1]["metadata"]["name"], "b");

        // A payload with no `items` key is an empty listing, not a panic.
        let none = interpret_list_json_output("application", "k=v", true, Some(0), b"{}", "")
            .expect("missing items key degrades");
        assert!(none.is_empty(), "{none:?}");
    }

    // ---- cached-kubeconfig resolution ------------------------------------

    #[test]
    fn cached_kubeconfig_prefers_the_age_slot_over_the_plaintext_one() {
        // INVARIANT: when both slots are populated the ENCRYPTED one wins.
        // Older states can still carry a stale plaintext body next to a
        // freshly written encrypted one; preferring the plaintext would hand
        // kubectl a kubeconfig for a cluster that no longer exists.
        use cli_core::secrets::encrypt_for_recipient;

        let identity = Identity::generate();
        let armored = encrypt_for_recipient(
            "apiVersion: v1\nclusters: [current]\n",
            &identity.to_public(),
        )
        .expect("encrypt");

        let picked = select_cached_kubeconfig(
            Some(&armored),
            Some("apiVersion: v1\nclusters: [stale]\n"),
            &identity,
        )
        .expect("age slot decrypts");
        assert!(
            picked.contains("current") && !picked.contains("stale"),
            "the encrypted slot must win over the plaintext one: {picked}"
        );
    }

    #[test]
    fn cached_kubeconfig_falls_back_to_plaintext_and_errors_when_neither_is_set() {
        let identity = Identity::generate();

        let plain = select_cached_kubeconfig(None, Some("apiVersion: v1\n"), &identity)
            .expect("plaintext fallback");
        assert_eq!(plain, "apiVersion: v1\n");

        // Neither slot: a hard error naming the remedy, NEVER an empty config
        // (an empty KUBECONFIG makes kubectl silently fall back to ~/.kube).
        let err = select_cached_kubeconfig(None, None, &identity).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("apprafter kubeconfig"),
            "the empty-state error must name the remedy: {msg}"
        );
    }

    #[test]
    fn cached_kubeconfig_rejects_ciphertext_for_a_different_identity() {
        // A key rotation must fail loudly rather than yielding garbage.
        use cli_core::secrets::encrypt_for_recipient;

        let owner = Identity::generate();
        let stranger = Identity::generate();
        let armored =
            encrypt_for_recipient("apiVersion: v1\n", &owner.to_public()).expect("encrypt");
        assert!(
            select_cached_kubeconfig(Some(&armored), None, &stranger).is_err(),
            "decrypting with the wrong identity must fail"
        );
    }

    #[test]
    fn kubeconfig_tempfile_holds_exactly_the_resolved_body() {
        // The tempfile IS what kubectl reads through KUBECONFIG, so a
        // truncated or re-encoded write would break every wrapped command.
        let body = "apiVersion: v1\nkind: Config\nclusters:\n- name: apprafter\n";
        let f = write_kubeconfig_tempfile(body).expect("write");
        let read = std::fs::read_to_string(f.path()).expect("read back");
        assert_eq!(read, body);
    }

    // ---- apply / delete / patch argv --------------------------------------

    #[test]
    fn apply_tempfile_round_trips_the_manifest() {
        let manifest = serde_json::json!({
            "apiVersion": "apprafter.io/v1alpha1",
            "kind": "Application",
            "metadata": {"name": "web", "namespace": "default"},
            "spec": {"base": {"image": "nginx:1", "replicas": 2}},
        });
        let f = write_apply_tempfile(&manifest).expect("write");
        assert!(
            f.path().extension().and_then(|e| e.to_str()) == Some("json"),
            "kubectl picks its parser off the extension: {:?}",
            f.path()
        );
        let read: serde_json::Value =
            serde_json::from_slice(&std::fs::read(f.path()).expect("read back")).expect("parse");
        assert_eq!(read, manifest, "the manifest must survive the round-trip");
    }

    #[test]
    fn delete_args_are_ignore_not_found() {
        // INVARIANT: `kubectl_delete` is documented idempotent, and the ONLY
        // thing that makes it so is this flag — without it a second delete
        // exits non-zero and the caller reports a failure for a no-op.
        let args = kubectl_delete_args("sealedsecret", "app-env", "web");
        assert_eq!(
            args,
            vec![
                "delete",
                "sealedsecret",
                "app-env",
                "-n",
                "web",
                "--ignore-not-found"
            ]
        );
    }

    #[test]
    fn delete_progress_line_is_silent_for_a_no_op_delete() {
        // `--ignore-not-found` prints nothing when the object was absent, so
        // empty stdout must not become a bare `  ` line in the output.
        assert_eq!(delete_progress_line(""), None);
        assert_eq!(delete_progress_line("   \n\n"), None);
        assert_eq!(
            delete_progress_line("secret \"app-env\" deleted\n"),
            Some("secret \"app-env\" deleted")
        );
    }

    #[test]
    fn merge_patch_args_route_through_the_subresource_endpoint() {
        // INVARIANT: a `status.phase` write MUST go through
        // `--subresource=status`; patched against the main endpoint a
        // spec-only admission webhook rejects it.
        let args = kubectl_merge_patch_args(
            "migrationplan",
            "upgrade-0-2-30",
            Some("apprafter-system"),
            Some("status"),
            r#"{"status":{"phase":"Approved"}}"#,
        );
        assert!(args.iter().any(|a| a == "--subresource=status"), "{args:?}");
        assert!(args.windows(2).any(|w| w == ["-n", "apprafter-system"]));
        assert!(args
            .windows(2)
            .any(|w| w == ["-p", r#"{"status":{"phase":"Approved"}}"#]));
        assert!(args.iter().any(|a| a == "--type=merge"), "{args:?}");
    }

    #[test]
    fn merge_patch_args_stay_bare_for_a_cluster_scoped_main_resource() {
        // No namespace and no subresource asked for ⇒ neither flag appears; a
        // stray `-n` on a cluster-scoped kind makes kubectl look in the wrong
        // scope and 404.
        let args =
            kubectl_merge_patch_args("platformstack", "apprafter", None, None, r#"{"spec":{}}"#);
        assert!(!args.iter().any(|a| a == "-n"), "{args:?}");
        assert!(
            !args.iter().any(|a| a.starts_with("--subresource")),
            "{args:?}"
        );
        assert_eq!(args[0], "patch");
        assert_eq!(args[1], "platformstack");
        assert_eq!(args[2], "apprafter");
    }

    #[test]
    fn server_side_apply_args_carry_ssa_ownership_and_read_stdin() {
        // INVARIANT (ADR 0045 §Decision #4): without `--server-side` kubectl
        // does a CLIENT-side apply that records no field ownership, and Argo
        // CD's next self-heal reverts the write. The field manager must be the
        // caller's, and `-f -` must read the piped manifest.
        let args = kubectl_server_side_apply_args("apprafter-cli-egress");
        assert!(args.iter().any(|a| a == "--server-side"), "{args:?}");
        assert!(
            args.iter()
                .any(|a| a == "--field-manager=apprafter-cli-egress"),
            "{args:?}"
        );
        assert!(args.iter().any(|a| a == "--force-conflicts"), "{args:?}");
        assert_eq!(
            &args[args.len() - 2..],
            ["-f", "-"],
            "the manifest must be read from stdin: {args:?}"
        );
    }
}
