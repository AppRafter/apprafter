// SPDX-License-Identifier: FSL-1.1-Apache-2.0
use std::collections::BTreeMap;

use cli_core::target::TargetStorePaths;
use cli_core::{CliError, Result};
use cli_providers::hetzner_cloud::{HetznerCloudClient, HetznerCloudProvider, ServerSpec};
use cli_providers::Provider;
use cli_state::{State, StatePaths, Write};
use tracing::info;

use crate::commands::hcloud::hcloud_base_url;
use crate::commands::state_paths::{resolve_state_paths, write_state};

pub fn run(yes: bool, target_override: Option<&str>) -> Result<()> {
    info!(yes, target_override, "destroy invoked");

    // State now lives under `<config>/state/<active-target>/`
    // (v0.1.154 migration).
    let resolved = resolve_state_paths(target_override)?;
    let paths = resolved.paths;
    let store = resolved.store;
    let state = State::load_or_default(&paths)?;

    let provider_id = state.provider.clone();
    let Some(provider_id) = provider_id else {
        println!("nothing to destroy: no provider in state");
        return Ok(());
    };

    if provider_id != "hetzner-cloud" {
        return Err(CliError::Other(format!(
            "provider `{provider_id}` is not yet implemented in this skeleton"
        )));
    }

    // Empty-state early exit before we even consult the resolver:
    // an operator running `destroy --yes` in a no-Hetzner-state
    // directory expects "nothing to do" rather than a credentials
    // error.
    if yes && state.hetzner_cloud.is_none() {
        println!("nothing to destroy: state has no Hetzner resources");
        return Ok(());
    }

    // Resolution chain (cli-dx-task.md §7), through the core (D.4a b).
    let token = crate::token::hetzner_token(target_override)?;

    if !yes {
        return Err(CliError::Other(
            "refusing to destroy without --yes (this would tear down live infrastructure)"
                .to_string(),
        ));
    }

    let cluster = state
        .cluster_name
        .clone()
        .unwrap_or_else(|| "platform-1".into());
    let region = state.region.clone().unwrap_or_else(|| "nbg1".into());

    let provider = HetznerCloudProvider {
        client: HetznerCloudClient::new(hcloud_base_url(), token),
        spec: ServerSpec {
            name: cluster,
            // destroy() reads live state from the API — server_type is not
            // consulted on the destroy path (no validation, no create call).
            server_type: None,
            image: "ubuntu-24.04".into(),
            location: region.clone(),
            labels: BTreeMap::new(),
            user_data: None,
        },
        // destroy() reads live state from the API; no spec needed.
        ssh_keys: Vec::new(),
        networks: Vec::new(),
        firewalls: Vec::new(),
        floating_ips: Vec::new(),
    };

    // Print a best-effort summary of what was destroyed so the operator can
    // note the type + region for a later `restore --reprovision`.
    // Read from the live API response — the first tagged server carries the
    // server_type name via the Hetzner types response if available.
    let live_before = provider.client.list_servers().ok();
    let server_summary: Option<String> = live_before.as_ref().and_then(|r| {
        r.servers
            .iter()
            .find(|s| s.labels.get("apprafter").map(String::as_str) == Some("true"))
            .and_then(|s| {
                s.server_type
                    .as_ref()
                    .map(|st| format!("type={} region={}", st.name, region))
            })
    });

    let outcome = provider.destroy()?;
    println!(
        "destroy complete: {} resource(s) removed",
        outcome.destroyed
    );
    if let Some(summary) = server_summary {
        println!("  (destroyed server: {summary} — note for restore --reprovision)");
    }

    forget_cluster(&store, &paths)?;

    // Drop the per-cluster known_hosts file alongside the state.
    // The next `apply` against a fresh server (Hetzner happily
    // recycles public IPs) starts with a clean slate; without this
    // step the file would carry the destroyed cluster's host key
    // and the next `kubeconfig` SSH call would fail with `Host key
    // verification failed` until the operator manually removes the
    // entry. Best-effort: ignore the error if the file is already
    // gone (e.g. kubeconfig was never run for this cluster).
    let kh = paths.known_hosts_file();
    if kh.exists() {
        if let Err(e) = std::fs::remove_file(&kh) {
            info!(path = %kh.display(), error = %e, "failed to remove per-cluster known_hosts");
        }
    }

    Ok(())
}

/// `destroy`'s one field: the cluster's record — the provider's objects, and the caches of that
/// cluster with them — dropped from the state read again under the store lock (WI-487). The
/// provider, tier, region and cluster name stay as they are on disk, whoever wrote them last.
fn forget_cluster(store: &TargetStorePaths, paths: &StatePaths) -> Result<()> {
    write_state(store, paths, |state| {
        state.hetzner_cloud = None;
        Ok(Write::Save)
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::state_paths::testing::{cache_kubeconfig, interleave, Scratch};
    use cli_core::Tier;
    use cli_state::seam::WritePoint;

    /// WI-487: what `init` wrote after destroy's first read and before its save — while it
    /// destroyed — stays; only the cluster's record goes.
    #[test]
    fn destroy_keeps_what_init_wrote_while_it_destroyed() {
        let s = Scratch::new();
        let (store, paths) = (s.store.clone(), s.paths.clone());
        let other = interleave(WritePoint::BeforeLock, move |_| {
            crate::commands::init::record(&store, &paths, "hetzner-cloud", Tier::Team, "fsn1")
        });
        forget_cluster(&s.store, &s.paths).unwrap();
        other.result().unwrap();
        let after = s.state();
        assert_eq!(
            (after.tier, after.region.as_deref(), after.hetzner_cloud),
            (Some(Tier::Team), Some("fsn1"), None)
        );
    }

    /// WI-487: destroy reads the state again and saves it while holding the store lock: an
    /// `init` started then waits, and neither loses the other's fields.
    #[test]
    fn init_waits_for_destroy_s_save() {
        let s = Scratch::new();
        let (store, paths) = (s.store.clone(), s.paths.clone());
        let other = interleave(WritePoint::Reread, move |_| {
            crate::commands::init::record(&store, &paths, "hetzner-cloud", Tier::Team, "fsn1")
        });
        forget_cluster(&s.store, &s.paths).unwrap();
        other.result().unwrap();
        let after = s.state();
        assert_eq!(
            (after.tier, after.region.as_deref(), after.hetzner_cloud),
            (Some(Tier::Team), Some("fsn1"), None)
        );
    }

    /// WI-487: a cache write that waited for destroy's save finds the cluster gone and writes
    /// nothing: a destroyed cluster's record never comes back with a cache.
    #[test]
    fn a_cache_write_that_waited_for_destroy_finds_no_cluster() {
        let s = Scratch::new();
        let ctx = s.ctx.clone();
        let other = interleave(WritePoint::Reread, move |signal| {
            cache_kubeconfig(&ctx, signal)
        });
        forget_cluster(&s.store, &s.paths).unwrap();
        let err = other.result().unwrap_err();
        assert!(
            err.to_string()
                .contains("state has no hetzner_cloud section"),
            "{err}"
        );
        assert_eq!(s.state().hetzner_cloud, None);
    }
}
