// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Pure validator for the v1alpha1 MigrationPlan object.
//!
//! Enforces cross-field invariants the OpenAPI v3 CRD layer
//! cannot express (B.1.75):
//!
//!   - **Scope discriminator.** `spec.scope.type == "application"`
//!     requires `spec.scope.application` populated with
//!     `ref.{name,namespace}` + `environment`. `type == "platform"`
//!     requires `spec.scope.platform.components` non-empty.
//!     `type == "sourcecredential"` (2.16b-sc) requires
//!     `spec.scope.sourcecredential.ref.{name,namespace}` (no
//!     `environment` — a SourceCredential has no per-env dimension).
//!     The mismatched sub-object is also rejected (a plan
//!     declaring `type: application` MUST NOT carry a
//!     `platform:`/`sourcecredential:` block — keeps the
//!     discriminator clean for trait dispatch in B.1.76).
//!   - **Approver emails.** Light RFC5322 — must contain `@`
//!     with non-empty local + domain parts and a dot in the
//!     domain. Surfaces obviously-malformed entries; doesn't
//!     attempt full RFC compliance because the admission
//!     surface isn't the right place for that.
//!   - **Scope immutability on UPDATE.** When the
//!     `AdmissionRequest.oldObject` is present (i.e. this is
//!     an UPDATE rather than a CREATE), reject any change to
//!     `spec.scope`. Trait dispatch is keyed on scope; mutating
//!     it mid-plan would silently switch which controller path
//!     executes. Other spec fields are allowed to change in 1.75;
//!     1.76 tightens the immutability rules around `plan`,
//!     `risks` etc. once the controller exists to enforce
//!     execution-order semantics.
//!
//! Typed against `operator_core::MigrationPlanSpec` (ADR 0047
//! Decision #4): the spec is deserialized into the operator-core
//! struct once and the scope-discriminator + approver-email rules
//! read TYPED fields (`scope.type_`, `application.ref_.{name,
//! namespace}`, `application.environment`, `platform.components`,
//! `approvers`), so a renamed field fails to compile instead of
//! silently bypassing a rule. The PRESENCE ("is required") and
//! not-a-string diagnostics stay on the raw `Value`: the typed
//! struct's non-`Option` fields (`scope`, `scope.type_`,
//! `application.ref_`, `application.environment`,
//! `platform.components` elements) cannot represent an *absent* /
//! wrong-typed input, and an approver that is not a string cannot
//! land in `Vec<String>`. A non-conforming object never reaches
//! admission in production (a validating webhook runs after the
//! apiserver's structural validation, which already enforced the
//! generated CRD's `required`/types); those branches exist only for
//! defence-in-depth and the unit tests. The status-phase FSM and the
//! scope-immutability diff stay on the raw `Value` — they compare
//! `oldObject`/`object` JSON directly (a typed round-trip would erase
//! the absent-vs-present distinction the FSM's empty-phase fast-path
//! relies on).

use operator_core::MigrationPlanSpec;
use serde_json::Value;

use crate::validator::{is_operator_or_admin, ValidationError};

/// 2.16b-sec (F-1b): field-level allowlist for an EXTERNAL write to the
/// `MigrationPlan/status` subresource. `status` has no ownership guard at the
/// CRD layer — only the FSM phase-order — so a subject holding
/// `patch migrationplans/status` RBAC could forge `phase→approved`
/// (self-approve) OR write `executedSteps`/`approvedBy`/other controller-owned
/// status fields. Today only the operator SA holds the verb (RBAC-contained),
/// but this is the defence-in-depth webhook guard.
///
/// Layered ON TOP of the FSM phase-order validator (which still runs) — this
/// adds a WRITER/FIELD restriction the FSM does not express:
///   - The operator SA (or a cluster-admin break-glass) may write ANY status
///     field (the controller stamps phase/executedSteps/approvedAt/…). Returns
///     `Ok(())`.
///   - Any OTHER (external) subject — the human approver — may change ONLY the
///     approval signal: `status.phase` from `pending-approval` to `approved`,
///     touching NO other status field. Every other write is rejected:
///       * setting `executedSteps`, `approvedAt`, `approvedBy`, `rejectedAt`,
///         or any status key other than `phase`;
///       * a `phase` transition other than `pending-approval → approved`
///         (e.g. `→ completed` skip, approving an already-`approved`/sealed
///         plan, or `→ rejected` — rejection is platform-controller work).
///
/// `approvedBy` handling: an external approver must NOT be able to forge
/// `approvedBy` to a false identity. Since the only external write we permit
/// is the bare `phase` flip, ANY external `approvedBy` write is rejected here
/// (it is not `phase`, so it fails the "only phase changed" check) — the
/// operator/controller stamps `approvedBy` from the authenticated approver
/// identity in the audit path, never the client. See the field-diff below.
///
/// The diff is computed structurally: gather the union of status keys across
/// `old_status` and `new_status`, and for each key compare the two values. The
/// allowed external write has EXACTLY one changed key (`phase`) with the exact
/// `pending-approval → approved` values; anything else is a rejection naming
/// the offending field(s).
pub fn migrationplan_status_write_allowed(
    user_info: &Value,
    expected_sa: &str,
    old_status: &Value,
    new_status: &Value,
) -> Result<(), String> {
    // The operator SA (or cluster-admin break-glass) writes anything — the
    // controller owns phase/executedSteps/approvedAt/approvedBy/rejectedAt.
    if is_operator_or_admin(user_info, expected_sa) {
        return Ok(());
    }

    let username = user_info
        .get("username")
        .and_then(Value::as_str)
        .unwrap_or("<unknown>");

    // Collect the union of status keys (old ∪ new) and record every key whose
    // value changed. An absent key on one side is treated as `Null`, so
    // ADDING `executedSteps` (absent → set) or REMOVING `phase` both register
    // as a change on that key.
    let empty = serde_json::Map::new();
    let old_map = old_status.as_object().unwrap_or(&empty);
    let new_map = new_status.as_object().unwrap_or(&empty);
    let null = Value::Null;
    let mut changed_fields: Vec<&str> = Vec::new();
    for key in old_map.keys().chain(new_map.keys()) {
        if changed_fields.contains(&key.as_str()) {
            continue;
        }
        let old_val = old_map.get(key).unwrap_or(&null);
        let new_val = new_map.get(key).unwrap_or(&null);
        if old_val != new_val {
            changed_fields.push(key.as_str());
        }
    }

    // The ONLY allowed external change is `phase` alone.
    let only_phase_changed = changed_fields == ["phase"];
    let old_phase = old_map.get("phase").and_then(Value::as_str).unwrap_or("");
    let new_phase = new_map.get("phase").and_then(Value::as_str).unwrap_or("");
    let is_approval_signal = old_phase == "pending-approval" && new_phase == "approved";

    if only_phase_changed && is_approval_signal {
        return Ok(());
    }

    // Reject — name the offending field(s) / transition. When only `phase`
    // changed but it was not the approval signal, surface the transition;
    // otherwise list the non-phase fields the external subject tried to write.
    let detail = if only_phase_changed {
        format!("status.phase {old_phase:?}→{new_phase:?}")
    } else {
        let fields: Vec<&str> = changed_fields
            .iter()
            .copied()
            .filter(|f| *f != "phase")
            .collect();
        format!("status fields [{}]", fields.join(", "))
    };
    Err(format!(
        "external approval may only set status.phase pending-approval→approved; \
         rejected write to {detail} by {username:?} (only the operator ServiceAccount \
         may write other MigrationPlan.status fields)"
    ))
}

/// Validate a MigrationPlan AdmissionReview object.
///
/// `object` is the request's `object` field (the desired state
/// after the operation). `old_object` is the request's
/// `oldObject` field (the prior state) — `None` on CREATE,
/// `Some` on UPDATE. The webhook server hands both through
/// from the AdmissionRequest directly.
pub fn validate_migrationplan(object: &Value, old_object: Option<&Value>) -> Vec<ValidationError> {
    let mut errors = Vec::new();

    let Some(obj) = object.as_object() else {
        errors.push(ValidationError::new(
            "object",
            "MigrationPlan object must be a JSON object",
        ));
        return errors;
    };

    let Some(spec_value) = obj.get("spec").filter(|s| s.is_object()) else {
        errors.push(ValidationError::new("spec", "spec is required"));
        return errors;
    };

    // Deserialize the spec into the typed operator-core struct. In
    // production this always succeeds — a validating webhook runs after
    // the apiserver's structural validation against the generated CRD.
    // When it succeeds the scope-discriminator + approver-email rules
    // read the TYPED fields, so a renamed field fails to compile instead
    // of silently bypassing a rule (ADR 0047 #4). When it does not (a
    // unit-test fixture exercising a malformed input, or a misconfigured
    // apiserver) the rules fall back to the raw `Value`, matching the
    // pre-refactor `as_object()`/`as_str()` semantics exactly.
    let typed = serde_json::from_value::<MigrationPlanSpec>(spec_value.clone()).ok();
    let spec = spec_value.as_object().expect("filtered to an object above");

    // ---- scope discriminator ----
    let scope = spec.get("scope").and_then(Value::as_object);
    let Some(scope) = scope else {
        errors.push(ValidationError::new("spec.scope", "scope is required"));
        return errors;
    };

    // Prefer the typed discriminator (`scope.type_`); fall back to the raw
    // value when the spec did not deserialize. `scope.type` is non-`Option`
    // in the struct, so an absent / non-string `type` (the "" / other
    // branches below) is only representable on the raw `Value`.
    let scope_type = typed
        .as_ref()
        .map(|t| t.scope.type_.as_str())
        .unwrap_or_else(|| scope.get("type").and_then(Value::as_str).unwrap_or(""));
    let typed_scope = typed.as_ref().map(|t| &t.scope);
    match scope_type {
        "application" => validate_application_scope(scope, typed_scope, &mut errors),
        "platform" => validate_platform_scope(scope, typed_scope, &mut errors),
        "sourcecredential" => validate_sourcecredential_scope(scope, typed_scope, &mut errors),
        "" => {
            errors.push(ValidationError::new(
                "spec.scope.type",
                "scope.type is required (one of application|platform|sourcecredential)",
            ));
        }
        other => {
            errors.push(ValidationError::new(
                "spec.scope.type",
                format!("scope.type must be application|platform|sourcecredential (got {other:?})"),
            ));
        }
    }

    // ---- approver emails ----
    // On the typed path `approvers` is `Option<Vec<String>>`, so every entry
    // is already a string (the "must be a string email" branch is then
    // unreachable — a non-string approver fails deserialization, dropping to
    // the raw path). The raw path preserves the per-element not-a-string
    // diagnostic for the test fixtures / a non-conforming object.
    match typed.as_ref().map(|t| t.approvers.as_deref()) {
        Some(Some(approvers)) => {
            for (i, email) in approvers.iter().enumerate() {
                if !is_emailish(email) {
                    errors.push(ValidationError::new(
                        format!("spec.approvers[{i}]"),
                        format!("{email:?} is not a valid email address"),
                    ));
                }
            }
        }
        // typed-but-no-approvers: nothing to check.
        Some(None) => {}
        // spec did not deserialize: fall back to the raw array, preserving
        // the not-a-string branch.
        None => {
            if let Some(approvers) = spec.get("approvers").and_then(Value::as_array) {
                for (i, val) in approvers.iter().enumerate() {
                    let Some(email) = val.as_str() else {
                        errors.push(ValidationError::new(
                            format!("spec.approvers[{i}]"),
                            "approver must be a string email",
                        ));
                        continue;
                    };
                    if !is_emailish(email) {
                        errors.push(ValidationError::new(
                            format!("spec.approvers[{i}]"),
                            format!("{email:?} is not a valid email address"),
                        ));
                    }
                }
            }
        }
    }

    // ---- scope immutability (UPDATE only) ----
    if let Some(old) = old_object {
        let old_scope = old.pointer("/spec/scope");
        let new_scope = object.pointer("/spec/scope");
        if old_scope.is_some() && old_scope != new_scope {
            errors.push(ValidationError::new(
                "spec.scope",
                "spec.scope is immutable after MigrationPlan creation; \
                 create a new MigrationPlan instead of mutating an existing one",
            ));
        }

        // ---- status.phase FSM (UPDATE only) — B.1.76 ----
        //
        // External actors are allowed to transition
        // `pending-approval → approved` (any scope) and
        // `pending-approval → rejected` (platform scope only).
        // Controller-side transitions (`approved → executing`,
        // `executing → completed/failed`, etc.) are allowed too
        // — the webhook does not gate them on identity (trust
        // RBAC for that). Sealed states (`completed`, `failed`,
        // `rejected`) are immutable.
        validate_phase_transition(object, old, &mut errors);
    }

    errors
}

/// Phase transition FSM (B.1.76). Reads `oldObject.status.phase`
/// and `object.status.phase`, accepts the legal transitions,
/// rejects everything else.
///
/// Application-scope `pending-approval → rejected` is the
/// acceptance test #4 case — ADR 0027 explicitly forbids reject
/// for application-scope plans (the user reverts the Git commit
/// instead). The webhook is the load-bearing guard.
fn validate_phase_transition(
    object: &Value,
    old_object: &Value,
    errors: &mut Vec<ValidationError>,
) {
    let old_phase = old_object
        .pointer("/status/phase")
        .and_then(Value::as_str)
        .unwrap_or("");
    let new_phase = object
        .pointer("/status/phase")
        .and_then(Value::as_str)
        .unwrap_or("");

    // No phase change → nothing to validate.
    if old_phase == new_phase {
        return;
    }

    let scope_type = object
        .pointer("/spec/scope/type")
        .and_then(Value::as_str)
        .unwrap_or("");

    if !is_allowed_phase_transition(old_phase, new_phase, scope_type) {
        // ADR 0027: application-scope plans cannot reach the
        // `rejected` phase by ANY path — direct CREATE-with-
        // status, kubectl patch from pending-approval, or
        // anywhere else. Surface the specific reason so users
        // hitting this know to `git revert` instead.
        //
        // Walk-fix #2 post-B.1.77: prior version only matched
        // the explicit `pending-approval → rejected` transition.
        // First-write to status.phase=rejected on a fresh CR
        // (oldObject.status.phase=="") slipped through the
        // FSM's first-write allow-everything branch and the
        // generic error message was less useful for that case.
        let reason = if scope_type == "application" && new_phase == "rejected" {
            "application-scope MigrationPlans cannot be rejected; \
             revert the Git commit in your application repo and let \
             the Application reconciler supersede this plan (ADR 0027)"
                .to_string()
        } else if scope_type == "sourcecredential" && new_phase == "rejected" {
            "sourcecredential-scope MigrationPlans cannot be rejected; \
             re-widen the SourceCredential coverage in your spec and let \
             the SourceCredential reconciler supersede this plan (ADR 0039). \
             sourcecredential-scope plans are approve-only"
                .to_string()
        } else {
            format!(
                "illegal status.phase transition {old_phase:?} → {new_phase:?} \
                 for scope.type={scope_type:?}; \
                 see spec.md §3.8 for the legal FSM"
            )
        };
        errors.push(ValidationError::new("status.phase", reason));
    }
}

/// Is `old_phase → new_phase` a legal transition for the
/// given scope type?
///
/// The FSM:
///
/// ```text
///   ""               → pending-approval                  (CREATE; status.phase unset on object)
///   ""               → approved | executing | completed | failed   (CREATE-with-status; allow for tooling)
///   ""               → rejected                          (platform scope only; ADR 0027/0039)
///   pending-approval → approved                          (any scope; external)
///   pending-approval → rejected                          (platform scope only; external)
///   approved         → executing                         (controller)
///   executing        → executing                         (same phase, no-op; covered by guard above)
///   executing        → completed | failed                (controller)
///   sealed (completed/failed/rejected) → *               (forbidden — immutable)
/// ```
fn is_allowed_phase_transition(old_phase: &str, new_phase: &str, scope_type: &str) -> bool {
    // ADR 0027: application-scope plans cannot be rejected by
    // any path. ADR 0039 / 2.16b-sc: sourcecredential-scope
    // plans are likewise approve-only (a coverage removal on a
    // config object — reject = keep the wider coverage and the
    // user re-widens the spec, no controller-side state to roll
    // back). These rules MUST apply BEFORE the empty-old-phase
    // first-write fast-path — otherwise a fresh CR (CR without
    // status; oldObject.status.phase = "") patched with
    // `phase=rejected` slips through with the permissive "allow
    // anything on first write" rule. Walk-fix #2 post-B.1.77.
    if new_phase == "rejected" && matches!(scope_type, "application" | "sourcecredential") {
        return false;
    }

    // First-write to status.phase from an empty / absent
    // value: accept anything legal for the FSM's downstream
    // transitions. Tooling that creates a plan already in
    // `approved` (admin shortcut) is rare but not malformed.
    if old_phase.is_empty() {
        return matches!(
            new_phase,
            "pending-approval" | "approved" | "executing" | "completed" | "failed" | "rejected"
        );
    }

    // Sealed states never transition. The CR can be deleted +
    // recreated under a new name if a fresh attempt is needed.
    if matches!(old_phase, "completed" | "failed" | "rejected") {
        return false;
    }

    match (old_phase, new_phase) {
        ("pending-approval", "approved") => true,
        ("pending-approval", "rejected") => scope_type == "platform",
        ("approved", "executing") => true,
        ("executing", "completed") => true,
        ("executing", "failed") => true,
        // Controller may stay on `executing` while running
        // step-by-step — the equality guard above prevents this
        // function from being called in that case.
        _ => false,
    }
}

/// `typed` is `Some` only when the whole spec deserialized; it carries
/// the typed `scope` (`application` / `platform` are `Option`, so a
/// populated `Some` reads the discriminator sub-objects with the
/// compiler gating the field names). The raw `scope` map is retained for
/// the PRESENCE / empty-string diagnostics the non-`Option` typed fields
/// cannot represent.
fn validate_application_scope(
    scope: &serde_json::Map<String, Value>,
    typed: Option<&operator_core::MigrationPlanScope>,
    errors: &mut Vec<ValidationError>,
) {
    // Mismatched sub-object: `platform` must be absent. Prefer the typed
    // `Option` (gates the field name); fall back to raw presence.
    let platform_present = match typed {
        Some(s) => s.platform.is_some(),
        None => scope.contains_key("platform"),
    };
    if platform_present {
        errors.push(ValidationError::new(
            "spec.scope.platform",
            "platform block must not be set when scope.type is application",
        ));
    }

    // `application` is `Option` on the typed scope; `None` (or a spec that
    // did not deserialize and has no `application` map) is the "required"
    // branch. When present, read the typed `ref_`/`environment`.
    let typed_app = typed.and_then(|s| s.application.as_ref());
    let raw_app = scope.get("application").and_then(Value::as_object);
    if typed_app.is_none() && raw_app.is_none() {
        errors.push(ValidationError::new(
            "spec.scope.application",
            "scope.application is required when scope.type is application",
        ));
        return;
    }

    match typed_app {
        // Typed happy path: `ref_` is non-`Option` so it is always present;
        // `name`/`namespace` are non-`Option` `String` so the only failure
        // the webhook still guards is an empty value (the CRD pattern also
        // rejects it — defence-in-depth + the unit fixtures).
        Some(app) => {
            if app.ref_.name.is_empty() {
                errors.push(ValidationError::new(
                    "spec.scope.application.ref.name",
                    "name is required",
                ));
            }
            if app.ref_.namespace.is_empty() {
                errors.push(ValidationError::new(
                    "spec.scope.application.ref.namespace",
                    "namespace is required",
                ));
            }
            // NOT validated: `environment`. The EMPTY STRING IS THE BASE
            // (unset `spec.environment`) DEPLOY, and is what the operator
            // writes for it everywhere — `env_owned`, `PlanKey`,
            // `plan_name` and `plans_to_delete` all key on "" for base.
            // Requiring it non-empty here froze every base-only
            // Application that hit a destructive change: the plan could
            // not be admitted, so the gate never engaged and the reconcile
            // retried a rejection every 30s indefinitely. D15, found by the
            // 2.22b needs-removal walk after the same assumption was fixed
            // one layer down in the CRD pattern — the webhook carried an
            // independent copy of it, which is why fixing the schema alone
            // moved the rejection rather than removing it.
        }
        // Raw fallback (spec did not deserialize): `ref` may be absent, so
        // the PRESENCE branch lives here, matching the pre-refactor
        // `as_object()`/`as_str()` semantics exactly.
        None => {
            let app =
                raw_app.expect("raw_app is Some when typed_app is None and we did not return");
            match app.get("ref").and_then(Value::as_object) {
                None => errors.push(ValidationError::new(
                    "spec.scope.application.ref",
                    "ref is required (name + namespace)",
                )),
                Some(r) => {
                    let name = r.get("name").and_then(Value::as_str).unwrap_or("");
                    let namespace = r.get("namespace").and_then(Value::as_str).unwrap_or("");
                    if name.is_empty() {
                        errors.push(ValidationError::new(
                            "spec.scope.application.ref.name",
                            "name is required",
                        ));
                    }
                    if namespace.is_empty() {
                        errors.push(ValidationError::new(
                            "spec.scope.application.ref.namespace",
                            "namespace is required",
                        ));
                    }
                }
            }
            // See the typed branch above: "" is the base env, not a
            // missing value.
        }
    }
}

fn validate_platform_scope(
    scope: &serde_json::Map<String, Value>,
    typed: Option<&operator_core::MigrationPlanScope>,
    errors: &mut Vec<ValidationError>,
) {
    // Mismatched sub-object: `application` must be absent.
    let application_present = match typed {
        Some(s) => s.application.is_some(),
        None => scope.contains_key("application"),
    };
    if application_present {
        errors.push(ValidationError::new(
            "spec.scope.application",
            "application block must not be set when scope.type is platform",
        ));
    }

    // `platform` is `Option`; `None` is the "required" branch. When present,
    // `components` is non-`Option` `Vec<String>` — every element is already a
    // string, so the only webhook guard left on the typed path is non-empty
    // list + non-empty element. The not-a-string element diagnostic survives
    // only on the raw fallback.
    let typed_platform = typed.and_then(|s| s.platform.as_ref());
    let raw_platform = scope.get("platform").and_then(Value::as_object);
    match typed_platform {
        Some(platform) => {
            if platform.components.is_empty() {
                errors.push(ValidationError::new(
                    "spec.scope.platform.components",
                    "components must be non-empty",
                ));
            }
            for (i, name) in platform.components.iter().enumerate() {
                if name.is_empty() {
                    errors.push(ValidationError::new(
                        format!("spec.scope.platform.components[{i}]"),
                        "component name must be a non-empty string",
                    ));
                }
            }
        }
        None => {
            let Some(platform) = raw_platform else {
                errors.push(ValidationError::new(
                    "spec.scope.platform",
                    "scope.platform is required when scope.type is platform",
                ));
                return;
            };
            match platform.get("components").and_then(Value::as_array) {
                None => errors.push(ValidationError::new(
                    "spec.scope.platform.components",
                    "components is required (non-empty list of component names)",
                )),
                Some(arr) if arr.is_empty() => errors.push(ValidationError::new(
                    "spec.scope.platform.components",
                    "components must be non-empty",
                )),
                Some(arr) => {
                    for (i, val) in arr.iter().enumerate() {
                        if val.as_str().is_none_or(|s| s.is_empty()) {
                            errors.push(ValidationError::new(
                                format!("spec.scope.platform.components[{i}]"),
                                "component name must be a non-empty string",
                            ));
                        }
                    }
                }
            }
        }
    }
}

/// 2.16b-sc: validate a `sourcecredential`-scope MigrationPlan. Mirrors
/// `validate_application_scope` — typed-then-raw fallback, mismatched
/// sub-object rejection, non-empty `ref.{name,namespace}` — minus the
/// `environment` field (a SourceCredential has no per-env dimension).
///
/// `typed` is `Some` only when the whole spec deserialized; it carries the
/// typed `scope` (`sourcecredential` is `Option`, so a populated `Some`
/// reads the discriminator sub-object with the compiler gating the field
/// names). The raw `scope` map is retained for the PRESENCE diagnostics the
/// non-`Option` typed fields cannot represent.
fn validate_sourcecredential_scope(
    scope: &serde_json::Map<String, Value>,
    typed: Option<&operator_core::MigrationPlanScope>,
    errors: &mut Vec<ValidationError>,
) {
    // Mismatched sub-objects: neither `application` nor `platform` may be set
    // when scope.type is sourcecredential. Prefer the typed `Option` (gates
    // the field name); fall back to raw presence.
    let application_present = match typed {
        Some(s) => s.application.is_some(),
        None => scope.contains_key("application"),
    };
    if application_present {
        errors.push(ValidationError::new(
            "spec.scope.application",
            "application block must not be set when scope.type is sourcecredential",
        ));
    }
    let platform_present = match typed {
        Some(s) => s.platform.is_some(),
        None => scope.contains_key("platform"),
    };
    if platform_present {
        errors.push(ValidationError::new(
            "spec.scope.platform",
            "platform block must not be set when scope.type is sourcecredential",
        ));
    }

    // `sourcecredential` is `Option` on the typed scope; `None` (or a spec
    // that did not deserialize and has no `sourcecredential` map) is the
    // "required" branch. When present, read the typed `ref_`.
    let typed_sc = typed.and_then(|s| s.sourcecredential.as_ref());
    let raw_sc = scope.get("sourcecredential").and_then(Value::as_object);
    if typed_sc.is_none() && raw_sc.is_none() {
        errors.push(ValidationError::new(
            "spec.scope.sourcecredential",
            "scope.sourcecredential is required when scope.type is sourcecredential",
        ));
        return;
    }

    match typed_sc {
        // Typed happy path: `ref_` is non-`Option` so it is always present;
        // `name`/`namespace` are non-`Option` `String` so the only failure
        // the webhook still guards is an empty value (the CRD pattern also
        // rejects it — defence-in-depth + the unit fixtures).
        Some(sc) => {
            if sc.ref_.name.is_empty() {
                errors.push(ValidationError::new(
                    "spec.scope.sourcecredential.ref.name",
                    "name is required",
                ));
            }
            if sc.ref_.namespace.is_empty() {
                errors.push(ValidationError::new(
                    "spec.scope.sourcecredential.ref.namespace",
                    "namespace is required",
                ));
            }
        }
        // Raw fallback (spec did not deserialize): `ref` may be absent, so
        // the PRESENCE branch lives here, matching the application-scope raw
        // fallback semantics.
        None => {
            let sc = raw_sc.expect("raw_sc is Some when typed_sc is None and we did not return");
            match sc.get("ref").and_then(Value::as_object) {
                None => errors.push(ValidationError::new(
                    "spec.scope.sourcecredential.ref",
                    "ref is required (name + namespace)",
                )),
                Some(r) => {
                    let name = r.get("name").and_then(Value::as_str).unwrap_or("");
                    let namespace = r.get("namespace").and_then(Value::as_str).unwrap_or("");
                    if name.is_empty() {
                        errors.push(ValidationError::new(
                            "spec.scope.sourcecredential.ref.name",
                            "name is required",
                        ));
                    }
                    if namespace.is_empty() {
                        errors.push(ValidationError::new(
                            "spec.scope.sourcecredential.ref.namespace",
                            "namespace is required",
                        ));
                    }
                }
            }
        }
    }
}

/// Loose RFC5322 — must contain exactly one `@`, with a
/// non-empty local part and a domain that has at least one `.`
/// with non-empty labels around it. Catches the obvious
/// typos; doesn't try to mirror every RFC corner case
/// (quoted locals, IP-literal domains, ...) because admission
/// webhooks aren't the right place to do that.
fn is_emailish(s: &str) -> bool {
    let mut parts = s.splitn(2, '@');
    let local = parts.next().unwrap_or("");
    let domain = parts.next().unwrap_or("");
    if s.matches('@').count() != 1 || local.is_empty() || domain.is_empty() {
        return false;
    }
    let domain_labels: Vec<&str> = domain.split('.').collect();
    domain_labels.len() >= 2 && domain_labels.iter().all(|l| !l.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn application_scope_object() -> Value {
        json!({
            "metadata": { "name": "parser-pg-2026-05-22", "namespace": "apprafter-system" },
            "spec": {
                "scope": {
                    "type": "application",
                    "application": {
                        "ref": { "name": "parser", "namespace": "demo" },
                        "environment": "prod"
                    }
                },
                "trigger": { "type": "selector-change", "field": "needs.pg.selector" },
                "approvers": ["alice@company.com"]
            }
        })
    }

    fn platform_scope_object() -> Value {
        json!({
            "metadata": { "name": "platform-0-2-0", "namespace": "apprafter-system" },
            "spec": {
                "scope": {
                    "type": "platform",
                    "platform": {
                        "components": ["apprafter-operator", "admission-webhook"]
                    }
                },
                "trigger": { "type": "platform-classification", "field": "spec.pin" },
                "approvers": ["alice@company.com", "bob@company.com"]
            }
        })
    }

    fn sourcecredential_scope_object() -> Value {
        json!({
            "metadata": { "name": "github-org-2026-08-07", "namespace": "apprafter-system" },
            "spec": {
                "scope": {
                    "type": "sourcecredential",
                    "sourcecredential": {
                        "ref": { "name": "github-org", "namespace": "apprafter-system" }
                    }
                },
                "trigger": { "type": "coverage-removal", "field": "spec.git.repoPrefixes" },
                "approvers": ["alice@company.com"]
            }
        })
    }

    // ---------- happy paths ----------

    #[test]
    fn accepts_canonical_application_scope() {
        assert!(validate_migrationplan(&application_scope_object(), None).is_empty());
    }

    #[test]
    fn accepts_canonical_platform_scope() {
        assert!(validate_migrationplan(&platform_scope_object(), None).is_empty());
    }

    #[test]
    fn accepts_omitted_approvers() {
        let mut obj = application_scope_object();
        obj["spec"].as_object_mut().unwrap().remove("approvers");
        assert!(validate_migrationplan(&obj, None).is_empty());
    }

    // ---------- scope discriminator ----------

    #[test]
    fn rejects_missing_scope() {
        let obj = json!({
            "spec": {
                "trigger": { "type": "t", "field": "f" }
            }
        });
        let errors = validate_migrationplan(&obj, None);
        assert!(errors.iter().any(|e| e.field == "spec.scope"));
    }

    #[test]
    fn rejects_missing_scope_type() {
        let obj = json!({
            "spec": {
                "scope": {},
                "trigger": { "type": "t", "field": "f" }
            }
        });
        let errors = validate_migrationplan(&obj, None);
        assert!(errors.iter().any(|e| e.field == "spec.scope.type"));
    }

    #[test]
    fn rejects_unknown_scope_type() {
        let mut obj = application_scope_object();
        obj["spec"]["scope"]["type"] = json!("tenant");
        let errors = validate_migrationplan(&obj, None);
        assert!(errors.iter().any(|e| e.field == "spec.scope.type"));
    }

    #[test]
    fn rejects_application_scope_missing_application_block() {
        let mut obj = application_scope_object();
        obj["spec"]["scope"]
            .as_object_mut()
            .unwrap()
            .remove("application");
        let errors = validate_migrationplan(&obj, None);
        assert!(errors.iter().any(|e| e.field == "spec.scope.application"));
    }

    #[test]
    fn rejects_application_scope_missing_ref() {
        let mut obj = application_scope_object();
        obj["spec"]["scope"]["application"]
            .as_object_mut()
            .unwrap()
            .remove("ref");
        let errors = validate_migrationplan(&obj, None);
        assert!(errors
            .iter()
            .any(|e| e.field == "spec.scope.application.ref"));
    }

    #[test]
    fn rejects_application_scope_with_platform_block() {
        let mut obj = application_scope_object();
        obj["spec"]["scope"]["platform"] = json!({ "components": ["x"] });
        let errors = validate_migrationplan(&obj, None);
        assert!(errors.iter().any(|e| e.field == "spec.scope.platform"));
    }

    #[test]
    fn accepts_the_base_env_scope() {
        // D15. This test previously asserted the OPPOSITE — that an
        // absent environment is rejected — and so defended a defect that
        // froze every base-only Application: the operator writes "" for
        // an unset `spec.environment`, the webhook refused it, and the
        // gating plan could never be admitted, so the reconcile retried a
        // rejection every 30s forever while the gate never engaged.
        //
        // Both shapes the operator can produce must pass: the field
        // present-and-empty (what `create_plan_for` actually emits) and
        // the field absent (the raw-fallback path).
        let mut present_empty = application_scope_object();
        present_empty["spec"]["scope"]["application"]["environment"] = json!("");
        assert!(
            !validate_migrationplan(&present_empty, None)
                .iter()
                .any(|e| e.field == "spec.scope.application.environment"),
            "the base env is a legitimate scope, not a missing value"
        );

        let mut absent = application_scope_object();
        absent["spec"]["scope"]["application"]
            .as_object_mut()
            .unwrap()
            .remove("environment");
        assert!(!validate_migrationplan(&absent, None)
            .iter()
            .any(|e| e.field == "spec.scope.application.environment"));
    }

    #[test]
    fn still_rejects_an_application_scope_with_no_ref() {
        // The guard that IS wanted must survive removing the one that was
        // not: dropping the environment requirement must not weaken the
        // ref checks that share the branch.
        let mut obj = application_scope_object();
        obj["spec"]["scope"]["application"]
            .as_object_mut()
            .unwrap()
            .remove("ref");
        let errors = validate_migrationplan(&obj, None);
        assert!(errors
            .iter()
            .any(|e| e.field.starts_with("spec.scope.application.ref")));
    }

    #[test]
    fn rejects_platform_scope_missing_platform_block() {
        let mut obj = platform_scope_object();
        obj["spec"]["scope"]
            .as_object_mut()
            .unwrap()
            .remove("platform");
        let errors = validate_migrationplan(&obj, None);
        assert!(errors.iter().any(|e| e.field == "spec.scope.platform"));
    }

    #[test]
    fn rejects_platform_scope_with_empty_components() {
        let mut obj = platform_scope_object();
        obj["spec"]["scope"]["platform"]["components"] = json!([]);
        let errors = validate_migrationplan(&obj, None);
        assert!(errors
            .iter()
            .any(|e| e.field == "spec.scope.platform.components"));
    }

    #[test]
    fn rejects_platform_scope_with_application_block() {
        let mut obj = platform_scope_object();
        obj["spec"]["scope"]["application"] = json!({
            "ref": { "name": "x", "namespace": "y" },
            "environment": "z"
        });
        let errors = validate_migrationplan(&obj, None);
        assert!(errors.iter().any(|e| e.field == "spec.scope.application"));
    }

    // ---------- sourcecredential scope (2.16b-sc) ----------

    #[test]
    fn accepts_canonical_sourcecredential_scope() {
        assert!(validate_migrationplan(&sourcecredential_scope_object(), None).is_empty());
    }

    #[test]
    fn rejects_sourcecredential_scope_missing_sourcecredential_block() {
        let mut obj = sourcecredential_scope_object();
        obj["spec"]["scope"]
            .as_object_mut()
            .unwrap()
            .remove("sourcecredential");
        let errors = validate_migrationplan(&obj, None);
        assert!(errors
            .iter()
            .any(|e| e.field == "spec.scope.sourcecredential"));
    }

    #[test]
    fn rejects_sourcecredential_scope_missing_ref() {
        let mut obj = sourcecredential_scope_object();
        obj["spec"]["scope"]["sourcecredential"]
            .as_object_mut()
            .unwrap()
            .remove("ref");
        let errors = validate_migrationplan(&obj, None);
        assert!(errors
            .iter()
            .any(|e| e.field == "spec.scope.sourcecredential.ref"));
    }

    #[test]
    fn rejects_sourcecredential_scope_empty_ref_name() {
        let mut obj = sourcecredential_scope_object();
        obj["spec"]["scope"]["sourcecredential"]["ref"]["name"] = json!("");
        let errors = validate_migrationplan(&obj, None);
        // An empty-string ref name deserializes fine (a valid `String`), so
        // it flows through the TYPED branch and hits the `name.is_empty()`
        // guard. The CRD pattern also rejects it; the webhook is the
        // load-bearing message here.
        assert!(errors
            .iter()
            .any(|e| e.field == "spec.scope.sourcecredential.ref.name"));
    }

    #[test]
    fn rejects_sourcecredential_scope_empty_ref_namespace() {
        let mut obj = sourcecredential_scope_object();
        obj["spec"]["scope"]["sourcecredential"]["ref"]["namespace"] = json!("");
        let errors = validate_migrationplan(&obj, None);
        assert!(errors
            .iter()
            .any(|e| e.field == "spec.scope.sourcecredential.ref.namespace"));
    }

    #[test]
    fn rejects_sourcecredential_scope_with_application_block() {
        let mut obj = sourcecredential_scope_object();
        obj["spec"]["scope"]["application"] = json!({
            "ref": { "name": "x", "namespace": "y" },
            "environment": "z"
        });
        let errors = validate_migrationplan(&obj, None);
        assert!(errors.iter().any(|e| e.field == "spec.scope.application"));
    }

    #[test]
    fn rejects_sourcecredential_scope_with_platform_block() {
        let mut obj = sourcecredential_scope_object();
        obj["spec"]["scope"]["platform"] = json!({ "components": ["x"] });
        let errors = validate_migrationplan(&obj, None);
        assert!(errors.iter().any(|e| e.field == "spec.scope.platform"));
    }

    // ---------- approver emails ----------

    #[test]
    fn rejects_malformed_approver_email() {
        let mut obj = application_scope_object();
        obj["spec"]["approvers"] = json!(["not-an-email"]);
        let errors = validate_migrationplan(&obj, None);
        assert!(errors.iter().any(|e| e.field == "spec.approvers[0]"));
    }

    #[test]
    fn rejects_missing_at_in_approver() {
        let mut obj = application_scope_object();
        obj["spec"]["approvers"] = json!(["aliceatcompany.com"]);
        let errors = validate_migrationplan(&obj, None);
        assert!(errors.iter().any(|e| e.field == "spec.approvers[0]"));
    }

    #[test]
    fn rejects_missing_domain_dot_in_approver() {
        let mut obj = application_scope_object();
        obj["spec"]["approvers"] = json!(["alice@localhost"]);
        let errors = validate_migrationplan(&obj, None);
        assert!(errors.iter().any(|e| e.field == "spec.approvers[0]"));
    }

    #[test]
    fn accepts_well_formed_email() {
        assert!(is_emailish("alice@company.com"));
        assert!(is_emailish("alice+plan@team.company.co"));
    }

    #[test]
    fn rejects_double_at_email() {
        let mut obj = application_scope_object();
        obj["spec"]["approvers"] = json!(["a@b@c.com"]);
        let errors = validate_migrationplan(&obj, None);
        assert!(errors.iter().any(|e| e.field == "spec.approvers[0]"));
    }

    #[test]
    fn reports_each_invalid_approver_separately() {
        let mut obj = application_scope_object();
        obj["spec"]["approvers"] = json!(["ok@x.com", "bad", "also-bad", "also@ok.com"]);
        let errors = validate_migrationplan(&obj, None);
        assert!(errors.iter().any(|e| e.field == "spec.approvers[1]"));
        assert!(errors.iter().any(|e| e.field == "spec.approvers[2]"));
        assert!(!errors.iter().any(|e| e.field == "spec.approvers[0]"));
        assert!(!errors.iter().any(|e| e.field == "spec.approvers[3]"));
    }

    // ---------- scope immutability (UPDATE) ----------

    #[test]
    fn allows_unchanged_scope_on_update() {
        let new = application_scope_object();
        let old = application_scope_object();
        assert!(validate_migrationplan(&new, Some(&old)).is_empty());
    }

    #[test]
    fn rejects_scope_type_change_on_update() {
        let old = application_scope_object();
        let new = platform_scope_object();
        let errors = validate_migrationplan(&new, Some(&old));
        assert!(errors.iter().any(|e| e.field == "spec.scope"));
    }

    #[test]
    fn rejects_application_ref_change_on_update() {
        let old = application_scope_object();
        let mut new = application_scope_object();
        new["spec"]["scope"]["application"]["ref"]["name"] = json!("renamed");
        let errors = validate_migrationplan(&new, Some(&old));
        assert!(errors.iter().any(|e| e.field == "spec.scope"));
    }

    #[test]
    fn allows_approvers_addition_on_update_when_scope_unchanged() {
        let old = application_scope_object();
        let mut new = application_scope_object();
        new["spec"]["approvers"] = json!(["alice@company.com", "bob@company.com"]);
        // approvers mutation is allowed in 1.75 (B.1.76 tightens
        // this rule); the only update-time guard is scope
        // immutability.
        let errors = validate_migrationplan(&new, Some(&old));
        assert!(
            errors.is_empty(),
            "approvers addition should not trigger errors when scope is unchanged; got {errors:?}"
        );
    }

    // ---------- top-level shape ----------

    #[test]
    fn rejects_non_object_input() {
        let errors = validate_migrationplan(&json!("not-an-object"), None);
        assert!(errors.iter().any(|e| e.field == "object"));
    }

    #[test]
    fn rejects_missing_spec() {
        let obj = json!({ "metadata": { "name": "x", "namespace": "apprafter-system" } });
        let errors = validate_migrationplan(&obj, None);
        assert!(errors.iter().any(|e| e.field == "spec"));
    }

    // ---------- phase FSM (B.1.76) ----------

    fn with_phase(mut obj: Value, phase: &str) -> Value {
        obj["status"] = json!({ "phase": phase });
        obj
    }

    #[test]
    fn allows_application_scope_pending_to_approved() {
        let new = with_phase(application_scope_object(), "approved");
        let old = with_phase(application_scope_object(), "pending-approval");
        assert!(validate_migrationplan(&new, Some(&old)).is_empty());
    }

    #[test]
    fn allows_platform_scope_pending_to_approved() {
        let new = with_phase(platform_scope_object(), "approved");
        let old = with_phase(platform_scope_object(), "pending-approval");
        assert!(validate_migrationplan(&new, Some(&old)).is_empty());
    }

    #[test]
    fn allows_platform_scope_pending_to_rejected() {
        let new = with_phase(platform_scope_object(), "rejected");
        let old = with_phase(platform_scope_object(), "pending-approval");
        assert!(validate_migrationplan(&new, Some(&old)).is_empty());
    }

    #[test]
    fn rejects_application_scope_pending_to_rejected_per_adr_0027() {
        // Acceptance test #4 for B.1.76. Application-scope
        // plans cannot be rejected per ADR 0027; the user
        // reverts the Git commit instead.
        let new = with_phase(application_scope_object(), "rejected");
        let old = with_phase(application_scope_object(), "pending-approval");
        let errors = validate_migrationplan(&new, Some(&old));
        assert!(errors.iter().any(|e| e.field == "status.phase"));
        let msg = &errors
            .iter()
            .find(|e| e.field == "status.phase")
            .unwrap()
            .message;
        assert!(
            msg.contains("application-scope") && msg.contains("ADR 0027"),
            "error message should explain why application reject is blocked; got {msg:?}"
        );
    }

    #[test]
    fn allows_sourcecredential_scope_pending_to_approved() {
        // 2.16b-sc: the approval signal on a sourcecredential-scope plan
        // must go through, exactly like application/platform.
        let new = with_phase(sourcecredential_scope_object(), "approved");
        let old = with_phase(sourcecredential_scope_object(), "pending-approval");
        assert!(validate_migrationplan(&new, Some(&old)).is_empty());
    }

    #[test]
    fn rejects_sourcecredential_scope_pending_to_rejected() {
        // 2.16b-sc: sourcecredential-scope plans are approve-only — like
        // application scope (ADR 0027/0039), reject is not a path; the
        // user re-widens the SourceCredential spec instead. The webhook
        // is the load-bearing guard.
        let new = with_phase(sourcecredential_scope_object(), "rejected");
        let old = with_phase(sourcecredential_scope_object(), "pending-approval");
        let errors = validate_migrationplan(&new, Some(&old));
        assert!(
            errors.iter().any(|e| e.field == "status.phase"),
            "sourcecredential reject must trip the FSM guard; got {errors:?}"
        );
    }

    #[test]
    fn rejects_sourcecredential_scope_first_write_to_rejected() {
        // The empty-old-phase first-write path must also block a
        // sourcecredential-scope `"" → rejected` (mirror the app-scope
        // walk-fix #2 hardening).
        let mut new = sourcecredential_scope_object();
        new["status"] = json!({ "phase": "rejected" });
        let old = sourcecredential_scope_object();
        let errors = validate_migrationplan(&new, Some(&old));
        assert!(
            errors.iter().any(|e| e.field == "status.phase"),
            "first-write to rejected on sourcecredential scope must trip the FSM guard; got {errors:?}"
        );
    }

    #[test]
    fn allows_controller_approved_to_executing() {
        let new = with_phase(application_scope_object(), "executing");
        let old = with_phase(application_scope_object(), "approved");
        assert!(validate_migrationplan(&new, Some(&old)).is_empty());
    }

    #[test]
    fn allows_controller_executing_to_completed() {
        let new = with_phase(application_scope_object(), "completed");
        let old = with_phase(application_scope_object(), "executing");
        assert!(validate_migrationplan(&new, Some(&old)).is_empty());
    }

    #[test]
    fn allows_controller_executing_to_failed() {
        let new = with_phase(application_scope_object(), "failed");
        let old = with_phase(application_scope_object(), "executing");
        assert!(validate_migrationplan(&new, Some(&old)).is_empty());
    }

    #[test]
    fn rejects_completed_sealed_to_anything() {
        let new = with_phase(application_scope_object(), "executing");
        let old = with_phase(application_scope_object(), "completed");
        let errors = validate_migrationplan(&new, Some(&old));
        assert!(errors.iter().any(|e| e.field == "status.phase"));
    }

    #[test]
    fn rejects_failed_sealed_to_anything() {
        let new = with_phase(application_scope_object(), "executing");
        let old = with_phase(application_scope_object(), "failed");
        let errors = validate_migrationplan(&new, Some(&old));
        assert!(errors.iter().any(|e| e.field == "status.phase"));
    }

    #[test]
    fn rejects_rejected_sealed_to_anything() {
        let new = with_phase(platform_scope_object(), "approved");
        let old = with_phase(platform_scope_object(), "rejected");
        let errors = validate_migrationplan(&new, Some(&old));
        assert!(errors.iter().any(|e| e.field == "status.phase"));
    }

    #[test]
    fn rejects_skipping_approved_step() {
        // pending-approval → executing (skipping approved) is
        // not in the FSM. External tooling tempted to fast-
        // forward must hit `approved` first.
        let new = with_phase(application_scope_object(), "executing");
        let old = with_phase(application_scope_object(), "pending-approval");
        let errors = validate_migrationplan(&new, Some(&old));
        assert!(errors.iter().any(|e| e.field == "status.phase"));
    }

    #[test]
    fn allows_no_phase_change_on_update() {
        // Updates that don't touch status.phase (e.g. tweaks
        // to `approvers`) must not trip the FSM guard.
        let new = with_phase(application_scope_object(), "executing");
        let old = with_phase(application_scope_object(), "executing");
        assert!(validate_migrationplan(&new, Some(&old)).is_empty());
    }

    #[test]
    fn rejects_application_scope_first_write_to_rejected_per_adr_0027() {
        // Walk-found bug v0.1.127 → v0.1.128: a fresh
        // MigrationPlan CR has empty `status.phase`. The
        // FSM's "first-write allow-everything" branch let
        // `kubectl patch --subresource=status -p
        // '{"status":{"phase":"rejected"}}'` through even for
        // application-scope plans, bypassing the ADR 0027
        // rule that only platform-scope can be rejected.
        //
        // Regression guard: webhook MUST reject `"" →
        // rejected` on application scope, with the same
        // ADR 0027 error message as the
        // `pending-approval → rejected` case.
        let mut new = application_scope_object();
        new["status"] = json!({ "phase": "rejected" });
        // oldObject has no status — simulates a fresh CR
        // that just got created (status.phase absent / empty).
        let old = application_scope_object();
        let errors = validate_migrationplan(&new, Some(&old));
        assert!(
            errors.iter().any(|e| e.field == "status.phase"),
            "first-write to rejected on app-scope must trip the FSM guard; got {errors:?}"
        );
        let msg = &errors
            .iter()
            .find(|e| e.field == "status.phase")
            .unwrap()
            .message;
        assert!(
            msg.contains("application-scope") && msg.contains("ADR 0027"),
            "error message must reference ADR 0027; got {msg:?}"
        );
    }

    #[test]
    fn allows_platform_scope_first_write_to_rejected() {
        // Platform-scope plans CAN be rejected from any state
        // including direct first-write. Tooling that
        // pre-rejects a platform-scope plan (e.g. operator
        // who already approved the bump out-of-band but wants
        // an audit-trail entry) goes through.
        let mut new = platform_scope_object();
        new["status"] = json!({ "phase": "rejected" });
        let old = platform_scope_object();
        let errors = validate_migrationplan(&new, Some(&old));
        assert!(
            errors.is_empty(),
            "platform-scope first-write to rejected must be allowed; got {errors:?}"
        );
    }

    #[test]
    fn rejects_application_scope_approved_to_rejected_per_adr_0027() {
        // Defensive guard: even if some external actor
        // somehow patches phase=approved on an app-scope
        // plan, a subsequent flip to `rejected` must STILL
        // be blocked. The FSM's existing match arm doesn't
        // cover `("approved", "rejected")` so this falls
        // through to `_ => false` regardless of the new
        // walk-fix #2 early guard — testing here pins the
        // behaviour against a future refactor that might
        // accidentally permit the transition.
        let mut new = application_scope_object();
        new["status"] = json!({ "phase": "rejected" });
        let mut old = application_scope_object();
        old["status"] = json!({ "phase": "approved" });
        let errors = validate_migrationplan(&new, Some(&old));
        assert!(errors.iter().any(|e| e.field == "status.phase"));
    }

    // ── 2.16b-sec F-1b: MigrationPlan.status field-level allowlist ────────

    const TEST_SA: &str = "system:serviceaccount:apprafter-system:apprafter-operator";

    fn operator_user() -> Value {
        json!({ "username": TEST_SA, "groups": ["system:serviceaccounts"] })
    }
    fn admin_user() -> Value {
        json!({ "username": "kubernetes-admin", "groups": ["system:masters", "system:authenticated"] })
    }
    fn external_user() -> Value {
        json!({ "username": "alice", "groups": ["system:authenticated"] })
    }

    #[test]
    fn f1b_external_approval_pending_to_approved_is_allowed() {
        // The legit human-approver path: flip ONLY phase pending→approved.
        let old = json!({ "phase": "pending-approval" });
        let new = json!({ "phase": "approved" });
        assert!(
            migrationplan_status_write_allowed(&external_user(), TEST_SA, &old, &new).is_ok(),
            "external phase→approved must still pass — the whole approve flow depends on it"
        );
    }

    #[test]
    fn f1b_operator_may_write_any_status_field() {
        // The controller stamps phase + executedSteps + approvedAt etc.
        let old = json!({ "phase": "approved" });
        let new = json!({
            "phase": "executing",
            "executedSteps": [{ "step": 1, "startedAt": "2026-08-07T00:00:00Z", "outcome": "ok" }],
            "approvedAt": "2026-08-07T00:00:00Z",
            "approvedBy": "alice@company.com"
        });
        assert!(
            migrationplan_status_write_allowed(&operator_user(), TEST_SA, &old, &new).is_ok(),
            "the operator's own status write must pass, or the reconcile breaks"
        );
    }

    #[test]
    fn f1b_admin_break_glass_may_write_any_status_field() {
        let old = json!({ "phase": "pending-approval" });
        let new = json!({ "phase": "rejected", "rejectedAt": "2026-08-07T00:00:00Z" });
        assert!(
            migrationplan_status_write_allowed(&admin_user(), TEST_SA, &old, &new).is_ok(),
            "cluster-admin break-glass may write any status field"
        );
    }

    #[test]
    fn f1b_external_write_to_executed_steps_is_rejected() {
        // Forging executedSteps (even alongside a legit phase flip) is denied.
        let old = json!({ "phase": "pending-approval" });
        let new = json!({
            "phase": "approved",
            "executedSteps": [{ "step": 1, "startedAt": "2026-08-07T00:00:00Z", "outcome": "ok" }]
        });
        let err = migrationplan_status_write_allowed(&external_user(), TEST_SA, &old, &new)
            .expect_err("external executedSteps write must be rejected");
        assert!(
            err.contains("executedSteps"),
            "message should name the field: {err}"
        );
        assert!(
            err.contains("alice"),
            "message should name the requester: {err}"
        );
    }

    #[test]
    fn f1b_external_forged_approved_by_is_rejected() {
        // approvedBy: an external approver must not stamp an identity — any
        // external approvedBy write is denied (only phase may change).
        let old = json!({ "phase": "pending-approval" });
        let new = json!({ "phase": "approved", "approvedBy": "ceo@company.com" });
        let err = migrationplan_status_write_allowed(&external_user(), TEST_SA, &old, &new)
            .expect_err("external approvedBy write must be rejected");
        assert!(
            err.contains("approvedBy"),
            "message should name approvedBy: {err}"
        );
    }

    #[test]
    fn f1b_external_phase_skip_to_completed_is_rejected() {
        // Skipping straight to a terminal phase (self-executing) is denied —
        // only pending-approval→approved is the external approval signal.
        let old = json!({ "phase": "pending-approval" });
        let new = json!({ "phase": "completed" });
        let err = migrationplan_status_write_allowed(&external_user(), TEST_SA, &old, &new)
            .expect_err("external phase→completed skip must be rejected");
        assert!(
            err.contains("pending-approval"),
            "message should reference the allowed transition: {err}"
        );
    }

    #[test]
    fn f1b_external_approve_of_already_approved_plan_is_rejected() {
        // Re-approving an already-approved plan is not the pending→approved
        // signal (old phase is not pending-approval) → rejected.
        let old = json!({ "phase": "approved" });
        let new = json!({ "phase": "approved", "approvedAt": "2026-08-07T00:00:00Z" });
        let err = migrationplan_status_write_allowed(&external_user(), TEST_SA, &old, &new)
            .expect_err("external write to an already-approved plan must be rejected");
        assert!(
            err.contains("approvedAt"),
            "message should name the offending field: {err}"
        );
    }

    #[test]
    fn f1b_external_approve_of_completed_plan_is_rejected() {
        // Approving a sealed/terminal plan is not pending→approved → rejected.
        let old = json!({ "phase": "completed" });
        let new = json!({ "phase": "approved" });
        let err = migrationplan_status_write_allowed(&external_user(), TEST_SA, &old, &new)
            .expect_err("external approve of a completed plan must be rejected");
        assert!(
            err.contains("completed"),
            "message should surface the bad transition: {err}"
        );
    }

    #[test]
    fn f1b_external_phase_to_rejected_is_denied() {
        // Rejection is platform-controller work (the FSM allows platform-scope
        // pending→rejected but that is a CONTROLLER write); an external subject
        // flipping to rejected is not the approval signal → denied here.
        let old = json!({ "phase": "pending-approval" });
        let new = json!({ "phase": "rejected" });
        assert!(
            migrationplan_status_write_allowed(&external_user(), TEST_SA, &old, &new).is_err(),
            "external phase→rejected must be denied (only pending→approved is external)"
        );
    }
}
