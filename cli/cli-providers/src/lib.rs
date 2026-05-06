// SPDX-License-Identifier: FSL-1.1-MIT
//! Infrastructure providers for `platform-cli`.
//! Built-in providers live in submodules of this crate; community
//! providers will arrive as `InfrastructureProviderPlugin`s in a
//! later phase.

pub mod dry_run;
pub mod provider;

pub use dry_run::DryRunProvider;
pub use provider::{Action, ApplyOutcome, DestroyOutcome, Plan, Provider};
