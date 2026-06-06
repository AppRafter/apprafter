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

    validate_needs_names(base, envs, &mut errors);
    validate_reserved_env_collision(base, envs, &mut errors);
    validate_disk_claims(base, envs, &mut errors);

    errors
}

/// 2.6b (ADR 0043): a `needs.<type>` value is either a scalar entry
/// (object) or an array of entries. Return the optional `name` of each
/// entry, in declaration order. A scalar object yields exactly one
/// entry; an array yields one per element. A non-object/non-array value
/// yields no entries (its shape is rejected by the CRD layer). An entry
/// with no `name` (or an empty `name`) is the unnamed default.
fn needs_entry_names(value: &Value) -> Vec<Option<&str>> {
    fn entry_name(v: &Value) -> Option<&str> {
        v.as_object()
            .and_then(|o| o.get("name"))
            .and_then(|n| n.as_str())
            .filter(|n| !n.is_empty())
    }
    match value {
        Value::Array(items) => items.iter().map(entry_name).collect(),
        Value::Object(_) => vec![entry_name(value)],
        _ => Vec::new(),
    }
}

/// 2.6b (ADR 0043): validate the `(type, name)` identity rules within
/// each `needs.<type>` value, in BOTH base and every environment. Each
/// explicit `name` must be env-foldable (a DNS-1123 label, so the fold
/// `-` → `_` + uppercase yields a valid `[A-Z_][A-Z0-9_]*` env-var
/// suffix); names must be unique within a single (scope, type) value;
/// and at most one unnamed default is allowed per (scope, type) value.
/// Multi-error: one error per offending `needs.<type>` field, no
/// short-circuit (matching the validator contract).
fn validate_needs_names(
    base: Option<&serde_json::Map<String, Value>>,
    envs: Option<&serde_json::Map<String, Value>>,
    errors: &mut Vec<ValidationError>,
) {
    let check_scope = |path: &str,
                       obj: Option<&serde_json::Map<String, Value>>,
                       errors: &mut Vec<ValidationError>| {
        let Some(needs) = obj.and_then(|o| o.get("needs")).and_then(|v| v.as_object()) else {
            return;
        };
        for (service_type, value) in needs {
            // `disk` is not a connection-secret/env-injected service — its
            // `(name, mountPath)` identity + DNS-1123/uniqueness rules are
            // validated by `validate_disk_claims` (which also derives a name
            // from `mountPath`). Skip it here to avoid double-reporting.
            if service_type == "disk" {
                continue;
            }
            let entries = needs_entry_names(value);
            let mut seen: Vec<&str> = Vec::new();
            let mut unnamed = 0usize;
            for name in &entries {
                match name {
                    None => unnamed += 1,
                    Some(n) => {
                        if !is_dns_1123_label(n) {
                            errors.push(ValidationError::new(
                                format!("{path}.{service_type}"),
                                format!(
                                    "needs.{service_type} entry name {n:?} must be a DNS-1123 label (lowercase alphanumeric + '-', start and end alphanumeric) so it folds to a valid [A-Z_][A-Z0-9_]* env-var suffix"
                                ),
                            ));
                        } else if seen.contains(n) {
                            errors.push(ValidationError::new(
                                format!("{path}.{service_type}"),
                                format!(
                                    "needs.{service_type} has a duplicate entry name {n:?}; names must be unique within a type"
                                ),
                            ));
                        } else {
                            seen.push(n);
                        }
                    }
                }
            }
            if unnamed > 1 {
                errors.push(ValidationError::new(
                    format!("{path}.{service_type}"),
                    format!(
                        "needs.{service_type} declares {unnamed} unnamed entries; at most one unnamed default per type is allowed (give the others a name)"
                    ),
                ));
            }
        }
    };

    check_scope("spec.base.needs", base, errors);
    if let Some(envs_obj) = envs {
        for (env_name, val) in envs_obj {
            check_scope(
                &format!("spec.environments.{env_name}.needs"),
                val.as_object(),
                errors,
            );
        }
    }
}

/// 2.6b (ADR 0043): the `needs.disk` value, scalar or array, as a list
/// of disk-entry objects in declaration order. Non-object array elements
/// and a non-object/non-array value yield no entries (their shape is
/// rejected by the CRD layer / the closed `#DiskClaim` schema).
fn disk_entries(value: &Value) -> Vec<&serde_json::Map<String, Value>> {
    match value {
        Value::Array(items) => items.iter().filter_map(|v| v.as_object()).collect(),
        Value::Object(o) => vec![o],
        _ => Vec::new(),
    }
}

/// 2.6b (ADR 0043): derive a disk claim's name — the explicit `name`,
/// else the last path segment of `mountPath` (`/var/lib/uploads` →
/// `uploads`, `/data` → `data`). Returns `None` when neither yields a
/// non-empty segment (a malformed/relative mountPath; the absolute-path
/// guard reports that separately).
fn disk_name(entry: &serde_json::Map<String, Value>) -> Option<String> {
    if let Some(n) = entry
        .get("name")
        .and_then(|v| v.as_str())
        .filter(|n| !n.is_empty())
    {
        return Some(n.to_string());
    }
    entry
        .get("mountPath")
        .and_then(|v| v.as_str())
        .and_then(|p| p.rsplit('/').find(|seg| !seg.is_empty()))
        .map(|seg| seg.to_string())
}

/// 2.6b (ADR 0043): a Kubernetes resource quantity for the disk `size`.
/// A decimal magnitude with an optional binary (Ei…Ki) or decimal
/// (E…k, with lower-case `k`) SI suffix — sufficient for the disk size
/// surface (no exponent / signed forms, which `#DiskClaim.size` never
/// needs). KEEP IN SYNC with the design's quantity grammar.
fn is_k8s_quantity(s: &str) -> bool {
    let suffix_ok = |suffix: &str| -> bool {
        matches!(
            suffix,
            "" | "Ei" | "Pi" | "Ti" | "Gi" | "Mi" | "Ki" | "E" | "P" | "T" | "G" | "M" | "k"
        )
    };
    // Split the leading numeric magnitude (digits, optionally one '.'
    // followed by digits) from the trailing suffix.
    let mut chars = s.char_indices().peekable();
    let mut seen_digit = false;
    let mut seen_dot = false;
    let mut split = s.len();
    while let Some(&(i, ch)) = chars.peek() {
        match ch {
            '0'..='9' => {
                seen_digit = true;
                chars.next();
            }
            '.' if !seen_dot => {
                seen_dot = true;
                chars.next();
            }
            _ => {
                split = i;
                break;
            }
        }
    }
    if !seen_digit {
        return false;
    }
    let (magnitude, suffix) = s.split_at(split);
    // A trailing '.' with no fractional digits is malformed.
    if magnitude.ends_with('.') {
        return false;
    }
    suffix_ok(suffix)
}

/// 2.6b-4 (ADR 0043): one `needs.disk`-bearing scope to validate —
/// `(field-path prefix, the scope object, effective replicas)`. The
/// prefix is `spec.base` or `spec.environments.<name>`; the effective
/// replicas is the env-scoped override else the inherited base value.
type DiskScope<'a> = (
    String,
    Option<&'a serde_json::Map<String, Value>>,
    Option<i64>,
);

/// 2.6b-4 (ADR 0043): disk-specific value guards, collected across
/// `spec.base.needs.disk` AND every `spec.environments.*.needs.disk`.
/// For each disk entry: the derived/explicit `name` must be a DNS-1123
/// label (it becomes part of the PVC name) and unique within disk;
/// `mountPath` must be absolute AND unique app-wide; `size` must parse as
/// a Kubernetes quantity; `class` must be `local` (replicated/shared are
/// T2-deferred). Separately, a non-empty `needs.disk` in a scope whose
/// EFFECTIVE replicas (env override else base) is > 1 is rejected — a
/// standalone RWO PVC supports only a single-replica Deployment at launch
/// (per-replica multi-replica is T2). Multi-error: one message per
/// offending field, no short-circuit (matching the validator contract).
fn validate_disk_claims(
    base: Option<&serde_json::Map<String, Value>>,
    envs: Option<&serde_json::Map<String, Value>>,
    errors: &mut Vec<ValidationError>,
) {
    let base_replicas = base
        .and_then(|b| b.get("replicas"))
        .and_then(|v| v.as_i64());

    // mountPath uniqueness is app-wide (across base + every environment):
    // collect every seen mountPath, reporting on the second+ occurrence.
    let mut seen_mount_paths: Vec<String> = Vec::new();

    // Each scope: (field-path prefix, the scope object, effective replicas).
    let mut scopes: Vec<DiskScope> = vec![("spec.base".to_string(), base, base_replicas)];
    if let Some(envs_obj) = envs {
        for (env_name, val) in envs_obj {
            let env_obj = val.as_object();
            // Env-override replaces base replicas; else inherit base.
            let effective = env_obj
                .and_then(|o| o.get("replicas"))
                .and_then(|v| v.as_i64())
                .or(base_replicas);
            scopes.push((format!("spec.environments.{env_name}"), env_obj, effective));
        }
    }

    for (prefix, scope, effective_replicas) in scopes {
        let Some(value) = scope
            .and_then(|o| o.get("needs"))
            .and_then(|v| v.as_object())
            .and_then(|n| n.get("disk"))
        else {
            continue;
        };
        let entries = disk_entries(value);
        if entries.is_empty() {
            continue;
        }
        let needs_disk_field = format!("{prefix}.needs.disk");

        // Names unique within this scope's disk value.
        let mut seen_names: Vec<String> = Vec::new();

        for entry in &entries {
            // ---- name (explicit or mountPath-derived) ----
            match disk_name(entry) {
                Some(name) => {
                    if !is_dns_1123_label(&name) {
                        errors.push(ValidationError::new(
                            &needs_disk_field,
                            format!(
                                "needs.disk entry name {name:?} must be a DNS-1123 label (lowercase alphanumeric + '-', start and end alphanumeric) — it becomes part of the PVC name"
                            ),
                        ));
                    } else if seen_names.contains(&name) {
                        errors.push(ValidationError::new(
                            &needs_disk_field,
                            format!(
                                "needs.disk has a duplicate entry name {name:?}; names must be unique within disk (set an explicit `name` to disambiguate)"
                            ),
                        ));
                    } else {
                        seen_names.push(name);
                    }
                }
                None => {
                    // No explicit name and no usable mountPath segment.
                    errors.push(ValidationError::new(
                        &needs_disk_field,
                        "needs.disk entry has no `name` and no usable `mountPath` to derive one from",
                    ));
                }
            }

            // ---- mountPath: absolute + app-wide unique ----
            match entry.get("mountPath").and_then(|v| v.as_str()) {
                Some(mp) if mp.starts_with('/') => {
                    if seen_mount_paths.iter().any(|p| p == mp) {
                        errors.push(ValidationError::new(
                            &needs_disk_field,
                            format!(
                                "needs.disk mountPath {mp:?} is declared more than once; each disk mountPath must be unique within the app"
                            ),
                        ));
                    } else {
                        seen_mount_paths.push(mp.to_string());
                    }
                }
                Some(mp) => {
                    errors.push(ValidationError::new(
                        &needs_disk_field,
                        format!(
                            "needs.disk mountPath {mp:?} must be an absolute path (start with '/')"
                        ),
                    ));
                }
                None => {
                    errors.push(ValidationError::new(
                        &needs_disk_field,
                        "needs.disk entry is missing the required `mountPath`",
                    ));
                }
            }

            // ---- size: a Kubernetes quantity ----
            match entry.get("size").and_then(|v| v.as_str()) {
                Some(size) if is_k8s_quantity(size) => {}
                Some(size) => {
                    errors.push(ValidationError::new(
                        &needs_disk_field,
                        format!(
                            "needs.disk size {size:?} must be a Kubernetes quantity (e.g. \"10Gi\", \"500Mi\", \"1G\")"
                        ),
                    ));
                }
                None => {
                    errors.push(ValidationError::new(
                        &needs_disk_field,
                        "needs.disk entry is missing the required `size`",
                    ));
                }
            }

            // ---- class: local only at launch ----
            if let Some(class) = entry.get("class").and_then(|v| v.as_str()) {
                if class != "local" {
                    errors.push(ValidationError::new(
                        &needs_disk_field,
                        format!(
                            "needs.disk class {class:?} is not supported; only `local` is available at launch (replicated/shared classes are T2, deferred)"
                        ),
                    ));
                }
            }
        }

        // ---- replicas: a disk needs a single-replica Deployment ----
        if let Some(replicas) = effective_replicas {
            if replicas > 1 {
                errors.push(ValidationError::new(
                    format!("{prefix}.replicas"),
                    "persistent disks currently support single-replica apps; use replicas: 1 (per-replica disks for multi-replica apps are T2)",
                ));
            }
        }
    }
}

/// 2.6b (ADR 0043): fold a `(type, name)` entry name into a valid
/// env-var-NAME segment — uppercase ASCII letters and map `-` → `_`.
/// KEEP IN SYNC with operator-rendering `fold_env_segment`. The
/// reserved env NAME of a named service claim is `<VAR>_<fold(name)>`.
fn fold_env_segment(name: &str) -> String {
    name.chars()
        .map(|ch| match ch {
            '-' => '_',
            other => other.to_ascii_uppercase(),
        })
        .collect()
}

/// needs-type → the env var names reserved (injected by the
/// provisioner/renderer via `valueFrom.secretKeyRef`) when that need
/// is declared. A user-set literal of any of these would collide with
/// the injected value. KEEP IN SYNC with operator-rendering
/// `NEEDS_ENV_BINDINGS` (pg→DATABASE_URL; redis→REDIS_URL,
/// REDIS_CHANNEL_PREFIX).
const RESERVED_ENV: &[(&str, &[&str])] = &[
    ("pg", &["DATABASE_URL"]),
    ("redis", &["REDIS_URL", "REDIS_CHANNEL_PREFIX"]),
];

/// 2.4e/2.6: reject an Application that declares a `needs.<type>` AND
/// sets a literal `env` var reserved for that need's injected
/// connection. The reservation is GLOBAL/cross-scope: a need declared
/// in base OR ANY environment reserves its env names everywhere across
/// the base env block and every environment env block. Hard reject
/// rather than warn, and multi-error with one error per offending
/// field and no short-circuit, matching the validator contract.
fn validate_reserved_env_collision(
    base: Option<&serde_json::Map<String, Value>>,
    envs: Option<&serde_json::Map<String, Value>>,
    errors: &mut Vec<ValidationError>,
) {
    let need_declared = |need: &str| -> bool {
        let declares = |obj: Option<&serde_json::Map<String, Value>>| -> bool {
            obj.and_then(|o| o.get("needs"))
                .and_then(|v| v.as_object())
                .is_some_and(|n| n.contains_key(need))
        };
        declares(base)
            || envs.is_some_and(|envs_obj| envs_obj.values().any(|val| declares(val.as_object())))
    };

    let env_has_key = |obj: Option<&serde_json::Map<String, Value>>, key: &str| -> bool {
        obj.and_then(|o| o.get("env"))
            .and_then(|v| v.as_object())
            .is_some_and(|env| env.contains_key(key))
    };

    // The set of distinct env-NAME suffixes a need declares across all
    // scopes. The unnamed default contributes `None` (no suffix → the
    // base `<VAR>`); a named entry contributes `Some("_<FOLD(name)>")`
    // (→ `<VAR>_<FOLD(name)>`). 2.6b (ADR 0043): a literal env var of any
    // reserved name (default OR suffixed) collides with the injected
    // connection and is rejected.
    let need_suffixes = |need: &str| -> Vec<Option<String>> {
        let mut suffixes: Vec<Option<String>> = Vec::new();
        let mut push_from = |obj: Option<&serde_json::Map<String, Value>>| {
            if let Some(value) = obj
                .and_then(|o| o.get("needs"))
                .and_then(|v| v.as_object())
                .and_then(|n| n.get(need))
            {
                for name in needs_entry_names(value) {
                    let suffix = name.map(|n| format!("_{}", fold_env_segment(n)));
                    if !suffixes.contains(&suffix) {
                        suffixes.push(suffix);
                    }
                }
            }
        };
        push_from(base);
        if let Some(envs_obj) = envs {
            for val in envs_obj.values() {
                push_from(val.as_object());
            }
        }
        suffixes
    };

    for (need, reserved_names) in RESERVED_ENV {
        if !need_declared(need) {
            continue;
        }
        let suffixes = need_suffixes(need);
        for base_name in *reserved_names {
            for suffix in &suffixes {
                let reserved = match suffix {
                    Some(s) => format!("{base_name}{s}"),
                    None => base_name.to_string(),
                };
                if env_has_key(base, &reserved) {
                    errors.push(ValidationError::new(
                        format!("spec.base.env.{reserved}"),
                        format!(
                            "{reserved} is reserved for the connection injected by needs.{need}; remove it from spec.base.env"
                        ),
                    ));
                }
                if let Some(envs_obj) = envs {
                    for (env_name, val) in envs_obj {
                        if env_has_key(val.as_object(), &reserved) {
                            errors.push(ValidationError::new(
                                format!("spec.environments.{env_name}.env.{reserved}"),
                                format!(
                                    "{reserved} is reserved for the connection injected by needs.{need}; remove it from spec.environments.{env_name}.env"
                                ),
                            ));
                        }
                    }
                }
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
const PLATFORM_SERVICE_TYPES: [&str; 7] = [
    "pg",
    "jetstream",
    "clickhouse",
    "redis",
    "s3",
    "notifications",
    // 2.6b (ADR 0043): persistent block storage. A `needs.disk` entry
    // generates a `type: disk` ResourceClaim; the disk-specific value
    // guards (mountPath/size/class/replicas) land in 2.6b-4.
    "disk",
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
    fn accepts_disk_needs_key() {
        // 2.6b (ADR 0043): `disk` is a known platform-service type, so a
        // `needs.disk` entry must be accepted (the disk value shape is
        // validated by the disk-specific guards in 2.6b-4).
        let spec = json!({
            "base": {
                "image": "ghcr.io/acme/web:1.0",
                "needs": { "disk": { "size": "1Gi", "mountPath": "/data" } }
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

    // ---- 2.6-6: REDIS_URL/REDIS_CHANNEL_PREFIX reserved under needs.redis ----

    #[test]
    fn rejects_redis_reserved_env_when_needs_redis() {
        let spec = json!({
            "base": {
                "image": "ghcr.io/acme/web:1.0",
                "needs": { "redis": {} },
                "env": { "REDIS_URL": "redis://x", "REDIS_CHANNEL_PREFIX": "p:" }
            }
        });
        let errors = validate_application_spec(&spec);
        let fields: Vec<&str> = errors.iter().map(|e| e.field.as_str()).collect();
        assert!(fields.contains(&"spec.base.env.REDIS_URL"));
        assert!(fields.contains(&"spec.base.env.REDIS_CHANNEL_PREFIX"));
    }

    #[test]
    fn rejects_redis_reserved_env_under_environment_when_base_needs_redis() {
        let spec = json!({
            "base": {
                "image": "ghcr.io/acme/web:1.0",
                "needs": { "redis": {} }
            },
            "environments": {
                "prod": { "env": { "REDIS_CHANNEL_PREFIX": "override:" } }
            }
        });
        let errors = validate_application_spec(&spec);
        assert_eq!(errors.len(), 1);
        assert_eq!(
            errors[0].field,
            "spec.environments.prod.env.REDIS_CHANNEL_PREFIX"
        );
    }

    #[test]
    fn accepts_redis_reserved_env_literal_when_no_needs_redis() {
        // REDIS_URL/REDIS_CHANNEL_PREFIX are reserved ONLY under
        // needs.redis. Without the need, they are normal env vars.
        let spec = json!({
            "base": {
                "image": "ghcr.io/acme/web:1.0",
                "env": { "REDIS_URL": "redis://my-own", "REDIS_CHANNEL_PREFIX": "p:" }
            }
        });
        assert!(validate_application_spec(&spec).is_empty());
    }

    #[test]
    fn pg_and_redis_reservations_coexist() {
        // Declaring both needs reserves all three names; a collision on
        // each surfaces independently (no cross-contamination).
        let spec = json!({
            "base": {
                "image": "ghcr.io/acme/web:1.0",
                "needs": { "pg": {}, "redis": {} },
                "env": {
                    "DATABASE_URL": "postgres://x",
                    "REDIS_URL": "redis://y"
                }
            }
        });
        let errors = validate_application_spec(&spec);
        let fields: Vec<&str> = errors.iter().map(|e| e.field.as_str()).collect();
        assert!(fields.contains(&"spec.base.env.DATABASE_URL"));
        assert!(fields.contains(&"spec.base.env.REDIS_URL"));
        assert_eq!(errors.len(), 2);
    }

    // ---- 2.6b-2: (type, name) uniqueness + foldability + reserved-env suffix ----

    #[test]
    fn accepts_named_pg_array_with_distinct_names() {
        // An array of named pg entries with distinct, env-foldable names
        // is valid (each yields a distinct DATABASE_URL_<NAME>).
        let spec = json!({
            "base": {
                "image": "ghcr.io/acme/web:1.0",
                "needs": { "pg": [{ "name": "primary" }, { "name": "analytics" }] }
            }
        });
        assert!(validate_application_spec(&spec).is_empty());
    }

    #[test]
    fn accepts_one_unnamed_default_plus_named_siblings() {
        // At most one unnamed default per type is allowed; an unnamed
        // default coexisting with named siblings is valid.
        let spec = json!({
            "base": {
                "image": "ghcr.io/acme/web:1.0",
                "needs": { "pg": [{}, { "name": "analytics" }] }
            }
        });
        assert!(validate_application_spec(&spec).is_empty());
    }

    #[test]
    fn rejects_duplicate_name_within_a_type() {
        let spec = json!({
            "base": {
                "image": "ghcr.io/acme/web:1.0",
                "needs": { "pg": [{ "name": "a" }, { "name": "a" }] }
            }
        });
        let errors = validate_application_spec(&spec);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].field, "spec.base.needs.pg");
        assert!(errors[0].message.contains("duplicate"));
    }

    #[test]
    fn rejects_more_than_one_unnamed_entry_in_one_type() {
        let spec = json!({
            "base": {
                "image": "ghcr.io/acme/web:1.0",
                "needs": { "pg": [{}, {}] }
            }
        });
        let errors = validate_application_spec(&spec);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].field, "spec.base.needs.pg");
        assert!(errors[0].message.contains("unnamed"));
    }

    #[test]
    fn rejects_non_foldable_name_with_underscore() {
        // `name` must be a DNS-1123 label so the fold yields a valid
        // [A-Z_][A-Z0-9_]* env suffix; an underscore is not allowed.
        let spec = json!({
            "base": {
                "image": "ghcr.io/acme/web:1.0",
                "needs": { "pg": [{ "name": "read_replica" }] }
            }
        });
        let errors = validate_application_spec(&spec);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].field, "spec.base.needs.pg");
        assert!(errors[0].message.contains("DNS-1123"));
    }

    #[test]
    fn rejects_non_foldable_name_uppercase() {
        let spec = json!({
            "base": {
                "image": "ghcr.io/acme/web:1.0",
                "needs": { "pg": [{ "name": "Analytics" }] }
            }
        });
        let errors = validate_application_spec(&spec);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].field, "spec.base.needs.pg");
        assert!(errors[0].message.contains("DNS-1123"));
    }

    #[test]
    fn rejects_literal_env_colliding_with_named_pg_reserved_suffix() {
        // A named pg claim `analytics` reserves DATABASE_URL_ANALYTICS;
        // a literal env of that name collides and is rejected.
        let spec = json!({
            "base": {
                "image": "ghcr.io/acme/web:1.0",
                "needs": { "pg": [{ "name": "analytics" }] },
                "env": { "DATABASE_URL_ANALYTICS": "postgres://override" }
            }
        });
        let errors = validate_application_spec(&spec);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].field, "spec.base.env.DATABASE_URL_ANALYTICS");
    }

    #[test]
    fn rejects_literal_env_colliding_with_named_redis_reserved_suffix() {
        // redis injects two vars; a named claim reserves BOTH suffixed
        // names (REDIS_URL_CACHE, REDIS_CHANNEL_PREFIX_CACHE).
        let spec = json!({
            "base": {
                "image": "ghcr.io/acme/web:1.0",
                "needs": { "redis": [{ "name": "cache" }] },
                "env": {
                    "REDIS_URL_CACHE": "redis://x",
                    "REDIS_CHANNEL_PREFIX_CACHE": "p:"
                }
            }
        });
        let errors = validate_application_spec(&spec);
        let fields: Vec<&str> = errors.iter().map(|e| e.field.as_str()).collect();
        assert!(fields.contains(&"spec.base.env.REDIS_URL_CACHE"));
        assert!(fields.contains(&"spec.base.env.REDIS_CHANNEL_PREFIX_CACHE"));
        assert_eq!(errors.len(), 2);
    }

    #[test]
    fn named_reserved_suffix_collision_is_cross_scope() {
        // A pg claim named `analytics` declared in an environment
        // reserves DATABASE_URL_ANALYTICS everywhere, including base.
        let spec = json!({
            "base": {
                "image": "ghcr.io/acme/web:1.0",
                "env": { "DATABASE_URL_ANALYTICS": "postgres://override" }
            },
            "environments": {
                "prod": { "needs": { "pg": [{ "name": "analytics" }] } }
            }
        });
        let errors = validate_application_spec(&spec);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].field, "spec.base.env.DATABASE_URL_ANALYTICS");
    }

    #[test]
    fn unnamed_default_does_not_reserve_a_suffix() {
        // The unnamed default reserves only the base var (DATABASE_URL),
        // never a suffixed name; a literal DATABASE_URL_FOO is free when
        // no claim is named `foo`.
        let spec = json!({
            "base": {
                "image": "ghcr.io/acme/web:1.0",
                "needs": { "pg": {} },
                "env": { "DATABASE_URL_FOO": "postgres://my-own" }
            }
        });
        assert!(validate_application_spec(&spec).is_empty());
    }

    #[test]
    fn collects_names_across_base_and_environments_for_duplicate_check() {
        // A name declared once in base and once in an environment for the
        // SAME type is the SAME claim identity in different scopes and is
        // not a duplicate within either scope's array — but two entries
        // with the same name in a single array is a duplicate. This test
        // pins the per-(scope,type) array duplicate semantics.
        let spec = json!({
            "base": {
                "image": "ghcr.io/acme/web:1.0",
                "needs": { "pg": [{ "name": "a" }, { "name": "b" }] }
            },
            "environments": {
                "prod": { "needs": { "pg": [{ "name": "a" }, { "name": "a" }] } }
            }
        });
        let errors = validate_application_spec(&spec);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].field, "spec.environments.prod.needs.pg");
        assert!(errors[0].message.contains("duplicate"));
    }

    #[test]
    fn scalar_named_entry_validates_its_name() {
        // The scalar form may also carry a name; a non-foldable scalar
        // name is rejected just like an array entry.
        let spec = json!({
            "base": {
                "image": "ghcr.io/acme/web:1.0",
                "needs": { "pg": { "name": "BAD_NAME" } }
            }
        });
        let errors = validate_application_spec(&spec);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].field, "spec.base.needs.pg");
        assert!(errors[0].message.contains("DNS-1123"));
    }

    // ---- 2.6b-4: disk value guards (name/mountPath/size/class/replicas) ----

    #[test]
    fn accepts_valid_single_replica_local_disk() {
        // The happy path: a single-replica app with a well-formed
        // `needs.disk` (valid quantity, absolute mountPath, class local,
        // mountPath-derived name) is accepted.
        let spec = json!({
            "base": {
                "image": "ghcr.io/acme/web:1.0",
                "replicas": 1,
                "needs": { "disk": { "size": "1Gi", "mountPath": "/var/lib/uploads" } }
            }
        });
        assert!(validate_application_spec(&spec).is_empty());
    }

    #[test]
    fn accepts_disk_with_explicit_name_and_array_form() {
        // An array of two disks with distinct explicit names and distinct
        // absolute mountPaths is valid.
        let spec = json!({
            "base": {
                "image": "ghcr.io/acme/web:1.0",
                "needs": {
                    "disk": [
                        { "name": "data", "size": "1Gi", "mountPath": "/data" },
                        { "name": "cache", "size": "500Mi", "mountPath": "/cache" }
                    ]
                }
            }
        });
        assert!(validate_application_spec(&spec).is_empty());
    }

    #[test]
    fn rejects_duplicate_disk_mount_path() {
        let spec = json!({
            "base": {
                "image": "ghcr.io/acme/web:1.0",
                "needs": {
                    "disk": [
                        { "name": "a", "size": "1Gi", "mountPath": "/data" },
                        { "name": "b", "size": "1Gi", "mountPath": "/data" }
                    ]
                }
            }
        });
        let errors = validate_application_spec(&spec);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].field, "spec.base.needs.disk");
        assert!(errors[0].message.contains("mountPath"));
        assert!(errors[0].message.contains("/data"));
    }

    #[test]
    fn rejects_duplicate_derived_disk_name() {
        // Two disks whose derived names collide (same last mountPath
        // segment) are rejected — the name becomes part of the PVC name.
        let spec = json!({
            "base": {
                "image": "ghcr.io/acme/web:1.0",
                "needs": {
                    "disk": [
                        { "size": "1Gi", "mountPath": "/var/lib/data" },
                        { "size": "1Gi", "mountPath": "/srv/data" }
                    ]
                }
            }
        });
        let errors = validate_application_spec(&spec);
        // mountPaths are distinct, but both derive name "data".
        let name_errs: Vec<&ValidationError> = errors
            .iter()
            .filter(|e| e.message.contains("duplicate") && e.message.contains("name"))
            .collect();
        assert_eq!(name_errs.len(), 1);
        assert_eq!(name_errs[0].field, "spec.base.needs.disk");
        assert!(name_errs[0].message.contains("data"));
    }

    #[test]
    fn rejects_disk_name_not_dns_1123() {
        // An explicit disk name that is not a DNS-1123 label is rejected
        // (it becomes part of the PVC name).
        let spec = json!({
            "base": {
                "image": "ghcr.io/acme/web:1.0",
                "needs": { "disk": { "name": "Bad_Name", "size": "1Gi", "mountPath": "/data" } }
            }
        });
        let errors = validate_application_spec(&spec);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].field, "spec.base.needs.disk");
        assert!(errors[0].message.contains("DNS-1123"));
    }

    #[test]
    fn rejects_disk_derived_name_not_dns_1123() {
        // The mountPath-derived name must also be a valid DNS-1123 label;
        // a last segment that is not (uppercase) is rejected.
        let spec = json!({
            "base": {
                "image": "ghcr.io/acme/web:1.0",
                "needs": { "disk": { "size": "1Gi", "mountPath": "/var/Uploads" } }
            }
        });
        let errors = validate_application_spec(&spec);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].field, "spec.base.needs.disk");
        assert!(errors[0].message.contains("DNS-1123"));
    }

    #[test]
    fn rejects_relative_disk_mount_path() {
        let spec = json!({
            "base": {
                "image": "ghcr.io/acme/web:1.0",
                "needs": { "disk": { "name": "data", "size": "1Gi", "mountPath": "data" } }
            }
        });
        let errors = validate_application_spec(&spec);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].field, "spec.base.needs.disk");
        assert!(errors[0].message.contains("absolute"));
    }

    #[test]
    fn rejects_disk_size_not_a_quantity() {
        let spec = json!({
            "base": {
                "image": "ghcr.io/acme/web:1.0",
                "needs": { "disk": { "name": "data", "size": "notaquantity", "mountPath": "/data" } }
            }
        });
        let errors = validate_application_spec(&spec);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].field, "spec.base.needs.disk");
        assert!(errors[0].message.contains("quantity"));
    }

    #[test]
    fn accepts_disk_decimal_and_plain_quantities() {
        // `1.5Gi`, a plain `1000000`, and lower-k `512k` are valid
        // Kubernetes quantities.
        for size in ["1.5Gi", "1000000", "512k", "10G", "256Mi"] {
            let spec = json!({
                "base": {
                    "image": "ghcr.io/acme/web:1.0",
                    "needs": { "disk": { "name": "data", "size": size, "mountPath": "/data" } }
                }
            });
            assert!(
                validate_application_spec(&spec).is_empty(),
                "size {size:?} should be a valid quantity"
            );
        }
    }

    #[test]
    fn rejects_disk_class_replicated_with_t2_hint() {
        let spec = json!({
            "base": {
                "image": "ghcr.io/acme/web:1.0",
                "needs": { "disk": { "name": "data", "size": "1Gi", "mountPath": "/data", "class": "replicated" } }
            }
        });
        let errors = validate_application_spec(&spec);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].field, "spec.base.needs.disk");
        assert!(errors[0].message.contains("local"));
        assert!(errors[0].message.contains("T2"));
    }

    #[test]
    fn accepts_disk_class_local_explicit() {
        let spec = json!({
            "base": {
                "image": "ghcr.io/acme/web:1.0",
                "needs": { "disk": { "name": "data", "size": "1Gi", "mountPath": "/data", "class": "local" } }
            }
        });
        assert!(validate_application_spec(&spec).is_empty());
    }

    #[test]
    fn rejects_disk_with_base_replicas_greater_than_one() {
        let spec = json!({
            "base": {
                "image": "ghcr.io/acme/web:1.0",
                "replicas": 2,
                "needs": { "disk": { "name": "data", "size": "1Gi", "mountPath": "/data" } }
            }
        });
        let errors = validate_application_spec(&spec);
        let replica_errs: Vec<&ValidationError> = errors
            .iter()
            .filter(|e| e.message.contains("single-replica"))
            .collect();
        assert_eq!(replica_errs.len(), 1);
        assert_eq!(replica_errs[0].field, "spec.base.replicas");
        assert!(replica_errs[0].message.contains("replicas: 1"));
        assert!(replica_errs[0].message.contains("T2"));
    }

    #[test]
    fn rejects_disk_with_env_override_replicas_greater_than_one() {
        // A per-environment replicas override > 1 with disk present in
        // that environment is rejected against the effective replicas.
        let spec = json!({
            "base": {
                "image": "ghcr.io/acme/web:1.0",
                "replicas": 1
            },
            "environments": {
                "prod": {
                    "replicas": 3,
                    "needs": { "disk": { "name": "data", "size": "1Gi", "mountPath": "/data" } }
                }
            }
        });
        let errors = validate_application_spec(&spec);
        let replica_errs: Vec<&ValidationError> = errors
            .iter()
            .filter(|e| e.message.contains("single-replica"))
            .collect();
        assert_eq!(replica_errs.len(), 1);
        assert_eq!(replica_errs[0].field, "spec.environments.prod.replicas");
    }

    #[test]
    fn rejects_env_disk_against_inherited_base_replicas() {
        // A disk declared in an environment with no env-scoped replicas
        // override inherits the base replicas; base > 1 is rejected.
        let spec = json!({
            "base": {
                "image": "ghcr.io/acme/web:1.0",
                "replicas": 4
            },
            "environments": {
                "prod": {
                    "needs": { "disk": { "name": "data", "size": "1Gi", "mountPath": "/data" } }
                }
            }
        });
        let errors = validate_application_spec(&spec);
        let replica_errs: Vec<&ValidationError> = errors
            .iter()
            .filter(|e| e.message.contains("single-replica"))
            .collect();
        assert_eq!(replica_errs.len(), 1);
        assert_eq!(replica_errs[0].field, "spec.environments.prod.replicas");
    }

    #[test]
    fn accepts_multi_replica_app_with_no_disk() {
        // The replicas guard fires ONLY when a disk is present; a
        // disk-less app may have replicas > 1.
        let spec = json!({
            "base": {
                "image": "ghcr.io/acme/web:1.0",
                "replicas": 5
            }
        });
        assert!(validate_application_spec(&spec).is_empty());
    }

    #[test]
    fn disk_mount_path_uniqueness_is_app_wide_across_scopes() {
        // mountPath must be unique within the app: the same mountPath in
        // base and in an environment collides.
        let spec = json!({
            "base": {
                "image": "ghcr.io/acme/web:1.0",
                "needs": { "disk": { "name": "data", "size": "1Gi", "mountPath": "/data" } }
            },
            "environments": {
                "prod": {
                    "needs": { "disk": { "name": "other", "size": "1Gi", "mountPath": "/data" } }
                }
            }
        });
        let errors = validate_application_spec(&spec);
        let mp_errs: Vec<&ValidationError> = errors
            .iter()
            .filter(|e| e.message.contains("mountPath") && e.message.contains("/data"))
            .collect();
        assert_eq!(mp_errs.len(), 1);
        assert!(mp_errs[0].message.contains("/data"));
    }

    #[test]
    fn reports_every_disk_violation_no_short_circuit() {
        // Multi-error: a bad size, a bad class, and a relative mountPath
        // in one disk array each surface (one message per offending
        // field). Distinct mountPaths/names so only these three fire.
        let spec = json!({
            "base": {
                "image": "ghcr.io/acme/web:1.0",
                "needs": {
                    "disk": [
                        { "name": "a", "size": "nope", "mountPath": "/a" },
                        { "name": "b", "size": "1Gi", "mountPath": "/b", "class": "shared" },
                        { "name": "c", "size": "1Gi", "mountPath": "rel" }
                    ]
                }
            }
        });
        let errors = validate_application_spec(&spec);
        assert!(errors.iter().any(|e| e.message.contains("quantity")));
        assert!(errors
            .iter()
            .any(|e| e.message.contains("local") && e.message.contains("T2")));
        assert!(errors.iter().any(|e| e.message.contains("absolute")));
        assert_eq!(errors.len(), 3);
    }

    // ---- 2.4h-b: imagePolicy is a CRD-enforced pass-through ----

    #[test]
    fn application_with_image_policy_is_accepted() {
        // `imagePolicy.resolve` is an enum the OpenAPI v3 CRD enforces;
        // there is no cross-field invariant, so the webhook has no rule
        // for it and must accept an Application that declares it.
        let spec = json!({
            "base": {
                "image": "ghcr.io/acme/web:latest",
                "imagePolicy": { "resolve": "off" }
            }
        });
        assert!(validate_application_spec(&spec).is_empty());
    }

    #[test]
    fn application_with_per_env_image_policy_is_accepted() {
        // The per-environment mirror of `imagePolicy` is likewise a pure
        // pass-through for the webhook.
        let spec = json!({
            "base": { "image": "ghcr.io/acme/web:latest" },
            "environments": {
                "prod": { "imagePolicy": { "resolve": "digest" } }
            }
        });
        assert!(validate_application_spec(&spec).is_empty());
    }
}
