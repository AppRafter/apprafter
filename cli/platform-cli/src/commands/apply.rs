// SPDX-License-Identifier: FSL-1.1-MIT
use std::collections::BTreeMap;
use std::path::Path;

use cli_core::manifest::{self, FirewallIngressRule, InfrastructureManifest};
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
const DEFAULT_SERVER_TYPE: &str = "cx22";
const DEFAULT_OS_IMAGE: &str = "ubuntu-24.04";

const DEFAULT_INGRESS_PORTS: &[&str] = &["22", "443"];
const DEFAULT_INGRESS_SOURCES: &[&str] = &["0.0.0.0/0", "::/0"];

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

    // Optional manifest path. If APPRAFTER_MANIFEST is set, parse
    // and overlay onto the v0.1.4 defaults; otherwise keep the
    // hardcoded behaviour.
    let manifest = match std::env::var("APPRAFTER_MANIFEST") {
        Ok(p) => {
            info!(path = %p, "reading Infrastructure manifest");
            Some(manifest::parse_infrastructure(&cwd, Path::new(&p))?)
        }
        Err(_) => None,
    };

    let cluster = state
        .cluster_name
        .clone()
        .unwrap_or_else(|| "platform-1".into());
    let region = manifest
        .as_ref()
        .and_then(|m| m.spec.region.clone())
        .or_else(|| state.region.clone())
        .unwrap_or_else(|| "nbg1".into());

    let server_spec = build_server_spec(manifest.as_ref(), &cluster, &region);
    let ssh_keys = build_ssh_specs(manifest.as_ref(), &cluster);
    let networks = vec![build_network_spec(manifest.as_ref(), &cluster)];
    let firewalls = vec![build_firewall_spec(manifest.as_ref(), &cluster)];

    let provider = HetznerCloudProvider {
        client: HetznerCloudClient::new(DEFAULT_BASE_URL, token),
        spec: server_spec,
        ssh_keys,
        networks,
        firewalls,
    };

    let outcome = provider.apply()?;
    println!("apply complete: {} action(s)", outcome.applied);

    persist_state(&provider, &mut state, &paths, &cluster)?;
    Ok(())
}

fn build_server_spec(
    manifest: Option<&InfrastructureManifest>,
    cluster: &str,
    region: &str,
) -> ServerSpec {
    let server_type = manifest
        .and_then(|m| m.spec.nodes.first())
        .map(|n| n.kind.clone())
        .unwrap_or_else(|| DEFAULT_SERVER_TYPE.into());
    let image = manifest
        .and_then(|m| m.spec.os_image.clone())
        .unwrap_or_else(|| DEFAULT_OS_IMAGE.into());

    ServerSpec {
        name: cluster.into(),
        server_type,
        image,
        location: region.into(),
        labels: BTreeMap::new(),
    }
}

fn build_ssh_specs(manifest: Option<&InfrastructureManifest>, cluster: &str) -> Vec<SshKeySpec> {
    if let Some(blocks) = manifest.and_then(|m| m.spec.ssh_keys.as_ref()) {
        return blocks
            .iter()
            .enumerate()
            .map(|(i, b)| SshKeySpec {
                name: b
                    .name
                    .clone()
                    .unwrap_or_else(|| format!("{cluster}-key-{i}")),
                public_key: b.public_key.clone(),
            })
            .collect();
    }
    match std::env::var("APPRAFTER_SSH_PUBLIC_KEY") {
        Ok(public_key) => {
            info!(cluster = %cluster, "configuring SSH key from env");
            vec![SshKeySpec {
                name: format!("{cluster}-key"),
                public_key,
            }]
        }
        Err(_) => Vec::new(),
    }
}

fn build_network_spec(manifest: Option<&InfrastructureManifest>, cluster: &str) -> NetworkSpec {
    let net = manifest.and_then(|m| m.spec.network.as_ref());
    let ip_range = net
        .and_then(|n| n.ip_range.clone())
        .unwrap_or_else(|| DEFAULT_NETWORK_IP_RANGE.into());
    let subnet = net.and_then(|n| n.subnet.as_ref());
    let subnet_ip_range = subnet
        .and_then(|s| s.ip_range.clone())
        .unwrap_or_else(|| DEFAULT_SUBNET_IP_RANGE.into());
    let network_zone = subnet
        .and_then(|s| s.zone.clone())
        .unwrap_or_else(|| DEFAULT_NETWORK_ZONE.into());
    NetworkSpec {
        name: format!("{cluster}-net"),
        ip_range,
        subnet_ip_range,
        network_zone,
    }
}

fn build_firewall_spec(manifest: Option<&InfrastructureManifest>, cluster: &str) -> FirewallSpec {
    let rules = manifest
        .and_then(|m| m.spec.firewall.as_ref())
        .and_then(|f| f.ingress.as_ref())
        .map(|ingress| ingress.iter().map(rule_from_manifest).collect::<Vec<_>>())
        .unwrap_or_else(default_ingress_rules);
    FirewallSpec {
        name: format!("{cluster}-fw"),
        rules,
    }
}

fn rule_from_manifest(r: &FirewallIngressRule) -> FirewallRuleSpec {
    FirewallRuleSpec {
        direction: "in".into(),
        port: Some(r.port.clone()),
        protocol: r.protocol.clone().unwrap_or_else(|| "tcp".into()),
        source_ips: r.source_ips.clone().unwrap_or_else(|| {
            DEFAULT_INGRESS_SOURCES
                .iter()
                .map(|s| s.to_string())
                .collect()
        }),
        destination_ips: vec![],
    }
}

fn default_ingress_rules() -> Vec<FirewallRuleSpec> {
    DEFAULT_INGRESS_PORTS
        .iter()
        .map(|p| FirewallRuleSpec {
            direction: "in".into(),
            port: Some((*p).to_string()),
            protocol: "tcp".into(),
            source_ips: DEFAULT_INGRESS_SOURCES
                .iter()
                .map(|s| s.to_string())
                .collect(),
            destination_ips: vec![],
        })
        .collect()
}

fn persist_state(
    provider: &HetznerCloudProvider,
    state: &mut State,
    paths: &StatePaths,
    cluster: &str,
) -> Result<()> {
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
        state.save(paths)?;
    }
    Ok(())
}
