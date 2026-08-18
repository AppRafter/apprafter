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
//! # Two spellings, and why one checker cannot know only one
//!
//! The corpus writes the reference two ways. Over the 33 in-scope pages
//! there are **62 references across 22 distinct ADRs**: 57 spelled
//! `ADR 0046`, 4 spelled `ADR-0032` — all four of those in
//! `docs/license.md`, as `Pre-ADR-0032` / `Post-ADR-0032` — and one
//! wrapped across a line break (below). Over every tracked `*.md` the
//! split is 952 and 19. A checker written against the space form alone
//! would skip `docs/license.md` entirely **and report success**, which
//! is the failure mode a gate must not have: a silent blind spot reads
//! exactly like a clean page.
//!
//! So [`references`] accepts a space **or** a hyphen. The left boundary
//! is "not a letter or digit" rather than "whitespace" precisely because
//! of `Pre-ADR-0032`: the character before those three letters is a
//! hyphen, and the reference is real.
//!
//! # The wrapped reference
//!
//! `docs/operator-guide/target-store.md:8` writes the citation across a
//! blockquote line break:
//!
//! ```text
//! > Authoritative design rationale: [ADR
//! > 0030](../adr/0030-cli-target-store-and-credential-chain.md).
//! ```
//!
//! A line-local scan finds 61 of the 62 and is silent about this one.
//! That is one reference, and it would not matter today — 0030 is
//! Accepted — except that "wrap the line" is then a one-keystroke way to
//! take a citation out of the gate's sight, with nothing in the diff
//! that reads as suppression. So a line ending in a bare `ADR` is joined
//! to the next line, after its leading whitespace and blockquote
//! markers, and the reference is reported on the line the citation
//! *starts* — the same rule
//! [`crate::invocation::logical_lines`] applies to a command whose flags
//! continue on the next line.
//!
//! # A leading token, not a substring
//!
//! Status lives under a `## Status` **heading** rather than on a
//! `Status:` line, and its body opens with a verdict token followed by
//! prose. That structure is what makes partial supersession safe to
//! distinguish, and it has to be distinguished, because three ADRs in
//! this repository are amended rather than withdrawn:
//!
//! | ADR | its `## Status` opens | the decision | in-scope references |
//! |---|---|---|---|
//! | 0011 | ``Superseded by ADR 0016``. Originally accepted 2026-05-06… | gone | 0 |
//! | 0001 | ``Accepted``. Date: 2026-05-06. ``Superseded`` by ADR 0032 (for the core base license choice; plugin MIT carve-out stands). | **stands** | 3 |
//! | 0042 | ``Accepted`` (2026-06-05). §7 superseded in part by ADR 0046 | **stands** | 1 |
//! | 0053 | ``Accepted`` (2026-08-08). §3 superseded in part by ADR 0055 | **stands** | 3 |
//!
//! A rule that looks for "superseded" anywhere in the block fires on the
//! last three — **7 in-scope references, not one of them a defect** —
//! against **zero** true positives in the corpus. A gate whose entire
//! observed output is false is a gate that gets switched off within the
//! week. Reading only the **first word** gets all four rows right, and
//! is what [`verdict`] does.
//!
//! # What is a finding, and what is not
//!
//! `Superseded` and `Deprecated` are findings: the decision was
//! reversed. `Unused` is a finding for a different reason — `0013` and
//! `0018` are slots reserved during early planning and abandoned, so the
//! number names nothing at all, exactly like a typo.
//!
//! `Draft` is **not** a finding, and this is a deliberate policy rather
//! than an omission: 9 of the 62 in-scope references point at ADRs
//! 0025–0029, all Draft, and documentation legitimately cites a decision
//! the project is working to. `Proposed` is not a finding for the same
//! reason. A gate that demanded `Accepted` would be demanding that nine
//! correct sentences be deleted.
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
//! **every** ADR in `docs/adr/` today parses to a known verdict
//! (`the_register_reads_this_repositorys_own_adrs` pins that), so this
//! arm reports nothing until someone invents a new word — which is the
//! moment it exists for. Widening the vocabulary is a one-line edit in
//! [`verdict`], and that edit is the review this check is asking for.
//!
//! # `0000-template.md` is skipped
//!
//! Its Status body is the literal enum menu — ``Proposed`` | ``Draft`` |
//! ``Accepted`` | ``Deprecated`` | ``Superseded by ADR NNNN`` — so
//! reading it puts a `Proposed` ADR 0000 in the register and makes the
//! menu look like a decision. It is excluded by name. `README.md` in the
//! same directory is dropped by shape: an ADR file is named for its
//! number, and a file whose name does not open on four digits is not
//! one.

use std::collections::BTreeMap;
use std::error::Error;
use std::path::Path;

/// The ADR directory, relative to the repository root.
const DIR: &str = "docs/adr";
/// The one file in it that is not an ADR but is named like one.
const TEMPLATE: &str = "0000-template.md";

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
    pub fn as_str(self) -> &'static str {
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
/// substring match on "superseded" would report 7 correct references and
/// no incorrect one.
pub fn verdict(status_body: &str) -> Verdict {
    let Some(word) = status_body.split_whitespace().next() else {
        return Verdict::Unknown;
    };
    // Backticks, asterisks, underscores, brackets and the trailing
    // `.`/`,`/`:` are Markdown decoration around the token, never part
    // of it. Trimming a character class rather than listing the
    // decorations means a sixth way of writing `Accepted` costs nothing.
    let bare = word.trim_matches(|c: char| !c.is_ascii_alphanumeric());
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
/// Anchored on both sides: `ADR` must not continue a word to its left
/// (`PADR 0046` is not one; `Pre-ADR-0032` is, because a hyphen is not a
/// letter), and the number must be **exactly** four digits with no
/// alphanumeric after them — `ADR 00461` names some other number that
/// happens to follow those letters, and `ADRs 0034` is a plural noun
/// with a list after it rather than a citation of ADR 0034.
pub fn references(src: &str) -> Vec<AdrRef> {
    let lines: Vec<&str> = src.lines().collect();
    let mut out = Vec::new();
    for (index, text) in lines.iter().enumerate() {
        let line = index + 1;
        scan_line(text, line, &mut out);
        // The wrapped shape, joined rather than missed: see the module
        // docs. Only a line whose LAST token is a bare `ADR` can start
        // one, so this never re-reads a reference `scan_line` took.
        if ends_on_adr(text) {
            if let Some(number) = lines.get(index + 1).and_then(|next| opens_on_number(next)) {
                out.push(AdrRef { line, number });
            }
        }
    }
    out
}

/// Every citation written wholly on one line.
fn scan_line(text: &str, line: usize, out: &mut Vec<AdrRef>) {
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
        let separator = at + 3;
        if !matches!(bytes.get(separator), Some(b' ') | Some(b'-')) {
            at += 3;
            continue;
        }
        match four_digits(bytes, separator + 1) {
            Some(number) => {
                out.push(AdrRef { line, number });
                at = separator + 5;
            }
            // Not a citation, but `ADR` might still be the start of the
            // next one on this line, so advance past these three letters
            // only.
            None => at = separator,
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

/// Whether the line's last token is a bare `ADR`, so the number may be
/// on the next one.
fn ends_on_adr(text: &str) -> bool {
    let Some(head) = text.trim_end().strip_suffix("ADR") else {
        return false;
    };
    !head
        .as_bytes()
        .last()
        .is_some_and(u8::is_ascii_alphanumeric)
}

/// The number a continuation line opens on, blockquote markers and
/// indentation stripped — `> 0030](…)` continues `[ADR`.
fn opens_on_number(text: &str) -> Option<String> {
    let mut rest = text.trim_start();
    while let Some(stripped) = rest.strip_prefix('>') {
        rest = stripped.trim_start();
    }
    four_digits(rest.as_bytes(), 0)
}

/// Every ADR in the repository, by number, with the verdict its
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
pub fn statuses(repo_root: &Path) -> Result<BTreeMap<String, Verdict>, Box<dyn Error>> {
    let dir = repo_root.join(DIR);
    let entries = std::fs::read_dir(&dir)
        .map_err(|e| format!("could not read the ADR register at {}: {e}", dir.display()))?;
    let mut out = BTreeMap::new();
    for entry in entries {
        let path = entry
            .map_err(|e| format!("could not read the ADR register at {}: {e}", dir.display()))?
            .path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
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
        let text = std::fs::read_to_string(&path)
            .map_err(|e| format!("could not read {}: {e}", path.display()))?;
        out.insert(number, verdict(&status_body(&text)));
    }
    if out.is_empty() {
        return Err(format!(
            "the ADR register at {} holds no ADRs — the gate cannot resolve a single \
             citation, which is the gate being broken rather than the documentation",
            dir.display()
        )
        .into());
    }
    Ok(out)
}

/// The body under the `## Status` heading, up to the next heading.
///
/// Empty when the page has no such section, which [`verdict`] reads as
/// [`Verdict::Unknown`] — the same answer as an unreadable one, because
/// both mean the decision's standing cannot be established from the
/// file.
fn status_body(text: &str) -> String {
    let mut lines = text.lines();
    if !lines.any(is_status_heading) {
        return String::new();
    }
    let mut body = String::new();
    for line in lines {
        // Any heading ends the section — `###` as readily as `##`, and
        // the next `##` most of all.
        if line.starts_with('#') {
            break;
        }
        body.push_str(line);
        body.push('\n');
    }
    body
}

/// `^##\s+Status\s*$`, spelled out: exactly two hashes, whitespace, and
/// nothing but the word.
///
/// Exact rather than "a heading containing Status", so that a section
/// named `## Status of the rollout` in some future ADR is not read as
/// the verdict block.
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
        // The body must not swallow the sections below it: `## Context`
        // in a superseded ADR routinely says "Accepted at the time",
        // which reads as a verdict to anything that keeps scanning.
        let text = "# ADR 0011\n\n## Status\n\n`Superseded by ADR 0016`.\n\n\
                    ## Context\n\nAccepted 2026-05-06 as the shim.\n";
        assert_eq!(status_body(text).trim(), "`Superseded by ADR 0016`.");
        assert_eq!(verdict(&status_body(text)), Verdict::Superseded);
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
