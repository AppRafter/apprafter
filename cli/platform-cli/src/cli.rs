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
    /// One-line summary of the operator's current shell context:
    /// identity + active target + provider-verified status + key
    /// config fields. Pings the provider API by default; pass
    /// `--no-ping` to skip the network round-trip.
    Whoami {
        /// Skip the Hetzner Cloud API ping that confirms the
        /// active target's token still authenticates. Honours
        /// `APPRAFTER_NO_PING=1` for shell-script ergonomics.
        #[arg(
            long = "no-ping",
            env = "APPRAFTER_NO_PING",
            default_value_t = false,
            value_parser = BoolishValueParser::new(),
        )]
        no_ping: bool,
    },
    /// Reserved for AppRafter Cloud (Managed) authentication. Not
    /// available yet — subcommands print a friendly redirect to
    /// `apprafter target add`. Hidden from `--help` until Managed
    /// lands so it doesn't crowd the new-user discovery surface.
    #[command(hide = true)]
    Auth {
        #[command(subcommand)]
        action: AuthCommand,
    },
    /// Self-diagnostic. Walks the active target's stored config,
    /// credentials, and reachability checks plus the surrounding
    /// shell environment (kubectl, helm, ssh, DNS). Prints PASS /
    /// WARN / FAIL per check; exits 1 if any FAIL fires so CI
    /// gates can wire `apprafter doctor` in directly.
    Doctor {
        /// Inspect a target other than the active one. Defaults
        /// to the active target.
        #[arg(long)]
        target: Option<String>,
        /// Skip the Hetzner Cloud API ping. Honours
        /// `APPRAFTER_NO_PING=1` for shell-script ergonomics.
        #[arg(
            long = "no-ping",
            env = "APPRAFTER_NO_PING",
            default_value_t = false,
            value_parser = BoolishValueParser::new(),
        )]
        no_ping: bool,
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
    Apply {
        /// Inspect a target other than the active one for the
        /// resolution chain (`--token` flag / `HCLOUD_TOKEN` env
        /// / target store). Useful when scripting against
        /// multiple targets without `apprafter target use`.
        #[arg(long)]
        target: Option<String>,
    },
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
        /// Override the active target for the credential
        /// resolution chain (see `apprafter apply --target`).
        #[arg(long)]
        target: Option<String>,
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
        /// Override the active target for the credential
        /// resolution chain (see `apprafter apply --target`).
        #[arg(long)]
        target: Option<String>,
    },
    /// Print the cached k3s kubeconfig (decrypted), fetching it
    /// over SSH on first use. Intended pipe target: `KUBECONFIG=
    /// /dev/stdin kubectl ...`.
    Kubeconfig {
        /// Force a re-fetch over SSH even if a cached kubeconfig
        /// is already in state.
        #[arg(long, default_value_t = false)]
        refresh: bool,
        /// Override the active target for the credential
        /// resolution chain (see `apprafter apply --target`).
        #[arg(long)]
        target: Option<String>,
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
    /// One-command provisioning: runs `apply` → polls for the k3s
    /// kubeconfig to become SSH-reachable → runs `cluster-bootstrap`
    /// for a freshly-provisioned cluster. Convenience wrapper —
    /// each phase still has its own subcommand for re-runs.
    #[command(name = "bootstrap-all")]
    BootstrapAll {
        /// Override the active target for the credential resolution
        /// chain (see `apprafter apply --target`).
        #[arg(long)]
        target: Option<String>,
        /// Print the phase plan without touching the provider or
        /// cluster. Useful for previewing what the wrapper would
        /// invoke and which target it resolves against.
        #[arg(long = "dry-run", default_value_t = false)]
        dry_run: bool,
    },
}

#[derive(Debug, Subcommand)]
pub enum TargetCommand {
    /// Add (or update, with `--renew`) a deployment target.
    ///
    /// On a TTY without `--no-interactive` the command launches a
    /// wizard that prompts for whatever flags were not provided.
    /// On a non-TTY (CI, pipe) the command is purely flag-driven
    /// and missing required inputs error out fast.
    Add {
        /// Target name. Must match `[A-Za-z0-9-]+`, max 64 chars.
        /// The first target created on a fresh store is auto-set
        /// as the active target. Optional on a TTY — the wizard
        /// asks for it with a `default` default. Required on a
        /// non-TTY / `--no-interactive` run.
        name: Option<String>,
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
    /// List every configured target, marking the active one. Empty
    /// store prints an onboarding hint pointing at `target add`.
    List,
    /// Switch the active target. Fails when the named target does
    /// not exist; subsequent operational commands (`apply`,
    /// `cluster-bootstrap`, ...) act on the new active target
    /// once Track A.8 wires the resolution chain in.
    Use {
        /// Name of the target to make active.
        name: String,
    },
    /// Show details of a target (defaults to the active one). The
    /// stored token is summarised as `set` / `not set` without
    /// echoing the value; read `credentials.yaml` directly if you
    /// need the raw bytes.
    Show {
        /// Target name. Defaults to the active target.
        name: Option<String>,
    },
    /// Rename a target, moving its config + credentials + state
    /// cache to the new name. Updates `active_target` when needed.
    Rename {
        /// Source target name (must exist).
        from: String,
        /// Destination target name (must not exist).
        to: String,
    },
    /// Remove a target. Interactive runs prompt for confirmation
    /// unless `--yes` is passed; non-interactive runs always
    /// require `--yes` (no silent destruction).
    Remove {
        /// Target name to remove.
        name: String,
        /// Skip the confirmation prompt.
        #[arg(long, default_value_t = false)]
        yes: bool,
    },
}

/// `apprafter auth …` subcommands. All currently print the same
/// friendly redirect to `apprafter target add` (AppRafter Cloud
/// is not yet available). Kept as a `Subcommand` enum from day
/// one so the future Managed implementation can fill them in
/// without reshaping the CLI surface.
#[derive(Debug, Subcommand)]
pub enum AuthCommand {
    /// Authenticate to AppRafter Cloud (Managed). Stub until the
    /// Managed offering lands; today prints the self-hosted
    /// redirect.
    Login,
    /// Sign out of AppRafter Cloud (Managed). Stub.
    Logout,
    /// Report current AppRafter Cloud (Managed) authentication
    /// status. Stub.
    Status,
}
