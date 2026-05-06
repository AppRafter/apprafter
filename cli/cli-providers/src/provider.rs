// SPDX-License-Identifier: FSL-1.1-MIT
//! Trait that every infrastructure provider implements.
//!
//! Built-in providers (Hetzner Cloud, Hetzner Robot, AWS) and
//! community InfrastructureProviderPlugins both speak this trait.
//! For phase 1.1 we ship only `DryRunProvider`; real providers
//! arrive in phases 1.2 / 5.2 / 6.2.

use cli_core::Result;

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Plan {
    pub changes: Vec<String>,
}

impl Plan {
    pub fn summary(&self) -> String {
        if self.changes.is_empty() {
            "no changes".to_string()
        } else {
            format!("{} change(s)", self.changes.len())
        }
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ApplyOutcome {
    pub applied: usize,
}

pub trait Provider {
    fn plan(&self) -> Result<Plan>;
    fn apply(&self) -> Result<ApplyOutcome>;
}
