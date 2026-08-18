// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The extraction grammar, without a repository.
//!
//! A path reference has two shapes that read alike and resolve
//! differently — a code span against the repository root, a link target
//! against the page's own directory — and `docs/reference/index.md:10`
//! writes the same text in both roles on one line. Every case here is
//! about telling those apart, so the top-level directory list is a fixed
//! fiction rather than this checkout's: what a claim *looks like* must
//! not change with the tree.

use docsgen::codepath::{is_pattern, references, Reference};

/// The repository's top-level directories, as the extractor is given
/// them — a fixed list here so a test never depends on the checkout.
fn tops() -> Vec<String> {
    ["cli", "operator", "schemas", "docs", "scripts", "e2e"]
        .iter()
        .map(|s| s.to_string())
        .collect()
}

fn found(src: &str) -> Vec<Reference> {
    references(src, &tops())
}

#[test]
fn a_code_span_holding_a_repo_path_is_a_claim() {
    let r = found("See `cli/docsgen/src/gate.rs` for the rules.\n");
    assert_eq!(r.len(), 1, "{r:?}");
    assert_eq!(r[0].path, "cli/docsgen/src/gate.rs");
    assert_eq!(r[0].line, 1);
    assert!(!r[0].page_relative);
}

#[test]
fn a_line_suffix_and_an_anchor_are_stripped() {
    // `file.rs:123` and `file.rs#L4` both name a file that must exist;
    // the suffix names a position inside it, which does not.
    let r = found("At `cli/docsgen/src/gate.rs:406` and `schemas/v1alpha1/x.cue#L2`.\n");
    assert_eq!(r.len(), 2, "{r:?}");
    assert_eq!(r[0].path, "cli/docsgen/src/gate.rs");
    assert_eq!(r[1].path, "schemas/v1alpha1/x.cue");
}

#[test]
fn a_code_span_that_is_a_links_text_is_page_relative() {
    // `docs/reference/index.md:10` is exactly this shape. Read as a
    // repo-root path it is a false positive; the target is what the
    // reader clicks, and it resolves next to the page.
    let r = found("the tree ships as [`cli/commands.json`](cli/commands.json), which\n");
    assert_eq!(r.len(), 1, "{r:?}");
    assert_eq!(r[0].path, "cli/commands.json");
    assert!(r[0].page_relative, "{r:?}");
}

#[test]
fn prose_that_merely_contains_a_slash_is_not_a_claim() {
    // Unbackticked prose is not checked at all: "look under cli/ for
    // the crates" is English, and the backticks are the author saying
    // this one is a path. Every one of the corpus's 54 code-span
    // references is written that way, so the span costs no coverage
    // and buys the whole separation from prose.
    let r = found("Look under cli/docsgen/src for the rules.\n");
    assert!(r.is_empty(), "{r:?}");
}

#[test]
fn a_span_not_starting_at_a_top_level_directory_is_not_a_claim() {
    // `apprafter.io/schemas/v1alpha1` is a CUE import path and
    // `spec/base` is not a file; neither names one.
    let r = found("Import `apprafter.io/schemas/v1alpha1` and set `spec/base` in it.\n");
    assert!(r.is_empty(), "{r:?}");
}

#[test]
fn a_markdown_link_to_a_markdown_page_is_left_to_mkdocs() {
    // `mkdocs build --strict` already resolves these, with anchors.
    // Checking them here would produce a second, weaker opinion.
    let r = found("See [the guide](../dev-guide/quickstart.md#prerequisites).\n");
    assert!(r.is_empty(), "{r:?}");
}

#[test]
fn a_relative_link_to_a_non_markdown_file_is_a_page_relative_claim() {
    let r = found("Run [the walk](../../e2e/mvp.sh) first.\n");
    assert_eq!(r.len(), 1, "{r:?}");
    assert_eq!(r[0].path, "../../e2e/mvp.sh");
    assert!(r[0].page_relative, "{r:?}");
}

#[test]
fn an_external_url_is_not_a_claim() {
    let r = found("See [releases](https://github.com/AppRafter/apprafter/releases).\n");
    assert!(r.is_empty(), "{r:?}");
}

// ---- cases the plan's eight leave open --------------------------------

#[test]
fn a_glob_is_extracted_here_and_answered_at_resolution() {
    // Eight corpus spans are patterns rather than paths — README's
    // `providers/*` and `backstage-plugins/*`, the five `**`
    // exclusions on `docs/contributing/documentation-gate.md`, and
    // `docs/reference/environment.md`'s `e2e/*.sh`. Every one is
    // correct documentation and would be a finding if a pattern were
    // resolved as a filename; they are the ONLY corpus references that
    // do not resolve.
    //
    // They are still extracted. The suppression belongs to the gate,
    // which asks `is_pattern` only after a path has FAILED to resolve
    // — because the characters that make a glob are legal in a
    // filename, and rejecting the shape here would put the repository's
    // three `[...slug]` Next.js routes beyond this check for good.
    let r = found("Under `docs/**/*.md` and `docs/adr/**`, plus `e2e/*.sh`.\n");
    assert_eq!(r.len(), 3, "{r:?}");
    assert!(
        r.iter().all(|found| is_pattern(&found.path)),
        "each one must be recognisable as a pattern downstream: {r:?}"
    );
}

#[test]
fn a_bracketed_filename_is_not_a_pattern_but_a_star_is() {
    // Brackets are glob syntax and are also legal in a filename; this
    // repository has three files that use them, because that is how
    // Next.js spells a dynamic route. Counting them as patterns would
    // suppress those references precisely when one is MOVED — the
    // reference stops resolving, gets read as a pattern, and goes
    // silent, which is the one moment this check exists for.
    //
    // Excluding brackets is free: all eight corpus patterns carry a
    // `*`, and none is bracket-only.
    assert!(!is_pattern("cli/docsgen/src/gate.rs"));
    assert!(!is_pattern("cli"));
    assert!(!is_pattern(
        "landing/cms/src/app/(payload)/api/[...slug]/route.ts"
    ));
    assert!(is_pattern("docs/**/*.md"));
    assert!(is_pattern("providers/*"));
    assert!(is_pattern("docs/{adr,changelog}/**"));
}

#[test]
fn a_relative_target_beginning_with_http_is_still_a_claim() {
    // The URL filter keys on `://`, not on a four-letter prefix. A
    // relative `httpd/conf.yaml` starts with those letters and is a
    // file, and dropping it would be a false negative nothing
    // downstream could recover.
    let r = found("Edit [the config](../httpd/conf.yaml) first.\n");
    assert_eq!(r.len(), 1, "{r:?}");
    assert_eq!(r[0].path, "../httpd/conf.yaml");
    assert!(r[0].page_relative, "{r:?}");
}

#[test]
fn a_directory_keeps_its_trailing_slash_off_the_path() {
    // The corpus writes the bare directory form 28 times (`cli/`,
    // `docs/`, `manifests/` …) against 18 deeper paths, so it is the
    // majority shape, not an edge. `cli/` and `cli` are one claim, and
    // the resolver is given the one spelling the git listing uses.
    let r = found("The crates live under `cli/` and the schemas under `schemas/v1alpha1/`.\n");
    assert_eq!(r.len(), 2, "{r:?}");
    assert_eq!(r[0].path, "cli");
    assert_eq!(r[1].path, "schemas/v1alpha1");
}

#[test]
fn a_span_that_is_a_sentence_about_a_path_is_not_a_path() {
    // Anchored, exactly as `identifier::extract_paths` is anchored: the
    // WHOLE span must be the path. Splitting a span into words to reach
    // a path inside it would admit every shell word in the corpus —
    // `cd cli && cargo test` is one span, and `cd` is not a file.
    let r = found("Run `cd cli && cargo build` from `cli/docsgen`, not `cli/ or operator/`.\n");
    assert_eq!(r.len(), 1, "{r:?}");
    assert_eq!(r[0].path, "cli/docsgen");
}

#[test]
fn a_link_titles_and_angle_brackets_come_off_the_target() {
    // Both are CommonMark link-destination syntax rather than part of
    // the path, and a resolver handed either one reports a file that is
    // named correctly.
    let r = found("[a](<../../e2e/mvp.sh>) and [b](../../e2e/mvp.sh \"the walk\")\n");
    assert_eq!(r.len(), 2, "{r:?}");
    assert!(
        r.iter().all(|found| found.path == "../../e2e/mvp.sh"),
        "{r:?}"
    );
}

#[test]
fn an_image_is_a_link_to_a_file_like_any_other() {
    // `![alt](x.svg)` differs from `[text](x.svg)` by one character and
    // by nothing that matters here: it names a file that must exist.
    let r = found("![logo](../apprafter-logo.svg)\n");
    assert_eq!(r.len(), 1, "{r:?}");
    assert_eq!(r[0].path, "../apprafter-logo.svg");
    assert!(r[0].page_relative, "{r:?}");
}

#[test]
fn a_line_number_is_the_pages_line_not_the_regions() {
    let r = found("intro\n\nSee `cli/docsgen/src/gate.rs`.\n\n[walk](../e2e/mvp.sh)\n");
    assert_eq!(r.len(), 2, "{r:?}");
    assert_eq!(r[0].line, 3);
    assert_eq!(r[1].line, 5);
}

#[test]
fn a_multi_backtick_span_is_read_by_the_same_rule_as_scan_uses() {
    // A run of N backticks opens a span the next run of exactly N
    // closes, so an inner single backtick is content and the span that
    // holds it is one span. Written out here because this module scans
    // spans itself — see its module docs for why — and a scanner that
    // paired single backticks instead would read `cli/nope.rs` as a
    // claim of its own and then mis-pair everything after it.
    let r = found("``a `cli/nope.rs` b`` then `cli/docsgen/src/gate.rs`\n");
    assert_eq!(r.len(), 1, "{r:?}");
    assert_eq!(r[0].path, "cli/docsgen/src/gate.rs");
}
