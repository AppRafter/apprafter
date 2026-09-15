// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The join between **registrations** and **workloads** (ADR 0062).
//!
//! One Argo CD `Application` — a *registration* — deploys 1..N AppRafter
//! `Application` CRs — the *workloads* of that bundle. Every `app` surface
//! needs both lists at once, and until 2.27 none of them had a place to put
//! the join: each re-derived a partial answer from whichever list it
//! happened to be holding, and each got it wrong for N > 1 (ADR 0062
//! §Context lists the three "first wins" chokepoints).
//!
//! This module is that place. It reads both lists ONCE, joins them on
//! `(namespace, name)`, and answers the only question the read and write
//! surfaces actually ask: *given the string the user typed, what am I
//! looking at?*
//!
//! Nothing here renders. [`AppIndex::resolve`] is pure — no IO, no clock,
//! no environment — so the resolution rules, which carry the whole
//! backward-compatibility story (ADR 0062 §Risks), are table-tested
//! without a cluster.

// The whole module is built for the b-2 (read surfaces) and b-3 (write
// surfaces) plans; nothing calls it yet, and `-D warnings` makes an
// uncalled `pub(crate)` item an error. DELETE THIS ATTRIBUTE IN b-2, when
// `app list` / `app status` become the callers — inventing a caller here
// to satisfy the lint would be worse than saying so.
//
// `expect`, not `allow`, and the first use of it in this repository:
// once b-2 supplies the callers the expectation goes unfulfilled and
// `unfulfilled_lint_expectations` fires under `-D warnings`. That turns
// the sentence above from a promise into a machine check — an `allow`
// left behind is invisible forever, which is how a temporary suppression
// becomes permanent.
#![expect(dead_code)]

use std::collections::BTreeMap;
use std::path::Path;

use cli_core::Result;
use serde_json::Value;

use crate::commands::app::ARGOCD_NAMESPACE;
use crate::commands::app_open::apprafter_app_refs;
use crate::commands::k8s_helpers::kubectl_get_json_by_selector;

/// The Argo CD label whose value groups the per-environment registrations
/// of one logical application (`<logical>-<env>`). Stamped by
/// `build_application_manifest`; it is the entire pre-2.9
/// backward-compatibility story — see [`AppIndex::resolve`] rule 2.
const GROUPING_LABEL: &str = "/metadata/labels/apprafter.io~1application";

/// One AppRafter `Application` CR, with the registration that deploys it.
#[derive(Debug, Clone)]
pub(crate) struct WorkloadEntry {
    pub(crate) cr: Value,
    pub(crate) name: String,
    pub(crate) namespace: String,
    /// The registration that deploys it, if any claims it.
    ///
    /// `None` is a real, readable state, not a defect: a CR applied by
    /// hand, one left behind by a removed registration, and one whose
    /// registration has not synced yet all land here. A surface must say
    /// "no registration claims this" rather than invent one.
    ///
    /// When TWO registrations claim the same workload — which `app add`
    /// permits today, since its duplicate guard keys only on the Argo
    /// object name — this names the alphabetically first of them, and
    /// only for attribution. Membership is NOT read from this field:
    /// [`AppIndex::workloads_of`] re-derives it from each registration's
    /// own `status.resources[]`, so a contested workload appears under
    /// *both* registrations rather than vanishing from the loser.
    pub(crate) registration: Option<String>,
}

/// Both cluster-wide lists, joined.
pub(crate) struct AppIndex {
    /// Every AppRafter Application CR in the cluster, sorted by
    /// `(namespace, name)` so listings are stable across runs.
    entries: Vec<WorkloadEntry>,
    /// Every Argo CD Application in `argocd`, by `metadata.name`.
    registrations: BTreeMap<String, Value>,
    /// Registration names that claim no workload — registered, not yet
    /// synced. Kept separate because "no workloads" and "not registered"
    /// are different answers and callers must not conflate them.
    empty_registrations: Vec<String>,
}

/// What the string a user typed turned out to name.
#[derive(Debug)]
pub(crate) enum Resolution {
    /// Exactly one workload, addressed by its own CR name.
    Workload(WorkloadEntry),
    /// One workload NAME living in two or more namespaces. Always 2+
    /// entries — a one-element "ambiguity" is a disambiguation prompt
    /// with nothing to disambiguate, so the single case resolves as
    /// [`Resolution::Workload`] instead.
    AmbiguousWorkload(Vec<WorkloadEntry>),
    /// One registration and every workload it deploys.
    Registration(String, Vec<WorkloadEntry>),
    /// Two or more registrations share the typed grouping label — the
    /// pre-2.9 per-environment shape — carried as `(registration,
    /// workloads)` pairs in registration-name order.
    ///
    /// The pairing is load-bearing, not decoration. `app status <logical>`
    /// AGGREGATES every environment today rather than demanding `--env`
    /// (`app.rs:1029-1046`: an index line, then one detail block per
    /// registration), and that rendering needs to know which workload
    /// belongs to which registration. A flat workload list cannot say.
    ///
    /// A candidate that has NOT synced is carried with an empty workload
    /// list rather than dropped: it is a registration the reader asked
    /// about, and omitting it would silently shrink the fleet the
    /// aggregate describes.
    AmbiguousRegistration(Vec<(String, Vec<WorkloadEntry>)>),
    /// A registration that has never synced — `status.resources[]` names
    /// no workload at all. NOT the same as having none in the index; see
    /// [`AppIndex::resolve_registration`].
    PendingRegistration(String),
    NotFound,
}

impl AppIndex {
    /// Read both cluster-wide lists and join them. EXACTLY two reads.
    ///
    /// Modelled on `app_rollup::ClusterApplications::read`, with one
    /// deliberate inversion: that struct keeps each side's `Result`
    /// because its two sections must fail differently. Here they must
    /// not. A failed Argo CD read would leave every workload
    /// unattributed, which does not read as "the read failed" — it reads
    /// as "nothing is registered", the single most misleading answer this
    /// index can give. So both sides propagate.
    pub(crate) fn read(kubeconfig: &Path) -> Result<Self> {
        // `-A`, via the `None` namespace. `kubectl_get_json` without a
        // namespace does NOT pass `-A`: it falls through to the
        // kubeconfig's default namespace, and a cluster whose workloads
        // live elsewhere then reads as "nothing exists"
        // (`app_rollup.rs:179-181` carries the same warning).
        let crs = kubectl_get_json_by_selector("application.apprafter.io", "", None, kubeconfig)?;
        let argo = kubectl_get_json_by_selector(
            "application.argoproj.io",
            "",
            Some(ARGOCD_NAMESPACE),
            kubeconfig,
        )?;
        Ok(Self::from_lists(crs, argo))
    }

    /// The join itself, separated from the two `kubectl` calls so the
    /// tests build an index through the code path `read` uses rather than
    /// hand-assembling the struct.
    fn from_lists(crs: Vec<Value>, argo: Vec<Value>) -> Self {
        let registrations: BTreeMap<String, Value> = argo
            .into_iter()
            .filter_map(|a| {
                let name = a
                    .pointer("/metadata/name")
                    .and_then(Value::as_str)?
                    .to_string();
                Some((name, a))
            })
            .collect();

        // (namespace, workload name) -> the registration credited with it.
        // `or_insert` over a BTreeMap iteration makes the winner of a
        // contested workload the alphabetically first registration, and
        // deterministic; see `WorkloadEntry::registration` for why the
        // loser does not lose the workload.
        let mut claimed: BTreeMap<(String, String), String> = BTreeMap::new();
        for (registration, argo_app) in &registrations {
            for cr_ref in apprafter_app_refs(argo_app) {
                // A ref with an unknown namespace cannot be keyed, so it
                // cannot mark a workload — and must NOT be dropped
                // silently either. It is still a CLAIM, and
                // `empty_registrations` counts claims (below) rather than
                // join results, so a registration that HAS synced is
                // never reported as "not yet synced" on account of one
                // unkeyable ref. The workload itself simply stays
                // unattributed, which is the honest answer: we cannot
                // prove which CR it is, and `CrRef::namespace == None`
                // must never be handed to `kubectl -n`.
                let Some(namespace) = cr_ref.namespace else {
                    continue;
                };
                claimed
                    .entry((namespace, cr_ref.name))
                    .or_insert_with(|| registration.clone());
            }
        }

        let mut entries: Vec<WorkloadEntry> = crs
            .into_iter()
            .filter_map(|cr| {
                // A CR the apiserver returns always carries both; one that
                // does not cannot be addressed by any surface downstream
                // (nothing to type, nothing to pass to `-n`).
                let name = cr
                    .pointer("/metadata/name")
                    .and_then(Value::as_str)?
                    .to_string();
                let namespace = cr
                    .pointer("/metadata/namespace")
                    .and_then(Value::as_str)?
                    .to_string();
                let registration = claimed.get(&(namespace.clone(), name.clone())).cloned();
                Some(WorkloadEntry {
                    cr,
                    name,
                    namespace,
                    registration,
                })
            })
            .collect();
        // No `deletionTimestamp` filter: a terminating workload is still
        // deployed, and a reader asking after it must see it. The
        // roll-up filters those for an unrelated reason — its problem
        // ledger can never be flushed again for a dying CR.
        entries.sort_by(|a, b| {
            a.namespace
                .cmp(&b.namespace)
                .then_with(|| a.name.cmp(&b.name))
        });

        // Claims, NOT join results. A registration whose `status.resources[]`
        // is empty has not synced; one that lists workloads the index does
        // not hold has synced and lost them (deleted out from under Argo CD,
        // filtered by RBAC, unkeyable namespace). Defining "empty" by the
        // join would file the second case under the first and tell an
        // operator to wait for a sync that already happened.
        let empty_registrations: Vec<String> = registrations
            .iter()
            .filter(|(_, a)| apprafter_app_refs(a).is_empty())
            .map(|(n, _)| n.clone())
            .collect();

        Self {
            entries,
            registrations,
            empty_registrations,
        }
    }

    /// Whether a registration's `status.resources[]` names no workload at
    /// all — i.e. it has never synced.
    ///
    /// Reads the fact [`AppIndex::from_lists`] already computed rather
    /// than recomputing the predicate: one fact, one computation, in the
    /// module whose whole purpose is to stop facts being re-derived per
    /// surface. Every caller holds a registration name the index knows
    /// (rule 1 checked `contains_key`, rule 2 collected it out of the
    /// map), so membership IS the answer.
    fn claims_nothing(&self, registration: &str) -> bool {
        self.empty_registrations.iter().any(|r| r == registration)
    }

    /// Every indexed workload a registration deploys.
    ///
    /// Derived from the registration's own `status.resources[]` rather
    /// than from [`WorkloadEntry::registration`], so a workload claimed by
    /// two registrations is reported under both. The alternative — filter
    /// the entries by their attributed registration — would make
    /// `app status <loser>` print fewer workloads than the registration
    /// actually deploys, which is the exact failure ADR 0062 exists to
    /// end.
    fn workloads_of(&self, registration: &str) -> Vec<WorkloadEntry> {
        let Some(argo_app) = self.registrations.get(registration) else {
            return Vec::new();
        };
        let refs = apprafter_app_refs(argo_app);
        self.entries
            .iter()
            .filter(|e| {
                refs.iter().any(|r| {
                    r.name == e.name && r.namespace.as_deref() == Some(e.namespace.as_str())
                })
            })
            .cloned()
            .collect()
    }

    /// Resolve the string a user typed, **registration first**.
    ///
    /// Order, and why it is this order:
    ///
    /// 1. a registration whose `metadata.name` is `name`;
    /// 2. a registration whose `apprafter.io/application` label is `name`;
    /// 3. a workload whose CR name is `name`, narrowed by `namespace`;
    /// 4. nothing.
    ///
    /// **Registration first is what makes ADR 0062's addressing rule true
    /// by construction** — "the positional argument of every `app` verb is
    /// the registration name, never a workload name". `derive_app_name`
    /// and `app scaffold` both default to the repository basename, so
    /// "the registration name is also a workload name" is the NORMAL
    /// state of a single-workload bundle, not an edge case. A precedence
    /// rule pointing the other way would therefore make the common case
    /// ambiguous: the same string would mean the bundle on one cluster and
    /// one CR inside it on another, depending on nothing the user can see.
    /// Put the other way round: under this order no input can mean either,
    /// so the grammar needs no tie-breaker and no `--workload` on the
    /// common path.
    ///
    /// **Rule 2 is the entire backward-compatibility story.** Pre-2.9
    /// per-environment registrations are named `<logical>-<env>` and share
    /// an `apprafter.io/application` label carrying the logical name. That
    /// logical name is what `app list` has always printed and what every
    /// user has in their shell history; if typing it stopped resolving,
    /// `app status <name>` would read as "removed" across a whole fleet.
    /// One labelled registration resolves outright — the precedent is
    /// `app.rs`'s `single_deployment_or_guidance`, which resolves a lone
    /// per-env deployment without demanding `--env`. Two or more come back
    /// as [`Resolution::AmbiguousRegistration`] — every candidate, paired
    /// with its own workloads — never silently collapsed to the first.
    /// "Ambiguous" is the resolver's word, not necessarily the caller's:
    /// `app status` aggregates those candidates rather than refusing, and
    /// it can only do that because the pairing survives.
    ///
    /// `namespace` narrows rule 3 ONLY. A bundle is one namespace (ADR
    /// 0062 §Decision), so narrowing a registration by it is at best a
    /// no-op and at worst turns a synced bundle into an empty one — which
    /// would render as "not yet synced", a statement that is simply false.
    ///
    /// Pure: no IO, no clock, no environment.
    pub(crate) fn resolve(&self, name: &str, namespace: Option<&str>) -> Resolution {
        // 1. The registration's own name.
        if self.registrations.contains_key(name) {
            return self.resolve_registration(name);
        }

        // 2. The grouping label — pre-2.9 `<logical>-<env>` registrations.
        let labelled: Vec<String> = self
            .registrations
            .iter()
            .filter(|(_, a)| a.pointer(GROUPING_LABEL).and_then(Value::as_str) == Some(name))
            .map(|(n, _)| n.clone())
            .collect();
        if labelled.len() == 1 {
            return self.resolve_registration(&labelled[0]);
        }
        if labelled.len() > 1 {
            // EVERY candidate, each paired with its own workloads — an
            // unsynced one included, carrying an empty list. See
            // [`Resolution::AmbiguousRegistration`] for why the pairing
            // and the empty candidates both have to survive.
            let candidates: Vec<(String, Vec<WorkloadEntry>)> = labelled
                .iter()
                .map(|r| (r.clone(), self.workloads_of(r)))
                .collect();
            // Pending is decided by the CLAIM, exactly as
            // [`AppIndex::resolve_registration`] decides it — never by
            // whether the join came back empty. Testing the join here
            // would make this branch contradict that one: two synced
            // registrations whose workloads are absent from the index
            // would resolve as "not yet synced" through their shared
            // label and as synced-with-no-workloads through either of
            // their own names. Same index, same objects, opposite answers
            // depending on which string the user typed.
            return if labelled.iter().all(|r| self.claims_nothing(r)) {
                Resolution::PendingRegistration(labelled[0].clone())
            } else {
                Resolution::AmbiguousRegistration(candidates)
            };
        }

        // 3. A workload by its own CR name.
        let mut hits: Vec<WorkloadEntry> = self
            .entries
            .iter()
            .filter(|e| e.name == name && namespace.is_none_or(|ns| e.namespace == ns))
            .cloned()
            .collect();
        match hits.len() {
            0 => Resolution::NotFound,
            1 => Resolution::Workload(hits.remove(0)),
            // Already in `(namespace, name)` order — `entries` is sorted
            // and this filter preserves it — so a caller's disambiguation
            // hint lists namespaces in a stable order.
            _ => Resolution::AmbiguousWorkload(hits),
        }
    }

    /// A known registration, with the "registered but not yet synced"
    /// case split out. `status.resources[]` is empty until the first
    /// sync, and "this bundle has no workloads" is a different answer
    /// from "there is no such bundle" — a caller that conflates them
    /// tells a user to check their spelling seconds after a successful
    /// `app add`.
    ///
    /// Pending is decided by the CLAIM, not by the join: a registration
    /// that lists workloads the index does not hold HAS synced, and
    /// telling its operator to wait would send them to watch a sync that
    /// already finished. That case resolves as a `Registration` with an
    /// empty workload list — which is exactly what is true of it.
    fn resolve_registration(&self, registration: &str) -> Resolution {
        let workloads = self.workloads_of(registration);
        if workloads.is_empty() && self.claims_nothing(registration) {
            Resolution::PendingRegistration(registration.to_string())
        } else {
            Resolution::Registration(registration.to_string(), workloads)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// An AppRafter `Application` CR as the apiserver returns it.
    fn cr(namespace: &str, name: &str) -> Value {
        json!({
            "apiVersion": "apprafter.io/v1alpha1",
            "kind": "Application",
            "metadata": { "name": name, "namespace": namespace },
        })
    }

    /// An Argo CD registration deploying `workloads` into `namespace`.
    /// `label` is the `apprafter.io/application` grouping value; pass the
    /// registration's own name for a post-2.9 single-environment bundle.
    fn registration(name: &str, label: &str, namespace: &str, workloads: &[&str]) -> Value {
        let resources: Vec<Value> = workloads
            .iter()
            .map(|w| {
                json!({ "group": "apprafter.io", "kind": "Application",
                        "version": "v1alpha1", "name": w, "namespace": namespace })
            })
            .collect();
        json!({
            "metadata": {
                "name": name,
                "labels": { "apprafter.io/application": label },
            },
            "spec": { "destination": { "namespace": namespace } },
            "status": { "resources": resources },
        })
    }

    /// The one fixture every resolution test runs against, built through
    /// the SAME private constructor [`AppIndex::read`] uses — so these
    /// tests exercise the real join, not a hand-assembled struct.
    ///
    /// It carries, deliberately, every shape the resolver must tell
    /// apart: a multi-workload bundle, a bundle whose registration name
    /// collides with its own workload's, a pre-2.9 group of
    /// per-environment registrations sharing a grouping label (one of
    /// which has never synced), the single-environment version of that
    /// same pre-2.9 shape, a registration that has not synced, a synced
    /// pair whose workloads are absent from the index, one workload name
    /// living in two namespaces, a workload two registrations both claim,
    /// and a CR no registration claims.
    fn fixture_index() -> AppIndex {
        let crs = vec![
            // A two-workload bundle.
            cr("acme", "acme-api"),
            cr("acme", "acme-web"),
            // A bundle whose registration name IS its workload's name.
            cr("blog", "blog"),
            // Pre-2.9: one logical app, three per-environment
            // registrations — one of which has never synced.
            cr("parser-prod", "parser-api"),
            cr("parser-staging", "parser-api"),
            // Pre-2.9, single environment: registration `cms-prod`
            // renders a CR called `landing-cms` (the real shape asserted
            // by `app.rs`'s own test).
            cr("cms", "landing-cms"),
            // One workload name in two namespaces.
            cr("prod", "dup"),
            cr("staging", "dup"),
            // ONE workload, claimed by TWO registrations — `app add`
            // permits this, its duplicate guard keying only on the Argo
            // object name.
            cr("shop", "shop"),
            // Claimed by nobody.
            cr("legacy-ns", "legacy"),
        ];
        let argo = vec![
            registration(
                "acme-platform",
                "acme-platform",
                "acme",
                &["acme-api", "acme-web"],
            ),
            registration("blog", "blog", "blog", &["blog"]),
            registration("parser-prod", "parser", "parser-prod", &["parser-api"]),
            registration(
                "parser-staging",
                "parser",
                "parser-staging",
                &["parser-api"],
            ),
            // Same grouping label, never synced — a candidate an
            // aggregating caller must still be told about.
            json!({
                "metadata": { "name": "parser-canary",
                              "labels": { "apprafter.io/application": "parser" } },
                "spec": { "destination": { "namespace": "parser-canary" } },
            }),
            registration("cms-prod", "cms", "cms", &["landing-cms"]),
            // Registered, never synced: no `status.resources[]` at all.
            json!({
                "metadata": { "name": "fresh", "labels": { "apprafter.io/application": "fresh" } },
                "spec": { "destination": { "namespace": "fresh" } },
            }),
            // Synced, but the workloads they claim are not in the index —
            // deleted out from under Argo CD, or hidden by RBAC. NOT the
            // same state as `fresh`, and it has to read the same way
            // through a registration's own name and through their shared
            // label.
            registration("ghost-prod", "ghost", "ghost-prod", &["ghost-api"]),
            registration("ghost-staging", "ghost", "ghost-staging", &["ghost-api"]),
            // Both claim `shop/shop`.
            registration("shop-a", "shop-a", "shop", &["shop"]),
            registration("shop-b", "shop-b", "shop", &["shop"]),
        ];
        AppIndex::from_lists(crs, argo)
    }

    #[test]
    fn resolve_matches_a_workload_by_its_own_name() {
        // Nothing is registered under `acme-api`, so rules 1 and 2 miss
        // and the workload arm answers — carrying the registration that
        // deploys it, which is the string every `app` verb takes.
        match fixture_index().resolve("acme-api", None) {
            Resolution::Workload(w) => {
                assert_eq!(w.name, "acme-api");
                assert_eq!(w.namespace, "acme");
                assert_eq!(w.registration.as_deref(), Some("acme-platform"));
                assert_eq!(
                    w.cr.pointer("/kind").and_then(Value::as_str),
                    Some("Application")
                );
            }
            other => panic!("expected the workload, got {other:?}"),
        }
    }

    #[test]
    fn resolve_prefers_the_registration_when_the_name_is_both() {
        // THE load-bearing rule (ADR 0062 §Addressing). `blog` names both
        // a registration and a workload inside it — the normal state of a
        // scaffolded single-workload bundle, since the registration name
        // and the CR name both default to the repo basename. The
        // registration must win, or the same string would mean the bundle
        // on one cluster and one CR inside it on another.
        match fixture_index().resolve("blog", None) {
            Resolution::Registration(reg, workloads) => {
                assert_eq!(reg, "blog");
                assert_eq!(workloads.len(), 1);
                assert_eq!(workloads[0].name, "blog");
                assert_eq!(workloads[0].namespace, "blog");
            }
            other => panic!("expected the registration to win, got {other:?}"),
        }
    }

    #[test]
    fn resolve_falls_back_to_the_grouping_label() {
        // The whole backward-compatibility story. Pre-2.9 registrations
        // are named `<logical>-<env>`; the logical name is what users have
        // in their shell history and what `app list` has always printed,
        // and no workload is called `parser` either — so without rule 2
        // this is NotFound and reads as "the app was removed".
        //
        // Every candidate comes back paired with its own workloads,
        // including `parser-canary`, which has never synced: `app status`
        // aggregates the environments rather than refusing, and a dropped
        // candidate would silently shrink the fleet it describes.
        match fixture_index().resolve("parser", None) {
            Resolution::AmbiguousRegistration(candidates) => {
                let names: Vec<&str> = candidates.iter().map(|(r, _)| r.as_str()).collect();
                assert_eq!(
                    names,
                    vec!["parser-canary", "parser-prod", "parser-staging"],
                    "every candidate, in registration-name order"
                );
                assert!(candidates[0].1.is_empty(), "{candidates:?}");
                let ns: Vec<&str> = candidates[1..]
                    .iter()
                    .flat_map(|(_, w)| w.iter().map(|w| w.namespace.as_str()))
                    .collect();
                assert_eq!(ns, vec!["parser-prod", "parser-staging"], "{candidates:?}");
                assert!(candidates[1..]
                    .iter()
                    .all(|(_, w)| w.len() == 1 && w[0].name == "parser-api"));
            }
            other => panic!("expected every environment, got {other:?}"),
        }
    }

    #[test]
    fn resolve_finds_a_single_environment_registration_by_its_label() {
        // The commonest pre-2.9 shape, and the one most likely to be
        // typed: one environment, so the logical name is unambiguous and
        // must resolve outright without demanding `--env` — the behaviour
        // `single_deployment_or_guidance` already ships. `cms-prod`
        // renders a CR called `landing-cms`, so neither rule 1 nor rule 3
        // can answer this.
        match fixture_index().resolve("cms", None) {
            Resolution::Registration(reg, workloads) => {
                assert_eq!(reg, "cms-prod");
                assert_eq!(workloads.len(), 1);
                assert_eq!(workloads[0].name, "landing-cms");
            }
            other => panic!("expected the labelled registration, got {other:?}"),
        }
    }

    #[test]
    fn resolve_reports_a_registration_that_has_not_synced() {
        // `status.resources[]` is empty until the first sync. "This
        // bundle has no workloads yet" and "there is no such bundle" are
        // different answers, and a caller that conflates them tells the
        // user to check their spelling seconds after a successful
        // `app add`.
        match fixture_index().resolve("fresh", None) {
            Resolution::PendingRegistration(reg) => assert_eq!(reg, "fresh"),
            other => panic!("expected a pending registration, got {other:?}"),
        }
    }

    #[test]
    fn a_synced_registration_whose_workloads_are_absent_is_not_reported_as_pending() {
        // The near miss. `ghost-prod` and `ghost-staging` HAVE synced —
        // each claims a workload — the index just does not hold it
        // (deleted out from under Argo CD, or hidden by RBAC). Reporting
        // that as "not yet synced" would send an operator to watch a sync
        // that already finished, so pending is decided by the CLAIM.
        //
        // Both arms are asserted because they are one rule: if only
        // `resolve_registration` consulted the claim, the same two objects
        // would read as synced through either registration's own name and
        // as pending through their shared label — opposite answers from
        // one index, decided by which string the user happened to type.
        let index = fixture_index();
        match index.resolve("ghost-prod", None) {
            Resolution::Registration(reg, workloads) => {
                assert_eq!(reg, "ghost-prod");
                assert!(workloads.is_empty(), "{workloads:?}");
            }
            other => panic!("expected a synced registration with no workloads, got {other:?}"),
        }
        match index.resolve("ghost", None) {
            Resolution::AmbiguousRegistration(candidates) => {
                let names: Vec<&str> = candidates.iter().map(|(r, _)| r.as_str()).collect();
                assert_eq!(names, vec!["ghost-prod", "ghost-staging"]);
                assert!(
                    candidates.iter().all(|(_, w)| w.is_empty()),
                    "{candidates:?}"
                );
            }
            other => panic!("expected both synced candidates, got {other:?}"),
        }
    }

    #[test]
    fn a_contested_workload_appears_under_both_registrations() {
        // `app add`'s duplicate guard keys only on the Argo object name,
        // so two registrations really can claim one CR. Membership is
        // therefore re-derived from each registration's own
        // `status.resources[]`, NOT filtered by the workload's attributed
        // registration — under that filter `shop-b` would report zero
        // workloads and `app status shop-b` would print fewer than it
        // deploys, which is the exact failure ADR 0062 exists to end.
        let index = fixture_index();
        for reg in ["shop-a", "shop-b"] {
            match index.resolve(reg, None) {
                Resolution::Registration(name, workloads) => {
                    assert_eq!(name, reg);
                    assert_eq!(workloads.len(), 1, "{reg} lost its workload");
                    assert_eq!(workloads[0].name, "shop");
                    // Attribution is the alphabetically first claimant,
                    // deterministically — and is attribution ONLY.
                    assert_eq!(workloads[0].registration.as_deref(), Some("shop-a"));
                }
                other => panic!("expected {reg} to carry the contested workload, got {other:?}"),
            }
        }
    }

    #[test]
    fn resolve_reports_ambiguity_when_one_workload_name_spans_namespaces() {
        // Picking one would act on a production workload while naming a
        // staging one. The entries carry namespace AND registration, so
        // the caller can print a disambiguation hint the user can act on.
        match fixture_index().resolve("dup", None) {
            Resolution::AmbiguousWorkload(workloads) => {
                let ns: Vec<&str> = workloads.iter().map(|w| w.namespace.as_str()).collect();
                assert_eq!(ns, vec!["prod", "staging"], "{workloads:?}");
                assert!(workloads.iter().all(|w| w.name == "dup"));
            }
            other => panic!("expected ambiguity, got {other:?}"),
        }
    }

    #[test]
    fn resolve_disambiguates_by_namespace() {
        let index = fixture_index();
        match index.resolve("dup", Some("prod")) {
            Resolution::Workload(w) => {
                assert_eq!(w.name, "dup");
                assert_eq!(w.namespace, "prod");
            }
            other => panic!("expected the prod workload, got {other:?}"),
        }
        // A namespace that holds no such workload narrows to nothing —
        // never to "any of them".
        assert!(matches!(
            index.resolve("dup", Some("nowhere")),
            Resolution::NotFound
        ));
    }

    #[test]
    fn resolve_is_not_found_for_an_unknown_name() {
        assert!(matches!(
            fixture_index().resolve("nope", None),
            Resolution::NotFound
        ));
    }

    #[test]
    fn a_workload_no_registration_claims_is_still_indexed() {
        // A CR applied by hand, or left behind by a removed registration.
        // Dropping it would make the index quietest about exactly the
        // objects most likely to be wrong; `registration: None` says what
        // is true without inventing an owner.
        match fixture_index().resolve("legacy", None) {
            Resolution::Workload(w) => {
                assert_eq!(w.namespace, "legacy-ns");
                assert_eq!(w.registration, None);
            }
            other => panic!("expected the orphan workload, got {other:?}"),
        }
    }
}
