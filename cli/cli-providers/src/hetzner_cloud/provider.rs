// SPDX-License-Identifier: FSL-1.1-MIT
//! Implementation of the `Provider` trait against Hetzner Cloud.

use cli_core::Result;
use tracing::info;

use crate::provider::{Action, ApplyOutcome, DestroyOutcome, Plan, Provider};

use super::client::HetznerCloudClient;
use super::server::{ServerSpec, APPRAFTER_LABEL, APPRAFTER_LABEL_VALUE};
use super::types::{Server, ServerCreateRequest};

#[derive(Debug, Clone)]
pub struct HetznerCloudProvider {
    pub client: HetznerCloudClient,
    pub spec: ServerSpec,
}

impl HetznerCloudProvider {
    /// Return the AppRafter-managed servers from Hetzner.
    fn refresh(&self) -> Result<Vec<Server>> {
        let resp = self.client.list_servers()?;
        let ours: Vec<Server> = resp
            .servers
            .into_iter()
            .filter(|s| {
                s.labels.get(APPRAFTER_LABEL).map(String::as_str) == Some(APPRAFTER_LABEL_VALUE)
            })
            .collect();
        Ok(ours)
    }

    fn create_request(&self) -> ServerCreateRequest {
        let mut labels = self.spec.labels.clone();
        labels.insert(APPRAFTER_LABEL.into(), APPRAFTER_LABEL_VALUE.into());
        ServerCreateRequest {
            name: self.spec.name.clone(),
            server_type: self.spec.server_type.clone(),
            image: self.spec.image.clone(),
            location: self.spec.location.clone(),
            labels,
            start_after_create: true,
        }
    }
}

impl Provider for HetznerCloudProvider {
    fn plan(&self) -> Result<Plan> {
        let live = self.refresh()?;
        let actions = if live.iter().any(|s| s.name == self.spec.name) {
            vec![]
        } else {
            vec![Action::CreateServer(self.spec.name.clone())]
        };
        Ok(Plan { actions })
    }

    fn apply(&self) -> Result<ApplyOutcome> {
        let plan = self.plan()?;
        let mut applied = 0;
        for action in &plan.actions {
            match action {
                Action::CreateServer(name) => {
                    info!(server = %name, "creating Hetzner server");
                    let req = self.create_request();
                    let resp = self.client.create_server(&req)?;
                    info!(server = %name, id = resp.server.id, "server created");
                    applied += 1;
                }
                Action::DestroyServer(_) | Action::Noop => {
                    // Plan() never emits these for apply.
                }
            }
        }
        Ok(ApplyOutcome { applied })
    }

    fn destroy(&self) -> Result<DestroyOutcome> {
        let live = self.refresh()?;
        let mut destroyed = 0;
        for server in live {
            info!(server = %server.name, id = server.id, "destroying Hetzner server");
            self.client.delete_server(server.id)?;
            destroyed += 1;
        }
        Ok(DestroyOutcome { destroyed })
    }
}
