// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Library target for the `apprafter` CLI.
//!
//! The binary is a thin `main` over this lib. The lib exists so
//! out-of-process tooling — the forthcoming `docsgen` crate — can
//! read the clap definitions and reuse the CUE validation path
//! instead of re-implementing either.
//!
//! The modules themselves stay crate-private: the only surface a
//! consumer may rely on is [`docs_api`] plus [`run`]. Publishing
//! `commands` wholesale would export ~45 modules' worth of
//! functions and make the facade decorative.

pub(crate) mod cli;
pub(crate) mod commands;
mod dispatch;
pub(crate) mod examples;

use clap::Parser;
use cli_core::logging;
use miette::{IntoDiagnostic, Result};

use crate::cli::Cli;
use crate::dispatch::dispatch;

/// The deliberate, greppable surface `docsgen` depends on.
///
/// Keeping it in one facade means widening the crate's public API is
/// an explicit act, not a side effect of a refactor. Anything added
/// here is a contract with the documentation gate.
pub mod docs_api {
    /// The clap root — `Cli::command()` yields the whole tree.
    pub use crate::cli::Cli;
    /// Validate one `Application.cue` through the injected-schema
    /// pipeline (the CLI no longer vendors a `cue.mod`, so a bare
    /// `cue vet` of a user manifest fails with `no cue.mod`).
    pub use crate::commands::app_validate::validate_manifest;
    /// The pieces that path builds its temp workspace from, so a
    /// consumer that must BATCH (the documentation gate vets every
    /// manifest in the docs in one `cue vet`) reproduces the same
    /// environment instead of embedding a second copy of the
    /// schemas that could drift from this one.
    pub use crate::commands::app_validate::{WORKSPACE_MODULE_CUE, WORKSPACE_SCHEMAS};
    /// The worked examples, structurally — the one array the binary
    /// renders into `--help` and `docsgen` projects into the reference.
    ///
    /// Exposed as data rather than recovered by parsing help text: a
    /// guard that reads a rendering of its own subject can only ever
    /// confirm the rendering, and a parse that finds nothing passes
    /// silently. See `crate::examples` for the full argument.
    ///
    /// [`EXAMPLES`] itself is exported alongside [`examples_for`]
    /// because coverage — "every leaf command has one" — has to be
    /// asserted over the whole table, not one key at a time.
    pub use crate::examples::{examples_for, CommandExamples, EXAMPLES};
}

/// Parse argv and run the CLI. `main` is a thin wrapper so the whole
/// post-argv path is linkable from outside the crate.
pub fn run() -> Result<()> {
    // Configure miette's reporter as the global error formatter.
    // `CliError` (defined in cli-core) derives `miette::Diagnostic`,
    // so unhandled errors render with the rustc-style `error:` /
    // `help:` / `code:` block instead of `Debug` output.
    //
    // No backtrace is rendered, and there is no knob that turns one
    // on: nothing below enables it, `CliError` captures no
    // `Backtrace`, and `miette::set_panic_hook()` is deliberately
    // not called (a wizard abort should print "wizard aborted", not
    // a trace). `RUST_BACKTRACE` therefore does NOT affect rendered
    // diagnostics — an earlier version of this comment said it did,
    // and that claim reached the published docs before being caught.
    // If backtraces are ever wanted, they need a real implementation
    // here, not a comment. See `docs/reference/environment.md`.
    miette::set_hook(Box::new(|_| {
        Box::new(
            miette::MietteHandlerOpts::new()
                .terminal_links(true)
                .unicode(true)
                .context_lines(2)
                .with_cause_chain()
                .build(),
        )
    }))
    .into_diagnostic()?;

    logging::init();

    // npm-style courtesy check for a newer CLI release. Best-
    // effort — never fails the invocation. A 6h cache means the
    // network round-trip happens four times a day at most.
    commands::version_check::maybe_warn_about_newer_version();

    let args = Cli::parse();
    dispatch(args).map_err(miette::Report::new)
}
