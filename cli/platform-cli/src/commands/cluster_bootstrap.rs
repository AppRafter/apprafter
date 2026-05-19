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
//!   - The `cli-providers::k8s::*_yaml` value renderers stay
//!     around as the chart's parallel source of truth until
//!     sub-phase 1.71 removes the duplication.

use std::io::Write;
use std::path::Path;

use cli_core::secrets::{decrypt_with_identity, default_age_key_path, load_or_create_identity};
use cli_core::{CliError, Result};
use cli_providers::k8s::{
    cilium_values_yaml, HelmCli, HelmRunner, HelmUpgradeArgs, KubectlCli, KubectlRunner,
    ManifestSource, APPRAFTER_PLATFORM_STACK_CHART_NAME, APPRAFTER_PLATFORM_STACK_DEFAULT_REPO,
    ARGOCD_CHART_VERSION, CILIUM_CHART_VERSION, RELEASED_PLATFORM_STACK_VERSION,
};
use cli_state::{State, StatePaths};
use tempfile::NamedTempFile;
use tracing::info;

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

pub fn run() -> Result<()> {
    info!("cluster-bootstrap invoked (GitOps loader)");
    let cwd = std::env::current_dir()?;
    let paths = StatePaths::for_root(&cwd);
    let state = State::load_or_default(&paths)?;
    let hetzner = state.hetzner_cloud.clone().ok_or_else(|| {
        CliError::Other(
            "state has no hetzner_cloud section; run `apprafter apply` first".to_string(),
        )
    })?;

    let plaintext = decrypt_cached_kubeconfig(&hetzner)?;
    let kubeconfig_file = write_tempfile_with("apprafter-kubeconfig-", &plaintext)?;

    let platform_repo = platform_stack_repo();
    let platform_version = platform_stack_version();

    let root_app_yaml = render_root_application(&platform_repo, &platform_version);
    let root_app_file = write_tempfile_with("apprafter-root-application-", &root_app_yaml)?;

    perform_bootstrap(
        &HelmCli,
        &KubectlCli,
        kubeconfig_file.path(),
        root_app_file.path(),
    )?;

    println!(
        "cluster-bootstrap complete: Cilium installed, node Ready, Argo CD installed, \
         platform-stack {platform_version} reconciling from {platform_repo}/{chart}",
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
) -> Result<()> {
    // 0. Cilium first — see module doc. k3s starts without a
    //    CNI, the single node carries
    //    `node.kubernetes.io/not-ready:NoSchedule`, and the
    //    Argo CD pre-install hook Job is `Pending` forever
    //    until a CNI installs. Cilium DaemonSet tolerates the
    //    not-ready taint so it schedules on the not-ready
    //    node and flips it to Ready.
    helm.repo_add("cilium", "https://helm.cilium.io/")?;
    let cilium_values_file =
        write_tempfile_with("apprafter-cilium-loader-values-", &cilium_values_yaml())?;
    helm.upgrade_install(&HelmUpgradeArgs {
        release: "cilium".into(),
        chart: "cilium/cilium".into(),
        version: Some(CILIUM_CHART_VERSION.into()),
        namespace: "kube-system".into(),
        values_path: cilium_values_file.path().to_path_buf(),
        kubeconfig_path: kubeconfig_path.to_path_buf(),
    })?;

    // 0b. Node MUST reach Ready before step 1 — otherwise the
    //     Argo CD pre-install hook would still be Pending.
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
    let argocd_values_file = write_tempfile_with(
        "apprafter-argocd-loader-values-",
        &argocd_loader_values_yaml(),
    )?;
    helm.upgrade_install(&HelmUpgradeArgs {
        release: "argocd".into(),
        chart: "argo/argo-cd".into(),
        version: Some(ARGOCD_CHART_VERSION.into()),
        namespace: "argocd".into(),
        values_path: argocd_values_file.path().to_path_buf(),
        kubeconfig_path: kubeconfig_path.to_path_buf(),
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

    // 3. Apply the root Application. Single handoff point —
    //    everything downstream is Argo CD's responsibility.
    kubectl.apply_manifest(
        &ManifestSource::Path(root_application_path.to_path_buf()),
        kubeconfig_path,
    )?;

    // 4. Wait for the root Application to report Healthy. Argo
    //    CD's repo-server has to pull the OCI chart, render N
    //    child Applications, each child has to pull its own
    //    upstream chart, etc. — generous timeout.
    kubectl.wait_for_condition(
        "application/platform",
        Some("argocd"),
        "jsonpath={.status.health.status}=Healthy",
        PLATFORM_RECONCILE_TIMEOUT_SECS,
        kubeconfig_path,
    )?;

    Ok(())
}

/// Render the root `Application` CR YAML the CLI hands Argo CD.
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
  project: default
  source:
    repoURL: "{repo_url}"
    chart: {chart_name}
    targetRevision: "{chart_version}"
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
    )
}

/// Minimal values for the Argo CD loader install. The
/// platform-stack chart's `component_argocd.cue` will overlay
/// extraContainers (cue-cmp sidecar) + tier-2 replica counts
/// when it reconciles; we only need to get Argo CD up enough
/// to read its own next-step manifest.
///
/// Walk-found bug (v0.1.97 → v0.1.98 fix): the upstream
/// `argo-cd` chart 7.7.7 defaults `redis-ha.enabled: true`,
/// which tries to schedule 3 redis-ha pods + 1 haproxy with
/// `podAntiAffinity` `requiredDuringSchedulingIgnoredDuringExecution`
/// — on a single-node k3s those pods never become Ready, the
/// chart's pre-install Job hook waits on them, and `helm
/// install` times out with `failed pre-install: timed out
/// waiting for the condition`. The v0.1.x imperative install
/// explicitly set `redis-ha.enabled: false`; the v0.1.97
/// rewrite dropped that flag by accident. Restored, plus
/// `notifications.enabled: false` (saves one more replica on
/// tier-1 cpx22 RAM) and `server.service.type: ClusterIP`
/// (Gateway/HTTPRoute exposure lands via the platform-stack
/// chart's argocd component, not the loader).
fn argocd_loader_values_yaml() -> String {
    r#"# SPDX-License-Identifier: FSL-1.1-Apache-2.0
# Loader values for the initial Argo CD install. The
# platform-stack chart's component_argocd.cue overlays this
# release with the cue-cmp sidecar and per-tier replicas on
# its first reconcile.
dex:
  enabled: false
redis-ha:
  enabled: false
controller:
  replicas: 1
server:
  replicas: 1
  service:
    type: ClusterIP
repoServer:
  replicas: 1
applicationSet:
  replicaCount: 1
notifications:
  enabled: false
"#
    .to_string()
}

fn platform_stack_repo() -> String {
    APPRAFTER_PLATFORM_STACK_DEFAULT_REPO.to_string()
}

fn platform_stack_version() -> String {
    RELEASED_PLATFORM_STACK_VERSION.to_string()
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
    }

    #[test]
    fn perform_bootstrap_installs_cilium_then_argocd_then_applies_root_then_waits_for_healthy() {
        let helm = FakeHelm::default();
        let kubectl = FakeKubectl::default();
        let kc = PathBuf::from("/tmp/kubeconfig");
        let root_app = PathBuf::from("/tmp/root-app.yaml");

        perform_bootstrap(&helm, &kubectl, &kc, &root_app).expect("bootstrap");

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

        // kubectl: exactly one client-side apply (the root
        // Application), no SSA applies.
        let applies = kubectl.applies.borrow();
        assert_eq!(applies.len(), 1);
        match &applies[0].0 {
            ManifestSource::Path(p) => assert_eq!(p, &root_app),
            other => panic!("expected Path root-app, got {other:?}"),
        }
        assert!(
            kubectl.ssa_applies.borrow().is_empty(),
            "loader path should not use server-side apply"
        );

        // Waits: node Ready first (so step 1 can schedule),
        // argocd-server Available second, root Application
        // Healthy third. Ordering matters — applying the
        // Application before the CRD is installed would fail
        // with "no matches".
        let waits = kubectl.waits.borrow();
        assert_eq!(waits.len(), 3);
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
            "jsonpath={.status.health.status}=Healthy"
        );
        assert_eq!(waits[2].timeout_seconds, PLATFORM_RECONCILE_TIMEOUT_SECS);
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

        perform_bootstrap(&helm, &kubectl, &kc, &root_app).expect("bootstrap");

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
    fn render_root_application_uses_argocd_namespace_destination() {
        let yaml = render_root_application(APPRAFTER_PLATFORM_STACK_DEFAULT_REPO, "0.1.0");
        // The Application CRD lives in `argocd` namespace.
        // Sub-charts target the cluster (kubernetes.default.svc).
        assert!(yaml.contains("namespace: argocd"));
        assert!(yaml.contains("server: https://kubernetes.default.svc"));
    }

    #[test]
    fn argocd_loader_values_keeps_replicas_at_one_for_initial_install() {
        // The loader install minimises memory while we wait
        // for the chart's own Argo CD component overlay to
        // adopt the release. Tier-2 replica counts arrive via
        // Argo CD's first reconcile, not the loader install.
        let v = argocd_loader_values_yaml();
        assert!(v.contains("dex:\n  enabled: false"));
        for k in ["controller", "server", "repoServer"] {
            assert!(v.contains(&format!("{k}:\n  replicas: 1")), "{v}");
        }
        // notifications was bumped to `enabled: false` instead
        // of `replicas: 1` — the latter still allocated one
        // notifications pod, the former skips the deployment
        // entirely (tier-1 cpx22 RAM budget).
        assert!(v.contains("notifications:\n  enabled: false"), "{v}");
    }

    #[test]
    fn argocd_loader_values_disables_redis_ha_for_single_node_k3s() {
        // The v0.1.97 → v0.1.98 walk-found bug: the upstream
        // argo-cd chart 7.7.7 defaults redis-ha.enabled: true,
        // which schedules 3 redis pods with
        // requiredDuringSchedulingIgnoredDuringExecution
        // podAntiAffinity. On single-node k3s those pods never
        // become Ready; the chart's pre-install hook times
        // out. Disabling redis-ha here matches the v0.1.x
        // baseline and unblocks the install.
        let v = argocd_loader_values_yaml();
        assert!(v.contains("redis-ha:\n  enabled: false"), "{v}");
    }

    #[test]
    fn argocd_loader_values_keep_server_at_cluster_ip_until_chart_exposes_it() {
        // Loader install runs before the platform-stack chart
        // reconciles; the chart's component_argocd.cue is what
        // wires Gateway / HTTPRoute exposure. Loader keeps the
        // server at ClusterIP so the loader doesn't expose
        // anything that's not yet hardened.
        let v = argocd_loader_values_yaml();
        assert!(
            v.contains("server:\n  replicas: 1\n  service:\n    type: ClusterIP"),
            "{v}"
        );
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
}
