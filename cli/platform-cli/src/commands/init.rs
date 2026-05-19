// SPDX-License-Identifier: FSL-1.1-Apache-2.0
use std::str::FromStr;

use cli_core::{Result, Tier};
use cli_state::{State, StatePaths};
use tracing::info;

pub fn run(provider: &str, tier: &str, region: &str) -> Result<()> {
    let parsed_tier = Tier::from_str(tier)?;
    info!(provider, tier = %parsed_tier, region, "init invoked");

    let cwd = std::env::current_dir()?;
    let paths = StatePaths::for_root(&cwd);
    let mut state = State::load_or_default(&paths)?;

    state.provider = Some(provider.to_string());
    state.tier = Some(parsed_tier);
    state.region = Some(region.to_string());
    if state.cluster_name.is_none() {
        state.cluster_name = Some("platform-1".to_string());
    }
    state.save(&paths)?;

    println!("would init: provider={provider} tier={parsed_tier} region={region}");
    println!("(state written to .apprafter/state.json; run `apply` next)");
    Ok(())
}
