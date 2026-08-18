// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The two halves of the ADR check, in isolation: what counts as a
//! reference, and what a `## Status` block says about the decision.
//!
//! Both are about *reading*, not about policy — the gate decides which
//! verdicts are findings, and it is tested in `gate_test.rs`. Split that
//! way because the interesting failures here are grammar failures: a
//! spelling the extractor does not know is a reference nobody checks,
//! and a status the parser reads too eagerly is a correct page reported
//! as wrong.

use docsgen::adr::{references, statuses, verdict, Verdict};

#[test]
fn both_spellings_are_found() {
    let r = references("Per ADR 0046 and ADR-0027, the field is optional.\n");
    assert_eq!(r.len(), 2, "{r:?}");
    assert_eq!(r[0].number, "0046");
    assert_eq!(r[1].number, "0027");
    assert_eq!(r[0].line, 1);
}

#[test]
fn a_leading_superseded_is_superseded() {
    assert_eq!(
        verdict("`Superseded by ADR 0016`. Originally accepted 2026-05-06."),
        Verdict::Superseded
    );
}

#[test]
fn supersession_in_part_leaves_the_adr_accepted() {
    let body = "`Accepted` (2026-06-05). **§7 (connection contract) superseded \
                in part by [ADR 0046](0046-env-value-references.md)**";
    assert_eq!(verdict(body), Verdict::Accepted);
}

#[test]
fn a_later_supersession_sentence_leaves_the_adr_accepted() {
    let body = "`Accepted`. Date: 2026-05-06. `Superseded` by ADR 0032 \
                (for the core base license choice; plugin MIT carve-out stands).";
    assert_eq!(verdict(body), Verdict::Accepted);
}

#[test]
fn draft_and_proposed_are_their_own_verdicts() {
    assert_eq!(verdict("Draft."), Verdict::Draft);
    assert_eq!(verdict("`Proposed`"), Verdict::Proposed);
}

#[test]
fn an_unused_slot_is_its_own_verdict() {
    assert_eq!(
        verdict("Unused. Slot reserved during early planning, KEDA became ADR 0019"),
        Verdict::Unused
    );
}

#[test]
fn a_bare_accepted_with_a_parenthesised_date_is_accepted() {
    assert_eq!(verdict("Accepted (2026-05-12)."), Verdict::Accepted);
}

// ---- cases the seven above leave open ----------------------------------

#[test]
fn a_verdict_the_vocabulary_does_not_hold_is_not_guessed() {
    // Added because `Unknown` is a finding, so this is the arm that
    // decides whether an ADR written in a new dialect is reported or
    // silently read as standing. Guessing "it starts with a word, so
    // presumably fine" is how the vocabulary drifts without anyone
    // choosing to widen it.
    assert_eq!(verdict("Ratified 2026-05-12."), Verdict::Unknown);
    assert_eq!(verdict(""), Verdict::Unknown);
    assert_eq!(verdict("   \n\n"), Verdict::Unknown);
    // The template's enum menu — read, it would say `Proposed` for every
    // ADR that copied it. It is excluded by filename instead, which
    // `the_register_skips_the_template` pins.
    assert_eq!(
        verdict("`Proposed` | `Draft` | `Accepted` | `Deprecated` | `Superseded by ADR NNNN`"),
        Verdict::Proposed
    );
}

#[test]
fn the_vocabulary_is_read_whatever_the_case_and_the_decoration() {
    // Added because the corpus writes the same verdict five ways —
    // `Accepted`, Accepted, `Accepted` (date), **bold** in the amended
    // ones — and a reader adding a sixth must not silently produce
    // `Unknown`.
    for body in [
        "accepted",
        "ACCEPTED.",
        "**`Accepted`**, see below",
        "_Accepted_:",
    ] {
        assert_eq!(verdict(body), Verdict::Accepted, "{body:?}");
    }
}

#[test]
fn adr_inside_a_longer_word_is_not_a_reference() {
    // Added because the extractor is anchored on both sides. The left
    // anchor is not hypothetical: `docs/license.md` writes
    // `Pre-ADR-0032` and `Post-ADR-0032` four times, and those ARE
    // references — the character before `ADR` is a hyphen, not a
    // letter — so the boundary rule has to admit them while rejecting a
    // word that merely ends in those three letters.
    assert!(references("A PADR 0046 gadget.\n").is_empty());
    assert!(references("A QADR-0046 gadget.\n").is_empty());
    let r = references("Pre-ADR-0032 releases shipped under MIT.\n");
    assert_eq!(r.len(), 1, "{r:?}");
    assert_eq!(r[0].number, "0032");
}

#[test]
fn a_number_that_is_not_exactly_four_digits_is_not_a_reference() {
    // Added because "exactly four" is the whole grammar: a longer run is
    // some other number that happens to follow the letters, and a
    // shorter one is not an ADR name.
    assert!(references("ADR 00461 is not one.\n").is_empty(), "too long");
    assert!(references("ADR 046 is not one.\n").is_empty(), "too short");
    assert!(references("ADR 0046a is not one.\n").is_empty(), "suffixed");
    assert!(references("ADRs 0034 and 0038.\n").is_empty(), "plural");
    // And the boundary that must still resolve: punctuation after the
    // number is how every corpus reference ends.
    for text in ["ADR 0046.", "ADR 0046,", "[ADR 0046](x.md)", "(ADR-0046)"] {
        let r = references(text);
        assert_eq!(r.len(), 1, "{text:?}: {r:?}");
        assert_eq!(r[0].number, "0046", "{text:?}");
    }
}

#[test]
fn a_reference_wrapped_across_a_line_break_is_still_a_reference() {
    // Added because the corpus holds exactly one, and it would otherwise
    // be the single reference nobody checks:
    // `docs/operator-guide/target-store.md:8` wraps `[ADR` / `> 0030]`
    // across a blockquote line break. A line-local scan alone reports 61
    // of the 62 references in the corpus and is silent about the 62nd —
    // and "wrap the line" is a one-keystroke way to hide a citation.
    let src = "> Authoritative design rationale: [ADR\n\
               > 0030](../adr/0030-cli-target-store-and-credential-chain.md).\n";
    let r = references(src);
    assert_eq!(r.len(), 1, "{r:?}");
    assert_eq!(r[0].number, "0030");
    // On the line the citation starts, as an invocation's flags are
    // reported on the line its command started.
    assert_eq!(r[0].line, 1);

    // A wrap that is not one: the next line has to open on the number.
    let prose = "The decision is an ADR\nand it lives in 0030-something.md\n";
    assert!(references(prose).is_empty(), "{prose:?}");
}

#[test]
fn every_reference_on_a_line_is_found_in_the_order_it_is_written() {
    // Added because a scanner that advances past the whole line after
    // its first hit passes `both_spellings_are_found` only by accident
    // of that test having two references; three catches the off-by-one
    // shapes, and a finding list is read against the page.
    let r = references("x\nSee ADR 0025, ADR 0026 and ADR-0027 for the surface.\n");
    let numbers: Vec<&str> = r.iter().map(|found| found.number.as_str()).collect();
    assert_eq!(numbers, ["0025", "0026", "0027"], "{r:?}");
    assert!(r.iter().all(|found| found.line == 2), "{r:?}");
}

// ---- the register, read from this repository ---------------------------

#[test]
fn the_register_reads_this_repositorys_own_adrs() {
    // The one case that has to be anchored on the real `docs/adr/`: the
    // parser above can be right about a string and still wrong about the
    // files, and every verdict the gate acts on comes from these.
    let root = docsgen::repo_root().unwrap();
    let register = statuses(&root).unwrap();

    assert_eq!(register.get("0011"), Some(&Verdict::Superseded), "0011");
    assert_eq!(register.get("0013"), Some(&Verdict::Unused), "0013");
    assert_eq!(register.get("0018"), Some(&Verdict::Unused), "0018");
    assert_eq!(register.get("0027"), Some(&Verdict::Draft), "0027");
    // The three the "superseded in part" rule exists for.
    for number in ["0001", "0042", "0053"] {
        assert_eq!(
            register.get(number),
            Some(&Verdict::Accepted),
            "{number} is amended, not withdrawn"
        );
    }
    // And nothing in the register is unreadable: `Unknown` is a finding,
    // so an ADR written in a dialect the parser does not know would
    // report on every page that cites it.
    let unknown: Vec<&String> = register
        .iter()
        .filter(|(_, status)| **status == Verdict::Unknown)
        .map(|(number, _)| number)
        .collect();
    assert!(unknown.is_empty(), "unreadable `## Status`: {unknown:?}");
}

#[test]
fn the_register_skips_the_template() {
    // `0000-template.md`'s Status body is the literal enum menu, so
    // reading it would put a `Proposed` ADR 0000 in the register and
    // make the menu look like a decision.
    let root = docsgen::repo_root().unwrap();
    let register = statuses(&root).unwrap();
    assert_eq!(register.get("0000"), None, "{register:?}");
    // The index page is not an ADR either, and it has no number to key
    // it by — the shape rule that drops it drops nothing else.
    assert!(register.len() > 50, "{}", register.len());
}
