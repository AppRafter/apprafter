# Changelog

All notable changes to this project are documented in this file.

Sections follow [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and versions
follow [Semantic Versioning](https://semver.org/spec/v2.0.0.html). **Headings do not**,
deliberately — see below.

**This file is generated from ATM** by the changelog sync; edit entries in ATM, not
here, or the next release will overwrite them. Anything outside a version section —
such as this note — survives regeneration.

**What a heading names.** Keep a Changelog assumes one artifact with one number. This
is a monorepo: a release moves some subset of independently versioned streams, and
their numbers are meant to differ. So a heading names **the streams that actually
moved**, which is this repository's own long-standing convention:

The examples below are indented one space on purpose. Both ATM's changelog sync
and `release-cli.yml` find releases by scanning for `##` at the start of a line,
and neither knows what a fenced code block is — flush-left examples here become
four releases, and a tag search answers with an illustration instead of its
notes. Verified: before the indent, a lookup for `v0.2.75` returned the first
line below.

```
 ## cli v0.2.75 — …                              a CLI-only release
 ## platform-stack 0.2.77 / cli v0.2.74 — …      both moved
 ## platform-stack 0.2.69 / operator v0.2.49 — … no CLI change, so no CLI tag
```

A CLI-only release carries no chart number because no chart was cut — the most common
shape in this repository's history, not an edge case. The CLI is written with its `v`,
identically to its git tag, which is what lets `release-cli.yml` find a release's notes
by tag alone. Only two of the numbers are chosen by a human — the chart train and the
CLI; the operator and cue-cmp versions cannot move without forcing a chart bump.
`CLAUDE.md` § "Releases and version streams" holds the full map, and each chart
release's record in `platform-stack/cue/compatibility.cue` carries its paired
`operatorVersion` and change class.

Everything from before ATM is frozen in `docs/changelog/history.md` — 237 sections of
already-shipped work, kept because `release-cli.yml` still reads it for tags cut before
the switch.

## platform-stack 0.2.79 / operator v0.2.51 — 2026-09-22

### Added

- Added a project-local `.mcp.json` registering the ATM MCP server, so a fresh checkout picks up the roadmap/claims connection without per-developer setup. The bearer is an `${ATM_TOKEN}` reference, not a literal — the credential itself stays in the gitignored `.claude/settings.local.json`, and the ATM discipline hooks (`.claude/`) remain local-only by repository convention. (WI-344)

### Changed

- The pre-commit hook now runs the version and invariant gates that previously existed only in `just lint` — six version-coherence checks (operator/CLI/backup-runner/cue-cmp bumps, the cue-cmp plugin mirror, and cross-stream pin coherence) plus five cheap invariant checks. No workflow invokes `just lint`, so until now a commit could change operator code without moving `appVersion`, and `release-operator`'s two-axis gate would silently skip the rebuild so the change never shipped. Every gate is glob-scoped, so it costs nothing on a commit that cannot break it, and none can break an offline commit: each asks `git ls-remote` whether its tag is already published and treats a failed call as "bump in flight". `cargo fmt`/`clippy` and the docsgen property tests deliberately stay out of the hook. (WI-345)
- `CHANGELOG.md` at the repository root is now the canonical changelog, tracked and self-describing. The root markdown allowlist in `.gitignore` had no entry for it, so the file ATM's changelog sync creates server-side would have been invisible in every local checkout. Its header now records what a heading names: this is a monorepo, so a heading lists **the streams a release actually moved** (`## cli v0.2.75 — …`, `## platform-stack 0.2.77 / cli v0.2.74 — …`), which is the repository's own long-standing convention and the reason `release-cli.yml` can find a release's notes by git tag alone. Only two of the four hand-edited numbers are actually chosen — the chart train and the CLI; the operator and cue-cmp versions cannot move without forcing a chart bump. That note lives outside any version section so regenerating a release does not wipe it. Everything from before ATM is frozen in `docs/changelog/history.md`, renamed from `UNRELEASED.md` because 237 of its sections describe already-shipped work; `docs/changelog/plan-history.md` is deleted, its role taken over by ATM. (WI-346)

### Fixed

- An application with fewer than four replicas now rolls out with `maxSurge: 0, maxUnavailable: 1` instead of the Kubernetes default. The default resolves to `maxSurge: 1, maxUnavailable: 0` at one, two and three replicas, which forces a rollout to acquire capacity before it may release any — on a node at its allocatable ceiling the replacement pod stays `Pending`, nothing is released, and the rollout deadlocks silently while the old pods keep serving. From four replicas upward the Kubernetes default already releases before it acquires and is left alone. At exactly one replica the old pod now terminates before its replacement is ready, so there is a brief window with nothing serving — accepted deliberately, since a single replica on a single node has no availability guarantee to lose. Applications mounting their own disk are unaffected: they already roll with `Recreate`. (WI-279)
- `release-cli.yml` no longer publishes a release with the wrong notes, or with none. Two defects, both found by reading rather than by a failure — and both permanent when they bite, because the workflow runs once per tag and never regenerates an older release. (1) The section lookup was a substring test, so tag `v0.2.5` matched inside the heading `## cli v0.2.51 — …`; verified reproducible, and hidden until now only because the file is newest-first so the intended section happened to come first. The match is now anchored on both sides, the tag's dots are escaped so they cannot act as regex wildcards, and only the part of the heading before the dash is searched — prose after it mentions other versions. (2) A missing section fell back to a stub body and exited 0, publishing a green release that describes nothing; it now fails loudly. The lookup reads the canonical root `CHANGELOG.md` first and falls back to `docs/changelog/history.md` for tags cut before the switch. (WI-347)
- `release-cli.yml` is parseable again, and a new `scripts/check-workflows.sh` gate (actionlint, wired into `just lint` and pre-commit) keeps it that way. A shell comment inside a `run:` block had contained empty GitHub Actions expression delimiters: Actions expands those before the shell sees the script and does not know the line is a comment, so an empty expression made the whole workflow unparseable. The failure mode is why this needs a gate rather than a fix — an unparseable workflow fails as a run with **zero jobs**, which `gh pr checks` does not list, so every pull-request check stays green while a release workflow is dead until the day it should have run. (WI-348)

### Internal

- `.gitignore` now carries the two entries the ATM Claude pack appends — `.claude/.atm/` for the hooks' per-session state and `.claude/settings.local.json` for the project token. Both are already covered by the repository-wide `.claude/` entry, so this is redundant here; it is committed only to keep a checkout from drifting away from what the onboarding produces. (WI-344)
