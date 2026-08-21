// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Pure policy for the shared-backend reaper (ADR 0042 §9).
//!
//! Both shared backends this crate creates lazily — the shared CNPG
//! `Cluster` (`provision_cloudnativepg`) and the Dragonfly pool instances
//! (`provision_dragonfly`) — are created on the first claim that needs them
//! and, until §9, were never given back. A tenantless instance keeps its
//! full Guaranteed reservation (256Mi CNPG / 320Mi Dragonfly), which on a
//! ~4 GB Tier-1 node is not a rounding error.
//!
//! This module answers exactly one question, for one candidate instance:
//! **may it be deleted?** It is I/O-free on purpose — no `Api`, no
//! `Client`, no `Instant::now()`. The caller LISTs, measures the clock and
//! performs the delete; keeping the decision pure is what makes the whole
//! §9.1 predicate table-testable without a cluster.
//!
//! ## The three vetoes (§9.1)
//!
//! A shared instance is reapable only when nothing in the cluster can be
//! pointing at it. Each veto is sufficient on its own to keep the instance:
//!
//!   - **ALLOCATED** — a live `ResourceClaim` names the instance. A tenant
//!     exists and is connected.
//!   - **INTENT** — an unallocated claim of the matching type and
//!     persistence class exists. It has not named an instance yet, but it
//!     is on its way to this one.
//!   - **RETAINED** — a `RetainedClaim` snapshot names the instance (with
//!     the ephemeral-Dragonfly exception, §9.7).
//!
//! **INTENT is not a precaution; it closes a real window in the provision
//! path.** `provision_dragonfly` SSA-applies the `Dragonfly` CR as its
//! first step and does not write `status.instance` until it calls
//! `patch_allocation`, several cluster-wide LISTs later. Between those two
//! points the instance exists and *no object in the cluster refers to it* —
//! a predicate built on ALLOCATED and RETAINED alone reaps the instance the
//! provisioner is halfway through building. The window widens without bound
//! on any error (`?` → `error_policy` → 30s requeue with the CR already
//! created) or across an operator restart. `provision_cloudnativepg` has the
//! same shape around its `Cluster` apply. The `ResourceClaim` *object*
//! exists throughout that window, so vetoing on "an unallocated claim of
//! this class exists" covers all of it.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use serde_json::Value;

use crate::dragonfly::PoolClass;
use crate::reconcile::Backend;
use operator_core::{ResourceClaim, RetainedClaim, ServiceProvider};

/// `ResourceClaim.spec.type` of a Redis claim (the Dragonfly backend's
/// service type — `platform-stack/cue/service_providers.cue`).
const REDIS_TYPE: &str = "redis";

/// `ResourceClaim.spec.type` of a Postgres claim (the CNPG backend's
/// service type).
const PG_TYPE: &str = "pg";

/// Shared CNPG `Cluster` name used when a `cloudnative-pg` provider's
/// `config./cluster` is absent. Mirrors `DEFAULT_CNPG_CLUSTER` in
/// `reconcile.rs` — the same fallback the provisioner itself applies, so
/// the reaper resolves a provider to exactly the cluster the provisioner
/// would have created for it.
const DEFAULT_CNPG_CLUSTER: &str = "platform-postgres";

/// One reap candidate: a shared backend instance that currently exists.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    /// A Dragonfly pool instance, identified by `metadata.name` and the
    /// persistence class parsed back out of it
    /// ([`crate::dragonfly::class_of_instance`]).
    Dragonfly { name: String, class: PoolClass },
    /// The shared CNPG `Cluster`, identified by `metadata.name`.
    Cnpg { name: String },
}

/// Which of the §9.1 vetoes kept an instance alive. Carried out of the
/// decision so the caller can log/meter *why* a candidate survived.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VetoReason {
    /// A live `ResourceClaim` is allocated on this instance.
    Live,
    /// An unallocated claim of this type/class exists — it may be
    /// mid-provision onto this very instance.
    Intent,
    /// A `RetainedClaim` snapshot still names this instance.
    Retained,
}

/// The fate of one candidate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    /// Keep it: something in the cluster can still be pointing at it.
    Veto(VetoReason),
    /// Nothing points at it, but it has not been empty long enough yet.
    Dwell,
    /// Nothing points at it and the dwell has elapsed — delete it.
    Reap,
}

/// The `ServiceProvider` facts the predicate needs, precomputed once per
/// sweep so `reap_decision` stays O(claims) rather than re-scanning
/// providers per candidate.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProviderIndex {
    /// Names of providers whose `spec.backend` is the Dragonfly backend.
    pub dragonfly: BTreeSet<String>,
    /// Provider name → the shared CNPG `Cluster` it targets
    /// (`config./cluster`, else the platform default).
    pub cnpg_cluster: BTreeMap<String, String>,
}

/// Build a [`ProviderIndex`] from every `ServiceProvider` in the cluster.
///
/// Backend strings are never compared literally here — they go through
/// [`Backend::from_spec_backend`], the single canonical mapper the
/// provisioner itself dispatches on, so a backend rename can never leave
/// the reaper silently indexing nothing.
pub fn provider_index(providers: &[ServiceProvider]) -> ProviderIndex {
    let mut idx = ProviderIndex::default();
    for p in providers {
        let Some(name) = p.metadata.name.clone() else {
            continue;
        };
        match Backend::from_spec_backend(&p.spec.backend) {
            Some(Backend::Dragonfly) => {
                idx.dragonfly.insert(name);
            }
            Some(Backend::Cloudnativepg) => {
                // Resolve the SAME way `provision_cloudnativepg` does, so the
                // cluster this maps to is exactly the one that provider's
                // claims were provisioned into. A provider whose `/cluster`
                // we resolved differently from the provisioner would leave a
                // live cluster looking untenanted.
                let cluster = p
                    .spec
                    .config
                    .as_ref()
                    .and_then(|c| c.pointer("/cluster"))
                    .and_then(Value::as_str)
                    .unwrap_or(DEFAULT_CNPG_CLUSTER)
                    .to_string();
                // FIRST wins, deliberately: `ServiceProvider` is namespaced but
                // both this and `find_provider` (reconcile.rs) key by BARE name
                // across namespaces, and `find_provider` resolves a collision
                // with `.find(...)` — the first in list order. Inserting
                // unconditionally would make the reaper last-wins, so two
                // same-named providers in different namespaces would resolve
                // the reaper to one cluster and the provisioner to the other,
                // breaking the equivalence the comment above depends on.
                idx.cnpg_cluster.entry(name).or_insert(cluster);
            }
            // `disk` / `shared-disk` provision no shared instance, and an
            // unknown backend is not ours to reap.
            _ => {}
        }
    }
    idx
}

/// Is this claim ALLOCATED on `target`? Deletion-AGNOSTIC: a claim
/// mid-deletion still holds its DB and role until its finalizer completes,
/// so it still pins the backend.
fn is_allocated_on(claim: &ResourceClaim, target: &Target, idx: &ProviderIndex) -> bool {
    let Some(status) = claim.status.as_ref() else {
        return false;
    };
    match target {
        // Exact under pool growth too — an allocated claim names the instance
        // it landed on, so no class-level widening is ever needed here.
        Target::Dragonfly { name, .. } => status.instance.as_deref() == Some(name.as_str()),
        // CNPG writes no per-claim instance binding, so the claim's tie to a
        // cluster runs through the provider it was scheduled onto.
        Target::Cnpg { name } => claim_cnpg_cluster(claim, idx) == Some(name.as_str()),
    }
}

/// The shared CNPG `Cluster` a claim's matched provider targets, if the
/// scheduler has resolved a provider we know as `cloudnative-pg`.
fn claim_cnpg_cluster<'a>(claim: &ResourceClaim, idx: &'a ProviderIndex) -> Option<&'a str> {
    let provider = claim.status.as_ref()?.provider.as_deref()?;
    idx.cnpg_cluster.get(provider).map(String::as_str)
}

/// "Is a redis claim" — by declared type, or by a provider the index knows
/// as the Dragonfly backend. Either is enough: a claim mid-provision may
/// have only one of the two settled, and INTENT exists precisely to cover
/// half-settled claims.
fn is_redis_claim(claim: &ResourceClaim, idx: &ProviderIndex) -> bool {
    claim.spec.type_ == REDIS_TYPE
        || claim
            .status
            .as_ref()
            .and_then(|s| s.provider.as_deref())
            .is_some_and(|p| idx.dragonfly.contains(p))
}

/// Does this claim carry INTENT toward `target` — i.e. might it be
/// mid-provision onto it?
///
/// Unlike ALLOCATED this DOES skip terminating claims: a claim on its way
/// out will never be provisioned, so it is on its way to nothing.
fn is_intent_for(claim: &ResourceClaim, target: &Target, idx: &ProviderIndex) -> bool {
    if claim.metadata.deletion_timestamp.is_some() {
        return false;
    }
    match target {
        Target::Dragonfly { class, .. } => {
            // Only an UNALLOCATED claim is in the window: once
            // `status.instance` lands, ALLOCATED is exact and this claim's
            // destination is no longer a guess.
            let allocated = claim
                .status
                .as_ref()
                .and_then(|s| s.instance.as_deref())
                .is_some();
            if allocated || !is_redis_claim(claim, idx) {
                return false;
            }
            let persistent = claim.spec.persistent.unwrap_or(false);
            // The class routes the claim — `provision_dragonfly` derives the
            // instance name from it — so a claim of the other class can never
            // land here.
            //
            // EXACT for Dragonfly only while POOL_INSTANCE_INDEX == 0 makes the
            // class→instance map total. WHEN POOL GROWTH LANDS, widen this: an
            // unallocated claim of class C must veto EVERY instance of class C,
            // because which one it lands on is not yet knowable.
            match class {
                PoolClass::Persistent => persistent,
                PoolClass::Ephemeral => !persistent,
            }
        }
        // CNPG has no per-claim instance binding, so an unresolved pg claim
        // vetoes EVERY cnpg cluster. There is normally exactly one, and
        // over-vetoing costs a dwell while under-vetoing costs a database.
        //
        // Considered and accepted: a permanently-unschedulable pg claim (its
        // provider deleted, or a pg backend this controller does not drive)
        // reads as unresolved forever and pins the cluster indefinitely. That
        // is the safe direction — a user who wants Postgres and cannot get it
        // is not someone whose cluster we should reap.
        Target::Cnpg { .. } => {
            claim.spec.type_ == PG_TYPE && claim_cnpg_cluster(claim, idx).is_none()
        }
    }
}

/// Does this snapshot veto `target`?
///
/// RETAINED does NOT veto ephemeral: ADR 0042 §8, "ephemeral claims hold no
/// data, so their number may be freed immediately on deletion". Waiting out
/// the grace there would strand 320Mi for no benefit. NOTE the asymmetry is
/// about the INSTANCE, not the SLOT — `GRACE_PERIOD` is an unconditional
/// 7 days and `used_dbnums` reserves every RetainedClaim regardless of
/// class, so the DB number stays reserved either way (ADR 0042 §9.7).
fn is_retained_for(snapshot: &RetainedClaim, target: &Target) -> bool {
    match target {
        Target::Dragonfly {
            name,
            class: PoolClass::Persistent,
        } => snapshot.spec.instance.as_deref() == Some(name.as_str()),
        Target::Dragonfly {
            class: PoolClass::Ephemeral,
            ..
        } => false,
        Target::Cnpg { name } => snapshot.spec.cnpg_cluster.as_deref() == Some(name.as_str()),
    }
}

/// Decide the fate of one shared-backend instance (ADR 0042 §9.1).
///
/// `live` / `retained` are cluster-wide LISTs of `ResourceClaim` and
/// `RetainedClaim`; `idx` is [`provider_index`] over the cluster's
/// providers. `empty_for` is how long the caller has observed this
/// instance with no tenants (`None` = not yet observed empty, e.g. the
/// first sweep after an operator restart), and `dwell` is how long that
/// must hold before a reap. The clock is the caller's: nothing in here
/// reads the current time, so every row of the table test is exact.
pub fn reap_decision(
    target: &Target,
    live: &[ResourceClaim],
    retained: &[RetainedClaim],
    idx: &ProviderIndex,
    empty_for: Option<Duration>,
    dwell: Duration,
) -> Decision {
    // The vetoes are evaluated before the clock, so a candidate something
    // still points at reports WHY it survived rather than "not yet" — the
    // dwell is a delay on an otherwise-reapable instance, not a veto of its
    // own.
    if live.iter().any(|c| is_allocated_on(c, target, idx)) {
        return Decision::Veto(VetoReason::Live);
    }
    if live.iter().any(|c| is_intent_for(c, target, idx)) {
        return Decision::Veto(VetoReason::Intent);
    }
    if retained.iter().any(|r| is_retained_for(r, target)) {
        return Decision::Veto(VetoReason::Retained);
    }
    // `None` = never yet observed empty (a fresh sweep, or the operator just
    // restarted), which starts the dwell rather than skipping it.
    match empty_for {
        Some(elapsed) if elapsed >= dwell => Decision::Reap,
        _ => Decision::Dwell,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use k8s_openapi::apimachinery::pkg::apis::meta::v1::Time;
    use operator_core::{
        ResourceClaimSpec, ResourceClaimStatus, RetainedClaimSpec, ServiceProviderSpec,
    };
    use serde_json::json;

    const EPHEMERAL_000: &str = "platform-redis-ephemeral-000";
    const PERSISTENT_000: &str = "platform-redis-persistent-000";
    const CLUSTER: &str = "platform-postgres";

    const DWELL: Duration = Duration::from_secs(600);
    /// Long enough that the dwell is satisfied — so a `Reap` in a row below
    /// is the veto layer's verdict, never an accident of the clock.
    const LONG_EMPTY: Option<Duration> = Some(Duration::from_secs(3600));

    // --- fixtures (constructed the way dragonfly.rs / gc.rs build theirs) ---

    fn dragonfly_target(name: &str, class: PoolClass) -> Target {
        Target::Dragonfly {
            name: name.to_owned(),
            class,
        }
    }

    fn cnpg_target(name: &str) -> Target {
        Target::Cnpg {
            name: name.to_owned(),
        }
    }

    fn redis_claim(
        name: &str,
        instance: Option<&str>,
        persistent: bool,
        provider: Option<&str>,
    ) -> ResourceClaim {
        let mut c = ResourceClaim::new(
            name,
            ResourceClaimSpec {
                type_: REDIS_TYPE.into(),
                persistent: Some(persistent),
                ..Default::default()
            },
        );
        c.metadata.namespace = Some("demo".into());
        c.status = Some(ResourceClaimStatus {
            instance: instance.map(str::to_owned),
            dbnum: instance.map(|_| 0),
            provider: provider.map(str::to_owned),
            ..Default::default()
        });
        c
    }

    fn pg_claim(name: &str, provider: Option<&str>) -> ResourceClaim {
        let mut c = ResourceClaim::new(
            name,
            ResourceClaimSpec {
                type_: PG_TYPE.into(),
                ..Default::default()
            },
        );
        c.metadata.namespace = Some("demo".into());
        c.status = Some(ResourceClaimStatus {
            provider: provider.map(str::to_owned),
            ..Default::default()
        });
        c
    }

    fn terminating(mut c: ResourceClaim) -> ResourceClaim {
        c.metadata.deletion_timestamp = Some(Time(Utc::now()));
        c
    }

    fn dragonfly_snapshot(name: &str, instance: &str) -> RetainedClaim {
        RetainedClaim::new(
            name,
            RetainedClaimSpec {
                claim_ref: operator_core::ClaimRef {
                    name: name.into(),
                    namespace: "demo".into(),
                },
                provider: "redis-integrated".into(),
                backend: "dragonfly".into(),
                instance: Some(instance.to_owned()),
                dbnum: Some(0),
                retain_until: "2026-08-28T00:00:00+00:00".into(),
                ..Default::default()
            },
        )
    }

    fn cnpg_snapshot(name: &str, cluster: &str) -> RetainedClaim {
        RetainedClaim::new(
            name,
            RetainedClaimSpec {
                claim_ref: operator_core::ClaimRef {
                    name: name.into(),
                    namespace: "demo".into(),
                },
                provider: "pg-integrated".into(),
                backend: "cloudnative-pg".into(),
                cnpg_cluster: Some(cluster.to_owned()),
                cnpg_namespace: Some("cnpg-system".into()),
                retain_until: "2026-08-28T00:00:00+00:00".into(),
                ..Default::default()
            },
        )
    }

    fn provider(name: &str, type_: &str, backend: &str, config: Option<Value>) -> ServiceProvider {
        let mut p = ServiceProvider::new(
            name,
            ServiceProviderSpec {
                type_: type_.into(),
                backend: backend.into(),
                config,
            },
        );
        p.metadata.namespace = Some("apprafter-system".into());
        p
    }

    /// The platform's seeded pair (`platform-stack/cue/service_providers.cue`).
    fn seeded_index() -> ProviderIndex {
        provider_index(&[
            provider(
                "pg-integrated",
                "pg",
                "cloudnative-pg",
                Some(json!({ "cluster": CLUSTER, "namespace": "cnpg-system" })),
            ),
            provider(
                "redis-integrated",
                "redis",
                "dragonfly",
                Some(json!({ "namespace": "dragonfly-system" })),
            ),
        ])
    }

    // --- 1/2/3: ALLOCATED, dragonfly ---

    #[test]
    fn allocated_live_claim_vetoes_its_instance() {
        let live = vec![redis_claim("web", Some(EPHEMERAL_000), false, None)];
        assert_eq!(
            reap_decision(
                &dragonfly_target(EPHEMERAL_000, PoolClass::Ephemeral),
                &live,
                &[],
                &seeded_index(),
                LONG_EMPTY,
                DWELL,
            ),
            Decision::Veto(VetoReason::Live)
        );
    }

    #[test]
    fn allocated_veto_is_deletion_agnostic() {
        // Deliberately UNLIKE `claims_to_repin` (acl_reconcile.rs), which DOES
        // skip terminating claims because it must not re-pin a dying user. A
        // claim mid-deletion still holds its DB and role until its finalizer
        // completes, so it still pins the backend.
        let live = vec![terminating(redis_claim(
            "web",
            Some(EPHEMERAL_000),
            false,
            None,
        ))];
        assert_eq!(
            reap_decision(
                &dragonfly_target(EPHEMERAL_000, PoolClass::Ephemeral),
                &live,
                &[],
                &seeded_index(),
                LONG_EMPTY,
                DWELL,
            ),
            Decision::Veto(VetoReason::Live)
        );
    }

    #[test]
    fn claim_allocated_elsewhere_does_not_veto() {
        // Same class, DIFFERENT instance. It is allocated, so INTENT does not
        // reach it either — the claim is provably not on its way here.
        let live = vec![redis_claim(
            "web",
            Some("platform-redis-ephemeral-001"),
            false,
            None,
        )];
        assert_eq!(
            reap_decision(
                &dragonfly_target(EPHEMERAL_000, PoolClass::Ephemeral),
                &live,
                &[],
                &seeded_index(),
                LONG_EMPTY,
                DWELL,
            ),
            Decision::Reap
        );
    }

    // --- 4/5/6: INTENT, dragonfly ---

    #[test]
    fn unallocated_claim_of_same_class_vetoes() {
        // The mid-provision guard: the Dragonfly CR exists, `status.instance`
        // has not landed yet, and NOTHING else in the cluster refers to it.
        let live = vec![redis_claim("web", None, false, None)];
        assert_eq!(
            reap_decision(
                &dragonfly_target(EPHEMERAL_000, PoolClass::Ephemeral),
                &live,
                &[],
                &seeded_index(),
                LONG_EMPTY,
                DWELL,
            ),
            Decision::Veto(VetoReason::Intent)
        );
    }

    #[test]
    fn unallocated_claim_of_other_class_does_not_veto() {
        let live = vec![redis_claim("web", None, true, None)];
        assert_eq!(
            reap_decision(
                &dragonfly_target(EPHEMERAL_000, PoolClass::Ephemeral),
                &live,
                &[],
                &seeded_index(),
                LONG_EMPTY,
                DWELL,
            ),
            Decision::Reap
        );
    }

    #[test]
    fn terminating_unallocated_claim_does_not_veto() {
        // It will never be provisioned, so it is on its way to nothing.
        let live = vec![terminating(redis_claim("web", None, false, None))];
        assert_eq!(
            reap_decision(
                &dragonfly_target(EPHEMERAL_000, PoolClass::Ephemeral),
                &live,
                &[],
                &seeded_index(),
                LONG_EMPTY,
                DWELL,
            ),
            Decision::Reap
        );
    }

    #[test]
    fn unallocated_persistent_claim_vetoes_the_persistent_instance() {
        // The mid-provision guard on the class that HOLDS DATA. Without this
        // row the `PoolClass::Persistent` arm of `is_intent_for` is never
        // exercised at all (every other live-claim dragonfly row targets
        // Ephemeral), and inverting it stays green — an under-veto that reaps
        // a persistent instance mid-provision.
        let live = vec![redis_claim("web", None, true, None)];
        assert_eq!(
            reap_decision(
                &dragonfly_target(PERSISTENT_000, PoolClass::Persistent),
                &live,
                &[],
                &seeded_index(),
                LONG_EMPTY,
                DWELL,
            ),
            Decision::Veto(VetoReason::Intent)
        );
    }

    #[test]
    fn unallocated_ephemeral_claim_does_not_veto_the_persistent_instance() {
        // The other half of the pair: the class routes the claim, so an
        // ephemeral claim can never land on a persistent instance. Asserting
        // both directions is what pins the arm against inversion.
        let live = vec![redis_claim("web", None, false, None)];
        assert_eq!(
            reap_decision(
                &dragonfly_target(PERSISTENT_000, PoolClass::Persistent),
                &live,
                &[],
                &seeded_index(),
                LONG_EMPTY,
                DWELL,
            ),
            Decision::Reap
        );
    }

    #[test]
    fn unallocated_claim_is_recognised_by_provider_when_type_is_unset() {
        // "is a redis claim" is `spec.type == redis` OR a provider the index
        // knows as dragonfly — a claim whose type field never landed but
        // which the scheduler already matched still vetoes.
        let mut c = redis_claim("web", None, false, Some("redis-integrated"));
        c.spec.type_ = String::new();
        assert_eq!(
            reap_decision(
                &dragonfly_target(EPHEMERAL_000, PoolClass::Ephemeral),
                &[c],
                &[],
                &seeded_index(),
                LONG_EMPTY,
                DWELL,
            ),
            Decision::Veto(VetoReason::Intent)
        );
    }

    // --- 7/8: RETAINED, dragonfly (the §8/§9.7 asymmetry) ---

    #[test]
    fn snapshot_vetoes_a_persistent_instance() {
        let retained = vec![dragonfly_snapshot("ret", PERSISTENT_000)];
        assert_eq!(
            reap_decision(
                &dragonfly_target(PERSISTENT_000, PoolClass::Persistent),
                &[],
                &retained,
                &seeded_index(),
                LONG_EMPTY,
                DWELL,
            ),
            Decision::Veto(VetoReason::Retained)
        );
    }

    #[test]
    fn snapshot_does_not_veto_an_ephemeral_instance() {
        // ADR 0042 §8/§9.7: an ephemeral claim holds no data, so its snapshot
        // has nothing to reattach to — waiting out the 7-day grace there
        // would strand 320Mi for no benefit.
        let retained = vec![dragonfly_snapshot("ret", EPHEMERAL_000)];
        assert_eq!(
            reap_decision(
                &dragonfly_target(EPHEMERAL_000, PoolClass::Ephemeral),
                &[],
                &retained,
                &seeded_index(),
                LONG_EMPTY,
                DWELL,
            ),
            Decision::Reap
        );
    }

    // --- 9/10/11/12: CNPG ---

    #[test]
    fn claim_whose_provider_maps_to_the_cluster_vetoes() {
        let live = vec![pg_claim("web", Some("pg-integrated"))];
        assert_eq!(
            reap_decision(
                &cnpg_target(CLUSTER),
                &live,
                &[],
                &seeded_index(),
                LONG_EMPTY,
                DWELL,
            ),
            Decision::Veto(VetoReason::Live)
        );
    }

    #[test]
    fn unresolved_pg_claim_vetoes_every_cluster() {
        // CNPG has no per-claim instance binding, so an unscheduled pg claim
        // vetoes every cnpg cluster (there is normally exactly one).
        let live = vec![pg_claim("web", None)];
        assert_eq!(
            reap_decision(
                &cnpg_target(CLUSTER),
                &live,
                &[],
                &seeded_index(),
                LONG_EMPTY,
                DWELL,
            ),
            Decision::Veto(VetoReason::Intent)
        );
    }

    #[test]
    fn snapshot_naming_the_cluster_vetoes() {
        let retained = vec![cnpg_snapshot("ret", CLUSTER)];
        assert_eq!(
            reap_decision(
                &cnpg_target(CLUSTER),
                &[],
                &retained,
                &seeded_index(),
                LONG_EMPTY,
                DWELL,
            ),
            Decision::Veto(VetoReason::Retained)
        );
    }

    #[test]
    fn snapshot_naming_a_different_cluster_does_not_veto() {
        let retained = vec![cnpg_snapshot("ret", "other-postgres")];
        assert_eq!(
            reap_decision(
                &cnpg_target(CLUSTER),
                &[],
                &retained,
                &seeded_index(),
                LONG_EMPTY,
                DWELL,
            ),
            Decision::Reap
        );
    }

    // --- 13/14/15: the dwell, over both target kinds ---

    #[test]
    fn never_observed_empty_dwells() {
        for target in [
            dragonfly_target(EPHEMERAL_000, PoolClass::Ephemeral),
            cnpg_target(CLUSTER),
        ] {
            assert_eq!(
                reap_decision(&target, &[], &[], &seeded_index(), None, DWELL),
                Decision::Dwell,
                "target {target:?}"
            );
        }
    }

    #[test]
    fn empty_just_under_the_dwell_dwells() {
        let just_under = Some(DWELL - Duration::from_secs(1));
        for target in [
            dragonfly_target(EPHEMERAL_000, PoolClass::Ephemeral),
            cnpg_target(CLUSTER),
        ] {
            assert_eq!(
                reap_decision(&target, &[], &[], &seeded_index(), just_under, DWELL),
                Decision::Dwell,
                "target {target:?}"
            );
        }
    }

    #[test]
    fn empty_for_the_full_dwell_reaps() {
        for target in [
            dragonfly_target(EPHEMERAL_000, PoolClass::Ephemeral),
            cnpg_target(CLUSTER),
        ] {
            assert_eq!(
                reap_decision(&target, &[], &[], &seeded_index(), Some(DWELL), DWELL),
                Decision::Reap,
                "target {target:?}"
            );
        }
    }

    // --- 16: provider_index ---

    #[test]
    fn provider_index_honours_a_cluster_override() {
        let idx = provider_index(&[
            provider(
                "pg-custom",
                "pg",
                "cloudnative-pg",
                Some(json!({ "cluster": "tenant-postgres" })),
            ),
            // No config at all → the platform default.
            provider("pg-default", "pg", "cloudnative-pg", None),
            provider("redis-integrated", "redis", "dragonfly", None),
            // A backend this controller does not drive contributes nothing.
            provider("s3-integrated", "s3", "minio", None),
        ]);
        assert_eq!(
            idx.cnpg_cluster.get("pg-custom").map(String::as_str),
            Some("tenant-postgres")
        );
        assert_eq!(
            idx.cnpg_cluster.get("pg-default").map(String::as_str),
            Some(CLUSTER)
        );
        assert_eq!(idx.cnpg_cluster.len(), 2);
        assert_eq!(
            idx.dragonfly,
            ["redis-integrated".to_string()].into_iter().collect()
        );
    }

    #[test]
    fn provider_index_resolves_a_name_collision_first_wins() {
        // `ServiceProvider` is namespaced, but this and `find_provider`
        // (reconcile.rs) both key by BARE name across namespaces.
        // `find_provider` takes the FIRST match in list order, so the index
        // must too — last-wins would resolve the reaper to a different
        // cluster than the provisioner used. Pathological today (shipped
        // providers are seeded into `apprafter-system` with distinct names);
        // pinned so the one-token `entry().or_insert()` cannot regress.
        let mut second = provider(
            "pg-integrated",
            "pg",
            "cloudnative-pg",
            Some(json!({ "cluster": "shadow-postgres" })),
        );
        second.metadata.namespace = Some("other-namespace".into());
        let idx = provider_index(&[
            provider(
                "pg-integrated",
                "pg",
                "cloudnative-pg",
                Some(json!({ "cluster": CLUSTER })),
            ),
            second,
        ]);
        assert_eq!(
            idx.cnpg_cluster.get("pg-integrated").map(String::as_str),
            Some(CLUSTER)
        );
    }

    #[test]
    fn override_cluster_is_what_a_live_claim_vetoes() {
        // The override is not decorative: a claim matched to `pg-custom`
        // pins `tenant-postgres` and leaves the default cluster reapable.
        let idx = provider_index(&[provider(
            "pg-custom",
            "pg",
            "cloudnative-pg",
            Some(json!({ "cluster": "tenant-postgres" })),
        )]);
        let live = vec![pg_claim("web", Some("pg-custom"))];
        assert_eq!(
            reap_decision(
                &cnpg_target("tenant-postgres"),
                &live,
                &[],
                &idx,
                LONG_EMPTY,
                DWELL,
            ),
            Decision::Veto(VetoReason::Live)
        );
    }
}
