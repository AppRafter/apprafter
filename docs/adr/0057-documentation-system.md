# ADR 0057: documentation system — MkDocs-Material retained, generated CLI reference, content-detected drift gate

## Status

`Accepted` (2026-08-18).

ADR for the documentation-system track (`plan.md` §2.19, subphases a–j). It
records decisions **as made**, and it is written out of order: this repository's
convention is ADR-first, the track's own design called for ratification before
the generator landed, and two of the ten subphases shipped before this document
existed. Recording that inversion is part of the record.

Delivered at the time of writing: **2.19a** (one Nix python environment, a
validating strict build, the documentation licence, and the gate wired into
`just lint` / lefthook / CI) and **2.19b** (the `cli/docsgen` crate and the
generated CLI reference under `docs/reference/cli/`). Subphases c–j — the
prose and table gate, xref and health metrics, the mkdocs hooks and LLM
artefacts, the information-architecture restructure, publication, the CLI-UX
work and the guide content — are decided here but **not implemented**. Each
such decision is marked at the point it is stated.

No release rides this decision so far: shipped CLI behaviour is unchanged (a
lib target plus doc-comment corrections), no chart, operator or cue-cmp
artefact moved, and the site is still unpublished.

## Context

AppRafter's documentation is a product surface, not an internal aid: a
developer's first contact with the platform is `apprafter --help` and a guide,
and the CLI is the primary interface to everything shipped so far. Measured
before this track:

- **93 tracked pages** (58 ADR, 35 non-ADR). `mkdocs build --strict` passed —
  while validating almost nothing. 64 pages sat outside the navigation under
  `validation.nav.omitted_files: info`, dead links and dead anchors were not
  checked at all, and `edit_uri` pointed at a branch that does not exist. One
  of the 64 orphans was `operator-guide/node-prep.md`, a real user guide that
  was invisible on the site.
- **No CI job ran on a documentation change.** The lint workflow's paths
  filter had no docs entry, so a docs-only pull request ran zero jobs.
- **The documented Nix fallback could not work.**
  `nix shell nixpkgs#python3Packages.mkdocs-material` provides no `mkdocs`
  binary, and adding `python3Packages.mkdocs` beside it yields a second
  environment whose site-packages lacks the theme
  (`Unrecognised theme name: 'material'`). The repository's own contributor
  notes documented that path as working.
- **The site is unpublished.** Nothing serves the URL the configuration
  promises; the landing page renders a "Soon" badge instead of a docs link.
- **The CLI reference was hand-written and incomplete** — 15 of 27 top-level
  commands, on a surface of 27 top-level plus 68 nested = **95 command
  paths**, with zero usage examples, no shell completions and no snapshot
  tests.

Six code-to-documentation drifts were confirmed by inspection. Their
*location* is the single most important input to this design:

| # | Drift | Where it physically sits |
|---|---|---|
| 1 | `expose.public` documented as an `Application` field the schema does not define — and `#Application` is closed, so a manifest copied from the page fails `cue vet` | one fenced snippet **plus** a field-table row **plus** prose |
| 2 | "env is literal strings only — no secret references" | pure prose, contradicting ADR 0046 |
| 3 | auto-injected `DATABASE_URL` / `REDIS_URL`, removed by ADR 0046 | 8 of 9 occurrences outside any fence |
| 4 | "`up` provisions a CPX22", plus an invocation missing an argument ADR 0056 made required | prose plus one fence |
| 5 | a CLI reference claiming completeness while covering 15 of 27 commands | the whole page |
| 6 | an internal planning coordinate leaking into a public page | prose |

**Five of the six live in tables and prose, not in code fences.** A gate that
extracts fenced snippets and compiles them — the obvious design — closes
exactly one of them.

That measurement is corroborated by a prior documentation system we audited in
depth, which had built the obvious mechanisms and still rotted. It carried 316
code-to-documentation tags and a checker to validate them; the checker was
wired into no CI job, it never opened a markdown file (it stripped anchors and
asserted only that a filename existed), its snippets were hand-copied into a
test file rather than extracted from the pages, and coverage was counted per
page rather than per claim. Over eight months it accumulated 124 documented
defects — 45 % of them contradictions of the implementation living in tables
and prose — while its gate stayed green.

AppRafter starts with an asset that system lacked: 288 `ADR NNNN` references
in Rust and 108 in CUE, across 25 ADRs, all resolving to real files. The
binding between code and decision already exists; what is missing is a
machine-checkable binding between code and *prose*.

## Decision

We will treat documentation as a gated build artefact: one pinned toolchain,
a reference generated from the code that defines it, and a drift gate whose
obligations are detected from content rather than declared by the author.

### 1. Keep MkDocs-Material

The site generator is not the problem, so it does not change. 93 pages stay
where they are, in repository-root `docs/`, and the strict build keeps
building them. Porting to another generator would spend the whole track's
budget on markup migration and produce none of the machinery this ADR exists
to add.

Two costs are accepted as a consequence:

- **Pygments ships no CUE lexer** (verified against pygments 2.20.0:
  `get_lexer_by_name("cue")` raises `ClassNotFound`), so a build hook must
  register one. The registration has a sharp edge — `pymdownx.highlight`
  swallows the exception from a malformed registration and renders the block
  unhighlighted, producing a green build with zero highlighted spans — so the
  hook ships with a unit test asserting the lexer resolves. That test is the
  only detector. (Not yet implemented.)
- **There is no packaged `llms.txt` plugin for MkDocs in nixpkgs**, so a
  second hook must write `llms.txt`, `llms-full.txt` and the per-page markdown
  twins itself. (Not yet implemented.)

`pymdownx.highlight` and `pymdownx.snippets` are enabled now, the latter with
`base_path` at the repository root and `check_paths: true`, so guides include
**real** files rather than carrying copies that drift.

### 2. One `python3.withPackages` environment in the flake

`flake.nix` provides a single combined environment — mkdocs-material,
mkdocs-literate-nav, mkdocs-redirects — and every docs target routes through
`nix develop`.

Two reasons, and the second is the binding one. Separate `nix shell` packages
never share site-packages, so the previously documented fallback could not
have worked in any form; and a byte-compared artefact requires **one** pinned
toolchain across a contributor's machine and CI, or the comparison decides
nothing. Consequently the documentation gate has no fallback to a
system-installed binary, unlike the CUE scripts, which do.

### 3. The clap tree is the source of truth for the CLI reference

`cli/docsgen` — a new crate in the CLI workspace — projects the clap
definitions into `docs/reference/cli/`: one page per top-level command (26
pages; the hidden `auth` tree is excluded from pages but present in the JSON),
an index, a `SUMMARY.md` navigation fragment consumed by
`mkdocs-literate-nav`, and a machine-readable `commands.json` covering all 95
command paths. `docsgen check` renders in memory and **byte-compares** against
the committed tree, reporting the file and the first differing line.

This deliberately mirrors `operator/crdgen` (ADR 0047): the same
generate/check shape, the same one-pinned-toolchain rule, the same
first-differing-line diagnostic, the same "generated files are committed so
they stay diff-reviewable" stance. A contributor who has met one meets the
other.

`docsgen` is **its own crate**, not a second binary inside the shipped
`apprafter` package: the release workflow builds `-p apprafter`, so a bin
target there would ship the generator inside every release artefact.

`generate` is write-only by design — a generator that deletes files it does
not recognise is one typo in a path constant away from removing a
documentation tree — so `check` carries a second assertion for strays: a file
under `docs/reference/cli/` that the generator does not produce fails, with a
`git rm` remedy rather than a regeneration reminder.

The hand-written `docs/reference/cli.md` is deleted, its inbound links
retargeted. What the clap tree cannot produce moved to a new hand-written
`docs/reference/environment.md`: only four environment variables are declared
through clap and survive in the generated flag tables; the other **25** that
page documents are invisible to any generator — read with `std::env::var` in
command code or by a dependency, or, in one case, documented precisely because
nothing reads it despite a source comment claiming otherwise.

### 4. The CLI crate gains a lib target with a narrow `docs_api` facade

A binary crate cannot be linked, so the clap tree was unreachable.
`cli/platform-cli` now has a lib target: `dispatch` moved out of the crate
root into its own module, `main.rs` is a thin shim over `apprafter::run()`,
and the public surface is `run()` plus a two-item `docs_api` facade — the clap
root, and the injected-schema manifest validator that `apprafter app validate`
itself calls.

The alternative of publishing `commands` wholesale was rejected: exporting
roughly 45 modules would have made the facade decorative, and the point of the
facade is that widening the crate's public API is a deliberate act rather than
a side effect of a refactor. Anything added there is a contract with the
documentation gate.

### 5. Documentation prose is CC-BY-4.0; embedded code samples are Apache-2.0

Prose under `docs/` — including the generated reference pages and, once they
ship, the LLM exports — is licensed **CC-BY-4.0**. The code samples inside
those pages are **Apache-2.0**, so they can be pasted into a project without
attribution obligations. Stated in `LICENSE-CC-BY-4.0`, `docs/license.md`,
`README.md`, `NOTICE` and the site footer.

This is named **before** any LLM bundle ships, not after. Publishing a
machine-ingestible corpus is an explicit invitation to reuse, and an
invitation with no stated terms is a liability for both sides. Markdown under
`docs/` carries no per-file SPDX header (five stray `FSL-1.1-Apache-2.0`
headers that contradicted the new licence were removed, and the contributor
guidance now says so).

### 6. The gate is content-detected, not language-keyed, and its escape hatch is typed

*(Decided; not yet implemented — this is the next subphase.)*

An obligation that the author declares is an obligation the author can delete.
Measured across the in-scope pages: 225 fences carry no language tag, and 10
of those already contain `apprafter` invocations — several in a flagship
guide. Keying the gate on the language tag would make "delete the tag" a free,
untraceable way to turn the gate green.

So obligation is derived from content:

- a fence whose body contains an `apprafter` invocation requires a CLI check,
  whatever its language tag;
- a fence whose body is CUE-shaped requires a CUE check;
- an unlabelled fence inside the gated set is an **error**.

The escape hatch is typed rather than free-text: an exemption must name one of
a fixed set of reasons (third-party output, illustrative fragment, external
tool, known broken), carry the version it was granted at, and expires — an
exemption older than 180 days fails the build, so it is re-justified rather
than inherited. The count of exemptions is a ratcheted metric.

The three counters to the audited system's three root causes are structural,
not aspirational:

1. **Validate anchors rather than stripping them.** Referenced headings carry
   explicit `{#id}` attributes and the build validates link anchors, so a
   renamed heading breaks the build instead of silently degrading a link.
2. **Extract snippets rather than copying them.** Guides include real files
   through `pymdownx.snippets`, and executable recipes are run, not merely
   parsed. A copied snippet cannot be kept honest by any checker; an included
   one cannot drift at all.
3. **Wire the gate in three places.** `just lint`, a lefthook pre-commit hook,
   and a CI job (`.github/workflows/docs.yml`) that runs on every pull request
   touching `docs/`, the CLI sources, the generator or the pinned toolchain.
   Whether that job is *required* to merge is branch-protection configuration
   and cannot be asserted from the repository, so this decision claims only
   that the job runs. The audited system's checker existed and ran nowhere;
   existence is not enforcement.

### 7. Drift is prose-shaped, so the gate is prose-shaped

*(Decided; not yet implemented.)*

Because five of six measured drifts live outside fences, the gate's scope is
not fences. It also covers:

- **Field tables** — a marker before a markdown table binds its identifier
  column to a CUE definition's field set, and its default column to the
  schema's defaults. This is what turns a table row documenting a field that
  does not exist into a build failure.
- **Inline invocations** — backticked `apprafter …` spans outside fences are
  resolved by the same resolver. Measured: 209 of them across 23 pages, a
  larger surface than the fences.
- **Frontmatter claims** — a page may assert that a schema path is present or
  absent, or that a command does not exist. A page claiming "env is literal
  strings only" declares the corresponding absence and fails the day the
  opposite ships.
- **A forbidden-claims lint** — a curated list of strings encoding removed
  behaviour or leaked internals, each with a reason and a since-version. This
  is the only guard against *recurrence* of prose drift.
- **The public root documents** `spec.md` and `README.md`, because the marker
  grammar is an HTML comment and works in any markdown. `CLAUDE.md` cannot be
  gated in CI — it is untracked by design — so it gets a local advisory only,
  and that limitation is stated rather than papered over.

Every row of the drift table in Context becomes a fixture that the gate must
reproduce as a failure. Without those fixtures this section is a claim; with
them it is a red test.

### 8. Publication target is `docs.apprafter.dev`

*(Decided; not yet implemented.)*

The site ships as its own image with its own `Application.cue`, mirroring the
landing and CMS deployments, and is served on its own hostname. A path on the
landing origin was considered and rejected: it would couple the docs release
cadence to the landing's, share one cache and one rollback unit for two
artefacts with unrelated change rates, and complicate the origin's routing for
no gain. Docs are a separate deployable with a separate lifecycle, so they get
a separate deployment.

The information-architecture restructure lands **before** publication, so
URLs do not churn under a live link from the landing page.

## Consequences

Already real, on the delivered subphases:

- **A renamed flag fails `just lint`.** Any change to the clap tree — a new
  command, a renamed or removed flag, a changed default, an edited doc comment
  — fails the byte-compare until `just docsgen-generate` is run and the result
  committed. This is the intended cost.
- **A release bump is a documentation change.** `commands.json` embeds
  `cli_version`, so bumping the CLI version changes a byte-compared artefact
  and requires a regeneration in the same commit. Deliberate: a consumer of
  the machine-readable surface needs to know which CLI it describes. The
  failure mode if forgotten is a drifted artefact on master that fails the
  next contributor's unrelated documentation change, so the release path,
  the pre-commit glob and the CI paths filter all include `cli/Cargo.toml`.
- **`just lint` now requires nix unconditionally.** The documentation gate has
  no system-binary fallback, by decision 2. Without nix it exits with an
  explanatory error; the local hook can be skipped, but the CI job still
  runs on the pull request.
- **Doc comments are published verbatim, which made four inaccurate `///`
  comments into documentation bugs** that had to be fixed before the reference
  could ship:
  - three commands (`status`, `login`, `upgrade-tier`) whose comments read as
    if they worked — the first reads local state and never contacts the
    cluster, the other two print what they would do and change nothing. The
    caveat now leads the first sentence, because the generator uses the lead
    sentence as both the overview-table summary and the page description;
  - seven flags carrying no help text at all, six of them on `backup enable`,
    where getting retention or a cron expression wrong is not recoverable;
  - `--no-ping`, whose help claimed any non-empty value flips the flag —
    false in both halves under the boolean value parser, where `=0` does not
    flip it and an empty value is a hard parse error;
  - `nodes[0].kind`, an internal Rust field name published as if it were the
    manifest key. The key is `type`; the leaked name appeared in three help
    strings and in an operator runbook, where it told operators to write a key
    that does not parse.

  This is the generator working as intended. A hand-written page can describe
  a command charitably; a generated one publishes exactly what the code says,
  so an inaccurate comment becomes visible instead of staying private.
- **Internal pages no longer publish.** The release-note working file, the
  plan ledger and the measurement notes are excluded from the site. The
  release-note file stays exactly where it is — the release workflow parses it
  — it is excluded from the *site*, never moved.
- **Some pages are deliberately reachable but unlisted.** ADRs, the licence
  page and the contributing index sit on an allow-list; anything else missing
  from navigation fails the build.

Expected, on the deferred subphases: writing a guide becomes more expensive
(frontmatter, markers on every fence and inline invocation, a typed test
binding), and that cost is the point — it is paid once per page instead of
being discovered as a defect later. The gate will also reject documentation
for behaviour that does not exist yet, which is why the CLI-UX work is
sequenced before the content work.

Neutral: generated files are committed, so they stay diff-reviewable; the
generator lives in the CLI workspace but ships in no release artefact.

## Amendment — 2.19d (xref and health) as delivered, 2026-08-18

The "xref tags and health metrics" bullet under *Deferred* below named five
mechanisms. Three shipped in a different shape than the bullet described, one
was dropped on measurement and one was deferred on measurement. Recording
what was **rejected**, and why, is the point of this amendment: an ADR that
lists only what was built invites the next person to rebuild what was
rejected.

No release and no monorepo tag rides it. `cli/Cargo.toml` is untouched, no
chart, operator or cue-cmp artefact moved, and `docsgen` is a build-time crate
that ships in no release artefact.

### Delivered

- **Obligations survive unfencing a block.** A four-space indented run at the
  top level, and an HTML `pre` element, now carry everything a fence carries.
  This closes an evasion the gate shipped with: a contributor told to label a
  fence could delete the delimiters and indent the body, which renders
  identically as code and was invisible to every check. Container tracking is
  load-bearing rather than incidental — tabbed blocks, admonitions and list
  items all indent four spaces, and without it the check reports six false
  positives on `dev-guide/quickstart.md` alone. A `pre` element that never
  closes is its own finding class, because the remedy is a different edit from
  closing a fence.
- **`code-path`** — a repository path named in a code span, or in a relative
  link target, that does not exist. The two surfaces resolve against different
  roots (a span from the repository root, a link target from the page's own
  directory), and a code span that is a link's *text* is checked as the
  link's target. Globs are suppressed at resolution rather than at
  extraction, and the pattern set is deliberately `*`, `?` and the braces —
  **not** the square brackets, because three tracked files carry brackets in
  their names, and counting those as patterns would suppress those references
  hardest at exactly the moment one of them moves.
- **`adr-reference`** — a citation naming no ADR, or naming a decision that is
  `Superseded`, `Deprecated` or an `Unused` reserved slot. It reads both
  spellings, the plural, a citation wrapped across a line break, and a number
  embedded in a link target or a path. The verdict comes from the **leading
  token** of the `## Status` body, so partial supersession — which four ADRs
  in this repository carry — is correctly not a finding; a substring rule over
  the same corpus produced only false positives.
- **`adr-check-ignore`** — a front-matter exemption channel for that class, on
  the same machinery as its two siblings: the same typed reasons, the same
  `since=` aged against real tag dates, the same void-when-unaged rule, the
  same unused-audit. It is keyed by the citation's canonical form, so one key
  covers every spelling of one decision. Citing a reversed decision on purpose
  is legitimate and is this repository's idiom, and the class shipped without
  any way to say so.
- **A committed corpus census** at `docs/measurements/docs-health.json`, seven
  counts, compared **by value**: six obligation counts may not decrease
  (growth passes silently), and the exemption count is equality in both
  directions. `docsgen metrics` re-records it. A missing or unparseable census
  is the gate's BROKEN exit code, never a documentation finding.

### Dropped or deferred, with the measurement behind each

- **Staleness gating: dropped.** Not viable at any threshold, and the numbers
  say so rather than a preference. The oldest page in the corpus is 104 days
  old, so a 180-day rule could not fire for months; a 90-day rule fires on the
  same four abstract pages forever; 24 of the 33 pages share two last-touched
  dates and therefore cross any threshold together; both clocks reset on
  non-events such as a licence-header sweep or a formatter pass;
  `git log -1 -- <path>` does not follow renames and one corpus page has
  already been renamed; and of the last 100 commits touching `schemas/**` or
  the CLI's command tree, **three** touched an in-scope page. If staleness
  returns, its shape is a diff against a published tag under an explicit
  pathspec — what `scripts/check-operator-version-bump.sh` already does — and
  not elapsed days.
- **Typed `verified-by` bindings: deferred.** Most of the e2e walks are run by
  nothing automated — 17 of the 28 tracked `e2e/*.sh` are named by no workflow
  and no `Justfile` target — and that set includes `backup-restore-walk.sh`,
  which a corpus page already cites as its verification. Requiring the field
  across the roughly twenty developer and operator pages would therefore
  manufacture about fifteen claims that read as verified and are not, which is
  worse than the absence it would replace.
- **A dedicated anchor checker: unnecessary.** `validation.links.anchors: warn`
  under `--strict` already fails the build on a dead anchor, and all 28
  in-scope anchored links resolve. A second, less informed opinion could only
  disagree with the first.
- **Three earlier-proposed ratchets: replaced.** `blocks_executed` is
  unimplementable — the marker grammar's `run=` key accepts only `local`, and
  the gate reports that as a finding because nothing executes a documented
  block, so the ratchet reads `0 >= 0` forever. `check_none` held at 0
  prohibits rather than ratchets: at that value it bans the typed, dated,
  expiring exemption channel this ADR decided to build. And a **byte-compared**
  census would go red the day after it was committed with no commit in
  between, because three of the fields originally proposed for it are
  functions of `now`. The seven kept fields are all obligation counts, and
  none of them is `now`-derived.

### The three checks find nothing today, and that is the point

All three new checks report **zero** findings on the corpus as it stands. That
is measured, not hoped, and it is the reason they were built in this order:
2.19i adds roughly fifteen guides that will cite ADRs and repository paths
heavily, and a guard installed *after* that content merely ratifies whatever
the content already got wrong. A regression guard installed on a surface that
is correct today is the only kind that can be installed honestly.

The census's own limits are stated rather than papered over, because they
bound what any of this is worth: it counts cardinality and never specificity,
so an obligation *substituted* rather than deleted costs nothing, and prose
carrying no counted claim is invisible to all seven numbers. These ratchets
defend against the careless; review defends against the motivated, and no
arithmetic over a corpus can take that job.

## Alternatives considered

- **Port the site to VitePress or Astro Starlight.** Rejected. The 93 existing
  pages, the ADR corpus and the strict build all work; the deficiency was
  machinery — validation, generation, gating, publication — none of which a
  different generator supplies. Porting would consume the track and deliver a
  differently-styled version of the same ungated site. The two costs of
  staying (a CUE lexer hook, an `llms.txt` hook) are two files.
- **Keep hand-maintaining the CLI reference.** Rejected by evidence: the
  hand-written page claimed completeness while documenting 15 of 27 top-level
  commands, and no mechanism existed to notice. A 95-path surface behind a
  human copy step has one steady state, and it is stale.
- **Expose `pub mod commands` instead of a `docs_api` facade.** Rejected.
  Publishing roughly 45 command modules to give the generator two items makes
  the facade decorative and turns every internal helper into a public API the
  next refactor must preserve. A two-item facade makes each widening a
  deliberate, greppable act.
- **Narrow the snippet `base_path` below the repository root.** Rejected. A
  narrow base would defeat the purpose — guides include real sources
  (`examples/`, `e2e/`, `schemas/`), not a curated snippets directory. The
  risk it addresses, a fence hiding inside an included file, is closed
  precisely in the forthcoming gate instead: includes are expanded before
  checking, and the includable set is an extension allow-list that excludes
  markdown, so no fence can enter a page unchecked. Traversal above the root
  is separately blocked by `restrict_base_path`.
- **Publish the docs under a path on the landing origin.** Rejected — see
  decision 8.

## Risks

- **R1 — the CUE lexer registration fails silently.** `pymdownx.highlight`
  catches the exception and renders the block unhighlighted, so a broken hook
  produces a green build with no highlighting. *Mitigation:* a mandatory unit
  test asserting `get_lexer_by_name("cue")` resolves. It is the only detector,
  so it is not optional.
- **R2 — the typed exemption becomes the default.** A gate with an escape
  hatch tends toward the hatch. *Mitigation:* typed reasons from a fixed set,
  a mandatory since-version, a 180-day expiry, and a ratcheted count in a
  committed metrics file, so growth is a reviewable diff rather than an
  invisible slide.
- **R3 — the gate is expensive in a pre-commit hook.** *Mitigation:* `check`
  is a lookup against committed JSON; only `generate` pays a build. The
  pre-commit hook is scoped to the paths that can invalidate the artefacts.
- **R4 — the drift gate is a second implementation of snippet-include
  semantics** and could diverge from the markdown extension's. *Mitigation:*
  the supported subset is pinned and covered by fixtures; unsupported forms
  are errors, not silent passes.
- **R5 — information-architecture churn under a live link.** *Mitigation:*
  the restructure and its committed redirect map land before publication, not
  after.
- **R6 — the ADR was written after implementation began**, so it risks
  rationalising what was built rather than deciding it. *We accept this* for
  the two delivered subphases, and mitigate it by marking every unimplemented
  decision as such, so a future reader can tell which parts this document
  ratified and which it recorded.

## Deferred (recorded explicitly)

- **The prose and table gate** — the largest design risk in the track and the
  part that closes five of the six measured drifts. Deferred only in
  sequence: it is the next subphase.
- **Xref tags and health metrics** — explicit code-to-docs tags, graded test
  bindings, a committed metrics file and its ratchets. They measure rot; the
  gate prevents it. Prevention first. *(Delivered in part by 2.19d — see the
  amendment above for what shipped, what was dropped on measurement and what
  was deferred on measurement.)*
- **The LLM artefacts** — `llms.txt`, `llms-full.txt` and per-page markdown
  twins. They derive from committed content, so they cannot drift and can
  land at any point; the licence they need was named in this decision.
- **The information-architecture restructure** — nav rebuild, splitting the
  contributor walks from the user guides, and a redirect map. Sequenced
  immediately before publication so URLs settle once.
- **Publication** — image, Caddy configuration, release workflow with a
  negated path filter, `Application.cue`, DNS zone, and the landing switch.
  Blocked on the restructure by design.
- **CLI-UX examples and completions** — per-command usage examples, shell
  completions and the surrounding usability fixes. Sequenced **before** the
  guide content, because the CLI check resolves examples against the *current*
  command tree: a guide documenting a command that does not exist yet is a
  hard error, not a to-do.
- **The guide content** — roughly fifteen missing guides for capabilities that
  exist only in `--help` today. Last, because every earlier package is a
  precondition for writing one that stays true.

## Owner

Andrey Ryahovskiy.

## Re-evaluation

- **When the prose and table gate lands:** confirm it reproduces all six
  measured drifts as failures. If it cannot, the content-detection premise is
  wrong and the gate's shape is revisited before any content is written
  against it.
- **If the two MkDocs hooks (CUE lexer, LLM artefacts) prove more expensive to
  maintain than they look:** reopen decision 1 — those two costs are the whole
  price of staying on MkDocs-Material, so if they inflate, the comparison
  changes.
- **If a packaged `llms.txt` plugin appears in nixpkgs:** replace the hook.
- **At the first documentation release after publication:** confirm the
  release path filter does not fire on internal changelog and measurement
  edits, which touch `docs/` on nearly every commit.
- **If the exemption count ratchets upward for two consecutive releases:**
  the typed escape hatch is not doing its job; tighten the expiry or the
  reason set.

## References

- ADR 0047 — CRD codegen from CUE. The generate/check/byte-compare pattern
  `docsgen` mirrors, including the pinned-toolchain requirement.
- ADR 0046 — `Application.env` value references. The decision that two of the
  measured drifts contradict.
- ADR 0056 — machine-picker. The removal of the implicit server-type default
  that a documented invocation no longer satisfies.
- ADR 0032 — base-license migration; `docs/license.md` — where the
  documentation licence sits in the wider licensing model.
- `plan.md` §2.19 — the a–j subphase decomposition and acceptance criteria.
- `cli/docsgen/` — the generator and the byte-compare gate.
- `cli/platform-cli/src/lib.rs` — the `docs_api` facade.
- `scripts/docs-check.sh`, `Justfile` (`docs-check`, `docs-build`,
  `docsgen-generate`), `.github/workflows/docs.yml` — the three wiring points.
- `mkdocs.yml` — the validation settings, the `not_in_nav` allow-list and the
  site exclusions.
- `LICENSE-CC-BY-4.0`, `NOTICE` — the documentation licence texts.
