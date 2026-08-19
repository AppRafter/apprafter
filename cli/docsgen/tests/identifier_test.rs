// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! One schema identifier is written wherever prose is. `expose.network`
//! and its `public` value sit in fences, in table rows and in prose
//! bullets across three different guides (`git grep -n 'expose\.network'
//! -- docs`); and elsewhere the corpus writes whole `apprafter.io`
//! resources inside `kubectl apply` YAML fences, where the apiserver
//! silently PRUNES an unknown key rather than rejecting it. Only
//! membership catches either, which is why this check runs
//! page-wide. (A per-surface tally stood here, of a field spelled
//! `expose.public` that the schema has since replaced — a count is the
//! part of a sentence that goes stale first, and a count of something
//! renamed goes stale invisibly.)
//!
//! The whole-corpus survey that used to live here as an `#[ignore]`d
//! reporter is now `docsgen::gate`, which prints the same census
//! (identifiers resolved, and how many only under an opaque subtree) on
//! every run.

use docsgen::identifier::{extract_paths, extract_structure, resolve_path, FieldSet, KIND_ROOTS};

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
    // README.md and eleven more: "`spec.md` (its `Revision` line) and
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
    // docs/dev-guide/application-cue.md, verbatim. Losing `pg`
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
    // connect-a-git-repository.md and troubleshooting.md.
    // `Infrastructure` is
    // the manifest `apprafter apply` reads and ships no CRD, so a
    // CRD-only field set calls both lines wrong.
    let f = FieldSet::from_repo(&docsgen::repo_root().unwrap()).unwrap();
    resolve_path(&f, "spec.argocd.bootstrapRepo").expect("Infrastructure field");
    resolve_path(&f, "spec.nodes").expect("Infrastructure field");
    // Harvesting it must not turn the subtree into a free pass.
    assert!(resolve_path(&f, "spec.argocd.bootstrapRepoo").is_err());
    assert!(resolve_path(&f, "spec.nodez").is_err());
}

// ---- kind-prefixed paths ----------------------------------------------

#[test]
fn the_kind_table_matches_the_shipped_schemas() {
    // A flat table is only safe while something forces a new kind into
    // it — otherwise adding a CRD silently stops its documented paths
    // from being extracted at all.
    let f = FieldSet::from_repo(&docsgen::repo_root().unwrap()).unwrap();
    assert_eq!(f.kinds(), KIND_ROOTS, "KIND_ROOTS is out of date");
}

#[test]
fn a_kind_prefix_is_extracted_but_a_filename_sharing_the_stem_is_not() {
    assert_eq!(
        extract_paths("`PlatformStack.spec.pin`"),
        vec!["PlatformStack.spec.pin"]
    );
    // Both corpus filenames whose stem is also a kind.
    assert!(extract_paths("`Application.cue`").is_empty());
    assert!(extract_paths("`Infrastructure.cue`").is_empty());
    // Not a kind, and `server_type` is not a field-name shape either.
    assert!(extract_paths("`HetznerCloudState.server_type`").is_empty());
}

#[test]
fn every_kind_prefixed_corpus_token_resolves() {
    // All six, verbatim from the tree.
    let f = FieldSet::from_repo(&docsgen::repo_root().unwrap()).unwrap();
    for token in [
        "PlatformStack.spec.pin",
        "PlatformStack.spec.backup",
        "PlatformStack.spec.backup.bucket",
        "PlatformStack.spec.network.egress.profile",
        "Application.spec.base.image",
    ] {
        resolve_path(&f, token).unwrap_or_else(|e| panic!("{token}: {e}"));
    }
}

#[test]
fn a_kind_prefix_closes_the_unions_silent_pass() {
    // `spec.nodes` is real — on `Infrastructure`. Bare, the union
    // resolves it for any page; named against `PlatformStack`, which
    // has no `nodes`, it is correctly rejected. That difference is the
    // whole point of the prefix.
    let f = FieldSet::from_repo(&docsgen::repo_root().unwrap()).unwrap();
    resolve_path(&f, "spec.nodes").expect("bare: resolves through the union");
    let err = resolve_path(&f, "PlatformStack.spec.nodes").unwrap_err();
    assert!(err.contains("PlatformStack"), "{err}");
    // Same for `expose`, which is Application's and not PlatformStack's.
    assert!(resolve_path(&f, "PlatformStack.spec.base.expose.port").is_err());
    resolve_path(&f, "Application.spec.base.expose.port").expect("Application's");
}

#[test]
fn a_kind_prefix_does_not_excuse_a_drifted_field() {
    let f = FieldSet::from_repo(&docsgen::repo_root().unwrap()).unwrap();
    let err = resolve_path(&f, "Application.spec.base.expose.public").unwrap_err();
    assert!(err.contains("public"), "{err}");
}
