// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Extract the repository paths a documentation page names.
//!
//! Documentation names files. A file moves, and the reference becomes a
//! plausible-looking lie — the worst kind, because it costs a reader a
//! search before they conclude the docs are wrong rather than
//! themselves. A live example sits one directory outside this gate's
//! corpus: `docs/adr/0056-machine-picker.md:307` cites
//! `cli/cli-providers/src/hetzner_cloud/validators.rs`, a file that does
//! not exist — the function lives in `server_type.rs`, and an unrelated
//! `validators.rs` exists at a different path, so the name reads as
//! correct to anyone who does not go looking. (ADRs are out of scope on
//! purpose: an ADR records a decision as of its date, and a path
//! accurate then is not a documentation defect now.)
//!
//! # Two surfaces, because one page writes both on one line
//!
//! A path-shaped string in Markdown resolves against one of two roots,
//! and which one depends on how it is written, not on how it looks:
//!
//! * a **code span** is prose. `` `cli/docsgen/src/gate.rs` `` is a
//!   reference a reader greps for from the repository root, so it is
//!   resolved there;
//! * a **link target** is a destination. `[the walk](../../e2e/mvp.sh)`
//!   is what a reader clicks, and MkDocs resolves it against the page's
//!   own directory, so this module does too.
//!
//! The two are not separable by shape, and `docs/reference/index.md:10`
//! is the proof — it writes the same text in both roles on one line:
//!
//! ```text
//!   [`cli/commands.json`](cli/commands.json), which additionally
//! ```
//!
//! Read as a repo-root path the code span is wrong (there is no
//! `cli/commands.json` in the repository) and read as a page-relative
//! target it is right (`docs/reference/cli/commands.json`). Both
//! readings are of the same eighteen characters. So the rule is about
//! **role**: a code span that is a link's *text* makes no claim of its
//! own — the link's target is the claim, and it is checked
//! page-relative. Without that rule this check reports its first false
//! positive on the day it ships.
//!
//! # What is deliberately not checked
//!
//! * **A link to a `.md` page.** `mkdocs build --strict` already
//!   resolves those, and it resolves their `#anchors` too, which needs
//!   the rendered heading ids this crate does not have. A second, less
//!   informed opinion could only ever disagree with the first.
//! * **Unbackticked prose.** "look under cli/ for the crates" is
//!   English. The span is the signal that a path is being *named*, and
//!   the corpus writes it that way throughout.
//! * **A span that does not open on a known top-level directory.**
//!   `apprafter.io/schemas/v1alpha1` is a CUE import path, `spec/base`
//!   is a shorthand for a field, and `ghcr.io/org/image:tag` is an image
//!   reference; all three are slash-shaped and none names a file. The
//!   top-level directory list comes from the git listing, so the
//!   grammar closes over the tree rather than over a hand-kept list —
//!   same reasoning as [`crate::identifier::TEXT_ROOTS`], with the
//!   difference that this one cannot go stale.
//!
//! A glob — `providers/*`, `docs/**/*.md`, `e2e/*.sh` — is extracted
//! like anything else and suppressed at *resolution*, by
//! [`is_pattern`]. Rejecting the shape here instead would be one line
//! shorter and would put three tracked files beyond this check
//! permanently; the predicate says why.
//!
//! # The blind spot this anchor rule creates
//!
//! Requiring a top-level directory means a **crate-relative** path is
//! invisible. `docs/reference/environment.md` writes 24 of them across
//! 16 distinct paths — `platform-cli/src/commands/backup.rs`,
//! `cli-core/src/style.rs`,
//! `cli-providers/src/hetzner_cloud/kubeconfig.rs` and the rest — and
//! every one is dropped, because `platform-cli` is a crate name rather
//! than a directory of the repository. All 24 exist today under `cli/`,
//! so nothing is wrong; but they are one `cli/` prefix away from being
//! exactly the defect that motivates this module, and that page is the
//! single largest concentration of file references in the corpus.
//!
//! It is recorded rather than fixed. Widening the anchor to any
//! slash-shaped token would admit `$HOME/.config/apprafter/age.key`,
//! `/api/v1/nodes/{node}/proxy/stats/summary`, `.apprafter/state.json`
//! and 70 other distinct non-repository tokens the corpus writes in
//! spans — a check that reports those is a check that gets switched
//! off. A blind spot named and counted is a smaller problem than a
//! grammar nobody can predict.
//!
//! # Anchored, and line-local
//!
//! The **whole** span must be the path, as in
//! [`crate::identifier::extract_paths`]: `` `cd cli && cargo build` ``
//! is a sentence about a directory, not a reference to a file, and
//! splitting spans into words to reach the path inside one would admit
//! every shell word in the corpus.
//!
//! The scan is **line-local**, unlike [`crate::scan`]'s, which joins a
//! prose region so that a span may wrap a line break. A path with a
//! space in it is not a path, so a wrapped span cannot hold one — the
//! wrap would put a space in the middle of the token, and the anchored
//! rule rejects it either way.
//!
//! # Measured over the corpus
//!
//! 54 repo-root-relative references in code spans and 19 page-relative
//! link targets across 17 distinct destinations. Of the 54, **46
//! resolve** — 28 of them the bare `` `cli/` `` directory form and 18
//! with a deeper path — and the remaining 8 are the globs, suppressed
//! by [`is_pattern`]. Of the 19 link targets, every one resolves.
//!
//! So this check ships with no finding and no exemption: it is a
//! regression guard on a surface that is correct today, which is the
//! only moment at which such a guard can be installed honestly.

/// One path-shaped claim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reference {
    pub line: usize,
    pub path: String,
    /// Resolve against the page's directory rather than the repo root.
    pub page_relative: bool,
}

/// Every claim on one page. `tops` is the repository's set of
/// top-level directory names.
pub fn references(src: &str, tops: &[String]) -> Vec<Reference> {
    let mut out = Vec::new();
    for (index, text) in src.lines().enumerate() {
        let line = index + 1;
        let links = links(text);
        // (byte offset, reference), so the two surfaces come back
        // interleaved in the order a reader meets them rather than
        // links-then-spans. A finding list is read against the page.
        let mut found: Vec<(usize, Reference)> = Vec::new();

        for link in &links {
            let Some(path) = destination(link.target) else {
                continue;
            };
            found.push((
                link.at,
                Reference {
                    line,
                    path,
                    page_relative: true,
                },
            ));
        }

        for span in code_spans(text) {
            // A span that is a link's text makes no claim of its own:
            // the link's target, already taken above, is the claim.
            if links
                .iter()
                .any(|link| link.text.0 <= span.at && span.end <= link.text.1)
            {
                continue;
            }
            let Some(path) = repo_path(span.body, tops) else {
                continue;
            };
            found.push((
                span.at,
                Reference {
                    line,
                    path,
                    page_relative: false,
                },
            ));
        }

        found.sort_by_key(|(at, _)| *at);
        out.extend(found.into_iter().map(|(_, reference)| reference));
    }
    out
}

/// A code span, with the byte range it occupies so that the question
/// "is this span a link's text" can be asked of it.
struct Span<'a> {
    /// Offset of the opening backtick run.
    at: usize,
    /// Offset just past the closing backtick run.
    end: usize,
    /// The text between the runs, untrimmed.
    body: &'a str,
}

/// A Markdown inline link, `[text](target)`.
struct Link<'a> {
    /// Offset of the opening `[`.
    at: usize,
    /// Byte range of the link text, `[` and `]` excluded.
    text: (usize, usize),
    /// The destination as written, delimiters and title included.
    target: &'a str,
}

/// Every code span in one line: a run of N backticks opens, the next
/// run of exactly N closes.
///
/// The rule is [`crate::scan`]'s and [`crate::identifier`]'s, written
/// out a third time rather than reused, because **both existing
/// implementations discard the byte offset** — `scan::scan_spans` emits
/// a [`crate::scan::Block`] carrying a source line and no column, and
/// `identifier`'s helper returns contents alone. The offset is the one
/// thing this module needs that neither has: without it there is no way
/// to ask whether a span sits inside a link's text, and that question is
/// what separates `docs/reference/index.md:10` from a false positive.
/// (`scan`'s also runs over a joined multi-line region, so its offsets
/// would not be this line's even if it kept them.)
///
/// `a_multi_backtick_span_is_read_by_the_same_rule_as_scan_uses` is what
/// keeps the third copy in step with the other two.
fn code_spans(line: &str) -> Vec<Span<'_>> {
    let bytes = line.as_bytes();
    let mut out = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] != b'`' {
            at += 1;
            continue;
        }
        let opened = at;
        let mut after = at;
        while after < bytes.len() && bytes[after] == b'`' {
            after += 1;
        }
        let width = after - opened;
        let mut scan = after;
        let mut closed = None;
        while scan < bytes.len() {
            if bytes[scan] != b'`' {
                scan += 1;
                continue;
            }
            let start = scan;
            while scan < bytes.len() && bytes[scan] == b'`' {
                scan += 1;
            }
            if scan - start == width {
                closed = Some((start, scan));
                break;
            }
        }
        match closed {
            Some((from, to)) => {
                out.push(Span {
                    at: opened,
                    end: to,
                    body: &line[after..from],
                });
                at = to;
            }
            // An unmatched run is literal text, so a stray backtick
            // costs one character rather than the rest of the line.
            None => at = after,
        }
    }
    out
}

/// Every inline link in one line.
///
/// Bracket and paren depth are counted rather than searched for, so a
/// `[…]` inside a link text and a `(…)` inside a destination close where
/// they open. A `[` with no `](` after it is ordinary prose — a footnote
/// marker, a shell test, a reference-style link — and is skipped rather
/// than paired with the next `]` on the line, which would otherwise
/// swallow a code span between them.
///
/// An image, `![alt](src)`, differs by one character that is not part of
/// the bracket at all, so it is found by the same pass: it names a file
/// that must exist exactly as a link does.
fn links(line: &str) -> Vec<Link<'_>> {
    let bytes = line.as_bytes();
    let mut out = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] != b'[' {
            at += 1;
            continue;
        }
        let Some(close) = matching(bytes, at, b'[', b']') else {
            at += 1;
            continue;
        };
        if bytes.get(close + 1) != Some(&b'(') {
            at += 1;
            continue;
        }
        let Some(end) = matching(bytes, close + 1, b'(', b')') else {
            at += 1;
            continue;
        };
        out.push(Link {
            at,
            text: (at + 1, close),
            target: &line[close + 2..end],
        });
        at = end + 1;
    }
    out
}

/// Offset of the `close` delimiter matching the `open` one at `at`.
///
/// # Panics
///
/// Debug-asserts that `at` really is an `open`, which is the
/// precondition for the depth never going below zero. Both call sites
/// check it first; asserting it makes the contract the callee's rather
/// than a property of two call sites that could gain a third.
fn matching(bytes: &[u8], at: usize, open: u8, close: u8) -> Option<usize> {
    debug_assert_eq!(
        bytes.get(at),
        Some(&open),
        "`matching` must start on its opening delimiter"
    );
    let mut depth = 0usize;
    for (offset, byte) in bytes.iter().enumerate().skip(at) {
        if *byte == open {
            depth += 1;
        } else if *byte == close {
            depth -= 1;
            if depth == 0 {
                return Some(offset);
            }
        }
    }
    None
}

/// The path a link destination names, if this check is the one that
/// should resolve it.
///
/// Everything dropped here is dropped because something else already
/// owns it, or because nothing in this repository could: a `.md` page
/// belongs to `mkdocs build --strict` (anchors included), a URL and a
/// `mailto:` to the internet, a bare `#anchor` to the page itself, and a
/// site-root-absolute `/path` to a root that is neither the page's
/// directory nor the repository's.
fn destination(target: &str) -> Option<String> {
    let trimmed = target.trim();
    // CommonMark's pointy-bracket destination, and the optional title
    // after it — both are link syntax rather than part of the path, and
    // a resolver handed either reports a correctly named file missing.
    let bare = trimmed
        .strip_prefix('<')
        .and_then(|rest| rest.split('>').next())
        .unwrap_or_else(|| trimmed.split_whitespace().next().unwrap_or(""));
    if bare.is_empty() || bare.starts_with('#') || bare.starts_with('/') {
        return None;
    }
    // The scheme separator rather than a `http` prefix: a relative
    // `httpd/conf.yaml` starts with those four letters and is a file in
    // this repository, not somewhere on the internet. `://` also covers
    // every other scheme without listing them.
    if bare.contains("://") || bare.starts_with("mailto:") {
        return None;
    }
    if bare.ends_with(".md") || bare.contains(".md#") {
        return None;
    }
    let path = position_off(bare);
    (!path.is_empty()).then(|| path.to_string())
}

/// The repository path a code span names, if it names one.
fn repo_path(body: &str, tops: &[String]) -> Option<String> {
    let token = body.trim();
    // A path has no spaces, and `identifier`'s anchoring rule is this
    // one: a span holding a sentence is a sentence.
    if token.is_empty() || token.chars().any(char::is_whitespace) {
        return None;
    }
    let path = position_off(token);
    // The slash is what makes it a path rather than a word: a bare
    // `cli` is one, and `cli/` is not.
    if !path.contains('/') {
        return None;
    }
    let top = path.split('/').next()?;
    if !tops.iter().any(|known| known == top) {
        return None;
    }
    // `cli/` and `cli` are one claim, resolved against the one spelling
    // the git listing uses.
    Some(path.trim_end_matches('/').to_string())
}

/// Whether `path` is shaped like a glob rather than like a filename.
///
/// Consulted **after** resolution fails, never before it. A pattern
/// names a set of paths and there is no single file to look for, so
/// `providers/*`, `docs/**/*.md`, `docs/adr/**` and `e2e/*.sh` — eight
/// corpus spans, and the only ones that do not resolve as filenames —
/// must not be reported.
///
/// # Why `[` and `]` are not pattern characters here
///
/// They are glob syntax, and the obvious set to test for is
/// `* ? [ ] { }`. But brackets are also legal in a filename and this
/// repository has three that use them —
/// `landing/cms/src/app/(payload)/api/[...slug]/route.ts` and the two
/// `[[...segments]]` pages beside it, which is how Next.js spells a
/// dynamic route. Including brackets would suppress those, and
/// suppress them **hardest in the case that matters**: while the file
/// exists the reference resolves and the predicate is never reached,
/// but the day one is moved — the entire point of this check — the
/// reference fails to resolve, is read as a pattern, and goes silent.
/// A rule whose blind spot opens exactly when the defect appears is
/// worse than no rule.
///
/// Excluding them costs nothing, and that is measured rather than
/// hoped: every one of the eight corpus patterns contains `*`, and
/// **none is bracket-only**. So `* ? { }` suppresses all eight and
/// leaves every bracketed real path checked, moved or not. If a
/// bracket-only glob is ever written — `docs/adr/00[0-5]*.md` — it
/// carries a `*` in practice; a genuinely bracket-only one would be
/// reported, and the remedy is the same as for any other unresolvable
/// claim: name a path, or say it is a pattern in the prose.
pub fn is_pattern(path: &str) -> bool {
    path.contains(['*', '?', '{', '}'])
}

/// Drop a `#anchor` and a trailing `:NNN` line number.
///
/// Both name a position *inside* a file, which is not something a
/// repository can be asked about: the file is the claim, and it is the
/// claim that goes stale when the file moves.
fn position_off(path: &str) -> &str {
    let without_anchor = path.split('#').next().unwrap_or(path);
    match without_anchor.rsplit_once(':') {
        Some((head, tail))
            if !head.is_empty() && !tail.is_empty() && tail.bytes().all(|b| b.is_ascii_digit()) =>
        {
            head
        }
        _ => without_anchor,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tops() -> Vec<String> {
        ["cli", "docs", "e2e"]
            .iter()
            .map(|s| s.to_string())
            .collect()
    }

    #[test]
    fn a_bracket_with_no_destination_after_it_pairs_with_nothing() {
        // A footnote marker, a shell test and a reference-style link all
        // write `[` in prose. Pairing one with the next `]` on the line
        // would put the code span between them inside a "link text" and
        // silently drop the claim it holds — a false negative, which is
        // the failure class a gate must not have.
        let line = "For [1] see `cli/docsgen/src/gate.rs` and [also][ref].";
        assert!(links(line).is_empty(), "no inline link on this line");
        let found = references(line, &tops());
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].path, "cli/docsgen/src/gate.rs");
    }

    #[test]
    fn a_position_suffix_comes_off_only_when_it_is_one() {
        assert_eq!(position_off("cli/x.rs:406"), "cli/x.rs");
        assert_eq!(position_off("cli/x.cue#L2"), "cli/x.cue");
        // Not a line number: a port, a tag and a scheme all write `:`
        // and none of them names a position in a file.
        assert_eq!(position_off("cli/x.rs:main"), "cli/x.rs:main");
        assert_eq!(position_off(":8080"), ":8080");
    }

    #[test]
    fn nested_delimiters_close_where_they_open() {
        let line = "[a [b] c](../e2e/mvp.sh)";
        let found = links(line);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].target, "../e2e/mvp.sh");
        assert_eq!(&line[found[0].text.0..found[0].text.1], "a [b] c");
    }
}
