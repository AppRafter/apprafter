// SPDX-License-Identifier: FSL-1.1-Apache-2.0
use std::collections::BTreeMap;
use std::path::Path;

use cli_core::manifest::{self, InfrastructureManifest};
use cli_core::resolve::resolve_precedence;
use cli_core::target::{
    load_active_target_config, load_target, save_target, TargetConfig, TargetStorePaths,
};
use cli_core::{resolve_hetzner_ssh_public_key, resolve_hetzner_token, CliError, Result};
use cli_providers::backfill::{backfill_from, classify_guard, BackfillOutcome, Guard};
use cli_providers::hetzner_cloud::types::Server;
use cli_providers::hetzner_cloud::{
    build_k3s_user_data, swap_eligible_from_env, FloatingIpSpec, HetznerCloudClient,
    HetznerCloudProvider, K3sBootstrapOptions, NetworkSpec, ServerSpec, SshKeySpec,
};
use cli_providers::Provider;
use cli_state::{HetznerCloudState, State, StatePaths};
use tracing::info;

use crate::commands::firewall_spec::build_firewall_spec;
use crate::commands::hcloud::hcloud_base_url;
use crate::commands::state_paths::resolve_state_paths;

const DEFAULT_NETWORK_IP_RANGE: &str = "10.0.0.0/16";
const DEFAULT_SUBNET_IP_RANGE: &str = "10.0.0.0/24";
const DEFAULT_NETWORK_ZONE: &str = "eu-central";
const DEFAULT_OS_IMAGE: &str = "ubuntu-24.04";

/// Read `APPRAFTER_SERVER_TYPE` from the environment. Returns `None` when the
/// var is absent or empty so the resolution chain treats it as unset rather
/// than propagating an empty string.
pub fn env_server_type() -> Option<String> {
    std::env::var("APPRAFTER_SERVER_TYPE")
        .ok()
        .filter(|s| !s.is_empty())
}

/// Which precedence rung resolved the server type, for the source-print.
/// Pure — takes the same inputs as `resolve_precedence` and returns the
/// first non-None rung's label, or `None` when all inputs are None (meaning
/// no type has been selected yet).
pub fn source_label(
    flag: Option<&str>,
    manifest: Option<&str>,
    state: Option<&str>,
    target: Option<&str>,
    env: Option<&str>,
) -> Option<&'static str> {
    if flag.is_some() {
        Some("--server-type flag")
    } else if manifest.is_some() {
        Some("manifest")
    } else if state.is_some() {
        Some("state")
    } else if target.is_some() {
        Some("target")
    } else if env.is_some() {
        Some("env")
    } else {
        None
    }
}

pub fn run(target_override: Option<&str>, server_type_flag: Option<&str>) -> Result<()> {
    info!(target_override, "apply invoked");

    // State now lives under `<config>/state/<active-target>/`
    // (v0.1.154 migration); see `commands::state_paths` for the
    // rationale and the legacy-file rescue path.
    let resolved = resolve_state_paths(target_override)?;
    let paths = resolved.paths;
    let target_store = resolved.store;
    let target_name = resolved.target_name;
    let mut state = State::load_or_default(&paths)?;

    let target_config = load_active_target_config(&target_store, target_override);

    // Provider resolves from state.json first (legacy `init`
    // path) and falls back to the active target's
    // `config.yaml.provider` — that's the v0.1.83 wiring that
    // makes `apprafter target add → apply` work without an
    // explicit `init`. `init` stays a command for operators who
    // prefer the explicit two-step setup; it just isn't
    // mandatory anymore.
    let provider_id = state
        .provider
        .clone()
        .or_else(|| target_config.as_ref().map(|c| c.provider.clone()))
        .ok_or_else(|| {
            CliError::Other(
                "no provider configured. Run `apprafter target add <name> --provider hetzner-cloud …` (recommended) or the legacy `apprafter init --provider hetzner-cloud --tier solo --region nbg1`."
                    .to_string(),
            )
        })?;

    if provider_id != "hetzner-cloud" {
        return Err(CliError::Other(format!(
            "provider `{provider_id}` is not yet implemented in this skeleton"
        )));
    }

    // Credential resolution chain (cli-dx-task.md §7): --token
    // flag (none wired on apply yet) → HCLOUD_TOKEN env → active
    // target's credentials.yaml. `--target` (when supplied)
    // overrides the active-pointer step. Backwards-compat
    // preserved — env-var-only workflows keep working.
    let token = resolve_hetzner_token(None, &target_store, target_override)?;

    // Optional manifest path. If APPRAFTER_MANIFEST is set, parse
    // and overlay onto the v0.1.4 defaults; otherwise keep the
    // hardcoded behaviour. Manifest paths are interpreted relative
    // to cwd because that matches the operator's mental model
    // ("`APPRAFTER_MANIFEST=./infra.yaml apprafter apply` reads the
    // file I'm looking at"); state location no longer touches cwd
    // but manifest-on-disk authoring still does.
    let manifest = match std::env::var("APPRAFTER_MANIFEST") {
        Ok(p) => {
            info!(path = %p, "reading Infrastructure manifest");
            let cwd = std::env::current_dir()?;
            Some(manifest::parse_infrastructure(&cwd, Path::new(&p))?)
        }
        Err(_) => None,
    };

    // cluster_name precedence: state.json (filled after first
    // successful apply) → target_config.cluster_name → default
    // "platform-1".
    let cluster = state
        .cluster_name
        .clone()
        .or_else(|| target_config.as_ref().and_then(|c| c.cluster_name.clone()))
        .unwrap_or_else(|| "platform-1".into());

    // region: no flag yet on apply — pass None for the flag rung, keep
    // the existing manifest > state > target > default chain.
    let manifest_region = manifest.as_ref().and_then(|m| m.spec.region.clone());
    let state_region = state.region.clone();
    let target_region = target_config.as_ref().and_then(|c| c.region.clone());
    let region_resolved = resolve_precedence(
        None, // no --region flag on apply yet
        manifest_region.as_deref(),
        state_region.as_deref(),
        target_region.as_deref(),
        None, // no env var rung for region
    );
    let region_defaulted = region_resolved.is_none();
    let region = region_resolved.unwrap_or_else(|| "nbg1".into());

    // server type: flag > manifest nodes[0].kind > state (recorded fact from last
    // provision/import) > target preference > APPRAFTER_SERVER_TYPE env.
    // NO default — provisioning a machine is a spend decision; the type must be
    // chosen explicitly. The create path in the provider will fire
    // `CliError::ServerTypeNotSelected` if `None` reaches it. An existing-cluster
    // (no-op) apply succeeds regardless.
    let manifest_node_kind = manifest
        .as_ref()
        .and_then(|m| m.spec.nodes.first())
        .map(|n| n.kind.clone());
    let target_server_type = target_config.as_ref().and_then(|t| t.server_type.clone());
    let state_server_type = state
        .hetzner_cloud
        .as_ref()
        .and_then(|h| h.server_type.clone());

    // 2.16h: capture the stable, pre-apply values used for the fact-drift /
    // deferred-intent guard (run after persist_state so they are never
    // overwritten by the live reconcile before the comparison).
    let state_server_type_at_start = state_server_type.clone();
    let target_server_type_at_start = target_server_type.clone();
    // server_id is only available when a cluster already exists.
    let existing_server_id: Option<u64> = state.hetzner_cloud.as_ref().map(|h| h.server_id);
    let env_type = env_server_type();
    let resolved_type: Option<String> = resolve_precedence(
        server_type_flag,
        manifest_node_kind.as_deref(),
        state_server_type.as_deref(),
        target_server_type.as_deref(),
        env_type.as_deref(),
    );
    let src = source_label(
        server_type_flag,
        manifest_node_kind.as_deref(),
        state_server_type.as_deref(),
        target_server_type.as_deref(),
        env_type.as_deref(),
    );
    match &resolved_type {
        Some(t) => eprintln!("  server type: {t} ({})", src.unwrap_or("unknown")),
        None => eprintln!(
            "  server type: (not selected — will fail on provision; run `apprafter target machine`)"
        ),
    }
    eprintln!(
        "  region: {region}{}",
        if region_defaulted { " (default)" } else { "" }
    );

    // First-run convenience: seed state.json with the resolved
    // provider so future `destroy` / `import` / next-`apply`
    // calls in this directory short-circuit through state
    // without re-reading the target store. Doesn't write to
    // disk yet — `persist_state` at the end of run() flushes.
    if state.provider.is_none() {
        state.provider = Some(provider_id.clone());
    }
    if state.cluster_name.is_none() {
        state.cluster_name = Some(cluster.clone());
    }
    if state.region.is_none() {
        state.region = Some(region.clone());
    }

    let server_spec = build_server_spec(manifest.as_ref(), &cluster, &region, resolved_type);
    let ssh_keys = build_ssh_specs(manifest.as_ref(), &cluster, &target_store, target_override)?;
    let networks = vec![build_network_spec(manifest.as_ref(), &cluster)];
    // 1.83d/1.83h: opt-in Cloudflare origin firewall. The manifest field wins,
    // else the persisted target-store toggle, else off. Fetch CF ranges FIRST —
    // a CF-endpoint outage aborts here (via `?`), before any cloud mutation
    // (`provider.apply()` below), so we never leave a half-provisioned cluster.
    let cf_on = cf_origin_enabled(manifest.as_ref(), target_config.as_ref());
    let cf_ips = resolve_cf_ips(cf_on, &cli_providers::UreqCloudflareIpSource)?;
    let firewalls = vec![build_firewall_spec(
        manifest.as_ref(),
        &cluster,
        cf_ips.as_deref(),
    )];
    let floating_ips = build_floating_ip_specs(manifest.as_ref(), &cluster, &region);

    let provider = HetznerCloudProvider {
        client: HetznerCloudClient::new(hcloud_base_url(), token),
        spec: server_spec,
        ssh_keys,
        networks,
        firewalls,
        floating_ips,
    };

    let outcome = provider.apply()?;
    println!("apply complete: {} action(s)", outcome.applied);

    // persist_state returns the live server list so we can reuse it for the
    // backfill + guard without an extra API round-trip (M17/N8).
    let live_servers = persist_state(&provider, &mut state, &paths, &cluster)?;

    // 2.16h: backfill + guard — exists-path only (server was already present
    // before this apply, i.e. we captured a server_id at apply start).
    if let Some(server_id) = existing_server_id {
        run_backfill_and_guard(
            &live_servers,
            server_id,
            state_server_type_at_start.as_deref(),
            target_server_type_at_start.as_deref(),
            &target_store,
            &target_name,
        );
    }

    Ok(())
}

fn build_server_spec(
    manifest: Option<&InfrastructureManifest>,
    cluster: &str,
    region: &str,
    server_type: Option<String>,
) -> ServerSpec {
    let image = manifest
        .and_then(|m| m.spec.os_image.clone())
        .unwrap_or_else(|| DEFAULT_OS_IMAGE.into());

    // 2.16g: bake host swap only on a single-node cluster (T1 today — the
    // only tier that ships swap; design decision 7). Sum every node role's
    // `count`; ≤1 total node ⇒ single-node ⇒ swap-eligible. A manifest with
    // no `nodes` block provisions the single default server → eligible.
    // The undocumented `APPRAFTER_SKIP_NODE_SWAP` hook forces it off (Q17).
    let total_nodes: u32 = manifest
        .map(|m| m.spec.nodes.iter().map(|n| n.count).sum())
        .unwrap_or(0);
    let single_node = total_nodes <= 1;
    let swap_eligible = swap_eligible_from_env(single_node);

    ServerSpec {
        name: cluster.into(),
        server_type,
        image,
        location: region.into(),
        labels: BTreeMap::new(),
        user_data: Some(build_k3s_user_data(&K3sBootstrapOptions {
            swap_eligible,
            ..Default::default()
        })),
    }
}

fn build_ssh_specs(
    manifest: Option<&InfrastructureManifest>,
    cluster: &str,
    target_store: &TargetStorePaths,
    target_override: Option<&str>,
) -> Result<Vec<SshKeySpec>> {
    // Highest priority: explicit `sshKeys` block in the
    // Infrastructure manifest. Matches the pre-target-store
    // behaviour — operator who hand-edited the manifest meant
    // it.
    if let Some(blocks) = manifest.and_then(|m| m.spec.ssh_keys.as_ref()) {
        return Ok(blocks
            .iter()
            .enumerate()
            .map(|(i, b)| SshKeySpec {
                name: b
                    .name
                    .clone()
                    .unwrap_or_else(|| format!("{cluster}-key-{i}")),
                public_key: b.public_key.clone(),
            })
            .collect());
    }
    // Otherwise the credential resolver picks up
    // APPRAFTER_SSH_PUBLIC_KEY env (legacy) or the active target's
    // ssh_key_path (new). Both produce the SSH public key BODY
    // string Hetzner expects.
    if let Some(public_key) = resolve_hetzner_ssh_public_key(target_store, target_override)? {
        info!(cluster = %cluster, "configuring SSH key from credentials resolver");
        return Ok(vec![SshKeySpec {
            name: format!("{cluster}-key"),
            public_key,
        }]);
    }
    Ok(Vec::new())
}

fn build_network_spec(manifest: Option<&InfrastructureManifest>, cluster: &str) -> NetworkSpec {
    let net = manifest.and_then(|m| m.spec.network.as_ref());
    let ip_range = net
        .and_then(|n| n.ip_range.clone())
        .unwrap_or_else(|| DEFAULT_NETWORK_IP_RANGE.into());
    let subnet = net.and_then(|n| n.subnet.as_ref());
    let subnet_ip_range = subnet
        .and_then(|s| s.ip_range.clone())
        .unwrap_or_else(|| DEFAULT_SUBNET_IP_RANGE.into());
    let network_zone = subnet
        .and_then(|s| s.zone.clone())
        .unwrap_or_else(|| DEFAULT_NETWORK_ZONE.into());
    NetworkSpec {
        name: format!("{cluster}-net"),
        ip_range,
        subnet_ip_range,
        network_zone,
    }
}

/// 1.83h: is the Cloudflare origin firewall on? A manifest `firewall.cloudflareOrigin`
/// (if set) wins; otherwise the persisted target-store toggle; otherwise off.
fn cf_origin_enabled(
    manifest: Option<&InfrastructureManifest>,
    target: Option<&TargetConfig>,
) -> bool {
    manifest
        .and_then(|m| m.spec.firewall.as_ref())
        .and_then(|f| f.cloudflare_origin)
        .or_else(|| {
            target
                .and_then(|t| t.firewall.as_ref())
                .map(|f| f.cloudflare_origin)
        })
        .unwrap_or(false)
}

/// 1.83d: fetch the Cloudflare IP ranges (fail-fast) when `enabled`, else `None`
/// and no network call. Generic over the source so tests inject a mock. Pure
/// w.r.t. cloud state — it runs before the provider is constructed in
/// `apply::run`, so a CF-endpoint outage aborts before any cloud mutation.
fn resolve_cf_ips(
    enabled: bool,
    cf_source: &impl cli_providers::CloudflareIpSource,
) -> Result<Option<Vec<String>>> {
    if enabled {
        Ok(Some(cli_providers::fetch_cloudflare_ips(cf_source)?))
    } else {
        Ok(None)
    }
}

fn build_floating_ip_specs(
    manifest: Option<&InfrastructureManifest>,
    cluster: &str,
    region: &str,
) -> Vec<FloatingIpSpec> {
    let names = manifest
        .and_then(|m| m.spec.network.as_ref())
        .and_then(|n| n.floating_ips.as_ref());
    match names {
        Some(list) if !list.is_empty() => list
            .iter()
            .map(|n| FloatingIpSpec {
                name: format!("{cluster}-{n}"),
                kind: "ipv4".into(),
                home_location: region.into(),
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// Reconcile `State` against the live Hetzner cloud objects and persist it.
///
/// Returns the full live server list fetched during reconciliation so callers
/// can reuse it for the backfill + guard without an extra API round-trip (M17).
///
/// ### Write-once `server_type` (2.16h)
///
/// `server_type` is a recorded FACT — the machine type at creation/import time.
/// It must never be silently overwritten by a subsequent apply (which might be
/// a no-op and the operator might have used `apprafter target machine` to
/// *request* a different type without yet re-provisioning). The rule:
///
/// * Existing recorded value (`state.hetzner_cloud.server_type = Some(...)`) →
///   **keep it**. The fact is already known; overwriting it would destroy the
///   reference point used by the drift guard.
/// * Recorded value is `None` (legacy state before 2.16h) → adopt from the
///   live API response. This is the self-heal path for pre-existing clusters.
fn persist_state(
    provider: &HetznerCloudProvider,
    state: &mut State,
    paths: &StatePaths,
    cluster: &str,
) -> Result<Vec<Server>> {
    let live_servers_response = provider.client.list_servers()?;
    let live_keys = provider.client.list_ssh_keys()?;
    let live_nets = provider.client.list_networks()?;
    let live_fws = provider.client.list_firewalls()?;
    let live_fips = provider.client.list_floating_ips()?;
    let all_servers: Vec<Server> = live_servers_response.servers;
    if let Some(server) = all_servers.iter().find(|s| s.name == cluster) {
        let key_ids: Vec<u64> = live_keys
            .ssh_keys
            .iter()
            .filter(|k| k.labels.get("apprafter").map(String::as_str) == Some("true"))
            .map(|k| k.id)
            .collect();
        let net_id: Option<u64> = live_nets
            .networks
            .iter()
            .find(|n| n.labels.get("apprafter").map(String::as_str) == Some("true"))
            .map(|n| n.id);
        let fw_id: Option<u64> = live_fws
            .firewalls
            .iter()
            .find(|f| f.labels.get("apprafter").map(String::as_str) == Some("true"))
            .map(|f| f.id);
        let fip_ids: Vec<u64> = live_fips
            .floating_ips
            .iter()
            .filter(|f| f.labels.get("apprafter").map(String::as_str) == Some("true"))
            .map(|f| f.id)
            .collect();
        // Write-once: preserve an already-recorded server_type fact; adopt from
        // the live API only when the fact is currently absent (legacy state or
        // first apply on a freshly-provisioned server).
        let recorded_type = state
            .hetzner_cloud
            .as_ref()
            .and_then(|h| h.server_type.clone())
            .or_else(|| server.server_type.as_ref().map(|st| st.name.clone()));
        // Preserve age-encrypted secrets across the state refresh — they are
        // written by `kubeconfig` / `argocd-password` and must not be clobbered
        // by an unrelated `apply`.
        let (kube_yaml, kube_age, argocd_age) = state
            .hetzner_cloud
            .as_ref()
            .map(|h| {
                (
                    h.kubeconfig_yaml.clone(),
                    h.kubeconfig_age.clone(),
                    h.argocd_admin_password_age.clone(),
                )
            })
            .unwrap_or((None, None, None));
        state.hetzner_cloud = Some(HetznerCloudState {
            server_id: server.id,
            server_name: server.name.clone(),
            server_type: recorded_type,
            ssh_key_ids: key_ids,
            network_id: net_id,
            firewall_id: fw_id,
            floating_ip_ids: fip_ids,
            kubeconfig_yaml: kube_yaml,
            kubeconfig_age: kube_age,
            argocd_admin_password_age: argocd_age,
        });
        state.save(paths)?;
    }
    Ok(all_servers)
}

/// 2.16h: backfill + guard — runs once on the exists-path after persist_state.
///
/// Receives the live server list already fetched by `persist_state` (no extra
/// API call). Reads the STABLE pre-apply state and target values (captured
/// before any mutation) so the guard comparison is deterministic.
///
/// Both write failures (backfill) are treated as best-effort: they print a
/// warning but never propagate as errors — a read-only config dir must not
/// fail `apply`.
fn run_backfill_and_guard(
    live_servers: &[Server],
    server_id: u64,
    state_type_at_start: Option<&str>,
    target_type_at_start: Option<&str>,
    target_store: &TargetStorePaths,
    target_name: &str,
) {
    let outcome = backfill_from(live_servers, server_id);
    let live_type = match &outcome {
        BackfillOutcome::Adopt {
            server_type: Some(t),
            ..
        } => t.clone(),
        BackfillOutcome::AmbiguousSkip => {
            eprintln!(
                "warning: multiple servers matched id {server_id} in the live listing — \
                 skipping backfill and drift guard"
            );
            return;
        }
        // Skip (no match) or Adopt with no server_type from API — nothing to do.
        _ => return,
    };

    // Backfill target.server_type when it is absent (best-effort).
    if target_type_at_start.is_none() {
        match load_target(target_store, target_name) {
            Ok(mut target) => {
                if target.config.server_type.is_none() {
                    target.config.server_type = Some(live_type.clone());
                    if let Err(e) = save_target(target_store, &target) {
                        eprintln!(
                            "warning: could not persist server type baseline to target config: {e}"
                        );
                    } else {
                        eprintln!(
                            "  server type baseline established: {live_type} \
                             (recorded from live server — run `apprafter target machine` \
                             to change)"
                        );
                    }
                }
            }
            Err(e) => {
                eprintln!("warning: could not load target config for backfill: {e}");
            }
        }
    }

    // Guard: compare the stable pre-apply fact + preference against live.
    match classify_guard(state_type_at_start, target_type_at_start, &live_type) {
        Guard::FactDriftWarn => {
            eprintln!(
                "warning: the running machine is `{live_type}` but AppRafter recorded \
                 `{}` — it was changed outside AppRafter",
                state_type_at_start.unwrap_or("<unknown>")
            );
        }
        Guard::Intent => {
            eprintln!(
                "  info: server type `{}` is planned for the next provision — \
                 run `apprafter up --reprovision` to apply",
                target_type_at_start.unwrap_or("<unknown>")
            );
        }
        Guard::Silent => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn manifest_from(value: serde_json::Value) -> InfrastructureManifest {
        serde_json::from_value(value).expect("valid InfrastructureManifest JSON")
    }

    fn minimal_manifest() -> InfrastructureManifest {
        manifest_from(json!({
            "apiVersion": "apprafter.io/v1alpha1",
            "kind": "Infrastructure",
            "metadata": {"name": "platform-1"},
            "spec": {"provider": "hetzner-cloud"}
        }))
    }

    fn manifest_with_cf_origin(on: bool) -> InfrastructureManifest {
        manifest_from(json!({
            "apiVersion": "apprafter.io/v1alpha1",
            "kind": "Infrastructure",
            "metadata": {"name": "platform-1"},
            "spec": {
                "provider": "hetzner-cloud",
                "firewall": {"cloudflareOrigin": on}
            }
        }))
    }

    use cli_core::target::{FirewallConfig, TargetConfig};

    fn target_with_cf(on: bool) -> TargetConfig {
        TargetConfig {
            provider: "hetzner-cloud".into(),
            firewall: Some(FirewallConfig {
                cloudflare_origin: on,
            }),
            ..Default::default()
        }
    }

    #[test]
    fn cf_origin_enabled_manifest_wins_over_target() {
        let m = manifest_with_cf_origin(true);
        assert!(cf_origin_enabled(Some(&m), Some(&target_with_cf(false))));
        let m0 = manifest_with_cf_origin(false);
        assert!(!cf_origin_enabled(Some(&m0), Some(&target_with_cf(true))));
    }

    #[test]
    fn cf_origin_enabled_falls_back_to_target_then_false() {
        assert!(cf_origin_enabled(None, Some(&target_with_cf(true))));
        assert!(!cf_origin_enabled(None, Some(&target_with_cf(false))));
        assert!(!cf_origin_enabled(None, None));
    }

    #[test]
    fn resolve_cf_ips_fetches_only_when_enabled_and_fails_fast() {
        use cli_core::{CliError, Result};
        use cli_providers::CloudflareIpSource;
        struct Failing;
        impl CloudflareIpSource for Failing {
            fn get(&self, _url: &str) -> Result<String> {
                Err(CliError::Other("network down".into()))
            }
        }
        assert!(resolve_cf_ips(false, &Failing).unwrap().is_none());
        let err = resolve_cf_ips(true, &Failing).unwrap_err();
        assert!(format!("{err}").contains("cannot fetch Cloudflare IP ranges"));
    }

    #[test]
    fn build_server_spec_stores_none_when_type_not_resolved() {
        // No type is passed — the spec stores None; the provider create-path
        // will fire ServerTypeNotSelected if a provision is attempted.
        let s = build_server_spec(None, "platform-1", "nbg1", None);
        assert_eq!(s.name, "platform-1");
        assert!(
            s.server_type.is_none(),
            "expected None, got {:?}",
            s.server_type
        );
        assert_eq!(s.image, DEFAULT_OS_IMAGE);
        assert_eq!(s.location, "nbg1");
        assert!(s.labels.is_empty());
    }

    #[test]
    fn build_server_spec_stores_resolved_type_when_provided() {
        let s = build_server_spec(None, "platform-1", "nbg1", Some("cpx22".into()));
        assert_eq!(s.server_type.as_deref(), Some("cpx22"));
    }

    #[test]
    fn resolve_precedence_returns_none_when_all_inputs_absent() {
        // No type on any rung → None (no default is applied).
        let result = resolve_precedence(None, None, None, None, None);
        assert!(result.is_none(), "expected None, got {:?}", result);
    }

    #[test]
    fn build_server_spec_overrides_from_manifest_first_node_and_os_image() {
        let m = manifest_from(json!({
            "apiVersion": "apprafter.io/v1alpha1",
            "kind": "Infrastructure",
            "metadata": {"name": "platform-1"},
            "spec": {
                "provider": "hetzner-cloud",
                "nodes": [
                    {"role": "control-plane", "type": "cpx21", "count": 1},
                    {"role": "worker", "type": "cx32", "count": 2}
                ],
                "osImage": "debian-12"
            }
        }));
        // The caller resolves the type via resolve_precedence; here we pass
        // the manifest-derived type directly, which is what run() would do.
        let s = build_server_spec(Some(&m), "cluster-x", "fsn1", Some("cpx21".into()));
        assert_eq!(s.server_type.as_deref(), Some("cpx21"));
        assert_eq!(s.image, "debian-12");
        assert_eq!(s.name, "cluster-x");
        assert_eq!(s.location, "fsn1");
    }

    fn empty_target_store() -> (tempfile::TempDir, TargetStorePaths) {
        let dir = tempfile::tempdir().unwrap();
        let paths = TargetStorePaths::for_root(dir.path().to_path_buf());
        (dir, paths)
    }

    #[test]
    fn build_ssh_specs_returns_manifest_keys_with_default_names_when_unnamed() {
        let m = manifest_from(json!({
            "apiVersion": "apprafter.io/v1alpha1",
            "kind": "Infrastructure",
            "metadata": {"name": "p"},
            "spec": {
                "provider": "hetzner-cloud",
                "sshKeys": [
                    {"public_key": "ssh-ed25519 AAAA"},
                    {"name": "named", "public_key": "ssh-ed25519 BBBB"}
                ]
            }
        }));
        // Env override + target store must NOT be consulted when
        // the manifest already declares keys.
        std::env::remove_var("APPRAFTER_SSH_PUBLIC_KEY");
        let (_dir, paths) = empty_target_store();
        let specs = build_ssh_specs(Some(&m), "cl", &paths, None).unwrap();
        assert_eq!(specs.len(), 2);
        assert_eq!(specs[0].name, "cl-key-0");
        assert_eq!(specs[0].public_key, "ssh-ed25519 AAAA");
        assert_eq!(specs[1].name, "named");
    }

    #[test]
    fn build_ssh_specs_returns_empty_when_no_manifest_no_env_no_target_store() {
        std::env::remove_var("APPRAFTER_SSH_PUBLIC_KEY");
        let (_dir, paths) = empty_target_store();
        assert!(build_ssh_specs(None, "cl", &paths, None)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn build_network_spec_falls_back_to_defaults_when_block_is_absent() {
        let n = build_network_spec(None, "cl");
        assert_eq!(n.name, "cl-net");
        assert_eq!(n.ip_range, DEFAULT_NETWORK_IP_RANGE);
        assert_eq!(n.subnet_ip_range, DEFAULT_SUBNET_IP_RANGE);
        assert_eq!(n.network_zone, DEFAULT_NETWORK_ZONE);
    }

    #[test]
    fn build_network_spec_overlays_manifest_values() {
        let m = manifest_from(json!({
            "apiVersion": "apprafter.io/v1alpha1",
            "kind": "Infrastructure",
            "metadata": {"name": "p"},
            "spec": {
                "provider": "hetzner-cloud",
                "network": {
                    "ip_range": "192.168.0.0/16",
                    "subnet": {"ip_range": "192.168.10.0/24", "zone": "us-east"}
                }
            }
        }));
        let n = build_network_spec(Some(&m), "p");
        assert_eq!(n.ip_range, "192.168.0.0/16");
        assert_eq!(n.subnet_ip_range, "192.168.10.0/24");
        assert_eq!(n.network_zone, "us-east");
    }

    #[test]
    fn build_server_spec_attaches_cloud_init_user_data_by_default() {
        let s = build_server_spec(None, "platform-1", "nbg1", Some("cpx22".into()));
        let yaml = s.user_data.expect("user_data set by default");
        assert!(yaml.starts_with("#cloud-config"), "{yaml}");
        assert!(yaml.contains("get.k3s.io"));
    }

    // All swap-eligibility cases live in ONE test: they mutate the
    // process-wide `APPRAFTER_SKIP_NODE_SWAP` env var, so splitting them into
    // separate `#[test]`s would let cargo's parallel runner race the var
    // (exactly the hazard cli-core's TEST_ENV_MUTEX guards against). Save +
    // restore once, run the three cases sequentially.
    #[test]
    fn build_server_spec_threads_swap_eligibility_from_node_count_and_env() {
        let saved = std::env::var("APPRAFTER_SKIP_NODE_SWAP").ok();

        // (1) Single control-plane node ⇒ eligible ⇒ SKIP_START + swap baked.
        std::env::remove_var("APPRAFTER_SKIP_NODE_SWAP");
        let single = manifest_from(json!({
            "apiVersion": "apprafter.io/v1alpha1",
            "kind": "Infrastructure",
            "metadata": {"name": "solo"},
            "spec": {
                "provider": "hetzner-cloud",
                "nodes": [{"role": "control-plane", "type": "cpx22", "count": 1}]
            }
        }));
        let yaml = build_server_spec(Some(&single), "solo", "nbg1", Some("cpx22".into()))
            .user_data
            .expect("user_data");
        assert!(
            yaml.contains("INSTALL_K3S_SKIP_START=true"),
            "single-node ⇒ swap-eligible SKIP_START install\n{yaml}"
        );
        assert!(
            yaml.contains("/var/lib/apprafter/swap-provision.status"),
            "swap breadcrumb baked for a single-node cluster\n{yaml}"
        );

        // (2) Total node count > 1 ⇒ NOT single-node ⇒ swap NOT baked.
        let multi = manifest_from(json!({
            "apiVersion": "apprafter.io/v1alpha1",
            "kind": "Infrastructure",
            "metadata": {"name": "multi"},
            "spec": {
                "provider": "hetzner-cloud",
                "nodes": [
                    {"role": "control-plane", "type": "cpx22", "count": 1},
                    {"role": "worker", "type": "cpx22", "count": 2}
                ]
            }
        }));
        let yaml = build_server_spec(Some(&multi), "multi", "nbg1", Some("cpx22".into()))
            .user_data
            .expect("user_data");
        assert!(
            !yaml.contains("INSTALL_K3S_SKIP_START"),
            "multi-node ⇒ one-shot install, no swap bootstrap\n{yaml}"
        );
        assert!(
            !yaml.contains("/swapfile"),
            "multi-node must not bake swap\n{yaml}"
        );

        // (3) The undocumented env hook wins over the single-node default.
        std::env::set_var("APPRAFTER_SKIP_NODE_SWAP", "1");
        let yaml = build_server_spec(None, "solo", "nbg1", Some("cpx22".into()))
            .user_data
            .expect("user_data");
        assert!(
            !yaml.contains("INSTALL_K3S_SKIP_START"),
            "APPRAFTER_SKIP_NODE_SWAP must force the one-shot (no-swap) install\n{yaml}"
        );
        assert!(
            !yaml.contains("/swapfile"),
            "APPRAFTER_SKIP_NODE_SWAP must suppress the swap script\n{yaml}"
        );

        // Restore whatever the outer environment had.
        match saved {
            Some(v) => std::env::set_var("APPRAFTER_SKIP_NODE_SWAP", v),
            None => std::env::remove_var("APPRAFTER_SKIP_NODE_SWAP"),
        }
    }

    #[test]
    fn build_floating_ip_specs_is_empty_when_no_floating_ips_declared() {
        assert!(build_floating_ip_specs(None, "cl", "nbg1").is_empty());
        let m = minimal_manifest();
        assert!(build_floating_ip_specs(Some(&m), "cl", "nbg1").is_empty());
    }

    #[test]
    fn build_floating_ip_specs_prefixes_names_with_cluster() {
        let m = manifest_from(json!({
            "apiVersion": "apprafter.io/v1alpha1",
            "kind": "Infrastructure",
            "metadata": {"name": "p"},
            "spec": {
                "provider": "hetzner-cloud",
                "network": {"floatingIPs": ["egress", "ingress"]}
            }
        }));
        let v = build_floating_ip_specs(Some(&m), "cl", "nbg1");
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].name, "cl-egress");
        assert_eq!(v[0].kind, "ipv4");
        assert_eq!(v[0].home_location, "nbg1");
        assert_eq!(v[1].name, "cl-ingress");
    }

    // ── source_label unit tests (pure, N34/M24) ───────────────────────────────

    #[test]
    fn source_label_flag_wins_when_set() {
        assert_eq!(
            source_label(
                Some("cpx22"),
                Some("cpx32"),
                Some("cpx42"),
                Some("cpx52"),
                Some("cpx62")
            ),
            Some("--server-type flag")
        );
    }

    #[test]
    fn source_label_manifest_when_flag_absent() {
        assert_eq!(
            source_label(
                None,
                Some("cpx32"),
                Some("cpx42"),
                Some("cpx52"),
                Some("cpx62")
            ),
            Some("manifest")
        );
    }

    #[test]
    fn source_label_state_when_flag_and_manifest_absent() {
        assert_eq!(
            source_label(None, None, Some("cpx42"), Some("cpx52"), Some("cpx62")),
            Some("state")
        );
    }

    #[test]
    fn source_label_target_when_flag_manifest_state_absent() {
        assert_eq!(
            source_label(None, None, None, Some("cpx52"), Some("cpx62")),
            Some("target")
        );
    }

    #[test]
    fn source_label_env_when_only_env_set() {
        assert_eq!(
            source_label(None, None, None, None, Some("cpx62")),
            Some("env")
        );
    }

    #[test]
    fn source_label_none_when_all_absent() {
        // No type on any rung → None (no default exists any more).
        assert_eq!(source_label(None, None, None, None, None), None);
    }
}
