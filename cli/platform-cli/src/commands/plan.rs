// SPDX-License-Identifier: FSL-1.1-Apache-2.0
use cli_core::Result;
use cli_providers::{DryRunProvider, Provider};
use cli_state::State;
use tracing::info;

use crate::commands::state_paths::resolve_state_paths;

pub fn run() -> Result<()> {
    // Per-target state (v0.1.154). Same active-target pattern as
    // `status`; the `plan` skeleton doesn't yet do anything
    // tier-specific, but reading state pins the future provider
    // factory to the right target.
    let resolved = resolve_state_paths(None)?;
    let state = State::load_or_default(&resolved.paths)?;
    info!(?state, target = %resolved.target_name, "plan invoked");

    let provider = DryRunProvider::new();
    let plan = provider.plan()?;

    println!("{}", plan.summary());
    Ok(())
}
