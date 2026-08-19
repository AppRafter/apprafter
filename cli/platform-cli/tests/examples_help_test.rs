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

use apprafter::docs_api::examples_for;
use assert_cmd::Command;

fn help_for(path: &[&str]) -> String {
    let mut command = Command::cargo_bin("apprafter").unwrap();
    let output = command.args(path).arg("--help").assert().success();
    String::from_utf8(output.get_output().stdout.clone()).expect("help is UTF-8")
}

/// Every line the table declares for `path` appears verbatim under an
/// `Examples:` heading in that command's `--help`.
fn assert_help_shows_the_declared_examples(path: &[&str]) {
    let declared = examples_for(path);
    assert!(
        !declared.is_empty(),
        "`apprafter {}` declares no example, so this assertion would \
         hold vacuously — fix the table, not the test",
        path.join(" ")
    );

    let help = help_for(path);
    let (_, after) = help.split_once("\nExamples:\n").unwrap_or_else(|| {
        panic!(
            "`apprafter {} --help` has no `Examples:` section:\n{help}",
            path.join(" ")
        )
    });
    for line in declared {
        assert!(
            after.contains(line),
            "`apprafter {} --help` does not show the declared example {line:?}",
            path.join(" ")
        );
    }
}

#[test]
fn a_leaf_commands_help_shows_its_examples() {
    // One from each shape the renderer has to survive: a long flag
    // list, a nested path, a value carrying `=`, and an entry whose
    // example is wrapped in a shell substitution.
    assert_help_shows_the_declared_examples(&["app", "add"]);
    assert_help_shows_the_declared_examples(&["target", "firewall", "cloudflare-origin"]);
    assert_help_shows_the_declared_examples(&["secret", "seal"]);
    assert_help_shows_the_declared_examples(&["kubeconfig"]);
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
    assert!(!help_for(&["app"]).contains("\nExamples:"));
}

#[test]
fn the_root_help_is_untouched() {
    assert!(examples_for(&[]).is_empty(), "the root declares none");
    let help = help_for(&[]);
    assert!(!help.contains("\nExamples:"));
    // Still the real root help, so the assertion above is about the
    // examples and not about a command that failed to render.
    assert!(help.contains("bootstrap-all"), "{help}");
}
