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
//!      — ADR 0042) non-empty; `retainUntil` parses as RFC3339. The dragonfly
//!      set is enforced only once `instance` is present + non-empty — a
//!      pre-allocation snapshot (claim deleted before provisioning) carries no
//!      allocation and the GC reclaims nothing, so it needs only the base set.
//!
//! Like `validator_resourceclaim.rs` it needs `request.userInfo` +
//! `request.operation` (+ `oldObject` for the immutability check), which
//! `server.rs` threads in for this kind.
//!
//! Typed against `operator_core::RetainedClaimSpec` (ADR 0047
//! Decision #4): the spec is deserialized into the operator-core struct
//! once and the field rules read TYPED fields, so a renamed field fails to
//! compile instead of silently bypassing a rule. The backend-conditional
//! set and the `backend` / `instance` / `volumeClaimRef` discriminators all
//! read TYPED `Option` fields. Two PRESENCE diagnostics stay on the raw
//! `Value` because the typed struct cannot represent the input they reject:
//! the `claimRef.{name,namespace}` checks (`claim_ref` / `name` / `namespace`
//! are non-`Option`, so the struct cannot model an absent `claimRef` or
//! member — the diagnostic names the exact missing leaf), and the
//! `retainUntil` presence check (`retain_until` is a non-`Option` `String`,
//! so it cannot model an absent field). Those branches are unreachable in
//! production — a validating webhook runs after the apiserver's structural
//! validation, which already enforced `required: [claimRef, provider,
//! backend, retainUntil]` and the `claimRef.{name,namespace}` requireds — and
//! exist for the unit tests / defence-in-depth.

use operator_core::RetainedClaimSpec;
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
/// GC-load-bearing set for a `dragonfly` snapshot WITH an allocation
/// (ADR 0042). The GC reads these to `FLUSHDB` the numbered DB + `ACL
/// DELUSER` the per-claim user. `dbnum` is intentionally excluded from this
/// set — it is an integer (0 is a valid DB), so the non-empty-string check
/// does not apply; its 0..1023 range is enforced by the CRD, not the webhook.
///
/// This set is only enforced once `instance` is present and non-empty (see
/// the `validate_retainedclaim` carve-out): a dragonfly claim deleted BEFORE
/// the provisioner wrote `status.instance`/`dbnum` snapshots with `instance`
/// empty, and the GC reclaims nothing for it (`dragonfly_reclaim_target`
/// returns `None` for an empty instance). Requiring the set unconditionally
/// would reject the operator's own pre-allocation snapshot CREATE
/// (failurePolicy: Fail → finalizer wedge → leak) — the mirror of the empty
/// provider/backend CNPG carve-out. ADR 0042 Pre-merge #5/#6.
const DRAGONFLY_REQUIRED_STRING_FIELDS: [&str; 4] = [
    "instance",
    "aclUser",
    "connectionSecretRef",
    "connectionSecretNamespace",
];

/// Disk-backend `spec` string fields that must be non-empty — the
/// GC-load-bearing set for a `disk` snapshot (2.6b). The GC reads these
/// to delete the unowned RWO PVC the claim provisioned.
///
/// Like the dragonfly set this is only enforced once `volumeClaimRef` is
/// present + non-empty: a disk claim deleted BEFORE the provisioner wrote
/// `status.volumeClaimRef` (the PVC was never created) snapshots with the
/// ref empty, and the GC reclaims nothing for it. Requiring the set
/// unconditionally would reject the operator's own pre-provision snapshot
/// CREATE (failurePolicy: Fail → finalizer wedge → leak) — the mirror of
/// the empty-instance dragonfly carve-out.
const DISK_REQUIRED_STRING_FIELDS: [&str; 2] = ["volumeClaimRef", "volumeClaimNamespace"];

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

    // Deserialize the spec into the typed operator-core struct. In production
    // this always succeeds — a validating webhook runs after the apiserver's
    // structural validation, which already enforced `required: [claimRef,
    // provider, backend, retainUntil]` (and `claimRef.{name,namespace}`). When
    // it succeeds the backend-conditional set + the `backend` / `instance` /
    // `volumeClaimRef` discriminators read TYPED fields, so a renamed field
    // fails to compile instead of silently bypassing a rule (ADR 0047 #4). The
    // `claimRef` / `retainUntil` PRESENCE checks stay on the raw `Value`: those
    // fields are non-`Option` in `RetainedClaimSpec`, so the typed struct
    // cannot represent an absent one (those branches are test-only).
    let typed = serde_json::from_value::<RetainedClaimSpec>(Value::Object(spec.clone())).ok();

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
    //
    // The discriminators (`backend`, `instance`, `volumeClaimRef`) read the
    // TYPED `Option` fields when the spec deserialized (always, in production);
    // a rename fails to compile. When `typed` is `None` (only reachable in
    // tests / a misconfigured apiserver) the reads fall back to the raw
    // `Value`, matching the pre-refactor `as_str()` semantics exactly.
    let backend = typed
        .as_ref()
        .map(|t| t.backend.as_str())
        .or_else(|| spec.get("backend").and_then(Value::as_str))
        .unwrap_or("");
    // A dragonfly snapshot's GC-load-bearing set (instance/aclUser/conn*) only
    // exists once the claim was actually allocated. A claim deleted before the
    // provisioner wrote `status.instance` snapshots with `instance` empty/absent
    // (dbnum 0); the GC reclaims nothing for it. Enforce the dragonfly set only
    // when `instance` is present + non-empty — otherwise fall through to the
    // base set (claimRef/provider/backend/retainUntil), mirroring the empty
    // provider/backend CNPG carve-out so the operator's own pre-allocation
    // snapshot CREATE is not rejected (failurePolicy: Fail → wedge → leak).
    // ADR 0042 Pre-merge #5/#6.
    let dragonfly_allocated =
        typed_or_raw_str(typed.as_ref(), spec, "instance").is_some_and(|s| !s.is_empty());
    // A disk snapshot's GC-load-bearing set (volumeClaimRef/Namespace) only
    // exists once the PVC was actually provisioned. A disk claim deleted
    // before the provisioner wrote `status.volumeClaimRef` snapshots with the
    // ref empty; the GC reclaims nothing for it. Enforce the disk set only when
    // `volumeClaimRef` is present + non-empty (mirror of the dragonfly carve-out).
    let disk_provisioned =
        typed_or_raw_str(typed.as_ref(), spec, "volumeClaimRef").is_some_and(|s| !s.is_empty());
    let required_fields: &[&str] = if backend == "dragonfly" {
        if dragonfly_allocated {
            &DRAGONFLY_REQUIRED_STRING_FIELDS
        } else {
            &[]
        }
    } else if backend == "disk" {
        if disk_provisioned {
            &DISK_REQUIRED_STRING_FIELDS
        } else {
            &[]
        }
    } else {
        &CNPG_REQUIRED_STRING_FIELDS
    };
    for field in required_fields {
        // `typed_or_raw_str` reads the TYPED `Option<String>` for this field
        // (gating its rename) and only falls back to the raw `Value` when the
        // spec failed to deserialize (test / misconfigured-apiserver path).
        if typed_or_raw_str(typed.as_ref(), spec, field).is_none_or(str::is_empty) {
            errors.push(ValidationError::new(
                format!("spec.{field}"),
                format!("spec.{field} is required"),
            ));
        }
    }

    // retainUntil must parse as RFC3339. The typed `retain_until` drives the
    // parse on the happy path (gating its rename); the empty/absent "is
    // required" branch stays on the raw `Value` because the non-`Option`
    // `String` cannot represent an absent field (test-only).
    let retain_until = typed
        .as_ref()
        .map(|t| t.retain_until.as_str())
        .or_else(|| spec.get("retainUntil").and_then(Value::as_str));
    match retain_until {
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

/// Read the backend-conditional `spec` field `name` from the TYPED
/// `RetainedClaimSpec` when it deserialized (gating a rename at compile
/// time), falling back to the raw `Value` when it did not (only reachable
/// in tests / a misconfigured apiserver — production always deserializes).
/// The match is exhaustive over every field name this validator reads; a
/// rename in `RetainedClaimSpec` makes the corresponding arm fail to
/// compile. Returns `None` for an absent field, matching the pre-refactor
/// `spec.get(name).and_then(Value::as_str)` semantics.
fn typed_or_raw_str<'a>(
    typed: Option<&'a RetainedClaimSpec>,
    spec: &'a serde_json::Map<String, Value>,
    name: &str,
) -> Option<&'a str> {
    if let Some(t) = typed {
        return match name {
            // CNPG set.
            "cnpgCluster" => t.cnpg_cluster.as_deref(),
            "cnpgNamespace" => t.cnpg_namespace.as_deref(),
            "role" => t.role.as_deref(),
            "database" => t.database.as_deref(),
            "databaseObjectName" => t.database_object_name.as_deref(),
            "passwordSecretName" => t.password_secret_name.as_deref(),
            // Dragonfly set + discriminator.
            "instance" => t.instance.as_deref(),
            "aclUser" => t.acl_user.as_deref(),
            "connectionSecretRef" => t.connection_secret_ref.as_deref(),
            "connectionSecretNamespace" => t.connection_secret_namespace.as_deref(),
            // Disk set + discriminator.
            "volumeClaimRef" => t.volume_claim_ref.as_deref(),
            "volumeClaimNamespace" => t.volume_claim_namespace.as_deref(),
            other => unreachable!("typed_or_raw_str: unmapped field {other:?}"),
        };
    }
    spec.get(name).and_then(Value::as_str)
}

fn is_operator_or_admin(user_info: &Value) -> bool {
    let is_operator = user_info.get("username").and_then(Value::as_str) == Some(OPERATOR_SA);
    // Cluster-admin break-glass. `system:masters` is the classic admin group;
    // kubeadm >= 1.29 issues admin.conf under `kubeadm:cluster-admins`
    // instead (k8s 1.35 nodes, e.g. kind, surface that), so accept both —
    // either group is already omnipotent on the cluster.
    let is_admin = user_info
        .get("groups")
        .and_then(Value::as_array)
        .is_some_and(|groups| {
            groups.iter().any(|g| {
                matches!(
                    g.as_str(),
                    Some("system:masters" | "kubeadm:cluster-admins")
                )
            })
        });
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
    fn allows_kubeadm_cluster_admins_create() {
        // kubeadm >= 1.29 (k8s 1.35 / kind) issues admin.conf under
        // `kubeadm:cluster-admins` rather than `system:masters`; break-glass
        // CREATE must still work for the actual cluster-admin group.
        let kubeadm_admin = json!({
            "username": "kubernetes-admin",
            "groups": ["kubeadm:cluster-admins", "system:authenticated"]
        });
        assert!(validate_retainedclaim(&retained(), None, &kubeadm_admin, "CREATE").is_empty());
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
        // A dragonfly snapshot WITH an allocation (non-empty instance) must
        // still carry its own GC-load-bearing fields (instance + aclUser);
        // the GC needs them to FLUSHDB + DELUSER. Once `instance` is present
        // the full set is enforced, so a missing `aclUser` is still rejected.
        let mut c = retained_dragonfly();
        c["spec"].as_object_mut().unwrap().remove("aclUser");
        let errors = validate_retainedclaim(&c, None, &operator_user(), "CREATE");
        assert!(
            errors.iter().any(|e| e.field == "spec.aclUser"),
            "an allocated dragonfly snapshot missing aclUser must be rejected; got {errors:?}"
        );
    }

    #[test]
    fn allows_dragonfly_pre_allocation_snapshot_with_empty_instance() {
        // 2.6 Fix #5/#6: a dragonfly claim deleted BEFORE the provisioner
        // wrote `status.instance`/`dbnum` snapshots with `instance` empty
        // (and `dbnum` 0). The snapshot writer ALWAYS sets aclUser /
        // connectionSecretRef / connectionSecretNamespace, but `instance` is
        // empty — there is no allocation, so the GC reclaims nothing
        // (`dragonfly_reclaim_target` returns None for an empty instance).
        // The webhook (failurePolicy: Fail) MUST accept it — otherwise the
        // operator's own snapshot CREATE is rejected, the finalizer wedges,
        // and the claim leaks. With no allocation only the BASE set is
        // required (claimRef / provider / backend / retainUntil). ADR 0042.
        let mut c = retained_dragonfly();
        c["spec"]["instance"] = json!("");
        c["spec"]["dbnum"] = json!(0);
        let errors = validate_retainedclaim(&c, None, &operator_user(), "CREATE");
        assert!(
            errors.is_empty(),
            "pre-allocation dragonfly snapshot (empty instance) must pass; got {errors:?}"
        );
    }

    #[test]
    fn allows_dragonfly_pre_allocation_snapshot_with_absent_allocation() {
        // Same pre-allocation case, but with the allocation keys ABSENT
        // (not just empty-string) — also accepted, base set only.
        let mut c = retained_dragonfly();
        let spec = c["spec"].as_object_mut().unwrap();
        spec.remove("instance");
        spec.remove("dbnum");
        spec.remove("aclUser");
        let errors = validate_retainedclaim(&c, None, &operator_user(), "CREATE");
        assert!(
            errors.is_empty(),
            "pre-allocation dragonfly snapshot (absent allocation) must pass; got {errors:?}"
        );
    }

    fn retained_disk() -> Value {
        json!({
            "metadata": { "name": "web-disk-data", "namespace": "apprafter-system" },
            "spec": {
                "claimRef": { "name": "web-disk-data", "namespace": "demo" },
                "provider": "disk-local",
                "backend": "disk",
                "volumeClaimRef": "claim-demo-web-disk-data",
                "volumeClaimNamespace": "demo",
                "retainUntil": "2026-06-13T00:00:00+00:00"
            }
        })
    }

    #[test]
    fn allows_operator_create_disk_snapshot_without_cnpg_fields() {
        // 2.6b: a disk snapshot carries NONE of the cnpg-required fields
        // (role/database/cnpgCluster/...) — only volumeClaimRef/Namespace.
        // The webhook MUST accept it (failurePolicy: Fail) — otherwise the
        // operator's own disk snapshot CREATE is rejected, the finalizer
        // wedges, and the PVC leaks (the leak-wedge guard, generalised by
        // backend like the dragonfly arm).
        let errors = validate_retainedclaim(&retained_disk(), None, &operator_user(), "CREATE");
        assert!(
            errors.is_empty(),
            "disk snapshot must pass the webhook; got {errors:?}"
        );
    }

    #[test]
    fn rejects_disk_snapshot_missing_volume_claim_namespace() {
        // A disk snapshot WITH a provisioned PVC (non-empty volumeClaimRef)
        // must still carry its namespace; the GC needs both to delete the PVC.
        let mut c = retained_disk();
        c["spec"]
            .as_object_mut()
            .unwrap()
            .remove("volumeClaimNamespace");
        let errors = validate_retainedclaim(&c, None, &operator_user(), "CREATE");
        assert!(
            errors
                .iter()
                .any(|e| e.field == "spec.volumeClaimNamespace"),
            "a provisioned disk snapshot missing volumeClaimNamespace must be rejected; got {errors:?}"
        );
    }

    #[test]
    fn allows_disk_pre_provision_snapshot_with_empty_volume_claim_ref() {
        // A disk claim deleted BEFORE the provisioner created the PVC
        // snapshots with `volumeClaimRef` empty — there is no PVC, so the GC
        // reclaims nothing. The webhook (failurePolicy: Fail) MUST accept it;
        // otherwise the finalizer wedges. With no PVC only the BASE set is
        // required (claimRef / provider / backend / retainUntil).
        let mut c = retained_disk();
        c["spec"]["volumeClaimRef"] = json!("");
        c["spec"]["volumeClaimNamespace"] = json!("");
        let errors = validate_retainedclaim(&c, None, &operator_user(), "CREATE");
        assert!(
            errors.is_empty(),
            "pre-provision disk snapshot (empty volumeClaimRef) must pass; got {errors:?}"
        );
    }

    #[test]
    fn allows_disk_pre_provision_snapshot_with_absent_volume_claim_ref() {
        // Same pre-provision case, but with the disk keys ABSENT (not just
        // empty-string) — also accepted, base set only.
        let mut c = retained_disk();
        let spec = c["spec"].as_object_mut().unwrap();
        spec.remove("volumeClaimRef");
        spec.remove("volumeClaimNamespace");
        let errors = validate_retainedclaim(&c, None, &operator_user(), "CREATE");
        assert!(
            errors.is_empty(),
            "pre-provision disk snapshot (absent volumeClaimRef) must pass; got {errors:?}"
        );
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
