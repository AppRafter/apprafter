// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! `apprafter target firewall cloudflare-origin enable|disable` (1.83h) —
//! manifest-free toggle of the Cloudflare origin firewall: persists the intent
//! in the target store AND immediately reconciles the live Hetzner firewall.

use cli_core::style;
use cli_core::target::{load_target, save_target, FirewallConfig};
use cli_core::Result;
use cli_providers::hetzner_cloud::{APPRAFTER_LABEL, APPRAFTER_LABEL_VALUE};
use cli_providers::HetznerCloudClient;
use cli_state::State;

use crate::cli::{FirewallToggle, TargetFirewallCommand};
use crate::commands::firewall_spec::build_firewall_spec;
use crate::commands::hcloud::hcloud_base_url;
use crate::commands::state_paths::resolve_state_paths;

pub fn run(action: TargetFirewallCommand) -> Result<()> {
    match action {
        TargetFirewallCommand::CloudflareOrigin { state } => {
            run_cloudflare_origin(matches!(state, FirewallToggle::Enable))
        }
    }
}

fn run_cloudflare_origin(enable: bool) -> Result<()> {
    let resolved = resolve_state_paths(None)?;
    let store = resolved.store;
    let target_name = resolved.target_name;

    // 1. Persist the toggle FIRST (intent survives even if the live reconcile
    //    can't run / the CF fetch fails).
    let mut target = load_target(&store, &target_name)?;
    target.config.firewall = Some(FirewallConfig {
        cloudflare_origin: enable,
    });
    save_target(&store, &target)?;

    // 2. Resolve cluster + token + state.
    let state = State::load_or_default(&resolved.paths)?;
    let cluster = state
        .cluster_name
        .clone()
        .or_else(|| target.config.cluster_name.clone())
        .unwrap_or_else(|| "platform-1".to_string());
    let token = cli_core::resolve_hetzner_token(None, &store, None)?;
    let client = HetznerCloudClient::new(hcloud_base_url(), token);

    // 3. Find the live firewall (cached id, else list+label+name).
    let fw_name = format!("{cluster}-fw");
    let firewall_id = match state.hetzner_cloud.as_ref().and_then(|h| h.firewall_id) {
        Some(id) => Some(id),
        None => client
            .list_firewalls()?
            .firewalls
            .into_iter()
            .find(|f| {
                f.name == fw_name
                    && f.labels.get(APPRAFTER_LABEL).map(String::as_str)
                        == Some(APPRAFTER_LABEL_VALUE)
            })
            .map(|f| f.id),
    };
    let Some(firewall_id) = firewall_id else {
        eprintln!(
            "{}",
            style::warn(&format!(
                "no firewall found for '{cluster}' — the toggle is saved and will apply on the next \
                 `apprafter up` / `apprafter apply`."
            ))
        );
        return Ok(());
    };

    // 4. Reconcile the live firewall (reuse 1.83d).
    let cf_ips = if enable {
        Some(cli_providers::fetch_cloudflare_ips(
            &cli_providers::UreqCloudflareIpSource,
        )?)
    } else {
        None
    };
    let spec = build_firewall_spec(None, &cluster, cf_ips.as_deref());
    let wire: Vec<_> = spec
        .rules
        .iter()
        .map(cli_providers::rule_spec_to_wire)
        .collect();
    client.set_firewall_rules(firewall_id, &wire)?;

    if enable {
        println!("✓ Cloudflare origin firewall enabled on {fw_name} (80/443 restricted to Cloudflare IP ranges).");
        println!("  Point DNS through Cloudflare (orange-cloud + SSL/TLS Full (strict)) — direct-to-node is now blocked.");
    } else {
        println!("✓ Cloudflare origin firewall disabled on {fw_name} (80/443 open to the internet again).");
    }
    Ok(())
}
