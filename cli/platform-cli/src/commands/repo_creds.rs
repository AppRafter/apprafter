// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! `apprafter repo creds …` — manage Argo CD repo creds
//! Secrets for private user repos. Track B.1.79a part 5.
//!
//! Argo CD's documented contract (`docs/operator-manual/
//! declarative-setup.md#credential-templates`):
//!
//! ```yaml
//! apiVersion: v1
//! kind: Secret
//! metadata:
//!   name: <friendly-name>
//!   namespace: argocd
//!   labels:
//!     argocd.argoproj.io/secret-type: repo-creds
//! stringData:
//!   url: <url-prefix>
//!   username: <user>
//!   password: <token-or-password>
//! ```
//!
//! Argo CD's repo-server scans the `argocd` namespace for
//! Secrets labeled `argocd.argoproj.io/secret-type: repo-creds`
//! and uses whichever entry's `url` field is a prefix-match for
//! an Application's `spec.source.repoURL`. So registering
//! `https://github.com/myorg` makes every Application pointing
//! to `https://github.com/myorg/<any>` inherit those creds
//! automatically.

use std::io::{self, IsTerminal, Write as _};
use std::path::Path;
use std::process::Command;

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use cli_core::{CliError, Result};
use serde_json::{json, Value};
use tabled::{Table, Tabled};

use crate::commands::k8s_helpers::{ensure_kubeconfig_tempfile, kubectl_get_json};

const ARGOCD_NAMESPACE: &str = "argocd";
const SECRET_TYPE_LABEL_VALUE: &str = "repo-creds";
const SECRET_TYPE_SELECTOR: &str = "argocd.argoproj.io/secret-type=repo-creds";
const APPRAFTER_MANAGED_LABEL_KEY: &str = "apprafter.io/managed-by";
const APPRAFTER_CRED_NAME_LABEL_KEY: &str = "apprafter.io/cred-name";

#[derive(Tabled)]
struct CredsRow {
    #[tabled(rename = "NAME")]
    name: String,
    #[tabled(rename = "URL PREFIX")]
    url_prefix: String,
    #[tabled(rename = "TYPE")]
    auth_type: String,
    #[tabled(rename = "USERNAME")]
    username: String,
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
    // Wizard gate — TTY + not opted out. Wizard fills every
    // missing field via inquire prompts; the token prompt is
    // masked. Pre-fills come from the flag values above so an
    // operator passing `--name x --url-prefix y` only gets
    // prompted for type/username/token.
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
    let existing = kubectl_get_json("secret", Some(name), Some(ARGOCD_NAMESPACE), kc.path())?;
    if existing.is_some() {
        return Err(CliError::Other(format!(
            "Secret '{name}' already exists in namespace {ARGOCD_NAMESPACE}. Run \
             `apprafter repo creds rotate {name}` to replace the token, or \
             `apprafter repo creds remove {name}` + `add` to recreate from scratch."
        )));
    }

    let manifest = build_repo_creds_secret(name, url_prefix, &auth, username, &token);
    apply_secret_manifest(&manifest, kc.path())?;

    println!("✓ Repo creds '{name}' registered for URL prefix '{url_prefix}'.");
    println!("  Type:     {auth_type}");
    println!("  Username: {username}");
    println!();
    println!(
        "Every Argo CD Application with `spec.source.repoURL` starting with '{url_prefix}' \
         will inherit these creds automatically."
    );
    Ok(())
}

/// Wizard entry point — gathers missing fields via inquire
/// prompts (masked Password for the token) and re-dispatches
/// into the non-interactive `add` path with
/// `no_interactive=true` to avoid recursion.
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
    let secrets = fetch_repo_creds_secrets(kc.path())?;

    if secrets.is_empty() {
        println!("No repo creds registered in namespace {ARGOCD_NAMESPACE}.");
        println!(
            "Hint: run `apprafter repo creds add <name> --url-prefix <url> --type pat \
             --token <pat>` to register the first entry."
        );
        return Ok(());
    }

    let rows: Vec<CredsRow> = secrets.iter().map(creds_row).collect();
    println!("{}", Table::new(&rows));
    Ok(())
}

pub fn show(name: &str) -> Result<()> {
    let kc = ensure_kubeconfig_tempfile()?;
    let secret = kubectl_get_json("secret", Some(name), Some(ARGOCD_NAMESPACE), kc.path())?
        .ok_or_else(|| {
            CliError::Other(format!(
                "Repo creds '{name}' not found in namespace {ARGOCD_NAMESPACE}."
            ))
        })?;

    print_creds_detail(&secret);
    Ok(())
}

pub fn rotate(name: &str, token: Option<String>, no_validate: bool) -> Result<()> {
    let kc = ensure_kubeconfig_tempfile()?;
    let secret = kubectl_get_json("secret", Some(name), Some(ARGOCD_NAMESPACE), kc.path())?
        .ok_or_else(|| {
            CliError::Other(format!(
                "Repo creds '{name}' not found in namespace {ARGOCD_NAMESPACE}."
            ))
        })?;

    let auth_type = secret
        .pointer("/metadata/annotations/apprafter.io~1auth-type")
        .and_then(Value::as_str)
        .unwrap_or("pat")
        .to_string();
    let auth = parse_auth_type(&auth_type)?;
    let new_token = resolve_token(token)?;

    if !no_validate {
        validate_token_format(&new_token, &auth)?;
    }

    // Patch Secret's `data.password` in-place rather than
    // recreating — repo-server holds a cached resourceVersion
    // pointer and a full recreate would cause a brief reconnect
    // window. Base64-encode because Secret's `data` (not
    // `stringData`) expects encoded values.
    let encoded = B64.encode(new_token.as_bytes());
    let body = format!(r#"{{"data":{{"password":"{encoded}"}}}}"#);

    crate::commands::k8s_helpers::kubectl_merge_patch(
        "secret",
        name,
        Some(ARGOCD_NAMESPACE),
        None,
        &body,
        kc.path(),
    )?;

    println!("✓ Token for repo creds '{name}' rotated.");
    println!("Argo CD repo-server will pick up the new token on the next pull cycle.");
    Ok(())
}

pub fn remove(name: &str, force: bool, yes: bool) -> Result<()> {
    let kc = ensure_kubeconfig_tempfile()?;
    let secret = kubectl_get_json("secret", Some(name), Some(ARGOCD_NAMESPACE), kc.path())?
        .ok_or_else(|| {
            CliError::Other(format!(
                "Repo creds '{name}' not found in namespace {ARGOCD_NAMESPACE}."
            ))
        })?;

    let url_prefix = secret
        .pointer("/data/url")
        .and_then(Value::as_str)
        .and_then(|b64| {
            B64.decode(b64)
                .ok()
                .and_then(|bytes| String::from_utf8(bytes).ok())
        })
        .unwrap_or_default();

    if !force {
        // Dependency check: refuse if there are Argo CD
        // Application(s) whose `spec.source.repoURL` matches
        // these creds by prefix. The operator can override
        // with `--force` when they know what they're doing
        // (e.g. migrating to another creds entry).
        let deps = find_dependent_applications(&url_prefix, kc.path())?;
        if !deps.is_empty() {
            return Err(CliError::Other(format!(
                "Repo creds '{name}' are used by {n} Application(s): {names}. \
                 Re-register them with different creds (`apprafter app remove`/`add`) \
                 or pass `--force` to delete the creds anyway.",
                n = deps.len(),
                names = deps.join(", ")
            )));
        }
    }

    if !yes && !force {
        if !io::stdin().is_terminal() {
            return Err(CliError::Other(
                "non-interactive shell — pass `--yes` (or `--force`) to skip the confirmation prompt".into(),
            ));
        }
        println!("Delete repo creds '{name}' (URL prefix: {url_prefix})?");
        let confirmed = inquire::Confirm::new("Confirm?")
            .with_default(false)
            .prompt()
            .map_err(|e| CliError::Other(format!("confirmation prompt: {e}")))?;
        if !confirmed {
            println!("Cancelled.");
            return Ok(());
        }
    }

    let out = Command::new("kubectl")
        .arg("delete")
        .arg("secret")
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
    println!("✓ Repo creds '{name}' deleted.");
    Ok(())
}

// =========================================================================
// PURE HELPERS — testable without kube::Client / git binary / network.
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

/// Provider-aware token regex check. **Best-effort** — the
/// operator can pass `--no-validate` to skip when running
/// self-hosted Gitea/Forgejo with custom token shapes.
/// Detection priority: GitHub fine-grained PAT > GitHub classic
/// PAT > GitLab PAT > fallback to a 20+-char generic check.
pub fn validate_token_format(token: &str, auth: &AuthType) -> Result<()> {
    if matches!(auth, AuthType::Basic) {
        // Basic auth tokens are arbitrary passwords — no
        // shape to validate. Just non-empty.
        if token.is_empty() {
            return Err(CliError::Other("`--token` cannot be empty".into()));
        }
        return Ok(());
    }
    // PAT — try the known formats.
    if token.starts_with("github_pat_") {
        // Fine-grained GitHub PAT. Documented prefix +
        // 80+ char body. Minimum length check protects
        // against a partial paste.
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
        // Classic GitHub PAT. Documented `ghp_` + 36-char
        // alphanumeric body. Total 40 chars.
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
        // GitLab PAT. Documented `glpat-` + 20-char body
        // (but operators sometimes truncate; lenient
        // minimum-length check).
        if token.len() < 20 {
            return Err(CliError::Other(format!(
                "GitLab PAT length {} is too short — expected `glpat-` + 20+ characters. \
                 Pass `--no-validate` to bypass.",
                token.len()
            )));
        }
        return Ok(());
    }
    // Unknown / generic — apply a minimum-length sanity
    // check. Most providers (Gitea, Forgejo, Codeberg) issue
    // tokens with at least 20-character bodies.
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

pub(crate) fn build_repo_creds_secret(
    name: &str,
    url_prefix: &str,
    auth: &AuthType,
    username: &str,
    token: &str,
) -> Value {
    let auth_type_str = match auth {
        AuthType::Pat => "pat",
        AuthType::Basic => "basic",
    };
    json!({
        "apiVersion": "v1",
        "kind": "Secret",
        "type": "Opaque",
        "metadata": {
            "name": name,
            "namespace": ARGOCD_NAMESPACE,
            "labels": {
                "argocd.argoproj.io/secret-type": SECRET_TYPE_LABEL_VALUE,
                APPRAFTER_MANAGED_LABEL_KEY: "apprafter",
                APPRAFTER_CRED_NAME_LABEL_KEY: name,
            },
            "annotations": {
                "apprafter.io/auth-type": auth_type_str,
                "apprafter.io/source": "cli",
            },
        },
        "stringData": {
            "url": url_prefix,
            "username": username,
            "password": token,
        },
    })
}

fn apply_secret_manifest(manifest: &Value, kubeconfig_path: &Path) -> Result<()> {
    let mut file = tempfile::Builder::new()
        .prefix("apprafter-creds-")
        .suffix(".json")
        .tempfile()
        .map_err(|e| CliError::Other(format!("create secret manifest tempfile: {e}")))?;
    let body = serde_json::to_vec_pretty(manifest)
        .map_err(|e| CliError::Other(format!("serialise secret manifest: {e}")))?;
    file.write_all(&body)
        .map_err(|e| CliError::Other(format!("write secret manifest tempfile: {e}")))?;
    file.flush()
        .map_err(|e| CliError::Other(format!("flush secret manifest tempfile: {e}")))?;

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

fn fetch_repo_creds_secrets(kubeconfig_path: &Path) -> Result<Vec<Value>> {
    let out = Command::new("kubectl")
        .arg("get")
        .arg("secret")
        .arg("-n")
        .arg(ARGOCD_NAMESPACE)
        .arg("-l")
        .arg(SECRET_TYPE_SELECTOR)
        .arg("-o")
        .arg("json")
        .env("KUBECONFIG", kubeconfig_path)
        .output()
        .map_err(|e| CliError::Other(format!("spawn kubectl: {e}")))?;
    if !out.status.success() {
        return Err(CliError::Other(format!(
            "kubectl get secrets failed (exit {:?}): {}",
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

fn creds_row(secret: &Value) -> CredsRow {
    let name = secret
        .pointer("/metadata/name")
        .and_then(Value::as_str)
        .unwrap_or("?")
        .to_string();
    let url_prefix = decode_data_field(secret, "url");
    let username = decode_data_field(secret, "username");
    let auth_type = secret
        .pointer("/metadata/annotations/apprafter.io~1auth-type")
        .and_then(Value::as_str)
        .unwrap_or("pat")
        .to_string();
    CredsRow {
        name,
        url_prefix,
        auth_type,
        username,
    }
}

fn decode_data_field(secret: &Value, field: &str) -> String {
    secret
        .pointer(&format!("/data/{field}"))
        .and_then(Value::as_str)
        .and_then(|b64| {
            B64.decode(b64)
                .ok()
                .and_then(|bytes| String::from_utf8(bytes).ok())
        })
        .unwrap_or_default()
}

fn print_creds_detail(secret: &Value) {
    let name = secret
        .pointer("/metadata/name")
        .and_then(Value::as_str)
        .unwrap_or("?");
    let auth_type = secret
        .pointer("/metadata/annotations/apprafter.io~1auth-type")
        .and_then(Value::as_str)
        .unwrap_or("pat");
    let url_prefix = decode_data_field(secret, "url");
    let username = decode_data_field(secret, "username");

    println!("Repo creds {ARGOCD_NAMESPACE}/{name}");
    println!("  URL prefix: {url_prefix}");
    println!("  Type:       {auth_type}");
    println!("  Username:   {username}");
    println!("  Password:   ****  (run `kubectl get secret {name} -n {ARGOCD_NAMESPACE} -o jsonpath='{{.data.password}}' | base64 -d` to decode the plaintext)");
}

fn find_dependent_applications(url_prefix: &str, kubeconfig_path: &Path) -> Result<Vec<String>> {
    let out = Command::new("kubectl")
        .arg("get")
        .arg("application.argoproj.io")
        .arg("-n")
        .arg(ARGOCD_NAMESPACE)
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
/// `url_prefix`. Pure fn — tests cover prefix-match semantics
/// without a cluster.
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
        assert!(
            err.contains("Phase 2"),
            "SSH error must hint at Phase 2 deferral: {err}"
        );
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
        // Documented spec: ghp_ + exactly 36 alphanumeric.
        // Refuse both shorter and longer values — likely
        // copy-paste error.
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
        // Self-hosted Gitea/Forgejo issue tokens that don't
        // match GitHub/GitLab prefixes; lenient length-only
        // fallback applies.
        let token = "abcdefghij1234567890abc".to_string();
        assert!(validate_token_format(&token, &AuthType::Pat).is_ok());
    }

    #[test]
    fn validate_token_format_rejects_short_generic_token() {
        // Most likely a partial paste or accidental
        // truncation — refuse and hint at --no-validate.
        let too_short = "shorty".to_string();
        let err = validate_token_format(&too_short, &AuthType::Pat)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("no-validate"),
            "must hint at --no-validate: {err}"
        );
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
    fn build_repo_creds_secret_carries_secret_type_label() {
        // Load-bearing — Argo CD's repo-server filters
        // Secrets by this label. If we drop it, the creds
        // are invisible to Argo CD and Applications fail to
        // pull private repos.
        let s = build_repo_creds_secret(
            "github-myorg",
            "https://github.com/myorg",
            &AuthType::Pat,
            "git",
            "ghp_x",
        );
        assert_eq!(
            s.pointer("/metadata/labels/argocd.argoproj.io~1secret-type")
                .and_then(Value::as_str),
            Some("repo-creds")
        );
    }

    #[test]
    fn build_repo_creds_secret_includes_managed_by_and_cred_name_labels() {
        let s = build_repo_creds_secret("github-myorg", "u", &AuthType::Pat, "git", "t");
        assert_eq!(
            s.pointer("/metadata/labels/apprafter.io~1managed-by")
                .and_then(Value::as_str),
            Some("apprafter")
        );
        assert_eq!(
            s.pointer("/metadata/labels/apprafter.io~1cred-name")
                .and_then(Value::as_str),
            Some("github-myorg")
        );
    }

    #[test]
    fn build_repo_creds_secret_routes_to_argocd_namespace() {
        let s = build_repo_creds_secret("n", "u", &AuthType::Pat, "git", "t");
        assert_eq!(
            s.pointer("/metadata/namespace").and_then(Value::as_str),
            Some("argocd")
        );
    }

    #[test]
    fn build_repo_creds_secret_uses_stringdata_for_round_trip() {
        // `stringData` (rather than `data`) is critical for
        // CLI ergonomics — kubectl encodes to base64 server-
        // side, so we don't need to pre-encode. If we
        // switched to `data`, the apiserver would take the
        // plaintext password as base64 and the repo-server
        // would see garbage.
        let s = build_repo_creds_secret("n", "u-prefix", &AuthType::Pat, "git", "TOKEN");
        assert_eq!(
            s.pointer("/stringData/url").and_then(Value::as_str),
            Some("u-prefix")
        );
        assert_eq!(
            s.pointer("/stringData/username").and_then(Value::as_str),
            Some("git")
        );
        assert_eq!(
            s.pointer("/stringData/password").and_then(Value::as_str),
            Some("TOKEN")
        );
        // `data` field must NOT exist — would break the
        // stringData semantics by competing for precedence.
        assert!(s.get("data").is_none());
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
        // Defensive: an Application with a missing/malformed
        // `spec.source.repoURL` (e.g. helm chart source only,
        // no git) must not trip the filter.
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
