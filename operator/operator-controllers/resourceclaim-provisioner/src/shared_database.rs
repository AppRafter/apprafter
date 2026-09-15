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

use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use kube::api::{Api, ApiResource, DynamicObject, Patch, PatchParams};
use kube::core::GroupVersionKind;
use kube::runtime::controller::Action;
use kube::runtime::reflector::{ObjectRef, Store};
use kube::{Client, ResourceExt};
use serde_json::{json, Value};
use tracing::{info, warn};

use operator_core::matching::{select_provider, Candidate};
use operator_core::{
    PgExtension, ResourceClaim, ServiceProvider, SharedDatabase, SharedDatabaseCondition,
    COND_EXTENSION_UNAVAILABLE, COND_READY,
};

use crate::cnpg::{self, k8s_name, pg_identifier};
use crate::shared_pg::{self, shared_group};
use crate::{Context, ReconcileError, FIELD_MANAGER};

/// Metric label + log identity for this controller.
const KIND: &str = "SharedDatabase";

/// Held so a delete is OBSERVED rather than racing the apiserver's own
/// cascade. Without it a `SharedDatabase` vanishes and the backing database
/// survives with nothing left pointing at it — an orphan holding real data
/// that no longer appears in any inventory.
const SD_FINALIZER: &str = "apprafter.io/shareddatabase-cleanup";

/// `Ready=False` reason while the shared Postgres cluster is not yet
/// answering. Distinct from a provisioning failure: it clears by itself.
const REASON_AWAITING_CLUSTER: &str = "AwaitingCluster";

/// `Ready=False` reason while CNPG has not yet reported the `Database` CR
/// reconciled.
const REASON_AWAITING_DATABASE: &str = "AwaitingDatabase";

/// `Ready=False` reason on a delete held open by live consumers.
const REASON_IN_USE: &str = "InUse";

/// Fallbacks when the matched `ServiceProvider` config omits them — the same
/// two the owned-pg arm reads, and deliberately the same spelling so a
/// provider seed that moves the cluster moves BOTH paths at once.
const DEFAULT_CNPG_CLUSTER: &str = "platform-postgres";
const DEFAULT_CNPG_NAMESPACE: &str = "cnpg-system";

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
    /// A pg database records BOTH the database name and the cluster it lives
    /// on.
    ///
    /// The cluster is not cosmetic. The shared-backend reaper deletes a CNPG
    /// `Cluster` nothing points at, and "points at" was defined entirely in
    /// terms of claims — so a cluster whose only occupant was a
    /// `SharedDatabase` read as empty and was on a dwell to be deleted, with
    /// the data underneath it. Recording the instance lets the reaper veto on
    /// an exact name match, the same way it does for a claim.
    pub fn pg(database: impl Into<String>, cluster: impl Into<String>) -> Self {
        Self {
            database: Some(database.into()),
            instance: Some(cluster.into()),
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

// ---------------------------------------------------------------------------
// Dynamic ApiResources + apply params
// ---------------------------------------------------------------------------

fn resourceclaim_ar() -> ApiResource {
    ApiResource::from_gvk(&GroupVersionKind::gvk(
        "apprafter.io",
        "v1alpha1",
        "ResourceClaim",
    ))
}

fn apply_params() -> PatchParams {
    PatchParams::apply(FIELD_MANAGER).force()
}

// ---------------------------------------------------------------------------
// Async reconcile
// ---------------------------------------------------------------------------

/// Reconcile one `SharedDatabase`.
///
/// 1. Under deletion → refuse while `refCount > 0`, else drop the backing and
///    release the finalizer.
/// 2. Ensure the finalizer, so a delete is observed at all.
/// 3. Branch on `spec.type`. Only `pg` is wired; `redis` reports why.
/// 4. Write the terminal status — `ready`, the backing, `refCount`, and BOTH
///    owned conditions in one body.
pub async fn reconcile_shared_database(
    sd: Arc<SharedDatabase>,
    ctx: Arc<Context>,
) -> Result<Action, ReconcileError> {
    let ns = sd.namespace().unwrap_or_default();
    let name = sd.name_any();
    let _timer = ctx
        .metrics
        .reconcile_duration
        .with_label_values(&[KIND])
        .start_timer();

    let prior: Vec<SharedDatabaseCondition> = sd
        .status
        .as_ref()
        .and_then(|s| s.conditions.clone())
        .unwrap_or_default();

    // 1. Deletion.
    let finalizers = sd.metadata.finalizers.clone().unwrap_or_default();
    if sd.metadata.deletion_timestamp.is_some() {
        if !finalizers.iter().any(|f| f == SD_FINALIZER) {
            return Ok(Action::await_change());
        }
        let binders = current_binders(&ctx.client, &ns, &name).await?;
        if !binders.is_empty() {
            // The webhook refuses this delete, so reaching here means the CR
            // was deleted by something that bypassed it — a force-delete, or a
            // cluster whose webhook was unavailable. Holding the finalizer is
            // the second gate, and the one that cannot be bypassed: the object
            // stays in Terminating with a condition naming the binders, and
            // the database keeps serving them.
            warn!(
                %name, %ns, binders = %binders.join(","),
                "SharedDatabase delete held: consumers are still bound"
            );
            let cond = ready_condition(
                "False",
                REASON_IN_USE,
                &format!(
                    "deletion is held while {} consumer(s) are still bound: {}. Remove \
                     `needs.<type>.ref` from each application first; this database and its \
                     data are untouched.",
                    binders.len(),
                    binders.join(", ")
                ),
                &prior,
            );
            write_status(
                &ctx,
                &sd,
                &ns,
                &name,
                false,
                None,
                binders.len() as i64,
                cond,
                extension_condition_of(&prior),
            )
            .await?;
            return Ok(Action::requeue(Duration::from_secs(30)));
        }
        drop_backing(&ctx, &sd, &ns, &name).await?;
        set_finalizers(&ctx.client, &ns, &name, without_finalizer(&finalizers)).await?;
        info!(%name, %ns, "SharedDatabase deleted — backing dropped, finalizer released");
        return Ok(Action::await_change());
    }

    // 2. Ensure the finalizer BEFORE provisioning anything. The other order
    //    leaves a window in which a database exists and a delete would not be
    //    observed, which is how an orphan carrying real data is made.
    if !finalizers.iter().any(|f| f == SD_FINALIZER) {
        set_finalizers(&ctx.client, &ns, &name, with_finalizer(&finalizers)).await?;
    }

    match sd.spec.type_.as_str() {
        "pg" => reconcile_pg(&sd, &ctx, &ns, &name, &prior).await,
        "redis" => reconcile_redis(&sd, &ctx, &ns, &name, &prior).await,
        other => {
            // Unreachable through the CRD, whose enum bounds the field to the
            // two above — but a controller that pattern-matches a string must
            // still say something when the string is neither, and "not
            // provisioned yet" is more use than a silent requeue.
            warn!(%name, %ns, type_ = %other, "SharedDatabase type not wired");
            let cond = ready_condition(
                "False",
                "UnsupportedType",
                &format!("shared databases of type {other:?} are not provisioned"),
                prior.as_slice(),
            );
            write_status(
                &ctx,
                &sd,
                &ns,
                &name,
                false,
                None,
                current_ref_count(&ctx.client, &ns, &name).await?,
                cond,
                None,
            )
            .await?;
            Ok(Action::requeue(Duration::from_secs(300)))
        }
    }
}

/// The pg arm: shared cluster → platform role → groups → database →
/// reader grants → extension probe → status.
async fn reconcile_pg(
    sd: &SharedDatabase,
    ctx: &Arc<Context>,
    ns: &str,
    name: &str,
    prior: &[SharedDatabaseCondition],
) -> Result<Action, ReconcileError> {
    // Provider selection uses the SharedDatabase's own selector, so a
    // namespace can be pinned to a particular pg provider exactly as a claim
    // can. An absent selector matches any pg provider.
    let providers: Vec<ServiceProvider> = Api::<ServiceProvider>::all(ctx.client.clone())
        .list(&Default::default())
        .await?
        .items;
    let candidates: Vec<Candidate> = providers.iter().map(Candidate::from_provider).collect();
    let selector = sd.spec.selector.clone().unwrap_or_default();
    let Some(provider_name) = select_provider("pg", &selector, &candidates) else {
        let cond = ready_condition(
            "False",
            "NoProvider",
            "no pg ServiceProvider matches this SharedDatabase's selector",
            prior,
        );
        write_status(
            ctx,
            sd,
            ns,
            name,
            false,
            None,
            current_ref_count(&ctx.client, ns, name).await?,
            cond,
            None,
        )
        .await?;
        return Ok(Action::requeue(Duration::from_secs(60)));
    };
    let cfg = providers
        .iter()
        .find(|p| p.name_any() == provider_name)
        .and_then(|p| p.spec.config.clone())
        .unwrap_or_else(|| json!({}));
    let cluster = cfg
        .pointer("/cluster")
        .and_then(Value::as_str)
        .unwrap_or(DEFAULT_CNPG_CLUSTER)
        .to_string();
    let cnpg_ns = cfg
        .pointer("/namespace")
        .and_then(Value::as_str)
        .unwrap_or(DEFAULT_CNPG_NAMESPACE)
        .to_string();

    // The shared CNPG Cluster, lazily. This line is the walk's first finding:
    // without it the next step's GET on the Cluster 404s on any cluster where
    // no owned pg claim has ever run, and the database never provisions. It
    // WORKED wherever an owned claim had already created it, which is why
    // nothing short of a fresh-cluster walk distinguishes the two.
    crate::reconcile::ensure_cnpg_cluster(ctx, &cfg, &cluster, &cnpg_ns).await?;

    // The platform role. One per CLUSTER, created through CNPG's
    // `managed.roles` so its password lives in a Secret CNPG reloads — and
    // NOT through SQL, because something has to be able to run the first
    // statement and this is that something.
    let pw_secret_name = cnpg::platform_role_secret_name(&cluster);
    let secret_api: Api<DynamicObject> =
        Api::namespaced_with(ctx.client.clone(), &cnpg_ns, &crate::reconcile::secret_ar());
    // READ-OR-CREATE, never an unconditional apply: `generate_password()`
    // returns a fresh value each call, so re-applying would rotate the
    // platform password on every reconcile of every shared database and break
    // whichever connection was open at the time.
    if secret_api.get_opt(&pw_secret_name).await?.is_none() {
        let pw = crate::reconcile::generate_password();
        let body = cnpg::basic_auth_secret(&pw_secret_name, &cnpg_ns, cnpg::PLATFORM_ROLE, &pw);
        secret_api
            .patch(&pw_secret_name, &apply_params(), &Patch::Apply(&body))
            .await?;
        info!(%cluster, %cnpg_ns, "created the platform role password Secret");
    }
    // A failure here is reported ON THE OBJECT rather than returned.
    //
    // The walk's second finding: when this was `?`, a `Cluster` that did not
    // yet exist produced an error the reconcile loop logged and retried, and
    // the SharedDatabase carried a finalizer and NO STATUS AT ALL. An operator
    // saw an object that was neither ready nor explained, and the only place
    // the reason existed was the operator's log — which is precisely the
    // shape this project treats as a bug rather than as terseness.
    if let Err(e) =
        crate::reconcile::upsert_platform_role(ctx, &cnpg_ns, &cluster, &pw_secret_name).await
    {
        warn!(%name, %ns, error = %e, "the shared cluster is not ready for the platform role yet");
        let cond = ready_condition(
            "False",
            REASON_AWAITING_CLUSTER,
            &format!("waiting for the shared PostgreSQL cluster {cluster} in {cnpg_ns} to come up"),
            prior,
        );
        write_status(
            ctx,
            sd,
            ns,
            name,
            false,
            None,
            current_ref_count(&ctx.client, ns, name).await?,
            cond,
            None,
        )
        .await?;
        return Ok(Action::requeue(Duration::from_secs(20)));
    }

    // The cluster must be answering before any SQL runs. Read the password
    // back rather than reusing the one generated above: on every reconcile
    // after the first there is no generated one, and the Secret is the single
    // source either way.
    let platform_pw =
        crate::acl_reconcile::read_secret_key(ctx, &cnpg_ns, &pw_secret_name, "password").await?;
    let database = shd_database_name(ns, name);
    let owner = shared_group(ns, name);

    // Connect to the DEFAULT database to create the groups: the shared
    // database does not exist yet on the first pass, and the groups must,
    // because CNPG will not create a `Database` whose owner is missing.
    let admin_dsn = cnpg::dsn(
        cnpg::PLATFORM_ROLE,
        &platform_pw,
        "postgres",
        &cluster,
        &cnpg_ns,
    );
    let groups = shared_pg::create_groups(ns, name, cnpg::PLATFORM_ROLE);
    if let Err(e) = ctx.pg.execute_all(&admin_dsn, &groups).await {
        // Not a hard error: a cluster still starting is the common case on a
        // fresh install, and the reason says so rather than surfacing a dial
        // failure as a provisioning fault.
        warn!(%name, %ns, error = %e, "shared cluster not answering yet");
        let cond = ready_condition(
            "False",
            REASON_AWAITING_CLUSTER,
            &format!("the shared PostgreSQL cluster {cluster} is not answering yet"),
            prior,
        );
        write_status(
            ctx,
            sd,
            ns,
            name,
            false,
            None,
            current_ref_count(&ctx.client, ns, name).await?,
            cond,
            None,
        )
        .await?;
        return Ok(Action::requeue(Duration::from_secs(20)));
    }

    // The database itself, owned by the group. Declarative through CNPG so
    // the extension list is CNPG's to apply and this controller never runs
    // `CREATE EXTENSION` as a privileged role itself.
    let object_name = shd_k8s_name(ns, name);
    let extensions: Vec<PgExtension> = sd.spec.extensions.clone().unwrap_or_default();
    let db_api: Api<DynamicObject> = Api::namespaced_with(
        ctx.client.clone(),
        &cnpg_ns,
        &crate::reconcile::database_ar(),
    );
    let db_body = cnpg::database_object(
        &object_name,
        &cnpg_ns,
        &cluster,
        &database,
        &owner,
        "present",
        &extensions,
    );
    db_api
        .patch(&object_name, &apply_params(), &Patch::Apply(&db_body))
        .await?;

    // The reader grants run against the shared database, so they need it to
    // exist — which is CNPG's job and takes a moment.
    let db_dsn = cnpg::dsn(
        cnpg::PLATFORM_ROLE,
        &platform_pw,
        &database,
        &cluster,
        &cnpg_ns,
    );
    if let Err(e) = ctx
        .pg
        .execute_all(&db_dsn, &shared_pg::grant_reader(ns, name, &database))
        .await
    {
        warn!(%name, %ns, error = %e, "shared database not ready for reader grants yet");
        let cond = ready_condition(
            "False",
            REASON_AWAITING_DATABASE,
            &format!("waiting for CNPG to create {database} in {cluster}"),
            prior,
        );
        write_status(
            ctx,
            sd,
            ns,
            name,
            false,
            None,
            current_ref_count(&ctx.client, ns, name).await?,
            cond,
            None,
        )
        .await?;
        return Ok(Action::requeue(Duration::from_secs(20)));
    }

    // Extensions: ask the running server, never infer from the allow list.
    // Whether `vector` exists is a property of the operand IMAGE, and the
    // image is not pinned (ADR 0066 §4.2).
    let probe = crate::pg_client::probe_extensions(ctx.pg.as_ref(), &db_dsn, &extensions).await;
    let missing = match &probe {
        crate::pg_client::ExtensionProbe::Missing(m) => m.clone(),
        // An unreachable server says nothing about extensions. Carry the
        // PREVIOUS finding forward rather than clearing it: a warning that
        // blinks out on a network hiccup and returns is worse than one that
        // stays until something actually contradicts it.
        crate::pg_client::ExtensionProbe::Unreachable => prior
            .iter()
            .find(|c| c.type_ == COND_EXTENSION_UNAVAILABLE)
            .and(Some(previously_missing(prior)))
            .unwrap_or_default(),
        crate::pg_client::ExtensionProbe::AllPresent => Vec::new(),
    };
    let ext_cond = extension_unavailable_condition(&missing, prior);

    let ref_count = current_ref_count(&ctx.client, ns, name).await?;
    // A missing extension leaves the database usable — it exists, it accepts
    // connections, and every consumer already bound keeps working. So this is
    // a WARNING condition beside a Ready=True, not a Ready=False: reporting
    // the database as not ready would make an existing binding look broken
    // because a NEW extension was added to the list and is not in the image.
    let cond = ready_condition(
        "True",
        "Provisioned",
        &format!("{database} in {cluster} ({cnpg_ns}), owned by {owner}"),
        prior,
    );
    write_status(
        ctx,
        sd,
        ns,
        name,
        true,
        Some(&Backing::pg(&database, &cluster)),
        ref_count,
        cond,
        ext_cond,
    )
    .await?;
    Ok(Action::requeue(Duration::from_secs(300)))
}

/// The redis arm: pool instance → a `$N` of its own → status.
///
/// No ACL user is created here. A shared cache's consumers each get their own
/// (`bind_redis_consumer`), all pinned to this one `$N` and all sharing one
/// channel prefix — the prefix is per DATABASE rather than per user because
/// Dragonfly channels are not `$N`-scoped, so a per-user prefix would leave
/// two consumers of one cache unable to pub/sub to each other.
async fn reconcile_redis(
    sd: &SharedDatabase,
    ctx: &Arc<Context>,
    ns: &str,
    name: &str,
    prior: &[SharedDatabaseCondition],
) -> Result<Action, ReconcileError> {
    let providers: Vec<ServiceProvider> = Api::<ServiceProvider>::all(ctx.client.clone())
        .list(&Default::default())
        .await?
        .items;
    let candidates: Vec<Candidate> = providers.iter().map(Candidate::from_provider).collect();
    let selector = sd.spec.selector.clone().unwrap_or_default();
    let Some(provider_name) = select_provider("redis", &selector, &candidates) else {
        let cond = ready_condition(
            "False",
            "NoProvider",
            "no redis ServiceProvider matches this SharedDatabase's selector",
            prior,
        );
        write_status(
            ctx,
            sd,
            ns,
            name,
            false,
            None,
            current_ref_count(&ctx.client, ns, name).await?,
            cond,
            None,
        )
        .await?;
        return Ok(Action::requeue(Duration::from_secs(60)));
    };
    let Some(provider) = providers
        .into_iter()
        .find(|p| p.name_any() == provider_name)
    else {
        return Ok(Action::requeue(Duration::from_secs(60)));
    };

    let persistent = sd.spec.persistent.unwrap_or(false);
    let pool = crate::reconcile::ensure_dragonfly_instance(ctx, &provider, persistent).await?;

    // Reuse this database's OWN allocation before consulting the allocator.
    // Without the short-circuit a re-reconcile would see its own `$N` in the
    // reserved set, call it taken, and move the database to a different one —
    // leaving every bound consumer pinned to a keyspace nobody writes to.
    let existing = sd
        .status
        .as_ref()
        .and_then(|st| match (st.instance.as_deref(), st.dbnum) {
            (Some(i), Some(n)) if i == pool.instance => u16::try_from(n).ok(),
            _ => None,
        });

    let dbnum = match existing {
        Some(n) => n,
        None => {
            let live: Vec<ResourceClaim> = Api::<ResourceClaim>::all(ctx.client.clone())
                .list(&Default::default())
                .await?
                .items;
            let retained: Vec<operator_core::RetainedClaim> =
                Api::<operator_core::RetainedClaim>::namespaced(
                    ctx.client.clone(),
                    crate::reconcile::RETAINED_CLAIM_NAMESPACE,
                )
                .list(&Default::default())
                .await?
                .items;
            let shared: Vec<SharedDatabase> = Api::<SharedDatabase>::all(ctx.client.clone())
                .list(&Default::default())
                .await?
                .items;
            let used = crate::dragonfly::used_dbnums(&live, &retained, &shared, &pool.instance);
            let Some(n) = crate::dragonfly::allocate_dbnum(&used, pool.dbnum_max) else {
                let cond = ready_condition(
                    "False",
                    "InsufficientCapacity",
                    &format!(
                        "every one of {}'s {} logical databases is taken — the pool needs \
                         another instance",
                        pool.instance, pool.dbnum_max
                    ),
                    prior,
                );
                write_status(
                    ctx,
                    sd,
                    ns,
                    name,
                    false,
                    None,
                    current_ref_count(&ctx.client, ns, name).await?,
                    cond,
                    None,
                )
                .await?;
                return Ok(Action::requeue(Duration::from_secs(120)));
            };
            // Recycle-safety (ADR 0042 §3): a reused `$N` must start empty, or
            // the first consumer of this shared cache reads a departed
            // tenant's keys. Only on a FRESH allocation — flushing an existing
            // one would wipe the data this database exists to hold.
            let addr = crate::dragonfly::instance_addr(&pool.instance, &pool.df_ns);
            let admin_pw = crate::acl_reconcile::read_secret_key(
                ctx,
                &pool.df_ns,
                &crate::dragonfly::admin_secret_name(&pool.instance),
                "password",
            )
            .await?;
            ctx.redis
                .flushdb(&addr, &admin_pw, n)
                .await
                .map_err(|e| ReconcileError::Provisioning(format!("flushdb ${n}: {e}")))?;
            n
        }
    };

    let ref_count = current_ref_count(&ctx.client, ns, name).await?;
    let cond = ready_condition(
        "True",
        "Provisioned",
        &format!("${dbnum} on {} ({})", pool.instance, pool.df_ns),
        prior,
    );
    write_status(
        ctx,
        sd,
        ns,
        name,
        true,
        Some(&Backing::redis(&pool.instance, i64::from(dbnum))),
        ref_count,
        cond,
        None,
    )
    .await?;
    Ok(Action::requeue(Duration::from_secs(300)))
}

/// Drop the backing resource at `refCount == 0`.
///
/// This is the one place in 2.29 that destroys data, and it runs only behind
/// two gates — the webhook's refusal and the finalizer's own recount — so what
/// it must not do is half the job silently. Both arms therefore resolve the
/// provider rather than assuming a namespace: a provider seed that moves the
/// CNPG cluster or the Dragonfly pool would otherwise leave the backing
/// orphaned while the CR disappeared, and nothing would ever mention it again.
///
/// pg: declare the CNPG `Database` absent, wait for CNPG to act on it, THEN
/// drop the two groups. The order is forced — the owning group owns the
/// database, and Postgres refuses to drop a role that does.
///
/// redis: flush the `$N`. The number itself needs no release: the allocator
/// derives the reserved set from live objects, so it frees when the CR goes.
/// The flush is what makes `db rm` mean what its prompt says.
async fn drop_backing(
    ctx: &Arc<Context>,
    sd: &SharedDatabase,
    ns: &str,
    name: &str,
) -> Result<(), ReconcileError> {
    let providers: Vec<ServiceProvider> = Api::<ServiceProvider>::all(ctx.client.clone())
        .list(&Default::default())
        .await?
        .items;
    let candidates: Vec<Candidate> = providers.iter().map(Candidate::from_provider).collect();
    let selector = sd.spec.selector.clone().unwrap_or_default();
    let cfg = select_provider(&sd.spec.type_, &selector, &candidates)
        .and_then(|n| providers.iter().find(|p| p.name_any() == n))
        .and_then(|p| p.spec.config.clone())
        .unwrap_or_else(|| json!({}));

    match sd.spec.type_.as_str() {
        "pg" => {
            // Never provisioned → nothing to drop. Checked against the
            // STATUS rather than by probing, because a probe that failed for
            // a network reason would read the same as "absent" and the
            // groups would be dropped out from under a live database.
            if sd
                .status
                .as_ref()
                .and_then(|s| s.database.as_ref())
                .is_none()
            {
                return Ok(());
            }
            let cluster = cfg
                .pointer("/cluster")
                .and_then(Value::as_str)
                .unwrap_or(DEFAULT_CNPG_CLUSTER)
                .to_string();
            let cnpg_ns = cfg
                .pointer("/namespace")
                .and_then(Value::as_str)
                .unwrap_or(DEFAULT_CNPG_NAMESPACE)
                .to_string();
            let object_name = shd_k8s_name(ns, name);
            let db_api: Api<DynamicObject> = Api::namespaced_with(
                ctx.client.clone(),
                &cnpg_ns,
                &crate::reconcile::database_ar(),
            );
            if let Some(existing) = db_api.get_opt(&object_name).await? {
                // The FULL body with `ensure: absent`, not a partial apply:
                // SSA replaces this manager's field-set, and a body carrying
                // only `ensure` would strip the `cluster`/`name`/`owner` it
                // also owns and leave CNPG unable to act on the drop.
                let mut spec = existing.data["spec"].clone();
                spec["ensure"] = json!("absent");
                let full = json!({
                    "apiVersion": "postgresql.cnpg.io/v1",
                    "kind": "Database",
                    "metadata": { "name": object_name, "namespace": cnpg_ns },
                    "spec": spec,
                });
                db_api
                    .patch(&object_name, &apply_params(), &Patch::Apply(&full))
                    .await?;
                info!(%name, %ns, %object_name, "declared the shared Database absent");
            }

            // The groups. Best-effort on the CONNECTION, like every other SQL
            // step here: a cluster that is down must not wedge the delete
            // forever, and the statements are existence-guarded so the next
            // pass completes what this one could not.
            let pw_secret = cnpg::platform_role_secret_name(&cluster);
            match crate::acl_reconcile::read_secret_key(ctx, &cnpg_ns, &pw_secret, "password").await
            {
                Ok(pw) => {
                    let dsn = cnpg::dsn(cnpg::PLATFORM_ROLE, &pw, "postgres", &cluster, &cnpg_ns);
                    if let Err(e) = ctx
                        .pg
                        .execute_all(&dsn, &shared_pg::drop_groups(ns, name))
                        .await
                    {
                        // Expected on the first pass: CNPG has not dropped the
                        // database yet, so its owner cannot go. The requeue
                        // picks it up.
                        warn!(%name, %ns, error = %e, "could not drop the groups yet");
                    } else {
                        info!(%name, %ns, "dropped the shared database's groups");
                    }
                }
                Err(e) => {
                    warn!(%name, %ns, error = %e, "platform role secret unreadable; groups left")
                }
            }
        }
        "redis" => {
            let st = sd.status.as_ref();
            let (Some(instance), Some(dbnum)) = (
                st.and_then(|s| s.instance.clone()),
                st.and_then(|s| s.dbnum).and_then(|n| u16::try_from(n).ok()),
            ) else {
                return Ok(());
            };
            let df_ns = cfg
                .pointer("/namespace")
                .and_then(Value::as_str)
                .unwrap_or("dragonfly-system")
                .to_string();
            let addr = crate::dragonfly::instance_addr(&instance, &df_ns);
            match crate::acl_reconcile::read_secret_key(
                ctx,
                &df_ns,
                &crate::dragonfly::admin_secret_name(&instance),
                "password",
            )
            .await
            {
                Ok(admin_pw) => {
                    if let Err(e) = ctx.redis.flushdb(&addr, &admin_pw, dbnum).await {
                        warn!(%name, %ns, error = %e, "could not flush the shared keyspace");
                    } else {
                        info!(%name, %ns, %instance, dbnum, "flushed the shared keyspace");
                    }
                }
                Err(e) => {
                    warn!(%name, %ns, error = %e, "instance admin secret unreadable; keyspace left")
                }
            }
        }
        _ => {}
    }
    Ok(())
}

/// Which backing a status write carries: the caller's override, or — when
/// there is none — whatever the object already reports.
///
/// Extracted from [`write_status`] so the CHOICE is testable. Asserting the
/// composed body directly did not cover it: a test that calls the body builder
/// with `backing_of(sd)` passes whether or not `write_status` actually
/// consults `backing_of`, which is precisely what a mutation of this default
/// demonstrated.
fn backing_for_write(override_: Option<&Backing>, sd: &SharedDatabase) -> Backing {
    match override_ {
        Some(b) => b.clone(),
        None => backing_of(sd),
    }
}

/// The backing recorded in the CR's own status, for a status write that must
/// not lose it. SSA prunes an omitted field, so a refusal path that rebuilt
/// an empty [`Backing`] would erase the database name from a CR that still
/// has one.
fn backing_of(sd: &SharedDatabase) -> Backing {
    let Some(st) = sd.status.as_ref() else {
        return Backing::default();
    };
    Backing {
        database: st.database.clone(),
        instance: st.instance.clone(),
        dbnum: st.dbnum,
    }
}

/// The `ExtensionUnavailable` condition already on the object, carried
/// forward by a path that did not re-probe. Same prune rule as
/// [`backing_of`].
fn extension_condition_of(prior: &[SharedDatabaseCondition]) -> Option<SharedDatabaseCondition> {
    prior
        .iter()
        .find(|c| c.type_ == COND_EXTENSION_UNAVAILABLE && c.status == "True")
        .cloned()
}

/// The extension names named by a previous `ExtensionUnavailable` message.
///
/// Parsed back out rather than stored separately: the condition IS the record,
/// and a second copy in the status would be one more field to keep in step.
fn previously_missing(prior: &[SharedDatabaseCondition]) -> Vec<String> {
    prior
        .iter()
        .find(|c| c.type_ == COND_EXTENSION_UNAVAILABLE && c.status == "True")
        .and_then(|c| c.message.as_deref())
        .and_then(|m| m.rsplit_once(": "))
        .map(|(_, list)| {
            list.split(", ")
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

pub fn error_policy_sd(sd: Arc<SharedDatabase>, err: &ReconcileError, ctx: Arc<Context>) -> Action {
    let name = sd.name_any();
    let namespace = sd.namespace().unwrap_or_default();
    warn!(%name, %namespace, %err, "SharedDatabase reconcile error");
    ctx.metrics
        .reconcile_total
        .with_label_values(&[KIND, &namespace, "error"])
        .inc();
    ctx.metrics
        .reconcile_errors
        .with_label_values(&[KIND])
        .inc();
    Action::requeue(Duration::from_secs(30))
}

// ---------------------------------------------------------------------------
// The consumer bind path (claim side)
// ---------------------------------------------------------------------------

/// `Ready=False` reason while the referenced `SharedDatabase` is absent or
/// not yet ready.
const REASON_AWAITING_SHARED_DATABASE: &str = "AwaitingSharedDatabase";

/// Bind ONE consumer claim to an existing shared pg database (ADR 0066 §3.1).
///
/// Provisions nothing shared: the database, the groups and the reader grants
/// belong to [`reconcile_shared_database`]. What this creates is the
/// consumer's own identity — a login role, a password, a connection Secret —
/// and it drops exactly those and nothing else when the claim goes.
///
/// The connection Secret has the SAME shape the owned-pg arm writes, which is
/// what lets `claim.pg.*` references, the egress rule and the readiness gate
/// stay unaware that the database is shared.
pub async fn bind_pg_consumer(
    ctx: &Arc<Context>,
    claim: &Arc<ResourceClaim>,
    ns: &str,
    name: &str,
    provider: &ServiceProvider,
) -> Result<Action, ReconcileError> {
    let prior: Vec<operator_core::ResourceClaimCondition> = claim
        .status
        .as_ref()
        .and_then(|s| s.conditions.clone())
        .unwrap_or_default();
    let shared_name = claim
        .spec
        .shared_ref
        .as_deref()
        .ok_or_else(|| ReconcileError::Provisioning("bind called without sharedRef".into()))?;

    // The SharedDatabase must exist, in THIS namespace, and be ready. Not
    // ready is the ordinary case on a fresh cluster (the database is being
    // created right now), so it requeues rather than failing.
    let sd_api: Api<SharedDatabase> = Api::namespaced(ctx.client.clone(), ns);
    let Some(sd) = sd_api.get_opt(shared_name).await? else {
        let cond = crate::reconcile::ready_condition(
            "False",
            REASON_AWAITING_SHARED_DATABASE,
            &format!(
                "no SharedDatabase {shared_name:?} in namespace {ns}. A shared database is \
                 created out of band (`apprafter db create`) and is never created by an \
                 application that references it — otherwise the first application to mention \
                 a name would own a database everybody else inherits."
            ),
            &prior,
        );
        crate::reconcile::patch_status(&ctx.client, ns, name, cond, Default::default()).await?;
        return Ok(Action::requeue(Duration::from_secs(30)));
    };
    let (Some(true), Some(database)) = (
        sd.status.as_ref().and_then(|s| s.ready),
        sd.status.as_ref().and_then(|s| s.database.clone()),
    ) else {
        let cond = crate::reconcile::ready_condition(
            "False",
            REASON_AWAITING_SHARED_DATABASE,
            &format!("SharedDatabase {shared_name:?} is not ready yet"),
            &prior,
        );
        crate::reconcile::patch_status(&ctx.client, ns, name, cond, Default::default()).await?;
        return Ok(Action::requeue(Duration::from_secs(20)));
    };

    let cfg = provider.spec.config.clone().unwrap_or_else(|| json!({}));
    let cluster = cfg
        .pointer("/cluster")
        .and_then(Value::as_str)
        .unwrap_or(DEFAULT_CNPG_CLUSTER)
        .to_string();
    let cnpg_ns = cfg
        .pointer("/namespace")
        .and_then(Value::as_str)
        .unwrap_or(DEFAULT_CNPG_NAMESPACE)
        .to_string();

    let role = consumer_role(ns, name);
    let conn_secret_name = crate::reconcile::connection_secret_name(name);
    let conn_api: Api<DynamicObject> =
        Api::namespaced_with(ctx.client.clone(), ns, &crate::reconcile::secret_ar());

    // READ-OR-GENERATE the password, rather than a fresh one per reconcile.
    //
    // The owned-pg arm regenerates every pass and lets CNPG reset the role to
    // match. That works because CNPG owns both ends. Here the two ends are the
    // SQL this controller runs and the Secret it writes, and between them
    // there is a window in which the server has the new password and the
    // Secret still has the old one — an application restarting in that window
    // gets an auth failure, on a reconcile that changed nothing it asked for.
    // Reusing the stored password closes the window in steady state: the bind
    // is then an ALTER to the value already in the Secret, which is a no-op
    // the server accepts.
    let password = match conn_api.get_opt(&conn_secret_name).await? {
        Some(existing) => read_secret_string(&existing, "pass")
            .unwrap_or_else(crate::reconcile::generate_password),
        None => crate::reconcile::generate_password(),
    };

    let access = shared_pg::Access::from_spec(claim.spec.access.as_deref());
    let platform_pw = crate::acl_reconcile::read_secret_key(
        ctx,
        &cnpg_ns,
        &cnpg::platform_role_secret_name(&cluster),
        "password",
    )
    .await?;
    let admin_dsn = cnpg::dsn(
        cnpg::PLATFORM_ROLE,
        &platform_pw,
        &database,
        &cluster,
        &cnpg_ns,
    );
    let statements = shared_pg::bind_consumer(ns, shared_name, &role, &database, &password, access);
    // NOTHING from `statements` may be logged: `bind_consumer` interpolates
    // the password, because `CREATE ROLE` is a utility statement PostgreSQL
    // will not parameterise. `PgAdminError` carries only an index, which is
    // what makes this error safe to surface at all.
    if let Err(e) = ctx.pg.execute_all(&admin_dsn, &statements).await {
        warn!(%name, %ns, error = %e, "binding the consumer failed");
        let cond = crate::reconcile::ready_condition(
            "False",
            REASON_AWAITING_SHARED_DATABASE,
            &format!("could not bind to {shared_name:?}: {e}"),
            &prior,
        );
        crate::reconcile::patch_status(&ctx.client, ns, name, cond, Default::default()).await?;
        return Ok(Action::requeue(Duration::from_secs(20)));
    }

    // The connection Secret, identical in shape to the owned-pg arm's.
    let pg_host = format!("{cluster}-rw.{cnpg_ns}.svc");
    let owner_uid = claim.metadata.uid.clone().unwrap_or_default();
    let conn_secret = crate::reconcile::connection_secret_object(
        &conn_secret_name,
        ns,
        &role,
        &password,
        &pg_host,
        5432,
        &database,
        &owner_uid,
        name,
    );
    conn_api
        .patch(
            &conn_secret_name,
            &apply_params(),
            &Patch::Apply(&conn_secret),
        )
        .await?;

    let level = match access {
        shared_pg::Access::ReadWrite => "rw",
        shared_pg::Access::ReadOnly => "ro",
    };
    info!(%name, %ns, %shared_name, %role, %level, "bound consumer to shared database");
    let cond = crate::reconcile::ready_condition(
        "True",
        "Provisioned",
        &format!("bound to shared database {shared_name:?} ({database}) as {level}"),
        &prior,
    );
    crate::reconcile::patch_status(
        &ctx.client,
        ns,
        name,
        cond,
        crate::reconcile::ClaimStatusFields {
            conn_secret_name: Some(&conn_secret_name),
            ..Default::default()
        },
    )
    .await?;
    Ok(Action::requeue(Duration::from_secs(300)))
}

/// Bind ONE consumer claim to an existing shared redis keyspace
/// (ADR 0066 §3.2).
///
/// The consumer gets its own ACL user, pinned to the database's `$N` and
/// scoped to the database's SHARED channel prefix — not its own. Dragonfly
/// channels are not `$N`-scoped, so a per-user prefix (what an owned claim
/// gets) would leave two consumers of one cache unable to publish to each
/// other, which is most of what sharing a cache is for.
pub async fn bind_redis_consumer(
    ctx: &Arc<Context>,
    claim: &Arc<ResourceClaim>,
    ns: &str,
    name: &str,
    provider: &ServiceProvider,
) -> Result<Action, ReconcileError> {
    let prior: Vec<operator_core::ResourceClaimCondition> = claim
        .status
        .as_ref()
        .and_then(|s| s.conditions.clone())
        .unwrap_or_default();
    let shared_name = claim
        .spec
        .shared_ref
        .as_deref()
        .ok_or_else(|| ReconcileError::Provisioning("bind called without sharedRef".into()))?;

    let sd_api: Api<SharedDatabase> = Api::namespaced(ctx.client.clone(), ns);
    let Some(sd) = sd_api.get_opt(shared_name).await? else {
        let cond = crate::reconcile::ready_condition(
            "False",
            REASON_AWAITING_SHARED_DATABASE,
            &format!("no SharedDatabase {shared_name:?} in namespace {ns}"),
            &prior,
        );
        crate::reconcile::patch_status(&ctx.client, ns, name, cond, Default::default()).await?;
        return Ok(Action::requeue(Duration::from_secs(30)));
    };
    let st = sd.status.as_ref();
    let (Some(true), Some(instance), Some(dbnum)) = (
        st.and_then(|s| s.ready),
        st.and_then(|s| s.instance.clone()),
        st.and_then(|s| s.dbnum).and_then(|n| u16::try_from(n).ok()),
    ) else {
        let cond = crate::reconcile::ready_condition(
            "False",
            REASON_AWAITING_SHARED_DATABASE,
            &format!("SharedDatabase {shared_name:?} is not ready yet"),
            &prior,
        );
        crate::reconcile::patch_status(&ctx.client, ns, name, cond, Default::default()).await?;
        return Ok(Action::requeue(Duration::from_secs(20)));
    };

    let cfg = provider.spec.config.clone().unwrap_or_else(|| json!({}));
    let df_ns = cfg
        .pointer("/namespace")
        .and_then(Value::as_str)
        .unwrap_or("dragonfly-system")
        .to_string();

    let user = crate::dragonfly::acl_user(ns, name);
    let conn_secret_name = crate::reconcile::connection_secret_name(name);
    let conn_api: Api<DynamicObject> =
        Api::namespaced_with(ctx.client.clone(), ns, &crate::reconcile::secret_ar());
    // Same read-or-generate as the pg bind, and for the same reason: an
    // owned claim can regenerate per reconcile because it also rewrites its
    // Secret in the same pass with nobody else reading the old value, whereas
    // here the re-pin and the Secret write are two steps with a window
    // between them.
    let password = match conn_api.get_opt(&conn_secret_name).await? {
        Some(existing) => read_secret_string(&existing, "pass")
            .unwrap_or_else(crate::reconcile::generate_password),
        None => crate::reconcile::generate_password(),
    };

    let access = match claim.spec.access.as_deref() {
        // Anything unrecognised is the LESSER privilege, matching
        // `shared_pg::Access::from_spec`: the webhook bounds the field to the
        // two, so an unexpected value means something upstream is wrong, and
        // the safe reading of that is read-only.
        Some("rw") | None => crate::dragonfly::SharedAccess::ReadWrite,
        Some(_) => crate::dragonfly::SharedAccess::ReadOnly,
    };
    let channel_prefix = shd_database_name(ns, shared_name);
    let args = crate::dragonfly::acl_setuser_args_scoped(
        &user,
        &password,
        dbnum,
        Some(&channel_prefix),
        access,
    );

    let addr = crate::dragonfly::instance_addr(&instance, &df_ns);
    let admin_pw = crate::acl_reconcile::read_secret_key(
        ctx,
        &df_ns,
        &crate::dragonfly::admin_secret_name(&instance),
        "password",
    )
    .await?;
    if let Err(e) = ctx.redis.acl_setuser(&addr, &admin_pw, &args).await {
        warn!(%name, %ns, error = %e, "binding the cache consumer failed");
        let cond = crate::reconcile::ready_condition(
            "False",
            REASON_AWAITING_SHARED_DATABASE,
            &format!("could not bind to {shared_name:?}: {e}"),
            &prior,
        );
        crate::reconcile::patch_status(&ctx.client, ns, name, cond, Default::default()).await?;
        return Ok(Action::requeue(Duration::from_secs(20)));
    }

    let redis_host = format!("{instance}.{df_ns}.svc");
    let owner_uid = claim.metadata.uid.clone().unwrap_or_default();
    let conn_secret = crate::reconcile::redis_connection_secret_object(
        &conn_secret_name,
        ns,
        &user,
        &password,
        &redis_host,
        6379,
        dbnum,
        // The SHARED prefix, so what the application is told to prefix its
        // channels with is the same thing the ACL above actually allows.
        &format!("{channel_prefix}:"),
        &owner_uid,
        name,
    );
    conn_api
        .patch(
            &conn_secret_name,
            &apply_params(),
            &Patch::Apply(&conn_secret),
        )
        .await?;

    let level = match access {
        crate::dragonfly::SharedAccess::ReadWrite => "rw",
        crate::dragonfly::SharedAccess::ReadOnly => "ro",
    };
    info!(%name, %ns, %shared_name, %user, dbnum, %level, "bound consumer to shared cache");
    let cond = crate::reconcile::ready_condition(
        "True",
        "Provisioned",
        &format!("bound to shared cache {shared_name:?} (${dbnum} on {instance}) as {level}"),
        &prior,
    );
    crate::reconcile::patch_status(
        &ctx.client,
        ns,
        name,
        cond,
        crate::reconcile::ClaimStatusFields {
            conn_secret_name: Some(&conn_secret_name),
            // The consumer does NOT own this allocation — the SharedDatabase
            // does. Publishing it on the claim would put the same `$N` in two
            // places the allocator reads, and a claim that outlived its
            // database would then reserve a keyspace nothing owns.
            ..Default::default()
        },
    )
    .await?;
    Ok(Action::requeue(Duration::from_secs(300)))
}

/// Revoke ONE consumer's credential when its claim is deleted (ADR 0066 §6).
///
/// Drops that consumer's role or ACL user and nothing else. The shared
/// database and its data are never touched by a consumer's lifecycle — the
/// property the whole CRD exists to provide — so this is deliberately the
/// smallest possible cleanup.
///
/// BEST-EFFORT, and it returns no error on purpose. This runs inside the
/// finalizer, and a backend that is momentarily unreachable must not wedge a
/// delete forever: an application stuck in Terminating because a database is
/// restarting is a worse outcome than a role that outlives its Secret by a
/// few minutes. The cost is bounded — the connection Secret cascades with the
/// claim either way, so the password stops being readable from the cluster at
/// the same moment regardless, and what survives a failure here is a login
/// nobody holds.
///
/// It is not silent, though: a failure is logged at WARN with the role named,
/// which is what an operator needs in order to drop it by hand.
pub async fn revoke_consumer(ctx: &Arc<Context>, claim: &Arc<ResourceClaim>, ns: &str, name: &str) {
    let Some(shared_name) = claim.spec.shared_ref.as_deref() else {
        return;
    };
    let sd_api: Api<SharedDatabase> = Api::namespaced(ctx.client.clone(), ns);
    let sd = match sd_api.get_opt(shared_name).await {
        Ok(Some(sd)) => sd,
        // The database is gone too — its own delete drops the groups, and a
        // consumer role inside a dropped database goes with `DROP OWNED`.
        Ok(None) => return,
        Err(e) => {
            warn!(%name, %ns, error = %e, "could not read the shared database to revoke a consumer");
            return;
        }
    };

    let providers: Vec<ServiceProvider> = match Api::<ServiceProvider>::all(ctx.client.clone())
        .list(&Default::default())
        .await
    {
        Ok(l) => l.items,
        Err(e) => {
            warn!(%name, %ns, error = %e, "could not list providers to revoke a consumer");
            return;
        }
    };
    let candidates: Vec<Candidate> = providers.iter().map(Candidate::from_provider).collect();
    let selector = sd.spec.selector.clone().unwrap_or_default();
    let cfg = select_provider(&sd.spec.type_, &selector, &candidates)
        .and_then(|n| providers.iter().find(|p| p.name_any() == n))
        .and_then(|p| p.spec.config.clone())
        .unwrap_or_else(|| json!({}));

    match sd.spec.type_.as_str() {
        "pg" => {
            let Some(database) = sd.status.as_ref().and_then(|s| s.database.clone()) else {
                return;
            };
            let cluster = cfg
                .pointer("/cluster")
                .and_then(Value::as_str)
                .unwrap_or(DEFAULT_CNPG_CLUSTER)
                .to_string();
            let cnpg_ns = cfg
                .pointer("/namespace")
                .and_then(Value::as_str)
                .unwrap_or(DEFAULT_CNPG_NAMESPACE)
                .to_string();
            let role = consumer_role(ns, name);
            let pw_secret = cnpg::platform_role_secret_name(&cluster);
            let Ok(pw) =
                crate::acl_reconcile::read_secret_key(ctx, &cnpg_ns, &pw_secret, "password").await
            else {
                warn!(%name, %ns, %role, "platform role secret unreadable; consumer role NOT dropped");
                return;
            };
            // Connected to the SHARED database, not to `postgres`: `DROP
            // OWNED BY` is per-database, and running it elsewhere would drop
            // the role while leaving whatever it owns here behind.
            let dsn = cnpg::dsn(cnpg::PLATFORM_ROLE, &pw, &database, &cluster, &cnpg_ns);
            if let Err(e) = ctx
                .pg
                .execute_all(&dsn, &shared_pg::unbind_consumer(&role))
                .await
            {
                warn!(%name, %ns, %role, error = %e, "consumer role NOT dropped — drop it by hand");
            } else {
                info!(%name, %ns, %role, "revoked the consumer's role");
            }
        }
        "redis" => {
            let Some(instance) = sd.status.as_ref().and_then(|s| s.instance.clone()) else {
                return;
            };
            let df_ns = cfg
                .pointer("/namespace")
                .and_then(Value::as_str)
                .unwrap_or("dragonfly-system")
                .to_string();
            let user = crate::dragonfly::acl_user(ns, name);
            let addr = crate::dragonfly::instance_addr(&instance, &df_ns);
            let Ok(admin_pw) = crate::acl_reconcile::read_secret_key(
                ctx,
                &df_ns,
                &crate::dragonfly::admin_secret_name(&instance),
                "password",
            )
            .await
            else {
                warn!(%name, %ns, %user, "instance admin secret unreadable; ACL user NOT dropped");
                return;
            };
            // DELUSER only. NOT flushdb — the keyspace belongs to the shared
            // database and its other consumers are still using it. That one
            // line is the difference between removing an application and
            // wiping everybody's cache.
            if let Err(e) = ctx.redis.acl_deluser(&addr, &admin_pw, &user).await {
                warn!(%name, %ns, %user, error = %e, "ACL user NOT dropped — drop it by hand");
            } else {
                info!(%name, %ns, %user, "revoked the consumer's ACL user");
            }
        }
        _ => {}
    }
}

/// Read one `data` key off a Secret object, base64-decoded.
///
/// `stringData` is write-only — the apiserver folds it into `data` — so a
/// read-back always goes through `data`.
fn read_secret_string(secret: &DynamicObject, key: &str) -> Option<String> {
    use base64::Engine as _;
    let encoded = secret.data.pointer(&format!("/data/{key}"))?.as_str()?;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .ok()?;
    String::from_utf8(bytes).ok()
}

// ---------------------------------------------------------------------------
// Status + finalizer I/O
// ---------------------------------------------------------------------------

/// The namespace's claims, as raw JSON, for the pure `refCount` helpers.
async fn namespace_claims(client: &Client, ns: &str) -> Result<Vec<Value>, ReconcileError> {
    Ok(
        Api::<DynamicObject>::namespaced_with(client.clone(), ns, &resourceclaim_ar())
            .list(&Default::default())
            .await?
            .items
            .iter()
            .map(serde_json::to_value)
            .collect::<Result<_, _>>()?,
    )
}

async fn current_ref_count(client: &Client, ns: &str, name: &str) -> Result<i64, ReconcileError> {
    Ok(ref_count_for(name, &namespace_claims(client, ns).await?))
}

async fn current_binders(
    client: &Client,
    ns: &str,
    name: &str,
) -> Result<Vec<String>, ReconcileError> {
    Ok(binders_of(name, &namespace_claims(client, ns).await?))
}

/// SSA-write the terminal status under the provisioner field manager.
///
/// `backing` is an OVERRIDE, and `None` — carry forward whatever the object
/// already reports — is the default on purpose.
///
/// SSA replaces this manager's owned field-set on every apply, so a body that
/// omits `status.database` DELETES it. The first version of this controller
/// passed a freshly-defaulted `Backing` on each failure path, which meant a
/// CNPG hiccup erased the database name from a provisioned object: `db status`
/// showed no backing, every consumer's bind refused with "not ready", and —
/// the serious half — a delete arriving during the outage read the now-absent
/// name as "never provisioned" and skipped dropping the database entirely.
///
/// Making the safe thing the default is the repair. Erasing the backing is
/// still possible, and still what the provisioning paths do, but it now takes
/// saying so.
#[allow(clippy::too_many_arguments)]
async fn write_status(
    ctx: &Arc<Context>,
    sd: &SharedDatabase,
    ns: &str,
    name: &str,
    ready: bool,
    backing: Option<&Backing>,
    ref_count: i64,
    ready_cond: SharedDatabaseCondition,
    extension_cond: Option<SharedDatabaseCondition>,
) -> Result<(), ReconcileError> {
    let backing = backing_for_write(backing, sd);
    let body = sd_status_apply_body_with_conditions(
        name,
        ready,
        &backing,
        ref_count,
        ready_cond,
        extension_cond,
    );
    let api: Api<SharedDatabase> = Api::namespaced(ctx.client.clone(), ns);
    api.patch_status(name, &apply_params(), &Patch::Apply(&body))
        .await?;
    Ok(())
}

async fn set_finalizers(
    client: &Client,
    ns: &str,
    name: &str,
    list: Vec<String>,
) -> Result<(), ReconcileError> {
    let api: Api<SharedDatabase> = Api::namespaced(client.clone(), ns);
    let patch = json!({ "metadata": { "finalizers": list } });
    api.patch(name, &PatchParams::default(), &Patch::Merge(&patch))
        .await?;
    Ok(())
}

fn with_finalizer(current: &[String]) -> Vec<String> {
    let mut out = current.to_vec();
    if !out.iter().any(|f| f == SD_FINALIZER) {
        out.push(SD_FINALIZER.to_string());
    }
    out
}

fn without_finalizer(current: &[String]) -> Vec<String> {
    current
        .iter()
        .filter(|f| *f != SD_FINALIZER)
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let body = sd_status_apply_body(
            "orders",
            true,
            &Backing::pg("shd_apps_orders", "platform-postgres"),
            2,
        );
        assert_eq!(body["status"]["ready"], json!(true));
        assert_eq!(body["status"]["refCount"], json!(2));
        assert_eq!(body["status"]["database"], json!("shd_apps_orders"));
        // The CLUSTER rides in `instance` for pg too. The reaper vetoes on an
        // exact instance-name match, and before this field was recorded a
        // cluster whose only occupant was a shared database read as empty and
        // went on a dwell to be deleted with the data underneath it.
        assert_eq!(body["status"]["instance"], json!("platform-postgres"));
        // `dbnum` stays redis-only: there is no logical database number in
        // Postgres, and inventing one would put a meaningless integer in the
        // same field the allocator reads.
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
            &Backing::pg("shd_apps_orders", "platform-postgres"),
            0,
            ready_condition("False", "ExtensionUnavailable", "missing", &[]),
            Some(ext),
        );
        let conds = body["status"]["conditions"].as_array().expect("array");
        assert_eq!(conds.len(), 2);
        assert_eq!(conds[0]["type"], json!(COND_READY));
        assert_eq!(conds[1]["type"], json!(COND_EXTENSION_UNAVAILABLE));
    }

    /// A `SharedDatabase` that already reports a provisioned backing.
    fn provisioned(database: &str) -> SharedDatabase {
        let mut sd = SharedDatabase::new(
            "orders",
            operator_core::SharedDatabaseSpec {
                type_: "pg".into(),
                ..Default::default()
            },
        );
        sd.status = Some(operator_core::SharedDatabaseStatus {
            ready: Some(true),
            ref_count: Some(2),
            database: Some(database.into()),
            instance: Some("platform-postgres".into()),
            ..Default::default()
        });
        sd
    }

    #[test]
    fn a_write_with_no_override_carries_the_backing_forward() {
        // THE regression, tested at the function that DECIDES rather than at
        // the body builder. A first version of this test asserted the composed
        // body instead, and passed happily when the default was mutated back
        // to an empty backing — it was pinning the builder, not the choice.
        let sd = provisioned("shd_shop_orders");
        assert_eq!(
            backing_for_write(None, &sd),
            Backing::pg("shd_shop_orders", "platform-postgres")
        );
    }

    #[test]
    fn an_explicit_override_wins_over_what_the_object_reports() {
        // The provisioning paths state their backing, and must be able to
        // change it — a redis database that moved instance says so.
        let sd = provisioned("shd_shop_orders");
        let fresh = Backing::redis("platform-redis-ephemeral-000", 4);
        assert_eq!(backing_for_write(Some(&fresh), &sd), fresh);
    }

    #[test]
    fn a_failure_path_status_write_keeps_the_backing_it_found() {
        // THE regression. SSA replaces this manager's field-set on each apply,
        // so a body that omits `status.database` deletes it. An earlier
        // version defaulted the backing on every failure path, which meant a
        // CNPG hiccup erased the database name from a provisioned object — and
        // a delete arriving during the outage then read the absence as "never
        // provisioned" and skipped dropping the database.
        let sd = provisioned("shd_shop_orders");
        let body = sd_status_apply_body_with_conditions(
            "orders",
            false,
            &backing_of(&sd),
            2,
            ready_condition("False", "AwaitingCluster", "not answering", &[]),
            None,
        );
        assert_eq!(body["status"]["database"], json!("shd_shop_orders"));
        assert_eq!(body["status"]["ready"], json!(false));
    }

    #[test]
    fn an_unprovisioned_object_carries_forward_nothing() {
        let sd = SharedDatabase::new(
            "orders",
            operator_core::SharedDatabaseSpec {
                type_: "pg".into(),
                ..Default::default()
            },
        );
        assert_eq!(backing_of(&sd), Backing::default());
    }

    #[test]
    fn a_carried_extension_warning_is_not_dropped_by_a_path_that_did_not_reprobe() {
        // Same prune rule, one condition further in: a body carrying only
        // `Ready` deletes the `ExtensionUnavailable` a previous apply set, and
        // an operator watching for it sees it blink out with nothing having
        // changed.
        let prior = vec![SharedDatabaseCondition {
            type_: COND_EXTENSION_UNAVAILABLE.into(),
            status: "True".into(),
            last_transition_time: "2026-01-01T00:00:00+00:00".into(),
            reason: Some("NotInOperandImage".into()),
            message: Some("the running PostgreSQL image does not provide: vector".into()),
        }];
        let carried = extension_condition_of(&prior).expect("carried forward");
        assert_eq!(carried.type_, COND_EXTENSION_UNAVAILABLE);
        assert_eq!(carried.last_transition_time, "2026-01-01T00:00:00+00:00");
        // ...and a condition that had CLEARED is not resurrected.
        let cleared = vec![SharedDatabaseCondition {
            status: "False".into(),
            ..prior[0].clone()
        }];
        assert!(extension_condition_of(&cleared).is_none());
    }

    #[test]
    fn no_extension_condition_when_nothing_is_missing() {
        assert!(extension_unavailable_condition(&[], &[]).is_none());
    }
}
