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
is the pointer to what has happened since: **2.19c through 2.19h have
shipped**, so the "not yet implemented" marks on decisions 1, 6 and 7 are
history rather than status, and decision 8's closing promise — that the
information-architecture restructure lands before publication — has been kept.
Decision 8 itself is implemented **in software**: publication has a release
path, an image and a manifest, and the site is still not serving, because what
remains is a handful of operator steps outside this repository. Only 2.19i
(the guide content) and 2.19j (the closing walk) are still unbuilt. The
amendments below record what each delivered and — the part worth reading — the
decisions this document made that measurement then overturned.

**This paragraph and the one below are the two places in this document that
are kept current;** everything under `## Decision` and every amendment is
written once and left alone.

Two CLI patch releases ride this decision — `v0.2.45` from 2.19g (a `cli-core`
parse fix the publication manifest forced) and `v0.2.46` from 2.19h (the
`completion` command and an `Examples:` section in `--help`). No chart,
operator or cue-cmp artefact has moved, and the site is still unpublished. The
first four subphases carried no release at all, which is why this paragraph
once read "no release rides this decision so far"; that sentence was true when
written and is replaced rather than amended.

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

*(Implemented in 2.19g — see the amendment below for the image, the release
workflow's path filter, and the five manual steps that stand between the
merged branch and a site a reader can reach.)*

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
  the property that holds is that **no counter fell**. The growth is the
  subphase's own pages and their claims resolving, plus one widened
  code-path scanner and an **eighth** counter, `cue_fences`, added late in
  the subphase.

  This paragraph carried a per-commit table of deltas through four
  revisions and was wrong in three of them — the last time attributing a
  step to the commit after the one that made it. The deltas are not
  recorded here any more, and that is the point rather than an omission:
  the numbers are committed in the repository, one file per side, so the
  pair re-derives from the tree rather than from this sentence, and a
  sentence that cannot go stale is worth more than one that is accurate
  the day it is written.

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

## Amendment — 2.19f (information architecture) as delivered, 2026-08-19

Decision 8 closed with a sentence that has now been paid: *the
information-architecture restructure lands **before** publication, so URLs do
not churn under a live link from the landing page.* This amendment records
what moved, and — the part worth reading — the one thing this document named
that measurement then said not to build.

No release and no monorepo tag rides it. `cli/Cargo.toml` is untouched at
0.2.44, and no chart, operator or cue-cmp artefact moved.

Rust did change, and the first version of this paragraph said it had not —
falsified by the subphase's own commits, which is worth recording rather
than quietly correcting. A doc comment in `cli/platform-cli/src/cli.rs`
carried an internal coordinate ("later sub-phases"), and a user reads that
in `apprafter --help`, so it was rewritten; the byte-compared
`commands.json` moved with it, regenerated in the same commit as the gate
requires. The remaining Rust edits are comments inside `cli/docsgen` that
cited pages this subphase renamed — a comment naming a file that no longer
exists is the defect class this whole system gates for, so they moved with
the files.

A help-string change still carries no bump: `cli/Cargo.toml` is bumped to
match a *released* tag in the commit that cuts it, and docs-only work cuts
none.

### The redirect map was not built, and its trigger is recorded instead

R5's mitigation names "a committed redirect map", and the Deferred bullet
below names one too. There is nothing to redirect **from**.

No URL this site serves has ever been public. The site is unpublished, no
workflow publishes it, and the landing page renders a "Soon" badge from an
empty `docsUrl`. The hostname is now in the one deployable source that
decides it — `git grep -n 'docs\.apprafter\.dev'` names `mkdocs.yml` beside
this ADR, `plan.md`, `FEATURE_TRACKER.md` and the two changelogs — and it got
there by this amendment's own correction below, not by a publication step.
Nothing serves it. A redirect entry would
therefore map a URL nobody has ever held onto one nobody can yet reach:
machinery for no user, which is the shape this track has refused at every
previous opportunity — the staleness gate, the typed `verified-by` bindings,
`since` in front matter and the `<llm-only>` tags are all the same call, each
recorded with the measurement behind it in the amendments above.

**The trigger is explicit — the first page that moves after 2.19g publishes
needs an entry.** `mkdocs-redirects` is already in the flake's python
environment, so adding it is a config block in `mkdocs.yml`, not a dependency
change. This is a recorded decision with a measured reason and a named
trigger, not an oversight; a future reader finding the plugin installed and
unconfigured is looking at that decision, not at a gap.

One class of URL *was* public, and no redirect map could ever have served it:
`README.md`'s relative links into `docs/` are GitHub blob URLs, served by
GitHub out of the repository tree, where an MkDocs plugin has no reach. The
only remedy for those is editing the README in the same commit as the rename,
which is what this subphase did.

**And what made the renames safe is not the census.** Every counter in
`docs/measurements/docs-health.json` is a total, so seven renames left `pages`
unmoved — to the census a rename and a deletion are indistinguishable. What
distinguishes them is `mkdocs build --strict` with link **and anchor**
validation, which fails on the first inbound link or heading anchor a rename
breaks, naming both ends. The link validator, not the census, is what makes a
URL change reviewable, and it is the reason a subphase of this shape could be
executed at all.

### Delivered

- **The documentation stopped addressing people inside the project.**
  Internal roadmap coordinates ("in phase 8.2"), subphase codes and pointers
  into `spec.md` were rewritten in reader terms, or deleted where the sentence
  was already complete without them. Where the specification genuinely is the
  answer, the citation is now an absolute GitHub URL **and the sentence says
  it is the repository's roadmap**, so a reader knows they are leaving the
  documentation.

  ```sh
  # 36 at fa3472e, 7 after the review corrections below
  git ls-files -- 'docs/*.md' \
    | grep -vE '^docs/(adr|changelog|measurements|superpowers)/' \
    | xargs grep -nEi 'phase[- ]?[0-9]|\b[0-9]+\.[0-9]+[a-z]\b|\bplan\.md\b|\bspec\.md\b|\bM[0-9]|track [ab]\b|sub-?phase|plan item'
  ```

  **That is the widened pattern, and the widening is the point** — see
  "the sweep's pattern defined its own success criterion" under Corrections
  below. The first pattern (`\bphase [0-9]` and three literals) read 25 at
  `fa3472e` and 7 at the first close, and ten insider coordinates spelled
  any other way passed straight through it.

  The seven that remain are deliberate and each is a different thing: four
  are the absolute-URL specification citations just described; one is
  `contributing/documentation-gate.md` naming `spec.md` as a **repository
  path** in a table of paths the gate excludes, where an https link would
  make the page describe its own scope in terms it does not use; and two are
  the generated reference's link targets for the `apprafter plan` command
  page, which is a real page named after a real command. The last two are a
  standing false positive of the pattern, not a leak — recorded here so the
  next reader does not go hunting for a clap doc comment that does not exist.

  The command deliberately scopes to `docs/`. `README.md` is in the gate's
  corpus but is not a site page, and its roadmap references resolve for its
  reader: someone on the repository's front page can open `plan.md`.

  **And it excludes four directories, one of which is a published site
  section — say so, or the count reads as a claim about the whole site.**
  `docs/changelog/`, `docs/measurements/` and `docs/superpowers/` do not
  publish at all. `docs/adr/` does: 59 pages behind a top-level nav tab,
  and at this commit 57 of them match the widened pattern
  — `plan.md` and `spec.md` citations, `Phase-4` and `Phase-8.5`,
  `Track A` and `Track B`, `sub-phases 1.5–1.18`, and "Phase 6.2" in a
  title that renders on the index.

  ```sh
  git ls-files -- 'docs/adr/*.md' | wc -l          # pages in the section
  git ls-files -- 'docs/adr/*.md' \
    | xargs grep -lEi 'phase[- ]?[0-9]|\b[0-9]+\.[0-9]+[a-z]\b|\bplan\.md\b|\bspec\.md\b|\bM[0-9]|track [ab]\b|sub-?phase|plan item' \
    | wc -l                                        # of those, matching
  ```

  Swap `-l` for `-n` to see the matching lines; their count is deliberately
  not written down, because the paragraph you are reading is inside the
  corpus that command counts and naming those coordinates above moved it —
  the same self-reference that made one of the artefact figures below
  wrong.

  **That is correct, and sweeping them would be the error.** An ADR is a
  dated record of a decision, written in the coordinates its authors were
  working in on the day they made it; rewriting one for an outside audience
  falsifies the record, and supersession — not editing — is how this
  repository moves a decision on. The section's own index says so before a
  reader opens anything: *"Read a record as history, not as status. Each
  describes the world on the day it was ratified."* So the count above is
  scoped to what it actually covers — **the guides and the reference**, the
  pages a reader arrives at to get something done — and is not a statement
  about the site's whole page count.

- **Two stub sections retired.** `architecture/index.md` and
  `concepts/index.md` each opened with "**Status:** stub" and pointed at a
  file the site does not carry, while holding two of nine top-level nav slots.
  Both were deleted rather than resolved in place: a page that exists only to
  say it does not exist yet costs a click and returns nothing.

- **The one table worth salvaging was re-derived, not moved.** The design
  called for moving the concepts page's CRD-to-owner table to
  `docs/reference/index.md` on the stated ground that it "is true today". It
  was not: checked against the CRDs the operator's Helm chart installs, it
  named four objects that ship no CRD at all — `AccessGrant`,
  `ExternalSurface`, `ServiceProviderPlugin` and `Infrastructure`, the last of
  which is not a cluster object in any phase but the local manifest the CLI
  parses — and omitted four that do ship. What moved is the table's *shape*;
  the rows come from the chart templates and each purpose from that schema's
  CUE docstring. Moving it unchanged would have promoted a stub's error into
  reference material.

- **Seven pages renamed, and `Public ingress` folded into the operator
  section.** "walk" is this repository's word for an end-to-end verification
  script (`e2e/needs-pg-walk.sh`); on the site it was a title and a URL a
  reader has no way to interpret. The pages are now named for what a reader
  gets. The two public-ingress pages moved directory as well as nav slot,
  because leaving them under `/public-ingress/` while listing them under
  Operator Guide is exactly the mismatch this subphase exists to settle. The
  nav is down from nine top-level entries to six, and `license.md` and
  `contributing/README.md` came off the `not_in_nav` allow-list into real nav
  positions — the licence in particular is a page visitors look for by name.

- **The `e2e/*-walk.sh` scripts keep their names.** That is what ships, the
  gate's `code_paths` floor resolves them, and the word is correct there. What
  was wrong was borrowing it for a reader.

### What the Deferred bullet said and did not happen

The bullet names "splitting the contributor walks from the user guides".
**That split was not made, because every one of those pages describes a task
a cluster owner performs.** There is no contributor/user axis running through
them to split them along. What was contributor-shaped was the *vocabulary*
and a handful of contributor-only sections, and removing the first and
demoting the second is what this subphase did instead. The operator/developer
axis — who owns the cluster versus who ships an app — holds and was kept.

An earlier version of this bullet supported that with a command count, and
the count was wrong: see "the command-dominance evidence was false" under
Corrections below. The four dependency guides are in fact `kubectl`-dominated
inside their fences, because the observations they teach are `kubectl` reads.
That is a reason to keep them together, not evidence for a split.

### Known gaps, recorded rather than papered over

Each of these is a real hole this subphase opened or left open, named here so
the next subphase can decide rather than discover.

- **There is no conceptual page.** Retiring both stubs means the site's entire
  conceptual explanation is now a table on a reference page. That is more
  honest than two apologies in prime nav position, and it is less than 2.19g's
  landing-page link will imply. A "what is this" page belongs to the content
  subphase, and a third stub is not an answer.
- **`Infrastructure` is now named on the reference page and documented
  nowhere.** No guide walks the local manifest; the nearest published surface
  is the `APPRAFTER_MANIFEST` row in `docs/reference/environment.md`. A reader
  who wants to write one has nowhere to go — a 2.19i guide.
- **The operator guide's *nav* is still one flat list of seventeen entries**,
  the longest on the site. The section's *index* now groups every page by job
  — set a cluster up, put it on the internet, give an application a
  dependency, day 2 — because it was failing as a map (it linked 11 of 16
  pages). The nav itself was left alone deliberately: grouping it means
  `navigation.sections` behaviour and possibly URL segments, and this
  subphase's remit was to settle URLs, not to import a documentation
  framework. A reader who lands on the index is served; a reader scanning the
  sidebar still sees seventeen undifferentiated lines.
- **Three sections on the dependency guides remain contributor-shaped**, and
  are demoted rather than removed: "Commands used on this page", the
  end-to-end-script note now at the foot of each page, and the checklists.
  They are useful to an operator running the chain for the first time, so
  deleting them would cost a reader something; they are also the last place
  where these pages read as a verification procedure rather than a guide. A
  content subphase should decide.

### Corrections after review

Three reviews ran on this subphase and each of the following was found by
**reading** something this amendment had asserted from a pattern. They are
recorded here rather than edited into the paragraphs above, so that the shape
of the mistake stays visible: *a census pattern is not a reading, and a check
that derives its expectation from the same side it checks is vacuous.*

- **`site_url` encoded the design decision 8 rejected.** `mkdocs.yml` read
  `site_url: https://apprafter.dev/docs` — a path on the landing origin,
  which decision 8 names and rejects for coupling two release cadences,
  caches and rollback units. That value is not cosmetic: `llm_export.py`
  bakes it into every absolute URL in `llms.txt`, `llms-full.txt` and the
  per-page markdown twins, and mkdocs bakes it into `sitemap.xml`, so all
  four artefact classes published the rejected design. **"The URLs are
  settled" was not true while it stood**, and settling them is what this
  subphase exists to do. Now `https://docs.apprafter.dev/`; rebuilt, and the
  artefacts follow.

  **The first proof committed here was itself false, and its replacement is
  the lesson.** It asserted "zero occurrences of `apprafter.dev/docs`
  anywhere under the built site" and committed
  `grep -rl 'apprafter\.dev/docs' /tmp/site | wc -l   # must be 0`. Run
  against a fresh strict build that returns **4**: this ADR's rendered page,
  its markdown twin, `llms-full.txt` and `search_index.json` — because the
  paragraph you are reading *describes* the rejected origin, and every
  artefact that carries prose carries it with them. A raw-string grep over
  the whole site cannot distinguish a URL the build **generated** from a URL
  a document **discusses**, and this ADR guarantees the second kind forever.
  Assert on the artefacts that carry generated URLs and nothing else —
  `sitemap.xml`, written by mkdocs, and `llms.txt`, written by
  `llm_export.py`, both of which are pure link indexes:

  ```sh
  nix develop --command mkdocs build --strict -d /tmp/site
  # every generated absolute URL is on the new origin
  grep -ohE 'https://[A-Za-z0-9.-]*apprafter\.dev[^ )">]*' \
      /tmp/site/sitemap.xml /tmp/site/llms.txt \
    | grep -v '^https://docs\.apprafter\.dev/' | wc -l   # -> 0
  ```

  The counts those artefacts carry are deliberately **not** recorded here.
  Each is a total over the corpus, so every page added or removed rots the
  figure while the command that produces it stays true — and one of the
  three originally written down (`llms-full.txt`) counted an artefact
  containing this document, so the act of recording it changed it. Read them
  off a build when you need them:

  ```sh
  grep -c 'https://docs\.apprafter\.dev' /tmp/site/llms.txt /tmp/site/sitemap.xml
  ```

- **The sweep's pattern defined its own success criterion, and four
  documents recorded the result as completeness.** The vocabulary sweep
  grepped `\bphase [0-9]`, `\b[0-9]+\.[0-9]+[a-z]\b`, `plan.md` and
  `spec.md`, hit zero on the site's pages, and the ledgers reported the site
  clean. Ten insider coordinates spelled any other way had passed straight
  through it — `Phase-4` and `Phase-8.5` (hyphenated, so `phase\s+[0-9]`
  cannot see them), `M3 target`, `M2+`, `Track A`, `Track B 1.70`, `plan
  item 1.80`, "later sub-phases" in a clap doc comment reaching
  `apprafter --help` and two generated pages, and two rows using "walk" as
  a noun for a document the reader has never heard of. One of them sat three
  lines below a blockquote this same sweep had cleaned, so the page
  contradicted its own cleanup.

  **This is the P1 shape the whole track is built to refuse** — an
  expectation derived from the same side it checks. The pattern is now
  `phase[- ]?[0-9]`, `\bM[0-9]`, `track [ab]`, `sub-?phase` and `plan item`
  alongside the originals; it reads **36** at `fa3472e` and **7** here, and
  the command in the Delivered bullet above is the one to re-run.

  **And the widened pattern is still a pattern.** Reading the pages found
  what no term above matches: `DoD checklist` as a user-facing heading on
  four pages, `MANDATORY: psql DROP assertion` and its two siblings, and
  `## Platform-CLI coverage` opening with "this guide exercises every
  shipped `platform-cli` subcommand … `kubectl` are sanity-only
  supplements" — the crate name rather than the product's, and this
  repository's walk-discipline vocabulary, in three guides' headings. All
  rewritten in reader terms. **Widen the pattern after reading, never
  instead of it.**

- **The command-dominance evidence was false, and it was the sole stated
  ground for a decision.** "Every one of those pages is
  `apprafter`-command-dominated … the one page with more `kubectl` than
  `apprafter` is the Git-repository guide" came from a regex counting
  `apprafter\s+[a-z]`, which sweeps up every prose mention — the same
  wrong-instrument error as the census, in the measurement that justified
  not splitting contributor pages from user pages.

  Counting **commands in command position inside fences** — what a reader
  actually runs — the four dependency guides are not close:
  `postgres.md` 40 `kubectl` against 10 `apprafter`, `redis.md` 39/10,
  `persistent-disk.md` 36/10, `egress-policy.md` 17/5. Those four are the
  output of the command below, re-run at this commit because they carry
  the argument. The rest of its table is deliberately not transcribed
  here: it moves with every fence anyone edits, and the command reproduces
  all of it in a second.

  **The instrument is the whole finding.** The claim it replaces counted
  `apprafter\s+[a-z]` across each page's full text, prose included — and a
  page's prose says "apprafter" because that is what the page is about, so
  the count measured subject matter and was read as command mix.

  ```sh
  # commands in command position, inside fences only
  for p in docs/operator-guide/*.md docs/dev-guide/*.md; do
    body=$(awk '/^[[:space:]]*```/{f=!f;next} f' "$p")
    a=$(printf '%s\n' "$body" | grep -coE '(^|[|;(]|&&|\$\()[[:space:]]*apprafter\b')
    k=$(printf '%s\n' "$body" | grep -coE '(^|[|;(]|&&|\$\()[[:space:]]*kubectl\b')
    printf '%-50s apprafter=%-4s kubectl=%s\n' "$p" "$a" "$k"
  done
  ```

  **The conclusion stands on its other ground and only on that:** these are
  all tasks a cluster owner performs, so there is no contributor/user axis
  to split them along. The true shape is worth stating rather than hiding —
  the dependency guides are `kubectl`-dominated inside their fences because
  the observations they teach (a claim reaching `ready`, a role appearing in
  `pg_roles`, a policy dropping a packet) are `kubectl` reads. That is a
  reason to keep them together, not to split them.

- **`ServiceProvider` was marked "Who writes it: Operator".** No `apprafter`
  subcommand creates one; the platform-stack umbrella seeds every one on a
  shipped cluster (`platform-stack/cue/service_providers.cue` —
  `pg-integrated`, `redis-integrated`, `disk-local`, `shared-local`), and
  four other pages on this site already say "the **seeded** `pg-integrated`
  ServiceProvider". The cell was carried over verbatim from the stub table
  the Delivered bullet above says was re-derived — so the re-derivation
  covered the row set and not the cells. The other four "Operator" cells
  were then re-derived by finding each writer in the tree rather than by
  inspection, and each cell now names the command or component that creates
  the object: `apprafter repo creds add`, `apprafter volume create`,
  `cluster_bootstrap.rs` plus `apprafter platform`.

- **Three references outside `docs/` still named renamed pages**, one of
  them program output. See the corrected rename bullet under Consequences
  below — that is the durable half of this finding.

- **Two front doors failed the jobs they exist for.** The operator index
  linked 11 of its section's 16 pages and mentioned backups only inside
  lists of unbuilt futures, while `backup-restore.md` is a 650-line shipped
  guide; it is now a map, grouped by job, with the unbuilt items in one
  labelled block. The operator quickstart offered a first-time operator one
  install path — `nix develop` plus `cargo install --path
  cli/platform-cli` — i.e. it answered "run this on my own server" by
  assuming a repository checkout; it now leads with the release binary the
  developer quickstart already documented. In the same shape, the four
  dependency guides opened with "## Step 0 — run the k3d e2e first", which
  told a site reader to run this project's test suite before using the
  product; moved to the foot of each page as an optional contributor note.

### Consequences

- **The URLs are settled, and this was the last subphase in which moving one
  was free.** After 2.19g every page move costs a redirect entry, and any move
  that crosses to a GitHub blob URL costs a README edit that no redirect can
  substitute for.
- **Inside `docs/`, a page rename is a `mkdocs --strict` problem. Outside it,
  the grep is the only instrument there is.** The first half of that was
  measured and holds: the inbound-link count for the renames was wrong by
  nearly half — same-directory relative links are invisible to the obvious
  grep — and the strict build found the remainder, naming both ends.

  The second half was asserted and is false, and every rename break that
  actually survived this subphase is in the blind spot it created. The gate's
  corpus is `git ls-files -- docs README.md` (the **root** README only),
  `mkdocs --strict` sees nothing outside `docs_dir`, and
  `.github/workflows/docs.yml` carries no `e2e/**` trigger — so three stale
  references to renamed pages shipped: a live markdown link in
  `e2e/README.md`, a comment in `cli/docsgen/tests/invocation_test.rs`, and —
  worst of the three — `e2e/lib.sh`, which printed a dead page name to an
  operator whose preflight had just failed, immediately before `exit 2`.

  **Name the directories, because the build cannot:** `e2e/`, `cli/`,
  `operator/`, `platform-stack/`, `scripts/`, `landing/`, `.github/` and the
  root working documents. Rename a page and sweep all of them; the excluded
  changelogs and `plan.md` are historical records and stay as they are.

  ```sh
  # after any page rename: references into docs/ that no longer resolve
  git grep -nE '(docs/[A-Za-z0-9_./-]+\.md)' -- \
    ':!docs/changelog' ':!docs/superpowers' ':!plan.md'
  ```

  Read the hits: in-test fixture paths (`docs/t.md`), a path belonging to
  another project (`docs/developer/crypto.md` in
  `cli/cli-providers/src/k8s/sealing.rs`, bitnami's), and the ADR-slug
  citations frozen into `platform-stack/cue/compatibility.cue` are all
  expected not to resolve. What must resolve is anything naming a page this
  site still serves.
- **The census caught one real loss and was not re-recorded around it.**
  Rewriting a page's reference list deleted a schema identifier along with the
  link text it sat inside; `identifiers` came back one below its floor, and
  the fix was to put the claim back, better placed, rather than to lower the
  number. That is the counter working exactly as decided in the 2.19d
  amendment.

## Amendment — 2.19g (publication) as delivered, 2026-08-19

Decision 8 is implemented. The site is a deployable application like any
other this platform runs: its own image, its own `Application.cue`, its own
release workflow, its own hostname. **It is not yet serving**, and the gap
between this branch and a reader reaching `https://docs.apprafter.dev/` is
entirely manual — see the handoff below, which is the part of this amendment
that must not be skimmed.

**This subphase does cut a CLI release, and the answer changed during
review** — it is recorded here in the direction it moved, because the earlier
answer was derived correctly from a tree that then grew a `cli/` change.

Re-derive rather than reading the conclusion:

```sh
git diff --name-only <base>..HEAD -- cli/ operator/ schemas/ \
    platform-stack/ argocd-cue-cmp/
```

`operator/`, `schemas/`, `platform-stack/` and `argocd-cue-cmp/` are empty,
so no chart artefact moved and no operator image is implicated. `cli/` is
not: `cli-core`'s `ApplicationExpose` gained an optional `port` (see the
consequence below), which changes shipped `apprafter app add` behaviour, so
`cli/Cargo.toml` is bumped in the commit that makes the change and the branch
carries a monorepo patch tag. `docs/reference/cli/commands.json` moves by
exactly one line — `cli_version` — which is the bump propagating rather than
a command-surface change; it is regenerated in the same commit, and
`docsgen check` is what would fail if it were not.

The new `ghcr.io/apprafter/docs` image is versioned by commit SHA rather than
by a git tag, so publication introduces no tag stream of its own.

### The site is built in the workflow, not in the Dockerfile

`docs-site/Dockerfile` is one stage over `caddy:2-alpine` and copies a
**prebuilt** `site/` in. It has no build stage, and the reason is recorded in
the file itself because it is the single most likely "improvement" a later
contributor makes.

A `pip install mkdocs-material` inside the image would introduce a second,
unpinned toolchain that produces the published site while the gate validates
a differently-built one. Decision 2 established one `python3.withPackages`
environment for exactly this reason: a byte-compared artefact needs one
pinned toolchain across a contributor's machine and CI. An image that builds
independently is that invariant's first breach, and the drift would be
invisible — both builds succeed and only the published bytes differ.

So `.github/workflows/release-docs.yml` runs `mkdocs build --strict` under
`nix develop` and hands the result to `docker/build-push-action`. The
published bytes are produced by a validating build rather than merely
vouched for by one.

### The release workflow gates itself, and does not lean on `docs.yml`

`docs.yml` guarantees nothing about this workflow: workflows triggered by one
event run independently, there is no `needs:` across them, and nothing here
observes that job's conclusion — a push that fails the gate would still run
this one to completion and publish. The two path filters also differ in the
publishing direction (`docs-site/**` fires a release and does not appear in
`docs.yml`'s filter at all), so commits exist that publish a site the gate
never ran on.

`--strict` alone is not a substitute. It catches dead links, dead anchors,
unlisted pages and missing includes; it does not catch a stale generated CLI
reference, a claim that no longer resolves against the product, a broken
`llms.txt` or markdown twin, or a link into an `exclude_docs`'d page — which
mkdocs reports at INFO, below `--strict`'s reach, and which
`scripts/docs-check.sh` greps for explicitly. Publishing on `--strict` alone
ships every one of those and reports success. So the release job runs
`scripts/docs-check.sh` itself, sharing `docs.yml`'s rust-cache key, and
checks out at `fetch-depth: 0` because `docsgen gate` ages exemptions against
real tag dates.

### The path filter fires when the published bytes change

The trigger is a path-filtered push to `master`, per the Deferred bullet's
"negated path filter" and the Re-evaluation bullet below it. The negation set
is **not** a copy of `mkdocs.yml`'s `exclude_docs`, and reading it as one
would break the site quietly. Re-derive the exclusion list with
`sed -n '/^exclude_docs:/,/^$/p' mkdocs.yml`; the entries split three ways:

- The internal changelog and measurement paths are **negated**, which is the
  Re-evaluation bullet discharged: they touch `docs/` on nearly every commit
  and change nothing a reader sees.
- `superpowers/` appears in neither list, because the tree is gitignored and
  cannot appear in a push diff. A pattern for an unreachable case is dead
  config.
- `hooks/` and `reference/cli/SUMMARY.md` are excluded from the site as
  static files but deliberately **fire**. The hooks *run* at build time and
  shape every page; `SUMMARY.md` is the literate-nav fragment and is the nav
  of every published page. A filter transliterated from `exclude_docs` would
  have made a hook rewrite and a nav change unreleasable, silently.

`mkdocs.yml` and `flake.nix`/`flake.lock` fire: a theme bump rewrites every
page's HTML and changes the fingerprinted asset filenames while nothing under
`docs/` moves. `docs-site/**` fires — the Caddyfile is baked into the image,
and without it a cache-header fix would reach production only whenever
someone next happened to edit a page. `docs-site/apprafter/**` is carved back
out: Argo CD reads the manifest from the cluster's config source, so it is
not an input to the image and editing it must not mint a digest. Product
source is absent by design: it changes what the gate *checks*, not what the
site *contains*, and the regeneration that must follow it touches `docs/` and
fires this.

**This coupling is stated and unenforced.** Removing an `exclude_docs` entry
publishes a page whose edits no longer cut a release, and nothing checks it.
Promoting that comparison to a real check is available work, recorded here
rather than in a comment nobody re-runs.

### The cache rules are shaped by what MkDocs actually emits

`mkdocs build` fingerprints exactly the bundle, the search worker and the two
stylesheets — `<name>.<8-hex>.min.<css|js>` with a `.map` each — and **that
is not all of `assets/`**: the favicon and the lunr language stemmers under
`assets/javascripts/lunr/` carry no digest. Re-derive with
`find site/assets -type f | grep -vE '\.[0-9a-f]{8}\.min\.'`.

The landing's blanket `header /_astro/* … immutable` is therefore not
transliterable, though it is correct there — Astro hashes every name it
emits. Copying it would pin mutable URLs for a year, and a theme upgrade
changing a stemmer would never reach a reader holding the old one. Three
disjoint matchers instead: a `path_regexp` on the digest form → immutable;
the two undigested subtrees → one day; everything else → 300s. The `{8}`
length anchor is load-bearing rather than decorative: a loose `[0-9a-f]+`
also matches `lunr.da.min.js` and `lunr.de.min.js`, since `d`, `a` and `e`
are hex digits. Anything under `assets/` matching neither rule falls through
with no `Cache-Control` at all, deliberately — under-caching is recoverable,
a year-long pin on a mutable URL is not.

HTML is 300s rather than the landing's 3600 because `search/search_index.json`
is not fingerprinted and must not outlive the pages it indexes. And `.md` is
forced to `text/plain`: the base image's `mailcap` supplies `text/markdown`,
which browsers offer as a download, defeating the twins' purpose — an input
this repository neither pins nor tests, so the header is set explicitly
rather than inherited.

That last rule matches on the **request** path, so it also decorated the
error response for a `.md` that does not exist: a missing twin answered `404`
with `Content-Type: text/plain` and the rendered HTML error page as its body.
`handle_errors` splits the two classes, so a reader who asked for plain text
is answered in plain text and a missing page still gets the rendered 404.

### The serving layer is gated, because nothing else reads it

Recorded because it was a real hole in this subphase as first written, not a
hypothetical: **nothing in this repository read `docs-site/Caddyfile`.**
`just lint` does not, `scripts/docs-check.sh` never looks below `docs/`, the
drift gate's corpus excludes `docs-site/` by construction (see the
consequence below), and no `caddy` binary is on a contributor's PATH. Every
decision argued at length above — the three disjoint cache matchers, the
content types, the deliberate absence of a `try_files` fallback — was
enforced by nothing. A config that crashed the container on start and one
that served every page `immutable` for a year were both published and
measured green.

Two guards, deliberately at different layers because they catch different
failures:

- **`caddy validate` as a build layer in `docs-site/Dockerfile`.** The
  binary is already the base image's entrypoint, so this costs a layer and
  turns a config the server cannot LOAD into a build failure instead of a
  crash-loop after the push. It is a syntax and loadability check and says
  nothing about behaviour.
- **`scripts/docs-site-smoke.sh`, run by the release workflow between build
  and push.** Starts the container and asserts how it ANSWERS: the four
  probes this track has used throughout, a real `404` on an unknown path, a
  readable plain-text `404` on a markdown twin that does not exist, and the
  `immutable` split over **every** file under `assets/`.

Two properties of the smoke script are the part worth keeping. Its probed
URLs are **derived from the file tree inside the image** rather than
hardcoded, so a page rename cannot turn a probe into a request for something
that never existed — which would satisfy the 404 assertions and vacuously
pass. And it asserts **both cache classes are non-empty** before claiming
every member of each passed, because a theme that stopped fingerprinting (or
started fingerprinting everything) would otherwise silently reduce one half
of that check to a claim about the empty set.

The expectations are not drawn from the side under test: the tree decides
WHICH URLs exist, the Caddyfile decides HOW they are answered, and only the
second is being checked.

Both were shown firing before being trusted. `caddy validate` fails the build
on a stray token, naming the file and line. The smoke script fails the
pre-split image on the markdown-404 byte bound (40,994 bytes against a 2,000
bound) and fails an image carrying the landing's blanket `/assets/*
immutable` rule on all 35 undigested assets — the exact mistake the matcher
argument above exists to prevent.

The script lives under `scripts/` rather than `docs-site/` and is therefore
**absent from the release path filter**, alongside the other gates: editing a
check must not mint a digest and roll the public site. That is the same rule
that carves `docs-site/apprafter/**` back out.

### A new subdomain needs no new zone and no new certificate

Worth recording because every future subdomain gets the same treatment, and
because it is the reason publication is a DNS record rather than a
certificate ceremony.

`platform-stack/cue/render_tool.cue` emits **two** HTTPS listeners per
`gateway.allowedDomains` entry — `https-apex-<san>` for the zone and
`https-wild-<san>` for `*.<zone>` — and both name the same
`importedCertRef` Secret. One registration, one certificate, both hostnames.
The operator's `render_httproute` sets `parentRefs[0]` to the `platform`
Gateway on port 443 with **no `sectionName`**, so a route attaches by
hostname match and nothing has to be told the wildcard listener exists.

The coverage rule is one label, in both layers, and that is the limit worth
knowing: `hostname_covered_by_zone` accepts the apex or a prefix containing
no dot, so `docs.apprafter.dev` is covered by `apprafter.dev` and a nested
host would not be. The CLI's `hostname_matches_domain` agrees. A subdomain
one level deep is free; anything deeper is a new zone.

### The handoff — what is left, and who does it

The software is complete and the site does not exist. **Six** steps stand
between this branch and a reader, none of which this repository can take.
`docs/operator-guide/publish-the-docs-site.md` is the runbook for the last
five and is written to be followed without this ADR or the plan.

1. **Push the branch.** Standing policy is that the agent commits locally and
   the operator pushes. Nothing else can start until the release workflow has
   run once and `ghcr.io/apprafter/docs:latest` exists.
2. **Make the package pullable.** A package published under an organisation
   for the first time is **private**, and nothing in the release workflow
   changes that. The cluster currently holds no `SourceCredential` matching
   `ghcr.io/apprafter/` — the credentials it does hold cover other orgs — so
   as things stand this lands as `ResolveFailed` plus `ImagePullBackOff`
   after every other step was done correctly. Either make the package public
   (which is why `landing-web` works today: it is anonymously pullable) or
   register a credential for the prefix. The runbook gives an anonymous-pull
   probe that distinguishes the two states, because GHCR's `403` does not.
3. **Confirm the zone.** `apprafter target domain list` must show
   `apprafter.dev`; the wildcard listener covers `docs.` by the rule above.
   If it is absent, registering it has certificate implications the runbook
   sets out.
4. **The DNS record**, in the operator's Cloudflare account: a proxied record
   for `docs` in the `apprafter.dev` zone. Outside the repository entirely.
5. **Register the application once**, from a checkout's `docs-site/`
   directory —
   `apprafter app add https://github.com/AppRafter/apprafter --name docs
   --path docs-site --branch master --no-interactive`. The working directory
   is part of the step: `app add` checks `<cwd>/apprafter/Application.cue`
   before anything else and, on a TTY, scaffolds a brand-new unrelated
   manifest when it is missing. `--path` is load-bearing in the other
   direction: a root registration sweeps in every other manifest this
   repository carries. Every later documentation change then deploys itself,
   because the operator re-resolves the rolling tag to its current digest
   each reconcile (ADR 0040), so this step happens exactly once.
6. **Set `docsUrl`** — in the CMS global *and* in
   `landing/web/src/data/fallback/siteSettings.json` — once, and only once,
   `https://docs.apprafter.dev/` answers. The tracked JSON feeds the
   *release* path (the landing image builds with `LANDING_USE_FALLBACK=1`)
   and the Payload global feeds the *preview* path, so both must be set;
   either one set before DNS resolves points the landing's front door at a
   host that does not answer, which is worse than the "Soon" badge it
   replaces because it looks shipped.

Until step 6, the landing keeps rendering the badge and nothing links a
reader anywhere. That is the correct state for a site that is not serving.

**Step 6 is a step and not a held-back commit, and the difference is not
stylistic.** It was first written as a commit of its own on the publication
branch, on the premise that a commit can be dropped from a push. That
premise is false in this repository: `.github/workflows/landing-autotag.yml`
fires on any push to the default branch whose diff touches `landing/**`,
bumps the newest `landing-v*` patch, pushes the tag and dispatches
`release-landing.yml`, which publishes `ghcr.io/apprafter/landing-web` at
`:<tag>` **and `:latest`** — and the landing's manifest watches `:latest`,
which the operator re-resolves each reconcile. Merging such a commit is
therefore the same act as deploying it, and holding it back would have meant
rebasing a commit out from under the closure that sits on top of it. The
commit was removed from the branch; the repository cannot make that ordering
safe, so the ordering belongs to the operator, written down where the
operator is already reading.

The same mechanism is why an unrelated landing defect found during this
subphase is recorded rather than fixed here — see the consequences below.

### Consequences

- **The documentation now has a deployment, and therefore an outage mode.**
  Everything before this was a build; from here a bad publish is a live site
  serving stale or broken pages. The mitigations are the release job's own
  gate, the 300s HTML TTL that bounds how long a bad page persists in caches,
  and rollback by SHA tag.
- **`docs-site/` is deliberately outside the gate's corpus**, and needed no
  exclusion in either place: `scan::in_scope` lists `git ls-files -z -- docs
  README.md`, where `docs` is a git leading-directory pathspec rather than a
  string prefix, and `exclude_docs` is relative to `docs_dir`. Verified by
  probe rather than assumed.
- **The shipped manifest was the first in the repository the CLI's own
  parser could not read, and that is a CLI defect rather than a manifest
  one.** `cli-core`'s `ApplicationExpose` declared `port` as a REQUIRED serde
  field, so `environments.dev.expose.network: "internal"` — an override that
  changes visibility and inherits the port, which is what a per-environment
  override is FOR — failed to deserialise with ``missing field `port` ``.
  Every other layer accepted it: `cue vet`, `apprafter app validate`, the
  admission webhook (zero errors) and the operator, which merges base's port.
  The sole caller of that parse is `app add`'s picker setup, and it responds
  to a parse error by warning, hiding the environment picker and skipping the
  namespace preselect — so a manifest the whole platform accepts silently
  lost two pieces of the wizard.

  Fixed in the model (`#[serde(default)] port: Option<u16>`), **not** by
  adding `port: 80` to the override. An A/B settled that: restating the port
  in `dev` makes the effective environment deployable-looking and pushes an
  operator toward the very environment the runbook forbids, which is a worse
  failure than a hidden picker. Requiring a field in this type can only cost
  reach — the CLI reads manifests and never writes them, and presence is
  enforced where it matters, in the schema, the CRD and the webhook.

- **Two deployable manifests are validated by nobody.** `scripts/lint-cue.sh`
  vets `schemas/`, `platform-stack/cue/` and `examples/` — not `docs-site/`
  and not `landing/`. The evidence that this matters is not hypothetical:
  `cue vet ./landing/web/apprafter` passes while `cue fmt --check` on the
  same directory fails, which is what an uncovered path looks like after a
  year. A schema change that invalidates either manifest currently surfaces
  at Argo CD sync time, in the cluster.
- **SPDX enforcement does not reach `docs-site/**` or `landing/**`.** The
  headers on the new files are correct and unenforced; `PATTERNS` in
  `scripts/check-spdx-headers.sh` has no glob that matches either tree. The
  tracked-file count the script prints moved by one when this subphase added
  a workflow and did not move when it added a `.cue` file under `docs-site/`,
  which is the measurement.
- **A finding about the landing, reported and not touched:** its Caddyfile's
  `try_files … /404.html` rewrites unknown URLs to a file that exists, so
  `file_server` never fails and every unknown URL answers **200** with the
  404 page body — a soft 404, which crawlers index as a real page. A/B
  measured on one image with only that line changed. The docs Caddyfile omits
  the fallback and returns a real 404, which is how the difference surfaced.

  **The reason it is not fixed here is the same mechanism as the handoff's
  step 6:** any commit touching `landing/**` that reaches the default branch
  auto-tags and publishes the landing, so a drive-by one-line fix in a
  documentation subphase would ship an unrelated production deploy of a
  different site. It belongs to whoever next changes the landing
  deliberately, which is why it is recorded in `plan.md`'s landing-migration
  item as well as here.
- **`scripts/docs-artefacts-check.py` crashes on a new page that is written
  but not yet staged** — `source` is built from `git ls-files` while
  `published` comes from the build output, and the `orphan_pages` guard that
  exists for exactly this case only appends to `problems`, below a line that
  dereferences the missing key first. Every contributor writing a new page
  meets a traceback before they meet the guard's message.

## Amendment — 2.19h (CLI UX) as delivered, 2026-08-19

The subphase this track was opened for. The request that started it named
`apprafter secret seal` as a command whose `--namespace` option was not
described, and asked that every key and option be described and current
examples shown.

**Half of that request was already satisfied before this subphase began, and
saying so is the first job of this amendment.** The measurement, re-derived at
the closing commit rather than quoted from the plan:

```sh
cli/target/debug/apprafter secret seal --help
```

prints `-n, --namespace <NAMESPACE>` with its description and its default, and
the doc comment now also carries the consequence that actually bites — "the
sealed blob only unseals as `<namespace>/<name>`". Corpus-wide, reading what
clap itself prints rather than the projection of it:

```sh
python3 - <<'PY'
import json, pathlib, re, subprocess
BIN = 'cli/target/debug/apprafter'
paths = [c['path'] for c in json.loads(
    pathlib.Path('docs/reference/cli/commands.json').read_text())['commands']]
ARG = re.compile(r'^ {2,6}(?:-\w, )?(--[\w-]+|-\w|<[A-Z_]+>|\[[A-Z_]+\.*\])(.*)$')
blank, seen = [], 0
for p in paths:
    lines = subprocess.run([BIN] + p + ['-h'],
                           capture_output=True, text=True).stdout.splitlines()
    sect = None
    for i, line in enumerate(lines):
        if line.endswith(':') and not line.startswith(' '):
            sect = line[:-1]; continue
        if sect not in ('Options', 'Arguments'): continue
        m = ARG.match(line)
        if not m or m.group(1) in ('--help', '-h'): continue
        seen += 1
        desc = m.group(2).strip()
        if not desc and i + 1 < len(lines) and lines[i + 1].startswith(' ' * 10):
            desc = lines[i + 1].strip()      # clap's second layout
        if not desc:
            blank.append(' '.join(['apprafter'] + p) + '  ' + m.group(1))
assert seen > 150, f'vacuous: only {seen} argument lines read'
print(seen, 'arguments rendered;', len(blank), 'with no description:', blank)
PY
```

`187 arguments rendered; 0 with no description: []`, across 97 help pages —
and `0` commands with no `about`. That was closed by earlier walk-fixes, not
by this track. **This subphase contributed the other half: the examples, and
the machinery that keeps both halves from rotting.** A closure that let a
reader believe otherwise would be the same class of false record this track
was built to police.

The sweep does **not** prove every description is *true*, only that every
argument carries one. It cannot: it reads the same text it judges. `apprafter
target add --token` was found by reading, not by this pattern — its help said
``Format `hcloud_<64+ alphanumeric>` `` while `validate_hetzner_token_format`
requires exactly 64 characters, all `[A-Za-z0-9]`, so an `hcloud_`-prefixed
value is rejected twice over. The validator was corrected in v0.1.74 and the
flag's help was not; the doc comment is fixed at this commit. A pattern is not
a reading.

One detail of the sweep is worth keeping: clap uses **two** layouts —
description on the same line, or indented on the next when the help is long —
so a parser that knows only the first reports five false blanks on this tree.
The first draft of the sweep did.

**This ADR previously offered a second detail as corroboration, and that
sentence was itself the defect it should have caught.** It read: the sweep
counts 187 arguments, "which is exactly the count in `commands.json`, so the
two derivations agree from opposite sides". The totals did agree. Per command
they disagreed in exactly two places that cancelled: the root's clap-generated
`help`, counted by `commands.json` and skipped by the sweep (−1), and
`apprafter platform freeze --version`, printed by the binary and **dropped**
from `commands.json` (+1). `docsgen::model` was filtering clap's generated
args by id string — `id == "help" || id == "version"` — and `platform freeze`
declares a genuine `--version <VERSION>` whose clap id is its field name. The
published page carried `Usage: apprafter platform freeze <COMPONENT>`, no
options table at all, and an `about` reading "Without `--version` — …" about a
flag the page never listed; every gate was green, because both sides of the
byte-compare are that same projection. **An aggregate equality presented as
independent corroboration is the vacuity of P1 in its purest form: a count is
not an assertion.**

The repair is on both sides. The filter is keyed on the arg's **action**
(`ArgAction::Help*` / `Version`) rather than its name, which is what
distinguishes an arg clap generated from one the CLI declared. And the
comparison is now a standing check made **per command** —
`every_command_projects_the_arguments_its_help_prints` in
`cli/platform-cli/tests/help_rot_test.rs` — running the SHIPPED binary's `-h`
over all 97 command paths and comparing the argument spellings it prints
against `docs/reference/cli/commands.json`, one command at a time. `--help` is
the one argument the two sides legitimately differ on (clap prints it
everywhere; the projection carries it once, on the root, where it is
documented), so that shape is asserted rather than exempted.

Re-derived at the parent commit, with that `--help` shape asserted rather than
skipped: **96 commands agreed, 1 disagreed** — `apprafter platform freeze`,
`prints ["--version", "COMPONENT"]`, `projects ["COMPONENT"]`. Counting the
root's generated `--help` as a difference too, the way the original sweep's
totals did, makes it 95 and 2. At this commit: 97 and 0, and the totals that
used to cancel now agree term by term — `commands.json` holds 188 arguments,
187 of them once the root's generated `--help` is set aside, which is the
sweep's 187.

### What shipped

- **`pub const EXAMPLES: &[CommandExamples]`** in
  `cli/platform-cli/src/examples.rs`, re-exported through the `docs_api`
  facade decision 4 established. **124 example lines over all 75 visible leaf
  commands** — every one of them, with the root, the 18 group nodes and the
  3 hidden `auth` leaves carrying none by rule.
- **`after_help` attached at run time**, not declared per variant:
  `run()` expands `Cli::parse()` into build → `examples::attach` → match.
  A `#[command(after_help = …)]` on each variant would put the text in a
  second place, which is the drift the table exists to prevent.
- **`docsgen`'s assertion C**, which resolves every example against the clap
  tree (`cli/docsgen/src/examples.rs`).
- **`apprafter completion <shell>`** over `clap_complete`, with install
  recipes in the developer quickstart.
- **A help-rot guard**, `cli/platform-cli/tests/help_rot_test.rs`,
  eight properties, seven derived from the clap tree at test time and one
  comparing the shipped binary's help against the published projection.

### The examples were in a blind spot between the two gates

An example is a claim about the CLI, written in the CLI's source and rendered
into a byte-compared artefact — and until this subphase nothing checked it:

- `docsgen check` byte-compares `docs/reference/cli/**` against a fresh
  render. It proves the pages match the source, never that the source is
  correct. An example with a misspelled flag renders identically both times
  and passes.
- `docsgen gate` deliberately excludes that tree, because `check` owns it, so
  the check that resolves `apprafter …` invocations never reads an example.

Each gate derives its expectation from the same side it checks. Writing 124
invocations into that gap would have shipped 124 unverified claims in the
artefact whose whole purpose is to be true — so the guard landed first, and
was seen failing on deliberately wrong examples before a real one was written.
It was demonstrated with `docsgen generate` run *first*, so the byte-compare
was clean while `docs/reference/cli/secret.md` published
`apprafter secret seal db-url --namesapce web`: the blind spot itself, shown
rather than described.

**One correction to this ADR's own account of that exclusion.** The plan for
this subphase cited `scan::EXCLUDED`. That array is
`["docs/adr/", "docs/changelog/", "docs/measurements/"]` and does not mention
the CLI reference; the exclusion is a dedicated `render::DIR`-prefix rule
inside `scan::is_in_scope`, spelled once at its source. The conclusion is
unchanged and the citation was wrong — recorded because a document citing a
measured fact that does not re-derive is itself a defect.

### The examples are a structural table, not a delimiter in `after_help`

Two designs were open: render the examples into `after_help` with a delimiter
`docsgen` parses back out, or keep the array reachable structurally through
`docs_api`.

The structural table won, and the decisive reason is not that parsing prose is
ugly. **A parse that finds nothing passes.** Change the delimiter, wrap the
block, restyle it, or write an example that contains the delimiter, and the
guard silently judges zero entries while still printing OK — the same vacuity
shape as a check deriving its expectation from the side it checks. Supporting:
there is no escaping story an example author would remember; parsing would
make the guard read a *rendering* of the truth when the truth is reachable
directly; and the table decoupled the guard from a rendering that did not
exist yet, which is the order this subphase needed.

The consequence carried through the design: because "unreadable ⇒ unjudged"
is the failure being avoided, an entry the guard cannot read is a **finding,
not a skip**. Five classes fire — unresolvable path, unresolvable flag, an
entry naming no `apprafter` invocation, an empty entry, and an example filed
under a command it does not invoke. The last one matters because copying a
sibling is the likeliest way to author a wrong page: `apprafter app list`
under `backup enable` is a true invocation and a false page. Aliases resolve
to the canonical path, so a child may be shown under its own page but not
under its parent's.

### Completions take `clap_complete::Shell` verbatim

The `<shell>` argument's type **is** `clap_complete::Shell` rather than a
local enum mapping onto it, because a local enum would be a second statement
of the supported set, kept by hand, about a capability owned elsewhere — and
`Shell` is `#[non_exhaustive]`, so that second list would freeze at today's
five the next time the generator grows one, with nothing to say so.

All five ship a script and each is asserted to name this CLI's commands, in
`cargo test`, driving the shipped binary. **Install recipes cover bash, zsh
and fish**; elvish and PowerShell get a script and no recipe, because a recipe
for conventions nobody here has walked would be invented rather than written.
The page says which three it covers and points at `apprafter completion
--help` for the full list instead of repeating it, so the documented set
cannot drift from the argument's.

The verb is `completion`, singular, matching `kubectl`, `helm`, `argocd` and
`k9s` — the tools this repository's own quickstart lists beside it. No alias:
every one of the 16 aliases in this tree is a shorthand, and clap's
did-you-mean already answers `completions` with the right suggestion.

Two upstream behaviours were found by walking the recipes rather than sizing
the files, and are recorded rather than worked around:

1. **A hidden subcommand still completes.** `#[command(hide = true)]` keeps
   `auth` out of `--help`; it does not keep it out of these scripts, and all
   five offer it. `clap_complete` filters hidden *possible values* only. Not
   fixed: clap exposes no way to remove a subcommand from a built tree, so
   suppressing it means rebuilding the tree by hand, and a hand-rebuilt
   `Command` silently drops whatever attribute the rebuild forgot — a worse
   defect than the one it fixes. If `auth`'s visibility is what matters, the
   decision to revisit is `hide` itself.
2. **Only bash and zsh complete a positional's value set.** `apprafter
   completion <TAB>` offers the five shells under bash and zsh and falls back
   to filename completion under fish, elvish and PowerShell. This is the
   tree's only positional `value_enum`.

### The help-rot guard pins properties, and one committed list on purpose

A full snapshot of every help page fails on every wording improvement and gets
regenerated without being read — worse than no test. The bar each property had
to clear instead: **a diff reviewer would not notice it breaking, and it
breaks something outside the diff when it does.** Eight passed it: every
visible leaf carries an example; no hidden command carries one; no example is
blank and no row promises lines it has none of; every command still has
`about`; every flag and positional still has short help; the alias
invocations the guides type still reach their command; the guides still
type every alias the table claims; and every command publishes exactly the
arguments its own `-h` prints.

The eighth arrived late — it is the standing check the repair for the dropped
`platform freeze --version` flag needed, and its absence is why this
paragraph said *seven* until a review re-counted the file. That is the rule
this document states, broken inside the branch that states it, so the number
now carries its witness: `grep -c '#\[test\]'
cli/platform-cli/tests/help_rot_test.rs`.

Seven of the eight derive from the clap tree at test time; the eighth
compares the shipped binary's help against the published projection. **One is
a committed list, and that is the design rather than a shortcut.** Deriving
"which aliases the guides use" from the clap tree is exactly circular: delete
the `t` alias and a clap-derived list simply stops containing it, so the check
passes on an alias that no longer exists. The list is the other side, and the
seventh property keeps it honest against the corpus — a row for a line a guide
stopped carrying fails, so the list cannot decay into folklore.

It was derived, not assumed. Building an alias-aware child index from
`commands.json` and walking every `apprafter …` token run in `docs/**`, seven
distinct alias invocations type seven alias tokens — `t`, `ls`, `info`, `rm`,
`kc`, `cb`, `up` — over **six** instructional pages, named here so the claim
carries its own witness: `docs/dev-guide/quickstart.md` and
`docs/operator-guide/`'s `backup-restore.md`, `quickstart.md`,
`shared-volumes.md`, `target-store.md` and `troubleshooting.md`. (This ADR,
`UNRELEASED.md` and `plan-history.md` all said *five*, copied from one slip in
the closing report's narrative — whose own table listed six. The seven
invocations and seven tokens re-derive; the page count did not. A count is
not an assertion, including this one: the command that produces it is on
`GUIDE_ALIASES`.) `apprafter volume rm` is correctly **not** among them
(`rm` there is the canonical name) while `apprafter app rm` is. Three parts of
the corpus type an alias and are excluded with a reason each: the generated
reference re-renders rather than going stale, `changelog/` and `adr/` are
records of what was decided when, and `superpowers/` is gitignored.

What that leaves uncovered is named rather than papered over: a guide that
*starts* typing a new alias is not added to the list automatically. It is
covered elsewhere — `docsgen gate` resolves every invocation in the in-scope
corpus, aliases included — and re-deriving the set inside the test would need
a second invocation tokeniser, which this track has already seen disagree with
the first.

### Five items the plan's own scope list named and this subphase did not build

`plan.md`'s (h) bullet named eight things. Three shipped — examples plus
`after_help`, completions, the help-coverage test — and the resolver guard,
which the bullet did not name, shipped as well. **Five did not, none of them
droppable by measurement: every one was probed and is a real gap.** They are
listed here because a checked box over an unbuilt list is the failure mode
this track exists to prevent.

| Named in (h) | Probed at closing | State |
|---|---|---|
| `secret list` | `apprafter secret --help` lists `seal`, `remove`, `help` | absent |
| namespace wizard for `secret seal` | no picker in `commands/secret.rs`; `--namespace` defaults to `apprafter-system` | absent |
| `status.conditions[]` + remediation in `app status` | `commands/app.rs` reads `/status/phase` and the claim's `Scheduled` condition; the Application CR's own `conditions[]` are never read, though `operator-core` emits them | absent |
| `requires = "select"` on `--namespace` | no clap `requires` attribute anywhere in `cli/platform-cli/src/cli.rs`; `export` and `backup create` still say "Ignored unless `--select` is also passed" in prose | absent |
| `app open --yes` | `commands/app_open.rs` prompts `Continue anyway?` on an unhealthy app and hard-refuses in a non-interactive shell — there is no escape for CI | absent |

Two of them have a trigger inside this track: 2.19i's sealed-secrets guide
cannot tell a reader how to list what is sealed or which namespace it landed
in without `secret list` and the wizard, and the guide for `app status` cannot
show remediation the command does not print. The other two are command-
semantics changes needing a decision of their own rather than a documentation
subphase: `requires = "select"` converts a silently-ignored flag into a parse
error on `export` and `backup create`, which breaks any script passing
`--namespace` alone, and `app open --yes` changes what a non-interactive shell
is allowed to do.

### Consequences

- **This subphase cuts a CLI patch release, `v0.2.46`.** Re-derive with
  `git diff --name-only <base>..HEAD -- cli/ operator/ schemas/
  platform-stack/ argocd-cue-cmp/`: every path but `cli/` is empty, so no
  chart artefact moved and no operator image is implicated; `cli/` is not, and
  the shipped binary changes twice over — a new `completion` command, and
  `--help` grows an `Examples:` section on 75 commands. `cli/Cargo.toml` is
  bumped in the same commit per the rule that `apprafter --version` names the
  code it is running, and `docs/reference/cli/commands.json` moves by exactly
  its `cli_version` line, regenerated in the same commit.
- **`after_help` is attached at run time, so a revert to `Cli::parse()` would
  be silent.** Every other gate stays green if the examples vanish from the
  only surface a user meets — the reference pages read the table directly.
  `cli/platform-cli/tests/examples_help_test.rs` is what fails instead, and it
  was seen failing in both directions before being restored.
- **clap aliases are invisible, and the two surfaces disagree.** `alias =`
  renders nowhere in `--help`, while the generated reference lists all 16 and
  the guides teach seven. The guard makes dropping one loud; it does not make
  them discoverable. `visible_alias` would, and that is a shipped-surface
  change, so it belongs in a decision rather than a test.
- **The `docs-artefacts-check.py` traceback recorded in the 2.19g amendment
  was met again**, by the new `completion` page: an unstaged new page dies
  with `KeyError: 'reference/cli/completion/'` rather than saying the page is
  untracked. Staging fixes it. Reported twice now, still unfixed.

## Amendment — 2.19i (the missing guides) as delivered, 2026-08-20

The last content subphase, and the one this whole track was sequenced to make
writable. It closes with **five** guides, not the "roughly fifteen" the
Deferred bullet below has carried since this ADR was written.

### The figure was never re-derived, and it was wrong in both directions

"Roughly fifteen missing guides" was an estimate made before any of the
machinery existed. Nothing re-checked it for seven subphases — the same defect
this track polices in every comment it reviews, applied to its own roadmap.
Re-derived from the shipped command tree rather than from the estimate:

```bash
python3 - <<'PY'
import json, pathlib
d = json.loads(pathlib.Path('docs/reference/cli/commands.json').read_text())
real = [c for c in d['commands'] if c['path'] and not c['hidden']]
leaf = [c for c in real if not any(o['path'][:len(c['path'])] == c['path']
                                   and len(o['path']) > len(c['path']) for o in real)]
guides = [p for p in pathlib.Path('docs').rglob('*.md')
          if not str(p).startswith(('docs/adr/', 'docs/changelog/', 'docs/measurements/',
                                    'docs/superpowers/', 'docs/reference/', 'docs/contributing/'))]
text = {str(p): p.read_text() for p in guides}          # keyed by PATH, not by p.name
for c in leaf:
    inv = 'apprafter ' + ' '.join(c['path'])
    if not any(inv in t for t in text.values()):
        print(inv)
PY
```

**The `str(p)` in that line is a correction, and the original defect is worth
recording because it produced three different answers to one question.** The
plan for this subphase keyed the corpus by `p.name`, so same-basename pages
overwrite each other in the dict: `docs/index.md`, `docs/dev-guide/index.md`
and `docs/operator-guide/index.md` collapse to one entry, as do the two
`quickstart.md`. Which one survives is dictionary-insertion order, so the
figure is not stable — the plan measured **12** at `0a2ba48`, re-running its
exact command on the same tree gave **11**, and keyed by full path the answer
is **10**. The two extras in the plan's count were `apprafter target show`
(shown twice in `docs/operator-guide/quickstart.md`) and `apprafter
upgrade-tier` (named in `docs/operator-guide/index.md`) — both pages lost to a
basename collision. Neither was a gap.

Of the stable 10, **three are deliberate absences** (below), so the real work
was **seven untaught leaf commands**, and they cluster onto five capabilities:

| Guide | Untaught leaves it closes |
|---|---|
| `docs/dev-guide/resources-and-autoscaling.md` | `platform autoscale set`, `platform autoscale show` |
| `docs/dev-guide/secrets.md` | `secret remove` |
| `docs/dev-guide/environments.md` | `platform env set`, `platform env show` |
| `docs/operator-guide/choosing-the-machine.md` | — (raises a one-mention capability to a page) |
| `docs/operator-guide/target-store.md` + `docs/dev-guide/image-iteration.md` | `target show`, `target rename`, `target remove`, `app rollback` |

The fifth row is two extensions rather than a new page, decided on content:
`target show`/`rename`/`remove` complete the lifecycle `target-store.md`
already opens with `add`/`list`/`use`, and splitting them across two pages
would have put `target use` and `target remove` in different places.
`app rollback` went to `image-iteration.md` because that page already teaches
"how a deploy happens and how to undo one" — and because its existing escape-
hatch bullet taught `kubectl rollout undo` as the *only* revert while a
first-class command shipped. That bullet is corrected in the same change.

### The three deliberate absences

`apprafter login`, `apprafter upgrade-tier` and `apprafter plan` are
**deliberately undocumented, and will re-appear in any future run of the
command above.** They are not an oversight and must not be written:

- **`apprafter login`** — its own `about` opens "NOT IMPLEMENTED, it prints
  what it would do and writes nothing", and points at `apprafter kubeconfig`
  as today's answer.
- **`apprafter upgrade-tier`** — likewise "NOT IMPLEMENTED, it validates
  `--to` and prints the move it would make". `docs/operator-guide/index.md`
  now says so in reader terms under "Not documented yet"; it previously read
  "`apprafter upgrade-tier` exists and the safety semantics run through
  `MigrationPlan`", which describes the capability as shipped and only the
  guide as missing. Corrected here.
- **`apprafter plan`** — a skeleton over `DryRunProvider`.

A guide for any of the three would document the future, which this project has
ruled out. Note that `upgrade-tier` no longer appears in the command's output
*because it is now named as absent* — the check is a mention test, not a
teaching test, and this is the one place that distinction bites. A reader
re-running it and seeing two rather than three should not conclude that
`upgrade-tier` acquired a guide.

### One defect the inventory surfaced, fixed rather than documented

`apprafter plan`'s `about` read "Show the diff between the desired state and
what is live" — but `cli/platform-cli/src/commands/plan.rs` constructs a
`DryRunProvider` and never reads the provider. `login` and `upgrade-tier` say
plainly that they are not implemented; `plan` claimed a capability it does not
have. The `about` is rewritten caveat-first (the generated reference lifts the
lead sentence into the overview table and the page description, so a caveat in
sentence two is invisible where the reader decides), naming `apprafter apply`
as where the comparison actually lives and `target show` / `platform status` /
`app status` as the read-only views. `apprafter status` was deliberately **not**
named: its own `about` says it is a skeleton that never contacts the cluster,
so pointing one skeleton at another is no answer.

### Contradictions between guides written in parallel, and how each was resolved

The five were written concurrently and could not see each other. Four
disagreements survived to the wiring step:

- **`--env` on an ambiguous app: "asks" versus "errors".**
  `environments.md`'s command table said the CLI "asks when there are two or
  more" while its own prose two sections later said it errors, and
  `image-iteration.md` said the flag "is only needed to disambiguate".
  `single_deployment_or_guidance` in `cli/platform-cli/src/commands/app.rs`
  returns `Err(per_env_guidance_message(…))` for two or more. The table now
  says it stops and lists them.
- **`metadata.namespace` pinned in a multi-environment example.**
  `environments.md` establishes that a manifest pinning `metadata.namespace`
  renders the same object for every environment — the configuration plugin
  stamps `spec.environment` and a label but never renames the CR — so a second
  environment collides. Both `docs/dev-guide/application-cue.md`'s
  multi-environment example and `resources-and-autoscaling.md`'s example
  pinned one while declaring `environments`, demonstrating the trap the new
  page warns against. Both now omit it, with the reason in a comment;
  `secrets.md` keeps its pin (its example declares no environments) and gained
  a pointer where it discusses sealing per environment.
- **The five per-field merge rules stated twice, near-verbatim.**
  `application-cue.md` §"Multi-environment patterns" and `environments.md`
  each carried the full list. Two parallel enumerations of one rule set drift.
  `application-cue.md` stays canonical — the merge rule is a field semantic
  and that page is the field reference — and `environments.md` compresses to a
  one-sentence summary plus a link, keeping only its own outcome tables.
- **`examples/applications/parser.cue` described as "a worked
  multi-environment manifest"** in the developer index. It pins
  `metadata.namespace: "demo"` and its `dev` override sets
  `expose.network: "vpn"`, which `operator/admission-webhook/src/validator.rs`
  rejects outright. The pointer is removed rather than repaired; the file
  itself is a `cue vet` fixture and is left alone.

### Both section indexes were rebuilt as maps

`docs/dev-guide/index.md` was a flat list of six bullets; three more would have
made nine, which is a list nobody reads. It is now grouped by the job — get
something running, describe your application, give it a dependency, ship it —
the same idiom 2.19f gave the operator index for the same reason.

### Consequences

- **This subphase cuts a CLI patch release, `v0.2.47`.** Not for the guides:
  the `plan` `about` fix changes `apprafter plan --help`, so the shipped
  binary changes, and the rule that `apprafter --version` names the code it is
  running applies. `cli/Cargo.toml` moves 0.2.46 → 0.2.47 and
  `docs/reference/cli/commands.json` moves by exactly its `cli_version` line,
  regenerated in the same commit. No chart, operator or configuration-plugin
  artefact is touched.
- **The census grew on every axis and shrank on none.** Live gate readings,
  `0a2ba48` → this commit: 33 → 37 pages, 427 → 566 invocations, 248 → 322
  identifiers, 2 → 5 complete CUE documents in 17 → 22 `cue` fences, 104 → 104
  code paths, 70 → 76 ADR references, and the LLM artefacts 60 → 64 indexed /
  119 → 123 bundled / 119 → 123 twins. **Exemptions stayed at 2** — five new
  pages needed no escape hatch. The *committed* floor had drifted well below
  the live reading (it still said 32/398/246/2/17/91/68/2, last written before
  2.19h) because it ratchets only on a fall; `docsgen metrics` re-records it
  here, which is what actually defends the new pages against being gutted.
- **`code_paths` did not move at all** — 104 before and 104 after. That is the
  "assume no repository checkout" rule showing up as a number: the five pages
  cite absolute GitHub URLs where they need a repository file and backtick no
  source path a reader without a checkout could not open.
- **Two of 2.19h's five unbuilt items had their trigger here, and it did not
  fire the way that amendment predicted.** It expected the sealed-secrets
  guide to be unable to say what is sealed and where without `secret list` and
  a namespace wizard. The guide answers both with `kubectl get sealedsecrets
  --all-namespaces` and a key listing, which is honest but is a `kubectl`
  answer on a page whose whole subject is a first-class CLI task. `secret
  list` and the wizard remain unbuilt, and the guide is the evidence that they
  are wanted rather than the blocker that was predicted.
- **`ServerTypeNotSelected`'s help names a manifest field that does not
  exist.** It tells the reader to set `nodes[0].kind`; the manifest key is
  `type` (`kind` is only the Rust field name behind a `serde(rename)`).
  `choosing-the-machine.md` quotes the real message and corrects it in prose
  rather than printing a field CUE will reject. The fix belongs in
  `cli/cli-core/src/error.rs` and is not made here — it would move
  `commands.json` a second time in a subphase whose CLI change is meant to be
  one `about`.
- **Shipped examples name `cx22`/`cx32`, which this repository's own changelog
  says were retired.** `cli/platform-cli/src/examples.rs` uses them in five
  places, which propagate into the generated reference and into
  `docs/reference/environment.md`. Either the examples teach a dead machine
  type or the changelog over-generalised a region-specific retirement; it
  could not be settled without a provider token. `choosing-the-machine.md`
  therefore names **no** machine type at all and teaches the picker as the
  list, which is the durable answer regardless of how that question resolves.
- **`docs/operator-guide/troubleshooting.md` gives advice that cannot be
  followed**: under `server_type_unavailable` it says to pass `--server-type`
  "with `--region`" on the provisioning command, but `--region` exists only on
  `target add` and `init`. The gate passes it because the two flags are never
  written in one invocation — a live example of "the gate checks names, not
  truth". Recorded, not fixed.

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
  immediately before publication so URLs settle once. *(Delivered in 2.19f —
  see the amendment above for the vocabulary sweep and the renames, for why
  the contributor/user split was **not** made, and for the measured reason the
  redirect map was not built together with the trigger that will require it.)*
- **Publication** — image, Caddy configuration, release workflow with a
  negated path filter, `Application.cue`, DNS zone, and the landing switch.
  Blocked on the restructure by design. *(Delivered in 2.19g — see the
  amendment above for the build-outside-the-Dockerfile decision, for what the
  path filter fires on and why it is not `exclude_docs`, for the wildcard
  listener that made a new zone and certificate unnecessary, and for the
  handoff: the software is complete and the site is not yet serving.)*
- **CLI-UX examples and completions** — per-command usage examples, shell
  completions and the surrounding usability fixes. Sequenced **before** the
  guide content, because the CLI check resolves examples against the *current*
  command tree: a guide documenting a command that does not exist yet is a
  hard error, not a to-do.
- **The guide content** — roughly fifteen missing guides for capabilities that
  exist only in `--help` today. Last, because every earlier package is a
  precondition for writing one that stays true. *(Delivered in 2.19i — and
  "roughly fifteen" was **wrong**: re-derived from the command tree it is
  five, closing seven untaught leaf commands, with three more deliberately
  left undocumented because they are not implemented. See the 2.19i amendment
  above for the corrected inventory command, why the original produced three
  different answers, the three deliberate absences, and the contradictions
  between guides written in parallel.)*

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
- `docs-site/` — the publication unit: `Dockerfile` (no build stage, by
  decision), `Caddyfile` (the three cache matchers and the `.md` content
  type) and `apprafter/Application.cue` (the deployable manifest).
- `.github/workflows/release-docs.yml` — the gated release path and its
  path filter; `docs/operator-guide/publish-the-docs-site.md` — the runbook
  for the manual steps it cannot take.
- ADR 0040 — tag-to-digest resolution. Why registering the application once
  is enough and every later documentation change deploys itself.
- ADR 0045 — per-application egress policy. Why the docs site shares the
  `apprafter` namespace with the landing rather than taking one of its own.
- `LICENSE-CC-BY-4.0`, `NOTICE` — the documentation licence texts.
