// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Batched validation: one module root, one package per snippet, one
//! `cue vet -c -i ./...`. Sequential validation costs 69 ms per snippet
//! under nix develop and 599 ms outside it; batched, 34 snippets take
//! 33 ms. `-i` is mandatory — without it cue stops at the first failing
//! package and hides the rest, costing one CI round-trip per bad fence.
//!
//! The whole-corpus classification survey that used to live here as an
//! `#[ignore]`d reporter is now `docsgen::gate`, which batches every
//! documented manifest in one `cue vet` on every run. What the survey
//! measured and the gate does not — how many fences carry a package
//! clause without the schema import, and the reverse — is defended by
//! `a_complete_document_needs_both_a_package_and_the_schema_import` in
//! `cuedoc.rs` instead, where it is an assertion rather than a printout.

use docsgen::cuedoc::{validate_documents, Document};

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
