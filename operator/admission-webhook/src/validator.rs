// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Pure validator for the v1alpha1 Application spec.
//!
//! Enforces v1alpha1 invariants the OpenAPI v3 CRD layer can't
//! express: image must be reachable through `base.image` OR through
//! every `environments[*].image` (cross-field; CUE itself accepts
//! any string for `image`, so non-empty is enforced here plus by
//! the CRD's `pattern: "^.+$"`), environment names are DNS-1123
//! labels, env keys match `^[A-Z_][A-Z0-9_]*$`, and `needs` keys
//! are known platform-service types.
//!
//! No `kube` types are pulled in — the validator works directly on
//! `serde_json::Value`. The HTTP layer (`server.rs`) extracts the
//! `request.object.spec` value before passing it here.

use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationError {
    pub field: String,
    pub message: String,
}

impl ValidationError {
    pub fn new(field: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            field: field.into(),
            message: message.into(),
        }
    }
}

/// Validate the `spec` block of a v1alpha1 Application. Returns
/// every error found (the validator does not short-circuit). An
/// empty `Vec` means the manifest is valid.
pub fn validate_application_spec(spec: &Value) -> Vec<ValidationError> {
    let mut errors = Vec::new();

    let Some(obj) = spec.as_object() else {
        errors.push(ValidationError::new("spec", "spec must be a JSON object"));
        return errors;
    };

    let base = obj.get("base").and_then(|v| v.as_object());
    let envs = obj.get("environments").and_then(|v| v.as_object());

    let base_image_set = base
        .and_then(|b| b.get("image"))
        .and_then(|v| v.as_str())
        .is_some_and(|s| !s.is_empty());

    if !base_image_set {
        match envs {
            None => errors.push(ValidationError::new(
                "spec.base.image",
                "spec.base.image is unset; either set it, or declare at least one entry under spec.environments with image set",
            )),
            Some(envs_obj) if envs_obj.is_empty() => errors.push(ValidationError::new(
                "spec.base.image",
                "spec.base.image is unset and spec.environments is empty; nothing to deploy",
            )),
            Some(envs_obj) => {
                for (name, val) in envs_obj {
                    let env_image_set = val
                        .as_object()
                        .and_then(|o| o.get("image"))
                        .and_then(|v| v.as_str())
                        .is_some_and(|s| !s.is_empty());
                    if !env_image_set {
                        errors.push(ValidationError::new(
                            format!("spec.environments.{name}.image"),
                            "spec.base.image is unset, so every spec.environments[*].image must be set",
                        ));
                    }
                }
            }
        }
    }

    if let Some(envs_obj) = envs {
        for name in envs_obj.keys() {
            if !is_dns_1123_label(name) {
                errors.push(ValidationError::new(
                    format!("spec.environments.{name}"),
                    format!(
                        "environment name {name:?} must be a DNS-1123 label (lowercase alphanumeric + '-', 1..=63 chars, start and end alphanumeric)"
                    ),
                ));
            }
        }
    }

    if let Some(base_obj) = base {
        if let Some(env) = base_obj.get("env").and_then(|v| v.as_object()) {
            validate_env_keys("spec.base.env", env, &mut errors);
        }
        if let Some(needs) = base_obj.get("needs").and_then(|v| v.as_object()) {
            validate_needs_keys("spec.base.needs", needs, &mut errors);
        }
    }
    if let Some(envs_obj) = envs {
        for (name, val) in envs_obj {
            if let Some(env) = val
                .as_object()
                .and_then(|o| o.get("env"))
                .and_then(|v| v.as_object())
            {
                validate_env_keys(&format!("spec.environments.{name}.env"), env, &mut errors);
            }
            if let Some(needs) = val
                .as_object()
                .and_then(|o| o.get("needs"))
                .and_then(|v| v.as_object())
            {
                validate_needs_keys(
                    &format!("spec.environments.{name}.needs"),
                    needs,
                    &mut errors,
                );
            }
        }
    }

    validate_reserved_env_collision(base, envs, &mut errors);

    errors
}

/// The env-var name reserved when `needs.pg` is declared. The
/// Application controller injects the PostgreSQL DSN under this name
/// via `valueFrom.secretKeyRef` (2.4e); a user-set literal would
/// collide. KEEP IN SYNC with operator-rendering NEEDS_ENV_VAR_NAME
/// (pg→DATABASE_URL).
const PG_RESERVED_ENV: &str = "DATABASE_URL";

/// 2.4e: reject an Application that declares `needs.pg` AND sets a
/// literal `env.DATABASE_URL` (collision with the injected DSN). The
/// reservation is GLOBAL/cross-scope: pg declared in base OR ANY
/// environment reserves `DATABASE_URL` everywhere (base + every
/// environment env block). Hard reject (not warn), multi-error (one
/// per offending field, no short-circuit) — matches the validator
/// contract.
fn validate_reserved_env_collision(
    base: Option<&serde_json::Map<String, Value>>,
    envs: Option<&serde_json::Map<String, Value>>,
    errors: &mut Vec<ValidationError>,
) {
    let needs_pg = |obj: Option<&serde_json::Map<String, Value>>| -> bool {
        obj.and_then(|o| o.get("needs"))
            .and_then(|v| v.as_object())
            .is_some_and(|n| n.contains_key("pg"))
    };

    let pg_declared = needs_pg(base)
        || envs.is_some_and(|envs_obj| envs_obj.values().any(|val| needs_pg(val.as_object())));

    if !pg_declared {
        return;
    }

    let has_database_url = |obj: Option<&serde_json::Map<String, Value>>| -> bool {
        obj.and_then(|o| o.get("env"))
            .and_then(|v| v.as_object())
            .is_some_and(|env| env.contains_key(PG_RESERVED_ENV))
    };

    if has_database_url(base) {
        errors.push(ValidationError::new(
            format!("spec.base.env.{PG_RESERVED_ENV}"),
            format!(
                "{PG_RESERVED_ENV} is reserved for the PostgreSQL connection injected by needs.pg; remove it from spec.base.env"
            ),
        ));
    }

    if let Some(envs_obj) = envs {
        for (name, val) in envs_obj {
            if has_database_url(val.as_object()) {
                errors.push(ValidationError::new(
                    format!("spec.environments.{name}.env.{PG_RESERVED_ENV}"),
                    format!(
                        "{PG_RESERVED_ENV} is reserved for the PostgreSQL connection injected by needs.pg; remove it from spec.environments.{name}.env"
                    ),
                ));
            }
        }
    }
}

fn validate_env_keys(
    path: &str,
    env: &serde_json::Map<String, Value>,
    errors: &mut Vec<ValidationError>,
) {
    for key in env.keys() {
        if !is_env_var_name(key) {
            errors.push(ValidationError::new(
                format!("{path}.{key}"),
                format!("env key {key:?} must match ^[A-Z_][A-Z0-9_]*$"),
            ));
        }
    }
}

/// Built-in `#PlatformServiceType` values. The webhook enforces the
/// `needs` key enum because the structural OpenAPI v3 CRD accepts
/// any `additionalProperties` key. Keep in sync with
/// `schemas/v1alpha1/types.cue` (`#PlatformServiceType`) and the
/// `type` enum in BOTH the ResourceClaim and ServiceProvider CRDs
/// (`crd-resourceclaim.yaml`, `crd-serviceprovider.yaml`) — there is
/// no CUE->Rust/CRD generator yet, so adding a service type means
/// editing all four sites.
const PLATFORM_SERVICE_TYPES: [&str; 6] = [
    "pg",
    "jetstream",
    "clickhouse",
    "redis",
    "s3",
    "notifications",
];

fn is_platform_service_type(s: &str) -> bool {
    PLATFORM_SERVICE_TYPES.contains(&s)
}

fn validate_needs_keys(
    path: &str,
    needs: &serde_json::Map<String, Value>,
    errors: &mut Vec<ValidationError>,
) {
    for key in needs.keys() {
        if !is_platform_service_type(key) {
            errors.push(ValidationError::new(
                format!("{path}.{key}"),
                format!(
                    "needs key {key:?} is not a known platform-service type ({})",
                    PLATFORM_SERVICE_TYPES.join(", ")
                ),
            ));
        }
    }
}

fn is_dns_1123_label(s: &str) -> bool {
    if s.is_empty() || s.len() > 63 {
        return false;
    }
    let bytes = s.as_bytes();
    let endpoint_ok = |b: u8| b.is_ascii_lowercase() || b.is_ascii_digit();
    if !endpoint_ok(bytes[0]) || !endpoint_ok(bytes[bytes.len() - 1]) {
        return false;
    }
    s.bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

fn is_env_var_name(s: &str) -> bool {
    let mut bytes = s.bytes();
    let Some(first) = bytes.next() else {
        return false;
    };
    if !(first.is_ascii_uppercase() || first == b'_') {
        return false;
    }
    bytes.all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn rejects_non_object_spec() {
        let errors = validate_application_spec(&json!("not-an-object"));
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].field, "spec");
    }

    #[test]
    fn accepts_minimal_base_only_manifest() {
        let spec = json!({
            "base": { "image": "ghcr.io/acme/web:1.0" }
        });
        assert!(validate_application_spec(&spec).is_empty());
    }

    #[test]
    fn rejects_missing_base_image_when_no_environments() {
        let spec = json!({ "base": {} });
        let errors = validate_application_spec(&spec);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].field, "spec.base.image");
    }

    #[test]
    fn rejects_empty_environments_with_no_base_image() {
        let spec = json!({ "environments": {} });
        let errors = validate_application_spec(&spec);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].message.contains("nothing to deploy"));
    }

    #[test]
    fn accepts_image_only_set_via_environment_overrides() {
        let spec = json!({
            "environments": {
                "dev":  { "image": "ghcr.io/acme/web:dev" },
                "prod": { "image": "ghcr.io/acme/web:prod" }
            }
        });
        assert!(validate_application_spec(&spec).is_empty());
    }

    #[test]
    fn rejects_environments_missing_image_when_base_has_none() {
        let spec = json!({
            "environments": {
                "dev":  { "image": "ghcr.io/acme/web:dev" },
                "prod": {}
            }
        });
        let errors = validate_application_spec(&spec);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].field, "spec.environments.prod.image");
    }

    #[test]
    fn accepts_dns_1123_environment_names() {
        let spec = json!({
            "base": { "image": "x" },
            "environments": {
                "dev": {},
                "prod-eu": {},
                "qa-1": {}
            }
        });
        assert!(validate_application_spec(&spec).is_empty());
    }

    #[test]
    fn rejects_uppercase_environment_name() {
        let spec = json!({
            "base": { "image": "x" },
            "environments": { "Prod": {} }
        });
        let errors = validate_application_spec(&spec);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].field, "spec.environments.Prod");
    }

    #[test]
    fn rejects_environment_name_with_underscore() {
        let spec = json!({
            "base": { "image": "x" },
            "environments": { "prod_us": {} }
        });
        let errors = validate_application_spec(&spec);
        assert_eq!(errors.len(), 1);
    }

    #[test]
    fn rejects_environment_name_starting_with_hyphen() {
        let spec = json!({
            "base": { "image": "x" },
            "environments": { "-dev": {} }
        });
        assert_eq!(validate_application_spec(&spec).len(), 1);
    }

    #[test]
    fn rejects_environment_name_over_63_chars() {
        let long = "a".repeat(64);
        let spec = json!({
            "base": { "image": "x" },
            "environments": { long.clone(): {} }
        });
        let errors = validate_application_spec(&spec);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].field, format!("spec.environments.{long}"));
    }

    #[test]
    fn accepts_uppercase_underscore_env_keys() {
        let spec = json!({
            "base": {
                "image": "x",
                "env": { "LOG_LEVEL": "info", "_PRIVATE": "ok", "RETRIES_3": "5" }
            }
        });
        assert!(validate_application_spec(&spec).is_empty());
    }

    #[test]
    fn rejects_lowercase_or_digit_starting_env_keys() {
        let spec = json!({
            "base": {
                "image": "x",
                "env": { "log_level": "info", "1RETRY": "5", "OK_KEY": "fine" }
            }
        });
        let errors = validate_application_spec(&spec);
        assert_eq!(errors.len(), 2);
        let fields: Vec<&str> = errors.iter().map(|e| e.field.as_str()).collect();
        assert!(fields.contains(&"spec.base.env.log_level"));
        assert!(fields.contains(&"spec.base.env.1RETRY"));
    }

    #[test]
    fn validates_env_keys_under_environment_overrides_too() {
        let spec = json!({
            "base": { "image": "x" },
            "environments": {
                "dev": { "env": { "BAD-KEY": "v" } }
            }
        });
        let errors = validate_application_spec(&spec);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].field, "spec.environments.dev.env.BAD-KEY");
    }

    #[test]
    fn accepts_known_needs_keys_in_base_and_environments() {
        let spec = json!({
            "base": {
                "image": "ghcr.io/acme/web:1.0",
                "needs": { "pg": { "selector": { "tier": "integrated" } } }
            },
            "environments": {
                "prod": { "needs": { "redis": {} } }
            }
        });
        assert!(validate_application_spec(&spec).is_empty());
    }

    #[test]
    fn rejects_unknown_needs_key_in_base() {
        let spec = json!({
            "base": {
                "image": "ghcr.io/acme/web:1.0",
                "needs": { "mysql": { "selector": { "tier": "integrated" } } }
            }
        });
        let errors = validate_application_spec(&spec);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].field, "spec.base.needs.mysql");
    }

    #[test]
    fn rejects_unknown_needs_key_under_environment_override() {
        let spec = json!({
            "base": { "image": "ghcr.io/acme/web:1.0" },
            "environments": {
                "prod": { "needs": { "elasticsearch": {} } }
            }
        });
        let errors = validate_application_spec(&spec);
        assert_eq!(errors.len(), 1);
        assert_eq!(
            errors[0].field,
            "spec.environments.prod.needs.elasticsearch"
        );
    }

    #[test]
    fn reports_every_unknown_needs_key_not_just_the_first() {
        // The validator does not short-circuit — two bad keys in one
        // `needs` map must surface two errors (mirrors the env-key
        // multi-error guarantee).
        let spec = json!({
            "base": {
                "image": "ghcr.io/acme/web:1.0",
                "needs": { "mysql": {}, "mongo": {} }
            }
        });
        let errors = validate_application_spec(&spec);
        assert_eq!(errors.len(), 2);
        assert!(errors
            .iter()
            .all(|e| e.field.starts_with("spec.base.needs.")));
    }

    // ---- 2.4e: DATABASE_URL is reserved when needs.pg is declared ----

    #[test]
    fn rejects_database_url_in_base_env_when_base_needs_pg() {
        let spec = json!({
            "base": {
                "image": "ghcr.io/acme/web:1.0",
                "needs": { "pg": {} },
                "env": { "DATABASE_URL": "postgres://override" }
            }
        });
        let errors = validate_application_spec(&spec);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].field, "spec.base.env.DATABASE_URL");
    }

    #[test]
    fn rejects_database_url_in_environment_env_when_base_needs_pg() {
        let spec = json!({
            "base": {
                "image": "ghcr.io/acme/web:1.0",
                "needs": { "pg": {} }
            },
            "environments": {
                "prod": { "env": { "DATABASE_URL": "postgres://override" } }
            }
        });
        let errors = validate_application_spec(&spec);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].field, "spec.environments.prod.env.DATABASE_URL");
    }

    #[test]
    fn rejects_database_url_in_base_env_when_environment_needs_pg_cross_scope() {
        // The reservation is GLOBAL/cross-scope: pg declared in an
        // environment reserves DATABASE_URL everywhere, including base.
        let spec = json!({
            "base": {
                "image": "ghcr.io/acme/web:1.0",
                "env": { "DATABASE_URL": "postgres://override" }
            },
            "environments": {
                "prod": { "needs": { "pg": {} } }
            }
        });
        let errors = validate_application_spec(&spec);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].field, "spec.base.env.DATABASE_URL");
    }

    #[test]
    fn accepts_needs_pg_without_database_url_literal() {
        let spec = json!({
            "base": {
                "image": "ghcr.io/acme/web:1.0",
                "needs": { "pg": {} },
                "env": { "LOG_LEVEL": "info" }
            }
        });
        assert!(validate_application_spec(&spec).is_empty());
    }

    #[test]
    fn accepts_database_url_literal_when_no_needs_pg() {
        // DATABASE_URL is reserved ONLY under needs.pg. With no pg
        // need, a literal DATABASE_URL is a normal env var.
        let spec = json!({
            "base": {
                "image": "ghcr.io/acme/web:1.0",
                "env": { "DATABASE_URL": "postgres://my-own-db" }
            }
        });
        assert!(validate_application_spec(&spec).is_empty());
    }

    #[test]
    fn reports_database_url_collision_in_both_base_and_environment() {
        // Multi-error contract: a DATABASE_URL literal in BOTH base and
        // an environment under needs.pg surfaces two errors (one per
        // offending field), no short-circuit.
        let spec = json!({
            "base": {
                "image": "ghcr.io/acme/web:1.0",
                "needs": { "pg": {} },
                "env": { "DATABASE_URL": "postgres://a" }
            },
            "environments": {
                "prod": { "env": { "DATABASE_URL": "postgres://b" } }
            }
        });
        let errors = validate_application_spec(&spec);
        assert_eq!(errors.len(), 2);
        let fields: Vec<&str> = errors.iter().map(|e| e.field.as_str()).collect();
        assert!(fields.contains(&"spec.base.env.DATABASE_URL"));
        assert!(fields.contains(&"spec.environments.prod.env.DATABASE_URL"));
    }
}
