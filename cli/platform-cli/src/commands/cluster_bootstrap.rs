// SPDX-License-Identifier: FSL-1.1-MIT
//! Install Cilium + Gateway API CRDs into the cluster pointed to
//! by the cached kubeconfig. See plan.md phase 1.4 (v0.1.11).

use std::io::Write;
use std::path::Path;

use cli_core::secrets::{decrypt_with_identity, default_age_key_path, load_or_create_identity};
use cli_core::{CliError, Result};
use cli_providers::k8s::{
    cilium_values_yaml, gateway_api_crds_url, HelmCli, HelmRunner, HelmUpgradeArgs, KubectlCli,
    KubectlRunner, ManifestSource, CILIUM_CHART_VERSION,
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

    perform_bootstrap(
        &HelmCli,
        &KubectlCli,
        kubeconfig_file.path(),
        values_file.path(),
    )?;

    println!(
        "cluster-bootstrap complete: cilium {CILIUM_CHART_VERSION} + Gateway API CRDs applied"
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

/// Pure orchestration — adds the Cilium repo, installs the chart,
/// applies the upstream Gateway API standard-install CRDs. Easily
/// driven with fake runners in tests.
pub(crate) fn perform_bootstrap<H: HelmRunner, K: KubectlRunner>(
    helm: &H,
    kubectl: &K,
    kubeconfig_path: &Path,
    cilium_values_path: &Path,
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
    }

    #[test]
    fn perform_bootstrap_runs_helm_repo_then_install_then_kubectl_apply() {
        let helm = FakeHelm::default();
        let kubectl = FakeKubectl::default();
        let kc = PathBuf::from("/tmp/kubeconfig");
        let values = PathBuf::from("/tmp/cilium-values.yaml");

        perform_bootstrap(&helm, &kubectl, &kc, &values).expect("bootstrap");

        let repos = helm.repos.borrow();
        assert_eq!(
            repos.as_slice(),
            &[("cilium".to_string(), "https://helm.cilium.io/".to_string())]
        );

        let installs = helm.installs.borrow();
        assert_eq!(installs.len(), 1);
        assert_eq!(installs[0].release, "cilium");
        assert_eq!(installs[0].chart, "cilium/cilium");
        assert_eq!(installs[0].version, CILIUM_CHART_VERSION);
        assert_eq!(installs[0].namespace, "kube-system");
        assert_eq!(installs[0].values_path, values);
        assert_eq!(installs[0].kubeconfig_path, kc);

        let applies = kubectl.applies.borrow();
        assert_eq!(applies.len(), 1);
        match &applies[0].0 {
            ManifestSource::Url(u) => {
                assert!(u.contains("standard-install.yaml"), "{u}");
                assert!(u.contains("gateway-api"), "{u}");
            }
            other => panic!("expected Url, got {other:?}"),
        }
        assert_eq!(applies[0].1, kc);
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
        };
        let err = decrypt_cached_kubeconfig(&hetzner).unwrap_err();
        let msg = format!("{err:?}");
        assert!(msg.contains("kubeconfig"), "{msg}");
    }
}
