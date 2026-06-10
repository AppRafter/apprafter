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

/// One claim's ACL re-pin payload, derived purely from a live
/// `ResourceClaim` (no I/O). The loop turns each of these into an `ACL
/// SETUSER` after it recovers the password from the connection Secret.
#[derive(Debug, Clone, PartialEq, Eq)]
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
            let ns = c.namespace().unwrap_or_default();
            let name = c.name_any();
            Some(RepinSpec {
                user: dragonfly::acl_user(&ns, &name),
                dbnum,
                conn_secret_ref,
                claim_namespace: ns,
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
        tokio::time::sleep(RESYNC_INTERVAL).await;
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

    let instances = allocated_instances(&claims);
    if instances.is_empty() {
        return Ok(());
    }
    for instance in instances {
        if let Err(err) = reconcile_instance_acls(ctx, &instance, &claims).await {
            warn!(%instance, %err, "ACL re-pin for instance failed — skipping; retried next tick");
        }
    }
    Ok(())
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
) -> Result<(), ReconcileError> {
    let to_repin = claims_to_repin(claims, instance);
    if to_repin.is_empty() {
        return Ok(());
    }

    let df_ns = redis_namespace(ctx).await?;
    let addr = dragonfly::instance_addr(instance, &df_ns);
    let admin_secret_name = dragonfly::admin_secret_name(instance);
    let admin_pw = read_secret_key(ctx, &df_ns, &admin_secret_name, "password").await?;

    info!(
        %instance, %df_ns, count = to_repin.len(),
        "re-pinning dragonfly claim ACL users (resync)"
    );

    for spec in to_repin {
        // Recover this claim's password from its connection Secret's `pass`
        // key (2.12 decomposed format; the Secret is written by
        // `redis_connection_secret_object`). A missing Secret / key is
        // non-fatal — skip + retry.
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
                    "could not read connection Secret `pass` — skipping re-pin this tick"
                );
                continue;
            }
        };

        let args = dragonfly::acl_setuser_args(&spec.user, &password, spec.dbnum);
        if let Err(err) = ctx.redis.acl_setuser(&addr, &admin_pw, &args).await {
            warn!(
                user = %spec.user, %instance, %err,
                "ACL SETUSER re-pin failed — skipping; retried next tick"
            );
            continue;
        }
    }
    Ok(())
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
    let ns = providers
        .iter()
        .find(|p| p.spec.backend == "dragonfly")
        .and_then(|p| p.spec.config.as_ref())
        .and_then(|cfg| cfg.pointer("/namespace").and_then(Value::as_str))
        .unwrap_or("dragonfly-system")
        .to_string();
    Ok(ns)
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
            }]
        );
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

    #[test]
    fn percent_decode_passes_through_invalid_escape() {
        // A bare '%' with no valid hex pair survives verbatim (never panics).
        assert_eq!(percent_decode("ab%zz"), "ab%zz");
        assert_eq!(percent_decode("trailing%"), "trailing%");
    }
}
