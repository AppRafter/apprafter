// SPDX-License-Identifier: FSL-1.1-MIT
use std::collections::BTreeMap;

use cli_core::{CliError, Result};
use cli_providers::hetzner_cloud::{HetznerCloudClient, HetznerCloudProvider, ServerSpec};
use cli_providers::Provider;
use cli_state::{State, StatePaths};
use tracing::info;

use crate::commands::hcloud::hcloud_base_url;

pub fn run(yes: bool) -> Result<()> {
    info!(yes, "destroy invoked");
    let cwd = std::env::current_dir()?;
    let paths = StatePaths::for_root(&cwd);
    let mut state = State::load_or_default(&paths)?;

    let provider_id = state.provider.clone();
    let Some(provider_id) = provider_id else {
        println!("nothing to destroy: no provider in state");
        return Ok(());
    };

    if provider_id != "hetzner-cloud" {
        return Err(CliError::Other(format!(
            "provider `{provider_id}` is not yet implemented in this skeleton"
        )));
    }

    let Ok(token) = std::env::var("HCLOUD_TOKEN") else {
        if yes && state.hetzner_cloud.is_none() {
            println!("nothing to destroy: state has no Hetzner resources");
            return Ok(());
        }
        return Err(CliError::Other(
            "HCLOUD_TOKEN env var is required for hetzner-cloud destroy".to_string(),
        ));
    };

    if !yes {
        return Err(CliError::Other(
            "refusing to destroy without --yes (this would tear down live infrastructure)"
                .to_string(),
        ));
    }

    let cluster = state
        .cluster_name
        .clone()
        .unwrap_or_else(|| "platform-1".into());
    let region = state.region.clone().unwrap_or_else(|| "nbg1".into());

    let provider = HetznerCloudProvider {
        client: HetznerCloudClient::new(hcloud_base_url(), token),
        spec: ServerSpec {
            name: cluster,
            server_type: "cpx22".into(),
            image: "ubuntu-24.04".into(),
            location: region,
            labels: BTreeMap::new(),
            user_data: None,
        },
        // destroy() reads live state from the API; no spec needed.
        ssh_keys: Vec::new(),
        networks: Vec::new(),
        firewalls: Vec::new(),
        floating_ips: Vec::new(),
    };

    let outcome = provider.destroy()?;
    println!(
        "destroy complete: {} resource(s) removed",
        outcome.destroyed
    );

    state.hetzner_cloud = None;
    state.save(&paths)?;
    Ok(())
}
