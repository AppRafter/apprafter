# ADR 0030: CLI target store and credential resolution chain

## Status

`Accepted`

Date: 2026-05-14.

## Context

Pre-Track-A (`v0.1.68` and earlier) the `apprafter` binary
sourced cluster credentials exclusively from environment
variables (`HCLOUD_TOKEN`, `APPRAFTER_SSH_PUBLIC_KEY`). Single-
target operators got a working but awkward UX: every shell
session had to re-export the token, sessions on different
clusters required separate shells, accidental leak surfaces
were the operator's `.bashrc`/`.zshrc` and `printenv` output,
and there was no CLI-level concept of "the cluster I'm
currently operating on".

User-facing errors went through `color-eyre` (the default
`anyhow`-style rendering), which produced helpful coloured
backtraces but no stable error codes, no multi-line help
text, and no signposting to next-step CLI commands. Walks
surfaced this repeatedly as a friction point: operators
either had to remember the right rotation command or grep
prior shell history.

The CLI itself only had verbose, two-word subcommand names
(`apprafter cluster-bootstrap`, `apprafter bootstrap-all`,
`apprafter argocd-password`). Muscle-memory was poor for
operators arriving from `kubectl` / `git` / similar tools
where short aliases are standard (`kubectl get` not `kubectl
list-resources`).

The M1.5 self-managing platform rethink (`spec.md` rev. 6,
`plan.md` Phase 1.5 Track A) committed to closing the
operator-UX gap before opening Track B (platform-stack
rethink). The work landed across `v0.1.69`–`v0.1.89` as 11
sub-versions plus a handful of walk-found hotfixes.

This ADR closes Track A by codifying the four design
decisions that survived the iteration cycle, so future
contributors can understand the WHY behind the current CLI
shape.

## Decision

We commit to four concrete CLI mechanisms, each immutable
except via a follow-up ADR:

### D1. Target store as the durable source of truth

A **target** is a named bundle of `(provider, region,
credentials, defaults)`. The target store lives under
`$XDG_CONFIG_HOME/apprafter/` (Linux default
`~/.config/apprafter/`) with this layout:

```text
$XDG_CONFIG_HOME/apprafter/
├── config.yaml                      # GlobalConfig (active_target + schema version)
├── targets/
│   ├── default/
│   │   ├── config.yaml              # non-secret: provider, region, tier, cluster_name, ssh_key_path
│   │   └── credentials.yaml         # secret: hetzner_token. Mode 0600.
│   └── work/
│       ├── config.yaml
│       └── credentials.yaml
├── auth/                            # reserved for `apprafter auth` (AppRafter Cloud Managed; stub today)
│   └── .keep
└── state/
    └── <target>/                    # per-target runtime cache (kubeconfig, argocd password). Reserved.
```

`credentials.yaml` is plain YAML with mode 0600 enforced by
the CLI on every write. Encryption is OUT OF SCOPE for the
target store itself (file-system perms are the boundary;
operators who need stronger isolation use a separate UNIX
user or a hardened secret manager and feed env vars in).

Exactly one target is active at a time (`config.yaml`'s
`active_target` field). `apprafter target use <name>`
switches it; `apprafter target add` auto-activates the first
target created on a fresh store.

`APPRAFTER_CONFIG_DIR` overrides the root (used verbatim, no
`apprafter/` suffix appended). The env var exists primarily
for integration tests pointing at a `tempfile::TempDir`, but
power users can also redirect their store for
compartmentalised experimentation.

### D2. Three-step credential resolution chain

Every operational command (`apply`, `destroy`, `import`,
`kubeconfig --refresh`, `bootstrap-all` Phase 2) resolves
credentials in this order:

1. **`--flag` value** (e.g. `--token "$T"`). Wins everything.
2. **Environment variable** (`HCLOUD_TOKEN`,
   `APPRAFTER_SSH_PUBLIC_KEY`). Wins over the store.
3. **Active target's `credentials.yaml`** (or `--target
   <name>` override). The default for shell sessions where
   nothing else is set.

When nothing resolves, the error message enumerates **all
three** paths so the operator sees every available way out.
The chain is the single point of truth: every command goes
through `cli_core::resolve_hetzner_token(flag, paths,
target_override)` and `cli_core::resolve_hetzner_ssh_public_key(...)`.
Test isolation is by `APPRAFTER_CONFIG_DIR` (target store
root) + a crate-local `TEST_ENV_MUTEX` (serialises env-var
flipping across parallel test threads).

`apprafter init` stays available as a legacy one-shot but
is no longer mandatory after `target add`. State-file
fallback (`<cwd>/.apprafter/state.json`) covers the legacy
flow; the target store covers the new flow; in-flight values
(provider, region, cluster_name) on `apply` resolve from
state-file FIRST, target-store SECOND, defaults LAST. The
order ensures hand-edited state.json files keep being
authoritative.

### D3. `miette` for user-facing diagnostics

Every user-facing `CliError` variant derives `miette::Diagnostic`:

- **Stable error code** of the form `apprafter::<area>::<reason>`
  (e.g. `apprafter::target::not_found`,
  `apprafter::provider::hetzner_api_error`,
  `apprafter::target::token_rejected`). Codes are part of the
  public surface — log-analytics pipelines / external monitoring
  may group on them across releases.
- **Multi-line `help(...)` text** describing root cause + the
  next-step CLI command(s) to fix it.
- **`#[diagnostic_source]` cause chains** for layered errors
  (Track A.10's typed token-rejection wraps the underlying
  Hetzner API error so miette renders both layers — the
  rotation-specific outer help PLUS the provider-specific
  inner help). Cause-chain source must be
  `Box<dyn miette::Diagnostic + Send + Sync + 'static>` — `Box<CliError>`
  alone fails the `Borrow<dyn Diagnostic>` trait bound.

The binary's `main` installs `miette::set_hook` with the
`fancy` reporter (terminal links, Unicode glyphs, 2 context
lines, cause chain). `color-eyre` is removed from the
workspace. `NO_COLOR=1` (or non-TTY stdout) strips all ANSI.

New call sites should prefer adding a typed variant with its
own code over `CliError::Other(format!(…))` — the catch-all
still exists for one-off messages but has the stable code
`apprafter::cli::other` so recurring `Other` messages
surface as promotion candidates in logs.

### D4. Subcommand aliases + semantic colour palette

Aliases are added at the `clap` level via `#[command(alias =
"…")]`. Canonical names stay primary; aliases exist for
muscle-memory typing:

| Canonical               | Alias  |
| ----------------------- | ------ |
| `target`                | `t`    |
| `target list`           | `ls`   |
| `target show`           | `info` |
| `target remove`         | `rm`   |
| `kubeconfig`            | `kc`   |
| `cluster-bootstrap`     | `cb`   |
| `bootstrap-all`         | `up`   |

Aliases chain naturally (`apprafter t ls` = `apprafter
target list`).

Colour is centralised in `cli_core::style` over `owo-colors
4.x` with the `supports-colors` feature. Six semantic
helpers — `ok` (green), `warn` (yellow), `fail` (red),
`info` (cyan), `dim` (dimmed), `bold` — all auto-honour
`NO_COLOR` + non-TTY pipes (each format call evaluates the
stream lazily). Callsites emit intent, not concrete colours.
A future rebrand flips a single file.

## Consequences

### Easier

- New operators run one command (`apprafter target add …`)
  and forget about `HCLOUD_TOKEN` env vars; the first target
  auto-activates, subsequent commands "just work".
- Multi-cluster operators carry a single shell session and
  `apprafter target use prod` between deploys.
- CI keeps the env-var workflow with zero changes — step 2
  in the chain handles it identically to v0.1.68 behaviour.
- Operator-facing failures carry `apprafter::<area>::<reason>`
  codes, so support tickets / runbooks can reference exact
  diagnostic codes instead of fuzzy log substrings.
- Walks repeatedly surfaced that the rendered `help:` line
  IS the documentation operators read; that's now a
  first-class output channel, not a footnote.
- Aliases close the kubectl-style ergonomic gap. `apprafter
  up --dry-run` is the new fastest-thing-to-type sanity
  check.

### Harder

- The target store adds a new on-disk surface (mode 0600,
  filesystem-level isolation). Operators sharing a UNIX user
  must understand that `~/.config/apprafter/targets/*/credentials.yaml`
  contains plaintext tokens — encryption-at-rest is OUT OF
  SCOPE here (use a hardened secret manager + env vars if you
  need it).
- Every new typed error variant is a public-surface
  commitment: the `code(apprafter::…)` namespace must stay
  stable across patch releases. Renaming a variant requires
  a deprecation cycle (new code added, old code kept aliasing
  for ≥1 minor, then removed in a `minor` bump).
- `miette` derive trips `unused_assignments` lint on
  generated code in current versions; file-level
  `#![allow(unused_assignments)]` in `cli_core/src/error.rs`
  is the workaround. Local `#[allow]` doesn't propagate
  through derive macros.
- Aliases multiply the public CLI surface. Each alias has to
  stay live until at least the next `minor` (Phase 2 / `v0.2.x`)
  to avoid breaking operator muscle memory.

### Neutral

- `init` stays in the surface (it's a useful one-shot for
  scripted setups even though it's no longer mandatory).
- `auth` exists as a hidden `Subcommand` stub for the future
  Managed AppRafter Cloud offering — present in the type
  system, redirected to `target add` at the user level.
- Backwards-compat env-var workflows survive Phase 2 onward;
  no deprecation timeline scheduled.

## Alternatives considered

### A1. Keep env-only credentials (no target store)

Rejected. The bug-bears were not about the env-var mechanism
itself — they were about the lack of a "current cluster"
concept. A store solves both single-cluster ergonomics and
multi-cluster switching with one mechanism.

### A2. Single-file target store (TOML / single YAML)

Rejected. The per-target directory layout makes mode-0600
enforcement trivial (one chmod per credentials file) and
lets a future `apprafter target rm <name>` be a single
`rm -rf` of one subdir. A single file would have required
in-place rewriting of every credential change.

### A3. Encrypt `credentials.yaml` with `age` / OpenBao

Rejected for v0.1.x. The kubeconfig + Argo CD password are
already age-encrypted in state.json (different threat
model — those land on every operator laptop that ran
`apprafter kubeconfig`). The TOKEN is shorter-lived and
filesystem-perm protection matches the security profile of
`~/.kube/config` and `~/.ssh/id_*`. Operators with stricter
needs can pipe `HCLOUD_TOKEN` from a hardened secret manager
and the chain's env-var step honours it.

### A4. Hand-rolled error UX (no miette, no color-eyre)

Rejected. miette is the de-facto standard for rustc-style
CLI errors in Rust 2026, ships diagnostic codes / help
text / source spans / cause chains out of the box, and has
a clear migration path back to `anyhow` if it goes
unmaintained. The cost of writing equivalent UX by hand was
~3× the cost of adopting miette.

### A5. Mass-promote every `CliError::Other(format!(...))` to typed variants

Deferred (not rejected). v0.1.86+ kept `Other` as a
catch-all with a stable `apprafter::cli::other` code; future
walks will surface which messages recur often enough to
warrant a typed variant. The promotion cadence is
walk-driven, not exhaustive — typing every one-off message
adds maintenance cost without UX benefit.

### A6. Coloured output without `NO_COLOR` opt-out

Rejected outright. Every CI / scripted consumer expects
ANSI-free pipes; the `supports-colors` crate's
auto-detection is the standard contract.

## Risks

### R1. Token-on-disk leak surface

`credentials.yaml` is mode 0600 but operators may copy it,
include it in dotfiles backups, or share via screen capture.

Mitigation: `apprafter target show` redacts tokens (prints
`set / not set`, never the value); `apprafter target add
--renew` rewrites the file atomically without ever printing
the new value; `apprafter doctor` warns on mode != 0600.
The walk-found rule is "credentials file SHOULD NOT be in
your `~/.dotfiles` repo" — documented in the new
`docs/operator-guide/target-store.md`.

### R2. Diagnostic-code stability

Adding a new variant later may collide with an existing
code path's grep. Mitigation: code names are namespaced
(`apprafter::<area>::<reason>`) and follow snake_case; new
codes go in a new `<reason>` slot rather than redefining an
existing one.

### R3. Alias collisions with future canonical commands

`ls`, `rm`, `info`, `kc`, `cb`, `up` are not currently used
as canonical subcommand names. A future canonical command
named `up` would collide. Mitigation: when introducing a new
canonical command, check the alias map first; if collision
exists, pick a different canonical or rename the alias with
a deprecation cycle.

### R4. `miette-derive` lint workaround drift

`#![allow(unused_assignments)]` at file scope in
`cli_core/src/error.rs` may mask real bugs in error.rs body
edits. Mitigation: error.rs body is intentionally tiny —
just `#[error]` / `#[diagnostic]` annotations on enum
variants, no logic. Any logic that creeps in goes in a
sibling module (`error_helpers.rs` or similar) without the
file-scope allow.

## Owner

Andrey Ryahovskiy (`remryahirev@gmail.com`).

## Re-evaluation

Re-evaluate when:

- The second infrastructure provider (AWS) lands. The
  current design assumes per-target provider; a multi-
  provider target shape would force re-thinking
  `TargetConfig.provider: String`.
- Phase 2 (M2) opens. The target store gains AWS / OpenBao
  / Managed Cloud credential shapes;
  `TargetCredentials.hetzner_token: Option<String>` will
  generalise to a discriminated union.
- A walk surfaces a real `credentials.yaml` leak. Then
  encryption-at-rest moves from out-of-scope to a follow-up
  ADR.

## References

- `cli-dx-task.md` — full Track A specification (12 sub-items).
- `plan.md` Phase 1.5 Track A history table (sub-phases
  1.66A.1 through 1.66A.12, versions v0.1.69 through v0.1.90).
- `docs/changelog/UNRELEASED.md` — per-version changelog
  with operator-facing rationale for each step.
- `docs/operator-guide/quickstart.md` — operator quickstart
  refreshed for the post-Track-A flow.
- `docs/operator-guide/target-store.md` — file-layout
  reference for the target store.
- `docs/operator-guide/troubleshooting.md` — diagnostic
  code catalogue.
- `docs/reference/cli.md` — full subcommand reference.
- `cli/cli-core/src/target.rs`, `cli/cli-core/src/credentials.rs`,
  `cli/cli-core/src/error.rs`, `cli/cli-core/src/style.rs`
  — load-bearing implementation modules.
- `cli/platform-cli/src/cli.rs` — clap definitions including
  alias attributes.
