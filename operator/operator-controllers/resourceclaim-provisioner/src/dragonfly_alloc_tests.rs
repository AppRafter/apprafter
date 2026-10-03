// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Dragonfly `$N` allocation, driven through the real reconciles (WI-402).
//!
//! The tests here run the provisioner's own reconciles against
//! [`FakeApiserver`], which keeps the objects it is given and applies the
//! writes it receives — so a retry reads back exactly the checkpoint the
//! previous pass left behind, which is the state both data-loss paths start
//! from.

use std::sync::Arc;

use async_trait::async_trait;
use kube::runtime::controller::Action;
use serde_json::{json, Value};

use crate::fake_apiserver::FakeApiserver;
use crate::redis_client::{FakeRedis, RedisAdmin, RedisAdminError};
use crate::{cnpg, dragonfly, reconcile, Context, ReconcileError, PROVISIONER_FINALIZER};
use operator_core::{Metrics, ResourceClaim};

const NS: &str = "apps";
const DF_NS: &str = "dragonfly-system";
const PERSISTENT: &str = "platform-redis-persistent-000";
const EPHEMERAL: &str = "platform-redis-ephemeral-000";

/// A scheduled redis claim, optionally already carrying an allocation
/// checkpoint (`status.instance` / `status.dbnum`) and not yet ready.
fn claim(name: &str, persistent: bool, alloc: Option<(&str, u16)>) -> Value {
    let mut status = json!({
        "provider": "redis-df",
        "conditions": [{
            "type": "Scheduled", "status": "True",
            "lastTransitionTime": "2026-10-01T00:00:00Z",
        }],
    });
    if let Some((instance, dbnum)) = alloc {
        status["instance"] = json!(instance);
        status["dbnum"] = json!(dbnum);
    }
    json!({
        "apiVersion": "apprafter.io/v1alpha1", "kind": "ResourceClaim",
        "metadata": {
            "name": name, "namespace": NS, "uid": format!("uid-{name}"),
            "finalizers": [PROVISIONER_FINALIZER],
        },
        "spec": { "type": "redis", "selector": {}, "persistent": persistent },
        "status": status,
    })
}

/// The snapshot the finalizer wrote when `claim_name` was deleted, holding
/// `(instance, dbnum)` until `retain_until`.
fn retained(claim_name: &str, instance: &str, dbnum: u16, retain_until: &str) -> Value {
    json!({
        "apiVersion": "apprafter.io/v1alpha1", "kind": "RetainedClaim",
        "metadata": {
            "name": cnpg::k8s_name(NS, claim_name),
            "namespace": crate::reconcile::RETAINED_CLAIM_NAMESPACE,
        },
        "spec": {
            "claimRef": { "name": claim_name, "namespace": NS },
            "provider": "redis-df", "backend": "dragonfly",
            "retainUntil": retain_until,
            "instance": instance, "dbnum": dbnum,
            "aclUser": dragonfly::acl_user(NS, claim_name),
        },
    })
}

/// A grace deadline comfortably in the future.
const IN_GRACE: &str = "2099-01-01T00:00:00Z";

fn api() -> FakeApiserver {
    let api = FakeApiserver::new();
    api.provider("redis-df", "redis", "dragonfly");
    api
}

fn ctx(api: &FakeApiserver, redis: Arc<dyn RedisAdmin>) -> Arc<Context> {
    let mut ctx = Context::new(api.client(), Arc::new(Metrics::new()));
    ctx.redis = redis;
    Arc::new(ctx)
}

/// Reconcile the claim AS THE APISERVER CURRENTLY HOLDS IT — what the
/// controller's next trigger would hand the reconcile.
async fn reconcile_stored(
    api: &FakeApiserver,
    name: &str,
    redis: Arc<dyn RedisAdmin>,
) -> Result<Action, ReconcileError> {
    let stored: ResourceClaim = serde_json::from_value(api.claim(NS, name)).expect("claim");
    reconcile::reconcile(Arc::new(stored), ctx(api, redis)).await
}

fn flushed(redis: &FakeRedis) -> Vec<(String, u16)> {
    redis.flushdb_calls.lock().unwrap().clone()
}

/// `ACL SETUSER` refuses — what a persistent instance answers while it
/// restarts. Any error between the allocation checkpoint and the terminal
/// status write leaves the claim in the state these tests start from.
#[derive(Default)]
struct RefusingSetuser(FakeRedis);

#[async_trait]
impl RedisAdmin for RefusingSetuser {
    async fn acl_setuser(
        &self,
        addr: &str,
        _pw: &str,
        _args: &[String],
    ) -> Result<(), RedisAdminError> {
        Err(RedisAdminError::Command {
            verb: "ACL SETUSER",
            addr: addr.to_string(),
            source: redis::RedisError::from((redis::ErrorKind::Io, "LOADING instance restarting")),
        })
    }
    async fn acl_deluser(&self, a: &str, p: &str, u: &str) -> Result<(), RedisAdminError> {
        self.0.acl_deluser(a, p, u).await
    }
    async fn flushdb(&self, a: &str, p: &str, n: u16) -> Result<(), RedisAdminError> {
        self.0.flushdb(a, p, n).await
    }
    async fn dbsize(&self, a: &str, p: &str, n: u16) -> Result<i64, RedisAdminError> {
        self.0.dbsize(a, p, n).await
    }
}

// ---- (1) an interrupted persistent reattach must not flush on the retry ----

#[tokio::test]
async fn an_interrupted_persistent_reattach_keeps_its_retained_data_on_the_retry() {
    // web-redis was deleted and re-created inside its grace: its snapshot
    // still holds persistent $7, the data the reattach exists to recover.
    let api = api();
    api.put_claim(claim("web-redis", true, None))
        .put_retained(retained("web-redis", PERSISTENT, 7, IN_GRACE));

    // Pass 1 reattaches — no FLUSHDB, the checkpoint lands — then SETUSER
    // refuses, before the terminal status write.
    let pass1 = Arc::new(RefusingSetuser::default());
    let err = reconcile_stored(&api, "web-redis", pass1.clone())
        .await
        .expect_err("pass 1 must fail at SETUSER");
    assert!(matches!(err, ReconcileError::Provisioning(_)), "{err}");
    assert!(flushed(&pass1.0).is_empty(), "a reattach skips FLUSHDB");
    assert_eq!(
        api.claim(NS, "web-redis")["status"]["dbnum"],
        7,
        "checkpoint written"
    );

    // Pass 2 starts from that checkpoint, as the controller's retry would.
    let pass2 = Arc::new(FakeRedis::default());
    reconcile_stored(&api, "web-redis", pass2.clone())
        .await
        .expect("pass 2 provisions");
    assert_eq!(
        flushed(&pass2),
        Vec::<(String, u16)>::new(),
        "the retry FLUSHDBed the retained $7 it was recovering"
    );
    assert_eq!(api.claim(NS, "web-redis")["status"]["ready"], true);
    assert!(
        api.retained(&cnpg::k8s_name(NS, "web-redis")).is_none(),
        "the snapshot is cancelled only after the terminal write"
    );
}

#[tokio::test]
async fn a_checkpointed_fresh_allocation_is_still_flushed_on_the_retry() {
    // Recycle-safety must survive the fix: a persistent claim whose
    // checkpoint came from a FRESH allocation (no snapshot of its own) may
    // hold a departed tenant's keys and must start empty.
    let api = api();
    api.put_claim(claim("web-redis", true, Some((PERSISTENT, 7))));
    let redis = Arc::new(FakeRedis::default());
    reconcile_stored(&api, "web-redis", redis.clone())
        .await
        .expect("provisions");
    assert_eq!(
        flushed(&redis),
        vec![(dragonfly::instance_addr(PERSISTENT, DF_NS), 7)]
    );
}

#[tokio::test]
async fn a_checkpointed_ephemeral_reattach_is_still_flushed_on_the_retry() {
    // An ephemeral instance retains nothing, so a reattach there flushes as
    // usual (`resolve_allocation`'s skip_flush = persistent) — on the retry
    // too.
    let api = api();
    api.put_claim(claim("web-redis", false, Some((EPHEMERAL, 4))))
        .put_retained(retained("web-redis", EPHEMERAL, 4, IN_GRACE));
    let redis = Arc::new(FakeRedis::default());
    reconcile_stored(&api, "web-redis", redis.clone())
        .await
        .expect("provisions");
    assert_eq!(
        flushed(&redis),
        vec![(dragonfly::instance_addr(EPHEMERAL, DF_NS), 4)]
    );
}

#[tokio::test]
async fn a_snapshot_on_another_dbnum_does_not_mark_the_checkpoint_as_a_reattach() {
    // The marker is the snapshot naming THIS checkpoint's (instance, dbnum).
    // A snapshot of this claim on a different number is not it.
    let api = api();
    api.put_claim(claim("web-redis", true, Some((PERSISTENT, 7))))
        .put_retained(retained("web-redis", PERSISTENT, 3, IN_GRACE));
    let redis = Arc::new(FakeRedis::default());
    reconcile_stored(&api, "web-redis", redis.clone())
        .await
        .expect("provisions");
    assert_eq!(
        flushed(&redis),
        vec![(dragonfly::instance_addr(PERSISTENT, DF_NS), 7)]
    );
}

// ---- (1b) the snapshot is the marker, so the GC must not take it mid-reattach ----

/// A grace deadline that has already passed.
const EXPIRED: &str = "2026-01-01T00:00:00Z";

/// Run the RetainedClaim GC over `claim_name`'s snapshot as stored.
async fn gc_stored(
    api: &FakeApiserver,
    claim_name: &str,
    redis: Arc<dyn RedisAdmin>,
) -> Result<Action, ReconcileError> {
    let rc: operator_core::RetainedClaim = serde_json::from_value(
        api.retained(&cnpg::k8s_name(NS, claim_name))
            .expect("snapshot present"),
    )
    .expect("snapshot");
    crate::gc::reconcile(Arc::new(rc), ctx(api, redis)).await
}

#[tokio::test]
async fn a_reattach_still_failing_when_grace_runs_out_keeps_its_retained_data() {
    // Re-created on the last day of grace; the reattach keeps failing past
    // `retainUntil`, so the GC visits the snapshot while the claim is live
    // but not ready.
    let api = api();
    api.put_claim(claim("web-redis", true, None))
        .put_retained(retained("web-redis", PERSISTENT, 7, EXPIRED));
    let pass1 = Arc::new(RefusingSetuser::default());
    reconcile_stored(&api, "web-redis", pass1.clone())
        .await
        .expect_err("pass 1 must fail at SETUSER");

    gc_stored(&api, "web-redis", Arc::new(FakeRedis::default()))
        .await
        .expect("gc pass");
    assert!(
        api.retained(&cnpg::k8s_name(NS, "web-redis")).is_some(),
        "the GC deleted the snapshot of a reattach still in flight"
    );

    let pass2 = Arc::new(FakeRedis::default());
    reconcile_stored(&api, "web-redis", pass2.clone())
        .await
        .expect("pass 2 provisions");
    assert_eq!(
        flushed(&pass2),
        Vec::<(String, u16)>::new(),
        "the retry flushed the recovered $7"
    );
}

#[tokio::test]
async fn a_recreated_claim_that_has_not_checkpointed_yet_keeps_its_expired_snapshot() {
    // The window before the first checkpoint: the provisioner has LISTed and
    // found the snapshot, its checkpoint PATCH has not landed, and the GC
    // looks now. The claim holds no allocation at all — which must not read
    // as "not re-attaching".
    let api = api();
    api.put_claim(claim("web-redis", true, None))
        .put_retained(retained("web-redis", PERSISTENT, 7, EXPIRED));
    gc_stored(&api, "web-redis", Arc::new(FakeRedis::default()))
        .await
        .expect("gc pass");
    assert!(
        api.retained(&cnpg::k8s_name(NS, "web-redis")).is_some(),
        "the GC deleted the snapshot a not-yet-checkpointed reattach is about to use"
    );

    let redis = Arc::new(FakeRedis::default());
    reconcile_stored(&api, "web-redis", redis.clone())
        .await
        .expect("provisions");
    assert_eq!(flushed(&redis), Vec::<(String, u16)>::new());
    assert_eq!(api.claim(NS, "web-redis")["status"]["dbnum"], 7);
}

#[tokio::test]
async fn the_gc_still_deletes_the_stale_snapshot_of_a_ready_claim() {
    // The pre-WI-402 live-guard, unchanged for a claim that finished: its
    // snapshot is stale, and the GC (not only the provisioner) removes it.
    let api = api();
    let mut ready = claim("web-redis", true, Some((PERSISTENT, 7)));
    ready["status"]["ready"] = json!(true);
    api.put_claim(ready)
        .put_retained(retained("web-redis", PERSISTENT, 7, EXPIRED));
    gc_stored(&api, "web-redis", Arc::new(FakeRedis::default()))
        .await
        .expect("gc pass");
    assert!(api.retained(&cnpg::k8s_name(NS, "web-redis")).is_none());
}

#[tokio::test]
async fn the_gc_still_deletes_an_expired_ephemeral_snapshot_of_a_claim_not_yet_ready() {
    // An ephemeral snapshot retains nothing, so it is never load-bearing: the
    // live-guard deletes it once grace has passed, ready or not, as before.
    let api = api();
    api.put_claim(claim("web-redis", false, None))
        .put_retained(retained("web-redis", EPHEMERAL, 4, EXPIRED));
    gc_stored(&api, "web-redis", Arc::new(FakeRedis::default()))
        .await
        .expect("gc pass");
    assert!(api.retained(&cnpg::k8s_name(NS, "web-redis")).is_none());
}

// ---- (2) every FLUSHDB re-checks that nobody else holds the $N ----

/// A ready shared cache holding `$dbnum` on `instance`.
fn shared_on(name: &str, instance: &str, dbnum: u16) -> Value {
    json!({
        "apiVersion": "apprafter.io/v1alpha1", "kind": "SharedDatabase",
        "metadata": { "name": name, "namespace": NS },
        "spec": { "type": "redis" },
        "status": { "ready": true, "instance": instance, "dbnum": dbnum },
    })
}

/// `(reason, message)` of the stored claim's `Ready` condition.
fn ready_reason(api: &FakeApiserver, name: &str) -> (String, String) {
    let claim = api.claim(NS, name);
    let ready = claim["status"]["conditions"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|c| c["type"] == "Ready")
        .cloned()
        .unwrap_or(Value::Null);
    (
        ready["reason"].as_str().unwrap_or_default().to_string(),
        ready["message"].as_str().unwrap_or_default().to_string(),
    )
}

#[tokio::test]
async fn a_late_committed_allocation_on_the_same_dbnum_blocks_the_flush_and_moves_the_claim() {
    // cart-redis's reconcile was dropped mid-checkpoint; its write commits
    // after web-redis's allocation LIST, onto the same $0.
    let api = api();
    api.put_claim(claim("web-redis", false, None)).commit_late(
        |c| c.is_status_patch("resourceclaims", "web-redis"),
        claim("cart-redis", false, Some((EPHEMERAL, 0))),
    );
    let redis = Arc::new(FakeRedis::default());
    let err = reconcile_stored(&api, "web-redis", redis.clone())
        .await
        .expect_err("a shared $N must not be provisioned");
    let msg = err.to_string();
    assert!(matches!(err, ReconcileError::Provisioning(_)), "{msg}");
    assert!(msg.contains("ResourceClaim apps/cart-redis"), "{msg}");
    assert!(
        flushed(&redis).is_empty(),
        "flushed a $N another claim holds"
    );
    assert!(
        redis.setuser_calls.lock().unwrap().is_empty(),
        "no ACL user on it either"
    );
    // The number held nothing of web-redis's own yet, so it lets go of it —
    // and says so on the claim, where `app status` shows it. Self-clearing,
    // so an `Awaiting` reason.
    let status = api.claim(NS, "web-redis")["status"].clone();
    assert_eq!(status["dbnum"], Value::Null, "the checkpoint is released");
    assert_eq!(status["instance"], Value::Null);
    assert_eq!(status["ready"], false);
    let (reason, message) = ready_reason(&api, "web-redis");
    assert_eq!(reason, "AwaitingKeyspace");
    assert!(
        message.contains("ResourceClaim apps/cart-redis"),
        "{message}"
    );

    // The next pass allocates around the other holder.
    let retry = Arc::new(FakeRedis::default());
    reconcile_stored(&api, "web-redis", retry.clone())
        .await
        .expect("provisions on another $N");
    assert_eq!(
        flushed(&retry),
        vec![(dragonfly::instance_addr(EPHEMERAL, DF_NS), 1)]
    );
    assert_eq!(api.claim(NS, "web-redis")["status"]["dbnum"], 1);
    assert_eq!(api.claim(NS, "cart-redis")["status"]["dbnum"], 0);
}

#[tokio::test]
async fn two_fresh_checkpoints_on_one_dbnum_end_with_one_holder_each() {
    // Two late commits left web-redis and cart-redis both checkpointed on
    // $0, neither ready. Whichever checks first lets go; the other provisions
    // $0; the first then takes $1. Nothing flushes $0 after its holder is
    // ready.
    let api = api();
    api.put_claim(claim("web-redis", false, Some((EPHEMERAL, 0))))
        .put_claim(claim("cart-redis", false, Some((EPHEMERAL, 0))));
    let redis = Arc::new(FakeRedis::default());
    for _round in 0..3 {
        for name in ["web-redis", "cart-redis"] {
            // A pass may refuse; a ready claim's pass provisions nothing.
            let _ = reconcile_stored(&api, name, redis.clone()).await;
        }
    }
    let addr = dragonfly::instance_addr(EPHEMERAL, DF_NS);
    for (name, dbnum) in [("cart-redis", 0), ("web-redis", 1)] {
        let status = api.claim(NS, name)["status"].clone();
        assert_eq!(status["ready"], true, "{name}");
        assert_eq!(status["dbnum"], dbnum, "{name}");
    }
    assert_eq!(
        flushed(&redis),
        vec![(addr.clone(), 0), (addr, 1)],
        "each $N is flushed once, before its holder is ready"
    );
}

#[tokio::test]
async fn a_fresh_checkpoint_on_a_dbnum_a_snapshot_reserves_moves_off_it() {
    // gone-redis's snapshot reserves $5 for its grace; a late commit put
    // web-redis's fresh checkpoint on it anyway. Waiting would hold
    // web-redis back until the snapshot's GC; moving costs nothing.
    let api = api();
    api.put_claim(claim("web-redis", false, Some((EPHEMERAL, 5))))
        .put_retained(retained("gone-redis", EPHEMERAL, 5, IN_GRACE));
    let redis = Arc::new(FakeRedis::default());
    let err = reconcile_stored(&api, "web-redis", redis.clone())
        .await
        .expect_err("a reserved $N must not be provisioned");
    assert!(
        err.to_string()
            .contains("RetainedClaim claim-apps-gone-redis (of apps/gone-redis)"),
        "{err}"
    );
    reconcile_stored(&api, "web-redis", redis.clone())
        .await
        .expect("provisions on a free $N");
    assert_eq!(
        flushed(&redis),
        vec![(dragonfly::instance_addr(EPHEMERAL, DF_NS), 0)]
    );
    assert!(api.retained(&cnpg::k8s_name(NS, "gone-redis")).is_some());
}

#[tokio::test]
async fn a_persistent_reattach_onto_a_dbnum_someone_else_holds_keeps_it_and_refuses() {
    // No FLUSHDB on this branch, but an ACL user pinned to $7 would read the
    // other holder's keys just the same. And $7 is the retained data, so this
    // claim is the one holder that must not move off it.
    let api = api();
    api.put_claim(claim("web-redis", true, None))
        .put_retained(retained("web-redis", PERSISTENT, 7, IN_GRACE))
        .put_shared(shared_on("orders", PERSISTENT, 7));
    let redis = Arc::new(FakeRedis::default());
    for _pass in 0..2 {
        let err = reconcile_stored(&api, "web-redis", redis.clone())
            .await
            .expect_err("a shared $N must not be provisioned");
        assert!(
            err.to_string().contains("SharedDatabase apps/orders"),
            "{err}"
        );
    }
    assert!(redis.setuser_calls.lock().unwrap().is_empty());
    assert!(flushed(&redis).is_empty());
    let status = api.claim(NS, "web-redis")["status"].clone();
    assert_eq!(
        status["instance"], PERSISTENT,
        "the reattach keeps its number"
    );
    assert_eq!(status["dbnum"], 7);
    // Not self-clearing: a person has to resolve it.
    let (reason, message) = ready_reason(&api, "web-redis");
    assert_eq!(reason, "DbnumConflict");
    assert!(message.contains("SharedDatabase apps/orders"), "{message}");
    assert!(api.retained(&cnpg::k8s_name(NS, "web-redis")).is_some());
}

#[tokio::test]
async fn an_ephemeral_reattach_onto_a_held_dbnum_cancels_its_snapshot_and_allocates_fresh() {
    // An ephemeral snapshot retains nothing, so its number is not worth
    // waiting for. Keeping the snapshot would send every later pass straight
    // back to the same reattach and the same refusal.
    let api = api();
    api.put_claim(claim("web-redis", false, None))
        .put_retained(retained("web-redis", EPHEMERAL, 4, IN_GRACE))
        .put_shared(shared_on("orders", EPHEMERAL, 4));
    let redis = Arc::new(FakeRedis::default());
    reconcile_stored(&api, "web-redis", redis.clone())
        .await
        .expect_err("a shared $N must not be provisioned");
    assert!(flushed(&redis).is_empty());
    assert!(api.retained(&cnpg::k8s_name(NS, "web-redis")).is_none());
    reconcile_stored(&api, "web-redis", redis.clone())
        .await
        .expect("provisions on a free $N");
    assert_eq!(
        flushed(&redis),
        vec![(dragonfly::instance_addr(EPHEMERAL, DF_NS), 0)]
    );
    assert_eq!(api.claim(NS, "web-redis")["status"]["dbnum"], 0);
}
