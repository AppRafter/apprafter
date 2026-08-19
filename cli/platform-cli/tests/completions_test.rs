// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Every shell offered emits a script that is real.
//!
//! `apprafter completion <shell>` advertises a fixed set of shells in
//! `--help`, and each entry is a claim: that this binary can produce a
//! working completion script for that shell. Nothing else in the
//! repository judges it. `docsgen check` sees only that the command and
//! its examples resolve against the clap tree, which is true of a
//! command that prints an empty string.
//!
//! So the assertions run the **shipped binary** — the dispatch arm
//! included, because a handler nothing reaches is the failure mode a
//! unit test over the generator would sail past — and compare each
//! script against the clap tree it must describe. Artefact against
//! committed source, never a snapshot against a snapshot.
//!
//! The shell list is read from `clap_complete::Shell::value_variants()`
//! rather than written here. Writing it would put a second copy of the
//! supported set in the one place meant to check the first, so a shell
//! dropped from the argument and from the test in the same edit would
//! still pass.

use clap::CommandFactory;
use clap::ValueEnum;
use clap_complete::Shell;

use apprafter::docs_api::Cli;
use assert_cmd::Command;

/// Run the real binary and hand back what it printed.
fn script_for(shell: &str) -> String {
    let output = Command::cargo_bin("apprafter")
        .unwrap()
        .args(["completion", shell])
        .assert()
        .success();
    String::from_utf8(output.get_output().stdout.clone())
        .unwrap_or_else(|_| panic!("the {shell} script is not UTF-8"))
}

/// The shells the argument accepts, spelled as a user types them.
fn offered_shells() -> Vec<String> {
    Shell::value_variants()
        .iter()
        .map(|shell| shell.to_string())
        .collect()
}

/// Top-level commands `--help` lists. Hidden ones are excluded on
/// purpose: this is a superset assertion, and the generator does emit
/// hidden subcommands (see `commands::completions`), so demanding their
/// absence here would pin somebody else's behaviour rather than ours.
fn visible_top_level_commands() -> Vec<String> {
    Cli::command()
        .get_subcommands()
        .filter(|command| !command.is_hide_set())
        .map(|command| command.get_name().to_string())
        .collect()
}

#[test]
fn every_offered_shell_emits_a_script_that_describes_this_cli() {
    let shells = offered_shells();
    let commands = visible_top_level_commands();
    // Both sides are derived, so both can go empty — and "every one of
    // nothing satisfies everything" is how a check of this shape dies
    // quietly.
    assert!(shells.len() >= 3, "only {shells:?} offered");
    assert!(commands.len() >= 20, "only {commands:?} in the clap tree");

    for shell in &shells {
        let script = script_for(shell);
        assert!(
            script.lines().count() > 50,
            "the {shell} script is {} line(s) — that is not a completion \
             script",
            script.lines().count()
        );
        for command in &commands {
            assert!(
                script.contains(command.as_str()),
                "the {shell} script never mentions `apprafter {command}`, \
                 so it does not complete this CLI"
            );
        }
    }
}

#[test]
fn every_script_completes_the_name_this_binary_answers_to() {
    // The generator writes the binary name into the script, and a
    // script keyed to some other name completes nothing. Read from the
    // clap tree so a rename cannot leave the two disagreeing.
    let binary = Cli::command().get_name().to_string();
    assert!(!binary.is_empty());
    for shell in offered_shells() {
        assert!(
            script_for(&shell).contains(&binary),
            "the {shell} script never names `{binary}`"
        );
    }
}

#[test]
fn an_unsupported_shell_is_refused_and_the_real_ones_are_named() {
    // Proves the argument is still a closed set rather than a free
    // string that quietly emits nothing for a typo.
    let output = Command::cargo_bin("apprafter")
        .unwrap()
        .args(["completion", "tcsh"])
        .assert()
        .failure();
    let stderr = String::from_utf8(output.get_output().stderr.clone()).expect("stderr is UTF-8");
    assert!(stderr.contains("tcsh"), "{stderr}");
    for shell in offered_shells() {
        assert!(
            stderr.contains(&shell),
            "the refusal does not name `{shell}`:\n{stderr}"
        );
    }
}
