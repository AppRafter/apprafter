// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Pure Hetzner firewall-rule builders, shared by `apply` and `target firewall`
//! (1.83h extracted them from `apply.rs`). `build_firewall_spec(None, cluster,
//! cf_ips)` yields the default ingress rules with the Cloudflare origin source
//! applied to tcp/80+443 when `cf_ips` is `Some` (1.83d).

use cli_core::manifest::{FirewallIngressRule, InfrastructureManifest};
use cli_providers::hetzner_cloud::{FirewallRuleSpec, FirewallSpec};

pub(crate) const DEFAULT_INGRESS_PORTS_TCP: &[&str] = &["22", "6443", "80", "443"];
pub(crate) const DEFAULT_INGRESS_PORTS_UDP: &[&str] = &["51820"];
pub(crate) const DEFAULT_INGRESS_SOURCES: &[&str] = &["0.0.0.0/0", "::/0"];

pub(crate) fn build_firewall_spec(
    manifest: Option<&InfrastructureManifest>,
    cluster: &str,
    cf_ips: Option<&[String]>,
) -> FirewallSpec {
    let mut rules = manifest
        .and_then(|m| m.spec.firewall.as_ref())
        .and_then(|f| f.ingress.as_ref())
        .map(|ingress| ingress.iter().map(rule_from_manifest).collect::<Vec<_>>())
        .unwrap_or_else(default_ingress_rules);
    if let Some(cf) = cf_ips {
        apply_cf_origin(&mut rules, cf);
    }
    FirewallSpec {
        name: format!("{cluster}-fw"),
        rules,
    }
}

/// 1.83d: rewrite the `tcp/80` and `tcp/443` ingress rules' sources to the
/// Cloudflare set, leaving every other rule (22, 6443, 51820, ICMP) untouched.
/// `6443` is deliberately NOT gated — Cloudflare does not proxy the kube
/// apiserver, so CF-gating it would break `kubectl`. Pure — unit-tested.
pub(crate) fn apply_cf_origin(rules: &mut [FirewallRuleSpec], cf_ips: &[String]) {
    for r in rules.iter_mut() {
        let is_http = r.protocol == "tcp" && matches!(r.port.as_deref(), Some("80") | Some("443"));
        if is_http {
            r.source_ips = cf_ips.to_vec();
        }
    }
}

pub(crate) fn rule_from_manifest(r: &FirewallIngressRule) -> FirewallRuleSpec {
    FirewallRuleSpec {
        direction: "in".into(),
        port: Some(r.port.clone()),
        protocol: r.protocol.clone().unwrap_or_else(|| "tcp".into()),
        source_ips: r.source_ips.clone().unwrap_or_else(|| {
            DEFAULT_INGRESS_SOURCES
                .iter()
                .map(|s| s.to_string())
                .collect()
        }),
        destination_ips: vec![],
    }
}

pub(crate) fn default_ingress_rules() -> Vec<FirewallRuleSpec> {
    let sources: Vec<String> = DEFAULT_INGRESS_SOURCES
        .iter()
        .map(|s| s.to_string())
        .collect();
    let mut rules: Vec<FirewallRuleSpec> = DEFAULT_INGRESS_PORTS_TCP
        .iter()
        .map(|p| FirewallRuleSpec {
            direction: "in".into(),
            port: Some((*p).to_string()),
            protocol: "tcp".into(),
            source_ips: sources.clone(),
            destination_ips: vec![],
        })
        .collect();
    rules.extend(DEFAULT_INGRESS_PORTS_UDP.iter().map(|p| FirewallRuleSpec {
        direction: "in".into(),
        port: Some((*p).to_string()),
        protocol: "udp".into(),
        source_ips: sources.clone(),
        destination_ips: vec![],
    }));
    // ICMP — required for Path MTU Discovery (ICMP fragmentation
    // needed) and IPv6 NDP/RA. Hetzner Cloud Firewall does NOT
    // distinguish ICMPv4 from ICMPv6: a single `protocol: "icmp"`
    // rule with both v4 + v6 sources covers both families. ADR 0017
    // §Per-tier explicitly requires this to keep dual-stack pods
    // reachable. `port` is `None` because ICMP has no L4 port.
    rules.push(FirewallRuleSpec {
        direction: "in".into(),
        port: None,
        protocol: "icmp".into(),
        source_ips: sources.clone(),
        destination_ips: vec![],
    });
    rules
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn manifest_from(value: serde_json::Value) -> InfrastructureManifest {
        serde_json::from_value(value).expect("valid InfrastructureManifest JSON")
    }

    #[test]
    fn cf_origin_restricts_only_80_and_443() {
        let cf = vec!["173.245.48.0/20".to_string(), "2400:cb00::/32".to_string()];
        let rules = build_firewall_spec(None, "demo", Some(&cf)).rules;
        let src_for = |port: &str, proto: &str| -> Vec<String> {
            rules
                .iter()
                .find(|r| r.port.as_deref() == Some(port) && r.protocol == proto)
                .unwrap_or_else(|| panic!("rule {proto}/{port} present"))
                .source_ips
                .clone()
        };
        // 80 + 443 → CF set.
        assert_eq!(src_for("80", "tcp"), cf);
        assert_eq!(src_for("443", "tcp"), cf);
        // 22 + 6443 stay open (6443 can't be CF-gated — kubectl).
        assert_eq!(
            src_for("22", "tcp"),
            vec!["0.0.0.0/0".to_string(), "::/0".to_string()]
        );
        assert_eq!(
            src_for("6443", "tcp"),
            vec!["0.0.0.0/0".to_string(), "::/0".to_string()]
        );
    }

    #[test]
    fn no_cf_origin_leaves_all_rules_open() {
        let rules = build_firewall_spec(None, "demo", None).rules;
        for r in &rules {
            assert_eq!(
                r.source_ips,
                vec!["0.0.0.0/0".to_string(), "::/0".to_string()],
                "rule {}/{:?} should stay open without CF origin",
                r.protocol,
                r.port
            );
        }
    }

    #[test]
    fn build_firewall_spec_uses_default_ingress_when_absent() {
        let f = build_firewall_spec(None, "cl", None);
        assert_eq!(f.name, "cl-fw");
        // Defaults whitelist ssh + kube API + HTTP + HTTPS over tcp
        // and wireguard over udp; all rules are inbound from
        // 0.0.0.0/0 + ::/0.
        let ports: Vec<String> = f.rules.iter().filter_map(|r| r.port.clone()).collect();
        for expected in ["22", "6443", "80", "443", "51820"] {
            assert!(
                ports.iter().any(|p| p == expected),
                "missing port {expected} in {ports:?}"
            );
        }
        for r in &f.rules {
            assert_eq!(r.direction, "in");
            assert_eq!(r.source_ips, vec!["0.0.0.0/0", "::/0"]);
        }
    }

    #[test]
    fn build_firewall_spec_uses_manifest_rules_when_present() {
        let m = manifest_from(json!({
            "apiVersion": "apprafter.io/v1alpha1",
            "kind": "Infrastructure",
            "metadata": {"name": "p"},
            "spec": {
                "provider": "hetzner-cloud",
                "firewall": {
                    "ingress": [
                        {"port": "8080", "protocol": "udp", "source_ips": ["10.0.0.0/8"]}
                    ]
                }
            }
        }));
        let f = build_firewall_spec(Some(&m), "p", None);
        assert_eq!(f.rules.len(), 1);
        assert_eq!(f.rules[0].port.as_deref(), Some("8080"));
        assert_eq!(f.rules[0].protocol, "udp");
        assert_eq!(f.rules[0].source_ips, vec!["10.0.0.0/8"]);
    }

    #[test]
    fn rule_from_manifest_defaults_protocol_to_tcp_and_global_sources() {
        let r = rule_from_manifest(&FirewallIngressRule {
            port: "9000".into(),
            protocol: None,
            source_ips: None,
        });
        assert_eq!(r.protocol, "tcp");
        assert_eq!(r.source_ips, vec!["0.0.0.0/0", "::/0"]);
    }

    #[test]
    fn default_ingress_rules_emits_one_rule_per_default_port_plus_icmp() {
        let r = default_ingress_rules();
        // TCP ports + UDP ports + 1 ICMP rule (ADR 0017 — dual-stack
        // PMTU + NDP).
        assert_eq!(
            r.len(),
            DEFAULT_INGRESS_PORTS_TCP.len() + DEFAULT_INGRESS_PORTS_UDP.len() + 1
        );
    }

    #[test]
    fn default_ingress_rules_now_include_kube_api_and_http_and_wireguard() {
        let r = default_ingress_rules();
        let ports: Vec<String> = r
            .iter()
            .map(|rule| {
                let p = rule.port.clone().unwrap_or_default();
                format!("{p}/{}", rule.protocol)
            })
            .collect();
        assert!(ports.contains(&"22/tcp".into()), "{ports:?}");
        assert!(ports.contains(&"6443/tcp".into()), "{ports:?}");
        assert!(ports.contains(&"80/tcp".into()), "{ports:?}");
        assert!(ports.contains(&"443/tcp".into()), "{ports:?}");
        assert!(ports.contains(&"51820/udp".into()), "{ports:?}");
    }

    #[test]
    fn default_ingress_rules_include_icmp_for_pmtu_and_ndp() {
        // ADR 0017 requires ICMP allowance for Path MTU Discovery
        // and IPv6 NDP/RA. Hetzner Cloud Firewall uses a single
        // `icmp` protocol that covers both ICMPv4 and ICMPv6.
        let r = default_ingress_rules();
        let icmp = r
            .iter()
            .find(|rule| rule.protocol == "icmp")
            .expect("default ingress must include an icmp rule for PMTU + NDP");
        assert_eq!(icmp.direction, "in");
        assert!(
            icmp.port.is_none(),
            "ICMP has no L4 port; Hetzner rejects ICMP rules that carry a port"
        );
        assert_eq!(icmp.source_ips, vec!["0.0.0.0/0", "::/0"]);
    }
}
