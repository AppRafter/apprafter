// SPDX-License-Identifier: FSL-1.1-MIT
use cli_core::Result;
use cli_providers::{DryRunProvider, Provider};
use cli_state::{State, StatePaths};
use tracing::info;

pub fn run() -> Result<()> {
    let cwd = std::env::current_dir()?;
    let paths = StatePaths::for_root(&cwd);
    let state = State::load_or_default(&paths)?;
    info!(?state, "plan invoked");

    let provider = DryRunProvider::new();
    let plan = provider.plan()?;

    println!("{}", plan.summary());
    Ok(())
}
