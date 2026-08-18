// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The obligation rules, on synthetic pages.
//!
//! Every case here is about *which check a block owes*, which is the
//! one decision this crate makes that no other module can be asked
//! about. The corpus is deliberately absent: a rule tested only against
//! the corpus is a rule that changes meaning the day somebody edits a
//! page.
//!
//! Exemptions are aged against a repository built for the test rather
//! than this one. That is not a convenience — `actions/checkout` fetches
//! no tags at its default depth, so a test leaning on the real tags
//! would pass on a developer's machine and fail in CI, which is exactly
//! the asymmetry the gate's expiry policy exists to close.

use docsgen::gate::{self, Gate};
use std::path::Path;
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tempfile::TempDir;

/// An arbitrary fixed instant the tag repository is built around, so
/// the day a test runs never decides whether it passes.
const EPOCH: i64 = 1_700_000_000;
/// How long after [`EPOCH`] the fresh tag is cut, and "now" is taken.
const LATER: i64 = 400 * 86_400;

/// A repository holding one tag past the expiry window (`v0.1.0`) and
/// one well inside it (`v0.9.0`), plus the instant to judge them at.
fn tag_repo() -> (TempDir, SystemTime) {
    let repo = tempfile::tempdir().unwrap();
    git(repo.path(), &["init", "-q"]);
    commit(repo.path(), "old", EPOCH);
    git(repo.path(), &["tag", "v0.1.0"]);
    commit(repo.path(), "new", EPOCH + LATER);
    git(repo.path(), &["tag", "v0.9.0"]);
    // Ten days after the fresh tag: `v0.9.0` is 10 days old, `v0.1.0`
    // is 410 — one either side of the 180-day window.
    let now = UNIX_EPOCH + Duration::from_secs((EPOCH + LATER + 10 * 86_400) as u64);
    (repo, now)
}

fn gate_with(tags: &Path, now: SystemTime) -> Gate {
    Gate::pinned(&docsgen::repo_root().unwrap(), tags, now).unwrap()
}

/// Findings of one class, as `line: message`, so an assertion names the
/// class it is about instead of counting a mixed list.
fn of(findings: &[gate::Finding], code: &str) -> Vec<String> {
    findings
        .iter()
        .filter(|found| found.code == code)
        .map(|found| format!("{}: {}", found.line, found.message))
        .collect()
}

// ---- obligations come from content, never from the tag ----------------

#[test]
fn an_invocation_in_a_fence_is_checked_whatever_the_tag() {
    // Hand-written shell in the corpus is tagged `sh`, `bash`, `console`
    // and nothing at all. Keying on the tag would make "delete the tag"
    // a way to turn a finding green with nothing in the diff that reads
    // as suppression.
    let (repo, now) = tag_repo();
    let gate = gate_with(repo.path(), now);
    for tag in ["sh", "bash", "console", ""] {
        let page = format!("# Page\n\n```{tag}\napprafter promote\n```\n");
        let findings = gate.check_source("docs/t.md", &page).unwrap();
        let cli = of(&findings, gate::CLI_INVOCATION);
        assert_eq!(cli.len(), 1, "tag {tag:?}: {findings:?}");
        assert!(cli[0].starts_with("4: "), "tag {tag:?}: {cli:?}");
        assert!(cli[0].contains("promote"), "tag {tag:?}: {cli:?}");
    }
}

#[test]
fn a_flag_on_a_continuation_line_is_still_the_commands_flag() {
    // 18 corpus invocations put the command on one line and its flags on
    // the next. Reading the body with `str::lines()` would see
    // `apprafter app status writer` — which resolves — and judge the
    // flags this check exists to judge on none of them.
    let (repo, now) = tag_repo();
    let gate = gate_with(repo.path(), now);
    let page = "```sh\napprafter app status writer \\\n  --namespace apps\n```\n";
    let findings = gate.check_source("docs/t.md", page).unwrap();
    let cli = of(&findings, gate::CLI_INVOCATION);
    assert_eq!(cli.len(), 1, "{findings:?}");
    assert!(cli[0].contains("--namespace"), "{cli:?}");
    // The finding lands on the line the command started, not the flag's.
    assert!(cli[0].starts_with("2: "), "{cli:?}");
}

#[test]
fn a_complete_cue_document_is_checked_whatever_the_tag() {
    // A `package` clause and the schema import make a fence a document;
    // the tag says nothing. Tagged `text` here precisely so a green run
    // cannot be explained by the tag.
    let (repo, now) = tag_repo();
    let gate = gate_with(repo.path(), now);
    let page = format!(
        "# Page\n\n```text\n{}```\n",
        document("network: \"internal\"")
    );
    let findings = gate.check_source("docs/t.md", &page).unwrap();
    assert!(
        findings.is_empty(),
        "a correct manifest must vet clean: {findings:?}"
    );

    let page = format!("# Page\n\n```text\n{}```\n", document("publik: false"));
    let findings = gate.check_source("docs/t.md", &page).unwrap();
    let cue = of(&findings, gate::CUE_DOCUMENT);
    assert_eq!(cue.len(), 1, "{findings:?}");
    assert!(cue[0].contains("publik"), "{cue:?}");
}

#[test]
fn a_fragment_is_not_a_document_and_owes_no_cue_check() {
    // 13 of 26 `cue` fences are bare fragments at seven anchor depths.
    // They are out of the CUE check by design — see cuedoc's module docs
    // — so a fragment must not become a finding for lacking a package
    // clause it was never going to have.
    let (repo, now) = tag_repo();
    let gate = gate_with(repo.path(), now);
    let page = "```cue\nspec: base: expose: {\n    port: 8080\n}\n```\n";
    let findings = gate.check_source("docs/t.md", page).unwrap();
    assert!(of(&findings, gate::CUE_DOCUMENT).is_empty(), "{findings:?}");
}

#[test]
fn an_untagged_fence_is_a_finding() {
    // The tag is needed for highlighting regardless, and requiring it
    // removes the one edit that could otherwise look innocent.
    let (repo, now) = tag_repo();
    let gate = gate_with(repo.path(), now);
    let findings = gate
        .check_source("docs/t.md", "# Page\n\n```\njust some output\n```\n")
        .unwrap();
    assert_eq!(
        of(&findings, gate::UNLABELLED_FENCE).len(),
        1,
        "{findings:?}"
    );
    // A tagged one is not.
    let findings = gate
        .check_source("docs/t.md", "# Page\n\n```text\njust some output\n```\n")
        .unwrap();
    assert!(findings.is_empty(), "{findings:?}");
}

#[test]
fn unfencing_a_block_does_not_shed_its_obligation() {
    // The whole reason `BlockKind::Literal` exists. If this ever goes
    // green by reporting nothing, the `unlabelled-fence` finding has
    // become a suggestion to indent instead of to label.
    let (repo, now) = tag_repo();
    let gate = gate_with(repo.path(), now);
    let fenced = "# Page\n\n```sh\napprafter promote\n```\n";
    let unfenced = "# Page\n\nProse.\n\n    apprafter promote\n";
    let a = of(
        &gate.check_source("docs/t.md", fenced).unwrap(),
        gate::CLI_INVOCATION,
    );
    let b = of(
        &gate.check_source("docs/t.md", unfenced).unwrap(),
        gate::CLI_INVOCATION,
    );
    assert_eq!(a.len(), 1, "fenced: {a:?}");
    assert_eq!(b.len(), 1, "unfenced: {b:?}");
    assert!(b[0].contains("promote"), "{b:?}");
    // On the line the command is written on, not the line after it: a
    // literal block has no delimiter above its body.
    assert!(b[0].starts_with("5: "), "{b:?}");
}

#[test]
fn a_literal_block_owes_no_fence_shaped_finding() {
    // It has no fence to label, so reporting one would name a remedy
    // that does not apply.
    //
    // Asserting an empty finding list would prove nothing here — a gate
    // that never looked at literal blocks at all would pass it too. So
    // the block holds a real invocation, and the second half of the
    // test misspells that same invocation to show the block IS being
    // read. Silence only means something once noise is reachable.
    let (repo, now) = tag_repo();
    let gate = gate_with(repo.path(), now);
    let page = "# Page\n\nProse.\n\n    apprafter app list\n";
    let findings = gate.check_source("docs/t.md", page).unwrap();
    assert!(
        of(&findings, gate::UNLABELLED_FENCE).is_empty(),
        "there is no fence here to label: {findings:?}"
    );
    assert!(
        of(&findings, gate::UNTERMINATED_FENCE).is_empty(),
        "{findings:?}"
    );
    assert!(
        findings.is_empty(),
        "a valid invocation resolves: {findings:?}"
    );

    // The same block, one letter wrong: proof the gate reads it.
    let page = "# Page\n\nProse.\n\n    apprafter app lst\n";
    let findings = gate.check_source("docs/t.md", page).unwrap();
    let cli = of(&findings, gate::CLI_INVOCATION);
    assert_eq!(cli.len(), 1, "{findings:?}");
    assert!(cli[0].starts_with("5: "), "{cli:?}");
    assert!(
        of(&findings, gate::UNLABELLED_FENCE).is_empty(),
        "still no fence to label: {findings:?}"
    );
}

#[test]
fn an_unclosed_pre_is_reported_rather_than_swallowing_the_page_in_silence() {
    // A `<pre>` runs to its closing tag, so one stray opener turns every
    // block below it into body text: the fences below shed their
    // unlabelled-fence finding and their marker audit, in one line and
    // with nothing in the diff that reads as suppression. The claims
    // inside are still judged — body is body — but the reader has to be
    // told the page renders as code from here down.
    let (repo, now) = tag_repo();
    let gate = gate_with(repo.path(), now);
    let page = "# Page\n\n<pre>\napprafter promote\n\n```\nstill inside\n```\n";
    let findings = gate.check_source("docs/t.md", page).unwrap();
    assert_eq!(of(&findings, gate::UNCLOSED_PRE).len(), 1, "{findings:?}");
    assert_eq!(
        of(&findings, gate::CLI_INVOCATION).len(),
        1,
        "the claim inside is still judged: {findings:?}"
    );
    assert!(
        of(&findings, gate::UNLABELLED_FENCE).is_empty(),
        "there is no fence down there any more — which is exactly why the \
         unclosed `<pre>` itself has to be reported: {findings:?}"
    );
    // And its remedy is its own: closing a tag is not closing a fence.
    assert!(
        of(&findings, gate::UNTERMINATED_FENCE).is_empty(),
        "{findings:?}"
    );

    // Closed, the same page owes only what its blocks owe.
    let page = "# Page\n\n<pre>\napprafter promote\n</pre>\n";
    let findings = gate.check_source("docs/t.md", page).unwrap();
    assert!(of(&findings, gate::UNCLOSED_PRE).is_empty(), "{findings:?}");
    assert_eq!(of(&findings, gate::CLI_INVOCATION).len(), 1, "{findings:?}");
}

// ---- the marker channel ------------------------------------------------

#[test]
fn a_valid_exemption_marker_silences_the_fence() {
    let (repo, now) = tag_repo();
    let gate = gate_with(repo.path(), now);
    let page = "<!-- docs: check=none reason=historical since=v0.9.0 — it was removed -->\n\
                ```sh\napprafter promote\n```\n";
    let findings = gate.check_source("docs/t.md", page).unwrap();
    assert!(findings.is_empty(), "{findings:?}");
}

#[test]
fn an_expired_exemption_is_a_finding_and_silences_nothing() {
    // An exemption is a claim about the world and the world moves.
    // Silencing on an expired one would let a two-year-old
    // `known-broken` keep hiding a real finding.
    let (repo, now) = tag_repo();
    let gate = gate_with(repo.path(), now);
    let page = "<!-- docs: check=none reason=known-broken since=v0.1.0 — tracked -->\n\
                ```sh\napprafter promote\n```\n";
    let findings = gate.check_source("docs/t.md", page).unwrap();
    let expired = of(&findings, gate::EXEMPTION_EXPIRED);
    assert_eq!(expired.len(), 1, "{findings:?}");
    assert!(expired[0].contains("410 days"), "{expired:?}");
    assert_eq!(
        of(&findings, gate::CLI_INVOCATION).len(),
        1,
        "a void exemption must not silence: {findings:?}"
    );
}

#[test]
fn an_exemption_that_cannot_be_aged_is_a_finding_and_silences_nothing() {
    // The CI policy, pinned: `since=` naming a tag the checkout does not
    // have would otherwise make expiry unenforceable exactly where it
    // matters. `v0.2.44` is a real tag of THIS repository and absent
    // from the tag repository built for the test, which is the same
    // shape as a shallow clone.
    let (repo, now) = tag_repo();
    let gate = gate_with(repo.path(), now);
    let page = "<!-- docs: check=none reason=historical since=v0.2.44 — removed -->\n\
                ```sh\napprafter promote\n```\n";
    let findings = gate.check_source("docs/t.md", page).unwrap();
    assert_eq!(
        of(&findings, gate::EXEMPTION_UNAGED).len(),
        1,
        "{findings:?}"
    );
    assert_eq!(
        of(&findings, gate::CLI_INVOCATION).len(),
        1,
        "an unageable exemption must not silence: {findings:?}"
    );
}

#[test]
fn a_malformed_marker_is_reported_and_silences_nothing() {
    // Reading a broken marker as an exemption would make a typo the
    // cheapest way to switch a check off.
    let (repo, now) = tag_repo();
    let gate = gate_with(repo.path(), now);
    let page = "<!-- docs: check=none reason=historical -->\n```sh\napprafter promote\n```\n";
    let findings = gate.check_source("docs/t.md", page).unwrap();
    assert_eq!(of(&findings, gate::DOCS_MARKER).len(), 1, "{findings:?}");
    assert_eq!(of(&findings, gate::CLI_INVOCATION).len(), 1, "{findings:?}");
}

#[test]
fn a_marker_cannot_claim_a_check_it_cannot_run() {
    // A marker that reads as doing more than it does is worse than no
    // marker, because a reviewer takes it at its word.
    let (repo, now) = tag_repo();
    let gate = gate_with(repo.path(), now);
    for (marker, body) in [
        ("<!-- docs: check=cli -->", "kubectl get pods\n"),
        ("<!-- docs: check=cue -->", "spec: base: image: \"x\"\n"),
        ("<!-- docs: check=cli run=local -->", "apprafter app list\n"),
    ] {
        let page = format!("{marker}\n```sh\n{body}```\n");
        let findings = gate.check_source("docs/t.md", &page).unwrap();
        assert_eq!(
            of(&findings, gate::DOCS_MARKER).len(),
            1,
            "{marker}: {findings:?}"
        );
    }
    // And a marker that matches its block says nothing.
    let page = "<!-- docs: check=cli -->\n```sh\napprafter app list\n```\n";
    let findings = gate.check_source("docs/t.md", page).unwrap();
    assert!(findings.is_empty(), "{findings:?}");
}

#[test]
fn a_declared_check_does_not_narrow_the_content_derived_ones() {
    // `check=cli` must not switch the other checks off: only
    // `check=none` silences, and it costs a typed reason and a version.
    // Here the marker is honest — the fence does hold an invocation —
    // and the schema path buried in the heredoc is still judged.
    let (repo, now) = tag_repo();
    let gate = gate_with(repo.path(), now);
    let page = "<!-- docs: check=cli -->\n\
                ```sh\n\
                apprafter app list\n\
                cat > app.yaml <<'YAML'\n\
                expose:\n  \
                publik: true\n\
                YAML\n\
                ```\n";
    let findings = gate.check_source("docs/t.md", page).unwrap();
    let schema = of(&findings, gate::SCHEMA_IDENTIFIER);
    assert_eq!(schema.len(), 1, "{findings:?}");
    assert!(schema[0].contains("publik"), "{schema:?}");
    assert!(of(&findings, gate::DOCS_MARKER).is_empty(), "{findings:?}");
}

// ---- the page-level exemption channel ---------------------------------

#[test]
fn a_front_matter_exemption_silences_exactly_the_span_it_names() {
    // A span carries no marker, so it is exempted by its literal text —
    // and by EXACT text: a near-miss is a different claim and must still
    // be judged.
    let (repo, now) = tag_repo();
    let gate = gate_with(repo.path(), now);
    let page = "---\n\
                title: Node prep\n\
                cli-check-ignore:\n  \
                - span: \"apprafter node reserve-headroom\"\n    \
                reason: historical\n    \
                since: v0.9.0\n    \
                note: documents a command that was removed\n\
                ---\n\n\
                The `apprafter node reserve-headroom` command was removed.\n\n\
                Unrelated: `apprafter promote`.\n\n\
                A near miss: `apprafter node reserve-headroom --dry-run`.\n";
    let findings = gate.check_source("docs/t.md", page).unwrap();
    let cli = of(&findings, gate::CLI_INVOCATION);
    assert_eq!(cli.len(), 2, "{findings:?}");
    assert!(cli.iter().any(|f| f.contains("promote")), "{cli:?}");
    assert!(cli.iter().any(|f| f.contains("--dry-run")), "{cli:?}");
    assert!(
        !cli.iter().any(|f| f.contains("reserve-headroom\"")),
        "the exempted span must be silent: {cli:?}"
    );
    assert!(
        of(&findings, gate::EXEMPTION_UNUSED).is_empty(),
        "{findings:?}"
    );
}

#[test]
fn a_front_matter_exemption_silences_a_schema_path_page_wide() {
    // The Argo CD case: correct documentation of a foreign CRD our field
    // set neither models nor should.
    let (repo, now) = tag_repo();
    let gate = gate_with(repo.path(), now);
    let page = "---\n\
                schema-check-ignore:\n  \
                - path: \"spec.source.path\"\n    \
                reason: external-tool\n    \
                since: v0.9.0\n    \
                note: Argo CD's Application, not ours\n\
                ---\n\n\
                Argo CD reads `spec.source.path`.\n\n\
                And again later: `spec.source.path`.\n\n\
                But `expose.public` is ours and wrong.\n";
    let findings = gate.check_source("docs/t.md", page).unwrap();
    let schema = of(&findings, gate::SCHEMA_IDENTIFIER);
    assert_eq!(schema.len(), 1, "{findings:?}");
    assert!(schema[0].contains("expose.public"), "{schema:?}");
    assert!(
        of(&findings, gate::EXEMPTION_UNUSED).is_empty(),
        "{findings:?}"
    );
}

#[test]
fn an_exemption_that_silences_nothing_is_reported() {
    // Once the page is fixed the exemption is a claim about a problem
    // that no longer exists; leaving it standing is how an exemption
    // list comes to cover things nobody chose.
    let (repo, now) = tag_repo();
    let gate = gate_with(repo.path(), now);
    let page = "---\n\
                cli-check-ignore:\n  \
                - span: \"apprafter node reserve-headroom\"\n    \
                reason: historical\n    \
                since: v0.9.0\n\
                ---\n\n\
                Nothing here mentions it.\n";
    let findings = gate.check_source("docs/t.md", page).unwrap();
    let unused = of(&findings, gate::EXEMPTION_UNUSED);
    assert_eq!(unused.len(), 1, "{findings:?}");
    assert!(unused[0].contains("reserve-headroom"), "{unused:?}");
    // Reported on the line it was written on, not the top of the page.
    assert!(unused[0].starts_with("3: "), "{unused:?}");
}

#[test]
fn a_malformed_exemption_entry_is_reported_and_covers_nothing() {
    let (repo, now) = tag_repo();
    let gate = gate_with(repo.path(), now);
    let page = "---\n\
                cli-check-ignore:\n  \
                - span: \"apprafter promote\"\n    \
                reason: because-i-said-so\n    \
                since: v0.9.0\n\
                ---\n\n\
                Run `apprafter promote`.\n";
    let findings = gate.check_source("docs/t.md", page).unwrap();
    assert_eq!(of(&findings, gate::FRONT_MATTER).len(), 1, "{findings:?}");
    assert_eq!(of(&findings, gate::CLI_INVOCATION).len(), 1, "{findings:?}");
}

#[test]
fn ordinary_front_matter_declares_nothing_and_is_never_a_claim() {
    // One in-scope page has front matter today. Its keys must not be
    // read as exemptions, and its values must not be read as prose.
    let (repo, now) = tag_repo();
    let gate = gate_with(repo.path(), now);
    let page = "---\ntitle: Environment\ndescription: \"`apprafter promote` is not run here\"\n---\n\n# Page\n";
    let findings = gate.check_source("docs/t.md", page).unwrap();
    assert!(findings.is_empty(), "{findings:?}");
}

// ---- the paths documentation names -------------------------------------

#[test]
fn a_path_that_does_not_exist_is_reported() {
    let (repo, now) = tag_repo();
    let gate = gate_with(repo.path(), now);
    let page = "# Page\n\nSee `cli/cli-providers/src/hetzner_cloud/validators.rs`.\n";
    let found = of(
        &gate.check_source("docs/t.md", page).unwrap(),
        gate::CODE_PATH,
    );
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].starts_with("3: "), "{found:?}");
}

#[test]
fn a_path_that_exists_is_not_reported() {
    let (repo, now) = tag_repo();
    let gate = gate_with(repo.path(), now);
    let page = "# Page\n\nSee `cli/docsgen/src/gate.rs`.\n";
    let found = of(
        &gate.check_source("docs/t.md", page).unwrap(),
        gate::CODE_PATH,
    );
    assert!(found.is_empty(), "{found:?}");
}

#[test]
fn a_directory_resolves_as_readily_as_a_file() {
    // Pages name directories constantly ("the crates under
    // `cli/docsgen/src`"). Resolving only blobs would report every one.
    let (repo, now) = tag_repo();
    let gate = gate_with(repo.path(), now);
    let page = "# Page\n\nThe rules live under `cli/docsgen/src`.\n";
    let found = of(
        &gate.check_source("docs/t.md", page).unwrap(),
        gate::CODE_PATH,
    );
    assert!(found.is_empty(), "{found:?}");
}

#[test]
fn a_page_relative_link_resolves_against_the_page() {
    // The `docs/reference/index.md:10` shape: read as a repo-root path
    // this fails, and reporting it would be the check's first false
    // positive on the day it shipped.
    let (repo, now) = tag_repo();
    let gate = gate_with(repo.path(), now);
    let page = "# Page\n\nships as [`cli/commands.json`](cli/commands.json), which\n";
    let found = of(
        &gate.check_source("docs/reference/index.md", page).unwrap(),
        gate::CODE_PATH,
    );
    assert!(found.is_empty(), "{found:?}");
}

#[test]
fn the_same_link_from_a_page_one_directory_over_does_not_resolve() {
    // Three of the four cases above assert an EMPTY finding list, so a
    // gate that dropped every page-relative reference on the floor —
    // never joining a target to anything, never resolving one — passes
    // all three and looks correct. This is the case that fails unless a
    // link target is really resolved: from `docs/` the same link means
    // `docs/cli/commands.json`, which does not exist. Silence only
    // means something once noise is reachable.
    let (repo, now) = tag_repo();
    let gate = gate_with(repo.path(), now);
    let page = "# Page\n\nships as [`cli/commands.json`](cli/commands.json), which\n";
    let found = of(
        &gate.check_source("docs/index.md", page).unwrap(),
        gate::CODE_PATH,
    );
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].contains("docs/cli/commands.json"), "{found:?}");
}

#[test]
fn a_broken_link_on_a_root_page_names_the_repository_root() {
    // `page_directory("README.md")` is empty, so a message that glued a
    // `/` onto it read "from `/`" — filesystem-absolute, which is a
    // different claim from the one being made. README is the only page
    // in the corpus at this depth, so nothing else would have caught it.
    let (repo, now) = tag_repo();
    let gate = gate_with(repo.path(), now);
    let page = "# Page\n\nRun [the walk](e2e/nope.sh) first.\n";
    let found = of(
        &gate.check_source("README.md", page).unwrap(),
        gate::CODE_PATH,
    );
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].contains("the repository root"), "{found:?}");
    assert!(!found[0].contains("from `/`"), "{found:?}");
}

#[test]
fn a_target_that_climbs_above_the_repository_root_says_so() {
    // The `None` arm of `normalise`. Climbing above the root is not a
    // path that happens not to exist — no edit to the tree could make
    // it resolve — so the message has to say something different, and
    // this is the only test that reaches it.
    let (repo, now) = tag_repo();
    let gate = gate_with(repo.path(), now);
    let page = "# Page\n\nSee [it](../../../../elsewhere/thing.sh).\n";
    let found = of(
        &gate.check_source("docs/reference/index.md", page).unwrap(),
        gate::CODE_PATH,
    );
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].contains("climbs above"), "{found:?}");
}

#[test]
fn a_bracketed_path_is_checked_in_both_directions() {
    // The reason `is_pattern` excludes brackets. Three tracked files
    // carry them — Next.js dynamic routes — and the pair below is what
    // makes the rule observable at all: the real one resolves silently,
    // and a MOVED one is reported rather than mistaken for a glob.
    // Asserting only the silence would pass just as well under a rule
    // that skipped every bracketed path on sight.
    let (repo, now) = tag_repo();
    let gate = gate_with(repo.path(), now);
    let real = "# Page\n\nSee `landing/cms/src/app/(payload)/api/[...slug]/route.ts`.\n";
    let moved = "# Page\n\nSee `landing/cms/src/app/(payload)/api/[...slug]/gone.ts`.\n";
    let ok = of(
        &gate.check_source("docs/t.md", real).unwrap(),
        gate::CODE_PATH,
    );
    let bad = of(
        &gate.check_source("docs/t.md", moved).unwrap(),
        gate::CODE_PATH,
    );
    assert!(ok.is_empty(), "the real route must resolve: {ok:?}");
    assert_eq!(bad.len(), 1, "the moved one must be reported: {bad:?}");
    assert!(bad[0].contains("gone.ts"), "{bad:?}");
}

#[test]
fn a_glob_is_silent_even_though_it_never_resolves() {
    // Eight corpus spans are patterns and all eight are correct
    // documentation. They are extracted like anything else and answered
    // at resolution, which is what keeps the census honest about them.
    let (repo, now) = tag_repo();
    let gate = gate_with(repo.path(), now);
    let page = "# Page\n\nUnder `docs/**/*.md` and `providers/*`, plus `e2e/*.sh`.\n";
    let found = of(
        &gate.check_source("docs/t.md", page).unwrap(),
        gate::CODE_PATH,
    );
    assert!(found.is_empty(), "{found:?}");
}

// ---- the ADRs documentation cites --------------------------------------

#[test]
fn a_reference_to_a_superseded_adr_is_reported() {
    // ADR 0011 is `Superseded by ADR 0016`. It has no reference in the
    // corpus, which is why this is written as a fixture rather than
    // asserted against real pages.
    let (repo, now) = tag_repo();
    let gate = gate_with(repo.path(), now);
    let page = "# Page\n\nStorage follows ADR 0011.\n";
    let found = of(
        &gate.check_source("docs/t.md", page).unwrap(),
        gate::ADR_REFERENCE,
    );
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].contains("0011"), "{found:?}");
}

#[test]
fn a_reference_to_a_partly_superseded_adr_is_not_reported() {
    // The negative control that matters: a blind "superseded" match
    // fires on ADRs 0001, 0042, 0043 and 0053 — 11 in-scope references,
    // none of them defects, against zero true positives in scope. (0043
    // was missing from this list, and from the figure: its `## Status`
    // reads "§1's env-injection naming superseded in part by ADR 0046"
    // and it carries 2 references.) Widen the substring to `supersed`
    // and it is 27 references across 6 cited ADRs, still with no true
    // positive among them.
    let (repo, now) = tag_repo();
    let gate = gate_with(repo.path(), now);
    let page = "# Page\n\nSee ADR 0042, ADR 0053, ADR 0043 and ADR 0001.\n";
    let found = of(
        &gate.check_source("docs/t.md", page).unwrap(),
        gate::ADR_REFERENCE,
    );
    assert!(found.is_empty(), "{found:?}");
}

#[test]
fn a_reference_to_a_draft_adr_is_not_reported() {
    let (repo, now) = tag_repo();
    let gate = gate_with(repo.path(), now);
    let page = "# Page\n\nThe portal follows ADR 0027.\n";
    let found = of(
        &gate.check_source("docs/t.md", page).unwrap(),
        gate::ADR_REFERENCE,
    );
    assert!(found.is_empty(), "{found:?}");
}

#[test]
fn a_reference_to_a_nonexistent_adr_is_reported() {
    let (repo, now) = tag_repo();
    let gate = gate_with(repo.path(), now);
    let page = "# Page\n\nAs ADR-9999 sets out.\n";
    let found = of(
        &gate.check_source("docs/t.md", page).unwrap(),
        gate::ADR_REFERENCE,
    );
    assert_eq!(found.len(), 1, "{found:?}");
}

#[test]
fn a_reference_to_an_unused_slot_is_reported() {
    // `0013` and `0018` were reserved during early planning and
    // abandoned. The file exists, so "no such ADR" would be the wrong
    // message, but the number names no decision — which is what a reader
    // following the citation discovers after opening it.
    let (repo, now) = tag_repo();
    let gate = gate_with(repo.path(), now);
    let page = "# Page\n\nAutoscaling is settled in ADR 0013.\n";
    let found = of(
        &gate.check_source("docs/t.md", page).unwrap(),
        gate::ADR_REFERENCE,
    );
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].contains("Unused"), "{found:?}");
}

#[test]
fn a_citation_inside_a_fence_is_judged_like_any_other() {
    // The design decision, pinned: ADR references are read page-wide off
    // the source, not from prose alone. The corpus writes one inside a
    // fence — `docs/operator-guide/needs-pg-walk.md:247`, a comment in a
    // shell block — so a prose-only rule would drop a real citation, and
    // would make "move the sentence into the block" a way to stop one
    // being checked.
    let (repo, now) = tag_repo();
    let gate = gate_with(repo.path(), now);
    let page = "# Page\n\n```sh\n# the shim (ADR 0011) is applied first\napprafter app list\n```\n";
    let found = of(
        &gate.check_source("docs/t.md", page).unwrap(),
        gate::ADR_REFERENCE,
    );
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].starts_with("4: "), "{found:?}");
}

#[test]
fn a_citation_wrapped_across_a_line_break_is_still_judged() {
    // Three of the cases above assert an EMPTY list, and two more assert
    // a finding on a shape a scanner could reach by accident. This is
    // the one that fails if references are dropped on the floor by the
    // wrap rule specifically: the corpus's only wrapped citation
    // (`docs/operator-guide/target-store.md:8`) is written exactly like
    // this, and "wrap the line" must not be a way to hide a citation.
    let (repo, now) = tag_repo();
    let gate = gate_with(repo.path(), now);
    let page = "# Page\n\n> Rationale: [ADR\n> 0011](../adr/0011-hybrid-rust-sdk-tofu-shim.md).\n";
    let found = of(
        &gate.check_source("docs/t.md", page).unwrap(),
        gate::ADR_REFERENCE,
    );
    assert_eq!(found.len(), 1, "{found:?}");
    // On the line the citation starts.
    assert!(found[0].starts_with("3: "), "{found:?}");
}

#[test]
fn a_citation_written_only_as_a_link_is_judged() {
    // The blind spot this class shipped with, and the one its own first
    // remedy recommended writing into: no `ADR NNNN` text anywhere, and
    // 0011 is `Superseded`. Four corpus citations are written this way.
    let (repo, now) = tag_repo();
    let gate = gate_with(repo.path(), now);
    let page = "# Page\n\nSee [0011](../adr/0011-hybrid-rust-sdk-tofu-shim.md) for the shim.\n";
    let found = of(
        &gate.check_source("docs/t.md", page).unwrap(),
        gate::ADR_REFERENCE,
    );
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].contains("0011"), "{found:?}");
}

#[test]
fn a_prose_line_ending_on_the_word_adr_is_not_a_citation() {
    // The false positive the wrap rule manufactured, through the real
    // gate: `ADR` is an ordinary noun in this corpus's register —
    // `docs/operator-guide/index.md:54` writes "An ADR describes the
    // world as it was when it was ratified" — and this reported
    // "`ADR 2026`: no such ADR", a hard finding on a correct sentence,
    // with a remedy that named neither cause.
    let (repo, now) = tag_repo();
    let gate = gate_with(repo.path(), now);
    let page =
        "# Page\n\n- Every decision is recorded as an ADR\n  2026-05-06 is the earliest one.\n";
    let found = of(
        &gate.check_source("docs/t.md", page).unwrap(),
        gate::ADR_REFERENCE,
    );
    assert!(found.is_empty(), "{found:?}");
}

#[test]
fn citing_the_template_says_so_rather_than_no_such_adr() {
    // `0000-template.md` is skipped by name, so the register has no
    // `0000` — and "docs/adr/ holds no 0000-*.md" would be false about
    // a file sitting right there, sending the reader to hunt a typo.
    let (repo, now) = tag_repo();
    let gate = gate_with(repo.path(), now);
    let page = "# Page\n\nCopy the shape of ADR 0000.\n";
    let found = of(
        &gate.check_source("docs/t.md", page).unwrap(),
        gate::ADR_REFERENCE,
    );
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].contains("template"), "{found:?}");
    assert!(!found[0].contains("no such ADR"), "{found:?}");
}

// ---- the ADR exemption channel -----------------------------------------

#[test]
fn an_adr_citation_is_exempted_in_front_matter_like_any_other_claim() {
    // Citing a reversed decision on purpose is legitimate and is this
    // repository's idiom — `spec.md` writes "superseded ADR 0011" and
    // "see superseded ADR 0011". Shipped without a channel, the remedy
    // told the author to delink the citation instead, which is a
    // permanent, unreviewable silencing invisible in a diff.
    let (repo, now) = tag_repo();
    let gate = gate_with(repo.path(), now);
    let page = "---\n\
                adr-check-ignore:\n  \
                - adr: \"ADR 0011\"\n    \
                reason: historical\n    \
                since: v0.9.0\n    \
                note: the sentence is about the shim we replaced, and says so\n\
                ---\n\n\
                The superseded ADR 0011 shim is described here for readers on \
                old clusters.\n\n\
                Autoscaling is settled in ADR 0013.\n";
    let findings = gate.check_source("docs/t.md", page).unwrap();
    let found = of(&findings, gate::ADR_REFERENCE);
    // Exactly the citation it names, and nothing else on the page.
    assert_eq!(found.len(), 1, "{findings:?}");
    assert!(found[0].contains("0013"), "{found:?}");
    assert!(
        of(&findings, gate::EXEMPTION_UNUSED).is_empty(),
        "{findings:?}"
    );
}

#[test]
fn one_adr_exemption_covers_every_spelling_of_that_citation() {
    // The one place this channel differs from the span channel, and
    // deliberately: a span's two spellings are two claims, a citation's
    // are not. `ADR-0011` and `../adr/0011-….md` name one decision, so
    // an exemption keyed `ADR 0011` covers both — otherwise a page
    // would silence its prose citation and still be reported for the
    // link in the same sentence.
    let (repo, now) = tag_repo();
    let gate = gate_with(repo.path(), now);
    let page = "---\n\
                adr-check-ignore:\n  \
                - adr: \"ADR 0011\"\n    \
                reason: historical\n    \
                since: v0.9.0\n\
                ---\n\n\
                Pre-ADR-0011 clusters ran the shim.\n\n\
                See [the original](../adr/0011-hybrid-rust-sdk-tofu-shim.md).\n";
    let findings = gate.check_source("docs/t.md", page).unwrap();
    assert!(
        of(&findings, gate::ADR_REFERENCE).is_empty(),
        "{findings:?}"
    );
}

#[test]
fn an_expired_adr_exemption_is_void_and_the_citation_is_reported() {
    // The window applies here exactly as it does everywhere else: an
    // exemption that can no longer be dated, or that is past its
    // window, silences nothing — otherwise a two-year-old `historical`
    // keeps hiding a citation nobody has re-read.
    let (repo, now) = tag_repo();
    let gate = gate_with(repo.path(), now);
    let page = "---\n\
                adr-check-ignore:\n  \
                - adr: \"ADR 0011\"\n    \
                reason: historical\n    \
                since: v0.1.0\n\
                ---\n\n\
                Storage follows ADR 0011.\n";
    let findings = gate.check_source("docs/t.md", page).unwrap();
    assert_eq!(
        of(&findings, gate::EXEMPTION_EXPIRED).len(),
        1,
        "{findings:?}"
    );
    assert_eq!(of(&findings, gate::ADR_REFERENCE).len(), 1, "{findings:?}");
    // Void, but matched — one problem, one finding.
    assert!(
        of(&findings, gate::EXEMPTION_UNUSED).is_empty(),
        "{findings:?}"
    );
}

#[test]
fn an_adr_exemption_that_silences_nothing_is_reported() {
    let (repo, now) = tag_repo();
    let gate = gate_with(repo.path(), now);
    let page = "---\n\
                adr-check-ignore:\n  \
                - adr: \"ADR 0011\"\n    \
                reason: historical\n    \
                since: v0.9.0\n\
                ---\n\n\
                This page cites the ADR that replaced it, ADR 0016.\n";
    let findings = gate.check_source("docs/t.md", page).unwrap();
    let unused = of(&findings, gate::EXEMPTION_UNUSED);
    assert_eq!(unused.len(), 1, "{findings:?}");
    assert!(unused[0].contains("ADR 0011"), "{unused:?}");
}

// ---- helpers -----------------------------------------------------------

/// A complete `Application` document with `expose` holding `field`.
fn document(field: &str) -> String {
    format!(
        "package apprafter\n\n\
         import v1alpha1 \"apprafter.io/schemas/v1alpha1\"\n\n\
         app: v1alpha1.#Application & {{\n    \
         metadata: name: \"my-service\"\n    \
         spec: base: {{\n        \
         image: \"ghcr.io/my-org/my-service:1.0.0\"\n        \
         expose: {{\n            \
         port: 8080\n            \
         {field}\n        \
         }}\n    \
         }}\n\
         }}\n"
    )
}

fn git(repo: &Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        // Hermetic: the developer's global config, hooks and signing
        // key must not decide whether this passes.
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
