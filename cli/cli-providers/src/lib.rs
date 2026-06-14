// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Infrastructure providers for `apprafter`.

pub mod cert;
pub mod cloudflare;
pub mod dry_run;
pub mod hetzner_cloud;
pub mod k8s;
pub mod provider;
pub mod validators;

pub use cert::{expiry_status, parse_and_validate, ExpiryStatus, ImportedCert};
pub use cloudflare::{fetch_cloudflare_ips, CloudflareIpSource, UreqCloudflareIpSource};
pub use dry_run::DryRunProvider;
pub use hetzner_cloud::{HetznerCloudClient, HetznerCloudProvider};
pub use provider::{Action, ApplyOutcome, DestroyOutcome, Plan, Provider};
pub use validators::{HetznerCloudValidator, ProviderValidator, RegionInfo};
