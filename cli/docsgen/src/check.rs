// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! `docsgen check` — the CLI-reference drift gate.
//!
//! Assertion A (clap ↔ committed): every artefact `render_all` produces
//! must be byte-identical to the committed file under
//! `docs/reference/cli/`. Catches "renamed a flag, forgot `just
//! docsgen-generate`" and "hand-edited a GENERATED page".
//!
//! Assertion B (no strays): a file in `docs/reference/cli/` the
//! generator does not produce is an error. Without it, deleting a
//! command would leave its page behind — still in the site build, still
//! indexed by search, documenting a command the binary no longer has.
//! That is the exact failure mode this whole track exists to prevent,
//! and assertion A alone cannot see it (it only looks at files it
//! expects).
//!
//! Assertion C (examples resolve): every worked example in the rendered
//! tree resolves against the clap tree — path and flag names, through
//! the same resolver `docsgen gate` uses on hand-written prose. A and B
//! are both byte-compares, so neither can see a *wrong* example: it
//! renders identically on both sides. And `docsgen gate` excludes this
//! tree precisely because A owns it, so the resolver never reaches one.
//! See [`crate::examples`] for the full argument.
//!
//! `operator/crdgen/src/check.rs` is the precedent; the diagnostic
//! shape (first differing line, committed vs generated) is deliberately
//! the same, because a contributor who has seen one gate should
//! recognise the other.

use crate::render::{render_all, DIR};
use clap::Command;
use std::collections::BTreeSet;
use std::error::Error;
use std::path::{Path, PathBuf};

/// The remedy for a drifted or missing artefact.
const REGENERATE: &str = "regenerate with `just docsgen-generate` (the clap tree is the source)";

/// The remedy for a stray. Regenerating cannot delete a file
/// `generate` no longer writes, so a report containing strays must not
/// lead with [`REGENERATE`] — the header is composed from the classes
/// actually present.
const DELETE: &str = "delete the stray file(s) — each line gives the `git rm`";

/// The remedy for an example that does not resolve. Regenerating cannot
/// help: the page already matches the source, and the source is wrong.
const FIX_EXAMPLE: &str =
    "fix the example in `cli/platform-cli/src/examples.rs` against `apprafter --help`";

/// Byte-compare the committed CLI reference against a fresh render of
/// `root_cmd`, and resolve every example in it. Every artefact is
/// compared before reporting, so one run surfaces the whole drift
/// rather than one file per iteration.
pub fn check(root_cmd: &Command, repo_root: &Path) -> Result<(), Box<dyn Error>> {
    let rendered = render_all(root_cmd, repo_root);
    let mut problems = Vec::new();
    let mut drift = 0usize;
    let mut strays = 0usize;

    // Assertion A — every rendered artefact matches the committed file.
    for artefact in &rendered {
        let shown = relative(repo_root, &artefact.path);
        match std::fs::read_to_string(&artefact.path) {
            Ok(committed) => {
                if let Some(diff) = first_diff(&committed, &artefact.text) {
                    drift += 1;
                    problems.push(format!("[A drift] {shown}\n      {diff}"));
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                drift += 1;
                problems.push(format!("[A missing] {shown}: generated but not committed"));
            }
            Err(e) => {
                drift += 1;
                problems.push(format!("[A unreadable] {shown}: {e}"));
            }
        }
    }

    // Assertion B — nothing else lives in the generated directory.
    let expected: BTreeSet<&Path> = rendered.iter().map(|a| a.path.as_path()).collect();
    for path in committed_files(&repo_root.join(DIR)) {
        if !expected.contains(path.as_path()) {
            let shown = relative(repo_root, &path);
            strays += 1;
            problems.push(format!(
                "[B stray] {shown}: committed but no longer generated — a removed \
                 command leaves a page that still publishes; delete it (`git rm {shown}`)"
            ));
        }
    }

    // Assertion C — every example in the tree resolves against clap.
    //
    // Judged on the projection the artefacts were rendered FROM, not on
    // the committed pages: it is the same data (A has just proved so
    // for every artefact), and reading it back out of markdown would
    // put a second parser in the gate for nothing.
    let version = root_cmd.get_version().unwrap_or("unknown").to_string();
    let tree = crate::model::tree_from(root_cmd, &version);
    let examples = crate::examples::check(&tree)?;
    let bad_examples = examples.len();
    problems.extend(examples.iter().map(|f| format!("[C example] {f}")));

    if problems.is_empty() {
        eprintln!(
            "docsgen check OK: {} artefact(s) in {DIR} match the clap tree, \
             {} example(s) resolve against it",
            rendered.len(),
            tree.commands
                .iter()
                .map(|c| c.examples.len())
                .sum::<usize>()
        );
        return Ok(());
    }
    // One remedy per class actually present: a strays-only run must
    // not lead with "regenerate", which cannot delete anything, and a
    // drift-only run must not send the reader hunting for a file to
    // remove. Counted per class rather than inferred by subtraction —
    // with three classes the arithmetic that worked for two would
    // silently mislabel every mixed report.
    let mut remedies = Vec::new();
    if drift > 0 {
        remedies.push(REGENERATE);
    }
    if strays > 0 {
        remedies.push(DELETE);
    }
    if bad_examples > 0 {
        remedies.push(FIX_EXAMPLE);
    }
    Err(format!(
        "docs-check FAILED: {} CLI-reference problem(s) in {DIR} — {}:\n  {}",
        problems.len(),
        remedies.join("; "),
        problems.join("\n  ")
    )
    .into())
}

/// Every file under `dir`, recursively, sorted. A missing directory
/// yields nothing: assertion A already reports each artefact as
/// missing, and a second "the directory is gone" line would add noise
/// to the same diagnosis. Directories themselves are not listed —
/// only files can be strays.
fn committed_files(dir: &Path) -> Vec<PathBuf> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, out);
            } else {
                out.push(path);
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, &mut out);
    out.sort();
    out
}

/// A repo-relative path for the diagnostic — an absolute one differs
/// per machine and makes two reports of the same drift look different.
fn relative(repo_root: &Path, path: &Path) -> String {
    path.strip_prefix(repo_root)
        .unwrap_or(path)
        .display()
        .to_string()
}

/// The first differing line (1-based) with a short excerpt, or `None`
/// if the two strings are byte-identical. Quoting both sides is what
/// makes the report actionable without running a diff: the reader sees
/// which direction drifted.
fn first_diff(committed: &str, generated: &str) -> Option<String> {
    if committed == generated {
        return None;
    }
    let mut cl = committed.lines();
    let mut gl = generated.lines();
    let mut n = 0;
    loop {
        n += 1;
        match (cl.next(), gl.next()) {
            (Some(a), Some(b)) if a == b => continue,
            (Some(a), Some(b)) => {
                let col = common_prefix_chars(a, b);
                return Some(format!(
                    "line {n}, col {}: committed {:?} != generated {:?}",
                    col + 1,
                    window(a, col),
                    window(b, col)
                ));
            }
            (Some(a), None) => {
                return Some(format!("line {n}: committed has extra {:?}", window(a, 0)))
            }
            (None, Some(b)) => {
                return Some(format!("line {n}: generated has extra {:?}", window(b, 0)))
            }
            // `lines()` ignores a trailing-newline difference, so equal
            // line sequences with unequal bytes land here.
            (None, None) => return Some("content equal but byte length differs".into()),
        }
    }
}

/// How many leading CHARS the two lines share. Char-counted, not
/// byte-counted: the rendered tables are full of em dashes, and a byte
/// offset would both mis-report the column and risk slicing one in
/// half.
fn common_prefix_chars(a: &str, b: &str) -> usize {
    a.chars().zip(b.chars()).take_while(|(x, y)| x == y).count()
}

/// A `…`-elided window of `line` around char index `col`.
///
/// A generated table row runs to ~600 characters, so quoting two of
/// them in full produced a ~1200-character diagnostic in which the one
/// changed token was invisible — the reader had to diff the diagnostic.
/// The asymmetric window keeps enough left context to locate the row
/// and enough right context to see what changed.
fn window(line: &str, col: usize) -> String {
    const BEFORE: usize = 30;
    const AFTER: usize = 50;
    let chars: Vec<char> = line.chars().collect();
    let start = col.saturating_sub(BEFORE);
    let end = col.saturating_add(AFTER).min(chars.len());
    let mut out = String::new();
    if start > 0 {
        out.push('…');
    }
    out.extend(chars[start..end].iter());
    if end < chars.len() {
        out.push('…');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_input_has_no_diff() {
        assert!(first_diff("a\nb\n", "a\nb\n").is_none());
    }

    #[test]
    fn a_changed_line_is_reported_with_both_sides() {
        let d = first_diff("a\nb\nc\n", "a\nX\nc\n").unwrap();
        assert!(d.contains("line 2"), "{d}");
        assert!(d.contains("col 1"), "{d}");
        assert!(d.contains("\"b\"") && d.contains("\"X\""), "{d}");
    }

    #[test]
    fn a_long_row_is_elided_around_the_change() {
        // A real table row is ~600 chars; quoting two in full buried
        // the one changed token in ~1200 characters of identical text.
        let committed = format!("{} NEEDLE {}", "x".repeat(400), "y".repeat(400));
        let generated = committed.replace("NEEDLE", "CHANGED");
        let d = first_diff(&committed, &generated).unwrap();
        assert!(
            d.len() < 300,
            "diagnostic must stay readable: {} chars",
            d.len()
        );
        assert!(d.contains("NEEDLE") && d.contains("CHANGED"), "{d}");
        // 400 `x`s + the space before the token.
        assert!(d.contains("col 402"), "the column locates it: {d}");
        assert!(d.contains('…'), "elision must be visible: {d}");
    }

    #[test]
    fn the_window_is_char_safe_around_an_em_dash() {
        // The tables are full of em dashes; a byte-sliced window would
        // panic on a char boundary or mis-count the column.
        let committed = format!("{} — a", "—".repeat(60));
        let generated = format!("{} — b", "—".repeat(60));
        let d = first_diff(&committed, &generated).unwrap();
        assert!(d.contains("col 64"), "{d}");
    }

    #[test]
    fn a_short_line_is_not_elided() {
        let d = first_diff("a\nshort line\n", "a\nshort lime\n").unwrap();
        assert!(!d.contains('…'), "{d}");
    }

    #[test]
    fn a_truncated_file_is_reported() {
        assert!(first_diff("a\n", "a\nb\n").unwrap().contains("extra"));
        assert!(first_diff("a\nb\n", "a\n").unwrap().contains("extra"));
    }

    #[test]
    fn a_trailing_newline_difference_is_still_a_diff() {
        // `lines()` cannot see it, but a byte-compare gate must.
        assert!(first_diff("a", "a\n").is_some());
    }

    #[test]
    fn listing_a_missing_directory_yields_nothing() {
        assert!(committed_files(Path::new("/nonexistent/docs/reference/cli")).is_empty());
    }

    #[test]
    fn listing_finds_nested_files_and_sorts_them() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("nested")).unwrap();
        std::fs::write(dir.path().join("b.md"), "b").unwrap();
        std::fs::write(dir.path().join("a.md"), "a").unwrap();
        std::fs::write(dir.path().join("nested/c.md"), "c").unwrap();
        let found: Vec<String> = committed_files(dir.path())
            .iter()
            .map(|p| relative(dir.path(), p))
            .collect();
        assert_eq!(found, vec!["a.md", "b.md", "nested/c.md"]);
    }
}
