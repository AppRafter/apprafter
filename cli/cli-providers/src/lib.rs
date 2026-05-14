// SPDX-License-Identifier: FSL-1.1-MIT
//! Infrastructure providers for `apprafter`.

pub mod dry_run;
pub mod hetzner_cloud;
pub mod k8s;
pub mod provider;

pub use dry_run::DryRunProvider;
pub use hetzner_cloud::{HetznerCloudClient, HetznerCloudProvider};
pub use provider::{Action, ApplyOutcome, DestroyOutcome, Plan, Provider};
