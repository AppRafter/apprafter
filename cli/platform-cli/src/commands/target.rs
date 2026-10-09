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
use std::path::PathBuf;

use apprafter_core::target::{
    self as core_target, CliDefaultPointer, SkuCheck, TargetRemoved, TargetReport,
};
use apprafter_core::{
    ActivePointerChange, CancellationToken, Context, CoreError, CoreResult, Outcome, SecretString,
    TargetRef,
};
use cli_core::target::TargetStorePaths;
use cli_core::{CliError, Result};
use tabled::{settings::Style, Table, Tabled};
use tracing::info;

use crate::cli::{TargetCertCommand, TargetCommand};
use crate::commands::k8s_helpers::{
    ensure_kubeconfig_tempfile, kubectl_apply_server_side, kubectl_get_json,
};
use cli_providers::cert::{
    build_tls_secret, expiry_status, parse_and_validate, validate_cert_name, ExpiryStatus,
};

use crate::commands::state_paths::resolve_state_paths;
use crate::commands::target_legacy;
use crate::render::core_error::report;
use crate::render::reporter::CliReporter;

/// Maximum length for a target name. Matches the spec
/// (`cli-dx-task.md` §5.1 validation rules). A short cap keeps
/// directory traversal and shell-history scenarios sane without
/// being meaningfully restrictive.
pub const MAX_TARGET_NAME_LEN: usize = 64;

/// `apprafter target …` for the sub-commands `dispatch` does not route to a core-backed arm.
/// `ip` runs on apprafter-core and renders its own errors
/// ([`crate::render::core_error::report`]); every other sub-command here is today's code, its
/// `CliError` mapped at this boundary.
pub fn run(action: TargetCommand) -> miette::Result<()> {
    match action {
        TargetCommand::Cert { action } => run_cert(action).map_err(miette::Report::new),
        TargetCommand::Domain { action } => {
            crate::commands::target_domain::run(action).map_err(miette::Report::new)
        }
        TargetCommand::Firewall { action } => {
            crate::commands::target_firewall::run(action).map_err(miette::Report::new)
        }
        TargetCommand::Ip => run_ip(),
        TargetCommand::Add { .. }
        | TargetCommand::List
        | TargetCommand::Show { .. }
        | TargetCommand::Use { .. }
        | TargetCommand::Rename { .. }
        | TargetCommand::Remove { .. }
        | TargetCommand::Machine { .. } => {
            unreachable!("`dispatch` runs this sub-command on the core")
        }
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

/// `target add` (and `--renew`) on the core. The CLI keeps the wizard, the inputs it requires
/// (name, `--provider`, `--token`) with today's texts and in today's order, the `info!` line
/// and the output; the checks, the ping, the SKU check and the save are the core's
/// (`plan_add` / `execute_add`). The save-time ping runs even after the wizard verified the
/// token (R2).
pub(crate) fn add(mut args: AddArgs) -> miette::Result<()> {
    // Decide before validating: the wizard is allowed to fill the missing inputs, so `apprafter
    // target add` (no name) is not rejected up front on a TTY. The wizard always fires on a TTY
    // so the optional fields get prompted too; pre-supplied fields are silent through
    // per-prompt prefill checks.
    let want_wizard = crate::commands::target_wizard::should_use_wizard(
        args.no_interactive,
        std::io::stdin().is_terminal(),
        std::io::stdout().is_terminal(),
    );
    // The context is built before the wizard only when the wizard runs, so the flag-driven
    // order (the `info!` line, then the context) is unchanged.
    let early = if want_wizard {
        Some(crate::context::cli_context()?)
    } else {
        None
    };
    if let Some(ctx) = &early {
        run_wizard_into_args(ctx, &mut args).map_err(report)?;
    }

    let name = args.name.clone().ok_or_else(|| {
        miette::Report::new(CliError::Other(
            "target name required — pass it as a positional argument (`apprafter target add <name>`) or run on a TTY to enter the wizard".to_string(),
        ))
    })?;
    info!(target = %name, renew = args.renew, force = args.force, "target add invoked");
    core_target::validate_name(&name).map_err(|problem| {
        target_legacy::add(
            CoreError::InvalidTargetName {
                name: name.clone(),
                problem,
            },
            "",
        )
    })?;
    let ctx = match early {
        Some(c) => c,
        None => crate::context::cli_context()?,
    }
    .with_no_ping(args.no_ping);

    if args.renew {
        return renew(&ctx, args, &name);
    }

    let supported = apprafter_core::provider::SUPPORTED_PROVIDERS;
    let provider = args.provider.clone().ok_or_else(|| {
        miette::Report::new(CliError::Other(format!(
            "`--provider` is required (supported: {})",
            supported.join(", ")
        )))
    })?;
    // Today's order: the provider is refused before the token is asked for.
    if !supported.contains(&provider.as_str()) {
        return Err(target_legacy::add(
            CoreError::UnknownProvider {
                provider,
                supported: supported.iter().map(|s| s.to_string()).collect(),
            },
            "",
        ));
    }
    let token = args.token.clone().ok_or_else(|| {
        miette::Report::new(CliError::Other(format!(
            "`--token` is required for provider `{provider}` (or set `HCLOUD_TOKEN` env var)"
        )))
    })?;
    let legacy = |e| target_legacy::add(e, &token);
    let plan = core_target::plan_add(
        &ctx,
        core_target::AddArgs {
            name: name.clone(),
            provider,
            token: SecretString::new(token.clone()),
            ssh_key: args.ssh_key,
            region: args.region,
            tier: args.tier,
            cluster_name: args.cluster_name,
            server_type: args.server_type,
            force: args.force,
        },
    )
    .map_err(legacy)?;
    let added = completed(
        core_target::execute_add(&ctx, plan, &CliReporter, &CancellationToken::new())
            .map_err(legacy)?,
    )?;

    if let Some(SkuCheck::NotValidated { sku }) = &added.sku {
        println!("{}", sku_not_validated_line(sku));
    }
    let verified_suffix = add_verified_suffix(args.no_ping);
    if added.cli_default.is_some() {
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
/// already supplied, reading through `ctx` (the core's token ping,
/// machine catalogue, region latencies and SSH key candidates).
/// Split out so the main `add` body reads linearly.
fn run_wizard_into_args(ctx: &Context, args: &mut AddArgs) -> CoreResult<()> {
    use crate::commands::target_wizard;
    if args.renew {
        // Renew wizard needs the existing target's provider, so
        // prompt for the name first (if missing) and load the
        // target so we know what provider's validator to wire.
        if args.name.is_none() {
            let n = inquire::Text::new("Target name to rotate credentials for:")
                .with_validator(|v: &str| match core_target::validate_name(v) {
                    Ok(()) => Ok(inquire::validator::Validation::Valid),
                    Err(problem) => Ok(inquire::validator::Validation::Invalid(
                        problem.reason(v).into(),
                    )),
                })
                .prompt()
                .map_err(map_wizard_prompt_error)?;
            args.name = Some(n);
        }
        // Both of the target's files, as before: a missing target is the raw `TargetNotFound`,
        // an unreadable one its file's error.
        let existing = cli_core::load_target(&ctx.store(), args.name.as_deref().unwrap())?;
        let (token, _verified) =
            target_wizard::run_renew_wizard(ctx, &existing.config.provider, args.no_ping)?;
        args.token = Some(token);
    } else {
        let out = target_wizard::run_add_wizard(ctx, args)?;
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

/// `target add --renew` on the core, in today's order: the target (both of its files, as
/// `load_renewable` read them), the config-flag refusal, the token, then `plan_renew` (format,
/// "is it new", SSH key) and `execute_renew` (ping, then the patch under the lock).
fn renew(ctx: &Context, args: AddArgs, name: &str) -> miette::Result<()> {
    let tref = TargetRef::named(ctx, name).map_err(|e| target_legacy::renew(e, ""))?;
    let provider = cli_core::load_target(&ctx.store(), name)
        .map_err(|e| target_legacy::renew(e.into(), ""))?
        .config
        .provider;
    // `--renew` deliberately ignores the config flags; refusing them up front beats silently
    // dropping a value the operator passed.
    reject_config_flags_on_renew(
        args.provider.as_deref(),
        args.region.as_deref(),
        args.tier.as_deref(),
        args.cluster_name.as_deref(),
    )
    .map_err(miette::Report::new)?;
    let token = args.token.ok_or_else(|| {
        miette::Report::new(CliError::Other(format!(
            "`--token` is required for provider `{provider}` (or set `HCLOUD_TOKEN` env var)"
        )))
    })?;
    let legacy = |e| target_legacy::renew(e, &token);
    let plan = core_target::plan_renew(
        ctx,
        &tref,
        core_target::RenewArgs {
            token: SecretString::new(token.clone()),
            ssh_key: args.ssh_key,
        },
    )
    .map_err(legacy)?;
    completed(
        core_target::execute_renew(ctx, plan, &CliReporter, &CancellationToken::new())
            .map_err(legacy)?,
    )?;
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

/// The store lock, or none when there is no store yet: a command that
/// will fail on a missing store must not create one by locking it.
pub(crate) fn store_lock_if_present(
    paths: &TargetStorePaths,
) -> Result<Option<cli_core::StoreLock>> {
    if paths.root().exists() {
        store_lock(paths).map(Some)
    } else {
        Ok(None)
    }
}

/// The store lock for a CLI edit of the store. While another AppRafter
/// process holds it the command waits, and says so once on stderr — a
/// silent wait reads as a hang. Where the store cannot be locked at all
/// (read-only, a filesystem without locks) it warns and the edit goes
/// ahead unlocked.
pub(crate) fn store_lock(paths: &TargetStorePaths) -> Result<cli_core::StoreLock> {
    cli_core::StoreLock::exclusive_or_wait(paths, report_store_lock_event)
}

/// Print what taking the store lock reported, one stderr line per event.
pub(crate) fn report_store_lock_event(event: cli_core::StoreLockEvent<'_>) {
    eprintln!("{}", store_lock_event_line(&event));
}

/// The stderr line for a store-lock event: the core's event as the CLI's reporter prints it,
/// so the commands still on the old code (`apply`, `target firewall`) and the core-backed ones
/// word a wait and a lock-less store the same way.
pub(crate) fn store_lock_event_line(event: &cli_core::StoreLockEvent<'_>) -> String {
    crate::render::reporter::CliReporter::line(&apprafter_core::target::store_lock_event(event))
        .unwrap_or_default()
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
    use apprafter_core::target::{ProvisionedState, TokenPresence};
    use std::path::Path;

    // ── the store lock: what a wait and a lock-less store print ──────────

    #[test]
    fn a_wait_for_the_store_lock_names_the_sentinel() {
        let sentinel = Path::new("/s/.lock");
        let line = store_lock_event_line(&cli_core::StoreLockEvent::Waiting { sentinel });
        assert_eq!(
            line,
            "waiting for another AppRafter process to release the target store (/s/.lock)…"
        );
    }

    #[test]
    fn a_store_that_cannot_be_locked_is_a_warning_naming_the_cause() {
        let sentinel = Path::new("/s/.lock");
        let error = std::io::Error::other("Read-only file system");
        let line = store_lock_event_line(&cli_core::StoreLockEvent::Unlocked {
            sentinel,
            error: &error,
        });
        assert_eq!(
            line,
            "warning: cannot lock the target store (/s/.lock): Read-only file system; \
             continuing without the lock"
        );
    }

    #[test]
    fn the_lock_lines_come_from_the_core_event_and_the_cli_reporter() {
        let sentinel = std::path::Path::new("/s/.lock");
        let waiting = cli_core::StoreLockEvent::Waiting { sentinel };
        assert_eq!(
            store_lock_event_line(&waiting),
            crate::render::reporter::CliReporter::line(&apprafter_core::target::store_lock_event(
                &waiting
            ))
            .unwrap(),
        );
    }

    // ── check_target_name (the wizard's prompt and the legacy texts) ─────

    #[test]
    fn check_target_name_accepts_kebab_lowercase() {
        for n in ["default", "work", "prod-eu", "team-2", "alpha9", "A-B-C"] {
            check_target_name(n).unwrap_or_else(|e| panic!("name `{n}` should be valid: {e}"));
        }
    }

    #[test]
    fn check_target_name_rejects_empty() {
        let msg = check_target_name("").expect_err("empty must error");
        assert!(msg.contains("must not be empty"), "{msg}");
    }

    #[test]
    fn check_target_name_rejects_punctuation() {
        for n in [
            "foo.bar",
            "with space",
            "slash/path",
            "under_score",
            "@home",
        ] {
            assert!(
                check_target_name(n).is_err(),
                "name `{n}` should be rejected"
            );
        }
    }

    #[test]
    fn check_target_name_rejects_leading_or_trailing_dash() {
        assert!(check_target_name("-leading").is_err());
        assert!(check_target_name("trailing-").is_err());
        assert!(check_target_name("--").is_err());
    }

    #[test]
    fn check_target_name_rejects_overlong() {
        let long = "a".repeat(MAX_TARGET_NAME_LEN + 1);
        assert!(check_target_name(&long).is_err());
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

    // ── list / show over the core's reports ──────────────────────────────

    fn report_with(name: &str, active: bool) -> TargetReport {
        TargetReport {
            name: name.into(),
            is_cli_default: active,
            provider: "hetzner-cloud".into(),
            region: Some("nbg1".into()),
            server_type: None,
            default_tier: Some("solo".into()),
            tier_level: Some(1),
            cluster_name: None,
            ssh_key: None,
            token: TokenPresence {
                set: true,
                chars: Some(64),
            },
            config_file: "/s/targets/p/config.yaml".into(),
            credentials_file: "/s/targets/p/credentials.yaml".into(),
            provisioned: ProvisionedState::NotProvisioned,
        }
    }

    /// Today's `target show` layout, line by line. The report carries the token's length, never
    /// its bytes, so the summary cannot echo it.
    #[test]
    fn show_lines_match_todays_layout() {
        let lines = show_lines(&report_with("prod", true));
        assert_eq!(lines[0], "Target: prod (active)");
        assert_eq!(lines[1], "  Provider:    hetzner-cloud");
        assert_eq!(lines[2], "  Region:      nbg1");
        assert_eq!(lines[3], "  Server type: not set");
        assert_eq!(lines[4], "  Default tier: solo");
        assert_eq!(lines[5], "  Cluster name: not set");
        assert_eq!(lines[6], "  SSH key:     not set");
        assert_eq!(
            lines[7],
            "  Hetzner token: set (64 chars; read credentials.yaml for the raw value)"
        );
        assert_eq!(lines[8], "");
        assert_eq!(lines[9], "Config:      /s/targets/p/config.yaml");
        assert_eq!(
            lines.last().unwrap(),
            "Credentials: /s/targets/p/credentials.yaml (mode 0600)"
        );
        assert_eq!(lines.len(), 11);

        let inactive = TargetReport {
            ssh_key: Some(apprafter_core::ssh::SshKeyInfo {
                path: "/h/.ssh/k.pub".into(),
                display: "~/.ssh/k.pub".into(),
                exists: true,
                algo: None,
            }),
            token: TokenPresence {
                set: false,
                chars: None,
            },
            ..report_with("other", false)
        };
        let lines = show_lines(&inactive);
        assert_eq!(lines[0], "Target: other");
        assert_eq!(lines[6], "  SSH key:     /h/.ssh/k.pub");
        assert_eq!(lines[7], "  Hetzner token: not set");
    }

    #[test]
    fn the_list_footer_uses_the_pointer_even_when_it_dangles() {
        assert_eq!(
            list_pointer_name(&CliDefaultPointer::Missing {
                name: "gone".into()
            }),
            "gone"
        );
        assert_eq!(
            list_pointer_name(&CliDefaultPointer::Set {
                name: "prod".into()
            }),
            "prod"
        );
        assert_eq!(list_pointer_name(&CliDefaultPointer::Unset), "");
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

    /// On a fresh store there is nothing to show: the typed no-active-target
    /// error, whose message and help name the ways out — an operator cannot
    /// guess "add one first" from a bare "not found".
    #[test]
    fn show_without_a_name_or_an_active_target_is_the_no_active_target_error() {
        let err = resolve_show_target(None, "").expect_err("nothing to show");
        assert!(matches!(err, CliError::NoActiveTarget), "{err:?}");
        let msg = format!("{err}");
        assert!(msg.contains("apprafter target add"), "{msg}");
        let help = miette::Diagnostic::help(&err)
            .map(|h| h.to_string())
            .unwrap_or_default();
        assert!(help.contains("apprafter target list"), "{help}");
    }

    /// `target remove`'s last line, for each way the CLI default can move.
    #[test]
    fn the_removal_line_says_where_the_cli_default_went() {
        let removed = |cli_default| TargetRemoved {
            name: "prod".into(),
            state_removed: false,
            orphaned_server: None,
            cli_default,
        };
        assert_eq!(remove_done_line(&removed(None)), "target `prod` removed");
        assert_eq!(
            remove_done_line(&removed(Some(ActivePointerChange {
                from: Some("prod".into()),
                to: Some("staging".into()),
            }))),
            "target `prod` removed; active switched to `staging` (alphabetically next)"
        );
        assert_eq!(
            remove_done_line(&removed(Some(ActivePointerChange {
                from: Some("prod".into()),
                to: None,
            }))),
            "target `prod` removed; no targets left, active pointer cleared"
        );
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

/// `target ip` on apprafter-core (D.3a): the active target's server, read by id.
fn run_ip() -> miette::Result<()> {
    // The legacy `<cwd>/.apprafter/state.json` migration stays CLI-only (spec §3.1); it runs
    // before the core reads state, and it answers "no active target" exactly as before.
    resolve_state_paths(None).map_err(miette::Report::new)?;
    let ctx = crate::context::cli_context()?;
    let target = TargetRef::active(&ctx).map_err(report)?;
    let address = apprafter_core::target::public_address(&ctx, &target, &CancellationToken::new())
        .map_err(report)?;
    for line in ip_report_lines(address.ipv4.as_deref(), address.ipv6.as_deref()) {
        println!("{line}");
    }
    Ok(())
}

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

/// `target list` on the core's report. A target whose `config.yaml` cannot be read stays a
/// tracing warning here (R7) — the desktop shows it as a row.
pub(crate) fn list() -> miette::Result<()> {
    info!("target list invoked");
    let ctx = crate::context::cli_context()?;
    let r = core_target::list(&ctx).map_err(report)?;
    for u in &r.unreadable {
        tracing::warn!(target = %u.name, error = %u.error.message, "skipping unreadable target in list");
    }
    if r.targets.is_empty() && r.unreadable.is_empty() {
        println!(
            "No targets configured. Run `apprafter target add` to create one — or `apprafter target add <name>` to skip the wizard's name prompt."
        );
        return Ok(());
    }
    let rows: Vec<TargetListRow> = r
        .targets
        .iter()
        .map(|t| TargetListRow {
            active: if t.is_cli_default {
                "*".into()
            } else {
                String::new()
            },
            name: t.name.clone(),
            provider: t.provider.clone(),
            region: t.region.clone().unwrap_or_else(|| "-".into()),
            tier: t.default_tier.clone().unwrap_or_else(|| "-".into()),
        })
        .collect();
    let mut table = Table::new(&rows);
    table.with(Style::sharp());
    println!("{table}");
    println!();
    println!(
        "{}",
        list_summary_line(rows.len(), list_pointer_name(&r.cli_default))
    );
    Ok(())
}

/// The name the list footer shows as active: the pointer's value even when it dangles (today's
/// footer), empty when there is none.
pub(crate) fn list_pointer_name(p: &CliDefaultPointer) -> &str {
    match p {
        CliDefaultPointer::Unset => "",
        CliDefaultPointer::Set { name } | CliDefaultPointer::Missing { name } => name,
    }
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

/// `target use` on the core: plan, then execute (which re-reads the pointer under the lock).
pub(crate) fn use_target(name: &str) -> miette::Result<()> {
    info!(target = %name, "target use invoked");
    let ctx = crate::context::cli_context()?;
    let tref = TargetRef::named(&ctx, name).map_err(report)?;
    require_loadable(&ctx, name)?;
    let plan = core_target::plan_use(&ctx, &tref).map_err(report)?;
    let used = completed(
        core_target::execute_use(&ctx, plan, &CliReporter, &CancellationToken::new())
            .map_err(report)?,
    )?;
    match used.pointer {
        None => println!("target `{name}` was already the active target"),
        Some(p) => println!(
            "{}",
            switched_active_line(p.from.as_deref().unwrap_or(""), name)
        ),
    }
    Ok(())
}

/// Today's `target use` / `remove` / `machine` found the target with `load_target`, which reads
/// both of its files: a target whose `config.yaml` or `credentials.yaml` cannot be read is
/// refused as before. (The core checks only that the target exists.)
pub(crate) fn require_loadable(ctx: &Context, name: &str) -> miette::Result<()> {
    cli_core::load_target(&ctx.store(), name)
        .map(drop)
        .map_err(miette::Report::new)
}

/// An outcome the CLI's never-tripped token cannot cancel; `Cancelled` still renders as the
/// error, as an `Err(CoreError::Cancelled)` from the same call does.
pub(crate) fn completed<T>(o: Outcome<T>) -> miette::Result<T> {
    match o {
        Outcome::Completed { result } => Ok(result),
        Outcome::Cancelled { .. } => Err(report(CoreError::Cancelled)),
    }
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

/// `target show` on the core's report. The name is resolved here first, for the `info!` line
/// (decision 5): the explicit one, else the CLI default.
pub(crate) fn show(name: Option<&str>) -> miette::Result<()> {
    let ctx = crate::context::cli_context()?;
    let pointer =
        cli_core::resolve_active_target_name(&ctx.store(), None).map_err(miette::Report::new)?;
    let resolved =
        resolve_show_target(name, pointer.as_deref().unwrap_or("")).map_err(miette::Report::new)?;
    info!(target = %resolved, "target show invoked");
    let tref = TargetRef::named(&ctx, &resolved).map_err(report)?;
    for line in show_lines(&core_target::show(&ctx, &tref).map_err(report)?) {
        println!("{line}");
    }
    Ok(())
}

/// The lines `target show` prints. The token is summarised by its length — the report never
/// carries its bytes; read `credentials.yaml` for the raw value.
pub(crate) fn show_lines(r: &TargetReport) -> Vec<String> {
    let or = |v: &Option<String>| v.clone().unwrap_or_else(|| "not set".into());
    vec![
        format!(
            "Target: {}{}",
            r.name,
            if r.is_cli_default { " (active)" } else { "" }
        ),
        format!("  Provider:    {}", r.provider),
        format!("  Region:      {}", or(&r.region)),
        format!("  Server type: {}", or(&r.server_type)),
        format!("  Default tier: {}", or(&r.default_tier)),
        format!("  Cluster name: {}", or(&r.cluster_name)),
        format!(
            "  SSH key:     {}",
            r.ssh_key
                .as_ref()
                .map_or_else(|| "not set".into(), |k| k.path.clone())
        ),
        format!(
            "  Hetzner token: {}",
            match r.token.chars {
                Some(n) if r.token.set => {
                    format!("set ({n} chars; read credentials.yaml for the raw value)")
                }
                _ => "not set".into(),
            }
        ),
        String::new(),
        format!("Config:      {}", r.config_file),
        format!("Credentials: {} (mode 0600)", r.credentials_file),
    ]
}

/// Which target `target show` displays: the explicit name, else the active
/// one. With neither, it is the typed no-active-target error every other
/// command gives (`apprafter::target::no_active`), whose message and help
/// name the ways out — `target add`, `target list`, `target use` — an
/// operator on a fresh store cannot guess from "not found".
pub(crate) fn resolve_show_target(name: Option<&str>, active: &str) -> Result<String> {
    match name {
        Some(n) => Ok(n.to_string()),
        None if active.is_empty() => Err(CliError::NoActiveTarget),
        None => Ok(active.to_string()),
    }
}

/// `target rename` on the core. Its refusals read as today (`target_legacy`).
pub(crate) fn rename(from: &str, to: &str) -> miette::Result<()> {
    info!(from = %from, to = %to, "target rename invoked");
    let ctx = crate::context::cli_context()?;
    let tref = TargetRef::named(&ctx, from).map_err(report)?;
    let plan = core_target::plan_rename(&ctx, &tref, to).map_err(target_legacy::rename)?;
    let done = completed(
        core_target::execute_rename(&ctx, plan, &CliReporter, &CancellationToken::new())
            .map_err(target_legacy::rename)?,
    )?;
    let suffix = if done.cli_default.is_some() {
        " (active pointer updated)"
    } else {
        ""
    };
    println!("target renamed: `{from}` → `{to}`{suffix}");
    Ok(())
}

/// `target remove` on the core. The confirmation stays the CLI's: `--yes`, else a TTY prompt
/// (never a lock held across it — `execute_remove` takes the lock after it).
pub(crate) fn remove(name: &str, yes: bool) -> miette::Result<()> {
    info!(target = %name, yes, "target remove invoked");
    let ctx = crate::context::cli_context()?;
    let tref = TargetRef::named(&ctx, name).map_err(report)?;
    require_loadable(&ctx, name)?;
    let plan = core_target::plan_remove(&ctx, &tref).map_err(report)?;
    if !yes {
        let stdin_tty = std::io::stdin().is_terminal();
        let stdout_tty = std::io::stdout().is_terminal();
        if !(stdin_tty && stdout_tty) {
            return Err(miette::Report::new(CliError::Other(format!(
                "non-interactive invocation: pass `--yes` to confirm removing target `{name}` (refusing silent destruction)"
            ))));
        }
        let confirmed = inquire::Confirm::new(&remove_prompt(name))
            .with_default(false)
            .prompt()
            .map_err(|e| miette::Report::new(map_remove_prompt_error(e)))?;
        if !confirmed {
            println!("{}", remove_aborted_line(name));
            return Ok(());
        }
    }
    let done = completed(
        core_target::execute_remove(&ctx, plan, &CliReporter, &CancellationToken::new())
            .map_err(report)?,
    )?;
    println!("{}", remove_done_line(&done));
    Ok(())
}

/// The line `target remove` ends with: where the CLI default went, when it named the target.
pub(crate) fn remove_done_line(r: &TargetRemoved) -> String {
    match &r.cli_default {
        Some(ActivePointerChange { to: Some(next), .. }) => format!(
            "target `{}` removed; active switched to `{next}` (alphabetically next)",
            r.name
        ),
        Some(ActivePointerChange { to: None, .. }) => format!(
            "target `{}` removed; no targets left, active pointer cleared",
            r.name
        ),
        None => format!("target `{}` removed", r.name),
    }
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
