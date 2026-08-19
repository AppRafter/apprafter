// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Resolve every documented example against the clap tree.
//!
//! # The blind spot this closes
//!
//! An example is a *claim about the CLI*, written in the CLI's own
//! source and rendered into `docs/reference/cli/**`. That tree sits
//! between this repository's two documentation gates and is judged by
//! neither:
//!
//! * [`crate::check`] **byte-compares** the tree against a fresh
//!   render. It proves the pages match the source — not that the source
//!   is true. An example naming a misspelled flag renders identically on
//!   both sides and passes.
//! * [`crate::gate`] resolves `apprafter …` invocations against the clap
//!   tree, but [`crate::scan`] puts `docs/reference/cli/**` out of scope
//!   (spelled at its source in `is_in_scope`, against `render::DIR`,
//!   because `check` owns that tree). So the resolver never reads an
//!   example.
//!
//! Each gate derives its expectation from the same side it checks.
//! Writing examples into that gap would put unverified invocations in
//! the one artefact whose whole purpose is to be true — so this module
//! ships **before** the examples do, and `docsgen check` runs it as
//! assertion C.
//!
//! # What is checked
//!
//! The command path and the flag names, exactly as [`crate::gate`]
//! checks them for hand-written prose, through the *same* resolver:
//! [`invocation::resolve_to`]. Values and required-ness are **not**
//! checked — the settled rule from 2.19c, for the reason recorded in
//! [`crate::invocation`]: a documented line is a reference to a command,
//! not a runnable one. Reusing that resolver rather than writing a
//! second one is deliberate; two resolvers in one gate eventually
//! disagree.
//!
//! One thing is checked here that the prose gate has no reason to: an
//! example must invoke **the command it is filed under**. `apprafter app
//! list` is a perfectly true invocation and a false page when it appears
//! under `apprafter backup enable`, and copying a sibling's example is
//! the likeliest way to author one wrong.
//!
//! # Nothing may go unread
//!
//! An entry that yields no invocation is a **finding**, not a skip.
//! That is the difference between a guard and a guard-shaped no-op: a
//! misspelled binary (`aprafter app list`), a stray prose line or an
//! empty string would otherwise pass by being unreadable, and the report
//! would still say OK.

use crate::invocation;
use crate::model;
use std::error::Error;
use std::fmt;

/// One example that does not hold up, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    /// The command path the example is filed under, `[]` for the root.
    pub command: Vec<String>,
    /// The example verbatim, so the reporter can be grepped for it.
    pub example: String,
    /// The defect, phrased so the offending token appears in it.
    pub problem: String,
}

impl fmt::Display for Finding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "`apprafter{}{}` example {:?}: {}",
            if self.command.is_empty() { "" } else { " " },
            self.command.join(" "),
            self.example,
            self.problem
        )
    }
}

/// Judge every example in `tree` against the clap tree it was projected
/// from. An empty result means every example resolves.
///
/// The error case is the projection being unindexable (a hand-edited
/// `commands.json` with no root node), never a finding: the two are
/// different work, and a caller that conflates them reports a broken
/// gate as a broken document.
pub fn check(tree: &model::Tree) -> Result<Vec<Finding>, Box<dyn Error>> {
    let index = invocation::Tree::from_model(tree.clone())?;
    let mut findings = Vec::new();
    for node in &tree.commands {
        for example in &node.examples {
            findings.extend(judge(&index, &node.path, example));
        }
    }
    Ok(findings)
}

/// Every defect in one example. All of them, not the first: an author
/// fixing a flag should not have to run the gate again to discover the
/// example was also filed under the wrong command.
fn judge(index: &invocation::Tree, command: &[String], example: &str) -> Vec<Finding> {
    let at = |problem: String| Finding {
        command: command.to_vec(),
        example: example.to_string(),
        problem,
    };

    if example.trim().is_empty() {
        return vec![at("it is empty".into())];
    }
    if example.contains('\n') {
        // The renderer emits one array entry per fence line, and
        // `invocation::extract` reads one line. A two-line entry would
        // be tokenised as a single command with the second line's flags
        // charged to the first — judged, wrongly, and green.
        return vec![at(
            "it spans more than one line — an entry is one command line; \
             use two entries"
                .into(),
        )];
    }

    let invocations = invocation::extract(example);
    if invocations.is_empty() {
        return vec![at(
            "it names no `apprafter` invocation, so nothing in it can be \
             checked — an example must open with `apprafter`"
                .into(),
        )];
    }

    let mut findings = Vec::new();
    let mut documents_this_command = false;
    for one in &invocations {
        match invocation::resolve_to(index, one) {
            Ok(paths) => {
                documents_this_command |= paths.iter().any(|path| path.starts_with(command));
            }
            Err(e) => findings.push(at(e.to_string())),
        }
    }
    if !documents_this_command && findings.is_empty() {
        // Suppressed when something already failed to resolve: an
        // unresolvable example lands nowhere, so "it documents X
        // instead" would be a guess dressed as a second defect.
        findings.push(at(format!(
            "it does not invoke the command it is filed under — every \
             `apprafter` command in it resolves elsewhere ({})",
            invocations
                .iter()
                .map(|one| format!("`{}`", one.raw))
                .collect::<Vec<_>>()
                .join(", ")
        )));
    }
    findings
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The real clap tree, with `examples` replaced by `entries` on
    /// `path`. Judging against the REAL tree is the point: a fabricated
    /// two-command fixture would let a wrong example pass for want of
    /// the flag it names.
    fn judged(path: &[&str], entries: &[&str]) -> Vec<Finding> {
        use clap::CommandFactory;
        let mut tree = model::tree_from(&apprafter::docs_api::Cli::command(), "test");
        let node = tree
            .commands
            .iter_mut()
            .find(|c| c.path == path)
            .unwrap_or_else(|| panic!("no command {path:?}"));
        node.examples = entries.iter().map(|e| (*e).to_string()).collect();
        check(&tree).expect("the real projection indexes")
    }

    fn only(findings: &[Finding]) -> &str {
        assert_eq!(findings.len(), 1, "{findings:?}");
        &findings[0].problem
    }

    #[test]
    fn a_correct_example_resolves() {
        assert!(judged(
            &["secret", "seal"],
            &["apprafter secret seal db-url --namespace web"]
        )
        .is_empty());
    }

    #[test]
    fn a_misspelled_flag_is_named() {
        let f = judged(
            &["secret", "seal"],
            &["apprafter secret seal db-url --namesapce web"],
        );
        assert!(only(&f).contains("--namesapce"), "{f:?}");
        assert!(only(&f).contains("secret seal"), "{f:?}");
    }

    #[test]
    fn a_real_flag_on_the_wrong_command_is_named() {
        // `--namespace` is real on twelve commands and absent on
        // `app status` — the sibling-borrow shape a reviewer misses.
        let f = judged(
            &["app", "status"],
            &["apprafter app status web --namespace web"],
        );
        assert!(only(&f).contains("--namespace"), "{f:?}");
        assert!(only(&f).contains("app status"), "{f:?}");
    }

    #[test]
    fn an_unknown_command_is_named() {
        let f = judged(&["app"], &["apprafter app promote web"]);
        assert!(only(&f).contains("promote"), "{f:?}");
    }

    #[test]
    fn an_example_filed_under_the_wrong_command_is_caught() {
        // True invocation, false page. Neither existing gate sees this.
        let f = judged(&["backup", "enable"], &["apprafter app list"]);
        assert!(only(&f).contains("does not invoke the command"), "{f:?}");
        assert!(only(&f).contains("apprafter app list"), "{f:?}");
    }

    #[test]
    fn a_group_node_may_show_one_of_its_children() {
        assert!(judged(&["target"], &["apprafter target use prod"]).is_empty());
        // ...and an alias resolves to the canonical path, so the
        // filed-under check does not reject the spelling the docs use.
        assert!(judged(&["target", "use"], &["apprafter t use prod"]).is_empty());
    }

    #[test]
    fn a_child_may_not_show_its_parent() {
        // `apprafter target` under `target use` documents the group,
        // not the command whose page it appears on.
        let f = judged(&["target", "use"], &["apprafter target"]);
        assert!(only(&f).contains("does not invoke the command"), "{f:?}");
    }

    #[test]
    fn an_entry_that_reads_as_nothing_is_a_finding_not_a_skip() {
        // Each of these yields zero invocations. If "no invocation"
        // meant "nothing to check", the guard would report OK on all
        // three — the exact shape of a gate that grades its own
        // homework.
        for entry in [
            "aprafter app list",
            "kubectl get pods",
            "see the guide for how to list apps",
        ] {
            let f = judged(&["app", "list"], &[entry]);
            assert!(
                only(&f).contains("names no `apprafter` invocation"),
                "{entry}: {f:?}"
            );
        }
    }

    #[test]
    fn an_empty_entry_is_a_finding() {
        assert!(only(&judged(&["app", "list"], &["   "])).contains("empty"));
    }

    #[test]
    fn a_multi_line_entry_is_refused_rather_than_misread() {
        // Tokenised as one line this reads as `app list --namespace`,
        // charging the second command's flag to the first.
        let f = judged(
            &["app", "list"],
            &["apprafter app list\napprafter app add --env prod"],
        );
        assert!(only(&f).contains("more than one line"), "{f:?}");
    }

    #[test]
    fn a_setup_line_before_the_command_is_allowed() {
        // A setup command sharing the line: both invocations are
        // checked and one of them is this command's. The shape is
        // exercised for the LEXER, not endorsed — this particular
        // line is the C3 defect (`KUBECONFIG` takes a path list, not
        // a document), and no shipped example writes it any more.
        assert!(judged(
            &["app", "list"],
            &["export KUBECONFIG=\"$(apprafter kubeconfig)\" && apprafter app list"]
        )
        .is_empty());
    }

    #[test]
    fn every_defect_in_one_entry_is_reported_together() {
        let f = judged(
            &["backup", "enable"],
            &["apprafter app status --namespace web"],
        );
        // The flag is wrong AND it is filed under the wrong command;
        // the misfiling is suppressed because the example resolves
        // nowhere, so exactly one actionable line comes back.
        assert_eq!(f.len(), 1, "{f:?}");
        assert!(f[0].problem.contains("--namespace"), "{f:?}");
    }

    #[test]
    fn a_finding_prints_the_command_and_the_example() {
        let f = judged(
            &["app", "status"],
            &["apprafter app status --namespace web"],
        );
        let shown = f[0].to_string();
        assert!(shown.contains("`apprafter app status`"), "{shown}");
        assert!(shown.contains("--namespace"), "{shown}");
    }
}
