// SPDX-License-Identifier: FSL-1.1-MIT
//! Read live Hetzner Cloud resources labelled `apprafter=true` and
//! rebuild `.apprafter/state.json`. See plan.md phase 1.2 (v0.1.7).

use cli_core::{CliError, Result};
use cli_providers::hetzner_cloud::{HetznerCloudClient, APPRAFTER_LABEL, APPRAFTER_LABEL_VALUE};
use cli_state::{HetznerCloudState, State, StatePaths};
use tracing::info;

use crate::commands::hcloud::hcloud_base_url;

pub fn run(force: bool, dry_run: bool) -> Result<()> {
    info!(force, dry_run, "import invoked");
    let cwd = std::env::current_dir()?;
    let paths = StatePaths::for_root(&cwd);
    let mut state = State::load_or_default(&paths)?;

    let provider_id = state.provider.clone().ok_or_else(|| {
        CliError::Other("state has no provider — run `platform-cli init …` first".to_string())
    })?;
    if provider_id != "hetzner-cloud" {
        return Err(CliError::Other(format!(
            "provider `{provider_id}` is not yet implemented in this skeleton"
        )));
    }

    let token = std::env::var("HCLOUD_TOKEN").map_err(|_| {
        CliError::Other("HCLOUD_TOKEN env var is required for hetzner-cloud import".to_string())
    })?;

    if state.hetzner_cloud.is_some() && !force {
        return Err(CliError::Other(
            "state already contains hetzner_cloud; pass --force to overwrite".to_string(),
        ));
    }

    let cluster = state
        .cluster_name
        .clone()
        .unwrap_or_else(|| "platform-1".into());

    let client = HetznerCloudClient::new(hcloud_base_url(), token);
    let snapshot = match build_snapshot(&client, &cluster)? {
        Some(s) => s,
        None => {
            println!(
                "import: no matching Hetzner resources tagged `{APPRAFTER_LABEL}={APPRAFTER_LABEL_VALUE}` for cluster `{cluster}`"
            );
            return Ok(());
        }
    };

    print_summary(dry_run, &cluster, &snapshot);

    if dry_run {
        return Ok(());
    }

    state.hetzner_cloud = Some(snapshot);
    state.save(&paths)?;
    Ok(())
}

fn build_snapshot(client: &HetznerCloudClient, cluster: &str) -> Result<Option<HetznerCloudState>> {
    let labelled = |labels: &std::collections::BTreeMap<String, String>| {
        labels.get(APPRAFTER_LABEL).map(String::as_str) == Some(APPRAFTER_LABEL_VALUE)
    };

    let servers = client.list_servers()?;
    let Some(server) = servers
        .servers
        .into_iter()
        .find(|s| s.name == cluster && labelled(&s.labels))
    else {
        return Ok(None);
    };

    let ssh_key_ids: Vec<u64> = client
        .list_ssh_keys()?
        .ssh_keys
        .into_iter()
        .filter(|k| labelled(&k.labels))
        .map(|k| k.id)
        .collect();
    let network_id: Option<u64> = client
        .list_networks()?
        .networks
        .into_iter()
        .find(|n| labelled(&n.labels))
        .map(|n| n.id);
    let firewall_id: Option<u64> = client
        .list_firewalls()?
        .firewalls
        .into_iter()
        .find(|f| labelled(&f.labels))
        .map(|f| f.id);
    let floating_ip_ids: Vec<u64> = client
        .list_floating_ips()?
        .floating_ips
        .into_iter()
        .filter(|f| labelled(&f.labels))
        .map(|f| f.id)
        .collect();

    Ok(Some(HetznerCloudState {
        server_id: server.id,
        server_name: server.name,
        ssh_key_ids,
        network_id,
        firewall_id,
        floating_ip_ids,
    }))
}

fn print_summary(dry_run: bool, cluster: &str, s: &HetznerCloudState) {
    let prefix = if dry_run {
        format!("import: would write state for cluster `{cluster}`")
    } else {
        format!("import: wrote state for cluster `{cluster}`")
    };
    println!("{prefix}");
    println!("  server: {} (id {})", s.server_name, s.server_id);
    println!("  ssh keys: {}", s.ssh_key_ids.len());
    match s.network_id {
        Some(id) => println!("  network: {id}"),
        None => println!("  network: none"),
    }
    match s.firewall_id {
        Some(id) => println!("  firewall: {id}"),
        None => println!("  firewall: none"),
    }
    println!("  floating IPs: {}", s.floating_ip_ids.len());
}
