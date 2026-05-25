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
    /// Inspect and control the cluster's PlatformStack — the
    /// declarative platform-version resource managed by
    /// PlatformController. Track B.1.79 thin wrapper.
    Platform {
        #[command(subcommand)]
        action: PlatformCommand,
    },
    /// Inspect and approve / reject MigrationPlans. Track B.1.79
    /// thin wrapper. Application-scope rejects denied by the
    /// admission webhook per ADR 0027 — surface the denial
    /// verbatim.
    Migration {
        #[command(subcommand)]
        action: MigrationCommand,
    },
    /// Open a platform UI (Argo CD today; Backstage / Grafana /
    /// Hubble follow in later sub-phases). Spawns a local
    /// port-forward, prints credentials, opens the default
    /// browser, blocks until Ctrl+C.
    Open {
        #[command(subcommand)]
        ui: OpenUi,
    },
    /// Manage user Applications — Argo CD Applications scoped to
    /// the `apps` AppProject, labeled `apprafter.io/managed-by:
    /// apprafter`. Track B.1.79a thin wrapper over Argo CD CR
    /// patching.
    #[command(alias = "a")]
    App {
        #[command(subcommand)]
        action: AppCommand,
    },
    /// Manage git-repo creds Argo CD uses to pull private user
    /// repos. Writes `repo-creds`-typed Secrets in the `argocd`
    /// namespace per Argo CD's documented contract. Track
    /// B.1.79a part 5.
    Repo {
        #[command(subcommand)]
        action: RepoCommand,
    },
}

#[derive(Debug, Subcommand)]
pub enum RepoCommand {
    /// Repository credentials subcommands.
    Creds {
        #[command(subcommand)]
        action: RepoCredsCommand,
    },
}

#[derive(Debug, Subcommand)]
pub enum RepoCredsCommand {
    /// Register a git-repo credential. Creates an Argo CD
    /// `repo-creds`-typed Secret in the `argocd` namespace.
    /// All Applications with a `repoURL` starting with the
    /// registered `--url-prefix` inherit these creds.
    Add {
        /// Friendly name. Used as the Secret's
        /// `metadata.name` (DNS-1123) and surfaces in
        /// `apprafter repo creds list`.
        name: String,
        /// URL prefix for which these creds apply (e.g.
        /// `https://github.com/myorg`). Required.
        #[arg(long = "url-prefix")]
        url_prefix: String,
        /// Auth type. Default `pat` — a personal access
        /// token (GitHub `github_pat_*` / `ghp_*`, GitLab
        /// `glpat-*`, etc). `basic` — username + password
        /// pair. SSH-key auth deferred to Phase 2 (Argo CD
        /// supports it, but CLI prompts get involved).
        #[arg(long = "type", default_value = "pat")]
        auth_type: String,
        /// Username. When `--type pat` — usually the token
        /// holder's git username (GitHub: any non-empty
        /// string works; GitLab requires the username).
        /// Defaults to `git` for PAT auth which works
        /// across most providers.
        #[arg(long, default_value = "git")]
        username: String,
        /// Token / password. Required. Reads from stdin via
        /// `inquire::Password` (masked entry) when
        /// omitted and stdin is a TTY; required as flag in
        /// non-interactive shells.
        #[arg(long, env = "APPRAFTER_REPO_TOKEN", hide_env_values = true)]
        token: Option<String>,
        /// Skip provider-specific token format regex check
        /// (GitHub: `github_pat_*` / `ghp_*`; GitLab:
        /// `glpat-*`). Useful for self-hosted Gitea / Forgejo
        /// where token formats are arbitrary.
        #[arg(long = "no-validate", default_value_t = false)]
        no_validate: bool,
        /// Skip the interactive wizard even when stdin + stdout
        /// are TTYs. Wizard fires by default and walks through
        /// name / URL prefix / type / username / token (token
        /// prompt is masked).
        #[arg(long = "no-interactive", default_value_t = false)]
        no_interactive: bool,
    },
    /// List registered creds.
    #[command(alias = "ls")]
    List,
    /// Show a creds entry; token is masked.
    Show {
        /// Creds name (as listed via `repo creds list`).
        name: String,
    },
    /// Rotate a creds entry's token in-place. Patches the
    /// existing Secret rather than recreating it — Argo CD
    /// repo-server holds a cached reference to the Secret's
    /// resourceVersion and a recreate would cause a brief
    /// reconnect window.
    Rotate {
        /// Creds name.
        name: String,
        /// New token. Reads from stdin (masked) when
        /// omitted and stdin is a TTY.
        #[arg(long, env = "APPRAFTER_REPO_TOKEN", hide_env_values = true)]
        token: Option<String>,
        /// Skip token format validation. See `repo creds add
        /// --no-validate`.
        #[arg(long = "no-validate", default_value_t = false)]
        no_validate: bool,
    },
    /// Delete a creds entry. Refuses by default when
    /// Applications depending on the `urlPrefix` are
    /// registered; `--force` overrides.
    #[command(alias = "rm")]
    Remove {
        /// Creds name.
        name: String,
        /// Skip confirmation + the dependency check.
        #[arg(long, default_value_t = false)]
        force: bool,
        /// Skip confirmation prompt only (still runs
        /// dependency check).
        #[arg(long, default_value_t = false)]
        yes: bool,
    },
}

#[derive(Debug, Subcommand)]
pub enum PlatformCommand {
    /// Print PlatformStack/default summary — versions,
    /// conditions, recent history.
    Status,
    /// Patch PlatformStack.spec.pin. With `--to <version>` —
    /// pin to that version. Without `--to` — clear pin and enable
    /// autoUpgrade (channel-following mode).
    Upgrade {
        #[arg(long = "to")]
        to: Option<String>,
    },
    /// Freeze a specific component's version through
    /// `PlatformStack.spec.overrides.<component>.pin`. Useful
    /// for out-of-band security backports or when the
    /// curated bundle's pinned version regresses a workload-
    /// specific shape. Without `--version` — uses the component's
    /// current effective version (read from status), reading the
    /// chart's own pin and locking that in.
    Freeze {
        /// Component name (must match a key in the umbrella
        /// chart's `values.components` map; e.g. `cilium`,
        /// `argocd`, `cert-manager`, `apprafter-operator`).
        component: String,
        /// Pin version. Omit to lock the current effective
        /// version (read from `status.componentVersions`).
        #[arg(long = "version")]
        version: Option<String>,
    },
    /// Remove a previously-set
    /// `PlatformStack.spec.overrides.<component>` entry —
    /// component falls back to the umbrella chart's curated pin.
    Unfreeze {
        /// Component name.
        component: String,
    },
    /// Emergency recovery: re-run the loader's cluster-bootstrap
    /// path (Cilium → Argo CD → CRDs → operator) against the
    /// active target. Useful when Argo CD itself is unable to
    /// self-adopt — a stale chart, a corrupted ConfigMap, or
    /// a pod-eviction loop that no `apprafter platform upgrade`
    /// can resolve. Thin wrapper around
    /// `apprafter cluster-bootstrap` with the recovery banner.
    Rescue {
        /// Skip confirmation prompt. Required in non-interactive
        /// shells.
        #[arg(long, default_value_t = false)]
        yes: bool,
    },
}

#[derive(Debug, Subcommand)]
pub enum MigrationCommand {
    /// List MigrationPlans in the apprafter-system namespace
    /// — name, scope, classification, phase.
    #[command(alias = "ls")]
    List,
    /// Patch status.phase=approved on a MigrationPlan.
    /// MigrationController transitions through executing →
    /// completed.
    Approve {
        /// MigrationPlan name (as listed via `apprafter
        /// migration list`).
        name: String,
    },
    /// Patch status.phase=rejected. The admission webhook
    /// denies application-scope rejects per ADR 0027 — the
    /// CLI surfaces the denial message verbatim. Platform-
    /// scope rejects succeed and PlatformMigrationStrategy.reject
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
    /// Writes an Argo CD Application CR in namespace `argocd`
    /// joined to AppProject `apps` (or `--project`), labeled
    /// `apprafter.io/managed-by: apprafter` so `app list` can
    /// surface only apprafter-managed Applications.
    Add {
        /// Explicit git URL. Required when cwd is not a git
        /// repo. Accepted forms: `https://…`, `git@host:org/repo.git`
        /// (SSH normalised to HTTPS), `ssh://git@host/repo`. Any
        /// trailing `.git` stripped when normalising.
        git_url: Option<String>,
        /// Application name. Defaults to the repo's basename
        /// (last path segment after stripping `.git`).
        #[arg(long)]
        name: Option<String>,
        /// Git branch / tag / commit SHA passed verbatim to
        /// `spec.source.targetRevision`. Defaults to the cwd's
        /// current branch when detected; `main` for explicit
        /// `<git-url>` without cwd context.
        #[arg(long)]
        branch: Option<String>,
        /// Path under the repo to render. Defaults to `/` —
        /// matches the cue-cmp plugin's `discover.find.glob:
        /// **/apprafter*.cue` rule (sidecar walks the entire
        /// repo for matching files when path is `/`).
        #[arg(long, default_value = "/")]
        path: String,
        /// AppProject the Application joins. Default `apps`
        /// matches the AppProject created in chart 0.1.40
        /// (Track B.1.79a part 1). Pass `--project platform`
        /// for platform-internal apps, `platform-providers`
        /// for ServiceProvider operators.
        #[arg(long, default_value = "apps")]
        project: String,
        /// Destination namespace — Argo CD's
        /// `spec.destination.namespace`. With `CreateNamespace=
        /// true` in syncOptions, Argo CD creates this namespace
        /// on first sync if it doesn't exist. Default `apprafter`
        /// matches the namespace where AppRafter operator watches
        /// for Application CRs and where landing-web /
        /// landing-cms manifests declare their CR. Walk-fix #12
        /// (v0.1.160) replaced the prior `<app-name>` default
        /// which created an orphan destination namespace
        /// mismatched with the manifest's own metadata.namespace.
        #[arg(long = "namespace", default_value = "apprafter")]
        namespace: String,
        /// Git remote name when detecting origin from cwd.
        /// Defaults to `origin`.
        #[arg(long, default_value = "origin")]
        remote: String,
        /// Skip the `git ls-remote` reachability check. Useful
        /// for CI where network isolation blocks external git
        /// hosts, or when the repo is freshly created and does
        /// not yet have an `HEAD` ref.
        #[arg(long = "no-ping", default_value_t = false)]
        no_ping: bool,
        /// Skip the interactive wizard even when stdin + stdout
        /// are TTYs. The wizard fires by default on TTY shells
        /// and asks for any field not supplied via flag,
        /// pre-filling defaults from the cwd's git remote + the
        /// current branch where available.
        #[arg(long = "no-interactive", default_value_t = false)]
        no_interactive: bool,
    },
    /// List Applications scoped to the `apps` AppProject (or
    /// `--project <name>`). Filters to Applications labeled
    /// `apprafter.io/managed-by: apprafter` by default;
    /// `--all-managed` drops that filter (shows ALL Applications
    /// in the project regardless of management label — useful
    /// for debugging stray applications that bypassed `app add`).
    #[command(alias = "ls")]
    List {
        /// AppProject filter. Defaults to `apps`. Use
        /// `--all-projects` to drop the filter.
        #[arg(long, default_value = "apps")]
        project: String,
        /// Drop the `--project` filter; list Applications in
        /// EVERY AppProject (subject to the managed-by label
        /// filter unless `--all-managed` is also set).
        #[arg(
            long = "all-projects",
            default_value_t = false,
            conflicts_with = "project"
        )]
        all_projects: bool,
        /// Drop the `apprafter.io/managed-by: apprafter` label
        /// filter — list every Application in the resolved
        /// project scope, not just apprafter-managed ones.
        #[arg(long = "all-managed", default_value_t = false)]
        all_managed: bool,
    },
    /// Show detail view for one Application: sync state, health,
    /// source repo + revision, destinations, recent sync history
    /// (last 3 revisions). With `--resources`: additionally lists
    /// Argo CD's tracked resources + workload pod states (READY,
    /// STATUS, RESTARTS, AGE) — surfaces image-pull / crash-loop
    /// issues that Argo CD's app-level Healthy aggregation hides
    /// when the operator marks the CR `phase=Ready` before pods
    /// reach Running.
    Status {
        /// Application name (as listed via `apprafter app list`).
        name: String,
        /// Show child workload state — Argo CD's
        /// `status.resources[]` plus pods в the destination
        /// namespace matching `app.kubernetes.io/name=<inner-
        /// app-name>` (the AppRafter operator's label). Walk-fix
        /// #3 post-B.1.79b closure of §1.79a line 2257.
        #[arg(long, short = 'r', default_value_t = false)]
        resources: bool,
    },
    /// Stream logs from the app's workload pods. Wraps
    /// `kubectl logs` with a label selector derived from the
    /// Argo CD `Application` (the workload namespace is a
    /// known property of the CR). Default: aggregate across
    /// pods. `--pod <name>` narrows to a single pod;
    /// `--container <c>` picks the container in multi-container
    /// pods; `--follow` enables tail; `--tail <N>` caps the
    /// initial backlog.
    Logs {
        /// Application name.
        name: String,
        /// Stream new log lines as they appear (`kubectl logs
        /// -f`).
        #[arg(short = 'f', long, default_value_t = false)]
        follow: bool,
        /// Show only the last `N` lines per pod / container
        /// (`kubectl logs --tail`). `-1` means no limit
        /// (`kubectl`'s default).
        #[arg(long, default_value_t = -1)]
        tail: i64,
        /// Pick a specific container in multi-container pods.
        /// Without the flag `kubectl` fails on a pod with two
        /// containers (requires an explicit choice); the CLI
        /// surface proxies that error verbatim.
        #[arg(long)]
        container: Option<String>,
        /// Narrow to a single pod instead of all matching the
        /// app's destination namespace.
        #[arg(long)]
        pod: Option<String>,
    },
    /// Roll back to a previous revision. Reads
    /// `status.history` from the Argo CD `Application`,
    /// patches `spec.source.targetRevision` to the target;
    /// Argo CD's auto-sync picks up the change on the next
    /// reconcile cycle. Without `--to` — rollback to the
    /// previous entry in history (offset -1).
    Rollback {
        /// Application name.
        name: String,
        /// Explicit revision (commit SHA / tag / branch).
        /// Without the flag: previous entry in `status.history`.
        #[arg(long = "to")]
        to: Option<String>,
        /// Skip confirmation prompt. Required in non-interactive
        /// shells.
        #[arg(long, default_value_t = false)]
        yes: bool,
    },
    /// Generate а starter `apprafter/Application.cue` based
    /// on the cwd's runtime markers (bun.lock / Cargo.toml /
    /// pyproject.toml / etc.). Writes to `<--path>/apprafter/
    /// Application.cue` and appends `.apprafter/local/` к the
    /// repo's `.gitignore` when present. Refuses к overwrite
    /// an existing manifest without `--force`. Track B.1.79b
    /// Part 3.
    Scaffold {
        /// Force-pick а runtime instead of detecting from
        /// cwd. Slugs: bun, node-pnpm, node-yarn, node-npm,
        /// python-poetry, python-uv, python-pipenv, python-
        /// pip, rust, go, docker, blank.
        #[arg(long)]
        runtime: Option<String>,
        /// Application name (DNS-1123 lowercase). Default =
        /// cwd basename.
        #[arg(long)]
        name: Option<String>,
        /// Destination namespace для `metadata.namespace`.
        /// Default `apprafter` matches `app add --namespace`.
        #[arg(long = "namespace")]
        namespace: Option<String>,
        /// Working directory the scaffold writes into.
        /// Default = current cwd.
        #[arg(long, default_value = ".")]
        path: std::path::PathBuf,
        /// Overwrite an existing `apprafter/Application.cue`.
        #[arg(long, default_value_t = false)]
        force: bool,
    },
    /// Port-forward the app's primary Service to localhost and
    /// open it in а browser. Wraps `kubectl port-forward` with
    /// AppRafter-aware resolution: Application name → Argo CD
    /// CR → `spec.destination.namespace` → child Service via
    /// the `app.kubernetes.io/instance=<name>` label →
    /// container port (Service.spec.ports[0] OR
    /// `--container-port` override). Picks а free local port
    /// starting at 8080 with auto-increment to 8090 if busy.
    /// Blocks on Ctrl+C — the port-forward dies with the
    /// command. Walk-fix #1 post-B.1.79 (Go SIGPIPE drainer
    /// pattern) is inherited from `commands::port_forward`.
    Open {
        /// Application name (as listed via `apprafter app list`).
        name: String,
        /// Local port к bind. Defaults to 8080; if busy, the
        /// command probes 8081…8090 before giving up.
        #[arg(long)]
        port: Option<u16>,
        /// Container port к forward к. Defaults к the
        /// Service's first declared `spec.ports[]` entry.
        /// Required when the Service declares no ports or when
        /// the operator wants а secondary port.
        #[arg(long = "container-port")]
        container_port: Option<u16>,
        /// Skip opening the browser; just print the URL and
        /// block on the port-forward. Useful for CI / scripts
        /// that want to forward в the background.
        #[arg(long = "no-browser", default_value_t = false)]
        no_browser: bool,
    },
    /// Delete an Application and cascade-remove the Argo CD CR
    /// (which Argo CD then tears down child resources for).
    /// Interactive: prompts for confirmation; non-interactive
    /// requires `--yes` to skip the prompt.
    #[command(alias = "rm")]
    Remove {
        /// Application name.
        name: String,
        /// Skip confirmation prompt. Required in non-interactive
        /// shells (no TTY) — there's no silent destruction
        /// path.
        #[arg(long, default_value_t = false)]
        yes: bool,
        /// Preserve PVCs / `ResourceClaim`s when tearing down.
        /// Implemented as a post-delete cleanup that strips
        /// the destructive child-prune from the cascading
        /// delete. Phase 2 ServiceProvider-backed claims need
        /// this to survive a user app teardown when the
        /// operator wants to re-attach the data later.
        #[arg(long = "keep-data", default_value_t = false)]
        keep_data: bool,
    },
}

#[derive(Debug, Subcommand)]
pub enum OpenUi {
    /// Open the Argo CD web UI on `https://localhost:8080`
    /// via `kubectl port-forward`, prefilling the admin
    /// username and printing the password.
    Argocd {
        /// AppProject filter applied to the opened URL —
        /// Argo CD's UI honours `?proj=<name>` to scope the
        /// Applications list. Defaults to `apps` so the
        /// operator lands on their own apps first; pass
        /// `--project platform` to inspect chart-managed
        /// components, or `--all-projects` to drop the
        /// filter entirely.
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
