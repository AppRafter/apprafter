// SPDX-License-Identifier: FSL-1.1-Apache-2.0
use cli_core::Result;
use tracing::info;

pub fn run() -> Result<()> {
    info!("login invoked");
    println!("would login: open device-flow OIDC and write kubeconfig");
    println!("(OIDC login is not yet available in this release)");
    Ok(())
}
