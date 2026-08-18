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
itself broke (no `cue` on `PATH`, an unreadable page, a checkout with
no tags). A **2** is a toolchain or checkout to repair, never a page.

## What is in scope

Every tracked `docs/**/*.md` plus the root `README.md`, minus four
trees, each out for its own reason:

| Out of scope | Why |
| --- | --- |
| `docs/reference/cli/**` | Generated. `docsgen check` already byte-compares it against the clap tree; two gates with contradictory remedies on one page is worse than one. |
| `docs/adr/**` | A historical record. An ADR describes the world as it was when it was ratified. |
| `docs/changelog/**` | Same — a record, not a description of today. |
| `docs/measurements/**` | Internal working data. |

`spec.md` is out by decision: it is the roadmap and deliberately names
capabilities that do not exist yet.

## What is checked

Four things, and **none of them is selected by a fence's language
tag** — obligations come from a block's content, so deleting a tag
cannot quietly turn a finding green.

| Code | What it means |
| --- | --- |
| `cli-invocation` | An `apprafter …` line — in a fence or an inline span — whose command path or flag names do not resolve against the clap tree. Values and required-ness are **not** checked: a documented command is a reference, not a runnable line. |
| `schema-identifier` | A backticked field path (`spec.…`, `base.…`, `expose.…`, `needs.…`, `Kind.spec.…`) that no shipped schema declares, or one naming a `needs` type no provider ships. Runs page-wide — prose, tables and fences alike. |
| `cue-document` | A fence that is a complete CUE manifest (a `package` clause *and* the schema import) which `cue vet` rejects. Fragments are out of scope. |
| `unlabelled-fence` | A fence with no info string. Also `unterminated-fence`, for one that never closes. |

Prefer the **kind-prefixed** form when you write a field path:
`PlatformStack.spec.pin` resolves against `PlatformStack` alone, while
a bare `spec.pin` is resolved against the union of every AppRafter
kind and can therefore pass for the wrong reason.

The other codes are about exemptions themselves: `docs-marker`
(malformed marker), `front-matter` (malformed exemption entry),
`exemption-expired`, `exemption-unaged`, `exemption-unused`.

Every finding prints its own remedy. Read it — they differ by class.

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

## Front matter: exempting an inline span or a field path

An inline span carries no marker — a comment mid-sentence is
unreadable as prose and unreviewable as an exemption. Spans and field
paths are exempted at page level, in the page's YAML front matter, by
their **literal text**:

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
---
```

Matching is **exact equality on the trimmed text, never substring**:
the `span:` entry above covers that command and only that command, not
the same line with a `--dry-run` appended, which is a different claim.
An exemption that matches nothing is itself a finding
(`exemption-unused`) — once the page is fixed, the entry is a claim
about a problem that no longer exists.

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
  CLI source, `docsgen`, or the schemas the checks resolve against.
- `.github/workflows/docs.yml`, on the same file set.

A schema or flag rename is a documentation change even when no page is
touched, which is why the last two entries in each list are there.
