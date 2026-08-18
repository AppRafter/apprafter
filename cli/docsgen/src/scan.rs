// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Enumerate the documentation the gate may judge, and split each page
//! into the two places a claim about the product hides: a fenced block
//! and an inline code span.
//!
//! Everything downstream resolves what this module hands it, so a blind
//! spot here is a blind spot in the gate — a command the scanner never
//! sees is a command that can rot forever. That is why the grammar is
//! pinned by test rather than assumed, and why the parse deliberately
//! covers three shapes a naive line-matcher misses, all of which occur
//! in the corpus: fences inside a blockquote (`> ```cue`), fences
//! indented under a list item, and inline spans that wrap across a line
//! break.
//!
//! # Scope
//!
//! In: every tracked `docs/**/*.md` plus the root `README.md`.
//!
//! Out, each for its own reason:
//!
//! * `docs/reference/cli/**` is generated, and `docsgen check` already
//!   byte-compares it against the clap tree. Two gates with
//!   contradictory remedies on one page is worse than one gate.
//! * `docs/adr/**` and `docs/changelog/**` are historical records. An
//!   ADR describes the world as it was when it was ratified; "fixing" a
//!   command name in it would falsify the record.
//! * `docs/measurements/**` is internal working data, not documentation.
//! * `spec.md` is excluded by decision. It is the roadmap and names
//!   commands that deliberately do not exist yet; documentation
//!   describes what ships today. Gating it would turn every planned
//!   capability into a build failure.
//! * `CLAUDE.md` is untracked, so `git ls-files` cannot see it and no
//!   gate can be enforced on it. It is ungatable, not exempt.

use std::error::Error;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Directory prefixes dropped from the corpus. See the module docs for
/// why each one is out; the list is deliberately short, because every
/// entry is a place documentation can drift unobserved.
const EXCLUDED: [&str; 4] = [
    "docs/adr/",
    "docs/changelog/",
    "docs/measurements/",
    "docs/reference/cli/",
];

/// What kind of code the block held.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlockKind {
    /// A fenced block. `tag` is the info string's first word, lowercased
    /// — `None` when the fence carries no info string.
    ///
    /// The tag is recorded but must never be used to *select* what to
    /// check: hand-written shell in this corpus is tagged `sh` (162),
    /// `bash` (11), `console` (1) and nothing at all (14), so a
    /// tag-keyed gate would see a fraction of the surface. Obligations
    /// come from block content.
    Fence { tag: Option<String> },
    /// A backticked run in prose. Measurement puts most command
    /// invocations here rather than in fences, so this is the primary
    /// surface, not an afterthought.
    InlineSpan,
}

/// One checkable chunk of a page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Block {
    pub kind: BlockKind,
    /// The HTML comment on the line immediately above a fence, verbatim,
    /// if there is one. The marker grammar parses it; this module only
    /// carries it, so the two concerns can be tested apart.
    pub tag_line: Option<String>,
    /// The block's content: fence body without the delimiter lines, or
    /// the span's text. Blockquote markers and the fence's own
    /// indentation are removed, so the body is directly parseable.
    pub body: String,
    /// 1-based source line of the opening delimiter. A finding without
    /// a real line number costs the reader a manual search, which is
    /// how gates come to be ignored.
    pub line: usize,
}

/// Every in-scope documentation file, as absolute paths, sorted.
///
/// `git ls-files` rather than a directory walk, mirroring
/// `scripts/check-no-cyrillic.sh`: only tracked files can be gated,
/// and a build directory or a scratch note left in the tree must not
/// change what the gate reports. `-z` because git C-quotes unusual
/// names otherwise, which would silently drop them from the corpus.
pub fn in_scope(repo_root: &Path) -> Result<Vec<PathBuf>, Box<dyn Error>> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["ls-files", "-z", "--", "docs", "README.md"])
        .output()?;
    if !out.status.success() {
        return Err(format!(
            "git ls-files failed in {}: {}",
            repo_root.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        )
        .into());
    }
    let listing = String::from_utf8(out.stdout)?;
    let mut files: Vec<PathBuf> = listing
        .split('\0')
        .filter(|p| is_in_scope(p))
        .map(|p| repo_root.join(p))
        .collect();
    files.sort();
    Ok(files)
}

/// The scope rule, on a repo-relative path, kept separate from the
/// `git` call so it can be tested without a repository.
fn is_in_scope(path: &str) -> bool {
    if !path.ends_with(".md") {
        return false;
    }
    if path == "README.md" {
        return true;
    }
    path.starts_with("docs/") && !EXCLUDED.iter().any(|dir| path.starts_with(dir))
}

/// A fence being parsed.
struct Open {
    /// `` ` `` or `~`. A tilde run cannot close a backtick fence.
    delimiter: char,
    /// Length of the opening run; the closer must be at least this long.
    run: usize,
    /// Indentation of the opening delimiter, stripped from body lines.
    indent: usize,
    tag: Option<String>,
    tag_line: Option<String>,
    line: usize,
    body: String,
}

/// Split a Markdown source into its fences and inline code spans, in
/// source order.
///
/// A line state machine rather than a Markdown library: the crate has
/// no parser dependency, the grammar needed is small, and — decisively
/// — a real parser would hand back an AST of *rendered* content, while
/// a gate needs the source line of every claim to report it.
pub fn scan_markdown(src: &str) -> Vec<Block> {
    let mut out = Vec::new();
    // Prose lines since the last blank line or fence. Spans are scanned
    // over the region rather than line by line, because a span may wrap
    // across a line break — 11 command invocations in the corpus do,
    // and a per-line scan silently loses every one of them.
    let mut region: Vec<(usize, String)> = Vec::new();
    let mut open: Option<Open> = None;
    // The HTML comment on the previous line, if that line was one.
    let mut comment: Option<String> = None;

    for (index, raw) in src.lines().enumerate() {
        let line = index + 1;
        let text = strip_quote_prefix(raw);
        match open.as_mut() {
            Some(fence) => {
                if fence_closes(text, fence.delimiter, fence.run) {
                    out.push(finish(open.take().expect("checked above")));
                } else {
                    fence.body.push_str(strip_indent(text, fence.indent));
                    fence.body.push('\n');
                }
            }
            None => match fence_open(text) {
                Some((delimiter, run, indent, tag)) => {
                    flush(&mut region, &mut out);
                    open = Some(Open {
                        delimiter,
                        run,
                        indent,
                        tag,
                        tag_line: comment.take(),
                        line,
                        body: String::new(),
                    });
                }
                None => {
                    comment = html_comment(text);
                    if text.trim().is_empty() {
                        // A code span cannot contain a blank line, so a
                        // blank line ends the region and with it any
                        // unclosed backtick run.
                        flush(&mut region, &mut out);
                    } else {
                        region.push((line, text.to_string()));
                    }
                }
            },
        }
    }
    // An unclosed fence runs to the end of the document (CommonMark).
    // Emitting it is what makes a truncated page checkable instead of
    // invisible.
    if let Some(fence) = open.take() {
        out.push(finish(fence));
    }
    flush(&mut region, &mut out);
    out
}

/// Turn a completed fence into a [`Block`].
fn finish(fence: Open) -> Block {
    Block {
        kind: BlockKind::Fence { tag: fence.tag },
        tag_line: fence.tag_line,
        body: fence.body,
        line: fence.line,
    }
}

/// Drop any blockquote markers from the front of a line.
///
/// Seven fences in the corpus live inside a `>` callout, and prose
/// spans wrap inside them too. Without this, the opening `> ```cue`
/// reads as prose holding an unterminated backtick run and the whole
/// callout body is mis-scanned.
fn strip_quote_prefix(line: &str) -> &str {
    let mut rest = line;
    loop {
        let trimmed = rest.trim_start_matches([' ', '\t']);
        match trimmed.strip_prefix('>') {
            // One optional space after each marker belongs to the
            // marker, not to the content.
            Some(after) => rest = after.strip_prefix(' ').unwrap_or(after),
            None => return rest,
        }
    }
}

/// Whether `line` opens a fence, and with what: delimiter, run length,
/// indentation and tag.
///
/// Indentation is recorded but not limited to CommonMark's three
/// spaces, because fences indented under a list item are real in the
/// corpus (three of them) and dropping those would make the gate blind
/// to every numbered walkthrough step. The cost is that a four-space
/// indented *literal* code block containing a fence line would be read
/// as a fence; the corpus has none, and the parse is self-checking —
/// every fence in scope closes.
fn fence_open(line: &str) -> Option<(char, usize, usize, Option<String>)> {
    let body = line.trim_start_matches([' ', '\t']);
    let indent = line.len() - body.len();
    let delimiter = body.chars().next()?;
    if delimiter != '`' && delimiter != '~' {
        return None;
    }
    let run = body.chars().take_while(|c| *c == delimiter).count();
    if run < 3 {
        return None;
    }
    let info = &body[run..];
    // CommonMark: a backtick fence's info string may not contain a
    // backtick. That rule is what stops a prose line like
    // ```` ```code``` is shorthand ```` from opening a fence.
    if delimiter == '`' && info.contains('`') {
        return None;
    }
    let tag = info.split_whitespace().next().map(str::to_lowercase);
    Some((delimiter, run, indent, tag))
}

/// Whether `line` closes a fence opened with `run` × `delimiter`.
///
/// The closer must use the same character, be at least as long, and
/// carry nothing else — so the shorter ``` inside a ```` fence stays
/// content, which is exactly how the docs show a fence inside a fence.
fn fence_closes(line: &str, delimiter: char, run: usize) -> bool {
    let body = line.trim_start_matches([' ', '\t']);
    let closing = body.chars().take_while(|c| *c == delimiter).count();
    closing >= run && body[closing..].trim().is_empty()
}

/// Remove up to `indent` leading spaces, the fence's own indentation.
/// Content indented further keeps the difference, so a nested snippet
/// inside a list item stays shaped the way it was written.
fn strip_indent(line: &str, indent: usize) -> &str {
    let mut rest = line;
    for _ in 0..indent {
        match rest.strip_prefix([' ', '\t']) {
            Some(shorter) => rest = shorter,
            None => break,
        }
    }
    rest
}

/// The line verbatim when it is a whole HTML comment, else `None`.
fn html_comment(line: &str) -> Option<String> {
    let trimmed = line.trim();
    let whole = trimmed.starts_with("<!--") && trimmed.ends_with("-->") && trimmed.len() >= 7;
    whole.then(|| trimmed.to_string())
}

/// Scan the accumulated prose region for inline spans and clear it.
fn flush(region: &mut Vec<(usize, String)>, out: &mut Vec<Block>) {
    if region.is_empty() {
        return;
    }
    let mut text = String::new();
    // (byte offset of the line's start, its 1-based source line).
    let mut starts: Vec<(usize, usize)> = Vec::with_capacity(region.len());
    for (line, content) in region.iter() {
        starts.push((text.len(), *line));
        text.push_str(content);
        text.push('\n');
    }
    scan_spans(&text, &starts, out);
    region.clear();
}

/// Emit every code span in one prose region.
///
/// A run of N backticks opens a span that the next run of *exactly* N
/// closes; a run of any other length inside is content, which is how
/// ``` ``a `b` c`` ``` stays one span. An unmatched run is literal
/// text, so a stray backtick in prose costs one character, not the
/// rest of the paragraph.
fn scan_spans(text: &str, starts: &[(usize, usize)], out: &mut Vec<Block>) {
    let bytes = text.as_bytes();
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
        match closing_run(bytes, after, width) {
            Some((from, to)) => {
                out.push(Block {
                    kind: BlockKind::InlineSpan,
                    tag_line: None,
                    body: normalise(&text[after..from]),
                    line: line_of(starts, opened),
                });
                at = to;
            }
            None => at = after,
        }
    }
}

/// The next run of exactly `width` backticks at or after `from`.
fn closing_run(bytes: &[u8], from: usize, width: usize) -> Option<(usize, usize)> {
    let mut at = from;
    while at < bytes.len() {
        if bytes[at] != b'`' {
            at += 1;
            continue;
        }
        let start = at;
        while at < bytes.len() && bytes[at] == b'`' {
            at += 1;
        }
        if at - start == width {
            return Some((start, at));
        }
    }
    None
}

/// CommonMark's code-span normalisation: line endings become spaces,
/// and one space is stripped from each end when both are present. It
/// matters because `` ` apprafter app list ` `` and `` `apprafter app
/// list` `` are the same claim and must resolve identically.
fn normalise(raw: &str) -> String {
    let joined = raw.replace('\n', " ");
    let strippable = joined.len() >= 2
        && joined.starts_with(' ')
        && joined.ends_with(' ')
        && !joined.trim().is_empty();
    if strippable {
        joined[1..joined.len() - 1].to_string()
    } else {
        joined
    }
}

/// The source line containing byte offset `pos` in a joined region.
fn line_of(starts: &[(usize, usize)], pos: usize) -> usize {
    let after = starts.partition_point(|(offset, _)| *offset <= pos);
    starts[after.saturating_sub(1)].1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scope_keeps_hand_written_docs_and_the_readme() {
        assert!(is_in_scope("README.md"));
        assert!(is_in_scope("docs/index.md"));
        assert!(is_in_scope("docs/reference/environment.md"));
        assert!(is_in_scope("docs/operator-guide/quickstart.md"));
    }

    #[test]
    fn scope_drops_the_generated_reference_and_the_records() {
        assert!(!is_in_scope("docs/reference/cli/app.md"));
        assert!(!is_in_scope("docs/adr/0057-documentation-system.md"));
        assert!(!is_in_scope("docs/changelog/UNRELEASED.md"));
        assert!(!is_in_scope(
            "docs/measurements/2.16d-baseline-2026-08-08.md"
        ));
    }

    #[test]
    fn scope_drops_the_roadmap_and_everything_outside_docs() {
        // The roadmap names commands that do not exist yet on purpose.
        assert!(!is_in_scope("spec.md"));
        assert!(!is_in_scope("plan.md"));
        assert!(!is_in_scope("cli/README.md"));
        assert!(!is_in_scope("docs/apprafter-logo.svg"));
        assert!(!is_in_scope(""));
    }

    #[test]
    fn quote_markers_are_stripped_however_deep() {
        assert_eq!(strip_quote_prefix("> ```cue"), "```cue");
        assert_eq!(strip_quote_prefix(">   indented"), "  indented");
        assert_eq!(strip_quote_prefix("  > > nested"), "nested");
        assert_eq!(strip_quote_prefix("plain"), "plain");
        assert_eq!(strip_quote_prefix(">"), "");
    }

    #[test]
    fn a_short_run_is_not_a_fence() {
        assert!(fence_open("``two").is_none());
        assert!(fence_open("text").is_none());
        assert!(fence_open("").is_none());
    }

    #[test]
    fn a_prose_line_holding_a_span_is_not_a_fence() {
        // The info string of a backtick fence may not hold a backtick.
        assert!(fence_open("```code``` is shorthand").is_none());
        assert!(fence_open("```sh").is_some());
    }

    #[test]
    fn the_tag_is_the_first_word_lowercased() {
        let (delimiter, run, indent, tag) = fence_open("    ```SH title=\"x\"").unwrap();
        assert_eq!((delimiter, run, indent), ('`', 3, 4));
        assert_eq!(tag.as_deref(), Some("sh"));
        assert_eq!(fence_open("~~~~").unwrap().3, None);
    }

    #[test]
    fn a_closer_must_be_bare_and_long_enough() {
        assert!(fence_closes("```", '`', 3));
        assert!(fence_closes("  ````  ", '`', 3));
        assert!(!fence_closes("``", '`', 3));
        assert!(!fence_closes("```sh", '`', 3));
        // A tilde run cannot close a backtick fence.
        assert!(!fence_closes("~~~", '`', 3));
    }

    #[test]
    fn only_the_fences_own_indentation_is_stripped() {
        assert_eq!(strip_indent("     deeper", 4), " deeper");
        assert_eq!(strip_indent("  shallower", 4), "shallower");
    }

    #[test]
    fn a_whole_comment_is_recognised_and_a_partial_one_is_not() {
        assert_eq!(
            html_comment("  <!-- docs: check=cli -->"),
            Some("<!-- docs: check=cli -->".to_string())
        );
        assert_eq!(html_comment("<!-- open"), None);
        assert_eq!(html_comment("text <!-- trailing"), None);
        assert_eq!(html_comment("plain"), None);
    }

    #[test]
    fn one_padding_space_each_side_is_dropped() {
        assert_eq!(normalise(" apprafter status "), "apprafter status");
        assert_eq!(normalise(" apprafter status"), " apprafter status");
        assert_eq!(normalise("a\nb"), "a b");
        assert_eq!(normalise("  "), "  ");
    }

    #[test]
    fn a_position_maps_back_to_its_source_line() {
        let starts = [(0usize, 7usize), (6, 8), (12, 9)];
        assert_eq!(line_of(&starts, 0), 7);
        assert_eq!(line_of(&starts, 5), 7);
        assert_eq!(line_of(&starts, 6), 8);
        assert_eq!(line_of(&starts, 99), 9);
    }
}
