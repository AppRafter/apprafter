// SPDX-License-Identifier: FSL-1.1-MIT
use std::str::FromStr;

use cli_core::{Result, Tier};
use tracing::info;

pub fn run(provider: &str, tier: &str, region: &str) -> Result<()> {
    let parsed_tier = Tier::from_str(tier)?;
    info!(provider, tier = %parsed_tier, region, "init invoked");

    println!("would init: provider={provider} tier={parsed_tier} region={region}");
    println!("(skeleton — provisioning lands in plan.md phase 1.2 / 1.3)");
    Ok(())
}
