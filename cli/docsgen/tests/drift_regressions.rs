// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! One fixture per confirmed drift, each with a paired negative control.
//!
//! # Why fixtures, when the gate is green
//!
//! The pages that proved this gate works were fixed the day it went
//! live, so without these files nothing in the repository demonstrates
//! that it catches what it was built to catch. A future refactor that
//! quietly narrows a check would leave the corpus green and nobody
//! would know. Each `testdata/drift/NN-<slug>.md` is a page that drifted
//! here, reduced to the one shape that made it drift.
//!
//! # Why every fixture is paired
//!
//! **Every drift's lexical shape also occurs legitimately**, and a gate
//! that fires on the drift *and* on the correct line next to it gets
//! switched off by the first contributor it inconveniences. From this
//! corpus alone: `node-prep.md` names a removed command on purpose;
//! five connection-key lines were correct while twenty-five were
//! stale (30 in all, across three files, as found at `dc4c5de`); a
//! heading reading "Phase 2 of `bootstrap-all`" is legitimate prose
//! whose anchor is linked. So each `NN-<slug>-ok.md` writes the *same
//! lexical shape* as its twin — the same block kind, the same tokens,
//! often the same bytes but one — and must pass clean. A control that
//! is merely a different correct page proves nothing.
//!
//! # Why the assertion is on the code set, not on "it failed"
//!
//! [`drift`] asserts the **exact** multiset of finding codes. Asserting
//! only that a page fails would let a fixture prove the wrong thing: a
//! page that failed because its fence was unlabelled says nothing about
//! `schema-identifier`, and would keep passing this test after the
//! identifier check was deleted. An exact set fails in both directions —
//! the check under test disappearing, and a second check starting to
//! fire — which is what makes these files a tripwire rather than a
//! decoration.
//!
//! # The tag repository
//!
//! Exemptions are aged against a repository built for the test, not this
//! one: `actions/checkout` fetches no tags at its default depth, so a
//! test leaning on the real tags would pass on a developer's machine and
//! fail in CI — the asymmetry the gate's expiry policy exists to close.
//! Same construction as `gate_test.rs`, duplicated rather than shared
//! because each integration test is its own crate and both files are
//! meant to be readable on their own.

use docsgen::gate::{self, Gate};
use docsgen::scan;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tempfile::TempDir;

/// Where the fixtures live, repo-relative. Also the path asserted to be
/// **outside** the gated corpus by
/// [`the_fixtures_are_outside_the_gated_corpus`].
const DIR: &str = "cli/docsgen/testdata/drift";

/// An arbitrary fixed instant the tag repository is built around, so the
/// day a test runs never decides whether it passes.
const EPOCH: i64 = 1_700_000_000;
/// How long after [`EPOCH`] the fresh tag is cut, and "now" is taken.
const LATER: i64 = 400 * 86_400;

// ---- the fixtures ------------------------------------------------------

#[test]
fn expose_public_in_a_fence_fails_the_cue_and_identifier_checks() {
    // The manifest is a complete document, so both checks own it: `cue
    // vet` rejects the field and membership rejects the path. Two codes
    // is the honest expectation — this fixture therefore does NOT
    // isolate `schema-identifier`, which is what 02/03/04 are for.
    drift(
        "01-expose-public-fence",
        &[gate::CUE_DOCUMENT, gate::SCHEMA_IDENTIFIER],
        "expose.public",
    );
}

#[test]
fn expose_public_in_a_field_table_row_fails_the_identifier_check() {
    // A table cell reaches no other check: it is not a fence, so it owes
    // neither the CLI nor the CUE obligation. If `schema-identifier` were
    // deleted this fixture would go green, which is exactly what makes it
    // evidence for that check and nothing else.
    drift(
        "02-expose-public-table",
        &[gate::SCHEMA_IDENTIFIER],
        "expose.public",
    );
}

#[test]
fn expose_public_in_a_prose_bullet_fails_the_identifier_check() {
    // The majority surface: most field names in this corpus are written
    // in a backticked span in prose, not in a fence.
    drift(
        "03-expose-public-prose",
        &[gate::SCHEMA_IDENTIFIER],
        "expose.public",
    );
}

#[test]
fn expose_public_inside_a_kubectl_heredoc_fails_the_identifier_check() {
    // The apiserver prunes an unknown key rather than rejecting it, so
    // "apply it and see" reports success on this page. Structural
    // membership is the only check that sees it — and the fence carries
    // no `apprafter` token and no package clause, so it owes nothing
    // else. A single expected code is the proof of that.
    drift(
        "04-expose-public-heredoc",
        &[gate::SCHEMA_IDENTIFIER],
        "expose.public",
    );
}

#[test]
fn a_command_that_never_shipped_fails_the_invocation_check() {
    drift(
        "05-command-that-never-shipped",
        &[gate::CLI_INVOCATION],
        "promote",
    );
}

#[test]
fn a_flag_borrowed_from_a_sibling_fails_the_invocation_check() {
    // `--namespace` is real on a dozen commands and absent on this one,
    // which is why the control has to attach the identical token to a
    // command that does take it.
    drift("06-sibling-flag", &[gate::CLI_INVOCATION], "--namespace");
}

#[test]
fn a_fence_with_no_info_string_is_a_finding_on_its_own() {
    // The invocation inside resolves, so `unlabelled-fence` is the only
    // thing wrong with the page and the only code expected.
    drift(
        "07-unlabelled-fence",
        &[gate::UNLABELLED_FENCE],
        "info string",
    );
}

#[test]
fn an_exemption_past_the_window_is_reported_and_stops_silencing() {
    // Two codes by design, not by accident: a void exemption is reported
    // AND the finding it used to cover comes back. Asserting only
    // `exemption-expired` would let "expired but still silencing" pass.
    drift(
        "09-exemption-expired",
        &[gate::CLI_INVOCATION, gate::EXEMPTION_EXPIRED],
        "reserve-headroom",
    );
}

#[test]
fn an_exemption_that_matches_nothing_is_reported() {
    drift(
        "10-exemption-unused",
        &[gate::EXEMPTION_UNUSED],
        "spec.source.path",
    );
}

// ---- a coverage gap, written down rather than curated away -------------

#[test]
fn a_stale_secret_key_in_a_jsonpath_is_a_known_hole() {
    // `08-stale-secret-key.md` carries REAL drift — ADR 0046 renamed the
    // `pg` connection-Secret keys, so `jsonpath='{.data.DATABASE_URL}'`
    // returns empty against a live cluster — and the gate reports
    // nothing. This is asserted rather than omitted: a corpus of fixtures
    // curated down to what already passes describes the gate's ambition,
    // not its coverage.
    //
    // Why nothing fires. The line names no `apprafter` subcommand, so
    // `invocation` never looks at it; `DATABASE_URL` carries no dot and
    // no `TEXT_ROOTS` component, so `identifier::extract_paths` does not
    // read it as a schema path; the jsonpath's braces sit inside a shell
    // single-quoted string, which `identifier::extract_structure` skips
    // wholesale, so no key path is produced either; and the fence is not
    // a CUE document.
    //
    // What would have to change to close it: a check that resolves a
    // documented Secret **key** against the keys the provisioner writes
    // (`operator-controllers/resourceclaim-provisioner`) — a new check
    // with its own corpus surface, not a widening of any existing one.
    // Widening `identifier` is the wrong lever: it resolves against the
    // CRDs, and a connection-Secret key is not a CRD field.
    //
    // If a future check does catch this page, move the fixture into a
    // `drift` row with its code — do not delete this test's fixture.
    let (repo, now) = tag_repo();
    let gate = gate_with(repo.path(), now);
    let (file, source) = fixture("08-stale-secret-key.md");
    let findings = gate.check_source(&file, &source).unwrap();
    assert!(
        findings.is_empty(),
        "the hole closed — good. Move 08 into a `drift` row naming the \
         code that now fires, and update the plan's known-gaps list: \
         {findings:#?}"
    );

    // The control still has to pass, because the two correct
    // `DATABASE_URL` shapes are what make a lexical ban unavailable as a
    // fix: banning the token would fail this page and leave the stale
    // jsonpath standing.
    let (file, source) = fixture("08-stale-secret-key-ok.md");
    let findings = gate.check_source(&file, &source).unwrap();
    assert!(
        findings.is_empty(),
        "08-stale-secret-key-ok.md is the negative control: {findings:#?}"
    );
}

// ---- the fixtures must not become corpus -------------------------------

#[test]
fn the_fixtures_are_outside_the_gated_corpus() {
    // Confirmed rather than assumed. `scan::in_scope` lists tracked files
    // under `docs/` plus `README.md`; these pages are neither, and they
    // are deliberately full of drift, so admitting one would turn the
    // real gate permanently red.
    let root = docsgen::repo_root().unwrap();
    let dir = root.join(DIR);
    let fixtures = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("reading {}: {e}", dir.display()))
        .count();
    assert!(
        fixtures >= 20,
        "only {fixtures} fixture(s) — this assertion would pass vacuously"
    );

    let in_scope = scan::in_scope(&root).unwrap();
    assert!(!in_scope.is_empty(), "the corpus listing came back empty");
    assert!(
        in_scope.iter().all(|path| !path.starts_with(&dir)),
        "a drift fixture is in the gated corpus: {:?}",
        in_scope
            .iter()
            .filter(|path| path.starts_with(&dir))
            .collect::<Vec<_>>()
    );
}

// ---- helpers -----------------------------------------------------------

/// Run the gate over `<stem>.md` and `<stem>-ok.md`.
///
/// The drift file must produce **exactly** `expected` — see the module
/// docs on why "it failed" is not a strong enough assertion — and at
/// least one of its findings must name `names`, so a fixture cannot be
/// satisfied by the right code raised about the wrong thing.
fn drift(stem: &str, expected: &[&str], names: &str) {
    let (repo, now) = tag_repo();
    let gate = gate_with(repo.path(), now);

    let (file, source) = fixture(&format!("{stem}.md"));
    let findings = gate.check_source(&file, &source).unwrap();
    assert_eq!(
        codes(&findings),
        expected,
        "{stem}.md must fail with exactly these codes: {findings:#?}"
    );
    assert!(
        findings.iter().any(|found| found.message.contains(names)),
        "{stem}.md failed, but no finding names `{names}` — the code is \
         right and the subject is not: {findings:#?}"
    );

    let (file, source) = fixture(&format!("{stem}-ok.md"));
    let findings = gate.check_source(&file, &source).unwrap();
    assert!(
        findings.is_empty(),
        "{stem}-ok.md is the negative control and must pass clean — a \
         gate that reports the correct line next to the drift is a gate \
         that gets switched off: {findings:#?}"
    );
}

/// A fixture's repo-relative name (so a finding points at the real file)
/// and its source.
fn fixture(name: &str) -> (String, String) {
    let path = docsgen::repo_root().unwrap().join(DIR).join(name);
    let source = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
    (format!("{DIR}/{name}"), source)
}

/// Every finding's code, sorted, duplicates kept — a second finding of
/// the same class is a fact about the page and must not be smoothed away.
fn codes(findings: &[gate::Finding]) -> Vec<&str> {
    let mut out: Vec<&str> = findings.iter().map(|found| found.code).collect();
    out.sort_unstable();
    out
}

/// A repository holding one tag past the expiry window (`v0.1.0`) and one
/// well inside it (`v0.9.0`), plus the instant to judge them at.
fn tag_repo() -> (TempDir, SystemTime) {
    let repo = tempfile::tempdir().unwrap();
    git(repo.path(), &["init", "-q"]);
    commit(repo.path(), "old", EPOCH);
    git(repo.path(), &["tag", "v0.1.0"]);
    commit(repo.path(), "new", EPOCH + LATER);
    git(repo.path(), &["tag", "v0.9.0"]);
    // Ten days after the fresh tag: `v0.9.0` is 10 days old, `v0.1.0` is
    // 410 — one either side of the 180-day window.
    let now = UNIX_EPOCH + Duration::from_secs((EPOCH + LATER + 10 * 86_400) as u64);
    (repo, now)
}

fn gate_with(tags: &Path, now: SystemTime) -> Gate {
    Gate::pinned(&repo_root(), tags, now).unwrap()
}

fn repo_root() -> PathBuf {
    docsgen::repo_root().unwrap()
}

fn git(repo: &Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        // Hermetic: the developer's global config, hooks and signing key
        // must not decide whether this passes.
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .args(["-c", "user.name=docsgen", "-c", "user.email=docsgen@test"])
        .args(args)
        .output()
        .expect("git must be on PATH");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn commit(repo: &Path, message: &str, at: i64) {
    let when = format!("{at} +0000");
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_AUTHOR_DATE", &when)
        .env("GIT_COMMITTER_DATE", &when)
        .args(["-c", "user.name=docsgen", "-c", "user.email=docsgen@test"])
        .args([
            "commit",
            "-q",
            "--allow-empty",
            "--no-gpg-sign",
            "-m",
            message,
        ])
        .output()
        .expect("git must be on PATH");
    assert!(
        out.status.success(),
        "git commit: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}
