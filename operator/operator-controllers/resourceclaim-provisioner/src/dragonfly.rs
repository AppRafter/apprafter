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

use operator_core::ResourceClaim;

/// Lowest free DB number `< max` not in `used`, or None if the instance
/// is full (the signal to grow the pool — ADR 0042 §3). DB 0 is
/// allocatable; the platform reserves nothing there for redis.
pub fn allocate_dbnum(used: &BTreeSet<u16>, max: u16) -> Option<u16> {
    (0..max).find(|n| !used.contains(n))
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

#[cfg(test)]
mod tests {
    use super::*;
    use operator_core::ResourceClaimStatus;
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
}
