// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! `SharedDatabase` reconcile loop (2.29 / ADR 0066).
//!
//! A `SharedDatabase` is a pg database or a redis `$N` that OUTLIVES every
//! application bound to it. Its consumers are ordinary `ResourceClaim`s
//! carrying `spec.sharedRef`; this controller owns the backing resource and
//! the two PostgreSQL groups, and the claim provisioner owns each consumer's
//! own role, password and Secret.
//!
//! The split matters for deletes: a consumer's GC drops that consumer's role
//! and Secret and touches nothing else, which is the property the whole CRD
//! exists to provide. Only this controller can drop the database, and only at
//! `refCount == 0`.
//!
//! ## `refCount` is DERIVED, and that is the design (ADR 0066 §6)
//!
//! It is recomputed from the live claims on every reconcile — never
//! incremented and decremented. An incremented counter drifts, and this one
//! gates a destructive `db rm`: a stale `0` deletes a database two
//! applications are using. The cost is a LIST per reconcile, which is the
//! right trade for a number with that consequence.
//!
//! Because the count comes from claims, a claim CREATE or DELETE must
//! re-trigger the parent — see [`shared_database_refs_in_store`] for the
//! watch-mapper, and for why an orphan reference must be dropped rather than
//! emitted (the same requeue storm `SharedVolume` hit as its walk-found Bug
//! C).
//!
//! ## Where the reference is read from, and why NOT a label
//!
//! `SharedVolume` fans out on the `apprafter.io/shared-volume` LABEL its
//! Application controller stamps. This controller reads `spec.sharedRef`
//! directly instead. `ResourceClaim` is a typed resource here, the field is
//! part of its schema, and a label would be a second copy of it that can
//! disagree with the first — with the disagreement resolving, silently, in
//! favour of whichever side the reader happened to consult. `refCount` gates
//! a destructive operation; it reads the field the provisioner acts on.
//!
//! ## SSA field-manager split (CRITICAL)
//!
//! Like the ResourceClaim provisioner and the SharedVolume controller, this
//! one writes ONLY the `SharedDatabase` `.status` under
//! [`crate::FIELD_MANAGER`] and never touches `.spec`. SSA REPLACES a
//! manager's owned field-set on each apply, so the terminal status body
//! carries ALL status fields this controller owns — a body that omits one
//! PRUNES it.

use chrono::Utc;
use serde_json::{json, Value};

use kube::runtime::reflector::{ObjectRef, Store};
use operator_core::{
    ResourceClaim, SharedDatabase, SharedDatabaseCondition, COND_EXTENSION_UNAVAILABLE, COND_READY,
};

use crate::cnpg::{k8s_name, pg_identifier};
use crate::shared_pg::shared_group;

// ---------------------------------------------------------------------------
// Naming
// ---------------------------------------------------------------------------

/// The PostgreSQL database name for a `SharedDatabase`.
///
/// Identical to [`shared_group`], deliberately: the database is OWNED by the
/// like-named group, and Postgres keeps databases and roles in separate
/// namespaces so the two cannot collide. Every statement that names either
/// quotes it, so there is no context in which the shared spelling is
/// ambiguous. Proven in this exact shape on a live server by
/// `e2e/shared-pg-sql-check.sh`, which creates `shd_apps_orders` owned by
/// `shd_apps_orders`.
pub fn shd_database_name(ns: &str, name: &str) -> String {
    shared_group(ns, name)
}

/// The DNS-1123 Kubernetes object name for a `SharedDatabase`'s CNPG
/// `Database` CR.
///
/// `shd-` rather than [`k8s_name`]'s `claim-`: a shared database's CR must not
/// be able to collide with a claim's, and the prefix is what keeps the two
/// name spaces disjoint even when a namespace and name happen to fold the same
/// way.
pub fn shd_k8s_name(ns: &str, name: &str) -> String {
    k8s_name(ns, name).replacen("claim-", "shd-", 1)
}

/// The deterministic consumer ROLE for a claim binding a shared database.
///
/// This is [`pg_identifier`] of the CLAIM's own coordinates, unchanged from
/// the owned-pg path: a consumer role belongs to the consumer, not to the
/// database it binds, so the same application keeps one identity whether its
/// database is owned or shared. It also means a `\du` on the server reads the
/// same way for both.
pub fn consumer_role(claim_ns: &str, claim_name: &str) -> String {
    pg_identifier(claim_ns, claim_name)
}

// ---------------------------------------------------------------------------
// Watch fan-out + refCount (pure; unit-tested without a cluster)
// ---------------------------------------------------------------------------

/// Map a consumer `ResourceClaim` to the `SharedDatabase` it binds, for the
/// SharedDatabase controller's `.watches()` fan-out.
///
/// Returns `None` for a claim with no `spec.sharedRef` (not a consumer) or no
/// namespace. The reference is namespace-local by construction — ADR 0066
/// scopes sharing to one namespace, and the webhook gives a `/` in the field
/// its own message rather than resolving it.
pub fn shared_database_ref_for_claim(claim: &ResourceClaim) -> Option<ObjectRef<SharedDatabase>> {
    let ns = claim.metadata.namespace.as_deref()?;
    let name = claim.spec.shared_ref.as_deref()?;
    Some(ObjectRef::<SharedDatabase>::new(name).within(ns))
}

/// The watch-mapper fan-out for a claim, FILTERED against the SharedDatabase
/// reflector `store`: only emit the referenced object if it actually exists.
///
/// An ORPHAN consumer claim — `sharedRef` naming a database that was deleted
/// or never existed — would otherwise map to a ref the Controller runtime
/// cannot resolve (`tried to reconcile object SharedDatabase/<gone> that was
/// not found in local store`), erroring and requeueing forever. That exact
/// storm is `SharedVolume`'s walk-found Bug C; it is cheaper to not repeat it
/// than to find it again.
///
/// A database that DOES exist self-reconciles on create and on the periodic
/// requeue, so the rare not-yet-in-store window costs at worst a delayed
/// `refCount` refresh.
pub fn shared_database_refs_in_store(
    claim: &ResourceClaim,
    store: &Store<SharedDatabase>,
) -> Option<ObjectRef<SharedDatabase>> {
    shared_database_ref_for_claim(claim).filter(|r| store.get(r).is_some())
}

/// Count the live consumer claims bound to this shared database.
///
/// Reads `spec.sharedRef` out of raw claim JSON — the caller LISTs the
/// namespace once and passes the items, so this stays pure.
pub fn ref_count_for(sd_name: &str, claims: &[Value]) -> i64 {
    binders_of(sd_name, claims).len() as i64
}

/// The NAMES of the claims bound to this shared database, sorted.
///
/// `refCount` alone tells an operator that a `db rm` was refused; this tells
/// them which application to go and look at. Sorted so the refusal message is
/// stable across two runs — an unstable list reads as churn and invites the
/// reader to diff it.
pub fn binders_of(sd_name: &str, claims: &[Value]) -> Vec<String> {
    let mut out: Vec<String> = claims
        .iter()
        .filter(|c| {
            c.pointer("/spec/sharedRef").and_then(Value::as_str) == Some(sd_name)
                // A claim under deletion is already released: its consumer
                // role and Secret are being dropped, and counting it would
                // hold the database hostage to a GC that has already started.
                && c.pointer("/metadata/deletionTimestamp").is_none()
        })
        .filter_map(|c| {
            c.pointer("/metadata/name")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .collect();
    out.sort();
    out
}

// ---------------------------------------------------------------------------
// Status + conditions (pure)
// ---------------------------------------------------------------------------

/// What this reconcile provisioned, as the status reports it. Grouped rather
/// than passed as four adjacent `Option`s: `database` belongs to pg and
/// `(instance, dbnum)` to redis, and a flat parameter list invites a call that
/// supplies one of the redis pair and forgets the other.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Backing {
    /// pg: the database name in the shared cluster.
    pub database: Option<String>,
    /// redis: the pool instance.
    pub instance: Option<String>,
    /// redis: the `$N` every consumer of this database is pinned to.
    pub dbnum: Option<i64>,
}

impl Backing {
    pub fn pg(database: impl Into<String>) -> Self {
        Self {
            database: Some(database.into()),
            ..Default::default()
        }
    }

    pub fn redis(instance: impl Into<String>, dbnum: i64) -> Self {
        Self {
            instance: Some(instance.into()),
            dbnum: Some(dbnum),
            ..Default::default()
        }
    }
}

/// Pure SSA status body for a `SharedDatabase` (field manager = provisioner).
///
/// Always carries `ready` + `refCount`; the backing fields appear only for the
/// type that has them. NO `connectionSecretRef` — see the type's own doc: each
/// consumer holds its own credential, and a shared one here would be the thing
/// the CRD exists to avoid.
pub fn sd_status_apply_body(
    sd_name: &str,
    ready: bool,
    backing: &Backing,
    ref_count: i64,
) -> Value {
    let mut status = json!({ "ready": ready, "refCount": ref_count });
    if let Some(db) = backing.database.as_deref() {
        status["database"] = json!(db);
    }
    if let Some(inst) = backing.instance.as_deref() {
        status["instance"] = json!(inst);
    }
    if let Some(n) = backing.dbnum {
        status["dbnum"] = json!(n);
    }
    json!({
        "apiVersion": "apprafter.io/v1alpha1",
        "kind": "SharedDatabase",
        "metadata": { "name": sd_name },
        "status": status
    })
}

/// As [`sd_status_apply_body`] but stamps the terminal `conditions` array.
///
/// SSA REPLACES the manager's owned field-set on each apply, so BOTH
/// conditions this controller owns must ride the one body: a body carrying
/// only `Ready` PRUNES an `ExtensionUnavailable` set on a prior apply, and an
/// operator watching for the extension warning would see it blink out on the
/// next cycle with nothing having changed.
pub fn sd_status_apply_body_with_conditions(
    sd_name: &str,
    ready: bool,
    backing: &Backing,
    ref_count: i64,
    ready_cond: SharedDatabaseCondition,
    extension_cond: Option<SharedDatabaseCondition>,
) -> Value {
    let mut body = sd_status_apply_body(sd_name, ready, backing, ref_count);
    body["status"]["conditions"] = match extension_cond {
        Some(ext) => json!([ready_cond, ext]),
        None => json!([ready_cond]),
    };
    body
}

/// Build a condition of `type_`, preserving `lastTransitionTime` when the
/// `(type, status)` pair is unchanged.
///
/// Preserving it is not cosmetic: a fresh timestamp on every reconcile is a
/// status WRITE on every reconcile, which re-triggers the watch and spins the
/// controller at the requeue interval forever. The same guard SharedVolume
/// carries, for the same reason.
pub fn condition(
    type_: &str,
    status: &str,
    reason: &str,
    message: &str,
    previous: &[SharedDatabaseCondition],
) -> SharedDatabaseCondition {
    let last_transition_time = previous
        .iter()
        .find(|c| c.type_ == type_ && c.status == status)
        .map(|c| c.last_transition_time.clone())
        .unwrap_or_else(|| Utc::now().to_rfc3339());
    SharedDatabaseCondition {
        type_: type_.to_string(),
        status: status.to_string(),
        last_transition_time,
        reason: Some(reason.to_string()),
        message: Some(message.to_string()),
    }
}

/// The `Ready` condition.
pub fn ready_condition(
    status: &str,
    reason: &str,
    message: &str,
    previous: &[SharedDatabaseCondition],
) -> SharedDatabaseCondition {
    condition(COND_READY, status, reason, message, previous)
}

/// The `ExtensionUnavailable` condition, raised when the running operand image
/// does not provide an extension the spec declares (ADR 0066 §4.2).
///
/// `Some` only when something is missing: an absent condition is how "every
/// declared extension is available" is said, and a `False` condition would
/// make an operator scanning for the type find one and have to read its status
/// to learn it is the good case.
pub fn extension_unavailable_condition(
    missing: &[String],
    previous: &[SharedDatabaseCondition],
) -> Option<SharedDatabaseCondition> {
    if missing.is_empty() {
        return None;
    }
    Some(condition(
        COND_EXTENSION_UNAVAILABLE,
        "True",
        "NotInOperandImage",
        &format!(
            "the running PostgreSQL image does not provide: {}",
            missing.join(", ")
        ),
        previous,
    ))
}

/// The extensions a spec declares that the server does not offer.
///
/// Compared case-insensitively because PostgreSQL folds unquoted identifiers
/// to lower case and `pg_available_extensions` reports the folded name, while
/// a manifest may well spell `pgVector`. The allow list already bounds what
/// can be asked for; this decides only whether the ASKED-FOR thing is present.
pub fn missing_extensions(
    declared: &[operator_core::PgExtension],
    available: &[String],
) -> Vec<String> {
    let have: Vec<String> = available.iter().map(|a| a.to_ascii_lowercase()).collect();
    declared
        .iter()
        .filter(|e| !have.contains(&e.name.to_ascii_lowercase()))
        .map(|e| e.name.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use operator_core::PgExtension;

    fn claim_json(name: &str, shared_ref: Option<&str>) -> Value {
        let mut c = json!({
            "metadata": { "name": name, "namespace": "apps" },
            "spec": { "type": "pg" }
        });
        if let Some(r) = shared_ref {
            c["spec"]["sharedRef"] = json!(r);
        }
        c
    }

    // --- naming ---

    #[test]
    fn the_database_and_its_owning_group_share_a_name() {
        // Asserted rather than left implicit: `e2e/shared-pg-sql-check.sh`
        // proved the role model against `shd_apps_orders` owned by
        // `shd_apps_orders`, and a rename on either side alone would leave the
        // live proof testing something the code no longer does.
        assert_eq!(shd_database_name("apps", "orders"), "shd_apps_orders");
        assert_eq!(
            shd_database_name("apps", "orders"),
            shared_group("apps", "orders")
        );
    }

    #[test]
    fn the_k8s_object_name_cannot_collide_with_a_claims() {
        assert_eq!(shd_k8s_name("apps", "orders"), "shd-apps-orders");
        assert_eq!(k8s_name("apps", "orders"), "claim-apps-orders");
        assert_ne!(shd_k8s_name("apps", "orders"), k8s_name("apps", "orders"));
    }

    #[test]
    fn a_consumer_role_is_the_claims_own_identity_not_the_databases() {
        // The same application binding an owned pg database and a shared one
        // is the same role either way — so a `\du` reads the same, and a
        // migration from one to the other does not rename the login.
        assert_eq!(consumer_role("apps", "web-pg"), "claim_apps_web_pg");
    }

    // --- refCount / binders ---

    #[test]
    fn ref_count_counts_only_claims_naming_this_database() {
        let claims = vec![
            claim_json("web-pg", Some("orders")),
            claim_json("api-pg", Some("orders")),
            claim_json("rep-pg", Some("reporting")),
            claim_json("own-pg", None),
        ];
        assert_eq!(ref_count_for("orders", &claims), 2);
        assert_eq!(ref_count_for("reporting", &claims), 1);
        assert_eq!(ref_count_for("absent", &claims), 0);
    }

    #[test]
    fn binders_are_named_and_sorted() {
        let claims = vec![
            claim_json("web-pg", Some("orders")),
            claim_json("api-pg", Some("orders")),
        ];
        assert_eq!(binders_of("orders", &claims), vec!["api-pg", "web-pg"]);
    }

    #[test]
    fn a_claim_under_deletion_no_longer_holds_the_database() {
        // Its consumer role and Secret are already being dropped. Counting it
        // would refuse a `db rm` on the strength of a binding that is going
        // away by itself, and the refusal would clear on its own some seconds
        // later — which reads as a flaky command, not as a guard.
        let mut dying = claim_json("web-pg", Some("orders"));
        dying["metadata"]["deletionTimestamp"] = json!("2026-09-15T10:00:00Z");
        let claims = vec![dying, claim_json("api-pg", Some("orders"))];
        assert_eq!(ref_count_for("orders", &claims), 1);
        assert_eq!(binders_of("orders", &claims), vec!["api-pg"]);
    }

    // --- status body ---

    #[test]
    fn a_pg_status_body_carries_the_database_and_no_redis_fields() {
        let body = sd_status_apply_body("orders", true, &Backing::pg("shd_apps_orders"), 2);
        assert_eq!(body["status"]["ready"], json!(true));
        assert_eq!(body["status"]["refCount"], json!(2));
        assert_eq!(body["status"]["database"], json!("shd_apps_orders"));
        assert!(body["status"].get("instance").is_none());
        assert!(body["status"].get("dbnum").is_none());
        // The absence is the design (ADR 0066 §1) — asserted here too so the
        // status WRITER and the status TYPE each carry the rule.
        assert!(body["status"].get("connectionSecretRef").is_none());
    }

    #[test]
    fn a_redis_status_body_carries_the_instance_and_its_pin() {
        let body = sd_status_apply_body(
            "cache",
            true,
            &Backing::redis("platform-redis-ephemeral-000", 4),
            1,
        );
        assert_eq!(
            body["status"]["instance"],
            json!("platform-redis-ephemeral-000")
        );
        assert_eq!(body["status"]["dbnum"], json!(4));
        assert!(body["status"].get("database").is_none());
    }

    #[test]
    fn dbnum_zero_is_reported_rather_than_dropped() {
        // `$0` is an allocatable DB (the allocator hands it out first), so a
        // falsy-value shortcut here would leave the FIRST shared cache on a
        // cluster with no pin in its status — and then the allocator, reading
        // that status back, would hand `0` out a second time.
        let body = sd_status_apply_body(
            "cache",
            true,
            &Backing::redis("platform-redis-ephemeral-000", 0),
            0,
        );
        assert_eq!(body["status"]["dbnum"], json!(0));
    }

    // --- conditions ---

    #[test]
    fn an_unchanged_condition_keeps_its_transition_time() {
        let previous = vec![SharedDatabaseCondition {
            type_: COND_READY.into(),
            status: "True".into(),
            last_transition_time: "2026-01-01T00:00:00+00:00".into(),
            reason: Some("Provisioned".into()),
            message: Some("ready".into()),
        }];
        let c = ready_condition("True", "Provisioned", "ready", &previous);
        assert_eq!(c.last_transition_time, "2026-01-01T00:00:00+00:00");
    }

    #[test]
    fn a_flipped_condition_takes_a_fresh_transition_time() {
        let previous = vec![SharedDatabaseCondition {
            type_: COND_READY.into(),
            status: "False".into(),
            last_transition_time: "2026-01-01T00:00:00+00:00".into(),
            reason: Some("AwaitingCluster".into()),
            message: Some("waiting".into()),
        }];
        let c = ready_condition("True", "Provisioned", "ready", &previous);
        assert_ne!(c.last_transition_time, "2026-01-01T00:00:00+00:00");
    }

    #[test]
    fn both_owned_conditions_ride_one_body() {
        // SSA prunes what a body omits. A `Ready`-only apply would silently
        // delete the extension warning the previous apply set.
        let ext = extension_unavailable_condition(&["vector".into()], &[]).expect("some");
        let body = sd_status_apply_body_with_conditions(
            "orders",
            false,
            &Backing::pg("shd_apps_orders"),
            0,
            ready_condition("False", "ExtensionUnavailable", "missing", &[]),
            Some(ext),
        );
        let conds = body["status"]["conditions"].as_array().expect("array");
        assert_eq!(conds.len(), 2);
        assert_eq!(conds[0]["type"], json!(COND_READY));
        assert_eq!(conds[1]["type"], json!(COND_EXTENSION_UNAVAILABLE));
    }

    #[test]
    fn no_extension_condition_when_nothing_is_missing() {
        assert!(extension_unavailable_condition(&[], &[]).is_none());
    }

    // --- extension availability ---

    #[test]
    fn a_declared_extension_the_image_lacks_is_reported() {
        let declared = vec![
            PgExtension {
                name: "vector".into(),
                ..Default::default()
            },
            PgExtension {
                name: "pg_trgm".into(),
                ..Default::default()
            },
        ];
        let available = vec!["pg_trgm".to_string(), "pgcrypto".to_string()];
        assert_eq!(missing_extensions(&declared, &available), vec!["vector"]);
    }

    #[test]
    fn availability_is_compared_case_insensitively() {
        // Postgres folds unquoted identifiers to lower case and
        // `pg_available_extensions` reports the folded name; a manifest may
        // spell it otherwise. Reporting `pgVector` missing while the server
        // offers `pgvector` would be a false alarm nobody could act on.
        let declared = vec![PgExtension {
            name: "pgVector".into(),
            ..Default::default()
        }];
        assert!(missing_extensions(&declared, &["pgvector".to_string()]).is_empty());
    }

    #[test]
    fn the_reported_name_is_the_one_the_manifest_used() {
        // Not the folded one: the operator has to find this string in their
        // own file.
        let declared = vec![PgExtension {
            name: "pgVector".into(),
            ..Default::default()
        }];
        assert_eq!(
            missing_extensions(&declared, &["pg_trgm".to_string()]),
            vec!["pgVector"]
        );
    }
}
