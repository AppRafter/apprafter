// SPDX-License-Identifier: FSL-1.1-MIT
//! Read live Hetzner Cloud resources labelled `apprafter=true` and
//! rebuild `.apprafter/state.json`. See plan.md phase 1.2 (v0.1.7).

use cli_core::{CliError, Result};
use cli_providers::hetzner_cloud::{HetznerCloudClient, APPRAFTER_LABEL, APPRAFTER_LABEL_VALUE};
use cli_state::{HetznerCloudState, State, StatePaths};
use tracing::info;

use crate::commands::hcloud::hcloud_base_url;

pub fn run(force: bool, dry_run: bool) -> Result<()> {
    info!(force, dry_run, "import invoked");
    let cwd = std::env::current_dir()?;
    let paths = StatePaths::for_root(&cwd);
    let mut state = State::load_or_default(&paths)?;

    let provider_id = state.provider.clone().ok_or_else(|| {
        CliError::Other("state has no provider — run `apprafter init …` first".to_string())
    })?;
    if provider_id != "hetzner-cloud" {
        return Err(CliError::Other(format!(
            "provider `{provider_id}` is not yet implemented in this skeleton"
        )));
    }

    let token = std::env::var("HCLOUD_TOKEN").map_err(|_| {
        CliError::Other("HCLOUD_TOKEN env var is required for hetzner-cloud import".to_string())
    })?;

    if state.hetzner_cloud.is_some() && !force {
        return Err(CliError::Other(
            "state already contains hetzner_cloud; pass --force to overwrite".to_string(),
        ));
    }

    let cluster = state
        .cluster_name
        .clone()
        .unwrap_or_else(|| "platform-1".into());

    let client = HetznerCloudClient::new(hcloud_base_url(), token);
    let snapshot = match build_snapshot(&client, &cluster)? {
        Some(s) => s,
        None => {
            println!(
                "import: no matching Hetzner resources tagged `{APPRAFTER_LABEL}={APPRAFTER_LABEL_VALUE}` for cluster `{cluster}`"
            );
            return Ok(());
        }
    };

    print_summary(dry_run, &cluster, &snapshot);

    if dry_run {
        return Ok(());
    }

    state.hetzner_cloud = Some(snapshot);
    state.save(&paths)?;
    Ok(())
}

fn build_snapshot(client: &HetznerCloudClient, cluster: &str) -> Result<Option<HetznerCloudState>> {
    let labelled = |labels: &std::collections::BTreeMap<String, String>| {
        labels.get(APPRAFTER_LABEL).map(String::as_str) == Some(APPRAFTER_LABEL_VALUE)
    };

    let servers = client.list_servers()?;
    let Some(server) = servers
        .servers
        .into_iter()
        .find(|s| s.name == cluster && labelled(&s.labels))
    else {
        return Ok(None);
    };

    let ssh_key_ids: Vec<u64> = client
        .list_ssh_keys()?
        .ssh_keys
        .into_iter()
        .filter(|k| labelled(&k.labels))
        .map(|k| k.id)
        .collect();
    let network_id: Option<u64> = client
        .list_networks()?
        .networks
        .into_iter()
        .find(|n| labelled(&n.labels))
        .map(|n| n.id);
    let firewall_id: Option<u64> = client
        .list_firewalls()?
        .firewalls
        .into_iter()
        .find(|f| labelled(&f.labels))
        .map(|f| f.id);
    let floating_ip_ids: Vec<u64> = client
        .list_floating_ips()?
        .floating_ips
        .into_iter()
        .filter(|f| labelled(&f.labels))
        .map(|f| f.id)
        .collect();

    Ok(Some(HetznerCloudState {
        server_id: server.id,
        server_name: server.name,
        ssh_key_ids,
        network_id,
        firewall_id,
        floating_ip_ids,
        kubeconfig_yaml: None,
        kubeconfig_age: None,
        argocd_admin_password_age: None,
    }))
}

fn print_summary(dry_run: bool, cluster: &str, s: &HetznerCloudState) {
    let prefix = if dry_run {
        format!("import: would write state for cluster `{cluster}`")
    } else {
        format!("import: wrote state for cluster `{cluster}`")
    };
    println!("{prefix}");
    println!("  server: {} (id {})", s.server_name, s.server_id);
    println!("  ssh keys: {}", s.ssh_key_ids.len());
    match s.network_id {
        Some(id) => println!("  network: {id}"),
        None => println!("  network: none"),
    }
    match s.firewall_id {
        Some(id) => println!("  firewall: {id}"),
        None => println!("  firewall: none"),
    }
    println!("  floating IPs: {}", s.floating_ip_ids.len());
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Stand up the five list endpoints with a configurable server
    /// list. Other resource lists default to a single labelled item
    /// each so we can assert the snapshot picks them up.
    fn mock_full_project(server: &mut mockito::ServerGuard, servers_body: &str) {
        server
            .mock("GET", "/v1/servers")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(servers_body)
            .create();
        server
            .mock("GET", "/v1/ssh_keys")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{"ssh_keys":[
                    {"id":7,"name":"k","public_key":"ssh-ed25519 AAA",
                     "fingerprint":"a","labels":{"apprafter":"true"}}
                ]}"#,
            )
            .create();
        server
            .mock("GET", "/v1/networks")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{"networks":[
                    {"id":100,"name":"n","ip_range":"10.0.0.0/16",
                     "subnets":[],"labels":{"apprafter":"true"}}
                ]}"#,
            )
            .create();
        server
            .mock("GET", "/v1/firewalls")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{"firewalls":[
                    {"id":200,"name":"fw","rules":[],"labels":{"apprafter":"true"}}
                ]}"#,
            )
            .create();
        server
            .mock("GET", "/v1/floating_ips")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{"floating_ips":[
                    {"id":300,"type":"ipv4","ip":"1.2.3.4","name":"fip",
                     "home_location":{"name":"nbg1"},"labels":{"apprafter":"true"}}
                ]}"#,
            )
            .create();
    }

    #[test]
    fn build_snapshot_returns_some_when_apprafter_server_matches_cluster() {
        let mut server = mockito::Server::new();
        mock_full_project(
            &mut server,
            r#"{"servers":[
                {"id":42,"name":"platform-1","status":"running",
                 "labels":{"apprafter":"true"}}
            ]}"#,
        );
        let client = HetznerCloudClient::new(server.url(), "test-token");

        let snap = build_snapshot(&client, "platform-1")
            .expect("build_snapshot")
            .expect("Some(snapshot)");
        assert_eq!(snap.server_id, 42);
        assert_eq!(snap.server_name, "platform-1");
        assert_eq!(snap.ssh_key_ids, vec![7]);
        assert_eq!(snap.network_id, Some(100));
        assert_eq!(snap.firewall_id, Some(200));
        assert_eq!(snap.floating_ip_ids, vec![300]);
    }

    #[test]
    fn build_snapshot_returns_none_when_server_lacks_apprafter_label() {
        let mut server = mockito::Server::new();
        mock_full_project(
            &mut server,
            r#"{"servers":[
                {"id":42,"name":"platform-1","status":"running","labels":{}}
            ]}"#,
        );
        let client = HetznerCloudClient::new(server.url(), "test-token");
        assert!(build_snapshot(&client, "platform-1").unwrap().is_none());
    }

    #[test]
    fn build_snapshot_returns_none_when_server_name_does_not_match() {
        let mut server = mockito::Server::new();
        mock_full_project(
            &mut server,
            r#"{"servers":[
                {"id":42,"name":"other-cluster","status":"running",
                 "labels":{"apprafter":"true"}}
            ]}"#,
        );
        let client = HetznerCloudClient::new(server.url(), "test-token");
        assert!(build_snapshot(&client, "platform-1").unwrap().is_none());
    }

    #[test]
    fn build_snapshot_filters_out_unlabelled_resources_in_each_category() {
        let mut server = mockito::Server::new();
        // The matching server is labelled (so we get past the
        // early return), but the ssh-key, network, firewall, and
        // floating-IP lists each contain a single unlabelled item.
        // Snapshot must come back with empty / None in those slots.
        server
            .mock("GET", "/v1/servers")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{"servers":[
                    {"id":42,"name":"platform-1","status":"running",
                     "labels":{"apprafter":"true"}}
                ]}"#,
            )
            .create();
        server
            .mock("GET", "/v1/ssh_keys")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{"ssh_keys":[
                    {"id":1,"name":"foreign","public_key":"ssh-ed25519 X",
                     "fingerprint":"f","labels":{}}
                ]}"#,
            )
            .create();
        server
            .mock("GET", "/v1/networks")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{"networks":[
                    {"id":2,"name":"foreign","ip_range":"172.16.0.0/16",
                     "subnets":[],"labels":{}}
                ]}"#,
            )
            .create();
        server
            .mock("GET", "/v1/firewalls")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{"firewalls":[
                    {"id":3,"name":"foreign","rules":[],"labels":{}}
                ]}"#,
            )
            .create();
        server
            .mock("GET", "/v1/floating_ips")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{"floating_ips":[
                    {"id":4,"type":"ipv4","ip":"1.2.3.4","name":"foreign",
                     "home_location":{"name":"nbg1"},"labels":{}}
                ]}"#,
            )
            .create();
        let client = HetznerCloudClient::new(server.url(), "test-token");

        let snap = build_snapshot(&client, "platform-1")
            .expect("ok")
            .expect("some");
        assert!(snap.ssh_key_ids.is_empty());
        assert_eq!(snap.network_id, None);
        assert_eq!(snap.firewall_id, None);
        assert!(snap.floating_ip_ids.is_empty());
    }

    #[test]
    fn print_summary_dry_run_branch_does_not_panic() {
        // Trivial smoke for the non-dry-run + dry-run branches and
        // the network/firewall None arms. It only asserts the call
        // returns and prints something — println output is captured
        // by libtest by default, so we don't try to inspect stdout.
        let s = HetznerCloudState {
            server_id: 1,
            server_name: "x".into(),
            ssh_key_ids: vec![],
            network_id: None,
            firewall_id: None,
            floating_ip_ids: vec![],
            kubeconfig_yaml: None,
            kubeconfig_age: None,
            argocd_admin_password_age: None,
        };
        print_summary(true, "cl", &s);
        print_summary(false, "cl", &s);
    }
}
