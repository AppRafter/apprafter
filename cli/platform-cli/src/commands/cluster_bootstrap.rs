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

use cli_core::secrets::{decrypt_with_identity, default_age_key_path, load_or_create_identity};
use cli_core::target::load_active_target_config;
use cli_core::{CliError, Result};
use cli_providers::k8s::{
    HelmCli, HelmRunner, HelmUpgradeArgs, KubectlCli, KubectlRunner, ManifestSource,
    APPRAFTER_CLI_FIELD_MANAGER, APPRAFTER_PLATFORM_STACK_CHART_NAME,
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

pub fn run() -> Result<()> {
    info!("cluster-bootstrap invoked (GitOps loader)");

    // Per-target state (v0.1.154). `cluster-bootstrap` has no
    // `--target` flag — it runs against the active target the
    // preceding `apply` populated, same as `argocd-password` and
    // the kubeconfig fetch.
    let resolved = resolve_state_paths(None)?;
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

    // Consult the active target for tier hint. Previously the
    // store handle was constructed inline (and tolerated a
    // missing config dir); per-target state resolution already
    // gives us a known-good `target_store`, so we just read off
    // it directly.
    let target_config = load_active_target_config(&target_store, None);

    let active_tier: u8 = target_config
        .as_ref()
        .and_then(|c| c.default_tier.as_deref())
        .and_then(|s| s.parse::<cli_core::Tier>().ok())
        .map(|t| t.level())
        .unwrap_or(1);

    let active_domain: Option<&str> = None;

    let platform_repo = platform_stack_repo();
    let platform_version = platform_stack_version();

    let root_app_yaml = render_root_application(&platform_repo, &platform_version);
    let root_app_file = write_tempfile_with("apprafter-root-application-", &root_app_yaml)?;

    let platformstack_yaml = render_platformstack_default(active_tier, active_domain);
    let platformstack_file =
        write_tempfile_with("apprafter-platformstack-default-", &platformstack_yaml)?;

    perform_bootstrap(
        &HelmCli,
        &KubectlCli,
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
        helm.upgrade_install(&HelmUpgradeArgs {
            release: "cilium".into(),
            chart: "cilium/cilium".into(),
            version: Some(CILIUM_CHART_VERSION.into()),
            namespace: "kube-system".into(),
            values_path: cilium_values_file.path().to_path_buf(),
            kubeconfig_path: kubeconfig_path.to_path_buf(),
            set_values: cilium_set_overrides(),
        })?;
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
    helm.upgrade_install(&HelmUpgradeArgs {
        release: "argocd".into(),
        chart: "argo/argo-cd".into(),
        version: Some(ARGOCD_CHART_VERSION.into()),
        namespace: "argocd".into(),
        values_path: argocd_values_file.path().to_path_buf(),
        kubeconfig_path: kubeconfig_path.to_path_buf(),
        set_values: Vec::new(),
    })?;

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
    kubectl.wait_for_condition(
        "application/platform",
        Some("argocd"),
        "jsonpath={.status.sync.status}=Synced",
        PLATFORM_RECONCILE_TIMEOUT_SECS,
        kubeconfig_path,
    )?;

    // 4b. Then wait for Healthy. After Synced, the child
    //     Applications exist; Healthy means their workloads
    //     reached the chart's health-check thresholds.
    kubectl.wait_for_condition(
        "application/platform",
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
    // `default` allows any destination server (legacy/ad-hoc);
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
            "Default project — Argo CD baseline, unrestricted (legacy + ad-hoc fallback).",
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
  # Track B.1.79a (chart 0.1.40): root platform Application
  # joins the dedicated `platform` AppProject. Project ships
  # with the initial Argo CD install through
  # `_loaderValues.argocd.values.configs.projects.platform`,
  # so it exists by the time this manifest is applied. Legacy
  # `default` project stays around for ad-hoc Applications
  # operators apply outside of `apprafter app add`.
  project: platform
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
"#,
        chart_name = APPRAFTER_PLATFORM_STACK_CHART_NAME,
        helm_block = helm_block,
    )
}

/// Render the default `PlatformStack` CR YAML the loader applies
/// once the platform Application reports Healthy. Singleton —
/// name=default, namespace=apprafter-system. Webhook enforces
/// both fields plus the rest of the contract.
///
/// `tier` is the active CLI target's tier (1..=4). `domain`
/// is optional — tier 1 deployments without a public domain
/// omit the field entirely and rely on the chart's defaults.
pub(crate) fn render_platformstack_default(tier: u8, domain: Option<&str>) -> String {
    let domain_line = match domain {
        Some(d) => format!("    domain: \"{d}\"\n"),
        None => String::new(),
    };
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
  autoUpgrade: false
  source:
    upstream: "oci://{repo}/{chart}"
    repoURL: "oci://{repo}/{chart}"
    checkInterval: 6h
  values:
    tier: {tier}
{domain_line}"#,
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
    }

    #[derive(Default)]
    struct FakeKubectl {
        applies: RefCell<Vec<(ManifestSource, PathBuf)>>,
        ssa_applies: RefCell<Vec<(ManifestSource, PathBuf, String)>>,
        waits: RefCell<Vec<WaitCall>>,
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
            self.waits.borrow_mut().push(WaitCall {
                resource_ref: resource_ref.to_string(),
                namespace: namespace.map(|s| s.to_string()),
                condition_expr: condition_expr.to_string(),
                timeout_seconds,
                kubeconfig_path: kubeconfig_path.to_path_buf(),
            });
            Ok(())
        }
        fn get_raw(&self, _: &str, _: &Path) -> Result<String> {
            unreachable!("cluster-bootstrap never reads raw API paths")
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

        assert_eq!(waits[2].resource_ref, "application/platform");
        assert_eq!(waits[2].namespace, Some("argocd".to_string()));
        assert_eq!(
            waits[2].condition_expr,
            "jsonpath={.status.sync.status}=Synced"
        );
        assert_eq!(waits[2].timeout_seconds, PLATFORM_RECONCILE_TIMEOUT_SECS);

        assert_eq!(waits[3].resource_ref, "application/platform");
        assert_eq!(waits[3].namespace, Some("argocd".to_string()));
        assert_eq!(
            waits[3].condition_expr,
            "jsonpath={.status.health.status}=Healthy"
        );
        assert_eq!(waits[3].timeout_seconds, PLATFORM_RECONCILE_TIMEOUT_SECS);

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
    fn render_root_application_joins_platform_app_project() {
        // Track B.1.79a chart 0.1.40 — root Application is a
        // platform-internal resource and must live in the
        // `platform` AppProject (declared in
        // `_loaderValues.argocd.values.configs.projects` so it
        // exists in the initial Argo CD install). The legacy
        // `default` project stays around for ad-hoc
        // Applications operators apply outside `apprafter app
        // add`, but the chart-managed root Application no
        // longer references it. Regression guard: if a future
        // refactor flips back to `default`, walks would
        // surface "AppProject default missing → root
        // Application Degraded" only at runtime; this test
        // fails at unit-test time instead.
        let yaml = render_root_application(APPRAFTER_PLATFORM_STACK_DEFAULT_REPO, "0.1.0");
        assert!(
            yaml.contains("project: platform"),
            "root Application should join the `platform` AppProject, got:\n{yaml}"
        );
        assert!(
            !yaml.contains("project: default"),
            "root Application must NOT use the legacy `default` project, got:\n{yaml}"
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
        let yaml = render_platformstack_default(2, Some("example.com"));
        assert!(yaml.contains("name: default"));
        assert!(yaml.contains("namespace: apprafter-system"));
        assert!(yaml.contains("channel: stable"));
        assert!(yaml.contains("tier: 2"));
        assert!(yaml.contains("domain: \"example.com\""));
        assert!(yaml.contains("checkInterval: 6h"));
    }

    #[test]
    fn render_platformstack_default_omits_domain_when_unset() {
        let yaml = render_platformstack_default(1, None);
        assert!(yaml.contains("tier: 1"));
        assert!(!yaml.contains("domain:"));
    }

    #[test]
    fn render_platformstack_default_uses_apprafter_oci_repo() {
        let yaml = render_platformstack_default(1, None);
        assert!(yaml.contains("oci://ghcr.io/apprafter/platform-stack"));
    }
}
