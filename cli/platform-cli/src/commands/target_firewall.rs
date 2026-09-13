// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! `apprafter target firewall cloudflare-origin enable|disable` (1.83h) —
//! manifest-free toggle of the Cloudflare origin firewall: persists the intent
//! in the target store, records it in the cluster's `PlatformStack`, AND
//! immediately reconciles the live Hetzner firewall.
//!
//! The intent is written in TWO places on purpose, and neither is redundant:
//!
//! * the **target store** is what `apprafter apply` reads when it builds the
//!   node's firewall, at a moment when there is no cluster to ask;
//! * the **`PlatformStack` CR** is what a BACKUP can see. The toggle used to
//!   live only on the operator's machine, so no snapshot carried it and a
//!   restore onto a new target brought the node up with 80/443 open to the
//!   internet (A4). `PlatformStack/default` is the first object every backup
//!   captures — including the scheduled in-cluster runner's, which has no
//!   target store to read — so recording it there is what makes the intent
//!   survive every backup mode.

use std::path::Path;

use cli_core::style;
use cli_core::target::{load_target, save_target, FirewallConfig, TargetConfig};
use cli_core::Result;
use cli_providers::hetzner_cloud::{
    Firewall, FirewallRule, APPRAFTER_LABEL, APPRAFTER_LABEL_VALUE,
};
use cli_providers::HetznerCloudClient;
use cli_state::State;
use serde_json::{json, Value};

use crate::cli::{FirewallToggle, TargetFirewallCommand};
use crate::commands::firewall_spec::build_firewall_spec;
use crate::commands::hcloud::hcloud_base_url;
use crate::commands::k8s_helpers::{ensure_kubeconfig_tempfile_for_target, kubectl_merge_patch};
use crate::commands::state_paths::resolve_state_paths;

/// The singleton `PlatformStack` the toggle records itself in.
const PLATFORMSTACK_NAME: &str = "default";
const PLATFORMSTACK_NAMESPACE: &str = "apprafter-system";

/// Cluster name assumed when neither the state file nor the target config
/// names one — matches the `apprafter apply` default.
const DEFAULT_CLUSTER_NAME: &str = "platform-1";

pub fn run(action: TargetFirewallCommand) -> Result<()> {
    match action {
        TargetFirewallCommand::CloudflareOrigin { state } => {
            run_cloudflare_origin(matches!(state, FirewallToggle::Enable))
        }
    }
}

// ---- pure helpers (unit-tested) -------------------------------------------

/// Which cluster's firewall this toggle reconciles.
///
/// The recorded fact (`state.cluster_name`, written by `apply` when the server
/// was actually provisioned) outranks the target's *preference*
/// (`target.config.cluster_name`), which may have been edited after the fact.
/// Falling back to the preference keeps the toggle usable before the first
/// `apply`; [`DEFAULT_CLUSTER_NAME`] is the last resort.
pub(crate) fn resolve_cluster_name(
    state_cluster: Option<&str>,
    target_cluster: Option<&str>,
) -> String {
    state_cluster
        .or(target_cluster)
        .unwrap_or(DEFAULT_CLUSTER_NAME)
        .to_string()
}

/// The Hetzner firewall name `apply` provisions for `cluster`.
pub(crate) fn firewall_name(cluster: &str) -> String {
    format!("{cluster}-fw")
}

/// Pick the firewall to rewrite out of a live listing (the fallback taken when
/// the state file has no cached firewall id).
///
/// The match requires BOTH the exact name AND the `apprafter=true` ownership
/// label: this command replaces a firewall's ENTIRE rule set, so matching on
/// name alone could blow away the rules of an unrelated firewall that merely
/// shares the name in the same Hetzner project.
pub(crate) fn find_owned_firewall_id(firewalls: &[Firewall], fw_name: &str) -> Option<u64> {
    firewalls
        .iter()
        .find(|f| {
            f.name == fw_name
                && f.labels.get(APPRAFTER_LABEL).map(String::as_str) == Some(APPRAFTER_LABEL_VALUE)
        })
        .map(|f| f.id)
}

/// The full desired rule set plus the confirmation the operator reads.
///
/// Both halves come out of the same `cf_ips` decision on purpose: the printed
/// line can then never describe the opposite of the rules that get pushed.
pub(crate) struct OriginReconcile {
    /// Wire rules for `set_firewall_rules`, which replaces the rule set
    /// atomically — so this is always the COMPLETE desired set, not a delta.
    pub rules: Vec<FirewallRule>,
    /// Lines to print after the rules land.
    pub lines: Vec<String>,
}

/// Plan the reconcile: `Some(cf_ips)` restricts tcp/80 + tcp/443 to the
/// Cloudflare ranges, `None` restores the open default set.
pub(crate) fn plan_origin_reconcile(
    cluster: &str,
    cf_ips: Option<&[String]>,
    fw_name: &str,
) -> OriginReconcile {
    let spec = build_firewall_spec(None, cluster, cf_ips);
    let rules: Vec<FirewallRule> = spec
        .rules
        .iter()
        .map(cli_providers::rule_spec_to_wire)
        .collect();
    let lines = if cf_ips.is_some() {
        vec![
            format!("✓ Cloudflare origin firewall enabled on {fw_name} (80/443 restricted to Cloudflare IP ranges)."),
            "  Point DNS through Cloudflare (orange-cloud + SSL/TLS Full (strict)) — direct-to-node is now blocked.".to_string(),
        ]
    } else {
        vec![format!(
            "✓ Cloudflare origin firewall disabled on {fw_name} (80/443 open to the internet again)."
        )]
    };
    OriginReconcile { rules, lines }
}

/// Record the toggle on a target's config WITHOUT disturbing anything else on
/// it — region, server type, cluster name and ssh key all have to survive, or
/// flipping the firewall would quietly reset the target.
pub(crate) fn with_cloudflare_origin(mut config: TargetConfig, enable: bool) -> TargetConfig {
    config.firewall = Some(FirewallConfig {
        cloudflare_origin: enable,
    });
    config
}

/// Resolve which firewall id to rewrite, listing only when we have to.
///
/// `list` is invoked lazily: a cached id from the state file short-circuits the
/// API round-trip entirely, which also means a stale/oversized Hetzner project
/// listing can never shadow the id `apply` recorded.
pub(crate) fn resolve_firewall_id(
    cached: Option<u64>,
    fw_name: &str,
    list: &mut dyn FnMut() -> Result<Vec<Firewall>>,
) -> Result<Option<u64>> {
    match cached {
        Some(id) => Ok(Some(id)),
        None => Ok(find_owned_firewall_id(&list()?, fw_name)),
    }
}

/// Decide whether the Cloudflare ranges need fetching at all.
///
/// `fetch` runs ONLY when enabling. Disabling must stay reachable while
/// Cloudflare's endpoint is unreachable — that is precisely the situation in
/// which an operator needs to re-open 80/443 on the node.
pub(crate) fn cf_ips_for(
    enable: bool,
    fetch: &mut dyn FnMut() -> Result<Vec<String>>,
) -> Result<Option<Vec<String>>> {
    if enable {
        Ok(Some(fetch()?))
    } else {
        Ok(None)
    }
}

/// Warning for the "toggle saved, but there is nothing live to reconcile yet"
/// case. It must name the cluster we looked for AND say the intent survived,
/// otherwise the operator reasonably assumes the command did nothing at all.
pub(crate) fn no_firewall_warning(cluster: &str) -> String {
    format!(
        "no firewall found for '{cluster}' — the toggle is saved and will apply on the next \
         `apprafter up` / `apprafter apply`."
    )
}

/// The merge-patch body that records the toggle in `PlatformStack/default`.
///
/// A merge patch, not a server-side apply, for the same reason `target domain
/// add` uses one: it names exactly `spec.firewall.cloudflareOrigin` and nothing
/// else, so it can neither prune a sibling nor claim ownership of a field it
/// did not set. `spec.firewall` need not exist — a merge patch creates the
/// intermediate object.
pub(crate) fn origin_firewall_patch(enable: bool) -> Value {
    json!({ "spec": { "firewall": { "cloudflareOrigin": enable } } })
}

/// Read the recorded origin-firewall intent out of a `PlatformStack` CR —
/// live, or replayed from a snapshot.
///
/// Three-valued, and the third value is the point. `None` is UNKNOWN: a CR
/// written before this field existed carries nothing, and so does the snapshot
/// of one. Collapsing that into `false` would have a restore tell an operator
/// the source cluster served its 80/443 wide open when the cluster simply
/// never recorded an answer.
pub(crate) fn recorded_origin_firewall(stack: &Value) -> Option<bool> {
    stack
        .pointer("/spec/firewall/cloudflareOrigin")
        .and_then(Value::as_bool)
}

/// Should [`apply_cloudflare_origin`] also record the intent in the cluster?
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ClusterRecord {
    /// The operator ran the toggle — the CR is where the intent has to land if
    /// any backup is ever to carry it.
    Write,
    /// The caller already has the intent FROM the cluster (a restore replaying
    /// a captured `PlatformStack`), so writing it back would be a round trip
    /// that can only fail — and a failure there would be reported as if the
    /// intent had not been recorded, when it demonstrably has.
    Skip,
}

/// What [`apply_cloudflare_origin`] did, for a caller that reports rather than
/// prints.
pub(crate) struct OriginApply {
    /// The target the toggle was written to — RESOLVED, so a caller reporting
    /// it names the target that changed rather than the argument it passed.
    pub target: String,
    /// The confirmation lines for a reconcile that landed.
    pub lines: Vec<String>,
    /// Set when the intent was SAVED but the live firewall was not touched —
    /// there is no firewall for the cluster yet. The toggle still applies on
    /// the next `apprafter up` / `apprafter apply`, which is what the warning
    /// says.
    pub warning: Option<String>,
    /// Set when the local write succeeded but the CLUSTER did not record the
    /// intent. The live firewall is still correct; what is lost is the trail a
    /// future backup would follow.
    pub cluster_warning: Option<String>,
}

/// Warning for "the target records it, the cluster does not".
///
/// It must not read as "the command failed": the node's firewall IS reconciled
/// from the local store, so this cluster is exactly as restricted as asked.
/// What is missing is the record a backup would carry, and that only shows up
/// much later, in a restore — so the consequence has to be spelled out here.
pub(crate) fn cluster_record_warning(err: &str) -> String {
    format!(
        "the toggle is saved for this target and the node's firewall is reconciled, but the \
         cluster's PlatformStack could not record it ({err}). A backup taken before it does \
         carries no origin-firewall intent, so a restore from that snapshot would bring the node \
         up with 80/443 open to the internet. Re-run this command once the cluster is reachable."
    )
}

/// Run the cluster write iff [`ClusterRecord::Write`], and turn a failure into
/// the warning rather than an error. Pure decision, `write` injected — the same
/// shape as [`cf_ips_for`], and for the same reason: the interesting half is
/// WHETHER the call happens.
///
/// `Skip` must not merely ignore the result, it must not CALL: the restore
/// carries a value it read out of the cluster a moment ago, and a round trip
/// there could only produce a warning contradicting a record that exists.
pub(crate) fn cluster_record_note(
    record: ClusterRecord,
    write: &mut dyn FnMut() -> Result<()>,
) -> Option<String> {
    match record {
        ClusterRecord::Skip => None,
        ClusterRecord::Write => write()
            .err()
            .map(|e| cluster_record_warning(&format!("{e}"))),
    }
}

/// Record the intent in `PlatformStack/default` on `target_override`'s cluster.
fn record_origin_firewall_in_cluster(target_override: Option<&str>, enable: bool) -> Result<()> {
    let kc = ensure_kubeconfig_tempfile_for_target(target_override)?;
    merge_patch_origin_firewall(enable, kc.path())
}

fn merge_patch_origin_firewall(enable: bool, kc: &Path) -> Result<()> {
    let body = serde_json::to_string(&origin_firewall_patch(enable))
        .map_err(|e| cli_core::CliError::Other(format!("serialize patch: {e}")))?;
    kubectl_merge_patch(
        "platformstack",
        PLATFORMSTACK_NAME,
        Some(PLATFORMSTACK_NAMESPACE),
        None,
        &body,
        kc,
    )
}

/// Persist the origin-firewall toggle on `target_override` (the active target
/// when `None`), record it in that cluster's `PlatformStack`, and reconcile
/// that target's live Hetzner firewall.
///
/// Target-scoped rather than active-target-only because `restore --reprovision
/// --target <new>` carries the source cluster's toggle onto the target it just
/// provisioned (A4), and that target is frequently NOT the active one —
/// writing the active target's firewall from a restore aimed elsewhere is
/// exactly the class of bug C1 was.
///
/// ORDER IS LOAD-BEARING: local store, then cluster, then cloud. The two
/// records of the intent are written before anything that can fail on the
/// network, so an operator whose Cloudflare fetch or Hetzner call dies still
/// has a cluster that remembers what was asked for — and a backup that carries
/// it.
pub(crate) fn apply_cloudflare_origin(
    target_override: Option<&str>,
    enable: bool,
    record: ClusterRecord,
) -> Result<OriginApply> {
    let resolved = resolve_state_paths(target_override)?;
    let store = resolved.store;

    // 1. Persist the toggle FIRST (intent survives even if the live reconcile
    //    can't run / the CF fetch fails).
    let mut target = load_target(&store, &resolved.target_name)?;
    target.config = with_cloudflare_origin(target.config, enable);
    save_target(&store, &target)?;

    // 1b. Record it in the cluster, where a backup can see it. Best-effort:
    //     the toggle is legitimately usable before there is a cluster at all
    //     (`target add` then `target firewall … enable` then `apprafter up`),
    //     so an unreachable apiserver must not fail the command — but it is
    //     reported, because the missing record is invisible until a restore.
    let cluster_warning = cluster_record_note(record, &mut || {
        record_origin_firewall_in_cluster(target_override, enable)
    });

    // 2. Resolve cluster + token + state.
    let state = State::load_or_default(&resolved.paths)?;
    let cluster = resolve_cluster_name(
        state.cluster_name.as_deref(),
        target.config.cluster_name.as_deref(),
    );
    let token = cli_core::resolve_hetzner_token(None, &store, target_override)?;
    let client = HetznerCloudClient::new(hcloud_base_url(), token);

    // 3. Find the live firewall (cached id, else list+label+name).
    let fw_name = firewall_name(&cluster);
    let cached_id = state.hetzner_cloud.as_ref().and_then(|h| h.firewall_id);
    let firewall_id = resolve_firewall_id(cached_id, &fw_name, &mut || {
        Ok(client.list_firewalls()?.firewalls)
    })?;
    let Some(firewall_id) = firewall_id else {
        return Ok(OriginApply {
            target: resolved.target_name,
            lines: Vec::new(),
            warning: Some(no_firewall_warning(&cluster)),
            cluster_warning,
        });
    };

    // 4. Reconcile the live firewall (reuse 1.83d).
    let cf_ips = cf_ips_for(enable, &mut || {
        cli_providers::fetch_cloudflare_ips(&cli_providers::UreqCloudflareIpSource)
    })?;
    let plan = plan_origin_reconcile(&cluster, cf_ips.as_deref(), &fw_name);
    client.set_firewall_rules(firewall_id, &plan.rules)?;

    Ok(OriginApply {
        target: resolved.target_name,
        lines: plan.lines,
        warning: None,
        cluster_warning,
    })
}

fn run_cloudflare_origin(enable: bool) -> Result<()> {
    let applied = apply_cloudflare_origin(None, enable, ClusterRecord::Write)?;
    // Printed before the outcome of the live reconcile, and printed on BOTH
    // branches: it is the half of the write that a later restore depends on,
    // and the "no firewall yet" branch below returns early.
    if let Some(warning) = &applied.cluster_warning {
        eprintln!("{}", style::warn(warning));
    }
    if let Some(warning) = &applied.warning {
        eprintln!("{}", style::warn(warning));
        return Ok(());
    }
    for line in &applied.lines {
        println!("{line}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    /// Sources every default ingress rule carries when the origin firewall is
    /// OFF — spelled out rather than imported so a silent widening/narrowing of
    /// the default in `firewall_spec` has to be acknowledged here too.
    const OPEN: [&str; 2] = ["0.0.0.0/0", "::/0"];

    fn cf_ranges() -> Vec<String> {
        vec!["173.245.48.0/20".to_string(), "2400:cb00::/32".to_string()]
    }

    fn firewall(id: u64, name: &str, labels: &[(&str, &str)]) -> Firewall {
        Firewall {
            id,
            name: name.to_string(),
            rules: vec![],
            labels: labels
                .iter()
                .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
                .collect::<BTreeMap<_, _>>(),
        }
    }

    fn owned(id: u64, name: &str) -> Firewall {
        firewall(id, name, &[(APPRAFTER_LABEL, APPRAFTER_LABEL_VALUE)])
    }

    fn sources_for(rules: &[FirewallRule], port: &str, protocol: &str) -> Vec<String> {
        rules
            .iter()
            .find(|r| r.port.as_deref() == Some(port) && r.protocol == protocol)
            .unwrap_or_else(|| panic!("expected a {protocol}/{port} rule in the desired set"))
            .source_ips
            .clone()
    }

    // ── resolve_cluster_name ─────────────────────────────────────────────

    /// The recorded fact beats the stored preference. If this inverts, an
    /// operator who edited `cluster_name` after provisioning would have the
    /// toggle rewrite a DIFFERENT cluster's firewall (or none at all).
    #[test]
    fn state_cluster_outranks_the_target_preference() {
        assert_eq!(
            resolve_cluster_name(Some("live-1"), Some("pref-1")),
            "live-1"
        );
    }

    #[test]
    fn target_preference_is_used_before_the_first_apply() {
        assert_eq!(resolve_cluster_name(None, Some("pref-1")), "pref-1");
        assert_eq!(resolve_cluster_name(None, None), "platform-1");
    }

    // ── firewall_name ────────────────────────────────────────────────────

    /// The name we LOOK UP has to be the name `apply` CREATES; they are built
    /// in two different modules, so pin them against each other rather than
    /// against a literal only.
    #[test]
    fn lookup_name_matches_the_name_apply_provisions() {
        assert_eq!(firewall_name("demo"), "demo-fw");
        assert_eq!(
            firewall_name("demo"),
            build_firewall_spec(None, "demo", None).name
        );
    }

    // ── find_owned_firewall_id ───────────────────────────────────────────

    #[test]
    fn owned_firewall_is_found_by_name() {
        let list = vec![owned(7, "other-fw"), owned(9, "demo-fw")];
        assert_eq!(find_owned_firewall_id(&list, "demo-fw"), Some(9));
    }

    /// Safety net: `set_firewall_rules` REPLACES the whole rule set, so a
    /// same-named firewall that AppRafter does not own must never be selected
    /// — we would silently wipe a user's unrelated rules.
    #[test]
    fn an_unlabelled_namesake_is_never_selected() {
        let foreign = firewall(9, "demo-fw", &[("team", "infra")]);
        assert_eq!(find_owned_firewall_id(&[foreign], "demo-fw"), None);

        let wrong_value = firewall(9, "demo-fw", &[(APPRAFTER_LABEL, "false")]);
        assert_eq!(find_owned_firewall_id(&[wrong_value], "demo-fw"), None);
    }

    /// An owned firewall and a foreign namesake can coexist in one project.
    /// The owned one must win no matter which the API lists first.
    #[test]
    fn the_owned_namesake_wins_over_a_foreign_one_in_either_order() {
        let foreign = firewall(1, "demo-fw", &[("team", "infra")]);
        let ours = owned(2, "demo-fw");
        assert_eq!(
            find_owned_firewall_id(&[foreign.clone(), ours.clone()], "demo-fw"),
            Some(2)
        );
        assert_eq!(find_owned_firewall_id(&[ours, foreign], "demo-fw"), Some(2));
    }

    #[test]
    fn no_match_at_all_yields_none() {
        assert_eq!(
            find_owned_firewall_id(&[owned(1, "prod-fw")], "demo-fw"),
            None
        );
        assert_eq!(find_owned_firewall_id(&[], "demo-fw"), None);
    }

    // ── resolve_firewall_id ──────────────────────────────────────────────

    /// A cached id short-circuits the listing entirely. If the fallback ever
    /// ran unconditionally, a project with a same-named firewall could shadow
    /// the id `apply` actually recorded.
    #[test]
    fn a_cached_id_wins_and_the_listing_is_never_fetched() {
        let mut listed = false;
        let id = resolve_firewall_id(Some(42), "demo-fw", &mut || {
            listed = true;
            Ok(vec![owned(99, "demo-fw")])
        })
        .unwrap();
        assert_eq!(id, Some(42));
        assert!(!listed, "the cached id must not trigger an API listing");
    }

    #[test]
    fn without_a_cached_id_the_listing_decides() {
        let id =
            resolve_firewall_id(None, "demo-fw", &mut || Ok(vec![owned(99, "demo-fw")])).unwrap();
        assert_eq!(id, Some(99));
    }

    /// A failed listing must surface, not be swallowed into "no firewall
    /// found" — the two lead the operator to opposite conclusions.
    #[test]
    fn a_listing_failure_propagates() {
        let err = resolve_firewall_id(None, "demo-fw", &mut || {
            Err(cli_core::CliError::Other("api down".to_string()))
        })
        .expect_err("listing errors must not be swallowed");
        assert!(format!("{err}").contains("api down"), "{err}");
    }

    // ── cf_ips_for ───────────────────────────────────────────────────────

    /// Disabling must not depend on Cloudflare being reachable — that is
    /// exactly the outage in which an operator needs 80/443 re-opened.
    #[test]
    fn disabling_never_calls_out_to_cloudflare() {
        let mut fetched = false;
        let out = cf_ips_for(false, &mut || {
            fetched = true;
            Ok(vec!["1.1.1.1/32".to_string()])
        })
        .unwrap();
        assert_eq!(out, None);
        assert!(!fetched, "disabling must not fetch the Cloudflare ranges");
    }

    #[test]
    fn enabling_fetches_and_forwards_the_ranges() {
        let out = cf_ips_for(true, &mut || Ok(vec!["1.1.1.1/32".to_string()])).unwrap();
        assert_eq!(out, Some(vec!["1.1.1.1/32".to_string()]));
    }

    // ── with_cloudflare_origin ───────────────────────────────────────────

    /// Flipping the toggle rewrites ONE field. Everything else on the target
    /// (region, server type, cluster, ssh key) has to come through untouched.
    #[test]
    fn the_toggle_leaves_the_rest_of_the_target_config_alone() {
        let before = TargetConfig {
            provider: "hetzner-cloud".to_string(),
            region: Some("hel1".to_string()),
            server_type: Some("cx32".to_string()),
            cluster_name: Some("platform-7".to_string()),
            ..TargetConfig::default()
        };
        let after = with_cloudflare_origin(before.clone(), true);
        assert_eq!(
            after.firewall,
            Some(FirewallConfig {
                cloudflare_origin: true
            })
        );
        assert_eq!(
            TargetConfig {
                firewall: before.firewall.clone(),
                ..after.clone()
            },
            before,
            "no field other than `firewall` may change"
        );
        assert_eq!(
            with_cloudflare_origin(before, false).firewall,
            Some(FirewallConfig {
                cloudflare_origin: false
            })
        );
    }

    // ── plan_origin_reconcile ────────────────────────────────────────────

    /// Enabling narrows tcp/80 + tcp/443 to Cloudflare and NOTHING else. 22
    /// (ssh) and 6443 (kube apiserver) must stay reachable — Cloudflare does
    /// not proxy either, so gating them would lock the operator out.
    #[test]
    fn enabling_narrows_only_http_and_https() {
        let cf = cf_ranges();
        let plan = plan_origin_reconcile("demo", Some(&cf), "demo-fw");
        assert_eq!(sources_for(&plan.rules, "80", "tcp"), cf);
        assert_eq!(sources_for(&plan.rules, "443", "tcp"), cf);
        assert_eq!(sources_for(&plan.rules, "22", "tcp"), OPEN);
        assert_eq!(sources_for(&plan.rules, "6443", "tcp"), OPEN);
    }

    /// Disabling pushes the COMPLETE open set — `set_firewall_rules` replaces
    /// atomically, so a plan that omitted a port would delete that rule
    /// outright rather than leave it alone.
    #[test]
    fn disabling_restores_every_default_rule_wide_open() {
        let plan = plan_origin_reconcile("demo", None, "demo-fw");
        for port in ["22", "6443", "80", "443"] {
            assert_eq!(sources_for(&plan.rules, port, "tcp"), OPEN, "tcp/{port}");
        }
        assert_eq!(sources_for(&plan.rules, "51820", "udp"), OPEN);
        assert!(
            plan.rules.iter().any(|r| r.protocol == "icmp"),
            "the ICMP rule must survive the rewrite or PMTU discovery breaks"
        );
    }

    /// Cloudflare proxies neither UDP nor ICMP, so the WireGuard port and the
    /// ICMP rule have to keep their wide-open sources when the origin firewall
    /// goes on — narrowing them would silently break the node mesh and Path
    /// MTU Discovery while looking like a successful hardening.
    #[test]
    fn enabling_leaves_wireguard_and_icmp_wide_open() {
        let cf = cf_ranges();
        let plan = plan_origin_reconcile("demo", Some(&cf), "demo-fw");
        assert_eq!(sources_for(&plan.rules, "51820", "udp"), OPEN);
        let icmp = plan
            .rules
            .iter()
            .find(|r| r.protocol == "icmp")
            .expect("the ICMP rule must survive an enable");
        assert_eq!(icmp.source_ips, OPEN);
        assert!(
            icmp.port.is_none(),
            "Hetzner rejects an ICMP rule that carries an L4 port"
        );
    }

    /// The toggle rewrites SOURCES only. Because `set_firewall_rules` replaces
    /// the rule set wholesale, a plan that added or dropped a rule on one side
    /// of the toggle would open or close a port as an invisible side effect.
    #[test]
    fn the_toggle_rewrites_sources_and_never_the_rule_set() {
        let cf = cf_ranges();
        let on = plan_origin_reconcile("demo", Some(&cf), "demo-fw").rules;
        let off = plan_origin_reconcile("demo", None, "demo-fw").rules;

        let identity = |rs: &[FirewallRule]| {
            let mut v: Vec<(String, String, Option<String>)> = rs
                .iter()
                .map(|r| (r.direction.clone(), r.protocol.clone(), r.port.clone()))
                .collect();
            v.sort();
            v
        };
        assert_eq!(
            identity(&on),
            identity(&off),
            "enabling/disabling must not add or drop a rule"
        );

        let moved: Vec<(String, Option<String>)> = on
            .iter()
            .zip(off.iter())
            .filter(|(a, b)| a.source_ips != b.source_ips)
            .map(|(a, _)| (a.protocol.clone(), a.port.clone()))
            .collect();
        assert_eq!(
            moved,
            vec![
                ("tcp".to_string(), Some("80".to_string())),
                ("tcp".to_string(), Some("443".to_string())),
            ],
            "exactly tcp/80 and tcp/443 may change sources"
        );
    }

    /// The ranges reach Hetzner verbatim — no reordering, no dedup, no
    /// silent truncation of the v6 half of the Cloudflare list.
    #[test]
    fn the_cloudflare_ranges_are_forwarded_unchanged() {
        let cf = cf_ranges();
        let plan = plan_origin_reconcile("demo", Some(&cf), "demo-fw");
        assert_eq!(sources_for(&plan.rules, "443", "tcp"), cf);
        assert_eq!(
            sources_for(&plan.rules, "443", "tcp").len(),
            2,
            "both the v4 and the v6 range must survive"
        );
    }

    /// The confirmation is derived from the same decision as the rules, so it
    /// can never tell the operator the opposite of what was pushed. The
    /// expectation here is read OUT OF THE RULES, not out of the input flag.
    #[test]
    fn the_confirmation_never_contradicts_the_pushed_rules() {
        let cf = cf_ranges();
        for cf_ips in [None, Some(&cf[..])] {
            let plan = plan_origin_reconcile("demo", cf_ips, "demo-fw");
            let https_is_open = sources_for(&plan.rules, "443", "tcp") == OPEN;
            let text = plan.lines.join("\n");

            let says_enabled = text.contains("firewall enabled");
            let says_disabled = text.contains("firewall disabled");
            assert!(
                says_enabled ^ says_disabled,
                "the confirmation must claim exactly one state: {text}"
            );
            assert_eq!(
                says_disabled, https_is_open,
                "message and rules disagree about tcp/443: {text}"
            );
            assert!(
                text.contains("demo-fw"),
                "the confirmation must name the firewall it changed: {text}"
            );
        }
    }

    // ── origin_firewall_patch / recorded_origin_firewall ─────────────────

    /// The patch names ONE path. A body that carried a whole `spec` would
    /// wipe `values.tier` and the rest of the singleton on a merge patch —
    /// the same reason `target domain add` scopes its patch to the exact
    /// `allowedDomains` path.
    #[test]
    fn the_cluster_record_patches_exactly_one_field() {
        let on = origin_firewall_patch(true);
        assert_eq!(
            on,
            json!({"spec": {"firewall": {"cloudflareOrigin": true}}})
        );
        assert_eq!(
            on.pointer("/spec")
                .and_then(Value::as_object)
                .unwrap()
                .len(),
            1,
            "the patch must touch nothing but `firewall`"
        );
        assert_eq!(
            origin_firewall_patch(false),
            json!({"spec": {"firewall": {"cloudflareOrigin": false}}}),
            "disable records the disable — it does not delete the record"
        );
    }

    /// Round trip: what the toggle writes is what a reader reads back. The
    /// two halves live in one module precisely so a renamed key cannot drift
    /// between them, and this pins it.
    #[test]
    fn what_the_patch_writes_is_what_a_reader_reads() {
        for enable in [true, false] {
            assert_eq!(
                recorded_origin_firewall(&origin_firewall_patch(enable)),
                Some(enable)
            );
        }
    }

    /// ABSENCE IS UNKNOWN, NOT OFF. A `PlatformStack` from before this field
    /// existed — every cluster running today — carries nothing, and so does
    /// its snapshot. A reader that answered `Some(false)` there would have a
    /// restore state that the source cluster served its 80/443 wide open,
    /// which nobody ever established.
    #[test]
    fn an_unrecorded_origin_firewall_reads_as_unknown_not_as_off() {
        let no_field = json!({"kind": "PlatformStack", "spec": {"values": {"tier": 1}}});
        assert_eq!(recorded_origin_firewall(&no_field), None);

        let no_spec = json!({"kind": "PlatformStack"});
        assert_eq!(recorded_origin_firewall(&no_spec), None);

        let empty_block = json!({"spec": {"firewall": {}}});
        assert_eq!(recorded_origin_firewall(&empty_block), None);

        // …and a recorded OFF is NOT unknown: the operator said something.
        let off = json!({"spec": {"firewall": {"cloudflareOrigin": false}}});
        assert_eq!(recorded_origin_firewall(&off), Some(false));
    }

    /// A non-boolean value (hand-edited CR, a future shape) reads as unknown
    /// rather than as truthy. Guessing `true` would claim a restriction that
    /// may not exist.
    #[test]
    fn a_non_boolean_record_reads_as_unknown() {
        let weird = json!({"spec": {"firewall": {"cloudflareOrigin": "yes"}}});
        assert_eq!(recorded_origin_firewall(&weird), None);
    }

    // ── cluster_record_note ──────────────────────────────────────────────

    /// FIRES: the operator's own toggle writes the CR, and a failure there is
    /// reported rather than raised — the toggle is legitimately usable before
    /// a cluster exists, so an unreachable apiserver must not fail the
    /// command.
    #[test]
    fn the_operators_toggle_writes_the_cluster_record() {
        let mut called = 0;
        let note = cluster_record_note(ClusterRecord::Write, &mut || {
            called += 1;
            Ok(())
        });
        assert_eq!(
            called, 1,
            "the CR write is what puts the intent in a backup"
        );
        assert_eq!(note, None, "a write that landed says nothing");

        let mut failed = 0;
        let note = cluster_record_note(ClusterRecord::Write, &mut || {
            failed += 1;
            Err(cli_core::CliError::Other("connection refused".into()))
        });
        assert_eq!(failed, 1);
        assert!(
            note.expect("a failed cluster write must be reported")
                .contains("connection refused"),
            "the operator has to see WHY, to know whether re-running helps"
        );
    }

    /// DOES NOT FIRE: a restore carrying an intent it just read out of the
    /// cluster must not write it back. It is not that the result is ignored —
    /// the call must not HAPPEN, because its only possible contribution is a
    /// warning that the CR does not record something it demonstrably does.
    #[test]
    fn a_restore_carry_never_writes_the_cluster_record_back() {
        let mut called = false;
        let note = cluster_record_note(ClusterRecord::Skip, &mut || {
            called = true;
            Err(cli_core::CliError::Other("never reached".into()))
        });
        assert!(!called, "Skip must not round-trip to the apiserver");
        assert_eq!(note, None);
    }

    // ── cluster_record_warning ───────────────────────────────────────────

    /// The cluster-record failure is NOT a command failure: the node's
    /// firewall was still reconciled from the local store. The warning has to
    /// say what is actually lost — the record a future backup would carry —
    /// or an operator reads it as noise and moves on.
    #[test]
    fn the_cluster_record_warning_names_the_consequence_not_just_the_error() {
        let w = cluster_record_warning("connection refused");
        assert!(w.contains("connection refused"), "{w}");
        assert!(w.contains("saved"), "{w}");
        assert!(
            w.contains("backup"),
            "the loss is a backup that carries no intent: {w}"
        );
        assert!(
            w.contains("restore"),
            "…which only surfaces at restore time: {w}"
        );
    }

    // ── no_firewall_warning ──────────────────────────────────────────────

    /// Nothing was reconciled, so the warning has to say the intent PERSISTED
    /// and when it takes effect — otherwise the operator assumes the command
    /// was a no-op and re-runs it forever.
    #[test]
    fn the_missing_firewall_warning_names_the_cluster_and_the_next_step() {
        let w = no_firewall_warning("demo");
        assert!(w.contains("demo"), "{w}");
        assert!(w.contains("saved"), "{w}");
        assert!(w.contains("apprafter apply"), "{w}");
    }
}
