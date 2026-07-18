// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! crdgen — generate Kubernetes CRDs from the v1alpha1 CUE schemas
//! (ADR 0047). `generate` writes the operator chart's `crd-*.yaml` from
//! `schemas/v1alpha1`; `check` (Phase 2) gates CUE↔committed and
//! Rust↔CUE drift. CUE is the source of truth — this tool is only the
//! transform engine, like `cli-providers/build.rs`.

mod check;
mod cue;
mod envelope;
mod structural;

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::path::PathBuf;

/// An in-scope CRD: the `components.schemas` component name (also the
/// `_crdMetas` key) and the chart file stem.
struct Crd {
    component: &'static str,
    file_stem: &'static str,
}

/// Phase 1 ships Application; the other six land in Phase 3 (ADR 0047).
const CRDS: &[Crd] = &[
    Crd {
        component: "Application",
        file_stem: "crd-application",
    },
    Crd {
        component: "ServiceProvider",
        file_stem: "crd-serviceprovider",
    },
    Crd {
        component: "ResourceClaim",
        file_stem: "crd-resourceclaim",
    },
    Crd {
        component: "RetainedClaim",
        file_stem: "crd-retainedclaim",
    },
    Crd {
        component: "MigrationPlan",
        file_stem: "crd-migrationplan",
    },
    Crd {
        component: "SourceCredential",
        file_stem: "crd-sourcecredential",
    },
    Crd {
        component: "PlatformStack",
        file_stem: "crd-platformstack",
    },
    Crd {
        component: "SharedVolume",
        file_stem: "crd-sharedvolume",
    },
];

/// A rendered CRD: its CUE component name, the chart file path, the YAML
/// text (assertion A compares this to the committed file), and the CRD
/// object (assertion B compares its field set to the kube-rs derivation).
pub(crate) struct Rendered {
    pub component: &'static str,
    pub path: PathBuf,
    pub text: String,
    pub crd: Value,
}

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("generate") => generate(),
        Some("check") => check::check(),
        other => bail!("usage: crdgen <generate|check>; got {other:?}"),
    }
}

fn find_repo_root() -> Result<PathBuf> {
    let mut dir = std::env::current_dir()?;
    loop {
        if dir.join("cue.mod/module.cue").exists() {
            return Ok(dir);
        }
        if !dir.pop() {
            bail!("could not locate repo root (no cue.mod/module.cue above CWD)");
        }
    }
}

/// Render every in-scope CRD to its `(chart path, YAML text)`. Shared by
/// `generate` (writes them) and `check` (compares them to the committed
/// files), so both see byte-identical output.
pub(crate) fn render_all() -> Result<Vec<Rendered>> {
    let root = find_repo_root()?;
    let schemas = cue::export_schemas(&root)?;
    let metas = cue::export_crd_metas(&root)?;
    let templates = root.join("operator/charts/apprafter-operator/templates");

    let mut rendered = Vec::new();
    for crd in CRDS {
        let component = schemas
            .get(crd.component)
            .with_context(|| format!("components.schemas missing {}", crd.component))?;
        let props = component
            .get("properties")
            .and_then(Value::as_object)
            .with_context(|| format!("{} has no properties", crd.component))?;

        let meta = metas
            .get(crd.component)
            .with_context(|| format!("_crdMetas missing {}", crd.component))?;

        let spec_in = props
            .get("spec")
            .cloned()
            .unwrap_or_else(|| json!({ "type": "object" }));
        let mut spec = structural::resolve(&spec_in, &schemas);
        // CRD-only constraints CUE deliberately keeps out of `cue vet`
        // (e.g. the image non-empty pattern) are restored here.
        if let Some(patches) = meta.get("schemaPatches").and_then(Value::as_object) {
            structural::apply_patches(&mut spec, patches)?;
        }

        // status is operator-owned: tolerate any shape the operator writes.
        let status = match props.get("status") {
            Some(s) => {
                let mut r = structural::resolve(s, &schemas);
                // Status-targeted CRD-only constraints CUE can't express
                // (e.g. the `x-kubernetes-list-type: map` SSA-merge marker on
                // `conditions` so independent field managers' conditions
                // coexist instead of overwriting). Same path mini-language as
                // the spec patches, but rooted at the `status` node. Applied
                // BEFORE the preserve-unknown wrapper so a navigate failure
                // surfaces the bad path.
                if let Some(patches) = meta.get("statusSchemaPatches").and_then(Value::as_object) {
                    structural::apply_patches(&mut r, patches)?;
                }
                if let Some(o) = r.as_object_mut() {
                    o.insert("x-kubernetes-preserve-unknown-fields".into(), json!(true));
                }
                r
            }
            None => json!({ "type": "object", "x-kubernetes-preserve-unknown-fields": true }),
        };

        let crd_obj = envelope::build_crd(meta, spec, status)?;
        let source = format!("schemas/v1alpha1 ({} via _crdMetas)", crd.component);
        let text = envelope::render(&source, &crd_obj)?;

        let path = templates.join(format!("{}.yaml", crd.file_stem));
        rendered.push(Rendered {
            component: crd.component,
            path,
            text,
            crd: crd_obj,
        });
    }
    Ok(rendered)
}

fn generate() -> Result<()> {
    for r in render_all()? {
        std::fs::write(&r.path, &r.text).with_context(|| format!("write {}", r.path.display()))?;
        eprintln!("generated {}", r.path.display());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Read a committed chart CRD by file stem (cluster-free, no `cue`).
    /// The file is a Helm template — `metadata.labels` is a single
    /// `{{- include ... }}` directive line that is not plain YAML, so drop
    /// any line containing a `{{` mustache before parsing (it never touches
    /// the `spec.versions[].schema` subtree this guard navigates).
    fn committed_crd(file_stem: &str) -> Value {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../charts/apprafter-operator/templates")
            .join(format!("{file_stem}.yaml"));
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        let plain: String = text
            .lines()
            .filter(|l| !l.contains("{{"))
            .collect::<Vec<_>>()
            .join("\n");
        serde_yaml::from_str(&plain).unwrap_or_else(|e| panic!("parse {}: {e}", path.display()))
    }

    /// 2.16b walk-found: `status.lastAppliedSpec` rendered as a bare
    /// `{type: object}` is a STRUCTURAL PRUNING BOUNDARY — the status
    /// root's preserve-unknown does NOT cascade past a child that declares
    /// its own `type: object`, so the apiserver strips every key inside it
    /// and the migration baseline round-trips as `{}` (gating never fires).
    /// The `statusSchemaPatches` entry in `_crdMetas: Application` restores
    /// `x-kubernetes-preserve-unknown-fields: true` ON the node so its
    /// children survive. This guards the committed CRD without a cluster
    /// (`crd-validate` can't catch it — the CRD stays structurally valid,
    /// it just prunes).
    #[test]
    fn application_status_last_applied_spec_is_preserve_unknown() {
        let crd = committed_crd("crd-application");
        let laf = &crd["spec"]["versions"][0]["schema"]["openAPIV3Schema"]["properties"]["status"]
            ["properties"]["lastAppliedSpec"];
        assert_eq!(
            laf["type"],
            json!("object"),
            "status.lastAppliedSpec must stay a typed object node"
        );
        assert_eq!(
            laf["x-kubernetes-preserve-unknown-fields"],
            json!(true),
            "status.lastAppliedSpec lost preserve-unknown — the apiserver \
             will prune the 2.16b migration baseline to {{}} and gating \
             never fires (re-run `just gen-crds`)"
        );
    }
}
