// SPDX-License-Identifier: FSL-1.1-MIT
//! clap definitions for `platform-cli`.

use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(
    name = "platform-cli",
    version,
    about = "AppRafter platform CLI: bootstrap and lifecycle of clusters.",
    long_about = None,
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Debug, Subcommand)]
pub enum Commands {
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
}
