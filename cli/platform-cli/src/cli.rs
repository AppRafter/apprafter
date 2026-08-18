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
        /// active target's token still authenticates. Also settable
        /// via `APPRAFTER_NO_PING`, which takes a boolish value:
        /// `1` `true` `yes` `y` `t` `on` skip the ping, `0` `false`
        /// `no` `n` `f` `off` keep it. Any other value, including
        /// the empty string, is an error rather than a no-op.
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
    /// Self-diagnostic over the active target's config, credentials
    /// and reachability plus the surrounding shell environment
    /// (kubectl, helm, ssh, DNS). Prints PASS / WARN / FAIL per
    /// check; exits 1 if any FAIL fires so CI gates can wire
    /// `apprafter doctor` in directly.
    Doctor {
        /// Inspect a target other than the active one. Defaults
        /// to the active target.
        #[arg(long)]
        target: Option<String>,
        /// Skip the Hetzner Cloud API ping. Also settable via
        /// `APPRAFTER_NO_PING`, which takes a boolish value: `1`
        /// `true` `yes` `y` `t` `on` skip the ping, `0` `false` `no`
        /// `n` `f` `off` keep it. Any other value, including the
        /// empty string, is an error rather than a no-op.
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
        /// Server type (SKU) to provision (e.g. `cx22`, `cx32`). Resolution:
        /// this flag > manifest `spec.nodes[0].type` > recorded state > target
        /// default > `APPRAFTER_SERVER_TYPE`. There is NO implicit default —
        /// if none is set, provisioning a new cluster fails with
        /// `apprafter::provider::server_type_not_selected` (pick one via the
        /// `target add` picker or `apprafter target machine`).
        #[arg(long = "server-type")]
        server_type: Option<String>,
    },
    // The caveat sits in the FIRST sentence of each of these three,
    // not below it: the generated reference uses the lead sentence as
    // the overview-table summary and the page description, so a
    // caveat in sentence two is invisible exactly where a reader
    // decides whether the command does what they need.
    /// Print the current cluster status — SKELETON, it reads local
    /// state and never contacts the cluster. What it prints: the
    /// active target, cluster name, tier and provider recorded in that
    /// target's state file, `<config-root>/state/<target>/.apprafter/state.json`.
    /// For live state use `apprafter platform status` (platform
    /// components and versions) or `apprafter app status <name>` (a
    /// workload).
    Status,
    /// Obtain an OIDC-backed kubeconfig — NOT IMPLEMENTED, it prints
    /// what it would do and writes nothing. The device flow is not
    /// wired up in this release; to get a working kubeconfig today run
    /// `apprafter kubeconfig`, which fetches the cluster's admin
    /// credentials into the age-encrypted local cache.
    Login,
    /// Upgrade the cluster from one tier to the next — NOT
    /// IMPLEMENTED, it validates `--to` and prints the move it would
    /// make. No infrastructure and no platform config change; the tier
    /// is chosen at `apprafter init` time until this lands.
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
        /// Server type (SKU) to provision (e.g. `cx22`, `cx32`). Forwarded
        /// to the `apply` phase of bootstrap-all. Resolution: this flag >
        /// manifest `spec.nodes[0].type` > recorded state > target default >
        /// `APPRAFTER_SERVER_TYPE`. There is NO implicit default — if none is
        /// set, provisioning fails with
        /// `apprafter::provider::server_type_not_selected`.
        #[arg(long = "server-type")]
        server_type: Option<String>,
    },
    /// Inspect and control the cluster's PlatformStack — the
    /// declarative platform-version resource managed by
    /// PlatformController.
    Platform {
        #[command(subcommand)]
        action: PlatformCommand,
    },
    /// Inspect and approve / reject MigrationPlans. Application-scope
    /// rejects denied by the admission webhook — the CLI surfaces
    /// the denial message verbatim.
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
    /// apprafter`.
    #[command(alias = "a")]
    App {
        #[command(subcommand)]
        action: AppCommand,
    },
    /// Manage git-repo creds Argo CD uses to pull private user
    /// repos. Writes `repo-creds`-typed Secrets in the `argocd`
    /// namespace per Argo CD's documented contract.
    Repo {
        #[command(subcommand)]
        action: RepoCommand,
    },
    /// Seal secret material with the in-cluster sealed-secrets
    /// controller's public cert. The CLI cannot decrypt what it
    /// seals (no cluster private key), so the output is safe to
    /// print, commit to Git, or apply. Backs the private-repo
    /// credential flow.
    Secret {
        #[command(subcommand)]
        action: SecretCommand,
    },
    /// Manage SharedVolume CRs — persistent volumes shared across
    /// multiple Applications. Volumes are namespaced;
    /// `rm` is refused while any Application references the volume.
    Volume {
        #[command(subcommand)]
        action: VolumeCommand,
    },
    /// Node-level operations on the active target's cluster node.
    /// `prep` retrofits the kubelet node reservations + k3s
    /// OOM-protection AND provisions host swap (NoSwap for pods) over
    /// one k3s restart; `status` reports the node's swap posture.
    Node {
        #[command(subcommand)]
        action: NodeAction,
    },
    /// Native data export (Kind 1) — pull pg dumps, volume tars, redis
    /// snapshots to a plain local folder + `manifest.json`. No CRs, no
    /// secrets, no encryption. A debugging / one-off-recovery convenience.
    Export {
        /// Narrow scope to these namespaces (repeatable). Ignored unless
        /// `--select` is also passed.
        #[arg(long)]
        namespace: Vec<String>,
        /// When set, only the namespaces given via `--namespace` are
        /// exported. Without `--select` the scope is the whole cluster.
        #[arg(long, default_value_t = false)]
        select: bool,
        /// Output directory. Defaults to `./apprafter-export`.
        #[arg(long)]
        out: Option<String>,
    },
    /// Encrypted backup (Kind 2, restic local-pull): native extraction +
    /// serialized config/app CRs + decrypted user secrets, all wrapped
    /// into a restic repository. Use `apprafter restore` to replay.
    Backup {
        #[command(subcommand)]
        action: BackupAction,
    },
    /// Restore a backup into a target cluster: replays the CRs, secrets and
    /// native data (pg/redis/volumes) captured by `apprafter backup`. Modes:
    /// restore-into-running (default; the target must already be bootstrapped),
    /// `--data-only` (reload native data only, no CR/secret replay), and
    /// `--reprovision` (provision a fresh cluster first, then replay). Secrets
    /// are re-sealed against the target cluster's sealed-secrets key.
    Restore {
        /// Path to the restic repository created by `apprafter backup`.
        repo: String,
        /// Target name to restore into (defaults to the active target).
        #[arg(long)]
        target: Option<String>,
        /// Re-provision the target cluster before replaying data.
        #[arg(long, default_value_t = false)]
        reprovision: bool,
        /// Snapshot id to restore (defaults to the latest).
        #[arg(long)]
        snapshot: Option<String>,
        /// Replay only native data (pg/redis/volumes); skip CR + secret
        /// replay. Useful when the cluster is already configured.
        #[arg(long, default_value_t = false)]
        data_only: bool,
        /// Passphrase for the restic repo. Falls back to
        /// `RESTIC_PASSWORD`; prompts interactively on a TTY.
        #[arg(long)]
        passphrase: Option<String>,
        /// Path to a dotenv credential file containing the operator's
        /// S3 credentials (`AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY`,
        /// `RESTIC_PASSWORD`, optional `AWS_DEFAULT_REGION`). REQUIRED
        /// to restore from a remote `s3:`/`b2:`/`gs:`/`azure:` repo
        /// (falls back to the matching env vars); the operator's full
        /// creds are read locally, NEVER from the cluster.
        #[arg(long)]
        credential_file: Option<std::path::PathBuf>,
        /// Server type (SKU) to use when `--reprovision` is set (e.g. `cx22`,
        /// `cx32`). Forwarded to the `apply` phase. Resolution: this flag >
        /// manifest `spec.nodes[0].type` > recorded state > target default >
        /// `APPRAFTER_SERVER_TYPE`. There is NO implicit default — if none is
        /// set, provisioning fails with
        /// `apprafter::provider::server_type_not_selected`.
        #[arg(long = "server-type")]
        server_type: Option<String>,
    },
}

#[derive(Debug, Subcommand)]
pub enum SecretCommand {
    /// Seal one or more `--from-literal KEY=VALUE` pairs into a
    /// bitnami `SealedSecret`. Fetches the controller's public
    /// cert over the TLS-authenticated kube API (strict scope:
    /// the sealed blob only unseals as `<namespace>/<name>`).
    ///
    /// Re-sealing an existing name REPLACES its keys — it does not
    /// merge; pass all keys in one command. Use `--yes` to skip the
    /// overwrite prompt.
    Seal {
        /// SealedSecret `metadata.name` (and the resulting
        /// Secret's name). DNS-1123.
        name: String,
        /// Repeatable `KEY=VALUE`. The value may be empty or
        /// contain `=` (only the first `=` splits the pair).
        #[arg(long = "from-literal", required = true)]
        from_literal: Vec<String>,
        /// Target namespace. Defaults to `apprafter-system`,
        /// where platform credential material lives.
        #[arg(long, short = 'n', default_value = "apprafter-system")]
        namespace: String,
        /// Resulting Secret `type` (e.g. `Opaque`,
        /// `kubernetes.io/dockerconfigjson`).
        #[arg(long = "type", default_value = "Opaque")]
        secret_type: String,
        /// Print the SealedSecret YAML to stdout instead of
        /// applying it (for `kubectl apply -f -` or committing
        /// to a config repo).
        #[arg(long, default_value_t = false)]
        stdout: bool,
        /// Skip the overwrite confirmation prompt when a secret
        /// of the same name already exists. In non-interactive
        /// shells this flag is required to replace an existing
        /// secret (without it the command errors instead of
        /// silently overwriting).
        #[arg(long, default_value_t = false)]
        yes: bool,
    },
    /// Delete a `SealedSecret` and the `Secret` the controller
    /// unsealed from it, by name — so you don't `kubectl delete
    /// sealedsecret,secret` by hand. Idempotent (absent objects
    /// are fine). Prompts unless `--yes`.
    Remove {
        /// SealedSecret / Secret `metadata.name`. DNS-1123.
        name: String,
        /// Target namespace. Defaults to `apprafter-system` (matches
        /// `secret seal`); pass the app's namespace for app secrets.
        #[arg(long, short = 'n', default_value = "apprafter-system")]
        namespace: String,
        /// Skip the confirmation prompt.
        #[arg(long, default_value_t = false)]
        yes: bool,
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
        /// pair. SSH-key auth is not supported yet.
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
    /// conditions, recent history. By default first asks the
    /// operator for an immediate upstream re-check (stamps
    /// `apprafter.io/recheck-requested` and waits up to 30s for
    /// `status.lastUpstreamCheck` to advance) so the reported
    /// `available` version is fresh rather than up to 6h stale.
    Status {
        /// Skip the upstream re-check and render the last-known
        /// status immediately (no wait). For scripting / when the
        /// 6h-cadence snapshot is good enough.
        #[arg(long, default_value_t = false)]
        cached: bool,
    },
    /// Patch PlatformStack.spec.pin. With `--to <version>` —
    /// pin to that version. Without `--to` — clear pin and enable
    /// autoUpgrade (channel-following mode). By default forces a
    /// fresh upstream re-check first (see `platform status`) so the
    /// upgrade decision acts on the latest availableVersion.
    Upgrade {
        /// Platform-stack version to pin, e.g. `0.2.33` — written to
        /// `PlatformStack.spec.pin`, which the operator then holds the
        /// cluster at. Omit to clear the pin and follow the channel
        /// (autoUpgrade).
        #[arg(long = "to", value_name = "version")]
        to: Option<String>,
        /// Skip the upstream re-check and act on the last-known
        /// status immediately (no wait).
        #[arg(long, default_value_t = false)]
        cached: bool,
    },
    /// Freeze a specific component's version through
    /// `PlatformStack.spec.overrides.<component>.pin`. Useful
    /// for out-of-band security backports or when the curated
    /// bundle's pinned version regresses a workload-specific
    /// shape. Without `--version` — uses the component's
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
    /// Inspect / set the cluster-wide egress posture
    /// (`PlatformStack.spec.network.egress.profile`). Gates which
    /// baseline egress allows the operator derives per Application
    /// (DNS / same-namespace / world / declared needs).
    Egress {
        #[command(subcommand)]
        action: EgressCommand,
    },
    /// Show or set the cluster's soft default environment.
    Env {
        #[command(subcommand)]
        action: EnvCommand,
    },
    /// Inspect or set the cluster-wide VPA autoscale mode
    /// (`PlatformStack.spec.resources.autoscale.mode`). Controls
    /// whether the Vertical Pod Autoscaler applies recommendations to
    /// Application pods automatically.
    Autoscale {
        #[command(subcommand)]
        action: AutoscaleCommand,
    },
}

/// `apprafter platform env …` subcommands.
#[derive(Debug, Subcommand)]
pub enum EnvCommand {
    /// Show the cluster's default environment.
    Show,
    /// Set the cluster's default environment (a soft `app add` preselect).
    Set {
        /// Environment name (e.g. prod, staging).
        env: String,
    },
}

/// `apprafter platform autoscale …` subcommands.
#[derive(Debug, Subcommand)]
pub enum AutoscaleCommand {
    /// Show the cluster's current VPA autoscale mode and available presets.
    Show,
    /// Set the cluster-wide VPA autoscale mode.
    Set {
        /// One of `full` (apply up and down), `up-only` (scale up only),
        /// or `off` (record recommendations but do not apply them).
        mode: String,
    },
}

#[derive(Debug, Subcommand)]
pub enum EgressCommand {
    /// Print the current egress profile and what each profile
    /// allows. Reads the singleton PlatformStack; an absent field
    /// reports the documented default (`internet`).
    Show,
    /// Set the cluster-wide egress profile via server-side apply
    /// (field manager `apprafter-cli`), so the value survives Argo
    /// CD self-heal on a Tier-1 no-infra-repo cluster.
    Set {
        /// One of `internet` (DNS + same-ns + world + needs),
        /// `internal` (DNS + same-ns + needs), or `strict`
        /// (DNS + needs; same-namespace also denied).
        profile: String,
    },
}

#[derive(Debug, Subcommand)]
pub enum MigrationCommand {
    /// List MigrationPlans across all namespaces — namespace,
    /// name, scope, classification, phase. Platform-scope plans
    /// live in `apprafter-system`; app-scope plans live in
    /// the Application's own namespace.
    #[command(alias = "ls")]
    List,
    /// Patch status.phase=approved on a MigrationPlan.
    /// MigrationController transitions through executing →
    /// completed. The plan's namespace is resolved automatically
    /// (app-scope plans live in the app's namespace); pass `-n`
    /// only to disambiguate a name that exists in multiple
    /// namespaces.
    Approve {
        /// MigrationPlan name (as listed via `apprafter
        /// migration list`).
        name: String,
        /// Namespace of the MigrationPlan. Omit to auto-resolve
        /// from `apprafter migration list`; required only when
        /// the same name exists in more than one namespace.
        #[arg(long, short = 'n')]
        namespace: Option<String>,
    },
    /// Patch status.phase=rejected. The admission webhook
    /// denies application-scope rejects — the CLI surfaces the
    /// denial message verbatim. Platform-scope rejects succeed
    /// and PlatformMigrationStrategy.reject reverts `spec.pin`.
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
        /// matches the AppProject for user apps. Pass
        /// `--project platform` for platform-internal apps,
        /// `platform-providers` for ServiceProvider operators.
        #[arg(long, default_value = "apps")]
        project: String,
        /// Destination namespace — Argo CD's
        /// `spec.destination.namespace`. With
        /// `CreateNamespace=true` in syncOptions, Argo CD creates this namespace
        /// on first sync if it doesn't exist. Default `apprafter`
        /// matches the namespace where AppRafter operator watches
        /// for Application CRs.
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
        /// Credential coverage gate. `present` (default) only warns,
        /// post-registration, when no `SourceCredential` declares a
        /// covering prefix. `confirmed` pre-flight-blocks a private
        /// (`https`) repo unless a `SourceCredential` whose status
        /// reports `GitValid=True` covers it — fail-fast for clusters
        /// whose operator has egress to validate. Restricted-egress
        /// clusters keep `present` (validity stays `Unverified`).
        #[arg(long = "coverage-gate", value_enum, default_value_t = crate::commands::app::CoverageGate::Present)]
        coverage_gate: crate::commands::app::CoverageGate,
        /// Skip the interactive wizard even when stdin + stdout
        /// are TTYs. The wizard fires by default on TTY shells
        /// and asks for any field not supplied via flag,
        /// pre-filling defaults from the cwd's git remote + the
        /// current branch where available.
        #[arg(long = "no-interactive", default_value_t = false)]
        no_interactive: bool,
        /// Auto-scaffold `<cwd>/apprafter/Application.cue` when
        /// missing, before registering. In a TTY the step-0
        /// wizard fires regardless of this flag; the flag only
        /// matters in `--no-interactive` mode where it gates
        /// silent scaffold (without the flag, the command
        /// refuses with a pointer to `apprafter app scaffold`).
        /// Non-interactive convenience for CI scripts that want
        /// one-shot scaffold + register.
        #[arg(long, default_value_t = false)]
        scaffold: bool,
        /// Environment to deploy. Selects which
        /// `spec.environments.<env>` override unifies onto base for THIS
        /// deployment; the Argo Application is named `<name>-<env>`. Must be
        /// one of the manifest's declared environments. Omit for a base-only
        /// deployment.
        #[arg(long)]
        env: Option<String>,
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
        /// LOGICAL application name (as passed to `apprafter app add`,
        /// listed via `apprafter app list`). When an app is deployed to
        /// multiple environments, `status` aggregates all of them
        /// grouped by the `apprafter.io/application=<name>` label and
        /// renders a per-environment section for each. A base-only app
        /// (one Application named exactly `<name>`) still resolves via
        /// the no-label fallback.
        name: String,
        /// Show child workload state — Argo CD's
        /// `status.resources[]` plus pods in the destination
        /// namespace matching
        /// `app.kubernetes.io/name=<inner-app-name>` (the AppRafter
        /// operator's label).
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
        /// Select the env-deployment `<name>-<env>`.
        /// Omit for a base/single-env app; if the app is deployed
        /// per-env and `--env` is omitted, the command errors with the
        /// available environments.
        #[arg(long)]
        env: Option<String>,
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
        /// Select the env-deployment `<name>-<env>`.
        /// Omit for a base/single-env app; if the app is deployed
        /// per-env and `--env` is omitted, the command errors with the
        /// available environments.
        #[arg(long)]
        env: Option<String>,
        /// Explicit revision (commit SHA / tag / branch).
        /// Without the flag: previous entry in `status.history`.
        #[arg(long = "to")]
        to: Option<String>,
        /// Skip confirmation prompt. Required in non-interactive
        /// shells.
        #[arg(long, default_value_t = false)]
        yes: bool,
    },
    /// Generate a starter `apprafter/Application.cue` based
    /// on the cwd's runtime markers (bun.lock / Cargo.toml /
    /// pyproject.toml / etc.). Writes to
    /// `<--path>/apprafter/Application.cue` and appends `.apprafter/local/` to the
    /// repo's `.gitignore` when present. Refuses to overwrite
    /// an existing manifest without `--force`.
    Scaffold {
        /// Force-pick a runtime instead of detecting from
        /// cwd. Slugs: bun, node-pnpm, node-yarn, node-npm,
        /// python-poetry, python-uv, python-pipenv,
        /// python-pip, rust, go, docker, blank.
        #[arg(long)]
        runtime: Option<String>,
        /// Application name (DNS-1123 lowercase). Default =
        /// cwd basename.
        #[arg(long)]
        name: Option<String>,
        /// Destination namespace for `metadata.namespace`.
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
        /// Declare a workload dependency to scaffold (repeatable). One of `pg`
        /// (needs.pg), `redis` (needs.redis), `disk` (needs.disk). Emits a
        /// `needs:` block in the generated Application.cue.
        #[arg(long = "needs")]
        needs: Vec<String>,
    },
    /// Validate an AppRafter `Application.cue` manifest LOCALLY,
    /// reproducing the cluster's render-time `cue` pipeline.
    /// Lays the CLI's bundled schema + a generated `claim` binding
    /// into a temp workspace and runs `cue vet`, so bare
    /// `claim.<type>.<field>` selectors and braceless
    /// `secret: "name/key"` refs resolve EXACTLY as the cue-cmp
    /// resolves them at sync time — a plain `cue vet` on the repo
    /// can't (the `claim` binding is never committed). Requires
    /// `cue` on PATH. Without `[manifest]`: defaults to
    /// `<cwd>/apprafter/Application.cue`, then a single `*.cue`
    /// in the cwd; otherwise pass the path explicitly.
    Validate {
        /// Manifest file (or directory holding `*.cue`) to
        /// validate. Omit to auto-discover.
        manifest: Option<std::path::PathBuf>,
    },
    /// Port-forward the app's primary Service to localhost and
    /// open it in a browser. Wraps `kubectl port-forward` with
    /// AppRafter-aware resolution: Application name → Argo CD
    /// CR → `spec.destination.namespace` → child Service via
    /// the `app.kubernetes.io/instance=<name>` label →
    /// container port (Service.spec.ports[0] OR
    /// `--container-port` override). Picks a free local port
    /// starting at 8080 with auto-increment to 8090 if busy.
    /// Blocks on Ctrl+C — the port-forward dies with the
    /// command.
    Open {
        /// Application name (as listed via `apprafter app list`).
        name: String,
        /// Target a specific environment's deployment (`<name>-<env>`).
        /// Optional — a single-env app resolves without it; needed only to
        /// disambiguate when the app is deployed to several environments.
        #[arg(long)]
        env: Option<String>,
        /// Local port to bind. Defaults to 8080; if busy, the
        /// command probes 8081…8090 before giving up.
        #[arg(long)]
        port: Option<u16>,
        /// Container port to forward to. Defaults to the
        /// Service's first declared `spec.ports[]` entry.
        /// Required when the Service declares no ports or when
        /// the operator wants a secondary port.
        #[arg(long = "container-port")]
        container_port: Option<u16>,
        /// Skip opening the browser; just print the URL and
        /// block on the port-forward. Useful for CI / scripts
        /// that want to forward in the background.
        #[arg(long = "no-browser", default_value_t = false)]
        no_browser: bool,
    },
    /// Delete an Application and cascade-remove the Argo CD CR
    /// (which Argo CD then tears down child resources for).
    /// Interactive: prompts for confirmation; non-interactive
    /// requires `--yes` to skip the prompt. `--env <env>` removes
    /// ONE per-environment deployment (`<name>-<env>`); without
    /// `--env`, ALL environment deployments grouped under
    /// `apprafter.io/application=<name>` are removed (a single
    /// confirmation covers the batch).
    #[command(alias = "rm")]
    Remove {
        /// LOGICAL application name. Without `--env` this removes every
        /// environment deployment of the app; with `--env` just the one.
        name: String,
        /// Skip confirmation prompt. Required in non-interactive
        /// shells (no TTY) — there's no silent destruction
        /// path.
        #[arg(long, default_value_t = false)]
        yes: bool,
        /// Preserve PVCs / `ResourceClaim`s when tearing down.
        /// Implemented as a post-delete cleanup that strips
        /// the destructive child-prune from the cascading
        /// delete. Useful when the operator wants to re-attach
        /// the data after a teardown.
        #[arg(long = "keep-data", default_value_t = false)]
        keep_data: bool,
        /// Environment to remove. Selects ONE per-environment
        /// deployment — the Argo Application named `<name>-<env>`
        /// registered by `app add --env <env>`. Omit to remove ALL
        /// environment deployments of `<name>` (grouped by the
        /// `apprafter.io/application` label); a base-only app
        /// with no env label resolves to the single `<name>` Application.
        #[arg(long)]
        env: Option<String>,
    },
}

#[derive(Debug, Subcommand)]
pub enum OpenUi {
    /// Open the Argo CD web UI on `https://localhost:8080`
    /// via `kubectl port-forward`, prefilling the admin
    /// username and printing the password.
    Argocd {
        /// AppProject filter applied to the opened URL —
        /// Argo CD's UI honours `?proj=<name>` (comma-separated
        /// list) to scope the Applications list. Defaults to
        /// `apps,default` so the operator lands on their own
        /// apps plus the platform root Application (in
        /// `default`), while the platform component apps stay
        /// hidden; pass `--project platform` to inspect
        /// chart-managed components, or `--all-projects` to
        /// drop the filter entirely.
        #[arg(long = "project", default_value = "apps,default")]
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
        /// Provider identifier. Only `hetzner-cloud` is wired
        /// today; AWS / Managed Cloud are planned for later.
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
        /// Skip the interactive wizard even when stdin + stdout
        /// are TTYs. The wizard fires by default on TTY shells
        /// and asks for any field not supplied via flag.
        #[arg(long = "no-interactive", default_value_t = false)]
        no_interactive: bool,
        /// Skip the Hetzner Cloud API ping that confirms the token
        /// authenticates (`GET /v1/locations`). Useful in CI when
        /// the network sandbox blocks outbound calls, or when
        /// pre-seeding a target store offline. Also settable via
        /// `APPRAFTER_NO_PING`, which takes a boolish value: `1`
        /// `true` `yes` `y` `t` `on` skip the ping, `0` `false` `no`
        /// `n` `f` `off` keep it. Any other value, including the
        /// empty string, is an error rather than a no-op.
        #[arg(
            long = "no-ping",
            env = "APPRAFTER_NO_PING",
            default_value_t = false,
            value_parser = BoolishValueParser::new(),
        )]
        no_ping: bool,
        /// Preferred server type SKU for this target (e.g. `cx22`, `cx32`).
        /// Saved as the "target preference" rung in the resolution chain —
        /// below an explicit `--server-type` flag or manifest value, above
        /// `APPRAFTER_SERVER_TYPE`. Omit to leave the slot empty (the chain
        /// continues down to the env var; there is NO implicit default — a
        /// fresh provision without any rung set fails with
        /// `apprafter::provider::server_type_not_selected`).
        /// When set and `--no-ping` is NOT passed, the SKU is validated
        /// against the live Hetzner API for the target's region.
        #[arg(long = "server-type")]
        server_type: Option<String>,
    },
    /// List every configured target, marking the active one. Empty
    /// store prints an onboarding hint pointing at `target add`.
    #[command(alias = "ls")]
    List,
    /// Switch the active target. Fails when the named target does
    /// not exist; subsequent operational commands (`apply`,
    /// `cluster-bootstrap`, ...) act on the new active target.
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
    /// Manage imported TLS certificates for the platform Gateway.
    Cert {
        #[command(subcommand)]
        action: TargetCertCommand,
    },
    /// Manage public-ingress zones (domains) on the platform Gateway.
    Domain {
        #[command(subcommand)]
        action: TargetDomainCommand,
    },
    /// Manage the cluster's cloud firewall (origin-firewall toggle).
    Firewall {
        #[command(subcommand)]
        action: TargetFirewallCommand,
    },
    /// Show the cluster node's public IPv4 + IPv6 (for DNS A/AAAA records).
    Ip,
    /// Set or change the server type (and region) on a target via the
    /// machine picker. This is the ONLY way to change the server type on an
    /// existing target — `target add <existing>` errors, and `--renew` is
    /// credentials-only.
    Machine {
        /// Target to modify (defaults to the active target).
        #[arg(long)]
        target: Option<String>,
        /// Set the type non-interactively (skips the picker). When
        /// `--no-ping` is also passed the SKU is saved without API
        /// validation; without `--no-ping` the SKU is validated against
        /// the live Hetzner API for the target's current region.
        #[arg(long = "server-type")]
        server_type: Option<String>,
        /// Skip the provider API. Requires `--server-type` — the picker
        /// cannot work without the API. Without `--server-type` this flag
        /// is an error, never a silent no-op. Also settable via
        /// `APPRAFTER_NO_PING`, which takes a boolish value: `1` `true`
        /// `yes` `y` `t` `on` skip the ping, `0` `false` `no` `n` `f`
        /// `off` keep it. Any other value, including the empty string,
        /// is an error rather than a no-op.
        #[arg(
            long = "no-ping",
            env = "APPRAFTER_NO_PING",
            default_value_t = false,
            value_parser = BoolishValueParser::new(),
        )]
        no_ping: bool,
    },
}

#[derive(Debug, Subcommand)]
pub enum TargetCertCommand {
    /// Import a TLS cert + key as a kubernetes.io/tls Secret in
    /// apprafter-system. The platform Gateway terminates TLS from
    /// it once a `target domain add --cert <name>` references it.
    Import {
        /// Cert name (DNS-1123 label, e.g. cf-origin-cert-apprafter-dev).
        /// Becomes the Secret name.
        name: String,
        /// Path to the PEM-encoded certificate (chain ok).
        #[arg(long)]
        cert: std::path::PathBuf,
        /// Path to the PEM-encoded private key (RSA only in this slice).
        #[arg(long)]
        key: std::path::PathBuf,
        /// Namespace for the cert Secret (the platform Gateway reads
        /// certs from apprafter-system).
        #[arg(long, short = 'n', default_value = "apprafter-system")]
        namespace: String,
        /// Update an existing cert Secret in place (no downtime — the
        /// Gateway picks up the new cert). Without this, importing over
        /// an existing name is rejected.
        #[arg(long, default_value_t = false)]
        replace: bool,
    },
}

#[derive(Debug, Subcommand)]
pub enum TargetDomainCommand {
    /// Register an apex zone, pointing it at an imported cert.
    Add {
        /// Apex domain (RFC-1123, no "*." prefix), e.g. apprafter.dev.
        domain: String,
        /// Imported cert name (the Secret from `target cert import`).
        #[arg(long)]
        cert: String,
        /// Audit "added by" (defaults to $USER).
        #[arg(long = "added-by")]
        added_by: Option<String>,
    },
    /// List registered zones + the apps using each.
    #[command(alias = "ls")]
    List,
    /// Unregister a zone. Blocked if apps use it (unless --force).
    Remove {
        /// Apex domain to unregister.
        domain: String,
        /// Remove even if applications still reference the domain.
        #[arg(long, default_value_t = false)]
        force: bool,
    },
}

#[derive(Debug, Subcommand)]
pub enum TargetFirewallCommand {
    /// Restrict the node's 80/443 to Cloudflare's IP ranges (origin firewall).
    CloudflareOrigin {
        /// enable | disable.
        #[arg(value_enum)]
        state: FirewallToggle,
    },
}

#[derive(Debug, Clone, Copy, clap::ValueEnum)]
pub enum FirewallToggle {
    Enable,
    Disable,
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

#[derive(Debug, Subcommand)]
/// `apprafter volume …` subcommands.
pub enum VolumeCommand {
    /// Create a SharedVolume CR in the cluster.
    Create {
        /// SharedVolume `metadata.name` (DNS-1123).
        name: String,
        /// Requested storage size (e.g. `5Gi`, `100Mi`).
        #[arg(long)]
        size: String,
        /// Target namespace. Defaults to `apprafter-system`.
        #[arg(long, short = 'n', default_value = "apprafter-system")]
        namespace: String,
    },
    /// List SharedVolumes with refCount and used/free capacity.
    #[command(alias = "ls")]
    List {
        /// Namespace to list. Omit for cluster-wide listing.
        #[arg(long, short = 'n')]
        namespace: Option<String>,
    },
    /// Show detail for one SharedVolume (pvcRef, refCount, capacity).
    Status {
        /// SharedVolume `metadata.name`.
        name: String,
        /// Namespace. Defaults to `apprafter-system`.
        #[arg(long, short = 'n', default_value = "apprafter-system")]
        namespace: String,
    },
    /// Delete a SharedVolume. Refused while still referenced by apps.
    Rm {
        /// SharedVolume `metadata.name`.
        name: String,
        /// Namespace. Defaults to `apprafter-system`.
        #[arg(long, short = 'n', default_value = "apprafter-system")]
        namespace: String,
        /// Skip the confirmation prompt.
        #[arg(long, default_value_t = false)]
        yes: bool,
    },
}

/// `apprafter node …` subcommands.
#[derive(Debug, Subcommand)]
pub enum NodeAction {
    /// Prepare the active target's cluster node: retrofit the kubelet
    /// node reservations + k3s OOM-protection AND provision host swap.
    ///
    /// Writes `/etc/rancher/k3s/config.yaml` reservations +
    /// `k3s.service.d/oom.conf` and — when the node is eligible
    /// (k8s ≥1.34 for the NoSwap GA gate + cgroup v2) — layers a host
    /// swapfile with `swapBehavior: NoSwap` so a control-plane spike
    /// degrades to swap instead of an OOM-kill while pods never swap.
    /// Applied atomically over one k3s restart (the API is briefly
    /// unavailable, ~30s), with a whole-step rollback on a `/readyz`
    /// timeout. Idempotent — a re-run refreshes the drop-in and is
    /// otherwise a near no-op.
    Prep {
        /// Skip the confirmation prompt (k3s restart briefly takes
        /// the API offline). Required in non-interactive shells.
        #[arg(long, default_value_t = false)]
        yes: bool,
    },
    /// Report the active target's node swap posture: `swapBehavior` /
    /// `failSwapOn` / swap capacity / k8s version (kube-API), plus live
    /// `swapon`, `vm.swappiness`, k3s `GOMEMLIMIT`, and the provision
    /// breadcrumb (SSH). Each field is labelled with its source
    /// (`[api]` / `[ssh]`); if SSH is unavailable the SSH-derived fields
    /// read `unknown` but the kube-API fields still render.
    Status,
}

/// `apprafter backup …` subcommands.
#[derive(Debug, Subcommand)]
pub enum BackupAction {
    /// Create a full encrypted backup of the cluster (default scope:
    /// whole cluster). Wraps native data + CRs + secrets into a
    /// restic repository.
    Create {
        /// Narrow scope to these namespaces (repeatable). Ignored
        /// unless `--select` is also passed.
        #[arg(long)]
        namespace: Vec<String>,
        /// When set, only the namespaces given via `--namespace` are
        /// backed up.
        #[arg(long, default_value_t = false)]
        select: bool,
        /// Path to the restic repository. Defaults to
        /// `<config>/backups/<target>`.
        #[arg(long)]
        repo: Option<String>,
        /// Passphrase for the restic repo. Falls back to
        /// `RESTIC_PASSWORD`; prompts interactively on a TTY.
        #[arg(long)]
        passphrase: Option<String>,
        /// Staging strategy: `monolithic` (default — stage every
        /// namespace's native data at once, one snapshot) or
        /// `sequential` (stage + snapshot one namespace at a time,
        /// bounding peak staging disk on large clusters).
        #[arg(long)]
        staging_mode: Option<String>,
    },
    /// List snapshots stored in a backup repo.
    #[command(alias = "ls")]
    List {
        /// Path to the restic repository. Defaults to
        /// `<config>/backups/<target>`.
        #[arg(long)]
        repo: Option<String>,
        /// Passphrase for the restic repo. Falls back to
        /// `RESTIC_PASSWORD`; prompts interactively on a TTY.
        #[arg(long)]
        passphrase: Option<String>,
    },
    /// Remove old snapshots from an S3-backed restic repository
    /// according to the configured retention policy. Run OUTSIDE the
    /// cluster with the operator's full S3 credentials.
    Prune {
        /// S3 restic repository URL (e.g. `s3:s3.amazonaws.com/my-bucket/prefix`).
        /// Defaults to `PlatformStack.spec.backup.bucket`.
        #[arg(long)]
        repo: Option<String>,
        /// Path to a dotenv credential file containing
        /// `AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY`,
        /// `RESTIC_PASSWORD` (and optionally `AWS_DEFAULT_REGION`).
        /// Falls back to the matching environment variables.
        #[arg(long)]
        credential_file: Option<std::path::PathBuf>,
        /// Keep-daily retention override (else spec.backup.retention, else 7).
        #[arg(long)]
        keep_daily: Option<u32>,
        /// Keep-weekly retention override (else spec.backup.retention, else 4).
        #[arg(long)]
        keep_weekly: Option<u32>,
        /// Keep-monthly retention override (else spec.backup.retention, else 6).
        #[arg(long)]
        keep_monthly: Option<u32>,
    },
    /// Verify the structural integrity of an S3-backed restic repository
    /// (`restic check`). Run OUTSIDE the cluster with the operator's full
    /// S3 credentials.
    Check {
        /// S3 restic repository URL. Defaults to `PlatformStack.spec.backup.bucket`.
        #[arg(long)]
        repo: Option<String>,
        /// Path to a dotenv credential file (`AWS_ACCESS_KEY_ID`,
        /// `AWS_SECRET_ACCESS_KEY`, `RESTIC_PASSWORD`, optional
        /// `AWS_DEFAULT_REGION`). Falls back to the matching env vars.
        #[arg(long)]
        credential_file: Option<std::path::PathBuf>,
        /// Deep verify: download and re-hash every pack (`--read-data`).
        #[arg(long, default_value_t = false)]
        read_data: bool,
    },
    /// Remove STALE locks from an S3-backed restic repository
    /// (`restic unlock`; live locks are never touched). Run OUTSIDE the
    /// cluster with the operator's full S3 credentials.
    Unlock {
        /// S3 restic repository URL. Defaults to `PlatformStack.spec.backup.bucket`.
        #[arg(long)]
        repo: Option<String>,
        /// Path to a dotenv credential file (`AWS_ACCESS_KEY_ID`,
        /// `AWS_SECRET_ACCESS_KEY`, `RESTIC_PASSWORD`, optional
        /// `AWS_DEFAULT_REGION`). Falls back to the matching env vars.
        #[arg(long)]
        credential_file: Option<std::path::PathBuf>,
    },
    /// Enable scheduled off-site S3 backup by patching PlatformStack.spec.backup.
    ///
    /// Credential input — exactly ONE of the following is required:
    ///
    ///   --credential-file <dotenv>  Parse the dotenv file (KEY=VALUE lines), probe
    ///                               the repo, and auto-seal the creds as a SealedSecret
    ///                               in apprafter-system. --credential names the Secret
    ///                               (optional; defaults to "apprafter-backup-s3").
    ///
    ///   --credential <name>         Name an existing Secret already in apprafter-system.
    ///                               The Secret's keys are read from the cluster and used
    ///                               to probe the repo. No sealing step.
    ///
    /// Accepted credential keys: S3_ACCESS_KEY_ID, S3_SECRET_ACCESS_KEY, RESTIC_PASSWORD
    /// (optional: S3_REGION). AWS_ACCESS_KEY_ID / AWS_SECRET_ACCESS_KEY /
    /// AWS_DEFAULT_REGION are accepted as aliases.
    Enable {
        /// Bucket name (with `--endpoint`), or a full restic repo URL
        /// (`s3:https://host/bucket`, `b2:...`, a local path, ...). With a
        /// bare name, pass `--endpoint` and the CLI builds the
        /// `s3:https://<endpoint>/<bucket>` URL for you.
        #[arg(long)]
        bucket: String,
        /// S3 endpoint host (e.g. `nbg1.your-objectstorage.com`). With a
        /// bare `--bucket` name, the CLI builds the `s3:https://<endpoint>/<bucket>`
        /// repo URL for you. Omit when passing a full restic URL in `--bucket`.
        #[arg(long)]
        endpoint: Option<String>,
        /// Optional path prefix inside the bucket (e.g. `backups/prod`). Only
        /// used together with a bare `--bucket` name and `--endpoint`; omit
        /// when passing a full restic URL in `--bucket`.
        #[arg(long)]
        prefix: Option<String>,
        /// Name of the credential Secret in apprafter-system.
        /// With --credential-file: the name to create (default: apprafter-backup-s3).
        /// Without --credential-file: an existing Secret to read creds from (required).
        #[arg(long, default_value = "")]
        credential: String,
        /// Path to a dotenv file with S3 + restic creds (S3_ACCESS_KEY_ID,
        /// S3_SECRET_ACCESS_KEY, RESTIC_PASSWORD; optional S3_REGION).
        /// AWS_ACCESS_KEY_ID / AWS_SECRET_ACCESS_KEY / AWS_DEFAULT_REGION accepted as aliases.
        /// When given, the creds are probed against the repo and then auto-sealed into the cluster.
        #[arg(long)]
        credential_file: Option<std::path::PathBuf>,
        /// Five-field cron schedule for the full backup Job.
        /// Default `0 3 * * *` — nightly at 03:00.
        #[arg(long, value_name = "cron")]
        cron: Option<String>,
        /// How many daily snapshots `restic forget` keeps. Default 7.
        /// Retention is only APPLIED when `--enforce cluster` is set
        /// or you run `apprafter backup prune`; under the default
        /// `--enforce operator` the scheduled Job never forgets.
        #[arg(long, value_name = "count")]
        keep_daily: Option<u32>,
        /// How many weekly snapshots `restic forget` keeps. Default 4.
        /// Applied under the same rule as `--keep-daily`.
        #[arg(long, value_name = "count")]
        keep_weekly: Option<u32>,
        /// How many monthly snapshots `restic forget` keeps. Default
        /// 6. Applied under the same rule as `--keep-daily`.
        #[arg(long, value_name = "count")]
        keep_monthly: Option<u32>,
        /// `operator` (default, cluster gets scoped creds) or `cluster` (in-cluster prune).
        #[arg(long)]
        enforce: Option<String>,
        /// `monolithic` (default) or `sequential`.
        #[arg(long)]
        staging_mode: Option<String>,
        /// Five-field cron schedule for the periodic `restic check`
        /// Job, which verifies repository integrity. Default
        /// `0 6 * * 0` — Sundays at 06:00, staggered clear of the
        /// nightly backup. The check is metadata-only; it does not
        /// re-download the data.
        #[arg(long, value_name = "cron")]
        check_cron: Option<String>,
        /// URL the runner POSTs a JSON failure report to when a
        /// backup or check Job fails. Egress to this host is opened
        /// automatically in the platform network policy. No default —
        /// unset means failures surface only in `backup status` and
        /// the Job logs.
        #[arg(long, value_name = "url")]
        failure_webhook: Option<String>,
        /// Confirm you have saved the restic passphrase + S3 credentials OUTSIDE the cluster.
        #[arg(long)]
        i_have_saved_credentials: bool,
    },
    /// Disable scheduled backup (sets spec.backup.enabled=false; keeps config).
    Disable,
    /// Show the current backup configuration, last Job outcomes, runner status,
    /// and last prune time (reads PlatformStack.spec.backup + Jobs + the
    /// apprafter-backup-status ConfigMap).
    Status,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_target_cert_import() {
        let cli = Cli::parse_from([
            "apprafter",
            "target",
            "cert",
            "import",
            "my-cert",
            "--cert",
            "c.pem",
            "--key",
            "k.pem",
        ]);
        match cli.command {
            Commands::Target {
                action:
                    TargetCommand::Cert {
                        action:
                            TargetCertCommand::Import {
                                name,
                                namespace,
                                replace,
                                ..
                            },
                    },
            } => {
                assert_eq!(name, "my-cert");
                assert_eq!(namespace, "apprafter-system"); // default
                assert!(!replace); // default
            }
            _ => panic!("expected target cert import"),
        }
    }

    #[test]
    fn parses_target_domain_subcommands() {
        let cli = Cli::parse_from([
            "apprafter",
            "target",
            "domain",
            "add",
            "apprafter.dev",
            "--cert",
            "c",
        ]);
        match cli.command {
            Commands::Target {
                action:
                    TargetCommand::Domain {
                        action:
                            TargetDomainCommand::Add {
                                domain,
                                cert,
                                added_by,
                            },
                    },
            } => {
                assert_eq!(domain, "apprafter.dev");
                assert_eq!(cert, "c");
                assert!(added_by.is_none());
            }
            _ => panic!("expected target domain add"),
        }
        let cli = Cli::parse_from(["apprafter", "target", "domain", "remove", "apprafter.dev"]);
        match cli.command {
            Commands::Target {
                action:
                    TargetCommand::Domain {
                        action: TargetDomainCommand::Remove { force, .. },
                    },
            } => assert!(!force),
            _ => panic!("expected target domain remove"),
        }
    }

    #[test]
    fn parses_target_firewall_cloudflare_origin() {
        let cli = Cli::parse_from([
            "apprafter",
            "target",
            "firewall",
            "cloudflare-origin",
            "enable",
        ]);
        match cli.command {
            Commands::Target {
                action:
                    TargetCommand::Firewall {
                        action: TargetFirewallCommand::CloudflareOrigin { state },
                    },
            } => assert!(matches!(state, FirewallToggle::Enable)),
            _ => panic!("expected target firewall cloudflare-origin enable"),
        }
        let cli = Cli::parse_from([
            "apprafter",
            "target",
            "firewall",
            "cloudflare-origin",
            "disable",
        ]);
        match cli.command {
            Commands::Target {
                action:
                    TargetCommand::Firewall {
                        action: TargetFirewallCommand::CloudflareOrigin { state },
                    },
            } => assert!(matches!(state, FirewallToggle::Disable)),
            _ => panic!("expected ... disable"),
        }
    }

    #[test]
    fn parses_target_ip() {
        let cli = Cli::parse_from(["apprafter", "target", "ip"]);
        assert!(matches!(
            cli.command,
            Commands::Target {
                action: TargetCommand::Ip
            }
        ));
    }

    #[test]
    fn parses_node_prep() {
        let cli = Cli::parse_from(["apprafter", "node", "prep"]);
        assert!(
            matches!(
                cli.command,
                Commands::Node {
                    action: NodeAction::Prep { yes: false }
                }
            ),
            "`apprafter node prep` must parse to NodeAction::Prep with yes defaulting to false"
        );
    }

    #[test]
    fn parses_node_prep_with_yes() {
        let cli = Cli::parse_from(["apprafter", "node", "prep", "--yes"]);
        assert!(matches!(
            cli.command,
            Commands::Node {
                action: NodeAction::Prep { yes: true }
            }
        ));
    }

    #[test]
    fn parses_node_status() {
        let cli = Cli::parse_from(["apprafter", "node", "status"]);
        assert!(
            matches!(
                cli.command,
                Commands::Node {
                    action: NodeAction::Status
                }
            ),
            "`apprafter node status` must parse to NodeAction::Status"
        );
    }

    #[test]
    fn rejects_removed_node_reserve_headroom() {
        // BREAKING (2.16g): `reserve-headroom` is HARD-REMOVED in favour of
        // `node prep`. clap must reject the old verb outright.
        let err = Cli::try_parse_from(["apprafter", "node", "reserve-headroom"]);
        assert!(
            err.is_err(),
            "`apprafter node reserve-headroom` must be UNKNOWN after the rename to `node prep`"
        );
    }

    #[test]
    fn skip_swap_flag_is_not_a_cli_arg() {
        // `APPRAFTER_SKIP_NODE_SWAP` stays an UNDOCUMENTED env hook — there is
        // no `--skip-swap` flag on `node prep`.
        let err = Cli::try_parse_from(["apprafter", "node", "prep", "--skip-swap"]);
        assert!(
            err.is_err(),
            "`node prep` must NOT accept a `--skip-swap` flag (env hook only)"
        );
    }

    #[test]
    fn platform_env_set_parses() {
        use clap::Parser;
        let cli = Cli::try_parse_from(["apprafter", "platform", "env", "set", "staging"])
            .expect("should parse");
        match cli.command {
            Commands::Platform {
                action:
                    PlatformCommand::Env {
                        action: EnvCommand::Set { env },
                    },
            } => assert_eq!(env, "staging"),
            other => panic!("unexpected parse: {other:?}"),
        }
    }

    #[test]
    fn platform_env_show_parses() {
        use clap::Parser;
        let cli =
            Cli::try_parse_from(["apprafter", "platform", "env", "show"]).expect("should parse");
        assert!(matches!(
            cli.command,
            Commands::Platform {
                action: PlatformCommand::Env {
                    action: EnvCommand::Show
                }
            }
        ));
    }
}
