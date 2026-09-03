// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Interactive wizard for `apprafter repo creds add` (Track
//! B.1.79a polish #3 / v0.1.145). Same gate as the `target
//! add` wizard — fires only when stdin + stdout are TTYs and
//! `--no-interactive` is not set.
//!
//! Prompt order:
//!  1. Friendly name (Text, DNS-1123 validated).
//!  2. URL prefix (Text, must look like a URL).
//!  3. Auth type (Select: `pat` / `basic`).
//!  4. Username (Text, default `git`).
//!  5. Token / password (Password, masked, inline
//!     provider-aware format check unless the operator
//!     explicitly passed `--no-validate`).

use cli_core::{CliError, Result};
use inquire::validator::Validation;
use inquire::{InquireError, Password, PasswordDisplayMode, Select, Text};

use crate::commands::repo_creds::{validate_token_format, AuthType};

const AUTH_TYPE_CHOICES: &[&str] = &["pat", "basic"];

pub fn should_use_wizard(
    no_interactive: bool,
    stdin_is_terminal: bool,
    stdout_is_terminal: bool,
) -> bool {
    if no_interactive {
        return false;
    }
    stdin_is_terminal && stdout_is_terminal
}

#[derive(Debug, Clone, Default)]
pub struct WizardInputs {
    pub name: Option<String>,
    pub url_prefix: Option<String>,
    pub auth_type: Option<String>,
    pub username: Option<String>,
    pub token: Option<String>,
    pub no_validate: bool,
}

pub struct WizardOutput {
    pub name: String,
    pub url_prefix: String,
    pub auth_type: String,
    pub username: String,
    pub token: String,
}

pub fn run(inputs: WizardInputs) -> Result<WizardOutput> {
    eprintln!();
    eprintln!("Register a git-repo credential. Fill in the gaps below;");
    eprintln!("press Enter to accept defaults shown in [brackets].");
    eprintln!();

    let name_default = inputs.name.unwrap_or_default();
    let name = prompt_text("Friendly name", &name_default, validate_dns_1123_for_creds)?;

    let url_default = inputs.url_prefix.unwrap_or_default();
    let url_prefix = prompt_text("URL prefix", &url_default, validate_url_prefix)?;

    let auth_default = inputs.auth_type.unwrap_or_else(|| "pat".into());
    let auth_type = prompt_select("Auth type:", &auth_default)?;

    let username_default = inputs.username.unwrap_or_else(|| "git".into());
    let username = prompt_text("Username", &username_default, validate_non_empty)?;

    let token = match inputs.token {
        Some(t) if !t.is_empty() => t,
        _ => prompt_token(&auth_type, inputs.no_validate)?,
    };

    Ok(WizardOutput {
        name,
        url_prefix,
        auth_type,
        username,
        token,
    })
}

fn prompt_text<F>(label: &str, default: &str, validate: F) -> Result<String>
where
    F: Fn(&str) -> std::result::Result<Validation, inquire::CustomUserError>
        + Send
        + Sync
        + Clone
        + 'static,
{
    let mut prompt = Text::new(label).with_validator(validate);
    if !default.is_empty() {
        prompt = prompt.with_default(default);
    }
    match prompt.prompt() {
        Ok(v) => Ok(v.trim().to_string()),
        Err(InquireError::OperationCanceled | InquireError::OperationInterrupted) => {
            Err(CliError::Other("wizard cancelled".into()))
        }
        Err(e) => Err(CliError::Other(format!("wizard prompt failed: {e}"))),
    }
}

fn prompt_select(label: &str, default: &str) -> Result<String> {
    let starting_cursor = AUTH_TYPE_CHOICES
        .iter()
        .position(|p| *p == default)
        .unwrap_or(0);
    match Select::new(label, AUTH_TYPE_CHOICES.to_vec())
        .with_starting_cursor(starting_cursor)
        .prompt()
    {
        Ok(v) => Ok(v.to_string()),
        Err(InquireError::OperationCanceled | InquireError::OperationInterrupted) => {
            Err(CliError::Other("wizard cancelled".into()))
        }
        Err(e) => Err(CliError::Other(format!("wizard prompt failed: {e}"))),
    }
}

fn prompt_token(auth_type: &str, no_validate: bool) -> Result<String> {
    // The token validator runs inline so the operator gets
    // immediate "✗ GitHub fine-grained PAT length too short"
    // feedback rather than discovering the regex failure
    // after submitting the whole form.
    let auth = parse_auth_type_safe(auth_type);
    let validator = move |value: &str| Ok(token_validation(value, &auth, no_validate));
    let prompt = Password::new("Token / password:")
        .with_display_mode(PasswordDisplayMode::Masked)
        .without_confirmation()
        .with_validator(validator);
    match prompt.prompt() {
        Ok(v) => Ok(v),
        Err(InquireError::OperationCanceled | InquireError::OperationInterrupted) => {
            Err(CliError::Other("wizard cancelled".into()))
        }
        Err(e) => Err(CliError::Other(format!("wizard prompt failed: {e}"))),
    }
}

/// Pure body of the inline token validator: `--no-validate` disables
/// the shape check entirely, an empty entry is always rejected, and
/// anything else is judged by the shared provider-aware format check so
/// the wizard and the flag path can never disagree.
fn token_validation(value: &str, auth: &AuthType, no_validate: bool) -> Validation {
    if no_validate {
        return Validation::Valid;
    }
    if value.is_empty() {
        return Validation::Invalid("must not be empty".into());
    }
    match validate_token_format(value, auth) {
        Ok(_) => Validation::Valid,
        Err(e) => Validation::Invalid(e.to_string().into()),
    }
}

/// Map a stringly-typed auth-type input to the typed
/// `AuthType` for format validation. Unknown input downgraded
/// to `Pat` so the token validator's generic fallback fires —
/// the upstream wizard's Select prevents this branch from
/// firing in practice; this is purely a defensive guard.
fn parse_auth_type_safe(raw: &str) -> AuthType {
    match raw {
        "basic" => AuthType::Basic,
        _ => AuthType::Pat,
    }
}

fn validate_non_empty(value: &str) -> std::result::Result<Validation, inquire::CustomUserError> {
    if value.trim().is_empty() {
        Ok(Validation::Invalid("must not be empty".into()))
    } else {
        Ok(Validation::Valid)
    }
}

fn validate_dns_1123_for_creds(
    value: &str,
) -> std::result::Result<Validation, inquire::CustomUserError> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Ok(Validation::Invalid("must not be empty".into()));
    }
    if trimmed.len() > 63 {
        return Ok(Validation::Invalid("must be 1-63 characters".into()));
    }
    let ok = trimmed
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && !trimmed.starts_with('-')
        && !trimmed.ends_with('-');
    if !ok {
        return Ok(Validation::Invalid(
            "DNS-1123 only: lower-case [a-z0-9-], no leading/trailing dash".into(),
        ));
    }
    Ok(Validation::Valid)
}

fn validate_url_prefix(value: &str) -> std::result::Result<Validation, inquire::CustomUserError> {
    let v = value.trim();
    if v.is_empty() {
        return Ok(Validation::Invalid("must not be empty".into()));
    }
    // A URL prefix Argo CD will prefix-match against
    // `spec.source.repoURL`. Accept anything that looks like
    // a scheme + host (`https://...`, `git@host:...`); reject
    // bare paths (no scheme, no SCP-style colon) because they
    // can never prefix-match a fully-qualified Application
    // repoURL and would silently fail later.
    if v.starts_with("https://") || v.starts_with("http://") || v.contains("@") {
        return Ok(Validation::Valid);
    }
    Ok(Validation::Invalid(
        "expected `https://...` or `git@host:...` form".into(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_use_wizard_gate() {
        assert!(should_use_wizard(false, true, true));
        assert!(!should_use_wizard(true, true, true));
        assert!(!should_use_wizard(false, false, true));
        assert!(!should_use_wizard(false, true, false));
    }

    #[test]
    fn validate_url_prefix_accepts_https_and_scp_forms() {
        assert!(matches!(
            validate_url_prefix("https://github.com/myorg").unwrap(),
            Validation::Valid
        ));
        assert!(matches!(
            validate_url_prefix("git@github.com:myorg").unwrap(),
            Validation::Valid
        ));
    }

    #[test]
    fn validate_url_prefix_rejects_bare_paths() {
        // Bare paths can't prefix-match a fully-qualified
        // Argo CD repoURL. Fail-loud at wizard time rather
        // than silently mismatch later.
        let r = validate_url_prefix("github.com/myorg").unwrap();
        assert!(matches!(r, Validation::Invalid(_)));
        let r = validate_url_prefix("").unwrap();
        assert!(matches!(r, Validation::Invalid(_)));
    }

    #[test]
    fn validate_dns_1123_for_creds_accepts_well_formed() {
        assert!(matches!(
            validate_dns_1123_for_creds("github-myorg").unwrap(),
            Validation::Valid
        ));
    }

    #[test]
    fn validate_dns_1123_for_creds_rejects_uppercase() {
        let r = validate_dns_1123_for_creds("GitHubMyOrg").unwrap();
        assert!(matches!(r, Validation::Invalid(_)));
    }

    #[test]
    fn parse_auth_type_safe_defaults_to_pat_on_garbage() {
        assert_eq!(parse_auth_type_safe("garbage"), AuthType::Pat);
        assert_eq!(parse_auth_type_safe("basic"), AuthType::Basic);
        assert_eq!(parse_auth_type_safe("pat"), AuthType::Pat);
    }

    /// The custom message of an `Invalid` verdict, or `None` for `Valid`
    /// (and for the library's default message, which we never emit).
    fn invalid_message(v: &Validation) -> Option<&str> {
        match v {
            Validation::Invalid(inquire::validator::ErrorMessage::Custom(m)) => Some(m),
            _ => None,
        }
    }

    #[test]
    fn validate_non_empty_rejects_whitespace_only_input() {
        // The wizard trims before use, so "   " would become "" — reject
        // it at the prompt instead of writing an empty username.
        assert!(invalid_message(&validate_non_empty("   ").unwrap()).is_some());
        assert!(matches!(
            validate_non_empty(" git ").unwrap(),
            Validation::Valid
        ));
    }

    #[test]
    fn validate_dns_1123_for_creds_rejects_edge_dashes_and_overlong_names() {
        // The name becomes a Kubernetes object name; a leading/trailing
        // dash or a 64th character is rejected by the apiserver, so the
        // wizard must catch it before four more prompts are answered.
        assert!(invalid_message(&validate_dns_1123_for_creds("-acme").unwrap()).is_some());
        assert!(invalid_message(&validate_dns_1123_for_creds("acme-").unwrap()).is_some());
        assert!(invalid_message(&validate_dns_1123_for_creds(&"a".repeat(64)).unwrap()).is_some());
        assert!(matches!(
            validate_dns_1123_for_creds(&"a".repeat(63)).unwrap(),
            Validation::Valid
        ));
        // Trimmed before validating — a stray trailing space is not an
        // "invalid character".
        assert!(matches!(
            validate_dns_1123_for_creds(" acme ").unwrap(),
            Validation::Valid
        ));
    }

    #[test]
    fn validate_url_prefix_accepts_plain_http_and_trims() {
        // Self-hosted Gitea over plain http is a supported remote.
        assert!(matches!(
            validate_url_prefix("http://git.internal/team").unwrap(),
            Validation::Valid
        ));
        assert!(matches!(
            validate_url_prefix("  https://github.com/acme  ").unwrap(),
            Validation::Valid
        ));
        assert!(invalid_message(&validate_url_prefix("   ").unwrap()).is_some());
    }

    #[test]
    fn token_validation_delegates_to_the_shared_format_check() {
        // The inline wizard verdict must be the SAME judgement the
        // non-interactive path applies, or an operator can type a token
        // the wizard accepts and `add` then rejects.
        let good = format!("ghp_{}", "a".repeat(36));
        assert!(matches!(
            token_validation(&good, &AuthType::Pat, false),
            Validation::Valid
        ));
        let msg = token_validation("ghp_short", &AuthType::Pat, false);
        let msg = invalid_message(&msg).expect("short classic PAT must be rejected");
        assert!(msg.contains("40"), "{msg}");
    }

    #[test]
    fn token_validation_rejects_empty_but_no_validate_disables_the_shape_check() {
        assert!(invalid_message(&token_validation("", &AuthType::Pat, false)).is_some());
        // `--no-validate` is the documented bypass for self-hosted
        // providers with custom token shapes.
        assert!(matches!(
            token_validation("weird-shape", &AuthType::Pat, true),
            Validation::Valid
        ));
        assert!(invalid_message(&token_validation("weird-shape", &AuthType::Pat, false)).is_some());
    }

    #[test]
    fn token_validation_accepts_any_non_empty_basic_password() {
        // Basic-auth passwords have no provider shape to check.
        assert!(matches!(
            token_validation("p", &AuthType::Basic, false),
            Validation::Valid
        ));
    }
}
