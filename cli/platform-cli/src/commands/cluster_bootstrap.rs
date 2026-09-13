// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Minimal cluster-bootstrap (plan.md sub-phase 1.70, per ADR
//! 0025). Replaces the v0.1.x imperative install path with a
//! GitOps loader:
//!
//!   0. `helm install cilium cilium/cilium` — CNI first.
//!      Without it the k3s node carries
//!      `node.kubernetes.io/not-ready:NoSchedule` (k3s starts
//!      with `--flannel-backend=none`) and the Argo CD
//!      pre-install hook Job is `Pending` forever, failing
//!      the loader install with `failed pre-install: timed out
//!      waiting for the condition`.
//!      0b. `kubectl wait --for=condition=Ready node --all`
//!      so step 1 starts on a Ready node.
//!   1. `helm install argocd argo/argo-cd` — bare Argo CD,
//!      nothing else.
//!   2. `kubectl wait` for the `argocd-server` Deployment to
//!      become Available.
//!   3. `kubectl apply` a single root `Application` CR named
//!      `platform`, pointing at
//!      `oci://ghcr.io/<owner>/platform-stack:<RELEASED_PLATFORM_STACK_VERSION>`.
//!   4. `kubectl wait` for that root Application to report
//!      `status.health.status: Healthy` and
//!      `status.sync.status: Synced`. Once Healthy, the chart's
//!      child Applications (cilium adopts the loader release,
//!      cert-manager, apprafter-operator, admission-webhook,
//!      network-policies, conditionally Backstage) are
//!      reconciling under Argo CD.
//!
//! Drift correction, prune semantics, and re-apply idempotency
//! all flow from Argo CD instead of the CLI. Re-running
//! `cluster-bootstrap` against an already-bootstrapped cluster
//! is a no-op: `helm upgrade --install` keeps Cilium + Argo CD
//! on the same revision, `kubectl apply` is idempotent on the
//! root Application, the waits succeed instantly because the
//! conditions are already met.
//!
//! Why Cilium is in the CLI loader and not the chart: k3s
//! comes up without a CNI, so nothing schedules until Cilium
//! is installed — including the Argo CD pre-install hook the
//! chart would need to bring Cilium in. Loader = "minimum to
//! make the node schedulable and Argo CD reach Available".
//! Once Argo CD is up, `component_cilium.cue` adopts the
//! loader release (same name/namespace, `prune: false`) and
//! takes over upgrades and value overlays.
//!
//! What deliberately went away vs. v0.1.x:
//!   - Direct `helm install` of cert-manager, the operator,
//!     the admission-webhook, the bootstrap Application,
//!     Backstage. All now reconciled BY Argo CD via the
//!     platform-stack chart.
//!   - Inline Gateway API + Application CRD + default-deny
//!     NetworkPolicy + self-signed ClusterIssuer manifests.
//!     Shipped as components inside the chart.
//!   - The `cli-providers::k8s::*_yaml` inline value renderers.
//!     Values are now owned by the chart (`_loaderValues.*` in
//!     CUE) and extracted into compile-time constants by
//!     `build.rs` — one source of truth, no duplication.

use std::io::Write;
use std::path::Path;

use cli_core::manifest::{self, InfrastructureManifest};
use cli_core::secrets::{decrypt_with_identity, default_age_key_path, load_or_create_identity};
use cli_core::target::load_active_target_config;
use cli_core::{CliError, Result};
use cli_providers::k8s::{
    loader_fingerprint, HelmCli, HelmRunner, HelmUpgradeArgs, KubectlCli, KubectlRunner,
    ManifestSource, APPRAFTER_CLI_FIELD_MANAGER, APPRAFTER_PLATFORM_STACK_CHART_NAME,
    APPRAFTER_PLATFORM_STACK_DEFAULT_REPO, ARGOCD_CHART_VERSION, ARGOCD_LOADER_VALUES_YAML,
    CILIUM_CHART_VERSION, CILIUM_VALUES_YAML,
};
use cli_state::State;
use tempfile::NamedTempFile;
use tracing::info;

use crate::commands::state_paths::resolve_state_paths;

/// Per-step timeouts. Generous enough that the loop survives a
/// slow cpx22 cold start (cloud-init + image pulls + helm pod
/// schedule), tight enough that genuine breakage fails fast.
///
/// `NODE_READY_TIMEOUT_SECS` covers Cilium DaemonSet image
/// pull → Cilium agent ready → node `Ready=True`. Cilium pods
/// tolerate `node.kubernetes.io/not-ready:NoSchedule` so they
/// schedule on the not-ready node immediately; the wall-clock
/// dominator is the ~140 MB image pull. 180s on cpx22.
const NODE_READY_TIMEOUT_SECS: u64 = 180;

/// `ARGOCD_DEPLOYMENT_TIMEOUT_SECS` covers the helm-install
/// chart deploy → Argo CD pods scheduled → Available condition.
/// Empirically ~90s on cpx22, 180s leaves headroom.
const ARGOCD_DEPLOYMENT_TIMEOUT_SECS: u64 = 180;

/// `PLATFORM_RECONCILE_TIMEOUT_SECS` covers Argo CD pulling the
/// OCI chart → rendering N child Applications → each child
/// pulling its own upstream chart → component pods scheduled
/// → reconciler reports Healthy. Roughly six to seven sequential
/// chart pulls on tier-1: Cilium adopt, cert-manager, ArgoCD
/// self-manage, operator, webhook, netpolicies. 10 minutes on
/// cpx22.
const PLATFORM_RECONCILE_TIMEOUT_SECS: u64 = 600;

/// `CRD_CREATE_TIMEOUT_SECS` covers the gap between
/// "root Application reports Healthy" and "the operator chart's
/// CRDs (`applications.apprafter.io` + `platformstacks.apprafter.io`)
/// first appear in the cluster."
///
/// Walk-found bug v0.1.112 → v0.1.113: step 4b's Healthy wait
/// passed, but `kubectl wait crd/applications.apprafter.io
/// --for=condition=Established` failed **immediately** with
/// `Error from server (NotFound)`. `kubectl wait` errors out
/// instantly on a missing resource — it does NOT poll for the
/// resource to appear. So a false-positive "root App Healthy"
/// (Argo CD aggregating from children whose `.status` is still
/// empty / Progressing in a brief window) sends us into a CRD
/// wait that fails before the operator chart's child
/// Application has had a chance to apply its CRD manifests.
///
/// Fix: two-stage wait per CRD. First `--for=create` (blocks
/// until the CRD exists as a cluster object — kubectl 1.27+
/// feature). Then `--for=condition=Established` (blocks until
/// the CRD is fully registered in the apiserver's discovery
/// endpoint). The two-stage wait both bridges the
/// false-positive-Healthy window AND forces a fresh kubectl
/// discovery resolution for the subsequent SSA apply at step 5.
///
/// 600s is comfortable for the chart-pull → render → apply
/// sequence under Argo CD load (operator chart sync-wave 0
/// after cert-manager wave -10; cert-manager itself takes
/// up to a minute on cpx22).
const CRD_CREATE_TIMEOUT_SECS: u64 = 600;

/// `CRD_ESTABLISHED_TIMEOUT_SECS` covers the gap between
/// "the CRD object exists in the cluster" and
/// "the CRD reports `condition=Established=True`". The
/// apiextensions controller establishes new CRDs sub-second
/// to a few seconds after creation; 60s is generous headroom.
const CRD_ESTABLISHED_TIMEOUT_SECS: u64 = 60;

/// Step 5's PlatformStack apply is gated by the
/// `platformstacks.apprafter.io` ValidatingWebhook, whose backing
/// admission-webhook pod needs its cert-manager-issued TLS cert +
/// Endpoints. Argo CD syncs the CRD and the webhook as separate child
/// Applications, so the webhook can still be `ContainerCreating` when
/// the CRD is already Established — the apply then fails with `no
/// endpoints available for service "admission-webhook"`. Retry with
/// backoff until the webhook is serving (or, when the admission stack
/// is disabled, the first apply just succeeds). 30 × 10s = 5 min.
const PLATFORMSTACK_APPLY_ATTEMPTS: u32 = 30;
const PLATFORMSTACK_APPLY_BACKOFF_SECS: u64 = 10;

/// Raw API path of the `PlatformStack` singleton — read before the render so
/// the apply can carry forward a field the local store has no opinion about
/// (see [`origin_firewall_for_render`]). `get_raw` rather than a typed get:
/// the CRD may not exist yet on a fresh cluster, and a 404 body is exactly the
/// "nothing to preserve" answer.
const PLATFORMSTACK_DEFAULT_RAW_PATH: &str =
    "/apis/apprafter.io/v1alpha1/namespaces/apprafter-system/platformstacks/default";

/// Run the GitOps loader against `target_override`, or against the
/// active target when it is `None`.
///
/// **`target_override` is load-bearing, not decorative.** Up to the fix
/// for finding C1 this function took no argument and resolved `None`
/// itself, on the reasoning that the standalone `apprafter
/// cluster-bootstrap` subcommand has no `--target` flag so the active
/// target is always the right answer. That reasoning held for the two
/// direct entry points and broke for the third: `apprafter restore
/// --reprovision --target X` runs this as phase 3 of
/// [`bootstrap_all::run`](super::bootstrap_all::run), whose phases 1
/// (`apply`) and 2 (`kubeconfig`) both honour the override. Phase 3
/// re-resolving the ACTIVE target meant a restore into `X` bootstrapped
/// whichever cluster happened to be active — `helm upgrade --install`
/// of Cilium and Argo CD, plus a server-side apply of the root
/// `Application` and of `PlatformStack/default` under field manager
/// `apprafter-cli`, resetting a deliberately chosen `spec.channel`,
/// `spec.autoUpgrade` and `targetRevision` on a cluster nobody named.
///
/// Both target-sensitive resolutions below take the override: the state
/// directory (which carries the cached kubeconfig) and the target
/// config (which carries the tier written into `PlatformStack`).
/// Passing it to one and not the other reintroduces half the bug.
pub fn run(target_override: Option<&str>) -> Result<()> {
    run_with(
        &HelmCli,
        &KubectlCli,
        target_override,
        platform_stack_version,
    )
}

/// `run` with its three outside-world edges injected: the helm runner,
/// the kubectl runner, and the platform-stack version resolver.
///
/// `resolve_platform_version` is a parameter rather than a direct call
/// to [`platform_stack_version`] because that function performs an HTTP
/// GET against the GitHub Releases API. Tests that drive the whole
/// phase-3 body need the state/config resolution, not the network.
pub(crate) fn run_with<H: HelmRunner, K: KubectlRunner>(
    helm: &H,
    kubectl: &K,
    target_override: Option<&str>,
    resolve_platform_version: fn() -> String,
) -> Result<()> {
    info!(target_override, "cluster-bootstrap invoked (GitOps loader)");

    // Per-target state (v0.1.154). The standalone `cluster-bootstrap`
    // subcommand has no `--target` flag, so `dispatch` passes `None`
    // here and the active target the preceding `apply` populated is
    // used — same as `argocd-password` and the kubeconfig fetch. When
    // we are phase 3 of `bootstrap-all --target X`, the caller hands
    // the override down and it wins over the active pointer.
    let resolved = resolve_state_paths(target_override)?;
    let paths = resolved.paths;
    let target_store = resolved.store;
    let state = State::load_or_default(&paths)?;
    let hetzner = state.hetzner_cloud.clone().ok_or_else(|| {
        CliError::Other(
            "state has no hetzner_cloud section; run `apprafter apply` first".to_string(),
        )
    })?;

    let plaintext = decrypt_cached_kubeconfig(&hetzner)?;
    let kubeconfig_file = write_tempfile_with("apprafter-kubeconfig-", &plaintext)?;

    // Consult the target for its tier hint. Previously the store
    // handle was constructed inline (and tolerated a missing config
    // dir); per-target state resolution already gives us a known-good
    // `target_store`, so we just read off it directly.
    //
    // The override goes here too. `load_active_target_config` re-runs
    // `resolve_active_target_name` internally, so passing `None` would
    // read the ACTIVE target's tier even when the kubeconfig above came
    // from the overridden one — a cluster bootstrapped at someone
    // else's tier. Second half of the C1 fix.
    let target_config = load_active_target_config(&target_store, target_override);

    let active_tier: u8 = target_config
        .as_ref()
        .and_then(|c| c.default_tier.as_deref())
        .and_then(|s| s.parse::<cli_core::Tier>().ok())
        .map(|t| t.level())
        .unwrap_or(1);

    let active_domain: Option<&str> = None;

    // A4: the origin-firewall toggle rides the CR, so that a backup — above
    // all the scheduled in-cluster one, which has no target store to read —
    // carries it. The order mirrors `apply::cf_origin_enabled` (manifest, then
    // target store), because that is what builds the node's real firewall; when
    // neither says anything the cluster's own value is carried forward rather
    // than pruned. See `origin_firewall_for_render` for why no rung is optional.
    let local_origin_firewall = target_config
        .as_ref()
        .and_then(|c| c.firewall.as_ref())
        .map(|f| f.cloudflare_origin);
    let origin_firewall = origin_firewall_for_render(
        manifest_origin_firewall(),
        local_origin_firewall,
        live_origin_firewall(kubectl, kubeconfig_file.path()),
    );

    let platform_repo = platform_stack_repo();
    let platform_version = resolve_platform_version();

    let root_app_yaml = render_root_application(&platform_repo, &platform_version);
    let root_app_file = write_tempfile_with("apprafter-root-application-", &root_app_yaml)?;

    let platformstack_yaml =
        render_platformstack_default(active_tier, active_domain, origin_firewall);
    let platformstack_file =
        write_tempfile_with("apprafter-platformstack-default-", &platformstack_yaml)?;

    perform_bootstrap(
        helm,
        kubectl,
        kubeconfig_file.path(),
        root_app_file.path(),
        platformstack_file.path(),
    )?;

    println!(
        "cluster-bootstrap complete: Cilium installed, node Ready, Argo CD installed, \
         platform-stack {platform_version} reconciling from {platform_repo}/{chart}; \
         PlatformStack/default created in apprafter-system (tier={active_tier})",
        chart = APPRAFTER_PLATFORM_STACK_CHART_NAME,
    );
    Ok(())
}

/// Pure orchestration: bring up Argo CD, hand off platform
/// reconciliation to it. Decoupled from `run()` so tests can
/// drive it with fake helm + kubectl runners.
pub(crate) fn perform_bootstrap<H: HelmRunner, K: KubectlRunner>(
    helm: &H,
    kubectl: &K,
    kubeconfig_path: &Path,
    root_application_path: &Path,
    platformstack_default_path: &Path,
) -> Result<()> {
    // 0. Cilium first — see module doc. k3s starts without a
    //    CNI, the single node carries
    //    `node.kubernetes.io/not-ready:NoSchedule`, and the
    //    Argo CD pre-install hook Job is `Pending` forever
    //    until a CNI installs. Cilium DaemonSet tolerates the
    //    not-ready taint so it schedules on the not-ready
    //    node and flips it to Ready.
    // `APPRAFTER_BOOTSTRAP_SKIP_CILIUM` leaves the cluster's existing
    // CNI in place instead of installing Cilium (and disables the
    // platform-stack chart's Cilium component too — see
    // `render_root_application`). Used by the k3d e2e: Cilium's eBPF
    // datapath converges pathologically slowly on k3d-in-CI (~10 min,
    // independent of single/dual-stack), so the e2e runs on k3d's
    // default flannel + kube-proxy to exercise the GitOps + migration
    // logic fast. Cilium itself is validated on real hardware by the
    // nightly Hetzner e2e/mvp.sh. NEVER set on a real cluster — the
    // platform expects Cilium (kube-proxy replacement, L2, NetworkPolicy).
    if !bootstrap_skip_cilium() {
        helm.repo_add("cilium", "https://helm.cilium.io/")?;
        let cilium_values_file =
            write_tempfile_with("apprafter-cilium-loader-values-", CILIUM_VALUES_YAML)?;
        upgrade_unless_current(
            helm,
            HelmUpgradeArgs {
                release: "cilium".into(),
                chart: "cilium/cilium".into(),
                version: Some(CILIUM_CHART_VERSION.into()),
                namespace: "kube-system".into(),
                values_path: cilium_values_file.path().to_path_buf(),
                kubeconfig_path: kubeconfig_path.to_path_buf(),
                set_values: cilium_set_overrides(),
                fingerprint: None,
            },
        )?;
    }

    // 0b. Node MUST reach Ready before step 1 — otherwise the
    //     Argo CD pre-install hook would still be Pending. (With
    //     skip-Cilium the existing CNI keeps the node Ready already.)
    kubectl.wait_for_condition(
        "node --all",
        None,
        "condition=Ready",
        NODE_READY_TIMEOUT_SECS,
        kubeconfig_path,
    )?;

    // 1. Argo CD loader install. `helm upgrade --install` is
    //    idempotent — re-running cluster-bootstrap on a
    //    healthy cluster is a no-op.
    helm.repo_add("argo", "https://argoproj.github.io/argo-helm")?;

    // Values inline as a tempfile so the chart's defaults
    // stay overridable without writing to disk long-term. We
    // minimise the loader to what's needed to BOOTSTRAP — the
    // platform-stack chart's own `component_argocd.cue`
    // overlay will adopt this release and add the cue-cmp
    // sidecar + tier-2 replica counts when it reconciles.
    let argocd_values_file =
        write_tempfile_with("apprafter-argocd-loader-values-", ARGOCD_LOADER_VALUES_YAML)?;
    upgrade_unless_current(
        helm,
        HelmUpgradeArgs {
            release: "argocd".into(),
            chart: "argo/argo-cd".into(),
            version: Some(ARGOCD_CHART_VERSION.into()),
            namespace: "argocd".into(),
            values_path: argocd_values_file.path().to_path_buf(),
            kubeconfig_path: kubeconfig_path.to_path_buf(),
            set_values: Vec::new(),
            fingerprint: None,
        },
    )?;

    // 2. Wait for argocd-server Deployment to become Available
    //    before applying the root Application — otherwise the
    //    Application CRD isn't yet installed and `kubectl
    //    apply` fails with "no matches for kind".
    kubectl.wait_for_condition(
        "deployment/argocd-server",
        Some("argocd"),
        "condition=Available",
        ARGOCD_DEPLOYMENT_TIMEOUT_SECS,
        kubeconfig_path,
    )?;

    // 2b. Apply the AppProjects the root Application references.
    //     The argo-cd 7.7.7 chart does NOT create AppProjects from
    //     `configs.projects` (it has no such template — that loader
    //     value is silently ignored); only the umbrella chart's
    //     `templates/appprojects.yaml` renders them, and that can't
    //     run until the root `platform` Application syncs, which needs
    //     the `platform` project to already exist. So apply them here,
    //     byte-identical to the umbrella output so Argo CD adopts them
    //     cleanly on the first sync. Without this the root Application
    //     fails `Application referencing project platform which does
    //     not exist` and the whole bootstrap deadlocks.
    let app_projects_file = write_tempfile_with("apprafter-app-projects-", &render_app_projects())?;
    kubectl.apply_manifest_server_side(
        &ManifestSource::Path(app_projects_file.path().to_path_buf()),
        kubeconfig_path,
        APPRAFTER_CLI_FIELD_MANAGER,
    )?;

    // 3. Apply the root Application via server-side apply with
    //    field manager `apprafter-cli`. Same SSA convention as
    //    step 5's PlatformStack apply. Walk-found bug v0.1.117
    //    → v0.1.118: a plain `kubectl apply -f` left field
    //    manager `kubectl-client-side-apply` owning
    //    `spec.source.targetRevision`, which PlatformController
    //    then flagged as a foreign writer
    //    (`UnauthorizedSourceModification=True`). SSA + the
    //    `apprafter-cli` whitelist in
    //    `operator-controllers/platform-stack`'s
    //    `WHITELISTED_FIELD_MANAGERS` keep the bootstrap clean.
    kubectl.apply_manifest_server_side(
        &ManifestSource::Path(root_application_path.to_path_buf()),
        kubeconfig_path,
        APPRAFTER_CLI_FIELD_MANAGER,
    )?;

    // 4a. Wait for the root Application to report Synced. The
    //     Synced flag is what tells us Argo CD actually pulled
    //     the chart and produced child Applications. Health is
    //     not enough on its own — a freshly-created root
    //     Application with **zero rendered children** reports
    //     `Healthy` (trivially, no resources to fail) while
    //     `Sync=Unknown` (chart pull errored). Walk-found bug
    //     v0.1.99 → v0.1.100 was this exact false-positive.
    //
    //     The resource is **group-qualified** (`applications.argoproj.io`,
    //     not the bare `application`): once the operator chart installs
    //     the `applications.apprafter.io` CRD (which also claims the
    //     `app`/`apps` short names), a bare `application/platform` is
    //     ambiguous and kubectl resolves it to `applications.apprafter.io`
    //     (alphabetically first) → `NotFound`. This only bites on a
    //     RE-RUN (the idempotency path), where the CRD is already present
    //     at wait time — walk-found bug at v0.2.10.
    kubectl.wait_for_condition(
        "applications.argoproj.io/platform",
        Some("argocd"),
        "jsonpath={.status.sync.status}=Synced",
        PLATFORM_RECONCILE_TIMEOUT_SECS,
        kubeconfig_path,
    )?;

    // 4b. Then wait for Healthy. After Synced, the child
    //     Applications exist; Healthy means their workloads
    //     reached the chart's health-check thresholds.
    kubectl.wait_for_condition(
        "applications.argoproj.io/platform",
        Some("argocd"),
        "jsonpath={.status.health.status}=Healthy",
        PLATFORM_RECONCILE_TIMEOUT_SECS,
        kubeconfig_path,
    )?;

    // 4c. Two-stage wait for the two CRDs the operator chart
    //     ships (sync-wave -5). Per CRD: first
    //     `kubectl wait --for=create` (blocks until the CRD
    //     object exists), then `--for=condition=Established`
    //     (blocks until the apiserver has registered it in
    //     discovery).
    //
    //     Why both stages: `kubectl wait` errors out instantly
    //     on a missing resource — it does NOT poll for
    //     creation. So we can't rely on a single
    //     `--for=condition=Established` to handle the gap
    //     between "root App Healthy" and "CRDs applied". Step
    //     4b's Healthy on the root Application can fire as a
    //     false-positive (Argo CD aggregating child health in a
    //     brief window where children's `.status` is empty),
    //     leaving the CRD wait racing the operator chart's
    //     apply. Walk-found bugs v0.1.111 → v0.1.112 (no CRD)
    //     and v0.1.112 → v0.1.113 (kubectl wait NotFound) both
    //     proved this.
    //
    //     The two-stage wait also forces a fresh kubectl
    //     discovery resolution for the subsequent SSA apply at
    //     step 5 — closing the stale-discovery-cache angle
    //     mentioned in v0.1.111 → v0.1.112 notes.
    for crd_name in [
        "applications.apprafter.io",
        "platformstacks.apprafter.io",
        "sourcecredentials.apprafter.io",
    ] {
        let crd_ref = format!("crd/{crd_name}");
        kubectl.wait_for_condition(
            &crd_ref,
            None,
            "create",
            CRD_CREATE_TIMEOUT_SECS,
            kubeconfig_path,
        )?;
        kubectl.wait_for_condition(
            &crd_ref,
            None,
            "condition=Established",
            CRD_ESTABLISHED_TIMEOUT_SECS,
            kubeconfig_path,
        )?;
    }

    // 5. Apply the default PlatformStack singleton. The
    //    operator chart's CRD reconciliation under Argo CD has
    //    by now registered platformstacks.apprafter.io (sync-wave
    //    -5 ensured this before the operator Deployment came up),
    //    so the apply lands on a defined schema. SSA with field
    //    manager `apprafter-cli` means a re-bootstrap is idempotent
    //    (managed fields stay with the loader; PlatformController
    //    in B.1.73 will own status under its own field manager).
    //    Retry on the admission-webhook race (see
    //    PLATFORMSTACK_APPLY_ATTEMPTS): the validating webhook's pod
    //    may not have Endpoints yet even after the CRD is Established.
    let mut attempt = 0u32;
    loop {
        attempt += 1;
        match kubectl.apply_manifest_server_side(
            &ManifestSource::Path(platformstack_default_path.to_path_buf()),
            kubeconfig_path,
            APPRAFTER_CLI_FIELD_MANAGER,
        ) {
            Ok(()) => break,
            Err(e) if attempt < PLATFORMSTACK_APPLY_ATTEMPTS => {
                info!(
                    attempt,
                    error = %e,
                    "PlatformStack apply failed (admission webhook likely not ready yet); retrying"
                );
                std::thread::sleep(std::time::Duration::from_secs(
                    PLATFORMSTACK_APPLY_BACKOFF_SECS,
                ));
            }
            Err(e) => return Err(e),
        }
    }

    Ok(())
}

/// `helm upgrade --install` the release UNLESS it is already deployed at
/// the desired fingerprint (a true-no-op re-run). Logs the skip.
///
/// Walk-found idempotency wart (2.4g): the unconditional
/// `helm upgrade --install` bumped the Cilium/Argo CD helm revision
/// (1→2…) on every `cluster-bootstrap` re-run even when nothing
/// changed. We compute a [`loader_fingerprint`] over the chart, version,
/// values-file content, and `--set` overrides; if the release is already
/// `deployed` at exactly that fingerprint we skip the upgrade. A CLI
/// upgrade that changes any of those inputs flips the fingerprint and
/// the upgrade runs — safety preserved. The fingerprint is injected as
/// `--set apprafterLoaderHash=<fp>` only on the real upgrade so helm
/// records it for the next run to read back.
fn upgrade_unless_current(helm: &impl HelmRunner, mut args: HelmUpgradeArgs) -> Result<()> {
    let fp = loader_fingerprint(&args)?;
    if helm.release_is_current(&args.release, &args.namespace, &args.kubeconfig_path, &fp)? {
        info!(
            release = %args.release,
            "already current — skipping helm upgrade (no revision bump)"
        );
        return Ok(());
    }
    args.fingerprint = Some(fp);
    helm.upgrade_install(&args)
}

/// Render the root `Application` CR YAML the CLI hands Argo CD.
/// Is `APPRAFTER_CILIUM_IPV4_ONLY` set to a truthy value? When it is,
/// `cluster-bootstrap` installs Cilium **single-stack (IPv4-only)** by
/// disabling IPv6 in BOTH places Cilium is configured: the loader
/// `helm install` (a `--set ipv6.enabled=false`) and the platform-stack
/// chart that Argo CD adopts (a `valuesObject` override in the root
/// Application). Production Tier-1 stays dual-stack (ADR 0017); this
/// fires only when the env var is explicitly set — its purpose is the
/// k3d e2e, where the node has no real IPv6 (k3d's ULA IPv6 doesn't
/// route, making Cilium's dual-stack eBPF datapath pathologically slow
/// to converge). Dual-stack is validated on real hardware via the
/// nightly Hetzner `e2e/mvp.sh`.
fn cilium_ipv4_only() -> bool {
    std::env::var("APPRAFTER_CILIUM_IPV4_ONLY")
        .map(|v| matches!(v.trim(), "1" | "true" | "TRUE" | "yes"))
        .unwrap_or(false)
}

/// Loader-Cilium `--set` overrides (see [`cilium_ipv4_only`]).
fn cilium_set_overrides() -> Vec<String> {
    if cilium_ipv4_only() {
        vec!["ipv6.enabled=false".to_string()]
    } else {
        Vec::new()
    }
}

/// Render the AppProjects the bootstrap must apply before the root
/// `platform` Application (see the call site). Byte-mirrors the
/// umbrella chart's `templates/appprojects.yaml` output (sync-wave
/// `-30`, the `apprafter.io/managed-by` + `source` labels, and the
/// permissive specs of `_appProjects` in `platform-stack/cue/
/// app_projects.cue`) so Argo CD adopts them without an SSA conflict
/// on the first umbrella sync. Keep in sync with `_appProjects`.
pub(crate) fn render_app_projects() -> String {
    // `default` is unrestricted and allows any destination server;
    // it hosts the root `platform` Application (created at bootstrap
    // by `render_root_application`) plus ad-hoc Applications.
    // `platform` + `platform-providers` pin the in-cluster API.
    let project = |name: &str, description: &str, server: &str| {
        format!(
            r#"---
apiVersion: argoproj.io/v1alpha1
kind: AppProject
metadata:
  name: {name}
  namespace: argocd
  annotations:
    argocd.argoproj.io/sync-wave: "-30"
  labels:
    apprafter.io/managed-by: apprafter
    apprafter.io/source: platform-stack
spec:
  description: {description}
  sourceRepos:
    - '*'
  destinations:
    - namespace: '*'
      server: {server}
  clusterResourceWhitelist:
    - group: '*'
      kind: '*'
  namespaceResourceWhitelist:
    - group: '*'
      kind: '*'
"#
        )
    };
    format!(
        "# SPDX-License-Identifier: FSL-1.1-Apache-2.0\n# Applied by `apprafter cluster-bootstrap`.\n{}{}{}",
        project(
            "default",
            "Default project — unrestricted; hosts the root platform Application + ad-hoc Applications.",
            "'*'",
        ),
        project(
            "platform",
            "Platform components — umbrella chart payload.",
            "https://kubernetes.default.svc",
        ),
        project(
            "platform-providers",
            "Platform service providers (CNPG, Dragonfly, NATS, Kamaji, …).",
            "https://kubernetes.default.svc",
        ),
    )
}

/// Is `APPRAFTER_BOOTSTRAP_SKIP_CILIUM` set? When it is, the bootstrap
/// leaves the cluster's existing CNI in place (no loader Cilium
/// install) and disables the platform-stack chart's Cilium component.
/// k3d-e2e-only — see the call site for the rationale.
fn bootstrap_skip_cilium() -> bool {
    std::env::var("APPRAFTER_BOOTSTRAP_SKIP_CILIUM")
        .map(|v| matches!(v.trim(), "1" | "true" | "TRUE" | "yes"))
        .unwrap_or(false)
}

/// One CR, no templating engine — the input space (repo + version)
/// is two strings; string interpolation is clearer than dragging
/// in `serde_yaml` for an 18-line document.
///
/// `syncPolicy.automated.prune: true` lets Argo CD remove child
/// Applications if they disappear from the chart (e.g. a chart
/// version that drops a deprecated component). The chart's OWN
/// `component_argocd.cue` overrides this with `prune: false` on
/// the Argo CD child Application — Argo CD doesn't self-prune,
/// preventing the chicken-and-egg foot-gun.
pub(crate) fn render_root_application(repo_url: &str, chart_version: &str) -> String {
    // Override for the Argo-CD-adopted platform-stack chart, mirroring
    // the loader knobs so a later chart sync does not undo them:
    //   - skip-Cilium  → disable the chart's Cilium component entirely
    //     (the cluster keeps its existing CNI);
    //   - IPv4-only    → set ipv6.enabled=false on the chart's Cilium
    //     so it is not reverted to dual-stack.
    let helm_block = if bootstrap_skip_cilium() {
        "\n    helm:\n      valuesObject:\n        components:\n          cilium:\n            enabled: false"
    } else if cilium_ipv4_only() {
        "\n    helm:\n      valuesObject:\n        components:\n          cilium:\n            values:\n              ipv6:\n                enabled: false"
    } else {
        ""
    };
    format!(
        r#"# SPDX-License-Identifier: FSL-1.1-Apache-2.0
# Rendered by `apprafter cluster-bootstrap`. The CLI generates
# this from cli-providers::k8s::RELEASED_PLATFORM_STACK_VERSION
# and APPRAFTER_PLATFORM_STACK_DEFAULT_REPO; edit those
# constants and re-tag the CLI to bump the platform layer.
apiVersion: argoproj.io/v1alpha1
kind: Application
metadata:
  name: platform
  namespace: argocd
spec:
  # Root platform Application joins the `default` AppProject so
  # it surfaces in the default operator Argo view (`apprafter
  # open argocd`) right next to the user's apps — kept separate
  # from the platform component Applications (cilium, operator,
  # cert-manager, …), which stay in the `platform` project.
  # `default` is created at bootstrap by `render_app_projects`
  # (one of its four projects, fully unrestricted) and that
  # manifest is applied BEFORE this one, so the project exists
  # by the time the root Application lands. (B.1.79a originally
  # parked the root App in `platform` only because the `default`
  # project wasn't auto-created back then — that reason is moot
  # now that `render_app_projects` always emits it.)
  project: default
  source:
    repoURL: "{repo_url}"
    chart: {chart_name}
    targetRevision: "{chart_version}"{helm_block}
  destination:
    server: https://kubernetes.default.svc
    namespace: argocd
  syncPolicy:
    automated:
      prune: true
      selfHeal: true
    syncOptions:
      - CreateNamespace=true
      - ServerSideApply=true
  # 1.83a F3 (live-Argo-DIFF-confirmed, T8 Run 2): the apiserver DEFAULTS
  # Gateway-API fields our chart leaves unset, so the platform Gateway +
  # redirect HTTPRoute would sit permanently OutOfSync (and selfHeal would
  # churn re-applying them). Mute the defaulted paths: the Gateway
  # certificateRef `group` (apiserver fills the empty core group ''), and on
  # the redirect HTTPRoute the parentRef `group`/`kind` defaults + the
  # auto-defaulted match-all `rules[].matches`.
  ignoreDifferences:
    - group: gateway.networking.k8s.io
      kind: Gateway
      name: platform
      namespace: apprafter-system
      jqPathExpressions:
        # `select(.tls)` skips the catch-all http listener (no tls) — without
        # it `.tls.certificateRefs[]` iterates null on that listener and the
        # whole jq expression errors, so the ignore silently never applies
        # (T8 Run 2: the HTTPRoute ignore worked but the Gateway stayed
        # OutOfSync until this filter was added).
        - .spec.listeners[] | select(.tls) | .tls.certificateRefs[].group
    - group: gateway.networking.k8s.io
      kind: HTTPRoute
      name: platform-http-redirect
      namespace: apprafter-system
      jqPathExpressions:
        - .spec.parentRefs[].group
        - .spec.parentRefs[].kind
        - .spec.rules[].matches
"#,
        chart_name = APPRAFTER_PLATFORM_STACK_CHART_NAME,
        helm_block = helm_block,
    )
}

/// The origin-firewall intent this bootstrap should write into the CR (A4).
/// Pure.
///
/// `manifest` is the Infrastructure manifest's toggle, `local` is the target
/// store's (`None` = the target never said), and `live` is what the cluster's
/// `PlatformStack` already records (`None` = a fresh cluster, an older CR, or a
/// read that did not come back).
///
/// **The order is `apply`'s, deliberately.** `apply::cf_origin_enabled` resolves
/// manifest → target store → `false` when it builds the node's actual firewall,
/// so anything else here would record an intent the node does not implement.
///
/// Without the manifest rung the two authoring routes were asymmetric for no
/// designed reason: an operator who ran `target firewall cloudflare-origin
/// enable` got their intent persisted and therefore backed up, while one who
/// declared it in `Infrastructure` did not. That second operator's restore was
/// silently unprotected — and silently, since a CR with no field reads as
/// *unknown*, so the restore had nothing to warn about either. It only bit when
/// the restore ran without `APPRAFTER_MANIFEST` pointing at their file (from
/// another machine, mid-incident); with the manifest in reach, `apply` is phase
/// one of a re-provision and the firewall goes up before the node serves
/// anything, which is strictly better than this record can manage.
///
/// THE SSA OMISSION TRAP. The render is server-side-applied under field
/// manager `apprafter-cli`, and under one manager **omitting a field removes
/// it**. So the three candidate behaviours for an unknown local toggle are not
/// interchangeable:
///
/// * emitting `false` would REWRITE the cluster's answer. A restore lands
///   `cloudflareOrigin: true` from the snapshot onto a target whose local
///   store is still `None` (a fresh `target add`, exactly the "move to a
///   bigger machine" route) — and the operator's very next `apprafter apply`
///   would flip it back to `false`, teaching every later backup that the
///   source had its ports open. That is the worst of the three: it does not
///   lose an answer, it manufactures a wrong one.
/// * omitting BLINDLY is safe only while `apprafter-cli` does not own the
///   field. Once a bootstrap has written it (this machine's store said
///   `true`), a later bootstrap from a store that says nothing — a second
///   workstation, a re-`target add` — would prune the value back out of the
///   CR. Silent, and it takes the backup trail with it.
/// * carrying the live value forward when the local store has no opinion
///   keeps both hands off: the field stays exactly as the cluster had it, so
///   neither a prune nor a rewrite can happen, and the render still only ever
///   OMITS when nobody anywhere has an answer.
///
/// The manifest wins over the local store, and the local store over the live
/// value, in both directions: together they are the authority `apprafter apply`
/// builds the node's real firewall from, so a CR that disagreed with them would
/// be a record of something untrue. `live` is not an opinion at all — it is the
/// existing record, consulted only so that having none of our own prunes
/// nothing.
pub(crate) fn origin_firewall_for_render(
    manifest: Option<bool>,
    local: Option<bool>,
    live: Option<bool>,
) -> Option<bool> {
    manifest.or(local).or(live)
}

/// The Infrastructure manifest's origin-firewall toggle, read the way
/// `apply::run` reads it — `APPRAFTER_MANIFEST` or nothing (A4).
///
/// Best-effort, like [`live_origin_firewall`]: a bootstrap must never fail over
/// a field nothing in the cluster reads. An unparseable manifest would already
/// have failed `apply` before any node existed, so the only route here is a
/// standalone `cluster-bootstrap` against a cluster that is already up — where
/// falling through to the target store is the behaviour that shipped. It is
/// logged rather than swallowed, because falling through silently is how a
/// wrong value gets recorded as if it were an answer.
fn manifest_origin_firewall() -> Option<bool> {
    let path = std::env::var("APPRAFTER_MANIFEST").ok()?;
    let cwd = std::env::current_dir().ok()?;
    manifest_origin_firewall_at(&cwd, Path::new(&path))
}

/// The half of [`manifest_origin_firewall`] that does not touch the process
/// environment, so it is testable without the cross-test races `set_var` in a
/// threaded runner invites. Same split `apply` uses for `cf_origin_enabled`.
fn manifest_origin_firewall_at(cwd: &Path, path: &Path) -> Option<bool> {
    match manifest::parse_infrastructure(cwd, path) {
        Ok(m) => origin_firewall_of(&m),
        Err(e) => {
            info!(
                path = %path.display(),
                error = %e,
                "APPRAFTER_MANIFEST did not parse — the origin-firewall intent \
                 recorded in the cluster falls back to the target store"
            );
            None
        }
    }
}

/// The toggle an Infrastructure manifest declares, or `None` when it is silent.
/// The same expression `apply::cf_origin_enabled` uses for its first rung —
/// deliberately, since the two must not disagree about what a manifest says.
fn origin_firewall_of(m: &InfrastructureManifest) -> Option<bool> {
    m.spec.firewall.as_ref().and_then(|f| f.cloudflare_origin)
}

/// Read the origin-firewall intent already recorded in the cluster, so the
/// bootstrap render can carry it forward rather than prune it (see
/// [`origin_firewall_for_render`]).
///
/// Best-effort by construction: on a fresh provision the CRD is not installed
/// yet (Argo CD applies it during this very bootstrap), so the read fails and
/// the answer is `None` — which is correct, there is nothing to preserve. A
/// bootstrap must never fail over a field nothing in the cluster reads.
fn live_origin_firewall<K: KubectlRunner>(kubectl: &K, kubeconfig_path: &Path) -> Option<bool> {
    let body = kubectl
        .get_raw(PLATFORMSTACK_DEFAULT_RAW_PATH, kubeconfig_path)
        .ok()?;
    let stack: serde_json::Value = serde_json::from_str(&body).ok()?;
    crate::commands::target_firewall::recorded_origin_firewall(&stack)
}

/// Render the default `PlatformStack` CR YAML the loader applies
/// once the platform Application reports Healthy. Singleton —
/// name=default, namespace=apprafter-system. Webhook enforces
/// both fields plus the rest of the contract.
///
/// `tier` is the active CLI target's tier (1..=4). `domain`
/// is optional — tier 1 deployments without a public domain
/// omit the field entirely and rely on the chart's defaults.
///
/// `origin_firewall` is the resolved intent from
/// [`origin_firewall_for_render`] — `None` emits NO `firewall:` block at all,
/// which is what keeps this apply from claiming (and then pruning) a field
/// nobody has an opinion about.
pub(crate) fn render_platformstack_default(
    tier: u8,
    domain: Option<&str>,
    origin_firewall: Option<bool>,
) -> String {
    let domain_line = match domain {
        Some(d) => format!("    domain: \"{d}\"\n"),
        None => String::new(),
    };
    // Absent, NOT `false`: the CR's three-valued field is the record a backup
    // carries, and "the operator never said" is a different fact from "the
    // operator said no".
    let firewall_block = match origin_firewall {
        Some(on) => format!("  firewall:\n    cloudflareOrigin: {on}\n"),
        None => String::new(),
    };
    // Tier-1 auto-upgrade is the OPT-OUT default at bootstrap because
    // non-safe platform diffs become a MigrationPlan (ADR 0026) — so
    // auto-advance is safe and matches the converge-by-default UX; an
    // operator opts OUT with `spec.autoUpgrade: false`. The bare
    // CUE/CRD schema default stays `false` (safe for raw/non-bootstrap
    // creation) and is intentionally NOT changed here. Tier 1 is the
    // only wired tier today; other tiers fall back to the schema-safe
    // `false`.
    let auto_upgrade = matches!(tier, 1);
    format!(
        r#"# SPDX-License-Identifier: FSL-1.1-Apache-2.0
# Rendered by `apprafter cluster-bootstrap` (Track B.1.72).
# The PlatformStack singleton is the declarative control plane
# for the platform version. PlatformController (B.1.73) will
# reconcile spec changes; in 1.72 the CR exists with empty
# status until then.
apiVersion: apprafter.io/v1alpha1
kind: PlatformStack
metadata:
  name: default
  namespace: apprafter-system
spec:
  channel: stable
  autoUpgrade: {auto_upgrade}
{firewall_block}  source:
    upstream: "oci://{repo}/{chart}"
    repoURL: "oci://{repo}/{chart}"
    checkInterval: 6h
  values:
    tier: {tier}
{domain_line}"#,
        auto_upgrade = auto_upgrade,
        firewall_block = firewall_block,
        repo = APPRAFTER_PLATFORM_STACK_DEFAULT_REPO,
        chart = APPRAFTER_PLATFORM_STACK_CHART_NAME,
        tier = tier,
        domain_line = domain_line,
    )
}

fn platform_stack_repo() -> String {
    APPRAFTER_PLATFORM_STACK_DEFAULT_REPO.to_string()
}

/// Resolve the platform-stack chart version the loader pins
/// the root Application to.
///
/// Walk-fix #9 post-B.1.79a (v0.1.159): used to return the
/// baked-in `RELEASED_PLATFORM_STACK_VERSION` constant
/// verbatim — every chart-only bump forced a CLI rebuild +
/// release just to keep fresh installs current. New flow
/// fetches the latest `platform-stack/v*` tag from the
/// upstream GitHub Releases API at bootstrap time and uses
/// that; the baked constant survives as a fallback for
/// air-gapped / firewalled / network-broken installs.
///
/// See `cli_providers::k8s::channel_latest` for the resolver.
fn platform_stack_version() -> String {
    cli_providers::k8s::resolve_latest_platform_stack_version()
}

fn decrypt_cached_kubeconfig(hetzner: &cli_state::HetznerCloudState) -> Result<String> {
    if let Some(armored) = &hetzner.kubeconfig_age {
        let identity = load_or_create_identity(&default_age_key_path())?;
        return decrypt_with_identity(armored, &identity);
    }
    if let Some(plain) = &hetzner.kubeconfig_yaml {
        return Ok(plain.clone());
    }
    Err(CliError::Other(
        "no cached kubeconfig in state; run `apprafter kubeconfig` first".to_string(),
    ))
}

fn write_tempfile_with(prefix: &str, contents: &str) -> Result<NamedTempFile> {
    let mut f = tempfile::Builder::new()
        .prefix(prefix)
        .tempfile()
        .map_err(|e| CliError::Other(format!("create tempfile {prefix}: {e}")))?;
    f.write_all(contents.as_bytes())
        .map_err(|e| CliError::Other(format!("write tempfile {prefix}: {e}")))?;
    Ok(f)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::path::PathBuf;

    // FakeHelm + FakeKubectl record the calls perform_bootstrap
    // makes so each assertion below can pin the exact sequence.

    #[derive(Default)]
    struct FakeHelm {
        repos: RefCell<Vec<(String, String)>>,
        installs: RefCell<Vec<HelmUpgradeArgs>>,
        /// Configurable answer for `release_is_current`. Default
        /// `false` so the existing tests still exercise the upgrade
        /// path; the skip-path test flips it to `true`. Records the
        /// fingerprints it was queried with for assertions.
        current: bool,
        current_queries: RefCell<Vec<(String, String)>>,
    }

    impl HelmRunner for FakeHelm {
        fn repo_add(&self, name: &str, url: &str) -> Result<()> {
            self.repos.borrow_mut().push((name.into(), url.into()));
            Ok(())
        }
        fn upgrade_install(&self, args: &HelmUpgradeArgs) -> Result<()> {
            self.installs.borrow_mut().push(args.clone());
            Ok(())
        }
        fn release_is_current(
            &self,
            release: &str,
            _namespace: &str,
            _kubeconfig: &Path,
            fingerprint: &str,
        ) -> Result<bool> {
            self.current_queries
                .borrow_mut()
                .push((release.to_string(), fingerprint.to_string()));
            Ok(self.current)
        }
    }

    #[derive(Default)]
    struct FakeKubectl {
        applies: RefCell<Vec<(ManifestSource, PathBuf)>>,
        ssa_applies: RefCell<Vec<(ManifestSource, PathBuf, String)>>,
        waits: RefCell<Vec<WaitCall>>,
        /// Every file this runner was handed, slurped AT CALL TIME.
        ///
        /// `run_with` writes the kubeconfig and both manifests into
        /// `NamedTempFile`s that are deleted the moment it returns, so a
        /// test that recorded only paths would have nothing left to read
        /// by assertion time. Reading eagerly is what lets the phase-3
        /// target-resolution tests below assert on CONTENT — which
        /// target's kubeconfig, which target's tier.
        slurped: RefCell<Vec<String>>,
        /// Body `get_raw` answers with. `None` (the default) errors, which
        /// is what a FRESH cluster does: the `platformstacks` CRD is applied
        /// by this very bootstrap, so the pre-render read of the existing CR
        /// cannot succeed there. Set it to stand in for a re-bootstrap of a
        /// cluster that already carries a PlatformStack.
        raw_body: RefCell<Option<String>>,
        /// Raw paths this runner was asked for.
        raw_gets: RefCell<Vec<String>>,
    }

    impl FakeKubectl {
        /// Record a file's contents. An unreadable path records the
        /// error rather than skipping: a silent skip would let a
        /// content assertion pass by finding nothing to contradict it.
        fn slurp(&self, path: &Path) {
            let body = std::fs::read_to_string(path)
                .unwrap_or_else(|e| format!("<unreadable {}: {e}>", path.display()));
            self.slurped.borrow_mut().push(body);
        }

        /// Everything slurped, concatenated — the corpus the phase-3
        /// tests match markers against.
        fn slurped_corpus(&self) -> String {
            self.slurped.borrow().join("\n---\n")
        }
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct WaitCall {
        resource_ref: String,
        namespace: Option<String>,
        condition_expr: String,
        timeout_seconds: u64,
        kubeconfig_path: PathBuf,
    }

    impl KubectlRunner for FakeKubectl {
        fn apply_manifest(&self, source: &ManifestSource, kubeconfig_path: &Path) -> Result<()> {
            self.applies
                .borrow_mut()
                .push((source.clone(), kubeconfig_path.to_path_buf()));
            Ok(())
        }
        fn apply_manifest_server_side(
            &self,
            source: &ManifestSource,
            kubeconfig_path: &Path,
            field_manager: &str,
        ) -> Result<()> {
            if let ManifestSource::Path(p) = source {
                self.slurp(p);
            }
            self.ssa_applies.borrow_mut().push((
                source.clone(),
                kubeconfig_path.to_path_buf(),
                field_manager.to_string(),
            ));
            Ok(())
        }
        fn get_secret_value(&self, _: &str, _: &str, _: &str, _: &Path) -> Result<String> {
            unreachable!("cluster-bootstrap never reads secrets in the GitOps loader path")
        }
        fn wait_for_condition(
            &self,
            resource_ref: &str,
            namespace: Option<&str>,
            condition_expr: &str,
            timeout_seconds: u64,
            kubeconfig_path: &Path,
        ) -> Result<()> {
            self.slurp(kubeconfig_path);
            self.waits.borrow_mut().push(WaitCall {
                resource_ref: resource_ref.to_string(),
                namespace: namespace.map(|s| s.to_string()),
                condition_expr: condition_expr.to_string(),
                timeout_seconds,
                kubeconfig_path: kubeconfig_path.to_path_buf(),
            });
            Ok(())
        }
        fn get_raw(&self, path: &str, _: &Path) -> Result<String> {
            self.raw_gets.borrow_mut().push(path.to_string());
            match self.raw_body.borrow().clone() {
                Some(body) => Ok(body),
                None => Err(cli_core::CliError::Other(
                    "the server doesn't have a resource type \"platformstacks\"".to_string(),
                )),
            }
        }
    }

    #[test]
    fn perform_bootstrap_installs_cilium_then_argocd_then_applies_root_then_waits_for_healthy() {
        let helm = FakeHelm::default();
        let kubectl = FakeKubectl::default();
        let kc = PathBuf::from("/tmp/kubeconfig");
        let root_app = PathBuf::from("/tmp/root-app.yaml");
        let platformstack = tempfile::NamedTempFile::new().unwrap();

        perform_bootstrap(&helm, &kubectl, &kc, &root_app, platformstack.path())
            .expect("bootstrap");

        // Helm: two repo_adds (cilium, argo) and two installs
        // (cilium first, then the loader Argo CD release).
        // cert-manager, operator, webhook reconcile via the
        // chart.
        let repos = helm.repos.borrow();
        assert_eq!(
            repos.as_slice(),
            &[
                ("cilium".to_string(), "https://helm.cilium.io/".to_string()),
                (
                    "argo".to_string(),
                    "https://argoproj.github.io/argo-helm".to_string()
                ),
            ]
        );
        let installs = helm.installs.borrow();
        assert_eq!(installs.len(), 2, "{installs:?}");
        assert_eq!(installs[0].release, "cilium");
        assert_eq!(installs[0].chart, "cilium/cilium");
        assert_eq!(installs[0].namespace, "kube-system");
        assert_eq!(installs[0].version.as_deref(), Some(CILIUM_CHART_VERSION));
        assert_eq!(installs[1].release, "argocd");
        assert_eq!(installs[1].chart, "argo/argo-cd");
        assert_eq!(installs[1].namespace, "argocd");
        assert_eq!(installs[1].version.as_deref(), Some(ARGOCD_CHART_VERSION));

        // kubectl: zero client-side applies (B.1.73 walk-fix #4
        // moved the root Application apply onto SSA with field
        // manager `apprafter-cli` — same convention as the
        // PlatformStack singleton), two SSA applies in order
        // (root Application THEN PlatformStack default).
        let applies = kubectl.applies.borrow();
        assert!(
            applies.is_empty(),
            "no client-side applies expected, got {applies:?}"
        );
        let ssa = kubectl.ssa_applies.borrow();
        // AppProjects (so the root App's project exists), THEN root
        // Application, THEN PlatformStack default.
        assert_eq!(ssa.len(), 3, "expected 3 SSA applies, got {ssa:?}");
        match &ssa[1].0 {
            ManifestSource::Path(p) => assert_eq!(p, &root_app),
            other => panic!("expected Path root-app at SSA[1], got {other:?}"),
        }
        assert_eq!(ssa[1].2, "apprafter-cli");
        assert_eq!(ssa[0].2, "apprafter-cli", "AppProjects applied as SSA[0]");

        // Waits: node Ready first (so step 1 can schedule),
        // argocd-server Available second, root Application
        // Synced third, root Application Healthy fourth, then
        // a two-stage wait per CRD (create + Established)
        // before the PlatformStack SSA apply at step 5. The
        // two-stage CRD wait closes both the v0.1.111 → v0.1.112
        // gap (CRD not Established yet) AND the v0.1.112 →
        // v0.1.113 gap (`kubectl wait` errors instantly on a
        // missing resource — `--for=create` polls for existence
        // before `--for=condition=Established` can run). See
        // rationale on `CRD_CREATE_TIMEOUT_SECS` +
        // `CRD_ESTABLISHED_TIMEOUT_SECS`.
        // Synced-before-Healthy is critical — a freshly-created
        // root Application reports Healthy trivially (zero
        // children) while Sync=Unknown on chart-pull failure.
        // Walk-found false-positive v0.1.99 → v0.1.100.
        let waits = kubectl.waits.borrow();
        assert_eq!(waits.len(), 10, "{waits:?}");
        assert_eq!(waits[0].resource_ref, "node --all");
        assert_eq!(waits[0].namespace, None);
        assert_eq!(waits[0].condition_expr, "condition=Ready");
        assert_eq!(waits[0].timeout_seconds, NODE_READY_TIMEOUT_SECS);

        assert_eq!(waits[1].resource_ref, "deployment/argocd-server");
        assert_eq!(waits[1].namespace, Some("argocd".to_string()));
        assert_eq!(waits[1].condition_expr, "condition=Available");
        assert_eq!(waits[1].timeout_seconds, ARGOCD_DEPLOYMENT_TIMEOUT_SECS);

        assert_eq!(waits[2].resource_ref, "applications.argoproj.io/platform");
        assert_eq!(waits[2].namespace, Some("argocd".to_string()));
        assert_eq!(
            waits[2].condition_expr,
            "jsonpath={.status.sync.status}=Synced"
        );
        assert_eq!(waits[2].timeout_seconds, PLATFORM_RECONCILE_TIMEOUT_SECS);

        assert_eq!(waits[3].resource_ref, "applications.argoproj.io/platform");
        assert_eq!(waits[3].namespace, Some("argocd".to_string()));
        assert_eq!(
            waits[3].condition_expr,
            "jsonpath={.status.health.status}=Healthy"
        );
        assert_eq!(waits[3].timeout_seconds, PLATFORM_RECONCILE_TIMEOUT_SECS);

        // Regression guard (walk-found at v0.2.10): every Argo CD
        // Application wait MUST be group-qualified
        // (`applications.argoproj.io/...`). A bare `application/...` is
        // ambiguous once the operator chart installs the
        // `applications.apprafter.io` CRD (the re-run / idempotency
        // path); kubectl mis-resolves it to `applications.apprafter.io`
        // and the wait fails `NotFound`.
        assert!(
            waits
                .iter()
                .all(|w| !w.resource_ref.starts_with("application/")),
            "Argo CD Application waits must be group-qualified, not the ambiguous bare `application/` form: {waits:?}"
        );

        assert_eq!(waits[4].resource_ref, "crd/applications.apprafter.io");
        assert_eq!(waits[4].namespace, None);
        assert_eq!(waits[4].condition_expr, "create");
        assert_eq!(waits[4].timeout_seconds, CRD_CREATE_TIMEOUT_SECS);

        assert_eq!(waits[5].resource_ref, "crd/applications.apprafter.io");
        assert_eq!(waits[5].namespace, None);
        assert_eq!(waits[5].condition_expr, "condition=Established");
        assert_eq!(waits[5].timeout_seconds, CRD_ESTABLISHED_TIMEOUT_SECS);

        assert_eq!(waits[6].resource_ref, "crd/platformstacks.apprafter.io");
        assert_eq!(waits[6].namespace, None);
        assert_eq!(waits[6].condition_expr, "create");
        assert_eq!(waits[6].timeout_seconds, CRD_CREATE_TIMEOUT_SECS);

        assert_eq!(waits[7].resource_ref, "crd/platformstacks.apprafter.io");
        assert_eq!(waits[7].namespace, None);
        assert_eq!(waits[7].condition_expr, "condition=Established");
        assert_eq!(waits[7].timeout_seconds, CRD_ESTABLISHED_TIMEOUT_SECS);

        assert_eq!(waits[8].resource_ref, "crd/sourcecredentials.apprafter.io");
        assert_eq!(waits[8].namespace, None);
        assert_eq!(waits[8].condition_expr, "create");
        assert_eq!(waits[8].timeout_seconds, CRD_CREATE_TIMEOUT_SECS);

        assert_eq!(waits[9].resource_ref, "crd/sourcecredentials.apprafter.io");
        assert_eq!(waits[9].namespace, None);
        assert_eq!(waits[9].condition_expr, "condition=Established");
        assert_eq!(waits[9].timeout_seconds, CRD_ESTABLISHED_TIMEOUT_SECS);
    }

    #[test]
    fn crd_established_waits_run_after_root_healthy_and_before_platformstack_apply() {
        // Regression guard for walk-fix v0.1.111 → v0.1.112 and
        // v0.1.112 → v0.1.113. Per CRD we issue TWO waits
        // (`--for=create`, then `condition=Established`) — four
        // CRD-prefixed waits total — and ALL must sit between
        // the root App Healthy wait (waits[3]) and the SSA
        // apply (recorded in `ssa_applies`). Reordering would
        // re-introduce one of the races: missing CRD object
        // (NotFound) or CRD-not-yet-Established (no matches for
        // kind) — operators saw both on consecutive walks.
        let helm = FakeHelm::default();
        let kubectl = FakeKubectl::default();
        let kc = PathBuf::from("/tmp/kubeconfig");
        let root_app = PathBuf::from("/tmp/root-app.yaml");
        let platformstack = tempfile::NamedTempFile::new().unwrap();

        perform_bootstrap(&helm, &kubectl, &kc, &root_app, platformstack.path())
            .expect("bootstrap");

        let waits = kubectl.waits.borrow();
        let crd_wait_positions: Vec<usize> = waits
            .iter()
            .enumerate()
            .filter(|(_, w)| w.resource_ref.starts_with("crd/"))
            .map(|(i, _)| i)
            .collect();
        assert_eq!(
            crd_wait_positions.len(),
            6,
            "expected exactly six CRD waits (2 per CRD × 3 CRDs), got {crd_wait_positions:?}"
        );
        // All CRD waits must come AFTER the App Healthy wait.
        assert!(
            crd_wait_positions[0] > 3,
            "first CRD wait must follow waits[3] (Healthy); positions: {crd_wait_positions:?}"
        );
        // For each CRD, the `--for=create` wait must precede
        // the `--for=condition=Established` wait. kubectl wait
        // errors fast on a missing resource, so the
        // create-wait MUST land first.
        assert_eq!(waits[crd_wait_positions[0]].condition_expr, "create");
        assert_eq!(
            waits[crd_wait_positions[1]].condition_expr,
            "condition=Established"
        );
        assert_eq!(waits[crd_wait_positions[2]].condition_expr, "create");
        assert_eq!(
            waits[crd_wait_positions[3]].condition_expr,
            "condition=Established"
        );
        assert_eq!(waits[crd_wait_positions[4]].condition_expr, "create");
        assert_eq!(
            waits[crd_wait_positions[5]].condition_expr,
            "condition=Established"
        );
        // And the SSA applies happen after the waits return; the
        // FakeKubectl records them in `ssa_applies` so the post-
        // perform_bootstrap snapshot pins the ordering. Count is 3:
        // AppProjects, root Application, then PlatformStack default.
        assert_eq!(kubectl.ssa_applies.borrow().len(), 3);
    }

    #[test]
    fn root_application_repourl_is_bare_without_oci_scheme() {
        // Companion to argocd_loader_values_register_apprafter_oci_repo.
        // The root Application's repoURL MUST match the
        // registration URL byte-for-byte (`ghcr.io/apprafter`)
        // — Argo CD does not normalise `oci://x` to `x`.
        let yaml = render_root_application(APPRAFTER_PLATFORM_STACK_DEFAULT_REPO, "0.1.4");
        assert!(yaml.contains("repoURL: \"ghcr.io/apprafter\""), "{yaml}");
        assert!(
            !yaml.contains("oci://"),
            "root Application repoURL must be bare: {yaml}"
        );
    }

    #[test]
    fn cilium_installs_before_argocd_so_node_can_become_ready() {
        // Regression guard for the v0.1.97 → v0.1.99 catch-22:
        // k3s starts without a CNI; the node carries
        // `node.kubernetes.io/not-ready:NoSchedule` until
        // Cilium installs. Argo CD's pre-install hook Job pod
        // doesn't tolerate it and stays Pending forever, helm
        // install times out with `failed pre-install`. Cilium
        // pods DO tolerate the taint, so they schedule and
        // flip the node to Ready. This test pins the ordering
        // so a future refactor can't put argocd back first.
        let helm = FakeHelm::default();
        let kubectl = FakeKubectl::default();
        let kc = PathBuf::from("/tmp/kubeconfig");
        let root_app = PathBuf::from("/tmp/root-app.yaml");
        let platformstack = tempfile::NamedTempFile::new().unwrap();

        perform_bootstrap(&helm, &kubectl, &kc, &root_app, platformstack.path())
            .expect("bootstrap");

        let installs = helm.installs.borrow();
        let cilium_idx = installs.iter().position(|i| i.release == "cilium");
        let argocd_idx = installs.iter().position(|i| i.release == "argocd");
        assert!(cilium_idx.is_some(), "cilium install missing");
        assert!(argocd_idx.is_some(), "argocd install missing");
        assert!(
            cilium_idx < argocd_idx,
            "cilium must install before argocd: cilium={cilium_idx:?} argocd={argocd_idx:?}"
        );

        // Node-Ready wait must come between cilium install
        // (otherwise CNI isn't installed yet) and argocd
        // install (otherwise the pre-install hook is stuck).
        let waits = kubectl.waits.borrow();
        let node_wait_idx = waits.iter().position(|w| w.resource_ref == "node --all");
        assert_eq!(
            node_wait_idx,
            Some(0),
            "node Ready wait must be the FIRST kubectl call: {waits:?}"
        );
    }

    #[test]
    fn render_app_projects_emits_the_three_referenced_projects() {
        let y = render_app_projects();
        // Exactly the three projects from `_appProjects`, so the root
        // `platform` Application's project exists at bootstrap.
        assert_eq!(y.matches("kind: AppProject").count(), 3, "{y}");
        assert!(y.contains("name: default"), "{y}");
        assert!(y.contains("name: platform\n"), "{y}");
        assert!(y.contains("name: platform-providers"), "{y}");
        // Adopted by the umbrella on first sync — same wave + labels.
        assert!(y.contains("argocd.argoproj.io/sync-wave: \"-30\""), "{y}");
        assert!(y.contains("apprafter.io/managed-by: apprafter"), "{y}");
    }

    #[test]
    fn render_root_application_includes_repo_url_and_chart_version() {
        let yaml = render_root_application("oci://ghcr.io/myorg", "0.1.2");
        assert!(yaml.contains("repoURL: \"oci://ghcr.io/myorg\""));
        assert!(yaml.contains("targetRevision: \"0.1.2\""));
        assert!(yaml.contains("chart: platform-stack"));
        // syncPolicy lets Argo CD prune + self-heal child
        // Applications when the chart removes / drifts a
        // component. Critical for upgrades.
        assert!(yaml.contains("prune: true"));
        assert!(yaml.contains("selfHeal: true"));
    }

    #[test]
    fn render_root_application_ignores_gateway_api_defaulting() {
        // 1.83a F3 (live-Argo-DIFF-confirmed): without these the platform
        // Gateway + redirect HTTPRoute sit permanently OutOfSync because the
        // apiserver defaults fields the chart leaves unset.
        let yaml = render_root_application(APPRAFTER_PLATFORM_STACK_DEFAULT_REPO, "0.2.0");
        assert!(yaml.contains("ignoreDifferences:"), "{yaml}");
        // Gateway certificateRef group (apiserver fills empty core group) —
        // `select(.tls)` skips the tls-less http listener so the jq doesn't
        // error out and silently drop the whole ignore.
        assert!(
            yaml.contains(".spec.listeners[] | select(.tls) | .tls.certificateRefs[].group"),
            "{yaml}"
        );
        // HTTPRoute parentRef + match-all defaulting.
        assert!(yaml.contains(".spec.parentRefs[].group"), "{yaml}");
        assert!(yaml.contains(".spec.parentRefs[].kind"), "{yaml}");
        assert!(yaml.contains(".spec.rules[].matches"), "{yaml}");
    }

    #[test]
    fn render_root_application_joins_default_app_project() {
        // The root platform Application lives in the `default`
        // AppProject so it shows up in the default operator Argo
        // view next to the user's apps — separate from the
        // platform component Applications (which stay in
        // `platform`). `default` is one of the projects
        // `render_app_projects` emits at bootstrap (and is fully
        // unrestricted), applied before this manifest, so the
        // reference resolves. Regression guard: a flip back to
        // `platform` would re-bury the root App away from the
        // default operator view; this test catches it at
        // unit-test time instead of at runtime.
        let yaml = render_root_application(APPRAFTER_PLATFORM_STACK_DEFAULT_REPO, "0.1.0");
        assert!(
            yaml.contains("project: default"),
            "root Application should join the `default` AppProject, got:\n{yaml}"
        );
        assert!(
            !yaml.contains("project: platform"),
            "root Application must NOT use the `platform` project (that's for component apps), got:\n{yaml}"
        );

        // The referenced `default` AppProject is actually emitted
        // at bootstrap, so the project exists when this manifest
        // applies — guards the comment's claim.
        assert!(
            render_app_projects().contains("name: default"),
            "render_app_projects must emit the `default` AppProject the root Application references"
        );
    }

    #[test]
    fn render_root_application_uses_argocd_namespace_destination() {
        let yaml = render_root_application(APPRAFTER_PLATFORM_STACK_DEFAULT_REPO, "0.1.0");
        // The Application CRD lives in `argocd` namespace.
        // Sub-charts target the cluster (kubernetes.default.svc).
        assert!(yaml.contains("namespace: argocd"));
        assert!(yaml.contains("server: https://kubernetes.default.svc"));
    }

    #[test]
    fn decrypt_cached_kubeconfig_prefers_age_then_falls_back_to_plaintext() {
        let hetzner = cli_state::HetznerCloudState {
            server_id: 1,
            server_name: "n".into(),
            server_type: None,
            ssh_key_ids: vec![],
            network_id: None,
            firewall_id: None,
            floating_ip_ids: vec![],
            kubeconfig_yaml: Some("apiVersion: v1\nfrom: legacy\n".into()),
            kubeconfig_age: None,
            argocd_admin_password_age: None,
        };
        let out = decrypt_cached_kubeconfig(&hetzner).unwrap();
        assert!(out.contains("from: legacy"), "{out}");
    }

    #[test]
    fn decrypt_cached_kubeconfig_errors_when_neither_field_set() {
        let hetzner = cli_state::HetznerCloudState {
            server_id: 1,
            server_name: "n".into(),
            server_type: None,
            ssh_key_ids: vec![],
            network_id: None,
            firewall_id: None,
            floating_ip_ids: vec![],
            kubeconfig_yaml: None,
            kubeconfig_age: None,
            argocd_admin_password_age: None,
        };
        let err = decrypt_cached_kubeconfig(&hetzner).unwrap_err();
        let msg = format!("{err:?}");
        assert!(msg.contains("kubeconfig"), "{msg}");
    }

    #[test]
    fn step_5_ssa_applies_platformstack_with_loader_field_manager() {
        let helm = FakeHelm::default();
        let kubectl = FakeKubectl::default();
        let kubeconfig = tempfile::NamedTempFile::new().unwrap();
        let root_app = tempfile::NamedTempFile::new().unwrap();
        let platformstack = tempfile::NamedTempFile::new().unwrap();

        perform_bootstrap(
            &helm,
            &kubectl,
            kubeconfig.path(),
            root_app.path(),
            platformstack.path(),
        )
        .unwrap();

        // SSA order: AppProjects (step 2b), root App (step 3),
        // PlatformStack (step 5). Step 5 is the THIRD ssa entry.
        let ssa = kubectl.ssa_applies.borrow();
        assert_eq!(ssa.len(), 3);
        match &ssa[2].0 {
            ManifestSource::Path(p) => assert_eq!(p, platformstack.path()),
            other => panic!("expected Path SSA source at [2], got {other:?}"),
        }
        assert_eq!(ssa[2].2, "apprafter-cli");
    }

    #[test]
    fn step_3_ssa_applies_root_application_with_loader_field_manager() {
        // Walk-fix #4 v0.1.118: the root Application is SSA-
        // applied (was client-side `kubectl apply` in prior
        // versions). Field manager is `apprafter-cli` — the
        // same one the PlatformStack singleton uses — and
        // PlatformController's `WHITELISTED_FIELD_MANAGERS`
        // includes it so the bootstrap state doesn't trip
        // `UnauthorizedSourceModification=True`.
        let helm = FakeHelm::default();
        let kubectl = FakeKubectl::default();
        let kubeconfig = tempfile::NamedTempFile::new().unwrap();
        let root_app = tempfile::NamedTempFile::new().unwrap();
        let platformstack = tempfile::NamedTempFile::new().unwrap();

        perform_bootstrap(
            &helm,
            &kubectl,
            kubeconfig.path(),
            root_app.path(),
            platformstack.path(),
        )
        .unwrap();

        let ssa = kubectl.ssa_applies.borrow();
        assert_eq!(ssa.len(), 3);
        // SSA[0] = AppProjects (step 2b), SSA[1] = root Application.
        match &ssa[1].0 {
            ManifestSource::Path(p) => assert_eq!(p, root_app.path()),
            other => panic!("expected Path root-app at SSA[1], got {other:?}"),
        }
        assert_eq!(ssa[1].2, "apprafter-cli");
        // The historical client-side apply path must be empty.
        assert!(
            kubectl.applies.borrow().is_empty(),
            "no client-side apply expected, got {:?}",
            kubectl.applies.borrow()
        );
    }

    #[test]
    fn render_platformstack_default_includes_tier_and_domain() {
        let yaml = render_platformstack_default(2, Some("example.com"), None);
        assert!(yaml.contains("name: default"));
        assert!(yaml.contains("namespace: apprafter-system"));
        assert!(yaml.contains("channel: stable"));
        assert!(yaml.contains("tier: 2"));
        assert!(yaml.contains("domain: \"example.com\""));
        assert!(yaml.contains("checkInterval: 6h"));
        // Non-T1 tiers fall back to the schema-safe `false` at bootstrap.
        assert!(yaml.contains("autoUpgrade: false"));
    }

    #[test]
    fn render_platformstack_default_omits_domain_when_unset() {
        let yaml = render_platformstack_default(1, None, None);
        assert!(yaml.contains("tier: 1"));
        assert!(!yaml.contains("domain:"));
    }

    /// A4, the WRITE half: a target that records the origin firewall puts it
    /// in the CR, which is the only place a backup can see it. Both values,
    /// because `false` is a real answer — an operator who turned the toggle
    /// off has said something, and a restore reading that must not treat it
    /// as "unknown".
    #[test]
    fn render_platformstack_default_writes_the_recorded_origin_firewall() {
        let on = render_platformstack_default(1, None, Some(true));
        assert!(
            on.contains("  firewall:\n    cloudflareOrigin: true\n"),
            "{on}"
        );
        let off = render_platformstack_default(1, None, Some(false));
        assert!(
            off.contains("  firewall:\n    cloudflareOrigin: false\n"),
            "{off}"
        );
        // The block is a sibling of `source:`/`values:`, not swallowed by
        // either — a wrongly-indented block would be a `source` sub-field and
        // the apiserver would reject the whole apply.
        assert!(on.contains("cloudflareOrigin: true\n  source:\n"), "{on}");
    }

    /// THE SSA OMISSION DECISION (A4). An unknown intent emits NO `firewall:`
    /// block — it does NOT emit `false`.
    ///
    /// This apply is server-side, under field manager `apprafter-cli`, and it
    /// runs on every `apprafter apply`. The sequence that matters: `restore
    /// --reprovision --target new` lands `cloudflareOrigin: true` from the
    /// snapshot onto a target whose local store is still empty (`target add`
    /// writes `firewall: None`). If the next bootstrap rendered `false` here,
    /// it would overwrite the restored answer with a wrong one, and every
    /// backup afterwards would carry "the source had its ports open".
    /// Omitting leaves the field to whoever owns it.
    #[test]
    fn render_platformstack_default_emits_no_firewall_block_when_unknown() {
        let yaml = render_platformstack_default(1, None, None);
        assert!(
            !yaml.contains("firewall"),
            "an unknown toggle must write NOTHING, not `false`: {yaml}"
        );
        assert!(
            !yaml.contains("cloudflareOrigin"),
            "an unknown toggle must write NOTHING, not `false`: {yaml}"
        );
    }

    /// The other half of the same trap: omitting is only safe while nobody
    /// has an answer. `apprafter-cli` owns this field the moment it writes
    /// it, and under one field manager omission is REMOVAL — so a second
    /// bootstrap from a machine whose target store says nothing would prune
    /// a value the cluster legitimately holds (a restore put it there, or
    /// another workstation did). The live value is carried forward for
    /// exactly that case.
    ///
    /// The local store still wins over the cluster when it HAS an opinion, in
    /// both directions: it is what `apprafter apply` builds the node's real
    /// firewall from, so a CR that disagreed would be a record of something
    /// untrue.
    #[test]
    fn origin_firewall_for_render_prefers_local_and_preserves_the_live_value() {
        // Nobody knows anything ⇒ write nothing.
        assert_eq!(origin_firewall_for_render(None, None, None), None);

        // The local store says nothing; the cluster does ⇒ carry it forward
        // rather than prune it. BOTH values, because preserving only `true`
        // would turn a recorded `false` into "unknown" on the next bootstrap.
        assert_eq!(
            origin_firewall_for_render(None, None, Some(true)),
            Some(true)
        );
        assert_eq!(
            origin_firewall_for_render(None, None, Some(false)),
            Some(false)
        );

        // The local store has an opinion ⇒ it wins, including when it
        // contradicts the cluster (the operator just ran `disable`).
        assert_eq!(
            origin_firewall_for_render(None, Some(false), Some(true)),
            Some(false)
        );
        assert_eq!(
            origin_firewall_for_render(None, Some(true), Some(false)),
            Some(true)
        );
        assert_eq!(
            origin_firewall_for_render(None, Some(true), None),
            Some(true)
        );
    }

    /// The manifest outranks the target store, matching
    /// `apply::cf_origin_enabled` — which is the whole point, since that is
    /// what builds the node's actual firewall. Recording anything else would
    /// put an intent in the CR that the node does not implement, and every
    /// later backup would carry that lie.
    #[test]
    fn origin_firewall_for_render_lets_the_manifest_outrank_the_target_store() {
        // The manifest wins in BOTH directions, not just when it says `true`:
        // an operator who declared `false` in Infrastructure while an old
        // target-store toggle still said `true` must not have the CR record
        // a firewall the node does not have.
        assert_eq!(
            origin_firewall_for_render(Some(true), Some(false), Some(false)),
            Some(true)
        );
        assert_eq!(
            origin_firewall_for_render(Some(false), Some(true), Some(true)),
            Some(false)
        );

        // With no manifest rung the function must be exactly what it was, or
        // the CLI-authored route silently changes meaning alongside the fix
        // for the manifest one.
        assert_eq!(
            origin_firewall_for_render(None, Some(true), Some(false)),
            Some(true)
        );
        assert_eq!(
            origin_firewall_for_render(None, None, Some(true)),
            Some(true)
        );
    }

    fn infra_manifest(value: serde_json::Value) -> InfrastructureManifest {
        serde_json::from_value(value).expect("valid InfrastructureManifest JSON")
    }

    /// A manifest that declares the toggle is read, in both directions — a
    /// declared `false` is an ANSWER, not an absence, or an operator who
    /// turned the firewall off in Infrastructure would silently inherit an
    /// old target-store `true` into the CR.
    #[test]
    fn a_manifest_that_declares_the_toggle_is_read_in_both_directions() {
        for declared in [true, false] {
            let m = infra_manifest(serde_json::json!({
                "apiVersion": "apprafter.io/v1alpha1",
                "kind": "Infrastructure",
                "metadata": {"name": "probe"},
                "spec": {
                    "provider": "hetzner-cloud",
                    "firewall": {"cloudflareOrigin": declared},
                },
            }));
            assert_eq!(
                origin_firewall_of(&m),
                Some(declared),
                "a manifest declaring {declared} must be read as such"
            );
        }
    }

    /// A manifest that says nothing about the firewall leaves the decision to
    /// the target store. Without this the manifest rung would shadow the CLI
    /// route for every IaC user, which is the opposite of the fix.
    #[test]
    fn a_manifest_silent_on_the_firewall_defers_to_the_target_store() {
        let silent = infra_manifest(serde_json::json!({
            "apiVersion": "apprafter.io/v1alpha1",
            "kind": "Infrastructure",
            "metadata": {"name": "probe"},
            "spec": {"provider": "hetzner-cloud"},
        }));
        assert_eq!(origin_firewall_of(&silent), None);

        // A firewall block carrying only ingress rules is equally silent about
        // the origin toggle — the two live in the same block, so reading the
        // block's PRESENCE as an answer would be the easy mistake.
        let ingress_only = infra_manifest(serde_json::json!({
            "apiVersion": "apprafter.io/v1alpha1",
            "kind": "Infrastructure",
            "metadata": {"name": "probe"},
            "spec": {
                "provider": "hetzner-cloud",
                "firewall": {"ingress": [{"port": "443"}]},
            },
        }));
        assert_eq!(origin_firewall_of(&ingress_only), None);
    }

    /// A path that does not parse must not become an ANSWER. Falling through
    /// to the target store is the behaviour that shipped; inventing `false`
    /// here would record a firewall-less intent for a cluster whose manifest
    /// may well have asked for one.
    #[test]
    fn an_unparseable_manifest_yields_no_opinion_rather_than_false() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            manifest_origin_firewall_at(dir.path(), Path::new("/nonexistent/infra.yaml")),
            None
        );
    }

    #[test]
    fn render_platformstack_default_tier1_auto_upgrade_is_opt_out() {
        // Tier-1 bootstrap default is opt-out auto-upgrade (true): the
        // MigrationPlan gate makes auto-advance safe (ADR 0026).
        let yaml = render_platformstack_default(1, None, None);
        assert!(yaml.contains("autoUpgrade: true"));
        assert!(!yaml.contains("autoUpgrade: false"));
    }

    #[test]
    fn render_platformstack_default_uses_apprafter_oci_repo() {
        let yaml = render_platformstack_default(1, None, None);
        assert!(yaml.contains("oci://ghcr.io/apprafter/platform-stack"));
    }

    #[test]
    fn skip_path_does_not_upgrade_but_still_applies_platform_and_waits() {
        // 2.4g: when both Cilium + Argo CD report `release_is_current`,
        // `perform_bootstrap` MUST skip the helm upgrade (no revision
        // bump) yet still proceed through the node-Ready / Argo-CD
        // Available waits, the SSA applies, and the CRD waits — a true
        // no-op re-run, not a half-bootstrap.
        let helm = FakeHelm {
            current: true,
            ..Default::default()
        };
        let kubectl = FakeKubectl::default();
        let kc = PathBuf::from("/tmp/kubeconfig");
        let root_app = PathBuf::from("/tmp/root-app.yaml");
        let platformstack = tempfile::NamedTempFile::new().unwrap();

        perform_bootstrap(&helm, &kubectl, &kc, &root_app, platformstack.path())
            .expect("bootstrap");

        // No upgrade_install for EITHER release — both were current.
        let installs = helm.installs.borrow();
        assert!(
            installs.is_empty(),
            "expected zero helm upgrades on the current-release skip path, got {installs:?}"
        );
        // Both releases were nonetheless QUERIED with their fingerprint.
        let queries = helm.current_queries.borrow();
        assert_eq!(queries.len(), 2, "{queries:?}");
        assert!(queries
            .iter()
            .any(|(r, fp)| r == "cilium" && !fp.is_empty()));
        assert!(queries
            .iter()
            .any(|(r, fp)| r == "argocd" && !fp.is_empty()));

        // The bootstrap still drives the platform forward: SSA applies
        // (AppProjects, root App, PlatformStack) and the full wait
        // sequence run regardless of the helm skip.
        assert_eq!(kubectl.ssa_applies.borrow().len(), 3);
        assert_eq!(kubectl.waits.borrow().len(), 10);
    }

    #[test]
    fn upgrade_path_injects_fingerprint_into_helm_args() {
        // 2.4g: when releases are NOT current (default fake → false),
        // each upgrade runs WITH a computed fingerprint set, so helm
        // records `apprafterLoaderHash` for the next run to read back.
        let helm = FakeHelm::default();
        let kubectl = FakeKubectl::default();
        let kc = PathBuf::from("/tmp/kubeconfig");
        let root_app = PathBuf::from("/tmp/root-app.yaml");
        let platformstack = tempfile::NamedTempFile::new().unwrap();

        perform_bootstrap(&helm, &kubectl, &kc, &root_app, platformstack.path())
            .expect("bootstrap");

        let installs = helm.installs.borrow();
        assert_eq!(installs.len(), 2, "{installs:?}");
        for inst in installs.iter() {
            assert!(
                inst.fingerprint.is_some(),
                "every upgraded release must carry a fingerprint: {inst:?}"
            );
        }
        // The fingerprint queried matches the one injected on upgrade
        // (same args → same `loader_fingerprint`).
        let queries = helm.current_queries.borrow();
        for inst in installs.iter() {
            let queried_fp = queries
                .iter()
                .find(|(r, _)| r == &inst.release)
                .map(|(_, fp)| fp.clone());
            assert_eq!(
                queried_fp.as_deref(),
                inst.fingerprint.as_deref(),
                "queried fingerprint must equal injected fingerprint for {}",
                inst.release
            );
        }
    }

    // ────────────────────────────────────────────────────────────────
    // Phase 3 target resolution (finding C1)
    //
    // `apprafter restore --reprovision --target X` runs this module as
    // phase 3 of `bootstrap_all::run`. Phases 1 and 2 honour the
    // override; phase 3 used to re-resolve the ACTIVE target and
    // bootstrap that cluster instead — `helm upgrade --install` of
    // Cilium and Argo CD plus a server-side apply of the root
    // `Application` and `PlatformStack/default` under field manager
    // `apprafter-cli`, resetting channel / autoUpgrade / targetRevision
    // on a cluster nobody named.
    //
    // Nothing caught it. Every test in `tests/bootstrap_all_test.rs` is
    // `--dry-run` or `--help` and stops before phase 3, and every DR
    // e2e makes the destination target active BEFORE restoring, with a
    // comment saying why — so the suite masked the bug rather than
    // finding it. These two tests run the whole phase-3 body against
    // fake helm/kubectl runners and assert on CONTENT: which target's
    // kubeconfig reached the cluster calls, and which target's tier was
    // written into the applied `PlatformStack`.
    // ────────────────────────────────────────────────────────────────

    /// Serialises the tests below. Both redirect `APPRAFTER_CONFIG_DIR`,
    /// which is process-global while `cargo test` runs test functions on
    /// parallel threads.
    static CONFIG_DIR_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Fixture: a target store holding two fully-populated targets.
    ///
    /// * `origin` — the ACTIVE target. Tier `team` (2), kubeconfig body
    ///   carries the marker `ORIGIN-CLUSTER`.
    /// * `dest` — the target a `restore --reprovision --target dest`
    ///   names. Tier `solo` (1), marker `DEST-CLUSTER`.
    ///
    /// Both are complete, so neither errors out early; the run reaches
    /// `perform_bootstrap` either way and the only difference visible to
    /// the runners is which target's material it carried.
    fn two_target_store() -> tempfile::TempDir {
        use cli_core::target::{
            save_global_config, save_target, GlobalConfig, Target, TargetConfig, TargetCredentials,
            TargetStorePaths,
        };
        use cli_state::{HetznerCloudState, State, StatePaths};

        let dir = tempfile::tempdir().expect("tempdir");
        let store = TargetStorePaths::for_root(dir.path().to_path_buf());

        for (name, tier, marker) in [
            ("origin", "team", "ORIGIN-CLUSTER"),
            ("dest", "solo", "DEST-CLUSTER"),
        ] {
            save_target(
                &store,
                &Target {
                    name: name.to_string(),
                    config: TargetConfig {
                        provider: "hetzner-cloud".into(),
                        default_tier: Some(tier.into()),
                        ..Default::default()
                    },
                    credentials: TargetCredentials::default(),
                },
            )
            .expect("save target");

            // Plaintext kubeconfig, not the age-encrypted field: the
            // plaintext branch of `decrypt_cached_kubeconfig` needs no
            // key material, so the fixture never touches the operator's
            // real `~/.config/apprafter/age.key`.
            let state = State {
                hetzner_cloud: Some(HetznerCloudState {
                    server_id: 1,
                    server_name: format!("{name}-node"),
                    server_type: None,
                    ssh_key_ids: vec![],
                    network_id: None,
                    firewall_id: None,
                    floating_ip_ids: vec![],
                    kubeconfig_yaml: Some(format!("apiVersion: v1\nkind: Config\n# {marker}\n")),
                    kubeconfig_age: None,
                    argocd_admin_password_age: None,
                }),
                ..Default::default()
            };
            state
                .save(&StatePaths::for_active_target(&store, name))
                .expect("save state");
        }

        save_global_config(
            &store,
            &GlobalConfig {
                active_target: "origin".into(),
                ..Default::default()
            },
        )
        .expect("save global config");

        dir
    }

    /// Run `run_with` with `APPRAFTER_CONFIG_DIR` pointed at `root`.
    ///
    /// The env var is set and restored under [`CONFIG_DIR_LOCK`], and
    /// only the call itself runs inside the guard — assertions happen
    /// after the variable is back, so a failing assertion cannot leave
    /// the process pointed at a deleted tempdir.
    fn run_phase_three(
        root: &Path,
        target_override: Option<&str>,
    ) -> (FakeHelm, FakeKubectl, Result<()>) {
        run_phase_three_with(root, target_override, FakeKubectl::default())
    }

    /// [`run_phase_three`] with a pre-armed kubectl — used by the tests that
    /// need the pre-render read of the live `PlatformStack` to answer with
    /// something other than "no such resource".
    fn run_phase_three_with(
        root: &Path,
        target_override: Option<&str>,
        kubectl: FakeKubectl,
    ) -> (FakeHelm, FakeKubectl, Result<()>) {
        use cli_core::target::CONFIG_DIR_ENV;

        let helm = FakeHelm::default();

        let guard = CONFIG_DIR_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let saved = std::env::var_os(CONFIG_DIR_ENV);
        std::env::set_var(CONFIG_DIR_ENV, root);
        let outcome = run_with(&helm, &kubectl, target_override, || {
            // Stubbed: the real resolver HTTP-GETs the GitHub Releases
            // API, and phase-3 target resolution has nothing to do with
            // the network.
            "0.0.0-test".to_string()
        });
        match saved {
            Some(v) => std::env::set_var(CONFIG_DIR_ENV, v),
            None => std::env::remove_var(CONFIG_DIR_ENV),
        }
        drop(guard);

        (helm, kubectl, outcome)
    }

    /// THE C1 REGRESSION GUARD. With `origin` active, phase 3 invoked
    /// with `--target dest` must bootstrap `dest`.
    ///
    /// Drop the `target_override` argument anywhere along the chain —
    /// `resolve_state_paths(None)` or
    /// `load_active_target_config(&store, None)` — and this goes red:
    /// the first resolution carries `origin`'s kubeconfig, the second
    /// writes `origin`'s tier into the `PlatformStack`.
    #[test]
    fn phase_three_bootstraps_the_overridden_target_not_the_active_one() {
        let dir = two_target_store();
        let (_helm, kubectl, outcome) = run_phase_three(dir.path(), Some("dest"));
        outcome.expect("bootstrap against dest");

        let corpus = kubectl.slurped_corpus();

        // (1) State resolution — the kubeconfig every cluster call was
        //     handed came out of `dest`'s state.json.
        assert!(
            corpus.contains("DEST-CLUSTER"),
            "phase 3 must act on the kubeconfig cached for `dest`\n{corpus}"
        );
        assert!(
            !corpus.contains("ORIGIN-CLUSTER"),
            "phase 3 must NOT touch the active target `origin` (C1)\n{corpus}"
        );

        // (2) Target-config resolution — the tier written into the
        //     server-side-applied PlatformStack is `dest`'s (solo = 1),
        //     not `origin`'s (team = 2).
        assert!(
            corpus.contains("tier: 1"),
            "PlatformStack must carry `dest`'s tier (solo ⇒ 1)\n{corpus}"
        );
        assert!(
            !corpus.contains("tier: 2"),
            "PlatformStack must not carry `origin`'s tier (team ⇒ 2)\n{corpus}"
        );
    }

    /// The mirror image: with no override, phase 3 still resolves the
    /// active target. Guards the fix against over-correction — a
    /// signature that took the target by value, or a caller that passed
    /// something other than `None` for `cluster-bootstrap` / `platform
    /// rescue`, would show up here.
    #[test]
    fn phase_three_without_an_override_still_resolves_the_active_target() {
        let dir = two_target_store();
        let (_helm, kubectl, outcome) = run_phase_three(dir.path(), None);
        outcome.expect("bootstrap against the active target");

        let corpus = kubectl.slurped_corpus();
        assert!(
            corpus.contains("ORIGIN-CLUSTER"),
            "no override ⇒ the active target `origin`\n{corpus}"
        );
        assert!(
            !corpus.contains("DEST-CLUSTER"),
            "no override must not reach `dest`\n{corpus}"
        );
        assert!(
            corpus.contains("tier: 2"),
            "PlatformStack must carry `origin`'s tier (team ⇒ 2)\n{corpus}"
        );
    }

    /// A single complete target whose local config records `firewall`
    /// exactly as `local` says — `None` is a target that never ran the
    /// toggle, which is what `target add` writes.
    fn one_target_store(local: Option<bool>) -> tempfile::TempDir {
        use cli_core::target::{
            save_global_config, save_target, FirewallConfig, GlobalConfig, Target, TargetConfig,
            TargetCredentials, TargetStorePaths,
        };
        use cli_state::{HetznerCloudState, State, StatePaths};

        let dir = tempfile::tempdir().expect("tempdir");
        let store = TargetStorePaths::for_root(dir.path().to_path_buf());
        save_target(
            &store,
            &Target {
                name: "solo".into(),
                config: TargetConfig {
                    provider: "hetzner-cloud".into(),
                    default_tier: Some("solo".into()),
                    firewall: local.map(|cloudflare_origin| FirewallConfig { cloudflare_origin }),
                    ..Default::default()
                },
                credentials: TargetCredentials::default(),
            },
        )
        .expect("save target");
        State {
            hetzner_cloud: Some(HetznerCloudState {
                server_id: 1,
                server_name: "solo-node".into(),
                server_type: None,
                ssh_key_ids: vec![],
                network_id: None,
                firewall_id: None,
                floating_ip_ids: vec![],
                kubeconfig_yaml: Some("apiVersion: v1\nkind: Config\n# SOLO-CLUSTER\n".into()),
                kubeconfig_age: None,
                argocd_admin_password_age: None,
            }),
            ..Default::default()
        }
        .save(&StatePaths::for_active_target(&store, "solo"))
        .expect("save state");
        save_global_config(
            &store,
            &GlobalConfig {
                active_target: "solo".into(),
                ..Default::default()
            },
        )
        .expect("save global config");
        dir
    }

    /// END TO END (A4): the toggle an operator set on this target reaches the
    /// `PlatformStack` the bootstrap applies — which is what puts it inside
    /// every future backup, including the scheduled in-cluster one that has
    /// no target store to read.
    ///
    /// Asserted on the SLURPED APPLY, not on the render: a render that got it
    /// right and a caller that passed the wrong target's config (or none)
    /// would leave this red, which is the same seam the C1 guard above uses.
    #[test]
    fn phase_three_carries_the_targets_origin_firewall_into_the_platformstack() {
        let dir = one_target_store(Some(true));
        let (_helm, kubectl, outcome) = run_phase_three(dir.path(), None);
        outcome.expect("bootstrap against the active target");

        let corpus = kubectl.slurped_corpus();
        assert!(
            corpus.contains("cloudflareOrigin: true"),
            "the target's origin-firewall toggle must reach the applied \
             PlatformStack — nothing else can put it in a backup\n{corpus}"
        );
    }

    /// THE PRUNE GUARD (A4). A target that says nothing must not erase what
    /// the cluster already recorded.
    ///
    /// This is the "restore put it there, the local store never heard about
    /// it" shape: `restore --reprovision --target new` applies
    /// `cloudflareOrigin: true` from the snapshot, and a fresh `target add`
    /// wrote `firewall: None`. The PlatformStack apply is server-side under
    /// `apprafter-cli`, so an omitted field is a REMOVED field once this
    /// manager owns it — the render therefore reads the live value first and
    /// carries it forward.
    #[test]
    fn phase_three_preserves_an_origin_firewall_the_target_does_not_know_about() {
        let dir = one_target_store(None);
        let kubectl = FakeKubectl {
            raw_body: RefCell::new(Some(
                r#"{"kind":"PlatformStack","spec":{"firewall":{"cloudflareOrigin":true}}}"#
                    .to_string(),
            )),
            ..Default::default()
        };
        let (_helm, kubectl, outcome) = run_phase_three_with(dir.path(), None, kubectl);
        outcome.expect("bootstrap against the active target");

        assert_eq!(
            kubectl.raw_gets.borrow().as_slice(),
            &[PLATFORMSTACK_DEFAULT_RAW_PATH.to_string()],
            "the live PlatformStack must be read exactly once, before the apply"
        );
        let corpus = kubectl.slurped_corpus();
        assert!(
            corpus.contains("cloudflareOrigin: true"),
            "the cluster's own value must be carried forward, not pruned\n{corpus}"
        );
        assert!(
            !corpus.contains("cloudflareOrigin: false"),
            "and never rewritten to `false`\n{corpus}"
        );
    }

    /// The same shape on a FRESH cluster, where the read cannot succeed (the
    /// `platformstacks` CRD is applied by this very bootstrap): nobody has an
    /// answer, so the apply carries no `firewall` block at all — rather than
    /// a `false` that a later restore would have to argue with.
    #[test]
    fn phase_three_writes_no_firewall_block_when_nobody_has_an_answer() {
        let dir = one_target_store(None);
        let (_helm, kubectl, outcome) = run_phase_three(dir.path(), None);
        outcome.expect("bootstrap against the active target");

        let corpus = kubectl.slurped_corpus();
        assert!(
            corpus.contains("kind: PlatformStack"),
            "sanity: the PlatformStack apply is in the corpus\n{corpus}"
        );
        assert!(
            !corpus.contains("cloudflareOrigin"),
            "an unreadable/absent live value plus an empty target store means \
             UNKNOWN, and unknown writes nothing\n{corpus}"
        );
    }
}
