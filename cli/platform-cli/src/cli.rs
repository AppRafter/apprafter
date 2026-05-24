// SPDX-License-Identifier: FSL-1.1-Apache-2.0
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
    #[command(alias = "kc")]
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
    #[command(name = "cluster-bootstrap", alias = "cb")]
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
    #[command(name = "bootstrap-all", alias = "up")]
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
    /// Inspect и control the cluster's PlatformStack — the
    /// declarative platform-version resource managed by
    /// PlatformController. Track B.1.79 thin wrapper.
    Platform {
        #[command(subcommand)]
        action: PlatformCommand,
    },
    /// Inspect и approve / reject MigrationPlans. Track B.1.79
    /// thin wrapper. Application-scope rejects denied by the
    /// admission webhook per ADR 0027 — surface the denial
    /// verbatim.
    Migration {
        #[command(subcommand)]
        action: MigrationCommand,
    },
    /// Open a platform UI (Argo CD today; Backstage / Grafana /
    /// Hubble follow в later sub-phases). Spawns a local
    /// port-forward, prints credentials, opens the default
    /// browser, blocks until Ctrl+C.
    Open {
        #[command(subcommand)]
        ui: OpenUi,
    },
    /// Manage user Applications — Argo CD Applications scoped к
    /// the `apps` AppProject, labeled `apprafter.io/managed-by:
    /// apprafter`. Track B.1.79a thin wrapper над Argo CD CR
    /// patching.
    #[command(alias = "a")]
    App {
        #[command(subcommand)]
        action: AppCommand,
    },
}

#[derive(Debug, Subcommand)]
pub enum PlatformCommand {
    /// Print PlatformStack/default summary — versions,
    /// conditions, recent history.
    Status,
    /// Patch PlatformStack.spec.pin. With `--to <version>` —
    /// pin к that version. Без `--to` — clear pin и enable
    /// autoUpgrade (channel-following mode).
    Upgrade {
        #[arg(long = "to")]
        to: Option<String>,
    },
}

#[derive(Debug, Subcommand)]
pub enum MigrationCommand {
    /// List MigrationPlans в the apprafter-system namespace
    /// — name, scope, classification, phase.
    #[command(alias = "ls")]
    List,
    /// Patch status.phase=approved on a MigrationPlan.
    /// MigrationController transitions через executing →
    /// completed.
    Approve {
        /// MigrationPlan name (as listed via `apprafter
        /// migration list`).
        name: String,
    },
    /// Patch status.phase=rejected. The admission webhook
    /// denies application-scope rejects per ADR 0027 — the
    /// CLI surfaces the denial message verbatim. Platform-
    /// scope rejects succeed и PlatformMigrationStrategy.reject
    /// reverts `spec.pin`.
    Reject {
        /// MigrationPlan name.
        name: String,
    },
}

#[derive(Debug, Subcommand)]
pub enum AppCommand {
    /// Register a user Application. Without `<git-url>`, detects
    /// the git origin remote of the current working directory.
    /// Writes an Argo CD Application CR в namespace `argocd`
    /// joined к AppProject `apps` (or `--project`), labeled
    /// `apprafter.io/managed-by: apprafter` so `app list` can
    /// surface only apprafter-managed Applications.
    Add {
        /// Explicit git URL. Required when cwd is not a git
        /// repo. Accepted forms: `https://…`, `git@host:org/repo.git`
        /// (SSH normalised к HTTPS), `ssh://git@host/repo`. Any
        /// trailing `.git` stripped когда normalising.
        git_url: Option<String>,
        /// Application name. Defaults к the repo's basename
        /// (last path segment after stripping `.git`).
        #[arg(long)]
        name: Option<String>,
        /// Git branch / tag / commit SHA passed verbatim к
        /// `spec.source.targetRevision`. Defaults к the cwd's
        /// current branch когда detected; `main` для explicit
        /// `<git-url>` без cwd context.
        #[arg(long)]
        branch: Option<String>,
        /// Path under the repo to render. Defaults к `/` —
        /// matches the cue-cmp plugin's `discover.find.glob:
        /// **/apprafter*.cue` rule (sidecar walks the entire
        /// repo for matching files when path is `/`).
        #[arg(long, default_value = "/")]
        path: String,
        /// AppProject the Application joins. Default `apps`
        /// matches the AppProject created в chart 0.1.40
        /// (Track B.1.79a part 1). Pass `--project platform`
        /// для platform-internal apps, `platform-providers`
        /// для ServiceProvider operators.
        #[arg(long, default_value = "apps")]
        project: String,
        /// Git remote name when detecting origin from cwd.
        /// Defaults к `origin`.
        #[arg(long, default_value = "origin")]
        remote: String,
        /// Skip the `git ls-remote` reachability check. Useful
        /// для CI где network isolation blocks external git
        /// hosts, or когда the repo is freshly created и does
        /// not yet have an `HEAD` ref.
        #[arg(long = "no-ping", default_value_t = false)]
        no_ping: bool,
    },
    /// List Applications scoped к the `apps` AppProject (or
    /// `--project <name>`). Filters к Applications labeled
    /// `apprafter.io/managed-by: apprafter` by default;
    /// `--all-managed` drops that filter (shows ALL Applications
    /// в the project regardless of management label — useful
    /// для debugging stray applications that bypassed `app add`).
    #[command(alias = "ls")]
    List {
        /// AppProject filter. Defaults к `apps`. Use
        /// `--all-projects` to drop the filter.
        #[arg(long, default_value = "apps")]
        project: String,
        /// Drop the `--project` filter; list Applications в
        /// EVERY AppProject (subject к the managed-by label
        /// filter unless `--all-managed` is also set).
        #[arg(
            long = "all-projects",
            default_value_t = false,
            conflicts_with = "project"
        )]
        all_projects: bool,
        /// Drop the `apprafter.io/managed-by: apprafter` label
        /// filter — list every Application в the resolved
        /// project scope, не just apprafter-managed ones.
        #[arg(long = "all-managed", default_value_t = false)]
        all_managed: bool,
    },
    /// Show detail view для one Application: sync state, health,
    /// source repo + revision, destinations, pending
    /// MigrationPlans (when AppRafter `Application` CR exists
    /// в the workload namespace), recent sync history (last 3
    /// revisions).
    Status {
        /// Application name (as listed via `apprafter app list`).
        name: String,
    },
    /// Delete an Application и cascade-remove the Argo CD CR
    /// (which Argo CD then tears down child resources for).
    /// Interactive: prompts для confirmation; non-interactive
    /// requires `--yes` to skip the prompt.
    #[command(alias = "rm")]
    Remove {
        /// Application name.
        name: String,
        /// Skip confirmation prompt. Required в non-interactive
        /// shells (no TTY) — there's no silent destruction
        /// path.
        #[arg(long, default_value_t = false)]
        yes: bool,
        /// Preserve PVCs / `ResourceClaim`s when tearing down.
        /// Implemented as а post-delete cleanup that strips
        /// the destructive child-prune from the cascading
        /// delete. Phase 2 ServiceProvider-backed claims need
        /// this to survive а user app teardown when the
        /// operator wants к re-attach the data later.
        #[arg(long = "keep-data", default_value_t = false)]
        keep_data: bool,
    },
}

#[derive(Debug, Subcommand)]
pub enum OpenUi {
    /// Open the Argo CD web UI on `https://localhost:8080`
    /// via `kubectl port-forward`, prefilling the admin
    /// username и printing the password.
    Argocd {
        /// AppProject filter applied к the opened URL —
        /// Argo CD's UI honours `?proj=<name>` to scope the
        /// Applications list. Defaults к `apps` so the
        /// operator lands on their own apps first; pass
        /// `--project platform` чтобы посмотреть
        /// chart-managed compoments, или `--all-projects`
        /// to drop the filter entirely.
        #[arg(long = "project", default_value = "apps")]
        project: String,
        /// Drop the `?proj=<name>` filter — UI shows all
        /// AppProjects.
        #[arg(
            long = "all-projects",
            default_value_t = false,
            conflicts_with = "project"
        )]
        all_projects: bool,
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
    #[command(alias = "ls")]
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
    #[command(alias = "info")]
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
    #[command(alias = "rm")]
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
