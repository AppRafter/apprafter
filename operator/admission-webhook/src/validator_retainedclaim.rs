// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Pure validator for the v1alpha1 RetainedClaim object (Phase 2.4f).
//!
//! Three responsibilities:
//!
//!   1. **Operator-only CREATE.** RetainedClaims are written by the
//!      `resourceclaim-provisioner` finalizer when a `ResourceClaim` is
//!      deleted; users must never hand-author them. On `CREATE` the
//!      requester must be the operator ServiceAccount OR a
//!      `system:masters` member (cluster-admin break-glass). Mirrors
//!      `validator_resourceclaim.rs`.
//!   2. **Spec immutability on UPDATE.** When `oldObject` is present
//!      (UPDATE), reject any change to `spec` — the snapshot is a fixed
//!      record of a deleted claim (the CRD layers the same CEL rule;
//!      the GC's only legitimate write is the terminal `delete`, which
//!      is not an UPDATE). UPDATE is NOT identity-gated (the operator
//!      patches finalizers etc. under its own SA; the immutability guard
//!      is what protects the spec).
//!   3. **Field validation** (always, regardless of identity / op):
//!      `claimRef.{name,namespace}` non-empty + a BACKEND-CONDITIONAL
//!      GC-load-bearing set (CNPG: role / database / databaseObjectName /
//!      passwordSecretName / cnpgCluster / cnpgNamespace; dragonfly:
//!      instance / aclUser / connectionSecretRef / connectionSecretNamespace
//!      — ADR 0042) non-empty; `retainUntil` parses as RFC3339.
//!
//! Like `validator_resourceclaim.rs` it needs `request.userInfo` +
//! `request.operation` (+ `oldObject` for the immutability check), which
//! `server.rs` threads in for this kind.

use serde_json::Value;

use crate::validator::ValidationError;

/// The operator's ServiceAccount username (default Helm release name
/// `apprafter-operator` in namespace `apprafter-system`). Duplicated per
/// the current per-validator style (matches `validator_resourceclaim.rs`).
const OPERATOR_SA: &str = "system:serviceaccount:apprafter-system:apprafter-operator";

/// CNPG-backend `spec` string fields that must be non-empty — the
/// GC-load-bearing set for a `cloudnative-pg` snapshot.
///
/// `provider` and `backend` are DELIBERATELY excluded: they are best-effort
/// lineage/audit fields, and the provisioner finalizer legitimately writes
/// them empty when the claim was never scheduled or its ServiceProvider was
/// deleted before teardown. Requiring them non-empty would make the
/// operator's own snapshot CREATE fail the webhook (failurePolicy: Fail),
/// wedging the finalizer and leaking the role/DB it was trying to retain —
/// the opposite of this feature's purpose. The GC consumes only the fields
/// below (all derived deterministically with fallbacks, so never empty).
const CNPG_REQUIRED_STRING_FIELDS: [&str; 6] = [
    "cnpgCluster",
    "cnpgNamespace",
    "role",
    "database",
    "databaseObjectName",
    "passwordSecretName",
];

/// Dragonfly-backend `spec` string fields that must be non-empty — the
/// GC-load-bearing set for a `dragonfly` snapshot (ADR 0042). The GC reads
/// these to `FLUSHDB` the numbered DB + `ACL DELUSER` the per-claim user.
/// `dbnum` is intentionally excluded from this set — it is an integer
/// (0 is a valid DB), so the non-empty-string check does not apply; its
/// 0..1023 range is enforced by the CRD, not the webhook.
const DRAGONFLY_REQUIRED_STRING_FIELDS: [&str; 4] = [
    "instance",
    "aclUser",
    "connectionSecretRef",
    "connectionSecretNamespace",
];

/// Validate a RetainedClaim AdmissionReview. `object` is the request's
/// `object`; `old_object` is `oldObject` (`None` on CREATE, `Some` on
/// UPDATE); `user_info` is `request.userInfo`; `operation` is
/// `request.operation`. Empty/missing userInfo on a CREATE fails closed.
pub fn validate_retainedclaim(
    object: &Value,
    old_object: Option<&Value>,
    user_info: &Value,
    operation: &str,
) -> Vec<ValidationError> {
    let mut errors = Vec::new();

    // 1. Identity gate — CREATE only.
    if operation == "CREATE" && !is_operator_or_admin(user_info) {
        let username = user_info
            .get("username")
            .and_then(Value::as_str)
            .unwrap_or("<unknown>");
        errors.push(ValidationError::new(
            "metadata",
            format!(
                "RetainedClaim objects may only be created by the apprafter-operator; \
                 they are snapshotted automatically when a ResourceClaim is deleted \
                 (requester {username:?} denied)"
            ),
        ));
    }

    // 2. Spec immutability — UPDATE only.
    if let Some(old) = old_object {
        let old_spec = old.pointer("/spec");
        let new_spec = object.pointer("/spec");
        if old_spec.is_some() && old_spec != new_spec {
            errors.push(ValidationError::new(
                "spec",
                "RetainedClaim spec is immutable",
            ));
        }
    }

    // 3. Field validations — always.
    let Some(obj) = object.as_object() else {
        errors.push(ValidationError::new(
            "object",
            "RetainedClaim object must be a JSON object",
        ));
        return errors;
    };
    let Some(spec) = obj.get("spec").and_then(Value::as_object) else {
        errors.push(ValidationError::new("spec", "spec is required"));
        return errors;
    };

    // claimRef.{name, namespace}
    match spec.get("claimRef").and_then(Value::as_object) {
        Some(claim_ref) => {
            for key in ["name", "namespace"] {
                if claim_ref
                    .get(key)
                    .and_then(Value::as_str)
                    .is_none_or(str::is_empty)
                {
                    errors.push(ValidationError::new(
                        format!("spec.claimRef.{key}"),
                        format!("claimRef.{key} is required"),
                    ));
                }
            }
        }
        None => errors.push(ValidationError::new(
            "spec.claimRef",
            "spec.claimRef is required (the deleted claim's name + namespace)",
        )),
    }

    // Backend-conditional non-empty string fields. The required set
    // depends on `spec.backend`: a CNPG snapshot carries role/database/
    // cnpg* (none of the dragonfly fields); a dragonfly snapshot carries
    // instance/aclUser/connectionSecret* (none of the cnpg fields). An
    // absent/empty backend defaults to CNPG — legacy snapshots predate the
    // dragonfly arm and are always CNPG-shaped. Validating only the
    // matching set keeps the operator's own snapshot CREATE from being
    // rejected (failurePolicy: Fail → finalizer wedge → leak). ADR 0042.
    let backend = spec.get("backend").and_then(Value::as_str).unwrap_or("");
    let required_fields: &[&str] = if backend == "dragonfly" {
        &DRAGONFLY_REQUIRED_STRING_FIELDS
    } else {
        &CNPG_REQUIRED_STRING_FIELDS
    };
    for field in required_fields {
        if spec
            .get(*field)
            .and_then(Value::as_str)
            .is_none_or(str::is_empty)
        {
            errors.push(ValidationError::new(
                format!("spec.{field}"),
                format!("spec.{field} is required"),
            ));
        }
    }

    // retainUntil must parse as RFC3339.
    match spec.get("retainUntil").and_then(Value::as_str) {
        Some(s) if !s.is_empty() => {
            if chrono::DateTime::parse_from_rfc3339(s).is_err() {
                errors.push(ValidationError::new(
                    "spec.retainUntil",
                    format!("retainUntil must be an RFC3339 timestamp (got {s:?})"),
                ));
            }
        }
        _ => errors.push(ValidationError::new(
            "spec.retainUntil",
            "spec.retainUntil is required (RFC3339)",
        )),
    }

    errors
}

fn is_operator_or_admin(user_info: &Value) -> bool {
    let is_operator = user_info.get("username").and_then(Value::as_str) == Some(OPERATOR_SA);
    let is_admin = user_info
        .get("groups")
        .and_then(Value::as_array)
        .is_some_and(|groups| groups.iter().any(|g| g.as_str() == Some("system:masters")));
    is_operator || is_admin
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn retained() -> Value {
        json!({
            "metadata": { "name": "claim-demo-demo-web-pg", "namespace": "apprafter-system" },
            "spec": {
                "claimRef": { "name": "demo-web-pg", "namespace": "demo" },
                "provider": "pg-integrated",
                "backend": "cloudnative-pg",
                "cnpgCluster": "platform-postgres",
                "cnpgNamespace": "cnpg-system",
                "role": "claim_demo_demo_web_pg",
                "database": "claim_demo_demo_web_pg",
                "databaseObjectName": "claim-demo-demo-web-pg",
                "passwordSecretName": "claim-demo-demo-web-pg-pw",
                "retainUntil": "2026-06-10T00:00:00+00:00"
            }
        })
    }
    fn operator_user() -> Value {
        json!({ "username": OPERATOR_SA, "groups": ["system:serviceaccounts"] })
    }
    fn admin_user() -> Value {
        json!({ "username": "kubernetes-admin", "groups": ["system:masters", "system:authenticated"] })
    }
    fn normal_user() -> Value {
        json!({ "username": "alice", "groups": ["system:authenticated"] })
    }

    #[test]
    fn allows_operator_create() {
        assert!(validate_retainedclaim(&retained(), None, &operator_user(), "CREATE").is_empty());
    }

    #[test]
    fn allows_operator_create_with_empty_provider_and_backend() {
        // Leak-wedge guard: when a claim was never scheduled (or its
        // ServiceProvider was deleted before teardown), the finalizer
        // snapshots with empty provider/backend. The webhook MUST accept it
        // (failurePolicy: Fail) — otherwise the operator's own snapshot
        // CREATE is rejected, the finalizer wedges, and the role/DB leak.
        let mut c = retained();
        c["spec"]["provider"] = json!("");
        c["spec"]["backend"] = json!("");
        assert!(validate_retainedclaim(&c, None, &operator_user(), "CREATE").is_empty());
    }

    #[test]
    fn allows_system_masters_create() {
        assert!(validate_retainedclaim(&retained(), None, &admin_user(), "CREATE").is_empty());
    }

    #[test]
    fn rejects_user_create() {
        let errors = validate_retainedclaim(&retained(), None, &normal_user(), "CREATE");
        assert!(errors
            .iter()
            .any(|e| e.field == "metadata" && e.message.contains("apprafter-operator")));
        assert!(errors.iter().any(|e| e.message.contains("alice")));
    }

    #[test]
    fn rejects_create_with_empty_userinfo_fails_closed() {
        let errors = validate_retainedclaim(&retained(), None, &json!({}), "CREATE");
        assert!(errors.iter().any(|e| e.field == "metadata"));
    }

    #[test]
    fn allows_update_when_spec_unchanged() {
        // UPDATE with identical spec (e.g. a metadata/label tweak)
        // passes — and is not identity-gated.
        let old = retained();
        let new = retained();
        assert!(validate_retainedclaim(&new, Some(&old), &normal_user(), "UPDATE").is_empty());
    }

    #[test]
    fn rejects_spec_mutation_on_update() {
        let old = retained();
        let mut new = retained();
        new["spec"]["retainUntil"] = json!("2026-12-31T00:00:00+00:00");
        let errors = validate_retainedclaim(&new, Some(&old), &normal_user(), "UPDATE");
        assert!(
            errors
                .iter()
                .any(|e| e.field == "spec" && e.message.contains("immutable")),
            "spec mutation must be rejected as immutable; got {errors:?}"
        );
    }

    #[test]
    fn rejects_claim_ref_mutation_on_update() {
        let old = retained();
        let mut new = retained();
        new["spec"]["claimRef"]["name"] = json!("renamed");
        let errors = validate_retainedclaim(&new, Some(&old), &operator_user(), "UPDATE");
        assert!(errors.iter().any(|e| e.field == "spec"));
    }

    #[test]
    fn rejects_non_rfc3339_retain_until() {
        let mut c = retained();
        c["spec"]["retainUntil"] = json!("not-a-timestamp");
        let errors = validate_retainedclaim(&c, None, &operator_user(), "CREATE");
        assert!(errors.iter().any(|e| e.field == "spec.retainUntil"));
    }

    #[test]
    fn rejects_missing_required_field() {
        let mut c = retained();
        c["spec"].as_object_mut().unwrap().remove("role");
        let errors = validate_retainedclaim(&c, None, &operator_user(), "CREATE");
        assert!(errors.iter().any(|e| e.field == "spec.role"));
    }

    #[test]
    fn rejects_missing_claim_ref() {
        let mut c = retained();
        c["spec"].as_object_mut().unwrap().remove("claimRef");
        let errors = validate_retainedclaim(&c, None, &operator_user(), "CREATE");
        assert!(errors.iter().any(|e| e.field == "spec.claimRef"));
    }

    #[test]
    fn rejects_empty_claim_ref_namespace() {
        let mut c = retained();
        c["spec"]["claimRef"]["namespace"] = json!("");
        let errors = validate_retainedclaim(&c, None, &operator_user(), "CREATE");
        assert!(errors.iter().any(|e| e.field == "spec.claimRef.namespace"));
    }

    #[test]
    fn rejects_missing_password_secret_name() {
        let mut c = retained();
        c["spec"]
            .as_object_mut()
            .unwrap()
            .remove("passwordSecretName");
        let errors = validate_retainedclaim(&c, None, &operator_user(), "CREATE");
        assert!(errors.iter().any(|e| e.field == "spec.passwordSecretName"));
    }

    fn retained_dragonfly() -> Value {
        json!({
            "metadata": { "name": "web-redis", "namespace": "apprafter-system" },
            "spec": {
                "claimRef": { "name": "web-redis", "namespace": "demo" },
                "provider": "redis-integrated",
                "backend": "dragonfly",
                "instance": "platform-redis-persistent-000",
                "dbnum": 7,
                "aclUser": "claim_demo_web_redis",
                "connectionSecretRef": "web-redis-conn",
                "connectionSecretNamespace": "demo",
                "retainUntil": "2026-06-12T00:00:00+00:00"
            }
        })
    }

    #[test]
    fn allows_operator_create_dragonfly_snapshot_without_cnpg_fields() {
        // A dragonfly snapshot carries NONE of the cnpg-required fields
        // (role/database/cnpgCluster/...). The webhook MUST accept it
        // (failurePolicy: Fail) — otherwise the operator's own dragonfly
        // snapshot CREATE is rejected, the finalizer wedges, and the
        // Dragonfly DB + ACL user leak (the same leak-wedge guard as for
        // empty provider/backend, generalised by backend). ADR 0042.
        let errors =
            validate_retainedclaim(&retained_dragonfly(), None, &operator_user(), "CREATE");
        assert!(
            errors.is_empty(),
            "dragonfly snapshot must pass the webhook; got {errors:?}"
        );
    }

    #[test]
    fn rejects_dragonfly_snapshot_missing_allocation_fields() {
        // A dragonfly snapshot must still carry its own GC-load-bearing
        // fields (instance + aclUser); the GC needs them to FLUSHDB + DELUSER.
        let mut c = retained_dragonfly();
        c["spec"].as_object_mut().unwrap().remove("instance");
        c["spec"].as_object_mut().unwrap().remove("aclUser");
        let errors = validate_retainedclaim(&c, None, &operator_user(), "CREATE");
        assert!(errors.iter().any(|e| e.field == "spec.instance"));
        assert!(errors.iter().any(|e| e.field == "spec.aclUser"));
    }

    #[test]
    fn rejects_missing_spec() {
        let obj = json!({ "metadata": { "name": "x", "namespace": "apprafter-system" } });
        let errors = validate_retainedclaim(&obj, None, &operator_user(), "CREATE");
        assert!(errors.iter().any(|e| e.field == "spec"));
    }

    #[test]
    fn user_create_with_bad_field_reports_both_identity_and_field() {
        let mut c = retained();
        c["spec"].as_object_mut().unwrap().remove("role");
        let errors = validate_retainedclaim(&c, None, &normal_user(), "CREATE");
        assert!(errors.iter().any(|e| e.field == "metadata"));
        assert!(errors.iter().any(|e| e.field == "spec.role"));
    }
}
