// SPDX-License-Identifier: FSL-1.1-MIT
use cli_core::Result;
use tracing::info;

pub fn run() -> Result<()> {
    info!("login invoked");
    println!("would login: open device-flow OIDC and write kubeconfig");
    println!("(skeleton — real OIDC flow lands in plan.md phase 4.7)");
    Ok(())
}
