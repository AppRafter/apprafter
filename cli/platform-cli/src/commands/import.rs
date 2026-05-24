// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Read live Hetzner Cloud resources labelled `apprafter=true` and
//! rebuild the per-target `state.json`. See plan.md phase 1.2
//! (v0.1.7) for the original scope; v0.1.154 moved the file
//! location from per-cwd to per-target (see
//! `commands::state_paths`).

use cli_core::target::load_active_target_config;
use cli_core::{resolve_hetzner_token, CliError, Result};
use cli_providers::hetzner_cloud::{HetznerCloudClient, APPRAFTER_LABEL, APPRAFTER_LABEL_VALUE};
use cli_state::{HetznerCloudState, State};
use tracing::info;

use crate::commands::hcloud::hcloud_base_url;
use crate::commands::state_paths::resolve_state_paths;

pub fn run(force: bool, dry_run: bool, target_override: Option<&str>) -> Result<()> {
    info!(force, dry_run, target_override, "import invoked");

    // Per-target state + reuse of the target-store handle so the
    // credential resolver below doesn't pay for a second env probe.
    let resolved = resolve_state_paths(target_override)?;
    let paths = resolved.paths;
    let target_store = resolved.store;
    let mut state = State::load_or_default(&paths)?;

    let target_config = load_active_target_config(&target_store, target_override);

    // Provider resolves from state.json → active target's
    // `config.yaml.provider` (v0.1.83 wiring). Makes `import`
    // usable directly after `apprafter target add` without an
    // intervening `init`.
    let provider_id = state
        .provider
        .clone()
        .or_else(|| target_config.as_ref().map(|c| c.provider.clone()))
        .ok_or_else(|| {
            CliError::Other(
                "no provider configured. Run `apprafter target add <name> --provider hetzner-cloud …` (recommended) or `apprafter init --provider hetzner-cloud …`."
                    .to_string(),
            )
        })?;
    if provider_id != "hetzner-cloud" {
        return Err(CliError::Other(format!(
            "provider `{provider_id}` is not yet implemented in this skeleton"
        )));
    }

    // Credential resolution chain (cli-dx-task.md §7).
    let token = resolve_hetzner_token(None, &target_store, target_override)?;

    if state.hetzner_cloud.is_some() && !force {
        return Err(CliError::Other(
            "state already contains hetzner_cloud; pass --force to overwrite".to_string(),
        ));
    }

    // cluster_name precedence: state.json → target_config →
    // default. Same as apply.
    let cluster = state
        .cluster_name
        .clone()
        .or_else(|| target_config.as_ref().and_then(|c| c.cluster_name.clone()))
        .unwrap_or_else(|| "platform-1".into());

    // Seed missing state fields so subsequent destroy/apply/import
    // round-trips in this directory short-circuit via state.json.
    if state.provider.is_none() {
        state.provider = Some(provider_id.clone());
    }
    if state.cluster_name.is_none() {
        state.cluster_name = Some(cluster.clone());
    }
    if state.region.is_none() {
        state.region = target_config.as_ref().and_then(|c| c.region.clone());
    }

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
