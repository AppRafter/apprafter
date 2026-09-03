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
    /// The server-type SKU chosen by the machine matrix. `None`
    /// when `--no-ping` was passed (matrix skipped) and no
    /// `--server-type` flag was given.
    pub server_type: Option<String>,
    /// Set to `true` when the wizard already ran a successful API
    /// ping (default) — caller can skip a second ping in
    /// `ping_provider`. `false` when `--no-ping` flowed through.
    pub token_already_verified: bool,
}

/// Render the wizard prompts. Reads from stdin via `inquire`,
/// writes prompts to stderr (inquire's default), only returns
/// once every required field has a valid value.
///
/// Prompt order (v0.2.42+):
///  1. name → 2. provider → 3. token → 4. ssh-key → 5. tier
///     → 6. machine matrix (region × SKU, replaces the old region step)
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
    // Tier comes BEFORE the machine matrix so a future tier-aware
    // filter can use the chosen tier to narrow the offer list.
    let tier = prompt_tier(initial.tier.as_deref(), "--tier flag")?;
    let (region, server_type) = prompt_machine(
        &provider,
        &token,
        initial.region.as_deref(),
        initial.server_type.as_deref(),
        initial.no_ping,
    )?;

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

    let options = build_ssh_key_choices(candidates);

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
        .with_validator(|v: &str| match validate_ssh_key_path_input(v) {
            Ok(()) => Ok(Validation::Valid),
            Err(msg) => Ok(Validation::Invalid(msg.into())),
        })
        .prompt()
        .map_err(map_inquire_err)?;
    Ok(ssh_key_answer_to_path(&answer))
}

/// Build the SSH-key picker rows: one `Path` row per scanned key
/// in scan order, then the two escape hatches pinned to the
/// bottom (`Other` before `Skip`). Pure — extracted from
/// `prompt_ssh_key` so the row set and the sentinel placement are
/// testable without a terminal.
fn build_ssh_key_choices(candidates: Vec<PathBuf>) -> Vec<SshKeyChoice> {
    let mut options: Vec<SshKeyChoice> = candidates
        .into_iter()
        .map(|p| {
            let label = ssh_key_label(&p);
            SshKeyChoice::Path { path: p, label }
        })
        .collect();
    options.push(SshKeyChoice::Other);
    options.push(SshKeyChoice::Skip);
    options
}

/// Accept/reject rule for the free-text SSH-key path: an empty
/// answer means "skip", anything else must already exist on disk
/// (after `~/` expansion). Pure — extracted from the `inquire`
/// validator closure in `prompt_ssh_key_text_fallback`.
fn validate_ssh_key_path_input(input: &str) -> std::result::Result<(), String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Ok(());
    }
    let expanded = expand_tilde(trimmed);
    if !expanded.exists() {
        return Err(format!("path `{}` does not exist", expanded.display()));
    }
    Ok(())
}

/// Turn an accepted free-text answer into the wizard's
/// `Option<PathBuf>`: empty (or whitespace-only) is "no key at
/// all", not an empty path. Pure — extracted from
/// `prompt_ssh_key_text_fallback`.
fn ssh_key_answer_to_path(answer: &str) -> Option<PathBuf> {
    let trimmed = answer.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(expand_tilde(trimmed))
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
        return Ok(region_text_answer(&answer));
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

    finalize_latency_rows(originals, results)
}

/// Reconcile what the probes reported against the full region
/// list, then sort fastest-first.
///
/// Synthesizes a `None`-latency entry for any region that never
/// reported within the overall timeout — the user still sees it in
/// the Select picker (sorted to the end), they just can't compare
/// latency. Without this, a run where every probe hangs would
/// silently shrink the picker to zero entries and the wizard would
/// offer the operator no region at all.
///
/// Pure — extracted from `measure_region_latencies` so the
/// reconcile + sort rule is testable without spawning probe
/// threads or waiting on a real timeout.
fn finalize_latency_rows(
    originals: Vec<RegionInfo>,
    mut results: Vec<RegionWithLatency>,
) -> Vec<RegionWithLatency> {
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

/// Fetch the machine catalog with an interactive retry loop on failure.
///
/// If the API call fails, prints the error and asks the user whether to
/// retry. Returning `false` from the prompt aborts with the original
/// error (no silent fallback).
fn fetch_offers_with_retry(
    validator: &HetznerCloudValidator,
) -> Result<Vec<cli_providers::machine::MachineOffer>> {
    loop {
        match validator.list_machine_offers() {
            Ok(o) => return Ok(o),
            Err(e) => {
                eprintln!("  could not fetch the machine catalog: {e}");
                let retry = inquire::Confirm::new("Retry fetching the machine catalog?")
                    .with_default(true)
                    .prompt()
                    .map_err(map_inquire_err)?;
                if !retry {
                    return Err(e);
                }
            }
        }
    }
}

/// Machine-matrix wizard step — replaces the old standalone region picker.
///
/// Returns `(region, server_type)`:
/// - Under `--no-ping`: skips the API entirely, falls back to the text
///   prompt with the `nbg1` default (reuses `prompt_region`), and returns
///   `(that_region, None)` so `server_type` stays unset.
/// - Normal path: fetches the full catalog, measures latency to each unique
///   location once, builds `MachineRow`s, and delegates to `pick_machine`.
pub fn prompt_machine(
    provider: &str,
    token: &str,
    prefill_region: Option<&str>,
    prefill_sku: Option<&str>,
    no_ping: bool,
) -> Result<(Option<String>, Option<String>)> {
    // H1: --no-ping shunt — do NOT hit the API.
    if no_ping {
        eprintln!(
            "  machine picker skipped (--no-ping); no server type chosen — \
             pass --server-type or set one via `apprafter target machine`, \
             or a fresh provision will fail"
        );
        let region = prompt_region(provider, token, prefill_region, "--region flag", true)?;
        return Ok((region, None));
    }

    // If both axes are already prefilled, announce and return immediately
    // (mirrors the pattern used for name / provider / region prefills).
    if let (Some(r), Some(s)) = (prefill_region, prefill_sku) {
        eprintln!("  ℹ Default region: {r} (from --region flag)");
        eprintln!("  ℹ Server type:    {s} (from --server-type flag)");
        return Ok((Some(r.to_string()), Some(s.to_string())));
    }

    // Build the validator and fetch the catalog (with retry on failure).
    let validator = match provider {
        "hetzner-cloud" => HetznerCloudValidator::new(hcloud_base_url(), token),
        other => {
            return Err(CliError::Other(format!(
                "no machine catalog for provider `{other}` — pass `--no-ping` to fall back to text entry"
            )));
        }
    };
    let offers = fetch_offers_with_retry(&validator)?;

    // Measure latency to each unique location once.
    let locations = unique_locations(&offers);
    eprintln!(
        "  ⏳ Measuring latency to {} location(s)... (best-effort, ≤2s)",
        locations.len()
    );
    let region_infos: Vec<RegionInfo> = locations
        .iter()
        .map(|name| RegionInfo {
            name: name.clone(),
            description: name.clone(),
        })
        .collect();
    let measured = measure_region_latencies(region_infos, Duration::from_millis(2000));
    let latency_map: std::collections::HashMap<String, Option<u32>> = measured
        .into_iter()
        .map(|r| (r.info.name.clone(), r.latency_ms))
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

fn prompt_tier(prefill: Option<&str>, source: &str) -> Result<Option<String>> {
    if let Some(t) = prefill {
        eprintln!("  ℹ Default tier: {t} (from {source})");
        return Ok(Some(t.to_string()));
    }
    let options = build_tier_choices();
    let selected = Select::new("Default tier:", options)
        .prompt()
        .map_err(map_inquire_err)?;
    Ok(Some(selected.key))
}

/// Materialise the tier picker rows from `TIER_CHOICES`. Pure —
/// extracted from `prompt_tier` so the offered set and its order
/// (price ladder, cheapest first) are testable without a terminal.
fn build_tier_choices() -> Vec<TierChoice> {
    TIER_CHOICES
        .iter()
        .map(|(k, label)| TierChoice {
            key: (*k).to_string(),
            label: (*label).to_string(),
        })
        .collect()
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
        let args = add_args_fully_supplied();
        let out = run_add_wizard(&args).expect("a fully prefilled wizard must not prompt");

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
    /// has no client wired for it.
    #[test]
    fn prompt_provider_accepts_a_supported_prefill_and_rejects_an_unknown_one() {
        let got = prompt_provider(Some("hetzner-cloud"), "--provider flag")
            .expect("supported provider must be accepted");
        assert_eq!(got, "hetzner-cloud");

        let err = prompt_provider(Some("aws"), "--provider flag")
            .expect_err("unsupported provider must be rejected");
        let msg = err.to_string();
        assert!(msg.contains("aws"), "{msg}");
        // The error has to name what IS supported, otherwise the
        // operator is left guessing.
        assert!(msg.contains("hetzner-cloud"), "{msg}");
    }

    /// Under `--no-ping` a well-formed prefilled token is accepted
    /// without a round-trip, and `token_already_verified` stays
    /// false so the save-time check in `run_add` still runs. All
    /// three `TokenSource` values take the same accept path — the
    /// source only changes the acknowledgement line.
    #[test]
    fn prompt_token_accepts_a_well_formed_prefill_under_no_ping_without_claiming_verification() {
        for source in [TokenSource::Env, TokenSource::Flag, TokenSource::Prompt] {
            let (token, verified) =
                prompt_token("hetzner-cloud", Some(&well_formed_token()), source, true)
                    .expect("well-formed prefill must be accepted under --no-ping");
            assert_eq!(token, well_formed_token(), "source={source:?}");
            assert!(
                !verified,
                "--no-ping ran no API call, so nothing was verified (source={source:?})"
            );
        }
    }

    /// A malformed prefilled token is rejected up front, attached
    /// to the flag that carried it, instead of surviving into the
    /// target store or surfacing as a confusing mid-wizard prompt.
    #[test]
    fn prompt_token_rejects_a_malformed_prefill_before_any_api_call() {
        let err = prompt_token("hetzner-cloud", Some("too-short"), TokenSource::Flag, true)
            .expect_err("a 9-char token is not a Hetzner token");
        let msg = err.to_string();
        assert!(msg.contains("64"), "{msg}");
    }

    /// A supplied SSH key is taken as-is: no `~/.ssh` scan, no
    /// picker, and crucially no existence probe — `run_add`
    /// verifies readability later with a better error.
    #[test]
    fn prompt_ssh_key_accepts_a_supplied_path_without_scanning() {
        let p = PathBuf::from("/nowhere/on/this/disk/id_ed25519.pub");
        let got = prompt_ssh_key(Some(&p), "--ssh-key flag").expect("prefill must not prompt");
        assert_eq!(got, Some(p));
    }

    /// The scanned keys come first in scan order and the two escape
    /// hatches are pinned below them, `Other` before `Skip`. Order
    /// matters: `Skip` sitting anywhere but last puts "attach no
    /// key" under the cursor's natural resting place.
    #[test]
    fn build_ssh_key_choices_pins_other_then_skip_below_the_scanned_keys() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.pub");
        let b = dir.path().join("b.pub");
        std::fs::write(&a, "ssh-ed25519 AAAA me@a\n").unwrap();
        std::fs::write(&b, "ssh-rsa BBBB me@b\n").unwrap();

        let opts = build_ssh_key_choices(vec![a.clone(), b.clone()]);
        assert_eq!(opts.len(), 4);
        match &opts[0] {
            SshKeyChoice::Path { path, .. } => assert_eq!(path, &a),
            other => panic!("first row must be the first scanned key, got {other}"),
        }
        match &opts[1] {
            SshKeyChoice::Path { path, .. } => assert_eq!(path, &b),
            other => panic!("second row must be the second scanned key, got {other}"),
        }
        assert_eq!(opts[2].to_string(), "Other (type a path)");
        assert_eq!(opts[3].to_string(), "Skip (don't attach an SSH key now)");

        // With nothing scanned the escape hatches are still the
        // whole list — an empty Select would trap the operator.
        let empty = build_ssh_key_choices(Vec::new());
        assert_eq!(empty.len(), 2);
        assert_eq!(empty[0].to_string(), "Other (type a path)");
        assert_eq!(empty[1].to_string(), "Skip (don't attach an SSH key now)");
    }

    /// The free-text SSH-key path accepts "nothing" (the documented
    /// way to skip) but refuses a path that isn't there — catching
    /// the typo at the prompt rather than at provisioning time.
    #[test]
    fn validate_ssh_key_path_input_accepts_blank_or_existing_and_rejects_missing() {
        assert!(validate_ssh_key_path_input("").is_ok());
        assert!(validate_ssh_key_path_input("   ").is_ok());

        let dir = tempfile::tempdir().unwrap();
        let key = dir.path().join("id_ed25519.pub");
        std::fs::write(&key, "ssh-ed25519 AAAA me@host\n").unwrap();
        assert!(validate_ssh_key_path_input(key.to_str().unwrap()).is_ok());

        let missing = dir.path().join("absent.pub");
        let err = validate_ssh_key_path_input(missing.to_str().unwrap())
            .expect_err("a non-existent path must be rejected");
        assert!(err.contains("does not exist"), "{err}");
    }

    /// Blank means "no key", not an empty path; a `~/` answer is
    /// expanded before it reaches the target store, because nothing
    /// downstream re-expands it.
    #[test]
    fn ssh_key_answer_to_path_maps_blank_to_none_and_expands_tilde() {
        assert_eq!(ssh_key_answer_to_path(""), None);
        assert_eq!(ssh_key_answer_to_path("   "), None);

        let home = dirs::home_dir().expect("home_dir resolves in test env");
        assert_eq!(
            ssh_key_answer_to_path("  ~/.ssh/id_ed25519.pub  "),
            Some(home.join(".ssh/id_ed25519.pub"))
        );
        assert_eq!(
            ssh_key_answer_to_path("/etc/ssh/host_key.pub"),
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

    /// `--region` wins over the latency picker. This one is called
    /// with `no_ping = false` on purpose: the prefill branch has to
    /// return *before* the region list is fetched, so a supplied
    /// region works offline too.
    #[test]
    fn prompt_region_returns_a_supplied_region_without_fetching_the_region_list() {
        let got = prompt_region(
            "hetzner-cloud",
            "unused-token",
            Some("hel1"),
            "--region flag",
            false,
        )
        .expect("prefilled region must short-circuit before the API call");
        assert_eq!(got, Some("hel1".to_string()));
    }

    /// Under `--no-ping` the machine matrix is skipped entirely: the
    /// region still comes back (from the flag, via the text-entry
    /// fallback) but no SKU is invented, because no catalog was
    /// fetched to validate one against.
    #[test]
    fn prompt_machine_under_no_ping_keeps_the_region_and_leaves_the_sku_unset() {
        let (region, sku) = prompt_machine(
            "hetzner-cloud",
            "unused-token",
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
        let (region, sku) = prompt_machine(
            "hetzner-cloud",
            "unused-token",
            Some("hel1"),
            Some("cx22"),
            false,
        )
        .expect("both axes prefilled must short-circuit before the API call");
        assert_eq!(region, Some("hel1".to_string()));
        assert_eq!(sku, Some("cx22".to_string()));
    }

    /// There is no machine catalog for a provider we haven't wired,
    /// and the error has to hand the operator the escape hatch
    /// (`--no-ping` → text entry) rather than just saying "no".
    #[test]
    fn prompt_machine_refuses_an_unwired_provider_and_points_at_the_escape_hatch() {
        let err = prompt_machine("aws", "unused-token", None, None, false)
            .expect_err("no catalog exists for aws");
        let msg = err.to_string();
        assert!(msg.contains("aws"), "{msg}");
        assert!(msg.contains("--no-ping"), "{msg}");
    }

    /// The tier picker offers all four hardware tiers in price
    /// order, cheapest first — `<Up>` from the resting cursor is
    /// meant to land on the next tier up, and `solo` is the
    /// spec-blessed default.
    #[test]
    fn build_tier_choices_offers_all_four_tiers_cheapest_first() {
        let keys: Vec<String> = build_tier_choices().into_iter().map(|c| c.key).collect();
        assert_eq!(keys, vec!["solo", "team", "prod", "regulated"]);
    }

    /// `--tier` skips the picker.
    #[test]
    fn prompt_tier_accepts_a_supplied_tier_verbatim() {
        let got = prompt_tier(Some("regulated"), "--tier flag").expect("prefill must not prompt");
        assert_eq!(got, Some("regulated".to_string()));
    }

    // ---------------------------------------------------------------
    // Error classification + remaining pure helpers.
    // ---------------------------------------------------------------

    /// A 401 is the operator's problem (rotate the token); anything
    /// else is the API's (retry / `doctor`). The two land on
    /// different typed variants because they carry different help
    /// text — collapsing them would send someone rotating a
    /// perfectly good token during a Hetzner outage.
    #[test]
    fn classify_ping_error_splits_401_from_every_other_failure() {
        let unauthorized = CliError::Hetzner {
            endpoint: "GET /v1/locations".into(),
            status: 401,
            code: "unauthorized".into(),
            message: "unable to authenticate".into(),
        };
        match classify_ping_error("hetzner-cloud", unauthorized) {
            CliError::ProviderTokenRejected { provider, .. } => {
                assert_eq!(provider, "hetzner-cloud")
            }
            other => panic!("401 must classify as ProviderTokenRejected, got {other:?}"),
        }

        let outage = CliError::Hetzner {
            endpoint: "GET /v1/locations".into(),
            status: 503,
            code: "unavailable".into(),
            message: "try later".into(),
        };
        match classify_ping_error("hetzner-cloud", outage) {
            CliError::ProviderApiUnreachable { provider, .. } => {
                assert_eq!(provider, "hetzner-cloud")
            }
            other => panic!("503 must classify as ProviderApiUnreachable, got {other:?}"),
        }

        // A non-HTTP failure (DNS, TLS, ...) is also "unreachable",
        // never "token rejected".
        match classify_ping_error("hetzner-cloud", CliError::Other("dns failure".into())) {
            CliError::ProviderApiUnreachable { .. } => {}
            other => panic!("a transport error must classify as unreachable, got {other:?}"),
        }
    }

    /// The non-Hetzner arms of the inline summary: a transport
    /// error reads as "could not reach", anything else falls back
    /// to a generic one-liner. Both must stay single-line — a
    /// multi-line message fights the `inquire` prompt redraw.
    #[test]
    fn inline_ping_error_summarises_transport_and_unknown_failures_on_one_line() {
        let transport = inline_ping_error(&CliError::Other("connection reset".into()));
        assert!(
            transport.starts_with("could not reach the provider:"),
            "{transport}"
        );
        assert!(!transport.contains('\n'), "{transport}");

        let unknown = inline_ping_error(&CliError::TargetNotFound {
            name: "ghost".into(),
            available: "dev".into(),
        });
        assert!(unknown.starts_with("ping failed:"), "{unknown}");
        assert!(!unknown.contains('\n'), "{unknown}");
    }

    /// Ping / region lookup for a provider with no client wired must
    /// fail loudly rather than silently succeeding with nothing —
    /// a silent `Ok` would save an unvalidated token and an empty
    /// region list.
    #[test]
    fn ping_and_region_lookup_refuse_an_unwired_provider() {
        let ping = ping_for_provider("aws", "unused-token").expect_err("no client wired for aws");
        assert!(ping.to_string().contains("aws"), "{ping}");

        let regions = fetch_regions("aws", "unused-token").expect_err("no client wired for aws");
        assert!(regions.to_string().contains("aws"), "{regions}");
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

    /// The offered default is the modern OpenSSH key name. It is
    /// the value most operators will accept with a single Return,
    /// so pointing it at a stale name (`id_rsa.pub`) would push
    /// people onto a weaker key or an empty prompt.
    #[test]
    fn default_ssh_key_hint_offers_the_modern_openssh_key_name() {
        let hint = default_ssh_key_hint();
        assert!(hint.ends_with(".ssh/id_ed25519.pub"), "{hint}");
    }

    /// A key file with no trailing comment renders with the algo
    /// alone — no dangling ", " separator. A file that isn't in
    /// OpenSSH layout at all degrades to the bare path instead of
    /// showing half-parsed junk in the picker.
    #[test]
    fn ssh_key_label_omits_the_comment_clause_when_the_key_has_none() {
        let dir = tempfile::tempdir().unwrap();

        let bare = dir.path().join("nocomment.pub");
        std::fs::write(&bare, "ssh-ed25519 AAAAfakebody\n").unwrap();
        let label = ssh_key_label(&bare);
        assert!(label.ends_with("(ssh-ed25519)"), "{label}");

        let junk = dir.path().join("junk.pub");
        std::fs::write(&junk, "not-a-key\n").unwrap();
        let junk_label = ssh_key_label(&junk);
        assert_eq!(junk_label, abbreviate_home_path(&junk));
    }

    /// Regions whose probe never reported still appear in the
    /// picker, at the end, with `n/a` latency. Dropping them would
    /// shrink the picker — in the worst case (every probe hung) to
    /// nothing at all, leaving the operator no region to choose.
    #[test]
    fn finalize_latency_rows_keeps_unreported_regions_and_sorts_them_last() {
        let region = |n: &str| RegionInfo {
            name: n.into(),
            description: n.to_uppercase(),
        };
        let originals = vec![region("fsn1"), region("hel1"), region("nbg1")];
        // Only two of the three probes came back, and the slower
        // one reported first.
        let reported = vec![
            RegionWithLatency {
                info: region("nbg1"),
                latency_ms: Some(42),
            },
            RegionWithLatency {
                info: region("hel1"),
                latency_ms: Some(7),
            },
        ];

        let rows = finalize_latency_rows(originals, reported);
        let order: Vec<(String, Option<u32>)> = rows
            .into_iter()
            .map(|r| (r.info.name, r.latency_ms))
            .collect();
        assert_eq!(
            order,
            vec![
                ("hel1".to_string(), Some(7)),
                ("nbg1".to_string(), Some(42)),
                ("fsn1".to_string(), None),
            ]
        );
    }
}
