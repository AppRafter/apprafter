// SPDX-License-Identifier: FSL-1.1-Apache-2.0
use std::str::FromStr;

use cli_core::{Result, Tier};
use cli_state::State;
use tracing::info;

use crate::commands::state_paths::resolve_state_paths;

pub fn run(provider: &str, tier: &str, region: &str) -> Result<()> {
    let parsed_tier = Tier::from_str(tier)?;
    info!(provider, tier = %parsed_tier, region, "init invoked");

    // Per-target state (v0.1.154). `init` writes the same data
    // `target add` writes; the recommended path is
    // `apprafter target add … && apprafter apply` (apply seeds
    // state from the target config). `init` stays for operators
    // who prefer the explicit two-step setup.
    let resolved = resolve_state_paths(None)?;
    let paths = resolved.paths;
    let mut state = State::load_or_default(&paths)?;

    state.provider = Some(provider.to_string());
    state.tier = Some(parsed_tier);
    state.region = Some(region.to_string());
    if state.cluster_name.is_none() {
        state.cluster_name = Some("platform-1".to_string());
    }
    state.save(&paths)?;

    println!("would init: provider={provider} tier={parsed_tier} region={region}");
    println!(
        "(state written to {}; run `apply` next)",
        paths.state_file().display()
    );
    Ok(())
}
