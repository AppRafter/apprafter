// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The projection must carry everything the snippet gate resolves
//! against: paths, aliases, flags, defaults, requiredness, enums.

use clap::CommandFactory;
use docsgen::model::{tree_from, Tree};

fn tree() -> Tree {
    tree_from(&apprafter::docs_api::Cli::command(), "test")
}

fn node<'a>(t: &'a Tree, path: &[&str]) -> &'a docsgen::model::CommandNode {
    t.commands
        .iter()
        .find(|c| c.path == path)
        .unwrap_or_else(|| panic!("no command {path:?}"))
}

#[test]
fn nested_commands_are_projected_by_full_path() {
    let t = tree();
    let seal = node(&t, &["secret", "seal"]);
    // `assert!(!x)` rather than `assert_eq!(x, false)`: clippy's
    // bool_assert_comparison is deny-by-`-D warnings` in `just lint`.
    assert!(!seal.hidden);
    assert!(!seal.about.is_empty());
}

#[test]
fn flag_defaults_and_shorts_survive_the_projection() {
    let t = tree();
    let seal = node(&t, &["secret", "seal"]);
    let ns = seal
        .args
        .iter()
        .find(|a| a.long.as_deref() == Some("namespace"))
        .expect("secret seal has --namespace");
    assert_eq!(ns.short, Some('n'));
    assert_eq!(ns.default.as_deref(), Some("apprafter-system"));
    assert!(!ns.required);
    assert!(ns.takes_value);
}

#[test]
fn required_positionals_are_marked_required() {
    let t = tree();
    let seal = node(&t, &["secret", "seal"]);
    let name = seal
        .positionals
        .iter()
        .find(|p| p.id == "name")
        .expect("positional name");
    assert!(
        name.required,
        "the CLI snippet gate needs this to catch a missing required arg"
    );
}

#[test]
fn hidden_commands_are_present_but_flagged() {
    let t = tree();
    assert!(
        node(&t, &["auth"]).hidden,
        "auth is hide=true; the gate still resolves it"
    );
}

#[test]
fn visible_aliases_are_captured() {
    let t = tree();
    assert!(
        node(&t, &["target"]).aliases.contains(&"t".to_string()),
        "docs use `apprafter t use <name>`; without aliases the gate would fail correct docs"
    );
}

#[test]
fn every_visible_command_has_an_about() {
    let t = tree();
    let missing: Vec<_> = t
        .commands
        .iter()
        .filter(|c| !c.hidden && c.about.trim().is_empty())
        .map(|c| c.path.join(" "))
        .collect();
    assert!(
        missing.is_empty(),
        "commands without an `about`: {missing:?}"
    );
}

#[test]
fn projection_is_sorted_and_stable() {
    let a = tree();
    let b = tree();
    assert_eq!(
        serde_json::to_string(&a).unwrap(),
        serde_json::to_string(&b).unwrap()
    );
    let paths: Vec<_> = a.commands.iter().map(|c| c.path.clone()).collect();
    let mut sorted = paths.clone();
    sorted.sort();
    assert_eq!(paths, sorted, "byte-compare needs a deterministic order");
}
