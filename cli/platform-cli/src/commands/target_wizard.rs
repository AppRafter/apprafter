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
//!  4. SSH public key (Text, default `~/.ssh/id_ed25519.pub`,
//!     skip with empty).
//!  5. Default region (Select, populated by
//!     `validator.list_regions()`).
//!  6. Default tier (Select; copies the kubectl-style one-of list
//!     from the spec).
//!
//! Validators run inline on each prompt so the user gets immediate
//! "✓ Token verified" / "✗ Hetzner Cloud rejected the token"
//! feedback instead of discovering the error after entering five
//! more fields.

use std::net::ToSocketAddrs;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use cli_core::target::validate_hetzner_token_format;
use cli_core::{CliError, Result};
use cli_providers::{HetznerCloudValidator, ProviderValidator, RegionInfo};
use inquire::validator::Validation;
use inquire::{InquireError, Password, PasswordDisplayMode, Select, Text};

use crate::commands::hcloud::hcloud_base_url;
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
    /// by comparing the value to `std::env::var(HCLOUD_TOKEN_ENV)`.
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

const PROVIDER_CHOICES: &[&str] = &["hetzner-cloud"];

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
    /// Set to `true` when the wizard already ran a successful API
    /// ping (default) — caller can skip a second ping in
    /// `ping_provider`. `false` when `--no-ping` flowed through.
    pub token_already_verified: bool,
}

/// Render the wizard prompts. Reads from stdin via `inquire`,
/// writes prompts to stderr (inquire's default), only returns
/// once every required field has a valid value.
pub fn run_add_wizard(initial: &AddArgs) -> Result<WizardOutput> {
    eprintln!();
    eprintln!("Welcome to AppRafter. Let's set up a deployment target.");
    eprintln!();

    let name = prompt_name(initial.name.as_deref(), "positional argument")?;
    let provider = prompt_provider(initial.provider.as_deref(), "--provider flag")?;
    let token_source = classify_token_source(initial.token.as_deref());
    let (token, token_already_verified) = prompt_token(
        &provider,
        initial.token.as_deref(),
        token_source,
        initial.no_ping,
    )?;
    let ssh_key_source = classify_ssh_key_source(initial.ssh_key.as_deref());
    let ssh_key = prompt_ssh_key(initial.ssh_key.as_ref(), ssh_key_source)?;
    let region = prompt_region(
        &provider,
        &token,
        initial.region.as_deref(),
        "--region flag",
        initial.no_ping,
    )?;
    let tier = prompt_tier(initial.tier.as_deref(), "--tier flag")?;

    Ok(WizardOutput {
        name,
        provider,
        token,
        ssh_key,
        region,
        tier,
        token_already_verified,
    })
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

fn classify_ssh_key_source(prefill: Option<&Path>) -> &'static str {
    classify_ssh_key_source_with(
        prefill,
        std::env::var("APPRAFTER_SSH_PUBLIC_KEY_PATH")
            .ok()
            .as_deref(),
    )
}

/// Classify how the token reached us: clap's `#[arg(env)]` blends
/// `--token` flag and `HCLOUD_TOKEN` env into the same `Option`.
/// We probe the env separately and compare to disambiguate, so
/// the wizard can print a friendly "Using HCLOUD_TOKEN from env"
/// notice. Pure on inputs — testable without touching real env.
pub fn classify_token_source_with(prefill: Option<&str>, env_value: Option<&str>) -> TokenSource {
    match (prefill, env_value) {
        (Some(p), Some(e)) if p == e => TokenSource::Env,
        (Some(_), _) => TokenSource::Flag,
        (None, _) => TokenSource::Prompt,
    }
}

fn classify_token_source(prefill: Option<&str>) -> TokenSource {
    classify_token_source_with(prefill, std::env::var(HCLOUD_TOKEN_ENV).ok().as_deref())
}

// ---------------------------------------------------------------
// Renew-flow wizard — only the token gets prompted; everything
// else is preserved from the existing target.
// ---------------------------------------------------------------

pub fn run_renew_wizard(provider: &str, no_ping: bool) -> Result<(String, bool)> {
    eprintln!();
    eprintln!(
        "Rotating credentials. The target's config (provider, region, tier, ...) stays as-is."
    );
    eprintln!();
    // Renew deliberately ignores `HCLOUD_TOKEN` — the env var
    // probably holds the OLD token that's being rotated. Always
    // prompt for the new one.
    prompt_token(provider, None, TokenSource::Prompt, no_ping)
}

// ---------------------------------------------------------------
// Individual prompts
// ---------------------------------------------------------------

fn prompt_name(prefill: Option<&str>, source: &str) -> Result<String> {
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
        .with_validator(|v: &str| match super::target::check_target_name(v) {
            Ok(()) => Ok(Validation::Valid),
            Err(msg) => Ok(Validation::Invalid(msg.into())),
        })
        .prompt()
        .map_err(map_inquire_err)?;
    Ok(answer)
}

fn prompt_provider(prefill: Option<&str>, source: &str) -> Result<String> {
    // Single-entry Select today — kept as a Select so adding a
    // provider in the future doesn't reshape the wizard surface.
    if let Some(p) = prefill {
        // Honour the flag if it was supplied even when the wizard
        // is otherwise prompting for other fields. Skipping the
        // prompt entirely matches the "wizard only fills missing
        // bits" contract.
        if PROVIDER_CHOICES.contains(&p) {
            eprintln!("  ℹ Provider: {p} (from {source})");
            return Ok(p.to_string());
        }
        return Err(CliError::Other(format!(
            "provider `{p}` is not supported (wizard surface: {})",
            PROVIDER_CHOICES.join(", ")
        )));
    }
    let answer = Select::new("Provider:", PROVIDER_CHOICES.to_vec())
        .prompt()
        .map_err(map_inquire_err)?;
    Ok(answer.to_string())
}

fn prompt_token(
    provider: &str,
    prefill: Option<&str>,
    source: TokenSource,
    no_ping: bool,
) -> Result<(String, bool)> {
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
        // surprise mid-wizard prompt.
        if let Err(reason) = validate_for_provider(provider, tok) {
            return Err(CliError::Other(reason));
        }
        let verified = if no_ping {
            false
        } else {
            ping_for_provider(provider, tok).map_err(|e| classify_ping_error(provider, e))?;
            eprintln!("  ✓ Token verified");
            true
        };
        return Ok((tok.to_string(), verified));
    }

    let provider_owned = provider.to_string();
    let validator = move |v: &str| -> std::result::Result<Validation, inquire::CustomUserError> {
        if let Err(reason) = validate_for_provider(&provider_owned, v) {
            return Ok(Validation::Invalid(reason.into()));
        }
        if !no_ping {
            if let Err(e) = ping_for_provider(&provider_owned, v) {
                let summary = inline_ping_error(&e);
                return Ok(Validation::Invalid(summary.into()));
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

fn prompt_ssh_key(prefill: Option<&PathBuf>, source: &str) -> Result<Option<PathBuf>> {
    if let Some(path) = prefill {
        let abbrev = abbreviate_home_path(path);
        eprintln!("  ℹ SSH public key: {abbrev} (from {source})");
        return Ok(Some(path.clone()));
    }

    // Inventory ~/.ssh/*.pub so users with multiple keys (work +
    // personal + per-host) get a real picker rather than a Text
    // input with a blind default. Falls back to the Text path
    // when the directory is empty / unreadable.
    let candidates = scan_ssh_pub_keys();

    if candidates.is_empty() {
        return prompt_ssh_key_text_fallback();
    }

    let mut options: Vec<SshKeyChoice> = candidates
        .into_iter()
        .map(|p| {
            let label = ssh_key_label(&p);
            SshKeyChoice::Path { path: p, label }
        })
        .collect();
    options.push(SshKeyChoice::Other);
    options.push(SshKeyChoice::Skip);

    let selected = Select::new("SSH public key:", options)
        .with_help_message("Used for server provisioning; can be added/changed later.")
        .prompt()
        .map_err(map_inquire_err)?;

    match selected {
        SshKeyChoice::Path { path, .. } => Ok(Some(path)),
        SshKeyChoice::Other => prompt_ssh_key_text_fallback(),
        SshKeyChoice::Skip => Ok(None),
    }
}

fn prompt_ssh_key_text_fallback() -> Result<Option<PathBuf>> {
    let default = default_ssh_key_hint();
    let answer = Text::new("SSH public key path (leave empty to skip):")
        .with_default(&default)
        .with_validator(|v: &str| {
            let trimmed = v.trim();
            if trimmed.is_empty() {
                return Ok(Validation::Valid);
            }
            let expanded = expand_tilde(trimmed);
            if !expanded.exists() {
                return Ok(Validation::Invalid(
                    format!("path `{}` does not exist", expanded.display()).into(),
                ));
            }
            Ok(Validation::Valid)
        })
        .prompt()
        .map_err(map_inquire_err)?;
    let trimmed = answer.trim();
    if trimmed.is_empty() {
        Ok(None)
    } else {
        Ok(Some(expand_tilde(trimmed)))
    }
}

/// Variants of the SSH-key Select picker. `Path` carries the
/// resolved path and the human label so the `Display` impl
/// doesn't have to re-read the file on every redraw.
#[derive(Clone)]
enum SshKeyChoice {
    Path { path: PathBuf, label: String },
    Other,
    Skip,
}

impl std::fmt::Display for SshKeyChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Path { label, .. } => f.write_str(label),
            Self::Other => f.write_str("Other (type a path)"),
            Self::Skip => f.write_str("Skip (don't attach an SSH key now)"),
        }
    }
}

/// Scan `~/.ssh/` for `*.pub` files. Returns paths sorted
/// alphabetically; empty Vec on any IO error (we fall back to a
/// Text input in that case rather than failing the wizard).
pub fn scan_ssh_pub_keys() -> Vec<PathBuf> {
    scan_ssh_pub_keys_in(dirs::home_dir().map(|h| h.join(".ssh")).as_deref())
}

/// Testable scan: caller picks the dir. `None` or a non-existent
/// dir yields an empty Vec.
pub fn scan_ssh_pub_keys_in(dir: Option<&Path>) -> Vec<PathBuf> {
    let Some(dir) = dir else { return Vec::new() };
    if !dir.is_dir() {
        return Vec::new();
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut keys: Vec<PathBuf> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_file())
        .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("pub"))
        .collect();
    keys.sort();
    keys
}

/// Build a Select label for `path`: the path itself plus the
/// public-key comment when the file looks like OpenSSH format.
/// Compact enough to fit on one terminal row even for verbose
/// `~/.ssh/...` paths.
pub fn ssh_key_label(path: &Path) -> String {
    let pretty = abbreviate_home_path(path);
    let Ok(body) = std::fs::read_to_string(path) else {
        return pretty;
    };
    let first_line = body.lines().next().unwrap_or("");
    let parts: Vec<&str> = first_line.split_whitespace().collect();
    // OpenSSH layout: <algo> <base64-key> [comment ...]
    if parts.len() >= 2 {
        let algo = parts[0];
        let comment: String = if parts.len() > 2 {
            parts[2..].join(" ")
        } else {
            String::new()
        };
        if comment.is_empty() {
            return format!("{pretty}  ({algo})");
        }
        return format!("{pretty}  ({algo}, {comment})");
    }
    pretty
}

/// Render `path` with `$HOME` collapsed to `~` so picker rows
/// stay readable. Pure on `home` for testability.
fn abbreviate_home_path_with(path: &Path, home: Option<&Path>) -> String {
    if let Some(home) = home {
        if let Ok(rest) = path.strip_prefix(home) {
            return format!("~/{}", rest.display());
        }
    }
    path.display().to_string()
}

fn abbreviate_home_path(path: &Path) -> String {
    abbreviate_home_path_with(path, dirs::home_dir().as_deref())
}

fn prompt_region(
    provider: &str,
    token: &str,
    prefill: Option<&str>,
    source: &str,
    no_ping: bool,
) -> Result<Option<String>> {
    if let Some(r) = prefill {
        eprintln!("  ℹ Default region: {r} (from {source})");
        return Ok(Some(r.to_string()));
    }
    if no_ping {
        // Can't query the API to populate the picker; fall back
        // to a Text input with the spec's default `nbg1`.
        let answer = Text::new("Default region:")
            .with_default("nbg1")
            .prompt()
            .map_err(map_inquire_err)?;
        let trimmed = answer.trim();
        return Ok(if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        });
    }
    let regions = fetch_regions(provider, token)?;
    if regions.is_empty() {
        return Ok(None);
    }
    eprintln!(
        "  ⏳ Measuring latency to {} region(s)... (best-effort, ≤2s)",
        regions.len()
    );
    let measured = measure_region_latencies(regions, Duration::from_millis(2000));
    let selected = Select::new("Default region (sorted by latency):", measured)
        .prompt()
        .map_err(map_inquire_err)?;
    Ok(Some(selected.info.name))
}

/// `RegionInfo` decorated with a measured TCP latency. `None`
/// means the probe didn't resolve / timed out — those entries
/// sort to the end of the Select so they don't crowd the
/// top-of-list reachable options.
#[derive(Clone)]
pub struct RegionWithLatency {
    pub info: RegionInfo,
    pub latency_ms: Option<u32>,
}

impl std::fmt::Display for RegionWithLatency {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.latency_ms {
            Some(ms) => write!(f, "{:>10}  ({:>4} ms)", self.info, ms),
            None => write!(f, "{:>10}  (  n/a   )", self.info),
        }
    }
}

/// Probe each region in parallel, return sorted ascending by
/// latency (`None` last). Bounded by `overall_timeout` so a
/// network outage doesn't hang the wizard — entries that didn't
/// report in time end up as `None`.
pub fn measure_region_latencies(
    regions: Vec<RegionInfo>,
    overall_timeout: Duration,
) -> Vec<RegionWithLatency> {
    if regions.is_empty() {
        return Vec::new();
    }
    let (tx, rx) = mpsc::channel::<RegionWithLatency>();
    let originals: Vec<RegionInfo> = regions.clone();
    let expected = regions.len();
    for r in regions {
        let tx = tx.clone();
        std::thread::spawn(move || {
            let latency_ms = probe_region_latency(&r.name, overall_timeout);
            // Send may fail if the receiver was dropped (overall
            // timeout hit) — ignore, the receiver has already
            // moved on.
            let _ = tx.send(RegionWithLatency {
                info: r,
                latency_ms,
            });
        });
    }
    drop(tx);

    let deadline = Instant::now() + overall_timeout;
    let mut results: Vec<RegionWithLatency> = Vec::with_capacity(expected);
    while results.len() < expected {
        let now = Instant::now();
        if now >= deadline {
            break;
        }
        let remaining = deadline - now;
        match rx.recv_timeout(remaining) {
            Ok(r) => results.push(r),
            Err(_) => break,
        }
    }
    // Latecomers that finished after the deadline but before we
    // returned still get picked up cheaply.
    while let Ok(r) = rx.try_recv() {
        results.push(r);
    }

    // Synthesize `None`-latency entries for any region that never
    // reported within the overall timeout — the user still sees
    // them in the Select picker (sorted to the end), they just
    // can't compare latency. Without this, an entire run of
    // hung probes would silently shrink the picker to zero
    // entries.
    let reported: std::collections::HashSet<String> =
        results.iter().map(|r| r.info.name.clone()).collect();
    for orig in originals {
        if !reported.contains(&orig.name) {
            results.push(RegionWithLatency {
                info: orig,
                latency_ms: None,
            });
        }
    }

    results.sort_by_key(|r| r.latency_ms.unwrap_or(u32::MAX));
    results
}

/// TCP-connect to the per-Hetzner-DC speedtest endpoint
/// (`<region>-speed.hetzner.com:443`) and return the round-trip
/// of the connect handshake in ms. Returns `None` if DNS fails,
/// connect times out, or any other IO error fires. Pure on the
/// `region` name — exposed as `pub` so tests can call it (they
/// won't actually probe in unit-tests, but the function isn't
/// hidden behind the `prompt_region` glue).
pub fn probe_region_latency(region: &str, timeout: Duration) -> Option<u32> {
    let host = format!("{region}-speed.hetzner.com:443");
    let addrs: Vec<_> = host.to_socket_addrs().ok()?.collect();
    let addr = *addrs.first()?;
    let start = Instant::now();
    std::net::TcpStream::connect_timeout(&addr, timeout).ok()?;
    Some(start.elapsed().as_millis().min(u32::MAX as u128) as u32)
}

fn prompt_tier(prefill: Option<&str>, source: &str) -> Result<Option<String>> {
    if let Some(t) = prefill {
        eprintln!("  ℹ Default tier: {t} (from {source})");
        return Ok(Some(t.to_string()));
    }
    let options: Vec<TierChoice> = TIER_CHOICES
        .iter()
        .map(|(k, label)| TierChoice {
            key: (*k).to_string(),
            label: (*label).to_string(),
        })
        .collect();
    let selected = Select::new("Default tier:", options)
        .prompt()
        .map_err(map_inquire_err)?;
    Ok(Some(selected.key))
}

#[derive(Clone)]
struct TierChoice {
    key: String,
    label: String,
}

impl std::fmt::Display for TierChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} — {}", self.key, self.label)
    }
}

// ---------------------------------------------------------------
// Per-provider routing for validation + ping
// ---------------------------------------------------------------

fn validate_for_provider(provider: &str, token: &str) -> std::result::Result<(), String> {
    match provider {
        "hetzner-cloud" => validate_hetzner_token_format(token),
        other => Err(format!(
            "wizard has no validator wired for provider `{other}`"
        )),
    }
}

fn ping_for_provider(provider: &str, token: &str) -> Result<()> {
    match provider {
        "hetzner-cloud" => {
            let validator = HetznerCloudValidator::new(hcloud_base_url(), token);
            validator.validate_credentials()
        }
        other => Err(CliError::Other(format!(
            "no validator for provider `{other}` — pass `--no-ping` to skip"
        ))),
    }
}

/// Sort a credential-validation error into the right typed variant.
/// 401 → `ProviderTokenRejected` (operator can rotate); everything
/// else → `ProviderApiUnreachable` (operator can `doctor` or wait
/// out the outage). Both wrappers carry the original error as a
/// cause chain so miette renders both layers — top-level summary
/// plus the underlying API envelope.
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

fn fetch_regions(provider: &str, token: &str) -> Result<Vec<RegionInfo>> {
    match provider {
        "hetzner-cloud" => {
            let validator = HetznerCloudValidator::new(hcloud_base_url(), token);
            validator.list_regions()
        }
        other => Err(CliError::Other(format!(
            "no validator for provider `{other}` — region picker unavailable"
        ))),
    }
}

/// One-line error string for inline rendering inside an inquire
/// validator. The full multi-line message from `CliError` would
/// fight the prompt UX, so we collapse it.
fn inline_ping_error(err: &CliError) -> String {
    match err {
        CliError::Hetzner {
            status: 401,
            message,
            ..
        } => format!("Hetzner Cloud rejected the token (HTTP 401): {message}"),
        CliError::Hetzner {
            status, message, ..
        } => format!("Hetzner Cloud API ping failed (HTTP {status}): {message}"),
        CliError::Other(msg) => format!("could not reach the provider: {msg}"),
        other => format!("ping failed: {other}"),
    }
}

// ---------------------------------------------------------------
// Tiny helpers
// ---------------------------------------------------------------

/// Expand a leading `~/` into `$HOME` (best-effort cross-platform).
/// Other tilde forms (`~user/`) are left unexpanded so the path
/// stays predictable — operators who need that can pass an
/// absolute path explicitly.
pub fn expand_tilde(input: &str) -> PathBuf {
    if let Some(rest) = input.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest);
        }
    }
    PathBuf::from(input)
}

fn default_ssh_key_hint() -> String {
    // ~/.ssh/id_ed25519.pub matches the modern OpenSSH default
    // and is what apprafter init / apply scaffolding already
    // expects. If the file doesn't exist, the validator gives the
    // user a clear "path does not exist" prompt without erroring
    // out the wizard — they can paste a different path.
    if let Some(home) = dirs::home_dir() {
        return home
            .join(".ssh/id_ed25519.pub")
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
// Manual walks cover the prompt UX; what we pin here is the pure
// decision logic + helpers.

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn expand_tilde_replaces_leading_tilde_slash_only() {
        let h = dirs::home_dir().expect("home_dir resolves in test env");
        assert_eq!(
            expand_tilde("~/.ssh/id_ed25519.pub"),
            h.join(".ssh/id_ed25519.pub")
        );
        // Without leading `~/` we pass through verbatim.
        assert_eq!(
            expand_tilde("/etc/ssh/host_key.pub"),
            PathBuf::from("/etc/ssh/host_key.pub")
        );
        // `~user/foo` form intentionally NOT expanded.
        assert_eq!(expand_tilde("~bob/key"), PathBuf::from("~bob/key"));
    }

    #[test]
    fn inline_ping_error_summarises_401_separately_from_other_http_errors() {
        let e = CliError::Hetzner {
            endpoint: "GET /v1/locations".into(),
            status: 401,
            code: "unauthorized".into(),
            message: "unable to authenticate".into(),
        };
        let s = inline_ping_error(&e);
        assert!(s.contains("HTTP 401"), "{s}");
        assert!(s.to_lowercase().contains("rejected the token"), "{s}");

        let e = CliError::Hetzner {
            endpoint: "GET /v1/locations".into(),
            status: 503,
            code: "unavailable".into(),
            message: "try later".into(),
        };
        let s = inline_ping_error(&e);
        assert!(s.contains("HTTP 503"), "{s}");
        assert!(s.to_lowercase().contains("api ping failed"), "{s}");
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
        let c = TierChoice {
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
    fn scan_ssh_pub_keys_in_returns_empty_for_missing_or_empty_dirs() {
        assert!(scan_ssh_pub_keys_in(None).is_empty());
        let dir = tempfile::tempdir().unwrap();
        // Non-existent subdir.
        assert!(scan_ssh_pub_keys_in(Some(&dir.path().join("nope"))).is_empty());
        // Empty existing dir.
        assert!(scan_ssh_pub_keys_in(Some(dir.path())).is_empty());
    }

    #[test]
    fn scan_ssh_pub_keys_in_returns_only_pub_files_sorted_alphabetically() {
        let dir = tempfile::tempdir().unwrap();
        // Mix `.pub`, private keys (no extension), other files,
        // and a subdirectory to make sure we don't recurse.
        std::fs::write(dir.path().join("id_ed25519.pub"), "ssh-ed25519 AAA me@x").unwrap();
        std::fs::write(dir.path().join("id_ed25519"), "private not for scan").unwrap();
        std::fs::write(dir.path().join("work.pub"), "ssh-rsa BBB me@work").unwrap();
        std::fs::write(dir.path().join("config"), "Host *\n  User me").unwrap();
        std::fs::create_dir(dir.path().join("subdir")).unwrap();
        std::fs::write(dir.path().join("subdir/nested.pub"), "should not appear").unwrap();

        let keys = scan_ssh_pub_keys_in(Some(dir.path()));
        let names: Vec<String> = keys
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["id_ed25519.pub", "work.pub"]);
    }

    #[test]
    fn ssh_key_label_emits_path_algo_and_comment_when_present() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("id_ed25519.pub");
        std::fs::write(&path, "ssh-ed25519 AAAAfakebody me@laptop\n").unwrap();
        let label = ssh_key_label(&path);
        assert!(
            label.contains("id_ed25519.pub"),
            "label should carry filename: {label}"
        );
        assert!(label.contains("ssh-ed25519"), "{label}");
        assert!(label.contains("me@laptop"), "{label}");
    }

    #[test]
    fn ssh_key_label_falls_back_to_path_when_file_is_unreadable() {
        let label = ssh_key_label(std::path::Path::new("/this/does/not/exist/key.pub"));
        // Path string is preserved verbatim; we just don't crash
        // on the missing file.
        assert!(label.contains("key.pub"), "{label}");
    }

    #[test]
    fn abbreviate_home_path_collapses_home_to_tilde() {
        let home = std::path::Path::new("/home/operator");
        assert_eq!(
            abbreviate_home_path_with(
                std::path::Path::new("/home/operator/.ssh/id_ed25519.pub"),
                Some(home)
            ),
            "~/.ssh/id_ed25519.pub"
        );
        // Path outside $HOME stays absolute.
        assert_eq!(
            abbreviate_home_path_with(std::path::Path::new("/etc/ssh/host_key.pub"), Some(home)),
            "/etc/ssh/host_key.pub"
        );
        // No home → verbatim.
        assert_eq!(
            abbreviate_home_path_with(std::path::Path::new("/foo/bar"), None),
            "/foo/bar"
        );
    }

    #[test]
    fn measure_region_latencies_sorts_unreachable_last_and_preserves_known() {
        // Use a deterministic region list with hostnames that
        // can't resolve (`.invalid` is reserved by RFC 6761 for
        // exactly this — DNS never answers). Probes will all
        // return `None`; the sort just has to put them in stable
        // input order without panicking.
        let regions = vec![
            RegionInfo {
                name: "z-fake.invalid".into(),
                description: "Z".into(),
            },
            RegionInfo {
                name: "a-fake.invalid".into(),
                description: "A".into(),
            },
        ];
        let measured = measure_region_latencies(regions, Duration::from_millis(200));
        // All probes failed → all latency_ms = None; order
        // amongst equals follows whatever the sort does (stable
        // by latency, equal latency → input order preserved by
        // Rust's stable sort).
        assert_eq!(measured.len(), 2);
        for m in &measured {
            assert!(
                m.latency_ms.is_none(),
                "synthetic .invalid hosts shouldn't resolve"
            );
        }
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

    #[test]
    fn region_with_latency_display_marks_unreachable_distinctly() {
        let info = RegionInfo {
            name: "nbg1".into(),
            description: "Nuremberg".into(),
        };
        let reachable = RegionWithLatency {
            info: info.clone(),
            latency_ms: Some(24),
        };
        let dead = RegionWithLatency {
            info,
            latency_ms: None,
        };
        let s_reach = reachable.to_string();
        let s_dead = dead.to_string();
        assert!(s_reach.contains("24 ms"), "{s_reach}");
        assert!(s_dead.contains("n/a"), "{s_dead}");
    }
}
