// SPDX-License-Identifier: FSL-1.1-MIT
//! `apprafter target …` subcommand handlers.
//!
//! v0.1.73 (Track A.3) ships **`target add`** in pure non-interactive
//! mode. CRUD commands (`list / use / show / rename / remove`)
//! arrive in Track A.5; the interactive wizard arrives in A.4.
//!
//! Resolution flow for `target add`:
//!   1. Parse + validate flags (provider known, token regex, ssh-key
//!      readable if provided, name shape).
//!   2. Load existing target if any.
//!   3. Apply create / renew / overwrite semantics.
//!   4. Persist via `cli_core::target::save_target`.
//!   5. If first target, set as active in `GlobalConfig`.
//!   6. Print one-line confirmation.

use std::path::{Path, PathBuf};

use cli_core::target::{
    default_config_root, load_global_config, load_target, save_global_config, save_target,
    validate_hetzner_token_format, GlobalConfig, Target, TargetConfig, TargetCredentials,
    TargetStorePaths,
};
use cli_core::{CliError, Result};
use tracing::info;

use crate::cli::TargetCommand;

/// Maximum length for a target name. Matches the spec
/// (`cli-dx-task.md` §5.1 validation rules). A short cap keeps
/// directory traversal and shell-history scenarios sane without
/// being meaningfully restrictive.
pub const MAX_TARGET_NAME_LEN: usize = 64;

/// Only `hetzner-cloud` is wired in v0.1.73. AWS / Managed land in
/// later phases; surface a clear error so users hit a typed wall
/// instead of saving a half-working config.
pub const SUPPORTED_PROVIDERS: &[&str] = &["hetzner-cloud"];

pub fn run(action: TargetCommand) -> Result<()> {
    match action {
        TargetCommand::Add {
            name,
            provider,
            token,
            ssh_key,
            region,
            tier,
            cluster_name,
            force,
            renew,
            no_interactive: _,
        } => run_add(AddArgs {
            name,
            provider,
            token,
            ssh_key,
            region,
            tier,
            cluster_name,
            force,
            renew,
        }),
    }
}

/// Plain bundle so the orchestration body below has one parameter
/// to thread instead of nine. Field shapes mirror the clap flags
/// exactly; keep the rename pressure low by not introducing
/// intermediate types.
pub struct AddArgs {
    pub name: String,
    pub provider: Option<String>,
    pub token: Option<String>,
    pub ssh_key: Option<PathBuf>,
    pub region: Option<String>,
    pub tier: Option<String>,
    pub cluster_name: Option<String>,
    pub force: bool,
    pub renew: bool,
}

fn run_add(args: AddArgs) -> Result<()> {
    info!(target = %args.name, renew = args.renew, force = args.force, "target add invoked");
    validate_target_name(&args.name)?;

    let paths = TargetStorePaths::for_root(default_config_root()?);

    if args.renew {
        return run_renew(&paths, args);
    }

    // Plain create / overwrite path.
    let provider = require_known_provider(args.provider.as_deref())?;
    let token = require_token(&provider, args.token.as_deref())?;
    if let Some(path) = args.ssh_key.as_ref() {
        verify_ssh_key_readable(path)?;
    }

    let existing = load_target(&paths, &args.name);
    match existing {
        Ok(_) if !args.force => {
            return Err(CliError::Other(format!(
                "target `{}` already exists — pass `--force` to overwrite or `--renew` to rotate credentials only",
                args.name
            )));
        }
        Ok(_) | Err(CliError::TargetNotFound { .. }) => {}
        Err(e) => return Err(e),
    }

    let target = Target {
        name: args.name.clone(),
        config: TargetConfig {
            provider,
            region: args.region,
            default_tier: args.tier,
            cluster_name: args.cluster_name,
            ssh_key_path: args.ssh_key,
        },
        credentials: TargetCredentials {
            hetzner_token: Some(token),
        },
    };
    save_target(&paths, &target)?;

    let became_active = ensure_active_target(&paths, &args.name)?;

    if became_active {
        println!(
            "target `{}` saved and set as active (first target on fresh store)",
            args.name
        );
    } else {
        println!(
            "target `{}` saved (active target unchanged — use `apprafter target use {}` to switch)",
            args.name, args.name
        );
    }
    Ok(())
}

fn run_renew(paths: &TargetStorePaths, args: AddArgs) -> Result<()> {
    let mut existing = match load_target(paths, &args.name) {
        Ok(t) => t,
        Err(CliError::TargetNotFound { .. }) => {
            return Err(CliError::Other(format!(
                "target `{}` does not exist — drop `--renew` to create it fresh",
                args.name
            )));
        }
        Err(e) => return Err(e),
    };

    // `--renew` deliberately ignores the config flags (provider,
    // region, tier, etc.). Refusing them up front beats silently
    // dropping a user-provided value.
    if args.provider.is_some()
        || args.region.is_some()
        || args.tier.is_some()
        || args.cluster_name.is_some()
    {
        return Err(CliError::Other(
            "`--renew` only updates credentials — `--provider`, `--region`, `--tier`, `--cluster-name` are not allowed alongside it. Drop `--renew` if you want to change config too.".to_string(),
        ));
    }

    // Token is required for renew (whole point of the flag);
    // ssh-key path is optional (user may renew only the token).
    let token = require_token(&existing.config.provider, args.token.as_deref())?;
    if let Some(path) = args.ssh_key.as_ref() {
        verify_ssh_key_readable(path)?;
        existing.config.ssh_key_path = Some(path.clone());
    }

    existing.credentials = TargetCredentials {
        hetzner_token: Some(token),
    };
    save_target(paths, &existing)?;

    println!("target `{}` credentials rotated", args.name);
    Ok(())
}

// ---------------------------------------------------------------
// Pure validators
// ---------------------------------------------------------------

fn validate_target_name(name: &str) -> Result<()> {
    if name.is_empty() {
        return Err(CliError::Other("target name must not be empty".to_string()));
    }
    if name.len() > MAX_TARGET_NAME_LEN {
        return Err(CliError::Other(format!(
            "target name must be ≤ {MAX_TARGET_NAME_LEN} chars (got {})",
            name.len()
        )));
    }
    // Avoid filesystem-reserved characters and any path-traversal
    // surface. The pattern matches Kubernetes resource names which
    // are already familiar to operators.
    if !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        return Err(CliError::Other(format!(
            "target name `{name}` is invalid — allowed: alphanumeric + `-`"
        )));
    }
    if name.starts_with('-') || name.ends_with('-') {
        return Err(CliError::Other(format!(
            "target name `{name}` must not start or end with `-`"
        )));
    }
    Ok(())
}

fn require_known_provider(provider: Option<&str>) -> Result<String> {
    let provider = provider.ok_or_else(|| {
        CliError::Other(format!(
            "`--provider` is required (supported: {})",
            SUPPORTED_PROVIDERS.join(", ")
        ))
    })?;
    if !SUPPORTED_PROVIDERS.contains(&provider) {
        return Err(CliError::Other(format!(
            "provider `{provider}` is not supported in v0.1.73 (supported: {})",
            SUPPORTED_PROVIDERS.join(", ")
        )));
    }
    Ok(provider.to_string())
}

fn require_token(provider: &str, token: Option<&str>) -> Result<String> {
    let token = token.ok_or_else(|| {
        CliError::Other(format!(
            "`--token` is required for provider `{provider}` (or set `HCLOUD_TOKEN` env var)"
        ))
    })?;
    if provider == "hetzner-cloud" {
        validate_hetzner_token_format(token)
            .map_err(|reason| CliError::Other(format!("invalid Hetzner Cloud token: {reason}")))?;
    }
    Ok(token.to_string())
}

fn verify_ssh_key_readable(path: &Path) -> Result<()> {
    if !path.exists() {
        return Err(CliError::Other(format!(
            "SSH key path `{}` does not exist",
            path.display()
        )));
    }
    // Surface unreadable file early — read_to_string is fine for
    // a public key (small file). Don't keep the contents around;
    // the path is what gets stored in `TargetConfig`, not the
    // key body.
    std::fs::read_to_string(path).map_err(|e| {
        CliError::Other(format!("SSH key `{}` is not readable: {e}", path.display()))
    })?;
    Ok(())
}

/// Promote the supplied target to active when the store has no
/// `GlobalConfig` yet (first-run case). Returns whether the active
/// pointer changed — caller uses it to vary the confirmation
/// message between "saved + active" and "saved, active unchanged".
/// Existing stores keep their active target; users switch
/// explicitly via `apprafter target use <name>` (Track A.5).
fn ensure_active_target(paths: &TargetStorePaths, name: &str) -> Result<bool> {
    match load_global_config(paths)? {
        Some(_) => Ok(false),
        None => {
            // No global config yet — either we just created the
            // very first target, or someone hand-deleted config.yaml.
            // Either way, point active at the most-recently-saved
            // target, which by definition exists on disk now.
            let cfg = GlobalConfig {
                active_target: name.to_string(),
                version: cli_core::target::TARGET_STORE_VERSION,
            };
            save_global_config(paths, &cfg)?;
            Ok(true)
        }
    }
}

// ---------------------------------------------------------------
// Unit tests for pure validators
// ---------------------------------------------------------------
//
// `run_add` itself goes through cli_core::target IO and is
// covered end-to-end by tests/target_test.rs (integration suite).

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_target_name_accepts_kebab_lowercase() {
        for n in ["default", "work", "prod-eu", "team-2", "alpha9", "A-B-C"] {
            validate_target_name(n).unwrap_or_else(|e| panic!("name `{n}` should be valid: {e}"));
        }
    }

    #[test]
    fn validate_target_name_rejects_empty() {
        let err = validate_target_name("").expect_err("empty must error");
        match err {
            CliError::Other(msg) => assert!(msg.contains("must not be empty"), "{msg}"),
            other => panic!("expected Other, got {other:?}"),
        }
    }

    #[test]
    fn validate_target_name_rejects_punctuation() {
        for n in [
            "foo.bar",
            "with space",
            "slash/path",
            "under_score",
            "@home",
        ] {
            assert!(
                validate_target_name(n).is_err(),
                "name `{n}` should be rejected"
            );
        }
    }

    #[test]
    fn validate_target_name_rejects_leading_or_trailing_dash() {
        assert!(validate_target_name("-leading").is_err());
        assert!(validate_target_name("trailing-").is_err());
        assert!(validate_target_name("--").is_err());
    }

    #[test]
    fn validate_target_name_rejects_overlong() {
        let long = "a".repeat(MAX_TARGET_NAME_LEN + 1);
        let err = validate_target_name(&long).expect_err("too long");
        assert!(matches!(err, CliError::Other(_)));
    }

    #[test]
    fn require_known_provider_rejects_missing_flag() {
        let err = require_known_provider(None).expect_err("missing provider");
        match err {
            CliError::Other(msg) => {
                assert!(msg.contains("`--provider` is required"), "{msg}");
                assert!(msg.contains("hetzner-cloud"), "{msg}");
            }
            other => panic!("expected Other, got {other:?}"),
        }
    }

    #[test]
    fn require_known_provider_rejects_unknown_value() {
        let err = require_known_provider(Some("aws-bedrock")).expect_err("unknown provider");
        match err {
            CliError::Other(msg) => assert!(msg.contains("not supported"), "{msg}"),
            other => panic!("expected Other, got {other:?}"),
        }
    }

    #[test]
    fn require_token_validates_hetzner_format() {
        let good = "a".repeat(60);
        let token = format!("hcloud_{good}");
        assert!(require_token("hetzner-cloud", Some(&token)).is_ok());

        let bad = require_token("hetzner-cloud", Some("not_a_token")).expect_err("bad token");
        match bad {
            CliError::Other(msg) => assert!(msg.contains("invalid Hetzner Cloud token"), "{msg}"),
            other => panic!("expected Other, got {other:?}"),
        }
    }

    #[test]
    fn verify_ssh_key_readable_errors_on_missing_path() {
        let err = verify_ssh_key_readable(Path::new("/this/should/not/exist/anywhere/key.pub"))
            .expect_err("missing path");
        match err {
            CliError::Other(msg) => assert!(msg.contains("does not exist"), "{msg}"),
            other => panic!("expected Other, got {other:?}"),
        }
    }

    #[test]
    fn verify_ssh_key_readable_accepts_real_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("id_ed25519.pub");
        std::fs::write(&path, "ssh-ed25519 AAAA...").unwrap();
        assert!(verify_ssh_key_readable(&path).is_ok());
    }
}
