---
description: "The front matter a page owes, what the two build hooks do with it, and what an author gets for free."
---

# Writing a documentation page

Everything under `docs/` is built by one pinned MkDocs toolchain and
checked by one gate. This page is the author's half of that contract:
what a page owes in its front matter, what the build does with it, and
what you get without asking for it.

Its companion is [the documentation drift gate](documentation-gate.md),
which covers what `just lint` checks in the *body* of a page — the
`apprafter` invocations, field paths, CUE manifests, repository paths
and decision citations that must resolve against what actually ships.
Read that page when the gate objects to something you wrote; read this
one before you write it.

## Front matter

Front matter is a YAML block between two `---` lines at the very top of
the file, before the `#` heading:

```yaml
---
description: "What this page answers, in one sentence."
audience: operators
---
```

Two fields are part of the authoring contract. Everything else in a
page's front matter is an [exemption key](#the-exemption-keys), read by
the gate rather than by the site.

### `description` — required {#description}

Every page the index lists must carry one. A build hook fails the build
when one does not, naming the pages, because the alternative is silence:
an index entry with no summary is a link nobody has a reason to follow,
and nothing else in the toolchain would ever report it.

It is required because it is the one thing that **cannot be derived**.
Falling back to a page's first paragraph produces an index whose entries
are scene-setting clauses rather than summaries — and it would be
silent, so nobody would learn the index had gone thin.

It is also the one front-matter field worth requiring, on the test this
repository applies to every proposed field: **a description is a
summary, not a claim about the world.** An author writing one cannot
make it false the way a test binding goes false while nobody is looking.
That test is what dropped `verified-by`, and it is what drops `since`
below.

Write what the page *answers*, in one sentence, without repeating the
title:

- **Good** — "The on-disk layout of target configuration, the credential
  resolution chain, and the multi-target patterns operators actually
  use."
- **Bad** — "Overview of the target store." It is read out of context,
  as one line in a list of thirty, where "Overview" tells a reader
  nothing they did not get from the title.

Say only what the page supports. A description is prose, so the gate
cannot check it against the page — three of the first batch written were
rewritten before landing because each promised something its page did
not deliver. That check is review, and review is the only one there is.

### `audience` — optional, and defaulted {#audience}

`audience` labels who a page is written for. It is **optional** because
it can be derived, and the derivation is what the site would otherwise
ask an author to retype:

1. the page's own front matter, when it has one;
2. otherwise the nav group the page sits in, lowercased and hyphenated —
   a page under *Operator Guide* becomes `operator-guide`;
3. otherwise the group of the nearest ancestor directory that holds nav
   pages, which is how a page deliberately kept out of the nav still
   inherits from its neighbours;
4. otherwise `general`.

Set it only where the derived answer is wrong. A field an author must
retype to state what the tree already says is a field that will one day
disagree with the tree.

### There is no `since` {#no-since}

An earlier design had every page declare the release it was written
against. It was dropped, and the reason is worth stating because it
generalises: **nothing can check it.** No gate reads it, no build fails
on it, and no reader can tell a page whose `since` is current from one
whose `since` was correct two releases ago. It is written once at
authoring time and rots in silence — the same shape as the test binding
that was dropped before it.

Contrast the `since=` inside an *exemption*, which stays: that one names
a released tag, and the gate resolves the tag to its commit date and
voids the exemption after 180 days. It is checkable, so it is kept. The
distinction is not the field name — it is whether anything in the tree
can call the claim wrong.

**If a page needs to name a version, it names it in the prose**, where a
reader will see it and a reviewer will question it.

### The exemption keys

`cli-check-ignore`, `schema-check-ignore` and `adr-check-ignore` also
live in front matter. They are the gate's escape hatch, not part of the
authoring contract, and each entry costs a typed reason and a dated
`since`. They are documented on
[the documentation drift gate](documentation-gate.md#front-matter-exempting-an-inline-span-a-field-path-or-a-citation).

Adding a `description` to a page that already carries one of these must
leave the existing keys untouched — the gate reads them, and breaking
one turns a documented exemption into a finding.

## The two build hooks

`mkdocs.yml` lists two hooks under its `hooks:` key. MkDocs loads them
by path and hands them the same event API a plugin gets, which is how
they do two things no packaged plugin here can. Both live in
`docs/hooks/` so they sit beside what they act on, and both are excluded
from the site itself so they are not published as static files.

Neither hook commits anything. Everything they produce derives from
committed content at build time, so none of it can drift from the pages
it describes.

### `docs/hooks/cue_lexer.py` — CUE renders as code

Pygments ships no CUE lexer, so without this hook every ` ```cue ` fence
on the site would render as unstyled text. The hook registers one.

The interesting half is that it then **proves the registration took**
and fails the build when it did not. The markdown extension that does
syntax highlighting catches a failed lexer lookup and quietly falls back
to plain text, so a broken registration is otherwise a green build,
finished-looking pages, and nothing in the log. The hook resolves the
alias, tokenises a sample, and raises unless the sample comes back as
CUE. Nothing else in the toolchain reports this.

As an author you owe it nothing: tag a fence `cue` and it is
highlighted.

### `docs/hooks/llm_export.py` — the machine-readable layer

Documentation is read by models as well as by people, and asking one to
scrape rendered HTML is asking it to read the theme. This hook writes
three things into the built site:

| Artefact | What it is |
| --- | --- |
| `llms.txt` | The curated index — the site name, its description, the licence line, then one section per nav group (plus one for pages deliberately kept out of the nav, so the index stays complete), each entry an absolute URL and the page's `description`. |
| `llms-full.txt` | Every published page's markdown in one file, each behind a header carrying its URL, description and audience. |
| a markdown twin | The page's own source markdown, published beside it: the page URL with the trailing `/` dropped and `.md` appended. |

Which pages are in is read from the build rather than restated: the site
exclusions in `mkdocs.yml` already remove the internal trees, so those
pages never reach the hook.

The architecture decision records are the one judgement call, and it is
split. They are **bundled and twinned but not indexed**: each describes
the world as it was on the day it was ratified — which is why the drift
gate holds them out of scope too — and they are half of everything the
site publishes, so indexing them would bury the guides. A model handed
the whole corpus should have them; a model handed a map should not have
to walk past them.

## What you get for free

Write the page, give it a `description`, put it in the nav, and the
build hands you:

- **syntax highlighting**, CUE included;
- **an HTML `meta` description** — pages without one fall back to the
  site-wide description, which is the same sentence on every page and
  therefore useless to a search result;
- **an entry in `llms.txt`**, under the nav group's heading, labelled
  with the name you gave it in the nav;
- **an entry in `llms-full.txt`** and **a markdown twin** at your page's
  own URL.

None of that needs a second registration anywhere. The nav entry and the
`description` are the whole input.

## This page is checked by the machinery it describes

`docs/contributing/documentation.md` is inside the gate's corpus, so
every claim on it is resolved on every run: each fence carries an info
string, each repository path it names must exist, and each decision it
cites must still stand. That is deliberate — a page explaining a gate
that the gate does not read is the first page to go stale.

## Checking your work

```sh
just lint
```

That runs `scripts/docs-check.sh`, which is the whole documentation gate
in four passes: the strict MkDocs build, the LLM artefacts checked
against the site that was just built, the generated CLI reference
byte-compared against the code, and the drift gate over the hand-written
pages. It needs Nix, because a byte-compare needs one pinned toolchain
across your machine and CI.

Preview the rendered site while you write:

```sh
just docs-serve
```

The preview server does not pass `--strict`, so a broken link is a
printed line there and a failed build in `just lint`. The build is the
enforcement point.

## Further reading

- [The documentation drift gate](documentation-gate.md) — what is
  checked in a page body, and how to exempt a line that cannot be fixed.
- [Local development setup](setup.md) — getting the pinned toolchain.
- [SPDX license headers](license-headers.md) — markdown under `docs/`
  carries no per-file header; the licence is stated once, site-wide.
- [ADR 0057](../adr/0057-documentation-system.md) — why the
  documentation is built and gated this way, including what was
  deliberately left out.
