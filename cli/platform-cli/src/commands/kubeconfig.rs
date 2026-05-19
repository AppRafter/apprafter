// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Print the k3s kubeconfig for the current cluster, fetching it
//! over SSH on first use and caching the result in state.

use cli_core::secrets::{
    decrypt_with_identity, default_age_key_path, encrypt_for_recipient, load_or_create_identity,
};
use cli_core::target::{default_config_root, TargetStorePaths};
use cli_core::{resolve_hetzner_token, CliError, Result};
use cli_providers::hetzner_cloud::{
    default_ssh_identity_path, rewrite_server_url, HetznerCloudClient, KubeconfigFetcher,
    SshKubeconfigFetcher, APPRAFTER_LABEL, APPRAFTER_LABEL_VALUE,
};
use cli_state::{State, StatePaths};
use tracing::info;

use crate::commands::hcloud::hcloud_base_url;

pub fn run(refresh: bool, target_override: Option<&str>) -> Result<()> {
    info!(refresh, target_override, "kubeconfig invoked");
    let yaml = fetch_and_cache(refresh, target_override)?;
    print!("{yaml}");
    Ok(())
}

/// Resolve / fetch / cache the kubeconfig, returning the YAML
/// without printing. Split out from `run` so `bootstrap-all` (and
/// future orchestrators) can retry the cold-fetch loop in-process
/// without spawning a child or capturing stdout.
pub fn fetch_and_cache(refresh: bool, target_override: Option<&str>) -> Result<String> {
    let cwd = std::env::current_dir()?;
    let paths = StatePaths::for_root(&cwd);
    let mut state = State::load_or_default(&paths)?;

    let hetzner = state.hetzner_cloud.clone().ok_or_else(|| {
        CliError::Other(
            "state has no hetzner_cloud section; run `apprafter apply` first".to_string(),
        )
    })?;

    // Decrypt cached value if present. age-encrypted preferred;
    // legacy plaintext field used as fallback for one cycle.
    let cached_plaintext: Option<String> = if let Some(armored) = &hetzner.kubeconfig_age {
        let identity = load_or_create_identity(&default_age_key_path())?;
        Some(decrypt_with_identity(armored, &identity)?)
    } else {
        hetzner.kubeconfig_yaml.clone()
    };

    if let Some(yaml) = &cached_plaintext {
        if !refresh {
            return Ok(yaml.clone());
        }
    }

    // Cold path or --refresh: SSH-fetch from the live server.
    // Credential resolution chain (cli-dx-task.md §7) — picks up
    // the active target's token when `HCLOUD_TOKEN` isn't set.
    let target_store = TargetStorePaths::for_root(default_config_root()?);
    let token = resolve_hetzner_token(None, &target_store, target_override)?;
    let client = HetznerCloudClient::new(hcloud_base_url(), token);
    let public_ip = resolve_public_ip(&client, hetzner.server_id)?;

    let fetcher = SshKubeconfigFetcher::new(default_ssh_identity_path(), paths.known_hosts_file());
    let yaml = compute_kubeconfig(&fetcher, &public_ip, cached_plaintext.as_deref(), refresh)?;

    // Encrypt before writing back; clear the legacy plaintext slot
    // so future runs go through the age path exclusively.
    let identity = load_or_create_identity(&default_age_key_path())?;
    let armored = encrypt_for_recipient(&yaml, &identity.to_public())?;
    let mut updated = hetzner.clone();
    updated.kubeconfig_yaml = None;
    updated.kubeconfig_age = Some(armored);
    state.hetzner_cloud = Some(updated);
    state.save(&paths)?;
    Ok(yaml)
}

/// Pure orchestration: returns the kubeconfig YAML to print and
/// cache. Decoupled from the real CLI side-effects so tests can
/// drive it with a fake fetcher.
pub(crate) fn compute_kubeconfig<F: KubeconfigFetcher>(
    fetcher: &F,
    public_ip: &str,
    cached: Option<&str>,
    refresh: bool,
) -> Result<String> {
    if let Some(c) = cached {
        if !refresh {
            return Ok(c.to_string());
        }
    }
    let raw = fetcher.fetch(public_ip)?;
    Ok(rewrite_server_url(&raw, public_ip))
}

fn resolve_public_ip(client: &HetznerCloudClient, server_id: u64) -> Result<String> {
    let resp = client.list_servers()?;
    let server = resp
        .servers
        .into_iter()
        .find(|s| {
            s.id == server_id
                && s.labels.get(APPRAFTER_LABEL).map(String::as_str) == Some(APPRAFTER_LABEL_VALUE)
        })
        .ok_or_else(|| {
            CliError::Other(format!(
                "server id {server_id} not found among apprafter-tagged servers"
            ))
        })?;
    let ip = server
        .public_net
        .and_then(|p| p.ipv4)
        .map(|v| v.ip)
        .ok_or_else(|| {
            CliError::Other(format!(
                "server id {server_id} has no public IPv4 yet — wait for cloud-init"
            ))
        })?;
    Ok(ip)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeFetcher {
        body: String,
        called: std::cell::Cell<u32>,
    }

    impl FakeFetcher {
        fn new(body: &str) -> Self {
            Self {
                body: body.into(),
                called: std::cell::Cell::new(0),
            }
        }
    }

    impl KubeconfigFetcher for FakeFetcher {
        fn fetch(&self, _host: &str) -> Result<String> {
            self.called.set(self.called.get() + 1);
            Ok(self.body.clone())
        }
    }

    #[test]
    fn cached_path_returns_cache_and_skips_fetch() {
        let f = FakeFetcher::new("ignored");
        let out = compute_kubeconfig(&f, "1.2.3.4", Some("cached: yaml\n"), false).unwrap();
        assert_eq!(out, "cached: yaml\n");
        assert_eq!(f.called.get(), 0);
    }

    #[test]
    fn cold_path_fetches_and_rewrites_server_url() {
        let f = FakeFetcher::new(
            "apiVersion: v1\nclusters:\n- cluster:\n    server: https://127.0.0.1:6443\n",
        );
        let out = compute_kubeconfig(&f, "203.0.113.10", None, false).unwrap();
        assert_eq!(f.called.get(), 1);
        assert!(out.contains("server: https://203.0.113.10:6443"), "{out}");
        assert!(!out.contains("127.0.0.1"));
    }

    #[test]
    fn refresh_forces_fetch_even_with_cache() {
        let f = FakeFetcher::new("apiVersion: v1\n    server: https://127.0.0.1:6443\n");
        let out = compute_kubeconfig(&f, "10.0.0.1", Some("cached"), true).unwrap();
        assert_eq!(f.called.get(), 1);
        assert!(out.contains("server: https://10.0.0.1:6443"));
    }
}
