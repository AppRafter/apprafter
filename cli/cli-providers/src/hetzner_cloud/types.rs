// SPDX-License-Identifier: FSL-1.1-Apache-2.0
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
pub struct PublicIpv4 {
    pub ip: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublicIpv6 {
    /// Hetzner returns the /64 delegation in CIDR form, e.g.
    /// `2a01:4f8:c0c:abcd::/64`. The first usable address is
    /// `<prefix>::1` by default; k3s + Cilium assign pod addresses
    /// from the rest of the /64.
    pub ip: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublicNet {
    #[serde(default)]
    pub ipv4: Option<PublicIpv4>,
    /// Hetzner delegates one /64 IPv6 prefix per server free of
    /// charge. Always present on cloud servers in dual-stack mode;
    /// `Option` for forward-compat if Hetzner ever ships an
    /// IPv4-only server type (none today).
    #[serde(default)]
    pub ipv6: Option<PublicIpv6>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerTypeRef {
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocationRef {
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Server {
    pub id: u64,
    pub name: String,
    pub status: ServerStatus,
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
    #[serde(default)]
    pub public_net: Option<PublicNet>,
    #[serde(default)]
    pub server_type: Option<ServerTypeRef>,
    #[serde(default)]
    pub location: Option<LocationRef>,
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
    /// cloud-init `#cloud-config` YAML the server is provisioned
    /// with. Consumed by Hetzner verbatim and never echoed back.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_data: Option<String>,
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

/// Body for `POST /firewalls/{id}/actions/set_rules` — replaces the entire
/// rule set atomically. 1.83d.
#[derive(Debug, Clone, Serialize)]
pub struct SetFirewallRulesRequest {
    pub rules: Vec<FirewallRule>,
}

/// Reference shape used in `ServerCreateRequest.firewalls`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FirewallReference {
    pub firewall: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HomeLocation {
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FloatingIp {
    pub id: u64,
    /// Hetzner uses "type" for the address family; renamed to
    /// `kind` so it doesn't collide with the Rust keyword.
    #[serde(rename = "type")]
    pub kind: String,
    pub ip: String,
    #[serde(default)]
    pub name: Option<String>,
    /// Currently-assigned server id (None when unassigned).
    #[serde(default)]
    pub server: Option<u64>,
    pub home_location: HomeLocation,
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FloatingIpListResponse {
    pub floating_ips: Vec<FloatingIp>,
}

#[derive(Debug, Clone, Serialize)]
pub struct FloatingIpCreateRequest {
    /// "ipv4" or "ipv6". `type` is a Rust keyword; serialised
    /// as the JSON key `type`.
    #[serde(rename = "type")]
    pub kind: String,
    /// Plain location name in the request (e.g. "nbg1"); the
    /// list/get response wraps it in a `HomeLocation` object.
    pub home_location: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub labels: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FloatingIpCreateResponse {
    pub floating_ip: FloatingIp,
}

#[derive(Debug, Clone, Deserialize)]
pub struct HetznerPrice {
    pub net: String,
    pub gross: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ServerTypePrice {
    pub location: String,
    pub price_monthly: HetznerPrice,
    pub price_hourly: HetznerPrice,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ServerType {
    pub id: u64,
    pub name: String,
    pub architecture: String,
    pub cpu_type: String,
    pub cores: u32,
    pub memory: f64,
    pub disk: u32,
    /// `null` when the type is alive; an object with announced /
    /// unavailable_after timestamps when it has been retired.
    #[serde(default)]
    pub deprecation: Option<Deprecation>,
    #[serde(default)]
    pub locations: Vec<ServerTypeLocation>,
    #[serde(default)]
    pub prices: Vec<ServerTypePrice>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Deprecation {
    #[serde(default)]
    pub announced: Option<String>,
    #[serde(default)]
    pub unavailable_after: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ServerTypeLocation {
    pub name: String,
    pub available: bool,
    #[serde(default)]
    pub recommended: bool,
    #[serde(default)]
    pub deprecation: Option<Deprecation>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct MetaPagination {
    /// `null` / absent when there are no more pages.
    #[serde(default)]
    pub next_page: Option<u32>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct Meta {
    #[serde(default)]
    pub pagination: MetaPagination,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ServerTypeListResponse {
    pub server_types: Vec<ServerType>,
    /// Hetzner paginates the server-type catalog. `None` when the
    /// response predates pagination support or when a fixture omits it.
    #[serde(default)]
    pub meta: Option<Meta>,
}

/// One row of `GET /v1/locations`. Hetzner cloud locations are
/// the physical datacenters servers boot into (e.g. `nbg1` =
/// Nuremberg). We use the list endpoint mainly as a lightweight
/// `Authorization` probe (any valid token can read it, no API
/// quota is spent), and the wizard / region-picker in Track A.4b
/// will surface the same response to drive an autocomplete picker.
#[derive(Debug, Clone, Deserialize)]
pub struct Location {
    pub id: u64,
    pub name: String,
    pub description: String,
    pub country: String,
    pub city: String,
    pub network_zone: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LocationListResponse {
    pub locations: Vec<Location>,
}
