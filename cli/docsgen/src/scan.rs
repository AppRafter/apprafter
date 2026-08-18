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
//!
//! # Front matter
//!
//! A leading `---` … `---` block is emitted as [`BlockKind::FrontMatter`]
//! and never as prose, so its values cannot become claims. That is not
//! tidiness: the page-level span exemption described on
//! [`Block::tag_line`] lives in front matter and quotes the literal text
//! of the span it exempts, so a front matter scanned as prose would be
//! read as the very span it is exempting — and the page would exempt
//! nothing. The same rule keeps a `description:` holding a backtick (as
//! the generated pages' do) from being checked as a command.
//!
//! Three in-scope files carry front matter. `docs/reference/environment.md`
//! predates the gate and holds no backticks; the other two
//! (`docs/dev-guide/application-cue.md`,
//! `docs/operator-guide/node-prep.md`) carry exemption lists whose
//! values quote the literal text they exempt — which is precisely the
//! shape that would exempt nothing if front matter were scanned as
//! prose.

use crate::render::DIR;
use std::error::Error;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Directory prefixes dropped from the corpus. See the module docs for
/// why each one is out; the list is deliberately short, because every
/// entry is a place documentation can drift unobserved.
///
/// The generated CLI reference is *not* listed here — it is derived
/// from [`DIR`] in [`is_in_scope`], so renaming the output directory
/// moves the exclusion with it instead of silently re-admitting
/// generated pages into the gate's corpus.
const EXCLUDED: [&str; 3] = ["docs/adr/", "docs/changelog/", "docs/measurements/"];

/// What kind of code the block held.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlockKind {
    /// A fenced block. `tag` is the info string's first word, lowercased
    /// — `None` when the fence carries no info string.
    ///
    /// The tag is recorded but must never be used to *select* what to
    /// check: hand-written shell in this corpus carries four different
    /// tags — as found at `dc4c5de`, `sh` (162), `bash` (11),
    /// `console` (1) and nothing at all (14) — so a tag-keyed gate
    /// would see a fraction of the surface. Obligations come from
    /// block content. (The untagged group is 0 today, because the gate
    /// reports an unlabelled fence; that it moved at all is the
    /// reason not to key on it. `corpus_census` prints the live
    /// distribution.)
    Fence { tag: Option<String> },
    /// A backticked run in prose. Measurement puts most command
    /// invocations here rather than in fences, so this is the primary
    /// surface, not an afterthought.
    InlineSpan,
    /// The YAML front matter opening the page, delimiters excluded.
    ///
    /// A block of its own rather than skipped material, because it is
    /// where a page declares things *about* the gate — the span
    /// exemption list — and a caller that cannot see it cannot honour
    /// them. It is never prose and never holds spans: see the module
    /// docs for why that is load-bearing rather than cosmetic.
    FrontMatter,
}

/// One checkable chunk of a page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Block {
    pub kind: BlockKind,
    /// The HTML comment on the line immediately above a fence,
    /// verbatim, if there is one. The marker grammar parses it; this
    /// module only carries it, so the two concerns can be tested apart.
    ///
    /// **A marker annotates the fence that follows it and nothing else:
    /// this is always `None` for a [`BlockKind::InlineSpan`].** Spans
    /// are the majority surface, so that is a decision rather than an
    /// oversight. A comment mid-sentence is unreadable as prose and
    /// unreviewable as an exemption; a span that must be exempted is
    /// exempted at page level, through front matter carrying the span's
    /// literal text, which keeps every exemption greppable and
    /// countable in one place instead of scattered through paragraphs.
    pub tag_line: Option<String>,
    /// The block's content: fence body without the delimiter lines, or
    /// the span's text. The fence's own indentation and its own
    /// blockquote depth are removed — and nothing deeper — so the body
    /// is both directly parseable and byte-faithful to what the author
    /// wrote inside the fence.
    pub body: String,
    /// 1-based source line of the opening delimiter. A finding without
    /// a real line number costs the reader a manual search, which is
    /// how gates come to be ignored.
    pub line: usize,
    /// A fence that ran to the end of the file without a closing
    /// delimiter. Always false for a span.
    ///
    /// This is the parse's self-check, and it has to be reported rather
    /// than assumed: a phantom fence closes at EOF like any other, so
    /// "every fence closed" proves nothing unless the ones that closed
    /// *only* because the file ended are counted. `corpus_census`
    /// asserts that count is zero.
    pub unterminated: bool,
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
    if !path.starts_with("docs/") {
        return false;
    }
    // Spelled once, at its source: `docsgen check` already owns
    // everything under `render::DIR`.
    if path.starts_with(DIR) && path[DIR.len()..].starts_with('/') {
        return false;
    }
    !EXCLUDED.iter().any(|dir| path.starts_with(dir))
}

/// A fence being parsed.
struct Open {
    /// `` ` `` or `~`. A tilde run cannot close a backtick fence.
    delimiter: char,
    /// Length of the opening run; the closer must be at least this long.
    run: usize,
    /// Indentation of the opening delimiter, stripped from body lines.
    indent: usize,
    /// Blockquote depth of the *opening* delimiter. Body lines give up
    /// at most this many markers and never more, because below that
    /// depth a `>` is content, not container.
    depth: usize,
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

    // Front matter first, and taken off the line stream entirely: it is
    // neither prose nor a fence, and the `---` delimiters would
    // otherwise read as thematic breaks around a paragraph of YAML.
    let mut skip = 0;
    if let Some((consumed, body)) = front_matter(src) {
        out.push(Block {
            kind: BlockKind::FrontMatter,
            tag_line: None,
            body,
            line: 1,
            unterminated: false,
        });
        skip = consumed;
    }

    for (index, raw) in src.lines().enumerate().skip(skip) {
        let line = index + 1;
        match open.as_mut() {
            Some(fence) => {
                // Only the container the fence itself opened in comes
                // off. Stripping unconditionally rewrote fence content:
                // a shell redirect continued onto its own line
                // (`  > /tmp/out.txt`) lost the `>` and became a
                // different, still-plausible command, and a body line
                // `> ```­` in a quoted-Markdown example closed a fence
                // that was never quoted — which ends the real block
                // early and lets a phantom one swallow the rest of the
                // page.
                let text = if fence.depth == 0 {
                    raw
                } else {
                    strip_quotes(raw, fence.depth).1
                };
                if fence_closes(text, fence.delimiter, fence.run) {
                    out.push(finish(open.take().expect("checked above"), false));
                } else {
                    fence.body.push_str(strip_indent(text, fence.indent));
                    fence.body.push('\n');
                }
            }
            None => {
                let (depth, text) = strip_quotes(raw, usize::MAX);
                if let Some((delimiter, run, indent, tag)) = fence_open(text) {
                    flush(&mut region, &mut out);
                    open = Some(Open {
                        delimiter,
                        run,
                        indent,
                        depth,
                        tag,
                        tag_line: comment.take(),
                        line,
                        body: String::new(),
                    });
                    continue;
                }
                if let Some(found) = html_comment(text) {
                    // The marker channel, not prose. Scanning it as
                    // both would double-count the first marker that
                    // quotes a command.
                    comment = Some(found);
                    continue;
                }
                comment = None;
                if text.trim().is_empty() {
                    // A code span cannot contain a blank line, so a
                    // blank line ends the region and with it any
                    // unclosed backtick run.
                    flush(&mut region, &mut out);
                } else {
                    let boundary = boundary(text);
                    if !matches!(boundary, Boundary::Continues) {
                        flush(&mut region, &mut out);
                    }
                    region.push((line, text.to_string()));
                    if matches!(boundary, Boundary::Standalone) {
                        flush(&mut region, &mut out);
                    }
                }
            }
        }
    }
    // An unclosed fence runs to the end of the document (CommonMark).
    // Emitting it is what makes a truncated page checkable instead of
    // invisible.
    if let Some(fence) = open.take() {
        out.push(finish(fence, true));
    }
    flush(&mut region, &mut out);
    out
}

/// The page's YAML front matter: how many lines it occupies, including
/// both delimiters, and its body without them.
///
/// Three conditions, each one narrowing a shape that is otherwise a
/// legitimate thematic break:
///
/// * it must open on **line 1**, at column 0 — a `---` anywhere else is
///   a break or a setext underline and stays prose;
/// * it must close, on a line that is exactly `---` or `...` (both are
///   accepted by the YAML-meta extensions MkDocs uses). An unterminated
///   opener is not front matter, so a page that genuinely starts with a
///   thematic break is scanned as it was before;
/// * nothing else: the body is handed back verbatim rather than parsed,
///   because this module's job is where content lives, not what it
///   means.
fn front_matter(src: &str) -> Option<(usize, String)> {
    let mut lines = src.lines();
    if lines.next()?.trim_end() != "---" {
        return None;
    }
    let mut body = String::new();
    let mut consumed = 1;
    for line in lines {
        consumed += 1;
        let closer = line.trim_end();
        if closer == "---" || closer == "..." {
            return Some((consumed, body));
        }
        body.push_str(line);
        body.push('\n');
    }
    None
}

/// Turn a completed fence into a [`Block`].
fn finish(fence: Open, unterminated: bool) -> Block {
    Block {
        kind: BlockKind::Fence { tag: fence.tag },
        tag_line: fence.tag_line,
        body: fence.body,
        line: fence.line,
        unterminated,
    }
}

/// Drop up to `limit` blockquote markers, returning how many came off
/// and what is left of the line.
///
/// Six fences in the corpus live inside a `>` callout, and prose spans
/// wrap inside them too. Without stripping, the opening `> ```cue`
/// reads as prose holding an unterminated backtick run and the whole
/// callout is mis-scanned. `limit` is what keeps that honest once a
/// fence is open: below the fence's own depth a `>` is content — a
/// redirect, a heredoc, a quoted Markdown example — and must survive
/// verbatim.
fn strip_quotes(line: &str, limit: usize) -> (usize, &str) {
    let mut rest = line;
    let mut depth = 0;
    while depth < limit {
        let trimmed = rest.trim_start_matches([' ', '\t']);
        match trimmed.strip_prefix('>') {
            // One optional space after each marker belongs to the
            // marker, not to the content.
            Some(after) => {
                rest = after.strip_prefix(' ').unwrap_or(after);
                depth += 1;
            }
            None => break,
        }
    }
    (depth, rest)
}

/// How a prose line relates to the lines around it.
///
/// A code span cannot cross a block boundary, so the region scan must
/// not either. Without this, a stray backtick in a heading or a table
/// cell pairs with the *next* real span and consumes it: the claim
/// inside disappears from the corpus entirely. That is a silent false
/// negative — the one failure class a gate must never have, because
/// nothing about it looks wrong.
///
/// The distinction between the two boundary kinds is load-bearing.
/// Treating a heading merely as a region *start* still leaves it
/// sharing inline scope with the paragraph below, which is precisely
/// the swallowing case; a list item, by contrast, must keep sharing
/// scope with its continuation lines, because that is where every
/// wrapped invocation in the corpus lives.
enum Boundary {
    /// Ordinary prose, continuing whatever came before.
    Continues,
    /// Opens a block that following lines continue — a list item.
    Opens,
    /// A block complete in itself: a heading, a table row, a setext
    /// underline. It shares inline scope with nothing.
    Standalone,
}

/// Classify a non-blank prose line.
fn boundary(line: &str) -> Boundary {
    let text = line.trim_start_matches([' ', '\t']);
    // A table row, which must both open and close with a pipe.
    //
    // Requiring the closing pipe is not pedantry: a shell pipeline
    // wrapped onto its own line also starts with `|`, and treating one
    // as a table row splits the span in half and destroys the command
    // inside it. Measured over the corpus the two forms separate
    // perfectly — 298 of the 301 `|`-leading lines close with a pipe
    // and every one is a real row; the 3 that do not are all wrapped
    // pipelines, one of them `apprafter kubeconfig | tee /tmp/kc`.
    let trimmed = text.trim_end();
    if trimmed.starts_with('|') && trimmed.ends_with('|') {
        return Boundary::Standalone;
    }
    // An ATX heading: one to six `#` then a space or end of line.
    let hashes = text.chars().take_while(|c| *c == '#').count();
    let after = &text[hashes..];
    if (1..=6).contains(&hashes) && (after.is_empty() || after.starts_with(' ')) {
        return Boundary::Standalone;
    }
    // A setext underline, which is also the thematic-break shape.
    if !text.is_empty() && (text.bytes().all(|b| b == b'=') || text.bytes().all(|b| b == b'-')) {
        return Boundary::Standalone;
    }
    // A bullet list item.
    if let Some(rest) = text.strip_prefix(['-', '*', '+']) {
        if rest.starts_with(' ') {
            return Boundary::Opens;
        }
    }
    // An ordered list item.
    let digits = text.chars().take_while(char::is_ascii_digit).count();
    let rest = &text[digits..];
    if digits > 0 && (rest.starts_with('.') || rest.starts_with(')')) && rest[1..].starts_with(' ')
    {
        return Boundary::Opens;
    }
    Boundary::Continues
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
    for (index, (line, content)) in region.iter().enumerate() {
        // A continuation line's indentation is block structure, not
        // content — CommonMark's block parser removes it before any
        // inline parsing. Carrying it through put a list item's indent
        // *inside* the command: ten spans in the corpus, four of them
        // invocations, read `apprafter app status my-service --env␣␣␣
        // staging`, which contradicts this module's own rule that a
        // padded and an unpadded span are the same claim.
        let content = if index == 0 {
            content.as_str()
        } else {
            content.trim_start()
        };
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
                    // A marker annotates a fence only — see the field.
                    tag_line: None,
                    body: normalise(&text[after..from]),
                    line: line_of(starts, opened),
                    unterminated: false,
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
    fn the_generated_tree_exclusion_follows_render_dir() {
        // Derived, not re-spelled: renaming `render::DIR` must move the
        // exclusion with it rather than re-admit generated pages.
        assert!(!is_in_scope(&format!("{DIR}/app.md")));
        assert!(!is_in_scope(&format!("{DIR}/nested/app.md")));
        // Only a real path segment matches — a sibling directory whose
        // name merely starts with it stays in scope.
        assert!(is_in_scope(&format!("{DIR}mate.md")));
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
        assert_eq!(strip_quotes("> ```cue", usize::MAX), (1, "```cue"));
        assert_eq!(strip_quotes(">   indented", usize::MAX), (1, "  indented"));
        assert_eq!(strip_quotes("  > > nested", usize::MAX), (2, "nested"));
        assert_eq!(strip_quotes("plain", usize::MAX), (0, "plain"));
        assert_eq!(strip_quotes(">", usize::MAX), (1, ""));
    }

    #[test]
    fn the_limit_leaves_markers_below_it_alone() {
        // Inside a fence, only the fence's own container comes off.
        assert_eq!(strip_quotes("  > /tmp/out.txt", 0), (0, "  > /tmp/out.txt"));
        assert_eq!(strip_quotes("> > quoted", 1), (1, "> quoted"));
    }

    #[test]
    fn self_contained_blocks_share_scope_with_nothing() {
        for line in [
            "## Heading",
            "#",
            "###### six",
            "| cell | cell |",
            "=====",
            "---",
        ] {
            assert!(
                matches!(boundary(line), Boundary::Standalone),
                "{line:?} must not share inline scope"
            );
        }
    }

    #[test]
    fn a_list_item_opens_a_block_its_continuations_join() {
        for line in ["- bullet", "* bullet", "+ bullet", "  1. ordered", "12) x"] {
            assert!(
                matches!(boundary(line), Boundary::Opens),
                "{line:?} must keep its continuation lines"
            );
        }
    }

    #[test]
    fn ordinary_prose_continues() {
        for line in [
            "just a sentence",
            "#hashtag",
            "####### seven",
            "-dash-word",
            "1.5 is a number",
            "a | b",
            "  a continuation line",
            // A wrapped shell pipeline is not a table row.
            "  | tee /tmp/kc` and `export KUBECONFIG=/tmp/kc`).",
            "  | grep claim_demo_parser_pg",
        ] {
            assert!(matches!(boundary(line), Boundary::Continues), "{line:?}");
        }
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
    fn front_matter_is_the_leading_delimited_block_and_only_that() {
        assert_eq!(
            front_matter("---\ntitle: \"x\"\n---\n\n# Heading\n"),
            Some((3, "title: \"x\"\n".to_string()))
        );
        // `...` also closes a YAML document.
        assert_eq!(
            front_matter("---\na: 1\n...\nbody\n"),
            Some((3, "a: 1\n".to_string()))
        );
        // Empty front matter is still front matter.
        assert_eq!(front_matter("---\n---\n"), Some((2, String::new())));
    }

    #[test]
    fn a_break_that_is_not_leading_or_not_closed_is_not_front_matter() {
        // Mid-document: a thematic break, not a header.
        assert_eq!(front_matter("# Title\n\n---\n\ntext\n"), None);
        // Never opened at column 0.
        assert_eq!(front_matter("  ---\na: 1\n---\n"), None);
        // Opened and never closed — the page is prose that happens to
        // start with a break, and must scan exactly as it did before.
        assert_eq!(front_matter("---\na: 1\nb: 2\n"), None);
        assert_eq!(front_matter(""), None);
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
