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

    let app_environment = obj.get("environment").and_then(|v| v.as_str());

    validate_needs_names(base, envs, &mut errors);
    validate_env_refs(base, envs, &mut errors);
    validate_disk_claims(base, envs, &mut errors);
    validate_spec_environment(app_environment, envs, &mut errors);

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

/// 2.6b-4 (ADR 0043): the `needs.disk` VALUE a scope literally declares,
/// if any. `Some(v)` means the scope's `needs.disk` key is present (even
/// if it is an empty array); `None` means the scope does not declare a
/// disk at all (so it INHERITS base's disk under the per-key needs merge).
/// Mirrors the renderer's `if env_needs.disk.is_some()` override pivot.
fn scope_disk_value(scope: Option<&serde_json::Map<String, Value>>) -> Option<&Value> {
    scope
        .and_then(|o| o.get("needs"))
        .and_then(|v| v.as_object())
        .and_then(|n| n.get("disk"))
}

/// 2.6b-4 (ADR 0043): disk-specific value guards, collected across
/// `spec.base.needs.disk` AND every `spec.environments.*.needs.disk`.
/// For each disk entry: the derived/explicit `name` must be a DNS-1123
/// label (it becomes part of the PVC name) and unique within disk;
/// `mountPath` must be absolute AND unique app-wide; `size` must parse as
/// a Kubernetes quantity; `class` must be `local` (replicated/shared are
/// T2-deferred). These guards run on each scope's LITERAL disk value.
///
/// Separately, the replicas invariant runs on the EFFECTIVE-merged view:
/// a scope's effective disk is its own `needs.disk` if it declares the
/// key, else base's `needs.disk` inherited under the per-key needs merge;
/// its effective replicas is the env override else the base value. When
/// the effective disk is non-empty AND effective replicas > 1 the scope
/// is rejected on `<scope>.replicas` — a standalone RWO PVC supports only
/// a single-replica Deployment at launch (per-replica multi-replica is
/// T2). This catches the bypass where an environment overrides only
/// `replicas` (no needs block) yet inherits base's disk.
///
/// Multi-error: one message per offending field, no short-circuit
/// (matching the validator contract).
fn validate_disk_claims(
    base: Option<&serde_json::Map<String, Value>>,
    envs: Option<&serde_json::Map<String, Value>>,
    errors: &mut Vec<ValidationError>,
) {
    let base_replicas = base
        .and_then(|b| b.get("replicas"))
        .and_then(|v| v.as_i64());

    // ---- replicas guard on the EFFECTIVE-merged view ----
    // base's literal disk, inherited by any env that omits the disk key.
    let base_disk = scope_disk_value(base);
    let base_disk_nonempty = base_disk.is_some_and(|v| !disk_entries(v).is_empty());

    let mut replicas_scopes: Vec<(String, bool, Option<i64>)> =
        vec![("spec.base".to_string(), base_disk_nonempty, base_replicas)];
    if let Some(envs_obj) = envs {
        for (env_name, val) in envs_obj {
            let env_obj = val.as_object();
            // Per-key needs merge: the env's disk wins when its `disk` key
            // is present (even empty); else it inherits base's disk.
            let effective_disk = match scope_disk_value(env_obj) {
                Some(v) => v,
                None => match base_disk {
                    Some(v) => v,
                    None => continue,
                },
            };
            if disk_entries(effective_disk).is_empty() {
                continue;
            }
            // Env-override replaces base replicas; else inherit base.
            let effective_replicas = env_obj
                .and_then(|o| o.get("replicas"))
                .and_then(|v| v.as_i64())
                .or(base_replicas);
            replicas_scopes.push((
                format!("spec.environments.{env_name}"),
                true,
                effective_replicas,
            ));
        }
    }

    for (prefix, disk_nonempty, effective_replicas) in replicas_scopes {
        if !disk_nonempty {
            continue;
        }
        if let Some(replicas) = effective_replicas {
            if replicas > 1 {
                errors.push(ValidationError::new(
                    format!("{prefix}.replicas"),
                    "persistent disks currently support single-replica apps; use replicas: 1 (per-replica disks for multi-replica apps are T2)",
                ));
            }
        }
    }

    // ---- per-scope LITERAL disk value guards (name/mountPath/size/class) ----
    // mountPath uniqueness is app-wide (across base + every environment):
    // collect every seen mountPath, reporting on the second+ occurrence.
    let mut seen_mount_paths: Vec<String> = Vec::new();

    // Each scope's own field-path prefix + the scope object.
    let mut value_scopes: Vec<(String, Option<&serde_json::Map<String, Value>>)> =
        vec![("spec.base".to_string(), base)];
    if let Some(envs_obj) = envs {
        for (env_name, val) in envs_obj {
            value_scopes.push((format!("spec.environments.{env_name}"), val.as_object()));
        }
    }

    for (prefix, scope) in value_scopes {
        let Some(value) = scope_disk_value(scope) else {
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
    }
}

/// pg connection-Secret field vocabulary (ADR 0046).
const PG_FIELDS: &[&str] = &["url", "user", "pass", "host", "port", "db"];
/// redis connection-Secret field vocabulary (ADR 0046).
const REDIS_FIELDS: &[&str] = &["url", "user", "pass", "host", "port", "db", "channelPrefix"];
/// Service types that have a connection Secret at launch (ADR 0046).
/// `disk` and the deferred types (jetstream, clickhouse, s3, notifications)
/// do NOT have a connection Secret.
const CLAIM_SUPPORTED_TYPES: &[(&str, &[&str])] = &[("pg", PG_FIELDS), ("redis", REDIS_FIELDS)];
/// Types that exist in the platform but have no connection Secret — any
/// `claim.<type>.*` ref to them is rejected at the webhook.
const CLAIM_UNSUPPORTED_TYPES: &[&str] =
    &["disk", "jetstream", "clickhouse", "s3", "notifications"];

/// 2.12 (ADR 0046): compute the effective `needs` map for a given scope.
/// Base scope: just `base.needs`. Per-environment scope: base.needs
/// merged per-key with environments[name].needs (override-wins per key),
/// matching the renderer's `effective_spec` logic.
///
/// Returns a `serde_json::Map` of `type → value` representing the
/// merged needs for the scope.  The returned map is constructed from
/// references so no cloning of the large tree is needed — callers only
/// read the type keys and their entry shapes.
fn effective_needs_for_scope<'a>(
    base: Option<&'a serde_json::Map<String, Value>>,
    env_scope: Option<&'a serde_json::Map<String, Value>>,
) -> std::collections::HashMap<&'a str, &'a Value> {
    let mut merged: std::collections::HashMap<&str, &Value> = std::collections::HashMap::new();
    // Start from base.
    if let Some(base_needs) = base
        .and_then(|b| b.get("needs"))
        .and_then(|v| v.as_object())
    {
        for (k, v) in base_needs {
            merged.insert(k.as_str(), v);
        }
    }
    // Override per-key with the env scope's needs.
    if let Some(env_needs) = env_scope
        .and_then(|e| e.get("needs"))
        .and_then(|v| v.as_object())
    {
        for (k, v) in env_needs {
            merged.insert(k.as_str(), v);
        }
    }
    merged
}

/// A scope for env-ref validation: (field-path-prefix, env-value-map,
/// env-scope-needs-obj). The third element is `None` for the base scope
/// (no extra needs to merge in) and `Some(env_obj)` for a per-environment
/// scope (its own `needs` override base's per-key).
type EnvRefScope<'a> = (
    String,
    &'a serde_json::Map<String, Value>,
    Option<&'a serde_json::Map<String, Value>>,
);

/// 2.12 (ADR 0046): validate env claim/secret refs across `base.env` and
/// every `environments[*].env`. For each scope the effective needs are
/// base.needs merged per-key with the scope's own needs (override-wins).
/// Multi-error, one message per bad ref, no short-circuit.
fn validate_env_refs(
    base: Option<&serde_json::Map<String, Value>>,
    envs: Option<&serde_json::Map<String, Value>>,
    errors: &mut Vec<ValidationError>,
) {
    // Build the list of (field-path-prefix, env-map, effective-needs-scope-obj)
    // for every scope we need to check.  For base the effective needs is just
    // base.needs; for each environment it's base merged with env's own needs.
    let mut scopes: Vec<EnvRefScope<'_>> = Vec::new();

    if let Some(base_obj) = base {
        if let Some(env_map) = base_obj.get("env").and_then(|v| v.as_object()) {
            scopes.push(("spec.base.env".to_string(), env_map, None));
        }
    }
    if let Some(envs_obj) = envs {
        for (env_name, val) in envs_obj {
            if let Some(env_map) = val
                .as_object()
                .and_then(|o| o.get("env"))
                .and_then(|v| v.as_object())
            {
                scopes.push((
                    format!("spec.environments.{env_name}.env"),
                    env_map,
                    val.as_object(),
                ));
            }
        }
    }

    for (prefix, env_map, env_scope) in scopes {
        let eff_needs = effective_needs_for_scope(base, env_scope);

        for (var_name, val) in env_map {
            match val {
                // A plain string → literal; no validation needed.
                Value::String(_) => {}
                // An object → must be exactly `{"claim": "..."}` or `{"secret": "..."}`
                Value::Object(obj) => {
                    if let Some(claim_path) = obj.get("claim").and_then(|v| v.as_str()) {
                        validate_claim_ref(
                            &format!("{prefix}.{var_name}"),
                            var_name,
                            claim_path,
                            &eff_needs,
                            errors,
                        );
                    } else if let Some(secret_path) = obj.get("secret").and_then(|v| v.as_str()) {
                        validate_secret_ref(
                            &format!("{prefix}.{var_name}"),
                            var_name,
                            secret_path,
                            errors,
                        );
                    }
                    // Other shapes are rejected by the CRD layer; nothing to add here.
                }
                _ => {}
            }
        }
    }
}

/// Validate a `claim` ref string (`"<type>.<field>"` or
/// `"<type>.<name>.<field>"`). Reports one error for each violation.
fn validate_claim_ref(
    field_path: &str,
    _var_name: &str,
    path: &str,
    eff_needs: &std::collections::HashMap<&str, &Value>,
    errors: &mut Vec<ValidationError>,
) {
    let parts: Vec<&str> = path.splitn(4, '.').collect();
    let (service_type, name_opt, field) = match parts.as_slice() {
        [t, f] => (*t, None, *f),
        [t, n, f] => (*t, Some(*n), *f),
        _ => {
            errors.push(ValidationError::new(
                field_path,
                format!(
                    "claim ref {path:?} is malformed; expected \"<type>.<field>\" or \"<type>.<name>.<field>\""
                ),
            ));
            return;
        }
    };

    // Check if the type is a known-unsupported type (disk + deferred).
    if CLAIM_UNSUPPORTED_TYPES.contains(&service_type) {
        errors.push(ValidationError::new(
            field_path,
            format!(
                "claim ref {path:?}: type {service_type:?} has no connection Secret (disk is storage-only; jetstream/clickhouse/s3/notifications are deferred to a future release)"
            ),
        ));
        return;
    }

    // Check if the type is in the effective needs for this scope.
    if !eff_needs.contains_key(service_type) {
        errors.push(ValidationError::new(
            field_path,
            format!(
                "claim ref {path:?}: type {service_type:?} is not declared in needs for this scope; add needs.{service_type} to use a claim ref"
            ),
        ));
        return;
    }

    // Check if the field is in the type's enum.
    let type_fields = CLAIM_SUPPORTED_TYPES
        .iter()
        .find(|(t, _)| *t == service_type)
        .map(|(_, fields)| *fields);

    if let Some(fields) = type_fields {
        if !fields.contains(&field) {
            errors.push(ValidationError::new(
                field_path,
                format!(
                    "claim ref {path:?}: field {field:?} is not valid for {service_type:?}; valid fields are: {}",
                    fields.join(", ")
                ),
            ));
            return;
        }
    }

    // If a name segment is present, validate the named entry exists.
    if let Some(name) = name_opt {
        let need_value = eff_needs[service_type];
        let entry_names = needs_entry_names(need_value);
        let named_entries: Vec<&str> = entry_names.iter().filter_map(|n| *n).collect();
        if !named_entries.contains(&name) {
            // The need is declared but has no entry by this name.
            // Check if it's a scalar (no named entries) vs an array lacking the name.
            let has_any_named = !named_entries.is_empty();
            if has_any_named {
                errors.push(ValidationError::new(
                    field_path,
                    format!(
                        "claim ref {path:?}: no entry named {name:?} in needs.{service_type}; declared names are: {}",
                        named_entries.join(", ")
                    ),
                ));
            } else {
                errors.push(ValidationError::new(
                    field_path,
                    format!(
                        "claim ref {path:?}: named ref (name={name:?}) used but needs.{service_type} is a scalar (unnamed default); omit the name segment or add a named entry"
                    ),
                ));
            }
        }
    }
}

/// Validate a `secret` ref string (`"<name>/<key>"`). Reports one error
/// for each violation.
fn validate_secret_ref(
    field_path: &str,
    _var_name: &str,
    path: &str,
    errors: &mut Vec<ValidationError>,
) {
    let Some(slash_pos) = path.find('/') else {
        errors.push(ValidationError::new(
            field_path,
            format!(
                "secret ref {path:?} is malformed; expected \"<name>/<key>\" (a DNS-1123 Secret name, a '/', then a key matching [-._a-zA-Z0-9]+)"
            ),
        ));
        return;
    };
    let (name, rest) = path.split_at(slash_pos);
    let key = &rest[1..]; // skip leading '/'

    if name.is_empty() {
        errors.push(ValidationError::new(
            field_path,
            format!("secret ref {path:?}: Secret name (before '/') must not be empty"),
        ));
    } else if !is_dns_1123_label(name) {
        errors.push(ValidationError::new(
            field_path,
            format!(
                "secret ref {path:?}: Secret name {name:?} must be a DNS-1123 label (lowercase alphanumeric + '-', start and end alphanumeric, 1..=63 chars)"
            ),
        ));
    }

    if key.is_empty() {
        errors.push(ValidationError::new(
            field_path,
            format!("secret ref {path:?}: key (after '/') must not be empty"),
        ));
    } else if !is_secret_key(key) {
        errors.push(ValidationError::new(
            field_path,
            format!("secret ref {path:?}: key {key:?} must match [-._a-zA-Z0-9]+"),
        ));
    }
}

/// Kubernetes Secret key character set: `[-._a-zA-Z0-9]+` (ADR 0046).
fn is_secret_key(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'.' || b == b'_')
}

/// `spec.environment`, when present and non-empty, must name a declared
/// `spec.environments` key — you cannot select an env you did not define
/// (ADR 0044). An empty string is treated as absent (codebase convention).
fn validate_spec_environment(
    app_environment: Option<&str>,
    envs: Option<&serde_json::Map<String, Value>>,
    errors: &mut Vec<ValidationError>,
) {
    let Some(env) = app_environment.filter(|s| !s.is_empty()) else {
        return;
    };
    let declared = envs.is_some_and(|m| m.contains_key(env));
    if !declared {
        errors.push(ValidationError::new(
            "spec.environment",
            format!("'{env}' is not a declared environment; add it under spec.environments or pick an existing one"),
        ));
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

    // ---- 2.12 (ADR 0046): 2.4e collision guard REMOVED; literal DATABASE_URL is now valid ----

    #[test]
    fn accepts_database_url_literal_under_needs_pg_2_12() {
        // 2.12: the 2.4e collision guard is removed. A literal DATABASE_URL
        // under needs.pg is now valid — the user owns every env-var name.
        let spec = json!({
            "base": {
                "image": "ghcr.io/acme/web:1.0",
                "needs": { "pg": {} },
                "env": { "DATABASE_URL": "postgres://override" }
            }
        });
        assert!(validate_application_spec(&spec).is_empty());
    }

    #[test]
    fn accepts_database_url_literal_in_env_scope_under_needs_pg_2_12() {
        // 2.12: the cross-scope collision guard is removed. A literal
        // DATABASE_URL in an environment scope under base.needs.pg is valid.
        let spec = json!({
            "base": {
                "image": "ghcr.io/acme/web:1.0",
                "needs": { "pg": {} }
            },
            "environments": {
                "prod": { "env": { "DATABASE_URL": "postgres://override" } }
            }
        });
        assert!(validate_application_spec(&spec).is_empty());
    }

    #[test]
    fn accepts_needs_pg_without_database_url_literal() {
        // 2.12: literals are unconstrained; a LOG_LEVEL literal alongside
        // needs.pg is accepted (it was before too, but tested explicitly).
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
        // Without needs.pg a literal DATABASE_URL is a normal env var
        // (unchanged behavior from before 2.4e was ever introduced).
        let spec = json!({
            "base": {
                "image": "ghcr.io/acme/web:1.0",
                "env": { "DATABASE_URL": "postgres://my-own-db" }
            }
        });
        assert!(validate_application_spec(&spec).is_empty());
    }

    #[test]
    fn accepts_redis_reserved_env_literal_when_no_needs_redis() {
        // Without needs.redis a literal REDIS_URL/REDIS_CHANNEL_PREFIX is
        // a normal env var (unchanged behavior).
        let spec = json!({
            "base": {
                "image": "ghcr.io/acme/web:1.0",
                "env": { "REDIS_URL": "redis://my-own", "REDIS_CHANNEL_PREFIX": "p:" }
            }
        });
        assert!(validate_application_spec(&spec).is_empty());
    }

    #[test]
    fn accepts_redis_url_literal_under_needs_redis_2_12() {
        // 2.12: the 2.6 redis collision guard is removed. A literal REDIS_URL
        // under needs.redis is now valid.
        let spec = json!({
            "base": {
                "image": "ghcr.io/acme/web:1.0",
                "needs": { "redis": {} },
                "env": { "REDIS_URL": "redis://x", "REDIS_CHANNEL_PREFIX": "p:" }
            }
        });
        assert!(validate_application_spec(&spec).is_empty());
    }

    // ---- 2.6b-2: (type, name) uniqueness + foldability (collision guard removed) ----

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
    fn accepts_named_pg_reserved_suffix_as_literal_2_12() {
        // 2.12: the named-suffix collision guard is removed. A literal
        // DATABASE_URL_ANALYTICS under needs.pg[name=analytics] is valid.
        let spec = json!({
            "base": {
                "image": "ghcr.io/acme/web:1.0",
                "needs": { "pg": [{ "name": "analytics" }] },
                "env": { "DATABASE_URL_ANALYTICS": "postgres://override" }
            }
        });
        assert!(validate_application_spec(&spec).is_empty());
    }

    #[test]
    fn accepts_named_redis_reserved_suffix_as_literal_2_12() {
        // 2.12: the named-suffix collision guard is removed. Literal
        // REDIS_URL_CACHE / REDIS_CHANNEL_PREFIX_CACHE are valid.
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
    fn rejects_inherited_base_disk_against_env_replicas_override() {
        // 2.6b-4 BYPASS GUARD: base declares needs.disk + replicas:1; an
        // environment overrides ONLY replicas (no needs block), so the
        // effective prod spec INHERITS base's disk and mounts it on 3
        // replicas. The replicas guard must reject on prod.replicas even
        // though prod has no literal needs.disk.
        let spec = json!({
            "base": {
                "image": "ghcr.io/acme/web:1.0",
                "replicas": 1,
                "needs": { "disk": { "name": "data", "size": "1Gi", "mountPath": "/data" } }
            },
            "environments": {
                "prod": { "replicas": 3 }
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
    fn rejects_inherited_base_disk_when_env_redeclares_other_needs_only() {
        // Per-key needs merge: an env that re-declares `needs` with a
        // DIFFERENT type (pg) but NO `disk` key still INHERITS base's
        // disk (the merge is per-key, not whole-block replace). So with
        // env replicas:2 the effective prod still mounts the inherited
        // disk on 2 replicas → rejected on prod.replicas.
        let spec = json!({
            "base": {
                "image": "ghcr.io/acme/web:1.0",
                "replicas": 1,
                "needs": { "disk": { "name": "data", "size": "1Gi", "mountPath": "/data" } }
            },
            "environments": {
                "prod": {
                    "replicas": 2,
                    "needs": { "pg": {} }
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
    fn accepts_inherited_base_disk_with_single_replica_env_override() {
        // The same base, but prod overrides replicas back to 1 → the
        // effective prod mounts the inherited disk on a single replica,
        // which is allowed. (Base replicas:1 already, but the env
        // explicitly re-pins 1 — both effective views must accept.)
        let spec = json!({
            "base": {
                "image": "ghcr.io/acme/web:1.0",
                "replicas": 1,
                "needs": { "disk": { "name": "data", "size": "1Gi", "mountPath": "/data" } }
            },
            "environments": {
                "prod": { "replicas": 1 }
            }
        });
        assert!(validate_application_spec(&spec).is_empty());
    }

    #[test]
    fn rejects_base_disk_inherited_into_default_env_replicas() {
        // base.needs.disk with NO base.replicas (default 1) + an env with
        // replicas:5 and no needs → the inherited disk is rejected against
        // the env override even though base never set replicas.
        let spec = json!({
            "base": {
                "image": "ghcr.io/acme/web:1.0",
                "needs": { "disk": { "name": "data", "size": "1Gi", "mountPath": "/data" } }
            },
            "environments": {
                "staging": { "replicas": 5 }
            }
        });
        let errors = validate_application_spec(&spec);
        let replica_errs: Vec<&ValidationError> = errors
            .iter()
            .filter(|e| e.message.contains("single-replica"))
            .collect();
        assert_eq!(replica_errs.len(), 1);
        assert_eq!(replica_errs[0].field, "spec.environments.staging.replicas");
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

    // ---- 2.9: spec.environment must name a declared spec.environments key ----

    #[test]
    fn rejects_spec_environment_not_in_environments() {
        let spec = json!({
            "base": { "image": "x" },
            "environments": { "dev": {}, "prod": {} },
            "environment": "staging"
        });
        let errors = validate_application_spec(&spec);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].field, "spec.environment");
    }

    #[test]
    fn accepts_spec_environment_matching_declared_key() {
        let spec = json!({
            "base": { "image": "x" },
            "environments": { "dev": {}, "prod": {} },
            "environment": "prod"
        });
        assert!(validate_application_spec(&spec).is_empty());
    }

    #[test]
    fn accepts_spec_without_environment_field() {
        let spec = json!({ "base": { "image": "x" }, "environments": { "dev": {} } });
        assert!(validate_application_spec(&spec).is_empty());
    }

    // ---- 2.12d (ADR 0046): env claim/secret ref validation ----

    #[test]
    fn rejects_claim_ref_type_not_in_needs() {
        // (a) `claim.foo.url` where `foo` is NOT in needs → REJECT.
        let spec = json!({
            "base": {
                "image": "ghcr.io/acme/web:1.0",
                "needs": { "pg": {} },
                "env": { "FOO_URL": { "claim": "foo.url" } }
            }
        });
        let errors = validate_application_spec(&spec);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].field, "spec.base.env.FOO_URL");
        assert!(errors[0].message.contains("foo"));
        assert!(errors[0].message.contains("not declared in needs"));
    }

    #[test]
    fn rejects_claim_ref_bogus_field_for_pg() {
        // (b) `claim.pg.bogus` — field not in the pg enum → REJECT.
        let spec = json!({
            "base": {
                "image": "ghcr.io/acme/web:1.0",
                "needs": { "pg": {} },
                "env": { "DB_BOGUS": { "claim": "pg.bogus" } }
            }
        });
        let errors = validate_application_spec(&spec);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].field, "spec.base.env.DB_BOGUS");
        assert!(errors[0].message.contains("bogus"));
        assert!(errors[0].message.contains("not valid for"));
    }

    #[test]
    fn rejects_claim_ref_disk_has_no_connection_secret() {
        // (c) `claim.disk.url` — disk has no connection Secret → REJECT.
        let spec = json!({
            "base": {
                "image": "ghcr.io/acme/web:1.0",
                "needs": { "disk": { "size": "1Gi", "mountPath": "/data" } },
                "env": { "DISK_URL": { "claim": "disk.url" } }
            }
        });
        let errors = validate_application_spec(&spec);
        // There may be additional errors from the disk validation shape,
        // but the claim-ref error must be present.
        let claim_errs: Vec<&ValidationError> = errors
            .iter()
            .filter(|e| e.field == "spec.base.env.DISK_URL")
            .collect();
        assert_eq!(claim_errs.len(), 1);
        assert!(claim_errs[0].message.contains("disk"));
        assert!(claim_errs[0].message.contains("no connection Secret"));
    }

    #[test]
    fn rejects_claim_ref_named_on_scalar_need() {
        // (d) `claim.pg.main.url` where needs.pg is scalar → REJECT (named
        // ref on scalar). Named ref on array WITH "main" → ACCEPT.
        let spec_reject = json!({
            "base": {
                "image": "ghcr.io/acme/web:1.0",
                "needs": { "pg": {} },
                "env": { "DB_MAIN": { "claim": "pg.main.url" } }
            }
        });
        let errors = validate_application_spec(&spec_reject);
        assert_eq!(errors.len(), 1, "scalar need: named ref must be rejected");
        assert_eq!(errors[0].field, "spec.base.env.DB_MAIN");
        assert!(errors[0].message.contains("named ref") || errors[0].message.contains("scalar"));

        // With a named array entry `main` → ACCEPT.
        let spec_accept = json!({
            "base": {
                "image": "ghcr.io/acme/web:1.0",
                "needs": { "pg": [{ "name": "main" }] },
                "env": { "DB_MAIN": { "claim": "pg.main.url" } }
            }
        });
        assert!(
            validate_application_spec(&spec_accept).is_empty(),
            "named array entry: named ref must be accepted"
        );
    }

    #[test]
    fn rejects_secret_ref_no_slash() {
        // (e) `secret: ""` and `secret: "nokey"` (no `/`) → REJECT.
        let spec_empty = json!({
            "base": {
                "image": "ghcr.io/acme/web:1.0",
                "env": { "KEY": { "secret": "" } }
            }
        });
        let errors = validate_application_spec(&spec_empty);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].field, "spec.base.env.KEY");
        assert!(errors[0].message.contains("malformed"));

        let spec_nokey = json!({
            "base": {
                "image": "ghcr.io/acme/web:1.0",
                "env": { "KEY": { "secret": "nokey" } }
            }
        });
        let errors = validate_application_spec(&spec_nokey);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].field, "spec.base.env.KEY");
        assert!(errors[0].message.contains("malformed"));
    }

    #[test]
    fn accepts_literal_database_url_under_needs_pg_2_12_guard_removed() {
        // (f) a literal `env.DATABASE_URL` under needs.pg → ACCEPT.
        // The 2.4e collision/reserved guard is removed.
        let spec = json!({
            "base": {
                "image": "ghcr.io/acme/web:1.0",
                "needs": { "pg": {} },
                "env": { "DATABASE_URL": "postgres://override" }
            }
        });
        assert!(
            validate_application_spec(&spec).is_empty(),
            "literal DATABASE_URL under needs.pg must be accepted after 2.4e guard removal"
        );
    }

    #[test]
    fn accepts_fully_valid_app_with_literal_claim_and_secret_refs() {
        // (g) a fully-valid app: literal + claim.pg.url + claim.pg.pass
        //     + secret stripe/api-key → ACCEPT.
        let spec = json!({
            "base": {
                "image": "ghcr.io/acme/web:1.0",
                "needs": { "pg": {} },
                "env": {
                    "LOG_LEVEL": "info",
                    "DATABASE_URL": { "claim": "pg.url" },
                    "DB_PASS": { "claim": "pg.pass" },
                    "STRIPE_KEY": { "secret": "stripe/api-key" }
                }
            }
        });
        assert!(
            validate_application_spec(&spec).is_empty(),
            "fully valid app with literal + claim + secret refs must be accepted"
        );
    }

    #[test]
    fn rejects_claim_ref_deferred_type_jetstream() {
        // A claim ref to a deferred type (jetstream) is rejected even
        // if declared in needs.
        let spec = json!({
            "base": {
                "image": "ghcr.io/acme/web:1.0",
                "needs": { "jetstream": {} },
                "env": { "JS_URL": { "claim": "jetstream.url" } }
            }
        });
        let errors = validate_application_spec(&spec);
        let claim_errs: Vec<&ValidationError> = errors
            .iter()
            .filter(|e| e.field == "spec.base.env.JS_URL")
            .collect();
        assert_eq!(claim_errs.len(), 1);
        assert!(
            claim_errs[0].message.contains("deferred")
                || claim_errs[0].message.contains("no connection Secret")
        );
    }

    #[test]
    fn rejects_claim_ref_malformed_too_many_segments() {
        // More than 3 segments is malformed.
        let spec = json!({
            "base": {
                "image": "ghcr.io/acme/web:1.0",
                "needs": { "pg": {} },
                "env": { "DB": { "claim": "pg.a.b.c" } }
            }
        });
        let errors = validate_application_spec(&spec);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].field, "spec.base.env.DB");
        assert!(errors[0].message.contains("malformed"));
    }

    #[test]
    fn claim_ref_env_scope_uses_effective_needs_not_just_env_needs() {
        // `environments.prod.env` has a claim.redis.url ref. base.needs.redis
        // is declared (not prod.needs). The effective needs for prod is
        // base.needs merged with prod.needs (empty) → redis IS in effective
        // needs → ACCEPT.
        let spec = json!({
            "base": {
                "image": "ghcr.io/acme/web:1.0",
                "needs": { "redis": {} }
            },
            "environments": {
                "prod": { "env": { "REDIS_URL": { "claim": "redis.url" } } }
            }
        });
        assert!(
            validate_application_spec(&spec).is_empty(),
            "env scope should inherit base needs when checking claim refs"
        );
    }

    #[test]
    fn claim_ref_env_scope_overridden_need_replaces_base() {
        // prod.needs.pg overrides base.needs.redis (different type, so
        // base.needs.redis is still in the merged effective).
        // prod.env has claim.redis.url → redis IS in effective needs → ACCEPT.
        // prod.env has claim.pg.url → pg IS in effective needs (from prod.needs) → ACCEPT.
        let spec = json!({
            "base": {
                "image": "ghcr.io/acme/web:1.0",
                "needs": { "redis": {} }
            },
            "environments": {
                "prod": {
                    "needs": { "pg": {} },
                    "env": {
                        "REDIS_CONN": { "claim": "redis.url" },
                        "DB_URL": { "claim": "pg.url" }
                    }
                }
            }
        });
        assert!(
            validate_application_spec(&spec).is_empty(),
            "per-key needs merge: both inherited redis and overriding pg should be accessible"
        );
    }

    #[test]
    fn rejects_claim_ref_named_entry_not_found_in_array() {
        // claim.pg.missing.url where needs.pg has [name=main] but not
        // name=missing → REJECT.
        let spec = json!({
            "base": {
                "image": "ghcr.io/acme/web:1.0",
                "needs": { "pg": [{ "name": "main" }] },
                "env": { "DB": { "claim": "pg.missing.url" } }
            }
        });
        let errors = validate_application_spec(&spec);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].field, "spec.base.env.DB");
        assert!(
            errors[0].message.contains("missing") || errors[0].message.contains("no entry named")
        );
    }

    #[test]
    fn accepts_secret_ref_valid_dns_name_and_key() {
        // A well-formed `secret: "stripe/api-key"` → ACCEPT.
        let spec = json!({
            "base": {
                "image": "ghcr.io/acme/web:1.0",
                "env": { "STRIPE_KEY": { "secret": "stripe/api-key" } }
            }
        });
        assert!(validate_application_spec(&spec).is_empty());
    }

    #[test]
    fn rejects_secret_ref_bad_dns_name() {
        // Secret name with uppercase is not DNS-1123 → REJECT.
        let spec = json!({
            "base": {
                "image": "ghcr.io/acme/web:1.0",
                "env": { "KEY": { "secret": "BadName/key" } }
            }
        });
        let errors = validate_application_spec(&spec);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].field, "spec.base.env.KEY");
        assert!(errors[0].message.contains("DNS-1123"));
    }

    #[test]
    fn rejects_secret_ref_empty_key() {
        // `secret: "myname/"` — key is empty → REJECT.
        let spec = json!({
            "base": {
                "image": "ghcr.io/acme/web:1.0",
                "env": { "KEY": { "secret": "myname/" } }
            }
        });
        let errors = validate_application_spec(&spec);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].field, "spec.base.env.KEY");
        assert!(errors[0].message.contains("key"));
    }

    #[test]
    fn env_ref_validation_multi_error_no_short_circuit() {
        // Two bad refs → two errors, no short-circuit.
        let spec = json!({
            "base": {
                "image": "ghcr.io/acme/web:1.0",
                "needs": { "pg": {} },
                "env": {
                    "DB_BOGUS": { "claim": "pg.bogus" },
                    "BAD_SEC": { "secret": "nokey" }
                }
            }
        });
        let errors = validate_application_spec(&spec);
        let claim_errs: Vec<&ValidationError> = errors
            .iter()
            .filter(|e| e.field == "spec.base.env.DB_BOGUS")
            .collect();
        let secret_errs: Vec<&ValidationError> = errors
            .iter()
            .filter(|e| e.field == "spec.base.env.BAD_SEC")
            .collect();
        assert_eq!(claim_errs.len(), 1);
        assert_eq!(secret_errs.len(), 1);
    }
}
