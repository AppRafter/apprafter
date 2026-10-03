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
