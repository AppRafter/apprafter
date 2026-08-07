// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Scope-agnostic migration state machine (2.16b-sc).
//!
//! Hoisted verbatim from the Application controller (2.16b Task 10/11) so a
//! SECOND controller (SourceCredential) can call the identical copy. The
//! machine is a pure function of "did we detect a destructive change this
//! reconcile?" × the live `PlanState` bucket, over the shared
//! [`MigrationPlan`] / [`DestructiveChange`] types — nothing app-specific
//! leaks in, so every scope that gates edits behind a MigrationPlan shares
//! one drift-free decision table.

use tracing::warn;

use crate::migration::change_hash;
use crate::{DestructiveChange, MigrationPlan};

/// Is this plan still gating (not completed/rejected)? Phase-only bucketing
/// used by [`plan_state`] to tell a live gate from a terminal relic.
fn plan_is_blocking(plan: &MigrationPlan) -> bool {
    let phase = plan
        .status
        .as_ref()
        .and_then(|s| s.phase.as_deref())
        .unwrap_or("pending-approval");
    !matches!(phase, "completed" | "rejected")
}

/// 2.16b Task 10 (R2-H2): the finer bucket a reconcile needs to
/// pick a decision. Coarser `plan_is_blocking` only answers
/// "does this pause the edit"; the state machine also needs to
/// know whether a live/terminal plan MATCHES the current change.
#[derive(Debug, PartialEq)]
pub enum PlanState {
    /// No plan exists for this key.
    None,
    /// A blocking (not completed/rejected) plan whose trigger
    /// `(type, field)` equals the current change's — the edit is
    /// legitimately paused on THIS change; leave it be.
    BlockingMatch,
    /// A blocking plan whose trigger is for a DIFFERENT change —
    /// stale gate; supersede it with a fresh plan.
    BlockingMismatch,
    /// Phase `failed` — needs operator/user action; keep gating.
    Failed,
    /// Phase `completed` AND trigger matches the current change —
    /// the migration ran, so the render may now consume + apply.
    CompletedMatch,
    /// Any other terminal/stale plan (completed-mismatch,
    /// rejected, unknown phase) — a relic to clean up.
    Relic,
}

/// 2.16b Task 10 (R2-H2 / R3-M1): the reconcile decision for one
/// gated object, as a pure function of "did we detect a
/// destructive change this reconcile?" × the live `PlanState`.
/// Total over the (bool × PlanState) product so async
/// wiring can never fall through an unhandled cell.
#[derive(Debug, PartialEq)]
pub enum MigrationDecision {
    /// No change + no plan → render children normally.
    Render,
    /// Change detected + no plan → create the gating plan.
    CreatePlan,
    /// Change detected + a matching blocking plan already gates →
    /// nothing to do; stay paused.
    NoOp,
    /// Change detected but the blocking/relic plan is for a
    /// different change → delete it, then create the right plan.
    DeleteThenCreate,
    /// Change detected + its plan already completed → consume the
    /// migration result and apply children.
    ConsumeApply,
    /// No change but a plan lingers (any state) → delete the stale
    /// plan, then render normally.
    DeleteThenRender,
    /// Change detected + its plan is `failed` → keep gating, do
    /// not silently re-plan; surface the failure.
    BlockFailed,
}

/// Pure decision table (2.16b spec state-machine section).
/// See `MigrationDecision` for what each arm means.
pub fn decide(has_change: bool, state: PlanState) -> MigrationDecision {
    use MigrationDecision::*;
    match (has_change, state) {
        // No destructive change this reconcile.
        (false, PlanState::None) => Render,
        // Any lingering plan (blocking/terminal/relic) with no
        // current change → supersede/cleanup, then render.
        (false, _) => DeleteThenRender,
        // Destructive change detected.
        (true, PlanState::None) => CreatePlan,
        (true, PlanState::BlockingMatch) => NoOp,
        (true, PlanState::BlockingMismatch) => DeleteThenCreate,
        (true, PlanState::Failed) => BlockFailed,
        (true, PlanState::CompletedMatch) => ConsumeApply,
        (true, PlanState::Relic) => DeleteThenCreate,
    }
}

/// Bucket a plan (if any) against the current change into a
/// `PlanState`. Blocking/terminal is decided by phase (via
/// `plan_is_blocking`); the trigger-TUPLE "match" compares the plan's
/// trigger `(type, field)` to the current PRIMARY change's `(trigger_type,
/// field)` — the two-tuple that identifies WHICH destructive change a plan
/// was cut for.
///
/// 2.16b S1.2 / S-4: the consume-time CONTENT-hash match is computed over
/// the FULL set of CURRENT destructive candidates (`current_changes`), NOT
/// just the primary. Hashing only the primary would let an attacker attach a
/// lower-severity destructive op that rides along UNHASHED — the plan was
/// cut (and its approval hash stamped) over the whole set at creation, so
/// consume must re-derive the hash over the whole current set too. `current`
/// is the primary (its `(type, field)` is the trigger-tuple match);
/// `current_changes` is every candidate this reconcile detected.
pub fn plan_state(
    plan: Option<&MigrationPlan>,
    current: &DestructiveChange,
    current_changes: &[DestructiveChange],
) -> PlanState {
    let Some(plan) = plan else {
        return PlanState::None;
    };
    let phase = plan
        .status
        .as_ref()
        .and_then(|s| s.phase.as_deref())
        .unwrap_or("pending-approval");
    let trigger_matches =
        plan.spec.trigger.type_ == current.trigger_type && plan.spec.trigger.field == current.field;

    if phase == "failed" {
        return PlanState::Failed;
    }
    if plan_is_blocking(plan) {
        // Not completed/rejected/failed → live gate.
        return if trigger_matches {
            PlanState::BlockingMatch
        } else {
            PlanState::BlockingMismatch
        };
    }
    // Terminal (completed | rejected). A `completed` plan whose trigger
    // TUPLE `(type, field)` matches is a candidate to consume — but
    // 2.16b S-4 additionally requires the plan's stamped CONTENT hash to
    // match the CURRENT change set's hash. Without that, an approval for
    // `replicas 2->0` would consume against a DIFFERENT `replicas 1->0`
    // (same tuple, different payload), fully defeating the gate for a
    // security-boundary op. S1.2 widens the hash to the FULL candidate set:
    // if the approver signed off on {needs-removal + scale-to-zero} and the
    // spec now carries ONLY scale-to-zero (the needs-removal op dropped, or
    // a NEW lower-severity op was added), the full-set hash differs → no
    // match → re-gate. On a hash MISMATCH — including a MISSING/EMPTY stamped
    // hash — we demote the completed plan to a `Relic`, so `decide` yields
    // `DeleteThenCreate` and the edit is re-gated as a fresh
    // `pending-approval` plan. App-scope migration is brand-new (no legacy
    // hashless plans exist), so consume REQUIRES a non-empty stamped hash
    // that matches (`plan_hash_matches`); a hashless completed plan never
    // consumes a destructive change.
    if phase == "completed" && trigger_matches {
        let current_hash = change_hash(current_changes);
        if plan_hash_matches(plan, &current_hash) {
            return PlanState::CompletedMatch;
        }
        // Bucketed as a Relic. Distinguish WHY for observability: a
        // MISSING/EMPTY stamped hash is qualitatively different from a
        // content MISMATCH. A legitimate app-scope plan is ALWAYS stamped
        // with a non-empty `approvedSpecHash` at creation (S-4), so a
        // completed, trigger-matching plan with NO hash can only be a
        // forged object or a pre-2.16b-sec artifact — it must never
        // consume, and its presence is worth surfacing loudly.
        let hash_missing = plan
            .spec
            .trigger
            .approved_spec_hash
            .as_deref()
            .is_none_or(str::is_empty);
        if hash_missing {
            warn!(
                plan = plan.metadata.name.as_deref().unwrap_or("<unknown>"),
                "completed MigrationPlan has no approvedSpecHash — re-gating; \
                 a legitimate plan is always stamped, so this is a forged or \
                 pre-2.16b-sec plan"
            );
        }
        PlanState::Relic
    } else {
        PlanState::Relic
    }
}

/// 2.16b S-4: does the plan's stamped `approvedSpecHash` match the
/// current detected change's content hash? App-scope migration is
/// brand-new and unpushed — there are ZERO legacy app-scope plans — so a
/// missing/empty stamped hash is NOT trusted as a legacy approval; it is
/// treated as NO match, and the completed plan is demoted to a relic and
/// re-gated. A hashless completed plan (forged or otherwise) must never
/// consume a destructive change. Consume therefore requires
/// `Some(non_empty_hash)` that EQUALS `current_hash` — this is what makes
/// an app-scope approval non-transferable across a different spec edit
/// (the security fix): the approver signed off on ONE `from->to`, and
/// swapping the payload before consume now yields a different hash → no
/// match → re-gate.
fn plan_hash_matches(plan: &MigrationPlan, current_hash: &str) -> bool {
    match plan.spec.trigger.approved_spec_hash.as_deref() {
        Some(h) if !h.is_empty() => h == current_hash,
        // Missing OR empty hash → no match. App-scope consume REQUIRES a
        // non-empty stamped hash; a hashless completed plan must re-gate,
        // never apply a destructive change.
        _ => false,
    }
}

/// 2.16b Task 11: bucket a plan (if any) when NO destructive change was
/// detected this reconcile. `decide` ignores the finer `PlanState`
/// distinctions in the `has_change == false` rows — it only cares
/// "no plan" (`None` → `Render`) vs "some lingering plan" (`_` →
/// `DeleteThenRender`). This helper therefore maps "no plan" → `None`
/// and "any live/terminal/relic plan" → `Relic`, so the caller can feed
/// a single `PlanState` into `decide(false, state)` without needing a
/// `DestructiveChange` to compare triggers against (which it lacks when
/// detection returned `None`).
pub fn plan_state_no_change(plan: Option<&MigrationPlan>) -> PlanState {
    match plan {
        None => PlanState::None,
        Some(_) => PlanState::Relic,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::migration_plan::{
        MigrationApplicationRef, MigrationApplicationScope, MigrationPlanScope, MigrationPlanSpec,
        MigrationPlanStatus, MigrationTrigger,
    };

    // 2.16b Task 10 (R2-H2 / R3-M1): pure state-machine decision fn.
    // All 8 detect × plan-state cells (12 total incl. detect=None
    // buckets) must map exactly to the spec's decision table.
    #[test]
    fn state_machine_cells() {
        use MigrationDecision::*;
        // detect = None
        assert_eq!(decide(false, PlanState::None), Render);
        assert_eq!(decide(false, PlanState::BlockingMatch), DeleteThenRender);
        assert_eq!(decide(false, PlanState::BlockingMismatch), DeleteThenRender);
        assert_eq!(decide(false, PlanState::Failed), DeleteThenRender);
        assert_eq!(decide(false, PlanState::CompletedMatch), DeleteThenRender);
        assert_eq!(decide(false, PlanState::Relic), DeleteThenRender);
        // detect = Some
        assert_eq!(decide(true, PlanState::None), CreatePlan);
        assert_eq!(decide(true, PlanState::BlockingMatch), NoOp);
        assert_eq!(decide(true, PlanState::BlockingMismatch), DeleteThenCreate);
        assert_eq!(decide(true, PlanState::Failed), BlockFailed);
        assert_eq!(decide(true, PlanState::CompletedMatch), ConsumeApply);
        assert_eq!(decide(true, PlanState::Relic), DeleteThenCreate);
    }

    // Helper: a change whose (trigger_type, field) can be tuned to
    // match / not-match a plan's trigger for the `plan_state` bucketer.
    fn change(trigger_type: &str, field: &str) -> DestructiveChange {
        DestructiveChange {
            trigger_type: trigger_type.into(),
            field: field.into(),
            from: None,
            to: None,
            classification: "breaking".into(),
        }
    }

    // Helper: an app-scope plan carrying a specific (type, field)
    // trigger and phase.
    fn plan_with_trigger(trigger_type: &str, field: &str, phase: Option<&str>) -> MigrationPlan {
        let spec = MigrationPlanSpec {
            scope: MigrationPlanScope {
                type_: "application".into(),
                application: Some(MigrationApplicationScope {
                    ref_: MigrationApplicationRef {
                        name: "parser".into(),
                        namespace: "demo".into(),
                    },
                    environment: "prod".into(),
                }),
                platform: None,
                sourcecredential: None,
            },
            trigger: MigrationTrigger {
                type_: trigger_type.into(),
                field: field.into(),
                from: None,
                to: None,
                approved_spec_hash: None,
            },
            risks: None,
            changes: None,
            plan: None,
            approvers: None,
            previous_spec_snapshot: None,
        };
        let mut plan = MigrationPlan::new("parser-pg", spec);
        if let Some(p) = phase {
            plan.status = Some(MigrationPlanStatus {
                phase: Some(p.into()),
                ..MigrationPlanStatus::default()
            });
        }
        plan
    }

    #[test]
    fn plan_state_buckets_by_phase_and_trigger_match() {
        let cur = change("selector-change", "needs.pg.selector");
        // Single-op edit: the current change set is just `cur`.
        let cur_set = std::slice::from_ref(&cur);
        // No plan → None.
        assert_eq!(plan_state(None, &cur, cur_set), PlanState::None);
        // Blocking (pending) + trigger matches → BlockingMatch.
        let matching = plan_with_trigger(
            "selector-change",
            "needs.pg.selector",
            Some("pending-approval"),
        );
        assert_eq!(
            plan_state(Some(&matching), &cur, cur_set),
            PlanState::BlockingMatch
        );
        // Blocking (pending) + trigger differs → BlockingMismatch.
        let mismatch = plan_with_trigger(
            "storage-class-change",
            "needs.pg.storage",
            Some("pending-approval"),
        );
        assert_eq!(
            plan_state(Some(&mismatch), &cur, cur_set),
            PlanState::BlockingMismatch
        );
        // Phase failed → Failed (regardless of trigger match).
        let failed = plan_with_trigger("selector-change", "needs.pg.selector", Some("failed"));
        assert_eq!(plan_state(Some(&failed), &cur, cur_set), PlanState::Failed);
        // Completed + trigger matches → CompletedMatch. Post-S-4-review the
        // completed plan MUST carry the matching stamped content hash to
        // consume (a hashless completed plan re-gates), so stamp it — over the
        // FULL current change set (here a single op).
        let mut done = plan_with_trigger("selector-change", "needs.pg.selector", Some("completed"));
        done.spec.trigger.approved_spec_hash = Some(change_hash(cur_set));
        assert_eq!(
            plan_state(Some(&done), &cur, cur_set),
            PlanState::CompletedMatch
        );
        // Completed + trigger differs → Relic.
        let done_other = plan_with_trigger(
            "storage-class-change",
            "needs.pg.storage",
            Some("completed"),
        );
        assert_eq!(
            plan_state(Some(&done_other), &cur, cur_set),
            PlanState::Relic
        );
        // Rejected → Relic.
        let rejected = plan_with_trigger("selector-change", "needs.pg.selector", Some("rejected"));
        assert_eq!(plan_state(Some(&rejected), &cur, cur_set), PlanState::Relic);
    }

    // ---- 2.16b S-4: app approval is NON-transferable across a spec change ----

    // Build a `completed` app-scope plan whose trigger carries a specific
    // `(type, field, from, to)` and stamped `approvedSpecHash`. Lets a test
    // build a plan whose trigger TUPLE matches the current change but whose
    // stamped CONTENT hash differs (the S-4 attack: approve a benign
    // `from->to`, then swap the payload before consume).
    fn completed_plan_with_hash(
        change: &DestructiveChange,
        approved_spec_hash: Option<String>,
    ) -> MigrationPlan {
        let spec = MigrationPlanSpec {
            scope: MigrationPlanScope {
                type_: "application".into(),
                application: Some(MigrationApplicationScope {
                    ref_: MigrationApplicationRef {
                        name: "parser".into(),
                        namespace: "demo".into(),
                    },
                    environment: "prod".into(),
                }),
                platform: None,
                sourcecredential: None,
            },
            trigger: MigrationTrigger {
                type_: change.trigger_type.clone(),
                field: change.field.clone(),
                from: change.from.clone(),
                to: change.to.clone(),
                approved_spec_hash,
            },
            risks: None,
            changes: None,
            plan: None,
            approvers: None,
            previous_spec_snapshot: None,
        };
        let mut plan = MigrationPlan::new("parser-pg", spec);
        plan.status = Some(MigrationPlanStatus {
            phase: Some("completed".into()),
            ..MigrationPlanStatus::default()
        });
        plan
    }

    // A completed plan whose stamped hash matches the CURRENT change's hash
    // → CompletedMatch (consume + apply). A completed plan whose tuple
    // matches but whose stamped hash is for a DIFFERENT change → Relic (NOT
    // CompletedMatch), so `decide` re-gates it as a fresh plan and the
    // approval does NOT transfer. A legacy plan (no stamped hash) still
    // consumes (don't break existing approvals).
    #[test]
    fn completed_plan_consumes_only_on_matching_content_hash() {
        // The change actually pending this reconcile: replicas 1 -> 0.
        let current = DestructiveChange {
            trigger_type: "scale-to-zero".into(),
            field: "replicas".into(),
            from: Some(serde_json::json!("1")),
            to: Some(serde_json::json!("0")),
            classification: "requires-restart".into(),
        };
        let cur_set = std::slice::from_ref(&current);
        let current_hash = change_hash(cur_set);

        // (a) hash matches → CompletedMatch.
        let matched = completed_plan_with_hash(&current, Some(current_hash.clone()));
        assert_eq!(
            plan_state(Some(&matched), &current, cur_set),
            PlanState::CompletedMatch
        );

        // (b) THE ATTACK: the approver signed off on a DIFFERENT change
        // (replicas 2 -> 0) — same `(type, field)` tuple, different
        // content. Its stamped hash must NOT transfer to `current`.
        let approved = DestructiveChange {
            from: Some(serde_json::json!("2")),
            ..current.clone()
        };
        let approved_hash = change_hash(std::slice::from_ref(&approved));
        assert_ne!(approved_hash, current_hash); // sanity: different content
        let transferable = completed_plan_with_hash(&approved, Some(approved_hash));
        // Tuple STILL matches (both scale-to-zero/replicas) …
        assert_eq!(transferable.spec.trigger.type_, current.trigger_type);
        assert_eq!(transferable.spec.trigger.field, current.field);
        // … but the content hash differs → Relic, NOT CompletedMatch.
        assert_eq!(
            plan_state(Some(&transferable), &current, cur_set),
            PlanState::Relic
        );

        // (c) THE S-4 REVIEW ATTACK: a completed plan carrying NO stamped
        // hash (forged or otherwise) must NOT consume. App-scope migration
        // is brand-new — there are zero legacy hashless plans — so the
        // former `None => true` "legacy safety" bypass is gone: a hashless
        // completed plan re-gates (Relic), never applies the change. This
        // hashless-completed case is also `warn!`-logged in `plan_state`
        // (F-4 observability) to surface a forged/pre-2.16b-sec plan.
        let hashless = completed_plan_with_hash(&current, None);
        assert_eq!(
            plan_state(Some(&hashless), &current, cur_set),
            PlanState::Relic
        );

        // (d) an EMPTY stamped hash is likewise no match → Relic.
        let empty_hash = completed_plan_with_hash(&current, Some(String::new()));
        assert_eq!(
            plan_state(Some(&empty_hash), &current, cur_set),
            PlanState::Relic
        );
    }

    // The state machine as a whole must yield DeleteThenCreate (re-gate),
    // NOT ConsumeApply, when a completed plan's tuple matches but its
    // content hash does not — i.e. the approval is refused transfer.
    #[test]
    fn state_machine_re_gates_on_hash_mismatch_not_consume() {
        let current = DestructiveChange {
            trigger_type: "scale-to-zero".into(),
            field: "replicas".into(),
            from: Some(serde_json::json!("1")),
            to: Some(serde_json::json!("0")),
            classification: "requires-restart".into(),
        };
        let approved = DestructiveChange {
            from: Some(serde_json::json!("2")),
            ..current.clone()
        };
        let approved_hash = change_hash(std::slice::from_ref(&approved));
        let transferable = completed_plan_with_hash(&approved, Some(approved_hash));
        let state = plan_state(
            Some(&transferable),
            &current,
            std::slice::from_ref(&current),
        );
        assert_eq!(state, PlanState::Relic);
        // has_change=true × Relic → DeleteThenCreate (re-gate as a fresh
        // pending-approval plan), NOT ConsumeApply.
        assert_eq!(decide(true, state), MigrationDecision::DeleteThenCreate);
    }

    // 2.16b S1.2 / S-4 close: the approval hash covers the FULL candidate set,
    // so DROPPING one of the approved ops (or adding a new one) re-gates the
    // edit — the whole point of hashing all candidates rather than just the
    // primary. Approve for {needs-removal (data-migration) + scale-to-zero
    // (requires-restart)}; then the spec carries ONLY scale-to-zero. The
    // trigger TUPLE still matches (the primary of the single-op set is
    // scale-to-zero, but the plan's trigger is the data-migration primary — so
    // even the tuple differs here). Regardless: the full-set content hash the
    // plan was cut over ≠ the single-op current hash → Relic, NOT consume.
    #[test]
    fn dropping_an_approved_op_re_gates_not_consume() {
        // The two ops the plan was approved for (full candidate set).
        let needs_removal = DestructiveChange {
            trigger_type: "needs-removal".into(),
            field: "needs.pg".into(),
            from: Some(serde_json::json!("needs.pg")),
            to: Some(serde_json::json!("(removed)")),
            classification: "data-migration".into(),
        };
        let scale_to_zero = DestructiveChange {
            trigger_type: "scale-to-zero".into(),
            field: "replicas".into(),
            from: Some(serde_json::json!("2")),
            to: Some(serde_json::json!("0")),
            classification: "requires-restart".into(),
        };
        // The plan was cut over BOTH ops; its stamped approval hash is the
        // full-set hash and its trigger is the primary (data-migration wins).
        let approved_set = [needs_removal.clone(), scale_to_zero.clone()];
        let approved_hash = change_hash(&approved_set);
        let mut plan = completed_plan_with_hash(&needs_removal, Some(approved_hash.clone()));
        // Make the plan a data-migration primary explicitly (completed_plan_
        // with_hash already copied needs_removal's tuple).
        assert_eq!(plan.spec.trigger.type_, "needs-removal");
        plan.spec.changes = None; // rollup rows don't affect consume-time hash

        // (a) SANITY: if the CURRENT set is still both ops, the plan consumes.
        let current_primary = &needs_removal; // data-migration is the primary
        assert_eq!(
            plan_state(Some(&plan), current_primary, &approved_set),
            PlanState::CompletedMatch,
            "unchanged full set must still consume"
        );

        // (b) THE DROP: the spec now carries ONLY scale-to-zero — the
        // needs-removal op was dropped (or never re-declared). The current
        // candidate set is a single op → its full-set hash ≠ the approved
        // two-op hash → Relic, so decide() re-gates instead of consuming.
        let current_set_after_drop = [scale_to_zero.clone()];
        let current_after_drop = &scale_to_zero;
        assert_ne!(
            change_hash(&current_set_after_drop),
            approved_hash,
            "dropping an op must change the full-set hash"
        );
        let state = plan_state(Some(&plan), current_after_drop, &current_set_after_drop);
        assert_eq!(
            state,
            PlanState::Relic,
            "dropped op → full-set hash mismatch → re-gate, not consume"
        );
        assert_eq!(decide(true, state), MigrationDecision::DeleteThenCreate);

        // (c) THE ADD-ALONG (the actual S-4 laundering vector): the approver
        // signed off on scale-to-zero ALONE, but the attacker rides a
        // needs-removal (data drop!) along. If we hashed only the primary the
        // add would be unhashed and consume; hashing the full set means the
        // current two-op hash ≠ the approved one-op hash → re-gate.
        let approved_one = [scale_to_zero.clone()];
        let approved_one_hash = change_hash(&approved_one);
        let plan_one = completed_plan_with_hash(&scale_to_zero, Some(approved_one_hash.clone()));
        // Now the spec carries scale-to-zero + a smuggled needs-removal.
        // detect_all sorts data-migration first, so the primary is
        // needs-removal — but even keeping the tuple aside, the hash differs.
        let smuggled_set = [scale_to_zero.clone(), needs_removal.clone()];
        assert_ne!(
            change_hash(&smuggled_set),
            approved_one_hash,
            "a smuggled op must change the full-set hash"
        );
        // Consume-time primary of the smuggled set is needs-removal; its tuple
        // differs from the plan's scale-to-zero trigger → Relic anyway, and
        // the hash confirms it. Assert re-gate.
        assert_eq!(
            plan_state(Some(&plan_one), &needs_removal, &smuggled_set),
            PlanState::Relic,
            "smuggled lower/other op → re-gate, never consume unhashed"
        );
    }

    // `plan_state_no_change` — the detect=None bucketer. No plan → None
    // (`decide(false, None) = Render`); any plan → Relic (`decide(false, _)
    // = DeleteThenRender`). The exact non-None variant is irrelevant to
    // `decide`'s `has_change=false` rows, so bucketing every plan as Relic
    // is sound.
    #[test]
    fn plan_state_no_change_buckets_presence_only() {
        assert_eq!(plan_state_no_change(None), PlanState::None);
        let any = plan_with_trigger("selector-change", "needs.pg.selector", Some("completed"));
        assert_eq!(plan_state_no_change(Some(&any)), PlanState::Relic);
        // And it composes with `decide` to the right no-change decisions.
        assert_eq!(
            decide(false, plan_state_no_change(None)),
            MigrationDecision::Render
        );
        assert_eq!(
            decide(false, plan_state_no_change(Some(&any))),
            MigrationDecision::DeleteThenRender
        );
    }
}
