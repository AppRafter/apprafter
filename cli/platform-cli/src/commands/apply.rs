// SPDX-License-Identifier: FSL-1.1-Apache-2.0
use std::collections::BTreeMap;
use std::path::Path;

use cli_core::manifest::{self, InfrastructureManifest};
use cli_core::resolve::resolve_precedence;
use cli_core::target::{
    load_active_target_config, load_target_config, save_target_config, TargetConfig,
    TargetStorePaths,
};
use cli_core::{resolve_hetzner_ssh_public_key, CliError, Result};
use cli_providers::backfill::{backfill_from, classify_guard, BackfillOutcome, Guard};
use cli_providers::hetzner_cloud::types::Server;
use cli_providers::hetzner_cloud::{
    build_k3s_user_data, swap_eligible_from_env, FloatingIpSpec, HetznerCloudClient,
    HetznerCloudProvider, K3sBootstrapOptions, NetworkSpec, ServerSpec, SshKeySpec,
};
use cli_providers::Provider;
use cli_state::{HetznerCloudState, State, StatePaths, Write};
use tracing::info;

use crate::commands::firewall_spec::build_firewall_spec;
use crate::commands::hcloud::hcloud_base_url;
use crate::commands::state_paths::{resolve_state_paths, write_state};

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

    // Credential resolution (cli-dx-task.md §7): HCLOUD_TOKEN, else the
    // stored token of `--target` or the CLI default, through the core
    // (`crate::token`, D.4a T15).
    let token = crate::token::hetzner_token(target_override)?;

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
    // M27/B2: also capture the pre-apply target region so the backfill can
    // adopt the live region when target.config.region was absent (legacy
    // targets). Region is immutable on a running server — no drift guard needed.
    let target_region_at_start = target_region.clone();
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
    // disk yet — `persist_state` at the end of run() writes the
    // seeds into the state it reads again under the store lock,
    // where that state records none (WI-487).
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
    let persisted = persist_state(&provider, &state, &target_store, &paths, &cluster)?;
    let live_servers = persisted.live_servers;

    match existing_server_id {
        // 2.16h exists-path: the server predates this apply (we captured a
        // server_id at apply start). Fill the target's empty slots and warn on
        // fact drift — the machine may have been changed outside AppRafter.
        Some(server_id) => run_backfill_and_guard(
            &live_servers,
            server_id,
            state_server_type_at_start.as_deref(),
            target_server_type_at_start.as_deref(),
            target_region_at_start.as_deref(),
            &target_store,
            &target_name,
        ),
        // v0.2.49 create-path: this apply PROVISIONED the server, so the live
        // facts are authoritative — adopt them into the target outright. The
        // new server's id was recorded by `persist_state` just above.
        None => {
            if let Some(new_server_id) = persisted.server_id {
                adopt_provisioned_machine(
                    &live_servers,
                    new_server_id,
                    &target_store,
                    &target_name,
                );
            }
        }
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
    // Every body is sent to the provider as is, so each must be one OpenSSH public key line: a
    // private key is refused here, before any provider call (GOTCHA-149).
    if let Some(blocks) = manifest.and_then(|m| m.spec.ssh_keys.as_ref()) {
        return blocks
            .iter()
            .enumerate()
            .map(|(i, b)| {
                cli_core::ssh_key::parse_public_key(&b.public_key).map_err(|e| {
                    e.refusal(
                        format!("the manifest's `sshKeys[{i}]`"),
                        cli_core::ssh_key::KeySource::Manifest { index: i },
                    )
                })?;
                Ok(SshKeySpec {
                    name: b
                        .name
                        .clone()
                        .unwrap_or_else(|| format!("{cluster}-key-{i}")),
                    public_key: b.public_key.clone(),
                })
            })
            .collect();
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

/// What [`persist_state`] found and recorded.
struct Persisted {
    /// The full live server list, for the backfill + guard (M17).
    live_servers: Vec<Server>,
    /// The id of the cluster's server it recorded; `None` when the listing had no server of
    /// the cluster's name, and nothing was written.
    server_id: Option<u64>,
}

/// Reconcile `State` against the live Hetzner cloud objects and persist it.
///
/// Returns the full live server list fetched during reconciliation so callers
/// can reuse it for the backfill + guard without an extra API round-trip (M17).
///
/// ### Under the store lock, from the state read again (WI-487)
///
/// The provider's listing runs first, without the lock. Then the state is read
/// again under the store's write lock ([`write_state`]) and `apply`'s fields
/// are written into that copy ([`record_apply`]): the cluster's live objects,
/// and `early`'s seeds (provider, cluster name, region) where the state records
/// none. `early` — the copy `run` read before provisioning — gives nothing
/// else: the caches and what sealed them come from the state read under the
/// lock, so a `lock on` re-seal or a `kubeconfig --refresh` that landed while
/// this apply provisioned is kept, never replaced by an older copy.
///
/// ### Write-once `server_type` (2.16h)
///
/// `server_type` is a recorded FACT — the machine type at creation/import time.
/// It must never be silently overwritten by a subsequent apply (which might be
/// a no-op and the operator might have used `apprafter target machine` to
/// *request* a different type without yet re-provisioning). The rule:
///
/// * Existing recorded value (`state.hetzner_cloud.server_type = Some(...)`,
///   as read under the lock) → **keep it**. The fact is already known;
///   overwriting it would destroy the reference point used by the drift guard.
/// * Recorded value is `None` (legacy state before 2.16h) → adopt from the
///   live API response. This is the self-heal path for pre-existing clusters.
fn persist_state(
    provider: &HetznerCloudProvider,
    early: &State,
    store: &TargetStorePaths,
    paths: &StatePaths,
    cluster: &str,
) -> Result<Persisted> {
    let live_servers_response = provider.client.list_servers()?;
    let live_keys = provider.client.list_ssh_keys()?;
    let live_nets = provider.client.list_networks()?;
    let live_fws = provider.client.list_firewalls()?;
    let live_fips = provider.client.list_floating_ips()?;
    let all_servers: Vec<Server> = live_servers_response.servers;
    let Some(server) = all_servers.iter().find(|s| s.name == cluster) else {
        return Ok(Persisted {
            live_servers: all_servers,
            server_id: None,
        });
    };
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
    let live = HetznerCloudState {
        server_id: server.id,
        server_name: server.name.clone(),
        server_type: server.server_type.as_ref().map(|st| st.name.clone()),
        ssh_key_ids: key_ids,
        network_id: net_id,
        firewall_id: fw_id,
        floating_ip_ids: fip_ids,
        kubeconfig_yaml: None,
        kubeconfig_age: None,
        argocd_admin_password_age: None,
        kubeconfig_sealed_to: None,
        argocd_admin_password_sealed_to: None,
    };
    let server_id = Some(live.server_id);
    write_state(store, paths, |state| {
        record_apply(state, early, live);
        Ok(Write::Save)
    })?;
    Ok(Persisted {
        live_servers: all_servers,
        server_id,
    })
}

/// `apply`'s fields written into `state`, the state read again under the store lock: the
/// cluster's `live` objects, its recorded server type kept (write-once, 2.16h) and adopted
/// from `live` only when none is recorded; `seeds`' provider, cluster name and region where
/// `state` records none. The secrets `kubeconfig` and `argocd-password` cached, and the record
/// of what sealed each (F13), stay as `state` holds them: an unrelated `apply` clobbers
/// neither, so a lost key's words still name the key, and a cache another command wrote while
/// this apply ran is not lost (WI-487).
fn record_apply(state: &mut State, seeds: &State, live: HetznerCloudState) {
    if state.provider.is_none() {
        state.provider = seeds.provider.clone();
    }
    if state.cluster_name.is_none() {
        state.cluster_name = seeds.cluster_name.clone();
    }
    if state.region.is_none() {
        state.region = seeds.region.clone();
    }
    let recorded = state.hetzner_cloud.as_ref();
    let mut refreshed = live;
    if let Some(fact) = recorded.and_then(|h| h.server_type.clone()) {
        refreshed.server_type = Some(fact);
    }
    refreshed.carry_caches_from(recorded);
    state.hetzner_cloud = Some(refreshed);
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
///
/// Both `server_type` and `region` backfills are combined into one
/// `load_target_config` + `save_target_config` cycle (config.yaml only: the
/// credentials file is neither read nor written here, D.4a b).
///
/// ### Why there is NO region drift guard
///
/// A server's region is immutable once created — unlike a machine type that
/// could be resized out-of-band, Hetzner does not allow relocating a running
/// server.  There is therefore no "region changed under us" scenario to warn
/// about, so `classify_guard` is intentionally not called for region.
///
/// ### Why there is NO deferred-intent guard
///
/// `target machine` now refuses on a provisioned cluster, so a
/// preference-vs-live mismatch cannot arise from normal usage. The former
/// `Guard::Intent` branch pointed at a non-existent `apprafter up
/// --reprovision` flag and has been removed.
fn run_backfill_and_guard(
    live_servers: &[Server],
    server_id: u64,
    state_type_at_start: Option<&str>,
    target_type_at_start: Option<&str>,
    target_region_at_start: Option<&str>,
    target_store: &TargetStorePaths,
    target_name: &str,
) {
    let outcome = backfill_from(live_servers, server_id);
    let (live_type_opt, live_region_opt) = match &outcome {
        BackfillOutcome::Adopt {
            server_type,
            region,
        } => (server_type.clone(), region.clone()),
        BackfillOutcome::AmbiguousSkip => {
            eprintln!(
                "warning: multiple servers matched id {server_id} in the live listing — \
                 skipping backfill and drift guard"
            );
            return;
        }
        // Skip (no match) — nothing to do.
        BackfillOutcome::Skip => return,
    };

    // Backfill target.server_type and/or target.region when absent (best-effort,
    // combined into one load+save to avoid double disk I/O).
    // `region_to_backfill` encodes the rule: adopt only when the pre-apply slot
    // was empty (see its doc-comment for the full rationale).
    let need_type_backfill = target_type_at_start.is_none() && live_type_opt.is_some();
    let region_candidate = region_to_backfill(target_region_at_start, live_region_opt.as_deref());
    let need_region_backfill = region_candidate.is_some();
    if need_type_backfill || need_region_backfill {
        match backfill_store_lock(target_store, BACKFILL_LOCK_WAIT) {
            Err(why) => eprintln!("warning: could not persist baseline to target config: {why}"),
            Ok(_store_lock) => match load_target_config(target_store, target_name) {
                Ok(mut config) => {
                    let mut changed = false;
                    if need_type_backfill && config.server_type.is_none() {
                        let t = live_type_opt.as_deref().unwrap();
                        config.server_type = Some(t.to_string());
                        eprintln!(
                            "  server type baseline established: {t} \
                             (recorded from live server — run `apprafter target machine` \
                             to change)"
                        );
                        changed = true;
                    }
                    if let Some(r) = region_candidate.filter(|_| config.region.is_none()) {
                        config.region = Some(r.to_string());
                        eprintln!(
                            "  region baseline established: {r} \
                             (recorded from live server)"
                        );
                        changed = true;
                    }
                    if changed {
                        if let Err(e) = save_target_config(target_store, target_name, &config) {
                            eprintln!("warning: could not persist baseline to target config: {e}");
                        }
                    }
                }
                Err(e) => {
                    eprintln!("warning: could not load target config for backfill: {e}");
                }
            },
        }
    }

    // Guard: compare the stable pre-apply fact against live (server type only —
    // region is immutable on a running server, no drift is possible).
    let live_type = match live_type_opt {
        Some(t) => t,
        None => return, // no type from API — nothing to guard
    };
    match classify_guard(state_type_at_start, &live_type) {
        Guard::FactDriftWarn => {
            eprintln!(
                "warning: the running machine is `{live_type}` but AppRafter recorded \
                 `{}` — it was changed outside AppRafter",
                state_type_at_start.unwrap_or("<unknown>")
            );
        }
        Guard::Silent => {}
    }
}

/// v0.2.49: adopt the machine we JUST PROVISIONED into the target config —
/// the create-path counterpart of [`run_backfill_and_guard`].
///
/// ### Why this exists
///
/// `run_backfill_and_guard` runs only when a server already existed at the
/// start of the apply, so a `restore --reprovision --server-type <new>` (which
/// *creates* the server) left the target's stored preference pointing at the
/// OLD machine. `apprafter target show` then reported the previous SKU while
/// the live box and `state.json` were both correct.
///
/// That is worse than cosmetic. The resolution chain is
/// flag → manifest → recorded state → **target preference** → env, and
/// `destroy` clears the recorded state — so the stale preference stops being
/// shadowed and the NEXT reprovision without an explicit `--server-type`
/// silently rebuilds the old, smaller machine. There is also no way to correct
/// it by hand: `apprafter target machine` refuses on a provisioned cluster.
///
/// ### Adopt, don't backfill
///
/// The exists-path fills only EMPTY slots ("the target never recorded one").
/// Here we know the truth authoritatively — we just built the box — so an
/// existing value that disagrees is overwritten. A preference that contradicts
/// the machine we provisioned is not a preference, it is stale data, and
/// leaving it is precisely the silent-downgrade hazard above. Both fields are
/// adopted for the same reason: `region` sits on the same shadowed rung.
///
/// ### No drift guard
///
/// [`classify_guard`] compares the *recorded fact* (`state.hetzner_cloud
/// .server_type`) against live. On the create path that fact is `None` by
/// construction — `existing_server_id` is derived from the same
/// `state.hetzner_cloud`, so reaching here means there was no recorded state to
/// disagree with live — and the classification would be `Guard::Silent` every
/// time. There is nothing to guard: we created this server, so live and intent
/// agree by construction. The create path therefore adopts without guarding.
///
/// Best-effort, exactly like the exists path: a config-write failure prints a
/// warning and never fails an apply that already provisioned successfully.
fn adopt_provisioned_machine(
    live_servers: &[Server],
    server_id: u64,
    target_store: &TargetStorePaths,
    target_name: &str,
) {
    let (live_type, live_region) = match backfill_from(live_servers, server_id) {
        BackfillOutcome::Adopt {
            server_type,
            region,
        } => (server_type, region),
        BackfillOutcome::AmbiguousSkip => {
            eprintln!(
                "warning: multiple servers matched id {server_id} in the live listing — \
                 not recording the provisioned machine in the target config"
            );
            return;
        }
        // No match — nothing authoritative to adopt (never blank a good value).
        BackfillOutcome::Skip => return,
    };
    if live_type.is_none() && live_region.is_none() {
        return; // the API reported neither field; leave the target as it is
    }

    let _store_lock = match backfill_store_lock(target_store, BACKFILL_LOCK_WAIT) {
        Ok(lock) => lock,
        Err(why) => {
            eprintln!("warning: could not persist the provisioned machine to target config: {why}");
            return;
        }
    };
    let mut config = match load_target_config(target_store, target_name) {
        Ok(c) => c,
        Err(e) => {
            eprintln!(
                "warning: could not load target config to record the provisioned machine: {e}"
            );
            return;
        }
    };

    let mut changed = false;
    if let Some(t) = live_type {
        if config.server_type.as_deref() != Some(t.as_str()) {
            match &config.server_type {
                Some(prev) => eprintln!("  target machine updated: {prev} → {t} (provisioned)"),
                None => eprintln!(
                    "  server type baseline established: {t} (recorded from the \
                     provisioned server)"
                ),
            }
            config.server_type = Some(t);
            changed = true;
        }
    }
    if let Some(r) = live_region {
        if config.region.as_deref() != Some(r.as_str()) {
            match &config.region {
                Some(prev) => eprintln!("  target region updated: {prev} → {r} (provisioned)"),
                None => eprintln!(
                    "  region baseline established: {r} (recorded from the provisioned server)"
                ),
            }
            config.region = Some(r);
            changed = true;
        }
    }
    if changed {
        if let Err(e) = save_target_config(target_store, target_name, &config) {
            eprintln!("warning: could not persist the provisioned machine to target config: {e}");
        }
    }
}

/// How long a backfill waits for another AppRafter process to release the
/// target store before it gives up.
const BACKFILL_LOCK_WAIT: std::time::Duration = std::time::Duration::from_secs(10);

/// The store lock around a backfill's load → save. A backfill is
/// best-effort and runs after the apply has done its work, so it waits for
/// another AppRafter process at most `within`, not indefinitely: `Err`
/// carries why the backfill is skipped, for the caller's warning. Where the
/// store cannot be locked at all (read-only, a filesystem without locks)
/// that is warned about and the backfill runs unlocked, as it always has —
/// a save that then fails warns too. `Ok(None)` when there is no store:
/// locking never creates one.
fn backfill_store_lock(
    target_store: &TargetStorePaths,
    within: std::time::Duration,
) -> std::result::Result<Option<cli_core::StoreLock>, String> {
    if !target_store.root().exists() {
        return Ok(None);
    }
    match cli_core::StoreLock::try_exclusive_for(
        target_store,
        within,
        crate::commands::target::report_store_lock_event,
    ) {
        Ok(Some(lock)) => Ok(Some(lock)),
        Ok(None) => Err(format!(
            "another AppRafter process held the target store ({}) for {within:?}",
            target_store.lock_file().display()
        )),
        Err(e) => Err(e.to_string()),
    }
}

/// Pure helper: returns the region that should be written back to
/// `target.config.region` during a backfill pass.
///
/// Returns `Some(adopted)` only when the pre-apply target region was `None`
/// (meaning the slot was empty and the live value fills a genuine gap).
/// When the target already had a region, we leave it alone — the recorded
/// value might be intentionally different from where the server was placed
/// (e.g. a migrated cluster whose target still points to the original DC).
///
/// Used by `run_backfill_and_guard` and independently unit-tested.
pub fn region_to_backfill<'a>(
    pre_apply_region: Option<&str>,
    adopted: Option<&'a str>,
) -> Option<&'a str> {
    if pre_apply_region.is_none() {
        adopted
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cli_core::target::{load_target, save_target};
    use serde_json::json;

    // ── F13: the caches and their seal record survive the state refresh ──

    fn bare(server_id: u64) -> HetznerCloudState {
        HetznerCloudState {
            server_id,
            server_name: "n".into(),
            server_type: None,
            ssh_key_ids: Vec::new(),
            network_id: None,
            firewall_id: None,
            floating_ip_ids: Vec::new(),
            kubeconfig_yaml: None,
            kubeconfig_age: None,
            argocd_admin_password_age: None,
            kubeconfig_sealed_to: None,
            argocd_admin_password_sealed_to: None,
        }
    }

    /// F13 (critic C-M-3): `apply` rebuilds the state's `hetzner_cloud` from the live API after
    /// every run; what a lock sealed the caches to goes with them, or a lost key's words would
    /// fall back to "put that key back" after any `apply` on a locked store. WI-487: they come
    /// from the state `record_apply` is given — the one read again under the store lock — with
    /// the recorded server type (write-once), and the seeds fill only what it lacks.
    #[test]
    fn the_state_refresh_keeps_the_caches_and_what_sealed_them() {
        let vault = || cli_state::SealedTo::StoreVault {
            store_id: "3f9c0a1b7d2e4c56".into(),
            vault_id: None,
        };
        let before = HetznerCloudState {
            server_type: Some("cx22".into()),
            kubeconfig_age: Some("K".into()),
            argocd_admin_password_age: Some("A".into()),
            kubeconfig_sealed_to: Some(vault()),
            argocd_admin_password_sealed_to: Some(vault()),
            ..bare(1)
        };
        let live = HetznerCloudState {
            server_type: Some("cx32".into()),
            ssh_key_ids: vec![7],
            network_id: Some(100),
            firewall_id: Some(200),
            floating_ip_ids: vec![300],
            ..bare(2)
        };
        let seeds = State {
            provider: Some("hetzner-cloud".into()),
            cluster_name: Some("platform-1".into()),
            region: Some("nbg1".into()),
            ..Default::default()
        };
        let recorded = State {
            provider: Some("recorded".into()),
            cluster_name: Some("recorded-cluster".into()),
            region: Some("fsn1".into()),
            tier: Some(cli_core::Tier::Team),
            hetzner_cloud: Some(before.clone()),
        };
        let mut state = recorded.clone();
        record_apply(&mut state, &seeds, live.clone());
        assert_eq!(
            state,
            State {
                hetzner_cloud: Some(HetznerCloudState {
                    server_type: Some("cx22".into()),
                    kubeconfig_age: Some("K".into()),
                    argocd_admin_password_age: Some("A".into()),
                    kubeconfig_sealed_to: Some(vault()),
                    argocd_admin_password_sealed_to: Some(vault()),
                    ..live.clone()
                }),
                ..recorded
            }
        );
        let mut first = State::default();
        record_apply(&mut first, &seeds, live.clone());
        assert_eq!(
            first,
            State {
                hetzner_cloud: Some(live),
                ..seeds
            }
        );
    }

    // ── WI-487: apply's save beside a cache write and `lock on` ─────────

    use crate::commands::state_paths::testing::{
        cache_kubeconfig, interleave, lock_on, Scratch, WaitSignal, YAML,
    };
    use cli_providers::hetzner_cloud::HetznerCloudClient;
    use cli_state::seam::WritePoint;

    /// The five listings `persist_state` reads: server 42 of `platform-1` (type cx22) and one
    /// labelled SSH key (7), network (100), firewall (200) and floating IP (300).
    fn live_project() -> mockito::ServerGuard {
        let mut api = mockito::Server::new();
        for (path, body) in [
            (
                "/v1/servers",
                r#"{"servers":[{"id":42,"name":"platform-1","status":"running",
                    "labels":{"apprafter":"true"},"server_type":{"name":"cx22"}}]}"#,
            ),
            (
                "/v1/ssh_keys",
                r#"{"ssh_keys":[{"id":7,"name":"k","public_key":"ssh-ed25519 AAA",
                    "fingerprint":"a","labels":{"apprafter":"true"}}]}"#,
            ),
            (
                "/v1/networks",
                r#"{"networks":[{"id":100,"name":"n","ip_range":"10.0.0.0/16",
                    "subnets":[],"labels":{"apprafter":"true"}}]}"#,
            ),
            (
                "/v1/firewalls",
                r#"{"firewalls":[{"id":200,"name":"fw","rules":[],
                    "labels":{"apprafter":"true"}}]}"#,
            ),
            (
                "/v1/floating_ips",
                r#"{"floating_ips":[{"id":300,"type":"ipv4","ip":"1.2.3.4","name":"fip",
                    "home_location":{"name":"nbg1"},"labels":{"apprafter":"true"}}]}"#,
            ),
        ] {
            api.mock("GET", path)
                .with_status(200)
                .with_header("content-type", "application/json")
                .with_body(body)
                .create();
        }
        api
    }

    fn provider_at(api: &mockito::ServerGuard) -> HetznerCloudProvider {
        HetznerCloudProvider {
            client: HetznerCloudClient::new(api.url(), "test-token"),
            spec: build_server_spec(None, "platform-1", "nbg1", None),
            ssh_keys: Vec::new(),
            networks: Vec::new(),
            firewalls: Vec::new(),
            floating_ips: Vec::new(),
        }
    }

    /// `apply`'s fields of a cluster record: what `persist_state` records from [`live_project`].
    type Applied = (
        u64,
        Option<String>,
        Vec<u64>,
        Option<u64>,
        Option<u64>,
        Vec<u64>,
    );

    fn applied(h: &HetznerCloudState) -> Applied {
        (
            h.server_id,
            h.server_type.clone(),
            h.ssh_key_ids.clone(),
            h.network_id,
            h.firewall_id,
            h.floating_ip_ids.clone(),
        )
    }

    fn from_live_project() -> Applied {
        (
            42,
            Some("cx22".into()),
            vec![7],
            Some(100),
            Some(200),
            vec![300],
        )
    }

    /// WI-487: a cache `kubeconfig --refresh` wrote after this apply's first read and before
    /// its save — while it provisioned — is in the state apply saves, beside apply's fields.
    #[test]
    fn apply_keeps_a_cache_written_while_it_provisioned() {
        let s = Scratch::new();
        let api = live_project();
        let early = s.state();
        let ctx = s.ctx.clone();
        let other = interleave(WritePoint::BeforeLock, move |signal| {
            cache_kubeconfig(&ctx, signal)
        });
        let persisted =
            persist_state(&provider_at(&api), &early, &s.store, &s.paths, "platform-1").unwrap();
        other.result().unwrap();
        assert_eq!(persisted.server_id, Some(42));
        assert_eq!(applied(&s.hetzner()), from_live_project());
        assert_eq!(s.cached_kubeconfig(&s.ctx).as_deref(), Some(YAML));
    }

    /// WI-487, the race of B73: `lock on` re-seals the cache to the store's own key while this
    /// apply provisions; apply's save keeps the re-sealed cache byte for byte and its record,
    /// never the copy under the shared key it read before.
    #[test]
    fn apply_keeps_the_cache_lock_on_resealed_while_it_provisioned() {
        let s = Scratch::new();
        cache_kubeconfig(&s.ctx, &WaitSignal::none()).unwrap();
        let api = live_project();
        let early = s.state();
        let (ctx, paths) = (s.ctx.clone(), s.paths.clone());
        let other = interleave(WritePoint::BeforeLock, move |signal| {
            lock_on(&ctx, signal);
            State::load_or_default(&paths)
                .unwrap()
                .hetzner_cloud
                .unwrap()
        });
        persist_state(&provider_at(&api), &early, &s.store, &s.paths, "platform-1").unwrap();
        let resealed = other.result();
        assert_ne!(
            resealed.kubeconfig_age,
            early.hetzner_cloud.unwrap().kubeconfig_age,
            "lock on re-sealed the cache"
        );
        assert!(
            matches!(
                resealed.kubeconfig_sealed_to,
                Some(cli_state::SealedTo::StoreVault { .. })
            ),
            "{resealed:?}"
        );
        let h = s.hetzner();
        assert_eq!(
            (&h.kubeconfig_age, &h.kubeconfig_sealed_to),
            (&resealed.kubeconfig_age, &resealed.kubeconfig_sealed_to)
        );
        assert_eq!(applied(&h), from_live_project());
    }

    /// WI-487: apply reads the state again and saves it while holding the store lock: a cache
    /// write started then waits for the lock, and its cache lands in what apply saved.
    #[test]
    fn a_cache_write_waits_for_apply_s_save() {
        let s = Scratch::new();
        let api = live_project();
        let early = s.state();
        let ctx = s.ctx.clone();
        let other = interleave(WritePoint::Reread, move |signal| {
            cache_kubeconfig(&ctx, signal)
        });
        persist_state(&provider_at(&api), &early, &s.store, &s.paths, "platform-1").unwrap();
        other.result().unwrap();
        assert_eq!(applied(&s.hetzner()), from_live_project());
        assert_eq!(s.cached_kubeconfig(&s.ctx).as_deref(), Some(YAML));
    }

    use crate::commands::state_paths::testing::{opened_by_new_password, rekey};

    /// The cache on disk opens with the store key of the vault on disk — opened with the lock
    /// password [`rekey`] set, one derivation at a time (critic round, finding 13) — and
    /// records that vault (B86).
    fn opens_with_the_store_key_on_disk(s: &Scratch, vault_id: &str) {
        use apprafter_core::secrets::FixedVaultAccess;
        let opened = opened_by_new_password(&s.ctx);
        assert_eq!(opened.vault_id.to_string(), vault_id);
        let reader = s
            .ctx
            .clone()
            .with_vault_access(std::sync::Arc::new(FixedVaultAccess::new(
                opened.key.clone(),
                opened.vault_id,
            )));
        assert_eq!(s.cached_kubeconfig(&reader).as_deref(), Some(YAML));
        assert!(
            matches!(
                &s.hetzner().kubeconfig_sealed_to,
                Some(cli_state::SealedTo::StoreVault { vault_id: Some(v), .. }) if v == vault_id
            ),
            "{:?}",
            s.hetzner().kubeconfig_sealed_to
        );
    }

    /// D.4b B73, G-ROT (acceptance 2): a whole key change runs between this apply's first
    /// read and its save — while it provisions — and apply's save keeps the cache the change
    /// re-sealed: it opens with the store key of the vault on disk, never the key the change
    /// retired, and apply's own fields land beside it. Nothing sleeps: the change runs at
    /// apply's `WritePoint::BeforeLock`.
    #[test]
    fn apply_keeps_the_cache_a_key_change_resealed_while_it_provisioned() {
        let s = Scratch::new();
        cache_kubeconfig(&s.ctx, &WaitSignal::none()).unwrap();
        lock_on(&s.ctx, &WaitSignal::none());
        let api = live_project();
        let early = s.state();
        let ctx = s.ctx.clone();
        let other = interleave(WritePoint::BeforeLock, move |signal| rekey(&ctx, signal));
        persist_state(&provider_at(&api), &early, &s.store, &s.paths, "platform-1").unwrap();
        let done = other.result();
        assert_eq!(done.caches_resealed, 1, "{done:?}");
        opens_with_the_store_key_on_disk(&s, &done.vault_id);
        assert_eq!(applied(&s.hetzner()), from_live_project());
    }

    /// D.4b B73, G-ROT: a key change started while apply holds the store lock — its save
    /// under way — waits for it (its own "waiting" event, never a clock), then re-seals the
    /// cache in the state apply saved.
    #[test]
    fn a_key_change_waits_for_apply_s_save() {
        let s = Scratch::new();
        cache_kubeconfig(&s.ctx, &WaitSignal::none()).unwrap();
        lock_on(&s.ctx, &WaitSignal::none());
        let api = live_project();
        let early = s.state();
        let ctx = s.ctx.clone();
        let other = interleave(WritePoint::Reread, move |signal| rekey(&ctx, signal));
        persist_state(&provider_at(&api), &early, &s.store, &s.paths, "platform-1").unwrap();
        let done = other.result();
        assert_eq!(done.caches_resealed, 1, "{done:?}");
        opens_with_the_store_key_on_disk(&s, &done.vault_id);
        assert_eq!(applied(&s.hetzner()), from_live_project());
    }

    /// D.4b B73, G-ROT (critic round, finding 6): an apply started while a key change holds
    /// the store lock — the key change at R3's re-read of this target's state — waits for it
    /// (its own `LockWait`, never a clock), then merges into the state R3 saved: the cache it
    /// saves opens with the store key of the vault on disk, and its own fields land beside
    /// it. The key change runs on this thread; apply on the one `interleave` starts.
    #[test]
    fn apply_started_during_a_key_change_waits_and_keeps_its_cache() {
        let s = Scratch::new();
        cache_kubeconfig(&s.ctx, &WaitSignal::none()).unwrap();
        lock_on(&s.ctx, &WaitSignal::none());
        let api = live_project();
        let provider = provider_at(&api);
        let early = s.state();
        let (store, paths) = (s.store.clone(), s.paths.clone());
        let apply = interleave(WritePoint::Reread, move |_| {
            persist_state(&provider, &early, &store, &paths, "platform-1")
        });
        let done = rekey(&s.ctx, &WaitSignal::none());
        apply.result().unwrap();
        assert_eq!(done.caches_resealed, 1, "{done:?}");
        opens_with_the_store_key_on_disk(&s, &done.vault_id);
        assert_eq!(applied(&s.hetzner()), from_live_project());
    }

    /// WI-487: a `lock on` started while apply holds the store lock waits for it, then
    /// re-seals the cache in the state apply saved: apply cannot write back the copy under the
    /// shared key it read before.
    #[test]
    fn lock_on_waits_for_apply_s_save() {
        let s = Scratch::new();
        cache_kubeconfig(&s.ctx, &WaitSignal::none()).unwrap();
        let under_shared = s.hetzner().kubeconfig_age;
        let api = live_project();
        let early = s.state();
        let ctx = s.ctx.clone();
        let other = interleave(WritePoint::Reread, move |signal| lock_on(&ctx, signal));
        persist_state(&provider_at(&api), &early, &s.store, &s.paths, "platform-1").unwrap();
        let done = other.result();
        assert_eq!(done.caches_resealed, 1, "{done:?}");
        let h = s.hetzner();
        assert_eq!(applied(&h), from_live_project());
        assert!(
            matches!(
                h.kubeconfig_sealed_to,
                Some(cli_state::SealedTo::StoreVault { .. })
            ),
            "{h:?}"
        );
        assert_ne!(
            h.kubeconfig_age, under_shared,
            "re-sealed to the store's key"
        );
    }

    // ── the backfill's store lock: bounded, never creating a store ───────

    /// A store at a fresh temp root, with something in it.
    fn existing_store() -> (tempfile::TempDir, TargetStorePaths) {
        let dir = tempfile::tempdir().unwrap();
        let paths = TargetStorePaths::for_root(dir.path().join("store"));
        std::fs::create_dir_all(paths.root()).unwrap();
        (dir, paths)
    }

    #[test]
    fn a_backfill_takes_a_free_store_lock() {
        let (_dir, paths) = existing_store();
        let lock = backfill_store_lock(&paths, std::time::Duration::from_secs(10))
            .expect("free")
            .expect("the store exists");
        assert!(lock.is_held());
    }

    #[test]
    fn a_backfill_never_creates_a_missing_store_to_lock_it() {
        let dir = tempfile::tempdir().unwrap();
        let paths = TargetStorePaths::for_root(dir.path().join("none"));
        assert!(backfill_store_lock(&paths, std::time::Duration::ZERO)
            .unwrap()
            .is_none());
        assert!(!paths.root().exists());
    }

    /// A lock another process keeps past the bound skips the backfill,
    /// with the reason the warning prints — the apply is not held up.
    #[test]
    fn a_backfill_gives_up_on_a_store_held_past_its_bound() {
        let (_dir, paths) = existing_store();
        let (held_tx, held_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let holder_paths = paths.clone();
        let holder = std::thread::spawn(move || {
            let _lock = cli_core::StoreLock::exclusive(&holder_paths).unwrap();
            held_tx.send(()).unwrap();
            let _ = release_rx.recv();
        });
        held_rx.recv().unwrap();
        let why = backfill_store_lock(&paths, std::time::Duration::from_millis(100))
            .map(|_| ())
            .expect_err("held throughout");
        release_tx.send(()).unwrap();
        holder.join().unwrap();
        assert!(
            why.contains("another AppRafter process held the target store"),
            "{why}"
        );
        assert!(
            why.contains(&paths.lock_file().display().to_string()),
            "{why}"
        );
    }

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

    /// GOTCHA-149: a manifest's key body goes to the provider as is, so a private key there is
    /// refused before any provider call, naming the entry and never quoting it.
    #[test]
    fn build_ssh_specs_refuses_a_private_key_in_the_manifest() {
        let private =
            "-----BEGIN OPENSSH PRIVATE KEY-----\nb3BlbnNzaA==\n-----END OPENSSH PRIVATE KEY-----";
        let m = manifest_from(json!({
            "apiVersion": "apprafter.io/v1alpha1",
            "kind": "Infrastructure",
            "metadata": {"name": "p"},
            "spec": {
                "provider": "hetzner-cloud",
                "sshKeys": [
                    {"public_key": "ssh-ed25519 AAAA"},
                    {"public_key": private}
                ]
            }
        }));
        let (_dir, paths) = empty_target_store();
        let err = build_ssh_specs(Some(&m), "cl", &paths, None).expect_err("a private key");
        assert_eq!(
            err.to_string(),
            "the manifest's `sshKeys[1]` is a private key: AppRafter never sends a private key \
             to the provider"
        );
        // Its help sends the reader to that entry: the manifest outranks the env and the target.
        assert!(
            matches!(
                err,
                CliError::SshKeyNotPublic {
                    from: cli_core::ssh_key::KeySource::Manifest { index: 1 },
                    ..
                }
            ),
            "{err:?}"
        );
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

    // ── region_to_backfill unit tests (M27/B2) ───────────────────────────────

    #[test]
    fn region_to_backfill_adopts_live_region_when_pre_apply_is_none() {
        // Core case: legacy target with no recorded region + live server in fsn1
        // → backfill should write "fsn1".
        assert_eq!(region_to_backfill(None, Some("fsn1")), Some("fsn1"));
    }

    #[test]
    fn region_to_backfill_returns_none_when_pre_apply_region_already_set() {
        // Target already recorded a region — don't overwrite it even if the
        // live server reports a different value.
        assert_eq!(region_to_backfill(Some("nbg1"), Some("fsn1")), None);
        assert_eq!(region_to_backfill(Some("nbg1"), Some("nbg1")), None);
    }

    #[test]
    fn region_to_backfill_returns_none_when_adopted_is_none() {
        // Live API didn't return a region (shouldn't happen in practice, but
        // the helper must handle it gracefully: no backfill).
        assert_eq!(region_to_backfill(None, None), None);
    }

    #[test]
    fn run_backfill_and_guard_sets_region_on_target_when_previously_absent() {
        // Integration-style test: build a temp target store with region=None,
        // run `run_backfill_and_guard` against a fake live-server list, and
        // confirm the saved target has region="fsn1" afterwards.
        use cli_core::target::{
            load_target, save_target, Target, TargetConfig, TargetCredentials, TargetStorePaths,
        };
        use cli_providers::hetzner_cloud::types::{
            LocationRef, Server, ServerStatus, ServerTypeRef,
        };

        let tmp = tempfile::tempdir().unwrap();
        let store = TargetStorePaths::for_root(tmp.path().to_path_buf());

        // Save a target with no server_type and no region.
        let target_name = "test-backfill-region";
        let t = Target {
            name: target_name.to_string(),
            config: TargetConfig {
                provider: "hetzner-cloud".into(),
                server_type: None,
                region: None,
                ..Default::default()
            },
            credentials: TargetCredentials::default(),
        };
        save_target(&store, &t).expect("save target");

        // Build a fake live server list for server id=42 in fsn1.
        let servers = vec![Server {
            id: 42,
            name: "platform-1".into(),
            status: ServerStatus::Running,
            server_type: Some(ServerTypeRef {
                name: "cx22".to_string(),
            }),
            location: Some(LocationRef {
                name: "fsn1".to_string(),
            }),
            public_net: None,
            labels: Default::default(),
        }];

        // server existed before apply → server_id is Some(42).
        // pre-apply target had region=None → target_region_at_start=None.
        run_backfill_and_guard(
            &servers,
            42,
            None, // state_type_at_start
            None, // target_type_at_start
            None, // target_region_at_start (absent — triggers backfill)
            &store,
            target_name,
        );

        let saved = load_target(&store, target_name).expect("load after backfill");
        assert_eq!(
            saved.config.region.as_deref(),
            Some("fsn1"),
            "region should have been backfilled from live server"
        );
        assert_eq!(
            saved.config.server_type.as_deref(),
            Some("cx22"),
            "server_type should also have been backfilled in the same pass"
        );
    }

    // ── adopt_provisioned_machine (create path) ──────────────────────────────

    /// Build a temp target store holding one target with the given stored
    /// machine preference. Returns `(tempdir, store, name)` — the tempdir must
    /// stay alive for the duration of the test.
    fn target_store_with(
        server_type: Option<&str>,
        region: Option<&str>,
    ) -> (
        tempfile::TempDir,
        cli_core::target::TargetStorePaths,
        String,
    ) {
        use cli_core::target::{Target, TargetConfig, TargetCredentials, TargetStorePaths};
        let tmp = tempfile::tempdir().unwrap();
        let store = TargetStorePaths::for_root(tmp.path().to_path_buf());
        let name = "test-adopt".to_string();
        save_target(
            &store,
            &Target {
                name: name.clone(),
                config: TargetConfig {
                    provider: "hetzner-cloud".into(),
                    server_type: server_type.map(str::to_string),
                    region: region.map(str::to_string),
                    ..Default::default()
                },
                credentials: TargetCredentials::default(),
            },
        )
        .expect("save target");
        (tmp, store, name)
    }

    fn live_server(id: u64, server_type: &str, region: &str) -> Server {
        use cli_providers::hetzner_cloud::types::{LocationRef, ServerStatus, ServerTypeRef};
        Server {
            id,
            name: "platform-1".into(),
            status: ServerStatus::Running,
            server_type: Some(ServerTypeRef {
                name: server_type.to_string(),
            }),
            location: Some(LocationRef {
                name: region.to_string(),
            }),
            public_net: None,
            labels: Default::default(),
        }
    }

    #[test]
    fn adopt_provisioned_machine_overwrites_a_stale_target_preference() {
        // The substrate-upgrade case: the target still says cx23 because that
        // is what it was before `destroy`, but we have just provisioned cx33.
        // A preference that disagrees with the box we built is stale data — and
        // it is what the next rebuild would silently use once `destroy` clears
        // the recorded state that currently shadows it.
        let (_tmp, store, name) = target_store_with(Some("cx23"), Some("nbg1"));

        adopt_provisioned_machine(&[live_server(77, "cx33", "fsn1")], 77, &store, &name);

        let saved = load_target(&store, &name).expect("load after adopt");
        assert_eq!(
            saved.config.server_type.as_deref(),
            Some("cx33"),
            "the target must record the machine that was actually provisioned"
        );
        assert_eq!(
            saved.config.region.as_deref(),
            Some("fsn1"),
            "the region actually provisioned is adopted in the same pass"
        );
    }

    /// D.4a b: the backfill and the adopt read and write `config.yaml` only. A credentials file
    /// they cannot read does not stop them, and the one there is left as it was, byte for byte.
    #[test]
    fn the_backfill_and_the_adopt_write_the_config_alone() {
        let (_tmp, store, name) = target_store_with(None, None);
        let creds = store.target_credentials_file(&name);
        let by_hand = "# by hand\nhetzner_token: [\n";
        std::fs::write(&creds, by_hand).unwrap();
        run_backfill_and_guard(
            &[live_server(42, "cx22", "fsn1")],
            42,
            None,
            None,
            None,
            &store,
            &name,
        );
        let config = cli_core::target::load_target_config(&store, &name).unwrap();
        assert_eq!(
            (config.server_type.as_deref(), config.region.as_deref()),
            (Some("cx22"), Some("fsn1"))
        );
        assert_eq!(std::fs::read_to_string(&creds).unwrap(), by_hand);
        adopt_provisioned_machine(&[live_server(43, "cx33", "nbg1")], 43, &store, &name);
        let config = cli_core::target::load_target_config(&store, &name).unwrap();
        assert_eq!(
            (config.server_type.as_deref(), config.region.as_deref()),
            (Some("cx33"), Some("nbg1"))
        );
        assert_eq!(std::fs::read_to_string(&creds).unwrap(), by_hand);
    }

    #[test]
    fn adopt_provisioned_machine_establishes_a_baseline_when_target_was_empty() {
        // Fresh target (no preference recorded yet) → the provisioned facts
        // become the baseline.
        let (_tmp, store, name) = target_store_with(None, None);

        adopt_provisioned_machine(&[live_server(5, "cpx31", "hel1")], 5, &store, &name);

        let saved = load_target(&store, &name).expect("load after adopt");
        assert_eq!(saved.config.server_type.as_deref(), Some("cpx31"));
        assert_eq!(saved.config.region.as_deref(), Some("hel1"));
    }

    #[test]
    fn adopt_provisioned_machine_leaves_target_alone_when_id_not_in_listing() {
        // No live server matched the id we just recorded → nothing authoritative
        // to adopt, so the stored preference must survive untouched.
        let (_tmp, store, name) = target_store_with(Some("cx23"), Some("nbg1"));

        adopt_provisioned_machine(&[live_server(1, "cx33", "fsn1")], 999, &store, &name);

        let saved = load_target(&store, &name).expect("load after no-op adopt");
        assert_eq!(saved.config.server_type.as_deref(), Some("cx23"));
        assert_eq!(saved.config.region.as_deref(), Some("nbg1"));
    }

    #[test]
    fn adopt_provisioned_machine_skips_an_ambiguous_id_match() {
        // Two servers with the same id (defensive — Hetzner ids are unique):
        // we cannot tell which is ours, so we must not overwrite anything.
        let (_tmp, store, name) = target_store_with(Some("cx23"), Some("nbg1"));

        adopt_provisioned_machine(
            &[
                live_server(7, "cx33", "fsn1"),
                live_server(7, "cx43", "nbg1"),
            ],
            7,
            &store,
            &name,
        );

        let saved = load_target(&store, &name).expect("load after ambiguous adopt");
        assert_eq!(saved.config.server_type.as_deref(), Some("cx23"));
        assert_eq!(saved.config.region.as_deref(), Some("nbg1"));
    }

    #[test]
    fn adopt_provisioned_machine_keeps_fields_the_api_did_not_report() {
        // The live API returned no type/location for the server. Adopt nothing
        // rather than blanking a good preference.
        use cli_providers::hetzner_cloud::types::ServerStatus;
        let (_tmp, store, name) = target_store_with(Some("cx23"), Some("nbg1"));
        let bare = Server {
            id: 3,
            name: "platform-1".into(),
            status: ServerStatus::Running,
            server_type: None,
            location: None,
            public_net: None,
            labels: Default::default(),
        };

        adopt_provisioned_machine(&[bare], 3, &store, &name);

        let saved = load_target(&store, &name).expect("load after empty adopt");
        assert_eq!(saved.config.server_type.as_deref(), Some("cx23"));
        assert_eq!(saved.config.region.as_deref(), Some("nbg1"));
    }
}
