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

use std::io::IsTerminal;
use std::path::{Path, PathBuf};

use cli_core::target::{
    default_config_root, list_target_names, load_global_config, load_target, remove_target,
    rename_target, save_global_config, save_target, validate_hetzner_token_format, GlobalConfig,
    Target, TargetConfig, TargetCredentials, TargetStorePaths,
};
use cli_core::{CliError, Result};
use cli_providers::{HetznerCloudValidator, ProviderValidator};
use tabled::{settings::Style, Table, Tabled};
use tracing::info;

use crate::cli::TargetCommand;

use crate::commands::hcloud::hcloud_base_url;

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
            no_interactive,
            no_ping,
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
            no_interactive,
            no_ping,
        }),
        TargetCommand::List => run_list(),
        TargetCommand::Use { name } => run_use(&name),
        TargetCommand::Show { name } => run_show(name.as_deref()),
        TargetCommand::Rename { from, to } => run_rename(&from, &to),
        TargetCommand::Remove { name, yes } => run_remove(&name, yes),
    }
}

/// Plain bundle so the orchestration body below has one parameter
/// to thread instead of eleven. Field shapes mirror the clap flags
/// exactly; keep the rename pressure low by not introducing
/// intermediate types.
pub struct AddArgs {
    /// Optional because the wizard prompts for it. Mandatory at
    /// the save step — `run_add` errors with a clear message if
    /// we reach save with `name == None`.
    pub name: Option<String>,
    pub provider: Option<String>,
    pub token: Option<String>,
    pub ssh_key: Option<PathBuf>,
    pub region: Option<String>,
    pub tier: Option<String>,
    pub cluster_name: Option<String>,
    pub force: bool,
    pub renew: bool,
    pub no_interactive: bool,
    pub no_ping: bool,
}

fn run_add(mut args: AddArgs) -> Result<()> {
    // Decide before validating: the wizard is allowed to fill the
    // missing inputs, so we shouldn't reject e.g. `apprafter target
    // add` (no name) up front when the user is on a TTY. v0.1.77
    // dropped the "skip when all required supplied" short-circuit —
    // wizard now always fires on a TTY so optional fields
    // (ssh-key, tier, region) get prompted too. Pre-supplied fields
    // are silent through per-prompt prefill checks.
    let want_wizard = crate::commands::target_wizard::should_use_wizard(
        args.no_interactive,
        std::io::stdin().is_terminal(),
        std::io::stdout().is_terminal(),
    );
    if want_wizard {
        run_wizard_into_args(&mut args)?;
    }

    let name = args.name.clone().ok_or_else(|| {
        CliError::Other(
            "target name required — pass it as a positional argument (`apprafter target add <name>`) or run on a TTY to enter the wizard".to_string(),
        )
    })?;
    info!(target = %name, renew = args.renew, force = args.force, "target add invoked");
    validate_target_name(&name)?;

    let paths = TargetStorePaths::for_root(default_config_root()?);

    if args.renew {
        return run_renew(&paths, args, &name);
    }

    // Plain create / overwrite path.
    let provider = require_known_provider(args.provider.as_deref())?;
    let token = require_token(&provider, args.token.as_deref())?;
    if let Some(path) = args.ssh_key.as_ref() {
        verify_ssh_key_readable(path)?;
    }

    let existing = load_target(&paths, &name);
    match existing {
        Ok(_) if !args.force => {
            return Err(CliError::Other(format!(
                "target `{name}` already exists — pass `--force` to overwrite or `--renew` to rotate credentials only"
            )));
        }
        Ok(_) | Err(CliError::TargetNotFound { .. }) => {}
        Err(e) => return Err(e),
    }

    // API ping confirms the token actually authenticates with the
    // provider. Happens AFTER the existing-target check so a no-op
    // run with `--no-ping` against an existing target still
    // surfaces the "already exists" error immediately. `--no-ping`
    // skips the round-trip for CI / offline setups. When the
    // wizard already ran an inline ping the result is re-verified
    // here on purpose — cheap (~200ms) and keeps the save-time
    // check authoritative.
    if !args.no_ping {
        ping_provider(&provider, &token)?;
    }

    let target = Target {
        name: name.clone(),
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

    let became_active = ensure_active_target(&paths, &name)?;

    let verified_suffix = if args.no_ping {
        " (token NOT verified against the API — `--no-ping` was passed)"
    } else {
        " (token verified against Hetzner Cloud)"
    };
    if became_active {
        println!(
            "target `{name}` saved and set as active (first target on fresh store){verified_suffix}"
        );
    } else {
        println!(
            "target `{name}` saved (active target unchanged — use `apprafter target use {name}` to switch){verified_suffix}"
        );
    }
    Ok(())
}

/// Fill `args` from wizard prompts for whatever fields aren't
/// already supplied. Split out so the main `run_add` body reads
/// linearly.
fn run_wizard_into_args(args: &mut AddArgs) -> Result<()> {
    use crate::commands::target_wizard;
    if args.renew {
        // Renew wizard needs the existing target's provider, so
        // prompt for the name first (if missing) and load the
        // target so we know what provider's validator to wire.
        if args.name.is_none() {
            let n = inquire::Text::new("Target name to rotate credentials for:")
                .with_validator(|v: &str| match check_target_name(v) {
                    Ok(()) => Ok(inquire::validator::Validation::Valid),
                    Err(msg) => Ok(inquire::validator::Validation::Invalid(msg.into())),
                })
                .prompt()
                .map_err(|err| match err {
                    inquire::InquireError::OperationCanceled
                    | inquire::InquireError::OperationInterrupted => {
                        CliError::Other("wizard aborted by user".to_string())
                    }
                    other => CliError::Other(format!("wizard prompt failed: {other}")),
                })?;
            args.name = Some(n);
        }
        let paths = TargetStorePaths::for_root(default_config_root()?);
        let existing = load_target(&paths, args.name.as_deref().unwrap())?;
        let (token, _verified) =
            target_wizard::run_renew_wizard(&existing.config.provider, args.no_ping)?;
        args.token = Some(token);
    } else {
        let out = target_wizard::run_add_wizard(args)?;
        args.name = Some(out.name);
        args.provider = Some(out.provider);
        args.token = Some(out.token);
        if args.ssh_key.is_none() {
            args.ssh_key = out.ssh_key;
        }
        if args.region.is_none() {
            args.region = out.region;
        }
        if args.tier.is_none() {
            args.tier = out.tier;
        }
        // `out.token_already_verified` is collected but currently
        // unused — the save-time ping below re-verifies (~200ms)
        // to keep the on-save check authoritative. A future
        // optimisation can short-circuit when the wizard's ping
        // already succeeded within the same invocation.
        let _ = out.token_already_verified;
    }
    Ok(())
}

fn run_renew(paths: &TargetStorePaths, args: AddArgs, name: &str) -> Result<()> {
    let mut existing = match load_target(paths, name) {
        Ok(t) => t,
        Err(CliError::TargetNotFound { .. }) => {
            return Err(CliError::Other(format!(
                "target `{name}` does not exist — drop `--renew` to create it fresh"
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

    // Reject identical-token "rotations" loudly. The wizard happily
    // accepts whatever the user types, the CLI happily accepts the
    // env var — and an operator who pastes the OLD token by
    // muscle-memory gets a green "credentials rotated" message
    // without anything actually changing in Hetzner. That's the
    // exact opposite of what `--renew` advertises. Match a token
    // by raw bytes so even a single-char drift counts as "new".
    if existing.credentials.hetzner_token.as_deref() == Some(token.as_str()) {
        return Err(CliError::Other(format!(
            "`--renew` requires a NEW token, but the value provided is identical to the one already saved for target `{name}`. Generate a fresh token in the Hetzner Cloud Console → Security → API Tokens, then re-run `apprafter target add {name} --renew` with the new value."
        )));
    }

    if let Some(path) = args.ssh_key.as_ref() {
        verify_ssh_key_readable(path)?;
        existing.config.ssh_key_path = Some(path.clone());
    }
    if !args.no_ping {
        ping_provider(&existing.config.provider, &token)?;
    }

    existing.credentials = TargetCredentials {
        hetzner_token: Some(token),
    };
    save_target(paths, &existing)?;

    let verified_suffix = if args.no_ping {
        " (token NOT verified — `--no-ping` was passed)"
    } else {
        " (token verified against Hetzner Cloud)"
    };
    println!("target `{name}` credentials rotated{verified_suffix}");
    Ok(())
}

// ---------------------------------------------------------------
// Pure validators
// ---------------------------------------------------------------

/// Pure target-name validator. Returns `Result<(), String>` so
/// callers can pick the right error wrapping (CliError for direct
/// CLI surface; `inquire::Validation::Invalid` for wizard
/// prompts). The string body is reused verbatim in both paths so
/// error UX stays consistent.
pub(crate) fn check_target_name(name: &str) -> std::result::Result<(), String> {
    if name.is_empty() {
        return Err("target name must not be empty".to_string());
    }
    if name.len() > MAX_TARGET_NAME_LEN {
        return Err(format!(
            "target name must be ≤ {MAX_TARGET_NAME_LEN} chars (got {})",
            name.len()
        ));
    }
    // Avoid filesystem-reserved characters and any path-traversal
    // surface. The pattern matches Kubernetes resource names which
    // are already familiar to operators.
    if !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        return Err(format!(
            "target name `{name}` is invalid — allowed: alphanumeric + `-`"
        ));
    }
    if name.starts_with('-') || name.ends_with('-') {
        return Err(format!(
            "target name `{name}` must not start or end with `-`"
        ));
    }
    Ok(())
}

fn validate_target_name(name: &str) -> Result<()> {
    check_target_name(name).map_err(CliError::Other)
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

/// Run the read-only `validate_credentials()` ping for the
/// provider. For Hetzner Cloud this is `GET /v1/locations`
/// against the production base URL (overridable via
/// `APPRAFTER_HCLOUD_BASE_URL` through `hcloud_base_url()` —
/// integration tests redirect against a `mockito::Server`).
///
/// Failure messages get wrapped in a CLI-shaped surface so the
/// user sees "Hetzner Cloud rejected the token …" instead of the
/// raw API envelope. Existing typed `CliError::Hetzner` variants
/// from the underlying call are preserved for `--verbose` paths
/// in future Track A.10 (miette).
fn ping_provider(provider: &str, token: &str) -> Result<()> {
    match provider {
        "hetzner-cloud" => {
            let base = hcloud_base_url();
            tracing::debug!(provider, base = %base, "running provider validator ping");
            let validator = HetznerCloudValidator::new(base.clone(), token);
            validator.validate_credentials().map_err(|err| match err {
                CliError::Hetzner { status: 401, message, .. } => CliError::Other(
                    format!("Hetzner Cloud rejected the token (HTTP 401): {message}. Double-check that the token has not been revoked and was copied complete (64 chars)."),
                ),
                CliError::Hetzner { status, message, .. } => CliError::Other(format!(
                    "Hetzner Cloud API ping failed (HTTP {status}): {message}. The target was NOT saved; rerun once the API is reachable, or pass `--no-ping` to skip the check."
                )),
                CliError::Other(msg) => CliError::Other(format!(
                    "could not reach Hetzner Cloud at {base}: {msg}. Pass `--no-ping` to skip the network round-trip (offline / CI setups)."
                )),
                other => other,
            })
        }
        _ => {
            // Defensive: require_known_provider already gates
            // on the whitelist, so this arm should be
            // unreachable. Surface a typed error rather than
            // panic so a future regression in the whitelist
            // doesn't blow up the user's shell.
            Err(CliError::Other(format!(
                "no validator wired for provider `{provider}` — pass `--no-ping` to skip"
            )))
        }
    }
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
        // Canonical 64-char alphanumeric, no prefix — what Hetzner
        // Cloud Console actually issues.
        let token = "a".repeat(64);
        assert!(require_token("hetzner-cloud", Some(&token)).is_ok());

        // Underscore is non-alphanumeric → rejected. Pinning this
        // case to make sure the v0.1.73 regression (which required
        // an `hcloud_` prefix) never sneaks back.
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

    #[test]
    fn token_summary_renders_set_or_not_set_without_leaking_bytes() {
        assert_eq!(token_summary(None), "not set");
        let summary = token_summary(Some("aaaaaaaaaaaaaaaa"));
        assert!(summary.contains("set"), "{summary}");
        assert!(summary.contains("16 chars"), "{summary}");
        // No literal token bytes in the rendered string.
        assert!(
            !summary.contains("aaaaaaaaaaaaaaaa"),
            "summary must not echo the token: {summary}"
        );
    }
}

// ---------------------------------------------------------------
// CRUD subcommands (Track A.5 / v0.1.79) — `list / use / show /
// rename / remove`. Built on top of the existing target store
// from Track A.2; CLI surface mirrors the kubectl-style verbs.
// ---------------------------------------------------------------

/// One row of `apprafter target list`. `tabled` derives the
/// header text from the field names + `#[tabled(rename = "...")]`
/// attributes; the `Active` column is a single `*` for the active
/// target and blank otherwise so the marker scans visually.
#[derive(Tabled)]
struct TargetListRow {
    #[tabled(rename = "Active")]
    active: String,
    #[tabled(rename = "Name")]
    name: String,
    #[tabled(rename = "Provider")]
    provider: String,
    #[tabled(rename = "Region")]
    region: String,
    #[tabled(rename = "Tier")]
    tier: String,
}

fn run_list() -> Result<()> {
    info!("target list invoked");
    let paths = TargetStorePaths::for_root(default_config_root()?);
    let names = list_target_names(&paths)?;
    if names.is_empty() {
        println!(
            "No targets configured. Run `apprafter target add` to create one — or `apprafter target add <name>` to skip the wizard's name prompt."
        );
        return Ok(());
    }
    let active = load_global_config(&paths)?
        .map(|g| g.active_target)
        .unwrap_or_default();

    let mut rows: Vec<TargetListRow> = Vec::with_capacity(names.len());
    for name in &names {
        // load_target needs both config.yaml and (optionally)
        // credentials.yaml. We only care about config here; if
        // either file is broken we skip the row with a tracing
        // warning rather than erroring out the whole listing.
        let cfg = match load_target(&paths, name) {
            Ok(t) => t.config,
            Err(e) => {
                tracing::warn!(target = %name, error = %e, "skipping unreadable target in list");
                continue;
            }
        };
        rows.push(TargetListRow {
            active: if *name == active {
                "*".into()
            } else {
                String::new()
            },
            name: name.clone(),
            provider: cfg.provider,
            region: cfg.region.unwrap_or_else(|| "-".into()),
            tier: cfg.default_tier.unwrap_or_else(|| "-".into()),
        });
    }

    let mut table = Table::new(&rows);
    table.with(Style::sharp());
    println!("{table}");
    println!();
    if active.is_empty() {
        println!(
            "{} targets configured. No active target — run `apprafter target use <name>` to pick one.",
            rows.len()
        );
    } else {
        println!("{} targets configured. Active: '{active}'.", rows.len());
    }
    Ok(())
}

fn run_use(name: &str) -> Result<()> {
    info!(target = %name, "target use invoked");
    let paths = TargetStorePaths::for_root(default_config_root()?);
    // `load_target` returns TargetNotFound with an `available`
    // hint when the name doesn't exist — we let that surface
    // verbatim.
    let _ = load_target(&paths, name)?;

    let mut global = load_global_config(&paths)?.unwrap_or_default();
    if global.active_target == name {
        println!("target `{name}` was already the active target");
        return Ok(());
    }
    let previous = std::mem::replace(&mut global.active_target, name.to_string());
    save_global_config(&paths, &global)?;
    if previous.is_empty() {
        println!("active target set to `{name}`");
    } else {
        println!("active target switched: `{previous}` → `{name}`");
    }
    Ok(())
}

fn run_show(name: Option<&str>) -> Result<()> {
    let paths = TargetStorePaths::for_root(default_config_root()?);
    let active = load_global_config(&paths)?
        .map(|g| g.active_target)
        .unwrap_or_default();

    let resolved = match name {
        Some(n) => n.to_string(),
        None => {
            if active.is_empty() {
                return Err(CliError::Other(
                    "no active target and no name supplied. Run `apprafter target list` to see configured targets, or `apprafter target add` to create one."
                        .to_string(),
                ));
            }
            active.clone()
        }
    };
    info!(target = %resolved, "target show invoked");
    let target = load_target(&paths, &resolved)?;

    let is_active = resolved == active;
    let active_marker = if is_active { " (active)" } else { "" };

    println!("Target: {resolved}{active_marker}");
    println!("  Provider:    {}", target.config.provider);
    println!(
        "  Region:      {}",
        target.config.region.as_deref().unwrap_or("not set")
    );
    println!(
        "  Default tier: {}",
        target.config.default_tier.as_deref().unwrap_or("not set")
    );
    println!(
        "  Cluster name: {}",
        target.config.cluster_name.as_deref().unwrap_or("not set")
    );
    println!(
        "  SSH key:     {}",
        target
            .config
            .ssh_key_path
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "not set".to_string())
    );
    println!(
        "  Hetzner token: {}",
        token_summary(target.credentials.hetzner_token.as_deref())
    );
    println!();
    println!(
        "Config:      {}",
        paths.target_config_file(&resolved).display()
    );
    println!(
        "Credentials: {} (mode 0600)",
        paths.target_credentials_file(&resolved).display()
    );
    Ok(())
}

fn run_rename(from: &str, to: &str) -> Result<()> {
    info!(from = %from, to = %to, "target rename invoked");
    // Validate the destination name shape here (cli_core layer
    // stays IO-pure on names).
    if let Err(msg) = check_target_name(to) {
        return Err(CliError::Other(msg));
    }
    if from == to {
        return Err(CliError::Other(
            "source and destination target names are identical — nothing to rename".to_string(),
        ));
    }
    let paths = TargetStorePaths::for_root(default_config_root()?);

    rename_target(&paths, from, to)?;

    // Keep `GlobalConfig.active_target` pointed at the right name.
    if let Some(mut global) = load_global_config(&paths)? {
        if global.active_target == from {
            global.active_target = to.to_string();
            save_global_config(&paths, &global)?;
            println!("target renamed: `{from}` → `{to}` (active pointer updated)");
            return Ok(());
        }
    }
    println!("target renamed: `{from}` → `{to}`");
    Ok(())
}

fn run_remove(name: &str, yes: bool) -> Result<()> {
    info!(target = %name, yes, "target remove invoked");
    let paths = TargetStorePaths::for_root(default_config_root()?);
    // Load to verify existence early and surface the canonical
    // "available targets" hint when the name is wrong.
    let _ = load_target(&paths, name)?;

    if !yes {
        let stdin_tty = std::io::stdin().is_terminal();
        let stdout_tty = std::io::stdout().is_terminal();
        if !(stdin_tty && stdout_tty) {
            return Err(CliError::Other(format!(
                "non-interactive invocation: pass `--yes` to confirm removing target `{name}` (refusing silent destruction)"
            )));
        }
        let confirmed = inquire::Confirm::new(&format!(
            "Remove target `{name}`? This deletes config + credentials + cached state."
        ))
        .with_default(false)
        .prompt()
        .map_err(|err| match err {
            inquire::InquireError::OperationCanceled
            | inquire::InquireError::OperationInterrupted => {
                CliError::Other("remove aborted by user".to_string())
            }
            other => CliError::Other(format!("confirmation prompt failed: {other}")),
        })?;
        if !confirmed {
            println!("aborted; target `{name}` left intact");
            return Ok(());
        }
    }

    remove_target(&paths, name)?;

    // If the removed target was active, repoint the active marker
    // at the first remaining target alphabetically. With no
    // targets left, clear the active pointer entirely (delete
    // config.yaml) so the next `target add` flips back to the
    // "first target on fresh store" greeting.
    let mut global = load_global_config(&paths)?.unwrap_or_default();
    if global.active_target == name {
        let remaining = list_target_names(&paths)?;
        match remaining.into_iter().next() {
            Some(next) => {
                global.active_target = next.clone();
                save_global_config(&paths, &global)?;
                println!(
                    "target `{name}` removed; active switched to `{next}` (alphabetically next)"
                );
            }
            None => {
                // No targets left — drop the global config file so
                // `load_global_config` returns None again, which
                // `target add` interprets as "fresh store".
                let cfg_file = paths.global_config_file();
                if cfg_file.exists() {
                    std::fs::remove_file(cfg_file)?;
                }
                println!("target `{name}` removed; no targets left, active pointer cleared");
            }
        }
    } else {
        println!("target `{name}` removed");
    }
    Ok(())
}

/// One-line summary of a stored token suitable for `target show`.
/// We intentionally do NOT echo any of the token bytes — even the
/// last 4 chars are identifying. The user reads
/// `credentials.yaml` directly when they need the raw value.
fn token_summary(token: Option<&str>) -> String {
    match token {
        None => "not set".to_string(),
        Some(t) => format!(
            "set ({} chars; read credentials.yaml for the raw value)",
            t.len()
        ),
    }
}
