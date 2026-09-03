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
//! | no          | None (TTY)      | Interactive picker (fetch + latency + pick_machine) |
//! | no          | None (no TTY)   | Error — need `--server-type` in non-interactive |
//!
//! **Provisioned guard**: when the resolved target already has a live server
//! (`state.hetzner_cloud` is `Some`), the command hard-refuses BEFORE writing
//! anything. There is no in-place machine resize; the rebuild path is
//! `apprafter backup create` + `apprafter restore --reprovision --server-type <sku>`.

use std::io::IsTerminal;

use cli_core::target::{
    default_config_root, load_target, save_target, TargetConfig, TargetStorePaths,
};
use cli_core::{CliError, Result};
use cli_providers::hetzner_cloud::validate_server_type;
use cli_providers::HetznerCloudClient;

use crate::commands::hcloud::hcloud_base_url;
use crate::commands::state_paths::resolve_state_paths;

/// Arguments extracted from the `TargetCommand::Machine` variant.
pub struct MachineArgs {
    pub target: Option<String>,
    pub server_type: Option<String>,
    pub no_ping: bool,
}

/// Region assumed when the target has none recorded yet. Server-type
/// availability is per-location on Hetzner, so validation needs *some* region.
pub const DEFAULT_REGION: &str = "nbg1";

/// Pure helper: returns `true` when the state indicates a server has been
/// provisioned (i.e. `state.hetzner_cloud` is `Some`).
///
/// Used to gate `target machine` so it refuses on a live cluster rather than
/// silently recording a preference that will never take effect without a rebuild.
pub fn is_provisioned(state: &cli_state::State) -> bool {
    state.hetzner_cloud.is_some()
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
        (true, None) => Err(CliError::Other(
            "`target machine` needs the provider API to show the picker — \
             drop `--no-ping` or pass `--server-type <sku>`"
                .to_string(),
        )),
        (false, Some(sku)) => Ok(MachineAction::ValidateThenRecord(sku.to_string())),
        (false, None) if interactive => Ok(MachineAction::Picker),
        (false, None) => Err(CliError::Other(
            "non-interactive shell: pass `--server-type <sku>` to set the machine type \
             without the interactive picker"
                .to_string(),
        )),
    }
}

/// Refusal shown when the target already runs a provisioned cluster.
///
/// There is no in-place resize, so the message has to hand the operator the
/// whole rebuild recipe — a bare "not allowed" leaves them stuck.
pub(crate) fn provisioned_refusal_message(target_name: &str) -> String {
    format!(
        "`{target_name}` already runs a provisioned cluster — its machine type cannot be \
         changed in place. To move to a different machine, rebuild from a backup:\n\n    \
         apprafter backup create\n    \
         apprafter restore --reprovision --server-type <sku>\n\n\
         (`target machine` only sets the type on a target that has NOT provisioned yet.)"
    )
}

/// The region a SKU is validated against: the target's own, else the default.
pub(crate) fn region_for_validation(current_region: Option<&str>) -> String {
    current_region.unwrap_or(DEFAULT_REGION).to_string()
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

/// Everything `run_machine` needs from the outside world: the target record it
/// patches, the provider catalogue it validates against, the picker, the
/// confirmation prompt and stdout.
///
/// Inverting these lets the whole decision body ([`machine_core`]) run under a
/// fake in tests, so the ordering guarantees that matter — refuse BEFORE any
/// write, validate BEFORE any write, save BEFORE reporting success — are
/// checked rather than assumed.
pub(crate) trait MachineEnv {
    /// Does this target already run a live server?
    fn is_provisioned(&mut self) -> bool;
    /// The target's current config.
    fn config(&mut self) -> TargetConfig;
    /// Persist the patched config.
    fn save(&mut self, config: TargetConfig) -> Result<()>;
    /// Check the SKU exists in `region` per the provider catalogue.
    fn validate_sku(&mut self, sku: &str, region: &str) -> Result<()>;
    /// Run the interactive machine picker; returns `(region, sku)`.
    fn pick_machine(&mut self) -> Result<(Option<String>, Option<String>)>;
    /// Ask a yes/no question; `false` means the operator declined.
    fn confirm(&mut self, prompt: &str) -> Result<bool>;
    /// Emit one line to the operator.
    fn report(&mut self, line: &str);
}

/// The whole `target machine` decision body, free of direct IO.
///
/// Behaviour is identical to the pre-extraction inline body; only the effects
/// are routed through [`MachineEnv`].
pub(crate) fn machine_core(
    env: &mut dyn MachineEnv,
    target_name: &str,
    args: &MachineArgs,
    interactive: bool,
) -> Result<()> {
    // ── Provisioned guard (before any write) ─────────────────────────────
    // There is no in-place machine resize. Attempting to change the type on
    // a running cluster would silently record a preference that apply() would
    // shadow with the recorded-fact value. Hard-refuse and guide to the
    // rebuild path instead.
    if env.is_provisioned() {
        return Err(CliError::Other(provisioned_refusal_message(target_name)));
    }

    let config = env.config();
    match decide_machine_action(args.no_ping, args.server_type.as_deref(), interactive)? {
        // (A) --no-ping + --server-type  → patch without validation
        MachineAction::RecordUnvalidated(sku) => {
            env.save(with_machine(config, &sku, None))?;
            env.report(&saved_message(target_name, &sku, SavedVia::Unvalidated));
        }

        // (C) --server-type without --no-ping → API-validate then patch
        MachineAction::ValidateThenRecord(sku) => {
            let current_region = region_for_validation(config.region.as_deref());
            env.validate_sku(&sku, &current_region)?;
            env.save(with_machine(config, &sku, None))?;
            env.report(&saved_message(
                target_name,
                &sku,
                SavedVia::ValidatedForRegion(&current_region),
            ));
        }

        // (D) Interactive (no --server-type, no --no-ping)
        MachineAction::Picker => {
            // Reuse the wizard's machine-matrix step verbatim: fetch
            // catalog → measure latency → pick_machine.
            let (region, sku) = env.pick_machine()?;
            // picked_region and picked_sku are Some(_) because no-ping=false
            // and prompt_machine only returns (None, None) in the no-ping branch.
            let (picked_region, picked_sku) = normalize_picker_result(region, sku)?;

            // Region-change confirm: only when a server is already provisioned
            // AND the picked region differs from the target's current region.
            // Note: the provisioned guard above already refused when a server
            // exists, so `server_provisioned` is always false here in practice.
            // The `needs_region_confirm` call is kept for symmetry / future
            // use if this path ever runs after a destroy with the state wiped.
            let server_provisioned = false; // guard above ensures no live server

            if needs_region_confirm(config.region.as_deref(), &picked_region, server_provisioned)
                && !env.confirm(&region_change_prompt(
                    config.region.as_deref(),
                    &picked_region,
                ))?
            {
                env.report(&aborted_message(target_name));
                return Ok(());
            }

            env.save(with_machine(config, &picked_sku, Some(&picked_region)))?;
            env.report(&saved_message(
                target_name,
                &picked_sku,
                SavedVia::Picked(&picked_region),
            ));
        }
    }

    Ok(())
}

/// The production [`MachineEnv`]: the real target store, the real Hetzner
/// catalogue, the real `inquire` prompt and the real stdout.
struct CliMachineEnv<'a> {
    store: &'a TargetStorePaths,
    target: &'a mut cli_core::target::Target,
    provisioned: bool,
    target_name: &'a str,
}

impl MachineEnv for CliMachineEnv<'_> {
    fn is_provisioned(&mut self) -> bool {
        self.provisioned
    }

    fn config(&mut self) -> TargetConfig {
        self.target.config.clone()
    }

    fn save(&mut self, config: TargetConfig) -> Result<()> {
        self.target.config = config;
        save_target(self.store, self.target)
    }

    fn validate_sku(&mut self, sku: &str, region: &str) -> Result<()> {
        let token = cli_core::resolve_hetzner_token(None, self.store, Some(self.target_name))?;
        let client = HetznerCloudClient::new(hcloud_base_url(), &token);
        let types = client.list_server_types()?.server_types;
        validate_server_type(&types, sku, region)
    }

    fn pick_machine(&mut self) -> Result<(Option<String>, Option<String>)> {
        let token = cli_core::resolve_hetzner_token(None, self.store, Some(self.target_name))?;
        crate::commands::target_wizard::prompt_machine(
            &self.target.config.provider,
            &token,
            None, // no prefill region — let the user pick
            None, // no prefill sku
            false,
        )
    }

    fn confirm(&mut self, prompt: &str) -> Result<bool> {
        inquire::Confirm::new(prompt)
            .with_default(false)
            .prompt()
            .map_err(map_confirm_error)
    }

    fn report(&mut self, line: &str) {
        println!("{line}");
    }
}

/// Run `apprafter target machine`.
pub fn run_machine(args: MachineArgs) -> Result<()> {
    // We need both the TargetStorePaths (for the target store) and the
    // State (for the provisioned-server check). Re-use resolve_state_paths
    // to get the state, but we also need the raw target record to patch it.
    let resolved = resolve_state_paths(args.target.as_deref())?;
    let store = TargetStorePaths::for_root(default_config_root()?);
    let mut target = load_target(&store, &resolved.target_name)?;
    let state = cli_state::State::load_or_default(&resolved.paths)?;
    let interactive = std::io::stdin().is_terminal() && std::io::stdout().is_terminal();

    let mut env = CliMachineEnv {
        store: &store,
        provisioned: is_provisioned(&state),
        target_name: &resolved.target_name,
        target: &mut target,
    };
    machine_core(&mut env, &resolved.target_name, &args, interactive)
}

/// Whether picking a new region needs a confirmation prompt.
///
/// Only returns `true` when:
/// - A server is already provisioned (`server_provisioned == true`), AND
/// - A current region is known AND it differs from the newly-picked region.
///
/// Rationale: the running server stays in the old region after a
/// metadata-only patch; the user needs to understand that before we save.
pub fn needs_region_confirm(
    current_region: Option<&str>,
    picked_region: &str,
    server_provisioned: bool,
) -> bool {
    server_provisioned && current_region.is_some_and(|c| c != picked_region)
}

/// Record a machine choice on a target's config.
///
/// `region` is `Some` only for the picker (which also moves the region);
/// the SKU-flag paths pass `None` so an existing region is preserved rather
/// than blanked. Everything else on the config is carried through untouched.
pub(crate) fn with_machine(
    mut config: TargetConfig,
    sku: &str,
    region: Option<&str>,
) -> TargetConfig {
    config.server_type = Some(sku.to_string());
    if let Some(r) = region {
        config.region = Some(r.to_string());
    }
    config
}

/// Normalise what the picker handed back.
///
/// A missing SKU is a hard error: silently substituting a default would
/// provision a machine the operator never chose. A missing region falls back to
/// [`DEFAULT_REGION`], which is what the picker itself defaults to.
pub(crate) fn normalize_picker_result(
    picked_region: Option<String>,
    picked_sku: Option<String>,
) -> Result<(String, String)> {
    let sku = picked_sku.ok_or_else(|| {
        CliError::Other("machine picker did not return a server type — please retry".to_string())
    })?;
    Ok((
        picked_region.unwrap_or_else(|| DEFAULT_REGION.to_string()),
        sku,
    ))
}

/// Translate an `inquire` prompt failure.
///
/// A Ctrl-C / Esc is a deliberate abort and must read like one; anything else
/// is a genuine terminal problem and keeps its underlying detail.
pub(crate) fn map_confirm_error(err: inquire::InquireError) -> CliError {
    match err {
        inquire::InquireError::OperationCanceled | inquire::InquireError::OperationInterrupted => {
            CliError::Other("aborted by user".to_string())
        }
        other => CliError::Other(format!("confirmation prompt failed: {other}")),
    }
}

/// Message for a declined region-change confirmation. It must state that
/// NOTHING was written — the operator declined mid-command.
pub(crate) fn aborted_message(target_name: &str) -> String {
    format!("aborted; target `{target_name}` left intact")
}

/// The region-change confirmation text.
///
/// It has to spell out that the RUNNING server does not move — the whole risk
/// of saying yes is that the metadata and the live machine end up in different
/// locations until a re-provision.
pub(crate) fn region_change_prompt(current_region: Option<&str>, picked_region: &str) -> String {
    let old_region = current_region.unwrap_or("(unset)");
    format!(
        "This also changes the region `{old_region}` → `{picked_region}`; \
         the running server stays in `{old_region}` until you re-provision. \
         Continue?"
    )
}

#[cfg(test)]
mod tests {
    use super::{is_provisioned, needs_region_confirm};
    use cli_state::{HetznerCloudState, State};

    // ── is_provisioned ───────────────────────────────────────────────────

    fn state_with_server() -> State {
        State {
            hetzner_cloud: Some(HetznerCloudState {
                server_id: 42,
                server_name: "platform-1".into(),
                server_type: Some("cx22".into()),
                ssh_key_ids: vec![],
                network_id: None,
                firewall_id: None,
                floating_ip_ids: vec![],
                kubeconfig_yaml: None,
                kubeconfig_age: None,
                argocd_admin_password_age: None,
            }),
            ..State::default()
        }
    }

    fn state_without_server() -> State {
        State::default()
    }

    #[test]
    fn is_provisioned_returns_true_when_hetzner_cloud_present() {
        assert!(is_provisioned(&state_with_server()));
    }

    #[test]
    fn is_provisioned_returns_false_when_hetzner_cloud_absent() {
        assert!(!is_provisioned(&state_without_server()));
    }

    // ── needs_region_confirm ─────────────────────────────────────────────

    #[test]
    fn provisioned_different_region_needs_confirm() {
        assert!(needs_region_confirm(Some("nbg1"), "hel1", true));
    }

    #[test]
    fn provisioned_same_region_no_confirm() {
        assert!(!needs_region_confirm(Some("nbg1"), "nbg1", true));
    }

    #[test]
    fn not_provisioned_different_region_no_confirm() {
        assert!(!needs_region_confirm(Some("nbg1"), "hel1", false));
    }

    #[test]
    fn no_current_region_no_confirm() {
        // When the target has no region set yet, no confirmation is needed
        // regardless of what the picker chose or whether a server is running.
        assert!(!needs_region_confirm(None, "hel1", true));
        assert!(!needs_region_confirm(None, "nbg1", false));
    }

    // ── decide_machine_action ────────────────────────────────────────────

    use super::{
        aborted_message, decide_machine_action, machine_core, map_confirm_error,
        normalize_picker_result, provisioned_refusal_message, region_change_prompt,
        region_for_validation, saved_message, with_machine, MachineAction, MachineArgs, MachineEnv,
        SavedVia, DEFAULT_REGION,
    };
    use cli_core::target::TargetConfig;
    use cli_core::{CliError, Result};

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
    }

    /// `--no-ping` on a non-TTY still records rather than tripping the
    /// non-interactive refusal — the pair of conditions must be evaluated in
    /// the documented order, not collapsed into "no TTY ⇒ error".
    #[test]
    fn a_non_tty_shell_is_not_refused_when_a_sku_is_supplied() {
        assert!(decide_machine_action(true, Some("cx32"), false).is_ok());
        assert!(decide_machine_action(false, Some("cx32"), false).is_ok());
    }

    // ── region_for_validation ────────────────────────────────────────────

    #[test]
    fn validation_uses_the_targets_region_and_falls_back_to_the_default() {
        assert_eq!(region_for_validation(Some("hel1")), "hel1");
        assert_eq!(region_for_validation(None), DEFAULT_REGION);
        assert_eq!(DEFAULT_REGION, "nbg1");
    }

    // ── provisioned_refusal_message ──────────────────────────────────────

    /// The refusal is the operator's only pointer to the rebuild path; it has
    /// to carry both commands, in order, plus the name it refused.
    #[test]
    fn the_provisioned_refusal_hands_over_the_whole_rebuild_recipe() {
        let m = provisioned_refusal_message("prod-eu");
        assert!(m.contains("prod-eu"), "{m}");
        let backup = m
            .find("apprafter backup create")
            .expect("names the backup step");
        let restore = m
            .find("apprafter restore --reprovision --server-type")
            .expect("names the reprovision step");
        assert!(backup < restore, "the steps must be listed in order: {m}");
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

    // ── with_machine ─────────────────────────────────────────────────────

    /// The SKU-flag paths pass `region: None` and must LEAVE the recorded
    /// region alone — blanking it there would silently move the next
    /// provision to the default location.
    #[test]
    fn recording_a_sku_alone_preserves_the_region_and_the_rest_of_the_config() {
        let before = TargetConfig {
            provider: "hetzner-cloud".to_string(),
            region: Some("hel1".to_string()),
            server_type: Some("cx22".to_string()),
            cluster_name: Some("platform-7".to_string()),
            ..TargetConfig::default()
        };
        let after = with_machine(before.clone(), "cx42", None);
        assert_eq!(after.server_type.as_deref(), Some("cx42"));
        assert_eq!(after.region.as_deref(), Some("hel1"));
        assert_eq!(
            TargetConfig {
                server_type: before.server_type.clone(),
                ..after
            },
            before,
            "only `server_type` may change when no region is supplied"
        );
    }

    /// The picker path supplies a region and must move BOTH fields together —
    /// a SKU saved without its region would be validated against the wrong
    /// location on the next run.
    #[test]
    fn the_picker_path_moves_the_server_type_and_the_region_together() {
        let before = TargetConfig {
            region: Some("hel1".to_string()),
            server_type: Some("cx22".to_string()),
            ..TargetConfig::default()
        };
        let after = with_machine(before, "cx42", Some("fsn1"));
        assert_eq!(after.server_type.as_deref(), Some("cx42"));
        assert_eq!(after.region.as_deref(), Some("fsn1"));
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
        assert_eq!(region, DEFAULT_REGION);

        let (region, sku) =
            normalize_picker_result(Some("fsn1".to_string()), Some("cx42".to_string())).unwrap();
        assert_eq!((region.as_str(), sku.as_str()), ("fsn1", "cx42"));
    }

    // ── map_confirm_error ────────────────────────────────────────────────

    /// Ctrl-C / Esc is a deliberate abort and must read as one. Rendering it
    /// as "confirmation prompt failed" sends operators debugging a terminal
    /// problem that does not exist.
    #[test]
    fn a_cancelled_prompt_reads_as_an_abort_not_a_failure() {
        for cancel in [
            inquire::InquireError::OperationCanceled,
            inquire::InquireError::OperationInterrupted,
        ] {
            let msg = format!("{}", map_confirm_error(cancel));
            assert_eq!(msg, "aborted by user");
        }
    }

    /// A real terminal failure keeps its underlying detail — that is the only
    /// clue the operator gets about what actually broke.
    #[test]
    fn a_genuine_prompt_failure_keeps_its_cause() {
        let err = map_confirm_error(inquire::InquireError::InvalidConfiguration(
            "no tty".to_string(),
        ));
        let msg = format!("{err}");
        assert!(msg.contains("confirmation prompt failed"), "{msg}");
        assert!(msg.contains("no tty"), "{msg}");
    }

    // ── aborted_message ──────────────────────────────────────────────────

    /// Declining the confirmation must state that nothing was written; a bare
    /// "aborted" leaves the operator unsure whether the target got half-saved.
    #[test]
    fn declining_says_the_target_was_left_intact() {
        let m = aborted_message("prod-eu");
        assert!(m.contains("prod-eu"), "{m}");
        assert!(m.contains("left intact"), "{m}");
    }

    // ── machine_core (against a recording fake env) ──────────────────────

    /// Records every effect `machine_core` asks for, in order, so tests can
    /// assert on the SEQUENCE (guard → validate → save → report) and not just
    /// on the final state.
    #[derive(Default)]
    struct FakeEnv {
        provisioned: bool,
        config: TargetConfig,
        picker: Option<(Option<String>, Option<String>)>,
        picker_error: bool,
        confirm_answer: bool,
        validate_rejects: bool,
        /// Ordered effect log: "validate:<sku>@<region>", "save:<sku>@<region>",
        /// "confirm", "pick", "report:<line>".
        log: Vec<String>,
    }

    impl FakeEnv {
        fn saved(&self) -> Option<&String> {
            self.log.iter().find(|l| l.starts_with("save:"))
        }
        fn reports(&self) -> Vec<&str> {
            self.log
                .iter()
                .filter_map(|l| l.strip_prefix("report:"))
                .collect()
        }
        fn steps(&self) -> Vec<&str> {
            self.log
                .iter()
                .map(|l| l.split(':').next().unwrap_or_default())
                .collect()
        }
    }

    impl MachineEnv for FakeEnv {
        fn is_provisioned(&mut self) -> bool {
            self.log.push("provisioned?".to_string());
            self.provisioned
        }
        fn config(&mut self) -> TargetConfig {
            self.config.clone()
        }
        fn save(&mut self, config: TargetConfig) -> Result<()> {
            self.log.push(format!(
                "save:{}@{}",
                config.server_type.as_deref().unwrap_or("-"),
                config.region.as_deref().unwrap_or("-")
            ));
            self.config = config;
            Ok(())
        }
        fn validate_sku(&mut self, sku: &str, region: &str) -> Result<()> {
            self.log.push(format!("validate:{sku}@{region}"));
            if self.validate_rejects {
                return Err(CliError::Other(format!("unknown server type `{sku}`")));
            }
            Ok(())
        }
        fn pick_machine(&mut self) -> Result<(Option<String>, Option<String>)> {
            self.log.push("pick".to_string());
            if self.picker_error {
                return Err(CliError::Other("picker exploded".to_string()));
            }
            Ok(self.picker.clone().unwrap_or((None, None)))
        }
        fn confirm(&mut self, prompt: &str) -> Result<bool> {
            self.log.push(format!("confirm:{prompt}"));
            Ok(self.confirm_answer)
        }
        fn report(&mut self, line: &str) {
            self.log.push(format!("report:{line}"));
        }
    }

    fn args(no_ping: bool, server_type: Option<&str>) -> MachineArgs {
        MachineArgs {
            target: None,
            server_type: server_type.map(str::to_string),
            no_ping,
        }
    }

    /// The provisioned guard has to fire BEFORE anything is written. A refusal
    /// that still saved would leave the target claiming a machine type its
    /// live server does not have.
    #[test]
    fn a_provisioned_target_is_refused_without_a_single_write() {
        let mut env = FakeEnv {
            provisioned: true,
            ..FakeEnv::default()
        };
        let err = machine_core(&mut env, "prod-eu", &args(true, Some("cx42")), false)
            .expect_err("a provisioned target must be refused");
        let msg = format!("{err}");
        assert!(msg.contains("prod-eu"), "{msg}");
        assert!(msg.contains("apprafter restore --reprovision"), "{msg}");
        assert_eq!(env.saved(), None, "nothing may be written: {:?}", env.log);
        assert!(env.reports().is_empty(), "{:?}", env.log);
    }

    /// `--no-ping` must reach `save` without ever consulting the catalogue —
    /// that is the entire point of the flag (CI / offline setups).
    #[test]
    fn the_no_ping_path_saves_without_touching_the_catalogue() {
        let mut env = FakeEnv {
            config: TargetConfig {
                region: Some("hel1".to_string()),
                ..TargetConfig::default()
            },
            ..FakeEnv::default()
        };
        machine_core(&mut env, "work", &args(true, Some("cx42")), false).unwrap();
        assert_eq!(env.saved().map(String::as_str), Some("save:cx42@hel1"));
        assert!(
            !env.steps().contains(&"validate"),
            "--no-ping must not validate: {:?}",
            env.log
        );
    }

    /// Validation happens BEFORE the write, and a rejected SKU leaves the
    /// target untouched — otherwise a typo'd SKU would be persisted and only
    /// blow up at the next `apply`.
    #[test]
    fn a_rejected_sku_is_never_written() {
        let mut env = FakeEnv {
            validate_rejects: true,
            config: TargetConfig {
                region: Some("hel1".to_string()),
                server_type: Some("cx22".to_string()),
                ..TargetConfig::default()
            },
            ..FakeEnv::default()
        };
        let err = machine_core(&mut env, "work", &args(false, Some("nope99")), false)
            .expect_err("an unknown SKU must abort");
        assert!(format!("{err}").contains("nope99"), "{err}");
        assert_eq!(env.saved(), None, "{:?}", env.log);
        assert_eq!(
            env.config.server_type.as_deref(),
            Some("cx22"),
            "the previous server type must survive a failed validation"
        );
    }

    /// The catalogue is consulted for the target's OWN region, then the write
    /// lands, then the confirmation prints — in that order.
    #[test]
    fn validation_precedes_the_write_and_uses_the_targets_region() {
        let mut env = FakeEnv {
            config: TargetConfig {
                region: Some("hel1".to_string()),
                ..TargetConfig::default()
            },
            ..FakeEnv::default()
        };
        machine_core(&mut env, "work", &args(false, Some("cx42")), false).unwrap();
        assert_eq!(
            env.steps(),
            vec!["provisioned?", "validate", "save", "report"],
            "{:?}",
            env.log
        );
        assert!(
            env.log.contains(&"validate:cx42@hel1".to_string()),
            "{:?}",
            env.log
        );
    }

    /// A target with no region yet still gets validated — against the default
    /// location rather than an empty string the API would reject.
    #[test]
    fn a_target_without_a_region_validates_against_the_default_region() {
        let mut env = FakeEnv::default();
        machine_core(&mut env, "work", &args(false, Some("cx42")), false).unwrap();
        assert!(
            env.log.contains(&format!("validate:cx42@{DEFAULT_REGION}")),
            "{:?}",
            env.log
        );
    }

    /// The picker writes both fields and reports the region it moved.
    #[test]
    fn the_picker_path_saves_the_picked_pair() {
        let mut env = FakeEnv {
            picker: Some((Some("fsn1".to_string()), Some("cx42".to_string()))),
            ..FakeEnv::default()
        };
        machine_core(&mut env, "work", &args(false, None), true).unwrap();
        assert_eq!(env.saved().map(String::as_str), Some("save:cx42@fsn1"));
        let reported = env.reports().join(" ");
        assert!(
            reported.contains("cx42") && reported.contains("fsn1"),
            "{reported}"
        );
    }

    /// A picker that returns no SKU must abort before writing rather than
    /// saving a default machine the operator never chose.
    #[test]
    fn a_picker_with_no_sku_aborts_before_writing() {
        let mut env = FakeEnv {
            picker: Some((Some("fsn1".to_string()), None)),
            ..FakeEnv::default()
        };
        machine_core(&mut env, "work", &args(false, None), true)
            .expect_err("a SKU-less pick must not be saved");
        assert_eq!(env.saved(), None, "{:?}", env.log);
    }

    /// A refusal from the matrix must not reach any effect at all — in
    /// particular it must not open the picker on a non-TTY.
    #[test]
    fn a_non_interactive_shell_never_reaches_the_picker() {
        let mut env = FakeEnv::default();
        machine_core(&mut env, "work", &args(false, None), false)
            .expect_err("a non-interactive shell must be refused");
        assert_eq!(env.steps(), vec!["provisioned?"], "{:?}", env.log);
    }

    // ── region_change_prompt ─────────────────────────────────────────────

    /// The prompt has to name BOTH regions and warn that the live server does
    /// not move — that consequence is the entire reason to ask.
    #[test]
    fn the_region_change_prompt_warns_the_server_does_not_move() {
        let p = region_change_prompt(Some("nbg1"), "hel1");
        assert!(p.contains("nbg1"), "{p}");
        assert!(p.contains("hel1"), "{p}");
        assert!(p.contains("stays in"), "{p}");

        // No region recorded yet — the placeholder must not render an empty
        // pair of backticks that reads like a corrupted config.
        let unset = region_change_prompt(None, "hel1");
        assert!(unset.contains("(unset)"), "{unset}");
    }
}
