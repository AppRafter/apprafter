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

use crate::cnpg::BackendResources;
use operator_core::{ResourceClaim, RetainedClaim};

/// Dragonfly's self-imposed RSS cap (`--maxmemory` server flag), sized BELOW
/// the `BackendResources::dragonfly_t1` 320Mi cgroup limit so the process
/// caps itself with headroom before the kernel OOM-kills the pod (ADR 0042 /
/// docs/measurements/2.16d-baseline-2026-08-08.md). This is the real Dragonfly
/// memory-limit flag — distinct from the rejected `--maxmemory-policy` /
/// `--maxmemory_policy` Redis-ism that crash-loops the binary.
const DRAGONFLY_MAXMEMORY: &str = "256mb";

/// The Dragonfly SERVER image, pinned by ADR 0042 §10. Bump deliberately and
/// re-verify §10's ACL facts when you do — they are properties of this tag,
/// not of Dragonfly in general.
pub const DRAGONFLY_SERVER_IMAGE: &str = "docker.dragonflydb.io/dragonflydb/dragonfly:v1.37.0";

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

/// Persistence class of a Dragonfly pool instance.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PoolClass {
    Ephemeral,
    Persistent,
}

/// Parse a pool-instance name back into its class — the exact inverse of
/// [`pool_instance_name`].
///
/// `None` for any name that is not one of ours. This is a SAFETY boundary,
/// not a convenience: the reaper (ADR 0042 §9) deletes by name, so anything
/// failing to parse here belongs to a user or another system and must never
/// be touched. Keep it strict — a loose `contains("-persistent-")` would
/// match a user's own `my-persistent-cache`.
pub fn class_of_instance(name: &str) -> Option<PoolClass> {
    let rest = name.strip_prefix("platform-redis-")?;
    let (class, index) = rest.rsplit_once('-')?;
    if index.is_empty() || !index.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    match class {
        "ephemeral" => Some(PoolClass::Ephemeral),
        "persistent" => Some(PoolClass::Persistent),
        _ => None,
    }
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
        // ADR 0042 §11: `+info` was re-granted in 2.6 on a "trusted
        // single-tenant" premise that `POOL_INSTANCE_INDEX = 0` had already
        // falsified — one instance serves every tenant in the cluster.
        // `INFO KEYSPACE` emits a line per NON-EMPTY database regardless of
        // the selected one, so it enumerates which tenants hold data, with
        // their key counts. The ACL grammar cannot scope `INFO` to a section
        // or a DB (`+info|keyspace` does not resolve), so it is on or off.
        //
        // `-pubsub` is the same disclosure, larger. `PUBSUB` is registered
        // SLOW-only, so it survives `-@admin -@dangerous`, and the
        // `&{user}:*` patterns constrain what a user may SUBSCRIBE to, not
        // what `PUBSUB CHANNELS` RETURNS. Channel names are
        // `claim_<ns>_<app>_redis:`, so `PUBSUB CHANNELS '*'` hands back the
        // namespace and application name of every pub/sub tenant in
        // plaintext. Pub/sub itself is untouched: `PUBSUB` is a distinct
        // command from `SUBSCRIBE` / `PUBLISH`.
        //
        // Cost, in full: a BullMQ tenant needs `skipVersionCheck: true`,
        // because its version probe is unconditional. ioredis >= 5 has a
        // `NOPERM` branch and degrades to a warning; node-redis, redis-py,
        // go-redis, lettuce and jedis never call `INFO` on connect.
        "-pubsub".into(),
        "+sort_ro".into(),
        // No per-subcommand `+client|setname` grants: Dragonfly's ACL parser
        // rejects the `command|subcommand` form ("Unrecognized parameter
        // +CLIENT|SETNAME"), and they are unnecessary — `+@all -@dangerous`
        // already leaves CLIENT SETNAME/SETINFO/GETNAME/ID available, so a
        // client library's connection init works (verified on Dragonfly
        // v1.37.0). EVAL and the `$N` keyspace pin are likewise retained.
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
///
/// The CR carries the `apprafter.io/managed-by` ownership stamp — the same
/// label [`admin_secret_object`] puts on the admin Secret. This is
/// LOAD-BEARING for the reaper (ADR 0042 §9), not inventory decoration; see
/// the comment on the label itself.
pub fn dragonfly_object(
    name: &str,
    ns: &str,
    dbnum: u16,
    num_shards: u16,
    replicas: u16,
    persistent: bool,
    res: &BackendResources,
) -> Value {
    // `replicas` is MANDATORY: the dragonfly-operator sets
    // `StatefulSet.spec.replicas = &df.Spec.Replicas` with NO default, so an
    // omitted/zero value yields a 0-replica StatefulSet (no instance pod), and
    // the provisioner can never reach the instance to create the ACL user.
    //
    // No `--maxmemory_policy` arg: that is a Redis-ism Dragonfly does NOT
    // accept (the binary exits "Unknown command line flag 'maxmemory_policy'").
    // Dragonfly does not evict by default (`--cache_mode=false`) — which IS the
    // noeviction behaviour queue/lock libraries need — so omitting it is the
    // fix, not a gap.
    //
    // `--maxmemory` (2.16d) IS a valid Dragonfly server flag (distinct from the
    // rejected `--maxmemory-policy`): it caps the process RSS BELOW the
    // Guaranteed cgroup memory limit (`res.memory`), giving headroom so
    // Dragonfly rejects writes at its own ceiling rather than being OOM-killed.
    let mut spec = json!({
        "replicas": replicas,
        // ADR 0042 §10: pin the SERVER image. The platform pinned none — the
        // CR carried no `image`, the chart default is empty — so the tag came
        // from the dragonfly-operator's compiled-in default. Every ACL fact
        // this platform relies on (the file grammar, the `$N` selector, and
        // the `default`-synthesis behaviour that makes the default line
        // mandatory) is a fact about v1.37.0, and an operator-chart bump
        // would have changed it silently.
        "image": DRAGONFLY_SERVER_IMAGE,
        "args": [
            format!("--dbnum={dbnum}"),
            format!("--num_shards={num_shards}"),
            format!("--maxmemory={DRAGONFLY_MAXMEMORY}"),
        ],
        // Guaranteed QoS: requests == limits on cpu + memory. The
        // dragonfly-operator propagates `spec.resources` (standard k8s
        // ResourceRequirements) onto the StatefulSet pod. `shared_buffers` on
        // `res` is a Postgres-ism Dragonfly ignores and is not emitted here.
        "resources": {
            "requests": {
                "cpu": res.cpu,
                "memory": res.memory,
            },
            "limits": {
                "cpu": res.cpu,
                "memory": res.memory,
            },
        },
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
        "metadata": {
            "name": name,
            "namespace": ns,
            // Ownership stamp, spelled exactly as `admin_secret_object`
            // spells it. LOAD-BEARING: the reaper (ADR 0042 §9) LISTs its
            // candidates under this selector, so an instance this operator
            // did not create can never become one — whatever it is called.
            //
            // Without it, parsing the NAME is the only gate on a delete, and
            // `platform-redis-<class>-<index>` is not a reserved namespace of
            // names: a user's own `Dragonfly` called
            // `platform-redis-ephemeral-007` in this namespace would be
            // reaped, and logged as a tenantless pool instance while it went.
            "labels": {
                "apprafter.io/managed-by": "apprafter",
            },
        },
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

/// The key the ACL file is projected from. The dragonfly-operator renames it
/// to `dragonfly.acl` on mount, so this name is ours alone (ADR 0042 §10).
pub const ACL_SECRET_KEY: &str = "acl";

/// `<instance>-acl` — the Secret holding one ACL file for a pool instance.
pub fn acl_secret_name(instance: &str) -> String {
    format!("{instance}-acl")
}

/// Why a file line cannot drift from the live grant, and why building the
/// file can fail (ADR 0042 §10).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AclFileError {
    /// A token carries whitespace, `\r`, or a control byte. The file grammar
    /// splits on spaces and newlines, so such a token silently becomes two
    /// tokens or a broken line — and one broken line rejects the WHOLE file.
    UnrepresentableToken { line: usize, token: String },
    /// Fewer than four tokens. `MaterializeFileContents` requires
    /// `USER <name> <rule> <rule>` at minimum and rejects the file otherwise.
    TooFewTokens { line: usize, count: usize },
    /// The admin password is empty. Emitting `USER default on >` would give
    /// the shared instance a default user with no credential.
    EmptyAdminPassword,
}

impl std::fmt::Display for AclFileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnrepresentableToken { line, token } => write!(
                f,
                "ACL file line {line} carries a token the file grammar cannot represent: {token:?}"
            ),
            Self::TooFewTokens { line, count } => {
                write!(
                    f,
                    "ACL file line {line} has {count} tokens, needs at least 4"
                )
            }
            Self::EmptyAdminPassword => {
                write!(
                    f,
                    "refusing to build an ACL file with an empty default password"
                )
            }
        }
    }
}

/// The file form of an `ACL SETUSER` argv.
///
/// The ONLY producer of a file line, and it takes the same argv the live
/// grant uses — so divergence between what a tenant is granted at runtime and
/// what survives a restart is unrepresentable rather than merely tested.
pub fn acl_file_line(args: &[String]) -> String {
    format!("USER {}", args.join(" "))
}

/// The `default` line: `USER default on >{pw} ~* &* +@all`.
///
/// Every token is load-bearing.
///
///  * `on` — `User::is_active_` defaults to FALSE, and the synthesised
///    default's `is_active = true` applies only when the file omits `default`
///    entirely. A default line without `on` is a user nobody can authenticate
///    as, which is the lockout the working note feared.
///  * `+@all ~* &*` — the file path pre-applies `-@all` to every user, so the
///    admin's grants must be stated rather than inherited.
///  * no `$N` — `User::db_` defaults to "all databases", which is what the
///    provisioner needs.
///
/// The line itself is mandatory, and that is a security gate rather than a
/// nicety: with `--aclfile` loaded, the registry initialiser that consumes the
/// operator-injected admin password never runs, so a file OMITTING `default`
/// yields an active `nopass +@all ~* &*` user and turns authentication off on
/// an instance serving every tenant in the cluster. Verified live.
pub fn admin_acl_args(admin_pw: &str) -> Vec<String> {
    vec![
        "default".into(),
        "on".into(),
        format!(">{admin_pw}"),
        "~*".into(),
        "&*".into(),
        "+@all".into(),
    ]
}

/// Build the whole ACL file: the `default` line, then one line per tenant,
/// sorted by username.
///
/// Sorted because the loop skips the write when the derived content equals the
/// live content, and an unsorted derivation from a LIST would churn the Secret
/// on every pass for no change.
///
/// **Refuses rather than emits a damaged file.** The caller skips the write and
/// leaves the previous file in place, which is strictly better than replacing a
/// working file with one the server will reject — a rejected file loads zero
/// users, and the instance falls back to a default the tenants cannot use.
pub fn acl_file_contents(admin_pw: &str, tenants: &[Vec<String>]) -> Result<String, AclFileError> {
    if admin_pw.is_empty() {
        return Err(AclFileError::EmptyAdminPassword);
    }
    let mut lines: Vec<Vec<String>> = vec![admin_acl_args(admin_pw)];
    let mut sorted: Vec<Vec<String>> = tenants.to_vec();
    sorted.sort_by(|a, b| a.first().cmp(&b.first()));
    lines.extend(sorted);

    for (i, args) in lines.iter().enumerate() {
        // +1 for the `USER` literal the file form prepends.
        let count = args.len() + 1;
        if count < 4 {
            return Err(AclFileError::TooFewTokens { line: i + 1, count });
        }
        for token in args {
            if token.is_empty()
                || token
                    .chars()
                    .any(|c| c.is_ascii_whitespace() || c.is_control())
            {
                return Err(AclFileError::UnrepresentableToken {
                    line: i + 1,
                    token: token.clone(),
                });
            }
        }
    }

    let mut out: String = lines
        .iter()
        .map(|a| acl_file_line(a))
        .collect::<Vec<_>>()
        .join("\n");
    out.push('\n');
    Ok(out)
}

/// The `<instance>-acl` Secret object. Labelled like the CR and the admin
/// Secret so the reaper's inventory selector finds it.
pub fn acl_secret_object(name: &str, ns: &str, contents: &str) -> Value {
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
            ACL_SECRET_KEY: contents,
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
    use crate::cnpg::BackendResources;
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

    // --- class_of_instance() (ADR 0042 §9 reaper — inverse of the above) ---

    #[test]
    fn class_of_instance_round_trips_pool_instance_name() {
        // The reaper deletes BY NAME, so this round-trip is what makes the
        // naming convention safe to depend on.
        for persistent in [false, true] {
            for index in [0u32, 7, 999] {
                let name = pool_instance_name(persistent, index);
                let want = if persistent {
                    PoolClass::Persistent
                } else {
                    PoolClass::Ephemeral
                };
                assert_eq!(
                    class_of_instance(&name),
                    Some(want),
                    "round-trip failed for {name}"
                );
            }
        }
    }

    #[test]
    fn class_of_instance_rejects_foreign_dragonflies() {
        for name in [
            "my-cache",
            "platform-redis-weird-000",
            "platform-redis-ephemeral",
            "platform-redis-ephemeral-",
            "platform-redis-ephemeral-abc",
            "platform-redis-ephemeral-000-extra",
            "",
        ] {
            assert_eq!(class_of_instance(name), None, "must reject {name:?}");
        }
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
            1,
            true,
            &BackendResources::dragonfly_t1(),
        );
        assert_eq!(cr["apiVersion"], "dragonflydb.io/v1alpha1");
        assert_eq!(cr["kind"], "Dragonfly");
        assert_eq!(cr["metadata"]["name"], "platform-redis-persistent-000");
        assert_eq!(cr["metadata"]["namespace"], "dragonfly-system");
        // replicas MUST be present and >= 1 — the operator does not default it,
        // so omitting it yields a 0-replica StatefulSet (no pod).
        assert_eq!(cr["spec"]["replicas"], 1);
        let args = cr["spec"]["args"].as_array().unwrap();
        assert!(args.iter().any(|a| a == "--dbnum=1024"));
        assert!(args.iter().any(|a| a == "--num_shards=1"));
        // `--maxmemory` (the RSS cap, 2.16d) IS emitted and valid.
        assert!(args.iter().any(|a| a == "--maxmemory=256mb"));
        // NO --maxmemory-policy / --maxmemory_policy: Dragonfly rejects those
        // Redis-isms (crash-loop); noeviction is its default. Guard against a
        // regression re-adding either policy flag (but allow `--maxmemory=…`).
        assert!(!args
            .iter()
            .any(|a| a.as_str().unwrap_or("").contains("maxmemory-policy")
                || a.as_str().unwrap_or("").contains("maxmemory_policy")));
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
            1,
            false,
            &BackendResources::dragonfly_t1(),
        );
        assert!(eph["spec"].get("snapshot").is_none());
    }

    #[test]
    fn dragonfly_cr_is_stamped_with_the_ownership_label() {
        // SAFETY, not inventory: the reaper (ADR 0042 §9) selects its delete
        // candidates on this label, so an unstamped CR is one whose only
        // protection from deletion is that its name failed to parse. The
        // stamp must match `admin_secret_object`'s byte for byte — a
        // divergence here silently empties the reaper's candidate set (it
        // would then reap nothing, which fails safe but never shrinks the
        // pool) or, if the selector were relaxed to match, widens it.
        for persistent in [false, true] {
            let cr = dragonfly_object(
                &pool_instance_name(persistent, 0),
                "dragonfly-system",
                1024,
                1,
                1,
                persistent,
                &BackendResources::dragonfly_t1(),
            );
            assert_eq!(
                cr["metadata"]["labels"]["apprafter.io/managed-by"], "apprafter",
                "pool instance CR must carry the ownership stamp"
            );
        }
        // Same key and value as the admin Secret's stamp.
        let secret = admin_secret_object("x-admin", "dragonfly-system", "pw");
        let cr = dragonfly_object(
            "platform-redis-ephemeral-000",
            "dragonfly-system",
            1024,
            1,
            1,
            false,
            &BackendResources::dragonfly_t1(),
        );
        assert_eq!(
            cr["metadata"]["labels"]["apprafter.io/managed-by"],
            secret["metadata"]["labels"]["apprafter.io/managed-by"]
        );
    }

    #[test]
    fn dragonfly_object_emits_guaranteed_resources() {
        let res = BackendResources::dragonfly_t1();
        let v = dragonfly_object("r", "ns", 0, 1, 1, false, &res);
        // requests == limits on cpu + memory → Guaranteed QoS.
        assert_eq!(
            v["spec"]["resources"]["requests"]["memory"],
            v["spec"]["resources"]["limits"]["memory"]
        );
        assert_eq!(
            v["spec"]["resources"]["requests"]["cpu"],
            v["spec"]["resources"]["limits"]["cpu"]
        );
        assert_eq!(v["spec"]["resources"]["limits"]["memory"], "320Mi");
        assert_eq!(v["spec"]["resources"]["limits"]["cpu"], "50m");
        // Dragonfly caps its own RSS via the `--maxmemory` server flag, sized
        // BELOW the 320Mi cgroup limit (RSS headroom). This is the real
        // Dragonfly memory-limit flag — distinct from the rejected
        // `--maxmemory-policy` Redis-ism.
        let args = v["spec"]["args"].as_array().unwrap();
        assert!(
            args.iter().any(|a| a == "--maxmemory=256mb"),
            "expected --maxmemory=256mb in args, got {args:?}"
        );
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
                name: None,
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

    // --- ACL file builder (ADR 0042 §10) ---

    fn hex_pw() -> String {
        "s3cr3t".to_string()
    }

    #[test]
    fn a_file_line_is_the_runtime_argv_with_one_literal_in_front() {
        // Divergence between the live grant and the persisted grant is
        // unrepresentable BY CONSTRUCTION: there is one producer, and it takes
        // the same argv `ACL SETUSER` is given.
        let args = acl_setuser_args("claim_demo_web_redis", &hex_pw(), 7);
        let line = acl_file_line(&args);
        assert_eq!(line, format!("USER {}", args.join(" ")));
        assert!(line.starts_with("USER claim_demo_web_redis on >"));
    }

    #[test]
    fn the_default_line_carries_on_and_states_its_grants() {
        let args = admin_acl_args(&hex_pw());
        assert_eq!(args[0], "default");
        // `on` — is_active_ defaults FALSE, and the synthesised default's
        // is_active applies only when the file omits `default` entirely. A
        // default line without `on` is a user nobody can authenticate as.
        assert!(args.iter().any(|a| a == "on"), "{args:?}");
        // The file path pre-applies `-@all` to every user, so the admin's
        // grants must be stated rather than inherited.
        assert!(args.iter().any(|a| a == "+@all"), "{args:?}");
        assert!(args.iter().any(|a| a == "~*"), "{args:?}");
        assert!(args.iter().any(|a| a == "&*"), "{args:?}");
        // No `$N`: db_ defaults to all databases, which is what the
        // provisioner needs.
        assert!(!args.iter().any(|a| a.starts_with('$')), "{args:?}");
    }

    #[test]
    fn the_file_always_opens_with_a_default_line() {
        // THE security gate. With `--aclfile` loaded, the registry
        // initialiser that consumes the operator-injected admin password
        // never runs — so a file omitting `default` yields an ACTIVE
        // `nopass +@all ~* &*` user and turns authentication OFF on an
        // instance serving every tenant in the cluster. Verified live.
        let f = acl_file_contents(&hex_pw(), &[]).expect("a tenantless file is still a file");
        assert!(f.starts_with("USER default on >s3cr3t "), "{f}");
        assert!(
            f.ends_with('\n'),
            "must end with exactly one newline: {f:?}"
        );
    }

    #[test]
    fn an_empty_admin_password_is_refused() {
        // `USER default on >` would be a default user with no credential.
        assert_eq!(
            acl_file_contents("", &[]),
            Err(AclFileError::EmptyAdminPassword)
        );
    }

    #[test]
    fn a_token_carrying_whitespace_is_refused_rather_than_written() {
        // The grammar splits on spaces, so a space inside a password turns
        // one line into a malformed one — and ONE malformed line rejects the
        // WHOLE file, taking every tenant's credential with it. Refusing
        // leaves the previous, working file in place.
        let bad = acl_setuser_args("claim_a", "pass word", 1);
        assert!(matches!(
            acl_file_contents(&hex_pw(), &[bad]),
            Err(AclFileError::UnrepresentableToken { .. })
        ));
        let cr = acl_setuser_args("claim_a", "pass\rword", 1);
        assert!(matches!(
            acl_file_contents(&hex_pw(), &[cr]),
            Err(AclFileError::UnrepresentableToken { .. })
        ));
        let nl = acl_setuser_args("claim_a", "pass\nword", 1);
        assert!(matches!(
            acl_file_contents(&hex_pw(), &[nl]),
            Err(AclFileError::UnrepresentableToken { .. })
        ));
    }

    #[test]
    fn a_line_with_too_few_tokens_is_refused() {
        // `MaterializeFileContents` requires `USER <name> <rule> <rule>`.
        assert!(matches!(
            acl_file_contents(&hex_pw(), &[vec!["claim_a".into(), "on".into()]]),
            Err(AclFileError::TooFewTokens { .. })
        ));
    }

    #[test]
    fn tenant_lines_are_sorted_so_a_re_derivation_compares_equal() {
        // The loop skips the write when the derived file equals the live one.
        // An unsorted derivation from a LIST would churn the Secret on every
        // pass, destroying the one cheap signal for "when did this instance's
        // ACL set last change".
        let a = acl_setuser_args("claim_aaa", "p1", 1);
        let b = acl_setuser_args("claim_bbb", "p2", 2);
        let one = acl_file_contents(&hex_pw(), &[a.clone(), b.clone()]).unwrap();
        let two = acl_file_contents(&hex_pw(), &[b, a]).unwrap();
        assert_eq!(one, two);
        let lines: Vec<&str> = one.trim_end().split('\n').collect();
        assert_eq!(lines.len(), 3);
        assert!(lines[1].starts_with("USER claim_aaa "), "{one}");
        assert!(lines[2].starts_with("USER claim_bbb "), "{one}");
    }

    #[test]
    fn a_built_file_carries_no_revoked_grant() {
        // The file is derived whole, so a tenant absent from the input is
        // absent from the file — which is what makes revocation durable
        // rather than merely runtime.
        let a = acl_setuser_args("claim_kept", "p1", 1);
        let f = acl_file_contents(&hex_pw(), &[a]).unwrap();
        assert!(f.contains("USER claim_kept "));
        assert!(!f.contains("claim_revoked"));
    }

    #[test]
    fn every_built_line_satisfies_the_file_grammar() {
        // Belt and braces against the whole-file rejection: four tokens
        // minimum, `USER` first, no empty lines.
        let t1 = acl_setuser_args("claim_a", "p1", 1);
        let t2 = acl_setuser_args("claim_b", "p2", 2);
        let f = acl_file_contents(&hex_pw(), &[t1, t2]).unwrap();
        for line in f.trim_end().split('\n') {
            assert!(!line.is_empty(), "empty line in {f:?}");
            let toks: Vec<&str> = line.split(' ').filter(|s| !s.is_empty()).collect();
            assert_eq!(toks[0], "USER", "{line}");
            assert!(toks.len() >= 4, "{line}");
        }
    }

    #[test]
    fn the_acl_secret_is_labelled_for_the_reaper() {
        let s = acl_secret_object(
            "platform-redis-ephemeral-000-acl",
            "dragonfly-system",
            "USER default on >x ~* &* +@all\n",
        );
        assert_eq!(s["type"], "Opaque");
        assert_eq!(
            s["metadata"]["labels"]["apprafter.io/managed-by"], "apprafter",
            "the reaper LISTs its inventory under this selector"
        );
        assert!(s["stringData"][ACL_SECRET_KEY]
            .as_str()
            .unwrap()
            .starts_with("USER default "));
    }

    #[test]
    fn the_server_image_is_pinned_on_the_cr() {
        // Every ACL fact ADR 0042 §10 relies on is a fact about this tag.
        let cr = dragonfly_object(
            "platform-redis-ephemeral-000",
            "dragonfly-system",
            16,
            1,
            1,
            false,
            &BackendResources::dragonfly_t1(),
        );
        assert_eq!(cr["spec"]["image"], DRAGONFLY_SERVER_IMAGE);
        assert!(DRAGONFLY_SERVER_IMAGE.ends_with(":v1.37.0"));
    }

    // --- acl_setuser_args() ---

    #[test]
    fn acl_setuser_args_pin_db_and_keyspace() {
        let args = acl_setuser_args("claim_demo_web_redis", "s3cr3t", 7);
        // ACL SETUSER <user> on >pw $7 resetkeys ~* resetchannels
        //   &claim_demo_web_redis:* +@all -@admin -@dangerous -move -copy
        //   -pubsub +sort_ro ...
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
        // ADR 0042 §11: `+info` is REVOKED. `INFO KEYSPACE` enumerates every
        // non-empty database on the shared instance regardless of the
        // selected one, so it tells a tenant which other tenants hold data
        // and how much. There is no section- or DB-scoped form.
        assert!(
            !args.iter().any(|a| a == "+info"),
            "+info must not be granted — it enumerates every tenant's key counts"
        );
        // And `PUBSUB` is the same disclosure with names attached: channel
        // names carry the namespace and app, and `PUBSUB CHANNELS` output is
        // NOT filtered by the user's `&{user}:*` patterns.
        assert!(
            args.iter().any(|a| a == "-pubsub"),
            "PUBSUB CHANNELS returns every tenant's namespace and app name — must be denied"
        );
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
        // Dragonfly's ACL parser rejects the `command|subcommand` form, so no
        // `+client|setname`-style tokens may be emitted (regression guard).
        assert!(
            !args.iter().any(|a| a.contains('|')),
            "Dragonfly ACL rejects command|subcommand grants"
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
