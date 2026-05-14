// SPDX-License-Identifier: FSL-1.1-MIT
//! Entry point for the `apprafter` CLI.
//!
//! v0.1.86 — error rendering migrated from `color-eyre` to
//! `miette`'s `fancy` reporter. `CliError` (defined in cli-core)
//! derives `miette::Diagnostic`, so unhandled errors render with
//! the rustc-style `error:` / `help:` / `code:` block instead of
//! `Debug` output.

mod cli;
mod commands;

use clap::Parser;
use cli_core::logging;
use miette::{IntoDiagnostic, Result};

use crate::cli::{Cli, Commands};

fn main() -> Result<()> {
    // Configure miette's `fancy` reporter as the global panic /
    // error formatter. Disable backtraces in the rendered output
    // — they're loud and almost never relevant for end users; the
    // diagnostic `help:` line is more actionable. Backtraces are
    // still available via `RUST_BACKTRACE=1` for development.
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

    let args = Cli::parse();
    dispatch(args).map_err(miette::Report::new)
}

/// Pure dispatch over the parsed CLI. Returns `cli_core::Result`
/// so the typed `CliError -> miette::Report` conversion happens
/// exactly once at the binary boundary (`main` above) and the
/// inner code keeps using the original `?` ergonomics over
/// `cli_core::Result<T>`. Easier to test in isolation if we ever
/// want to, too.
fn dispatch(args: Cli) -> cli_core::Result<()> {
    match args.command {
        Commands::Target { action } => commands::target::run(action)?,
        Commands::Whoami { no_ping } => commands::whoami::run(no_ping)?,
        Commands::Auth { action } => commands::auth::run(action)?,
        Commands::Doctor { target, no_ping } => commands::doctor::run(target.as_deref(), no_ping)?,
        Commands::Init {
            provider,
            tier,
            region,
        } => commands::init::run(&provider, &tier, &region)?,
        Commands::Plan => commands::plan::run()?,
        Commands::Apply { target } => commands::apply::run(target.as_deref())?,
        Commands::Status => commands::status::run()?,
        Commands::Login => commands::login::run()?,
        Commands::UpgradeTier { to } => commands::upgrade_tier::run(&to)?,
        Commands::Destroy { yes, target } => commands::destroy::run(yes, target.as_deref())?,
        Commands::Import {
            force,
            dry_run,
            target,
        } => commands::import::run(force, dry_run, target.as_deref())?,
        Commands::Kubeconfig { refresh, target } => {
            commands::kubeconfig::run(refresh, target.as_deref())?
        }
        Commands::ClusterBootstrap => commands::cluster_bootstrap::run()?,
        Commands::ArgocdPassword { refresh } => commands::argocd_password::run(refresh)?,
        Commands::BootstrapAll { target, dry_run } => {
            commands::bootstrap_all::run(target.as_deref(), dry_run)?
        }
    }
    Ok(())
}
