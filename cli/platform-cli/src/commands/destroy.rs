// SPDX-License-Identifier: FSL-1.1-Apache-2.0
use std::collections::BTreeMap;

use cli_core::target::{default_config_root, TargetStorePaths};
use cli_core::{resolve_hetzner_token, CliError, Result};
use cli_providers::hetzner_cloud::{HetznerCloudClient, HetznerCloudProvider, ServerSpec};
use cli_providers::Provider;
use cli_state::{State, StatePaths};
use tracing::info;

use crate::commands::hcloud::hcloud_base_url;

pub fn run(yes: bool, target_override: Option<&str>) -> Result<()> {
    info!(yes, target_override, "destroy invoked");
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

    // Empty-state early exit before we even consult the resolver:
    // an operator running `destroy --yes` in a no-Hetzner-state
    // directory expects "nothing to do" rather than a credentials
    // error.
    if yes && state.hetzner_cloud.is_none() {
        println!("nothing to destroy: state has no Hetzner resources");
        return Ok(());
    }

    // Resolution chain (cli-dx-task.md §7).
    let target_store = TargetStorePaths::for_root(default_config_root()?);
    let token = resolve_hetzner_token(None, &target_store, target_override)?;

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

    // Drop the per-cluster known_hosts file alongside the state.
    // The next `apply` against a fresh server (Hetzner happily
    // recycles public IPs) starts with a clean slate; without this
    // step the file would carry the destroyed cluster's host key
    // and the next `kubeconfig` SSH call would fail with `Host key
    // verification failed` until the operator manually removes the
    // entry. Best-effort: ignore the error if the file is already
    // gone (e.g. kubeconfig was never run for this cluster).
    let kh = paths.known_hosts_file();
    if kh.exists() {
        if let Err(e) = std::fs::remove_file(&kh) {
            info!(path = %kh.display(), error = %e, "failed to remove per-cluster known_hosts");
        }
    }

    Ok(())
}
