// SPDX-License-Identifier: FSL-1.1-MIT
//! Install Cilium + Gateway API CRDs into the cluster pointed to
//! by the cached kubeconfig. See plan.md phase 1.4 (v0.1.11).

use std::io::Write;
use std::path::Path;

use cli_core::secrets::{decrypt_with_identity, default_age_key_path, load_or_create_identity};
use cli_core::{CliError, Result};
use cli_providers::k8s::{
    argocd_values_yaml, cert_manager_values_yaml, cilium_values_yaml,
    default_deny_network_policy_yaml, gateway_api_crds_url, selfsigned_cluster_issuer_yaml,
    HelmCli, HelmRunner, HelmUpgradeArgs, KubectlCli, KubectlRunner, ManifestSource,
    ARGOCD_CHART_VERSION, CERT_MANAGER_CHART_VERSION, CILIUM_CHART_VERSION,
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

    let plaintext = decrypt_cached_kubeconfig(&hetzner)?;
    let kubeconfig_file = write_tempfile_with("apprafter-kubeconfig-", &plaintext)?;
    let values_file = write_tempfile_with("apprafter-cilium-values-", &cilium_values_yaml())?;
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

    perform_bootstrap(
        &HelmCli,
        &KubectlCli,
        kubeconfig_file.path(),
        values_file.path(),
        np_file.path(),
        argocd_values_file.path(),
        cert_manager_values_file.path(),
        selfsigned_issuer_file.path(),
    )?;

    println!(
        "cluster-bootstrap complete: cilium {CILIUM_CHART_VERSION} + Gateway API CRDs + default-deny NetworkPolicy + argocd {ARGOCD_CHART_VERSION} + cert-manager {CERT_MANAGER_CHART_VERSION} + self-signed ClusterIssuer applied"
    );
    Ok(())
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

/// Pure orchestration — installs Cilium + Gateway API CRDs +
/// default-deny NetworkPolicy + Argo CD + cert-manager + the
/// self-signed ClusterIssuer in that order. Easily driven with
/// fake runners in tests.
#[allow(clippy::too_many_arguments)]
pub(crate) fn perform_bootstrap<H: HelmRunner, K: KubectlRunner>(
    helm: &H,
    kubectl: &K,
    kubeconfig_path: &Path,
    cilium_values_path: &Path,
    default_deny_path: &Path,
    argocd_values_path: &Path,
    cert_manager_values_path: &Path,
    selfsigned_issuer_path: &Path,
) -> Result<()> {
    helm.repo_add("cilium", "https://helm.cilium.io/")?;
    helm.upgrade_install(&HelmUpgradeArgs {
        release: "cilium".into(),
        chart: "cilium/cilium".into(),
        version: CILIUM_CHART_VERSION.into(),
        namespace: "kube-system".into(),
        values_path: cilium_values_path.to_path_buf(),
        kubeconfig_path: kubeconfig_path.to_path_buf(),
    })?;
    kubectl.apply_manifest(
        &ManifestSource::Url(gateway_api_crds_url()),
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
        version: ARGOCD_CHART_VERSION.into(),
        namespace: "argocd".into(),
        values_path: argocd_values_path.to_path_buf(),
        kubeconfig_path: kubeconfig_path.to_path_buf(),
    })?;

    helm.repo_add("jetstack", "https://charts.jetstack.io")?;
    helm.upgrade_install(&HelmUpgradeArgs {
        release: "cert-manager".into(),
        chart: "jetstack/cert-manager".into(),
        version: CERT_MANAGER_CHART_VERSION.into(),
        namespace: "cert-manager".into(),
        values_path: cert_manager_values_path.to_path_buf(),
        kubeconfig_path: kubeconfig_path.to_path_buf(),
    })?;
    kubectl.apply_manifest(
        &ManifestSource::Path(selfsigned_issuer_path.to_path_buf()),
        kubeconfig_path,
    )?;

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
        let np = PathBuf::from("/tmp/default-deny.yaml");
        let argocd_values = PathBuf::from("/tmp/argocd-values.yaml");
        let cm_values = PathBuf::from("/tmp/cert-manager-values.yaml");
        let issuer = PathBuf::from("/tmp/selfsigned-issuer.yaml");

        perform_bootstrap(
            &helm,
            &kubectl,
            &kc,
            &cilium_values,
            &np,
            &argocd_values,
            &cm_values,
            &issuer,
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
        assert_eq!(installs[0].version, CILIUM_CHART_VERSION);

        assert_eq!(installs[1].release, "argocd");
        assert_eq!(installs[1].version, ARGOCD_CHART_VERSION);

        assert_eq!(installs[2].release, "cert-manager");
        assert_eq!(installs[2].chart, "jetstack/cert-manager");
        assert_eq!(installs[2].version, CERT_MANAGER_CHART_VERSION);
        assert_eq!(installs[2].namespace, "cert-manager");
        assert_eq!(installs[2].values_path, cm_values);

        let applies = kubectl.applies.borrow();
        assert_eq!(
            applies.len(),
            3,
            "expected Gateway CRDs + NetworkPolicy + ClusterIssuer"
        );

        match &applies[0].0 {
            ManifestSource::Url(u) => {
                assert!(u.contains("standard-install.yaml"), "{u}");
                assert!(u.contains("gateway-api"), "{u}");
            }
            other => panic!("first apply must be a URL, got {other:?}"),
        }
        match &applies[1].0 {
            ManifestSource::Path(p) => assert_eq!(p, &np),
            other => panic!("second apply must be the default-deny Path, got {other:?}"),
        }
        match &applies[2].0 {
            ManifestSource::Path(p) => assert_eq!(p, &issuer),
            other => panic!("third apply must be the ClusterIssuer Path, got {other:?}"),
        }
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
