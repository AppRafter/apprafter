// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Assemble the chart CRD (a Helm *template*) from the structural schema
//! and the `_crdMetas` entry.
//!
//! The output is a Helm template, not pure YAML: `metadata.labels`
//! carries the operator chart's `{{ include "apprafter-operator.labels" }}`,
//! which `serde_yaml` cannot represent (it is a Go template, not YAML).
//! We emit a placeholder there and replace it with the include after
//! serialization.

use anyhow::{Context, Result};
use serde_json::{json, Value};

const LABELS_PLACEHOLDER: &str = "__APPRAFTER_HELM_LABELS__";
const LABELS_INCLUDE: &str = "{{- include \"apprafter-operator.labels\" . | nindent 4 }}";

/// Build the full CRD object from the `_crdMetas` entry + the resolved
/// `spec`/`status` schemas. `metadata.labels` is the placeholder.
pub fn build_crd(meta: &Value, spec_schema: Value, status_schema: Value) -> Result<Value> {
    let group = str_field(meta, "group")?;
    let version = str_field(meta, "version")?;
    let scope = str_field(meta, "scope")?;
    let names = meta
        .get("names")
        .context("_crdMetas: missing names")?
        .clone();
    let plural = names
        .get("plural")
        .and_then(Value::as_str)
        .context("_crdMetas.names: missing plural")?;

    let mut version_obj = json!({
        "name": version,
        "served": true,
        "storage": true,
        "schema": { "openAPIV3Schema": {
            "type": "object",
            "required": ["spec"],
            "properties": {
                "apiVersion": { "type": "string" },
                "kind": { "type": "string" },
                "metadata": { "type": "object" },
                "spec": spec_schema,
                "status": status_schema,
            }
        }},
    });
    if let Some(sr) = meta.get("subresources") {
        version_obj["subresources"] = sr.clone();
    }
    if let Some(pc) = meta.get("printerColumns") {
        version_obj["additionalPrinterColumns"] = pc.clone();
    }

    let mut metadata = json!({
        "name": format!("{plural}.{group}"),
        "labels": LABELS_PLACEHOLDER,
    });
    if let Some(ann) = meta.get("annotations") {
        metadata["annotations"] = ann.clone();
    }

    Ok(json!({
        "apiVersion": "apiextensions.k8s.io/v1",
        "kind": "CustomResourceDefinition",
        "metadata": metadata,
        "spec": {
            "group": group,
            "scope": scope,
            "names": names,
            "versions": [version_obj],
        }
    }))
}

fn str_field<'a>(v: &'a Value, key: &str) -> Result<&'a str> {
    v.get(key)
        .and_then(Value::as_str)
        .with_context(|| format!("_crdMetas: missing string field `{key}`"))
}

/// The YAML 1.1 boolean keyword spellings (per the 1.1 `bool` type
/// resolver). These are *not* booleans under YAML 1.2 core schema, so
/// `serde_yaml` (a YAML 1.2 emitter) leaves the `y|n|yes|no|on|off` family
/// as **bare** scalars — but the Kubernetes apiserver parses CRD YAML with
/// `sigs.k8s.io/yaml`, which is YAML 1.1, and coerces an unquoted `off` to
/// boolean `false`. That corrupts a string enum value (e.g.
/// `Application.spec.base.imagePolicy.resolve` = `"digest" | "off"`, ADR
/// 0040): the apiserver's OpenAPI enum silently becomes `["digest", false]`
/// and hard-rejects the string `"off"` the operator emits.
///
/// serde_yaml already single/double-quotes the YAML 1.2 core keywords it
/// recognises (`true`/`True`/`TRUE`, `false`/…, `null`/…, `~`) and any
/// numeric-looking string, so the only gap this transform closes is exactly
/// this `y/n/yes/no/on/off` set. We match the *exact* 1.1 spellings (not a
/// case-insensitive superset) so we never over-quote a genuine string like
/// `"oFf"` or `"yES"`, which are not YAML 1.1 booleans.
const YAML_11_BOOL_SPELLINGS: &[&str] = &[
    "y", "Y", "yes", "Yes", "YES", "n", "N", "no", "No", "NO", "true", "True", "TRUE", "false",
    "False", "FALSE", "on", "On", "ON", "off", "Off", "OFF",
];

/// A printable sentinel wrapped around every YAML-1.1-ambiguous *string*
/// leaf before serialization. A leading `@` (a YAML reserved indicator)
/// forces `serde_yaml` to single-quote the scalar, and serde_yaml emits the
/// sentinel verbatim (no escaping), so stripping the sentinel back out after
/// serialization leaves a correctly single-quoted YAML string. Chosen to be
/// impossible in any real CRD scalar.
const QUOTE_SENTINEL: &str = "@__crdgen_force_quote__@";

/// Walk the CRD `Value` tree and wrap every **string** leaf whose content is
/// a YAML 1.1 boolean spelling (see [`YAML_11_BOOL_SPELLINGS`]) in
/// [`QUOTE_SENTINEL`]. Operating on the typed tree — not the serialized text
/// — is what makes this correct: only genuine `Value::String` leaves are
/// touched, so real boolean/number/`null` values (e.g. `served: true`,
/// `maximum: 65535`, `x-kubernetes-preserve-unknown-fields: true`) keep
/// their bare, correctly-typed YAML form.
fn wrap_ambiguous_strings(v: &mut Value) {
    match v {
        Value::String(s) if YAML_11_BOOL_SPELLINGS.contains(&s.as_str()) => {
            *s = format!("{QUOTE_SENTINEL}{s}{QUOTE_SENTINEL}");
        }
        Value::Array(a) => a.iter_mut().for_each(wrap_ambiguous_strings),
        Value::Object(o) => o.values_mut().for_each(wrap_ambiguous_strings),
        _ => {}
    }
}

/// Render the CRD object to the chart YAML text: GENERATED + SPDX header,
/// the `serde_yaml` body, with the labels placeholder swapped for the
/// helm include and YAML-1.1-ambiguous string scalars force-quoted.
pub fn render(source_file: &str, crd: &Value) -> Result<String> {
    // Force-quote YAML-1.1-ambiguous string scalars (e.g. the `off` enum
    // value) so `sigs.k8s.io/yaml` in the apiserver keeps them strings.
    let mut crd = crd.clone();
    wrap_ambiguous_strings(&mut crd);
    let body = serde_yaml::to_string(&crd).context("serialize CRD to yaml")?;
    // The sentinel round-trips as a single-quoted scalar; stripping it leaves
    // e.g. `- 'off'`.
    let body = body.replace(QUOTE_SENTINEL, "");
    let body = body.replacen(
        &format!("labels: {LABELS_PLACEHOLDER}"),
        &format!("labels:\n    {LABELS_INCLUDE}"),
        1,
    );
    Ok(format!(
        "# GENERATED by crdgen from {source_file} — DO NOT EDIT.\n\
         # Run `just gen-crds` to regenerate; edit the CUE schema instead.\n\
         # SPDX-License-Identifier: MIT\n{body}"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta() -> Value {
        json!({
            "group": "apprafter.io",
            "version": "v1alpha1",
            "scope": "Namespaced",
            "names": { "plural": "applications", "kind": "Application" },
            "annotations": { "argocd.argoproj.io/sync-wave": "-5" },
            "subresources": { "status": {} },
            "printerColumns": [ { "name": "Age", "type": "date", "jsonPath": ".metadata.creationTimestamp" } ]
        })
    }

    #[test]
    fn builds_envelope_with_name_and_subresources() {
        let crd = build_crd(
            &meta(),
            json!({"type": "object"}),
            json!({"type": "object"}),
        )
        .unwrap();
        assert_eq!(crd["metadata"]["name"], json!("applications.apprafter.io"));
        assert_eq!(crd["spec"]["scope"], json!("Namespaced"));
        assert_eq!(
            crd["spec"]["versions"][0]["subresources"]["status"],
            json!({})
        );
        assert_eq!(crd["spec"]["versions"][0]["name"], json!("v1alpha1"));
    }

    #[test]
    fn render_replaces_labels_placeholder_with_helm_include() {
        let crd = build_crd(
            &meta(),
            json!({"type": "object"}),
            json!({"type": "object"}),
        )
        .unwrap();
        let text = render("schemas/v1alpha1", &crd).unwrap();
        assert!(text.contains("# SPDX-License-Identifier: MIT"));
        assert!(text.contains(LABELS_INCLUDE));
        assert!(!text.contains(LABELS_PLACEHOLDER));
        // the include sits under a `labels:` key
        assert!(text.contains("labels:\n    {{- include"));
    }

    /// Render a small ad-hoc CRD body and return the serialized text — a
    /// helper for the quoting tests.
    fn render_value(spec: Value) -> String {
        let crd = build_crd(&meta(), spec, json!({"type": "object"})).unwrap();
        render("schemas/v1alpha1", &crd).unwrap()
    }

    #[test]
    fn render_quotes_a_yaml11_boolean_enum_value() {
        // The ADR-0040 regression: `imagePolicy.resolve` = `"digest" | "off"`.
        // `off` (a YAML 1.1 bool) MUST be quoted so `sigs.k8s.io/yaml` in the
        // apiserver keeps it a string instead of coercing it to `false`.
        let text = render_value(json!({
            "type": "object",
            "properties": { "resolve": { "type": "string", "enum": ["digest", "off"] } }
        }));
        assert!(text.contains("- 'off'"), "text was:\n{text}");
        // `digest` is a safe bare string — left untouched.
        assert!(text.contains("- digest\n"));
        assert!(!text.contains("- 'digest'"));
        // The sentinel never leaks into the output.
        assert!(!text.contains(QUOTE_SENTINEL));
        assert!(!text.contains("crdgen_force_quote"));
    }

    #[test]
    fn render_quotes_every_yaml11_boolean_spelling() {
        // Every YAML 1.1 boolean spelling used as a *string* enum value must
        // come out quoted, whatever the case spelling.
        for v in [
            "off", "on", "yes", "no", "y", "n", "Off", "ON", "Yes", "NO", "Y", "N", "true", "TRUE",
            "false", "FALSE",
        ] {
            let text = render_value(json!({
                "type": "object",
                "properties": { "x": { "type": "string", "enum": [v] } }
            }));
            assert!(
                text.contains(&format!("- '{v}'")),
                "string enum `{v}` should be quoted; text:\n{text}"
            );
        }
    }

    #[test]
    fn render_does_not_over_quote_non_yaml11_strings() {
        // Non-1.1 spellings and ordinary strings stay bare — over-quoting
        // would be a needless behavioral change.
        for v in ["digest", "yES", "tRuE", "oFf", "public", "internal", "vpn"] {
            let text = render_value(json!({
                "type": "object",
                "properties": { "x": { "type": "string", "enum": [v] } }
            }));
            assert!(
                text.contains(&format!("- {v}\n")),
                "string `{v}` must stay bare; text:\n{text}"
            );
        }
    }

    #[test]
    fn render_leaves_genuine_booleans_and_numbers_bare() {
        // The load-bearing invariant of the tree-based approach: a real
        // boolean value (`served: true`, `x-kubernetes-preserve-unknown-fields:
        // true`) or number must NOT be quoted into a string — that would
        // change the CRD's meaning.
        let text = render_value(json!({
            "type": "object",
            "properties": {
                "flag": { "type": "boolean" },
                "n": { "type": "integer", "maximum": 65535 }
            },
            "x-kubernetes-preserve-unknown-fields": true,
            "required": ["flag"]
        }));
        // `served`/`storage` in the version envelope are genuine booleans.
        assert!(text.contains("served: true\n"), "served altered:\n{text}");
        assert!(text.contains("storage: true\n"), "storage altered:\n{text}");
        assert!(
            text.contains("x-kubernetes-preserve-unknown-fields: true\n"),
            "preserve-unknown bool altered:\n{text}"
        );
        assert!(text.contains("maximum: 65535\n"), "number altered:\n{text}");
        // No spurious quoting of any of those.
        assert!(!text.contains("served: 'true'"));
        assert!(!text.contains("x-kubernetes-preserve-unknown-fields: 'true'"));
    }

    #[test]
    fn wrap_ambiguous_strings_targets_only_string_leaves() {
        let mut v = json!({
            "s": "off",
            "b": true,
            "n": 65535,
            "arr": ["yes", "digest", false, 1],
            "nested": { "deep": "no" }
        });
        wrap_ambiguous_strings(&mut v);
        assert_eq!(
            v["s"],
            json!(format!("{QUOTE_SENTINEL}off{QUOTE_SENTINEL}"))
        );
        assert_eq!(v["b"], json!(true)); // genuine bool untouched
        assert_eq!(v["n"], json!(65535)); // number untouched
        assert_eq!(
            v["arr"][0],
            json!(format!("{QUOTE_SENTINEL}yes{QUOTE_SENTINEL}"))
        );
        assert_eq!(v["arr"][1], json!("digest")); // safe string untouched
        assert_eq!(v["arr"][2], json!(false)); // bool in array untouched
        assert_eq!(
            v["nested"]["deep"],
            json!(format!("{QUOTE_SENTINEL}no{QUOTE_SENTINEL}"))
        );
    }
}
