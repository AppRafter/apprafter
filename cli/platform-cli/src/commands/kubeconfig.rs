// SPDX-License-Identifier: FSL-1.1-MIT
//! Print the k3s kubeconfig for the current cluster, fetching it
//! over SSH on first use and caching the result in state.

use cli_core::Result;

pub fn run(_refresh: bool) -> Result<()> {
    // Real body lands in the next commit.
    Ok(())
}
