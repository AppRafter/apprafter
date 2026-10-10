// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Interactive wizard for `apprafter target add` (Track A.4b /
//! v0.1.76). Triggers when stdin + stdout are both TTYs and
//! `--no-interactive` is not set; fills only the fields the user
//! didn't already supply via flags.
//!
//! Per `cli-dx-task.md` §5.1, prompts run in this order:
//!  1. Target name (Text, default `default`).
//!  2. Provider (Select; one entry today — kept as a Select so
//!     adding a provider later is a one-line surface change).
//!  3. Provider token (Password, masked; inline format check +
//!     API ping — unless `--no-ping` was passed).
//!  4. SSH public key (Select over `~/.ssh/*.pub`, or Text with
//!     default `~/.ssh/id_ed25519.pub`; skip with empty).
//!  5. Default tier (Select; copies the kubectl-style one-of list
//!     from the spec).
//!  6. Machine matrix (region × server type, from the provider's
//!     catalogue with each region's latency); under `--no-ping` a
//!     Text region prompt instead.
//!
//! The provider and filesystem reads (token ping, catalogue,
//! latencies, SSH key candidates, the home directory) are
//! apprafter-core's, through the command's `Context`.
//!
//! Validators run inline on each prompt so the user gets immediate
//! "✓ Token verified" / "✗ Hetzner Cloud rejected the token"
//! feedback instead of discovering the error after entering five
//! more fields.

use std::path::{Path, PathBuf};

use apprafter_core::machine::{CatalogueSource, MachineCatalogue, MachineOfferView};
use apprafter_core::ssh::SshKeyCandidate;
use apprafter_core::{CancellationToken, Context, CoreError, CoreResult, SecretString};
use cli_core::target::validate_hetzner_token_format;
use cli_core::CliError;
use inquire::validator::Validation;
use inquire::{InquireError, Password, PasswordDisplayMode, Select, Text};

use crate::commands::machine_picker::{pick_machine, MachineRow};
use crate::commands::target::AddArgs;

/// `HCLOUD_TOKEN` env-var name. Defined here so the wizard's
/// "token-from-env" detection has the same string as clap's
/// `#[arg(env = "HCLOUD_TOKEN")]` annotation on `--token`.
const HCLOUD_TOKEN_ENV: &str = "HCLOUD_TOKEN";

/// Where the supplied token came from — surfaced to the wizard so
/// it can print a one-line acknowledgement when the token rode in
/// on the env var instead of an explicit `--token` flag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenSource {
    /// Passed via `--token` on the command line.
    Flag,
    /// Picked up from the env var bound by clap's
    /// `#[arg(env = "HCLOUD_TOKEN")]`. Distinguishable from `Flag`
    /// by comparing the value to the context's `HCLOUD_TOKEN` override.
    Env,
    /// No prefill at all — wizard prompts.
    Prompt,
}

/// `solo` is the spec-blessed default tier from `spec.md` (Tier 1
/// single VDS) and the cheapest entry. Order in the picker matches
/// price ladder so a `<Up>` arrow lands on the next-tier-up.
const TIER_CHOICES: &[(&str, &str)] = &[
    ("solo", "Tier 1 — €5-20/mo, single VDS"),
    ("team", "Tier 2 — 3+ nodes, HA control plane"),
    ("prod", "Tier 3 — bare-metal Talos, EPYC"),
    ("regulated", "Tier 4 — confidential compute"),
];

const DEFAULT_TARGET_NAME: &str = "default";

/// Decide whether the wizard should fire for this invocation.
///
/// Pure on inputs to make the call testable: don't probe stdin /
/// stdout here, only consume the booleans the caller has already
/// resolved.
///
/// Until v0.1.76 the function also short-circuited "skip when all
/// required flags are present" — v0.1.77 dropped that. The wizard
/// now always runs on a TTY; per-prompt prefill checks make
/// already-supplied fields silent, while optional ones (ssh-key,
/// tier, ...) still get prompted. Explicit `--no-interactive`
/// remains the way to force flag-driven mode.
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

/// Result of the `add`-flow wizard. The wizard fills only fields
/// that weren't already provided via flags; the orchestrator
/// merges this back into `AddArgs` before continuing into save.
pub struct WizardOutput {
    pub name: String,
    pub provider: String,
    pub token: String,
    pub ssh_key: Option<PathBuf>,
    pub region: Option<String>,
    pub tier: Option<String>,
    /// The server-type SKU chosen by the machine matrix. `None`
    /// when `--no-ping` was passed (matrix skipped) and no
    /// `--server-type` flag was given.
    pub server_type: Option<String>,
    /// Set to `true` when the wizard already ran a successful API
    /// ping (default). `target add` pings again at save time anyway
    /// (R2). `false` when `--no-ping` flowed through.
    pub token_already_verified: bool,
}

/// Render the wizard prompts. Reads from stdin via `inquire`,
/// writes prompts to stderr (inquire's default), only returns
/// once every required field has a valid value. The provider reads
/// (token ping, machine catalogue, region latencies, SSH key
/// candidates) are apprafter-core's, through `ctx`.
///
/// Prompt order (v0.2.42+):
///  1. name → 2. provider → 3. token → 4. ssh-key → 5. tier
///     → 6. machine matrix (region × SKU, replaces the old region step)
///
/// With `--force` on a stored target (bug 8) the wizard starts from that target
/// (`ForceBase`): each optional prompt offers "keep the stored value" as its default, so
/// Enter changes nothing, and a target whose state records a server skips the machine step —
/// its region and server type cannot change, the guard `plan_add` applies. A kept field comes
/// back `None`, which `target add` treats as a flag not passed: the core keeps the stored value.
pub fn run_add_wizard(ctx: &Context, initial: &AddArgs) -> CoreResult<WizardOutput> {
    eprintln!();
    eprintln!("Welcome to AppRafter. Let's set up a deployment target.");
    eprintln!();

    let name = prompt_name(initial.name.as_deref(), "positional argument")?;
    let base = ForceBase::load(ctx, &name, initial.force)?;
    if base.is_some() {
        eprintln!(
            "  ℹ Target `{name}` exists: --force keeps each stored value you do not change \
             (Enter keeps it)"
        );
    }
    let provider = prompt_provider(initial.provider.as_deref(), "--provider flag")?;
    let token_source = classify_token_source(ctx, initial.token.as_deref());
    let (token, token_already_verified) = prompt_token(
        ctx,
        &provider,
        initial.token.as_deref(),
        token_source,
        initial.no_ping,
    )?;
    let ssh_key_source = classify_ssh_key_source(initial.ssh_key.as_deref());
    let ssh_key = prompt_ssh_key(
        ctx,
        initial.ssh_key.as_ref(),
        ssh_key_source,
        base.as_ref().map(|b| b.ssh_key_shown(ctx)),
    )?;
    // Tier comes BEFORE the machine matrix so a future tier-aware
    // filter can use the chosen tier to narrow the offer list.
    let tier = prompt_tier(
        initial.tier.as_deref(),
        "--tier flag",
        base.as_ref().map(|b| b.config.default_tier.as_deref()),
    )?;
    let secret = SecretString::new(token.clone());
    let step = force_machine_step(
        base.as_ref(),
        initial.region.as_deref(),
        initial.server_type.as_deref(),
    );
    let (region, server_type) = match (step, &base) {
        (ForceMachineStep::Skip(line), _) => {
            eprintln!("{line}");
            (None, None)
        }
        (ForceMachineStep::OfferKeep, Some(b)) => {
            prompt_machine_keeping(ctx, &provider, &secret, &b.config, initial.no_ping)?
        }
        (ForceMachineStep::Ask, _) | (ForceMachineStep::OfferKeep, None) => prompt_machine(
            ctx,
            &provider,
            &secret,
            initial.region.as_deref(),
            initial.server_type.as_deref(),
            initial.no_ping,
        )?,
    };

    Ok(WizardOutput {
        name,
        provider,
        token,
        ssh_key,
        region,
        tier,
        server_type,
        token_already_verified,
    })
}

/// What `target add <name> --force` overwrites, read before the optional prompts so each can
/// offer to keep its stored value (bug 8: the wizard's defaults used to replace them).
struct ForceBase {
    name: String,
    config: cli_core::TargetConfig,
    server: StoredServer,
}

/// Whether the stored target's state records a server.
enum StoredServer {
    None,
    Recorded(apprafter_core::target::ProvisionedServer),
    /// The state cannot be read (the error, as shown).
    Unreadable(String),
}

impl ForceBase {
    /// The stored target `--force` overwrites. `None` without `--force`, for a name that is
    /// not stored, or for an invalid name (refused after the wizard, as today). A target whose
    /// files cannot be read is that error now: `plan_add` reads both files and would refuse it
    /// with the same error after the whole wizard.
    fn load(ctx: &Context, name: &str, force: bool) -> CoreResult<Option<ForceBase>> {
        if !force || apprafter_core::target::validate_name(name).is_err() {
            return Ok(None);
        }
        let config = match cli_core::load_target(&ctx.store(), name) {
            Ok(t) => t.config,
            Err(CliError::TargetNotFound { .. }) => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        let server = match apprafter_core::TargetRef::named(ctx, name)
            .and_then(|t| apprafter_core::target::provisioned(ctx, &t))
        {
            Ok(None) => StoredServer::None,
            Ok(Some(s)) => StoredServer::Recorded(s),
            Err(e) => StoredServer::Unreadable(e.to_string()),
        };
        Ok(Some(ForceBase {
            name: name.to_string(),
            config,
            server,
        }))
    }

    /// The stored SSH key as the prompts show paths (`~/…`), or `None` when none is stored.
    fn ssh_key_shown(&self, ctx: &Context) -> Option<String> {
        self.config
            .ssh_key_path
            .as_deref()
            .map(|p| cli_core::paths::abbreviate_home(p, ctx.home_dir()))
    }

    /// "region hel1, server type cx32", each "not set" when unset.
    fn machine_shown(&self) -> String {
        stored_machine(&self.config)
    }
}

/// "region hel1, server type cx32", each "not set" when unset.
fn stored_machine(c: &cli_core::TargetConfig) -> String {
    format!(
        "region {}, server type {}",
        c.region.as_deref().unwrap_or("not set"),
        c.server_type.as_deref().unwrap_or("not set")
    )
}

/// The machine step of the add wizard.
#[derive(Debug, PartialEq, Eq)]
enum ForceMachineStep {
    /// Today's step: no stored target, or `--region` / `--server-type` passed.
    Ask,
    /// A provisioned target (or one whose state cannot be read): no prompt; the line says why.
    Skip(String),
    /// A stored target that has not provisioned: keeping its machine is the default.
    OfferKeep,
}

/// Which machine step a `--force` overwrite gets. A target whose state records a server keeps
/// its machine (no in-place resize: the guard `plan_add` and `target machine` apply), so the
/// step is skipped; a flag that moves it is refused by `plan_add` with that guard's error.
fn force_machine_step(
    base: Option<&ForceBase>,
    flag_region: Option<&str>,
    flag_sku: Option<&str>,
) -> ForceMachineStep {
    let Some(b) = base else {
        return ForceMachineStep::Ask;
    };
    match &b.server {
        StoredServer::Recorded(s) => ForceMachineStep::Skip(format!(
            "  ℹ Machine: kept ({}) — target `{}` records server `{}` (id {}), and a \
             provisioned target's region and server type cannot change",
            b.machine_shown(),
            b.name,
            s.server_name,
            s.server_id
        )),
        StoredServer::Unreadable(e) => ForceMachineStep::Skip(format!(
            "  ℹ Machine: kept ({}) — the state of target `{}` cannot be read ({e}), so \
             whether it records a server is unknown",
            b.machine_shown(),
            b.name
        )),
        StoredServer::None if flag_region.is_some() || flag_sku.is_some() => ForceMachineStep::Ask,
        StoredServer::None => ForceMachineStep::OfferKeep,
    }
}

/// The machine step of a `--force` overwrite of a target that has not provisioned, with no
/// `--region` / `--server-type`: keeping the stored machine is the default, and picking another
/// is today's step. Under `--no-ping` the region prompt defaults to the stored region and the
/// stored server type is kept.
fn prompt_machine_keeping(
    ctx: &Context,
    provider: &str,
    token: &SecretString,
    stored: &cli_core::TargetConfig,
    no_ping: bool,
) -> CoreResult<(Option<String>, Option<String>)> {
    if no_ping {
        match &stored.server_type {
            Some(sku) => eprintln!(
                "  machine picker skipped (--no-ping); the stored server type {sku} is kept"
            ),
            None => eprintln!(
                "  machine picker skipped (--no-ping); no server type is stored — pass \
                 --server-type or set one via `apprafter target machine`, or a fresh provision \
                 will fail"
            ),
        }
        return Ok((prompt_region(None, "", stored.region.as_deref())?, None));
    }
    let choices = vec![
        MachineKeepChoice::Keep(stored_machine(stored)),
        MachineKeepChoice::Pick,
    ];
    match Select::new("Machine (region × server type):", choices)
        .prompt()
        .map_err(map_inquire_err)?
    {
        MachineKeepChoice::Keep(_) => Ok((None, None)),
        MachineKeepChoice::Pick => prompt_machine(ctx, provider, token, None, None, false),
    }
}

/// The two rows of [`prompt_machine_keeping`]'s Select, keeping first (the default).
#[derive(Debug, Clone, PartialEq, Eq)]
enum MachineKeepChoice {
    Keep(String),
    Pick,
}

impl std::fmt::Display for MachineKeepChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Keep(m) => write!(f, "Keep the stored machine: {m}"),
            Self::Pick => f.write_str("Pick another machine"),
        }
    }
}

/// Whether the SSH-key prefill came in via the `--ssh-key` flag or
/// the `APPRAFTER_SSH_PUBLIC_KEY_PATH` env var (clap's
/// `#[arg(env)]` blends both into the same `Option<PathBuf>`).
/// Returns the user-facing source label for the prefill
/// announcement. The default `"--ssh-key flag"` is what most
/// people will see; `"APPRAFTER_SSH_PUBLIC_KEY_PATH env var"`
/// fires only when the path on disk matches the env value byte
/// for byte.
pub fn classify_ssh_key_source_with(
    prefill: Option<&Path>,
    env_value: Option<&str>,
) -> &'static str {
    match (prefill, env_value) {
        (Some(p), Some(e)) if p.to_string_lossy() == e => "APPRAFTER_SSH_PUBLIC_KEY_PATH env var",
        _ => "--ssh-key flag",
    }
}

/// The env var is read through the CLI's one counted environment reader (`ProcessEnv`).
fn classify_ssh_key_source(prefill: Option<&Path>) -> &'static str {
    use apprafter_core::EnvSource;
    classify_ssh_key_source_with(
        prefill,
        crate::context::ProcessEnv
            .var("APPRAFTER_SSH_PUBLIC_KEY_PATH")
            .as_deref(),
    )
}

/// Classify how the token reached us: clap's `#[arg(env)]` blends
/// `--token` flag and `HCLOUD_TOKEN` env into the same `Option`.
/// We compare with the env value separately to disambiguate, so
/// the wizard can print a friendly "Using HCLOUD_TOKEN from env"
/// notice. Pure on inputs — testable without touching real env.
pub fn classify_token_source_with(prefill: Option<&str>, env_value: Option<&str>) -> TokenSource {
    match (prefill, env_value) {
        (Some(p), Some(e)) if p == e => TokenSource::Env,
        (Some(_), _) => TokenSource::Flag,
        (None, _) => TokenSource::Prompt,
    }
}

/// The env value is the context's `HCLOUD_TOKEN` override, as `Context::from_cli_env` read it.
fn classify_token_source(ctx: &Context, prefill: Option<&str>) -> TokenSource {
    classify_token_source_with(
        prefill,
        ctx.overrides().hetzner_token.as_ref().map(|t| t.expose()),
    )
}

// ---------------------------------------------------------------
// Renew-flow wizard — only the token gets prompted; everything
// else is preserved from the existing target.
// ---------------------------------------------------------------

pub fn run_renew_wizard(
    ctx: &Context,
    provider: &str,
    no_ping: bool,
) -> CoreResult<(String, bool)> {
    eprintln!();
    eprintln!(
        "Rotating credentials. The target's config (provider, region, tier, ...) stays as-is."
    );
    eprintln!();
    // Renew deliberately ignores `HCLOUD_TOKEN` — the env var
    // probably holds the OLD token that's being rotated. Always
    // prompt for the new one.
    prompt_token(ctx, provider, None, TokenSource::Prompt, no_ping)
}

// ---------------------------------------------------------------
// Individual prompts
// ---------------------------------------------------------------

fn prompt_name(prefill: Option<&str>, source: &str) -> CoreResult<String> {
    if let Some(name) = prefill {
        // Don't re-prompt for a pre-supplied name — the v0.1.76
        // behaviour of asking with `<name>` as the default was
        // mostly noise. Announce + accept, mirroring the silent
        // path the other prefilled fields already take.
        eprintln!("  ℹ Target name: {name} (from {source})");
        return Ok(name.to_string());
    }
    let answer = Text::new("Target name:")
        .with_default(DEFAULT_TARGET_NAME)
        .with_validator(|v: &str| match apprafter_core::target::validate_name(v) {
            Ok(()) => Ok(Validation::Valid),
            Err(problem) => Ok(Validation::Invalid(problem.reason(v).into())),
        })
        .prompt()
        .map_err(map_inquire_err)?;
    Ok(answer)
}

fn prompt_provider(prefill: Option<&str>, source: &str) -> CoreResult<String> {
    // Single-entry Select today — kept as a Select so adding a
    // provider in the future doesn't reshape the wizard surface.
    if let Some(p) = prefill {
        // Honour the flag if it was supplied even when the wizard
        // is otherwise prompting for other fields. Skipping the
        // prompt entirely matches the "wizard only fills missing
        // bits" contract.
        crate::commands::target::check_provider(p)?;
        eprintln!("  ℹ Provider: {p} (from {source})");
        return Ok(p.to_string());
    }
    let answer = Select::new(
        "Provider:",
        apprafter_core::provider::SUPPORTED_PROVIDERS.to_vec(),
    )
    .prompt()
    .map_err(map_inquire_err)?;
    Ok(answer.to_string())
}

fn prompt_token(
    ctx: &Context,
    provider: &str,
    prefill: Option<&str>,
    source: TokenSource,
    no_ping: bool,
) -> CoreResult<(String, bool)> {
    if let Some(tok) = prefill {
        // Surface where the token came from so the user doesn't
        // wonder "where did that token come from?" — especially
        // for the env-var case which is otherwise invisible.
        match source {
            TokenSource::Env => {
                eprintln!(
                    "  ℹ Using token from {HCLOUD_TOKEN_ENV} env var (length {} chars)",
                    tok.len()
                );
            }
            TokenSource::Flag => {
                eprintln!("  ℹ Using token from --token flag");
            }
            TokenSource::Prompt => {
                // Caller shouldn't pass Prompt with Some(prefill);
                // not a hard error but worth a debug breadcrumb.
                tracing::debug!("prompt_token: prefill is Some while source = Prompt");
            }
        }

        // Validate up-front so the user gets the error attached to
        // the flag (`--token` or `HCLOUD_TOKEN` env) rather than a
        // surprise mid-wizard prompt — the flag path's own checks
        // (`plan_add`), so the refusal is the same typed
        // `apprafter::target::invalid_token` with the same text.
        crate::commands::target::check_provider(provider)?;
        apprafter_core::provider::TokenProblem::check(tok)
            .map_err(|problem| CoreError::InvalidToken { problem })?;
        let verified = if no_ping {
            false
        } else {
            // Already classified by the core: 401 → token rejected, else unreachable.
            apprafter_core::provider::ping(
                ctx,
                provider,
                &SecretString::new(tok),
                &CancellationToken::new(),
            )?;
            eprintln!("  ✓ Token verified");
            true
        };
        return Ok((tok.to_string(), verified));
    }

    let provider_owned = provider.to_string();
    // `Password` validators are `'static`: the closure owns a clone of the context.
    let ctx = ctx.clone();
    let validator = move |v: &str| -> std::result::Result<Validation, inquire::CustomUserError> {
        if let Err(reason) = validate_for_provider(&provider_owned, v) {
            return Ok(Validation::Invalid(reason.into()));
        }
        if !no_ping {
            if let Err(e) = apprafter_core::provider::ping(
                &ctx,
                &provider_owned,
                &SecretString::new(v),
                &CancellationToken::new(),
            ) {
                return Ok(Validation::Invalid(inline_ping_error(&e).into()));
            }
        }
        Ok(Validation::Valid)
    };

    let prompt_text = match provider {
        "hetzner-cloud" => "Hetzner Cloud API token:",
        _ => "Provider API token:",
    };
    let answer = Password::new(prompt_text)
        .with_display_mode(PasswordDisplayMode::Masked)
        .with_validator(validator)
        // Display the formatter mask on submit so the line stays
        // tidy in the scrollback; the actual token never appears.
        .without_confirmation()
        .prompt()
        .map_err(map_inquire_err)?;

    if no_ping {
        eprintln!("  ✓ Token format valid (verification skipped — `--no-ping`)");
    } else {
        eprintln!("  ✓ Token verified");
    }
    Ok((answer, !no_ping))
}

/// `keep` is a `--force` overwrite's stored key (`Some(None)`: none is stored): keeping it is
/// the first row and the default, and returns `None` — the flag not passed, so the core keeps
/// the stored value.
fn prompt_ssh_key(
    ctx: &Context,
    prefill: Option<&PathBuf>,
    source: &str,
    keep: Option<Option<String>>,
) -> CoreResult<Option<PathBuf>> {
    if let Some(path) = prefill {
        let abbrev = cli_core::paths::abbreviate_home(path, ctx.home_dir());
        eprintln!("  ℹ SSH public key: {abbrev} (from {source})");
        return Ok(Some(path.clone()));
    }

    // Inventory ~/.ssh/*.pub so users with multiple keys (work +
    // personal + per-host) get a real picker rather than a Text
    // input with a blind default. Falls back to the Text path
    // when the directory is empty / unreadable (and there is no
    // stored key to keep).
    let candidates = apprafter_core::ssh::public_key_candidates(ctx)?;

    if candidates.is_empty() && keep.is_none() {
        return prompt_ssh_key_text_fallback(ctx.home_dir());
    }

    let options = build_ssh_key_choices(candidates, keep);

    let selected = Select::new("SSH public key:", options)
        .with_help_message("Used for server provisioning; can be added/changed later.")
        .prompt()
        .map_err(map_inquire_err)?;

    match selected {
        SshKeyChoice::Path { path, .. } => Ok(Some(path)),
        SshKeyChoice::Other => prompt_ssh_key_text_fallback(ctx.home_dir()),
        SshKeyChoice::Skip | SshKeyChoice::Keep(_) => Ok(None),
    }
}

fn prompt_ssh_key_text_fallback(home: Option<&Path>) -> CoreResult<Option<PathBuf>> {
    let default = default_ssh_key_hint(home);
    let answer = Text::new("SSH public key path (leave empty to skip):")
        .with_default(&default)
        .with_validator(move |v: &str| match validate_ssh_key_path_input(v, home) {
            Ok(()) => Ok(Validation::Valid),
            Err(msg) => Ok(Validation::Invalid(msg.into())),
        })
        .prompt()
        .map_err(map_inquire_err)?;
    Ok(ssh_key_answer_to_path(&answer, home))
}

/// Build the SSH-key picker rows: one `Path` row per found key
/// in the core's order, then the two escape hatches pinned to the
/// bottom (`Other` before `Skip`). With `keep` (a `--force`
/// overwrite's stored key) its `Keep` row comes first — the
/// default — and `Skip` is left out: a forced overwrite cannot
/// clear a stored key, so "don't attach one" would only keep it
/// under another name. Pure — extracted from `prompt_ssh_key` so
/// the row set and the sentinel placement are testable without a
/// terminal.
fn build_ssh_key_choices(
    candidates: Vec<SshKeyCandidate>,
    keep: Option<Option<String>>,
) -> Vec<SshKeyChoice> {
    let forced = keep.is_some();
    let mut options: Vec<SshKeyChoice> = keep.map(SshKeyChoice::Keep).into_iter().collect();
    options.extend(candidates.into_iter().map(|c| SshKeyChoice::Path {
        label: candidate_label(&c),
        path: PathBuf::from(c.path),
    }));
    options.push(SshKeyChoice::Other);
    if !forced {
        options.push(SshKeyChoice::Skip);
    }
    options
}

/// A picker row's label: the `~/` path, plus the key type and its comment when the file reads
/// as an OpenSSH public key (`<algo> <base64> [comment…]`). Compact enough to fit on one
/// terminal row even for verbose `~/.ssh/...` paths.
fn candidate_label(c: &SshKeyCandidate) -> String {
    match (&c.algo, &c.comment) {
        (Some(algo), Some(comment)) => format!("{}  ({algo}, {comment})", c.display),
        (Some(algo), None) => format!("{}  ({algo})", c.display),
        (None, _) => c.display.clone(),
    }
}

/// Accept/reject rule for the free-text SSH-key path: an empty
/// answer means "skip", anything else must be an OpenSSH public key
/// file (after `~/` expansion against `home`) — a private key is
/// refused by name, since the file is what the provider is sent
/// (GOTCHA-149). Pure but for the read — extracted from the `inquire`
/// validator closure in `prompt_ssh_key_text_fallback`.
fn validate_ssh_key_path_input(
    input: &str,
    home: Option<&Path>,
) -> std::result::Result<(), String> {
    use cli_core::ssh_key::{parse_public_key, NotAPublicKey};
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Ok(());
    }
    let expanded = expand_tilde(trimmed, home);
    if !expanded.exists() {
        return Err(format!("path `{}` does not exist", expanded.display()));
    }
    let body = std::fs::read_to_string(&expanded)
        .map_err(|e| format!("cannot read `{}`: {e}", expanded.display()))?;
    match parse_public_key(&body) {
        Ok(_) => Ok(()),
        Err(NotAPublicKey::PrivateKey) => Err(format!(
            "`{}` is a private key: choose its public half, the `.pub` file next to it",
            expanded.display()
        )),
        Err(NotAPublicKey::Other) => Err(format!(
            "`{}` is not an OpenSSH public key (one line: `<type> <base64> [comment]`)",
            expanded.display()
        )),
    }
}

/// Turn an accepted free-text answer into the wizard's
/// `Option<PathBuf>`: empty (or whitespace-only) is "no key at
/// all", not an empty path. Pure — extracted from
/// `prompt_ssh_key_text_fallback`.
fn ssh_key_answer_to_path(answer: &str, home: Option<&Path>) -> Option<PathBuf> {
    let trimmed = answer.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(expand_tilde(trimmed, home))
    }
}

/// Variants of the SSH-key Select picker. `Path` carries the
/// resolved path and the human label so the `Display` impl
/// doesn't have to re-read the file on every redraw.
#[derive(Clone)]
enum SshKeyChoice {
    /// A `--force` overwrite's stored key (as shown), or `None` when none is stored.
    Keep(Option<String>),
    Path {
        path: PathBuf,
        label: String,
    },
    Other,
    Skip,
}

impl std::fmt::Display for SshKeyChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Keep(Some(key)) => write!(f, "Keep the stored key: {key}"),
            Self::Keep(None) => f.write_str("Keep: no SSH key (none is stored)"),
            Self::Path { label, .. } => f.write_str(label),
            Self::Other => f.write_str("Other (type a path)"),
            Self::Skip => f.write_str("Skip (don't attach an SSH key now)"),
        }
    }
}

/// The `--no-ping` region prompt: a Text input with `default` (the spec's `nbg1` for a new
/// target; a `--force` overwrite's stored region, so Enter keeps it — or none, when none is
/// stored, so Enter keeps it unset), since the API cannot be asked for the region list. (It had
/// a network branch, which nothing could reach: its one caller passed `no_ping = true`.)
fn prompt_region(
    prefill: Option<&str>,
    source: &str,
    default: Option<&str>,
) -> CoreResult<Option<String>> {
    if let Some(r) = prefill {
        eprintln!("  ℹ Default region: {r} (from {source})");
        return Ok(Some(r.to_string()));
    }
    let prompt = Text::new("Default region:");
    let prompt = match default {
        Some(d) => prompt.with_default(d),
        None => prompt.with_help_message("leave empty to keep it unset"),
    };
    let answer = prompt.prompt().map_err(map_inquire_err)?;
    Ok(region_text_answer(&answer))
}

/// An empty answer at the `--no-ping` region prompt means "leave
/// the target's region unset" — saving `Some("")` would later be
/// interpolated into API calls as a real region name. Pure —
/// extracted from `prompt_region`.
fn region_text_answer(answer: &str) -> Option<String> {
    let trimmed = answer.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// Return the distinct `location` values from a slice of `MachineOffer`s,
/// in first-occurrence order (no duplicates, input order preserved).
///
/// Pure helper — unit-tested.
pub fn unique_locations(offers: &[cli_providers::machine::MachineOffer]) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for o in offers {
        if seen.insert(o.location.clone()) {
            out.push(o.location.clone());
        }
    }
    out
}

/// Whether a failed catalogue read is worth retrying: a provider request that failed (an API
/// status, or a transport / timeout / parse failure). Anything else — an unsupported provider,
/// a cancelled operation — would fail the same way again.
fn is_request_failure(e: &CoreError) -> bool {
    matches!(
        e,
        CoreError::ProviderRequestFailed { .. } | CoreError::Cli(CliError::Hetzner { .. })
    )
}

/// `e` and its causes on one line, as the retry notice shows them.
fn with_causes(e: &CoreError) -> String {
    let mut line = e.to_string();
    let mut cause = std::error::Error::source(e);
    while let Some(c) = cause {
        line.push_str(&format!(": {c}"));
        cause = c.source();
    }
    line
}

/// Fetch the machine catalog with an interactive retry loop on a failed request.
///
/// If the request fails, prints the error and asks the user whether to
/// retry. Returning `false` from the prompt aborts with the original
/// error (no silent fallback). Any other error returns at once.
fn fetch_catalogue_with_retry(
    fetch: impl Fn() -> CoreResult<MachineCatalogue>,
) -> CoreResult<MachineCatalogue> {
    loop {
        match fetch() {
            Ok(c) => return Ok(c),
            Err(e) if is_request_failure(&e) => {
                eprintln!("  could not fetch the machine catalog: {}", with_causes(&e));
                let retry = inquire::Confirm::new("Retry fetching the machine catalog?")
                    .with_default(true)
                    .prompt()
                    .map_err(map_inquire_err)?;
                if !retry {
                    return Err(e);
                }
            }
            Err(e) => return Err(e),
        }
    }
}

/// Machine-matrix wizard step — replaces the old standalone region picker.
///
/// Returns `(region, server_type)`:
/// - Under `--no-ping`: skips the API entirely, falls back to the text
///   prompt with the `nbg1` default (`prompt_region`), and returns
///   `(that_region, None)` so `server_type` stays unset.
/// - Normal path: fetches the full catalog (`machine::catalogue`, the token
///   given), measures latency to each unique location once
///   (`machine::region_latencies`), builds `MachineRow`s, and delegates to
///   `pick_machine`.
pub fn prompt_machine(
    ctx: &Context,
    provider: &str,
    token: &SecretString,
    prefill_region: Option<&str>,
    prefill_sku: Option<&str>,
    no_ping: bool,
) -> CoreResult<(Option<String>, Option<String>)> {
    // H1: --no-ping shunt — do NOT hit the API.
    if no_ping {
        eprintln!(
            "  machine picker skipped (--no-ping); no server type chosen — \
             pass --server-type or set one via `apprafter target machine`, \
             or a fresh provision will fail"
        );
        let region = prompt_region(
            prefill_region,
            "--region flag",
            Some(apprafter_core::machine::DEFAULT_REGION),
        )?;
        return Ok((region, None));
    }

    // If both axes are already prefilled, announce and return immediately
    // (mirrors the pattern used for name / provider / region prefills).
    if let (Some(r), Some(s)) = (prefill_region, prefill_sku) {
        eprintln!("  ℹ Default region: {r} (from --region flag)");
        eprintln!("  ℹ Server type:    {s} (from --server-type flag)");
        return Ok((Some(r.to_string()), Some(s.to_string())));
    }

    // Fetch the catalog (with retry on a failed request).
    let cancel = CancellationToken::new();
    let catalogue = fetch_catalogue_with_retry(|| {
        apprafter_core::machine::catalogue(ctx, CatalogueSource::Token { provider, token }, &cancel)
    })?;
    let offers: Vec<cli_providers::machine::MachineOffer> = catalogue
        .offers
        .iter()
        .map(MachineOfferView::to_offer)
        .collect();

    // Measure latency to each unique location once.
    let locations = unique_locations(&offers);
    eprintln!(
        "  ⏳ Measuring latency to {} location(s)... (best-effort, ≤2s)",
        locations.len()
    );
    let latency_map: std::collections::HashMap<String, Option<u32>> =
        apprafter_core::machine::region_latencies(ctx, &locations, &cancel)
            .into_iter()
            .map(|r| (r.region, r.latency_ms))
            .collect();

    // Build MachineRow vec: pair each offer with its location's latency.
    let rows: Vec<MachineRow> = offers
        .into_iter()
        .map(|offer| {
            let latency_ms = latency_map.get(&offer.location).copied().flatten();
            MachineRow { offer, latency_ms }
        })
        .collect();

    let (region, sku) = pick_machine(rows, prefill_region, prefill_sku)?;
    Ok((Some(region), Some(sku)))
}

/// `keep` is a `--force` overwrite's stored tier (`Some(None)`: none is stored): keeping it is
/// the first row and the default, and returns `None` — the flag not passed, so the core keeps
/// the stored value.
fn prompt_tier(
    prefill: Option<&str>,
    source: &str,
    keep: Option<Option<&str>>,
) -> CoreResult<Option<String>> {
    if let Some(t) = prefill {
        eprintln!("  ℹ Default tier: {t} (from {source})");
        return Ok(Some(t.to_string()));
    }
    let options = build_tier_choices(keep);
    let selected = Select::new("Default tier:", options)
        .prompt()
        .map_err(map_inquire_err)?;
    Ok(selected.tier())
}

/// Materialise the tier picker rows from `TIER_CHOICES`, after a
/// `Keep` row (the default) when `keep` is a `--force` overwrite's
/// stored tier. Pure — extracted from `prompt_tier` so the offered
/// set and its order (price ladder, cheapest first) are testable
/// without a terminal.
fn build_tier_choices(keep: Option<Option<&str>>) -> Vec<TierChoice> {
    let keep = keep.map(|k| TierChoice::Keep(k.map(str::to_string)));
    keep.into_iter()
        .chain(TIER_CHOICES.iter().map(|(k, label)| TierChoice::Tier {
            key: (*k).to_string(),
            label: (*label).to_string(),
        }))
        .collect()
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum TierChoice {
    /// A `--force` overwrite's stored tier, or `None` when none is stored.
    Keep(Option<String>),
    Tier {
        key: String,
        label: String,
    },
}

impl TierChoice {
    /// What the prompt answers: a tier, or `None` to keep the stored one.
    fn tier(self) -> Option<String> {
        match self {
            TierChoice::Keep(_) => None,
            TierChoice::Tier { key, .. } => Some(key),
        }
    }
}

impl std::fmt::Display for TierChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Keep(Some(t)) => write!(f, "Keep the stored tier: {t}"),
            Self::Keep(None) => f.write_str("Keep: no tier (none is stored)"),
            Self::Tier { key, label } => write!(f, "{key} — {label}"),
        }
    }
}

// ---------------------------------------------------------------
// Per-provider token format check, and the inline ping summary
// ---------------------------------------------------------------

fn validate_for_provider(provider: &str, token: &str) -> std::result::Result<(), String> {
    match provider {
        "hetzner-cloud" => validate_hetzner_token_format(token),
        other => Err(format!(
            "wizard has no validator wired for provider `{other}`"
        )),
    }
}

/// One-line error string for inline rendering inside an inquire
/// validator. The full multi-line message would fight the prompt
/// UX, so we collapse it: today's one-liners over the raw error the
/// core's classified ping error carries.
fn inline_ping_error(e: &CoreError) -> String {
    let raw = match e {
        CoreError::Cli(CliError::ProviderTokenRejected { cause, .. })
        | CoreError::Cli(CliError::ProviderApiUnreachable { cause, .. }) => cause,
        other => return format!("ping failed: {other}"),
    };
    // Upcast `dyn Diagnostic` to `dyn Error` (stable trait upcasting) to downcast the cause.
    let raw_error: &(dyn std::error::Error + 'static) = &**raw;
    match raw_error.downcast_ref::<CliError>() {
        Some(CliError::Hetzner {
            status: 401,
            message,
            ..
        }) => format!("Hetzner Cloud rejected the token (HTTP 401): {message}"),
        Some(CliError::Hetzner {
            status, message, ..
        }) => format!("Hetzner Cloud API ping failed (HTTP {status}): {message}"),
        Some(CliError::Other(msg)) => format!("could not reach the provider: {msg}"),
        _ => format!("ping failed: {raw}"),
    }
}

// ---------------------------------------------------------------
// Tiny helpers
// ---------------------------------------------------------------

/// Expand a leading `~/` into `home` (the context's home directory).
/// Other tilde forms (`~user/`) are left unexpanded so the path
/// stays predictable — operators who need that can pass an
/// absolute path explicitly. No home: the input as typed.
pub fn expand_tilde(input: &str, home: Option<&Path>) -> PathBuf {
    if let Some(rest) = input.strip_prefix("~/") {
        if let Some(home) = home {
            return home.join(rest);
        }
    }
    PathBuf::from(input)
}

fn default_ssh_key_hint(home: Option<&Path>) -> String {
    // ~/.ssh/id_ed25519.pub matches the modern OpenSSH default
    // and is what apprafter init / apply scaffolding already
    // expects. If the file doesn't exist, the validator gives the
    // user a clear "path does not exist" prompt without erroring
    // out the wizard — they can paste a different path.
    // One component per `join`, so the platform's separator sits between each of them.
    if let Some(home) = home {
        return home
            .join(".ssh")
            .join("id_ed25519.pub")
            .to_string_lossy()
            .into_owned();
    }
    "~/.ssh/id_ed25519.pub".to_string()
}

/// Map an `inquire::InquireError` into our own `CliError`. The
/// most common branch is `OperationCanceled` (user pressed Esc /
/// Ctrl-C) — we surface that with a non-panicky friendly message
/// so the user sees "wizard aborted" instead of a backtrace.
fn map_inquire_err(err: InquireError) -> CliError {
    match err {
        InquireError::OperationCanceled | InquireError::OperationInterrupted => {
            CliError::Other("wizard aborted by user".to_string())
        }
        other => CliError::Other(format!("wizard prompt failed: {other}")),
    }
}

// ---------------------------------------------------------------
// Tests for pure helpers
// ---------------------------------------------------------------
//
// inquire prompts read from a real terminal so end-to-end wizard
// tests would need a PTY harness (overkill for the current MVP).
// Manual walks and the scripted `expect` run cover the prompt UX;
// what we pin here is the pure decision logic + helpers.

#[cfg(test)]
mod tests {
    use super::*;
    use apprafter_core::UiError;

    /// A context that reaches no network (`127.0.0.1:1` refuses) and whose home is `home`.
    fn ctx_with_home(home: &Path) -> Context {
        Context::for_desktop("/unused".into(), "http://127.0.0.1:1")
            .with_home_dir(Some(home.into()))
    }

    #[test]
    fn should_use_wizard_fires_on_tty_unless_no_interactive() {
        // --no-interactive wins regardless of TTY state.
        assert!(!should_use_wizard(true, true, true));
        // No TTY on either stream → no wizard.
        assert!(!should_use_wizard(false, false, true));
        assert!(!should_use_wizard(false, true, false));
        assert!(!should_use_wizard(false, false, false));
        // TTY on both streams + no opt-out → wizard fires (even
        // with all required flags present — per-prompt prefill
        // makes the supplied fields silent; optional fields like
        // ssh-key/tier still get prompted; v0.1.76 short-circuit
        // dropped in v0.1.77).
        assert!(should_use_wizard(false, true, true));
    }

    /// `~/` expands against the context's home and nothing else: no home leaves the input as
    /// typed, and `~user/` is never expanded.
    #[test]
    fn tilde_expands_against_the_contexts_home_only() {
        let home = Path::new("/home/op");
        assert_eq!(
            expand_tilde("~/.ssh/k.pub", Some(home)),
            PathBuf::from("/home/op/.ssh/k.pub")
        );
        assert_eq!(
            expand_tilde("~/.ssh/k.pub", None),
            PathBuf::from("~/.ssh/k.pub")
        );
        assert_eq!(expand_tilde("~bob/k", Some(home)), PathBuf::from("~bob/k"));
        assert_eq!(
            expand_tilde("/etc/ssh/host_key.pub", Some(home)),
            PathBuf::from("/etc/ssh/host_key.pub")
        );
    }

    /// The picker row for a found key: the `~/` path, then the key type and its comment when
    /// the file reads as an OpenSSH public key — exactly the row the wizard always showed.
    #[test]
    fn a_candidate_label_is_the_path_then_algo_and_comment() {
        let c = SshKeyCandidate {
            path: "/h/.ssh/w.pub".into(),
            display: "~/.ssh/w.pub".into(),
            algo: Some("ssh-ed25519".into()),
            comment: Some("me@w".into()),
        };
        assert_eq!(candidate_label(&c), "~/.ssh/w.pub  (ssh-ed25519, me@w)");
        assert_eq!(
            candidate_label(&SshKeyCandidate {
                comment: None,
                ..c.clone()
            }),
            "~/.ssh/w.pub  (ssh-ed25519)"
        );
        assert_eq!(
            candidate_label(&SshKeyCandidate {
                algo: None,
                comment: None,
                ..c
            }),
            "~/.ssh/w.pub"
        );
    }

    /// The "from the environment" label compares the prefilled token with the CLI's
    /// `HCLOUD_TOKEN` override as the context read it, never with the process environment.
    #[test]
    fn the_token_source_compares_against_the_cli_override_not_the_process_env() {
        let env = apprafter_core::MapEnv::new()
            .with("APPRAFTER_CONFIG_DIR", "/tmp/x")
            .with("HCLOUD_TOKEN", "t".repeat(64).as_str());
        let ctx = Context::from_cli_env(&env).unwrap();
        assert_eq!(
            classify_token_source(&ctx, Some(&"t".repeat(64))),
            TokenSource::Env
        );
        assert_eq!(
            classify_token_source(&ctx, Some(&"u".repeat(64))),
            TokenSource::Flag
        );
        assert_eq!(classify_token_source(&ctx, None), TokenSource::Prompt);
    }

    /// No catalogue exists for a provider the core does not support: the core's typed
    /// refusal, at once — retrying cannot change it, so no retry prompt.
    #[test]
    fn prompt_machine_refuses_an_unsupported_provider_with_the_typed_error() {
        let dir = tempfile::tempdir().unwrap();
        let token = SecretString::new("a".repeat(64));
        let err = prompt_machine(&ctx_with_home(dir.path()), "aws", &token, None, None, false)
            .unwrap_err();
        assert_eq!(
            UiError::from(&err).code.as_deref(),
            Some("apprafter::target::unknown_provider")
        );
    }

    /// Only a failed provider request is offered a retry; anything else returns at once, and a
    /// catalogue that arrives is returned as is.
    #[test]
    fn only_a_failed_request_is_worth_a_retry() {
        let calls = std::cell::Cell::new(0);
        let err = fetch_catalogue_with_retry(|| {
            calls.set(calls.get() + 1);
            Err(CoreError::Cancelled)
        })
        .unwrap_err();
        assert!(matches!(err, CoreError::Cancelled), "{err:?}");
        assert_eq!(calls.get(), 1);

        let empty = MachineCatalogue {
            regions: Vec::new(),
            offers: Vec::new(),
        };
        let got = fetch_catalogue_with_retry(|| Ok(empty.clone())).unwrap();
        assert_eq!(got, empty);

        assert!(is_request_failure(&CoreError::ProviderRequestFailed {
            provider: "hetzner-cloud".into(),
            endpoint: "GET /v1/locations".into(),
            cause: Box::new(CoreError::Cli(CliError::Other("reset".into()))),
        }));
        assert!(is_request_failure(&CoreError::Cli(CliError::Hetzner {
            endpoint: "GET /v1/server_types".into(),
            status: 503,
            code: "unavailable".into(),
            message: "try later".into(),
        })));
        assert!(!is_request_failure(&CoreError::UnknownProvider {
            provider: "aws".into(),
            supported: vec!["hetzner-cloud".into()],
        }));
    }

    fn rejected(status: u16, message: &str) -> CoreError {
        let raw = CliError::Hetzner {
            endpoint: "GET /v1/locations".into(),
            status,
            code: "x".into(),
            message: message.into(),
        };
        CoreError::Cli(if status == 401 {
            CliError::ProviderTokenRejected {
                provider: "hetzner-cloud".into(),
                cause: Box::new(raw),
            }
        } else {
            CliError::ProviderApiUnreachable {
                provider: "hetzner-cloud".into(),
                cause: Box::new(raw),
            }
        })
    }

    /// The inline line under the token prompt reads the raw API error inside the core's
    /// classified one: a 401 is "rejected", any other status "ping failed (HTTP n)".
    #[test]
    fn inline_ping_error_summarises_401_separately_from_other_http_errors() {
        assert_eq!(
            inline_ping_error(&rejected(401, "unable to authenticate")),
            "Hetzner Cloud rejected the token (HTTP 401): unable to authenticate"
        );
        assert_eq!(
            inline_ping_error(&rejected(503, "try later")),
            "Hetzner Cloud API ping failed (HTTP 503): try later"
        );
    }

    /// A transport failure reads as "could not reach", anything else as a generic one-liner;
    /// both stay on one line — a multi-line message fights the `inquire` prompt redraw.
    #[test]
    fn inline_ping_error_summarises_transport_and_unknown_failures_on_one_line() {
        let transport = inline_ping_error(&CoreError::Cli(CliError::ProviderApiUnreachable {
            provider: "hetzner-cloud".into(),
            cause: Box::new(CliError::Other("connection reset".into())),
        }));
        assert_eq!(transport, "could not reach the provider: connection reset");

        let unknown = inline_ping_error(&CoreError::Cli(CliError::ProviderApiUnreachable {
            provider: "hetzner-cloud".into(),
            cause: Box::new(CliError::TargetNotFound {
                name: "ghost".into(),
                available: "dev".into(),
            }),
        }));
        assert!(unknown.starts_with("ping failed:"), "{unknown}");
        assert!(!unknown.contains('\n'), "{unknown}");

        let unsupported = inline_ping_error(&CoreError::UnknownProvider {
            provider: "aws".into(),
            supported: vec!["hetzner-cloud".into()],
        });
        assert!(unsupported.starts_with("ping failed:"), "{unsupported}");
    }

    #[test]
    fn validate_for_provider_accepts_hetzner_64_char_token_and_rejects_others() {
        let good = "a".repeat(64);
        assert!(validate_for_provider("hetzner-cloud", &good).is_ok());
        assert!(validate_for_provider("hetzner-cloud", "short").is_err());
        let err =
            validate_for_provider("aws", "anything").expect_err("unknown provider must error");
        assert!(err.contains("aws"), "{err}");
    }

    #[test]
    fn tier_choice_display_includes_both_key_and_label() {
        let c = TierChoice::Tier {
            key: "solo".into(),
            label: "Tier 1 — €5".into(),
        };
        let s = c.to_string();
        assert!(s.starts_with("solo —"), "{s}");
        assert!(s.contains("Tier 1"), "{s}");
    }

    #[test]
    fn classify_token_source_distinguishes_env_flag_and_none() {
        // Env-supplied: prefill equals env value.
        assert_eq!(
            classify_token_source_with(Some("abc"), Some("abc")),
            TokenSource::Env
        );
        // Flag-supplied: prefill differs from env (or env unset).
        assert_eq!(
            classify_token_source_with(Some("flag"), Some("env")),
            TokenSource::Flag
        );
        assert_eq!(
            classify_token_source_with(Some("flag"), None),
            TokenSource::Flag
        );
        // None: no prefill at all.
        assert_eq!(
            classify_token_source_with(None, Some("env-ignored")),
            TokenSource::Prompt
        );
        assert_eq!(classify_token_source_with(None, None), TokenSource::Prompt);
    }

    #[test]
    fn classify_ssh_key_source_prefers_env_label_when_path_matches_env_value() {
        let p = PathBuf::from("/home/me/.ssh/id_ed25519.pub");
        // Path matches env-var value byte-for-byte → labelled
        // as the env source so the user can find where it came
        // from.
        assert_eq!(
            classify_ssh_key_source_with(Some(&p), Some("/home/me/.ssh/id_ed25519.pub")),
            "APPRAFTER_SSH_PUBLIC_KEY_PATH env var"
        );
        // Path differs → flag source.
        assert_eq!(
            classify_ssh_key_source_with(Some(&p), Some("/somewhere/else.pub")),
            "--ssh-key flag"
        );
        // No env at all → flag source.
        assert_eq!(
            classify_ssh_key_source_with(Some(&p), None),
            "--ssh-key flag"
        );
        // No prefill — the function isn't called in practice but
        // the fallback is still "--ssh-key flag" (callers gate
        // the call on prefill.is_some() anyway).
        assert_eq!(classify_ssh_key_source_with(None, None), "--ssh-key flag");
    }

    // ---------------------------------------------------------------
    // unique_locations tests (pure helper, task 9)
    // ---------------------------------------------------------------

    fn make_offer(loc: &str, sku: &str) -> cli_providers::machine::MachineOffer {
        cli_providers::machine::MachineOffer {
            location: loc.into(),
            sku: sku.into(),
            cores: 2,
            memory_gb: 4.0,
            disk_gb: 40,
            arch: "x86".into(),
            cpu_type: "shared".into(),
            price_monthly_net: None,
            price_hourly_net: None,
            available: true,
            recommended: false,
            deprecation: None,
        }
    }

    #[test]
    fn unique_locations_deduplicates_preserving_first_occurrence_order() {
        let offers = vec![
            make_offer("hel1", "cx22"),
            make_offer("nbg1", "cx22"),
            make_offer("hel1", "ccx23"), // duplicate location
            make_offer("fsn1", "cx22"),
            make_offer("nbg1", "cx42"), // duplicate location
        ];
        let locs = unique_locations(&offers);
        assert_eq!(locs, vec!["hel1", "nbg1", "fsn1"]);
    }

    #[test]
    fn unique_locations_empty_input_returns_empty() {
        let locs = unique_locations(&[]);
        assert!(locs.is_empty());
    }

    #[test]
    fn unique_locations_single_location_returns_once() {
        let offers = vec![
            make_offer("nbg1", "cx22"),
            make_offer("nbg1", "cx32"),
            make_offer("nbg1", "cx42"),
        ];
        let locs = unique_locations(&offers);
        assert_eq!(locs, vec!["nbg1"]);
    }

    // ---------------------------------------------------------------
    // Prefill (prompt-skipping) decisions.
    //
    // Every prompt in this file starts with a "was it already
    // supplied?" branch. Those branches are reachable without a
    // terminal — a prefilled prompt returns the supplied value and
    // never touches stdin — so they are pinned directly here. Only
    // the branches that actually open an `inquire` widget are left
    // to the manual walk.
    //
    // Nothing below may reach the network: every call either passes
    // `no_ping = true` or takes a prefill short-circuit that
    // returns before the provider client is constructed. A test
    // that starts hitting the Hetzner API is a regression in the
    // production short-circuit, not in the test.
    // ---------------------------------------------------------------

    /// A 64-char ASCII-alphanumeric token — the shape
    /// `validate_hetzner_token_format` accepts. Not a real
    /// credential; nothing in these tests pings the API.
    fn well_formed_token() -> String {
        "a".repeat(64)
    }

    fn add_args_fully_supplied() -> AddArgs {
        AddArgs {
            name: Some("prod".into()),
            provider: Some("hetzner-cloud".into()),
            token: Some(well_formed_token()),
            ssh_key: Some(PathBuf::from("/home/operator/.ssh/id_ed25519.pub")),
            region: Some("hel1".into()),
            tier: Some("team".into()),
            cluster_name: None,
            force: false,
            renew: false,
            no_interactive: false,
            no_ping: true,
            server_type: Some("cx22".into()),
        }
    }

    /// The whole-wizard contract for the "operator supplied
    /// everything on the command line but is still on a TTY" case:
    /// `run_add_wizard` must return each flag's value untouched and
    /// open no prompt at all. This is the path `target add` takes on
    /// every scripted-but-interactive invocation, and it is the one
    /// place where a mis-wired prompt order (e.g. reading the tier
    /// into the region) would be silently accepted, since each
    /// individual prompt still "works".
    #[test]
    fn run_add_wizard_returns_every_supplied_flag_unchanged_and_prompts_for_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let args = add_args_fully_supplied();
        let out = run_add_wizard(&ctx_with_home(dir.path()), &args)
            .expect("a fully prefilled wizard must not prompt");

        assert_eq!(out.name, "prod");
        assert_eq!(out.provider, "hetzner-cloud");
        assert_eq!(out.token, well_formed_token());
        assert_eq!(
            out.ssh_key,
            Some(PathBuf::from("/home/operator/.ssh/id_ed25519.pub"))
        );
        assert_eq!(out.region.as_deref(), Some("hel1"));
        assert_eq!(out.tier.as_deref(), Some("team"));
        // `--no-ping` skips the machine matrix, so the WIZARD
        // contributes no SKU even though `--server-type` was
        // passed. That is deliberate: `run_wizard_into_args` only
        // adopts `out.server_type` when the flag was absent, so the
        // operator's `cx22` survives. Returning the prefill here
        // instead would make the wizard look like it had picked a
        // SKU it never validated.
        assert_eq!(out.server_type, None);
        // No ping ran, so the save-time check must not be told the
        // token is already verified.
        assert!(!out.token_already_verified);
    }

    /// A store holding target `prod` as an operator would have it after a provision: tier team,
    /// region hel1, server type cx32, an SSH key, a Cloudflare firewall — and, when `server` is
    /// set, a state that records server 42.
    fn stored_prod(server: bool) -> (tempfile::TempDir, Context, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let ctx = Context::for_desktop(dir.path().join("store"), "http://127.0.0.1:1")
            .with_home_dir(Some(dir.path().join("home")));
        let key = dir.path().join("home").join(".ssh").join("prod.pub");
        std::fs::create_dir_all(key.parent().unwrap()).unwrap();
        std::fs::write(&key, "ssh-ed25519 AAAA op@prod\n").unwrap();
        cli_core::save_target(
            &ctx.store(),
            &cli_core::Target {
                name: "prod".into(),
                config: cli_core::TargetConfig {
                    provider: "hetzner-cloud".into(),
                    region: Some("hel1".into()),
                    default_tier: Some("team".into()),
                    cluster_name: None,
                    ssh_key_path: Some(key.clone()),
                    firewall: Some(cli_core::target::FirewallConfig {
                        cloudflare_origin: true,
                    }),
                    server_type: Some("cx32".into()),
                },
                credentials: cli_core::TargetCredentials {
                    hetzner_token: Some("a".repeat(64)),
                },
            },
        )
        .unwrap();
        if server {
            let state = cli_state::StatePaths::for_active_target(&ctx.store(), "prod");
            std::fs::create_dir_all(state.state_dir()).unwrap();
            std::fs::write(
                state.state_file(),
                r#"{"hetzner_cloud":{"server_id":42,"server_name":"prod-node","server_type":"cx32"}}"#,
            )
            .unwrap();
        }
        (dir, ctx, key)
    }

    /// `target add prod --force` with every optional flag left out but `--ssh-key` / `--tier`
    /// (so no Select opens in a test), as a terminal user rotating only the token would run it.
    fn force_args(key: &Path) -> AddArgs {
        AddArgs {
            name: Some("prod".into()),
            provider: Some("hetzner-cloud".into()),
            token: Some("b".repeat(64)),
            ssh_key: Some(key.to_path_buf()),
            region: None,
            tier: Some("team".into()),
            cluster_name: None,
            force: true,
            renew: false,
            no_interactive: false,
            no_ping: true,
            server_type: None,
        }
    }

    /// The `--force` wizard and its merge, through to the core's plan: what the wizard hands
    /// back, merged as `target add` merges it, must plan no change the operator did not make.
    fn plan_of(
        ctx: &Context,
        mut args: AddArgs,
    ) -> apprafter_core::CoreResult<Vec<(String, String)>> {
        let out = run_add_wizard(ctx, &args)?;
        crate::commands::target::merge_wizard_output(&mut args, out);
        let plan = apprafter_core::target::plan_add(
            ctx,
            apprafter_core::target::AddArgs {
                name: args.name.unwrap(),
                provider: args.provider.unwrap(),
                token: SecretString::new(args.token.unwrap()),
                ssh_key: args.ssh_key,
                region: args.region,
                tier: args.tier,
                cluster_name: args.cluster_name,
                server_type: args.server_type,
                force: args.force,
            },
        )?;
        Ok(plan
            .changes
            .iter()
            .filter(|c| c.kind == "Target")
            .map(|c| {
                (
                    format!("{:?}", c.action),
                    c.detail.clone().unwrap_or_default(),
                )
            })
            .collect())
    }

    /// Bug 8 on a terminal: with `--force` each optional Select opens on "keep the stored
    /// value", so Enter changes nothing. The tier row keeps the stored tier (or its absence) and
    /// answers `None` — the flag not passed — and the four tiers follow in price order.
    #[test]
    fn a_forced_tier_prompt_opens_on_keeping_the_stored_tier() {
        let rows = build_tier_choices(Some(Some("team")));
        assert_eq!(rows[0], TierChoice::Keep(Some("team".into())));
        assert_eq!(rows[0].to_string(), "Keep the stored tier: team");
        assert_eq!(rows[0].clone().tier(), None, "keeping is no flag");
        assert_eq!(
            rows[1..]
                .iter()
                .cloned()
                .filter_map(TierChoice::tier)
                .collect::<Vec<_>>(),
            ["solo", "team", "prod", "regulated"]
        );
        let unset = build_tier_choices(Some(None));
        assert_eq!(unset[0].to_string(), "Keep: no tier (none is stored)");
        assert_eq!(unset[0].clone().tier(), None);
        assert!(
            !build_tier_choices(None)
                .iter()
                .any(|c| matches!(c, TierChoice::Keep(_))),
            "a new target has nothing to keep"
        );
    }

    /// The same for the SSH key: keeping the stored key is the first row and answers `None`,
    /// and `Skip` is gone — a forced overwrite cannot clear a stored key, so "don't attach one"
    /// would only have kept it under another name. A new target's rows are unchanged.
    #[test]
    fn a_forced_ssh_key_prompt_opens_on_keeping_the_stored_key() {
        let candidate = SshKeyCandidate {
            path: "/h/.ssh/a.pub".into(),
            display: "~/.ssh/a.pub".into(),
            algo: None,
            comment: None,
        };
        let rows = build_ssh_key_choices(
            vec![candidate.clone()],
            Some(Some("~/.ssh/prod.pub".into())),
        );
        assert_eq!(
            rows.iter().map(|r| r.to_string()).collect::<Vec<_>>(),
            [
                "Keep the stored key: ~/.ssh/prod.pub",
                "~/.ssh/a.pub",
                "Other (type a path)"
            ]
        );
        let none_stored = build_ssh_key_choices(Vec::new(), Some(None));
        assert_eq!(
            none_stored
                .iter()
                .map(|r| r.to_string())
                .collect::<Vec<_>>(),
            ["Keep: no SSH key (none is stored)", "Other (type a path)"]
        );
        let fresh = build_ssh_key_choices(vec![candidate], None);
        assert_eq!(
            fresh.last().unwrap().to_string(),
            "Skip (don't attach an SSH key now)"
        );
    }

    /// The machine step of `--force`: a provisioned target (or one whose state cannot be read)
    /// skips it with a line that says why and what is kept; a target that has not provisioned
    /// offers to keep its machine unless `--region` / `--server-type` was passed; no stored
    /// target is today's step.
    #[test]
    fn the_forced_machine_step_follows_the_provisioned_guard() {
        let (_d, ctx, _) = stored_prod(true);
        let base = ForceBase::load(&ctx, "prod", true).unwrap().unwrap();
        let ForceMachineStep::Skip(line) = force_machine_step(Some(&base), None, None) else {
            panic!("a provisioned target skips the machine step")
        };
        assert_eq!(
            line,
            "  ℹ Machine: kept (region hel1, server type cx32) — target `prod` records server \
             `prod-node` (id 42), and a provisioned target's region and server type cannot change"
        );
        assert!(matches!(
            force_machine_step(Some(&base), Some("fsn1"), None),
            ForceMachineStep::Skip(_)
        ));

        let (_d, ctx, _) = stored_prod(false);
        let base = ForceBase::load(&ctx, "prod", true).unwrap().unwrap();
        assert_eq!(
            force_machine_step(Some(&base), None, None),
            ForceMachineStep::OfferKeep
        );
        assert_eq!(
            force_machine_step(Some(&base), None, Some("cx22")),
            ForceMachineStep::Ask
        );
        assert_eq!(force_machine_step(None, None, None), ForceMachineStep::Ask);
        assert_eq!(
            MachineKeepChoice::Keep(base.machine_shown()).to_string(),
            "Keep the stored machine: region hel1, server type cx32"
        );

        let state = cli_state::StatePaths::for_active_target(&ctx.store(), "prod");
        std::fs::create_dir_all(state.state_dir()).unwrap();
        std::fs::write(state.state_file(), "{").unwrap();
        let base = ForceBase::load(&ctx, "prod", true).unwrap().unwrap();
        let ForceMachineStep::Skip(line) = force_machine_step(Some(&base), None, None) else {
            panic!("an unreadable state skips the machine step")
        };
        assert!(line.contains("cannot be read"), "{line}");
    }

    /// The base is the stored target only under `--force`, for a valid stored name; a target
    /// whose files cannot be read is that error before any prompt (`plan_add` would give it
    /// after the whole wizard).
    #[test]
    fn the_force_base_is_the_stored_target_or_its_read_error() {
        let (_d, ctx, _) = stored_prod(false);
        assert!(ForceBase::load(&ctx, "prod", false).unwrap().is_none());
        assert!(ForceBase::load(&ctx, "fresh", true).unwrap().is_none());
        assert!(ForceBase::load(&ctx, "../prod", true).unwrap().is_none());
        let base = ForceBase::load(&ctx, "prod", true).unwrap().unwrap();
        assert_eq!(base.config.default_tier.as_deref(), Some("team"));
        assert_eq!(
            base.ssh_key_shown(&ctx).as_deref(),
            Some(&*format!(
                "~/{}",
                Path::new(".ssh").join("prod.pub").display()
            ))
        );

        std::fs::write(
            ctx.store().target_credentials_file("prod"),
            "hetzner_token: [unclosed",
        )
        .unwrap();
        let err = ForceBase::load(&ctx, "prod", true)
            .err()
            .expect("unreadable");
        assert_eq!(
            UiError::from(&err).code.as_deref(),
            Some("apprafter::target::invalid_config")
        );
    }

    /// Bug 8 on a terminal: `--force` on a target whose state records a server skips the
    /// machine step (no prompt opens — under `--no-ping` it used to ask for a region with
    /// `nbg1` as the default), so the stored machine is kept, the plan is not refused as a
    /// machine change after the whole wizard, and every field reads as kept.
    #[test]
    fn a_forced_wizard_keeps_the_machine_of_a_provisioned_target() {
        let (_d, ctx, key) = stored_prod(true);
        let changes = plan_of(&ctx, force_args(&key)).expect("no prompt, no refusal");
        assert!(
            changes.iter().all(|(action, _)| action == "Keep"),
            "{changes:?}"
        );
        for kept in ["region: hel1", "tier: team", "server type: cx32"] {
            assert!(
                changes.iter().any(|(_, d)| d == kept),
                "{kept}: {changes:?}"
            );
        }
    }

    /// A pre-supplied name is announced and accepted verbatim —
    /// v0.1.76 re-prompted with the name as the default, which was
    /// pure noise.
    #[test]
    fn prompt_name_accepts_a_supplied_name_verbatim() {
        let got = prompt_name(Some("staging"), "positional argument")
            .expect("prefilled name must not prompt");
        assert_eq!(got, "staging");
    }

    /// The provider prefill is honoured only when it is a provider
    /// the wizard can actually drive. An unknown one has to fail
    /// here, at the flag, rather than later inside a validator that
    /// has no client wired for it — with the core's code and its one
    /// provider list (bug 5: the wizard had a second list and wording).
    #[test]
    fn prompt_provider_takes_the_core_list_and_refuses_with_the_core_code() {
        assert_eq!(
            prompt_provider(Some("hetzner-cloud"), "--provider flag").unwrap(),
            "hetzner-cloud"
        );
        let err =
            prompt_provider(Some("aws"), "--provider flag").expect_err("aws is not supported");
        let ui = apprafter_core::UiError::from(&err);
        assert_eq!(
            ui.code.as_deref(),
            Some("apprafter::target::unknown_provider")
        );
        assert_eq!(ui.fields["provider"], serde_json::json!("aws"));
        assert_eq!(
            ui.fields["supported"],
            serde_json::json!(apprafter_core::provider::SUPPORTED_PROVIDERS)
        );
    }

    /// Under `--no-ping` a well-formed prefilled token is accepted
    /// without a round-trip, and `token_already_verified` stays
    /// false so the save-time check in `target add` still runs. All
    /// three `TokenSource` values take the same accept path — the
    /// source only changes the acknowledgement line.
    #[test]
    fn prompt_token_accepts_a_well_formed_prefill_under_no_ping_without_claiming_verification() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ctx_with_home(dir.path());
        for source in [TokenSource::Env, TokenSource::Flag, TokenSource::Prompt] {
            let (token, verified) = prompt_token(
                &ctx,
                "hetzner-cloud",
                Some(&well_formed_token()),
                source,
                true,
            )
            .expect("well-formed prefill must be accepted under --no-ping");
            assert_eq!(token, well_formed_token(), "source={source:?}");
            assert!(
                !verified,
                "--no-ping ran no API call, so nothing was verified (source={source:?})"
            );
        }
    }

    /// A malformed prefilled token (`--token` or `HCLOUD_TOKEN`) is rejected up front, before
    /// any API call, with the refusal the flag-driven path gives: the core's
    /// `apprafter::target::invalid_token`, the same message (bugs 2: the wizard gave the
    /// catch-all `apprafter::cli::other` with its "file an issue" help).
    #[test]
    fn prompt_token_refuses_a_malformed_prefill_as_the_flag_path_does() {
        let dir = tempfile::tempdir().unwrap();
        for (token, source) in [
            ("too-short", TokenSource::Flag),
            ("too-short", TokenSource::Env),
            (&*format!("{}-", "a".repeat(63)), TokenSource::Flag),
        ] {
            let err = prompt_token(
                &ctx_with_home(dir.path()),
                "hetzner-cloud",
                Some(token),
                source,
                false,
            )
            .expect_err("not a Hetzner token");
            assert_eq!(
                UiError::from(&err).code.as_deref(),
                Some("apprafter::target::invalid_token"),
                "{source:?}: {err:?}"
            );
            let flag_path = CoreError::InvalidToken {
                problem: apprafter_core::provider::TokenProblem::check(token).unwrap_err(),
            };
            assert_eq!(err.to_string(), flag_path.to_string(), "{source:?}");
        }
    }

    /// A supplied SSH key is taken as-is: no `~/.ssh` scan, no
    /// picker, and crucially no existence probe — `target add`
    /// verifies readability later with a better error.
    #[test]
    fn prompt_ssh_key_accepts_a_supplied_path_without_scanning() {
        let dir = tempfile::tempdir().unwrap();
        let p = PathBuf::from("/nowhere/on/this/disk/id_ed25519.pub");
        let got = prompt_ssh_key(&ctx_with_home(dir.path()), Some(&p), "--ssh-key flag", None)
            .expect("prefill must not prompt");
        assert_eq!(got, Some(p));
    }

    /// The found keys come first in their order and the two escape
    /// hatches are pinned below them, `Other` before `Skip`. Order
    /// matters: `Skip` sitting anywhere but last puts "attach no
    /// key" under the cursor's natural resting place.
    #[test]
    fn build_ssh_key_choices_pins_other_then_skip_below_the_scanned_keys() {
        let candidate = |n: &str| SshKeyCandidate {
            path: format!("/h/.ssh/{n}.pub"),
            display: format!("~/.ssh/{n}.pub"),
            algo: Some("ssh-ed25519".into()),
            comment: Some(format!("me@{n}")),
        };
        let opts = build_ssh_key_choices(vec![candidate("a"), candidate("b")], None);
        assert_eq!(opts.len(), 4);
        match &opts[0] {
            SshKeyChoice::Path { path, label } => {
                assert_eq!(path, &PathBuf::from("/h/.ssh/a.pub"));
                assert_eq!(label, "~/.ssh/a.pub  (ssh-ed25519, me@a)");
            }
            other => panic!("first row must be the first found key, got {other}"),
        }
        match &opts[1] {
            SshKeyChoice::Path { path, .. } => assert_eq!(path, &PathBuf::from("/h/.ssh/b.pub")),
            other => panic!("second row must be the second found key, got {other}"),
        }
        assert_eq!(opts[2].to_string(), "Other (type a path)");
        assert_eq!(opts[3].to_string(), "Skip (don't attach an SSH key now)");

        // With nothing found the escape hatches are still the
        // whole list — an empty Select would trap the operator.
        let empty = build_ssh_key_choices(Vec::new(), None);
        assert_eq!(empty.len(), 2);
        assert_eq!(empty[0].to_string(), "Other (type a path)");
        assert_eq!(empty[1].to_string(), "Skip (don't attach an SSH key now)");
    }

    /// The free-text SSH-key path accepts "nothing" (the documented
    /// way to skip) but refuses a path that isn't there — catching
    /// the typo at the prompt rather than at provisioning time. A
    /// `~/` answer is checked under the context's home.
    #[test]
    fn validate_ssh_key_path_input_accepts_blank_or_existing_and_rejects_missing() {
        let dir = tempfile::tempdir().unwrap();
        assert!(validate_ssh_key_path_input("", Some(dir.path())).is_ok());
        assert!(validate_ssh_key_path_input("   ", None).is_ok());

        std::fs::create_dir(dir.path().join(".ssh")).unwrap();
        let key = dir.path().join(".ssh/id_ed25519.pub");
        std::fs::write(&key, "ssh-ed25519 AAAA me@host\n").unwrap();
        assert!(validate_ssh_key_path_input(key.to_str().unwrap(), None).is_ok());
        assert!(validate_ssh_key_path_input("~/.ssh/id_ed25519.pub", Some(dir.path())).is_ok());

        let missing = dir.path().join("absent.pub");
        let err = validate_ssh_key_path_input(missing.to_str().unwrap(), Some(dir.path()))
            .expect_err("a non-existent path must be rejected");
        assert!(err.contains("does not exist"), "{err}");
    }

    /// GOTCHA-149: the typed path is the file the provider is sent, so the prompt refuses a
    /// private key by name (pointing at its `.pub`) and any other file that is not a public
    /// key, and never quotes it.
    #[test]
    fn validate_ssh_key_path_input_refuses_a_private_key_and_a_non_key() {
        let dir = tempfile::tempdir().unwrap();
        let private = dir.path().join("id_ed25519");
        std::fs::write(
            &private,
            "-----BEGIN OPENSSH PRIVATE KEY-----\nb3Blbg==\n-----END OPENSSH PRIVATE KEY-----\n",
        )
        .unwrap();
        let err = validate_ssh_key_path_input(private.to_str().unwrap(), None).unwrap_err();
        assert!(
            err.contains("is a private key") && err.contains(".pub"),
            "{err}"
        );
        assert!(!err.contains("b3Blbg"), "{err}");
        let junk = dir.path().join("notes.pub");
        std::fs::write(&junk, "not-a-key\n").unwrap();
        let err = validate_ssh_key_path_input(junk.to_str().unwrap(), None).unwrap_err();
        assert!(err.contains("is not an OpenSSH public key"), "{err}");
    }

    /// Blank means "no key", not an empty path; a `~/` answer is
    /// expanded (against the context's home) before it reaches the
    /// target store, because nothing downstream re-expands it.
    #[test]
    fn ssh_key_answer_to_path_maps_blank_to_none_and_expands_tilde() {
        let home = Path::new("/home/op");
        assert_eq!(ssh_key_answer_to_path("", Some(home)), None);
        assert_eq!(ssh_key_answer_to_path("   ", Some(home)), None);
        assert_eq!(
            ssh_key_answer_to_path("  ~/.ssh/id_ed25519.pub  ", Some(home)),
            Some(home.join(".ssh/id_ed25519.pub"))
        );
        assert_eq!(
            ssh_key_answer_to_path("/etc/ssh/host_key.pub", Some(home)),
            Some(PathBuf::from("/etc/ssh/host_key.pub"))
        );
    }

    /// Same "blank means unset" rule for the `--no-ping` region
    /// prompt: an empty answer must not be stored as the region
    /// `""`, which would later be interpolated into API URLs.
    #[test]
    fn region_text_answer_maps_blank_to_none_and_trims_the_rest() {
        assert_eq!(region_text_answer(""), None);
        assert_eq!(region_text_answer("  \t "), None);
        assert_eq!(region_text_answer("  hel1 "), Some("hel1".to_string()));
    }

    /// `--region` skips the region prompt.
    #[test]
    fn prompt_region_returns_a_supplied_region_without_prompting() {
        let got = prompt_region(Some("hel1"), "--region flag", Some("nbg1"))
            .expect("prefilled region must not prompt");
        assert_eq!(got, Some("hel1".to_string()));
    }

    /// Under `--no-ping` the machine matrix is skipped entirely: the
    /// region still comes back (from the flag, via the text-entry
    /// fallback) but no SKU is invented, because no catalog was
    /// fetched to validate one against.
    #[test]
    fn prompt_machine_under_no_ping_keeps_the_region_and_leaves_the_sku_unset() {
        let dir = tempfile::tempdir().unwrap();
        let (region, sku) = prompt_machine(
            &ctx_with_home(dir.path()),
            "hetzner-cloud",
            &SecretString::new("unused-token"),
            Some("hel1"),
            Some("cx22"),
            true,
        )
        .expect("--no-ping must not touch the API");
        assert_eq!(region, Some("hel1".to_string()));
        assert_eq!(sku, None, "no catalog was fetched, so no SKU was chosen");
    }

    /// Both axes supplied → return them and skip the catalog fetch.
    /// Without this short-circuit the wizard would spend a round-trip
    /// (and fail offline) building a picker it is about to discard.
    #[test]
    fn prompt_machine_returns_both_prefills_without_fetching_the_catalog() {
        let dir = tempfile::tempdir().unwrap();
        let (region, sku) = prompt_machine(
            &ctx_with_home(dir.path()),
            "hetzner-cloud",
            &SecretString::new("unused-token"),
            Some("hel1"),
            Some("cx22"),
            false,
        )
        .expect("both axes prefilled must short-circuit before the API call");
        assert_eq!(region, Some("hel1".to_string()));
        assert_eq!(sku, Some("cx22".to_string()));
    }

    /// The tier picker offers all four hardware tiers in price
    /// order, cheapest first — `<Up>` from the resting cursor is
    /// meant to land on the next tier up, and `solo` is the
    /// spec-blessed default.
    #[test]
    fn build_tier_choices_offers_all_four_tiers_cheapest_first() {
        let keys: Vec<String> = build_tier_choices(None)
            .into_iter()
            .filter_map(TierChoice::tier)
            .collect();
        assert_eq!(keys, vec!["solo", "team", "prod", "regulated"]);
    }

    /// `--tier` skips the picker.
    #[test]
    fn prompt_tier_accepts_a_supplied_tier_verbatim() {
        let got =
            prompt_tier(Some("regulated"), "--tier flag", None).expect("prefill must not prompt");
        assert_eq!(got, Some("regulated".to_string()));
    }

    /// Esc / Ctrl-C is a user decision, not a crash: it maps to a
    /// plain "aborted" line. Everything else keeps the underlying
    /// inquire error so genuine failures stay diagnosable.
    #[test]
    fn map_inquire_err_reports_user_abort_separately_from_real_prompt_failures() {
        assert_eq!(
            map_inquire_err(InquireError::OperationCanceled).to_string(),
            "wizard aborted by user"
        );
        assert_eq!(
            map_inquire_err(InquireError::OperationInterrupted).to_string(),
            "wizard aborted by user"
        );
        let other = map_inquire_err(InquireError::NotTTY).to_string();
        assert!(other.starts_with("wizard prompt failed:"), "{other}");
    }

    /// The offered default is the modern OpenSSH key name under the
    /// context's home. It is the value most operators will accept
    /// with a single Return, so pointing it at a stale name
    /// (`id_rsa.pub`) would push people onto a weaker key or an
    /// empty prompt. No home: the `~/` form. The path is shown as the platform renders it —
    /// on Windows `\` between every component, never `C:\Users\op\.ssh/id_ed25519.pub`.
    #[test]
    fn default_ssh_key_hint_offers_the_modern_openssh_key_name() {
        let home = Path::new("/home/op");
        assert_eq!(
            default_ssh_key_hint(Some(home)),
            home.join(".ssh")
                .join("id_ed25519.pub")
                .display()
                .to_string()
        );
        assert_eq!(default_ssh_key_hint(None), "~/.ssh/id_ed25519.pub");
    }
}
