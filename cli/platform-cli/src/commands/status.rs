// SPDX-License-Identifier: FSL-1.1-Apache-2.0
use cli_core::Result;
use cli_state::{State, StatePaths};
use tracing::info;

pub fn run() -> Result<()> {
    let cwd = std::env::current_dir()?;
    let paths = StatePaths::for_root(&cwd);
    let state = State::load_or_default(&paths)?;
    info!(?state, "status invoked");

    println!("would show status:");
    println!("  cluster: {:?}", state.cluster_name);
    println!("  tier:    {:?}", state.tier);
    println!("  provider:{:?}", state.provider);
    println!("(skeleton — live status arrives with a real provider in phase 1.2+)");
    Ok(())
}
