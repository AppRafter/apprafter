---
description: "What `just lint` checks in every documentation page, how to read a finding, and how to exempt a line that cannot be fixed."
---

# The documentation drift gate

`just lint` runs `docsgen gate`, which resolves what the documentation
*claims* against what the tree *ships*. If you add or edit a page and
the gate objects, this page tells you what it checked and what your
options are.

Run it directly while you work:

```sh
cd cli && cargo run -p docsgen -- gate
```

Exit **0** = no findings, **1** = findings to fix, **2** = the gate
itself broke (no `cue` on `PATH`, an unreadable page, a `cue`
diagnostic naming none of the documents under test, a missing or
unparseable census). A **2** is a toolchain or checkout to repair,
never a page.

A checkout with no tags is **not** a **2**. It reports every exemption
as `exemption-unaged` and exits **1**, which reads as "the
documentation is wrong" when the remedy is `git fetch --tags`. Only
the finding's own remedy text carries that distinction; the exit code
does not.

## What is in scope

Every tracked `docs/**/*.md` plus the root `README.md`, minus four
trees, each out for its own reason:

| Out of scope | Why |
| --- | --- |
| `docs/reference/cli/**` | Generated. `docsgen check` already byte-compares it against the clap tree; two gates with contradictory remedies on one page is worse than one. |
| `docs/adr/**` | A historical record. An ADR describes the world as it was when it was ratified. |
| `docs/changelog/**` | Same — a record, not a description of today. |
| `docs/measurements/**` | Internal working data. |

The repository's architectural specification, `spec.md`, is out by
decision and is not published on this site: it is the roadmap and
deliberately names capabilities that do not exist yet.

## What is checked

Ten classes. The first five resolve a claim about the product, and
**none of them is selected by a fence's language tag** — obligations
come from a block's content, so deleting a tag cannot quietly turn a
finding green. Nor can deleting the fence: an indented block and an
HTML `pre` element still owe the content checks — with two limits
worth knowing, set out below.

| Code | What it means |
| --- | --- |
| `cli-invocation` | An `apprafter …` line — in a fence, a literal block, or an inline span — whose command path or flag names do not resolve against the clap tree. Values and required-ness are **not** checked: a documented command is a reference, not a runnable line. |
| `schema-identifier` | A backticked field path (`spec.…`, `base.…`, `expose.…`, `needs.…`, `Kind.spec.…`) that no shipped schema declares, or one naming a `needs` type no provider ships. Runs page-wide — prose, tables and fences alike. |
| `cue-document` | A fence or literal block that is a complete CUE manifest (a `package` clause *and* the schema import) which `cue vet` rejects. Fragments are out of scope. |
| `code-path` | A repository path the page names — in a code span opening on a real top-level directory, or as a relative link target — that does not exist in the repository. |
| `adr-reference` | An ADR citation that names no ADR at all, or one whose decision no longer stands: `Superseded`, `Deprecated`, or an `Unused` reserved slot. |
| `unlabelled-fence` | A fence with no info string. |
| `unterminated-fence` | A fence that never closes, so everything below it renders as code. |
| `unclosed-pre` | An HTML `pre` element that never meets its closing tag. The same failure as the row above with a different edit behind it, which is why it is its own class. |
| `health-baseline` | An obligation count fell below the committed census: the corpus lost documented surface it used to have. |
| `health-exemptions` | The declared-exemption count no longer equals the census — one was declared, or one was retired. |
| `recipe-purity` | A foreign command in a guide's recipe path: not `apprafter`, not an allowlisted external tool, not inside a collapsed disclosure, and not on a break-glass page. |

## `recipe-purity`

[ADR 0058](../adr/0058-public-surfaces-are-written-for-their-reader.md)
makes a guide a recipe: its main flow carries `apprafter` commands and
genuinely external tools, and everything else routes by role — walk
material deleted, independent verification collapsed into a `???`
disclosure, mechanism to its own page, failure handling to
`troubleshooting.md`.

The check runs as part of `docsgen gate`. It was built **red** — 174
foreign commands across 14 guide pages — and deliberately held out of
that run for the length of the 2.20c restructure, because a red check in
the run lefthook and `just lint` call would have made the repository
uncommittable while 27 pages were being rewritten. The commit that took
the corpus to zero moved it in.

When there are findings, the readable form is grouped by page:

```sh
cd cli && cargo run -p docsgen -- recipe-report
```

It exits 0 whatever it finds — it is a report, not a second gate.

Two limits are deliberate and worth knowing before you read a silent
page as a clean one:

- **Fenced and literal blocks only.** A fence body is shell by
  construction, so reading its first token as a command word is sound;
  prose is not, and the same reading over inline spans returns the first
  word of the sentence. A `kubectl` written in a prose span is not seen.
- **It cannot tell whether a disclosure is honest.** It knows a foreign
  command is inside one, not that the block explains anything. A page
  could satisfy it by collapsing everything and saying nothing; that is
  review's job, and the one-block-per-section-outcome rule is stated in
  ADR 0058 rather than enforced here.

A block that is a documented workaround for a tracked defect is the
third way to be silent, and the honest one: mark it
`<!-- docs: check=none reason=known-broken since=vX.Y.Z — why -->`.
Collapsing such a block into a disclosure would bury the thing a reader
most needs to see, while the typed channel keeps it visible, counted,
and expiring — so it goes when the defect does, or it says so loudly.
The corpus carries several today, each pointing at an entry in
`docs/measurements/day2-followups.md`.

An external tool the platform genuinely cannot own belongs on the
allowlist in `cli/docsgen/src/recipe.rs`, with the reason on the row —
the same shape `shipped.rs` uses. A whole page that is not a recipe
belongs in that module's break-glass table, also with a reason; today
that is the rescue runbook, whose reader is in a provider ramdisk with
no cluster and no binary, and the troubleshooting catalogue, which is
where the ADR routes failure handling in the first place.

Prefer the **kind-prefixed** form when you write a field path:
`PlatformStack.spec.pin` resolves against `PlatformStack` alone, while
a bare `spec.pin` is resolved against the union of every AppRafter
kind and can therefore pass for the wrong reason.

Five more codes are about the exemption machinery rather than about
the documentation: `docs-marker` (malformed marker), `front-matter`
(malformed exemption entry), `exemption-expired`, `exemption-unaged`,
`exemption-unused`.

Every finding prints its own remedy. Read it — they differ by class.

### Unfencing a block keeps its content obligations

A run indented four spaces at the top level, and an HTML `pre`
element, both render as code and both owe the obligations that come
from a block's **content** — the CLI, CUE and identifier checks. That
is deliberate rather than thorough: the remedy for an unlabelled fence
is "give the fence an info string", and deleting the delimiters and
indenting the body instead is the shorter way to make that report go
away. The page renders identically and nothing in the diff reads as
suppression, so the content checks have to follow the content.

Two limits on that, stated because a heading like the one above used
to overstate them:

- **The `unlabelled-fence` finding itself does go away.** A literal
  block has no fence to label, so reporting it would name a remedy
  that does not apply. That escape is real and no census field counts
  unlabelled fences, so nothing ratchets it either.
- **Indentation inside a container is not a literal block at all.**
  Tabbed blocks, admonitions and list items all indent their contents
  four spaces, and those contents are read as prose — so a block
  indented under a list item sheds the CLI, CUE and identifier checks
  entirely. Only a run indented at the top level is code. That is the
  price of not reporting six false positives on
  `docs/dev-guide/quickstart.md`, whose install instructions are
  tabbed, and it is a price worth naming: these checks defend against
  the careless, not against the motivated. Review defends against the
  motivated.

A `pre` element that never closes is `unclosed-pre`, because
everything below it is inside the block — including any fence, which
stops being read as a fence.

A literal block **cannot be exempted by a marker**: a marker annotates
the fence on the line below it, and a literal block has no such line.
If the content genuinely needs an exemption, fence it and mark the
fence.

### `code-path`: two surfaces, two roots

Which root a path resolves against depends on how it is written, not
on how it looks:

- a **code span** is prose — a path a reader greps for from the
  repository root, so it resolves there;
- a **relative link target** is a destination, so it resolves against
  the page's own directory, exactly as MkDocs resolves it.

A code span that is a link's *text* makes no claim of its own: the
link's target is the claim, and it is checked page-relative.

A glob is not a claim about one file, so `providers/*` and
`docs/**/*.md` are extracted like anything else and then suppressed.
The pattern characters are `*`, `?` and the two braces — deliberately
**not** the square brackets, because three tracked files carry
brackets in their names, and counting those as patterns would suppress
exactly the references that matter most on the day one of them moves.

**A code span is only read as a path when it opens on a real
top-level directory of the repository** — and that is the largest
thing this check does not see, so write with it in mind.
`cli/cli-core/src/style.rs` is checked; the crate-relative
`cli-core/src/style.rs` is invisible, because `cli-core` is a crate
name and not a directory of the repository. That is not
hypothetical: `docs/reference/environment.md` writes 24 such
references across 16 distinct paths, the largest concentration of file
references anywhere in the corpus, and not one of them is verified.
The anchor stays narrow because widening it to any slash-shaped token
would admit some seventy non-repository tokens the corpus legitimately
writes — `$HOME/.config/apprafter/age.key`,
`/api/v1/nodes/{node}/proxy/stats/summary`, `.apprafter/state.json`
and the rest — and a check that reports those is a check people switch
off. **Write the `cli/` prefix and your path gets checked.**

This class has **no ignore key**. A path belonging to someone else's
project is named as theirs in the sentence rather than exempted,
because a reader will otherwise grep this repository for it.

Links to a `.md` page are not this check's business: the strict MkDocs
build already resolves those, and their anchors with them.

### `adr-reference`: citing a decision that still stands

`Draft` and `Proposed` are **not** findings — documentation
legitimately cites a decision the project is working to. `Superseded`,
`Deprecated` and `Unused` are, and so is a status the gate cannot read
at all: treating "I cannot read this" as "the decision stands" is a
guess made in the direction of silence.

The verdict comes from the **leading token** of the ADR's `## Status`
body, never from a search through it. Several ADRs here are amended
rather than withdrawn — the status opens `Accepted` and goes on to say
that one section is superseded in part — and a substring rule would
report every page citing one of them, which is a gate that gets
switched off within the week.

All the spellings count as one citation: `ADR 0046`, `ADR-0046`, the
plural, a citation wrapped across a line break, and the number
embedded in a link target or a file path.

**Citing a decision that no longer stands is sometimes exactly
right** — a sentence that announces the supersession in the same
breath is describing the history, not relying on it. Say so in the
sentence and why; if the citation is still reported, declare an
`adr-check-ignore` entry (below).

### The census

`docs/measurements/docs-health.json` records a number per counter about
the corpus, and the gate compares this run against them **by value**:

- the **obligation counts** — pages, invocations, resolved
  identifiers, complete CUE documents, `cue` fences, code paths, ADR
  references — may not decrease. (`cue` fences are counted separately
  from complete CUE documents because it is the *fence* that the built
  site has to render as highlighted CUE, and
  `scripts/docs-artefacts-check.py` is only as strong as the number of
  them there are to check.)
  Growth passes silently and needs no action: adding
  a guide is not a regression, and a gate that made you re-record a
  file in order to write a page is a gate you would route around.
- **`exemptions`** is compared for equality in *both* directions, so
  declaring one and retiring one are each a finding. Moving that
  number costs one line in a committed file, which is exactly the
  review moment an exemption should get.

Re-record with:

```sh
cd cli && cargo run -p docsgen -- metrics
```

Never to make a finding go away. A `health-baseline` finding usually
means something *else* in the same run is the real edit — `identifiers`
counts the paths that **resolved**, so a renamed schema field or a
fresh `schema-check-ignore` entry lowers it with nothing deleted, and
the `schema-identifier` findings beside it are what to fix. When
documentation really did go and the removal was deliberate, re-record
in the **same commit** as the deletion and say in the commit message
what went and why: the re-recorded number is the only trace that
remains.

A missing or unparseable census is exit **2**, never **1** — a broken
gate, not a page to edit.

What the census cannot see is worth knowing before you rely on it. It
counts cardinality, not specificity: swapping a twelve-flag invocation
for a bare one, a deep field path for a shallow one, or a file for its
parent directory all hold every number. Prose carrying no counted
claim is invisible to it entirely. These ratchets defend against the
careless, not against the motivated — review defends against the
motivated.

## The marker: exempting a fence

A `docs:` HTML comment on the line **immediately above** a fence
annotates that fence and nothing else:

```text
<!-- docs: check=none reason=third-party-output since=v0.2.44 — helm's own table -->
```

`check=` takes `cli`, `cue` or `none`. A marker never *narrows* an
obligation — `check=cli` on a block that is also a complete CUE
document does not switch the CUE check off. Only `check=none`
silences, and it costs both a typed reason and a `since=`.

The grammar is strict on purpose: an unknown key is an error, a
duplicated key is an error, and a key that does not pair with the
chosen check is an error. A marker that silently means nothing reads
to a reviewer as though it works.

## Front matter: exempting an inline span, a field path or a citation

An inline span carries no marker — a comment mid-sentence is
unreadable as prose and unreviewable as an exemption. Spans, field
paths and ADR citations are exempted at page level, in the page's YAML
front matter, by their **literal text**:

```yaml
---
cli-check-ignore:
  - span: "apprafter node reserve-headroom"
    reason: historical
    since: v0.2.44
    note: names the removed command so scripts calling it can be migrated

schema-check-ignore:
  - path: "spec.source.path"
    reason: external-tool
    since: v0.2.44
    note: Argo CD's Application CR, whose field set AppRafter does not model

adr-check-ignore:
  - adr: "ADR NNNN"
    reason: historical
    since: v0.2.44
    note: the sentence is about the decision that was replaced, and says so
---
```

Matching is **exact equality on the trimmed text, never substring**:
the `span:` entry above covers that command and only that command, not
the same line with a `--dry-run` appended, which is a different claim.
An exemption that matches nothing is itself a finding
(`exemption-unused`) — once the page is fixed, the entry is a claim
about a problem that no longer exists.

`adr-check-ignore` is the one exception to "literal text". Its key is
the citation's **canonical** form — the four-digit number after the
word `ADR`, written out in place of the `NNNN` above, whatever the page
itself writes. Two spellings of a command are two different claims;
two spellings of a citation are one, so a single key covers the prose
form, the hyphenated form and the link. It is also the text the finding
quotes, so the key is copied straight out of the report.

That key is why the example above cannot carry a real number: this
page is itself inside the gate's corpus, and an ADR number written
here — in a fence as readily as in a sentence — is a citation the gate
resolves like any other.

## The typed reasons

A closed vocabulary, so exemptions are countable by kind. "We have
fourteen third-party-output exemptions and one known-broken" is a fact
a reviewer can act on; fifteen free-text sentences are not.

| Reason | Use it for |
| --- | --- |
| `third-party-output` | Output of something we do not own — `kubectl get`, `helm ls`. |
| `illustrative-fragment` | A deliberately incomplete snippet, shown to make one point. |
| `external-tool` | An invocation, or a schema, belonging to a tool that is not AppRafter. Correctly documented; not ours to model. |
| `known-broken` | The documented thing **is** wrong and is tracked elsewhere. The most expensive one to leave standing, and the one expiry exists for. |
| `historical` | Surface that deliberately no longer exists — a page correctly documenting a removal. The resolver cannot tell that from drift, and calling it `known-broken` would claim a correct page is wrong. |

The free text after the em dash (marker) or in `note:` (front matter)
is kept, not dropped. It is the only human-readable part of an
exemption.

## Expiry: 180 days

`since=` names a release; the gate resolves that tag to its commit
date. Past 180 days the exemption is **void** — it stops silencing and
is reported. An exemption is a claim about the world, and the world
moves; re-justifying one is a minute's work, while inheriting one
nobody has looked at in two years is how a gate ends up guarding
nothing.

Two consequences worth knowing before you meet them:

- **`since=` must name an already-released tag** — the last release,
  not the one being prepared. A tag that does not exist yet cannot
  date anything.
- **An exemption the gate cannot age is void too** (`exemption-unaged`),
  and a checkout with no tags produces that for *every* exemption at
  once. If the gate suddenly objects to exemptions you did not touch,
  run `git fetch --tags` before editing anything. CI is immune because
  the docs job checks out with `fetch-depth: 0`.

## When the gate runs

- `just lint`, via `scripts/docs-check.sh`.
- The lefthook `pre-commit` hook, when a commit touches the pages, the
  CLI source, `docsgen`, the schemas the checks resolve against, or the
  committed census.
- `.github/workflows/docs.yml`, on the same file set.

A schema or flag rename is a documentation change even when no page is
touched, which is why the schema entry is there. The census is listed
for the opposite reason: it is an **input** to the gate rather than
another page, so a commit that touches only it still has to be
judged — and no hook ever regenerates it, which is what keeps the
ratchet from being a no-op.
