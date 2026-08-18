// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The scanner decides what the gate can see. Its blind spots become the
//! gate's blind spots, so the fence grammar and the scope rule are pinned.

use docsgen::scan::{scan_markdown, Block, BlockKind};

fn blocks(src: &str) -> Vec<Block> {
    scan_markdown(src)
}

#[test]
fn fences_are_parsed_with_any_tag_or_none() {
    let src = "text\n\n```sh\na\n```\n\n```\nb\n```\n\n~~~cue\nc\n~~~\n";
    let fences: Vec<_> = blocks(src)
        .into_iter()
        .filter(|b| matches!(b.kind, BlockKind::Fence { .. }))
        .collect();
    assert_eq!(fences.len(), 3, "sh, untagged and tilde fences all count");
    let tags: Vec<_> = fences
        .iter()
        .map(|b| match &b.kind {
            BlockKind::Fence { tag } => tag.clone(),
            _ => unreachable!(),
        })
        .collect();
    assert_eq!(
        tags,
        vec![Some("sh".to_string()), None, Some("cue".to_string())]
    );
}

#[test]
fn a_longer_closing_run_closes_and_a_shorter_one_does_not() {
    // CommonMark: the closer must be at least as long as the opener.
    let src = "````sh\na\n```\nstill inside\n````\n";
    let fences: Vec<_> = blocks(src)
        .into_iter()
        .filter(|b| matches!(b.kind, BlockKind::Fence { .. }))
        .collect();
    assert_eq!(fences.len(), 1);
    assert!(fences[0].body.contains("still inside"));
}

#[test]
fn inline_spans_inside_a_fence_are_not_spans() {
    let src = "```sh\napprafter app list\n```\n\nRun `apprafter app add` next.\n";
    let spans: Vec<_> = blocks(src)
        .into_iter()
        .filter(|b| matches!(b.kind, BlockKind::InlineSpan))
        .collect();
    assert_eq!(spans.len(), 1, "only the prose span counts");
    assert_eq!(spans[0].body, "apprafter app add");
}

#[test]
fn multi_backtick_spans_are_one_span() {
    let src = "Use ``apprafter app logs -f `name` `` here.\n";
    let spans: Vec<_> = blocks(src)
        .into_iter()
        .filter(|b| matches!(b.kind, BlockKind::InlineSpan))
        .collect();
    assert_eq!(spans.len(), 1);
    assert!(spans[0].body.contains("apprafter app logs"));
}

#[test]
fn every_block_carries_its_source_line() {
    let src = "one\ntwo\n\n```sh\napprafter status\n```\n";
    let fence = blocks(src)
        .into_iter()
        .find(|b| matches!(b.kind, BlockKind::Fence { .. }))
        .unwrap();
    assert_eq!(
        fence.line, 4,
        "diagnostics are useless without the real line"
    );
}

// The three shapes below are not hypothetical: each occurs in the
// corpus, and each is invisible to a scanner that matches only an
// unindented, unquoted fence on a single line.

#[test]
fn a_fence_inside_a_callout_is_a_fence() {
    // Seven of the corpus fences live in a `>` callout. Reading the
    // opener as prose loses the block AND leaves an unterminated
    // backtick run that mis-scans the rest of the callout.
    let src = "> Note:\n>\n> ```cue\n> needs: pg: true\n> ```\n>\n> after\n";
    let fences: Vec<_> = blocks(src)
        .into_iter()
        .filter(|b| matches!(b.kind, BlockKind::Fence { .. }))
        .collect();
    assert_eq!(fences.len(), 1);
    assert_eq!(fences[0].line, 3);
    assert_eq!(
        fences[0].body, "needs: pg: true\n",
        "the quote markers are not part of the snippet"
    );
}

#[test]
fn a_fence_indented_under_a_list_item_is_a_fence() {
    let src = "1. Run it:\n\n     ```sh\n     apprafter status\n     ```\n";
    let fences: Vec<_> = blocks(src)
        .into_iter()
        .filter(|b| matches!(b.kind, BlockKind::Fence { .. }))
        .collect();
    assert_eq!(fences.len(), 1);
    assert_eq!(
        fences[0].body, "apprafter status\n",
        "the item's indentation is not part of the snippet"
    );
}

#[test]
fn a_span_that_wraps_across_a_line_break_is_one_span() {
    // Eleven command invocations in the corpus wrap; a per-line scan
    // loses every one of them.
    let src = "Reach it with `apprafter app status my-service --env\nstaging` today.\n";
    let spans: Vec<_> = blocks(src)
        .into_iter()
        .filter(|b| matches!(b.kind, BlockKind::InlineSpan))
        .collect();
    assert_eq!(spans.len(), 1);
    assert_eq!(
        spans[0].body,
        "apprafter app status my-service --env staging"
    );
    assert_eq!(spans[0].line, 1, "a span is reported where it opens");
}

#[test]
fn an_unmatched_backtick_does_not_swallow_the_paragraph() {
    let src = "A stray ` tick here.\n\nThen `apprafter status` later.\n";
    let spans: Vec<_> = blocks(src)
        .into_iter()
        .filter(|b| matches!(b.kind, BlockKind::InlineSpan))
        .collect();
    assert_eq!(spans.len(), 1);
    assert_eq!(spans[0].body, "apprafter status");
    assert_eq!(spans[0].line, 3);
}

#[test]
fn a_comment_above_a_fence_is_carried_but_not_one_further_up() {
    let src = "<!-- docs: check=cli -->\n```sh\napprafter status\n```\n\n<!-- docs: check=cli -->\n\n```sh\napprafter status\n```\n";
    let fences: Vec<_> = blocks(src)
        .into_iter()
        .filter(|b| matches!(b.kind, BlockKind::Fence { .. }))
        .collect();
    assert_eq!(fences.len(), 2);
    assert_eq!(
        fences[0].tag_line.as_deref(),
        Some("<!-- docs: check=cli -->")
    );
    assert_eq!(
        fences[1].tag_line, None,
        "a blank line between them means the marker is not attached"
    );
}

#[test]
fn an_unclosed_fence_still_yields_a_block() {
    let src = "text\n\n```sh\napprafter status\n";
    let fences: Vec<_> = blocks(src)
        .into_iter()
        .filter(|b| matches!(b.kind, BlockKind::Fence { .. }))
        .collect();
    assert_eq!(fences.len(), 1, "a truncated page must stay checkable");
    assert_eq!(fences[0].body, "apprafter status\n");
}

/// A tripwire, not an assertion: it prints what the scanner sees across
/// the whole corpus so a later reader can tell "the docs changed" from
/// "the scanner changed". Ignored because it shells out to `git` and
/// reads the working tree.
#[test]
#[ignore = "corpus census; run with --ignored --nocapture"]
fn corpus_census() {
    let root = docsgen::repo_root().unwrap();
    let files = docsgen::scan::in_scope(&root).unwrap();
    let mut fences = 0usize;
    let mut spans = 0usize;
    let mut invocations = 0usize;
    let mut tags: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    let mut readme = false;
    for path in &files {
        readme |= path.ends_with("README.md");
        let src = std::fs::read_to_string(path).unwrap();
        for block in scan_markdown(&src) {
            match &block.kind {
                BlockKind::Fence { tag } => {
                    fences += 1;
                    *tags
                        .entry(tag.clone().unwrap_or_else(|| "(none)".into()))
                        .or_default() += 1;
                }
                BlockKind::InlineSpan => {
                    spans += 1;
                    if block.body.starts_with("apprafter ") {
                        invocations += 1;
                    }
                }
            }
        }
    }
    println!(
        "in-scope files:        {} (README.md: {readme})",
        files.len()
    );
    println!("fences:                {fences}");
    println!("inline spans:          {spans}");
    println!("spans `apprafter …`:   {invocations}");
    println!("fence tags:            {tags:?}");
}
