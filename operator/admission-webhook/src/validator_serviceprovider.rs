// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Pure validator for the v1alpha1 ServiceProvider object.
//!
//! Enforces what the OpenAPI v3 CRD layer expresses weakly and what
//! deserves a clearer error message:
//!
//!   - **`spec.type` is one of the closed built-in set**
//!     (`pg|jetstream|clickhouse|redis|s3|notifications|disk`). An
//!     unknown type is rejected — in v1alpha1 there is no plugin registry
//!     yet (ServiceProviderPlugin lands in Phase 7), so the set is closed.
//!   - **`spec.backend` is present, non-empty, and one of the closed set
//!     of wired provisioner backends**
//!     (`cloudnative-pg|dragonfly|disk`). A backend with no provisioner
//!     arm would leave a matched claim stalled forever, so the set is
//!     closed in v1alpha1 (extended when a backend arm is wired, mirroring
//!     the closed `type` set).
//!
//! No `kube` types — operates on `serde_json::Value` like the other
//! validators.

use serde_json::Value;

use crate::validator::ValidationError;

/// The closed built-in service-type set. Mirrors
/// `#PlatformServiceType` in `schemas/v1alpha1/types.cue`. Phase 7
/// `ServiceProviderPlugin` will extend this at runtime; until then a
/// type outside this set is an admission error.
const BUILTIN_TYPES: [&str; 7] = [
    "pg",
    "jetstream",
    "clickhouse",
    "redis",
    "s3",
    "notifications",
    "disk",
];

/// The closed set of provisioner backends with a wired arm in the
/// resourceclaim-provisioner (`Backend::from_spec_backend`). A backend
/// outside this set has no provisioner, so a matched claim would stall
/// forever — reject it at admission. Extended when a new backend arm is
/// wired (e.g. `aws-rds` for an external pg provider).
const WIRED_BACKENDS: [&str; 3] = ["cloudnative-pg", "dragonfly", "disk"];

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

    let Some(spec) = obj.get("spec").and_then(Value::as_object) else {
        errors.push(ValidationError::new("spec", "spec is required"));
        return errors;
    };

    match spec.get("type").and_then(Value::as_str) {
        Some(t) if BUILTIN_TYPES.contains(&t) => {}
        Some(t) => errors.push(ValidationError::new(
            "spec.type",
            format!(
                "type must be one of {} (got {t:?}); register a \
                 ServiceProviderPlugin to extend this set (Phase 7)",
                BUILTIN_TYPES.join("|")
            ),
        )),
        None => errors.push(ValidationError::new("spec.type", "spec.type is required")),
    }

    match spec.get("backend").and_then(Value::as_str) {
        Some(b) if WIRED_BACKENDS.contains(&b) => {}
        Some("") => errors.push(ValidationError::new(
            "spec.backend",
            "spec.backend must not be empty",
        )),
        Some(b) => errors.push(ValidationError::new(
            "spec.backend",
            format!(
                "backend must be one of {} (got {b:?}); no provisioner \
                 arm is wired for this backend",
                WIRED_BACKENDS.join("|")
            ),
        )),
        None => errors.push(ValidationError::new(
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
    fn accepts_every_wired_backend() {
        for b in WIRED_BACKENDS {
            let mut obj = valid_object();
            obj["spec"]["backend"] = json!(b);
            assert!(
                validate_serviceprovider(&obj).is_empty(),
                "wired backend {b} should be accepted"
            );
        }
    }

    #[test]
    fn accepts_disk_backend() {
        // The 2.6b `disk-local` seed declares `backend: disk`.
        let mut obj = valid_object();
        obj["spec"]["type"] = json!("disk");
        obj["spec"]["backend"] = json!("disk");
        assert!(validate_serviceprovider(&obj).is_empty());
    }

    #[test]
    fn rejects_unknown_backend() {
        let mut obj = valid_object();
        obj["spec"]["backend"] = json!("aws-rds");
        let errors = validate_serviceprovider(&obj);
        assert!(errors
            .iter()
            .any(|e| e.field == "spec.backend" && e.message.contains("aws-rds")));
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
