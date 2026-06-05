// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Pure Dragonfly builders + the dbnum allocator. I/O-free; the
//! reconcile loop and the redis_client seam call these. ADR 0042.
//!
//! Every function here is pure (`-> serde_json::Value` / `String` /
//! `Option` / `BTreeSet`), so the whole module is unit-testable without a
//! cluster. The reconcile loop (`reconcile.rs`) wires these into SSA-applies
//! of the lazily-created shared `Dragonfly` CR + the imperative Redis I/O
//! seam (`redis_client.rs`) that drives the per-claim `$N` ACL user.

use std::collections::BTreeSet;

use serde_json::{json, Value};

use operator_core::{ResourceClaim, RetainedClaim};

/// Lowest free DB number `< max` not in `used`, or None if the instance
/// is full (the signal to grow the pool — ADR 0042 §3). DB 0 is
/// allocatable; the platform reserves nothing there for redis.
pub fn allocate_dbnum(used: &BTreeSet<u16>, max: u16) -> Option<u16> {
    (0..max).find(|n| !used.contains(n))
}

/// The outcome of resolving a dragonfly claim's DB allocation (ADR 0042
/// §8). Pure, so the reattach-vs-fresh decision is unit-pinned away from
/// the I/O path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution {
    /// An existing `RetainedClaim` for THIS claim is still within grace —
    /// reattach to its original `(instance, dbnum)` rather than allocating
    /// fresh, recovering the retained data (ADR 0042 §8). `skip_flush` is
    /// `true` on a persistent instance (the snapshot preserved real data we
    /// must keep) and `false` on an ephemeral instance (it holds nothing,
    /// so flushing is harmless and keeps the recycle-safety invariant).
    Reattach {
        instance: String,
        dbnum: u16,
        skip_flush: bool,
    },
    /// No retained snapshot for this claim — allocate a fresh DB. The
    /// provision path FLUSHDBs it (recycle-safety, ADR 0042 §3).
    Fresh { dbnum: u16 },
    /// No retained snapshot AND the instance is full — grow the pool.
    Insufficient,
}

/// Decide a dragonfly claim's DB allocation (ADR 0042 §8).
///
/// `existing` is the `(instance, dbnum)` of a `RetainedClaim` snapshotted
/// for THIS claim under its deterministic name, if one is still pending
/// (i.e. the claim was deleted and re-created within the 7-day grace).
/// `persistent` is the claim's persistence class.
///
///   - `existing = Some((i, n))` → `Reattach { i, n, skip_flush =
///     persistent }`. Reusing the same DB on a PERSISTENT instance recovers
///     the retained data (don't flush); on an EPHEMERAL instance there is no
///     data to recover, so flush as usual.
///   - `existing = None` → `Fresh` with the lowest free DB, or
///     `Insufficient` when the instance is full.
///
/// The provision path, on `Reattach`, reuses `(instance, dbnum)`, flushes
/// only when NOT `skip_flush`, and DELETEs the now-stale `RetainedClaim`
/// (404-tolerant) so the GC can never reclaim the re-attached, live claim.
pub fn resolve_allocation(
    existing: Option<(String, u16)>,
    persistent: bool,
    used: &BTreeSet<u16>,
    max: u16,
) -> Resolution {
    match existing {
        Some((instance, dbnum)) => Resolution::Reattach {
            instance,
            dbnum,
            skip_flush: persistent,
        },
        None => match allocate_dbnum(used, max) {
            Some(dbnum) => Resolution::Fresh { dbnum },
            None => Resolution::Insufficient,
        },
    }
}

/// Per-persistence-class pool instance name, zero-padded index.
pub fn pool_instance_name(persistent: bool, index: u32) -> String {
    let class = if persistent {
        "persistent"
    } else {
        "ephemeral"
    };
    format!("platform-redis-{class}-{index:03}")
}

/// Deterministic ACL username for a claim (DNS-ish, redis-safe).
pub fn acl_user(claim_ns: &str, claim_name: &str) -> String {
    format!("claim_{claim_ns}_{claim_name}_redis")
}

/// `metadata.name` of a pool instance's admin-password Secret. The
/// provisioner creates one per shared `Dragonfly` and references it from
/// the CR via `authentication.passwordFromSecret`.
pub fn admin_secret_name(instance: &str) -> String {
    format!("{instance}-admin")
}

/// In-cluster service address (`host:port`, no scheme) of a pool
/// instance. The dragonfly-operator fronts each `Dragonfly` with a
/// Service named after the CR.
pub fn instance_addr(instance: &str, ns: &str) -> String {
    format!("{instance}.{ns}.svc:6379")
}

/// `ACL SETUSER` argument vector for a `$N`-pinned, keyspace-isolated
/// claim user (ADR 0042 §2). Hard DB boundary via `$N`; ordinary key
/// names via `~*` (scoped to DB N by the pin); pub/sub confined to the
/// claim's channel prefix; admin/dangerous blocked, safe introspection
/// re-granted. Scripting (`@scripting`) is retained (the `+@all` grant
/// keeps it; declared-keys default + the `$N` pin confine any script to
/// the claim's DB). Driven over the Redis client (`redis_client.rs`), not
/// the CR. `resetkeys` / `resetchannels` clear any inherited grants
/// before re-applying, so a re-pin (instance restart) is idempotent.
///
/// `MOVE` and `COPY` name a DESTINATION DB index as a command argument and
/// are NOT members of `@admin` or `@dangerous` (only `SWAPDB` is in
/// `@dangerous`), so `+@all -@admin -@dangerous` would leave them GRANTED —
/// a cross-DB escape past the `$N` pin. Deny both explicitly; queue / cache
/// / pub-sub workloads never need them. `SWAPDB` stays denied via
/// `@dangerous`.
pub fn acl_setuser_args(user: &str, password: &str, dbnum: u16) -> Vec<String> {
    vec![
        user.to_string(),
        "on".into(),
        format!(">{password}"),
        format!("${dbnum}"),
        "resetkeys".into(),
        "~*".into(),
        "resetchannels".into(),
        format!("&{user}:*"),
        "+@all".into(),
        "-@admin".into(),
        "-@dangerous".into(),
        "-move".into(),
        "-copy".into(),
        "+info".into(),
        "+sort_ro".into(),
        "+client|setname".into(),
        "+client|setinfo".into(),
        "+client|getname".into(),
        "+client|id".into(),
    ]
}

/// DB-pinned connection URL. The `/N` selects DB N; the `$N` ACL pins the
/// user there, so the app uses ordinary key names (no prefix). The host
/// is the pool instance's in-cluster Service.
pub fn redis_dsn(user: &str, password: &str, instance: &str, ns: &str, dbnum: u16) -> String {
    format!("redis://{user}:{password}@{instance}.{ns}.svc:6379/{dbnum}")
}

/// Pub/sub channel name prefix the app must apply (the `&{user}:*` ACL
/// enforces it). Keys need no prefix — only channels are not DB-scoped,
/// so the channel namespace is shared across DBs on one instance and must
/// be partitioned by the user prefix.
pub fn channel_prefix(user: &str) -> String {
    format!("{user}:")
}

/// Build a shared Dragonfly CR body for SSA apply. `persistent` adds a
/// snapshot→PVC block (whole-instance durability; ADR 0042 §6). The
/// provisioner creates a per-instance admin-password Secret separately
/// and references it via `authentication.passwordFromSecret`.
pub fn dragonfly_object(
    name: &str,
    ns: &str,
    dbnum: u16,
    num_shards: u16,
    persistent: bool,
) -> Value {
    let mut spec = json!({
        "args": [
            format!("--dbnum={dbnum}"),
            format!("--num_shards={num_shards}"),
            "--maxmemory_policy=noeviction",
        ],
        "authentication": {
            "passwordFromSecret": { "name": admin_secret_name(name), "key": "password" }
        },
    });
    if persistent {
        spec["snapshot"] = json!({
            "cron": "*/30 * * * *",
            "persistentVolumeClaimSpec": {
                "accessModes": ["ReadWriteOnce"],
                "resources": { "requests": { "storage": "10Gi" } }
            }
        });
    }
    json!({
        "apiVersion": "dragonflydb.io/v1alpha1",
        "kind": "Dragonfly",
        "metadata": { "name": name, "namespace": ns },
        "spec": spec,
    })
}

/// Build the admin-password Secret for a pool instance: an `Opaque`
/// Secret carrying a single `password` key the `Dragonfly` CR references
/// via `authentication.passwordFromSecret`. Sole-owned by the provisioner
/// (no ownerRef — the instance is platform infrastructure, not tenant-
/// owned), labelled for inventory.
pub fn admin_secret_object(name: &str, ns: &str, password: &str) -> Value {
    json!({
        "apiVersion": "v1",
        "kind": "Secret",
        "metadata": {
            "name": name,
            "namespace": ns,
            "labels": {
                "apprafter.io/managed-by": "apprafter",
            },
        },
        "type": "Opaque",
        "stringData": {
            "password": password,
        },
    })
}

/// The set of DB numbers already allocated on `instance`, read from the
/// live `ResourceClaim` source of truth (`status.instance` /
/// `status.dbnum`). Race-free: allocation always re-scans live claims, so
/// two concurrent provisions cannot pick the same DB once the first one's
/// status lands. This returns **every** matching dbnum, including the one
/// held by the claim currently being provisioned — there is no
/// exclude-by-name parameter or exclusion logic. Idempotency is the
/// caller's responsibility: a re-reconcile must look up its own prior
/// allocation (`status.dbnum`) and reuse it *before* calling this, or it
/// would treat its own DB as taken and pick a different one. See the
/// `existing_alloc` short-circuit in `provision_dragonfly`.
pub fn used_dbnums_on_instance(claims: &[ResourceClaim], instance: &str) -> BTreeSet<u16> {
    claims
        .iter()
        .filter_map(|c| {
            let st = c.status.as_ref()?;
            if st.instance.as_deref() == Some(instance) {
                st.dbnum
            } else {
                None
            }
        })
        .collect()
}

/// The set of DB numbers RESERVED on `instance` — the union of (a) every
/// LIVE `ResourceClaim`'s `status.dbnum` whose `status.instance` matches
/// (the existing live source of truth) and (b) every `RetainedClaim`'s
/// `spec.dbnum` whose `spec.instance` matches (the ADR 0042 §8 reservation
/// of freed-but-still-in-grace DBs).
///
/// Why retained DBs MUST be reserved (data-loss bug otherwise): when a
/// claim is deleted its DB is snapshotted into a `RetainedClaim` and held
/// for the 7-day grace. If the allocator looked at LIVE claims only, that
/// freed dbnum would be reusable immediately — and the snapshot's grace-GC
/// later runs `FLUSHDB` on the number, wiping whichever NEW tenant recycled
/// it (cross-tenant data loss) while a stale credential could still reach
/// the new tenant. Every *existing* `RetainedClaim` is within grace by
/// definition (the GC deletes it the moment grace elapses), so reserving
/// all of them implements the §8 reservation without a per-snapshot
/// deadline check here. The provision path LISTs `RetainedClaim`s in
/// `apprafter-system` and passes them in.
pub fn used_dbnums(
    live: &[ResourceClaim],
    retained: &[RetainedClaim],
    instance: &str,
) -> BTreeSet<u16> {
    let mut used = used_dbnums_on_instance(live, instance);
    used.extend(retained.iter().filter_map(|r| {
        if r.spec.instance.as_deref() == Some(instance) {
            r.spec.dbnum
        } else {
            None
        }
    }));
    used
}

#[cfg(test)]
mod tests {
    use super::*;
    use operator_core::{ResourceClaimStatus, RetainedClaim};
    use std::collections::BTreeSet;

    // --- allocate_dbnum() ---

    #[test]
    fn allocate_dbnum_picks_lowest_free() {
        let used: BTreeSet<u16> = [0u16, 1, 3].into_iter().collect();
        assert_eq!(allocate_dbnum(&used, 1024), Some(2));
    }

    #[test]
    fn allocate_dbnum_skips_zero_when_taken_and_returns_first_gap() {
        let used: BTreeSet<u16> = (0u16..5).collect();
        assert_eq!(allocate_dbnum(&used, 1024), Some(5));
    }

    #[test]
    fn allocate_dbnum_none_when_full() {
        let used: BTreeSet<u16> = (0u16..1024).collect();
        assert_eq!(allocate_dbnum(&used, 1024), None);
    }

    #[test]
    fn allocate_dbnum_picks_zero_when_unused() {
        let used: BTreeSet<u16> = BTreeSet::new();
        assert_eq!(allocate_dbnum(&used, 1024), Some(0));
    }

    // --- pool_instance_name() ---

    #[test]
    fn instance_name_is_per_class_indexed() {
        assert_eq!(pool_instance_name(false, 0), "platform-redis-ephemeral-000");
        assert_eq!(pool_instance_name(true, 0), "platform-redis-persistent-000");
        assert_eq!(
            pool_instance_name(true, 12),
            "platform-redis-persistent-012"
        );
    }

    // --- acl_user() ---

    #[test]
    fn acl_user_is_deterministic_and_redis_safe() {
        assert_eq!(acl_user("demo", "web-redis"), "claim_demo_web-redis_redis");
    }

    // --- admin_secret_name() / instance_addr() ---

    #[test]
    fn admin_secret_name_suffixes_instance() {
        assert_eq!(
            admin_secret_name("platform-redis-ephemeral-000"),
            "platform-redis-ephemeral-000-admin"
        );
    }

    #[test]
    fn instance_addr_is_service_host_port() {
        assert_eq!(
            instance_addr("platform-redis-ephemeral-000", "dragonfly-system"),
            "platform-redis-ephemeral-000.dragonfly-system.svc:6379"
        );
    }

    // --- dragonfly_object() ---

    #[test]
    fn dragonfly_cr_sets_dbnum_and_shards_and_persistence() {
        let cr = dragonfly_object(
            "platform-redis-persistent-000",
            "dragonfly-system",
            1024,
            1,
            true,
        );
        assert_eq!(cr["apiVersion"], "dragonflydb.io/v1alpha1");
        assert_eq!(cr["kind"], "Dragonfly");
        assert_eq!(cr["metadata"]["name"], "platform-redis-persistent-000");
        assert_eq!(cr["metadata"]["namespace"], "dragonfly-system");
        let args = cr["spec"]["args"].as_array().unwrap();
        assert!(args.iter().any(|a| a == "--dbnum=1024"));
        assert!(args.iter().any(|a| a == "--num_shards=1"));
        assert!(args.iter().any(|a| a == "--maxmemory_policy=noeviction"));
        // The admin password Secret is referenced by name.
        assert_eq!(
            cr["spec"]["authentication"]["passwordFromSecret"]["name"],
            "platform-redis-persistent-000-admin"
        );
        assert_eq!(
            cr["spec"]["authentication"]["passwordFromSecret"]["key"],
            "password"
        );
        // persistent → snapshot to a PVC; ephemeral → no snapshot block.
        assert!(cr["spec"]["snapshot"]["persistentVolumeClaimSpec"].is_object());
        let eph = dragonfly_object(
            "platform-redis-ephemeral-000",
            "dragonfly-system",
            1024,
            1,
            false,
        );
        assert!(eph["spec"].get("snapshot").is_none());
    }

    // --- admin_secret_object() ---

    #[test]
    fn admin_secret_carries_the_password_key() {
        let s = admin_secret_object(
            "platform-redis-ephemeral-000-admin",
            "dragonfly-system",
            "s3cr3t",
        );
        assert_eq!(s["apiVersion"], "v1");
        assert_eq!(s["kind"], "Secret");
        assert_eq!(s["metadata"]["name"], "platform-redis-ephemeral-000-admin");
        assert_eq!(s["metadata"]["namespace"], "dragonfly-system");
        assert_eq!(s["type"], "Opaque");
        assert_eq!(s["stringData"]["password"], "s3cr3t");
        assert_eq!(
            s["metadata"]["labels"]["apprafter.io/managed-by"],
            "apprafter"
        );
    }

    // --- used_dbnums_on_instance() ---

    fn claim_with_alloc(name: &str, instance: Option<&str>, dbnum: Option<u16>) -> ResourceClaim {
        let mut c = ResourceClaim::new(
            name,
            operator_core::ResourceClaimSpec {
                type_: "redis".into(),
                selector: Default::default(),
                size: None,
                persistent: None,
            },
        );
        c.status = Some(ResourceClaimStatus {
            instance: instance.map(str::to_owned),
            dbnum,
            ..Default::default()
        });
        c
    }

    #[test]
    fn used_dbnums_reads_only_matching_instance() {
        let claims = vec![
            claim_with_alloc("a", Some("platform-redis-ephemeral-000"), Some(0)),
            claim_with_alloc("b", Some("platform-redis-ephemeral-000"), Some(2)),
            // Different instance — must be excluded.
            claim_with_alloc("c", Some("platform-redis-ephemeral-001"), Some(0)),
            // No allocation yet — must be excluded.
            claim_with_alloc("d", None, None),
        ];
        let used = used_dbnums_on_instance(&claims, "platform-redis-ephemeral-000");
        assert_eq!(used, [0u16, 2].into_iter().collect());
    }

    #[test]
    fn used_dbnums_empty_when_no_claims_on_instance() {
        let claims = vec![claim_with_alloc(
            "a",
            Some("platform-redis-ephemeral-001"),
            Some(0),
        )];
        let used = used_dbnums_on_instance(&claims, "platform-redis-ephemeral-000");
        assert!(used.is_empty());
    }

    // --- used_dbnums() (live + retained union; ADR 0042 §8 reservation) ---

    fn retained_with_alloc(name: &str, instance: &str, dbnum: u16) -> RetainedClaim {
        RetainedClaim::new(
            name,
            operator_core::RetainedClaimSpec {
                claim_ref: operator_core::retainedclaim::ClaimRef {
                    name: name.into(),
                    namespace: "demo".into(),
                },
                provider: "redis-integrated".into(),
                backend: "dragonfly".into(),
                instance: Some(instance.to_owned()),
                dbnum: Some(dbnum),
                retain_until: "2026-06-12T00:00:00+00:00".into(),
                ..Default::default()
            },
        )
    }

    #[test]
    fn used_dbnums_unions_live_and_retained_on_instance() {
        let live = vec![
            claim_with_alloc("a", Some("platform-redis-ephemeral-000"), Some(0)),
            claim_with_alloc("b", Some("platform-redis-ephemeral-000"), Some(2)),
        ];
        let retained = vec![
            // A freed dbnum still within its 7-day grace — MUST be reserved so
            // the snapshot grace-GC never FLUSHDBs a recycled, now-live DB.
            retained_with_alloc("ret-a", "platform-redis-ephemeral-000", 5),
            // A retained dbnum on a DIFFERENT instance — must NOT be reserved.
            retained_with_alloc("ret-b", "platform-redis-ephemeral-001", 9),
        ];
        let used = used_dbnums(&live, &retained, "platform-redis-ephemeral-000");
        assert_eq!(used, [0u16, 2, 5].into_iter().collect());
    }

    #[test]
    fn used_dbnums_reserves_retained_even_with_no_live_claims() {
        let retained = vec![retained_with_alloc(
            "ret-a",
            "platform-redis-ephemeral-000",
            3,
        )];
        let used = used_dbnums(&[], &retained, "platform-redis-ephemeral-000");
        assert_eq!(used, [3u16].into_iter().collect());
    }

    #[test]
    fn used_dbnums_excludes_retained_on_other_instance() {
        let retained = vec![retained_with_alloc(
            "ret-b",
            "platform-redis-ephemeral-001",
            9,
        )];
        let used = used_dbnums(&[], &retained, "platform-redis-ephemeral-000");
        assert!(used.is_empty());
    }

    // --- resolve_allocation() (ADR 0042 §8 reattach vs fresh) ---

    #[test]
    fn resolve_allocation_reattaches_persistent_without_flush() {
        // A persistent instance keeps the retained data, so reattach reuses
        // the SAME (instance, dbnum) and must NOT flush.
        let used: BTreeSet<u16> = [0u16, 1].into_iter().collect();
        let r = resolve_allocation(
            Some(("platform-redis-persistent-000".into(), 4)),
            true,
            &used,
            1024,
        );
        assert_eq!(
            r,
            Resolution::Reattach {
                instance: "platform-redis-persistent-000".into(),
                dbnum: 4,
                skip_flush: true,
            }
        );
    }

    #[test]
    fn resolve_allocation_reattaches_ephemeral_with_flush() {
        // An ephemeral instance holds no retained data, so reattach reuses
        // the (instance, dbnum) but DOES flush (skip_flush = false).
        let used: BTreeSet<u16> = BTreeSet::new();
        let r = resolve_allocation(
            Some(("platform-redis-ephemeral-000".into(), 7)),
            false,
            &used,
            1024,
        );
        assert_eq!(
            r,
            Resolution::Reattach {
                instance: "platform-redis-ephemeral-000".into(),
                dbnum: 7,
                skip_flush: false,
            }
        );
    }

    #[test]
    fn resolve_allocation_fresh_when_no_existing_retained_claim() {
        // No retained snapshot for this claim → allocate the lowest free DB.
        let used: BTreeSet<u16> = [0u16, 1, 3].into_iter().collect();
        let r = resolve_allocation(None, false, &used, 1024);
        assert_eq!(r, Resolution::Fresh { dbnum: 2 });
    }

    #[test]
    fn resolve_allocation_insufficient_when_instance_full() {
        // No existing snapshot AND the instance is full → grow the pool.
        let used: BTreeSet<u16> = (0u16..8).collect();
        let r = resolve_allocation(None, true, &used, 8);
        assert_eq!(r, Resolution::Insufficient);
    }

    // --- acl_setuser_args() ---

    #[test]
    fn acl_setuser_args_pin_db_and_keyspace() {
        let args = acl_setuser_args("claim_demo_web_redis", "s3cr3t", 7);
        // ACL SETUSER <user> on >pw $7 resetkeys ~* resetchannels
        //   &claim_demo_web_redis:* +@all -@admin -@dangerous -move -copy
        //   +info +sort_ro ...
        assert_eq!(args[0], "claim_demo_web_redis");
        assert!(args.iter().any(|a| a == "on"));
        assert!(args.iter().any(|a| a == ">s3cr3t"));
        assert!(args.iter().any(|a| a == "$7"));
        assert!(args.iter().any(|a| a == "resetkeys"));
        assert!(args.iter().any(|a| a == "~*"));
        assert!(args.iter().any(|a| a == "resetchannels"));
        assert!(args.iter().any(|a| a == "&claim_demo_web_redis:*"));
        assert!(args.iter().any(|a| a == "+@all"));
        assert!(args.iter().any(|a| a == "-@admin"));
        assert!(args.iter().any(|a| a == "-@dangerous"));
        assert!(args.iter().any(|a| a == "+info"));
        // Cross-DB escape commands (MOVE/COPY) name a DESTINATION DB outside
        // the `$N` pin and are NOT in @admin/@dangerous (only SWAPDB is), so
        // they must be denied explicitly. SWAPDB stays denied via @dangerous.
        assert!(
            args.iter().any(|a| a == "-move"),
            "MOVE escapes the $N pin — must be denied"
        );
        assert!(
            args.iter().any(|a| a == "-copy"),
            "COPY escapes the $N pin — must be denied"
        );
    }

    // --- redis_dsn() ---

    #[test]
    fn redis_dsn_pins_db_number() {
        let dsn = redis_dsn(
            "claim_demo_web_redis",
            "s3cr3t",
            "platform-redis-ephemeral-000",
            "dragonfly-system",
            7,
        );
        assert_eq!(
            dsn,
            "redis://claim_demo_web_redis:s3cr3t@platform-redis-ephemeral-000.dragonfly-system.svc:6379/7"
        );
    }

    // --- channel_prefix() ---

    #[test]
    fn channel_prefix_matches_acl() {
        assert_eq!(
            channel_prefix("claim_demo_web_redis"),
            "claim_demo_web_redis:"
        );
    }
}
