// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Interactive wizard for `apprafter app add` (Track B.1.79a
//! polish #3 / v0.1.145). Mirrors the `target add` wizard
//! pattern: triggers when stdin + stdout are TTYs and
//! `--no-interactive` is not set; fills only the fields the
//! user didn't already supply via flags.
//!
//! Prompt order:
//!  1. Git URL (Text, default = detected git origin or empty).
//!  2. Application name (Text, default = derived from URL).
//!  3. Branch / revision (Text, default = current branch or
//!     `main`).
//!  4. Path within the repo (Text, default `/`).
//!  5. AppProject (Select: `apps` / `platform` /
//!     `platform-providers`, default = the `--project` flag's
//!     value).
//!
//! Validators run inline on each prompt: URL is normalised,
//! name is DNS-1123 checked, branch is non-empty.

use std::process::Command;

use cli_core::{CliError, Result};
use inquire::validator::Validation;
use inquire::{InquireError, Select, Text};

use crate::commands::app::{derive_app_name, normalise_git_url};

/// Same gate `target add` uses — fires only when both ends of
/// stdio are terminals and the operator hasn't opted out via
/// `--no-interactive`.
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

/// Inputs that flowed in via flags before the wizard fired.
/// The wizard treats these as prefills the operator may
/// accept (Enter) or override per-field.
#[derive(Debug, Clone, Default)]
pub struct WizardInputs {
    pub git_url: Option<String>,
    pub name: Option<String>,
    pub branch: Option<String>,
    pub path: Option<String>,
    pub project: Option<String>,
    pub detected_origin: Option<String>,
    pub detected_branch: Option<String>,
    /// cwd-relative-to-repo-root path detected by the caller
    /// via `detect_path_relative_to_repo_root`. Used as the
    /// default value for the "Path within repo" prompt when
    /// `path` is the clap default (`"/"`) — running `apprafter
    /// app add` from `landing/cms/` should default the prompt
    /// to `landing/cms` rather than the repo root, since
    /// that's almost always what the operator means.
    /// `None` when cwd isn't inside a git working tree, or git
    /// is unavailable.
    pub detected_path: Option<String>,
}

/// Wizard result — every field filled, ready to flow into
/// `commands::app::add`.
pub struct WizardOutput {
    pub git_url: String,
    pub name: String,
    pub branch: String,
    pub path: String,
    pub project: String,
}

const PROJECT_CHOICES: &[&str] = &["apps", "platform", "platform-providers"];

pub fn run(inputs: WizardInputs) -> Result<WizardOutput> {
    eprintln!();
    eprintln!("Register a user Application. Fill in the gaps below;");
    eprintln!("press Enter to accept defaults shown in [brackets].");
    eprintln!();

    let default_url = inputs
        .git_url
        .clone()
        .or(inputs.detected_origin.clone())
        .unwrap_or_default();
    let git_url = prompt_text("Git URL", &default_url, validate_non_empty)?;
    let normalised = normalise_git_url(&git_url);

    let name_default = inputs.name.unwrap_or_else(|| derive_app_name(&normalised));
    let name = prompt_text("Application name", &name_default, validate_dns_1123_for_app)?;

    let branch_default = inputs
        .branch
        .or(inputs.detected_branch)
        .unwrap_or_else(|| "main".into());
    let branch = prompt_text("Branch / revision", &branch_default, validate_non_empty)?;

    // Path default precedence:
    //   1. Operator passed `--path X` explicitly with a
    //      non-default value → honour it.
    //   2. cwd lives inside a git working tree → use the
    //      cwd-relative-to-repo-root path (e.g. cwd =
    //      `landing/cms/` → default `landing/cms`).
    //   3. Fall back to the clap default `"/"`.
    //
    // `inputs.path` is always `Some("/")` when the wizard is
    // entered via the `app add` dispatch with clap's default,
    // so "operator did not specify --path" looks identical to
    // "operator specified --path /". We treat both the same:
    // override with the cwd-detect if available. Operators
    // who want the repo root explicitly can still type `/`
    // at the prompt.
    let explicit_path = inputs.path.filter(|p| !p.is_empty() && p != "/");
    let path_default = explicit_path
        .or(inputs.detected_path)
        .unwrap_or_else(|| "/".into());
    let path = prompt_text("Path within repo", &path_default, validate_non_empty)?;

    let project_default = inputs.project.unwrap_or_else(|| "apps".into());
    let project = prompt_project(&project_default)?;

    Ok(WizardOutput {
        git_url: normalised,
        name,
        branch,
        path,
        project,
    })
}

/// Probe `git remote get-url <remote>` non-fatally — returns
/// `None` outside a git repo, with `git` missing, or when the
/// named remote isn't configured. The wizard treats the
/// absence as "no prefill" rather than an error so operators
/// can register apps from arbitrary cwds.
pub fn detect_git_origin(remote: &str) -> Option<String> {
    let out = Command::new("git")
        .args(["remote", "get-url", remote])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let raw = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if raw.is_empty() {
        return None;
    }
    Some(normalise_git_url(&raw))
}

/// Probe `git rev-parse --show-toplevel` to find the repo
/// root and compute the cwd's path relative to it. Returns
/// `Some("landing/cms")` when cwd is a subdir of a git work
/// tree, `Some("")` when cwd IS the repo root (rendered as
/// `/` upstream so the prompt default matches the documented
/// "render the whole repo" idiom), and `None` when cwd
/// isn't inside any git tree or git is missing.
///
/// The convention `apprafter app add` defaults to feels
/// natural for a monorepo: `cd landing/cms && apprafter
/// app add` → prompt defaults to `landing/cms`. The CMP
/// plugin's discover glob (chart 0.1.42+) matches files
/// in any `apprafter/` directory under that path, so the
/// operator doesn't have to spell out `landing/cms/apprafter`
/// — pointing at the subdir that owns the manifests is
/// enough.
pub fn detect_path_relative_to_repo_root() -> Option<String> {
    let out = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let root_raw = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if root_raw.is_empty() {
        return None;
    }
    let root = std::path::PathBuf::from(&root_raw);
    let cwd = std::env::current_dir().ok()?;
    // Canonicalise both so symlinks (common on macOS where
    // `/var` is a symlink to `/private/var`, plus arbitrary
    // operator setups) don't trip the strip_prefix check.
    let root = root.canonicalize().unwrap_or(root);
    let cwd = cwd.canonicalize().unwrap_or(cwd);
    let rel = cwd.strip_prefix(&root).ok()?;
    let rendered = rel
        .to_string_lossy()
        .replace(std::path::MAIN_SEPARATOR, "/");
    if rendered.is_empty() {
        // cwd == repo root. Empty string rendered as `/` so
        // the prompt UI matches operator expectations
        // ("render the whole repo").
        Some("/".to_string())
    } else {
        Some(rendered)
    }
}

/// Probe `git symbolic-ref --short HEAD` non-fatally — returns
/// the current branch when the cwd is a git repo and HEAD
/// points at a branch (not a detached commit).
pub fn detect_git_branch() -> Option<String> {
    let out = Command::new("git")
        .args(["symbolic-ref", "--short", "HEAD"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let raw = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if raw.is_empty() {
        None
    } else {
        Some(raw)
    }
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

fn prompt_project(default: &str) -> Result<String> {
    let starting_cursor = PROJECT_CHOICES
        .iter()
        .position(|p| *p == default)
        .unwrap_or(0);
    match Select::new("AppProject:", PROJECT_CHOICES.to_vec())
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

fn validate_non_empty(value: &str) -> std::result::Result<Validation, inquire::CustomUserError> {
    if value.trim().is_empty() {
        Ok(Validation::Invalid("must not be empty".into()))
    } else {
        Ok(Validation::Valid)
    }
}

fn validate_dns_1123_for_app(
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_use_wizard_respects_no_interactive_flag() {
        assert!(!should_use_wizard(true, true, true));
        assert!(!should_use_wizard(true, false, false));
    }

    #[test]
    fn should_use_wizard_requires_both_streams_to_be_terminals() {
        assert!(should_use_wizard(false, true, true));
        assert!(!should_use_wizard(false, true, false));
        assert!(!should_use_wizard(false, false, true));
        assert!(!should_use_wizard(false, false, false));
    }

    #[test]
    fn validate_dns_1123_for_app_accepts_well_formed() {
        let r = validate_dns_1123_for_app("my-app").unwrap();
        assert!(matches!(r, Validation::Valid));
    }

    #[test]
    fn validate_dns_1123_for_app_rejects_uppercase_and_dashes() {
        let r = validate_dns_1123_for_app("MyApp").unwrap();
        assert!(matches!(r, Validation::Invalid(_)));
        let r = validate_dns_1123_for_app("-leading").unwrap();
        assert!(matches!(r, Validation::Invalid(_)));
        let r = validate_dns_1123_for_app("trailing-").unwrap();
        assert!(matches!(r, Validation::Invalid(_)));
        let r = validate_dns_1123_for_app("").unwrap();
        assert!(matches!(r, Validation::Invalid(_)));
    }

    #[test]
    fn validate_non_empty_trims_whitespace() {
        assert!(matches!(
            validate_non_empty("").unwrap(),
            Validation::Invalid(_)
        ));
        assert!(matches!(
            validate_non_empty("   ").unwrap(),
            Validation::Invalid(_)
        ));
        assert!(matches!(
            validate_non_empty("a").unwrap(),
            Validation::Valid
        ));
    }
}
