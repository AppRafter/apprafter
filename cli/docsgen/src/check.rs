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
//! `operator/crdgen/src/check.rs` is the precedent; the diagnostic
//! shape (first differing line, committed vs generated) is deliberately
//! the same, because a contributor who has seen one gate should
//! recognise the other.

use crate::render::{render_all, DIR};
use clap::Command;
use std::collections::BTreeSet;
use std::error::Error;
use std::path::{Path, PathBuf};

/// The remedy for a drifted or missing artefact — the common case, so
/// it leads the report. A stray is the exception: regenerating cannot
/// delete a file `generate` no longer writes, so those lines carry
/// their own `git rm` remedy rather than silently inheriting one that
/// would not work.
const REMEDY: &str = "run `just docsgen-generate` (the clap tree is the source of truth)";

/// Byte-compare the committed CLI reference against a fresh render of
/// `root_cmd`. Every artefact is compared before reporting, so one run
/// surfaces the whole drift rather than one file per iteration.
pub fn check(root_cmd: &Command, repo_root: &Path) -> Result<(), Box<dyn Error>> {
    let rendered = render_all(root_cmd, repo_root);
    let mut problems = Vec::new();

    // Assertion A — every rendered artefact matches the committed file.
    for artefact in &rendered {
        let shown = relative(repo_root, &artefact.path);
        match std::fs::read_to_string(&artefact.path) {
            Ok(committed) => {
                if let Some(diff) = first_diff(&committed, &artefact.text) {
                    problems.push(format!("[A drift] {shown}\n      {diff}"));
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                problems.push(format!("[A missing] {shown}: generated but not committed"));
            }
            Err(e) => problems.push(format!("[A unreadable] {shown}: {e}")),
        }
    }

    // Assertion B — nothing else lives in the generated directory.
    let expected: BTreeSet<&Path> = rendered.iter().map(|a| a.path.as_path()).collect();
    for path in committed_files(&repo_root.join(DIR)) {
        if !expected.contains(path.as_path()) {
            let shown = relative(repo_root, &path);
            problems.push(format!(
                "[B stray] {shown}: committed but no longer generated — a removed \
                 command leaves a page that still publishes; delete it (`git rm {shown}`)"
            ));
        }
    }

    if problems.is_empty() {
        eprintln!(
            "docsgen check OK: {} artefact(s) in {DIR} match the clap tree",
            rendered.len()
        );
        return Ok(());
    }
    Err(format!(
        "docs-check FAILED: {} CLI-reference drift(s) — {REMEDY}:\n  {}",
        problems.len(),
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
                return Some(format!("line {n}: committed {a:?} != generated {b:?}"))
            }
            (Some(a), None) => return Some(format!("line {n}: committed has extra {a:?}")),
            (None, Some(b)) => return Some(format!("line {n}: generated has extra {b:?}")),
            // `lines()` ignores a trailing-newline difference, so equal
            // line sequences with unequal bytes land here.
            (None, None) => return Some("content equal but byte length differs".into()),
        }
    }
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
        assert!(d.contains("\"b\"") && d.contains("\"X\""), "{d}");
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
