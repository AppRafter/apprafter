// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! `crdgen check` — the local-first CRD drift gate (ADR 0047).
//!
//! Assertion A (CUE ↔ committed): every chart `crd-*.yaml` must be
//! byte-identical to what `crdgen generate` produces from the CUE schemas
//! right now. Catches "edited the CUE, forgot `just gen-crds`" and
//! "hand-edited a GENERATED file".
//!
//! Assertion B (Rust ↔ CUE) — comparing the kube-rs `CustomResourceExt`
//! derivation against the CUE-derived CRD — lands next (ADR 0047
//! Decision #3); it carries a reasoned allowlist for the schemars↔cue
//! shape differences (untagged unions, status).

use anyhow::{bail, Context, Result};

pub fn check() -> Result<()> {
    let rendered = crate::render_all()?;
    let mut drift = Vec::new();
    for (path, expected) in &rendered {
        let committed =
            std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
        if let Some(line) = first_diff(&committed, expected) {
            drift.push((path.display().to_string(), line));
        }
    }
    if !drift.is_empty() {
        eprintln!("CRD drift from CUE — run `just gen-crds`:");
        for (path, line) in &drift {
            eprintln!("  {path}\n    {line}");
        }
        bail!("{} CRD(s) drift from their CUE source", drift.len());
    }
    eprintln!("crd-check: {} CRD(s) match CUE", rendered.len());
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

#[cfg(test)]
mod tests {
    use super::first_diff;

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
