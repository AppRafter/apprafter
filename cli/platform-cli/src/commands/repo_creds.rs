// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! `apprafter repo creds …` — manage Argo CD repo creds
//! Secrets для private user repos. Track B.1.79a part 5.
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
//! Argo CD's repo-server scans `argocd` namespace for Secrets
//! labeled `argocd.argoproj.io/secret-type: repo-creds` и uses
//! whichever entry's `url` field is а prefix-match for an
//! Application's `spec.source.repoURL`. So registering
//! `https://github.com/myorg` makes every Application
//! pointing к `https://github.com/myorg/<any>` inherit those
//! creds automatically.

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

pub fn add(
    name: &str,
    url_prefix: &str,
    auth_type: &str,
    username: &str,
    token: Option<String>,
    no_validate: bool,
) -> Result<()> {
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
            "Secret '{name}' уже существует в namespace {ARGOCD_NAMESPACE}. Запусти \
             `apprafter repo creds rotate {name}` чтобы заменить token, либо \
             `apprafter repo creds remove {name}` + `add` чтобы пересоздать с нуля."
        )));
    }

    let manifest = build_repo_creds_secret(name, url_prefix, &auth, username, &token);
    apply_secret_manifest(&manifest, kc.path())?;

    println!("✓ Repo creds '{name}' зарегистрированы для URL prefix '{url_prefix}'.");
    println!("  Type:     {auth_type}");
    println!("  Username: {username}");
    println!();
    println!(
        "Все Argo CD Applications с `spec.source.repoURL` starting with '{url_prefix}' \
         автоматически inherit эти creds."
    );
    Ok(())
}

pub fn list() -> Result<()> {
    let kc = ensure_kubeconfig_tempfile()?;
    let secrets = fetch_repo_creds_secrets(kc.path())?;

    if secrets.is_empty() {
        println!("No repo creds registered в namespace {ARGOCD_NAMESPACE}.");
        println!(
            "Подсказка: `apprafter repo creds add <name> --url-prefix <url> --type pat \
             --token <pat>` чтобы зарегистрировать первый entry."
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
                "Repo creds '{name}' не найдены в namespace {ARGOCD_NAMESPACE}."
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
                "Repo creds '{name}' не найдены в namespace {ARGOCD_NAMESPACE}."
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
    // recreating — repo-server holds а cached resourceVersion
    // pointer и а full recreate would cause а brief reconnect
    // window. Base64-encode т.к. Secret's `data` (а не
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

    println!("✓ Token для repo creds '{name}' rotated.");
    println!("Argo CD repo-server подхватит новый token на следующем pull cycle.");
    Ok(())
}

pub fn remove(name: &str, force: bool, yes: bool) -> Result<()> {
    let kc = ensure_kubeconfig_tempfile()?;
    let secret = kubectl_get_json("secret", Some(name), Some(ARGOCD_NAMESPACE), kc.path())?
        .ok_or_else(|| {
            CliError::Other(format!(
                "Repo creds '{name}' не найдены в namespace {ARGOCD_NAMESPACE}."
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
        // Dependency check: refuse если есть Argo CD
        // Application(s) у которых `spec.source.repoURL`
        // имеет урезанный prefix-match с этими creds.
        // Operator может override через `--force` если
        // понимает что делает (например, мигрирует к
        // другому creds entry).
        let deps = find_dependent_applications(&url_prefix, kc.path())?;
        if !deps.is_empty() {
            return Err(CliError::Other(format!(
                "Repo creds '{name}' используются {n} Application(s): {names}. \
                 Перерегистрируй их с другими creds (`apprafter app remove`/`add`) \
                 либо передай `--force` чтобы удалить creds anyway.",
                n = deps.len(),
                names = deps.join(", ")
            )));
        }
    }

    if !yes && !force {
        if !io::stdin().is_terminal() {
            return Err(CliError::Other(
                "non-interactive shell — pass `--yes` (или `--force`) чтобы пропустить confirmation prompt".into(),
            ));
        }
        println!("Удалить repo creds '{name}' (URL prefix: {url_prefix})?");
        let confirmed = inquire::Confirm::new("Подтвердить?")
            .with_default(false)
            .prompt()
            .map_err(|e| CliError::Other(format!("confirmation prompt: {e}")))?;
        if !confirmed {
            println!("Отмена.");
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
    println!("✓ Repo creds '{name}' удалены.");
    Ok(())
}

// =========================================================================
// PURE HELPERS — testable без kube::Client / git binary / network.
// =========================================================================

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub(crate) enum AuthType {
    Pat,
    Basic,
}

fn parse_auth_type(raw: &str) -> Result<AuthType> {
    match raw {
        "pat" => Ok(AuthType::Pat),
        "basic" => Ok(AuthType::Basic),
        "ssh" => Err(CliError::Other(
            "SSH-key auth deferred к Phase 2 — pass `--type pat` или `--type basic` для now".into(),
        )),
        other => Err(CliError::Other(format!(
            "Неизвестный `--type {other}` — допускаются `pat` / `basic`."
        ))),
    }
}

fn validate_dns_1123(name: &str) -> Result<()> {
    if name.is_empty() || name.len() > 64 {
        return Err(CliError::Other(format!(
            "Имя creds '{name}' должно быть 1..63 символа DNS-1123 (lower-case [a-z0-9-])."
        )));
    }
    let ok = name
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && !name.starts_with('-')
        && !name.ends_with('-');
    if !ok {
        return Err(CliError::Other(format!(
            "Имя creds '{name}' содержит недопустимые символы; ожидается DNS-1123."
        )));
    }
    Ok(())
}

fn resolve_token(token: Option<String>) -> Result<String> {
    if let Some(t) = token {
        if t.is_empty() {
            return Err(CliError::Other(
                "пустой `--token` — передай non-empty token либо опусти флаг для interactive prompt"
                    .into(),
            ));
        }
        return Ok(t);
    }
    if !io::stdin().is_terminal() {
        return Err(CliError::Other(
            "`--token` не передан и stdin не TTY — добавь `--token <value>` либо запусти из \
             interactive shell"
                .into(),
        ));
    }
    let token = inquire::Password::new("Token:")
        .with_display_mode(inquire::PasswordDisplayMode::Masked)
        .without_confirmation()
        .prompt()
        .map_err(|e| CliError::Other(format!("token prompt: {e}")))?;
    if token.is_empty() {
        return Err(CliError::Other("token не может быть пустым".into()));
    }
    Ok(token)
}

/// Provider-aware token regex check. **Best-effort** — operator
/// может передать `--no-validate` чтобы skip когда self-hosted
/// Gitea/Forgejo с custom token shapes. Detection priority:
/// GitHub fine-grained PAT > GitHub classic PAT > GitLab PAT >
/// fallback к 8+-char generic check.
pub(crate) fn validate_token_format(token: &str, auth: &AuthType) -> Result<()> {
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
        // против partial paste.
        if token.len() < 80 {
            return Err(CliError::Other(format!(
                "GitHub fine-grained PAT length {} слишком короткий — ожидается 80+ символов. \
                 Передай `--no-validate` если уверен что token корректный.",
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
                "GitHub classic PAT length {} ≠ 40 (ожидается `ghp_` + 36 alphanumeric). \
                 Передай `--no-validate` для bypass.",
                token.len()
            )));
        }
        return Ok(());
    }
    if token.starts_with("glpat-") {
        // GitLab PAT. Documented `glpat-` + 20-char body
        // (но операторы иногда обрезают; lenient check
        // на minimum length).
        if token.len() < 20 {
            return Err(CliError::Other(format!(
                "GitLab PAT length {} слишком короткий — ожидается `glpat-` + 20+ символов. \
                 Передай `--no-validate` для bypass.",
                token.len()
            )));
        }
        return Ok(());
    }
    // Unknown / generic — apply а minimum-length sanity
    // check. Most providers (Gitea, Forgejo, Codeberg)
    // issue tokens with а least 20 character bodies.
    if token.len() < 20 {
        return Err(CliError::Other(format!(
            "Token shape не распознан (не GitHub PAT, не GitLab PAT), а length {} < 20 \
             — слишком короткий для большинства providers. Передай `--no-validate` если \
             self-hosted Gitea/Forgejo с custom token shape.",
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
    println!("  Password:   ****  (передай `kubectl get secret {name} -n {ARGOCD_NAMESPACE} -o jsonpath='{{.data.password}}' | base64 -d` чтобы decode plaintext)");
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
/// без cluster.
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
        // Refuse both shorter и longer values — likely
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
        // Most likely а partial paste or accidental
        // truncation — refuse и hint at --no-validate.
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
        // are invisible к Argo CD и Applications fail к pull
        // private repos.
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
        // `stringData` (rather than `data`) is critical для
        // CLI ergonomics — kubectl encodes к base64 server-
        // side, so we don't need к pre-encode. Если switch
        // к `data`, апи возьмёт plaintext password как
        // base64 и repo-server увидит garbage.
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
        // stringData semantics by competing для precedence.
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
        // Defensive: an Application с missing/malformed
        // `spec.source.repoURL` (e.g. helm chart source
        // только без git) must not trip the filter.
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
