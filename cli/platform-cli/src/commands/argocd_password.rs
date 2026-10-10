// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Print the Argo CD admin password from the cluster, caching the
//! result age-encrypted in state. See plan.md phase 1.5 (v0.1.14).

use std::io::Write;
use std::path::Path;

use cli_core::secrets::{
    default_age_key_path, encrypt_for_recipient, load_or_create_identity, CachedSecret,
};
use cli_core::{CliError, Result};
use cli_providers::k8s::{KubectlCli, KubectlRunner};
use cli_state::State;
use tempfile::NamedTempFile;
use tracing::info;

use crate::commands::age_cache;
use crate::commands::state_paths::resolve_state_paths;

const ARGOCD_NAMESPACE: &str = "argocd";
const ARGOCD_ADMIN_SECRET: &str = "argocd-initial-admin-secret";
const ARGOCD_ADMIN_KEY: &str = "password";

pub fn run(refresh: bool, target_override: Option<&str>) -> Result<()> {
    info!(refresh, target_override, "argocd-password invoked");

    // Per-target state (v0.1.154): the active target's, or `--target`'s (WI-457 review #7, so
    // the way back for another target's password never needs a switch of the active one).
    let resolved = resolve_state_paths(target_override)?;
    let target = resolved.target_name.as_str();
    let mut state = State::load_or_default(&resolved.paths)?;
    let hetzner = state.hetzner_cloud.clone().ok_or_else(|| {
        CliError::Other(
            "state has no hetzner_cloud section; run `apprafter apply` first".to_string(),
        )
    })?;
    let key_path = default_age_key_path();

    // Cached fast-path: a read, which never creates the age key (GOTCHA-120).
    if let Some(armored) = &hetzner.argocd_admin_password_age {
        if !refresh {
            let plaintext =
                age_cache::decrypt(armored, CachedSecret::ArgocdPassword, target, &key_path)?;
            print!("{plaintext}");
            return Ok(());
        }
    }

    // Cold path: decrypt kubeconfig, fetch secret via kubectl,
    // encrypt password into state, print plaintext.
    //
    // The key the password is cached under is settled first, before the cluster is read: beside
    // a lost key (a plaintext kubeconfig needs none, another target's cache does) the read
    // would be thrown away (review #1). A lost key is `kubeconfig --refresh`'s to replace,
    // after it asks.
    let key = age_cache::key_for_new_secret(&resolved.store, target, &key_path)?;
    let kubeconfig = age_cache::cached_kubeconfig(&hetzner, target, &key_path)?;
    let kubeconfig_file = write_tempfile_with("apprafter-kubeconfig-", &kubeconfig)?;

    let plaintext = compute_argocd_password(&KubectlCli, kubeconfig_file.path())?;

    // A first use (a plaintext kubeconfig, and nothing encrypted anywhere) creates the key now
    // that there is something to cache.
    let identity = match key {
        Some(identity) => identity,
        None => load_or_create_identity(&key_path)?,
    };
    let armored = encrypt_for_recipient(&plaintext, &identity.to_public())?;
    let mut updated = hetzner.clone();
    updated.argocd_admin_password_age = Some(armored);
    state.hetzner_cloud = Some(updated);
    state.save(&resolved.paths)?;

    print!("{plaintext}");
    Ok(())
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
        fn apply_manifest_server_side(&self, _: &ManifestSource, _: &Path, _: &str) -> Result<()> {
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
        fn wait_for_condition(
            &self,
            _: &str,
            _: Option<&str>,
            _: &str,
            _: u64,
            _: &Path,
        ) -> Result<()> {
            unreachable!("argocd-password never waits on resources")
        }
        fn get_raw(&self, _: &str, _: &Path) -> Result<String> {
            unreachable!("argocd-password never reads raw API paths")
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
}
