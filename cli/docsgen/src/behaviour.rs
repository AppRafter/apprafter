// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Sentences that stopped being true, and the code fact that decides.
//!
//! Every other check in this gate resolves a **name**: a command path, a
//! flag, a field, a file, an ADR number. That is deliberate and it is
//! also the blind spot — the class that costs most is a passage whose
//! every noun resolves and whose verb is false. `backup-restore.md`
//! documented two fixed defects as current behaviour for a week while
//! `docsgen gate` reported green, because `apprafter target machine` was
//! still a command, `backup.rs` was still a file, and `run_backup_check`
//! was still a function.
//!
//! # Why this is not a forbidden-phrase list
//!
//! ADR 0057 decision 7 specified a curated list of strings encoding
//! removed behaviour. That shape has one fatal property: it asserts,
//! and an assertion rots. A phrase forbidden in 2026 is silently wrong
//! the day someone reverts the fix that made it wrong, and nothing in
//! the list notices.
//!
//! So each entry here **derives its force from the tree**. The phrase is
//! forbidden only while a named code fact says it is false; revert the
//! fix and the fact flips, the phrase becomes true again, and the check
//! stops objecting — correctly, without anyone editing this file. The
//! table records which sentence to watch and where the answer lives; the
//! repository decides the answer.
//!
//! This is [`crate::shipped`] one level up. That module classifies a
//! `needs` key by whether a provisioner arm and a seeded CR exist; this
//! one classifies a *sentence* by whether the code it describes still
//! behaves that way. Same three parts: a flat table carrying its own
//! evidence, a test derived from the tree rather than from the table,
//! and loud failure rather than a vacuous pass.
//!
//! # What it cannot do
//!
//! It watches sentences somebody thought to add. A behavioural claim
//! nobody has burned on yet is not in the table and is not checked.
//! That is the honest scope: this closes the *recurrence* of a known
//! class, which is exactly what ADR 0057 asked decision 7 to do, and it
//! does not pretend to be a general truth-checker.

use std::error::Error;
use std::path::Path;

/// What the evidence anchor must be for the phrase to be TRUE.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Expect {
    /// The phrase is true while the anchor is PRESENT in the file.
    Present,
    /// The phrase is true while the anchor is ABSENT from the file.
    Absent,
}

/// One sentence worth watching, and the fact that decides it.
#[derive(Debug, Clone, Copy)]
pub struct Claim {
    /// The phrase as a page would carry it, lowercased for matching.
    /// Short enough to be stable across rewording, long enough not to
    /// fire on an unrelated sentence.
    pub phrase: &'static str,
    /// Repo-relative file holding the deciding fact.
    pub evidence: &'static str,
    /// The string to look for in it.
    pub anchor: &'static str,
    /// Whether the anchor's presence makes the phrase true or false.
    pub truth: Expect,
    /// The incident. Every entry earned its place; this says how, so a
    /// later reader can judge whether it is still worth carrying.
    pub because: &'static str,
    /// Whether the phrase is TRUE of the tree as this entry was last
    /// reviewed.
    ///
    /// Most entries are `false` — a page said something the fix had
    /// already made untrue. An entry flips to `true` when its fix lands,
    /// and stays in the table to catch a revert.
    ///
    /// Recorded rather than assumed because assuming it cost us: the
    /// original GC entry anchored on the `resourceclaims` verb list
    /// *without* `delete`, reasoning that "no implementation can delete a
    /// claim without this rule changing first". 2.22b appended `- delete`
    /// to that rule and the anchor, being a prefix of the new list, went
    /// on matching — so the claim kept reading false while the behaviour
    /// had become true, and the invariant test could not see it because a
    /// rotted anchor reads exactly like an unfixed defect.
    pub holds_today: bool,
}

/// The watched sentences.
///
/// Adding one is cheap and should stay cheap: when a fix lands for a
/// behaviour a page described, the entry is how that page is stopped
/// from describing it again.
pub const CLAIMS: &[Claim] = &[
    Claim {
        phrase: "resolve a kubeconfig as their first statement",
        evidence: "cli/platform-cli/src/commands/backup.rs",
        anchor: "kubeconfig_if_cluster_needed",
        truth: Expect::Absent,
        because: "backup check/prune/unlock demanded a live cluster until \
                  f7ee105 made the kubeconfig lazy. backup-restore.md kept \
                  describing the old behaviour for a week, and every name in \
                  the passage still resolved.",
        holds_today: false,
    },
    Claim {
        phrase: "keeps reporting the old machine",
        evidence: "cli/platform-cli/src/commands/apply.rs",
        anchor: "fn adopt_provisioned_machine",
        truth: Expect::Absent,
        because: "the target's saved machine went stale after a \
                  restore --reprovision until e94fb4c adopted the live facts \
                  on the create path. Same page, same week, same silence.",
        holds_today: false,
    },
    Claim {
        phrase: "rollout restart ds/cilium",
        evidence: "platform-stack/cue/component_cilium.cue",
        anchor: "rollOutCiliumPods: true",
        truth: Expect::Absent,
        because: "troubleshooting.md told operators to roll the DaemonSet by \
                  hand after a chart bump long after the chart began stamping \
                  a config checksum that rolls it automatically.",
        holds_today: false,
    },
    Claim {
        phrase: "redis contents are not captured",
        evidence: "cli/backup-core/src/extract.rs",
        anchor: "\"redis\" =>",
        truth: Expect::Absent,
        because: "export and restore help both claimed redis is never \
                  captured, false since cli v0.2.51 shipped persistent-redis \
                  snapshots — and the generated reference republished the \
                  claim verbatim from the doc comment.",
        holds_today: false,
    },
    Claim {
        phrase: "the pinned k8s v1.35",
        evidence: "cli/cli-providers/src/hetzner_cloud/user_data.rs",
        anchor: "INSTALL_K3S_VERSION",
        // PRESENT, not Absent: this phrase becomes TRUE the day someone
        // actually pins the version, and the entry retires itself. That
        // is the property this table exists for — the sentence is not
        // forbidden, it is unsupported, and the tree decides which.
        truth: Expect::Present,
        because: "ADR 0054 rests its whole in-place premise on a pinned \
                  Kubernetes, and nothing pins one: build_k3s_user_data \
                  installs stable-channel k3s with no INSTALL_K3S_VERSION, \
                  which quickstart.md states outright. Benign today only \
                  because the gate is on by default at the versions the \
                  channel serves — an upstream default, not something the \
                  platform arranges or would notice changing. D10.",
        holds_today: false,
    },
    Claim {
        phrase: "backing claim and its data are garbage-collected",
        evidence: "operator/operator-controllers/application/src/lib.rs",
        // Anchored on the FUNCTION that does the deleting, not on the RBAC
        // that permits it. The first version anchored on the resourceclaims
        // verb list without `delete`, on the reasoning that no code could
        // delete a claim without that rule changing — true, but the anchor
        // was a PREFIX of the list, so 2.22b appended `- delete` and the
        // anchor went on matching. Anchoring on the behaviour cannot rot
        // that way: the phrase is true exactly while something deletes.
        anchor: "async fn prune_orphaned_claims",
        truth: Expect::Present,
        because: "four documents said removing a needs.* entry garbage-collects \
                  the claim — ADR 0051 twice, spec.md and migration-plans.md — \
                  while nothing deleted it and the operator's RBAC carried no \
                  delete verb at all. Fixed in 2.22b (D4); the entry stays to \
                  catch a revert, which would make every one of those four \
                  documents wrong again.",
        holds_today: true,
    },
];

/// Whether a claim's phrase is true of the tree right now.
///
/// `Err` when the evidence file cannot be read. That is deliberately not
/// "assume the phrase is fine": a moved or renamed file is exactly how a
/// check like this goes quiet, so it fails loudly and names the path.
pub fn holds(repo_root: &Path, claim: &Claim) -> Result<bool, Box<dyn Error>> {
    let path = repo_root.join(claim.evidence);
    let text = std::fs::read_to_string(&path).map_err(|e| {
        format!(
            "behaviour-claim `{}` cannot be judged: its evidence file {} is \
             unreadable ({e}). The check is not passing — it is broken, and a \
             moved file is how it would go quiet.",
            claim.phrase, claim.evidence
        )
    })?;
    let found = text.contains(claim.anchor);
    Ok(match claim.truth {
        Expect::Present => found,
        Expect::Absent => !found,
    })
}

/// Every watched phrase that a page carries while the tree says it is
/// false, as `(claim, 1-based line)`.
///
/// Matching is case-insensitive and ignores nothing else: the phrases
/// are chosen to be the distinctive part of the sentence, so a page that
/// rewords around one has changed the claim rather than evaded the
/// check.
pub fn falsified<'a>(
    repo_root: &Path,
    source: &str,
    claims: &'a [Claim],
) -> Result<Vec<(&'a Claim, usize)>, Box<dyn Error>> {
    let lower = source.to_lowercase();
    let mut out = Vec::new();
    for claim in claims {
        if !lower.contains(claim.phrase) {
            continue;
        }
        if holds(repo_root, claim)? {
            continue;
        }
        let line = lower
            .lines()
            .position(|l| l.contains(claim.phrase))
            .map_or(1, |i| i + 1);
        out.push((claim, line));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root() -> std::path::PathBuf {
        let mut dir = std::env::current_dir().expect("cwd");
        loop {
            if dir.join("cue.mod/module.cue").exists() {
                return dir;
            }
            assert!(dir.pop(), "no cue.mod/module.cue above the test's cwd");
        }
    }

    #[test]
    fn every_claim_names_a_readable_evidence_file() {
        // The failure this guards is the silent one: a file renamed out
        // from under an entry turns the whole check into a no-op, and
        // nothing else in the suite would notice.
        for claim in CLAIMS {
            holds(&root(), claim).unwrap_or_else(|e| panic!("{}: {e}", claim.phrase));
        }
    }

    #[test]
    fn every_claim_matches_its_recorded_state() {
        // Replaces an assertion that every claim is false today. That held
        // while every entry was an unfixed defect, and it hid the failure
        // it was meant to catch: when 2.22b fixed the GC defect, its
        // anchor did not flip (it was a prefix of the rule it watched), so
        // the claim kept reading false and a blanket "all false" passed.
        //
        // Recording each entry's state makes both directions loud: a fix
        // landing without its entry being reviewed fails here, and so does
        // a revert.
        let root = root();
        for claim in CLAIMS {
            let actual = holds(&root, claim).expect("evidence readable");
            assert_eq!(
                actual, claim.holds_today,
                "`{}` is now {} of the tree but the table records {}. Either the \
                 behaviour changed and this entry needs reviewing, or `{}` stopped \
                 deciding it. Both need a human.",
                claim.phrase, actual, claim.holds_today, claim.anchor
            );
        }
    }

    #[test]
    fn every_claim_states_why_it_is_watched() {
        for claim in CLAIMS {
            assert!(
                claim.because.len() > 40,
                "`{}` needs the incident that earned it, not a label",
                claim.phrase
            );
            assert!(
                claim.phrase == claim.phrase.to_lowercase(),
                "`{}` must be lowercase — matching is case-insensitive via \
                 the source, not the table",
                claim.phrase
            );
        }
    }

    #[test]
    fn a_page_carrying_a_falsified_phrase_is_reported() {
        let root = root();
        let page = "The three verbs resolve a kubeconfig as their first statement.\n";
        let found = falsified(&root, page, CLAIMS).expect("judged");
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].1, 1);
    }

    #[test]
    fn a_page_saying_nothing_watched_is_silent() {
        let root = root();
        let page = "Declare the dependency and the platform provisions it.\n";
        assert!(falsified(&root, page, CLAIMS).expect("judged").is_empty());
    }

    #[test]
    fn a_phrase_whose_fact_still_holds_is_not_reported() {
        // The property that stops this becoming a rotting deny-list: an
        // entry whose code fact says the phrase is TRUE reports nothing,
        // whatever the table says.
        let root = root();
        let alive = Claim {
            phrase: "the operator reconciles the application",
            evidence: "cli/docsgen/src/behaviour.rs",
            anchor: "pub const CLAIMS",
            truth: Expect::Present,
            because: "a fixture: its anchor is this table, so the phrase is \
                      true for as long as this module exists.",
            holds_today: true,
        };
        let page = "The operator reconciles the application on every change.\n";
        assert!(falsified(&root, page, &[alive]).expect("judged").is_empty());
    }
}
