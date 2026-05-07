// SPDX-License-Identifier: FSL-1.1-MIT
//! Print the k3s kubeconfig for the current cluster, fetching it
//! over SSH on first use and caching the result in state.

use cli_core::{CliError, Result};
use cli_providers::hetzner_cloud::{
    default_ssh_identity_path, rewrite_server_url, HetznerCloudClient, KubeconfigFetcher,
    SshKubeconfigFetcher, APPRAFTER_LABEL, APPRAFTER_LABEL_VALUE,
};
use cli_state::{State, StatePaths};
use tracing::info;

use crate::commands::hcloud::hcloud_base_url;

pub fn run(refresh: bool) -> Result<()> {
    info!(refresh, "kubeconfig invoked");
    let cwd = std::env::current_dir()?;
    let paths = StatePaths::for_root(&cwd);
    let mut state = State::load_or_default(&paths)?;

    let hetzner = state.hetzner_cloud.clone().ok_or_else(|| {
        CliError::Other(
            "state has no hetzner_cloud section; run `platform-cli apply` first".to_string(),
        )
    })?;

    let cached = hetzner.kubeconfig_yaml.clone();
    if let Some(yaml) = &cached {
        if !refresh {
            print!("{yaml}");
            return Ok(());
        }
    }

    // Cold path or --refresh: SSH-fetch from the live server.
    let token = std::env::var("HCLOUD_TOKEN").map_err(|_| {
        CliError::Other(
            "HCLOUD_TOKEN env var is required to fetch kubeconfig from a live cluster".to_string(),
        )
    })?;
    let client = HetznerCloudClient::new(hcloud_base_url(), token);
    let public_ip = resolve_public_ip(&client, hetzner.server_id)?;

    let fetcher = SshKubeconfigFetcher::new(default_ssh_identity_path());
    let yaml = compute_kubeconfig(&fetcher, &public_ip, cached.as_deref(), refresh)?;

    let mut updated = hetzner.clone();
    updated.kubeconfig_yaml = Some(yaml.clone());
    state.hetzner_cloud = Some(updated);
    state.save(&paths)?;
    print!("{yaml}");
    Ok(())
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
