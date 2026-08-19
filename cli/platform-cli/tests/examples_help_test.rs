// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! `--help` must actually SHOW the worked examples.
//!
//! `docsgen` proves the table reaches `docs/reference/cli/**`, and
//! `docsgen check` proves every entry resolves against the clap tree.
//! Neither runs the binary, so both stay green if `run()` goes back to
//! a plain `Cli::parse()` and the examples silently vanish from the one
//! surface a user meets at the terminal. That is the whole defect class
//! this subphase exists to close, so it gets an assertion of its own.
//!
//! Each assertion reads the shipped table at test time and demands the
//! rendered help agree — artefact against committed source, never a
//! snapshot against a snapshot. The declared lines are checked for
//! being non-empty first, because "contains all of nothing" is true.
//!
//! # Every command, and both renderings
//!
//! An earlier version sampled four commands and only `--help`. Both
//! narrowings were holes. `-h` and `--help` are separate renderings in
//! clap — `after_long_help` takes over the long one and falls back to
//! `after_help` for the short — so one ordinary attribute on one
//! variant removed the `Examples:` section from `--help` while `-h`,
//! the reference page, `docsgen check` and all four sampled commands
//! stayed green. `crate::examples::attach` now refuses that attribute
//! outright; this file is the other side of the same defect, and it
//! walks the WHOLE table in BOTH renderings so a hole cannot hide in
//! the 71 commands nobody sampled.

use apprafter::docs_api::{examples_for, EXAMPLES};
use assert_cmd::Command;

/// `path`'s help as the binary prints it, through the given flag.
fn help_for(path: &[&str], flag: &str) -> String {
    let mut command = Command::cargo_bin("apprafter").unwrap();
    let output = command.args(path).arg(flag).assert().success();
    String::from_utf8(output.get_output().stdout.clone()).expect("help is UTF-8")
}

/// Every line the table declares for `path` appears verbatim under an
/// `Examples:` heading, in both `-h` and `--help`.
fn assert_help_shows_the_declared_examples(path: &[&str]) {
    let declared = examples_for(path);
    assert!(
        !declared.is_empty(),
        "`apprafter {}` declares no example, so this assertion would \
         hold vacuously — fix the table, not the test",
        path.join(" ")
    );

    for flag in ["-h", "--help"] {
        let help = help_for(path, flag);
        let (_, after) = help.split_once("\nExamples:\n").unwrap_or_else(|| {
            panic!(
                "`apprafter {} {flag}` has no `Examples:` section:\n{help}",
                path.join(" ")
            )
        });
        for line in declared {
            assert!(
                after.contains(line),
                "`apprafter {} {flag}` does not show the declared example {line:?}",
                path.join(" ")
            );
        }
    }
}

#[test]
fn every_command_that_declares_an_example_shows_it_in_both_help_renderings() {
    assert!(
        EXAMPLES.len() >= 70,
        "the examples table holds {} entr(ies) — too few for this to be \
         walking the shipped table",
        EXAMPLES.len()
    );
    for entry in EXAMPLES {
        assert_help_shows_the_declared_examples(entry.path);
    }
}

#[test]
fn a_command_with_no_examples_grows_no_empty_section() {
    // `app` is a pure branch: its help is the subcommand list, and an
    // `Examples:` heading with nothing under it would be worse than
    // none. Proves `attach` is selective rather than unconditional.
    assert!(
        examples_for(&["app"]).is_empty(),
        "this test asserts the ABSENCE of a section; it means nothing \
         once `apprafter app` declares an example"
    );
    for flag in ["-h", "--help"] {
        assert!(!help_for(&["app"], flag).contains("\nExamples:"));
    }
}

#[test]
fn the_root_help_is_untouched() {
    assert!(examples_for(&[]).is_empty(), "the root declares none");
    for flag in ["-h", "--help"] {
        let help = help_for(&[], flag);
        assert!(!help.contains("\nExamples:"));
        // Still the real root help, so the assertion above is about the
        // examples and not about a command that failed to render.
        assert!(help.contains("bootstrap-all"), "{help}");
    }
}
