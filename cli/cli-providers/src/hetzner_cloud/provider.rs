// SPDX-License-Identifier: FSL-1.1-MIT
//! Implementation of the `Provider` trait against Hetzner Cloud.

use cli_core::Result;
use tracing::info;

use crate::provider::{Action, ApplyOutcome, DestroyOutcome, Plan, Provider};

use super::client::HetznerCloudClient;
use super::server::{ServerSpec, SshKeySpec, APPRAFTER_LABEL, APPRAFTER_LABEL_VALUE};
use super::types::{Server, ServerCreateRequest, SshKey, SshKeyCreateRequest};

#[derive(Debug, Clone)]
pub struct HetznerCloudProvider {
    pub client: HetznerCloudClient,
    pub spec: ServerSpec,
    pub ssh_keys: Vec<SshKeySpec>,
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

    fn create_request(&self, ssh_key_ids: &[u64]) -> ServerCreateRequest {
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
}

impl Provider for HetznerCloudProvider {
    fn plan(&self) -> Result<Plan> {
        let live_keys = self.refresh_ssh_keys()?;
        let live_servers = self.refresh_servers()?;

        let mut actions = Vec::new();
        for key_spec in &self.ssh_keys {
            if !live_keys.iter().any(|k| k.name == key_spec.name) {
                actions.push(Action::CreateSshKey(key_spec.name.clone()));
            }
        }
        if !live_servers.iter().any(|s| s.name == self.spec.name) {
            actions.push(Action::CreateServer(self.spec.name.clone()));
        }
        Ok(Plan { actions })
    }

    fn apply(&self) -> Result<ApplyOutcome> {
        let mut applied = 0;

        // 1) SSH keys first; collect resolved ids (existing or freshly-created).
        let live_keys = self.refresh_ssh_keys()?;
        let mut resolved_ids: Vec<u64> = Vec::new();
        for spec in &self.ssh_keys {
            if let Some(existing) = live_keys.iter().find(|k| k.name == spec.name) {
                resolved_ids.push(existing.id);
            } else {
                info!(ssh_key = %spec.name, "creating Hetzner SSH key");
                let resp = self.client.create_ssh_key(&self.ssh_create_request(spec))?;
                info!(ssh_key = %spec.name, id = resp.ssh_key.id, "ssh key created");
                resolved_ids.push(resp.ssh_key.id);
                applied += 1;
            }
        }

        // 2) Server next.
        let live_servers = self.refresh_servers()?;
        if !live_servers.iter().any(|s| s.name == self.spec.name) {
            info!(server = %self.spec.name, "creating Hetzner server");
            let req = self.create_request(&resolved_ids);
            let resp = self.client.create_server(&req)?;
            info!(server = %self.spec.name, id = resp.server.id, "server created");
            applied += 1;
        }

        Ok(ApplyOutcome { applied })
    }

    fn destroy(&self) -> Result<DestroyOutcome> {
        let mut destroyed = 0;

        // 1) Servers first (they reference SSH keys).
        let live_servers = self.refresh_servers()?;
        for server in live_servers {
            info!(server = %server.name, id = server.id, "destroying Hetzner server");
            self.client.delete_server(server.id)?;
            destroyed += 1;
        }

        // 2) SSH keys after.
        let live_keys = self.refresh_ssh_keys()?;
        for key in live_keys {
            info!(ssh_key = %key.name, id = key.id, "destroying Hetzner SSH key");
            self.client.delete_ssh_key(key.id)?;
            destroyed += 1;
        }

        Ok(DestroyOutcome { destroyed })
    }
}
