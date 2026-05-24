// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! `apprafter app …` user-application management. Track
//! B.1.79a part 3.
//!
//! Surface:
//!
//! * `apprafter app add` — register a user Application. Detects
//!   git origin from cwd or accepts explicit URL, normalises to
//!   HTTPS, reachability-checks via `git ls-remote` (skippable),
//!   writes an Argo CD `Application` CR labeled
//!   `apprafter.io/managed-by: apprafter`.
//!
//! * `apprafter app list` — table of Applications scoped to the
//!   `apps` AppProject (default), filtered to apprafter-managed
//!   ones unless `--all-managed` flips.
//!
//! * `apprafter app status` — detail view (sync + health + source
//!   + destinations + pending MigrationPlans + recent history).
//!
//! * `apprafter app remove` — delete the Argo CD CR; Argo CD
//!   tears down child resources for us via owner-ref cascade.
//!   `--keep-data` strips destructive child prune.
//!
//! All paths shell out to `kubectl` through
//! `commands::k8s_helpers` — keeps the wire format consistent
//! with the `platform` / `migration` wrappers.

use std::io::{self, IsTerminal};
use std::path::Path;
use std::process::Command;

use cli_core::{CliError, Result};
use serde_json::{json, Value};
use tabled::{Table, Tabled};

use crate::commands::k8s_helpers::{
    ensure_kubeconfig_tempfile, kubectl_get_json, kubectl_merge_patch,
};

const ARGOCD_NAMESPACE: &str = "argocd";
const APPRAFTER_MANAGED_LABEL: &str = "apprafter.io/managed-by=apprafter";
const APPRAFTER_SOURCE_ANNOTATION: &str = "apprafter.io/source";

#[derive(Tabled)]
struct AppRow {
    #[tabled(rename = "NAME")]
    name: String,
    #[tabled(rename = "PROJECT")]
    project: String,
    #[tabled(rename = "REPO")]
    repo: String,
    #[tabled(rename = "REV")]
    revision: String,
    #[tabled(rename = "SYNC")]
    sync: String,
    #[tabled(rename = "HEALTH")]
    health: String,
}

#[allow(clippy::too_many_arguments)]
pub fn add(
    git_url: Option<String>,
    name: Option<String>,
    branch: Option<String>,
    path: &str,
    project: &str,
    remote: &str,
    no_ping: bool,
    no_interactive: bool,
) -> Result<()> {
    // Trigger the interactive wizard when stdin + stdout are
    // TTYs and the operator hasn't opted out via
    // `--no-interactive`. The wizard pre-fills every field
    // from the flags above plus cwd-detected git origin and
    // branch where available; the operator may accept defaults
    // or override per-field.
    let stdin_is_tty = io::stdin().is_terminal();
    let stdout_is_tty = std::io::stdout().is_terminal();
    if crate::commands::app_wizard::should_use_wizard(no_interactive, stdin_is_tty, stdout_is_tty) {
        return add_via_wizard(git_url, name, branch, path, project, remote, no_ping);
    }

    let (repo_url, derived_branch) = match git_url {
        Some(explicit) => (normalise_git_url(&explicit), None),
        None => detect_git_repo_for_cwd(remote)?,
    };

    let derived_name = name.unwrap_or_else(|| derive_app_name(&repo_url));
    validate_dns_1123(&derived_name)?;

    let target_revision = branch.or(derived_branch).unwrap_or_else(|| "main".into());

    if !no_ping {
        ensure_repo_reachable(&repo_url)?;
    }

    let kc = ensure_kubeconfig_tempfile()?;

    // Pre-flight: refuse if an Application with this name already
    // exists in the `argocd` namespace. Argo CD's apiserver
    // wouldn't allow a duplicate `metadata.name` anyway, but the
    // kubectl 409 message is cryptic for new users — give a
    // cleaner hint and an explicit pointer to `app status` /
    // `app remove` instead.
    let existing = kubectl_get_json(
        "application.argoproj.io",
        Some(&derived_name),
        Some(ARGOCD_NAMESPACE),
        kc.path(),
    )?;
    if existing.is_some() {
        return Err(CliError::Other(format!(
            "Application '{derived_name}' is already registered in namespace \
             {ARGOCD_NAMESPACE}. Run `apprafter app status {derived_name}` to inspect its \
             current state, `apprafter app remove {derived_name}` to cascade-delete it, \
             or pass a different `--name`."
        )));
    }

    let manifest =
        build_application_manifest(&derived_name, &repo_url, &target_revision, path, project);
    apply_application_manifest(&manifest, kc.path())?;

    println!("✓ Application '{derived_name}' registered in AppProject '{project}'.");
    println!("  Repo:     {repo_url}");
    println!("  Revision: {target_revision}");
    println!("  Path:     {path}");
    println!();
    println!("Argo CD will sync the workload within a reconcile cycle. State:");
    println!("  apprafter app status {derived_name}");
    Ok(())
}

/// Wizard entry point — gathers any missing field via inquire
/// prompts and re-dispatches to the non-interactive `add` with
/// `no_interactive=true` to avoid recursion. The flag values
/// above are passed through verbatim; cwd detection pre-fills
/// the wizard's Git URL and branch suggestions.
#[allow(clippy::too_many_arguments)]
fn add_via_wizard(
    git_url: Option<String>,
    name: Option<String>,
    branch: Option<String>,
    path: &str,
    project: &str,
    remote: &str,
    no_ping: bool,
) -> Result<()> {
    let detected_origin = crate::commands::app_wizard::detect_git_origin(remote);
    let detected_branch = crate::commands::app_wizard::detect_git_branch();
    let detected_path = crate::commands::app_wizard::detect_path_relative_to_repo_root();
    let inputs = crate::commands::app_wizard::WizardInputs {
        git_url,
        name,
        branch,
        path: Some(path.to_string()),
        project: Some(project.to_string()),
        detected_origin,
        detected_branch,
        detected_path,
    };
    let out = crate::commands::app_wizard::run(inputs)?;
    add(
        Some(out.git_url),
        Some(out.name),
        Some(out.branch),
        &out.path,
        &out.project,
        remote,
        no_ping,
        true, // no_interactive — prevent recursion into the wizard.
    )
}

pub fn list(project: &str, all_projects: bool, all_managed: bool) -> Result<()> {
    let kc = ensure_kubeconfig_tempfile()?;

    let mut cmd = Command::new("kubectl");
    cmd.arg("get")
        .arg("application.argoproj.io")
        .arg("-n")
        .arg(ARGOCD_NAMESPACE)
        .arg("-o")
        .arg("json")
        .env("KUBECONFIG", kc.path());
    if !all_managed {
        cmd.arg("-l").arg(APPRAFTER_MANAGED_LABEL);
    }

    let out = cmd
        .output()
        .map_err(|e| CliError::Other(format!("spawn kubectl: {e}")))?;
    if !out.status.success() {
        return Err(CliError::Other(format!(
            "kubectl get applications failed (exit {:?}): {}",
            out.status.code(),
            String::from_utf8_lossy(&out.stderr)
        )));
    }
    let parsed: Value = serde_json::from_slice(&out.stdout)
        .map_err(|e| CliError::Other(format!("kubectl JSON parse: {e}")))?;

    let items: Vec<Value> = parsed
        .get("items")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    let filtered: Vec<Value> = if all_projects {
        items
    } else {
        items
            .into_iter()
            .filter(|app| {
                app.pointer("/spec/project")
                    .and_then(Value::as_str)
                    .map(|p| p == project)
                    .unwrap_or(false)
            })
            .collect()
    };

    if filtered.is_empty() {
        if all_projects {
            println!("No apprafter-managed Applications in the cluster.");
        } else {
            println!("No apprafter-managed Applications in AppProject '{project}'.");
        }
        if !all_managed {
            println!(
                "Hint: try `--all-managed` to list Applications that were not registered \
                 through `apprafter app add`."
            );
        }
        return Ok(());
    }

    let rows: Vec<AppRow> = filtered.iter().map(app_row).collect();
    println!("{}", Table::new(&rows));
    Ok(())
}

pub fn status(name: &str) -> Result<()> {
    let kc = ensure_kubeconfig_tempfile()?;
    let app = kubectl_get_json(
        "application.argoproj.io",
        Some(name),
        Some(ARGOCD_NAMESPACE),
        kc.path(),
    )?
    .ok_or_else(|| {
        CliError::Other(format!(
            "Application '{name}' not found in namespace {ARGOCD_NAMESPACE}. Check \
             `apprafter app list` for the registered applications."
        ))
    })?;

    print_status(&app);
    Ok(())
}

/// `apprafter app logs <name>` — stream logs from the
/// workload pods of an apprafter-managed Application. Pure
/// shell-out to `kubectl logs`, scoped to the app's destination
/// namespace (read from the Application CR's
/// `spec.destination.namespace`).
///
/// Without `--pod`: aggregate via `-l <selector>` — Argo CD
/// stamps `app.kubernetes.io/instance: <app-name>` on every
/// child resource it manages (the documented standard label
/// the kubectl + helm + argo ecosystem agrees on), so
/// `-l app.kubernetes.io/instance=<name>` reaches all pods.
/// `--pod` overrides the selector with a direct pod name.
pub fn logs(
    name: &str,
    follow: bool,
    tail: i64,
    container: Option<String>,
    pod: Option<String>,
) -> Result<()> {
    let kc = ensure_kubeconfig_tempfile()?;

    let app = kubectl_get_json(
        "application.argoproj.io",
        Some(name),
        Some(ARGOCD_NAMESPACE),
        kc.path(),
    )?
    .ok_or_else(|| {
        CliError::Other(format!(
            "Application '{name}' not found in namespace {ARGOCD_NAMESPACE}. Run \
             `apprafter app list` for the registered applications."
        ))
    })?;

    let workload_ns = app
        .pointer("/spec/destination/namespace")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            CliError::Other(format!(
                "Application '{name}' does not carry `spec.destination.namespace` — no \
                 namespace to point kubectl logs at. The CR may have been created outside \
                 `apprafter app add`."
            ))
        })?;

    let target = build_kubectl_logs_target(name, pod.as_deref());
    let args = build_kubectl_logs_args(&target, workload_ns, follow, tail, container.as_deref());

    let status = Command::new("kubectl")
        .args(&args)
        .env("KUBECONFIG", kc.path())
        .status()
        .map_err(|e| CliError::Other(format!("spawn kubectl logs: {e}")))?;
    if !status.success() {
        return Err(CliError::Other(format!(
            "kubectl logs failed (exit {:?})",
            status.code()
        )));
    }
    Ok(())
}

/// `apprafter app rollback <name> [--to <rev>]` — patches
/// `spec.source.targetRevision` to the requested revision (or
/// the previous entry in `status.history` when `--to` is
/// omitted). Argo CD's automated sync picks up the change on
/// the next reconcile and rolls back the workload.
pub fn rollback(name: &str, to: Option<String>, yes: bool) -> Result<()> {
    let kc = ensure_kubeconfig_tempfile()?;
    let app = kubectl_get_json(
        "application.argoproj.io",
        Some(name),
        Some(ARGOCD_NAMESPACE),
        kc.path(),
    )?
    .ok_or_else(|| {
        CliError::Other(format!(
            "Application '{name}' not found in namespace {ARGOCD_NAMESPACE}."
        ))
    })?;

    let current_revision = app
        .pointer("/spec/source/targetRevision")
        .and_then(Value::as_str)
        .unwrap_or("?")
        .to_string();

    let target_revision = match to {
        Some(explicit) => explicit,
        None => pick_previous_revision(&app)?.to_string(),
    };

    if target_revision == current_revision {
        return Err(CliError::Other(format!(
            "Target revision '{target_revision}' matches the current \
             `spec.source.targetRevision` — rollback would be a no-op."
        )));
    }

    if !yes {
        if !io::stdin().is_terminal() {
            return Err(CliError::Other(
                "non-interactive shell — pass `--yes` to skip the confirmation prompt".into(),
            ));
        }
        println!(
            "Roll back Application '{name}' from revision '{current_revision}' to \
             '{target_revision}'?"
        );
        let confirmed = inquire::Confirm::new("Confirm?")
            .with_default(false)
            .prompt()
            .map_err(|e| CliError::Other(format!("confirmation prompt: {e}")))?;
        if !confirmed {
            println!("Cancelled.");
            return Ok(());
        }
    }

    let body = format!(r#"{{"spec":{{"source":{{"targetRevision":"{target_revision}"}}}}}}"#);
    kubectl_merge_patch(
        "application.argoproj.io",
        name,
        Some(ARGOCD_NAMESPACE),
        None,
        &body,
        kc.path(),
    )?;

    println!(
        "✓ Application '{name}' rolled back to revision '{target_revision}'. Argo CD will \
         sync the workload within a reconcile cycle."
    );
    Ok(())
}

pub fn remove(name: &str, yes: bool, keep_data: bool) -> Result<()> {
    let kc = ensure_kubeconfig_tempfile()?;

    // Pre-flight: ensure the Application exists. Otherwise
    // kubectl delete reports `applications.argoproj.io
    // "<name>" not found` with exit 1 — we can surface this as
    // a cleaner CLI message than raw kubectl output.
    let existing = kubectl_get_json(
        "application.argoproj.io",
        Some(name),
        Some(ARGOCD_NAMESPACE),
        kc.path(),
    )?;
    let app = existing.ok_or_else(|| {
        CliError::Other(format!(
            "Application '{name}' not found in namespace {ARGOCD_NAMESPACE}."
        ))
    })?;

    if !yes {
        // Interactive confirm: refuse silently without a TTY
        // when `--yes` was not passed. Symmetric with the
        // `apprafter target remove` ergonomics.
        if !io::stdin().is_terminal() {
            return Err(CliError::Other(
                "non-interactive shell — pass `--yes` to skip the confirmation prompt".into(),
            ));
        }
        let project = app
            .pointer("/spec/project")
            .and_then(Value::as_str)
            .unwrap_or("?");
        let repo = app
            .pointer("/spec/source/repoURL")
            .and_then(Value::as_str)
            .unwrap_or("?");
        println!("Delete Application '{name}' (project: {project}, repo: {repo})?");
        let confirmed = inquire::Confirm::new("Confirm?")
            .with_default(false)
            .prompt()
            .map_err(|e| CliError::Other(format!("confirmation prompt: {e}")))?;
        if !confirmed {
            println!("Cancelled.");
            return Ok(());
        }
    }

    if keep_data {
        // Strip destructive child-prune by flipping the
        // syncPolicy.automated.prune flag to false BEFORE
        // deleting the CR. Argo CD will tear down the
        // Application object itself, but child resources
        // (Deployments, PVCs, etc.) will be left behind — the
        // operator can re-attach later by re-registering the
        // Application against the same destination namespace.
        //
        // RFC 7396 merge-patch — leaves the rest of
        // `syncPolicy` untouched.
        kubectl_merge_patch(
            "application.argoproj.io",
            name,
            Some(ARGOCD_NAMESPACE),
            None,
            r#"{"spec":{"syncPolicy":{"automated":{"prune":false}}}}"#,
            kc.path(),
        )?;
    }

    // Cascading delete: Argo CD's `Application` CR owns its
    // child resources through ownerReferences only when
    // `syncPolicy.automated.prune: true`. With prune=false
    // (`--keep-data`), Argo CD will delete only the CR; child
    // resources stay around. Either way the kubectl invocation
    // is the same.
    let out = Command::new("kubectl")
        .arg("delete")
        .arg("application.argoproj.io")
        .arg(name)
        .arg("-n")
        .arg(ARGOCD_NAMESPACE)
        .env("KUBECONFIG", kc.path())
        .output()
        .map_err(|e| CliError::Other(format!("spawn kubectl delete: {e}")))?;
    if !out.status.success() {
        return Err(CliError::Other(format!(
            "kubectl delete failed (exit {:?}): {}",
            out.status.code(),
            String::from_utf8_lossy(&out.stderr)
        )));
    }

    if keep_data {
        println!(
            "✓ Application '{name}' deleted. Child resources (PVCs, ResourceClaims) preserved \
             — re-register with the same destination namespace to re-attach."
        );
    } else {
        println!("✓ Application '{name}' deleted. Argo CD will cascade-prune child resources.");
    }
    Ok(())
}

// =========================================================================
// PURE HELPERS — testable without kube::Client / git binary / network.
// =========================================================================

/// Normalise a git URL to the HTTPS form Argo CD prefers:
///
/// - `git@host:org/repo.git` → `https://host/org/repo`
/// - `ssh://git@host/org/repo.git` → `https://host/org/repo`
/// - `https://host/org/repo.git` → `https://host/org/repo`
/// - anything else returned verbatim (caller's responsibility)
///
/// Strips trailing `.git`. Keeps the URL human-readable since
/// it surfaces in `apprafter app list` output.
pub(crate) fn normalise_git_url(url: &str) -> String {
    let url = url.trim();
    let url = url.strip_suffix(".git").unwrap_or(url);
    // SCP-style `git@host:org/repo` — convert to HTTPS.
    if let Some(rest) = url.strip_prefix("git@") {
        if let Some((host, path)) = rest.split_once(':') {
            return format!("https://{host}/{path}");
        }
    }
    // `ssh://git@host/path` → strip `git@` userinfo + flip
    // scheme to https.
    if let Some(rest) = url.strip_prefix("ssh://") {
        let rest = rest.strip_prefix("git@").unwrap_or(rest);
        return format!("https://{rest}");
    }
    url.to_string()
}

/// Derive a sane Application name from a normalised repo URL.
/// `https://github.com/foo/my-app` → `my-app`. Strips trailing
/// `.git` defensively (in case the URL slipped through
/// normalisation).
pub(crate) fn derive_app_name(repo_url: &str) -> String {
    let stripped = repo_url.strip_suffix(".git").unwrap_or(repo_url);
    let last_segment = stripped.rsplit('/').next().unwrap_or("app");
    let cleaned: String = last_segment
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect();
    let trimmed = cleaned.trim_matches('-');
    if trimmed.is_empty() {
        "app".to_string()
    } else {
        trimmed.to_ascii_lowercase()
    }
}

/// Validate a derived name matches Argo CD's DNS-1123 label
/// constraint. Argo CD enforces this server-side; we catch it
/// client-side with a friendlier error.
fn validate_dns_1123(name: &str) -> Result<()> {
    if name.is_empty() || name.len() > 64 {
        return Err(CliError::Other(format!(
            "Application name '{name}' must be 1..63 DNS-1123 characters (lowercase, [a-z0-9-])."
        )));
    }
    let ok = name
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && !name.starts_with('-')
        && !name.ends_with('-');
    if !ok {
        return Err(CliError::Other(format!(
            "Application name '{name}' contains invalid characters; expected DNS-1123 \
             (lowercase [a-z0-9-], does not start or end with '-')."
        )));
    }
    Ok(())
}

/// Detect the git remote URL + current branch for the cwd.
/// Returns `(repo_url, Some(branch))`, or an error mapped to a
/// CLI-friendly message when the cwd is not a git repo or the
/// named remote does not exist.
fn detect_git_repo_for_cwd(remote: &str) -> Result<(String, Option<String>)> {
    let remote_out = Command::new("git")
        .args(["remote", "get-url", remote])
        .output()
        .map_err(|e| {
            CliError::Other(format!(
                "failed to run git remote get-url {remote}: {e}. Pass the git URL as an \
                 argument: `apprafter app add <git-url>`"
            ))
        })?;
    if !remote_out.status.success() {
        let stderr = String::from_utf8_lossy(&remote_out.stderr);
        return Err(CliError::Other(format!(
            "git remote get-url {remote} failed (exit {:?}): {stderr}\n\
             Run from a git repository or pass the URL explicitly via `apprafter app add <git-url>`.",
            remote_out.status.code()
        )));
    }
    let raw_url = String::from_utf8_lossy(&remote_out.stdout)
        .trim()
        .to_string();
    if raw_url.is_empty() {
        return Err(CliError::Other(format!(
            "git remote {remote} returned an empty URL"
        )));
    }
    let url = normalise_git_url(&raw_url);

    let branch_out = Command::new("git")
        .args(["symbolic-ref", "--short", "HEAD"])
        .output();
    let branch = match branch_out {
        Ok(o) if o.status.success() => Some(String::from_utf8_lossy(&o.stdout).trim().to_string()),
        _ => None,
    };
    Ok((url, branch))
}

/// `git ls-remote` reachability check. Returns `Ok(())` when
/// the remote responds (HEAD listed), `Err` with an auth-hint
/// when `git` reports authentication failure. Fail-quiet on a
/// missing `git` binary — but errors out so the caller can
/// suggest `--no-ping`.
fn ensure_repo_reachable(repo_url: &str) -> Result<()> {
    let out = Command::new("git")
        .args(["ls-remote", "--exit-code", repo_url, "HEAD"])
        .output()
        .map_err(|e| {
            CliError::Other(format!(
                "failed to run `git ls-remote {repo_url}`: {e}. Pass `--no-ping` to skip \
                 the reachability check."
            ))
        })?;
    if out.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&out.stderr);
    let lower = stderr.to_lowercase();
    if lower.contains("authentication") || lower.contains("permission denied") {
        return Err(CliError::Other(format!(
            "git ls-remote refused access to {repo_url}.\n\
             {stderr}\n\
             Register creds via `apprafter repo creds add` and retry `apprafter app add`."
        )));
    }
    Err(CliError::Other(format!(
        "git ls-remote {repo_url} failed (exit {:?}): {stderr}",
        out.status.code()
    )))
}

/// Build `kubectl logs` arg vector. Pure fn — tests cover
/// flag combinations exhaustively.
pub(crate) fn build_kubectl_logs_args(
    target: &KubectlLogsTarget,
    namespace: &str,
    follow: bool,
    tail: i64,
    container: Option<&str>,
) -> Vec<String> {
    let mut args = vec!["logs".to_string()];
    match target {
        KubectlLogsTarget::Pod(name) => args.push(name.clone()),
        KubectlLogsTarget::Selector(selector) => {
            args.push("-l".into());
            args.push(selector.clone());
        }
    }
    args.push("-n".into());
    args.push(namespace.to_string());
    if follow {
        args.push("-f".into());
    }
    if tail >= 0 {
        args.push(format!("--tail={tail}"));
    }
    if let Some(c) = container {
        args.push("-c".into());
        args.push(c.to_string());
    }
    // In selector mode there's no single-container guarantee,
    // so explicitly prefix lines with the pod name for the
    // multi-pod case. The single-pod target stays prefix-free
    // — lines already arrive in natural order there.
    if matches!(target, KubectlLogsTarget::Selector(_)) {
        args.push("--prefix=true".into());
        // On a large scale-out the stream from many pods could
        // overwhelm the terminal; --max-log-requests=N caps
        // kubectl's parallel streaming in selector mode. 10 is
        // kubectl's documented default ceiling; we pass it
        // explicitly for predictability.
        args.push("--max-log-requests=10".into());
    }
    args
}

/// What `kubectl logs` targets — a direct pod name or a label
/// selector. The two forms emit incompatible CLI flags
/// (positional pod name vs `-l` selector), so we model the
/// branch up-front.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum KubectlLogsTarget {
    Pod(String),
    Selector(String),
}

/// Resolve the `kubectl logs` target. Without `--pod` — label
/// selector via Argo CD's standard `app.kubernetes.io/instance:
/// <app-name>` label (stamped on every resource it syncs).
pub(crate) fn build_kubectl_logs_target(app_name: &str, pod: Option<&str>) -> KubectlLogsTarget {
    match pod {
        Some(name) => KubectlLogsTarget::Pod(name.to_string()),
        None => KubectlLogsTarget::Selector(format!("app.kubernetes.io/instance={app_name}")),
    }
}

/// Pick the "previous" revision from `status.history`. History
/// is ordered chronologically (oldest first, newest last) with
/// monotonically increasing `id`. The previous entry is the
/// second-to-last; roll back to it.
///
/// Returns a string with a lifetime tied to `app` through the
/// `Value` borrow.
pub(crate) fn pick_previous_revision(app: &Value) -> Result<&str> {
    let history = app
        .pointer("/status/history")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            CliError::Other(
                "Application status.history is empty — no previous revision to roll back to. \
                 Pass `--to <rev>` explicitly."
                    .into(),
            )
        })?;
    if history.len() < 2 {
        return Err(CliError::Other(format!(
            "Application status.history contains {} entry — not enough to roll back to a \
             previous revision. Pass `--to <rev>` explicitly.",
            history.len()
        )));
    }
    history[history.len() - 2]
        .get("revision")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            CliError::Other(
                "Previous status.history entry does not carry a `revision` field — corrupt CR?"
                    .into(),
            )
        })
}

/// Build the Argo CD `Application` CR manifest YAML. Pure fn
/// — tests cover the shape exhaustively without a cluster.
pub(crate) fn build_application_manifest(
    name: &str,
    repo_url: &str,
    target_revision: &str,
    path: &str,
    project: &str,
) -> Value {
    json!({
        "apiVersion": "argoproj.io/v1alpha1",
        "kind": "Application",
        "metadata": {
            "name": name,
            "namespace": ARGOCD_NAMESPACE,
            "labels": {
                "apprafter.io/managed-by": "apprafter",
            },
            "annotations": {
                APPRAFTER_SOURCE_ANNOTATION: "cli",
            },
        },
        "spec": {
            "project": project,
            "source": {
                "repoURL": repo_url,
                "path": path,
                "targetRevision": target_revision,
            },
            "destination": {
                "server": "https://kubernetes.default.svc",
                "namespace": name,
            },
            "syncPolicy": {
                "automated": {
                    "prune": true,
                    "selfHeal": true,
                },
                "syncOptions": [
                    "CreateNamespace=true",
                    "ServerSideApply=true",
                ],
            },
        },
    })
}

fn apply_application_manifest(manifest: &Value, kubeconfig_path: &Path) -> Result<()> {
    use std::io::Write as _;
    let mut file = tempfile::Builder::new()
        .prefix("apprafter-app-")
        .suffix(".json")
        .tempfile()
        .map_err(|e| CliError::Other(format!("create app manifest tempfile: {e}")))?;
    let body = serde_json::to_vec_pretty(manifest)
        .map_err(|e| CliError::Other(format!("serialise app manifest: {e}")))?;
    file.write_all(&body)
        .map_err(|e| CliError::Other(format!("write app manifest tempfile: {e}")))?;
    file.flush()
        .map_err(|e| CliError::Other(format!("flush app manifest tempfile: {e}")))?;

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

fn app_row(app: &Value) -> AppRow {
    let name = app
        .pointer("/metadata/name")
        .and_then(Value::as_str)
        .unwrap_or("?")
        .to_string();
    let project = app
        .pointer("/spec/project")
        .and_then(Value::as_str)
        .unwrap_or("?")
        .to_string();
    let repo = app
        .pointer("/spec/source/repoURL")
        .and_then(Value::as_str)
        .unwrap_or("?")
        .to_string();
    let revision = app
        .pointer("/spec/source/targetRevision")
        .and_then(Value::as_str)
        .unwrap_or("?")
        .to_string();
    let sync = app
        .pointer("/status/sync/status")
        .and_then(Value::as_str)
        .unwrap_or("Unknown")
        .to_string();
    let health = app
        .pointer("/status/health/status")
        .and_then(Value::as_str)
        .unwrap_or("Unknown")
        .to_string();
    AppRow {
        name,
        project,
        repo,
        revision,
        sync,
        health,
    }
}

/// Pure formatter — tests drive it with a fixture JSON.
fn print_status(app: &Value) {
    let name = app
        .pointer("/metadata/name")
        .and_then(Value::as_str)
        .unwrap_or("?");
    let project = app
        .pointer("/spec/project")
        .and_then(Value::as_str)
        .unwrap_or("?");
    let repo = app
        .pointer("/spec/source/repoURL")
        .and_then(Value::as_str)
        .unwrap_or("?");
    let revision = app
        .pointer("/spec/source/targetRevision")
        .and_then(Value::as_str)
        .unwrap_or("?");
    let path = app
        .pointer("/spec/source/path")
        .and_then(Value::as_str)
        .unwrap_or("/");
    let dest_ns = app
        .pointer("/spec/destination/namespace")
        .and_then(Value::as_str)
        .unwrap_or("?");
    let sync = app
        .pointer("/status/sync/status")
        .and_then(Value::as_str)
        .unwrap_or("Unknown");
    let health = app
        .pointer("/status/health/status")
        .and_then(Value::as_str)
        .unwrap_or("Unknown");

    println!("Application {ARGOCD_NAMESPACE}/{name}");
    println!("  project:       {project}");
    println!("  repo:          {repo}");
    println!("  revision:      {revision}");
    println!("  path:          {path}");
    println!("  destination:   {dest_ns}");
    println!("  sync state:    {sync}");
    println!("  health:        {health}");

    if let Some(revs) = app.pointer("/status/history").and_then(Value::as_array) {
        if !revs.is_empty() {
            println!();
            println!("Recent revisions (last {}):", revs.len().min(3));
            for rev in revs.iter().rev().take(3) {
                let id = rev.get("id").and_then(Value::as_u64).unwrap_or(0);
                let rev_str = rev.get("revision").and_then(Value::as_str).unwrap_or("?");
                let deployed_at = rev.get("deployedAt").and_then(Value::as_str).unwrap_or("?");
                println!("  #{id:>3} {rev_str:<10} {deployed_at}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalise_git_url_strips_dotgit_suffix() {
        assert_eq!(
            normalise_git_url("https://github.com/foo/bar.git"),
            "https://github.com/foo/bar"
        );
    }

    #[test]
    fn normalise_git_url_converts_scp_style_to_https() {
        // SCP-style (git@host:org/repo) — the form `git clone`
        // accepts but Argo CD does NOT (the repo-server's git
        // backend wants a scheme prefix). Conversion makes the
        // URL Argo-CD-friendly without operator gymnastics.
        assert_eq!(
            normalise_git_url("git@github.com:foo/bar.git"),
            "https://github.com/foo/bar"
        );
        assert_eq!(
            normalise_git_url("git@gitlab.com:acme/app"),
            "https://gitlab.com/acme/app"
        );
    }

    #[test]
    fn normalise_git_url_strips_ssh_scheme() {
        assert_eq!(
            normalise_git_url("ssh://git@github.com/foo/bar.git"),
            "https://github.com/foo/bar"
        );
    }

    #[test]
    fn normalise_git_url_passes_through_https() {
        // The common Argo-CD-native shape — must round-trip
        // identically except for the optional `.git` strip.
        assert_eq!(
            normalise_git_url("https://gitlab.com/acme/platform"),
            "https://gitlab.com/acme/platform"
        );
    }

    #[test]
    fn derive_app_name_takes_last_path_segment() {
        assert_eq!(derive_app_name("https://github.com/foo/bar-baz"), "bar-baz");
        assert_eq!(derive_app_name("https://gitlab.com/acme/My_App"), "my-app");
    }

    #[test]
    fn derive_app_name_strips_invalid_chars() {
        // Underscores / dots → dashes; whole thing lowercased.
        // Edge case: a URL ending in a dot or only non-alnum
        // chars defaults to "app".
        assert_eq!(
            derive_app_name("https://x.com/MIXED.case_v2"),
            "mixed-case-v2"
        );
        assert_eq!(derive_app_name("https://x.com/..."), "app");
    }

    #[test]
    fn validate_dns_1123_accepts_well_formed_names() {
        assert!(validate_dns_1123("my-app").is_ok());
        assert!(validate_dns_1123("app1").is_ok());
        assert!(validate_dns_1123("a").is_ok());
    }

    #[test]
    fn validate_dns_1123_rejects_uppercase_underscore_leading_dash() {
        assert!(validate_dns_1123("MyApp").is_err());
        assert!(validate_dns_1123("my_app").is_err());
        assert!(validate_dns_1123("-leading").is_err());
        assert!(validate_dns_1123("trailing-").is_err());
        assert!(validate_dns_1123("").is_err());
    }

    #[test]
    fn build_application_manifest_includes_managed_by_label() {
        // Load-bearing — `app list` filters by this label.
        // If a future refactor drops it, `list` shows nothing.
        let m =
            build_application_manifest("my-app", "https://github.com/foo/bar", "main", "/", "apps");
        assert_eq!(
            m.pointer("/metadata/labels/apprafter.io~1managed-by")
                .and_then(Value::as_str),
            Some("apprafter")
        );
        assert_eq!(
            m.pointer("/metadata/annotations/apprafter.io~1source")
                .and_then(Value::as_str),
            Some("cli")
        );
    }

    #[test]
    fn build_application_manifest_routes_to_argocd_namespace() {
        // Argo CD CRs live in the `argocd` namespace by
        // convention; the destination namespace defaults to
        // the app name (workload lives there, app CR lives
        // in argocd).
        let m = build_application_manifest("payments", "https://x/y", "v1.0", "/", "apps");
        assert_eq!(
            m.pointer("/metadata/namespace").and_then(Value::as_str),
            Some("argocd")
        );
        assert_eq!(
            m.pointer("/spec/destination/namespace")
                .and_then(Value::as_str),
            Some("payments")
        );
    }

    #[test]
    fn build_application_manifest_carries_project_and_revision() {
        let m = build_application_manifest("a", "u", "v", "/p", "apps");
        assert_eq!(
            m.pointer("/spec/project").and_then(Value::as_str),
            Some("apps")
        );
        assert_eq!(
            m.pointer("/spec/source/targetRevision")
                .and_then(Value::as_str),
            Some("v")
        );
        assert_eq!(
            m.pointer("/spec/source/path").and_then(Value::as_str),
            Some("/p")
        );
    }

    #[test]
    fn build_kubectl_logs_target_defaults_to_selector() {
        let target = build_kubectl_logs_target("payments", None);
        assert_eq!(
            target,
            KubectlLogsTarget::Selector("app.kubernetes.io/instance=payments".to_string())
        );
    }

    #[test]
    fn build_kubectl_logs_target_uses_pod_name_when_provided() {
        let target = build_kubectl_logs_target("payments", Some("payments-7f9c-xyz"));
        assert_eq!(
            target,
            KubectlLogsTarget::Pod("payments-7f9c-xyz".to_string())
        );
    }

    #[test]
    fn build_kubectl_logs_args_selector_form_includes_prefix_and_max_requests() {
        // Selector mode aggregates across multiple pods;
        // kubectl benefits from --prefix to distinguish lines,
        // and --max-log-requests to cap fan-out. Both are
        // load-bearing for real usability on multi-pod apps.
        let args = build_kubectl_logs_args(
            &KubectlLogsTarget::Selector("app.kubernetes.io/instance=payments".to_string()),
            "payments",
            false,
            -1,
            None,
        );
        assert!(args.contains(&"-l".to_string()));
        assert!(args.contains(&"app.kubernetes.io/instance=payments".to_string()));
        assert!(args.contains(&"-n".to_string()));
        assert!(args.contains(&"payments".to_string()));
        assert!(args.contains(&"--prefix=true".to_string()));
        assert!(args.contains(&"--max-log-requests=10".to_string()));
        assert!(!args.contains(&"-f".to_string()));
        assert!(!args.iter().any(|a| a.starts_with("--tail=")));
    }

    #[test]
    fn build_kubectl_logs_args_pod_form_drops_prefix() {
        // Single-pod target — no need for a line prefix;
        // kubectl's default output is already clean. Defensive:
        // ensure we don't accidentally emit --prefix or
        // --max-log-requests when it would just clutter the
        // output.
        let args = build_kubectl_logs_args(
            &KubectlLogsTarget::Pod("payments-7f9c-xyz".to_string()),
            "payments",
            true,
            100,
            Some("api"),
        );
        assert_eq!(args[0], "logs");
        assert_eq!(args[1], "payments-7f9c-xyz");
        assert!(args.contains(&"-f".to_string()));
        assert!(args.contains(&"--tail=100".to_string()));
        assert!(args.contains(&"-c".to_string()));
        assert!(args.contains(&"api".to_string()));
        assert!(!args.contains(&"--prefix=true".to_string()));
        assert!(!args.iter().any(|a| a.contains("--max-log-requests")));
    }

    #[test]
    fn pick_previous_revision_returns_second_to_last() {
        // Argo CD's status.history is chronological — oldest
        // first, newest last. Previous = second-to-last.
        let app = serde_json::json!({
            "status": {
                "history": [
                    {"id": 1, "revision": "abc123"},
                    {"id": 2, "revision": "def456"},
                    {"id": 3, "revision": "ghi789"}
                ]
            }
        });
        assert_eq!(pick_previous_revision(&app).unwrap(), "def456");
    }

    #[test]
    fn pick_previous_revision_errors_when_history_too_short() {
        // Fresh app — one or zero history entries — has no
        // "previous" to roll back to. The operator must pass
        // --to explicitly. Tests both edge cases.
        let one = serde_json::json!({
            "status": { "history": [ {"id": 1, "revision": "abc"} ] }
        });
        let zero = serde_json::json!({ "status": { "history": [] } });
        let missing = serde_json::json!({ "status": {} });
        assert!(pick_previous_revision(&one).is_err());
        assert!(pick_previous_revision(&zero).is_err());
        assert!(pick_previous_revision(&missing).is_err());
    }

    #[test]
    fn print_status_handles_app_without_status_block() {
        // Freshly-created Argo CD Application has no status
        // yet — must not panic, must default to Unknown for
        // sync + health.
        let app = serde_json::json!({
            "metadata": { "name": "x" },
            "spec": {
                "project": "apps",
                "source": { "repoURL": "https://r", "targetRevision": "main" },
                "destination": { "namespace": "x" }
            }
        });
        print_status(&app);
    }
}
