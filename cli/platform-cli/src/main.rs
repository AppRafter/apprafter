// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Entry point for the `apprafter` CLI.
//!
//! v0.1.86 — error rendering migrated from `color-eyre` to
//! `miette`'s `fancy` reporter. `CliError` (defined in cli-core)
//! derives `miette::Diagnostic`, so unhandled errors render with
//! the rustc-style `error:` / `help:` / `code:` block instead of
//! `Debug` output.
//!
//! Everything below the argv boundary lives in the `apprafter` lib
//! target so `docsgen` can project the clap tree; this binary is a
//! thin wrapper over [`apprafter::run`].

use miette::Result;

fn main() -> Result<()> {
    apprafter::run()
}
