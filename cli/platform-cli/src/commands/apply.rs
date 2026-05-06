// SPDX-License-Identifier: FSL-1.1-MIT
use std::collections::BTreeMap;

use cli_core::{CliError, Result};
use cli_providers::hetzner_cloud::{
    FirewallRuleSpec, FirewallSpec, HetznerCloudClient, HetznerCloudProvider, NetworkSpec,
    ServerSpec, SshKeySpec, DEFAULT_BASE_URL,
};
use cli_providers::Provider;
use cli_state::{HetznerCloudState, State, StatePaths};
use tracing::info;

const DEFAULT_NETWORK_IP_RANGE: &str = "10.0.0.0/16";
const DEFAULT_SUBNET_IP_RANGE: &str = "10.0.0.0/24";
const DEFAULT_NETWORK_ZONE: &str = "eu-central";

pub fn run() -> Result<()> {
    info!("apply invoked");
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
        CliError::Other("HCLOUD_TOKEN env var is required for hetzner-cloud apply".to_string())
    })?;

    let cluster = state
        .cluster_name
        .clone()
        .unwrap_or_else(|| "platform-1".into());
    let region = state.region.clone().unwrap_or_else(|| "nbg1".into());

    let ssh_keys = match std::env::var("APPRAFTER_SSH_PUBLIC_KEY") {
        Ok(public_key) => {
            info!(cluster = %cluster, "configuring SSH key from env");
            vec![SshKeySpec {
                name: format!("{cluster}-key"),
                public_key,
            }]
        }
        Err(_) => Vec::new(),
    };

    let networks = vec![NetworkSpec {
        name: format!("{cluster}-net"),
        ip_range: DEFAULT_NETWORK_IP_RANGE.into(),
        subnet_ip_range: DEFAULT_SUBNET_IP_RANGE.into(),
        network_zone: DEFAULT_NETWORK_ZONE.into(),
    }];

    let firewalls = vec![FirewallSpec {
        name: format!("{cluster}-fw"),
        rules: vec![
            FirewallRuleSpec {
                direction: "in".into(),
                port: Some("22".into()),
                protocol: "tcp".into(),
                source_ips: vec!["0.0.0.0/0".into(), "::/0".into()],
                destination_ips: vec![],
            },
            FirewallRuleSpec {
                direction: "in".into(),
                port: Some("443".into()),
                protocol: "tcp".into(),
                source_ips: vec!["0.0.0.0/0".into(), "::/0".into()],
                destination_ips: vec![],
            },
        ],
    }];

    let provider = HetznerCloudProvider {
        client: HetznerCloudClient::new(DEFAULT_BASE_URL, token),
        spec: ServerSpec {
            name: cluster.clone(),
            server_type: "cx22".into(),
            image: "ubuntu-24.04".into(),
            location: region,
            labels: BTreeMap::new(),
        },
        ssh_keys,
        networks,
        firewalls,
    };

    let outcome = provider.apply()?;
    println!("apply complete: {} action(s)", outcome.applied);

    // Persist all ids for diagnostics / later destroy.
    let live_servers = provider.client.list_servers()?;
    let live_keys = provider.client.list_ssh_keys()?;
    let live_nets = provider.client.list_networks()?;
    let live_fws = provider.client.list_firewalls()?;
    if let Some(server) = live_servers.servers.into_iter().find(|s| s.name == cluster) {
        let key_ids: Vec<u64> = live_keys
            .ssh_keys
            .iter()
            .filter(|k| k.labels.get("apprafter").map(String::as_str) == Some("true"))
            .map(|k| k.id)
            .collect();
        let net_id: Option<u64> = live_nets
            .networks
            .iter()
            .find(|n| n.labels.get("apprafter").map(String::as_str) == Some("true"))
            .map(|n| n.id);
        let fw_id: Option<u64> = live_fws
            .firewalls
            .iter()
            .find(|f| f.labels.get("apprafter").map(String::as_str) == Some("true"))
            .map(|f| f.id);
        state.hetzner_cloud = Some(HetznerCloudState {
            server_id: server.id,
            server_name: server.name,
            ssh_key_ids: key_ids,
            network_id: net_id,
            firewall_id: fw_id,
        });
        state.save(&paths)?;
    }

    Ok(())
}
