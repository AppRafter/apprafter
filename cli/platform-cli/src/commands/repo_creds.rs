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
    // D11 / 2.22a: before the wizard, which collects a production PAT
    // over four prompts and only then discovers there is no kubectl,
    // no cluster, or a duplicate name. `rotate` below already resolves
    // the cluster first — the correct order was one function away.
    cli_core::tools::preflight_tool(&cli_core::tools::KUBECTL, "apprafter repo creds add")?;

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
        return Err(duplicate_credential_error(name));
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

    for line in add_summary_lines(name, &git_prefix, registry_host.as_deref()) {
        println!("{line}");
    }
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
    let inputs = wizard_inputs(name, url_prefix, auth_type, username, token, no_validate);
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
    println!("{}", list_output(&creds));
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
    .ok_or_else(|| not_found_error(name))?;
    print_creds_detail(&cred);
    Ok(())
}

pub fn rotate(name: &str, token: Option<String>, no_validate: bool) -> Result<()> {
    cli_core::tools::preflight_tool(&cli_core::tools::KUBECTL, "apprafter repo creds rotate")?;
    let kc = ensure_kubeconfig_tempfile()?;
    let cred = kubectl_get_json(
        "sourcecredential",
        Some(name),
        Some(SOURCECRED_NAMESPACE),
        kc.path(),
    )?
    .ok_or_else(|| not_found_error(name))?;

    let username = annotation_or(&cred, GIT_USERNAME_ANNOTATION, "git");
    let auth_type = annotation_or(&cred, AUTH_TYPE_ANNOTATION, "pat");
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
    .ok_or_else(|| not_found_error(name))?;

    // Best-effort reverse-dependency lookup for the gate below: which
    // Argo CD Application(s) point under one of this credential's repo
    // prefixes? Skipped entirely under `--force` (the gate ignores it
    // anyway, and this costs a cluster round-trip per prefix).
    let mut deps: Vec<String> = Vec::new();
    if !force {
        for prefix in &repo_prefixes_of(&cred) {
            deps.extend(find_dependent_applications(
                &normalize_repo_url(prefix),
                kc.path(),
            )?);
        }
    }

    match removal_gate(name, deps, force, yes, io::stdin().is_terminal())? {
        RemovalDecision::Proceed => {}
        RemovalDecision::ConfirmInteractively => {
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
            "SSH-key auth is not supported yet — pass `--type pat` or `--type basic` for now"
                .into(),
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

/// Where the token for this invocation comes from — decided purely from
/// the flag value plus whether stdin can host a masked prompt.
#[derive(Debug, PartialEq, Eq)]
enum TokenSource {
    Provided(String),
    Prompt,
}

/// Pure half of [`resolve_token`]: flag/TTY arbitration, no I/O.
fn token_source(token: Option<String>, stdin_is_tty: bool) -> Result<TokenSource> {
    if let Some(t) = token {
        if t.is_empty() {
            return Err(CliError::Other(
                "empty `--token` — pass a non-empty token or omit the flag for an interactive prompt"
                    .into(),
            ));
        }
        return Ok(TokenSource::Provided(t));
    }
    if !stdin_is_tty {
        return Err(CliError::Other(
            "`--token` not provided and stdin is not a TTY — pass `--token <value>` or run from \
             an interactive shell"
                .into(),
        ));
    }
    Ok(TokenSource::Prompt)
}

fn resolve_token(token: Option<String>) -> Result<String> {
    if let TokenSource::Provided(t) = token_source(token, io::stdin().is_terminal())? {
        return Ok(t);
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

/// Rendered `show` body, one line per emitted `println!`. Pure so the
/// detail view can be asserted without a cluster.
fn creds_detail_lines(cred: &Value) -> Vec<String> {
    let name = cred
        .pointer("/metadata/name")
        .and_then(Value::as_str)
        .unwrap_or("?");
    let mut lines = vec![
        format!("SourceCredential {SOURCECRED_NAMESPACE}/{name}"),
        format!(
            "  Repo prefixes:  {}",
            join_or_dash(&repo_prefixes_of(cred))
        ),
        format!("  Registry hosts: {}", join_or_dash(&hosts_of(cred))),
        format!("  Status:         {}", status_summary(cred)),
    ];
    if let Some(conds) = cred.pointer("/status/conditions").and_then(Value::as_array) {
        for c in conds {
            let t = c.get("type").and_then(Value::as_str).unwrap_or("?");
            let s = c.get("status").and_then(Value::as_str).unwrap_or("?");
            let r = c.get("reason").and_then(Value::as_str).unwrap_or("");
            let m = c.get("message").and_then(Value::as_str).unwrap_or("");
            lines.push(format!("    - {t}={s} ({r}) {m}"));
        }
    }
    lines.push(
        "  Material:       sealed — unreadable from the CLI (no cluster private key).".into(),
    );
    lines
}

fn print_creds_detail(cred: &Value) {
    for line in creds_detail_lines(cred) {
        println!("{line}");
    }
}

/// `add`'s success block, one line per emitted `println!` (the empty
/// string is the blank separator line).
fn add_summary_lines(name: &str, git_prefix: &str, registry_host: Option<&str>) -> Vec<String> {
    vec![
        format!("✓ SourceCredential '{name}' registered (material sealed)."),
        format!("  Repo prefix:   {git_prefix}"),
        match registry_host {
            Some(h) => format!("  Registry host: {h}  (inferred)"),
            None => {
                "  Registry host: —  (git-only; pass a github.com repo to infer ghcr.io)".into()
            }
        },
        String::new(),
        "The operator derives the Argo repo-cred + workload pull-secret. Check validity with:"
            .into(),
        format!("  apprafter repo creds show {name}"),
    ]
}

/// `list`'s whole stdout body — the table, or the empty-state hint.
fn list_output(creds: &[Value]) -> String {
    if creds.is_empty() {
        return format!(
            "No SourceCredentials registered in {SOURCECRED_NAMESPACE}.\nHint: run `apprafter \
             repo creds add <name> --url-prefix <url> --token <pat>` to register the first entry."
        );
    }
    let rows: Vec<CredsRow> = creds.iter().map(creds_row).collect();
    Table::new(&rows).to_string()
}

/// The one "no such credential" error `show` / `rotate` / `remove` all
/// raise, so the three read-then-act paths cannot drift apart.
fn not_found_error(name: &str) -> CliError {
    CliError::Other(format!(
        "SourceCredential '{name}' not found in {SOURCECRED_NAMESPACE}."
    ))
}

/// `add` refuses to clobber an existing CR: the token would be re-sealed
/// but the CR's coverage silently kept, so point the operator at the two
/// subcommands that do the right thing.
fn duplicate_credential_error(name: &str) -> CliError {
    CliError::Other(format!(
        "SourceCredential '{name}' already exists in {SOURCECRED_NAMESPACE}. Run \
         `apprafter repo creds rotate {name}` to replace the token, or \
         `apprafter repo creds remove {name}` then `add` to recreate."
    ))
}

/// Read a `metadata.annotations` entry whose key contains `/`, falling
/// back to `default` when absent or non-string.
fn annotation_or(cred: &Value, key: &str, default: &str) -> String {
    cred.pointer(&format!("/metadata/annotations/{}", esc(key)))
        .and_then(Value::as_str)
        .unwrap_or(default)
        .to_string()
}

/// Map the CLI args of `repo creds add` onto wizard pre-fills: an empty
/// positional/flag means "not supplied", so the wizard prompts for it
/// instead of pre-filling an empty default the operator must delete.
fn wizard_inputs(
    name: &str,
    url_prefix: &str,
    auth_type: &str,
    username: &str,
    token: Option<String>,
    no_validate: bool,
) -> crate::commands::repo_creds_wizard::WizardInputs {
    crate::commands::repo_creds_wizard::WizardInputs {
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
    }
}

#[derive(Debug, PartialEq, Eq)]
enum RemovalDecision {
    /// Delete straight away — `--force`, or `--yes` with no dependents.
    Proceed,
    /// Ask the operator first (interactive shell, no `--yes`).
    ConfirmInteractively,
}

/// The two client-side gates in front of a whole-credential delete:
/// the reverse-dependency refusal and the confirmation requirement.
/// `--force` bypasses both; `--yes` only the confirmation.
///
/// Complementary to — never a substitute for — the operator's
/// MigrationPlan gate, which is the authoritative, actor-agnostic
/// control on a coverage-NARROWING edit (dropping a repoPrefix /
/// registry host while keeping the CR). This one just fast-fails the
/// destructive whole-CR delete before it reaches the cluster.
fn removal_gate(
    name: &str,
    mut deps: Vec<String>,
    force: bool,
    yes: bool,
    stdin_is_tty: bool,
) -> Result<RemovalDecision> {
    if !force {
        deps.sort();
        deps.dedup();
        if !deps.is_empty() {
            return Err(CliError::Other(format!(
                "SourceCredential '{name}' is used by {n} Application(s): {names}. \
                 Re-point them or pass `--force` to delete anyway. \
                 (Note: this is a fast client-side check on a full delete. \
                 A coverage-NARROWING edit — dropping a repoPrefix / registry \
                 host on the CR — is gated instead by an auto-created \
                 MigrationPlan that you approve with `apprafter migration approve`.)",
                n = deps.len(),
                names = deps.join(", ")
            )));
        }
    }
    if !yes && !force {
        if !stdin_is_tty {
            return Err(CliError::Other(
                "non-interactive shell — pass `--yes` (or `--force`) to skip the confirmation prompt"
                    .into(),
            ));
        }
        return Ok(RemovalDecision::ConfirmInteractively);
    }
    Ok(RemovalDecision::Proceed)
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
    fn parse_auth_type_rejects_ssh_as_unsupported() {
        let err = parse_auth_type("ssh").unwrap_err().to_string();
        // Neutral phrasing — no internal roadmap/phase reference leaks.
        assert!(err.contains("not supported"), "{err}");
        assert!(err.contains("--type pat"), "{err}");
        assert!(!err.contains("Phase"), "roadmap ref leaked: {err}");
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

    #[test]
    fn find_apps_matching_prefix_on_an_empty_list_response() {
        // `kubectl get application -o json` on a cluster with no Argo
        // Applications returns an object without `items`. That must read
        // as "no dependents", not blow up the `remove` gate.
        assert!(find_apps_matching_prefix(&json!({}), "https://github.com/myorg").is_empty());
        assert!(find_apps_matching_prefix(&json!({ "items": [] }), "https://x").is_empty());
    }

    // ---------------------------------------------------------------
    // Token source arbitration (`--token` flag vs. masked prompt).
    // ---------------------------------------------------------------

    #[test]
    fn token_source_uses_the_flag_even_on_a_tty() {
        // An explicit `--token` must never be overridden by a prompt,
        // otherwise scripted runs on an interactive terminal would hang.
        assert_eq!(
            token_source(Some("ghp_value".into()), true).unwrap(),
            TokenSource::Provided("ghp_value".into())
        );
        assert_eq!(
            token_source(Some("ghp_value".into()), false).unwrap(),
            TokenSource::Provided("ghp_value".into())
        );
    }

    #[test]
    fn token_source_rejects_an_empty_flag_instead_of_prompting() {
        // `--token ""` (a shell variable that expanded to nothing) is an
        // operator error: seal it and the credential is silently broken.
        // It must fail loudly, and must NOT degrade into a prompt.
        let err = token_source(Some(String::new()), true)
            .unwrap_err()
            .to_string();
        assert!(err.contains("empty"), "{err}");
        assert!(err.contains("--token"), "{err}");
    }

    #[test]
    fn token_source_needs_a_tty_when_the_flag_is_absent() {
        // No flag + no TTY has no way to obtain a token: fail with the
        // remedy rather than blocking on a prompt nobody can answer.
        let err = token_source(None, false).unwrap_err().to_string();
        assert!(err.contains("TTY"), "{err}");
        assert_eq!(token_source(None, true).unwrap(), TokenSource::Prompt);
    }

    // ---------------------------------------------------------------
    // `remove` gates.
    // ---------------------------------------------------------------

    #[test]
    fn removal_gate_refuses_while_applications_still_depend_on_the_credential() {
        // Deleting a credential out from under live Applications breaks
        // their next sync. The refusal counts DEDUPED names (one app can
        // match two of the credential's prefixes) and lists them sorted.
        let deps = vec!["landing".into(), "api".into(), "landing".into()];
        let err = removal_gate("acme", deps, false, true, true)
            .unwrap_err()
            .to_string();
        assert!(err.contains("2 Application"), "{err}");
        assert!(err.contains("api, landing"), "{err}");
        assert!(err.contains("--force"), "{err}");
    }

    #[test]
    fn removal_gate_force_bypasses_both_gates() {
        // `--force` is the documented escape hatch: it skips the
        // dependency refusal AND the confirmation, even with no TTY.
        assert_eq!(
            removal_gate("acme", vec!["landing".into()], true, false, false).unwrap(),
            RemovalDecision::Proceed
        );
    }

    #[test]
    fn removal_gate_yes_skips_only_the_confirmation() {
        // `--yes` answers the prompt in advance; it is NOT an override of
        // the reverse-dependency check (that is `--force`'s job).
        assert_eq!(
            removal_gate("acme", Vec::new(), false, true, false).unwrap(),
            RemovalDecision::Proceed
        );
        assert!(removal_gate("acme", vec!["landing".into()], false, true, true).is_err());
    }

    #[test]
    fn removal_gate_confirms_interactively_or_demands_a_flag() {
        assert_eq!(
            removal_gate("acme", Vec::new(), false, false, true).unwrap(),
            RemovalDecision::ConfirmInteractively
        );
        // Without a TTY there is nobody to confirm — a CI run must be
        // told to pass `--yes` rather than silently deleting.
        let err = removal_gate("acme", Vec::new(), false, false, false)
            .unwrap_err()
            .to_string();
        assert!(err.contains("--yes"), "{err}");
    }

    // ---------------------------------------------------------------
    // Reading the CR back.
    // ---------------------------------------------------------------

    #[test]
    fn esc_escapes_json_pointer_tokens_in_the_right_order() {
        // RFC 6901: `~` must be escaped BEFORE `/`, otherwise the `~1`
        // produced for a slash gets re-escaped into `~01` and the
        // annotation lookup silently misses.
        assert_eq!(esc("apprafter.io/auth-type"), "apprafter.io~1auth-type");
        assert_eq!(esc("a~b/c"), "a~0b~1c");
    }

    #[test]
    fn annotation_or_reads_slash_containing_keys_with_a_fallback() {
        // `rotate` re-seals using the username recorded at `add` time; if
        // the lookup missed, every rotation would silently rewrite the
        // material under the default `git` user.
        let cred = json!({
            "metadata": { "annotations": {
                "apprafter.io/git-username": "deploy-bot",
                "apprafter.io/auth-type": "basic"
            } }
        });
        assert_eq!(
            annotation_or(&cred, GIT_USERNAME_ANNOTATION, "git"),
            "deploy-bot"
        );
        assert_eq!(annotation_or(&cred, AUTH_TYPE_ANNOTATION, "pat"), "basic");
        // Missing annotations (a hand-written CR) fall back.
        assert_eq!(
            annotation_or(&json!({ "metadata": {} }), GIT_USERNAME_ANNOTATION, "git"),
            "git"
        );
        // Non-string annotation values fall back rather than panicking.
        let odd = json!({ "metadata": { "annotations": { "apprafter.io/auth-type": 7 } } });
        assert_eq!(annotation_or(&odd, AUTH_TYPE_ANNOTATION, "pat"), "pat");
    }

    #[test]
    fn repo_prefixes_and_hosts_ignore_non_string_entries() {
        let cred = json!({
            "spec": {
                "git": { "repoPrefixes": ["github.com/acme/", 42] },
                "registry": { "hosts": ["ghcr.io/acme/", null] }
            }
        });
        assert_eq!(
            repo_prefixes_of(&cred),
            vec!["github.com/acme/".to_string()]
        );
        assert_eq!(hosts_of(&cred), vec!["ghcr.io/acme/".to_string()]);
        // A git-only credential has no registry block at all.
        assert!(hosts_of(&json!({ "spec": { "git": {} } })).is_empty());
    }

    #[test]
    fn status_summary_reports_each_half_independently() {
        // A git-only credential never gets a RegistryPresent condition;
        // the summary must not invent one.
        let git_only = json!({
            "status": { "conditions": [{ "type": "GitPresent", "status": "True" }] }
        });
        assert_eq!(status_summary(&git_only), "git:True");

        let registry_failing = json!({
            "status": { "conditions": [{ "type": "RegistryPresent", "status": "False" }] }
        });
        assert_eq!(status_summary(&registry_failing), "registry:False");

        // Conditions exist but none of the two the summary reports on —
        // still "pending", not an empty column.
        let unrelated = json!({
            "status": { "conditions": [{ "type": "GitValid", "status": "True" }] }
        });
        assert_eq!(status_summary(&unrelated), "pending");
    }

    #[test]
    fn normalize_repo_url_restores_the_scheme_for_comparison() {
        // `spec.git.repoPrefixes` is stored scheme-less; Argo CD's
        // `spec.source.repoURL` is not. The comparison form adds https://
        // and drops the trailing slash so `.starts_with` can match.
        assert_eq!(
            normalize_repo_url("github.com/acme/"),
            "https://github.com/acme"
        );
        // A prefix that already carries a scheme keeps it (self-hosted
        // http:// or ssh:// remotes must not be rewritten to https).
        assert_eq!(
            normalize_repo_url("http://git.internal/acme/"),
            "http://git.internal/acme"
        );
    }

    #[test]
    fn any_credential_covers_matches_a_scheme_qualified_prefix() {
        // Prefixes typed with a scheme must still match — the coverage
        // check normalises both sides rather than comparing raw strings.
        let creds = vec![json!({
            "spec": { "git": { "repoPrefixes": ["other.example/x/", "https://github.com/myorg/"] } }
        })];
        assert!(any_credential_covers(
            &creds,
            "https://github.com/myorg/repo"
        ));
        // A different org is not covered, and neither is the same path on
        // a different host.
        assert!(!any_credential_covers(
            &creds,
            "https://github.com/other/repo"
        ));
        assert!(!any_credential_covers(
            &creds,
            "https://gitlab.com/myorg/repo"
        ));
    }

    // ---------------------------------------------------------------
    // Rendering.
    // ---------------------------------------------------------------

    #[test]
    fn join_or_dash_marks_an_empty_column() {
        assert_eq!(join_or_dash(&[]), "—");
        assert_eq!(join_or_dash(&["a".to_string(), "b".to_string()]), "a, b");
    }

    #[test]
    fn creds_row_falls_back_for_a_nameless_credential() {
        let row = creds_row(&json!({ "spec": { "git": {} } }));
        assert_eq!(row.name, "?");
        assert_eq!(row.repo_prefixes, "—");
        assert_eq!(row.hosts, "—");
        assert_eq!(row.status, "pending");
    }

    #[test]
    fn list_output_renders_one_row_per_credential() {
        let creds = vec![
            json!({
                "metadata": { "name": "acme" },
                "spec": {
                    "git": { "repoPrefixes": ["github.com/acme/"] },
                    "registry": { "hosts": ["ghcr.io/acme/"] }
                },
                "status": { "conditions": [
                    { "type": "GitPresent", "status": "True" },
                    { "type": "RegistryPresent", "status": "True" }
                ] }
            }),
            json!({
                "metadata": { "name": "selfhosted" },
                "spec": { "git": { "repoPrefixes": ["git.internal/team/"] } }
            }),
        ];
        let out = list_output(&creds);
        for header in ["NAME", "REPO PREFIXES", "REGISTRY HOSTS", "STATUS"] {
            assert!(out.contains(header), "missing column {header}:\n{out}");
        }
        assert!(out.contains("acme"), "{out}");
        assert!(out.contains("git.internal/team/"), "{out}");
        assert!(out.contains("git:True registry:True"), "{out}");
        // The git-only credential renders a dash, not a blank cell.
        assert!(out.contains('—'), "{out}");
    }

    #[test]
    fn list_output_empty_state_is_a_hint_not_a_bare_table() {
        let out = list_output(&[]);
        assert!(out.contains(SOURCECRED_NAMESPACE), "{out}");
        assert!(out.contains("apprafter repo creds add"), "{out}");
        // An empty table header would read as "something is here" — the
        // empty state must suppress it entirely.
        assert!(!out.contains("REPO PREFIXES"), "{out}");
    }

    #[test]
    fn creds_detail_lines_render_every_condition() {
        let cred = json!({
            "metadata": { "name": "acme" },
            "spec": { "git": { "repoPrefixes": ["github.com/acme/"] } },
            "status": { "conditions": [
                { "type": "GitPresent", "status": "True", "reason": "Sealed", "message": "ok" },
                { "type": "GitValid", "status": "False" }
            ] }
        });
        let lines = creds_detail_lines(&cred);
        assert!(lines[0].contains("apprafter-system/acme"), "{lines:?}");
        assert!(
            lines
                .iter()
                .any(|l| l.contains("GitPresent=True (Sealed) ok")),
            "{lines:?}"
        );
        // A condition without reason/message still renders — the
        // operator must see that GitValid is False.
        assert!(
            lines.iter().any(|l| l.contains("GitValid=False")),
            "{lines:?}"
        );
        // The material is never printed; the last line says so.
        let last = lines.last().unwrap();
        assert!(last.contains("sealed"), "{last}");
    }

    #[test]
    fn creds_detail_lines_without_status_emit_no_condition_lines() {
        let lines = creds_detail_lines(&json!({ "metadata": { "name": "acme" } }));
        assert_eq!(lines.len(), 5, "{lines:?}");
        assert!(lines[3].contains("pending"), "{lines:?}");
        assert!(!lines[0].contains('?'), "{lines:?}");
    }

    #[test]
    fn add_summary_lines_flag_inferred_vs_git_only() {
        let inferred = add_summary_lines("acme", "github.com/acme/", Some("ghcr.io/acme/"));
        let joined = inferred.join("\n");
        assert!(joined.contains("ghcr.io/acme/"), "{joined}");
        assert!(joined.contains("(inferred)"), "{joined}");
        // Point the operator at the command that reports validity.
        assert!(
            joined.contains("apprafter repo creds show acme"),
            "{joined}"
        );

        let git_only = add_summary_lines("acme", "gitlab.com/acme/", None).join("\n");
        assert!(!git_only.contains("(inferred)"), "{git_only}");
        assert!(git_only.contains("git-only"), "{git_only}");
        // Explain how to get the registry half rather than silently
        // dropping it.
        assert!(git_only.contains("ghcr.io"), "{git_only}");
    }

    // ---------------------------------------------------------------
    // Error surfaces + wizard hand-off.
    // ---------------------------------------------------------------

    #[test]
    fn credential_errors_name_the_remedy() {
        let missing = not_found_error("acme").to_string();
        assert!(missing.contains("acme"), "{missing}");
        assert!(missing.contains(SOURCECRED_NAMESPACE), "{missing}");

        // `add` on an existing name must not clobber it — the message
        // routes to the two subcommands that do the right thing.
        let dup = duplicate_credential_error("acme").to_string();
        assert!(dup.contains("repo creds rotate acme"), "{dup}");
        assert!(dup.contains("repo creds remove acme"), "{dup}");
    }

    #[test]
    fn wizard_inputs_treat_empty_arguments_as_unset() {
        // clap hands us "" for an omitted positional/flag. Passing that
        // through as a pre-filled default would make the operator delete
        // an empty default before typing; None makes the wizard prompt.
        let blank = wizard_inputs("", "", "pat", "git", None, false);
        assert!(blank.name.is_none());
        assert!(blank.url_prefix.is_none());
        assert_eq!(blank.auth_type.as_deref(), Some("pat"));
        assert_eq!(blank.username.as_deref(), Some("git"));
        assert!(blank.token.is_none());

        let given = wizard_inputs(
            "acme",
            "https://github.com/acme",
            "basic",
            "deploy",
            Some("secret".into()),
            true,
        );
        assert_eq!(given.name.as_deref(), Some("acme"));
        assert_eq!(given.url_prefix.as_deref(), Some("https://github.com/acme"));
        assert_eq!(given.token.as_deref(), Some("secret"));
        assert!(given.no_validate);
    }

    // ---------------------------------------------------------------
    // Sealed-material + CR shape.
    // ---------------------------------------------------------------

    #[test]
    fn material_data_uses_the_keys_the_operator_reads() {
        // The operator (and Argo's repo-creds Secret) look up exactly
        // `username` / `password`; renaming either breaks both derived
        // halves with no client-side error.
        let m = material_data("deploy", "ghp_token");
        assert_eq!(m.get("username").map(Vec::as_slice), Some(&b"deploy"[..]));
        assert_eq!(
            m.get("password").map(Vec::as_slice),
            Some(&b"ghp_token"[..])
        );
        assert_eq!(m.len(), 2);
    }

    #[test]
    fn build_source_credential_cr_shares_one_material_between_both_halves() {
        // The launch default is a single PAT: git and registry must
        // reference the SAME sealed secret, and the auth-type annotation
        // must round-trip for `rotate` to re-validate the new token.
        let cr = build_source_credential_cr(
            "acme",
            "github.com/acme/",
            Some("ghcr.io/acme/"),
            "srccred-acme-material",
            "deploy",
            "basic",
        );
        assert_eq!(
            cr.pointer("/spec/registry/backend/sealedSecretRef/name"),
            cr.pointer("/spec/git/backend/sealedSecretRef/name")
        );
        assert_eq!(
            cr.pointer("/metadata/annotations/apprafter.io~1auth-type")
                .and_then(Value::as_str),
            Some("basic")
        );
        assert_eq!(
            cr.pointer("/metadata/labels/apprafter.io~1managed-by")
                .and_then(Value::as_str),
            Some("apprafter")
        );
    }

    #[test]
    fn infer_registry_host_requires_an_org_segment() {
        // `https://github.com` with no org normalises to `github.com/`;
        // inferring `ghcr.io//` from it would produce a host prefix that
        // matches every GHCR image in the world.
        assert_eq!(infer_registry_host("github.com/"), None);
    }

    #[test]
    fn validate_token_format_rejects_an_empty_basic_password() {
        // `--type basic` skips the provider shape check, but an empty
        // password would still be sealed and silently fail every clone.
        let err = validate_token_format("", &AuthType::Basic)
            .unwrap_err()
            .to_string();
        assert!(err.contains("--token"), "{err}");
    }

    #[test]
    fn validate_token_format_rejects_a_short_gitlab_pat() {
        let err = validate_token_format("glpat-tooshort", &AuthType::Pat)
            .unwrap_err()
            .to_string();
        assert!(err.contains("GitLab"), "{err}");
        assert!(err.contains("no-validate"), "{err}");
        // 20 characters after the prefix is the accepted minimum.
        let ok = format!("glpat-{}", "x".repeat(14));
        assert_eq!(ok.len(), 20);
        assert!(validate_token_format(&ok, &AuthType::Pat).is_ok());
    }

    #[test]
    fn validate_token_format_fine_grained_pat_boundary_is_80() {
        // Exactly 80 passes, 79 does not — the check is `< 80`, and an
        // off-by-one here rejects real GitHub tokens.
        let at_boundary = format!("github_pat_{}", "x".repeat(69));
        assert_eq!(at_boundary.len(), 80);
        assert!(validate_token_format(&at_boundary, &AuthType::Pat).is_ok());
        let below = format!("github_pat_{}", "x".repeat(68));
        assert!(validate_token_format(&below, &AuthType::Pat).is_err());
    }

    #[test]
    fn valid_credential_covers_needs_one_credential_to_satisfy_both_halves() {
        // Coverage and validity must come from the SAME credential: a
        // validated credential for another org must not vouch for a
        // declared-but-unvalidated one.
        let creds = vec![
            json!({
                "spec": { "git": { "repoPrefixes": ["github.com/myorg/"] } },
                "status": { "conditions": [{ "type": "GitValid", "status": "False" }] }
            }),
            json!({
                "spec": { "git": { "repoPrefixes": ["github.com/elsewhere/"] } },
                "status": { "conditions": [{ "type": "GitValid", "status": "True" }] }
            }),
        ];
        let repo = "https://github.com/myorg/repo";
        assert!(any_credential_covers(&creds, repo));
        assert!(!valid_credential_covers(&creds, repo));
    }

    #[test]
    fn normalize_git_prefix_collapses_repeated_trailing_slashes() {
        // `https://github.com/acme//` pasted from a browser must produce
        // the same stored prefix as the clean form, or the operator's
        // prefix match silently never fires.
        assert_eq!(
            normalize_git_prefix("https://github.com/acme//"),
            "github.com/acme/"
        );
        assert_eq!(normalize_git_prefix("github.com/acme"), "github.com/acme/");
    }

    #[test]
    fn find_apps_matching_prefix_skips_a_matching_app_without_a_name() {
        // A dependent we cannot name cannot be reported; it must not
        // surface as an empty entry in the refusal message.
        let apps = json!({
            "items": [
                { "spec": { "source": { "repoURL": "https://github.com/myorg/x" } } },
                { "metadata": { "name": "named" }, "spec": { "source": { "repoURL": "https://github.com/myorg/y" } } }
            ]
        });
        assert_eq!(
            find_apps_matching_prefix(&apps, "https://github.com/myorg"),
            vec!["named".to_string()]
        );
    }

    #[test]
    fn validate_dns_1123_rejects_edge_dashes_and_overlong_names() {
        assert!(validate_dns_1123("-acme").is_err());
        assert!(validate_dns_1123("acme-").is_err());
        assert!(validate_dns_1123(&"a".repeat(65)).is_err());
        assert!(validate_dns_1123("a").is_ok());
        assert!(validate_dns_1123("a-9").is_ok());
    }
}
