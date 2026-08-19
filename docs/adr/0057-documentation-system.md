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
such decision is marked at the point it is stated. **Those marks record the
world on the day this was ratified and are deliberately not rewritten as
subphases land** — an ADR describes a decision as it was made. This paragraph
is the pointer to what has happened since: **2.19c, 2.19d and 2.19e have
shipped**, so the "not yet implemented" marks on decisions 1, 6 and 7 are
history rather than status. The amendments below record what each delivered
and — the part worth reading — the decisions this document made that
measurement then overturned.

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

The "xref tags and health metrics" bullet under *Deferred* below names three
things. Here is what became of each, because recording what was **rejected**
is the point of this amendment — an ADR that lists only what was built
invites the next person to rebuild what was rejected.

| The bullet said | What happened |
|---|---|
| explicit code-to-docs tags | **Dropped**, and the goal met from the other direction. See below. |
| graded test bindings | **Deferred on measurement** — 17 of the 28 tracked `e2e/*.sh` walks are run by nothing automated. |
| a committed metrics file and its ratchets | **Shipped**, with the ratchets re-chosen: all three originally proposed were unimplementable or inverted. |

**Explicit code-to-docs tags were dropped, and the same goal is met by
`code-path` and `adr-reference` running the other way.** The original idea
was a `// docs: <path>#<anchor>` comment in Rust and CUE, so that changing a
function would surface the pages describing it. Measurement made the
obligation unpayable: roughly 979 public items across 185 non-test modules,
and even a module-level rule means 73 new tags on the doc-facing surface
alone — 73 assertions written at authoring time to satisfy a checker, which
is the defect class this whole system exists to remove. Sniffing the tags
implicitly is worse: `cli/cli-providers/src/k8s/sealing.rs:7` already cites a
`docs/developer/crypto.md` belonging to bitnami's sealed-secrets, scoped only
by prose on the preceding line, and it cannot be fixed by renaming because
the file is not ours.

So the direction was inverted. A reference from documentation *into* the
repository is written by the person who is already looking at both, is
checkable without anyone maintaining a second index, and fails loudly when
the target moves — which is the property the tags were wanted for. What is
given up is the reverse lookup: nothing tells a contributor editing
`server_type.rs` which pages describe it. That remains open, and if it is
ever built it should be built as a query over the references that already
exist rather than as a second set of hand-written tags.

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
  positives on `docs/dev-guide/quickstart.md` alone. A `pre` element that never
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
- **A committed corpus census** at `docs/measurements/docs-health.json`,
  compared **by value**: every obligation count may not decrease (growth
  passes silently), and the exemption count is equality in both
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
  under `--strict` already fails the build on a dead anchor, and **every**
  in-scope anchored link resolves. A second, less informed opinion could only
  disagree with the first. (This read "all 28" until 2.19e added a page that
  wrote two more. The property held; the tally did not, which is why it is a
  property here now. Re-derive the occurrences with
  `git ls-files -- 'docs/*.md' README.md | grep -vE '^docs/(adr|changelog|measurements|reference/cli)/' | xargs grep -ohE '\]\([^) ]*#[^) ]+\)'`.)
- **Three earlier-proposed ratchets: replaced.** `blocks_executed` is
  unimplementable — the marker grammar's `run=` key accepts only `local`, and
  the gate reports that as a finding because nothing executes a documented
  block, so the ratchet reads `0 >= 0` forever. `check_none` held at 0
  prohibits rather than ratchets: at that value it bans the typed, dated,
  expiring exemption channel this ADR decided to build. And a **byte-compared**
  census would go red the day after it was committed with no commit in
  between, because three of the fields originally proposed for it are
  functions of `now`. The kept fields are all obligation counts, and none of
  them is `now`-derived.

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
carrying no counted claim is invisible to every one of them. These ratchets
defend against the careless; review defends against the motivated, and no
arithmetic over a corpus can take that job.

## Amendment — 2.19e (the two build hooks and the LLM layer) as delivered, 2026-08-19

Decision 1 accepted two costs of staying on MkDocs-Material and marked both
unimplemented: pygments ships no CUE lexer, and nixpkgs has no `llms.txt`
plugin. Both are now paid, by two build hooks in `docs/hooks/` registered
under a `hooks:` key in `mkdocs.yml`. **Neither hook commits an artefact.**
Everything they produce derives from committed content at build time, so
none of it can drift from the pages it describes — which is why this package
could land at any point in the track, exactly as the Deferred list said.

No release and no monorepo tag rides it. `cli/Cargo.toml` is untouched, no
chart, operator or cue-cmp artefact moved, and the only Rust edit is a
docstring in `cli/docsgen/src/scan.rs` that a count taken in this subphase
falsified.

### Delivered

- **A CUE lexer, whose self-check is the actual deliverable.** The corpus
  writes **37** `cue` fences across 20 tracked pages, **31** of them on the
  built site — the changelog working file holds the other six and is excluded
  from the site. Without the hook all 31 render as unstyled text.

  R1 named the mitigation as "a mandatory unit test asserting
  `get_lexer_by_name("cue")` resolves". What shipped is stricter and runs
  more often. It is an `on_config` handler, so it runs on **every** build —
  local preview, `just lint` and CI alike — rather than in a suite that has
  to be invoked; and it is three checks rather than one. The alias must
  resolve, with `ClassNotFound` caught and reported in a sentence that names
  the consequence. It must resolve to **this** hook's lexer, so a different
  lexer claiming the name is caught rather than trusted. And a sample must
  come back carrying six token families — comment, keyword, string, name,
  number, operator — tested through pygments' token hierarchy, so
  retokenising a construct to a sibling subtype does not misfire while losing
  the construct entirely does. Both failure modes were watched firing before
  the hook was committed: a self-check nobody has seen fire is a self-check
  nobody knows works.

  **One measurement in the subphase plan did not reproduce, and is corrected
  here rather than adjusted away:** the plan recorded 34 CUE fences. It is 37
  tracked and 31 on-site. A count that requires the fence to open the line
  returns 33, silently missing four — three written inside blockquotes, one
  indented under a list item. The build settles it independently: the
  site-wide number of token-carrying code blocks rose by exactly 31 when the
  hook was registered.

- **A `description` on every page the index lists.** 31 hand-written pages
  gained one. Of the **34** pages in the gate's corpus, 33 now carry front
  matter with a description; the hold-out is the root `README.md`, which is
  not a site page and so has no index entry and no meta tag to fill. Before
  this subphase exactly **one** hand-written page had a description — the
  generated command pages already carried `title`, `description`, `audience`
  and `status`, because the generator writes them. On the built site, pages
  emitting their **own** HTML meta description went 28 to **60**; the
  remaining 60 are the 59 ADRs and the 404 page, and they fall back to
  `site_description`, which is the same sentence everywhere and therefore
  worth nothing in a search result.

  Its cost is also its limit, and it was paid immediately: three of the first
  drafts were rewritten before landing because each asserted something its
  page did not support — a future-tense enforcement section described as
  present, a command counted into a group it does not belong to, a
  platform-scope verb described as available per-application. A description
  is prose, so nothing checks it but review.

- **`llms.txt`, `llms-full.txt` and a markdown twin beside every page.**
  Measured on the built site: **119** published pages, 59 of them under
  `adr/`; **60** indexed, **119** bundled, **119** twinned. The ADR split
  holds — bundled and twinned but not indexed, because each ADR describes
  the world as it was on the day it was ratified (which is why the drift gate
  holds them out of scope too) and they are half of everything the site
  publishes, so indexing them would bury the guides.

  Two shapes the design did not anticipate. The index needed an **`Other
  pages`** section: two published **non-ADR** pages sit on the `not_in_nav`
  allow-list (`license.md` and `contributing/README.md`; the list's third
  entry, `adr/*`, covers every ADR and those are excluded from the index
  anyway), so a purely nav-driven index would have dropped them silently —
  with the section, the index is *complete* over published non-ADR pages,
  which is what let the guard below be written as completeness rather than
  as a restated list of group names. And nested nav sections are carried as a
  **label prefix** rather than flattened, because flattened outright the
  *Reference* group listed two different pages as "Overview".

  The hook **fails the build** when an indexed page carries no
  `description`. The plan stated that as a contract and nothing enforced it;
  this is its natural home, and it passes today over every indexed page.

- **A new pass in `scripts/docs-check.sh` — its second, right after the
  strict build — covering BOTH hooks.** The
  mkdocs build does not know these three artefacts were supposed to exist,
  so a hook that stopped writing them leaves a green build and a published
  site with a broken machine-readable layer — and `pymdownx.highlight`
  swallows a failed lexer lookup, so an unregistered CUE lexer leaves a
  green build too. The pass asserts, over the site just built and the
  committed source it was built from: the index exists and names the
  licence; its links carry an `http`/`https` scheme **and** sit under
  `site_url` (two properties, because the second alone is vacuous —
  `site_url` comes from the same config the hook read); every link
  resolves to a page the site contains; every published non-ADR page is
  listed, and every entry carries **the description that page's front
  matter authored**; the bundle holds one entry per published page, and
  each entry's body and description **are** that page's committed
  markdown and committed description; each twin likewise; every page
  declared `not_in_nav` was in fact published; and every `cue` fence in
  a page's committed source matched a rendered block on that page which
  read it **as CUE** — a `//` line coming back as a comment token, a
  `#Definition` as a definition.

  It also reads the **published page itself**, which the list above does
  not: every artefact there derives from `page.markdown`, the text as it
  stands *before* the page renders, so all three can be perfect on a site
  that publishes nothing. Each published page's rendered `<article>` must
  therefore carry that page's committed H1 and the longest runs of its
  committed prose, must have **consumed** the lines the source wrote as
  block syntax (an admonition, a tab, a table row, a task item, a snippet
  include — one rule covering six extensions), and must carry the page's
  authored description, or `site_description` where it has none, as its
  HTML meta description.

  Every expectation is derived from **the other side**: the page set
  from the built site, the content from the committed source, never a
  thing against itself and never against its own length. That rule was
  arrived at by attack, over four adversarial passes, and every hole
  found was one violation of it:

  - the lexer's only detector was the hook's own `on_config`
    self-check, **inside the thing it guards** — deleting its line from
    `hooks:` removed detector and deliverable together, as did a later
    hook overwriting the registration and `use_pygments: false`;
  - the "links are absolute" test read its prefix from the config that
    wrote the links, so it could not fail;
  - the index entry was matched by a pattern that did not require the
    `: description` suffix, so a writer dropping every authored
    description left the pass reporting OK;
  - the bundle and twins were checked for **length**, so a writer that
    truncated and padded passed, and two swapped twins were
    indistinguishable;
  - the fence set was read from the twins, so a writer that stripped
    fenced blocks took 28 of 31 fences out of scope silently;
  - the fence check asserted only that *some* lexer took, so a hook
    installing `class CUE(YamlLexer)` passed with every block
    tokenised as YAML;
  - the fence scanner required a backtick run and a bare language word,
    so `~~~cue` and pymdownx's own ` ```{.cue} ` spelling rendered as
    highlighted CUE and were never checked — while
    `cli/docsgen/src/scan.rs` already accepted `~`, so the gate held two
    scanners disagreeing about what a fence is;
  - `exclude_docs` could unpublish any page outside `nav:` with no
    ERROR and no WARNING — mkdocs reports a link into an excluded page
    at INFO, which `--strict` does not promote. `scripts/docs-check.sh`
    now fails on that line, naming both pages, and the pass cross-checks
    `not_in_nav` against what was published for the case nothing links.
    That closed the *declared* case only: **any** page could still be
    dropped from `nav:` and added to `exclude_docs` together, which is a
    clean build with nothing logged at all. The reverse direction —
    every page the drift gate counts must be published — is now asserted
    against the gate's own scope rule, read out of
    `cli/docsgen/src/scan.rs`, so unpublishing a guide has to move a
    Rust constant as well as two lines of `mkdocs.yml`;
  - the bundle was compared by **cardinality**, so a dropped page hid
    behind a duplicated one — the exact reasoning the twins block
    already carried, forty lines further down the same file, and not
    carried across;
  - the index's **shape** was unasserted and the label it parsed was
    discarded, so an index reduced to the licence line plus one flat
    `- [Page](url): description` per entry lost its H1, its
    site-description blockquote, every `##` section and the generated
    twin-rule sentence with the pass reporting OK;
  - and the whole pass read the artefacts and never the rendered HTML,
    so a hook stripping every `<p>` from `on_page_content` published a
    site with no prose at all and every artefact assertion stayed true,
    because the artefacts derive from the markdown rather than from the
    page.

- **An eighth census counter, `cue_fences`.** The lexer check is only as
  strong as the number of fences it has to check, and it had a
  zero-check rather than a floor. The counter is recorded **and**
  enforced by `docsgen` — the tool that reads the corpus — rather than
  by the Python pass that reads the site: a counter one tool records and
  another enforces is a seam.

  The scopes differ, and the gap that leaves is recorded rather than
  closed. The census counts the **in-scope** corpus; the Python pass
  checks every **published** page, `docs/adr/` included. So the `cue`
  fences under `docs/adr/` are checked and floored by nothing —
  retagging one ` ```yaml ` takes it out of the checked set with the
  counter unmoved. Widening this one counter to the published corpus
  would make the census two measurements of two different trees, when
  every other field in it counts claims the gate *resolves* and
  `docs/adr/` is out of all of them; one field silently spanning a
  different corpus from its seven siblings is the worse defect. The
  artefact pass prints the split on every run instead.

### The two things it deliberately did not build

Recording these is the point of the amendment. Both were in the design, both
were dropped or deferred on a stated reason, and each is a shape this track
has already been burned by once.

- **`since` in front matter: dropped.** The design called for the
  front-matter gate to cover `description`, `audience` and `since`. `since`
  is exactly the shape that failed for `verified-by` one subphase earlier:
  **a per-page version claim that nothing can check.** No gate reads it, no
  build fails on it, and no reader can distinguish a page whose `since` is
  current from one whose `since` was correct two releases ago — it is written
  once at authoring time and rots in silence. Requiring it would manufacture
  one unverifiable claim per page, which is the defect class this whole
  system exists to remove.

  The contrast that decides it is inside this repository already: the
  `since=` on an **exemption** stays, because the gate resolves that tag to
  its commit date and voids the exemption after 180 days. The distinction is
  not the field name but whether anything in the tree can call the claim
  wrong. Where a page needs to name a version it names it in the prose, where
  a reader sees it and a reviewer questions it.

  `audience` survives the same test from the other side: it is derivable —
  front matter, else the nav group, else the nearest ancestor directory's
  group, else `general` — so it is optional rather than required. A field an
  author must retype to state what the tree already says is a field that will
  one day disagree with the tree.

- **`<llm-only>` and `<llm-exclude>`: deferred, with the reason recorded
  rather than the feature half-built.** Measured: **zero use sites** —
  nothing in the repository writes either tag. The markdown-twin transform is
  their natural home, and that pass now exists, so adding them later is a
  handful of lines against a real use site instead of a mechanism guessed at
  in advance. Building a stripping mechanism nobody has asked for is
  precisely how the documentation system audited in *Context* accumulated
  machinery that no CI job ran.

  The same reasoning is recorded in the hook itself for `pymdownx.snippets`
  include lines, which a twin currently carries rather than resolves: also
  zero use sites today, also a handful of lines on the day there is one to
  test against.

### Consequences

- **Writing a page costs one line of front matter**, and forgetting it fails
  the build with the page named. That is the intended cost, and it is the
  only new obligation this subphase puts on an author.
- **A hook change alters every page on the site**, so `docs/hooks/**` is
  listed explicitly in the docs workflow's path filters even though `docs/**`
  already covers it — the same reason the committed census is listed there.
- **The contributor-facing half is `docs/contributing/documentation.md`**,
  which is itself inside the gate's corpus. A page explaining the gate that
  the gate does not read is the first page to go stale.
- **The census grew rather than shrank**, which passes silently by design:
  the property that holds is that **no counter fell**. Measured mid-subphase
  — `docsgen gate` at the base (`a761860`) and at `57e48dd`, with the seven
  counters and the scanners as they stood *then* — four of seven moved:
  pages 33 → 34, invocations 384 → 385, code paths 80 → 87, ADR references
  67 → 68, every one of them the new contributor page's own claims
  resolving. Five commits followed `57e48dd`, and one of them moved the
  census again: `c5ae92b` widened the code-path scanner (87 → 90) and added
  an **eighth** counter, `cue_fences`. The base-to-tip pair as committed is
  therefore what re-derives, and it re-derives from the repository rather
  than from this sentence:

  ```sh
  git show a761860:docs/measurements/docs-health.json   # seven counters
  git show HEAD:docs/measurements/docs-health.json      # eight
  ```

  Read that pair as content growth *plus* a widened scanner, not as
  content growth alone — the two ends were recorded by different versions
  of the tool, which is exactly why the mid-subphase measurement above is
  labelled with the commit it was taken at. (Two earlier drafts of this
  line were wrong: the first said no other count moved, the second named
  `57e48dd` as the subphase's last commit. Both are corrected here rather
  than quietly bumped — a permanent record stating a measurement that does
  not re-derive is the defect this ADR exists to remove.)

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
  *(Delivered in 2.19e, together with the CUE lexer decision 1 accepted as
  the other cost of staying on MkDocs-Material — see the 2.19e amendment
  above for what shipped, and for the two front-matter features that were
  dropped and deferred on measurement.)*
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
- `docs/hooks/cue_lexer.py`, `docs/hooks/llm_export.py` — the two build
  hooks decision 1 accepted as the cost of staying on MkDocs-Material.
- `docs/contributing/documentation.md` — the author-facing front-matter
  contract; `docs/contributing/documentation-gate.md` — the gate's own page.
- `mkdocs.yml` — the validation settings, the `not_in_nav` allow-list and the
  site exclusions.
- `LICENSE-CC-BY-4.0`, `NOTICE` — the documentation licence texts.
