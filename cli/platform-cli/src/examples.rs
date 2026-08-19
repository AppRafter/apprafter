// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Worked examples, one table, two consumers.
//!
//! An example is a **claim about this CLI**: `apprafter backup enable
//! --bucket …` asserts that the path exists and that the flag is that
//! command's. Two things read the claim — the binary, which renders it
//! into `--help` as `after_help`, and `docsgen`, which projects it into
//! `docs/reference/cli/**`. They must not be able to disagree, so there
//! is exactly one array and both read it.
//!
//! # Why a table rather than a delimiter inside `after_help`
//!
//! The alternative was to render the examples into `after_help` and have
//! `docsgen` parse them back out on a stable delimiter. It was rejected:
//!
//! * It would make the guard read a **rendering** of the truth instead
//!   of the truth. `docsgen check` already byte-compares a render
//!   against a render; adding a second check that derives its input from
//!   the same rendering path is the vacuity this track keeps finding.
//! * A parse that finds nothing **passes**. Change the delimiter, wrap
//!   the block, style it, or write an example that happens to contain
//!   the delimiter, and the guard silently judges zero examples while
//!   still reporting success. A guard whose input can quietly become
//!   empty is not a guard. (`docsgen::examples` still refuses an entry
//!   it cannot read, precisely because that failure mode has to be loud
//!   wherever it can occur.)
//! * A free-text convention is what the design spec warned against, and
//!   there is no escaping story for a delimiter that an author writing
//!   an example would ever remember.
//! * It couples the guard to the presentation. With the table, changing
//!   how `--help` groups, indents or colours the examples cannot break
//!   the check — and the check keeps working before the rendering
//!   exists at all, which is the order this subphase needs.
//!
//! [`crate::docs_api`] re-exports this module's surface, so widening it
//! stays a deliberate act rather than a side effect of a refactor.
//!
//! # What lives here, and what does not
//!
//! The array is content, not policy. Whether every leaf command *has*
//! an example, and whether the examples resolve against the clap tree,
//! are asserted elsewhere: the resolution guard is
//! `docsgen::examples::check` (run by `docsgen check`), because the clap
//! tree is what an example must be true of.

/// One command's examples, keyed by its path **without** the binary
/// name — `&["secret", "seal"]` for `apprafter secret seal`. The root
/// binary is the empty slice, the same key `docsgen`'s projection uses,
/// so the two indexes cannot drift apart.
#[derive(Debug)]
pub struct CommandExamples {
    /// The command path this example set belongs to.
    pub path: &'static [&'static str],
    /// One shell line each, written as a reader would type it, opening
    /// with `apprafter`.
    pub lines: &'static [&'static str],
}

/// Every documented example, in command-path order.
///
/// Empty today. The guard that judges an entry
/// (`docsgen::examples::check`) ships before the content does, on
/// purpose: `docs/reference/cli/**` is byte-compared rather than
/// resolved, so an unverified example lands in a blind spot between the
/// two documentation gates. Filling this array without the guard would
/// ship unverified invocations inside the artefact whose whole purpose
/// is to be true.
pub const EXAMPLES: &[CommandExamples] = &[];

/// The examples declared for `path`, or an empty slice.
pub fn examples_for(path: &[&str]) -> &'static [&'static str] {
    lookup(EXAMPLES, path)
}

/// The lookup, over an arbitrary table so it can be exercised on one
/// that actually has entries while [`EXAMPLES`] is still empty.
fn lookup(table: &'static [CommandExamples], path: &[&str]) -> &'static [&'static str] {
    table
        .iter()
        .find(|entry| entry.path == path)
        .map(|entry| entry.lines)
        .unwrap_or(&[])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The first path declared twice, if any.
    ///
    /// A duplicate is not a compile error and [`lookup`] would silently
    /// serve the first entry, so the second author's examples would
    /// vanish from both `--help` and the reference with nothing to
    /// notice. `docsgen`'s `the_projection_carries_exactly_what_the_cli
    /// _declares` catches the same thing from the other side, by
    /// counting lines; this catches it here, where the fix is.
    fn duplicate_path(table: &[CommandExamples]) -> Option<Vec<&'static str>> {
        let mut seen: Vec<&'static [&'static str]> = Vec::new();
        for entry in table {
            if seen.contains(&entry.path) {
                return Some(entry.path.to_vec());
            }
            seen.push(entry.path);
        }
        None
    }

    const SAMPLE: &[CommandExamples] = &[
        CommandExamples {
            path: &["app", "list"],
            lines: &["apprafter app list"],
        },
        CommandExamples {
            path: &["secret", "seal"],
            lines: &[
                "apprafter secret seal db-url --namespace web",
                "apprafter secret seal --help",
            ],
        },
    ];

    const DUPLICATED: &[CommandExamples] = &[
        CommandExamples {
            path: &["app", "list"],
            lines: &["apprafter app list"],
        },
        CommandExamples {
            path: &["app", "list"],
            lines: &["apprafter app list --env prod"],
        },
    ];

    #[test]
    fn a_declared_path_returns_its_lines_and_anything_else_is_empty() {
        assert_eq!(lookup(SAMPLE, &["app", "list"]), &["apprafter app list"]);
        assert_eq!(lookup(SAMPLE, &["secret", "seal"]).len(), 2);
        // A prefix of a declared path is a different command.
        assert!(lookup(SAMPLE, &["app"]).is_empty());
        assert!(lookup(SAMPLE, &["app", "list", "extra"]).is_empty());
        assert!(lookup(SAMPLE, &[]).is_empty());
    }

    #[test]
    fn a_duplicate_path_is_found() {
        // Proves the detector fires — the shipped table is empty today,
        // so asserting only on it would assert nothing.
        assert_eq!(duplicate_path(DUPLICATED), Some(vec!["app", "list"]));
        assert_eq!(duplicate_path(SAMPLE), None);
    }

    #[test]
    fn the_shipped_table_declares_each_path_once() {
        assert_eq!(duplicate_path(EXAMPLES), None);
    }
}
