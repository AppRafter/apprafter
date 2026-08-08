// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! `crdgen check` — the local-first CRD drift gate (ADR 0047).
//!
//! Assertion A (CUE ↔ committed): every chart `crd-*.yaml` must be
//! byte-identical to what `crdgen generate` produces from the CUE schemas
//! right now. Catches "edited the CUE, forgot `just gen-crds`" and
//! "hand-edited a GENERATED file".
//!
//! Assertion B (Rust ↔ CUE, ADR 0047 Decision #3): the kube-rs
//! `CustomResourceExt` derivation must carry the same field set as the
//! CUE-derived CRD. Unions (`oneOf`/`anyOf`) and
//! `x-kubernetes-preserve-unknown-fields` both read as `opaque`, so the
//! schemars `anyOf` lines up with the CUE preserve-unknown; standard
//! object fields and a reasoned allowlist (the operator-written `status`)
//! cover the legitimate schemars↔cue differences.

use anyhow::{bail, Context, Result};
use serde_json::Value;
use std::collections::BTreeMap;

pub fn check() -> Result<()> {
    let root = crate::find_repo_root()?;
    let schemas = crate::cue::export_schemas(&root)?;
    let rendered = crate::render_all()?;
    let mut problems = Vec::new();

    // Assertion A — every committed chart CRD is byte-identical to CUE.
    for r in &rendered {
        let committed = std::fs::read_to_string(&r.path)
            .with_context(|| format!("read {}", r.path.display()))?;
        if let Some(line) = first_diff(&committed, &r.text) {
            problems.push(format!(
                "[A CUE↔committed] {}\n      {line}",
                r.path.display()
            ));
        }
    }

    // Assertion B — the kube-rs derivation matches the CUE field set.
    for d in assertion_b(&rendered) {
        problems.push(format!("[B Rust↔CUE] {d}"));
    }

    // Assertion R4-M1 — #ApplicationEnvOverride mirrors #ApplicationSpec
    // (structural superset modulo optionality, both directions), so no
    // #ApplicationSpec leaf is silently non-overridable and no override-only
    // leaf is a silent no-op. The two definitions surface in the same
    // OpenAPI export render_all already consumes (components.schemas keys
    // ApplicationSpec / ApplicationEnvOverride).
    let spec = schemas
        .get("ApplicationSpec")
        .context("components.schemas missing ApplicationSpec")?;
    let over = schemas
        .get("ApplicationEnvOverride")
        .context("components.schemas missing ApplicationEnvOverride")?;
    if let Err(errs) = check_env_override_superset(
        &flatten_leaves(spec),
        &flatten_leaves(over),
        ENV_OVERRIDE_ALLOWLIST,
    ) {
        for e in errs {
            problems.push(format!("[R4-M1 envOverride⊇spec] {e}"));
        }
    }

    // Assertion R4-M2 — no server-side `default:` survives into a rendered
    // CRD. structural::resolve strips CUE `*x` defaults; this is the standing
    // gate that turns the R3-H1 one-time grep into a machine assertion.
    for r in &rendered {
        if let Err(errs) = check_no_defaults(&r.crd) {
            for e in errs {
                problems.push(format!("[R4-M2 no-default] {}: {e}", r.component));
            }
        }
    }

    if !problems.is_empty() {
        eprintln!(
            "crd-check FAILED (A: run `just gen-crds`; B: reconcile the type or allowlist; \
             R4-M1: mirror the leaf in the override / allowlist it; R4-M2: drop the default):"
        );
        for p in &problems {
            eprintln!("  {p}");
        }
        bail!("{} CRD drift(s)", problems.len());
    }
    eprintln!(
        "crd-check OK: {} CRD(s) — A (CUE↔committed) + B (Rust↔CUE) + \
         R4-M1 (envOverride⊇spec) + R4-M2 (no default)",
        rendered.len()
    );
    Ok(())
}

// ---- R4-M1: #ApplicationEnvOverride ⊇ #ApplicationSpec (modulo optionality) ----

/// Deliberately-non-overridable #ApplicationSpec leaves — a base leaf that
/// SHOULD NOT be mirrored in #ApplicationEnvOverride (so it can only be set on
/// `base`, never diverge per-env). Empty today: the current override mirrors
/// every base leaf. Each entry MUST carry a reason (enforced by
/// `env_override_allowlist_entries_carry_reasons`), like the assertion-B
/// ALLOWLIST.
const ENV_OVERRIDE_ALLOWLIST: &[(&str, &str)] = &[];

/// One flattened leaf of a CUE-derived OpenAPI type: its coarse type (from
/// `leaf_type`) and whether it is optional (absent from its IMMEDIATE parent's
/// `required` list — a local property, not cascaded from ancestors, so
/// `expose.port` reads REQUIRED even though `expose` itself is optional).
#[derive(Clone, Debug, PartialEq, Eq)]
struct Leaf {
    optional: bool,
    ty: String,
}

/// The coarse type of an OpenAPI node for override-vs-spec comparison. A
/// `$ref` is keyed by its target (`$ref:Needs`) so two types reusing the SAME
/// definition compare equal AND a re-typed ref is caught; a union
/// (`oneOf`/`anyOf`) is `union` so a type-narrowing (`string|[...string]` →
/// `string`) diverges; otherwise the declared scalar/object/array `type`.
fn leaf_type(node: &Value) -> String {
    if let Some(reference) = node.get("$ref").and_then(Value::as_str) {
        let name = reference.rsplit('/').next().unwrap_or(reference);
        return format!("$ref:{name}");
    }
    if node.get("oneOf").is_some() || node.get("anyOf").is_some() {
        return "union".to_string();
    }
    match node.get("type").and_then(Value::as_str) {
        Some(t) => t.to_string(),
        None => "unknown".to_string(),
    }
}

/// Flatten a CUE-derived OpenAPI object schema to `{leaf_path -> Leaf}`,
/// recursing through `properties` (tracking each level's `required` list),
/// `additionalProperties` (`[*]`), and array `items` (`[]`). A `$ref` node and
/// a union node are terminal leaves (keyed by `leaf_type`) — the `$ref` boundary
/// is where "same underlying type ⇒ identical subtree" holds, so we compare at
/// the boundary rather than inlining. `optional` is a LOCAL property: absent
/// from the immediate parent's `required` list (not cascaded from ancestors).
fn flatten_leaves(schema: &Value) -> BTreeMap<String, Leaf> {
    fn walk(node: &Value, path: &str, optional: bool, out: &mut BTreeMap<String, Leaf>) {
        // A $ref or union node is terminal: record it and stop (its subtree is
        // owned by the referenced definition / validated by the webhook).
        let is_ref = node.get("$ref").is_some();
        let is_union = node.get("oneOf").is_some() || node.get("anyOf").is_some();
        let has_props = node.get("properties").is_some();
        let has_ap = node
            .get("additionalProperties")
            .filter(|v| v.is_object())
            .is_some();
        let has_items = node.get("items").filter(|v| v.is_object()).is_some();

        if !path.is_empty() {
            // Record every non-root node (leaf AND composite) so a
            // required↔optional flip or a type change on a nested object is
            // visible, then descend into composites.
            out.insert(
                path.to_string(),
                Leaf {
                    optional,
                    ty: leaf_type(node),
                },
            );
        }
        if is_ref || is_union || !(has_props || has_ap || has_items) {
            return;
        }

        let required: Vec<&str> = node
            .get("required")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        let child = |seg: &str| {
            if path.is_empty() {
                seg.to_string()
            } else {
                format!("{path}.{seg}")
            }
        };
        if let Some(props) = node.get("properties").and_then(Value::as_object) {
            for (k, v) in props {
                walk(v, &child(k), !required.contains(&k.as_str()), out);
            }
        }
        if let Some(ap) = node.get("additionalProperties").filter(|v| v.is_object()) {
            // A map value is inherently optional (the map may be empty).
            walk(ap, &child("[*]"), true, out);
        }
        if let Some(items) = node.get("items").filter(|v| v.is_object()) {
            walk(items, &child("[]"), true, out);
        }
    }
    let mut out = BTreeMap::new();
    walk(schema, "", false, &mut out);
    out
}

/// R4-M1 (pure). Assert `#ApplicationEnvOverride` is a structural superset of
/// `#ApplicationSpec` modulo optionality, both directions. The allowlist
/// entries are `(leaf_path, reason)` for base leaves that are deliberately
/// non-overridable. Failures:
///
/// - every base leaf must exist in the override (allowing base-required →
///   override-optional) — a base leaf absent from the override is silently
///   non-overridable → FAIL, unless allowlisted;
/// - every override leaf must exist in base (a typo / leftover override-only
///   leaf is a silent no-op) — FAIL, unless allowlisted;
/// - a leaf's `ty` must match on both sides (a type-narrowing such as `union`
///   → `string`, or a re-`$ref`, diverges) → FAIL.
fn check_env_override_superset(
    spec: &BTreeMap<String, Leaf>,
    over: &BTreeMap<String, Leaf>,
    allowlist: &[(&str, &str)],
) -> std::result::Result<(), Vec<String>> {
    let allowed = |p: &str| allowlist.iter().any(|(a, _)| *a == p);
    let mut errs = Vec::new();

    for (path, sl) in spec {
        if allowed(path) {
            continue;
        }
        match over.get(path) {
            None => errs.push(format!(
                "base leaf `{path}` ({}) is not overridable — mirror it in \
                 #ApplicationEnvOverride (optional) or add it to ENV_OVERRIDE_ALLOWLIST",
                sl.ty
            )),
            Some(ol) if ol.ty != sl.ty => errs.push(format!(
                "leaf `{path}` type diverges: base {} vs override {} \
                 (type-narrowing loses overridability)",
                sl.ty, ol.ty
            )),
            Some(_) => {}
        }
    }

    for (path, ol) in over {
        if allowed(path) {
            // An allowlisted base leaf that the override nonetheless declares
            // is a contradiction — flag it so the allowlist stays honest.
            errs.push(format!(
                "override-only leaf `{path}` is in ENV_OVERRIDE_ALLOWLIST \
                 (marked non-overridable) yet present in #ApplicationEnvOverride"
            ));
            continue;
        }
        if !spec.contains_key(path) {
            errs.push(format!(
                "override-only leaf `{path}` ({}) has no #ApplicationSpec counterpart \
                 (typo / leftover → a silent no-op override)",
                ol.ty
            ));
        }
    }

    if errs.is_empty() {
        Ok(())
    } else {
        errs.sort();
        Err(errs)
    }
}

// ---- R4-M2: no server-side `default:` in a rendered CRD ----

/// R4-M2 (pure). Assert a rendered CRD carries no OpenAPI `default` key
/// anywhere in its schema tree. `structural::resolve` drops CUE `*x` defaults
/// (behavior-neutrality: the renderer, not the apiserver, applies them), so any
/// `default` reaching a rendered CRD is a regression. Walks the whole `Value`
/// so nested defaults (e.g. `spec.expose.network.default`) are caught, and
/// reports the JSON pointer of each offender.
fn check_no_defaults(crd: &Value) -> std::result::Result<(), Vec<String>> {
    fn walk(node: &Value, ptr: &str, out: &mut Vec<String>) {
        match node {
            Value::Object(map) => {
                for (k, v) in map {
                    if k == "default" {
                        out.push(format!("{ptr}/default = {v}"));
                    }
                    walk(v, &format!("{ptr}/{k}"), out);
                }
            }
            Value::Array(items) => {
                for (i, v) in items.iter().enumerate() {
                    walk(v, &format!("{ptr}/{i}"), out);
                }
            }
            _ => {}
        }
    }
    let mut out = Vec::new();
    walk(crd, "", &mut out);
    if out.is_empty() {
        Ok(())
    } else {
        Err(out)
    }
}

/// The first differing line (1-based) with a short excerpt, or `None` if
/// the two strings are byte-identical.
fn first_diff(committed: &str, expected: &str) -> Option<String> {
    if committed == expected {
        return None;
    }
    let mut cl = committed.lines();
    let mut el = expected.lines();
    let mut n = 0;
    loop {
        n += 1;
        match (cl.next(), el.next()) {
            (Some(a), Some(b)) if a == b => continue,
            (Some(a), Some(b)) => {
                return Some(format!("line {n}: committed {a:?} != generated {b:?}"));
            }
            (Some(a), None) => return Some(format!("line {n}: committed has extra {a:?}")),
            (None, Some(b)) => return Some(format!("line {n}: generated has extra {b:?}")),
            (None, None) => return Some("content equal but byte length differs".into()),
        }
    }
}

// ---- Assertion B helpers (Rust ↔ CUE field-set comparison) ----

/// The `openAPIV3Schema` of a CRD object's (single) version.
fn schema_of(crd: &Value) -> Option<&Value> {
    crd.get("spec")?
        .get("versions")?
        .get(0)?
        .get("schema")?
        .get("openAPIV3Schema")
}

/// A node's coarse kind. Unions (`oneOf`/`anyOf`) and
/// `x-kubernetes-preserve-unknown-fields` both read as `opaque` so the
/// schemars `anyOf` derivation and the CUE-derived preserve-unknown line
/// up (the webhook validates those at runtime, not the CRD schema).
fn kind_of(node: &Value) -> &'static str {
    if node.get("x-kubernetes-preserve-unknown-fields").is_some()
        || node.get("oneOf").is_some()
        || node.get("anyOf").is_some()
    {
        return "opaque";
    }
    match node.get("type").and_then(Value::as_str) {
        Some("object") => "object",
        Some("array") => "array",
        Some("string") => "string",
        Some("integer") => "integer",
        Some("number") => "number",
        Some("boolean") => "boolean",
        _ => "unknown",
    }
}

/// `{field-path -> kind}` for a schema, walking `properties`, one level of
/// `additionalProperties` (`[*]`), and array `items` (`[]`), stopping at
/// opaque nodes.
fn field_kinds(schema: &Value) -> BTreeMap<String, String> {
    fn walk(node: &Value, path: &str, out: &mut BTreeMap<String, String>) {
        let kind = kind_of(node);
        if !path.is_empty() {
            out.insert(path.to_string(), kind.to_string());
        }
        if kind == "opaque" {
            return;
        }
        let child = |seg: &str| {
            if path.is_empty() {
                seg.to_string()
            } else {
                format!("{path}.{seg}")
            }
        };
        if let Some(props) = node.get("properties").and_then(Value::as_object) {
            for (k, v) in props {
                walk(v, &child(k), out);
            }
        }
        if let Some(ap) = node.get("additionalProperties").filter(|a| a.is_object()) {
            walk(ap, &child("[*]"), out);
        }
        if let Some(items) = node.get("items").filter(|i| i.is_object()) {
            walk(items, &child("[]"), out);
        }
    }
    let mut out = BTreeMap::new();
    walk(schema, "", &mut out);
    out
}

/// Reasoned Rust↔CUE CRD-vs-CRD deltas (ADR 0047 Decision #3). Each entry
/// `(component, field-path prefix, reason)` allowlists a path that
/// legitimately differs between the kube-rs (schemars) derivation and the
/// CUE-derived CRD. Every entry MUST carry a non-empty reason
/// (`allowlist_entries_carry_reasons` enforces it).
const ALLOWLIST: &[(&str, &str, &str)] = &[
    (
        "Application",
        "status",
        "operator-written status: the CUE-derived CRD marks it \
         x-kubernetes-preserve-unknown-fields (opaque), the kube-rs type declares the \
         concrete status fields. The status subtree constrains no user input.",
    ),
    (
        "ServiceProvider",
        "status",
        "operator-written status: the CUE-derived CRD marks it \
         x-kubernetes-preserve-unknown-fields (opaque), the kube-rs type declares the \
         concrete status.health field. The status subtree constrains no user input.",
    ),
    (
        "ServiceProvider",
        "spec.config",
        "backend-defined opaque config: the CUE-derived CRD marks it \
         x-kubernetes-preserve-unknown-fields (opaque, restored via a schemaPatch since \
         CUE's `config?: _` top type exports as a bare node), the kube-rs type declares \
         it `Option<serde_json::Value>` which schemars renders as an untyped node. Both \
         accept any object; neither constrains its shape.",
    ),
    (
        "ResourceClaim",
        "status",
        "operator-written status: the CUE-derived CRD marks it \
         x-kubernetes-preserve-unknown-fields (opaque), the kube-rs type declares the \
         concrete status fields (provider, conditions, dbnum, …). The status subtree \
         constrains no user input.",
    ),
    (
        "RetainedClaim",
        "status",
        "no status subresource: RetainedClaim has none (the GC reads spec only), so the \
         kube-rs type declares no status and CustomResourceExt omits it, while crdgen \
         always emits a catch-all x-kubernetes-preserve-unknown-fields (opaque) status \
         node. The status subtree constrains no user input and no subresource is wired \
         (no `subresources` in _crdMetas, so the generated CRD has no status subresource).",
    ),
    (
        "MigrationPlan",
        "status",
        "operator-written status: the CUE-derived CRD marks it \
         x-kubernetes-preserve-unknown-fields (opaque), the kube-rs type declares the \
         concrete status fields (phase, approvedAt, executedSteps, …). The status subtree \
         constrains no user input.",
    ),
    (
        "MigrationPlan",
        "spec.trigger.from",
        "free-form JSON trigger payload: the CUE-derived CRD marks it \
         x-kubernetes-preserve-unknown-fields (opaque, restored via a schemaPatch since \
         CUE's `from?: _` top type exports as a bare node), the kube-rs type declares it \
         `Option<serde_json::Value>` which schemars renders as an untyped node. Both \
         accept any value; neither constrains its shape.",
    ),
    (
        "MigrationPlan",
        "spec.trigger.to",
        "free-form JSON trigger payload: the CUE-derived CRD marks it \
         x-kubernetes-preserve-unknown-fields (opaque, restored via a schemaPatch since \
         CUE's `to?: _` top type exports as a bare node), the kube-rs type declares it \
         `Option<serde_json::Value>` which schemars renders as an untyped node. Both \
         accept any value; neither constrains its shape.",
    ),
    (
        "MigrationPlan",
        "spec.previousSpecSnapshot",
        "free-form JSON snapshot: the CUE-derived CRD marks it \
         x-kubernetes-preserve-unknown-fields (opaque, restored via a schemaPatch since \
         CUE's `previousSpecSnapshot?: {...}` open struct exports as a closed empty \
         object), the kube-rs type declares it `Option<serde_json::Value>` which schemars \
         renders as an untyped node. Both accept any object; neither constrains its shape.",
    ),
    (
        "MigrationPlan",
        "spec.changes.[].from",
        "free-form JSON change-rollup payload (2.16b S1.2): same shape as \
         spec.trigger.from — the CUE-derived CRD marks it \
         x-kubernetes-preserve-unknown-fields (opaque, restored via a `changes[].from` \
         schemaPatch since CUE's `from?: _` top type exports as a bare node), the kube-rs \
         `MigrationChange.from` declares it `Option<serde_json::Value>` which schemars \
         renders as an untyped node. Both accept any value; neither constrains its shape.",
    ),
    (
        "MigrationPlan",
        "spec.changes.[].to",
        "free-form JSON change-rollup payload (2.16b S1.2): same shape as \
         spec.trigger.to — the CUE-derived CRD marks it \
         x-kubernetes-preserve-unknown-fields (opaque, restored via a `changes[].to` \
         schemaPatch since CUE's `to?: _` top type exports as a bare node), the kube-rs \
         `MigrationChange.to` declares it `Option<serde_json::Value>` which schemars \
         renders as an untyped node. Both accept any value; neither constrains its shape.",
    ),
    (
        "SourceCredential",
        "status",
        "operator-written status: the CUE-derived CRD marks it \
         x-kubernetes-preserve-unknown-fields (opaque), the kube-rs type declares the \
         concrete status fields (conditions, coveredRepoPrefixes, coveredHosts, \
         lastValidated). The status subtree constrains no user input.",
    ),
    (
        "SourceCredential",
        "spec.git.backend",
        "backend discriminator union: `#SourceCredentialBackend` is a CUE `oneOf` \
         (sealedSecretRef | openBaoPath) the apiserver cannot type structurally, so the \
         CUE-derived CRD collapses it to x-kubernetes-preserve-unknown-fields (opaque); \
         the kube-rs `SourceBackend` declares both fields optional on one struct (the \
         apiserver rejects oneOf — same shape MigrationPlan's scope uses). The admission \
         webhook enforces exactly-one + the inner non-empty rules. Both accept the \
         apiserver-valid shapes; the union choice is webhook-validated, not CRD-validated.",
    ),
    (
        "SourceCredential",
        "spec.registry.backend",
        "backend discriminator union: identical to spec.git.backend — `#SourceCredentialBackend` \
         is a CUE `oneOf` collapsed to x-kubernetes-preserve-unknown-fields (opaque) in the \
         CUE-derived CRD, while the kube-rs `SourceBackend` declares both fields optional on \
         one struct. The admission webhook enforces exactly-one + the inner non-empty rules.",
    ),
    (
        "PlatformStack",
        "status",
        "operator-written status: the CUE-derived CRD marks it \
         x-kubernetes-preserve-unknown-fields (opaque), the kube-rs type declares the concrete \
         status fields (currentVersion, targetVersion, availableVersion, components, \
         versionHistory, conditions, …). The status subtree constrains no user input.",
    ),
    (
        "SharedVolume",
        "status",
        "operator-written status: the CUE-derived CRD marks it \
         x-kubernetes-preserve-unknown-fields (opaque), the kube-rs type declares the \
         concrete status fields (ready, pvcRef, refCount, capacity, conditions). The status \
         subtree constrains no user input.",
    ),
    (
        "PlatformStack",
        "spec.overrides.[*].values",
        "free-form per-component values merge: the CUE-derived CRD marks it \
         x-kubernetes-preserve-unknown-fields (opaque, restored via a schemaPatch since CUE's \
         `values?: {...}` open struct exports as a closed empty object), the kube-rs \
         `PlatformStackComponentOverride.values` is `Option<serde_json::Value>` which schemars \
         renders as an untyped node. Both accept any object; neither constrains its shape. \
         (The top-level `spec.values` matches on both sides — schemars renders the `#[serde(flatten)] \
         extras` map as a preserve-unknown node, same opaque kind as the CUE patch.)",
    ),
];

/// The kube-rs (`CustomResourceExt::crd()`) CRD for a component, or `None`
/// if it has no Rust type yet (the schema-only CRDs).
fn rust_crd(component: &str) -> Option<Value> {
    use kube::CustomResourceExt;
    let crd = match component {
        "Application" => operator_core::Application::crd(),
        "ServiceProvider" => operator_core::ServiceProvider::crd(),
        "ResourceClaim" => operator_core::ResourceClaim::crd(),
        "RetainedClaim" => operator_core::RetainedClaim::crd(),
        "MigrationPlan" => operator_core::MigrationPlan::crd(),
        "SourceCredential" => operator_core::SourceCredential::crd(),
        "PlatformStack" => operator_core::PlatformStack::crd(),
        "SharedVolume" => operator_core::SharedVolume::crd(),
        _ => return None,
    };
    serde_json::to_value(crd).ok()
}

fn allowed(component: &str, path: &str) -> bool {
    ALLOWLIST.iter().any(|(c, prefix, _)| {
        *c == component && (path == *prefix || path.starts_with(&format!("{prefix}.")))
    })
}

/// Standard Kubernetes object fields the CUE-derived CRD lists in its
/// schema (like the hand-rolled CRDs do) but kube-rs omits from
/// `openAPIV3Schema` — not domain schema, so the comparison ignores them.
fn is_standard(path: &str) -> bool {
    matches!(path, "apiVersion" | "kind" | "metadata") || path.starts_with("metadata.")
}

/// Diff two field-kind maps: a path missing on one side, or a kind
/// mismatch, is a delta — unless it is a standard object field or
/// allowlisted for this component.
fn compare_fields(
    component: &str,
    cue: &BTreeMap<String, String>,
    rust: &BTreeMap<String, String>,
) -> Vec<String> {
    let mut deltas = Vec::new();
    for (path, ck) in cue {
        if is_standard(path) || allowed(component, path) {
            continue;
        }
        match rust.get(path) {
            None => deltas.push(format!("{component}: {path}: CUE {ck}, absent in Rust")),
            Some(rk) if rk != ck => {
                deltas.push(format!("{component}: {path}: CUE {ck} vs Rust {rk}"))
            }
            _ => {}
        }
    }
    for (path, rk) in rust {
        if is_standard(path) || allowed(component, path) || cue.contains_key(path) {
            continue;
        }
        deltas.push(format!("{component}: {path}: Rust {rk}, absent in CUE"));
    }
    deltas
}

/// For every CRD with a kube-rs type, compare its CUE-derived field set to
/// the kube-rs derivation. Returns the non-allowlisted deltas.
fn assertion_b(rendered: &[crate::Rendered]) -> Vec<String> {
    let mut deltas = Vec::new();
    for r in rendered {
        let Some(rust) = rust_crd(r.component) else {
            continue;
        };
        let (Some(cue_schema), Some(rust_schema)) = (schema_of(&r.crd), schema_of(&rust)) else {
            deltas.push(format!("{}: missing openAPIV3Schema", r.component));
            continue;
        };
        deltas.extend(compare_fields(
            r.component,
            &field_kinds(cue_schema),
            &field_kinds(rust_schema),
        ));
    }
    deltas
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn kinds(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(p, k)| (p.to_string(), k.to_string()))
            .collect()
    }

    // ---- R4-M1: env-override superset ----

    /// A minimal #ApplicationSpec-shaped OpenAPI object with a required nested
    /// `expose.port` and an optional nested `expose.protocol`.
    fn spec_fixture() -> Value {
        json!({
            "type": "object",
            "properties": {
                "image": { "type": "string" },
                "expose": {
                    "type": "object",
                    "required": ["port"],
                    "properties": {
                        "port": { "type": "integer" },
                        "protocol": { "type": "string" }
                    }
                }
            }
        })
    }

    #[test]
    fn env_override_superset_detects_missing_nested_leaf() {
        // Override mirrors expose but LACKS the nested expose.protocol leaf →
        // silently non-overridable.
        let over = json!({
            "type": "object",
            "properties": {
                "image": { "type": "string" },
                "expose": {
                    "type": "object",
                    "properties": { "port": { "type": "integer" } }
                }
            }
        });
        let errs = check_env_override_superset(
            &flatten_leaves(&spec_fixture()),
            &flatten_leaves(&over),
            &[],
        )
        .unwrap_err();
        assert!(
            errs.iter()
                .any(|e| e.contains("expose.protocol") && e.contains("not overridable")),
            "{errs:?}"
        );
    }

    #[test]
    fn env_override_superset_ok_when_only_optionality_differs() {
        // Identical to spec EXCEPT expose.port is optional in the override
        // (no `required`) — the intended base-required→override-optional delta.
        let over = json!({
            "type": "object",
            "properties": {
                "image": { "type": "string" },
                "expose": {
                    "type": "object",
                    "properties": {
                        "port": { "type": "integer" },
                        "protocol": { "type": "string" }
                    }
                }
            }
        });
        // sanity: the base marks port required, the override does not.
        assert!(!flatten_leaves(&spec_fixture())["expose.port"].optional);
        assert!(flatten_leaves(&over)["expose.port"].optional);
        assert!(check_env_override_superset(
            &flatten_leaves(&spec_fixture()),
            &flatten_leaves(&over),
            &[],
        )
        .is_ok());
    }

    #[test]
    fn env_override_superset_detects_override_only_leaf() {
        // Override adds a leaf the base lacks → a typo / silent no-op override.
        let over = json!({
            "type": "object",
            "properties": {
                "image": { "type": "string" },
                "expose": {
                    "type": "object",
                    "properties": {
                        "port": { "type": "integer" },
                        "protocol": { "type": "string" },
                        "typo": { "type": "string" }
                    }
                }
            }
        });
        let errs = check_env_override_superset(
            &flatten_leaves(&spec_fixture()),
            &flatten_leaves(&over),
            &[],
        )
        .unwrap_err();
        assert!(
            errs.iter()
                .any(|e| e.contains("expose.typo") && e.contains("no #ApplicationSpec")),
            "{errs:?}"
        );
    }

    #[test]
    fn env_override_superset_detects_type_narrowing() {
        // Base hostname is a union (string|[...string]); override narrows it to
        // a bare string → type diverges.
        let spec = json!({
            "type": "object",
            "properties": {
                "hostname": { "oneOf": [ { "type": "string" }, { "type": "array", "items": { "type": "string" } } ] }
            }
        });
        let over = json!({
            "type": "object",
            "properties": { "hostname": { "type": "string" } }
        });
        let errs = check_env_override_superset(&flatten_leaves(&spec), &flatten_leaves(&over), &[])
            .unwrap_err();
        assert!(
            errs.iter()
                .any(|e| e.contains("hostname") && e.contains("diverges")),
            "{errs:?}"
        );
    }

    #[test]
    fn env_override_superset_ref_boundary_is_a_leaf_comparing_by_target() {
        // #Needs / #ImagePolicy reuse the SAME $ref on both sides → identical
        // leaf type → OK, without inlining the subtree.
        let spec = json!({
            "type": "object",
            "properties": { "needs": { "$ref": "#/components/schemas/Needs" } }
        });
        let over = spec.clone();
        assert!(
            check_env_override_superset(&flatten_leaves(&spec), &flatten_leaves(&over), &[],)
                .is_ok()
        );
        assert_eq!(flatten_leaves(&spec)["needs"].ty, "$ref:Needs");
    }

    #[test]
    fn env_override_superset_allowlist_suppresses_a_base_only_leaf() {
        // A base leaf deliberately non-overridable: absent from the override,
        // but allowlisted → OK.
        let spec = json!({
            "type": "object",
            "properties": { "image": { "type": "string" }, "pinned": { "type": "string" } }
        });
        let over = json!({
            "type": "object",
            "properties": { "image": { "type": "string" } }
        });
        assert!(check_env_override_superset(
            &flatten_leaves(&spec),
            &flatten_leaves(&over),
            &[("pinned", "deliberately base-only for the test")],
        )
        .is_ok());
    }

    #[test]
    fn env_override_allowlist_entries_carry_reasons() {
        for (p, reason) in ENV_OVERRIDE_ALLOWLIST {
            assert!(
                !reason.trim().is_empty(),
                "env-override allowlist {p} has no reason"
            );
        }
    }

    // ---- R4-M2: no default in a rendered CRD ----

    #[test]
    fn no_default_detects_openapi_default() {
        let crd = json!({
            "spec": { "versions": [ { "schema": { "openAPIV3Schema": {
                "properties": { "expose": { "properties": {
                    "network": { "type": "string", "default": "internal" }
                } } }
            } } } ] }
        });
        let errs = check_no_defaults(&crd).unwrap_err();
        assert!(
            errs.iter()
                .any(|e| e.contains("default") && e.contains("internal")),
            "{errs:?}"
        );
    }

    #[test]
    fn no_default_passes_on_clean_crd() {
        let crd = json!({
            "spec": { "versions": [ { "schema": { "openAPIV3Schema": {
                "properties": { "expose": { "properties": {
                    "network": { "type": "string", "enum": ["internal", "public", "vpn"] }
                } } }
            } } } ] }
        });
        assert!(check_no_defaults(&crd).is_ok());
    }

    #[test]
    #[ignore = "calibration probe — run with --ignored --nocapture"]
    fn probe_rust_crd_paths() {
        use kube::CustomResourceExt;
        let crd = serde_json::to_value(operator_core::Application::crd()).unwrap();
        let schema = schema_of(&crd).expect("rust crd schema");
        for (p, k) in field_kinds(schema) {
            println!("RUST {p} = {k}");
        }
    }

    #[test]
    fn allowlist_entries_carry_reasons() {
        for (c, p, reason) in ALLOWLIST {
            assert!(!reason.trim().is_empty(), "allowlist {c}/{p} has no reason");
        }
    }

    #[test]
    fn union_and_preserve_unknown_both_read_as_opaque() {
        assert_eq!(kind_of(&json!({ "oneOf": [] })), "opaque");
        assert_eq!(kind_of(&json!({ "anyOf": [] })), "opaque");
        assert_eq!(
            kind_of(&json!({ "x-kubernetes-preserve-unknown-fields": true })),
            "opaque"
        );
        assert_eq!(kind_of(&json!({ "type": "string" })), "string");
    }

    #[test]
    fn allowed_matches_component_and_prefix() {
        assert!(allowed("Application", "status"));
        assert!(allowed("Application", "status.phase"));
        assert!(!allowed("Application", "statustypo"));
        assert!(!allowed("Application", "spec.base.image"));
        // ServiceProvider allowlists its operator-written status subtree and
        // the backend-opaque spec.config node.
        assert!(allowed("ServiceProvider", "status"));
        assert!(allowed("ServiceProvider", "status.health"));
        assert!(allowed("ServiceProvider", "spec.config"));
        assert!(!allowed("ServiceProvider", "spec.backend"));
        // ResourceClaim allowlists its operator-written status subtree; its
        // typed spec fields are not allowlisted.
        assert!(allowed("ResourceClaim", "status"));
        assert!(allowed("ResourceClaim", "status.dbnum"));
        assert!(!allowed("ResourceClaim", "spec.selector"));
        // RetainedClaim allowlists the crdgen-emitted catch-all status node
        // (it has no status subresource / Rust status type); its spec fields
        // are not allowlisted.
        assert!(allowed("RetainedClaim", "status"));
        assert!(!allowed("RetainedClaim", "spec.claimRef"));
        // MigrationPlan allowlists its operator-written status subtree and the
        // three free-form-JSON spec nodes (preserve-unknown in CUE vs an
        // untyped `serde_json::Value` node in Rust); its other spec fields are
        // not allowlisted.
        assert!(allowed("MigrationPlan", "status"));
        assert!(allowed("MigrationPlan", "status.phase"));
        assert!(allowed("MigrationPlan", "spec.trigger.from"));
        assert!(allowed("MigrationPlan", "spec.trigger.to"));
        assert!(allowed("MigrationPlan", "spec.previousSpecSnapshot"));
        assert!(!allowed("MigrationPlan", "spec.scope"));
        assert!(!allowed("MigrationPlan", "spec.trigger.field"));
        // SourceCredential allowlists its operator-written status subtree and
        // both backend-discriminator union nodes (CUE oneOf → opaque vs the
        // kube-rs two-optional-field struct); the inner backend leaves match by
        // prefix. Its other spec fields (the coverage lists) are not allowlisted.
        assert!(allowed("SourceCredential", "status"));
        assert!(allowed("SourceCredential", "status.conditions"));
        assert!(allowed("SourceCredential", "spec.git.backend"));
        assert!(allowed(
            "SourceCredential",
            "spec.git.backend.sealedSecretRef.name"
        ));
        assert!(allowed("SourceCredential", "spec.registry.backend"));
        assert!(allowed(
            "SourceCredential",
            "spec.registry.backend.openBaoPath"
        ));
        assert!(!allowed("SourceCredential", "spec.git.repoPrefixes"));
        assert!(!allowed("SourceCredential", "spec.registry.hosts"));
        // PlatformStack allowlists its operator-written status subtree and the
        // free-form per-component `overrides[*].values` node (CUE opaque vs an
        // untyped serde_json::Value in Rust); its other typed spec fields
        // (source, values.tier, …) are not allowlisted.
        assert!(allowed("PlatformStack", "status"));
        assert!(allowed("PlatformStack", "status.currentVersion"));
        assert!(allowed("PlatformStack", "status.components.[].ready"));
        assert!(allowed("PlatformStack", "spec.overrides.[*].values"));
        assert!(!allowed("PlatformStack", "spec.source.upstream"));
        assert!(!allowed("PlatformStack", "spec.values.tier"));
        // SharedVolume allowlists its operator-written status subtree; its typed
        // spec fields (size, class) are not allowlisted.
        assert!(allowed("SharedVolume", "status"));
        assert!(allowed("SharedVolume", "status.ready"));
        assert!(!allowed("SharedVolume", "spec.size"));
        assert!(!allowed("SharedVolume", "spec.class"));
        // A component absent from the allowlist matches nothing.
        assert!(!allowed("NotACrd", "status"));
    }

    #[test]
    fn compare_fields_flags_divergence_and_respects_allowlist() {
        let cue = kinds(&[("spec.base.image", "string"), ("status", "opaque")]);
        // Rust drops image, adds a stray field, expands status (allowlisted).
        let rust = kinds(&[
            ("spec.base.replicas", "integer"),
            ("status.phase", "string"),
        ]);
        let deltas = compare_fields("Application", &cue, &rust);
        assert!(
            deltas
                .iter()
                .any(|d| d.contains("spec.base.image") && d.contains("absent in Rust")),
            "{deltas:?}"
        );
        assert!(
            deltas
                .iter()
                .any(|d| d.contains("spec.base.replicas") && d.contains("absent in CUE")),
            "{deltas:?}"
        );
        assert!(
            !deltas.iter().any(|d| d.contains("status")),
            "status must be allowlisted: {deltas:?}"
        );
    }

    #[test]
    fn compare_fields_passes_on_identical_sets() {
        let m = kinds(&[
            ("spec.base.image", "string"),
            ("spec.base.env", "object"),
            ("spec.base.env.[*]", "opaque"),
        ]);
        assert!(compare_fields("Application", &m, &m).is_empty());
    }

    #[test]
    fn standard_object_fields_are_ignored() {
        assert!(is_standard("apiVersion") && is_standard("kind") && is_standard("metadata"));
        assert!(is_standard("metadata.name") && !is_standard("spec"));
        // CUE lists the standard fields, kube-rs omits them → not a delta.
        let cue = kinds(&[
            ("apiVersion", "string"),
            ("kind", "string"),
            ("metadata", "object"),
            ("spec", "object"),
        ]);
        let rust = kinds(&[("spec", "object")]);
        assert!(compare_fields("Application", &cue, &rust).is_empty());
    }

    #[test]
    fn identical_input_has_no_diff() {
        assert!(first_diff("a\nb\n", "a\nb\n").is_none());
    }

    #[test]
    fn detects_a_changed_line() {
        let d = first_diff("a\nb\nc\n", "a\nX\nc\n").unwrap();
        assert!(d.contains("line 2"), "{d}");
    }

    #[test]
    fn detects_an_extra_committed_line() {
        let d = first_diff("a\nb\n", "a\n").unwrap();
        assert!(d.contains("extra"), "{d}");
    }
}
