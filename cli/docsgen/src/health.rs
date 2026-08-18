// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The committed corpus census, and the rule that compares it.
//!
//! [`crate::gate::Stats`] measures the documentation on every run and
//! prints one line. That line is a fact about the checkout it was
//! printed in and nothing else: a run that finds 20 pages where the
//! last one found 33 prints its 20 as calmly as its predecessor printed
//! 33, and passes. This module commits the numbers so that the *next*
//! run has something to disagree with.
//!
//! # Compared by value, never by bytes
//!
//! An earlier design byte-compared this file, as
//! [`crate::check`] byte-compares the generated CLI reference. Three
//! separate things make that wrong here, and each is worth stating
//! because "just diff the file" is the obvious idea a later reader will
//! have again:
//!
//! * **The interesting numbers are functions of `now`.** That design's
//!   own field list carried an exemption's age in days and a staleness
//!   window. A byte-compared file holding any of them goes red the day
//!   after it is committed, with no commit in between — which is the
//!   rule [`crate::render`] already keeps for the generated pages (a
//!   timestamp would turn unrelated commits into CI failures), applied
//!   to the one file that would have broken it.
//! * **It would move the comparison into the wrong command.**
//!   `docsgen check` is a pure walk of the clap tree: no `git`, no
//!   tags, no `cue`. Byte-comparing a census means recomputing it,
//!   which drags all three into a command that has none of them today.
//! * **It would collapse the exit-code split.** `gate` answers 1 for
//!   "the documentation is wrong" and 2 for "the gate itself broke";
//!   `check` answers 0/1. A census that fails through `check` has one
//!   code for both, and a caller that cannot tell them apart eventually
//!   treats both as "the docs are wrong again".
//!
//! So: seven integers, compared by value, with the file's exact bytes
//! nobody's business but a reviewer's.
//!
//! # Why these seven, and what growth means
//!
//! Every field here is an **obligation count** — a number that falls
//! when documented surface is deleted. That is the whole selection
//! rule, and it came from watching three earlier candidates fail it:
//!
//! * `blocks_executed` **cannot be implemented.** The marker grammar's
//!   `run=` key accepts only `local` ([`crate::marker`]), and
//!   [`crate::gate`] reports `run=local` as a finding because nothing
//!   executes a documented block. The number is 0, cannot rise, and a
//!   "may not decrease" rule over it reads `0 >= 0` forever: coverage
//!   on paper.
//! * `check_none` **may not increase** prohibits rather than ratchets.
//!   At its live value of 0 it is a ban on the typed, dated, expiring
//!   exemption channel `docs/contributing/documentation-gate.md`
//!   documents — a channel nobody may use is a channel we should not
//!   have built.
//! * `unlabelled_fences must be 0` was, until [`crate::scan`] grew
//!   [`crate::scan::BlockKind::Literal`], evadable by indenting the
//!   block four spaces or wrapping it in `<pre>`: it rendered as code
//!   and no check could see it. That hole is closed and the number is
//!   meaningful now — but it is still not an obligation count, because
//!   **deleting a page cannot move it**, and deletion is what this file
//!   exists to notice.
//!
//! That last point is the one to keep. Against all three rejected
//! ratchets, deleting a page was free and invisible. Against these
//! seven it is not.
//!
//! Growth passes silently and deliberately. A content phase adds
//! guides — 2.19i adds roughly fifteen — and each one adds
//! invocations, identifiers, paths and citations. **Shrinkage is the
//! signal**; a gate that made a contributor re-record the census in
//! order to write a page is a gate contributors route around.
//!
//! # Equality for `exemptions`, a ratchet for the rest
//!
//! `exemptions` is compared for **equality**, so a new one and a
//! retired one are both findings. That is the same prohibition shape
//! that was wrong for `check_none`, and the difference is the escape:
//! `check_none` at 0 had no documented way out at all, whereas moving
//! this number means editing one line of a committed file **in the same
//! commit** — which is exactly the review moment a new exemption should
//! get, and one diff line is a fair price for retiring one.
//!
//! # Reported, not committed
//!
//! Three numbers [`crate::gate::Stats::line`] prints stay out of the
//! file: the **opaque** share of resolved identifiers, and the
//! exemptions' **kinds** and **ages**. The opaque share is a ratio of
//! two numbers already here, so committing it is a third number to keep
//! in step; the ages are `now`-derived, which is the first refutation
//! above. They are printed on every run because they are worth reading,
//! and not committed because neither belongs in a value comparison.
//!
//! # The gap, stated rather than papered over
//!
//! Obligations derive from a block's **content**, so a change that
//! rewords an `apprafter` invocation until it stops looking like one,
//! or drops the `package` clause that makes a fence a complete CUE
//! document, deletes the obligation without moving any counter here.
//! The count is unchanged; the check is gone. **These ratchets defend
//! against the careless, not against the motivated** — review defends
//! against the motivated, and no arithmetic over a corpus can take that
//! job.

use serde::{Deserialize, Serialize};
use std::error::Error;
use std::path::{Path, PathBuf};

/// Where the census lives, repo-relative.
///
/// Under `docs/measurements/` because that is already, three times
/// over, the place for a number rather than a page: `mkdocs.yml`'s
/// `exclude_docs` keeps the directory out of the built site,
/// [`crate::scan`]'s exclusion list keeps it out of the corpus this
/// file counts (a census inside its own corpus would count itself),
/// and `docs/contributing/documentation-gate.md` already names it as
/// internal working data. No new exclusion anywhere.
pub const FILE: &str = "docs/measurements/docs-health.json";

/// The command that records a new census, named in every message that
/// asks for one.
pub const RECORD: &str = "docsgen metrics";

/// What the corpus held when this file was last written.
///
/// Field order is the file's key order — [`serde_json`] emits a struct
/// in declaration order — and the file is committed, so the order is
/// part of what a reviewer reads. Reordering these fields rewrites
/// lines nobody changed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Baseline {
    /// In-scope pages: `git ls-files -- docs README.md`, `.md` only,
    /// minus the excluded trees.
    pub pages: usize,
    /// Every `apprafter …` invocation seen, exempted ones included.
    pub invocations: usize,
    /// Schema identifiers that resolved. Not the total written: an
    /// unresolved one is already a finding, and counting it here would
    /// let a page keep its number by leaving the error in place.
    pub identifiers: usize,
    /// Fenced blocks that are complete CUE documents, and so go
    /// through `cue vet`.
    pub cue_documents: usize,
    /// Repository paths the documentation names, in code spans and in
    /// link targets alike.
    pub code_paths: usize,
    /// ADR citations, every spelling of one counted once per sentence.
    pub adr_references: usize,
    /// Declared exemptions across every channel. Compared for
    /// equality — see the module docs.
    pub exemptions: usize,
}

/// Every way the current corpus differs from the committed census, one
/// message per field.
///
/// One message per divergence rather than one per run: the report
/// accumulates for the reason [`crate::gate::report`] does — a gate
/// that surfaces one problem at a time costs a CI round-trip per fix
/// and teaches contributors to stop running it.
///
/// Both numbers appear in every message. "The census regressed" is not
/// actionable; a reader has to know whether one page went or fifteen
/// did before they can tell whether it was theirs.
pub fn compare(committed: &Baseline, current: &Baseline) -> Vec<String> {
    let mut out = Vec::new();
    for (field, was, now) in [
        ("pages", committed.pages, current.pages),
        ("invocations", committed.invocations, current.invocations),
        ("identifiers", committed.identifiers, current.identifiers),
        (
            "cue_documents",
            committed.cue_documents,
            current.cue_documents,
        ),
        ("code_paths", committed.code_paths, current.code_paths),
        (
            "adr_references",
            committed.adr_references,
            current.adr_references,
        ),
    ] {
        if now < was {
            out.push(format!(
                "`{field}`: the committed census records {was}, this run found {now} — \
                 {} fewer, so the documentation lost surface it used to have",
                was - now
            ));
        }
    }
    if current.exemptions != committed.exemptions {
        // Both directions, and said in the message rather than left to
        // the remedy: a reader who has just ADDED an exemption is
        // looking at a finding about growth, which reads as a bug in
        // the gate until the sentence explains itself.
        let direction = if current.exemptions > committed.exemptions {
            "an exemption was declared"
        } else {
            "an exemption was retired"
        };
        out.push(format!(
            "`exemptions`: the committed census records {}, this run found {} — \
             {direction}, and this count is compared for equality in BOTH directions \
             so that either shows up as one reviewable line",
            committed.exemptions, current.exemptions
        ));
    }
    out
}

/// Read the committed census.
///
/// Every failure is `Err`, never an empty or defaulted [`Baseline`]:
/// carrying on with zeroes would make a checkout missing this file
/// report every future run as growth, which is the one answer that is
/// wrong in both directions at once. The caller turns this into the
/// gate's BROKEN exit code — a checkout to repair, not a page to edit.
pub fn read(repo_root: &Path) -> Result<Baseline, Box<dyn Error>> {
    let path = repo_root.join(FILE);
    let text = std::fs::read_to_string(&path)
        .map_err(|e| format!("reading {FILE}: {e} — record it with `{RECORD}`"))?;
    serde_json::from_str(&text)
        .map_err(|e| format!("{FILE} is not a census this build understands: {e} — if a field was added or renamed, re-record it with `{RECORD}`").into())
}

/// Write the census, returning the absolute path written.
///
/// Pretty-printed with a trailing newline: it is committed, so its diff
/// is the review, and a single-line JSON blob makes every change look
/// like a whole-file rewrite.
pub fn write(repo_root: &Path, baseline: &Baseline) -> Result<PathBuf, Box<dyn Error>> {
    let path = repo_root.join(FILE);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut text = serde_json::to_string_pretty(baseline)?;
    text.push('\n');
    std::fs::write(&path, text)?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The comparison's own contract lives in
    /// `cli/docsgen/tests/health_test.rs`; these two cover what only an
    /// in-module test can see.
    #[test]
    fn an_unknown_field_is_rejected_rather_than_ignored() {
        // `deny_unknown_fields`, deliberately: a field renamed in the
        // struct and not re-recorded would otherwise deserialise to
        // whatever `usize` a missing key defaults to — except it would
        // not, because a missing key is already an error. The pair is
        // what makes a hand-edited file fail loudly in both directions.
        let text = r#"{"pages":1,"invocations":1,"identifiers":1,"cue_documents":1,
            "code_paths":1,"adr_references":1,"exemptions":1,"blocks_executed":0}"#;
        assert!(serde_json::from_str::<Baseline>(text).is_err());
    }

    #[test]
    fn a_message_carries_the_field_and_both_numbers() {
        let committed = Baseline {
            pages: 33,
            invocations: 384,
            identifiers: 236,
            cue_documents: 2,
            code_paths: 73,
            adr_references: 66,
            exemptions: 2,
        };
        let mut current = committed.clone();
        current.cue_documents = 1;
        let found = compare(&committed, &current);
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].contains("cue_documents"), "{}", found[0]);
        assert!(
            found[0].contains('2') && found[0].contains('1'),
            "{}",
            found[0]
        );
    }
}
