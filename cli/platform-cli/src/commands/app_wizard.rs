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
//!  6. Destination namespace (Text, default `apprafter` — see
//!     walk-fix #12 / v0.1.160).
//!
//! Validators run inline on each prompt: URL is normalised,
//! name is DNS-1123 checked, branch is non-empty, namespace is
//! DNS-1123 (Kubernetes namespace constraints).

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
    pub namespace: Option<String>,
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
    /// Deploy environment supplied via `--env` (ADR 0044 / 2.9).
    /// When `Some` in a TTY run the env picker is pre-filled/skipped;
    /// `None` lets the wizard prompt over the manifest's declared
    /// environments.
    pub env: Option<String>,
}

/// Wizard result — every field filled, ready to flow into
/// `commands::app::add`.
pub struct WizardOutput {
    pub git_url: String,
    pub name: String,
    pub branch: String,
    pub path: String,
    pub project: String,
    pub namespace: String,
    /// Chosen deploy environment (ADR 0044 / 2.9). `Some(env)` when the
    /// manifest declares environments and the operator picked one (or
    /// supplied `--env`); `None` for a base-only deploy.
    pub env: Option<String>,
}

/// Default destination namespace for user apps registered via
/// the wizard. Aligns with the `apprafter` namespace where the
/// AppRafter operator watches for `Application` CRs and where
/// landing manifests (and most user manifests by convention)
/// declare their `metadata.namespace`. Walk-fix #12 (v0.1.160)
/// replaced the prior `<app-name>` default that produced
/// orphan destination namespaces mismatched with the manifest's
/// own namespace.
pub const DEFAULT_DESTINATION_NAMESPACE: &str = "apprafter";

const PROJECT_CHOICES: &[&str] = &["apps", "platform", "platform-providers"];

/// Sentinel item appended to the namespace `Select`: choosing it
/// falls through to a free-text `Text` prompt so the operator can
/// register an app into a namespace that doesn't exist yet (Argo CD
/// `CreateNamespace=true` then provisions it on first sync).
const NEW_NAMESPACE_SENTINEL: &str = "+ enter a new namespace";

/// Index to pre-select in the env picker: the platform default when it is a
/// declared env, else the first declared env; None when nothing is declared.
pub(crate) fn preselect_env(declared: &[String], platform_default: Option<&str>) -> Option<usize> {
    if declared.is_empty() {
        return None;
    }
    Some(
        platform_default
            .and_then(|d| declared.iter().position(|e| e == d))
            .unwrap_or(0),
    )
}

pub fn run(
    inputs: WizardInputs,
    declared_envs: &[String],
    platform_default: Option<&str>,
    existing_namespaces: &[String],
) -> Result<WizardOutput> {
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

    // Environment picker (ADR 0044 / 2.9). Only prompt when the
    // manifest declares environments; a base-only app skips it and
    // produces a base-only deploy (`env: None`). A `--env` flag
    // supplied in a TTY run pre-fills and skips the prompt.
    let env = if let Some(e) = inputs.env.filter(|e| !e.is_empty()) {
        Some(e)
    } else if declared_envs.is_empty() {
        None
    } else {
        Some(prompt_env(declared_envs, platform_default)?)
    };

    let namespace_default = inputs
        .namespace
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| DEFAULT_DESTINATION_NAMESPACE.into());
    let namespace = prompt_namespace_interactive(existing_namespaces, &namespace_default)?;

    Ok(WizardOutput {
        git_url: normalised,
        name,
        branch,
        path,
        project,
        namespace,
        env,
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
    Some(strip_apprafter_tail(&rendered))
}

/// If the relative path ends in the convention directory
/// segment `apprafter`, strip it. Walk-fix #8 post-B.1.79a:
/// when the operator runs `apprafter app add` from inside
/// `landing/cms/apprafter/` (the convention directory itself,
/// not its parent), the natural cwd → path mapping would
/// suggest `landing/cms/apprafter` — but the CMP plugin's
/// discover prefers the **parent** because that's where
/// `cue export ./...` finds the manifest under a recognisable
/// `apprafter/Application.cue` shape. Stripping the trailing
/// `apprafter` segment in the wizard's suggested default
/// gives the operator the right path without needing to
/// know the discovery internals.
///
/// Pure fn — operates on the rendered slash-separated string.
/// Returns the input verbatim when there's no `apprafter`
/// tail to strip, or `/` when the strip would leave an
/// empty path (matches the documented "render whole repo"
/// idiom).
fn strip_apprafter_tail(rel: &str) -> String {
    if rel.is_empty() {
        return "/".to_string();
    }
    // Use rsplit_once so we strip only the LAST segment if
    // it's `apprafter`. `landing/apprafter/x` (apprafter in
    // the middle) is left alone — that's a weird layout an
    // operator probably has a reason for.
    if let Some((parent, tail)) = rel.rsplit_once('/') {
        if tail == "apprafter" {
            return if parent.is_empty() {
                "/".to_string()
            } else {
                parent.to_string()
            };
        }
    } else if rel == "apprafter" {
        // cwd is `<repo-root>/apprafter` — the entire
        // relative path IS `apprafter`. Strip to repo root.
        return "/".to_string();
    }
    rel.to_string()
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

/// Environment picker over the manifest's declared `spec.environments`
/// keys (ADR 0044 / 2.9). The starting cursor lands on the cluster's
/// `PlatformStack.spec.defaultEnvironment` when that env is declared,
/// else on the first declared env (see `preselect_env`). Caller
/// guarantees `declared` is non-empty.
fn prompt_env(declared: &[String], platform_default: Option<&str>) -> Result<String> {
    let starting_cursor = preselect_env(declared, platform_default).unwrap_or(0);
    match Select::new("Environment:", declared.to_vec())
        .with_starting_cursor(starting_cursor)
        .prompt()
    {
        Ok(v) => Ok(v),
        Err(InquireError::OperationCanceled | InquireError::OperationInterrupted) => {
            Err(CliError::Other("wizard cancelled".into()))
        }
        Err(e) => Err(CliError::Other(format!("wizard prompt failed: {e}"))),
    }
}

/// Destination-namespace picker. Presents the cluster's existing
/// namespaces as an `inquire::Select` plus a trailing
/// `NEW_NAMESPACE_SENTINEL`; picking the sentinel falls through to a
/// DNS-1123-validated `Text` prompt for a brand-new namespace.
///
/// When `existing` is empty — listing namespaces failed (no cluster /
/// no kubeconfig) or the cluster genuinely has none — the picker is
/// skipped and the operator drops straight into the plain text prompt,
/// so the wizard never hard-fails on an unreachable apiserver.
fn prompt_namespace_interactive(existing: &[String], default: &str) -> Result<String> {
    if existing.is_empty() {
        return prompt_text(
            "Destination namespace",
            default,
            validate_dns_1123_for_namespace,
        );
    }

    let mut choices: Vec<String> = existing.to_vec();
    choices.push(NEW_NAMESPACE_SENTINEL.to_string());
    let starting_cursor = choices.iter().position(|n| n == default).unwrap_or(0);

    let picked = match Select::new("Destination namespace:", choices)
        .with_starting_cursor(starting_cursor)
        .prompt()
    {
        Ok(v) => v,
        Err(InquireError::OperationCanceled | InquireError::OperationInterrupted) => {
            return Err(CliError::Other("wizard cancelled".into()));
        }
        Err(e) => return Err(CliError::Other(format!("wizard prompt failed: {e}"))),
    };

    if picked == NEW_NAMESPACE_SENTINEL {
        prompt_text(
            "New namespace name",
            default,
            validate_dns_1123_for_namespace,
        )
    } else {
        Ok(picked)
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

/// Same DNS-1123 ruleset as the application name (Kubernetes
/// namespace constraints are byte-identical to its label
/// constraints). Wrapper exists so the namespace prompt's
/// error message reads "must be a valid namespace" rather
/// than the more generic app-name phrasing.
fn validate_dns_1123_for_namespace(
    value: &str,
) -> std::result::Result<Validation, inquire::CustomUserError> {
    validate_dns_1123_for_app(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_preselect_uses_platform_default_only_when_declared() {
        assert_eq!(
            preselect_env(&["dev".into(), "prod".into()], Some("prod")),
            Some(1)
        );
        assert_eq!(
            preselect_env(&["dev".into(), "prod".into()], Some("stage")),
            Some(0)
        );
        assert_eq!(preselect_env(&[], Some("prod")), None);
    }

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
    fn strip_apprafter_tail_drops_trailing_apprafter_segment() {
        // Walk-fix #8 scenario: operator ran `apprafter app add`
        // from inside `landing/cms/apprafter/` (the convention
        // directory). Wizard's path default should be
        // `landing/cms`, not `landing/cms/apprafter`, so the
        // CMP discover from the parent locates the manifest
        // under a recognisable shape.
        assert_eq!(strip_apprafter_tail("landing/cms/apprafter"), "landing/cms");
    }

    #[test]
    fn strip_apprafter_tail_collapses_top_level_apprafter_to_root() {
        // cwd == `<repo-root>/apprafter` — the entire
        // relative path IS `apprafter`. Strip to repo root
        // (rendered as `/`).
        assert_eq!(strip_apprafter_tail("apprafter"), "/");
    }

    #[test]
    fn strip_apprafter_tail_leaves_subdir_apprafter_alone() {
        // `landing/apprafter/web` — apprafter is a middle
        // segment, not the trailing one. Likely intentional
        // (operator may have a weird layout); don't touch.
        assert_eq!(
            strip_apprafter_tail("landing/apprafter/web"),
            "landing/apprafter/web"
        );
    }

    #[test]
    fn strip_apprafter_tail_handles_empty_input() {
        // Repo root case — empty relative path becomes the
        // documented `/` placeholder.
        assert_eq!(strip_apprafter_tail(""), "/");
    }

    #[test]
    fn strip_apprafter_tail_preserves_unrelated_paths() {
        assert_eq!(strip_apprafter_tail("landing/cms"), "landing/cms");
        assert_eq!(strip_apprafter_tail("foo"), "foo");
        // Same-segment-name-but-not-apprafter case.
        assert_eq!(strip_apprafter_tail("landing/cms/web"), "landing/cms/web");
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

    #[test]
    fn default_destination_namespace_constant_is_apprafter() {
        // Walk-fix #12 anchor — load-bearing constant. Drift
        // here would re-introduce the orphan-namespace bug.
        assert_eq!(DEFAULT_DESTINATION_NAMESPACE, "apprafter");
    }

    #[test]
    fn validate_dns_1123_for_namespace_accepts_kubernetes_namespace_shapes() {
        // Sanity-check the namespace validator surfaces against
        // the cases an operator is likely to type at the prompt.
        assert!(matches!(
            validate_dns_1123_for_namespace("apprafter").unwrap(),
            Validation::Valid
        ));
        assert!(matches!(
            validate_dns_1123_for_namespace("tenant-1").unwrap(),
            Validation::Valid
        ));
        assert!(matches!(
            validate_dns_1123_for_namespace("a").unwrap(),
            Validation::Valid
        ));
        assert!(matches!(
            validate_dns_1123_for_namespace("").unwrap(),
            Validation::Invalid(_)
        ));
        assert!(matches!(
            validate_dns_1123_for_namespace("Tenant").unwrap(),
            Validation::Invalid(_)
        ));
        assert!(matches!(
            validate_dns_1123_for_namespace("-leading").unwrap(),
            Validation::Invalid(_)
        ));
    }
}
