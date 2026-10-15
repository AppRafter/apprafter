// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! `apprafter target machine` — set or change the server type (and region)
//! on an existing target.
//!
//! This is the ONLY way to change the server type on an existing target:
//! `target add <existing>` errors without `--force`, and `--renew` is
//! credentials-only.
//!
//! Behaviour matrix:
//!
//! | `--no-ping` | `--server-type` | Result |
//! |-------------|-----------------|--------|
//! | yes         | Some(sku)       | Patch mode — save without API validation |
//! | yes         | None            | Error — picker needs the API |
//! | no          | Some(sku)       | Validate SKU via API, then save |
//! | no          | None (TTY)      | Interactive picker (fetch + latency + pick_machine), then validate and save |
//! | no          | None (no TTY)   | Error — need `--server-type` in non-interactive |
//!
//! **Provisioned guard**: when the resolved target already has a live server
//! (`state.hetzner_cloud` is `Some`), the command hard-refuses BEFORE writing
//! anything. There is no in-place machine resize; the rebuild path is
//! `backup create`, `destroy`, then `restore <repo> --reprovision --server-type <sku>`
//! (`render::core_error::resize_recipe`).
//!
//! The work is apprafter-core's (`target::plan_machine` / `execute_machine`: the SKU check,
//! then the store lock, a re-check, and a patch of the machine fields only). This module keeps
//! the CLI's part: the legacy cwd migration, the flag matrix, the TTY picker and the output.

use std::io::IsTerminal;

use apprafter_core::target::{MachineChoice, SkuCheck};
use apprafter_core::{CancellationToken, CoreError, TargetRef};
use cli_core::{CliError, Result};

use crate::commands::state_paths::resolve_state_paths;
use crate::commands::target::{completed, require_loadable};
use crate::render::core_error::report;
use crate::render::reporter::CliReporter;

/// Arguments extracted from the `TargetCommand::Machine` variant.
pub struct MachineArgs {
    pub target: Option<String>,
    pub server_type: Option<String>,
    pub no_ping: bool,
}

/// The branch `run_machine` takes, decided before any IO happens.
///
/// This is the module-doc behaviour matrix as data: keeping the decision in one
/// pure place means the "picker needs the API" and "non-interactive needs a
/// SKU" refusals cannot drift away from the flags that trigger them.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum MachineAction {
    /// `--no-ping --server-type <sku>` — record the SKU as-is, no API call.
    RecordUnvalidated(String),
    /// `--server-type <sku>` — validate against the live catalogue, then record.
    ValidateThenRecord(String),
    /// Neither flag on a TTY — fetch the catalogue and run the picker.
    Picker,
}

/// Resolve the behaviour matrix. `interactive` is "both stdin and stdout are
/// TTYs"; the caller computes it so this stays testable.
pub(crate) fn decide_machine_action(
    no_ping: bool,
    server_type: Option<&str>,
    interactive: bool,
) -> Result<MachineAction> {
    match (no_ping, server_type) {
        (true, Some(sku)) => Ok(MachineAction::RecordUnvalidated(sku.to_string())),
        (true, None) => Err(CliError::UsageRefused {
            message: "`target machine` needs the provider API to show the picker — \
                      drop `--no-ping` or pass `--server-type <sku>`"
                .to_string(),
            help: "The picker reads the provider's catalogue: drop `--no-ping`, or pass \
                   `--server-type <sku>` to record a type without checking it."
                .to_string(),
        }),
        (false, Some(sku)) => Ok(MachineAction::ValidateThenRecord(sku.to_string())),
        (false, None) if interactive => Ok(MachineAction::Picker),
        (false, None) => Err(CliError::UsageRefused {
            message: "non-interactive shell: pass `--server-type <sku>` to set the machine type \
                      without the interactive picker"
                .to_string(),
            help: "Pass `--server-type <sku>`, or run `apprafter target machine` in a terminal to \
                   open the picker."
                .to_string(),
        }),
    }
}

/// How the SKU that just got saved was arrived at. Drives the confirmation, so
/// an operator can always tell an API-checked write from an unchecked one.
pub(crate) enum SavedVia<'a> {
    /// `--no-ping` — nothing verified the SKU exists.
    Unvalidated,
    /// Checked against the live catalogue for this region.
    ValidatedForRegion(&'a str),
    /// Chosen in the picker, which also (re)sets the region.
    Picked(&'a str),
}

/// One-line confirmation for a saved machine type.
pub(crate) fn saved_message(target_name: &str, sku: &str, via: SavedVia<'_>) -> String {
    match via {
        SavedVia::Unvalidated => format!(
            "server type set to `{sku}` on target `{target_name}` — NOT validated (--no-ping)"
        ),
        SavedVia::ValidatedForRegion(region) => format!(
            "server type set to `{sku}` on target `{target_name}` \
             (validated against Hetzner Cloud for region `{region}`)"
        ),
        SavedVia::Picked(region) => {
            format!("server type set to `{sku}` / region `{region}` on target `{target_name}`")
        }
    }
}

/// Run `apprafter target machine`: the legacy cwd migration (CLI-only), the provisioned refusal
/// before the flag matrix (today's order), the picker on a TTY, then the core's plan and
/// execute. A machine picked in the picker is validated again by `execute_machine` (one more
/// `/v1/server_types` request, deviation 4).
pub fn run_machine(args: MachineArgs) -> miette::Result<()> {
    let resolved = resolve_state_paths(args.target.as_deref()).map_err(miette::Report::new)?;
    let ctx = crate::context::cli_context()?.with_no_ping(args.no_ping);
    let tref = TargetRef::named(&ctx, &resolved.target_name).map_err(report)?;
    require_loadable(&ctx, tref.name())?;
    if let Some(s) = apprafter_core::target::provisioned(&ctx, &tref).map_err(report)? {
        return Err(report(CoreError::TargetProvisioned {
            name: resolved.target_name.clone(),
            server_id: s.server_id,
            server_name: s.server_name,
        }));
    }
    let interactive = std::io::stdin().is_terminal() && std::io::stdout().is_terminal();
    let (choice, picked) =
        match decide_machine_action(args.no_ping, args.server_type.as_deref(), interactive)
            .map_err(miette::Report::new)?
        {
            MachineAction::RecordUnvalidated(sku) | MachineAction::ValidateThenRecord(sku) => {
                (MachineChoice { sku, region: None }, false)
            }
            MachineAction::Picker => {
                let token = apprafter_core::target::hetzner_token(&ctx, &tref).map_err(report)?;
                let provider = cli_core::target::load_target_config(&ctx.store(), tref.name())
                    .map_err(miette::Report::new)?
                    .provider;
                // No prefill: the operator picks both. `report`, never `Report::new`: a picker
                // failure keeps the CLI's help (overview §3.6.4).
                let (region, sku) = crate::commands::target_wizard::prompt_machine(
                    &ctx, &provider, &token, None, None, false,
                )
                .map_err(report)?;
                let (region, sku) =
                    normalize_picker_result(region, sku).map_err(miette::Report::new)?;
                (
                    MachineChoice {
                        sku,
                        region: Some(region),
                    },
                    true,
                )
            }
        };
    let plan = apprafter_core::target::plan_machine(&ctx, &tref, choice).map_err(report)?;
    let set = completed(
        apprafter_core::target::execute_machine(
            &ctx,
            plan,
            &CliReporter,
            &CancellationToken::new(),
        )
        .map_err(report)?,
    )?;
    let via = match (&set.sku_check, &set.region) {
        (_, Some(r)) if picked => SavedVia::Picked(r),
        (SkuCheck::Validated { region, .. }, _) => SavedVia::ValidatedForRegion(region),
        (SkuCheck::NotValidated { .. }, _) => SavedVia::Unvalidated,
    };
    println!("{}", saved_message(&resolved.target_name, &set.sku, via));
    Ok(())
}

/// Normalise what the picker handed back.
///
/// A missing SKU is a hard error: silently substituting a default would
/// provision a machine the operator never chose. A missing region falls back to
/// [`apprafter_core::machine::DEFAULT_REGION`], which is what the picker itself defaults to.
pub(crate) fn normalize_picker_result(
    picked_region: Option<String>,
    picked_sku: Option<String>,
) -> Result<(String, String)> {
    let sku = picked_sku.ok_or_else(|| {
        CliError::Other("machine picker did not return a server type — please retry".to_string())
    })?;
    Ok((
        picked_region.unwrap_or_else(|| apprafter_core::machine::DEFAULT_REGION.to_string()),
        sku,
    ))
}

#[cfg(test)]
mod tests {
    use super::{
        decide_machine_action, normalize_picker_result, saved_message, MachineAction, SavedVia,
    };

    // ── decide_machine_action ────────────────────────────────────────────

    /// `--no-ping --server-type` is the ONLY combination that may write a SKU
    /// without asking the API. If any other row of the matrix landed here, a
    /// typo'd SKU would be persisted unchecked.
    #[test]
    fn no_ping_with_a_sku_records_without_validating() {
        assert_eq!(
            decide_machine_action(true, Some("cx32"), true).unwrap(),
            MachineAction::RecordUnvalidated("cx32".to_string())
        );
        // TTY-ness is irrelevant once a SKU is supplied.
        assert_eq!(
            decide_machine_action(true, Some("cx32"), false).unwrap(),
            MachineAction::RecordUnvalidated("cx32".to_string())
        );
    }

    /// A SKU without `--no-ping` must reach the API-validating branch, on a TTY
    /// or not — the picker is what needs a terminal, validation is not.
    #[test]
    fn a_sku_without_no_ping_is_validated_first() {
        assert_eq!(
            decide_machine_action(false, Some("cx32"), true).unwrap(),
            MachineAction::ValidateThenRecord("cx32".to_string())
        );
        assert_eq!(
            decide_machine_action(false, Some("cx32"), false).unwrap(),
            MachineAction::ValidateThenRecord("cx32".to_string())
        );
    }

    #[test]
    fn no_flags_on_a_tty_opens_the_picker() {
        assert_eq!(
            decide_machine_action(false, None, true).unwrap(),
            MachineAction::Picker
        );
    }

    /// The two refusals must stay distinguishable: `--no-ping` alone is a flag
    /// contradiction (the picker needs the API), no-flags-no-TTY is a missing
    /// input. Each message names the flag that fixes it.
    #[test]
    fn the_two_refusals_name_the_flag_that_fixes_them() {
        let no_api = decide_machine_action(true, None, true)
            .expect_err("`--no-ping` with no SKU cannot run the picker");
        let msg = format!("{no_api}");
        assert!(msg.contains("--no-ping"), "{msg}");
        assert!(msg.contains("--server-type"), "{msg}");

        let no_tty = decide_machine_action(false, None, false)
            .expect_err("a non-interactive shell cannot run the picker");
        let msg = format!("{no_tty}");
        assert!(msg.contains("non-interactive"), "{msg}");
        assert!(msg.contains("--server-type"), "{msg}");

        // Bug 2: a usage refusal with its own code and a way forward, never the catch-all.
        for err in [no_api, no_tty] {
            let code = miette::Diagnostic::code(&err).map(|c| c.to_string());
            assert_eq!(code.as_deref(), Some("apprafter::cli::usage_refused"));
            let help = miette::Diagnostic::help(&err)
                .map(|h| h.to_string())
                .unwrap_or_default();
            assert!(help.contains("--server-type <sku>"), "{help}");
            crate::commands::target::assert_commands_parse(&help);
        }
    }

    /// `--no-ping` on a non-TTY still records rather than tripping the
    /// non-interactive refusal — the pair of conditions must be evaluated in
    /// the documented order, not collapsed into "no TTY ⇒ error".
    #[test]
    fn a_non_tty_shell_is_not_refused_when_a_sku_is_supplied() {
        assert!(decide_machine_action(true, Some("cx32"), false).is_ok());
        assert!(decide_machine_action(false, Some("cx32"), false).is_ok());
    }

    // ── saved_message ────────────────────────────────────────────────────

    /// An unvalidated write MUST say so. Silently rendering it like a checked
    /// one would let a typo'd SKU sit in the target until the next `apply`
    /// fails on the provider side.
    #[test]
    fn an_unvalidated_save_is_flagged_and_a_validated_one_names_its_region() {
        let unvalidated = saved_message("work", "cx32", SavedVia::Unvalidated);
        assert!(unvalidated.contains("NOT validated"), "{unvalidated}");
        assert!(unvalidated.contains("cx32"), "{unvalidated}");
        assert!(unvalidated.contains("work"), "{unvalidated}");

        let validated = saved_message("work", "cx32", SavedVia::ValidatedForRegion("hel1"));
        assert!(!validated.contains("NOT validated"), "{validated}");
        assert!(validated.contains("hel1"), "{validated}");
    }

    /// The picker also moves the region, so its confirmation has to report the
    /// region as well — otherwise an operator who did not notice the region
    /// change reads it as a machine-only edit.
    #[test]
    fn a_picked_save_reports_the_region_it_also_changed() {
        let picked = saved_message("work", "cx42", SavedVia::Picked("fsn1"));
        assert!(picked.contains("cx42"), "{picked}");
        assert!(picked.contains("fsn1"), "{picked}");
        assert!(picked.contains("work"), "{picked}");
    }

    // ── normalize_picker_result ──────────────────────────────────────────

    /// A picker that returned no SKU is a bug, not a default: substituting one
    /// would provision a machine the operator never chose (and never saw a
    /// price for).
    #[test]
    fn a_picker_that_returned_no_sku_is_an_error_not_a_default() {
        let err = normalize_picker_result(Some("hel1".to_string()), None)
            .expect_err("a missing SKU must not be defaulted");
        assert!(format!("{err}").contains("did not return a server type"));
    }

    #[test]
    fn a_picked_sku_without_a_region_falls_back_to_the_default_region() {
        let (region, sku) = normalize_picker_result(None, Some("cx42".to_string())).unwrap();
        assert_eq!(sku, "cx42");
        assert_eq!(region, apprafter_core::machine::DEFAULT_REGION);

        let (region, sku) =
            normalize_picker_result(Some("fsn1".to_string()), Some("cx42".to_string())).unwrap();
        assert_eq!((region.as_str(), sku.as_str()), ("fsn1", "cx42"));
    }
}
