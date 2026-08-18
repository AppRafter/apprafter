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
    // that does not apply — and the obligations that DO apply are
    // asserted above.
    let (repo, now) = tag_repo();
    let gate = gate_with(repo.path(), now);
    let page = "# Page\n\nProse.\n\n    just some output\n";
    let findings = gate.check_source("docs/t.md", page).unwrap();
    assert!(findings.is_empty(), "{findings:?}");
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
