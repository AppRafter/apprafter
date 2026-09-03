// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! `apprafter target firewall cloudflare-origin enable|disable` (1.83h) —
//! manifest-free toggle of the Cloudflare origin firewall: persists the intent
//! in the target store AND immediately reconciles the live Hetzner firewall.

use cli_core::style;
use cli_core::target::{load_target, save_target, FirewallConfig, TargetConfig};
use cli_core::Result;
use cli_providers::hetzner_cloud::{
    Firewall, FirewallRule, APPRAFTER_LABEL, APPRAFTER_LABEL_VALUE,
};
use cli_providers::HetznerCloudClient;
use cli_state::State;

use crate::cli::{FirewallToggle, TargetFirewallCommand};
use crate::commands::firewall_spec::build_firewall_spec;
use crate::commands::hcloud::hcloud_base_url;
use crate::commands::state_paths::resolve_state_paths;

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

fn run_cloudflare_origin(enable: bool) -> Result<()> {
    let resolved = resolve_state_paths(None)?;
    let store = resolved.store;

    // 1. Persist the toggle FIRST (intent survives even if the live reconcile
    //    can't run / the CF fetch fails).
    let mut target = load_target(&store, &resolved.target_name)?;
    target.config = with_cloudflare_origin(target.config, enable);
    save_target(&store, &target)?;

    // 2. Resolve cluster + token + state.
    let state = State::load_or_default(&resolved.paths)?;
    let cluster = resolve_cluster_name(
        state.cluster_name.as_deref(),
        target.config.cluster_name.as_deref(),
    );
    let token = cli_core::resolve_hetzner_token(None, &store, None)?;
    let client = HetznerCloudClient::new(hcloud_base_url(), token);

    // 3. Find the live firewall (cached id, else list+label+name).
    let fw_name = firewall_name(&cluster);
    let cached_id = state.hetzner_cloud.as_ref().and_then(|h| h.firewall_id);
    let firewall_id = resolve_firewall_id(cached_id, &fw_name, &mut || {
        Ok(client.list_firewalls()?.firewalls)
    })?;
    let Some(firewall_id) = firewall_id else {
        eprintln!("{}", style::warn(&no_firewall_warning(&cluster)));
        return Ok(());
    };

    // 4. Reconcile the live firewall (reuse 1.83d).
    let cf_ips = cf_ips_for(enable, &mut || {
        cli_providers::fetch_cloudflare_ips(&cli_providers::UreqCloudflareIpSource)
    })?;
    let plan = plan_origin_reconcile(&cluster, cf_ips.as_deref(), &fw_name);
    client.set_firewall_rules(firewall_id, &plan.rules)?;

    for line in &plan.lines {
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
