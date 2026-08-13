// SPDX-License-Identifier: FSL-1.1-Apache-2.0
use std::str::FromStr;

use cli_core::{Result, Tier};
use tracing::info;

pub fn run(to: &str) -> Result<()> {
    let target = Tier::from_str(to)?;
    info!(target = %target, "upgrade-tier invoked");
    println!("would upgrade tier to {target}");
    println!("(tier upgrades are not yet available in this release)");
    Ok(())
}
