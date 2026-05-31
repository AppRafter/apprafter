// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! `apprafter repo creds …` — manage private-source credentials via the
//! `SourceCredential` CRD (1.79c / ADR 0039).
//!
//! The CLI is a thin, gated front-end: it shape-checks the PAT, seals the
//! material client-side with the in-cluster sealed-secrets controller's
//! public cert, and writes a `SealedSecret` (the material, never
//! plaintext) plus a `SourceCredential` CR (config-only, references the
//! sealed material). The operator derives a prefix-matched Argo
//! `repo-creds` Secret and a host-matched workload pull-secret from the
//! one CR. `list` / `show` read `.status` only — the CLI cannot decrypt a
//! SealedSecret (no cluster private key), so the material is unreadable
//! from here by construction.
//!
//! Launch default: a single classic PAT used in both halves. For a
//! `github.com/<org>` repo the registry host (`ghcr.io/<org>`) is
//! inferred automatically; other hosts get a git-only credential.

use std::collections::BTreeMap;
use std::io::{self, IsTerminal, Write as _};
use std::path::Path;
use std::process::Command;

use cli_core::{CliError, Result};
use cli_providers::k8s::kubectl::KubectlCli;
use cli_providers::k8s::sealing::{build_sealed_secret, fetch_controller_public_key};
use serde_json::{json, Value};
use tabled::{Table, Tabled};

use crate::commands::k8s_helpers::{ensure_kubeconfig_tempfile, kubectl_get_json};

/// Namespace SourceCredentials + their sealed material live in.
const SOURCECRED_NAMESPACE: &str = "apprafter-system";
const GIT_USERNAME_ANNOTATION: &str = "apprafter.io/git-username";
const AUTH_TYPE_ANNOTATION: &str = "apprafter.io/auth-type";

#[derive(Tabled)]
struct CredsRow {
    #[tabled(rename = "NAME")]
    name: String,
    #[tabled(rename = "REPO PREFIXES")]
    repo_prefixes: String,
    #[tabled(rename = "REGISTRY HOSTS")]
    hosts: String,
    #[tabled(rename = "STATUS")]
    status: String,
}

#[allow(clippy::too_many_arguments)]
pub fn add(
    name: &str,
    url_prefix: &str,
    auth_type: &str,
    username: &str,
    token: Option<String>,
    no_validate: bool,
    no_interactive: bool,
) -> Result<()> {
    let stdin_is_tty = io::stdin().is_terminal();
    let stdout_is_tty = std::io::stdout().is_terminal();
    if crate::commands::repo_creds_wizard::should_use_wizard(
        no_interactive,
        stdin_is_tty,
        stdout_is_tty,
    ) {
        return add_via_wizard(name, url_prefix, auth_type, username, token, no_validate);
    }

    validate_dns_1123(name)?;
    let auth = parse_auth_type(auth_type)?;
    let token = resolve_token(token)?;
    if !no_validate {
        validate_token_format(&token, &auth)?;
    }

    let kc = ensure_kubeconfig_tempfile()?;
    let existing = kubectl_get_json(
        "sourcecredential",
        Some(name),
        Some(SOURCECRED_NAMESPACE),
        kc.path(),
    )?;
    if existing.is_some() {
        return Err(CliError::Other(format!(
            "SourceCredential '{name}' already exists in {SOURCECRED_NAMESPACE}. Run \
             `apprafter repo creds rotate {name}` to replace the token, or \
             `apprafter repo creds remove {name}` then `add` to recreate."
        )));
    }

    // Seal the material client-side and write the SealedSecret. Only the
    // controller's private key can decrypt — the CLI never holds it.
    let pub_key = fetch_controller_public_key(&KubectlCli, kc.path())?;
    let material_name = material_secret_name(name);
    let sealed = build_sealed_secret(
        &pub_key,
        SOURCECRED_NAMESPACE,
        &material_name,
        &material_data(username, &token),
        "Opaque",
    )?;
    apply_manifest(&sealed, kc.path())?;

    let git_prefix = normalize_git_prefix(url_prefix);
    let registry_host = infer_registry_host(&git_prefix);
    let cr = build_source_credential_cr(
        name,
        &git_prefix,
        registry_host.as_deref(),
        &material_name,
        username,
        auth_type,
    );
    apply_manifest(&cr, kc.path())?;

    println!("✓ SourceCredential '{name}' registered (material sealed).");
    println!("  Repo prefix:   {git_prefix}");
    match &registry_host {
        Some(h) => println!("  Registry host: {h}  (inferred)"),
        None => println!("  Registry host: —  (git-only; pass a github.com repo to infer ghcr.io)"),
    }
    println!();
    println!(
        "The operator derives the Argo repo-cred + workload pull-secret. Check validity with:"
    );
    println!("  apprafter repo creds show {name}");
    Ok(())
}

/// Wizard entry point — gathers missing fields via inquire prompts
/// (masked token) and re-dispatches into the non-interactive `add`.
fn add_via_wizard(
    name: &str,
    url_prefix: &str,
    auth_type: &str,
    username: &str,
    token: Option<String>,
    no_validate: bool,
) -> Result<()> {
    let inputs = crate::commands::repo_creds_wizard::WizardInputs {
        name: if name.is_empty() {
            None
        } else {
            Some(name.to_string())
        },
        url_prefix: if url_prefix.is_empty() {
            None
        } else {
            Some(url_prefix.to_string())
        },
        auth_type: Some(auth_type.to_string()),
        username: Some(username.to_string()),
        token,
        no_validate,
    };
    let out = crate::commands::repo_creds_wizard::run(inputs)?;
    add(
        &out.name,
        &out.url_prefix,
        &out.auth_type,
        &out.username,
        Some(out.token),
        no_validate,
        true, // no_interactive — prevents wizard recursion.
    )
}

pub fn list() -> Result<()> {
    let kc = ensure_kubeconfig_tempfile()?;
    let creds = fetch_source_credentials(kc.path())?;
    if creds.is_empty() {
        println!("No SourceCredentials registered in {SOURCECRED_NAMESPACE}.");
        println!(
            "Hint: run `apprafter repo creds add <name> --url-prefix <url> --token <pat>` to \
             register the first entry."
        );
        return Ok(());
    }
    let rows: Vec<CredsRow> = creds.iter().map(creds_row).collect();
    println!("{}", Table::new(&rows));
    Ok(())
}

pub fn show(name: &str) -> Result<()> {
    let kc = ensure_kubeconfig_tempfile()?;
    let cred = kubectl_get_json(
        "sourcecredential",
        Some(name),
        Some(SOURCECRED_NAMESPACE),
        kc.path(),
    )?
    .ok_or_else(|| {
        CliError::Other(format!(
            "SourceCredential '{name}' not found in {SOURCECRED_NAMESPACE}."
        ))
    })?;
    print_creds_detail(&cred);
    Ok(())
}

pub fn rotate(name: &str, token: Option<String>, no_validate: bool) -> Result<()> {
    let kc = ensure_kubeconfig_tempfile()?;
    let cred = kubectl_get_json(
        "sourcecredential",
        Some(name),
        Some(SOURCECRED_NAMESPACE),
        kc.path(),
    )?
    .ok_or_else(|| {
        CliError::Other(format!(
            "SourceCredential '{name}' not found in {SOURCECRED_NAMESPACE}."
        ))
    })?;

    let username = cred
        .pointer(&format!(
            "/metadata/annotations/{}",
            esc(GIT_USERNAME_ANNOTATION)
        ))
        .and_then(Value::as_str)
        .unwrap_or("git")
        .to_string();
    let auth_type = cred
        .pointer(&format!(
            "/metadata/annotations/{}",
            esc(AUTH_TYPE_ANNOTATION)
        ))
        .and_then(Value::as_str)
        .unwrap_or("pat")
        .to_string();
    let auth = parse_auth_type(&auth_type)?;
    let new_token = resolve_token(token)?;
    if !no_validate {
        validate_token_format(&new_token, &auth)?;
    }

    // Re-seal the material in place — the operator re-derives both halves.
    let pub_key = fetch_controller_public_key(&KubectlCli, kc.path())?;
    let material_name = material_secret_name(name);
    let sealed = build_sealed_secret(
        &pub_key,
        SOURCECRED_NAMESPACE,
        &material_name,
        &material_data(&username, &new_token),
        "Opaque",
    )?;
    apply_manifest(&sealed, kc.path())?;

    println!("✓ Material for SourceCredential '{name}' re-sealed.");
    println!("The operator re-derives the Argo repo-cred + pull-secret on its next reconcile.");
    Ok(())
}

pub fn remove(name: &str, force: bool, yes: bool) -> Result<()> {
    let kc = ensure_kubeconfig_tempfile()?;
    let cred = kubectl_get_json(
        "sourcecredential",
        Some(name),
        Some(SOURCECRED_NAMESPACE),
        kc.path(),
    )?
    .ok_or_else(|| {
        CliError::Other(format!(
            "SourceCredential '{name}' not found in {SOURCECRED_NAMESPACE}."
        ))
    })?;

    let prefixes = repo_prefixes_of(&cred);

    if !force {
        // Best-effort reverse-dependency gate: refuse if Argo CD
        // Application(s) point under one of this credential's repo
        // prefixes. (The operator's MigrationPlan gate is the
        // authoritative actor-agnostic check; this is fast UX.)
        let mut deps: Vec<String> = Vec::new();
        for prefix in &prefixes {
            deps.extend(find_dependent_applications(
                &normalize_repo_url(prefix),
                kc.path(),
            )?);
        }
        deps.sort();
        deps.dedup();
        if !deps.is_empty() {
            return Err(CliError::Other(format!(
                "SourceCredential '{name}' is used by {n} Application(s): {names}. \
                 Re-point them or pass `--force` to delete anyway.",
                n = deps.len(),
                names = deps.join(", ")
            )));
        }
    }

    if !yes && !force {
        if !io::stdin().is_terminal() {
            return Err(CliError::Other(
                "non-interactive shell — pass `--yes` (or `--force`) to skip the confirmation prompt"
                    .into(),
            ));
        }
        println!("Delete SourceCredential '{name}' and its sealed material?");
        let confirmed = inquire::Confirm::new("Confirm?")
            .with_default(false)
            .prompt()
            .map_err(|e| CliError::Other(format!("confirmation prompt: {e}")))?;
        if !confirmed {
            println!("Cancelled.");
            return Ok(());
        }
    }

    // Delete the CR + the SealedSecret. The sealed-secrets controller
    // owns the unsealed material Secret via ownerReference, so it
    // cascades. Derived repo-cred / pull-secret are left as-is (GC on
    // coverage removal is the operator's MigrationPlan concern).
    kubectl_delete("sourcecredential", name, SOURCECRED_NAMESPACE, kc.path())?;
    kubectl_delete_ignore_missing(
        "sealedsecret",
        &material_secret_name(name),
        SOURCECRED_NAMESPACE,
        kc.path(),
    )?;
    println!("✓ SourceCredential '{name}' deleted.");
    Ok(())
}

// =========================================================================
// PURE HELPERS — testable without kube::Client / network.
// =========================================================================

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum AuthType {
    Pat,
    Basic,
}

fn parse_auth_type(raw: &str) -> Result<AuthType> {
    match raw {
        "pat" => Ok(AuthType::Pat),
        "basic" => Ok(AuthType::Basic),
        "ssh" => Err(CliError::Other(
            "SSH-key auth deferred to Phase 2 — pass `--type pat` or `--type basic` for now".into(),
        )),
        other => Err(CliError::Other(format!(
            "Unknown `--type {other}` — accepted values are `pat` / `basic`."
        ))),
    }
}

fn validate_dns_1123(name: &str) -> Result<()> {
    if name.is_empty() || name.len() > 64 {
        return Err(CliError::Other(format!(
            "Creds name '{name}' must be 1..63 DNS-1123 characters (lowercase [a-z0-9-])."
        )));
    }
    let ok = name
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && !name.starts_with('-')
        && !name.ends_with('-');
    if !ok {
        return Err(CliError::Other(format!(
            "Creds name '{name}' contains invalid characters; expected DNS-1123."
        )));
    }
    Ok(())
}

fn resolve_token(token: Option<String>) -> Result<String> {
    if let Some(t) = token {
        if t.is_empty() {
            return Err(CliError::Other(
                "empty `--token` — pass a non-empty token or omit the flag for an interactive prompt"
                    .into(),
            ));
        }
        return Ok(t);
    }
    if !io::stdin().is_terminal() {
        return Err(CliError::Other(
            "`--token` not provided and stdin is not a TTY — pass `--token <value>` or run from \
             an interactive shell"
                .into(),
        ));
    }
    let token = inquire::Password::new("Token:")
        .with_display_mode(inquire::PasswordDisplayMode::Masked)
        .without_confirmation()
        .prompt()
        .map_err(|e| CliError::Other(format!("token prompt: {e}")))?;
    if token.is_empty() {
        return Err(CliError::Other("token cannot be empty".into()));
    }
    Ok(token)
}

/// Provider-aware token regex check. **Best-effort** — `--no-validate`
/// skips it for self-hosted Gitea/Forgejo with custom token shapes.
pub fn validate_token_format(token: &str, auth: &AuthType) -> Result<()> {
    if matches!(auth, AuthType::Basic) {
        if token.is_empty() {
            return Err(CliError::Other("`--token` cannot be empty".into()));
        }
        return Ok(());
    }
    if token.starts_with("github_pat_") {
        if token.len() < 80 {
            return Err(CliError::Other(format!(
                "GitHub fine-grained PAT length {} is too short — expected 80+ characters. \
                 Pass `--no-validate` if you're sure the token is correct.",
                token.len()
            )));
        }
        return Ok(());
    }
    if token.starts_with("ghp_") {
        if token.len() != 40 {
            return Err(CliError::Other(format!(
                "GitHub classic PAT length {} ≠ 40 (expected `ghp_` + 36 alphanumeric). \
                 Pass `--no-validate` to bypass.",
                token.len()
            )));
        }
        return Ok(());
    }
    if token.starts_with("glpat-") {
        if token.len() < 20 {
            return Err(CliError::Other(format!(
                "GitLab PAT length {} is too short — expected `glpat-` + 20+ characters. \
                 Pass `--no-validate` to bypass.",
                token.len()
            )));
        }
        return Ok(());
    }
    if token.len() < 20 {
        return Err(CliError::Other(format!(
            "Token shape not recognised (not a GitHub PAT, not a GitLab PAT), and length \
             {} < 20 — too short for most providers. Pass `--no-validate` if running \
             self-hosted Gitea/Forgejo with a custom token shape.",
            token.len()
        )));
    }
    Ok(())
}

fn material_secret_name(cred_name: &str) -> String {
    format!("srccred-{cred_name}-material")
}

fn material_data(username: &str, token: &str) -> BTreeMap<String, Vec<u8>> {
    let mut m = BTreeMap::new();
    m.insert("username".to_string(), username.as_bytes().to_vec());
    m.insert("password".to_string(), token.as_bytes().to_vec());
    m
}

/// Normalise a `--url-prefix` into a scheme-less, trailing-slash repo
/// prefix for `spec.git.repoPrefixes`: `"https://github.com/myorg"` →
/// `"github.com/myorg/"`.
fn normalize_git_prefix(url_prefix: &str) -> String {
    let no_scheme = url_prefix
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(url_prefix);
    format!("{}/", no_scheme.trim_end_matches('/'))
}

/// Add the scheme back for a repoURL prefix comparison against Argo CD
/// Applications (`"github.com/myorg/"` → `"https://github.com/myorg"`).
fn normalize_repo_url(prefix: &str) -> String {
    let with_scheme = if prefix.contains("://") {
        prefix.to_string()
    } else {
        format!("https://{prefix}")
    };
    with_scheme.trim_end_matches('/').to_string()
}

/// Infer the registry-host prefix for the launch-default single PAT.
/// GitHub repos publish images to GHCR under the same org:
/// `"github.com/myorg/"` → `"ghcr.io/myorg/"`. Other hosts return
/// `None` (git-only credential).
fn infer_registry_host(git_prefix: &str) -> Option<String> {
    let org = git_prefix.strip_prefix("github.com/")?;
    let org = org.split('/').next().filter(|s| !s.is_empty())?;
    // GHCR namespaces are lowercase (OCI image refs are lowercase-only),
    // even when the GitHub org has mixed case — the operator host-matches
    // a lowercase rendered image. Only the registry host is lowercased;
    // the git prefix keeps its case to prefix-match the Application's
    // repoURL (which carries whatever case the user typed).
    Some(format!("ghcr.io/{}/", org.to_ascii_lowercase()))
}

fn build_source_credential_cr(
    name: &str,
    git_prefix: &str,
    registry_host: Option<&str>,
    material_name: &str,
    username: &str,
    auth_type: &str,
) -> Value {
    let mut spec = json!({
        "git": {
            "backend": { "sealedSecretRef": { "name": material_name } },
            "repoPrefixes": [git_prefix],
        }
    });
    if let Some(host) = registry_host {
        spec["registry"] = json!({
            "backend": { "sealedSecretRef": { "name": material_name } },
            "hosts": [host],
        });
    }
    json!({
        "apiVersion": "apprafter.io/v1alpha1",
        "kind": "SourceCredential",
        "metadata": {
            "name": name,
            "namespace": SOURCECRED_NAMESPACE,
            "labels": { "apprafter.io/managed-by": "apprafter" },
            "annotations": {
                GIT_USERNAME_ANNOTATION: username,
                AUTH_TYPE_ANNOTATION: auth_type,
            },
        },
        "spec": spec,
    })
}

/// JSON-pointer escaping for an annotation key (the `/` in
/// `apprafter.io/...` becomes `~1`).
fn esc(key: &str) -> String {
    key.replace('~', "~0").replace('/', "~1")
}

fn repo_prefixes_of(cred: &Value) -> Vec<String> {
    cred.pointer("/spec/git/repoPrefixes")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn hosts_of(cred: &Value) -> Vec<String> {
    cred.pointer("/spec/registry/hosts")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// One-line status summary from `.status.conditions` — e.g.
/// `git:True registry:True` or `pending`.
fn status_summary(cred: &Value) -> String {
    let conds = cred.pointer("/status/conditions").and_then(Value::as_array);
    let Some(conds) = conds else {
        return "pending".to_string();
    };
    let find = |type_: &str| {
        conds
            .iter()
            .find(|c| c.get("type").and_then(Value::as_str) == Some(type_))
            .and_then(|c| c.get("status").and_then(Value::as_str))
    };
    let mut parts = Vec::new();
    if let Some(s) = find("GitPresent") {
        parts.push(format!("git:{s}"));
    }
    if let Some(s) = find("RegistryPresent") {
        parts.push(format!("registry:{s}"));
    }
    if parts.is_empty() {
        "pending".to_string()
    } else {
        parts.join(" ")
    }
}

fn creds_row(cred: &Value) -> CredsRow {
    let name = cred
        .pointer("/metadata/name")
        .and_then(Value::as_str)
        .unwrap_or("?")
        .to_string();
    CredsRow {
        name,
        repo_prefixes: join_or_dash(&repo_prefixes_of(cred)),
        hosts: join_or_dash(&hosts_of(cred)),
        status: status_summary(cred),
    }
}

fn join_or_dash(items: &[String]) -> String {
    if items.is_empty() {
        "—".to_string()
    } else {
        items.join(", ")
    }
}

fn print_creds_detail(cred: &Value) {
    let name = cred
        .pointer("/metadata/name")
        .and_then(Value::as_str)
        .unwrap_or("?");
    println!("SourceCredential {SOURCECRED_NAMESPACE}/{name}");
    println!(
        "  Repo prefixes:  {}",
        join_or_dash(&repo_prefixes_of(cred))
    );
    println!("  Registry hosts: {}", join_or_dash(&hosts_of(cred)));
    println!("  Status:         {}", status_summary(cred));
    if let Some(conds) = cred.pointer("/status/conditions").and_then(Value::as_array) {
        for c in conds {
            let t = c.get("type").and_then(Value::as_str).unwrap_or("?");
            let s = c.get("status").and_then(Value::as_str).unwrap_or("?");
            let r = c.get("reason").and_then(Value::as_str).unwrap_or("");
            let m = c.get("message").and_then(Value::as_str).unwrap_or("");
            println!("    - {t}={s} ({r}) {m}");
        }
    }
    println!("  Material:       sealed — unreadable from the CLI (no cluster private key).");
}

// =========================================================================
// kube-touching helpers.
// =========================================================================

fn apply_manifest(manifest: &Value, kubeconfig_path: &Path) -> Result<()> {
    let mut file = tempfile::Builder::new()
        .prefix("apprafter-srccred-")
        .suffix(".json")
        .tempfile()
        .map_err(|e| CliError::Other(format!("create manifest tempfile: {e}")))?;
    let body = serde_json::to_vec_pretty(manifest)
        .map_err(|e| CliError::Other(format!("serialise manifest: {e}")))?;
    file.write_all(&body)
        .map_err(|e| CliError::Other(format!("write manifest tempfile: {e}")))?;
    file.flush()
        .map_err(|e| CliError::Other(format!("flush manifest tempfile: {e}")))?;

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

fn kubectl_delete(kind: &str, name: &str, namespace: &str, kubeconfig_path: &Path) -> Result<()> {
    let out = Command::new("kubectl")
        .arg("delete")
        .arg(kind)
        .arg(name)
        .arg("-n")
        .arg(namespace)
        .env("KUBECONFIG", kubeconfig_path)
        .output()
        .map_err(|e| CliError::Other(format!("spawn kubectl delete: {e}")))?;
    if !out.status.success() {
        return Err(CliError::Other(format!(
            "kubectl delete {kind} {name} failed (exit {:?}): {}",
            out.status.code(),
            String::from_utf8_lossy(&out.stderr)
        )));
    }
    Ok(())
}

fn kubectl_delete_ignore_missing(
    kind: &str,
    name: &str,
    namespace: &str,
    kubeconfig_path: &Path,
) -> Result<()> {
    let out = Command::new("kubectl")
        .arg("delete")
        .arg(kind)
        .arg(name)
        .arg("-n")
        .arg(namespace)
        .arg("--ignore-not-found")
        .env("KUBECONFIG", kubeconfig_path)
        .output()
        .map_err(|e| CliError::Other(format!("spawn kubectl delete: {e}")))?;
    if !out.status.success() {
        return Err(CliError::Other(format!(
            "kubectl delete {kind} {name} failed (exit {:?}): {}",
            out.status.code(),
            String::from_utf8_lossy(&out.stderr)
        )));
    }
    Ok(())
}

/// Fetch all SourceCredentials in the platform namespace (for `app add`
/// coverage checks too).
pub(crate) fn fetch_source_credentials_public(kubeconfig_path: &Path) -> Result<Vec<Value>> {
    fetch_source_credentials(kubeconfig_path)
}

fn fetch_source_credentials(kubeconfig_path: &Path) -> Result<Vec<Value>> {
    let out = Command::new("kubectl")
        .arg("get")
        .arg("sourcecredential")
        .arg("-n")
        .arg(SOURCECRED_NAMESPACE)
        .arg("-o")
        .arg("json")
        .env("KUBECONFIG", kubeconfig_path)
        .output()
        .map_err(|e| CliError::Other(format!("spawn kubectl: {e}")))?;
    if !out.status.success() {
        return Err(CliError::Other(format!(
            "kubectl get sourcecredentials failed (exit {:?}): {}",
            out.status.code(),
            String::from_utf8_lossy(&out.stderr)
        )));
    }
    let parsed: Value = serde_json::from_slice(&out.stdout)
        .map_err(|e| CliError::Other(format!("kubectl JSON parse: {e}")))?;
    Ok(parsed
        .get("items")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default())
}

/// Does any SourceCredential's declared repo prefix cover `repo_url`?
/// Used by `app add`'s coverage hint (pure — tested without a cluster).
pub(crate) fn any_credential_covers(creds: &[Value], repo_url: &str) -> bool {
    creds
        .iter()
        .any(|cred| credential_prefix_covers(cred, repo_url))
}

/// Does any SourceCredential cover `repo_url` **and** report
/// `GitValid=True` in its status? The confirmed coverage gate
/// (1.79c S5) requires a validated credential, not merely a declared
/// prefix. Pure — tested without a cluster.
pub(crate) fn valid_credential_covers(creds: &[Value], repo_url: &str) -> bool {
    creds
        .iter()
        .any(|cred| credential_prefix_covers(cred, repo_url) && git_valid_is_true(cred))
}

/// Shared prefix-coverage predicate for one credential.
fn credential_prefix_covers(cred: &Value, repo_url: &str) -> bool {
    repo_prefixes_of(cred)
        .iter()
        .any(|p| repo_url.starts_with(&normalize_repo_url(p)))
}

/// Is the credential's `status.conditions[type=GitValid].status` == "True"?
fn git_valid_is_true(cred: &Value) -> bool {
    cred.pointer("/status/conditions")
        .and_then(Value::as_array)
        .map(|conds| {
            conds.iter().any(|c| {
                c.get("type").and_then(Value::as_str) == Some("GitValid")
                    && c.get("status").and_then(Value::as_str) == Some("True")
            })
        })
        .unwrap_or(false)
}

fn find_dependent_applications(url_prefix: &str, kubeconfig_path: &Path) -> Result<Vec<String>> {
    let out = Command::new("kubectl")
        .arg("get")
        .arg("application.argoproj.io")
        .arg("-n")
        .arg("argocd")
        .arg("-o")
        .arg("json")
        .env("KUBECONFIG", kubeconfig_path)
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
    Ok(find_apps_matching_prefix(&parsed, url_prefix))
}

/// Filter Applications whose `spec.source.repoURL` starts with
/// `url_prefix`. Pure fn — tests cover prefix-match semantics.
pub(crate) fn find_apps_matching_prefix(
    applications_json: &Value,
    url_prefix: &str,
) -> Vec<String> {
    applications_json
        .get("items")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter(|app| {
                    app.pointer("/spec/source/repoURL")
                        .and_then(Value::as_str)
                        .map(|u| u.starts_with(url_prefix))
                        .unwrap_or(false)
                })
                .filter_map(|app| {
                    app.pointer("/metadata/name")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                })
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parse_auth_type_accepts_pat_and_basic() {
        assert_eq!(parse_auth_type("pat").unwrap(), AuthType::Pat);
        assert_eq!(parse_auth_type("basic").unwrap(), AuthType::Basic);
    }

    #[test]
    fn parse_auth_type_rejects_ssh_with_phase2_hint() {
        let err = parse_auth_type("ssh").unwrap_err().to_string();
        assert!(err.contains("Phase 2"), "{err}");
    }

    #[test]
    fn parse_auth_type_rejects_unknown() {
        assert!(parse_auth_type("oauth").is_err());
    }

    #[test]
    fn validate_token_format_accepts_github_fine_grained_pat() {
        let token = format!("github_pat_{}", "x".repeat(80));
        assert!(validate_token_format(&token, &AuthType::Pat).is_ok());
    }

    #[test]
    fn validate_token_format_rejects_short_github_fine_grained_pat() {
        let too_short = format!("github_pat_{}", "x".repeat(40));
        assert!(validate_token_format(&too_short, &AuthType::Pat).is_err());
    }

    #[test]
    fn validate_token_format_accepts_github_classic_pat() {
        let token = format!("ghp_{}", "a".repeat(36));
        assert_eq!(token.len(), 40);
        assert!(validate_token_format(&token, &AuthType::Pat).is_ok());
    }

    #[test]
    fn validate_token_format_rejects_wrong_length_github_classic_pat() {
        assert!(validate_token_format("ghp_short", &AuthType::Pat).is_err());
        let too_long = format!("ghp_{}", "a".repeat(50));
        assert!(validate_token_format(&too_long, &AuthType::Pat).is_err());
    }

    #[test]
    fn validate_token_format_accepts_gitlab_pat() {
        let token = format!("glpat-{}", "x".repeat(20));
        assert!(validate_token_format(&token, &AuthType::Pat).is_ok());
    }

    #[test]
    fn validate_token_format_accepts_generic_long_token() {
        let token = "abcdefghij1234567890abc".to_string();
        assert!(validate_token_format(&token, &AuthType::Pat).is_ok());
    }

    #[test]
    fn validate_token_format_rejects_short_generic_token() {
        let too_short = "shorty".to_string();
        let err = validate_token_format(&too_short, &AuthType::Pat)
            .unwrap_err()
            .to_string();
        assert!(err.contains("no-validate"), "{err}");
    }

    #[test]
    fn validate_token_format_basic_accepts_anything_non_empty() {
        assert!(validate_token_format("p", &AuthType::Basic).is_ok());
        assert!(validate_token_format("really.long.basic.auth.password", &AuthType::Basic).is_ok());
    }

    #[test]
    fn validate_dns_1123_for_creds_name() {
        assert!(validate_dns_1123("my-repo").is_ok());
        assert!(validate_dns_1123("MyRepo").is_err());
        assert!(validate_dns_1123("my_repo").is_err());
        assert!(validate_dns_1123("").is_err());
    }

    #[test]
    fn material_secret_name_is_deterministic() {
        assert_eq!(material_secret_name("acme"), "srccred-acme-material");
    }

    #[test]
    fn normalize_git_prefix_strips_scheme_and_adds_trailing_slash() {
        assert_eq!(
            normalize_git_prefix("https://github.com/myorg"),
            "github.com/myorg/"
        );
        assert_eq!(
            normalize_git_prefix("github.com/myorg/"),
            "github.com/myorg/"
        );
    }

    #[test]
    fn infer_registry_host_maps_github_org_to_ghcr() {
        assert_eq!(
            infer_registry_host("github.com/myorg/"),
            Some("ghcr.io/myorg/".to_string())
        );
        assert_eq!(infer_registry_host("gitlab.com/myorg/"), None);
    }

    #[test]
    fn infer_registry_host_lowercases_the_org() {
        // GHCR namespaces are canonically lowercase (OCI image refs are
        // lowercase-only), even when the GitHub org has mixed case. The
        // operator host-matches a lowercase rendered image, so the stored
        // host must be lowercase or the pull-secret never attaches.
        assert_eq!(
            infer_registry_host("github.com/ProcVue/landing/"),
            Some("ghcr.io/procvue/".to_string())
        );
    }

    #[test]
    fn build_source_credential_cr_single_pat_covers_both_halves() {
        let cr = build_source_credential_cr(
            "acme",
            "github.com/acme/",
            Some("ghcr.io/acme/"),
            "srccred-acme-material",
            "git",
            "pat",
        );
        assert_eq!(cr["apiVersion"], "apprafter.io/v1alpha1");
        assert_eq!(cr["kind"], "SourceCredential");
        assert_eq!(cr["metadata"]["namespace"], "apprafter-system");
        assert_eq!(
            cr["spec"]["git"]["backend"]["sealedSecretRef"]["name"],
            "srccred-acme-material"
        );
        assert_eq!(cr["spec"]["git"]["repoPrefixes"][0], "github.com/acme/");
        assert_eq!(cr["spec"]["registry"]["hosts"][0], "ghcr.io/acme/");
        assert_eq!(
            cr.pointer("/metadata/annotations/apprafter.io~1git-username")
                .and_then(Value::as_str),
            Some("git")
        );
    }

    #[test]
    fn build_source_credential_cr_git_only_when_no_registry() {
        let cr = build_source_credential_cr(
            "acme",
            "gitlab.com/acme/",
            None,
            "srccred-acme-material",
            "git",
            "pat",
        );
        assert!(cr["spec"]["registry"].is_null());
        assert_eq!(cr["spec"]["git"]["repoPrefixes"][0], "gitlab.com/acme/");
    }

    #[test]
    fn status_summary_reads_conditions() {
        let cred = json!({
            "status": { "conditions": [
                { "type": "GitPresent", "status": "True" },
                { "type": "RegistryPresent", "status": "True" }
            ]}
        });
        assert_eq!(status_summary(&cred), "git:True registry:True");
        assert_eq!(status_summary(&json!({})), "pending");
    }

    #[test]
    fn any_credential_covers_matches_declared_prefix() {
        let creds = vec![json!({
            "spec": { "git": { "repoPrefixes": ["github.com/myorg/"] } }
        })];
        assert!(any_credential_covers(
            &creds,
            "https://github.com/myorg/repo"
        ));
        assert!(!any_credential_covers(
            &creds,
            "https://github.com/other/repo"
        ));
    }

    #[test]
    fn valid_credential_covers_requires_gitvalid_true() {
        let repo = "https://github.com/myorg/repo";
        let covering_unverified = vec![json!({
            "spec": { "git": { "repoPrefixes": ["github.com/myorg/"] } },
            "status": { "conditions": [
                { "type": "GitValid", "status": "Unknown", "reason": "Unverified" }
            ] }
        })];
        // present (prefix declared) but NOT confirmed (not GitValid=True)
        assert!(any_credential_covers(&covering_unverified, repo));
        assert!(!valid_credential_covers(&covering_unverified, repo));

        let covering_valid = vec![json!({
            "spec": { "git": { "repoPrefixes": ["github.com/myorg/"] } },
            "status": { "conditions": [
                { "type": "GitValid", "status": "True", "reason": "Reachable" }
            ] }
        })];
        assert!(valid_credential_covers(&covering_valid, repo));

        // GitValid=True but prefix does not cover the repo → not covered
        assert!(!valid_credential_covers(
            &covering_valid,
            "https://github.com/other/repo"
        ));
    }

    #[test]
    fn find_apps_matching_prefix_filters_by_repo_url() {
        let apps = json!({
            "items": [
                { "metadata": { "name": "a1" }, "spec": { "source": { "repoURL": "https://github.com/myorg/x" } } },
                { "metadata": { "name": "a2" }, "spec": { "source": { "repoURL": "https://github.com/otherorg/y" } } },
                { "metadata": { "name": "a3" }, "spec": { "source": { "repoURL": "https://github.com/myorg/z" } } }
            ]
        });
        let deps = find_apps_matching_prefix(&apps, "https://github.com/myorg");
        assert_eq!(deps, vec!["a1".to_string(), "a3".to_string()]);
    }

    #[test]
    fn find_apps_matching_prefix_skips_apps_without_repo_url() {
        let apps = json!({
            "items": [
                { "metadata": { "name": "helm-only" }, "spec": { "source": { "chart": "x", "repoURL": "https://charts.example/" } } },
                { "metadata": { "name": "missing-source" }, "spec": {} }
            ]
        });
        let deps = find_apps_matching_prefix(&apps, "https://github.com/myorg");
        assert!(deps.is_empty());
    }
}
