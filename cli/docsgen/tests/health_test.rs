// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The committed corpus census, compared by value.
//!
//! Every case here is about one question: **can this change delete
//! documented surface without saying so?** The numbers are the live
//! corpus's, so a reader of a failure sees the shape of a real run
//! rather than a toy one.

use docsgen::health::{compare, read, write, Baseline, Divergence, Kind, FILE};

/// Every message, so an assertion reads the text without unwrapping a
/// [`Divergence`] at each site.
fn messages(found: &[Divergence]) -> Vec<&str> {
    found.iter().map(|d| d.message.as_str()).collect()
}

/// The corpus as `docsgen metrics` measures it today. Used as both
/// sides of the comparison, so each test perturbs exactly one field
/// and the rest are provably irrelevant to the message it asserts.
fn census() -> Baseline {
    Baseline {
        pages: 33,
        invocations: 384,
        identifiers: 236,
        cue_documents: 2,
        cue_fences: 17,
        code_paths: 73,
        adr_references: 66,
        exemptions: 2,
    }
}

/// Every field a change may only grow, named once so a test that
/// sweeps them cannot silently miss one.
const OBLIGATIONS: [&str; 7] = [
    "pages",
    "invocations",
    "identifiers",
    "cue_documents",
    "cue_fences",
    "code_paths",
    "adr_references",
];

/// Mutate one obligation count by `delta`, by name.
fn shift(baseline: &mut Baseline, field: &str, delta: isize) {
    let slot = match field {
        "pages" => &mut baseline.pages,
        "invocations" => &mut baseline.invocations,
        "identifiers" => &mut baseline.identifiers,
        "cue_documents" => &mut baseline.cue_documents,
        "cue_fences" => &mut baseline.cue_fences,
        "code_paths" => &mut baseline.code_paths,
        "adr_references" => &mut baseline.adr_references,
        "exemptions" => &mut baseline.exemptions,
        other => panic!("no such census field: {other}"),
    };
    *slot = slot
        .checked_add_signed(delta)
        .expect("the test's own arithmetic must stay in range");
}

#[test]
fn an_unchanged_census_is_silent() {
    assert!(compare(&census(), &census()).is_empty());
}

#[test]
fn growth_in_an_obligation_count_is_silent() {
    // Growth is unremarkable: a content phase adds guides, and every
    // guide adds invocations, paths and citations. A gate that made a
    // contributor re-record the census to add a page would be a gate
    // contributors route around.
    for field in OBLIGATIONS {
        for delta in [1, 200] {
            let mut current = census();
            shift(&mut current, field, delta);
            let found = compare(&census(), &current);
            assert!(
                found.is_empty(),
                "{field} +{delta} must pass silently, got {found:?}"
            );
        }
    }
}

#[test]
fn a_lost_page_is_reported_naming_both_numbers() {
    // Both numbers, because "the census regressed" is not actionable:
    // the reader has to know whether one page went or fifteen did
    // before deciding whether it was theirs.
    let mut current = census();
    shift(&mut current, "pages", -1);
    let found = compare(&census(), &current);
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].kind, Kind::Loss, "{found:?}");
    assert!(found[0].message.contains("pages"), "{found:?}");
    assert!(
        found[0].message.contains("records 33"),
        "the committed number: {found:?}"
    );
    assert!(
        found[0].message.contains("found 32"),
        "this run's number: {found:?}"
    );
}

#[test]
fn a_lost_invocation_is_reported() {
    // The commonest real regression: a page is rewritten and a
    // documented command disappears with it. Nothing else in the
    // census moves, so this is the only counter that can see it.
    let mut current = census();
    shift(&mut current, "invocations", -1);
    let found = compare(&census(), &current);
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].kind, Kind::Loss, "{found:?}");
    assert!(found[0].message.contains("invocations"), "{found:?}");
    assert!(
        found[0].message.contains("records 384"),
        "the committed number: {found:?}"
    );
    assert!(
        found[0].message.contains("found 383"),
        "this run's number: {found:?}"
    );
}

#[test]
fn every_obligation_count_is_ratcheted() {
    // A sweep rather than six near-identical tests, and no more than
    // that: it enumerates [`OBLIGATIONS`] by hand, so it says nothing
    // about a field NOT in that list. Completeness is
    // `the_census_holds_exactly_the_fields_this_file_sweeps`'s job.
    for field in OBLIGATIONS {
        let mut current = census();
        shift(&mut current, field, -1);
        let found = compare(&census(), &current);
        assert_eq!(found.len(), 1, "{field}: {found:?}");
        assert_eq!(found[0].kind, Kind::Loss, "{field}: {found:?}");
        assert!(found[0].message.contains(field), "{field}: {found:?}");
    }
}

#[test]
fn the_census_holds_exactly_the_fields_this_file_sweeps() {
    // The guard the sweep above cannot be: an eighth field added to
    // `Baseline` and left out of `compare` would be a ratchet nobody
    // ratchets, and the sweep would keep passing because it never
    // names it. The committed file is the source read here rather than
    // the struct, because the file is what a reviewer and a future
    // `read` both see — and it is the only listing of the fields that
    // cannot go stale without this failing.
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), &census()).unwrap();
    let text = std::fs::read_to_string(dir.path().join(FILE)).unwrap();

    let mut written: Vec<String> = text
        .lines()
        .filter_map(|line| line.trim().strip_prefix('"'))
        .filter_map(|rest| rest.split_once("\":"))
        .map(|(key, _)| key.to_string())
        .collect();
    let mut expected: Vec<String> = OBLIGATIONS.iter().map(|f| f.to_string()).collect();
    expected.push("exemptions".to_string());
    written.sort();
    expected.sort();
    assert_eq!(
        written, expected,
        "a census field this file does not sweep, or a swept field the census dropped: {text}"
    );
}

#[test]
fn a_new_exemption_is_reported_even_though_it_is_growth() {
    // The one field where growth is the finding. An exemption is a
    // documented claim the gate has been told not to check, and the
    // review moment it deserves is the diff line that records it.
    let mut current = census();
    shift(&mut current, "exemptions", 1);
    let found = compare(&census(), &current);
    assert_eq!(found.len(), 1, "{found:?}");
    // Its OWN kind, so the gate can print a remedy that does not tell
    // the reader to restore documentation nobody removed.
    assert_eq!(found[0].kind, Kind::Exemptions, "{found:?}");
    assert!(found[0].message.contains("exemptions"), "{found:?}");
    assert!(found[0].message.contains("records 2"), "{found:?}");
    assert!(found[0].message.contains("found 3"), "{found:?}");
    assert!(
        found[0].message.contains("an exemption was declared"),
        "{found:?}"
    );
}

#[test]
fn a_retired_exemption_is_reported_too() {
    // Symmetry is the point: retiring an exemption is good news, and
    // it is still worth one diff line. Equality in both directions is
    // what keeps the committed number a true statement about the
    // corpus rather than a ceiling nobody lowered.
    let mut current = census();
    shift(&mut current, "exemptions", -1);
    let found = compare(&census(), &current);
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].kind, Kind::Exemptions, "{found:?}");
    assert!(found[0].message.contains("exemptions"), "{found:?}");
    assert!(found[0].message.contains("records 2"), "{found:?}");
    assert!(found[0].message.contains("found 1"), "{found:?}");
    assert!(
        found[0].message.contains("an exemption was retired"),
        "{found:?}"
    );
}

#[test]
fn several_regressions_are_all_reported_in_one_run() {
    // Same reasoning as `gate::report`'s accumulation: one finding per
    // run costs a CI round-trip per fix and teaches contributors to
    // stop running the gate locally.
    let mut current = census();
    shift(&mut current, "pages", -2);
    shift(&mut current, "invocations", -30);
    shift(&mut current, "adr_references", -4);
    shift(&mut current, "exemptions", 1);
    let found = compare(&census(), &current);
    assert_eq!(found.len(), 4, "{found:?}");
    for field in ["pages", "invocations", "adr_references", "exemptions"] {
        assert!(
            messages(&found).iter().any(|m| m.contains(field)),
            "{field} missing from {found:?}"
        );
    }
    // One run, two kinds: three losses and the exemption that grew. A
    // run that reported them all as one kind would print the deletion
    // remedy above a line about an exemption nobody deleted.
    assert_eq!(
        found.iter().filter(|d| d.kind == Kind::Loss).count(),
        3,
        "{found:?}"
    );
    assert_eq!(
        found.iter().filter(|d| d.kind == Kind::Exemptions).count(),
        1,
        "{found:?}"
    );
}

#[test]
fn growth_beside_a_regression_reports_only_the_regression() {
    // The realistic deletion: a rewrite that adds two guides and drops
    // a third. The added surface must not net out the lost surface —
    // a ratchet that summed the corpus into one number would report
    // this run as growth.
    let mut current = census();
    shift(&mut current, "pages", 2);
    shift(&mut current, "invocations", 40);
    shift(&mut current, "identifiers", -1);
    let found = compare(&census(), &current);
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].message.contains("identifiers"), "{found:?}");
}

#[test]
fn the_committed_file_round_trips_and_is_diffable() {
    // Committed, so its diff is what a reviewer reads: stable key
    // order and a trailing newline, or every unrelated run rewrites
    // lines nobody changed.
    let dir = tempfile::tempdir().unwrap();
    let path = write(dir.path(), &census()).unwrap();
    assert_eq!(path, dir.path().join(FILE));

    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.ends_with("}\n"), "must end in one newline: {text:?}");
    let order: Vec<usize> = [
        "pages",
        "invocations",
        "identifiers",
        "cue_documents",
        "code_paths",
        "adr_references",
        "exemptions",
    ]
    .iter()
    .map(|key| text.find(&format!("\"{key}\"")).expect(key))
    .collect();
    let mut sorted = order.clone();
    sorted.sort_unstable();
    assert_eq!(order, sorted, "declaration order is the file order: {text}");

    assert_eq!(read(dir.path()).unwrap(), census());
}

#[test]
fn a_missing_or_unparseable_baseline_is_an_error() {
    // Never a finding: a checkout without the file is a checkout to
    // repair, not a page to edit — the same split the gate draws
    // between "no `cue` on PATH" and "this command does not resolve".
    let dir = tempfile::tempdir().unwrap();
    let missing = read(dir.path()).unwrap_err().to_string();
    assert!(missing.contains(FILE), "must name the file: {missing}");

    std::fs::create_dir_all(dir.path().join("docs/measurements")).unwrap();
    std::fs::write(dir.path().join(FILE), "{ not json").unwrap();
    let broken = read(dir.path()).unwrap_err().to_string();
    assert!(broken.contains(FILE), "must name the file: {broken}");

    // A file that is valid JSON but not this shape is the same class
    // of breakage — a hand-edit that dropped a field, most likely.
    std::fs::write(dir.path().join(FILE), "{\"pages\": 33}\n").unwrap();
    assert!(read(dir.path()).is_err());
}
