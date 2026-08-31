// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The `Commands` -> handler dispatch table.
//!
//! Wiring only: every arm destructures one clap variant and forwards
//! it to the module under `commands/` that implements the verb. Kept
//! out of `lib.rs` so the crate root stays a readable statement of
//! the public surface.

use crate::cli::{
    AppCommand, AutoscaleCommand, BackupAction, Cli, Commands, EgressCommand, EnvCommand,
    MigrationCommand, OpenUi, PlatformCommand, RepoCommand, RepoCredsCommand, SecretCommand,
};
use crate::commands;

/// Pure dispatch over the parsed CLI. Returns `cli_core::Result`
/// so the typed `CliError -> miette::Report` conversion happens
/// exactly once at the crate's entry point ([`crate::run`]) and the
/// inner code keeps using the original `?` ergonomics over
/// `cli_core::Result<T>`.
pub(crate) fn dispatch(args: Cli) -> cli_core::Result<()> {
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
        Commands::Apply {
            target,
            server_type,
        } => commands::apply::run(target.as_deref(), server_type.as_deref())?,
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
        Commands::BootstrapAll {
            target,
            dry_run,
            server_type,
        } => commands::bootstrap_all::run(target.as_deref(), dry_run, server_type.as_deref())?,
        Commands::Platform { action } => match action {
            PlatformCommand::Status { cached } => commands::platform::status(cached)?,
            PlatformCommand::Upgrade { to, cached } => {
                commands::platform::upgrade(to.as_deref(), cached)?
            }
            PlatformCommand::Freeze { component, version } => {
                commands::platform::freeze(&component, version.as_deref())?
            }
            PlatformCommand::Unfreeze { component } => commands::platform::unfreeze(&component)?,
            PlatformCommand::Rescue { yes } => commands::platform::rescue(yes)?,
            PlatformCommand::Egress { action } => match action {
                EgressCommand::Show => commands::platform::egress_show()?,
                EgressCommand::Set { profile } => commands::platform::egress_set(&profile)?,
            },
            PlatformCommand::Env { action } => match action {
                EnvCommand::Show => commands::platform::env_show()?,
                EnvCommand::Set { env } => commands::platform::env_set(&env)?,
            },
            PlatformCommand::Autoscale { action } => match action {
                AutoscaleCommand::Show => commands::platform::autoscale_show()?,
                AutoscaleCommand::Set { mode } => commands::platform::autoscale_set(&mode)?,
            },
        },
        Commands::Migration { action } => match action {
            MigrationCommand::List => commands::migration::list()?,
            MigrationCommand::Approve { name, namespace } => {
                commands::migration::approve(&name, namespace.as_deref())?
            }
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
                env,
                follow,
                tail,
                container,
                pod,
            } => commands::app::logs(&name, env, follow, tail, container, pod)?,
            AppCommand::Rollback { name, env, to, yes } => {
                commands::app::rollback(&name, env, to, yes)?
            }
            AppCommand::Unpin { name, env, yes } => commands::app::unpin(&name, env, yes)?,
            AppCommand::Open {
                name,
                env,
                port,
                container_port,
                no_browser,
            } => commands::app_open::open(&name, env, port, container_port, no_browser)?,
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
            AppCommand::Validate { manifest } => commands::app_validate::run_validate(manifest)?,
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
                yes,
            } => commands::secret::run_seal(
                &name,
                &namespace,
                &from_literal,
                &secret_type,
                stdout,
                yes,
            )?,
            SecretCommand::List { namespace } => commands::secret::run_list(namespace.as_deref())?,
            SecretCommand::Remove {
                name,
                namespace,
                yes,
            } => commands::secret::run_remove(&name, &namespace, yes)?,
        },
        Commands::Volume { action } => commands::volume::run(action)?,
        Commands::Node { action } => commands::node_prep::run(action)?,
        Commands::Export {
            namespace,
            select,
            out,
        } => commands::backup::run_export(&namespace, select, out.as_deref())?,
        Commands::Backup { action } => match action {
            BackupAction::Create {
                namespace,
                select,
                repo,
                passphrase,
                staging_mode,
            } => commands::backup::run_backup(
                &namespace,
                select,
                repo.as_deref(),
                passphrase.as_deref(),
                staging_mode.as_deref(),
            )?,
            BackupAction::List { repo, passphrase } => {
                commands::backup::run_backup_list(repo.as_deref(), passphrase.as_deref())?
            }
            BackupAction::Prune {
                repo,
                credential_file,
                keep_daily,
                keep_weekly,
                keep_monthly,
            } => commands::backup::run_backup_prune(
                repo.as_deref(),
                credential_file.as_deref(),
                keep_daily,
                keep_weekly,
                keep_monthly,
            )?,
            BackupAction::Check {
                repo,
                credential_file,
                read_data,
            } => commands::backup::run_backup_check(
                repo.as_deref(),
                credential_file.as_deref(),
                read_data,
            )?,
            BackupAction::Unlock {
                repo,
                credential_file,
            } => commands::backup::run_backup_unlock(repo.as_deref(), credential_file.as_deref())?,
            BackupAction::Enable {
                bucket,
                endpoint,
                prefix,
                credential,
                credential_file,
                cron,
                keep_daily,
                keep_weekly,
                keep_monthly,
                enforce,
                staging_mode,
                check_cron,
                failure_webhook,
                i_have_saved_credentials,
            } => commands::backup::run_backup_enable(
                commands::backup::EnableOpts {
                    bucket,
                    credential,
                    cron,
                    keep_daily,
                    keep_weekly,
                    keep_monthly,
                    enforce,
                    staging_mode,
                    check_cron,
                    failure_webhook,
                },
                endpoint.as_deref(),
                prefix.as_deref(),
                credential_file.as_deref(),
                i_have_saved_credentials,
            )?,
            BackupAction::Disable => commands::backup::run_backup_disable()?,
            BackupAction::Status => commands::backup::run_backup_status()?,
        },
        Commands::Restore {
            repo,
            target,
            reprovision,
            snapshot,
            data_only,
            passphrase,
            credential_file,
            server_type,
        } => commands::restore::run_restore(
            &repo,
            target.as_deref(),
            reprovision,
            snapshot.as_deref(),
            data_only,
            passphrase.as_deref(),
            credential_file.as_deref(),
            server_type.as_deref(),
        )?,
        Commands::Completion { shell } => commands::completions::run(shell)?,
    }
    Ok(())
}
