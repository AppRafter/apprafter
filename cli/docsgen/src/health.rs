// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The committed corpus census, and the rule that compares it.
//!
//! [`crate::gate::Stats`] measures the documentation on every run and
//! prints one line. That line is a fact about the checkout it was
//! printed in and nothing else: a run that finds half the corpus prints
//! its half as calmly as its predecessor printed the whole of it, and
//! passes. This module commits the numbers so that the *next* run has
//! something to disagree with.
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
//!
//!   No counter kept here is such a number, and that is
//!   **maintained rather than assumed**. Exemptions are aged against
//!   `now` and this checkout's tags on every run, so the obvious
//!   implementation — let a `check=none` fence drop out of the walk —
//!   would put both back in: the same page bytes would measure one way
//!   with the tag present and another way on a `git clone --depth 1`,
//!   and a census recorded on a shallow clone would report a loss on
//!   every full one. So a marker silences **findings, never counts**:
//!   [`crate::gate::Gate::page`] accumulates a silenced block's
//!   invocations, identifiers and CUE documents before it stops
//!   reporting on them. Re-run over one `check=none` fence on a corpus
//!   page, with `since=` naming a tag this checkout has and then one it
//!   does not: both runs printed the same census, and only the
//!   findings differed. Any change that moves a count inside a
//!   silencing branch re-opens this.
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
//! So: integers, compared by value, with the file's exact bytes
//! nobody's business but a reviewer's.
//!
//! # Why these counters, and what growth means
//!
//! Every field here is an **obligation count** — a number that falls
//! when documented surface is deleted. That is the whole selection
//! rule (with one caveat on `identifiers`, below, which falls for a
//! second reason too), and it came from watching three earlier
//! candidates fail it:
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
//! counters a **file** cannot leave `git ls-files` on its own without one
//! of them falling — which is a narrower guarantee than it first
//! reads, and the gap section below states how much narrower: what
//! goes loud is the file's disappearance, not its content's.
//!
//! Even that holds only per commit. They are corpus-wide sums, so
//! a deletion nets out against unrelated growth beside it. Re-run:
//! `git rm docs/index.md` (which contributes only to `pages`) beside a
//! three-line stub page printed the committed census byte for byte and
//! exited 0. Sizing a deletion is the reviewer's job; this file only
//! makes an *unaccompanied* one loud.
//!
//! One of them is not purely an obligation count, and the
//! exception is worth naming rather than discovering: `identifiers`
//! counts the paths that **resolved**, so it also falls when a path
//! stops resolving — a field renamed in `schemas/v1alpha1`, a CRD
//! regenerated, or a broken path silenced by a `schema-check-ignore`
//! entry (which moves `exemptions` too, so one edit reports twice). No
//! deletion happened in any of those. It is kept anyway because the
//! alternative — counting every path written — lets a page hold its
//! number by leaving a broken reference in place, which is the worse
//! failure; [`crate::gate::HEALTH_BASELINE`]'s remedy opens by sending
//! the reader to the other findings in the same run for exactly this
//! reason.
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
//! Equality guarantees a review line for a **net** change, and only
//! per commit — the same netting the ratcheted sums have, and it was worth
//! stating there and not here until a reviewer landed it. Declaring
//! one exemption while retiring another passes green: trading
//! `docs/dev-guide/application-cue.md`'s narrow one-path
//! `schema-check-ignore` for a blanket `check=none` over the
//! four-invocation fence in `docs/operator-guide/quickstart.md`
//! printed `2 exemptions (2 historical)` and exited 0, having widened
//! the exempted surface from a single identifier to a whole fence's
//! CLI, CUE and identifier checks. Committing the per-kind breakdown
//! would not close it either — that reports a retype as a regression,
//! which the next paragraph rejects — so the answer here is the
//! statement, not another rule.
//!
//! Equality on the **number**, and on nothing else. The kinds are
//! printed, not committed (below), so re-typing a standing exemption
//! passes green: flipping the corpus's one `reason: historical` entry
//! to `reason: known-broken` printed the new kind on the census line,
//! held `exemptions` at 2 and exited 0. A retype is a re-justification
//! and belongs to review, which sees the diff line either way — the
//! count is here to make sure there *is* a diff line.
//!
//! # Reported, not committed
//!
//! [`crate::gate::Stats::line`] prints two things this file does not
//! hold: the **opaque** count — identifiers that resolved *only*
//! because they sit under a subtree the CRDs do not describe — and the
//! exemption total broken down **by kind**. The kinds are labels
//! rather than a magnitude, and a value comparison over them would
//! report a retype as a regression.
//!
//! The opaque count is left out for a harder reason than it once said
//! here. An earlier revision claimed it was "a ratio of two numbers
//! already here", which is false — it is in no [`Baseline`] field, so
//! only the denominator is committed and
//! the count is an independent measurement. The gap section below
//! refutes it outright: its second example moves the opaque count
//! 115 → 114 while every committed number holds, which no
//! derivation of them could do.
//!
//! The real reason is that **its direction cannot be read**. It falls
//! when the documentation degrades — that example replaces a deep
//! claim with a shallow one, and the opaque claim goes with it — and
//! it falls just as surely when the *schemas improve*, because a path
//! the CRDs learn to describe stops resolving opaquely. One number,
//! two opposite meanings, so neither a ratchet nor an equality rule
//! over it says anything a reader could act on. It is worth printing
//! for exactly the reason it is not worth committing: a human can tell
//! those two cases apart and a comparison cannot.
//!
//! Exemption **ages** are printed by neither. They are `now`-derived —
//! the first refutation above — and they are computed on every run for
//! the expiry rule, so they surface only inside an
//! [`crate::gate::EXEMPTION_EXPIRED`] or
//! [`crate::gate::EXEMPTION_UNAGED`] finding: a passing run prints no
//! age at all.
//!
//! # The gap, stated rather than papered over
//!
//! Every counter here measures **cardinality, never specificity**. An
//! obligation that is *substituted* rather than deleted therefore costs
//! nothing at all: the count holds, the gate exits 0, and the check
//! that went leaves no line in the census to show for it. Three shapes
//! of that, each one re-run against the corpus these numbers describe:
//!
//! * **A rich invocation for a trivially-resolving one.**
//!   `docs/operator-guide/backup-restore.md` documents `apprafter
//!   backup enable` with twelve flags. Replaced by the bare `apprafter
//!   backup enable`, every number held and the gate exited 0 —
//!   `invocations` counts invocations, never the flags inside one.
//! * **A deep identifier for a shallow one.**
//!   `docs/operator-guide/postgres.md` claims `spec.base.needs.pg`
//!   in its opening sentence. Rewritten to `spec.base`, `identifiers`
//!   held and the gate exited 0; the only number that moved was the
//!   opaque share, which is printed and not committed, so nothing
//!   compared it. (No figures stand beside that. They were written
//!   here once and were stale within the branch that wrote them —
//!   every counter is a corpus total, so any unrelated page edit moves
//!   them. The property is what re-derives: shorten the identifier,
//!   re-run `docsgen gate`, watch `identifiers` hold.)
//! * **A specific path for its parent directory.**
//!   `docs/contributing/documentation.md` names
//!   `cli/docsgen/src/render.rs`. Rewritten to `cli/docsgen/src`,
//!   every number held and the gate exited 0: a directory resolves
//!   exactly as a blob does, and `code_paths` counts references, not
//!   depth.
//!
//! And a fourth shape, larger than the three above and the one to read
//! first: **the counters count claims, not documentation.** Prose that
//! carries no counted claim is invisible to all of them, so a page can
//! lose its entire explanation with the census unchanged. Both re-run
//! through the whole of `scripts/docs-check.sh`, not just the gate:
//! deleting the non-claim prose of
//! `docs/operator-guide/troubleshooting.md` — well over a third of the
//! longest operator guide there is — printed the committed census byte
//! for byte and exited 0; and `docs/index.md` — which makes no counted
//! claim at all — can be gutted **in place** to a single `# AppRafter`
//! heading, with no `git rm` and nothing added beside it, and every
//! number still holds. (No before-and-after line count stands here.
//! The first version of this paragraph carried one whose arithmetic did
//! not add up, on a file this branch then lengthened; the property is
//! what re-derives, and re-running the deletion is what checks it.)
//! That is the honest reading of "a file cannot leave on its own"
//! above: they notice a page leaving `git ls-files`, not a page being
//! emptied where it stands. Sweeping **every** page of the corpus —
//!
//! ```text
//! git ls-files -- 'docs/*.md' README.md |
//!     grep -vE '^docs/(adr|changelog|measurements|reference/cli)/'
//! ```
//!
//! — `docs/index.md` is the only one that can be gutted for free
//! today; the sweep was re-run over the whole list after this branch
//! added `docs/contributing/documentation.md`, and gutting that new
//! page fires `health-baseline` three times over. But every page in it
//! can lose all of its non-claim prose for free.
//!
//! **These ratchets defend against the careless, not against the
//! motivated** — review defends against the motivated, and no
//! arithmetic over a corpus can take that job.

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
    ///
    /// This is the **opposite** rule from [`Baseline::invocations`] and
    /// [`Baseline::code_paths`], which count every claim written. The
    /// divergence is a decision, not an oversight: those two have no
    /// per-claim silencing channel that would let a page swap a
    /// resolving claim for a broken-but-exempted one, and this one
    /// does. The cost is that this field is not purely an obligation
    /// count — see the module docs, and
    /// [`crate::gate::HEALTH_BASELINE`]'s remedy, on what else moves
    /// it.
    pub identifiers: usize,
    /// Fenced blocks that are complete CUE documents, and so go
    /// through `cue vet`.
    pub cue_documents: usize,
    /// Fences tagged `cue`, complete documents or not — the floor under
    /// the built site's lexer check. See [`crate::gate::Stats::cue_fences`]
    /// for why the number is recorded and enforced on this side rather
    /// than in `scripts/docs-artefacts-check.py`, which is what reads
    /// the rendered blocks — **and** for the gap that leaves: this
    /// floors the in-scope corpus only, so the `cue` fences on `adr/`
    /// pages are checked by that pass and floored by nothing.
    pub cue_fences: usize,
    /// Repository paths the documentation names, in code spans and in
    /// link targets alike.
    pub code_paths: usize,
    /// ADR citations, every spelling of one counted once per sentence.
    pub adr_references: usize,
    /// Declared exemptions across every channel — a fence's
    /// `check=none` marker and all three front-matter lists
    /// (`cli-check-ignore`, `schema-check-ignore`, `adr-check-ignore`).
    /// Compared for equality — see the module docs.
    ///
    /// "Every channel" is load-bearing and was once false: the fence
    /// marker is the **first** channel
    /// `docs/contributing/documentation-gate.md` documents, and while
    /// it went uncounted a page could take one out with the committed
    /// number unchanged. `Gate::fence_marker` counts it where it builds
    /// it, for that reason.
    pub exemptions: usize,
}

/// Which of the two events a divergence is.
///
/// The census carries one comparison rule per field but **two** kinds
/// of news, and [`crate::gate`] prints one remedy per finding class: a
/// reader who has just declared an exemption must not be told
/// to put back documentation nobody removed. So the caller keys the
/// class off this rather than off the field name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// An obligation count fell — documented surface went.
    Loss,
    /// The exemption count moved, in either direction.
    Exemptions,
}

/// One field's disagreement with the committed census.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Divergence {
    /// What kind of event it is, which decides the finding class.
    pub kind: Kind,
    /// The one-line message, naming the field and both numbers.
    pub message: String,
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
pub fn compare(committed: &Baseline, current: &Baseline) -> Vec<Divergence> {
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
        ("cue_fences", committed.cue_fences, current.cue_fences),
        ("code_paths", committed.code_paths, current.code_paths),
        (
            "adr_references",
            committed.adr_references,
            current.adr_references,
        ),
    ] {
        if now < was {
            out.push(Divergence {
                kind: Kind::Loss,
                message: format!(
                    "`{field}`: the committed census records {was}, this run found {now} — \
                     {} fewer, so the documentation lost surface it used to have",
                    was - now
                ),
            });
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
        out.push(Divergence {
            kind: Kind::Exemptions,
            message: format!(
                "`exemptions`: the committed census records {}, this run found {} — \
                 {direction}, and this count is compared for equality in BOTH directions \
                 so that either shows up as one reviewable line",
                committed.exemptions, current.exemptions
            ),
        });
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
        let mut committed = Baseline {
            pages: 33,
            invocations: 384,
            identifiers: 236,
            cue_documents: 2,
            cue_fences: 17,
            code_paths: 73,
            adr_references: 66,
            exemptions: 2,
        };
        let mut current = committed.clone();
        // 41 and 40, not the live 2 and 1: a single-digit assertion
        // passes on any message that happens to hold that digit, and
        // every other field in this census holds one.
        committed.cue_documents = 41;
        current.cue_documents = 40;
        let found = compare(&committed, &current);
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].kind, Kind::Loss, "{found:?}");
        assert!(found[0].message.contains("cue_documents"), "{found:?}");
        assert!(
            found[0].message.contains("records 41"),
            "the committed number: {found:?}"
        );
        assert!(
            found[0].message.contains("found 40"),
            "this run's number: {found:?}"
        );
    }
}
