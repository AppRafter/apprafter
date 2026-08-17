// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Entry point for the `apprafter` CLI.
//!
//! Everything from argv onward lives in the `apprafter` lib target
//! so `docsgen` can project the clap tree; this binary is a thin
//! wrapper over [`apprafter::run`].

use miette::Result;

fn main() -> Result<()> {
    apprafter::run()
}
