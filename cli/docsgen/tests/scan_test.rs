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
    // Six of the corpus fences live in a `>` callout. Reading the
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
    // loses every one of them. The fixture is `dev-guide/quickstart.md`
    // lines 354-357 verbatim — every wrapped span in the corpus sits in
    // an indented list item, so an unindented continuation would test a
    // shape that does not occur.
    let src = "- Per-environment overrides via `spec.environments.<env>`: deploy a\n  \
               named environment with `apprafter app add --env staging` (a separate\n  \
               deployment you reach with `apprafter app status my-service --env\n  \
               staging`), and set the cluster's default environment with\n  \
               `apprafter platform env set`.\n";
    let spans: Vec<_> = blocks(src)
        .into_iter()
        .filter(|b| matches!(b.kind, BlockKind::InlineSpan))
        .collect();
    let bodies: Vec<&str> = spans.iter().map(|b| b.body.as_str()).collect();
    assert_eq!(
        bodies,
        vec![
            "spec.environments.<env>",
            "apprafter app add --env staging",
            "apprafter app status my-service --env staging",
            "apprafter platform env set",
        ],
        "the continuation's indent is block structure, not part of the command"
    );
    assert_eq!(spans[2].line, 3, "a span is reported where it opens");
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

// A container marker means one thing outside a fence and another
// inside it. Conflating the two rewrote snippets and, worse, let a
// quoted example close a fence it was inside.

#[test]
fn a_redirect_inside_a_plain_fence_is_not_a_quote_marker() {
    let src = "```sh\napprafter app logs my-service \\\n  > /tmp/logs.txt\n```\n";
    let fences: Vec<_> = blocks(src)
        .into_iter()
        .filter(|b| matches!(b.kind, BlockKind::Fence { .. }))
        .collect();
    assert_eq!(fences.len(), 1);
    assert_eq!(
        fences[0].body, "apprafter app logs my-service \\\n  > /tmp/logs.txt\n",
        "stripping the `>` yields a different, still-plausible command"
    );
}

#[test]
fn a_quoted_fence_marker_does_not_close_a_plain_fence() {
    // The shape every `needs-*-walk.md` page uses to show a callout:
    // a Markdown fence whose body is itself quoted Markdown. Closing
    // on the inner `> ``` ` ends the real block early and leaves a
    // phantom fence that swallows the rest of the page — and the
    // phantom closes at EOF, so "every fence closed" does not catch it.
    let src = "```md\n> Note:\n>\n> ```cue\n> needs: pg: true\n> ```\n```\n\nRun `apprafter app add` next.\n";
    let all = blocks(src);
    let fences: Vec<_> = all
        .iter()
        .filter(|b| matches!(b.kind, BlockKind::Fence { .. }))
        .collect();
    assert_eq!(fences.len(), 1, "the quoted example is body, not a fence");
    assert!(fences[0].body.contains("> needs: pg: true"));
    assert!(!fences[0].unterminated);
    let spans: Vec<_> = all
        .iter()
        .filter(|b| matches!(b.kind, BlockKind::InlineSpan))
        .collect();
    assert_eq!(
        spans.len(),
        1,
        "the claim after the fence must survive: {:?}",
        spans.iter().map(|b| &b.body).collect::<Vec<_>>()
    );
    assert_eq!(spans[0].body, "apprafter app add");
}

// A code span cannot cross a block boundary. Where the region scan
// crossed one, it consumed the next real claim instead of reporting it.

#[test]
fn a_stray_backtick_in_a_heading_does_not_swallow_the_next_claim() {
    let src = "## The ` character\nRun `apprafter status` to check.\n";
    let spans: Vec<_> = blocks(src)
        .into_iter()
        .filter(|b| matches!(b.kind, BlockKind::InlineSpan))
        .collect();
    assert_eq!(spans.len(), 1, "the invocation must not be absorbed");
    assert_eq!(spans[0].body, "apprafter status");
}

#[test]
fn a_wrapped_shell_pipeline_is_not_a_table_row() {
    // `operator-guide/gitops-walk.md` lines 38-39 verbatim. Bounding
    // the region on any `|`-leading line splits this span and deletes
    // the invocation — the same class of loss the bounding prevents.
    let src = "- `kubectl` configured against the cluster (run `apprafter kubeconfig\n  \
               | tee /tmp/kc` and `export KUBECONFIG=/tmp/kc`).\n";
    let bodies: Vec<String> = blocks(src)
        .into_iter()
        .filter(|b| matches!(b.kind, BlockKind::InlineSpan))
        .map(|b| b.body)
        .collect();
    assert_eq!(
        bodies,
        vec![
            "kubectl",
            "apprafter kubeconfig | tee /tmp/kc",
            "export KUBECONFIG=/tmp/kc",
        ]
    );
}

#[test]
fn a_table_row_and_a_list_item_each_bound_the_region() {
    // Real corpus shapes: an option table, then a bullet list.
    let table = "| Flag | Meaning |\n| `--env` | picks ` the environment |\n| `--namespace` | picks the namespace |\n";
    let bodies: Vec<String> = blocks(table)
        .into_iter()
        .filter(|b| matches!(b.kind, BlockKind::InlineSpan))
        .map(|b| b.body)
        .collect();
    assert_eq!(bodies, vec!["--env", "--namespace"]);

    let list = "- Register it with `apprafter app add` and a stray ` here\n- Then run `apprafter app status` on it\n";
    let bodies: Vec<String> = blocks(list)
        .into_iter()
        .filter(|b| matches!(b.kind, BlockKind::InlineSpan))
        .map(|b| b.body)
        .collect();
    assert_eq!(bodies, vec!["apprafter app add", "apprafter app status"]);
}

#[test]
fn a_marker_comment_is_not_also_scanned_as_prose() {
    // A marker that quotes the command it exempts must be counted once,
    // as a marker — not once more as a claim to check.
    let src = "<!-- docs: check=cli run=`apprafter status` -->\n```sh\napprafter status\n```\n";
    let all = blocks(src);
    let spans: Vec<_> = all
        .iter()
        .filter(|b| matches!(b.kind, BlockKind::InlineSpan))
        .collect();
    assert!(
        spans.is_empty(),
        "the marker is a channel, not prose: {:?}",
        spans.iter().map(|b| &b.body).collect::<Vec<_>>()
    );
    assert_eq!(
        all[0].tag_line.as_deref(),
        Some("<!-- docs: check=cli run=`apprafter status` -->")
    );
}

#[test]
fn an_inline_span_never_carries_a_marker() {
    // Markers annotate fences only; a span is exempted at page level.
    let src = "<!-- docs: check=cli -->\nRun `apprafter status` to check.\n";
    let spans: Vec<_> = blocks(src)
        .into_iter()
        .filter(|b| matches!(b.kind, BlockKind::InlineSpan))
        .collect();
    assert_eq!(spans.len(), 1);
    assert_eq!(spans[0].tag_line, None);
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
fn an_unclosed_fence_still_yields_a_block_and_is_flagged() {
    let src = "text\n\n```sh\napprafter status\n";
    let fences: Vec<_> = blocks(src)
        .into_iter()
        .filter(|b| matches!(b.kind, BlockKind::Fence { .. }))
        .collect();
    assert_eq!(fences.len(), 1, "a truncated page must stay checkable");
    assert_eq!(fences[0].body, "apprafter status\n");
    assert!(
        fences[0].unterminated,
        "the flag is the parse's self-check; without it a phantom fence \
         that closes at EOF looks exactly like a real one"
    );
    let closed = "```sh\napprafter status\n```\n";
    let fence = blocks(closed).into_iter().next().unwrap();
    assert!(!fence.unterminated);
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
    let mut truncated: Vec<String> = Vec::new();
    let mut padded: Vec<String> = Vec::new();
    for path in &files {
        readme |= path.ends_with("README.md");
        let src = std::fs::read_to_string(path).unwrap();
        for block in scan_markdown(&src) {
            if block.unterminated {
                truncated.push(format!("{}:{}", path.display(), block.line));
            }
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
                    if block.body.contains("  ") {
                        padded.push(format!(
                            "{}:{} {:?}",
                            path.display(),
                            block.line,
                            block.body
                        ));
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
    println!("unterminated fences:   {}", truncated.len());
    println!("spans with a gap:      {}", padded.len());
    println!("fence tags:            {tags:?}");
    // The parse's self-check, with a mechanism rather than a claim: a
    // fence that closed only because the file ended is either a
    // truncated page or a mis-parse, and both invalidate every count
    // printed above.
    assert!(
        truncated.is_empty(),
        "every in-scope fence must close in the source: {truncated:?}"
    );
    // A wrapped span that keeps its continuation indent shows up as an
    // interior run of spaces, which is how ten spans read before the
    // indent was stripped. A double space is legal Markdown, so this is
    // a tripwire rather than a rule — but it is currently zero across
    // the corpus, and the cheapest way to notice that corruption if it
    // returns.
    assert!(
        padded.is_empty(),
        "a span holding an interior gap is usually a wrapped span that \
         kept its indent: {padded:?}"
    );
}
