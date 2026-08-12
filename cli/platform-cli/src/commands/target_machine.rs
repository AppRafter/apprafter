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

use cli_core::target::{default_config_root, load_target, save_target, TargetStorePaths};
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

/// Pure helper: returns `true` when the state indicates a server has been
/// provisioned (i.e. `state.hetzner_cloud` is `Some`).
///
/// Used to gate `target machine` so it refuses on a live cluster rather than
/// silently recording a preference that will never take effect without a rebuild.
pub fn is_provisioned(state: &cli_state::State) -> bool {
    state.hetzner_cloud.is_some()
}

/// Run `apprafter target machine`.
pub fn run_machine(args: MachineArgs) -> Result<()> {
    // ── Resolve target name + load ───────────────────────────────────────
    // We need both the TargetStorePaths (for the target store) and the
    // State (for the provisioned-server check). Re-use resolve_state_paths
    // to get the state, but we also need the raw target record to patch it.
    let resolved = resolve_state_paths(args.target.as_deref())?;
    let store = TargetStorePaths::for_root(default_config_root()?);
    let mut target = load_target(&store, &resolved.target_name)?;
    let target_name = &resolved.target_name;

    // ── Provisioned guard (before any write) ─────────────────────────────
    // There is no in-place machine resize. Attempting to change the type on
    // a running cluster would silently record a preference that apply() would
    // shadow with the recorded-fact value. Hard-refuse and guide to the
    // rebuild path instead.
    let state = cli_state::State::load_or_default(&resolved.paths)?;
    if is_provisioned(&state) {
        return Err(CliError::Other(format!(
            "`{target_name}` already runs a provisioned cluster — its machine type cannot be \
             changed in place. To move to a different machine, rebuild from a backup:\n\n    \
             apprafter backup create\n    \
             apprafter restore --reprovision --server-type <sku>\n\n\
             (`target machine` only sets the type on a target that has NOT provisioned yet.)"
        )));
    }

    // ── Branch on --no-ping ──────────────────────────────────────────────
    match (args.no_ping, args.server_type.as_deref()) {
        // (A) --no-ping + --server-type  → patch without validation
        (true, Some(sku)) => {
            target.config.server_type = Some(sku.to_string());
            save_target(&store, &target)?;
            println!(
                "server type set to `{sku}` on target `{target_name}` — NOT validated (--no-ping)"
            );
        }

        // (B) --no-ping alone → loud error (picker needs the API)
        (true, None) => {
            return Err(CliError::Other(
                "`target machine` needs the provider API to show the picker — \
                 drop `--no-ping` or pass `--server-type <sku>`"
                    .to_string(),
            ));
        }

        // (C) --server-type without --no-ping → API-validate then patch
        (false, Some(sku)) => {
            let current_region = target
                .config
                .region
                .as_deref()
                .unwrap_or("nbg1")
                .to_string();
            let token = cli_core::resolve_hetzner_token(None, &store, Some(target_name))?;
            let client = HetznerCloudClient::new(hcloud_base_url(), &token);
            let types = client.list_server_types()?.server_types;
            validate_server_type(&types, sku, &current_region)?;

            target.config.server_type = Some(sku.to_string());
            save_target(&store, &target)?;
            println!(
                "server type set to `{sku}` on target `{target_name}` \
                 (validated against Hetzner Cloud for region `{current_region}`)"
            );
        }

        // (D) Interactive (no --server-type, no --no-ping)
        (false, None) => {
            let stdin_tty = std::io::stdin().is_terminal();
            let stdout_tty = std::io::stdout().is_terminal();
            if !(stdin_tty && stdout_tty) {
                return Err(CliError::Other(
                    "non-interactive shell: pass `--server-type <sku>` to set the machine type \
                     without the interactive picker"
                        .to_string(),
                ));
            }

            // Need the token to drive the Hetzner API.
            let token = cli_core::resolve_hetzner_token(None, &store, Some(target_name))?;

            // Reuse the wizard's machine-matrix step verbatim: fetch
            // catalog → measure latency → pick_machine.
            let (picked_region, picked_sku) = crate::commands::target_wizard::prompt_machine(
                &target.config.provider,
                &token,
                None, // no prefill region — let the user pick
                None, // no prefill sku
                false,
            )?;

            // picked_region and picked_sku are Some(_) because no-ping=false
            // and prompt_machine only returns (None, None) in the no-ping branch.
            let picked_region = picked_region.unwrap_or_else(|| "nbg1".to_string());
            let picked_sku = picked_sku.ok_or_else(|| {
                CliError::Other(
                    "machine picker did not return a server type — please retry".to_string(),
                )
            })?;

            // Region-change confirm: only when a server is already provisioned
            // AND the picked region differs from the target's current region.
            // Note: the provisioned guard above already refused when a server
            // exists, so `server_provisioned` is always false here in practice.
            // The `needs_region_confirm` call is kept for symmetry / future
            // use if this path ever runs after a destroy with the state wiped.
            let server_provisioned = false; // guard above ensures no live server

            if needs_region_confirm(
                target.config.region.as_deref(),
                &picked_region,
                server_provisioned,
            ) {
                let old_region = target.config.region.as_deref().unwrap_or("(unset)");
                let confirmed = inquire::Confirm::new(&format!(
                    "This also changes the region `{old_region}` → `{picked_region}`; \
                     the running server stays in `{old_region}` until you re-provision. \
                     Continue?"
                ))
                .with_default(false)
                .prompt()
                .map_err(|err| match err {
                    inquire::InquireError::OperationCanceled
                    | inquire::InquireError::OperationInterrupted => {
                        CliError::Other("aborted by user".to_string())
                    }
                    other => CliError::Other(format!("confirmation prompt failed: {other}")),
                })?;
                if !confirmed {
                    println!("aborted; target `{target_name}` left intact");
                    return Ok(());
                }
            }

            target.config.server_type = Some(picked_sku.clone());
            target.config.region = Some(picked_region.clone());
            save_target(&store, &target)?;
            println!(
                "server type set to `{picked_sku}` / region `{picked_region}` \
                 on target `{target_name}`"
            );
        }
    }

    Ok(())
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
}
