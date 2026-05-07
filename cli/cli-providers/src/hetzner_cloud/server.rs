// SPDX-License-Identifier: FSL-1.1-MIT
//! Higher-level resource specs for the Hetzner Cloud provider.
//! Each spec is the desired shape; the provider diffs it against
//! the API view and emits Actions.

use std::collections::BTreeMap;

#[derive(Debug, Clone)]
pub struct ServerSpec {
    pub name: String,
    pub server_type: String,
    pub image: String,
    pub location: String,
    pub labels: BTreeMap<String, String>,
    /// cloud-init `#cloud-config` payload, attached at server
    /// creation. None means no user_data is sent.
    pub user_data: Option<String>,
}

#[derive(Debug, Clone)]
pub struct SshKeySpec {
    pub name: String,
    pub public_key: String,
}

#[derive(Debug, Clone)]
pub struct NetworkSpec {
    pub name: String,
    /// e.g. "10.0.0.0/16"
    pub ip_range: String,
    /// e.g. "10.0.0.0/24"
    pub subnet_ip_range: String,
    /// Hetzner network zone (e.g. "eu-central").
    pub network_zone: String,
}

#[derive(Debug, Clone)]
pub struct FirewallSpec {
    pub name: String,
    pub rules: Vec<FirewallRuleSpec>,
}

#[derive(Debug, Clone)]
pub struct FirewallRuleSpec {
    /// "in" or "out".
    pub direction: String,
    /// Port number, range ("80-90"), or "any". `None` for icmp.
    pub port: Option<String>,
    /// "tcp", "udp", "icmp", "esp", "gre".
    pub protocol: String,
    pub source_ips: Vec<String>,
    pub destination_ips: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct FloatingIpSpec {
    pub name: String,
    /// "ipv4" or "ipv6".
    pub kind: String,
    /// Hetzner location, e.g. "nbg1".
    pub home_location: String,
}

/// Tag every AppRafter-managed Hetzner resource with this label so
/// `import` / `destroy` can find them later.
pub const APPRAFTER_LABEL: &str = "apprafter";
pub const APPRAFTER_LABEL_VALUE: &str = "true";
