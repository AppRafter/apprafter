// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Pure validator for the v1alpha1 ServiceProvider object.
//!
//! Enforces what the OpenAPI v3 CRD layer expresses weakly and what
//! deserves a clearer error message:
//!
//!   - **`spec.type` is one of the closed built-in set**
//!     (`pg|jetstream|clickhouse|redis|s3|notifications|disk|shared-disk`). An
//!     unknown type is rejected — in v1alpha1 there is no plugin registry
//!     yet (ServiceProviderPlugin lands in Phase 7), so the set is closed.
//!   - **`spec.backend` is present and non-empty.** The backend set is
//!     intentionally OPEN — an external backend such as `aws-rds` plugs in
//!     here (community providers load as gRPC sidecars in Phase 7), so we
//!     reject only an empty/missing backend, not an unrecognised one.
//!
//! Typed against `operator_core::ServiceProviderSpec` (ADR 0047
//! Decision #4): the spec is deserialized into the operator-core struct
//! once and the value rules read TYPED fields, so a renamed field fails
//! to compile rather than silently bypassing a rule. The presence
//! ("required") diagnostics stay on the raw `Value` — the typed struct's
//! non-`Option` `type`/`backend` cannot represent an absent field, and a
//! non-conforming object never reaches admission in production (a
//! validating webhook runs after the apiserver's structural validation,
//! which already enforces `required: [type, backend]`); these branches
//! exist only for defence-in-depth and the unit tests below.

use operator_core::ServiceProviderSpec;
use serde_json::Value;

use crate::validator::ValidationError;

/// The closed built-in service-type set. Mirrors
/// `#PlatformServiceType` in `schemas/v1alpha1/types.cue`. Phase 7
/// `ServiceProviderPlugin` will extend this at runtime; until then a
/// type outside this set is an admission error.
const BUILTIN_TYPES: [&str; 8] = [
    "pg",
    "jetstream",
    "clickhouse",
    "redis",
    "s3",
    "notifications",
    "disk",
    "shared-disk",
];

/// Validate a ServiceProvider AdmissionReview object. The caller
/// (server.rs) passes the full `request.object`. Returns every error
/// found; empty Vec = valid.
pub fn validate_serviceprovider(object: &Value) -> Vec<ValidationError> {
    let mut errors = Vec::new();

    let Some(obj) = object.as_object() else {
        errors.push(ValidationError::new(
            "object",
            "ServiceProvider object must be a JSON object",
        ));
        return errors;
    };

    let Some(spec_value) = obj.get("spec").filter(|s| s.is_object()) else {
        errors.push(ValidationError::new("spec", "spec is required"));
        return errors;
    };

    // Deserialize the spec into the typed operator-core struct. In
    // production this always succeeds — a validating webhook runs after the
    // apiserver's structural validation, which already enforced
    // `required: [type, backend]` and `type/backend: string`. When it does
    // succeed we read the TYPED `type_`/`backend` fields, so a renamed field
    // fails to compile instead of silently bypassing a rule (ADR 0047 #4).
    //
    // `type`/`backend` are non-`Option` in `ServiceProviderSpec`, so the
    // typed struct cannot represent an *absent* field; the per-field
    // presence ("is required") branches below therefore stay on the raw
    // `Value`, matching the pre-refactor `as_str()` semantics exactly (a
    // field that is not a usable string is treated as missing). They are
    // only reachable in tests / a misconfigured apiserver.
    let typed = serde_json::from_value::<ServiceProviderSpec>(spec_value.clone()).ok();
    let spec = spec_value.as_object().expect("filtered to an object above");

    match (typed.as_ref(), spec.get("type").and_then(Value::as_str)) {
        // Happy path: read the typed field so the compiler gates `type_`.
        (Some(t), Some(_)) if BUILTIN_TYPES.contains(&t.type_.as_str()) => {}
        (_, Some(t)) if BUILTIN_TYPES.contains(&t) => {}
        (_, Some(t)) => errors.push(ValidationError::new(
            "spec.type",
            format!(
                "type must be one of {} (got {t:?}); register a \
                 ServiceProviderPlugin to extend this set (Phase 7)",
                BUILTIN_TYPES.join("|")
            ),
        )),
        (_, None) => errors.push(ValidationError::new("spec.type", "spec.type is required")),
    }

    match (typed.as_ref(), spec.get("backend").and_then(Value::as_str)) {
        // Happy path: read the typed field so the compiler gates `backend`.
        (Some(t), Some(_)) if !t.backend.is_empty() => {}
        (_, Some(b)) if !b.is_empty() => {}
        (_, Some(_)) => errors.push(ValidationError::new(
            "spec.backend",
            "spec.backend must not be empty",
        )),
        (_, None) => errors.push(ValidationError::new(
            "spec.backend",
            "spec.backend is required",
        )),
    }

    errors
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn valid_object() -> Value {
        json!({
            "metadata": { "name": "pg-integrated", "namespace": "apprafter-system" },
            "spec": { "type": "pg", "backend": "cloudnative-pg" }
        })
    }

    #[test]
    fn accepts_valid_provider() {
        assert!(validate_serviceprovider(&valid_object()).is_empty());
    }

    #[test]
    fn accepts_every_builtin_type() {
        for t in BUILTIN_TYPES {
            let mut obj = valid_object();
            obj["spec"]["type"] = json!(t);
            assert!(
                validate_serviceprovider(&obj).is_empty(),
                "builtin type {t} should be accepted"
            );
        }
    }

    #[test]
    fn rejects_unknown_type() {
        let mut obj = valid_object();
        obj["spec"]["type"] = json!("kafka");
        let errors = validate_serviceprovider(&obj);
        assert!(errors
            .iter()
            .any(|e| e.field == "spec.type" && e.message.contains("kafka")));
    }

    #[test]
    fn rejects_missing_type() {
        let mut obj = valid_object();
        obj["spec"].as_object_mut().unwrap().remove("type");
        let errors = validate_serviceprovider(&obj);
        assert!(errors
            .iter()
            .any(|e| e.field == "spec.type" && e.message.contains("required")));
    }

    #[test]
    fn rejects_empty_backend() {
        let mut obj = valid_object();
        obj["spec"]["backend"] = json!("");
        let errors = validate_serviceprovider(&obj);
        assert!(errors.iter().any(|e| e.field == "spec.backend"));
    }

    #[test]
    fn rejects_missing_backend() {
        let mut obj = valid_object();
        obj["spec"].as_object_mut().unwrap().remove("backend");
        let errors = validate_serviceprovider(&obj);
        assert!(errors.iter().any(|e| e.field == "spec.backend"));
    }

    #[test]
    fn accepts_disk_backend() {
        // The 2.6b `disk-local` seed declares `backend: disk` — accepted
        // as any other non-empty backend string.
        let mut obj = valid_object();
        obj["spec"]["type"] = json!("disk");
        obj["spec"]["backend"] = json!("disk");
        assert!(validate_serviceprovider(&obj).is_empty());
    }

    #[test]
    fn accepts_shared_disk_type() {
        // Regression test for the 2.6c walk-found bug: the `shared-local`
        // ServiceProvider seed uses `type: shared-disk` but the enum in
        // the CRD + BUILTIN_TYPES did not include it, causing the apiserver
        // to reject the seed with "Unsupported value: shared-disk".
        let mut obj = valid_object();
        obj["spec"]["type"] = json!("shared-disk");
        obj["spec"]["backend"] = json!("shared-disk");
        assert!(
            validate_serviceprovider(&obj).is_empty(),
            "shared-disk must be accepted as a built-in ServiceProvider type"
        );
    }

    #[test]
    fn accepts_open_external_backend() {
        // The backend set is OPEN: an external backend such as `aws-rds`
        // (community providers, Phase 7) is a valid non-empty backend and
        // must NOT be rejected.
        let mut obj = valid_object();
        obj["spec"]["backend"] = json!("aws-rds");
        assert!(validate_serviceprovider(&obj).is_empty());
    }

    #[test]
    fn rejects_missing_spec() {
        let obj = json!({ "metadata": { "name": "pg-integrated" } });
        let errors = validate_serviceprovider(&obj);
        assert!(errors.iter().any(|e| e.field == "spec"));
    }

    #[test]
    fn aggregates_unknown_type_and_empty_backend() {
        let obj = json!({
            "metadata": { "name": "x", "namespace": "apprafter-system" },
            "spec": { "type": "kafka", "backend": "" }
        });
        let errors = validate_serviceprovider(&obj);
        assert!(errors.len() >= 2);
        assert!(errors.iter().any(|e| e.field == "spec.type"));
        assert!(errors.iter().any(|e| e.field == "spec.backend"));
    }
}
