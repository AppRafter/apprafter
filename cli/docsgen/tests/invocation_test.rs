// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Every case here is a real line from the corpus. The `resolves`
//! cases are correct documentation that a naive resolver rejects; the
//! `rejects` cases are drift that exists in the tree today.

use docsgen::invocation::{extract, logical_lines, resolve, Tree};
use docsgen::model;
use docsgen::scan::{in_scope, scan_markdown, BlockKind};
use std::collections::{HashMap, HashSet};

fn commands_json() -> std::path::PathBuf {
    // `repo_root` is fallible (it walks up for `cue.mod/module.cue`),
    // so the plan's `repo_root().join(…)` needs the unwrap.
    docsgen::repo_root()
        .unwrap()
        .join("docs/reference/cli/commands.json")
}

fn tree() -> Tree {
    Tree::from_commands_json(&commands_json()).unwrap()
}

fn model_tree() -> model::Tree {
    serde_json::from_str(&std::fs::read_to_string(commands_json()).unwrap()).unwrap()
}

// ---- extraction -------------------------------------------------------

#[test]
fn an_invocation_must_start_a_command() {
    // `apprafter` appearing as a filename or a label is not an invocation.
    assert!(extract("sudo mv apprafter /usr/local/bin/").is_empty());
    assert!(extract("cargo run --bin apprafter -- app list").is_empty());
    assert!(extract("the `apprafter.io/hostname` label").is_empty());
    assert_eq!(extract("apprafter app list").len(), 1);
    assert_eq!(extract("$ apprafter app list").len(), 1);
    assert_eq!(extract("KUBECONFIG=$(apprafter kubeconfig)").len(), 1);
    assert_eq!(extract("apprafter apply && apprafter kubeconfig").len(), 2);
}

#[test]
fn a_substitution_is_reached_through_a_pipeline_and_reported_in_order() {
    // Substitutions are unwrapped before the line is split, so nesting
    // one inside a pipeline stage does not hide it — and the results
    // come back in source order, not outer-then-inner.
    let found = extract("apprafter status && kubectl apply -f $(apprafter export) | tee /tmp/l");
    assert_eq!(found.len(), 2, "{found:?}");
    assert_eq!(found[0].path, ["status"]);
    assert_eq!(found[1].path, ["export"]);
}

#[test]
fn a_comment_is_prose_not_an_invocation() {
    // `# alias: apprafter bootstrap-all` documents a name; it is not run.
    assert!(extract("# alias: apprafter bootstrap-all").is_empty());
}

// ---- resolution: correct docs that must stay green ---------------------

#[test]
fn synopsis_notation_resolves() {
    let t = tree();
    for line in [
        "apprafter backup create [--namespace <ns> ...]",
        "apprafter backup enable [--keep-daily N]",
        "apprafter restore <repo> [--reprovision | --data-only]",
    ] {
        for inv in extract(line) {
            resolve(&t, &inv).unwrap_or_else(|e| panic!("{line} must resolve: {e}"));
        }
    }
}

#[test]
fn aliases_resolve() {
    let t = tree();
    for line in [
        "apprafter up",
        "apprafter t ls",
        "apprafter cb",
        "apprafter kc --refresh",
    ] {
        for inv in extract(line) {
            resolve(&t, &inv).unwrap_or_else(|e| panic!("{line} must resolve: {e}"));
        }
    }
}

#[test]
fn global_flags_and_hidden_commands_resolve() {
    let t = tree();
    for line in [
        "apprafter --version",
        "apprafter app add --help",
        "apprafter auth status",
    ] {
        for inv in extract(line) {
            resolve(&t, &inv).unwrap_or_else(|e| panic!("{line} must resolve: {e}"));
        }
    }
}

#[test]
fn a_bare_command_path_resolves_without_its_required_arguments() {
    // 161 of 213 inline spans are bare paths; demanding arguments would
    // fail every one, including inside docsgen's own generated output.
    let t = tree();
    for inv in extract("apprafter secret seal") {
        resolve(&t, &inv).expect("a bare path is a reference, not an invocation to complete");
    }
}

#[test]
fn a_command_family_named_in_one_span_resolves() {
    // Four pages introduce a family this way. Every alternative is
    // checked, so the notation costs the gate nothing.
    let t = tree();
    for line in [
        // docs/operator-guide/needs-{disk,pg,redis}-walk.md
        "apprafter repo creds add/list/show",
        // docs/operator-guide/migration-plans.md
        "apprafter migration list/approve/reject",
        // docs/dev-guide/quickstart.md — the `app` family as a whole.
        "apprafter app *",
    ] {
        for inv in extract(line) {
            resolve(&t, &inv).unwrap_or_else(|e| panic!("{line} must resolve: {e}"));
        }
    }
    // …and one bad alternative still fails, by name.
    let inv = extract("apprafter repo creds add/frobnicate")
        .into_iter()
        .next()
        .unwrap();
    let err = resolve(&t, &inv).unwrap_err();
    assert!(err.to_string().contains("frobnicate"), "{err}");
}

#[test]
fn a_substitutions_flags_stay_with_the_substituted_command() {
    // docs/operator-guide/troubleshooting.md: `-c` belongs to `python`,
    // not to `apprafter target add`, which has never had it.
    let t = tree();
    let line = r#"apprafter target add bad --token "$(python -c 'print("a"*64)')" …"#;
    let found = extract(line);
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].flags, ["--token"]);
    // The report still shows what the page says, substitution included.
    assert!(found[0].raw.contains("python"), "{}", found[0].raw);
    resolve(&t, &found[0]).unwrap();
}

// ---- resolution: drift that exists in the tree today -------------------

#[test]
fn an_unknown_command_is_rejected() {
    let t = tree();
    let inv = extract("apprafter promote").into_iter().next().unwrap();
    let err = resolve(&t, &inv).unwrap_err();
    assert!(err.to_string().contains("promote"), "{err}");
}

#[test]
fn a_flag_from_a_sibling_command_is_rejected() {
    // `--namespace` exists on 12 commands. `app status` is not one.
    let t = tree();
    let inv = extract("apprafter app status writer --namespace apps")
        .into_iter()
        .next()
        .unwrap();
    let err = resolve(&t, &inv).unwrap_err();
    assert!(err.to_string().contains("--namespace"), "{err}");
}

// ---- the two tree invariants the resolver's soundness rests on -------

#[test]
fn an_alias_is_unique_among_its_siblings() {
    // Aliases are indexed per parent because they are unique only
    // there: `ls` names seven different commands and `rm` three. What
    // must never happen is two SIBLINGS sharing a token — that would
    // make resolution pick one of two meanings silently.
    let model = model_tree();
    let mut seen: HashMap<(Vec<String>, String), String> = HashMap::new();
    for node in &model.commands {
        let Some((name, parent)) = node.path.split_last() else {
            continue;
        };
        for token in std::iter::once(name).chain(&node.aliases) {
            let claimed = seen.insert((parent.to_vec(), token.clone()), name.clone());
            assert!(
                claimed.is_none(),
                "`{token}` names both `{}` and `{name}` under `{}`",
                claimed.unwrap(),
                parent.join(" ")
            );
        }
    }
}

#[test]
fn every_branch_node_is_a_pure_branch() {
    // `resolve` treats an unmatched token at a node WITH subcommands as
    // an unknown command, and at a leaf as a positional value. That
    // split is only sound while no node is both — the day one is, this
    // test fails instead of the docs.
    let model = model_tree();
    let parents: HashSet<&[String]> = model
        .commands
        .iter()
        .filter_map(|node| node.path.split_last().map(|(_, parent)| parent))
        .collect();
    for node in &model.commands {
        if parents.contains(node.path.as_slice()) {
            assert!(
                node.positionals.is_empty(),
                "`apprafter {}` has both subcommands and positionals",
                node.path.join(" ")
            );
        }
    }
}

// ---- the whole corpus, as a report ------------------------------------

/// Resolve every invocation in the corpus and print the failures.
///
/// Ignored by default: this is the *input* to fixing the pages, not a
/// gate — the gate is assembled in a later package, once the pages it
/// would fail have been dealt with.
#[test]
#[ignore = "reports over the whole corpus; run with --ignored --nocapture"]
fn corpus_resolution() {
    let root = docsgen::repo_root().unwrap();
    let t = tree();
    let mut files = 0usize;
    let mut total = 0usize;
    let mut failures = Vec::new();

    for path in in_scope(&root).unwrap() {
        files += 1;
        let shown = path
            .strip_prefix(&root)
            .unwrap_or(&path)
            .display()
            .to_string();
        let src = std::fs::read_to_string(&path).unwrap();
        for block in scan_markdown(&src) {
            // Front matter is not prose and holds no claims.
            let first = match block.kind {
                BlockKind::FrontMatter => continue,
                // A fence's body starts on the line after its opener.
                BlockKind::Fence { .. } => block.line + 1,
                BlockKind::InlineSpan => block.line,
            };
            for (offset, text) in logical_lines(&block.body) {
                for invocation in extract(&text) {
                    total += 1;
                    if let Err(e) = resolve(&t, &invocation) {
                        failures.push(format!(
                            "{shown}:{}  {}  — {e}",
                            first + offset,
                            invocation.raw
                        ));
                    }
                }
            }
        }
    }

    println!(
        "\ncorpus: {files} files, {total} invocations, {} failing",
        failures.len()
    );
    for line in &failures {
        println!("{line}");
    }
}
