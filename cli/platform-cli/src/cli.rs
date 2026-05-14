// SPDX-License-Identifier: FSL-1.1-MIT
//! clap definitions for the `apprafter` CLI.

use clap::builder::BoolishValueParser;
use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(
    name = "apprafter",
    version,
    about = "AppRafter CLI: bootstrap and lifecycle of clusters.",
    long_about = None,
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Debug, Subcommand)]
pub enum Commands {
    /// Manage deployment targets — persistent named bundles of
    /// `(provider, region, credentials, defaults)`. One target is
    /// active at a time; operational commands (`apply`,
    /// `cluster-bootstrap`, …) act on it unless overridden.
    #[command(alias = "t")]
    Target {
        #[command(subcommand)]
        action: TargetCommand,
    },
    /// Bootstrap a fresh cluster on the given provider/tier.
    Init {
        /// Infrastructure provider identifier.
        #[arg(long)]
        provider: String,
        /// Deployment tier.
        #[arg(long)]
        tier: String,
        /// Provider-specific region.
        #[arg(long)]
        region: String,
    },
    /// Show the diff between the desired state and what is live.
    Plan,
    /// Apply the desired state.
    Apply,
    /// Print the current cluster status.
    Status,
    /// Obtain an OIDC-backed kubeconfig.
    Login,
    /// Upgrade the cluster from one tier to the next.
    #[command(name = "upgrade-tier")]
    UpgradeTier {
        /// Target tier (solo/team/prod/regulated).
        #[arg(long = "to")]
        to: String,
    },
    /// Destroy infrastructure managed by this state.
    Destroy {
        /// Confirm without prompting.
        #[arg(long, default_value_t = false)]
        yes: bool,
    },
    /// Rebuild local state from live Hetzner Cloud resources tagged
    /// with `apprafter=true`. Read-only — never deletes or creates.
    Import {
        /// Overwrite an already-populated `state.hetzner_cloud`.
        #[arg(long, default_value_t = false)]
        force: bool,
        /// Print what would be imported without writing state.
        #[arg(long = "dry-run", default_value_t = false)]
        dry_run: bool,
    },
    /// Print the cached k3s kubeconfig (decrypted), fetching it
    /// over SSH on first use. Intended pipe target: `KUBECONFIG=
    /// /dev/stdin kubectl ...`.
    Kubeconfig {
        /// Force a re-fetch over SSH even if a cached kubeconfig
        /// is already in state.
        #[arg(long, default_value_t = false)]
        refresh: bool,
    },
    /// Install Cilium (CNI + kube-proxy replacement) and apply
    /// the Gateway API standard-install CRDs into the cluster
    /// pointed to by the cached kubeconfig.
    #[command(name = "cluster-bootstrap")]
    ClusterBootstrap,
    /// Print the Argo CD admin password (decrypted), fetching it
    /// from the cluster on first use. The plaintext is cached
    /// age-encrypted in state for subsequent O(1) reads.
    #[command(name = "argocd-password")]
    ArgocdPassword {
        /// Force a re-fetch of the secret even if a cached
        /// password is already in state.
        #[arg(long, default_value_t = false)]
        refresh: bool,
    },
}

#[derive(Debug, Subcommand)]
pub enum TargetCommand {
    /// Add (or update, with `--renew`) a deployment target. The
    /// non-interactive form (current default — interactive wizard
    /// arrives in v0.1.74 / Track A.4) requires `--provider` plus
    /// any provider-specific flags such as `--token`.
    Add {
        /// Target name. Must match `[A-Za-z0-9-]+`, max 64 chars.
        /// The first target created on a fresh store is auto-set
        /// as the active target.
        name: String,
        /// Provider identifier. Only `hetzner-cloud` is wired in
        /// v0.1.73; AWS / Managed Cloud follow in later tracks.
        #[arg(long)]
        provider: Option<String>,
        /// Hetzner Cloud API token. Required when `--provider
        /// hetzner-cloud`. Format `hcloud_<64+ alphanumeric>`;
        /// passed via `--token` or env `HCLOUD_TOKEN` (the env
        /// fallback is for CI ergonomics — interactive use should
        /// prefer the flag so the token doesn't linger in shell
        /// history's env-leak surface).
        #[arg(long, env = "HCLOUD_TOKEN", hide_env_values = true)]
        token: Option<String>,
        /// Path to the SSH public key used for server provisioning.
        /// Stays a path (not the key body) so the user's `~/.ssh/`
        /// remains the source of truth.
        #[arg(long = "ssh-key", env = "APPRAFTER_SSH_PUBLIC_KEY_PATH")]
        ssh_key: Option<std::path::PathBuf>,
        /// Default provider region (e.g. Hetzner `nbg1`).
        #[arg(long)]
        region: Option<String>,
        /// Default tier identifier (`solo` / `team` / `prod` /
        /// `regulated`). Hint for `init` / `bootstrap-all`; can
        /// always be overridden per-command.
        #[arg(long)]
        tier: Option<String>,
        /// Default cluster name; falls back to `platform-1`.
        #[arg(long = "cluster-name")]
        cluster_name: Option<String>,
        /// Overwrite an existing target. Without `--force`, the
        /// command fails when the target name is taken.
        #[arg(long, default_value_t = false)]
        force: bool,
        /// Update only the credentials of an existing target.
        /// Errors when the target does not exist. Mutually
        /// exclusive with `--force` (use `--force` if you want
        /// to replace the whole target, not just rotate the
        /// token).
        #[arg(long, default_value_t = false, conflicts_with = "force")]
        renew: bool,
        /// Reserved for the upcoming interactive wizard (v0.1.74 /
        /// Track A.4). v0.1.73 only ships the non-interactive
        /// flag-driven path; accepting `--no-interactive` here is
        /// forward-compat — every Track A.3 invocation is already
        /// non-interactive regardless of TTY.
        #[arg(long = "no-interactive", default_value_t = false)]
        no_interactive: bool,
        /// Skip the Hetzner Cloud API ping that confirms the token
        /// authenticates (`GET /v1/locations`). Useful in CI when
        /// the network sandbox blocks outbound calls, or when
        /// pre-seeding a target store offline. v0.1.75 wired the
        /// ping in by default; this opt-out keeps the previous
        /// (format-only) behaviour available. Also honours
        /// `APPRAFTER_NO_PING=1` for shell-script ergonomics —
        /// any non-empty value flips the flag.
        #[arg(
            long = "no-ping",
            env = "APPRAFTER_NO_PING",
            default_value_t = false,
            value_parser = BoolishValueParser::new(),
        )]
        no_ping: bool,
    },
}
