// SPDX-License-Identifier: FSL-1.1-Apache-2.0
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

use crate::cli::{
    AppCommand, Cli, Commands, MigrationCommand, OpenUi, PlatformCommand, RepoCommand,
    RepoCredsCommand, SecretCommand,
};

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

    // npm-style courtesy check for a newer CLI release. Best-
    // effort — never fails the invocation. 24h cache means
    // the network round-trip happens once a day at most.
    commands::version_check::maybe_warn_about_newer_version();

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
        Commands::Platform { action } => match action {
            PlatformCommand::Status => commands::platform::status()?,
            PlatformCommand::Upgrade { to } => commands::platform::upgrade(to.as_deref())?,
            PlatformCommand::Freeze { component, version } => {
                commands::platform::freeze(&component, version.as_deref())?
            }
            PlatformCommand::Unfreeze { component } => commands::platform::unfreeze(&component)?,
            PlatformCommand::Rescue { yes } => commands::platform::rescue(yes)?,
        },
        Commands::Migration { action } => match action {
            MigrationCommand::List => commands::migration::list()?,
            MigrationCommand::Approve { name } => commands::migration::approve(&name)?,
            MigrationCommand::Reject { name } => commands::migration::reject(&name)?,
        },
        Commands::Open { ui } => match ui {
            OpenUi::Argocd {
                project,
                all_projects,
            } => {
                let filter = if all_projects { None } else { Some(project) };
                commands::open::argocd(filter.as_deref())?
            }
        },
        Commands::App { action } => match action {
            AppCommand::Add {
                git_url,
                name,
                branch,
                path,
                project,
                namespace,
                remote,
                no_ping,
                coverage_gate,
                no_interactive,
                scaffold,
                env,
            } => commands::app::add(
                git_url,
                name,
                branch,
                &path,
                &project,
                &namespace,
                &remote,
                no_ping,
                coverage_gate,
                env,
                no_interactive,
                scaffold,
            )?,
            AppCommand::List {
                project,
                all_projects,
                all_managed,
            } => commands::app::list(&project, all_projects, all_managed)?,
            AppCommand::Status { name, resources } => commands::app::status(&name, resources)?,
            AppCommand::Logs {
                name,
                follow,
                tail,
                container,
                pod,
            } => commands::app::logs(&name, follow, tail, container, pod)?,
            AppCommand::Rollback { name, to, yes } => commands::app::rollback(&name, to, yes)?,
            AppCommand::Open {
                name,
                port,
                container_port,
                no_browser,
            } => commands::app_open::open(&name, port, container_port, no_browser)?,
            AppCommand::Scaffold {
                runtime,
                name,
                namespace,
                path,
                force,
                needs,
            } => {
                let resolved_runtime = match runtime {
                    Some(slug) => Some(
                        commands::app_scaffold::parse_runtime_slug(&slug).ok_or_else(|| {
                            cli_core::CliError::Other(format!(
                                "unknown runtime slug '{slug}'. Valid: bun, node-pnpm, \
                                 node-yarn, node-npm, python-poetry, python-uv, \
                                 python-pipenv, python-pip, rust, go, docker, blank."
                            ))
                        })?,
                    ),
                    None => None,
                };
                commands::app_scaffold::scaffold(commands::app_scaffold::ScaffoldOpts {
                    runtime: resolved_runtime,
                    name,
                    namespace,
                    path,
                    force,
                    needs,
                })?
            }
            AppCommand::Remove {
                name,
                yes,
                keep_data,
                env,
            } => commands::app::remove(&name, yes, keep_data, env)?,
        },
        Commands::Repo { action } => match action {
            RepoCommand::Creds { action } => match action {
                RepoCredsCommand::Add {
                    name,
                    url_prefix,
                    auth_type,
                    username,
                    token,
                    no_validate,
                    no_interactive,
                } => commands::repo_creds::add(
                    &name,
                    &url_prefix,
                    &auth_type,
                    &username,
                    token,
                    no_validate,
                    no_interactive,
                )?,
                RepoCredsCommand::List => commands::repo_creds::list()?,
                RepoCredsCommand::Show { name } => commands::repo_creds::show(&name)?,
                RepoCredsCommand::Rotate {
                    name,
                    token,
                    no_validate,
                } => commands::repo_creds::rotate(&name, token, no_validate)?,
                RepoCredsCommand::Remove { name, force, yes } => {
                    commands::repo_creds::remove(&name, force, yes)?
                }
            },
        },
        Commands::Secret { action } => match action {
            SecretCommand::Seal {
                name,
                from_literal,
                namespace,
                secret_type,
                stdout,
            } => {
                commands::secret::run_seal(&name, &namespace, &from_literal, &secret_type, stdout)?
            }
        },
    }
    Ok(())
}
