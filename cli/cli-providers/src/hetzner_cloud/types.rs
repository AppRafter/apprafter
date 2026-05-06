// SPDX-License-Identifier: FSL-1.1-MIT
//! Wire types for the Hetzner Cloud REST API.
//!
//! Covers the subset needed by server CRUD and SSH-key CRUD.
//! Network/firewall/floating-IP types arrive in the follow-up
//! cycles for plan.md phase 1.2.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServerStatus {
    Running,
    Initializing,
    Starting,
    Stopping,
    Off,
    Deleting,
    Migrating,
    Rebuilding,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Server {
    pub id: u64,
    pub name: String,
    pub status: ServerStatus,
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ServerListResponse {
    pub servers: Vec<Server>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ServerCreateRequest {
    pub name: String,
    pub server_type: String,
    pub image: String,
    pub location: String,
    pub labels: BTreeMap<String, String>,
    pub start_after_create: bool,
    /// Hetzner SSH-key ids to attach.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ssh_keys: Option<Vec<u64>>,
    /// Hetzner Network ids to attach the server to. The IP is
    /// auto-allocated from the network's subnet.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub networks: Option<Vec<u64>>,
    /// Hetzner Firewall references to apply to the server.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub firewalls: Option<Vec<FirewallReference>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ServerCreateResponse {
    pub server: Server,
    /// Hetzner returns a one-time root password unless SSH keys
    /// are passed at creation time.
    #[serde(default)]
    pub root_password: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SshKey {
    pub id: u64,
    pub name: String,
    pub public_key: String,
    pub fingerprint: String,
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SshKeyListResponse {
    pub ssh_keys: Vec<SshKey>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SshKeyCreateRequest {
    pub name: String,
    pub public_key: String,
    pub labels: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SshKeyCreateResponse {
    pub ssh_key: SshKey,
}

/// Body of an error response from the Hetzner API.
#[derive(Debug, Clone, Deserialize)]
pub struct ApiErrorEnvelope {
    pub error: ApiErrorDetails,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ApiErrorDetails {
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Subnet {
    /// Hetzner subnet kind. Always "cloud" for cloud servers.
    #[serde(rename = "type")]
    pub kind: String,
    pub ip_range: String,
    pub network_zone: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Network {
    pub id: u64,
    pub name: String,
    pub ip_range: String,
    #[serde(default)]
    pub subnets: Vec<Subnet>,
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct NetworkListResponse {
    pub networks: Vec<Network>,
}

#[derive(Debug, Clone, Serialize)]
pub struct NetworkCreateRequest {
    pub name: String,
    pub ip_range: String,
    pub subnets: Vec<Subnet>,
    pub labels: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct NetworkCreateResponse {
    pub network: Network,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FirewallRule {
    pub direction: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<String>,
    pub protocol: String,
    #[serde(default)]
    pub source_ips: Vec<String>,
    #[serde(default)]
    pub destination_ips: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Firewall {
    pub id: u64,
    pub name: String,
    #[serde(default)]
    pub rules: Vec<FirewallRule>,
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FirewallListResponse {
    pub firewalls: Vec<Firewall>,
}

#[derive(Debug, Clone, Serialize)]
pub struct FirewallCreateRequest {
    pub name: String,
    pub rules: Vec<FirewallRule>,
    pub labels: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FirewallCreateResponse {
    pub firewall: Firewall,
}

/// Reference shape used in `ServerCreateRequest.firewalls`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FirewallReference {
    pub firewall: u64,
}
