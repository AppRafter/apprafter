// SPDX-License-Identifier: FSL-1.1-Apache-2.0
use std::str::FromStr;

use cli_core::target::TargetStorePaths;
use cli_core::{Result, Tier};
use cli_state::{State, StatePaths, Write};
use tracing::info;

use crate::commands::state_paths::{resolve_state_paths, write_state};

pub fn run(provider: &str, tier: &str, region: &str) -> Result<()> {
    let parsed_tier = Tier::from_str(tier)?;
    info!(provider, tier = %parsed_tier, region, "init invoked");

    // Per-target state (v0.1.154). `init` writes the same data
    // `target add` writes; the recommended path is
    // `apprafter target add … && apprafter apply` (apply seeds
    // state from the target config). `init` stays for operators
    // who prefer the explicit two-step setup.
    let resolved = resolve_state_paths(None)?;
    let paths = resolved.paths;
    record(&resolved.store, &paths, provider, parsed_tier, region)?;

    println!("would init: provider={provider} tier={parsed_tier} region={region}");
    println!(
        "(state written to {}; run `apply` next)",
        paths.state_file().display()
    );
    Ok(())
}

/// `init`'s fields — the provider, the tier, the region, and the cluster name when none is
/// recorded — written into the state read again under the store lock (WI-487): everything else
/// in it, a cluster `apply` recorded and the caches with it, stays as it is on disk.
pub(crate) fn record(
    store: &TargetStorePaths,
    paths: &StatePaths,
    provider: &str,
    tier: Tier,
    region: &str,
) -> Result<()> {
    write_state(store, paths, |state: &mut State| {
        state.provider = Some(provider.to_string());
        state.tier = Some(tier);
        state.region = Some(region.to_string());
        if state.cluster_name.is_none() {
            state.cluster_name = Some("platform-1".to_string());
        }
        Ok(Write::Save)
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::state_paths::testing::{
        cache_kubeconfig, interleave, server, Scratch, WaitSignal, YAML,
    };
    use cli_state::seam::WritePoint;

    /// WI-487: `init` writes its fields into the state as it is on disk: the cluster `apply`
    /// recorded and the cache with it stay.
    #[test]
    fn init_keeps_the_cluster_and_its_cache() {
        let s = Scratch::new();
        cache_kubeconfig(&s.ctx, &WaitSignal::none()).unwrap();
        let before = s.hetzner();
        record(&s.store, &s.paths, "hetzner-cloud", Tier::Team, "fsn1").unwrap();
        let after = s.state();
        assert_eq!(
            (
                after.tier,
                after.region.as_deref(),
                after.cluster_name.as_deref()
            ),
            (Some(Tier::Team), Some("fsn1"), Some("platform-1"))
        );
        assert_eq!(after.hetzner_cloud, Some(before));
        assert_eq!(s.cached_kubeconfig(&s.ctx).as_deref(), Some(YAML));
    }

    /// WI-487: `init` reads the state again and saves it while holding the store lock: a cache
    /// write started then waits, and neither loses the other's fields.
    #[test]
    fn a_cache_write_waits_for_init_s_save() {
        let s = Scratch::new();
        let ctx = s.ctx.clone();
        let other = interleave(WritePoint::Reread, move |signal| {
            cache_kubeconfig(&ctx, signal)
        });
        record(&s.store, &s.paths, "hetzner-cloud", Tier::Team, "fsn1").unwrap();
        other.result().unwrap();
        let after = s.state();
        assert_eq!(
            (after.tier, after.region.as_deref()),
            (Some(Tier::Team), Some("fsn1"))
        );
        assert_eq!(s.hetzner().server_id, server(42).server_id);
        assert_eq!(s.cached_kubeconfig(&s.ctx).as_deref(), Some(YAML));
    }
}
