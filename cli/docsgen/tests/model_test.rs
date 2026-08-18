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
fn env_backed_flags_carry_their_variable() {
    let t = tree();
    // `apprafter target add --token` reads HCLOUD_TOKEN. The
    // hand-written reference page documented the env surface in a
    // table of its own; once that page is retired this projection is
    // the only place it survives.
    let token = node(&t, &["target", "add"])
        .args
        .iter()
        .find(|a| a.long.as_deref() == Some("token"))
        .expect("target add has --token");
    assert_eq!(token.env.as_deref(), Some("HCLOUD_TOKEN"));

    // ...and a flag without one stays None rather than empty-string.
    let seal = node(&t, &["secret", "seal"]);
    assert_eq!(
        seal.args
            .iter()
            .find(|a| a.long.as_deref() == Some("namespace"))
            .expect("secret seal has --namespace")
            .env,
        None
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
fn hidden_is_inherited_by_descendants() {
    let t = tree();
    // clap sets `hide` only on the node it is declared on, so without
    // explicit inheritance `auth login` reads as public and a renderer
    // filtering on `hidden` would emit pages for a subtree that
    // `apprafter --help` does not show.
    assert!(
        node(&t, &["auth", "login"]).hidden,
        "auth login sits under a hide=true parent"
    );
}

#[test]
fn switches_report_no_default_even_though_clap_synthesises_one() {
    let t = tree();
    let seal = node(&t, &["secret", "seal"]);
    let flag = |long: &str| {
        seal.args
            .iter()
            .find(|a| a.long.as_deref() == Some(long))
            .unwrap_or_else(|| panic!("secret seal has --{long}"))
    };

    // `Arg::_build` injects a synthetic "false" default for every
    // ArgAction::SetTrue. Reporting it would print `default: false`
    // on a switch where `--help` prints nothing — drift baked into a
    // byte-compared artefact.
    let stdout = flag("stdout");
    assert!(!stdout.takes_value);
    assert_eq!(stdout.default, None, "a bare switch has no default");

    // ...while a genuine value-taking option keeps its default.
    assert_eq!(
        flag("namespace").default.as_deref(),
        Some("apprafter-system")
    );
}

#[test]
fn value_enum_positionals_keep_their_accepted_values() {
    let t = tree();
    let state = node(&t, &["target", "firewall", "cloudflare-origin"])
        .positionals
        .iter()
        .find(|p| p.id == "state")
        .expect("positional state");
    assert_eq!(
        state.possible_values,
        vec!["enable".to_string(), "disable".to_string()],
        "renaming a ValueEnum variant must not byte-compare clean"
    );
}

#[test]
fn aliases_are_captured() {
    let t = tree();
    // These are `alias = ` (HIDDEN) aliases, not `visible_alias`, so
    // `get_visible_aliases()` returns nothing for any of them. The
    // docs use them anyway (`apprafter t use <name>`, `apprafter cb`),
    // so a visible-only projection would fail correct docs.
    assert!(node(&t, &["target"]).aliases.contains(&"t".to_string()));
    assert!(node(&t, &["cluster-bootstrap"])
        .aliases
        .contains(&"cb".to_string()));
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

#[test]
fn usage_carries_positional_order_which_the_sorted_table_loses() {
    let t = tree();
    // `target rename <FROM> <TO>`: the arg table is sorted by id, so
    // "from" and "to" happen to come out in order — `<TO> <FROM>`
    // would render identically. Only the usage line pins it.
    assert_eq!(
        node(&t, &["target", "rename"]).usage,
        "Usage: apprafter target rename <FROM> <TO>"
    );
    // A group node still gets one, and it names the binary path.
    assert_eq!(
        node(&t, &["target", "cert"]).usage,
        "Usage: apprafter target cert <COMMAND>"
    );
}

/// The usage line is byte-compared in CI, so anything width-sensitive
/// in it would drift between a wide local terminal and a CI runner.
///
/// Nothing in it can be. Terminal width reaches clap through exactly
/// one function, `clap_builder::output::help_template::dimensions()`,
/// and the only two callers are in the HELP template renderer. Usage
/// is built by a different path — `Command::render_usage` →
/// `Usage::create_usage_with_title` — which concatenates the arg specs
/// and never consults it. On top of that, `dimensions()` is behind
/// clap's `wrap_help` feature, and `cli/Cargo.toml` enables only
/// `derive` + `env`.
///
/// So this asserts the STRUCTURE that follows: one line, no wrapping,
/// and no run of padding spaces. A previous version of this test set
/// `COLUMNS` to 20 and to 400 and compared the two renders. That could
/// never fail: it poked the one mechanism already proven inert, so it
/// would have passed identically had usage become width-sensitive by
/// any other route.
#[test]
fn the_root_binary_is_a_node_so_global_flags_resolve() {
    let t = tree();
    let root = t
        .commands
        .iter()
        .find(|c| c.path.is_empty())
        .expect("the root binary must be projected, or `apprafter --version` cannot resolve");
    let longs: Vec<_> = root.args.iter().filter_map(|a| a.long.clone()).collect();
    assert!(longs.contains(&"help".to_string()));
    assert!(longs.contains(&"version".to_string()));
}

#[test]
fn usage_is_a_single_unwrapped_line() {
    let t = tree();
    for c in &t.commands {
        assert!(
            !c.usage.contains('\n'),
            "usage for {:?} wrapped onto a second line: {:?}",
            c.path,
            c.usage
        );
        assert!(
            !c.usage.contains("  "),
            "usage for {:?} carries padding, which a width-aware renderer \
             would size differently: {:?}",
            c.path,
            c.usage
        );
    }
}
