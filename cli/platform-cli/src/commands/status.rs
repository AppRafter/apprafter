// SPDX-License-Identifier: FSL-1.1-Apache-2.0
use cli_core::Result;
use cli_state::State;
use tracing::info;

use crate::commands::state_paths::resolve_state_paths;

pub fn run() -> Result<()> {
    // Per-target state (v0.1.154). Read-only command, no
    // `--target` override yet — surfaces the active target's
    // last-known cluster pointer.
    let resolved = resolve_state_paths(None)?;
    let state = State::load_or_default(&resolved.paths)?;
    info!(?state, target = %resolved.target_name, "status invoked");

    println!("would show status for target `{}`:", resolved.target_name);
    println!("  cluster: {:?}", state.cluster_name);
    println!("  tier:    {:?}", state.tier);
    println!("  provider:{:?}", state.provider);
    println!("(skeleton — live status is not yet implemented for this provider)");
    Ok(())
}
