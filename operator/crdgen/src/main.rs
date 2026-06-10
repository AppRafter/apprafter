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
const CRDS: &[Crd] = &[Crd {
    component: "Application",
    file_stem: "crd-application",
}];

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
