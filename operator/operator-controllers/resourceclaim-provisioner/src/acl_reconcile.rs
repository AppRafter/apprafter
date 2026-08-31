// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! ACL re-pin reconcile loop for the dragonfly backend (Phase 2.6-5,
//! ADR 0042 §4).
//!
//! Per-claim `$N`-pinned ACL users are *runtime* state on a Dragonfly
//! instance: they live in the running process, not on the `Dragonfly` CR,
//! so a pod restart (OOM, node drain, image roll, a `kubectl delete pod`)
//! wipes every claim user the provisioner created. The connection Secrets
//! still hold the right DSN, but the app's login now fails (`WRONGPASS` /
//! `NOPERM`) until the user is re-asserted. This loop closes that gap: on a
//! periodic resync it lists every live, ready dragonfly `ResourceClaim`,
//! groups them by pool instance, and re-runs `ACL SETUSER` for each — an
//! idempotent upsert (the `acl_setuser_args` vector carries
//! `resetkeys` / `resetchannels`, so a re-pin is byte-stable whether the
//! user was wiped or already correct).
//!
//! ## Why a periodic resync, not a pod watch
//!
//! The provisioner deliberately runs watch-free (see `lib::run` — every
//! claim re-evaluates on a 300s requeue), and a `Dragonfly` pod can churn
//! without the operator ever seeing a clean "ready transition" edge (the
//! dragonfly-operator owns the StatefulSet; we do not). A short periodic
//! resync is the robust, simplest mechanism: it re-pins within one
//! interval of ANY restart, needs no pod-readiness bookkeeping, and is
//! cheap (a handful of control-plane `ACL SETUSER` calls per instance, not
//! a hot path). The reconnect window is bounded by [`RESYNC_INTERVAL`].
//!
//! ## Password recovery — no separate per-claim password store
//!
//! Re-asserting a claim user needs that user's password. It is NOT stored
//! anywhere separate: the per-claim connection Secret's `pass` key (2.12
//! decomposed format, ADR 0046) carries it directly, so the loop reads the
//! Secret the claim points at (`status.connectionSecretRef`) and reads the
//! `pass` key ([`read_secret_key`]). This keeps the password in exactly one
//! place (the connection Secret) — the same value the app uses.
//!
//! ## A second passenger: how much each tenant holds (2.22d / D8)
//!
//! The loop also samples each claim's `DBSIZE` and stamps it on
//! `status.size.keys`, because it already holds everything that needs — the
//! instance address and its admin password, resolved once per instance — and
//! its cadence matches the window the Postgres size scrape is cached for, so
//! the two figures age alike. See [`refresh_claim_keys`] for why the figure
//! is a key count and not bytes.

use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use kube::api::{Api, DynamicObject};
use kube::ResourceExt;
use serde_json::Value;
use tracing::{info, warn};

use operator_core::ResourceClaim;

use crate::dragonfly;
use crate::reconcile::secret_ar;
use crate::{Context, ReconcileError};

/// How often the loop re-asserts every live claim's ACL user. Short
/// enough that an app's reconnect window after a Dragonfly pod restart is
/// bounded to ~one interval; long enough that the per-instance `ACL
/// SETUSER` fan-out stays a negligible control-plane cost. Mirrors the
/// provisioner's own 300s requeue cadence.
const RESYNC_INTERVAL: Duration = Duration::from_secs(300);

/// The `redis` service-type string a claim's `spec.type` carries when it
/// was generated from a `needs.redis`. Only these claims own a dragonfly
/// ACL user to re-pin.
const REDIS_TYPE: &str = "redis";

/// How long to wait after a poke before re-deriving, so a burst of claims
/// provisioned together yields one file write rather than one per claim.
const POKE_DEBOUNCE: Duration = Duration::from_secs(2);

/// Field manager for the ACL file Secret and for the `Dragonfly` CR fields
/// the loop owns (ADR 0042 §10).
///
/// Dedicated, for the reason the CLI's `apprafter-cli-egress` is: the
/// provisioner applies the WHOLE `Dragonfly` CR under
/// `resourceclaim-provisioner`, so writing `aclFromSecret` under that manager
/// would make the provisioner's next apply — which does not carry the field —
/// prune it, and the two would fight in a roll-war.
const ACL_FIELD_MANAGER: &str = "resourceclaim-provisioner-acl";

/// One claim's per-tick payload, derived purely from a live `ResourceClaim`
/// (no I/O). The loop turns each of these into an `ACL SETUSER` after it
/// recovers the password from the connection Secret, and into a `DBSIZE`
/// sample of the same claim's logical DB.
#[derive(Debug, Clone, PartialEq)]
pub struct RepinSpec {
    /// The deterministic `$N` ACL username (`claim_<ns>_<name>_redis`).
    pub user: String,
    /// The numbered logical DB the user is pinned to.
    pub dbnum: u16,
    /// The connection Secret (in the claim's namespace) whose `pass` key
    /// carries this user's password — the loop reads it back to re-pin.
    pub conn_secret_ref: String,
    /// The claim's namespace (where `conn_secret_ref` lives).
    pub claim_namespace: String,
    /// The claim's own name — the object the size sample is written back to.
    pub claim_name: String,
    /// The size figure already on the claim, so the deadband can decide
    /// whether a fresh sample is worth a write without a second read.
    pub previous_size: Option<operator_core::ClaimSize>,
}

/// Pure decision: the set of claims to re-pin on `instance`.
///
/// A claim qualifies iff it is a `redis`-type claim that the provisioner
/// has fully provisioned onto THIS instance — i.e. it carries a complete
/// allocation (`status.instance == instance`, `status.dbnum`), is
/// `ready`, and has a `connectionSecretRef` (the password source). Claims
/// mid-provision (no dbnum yet / not ready / no Secret) are skipped: the
/// provisioner's own reconcile owns those until they complete. Claims on a
/// different instance, or non-redis claims (cnpg), are skipped.
///
/// The returned [`RepinSpec`]s carry the deterministic ACL username
/// (re-derived from the claim's `(namespace, name)`, the SAME way the
/// provisioner built it) so the loop never has to trust a status field for
/// the username.
pub fn claims_to_repin(claims: &[ResourceClaim], instance: &str) -> Vec<RepinSpec> {
    claims
        .iter()
        .filter_map(|c| {
            if c.spec.type_ != REDIS_TYPE {
                return None;
            }
            // A claim mid-deletion is on its way out — its user will be
            // dropped by the GC; do not re-pin it.
            if c.metadata.deletion_timestamp.is_some() {
                return None;
            }
            let st = c.status.as_ref()?;
            if st.instance.as_deref() != Some(instance) {
                return None;
            }
            if st.ready != Some(true) {
                return None;
            }
            let dbnum = st.dbnum?;
            let conn_secret_ref = st.connection_secret_ref.clone()?;
            let previous_size = st.size.clone();
            let ns = c.namespace().unwrap_or_default();
            let name = c.name_any();
            Some(RepinSpec {
                user: dragonfly::acl_user(&ns, &name),
                dbnum,
                conn_secret_ref,
                claim_namespace: ns,
                claim_name: name,
                previous_size,
            })
        })
        .collect()
}

/// Pure: the set of distinct pool instances any live ready dragonfly claim
/// is allocated on. The loop iterates these (rather than a hardcoded
/// instance name) so it re-pins whatever instances the pool actually grew
/// to. Non-redis / unallocated / not-ready claims contribute nothing.
pub fn allocated_instances(claims: &[ResourceClaim]) -> Vec<String> {
    let mut set: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for c in claims {
        if c.spec.type_ != REDIS_TYPE {
            continue;
        }
        if c.metadata.deletion_timestamp.is_some() {
            continue;
        }
        let Some(st) = c.status.as_ref() else {
            continue;
        };
        if st.ready != Some(true) {
            continue;
        }
        if let Some(inst) = st.instance.as_deref() {
            set.insert(inst.to_string());
        }
    }
    set.into_iter().collect()
}

/// Pure: recover the password from a `redis://user:password@host:port/db`
/// DSN. Returns the (percent-decoded) password, or `None` when the URL has
/// no userinfo password component. URL-encoded passwords (the provisioner
/// generates alphanumeric ones, but a hand-rotated password could contain
/// reserved chars) are decoded.
///
/// This is the loop's sole password source: the connection Secret's
/// `REDIS_URL` is the single place a claim user's password lives, so a
/// re-pin reads it back rather than keeping a separate store.
pub fn password_from_redis_url(url: &str) -> Option<String> {
    // Strip the scheme, then split on the LAST '@' (a password may itself
    // contain an '@', though the generated ones do not).
    let after_scheme = url.split_once("://").map(|(_, rest)| rest).unwrap_or(url);
    let (userinfo, _) = after_scheme.rsplit_once('@')?;
    // userinfo = "user:password" — split on the FIRST ':' (a password may
    // contain ':'; the username never does here).
    let (_, password) = userinfo.split_once(':')?;
    if password.is_empty() {
        return None;
    }
    Some(percent_decode(password))
}

/// Minimal percent-decoder for a DSN password component (`%XX` → byte).
/// Invalid/truncated escapes pass through verbatim — a best-effort decode
/// that never panics. (`+` is NOT treated as a space: that is form-encoding,
/// not generic percent-encoding, and a redis password `+` is literal.)
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hi = (bytes[i + 1] as char).to_digit(16);
            let lo = (bytes[i + 2] as char).to_digit(16);
            if let (Some(hi), Some(lo)) = (hi, lo) {
                out.push((hi * 16 + lo) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Spawn the ACL re-pin loop. Runs forever on a [`RESYNC_INTERVAL`] tick,
/// re-asserting every live ready dragonfly claim's `$N` ACL user against
/// its pool instance. Wired in `main.rs` alongside the provisioner +
/// GC controllers (same crate; shares the [`Context`] redis seam).
///
/// The loop is best-effort and self-healing: any per-instance or per-claim
/// error (instance unreachable, admin Secret missing, a Secret read race)
/// is logged and skipped — the next tick retries. It NEVER aborts the
/// task, so a transient failure on one claim cannot stop re-pinning the
/// rest or future ticks.
pub async fn run(ctx: Arc<Context>) -> Result<(), ReconcileError> {
    info!(
        resync_secs = RESYNC_INTERVAL.as_secs(),
        "DragonflyAclReconcile loop starting"
    );
    loop {
        if let Err(err) = resync_all(&ctx).await {
            warn!(%err, "DragonflyAclReconcile resync pass failed — retrying next tick");
        }
        // ADR 0042 §10: wake early when the live ACL set changed, so the
        // durable file catches up in seconds rather than on the next tick.
        // The periodic arm stays — it is what re-pins a runtime user after a
        // restart nobody signalled.
        tokio::select! {
            _ = tokio::time::sleep(RESYNC_INTERVAL) => {}
            _ = ctx.acl_dirty.notified() => {
                // Coalesce a burst: several claims provisioned together
                // should produce one file derivation, not one per claim.
                tokio::time::sleep(POKE_DEBOUNCE).await;
            }
        }
    }
}

/// One full resync pass: list every `ResourceClaim` cluster-wide, group
/// the live ready dragonfly ones by instance, and re-pin each instance's
/// users. Returns an error only on the initial list failure (the per-
/// instance loop swallows + logs its own errors so one bad instance never
/// blocks the others).
async fn resync_all(ctx: &Arc<Context>) -> Result<(), ReconcileError> {
    let claims: Vec<ResourceClaim> = Api::<ResourceClaim>::all(ctx.client.clone())
        .list(&Default::default())
        .await?
        .items;

    let df_ns = redis_namespace(ctx).await?;

    // The instance set is the union of "has a live claim" and "exists as a
    // CR we manage". Claim-derived alone never visits an instance whose last
    // tenant was GC'd — so its file would never shrink, and the revoked
    // tenant's line would survive every restart. That is the revocation
    // being durable in the wrong direction.
    //
    // The CR list is best-effort (the ADR 0048 lesson): on failure, warn and
    // fall back to the claim-derived set. That misses a tenantless instance,
    // which delays a shrink; it can never produce a WRONG file.
    let mut instances = allocated_instances(&claims);
    match managed_instances(ctx, &df_ns).await {
        Ok(crs) => {
            for i in crs {
                if !instances.contains(&i) {
                    instances.push(i);
                }
            }
            instances.sort();
        }
        Err(err) => warn!(
            %err,
            "could not list managed Dragonfly instances — falling back to claim-derived set \
             (a tenantless instance's file will not shrink this pass)"
        ),
    }

    for instance in instances {
        if let Err(err) = reconcile_instance_acls(ctx, &instance, &claims, &df_ns).await {
            warn!(%instance, %err, "ACL re-pin for instance failed — skipping; retried next tick");
        }
    }
    Ok(())
}

/// Names of the `Dragonfly` CRs this platform manages in `df_ns`.
///
/// Selected on the same `apprafter.io/managed-by` stamp the reaper uses, so
/// an instance this operator did not create can never be written to.
async fn managed_instances(ctx: &Arc<Context>, df_ns: &str) -> Result<Vec<String>, ReconcileError> {
    let api: Api<DynamicObject> = Api::namespaced_with(
        ctx.client.clone(),
        df_ns,
        &crate::reconcile::dragonfly_cluster_ar(),
    );
    let lp = kube::api::ListParams::default().labels("apprafter.io/managed-by=apprafter");
    Ok(api
        .list(&lp)
        .await?
        .items
        .into_iter()
        .filter_map(|o| o.metadata.name)
        .collect())
}

/// Re-assert every live ready claim user on a single pool instance.
///
/// Reads the instance admin password once, then for each claim derived by
/// [`claims_to_repin`]: reads the claim's connection Secret `pass` key
/// (2.12 decomposed format) and re-runs `ACL SETUSER` (idempotent).
/// A claim whose Secret is missing/unreadable, or whose DSN carries no
/// password, is logged + skipped (the next tick retries once the Secret
/// settles) — it never aborts the instance's remaining re-pins.
///
/// The dragonfly namespace is resolved from the instance's
/// admin-Secret lookup convention (`<instance>-admin` in the dragonfly
/// namespace). The instance name is class-prefixed (`platform-redis-*`),
/// and the platform seeds exactly one dragonfly namespace, so the loop
/// reads the namespace from the matched `redis-integrated` ServiceProvider
/// config (falling back to the well-known default) the SAME way the
/// provisioner does.
pub async fn reconcile_instance_acls(
    ctx: &Arc<Context>,
    instance: &str,
    claims: &[ResourceClaim],
    df_ns: &str,
) -> Result<(), ReconcileError> {
    let to_repin = claims_to_repin(claims, instance);
    // NO early return on an empty set. A tenantless instance still needs a
    // `default`-only file: returning here is exactly how a revoked tenant's
    // line survives in the file after its last claim is GC'd, and a restart
    // then re-grants a credential the platform revoked.

    let addr = dragonfly::instance_addr(instance, df_ns);
    let admin_secret_name = dragonfly::admin_secret_name(instance);
    // Hard `?`, and first. Without the admin password there is no `default`
    // line, and a file without one turns authentication OFF on this instance
    // (ADR 0042 §10) — so abort the instance and leave the file untouched.
    let admin_pw = read_secret_key(ctx, df_ns, &admin_secret_name, "password").await?;

    info!(
        %instance, %df_ns, count = to_repin.len(),
        "re-pinning dragonfly claim ACL users (resync)"
    );

    // Whether this pass saw EVERY live tenant. A single unreadable connection
    // Secret means the derived file would be missing that tenant's line — and
    // under whole-file derivation, writing it would DELETE a durable grant
    // over a transient read error. So a `pass` read failure taints the file
    // and the write is skipped entirely; the runtime user is untouched and
    // the next tick retries.
    //
    // Deliberately NOT tainted by a `DBSIZE` failure (decorative, ADR 0048)
    // or by an `ACL SETUSER` failure (the credential is still valid — only
    // the re-pin did not land, and persisting it is still correct).
    let mut file_complete = true;
    let mut tenant_lines: Vec<Vec<String>> = Vec::new();

    for spec in to_repin {
        // Sample how much this tenant holds, before the ACL half — the two
        // are independent, and an unreadable connection Secret should not
        // also blind the size.
        refresh_claim_keys(ctx, &addr, &admin_pw, &spec).await;

        // Recover this claim's password from its connection Secret's `pass`
        // key (2.12 decomposed format; the Secret is written by
        // `redis_connection_secret_object`).
        let password = match read_secret_key(
            ctx,
            &spec.claim_namespace,
            &spec.conn_secret_ref,
            "pass",
        )
        .await
        {
            Ok(p) => p,
            Err(err) => {
                warn!(
                    user = %spec.user, secret = %spec.conn_secret_ref, %err,
                    "could not read connection Secret `pass` — skipping re-pin AND the ACL \
                     file write this tick (a partial file would delete a durable grant)"
                );
                file_complete = false;
                continue;
            }
        };

        let args = dragonfly::acl_setuser_args(&spec.user, &password, spec.dbnum);
        tenant_lines.push(args.clone());
        if let Err(err) = ctx.redis.acl_setuser(&addr, &admin_pw, &args).await {
            warn!(
                user = %spec.user, %instance, %err,
                "ACL SETUSER re-pin failed — skipping; retried next tick"
            );
            continue;
        }
    }

    if file_complete {
        persist_acl_file(ctx, instance, df_ns, &admin_pw, &tenant_lines).await;
    }
    Ok(())
}

/// Write the instance's durable ACL file, then point the CR at it
/// (ADR 0042 §10). Best-effort: every failure leaves the previous state and
/// is retried next pass — this is durability, not the grant path.
///
/// ORDER IS LOAD-BEARING. The Secret must exist before the CR names it: the
/// dragonfly-operator never sets `SecretVolumeSource.Optional`, so a mount of
/// a missing Secret is REQUIRED and the pod cannot start. Getting this
/// backwards costs minutes of `FailedMount` backoff, unbounded if the Secret
/// never appears.
async fn persist_acl_file(
    ctx: &Arc<Context>,
    instance: &str,
    df_ns: &str,
    admin_pw: &str,
    tenants: &[Vec<String>],
) {
    let contents = match dragonfly::acl_file_contents(admin_pw, tenants) {
        Ok(c) => c,
        Err(err) => {
            // Refusing is the safe direction: one malformed line rejects the
            // WHOLE file, so replacing a working file with a broken one would
            // lock out every tenant at the next restart.
            warn!(%instance, %err, "refusing to write a damaged ACL file — keeping the previous one");
            return;
        }
    };

    let secret_name = dragonfly::acl_secret_name(instance);
    let api: Api<DynamicObject> = Api::namespaced_with(ctx.client.clone(), df_ns, &secret_ar());

    // Read-compare-write. A no-op apply every pass forever would churn
    // resourceVersion and managedFields, and destroy the one cheap signal for
    // "when did this instance's ACL set last change".
    let live = api
        .get_opt(&secret_name)
        .await
        .ok()
        .flatten()
        .and_then(|s| {
            s.data
                .pointer(&format!("/data/{}", dragonfly::ACL_SECRET_KEY))
                .and_then(Value::as_str)
                .and_then(|b| base64::engine::general_purpose::STANDARD.decode(b).ok())
                .and_then(|v| String::from_utf8(v).ok())
        });

    if live.as_deref() != Some(contents.as_str()) {
        let body = dragonfly::acl_secret_object(&secret_name, df_ns, &contents);
        let pp = kube::api::PatchParams::apply(ACL_FIELD_MANAGER).force();
        if let Err(err) = api
            .patch(&secret_name, &pp, &kube::api::Patch::Apply(&body))
            .await
        {
            warn!(%instance, %err, "ACL file Secret write failed — retrying next pass");
            return;
        }
        info!(%instance, tenants = tenants.len(), "wrote durable ACL file");
    }

    // Only now may the CR name the Secret. Merge-patch, not SSA: the field is
    // a leaf on a CR the provisioner applies wholesale, and a path-scoped
    // patch cannot prune a sibling (the 1.83f rule).
    let df_api: Api<DynamicObject> = Api::namespaced_with(
        ctx.client.clone(),
        df_ns,
        &crate::reconcile::dragonfly_cluster_ar(),
    );
    let Ok(Some(cr)) = df_api.get_opt(instance).await else {
        return;
    };
    let already = cr
        .data
        .pointer("/spec/aclFromSecret/name")
        .and_then(Value::as_str)
        == Some(secret_name.as_str());
    if already {
        return;
    }
    let patch = serde_json::json!({
        "spec": { "aclFromSecret": { "name": secret_name, "key": dragonfly::ACL_SECRET_KEY } }
    });
    if let Err(err) = df_api
        .patch(
            instance,
            &kube::api::PatchParams::apply(ACL_FIELD_MANAGER),
            &kube::api::Patch::Merge(&patch),
        )
        .await
    {
        warn!(%instance, %err, "could not point the Dragonfly CR at its ACL file — retrying next pass");
    } else {
        info!(%instance, "Dragonfly CR now loads its ACL file at startup (one-time roll)");
    }
}

/// Refresh `status.size.keys` on one ready dragonfly claim (2.22d / D8).
///
/// # Why a key count and not bytes
///
/// Because a key count is the whole of what Dragonfly will honestly say
/// about a single logical DB. The full reasoning — which routes exist, which
/// look like sizes and are not, and what it would cost to get real bytes —
/// is on [`crate::redis_client::RedisAdmin::dbsize`]. The short of it: the
/// per-DB byte figures exist inside the server and are summed away at every
/// point they could reach a client.
///
/// The CLI therefore renders this as `N keys`, never as a size. A number
/// labelled in the wrong unit is worse than an absent one.
///
/// # Why it lives here
///
/// This loop already holds what the sample needs: the instance address and
/// its admin password, read once per instance rather than once per claim.
/// Its 300s cadence is also the right one — it matches the window the
/// Postgres scrape is cached for, so the two figures age alike.
///
/// Best-effort throughout. An unreachable instance, a failed `SELECT`, a
/// rejected patch: all are logged at debug and leave the previous figure
/// alone. It must never disturb the ACL re-pin that is this loop's actual
/// job, and it must never write a zero it did not measure — "not sampled"
/// and "empty" are different facts about a tenant's data.
async fn refresh_claim_keys(ctx: &Arc<Context>, addr: &str, admin_pw: &str, spec: &RepinSpec) {
    let keys = match ctx.redis.dbsize(addr, admin_pw, spec.dbnum).await {
        Ok(n) => n,
        Err(err) => {
            tracing::debug!(
                user = %spec.user, dbnum = spec.dbnum, %err,
                "DBSIZE sample failed — keeping the previous figure"
            );
            return;
        }
    };
    let now = chrono::Utc::now().to_rfc3339();
    if !crate::reconcile::keys_write_is_worth_it(spec.previous_size.as_ref(), keys, &now) {
        return;
    }
    let api: Api<ResourceClaim> = Api::namespaced(ctx.client.clone(), &spec.claim_namespace);
    let body = serde_json::json!({
        "apiVersion": "apprafter.io/v1alpha1",
        "kind": "ResourceClaim",
        "metadata": { "name": spec.claim_name },
        "status": { "size": { "keys": keys, "measuredAt": now } },
    });
    if let Err(e) = api
        .patch_status(
            &spec.claim_name,
            // DEDICATED manager — see `crate::SIZE_FIELD_MANAGER`. Under the
            // provisioner's own manager this size-only body would prune the
            // claim's entire allocation.
            &crate::reconcile::size_apply_params(),
            &kube::api::Patch::Apply(&body),
        )
        .await
    {
        tracing::debug!(
            name = %spec.claim_name, ns = %spec.claim_namespace, %e,
            "key-count status write failed (retrying next tick)"
        );
    }
}

/// Resolve the dragonfly namespace from the FIRST `ServiceProvider` whose
/// `spec.backend == "dragonfly"`, reading its `config.namespace`, falling
/// back to the well-known default. This is equivalent to
/// `provision_dragonfly`'s `config.namespace` read ONLY under the
/// single-dragonfly-provider assumption the loop below already documents —
/// `provision_dragonfly` reads the namespace off the claim's MATCHED
/// provider, whereas this picks the first dragonfly provider it finds. With
/// one dragonfly provider (the seeded `redis-integrated`) they are the same
/// namespace; with several they could differ.
///
/// `pub(crate)` so the GC (`gc.rs`) resolves the dragonfly namespace the
/// SAME way when reclaiming a snapshot — the RetainedClaim carries the
/// instance NAME but not its namespace, so both readers derive it from the
/// seeded ServiceProvider config (one source of truth).
pub(crate) async fn redis_namespace(ctx: &Arc<Context>) -> Result<String, ReconcileError> {
    let providers: Vec<operator_core::ServiceProvider> =
        Api::<operator_core::ServiceProvider>::all(ctx.client.clone())
            .list(&Default::default())
            .await?
            .items;
    Ok(redis_namespace_from(&providers))
}

/// The resolution itself, over a provider list the caller already holds.
///
/// Split out of [`redis_namespace`] so a caller that has ALREADY listed
/// providers does not list them a second time. The reaper (ADR 0042 §9) is
/// exactly that caller, and for it the second list was not merely wasteful:
/// it came from a DIFFERENT apiserver snapshot and was not truncation-
/// checked, on a path that deletes. `redis_namespace` now delegates here, so
/// the async and pure forms cannot drift apart.
///
/// Pure, so unlike the async wrapper it is table-testable.
pub(crate) fn redis_namespace_from(providers: &[operator_core::ServiceProvider]) -> String {
    providers
        .iter()
        .find(|p| p.spec.backend == "dragonfly")
        .and_then(|p| p.spec.config.as_ref())
        .and_then(|cfg| cfg.pointer("/namespace").and_then(Value::as_str))
        .unwrap_or("dragonfly-system")
        .to_string()
}

/// Read a single `data.<key>` value from a Secret, base64-decoded to a
/// UTF-8 string. (Secrets created with `stringData` come back base64 under
/// `data` on read.) Errors if the Secret or key is missing / not base64 /
/// not UTF-8.
///
/// `pub(crate)` so the GC reads a pool instance's admin password the same
/// way (the `{instance}-admin` Secret's `password` key).
pub(crate) async fn read_secret_key(
    ctx: &Arc<Context>,
    ns: &str,
    secret_name: &str,
    key: &str,
) -> Result<String, ReconcileError> {
    let api: Api<DynamicObject> = Api::namespaced_with(ctx.client.clone(), ns, &secret_ar());
    let secret = api.get(secret_name).await?;
    let raw = secret
        .data
        .pointer(&format!("/data/{key}"))
        .and_then(Value::as_str)
        .ok_or_else(|| {
            ReconcileError::Provisioning(format!("Secret {secret_name} missing data.{key}"))
        })?;
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(raw)
        .map_err(|e| {
            ReconcileError::Provisioning(format!("Secret {secret_name} {key} not base64: {e}"))
        })?;
    String::from_utf8(decoded).map_err(|e| {
        ReconcileError::Provisioning(format!("Secret {secret_name} {key} not UTF-8: {e}"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use operator_core::{ResourceClaimSpec, ResourceClaimStatus};
    use std::collections::BTreeMap;

    /// Build a redis `ResourceClaim` with the given allocation status.
    fn redis_claim(
        ns: &str,
        name: &str,
        instance: Option<&str>,
        dbnum: Option<u16>,
        ready: Option<bool>,
        conn: Option<&str>,
    ) -> ResourceClaim {
        let mut c = ResourceClaim::new(
            name,
            ResourceClaimSpec {
                type_: "redis".into(),
                name: None,
                selector: BTreeMap::new(),
                size: None,
                persistent: None,
            },
        );
        c.metadata.namespace = Some(ns.to_string());
        c.status = Some(ResourceClaimStatus {
            instance: instance.map(str::to_owned),
            dbnum,
            ready,
            connection_secret_ref: conn.map(str::to_owned),
            ..Default::default()
        });
        c
    }

    // --- claims_to_repin() ---

    #[test]
    fn claims_to_repin_returns_ready_allocated_claims_on_instance() {
        let claims = vec![redis_claim(
            "demo",
            "web-redis",
            Some("platform-redis-ephemeral-000"),
            Some(7),
            Some(true),
            Some("web-redis-conn"),
        )];
        let repin = claims_to_repin(&claims, "platform-redis-ephemeral-000");
        assert_eq!(
            repin,
            vec![RepinSpec {
                user: "claim_demo_web-redis_redis".to_string(),
                dbnum: 7,
                conn_secret_ref: "web-redis-conn".to_string(),
                claim_namespace: "demo".to_string(),
                claim_name: "web-redis".to_string(),
                previous_size: None,
            }]
        );
    }

    #[test]
    fn claims_to_repin_carries_the_previous_size_for_the_deadband() {
        // The size sample runs inside this loop, so the figure already on
        // the claim has to travel with the spec. Reading it back per claim
        // would be a second apiserver round-trip on a path that already
        // holds the object.
        let mut c = redis_claim(
            "demo",
            "web-redis",
            Some("platform-redis-ephemeral-000"),
            Some(7),
            Some(true),
            Some("web-redis-conn"),
        );
        c.status.as_mut().unwrap().size = Some(operator_core::ClaimSize {
            bytes: None,
            keys: Some(4200),
            measured_at: Some("2026-08-31T10:00:00+00:00".into()),
        });
        let repin = claims_to_repin(&[c], "platform-redis-ephemeral-000");
        assert_eq!(repin.len(), 1);
        assert_eq!(repin[0].claim_name, "web-redis");
        assert_eq!(
            repin[0].previous_size.as_ref().and_then(|s| s.keys),
            Some(4200)
        );
        // And never a byte figure: the CLI renders `bytes` as a size, so a
        // key count landing in that field would be shown in the wrong unit.
        assert_eq!(repin[0].previous_size.as_ref().and_then(|s| s.bytes), None);
    }

    #[test]
    fn claims_to_repin_excludes_other_instances() {
        let claims = vec![redis_claim(
            "demo",
            "web-redis",
            Some("platform-redis-ephemeral-001"),
            Some(7),
            Some(true),
            Some("web-redis-conn"),
        )];
        assert!(claims_to_repin(&claims, "platform-redis-ephemeral-000").is_empty());
    }

    #[test]
    fn claims_to_repin_excludes_not_ready() {
        // Mid-provision: allocated but not yet Ready → the provisioner owns
        // it, the re-pin loop must not race ahead and assert a half-built user.
        let claims = vec![redis_claim(
            "demo",
            "web-redis",
            Some("platform-redis-ephemeral-000"),
            Some(7),
            Some(false),
            Some("web-redis-conn"),
        )];
        assert!(claims_to_repin(&claims, "platform-redis-ephemeral-000").is_empty());
    }

    #[test]
    fn claims_to_repin_excludes_unallocated_or_no_secret() {
        let no_dbnum = redis_claim(
            "demo",
            "a",
            Some("platform-redis-ephemeral-000"),
            None,
            Some(true),
            Some("a-conn"),
        );
        let no_secret = redis_claim(
            "demo",
            "b",
            Some("platform-redis-ephemeral-000"),
            Some(1),
            Some(true),
            None,
        );
        let claims = vec![no_dbnum, no_secret];
        assert!(claims_to_repin(&claims, "platform-redis-ephemeral-000").is_empty());
    }

    #[test]
    fn claims_to_repin_excludes_non_redis_and_deleting() {
        // A cnpg (pg) claim — even with a coincidental instance status —
        // owns no dragonfly user.
        let mut pg = ResourceClaim::new(
            "web-pg",
            ResourceClaimSpec {
                type_: "pg".into(),
                name: None,
                selector: BTreeMap::new(),
                size: None,
                persistent: None,
            },
        );
        pg.metadata.namespace = Some("demo".into());
        pg.status = Some(ResourceClaimStatus {
            instance: Some("platform-redis-ephemeral-000".into()),
            dbnum: Some(3),
            ready: Some(true),
            connection_secret_ref: Some("web-pg-conn".into()),
            ..Default::default()
        });
        // A deleting redis claim — on its way out, the GC drops its user.
        let mut deleting = redis_claim(
            "demo",
            "gone",
            Some("platform-redis-ephemeral-000"),
            Some(4),
            Some(true),
            Some("gone-conn"),
        );
        deleting.metadata.deletion_timestamp = Some(
            k8s_openapi::apimachinery::pkg::apis::meta::v1::Time(chrono::Utc::now()),
        );
        let claims = vec![pg, deleting];
        assert!(claims_to_repin(&claims, "platform-redis-ephemeral-000").is_empty());
    }

    // --- allocated_instances() ---

    #[test]
    fn allocated_instances_returns_distinct_sorted() {
        let claims = vec![
            redis_claim(
                "demo",
                "a",
                Some("platform-redis-ephemeral-000"),
                Some(0),
                Some(true),
                Some("a-conn"),
            ),
            redis_claim(
                "demo",
                "b",
                Some("platform-redis-persistent-000"),
                Some(0),
                Some(true),
                Some("b-conn"),
            ),
            redis_claim(
                "demo",
                "c",
                Some("platform-redis-ephemeral-000"),
                Some(1),
                Some(true),
                Some("c-conn"),
            ),
            // Not ready — excluded.
            redis_claim(
                "demo",
                "d",
                Some("platform-redis-ephemeral-002"),
                Some(0),
                Some(false),
                Some("d-conn"),
            ),
        ];
        assert_eq!(
            allocated_instances(&claims),
            vec![
                "platform-redis-ephemeral-000".to_string(),
                "platform-redis-persistent-000".to_string(),
            ]
        );
    }

    #[test]
    fn allocated_instances_empty_when_no_redis_claims() {
        assert!(allocated_instances(&[]).is_empty());
    }

    // --- password_from_redis_url() ---

    #[test]
    fn password_from_redis_url_extracts_userinfo_password() {
        let dsn =
            "redis://claim_demo_web_redis:s3cr3t@platform-redis-ephemeral-000.dragonfly-system.svc:6379/7";
        assert_eq!(password_from_redis_url(dsn).as_deref(), Some("s3cr3t"));
    }

    #[test]
    fn password_from_redis_url_decodes_percent_escapes() {
        // A password with reserved chars (`@`, `:`, `/`) percent-encoded.
        let dsn = "redis://user:p%40ss%3Aword@host:6379/3";
        assert_eq!(password_from_redis_url(dsn).as_deref(), Some("p@ss:word"));
    }

    #[test]
    fn password_from_redis_url_handles_password_with_at_and_colon() {
        // A literal '@' in the password: split on the LAST '@' for host,
        // FIRST ':' for the user:pass boundary.
        let dsn = "redis://user:pa:ss@word@host:6379/1";
        assert_eq!(password_from_redis_url(dsn).as_deref(), Some("pa:ss@word"));
    }

    #[test]
    fn password_from_redis_url_none_when_no_password() {
        assert_eq!(password_from_redis_url("redis://host:6379/0"), None);
        assert_eq!(password_from_redis_url("redis://user@host:6379/0"), None);
        // Empty password component.
        assert_eq!(password_from_redis_url("redis://user:@host:6379/0"), None);
    }

    // --- redis_namespace_from() ---

    fn dragonfly_provider(config: Option<serde_json::Value>) -> operator_core::ServiceProvider {
        operator_core::ServiceProvider::new(
            "redis-integrated",
            operator_core::ServiceProviderSpec {
                type_: "redis".into(),
                backend: "dragonfly".into(),
                config,
            },
        )
    }

    #[test]
    fn redis_namespace_from_reads_the_config_override() {
        let providers = vec![dragonfly_provider(Some(
            serde_json::json!({ "namespace": "tenant-dragonfly" }),
        ))];
        assert_eq!(redis_namespace_from(&providers), "tenant-dragonfly");
    }

    #[test]
    fn redis_namespace_from_falls_back_to_the_well_known_default() {
        // No providers at all, a dragonfly provider with no config, and one
        // whose config omits `/namespace` all resolve to the default.
        assert_eq!(redis_namespace_from(&[]), "dragonfly-system");
        assert_eq!(
            redis_namespace_from(&[dragonfly_provider(None)]),
            "dragonfly-system"
        );
        assert_eq!(
            redis_namespace_from(&[dragonfly_provider(Some(
                serde_json::json!({ "other": "field" })
            ))]),
            "dragonfly-system"
        );
    }

    #[test]
    fn redis_namespace_from_ignores_non_dragonfly_backends() {
        // A cnpg provider listed FIRST must not shadow the dragonfly one —
        // the match is on `spec.backend`, not on list position.
        let pg = operator_core::ServiceProvider::new(
            "pg-integrated",
            operator_core::ServiceProviderSpec {
                type_: "pg".into(),
                backend: "cloudnative-pg".into(),
                config: Some(serde_json::json!({ "namespace": "cnpg-system" })),
            },
        );
        let providers = vec![
            pg,
            dragonfly_provider(Some(serde_json::json!({ "namespace": "df-ns" }))),
        ];
        assert_eq!(redis_namespace_from(&providers), "df-ns");
    }

    #[test]
    fn percent_decode_passes_through_invalid_escape() {
        // A bare '%' with no valid hex pair survives verbatim (never panics).
        assert_eq!(percent_decode("ab%zz"), "ab%zz");
        assert_eq!(percent_decode("trailing%"), "trailing%");
    }
}
