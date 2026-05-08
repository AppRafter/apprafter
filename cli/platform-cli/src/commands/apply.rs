// SPDX-License-Identifier: FSL-1.1-MIT
use std::collections::BTreeMap;
use std::path::Path;

use cli_core::manifest::{self, FirewallIngressRule, InfrastructureManifest};
use cli_core::{CliError, Result};
use cli_providers::hetzner_cloud::{
    build_k3s_user_data, FirewallRuleSpec, FirewallSpec, FloatingIpSpec, HetznerCloudClient,
    HetznerCloudProvider, K3sBootstrapOptions, NetworkSpec, ServerSpec, SshKeySpec,
};
use cli_providers::Provider;
use cli_state::{HetznerCloudState, State, StatePaths};
use tracing::info;

use crate::commands::hcloud::hcloud_base_url;

const DEFAULT_NETWORK_IP_RANGE: &str = "10.0.0.0/16";
const DEFAULT_SUBNET_IP_RANGE: &str = "10.0.0.0/24";
const DEFAULT_NETWORK_ZONE: &str = "eu-central";
const DEFAULT_SERVER_TYPE: &str = "cpx22";
const DEFAULT_OS_IMAGE: &str = "ubuntu-24.04";

const DEFAULT_INGRESS_PORTS_TCP: &[&str] = &["22", "6443", "80", "443"];
const DEFAULT_INGRESS_PORTS_UDP: &[&str] = &["51820"];
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
    let floating_ips = build_floating_ip_specs(manifest.as_ref(), &cluster, &region);

    let provider = HetznerCloudProvider {
        client: HetznerCloudClient::new(hcloud_base_url(), token),
        spec: server_spec,
        ssh_keys,
        networks,
        firewalls,
        floating_ips,
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
        user_data: Some(build_k3s_user_data(&K3sBootstrapOptions::default())),
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
    let sources: Vec<String> = DEFAULT_INGRESS_SOURCES
        .iter()
        .map(|s| s.to_string())
        .collect();
    let mut rules: Vec<FirewallRuleSpec> = DEFAULT_INGRESS_PORTS_TCP
        .iter()
        .map(|p| FirewallRuleSpec {
            direction: "in".into(),
            port: Some((*p).to_string()),
            protocol: "tcp".into(),
            source_ips: sources.clone(),
            destination_ips: vec![],
        })
        .collect();
    rules.extend(DEFAULT_INGRESS_PORTS_UDP.iter().map(|p| FirewallRuleSpec {
        direction: "in".into(),
        port: Some((*p).to_string()),
        protocol: "udp".into(),
        source_ips: sources.clone(),
        destination_ips: vec![],
    }));
    rules
}

fn build_floating_ip_specs(
    manifest: Option<&InfrastructureManifest>,
    cluster: &str,
    region: &str,
) -> Vec<FloatingIpSpec> {
    let names = manifest
        .and_then(|m| m.spec.network.as_ref())
        .and_then(|n| n.floating_ips.as_ref());
    match names {
        Some(list) if !list.is_empty() => list
            .iter()
            .map(|n| FloatingIpSpec {
                name: format!("{cluster}-{n}"),
                kind: "ipv4".into(),
                home_location: region.into(),
            })
            .collect(),
        _ => Vec::new(),
    }
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
    let live_fips = provider.client.list_floating_ips()?;
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
        let fip_ids: Vec<u64> = live_fips
            .floating_ips
            .iter()
            .filter(|f| f.labels.get("apprafter").map(String::as_str) == Some("true"))
            .map(|f| f.id)
            .collect();
        state.hetzner_cloud = Some(HetznerCloudState {
            server_id: server.id,
            server_name: server.name,
            ssh_key_ids: key_ids,
            network_id: net_id,
            firewall_id: fw_id,
            floating_ip_ids: fip_ids,
            kubeconfig_yaml: None,
            kubeconfig_age: None,
            argocd_admin_password_age: None,
        });
        state.save(paths)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn manifest_from(value: serde_json::Value) -> InfrastructureManifest {
        serde_json::from_value(value).expect("valid InfrastructureManifest JSON")
    }

    fn minimal_manifest() -> InfrastructureManifest {
        manifest_from(json!({
            "apiVersion": "apprafter.io/v1alpha1",
            "kind": "Infrastructure",
            "metadata": {"name": "platform-1"},
            "spec": {"provider": "hetzner-cloud"}
        }))
    }

    #[test]
    fn build_server_spec_uses_defaults_when_manifest_is_absent() {
        let s = build_server_spec(None, "platform-1", "nbg1");
        assert_eq!(s.name, "platform-1");
        assert_eq!(s.server_type, DEFAULT_SERVER_TYPE);
        assert_eq!(s.image, DEFAULT_OS_IMAGE);
        assert_eq!(s.location, "nbg1");
        assert!(s.labels.is_empty());
    }

    #[test]
    fn build_server_spec_overrides_from_manifest_first_node_and_os_image() {
        let m = manifest_from(json!({
            "apiVersion": "apprafter.io/v1alpha1",
            "kind": "Infrastructure",
            "metadata": {"name": "platform-1"},
            "spec": {
                "provider": "hetzner-cloud",
                "nodes": [
                    {"role": "control-plane", "type": "cpx21", "count": 1},
                    {"role": "worker", "type": "cx32", "count": 2}
                ],
                "osImage": "debian-12"
            }
        }));
        let s = build_server_spec(Some(&m), "cluster-x", "fsn1");
        // Picks the FIRST node's type, ignores subsequent ones.
        assert_eq!(s.server_type, "cpx21");
        assert_eq!(s.image, "debian-12");
        assert_eq!(s.name, "cluster-x");
        assert_eq!(s.location, "fsn1");
    }

    #[test]
    fn build_ssh_specs_returns_manifest_keys_with_default_names_when_unnamed() {
        let m = manifest_from(json!({
            "apiVersion": "apprafter.io/v1alpha1",
            "kind": "Infrastructure",
            "metadata": {"name": "p"},
            "spec": {
                "provider": "hetzner-cloud",
                "sshKeys": [
                    {"public_key": "ssh-ed25519 AAAA"},
                    {"name": "named", "public_key": "ssh-ed25519 BBBB"}
                ]
            }
        }));
        // Env override must NOT be consulted when manifest declares keys.
        std::env::remove_var("APPRAFTER_SSH_PUBLIC_KEY");
        let specs = build_ssh_specs(Some(&m), "cl");
        assert_eq!(specs.len(), 2);
        assert_eq!(specs[0].name, "cl-key-0");
        assert_eq!(specs[0].public_key, "ssh-ed25519 AAAA");
        assert_eq!(specs[1].name, "named");
    }

    #[test]
    fn build_ssh_specs_returns_empty_when_no_manifest_and_no_env() {
        std::env::remove_var("APPRAFTER_SSH_PUBLIC_KEY");
        assert!(build_ssh_specs(None, "cl").is_empty());
    }

    #[test]
    fn build_network_spec_falls_back_to_defaults_when_block_is_absent() {
        let n = build_network_spec(None, "cl");
        assert_eq!(n.name, "cl-net");
        assert_eq!(n.ip_range, DEFAULT_NETWORK_IP_RANGE);
        assert_eq!(n.subnet_ip_range, DEFAULT_SUBNET_IP_RANGE);
        assert_eq!(n.network_zone, DEFAULT_NETWORK_ZONE);
    }

    #[test]
    fn build_network_spec_overlays_manifest_values() {
        let m = manifest_from(json!({
            "apiVersion": "apprafter.io/v1alpha1",
            "kind": "Infrastructure",
            "metadata": {"name": "p"},
            "spec": {
                "provider": "hetzner-cloud",
                "network": {
                    "ip_range": "192.168.0.0/16",
                    "subnet": {"ip_range": "192.168.10.0/24", "zone": "us-east"}
                }
            }
        }));
        let n = build_network_spec(Some(&m), "p");
        assert_eq!(n.ip_range, "192.168.0.0/16");
        assert_eq!(n.subnet_ip_range, "192.168.10.0/24");
        assert_eq!(n.network_zone, "us-east");
    }

    #[test]
    fn build_firewall_spec_uses_default_ingress_when_absent() {
        let f = build_firewall_spec(None, "cl");
        assert_eq!(f.name, "cl-fw");
        // Defaults whitelist ssh + kube API + HTTP + HTTPS over tcp
        // and wireguard over udp; all rules are inbound from
        // 0.0.0.0/0 + ::/0.
        let ports: Vec<String> = f.rules.iter().filter_map(|r| r.port.clone()).collect();
        for expected in ["22", "6443", "80", "443", "51820"] {
            assert!(
                ports.iter().any(|p| p == expected),
                "missing port {expected} in {ports:?}"
            );
        }
        for r in &f.rules {
            assert_eq!(r.direction, "in");
            assert_eq!(r.source_ips, vec!["0.0.0.0/0", "::/0"]);
        }
    }

    #[test]
    fn build_firewall_spec_uses_manifest_rules_when_present() {
        let m = manifest_from(json!({
            "apiVersion": "apprafter.io/v1alpha1",
            "kind": "Infrastructure",
            "metadata": {"name": "p"},
            "spec": {
                "provider": "hetzner-cloud",
                "firewall": {
                    "ingress": [
                        {"port": "8080", "protocol": "udp", "source_ips": ["10.0.0.0/8"]}
                    ]
                }
            }
        }));
        let f = build_firewall_spec(Some(&m), "p");
        assert_eq!(f.rules.len(), 1);
        assert_eq!(f.rules[0].port.as_deref(), Some("8080"));
        assert_eq!(f.rules[0].protocol, "udp");
        assert_eq!(f.rules[0].source_ips, vec!["10.0.0.0/8"]);
    }

    #[test]
    fn rule_from_manifest_defaults_protocol_to_tcp_and_global_sources() {
        let r = rule_from_manifest(&FirewallIngressRule {
            port: "9000".into(),
            protocol: None,
            source_ips: None,
        });
        assert_eq!(r.protocol, "tcp");
        assert_eq!(r.source_ips, vec!["0.0.0.0/0", "::/0"]);
    }

    #[test]
    fn default_ingress_rules_emits_one_rule_per_default_port() {
        let r = default_ingress_rules();
        assert_eq!(
            r.len(),
            DEFAULT_INGRESS_PORTS_TCP.len() + DEFAULT_INGRESS_PORTS_UDP.len()
        );
    }

    #[test]
    fn default_ingress_rules_now_include_kube_api_and_http_and_wireguard() {
        let r = default_ingress_rules();
        let ports: Vec<String> = r
            .iter()
            .map(|rule| {
                let p = rule.port.clone().unwrap_or_default();
                format!("{p}/{}", rule.protocol)
            })
            .collect();
        assert!(ports.contains(&"22/tcp".into()), "{ports:?}");
        assert!(ports.contains(&"6443/tcp".into()), "{ports:?}");
        assert!(ports.contains(&"80/tcp".into()), "{ports:?}");
        assert!(ports.contains(&"443/tcp".into()), "{ports:?}");
        assert!(ports.contains(&"51820/udp".into()), "{ports:?}");
    }

    #[test]
    fn build_server_spec_attaches_cloud_init_user_data_by_default() {
        let s = build_server_spec(None, "platform-1", "nbg1");
        let yaml = s.user_data.expect("user_data set by default");
        assert!(yaml.starts_with("#cloud-config"), "{yaml}");
        assert!(yaml.contains("get.k3s.io"));
    }

    #[test]
    fn build_floating_ip_specs_is_empty_when_no_floating_ips_declared() {
        assert!(build_floating_ip_specs(None, "cl", "nbg1").is_empty());
        let m = minimal_manifest();
        assert!(build_floating_ip_specs(Some(&m), "cl", "nbg1").is_empty());
    }

    #[test]
    fn build_floating_ip_specs_prefixes_names_with_cluster() {
        let m = manifest_from(json!({
            "apiVersion": "apprafter.io/v1alpha1",
            "kind": "Infrastructure",
            "metadata": {"name": "p"},
            "spec": {
                "provider": "hetzner-cloud",
                "network": {"floatingIPs": ["egress", "ingress"]}
            }
        }));
        let v = build_floating_ip_specs(Some(&m), "cl", "nbg1");
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].name, "cl-egress");
        assert_eq!(v[0].kind, "ipv4");
        assert_eq!(v[0].home_location, "nbg1");
        assert_eq!(v[1].name, "cl-ingress");
    }
}
