// SPDX-License-Identifier: FSL-1.1-MIT
use std::collections::BTreeMap;

use cli_core::{CliError, Result};
use cli_providers::hetzner_cloud::{
    HetznerCloudClient, HetznerCloudProvider, ServerSpec, SshKeySpec, DEFAULT_BASE_URL,
};
use cli_providers::Provider;
use cli_state::{HetznerCloudState, State, StatePaths};
use tracing::info;

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

    // Optional SSH key from env. If APPRAFTER_SSH_PUBLIC_KEY is
    // present, the server boots attached to that key (no root pwd).
    let ssh_keys = match std::env::var("APPRAFTER_SSH_PUBLIC_KEY") {
        Ok(public_key) => {
            tracing::info!(cluster = %cluster, "configuring SSH key from env");
            vec![SshKeySpec {
                name: format!("{cluster}-key"),
                public_key,
            }]
        }
        Err(_) => Vec::new(),
    };

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
    };

    let outcome = provider.apply()?;
    println!("apply complete: {} action(s)", outcome.applied);

    // Persist the server we just created (or confirmed) so future
    // applies / destroys see it.
    let live_servers = provider.client.list_servers()?;
    let live_keys = provider.client.list_ssh_keys()?;
    if let Some(server) = live_servers.servers.into_iter().find(|s| s.name == cluster) {
        let key_ids: Vec<u64> = live_keys
            .ssh_keys
            .iter()
            .filter(|k| k.labels.get("apprafter").map(String::as_str) == Some("true"))
            .map(|k| k.id)
            .collect();
        state.hetzner_cloud = Some(HetznerCloudState {
            server_id: server.id,
            server_name: server.name,
            ssh_key_ids: key_ids,
            network_id: None,
            firewall_id: None,
        });
        state.save(&paths)?;
    }

    Ok(())
}
