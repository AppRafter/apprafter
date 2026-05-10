// SPDX-License-Identifier: FSL-1.1-MIT
//! Implementation of the `Provider` trait against Hetzner Cloud.

use std::collections::BTreeMap;
use std::thread::sleep;
use std::time::{Duration, Instant};

use cli_core::{CliError, Result};
use tracing::info;

use crate::provider::{Action, ApplyOutcome, DestroyOutcome, Plan, Provider};

use super::client::HetznerCloudClient;
use super::server::{
    FirewallRuleSpec, FirewallSpec, FloatingIpSpec, NetworkSpec, ServerSpec, SshKeySpec,
    APPRAFTER_LABEL, APPRAFTER_LABEL_VALUE,
};
use super::server_type::validate_server_type;
use super::types::{
    Firewall, FirewallCreateRequest, FirewallReference, FirewallRule, FloatingIp,
    FloatingIpCreateRequest, Network, NetworkCreateRequest, Server, ServerCreateRequest, SshKey,
    SshKeyCreateRequest, Subnet,
};

#[derive(Debug, Clone)]
pub struct HetznerCloudProvider {
    pub client: HetznerCloudClient,
    pub spec: ServerSpec,
    pub ssh_keys: Vec<SshKeySpec>,
    pub networks: Vec<NetworkSpec>,
    pub firewalls: Vec<FirewallSpec>,
    pub floating_ips: Vec<FloatingIpSpec>,
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

    fn refresh_floating_ips(&self) -> Result<Vec<FloatingIp>> {
        let resp = self.client.list_floating_ips()?;
        Ok(resp
            .floating_ips
            .into_iter()
            .filter(|f| {
                f.labels.get(APPRAFTER_LABEL).map(String::as_str) == Some(APPRAFTER_LABEL_VALUE)
            })
            .collect())
    }

    /// Poll `GET /servers` until `id` is no longer in the response.
    /// Hetzner's `DELETE /servers/{id}` is asynchronous; this
    /// function gives `destroy()` a synchronous boundary so the
    /// follow-up `delete_network` doesn't 409 on a still-attached
    /// network interface. Exponential-ish back-off (500ms → 5s
    /// cap), 60s deadline. Returns Err only if the server is
    /// still listed after the deadline.
    fn wait_for_server_gone(&self, id: u64) -> Result<()> {
        let deadline = Instant::now() + Duration::from_secs(60);
        let mut delay = Duration::from_millis(500);
        loop {
            let resp = self.client.list_servers()?;
            if !resp.servers.iter().any(|s| s.id == id) {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(CliError::Other(format!(
                    "server {id} still listed by Hetzner 60s after delete; \
                     async-cleanup unusually slow — re-run `destroy --yes` \
                     in a minute, or check the Hetzner Cloud Console."
                )));
            }
            sleep(delay);
            delay = (delay * 2).min(Duration::from_secs(5));
        }
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
            user_data: self.spec.user_data.clone(),
        }
    }

    fn ssh_create_request(&self, spec: &SshKeySpec) -> SshKeyCreateRequest {
        let mut labels = BTreeMap::new();
        labels.insert(APPRAFTER_LABEL.into(), APPRAFTER_LABEL_VALUE.into());
        SshKeyCreateRequest {
            name: spec.name.clone(),
            public_key: spec.public_key.clone(),
            labels,
        }
    }

    fn network_create_request(&self, spec: &NetworkSpec) -> NetworkCreateRequest {
        let mut labels = BTreeMap::new();
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
        let mut labels = BTreeMap::new();
        labels.insert(APPRAFTER_LABEL.into(), APPRAFTER_LABEL_VALUE.into());
        FirewallCreateRequest {
            name: spec.name.clone(),
            rules: spec.rules.iter().map(rule_spec_to_wire).collect(),
            labels,
        }
    }

    fn floating_ip_create_request(
        &self,
        spec: &FloatingIpSpec,
        server_id: u64,
    ) -> FloatingIpCreateRequest {
        let mut labels = BTreeMap::new();
        labels.insert(APPRAFTER_LABEL.into(), APPRAFTER_LABEL_VALUE.into());
        FloatingIpCreateRequest {
            kind: spec.kind.clone(),
            home_location: spec.home_location.clone(),
            server: Some(server_id),
            name: Some(spec.name.clone()),
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
        let live_fips = self.refresh_floating_ips()?;

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
        for s in &self.floating_ips {
            if !live_fips
                .iter()
                .any(|f| f.name.as_deref() == Some(s.name.as_str()))
            {
                actions.push(Action::CreateFloatingIp(s.name.clone()));
            }
        }
        Ok(Plan { actions })
    }

    fn apply(&self) -> Result<ApplyOutcome> {
        let mut applied = 0;

        // 0) Pre-flight: if we're about to POST a new server,
        // validate the type/region combo against the live Hetzner
        // catalog FIRST, before any other CREATE. A retired type
        // here used to fail mid-apply at step 4, leaking SSH-key +
        // network + firewall state. The list_server_types round-
        // trip is ONLY paid on the create path — no-op applies
        // (server already exists with our label) skip it.
        let live_servers = self.refresh_servers()?;
        let needs_server_create =
            !self.spec.name.is_empty() && !live_servers.iter().any(|s| s.name == self.spec.name);
        if needs_server_create {
            info!(
                server = %self.spec.name,
                server_type = %self.spec.server_type,
                location = %self.spec.location,
                "validating server_type pre-flight"
            );
            let types = self.client.list_server_types()?;
            validate_server_type(
                &types.server_types,
                &self.spec.server_type,
                &self.spec.location,
            )?;
        }

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

        // 4) Server (resolve id even if it already exists, so
        // floating-IP attach has somewhere to point).
        let mut server_id: Option<u64> = None;
        if let Some(existing) = live_servers.iter().find(|s| s.name == self.spec.name) {
            server_id = Some(existing.id);
        } else if !self.spec.name.is_empty() {
            info!(server = %self.spec.name, "creating Hetzner server");
            let req = self.create_request(&ssh_ids, &net_ids, &fw_ids);
            let resp = self.client.create_server(&req)?;
            info!(server = %self.spec.name, id = resp.server.id, "server created");
            server_id = Some(resp.server.id);
            applied += 1;
        }

        // 5) Floating IPs (need server id; skipped entirely when
        // no specs are declared so we don't make a needless list
        // call against the API).
        if !self.floating_ips.is_empty() {
            if let Some(sid) = server_id {
                let live_fips = self.refresh_floating_ips()?;
                for spec in &self.floating_ips {
                    if !live_fips
                        .iter()
                        .any(|f| f.name.as_deref() == Some(spec.name.as_str()))
                    {
                        info!(
                            floating_ip = %spec.name,
                            server = sid,
                            "creating Hetzner floating IP"
                        );
                        let req = self.floating_ip_create_request(spec, sid);
                        let resp = self.client.create_floating_ip(&req)?;
                        info!(
                            floating_ip = %spec.name,
                            id = resp.floating_ip.id,
                            "floating IP created"
                        );
                        applied += 1;
                    }
                }
            }
        }

        Ok(ApplyOutcome { applied })
    }

    fn destroy(&self) -> Result<DestroyOutcome> {
        let mut destroyed = 0;

        // 1) Floating IPs first (they reference the server).
        for fip in self.refresh_floating_ips()? {
            info!(
                floating_ip = ?fip.name,
                id = fip.id,
                "destroying Hetzner floating IP"
            );
            self.client.delete_floating_ip(fip.id)?;
            destroyed += 1;
        }

        // 2) Servers.
        let mut deleted_server_ids: Vec<u64> = Vec::new();
        for server in self.refresh_servers()? {
            info!(server = %server.name, id = server.id, "destroying Hetzner server");
            self.client.delete_server(server.id)?;
            deleted_server_ids.push(server.id);
            destroyed += 1;
        }

        // 2b) Wait for each deleted server to actually be gone.
        // Hetzner's `DELETE /servers/{id}` returns 200 immediately
        // and processes the cleanup asynchronously; meanwhile the
        // server's network interface still holds the network
        // attachment, so the next step's `delete_network` returns
        // 409 "still in use". Polling `GET /servers` until the id
        // is no longer listed is the cleanest sync point — single
        // place that knows about the async semantics, no per-call
        // retry sprinkled across delete_firewall / delete_network.
        for id in deleted_server_ids {
            self.wait_for_server_gone(id)?;
        }

        // 3) Firewalls.
        for fw in self.refresh_firewalls()? {
            info!(firewall = %fw.name, id = fw.id, "destroying Hetzner firewall");
            self.client.delete_firewall(fw.id)?;
            destroyed += 1;
        }

        // 4) Networks.
        for net in self.refresh_networks()? {
            info!(network = %net.name, id = net.id, "destroying Hetzner network");
            self.client.delete_network(net.id)?;
            destroyed += 1;
        }

        // 5) SSH keys.
        for key in self.refresh_ssh_keys()? {
            info!(ssh_key = %key.name, id = key.id, "destroying Hetzner SSH key");
            self.client.delete_ssh_key(key.id)?;
            destroyed += 1;
        }

        Ok(DestroyOutcome { destroyed })
    }
}
