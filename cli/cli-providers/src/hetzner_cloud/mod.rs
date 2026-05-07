// SPDX-License-Identifier: FSL-1.1-MIT
//! Built-in provider for the Hetzner Cloud API.

pub mod client;
pub mod provider;
pub mod server;
pub mod types;
pub mod user_data;

pub use client::{HetznerCloudClient, DEFAULT_BASE_URL};
pub use provider::HetznerCloudProvider;
pub use server::{
    FirewallRuleSpec, FirewallSpec, FloatingIpSpec, NetworkSpec, ServerSpec, SshKeySpec,
    APPRAFTER_LABEL, APPRAFTER_LABEL_VALUE,
};
pub use types::{
    ApiErrorDetails, ApiErrorEnvelope, Firewall, FirewallCreateRequest, FirewallCreateResponse,
    FirewallListResponse, FirewallReference, FirewallRule, FloatingIp, FloatingIpCreateRequest,
    FloatingIpCreateResponse, FloatingIpListResponse, HomeLocation, Network, NetworkCreateRequest,
    NetworkCreateResponse, NetworkListResponse, Server, ServerCreateRequest, ServerCreateResponse,
    ServerListResponse, ServerStatus, SshKey, SshKeyCreateRequest, SshKeyCreateResponse,
    SshKeyListResponse, Subnet,
};
pub use user_data::{build_k3s_user_data, K3sBootstrapOptions};
