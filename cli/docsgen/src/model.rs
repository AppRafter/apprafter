// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! A serialisable projection of the `apprafter` clap tree.
//!
//! This is the contract the reference renderer and the documentation
//! snippet gate both read (the latter as `commands.json`). It
//! deliberately carries requiredness, defaults, value enums and
//! aliases: each of those is a class of drift the gate exists to
//! catch, so dropping one here silently blinds the gate.
//!
//! Determinism is a hard requirement — `docsgen check` byte-compares
//! a fresh render against the committed one, so any ordering that
//! wobbled between runs would produce phantom drift. Every collection
//! is built from clap's `Vec`-backed iterators (never a hash map) and
//! explicitly sorted where declaration order carries no meaning.
//!
//! Note the model has no `requires` / `conflicts`: this clap version
//! exposes no getter for them, so they are out of scope by necessity
//! rather than by choice.

use clap::builder::PossibleValue;
use clap::{Arg, Command};
use serde::{Deserialize, Serialize};

/// The whole CLI, flattened to one node per command path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tree {
    pub cli_version: String,
    pub commands: Vec<CommandNode>,
}

/// One command, including the root binary itself (empty `path`) —
/// otherwise `apprafter --version` and `apprafter <cmd> --help` have
/// no node to resolve against.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandNode {
    /// Full path without the binary name, e.g. `["secret", "seal"]`.
    pub path: Vec<String>,
    /// Every alias, visible or hidden. The CLI declares its aliases
    /// with `alias =` (hidden), and the docs still use them
    /// (`apprafter t use <name>`), so a visible-only projection would
    /// make the gate reject correct documentation.
    pub aliases: Vec<String>,
    pub about: String,
    pub long_about: Option<String>,
    /// clap's own usage line, e.g.
    /// `Usage: apprafter target rename <FROM> <TO>`.
    ///
    /// Argument ORDER is meaning for a multi-positional command, and
    /// the rendered table is sorted by id — so without this the
    /// reference cannot answer "which one comes first?". Taken from
    /// clap rather than assembled here so it cannot disagree with
    /// `--help`. It is width-independent: usage is built by
    /// `Usage::create_usage_with_title`, which never reaches the
    /// terminal-width lookup that the help-template renderer uses, and
    /// that lookup is behind the `wrap_help` feature this workspace
    /// does not enable. The resulting structure — one line, no padding
    /// — is asserted in `usage_is_a_single_unwrapped_line`.
    pub usage: String,
    /// Hidden from `--help`, directly or via an ancestor; the gate
    /// still resolves these paths. clap sets `hide` on the node it is
    /// declared on, so this is inherited explicitly — otherwise
    /// `auth login` would look public while `auth` did not.
    pub hidden: bool,
    /// Empty today; a later package fills these in.
    pub examples: Vec<String>,
    /// Flags and options, sorted by id.
    pub args: Vec<ArgSpec>,
    /// Positionals, in declaration order — their order is their
    /// meaning, so this vec is never sorted.
    pub positionals: Vec<PositionalSpec>,
}

/// A flag or option (anything reached by `-x` / `--xyz`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArgSpec {
    pub id: String,
    pub long: Option<String>,
    pub short: Option<char>,
    pub takes_value: bool,
    pub value_name: Option<String>,
    pub default: Option<String>,
    /// The environment variable this arg also reads, if any
    /// (`#[arg(env = "HCLOUD_TOKEN")]`). Eight declarations across four
    /// variables exist, and the hand-written reference page that used
    /// to document them is being retired — without this the env
    /// surface would simply disappear from the docs.
    pub env: Option<String>,
    pub required: bool,
    pub possible_values: Vec<String>,
    pub help: String,
}

/// A positional argument.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PositionalSpec {
    pub id: String,
    pub value_name: String,
    pub required: bool,
    pub multiple: bool,
    /// `ValueEnum` variants, same as [`ArgSpec::possible_values`].
    /// `target firewall cloudflare-origin <STATE>` accepts only
    /// `enable`/`disable`; without this the accepted set survives
    /// solely as prose in `help`, so renaming a variant would change
    /// the real CLI surface and still byte-compare clean.
    pub possible_values: Vec<String>,
    pub help: String,
}

/// Project a clap tree into [`Tree`].
///
/// The command is cloned and `build()`-ed first. `CommandFactory::command()`
/// hands back an *unbuilt* tree in which derived state — notably
/// `num_args` — is still unset; clap documents `build()` as the entry
/// point for exactly this kind of introspection.
pub fn tree_from(root: &Command, cli_version: &str) -> Tree {
    let mut built = root.clone();
    built.build();

    let mut commands = Vec::new();
    // The root binary itself is a node (empty `path`), not just its
    // subcommands: `apprafter --version` and `apprafter <cmd> --help`
    // resolve against IT, and without a node here the projection has
    // nowhere to answer either. It is the one node that keeps clap's
    // generated `--help`/`--version` — every other node has them
    // filtered by `node_from`, because at every other level they are
    // documented once, globally, right here.
    commands.push(node_from_keeping_generated(&built));
    collect(&built, &mut Vec::new(), false, &mut commands);
    // Sorting by path makes the projection independent of declaration
    // order, so reordering an enum variant is not spurious drift. An
    // empty path sorts first, so the root node lands at index 0 —
    // stable, and never a candidate for the `path.len() == 1` filter
    // `render.rs` uses to pick top-level pages.
    commands.sort_by(|a, b| a.path.cmp(&b.path));

    Tree {
        cli_version: cli_version.to_string(),
        commands,
    }
}

/// Walk every subcommand depth-first, accumulating full paths.
///
/// `ancestor_hidden` carries `hide` down the tree: a subtree under a
/// hidden parent is itself absent from `--help`, so a renderer that
/// filters on `hidden` must see that.
fn collect(
    cmd: &Command,
    prefix: &mut Vec<String>,
    ancestor_hidden: bool,
    out: &mut Vec<CommandNode>,
) {
    let mut subs: Vec<&Command> = cmd
        .get_subcommands()
        // `build()` materialises clap's own `help` subcommand at every
        // level. It is not part of the documented surface.
        .filter(|s| s.get_name() != "help")
        .collect();
    subs.sort_by(|a, b| a.get_name().cmp(b.get_name()));

    for sub in subs {
        let hidden = ancestor_hidden || sub.is_hide_set();
        prefix.push(sub.get_name().to_string());
        out.push(node_from(sub, prefix.clone(), hidden));
        collect(sub, prefix, hidden, out);
        prefix.pop();
    }
}

fn node_from(cmd: &Command, path: Vec<String>, hidden: bool) -> CommandNode {
    node_from_inner(cmd, path, hidden, false)
}

/// The root binary's own node: everywhere else `--help`/`--version` are
/// filtered out because they are documented once, globally — but the
/// root IS that "once, globally", so it must keep them or the gate has
/// nowhere to resolve `apprafter --version` against.
fn node_from_keeping_generated(cmd: &Command) -> CommandNode {
    node_from_inner(cmd, Vec::new(), false, true)
}

fn node_from_inner(
    cmd: &Command,
    path: Vec<String>,
    hidden: bool,
    keep_generated: bool,
) -> CommandNode {
    let mut aliases: Vec<String> = cmd.get_all_aliases().map(str::to_string).collect();
    aliases.sort();
    aliases.dedup();

    let mut args = Vec::new();
    let mut positionals = Vec::new();
    for arg in cmd.get_arguments() {
        let id = arg.get_id().as_str();
        // clap generates these; they are documented once, globally —
        // except on the root node itself, which IS that one place.
        if !keep_generated && (id == "help" || id == "version") {
            continue;
        }
        if arg.is_positional() {
            positionals.push(positional_from(arg));
        } else {
            args.push(arg_from(arg));
        }
    }
    args.sort_by(|a: &ArgSpec, b: &ArgSpec| a.id.cmp(&b.id));

    CommandNode {
        path,
        aliases,
        about: cmd.get_about().map(styled_one_line).unwrap_or_default(),
        long_about: cmd.get_long_about().map(|s| reflow(&s.to_string())),
        usage: cmd.clone().render_usage().to_string(),
        hidden,
        examples: Vec::new(),
        args,
        positionals,
    }
}

fn arg_from(arg: &Arg) -> ArgSpec {
    ArgSpec {
        id: arg.get_id().as_str().to_string(),
        long: arg.get_long().map(str::to_string),
        short: arg.get_short(),
        takes_value: takes_value(arg),
        value_name: first_value_name(arg),
        default: default_of(arg),
        // `hide_env_values` hides the value, not the name, so the
        // variable is still documentable for a secret-bearing flag
        // like `--token`.
        env: arg
            .get_env()
            .map(|e| e.to_string_lossy().into_owned())
            .filter(|_| !arg.is_hide_env_set()),
        required: arg.is_required_set(),
        possible_values: possible_values_of(arg),
        help: arg.get_help().map(styled_one_line).unwrap_or_default(),
    }
}

fn positional_from(arg: &Arg) -> PositionalSpec {
    PositionalSpec {
        id: arg.get_id().as_str().to_string(),
        value_name: first_value_name(arg).unwrap_or_else(|| arg.get_id().as_str().to_uppercase()),
        required: arg.is_required_set(),
        multiple: arg.get_num_args().is_some_and(|r| r.max_values() > 1),
        possible_values: possible_values_of(arg),
        help: arg.get_help().map(styled_one_line).unwrap_or_default(),
    }
}

fn possible_values_of(arg: &Arg) -> Vec<String> {
    arg.get_possible_values()
        .iter()
        .map(PossibleValue::get_name)
        .map(str::to_string)
        .collect()
}

/// Whether the arg consumes a value. `tree_from` always builds, so
/// `num_args` is populated and authoritative.
fn takes_value(arg: &Arg) -> bool {
    arg.get_num_args().is_some_and(|range| range.takes_values())
}

fn first_value_name(arg: &Arg) -> Option<String> {
    arg.get_value_names()
        .and_then(|names| names.first())
        .map(|name| name.to_string())
}

/// The arg's default, or `None` when it has none *or* takes no value.
///
/// The `takes_value` gate is load-bearing, not defensive: `Arg::_build`
/// materialises a synthetic `"false"` default for every
/// `ArgAction::SetTrue`, so without it all 48 boolean switches
/// (`--yes`, `--stdout`, `--force`, …) would project
/// `default: Some("false")` and the rendered reference would claim a
/// default where `apprafter --help` prints none. clap's own help
/// writer gates the same way (`is_takes_value_set()`).
fn default_of(arg: &Arg) -> Option<String> {
    if !takes_value(arg) {
        return None;
    }
    let defaults = arg.get_default_values();
    if defaults.is_empty() {
        return None;
    }
    Some(
        defaults
            .iter()
            .map(|value| value.to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join(","),
    )
}

fn styled_one_line(styled: &clap::builder::StyledStr) -> String {
    one_line(&styled.to_string())
}

/// Collapse a doc comment to one line.
///
/// clap doc comments are hard-wrapped in the source, so every run of
/// whitespace — newlines included — becomes a single space.
///
/// A wrap that falls INSIDE a token (`workload-` / `specific`) cannot
/// be repaired here, and the attempt was reverted: `clap_derive`
/// collapses each paragraph's hard wraps to spaces before clap — let
/// alone docsgen — ever sees the text, so the line boundary a repair
/// would key on does not exist by then (`get_about()` for `app
/// scaffold` returns one line, no `\n`). Wrapping mid-token is
/// therefore a defect in the doc comment, and
/// `a_hard_wrap_never_falls_inside_a_token` in `render_test.rs` fails
/// the build when one appears — over the REAL rendered artefacts,
/// which is the only place the evidence survives.
pub fn one_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Undo source hard-wrapping in a `long_about` while keeping its
/// paragraph structure.
///
/// [`one_line`] is right for `about` and per-arg help, which are single
/// sentences by construction. `long_about` is not — four commands carry
/// blank-line-separated paragraphs — so each paragraph is unwrapped
/// individually and the blank lines between them survive for the
/// renderer to turn into real paragraphs.
///
/// Indentation is deliberately not special-cased: `clap_derive`
/// de-indents doc comments before clap sees them, so a hand-aligned
/// table in the source (`backup enable` has one) has already lost its
/// columns by the time it reaches us. A pre-formatted branch here
/// could never fire; if such a table is ever wanted in the rendered
/// docs it has to be re-authored downstream, not recovered here.
fn reflow(s: &str) -> String {
    let mut paragraphs: Vec<String> = Vec::new();
    let mut current: Vec<&str> = Vec::new();

    for line in s.lines() {
        if line.trim().is_empty() {
            flush(&mut current, &mut paragraphs);
        } else {
            current.push(line);
        }
    }
    flush(&mut current, &mut paragraphs);

    paragraphs.join("\n\n")
}

fn flush(current: &mut Vec<&str>, paragraphs: &mut Vec<String>) {
    if current.is_empty() {
        return;
    }
    paragraphs.push(one_line(&current.join(" ")));
    current.clear();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_line_collapses_hard_wraps() {
        assert_eq!(one_line("a\n  b\n\nc  d"), "a b c d");
        assert_eq!(one_line(""), "");
    }

    #[test]
    fn reflow_keeps_paragraph_breaks() {
        assert_eq!(
            reflow("one\nline\n\nsecond\npara"),
            "one line\n\nsecond para"
        );
    }

    #[test]
    fn reflow_does_not_weld_paragraphs_into_a_run_on() {
        // The renderer needs the break to emit two <p>s; `one_line`
        // over the whole string would lose it.
        assert_ne!(reflow("a\n\nb"), one_line("a\n\nb"));
    }
}
