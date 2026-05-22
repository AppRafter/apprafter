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
//!     The mismatched sub-object is also rejected (a plan
//!     declaring `type: application` MUST NOT carry a
//!     `platform:` block — keeps the discriminator clean for
//!     trait dispatch in B.1.76).
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
//! Operates on `serde_json::Value` like the other validators
//! in this crate.

use serde_json::Value;

use crate::validator::ValidationError;

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

    let Some(spec) = obj.get("spec").and_then(Value::as_object) else {
        errors.push(ValidationError::new("spec", "spec is required"));
        return errors;
    };

    // ---- scope discriminator ----
    let scope = spec.get("scope").and_then(Value::as_object);
    let Some(scope) = scope else {
        errors.push(ValidationError::new("spec.scope", "scope is required"));
        return errors;
    };

    let scope_type = scope.get("type").and_then(Value::as_str).unwrap_or("");
    match scope_type {
        "application" => validate_application_scope(scope, &mut errors),
        "platform" => validate_platform_scope(scope, &mut errors),
        "" => {
            errors.push(ValidationError::new(
                "spec.scope.type",
                "scope.type is required (one of application|platform)",
            ));
        }
        other => {
            errors.push(ValidationError::new(
                "spec.scope.type",
                format!("scope.type must be application|platform (got {other:?})"),
            ));
        }
    }

    // ---- approver emails ----
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
        let reason = if scope_type == "application"
            && old_phase == "pending-approval"
            && new_phase == "rejected"
        {
            // ADR 0027: application-scope plans cannot be
            // rejected. Surface the specific reason so users
            // hitting this know to `git revert` instead.
            "application-scope MigrationPlans cannot be rejected; \
             revert the Git commit in your application repo and let \
             the Application reconciler supersede this plan (ADR 0027)"
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
///   ""               → approved | rejected | executing  (CREATE-with-status; allow for tooling)
///   pending-approval → approved                          (any scope; external)
///   pending-approval → rejected                          (platform scope only; external)
///   approved         → executing                         (controller)
///   executing        → executing                         (same phase, no-op; covered by guard above)
///   executing        → completed | failed                (controller)
///   sealed (completed/failed/rejected) → *               (forbidden — immutable)
/// ```
fn is_allowed_phase_transition(old_phase: &str, new_phase: &str, scope_type: &str) -> bool {
    // First-write to status.phase from an empty / absent
    // value: accept anything legal for the FSM's downstream
    // transitions. Tooling that creates a plan already in
    // `approved` is rare but not malformed.
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

fn validate_application_scope(
    scope: &serde_json::Map<String, Value>,
    errors: &mut Vec<ValidationError>,
) {
    if scope.contains_key("platform") {
        errors.push(ValidationError::new(
            "spec.scope.platform",
            "platform block must not be set when scope.type is application",
        ));
    }

    let Some(app) = scope.get("application").and_then(Value::as_object) else {
        errors.push(ValidationError::new(
            "spec.scope.application",
            "scope.application is required when scope.type is application",
        ));
        return;
    };

    let ref_ = app.get("ref").and_then(Value::as_object);
    match ref_ {
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

    let env = app.get("environment").and_then(Value::as_str).unwrap_or("");
    if env.is_empty() {
        errors.push(ValidationError::new(
            "spec.scope.application.environment",
            "environment is required",
        ));
    }
}

fn validate_platform_scope(
    scope: &serde_json::Map<String, Value>,
    errors: &mut Vec<ValidationError>,
) {
    if scope.contains_key("application") {
        errors.push(ValidationError::new(
            "spec.scope.application",
            "application block must not be set when scope.type is platform",
        ));
    }

    let Some(platform) = scope.get("platform").and_then(Value::as_object) else {
        errors.push(ValidationError::new(
            "spec.scope.platform",
            "scope.platform is required when scope.type is platform",
        ));
        return;
    };

    let components = platform.get("components").and_then(Value::as_array);
    match components {
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
    fn rejects_application_scope_missing_environment() {
        let mut obj = application_scope_object();
        obj["spec"]["scope"]["application"]
            .as_object_mut()
            .unwrap()
            .remove("environment");
        let errors = validate_migrationplan(&obj, None);
        assert!(errors
            .iter()
            .any(|e| e.field == "spec.scope.application.environment"));
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
}
