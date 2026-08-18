// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Extract the ADRs a documentation page cites, and read whether the
//! decision each one records still stands.
//!
//! An ADR is this repository's decision record, and the documentation
//! cites one as *authority*: "per ADR 0046, `env` takes a claim
//! reference". Two things can go wrong with that sentence and neither is
//! visible to any other gate. The number can name nothing — a typo, or a
//! slot that was never used — and the reader searches `docs/adr/` for a
//! decision that does not exist. Or the number can name a decision that
//! was **reversed**, and the reader is handed the superseded answer with
//! the authority of a citation behind it. The second is the worse
//! failure: nothing about it looks wrong.
//!
//! # Three spellings, and why one checker cannot know only one
//!
//! Over the 33 in-scope pages there are **66 citations across 22
//! distinct ADRs**:
//!
//! * 57 spelled `ADR 0046`;
//! * 4 spelled `ADR-0032` — all four in `docs/license.md`, as
//!   `Pre-ADR-0032` / `Post-ADR-0032`;
//! * 1 wrapped across a line break (below);
//! * 4 carrying **no `ADR NNNN` text at all**, where the number is
//!   readable only inside an ADR's filename: `README.md:121` writes
//!   `and ADRs [0001](./docs/adr/0001-license-fsl-1-1-mit.md) … and
//!   [0032](…)`, and `docs/license.md` names one ADR per line in a code
//!   span — `:91` `` `docs/adr/0001-license-fsl-1-1-mit.md` `` and
//!   `:93` `` `docs/adr/0032-license-fsl-1-1-apache-2-0.md` ``.
//!
//! The first three are the `ADR NNNN` grammar — **62 of the 66**. An
//! earlier revision of this module measured those 62 and presented the
//! figure as the count in the corpus. It was not: the other four are
//! citations a reader follows exactly like the rest, and every one of
//! them is a link or a path — the very form the first version of this
//! check's remedy told an author to write instead of naming a number.
//! A grammar blind to the shape its own advice recommends has a hole
//! shaped like that advice. So [`references`] reads `adr/NNNN-` as
//! well: the number is embedded in every ADR's filename, so this costs
//! one more scan of the line and no new vocabulary.
//!
//! Over every tracked `*.md` (188 files) the `ADR NNNN` split is 960
//! space-spelled and 19 hyphen-spelled. A checker written against the
//! space form alone would skip `docs/license.md` entirely **and report
//! success**, which is the failure mode a gate must not have: a silent
//! blind spot reads exactly like a clean page.
//!
//! So [`references`] accepts a space **or** a hyphen. The left boundary
//! is "not a letter or digit" rather than "whitespace" precisely because
//! of `Pre-ADR-0032`: the character before those three letters is a
//! hyphen, and the reference is real.
//!
//! One citation spelled two ways on one line — `[ADR 0046](../adr/0046-env-value-references.md)`,
//! which is how most of the corpus links one — is **one** reference.
//! The shapes are deduplicated per line and per number, because a
//! finding is about the citation, not about its spelling.
//!
//! ## The separator is a run
//!
//! `ADR  0046` (two spaces) and `ADR\t0046` cite 0046. The corpus
//! writes neither, and that is the point: a second space must not be a
//! way to take a citation out of sight, which is exactly the threat the
//! wrap rule below exists to answer for a newline. A grammar that
//! closes the newline hole and leaves the space hole open contradicts
//! its own threat model.
//!
//! The hyphen form stays **exactly one** hyphen. `Pre-ADR-0032` depends
//! on it, and a run of hyphens is a horizontal rule rather than a
//! separator.
//!
//! ## What the grammar deliberately does not hold
//!
//! `ADRs 0034` **is** read as a citation of 0034. The note that used to
//! justify excluding the plural — "a plural noun with a list after it
//! rather than a citation" — was wrong about this repository:
//! `README.md:121` writes `and ADRs [0001](…) and [0032](…)`,
//! `spec.md:1306` writes "ADRs 0025–0029 reframe `cluster-bootstrap`"
//! and `docs/adr/0029-cue-cmp.md:9` writes "ADRs 0025–0028 address the
//! platform-stack side"; every one of those cites. Excluding the plural
//! also made a trailing `s` a one-keystroke suppression, the same hole
//! the wrap rule is here to close. Over the tracked `*.md` the plural
//! is worth **8 raw occurrences** (the space-spelled scan goes 952 →
//! 960) but **7 references** — the unit [`references`] returns, and the
//! unit the 66/62/57/4 figures above are in — because `spec.md:7`
//! writes both `ADRs 0034–0038` and `ADR 0034`, and per-line
//! deduplication collapses those two spellings of one citation into
//! one. In scope it is worth none: of the corpus's 4 `ADRs`, one is a
//! bare plural noun, two link the directory index, and the fourth is
//! `README.md:121`, whose numbers are links the filename rule reads.
//!
//! Only the **first** number after `ADRs` is read. Continuing the list
//! through `, 0038` or `and 0038` would mean treating a bare four-digit
//! run in prose as a citation, and the commonest four-digit run in this
//! corpus is a year — the exact false positive the wrap rule had to be
//! narrowed to avoid.
//!
//! Out of grammar on purpose, and by measurement — the corpus writes
//! none of them — are lowercase `adr 0011`, `ADR#0011` and `ADR: 0011`.
//! Each would be cheap to add, and none of them is a plausible
//! *suppression*: an author reaching for `ADR: 0011` is not reaching
//! for it instead of a spelling this module reads. If one turns up in
//! review, widening is a line in [`scan_line`] — but a grammar that
//! guesses at spellings nobody writes is a grammar nobody can
//! re-derive from the corpus.
//!
//! # The wrapped reference, and what it must not swallow
//!
//! `docs/operator-guide/target-store.md:8` writes the citation across a
//! blockquote line break:
//!
//! ```text
//! > Authoritative design rationale: [ADR
//! > 0030](../adr/0030-cli-target-store-and-credential-chain.md).
//! ```
//!
//! A line-local scan finds 61 of the 62 in-grammar citations and is
//! silent about this one. That is one reference, and it would not matter
//! today — 0030 is Accepted — except that "wrap the line" is then a
//! one-keystroke way to take a citation out of the gate's sight, with
//! nothing in the diff that reads as suppression. So a line ending in a
//! bare `ADR` is joined to the next line, after its leading whitespace
//! and blockquote markers, and the reference is reported on the line the
//! citation *starts* — the same rule
//! [`crate::invocation::logical_lines`] applies to a command whose flags
//! continue on the next line.
//!
//! The join is **bracketed on one side or the other**: the trailing
//! `ADR` immediately preceded by `[`, or the continuation number
//! immediately followed by `]`. Without that condition the rule
//! manufactures findings out of ordinary prose, because `ADR` is an
//! ordinary English noun in this register —
//! `docs/operator-guide/index.md:54` writes "An ADR describes the world
//! as it was when it was ratified" — and a year is the commonest
//! four-digit run in the corpus. So
//!
//! ```text
//! - Every decision is recorded as an ADR
//!   2026-05-06 is the earliest one.
//! ```
//!
//! reported `` `ADR 2026`: no such ADR `` — a hard finding, with no
//! ignore key, on a sentence that is correct. The same join crossed
//! sentence boundaries silently whenever the number happened to
//! resolve.
//!
//! Either bracket satisfies the condition, so the accept set is larger
//! than either alone and one year-shaped false positive survives it: a
//! line ending on the bare word `ADR` followed by a line opening
//! `2026]` is read as a citation of 2026. It is kept as an `||` even
//! so, because the corpus's one wrap is bracketed on the *left* and the
//! closing bracket is what catches the reverse-direction wrap; the
//! residual needs a `]` immediately after a four-digit run opening a
//! line, which the corpus writes nowhere.
//!
//! What the narrowing gives up is the unbracketed prose wrap:
//! `follows ADR` / `0011.` is no longer joined. Three measurements
//! decide that trade. The corpus's only wrapped citation is the
//! bracketed one above. The unanchored join manufactured a hard
//! finding on correct prose (`ADR` / `2026-05-06`), and a false
//! positive is a page the gate is *wrong* about — unfixable except by
//! rewording a correct sentence — while a missed citation is one it is
//! merely quiet about. And what is given up is real rather than
//! hypothetical: **26 of the 66** in-scope citations are bare
//! `ADR NNNN` prose with no path anywhere on the line, so a
//! deliberately wrapped bare-prose citation goes unseen. That cost is
//! accepted, not argued away.
//!
//! # A leading token, not a substring
//!
//! Status lives under a `## Status` **heading** rather than on a
//! `Status:` line, and its body opens with a verdict token followed by
//! prose. That structure is what makes partial supersession safe to
//! distinguish, and it has to be distinguished, because four ADRs in
//! this repository are amended rather than withdrawn:
//!
//! | ADR | its `## Status` opens | and then says | the decision | in-scope references |
//! |---|---|---|---|---|
//! | 0011 | ``Superseded by ADR 0016``. | originally accepted 2026-05-06 | gone | 0 |
//! | 0001 | ``Accepted``. Date: 2026-05-06. | ``Superseded`` by ADR 0032 (core base licence only; the plugin MIT carve-out stands) | **stands** | 5 |
//! | 0042 | ``Accepted`` (2026-06-05). | §7 superseded in part by ADR 0046 | **stands** | 1 |
//! | 0043 | ``Accepted`` (2026-06-06). | §1's env-injection naming superseded in part by ADR 0046 | **stands** | 2 |
//! | 0053 | ``Accepted`` (2026-08-08). | §3 superseded in part by ADR 0055 | **stands** | 3 |
//!
//! A rule that looks for `superseded` anywhere in the block fires on the
//! last four — **11 in-scope references, not one of them a defect** —
//! and on 0011, which is cited nowhere in scope. Its whole observed
//! output over this corpus is 11 false positives and no true one. Widen
//! the substring to `supersed` and it picks up the ADRs that
//! *supersede* something — 0032 ("Supersedes the MIT base license
//! choice in ADR 0001", 13 references), 0055 ("This supersedes in part
//! ADR 0053 §3", 3) and 0016 (0) — for **27 references across 6 cited
//! ADRs**, still with no true positive among them. A gate whose entire
//! observed output is false is a gate that gets switched off within the
//! week. Reading only the **first word** gets every row right, and is
//! what [`verdict`] does.
//!
//! ## The first word, after decoration
//!
//! "First word" means the first token that is non-empty **once
//! decoration is trimmed**, not the first token. Trimming after
//! choosing the token made a Status body opening on a blockquote or a
//! list marker — `> Accepted`, `- Accepted`, ``* `Accepted` `` — trim
//! to the empty string and fall through to [`Verdict::Unknown`]: a hard
//! finding on every page citing that ADR, sent to fix a vocabulary that
//! was never the problem. It also contradicted the reason given for
//! trimming a character class in the first place, which is that a sixth
//! way of writing `Accepted` should cost nothing.
//!
//! One marker survives on purpose: `1. Accepted` reads `1`, which is
//! alphanumeric and therefore a token in its own right, so an ordered
//! list under `## Status` still reads `Unknown`. Skipping numeric
//! tokens would make `2026-05-06 Accepted` read as a verdict, which is
//! a worse answer than the one this leaves.
//!
//! The class holds `#` like every other non-alphanumeric character,
//! which couples this rule to where the Status section *ends*: a
//! heading the section wrongly swallowed becomes the verdict, `###`
//! trimming away to leave `Accepted`. That is answered where it
//! belongs, in [`ends_status_section`], and not by carving `#` out of
//! the class here — the mis-scoping is a defect on its own terms, and
//! an exception list is what this class exists not to be.
//!
//! # What is a finding, and what is not
//!
//! `Superseded` and `Deprecated` are findings: the decision was
//! reversed. `Unused` is a finding for a different reason — `0013` and
//! `0018` are slots reserved during early planning and abandoned, so the
//! number names nothing at all, exactly like a typo.
//!
//! `Draft` is **not** a finding, and this is a deliberate policy rather
//! than an omission: 9 of the 66 in-scope references point at ADRs
//! 0025–0029, all Draft, and documentation legitimately cites a decision
//! the project is working to. `Proposed` is not a finding for the same
//! reason. A gate that demanded `Accepted` would be demanding that nine
//! correct sentences be deleted.
//!
//! Citing a decision that no longer stands is sometimes exactly right —
//! `spec.md:1454` writes "superseded ADR 0011 → ADR 0016" and
//! `spec.md:1574` "see superseded ADR 0011", sentences that announce the
//! supersession in the same breath. Those two are out of this gate's
//! corpus (it holds `docs/` and `README.md`), but they are the same authors
//! writing the same documentation, and the idiom lands in `docs/` the
//! moment a page needs it. That case goes through
//! [`crate::gate::ADR_IGNORE`] — a front-matter exemption with the same
//! typed reason, the same `since=` and the same expiry as every other
//! exemption in this crate — rather than through a rewrite of the
//! sentence into a form this module cannot see.
//!
//! ## `Unknown`: argued both ways, and chosen
//!
//! [`Verdict::Unknown`] is what a `## Status` that opens on a word this
//! module does not know parses to, and it is **a finding**.
//!
//! *Against*: the defect it reports is in the ADR, while the finding
//! lands on every page that cites it — a contributor editing a guide is
//! sent to a file they did not touch and may have no standing to
//! re-status. And ADRs are out of the gate's corpus by design, so this
//! is the one route by which an ADR's own text can fail the docs build.
//!
//! *For*: the alternative is to treat "I cannot read this status" as "the
//! decision stands", which is a guess, and the guess is made in the
//! direction of silence. A vocabulary drifts one word at a time —
//! someone writes `Ratified`, or `Accepted, amended` becomes `Amended` —
//! and every reference to that ADR stops being checked with nothing
//! anywhere reporting it. The whole reason this check exists is that a
//! citation which is silently unchecked reads exactly like a citation
//! which is fine.
//!
//! It is chosen as a finding, and the cost is bounded by measurement:
//! **every** one of the 57 ADRs the register holds today parses to a
//! known verdict
//! (`the_register_reads_this_repositorys_own_adrs` pins that), so this
//! arm reports nothing until someone invents a new word — which is the
//! moment it exists for. Widening the vocabulary is a one-line edit in
//! [`verdict`], and that edit is the review this check is asking for.
//!
//! # The register is what `git` tracks
//!
//! [`statuses`] reads `git ls-files -- docs/adr`, not the directory.
//! Everything else this gate resolves against comes from the index —
//! the tracked file listing, the corpus itself — and a register read off
//! the worktree answers differently in the two places the gate runs: an
//! ADR written but not staged resolves in the pre-commit hook and not in
//! CI, so a page passes locally and fails on the pull request with the
//! *citation* named as the defect rather than the missing `git add`.
//! That is the asymmetry [`crate::gate::Gate`]'s tag handling already
//! exists to close, and it costs one `git` call to close here.
//!
//! Only the **listing** comes from the index. [`statuses`] then reads
//! each listed file's bytes from the **worktree**, so an ADR staged as
//! `Accepted` and edited to `Superseded` without staging registers as
//! `Superseded`. That is the same split the gate applies to the pages
//! it checks — `git ls-files` for what is in scope, the worktree for
//! what each page says — so a citation and the decision it names are
//! read at the same moment in the same tree.
//!
//! `git ls-files -- docs/adr` is **recursive**, which `read_dir` was
//! not. An ADR filed at `docs/adr/<subdir>/NNNN-slug.md` therefore
//! enters the register keyed by the number in its bare filename, and collides with a
//! top-level `NNNN-` file into the hard `Err` below. All 59 tracked
//! entries sit directly in `docs/adr/` today, so this is scope the
//! register gained rather than a case it mishandles — but a nested ADR
//! is now visible to it, and a duplicate number across the two levels
//! stops the gate rather than resolving to one of them.
//!
//! Two files claiming one number is an **`Err`**, not last-write-wins.
//! `read_dir` hands entries back in an unspecified order, so
//! `0011-original.md` (Superseded) beside `0011-zz-scratch-copy.md`
//! (Accepted) resolved to whichever came last — and when that was
//! `Accepted`, every citation of a reversed decision quietly stopped
//! being reported, with nothing saying so. A register that cannot say
//! what a number names is the gate being broken: exit 2, not a finding.
//!
//! # `0000-template.md` is skipped
//!
//! Its Status body is the literal enum menu — ``Proposed`` | ``Draft`` |
//! ``Accepted`` | ``Deprecated`` | ``Superseded by ADR NNNN`` — so
//! reading it puts a `Proposed` ADR 0000 in the register and makes the
//! menu look like a decision. It is excluded by name, and a page citing
//! `ADR 0000` gets its own message from [`crate::gate`] rather than
//! "no such ADR", which would be false about a file that is right
//! there. `README.md` in the same directory is dropped by shape: an ADR
//! file is named for its number, and a file whose name does not open on
//! four digits is not one.

use std::collections::BTreeMap;
use std::error::Error;
use std::path::Path;
use std::process::Command;

/// The ADR directory, relative to the repository root.
const DIR: &str = "docs/adr";
/// The one file in it that is not an ADR but is named like one.
pub(crate) const TEMPLATE: &str = "0000-template.md";
/// The number [`TEMPLATE`] is named for. In no register, and not a typo
/// either, so [`crate::gate`] answers a citation of it in its own words
/// rather than with "no such ADR".
pub(crate) const TEMPLATE_NUMBER: &str = "0000";

/// What an ADR's `## Status` opens with.
///
/// A closed vocabulary rather than a string, so that "is this decision
/// still standing" is decided in one place — and so that a word nobody
/// has seen before lands in [`Verdict::Unknown`] rather than being
/// silently read as fine. See the module docs for why that is a finding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Accepted,
    Draft,
    Proposed,
    Deprecated,
    Superseded,
    Unused,
    /// The Status section is missing, empty, or opens on a word this
    /// module's vocabulary does not hold.
    Unknown,
}

impl Verdict {
    /// The token as the ADRs spell it, for a finding's message.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Verdict::Accepted => "Accepted",
            Verdict::Draft => "Draft",
            Verdict::Proposed => "Proposed",
            Verdict::Deprecated => "Deprecated",
            Verdict::Superseded => "Superseded",
            Verdict::Unused => "Unused",
            Verdict::Unknown => "unknown",
        }
    }
}

/// The verdict an ADR's `## Status` body opens with.
///
/// The **first** word, decoration stripped from both ends: the corpus
/// writes the same verdict as `Accepted`, `` `Accepted` ``,
/// `` `Accepted`. ``, `**Accepted**` and `Accepted (2026-05-12).`, and
/// all five are one decision. Everything after that first word is prose
/// and is deliberately not read — see the module docs on why a
/// substring match on "superseded" would report 11 correct references
/// and no incorrect one.
///
/// "First word" is the first token that is **non-empty after
/// trimming**, not the first token: a body opening `> Accepted` or
/// `- Accepted` trims its first token to nothing, and reading that as
/// "no verdict" would report every page citing the ADR. See the module
/// docs for the one marker that still reads `Unknown`.
pub fn verdict(status_body: &str) -> Verdict {
    // Backticks, asterisks, underscores, brackets, blockquote and list
    // markers and the trailing `.`/`,`/`:` are Markdown decoration
    // around the token, never part of it. Trimming a character class
    // rather than listing the decorations means a sixth way of writing
    // `Accepted` costs nothing.
    let bare = status_body
        .split_whitespace()
        .map(|word| word.trim_matches(|c: char| !c.is_ascii_alphanumeric()))
        .find(|word| !word.is_empty());
    let Some(bare) = bare else {
        return Verdict::Unknown;
    };
    match bare.to_ascii_lowercase().as_str() {
        "accepted" => Verdict::Accepted,
        "draft" => Verdict::Draft,
        "proposed" => Verdict::Proposed,
        "deprecated" => Verdict::Deprecated,
        "superseded" => Verdict::Superseded,
        "unused" => Verdict::Unused,
        _ => Verdict::Unknown,
    }
}

/// One ADR citation on a page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdrRef {
    /// 1-based source line the citation **starts** on.
    pub line: usize,
    /// The four digits, as written — the key into [`statuses`].
    pub number: String,
}

/// Every ADR citation in a page's source, in reading order.
///
/// Two shapes, both anchored on the left by "not a letter or digit"
/// (`PADR 0046` is not a citation; `Pre-ADR-0032` is, because a hyphen
/// is not a letter):
///
/// * `ADR 0046` / `ADRs 0046` / `ADR-0046`, where the number must be
///   **exactly** four digits with nothing alphanumeric after them —
///   `ADR 00461` names some other number that happens to follow those
///   letters;
/// * `adr/0046-`, the number as an ADR's filename spells it, which is
///   how a link target and a code span cite one.
///
/// A line that writes both — the usual `[ADR 0046](../adr/0046-….md)` —
/// yields one reference, not two.
pub fn references(src: &str) -> Vec<AdrRef> {
    let lines: Vec<&str> = src.lines().collect();
    let mut out = Vec::new();
    // The number a wrapped citation on the line above already reported.
    // Its continuation line carries that citation's link target, so the
    // filename scan would otherwise report one citation twice, on two
    // lines, which is two findings for one sentence.
    let mut wrapped: Option<String> = None;
    for (index, text) in lines.iter().enumerate() {
        let line = index + 1;
        let mut here = Vec::new();
        scan_line(text, &mut here);
        scan_filenames(text, &mut here);
        let mut seen: Vec<String> = Vec::new();
        for number in here {
            if wrapped.as_deref() == Some(number.as_str()) || seen.contains(&number) {
                continue;
            }
            seen.push(number.clone());
            out.push(AdrRef { line, number });
        }
        // The wrapped shape, joined rather than missed: see the module
        // docs. Only a line whose LAST token is a bare `ADR` can start
        // one, so this never re-reads the same TEXT `scan_line` took —
        // but it can read the same NUMBER, and it goes through the same
        // per-line `seen` gate for it. `See ADR 0030 and also [ADR` /
        // `0030](…)` is one citation of one decision on one line, and
        // pushing it twice is two identical findings a reader cannot
        // tell apart or fix separately.
        wrapped = wrapped_number(text, lines.get(index + 1).copied());
        if let Some(number) = &wrapped {
            if !seen.contains(number) {
                out.push(AdrRef {
                    line,
                    number: number.clone(),
                });
            }
        }
    }
    out
}

/// Every `ADR NNNN` / `ADR-NNNN` citation written on one line.
fn scan_line(text: &str, out: &mut Vec<String>) {
    let bytes = text.as_bytes();
    let mut at = 0;
    while at + 3 <= bytes.len() {
        if &bytes[at..at + 3] != b"ADR" {
            at += 1;
            continue;
        }
        // Left boundary. Indexing a byte before `at` is safe against
        // UTF-8: a continuation byte is >= 0x80 and so not alphanumeric,
        // which is the answer a multi-byte character deserves anyway.
        if at > 0 && bytes[at - 1].is_ascii_alphanumeric() {
            at += 3;
            continue;
        }
        // `ADRs 0034` cites 0034 — see the module docs on why the
        // plural is in the grammar and why only its first number is.
        let mut separator = at + 3;
        if bytes.get(separator) == Some(&b's') {
            separator += 1;
        }
        // A run of spaces or tabs, or exactly one hyphen. Every path
        // out of this match advances past `ADR`, so the loop makes
        // progress whatever the line says.
        let number_at = match bytes.get(separator) {
            Some(b'-') => separator + 1,
            Some(b' ' | b'\t') => {
                let mut cursor = separator;
                while matches!(bytes.get(cursor), Some(b' ' | b'\t')) {
                    cursor += 1;
                }
                cursor
            }
            _ => {
                at = separator;
                continue;
            }
        };
        match four_digits(bytes, number_at) {
            Some(number) => {
                out.push(number);
                at = number_at + 4;
            }
            // Not a citation, but `ADR` might still be the start of the
            // next one on this line, so advance past these letters only.
            None => at = separator,
        }
    }
}

/// Every `adr/NNNN-` on one line — the number as an ADR's *filename*
/// spells it.
///
/// Not restricted to a link target or a code span: distinguishing them
/// would mean an inline-markdown parser, and `adr/NNNN-` is a citation
/// wherever it is written, because that is a path to the decision.
///
/// Not restricted to *this repository's* `docs/adr/` either, and that
/// is the cost of the same simplicity: `https://example.com/adr/1234-foo`
/// links **another** project's decision record, and this reads it as a
/// citation of our 1234 — "names no decision in the register", on a
/// page that is correct. Anchoring on `docs/adr/` instead would drop
/// the corpus's own relative links (`../adr/0046-….md`), which are the
/// shape this scan exists for. So the answer is the exemption channel
/// rather than the grammar: an external ADR link is exactly what a
/// [`crate::gate::ADR_IGNORE`] entry is for, dated and expiring like
/// every other exemption in this crate. No page in scope links one
/// today.
fn scan_filenames(text: &str, out: &mut Vec<String>) {
    let bytes = text.as_bytes();
    let mut at = 0;
    while at + 4 <= bytes.len() {
        if &bytes[at..at + 4] != b"adr/" {
            at += 1;
            continue;
        }
        if at > 0 && bytes[at - 1].is_ascii_alphanumeric() {
            at += 4;
            continue;
        }
        let number_at = at + 4;
        // The hyphen is what tells `0046-env-value-references.md` from
        // `README.md`: every ADR filename is `NNNN-slug.md`.
        match four_digits(bytes, number_at) {
            Some(number) if bytes.get(number_at + 4) == Some(&b'-') => {
                out.push(number);
                at = number_at + 5;
            }
            _ => at += 4,
        }
    }
}

/// Exactly four digits at `at`, terminated by something that is not
/// alphanumeric.
fn four_digits(bytes: &[u8], at: usize) -> Option<String> {
    let end = at.checked_add(4)?;
    if end > bytes.len() || !bytes[at..end].iter().all(u8::is_ascii_digit) {
        return None;
    }
    if bytes.get(end).is_some_and(u8::is_ascii_alphanumeric) {
        return None;
    }
    String::from_utf8(bytes[at..end].to_vec()).ok()
}

/// The number a citation wrapped across a line break carries.
///
/// Bracketed on one side or the other — see the module docs. Without
/// that condition, any four-digit run opening a continuation line after
/// a sentence that ends on the English word "ADR" is read as a
/// citation, and a year is the commonest four-digit run in this corpus.
fn wrapped_number(text: &str, next: Option<&str>) -> Option<String> {
    let bracketed = ends_on_adr(text)?;
    let (number, rest) = opens_on_number(next?)?;
    (bracketed || rest.starts_with(']')).then_some(number)
}

/// Whether the line's last token is a bare `ADR`, so the number may be
/// on the next one — and, if so, whether a `[` immediately precedes it.
fn ends_on_adr(text: &str) -> Option<bool> {
    let head = text.trim_end().strip_suffix("ADR")?;
    let last = head.as_bytes().last().copied();
    if last.is_some_and(|byte| byte.is_ascii_alphanumeric()) {
        return None;
    }
    Some(last == Some(b'['))
}

/// The number a continuation line opens on and the text after it,
/// blockquote markers and indentation stripped — `> 0030](…)` continues
/// `[ADR`.
fn opens_on_number(text: &str) -> Option<(String, &str)> {
    let mut rest = text.trim_start();
    while let Some(stripped) = rest.strip_prefix('>') {
        rest = stripped.trim_start();
    }
    let number = four_digits(rest.as_bytes(), 0)?;
    Some((number, &rest[4..]))
}

/// Every ADR the repository tracks, by number, with the verdict its
/// `## Status` opens with.
///
/// # What a missing `## Status` becomes
///
/// [`Verdict::Unknown`], recorded in the register — **not** an absent
/// key. The distinction decides which of two messages a reader gets, and
/// only one of them is true: an absent key means "there is no such ADR",
/// which is a lie about a file sitting right there in `docs/adr/`, and
/// sends a contributor to fix the citation when the citation is correct.
/// `Unknown` says the status cannot be read, which is the actual defect
/// and names the file that holds it.
///
/// # An empty register is breakage, not a clean repository
///
/// A directory that yields no ADRs means the root is wrong or the
/// directory moved, and a caller that carried on would report every
/// citation in the corpus as naming a nonexistent ADR — dozens of
/// findings, all of them false, none of them fixable by editing a page.
/// That is the gate being broken, and it says so as an `Err`, which
/// `main` maps to exit 2 rather than to "the documentation is wrong".
/// Two files claiming one number is an `Err` for the same reason: see
/// the module docs on why last-write-wins was worse than a hard stop.
pub fn statuses(repo_root: &Path) -> Result<BTreeMap<String, Verdict>, Box<dyn Error>> {
    let dir = repo_root.join(DIR);
    let mut out = BTreeMap::new();
    // Number → the file that claimed it, so a duplicate names both.
    let mut claimed: BTreeMap<String, String> = BTreeMap::new();
    for path in tracked_adrs(repo_root)? {
        let Some(name) = Path::new(&path).file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if !name.ends_with(".md") || name == TEMPLATE {
            continue;
        }
        // An ADR is named for its number. `README.md` is the index, not
        // a decision, and has no number to key it by.
        let Some(number) = four_digits(name.as_bytes(), 0) else {
            continue;
        };
        if let Some(first) = claimed.get(&number) {
            return Err(format!(
                "the ADR register at {} holds two files claiming ADR {number} — `{first}` \
                 and `{name}`. Which decision the number names cannot be read, so every \
                 citation of it would be resolved against whichever file was listed last: \
                 this is the gate being broken rather than the documentation",
                dir.display()
            )
            .into());
        }
        let file = repo_root.join(&path);
        let text = std::fs::read_to_string(&file)
            .map_err(|e| format!("could not read {}: {e}", file.display()))?;
        claimed.insert(number.clone(), name.to_string());
        out.insert(number, verdict(&status_body(&text)));
    }
    if out.is_empty() {
        return Err(format!(
            "the ADR register at {} holds no ADRs that `git` tracks — the gate cannot \
             resolve a single citation, which is the gate being broken rather than the \
             documentation (the directory moved, or the root is wrong)",
            dir.display()
        )
        .into());
    }
    Ok(out)
}

/// Every path `git` tracks under `docs/adr`, repo-relative.
///
/// `git ls-files` rather than `std::fs::read_dir`, so the register holds
/// what a CI checkout holds — see the module docs. `-z` because git
/// C-quotes unusual names otherwise, and a silently dropped ADR is a
/// register that reports "no such ADR" about a decision that exists.
fn tracked_adrs(repo_root: &Path) -> Result<Vec<String>, Box<dyn Error>> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["ls-files", "-z", "--", DIR])
        .output()
        .map_err(|e| format!("could not list the ADR register (is `git` on PATH?): {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "git ls-files failed in {}: {}",
            repo_root.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        )
        .into());
    }
    Ok(String::from_utf8(out.stdout)?
        .split('\0')
        .filter(|path| !path.is_empty())
        .map(str::to_string)
        .collect())
}

/// The body under the `## Status` heading, up to the next heading.
///
/// Empty when the page has no such section, which [`verdict`] reads as
/// [`Verdict::Unknown`] — the same answer as an unreadable one, because
/// both mean the decision's standing cannot be established from the
/// file.
fn status_body(text: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let Some(start) = lines.iter().position(|line| is_status_heading(line)) else {
        return String::new();
    };
    let rest = &lines[start + 1..];
    let mut body = String::new();
    for (offset, line) in rest.iter().enumerate() {
        if ends_status_section(line) {
            break;
        }
        // A setext heading is declared by the line *below* it, so it is
        // recognised here rather than in [`ends_status_section`], whose
        // per-line signature cannot see one. Without this the section
        // runs on through `Accepted approach` / `===`, and under an
        // empty `## Status` that swallowed heading becomes the ADR's
        // verdict — the same defect the ATX widening closed, in the one
        // spelling of "heading" that widening could not reach.
        if !line.trim().is_empty()
            && rest
                .get(offset + 1)
                .is_some_and(|next| is_setext_rule(next))
        {
            break;
        }
        body.push_str(line);
        body.push('\n');
    }
    body
}

/// Whether `line` is a setext underline: nothing but `=` or nothing but
/// `-`, which makes the non-blank line above it a heading.
///
/// One character is enough — CommonMark asks for one or more, and a
/// lone `-` under a paragraph is an h2, not a list. The rule is only
/// consulted when the line above is non-blank, which is what keeps a
/// `---` thematic break (blank line above) and a `|---|---|` table
/// separator (not all one character) out of it.
fn is_setext_rule(line: &str) -> bool {
    let rest = line.trim();
    !rest.is_empty() && (rest.chars().all(|c| c == '=') || rest.chars().all(|c| c == '-'))
}

/// Whether a line closes the `## Status` section.
///
/// Any **ATX** heading ends it — `###` as readily as `##`, and the next
/// `##` most of all — but "a heading" is not `starts_with('#')`. A
/// setext heading is a heading too and is not judged here, because it
/// is declared by the line below it and this signature sees one line;
/// [`status_body`] handles that case with the lookahead it has.
/// CommonMark
/// allows an ATX heading up to three spaces of indentation, and a
/// heading inside a blockquote is still a heading, so
/// `   ### Accepted approach` and `> ## Accepted approach` were read as
/// *body*. Under a `## Status` that is empty that swallowed heading
/// became the ADR's verdict, because [`verdict`] trims `#` as
/// decoration like every other Markdown character: an ADR with no
/// status read `Accepted`, and one followed by
/// `   #### Superseded discussion` read `Superseded` — a hard finding
/// on every page citing it. Widening the *section* rule is the fix
/// rather than carving `#` out of [`verdict`]'s trim class, because the
/// mis-scoping is real on its own (the body genuinely ends there) and
/// because listing an exception to that class is the thing the class
/// exists not to do — a sixth spelling of `Accepted` must stay free.
///
/// Four or more leading spaces is an indented code block rather than a
/// heading, and ends the section here anyway. The cost is a body
/// truncated early, whose verdict is [`Verdict::Unknown`] — a finding,
/// and so loud — while the cost of the other answer is a wrong verdict
/// read in silence.
fn ends_status_section(line: &str) -> bool {
    let mut rest = line.trim_start();
    while let Some(stripped) = rest.strip_prefix('>') {
        rest = stripped.trim_start();
    }
    rest.starts_with('#')
}

/// `^##\s+Status\s*$`, spelled out: exactly two hashes, whitespace, and
/// nothing but the word.
///
/// Exact rather than "a heading containing Status", so that a section
/// named `## Status of the rollout` in some future ADR is not read as
/// the verdict block.
///
/// The scan is line-local and not fence-aware, so a `## Status` quoted
/// inside a fenced block would open the section — an ADR demonstrating
/// the template in a fence would take its verdict from whatever prose
/// followed. Measured rather than assumed: across the 59 tracked files
/// under `docs/adr/` the first matching line is inside a fence in none
/// of them, and every numbered ADR has a real Status section. Recorded
/// because the reach is zero today, not because it cannot change.
fn is_status_heading(line: &str) -> bool {
    let Some(rest) = line.strip_prefix("##") else {
        return false;
    };
    if !rest.starts_with([' ', '\t']) {
        return false;
    }
    rest.trim() == "Status"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_status_section_ends_at_the_next_heading() {
        // The body must not swallow the sections below it. `verdict`
        // reads the first token, so this matters most where the Status
        // section is EMPTY: with `#` trimmed as decoration like any
        // other, a swallowed heading — or the prose under it — is read
        // as the verdict of an ADR whose status is in fact missing.
        let text = "# ADR 0011\n\n## Status\n\n`Superseded by ADR 0016`.\n\n\
                    ## Context\n\nAccepted 2026-05-06 as the shim.\n";
        assert_eq!(status_body(text).trim(), "`Superseded by ADR 0016`.");
        assert_eq!(verdict(&status_body(text)), Verdict::Superseded);

        // The heading shapes `starts_with('#')` missed, and they are
        // the ones that matter: an unindented `### Accepted approach`
        // was already stopped at, so pinning the hazard on THAT form
        // passed for a reason unrelated to the hazard. CommonMark takes
        // up to three spaces of indentation on an ATX heading, and a
        // heading in a blockquote is a heading.
        //
        // Both directions are pinned, because they fail differently: a
        // swallowed `Accepted` reads as a decision that was never
        // recorded (silence where a finding belongs), and a swallowed
        // `Superseded` reports a hard finding on every page citing an
        // ADR that is merely missing its status.
        for empty in [
            "# ADR 0099\n\n## Status\n\n### Accepted approach\n\nIt was fine.\n",
            "# ADR 0099\n\n## Status\n\n   ### Accepted approach\n\nIt was fine.\n",
            "# ADR 0099\n\n## Status\n\n> ## Accepted approach\n\nIt was fine.\n",
            "# ADR 0099\n\n## Status\n\n   #### Superseded discussion\n\nprose.\n",
            // Setext: the heading is declared by the line BELOW it, so
            // no per-line rule can see one. Both underlines, and the
            // one-character form CommonMark also allows — a lone `-`
            // under a paragraph is an h2, not a list item.
            "# ADR 0099\n\n## Status\n\nAccepted approach\n=================\n\nprose.\n",
            "# ADR 0099\n\n## Status\n\nSuperseded approach\n-------------------\n\nprose.\n",
            "# ADR 0099\n\n## Status\n\nAccepted approach\n-\n\nprose.\n",
        ] {
            assert!(
                status_body(empty).trim().is_empty(),
                "{empty:?}: {:?}",
                status_body(empty)
            );
            assert_eq!(verdict(&status_body(empty)), Verdict::Unknown, "{empty:?}");
        }

        // And the negative control the widening must not cost: a
        // blockquoted or indented verdict is still a verdict, because
        // the section ends on a heading, not on decoration.
        for body in [
            "# ADR 0099\n\n## Status\n\n> `Accepted` (2026-05-12).\n",
            "# ADR 0099\n\n## Status\n\n   Accepted (2026-05-12).\n",
            // A `---` under a BLANK line is a thematic break, not a
            // setext underline — which is the whole reason the setext
            // rule only fires when the line above it has content.
            "# ADR 0099\n\n## Status\n\n`Accepted` (2026-05-12).\n\n---\n\nprose.\n",
            // A table separator is not all one character, so a Status
            // body may hold a table without losing its verdict.
            "# ADR 0099\n\n## Status\n\n`Accepted` (2026-05-12).\n\n| a | b |\n|---|---|\n| 1 | 2 |\n",
        ] {
            assert_eq!(verdict(&status_body(body)), Verdict::Accepted, "{body:?}");
        }
    }

    #[test]
    fn a_page_without_a_status_section_reads_as_unknown() {
        // Recorded, not omitted: see `statuses`' docs on why an absent
        // key would tell the reader something false.
        let text = "# ADR 0099\n\n## Context\n\nSomething.\n";
        assert!(status_body(text).is_empty());
        assert_eq!(verdict(&status_body(text)), Verdict::Unknown);
    }

    #[test]
    fn only_the_status_heading_itself_opens_the_section() {
        assert!(is_status_heading("## Status"));
        assert!(is_status_heading("##  Status  "));
        assert!(!is_status_heading("### Status"));
        assert!(!is_status_heading("##Status"));
        assert!(!is_status_heading("## Status of the rollout"));
        assert!(!is_status_heading("# Status"));
    }
}
