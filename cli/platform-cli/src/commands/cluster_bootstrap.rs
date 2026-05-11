// SPDX-License-Identifier: FSL-1.1-MIT
//! Install Cilium + Gateway API CRDs into the cluster pointed to
//! by the cached kubeconfig. See plan.md phase 1.4 (v0.1.11).

use std::io::Write;
use std::path::Path;

use cli_core::manifest::{self, InfrastructureManifest};
use cli_core::secrets::{decrypt_with_identity, default_age_key_path, load_or_create_identity};
use cli_core::{CliError, Result};
use cli_providers::k8s::{
    admission_webhook_yaml, application_crd_yaml, argocd_gateway_yaml, argocd_values_yaml,
    backstage_manifests_yaml, bootstrap_application_yaml, cert_manager_values_yaml,
    cilium_values_yaml, default_deny_network_policy_yaml, gateway_api_crds_url,
    selfsigned_cluster_issuer_yaml, HelmCli, HelmRunner, HelmUpgradeArgs, KubectlCli,
    KubectlRunner, ManifestSource, APPRAFTER_OPERATOR_RELEASE_NAME, APPRAFTER_SYSTEM_NAMESPACE,
    ARGOCD_CHART_VERSION, BACKSTAGE_DEFAULT_IMAGE, BOOTSTRAP_APP_DEFAULT_PATH,
    CERT_MANAGER_CHART_VERSION, CILIUM_CHART_VERSION,
};
use cli_state::{State, StatePaths};
use tempfile::NamedTempFile;
use tracing::info;

pub fn run() -> Result<()> {
    info!("cluster-bootstrap invoked");
    let cwd = std::env::current_dir()?;
    let paths = StatePaths::for_root(&cwd);
    let state = State::load_or_default(&paths)?;
    let hetzner = state.hetzner_cloud.clone().ok_or_else(|| {
        CliError::Other(
            "state has no hetzner_cloud section; run `platform-cli apply` first".to_string(),
        )
    })?;

    let cluster = read_cluster_settings_from_manifest(&cwd)?;

    let plaintext = decrypt_cached_kubeconfig(&hetzner)?;
    let kubeconfig_file = write_tempfile_with("apprafter-kubeconfig-", &plaintext)?;
    let values_file = write_tempfile_with("apprafter-cilium-values-", &cilium_values_yaml())?;
    let application_crd_file =
        write_tempfile_with("apprafter-application-crd-", &application_crd_yaml())?;
    let np_file = write_tempfile_with(
        "apprafter-default-deny-",
        &default_deny_network_policy_yaml("default"),
    )?;
    let argocd_values_file =
        write_tempfile_with("apprafter-argocd-values-", &argocd_values_yaml())?;
    let cert_manager_values_file = write_tempfile_with(
        "apprafter-cert-manager-values-",
        &cert_manager_values_yaml(),
    )?;
    let selfsigned_issuer_file = write_tempfile_with(
        "apprafter-selfsigned-issuer-",
        &selfsigned_cluster_issuer_yaml(),
    )?;

    let argocd_gateway_file = match &cluster.argocd_domain {
        Some(domain) => Some(write_tempfile_with(
            "apprafter-argocd-gateway-",
            &argocd_gateway_yaml(domain),
        )?),
        None => None,
    };

    let bootstrap_app_file = match &cluster.bootstrap_repo {
        Some(repo) => {
            let path = cluster
                .bootstrap_path
                .as_deref()
                .unwrap_or(BOOTSTRAP_APP_DEFAULT_PATH);
            Some(write_tempfile_with(
                "apprafter-bootstrap-app-",
                &bootstrap_application_yaml(repo, path),
            )?)
        }
        None => None,
    };

    let backstage_file = match &cluster.backstage_domain {
        Some(domain) => {
            let image = cluster
                .backstage_image
                .as_deref()
                .unwrap_or(BACKSTAGE_DEFAULT_IMAGE);
            Some(write_tempfile_with(
                "apprafter-backstage-",
                &backstage_manifests_yaml(domain, image),
            )?)
        }
        None => None,
    };

    let admission_webhook_file = match &cluster.admission_webhook_image {
        Some(image) => Some(write_tempfile_with(
            "apprafter-admission-webhook-",
            &admission_webhook_yaml(image),
        )?),
        None => None,
    };

    perform_bootstrap(
        &HelmCli,
        &KubectlCli,
        kubeconfig_file.path(),
        values_file.path(),
        application_crd_file.path(),
        np_file.path(),
        argocd_values_file.path(),
        cert_manager_values_file.path(),
        selfsigned_issuer_file.path(),
        None, // operator_chart_path — wired in Task 8
        None, // operator_values_path — wired in Task 8
        admission_webhook_file.as_ref().map(|f| f.path()),
        argocd_gateway_file.as_ref().map(|f| f.path()),
        bootstrap_app_file.as_ref().map(|f| f.path()),
        backstage_file.as_ref().map(|f| f.path()),
    )?;

    let mut suffix = String::new();
    if let Some(d) = &cluster.argocd_domain {
        suffix.push_str(&format!(" + Argo CD Gateway/HTTPRoute on {d}"));
    }
    if let Some(repo) = &cluster.bootstrap_repo {
        suffix.push_str(&format!(" + bootstrap Application from {repo}"));
    }
    if let Some(d) = &cluster.backstage_domain {
        suffix.push_str(&format!(" + Backstage on {d}"));
    }
    if cluster.admission_webhook_image.is_some() {
        suffix.push_str(" + admission-webhook in apprafter-system");
    }
    println!(
        "cluster-bootstrap complete: cilium {CILIUM_CHART_VERSION} + Gateway API CRDs + Application CRD + default-deny NetworkPolicy + argocd {ARGOCD_CHART_VERSION} + cert-manager {CERT_MANAGER_CHART_VERSION} + self-signed ClusterIssuer{suffix} applied"
    );
    Ok(())
}

#[derive(Debug, Default, Clone)]
struct ClusterSettings {
    argocd_domain: Option<String>,
    bootstrap_repo: Option<String>,
    bootstrap_path: Option<String>,
    backstage_domain: Option<String>,
    backstage_image: Option<String>,
    admission_webhook_image: Option<String>,
}

fn read_cluster_settings_from_manifest(cwd: &Path) -> Result<ClusterSettings> {
    let path = match std::env::var("APPRAFTER_MANIFEST") {
        Ok(p) => p,
        Err(_) => return Ok(ClusterSettings::default()),
    };
    info!(path = %path, "reading Infrastructure manifest for cluster settings");
    let parsed: InfrastructureManifest = manifest::parse_infrastructure(cwd, Path::new(&path))?;
    let argocd = parsed.spec.argocd.unwrap_or_default();
    let backstage = parsed.spec.backstage.unwrap_or_default();
    let admission_webhook = parsed.spec.admission_webhook.unwrap_or_default();
    Ok(ClusterSettings {
        argocd_domain: argocd.domain.filter(|d| !d.is_empty()),
        bootstrap_repo: argocd.bootstrap_repo.filter(|d| !d.is_empty()),
        bootstrap_path: argocd.bootstrap_path.filter(|d| !d.is_empty()),
        backstage_domain: backstage.domain.filter(|d| !d.is_empty()),
        backstage_image: backstage.image.filter(|d| !d.is_empty()),
        admission_webhook_image: admission_webhook.image.filter(|d| !d.is_empty()),
    })
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
        "no cached kubeconfig in state; run `platform-cli kubeconfig` first".to_string(),
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

/// Pure orchestration — installs Cilium + Gateway API CRDs + the
/// AppRafter Application CRD + default-deny NetworkPolicy + Argo
/// CD + cert-manager + the self-signed ClusterIssuer, optionally
/// followed by the AppRafter operator helm release (default on,
/// suppressed by `operator_chart_path: None`), the admission-webhook
/// manifest (default on, suppressed by `admission_webhook_path: None`),
/// the Argo CD Gateway / HTTPRoute / Certificate manifest, the
/// bootstrap `Application` resource, and the tier-1 Backstage
/// manifest set. Easily driven with fake runners in tests.
#[allow(clippy::too_many_arguments)]
pub(crate) fn perform_bootstrap<H: HelmRunner, K: KubectlRunner>(
    helm: &H,
    kubectl: &K,
    kubeconfig_path: &Path,
    cilium_values_path: &Path,
    application_crd_path: &Path,
    default_deny_path: &Path,
    argocd_values_path: &Path,
    cert_manager_values_path: &Path,
    selfsigned_issuer_path: &Path,
    operator_chart_path: Option<&Path>,
    operator_values_path: Option<&Path>,
    admission_webhook_path: Option<&Path>,
    argocd_gateway_path: Option<&Path>,
    bootstrap_app_path: Option<&Path>,
    backstage_manifests_path: Option<&Path>,
) -> Result<()> {
    helm.repo_add("cilium", "https://helm.cilium.io/")?;
    helm.upgrade_install(&HelmUpgradeArgs {
        release: "cilium".into(),
        chart: "cilium/cilium".into(),
        version: Some(CILIUM_CHART_VERSION.into()),
        namespace: "kube-system".into(),
        values_path: cilium_values_path.to_path_buf(),
        kubeconfig_path: kubeconfig_path.to_path_buf(),
    })?;
    kubectl.apply_manifest(
        &ManifestSource::Url(gateway_api_crds_url()),
        kubeconfig_path,
    )?;
    kubectl.apply_manifest(
        &ManifestSource::Path(application_crd_path.to_path_buf()),
        kubeconfig_path,
    )?;
    kubectl.apply_manifest(
        &ManifestSource::Path(default_deny_path.to_path_buf()),
        kubeconfig_path,
    )?;

    helm.repo_add("argo", "https://argoproj.github.io/argo-helm")?;
    helm.upgrade_install(&HelmUpgradeArgs {
        release: "argocd".into(),
        chart: "argo/argo-cd".into(),
        version: Some(ARGOCD_CHART_VERSION.into()),
        namespace: "argocd".into(),
        values_path: argocd_values_path.to_path_buf(),
        kubeconfig_path: kubeconfig_path.to_path_buf(),
    })?;

    helm.repo_add("jetstack", "https://charts.jetstack.io")?;
    helm.upgrade_install(&HelmUpgradeArgs {
        release: "cert-manager".into(),
        chart: "jetstack/cert-manager".into(),
        version: Some(CERT_MANAGER_CHART_VERSION.into()),
        namespace: "cert-manager".into(),
        values_path: cert_manager_values_path.to_path_buf(),
        kubeconfig_path: kubeconfig_path.to_path_buf(),
    })?;
    kubectl.apply_manifest(
        &ManifestSource::Path(selfsigned_issuer_path.to_path_buf()),
        kubeconfig_path,
    )?;

    // Step 8 — AppRafter operator helm release (apprafter-system).
    if let (Some(chart), Some(values)) = (operator_chart_path, operator_values_path) {
        helm.upgrade_install(&HelmUpgradeArgs {
            release: APPRAFTER_OPERATOR_RELEASE_NAME.into(),
            chart: chart.to_string_lossy().into_owned(),
            version: None,
            namespace: APPRAFTER_SYSTEM_NAMESPACE.into(),
            values_path: values.to_path_buf(),
            kubeconfig_path: kubeconfig_path.to_path_buf(),
        })?;
    }

    // Step 9 — Admission-webhook inline manifest (apprafter-system).
    if let Some(aw_path) = admission_webhook_path {
        kubectl.apply_manifest(
            &ManifestSource::Path(aw_path.to_path_buf()),
            kubeconfig_path,
        )?;
    }

    if let Some(gw_path) = argocd_gateway_path {
        kubectl.apply_manifest(
            &ManifestSource::Path(gw_path.to_path_buf()),
            kubeconfig_path,
        )?;
    }

    if let Some(bootstrap_path) = bootstrap_app_path {
        kubectl.apply_manifest(
            &ManifestSource::Path(bootstrap_path.to_path_buf()),
            kubeconfig_path,
        )?;
    }

    if let Some(bs_path) = backstage_manifests_path {
        kubectl.apply_manifest(
            &ManifestSource::Path(bs_path.to_path_buf()),
            kubeconfig_path,
        )?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::path::PathBuf;

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
    }

    impl KubectlRunner for FakeKubectl {
        fn apply_manifest(&self, source: &ManifestSource, kubeconfig_path: &Path) -> Result<()> {
            self.applies
                .borrow_mut()
                .push((source.clone(), kubeconfig_path.to_path_buf()));
            Ok(())
        }
        fn get_secret_value(
            &self,
            _secret_name: &str,
            _namespace: &str,
            _key: &str,
            _kubeconfig_path: &Path,
        ) -> Result<String> {
            unreachable!("cluster-bootstrap never reads secrets")
        }
    }

    #[test]
    fn perform_bootstrap_installs_full_stack_in_order() {
        let helm = FakeHelm::default();
        let kubectl = FakeKubectl::default();
        let kc = PathBuf::from("/tmp/kubeconfig");
        let cilium_values = PathBuf::from("/tmp/cilium-values.yaml");
        let app_crd = PathBuf::from("/tmp/application-crd.yaml");
        let np = PathBuf::from("/tmp/default-deny.yaml");
        let argocd_values = PathBuf::from("/tmp/argocd-values.yaml");
        let cm_values = PathBuf::from("/tmp/cert-manager-values.yaml");
        let issuer = PathBuf::from("/tmp/selfsigned-issuer.yaml");

        perform_bootstrap(
            &helm,
            &kubectl,
            &kc,
            &cilium_values,
            &app_crd,
            &np,
            &argocd_values,
            &cm_values,
            &issuer,
            None, // operator chart
            None, // operator values
            None, // admission webhook
            None, // argocd gateway
            None, // bootstrap app
            None, // backstage
        )
        .expect("bootstrap");

        let repos = helm.repos.borrow();
        assert_eq!(
            repos.as_slice(),
            &[
                ("cilium".to_string(), "https://helm.cilium.io/".to_string()),
                (
                    "argo".to_string(),
                    "https://argoproj.github.io/argo-helm".to_string()
                ),
                (
                    "jetstack".to_string(),
                    "https://charts.jetstack.io".to_string()
                ),
            ]
        );

        let installs = helm.installs.borrow();
        assert_eq!(installs.len(), 3);
        assert_eq!(installs[0].release, "cilium");
        assert_eq!(installs[0].version.as_deref(), Some(CILIUM_CHART_VERSION));

        assert_eq!(installs[1].release, "argocd");
        assert_eq!(installs[1].version.as_deref(), Some(ARGOCD_CHART_VERSION));

        assert_eq!(installs[2].release, "cert-manager");
        assert_eq!(installs[2].chart, "jetstack/cert-manager");
        assert_eq!(installs[2].version.as_deref(), Some(CERT_MANAGER_CHART_VERSION));
        assert_eq!(installs[2].namespace, "cert-manager");
        assert_eq!(installs[2].values_path, cm_values);

        let applies = kubectl.applies.borrow();
        assert_eq!(
            applies.len(),
            4,
            "expected Gateway CRDs + Application CRD + NetworkPolicy + ClusterIssuer"
        );

        match &applies[0].0 {
            ManifestSource::Url(u) => {
                assert!(u.contains("standard-install.yaml"), "{u}");
                assert!(u.contains("gateway-api"), "{u}");
            }
            other => panic!("first apply must be a URL, got {other:?}"),
        }
        match &applies[1].0 {
            ManifestSource::Path(p) => assert_eq!(p, &app_crd),
            other => panic!("second apply must be the Application CRD Path, got {other:?}"),
        }
        match &applies[2].0 {
            ManifestSource::Path(p) => assert_eq!(p, &np),
            other => panic!("third apply must be the default-deny Path, got {other:?}"),
        }
        match &applies[3].0 {
            ManifestSource::Path(p) => assert_eq!(p, &issuer),
            other => panic!("fourth apply must be the ClusterIssuer Path, got {other:?}"),
        }
    }

    #[test]
    fn perform_bootstrap_applies_argocd_gateway_when_path_provided() {
        let helm = FakeHelm::default();
        let kubectl = FakeKubectl::default();
        let kc = PathBuf::from("/tmp/kubeconfig");
        let cilium_values = PathBuf::from("/tmp/cilium-values.yaml");
        let app_crd = PathBuf::from("/tmp/application-crd.yaml");
        let np = PathBuf::from("/tmp/default-deny.yaml");
        let argocd_values = PathBuf::from("/tmp/argocd-values.yaml");
        let cm_values = PathBuf::from("/tmp/cert-manager-values.yaml");
        let issuer = PathBuf::from("/tmp/selfsigned-issuer.yaml");
        let gateway = PathBuf::from("/tmp/argocd-gateway.yaml");

        perform_bootstrap(
            &helm,
            &kubectl,
            &kc,
            &cilium_values,
            &app_crd,
            &np,
            &argocd_values,
            &cm_values,
            &issuer,
            None,           // operator chart
            None,           // operator values
            None,           // admission webhook
            Some(&gateway), // argocd gateway
            None,           // bootstrap
            None,           // backstage
        )
        .expect("bootstrap");

        let applies = kubectl.applies.borrow();
        // 5 applies: Gateway CRDs URL, Application CRD Path,
        // default-deny Path, ClusterIssuer Path, Argo CD Gateway Path.
        assert_eq!(applies.len(), 5);
        match &applies[4].0 {
            ManifestSource::Path(p) => assert_eq!(p, &gateway),
            other => panic!("fifth apply must be the Argo CD Gateway Path, got {other:?}"),
        }
        assert_eq!(applies[4].1, kc);
    }

    #[test]
    fn perform_bootstrap_applies_bootstrap_application_when_path_provided() {
        let helm = FakeHelm::default();
        let kubectl = FakeKubectl::default();
        let kc = PathBuf::from("/tmp/kubeconfig");
        let cilium_values = PathBuf::from("/tmp/cilium-values.yaml");
        let app_crd = PathBuf::from("/tmp/application-crd.yaml");
        let np = PathBuf::from("/tmp/default-deny.yaml");
        let argocd_values = PathBuf::from("/tmp/argocd-values.yaml");
        let cm_values = PathBuf::from("/tmp/cert-manager-values.yaml");
        let issuer = PathBuf::from("/tmp/selfsigned-issuer.yaml");
        let gateway = PathBuf::from("/tmp/argocd-gateway.yaml");
        let bootstrap = PathBuf::from("/tmp/bootstrap-app.yaml");

        perform_bootstrap(
            &helm,
            &kubectl,
            &kc,
            &cilium_values,
            &app_crd,
            &np,
            &argocd_values,
            &cm_values,
            &issuer,
            None,             // operator chart
            None,             // operator values
            None,             // admission webhook
            Some(&gateway),   // argocd gateway
            Some(&bootstrap), // bootstrap
            None,             // backstage
        )
        .expect("bootstrap");

        let applies = kubectl.applies.borrow();
        // 6 applies: Gateway CRDs URL, Application CRD Path,
        // default-deny Path, ClusterIssuer Path, Argo CD Gateway Path,
        // bootstrap Application Path.
        assert_eq!(applies.len(), 6);
        match &applies[5].0 {
            ManifestSource::Path(p) => assert_eq!(p, &bootstrap),
            other => panic!("sixth apply must be the bootstrap Application Path, got {other:?}"),
        }
        assert_eq!(applies[5].1, kc);
    }

    #[test]
    fn perform_bootstrap_applies_backstage_when_path_provided() {
        let helm = FakeHelm::default();
        let kubectl = FakeKubectl::default();
        let kc = PathBuf::from("/tmp/kubeconfig");
        let cilium_values = PathBuf::from("/tmp/cilium-values.yaml");
        let app_crd = PathBuf::from("/tmp/application-crd.yaml");
        let np = PathBuf::from("/tmp/default-deny.yaml");
        let argocd_values = PathBuf::from("/tmp/argocd-values.yaml");
        let cm_values = PathBuf::from("/tmp/cert-manager-values.yaml");
        let issuer = PathBuf::from("/tmp/selfsigned-issuer.yaml");
        let gateway = PathBuf::from("/tmp/argocd-gateway.yaml");
        let bootstrap = PathBuf::from("/tmp/bootstrap-app.yaml");
        let backstage = PathBuf::from("/tmp/backstage.yaml");

        perform_bootstrap(
            &helm,
            &kubectl,
            &kc,
            &cilium_values,
            &app_crd,
            &np,
            &argocd_values,
            &cm_values,
            &issuer,
            None,             // operator chart
            None,             // operator values
            None,             // admission webhook
            Some(&gateway),   // argocd gateway
            Some(&bootstrap), // bootstrap
            Some(&backstage), // backstage
        )
        .expect("bootstrap");

        let applies = kubectl.applies.borrow();
        // 7 applies: Gateway CRDs URL, Application CRD Path,
        // default-deny Path, ClusterIssuer Path, Argo CD Gateway
        // Path, bootstrap App Path, Backstage Path.
        assert_eq!(applies.len(), 7);
        match &applies[6].0 {
            ManifestSource::Path(p) => assert_eq!(p, &backstage),
            other => panic!("seventh apply must be the Backstage Path, got {other:?}"),
        }
        assert_eq!(applies[6].1, kc);
    }

    #[test]
    fn perform_bootstrap_applies_admission_webhook_when_path_provided() {
        let helm = FakeHelm::default();
        let kubectl = FakeKubectl::default();
        let kc = PathBuf::from("/tmp/kubeconfig");
        let cilium_values = PathBuf::from("/tmp/cilium-values.yaml");
        let app_crd = PathBuf::from("/tmp/application-crd.yaml");
        let np = PathBuf::from("/tmp/default-deny.yaml");
        let argocd_values = PathBuf::from("/tmp/argocd-values.yaml");
        let cm_values = PathBuf::from("/tmp/cert-manager-values.yaml");
        let issuer = PathBuf::from("/tmp/selfsigned-issuer.yaml");
        let gateway = PathBuf::from("/tmp/argocd-gateway.yaml");
        let bootstrap = PathBuf::from("/tmp/bootstrap-app.yaml");
        let backstage = PathBuf::from("/tmp/backstage.yaml");
        let admission_webhook = PathBuf::from("/tmp/admission-webhook.yaml");

        perform_bootstrap(
            &helm,
            &kubectl,
            &kc,
            &cilium_values,
            &app_crd,
            &np,
            &argocd_values,
            &cm_values,
            &issuer,
            None,                     // operator chart
            None,                     // operator values
            Some(&admission_webhook),
            Some(&gateway),
            Some(&bootstrap),
            Some(&backstage),
        )
        .expect("bootstrap");

        let applies = kubectl.applies.borrow();
        // 8 applies: Gateway CRDs URL, Application CRD Path,
        // default-deny Path, ClusterIssuer Path, admission-webhook
        // Path, Argo CD Gateway Path, bootstrap App Path,
        // Backstage Path.
        assert_eq!(applies.len(), 8);
        match &applies[4].0 {
            ManifestSource::Path(p) => assert_eq!(p, &admission_webhook),
            other => panic!("fifth apply must be admission-webhook Path, got {other:?}"),
        }
        match &applies[5].0 {
            ManifestSource::Path(p) => assert_eq!(p, &gateway),
            other => panic!("sixth apply must be argocd gateway Path, got {other:?}"),
        }
        assert_eq!(applies[4].1, kc);
    }

    #[test]
    fn default_manifest_installs_operator_and_webhook_in_order() {
        let helm = FakeHelm::default();
        let kubectl = FakeKubectl::default();
        let kc = PathBuf::from("/tmp/kubeconfig");
        let cilium_values = PathBuf::from("/tmp/cilium-values.yaml");
        let app_crd = PathBuf::from("/tmp/application-crd.yaml");
        let np = PathBuf::from("/tmp/default-deny.yaml");
        let argocd_values = PathBuf::from("/tmp/argocd-values.yaml");
        let cm_values = PathBuf::from("/tmp/cert-manager-values.yaml");
        let issuer = PathBuf::from("/tmp/selfsigned-issuer.yaml");
        let op_chart = PathBuf::from("/tmp/operator-chart");
        let op_values = PathBuf::from("/tmp/operator-values.yaml");
        let aw_manifest = PathBuf::from("/tmp/admission-webhook.yaml");

        perform_bootstrap(
            &helm,
            &kubectl,
            &kc,
            &cilium_values,
            &app_crd,
            &np,
            &argocd_values,
            &cm_values,
            &issuer,
            Some(&op_chart),
            Some(&op_values),
            Some(&aw_manifest),
            None,
            None,
            None,
        )
        .expect("bootstrap");

        let installs = helm.installs.borrow();
        assert_eq!(installs.len(), 4, "expected cilium, argocd, cert-manager, operator");
        assert_eq!(installs[3].release, "apprafter-operator");
        assert_eq!(installs[3].chart, op_chart.to_string_lossy());
        assert_eq!(installs[3].namespace, "apprafter-system");
        assert_eq!(installs[3].values_path, op_values);
        assert!(installs[3].version.is_none(), "operator chart is local-path");

        let applies = kubectl.applies.borrow();
        // 5 applies: Gateway CRDs URL, Application CRD, default-deny,
        // ClusterIssuer, admission-webhook.
        assert_eq!(applies.len(), 5);
        match &applies[4].0 {
            ManifestSource::Path(p) => assert_eq!(p, &aw_manifest),
            other => panic!("fifth apply must be admission-webhook Path, got {other:?}"),
        }
    }

    #[test]
    fn operator_enabled_false_skips_helm_install() {
        let helm = FakeHelm::default();
        let kubectl = FakeKubectl::default();
        let kc = PathBuf::from("/tmp/kubeconfig");
        let cilium_values = PathBuf::from("/tmp/cilium-values.yaml");
        let app_crd = PathBuf::from("/tmp/application-crd.yaml");
        let np = PathBuf::from("/tmp/default-deny.yaml");
        let argocd_values = PathBuf::from("/tmp/argocd-values.yaml");
        let cm_values = PathBuf::from("/tmp/cert-manager-values.yaml");
        let issuer = PathBuf::from("/tmp/selfsigned-issuer.yaml");
        let aw_manifest = PathBuf::from("/tmp/admission-webhook.yaml");

        perform_bootstrap(
            &helm,
            &kubectl,
            &kc,
            &cilium_values,
            &app_crd,
            &np,
            &argocd_values,
            &cm_values,
            &issuer,
            None, // operator chart — skipped via spec.operator.enabled: false at the caller
            None, // operator values — paired
            Some(&aw_manifest),
            None,
            None,
            None,
        )
        .expect("bootstrap");

        let installs = helm.installs.borrow();
        assert!(
            !installs.iter().any(|i| i.release == "apprafter-operator"),
            "operator should not be installed when chart path is None: {:?}",
            installs
        );
        // Only the baseline 3 helm installs.
        assert_eq!(installs.len(), 3);
    }

    #[test]
    fn admission_webhook_enabled_false_skips_kubectl_apply() {
        let helm = FakeHelm::default();
        let kubectl = FakeKubectl::default();
        let kc = PathBuf::from("/tmp/kubeconfig");
        let cilium_values = PathBuf::from("/tmp/cilium-values.yaml");
        let app_crd = PathBuf::from("/tmp/application-crd.yaml");
        let np = PathBuf::from("/tmp/default-deny.yaml");
        let argocd_values = PathBuf::from("/tmp/argocd-values.yaml");
        let cm_values = PathBuf::from("/tmp/cert-manager-values.yaml");
        let issuer = PathBuf::from("/tmp/selfsigned-issuer.yaml");
        let op_chart = PathBuf::from("/tmp/operator-chart");
        let op_values = PathBuf::from("/tmp/operator-values.yaml");

        perform_bootstrap(
            &helm,
            &kubectl,
            &kc,
            &cilium_values,
            &app_crd,
            &np,
            &argocd_values,
            &cm_values,
            &issuer,
            Some(&op_chart),
            Some(&op_values),
            None, // admission_webhook_path — None means skip
            None,
            None,
            None,
        )
        .expect("bootstrap");

        let applies = kubectl.applies.borrow();
        // Baseline 4 applies: Gateway CRDs URL, Application CRD,
        // default-deny, ClusterIssuer. No admission-webhook path.
        assert_eq!(applies.len(), 4);
        for a in applies.iter() {
            if let ManifestSource::Path(p) = &a.0 {
                assert!(
                    !p.to_string_lossy().contains("admission-webhook"),
                    "should not apply admission-webhook when None: {p:?}",
                );
            }
        }
    }

    // Tests `operator_image_override_with_colon_uses_full_ref_verbatim` and
    // `operator_tag_override_alone_keeps_default_registry` live with Task 8,
    // where the manifest-block → resolved-image helper is introduced.

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
