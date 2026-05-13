// SPDX-License-Identifier: FSL-1.1-MIT
//! Print the Argo CD admin password from the cluster, caching the
//! result age-encrypted in state. See plan.md phase 1.5 (v0.1.14).

use std::io::Write;
use std::path::Path;

use cli_core::secrets::{
    decrypt_with_identity, default_age_key_path, encrypt_for_recipient, load_or_create_identity,
};
use cli_core::{CliError, Result};
use cli_providers::k8s::{KubectlCli, KubectlRunner};
use cli_state::{State, StatePaths};
use tempfile::NamedTempFile;
use tracing::info;

const ARGOCD_NAMESPACE: &str = "argocd";
const ARGOCD_ADMIN_SECRET: &str = "argocd-initial-admin-secret";
const ARGOCD_ADMIN_KEY: &str = "password";

pub fn run(refresh: bool) -> Result<()> {
    info!(refresh, "argocd-password invoked");
    let cwd = std::env::current_dir()?;
    let paths = StatePaths::for_root(&cwd);
    let mut state = State::load_or_default(&paths)?;
    let hetzner = state.hetzner_cloud.clone().ok_or_else(|| {
        CliError::Other(
            "state has no hetzner_cloud section; run `platform-cli apply` first".to_string(),
        )
    })?;

    let identity = load_or_create_identity(&default_age_key_path())?;

    // Cached fast-path.
    if let Some(armored) = &hetzner.argocd_admin_password_age {
        if !refresh {
            let plaintext = decrypt_with_identity(armored, &identity)?;
            print!("{plaintext}");
            return Ok(());
        }
    }

    // Cold path: decrypt kubeconfig, fetch secret via kubectl,
    // encrypt password into state, print plaintext.
    let kubeconfig = decrypt_kubeconfig(&hetzner, &identity)?;
    let kubeconfig_file = write_tempfile_with("apprafter-kubeconfig-", &kubeconfig)?;

    let plaintext = compute_argocd_password(&KubectlCli, kubeconfig_file.path())?;

    let armored = encrypt_for_recipient(&plaintext, &identity.to_public())?;
    let mut updated = hetzner.clone();
    updated.argocd_admin_password_age = Some(armored);
    state.hetzner_cloud = Some(updated);
    state.save(&paths)?;

    print!("{plaintext}");
    Ok(())
}

fn decrypt_kubeconfig(
    hetzner: &cli_state::HetznerCloudState,
    identity: &age::x25519::Identity,
) -> Result<String> {
    if let Some(armored) = &hetzner.kubeconfig_age {
        return decrypt_with_identity(armored, identity);
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

/// Pure orchestration: ask `kubectl` for the admin secret value
/// and return the decoded plaintext. Decoupled from CLI side
/// effects so tests can drive it with a fake kubectl runner.
pub(crate) fn compute_argocd_password<K: KubectlRunner>(
    kubectl: &K,
    kubeconfig_path: &Path,
) -> Result<String> {
    kubectl.get_secret_value(
        ARGOCD_ADMIN_SECRET,
        ARGOCD_NAMESPACE,
        ARGOCD_ADMIN_KEY,
        kubeconfig_path,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use cli_providers::k8s::ManifestSource;
    use std::cell::RefCell;

    #[derive(Default)]
    struct FakeKubectl {
        gets: RefCell<Vec<(String, String, String)>>,
        body: String,
    }

    impl FakeKubectl {
        fn returning(body: &str) -> Self {
            Self {
                gets: RefCell::new(Vec::new()),
                body: body.into(),
            }
        }
    }

    impl KubectlRunner for FakeKubectl {
        fn apply_manifest(&self, _: &ManifestSource, _: &Path) -> Result<()> {
            unreachable!("argocd-password never applies manifests")
        }
        fn apply_manifest_server_side(
            &self,
            _: &ManifestSource,
            _: &Path,
            _: &str,
        ) -> Result<()> {
            unreachable!("argocd-password never applies manifests")
        }
        fn get_secret_value(
            &self,
            secret: &str,
            namespace: &str,
            key: &str,
            _kubeconfig_path: &Path,
        ) -> Result<String> {
            self.gets
                .borrow_mut()
                .push((secret.into(), namespace.into(), key.into()));
            Ok(self.body.clone())
        }
    }

    #[test]
    fn compute_argocd_password_asks_kubectl_for_the_admin_secret() {
        let k = FakeKubectl::returning("hunter2");
        let out = compute_argocd_password(&k, Path::new("/tmp/kc")).unwrap();
        assert_eq!(out, "hunter2");
        let gets = k.gets.borrow();
        assert_eq!(gets.len(), 1);
        assert_eq!(
            gets[0],
            (
                ARGOCD_ADMIN_SECRET.to_string(),
                ARGOCD_NAMESPACE.to_string(),
                ARGOCD_ADMIN_KEY.to_string(),
            )
        );
    }

    #[test]
    fn decrypt_kubeconfig_falls_back_to_plaintext_yaml() {
        let identity = age::x25519::Identity::generate();
        let hetzner = cli_state::HetznerCloudState {
            server_id: 1,
            server_name: "n".into(),
            ssh_key_ids: vec![],
            network_id: None,
            firewall_id: None,
            floating_ip_ids: vec![],
            kubeconfig_yaml: Some("from-plaintext".into()),
            kubeconfig_age: None,
            argocd_admin_password_age: None,
        };
        let out = decrypt_kubeconfig(&hetzner, &identity).unwrap();
        assert_eq!(out, "from-plaintext");
    }

    #[test]
    fn decrypt_kubeconfig_errors_when_neither_field_set() {
        let identity = age::x25519::Identity::generate();
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
        let err = decrypt_kubeconfig(&hetzner, &identity).unwrap_err();
        let msg = format!("{err:?}");
        assert!(msg.contains("kubeconfig"), "{msg}");
    }
}
