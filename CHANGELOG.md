# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

**This file is generated from ATM** by the changelog sync; edit entries in ATM, not
here, or the next release will overwrite them. Anything outside a `## [version]`
section — such as this note — survives regeneration.

**Which version the heading names.** This is a monorepo with several independently
versioned streams, and their numbers are meant to differ. A heading here tracks the
**platform-stack chart**, the artifact a cluster actually consumes; the CLI, the
operator charts and the CUE CMP sidecar carry their own numbers. To see which of them
a given release moved, read that version's record in
`platform-stack/cue/compatibility.cue` — it carries the paired `operatorVersion` and
the upgrade's change class. `CLAUDE.md` § "Releases and version streams" holds the full
map. The narrative, per-phase ledger this repository kept before ATM lives on in
`docs/changelog/UNRELEASED.md` and `docs/changelog/plan-history.md`.

## [0.2.79]

### Added

- Added a project-local `.mcp.json` registering the ATM MCP server, so a fresh checkout picks up the roadmap/claims connection without per-developer setup. The bearer is an `${ATM_TOKEN}` reference, not a literal — the credential itself stays in the gitignored `.claude/settings.local.json`, and the ATM discipline hooks (`.claude/`) remain local-only by repository convention. (WI-344)

### Changed

- The pre-commit hook now runs the version and invariant gates that previously existed only in `just lint` — six version-coherence checks (operator/CLI/backup-runner/cue-cmp bumps, the cue-cmp plugin mirror, and cross-stream pin coherence) plus five cheap invariant checks. No workflow invokes `just lint`, so until now a commit could change operator code without moving `appVersion`, and `release-operator`'s two-axis gate would silently skip the rebuild so the change never shipped. Every gate is glob-scoped, so it costs nothing on a commit that cannot break it, and none can break an offline commit: each asks `git ls-remote` whether its tag is already published and treats a failed call as "bump in flight". `cargo fmt`/`clippy` and the docsgen property tests deliberately stay out of the hook. (WI-345)

### Fixed

- An application with fewer than four replicas now rolls out with `maxSurge: 0, maxUnavailable: 1` instead of the Kubernetes default. The default resolves to `maxSurge: 1, maxUnavailable: 0` at one, two and three replicas, which forces a rollout to acquire capacity before it may release any — on a node at its allocatable ceiling the replacement pod stays `Pending`, nothing is released, and the rollout deadlocks silently while the old pods keep serving. From four replicas upward the Kubernetes default already releases before it acquires and is left alone. At exactly one replica the old pod now terminates before its replacement is ready, so there is a brief window with nothing serving — accepted deliberately, since a single replica on a single node has no availability guarantee to lose. Applications mounting their own disk are unaffected: they already roll with `Recreate`. (WI-279)

### Internal

- `.gitignore` now carries the two entries the ATM Claude pack appends — `.claude/.atm/` for the hooks' per-session state and `.claude/settings.local.json` for the project token. Both are already covered by the repository-wide `.claude/` entry, so this is redundant here; it is committed only to keep a checkout from drifting away from what the onboarding produces. (WI-344)
