// SPDX-License-Identifier: FSL-1.1-Apache-2.0
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
use cli_providers::hetzner_cloud::validate_server_type;
use cli_providers::{HetznerCloudClient, HetznerCloudValidator, ProviderValidator};
use cli_state::State;
use tabled::{settings::Style, Table, Tabled};
use tracing::info;

use crate::cli::{TargetCertCommand, TargetCommand};
use crate::commands::k8s_helpers::{
    ensure_kubeconfig_tempfile, kubectl_apply_server_side, kubectl_get_json,
};
use cli_providers::cert::{
    build_tls_secret, expiry_status, parse_and_validate, validate_cert_name, ExpiryStatus,
};

use crate::commands::hcloud::hcloud_base_url;
use crate::commands::state_paths::resolve_state_paths;

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
            server_type,
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
            server_type,
        }),
        TargetCommand::List => run_list(),
        TargetCommand::Use { name } => run_use(&name),
        TargetCommand::Show { name } => run_show(name.as_deref()),
        TargetCommand::Rename { from, to } => run_rename(&from, &to),
        TargetCommand::Remove { name, yes } => run_remove(&name, yes),
        TargetCommand::Cert { action } => run_cert(action),
        TargetCommand::Domain { action } => crate::commands::target_domain::run(action),
        TargetCommand::Firewall { action } => crate::commands::target_firewall::run(action),
        TargetCommand::Ip => run_ip(),
        TargetCommand::Machine {
            target,
            server_type,
            no_ping,
        } => crate::commands::target_machine::run_machine(
            crate::commands::target_machine::MachineArgs {
                target,
                server_type,
                no_ping,
            },
        ),
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
    /// 2.16h: preferred server type SKU to persist in the target store.
    pub server_type: Option<String>,
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

    // 2.16h: if a server type SKU was supplied, validate it against the
    // live API for the resolved region before saving. Skipped when
    // `--no-ping` is set (same rationale as the token ping above).
    if let Some(ref sku) = args.server_type {
        if args.no_ping {
            println!("{}", sku_not_validated_line(sku));
        } else {
            let resolved_region = region_for_sku_check(args.region.as_deref());
            let client = HetznerCloudClient::new(hcloud_base_url(), &token);
            let types = client.list_server_types()?.server_types;
            validate_server_type(&types, sku, resolved_region)?;
        }
    }

    let target = Target {
        name: name.clone(),
        config: TargetConfig {
            provider,
            region: args.region,
            default_tier: args.tier,
            cluster_name: args.cluster_name,
            ssh_key_path: args.ssh_key,
            firewall: None,
            server_type: args.server_type,
        },
        credentials: TargetCredentials {
            hetzner_token: Some(token),
        },
    };
    save_target(&paths, &target)?;

    let became_active = ensure_active_target(&paths, &name)?;

    let verified_suffix = add_verified_suffix(args.no_ping);
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

/// Same idea as [`add_verified_suffix`], for the `--renew` path — a rotation
/// that never touched the API has not proved the NEW token works.
pub(crate) fn renew_verified_suffix(no_ping: bool) -> &'static str {
    if no_ping {
        " (token NOT verified — `--no-ping` was passed)"
    } else {
        " (token verified against Hetzner Cloud)"
    }
}

/// Notice printed when `--no-ping` skipped the SKU check. It has to be loud:
/// the value lands in the target store either way, and only the next `apply`
/// will find out it does not exist.
pub(crate) fn sku_not_validated_line(sku: &str) -> String {
    format!("server type `{sku}` NOT validated against the Hetzner API (`--no-ping` was passed)")
}

/// Region a `--server-type` is checked against at `target add` time. Server
/// types are per-location on Hetzner, so an unset `--region` still needs the
/// same default the rest of the CLI provisions into.
pub(crate) fn region_for_sku_check(region: Option<&str>) -> &str {
    region.unwrap_or(crate::commands::target_machine::DEFAULT_REGION)
}

/// Suffix on the `target add` confirmation stating whether the token was
/// actually authenticated. `--no-ping` saves an UNVERIFIED credential, and the
/// operator has to be told rather than left assuming a green run means a
/// working token.
pub(crate) fn add_verified_suffix(no_ping: bool) -> &'static str {
    if no_ping {
        " (token NOT verified against the API — `--no-ping` was passed)"
    } else {
        " (token verified against Hetzner Cloud)"
    }
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
                .map_err(map_wizard_prompt_error)?;
            args.name = Some(n);
        }
        let paths = TargetStorePaths::for_root(default_config_root()?);
        let existing = load_target(&paths, args.name.as_deref().unwrap())?;
        let (token, _verified) =
            target_wizard::run_renew_wizard(&existing.config.provider, args.no_ping)?;
        args.token = Some(token);
    } else {
        let out = target_wizard::run_add_wizard(args)?;
        merge_wizard_output(args, out);
    }
    Ok(())
}

/// Fold the wizard's answers into `args`.
///
/// Name / provider / token always come from the wizard (it prefills them from
/// the flags and re-emits whatever the operator confirmed). Every OPTIONAL
/// field is flag-wins: an explicitly-passed `--region` / `--tier` /
/// `--ssh-key` / `--server-type` must survive a wizard run that defaulted it,
/// or the flag silently does nothing.
pub(crate) fn merge_wizard_output(
    args: &mut AddArgs,
    out: crate::commands::target_wizard::WizardOutput,
) {
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
    // Merge the wizard's machine-matrix choice for server_type.
    // The flag (`--server-type`) takes precedence; the matrix
    // result fills the field only when the flag was absent.
    if args.server_type.is_none() {
        args.server_type = out.server_type;
    }
    // `out.token_already_verified` is collected but currently
    // unused — the save-time ping below re-verifies (~200ms)
    // to keep the on-save check authoritative. A future
    // optimisation can short-circuit when the wizard's ping
    // already succeeded within the same invocation.
    let _ = out.token_already_verified;
}

/// Translate an `inquire` prompt failure raised inside the wizard.
///
/// A Ctrl-C / Esc is a deliberate abort and must read like one; anything else
/// keeps its underlying detail so a broken terminal is diagnosable.
pub(crate) fn map_wizard_prompt_error(err: inquire::InquireError) -> CliError {
    match err {
        inquire::InquireError::OperationCanceled | inquire::InquireError::OperationInterrupted => {
            CliError::Other("wizard aborted by user".to_string())
        }
        other => CliError::Other(format!("wizard prompt failed: {other}")),
    }
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
    reject_config_flags_on_renew(
        args.provider.as_deref(),
        args.region.as_deref(),
        args.tier.as_deref(),
        args.cluster_name.as_deref(),
    )?;

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
    reject_identical_token(existing.credentials.hetzner_token.as_deref(), &token, name)?;

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

    println!(
        "target `{name}` credentials rotated{}",
        renew_verified_suffix(args.no_ping)
    );
    Ok(())
}

// ---------------------------------------------------------------
// Pure validators
// ---------------------------------------------------------------

/// `--renew` rotates credentials and nothing else. Refusing the config flags
/// up front beats silently dropping a value the operator clearly meant to
/// change.
pub(crate) fn reject_config_flags_on_renew(
    provider: Option<&str>,
    region: Option<&str>,
    tier: Option<&str>,
    cluster_name: Option<&str>,
) -> Result<()> {
    if provider.is_some() || region.is_some() || tier.is_some() || cluster_name.is_some() {
        return Err(CliError::Other(
            "`--renew` only updates credentials — `--provider`, `--region`, `--tier`, `--cluster-name` are not allowed alongside it. Drop `--renew` if you want to change config too.".to_string(),
        ));
    }
    Ok(())
}

/// Reject an identical-token "rotation" loudly.
///
/// The wizard happily accepts whatever the operator types and the CLI happily
/// accepts the env var — so someone who pastes the OLD token by muscle memory
/// otherwise gets a green "credentials rotated" with nothing rotated. Compared
/// by raw bytes: a single-character drift counts as new.
pub(crate) fn reject_identical_token(existing: Option<&str>, new: &str, name: &str) -> Result<()> {
    if existing == Some(new) {
        return Err(CliError::Other(format!(
            "`--renew` requires a NEW token, but the value provided is identical to the one already saved for target `{name}`. Generate a fresh token in the Hetzner Cloud Console → Security → API Tokens, then re-run `apprafter target add {name} --renew` with the new value."
        )));
    }
    Ok(())
}

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
/// v0.1.87: failures classify into one of two typed CliError
/// variants (`ProviderTokenRejected` for 401, otherwise
/// `ProviderApiUnreachable`) and carry the original error as a
/// `#[source]` cause chain. Miette renders both layers — operator
/// gets the high-level rotation / reachability help PLUS the raw
/// API envelope underneath.
fn ping_provider(provider: &str, token: &str) -> Result<()> {
    match provider {
        "hetzner-cloud" => {
            let base = hcloud_base_url();
            tracing::debug!(provider, base = %base, "running provider validator ping");
            let validator = HetznerCloudValidator::new(base.clone(), token);
            validator
                .validate_credentials()
                .map_err(|err| classify_ping_error(provider, err))
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

/// Sort a credential-validation error into the right typed variant.
/// 401 → `ProviderTokenRejected` (operator can rotate); everything
/// else → `ProviderApiUnreachable` (operator can run
/// `apprafter doctor` or wait out the outage). Both wrappers carry
/// the original error as a `#[source]` cause chain so miette
/// renders both the top-level summary and the underlying API
/// envelope. Shared with the wizard's classification path so both
/// flows emit identical diagnostic codes.
fn classify_ping_error(provider: &str, err: CliError) -> CliError {
    match err {
        CliError::Hetzner { status: 401, .. } => CliError::ProviderTokenRejected {
            provider: provider.to_string(),
            cause: Box::new(err),
        },
        _ => CliError::ProviderApiUnreachable {
            provider: provider.to_string(),
            cause: Box::new(err),
        },
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

    // ── ping_provider / classify_ping_error ──────────────────────────────

    /// A 401 means the token is wrong and the operator can fix it by rotating;
    /// anything else means the API could not be reached and rotating would be
    /// a waste of time. The two must not collapse into one diagnostic.
    #[test]
    fn a_401_classifies_as_a_rejected_token_and_everything_else_as_unreachable() {
        let rejected = classify_ping_error(
            "hetzner-cloud",
            CliError::Hetzner {
                endpoint: "GET /v1/locations".to_string(),
                status: 401,
                code: "unauthorized".to_string(),
                message: "unauthorized".to_string(),
            },
        );
        assert!(
            matches!(rejected, CliError::ProviderTokenRejected { .. }),
            "{rejected:?}"
        );

        let unreachable = classify_ping_error(
            "hetzner-cloud",
            CliError::Hetzner {
                endpoint: "GET /v1/locations".to_string(),
                status: 503,
                code: "unavailable".to_string(),
                message: "maintenance".to_string(),
            },
        );
        assert!(
            matches!(unreachable, CliError::ProviderApiUnreachable { .. }),
            "{unreachable:?}"
        );

        let offline = classify_ping_error("hetzner-cloud", CliError::Other("dns".to_string()));
        assert!(
            matches!(offline, CliError::ProviderApiUnreachable { .. }),
            "{offline:?}"
        );
    }

    /// The classified error keeps the original as its `#[source]` cause, so
    /// miette renders the raw API envelope under the high-level help. Losing
    /// it would leave the operator with advice and no evidence.
    #[test]
    fn a_classified_ping_error_keeps_the_original_as_its_cause() {
        let classified = classify_ping_error(
            "hetzner-cloud",
            CliError::Hetzner {
                endpoint: "GET /v1/locations".to_string(),
                status: 401,
                code: "unauthorized".to_string(),
                message: "token invalid".to_string(),
            },
        );
        let cause = std::error::Error::source(&classified).expect("a cause chain");
        assert!(format!("{cause}").contains("token invalid"), "{cause}");
    }

    /// A provider that slips past the whitelist must surface a typed error
    /// with a way forward, not panic the operator's shell.
    #[test]
    fn an_unwired_provider_errors_instead_of_panicking() {
        let err = ping_provider("aws", "irrelevant").expect_err("no validator for aws");
        let msg = format!("{err}");
        assert!(msg.contains("aws"), "{msg}");
        assert!(msg.contains("--no-ping"), "{msg}");
    }

    // ── renew guards ─────────────────────────────────────────────────────

    /// Each config flag on its own is enough to refuse: `--renew` silently
    /// dropping a `--region` the operator passed is exactly the failure this
    /// guard exists to prevent.
    #[test]
    fn every_config_flag_is_refused_alongside_renew() {
        assert!(reject_config_flags_on_renew(None, None, None, None).is_ok());
        for (p, r, t, c) in [
            (Some("hetzner-cloud"), None, None, None),
            (None, Some("hel1"), None, None),
            (None, None, Some("1"), None),
            (None, None, None, Some("platform-2")),
        ] {
            let err = reject_config_flags_on_renew(p, r, t, c)
                .expect_err("a config flag alongside --renew must be refused");
            assert!(
                format!("{err}").contains("only updates credentials"),
                "{err}"
            );
        }
    }

    /// Re-pasting the SAME token must fail loudly. A green "credentials
    /// rotated" that rotated nothing is worse than an error — the operator
    /// believes the old, possibly leaked, token is out of use.
    #[test]
    fn re_pasting_the_same_token_is_refused_but_one_char_counts_as_new() {
        let old = "a".repeat(64);
        let err = reject_identical_token(Some(&old), &old, "work")
            .expect_err("an identical token is not a rotation");
        let msg = format!("{err}");
        assert!(msg.contains("requires a NEW token"), "{msg}");
        assert!(msg.contains("work"), "{msg}");

        let nearly = format!("{}b", &old[..63]);
        assert!(reject_identical_token(Some(&old), &nearly, "work").is_ok());
        assert!(reject_identical_token(None, &old, "work").is_ok());
    }

    // ── verification suffixes ────────────────────────────────────────────

    /// A `--no-ping` save must say the token was NOT verified. Rendering it
    /// like a checked one lets a typo'd credential sit in the store until the
    /// first `apply` fails.
    #[test]
    fn the_unverified_suffixes_say_so_on_both_add_and_renew() {
        assert!(add_verified_suffix(true).contains("NOT verified"));
        assert!(!add_verified_suffix(false).contains("NOT"));
        assert!(add_verified_suffix(false).contains("verified against Hetzner Cloud"));

        assert!(renew_verified_suffix(true).contains("NOT verified"));
        assert!(!renew_verified_suffix(false).contains("NOT"));
    }

    /// Same contract for the unvalidated SKU notice.
    #[test]
    fn the_unvalidated_sku_notice_names_the_sku_and_the_flag() {
        let line = sku_not_validated_line("cx42");
        assert!(line.contains("cx42"), "{line}");
        assert!(line.contains("NOT validated"), "{line}");
        assert!(line.contains("--no-ping"), "{line}");
    }

    /// Server types are per-location, so an unset `--region` must fall back to
    /// the same default the CLI provisions into — checking against a different
    /// one would pass here and fail at `apply`.
    #[test]
    fn the_sku_check_region_defaults_to_the_provisioning_default() {
        assert_eq!(region_for_sku_check(Some("hel1")), "hel1");
        assert_eq!(
            region_for_sku_check(None),
            crate::commands::target_machine::DEFAULT_REGION
        );
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

    // ── merge_wizard_output ──────────────────────────────────────────────

    fn wizard_output() -> crate::commands::target_wizard::WizardOutput {
        crate::commands::target_wizard::WizardOutput {
            name: "from-wizard".to_string(),
            provider: "hetzner-cloud".to_string(),
            token: "wizard-token".to_string(),
            ssh_key: Some(PathBuf::from("/wizard/id.pub")),
            region: Some("wizard-region".to_string()),
            tier: Some("2".to_string()),
            server_type: Some("wizard-sku".to_string()),
            token_already_verified: true,
        }
    }

    fn empty_args() -> AddArgs {
        AddArgs {
            name: None,
            provider: None,
            token: None,
            ssh_key: None,
            region: None,
            tier: None,
            cluster_name: None,
            force: false,
            renew: false,
            no_interactive: false,
            no_ping: false,
            server_type: None,
        }
    }

    /// Every OPTIONAL field is flag-wins. A `--region` / `--tier` /
    /// `--ssh-key` / `--server-type` the operator passed explicitly must
    /// survive a wizard run that defaulted it — otherwise the flag silently
    /// does nothing.
    #[test]
    fn explicit_flags_survive_the_wizard_merge() {
        let mut args = AddArgs {
            ssh_key: Some(PathBuf::from("/flag/id.pub")),
            region: Some("flag-region".to_string()),
            tier: Some("1".to_string()),
            server_type: Some("flag-sku".to_string()),
            ..empty_args()
        };
        merge_wizard_output(&mut args, wizard_output());
        assert_eq!(args.ssh_key, Some(PathBuf::from("/flag/id.pub")));
        assert_eq!(args.region.as_deref(), Some("flag-region"));
        assert_eq!(args.tier.as_deref(), Some("1"));
        assert_eq!(args.server_type.as_deref(), Some("flag-sku"));
    }

    /// Unset optional fields DO take the wizard's answers — otherwise every
    /// prompt the operator just answered would be thrown away.
    #[test]
    fn unset_fields_take_the_wizards_answers() {
        let mut args = empty_args();
        merge_wizard_output(&mut args, wizard_output());
        assert_eq!(args.ssh_key, Some(PathBuf::from("/wizard/id.pub")));
        assert_eq!(args.region.as_deref(), Some("wizard-region"));
        assert_eq!(args.tier.as_deref(), Some("2"));
        assert_eq!(args.server_type.as_deref(), Some("wizard-sku"));
    }

    /// Name / provider / token always come from the wizard: it prefills them
    /// from the flags and re-emits whatever the operator actually confirmed,
    /// so keeping a stale flag value here would ignore a correction.
    #[test]
    fn the_wizard_owns_name_provider_and_token() {
        let mut args = AddArgs {
            name: Some("typo".to_string()),
            provider: Some("stale".to_string()),
            token: Some("stale-token".to_string()),
            ..empty_args()
        };
        merge_wizard_output(&mut args, wizard_output());
        assert_eq!(args.name.as_deref(), Some("from-wizard"));
        assert_eq!(args.provider.as_deref(), Some("hetzner-cloud"));
        assert_eq!(args.token.as_deref(), Some("wizard-token"));
    }

    // ── prompt error mapping ─────────────────────────────────────────────

    /// Ctrl-C / Esc is a deliberate abort in both prompts and must read as
    /// one; a genuine terminal fault keeps its detail. The two prompts word
    /// their abort differently on purpose (wizard vs removal), so both are
    /// pinned.
    #[test]
    fn cancelling_a_prompt_reads_as_an_abort_in_both_flows() {
        for cancel in [
            inquire::InquireError::OperationCanceled,
            inquire::InquireError::OperationInterrupted,
        ] {
            assert_eq!(
                format!("{}", map_wizard_prompt_error(cancel)),
                "wizard aborted by user"
            );
        }
        for cancel in [
            inquire::InquireError::OperationCanceled,
            inquire::InquireError::OperationInterrupted,
        ] {
            assert_eq!(
                format!("{}", map_remove_prompt_error(cancel)),
                "remove aborted by user"
            );
        }
    }

    #[test]
    fn a_genuine_prompt_fault_keeps_its_cause_in_both_flows() {
        let wizard = format!(
            "{}",
            map_wizard_prompt_error(inquire::InquireError::InvalidConfiguration(
                "no tty".to_string()
            ))
        );
        assert!(wizard.contains("wizard prompt failed"), "{wizard}");
        assert!(wizard.contains("no tty"), "{wizard}");

        let remove = format!(
            "{}",
            map_remove_prompt_error(inquire::InquireError::InvalidConfiguration(
                "no tty".to_string()
            ))
        );
        assert!(remove.contains("confirmation prompt failed"), "{remove}");
        assert!(remove.contains("no tty"), "{remove}");
    }

    // ── destructive-command copy ─────────────────────────────────────────

    /// The removal prompt must enumerate WHAT is destroyed — an operator who
    /// reads "remove target" alone does not expect the cached kubeconfig and
    /// credentials to go with it.
    #[test]
    fn the_removal_prompt_spells_out_what_is_destroyed() {
        let p = remove_prompt("prod-eu");
        assert!(p.contains("prod-eu"), "{p}");
        assert!(p.contains("credentials"), "{p}");
        assert!(p.contains("state"), "{p}");
    }

    #[test]
    fn declining_a_removal_says_the_target_survived() {
        let line = remove_aborted_line("prod-eu");
        assert!(line.contains("prod-eu"), "{line}");
        assert!(line.contains("left intact"), "{line}");
    }

    // ── show / rename decisions ──────────────────────────────────────────

    /// `target show` falls back to the active target, and an explicit name
    /// always wins over it — otherwise `target show other` would silently
    /// print the active target's credentials summary instead.
    #[test]
    fn show_prefers_an_explicit_name_over_the_active_target() {
        assert_eq!(resolve_show_target(Some("other"), "work").unwrap(), "other");
        assert_eq!(resolve_show_target(None, "work").unwrap(), "work");
        // An explicit name works even on a store with no active pointer.
        assert_eq!(resolve_show_target(Some("other"), "").unwrap(), "other");
    }

    /// On a fresh store there is nothing to show, and the error has to name
    /// BOTH ways out — an operator cannot guess "add one first" from a bare
    /// "not found".
    #[test]
    fn show_without_a_name_or_an_active_target_points_at_both_ways_out() {
        let err = resolve_show_target(None, "").expect_err("nothing to show");
        let msg = format!("{err}");
        assert!(msg.contains("apprafter target list"), "{msg}");
        assert!(msg.contains("apprafter target add"), "{msg}");
    }

    /// A self-rename is refused rather than performed as a no-op that reports
    /// success, and the DESTINATION name is shape-checked before the store is
    /// touched (a rename to `../evil` must never reach the filesystem).
    #[test]
    fn rename_refuses_a_self_rename_and_a_malformed_destination() {
        assert!(check_rename("work", "prod-eu").is_ok());

        let same = check_rename("work", "work").expect_err("a self-rename is a no-op");
        assert!(format!("{same}").contains("identical"), "{same}");

        for bad in ["../evil", "with space", "-leading", ""] {
            assert!(
                check_rename("work", bad).is_err(),
                "destination `{bad}` must be rejected"
            );
        }
    }

    // ── list / use readouts ──────────────────────────────────────────────

    /// With no active target the footer must SAY so and name the command that
    /// sets one; an empty `Active:` field reads like a corrupted store.
    #[test]
    fn the_list_footer_distinguishes_no_active_target_from_one() {
        let none = list_summary_line(3, "");
        assert!(none.contains('3'), "{none}");
        assert!(none.contains("No active target"), "{none}");
        assert!(none.contains("apprafter target use"), "{none}");

        let some = list_summary_line(3, "work");
        assert!(some.contains("work"), "{some}");
        assert!(!some.contains("No active target"), "{some}");
    }

    /// Switching away names the target being left behind — walking off a
    /// production target by accident is exactly what this readout catches.
    #[test]
    fn switching_the_active_target_names_the_one_left_behind() {
        let switched = switched_active_line("prod-eu", "work");
        assert!(switched.contains("prod-eu"), "{switched}");
        assert!(switched.contains("work"), "{switched}");

        let first = switched_active_line("", "work");
        assert!(first.contains("work"), "{first}");
        assert!(!first.contains("switched"), "{first}");
    }

    // ── target ip readout ────────────────────────────────────────────────

    /// The IPv4 line is ALWAYS emitted — as a record when Hetzner reported one
    /// and as an explicit "none reported" otherwise. A silently absent A line
    /// is indistinguishable from a rendering bug.
    #[test]
    fn the_ip_readout_always_accounts_for_ipv4() {
        let both = ip_report_lines(Some("203.0.113.7"), Some("2a01:db8::1")).join("\n");
        assert!(both.contains("A    record → 203.0.113.7"), "{both}");
        assert!(both.contains("AAAA record → 2a01:db8::1"), "{both}");

        let v4_only = ip_report_lines(Some("203.0.113.7"), None).join("\n");
        assert!(v4_only.contains("203.0.113.7"), "{v4_only}");
        assert!(!v4_only.contains("AAAA"), "{v4_only}");

        let neither = ip_report_lines(None, None).join("\n");
        assert!(neither.contains("no IPv4 reported"), "{neither}");
    }

    /// A node with IPv6 only still gets its AAAA record printed alongside the
    /// explicit "no IPv4" note — dropping either would leave the operator
    /// unable to point DNS anywhere.
    #[test]
    fn an_ipv6_only_node_still_reports_its_aaaa_record() {
        let text = ip_report_lines(None, Some("2a01:db8::1")).join("\n");
        assert!(text.contains("no IPv4 reported"), "{text}");
        assert!(text.contains("AAAA record → 2a01:db8::1"), "{text}");
    }

    /// The readout ends by naming the next command — the records are useless
    /// until a zone is registered against them.
    #[test]
    fn the_ip_readout_points_at_the_domain_command() {
        let text = ip_report_lines(Some("203.0.113.7"), None).join("\n");
        assert!(text.contains("apprafter target domain add"), "{text}");
    }

    // ── cert import ──────────────────────────────────────────────────────

    fn at(secs: i64) -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::from_timestamp(secs, 0).expect("valid timestamp")
    }

    /// An expired or not-yet-valid cert is refused outright: importing it
    /// would wire the Gateway to a listener every browser rejects. The error
    /// carries the offending boundary so the operator can see WHY.
    #[test]
    fn an_out_of_window_certificate_is_refused_with_its_boundary() {
        let expired = check_cert_validity("cf", ExpiryStatus::Expired, at(0), at(1_000))
            .expect_err("an expired cert must not import");
        let msg = format!("{expired}");
        assert!(msg.contains("expired"), "{msg}");
        assert!(msg.contains(&at(1_000).to_rfc3339()), "{msg}");

        let early = check_cert_validity("cf", ExpiryStatus::NotYetValid, at(2_000), at(9_000))
            .expect_err("a not-yet-valid cert must not import");
        let msg = format!("{early}");
        assert!(msg.contains("not yet valid"), "{msg}");
        assert!(msg.contains(&at(2_000).to_rfc3339()), "{msg}");
    }

    /// A near-expiry cert still imports — refusing it would strand an operator
    /// rotating late — but it must warn, naming the cert and the days left.
    #[test]
    fn a_near_expiry_certificate_imports_with_a_warning() {
        let warning = check_cert_validity(
            "cf-cert",
            ExpiryStatus::NearExpiry { days: 5 },
            at(0),
            at(9_000),
        )
        .expect("a near-expiry cert must still import")
        .expect("and must warn");
        assert!(warning.contains("cf-cert"), "{warning}");
        assert!(warning.contains('5'), "{warning}");

        assert_eq!(
            check_cert_validity("cf-cert", ExpiryStatus::Ok, at(0), at(9_000)).unwrap(),
            None,
            "a healthy cert must not warn"
        );
    }

    /// Overwriting a live cert is opt-in, and the refusal names the flag.
    #[test]
    fn the_existing_secret_refusal_names_the_replace_flag() {
        let msg = secret_exists_error("cf-cert", "apprafter-system");
        assert!(msg.contains("cf-cert"), "{msg}");
        assert!(msg.contains("apprafter-system"), "{msg}");
        assert!(msg.contains("--replace"), "{msg}");
    }

    /// The confirmation echoes the SANs and the expiry: a cert whose SANs do
    /// not cover the apex fails at handshake time, and this readout is the
    /// operator's only chance to catch that before registering the zone.
    #[test]
    fn the_import_confirmation_echoes_the_sans_and_the_expiry() {
        let text = cert_import_lines(
            "cf-cert",
            "apprafter-system",
            &["apprafter.dev".to_string(), "*.apprafter.dev".to_string()],
            at(1_800_000_000),
        )
        .join("\n");
        assert!(text.contains("cf-cert"), "{text}");
        assert!(text.contains("apprafter-system"), "{text}");
        assert!(text.contains("apprafter.dev, *.apprafter.dev"), "{text}");
        assert!(
            text.contains(&at(1_800_000_000).format("%Y-%m-%d").to_string()),
            "{text}"
        );
        assert!(text.contains("apprafter target domain add"), "{text}");
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

fn run_ip() -> Result<()> {
    let resolved = resolve_state_paths(None)?;
    let store = resolved.store;
    let state = State::load_or_default(&resolved.paths)?;

    let Some(server_id) = state.hetzner_cloud.as_ref().map(|h| h.server_id) else {
        println!("{NO_PROVISIONED_SERVER_HINT}");
        return Ok(());
    };

    let token = cli_core::resolve_hetzner_token(None, &store, None)?;
    let client = HetznerCloudClient::new(hcloud_base_url(), token);
    let (v4, v6) = cli_providers::node_public_ips(&client, server_id)?;

    for line in ip_report_lines(v4.as_deref(), v6.as_deref()) {
        println!("{line}");
    }
    Ok(())
}

/// Shown by `target ip` when the active target has never provisioned.
const NO_PROVISIONED_SERVER_HINT: &str =
    "No provisioned server for the active target — run `apprafter up` first.";

/// Render the DNS records for `target ip`.
///
/// The IPv4 line is ALWAYS emitted — as a record when Hetzner reported one and
/// as an explicit "none reported" otherwise, because a silently missing A
/// record is the difference between "no IPv4" and "we forgot to print it".
/// IPv6 is genuinely optional, so its line only appears when there is one.
pub(crate) fn ip_report_lines(v4: Option<&str>, v6: Option<&str>) -> Vec<String> {
    let mut lines = vec![match v4 {
        Some(ip) => format!("  A    record → {ip}"),
        None => "  (no IPv4 reported by Hetzner)".to_string(),
    }];
    if let Some(ip6) = v6 {
        lines.push(format!("  AAAA record → {ip6}"));
    }
    lines.push(String::new());
    lines.push(
        "Set these as your domain's DNS records (proxied through Cloudflare), then:".to_string(),
    );
    lines.push("  apprafter target domain add <zone> --cert <name>".to_string());
    lines
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
    println!("{}", list_summary_line(rows.len(), &active));
    Ok(())
}

/// Footer under `target list`.
///
/// With no active target the line has to say so AND name the command that sets
/// one — an empty `Active:` field reads like a corrupted store.
pub(crate) fn list_summary_line(count: usize, active: &str) -> String {
    if active.is_empty() {
        format!(
            "{count} targets configured. No active target — run `apprafter target use <name>` to pick one."
        )
    } else {
        format!("{count} targets configured. Active: '{active}'.")
    }
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
    println!("{}", switched_active_line(&previous, name));
    Ok(())
}

/// Confirmation for `target use`.
///
/// When there WAS a previous active target the line names it: switching away
/// from a production target by accident is exactly the mistake this readout
/// exists to catch.
pub(crate) fn switched_active_line(previous: &str, name: &str) -> String {
    if previous.is_empty() {
        format!("active target set to `{name}`")
    } else {
        format!("active target switched: `{previous}` → `{name}`")
    }
}

fn run_show(name: Option<&str>) -> Result<()> {
    let paths = TargetStorePaths::for_root(default_config_root()?);
    let active = load_global_config(&paths)?
        .map(|g| g.active_target)
        .unwrap_or_default();

    let resolved = resolve_show_target(name, &active)?;
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
        "  Server type: {}",
        target.config.server_type.as_deref().unwrap_or("not set")
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

/// Which target `target show` displays: the explicit name, else the active
/// one. With neither, the error has to name BOTH ways out — an operator on a
/// fresh store has no target to show and no way to guess that from "not
/// found".
pub(crate) fn resolve_show_target(name: Option<&str>, active: &str) -> Result<String> {
    match name {
        Some(n) => Ok(n.to_string()),
        None if active.is_empty() => Err(CliError::Other(
            "no active target and no name supplied. Run `apprafter target list` to see configured targets, or `apprafter target add` to create one."
                .to_string(),
        )),
        None => Ok(active.to_string()),
    }
}

/// Pre-flight for `target rename`: the DESTINATION name must be well-formed
/// (the source is validated by the store lookup), and a self-rename is refused
/// rather than performed as a no-op that reports success.
pub(crate) fn check_rename(from: &str, to: &str) -> Result<()> {
    // Validate the destination name shape here (cli_core layer
    // stays IO-pure on names).
    check_target_name(to).map_err(CliError::Other)?;
    if from == to {
        return Err(CliError::Other(
            "source and destination target names are identical — nothing to rename".to_string(),
        ));
    }
    Ok(())
}

fn run_rename(from: &str, to: &str) -> Result<()> {
    info!(from = %from, to = %to, "target rename invoked");
    check_rename(from, to)?;
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
        let confirmed = inquire::Confirm::new(&remove_prompt(name))
            .with_default(false)
            .prompt()
            .map_err(map_remove_prompt_error)?;
        if !confirmed {
            println!("{}", remove_aborted_line(name));
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

/// The removal confirmation. It has to enumerate WHAT is destroyed: an
/// operator who reads "remove target" alone does not expect the cached
/// kubeconfig and credentials to go with it.
pub(crate) fn remove_prompt(name: &str) -> String {
    format!("Remove target `{name}`? This deletes config + credentials + cached state.")
}

/// Line printed when the operator declines the removal — it must state that
/// the target survived intact.
pub(crate) fn remove_aborted_line(name: &str) -> String {
    format!("aborted; target `{name}` left intact")
}

/// Translate an `inquire` failure raised by the removal confirmation. A
/// Ctrl-C is an abort, not a terminal fault.
pub(crate) fn map_remove_prompt_error(err: inquire::InquireError) -> CliError {
    match err {
        inquire::InquireError::OperationCanceled | inquire::InquireError::OperationInterrupted => {
            CliError::Other("remove aborted by user".to_string())
        }
        other => CliError::Other(format!("confirmation prompt failed: {other}")),
    }
}

fn run_cert(action: TargetCertCommand) -> Result<()> {
    match action {
        TargetCertCommand::Import {
            name,
            cert,
            key,
            namespace,
            replace,
        } => run_cert_import(&name, &cert, &key, &namespace, replace),
    }
}

// Command fn fans out cert path, key path, namespace, and the replace
// flag — five inputs is intrinsic to the subcommand surface.
#[allow(clippy::too_many_arguments)]
fn run_cert_import(
    name: &str,
    cert_path: &std::path::Path,
    key_path: &std::path::Path,
    namespace: &str,
    replace: bool,
) -> Result<()> {
    validate_cert_name(name)?;

    let cert_pem = std::fs::read_to_string(cert_path)
        .map_err(|e| CliError::Other(format!("read cert {}: {e}", cert_path.display())))?;
    let key_pem = std::fs::read_to_string(key_path)
        .map_err(|e| CliError::Other(format!("read key {}: {e}", key_path.display())))?;

    let imported = parse_and_validate(&cert_pem, &key_pem)?;

    if let Some(warning) = check_cert_validity(
        name,
        expiry_status(imported.not_before, imported.not_after, chrono::Utc::now()),
        imported.not_before,
        imported.not_after,
    )? {
        eprintln!("{}", cli_core::style::warn(&warning));
    }

    let kc = ensure_kubeconfig_tempfile()?;

    if !replace && kubectl_get_json("secret", Some(name), Some(namespace), kc.path())?.is_some() {
        return Err(CliError::Other(secret_exists_error(name, namespace)));
    }

    let secret = build_tls_secret(name, namespace, &imported);
    let manifest = serde_json::to_string(&secret)
        .map_err(|e| CliError::Other(format!("serialize Secret: {e}")))?;
    // "apprafter-cli" == cli_providers::k8s::kubectl::APPRAFTER_CLI_FIELD_MANAGER.
    kubectl_apply_server_side(&manifest, "apprafter-cli", kc.path())?;

    for line in cert_import_lines(name, namespace, &imported.sans, imported.not_after) {
        println!("{line}");
    }
    Ok(())
}

/// Gate an import on the certificate's validity window.
///
/// An expired or not-yet-valid cert is refused outright — importing it would
/// wire the Gateway to a listener browsers reject. A near-expiry one still
/// imports but returns a warning, because refusing it would strand an operator
/// who is deliberately rotating late.
pub(crate) fn check_cert_validity(
    name: &str,
    status: ExpiryStatus,
    not_before: chrono::DateTime<chrono::Utc>,
    not_after: chrono::DateTime<chrono::Utc>,
) -> Result<Option<String>> {
    match status {
        ExpiryStatus::Expired => Err(CliError::Other(format!(
            "certificate expired (notAfter {})",
            not_after.to_rfc3339()
        ))),
        ExpiryStatus::NotYetValid => Err(CliError::Other(format!(
            "certificate not yet valid (notBefore {})",
            not_before.to_rfc3339()
        ))),
        ExpiryStatus::NearExpiry { days } => Ok(Some(format!(
            "certificate '{name}' expires in {days} days — import proceeding"
        ))),
        ExpiryStatus::Ok => Ok(None),
    }
}

/// Refusal for an import that would clobber an existing Secret. It names the
/// flag that opts in, because the safe default is to leave the live cert alone.
pub(crate) fn secret_exists_error(name: &str, namespace: &str) -> String {
    format!(
        "Secret '{name}' already exists in {namespace}. \
         Re-run with --replace to update it in place."
    )
}

/// Lines printed after a successful cert import.
///
/// The SANs and the expiry are echoed back because they are what the operator
/// must cross-check against the zone they are about to register — a cert whose
/// SANs do not cover the apex silently fails at TLS handshake time.
pub(crate) fn cert_import_lines(
    name: &str,
    namespace: &str,
    sans: &[String],
    not_after: chrono::DateTime<chrono::Utc>,
) -> Vec<String> {
    vec![
        format!("✓ Certificate '{name}' imported to {namespace}"),
        format!("  SANs:        {}", sans.join(", ")),
        format!("  Valid until: {}", not_after.format("%Y-%m-%d %H:%M UTC")),
        String::new(),
        "Register a domain that uses it:".to_string(),
        format!("  apprafter target domain add <zone> --cert {name}"),
        "(How to mint a Cloudflare Origin CA cert: docs → Public ingress → Cloudflare Origin CA cert.)"
            .to_string(),
    ]
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
