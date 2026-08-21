// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The shared-backend reaper (ADR 0042 §9) — pure policy, and the sweep
//! that drives it.
//!
//! Both shared backends this crate creates lazily — the shared CNPG
//! `Cluster` (`provision_cloudnativepg`) and the Dragonfly pool instances
//! (`provision_dragonfly`) — are created on the first claim that needs them
//! and, until §9, were never given back. A tenantless instance keeps its
//! full Guaranteed reservation (256Mi CNPG / 320Mi Dragonfly), which on a
//! ~4 GB Tier-1 node is not a rounding error.
//!
//! The module is in two halves, and the seam between them is deliberate.
//!
//! The **predicate** answers exactly one question, for one candidate
//! instance: **may it be deleted?** It is I/O-free on purpose — no `Api`,
//! no `Client`, no `Instant::now()`. Keeping the decision pure is what
//! makes the whole §9.1 predicate table-testable without a cluster.
//!
//! The **sweep** ([`run`] / `sweep`, at the bottom of this file) is the I/O
//! that feeds it: it LISTs, measures the clock, and performs the delete.
//! It holds no policy of its own.
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

use std::collections::hash_map::Entry;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;
use std::time::{Duration, Instant};

use k8s_openapi::apimachinery::pkg::apis::meta::v1::{ListMeta, ObjectMeta};
use kube::api::{Api, DeleteParams, DynamicObject, ListParams, Patch, PatchParams, Preconditions};
use kube::ResourceExt;
use serde_json::{json, Value};
use tracing::{debug, info, warn};

use crate::acl_reconcile::redis_namespace_from;
use crate::dragonfly::{class_of_instance, PoolClass};
use crate::reconcile::{cluster_ar, dragonfly_cluster_ar, pvc_ar, Backend};
use crate::{Context, ReconcileError};
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

/// Namespace the shared CNPG `Cluster` lives in when a `cloudnative-pg`
/// provider's `config./namespace` is absent. Mirrors the literal
/// `provision_cloudnativepg` falls back to (reconcile.rs) for the same
/// reason [`DEFAULT_CNPG_CLUSTER`] mirrors its cluster name.
const DEFAULT_CNPG_NAMESPACE: &str = "cnpg-system";

/// Label key of the ownership stamp every candidate of every arm must
/// carry (`dragonfly::dragonfly_object` and `cnpg::cluster_object` both
/// emit it; each module has a test pinning its object's stamp against the
/// other's).
const MANAGED_BY_KEY: &str = "apprafter.io/managed-by";

/// Label value of that stamp.
const MANAGED_BY_VALUE: &str = "apprafter";

/// The stamp as a LIST label selector.
///
/// ONE source for both arms on purpose — otherwise one arm's literal could
/// be retyped and only that arm's candidate set would silently empty.
/// Built from the same two constants [`is_stamped`] checks, so the
/// server-side filter and the client-side re-check cannot disagree.
fn managed_by_selector() -> String {
    format!("{MANAGED_BY_KEY}={MANAGED_BY_VALUE}")
}

/// Does this object carry the ownership stamp?
///
/// **GATE 2, made structural.** Every arm also passes
/// [`managed_by_selector`] to its LIST, so in the normal case this is
/// redundant — deliberately. That selector is ONE argument on ONE call:
/// drop it, add a field selector beside it, or "optimise" the LIST into a
/// reflector, and the entire second gate disappears with nothing failing
/// and no test noticing, because a test can only pin the selector STRING,
/// never that the arm still filters on it. This re-check is the gate the
/// arms cannot lose by editing a call argument, and it is what the
/// `arm_rejects_an_unstamped_candidate` tests actually exercise.
fn is_stamped(obj: &DynamicObject) -> bool {
    obj.labels().get(MANAGED_BY_KEY).map(String::as_str) == Some(MANAGED_BY_VALUE)
}

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

/// The namespace the shared CNPG `Cluster`s live in, resolved over a
/// provider list the caller ALREADY holds.
///
/// The `redis_namespace_from` pattern (acl_reconcile.rs), for the same
/// reason: the reaper must not take a second, un-truncation-checked
/// apiserver snapshot on a path that deletes. It lives here rather than
/// beside its Dragonfly twin because it pairs with [`provider_index`]'s
/// `/cluster` resolution directly above — both mirror
/// `provision_cloudnativepg` (reconcile.rs), and a namespace resolved
/// differently from the provisioner's is a namespace the reaper finds no
/// clusters in.
///
/// Deliberately ONE namespace for the whole arm, from the FIRST
/// `cloudnative-pg` provider in apiserver order — exactly what
/// `redis_namespace_from` does for Dragonfly. A second cnpg provider
/// pointing at a DIFFERENT namespace therefore has its cluster listed in
/// the wrong place and simply never becomes a candidate: an under-reap,
/// the safe direction. It cannot fail the other way, because
/// `is_allocated_on` / `is_retained_for` match a cluster by NAME with no
/// namespace — a claim on a same-named cluster in the other namespace
/// OVER-vetoes this one rather than under-vetoing it.
///
/// Backend matching goes through [`Backend::from_spec_backend`] rather
/// than a literal, so a backend rename cannot leave this silently
/// resolving to the default.
///
/// Pure, so unlike a namespace read off a live LIST it is table-testable.
fn cnpg_namespace_from(providers: &[ServiceProvider]) -> String {
    providers
        .iter()
        .find(|p| {
            matches!(
                Backend::from_spec_backend(&p.spec.backend),
                Some(Backend::Cloudnativepg)
            )
        })
        .and_then(|p| p.spec.config.as_ref())
        .and_then(|cfg| cfg.pointer("/namespace").and_then(Value::as_str))
        .unwrap_or(DEFAULT_CNPG_NAMESPACE)
        .to_string()
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

// ---------------------------------------------------------------------------
// The sweep — the I/O that drives the predicate above
// ---------------------------------------------------------------------------

/// How often the sweep runs. The reap itself is gated by `dwell` (minutes),
/// so the tick only bounds how promptly an elapsed dwell is noticed; a
/// minute is far below any dwell worth configuring and costs three LISTs.
const TICK: Duration = Duration::from_secs(60);

/// Metric `backend` label for a candidate — the `backend` dimension of
/// `apprafter_shared_backend_reap_total`.
fn backend_label(target: &Target) -> &'static str {
    match target {
        Target::Dragonfly {
            class: PoolClass::Ephemeral,
            ..
        } => "dragonfly-ephemeral",
        Target::Dragonfly {
            class: PoolClass::Persistent,
            ..
        } => "dragonfly-persistent",
        Target::Cnpg { .. } => "cnpg",
    }
}

/// Metric `result` label for a veto.
fn veto_label(reason: VetoReason) -> &'static str {
    match reason {
        VetoReason::Live => "veto_live",
        VetoReason::Intent => "veto_intent",
        VetoReason::Retained => "veto_retained",
    }
}

/// The candidate's own `metadata.name`.
fn target_name(target: &Target) -> &str {
    match target {
        Target::Dragonfly { name, .. } | Target::Cnpg { name } => name,
    }
}

/// Key under which a candidate's dwell clock is held.
///
/// BACKEND-QUALIFIED, not the bare name: the dwell map is shared by every
/// backend arm, and a CNPG provider whose `config./cluster` happened to
/// name a string in the `platform-redis-<class>-<index>` shape would
/// otherwise share one clock with a Dragonfly pool instance — two
/// different instances, one timer, and whichever emptied first could reap
/// the other early. Qualifying the key costs a `format!` per candidate per
/// tick and removes the class of bug entirely.
fn dwell_key(target: &Target) -> String {
    format!("{}/{}", backend_label(target), target_name(target))
}

/// Did this LIST come back truncated (a non-empty `continue` token)?
///
/// LOAD-BEARING. A success-but-partial LIST is the one realistic path to a
/// false "empty": it omits objects with no error and no signal other than
/// this token, and it omits live `ResourceClaim`s and `RetainedClaim`s with
/// equal silence. Every list the predicate reasons from is checked with
/// this, and a `true` aborts the pass.
fn list_truncated(meta: &ListMeta) -> bool {
    meta.continue_.as_deref().is_some_and(|c| !c.is_empty())
}

/// The cluster-wide veto sets one sweep pass reasons from.
///
/// Gathered ONCE per pass and shared by every backend arm, so all arms
/// judge against the same instant of cluster state rather than each
/// re-listing and drifting apart. `providers` is carried alongside `idx`
/// because arms need the raw list too (namespace resolution), and re-listing
/// it would reintroduce exactly the second, unchecked snapshot this struct
/// exists to prevent.
struct SweepInputs {
    live: Vec<ResourceClaim>,
    retained: Vec<RetainedClaim>,
    providers: Vec<ServiceProvider>,
    idx: ProviderIndex,
    /// The instant the pass began — the clock every candidate in it is
    /// measured against.
    now: Instant,
}

/// Run the shared-backend reaper (ADR 0042 §9) forever.
///
/// # An interval task, NOT a kube-rs `Controller`
///
/// This shape is deliberate, and there are two independent reasons for it.
///
/// **A destructive sweeper must not own a reflector store.** The predicate
/// this task drives is a NEGATIVE — "nothing in the cluster references this
/// instance" — and a stale cache reads as emptiness. Every other controller
/// in this crate reconciles a positive (an object exists, make it so), where
/// a stale store costs a late reconcile; here it costs a live database. A
/// `Controller` would hand this task exactly the cache it must not have.
///
/// **A `Controller` would watch-storm where the CRDs are not served.** The
/// foreign `dragonflydb.io` / `postgresql.cnpg.io` CRDs are cluster-optional
/// (absent on kind/e2e without the component). A watch against an unserved
/// resource retries forever; a LIST simply 404s once per tick and is
/// tolerated.
pub async fn run(ctx: Arc<Context>, dwell: Duration) -> Result<(), ReconcileError> {
    info!(
        tick_secs = TICK.as_secs(),
        dwell_secs = dwell.as_secs(),
        "SharedBackendReaper loop starting"
    );
    // In-memory ONLY. A restart forgets the dwell and costs one extra dwell —
    // it fails in the safe direction. Do NOT persist it by annotating a
    // foreign CR: that is an SSA write into an object another operator owns,
    // on every tick, and it would make the reap decision depend on a write
    // succeeding.
    let mut empty_since: HashMap<String, Instant> = HashMap::new();
    loop {
        if let Err(err) = sweep(&ctx, dwell, &mut empty_since).await {
            warn!(%err, "SharedBackendReaper sweep failed — retrying next tick");
            ctx.metrics
                .shared_backend_reap_total
                .with_label_values(&["unknown", "error"])
                .inc();
        }
        tokio::time::sleep(TICK).await;
    }
}

/// One sweep pass: gather the shared veto sets, then run each backend arm
/// against them.
async fn sweep(
    ctx: &Arc<Context>,
    dwell: Duration,
    empty_since: &mut HashMap<String, Instant>,
) -> Result<(), ReconcileError> {
    // LOAD-BEARING INVARIANT 1 — every read is LIVE. This task deliberately
    // owns no reflector store: the predicate is a NEGATIVE ("nothing
    // references this"), so a stale cache reads as emptiness and deletes a
    // live backend. Never swap these for a store "because it is faster".
    let live = Api::<ResourceClaim>::all(ctx.client.clone())
        .list(&Default::default())
        .await?;
    let retained = Api::<RetainedClaim>::all(ctx.client.clone())
        .list(&Default::default())
        .await?;
    let providers = Api::<ServiceProvider>::all(ctx.client.clone())
        .list(&Default::default())
        .await?;

    // LOAD-BEARING INVARIANT 2 — a success-but-partial LIST is the one
    // realistic path to a false "empty". Abort the pass rather than reason
    // from a truncated veto set.
    //
    // NOTE the deliberate asymmetry with the arm-level truncation check: this
    // one returns BEFORE `empty_since.retain` below, so every dwell clock
    // survives an aborted pass; an arm's own check returns after, so its
    // clocks are dropped. Both fail safe (the first resumes where it left
    // off, the second costs one extra dwell), and the difference is only that
    // this abort happens before any arm has had a chance to report what it
    // saw — retaining on an empty `seen` here would wipe every clock in the
    // cluster on a single truncated read.
    for (what, meta) in [
        ("resourceclaims", &live.metadata),
        ("retainedclaims", &retained.metadata),
        ("serviceproviders", &providers.metadata),
    ] {
        if list_truncated(meta) {
            warn!(list = what, "LIST truncated — skipping this reap pass");
            return Ok(());
        }
    }

    // LOAD-BEARING INVARIANT 3 — every error above propagated via `?` and
    // aborted the pass. An error is NEVER read as emptiness.
    //
    // `providers.items` is fed in APISERVER ORDER — deliberately not sorted,
    // deduped or filtered first. `provider_index`'s collision tie-break is
    // first-wins specifically to match `find_provider` (reconcile.rs), which
    // resolves with `.find(...)` over the same list order; reordering here
    // would silently break that equivalence and resolve the reaper to a
    // different cluster than the provisioner used.
    let idx = provider_index(&providers.items);
    let inputs = SweepInputs {
        live: live.items,
        retained: retained.items,
        providers: providers.items,
        idx,
        // One clock reading for the whole pass, so every candidate in it is
        // judged against the same instant — as `SweepInputs` claims.
        now: Instant::now(),
    };

    // Each arm RETURNS the set of dwell keys it examined, and `sweep` unions
    // them. This is a type-enforced contract on purpose: the earlier shape
    // passed a `&mut BTreeSet` for arms to fill, and an arm that forgot to
    // fill it failed SILENTLY — its clocks were wiped on every tick, so its
    // instances could never reach the dwell, with no error, no metric and no
    // log to say so. Returning the set makes forgetting impossible.
    //
    // The arms are UNCOUPLED, deliberately: neither is `?`-ed until BOTH
    // have run. A `?` on the first arm would abort the pass before the
    // second ever executed, and the realistic trigger is not a transient but
    // a 403 from missing `dragonflydb.io` RBAC — which recurs every tick, so
    // CNPG would never be reaped at all, permanently, while the metric
    // reported only `unknown/error` with nothing to say a different backend
    // was the collateral damage. Invariant 3 requires that an error not be
    // read as emptiness FOR THE RESOURCE THAT ERRORED; it does not require
    // one backend's failure to veto another.
    //
    // They run SEQUENTIALLY rather than joined: both take `&mut empty_since`,
    // and they share that map by design (`dwell_key` is backend-qualified
    // precisely so they can).
    let dragonfly = sweep_dragonfly(ctx, &inputs, dwell, empty_since).await;
    let cnpg = sweep_cnpg(ctx, &inputs, dwell, empty_since).await;

    // Union what each arm reported. An arm that ERRORED contributes nothing,
    // so its clocks are dropped below — correct: an arm that could not read
    // its instances cannot vouch for any of them, and the cost is one extra
    // dwell once it recovers. The other arm's clocks are unaffected, which is
    // the whole point of not `?`-ing above. (`.flatten()` over the two
    // `Result`s is what yields only the `Ok` sets.)
    let mut seen: BTreeSet<String> = BTreeSet::new();
    for keys in [dragonfly.as_ref(), cnpg.as_ref()].into_iter().flatten() {
        seen.extend(keys.iter().cloned());
    }

    // An arm that bailed early (CRD not served, or its own LIST truncated)
    // reports an empty set, so this drops its clocks. That is correct for
    // "CRD not served" (there are no instances to time) and costs one extra
    // dwell for a truncated list — the same cost, in the same safe direction,
    // that this loop already accepts on an operator restart. Deliberate, not
    // an oversight; see the note on the top-level truncation check above.
    empty_since.retain(|key, _| seen.contains(key));

    // Only NOW propagate. Dragonfly first so a simultaneous double failure
    // reports deterministically rather than by whichever the compiler
    // evaluated first — which means the CNPG error is about to be dropped
    // unseen, so log it before it goes. A double failure is the case most
    // worth reading (it usually means the apiserver, not a backend), and it
    // is the one where the returned error is least representative.
    if dragonfly.is_err() {
        if let Err(err) = &cnpg {
            warn!(%err, "CNPG arm also failed this pass — only the Dragonfly error is returned");
        }
    }
    dragonfly?;
    cnpg?;
    Ok(())
}

/// The Dragonfly arm: reap tenantless pool instances (ADR 0042 §9.2 — the
/// Dragonfly teardown is just the delete; the volume asymmetry to engineer
/// around is CNPG's alone).
async fn sweep_dragonfly(
    ctx: &Arc<Context>,
    inputs: &SweepInputs,
    dwell: Duration,
    empty_since: &mut HashMap<String, Instant>,
) -> Result<BTreeSet<String>, ReconcileError> {
    let mut seen: BTreeSet<String> = BTreeSet::new();
    // Resolved from the providers this pass ALREADY listed and truncation-
    // checked — not a second `redis_namespace(ctx)` call, which would read a
    // different apiserver snapshot, unchecked, on a path that deletes.
    let df_ns = redis_namespace_from(&inputs.providers);
    let api: Api<DynamicObject> =
        Api::namespaced_with(ctx.client.clone(), &df_ns, &dragonfly_cluster_ar());

    // GATE 1 of 2 — only instances THIS operator stamped are candidates
    // (`dragonfly_object` sets the label; `admin_secret_object` uses the same
    // one). Name-parsing below is gate 2. Two independent gates is the whole
    // point: `platform-redis-<class>-<index>` is not a reserved namespace of
    // names, so on the name alone a user's own `Dragonfly` in this namespace
    // would be a candidate. An instance created before the stamp existed is
    // simply not a candidate until the provisioner's next SSA apply stamps
    // it — the safe direction, and it self-heals on the next reconcile.
    let selector = ListParams::default().labels(&managed_by_selector());
    let dragonflies = match api.list(&selector).await {
        Ok(list) => list,
        // The dragonflydb.io CRD is cluster-OPTIONAL — absent on kind/e2e
        // without the component. Nothing to reap is not a failure, and it
        // must not be logged as one on every tick. Any OTHER error aborts
        // the pass (invariant 3: an error is never emptiness).
        Err(kube::Error::Api(e)) if e.code == 404 => {
            debug!(namespace = %df_ns, "dragonflydb.io CRD not served — nothing to reap");
            return Ok(seen);
        }
        Err(e) => return Err(e.into()),
    };
    // Invariant 2 again — a truncated instance list would hide instances,
    // which is harmless, but it is also the signal that this apiserver is
    // paginating our reads, so the veto sets above cannot be trusted either.
    if list_truncated(&dragonflies.metadata) {
        warn!(
            list = "dragonflies",
            "LIST truncated — skipping this reap pass"
        );
        return Ok(seen);
    }

    let now = inputs.now;

    for obj in &dragonflies.items {
        let name = obj.name_any();
        // GATE 2 of 2 — a name that does not parse is NOT ours even if it
        // carries our label. Skip it silently and never touch it.
        let Some(class) = class_of_instance(&name) else {
            continue;
        };
        // GATE 1 again, per object — see [`is_stamped`]. The LIST selector
        // above already filtered on this server-side; re-checking here is what
        // keeps the gate from being deleted along with one call argument.
        if !is_stamped(obj) {
            continue;
        }
        // Already being deleted — leave it alone. Without this an instance
        // stuck in graceful deletion cycles forever: Reap → the delete is a
        // no-op → clock cleared → Dwell → the dwell elapses → Reap again,
        // logging "reaped" and incrementing the `reaped` counter once per
        // dwell, indefinitely. That corrupts the one number anyone will use
        // to ask whether the reaper did something it should not have.
        if obj.metadata.deletion_timestamp.is_some() {
            continue;
        }
        let target = Target::Dragonfly {
            name: name.clone(),
            class,
        };
        let key = dwell_key(&target);
        seen.insert(key.clone());

        let empty_for = empty_since.get(&key).map(|t| now.duration_since(*t));
        match reap_decision(
            &target,
            &inputs.live,
            &inputs.retained,
            &inputs.idx,
            empty_for,
            dwell,
        ) {
            Decision::Veto(reason) => {
                // Something points at it again — drop any clock so a later
                // emptying starts a FULL dwell rather than resuming a stale
                // one.
                empty_since.remove(&key);
                // `debug!`, NOT `info!`: this fires every tick for every
                // instance that has a tenant, i.e. constantly on any healthy
                // cluster running an app.
                debug!(
                    instance = %name, ?class, ?reason,
                    "shared-backend instance still referenced — not reaping"
                );
                ctx.metrics
                    .shared_backend_reap_total
                    .with_label_values(&[backend_label(&target), veto_label(reason)])
                    .inc();
            }
            Decision::Dwell => {
                // `info!` ONCE, on the tick the dwell starts — not on every
                // tick it continues, which would be a line per instance per
                // minute for the whole dwell.
                if let Entry::Vacant(slot) = empty_since.entry(key.clone()) {
                    slot.insert(now);
                    info!(
                        instance = %name, ?class, dwell_secs = dwell.as_secs(),
                        "shared-backend instance has no tenants — starting dwell"
                    );
                }
                ctx.metrics
                    .shared_backend_reap_total
                    .with_label_values(&[backend_label(&target), "dwelling"])
                    .inc();
            }
            Decision::Reap => {
                // No uid ⇒ NO PRECONDITION AT ALL. `Preconditions { uid: None,
                // resource_version: None }` is not a weaker precondition, it is
                // an unconditional delete: the name-reuse race below would then
                // kill whatever now holds the name. Refuse the candidate and
                // leave the clock so the next tick retries. (The apiserver
                // always sets a uid — this is a "cannot happen" that must still
                // not fall through to a delete.)
                let Some(uid) = obj.metadata.uid.clone().filter(|u| !u.is_empty()) else {
                    warn!(
                        instance = %name, ?class,
                        "pool instance has no uid — cannot precondition the delete; not reaping"
                    );
                    continue;
                };

                // Delete under a uid precondition, so a name-reuse race —
                // the instance we judged was deleted and re-created by a
                // fresh provision between our LIST and this call — fails
                // with 409 instead of killing the new instance.
                let params = DeleteParams {
                    preconditions: Some(Preconditions {
                        uid: Some(uid.clone()),
                        resource_version: None,
                    }),
                    ..Default::default()
                };

                // Deletes the Dragonfly CR ONLY.
                //
                // NEVER the snapshot PVC. Measured on dragonfly-operator
                // v1.5.0: it is unowned with `persistentVolumeClaimRetentionPolicy`
                // Retain, survives the delete, and the next provision adopts it
                // by name with its data intact (ADR 0042 §9.2/§9.5). Preservation
                // is UNCONDITIONAL — a truncated LIST lies identically about live
                // and retained claims, so the reaper cannot tell a correct reap
                // from an incorrect one, and a conditional safety net is none.
                //
                // NEVER `<instance>-admin`. It is unowned by design and will not
                // cascade; deleting it would make the read-or-create in
                // `provision_dragonfly` mint a FRESH admin password on the next
                // provision while the old pod may still be Terminating behind the
                // Service — an admin auth failure with no upside.
                //
                // The restriction is enforced ONLY here. RBAC grants
                // `persistentvolumeclaims: delete` and `secrets: delete` for
                // other arms of this crate and will not stop the mistake.
                match api.delete(&name, &params).await {
                    Ok(_) => {
                        empty_since.remove(&key);
                        // Structurally 0 — `reap_decision` returns `Reap` only
                        // after both vetoes failed to fire. Asserting rather
                        // than logging them: they can only ever catch a
                        // plumbing regression (an arm passing veto sets that
                        // are not the ones the decision was made from), which
                        // an assert expresses precisely and which costs
                        // nothing in a release build.
                        debug_assert_eq!(
                            inputs
                                .live
                                .iter()
                                .filter(|c| is_allocated_on(c, &target, &inputs.idx))
                                .count(),
                            0,
                            "reaped {name} while a live claim was allocated on it"
                        );
                        debug_assert_eq!(
                            inputs
                                .live
                                .iter()
                                .filter(|c| is_intent_for(c, &target, &inputs.idx))
                                .count(),
                            0,
                            "reaped {name} while a claim carried intent toward it"
                        );
                        // NOT structurally 0, and that is why it is logged:
                        // on an ephemeral instance a snapshot may name it and
                        // deliberately not veto (the §9.7 exception). This
                        // makes that exception visible at the moment it is
                        // exercised rather than leaving it implied.
                        let retained_naming = inputs
                            .retained
                            .iter()
                            .filter(|r| r.spec.instance.as_deref() == Some(name.as_str()))
                            .count();
                        info!(
                            instance = %name,
                            ?class,
                            %uid,
                            dwell_secs = dwell.as_secs(),
                            empty_secs = empty_for.map(|d| d.as_secs()).unwrap_or_default(),
                            retained = retained_naming,
                            "reaped tenantless shared-backend instance"
                        );
                        ctx.metrics
                            .shared_backend_reap_total
                            .with_label_values(&[backend_label(&target), "reaped"])
                            .inc();
                    }
                    // 409 = the uid precondition failed (name reused under a
                    // new instance); 404 = it went away under us. Both mean
                    // the thing we judged is not the thing that is there now,
                    // so the reap is abandoned and the clock reset — the next
                    // pass re-judges whatever is there on its own merits.
                    Err(kube::Error::Api(e)) if e.code == 409 || e.code == 404 => {
                        empty_since.remove(&key);
                        warn!(
                            instance = %name, ?class, code = e.code,
                            "reap abandoned — instance changed identity under us; re-judged next tick"
                        );
                        ctx.metrics
                            .shared_backend_reap_total
                            .with_label_values(&[backend_label(&target), "veto_uid_conflict"])
                            .inc();
                    }
                    Err(e) => return Err(e.into()),
                }
            }
        }
    }
    Ok(seen)
}

// ---------------------------------------------------------------------------
// The CNPG arm — and the §9.3 ownerReference strip it cannot delete without
// ---------------------------------------------------------------------------

/// Indices into `meta.ownerReferences` of every entry whose `uid` is `uid`,
/// in DESCENDING order.
///
/// **Matching on UID and not on name is the whole point.** A `Cluster` that
/// was deleted and re-created keeps its name and gets a fresh uid, and CNPG
/// re-adds the reference pointing at the NEW uid when it re-adopts the
/// volume (ADR 0042 §9.5). Matching by name would strip the live cluster's
/// reference off a volume it legitimately owns, turning the reaper into a
/// generator of orphaned PVCs.
///
/// Descending so the caller can emit one `remove` op per index without an
/// earlier removal shifting the indices of the later ones.
///
/// An EMPTY `uid` matches nothing, ever. The caller already refuses to
/// judge a candidate without a uid, but a helper that would happily select
/// every reference with an absent uid is not one to leave loaded.
fn owner_ref_indices(meta: &ObjectMeta, uid: &str) -> Vec<usize> {
    if uid.is_empty() {
        return Vec::new();
    }
    let Some(refs) = meta.owner_references.as_ref() else {
        return Vec::new();
    };
    refs.iter()
        .enumerate()
        .filter(|(_, r)| r.uid == uid)
        .map(|(i, _)| i)
        .rev()
        .collect()
}

/// RFC 6902 ops that remove exactly the `ownerReferences` entries pointing
/// at `uid` — and nothing else.
///
/// Each `remove` is preceded by a `test` on the uid at that exact index. A
/// JSON Patch is applied op-by-op and a failed `test` rejects the WHOLE
/// patch (422), so if the array shifted between the read and this write the
/// patch fails loudly instead of removing whatever now sits at that index.
///
/// That is why this is a JSON Patch and not a merge-patch of the filtered
/// array: RFC 7396 replaces an array wholesale, so a merge-patch would
/// silently discard a co-owner added concurrently — the one thing the
/// "strip only OUR entry" rule exists to prevent.
fn strip_owner_ops(meta: &ObjectMeta, uid: &str) -> Vec<Value> {
    owner_ref_indices(meta, uid)
        .into_iter()
        .flat_map(|i| {
            [
                json!({
                    "op": "test",
                    "path": format!("/metadata/ownerReferences/{i}/uid"),
                    "value": uid,
                }),
                json!({
                    "op": "remove",
                    "path": format!("/metadata/ownerReferences/{i}"),
                }),
            ]
        })
        .collect()
}

/// Outcome of the §9.3 strip for one `Cluster`.
enum Strip {
    /// VERIFIED by re-read: no PVC in the namespace still carries an
    /// ownerReference to this `Cluster`. Carries how many were stripped.
    Verified(usize),
    /// NOT verified — the delete must NOT proceed.
    Unverified(StripFailure),
}

/// Why a strip did not verify.
///
/// The log reason and the metric bucket both hang off this ONE enum rather
/// than being chosen at each call site, so a new failure mode cannot be
/// added with a log string but no metric mapping (or worse, silently
/// inherit a bucket that misdescribes it).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StripFailure {
    /// A PVC still carried the ownerReference on the verifying re-read.
    /// GENUINE reattachment — something put it back.
    OwnerReattached,
    /// The PVC LIST taken before the patches came back truncated, so the
    /// set of PVCs to strip was not knowable.
    ListTruncated,
    /// The verifying re-LIST came back truncated, so the strip could not
    /// be confirmed.
    VerifyListTruncated,
    /// A LIST or PATCH errored outright, so nothing about the PVCs is
    /// known.
    ReadFailed,
}

impl StripFailure {
    /// The `result` label for `apprafter_shared_backend_reap_total`.
    ///
    /// Only GENUINE reattachment gets `veto_owner_reattached`. The other
    /// three are read failures, and calling them reattachment would make
    /// the metric assert something about CNPG's behaviour that the reaper
    /// never observed — precisely backwards in the debugging session where
    /// the difference matters. They share `veto_unverified`; the `reason`
    /// log field is what separates them there.
    fn metric_label(self) -> &'static str {
        match self {
            Self::OwnerReattached => "veto_owner_reattached",
            Self::ListTruncated | Self::VerifyListTruncated | Self::ReadFailed => "veto_unverified",
        }
    }

    /// The `reason` log field — finer-grained than the metric bucket.
    fn reason(self) -> &'static str {
        match self {
            Self::OwnerReattached => "owner_reference_present",
            Self::ListTruncated => "pvc_list_truncated",
            Self::VerifyListTruncated => "pvc_list_truncated_on_verify",
            Self::ReadFailed => "pvc_read_failed",
        }
    }
}

/// Remove this `Cluster`'s `ownerReference` from every PVC that carries it,
/// then RE-READ to confirm it is gone.
///
/// Returns [`Strip::Verified`] only when a fresh, untruncated LIST shows no
/// PVC in `ns` owned by `uid`. Anything else is [`Strip::Unverified`] and
/// the caller must abandon the reap.
async fn strip_cnpg_pvc_owners(
    ctx: &Arc<Context>,
    ns: &str,
    uid: &str,
) -> Result<Strip, ReconcileError> {
    let pvc_api: Api<DynamicObject> = Api::namespaced_with(ctx.client.clone(), ns, &pvc_ar());

    // NO label selector, unlike every other LIST in this file: the PVC is
    // CNPG's own object and carries no apprafter stamp. The ownerReference
    // uid IS the filter, and it is a tighter one than any label — it names
    // this exact `Cluster` instance.
    //
    // DO NOT NARROW THIS LIST. Not with a label selector "for efficiency",
    // not with a field selector, and check the namespace twice. The failure
    // is silent and total: a narrowed scope returns no PVCs, every one this
    // `Cluster` owns is left with its ownerReference intact, and the
    // function reports `Verified(0)` — which the caller reads as "nothing to
    // preserve, safe to delete" and the volume cascades away. The verifying
    // re-LIST below cannot catch it either, because it uses the SAME scope
    // and so agrees, confidently, that no PVC points at this cluster. There
    // is one PVC per instance on a Tier-1 cluster; the LIST is not the cost
    // worth optimising.
    let before = pvc_api.list(&ListParams::default()).await?;
    // Invariant 2, and here it is not merely a hygiene check: a truncated
    // PVC LIST would hide a PVC this `Cluster` owns, we would leave that
    // one's reference intact, and the delete would cascade it away.
    if list_truncated(&before.metadata) {
        return Ok(Strip::Unverified(StripFailure::ListTruncated));
    }

    let mut stripped = 0usize;
    for pvc in &before.items {
        let ops = strip_owner_ops(&pvc.metadata, uid);
        if ops.is_empty() {
            continue;
        }
        let patch: json_patch::Patch = serde_json::from_value(Value::Array(ops))?;
        pvc_api
            .patch(
                &pvc.name_any(),
                &PatchParams::default(),
                &Patch::Json::<()>(patch),
            )
            .await?;
        stripped += 1;
    }

    // RE-READ, and re-LIST rather than re-GET each PVC we touched: this also
    // catches a PVC that was not in the first LIST at all (created between
    // the two calls, or hidden by a partial read the truncation check did not
    // flag). The question that must be answered `false` before deleting is
    // "does ANY PVC here still point at this Cluster?", not "did my patches
    // land?".
    let after = pvc_api.list(&ListParams::default()).await?;
    if list_truncated(&after.metadata) {
        return Ok(Strip::Unverified(StripFailure::VerifyListTruncated));
    }
    if after
        .items
        .iter()
        .any(|pvc| !owner_ref_indices(&pvc.metadata, uid).is_empty())
    {
        return Ok(Strip::Unverified(StripFailure::OwnerReattached));
    }
    Ok(Strip::Verified(stripped))
}

/// The CNPG arm: reap the shared `Cluster` once nothing claims it (ADR 0042
/// §9.2/§9.3).
///
/// Structurally the Dragonfly arm, with one addition that is not cosmetic:
/// CNPG owns its PVC through a `controller: true` ownerReference, so this
/// arm must STRIP that reference and VERIFY the strip before it may delete.
/// See the comment at the strip site.
async fn sweep_cnpg(
    ctx: &Arc<Context>,
    inputs: &SweepInputs,
    dwell: Duration,
    empty_since: &mut HashMap<String, Instant>,
) -> Result<BTreeSet<String>, ReconcileError> {
    let mut seen: BTreeSet<String> = BTreeSet::new();

    // GATE 1 of 2, and the PRIMARY safety gate of this arm: a `Cluster` is a
    // candidate only if its name appears as a VALUE in the provider index,
    // i.e. some `ServiceProvider` the platform ships config points at it.
    // NEVER "every Cluster in the namespace" — a user may run their own CNPG
    // clusters there, and the only one this reaper may touch is the one the
    // platform's own config named.
    //
    // Empty set ⇒ no `cloudnative-pg` provider exists ⇒ this operator cannot
    // have created a shared cluster ⇒ nothing here is ours. Returning before
    // the LIST is the same gate, expressed once more where it cannot be
    // bypassed. NOTE this also means deleting the pg provider while its
    // cluster still runs makes that cluster permanently un-reapable — the
    // safe direction, and the same stance `is_intent_for` takes on a
    // permanently-unschedulable pg claim.
    let managed: BTreeSet<&str> = inputs
        .idx
        .cnpg_cluster
        .values()
        .map(String::as_str)
        .collect();
    if managed.is_empty() {
        return Ok(seen);
    }

    // Resolved from the providers this pass ALREADY listed and truncation-
    // checked — not a second LIST, which would be a different apiserver
    // snapshot, unchecked, on a path that deletes.
    let cnpg_ns = cnpg_namespace_from(&inputs.providers);
    let api: Api<DynamicObject> = Api::namespaced_with(ctx.client.clone(), &cnpg_ns, &cluster_ar());

    // GATE 2 of 2 — only clusters THIS operator stamped
    // (`cnpg::cluster_object` sets the label; `cnpg::basic_auth_secret` uses
    // the same one). Two INDEPENDENT gates is the whole point: neither a
    // borrowed name nor a borrowed label is enough on its own. A cluster
    // created before the stamp existed is simply not a candidate until the
    // provisioner's next SSA apply stamps it — the safe direction, and it
    // self-heals on the next reconcile of any pg claim.
    let selector = ListParams::default().labels(&managed_by_selector());
    let clusters = match api.list(&selector).await {
        Ok(list) => list,
        // The postgresql.cnpg.io CRD is cluster-OPTIONAL — absent on kind/e2e
        // without the component. Nothing to reap is not a failure, and it
        // must not be logged as one on every tick. Any OTHER error aborts
        // this arm (invariant 3: an error is never emptiness).
        Err(kube::Error::Api(e)) if e.code == 404 => {
            debug!(namespace = %cnpg_ns, "postgresql.cnpg.io CRD not served — nothing to reap");
            return Ok(seen);
        }
        Err(e) => return Err(e.into()),
    };
    // Invariant 2 again — a truncated cluster list would hide clusters, which
    // is harmless, but it is also the signal that this apiserver is
    // paginating our reads, so the veto sets above cannot be trusted either.
    if list_truncated(&clusters.metadata) {
        warn!(
            list = "clusters",
            "LIST truncated — skipping this reap pass"
        );
        return Ok(seen);
    }

    let now = inputs.now;

    for obj in &clusters.items {
        let name = obj.name_any();
        // GATE 1 again, per object — the label alone never selects a target.
        if !managed.contains(name.as_str()) {
            continue;
        }
        // GATE 2 again, per object — see [`is_stamped`]. The LIST selector
        // above already filtered on this server-side; re-checking here is what
        // keeps the gate from being deleted along with one call argument.
        if !is_stamped(obj) {
            continue;
        }
        // Already being deleted — leave it alone. Without this a cluster
        // stuck in graceful deletion cycles forever: Reap → the delete is a
        // no-op → clock cleared → Dwell → the dwell elapses → Reap again,
        // once per dwell, indefinitely, corrupting the one number anyone will
        // use to ask whether the reaper did something it should not have.
        if obj.metadata.deletion_timestamp.is_some() {
            continue;
        }
        let target = Target::Cnpg { name: name.clone() };
        let key = dwell_key(&target);
        seen.insert(key.clone());

        let empty_for = empty_since.get(&key).map(|t| now.duration_since(*t));
        match reap_decision(
            &target,
            &inputs.live,
            &inputs.retained,
            &inputs.idx,
            empty_for,
            dwell,
        ) {
            Decision::Veto(reason) => {
                // Something points at it again — drop any clock so a later
                // emptying starts a FULL dwell rather than resuming a stale
                // one.
                empty_since.remove(&key);
                // `debug!`, NOT `info!`: this fires every tick for every
                // cluster that has a tenant, i.e. constantly on any healthy
                // cluster running an app with `needs.pg`.
                debug!(
                    cluster = %name, namespace = %cnpg_ns, ?reason,
                    "shared CNPG cluster still referenced — not reaping"
                );
                ctx.metrics
                    .shared_backend_reap_total
                    .with_label_values(&[backend_label(&target), veto_label(reason)])
                    .inc();
            }
            Decision::Dwell => {
                // `info!` ONCE, on the tick the dwell starts.
                if let Entry::Vacant(slot) = empty_since.entry(key.clone()) {
                    slot.insert(now);
                    info!(
                        cluster = %name, namespace = %cnpg_ns, dwell_secs = dwell.as_secs(),
                        "shared CNPG cluster has no tenants — starting dwell"
                    );
                }
                ctx.metrics
                    .shared_backend_reap_total
                    .with_label_values(&[backend_label(&target), "dwelling"])
                    .inc();
            }
            Decision::Reap => {
                // No uid ⇒ no uid-matched strip and no delete precondition.
                // Both are load-bearing here, so this candidate is not
                // judgeable; leave the clock so the next tick retries. (The
                // apiserver always sets a uid — this is a "cannot happen"
                // that must still not fall through to a delete.)
                let Some(uid) = obj.metadata.uid.clone().filter(|u| !u.is_empty()) else {
                    warn!(
                        cluster = %name, namespace = %cnpg_ns,
                        "CNPG cluster has no uid — cannot strip or precondition; not reaping"
                    );
                    continue;
                };

                // The CNPG instance PVC carries a `controller: true` ownerReference to the
                // Cluster, so a plain delete CASCADES and destroys the database. Measured on
                // the shipped chart cloudnative-pg-0.28.2 / app 1.29.1: an unstripped delete
                // left `No resources found` and a marker row written beforehand was gone.
                //
                // Stripping first converts that into a preserved volume: the pod and initdb
                // Job still cascade (the memory is genuinely reclaimed) while the PVC survives
                // Bound, and the next provision adopts it by name with the data intact.
                // CNPG does not re-add the reference — verified at t=30/60/90s and after a
                // forced reconcile. `--cascade=orphan` was tested and REJECTED: it orphans the
                // pod too, which stays Running and reclaims nothing.
                //
                // Preservation is UNCONDITIONAL (ADR 0042 §9.4). Do not make it depend on the
                // reap looking "clean": a truncated LIST lies identically about live claims and
                // retained ones, so the reaper cannot tell a correct reap from an incorrect
                // one, and a conditional safety net is no safety net.
                //
                // This arm NEVER deletes a PersistentVolumeClaim. RBAC grants
                // persistentvolumeclaims:delete for other arms of this crate and will not stop
                // the mistake — this comment and code review are the only guard.
                // NO `?` ON THIS CALL, and it must never grow one. A `?` here
                // does not merely lose the backend attribution — it returns
                // `Err` from the whole arm, so the arm contributes NO keys to
                // `seen`, so `empty_since.retain` in `sweep` wipes EVERY CNPG
                // dwell clock. The cycle is then: Dwell → clock set → dwell
                // elapses → Reap → strip errors → clock wiped → Dwell, forever.
                // The cluster is never reaped, and the only signal is
                // `unknown/error`. That is precisely the arm-coupling failure
                // the two arms were uncoupled to prevent, re-created one level
                // down and confined to a single backend.
                //
                // So an I/O failure is folded into the SAME `Unverified` path
                // as a reattached reference: it is one more way of not being
                // able to establish preservation, and every one of them means
                // "do not delete, keep the clock, retry next tick". Realistic
                // triggers are a 422 from a lost `test` race and 5xx; RBAC does
                // grant `persistentvolumeclaims: patch`, so 403 is unlikely.
                let outcome = strip_cnpg_pvc_owners(ctx, &cnpg_ns, &uid)
                    .await
                    .unwrap_or_else(|err| {
                        // Logged HERE because this is the only place the
                        // underlying kube error still exists — the arm below
                        // sees a reason code, not a cause.
                        warn!(
                            cluster = %name, namespace = %cnpg_ns, %uid, %err,
                            "CNPG PVC ownerReference strip could not complete"
                        );
                        Strip::Unverified(StripFailure::ReadFailed)
                    });

                let stripped = match outcome {
                    Strip::Verified(n) => n,
                    // The strip did not verify. ABORT this candidate: do not
                    // delete, and leave the dwell clock ALONE so the next tick
                    // retries rather than restarting the whole dwell.
                    Strip::Unverified(failure) => {
                        warn!(
                            cluster = %name, namespace = %cnpg_ns, %uid,
                            reason = failure.reason(),
                            "CNPG PVC ownerReference strip did not verify — NOT reaping \
                             (deleting now would cascade the volume away)"
                        );
                        ctx.metrics
                            .shared_backend_reap_total
                            .with_label_values(&[backend_label(&target), failure.metric_label()])
                            .inc();
                        continue;
                    }
                };

                // FOLLOW-UP (not implemented, deliberately): a post-delete
                // PVC-survival check.
                //
                // The verify above closes the window it can. One window it
                // cannot close: if a future CNPG version SSA-applies its PVC
                // with `ownerReferences` in the apply body, SSA RESTORES the
                // reference — including in the gap between the verify and this
                // delete. No code-level defence closes that gap, because the
                // last read before a delete is always stale by the time the
                // delete lands.
                //
                // What would help is a re-LIST AFTER the delete that `warn!`s
                // if a PVC we stripped now carries a `deletionTimestamp`. It
                // cannot prevent the loss — by then the GC has it — but it
                // converts silent destruction into an attributable signal, and
                // it is the only thing that would tell us §9.3's measured "CNPG
                // does not re-add it" had stopped holding. Left out for now
                // because it adds a LIST per reap to a path that runs rarely
                // and the e2e walk (Task 9) asserts PVC survival directly; the
                // reasoning is recorded here so the next person finds it rather
                // than rediscovering it.
                //
                // Delete under a uid precondition, so a name-reuse race — the
                // cluster we judged was deleted and re-created by a fresh
                // provision between our LIST and this call — fails with 409
                // instead of killing the new one.
                let params = DeleteParams {
                    preconditions: Some(Preconditions {
                        uid: Some(uid.clone()),
                        resource_version: None,
                    }),
                    ..Default::default()
                };

                match api.delete(&name, &params).await {
                    Ok(_) => {
                        empty_since.remove(&key);
                        // Structurally 0 — `reap_decision` returns `Reap` only
                        // after all three vetoes failed to fire. Asserting
                        // rather than logging: they can only ever catch a
                        // plumbing regression (an arm passing veto sets that
                        // are not the ones the decision was made from), which
                        // an assert expresses precisely and which costs
                        // nothing in a release build.
                        //
                        // Note the third one is an assert here where the
                        // Dragonfly arm logs a count: `is_retained_for` has no
                        // §9.7 exception for CNPG, so a snapshot naming this
                        // cluster ALWAYS vetoes and can never coexist with a
                        // reap.
                        debug_assert_eq!(
                            inputs
                                .live
                                .iter()
                                .filter(|c| is_allocated_on(c, &target, &inputs.idx))
                                .count(),
                            0,
                            "reaped {name} while a live claim was allocated on it"
                        );
                        debug_assert_eq!(
                            inputs
                                .live
                                .iter()
                                .filter(|c| is_intent_for(c, &target, &inputs.idx))
                                .count(),
                            0,
                            "reaped {name} while a claim carried intent toward it"
                        );
                        debug_assert_eq!(
                            inputs
                                .retained
                                .iter()
                                .filter(|r| is_retained_for(r, &target))
                                .count(),
                            0,
                            "reaped {name} while a RetainedClaim snapshot named it"
                        );
                        info!(
                            cluster = %name,
                            namespace = %cnpg_ns,
                            %uid,
                            pvcs_preserved = stripped,
                            dwell_secs = dwell.as_secs(),
                            empty_secs = empty_for.map(|d| d.as_secs()).unwrap_or_default(),
                            "reaped tenantless shared CNPG cluster — its PVCs were preserved"
                        );
                        ctx.metrics
                            .shared_backend_reap_total
                            .with_label_values(&[backend_label(&target), "reaped"])
                            .inc();
                    }
                    // 409 = the uid precondition failed (name reused under a
                    // new cluster); 404 = it went away under us. Both mean the
                    // thing we judged is not the thing that is there now, so
                    // the reap is abandoned and the clock reset.
                    //
                    // Both leave a stripped PVC behind. On 409 that self-heals
                    // — the new cluster adopts the volume and CNPG re-adds its
                    // own ownerReference (§9.5). On 404 the volume is simply
                    // unowned, which is the state §9.3 was aiming for anyway.
                    Err(kube::Error::Api(e)) if e.code == 409 || e.code == 404 => {
                        empty_since.remove(&key);
                        warn!(
                            cluster = %name, namespace = %cnpg_ns, code = e.code,
                            "reap abandoned — cluster changed identity under us; re-judged next tick"
                        );
                        ctx.metrics
                            .shared_backend_reap_total
                            .with_label_values(&[backend_label(&target), "veto_uid_conflict"])
                            .inc();
                    }
                    // A delete that failed AFTER a successful strip. This is
                    // the one bad state this ordering can leave, and it is
                    // logged loudly because nothing else will surface it: the
                    // cluster is LIVE and its PVC is now UNOWNED, so it will
                    // LEAK rather than cascade if a human deletes the cluster
                    // later. It does not self-heal on a running cluster —
                    // CNPG re-adds the reference only when it re-adopts a
                    // volume for a re-created cluster (§9.5), and the measured
                    // behaviour is that it never re-adds it to a live one.
                    // Small and recoverable (re-add the ownerReference, or
                    // delete the PVC by hand once the data is not wanted), but
                    // it must be visible.
                    Err(e) => {
                        warn!(
                            cluster = %name, namespace = %cnpg_ns, %uid,
                            pvcs_stripped = stripped, %e,
                            "CNPG cluster delete FAILED after its PVC ownerReference was \
                             stripped — the cluster is LIVE with an UNOWNED PVC, which will \
                             LEAK instead of cascading if it is deleted by hand later \
                             (ADR 0042 §9.3)"
                        );
                        return Err(e.into());
                    }
                }
            }
        }
    }
    Ok(seen)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use k8s_openapi::apimachinery::pkg::apis::meta::v1::{OwnerReference, Time};
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

    // --- 19/20/21/22: the sweep's pure helpers ---

    #[test]
    fn backend_label_distinguishes_every_target_kind() {
        // These strings are the `backend` dimension of
        // `apprafter_shared_backend_reap_total` and are declared in
        // `operator_core::metrics`. A rename here silently splits a metric
        // series in two, so the mapping is pinned exactly.
        assert_eq!(
            backend_label(&dragonfly_target(EPHEMERAL_000, PoolClass::Ephemeral)),
            "dragonfly-ephemeral"
        );
        assert_eq!(
            backend_label(&dragonfly_target(PERSISTENT_000, PoolClass::Persistent)),
            "dragonfly-persistent"
        );
        assert_eq!(backend_label(&cnpg_target(CLUSTER)), "cnpg");
    }

    #[test]
    fn veto_label_maps_every_reason() {
        assert_eq!(veto_label(VetoReason::Live), "veto_live");
        assert_eq!(veto_label(VetoReason::Intent), "veto_intent");
        assert_eq!(veto_label(VetoReason::Retained), "veto_retained");
    }

    #[test]
    fn dwell_key_is_backend_qualified_so_backends_cannot_share_a_clock() {
        // The dwell map is shared by every backend arm. A CNPG cluster whose
        // name collided with a Dragonfly pool instance's must NOT share its
        // clock — two instances on one timer means whichever empties first
        // can reap the other early.
        let collide = "platform-redis-ephemeral-000";
        let df = dwell_key(&dragonfly_target(collide, PoolClass::Ephemeral));
        let pg = dwell_key(&cnpg_target(collide));
        assert_ne!(df, pg);
        assert_eq!(df, "dragonfly-ephemeral/platform-redis-ephemeral-000");
        assert_eq!(pg, "cnpg/platform-redis-ephemeral-000");
        // The same class + name is stable across calls — the key is what the
        // clock is stored under, so an unstable key would never accumulate a
        // dwell at all.
        assert_eq!(
            df,
            dwell_key(&dragonfly_target(collide, PoolClass::Ephemeral))
        );
        // The two dragonfly classes are also distinct keys.
        assert_ne!(
            dwell_key(&dragonfly_target(collide, PoolClass::Ephemeral)),
            dwell_key(&dragonfly_target(collide, PoolClass::Persistent))
        );
    }

    #[test]
    fn list_truncated_detects_only_a_non_empty_continue_token() {
        // A complete LIST carries no continue token; the apiserver also
        // returns an EMPTY string rather than null in some encodings, and
        // that is complete too. Treating "" as truncated would abort every
        // pass; treating a real token as complete would reason from a
        // partial veto set — the failure this guard exists for.
        assert!(!list_truncated(&ListMeta::default()));
        assert!(!list_truncated(&ListMeta {
            continue_: Some(String::new()),
            ..Default::default()
        }));
        assert!(list_truncated(&ListMeta {
            continue_: Some("eyJ2IjoibWV0YS5rOHMuaW8vdjEi".into()),
            ..Default::default()
        }));
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

    // --- 23/24: cnpg_namespace_from() ---

    #[test]
    fn cnpg_namespace_from_reads_the_config_override() {
        assert_eq!(
            cnpg_namespace_from(&[provider(
                "pg-integrated",
                "pg",
                "cloudnative-pg",
                Some(json!({ "cluster": CLUSTER, "namespace": "tenant-cnpg" })),
            )]),
            "tenant-cnpg"
        );
    }

    #[test]
    fn cnpg_namespace_from_falls_back_to_the_well_known_default() {
        // No providers at all, a cnpg provider with no config, one whose
        // config omits `/namespace`, and a cluster whose only provider is a
        // backend this controller does not drive all resolve to the default.
        // The resolution must match `provision_cloudnativepg`'s inline
        // fallback — a namespace resolved differently is one the reaper finds
        // no clusters in, so the arm would silently reap nothing.
        assert_eq!(cnpg_namespace_from(&[]), "cnpg-system");
        assert_eq!(
            cnpg_namespace_from(&[provider("pg-integrated", "pg", "cloudnative-pg", None)]),
            "cnpg-system"
        );
        assert_eq!(
            cnpg_namespace_from(&[provider(
                "pg-integrated",
                "pg",
                "cloudnative-pg",
                Some(json!({ "cluster": CLUSTER })),
            )]),
            "cnpg-system"
        );
        assert_eq!(
            cnpg_namespace_from(&[provider("redis-integrated", "redis", "dragonfly", None)]),
            "cnpg-system"
        );
    }

    #[test]
    fn cnpg_namespace_from_takes_the_first_cnpg_provider_in_list_order() {
        // Same first-wins tie-break `provider_index` and `find_provider` use,
        // over the same apiserver-ordered list.
        let ns = cnpg_namespace_from(&[
            provider(
                "pg-a",
                "pg",
                "cloudnative-pg",
                Some(json!({ "namespace": "first-cnpg" })),
            ),
            provider(
                "pg-b",
                "pg",
                "cloudnative-pg",
                Some(json!({ "namespace": "second-cnpg" })),
            ),
        ]);
        assert_eq!(ns, "first-cnpg");
    }

    // --- 25..29: the §9.3 ownerReference strip's pure selection ---

    /// The uid of the `Cluster` being reaped in the strip tests.
    const CLUSTER_UID: &str = "dcd48d90-5e54-4afd-850f-4d0857bd06a3";

    fn owner(uid: &str, name: &str, controller: bool) -> OwnerReference {
        OwnerReference {
            api_version: "postgresql.cnpg.io/v1".into(),
            kind: "Cluster".into(),
            name: name.into(),
            uid: uid.into(),
            controller: Some(controller),
            block_owner_deletion: None,
        }
    }

    fn meta_owned_by(owners: Vec<OwnerReference>) -> ObjectMeta {
        ObjectMeta {
            name: Some("platform-postgres-1".into()),
            namespace: Some("cnpg-system".into()),
            owner_references: Some(owners),
            ..Default::default()
        }
    }

    /// A PVC document shaped the way the apiserver returns it, so the ops
    /// under test can actually be APPLIED rather than merely inspected.
    fn pvc_doc(meta: &ObjectMeta) -> Value {
        json!({
            "apiVersion": "v1",
            "kind": "PersistentVolumeClaim",
            "metadata": serde_json::to_value(meta).unwrap(),
        })
    }

    #[test]
    fn owner_ref_indices_matches_on_uid_not_name() {
        // The recreate case, and the reason this selects on uid: a `Cluster`
        // deleted and re-created keeps its NAME and gets a fresh uid, and CNPG
        // re-adds the reference under the new one. Matching by name would
        // strip a live cluster's reference off a volume it owns.
        let meta = meta_owned_by(vec![owner("a-fresh-uid", "platform-postgres", true)]);
        assert!(
            owner_ref_indices(&meta, CLUSTER_UID).is_empty(),
            "same NAME, different uid must not be selected"
        );
        let meta = meta_owned_by(vec![owner(CLUSTER_UID, "renamed-somehow", true)]);
        assert_eq!(
            owner_ref_indices(&meta, CLUSTER_UID),
            vec![0],
            "the uid is what selects, whatever the entry calls itself"
        );
    }

    #[test]
    fn owner_ref_indices_are_descending_and_ignore_absent_or_empty_uids() {
        // Descending so one `remove` per index does not shift the next.
        let meta = meta_owned_by(vec![
            owner(CLUSTER_UID, "platform-postgres", true),
            owner("other-uid", "someone-else", false),
            owner(CLUSTER_UID, "platform-postgres", true),
        ]);
        assert_eq!(owner_ref_indices(&meta, CLUSTER_UID), vec![2, 0]);
        // No ownerReferences at all — a PVC nothing owns (the Dragonfly
        // shape) must select nothing rather than panic.
        assert!(owner_ref_indices(&ObjectMeta::default(), CLUSTER_UID).is_empty());
        // An EMPTY uid must match nothing, ever: the caller refuses to judge
        // a candidate without a uid, but a helper that selected every
        // reference with an absent uid is not one to leave loaded.
        let meta = meta_owned_by(vec![owner("", "no-uid", true)]);
        assert!(owner_ref_indices(&meta, "").is_empty());
    }

    #[test]
    fn strip_owner_ops_is_empty_when_the_pvc_is_not_ours() {
        // The no-op path the sweep uses to skip a PVC entirely — an empty op
        // list must never be sent as a patch, and must never be mistaken for
        // "strip succeeded on a PVC we do not own".
        let meta = meta_owned_by(vec![owner("other-uid", "someone-else", true)]);
        assert!(strip_owner_ops(&meta, CLUSTER_UID).is_empty());
    }

    #[test]
    fn strip_owner_ops_removes_only_our_entry_when_applied() {
        // The highest-stakes selection in the file, exercised by APPLYING the
        // ops rather than eyeballing them: a co-owner must survive, and the
        // entry pointing at this Cluster must not.
        let meta = meta_owned_by(vec![
            owner("other-uid", "someone-else", false),
            owner(CLUSTER_UID, "platform-postgres", true),
        ]);
        let ops = strip_owner_ops(&meta, CLUSTER_UID);
        // test+remove per matching index — the `test` is what makes the
        // apiserver reject the whole patch if the array shifted.
        assert_eq!(ops.len(), 2);
        assert_eq!(ops[0]["op"], "test");
        assert_eq!(ops[0]["path"], "/metadata/ownerReferences/1/uid");
        assert_eq!(ops[0]["value"], CLUSTER_UID);
        assert_eq!(ops[1]["op"], "remove");
        assert_eq!(ops[1]["path"], "/metadata/ownerReferences/1");

        let mut doc = pvc_doc(&meta);
        let patch: json_patch::Patch = serde_json::from_value(Value::Array(ops)).unwrap();
        json_patch::patch(&mut doc, &patch).expect("the strip must apply cleanly");
        let left = doc["metadata"]["ownerReferences"].as_array().unwrap();
        assert_eq!(left.len(), 1, "the co-owner must survive the strip");
        assert_eq!(left[0]["uid"], "other-uid");
        // And the object is now unowned BY US, which is what the sweep
        // re-reads to verify before it may delete.
        let after: ObjectMeta = serde_json::from_value(doc["metadata"].clone()).unwrap();
        assert!(owner_ref_indices(&after, CLUSTER_UID).is_empty());
    }

    #[test]
    fn strip_owner_ops_removes_every_matching_entry_without_shifting() {
        // Two entries pointing at the same Cluster (pathological, but the
        // descending order is what makes it correct rather than lucky): both
        // go, the one between them stays.
        let meta = meta_owned_by(vec![
            owner(CLUSTER_UID, "platform-postgres", true),
            owner("other-uid", "someone-else", false),
            owner(CLUSTER_UID, "platform-postgres", false),
        ]);
        let ops = strip_owner_ops(&meta, CLUSTER_UID);
        let mut doc = pvc_doc(&meta);
        let patch: json_patch::Patch = serde_json::from_value(Value::Array(ops)).unwrap();
        json_patch::patch(&mut doc, &patch).expect("descending removals must not shift");
        let left = doc["metadata"]["ownerReferences"].as_array().unwrap();
        assert_eq!(left.len(), 1);
        assert_eq!(left[0]["uid"], "other-uid");
    }

    #[test]
    fn strip_owner_ops_fail_closed_when_the_array_shifted_under_them() {
        // The reason for the `test` op. Ops computed against a two-entry
        // array, applied to a document where the other owner has since been
        // removed and ours has slid to index 0: the patch must be REJECTED
        // whole, not remove whatever now sits at index 1. This is the
        // apiserver's behaviour on a 422, reproduced here on the same ops.
        let read = meta_owned_by(vec![
            owner("other-uid", "someone-else", false),
            owner(CLUSTER_UID, "platform-postgres", true),
        ]);
        let ops = strip_owner_ops(&read, CLUSTER_UID);
        let shifted = meta_owned_by(vec![owner(CLUSTER_UID, "platform-postgres", true)]);
        let mut doc = pvc_doc(&shifted);
        let patch: json_patch::Patch = serde_json::from_value(Value::Array(ops)).unwrap();
        assert!(
            json_patch::patch(&mut doc, &patch).is_err(),
            "a shifted ownerReferences array must fail the test op, not be blindly edited"
        );
    }

    // --- 30: the ApiResources the arms delete through ---

    #[test]
    fn the_arms_address_the_plurals_the_apiserver_serves() {
        // A wrong plural 404s forever, and each arm's own "CRD not served"
        // branch swallows a 404 as `Ok` — so the reaper would report nothing
        // to reap, on every tick, silently, for the entire life of the
        // cluster. `ApiResource::from_gvk` INFERS the plural from the kind
        // (kube-core `to_plural`), so it is inference that is pinned here,
        // not a string this file wrote.
        assert_eq!(cluster_ar().plural, "clusters");
        assert_eq!(cluster_ar().group, "postgresql.cnpg.io");
        assert_eq!(cluster_ar().version, "v1");
        assert_eq!(pvc_ar().plural, "persistentvolumeclaims");
        assert_eq!(dragonfly_cluster_ar().plural, "dragonflies");
    }

    // --- 31: the selector both arms share ---

    #[test]
    fn the_managed_by_selector_matches_the_stamp_both_arms_emit() {
        // One selector covers both arms. The per-module tests pin each
        // object's stamp against the other's; this pins the SELECTOR against
        // the objects, so a stamp change that kept both objects in agreement
        // could still not silently empty both candidate sets at once.
        let dragonfly = crate::dragonfly::dragonfly_object(
            EPHEMERAL_000,
            "dragonfly-system",
            1024,
            1,
            1,
            false,
            &crate::cnpg::BackendResources::dragonfly_t1(),
        );
        let cluster = crate::cnpg::cluster_object(
            CLUSTER,
            "cnpg-system",
            1,
            "10Gi",
            &crate::cnpg::BackendResources::cnpg_t1(),
        );
        for obj in [&dragonfly, &cluster] {
            let value = obj["metadata"]["labels"][MANAGED_BY_KEY]
                .as_str()
                .expect("both arms' objects must carry the stamp");
            assert_eq!(
                managed_by_selector(),
                format!("{MANAGED_BY_KEY}={value}"),
                "the LIST selector must match the label actually emitted"
            );
        }
    }

    // --- 32/33: GATE 2 is structural, not just a LIST argument ---

    /// A candidate object as the apiserver would return it, with whatever
    /// labels the caller wants — including none.
    fn stamped_object(labels: &[(&str, &str)]) -> DynamicObject {
        let mut obj = DynamicObject::new("platform-postgres", &cluster_ar());
        obj.metadata.labels = Some(
            labels
                .iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                .collect(),
        );
        obj
    }

    #[test]
    fn is_stamped_accepts_only_the_exact_key_and_value() {
        assert!(is_stamped(&stamped_object(&[(
            MANAGED_BY_KEY,
            MANAGED_BY_VALUE
        )])));
        // Extra labels are fine — the stamp is a marker, not the whole map.
        assert!(is_stamped(&stamped_object(&[
            ("app.kubernetes.io/name", "postgres"),
            (MANAGED_BY_KEY, MANAGED_BY_VALUE),
        ])));
        // A DIFFERENT value on the right key is not our object. This is the
        // realistic near-miss: another controller stamping its own name.
        assert!(!is_stamped(&stamped_object(&[(
            MANAGED_BY_KEY,
            "somebody-else"
        )])));
        // The right value on a different key is not it either.
        assert!(!is_stamped(&stamped_object(&[(
            "managed-by",
            MANAGED_BY_VALUE
        )])));
        // No labels at all — a user's own Cluster, or one created before the
        // stamp existed.
        assert!(!is_stamped(&stamped_object(&[])));
        let mut bare = DynamicObject::new("platform-postgres", &cluster_ar());
        bare.metadata.labels = None;
        assert!(!is_stamped(&bare));
    }

    #[test]
    fn the_client_side_gate_survives_a_selectorless_list() {
        // The point of the re-check, stated as a test: an object that a
        // selectorless LIST would return — because someone dropped the
        // selector argument, added a field selector beside it, or swapped in
        // a reflector — is still rejected. A test that only pinned the
        // selector STRING would pass in all of those cases.
        let unstamped = stamped_object(&[]);
        assert!(
            !is_stamped(&unstamped),
            "an unstamped object must be rejected by the arm itself, not only by the apiserver"
        );
        // And it is rejected DESPITE passing gate 1: the name is exactly the
        // one the platform's own ServiceProvider config points at.
        let idx = seeded_index();
        assert!(
            idx.cnpg_cluster.values().any(|c| c == "platform-postgres"),
            "fixture must actually pass gate 1 for this to mean anything"
        );
    }

    // --- 34: the strip-failure → metric/log mapping (FIX 3) ---

    #[test]
    fn strip_failure_maps_read_failures_away_from_reattachment() {
        // Only a GENUINE reattachment may claim `veto_owner_reattached` — it
        // is the signal that §9.3's measured "CNPG does not re-add the
        // reference" has stopped holding, and a read failure counted into it
        // would be an assertion about CNPG the reaper never observed.
        assert_eq!(
            StripFailure::OwnerReattached.metric_label(),
            "veto_owner_reattached"
        );
        for failure in [
            StripFailure::ListTruncated,
            StripFailure::VerifyListTruncated,
            StripFailure::ReadFailed,
        ] {
            assert_eq!(
                failure.metric_label(),
                "veto_unverified",
                "{failure:?} is a read failure, not a reattachment"
            );
        }
        // The log reason stays finer than the metric bucket — that is what
        // separates the three inside `veto_unverified`.
        let reasons = [
            StripFailure::OwnerReattached.reason(),
            StripFailure::ListTruncated.reason(),
            StripFailure::VerifyListTruncated.reason(),
            StripFailure::ReadFailed.reason(),
        ];
        let distinct: BTreeSet<&str> = reasons.into_iter().collect();
        assert_eq!(
            distinct.len(),
            reasons.len(),
            "every failure mode needs its own log reason"
        );
        // Both buckets must actually render as distinct series under the
        // `cnpg` backend. Asserting on the ENCODED exposition rather than on
        // `with_label_values` returning something: the CounterVec accepts any
        // string, so "it did not panic" would be a test that cannot fail.
        let m = operator_core::metrics::Metrics::new();
        for failure in [
            StripFailure::OwnerReattached,
            StripFailure::ListTruncated,
            StripFailure::VerifyListTruncated,
            StripFailure::ReadFailed,
        ] {
            m.shared_backend_reap_total
                .with_label_values(&["cnpg", failure.metric_label()])
                .inc();
        }
        let body = String::from_utf8(m.encode()).unwrap();
        for label in ["veto_owner_reattached", "veto_unverified"] {
            assert!(
                body.contains(&format!(r#"backend="cnpg",result="{label}""#)),
                "{label} must render as a series under the cnpg backend:\n{body}"
            );
        }
        // The three read failures shared one bucket, so it counted 3 while
        // reattachment counted 1 — which is what "separate buckets" means
        // operationally.
        assert!(
            body.contains(r#"backend="cnpg",result="veto_unverified"} 3"#),
            "the three read failures must aggregate into one bucket:\n{body}"
        );
        assert!(
            body.contains(r#"backend="cnpg",result="veto_owner_reattached"} 1"#),
            "reattachment must not absorb the read failures:\n{body}"
        );
    }
}
