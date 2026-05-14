// SPDX-License-Identifier: FSL-1.1-MIT
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

use std::path::PathBuf;

use cli_core::target::validate_hetzner_token_format;
use cli_core::{CliError, Result};
use cli_providers::{HetznerCloudValidator, ProviderValidator, RegionInfo};
use inquire::validator::Validation;
use inquire::{InquireError, Password, PasswordDisplayMode, Select, Text};

use crate::commands::hcloud::hcloud_base_url;
use crate::commands::target::AddArgs;

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
pub fn should_use_wizard(
    no_interactive: bool,
    stdin_is_terminal: bool,
    stdout_is_terminal: bool,
    has_all_required_flags: bool,
) -> bool {
    if no_interactive {
        return false;
    }
    if !(stdin_is_terminal && stdout_is_terminal) {
        return false;
    }
    // If the user already supplied every required input, respect
    // their intent and skip the wizard — the non-interactive flow
    // from v0.1.73 + the ping from v0.1.75 already give them
    // everything the wizard would.
    !has_all_required_flags
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

    let name = prompt_name(initial.name.as_deref())?;
    let provider = prompt_provider(initial.provider.as_deref())?;
    let (token, token_already_verified) =
        prompt_token(&provider, initial.token.as_deref(), initial.no_ping)?;
    let ssh_key = prompt_ssh_key(initial.ssh_key.as_ref())?;
    let region = prompt_region(
        &provider,
        &token,
        initial.region.as_deref(),
        initial.no_ping,
    )?;
    let tier = prompt_tier(initial.tier.as_deref())?;

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
    prompt_token(provider, None, no_ping)
}

// ---------------------------------------------------------------
// Individual prompts
// ---------------------------------------------------------------

fn prompt_name(prefill: Option<&str>) -> Result<String> {
    let default = prefill.unwrap_or(DEFAULT_TARGET_NAME).to_string();
    let answer = Text::new("Target name:")
        .with_default(&default)
        .with_validator(|v: &str| match super::target::check_target_name(v) {
            Ok(()) => Ok(Validation::Valid),
            Err(msg) => Ok(Validation::Invalid(msg.into())),
        })
        .prompt()
        .map_err(map_inquire_err)?;
    Ok(answer)
}

fn prompt_provider(prefill: Option<&str>) -> Result<String> {
    // Single-entry Select today — kept as a Select so adding a
    // provider in the future doesn't reshape the wizard surface.
    if let Some(p) = prefill {
        // Honour the flag if it was supplied even when the wizard
        // is otherwise prompting for other fields. Skipping the
        // prompt entirely matches the "wizard only fills missing
        // bits" contract.
        if PROVIDER_CHOICES.contains(&p) {
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

fn prompt_token(provider: &str, prefill: Option<&str>, no_ping: bool) -> Result<(String, bool)> {
    if let Some(tok) = prefill {
        // Validate up-front so the user gets the error attached to
        // the flag (`--token` or `HCLOUD_TOKEN` env) rather than a
        // surprise mid-wizard prompt.
        if let Err(reason) = validate_for_provider(provider, tok) {
            return Err(CliError::Other(reason));
        }
        let verified = if no_ping {
            false
        } else {
            ping_for_provider(provider, tok)
                .map_err(|e| CliError::Other(format!("token rejected by provider: {e}")))?;
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

fn prompt_ssh_key(prefill: Option<&PathBuf>) -> Result<Option<PathBuf>> {
    if let Some(path) = prefill {
        return Ok(Some(path.clone()));
    }
    let default = default_ssh_key_hint();
    let answer = Text::new("SSH public key path (leave empty to skip):")
        .with_default(&default)
        .with_help_message("Used for server provisioning; can be set later.")
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

fn prompt_region(
    provider: &str,
    token: &str,
    prefill: Option<&str>,
    no_ping: bool,
) -> Result<Option<String>> {
    if let Some(r) = prefill {
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
    let selected = Select::new("Default region:", regions)
        .prompt()
        .map_err(map_inquire_err)?;
    Ok(Some(selected.name))
}

fn prompt_tier(prefill: Option<&str>) -> Result<Option<String>> {
    if let Some(t) = prefill {
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
    fn should_use_wizard_only_when_tty_and_no_flag_and_missing_required() {
        // No-interactive flag wins everything else.
        assert!(!should_use_wizard(true, true, true, false));
        // No TTY on either stream → no wizard.
        assert!(!should_use_wizard(false, false, true, false));
        assert!(!should_use_wizard(false, true, false, false));
        // All flags supplied → no wizard (respect explicit non-interactive intent).
        assert!(!should_use_wizard(false, true, true, true));
        // TTY + something missing → wizard.
        assert!(should_use_wizard(false, true, true, false));
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
}
