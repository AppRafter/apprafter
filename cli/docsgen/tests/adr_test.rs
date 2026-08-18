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
use std::collections::BTreeSet;
use std::path::Path;
use std::process::Command;
use tempfile::TempDir;

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
    // The one decoration that is still a token of its own: `1` is
    // alphanumeric, so it survives trimming. Skipping numeric tokens
    // to reach the word behind them would make `2026-05-06 Accepted`
    // read as a verdict, which is the worse answer of the two.
    assert_eq!(verdict("1. Accepted"), Verdict::Unknown);
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
    //
    // The blockquote and list forms are here because they were the
    // counter-example: reading the FIRST token and trimming afterwards
    // left `>` and `-` trimmed to the empty string, so `> Accepted`
    // fell through to `Unknown` — a hard finding, with no ignore key,
    // on every page citing that ADR, sent to fix a vocabulary that was
    // never the problem. The property this test claims is the one the
    // module doc claims: a new way of writing `Accepted` costs nothing.
    for body in [
        "accepted",
        "ACCEPTED.",
        "**`Accepted`**, see below",
        "_Accepted_:",
        "> Accepted.",
        ">  `Accepted` (2026-05-12).",
        "- Accepted",
        "* `Accepted`",
        "(Accepted)",
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
    // And the boundary that must still resolve: punctuation after the
    // number is how every corpus reference ends.
    for text in ["ADR 0046.", "ADR 0046,", "[ADR 0046](x.md)", "(ADR-0046)"] {
        let r = references(text);
        assert_eq!(r.len(), 1, "{text:?}: {r:?}");
        assert_eq!(r[0].number, "0046", "{text:?}");
    }
}

#[test]
fn the_separator_may_be_a_run_of_spaces_or_a_tab() {
    // Added because a second space was a one-keystroke way to take a
    // citation out of sight — the same suppression the wrap rule exists
    // to close, left open by a grammar that accepted exactly one space.
    // The corpus writes neither of these; the point is that it cannot
    // start to.
    for text in ["ADR  0046", "ADR\t0046", "ADR \t 0046."] {
        let r = references(text);
        assert_eq!(r.len(), 1, "{text:?}: {r:?}");
        assert_eq!(r[0].number, "0046", "{text:?}");
    }
    // The hyphen form stays exactly one hyphen: `Pre-ADR-0032` depends
    // on it, and a run of hyphens is a rule, not a separator.
    assert!(references("ADR--0046").is_empty());
}

#[test]
fn the_plural_cites_its_first_number() {
    // The justification for excluding `ADRs` used to be that it is "a
    // plural noun with a list after it rather than a citation". It is
    // not: `README.md:121` writes `and ADRs [0001](…) and [0032](…)`,
    // `spec.md:1306` writes "ADRs 0025–0029 reframe cluster-bootstrap"
    // and `docs/adr/0029-cue-cmp.md:9` writes "ADRs 0025–0028 address
    // the platform-stack side". Excluding it also made a trailing `s` a
    // suppression.
    let r = references("ADRs 0034 and 0038.\n");
    let numbers: Vec<&str> = r.iter().map(|found| found.number.as_str()).collect();
    // Only the first: continuing the list would mean reading a bare
    // four-digit run in prose as a citation, and the commonest
    // four-digit run in this corpus is a year.
    assert_eq!(numbers, ["0034"], "{r:?}");
}

#[test]
fn a_citation_written_only_as_a_link_or_a_path_is_a_reference() {
    // Added because four corpus citations carry no `ADR NNNN` text at
    // all, and they are exactly the form the first version of this
    // check's remedy told authors to write. The number is in every
    // ADR's filename, so this costs one more scan.
    let link = references("See [the original shim](../adr/0011-hybrid-rust-sdk-tofu-shim.md).\n");
    assert_eq!(link.len(), 1, "{link:?}");
    assert_eq!(link[0].number, "0011");

    let span = references("- `docs/adr/0032-license-fsl-1-1-apache-2-0.md` — base license\n");
    assert_eq!(span.len(), 1, "{span:?}");
    assert_eq!(span[0].number, "0032");

    // One citation spelled both ways on one line is ONE reference —
    // otherwise the corpus's ordinary link shape reports every problem
    // twice.
    let both = references("- [ADR 0046](../adr/0046-env-value-references.md) — env values\n");
    assert_eq!(both.len(), 1, "{both:?}");
    assert_eq!(both[0].number, "0046");

    // `README.md` is the index of the ADR directory, not a decision,
    // and has no number to key it by.
    assert!(references("[the ADRs](../adr/README.md)").is_empty());
}

#[test]
fn a_reference_wrapped_across_a_line_break_is_still_a_reference() {
    // Added because the corpus holds exactly one, and it would otherwise
    // be the single reference nobody checks:
    // `docs/operator-guide/target-store.md:8` wraps `[ADR` / `> 0030]`
    // across a blockquote line break. A line-local scan alone reports 61
    // of the 62 IN-GRAMMAR citations and is silent about the 62nd — the
    // corpus holds 66, the other 4 carrying no `ADR NNNN` text at all —
    // and "wrap the line" is a one-keystroke way to hide a citation.
    let src = "> Authoritative design rationale: [ADR\n\
               > 0030](../adr/0030-cli-target-store-and-credential-chain.md).\n";
    let r = references(src);
    assert_eq!(r.len(), 1, "{r:?}");
    assert_eq!(r[0].number, "0030");
    // On the line the citation starts, as an invocation's flags are
    // reported on the line its command started.
    assert_eq!(r[0].line, 1);
    // And exactly one: the continuation line IS the link target, so the
    // filename shape must not report the same citation a second time,
    // on the line below.

    // The other half of that dedup, on ONE line rather than across two:
    // a line may cite a number inline and also open a wrap for the same
    // number. The wrapped push sat outside the per-line `seen` gate, so
    // this yielded two identical `AdrRef`s — two findings a reader
    // cannot tell apart or fix separately. Zero corpus occurrences; it
    // is the shape of the bug that makes it worth pinning.
    let twice = "See ADR 0030 and also [ADR\n0030](../adr/0030-x.md).\n";
    let r = references(twice);
    assert_eq!(r.len(), 1, "{r:?}");
    assert_eq!((r[0].line, r[0].number.as_str()), (1, "0030"));

    // A wrap that is not one: the next line has to open on the number.
    let prose = "The decision is an ADR\nand it lives in 0030-something.md\n";
    assert!(references(prose).is_empty(), "{prose:?}");

    // The other bracket: a citation whose number carries the `]`.
    let closing =
        "Rationale: see ADR\n0030](../adr/0030-cli-target-store-and-credential-chain.md).\n";
    let r = references(closing);
    assert_eq!(r.len(), 1, "{r:?}");
    assert_eq!(r[0].line, 1);
}

#[test]
fn a_line_ending_on_the_word_adr_does_not_swallow_the_next_number() {
    // The negative control the wrap rule was missing. `ADR` is an
    // ordinary English noun in this corpus —
    // `docs/operator-guide/index.md:54` writes "An ADR describes the
    // world as it was when it was ratified" — and a year is the
    // commonest four-digit run in it, so an unanchored join reported
    // `ADR 2026`: no such ADR, a hard finding on a correct sentence.
    let year = "- Every decision is recorded as an ADR\n  2026-05-06 is the earliest one.\n";
    assert!(references(year).is_empty(), "{:?}", references(year));

    let port = "The exporter is reached through the ADR\n9090 is the port.\n";
    assert!(references(port).is_empty(), "{:?}", references(port));

    // The join is bracketed on one side or the other, so an
    // unbracketed prose wrap is given up with it. That is the trade:
    // the corpus writes the bracketed shape, and a false positive is a
    // page the gate is wrong about while this is one it is quiet about.
    let bare = "Storage follows ADR\n0011.\n";
    assert!(references(bare).is_empty(), "{:?}", references(bare));
}

#[test]
fn the_corpus_citations_that_carry_no_adr_text_are_found() {
    // The one case that has to be anchored on real pages: `README.md`
    // cites two ADRs and writes neither as `ADR NNNN` — it writes
    // `and ADRs [0001](./docs/adr/0001-…md) … and [0032](…)`. Under a
    // grammar that reads the `ADR NNNN` text alone, `README.md` held
    // zero citations and reported clean, which is what a blind spot
    // looks like from the outside.
    let root = docsgen::repo_root().unwrap();
    let readme = std::fs::read_to_string(root.join("README.md")).unwrap();
    let numbers: BTreeSet<String> = references(&readme)
        .into_iter()
        .map(|found| found.number)
        .collect();
    assert!(numbers.contains("0001"), "{numbers:?}");
    assert!(numbers.contains("0032"), "{numbers:?}");

    // `docs/license.md` writes the same two as bare paths in code
    // spans, alongside the `Pre-ADR-0032` text form.
    let license = std::fs::read_to_string(root.join("docs/license.md")).unwrap();
    let cited: BTreeSet<String> = references(&license)
        .into_iter()
        .map(|found| found.number)
        .collect();
    assert!(cited.contains("0001"), "{cited:?}");
    assert!(cited.contains("0032"), "{cited:?}");
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
    // The four the "superseded in part" rule exists for — 0043's
    // Status reads "§1's env-injection naming superseded in part by
    // [ADR 0046]" and it carries 2 in-scope references, so leaving it
    // out let this file contradict the two docs that name all four.
    for number in ["0001", "0042", "0043", "0053"] {
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
fn the_register_holds_every_tracked_adr_and_nothing_else() {
    // `0000-template.md`'s Status body is the literal enum menu, so
    // reading it would put a `Proposed` ADR 0000 in the register and
    // make the menu look like a decision. `README.md` is the index, and
    // has no number to key it by.
    //
    // Asserted as a set derived from the listing the register is built
    // from, rather than as `len() > 50`: that bound would still have
    // passed after a bug that dropped six real ADRs, which is not the
    // property this test is here for.
    let root = docsgen::repo_root().unwrap();
    let register = statuses(&root).unwrap();
    let expected: BTreeSet<String> = tracked(&root, "docs/adr")
        .iter()
        .filter_map(|path| path.rsplit('/').next())
        .filter(|name| name.ends_with(".md") && *name != "0000-template.md")
        .filter_map(number_of)
        .collect();
    assert!(!expected.is_empty(), "the listing is empty — helper broken");
    let keys: BTreeSet<String> = register.keys().cloned().collect();
    assert_eq!(keys, expected);
    assert_eq!(register.get("0000"), None, "the template is not a decision");
}

// ---- the register as breakage ------------------------------------------

#[test]
fn a_register_holding_no_adrs_is_an_error() {
    // The branch that keeps a moved or emptied `docs/adr/` from reading
    // as "every citation in the corpus names a nonexistent ADR" —
    // dozens of findings, all false, none fixable by editing a page.
    // Untested until now, which is how a whole-module argument comes to
    // rest on a branch nobody ran.
    let repo = register_repo(&[
        ("0000-template.md", "## Status\n\n`Proposed` | `Draft`\n"),
        ("README.md", "# Index\n"),
    ]);
    let e = statuses(repo.path()).unwrap_err().to_string();
    assert!(e.contains("docs/adr"), "{e}");
    assert!(e.contains("holds no ADRs"), "{e}");
}

#[test]
fn a_register_directory_that_is_not_there_is_an_error() {
    // A missing directory has no branch of its own: `git ls-files`
    // returns nothing for it, exactly as it does for one holding only
    // the template, so both land on the empty-register `Err`. That is
    // deliberate — the reader's problem is identical either way, and
    // the message names the path so they can see which it is — but it
    // means this test walks its sibling's code path, and asserting only
    // that the path is mentioned would be satisfied by any unrelated
    // error that happened to name it.
    let repo = register_repo(&[]);
    let e = statuses(repo.path()).unwrap_err().to_string();
    assert!(e.contains("docs/adr"), "{e}");
    assert!(e.contains("holds no ADRs"), "{e}");
}

#[test]
fn two_files_claiming_one_number_is_an_error() {
    // `read_dir` returned entries in an unspecified order, so a
    // scratch copy beside the original resolved to whichever came
    // last — and when that was `Accepted`, every citation of a
    // superseded decision quietly stopped being reported, with nothing
    // saying so. A register that cannot say what a number names is the
    // gate being broken: exit 2, not a finding.
    let repo = register_repo(&[
        (
            "0011-original.md",
            "## Status\n\n`Superseded by ADR 0016`.\n",
        ),
        ("0011-zz-scratch-copy.md", "## Status\n\n`Accepted`.\n"),
    ]);
    let e = statuses(repo.path()).unwrap_err().to_string();
    assert!(e.contains("0011-original.md"), "{e}");
    assert!(e.contains("0011-zz-scratch-copy.md"), "{e}");
}

#[test]
fn an_adr_that_is_not_staged_is_not_in_the_register() {
    // The reason the register reads `git ls-files` and not the
    // directory: everything else the gate resolves against comes from
    // the index, and a register read off the worktree would resolve a
    // citation in the pre-commit hook and not in CI — the page passing
    // locally and failing on the pull request, with the citation named
    // as the defect rather than the missing `git add`.
    let repo = register_repo(&[("0011-staged.md", "## Status\n\n`Accepted`.\n")]);
    std::fs::write(
        repo.path().join("docs/adr/0056-unstaged.md"),
        "## Status\n\n`Accepted`.\n",
    )
    .unwrap();
    let register = statuses(repo.path()).unwrap();
    assert_eq!(register.get("0011"), Some(&Verdict::Accepted));
    assert_eq!(register.get("0056"), None, "{register:?}");
}

// ---- helpers -----------------------------------------------------------

/// The four digits an ADR filename opens on, by the same shape rule the
/// register uses — four digits, then anything that is not alphanumeric.
fn number_of(name: &str) -> Option<String> {
    let bytes = name.as_bytes();
    if bytes.len() < 4 || !bytes[..4].iter().all(u8::is_ascii_digit) {
        return None;
    }
    if bytes.get(4).is_some_and(u8::is_ascii_alphanumeric) {
        return None;
    }
    Some(name[..4].to_string())
}

/// Every path `git` tracks under `dir`, repo-relative.
fn tracked(repo: &Path, dir: &str) -> Vec<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["ls-files", "-z", "--", dir])
        .output()
        .expect("git must be on PATH");
    assert!(out.status.success(), "git ls-files failed");
    String::from_utf8(out.stdout)
        .unwrap()
        .split('\0')
        .filter(|path| !path.is_empty())
        .map(str::to_string)
        .collect()
}

/// A throwaway repository whose `docs/adr/` holds `files`, all staged.
///
/// Staged rather than merely written, because the register is
/// `git ls-files`: an unstaged file is deliberately not in it.
fn register_repo(files: &[(&str, &str)]) -> TempDir {
    let repo = tempfile::tempdir().unwrap();
    run_git(repo.path(), &["init", "-q"]);
    if !files.is_empty() {
        std::fs::create_dir_all(repo.path().join("docs/adr")).unwrap();
        for (name, body) in files {
            std::fs::write(repo.path().join("docs/adr").join(name), body).unwrap();
        }
        run_git(repo.path(), &["add", "--", "docs"]);
    }
    repo
}

fn run_git(repo: &Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        // Hermetic: the developer's global config, hooks and templates
        // must not decide whether this passes.
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .args(args)
        .output()
        .expect("git must be on PATH");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}
