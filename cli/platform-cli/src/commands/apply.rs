// SPDX-License-Identifier: FSL-1.1-MIT
use cli_core::Result;
use cli_providers::{DryRunProvider, Provider};
use tracing::info;

pub fn run() -> Result<()> {
    info!("apply invoked");
    let provider = DryRunProvider::new();
    let outcome = provider.apply()?;
    println!("would apply: {} change(s)", outcome.applied);
    println!("(skeleton — real provisioning lands in plan.md phase 1.2 / 1.3)");
    Ok(())
}
