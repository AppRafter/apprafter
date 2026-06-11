// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! OpenAPI → Kubernetes *structural* schema transform (ADR 0047,
//! proven by the Phase-0 Application spike).
//!
//! `cue export --out openapi` emits OpenAPI v3 schemas that are not
//! directly a valid Kubernetes structural schema: they use `$ref`
//! (forbidden in structural schemas) and `oneOf`/`anyOf` unions (the
//! untagged `EnvValue`, the `OneOrMany` `needs` shapes) that a node
//! requiring a single `type` cannot express. The transform:
//!
//!   1. inlines every `$ref` into the referenced schema;
//!   2. collapses any union node (`oneOf`/`anyOf`, or a `$ref` to a
//!      union) to `x-kubernetes-preserve-unknown-fields: true` — the
//!      admission webhook validates the union at runtime;
//!   3. gives every object node an explicit `type: object`.
//!
//! `description` keys are dropped (irrelevant to validation, and the
//! hand-rolled CRDs omit them).

use anyhow::{Context, Result};
use serde_json::{json, Map, Value};

fn is_union(node: &Value) -> bool {
    node.is_object() && (node.get("oneOf").is_some() || node.get("anyOf").is_some())
}

fn preserve_unknown() -> Value {
    json!({ "x-kubernetes-preserve-unknown-fields": true })
}

/// Resolve an OpenAPI schema node into a Kubernetes structural schema,
/// inlining `$ref` against `schemas` (the `components.schemas` map).
pub fn resolve(node: &Value, schemas: &Map<String, Value>) -> Value {
    resolve_inner(node, schemas, &mut Vec::new())
}

fn resolve_inner(node: &Value, schemas: &Map<String, Value>, seen: &mut Vec<String>) -> Value {
    let obj = match node.as_object() {
        Some(o) => o,
        None => return node.clone(),
    };

    // $ref: inline the target (or collapse if it is a union / cyclic).
    if let Some(reference) = obj.get("$ref").and_then(Value::as_str) {
        let name = reference
            .rsplit('/')
            .next()
            .unwrap_or(reference)
            .to_string();
        let target = match schemas.get(&name) {
            Some(t) => t,
            None => return preserve_unknown(),
        };
        if is_union(target) || seen.contains(&name) {
            return preserve_unknown();
        }
        seen.push(name.clone());
        let out = resolve_inner(target, schemas, seen);
        seen.pop();
        return out;
    }

    // A union node the apiserver cannot type structurally.
    if is_union(node) {
        return preserve_unknown();
    }

    let mut out = Map::new();
    for (key, val) in obj {
        match key.as_str() {
            // `description` is irrelevant to validation; `default` is dropped
            // for behavior-neutrality — the hand-rolled CRDs declared no
            // server-side defaults (the renderer applies CUE defaults), so
            // promoting CUE `*x` defaults to CRD defaulting is a separate,
            // deliberate change, not part of this refactor.
            "description" | "default" => continue,
            "properties" => {
                let mut props = Map::new();
                if let Some(p) = val.as_object() {
                    for (pk, pv) in p {
                        props.insert(pk.clone(), resolve_inner(pv, schemas, seen));
                    }
                }
                out.insert("properties".to_string(), Value::Object(props));
            }
            "additionalProperties" => {
                let v = if val.is_object() {
                    resolve_inner(val, schemas, seen)
                } else {
                    val.clone()
                };
                out.insert(key.clone(), v);
            }
            "items" => {
                out.insert(key.clone(), resolve_inner(val, schemas, seen));
            }
            _ => {
                out.insert(key.clone(), val.clone());
            }
        }
    }

    // Structural requirement: an object with members must declare its type.
    let has_members = out.contains_key("properties") || out.contains_key("additionalProperties");
    let typed =
        out.contains_key("type") || out.contains_key("x-kubernetes-preserve-unknown-fields");
    if has_members && !typed {
        out.insert("type".to_string(), json!("object"));
    }

    Value::Object(out)
}

/// Merge CRD-only constraint patches into the resolved `spec` schema.
/// These carry constraints CUE deliberately keeps out of `cue vet` (the
/// `image` non-empty `pattern`, per the CUE-validation policy) so the
/// generated CRD still enforces them. Path mini-language, relative to the
/// spec node: a `name` segment descends into `properties.name`; a
/// `name[*]` segment descends into `properties.name.additionalProperties`;
/// the empty path `""` targets the spec root node itself (e.g. to attach a
/// spec-level `x-kubernetes-validations` CEL rule).
pub fn apply_patches(spec: &mut Value, patches: &Map<String, Value>) -> Result<()> {
    for (path, patch) in patches {
        let node =
            navigate(spec, path).with_context(|| format!("schemaPatch path not found: {path}"))?;
        let obj = node
            .as_object_mut()
            .with_context(|| format!("schemaPatch target is not an object: {path}"))?;
        if let Some(p) = patch.as_object() {
            for (k, v) in p {
                obj.insert(k.clone(), v.clone());
            }
        }
    }
    Ok(())
}

fn navigate<'a>(root: &'a mut Value, path: &str) -> Option<&'a mut Value> {
    // An empty path targets the node itself (the `spec` root) — used to
    // patch spec-level keys like `x-kubernetes-validations`.
    if path.is_empty() {
        return Some(root);
    }
    let mut cur = root;
    for seg in path.split('.') {
        let (name, additional) = match seg.strip_suffix("[*]") {
            Some(n) => (n, true),
            None => (seg, false),
        };
        cur = cur.get_mut("properties")?.get_mut(name)?;
        if additional {
            cur = cur.get_mut("additionalProperties")?;
        }
    }
    Some(cur)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn schemas(v: Value) -> Map<String, Value> {
        v.as_object().unwrap().clone()
    }

    #[test]
    fn collapses_a_oneof_union_to_preserve_unknown() {
        let s = schemas(json!({}));
        let node = json!({ "oneOf": [ {"type": "string"}, {"type": "object"} ] });
        assert_eq!(resolve(&node, &s), preserve_unknown());
    }

    #[test]
    fn env_map_value_ref_to_union_becomes_preserve_unknown() {
        // Mirrors the real shape: env is a map whose values $ref EnvValue,
        // and EnvValue is a oneOf union. The map value must collapse.
        let s = schemas(json!({
            "EnvValue": { "oneOf": [ {"type": "string"}, {"$ref": "#/components/schemas/EnvRef"} ] }
        }));
        let env = json!({ "type": "object", "additionalProperties": {"$ref": "#/components/schemas/EnvValue"} });
        assert_eq!(
            resolve(&env, &s),
            json!({ "type": "object", "additionalProperties": { "x-kubernetes-preserve-unknown-fields": true } })
        );
    }

    #[test]
    fn inlines_a_plain_ref_and_adds_type_to_objects() {
        let s = schemas(json!({
            "Expose": { "type": "object", "required": ["port"], "properties": { "port": {"type": "integer"} } }
        }));
        let node = json!({ "$ref": "#/components/schemas/Expose" });
        assert_eq!(
            resolve(&node, &s),
            json!({ "type": "object", "required": ["port"], "properties": { "port": {"type": "integer"} } })
        );
    }

    #[test]
    fn object_with_properties_but_no_type_gets_type_object() {
        let s = schemas(json!({}));
        let node = json!({ "properties": { "a": {"type": "string"} } });
        let got = resolve(&node, &s);
        assert_eq!(got.get("type").unwrap(), &json!("object"));
    }

    #[test]
    fn drops_description() {
        let s = schemas(json!({}));
        let node = json!({ "type": "string", "description": "noise" });
        assert_eq!(resolve(&node, &s), json!({ "type": "string" }));
    }

    #[test]
    fn cyclic_ref_collapses_instead_of_looping() {
        let s = schemas(json!({
            "Node": { "type": "object", "properties": { "child": {"$ref": "#/components/schemas/Node"} } }
        }));
        let node = json!({ "$ref": "#/components/schemas/Node" });
        // Must terminate; the self-reference collapses to preserve-unknown.
        let got = resolve(&node, &s);
        assert_eq!(
            got.get("properties").unwrap().get("child").unwrap(),
            &preserve_unknown()
        );
    }

    #[test]
    fn drops_default() {
        let s = schemas(json!({}));
        let node = json!({ "type": "string", "default": "internal" });
        assert_eq!(resolve(&node, &s), json!({ "type": "string" }));
    }

    #[test]
    fn apply_patches_sets_pattern_on_nested_and_map_fields() {
        let mut spec = json!({
            "type": "object",
            "properties": {
                "base": { "type": "object", "properties": { "image": {"type": "string"} } },
                "environments": { "type": "object", "additionalProperties": {
                    "type": "object", "properties": { "image": {"type": "string"} } } }
            }
        });
        let patches = schemas(json!({
            "base.image": { "pattern": "^.+$" },
            "environments[*].image": { "pattern": "^.+$" }
        }));
        apply_patches(&mut spec, &patches).unwrap();
        assert_eq!(
            spec["properties"]["base"]["properties"]["image"]["pattern"],
            json!("^.+$")
        );
        assert_eq!(
            spec["properties"]["environments"]["additionalProperties"]["properties"]["image"]
                ["pattern"],
            json!("^.+$")
        );
    }

    #[test]
    fn apply_patches_empty_path_targets_the_spec_root() {
        let mut spec = json!({ "type": "object", "properties": { "a": {"type": "string"} } });
        let patches = schemas(json!({
            "": { "x-kubernetes-validations": [{ "rule": "self == oldSelf" }] }
        }));
        apply_patches(&mut spec, &patches).unwrap();
        assert_eq!(
            spec["x-kubernetes-validations"][0]["rule"],
            json!("self == oldSelf")
        );
        // existing keys untouched
        assert_eq!(spec["properties"]["a"]["type"], json!("string"));
    }

    #[test]
    fn apply_patches_errors_on_missing_path() {
        let mut spec = json!({ "type": "object", "properties": {} });
        let patches = schemas(json!({ "base.image": { "pattern": "^.+$" } }));
        assert!(apply_patches(&mut spec, &patches).is_err());
    }
}
