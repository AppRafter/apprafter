// SPDX-License-Identifier: FSL-1.1-MIT
use std::str::FromStr;

use cli_core::{Result, Tier};
use tracing::info;

pub fn run(to: &str) -> Result<()> {
    let target = Tier::from_str(to)?;
    info!(target = %target, "upgrade-tier invoked");
    println!("would upgrade tier to {target}");
    println!("(skeleton — tier upgrades land in plan.md phase 3.10 / 5.7)");
    Ok(())
}
