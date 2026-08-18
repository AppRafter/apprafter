// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! `expose.public` sits in 8 places across 3 files — 6 fences, a table
//! row and a prose bullet — and 3 of the fences are `kubectl apply` YAML
//! where the apiserver silently PRUNES the unknown key. Only membership
//! catches those, which is why this check runs page-wide.

use docsgen::identifier::{extract_paths, extract_structure, resolve_path, span_path, FieldSet};
use docsgen::scan::{in_scope, scan_markdown, BlockKind};

fn fields() -> FieldSet {
    FieldSet::from_crds(
        &docsgen::repo_root()
            .unwrap()
            .join("operator/charts/apprafter-operator/templates"),
    )
    .unwrap()
}

#[test]
fn only_schema_rooted_tokens_are_extracted() {
    assert_eq!(
        extract_paths("the `expose.public` flag"),
        vec!["expose.public"]
    );
    assert_eq!(
        extract_paths("`spec.base.image` is required"),
        vec!["spec.base.image"]
    );
    // Not schema paths: labels, filenames, module paths, versions.
    assert!(extract_paths("`apprafter.io/hostname`").is_empty());
    assert!(extract_paths("`cue.mod`").is_empty());
    assert!(extract_paths("`plan.md`").is_empty());
    assert!(extract_paths("`v0.2.44`").is_empty());
}

#[test]
fn a_real_field_resolves() {
    let f = fields();
    resolve_path(&f, "expose.port").expect("expose.port exists");
    resolve_path(&f, "spec.base.image").expect("spec.base.image exists");
}

#[test]
fn the_drifted_field_is_rejected() {
    let f = fields();
    let err = resolve_path(&f, "expose.public").unwrap_err();
    assert!(err.contains("public"), "{err}");
}

#[test]
fn a_declared_but_unshipped_need_is_flagged_distinctly() {
    let f = fields();
    // `needs.jetstream` IS in the schema, so membership passes — but it
    // has no provider, and a guide telling a reader to use it is wrong.
    let verdict = resolve_path(&f, "needs.jetstream").expect("declared in the schema");
    assert!(
        verdict.unshipped,
        "must be reported as declared-not-shipped"
    );
}

// ---- corpus lines the first pass got wrong -----------------------------
//
// Each of these is verbatim from the tree and *correct*; the checker
// reported it anyway, which makes it a checker bug and not drift.

#[test]
fn a_filename_that_shares_a_root_stem_is_not_a_path() {
    // README.md:17 and eleven more: "`spec.md` (its `Revision` line) and
    // `plan.md` are the source of truth". `plan.md` was already out
    // because `plan` is not a root; `spec.md` was not.
    assert!(extract_paths("`spec.md` §3.8 — full field reference").is_empty());
    assert!(extract_paths("`spec.json`").is_empty());
    // The guard must not reach a real two-component field.
    assert_eq!(extract_paths("`base.env`"), vec!["base.env"]);
    assert_eq!(extract_paths("`spec.base`"), vec!["spec.base"]);
}

#[test]
fn a_named_multi_claim_list_keeps_its_type_component() {
    // docs/dev-guide/application-cue.md:118-127, verbatim. Losing `pg`
    // across the `[` turned `needs.pg.name` into `needs.name` and
    // reported eleven correct lines.
    let fence = r#"needs: {
    pg: [
        { name: "primary",   selector: { tier: "integrated" } },
        { name: "analytics", selector: { tier: "integrated" } },
    ]
    disk: [
        { name: "uploads", size: "5Gi", mountPath: "/var/uploads" },
    ]
}
"#;
    let found: Vec<String> = extract_structure(fence)
        .into_iter()
        .map(|(_, p)| p)
        .collect();
    assert!(found.contains(&"needs.pg.name".to_string()), "{found:?}");
    assert!(
        found.contains(&"needs.pg.selector.tier".to_string()),
        "{found:?}"
    );
    assert!(
        found.contains(&"needs.disk.mountPath".to_string()),
        "{found:?}"
    );
    assert!(!found.contains(&"needs.name".to_string()), "{found:?}");

    let f = fields();
    for path in &found {
        resolve_path(&f, path).unwrap_or_else(|e| panic!("{path}: {e}"));
    }
}

#[test]
fn a_cue_only_schema_still_backs_the_docs_that_name_it() {
    // gitops-walk.md:43 and troubleshooting.md:93. `Infrastructure` is
    // the manifest `apprafter apply` reads and ships no CRD, so a
    // CRD-only field set calls both lines wrong.
    let f = FieldSet::from_repo(&docsgen::repo_root().unwrap()).unwrap();
    resolve_path(&f, "spec.argocd.bootstrapRepo").expect("Infrastructure field");
    resolve_path(&f, "spec.nodes").expect("Infrastructure field");
    // Harvesting it must not turn the subtree into a free pass.
    assert!(resolve_path(&f, "spec.argocd.bootstrapRepoo").is_err());
    assert!(resolve_path(&f, "spec.nodez").is_err());
}

// ---- corpus survey ----------------------------------------------------

/// Every documented identifier the CRDs do not back, as
/// `file:line  token  reason`.
///
/// `#[ignore]`d: it is a survey, not an assertion. Turning it into one
/// before the corpus is fixed would commit a red gate, and the list it
/// prints is the input to the page fixes rather than their substitute.
#[test]
#[ignore]
fn corpus_identifiers() {
    let root = docsgen::repo_root().unwrap();
    let fields = FieldSet::from_repo(&root).unwrap();

    let mut findings: Vec<(String, usize, String, String)> = Vec::new();
    let mut resolved = 0usize;
    let mut opaque = 0usize;
    let mut files = 0usize;

    for path in in_scope(&root).unwrap() {
        files += 1;
        let shown = path
            .strip_prefix(&root)
            .unwrap_or(&path)
            .display()
            .to_string();
        let source = std::fs::read_to_string(&path).unwrap();

        // (source line, token) from every surface on the page.
        let mut seen: Vec<(usize, String)> = Vec::new();
        for block in scan_markdown(&source) {
            match &block.kind {
                // Front matter is the gate's own exemption channel, not
                // a claim about the product.
                BlockKind::FrontMatter => {}
                BlockKind::InlineSpan => {
                    if let Some(token) = span_path(&block.body) {
                        seen.push((block.line, token));
                    }
                }
                BlockKind::Fence { .. } => {
                    for (offset, line) in block.body.lines().enumerate() {
                        for token in extract_paths(line) {
                            seen.push((block.line + 1 + offset, token));
                        }
                    }
                    for (offset, token) in extract_structure(&block.body) {
                        seen.push((block.line + 1 + offset, token));
                    }
                }
            }
        }
        seen.sort();
        seen.dedup();

        for (line, token) in seen {
            match resolve_path(&fields, &token) {
                Err(reason) => findings.push((shown.clone(), line, token, reason)),
                Ok(verdict) if verdict.unshipped => findings.push((
                    shown.clone(),
                    line,
                    token,
                    "declared in the schema but no provider ships it".to_string(),
                )),
                Ok(verdict) => {
                    resolved += 1;
                    opaque += usize::from(verdict.opaque);
                }
            }
        }
    }

    findings.sort();
    println!("\n=== unresolved / unshipped identifiers ===");
    for (file, line, token, reason) in &findings {
        println!("{file}:{line}  {token}  {reason}");
    }
    println!(
        "\n{} files, {resolved} identifiers resolved ({opaque} of them only \
         because they sit under an opaque subtree), {} flagged",
        files,
        findings.len()
    );
}
