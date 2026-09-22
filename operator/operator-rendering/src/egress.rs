// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Pure builder for the per-Application egress `CiliumNetworkPolicy`
//! (2.10 / ADR 0045).
//!
//! The renderer emits one CNP per Application that selects the app's
//! pods on egress (Cilium default-deny-on-select) and allows: DNS,
//! same-namespace + the external internet (profile-gated), and one
//! egress rule per declared **network** need (pg, redis, jetstream).
//! Everything else in-cluster (cross-namespace, other apps, undeclared
//! services) is denied.
//!
//! Because selection alone is what makes the pods default-deny, a need
//! type with no [`ConnectionTarget`] does not merely miss a rule — it is
//! locked out of its own backend. The census test at the bottom of this
//! file is what keeps that from happening silently again.
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
/// in `cnpg-system`; `redis` → dragonfly in `dragonfly-system`;
/// `jetstream` → the `nats` StatefulSet in `nats-system`).
/// Returns `None` for types with no network target (`disk`) or types
/// not in the launch slice. The controller starts from these defaults
/// and may override the namespace from `ServiceProvider.spec.config`.
///
/// **Adding a shipped need type means adding an arm here.** The `_` arm
/// below is silent by construction — a missing arm costs the new type its
/// egress, not a compile error — so the census test
/// (`every_need_type_has_a_target_or_is_listed_as_target_less`) exists to
/// make the omission loud. Verify a new arm's `pod_selector` against the
/// labels on a LIVE pod; a selector that renders but matches nothing is
/// indistinguishable from a correct one in every test that does not run
/// Cilium.
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
        // 2.5 (ADR 0061): the NATS server the `jetstream-integrated`
        // provider resolves to. `nats-system` matches BOTH
        // `component_nats.cue`'s `namespace` and the `jetstream-integrated`
        // seed's `config.namespace` (service_providers.cue) — the same
        // namespace the connection Secret's
        // `nats://nats.nats-system.svc:4222` URL names. 4222 is the client
        // port (8222 is monitoring; an application never needs it).
        //
        // The two labels are the VERSION-INDEPENDENT half of what the
        // upstream `nats` chart 2.14.6 stamps on its StatefulSet pod
        // template — read off a live `nats-0` in a throwaway kind cluster
        // (`app.kubernetes.io/component=nats`,
        // `app.kubernetes.io/instance=nats`, `app.kubernetes.io/name=nats`,
        // plus `app.kubernetes.io/managed-by`, `app.kubernetes.io/version`
        // and `helm.sh/chart`), not read off the chart's documentation.
        //
        // `version` and `helm.sh/chart` are deliberately EXCLUDED: both
        // carry the chart/appVersion, so selecting on either would silently
        // stop matching the moment `component_nats.cue` re-pins the chart —
        // a CNP that renders and matches nothing is this very defect again.
        // `instance` is excluded too: it is the Helm RELEASE name, which
        // here is whatever Argo CD names the component's Application
        // (`component_nats.cue` sets no `releaseName`), so pinning it would
        // couple an egress rule to a naming detail of the delivery layer.
        //
        // `component` is NOT redundant with `name`, which the same probe is
        // what showed: the chart's OWN `nats-box` and `test-request-reply`
        // pods also carry `app.kubernetes.io/name: nats` and differ from the
        // server ONLY by `component` (`nats-box` / `test-request-reply` vs
        // `nats`). Selecting on `name` alone would widen this rule to a
        // debug shell — narrow, but wider than "the server", and for no gain.
        // The pair matches exactly `pod/nats-0` (verified, same probe).
        //
        // Like the `pg` arm above, this rule MUST match the real pod
        // identity: Cilium's socket-LB rewrites the `nats` ClusterIP to a
        // backend pod IP at connect() time, so a selector that matches
        // nothing drops every `needs.jetstream` app's NATS traffic while the
        // app holds perfectly valid credentials. `e2e/needs-jetstream-walk.sh`
        // asserts this selector against the labels on the live `nats-0`.
        "jetstream" => Some(ConnectionTarget {
            namespace: "nats-system".to_string(),
            pod_selector: BTreeMap::from([
                ("app.kubernetes.io/name".to_string(), "nats".to_string()),
                (
                    "app.kubernetes.io/component".to_string(),
                    "nats".to_string(),
                ),
            ]),
            port: 4222,
        }),
        _ => None,
    }
}

/// Every `needs.<type>` key that legitimately has **no** connection target,
/// each with the reason it has none. Read by
/// `every_need_type_has_a_target_or_is_listed_as_target_less` (the test
/// below) as the allow-list half of the census; anything NOT listed here must
/// resolve through [`default_target`].
///
/// This exists because a catch-all `_ =>` arm silently swallowing a newly
/// shipped need type happened three times in one subphase: the `gc_backend`
/// CNPG mis-route, the `RetainedClaim` webhook's missing `nats` arm, and
/// [`default_target`]'s own missing `jetstream` arm — which handed
/// `needs.jetstream` apps working credentials for a server their own pods
/// could not open a socket to.
///
/// The first two were caught by a live walk that happened to exercise them.
/// The third was not caught by anything: the unit suite only ever asserted
/// the types that DO have arms, and the walk runs without Cilium, so no
/// policy is enforced there at all. A documentation pass found it. The census
/// turns the next omission into a `cargo test` failure rather than leaving it
/// to whether some walk's shape happens to cover it.
///
/// **Two of the exemptions mean different things, and the difference is
/// load-bearing** — hence [`WhyNoTarget`]. `disk` will never have a target.
/// `clickhouse`/`s3`/`notifications` have none only while nothing is running
/// for them to reach, so each of those is a claim about the tree's current
/// state that can quietly expire. `a_type_exempted_as_unshipped_is_still_unshipped`
/// checks that claim against `cli/docsgen/src/shipped.rs` — the repository's
/// authority on whether a `needs` key is provisionable — so the day one flips
/// to `Shipped`, the census fails here rather than a user's application
/// discovering it.
#[cfg(test)]
const NEED_TYPES_WITHOUT_A_NETWORK_TARGET: &[(&str, WhyNoTarget, &str)] = &[
    (
        "disk",
        WhyNoTarget::NothingToConnectTo,
        "a PVC mounted into the pod — there is no socket to open, now or ever",
    ),
    (
        "clickhouse",
        WhyNoTarget::NotShippedYet,
        "declared in the schema, no provisioner backend and no ServiceProvider seed \
         — nothing is running to reach",
    ),
    (
        "s3",
        WhyNoTarget::NotShippedYet,
        "declared in the schema, no provisioner backend and no ServiceProvider seed \
         — nothing is running to reach",
    ),
    (
        "notifications",
        WhyNoTarget::NotShippedYet,
        "declared in the schema, no provisioner backend and no ServiceProvider seed \
         — and unlike the other two, no providers/ directory exists at all",
    ),
];

/// Why a `needs.<type>` key has no [`ConnectionTarget`] — a permanent
/// property of the type, or a fact about what is shipped today.
#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WhyNoTarget {
    /// There is nothing to open a socket to, by the nature of the need.
    /// Nothing about shipping more of the platform changes this.
    NothingToConnectTo,
    /// The type is declared in the schema but has no provisioner backend, so
    /// there is no service in the cluster to reach. This exemption EXPIRES
    /// the moment the backend lands, and the test below is what notices.
    NotShippedYet,
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

    /// Every `needs.<type>` key the schema DECLARES, read off `Needs`'s OWN
    /// derived JSON schema rather than written out here.
    ///
    /// Declared, not shipped — the two differ (`clickhouse`/`s3`/
    /// `notifications` are declared with no backend behind them), and the
    /// census wants the wider set: a type must be classified the moment it
    /// can be written in a manifest, not the moment it starts working.
    ///
    /// The indirection is the point: `Needs` (operator-core) is the single
    /// place the key set is declared, and a new `pub` field on it appears in
    /// this list automatically. A hand-copied list in this file would go
    /// stale exactly the way the four hand-copied `PLATFORM_SERVICE_TYPES` /
    /// `BUILTIN_TYPES` mirrors in the webhook already warn about in their own
    /// doc comments — and a stale list here would make the census below pass
    /// while saying nothing about the type that was just added, which is the
    /// one failure this whole test exists to prevent.
    fn declared_need_types() -> Vec<String> {
        let schema = schemars::schema_for!(operator_core::Needs);
        let props = schema
            .get("properties")
            .and_then(serde_json::Value::as_object)
            .expect("Needs is an object schema")
            .keys()
            .cloned()
            .collect::<Vec<String>>();
        assert!(
            props.len() >= 7,
            "Needs schema yielded {} keys ({props:?}) — the schema shape changed and this \
             census is no longer reading the real key set",
            props.len()
        );
        props
    }

    /// THE CENSUS. Every declared `needs.<type>` either resolves to a
    /// [`ConnectionTarget`] or is named, with a reason, in
    /// [`NEED_TYPES_WITHOUT_A_NETWORK_TARGET`]. There is no third outcome —
    /// in particular, "nobody got round to adding the arm" is not one.
    ///
    /// This is the gate for a defect CLASS, not for the `jetstream` arm.
    /// `render_egress_policy` makes an application's pods egress
    /// DEFAULT-DENY (see its doc comment) and then allows one rule per
    /// declared need. A need type that falls through `default_target`'s `_`
    /// arm therefore gets no rule, and the app is handed working credentials
    /// for a backend its own pods cannot open a socket to. Nothing else in
    /// the tree reports that: the unit tests pass (they only ever asserted
    /// the types that DO have arms), and the live walks pass (they run
    /// without Cilium, so no CNP is ever enforced, and their backend traffic
    /// comes from debug pods that are not Applications and carry no CNP).
    ///
    /// Both directions are asserted. A type in the exempt list that LATER
    /// gains a target must be removed from the list in the same change,
    /// otherwise the list silently becomes a lie about the platform.
    #[test]
    fn every_need_type_has_a_target_or_is_listed_as_target_less() {
        let exempt: BTreeMap<&str, &str> = NEED_TYPES_WITHOUT_A_NETWORK_TARGET
            .iter()
            .map(|(ty, _, reason)| (*ty, *reason))
            .collect();

        for ty in declared_need_types() {
            match (default_target(&ty), exempt.get(ty.as_str())) {
                (Some(_), None) => {}
                (None, Some(_)) => {}
                (None, None) => panic!(
                    "needs.{ty} has no default_target() and is not listed in \
                     NEED_TYPES_WITHOUT_A_NETWORK_TARGET. An application declaring it would be \
                     rendered egress default-deny with NO rule reaching its backend — working \
                     credentials for a server its own pods cannot connect to. Add the arm to \
                     default_target (verified against the real pod labels, not guessed), or add \
                     the type to the exempt list with the reason it has no network target."
                ),
                (Some(t), Some(reason)) => panic!(
                    "needs.{ty} now resolves to a target ({}:{}) but is still listed in \
                     NEED_TYPES_WITHOUT_A_NETWORK_TARGET as {reason:?} — drop the entry.",
                    t.namespace, t.port
                ),
            }
        }
    }

    /// An exemption claimed as [`WhyNoTarget::NotShippedYet`] must still be
    /// unshipped. This is the seam the census alone cannot see: a type that is
    /// exempt because nothing is running for it goes on passing the census
    /// forever, including on the day a provisioner backend lands and an
    /// application can finally declare it for real.
    ///
    /// `cli/docsgen/src/shipped.rs` is the repository's authority on that
    /// state — the same table that decides whether a documentation page may
    /// present a `needs` key as usable — so this reads the fact from there
    /// rather than restating it. Reading a file from the other Cargo
    /// workspace is deliberate and has precedent: `docsgen`'s own
    /// `behaviour::holds` judges a doc sentence by reading the code file that
    /// decides it, for the same reason (a claim that is not re-derived from
    /// the tree rots without telling anyone).
    ///
    /// It fails LOUDLY rather than skipping when the file cannot be read or
    /// the entry cannot be found, which is the other half of that precedent:
    /// a moved file or a reshaped table is exactly how a check like this goes
    /// quiet, and a quiet check here reads identically to a platform that has
    /// not shipped clickhouse.
    #[test]
    fn a_type_exempted_as_unshipped_is_still_unshipped() {
        let path = repo_root().join("cli/docsgen/src/shipped.rs");
        let table = std::fs::read_to_string(&path).unwrap_or_else(|e| {
            panic!(
                "cannot judge the unshipped exemptions: {} is unreadable ({e}). This check is \
                 not passing — it is broken, and a moved file is how it would go quiet.",
                path.display()
            )
        });
        // `pub const SHIPPED:` WITH the colon. Without it the anchor is a
        // PREFIX of any renamed constant (`SHIPPED_TABLE`, `SHIPPED_V2`) and
        // goes on matching a table that is no longer the one being read —
        // mutation-tested, and it is the same rot `docsgen`'s own
        // `behaviour::Claim::holds_today` doc records paying for once.
        assert!(
            table.contains("pub const SHIPPED:"),
            "{} no longer defines `SHIPPED:` — the anchor this check reads is gone, so it would \
             pass vacuously from here on.",
            path.display()
        );

        for (ty, why, _) in NEED_TYPES_WITHOUT_A_NETWORK_TARGET {
            if *why != WhyNoTarget::NotShippedYet {
                continue;
            }
            assert!(
                table.contains(&format!("(\"{ty}\", Status::Declared)")),
                "needs.{ty} is exempt from the egress census only because nothing is running \
                 for it to reach, but {} no longer records it as `Status::Declared`. Either it \
                 SHIPPED — in which case it needs an arm in default_target (verified against \
                 the real pod labels) and its entry dropped from \
                 NEED_TYPES_WITHOUT_A_NETWORK_TARGET — or that table changed shape and this \
                 check can no longer read it.",
                path.display()
            );
        }
    }

    /// The repo root, found by walking up to `cue.mod/module.cue` — the same
    /// anchor `cli/docsgen/src/behaviour.rs` uses to locate files it judges.
    /// The CUE module manifest is at the repo root precisely so `schemas/`
    /// and `examples/` share import paths, which makes it a stable marker.
    fn repo_root() -> std::path::PathBuf {
        let mut dir = std::env::current_dir().expect("cwd");
        loop {
            if dir.join("cue.mod/module.cue").exists() {
                return dir;
            }
            assert!(dir.pop(), "no cue.mod/module.cue above the test's cwd");
        }
    }

    /// The exempt list may only name types that actually exist. A typo'd or
    /// removed key would otherwise sit there forever, exempting nothing while
    /// reading as though it covered something.
    #[test]
    fn the_target_less_list_names_only_real_need_types() {
        let declared = declared_need_types();
        for (ty, _, _) in NEED_TYPES_WITHOUT_A_NETWORK_TARGET {
            assert!(
                declared.iter().any(|d| d == ty),
                "NEED_TYPES_WITHOUT_A_NETWORK_TARGET names {ty:?}, which is not a needs key \
                 ({declared:?})"
            );
        }
    }

    /// 2.5 (ADR 0061): the jetstream arm, pinned field by field. The
    /// selector's labels are the ones a live `nats-0` from the upstream
    /// `nats` chart 2.14.6 actually carries; `e2e/needs-jetstream-walk.sh`
    /// re-checks that against a running server, which is the half this
    /// unit test cannot do.
    #[test]
    fn default_target_jetstream() {
        let js = default_target("jetstream").expect("jetstream target");
        assert_eq!(js.namespace, "nats-system");
        assert_eq!(js.port, 4222);
        assert_eq!(
            js.pod_selector
                .get("app.kubernetes.io/name")
                .map(String::as_str),
            Some("nats")
        );
        assert_eq!(
            js.pod_selector
                .get("app.kubernetes.io/component")
                .map(String::as_str),
            Some("nats")
        );
        // No version-bound label: selecting on either of these would stop
        // matching the moment component_nats.cue re-pins the chart.
        assert!(
            !js.pod_selector.contains_key("app.kubernetes.io/version")
                && !js.pod_selector.contains_key("helm.sh/chart"),
            "a version-bound label in the selector breaks on the next chart bump: {:?}",
            js.pod_selector
        );
    }

    /// A `needs.jetstream` application's CNP carries the NATS rule — the
    /// end-to-end shape of the fix, through the renderer the controller
    /// actually calls.
    #[test]
    fn jetstream_need_adds_the_nats_egress_rule() {
        let labels = app_labels();
        let needs_entries: Vec<(String, NeedEntry)> = vec![(
            "jetstream".to_string(),
            NeedEntry {
                name: None,
                service: Some(ServiceNeed::default()),
                disk: None,
            },
        )];
        let targets = BTreeMap::from([(
            "jetstream".to_string(),
            default_target("jetstream").unwrap(),
        )]);
        let cnp = render_egress_policy(
            "feeder",
            "feeder",
            &labels,
            &needs_entries,
            EgressProfile::Strict,
            &targets,
        );
        let rules = egress_rules(&cnp);
        // Strict baseline (DNS) + the jetstream rule. Strict on purpose:
        // under `internet`/`internal` the same-namespace and world rules
        // would mask a missing NATS rule for anything reachable another way.
        assert_eq!(rules.len(), 2, "strict baseline (DNS) + jetstream need");
        let nats_rule = rules
            .iter()
            .find(|r| {
                r["toEndpoints"][0]["matchLabels"]["io.kubernetes.pod.namespace"] == "nats-system"
            })
            .expect("jetstream egress rule present");
        assert_eq!(
            nats_rule["toEndpoints"][0]["matchLabels"]["app.kubernetes.io/name"],
            "nats"
        );
        assert_eq!(
            nats_rule["toEndpoints"][0]["matchLabels"]["app.kubernetes.io/component"],
            "nats"
        );
        assert_eq!(nats_rule["toPorts"][0]["ports"][0]["port"], "4222");
        assert_eq!(nats_rule["toPorts"][0]["ports"][0]["protocol"], "TCP");
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
                        size: Some("1Gi".to_string()), // 2.6c: owned-disk path; reference handling in T9/T10
                        reference: None,
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
