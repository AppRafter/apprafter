// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Batched validation: one module root, one package per snippet, one
//! `cue vet -c -i ./...`. Sequential validation costs 69 ms per snippet
//! under nix develop and 599 ms outside it; batched, 34 snippets take
//! 33 ms. `-i` is mandatory — without it cue stops at the first failing
//! package and hides the rest, costing one CI round-trip per bad fence.

use docsgen::cuedoc::{is_complete_document, validate_documents, Document};
use docsgen::scan::{in_scope, scan_markdown, BlockKind};

#[test]
fn a_valid_application_passes() {
    let doc = Document {
        origin: "t.md:1".into(),
        body: std::fs::read_to_string(
            docsgen::repo_root()
                .unwrap()
                .join("examples/applications/redis-app.cue"),
        )
        .unwrap(),
    };
    let findings = validate_documents(&[doc]).unwrap();
    assert!(findings.is_empty(), "{findings:?}");
}

#[test]
fn an_unknown_field_is_rejected_and_names_its_origin() {
    let doc = Document {
        origin: "guide.md:42".into(),
        body: r#"package example
import v1alpha1 "apprafter.io/schemas/v1alpha1"
app: v1alpha1.#Application & {
    metadata: {name: "web", namespace: "demo"}
    spec: base: {image: "ghcr.io/x/y:1", expose: {port: 8080, public: false}}
}
"#
        .into(),
    };
    let findings = validate_documents(&[doc]).unwrap();
    assert_eq!(findings.len(), 1);
    assert!(findings[0].origin == "guide.md:42", "{findings:?}");
    assert!(findings[0].message.contains("public"), "{findings:?}");
}

#[test]
fn one_bad_document_does_not_hide_the_others() {
    let bad = |o: &str| Document {
        origin: o.into(),
        body: r#"package example
import v1alpha1 "apprafter.io/schemas/v1alpha1"
app: v1alpha1.#Application & {
    metadata: {name: "web", namespace: "demo"}
    spec: base: {image: "ghcr.io/x/y:1", expose: {port: 8080, public: false}}
}
"#
        .into(),
    };
    let findings = validate_documents(&[bad("a.md:1"), bad("b.md:1")]).unwrap();
    assert_eq!(
        findings.len(),
        2,
        "cue vet needs -i or it stops at the first package"
    );
}

#[test]
fn a_kind_that_does_not_exist_is_a_finding_not_a_pass() {
    // The rejected `#`-definition wrapper would have vetted
    // `#x: v1alpha1.#TotallyBogusKind & {}` GREEN. Concrete documents do
    // not: an unknown kind is an undefined field, and it is named.
    let doc = Document {
        origin: "guide.md:9".into(),
        body: "package example\nimport v1alpha1 \"apprafter.io/schemas/v1alpha1\"\nt: v1alpha1.#Tenant & {\n\tmetadata: name: \"acme\"\n}\n".into(),
    };
    let findings = validate_documents(&[doc]).unwrap();
    assert_eq!(findings.len(), 1, "{findings:?}");
    assert!(findings[0].message.contains("#Tenant"), "{findings:?}");
    assert_eq!(findings[0].line, Some(3), "{findings:?}");
}

#[test]
fn a_fragment_is_refused_rather_than_skipped() {
    // `cue vet ./...` skips a directory whose file has no package
    // clause without a word, so a fragment reaching the batch would
    // PASS unexamined. This is the guard that turns that silent gap
    // into a loud one, and it is why the classifier exists.
    let doc = Document {
        origin: "guide.md:7".into(),
        body: "spec: base: expose: {\n\tport: 8080\n}\n".into(),
    };
    let findings = validate_documents(&[doc]).unwrap();
    assert_eq!(findings.len(), 1, "{findings:?}");
    assert!(findings[0].message.contains("package"), "{findings:?}");
}

/// Classify every fence in the corpus and vet the complete documents.
///
/// `#[ignore]`d for the same reason its sibling surveys are: it reports
/// what is there rather than asserting what should be, and the list it
/// prints is the input to the page fixes, not a substitute for them.
///
/// The census columns exist to defend the classification rule, not to
/// pad the output. A fence with a package clause but no schema import
/// unifies against nothing and would vet green whatever it claimed; a
/// fence with the import but no package clause is a fragment cue would
/// skip in silence. Both counts being visible is what makes the rule
/// arguable instead of asserted.
#[test]
#[ignore]
fn corpus_documents() {
    /// The import that makes a CUE file a claim about AppRafter.
    const SCHEMA_IMPORT: &str = "\"apprafter.io/schemas/v1alpha1\"";

    /// The package-clause half of the rule on its own. Appending the
    /// import satisfies the other half without disturbing the first
    /// non-comment line, which is the only thing the package check
    /// reads — so what this measures is exactly "has a package clause".
    fn has_package_clause(body: &str) -> bool {
        is_complete_document(&format!("{body}\nimport v1alpha1 {SCHEMA_IMPORT}\n"))
    }

    let root = docsgen::repo_root().unwrap();

    let mut files = 0usize;
    let mut fences = 0usize;
    let mut cue_tagged = 0usize;
    let mut package_only = Vec::new();
    let mut import_only = Vec::new();
    let mut documents = Vec::new();

    for path in in_scope(&root).unwrap() {
        files += 1;
        let shown = path
            .strip_prefix(&root)
            .unwrap_or(&path)
            .display()
            .to_string();
        let source = std::fs::read_to_string(&path).unwrap();
        for block in scan_markdown(&source) {
            let BlockKind::Fence { tag } = &block.kind else {
                continue;
            };
            fences += 1;
            let tagged = tag.as_deref() == Some("cue");
            cue_tagged += usize::from(tagged);
            let origin = format!("{shown}:{}", block.line);

            // The two halves of the rule, counted apart so the corpus
            // can say whether either alone would have been enough.
            let has_import = block.body.contains(SCHEMA_IMPORT);

            if is_complete_document(&block.body) {
                documents.push((origin, tagged, block.body.clone()));
            } else if has_package_clause(&block.body) {
                package_only.push(origin);
            } else if has_import {
                import_only.push(origin);
            }
        }
    }

    println!("\n=== corpus ===");
    println!("{files} files, {fences} fences, {cue_tagged} of them tagged `cue`");
    println!(
        "{} complete documents ({} of them tagged `cue`)",
        documents.len(),
        documents.iter().filter(|(_, tagged, _)| *tagged).count()
    );
    println!(
        "{} fences with a package clause but no schema import: {package_only:?}",
        package_only.len()
    );
    println!(
        "{} fences with the schema import but no package clause: {import_only:?}",
        import_only.len()
    );

    println!("\n=== documents under test ===");
    for (origin, tagged, _) in &documents {
        println!("{origin}  tag={}", if *tagged { "cue" } else { "other" });
    }

    let batch: Vec<Document> = documents
        .iter()
        .map(|(origin, _, body)| Document {
            origin: origin.clone(),
            body: body.clone(),
        })
        .collect();
    let started = std::time::Instant::now();
    let findings = validate_documents(&batch).unwrap();
    let elapsed = started.elapsed();

    println!("\n=== findings ===");
    for finding in &findings {
        println!("{finding}");
    }
    println!(
        "\n{} document(s) validated in {:?} ({} finding(s))",
        batch.len(),
        elapsed,
        findings.len()
    );
}
