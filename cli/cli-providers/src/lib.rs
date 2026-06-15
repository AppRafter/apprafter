// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Infrastructure providers for `apprafter`.

pub mod cert;
pub mod cloudflare;
pub mod dry_run;
pub mod hetzner_cloud;
pub mod k8s;
pub mod provider;
pub mod validators;

pub use cert::{
    build_tls_secret, expiry_status, parse_and_validate, validate_cert_name, ExpiryStatus,
    ImportedCert,
};
pub use cloudflare::{fetch_cloudflare_ips, CloudflareIpSource, UreqCloudflareIpSource};
pub use dry_run::DryRunProvider;
pub use hetzner_cloud::{
    extract_public_ips, node_ipv6_address, node_public_ips, rule_spec_to_wire, HetznerCloudClient,
    HetznerCloudProvider,
};
pub use provider::{Action, ApplyOutcome, DestroyOutcome, Plan, Provider};
pub use validators::{HetznerCloudValidator, ProviderValidator, RegionInfo};
