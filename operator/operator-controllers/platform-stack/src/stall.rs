// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! `ReconcileStalled` on `PlatformStack/default`: the condition that says the
//! last reconcile was abandoned at its deadline (WI-400).
//!
//! The pass that would have reported the cut is the one that did not finish,
//! so the condition has a writer of its own, [`STALL_FIELD_MANAGER`], beside
//! `platform-controller`. Every apply under it carries that one condition and
//! nothing else, and `status.conditions` is a list-map keyed by `type`
//! (`schemas/crdmeta/meta.cue`), so the apiserver merges it by key: the apply
//! adds or replaces `ReconcileStalled` and leaves every other condition where
//! it is. [`mark`] sets it when a pass is cut; [`clear`] removes it after the
//! next pass that finishes, by applying an EMPTY list: the manager then owns
//! nothing, and an entry no other manager owns goes with it.
//!
//! Two guards keep every apply the stall manager makes inside the transitions
//! `e2e/platformstack-listmap-upgrade-proof.sh` measures on a real apiserver.
//!
//! - **The served CRD.** Both [`mark`] and [`clear`] read the PlatformStack
//!   CRD first and write nothing unless it makes the list a list-map
//!   ([`crd_merges_conditions_by_type`]). A rollback re-applies the older,
//!   ATOMIC CRD while this operator can still hold the lease, and the switch
//!   leaves every by-key `managedFields` entry in place: an apply of
//!   `[ReconcileStalled]` would then REPLACE every other condition.
//! - **The ownership on the stack.** [`mark`] writes beside any ownership
//!   but one ([`mark_may_write`]). [`clear`], whose adopt-then-release
//!   relies on `platform-controller` holding its own conditions by key,
//!   waits until it does ([`controller_owns_conditions_by_key`]). A stack
//!   last written while the list was atomic is owned whole until
//!   `platform-controller`'s first apply under the list-map CRD, which
//!   `reconcile::write_status_if_changed` makes on the first pass that
//!   reaches it ([`controller_must_reapply`]).

use std::time::Duration;

use k8s_openapi::apiextensions_apiserver::pkg::apis::apiextensions::v1::CustomResourceDefinition;
use kube::api::{Api, Patch, PatchParams};
use kube::{Client, ResourceExt};
use operator_core::{PlatformStack, PlatformStackCondition, PlatformStackStatus};
use serde_json::{json, Value};
use tracing::{debug, info, warn};

use crate::status::{condition, COND_RECONCILE_STALLED};
use crate::{FIELD_MANAGER, SINGLETON_NAME, SINGLETON_NAMESPACE};

/// The SSA field manager that owns `ReconcileStalled`, and nothing else.
pub const STALL_FIELD_MANAGER: &str = "apprafter-reconcile-deadline";

/// How long [`mark`] or [`clear`] may take, the CRD read and every apply
/// included. Both run after the pass and outside its deadline, so a pass
/// holds its slot for at most `RECONCILE_DEADLINE + STALL_WRITE_BUDGET`.
/// Against an apiserver that answers nothing the write is given up and
/// logged; the next pass tries again.
pub const STALL_WRITE_BUDGET: Duration = Duration::from_secs(10);

/// The condition's `reason`: the same word as the Warning Event a cut
/// publishes (`reconcile::publish_deadline_event`).
pub const REASON: &str = "ReconcileTimedOut";

/// The PlatformStack CRD, read before every write ([`crd_merges_conditions_by_type`]).
const CRD_NAME: &str = "platformstacks.apprafter.io";

/// `ReconcileStalled`'s key in a `managedFields` entry's `fieldsV1`.
const STALLED_KEY: &str = r#"k:{"type":"ReconcileStalled"}"#;

/// The `managedFields` entries for the status, each with its `f:conditions`.
/// `{}` is an ownership of the whole list, which an apply made while the list
/// was atomic leaves behind; otherwise the keys are the conditions owned.
fn status_owners(
    stack: &PlatformStack,
) -> impl Iterator<Item = (&str, &serde_json::Map<String, Value>)> {
    stack
        .metadata
        .managed_fields
        .iter()
        .flatten()
        .filter(|e| e.subresource.as_deref() == Some("status"))
        .filter_map(|e| {
            let owned = e
                .fields_v1
                .as_ref()?
                .0
                .pointer("/f:status/f:conditions")?
                .as_object()?;
            Some((e.manager.as_deref()?, owned))
        })
}

/// `manager`'s `f:conditions` on the status, if it owns any.
fn owned_conditions<'a>(
    stack: &'a PlatformStack,
    manager: &str,
) -> Option<&'a serde_json::Map<String, Value>> {
    status_owners(stack).find_map(|(m, owned)| (m == manager).then_some(owned))
}

/// Whether `manager` owns `status.conditions` WHOLE (`f:conditions: {}`): the
/// ownership an apply made while the list was atomic leaves behind. An empty
/// apply under the list-map leaves no `f:conditions` at all, so `{}` comes
/// only from the atomic CRD.
fn owns_list_whole(stack: &PlatformStack, manager: &str) -> bool {
    owned_conditions(stack, manager).is_some_and(|owned| owned.is_empty())
}

/// Whether `platform-controller` owns `status.conditions` whole: a stack last
/// written under the atomic CRD. Its next apply converts that ownership to
/// one by key; until then, a condition its apply leaves out is not removed
/// but stays, owned by nobody (`reconcile::write_status_if_changed`).
pub fn controller_owns_list_whole(stack: &PlatformStack) -> bool {
    owns_list_whole(stack, FIELD_MANAGER)
}

/// Whether `platform-controller` must first re-apply the status as it read
/// it, before it writes `new`. From either state below, the write of `new`
/// would keep a condition it leaves out, owned by nobody, for good (measured
/// on kind); after the re-apply `platform-controller` owns every condition by
/// key, and the write of `new` removes what it leaves out.
///
/// - It owns the list whole: a stack last written under the atomic CRD.
/// - It has written the stack by key, and `new` leaves out a condition that
///   no manager holds, by key or as a whole list: what an older operator's
///   apply by key leaves behind when it runs under the list-map CRD between
///   the CRD's sync wave and its own pod being replaced. A condition another
///   manager holds is never adopted: the re-apply would only share it, and
///   repeated on every pass would be a write loop. `ReconcileStalled` is the
///   stall manager's to remove.
pub fn controller_must_adopt_first(stack: &PlatformStack, new: &PlatformStackStatus) -> bool {
    if controller_owns_list_whole(stack) {
        return true;
    }
    if owned_conditions(stack, FIELD_MANAGER).is_none()
        || status_owners(stack).any(|(_, owned)| owned.is_empty())
    {
        return false;
    }
    let kept: Vec<&str> = new
        .conditions
        .iter()
        .flatten()
        .map(|c| c.type_.as_str())
        .collect();
    let held = |t: &str| {
        let key = format!(r#"k:{{"type":"{t}"}}"#);
        status_owners(stack).any(|(_, owned)| owned.contains_key(&key))
    };
    stack
        .status
        .iter()
        .flat_map(|s| s.conditions.iter().flatten())
        .map(|c| c.type_.as_str())
        .filter(|t| *t != COND_RECONCILE_STALLED && !kept.contains(t))
        .any(|t| !held(t))
}

/// Whether `platform-controller` owns its conditions one by one, and not
/// `ReconcileStalled`: the only ownership under which [`clear`] writes. Owned
/// whole, not owned at all, or co-owning `ReconcileStalled` (an older operator
/// copies every condition it reads into its own apply) are states in which
/// clear's adopt-then-release was not measured, or could not remove the
/// condition.
pub fn controller_owns_conditions_by_key(stack: &PlatformStack) -> bool {
    owned_conditions(stack, FIELD_MANAGER).is_some_and(|owned| {
        owned.keys().any(|k| k.starts_with("k:")) && !owned.contains_key(STALLED_KEY)
    })
}

/// Whether [`mark`] may write on this stack's ownership: always, except while
/// the stall manager owns the list WHOLE and `platform-controller` does not
/// own its conditions by key. A rollback while stalled, an older operator's
/// write that carried the condition as it was, and a re-upgrade leave that
/// state until `platform-controller` applies again. Letting go of the
/// whole-list ownership there is an empty apply that was not measured, and
/// the `ReconcileStalled` the older operator carried is on the stack already.
/// Beside a list `platform-controller` owns whole, or no ownership at all,
/// the apply adds the condition and keeps every other one (measured).
pub fn mark_may_write(stack: &PlatformStack) -> bool {
    !(owns_list_whole(stack, STALL_FIELD_MANAGER) && !controller_owns_conditions_by_key(stack))
}

/// Whether `platform-controller` must apply its conditions although they did
/// not change: it owns the list whole, or it co-owns `ReconcileStalled`. Only
/// its own apply puts either right, and after it it owns each of its
/// conditions by key and not `ReconcileStalled`, so this asks for one write.
pub fn controller_must_reapply(stack: &PlatformStack) -> bool {
    owned_conditions(stack, FIELD_MANAGER)
        .is_some_and(|owned| owned.is_empty() || owned.contains_key(STALLED_KEY))
}

/// Managers other than the stall manager that keep `ReconcileStalled` on the
/// stack: one that owns it by key, or one that owns the whole list. The stall
/// manager adopts the condition only when there is none, because an adopt and
/// release beside such a holder would remove nothing, and repeated on every
/// pass would be a write loop.
fn other_holders(stack: &PlatformStack) -> Vec<String> {
    status_owners(stack)
        .filter(|(m, _)| *m != STALL_FIELD_MANAGER)
        .filter(|(_, owned)| owned.is_empty() || owned.contains_key(STALLED_KEY))
        .map(|(m, _)| m.to_string())
        .collect()
}

/// The `ReconcileStalled` condition on the stack, if there is one.
fn stalled(stack: &PlatformStack) -> Option<&PlatformStackCondition> {
    stack
        .status
        .as_ref()?
        .conditions
        .as_ref()?
        .iter()
        .find(|c| c.type_ == COND_RECONCILE_STALLED)
}

/// Whether the PlatformStack CRD the apiserver serves makes
/// `status.conditions` a list-map. Read before every write rather than once:
/// Argo CD re-applies the CRD at sync-wave -5 on a rollback, before this
/// operator's pod is replaced. A read that fails is an `Err`, and the caller
/// writes nothing.
async fn crd_merges_conditions_by_type(client: &Client) -> Result<bool, kube::Error> {
    let api: Api<CustomResourceDefinition> = Api::all(client.clone());
    Ok(merges_conditions_by_type(&api.get(CRD_NAME).await?))
}

/// The reading [`crd_merges_conditions_by_type`] makes of one CRD:
/// `x-kubernetes-list-type: map` on `status.conditions` in `v1alpha1`.
fn merges_conditions_by_type(crd: &CustomResourceDefinition) -> bool {
    crd.spec
        .versions
        .iter()
        .find(|v| v.name == "v1alpha1")
        .and_then(|v| v.schema.as_ref()?.open_api_v3_schema.as_ref())
        .and_then(|s| s.properties.as_ref()?.get("status"))
        .and_then(|s| s.properties.as_ref()?.get("conditions"))
        .is_some_and(|c| c.x_kubernetes_list_type.as_deref() == Some("map"))
}

/// One apply under [`STALL_FIELD_MANAGER`] whose status carries `conditions`
/// and nothing else.
async fn apply(
    api: &Api<PlatformStack>,
    name: &str,
    conditions: Vec<PlatformStackCondition>,
) -> Result<PlatformStack, kube::Error> {
    let body = json!({
        "apiVersion": "apprafter.io/v1alpha1",
        "kind": "PlatformStack",
        "metadata": { "name": name },
        "status": { "conditions": conditions },
    });
    api.patch_status(
        name,
        &PatchParams::apply(STALL_FIELD_MANAGER).force(),
        &Patch::Apply(&body),
    )
    .await
}

/// Set `ReconcileStalled=True` on a stack whose reconcile was cut at `after`.
///
/// `lastTransitionTime` is carried over from a `ReconcileStalled=True`
/// already there, so a stall that goes on re-applies an identical condition,
/// which the apiserver stores as no change: no new resourceVersion, and no
/// watch event to start another pass.
pub async fn mark(client: &Client, stack: &PlatformStack, after: Duration) {
    if stack.name_any() != SINGLETON_NAME {
        return;
    }
    if !mark_may_write(stack) {
        warn!(
            field_manager = STALL_FIELD_MANAGER,
            "ReconcileStalled not set: the stack still carries the one an older release \
             left, owned whole, until platform-controller applies its conditions again; the \
             ReconcileTimedOut Event reports this cut"
        );
        return;
    }
    let prior = stack
        .status
        .as_ref()
        .and_then(|s| s.conditions.clone())
        .unwrap_or_default();
    let stalled = condition(
        COND_RECONCILE_STALLED,
        "True",
        REASON,
        &format!(
            "the last reconcile did not finish within {}s and was abandoned; the other \
             conditions are from the last reconcile that finished",
            after.as_secs()
        ),
        &prior,
    );
    let api: Api<PlatformStack> = Api::namespaced(client.clone(), SINGLETON_NAMESPACE);
    let name = stack.name_any();
    let write = async {
        if !crd_merges_conditions_by_type(client).await? {
            return Ok(false);
        }
        // An ownership of the whole list left from before the list-map CRD
        // (a rollback while stalled leaves one) is let go first, by the empty
        // apply measured to remove nothing while platform-controller owns its
        // conditions by key — the only state `mark_may_write` lets reach here
        // with it.
        if owns_list_whole(stack, STALL_FIELD_MANAGER) {
            apply(&api, &name, Vec::new()).await?;
        }
        apply(&api, &name, vec![stalled]).await.map(|_| true)
    };
    match tokio::time::timeout(STALL_WRITE_BUDGET, write).await {
        Ok(Ok(true)) => info!(
            field_manager = STALL_FIELD_MANAGER,
            "ReconcileStalled=True set on PlatformStack/default"
        ),
        Ok(Ok(false)) => warn!(
            crd = CRD_NAME,
            "ReconcileStalled not set: the served CRD keeps status.conditions atomic (rolled \
             back to an older release?), so the apply would replace every other condition; \
             the ReconcileTimedOut Event reports this cut"
        ),
        Ok(Err(e)) => warn!(error = %e, "could not set ReconcileStalled (continuing)"),
        Err(_) => warn!(
            budget_secs = STALL_WRITE_BUDGET.as_secs(),
            "setting ReconcileStalled did not answer in time (continuing)"
        ),
    }
}

/// What [`clear`] did.
enum Cleared {
    /// The condition is gone.
    Removed,
    /// Other managers keep it, named here.
    Held(Vec<String>),
    /// Nothing written: the served CRD keeps the list atomic.
    AtomicCrd,
}

/// Remove `ReconcileStalled` after a pass that finished, if the stack it read
/// carries one.
pub async fn clear(client: &Client, stack: &PlatformStack) {
    if stack.name_any() != SINGLETON_NAME || stalled(stack).is_none() {
        return;
    }
    if !controller_owns_conditions_by_key(stack) {
        debug!(
            "ReconcileStalled kept for now: platform-controller has not applied its \
             conditions by key yet; the pass after its next write removes it"
        );
        return;
    }
    let api: Api<PlatformStack> = Api::namespaced(client.clone(), SINGLETON_NAMESPACE);
    match tokio::time::timeout(STALL_WRITE_BUDGET, remove(client, &api, &stack.name_any())).await {
        Ok(Ok(Cleared::Removed)) => info!(
            field_manager = STALL_FIELD_MANAGER,
            "ReconcileStalled removed: a reconcile finished"
        ),
        Ok(Ok(Cleared::Held(holders))) => warn!(
            ?holders,
            "ReconcileStalled kept: another field manager holds it, and it stays until that \
             manager lets it go"
        ),
        Ok(Ok(Cleared::AtomicCrd)) => warn!(
            crd = CRD_NAME,
            "ReconcileStalled kept: the served CRD keeps status.conditions atomic (rolled back \
             to an older release?), so an apply would replace every other condition"
        ),
        Ok(Err(e)) => warn!(error = %e, "could not remove ReconcileStalled (continuing)"),
        Err(_) => warn!(
            budget_secs = STALL_WRITE_BUDGET.as_secs(),
            "removing ReconcileStalled did not answer in time (continuing)"
        ),
    }
}

/// Let go of `ReconcileStalled`, and adopt-then-let-go of one left behind
/// that nobody owns, once the served CRD is known to merge by key.
async fn remove(
    client: &Client,
    api: &Api<PlatformStack>,
    name: &str,
) -> Result<Cleared, kube::Error> {
    if !crd_merges_conditions_by_type(client).await? {
        return Ok(Cleared::AtomicCrd);
    }
    let released = apply(api, name, Vec::new()).await?;
    let Some(left) = stalled(&released).cloned() else {
        return Ok(Cleared::Removed);
    };
    let holders = other_holders(&released);
    if !holders.is_empty() {
        return Ok(Cleared::Held(holders));
    }
    // Owned by nobody: an older operator, rolled back to while the stack was
    // stalled, copied the condition into its own apply of the whole list,
    // and the newer one's apply by key then let go of it. Applying it
    // unchanged makes it this manager's, and the empty apply removes it.
    apply(api, name, vec![left]).await?;
    let released = apply(api, name, Vec::new()).await?;
    Ok(match stalled(&released) {
        None => Cleared::Removed,
        Some(_) => Cleared::Held(other_holders(&released)),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn by_key(types: &[&str]) -> Value {
        Value::Object(
            types
                .iter()
                .map(|t| (format!(r#"k:{{"type":"{t}"}}"#), json!({ ".": {} })))
                .collect(),
        )
    }

    fn status_entry(manager: &str, conditions: Value) -> Value {
        json!({ "manager": manager, "operation": "Apply", "subresource": "status",
                "apiVersion": "apprafter.io/v1alpha1", "fieldsType": "FieldsV1",
                "fieldsV1": { "f:status": { "f:conditions": conditions } } })
    }

    fn stack(managed: Vec<Value>) -> PlatformStack {
        serde_json::from_value(json!({
            "apiVersion": "apprafter.io/v1alpha1", "kind": "PlatformStack",
            "metadata": { "name": "default", "namespace": "apprafter-system",
                          "managedFields": managed },
            "spec": { "channel": "stable",
                      "source": { "upstream": "oci://ghcr.io/apprafter/platform-stack",
                                  "repoURL": "oci://ghcr.io/apprafter/platform-stack",
                                  "checkInterval": "6h" },
                      "values": { "tier": 1 } },
        }))
        .expect("stack")
    }

    #[test]
    fn clear_writes_only_while_the_controller_owns_its_conditions_by_key() {
        let owned = |managed: Vec<Value>| controller_owns_conditions_by_key(&stack(managed));
        assert!(owned(vec![status_entry(
            FIELD_MANAGER,
            by_key(&["Ready", "Synced"])
        )]));
        assert!(owned(vec![
            status_entry(FIELD_MANAGER, by_key(&["Ready"])),
            status_entry(STALL_FIELD_MANAGER, by_key(&["ReconcileStalled"])),
        ]));
        assert!(
            !owned(vec![status_entry(FIELD_MANAGER, json!({}))]),
            "whole"
        );
        assert!(
            !owned(vec![status_entry(
                FIELD_MANAGER,
                by_key(&["Ready", "ReconcileStalled"])
            )]),
            "co-owning the stall"
        );
        assert!(!owned(vec![]), "not at all");
        assert!(
            !owned(vec![status_entry(STALL_FIELD_MANAGER, by_key(&["Ready"]))]),
            "another manager's keys are not the controller's"
        );
        let mut spec_entry = status_entry(FIELD_MANAGER, by_key(&["Ready"]));
        spec_entry.as_object_mut().unwrap().remove("subresource");
        assert!(!owned(vec![spec_entry]), "only the status entry counts");
    }

    /// Beside every ownership measured (a fresh stack, a list
    /// `platform-controller` owns whole or by key), `mark` writes; with the
    /// stall manager owning the list whole, only once `platform-controller`
    /// owns its conditions by key.
    #[test]
    fn mark_writes_beside_any_ownership_but_a_whole_list_of_its_own_without_keys() {
        let may = |managed: Vec<Value>| mark_may_write(&stack(managed));
        assert!(may(vec![]), "a fresh stack");
        assert!(
            may(vec![status_entry(FIELD_MANAGER, json!({}))]),
            "beside a list the controller owns whole"
        );
        assert!(may(vec![
            status_entry(FIELD_MANAGER, json!({})),
            status_entry(STALL_FIELD_MANAGER, by_key(&["ReconcileStalled"])),
        ]));
        assert!(may(vec![
            status_entry(FIELD_MANAGER, by_key(&["Ready"])),
            status_entry(STALL_FIELD_MANAGER, json!({})),
        ]));
        assert!(
            !may(vec![
                status_entry(FIELD_MANAGER, json!({})),
                status_entry(STALL_FIELD_MANAGER, json!({})),
            ]),
            "both whole: what a rollback while stalled and a re-upgrade leave"
        );
        assert!(
            !may(vec![status_entry(STALL_FIELD_MANAGER, json!({}))]),
            "its own whole list, the controller owning none"
        );
        assert!(
            !may(vec![
                status_entry(FIELD_MANAGER, by_key(&["Ready", "ReconcileStalled"])),
                status_entry(STALL_FIELD_MANAGER, json!({})),
            ]),
            "the controller co-owning the stall"
        );
    }

    #[test]
    fn the_controller_reapplies_a_list_it_owns_whole_or_one_that_carries_the_stall() {
        let must = |conditions: Value| {
            controller_must_reapply(&stack(vec![status_entry(FIELD_MANAGER, conditions)]))
        };
        assert!(must(json!({})));
        assert!(must(by_key(&["Ready", "ReconcileStalled"])));
        assert!(!must(by_key(&["Ready", "Synced"])));
        assert!(!controller_must_reapply(&stack(vec![])));
        let whole = |conditions: Value| {
            controller_owns_list_whole(&stack(vec![status_entry(FIELD_MANAGER, conditions)]))
        };
        assert!(whole(json!({})));
        assert!(!whole(by_key(&["Ready", "ReconcileStalled"])));
        assert!(!controller_owns_list_whole(&stack(vec![status_entry(
            STALL_FIELD_MANAGER,
            json!({})
        )])));
    }

    /// `stack(managed)` carrying conditions of `types`, and the status a
    /// write would send with `new` of them.
    fn carrying(
        managed: Vec<Value>,
        types: &[&str],
        new: &[&str],
    ) -> (PlatformStack, PlatformStackStatus) {
        let conditions = |ts: &[&str]| -> Vec<PlatformStackCondition> {
            ts.iter()
                .map(|t| PlatformStackCondition {
                    type_: (*t).into(),
                    status: "True".into(),
                    reason: None,
                    message: None,
                    last_transition_time: "t".into(),
                })
                .collect()
        };
        let mut stack = stack(managed);
        stack.status = Some(PlatformStackStatus {
            conditions: Some(conditions(types)),
            ..PlatformStackStatus::default()
        });
        let new = PlatformStackStatus {
            conditions: Some(conditions(new)),
            ..PlatformStackStatus::default()
        };
        (stack, new)
    }

    #[test]
    fn the_controller_adopts_first_only_what_its_write_would_orphan() {
        let must = |managed: Vec<Value>, types: &[&str], new: &[&str]| {
            let (stack, new) = carrying(managed, types, new);
            controller_must_adopt_first(&stack, &new)
        };
        let all = ["Ready", "BackupHealthy"];
        assert!(
            must(vec![status_entry(FIELD_MANAGER, json!({}))], &all, &all),
            "owned whole, even with nothing left out"
        );
        assert!(
            must(
                vec![status_entry(FIELD_MANAGER, by_key(&["Ready"]))],
                &all,
                &["Ready"]
            ),
            "a condition nobody holds, left out"
        );
        assert!(
            !must(
                vec![status_entry(FIELD_MANAGER, by_key(&["Ready"]))],
                &all,
                &all
            ),
            "one nobody holds, still sent: the write takes it"
        );
        assert!(
            !must(
                vec![status_entry(FIELD_MANAGER, by_key(&all))],
                &all,
                &["Ready"]
            ),
            "left out but owned by key: the write removes it"
        );
        assert!(
            !must(
                vec![
                    status_entry(FIELD_MANAGER, by_key(&["Ready"])),
                    status_entry("kubectl-edit", by_key(&["BackupHealthy"])),
                ],
                &all,
                &["Ready"]
            ),
            "another manager holds it by key"
        );
        assert!(
            !must(
                vec![
                    status_entry(FIELD_MANAGER, by_key(&["Ready"])),
                    status_entry(STALL_FIELD_MANAGER, json!({})),
                ],
                &all,
                &["Ready"]
            ),
            "another manager holds the whole list"
        );
        assert!(
            !must(
                vec![status_entry(FIELD_MANAGER, by_key(&["Ready"]))],
                &["Ready", "ReconcileStalled"],
                &["Ready"]
            ),
            "ReconcileStalled is the stall manager's"
        );
        assert!(
            !must(vec![], &all, &["Ready"]),
            "a stack platform-controller has not written"
        );
    }

    #[test]
    fn the_managers_that_keep_the_stall_besides_its_own_are_named() {
        let holders = other_holders(&stack(vec![
            status_entry(FIELD_MANAGER, by_key(&["Ready"])),
            status_entry(STALL_FIELD_MANAGER, by_key(&["ReconcileStalled"])),
            status_entry("kubectl-edit", by_key(&["ReconcileStalled"])),
            status_entry("kubectl-patch", json!({})),
        ]));
        assert_eq!(holders, vec!["kubectl-edit", "kubectl-patch"]);
        assert!(other_holders(&stack(vec![
            status_entry(FIELD_MANAGER, by_key(&["Ready"])),
            status_entry(STALL_FIELD_MANAGER, json!({})),
        ]))
        .is_empty());
    }

    /// The reading of the served CRD, on the CRD this chart ships: the
    /// list-map it is, and the atomic list it was before WI-400 (the CRD a
    /// rollback serves).
    #[test]
    fn the_shipped_crd_merges_conditions_by_type_and_the_atomic_one_does_not() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../charts/apprafter-operator/templates/crd-platformstack.yaml"
        );
        let shipped: String = std::fs::read_to_string(path)
            .unwrap_or_else(|e| panic!("{path}: {e}"))
            .lines()
            .filter(|l| !l.contains("{{"))
            .map(|l| format!("{l}\n"))
            .collect();
        let crd = |yaml: &str| -> CustomResourceDefinition {
            serde_yaml::from_str(yaml).expect("the CRD parses")
        };
        assert!(merges_conditions_by_type(&crd(&shipped)), "{path}");
        let markers = "                x-kubernetes-list-map-keys:\n                - type\n                x-kubernetes-list-type: map\n";
        assert_eq!(shipped.matches(markers).count(), 1, "{path}");
        assert!(!merges_conditions_by_type(&crd(
            &shipped.replace(markers, "")
        )));
    }
}
