// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The shipped examples, against the shipped clap tree.
//!
//! `docsgen check` runs the same assertion, but only under `nix
//! develop` via `scripts/docs-check.sh`. This puts it in `cargo test
//! --workspace` too, where an author writing an example is already
//! working — a guard that only fires in the slowest gate is a guard
//! discovered late.
//!
//! The per-defect proofs (a misspelled flag, a sibling's flag, an entry
//! that reads as nothing) live in `src/examples.rs`, where they can
//! inject examples into the real tree. This file asserts the one thing
//! they cannot: that what is ACTUALLY declared holds up.

use clap::CommandFactory;

#[test]
fn every_shipped_example_resolves_against_the_clap_tree() {
    let tree = docsgen::model::tree_from(&apprafter::docs_api::Cli::command(), "test");
    let findings = docsgen::examples::check(&tree).expect("the real projection indexes");
    assert!(
        findings.is_empty(),
        "{} example(s) do not resolve:\n  {}",
        findings.len(),
        findings
            .iter()
            .map(|f| f.to_string())
            .collect::<Vec<_>>()
            .join("\n  ")
    );
}

/// The census this guard covers, printed rather than pinned.
///
/// A committed count would rot on the first example written; the
/// command that produces it does not. `cargo test -p docsgen --test
/// examples_test -- --nocapture` reads it out.
#[test]
fn report_the_example_census() {
    let tree = docsgen::model::tree_from(&apprafter::docs_api::Cli::command(), "test");
    let commands = tree.commands.iter().filter(|c| !c.path.is_empty()).count();
    let with = tree
        .commands
        .iter()
        .filter(|c| !c.examples.is_empty())
        .count();
    let lines: usize = tree.commands.iter().map(|c| c.examples.len()).sum();
    println!("examples: {with} of {commands} command(s) carry one; {lines} line(s) total");
}
