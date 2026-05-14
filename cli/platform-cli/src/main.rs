// SPDX-License-Identifier: FSL-1.1-MIT
//! Entry point for the `apprafter` CLI.

mod cli;
mod commands;

use clap::Parser;
use cli_core::logging;

use crate::cli::{Cli, Commands};

fn main() -> color_eyre::Result<()> {
    color_eyre::install()?;
    logging::init();

    let args = Cli::parse();
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
        Commands::Kubeconfig { refresh } => commands::kubeconfig::run(refresh)?,
        Commands::ClusterBootstrap => commands::cluster_bootstrap::run()?,
        Commands::ArgocdPassword { refresh } => commands::argocd_password::run(refresh)?,
    }
    Ok(())
}
