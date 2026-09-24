// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The two startup checks, and which invocations run them.
//!
//! Before a command runs, `apprafter` may say that a newer CLI release exists
//! ([`crate::commands::version_check`], a call to api.github.com) and that the
//! node is nearly out of disk ([`crate::commands::node_disk_check`], a
//! `kubectl get platformstack` against the ambient kubeconfig). Both reach
//! outside this machine, the second to whichever cluster the current kubeconfig
//! context names.
//!
//! # Only after clap has parsed, and only for a command that goes out anyway
//!
//! They used to run before clap had parsed anything, so `apprafter --help`
//! ran `kubectl` against the current context: a request for the usage text
//! reached a cluster. Run after parsing, they are never reached by the
//! invocations clap answers and exits on itself: `--help`, `-h`, `help`,
//! `--version`, `-V`, and an argument error.
//!
//! Of the commands that do run, they are skipped by the ones that never leave
//! this machine — [`reaches_beyond_this_machine`] lists them. Those read and
//! write local files and print; they work offline, and the checks would turn
//! each into a network call and a cluster read the command itself never
//! makes (`apprafter completion bash` is often run by every new shell).
//! Every other command talks to a cluster, a node or a provider, and keeps
//! both checks — including the ones whose destination depends on a flag,
//! since the check runs before the flag is acted on.
//!
//! # `APPRAFTER_SKIP_STARTUP_CHECKS`
//!
//! Set to anything, it turns both off for every command. In a harness that
//! spawns this binary hundreds of times the checks are slow and
//! non-deterministic, and they made the measured CODE COVERAGE of this crate
//! depend on whether an earlier run had warmed their caches — two workspace
//! measurements 0.14pp apart with nothing changed between them, traced to
//! exactly this. `cli/.cargo/config.toml` sets it for everything cargo runs,
//! which covers the integration suite and `cargo run`: a source build is not
//! a release, so telling its user that some published version is "newer" is
//! noise about a comparison that does not mean what it appears to.

use crate::cli::{AppCommand, Commands, TargetCommand};
use crate::commands;

/// Run the startup checks for `command`, unless it never leaves this machine
/// or `APPRAFTER_SKIP_STARTUP_CHECKS` is set. Best-effort: neither check ever
/// fails or delays a command beyond its own short timeout.
pub(crate) fn run_startup_checks(command: &Commands) {
    if std::env::var_os("APPRAFTER_SKIP_STARTUP_CHECKS").is_some()
        || !reaches_beyond_this_machine(command)
    {
        return;
    }
    // npm-style courtesy notice for a newer CLI release, behind a 6h cache.
    commands::version_check::maybe_warn_about_newer_version();
    // 2.22d (D8): the node's disk is not specific to any one command — when
    // it fills, every workload on that node stops writing at once — so it
    // warns here, on the same hook, rather than waiting for someone to run
    // the one status command that would have shown it.
    commands::node_disk_check::maybe_warn_about_node_disk();
}

/// Does `command` reach anything beyond this machine — a cluster, a node over
/// SSH, a provider's API, a Git host or a bucket? The startup checks run only
/// for those.
///
/// `false` for the commands that only read or write this machine's files and
/// print:
///
/// * `completion` — prints (or writes) a script generated from the clap tree.
/// * `app validate` — `cue vet` of a local manifest, with the schemas built
///   into the binary.
/// * `app scaffold` — writes an application skeleton; the only process it
///   starts is a local `git remote get-url`.
/// * `target list`, `target use`, `target show`, `target rename`, `target
///   remove` — the local target store. (`target add` pings the provider,
///   `target ip` and `target machine` ask it, and `target cert`, `target
///   domain` and `target firewall` write to the cluster: they keep the
///   checks.)
/// * `init` and `plan` — the local state file only; `plan` compares nothing
///   against live infrastructure.
/// * `login`, `upgrade-tier` and `auth` — not available yet: each prints
///   what to use instead.
///
/// The match is exhaustive on purpose, with no catch-all: a new command does
/// not compile until someone decides which side it is on.
pub(crate) fn reaches_beyond_this_machine(command: &Commands) -> bool {
    match command {
        Commands::Completion { .. }
        | Commands::Init { .. }
        | Commands::Plan
        | Commands::Login
        | Commands::UpgradeTier { .. }
        | Commands::Auth { .. } => false,
        Commands::Target { action } => match action {
            TargetCommand::List
            | TargetCommand::Use { .. }
            | TargetCommand::Show { .. }
            | TargetCommand::Rename { .. }
            | TargetCommand::Remove { .. } => false,
            TargetCommand::Add { .. }
            | TargetCommand::Cert { .. }
            | TargetCommand::Domain { .. }
            | TargetCommand::Firewall { .. }
            | TargetCommand::Ip
            | TargetCommand::Machine { .. } => true,
        },
        Commands::App { action } => match action {
            AppCommand::Validate { .. } | AppCommand::Scaffold { .. } => false,
            AppCommand::Add { .. }
            | AppCommand::List { .. }
            | AppCommand::Status { .. }
            | AppCommand::Logs { .. }
            | AppCommand::Rollback { .. }
            | AppCommand::Unpin { .. }
            | AppCommand::Restart { .. }
            | AppCommand::Open { .. }
            | AppCommand::Remove { .. } => true,
        },
        Commands::Whoami { .. }
        | Commands::Doctor { .. }
        | Commands::Apply { .. }
        | Commands::Status
        | Commands::Destroy { .. }
        | Commands::Import { .. }
        | Commands::Kubeconfig { .. }
        | Commands::ClusterBootstrap
        | Commands::ArgocdPassword { .. }
        | Commands::BootstrapAll { .. }
        | Commands::Platform { .. }
        | Commands::Migration { .. }
        | Commands::Open { .. }
        | Commands::Repo { .. }
        | Commands::Secret { .. }
        | Commands::Volume { .. }
        | Commands::Db { .. }
        | Commands::Node { .. }
        | Commands::Top
        | Commands::Export { .. }
        | Commands::Backup { .. }
        | Commands::Restore { .. } => true,
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::reaches_beyond_this_machine;
    use crate::cli::Cli;

    fn goes_out(argv: &[&str]) -> bool {
        let cli = Cli::try_parse_from(std::iter::once("apprafter").chain(argv.iter().copied()))
            .unwrap_or_else(|e| panic!("{argv:?} does not parse: {e}"));
        reaches_beyond_this_machine(&cli.command)
    }

    /// The commands that only touch this machine run no startup check.
    #[test]
    fn a_command_that_never_leaves_this_machine_skips_the_startup_checks() {
        for argv in [
            &["completion", "bash"][..],
            &["completion", "zsh", "--install"],
            &["app", "validate", "Application.cue"],
            &["app", "scaffold", "--runtime", "bun", "--name", "web"],
            &["target", "list"],
            &["target", "use", "prod"],
            &["target", "show"],
            &["target", "rename", "a", "b"],
            &["target", "remove", "prod", "--yes"],
            &[
                "init",
                "--provider",
                "hetzner-cloud",
                "--tier",
                "solo",
                "--region",
                "fsn1",
            ],
            &["plan"],
            &["login"],
            &["upgrade-tier", "--to", "team"],
            &["auth", "status"],
        ] {
            assert!(!goes_out(argv), "{argv:?} never leaves this machine");
        }
    }

    /// Every command that talks to a cluster, a node or a provider keeps
    /// them — including one whose destination a flag decides.
    #[test]
    fn a_command_that_reaches_a_cluster_or_a_provider_keeps_the_startup_checks() {
        for argv in [
            &["platform", "status"][..],
            &["backup", "status"],
            &["backup", "list", "--local"],
            &[
                "backup",
                "prune",
                "--cluster-uid",
                "11111111-2222-3333-4444-555555555555",
            ],
            &["app", "list"],
            &["app", "status", "web"],
            &["secret", "list"],
            &["top"],
            &["kubeconfig"],
            &["whoami", "--no-ping"],
            &["target", "add", "prod"],
            &["target", "ip"],
            &["target", "machine"],
            &["target", "domain", "list"],
            &["apply"],
            &["destroy", "--yes"],
            &["doctor"],
        ] {
            assert!(goes_out(argv), "{argv:?} reaches beyond this machine");
        }
    }
}
