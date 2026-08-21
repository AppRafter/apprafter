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

use k8s_openapi::apimachinery::pkg::apis::meta::v1::ListMeta;
use kube::api::{Api, DeleteParams, DynamicObject, ListParams, Preconditions};
use kube::ResourceExt;
use serde_json::Value;
use tracing::{debug, info, warn};

use crate::acl_reconcile::redis_namespace_from;
use crate::dragonfly::{class_of_instance, PoolClass};
use crate::reconcile::{dragonfly_cluster_ar, Backend};
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
    // The `?` here is PROVISIONAL and must not survive the CNPG arm. It
    // couples the arms: any non-404 Dragonfly error aborts the pass before a
    // second arm runs. The realistic trigger is not a transient but a 403
    // from missing `dragonflydb.io` RBAC — which recurs every tick, so CNPG
    // would never be reaped at all, permanently, while the metric reported
    // only `unknown/error` with nothing to say a different backend was the
    // collateral damage. Invariant 3 requires that an error not be read as
    // emptiness FOR THE RESOURCE THAT ERRORED; it does not require one
    // backend's failure to veto another. Task 6 must run both arms and
    // combine their results (union the `seen` sets, propagate a failure only
    // after both have run).
    let seen: BTreeSet<String> = sweep_dragonfly(ctx, &inputs, dwell, empty_since).await?;

    // An arm that bailed early (CRD not served, or its own LIST truncated)
    // reports an empty set, so this drops its clocks. That is correct for
    // "CRD not served" (there are no instances to time) and costs one extra
    // dwell for a truncated list — the same cost, in the same safe direction,
    // that this loop already accepts on an operator restart. Deliberate, not
    // an oversight; see the note on the top-level truncation check above.
    empty_since.retain(|key, _| seen.contains(key));
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
    let selector = ListParams::default().labels("apprafter.io/managed-by=apprafter");
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
                // Delete under a uid precondition, so a name-reuse race —
                // the instance we judged was deleted and re-created by a
                // fresh provision between our LIST and this call — fails
                // with 409 instead of killing the new instance.
                let params = DeleteParams {
                    preconditions: Some(Preconditions {
                        uid: obj.metadata.uid.clone(),
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
                            uid = obj.metadata.uid.as_deref().unwrap_or("<none>"),
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
}
