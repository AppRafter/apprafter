// SPDX-License-Identifier: FSL-1.1-MIT
//! Implementation of the `Provider` trait against Hetzner Cloud.

use cli_core::Result;
use tracing::info;

use crate::provider::{Action, ApplyOutcome, DestroyOutcome, Plan, Provider};

use super::client::HetznerCloudClient;
use super::server::{
    FirewallRuleSpec, FirewallSpec, NetworkSpec, ServerSpec, SshKeySpec, APPRAFTER_LABEL,
    APPRAFTER_LABEL_VALUE,
};
use super::types::{
    Firewall, FirewallCreateRequest, FirewallReference, FirewallRule, Network,
    NetworkCreateRequest, Server, ServerCreateRequest, SshKey, SshKeyCreateRequest, Subnet,
};

#[derive(Debug, Clone)]
pub struct HetznerCloudProvider {
    pub client: HetznerCloudClient,
    pub spec: ServerSpec,
    pub ssh_keys: Vec<SshKeySpec>,
    pub networks: Vec<NetworkSpec>,
    pub firewalls: Vec<FirewallSpec>,
}

impl HetznerCloudProvider {
    fn refresh_servers(&self) -> Result<Vec<Server>> {
        let resp = self.client.list_servers()?;
        Ok(resp
            .servers
            .into_iter()
            .filter(|s| {
                s.labels.get(APPRAFTER_LABEL).map(String::as_str) == Some(APPRAFTER_LABEL_VALUE)
            })
            .collect())
    }

    fn refresh_ssh_keys(&self) -> Result<Vec<SshKey>> {
        let resp = self.client.list_ssh_keys()?;
        Ok(resp
            .ssh_keys
            .into_iter()
            .filter(|k| {
                k.labels.get(APPRAFTER_LABEL).map(String::as_str) == Some(APPRAFTER_LABEL_VALUE)
            })
            .collect())
    }

    fn refresh_networks(&self) -> Result<Vec<Network>> {
        let resp = self.client.list_networks()?;
        Ok(resp
            .networks
            .into_iter()
            .filter(|n| {
                n.labels.get(APPRAFTER_LABEL).map(String::as_str) == Some(APPRAFTER_LABEL_VALUE)
            })
            .collect())
    }

    fn refresh_firewalls(&self) -> Result<Vec<Firewall>> {
        let resp = self.client.list_firewalls()?;
        Ok(resp
            .firewalls
            .into_iter()
            .filter(|f| {
                f.labels.get(APPRAFTER_LABEL).map(String::as_str) == Some(APPRAFTER_LABEL_VALUE)
            })
            .collect())
    }

    fn create_request(
        &self,
        ssh_key_ids: &[u64],
        network_ids: &[u64],
        firewall_ids: &[u64],
    ) -> ServerCreateRequest {
        let mut labels = self.spec.labels.clone();
        labels.insert(APPRAFTER_LABEL.into(), APPRAFTER_LABEL_VALUE.into());
        ServerCreateRequest {
            name: self.spec.name.clone(),
            server_type: self.spec.server_type.clone(),
            image: self.spec.image.clone(),
            location: self.spec.location.clone(),
            labels,
            start_after_create: true,
            ssh_keys: if ssh_key_ids.is_empty() {
                None
            } else {
                Some(ssh_key_ids.to_vec())
            },
            networks: if network_ids.is_empty() {
                None
            } else {
                Some(network_ids.to_vec())
            },
            firewalls: if firewall_ids.is_empty() {
                None
            } else {
                Some(
                    firewall_ids
                        .iter()
                        .copied()
                        .map(|firewall| FirewallReference { firewall })
                        .collect(),
                )
            },
        }
    }

    fn ssh_create_request(&self, spec: &SshKeySpec) -> SshKeyCreateRequest {
        let mut labels = std::collections::BTreeMap::new();
        labels.insert(APPRAFTER_LABEL.into(), APPRAFTER_LABEL_VALUE.into());
        SshKeyCreateRequest {
            name: spec.name.clone(),
            public_key: spec.public_key.clone(),
            labels,
        }
    }

    fn network_create_request(&self, spec: &NetworkSpec) -> NetworkCreateRequest {
        let mut labels = std::collections::BTreeMap::new();
        labels.insert(APPRAFTER_LABEL.into(), APPRAFTER_LABEL_VALUE.into());
        NetworkCreateRequest {
            name: spec.name.clone(),
            ip_range: spec.ip_range.clone(),
            subnets: vec![Subnet {
                kind: "cloud".to_string(),
                ip_range: spec.subnet_ip_range.clone(),
                network_zone: spec.network_zone.clone(),
            }],
            labels,
        }
    }

    fn firewall_create_request(&self, spec: &FirewallSpec) -> FirewallCreateRequest {
        let mut labels = std::collections::BTreeMap::new();
        labels.insert(APPRAFTER_LABEL.into(), APPRAFTER_LABEL_VALUE.into());
        FirewallCreateRequest {
            name: spec.name.clone(),
            rules: spec.rules.iter().map(rule_spec_to_wire).collect(),
            labels,
        }
    }
}

fn rule_spec_to_wire(spec: &FirewallRuleSpec) -> FirewallRule {
    FirewallRule {
        direction: spec.direction.clone(),
        port: spec.port.clone(),
        protocol: spec.protocol.clone(),
        source_ips: spec.source_ips.clone(),
        destination_ips: spec.destination_ips.clone(),
        description: None,
    }
}

impl Provider for HetznerCloudProvider {
    fn plan(&self) -> Result<Plan> {
        let live_keys = self.refresh_ssh_keys()?;
        let live_networks = self.refresh_networks()?;
        let live_firewalls = self.refresh_firewalls()?;
        let live_servers = self.refresh_servers()?;

        let mut actions = Vec::new();
        for s in &self.ssh_keys {
            if !live_keys.iter().any(|k| k.name == s.name) {
                actions.push(Action::CreateSshKey(s.name.clone()));
            }
        }
        for s in &self.networks {
            if !live_networks.iter().any(|n| n.name == s.name) {
                actions.push(Action::CreateNetwork(s.name.clone()));
            }
        }
        for s in &self.firewalls {
            if !live_firewalls.iter().any(|f| f.name == s.name) {
                actions.push(Action::CreateFirewall(s.name.clone()));
            }
        }
        if !live_servers.iter().any(|s| s.name == self.spec.name) {
            actions.push(Action::CreateServer(self.spec.name.clone()));
        }
        Ok(Plan { actions })
    }

    fn apply(&self) -> Result<ApplyOutcome> {
        let mut applied = 0;

        // 1) SSH keys.
        let live_keys = self.refresh_ssh_keys()?;
        let mut ssh_ids: Vec<u64> = Vec::new();
        for spec in &self.ssh_keys {
            if let Some(existing) = live_keys.iter().find(|k| k.name == spec.name) {
                ssh_ids.push(existing.id);
            } else {
                info!(ssh_key = %spec.name, "creating Hetzner SSH key");
                let resp = self.client.create_ssh_key(&self.ssh_create_request(spec))?;
                ssh_ids.push(resp.ssh_key.id);
                applied += 1;
            }
        }

        // 2) Networks.
        let live_nets = self.refresh_networks()?;
        let mut net_ids: Vec<u64> = Vec::new();
        for spec in &self.networks {
            if let Some(existing) = live_nets.iter().find(|n| n.name == spec.name) {
                net_ids.push(existing.id);
            } else {
                info!(network = %spec.name, "creating Hetzner network");
                let resp = self
                    .client
                    .create_network(&self.network_create_request(spec))?;
                net_ids.push(resp.network.id);
                applied += 1;
            }
        }

        // 3) Firewalls.
        let live_fws = self.refresh_firewalls()?;
        let mut fw_ids: Vec<u64> = Vec::new();
        for spec in &self.firewalls {
            if let Some(existing) = live_fws.iter().find(|f| f.name == spec.name) {
                fw_ids.push(existing.id);
            } else {
                info!(firewall = %spec.name, "creating Hetzner firewall");
                let resp = self
                    .client
                    .create_firewall(&self.firewall_create_request(spec))?;
                fw_ids.push(resp.firewall.id);
                applied += 1;
            }
        }

        // 4) Server.
        let live_servers = self.refresh_servers()?;
        if !live_servers.iter().any(|s| s.name == self.spec.name) {
            info!(server = %self.spec.name, "creating Hetzner server");
            let req = self.create_request(&ssh_ids, &net_ids, &fw_ids);
            let resp = self.client.create_server(&req)?;
            info!(server = %self.spec.name, id = resp.server.id, "server created");
            applied += 1;
        }

        Ok(ApplyOutcome { applied })
    }

    fn destroy(&self) -> Result<DestroyOutcome> {
        let mut destroyed = 0;

        // 1) Servers (they reference firewalls and networks).
        for server in self.refresh_servers()? {
            info!(server = %server.name, id = server.id, "destroying Hetzner server");
            self.client.delete_server(server.id)?;
            destroyed += 1;
        }

        // 2) Firewalls.
        for fw in self.refresh_firewalls()? {
            info!(firewall = %fw.name, id = fw.id, "destroying Hetzner firewall");
            self.client.delete_firewall(fw.id)?;
            destroyed += 1;
        }

        // 3) Networks.
        for net in self.refresh_networks()? {
            info!(network = %net.name, id = net.id, "destroying Hetzner network");
            self.client.delete_network(net.id)?;
            destroyed += 1;
        }

        // 4) SSH keys.
        for key in self.refresh_ssh_keys()? {
            info!(ssh_key = %key.name, id = key.id, "destroying Hetzner SSH key");
            self.client.delete_ssh_key(key.id)?;
            destroyed += 1;
        }

        Ok(DestroyOutcome { destroyed })
    }
}
