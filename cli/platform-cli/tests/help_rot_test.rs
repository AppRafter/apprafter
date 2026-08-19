// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! `--help` rots quietly. These are the properties that hold it up.
//!
//! A snapshot of every help page was considered and rejected: it fails
//! on every wording improvement, and a test that fails for a good
//! reason gets regenerated without being read — which makes it worse
//! than no test, because it also absorbs the failures that were real.
//! What is pinned here instead is the small set of properties that a
//! human reading the diff would not notice breaking, and that break
//! something *outside* the diff when they do.
//!
//! # Every expectation is derived, at test time
//!
//! The clap tree is read live (`Cli::command()`), never from a
//! committed copy of itself. A snapshot compared against a snapshot
//! proves only that a file was regenerated, and this track has now
//! found that shape roughly ten times.
//!
//! The one committed list here is [`GUIDE_ALIASES`] — the alias
//! invocations the published guides type — and it is committed
//! *because* it is the other side of the assertion. Deriving it from
//! the clap tree would be exactly circular: delete the `t` alias and a
//! clap-derived list simply stops containing it, so the check passes on
//! an alias that no longer exists. The list is instead re-derived from
//! the corpus by the command recorded on it, and
//! [`the_guides_still_type_every_alias_this_table_claims`] fails if the
//! table has drifted into fiction.
//!
//! # No assertion may hold vacuously
//!
//! Each one walks a collection it derived, and each asserts that
//! collection is populated before judging it. "Every one of nothing
//! satisfies everything" is how a check of this shape dies silently,
//! and a silent pass is the failure mode the whole subphase exists to
//! close.
//!
//! # What is deliberately not pinned here
//!
//! * **Wording, ordering, layout of any help page.** That is the
//!   snapshot this file refuses to be.
//! * **That an example resolves against the clap tree** (its path and
//!   flags exist) — `docsgen::examples::check`, run by `docsgen check`
//!   and by `cli/docsgen/tests/examples_test.rs`. The clap tree is what
//!   an example must be true *of*, so the guard lives beside it.
//! * **That `--help` renders the examples at all** —
//!   `examples_help_test.rs`.
//! * **That two entries do not claim the same command** (the second
//!   would be shadowed and render nowhere) — `docsgen`'s
//!   `the_projection_carries_exactly_what_the_cli_declares`.
//! * **That a *newly written* guide line using an alias resolves** —
//!   `docsgen gate` resolves every invocation in the in-scope corpus
//!   against the clap tree, aliases included, so a guide that starts
//!   typing one is judged the moment it is written. This file covers
//!   the known set in `cargo test`, which is the loop somebody editing
//!   `cli.rs` actually runs, and names the defect in those terms.
//!   Re-deriving the set here would need a second invocation
//!   tokeniser, and two tokenisers in one repository disagree.

use std::path::PathBuf;

use apprafter::docs_api::{examples_for, Cli, EXAMPLES};
use assert_cmd::Command as ShippedBinary;
use clap::{Command, CommandFactory};

// ---------------------------------------------------------------- tree

/// One command in the built tree.
struct Node<'a> {
    /// Path without the binary name — `["secret", "seal"]`.
    path: Vec<String>,
    /// Hidden from `--help`, itself or through an ancestor.
    hidden: bool,
    /// No subcommands of its own — see [`is_leaf`].
    leaf: bool,
    command: &'a Command,
}

impl Node<'_> {
    fn display(&self) -> String {
        if self.path.is_empty() {
            "apprafter".into()
        } else {
            format!("apprafter {}", self.path.join(" "))
        }
    }

    fn key(&self) -> Vec<&str> {
        self.path.iter().map(String::as_str).collect()
    }
}

/// The built clap tree.
///
/// `build()` is what materialises clap's derived state; `docsgen`'s
/// projection makes the same call for the same reason, so both read one
/// tree in one condition.
fn built_tree() -> Command {
    let mut root = Cli::command();
    root.build();
    root
}

/// Every node, root included, hidden inherited down the tree.
///
/// clap's own generated `help` subcommand is filtered out at every
/// level: it is clap's surface, not this CLI's, and pinning properties
/// on it would make these tests partly a test of clap. `docsgen::model`
/// filters it by the same rule.
fn nodes(root: &Command) -> Vec<Node<'_>> {
    let mut out = vec![Node {
        path: Vec::new(),
        hidden: false,
        leaf: is_leaf(root),
        command: root,
    }];
    collect(root, &mut Vec::new(), false, &mut out);
    out
}

/// A command a user runs rather than a menu they pass through.
///
/// clap's own `help` is not a menu a reader navigates into, so a node
/// whose only child is `help` still counts as one. That is defensive
/// rather than load-bearing: `build()` hangs `help` off the 19 nodes
/// that have subcommands and off none of the 78 leaves, so today the
/// filtered and the strict predicate return the same 78 either way.
fn is_leaf(cmd: &Command) -> bool {
    cmd.get_subcommands().all(|sub| sub.get_name() == "help")
}

fn collect<'a>(cmd: &'a Command, prefix: &mut Vec<String>, hidden: bool, out: &mut Vec<Node<'a>>) {
    for sub in cmd.get_subcommands().filter(|s| s.get_name() != "help") {
        let hidden = hidden || sub.is_hide_set();
        prefix.push(sub.get_name().to_string());
        out.push(Node {
            path: prefix.clone(),
            hidden,
            leaf: is_leaf(sub),
            command: sub,
        });
        collect(sub, prefix, hidden, out);
        prefix.pop();
    }
}

// ------------------------------------------------------------ examples

#[test]
fn every_visible_leaf_command_carries_a_worked_example() {
    // The property a reader feels directly: a command they can reach
    // from `--help` shows them how it is used. It rots by addition
    // rather than by edit — nobody deletes the examples, somebody adds
    // a command — which is exactly the change a diff reviewer reads as
    // complete because everything they see is right.
    let tree = built_tree();
    let all = nodes(&tree);
    let leaves: Vec<&Node<'_>> = all.iter().filter(|n| n.leaf && !n.hidden).collect();
    assert!(
        leaves.len() >= 70,
        "only {} visible leaf command(s) found — the walk is broken, and \
         a coverage assertion over almost nothing passes for the wrong \
         reason",
        leaves.len()
    );

    let bare: Vec<String> = leaves
        .iter()
        .filter(|n| examples_for(&n.key()).is_empty())
        .map(|n| n.display())
        .collect();
    assert!(
        bare.is_empty(),
        "{} visible leaf command(s) carry no worked example — add one to \
         `EXAMPLES` in cli/platform-cli/src/examples.rs, then run \
         `just docsgen-generate`:\n  {}",
        bare.len(),
        bare.join("\n  ")
    );
}

#[test]
fn no_hidden_command_carries_an_example() {
    // The inverse, and it is not symmetry for its own sake: `auth
    // login` / `logout` / `status` are hidden because AppRafter Cloud
    // is not available yet. An example would re-advertise them in
    // `--help` and in the generated reference — undoing the decision
    // from the far side, where nobody would look for it.
    let tree = built_tree();
    let all = nodes(&tree);
    let hidden: Vec<&Node<'_>> = all.iter().filter(|n| n.hidden).collect();
    assert!(
        !hidden.is_empty(),
        "no hidden command in the tree — this assertion means nothing \
         until one exists again; delete it or restore the walk"
    );

    let advertised: Vec<String> = hidden
        .iter()
        .filter(|n| !examples_for(&n.key()).is_empty())
        .map(|n| n.display())
        .collect();
    assert!(
        advertised.is_empty(),
        "hidden command(s) carry an example, which puts them back in \
         `--help` and in the reference:\n  {}",
        advertised.join("\n  ")
    );
}

#[test]
fn no_declared_example_is_blank() {
    // A blank line renders as an empty row under `Examples:` in both
    // surfaces. Nothing else rejects it: `docsgen`'s resolver reports
    // an entry it cannot read, but only per line, and a table whose
    // ENTRY carries an empty `lines` slice reads as "no examples" — a
    // command silently losing its examples while still holding a row
    // that says it has them.
    assert!(
        EXAMPLES.len() >= 70,
        "the examples table holds {} entr(ies) — too few for this to be \
         judging the shipped table",
        EXAMPLES.len()
    );

    for entry in EXAMPLES {
        let command = format!("apprafter {}", entry.path.join(" "));
        assert!(
            !entry.lines.is_empty(),
            "`{command}` has a row in the examples table and no lines in \
             it — delete the row or write the example"
        );
        for line in entry.lines {
            assert!(
                !line.trim().is_empty(),
                "`{command}` declares a blank example line"
            );
        }
    }
}

// ----------------------------------------------------------- help text

#[test]
fn every_command_still_has_about_text() {
    // `about` is the one line that appears in the parent's subcommand
    // list, in the generated reference index, and in every completion
    // script. A command added without it is invisible in three places
    // at once and looks fine in all of them — there is no rendering
    // that shows a hole.
    let tree = built_tree();
    let all = nodes(&tree);
    assert!(
        all.len() >= 90,
        "only {} command(s) walked — the tree did not build",
        all.len()
    );

    let silent: Vec<String> = all
        .iter()
        .filter(|n| {
            n.command
                .get_about()
                .map(|about| about.to_string().trim().is_empty())
                .unwrap_or(true)
        })
        .map(|n| n.display())
        .collect();
    assert!(
        silent.is_empty(),
        "command(s) with no `about` text — write a doc comment on the \
         clap variant:\n  {}",
        silent.join("\n  ")
    );
}

#[test]
fn every_flag_and_positional_still_has_help_text() {
    // The defect this whole track was opened for: a flag a user can
    // pass, with nothing telling them what it does. It reads as an
    // aligned, plausible, empty column, and it is invisible to every
    // other gate — `docsgen check` byte-compares a rendering of the
    // hole against the same hole.
    //
    // `get_help()` is the short help, and this deliberately does not
    // accept `long_help` in its place. Checked rather than assumed: a
    // flag carrying only `long_help` still renders in `-h` (clap falls
    // back to it) but reaches the generated reference as an EMPTY
    // description cell, because `docsgen::model::arg_from` reads
    // `get_help()` and `render` prints it straight into the table.
    // Using the same getter as the projection is what keeps this
    // assertion and the published page agreeing on what counts as
    // documented.
    let tree = built_tree();
    let all = nodes(&tree);

    let mut checked = 0usize;
    let mut silent = Vec::new();
    for node in &all {
        for arg in node.command.get_arguments() {
            let id = arg.get_id().as_str();
            // clap generates `--help` / `--version`; their text is
            // clap's, and the root documents them once for everybody.
            if id == "help" || id == "version" {
                continue;
            }
            checked += 1;
            let blank = arg
                .get_help()
                .map(|help| help.to_string().trim().is_empty())
                .unwrap_or(true);
            if blank {
                let kind = if arg.is_positional() {
                    "positional"
                } else {
                    "flag"
                };
                silent.push(format!("{} — {kind} `{id}`", node.display()));
            }
        }
    }

    assert!(
        checked >= 150,
        "only {checked} argument(s) walked — the tree did not build, and \
         a help-coverage assertion over almost nothing passes for the \
         wrong reason"
    );
    assert!(
        silent.is_empty(),
        "{} argument(s) with no help text — a user can pass them and \
         nothing says what they do:\n  {}",
        silent.len(),
        silent.join("\n  ")
    );
}

// -------------------------------------------------------------- aliases

/// One alias invocation a published guide types.
struct GuideAlias {
    /// Typed exactly as the guide types it, binary name excluded.
    typed: &'static [&'static str],
    /// The command it has to reach.
    canonical: &'static [&'static str],
    /// A page that types it, relative to the repository root, and the
    /// literal that page carries. Re-read at test time so this table
    /// cannot quietly become a claim about documentation that no longer
    /// says any such thing.
    page: &'static str,
    literal: &'static str,
}

/// The alias invocations the published guides type.
///
/// An alias is `alias =` in this repository, which is *hidden*: it
/// appears in no `--help` output anywhere. So dropping one is a change
/// with no visible consequence inside the CLI, and a consequence
/// entirely outside it — a shipped guide whose copy-pasteable line now
/// exits 2 at the reader's terminal.
///
/// Re-derive with (from the repository root):
///
/// ```bash
/// python3 - <<'PY'
/// import re, json, pathlib
/// d = json.loads(pathlib.Path('docs/reference/cli/commands.json').read_text())
/// kids = {}
/// for c in d['commands']:
///     p = tuple(c['path'])
///     if not p: continue
///     kids.setdefault(p[:-1], {})[p[-1]] = (p[-1], False)
///     for a in c.get('aliases') or []:
///         kids.setdefault(p[:-1], {})[a] = (p[-1], True)
/// inv = re.compile(r'\bapprafter((?:\s+[A-Za-z0-9][A-Za-z0-9._-]*)*)')
/// GUIDES = ('docs/dev-guide/', 'docs/operator-guide/', 'docs/index.md')
/// for path in sorted(pathlib.Path('docs').rglob('*.md')):
///     if not str(path).startswith(GUIDES): continue
///     for n, line in enumerate(path.read_text().splitlines(), 1):
///         for m in inv.finditer(line):
///             cur, typed, used = (), [], False
///             for tk in m.group(1).split():
///                 if tk not in kids.get(cur, {}): break
///                 canon, is_alias = kids[cur][tk]
///                 typed.append(tk); used |= is_alias; cur = cur + (canon,)
///             if used: print(f"{path}:{n}  apprafter {' '.join(typed)} -> apprafter {' '.join(cur)}")
/// PY
/// ```
///
/// # Scope, and what it leaves out on purpose
///
/// **Instructional pages only** — `docs/dev-guide/`,
/// `docs/operator-guide/`, `docs/index.md`. Three other places in the
/// corpus type an alias and are excluded, each for a reason:
///
/// * `docs/reference/cli/**` is generated from this same tree, so it
///   re-renders with the alias rather than going stale. It also lists
///   `apprafter a`, `apprafter a ls`, `apprafter backup ls`,
///   `apprafter migration ls`, `apprafter repo creds ls|rm`,
///   `apprafter t domain ls`, `apprafter t rm` and `apprafter volume
///   ls` — every alias, whether or not a human ever typed it.
/// * `docs/changelog/**` and `docs/adr/**` are records of what was
///   decided when (`apprafter a`, `apprafter t rm`, `apprafter target
///   rm` appear there and nowhere else). Retiring an alias does not
///   make a historical entry wrong, and rewriting history to keep a
///   test green would be the wrong repair.
/// * `docs/superpowers/**` is the gitignored plan tree, not published.
const GUIDE_ALIASES: &[GuideAlias] = &[
    GuideAlias {
        typed: &["app", "rm"],
        canonical: &["app", "remove"],
        page: "docs/operator-guide/shared-volumes.md",
        literal: "apprafter app rm",
    },
    GuideAlias {
        typed: &["cb"],
        canonical: &["cluster-bootstrap"],
        page: "docs/operator-guide/quickstart.md",
        literal: "apprafter cb",
    },
    GuideAlias {
        typed: &["kc"],
        canonical: &["kubeconfig"],
        page: "docs/operator-guide/quickstart.md",
        literal: "apprafter kc",
    },
    GuideAlias {
        typed: &["t", "info"],
        canonical: &["target", "show"],
        page: "docs/operator-guide/quickstart.md",
        literal: "apprafter t info",
    },
    GuideAlias {
        typed: &["t", "ls"],
        canonical: &["target", "list"],
        page: "docs/operator-guide/quickstart.md",
        literal: "apprafter t ls",
    },
    GuideAlias {
        typed: &["t", "use"],
        canonical: &["target", "use"],
        page: "docs/operator-guide/quickstart.md",
        literal: "apprafter t use",
    },
    GuideAlias {
        typed: &["up"],
        canonical: &["bootstrap-all"],
        page: "docs/dev-guide/quickstart.md",
        literal: "apprafter up",
    },
];

/// The alias tokens [`GUIDE_ALIASES`] exercises — `t`, `ls`, `info`,
/// `rm`, `kc`, `cb`, `up`.
fn alias_tokens() -> Vec<&'static str> {
    let mut tokens: Vec<&'static str> = GUIDE_ALIASES
        .iter()
        .flat_map(|entry| {
            entry
                .typed
                .iter()
                .zip(entry.canonical.iter())
                .filter(|(typed, canonical)| typed != canonical)
                .map(|(typed, _)| *typed)
        })
        .collect();
    tokens.sort_unstable();
    tokens.dedup();
    tokens
}

/// The repository root, from this crate's manifest. The crate already
/// reads out of its own directory at build time (`app_validate`
/// `include_str!`s `schemas/v1alpha1/**`), so this adds no constraint
/// that was not there.
fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn help_for(path: &[&str]) -> String {
    let output = ShippedBinary::cargo_bin("apprafter")
        .unwrap()
        .args(path)
        .arg("--help")
        .output()
        .expect("the binary runs");
    assert!(
        output.status.success(),
        "`apprafter {} --help` exited {:?} — the path does not resolve:\n{}",
        path.join(" "),
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("help is UTF-8")
}

#[test]
fn the_alias_invocations_the_guides_type_still_reach_their_command() {
    // Run the SHIPPED binary, not the tree: an alias has to survive
    // parsing and dispatch, and a tree-only assertion would pass on a
    // build where `run()` never sees it.
    assert!(
        alias_tokens().len() >= 5,
        "only {:?} exercised — the plan measured `t`, `kc`, `up`, `ls` \
         and `rm` in the guides, so a table below that has been emptied \
         rather than re-derived",
        alias_tokens()
    );

    for entry in GUIDE_ALIASES {
        let typed = help_for(entry.typed);
        let canonical = help_for(entry.canonical);
        // Non-vacuity: prove BOTH renderings are the command in
        // question before comparing them, so two identical nothings
        // cannot satisfy the equality below.
        let usage = format!("Usage: apprafter {}", entry.canonical.join(" "));
        for (label, help) in [("aliased", &typed), ("canonical", &canonical)] {
            assert!(
                help.contains(&usage),
                "the {label} rendering of `apprafter {}` does not carry \
                 `{usage}`:\n{help}",
                entry.typed.join(" ")
            );
        }
        assert_eq!(
            typed,
            canonical,
            "`apprafter {}` and `apprafter {}` render different help, so \
             the alias no longer reaches the command {} documents",
            entry.typed.join(" "),
            entry.canonical.join(" "),
            entry.page
        );
    }
}

#[test]
fn the_guides_still_type_every_alias_this_table_claims() {
    // The table above is an assertion about published documentation.
    // Left unchecked it decays into folklore — rows for lines a guide
    // stopped carrying, protecting nothing while reading as coverage.
    // This is also what keeps the other direction honest: a row can be
    // added only if a page really types it.
    let root = repo_root();
    assert!(
        root.join("docs/operator-guide").is_dir(),
        "no docs/ tree at {} — this test judges published documentation \
         and cannot silently skip it",
        root.display()
    );

    for entry in GUIDE_ALIASES {
        let page = root.join(entry.page);
        let text = std::fs::read_to_string(&page).unwrap_or_else(|e| {
            panic!(
                "{} is cited as typing `{}` and could not be read: {e}",
                entry.page, entry.literal
            )
        });
        assert!(
            text.contains(entry.literal),
            "{} no longer types `{}` — re-derive this table with the \
             command on `GUIDE_ALIASES` rather than deleting the row on \
             a guess",
            entry.page,
            entry.literal
        );
    }
}
