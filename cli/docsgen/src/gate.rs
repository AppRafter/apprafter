// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! `docsgen gate` — the documentation drift gate.
//!
//! Six modules become one check here. [`crate::scan`] says what text is
//! a claim, [`crate::invocation`] / [`crate::identifier`] /
//! [`crate::cuedoc`] say whether a claim is true, [`crate::marker`]
//! parses the one channel an author has to say "not this one", and this
//! module decides *which check a block owes* and what a finding means.
//!
//! # Obligations come from content, never from the language tag
//!
//! Hand-written shell in this corpus carries four different tags. As
//! the corpus was **found**, at `dc4c5de` — the commit this gate was
//! written against — that was `sh` (162), `bash` (11), `console` (1)
//! and **nothing at all** (14). The gate's own unlabelled-fence
//! finding has since taken that last group to 0 (`sh` is now 164),
//! which is the argument rather than a footnote to it: the
//! distribution is a moving target, and an obligation keyed on it
//! would have moved with it. A tag-keyed gate would see a fraction of
//! the surface, and — worse — deleting a tag would turn a finding
//! green with nothing in the diff that reads as suppression. So:
//!
//! * a fence whose body holds an `apprafter` invocation owes the CLI
//!   check, whatever its tag;
//! * a fence that is a complete CUE document (a `package` clause *and*
//!   the schema import — [`cuedoc::is_complete_document`]) owes the CUE
//!   check, whatever its tag;
//! * the identifier check runs **page-wide**, over spans and fences
//!   alike, because a field name is written in both;
//! * an **unlabelled fence is itself a finding**. The tag is needed for
//!   highlighting regardless, and requiring it removes the one edit
//!   ("drop the tag") that could otherwise look innocent;
//! * a block that renders as code while carrying no fence at all — an
//!   indented run, an HTML `<pre>` ([`BlockKind::Literal`]) — owes
//!   everything a fence owes. Without that, the remedy for an
//!   unlabelled fence reads as "indent it instead of labelling it": the
//!   finding vanishes, the page renders identically, and every
//!   content-derived obligation lapses with nothing in the diff that
//!   looks like suppression. The corpus holds none, so this is a
//!   regression guard rather than a source of findings.
//!
//! A marker never *narrows* an obligation. `check=cli` on a fence that
//! is also a complete CUE document does not switch the CUE check off —
//! only `check=none` silences, and it costs a typed reason and a
//! version.
//!
//! # The page-level exemption channel
//!
//! A marker annotates the fence that follows it. An inline span can
//! carry no marker — a comment mid-sentence is unreadable as prose and
//! unreviewable as an exemption — so a span is exempted in the page's
//! **front matter**, by its literal text:
//!
//! ```yaml
//! cli-check-ignore:
//!   - span: "apprafter node reserve-headroom"
//!     reason: historical
//!     since: v0.2.44
//!     note: documents a command that was removed
//!
//! schema-check-ignore:
//!   - path: "spec.source.path"
//!     reason: external-tool
//!     since: v0.2.44
//!     note: Argo CD's Application, which our field set does not model
//! ```
//!
//! [`SPAN_IGNORE`] carries the literal span text; [`PATH_IGNORE`] the
//! literal schema path, page-wide. Both are matched by **exact
//! equality** on the trimmed text, never by substring: an exemption for
//! `apprafter node reserve-headroom` does not cover
//! `apprafter node reserve-headroom --dry-run`, which is a different
//! claim. Both go through [`marker::parse_marker`] rather than a second
//! grammar, so the closed [`Reason`] vocabulary, the `vMAJOR.MINOR.PATCH`
//! shape of `since` and the expiry window are one implementation.
//!
//! Front matter is excluded from the prose scan by [`crate::scan`],
//! which is what makes this possible at all: scanned as prose, the
//! exemption list would be read as the very span it exempts.
//!
//! # Expiry, and what CI does with it
//!
//! `since=` names a release; [`marker::age`] resolves it to that tag's
//! commit date. `actions/checkout` fetches no tags at its default
//! `fetch-depth: 1`, so on a stock runner every exemption would age to
//! [`Age::Unresolved`] — expiring correctly on a developer's machine
//! and never in the one place that gates the pull request.
//!
//! **The policy chosen here: an exemption that cannot be aged is void
//! and is reported ([`EXEMPTION_UNAGED`]), and the docs job carries
//! `fetch-depth: 0`.** An exemption whose age cannot be established is
//! an exemption that can never expire, which is exactly what the window
//! exists to prevent; treating it as a note rather than a finding would
//! leave the gate quietly toothless on every pull request. The cost is
//! a convention worth stating: **`since=` names an already-released
//! tag** — the last release, not the one being prepared — because a tag
//! that does not exist yet cannot date anything.
//!
//! An expired or unageable exemption is **void**: it does not silence.
//! Silencing on it would let a two-year-old `known-broken` keep hiding
//! a real finding, which is the failure the window exists for. It still
//! counts as *matched*, so a void exemption is not additionally
//! reported as unused.
//!
//! An exemption that silences nothing is reported ([`EXEMPTION_UNUSED`]):
//! once the page is fixed, the exemption is a claim about a problem that
//! no longer exists, and leaving it standing is how an exemption list
//! comes to cover things nobody chose.
//!
//! Two consequences are recorded here because this is where the next
//! reader meets them:
//!
//! * **Both exemptions in the corpus carry `since: v0.2.44`**, so
//!   against the 180-day window they go void on the *same day*, and
//!   from that day `just lint` fails for everyone until each is
//!   re-justified. That is the window working as designed, but it
//!   arrives as one event rather than two, and whoever meets it is
//!   re-reading two pages rather than one. Staggering `since=` across
//!   exemptions is the only thing that would spread the cost, and it
//!   is not worth lying about a date to get it.
//! * **A shallow or tagless checkout produces the same failure.**
//!   [`marker::age`] cannot distinguish "this tag is 200 days old"
//!   from "this clone has no tags", so `git clone --depth 1`, a fork
//!   that never fetched tags, or any CI job without `fetch-depth: 0`
//!   reports [`EXEMPTION_UNAGED`] for every exemption at once. The
//!   exit code is **1**, which reads as "the documentation is wrong",
//!   when the remedy is `git fetch --tags` — a checkout to repair, not
//!   a page. Only the finding's remedy text carries that distinction
//!   ([`remedy`]), so a caller that prints codes and swallows text
//!   will mislead its reader.
//!
//! # Exit codes
//!
//! `main` maps this module's three outcomes to **0** (no findings),
//! **1** (findings) and **2** (the gate itself broke — no `cue` on
//! `PATH`, an unreadable page, a `cue` diagnostic naming none of the
//! documents under test). Reporting our own breakage as a clean run is
//! the worst available outcome, and reporting it as documentation drift
//! is the second worst; both are why [`cuedoc::validate_documents`]'s
//! `Err` is propagated here rather than folded into the finding list.

use crate::codepath;
use crate::cuedoc::{self, Document};
use crate::identifier::{self, FieldSet};
use crate::invocation::{self, Tree};
use crate::marker::{self, Age, Check, Marker, Reason};
use crate::model;
use crate::scan::{self, BlockKind};

use clap::CommandFactory;
use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::SystemTime;

/// A documented `apprafter …` line that does not resolve against the
/// clap tree.
pub const CLI_INVOCATION: &str = "cli-invocation";
/// A documented schema path no shipped schema declares, or one naming a
/// `needs` type no provider ships.
pub const SCHEMA_IDENTIFIER: &str = "schema-identifier";
/// A complete CUE manifest `cue vet` rejects against the shipped
/// schemas.
pub const CUE_DOCUMENT: &str = "cue-document";
/// A fence with no info string.
pub const UNLABELLED_FENCE: &str = "unlabelled-fence";
/// A fence that runs to the end of the file without a closing
/// delimiter, so everything below it renders as code.
pub const UNTERMINATED_FENCE: &str = "unterminated-fence";
/// An HTML `<pre>` that never meets its closing tag, so the rest of the
/// page is inside it — see the module docs on why a block that renders
/// as code without a fence is this gate's business at all.
///
/// Its own class rather than [`UNTERMINATED_FENCE`] because the two
/// remedies are different — one closes backticks, the other a tag — and
/// the report prints one remedy per class, so folding them together
/// would send half its readers to the wrong edit.
pub const UNCLOSED_PRE: &str = "unclosed-pre";
/// A `<!-- docs: … -->` marker that is malformed, or that claims a
/// check this gate cannot run on that block.
pub const DOCS_MARKER: &str = "docs-marker";
/// A malformed entry in a page's front-matter exemption list.
pub const FRONT_MATTER: &str = "front-matter";
/// An exemption past the [`marker::EXPIRY_DAYS`] window.
pub const EXEMPTION_EXPIRED: &str = "exemption-expired";
/// An exemption whose `since=` names no tag this checkout has.
pub const EXEMPTION_UNAGED: &str = "exemption-unaged";
/// An exemption that silences nothing.
pub const EXEMPTION_UNUSED: &str = "exemption-unused";
/// A path this documentation names does not exist in the repository.
pub const CODE_PATH: &str = "code-path";

/// Front-matter key exempting an inline span from the CLI check, by the
/// span's literal text.
pub const SPAN_IGNORE: &str = "cli-check-ignore";
/// Front-matter key exempting a schema path from the identifier check,
/// page-wide, by the path's literal text.
pub const PATH_IGNORE: &str = "schema-check-ignore";

/// What to do about a finding, named per class. A report that leads
/// with one remedy for everything sends most of its readers to the
/// wrong place, so each class carries its own.
fn remedy(code: &str) -> &'static str {
    match code {
        CLI_INVOCATION => {
            "fix the command against `apprafter --help` (or docs/reference/cli/), \
             or — if it is deliberately historical or third-party — exempt it: a \
             fence with `<!-- docs: check=none reason=… since=vX.Y.Z — why -->`, \
             a span with a `cli-check-ignore` entry in the page's front matter"
        }
        SCHEMA_IDENTIFIER => {
            "fix the field path against schemas/v1alpha1 (the `Kind.spec.…` form \
             resolves against that kind alone and is the one to prefer), or exempt \
             it with a `schema-check-ignore` entry in the page's front matter"
        }
        CUE_DOCUMENT => {
            "fix the manifest until `apprafter app validate` accepts it — a reader \
             copies this block verbatim"
        }
        UNLABELLED_FENCE => {
            "give the fence an info string (```sh, ```cue, ```yaml, ```text …) — \
             highlighting needs it, and the gate records the tag so that deleting \
             one cannot quietly change what is checked"
        }
        UNTERMINATED_FENCE => "close the fence — everything below it renders as code",
        UNCLOSED_PRE => {
            "close the `<pre>` tag — everything below it is inside the block, so it \
             renders as code and no fence below it is read as a fence"
        }
        DOCS_MARKER => "fix the marker — the grammar is in cli/docsgen/src/marker.rs",
        FRONT_MATTER => {
            "fix the exemption entry — the keys are span/path, reason, since and \
             an optional note"
        }
        EXEMPTION_EXPIRED => {
            "re-justify the exemption with a fresh `since=` after confirming it is \
             still true, or fix the page and delete it"
        }
        EXEMPTION_UNAGED => {
            "`since=` must name a tag this checkout has — use the last RELEASED \
             version, and give the docs job `fetch-depth: 0` so CI can see tags"
        }
        EXEMPTION_UNUSED => {
            "the exemption silences nothing — delete it, or fix the literal text it \
             names (matching is exact, not substring)"
        }
        CODE_PATH => {
            "A path named here is not in the repository. Correct it, \
             or — if it is a path in someone else's project — say whose \
             it is in the sentence, because a reader will otherwise \
             grep this repository for it."
        }
        _ => "see cli/docsgen/src/gate.rs",
    }
}

/// One thing the gate objects to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    /// Repo-relative path of the page, as a reader would grep for it.
    pub file: String,
    /// 1-based source line.
    pub line: usize,
    /// The finding class, one of the `pub const`s above. Stable and
    /// greppable: a contributor discussing a finding names this.
    pub code: &'static str,
    /// What is wrong, in one line.
    pub message: String,
    /// What to do about it — [`remedy`] of [`Finding::code`].
    pub remedy: &'static str,
}

impl std::fmt::Display for Finding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}:{} [{}] {}",
            self.file, self.line, self.code, self.message
        )
    }
}

fn finding(code: &'static str, file: &str, line: usize, message: String) -> Finding {
    Finding {
        file: file.to_string(),
        line,
        code,
        message,
        remedy: remedy(code),
    }
}

/// What the corpus looked like, for the line printed on every run.
///
/// These numbers used to live in three `#[ignore]`d corpus surveys, one
/// per check. They are here now because two code paths over the same
/// corpus can disagree, and the one nobody runs is the one that drifts.
#[derive(Debug, Default, Clone)]
pub struct Stats {
    /// Every `apprafter` invocation seen, exempted ones included.
    pub invocations: usize,
    /// Schema identifiers that resolved.
    pub identifiers: usize,
    /// How many of those resolved **only** because they sit under a
    /// subtree the CRDs do not describe. Reported because reading the
    /// resolved total as a *checked* total would be a real
    /// overstatement — the opaque share is a shade under half.
    ///
    /// No figure is repeated in this comment on purpose: [`Stats::line`]
    /// prints both numbers on every run, and a copy here would be a
    /// third number to keep in step with the corpus and with
    /// [`identifier`]'s module docs. See [`identifier::Verdict::opaque`].
    pub opaque: usize,
    /// One entry per exemption declared, so they can be counted by kind
    /// — which is the whole point of the closed [`Reason`] vocabulary.
    pub exemptions: Vec<Reason>,
}

impl Stats {
    fn absorb(&mut self, other: Stats) {
        self.invocations += other.invocations;
        self.identifiers += other.identifiers;
        self.opaque += other.opaque;
        self.exemptions.extend(other.exemptions);
    }

    /// The one-line census, printed whether the gate passes or fails.
    pub fn line(&self, pages: usize, documents: usize) -> String {
        let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
        for reason in &self.exemptions {
            *counts.entry(reason.as_str()).or_default() += 1;
        }
        let exemptions = if counts.is_empty() {
            "0 exemptions".to_string()
        } else {
            let by_kind: Vec<String> = counts
                .iter()
                .map(|(kind, count)| format!("{count} {kind}"))
                .collect();
            format!(
                "{} exemptions ({})",
                self.exemptions.len(),
                by_kind.join(", ")
            )
        };
        format!(
            "docsgen gate: {pages} page(s), {} invocation(s), {} identifier(s) resolved \
             ({} of them only under an opaque subtree), {documents} complete CUE \
             document(s), {exemptions}",
            self.invocations, self.identifiers, self.opaque
        )
    }
}

/// Everything one page contributed.
///
/// The CUE documents come back rather than being vetted here: `cue`
/// costs 69 ms per invocation inside `nix develop` and 599 ms outside
/// it, so the corpus goes through **one** batch, not one per page.
pub struct Page {
    pub findings: Vec<Finding>,
    pub documents: Vec<Document>,
    pub stats: Stats,
}

/// A parsed exemption, from a fence marker or from front matter.
struct Exemption {
    /// The literal text it names: a span's text, or a schema path.
    /// Empty for a fence marker, which names the fence it precedes.
    literal: String,
    reason: Reason,
    since: String,
    /// Source line to report it on.
    line: usize,
    /// It matched something the gate would otherwise have reported.
    /// Tracked separately from "silenced" so a **void** exemption is
    /// not also reported as unused — one problem, one finding.
    matched: bool,
    /// Expired or unageable: reported, and silences nothing.
    void: bool,
}

impl Exemption {
    /// Whether it may silence: matched *and* still standing.
    fn silences(&self) -> bool {
        !self.void
    }
}

/// The gate, with everything it resolves against loaded once.
pub struct Gate {
    tree: Tree,
    fields: FieldSet,
    /// Every tracked path, as `git ls-files` spells it.
    tracked: BTreeSet<String>,
    /// Every directory one of those paths passes through. Derived
    /// rather than listed, because `git ls-files` reports blobs and
    /// pages name directories constantly — "the crates under
    /// `cli/docsgen/src`". Resolving only blobs would report every one
    /// of them.
    directories: BTreeSet<String>,
    /// The repository's top-level directory names, which is what makes
    /// a slash-shaped code span a path claim at all. Derived from the
    /// same listing, so the grammar closes over the tree instead of
    /// over a hand-kept list that can go stale.
    tops: Vec<String>,
    /// Where the corpus and the schemas live.
    repo_root: PathBuf,
    /// The repository whose tags date `since=`.
    ///
    /// A parameter rather than `repo_root` for the same reason
    /// [`marker::age`] takes `now`: the expiry rule has to be testable
    /// against a repository built for the test. A shallow clone has no
    /// tags, so a test that leaned on the real ones would pass on a
    /// developer's machine and fail in CI — the exact asymmetry this
    /// module's expiry policy exists to close.
    tags: PathBuf,
    now: SystemTime,
}

impl Gate {
    /// Load the clap tree and the shipped field set, aging exemptions
    /// against this repository as of now.
    pub fn new(repo_root: &Path) -> Result<Gate, Box<dyn Error>> {
        Gate::pinned(repo_root, repo_root, SystemTime::now())
    }

    /// As [`Gate::new`], with the tag repository and the instant fixed.
    pub fn pinned(repo_root: &Path, tags: &Path, now: SystemTime) -> Result<Gate, Box<dyn Error>> {
        // The live clap tree, not the committed `commands.json`:
        // `docsgen check` already byte-compares the two, and resolving
        // against the binary's own definitions means a stale reference
        // cannot make the gate agree with it.
        let cli = apprafter::docs_api::Cli::command();
        let version = cli.get_version().unwrap_or("unknown").to_string();
        // Once, here, rather than once per page: the listing is the
        // same for every page, and shelling out 33 times would put a
        // subprocess per page in a pre-commit path.
        let tracked = tracked_files(repo_root)?;
        let mut directories = BTreeSet::new();
        let mut tops = BTreeSet::new();
        for path in &tracked {
            let components: Vec<&str> = path.split('/').collect();
            // Every component but the last: the last is the blob.
            let mut prefix = String::new();
            for component in &components[..components.len() - 1] {
                if !prefix.is_empty() {
                    prefix.push('/');
                }
                prefix.push_str(component);
                directories.insert(prefix.clone());
            }
            if components.len() > 1 {
                tops.insert(components[0].to_string());
            }
        }
        Ok(Gate {
            tree: Tree::from_model(model::tree_from(&cli, &version))?,
            fields: FieldSet::from_repo(repo_root)?,
            tracked,
            directories,
            tops: tops.into_iter().collect(),
            repo_root: repo_root.to_path_buf(),
            tags: tags.to_path_buf(),
            now,
        })
    }

    /// Whether the repository holds this path — as a file, as a
    /// directory, or as the root itself.
    ///
    /// The root is reachable: a page in `docs/` writing `[the
    /// repository](..)` names it, and it exists however empty the
    /// normalised string is.
    fn resolves(&self, path: &str) -> bool {
        path.is_empty() || self.tracked.contains(path) || self.directories.contains(path)
    }

    /// Run every check over the whole in-scope corpus.
    ///
    /// `Err` is the gate's own breakage, never a page's — see the
    /// module docs on exit codes.
    pub fn corpus(&self) -> Result<Vec<Finding>, Box<dyn Error>> {
        // Named rather than propagated bare: the failure mode is `git`
        // missing from PATH, and the io error alone is "No such file or
        // directory (os error 2)" with nothing to act on.
        let files = scan::in_scope(&self.repo_root).map_err(|e| {
            format!("could not list the in-scope documentation (is `git` on PATH?): {e}")
        })?;
        let mut findings = Vec::new();
        let mut documents = Vec::new();
        let mut stats = Stats::default();

        for path in &files {
            let shown = path
                .strip_prefix(&self.repo_root)
                .unwrap_or(path)
                .display()
                .to_string();
            let source =
                std::fs::read_to_string(path).map_err(|e| format!("reading {shown}: {e}"))?;
            let page = self.page(&shown, &source);
            findings.extend(page.findings);
            documents.extend(page.documents);
            stats.absorb(page.stats);
        }

        findings.extend(self.vet(&documents)?);
        sort(&mut findings);
        eprintln!("{}", stats.line(files.len(), documents.len()));
        Ok(findings)
    }

    /// Every finding on one page, CUE documents included — the shape a
    /// test wants, and the shape [`Gate::corpus`] deliberately does not
    /// use, because it would run `cue` once per page.
    pub fn check_source(&self, file: &str, source: &str) -> Result<Vec<Finding>, Box<dyn Error>> {
        let page = self.page(file, source);
        let mut findings = page.findings;
        findings.extend(self.vet(&page.documents)?);
        sort(&mut findings);
        Ok(findings)
    }

    /// Vet a batch of documents and put each finding back on its page.
    ///
    /// [`cuedoc::validate_documents`] returns `Err` when `cue` says
    /// something naming none of the documents under test. That is the
    /// harness being wrong, not a page, so it is propagated with its own
    /// wrapper rather than turned into a finding a reader would take to
    /// a page that is fine.
    fn vet(&self, documents: &[Document]) -> Result<Vec<Finding>, Box<dyn Error>> {
        let raw = cuedoc::validate_documents(documents).map_err(|e| {
            format!(
                "the CUE half of the gate could not run, so nothing was vetted — \
                 this is the gate being broken, not the documentation:\n{e}"
            )
        })?;
        Ok(raw
            .into_iter()
            .map(|found| {
                let (file, anchor) = split_origin(&found.origin);
                // The document is laid out unwrapped, so cue's line is a
                // 1-based offset into the block body, which starts on
                // the line after the anchor the origin carries.
                let line = anchor + found.line.unwrap_or(0);
                finding(CUE_DOCUMENT, &file, line, found.message)
            })
            .collect())
    }

    /// Derive every block's obligation on one page and apply it.
    pub fn page(&self, file: &str, source: &str) -> Page {
        let blocks = scan::scan_markdown(source);
        let mut findings = Vec::new();
        let mut documents = Vec::new();
        let mut stats = Stats::default();

        let front = blocks
            .iter()
            .find(|block| block.kind == BlockKind::FrontMatter);
        let (mut spans, mut paths) = match front {
            Some(block) => self.exemptions(file, &block.body, &mut findings),
            None => (Vec::new(), Vec::new()),
        };
        for exemption in spans.iter_mut().chain(paths.iter_mut()) {
            stats.exemptions.push(exemption.reason);
            self.age(file, exemption, &mut findings);
        }

        // (source line, token) from every surface on the page. The
        // identifier check is page-wide, not fence-scoped: a field name
        // is written in a fence, a table cell and a prose span, and the
        // same token in two places is one claim.
        let mut seen: Vec<(usize, String)> = Vec::new();

        for block in &blocks {
            match &block.kind {
                // Never a claim: it is where the page addresses the
                // gate, and it was read above.
                BlockKind::FrontMatter => {}
                // A literal block owes exactly what a fence owes: the
                // obligation is read off `body`, which both carry, and
                // any other rule would make "delete the delimiters" a
                // way to shed it. Only the two findings keyed to
                // delimiters a literal block does not have are fenced
                // off below.
                BlockKind::Fence { .. } | BlockKind::Literal => {
                    // Which of the two shapes this arm has. Spelled out
                    // over every variant rather than defaulted with
                    // `_`, so that a variant added later and routed
                    // here is a compile error instead of a silent
                    // "unclosed `<pre>`".
                    let fenced = match &block.kind {
                        BlockKind::Fence { .. } => true,
                        BlockKind::Literal => false,
                        BlockKind::FrontMatter | BlockKind::InlineSpan => {
                            unreachable!("this arm matches Fence and Literal only")
                        }
                    };

                    if block.unterminated {
                        // Both delimiters, named honestly. A literal
                        // block is only ever flagged when it is a
                        // `<pre>`: the indented form has no closing
                        // delimiter to be missing.
                        let (code, message) = if fenced {
                            (
                                UNTERMINATED_FENCE,
                                "the fence is never closed, so the rest of the page \
                                 renders as code",
                            )
                        } else {
                            (
                                UNCLOSED_PRE,
                                "the `<pre>` holding this block is never closed, so the \
                                 rest of the page is inside it: it renders as code, and \
                                 no fence below it is read as a fence",
                            )
                        };
                        findings.push(finding(code, file, block.line, message.to_string()));
                    }
                    // Never reported for a literal block: it has no
                    // fence to label, and the remedy — fence it — is
                    // not what this finding's text asks for.
                    if matches!(&block.kind, BlockKind::Fence { tag } if tag.is_none()) {
                        findings.push(finding(
                            UNLABELLED_FENCE,
                            file,
                            block.line,
                            "the fence carries no info string".to_string(),
                        ));
                    }

                    // Always `None` for a literal block, so a marker can
                    // neither annotate nor silence one.
                    let marker =
                        self.fence_marker(file, block.line, &block.tag_line, &mut findings);

                    // Content, not tag: both obligations are read off
                    // the body before any marker is consulted.
                    //
                    // The block's anchor: the line `vet` adds cue's
                    // 1-based line number to, and so the line the body
                    // starts *after*. A fence anchors on its opening
                    // delimiter; a literal block has none, so it
                    // anchors on the line above its body. Both derive
                    // from one value rather than each carrying its own
                    // ±1, which was correct before only by the two
                    // cancelling.
                    let anchor = if fenced {
                        block.line
                    } else {
                        block.line.saturating_sub(1)
                    };
                    let body_line = anchor + 1;
                    let mut invocations = Vec::new();
                    for (offset, text) in invocation::logical_lines(&block.body) {
                        for found in invocation::extract(&text) {
                            invocations.push((body_line + offset, found));
                        }
                    }
                    let document = cuedoc::is_complete_document(&block.body);
                    stats.invocations += invocations.len();

                    if let Some((marker, _)) = &marker {
                        findings.extend(audit(
                            file,
                            block.line,
                            marker,
                            !invocations.is_empty(),
                            document,
                        ));
                    }
                    let silenced = marker.as_ref().is_some_and(|(m, exemption)| {
                        m.check == Check::None
                            && exemption.as_ref().is_some_and(Exemption::silences)
                    });
                    if silenced {
                        continue;
                    }

                    for (line, found) in &invocations {
                        if let Err(e) = invocation::resolve(&self.tree, found) {
                            findings.push(finding(
                                CLI_INVOCATION,
                                file,
                                *line,
                                format!("`{}` — {e}", found.raw),
                            ));
                        }
                    }
                    if document {
                        documents.push(Document {
                            origin: format!("{file}:{anchor}"),
                            body: block.body.clone(),
                        });
                    }
                    for (offset, line) in block.body.lines().enumerate() {
                        for token in identifier::extract_paths(line) {
                            seen.push((body_line + offset, token));
                        }
                    }
                    for (offset, token) in identifier::extract_structure(&block.body) {
                        seen.push((body_line + offset, token));
                    }
                }
                BlockKind::InlineSpan => {
                    let mut invocations = Vec::new();
                    for (_, text) in invocation::logical_lines(&block.body) {
                        invocations.extend(invocation::extract(&text));
                    }
                    stats.invocations += invocations.len();

                    let mut problems = Vec::new();
                    for found in &invocations {
                        if let Err(e) = invocation::resolve(&self.tree, found) {
                            problems.push(format!("`{}` — {e}", found.raw));
                        }
                    }
                    if !problems.is_empty() {
                        let literal = block.body.trim();
                        let exempt = spans.iter_mut().find(|e| e.literal == literal);
                        let silenced = match exempt {
                            Some(exemption) => {
                                exemption.matched = true;
                                exemption.silences()
                            }
                            None => false,
                        };
                        if !silenced {
                            for message in problems {
                                findings.push(finding(CLI_INVOCATION, file, block.line, message));
                            }
                        }
                    }
                    if let Some(token) = identifier::span_path(&block.body) {
                        seen.push((block.line, token));
                    }
                }
            }
        }

        seen.sort();
        seen.dedup();
        for (line, token) in seen {
            let problem = match identifier::resolve_path(&self.fields, &token) {
                Err(reason) => Some(reason),
                Ok(verdict) if verdict.unshipped => Some(format!(
                    "`{token}`: declared in the schema but no provider ships it, so a \
                     claim of this type never leaves AwaitingResourceClaim"
                )),
                Ok(verdict) => {
                    stats.identifiers += 1;
                    stats.opaque += usize::from(verdict.opaque);
                    None
                }
            };
            let Some(message) = problem else { continue };
            let exempt = paths.iter_mut().find(|e| e.literal == token);
            let silenced = match exempt {
                Some(exemption) => {
                    exemption.matched = true;
                    exemption.silences()
                }
                None => false,
            };
            if !silenced {
                findings.push(finding(SCHEMA_IDENTIFIER, file, line, message));
            }
        }

        // Page-wide and read off the source rather than off a block,
        // because the two shapes a path claim takes — a code span and a
        // link target — are inline syntax that `scan` deliberately does
        // not distinguish: it hands back a span's text with no offset
        // and no idea whether that span was a link's label. See
        // [`codepath`] for why the distinction decides the answer.
        for reference in codepath::references(source, &self.tops) {
            let directory = page_directory(file);
            let resolved = if reference.page_relative {
                normalise(directory, &reference.path)
            } else {
                Some(reference.path.clone())
            };
            let message = match resolved {
                Some(target) if self.resolves(&target) => continue,
                Some(target) if reference.page_relative => format!(
                    "`{}`: no such path — from `{directory}/` it resolves to `{target}`, \
                     which is not in the repository",
                    reference.path
                ),
                Some(target) => format!("`{target}`: no such path in the repository"),
                None => format!(
                    "`{}`: no such path — from `{directory}/` it climbs above the \
                     repository root",
                    reference.path
                ),
            };
            findings.push(finding(CODE_PATH, file, reference.line, message));
        }

        for exemption in spans.iter().chain(paths.iter()) {
            if !exemption.matched {
                findings.push(finding(
                    EXEMPTION_UNUSED,
                    file,
                    exemption.line,
                    format!(
                        "the `{}` exemption for `{}` silences nothing on this page",
                        exemption.reason.as_str(),
                        exemption.literal
                    ),
                ));
            }
        }

        sort(&mut findings);
        Page {
            findings,
            documents,
            stats,
        }
    }

    /// Parse the comment above a fence, and age it if it is an
    /// exemption.
    ///
    /// A malformed marker is reported **and ignored**: the block keeps
    /// every obligation its content gives it. Reading a broken marker as
    /// an exemption would make a typo the cheapest way to switch a check
    /// off.
    fn fence_marker(
        &self,
        file: &str,
        fence_line: usize,
        tag_line: &Option<String>,
        findings: &mut Vec<Finding>,
    ) -> Option<(Marker, Option<Exemption>)> {
        let text = tag_line.as_ref()?;
        // The comment sits on the line immediately above the fence.
        let line = fence_line.saturating_sub(1).max(1);
        match marker::parse_marker(text) {
            Ok(None) => None,
            Ok(Some(marker)) => {
                let exemption = (marker.check == Check::None).then(|| {
                    let mut exemption = Exemption {
                        literal: String::new(),
                        reason: marker.reason.expect("check=none carries a reason"),
                        since: marker.since.clone().expect("check=none carries a since"),
                        line,
                        // A fence marker names the fence it precedes, so
                        // it always has its target: unused is not a
                        // question the way it is for a front-matter
                        // entry that names text by hand.
                        matched: true,
                        void: false,
                    };
                    self.age(file, &mut exemption, findings);
                    exemption
                });
                Some((marker, exemption))
            }
            Err(e) => {
                findings.push(finding(DOCS_MARKER, file, line, e));
                None
            }
        }
    }

    /// Age an exemption, marking it void when it cannot stand.
    fn age(&self, file: &str, exemption: &mut Exemption, findings: &mut Vec<Finding>) {
        let named = if exemption.literal.is_empty() {
            "the fence below".to_string()
        } else {
            format!("`{}`", exemption.literal)
        };
        match marker::age(&exemption.since, &self.tags, self.now) {
            Age::Days(days) if days > marker::EXPIRY_DAYS => {
                exemption.void = true;
                findings.push(finding(
                    EXEMPTION_EXPIRED,
                    file,
                    exemption.line,
                    format!(
                        "the `{}` exemption for {named} was taken in {} — {days} days \
                         ago, past the {}-day window, so it no longer silences anything",
                        exemption.reason.as_str(),
                        exemption.since,
                        marker::EXPIRY_DAYS
                    ),
                ));
            }
            Age::Unresolved(why) => {
                exemption.void = true;
                findings.push(finding(
                    EXEMPTION_UNAGED,
                    file,
                    exemption.line,
                    format!(
                        "the `{}` exemption for {named} names `since={}`, which this \
                         checkout cannot date ({why}) — an exemption that cannot be \
                         aged can never expire, so it silences nothing",
                        exemption.reason.as_str(),
                        exemption.since
                    ),
                ));
            }
            Age::Days(_) => {}
        }
    }

    /// Read a page's front-matter exemption lists.
    fn exemptions(
        &self,
        file: &str,
        body: &str,
        findings: &mut Vec<Finding>,
    ) -> (Vec<Exemption>, Vec<Exemption>) {
        let document: serde_yaml::Value = match serde_yaml::from_str(body) {
            Ok(value) => value,
            Err(e) => {
                findings.push(finding(
                    FRONT_MATTER,
                    file,
                    1,
                    format!("the page's front matter is not valid YAML: {e}"),
                ));
                return (Vec::new(), Vec::new());
            }
        };
        let Some(map) = document.as_mapping() else {
            // Empty front matter parses to `null`, which is fine and
            // declares nothing.
            return (Vec::new(), Vec::new());
        };
        (
            read_list(file, body, map, SPAN_IGNORE, "span", findings),
            read_list(file, body, map, PATH_IGNORE, "path", findings),
        )
    }
}

/// Every path `git` tracks in `repo_root`, as repo-relative strings.
///
/// The same call, the same flags and the same error wrapping as
/// [`scan::in_scope`] — `-z` because git C-quotes unusual names
/// otherwise, which would silently drop them from the set and turn a
/// correct reference into a finding. Named rather than propagated bare
/// for the reason [`Gate::corpus`] names its own: the failure mode is
/// `git` missing from `PATH`, and the io error alone is "No such file
/// or directory (os error 2)", which reads as the documentation being
/// wrong rather than the gate being broken.
fn tracked_files(repo_root: &Path) -> Result<BTreeSet<String>, Box<dyn Error>> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["ls-files", "-z"])
        .output()
        .map_err(|e| format!("could not list the repository (is `git` on PATH?): {e}"))?;
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

/// Resolve `target` against the directory `dir`, **lexically**.
///
/// No filesystem call is made and none may be: an answer that came from
/// the disk would depend on a checkout's case sensitivity and on
/// whatever untracked file happens to sit there, so the same page would
/// pass on one machine and fail on another. `..` pops a component and
/// `.` drops out; popping above the root is unresolvable and says so.
fn normalise(dir: &str, target: &str) -> Option<String> {
    let mut parts: Vec<&str> = Vec::new();
    for segment in dir.split('/').chain(target.split('/')) {
        match segment {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            other => parts.push(other),
        }
    }
    Some(parts.join("/"))
}

/// The directory a page sits in, empty for one at the repository root.
fn page_directory(file: &str) -> &str {
    file.rsplit_once('/').map_or("", |(dir, _)| dir)
}

/// Whether a marker claims something this gate cannot do on this block.
///
/// A marker that reads as doing more than it does is worse than no
/// marker, because a reviewer takes it at its word — the same reasoning
/// that makes an unknown key an error in [`marker`] rather than an
/// ignored one.
fn audit(
    file: &str,
    fence_line: usize,
    marker: &Marker,
    has_invocation: bool,
    is_document: bool,
) -> Option<Finding> {
    let line = fence_line.saturating_sub(1).max(1);
    let message = if marker.run.is_some() {
        "`run=local` is not implemented by this gate: it checks the command path \
         and flag names only, never values or required arguments"
            .to_string()
    } else if marker.kind.is_some() {
        "`kind=` is not implemented by this gate: the CUE check vets complete \
         documents as written and lays in no alternative kind"
            .to_string()
    } else if marker.check == Check::Cli && !has_invocation {
        "`check=cli` on a fence that holds no `apprafter` invocation — the marker \
         checks nothing"
            .to_string()
    } else if marker.check == Check::Cue && !is_document {
        "`check=cue` on a fence that is not a complete CUE document (it needs a \
         `package` clause and the schema import), so `cue vet` would skip it"
            .to_string()
    } else {
        return None;
    };
    Some(finding(DOCS_MARKER, file, line, message))
}

/// Read one front-matter exemption list.
fn read_list(
    file: &str,
    body: &str,
    map: &serde_yaml::Mapping,
    key: &str,
    literal_key: &str,
    findings: &mut Vec<Finding>,
) -> Vec<Exemption> {
    let Some(value) = map.get(serde_yaml::Value::from(key)) else {
        return Vec::new();
    };
    let at = line_of(body, key);
    let Some(entries) = value.as_sequence() else {
        findings.push(finding(
            FRONT_MATTER,
            file,
            at,
            format!("`{key}` must be a list of exemption entries"),
        ));
        return Vec::new();
    };
    let mut out = Vec::new();
    for (index, entry) in entries.iter().enumerate() {
        match read_entry(entry, literal_key) {
            Ok((literal, marker)) => {
                let line = line_of(body, &literal);
                out.push(Exemption {
                    reason: marker.reason.expect("check=none carries a reason"),
                    since: marker.since.expect("check=none carries a since"),
                    literal,
                    line,
                    matched: false,
                    void: false,
                });
            }
            Err(e) => findings.push(finding(
                FRONT_MATTER,
                file,
                at,
                format!("`{key}`[{index}]: {e}"),
            )),
        }
    }
    out
}

/// Validate one exemption entry, returning its literal text and the
/// marker it is equivalent to.
///
/// The fields are handed to [`marker::parse_marker`] as a reconstructed
/// marker line rather than validated here, so the closed reason
/// vocabulary, the release-tag shape of `since` and the
/// "check=none needs both" rule are one implementation and cannot
/// diverge. The free-text `note` is deliberately left out of the
/// reconstruction: it is the one field with no grammar, and an em dash
/// inside it would split the marker.
fn read_entry(entry: &serde_yaml::Value, literal_key: &str) -> Result<(String, Marker), String> {
    let Some(map) = entry.as_mapping() else {
        return Err("each entry must be a mapping".to_string());
    };
    let mut literal: Option<String> = None;
    let mut reason: Option<String> = None;
    let mut since: Option<String> = None;
    for (key, value) in map {
        let Some(key) = key.as_str() else {
            return Err("every key must be a string".to_string());
        };
        let Some(text) = value.as_str() else {
            return Err(format!("`{key}` must be a string"));
        };
        let slot = match key {
            _ if key == literal_key => &mut literal,
            "reason" => &mut reason,
            "since" => &mut since,
            // The one field with no grammar, kept for the reader and
            // not read by the gate.
            "note" => continue,
            other => {
                return Err(format!(
                    "unknown key `{other}` — known keys are {literal_key}, reason, \
                     since, note"
                ))
            }
        };
        if slot.is_some() {
            return Err(format!("`{key}` given twice"));
        }
        *slot = Some(text.to_string());
    }

    let Some(literal) = literal.filter(|text| !text.trim().is_empty()) else {
        return Err(format!(
            "`{literal_key}:` is required — an exemption must name the exact text it \
             covers, because matching is exact and not substring"
        ));
    };
    let reason = reason.ok_or("`reason:` is required — an exemption must say why")?;
    let since = since.ok_or("`since:` is required — an exemption must say since when")?;
    for (name, value) in [("reason", &reason), ("since", &since)] {
        if value.split_whitespace().count() != 1 {
            return Err(format!("`{name}: {value}` must be a single token"));
        }
    }
    let line = format!("<!-- docs: check=none reason={reason} since={since} -->");
    let marker = marker::parse_marker(&line)?
        .ok_or_else(|| format!("could not read `{line}` as an exemption"))?;
    Ok((literal.trim().to_string(), marker))
}

/// The 1-based source line of the first front-matter line holding
/// `needle`, so an exemption is reported where it was written.
///
/// Front matter opens on line 1 with `---`, so its body's first line is
/// source line 2.
fn line_of(body: &str, needle: &str) -> usize {
    body.lines()
        .position(|line| line.contains(needle))
        .map_or(1, |at| at + 2)
}

/// Split a [`Document::origin`] back into its page and fence line.
fn split_origin(origin: &str) -> (String, usize) {
    match origin.rsplit_once(':') {
        Some((file, line)) => (file.to_string(), line.parse().unwrap_or(0)),
        None => (origin.to_string(), 0),
    }
}

/// Group by class, then by place. The report prints one remedy per
/// class, so a stable class-major order is what makes it readable —
/// and a deterministic one is what keeps two runs comparable.
fn sort(findings: &mut [Finding]) {
    findings.sort_by(|a, b| {
        (a.code, &a.file, a.line, &a.message).cmp(&(b.code, &b.file, b.line, &b.message))
    });
}

/// Run the gate over `repo_root`'s in-scope documentation.
pub fn run(repo_root: &Path) -> Result<Vec<Finding>, Box<dyn Error>> {
    Gate::new(repo_root)?.corpus()
}

/// The report, one block, grouped by class with that class's remedy.
///
/// Same shape as `docsgen check`'s: everything is accumulated and
/// printed once, because a gate that reports one problem per run costs
/// a CI round-trip per fix and teaches contributors to stop running it.
pub fn report(findings: &[Finding]) -> String {
    let mut by_class: BTreeMap<&str, Vec<&Finding>> = BTreeMap::new();
    for found in findings {
        by_class.entry(found.code).or_default().push(found);
    }
    let mut out = format!(
        "docs-gate FAILED: {} documentation problem(s) in {} class(es).\n",
        findings.len(),
        by_class.len()
    );
    for (code, group) in &by_class {
        out.push_str(&format!(
            "\n  [{code} ×{}] {}\n",
            group.len(),
            group[0].remedy
        ));
        for found in group {
            out.push_str(&format!(
                "    {}:{}  {}\n",
                found.file, found.line, found.message
            ));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_code_names_its_own_remedy() {
        // A class whose remedy fell through to the placeholder would
        // print "see gate.rs" at the head of its block, which is not a
        // remedy.
        for code in [
            CLI_INVOCATION,
            SCHEMA_IDENTIFIER,
            CUE_DOCUMENT,
            UNLABELLED_FENCE,
            UNTERMINATED_FENCE,
            UNCLOSED_PRE,
            DOCS_MARKER,
            FRONT_MATTER,
            EXEMPTION_EXPIRED,
            EXEMPTION_UNAGED,
            EXEMPTION_UNUSED,
            CODE_PATH,
        ] {
            assert_ne!(remedy(code), remedy("no-such-code"), "{code}");
        }
    }

    #[test]
    fn an_origin_round_trips_to_a_page_and_a_line() {
        assert_eq!(
            split_origin("docs/dev-guide/application-cue.md:42"),
            ("docs/dev-guide/application-cue.md".to_string(), 42)
        );
        assert_eq!(split_origin("no-colon"), ("no-colon".to_string(), 0));
    }

    #[test]
    fn a_front_matter_line_is_reported_where_it_was_written() {
        // Body line 0 is source line 2: `---` occupies line 1.
        let body = "title: x\ncli-check-ignore:\n  - span: \"apprafter promote\"\n";
        assert_eq!(line_of(body, "cli-check-ignore"), 3);
        assert_eq!(line_of(body, "apprafter promote"), 4);
        assert_eq!(line_of(body, "absent"), 1);
    }

    #[test]
    fn an_entry_is_validated_through_the_marker_grammar() {
        let ok = serde_yaml::from_str::<serde_yaml::Value>(
            "span: \"apprafter promote\"\nreason: historical\nsince: v0.2.44\nnote: gone\n",
        )
        .unwrap();
        let (literal, marker) = read_entry(&ok, "span").unwrap();
        assert_eq!(literal, "apprafter promote");
        assert_eq!(marker.reason, Some(Reason::Historical));
        assert_eq!(marker.since.as_deref(), Some("v0.2.44"));

        // Every rejection the marker grammar owns is rejected here too,
        // because it IS the marker grammar.
        for (yaml, why) in [
            (
                "span: x\nreason: made-up\nsince: v0.2.44\n",
                "unknown reason",
            ),
            (
                "span: x\nreason: historical\nsince: yesterday\n",
                "bad since",
            ),
            ("span: x\nreason: historical\n", "no since"),
            ("span: x\nsince: v0.2.44\n", "no reason"),
            ("reason: historical\nsince: v0.2.44\n", "no span"),
            (
                "span: x\nreason: historical\nsince: v0.2.44\nwhy: z\n",
                "unknown key",
            ),
        ] {
            let value = serde_yaml::from_str::<serde_yaml::Value>(yaml).unwrap();
            assert!(read_entry(&value, "span").is_err(), "{why}: {yaml}");
        }
    }

    #[test]
    fn the_census_counts_exemptions_by_kind() {
        let stats = Stats {
            exemptions: vec![Reason::Historical, Reason::ExternalTool, Reason::Historical],
            ..Stats::default()
        };
        let line = stats.line(32, 2);
        assert!(line.contains("3 exemptions"), "{line}");
        assert!(line.contains("1 external-tool"), "{line}");
        assert!(line.contains("2 historical"), "{line}");
        assert!(Stats::default().line(1, 0).contains("0 exemptions"));
    }
}
