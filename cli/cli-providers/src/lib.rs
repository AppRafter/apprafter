// SPDX-License-Identifier: FSL-1.1-MIT
//! Infrastructure providers for `platform-cli`. Real built-in
//! providers (Hetzner Cloud, Hetzner Robot, AWS) land in later
//! phases; this crate currently ships a `DryRunProvider` for the
//! skeleton command flow.

pub mod dry_run;
pub mod provider;

pub use dry_run::DryRunProvider;
pub use provider::{ApplyOutcome, Plan, Provider};
