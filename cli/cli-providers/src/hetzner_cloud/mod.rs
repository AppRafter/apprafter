// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Built-in provider for the Hetzner Cloud API.

pub mod client;
pub mod kubeconfig;
pub mod node_ip;
pub mod provider;
pub mod server;
pub mod server_type;
pub mod types;
pub mod user_data;

pub use client::{HetznerCloudClient, DEFAULT_BASE_URL};
pub use kubeconfig::{
    default_ssh_identity_path, rewrite_server_url, KubeconfigFetcher, SshCommandRunner,
    SshKubeconfigFetcher,
};
pub use node_ip::{extract_public_ips, node_ipv6_address, node_public_ips};
pub use provider::{rule_spec_to_wire, HetznerCloudProvider};
pub use server::{
    FirewallRuleSpec, FirewallSpec, FloatingIpSpec, NetworkSpec, ServerSpec, SshKeySpec,
    APPRAFTER_LABEL, APPRAFTER_LABEL_VALUE,
};
pub use server_type::validate_server_type;
pub use types::{
    ApiErrorDetails, ApiErrorEnvelope, Deprecation, Firewall, FirewallCreateRequest,
    FirewallCreateResponse, FirewallListResponse, FirewallReference, FirewallRule, FloatingIp,
    FloatingIpCreateRequest, FloatingIpCreateResponse, FloatingIpListResponse, HomeLocation,
    Location, LocationListResponse, Network, NetworkCreateRequest, NetworkCreateResponse,
    NetworkListResponse, PublicIpv4, PublicIpv6, PublicNet, Server, ServerCreateRequest,
    ServerCreateResponse, ServerListResponse, ServerStatus, ServerType, ServerTypeListResponse,
    ServerTypeLocation, SshKey, SshKeyCreateRequest, SshKeyCreateResponse, SshKeyListResponse,
    Subnet,
};
pub use user_data::{
    build_k3s_user_data, K3sBootstrapOptions, CLUSTER_CIDR_DUAL_STACK, SERVICE_CIDR_DUAL_STACK,
};
