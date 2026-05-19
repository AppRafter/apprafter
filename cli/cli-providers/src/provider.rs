// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Trait that every infrastructure provider implements.
//!
//! Built-in providers (Hetzner Cloud, Hetzner Robot, AWS) and
//! community InfrastructureProviderPlugins both speak this trait.

use cli_core::Result;

/// A single declarative change a provider plans to make.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Provision a Hetzner Cloud server.
    CreateServer(String),
    /// Destroy a Hetzner Cloud server by id.
    DestroyServer(u64),
    /// Provision a Hetzner Cloud SSH-key resource by spec name.
    CreateSshKey(String),
    /// Destroy a Hetzner Cloud SSH-key resource by id.
    DestroySshKey(u64),
    /// Provision a Hetzner Cloud network by spec name.
    CreateNetwork(String),
    /// Destroy a Hetzner Cloud network by id.
    DestroyNetwork(u64),
    /// Provision a Hetzner Cloud firewall by spec name.
    CreateFirewall(String),
    /// Destroy a Hetzner Cloud firewall by id.
    DestroyFirewall(u64),
    /// Provision a Hetzner Cloud floating IP by spec name.
    CreateFloatingIp(String),
    /// Destroy a Hetzner Cloud floating IP by id.
    DestroyFloatingIp(u64),
    /// Placeholder for future actions; kept so tests can build
    /// `Plan` instances without depending on a real resource.
    Noop,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Plan {
    pub actions: Vec<Action>,
}

impl Plan {
    pub fn summary(&self) -> String {
        match self.actions.len() {
            0 => "no changes".to_string(),
            1 => "1 action".to_string(),
            n => format!("{n} actions"),
        }
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ApplyOutcome {
    pub applied: usize,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct DestroyOutcome {
    pub destroyed: usize,
}

pub trait Provider {
    fn plan(&self) -> Result<Plan>;
    fn apply(&self) -> Result<ApplyOutcome>;
    fn destroy(&self) -> Result<DestroyOutcome>;
}
