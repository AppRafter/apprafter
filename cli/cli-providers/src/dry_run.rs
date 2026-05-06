// SPDX-License-Identifier: FSL-1.1-MIT
//! No-op provider used by the skeleton CLI.

use cli_core::Result;

use crate::provider::{ApplyOutcome, Plan, Provider};

#[derive(Debug, Default, Clone)]
pub struct DryRunProvider;

impl DryRunProvider {
    pub fn new() -> Self {
        Self
    }
}

impl Provider for DryRunProvider {
    fn plan(&self) -> Result<Plan> {
        Ok(Plan::default())
    }

    fn apply(&self) -> Result<ApplyOutcome> {
        Ok(ApplyOutcome::default())
    }
}
