// SPDX-License-Identifier: FSL-1.1-MIT
//! Print the Argo CD admin password from the cluster, caching the
//! result age-encrypted in state. See plan.md phase 1.5 (v0.1.14).

use cli_core::Result;

pub fn run(_refresh: bool) -> Result<()> {
    // Real body lands in the next commit.
    Ok(())
}
