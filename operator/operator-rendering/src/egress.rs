// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Pure builder for the per-Application egress `CiliumNetworkPolicy`
//! (2.10 / ADR 0045).
//!
//! The renderer emits one CNP per Application that selects the app's
//! pods on egress (Cilium default-deny-on-select) and allows: DNS,
//! same-namespace + the external internet (profile-gated), and one
//! egress rule per declared **network** need (pg, redis). Everything
//! else in-cluster (cross-namespace, other apps, undeclared services)
//! is denied.
//!
//! Built with `serde_json::json!` (the external-CR pattern, ADR 0045
//! §F) — there is no hand-rolled CNP type. The controller resolves the
//! profile (from the singleton PlatformStack) and the connection-target
//! catalog (with namespace overrides from `ServiceProvider.spec.config`)
//! and threads both into this pure function; the renderer never reaches
//! into provisioner internals or runtime status.

use std::collections::BTreeMap;

use operator_core::{EgressProfile, NeedEntry};
use serde_json::{json, Value};

/// One in-cluster service an egress rule can target — a `(namespace,
/// pod_selector, port)` triple. The provisioner's defaults (cnpg
/// `platform-postgres` in `cnpg-system`, dragonfly in
/// `dragonfly-system`) are mirrored by [`default_target`]; the
/// controller may override the namespace from `ServiceProvider.spec.config`
/// before threading the catalog into [`render_egress_policy`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectionTarget {
    /// The namespace the service runs in (the CNP rule's
    /// `io.kubernetes.pod.namespace`).
    pub namespace: String,
    /// Service-level pod label selector (e.g.
    /// `{cnpg.io/cluster: platform-postgres}`). Targets the
    /// *service*, not the specific shared instance a claim landed on —
    /// per-instance data isolation is enforced at the app layer
    /// (2.4 per-role pg, 2.6 redis `$N` ACL).
    pub pod_selector: BTreeMap<String, String>,
    /// The TCP port the service listens on.
    pub port: u16,
}

/// The static connection-target catalog (ADR 0045 §B). Maps a service
/// `type` to its in-cluster `(namespace, pod_selector, port)` default —
/// mirroring the provisioner defaults (`pg` → cnpg `platform-postgres`
/// in `cnpg-system`; `redis` → dragonfly in `dragonfly-system`).
/// Returns `None` for types with no network target (`disk`) or types
/// not in the launch slice. The controller starts from these defaults
/// and may override the namespace from `ServiceProvider.spec.config`.
pub fn default_target(service_type: &str) -> Option<ConnectionTarget> {
    match service_type {
        "pg" => Some(ConnectionTarget {
            namespace: "cnpg-system".to_string(),
            // CloudNativePG labels every cluster pod with `cnpg.io/cluster`
            // (NOT the deprecated `postgresql.cnpg.io/cluster`) — verified
            // against the live CNPG 1.29 pods the provisioner creates and the
            // selector `needs-pg-walk.sh` uses to reach the primary. Cilium's
            // socket-LB rewrites the `-rw` ClusterIP to the backend pod IP at
            // connect() time, so this rule MUST match the real pod identity or
            // every `needs.pg` app's egress to Postgres is silently dropped.
            pod_selector: BTreeMap::from([(
                "cnpg.io/cluster".to_string(),
                "platform-postgres".to_string(),
            )]),
            port: 5432,
        }),
        "redis" => Some(ConnectionTarget {
            namespace: "dragonfly-system".to_string(),
            pod_selector: BTreeMap::from([(
                "app.kubernetes.io/name".to_string(),
                "dragonfly".to_string(),
            )]),
            port: 6379,
        }),
        _ => None,
    }
}

/// Build the per-Application egress `CiliumNetworkPolicy` as a
/// `serde_json::Value` (ADR 0045 §A).
///
/// `name` is the Application's metadata name (only used in doc context
/// today); `rendered_name` is the env-aware child name (2.9) the CNP is
/// named after (`<rendered_name>-egress`) so per-env deployments never
/// collide. `labels` are the app's pod labels (the same ones
/// `make_labels` stamps) — they become the CNP `endpointSelector`,
/// making the app's pods egress default-deny.
///
/// `needs_entries` is the effective spec's flattened `needs`
/// ([`Needs::entries`]) — iterated in its deterministic order so the
/// emitted egress rule list is byte-stable across reconciles (SSA
/// no-op). Disk entries (`entry.disk.is_some()`) carry no network
/// target and are skipped; a service entry adds one egress rule iff
/// `targets` has a `ConnectionTarget` for its type.
///
/// `profile` gates the baseline rules: DNS is always emitted; the
/// same-namespace rule is emitted unless `Strict`; the `toEntities:
/// [world]` (external internet) rule is emitted only for `Internet`.
pub fn render_egress_policy(
    name: &str,
    rendered_name: &str,
    labels: &BTreeMap<String, String>,
    needs_entries: &[(String, NeedEntry)],
    profile: EgressProfile,
    targets: &BTreeMap<String, ConnectionTarget>,
) -> Value {
    let _ = name;
    let mut egress: Vec<Value> = Vec::new();

    // DNS → kube-dns in kube-system (always, every profile). UDP+TCP/53.
    egress.push(json!({
        "toEndpoints": [{
            "matchLabels": {
                "io.kubernetes.pod.namespace": "kube-system",
                "k8s-app": "kube-dns"
            }
        }],
        "toPorts": [{
            "ports": [
                { "port": "53", "protocol": "UDP" },
                { "port": "53", "protocol": "TCP" }
            ]
        }]
    }));

    // Same-namespace (profile internet|internal; NOT strict). An empty
    // matchLabels selects every endpoint in the policy's own namespace.
    if profile != EgressProfile::Strict {
        egress.push(json!({
            "toEndpoints": [{ "matchLabels": {} }]
        }));
    }

    // External internet (profile internet only). `world` matches only
    // IPs outside the cluster's known endpoints, so it does not re-open
    // cross-service in-cluster egress.
    if profile == EgressProfile::Internet {
        egress.push(json!({
            "toEntities": ["world"]
        }));
    }

    // One rule per declared network need (always, regardless of
    // profile). Iterate `needs_entries` (the deterministic
    // `Needs::entries` order) for byte-stable rule ordering. Disk
    // entries carry no network target → skipped; an unknown/absent
    // target type → skipped.
    for (service_type, entry) in needs_entries {
        if entry.disk.is_some() {
            continue;
        }
        let Some(target) = targets.get(service_type) else {
            continue;
        };
        let mut match_labels = serde_json::Map::new();
        match_labels.insert(
            "io.kubernetes.pod.namespace".to_string(),
            Value::String(target.namespace.clone()),
        );
        for (k, v) in &target.pod_selector {
            match_labels.insert(k.clone(), Value::String(v.clone()));
        }
        egress.push(json!({
            "toEndpoints": [{ "matchLabels": Value::Object(match_labels) }],
            "toPorts": [{
                "ports": [{ "port": target.port.to_string(), "protocol": "TCP" }]
            }]
        }));
    }

    json!({
        "apiVersion": "cilium.io/v2",
        "kind": "CiliumNetworkPolicy",
        "metadata": {
            "name": format!("{rendered_name}-egress"),
        },
        "spec": {
            "endpointSelector": {
                "matchLabels": labels,
            },
            "egress": egress,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use operator_core::{DiskClaim, ServiceNeed};

    fn app_labels() -> BTreeMap<String, String> {
        BTreeMap::from([
            ("app.kubernetes.io/name".to_string(), "web".to_string()),
            ("apprafter.io/application".to_string(), "web".to_string()),
        ])
    }

    /// The launch catalog (pg + redis) used as the threaded `targets`.
    fn launch_targets() -> BTreeMap<String, ConnectionTarget> {
        BTreeMap::from([
            ("pg".to_string(), default_target("pg").unwrap()),
            ("redis".to_string(), default_target("redis").unwrap()),
        ])
    }

    fn egress_rules(cnp: &Value) -> &Vec<Value> {
        cnp["spec"]["egress"].as_array().expect("egress array")
    }

    // ---- Step 1: catalog ----

    #[test]
    fn default_target_pg_redis_disk() {
        let pg = default_target("pg").expect("pg target");
        assert_eq!(pg.namespace, "cnpg-system");
        assert_eq!(pg.port, 5432);
        assert_eq!(
            pg.pod_selector.get("cnpg.io/cluster").map(String::as_str),
            Some("platform-postgres")
        );

        let redis = default_target("redis").expect("redis target");
        assert_eq!(redis.namespace, "dragonfly-system");
        assert_eq!(redis.port, 6379);
        assert_eq!(
            redis
                .pod_selector
                .get("app.kubernetes.io/name")
                .map(String::as_str),
            Some("dragonfly")
        );

        // disk + unknown have no network target.
        assert!(default_target("disk").is_none());
        assert!(default_target("clickhouse").is_none());
    }

    // ---- Step 2: per-profile baselines (no needs) ----

    #[test]
    fn internet_baseline_three_rules() {
        let labels = app_labels();
        let cnp = render_egress_policy(
            "web",
            "web",
            &labels,
            &[],
            EgressProfile::Internet,
            &launch_targets(),
        );
        assert_eq!(cnp["apiVersion"], "cilium.io/v2");
        assert_eq!(cnp["kind"], "CiliumNetworkPolicy");
        assert_eq!(cnp["metadata"]["name"], "web-egress");
        // endpointSelector selects the app pods (the app labels).
        assert_eq!(
            cnp["spec"]["endpointSelector"]["matchLabels"]["apprafter.io/application"],
            "web"
        );
        assert_eq!(
            cnp["spec"]["endpointSelector"]["matchLabels"]["app.kubernetes.io/name"],
            "web"
        );
        let rules = egress_rules(&cnp);
        // DNS + same-ns + world = 3.
        assert_eq!(rules.len(), 3, "internet baseline = DNS + same-ns + world");
        // First rule is DNS.
        assert_eq!(
            rules[0]["toEndpoints"][0]["matchLabels"]["k8s-app"],
            "kube-dns"
        );
        // Has a world rule.
        let has_world = rules.iter().any(|r| r["toEntities"] == json!(["world"]));
        assert!(has_world, "internet baseline has toEntities:[world]");
    }

    #[test]
    fn internal_baseline_two_rules_no_world() {
        let labels = app_labels();
        let cnp = render_egress_policy(
            "web",
            "web",
            &labels,
            &[],
            EgressProfile::Internal,
            &launch_targets(),
        );
        let rules = egress_rules(&cnp);
        // DNS + same-ns = 2, NO world.
        assert_eq!(rules.len(), 2, "internal baseline = DNS + same-ns");
        let has_world = rules.iter().any(|r| r.get("toEntities").is_some());
        assert!(!has_world, "internal baseline has no world rule");
        // Still has same-ns (empty matchLabels).
        let has_same_ns = rules
            .iter()
            .any(|r| r["toEndpoints"][0]["matchLabels"] == json!({}));
        assert!(has_same_ns, "internal baseline keeps same-ns");
    }

    #[test]
    fn strict_baseline_dns_only() {
        let labels = app_labels();
        let cnp = render_egress_policy(
            "web",
            "web",
            &labels,
            &[],
            EgressProfile::Strict,
            &launch_targets(),
        );
        let rules = egress_rules(&cnp);
        // DNS only.
        assert_eq!(rules.len(), 1, "strict baseline = DNS only");
        assert_eq!(
            rules[0]["toEndpoints"][0]["matchLabels"]["k8s-app"],
            "kube-dns"
        );
        // No world, no same-ns.
        let has_world = rules.iter().any(|r| r.get("toEntities").is_some());
        assert!(!has_world, "strict has no world rule");
        let has_same_ns = rules
            .iter()
            .any(|r| r["toEndpoints"][0]["matchLabels"] == json!({}));
        assert!(!has_same_ns, "strict has no same-ns rule");
    }

    // ---- Step 4: needs add rules, disk excluded ----

    #[test]
    fn pg_and_redis_needs_add_rules_disk_excluded() {
        let labels = app_labels();
        // Construct the effective needs entries from REAL shapes:
        // one pg service need, one redis service need, one disk entry.
        let needs_entries: Vec<(String, NeedEntry)> = vec![
            (
                "pg".to_string(),
                NeedEntry {
                    name: None,
                    service: Some(ServiceNeed::default()),
                    disk: None,
                },
            ),
            (
                "redis".to_string(),
                NeedEntry {
                    name: None,
                    service: Some(ServiceNeed::default()),
                    disk: None,
                },
            ),
            (
                "disk".to_string(),
                NeedEntry {
                    name: Some("data".to_string()),
                    service: None,
                    disk: Some(DiskClaim {
                        name: Some("data".to_string()),
                        size: "1Gi".to_string(),
                        mount_path: "/data".to_string(),
                        class: None,
                        read_only: None,
                    }),
                },
            ),
        ];
        let cnp = render_egress_policy(
            "web",
            "web",
            &labels,
            &needs_entries,
            EgressProfile::Internet,
            &launch_targets(),
        );
        let rules = egress_rules(&cnp);
        // 3 baseline (DNS + same-ns + world) + pg + redis = 5. Disk
        // contributes NO rule (no network target).
        assert_eq!(
            rules.len(),
            5,
            "internet baseline (3) + pg + redis; disk excluded"
        );

        // pg rule: cnpg-system, cluster selector, port 5432/TCP.
        let pg_rule = rules
            .iter()
            .find(|r| {
                r["toEndpoints"][0]["matchLabels"]["io.kubernetes.pod.namespace"] == "cnpg-system"
            })
            .expect("pg egress rule present");
        assert_eq!(
            pg_rule["toEndpoints"][0]["matchLabels"]["cnpg.io/cluster"],
            "platform-postgres"
        );
        assert_eq!(pg_rule["toPorts"][0]["ports"][0]["port"], "5432");
        assert_eq!(pg_rule["toPorts"][0]["ports"][0]["protocol"], "TCP");

        // redis rule: dragonfly-system, dragonfly selector, port 6379/TCP.
        let redis_rule = rules
            .iter()
            .find(|r| {
                r["toEndpoints"][0]["matchLabels"]["io.kubernetes.pod.namespace"]
                    == "dragonfly-system"
            })
            .expect("redis egress rule present");
        assert_eq!(
            redis_rule["toEndpoints"][0]["matchLabels"]["app.kubernetes.io/name"],
            "dragonfly"
        );
        assert_eq!(redis_rule["toPorts"][0]["ports"][0]["port"], "6379");

        // No egress rule targets the disk (no disk-derived endpoint).
        assert!(
            !rules
                .iter()
                .any(|r| r["toPorts"][0]["ports"][0]["port"] == "1Gi"),
            "disk contributes no egress rule"
        );
    }

    #[test]
    fn needs_rules_present_even_under_strict() {
        // Profile gates only the baseline; declared-need rules are
        // always emitted. strict + pg = DNS + pg = 2 rules.
        let labels = app_labels();
        let needs_entries: Vec<(String, NeedEntry)> = vec![(
            "pg".to_string(),
            NeedEntry {
                name: None,
                service: Some(ServiceNeed::default()),
                disk: None,
            },
        )];
        let cnp = render_egress_policy(
            "web",
            "web",
            &labels,
            &needs_entries,
            EgressProfile::Strict,
            &launch_targets(),
        );
        let rules = egress_rules(&cnp);
        assert_eq!(rules.len(), 2, "strict baseline (DNS) + pg need");
        assert!(rules.iter().any(|r| {
            r["toEndpoints"][0]["matchLabels"]["io.kubernetes.pod.namespace"] == "cnpg-system"
        }));
    }

    #[test]
    fn rendered_name_drives_cnp_name() {
        // The env-aware rendered child name (2.9) drives the CNP name,
        // so per-env deployments never collide.
        let labels = app_labels();
        let cnp = render_egress_policy(
            "web",
            "web-prod",
            &labels,
            &[],
            EgressProfile::Internet,
            &launch_targets(),
        );
        assert_eq!(cnp["metadata"]["name"], "web-prod-egress");
    }
}
