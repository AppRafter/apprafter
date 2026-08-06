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

    if !problems.is_empty() {
        eprintln!("crd-check FAILED (A: run `just gen-crds`; B: reconcile the type or allowlist):");
        for p in &problems {
            eprintln!("  {p}");
        }
        bail!("{} CRD drift(s)", problems.len());
    }
    eprintln!(
        "crd-check OK: {} CRD(s) — A (CUE↔committed) + B (Rust↔CUE)",
        rendered.len()
    );
    Ok(())
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
