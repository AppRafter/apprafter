---
description: "What reference material is published today, what is still only source of truth in the repository, and which surface is canonical for each question."
---

# Reference

Field-by-field reference for everything the platform exposes:

- **`apprafter` CLI** — [`cli/`](cli/index.md) covers every published
  subcommand, every flag, every default and every alias. It is
  **generated** from `cli/platform-cli/src/cli.rs` by `docsgen` and
  byte-compared in CI, so it cannot fall behind the binary. The same
  tree ships as data in
  [`cli/commands.json`](cli/commands.json), which additionally
  carries the commands hidden from `--help` and the CLI version it
  was generated from.
- **Environment variables** — [`environment.md`](environment.md).
  Only four of them are flag fallbacks declared through clap, so the
  generated CLI reference cannot carry the rest; this page is
  hand-written and names the call site for each.
- **CRDs** — `Application`, `ServiceProvider`, `ResourceClaim`,
  `AccessGrant`, `ExternalSurface`, `MigrationPlan`,
  `Infrastructure`, `ServiceProviderPlugin`,
  `InfrastructureProviderPlugin`. Source of truth:
  `schemas/v1alpha1/` (CUE). A generated field reference is not
  yet published.
- **Notifications HTTP API** — request/response shapes, error
  codes, idempotency keys. Not yet published.
- **gRPC plugin contracts** — `ServiceProviderInterface`,
  `InfrastructureProviderInterface`. Not yet published.

For the immediate operator-facing surface, [`cli/`](cli/index.md) is
canonical. Cross-reference with `apprafter <subcmd> --help` for
shell-formatted output, and
[`operator-guide/troubleshooting.md`](../operator-guide/troubleshooting.md)
for the diagnostic-code catalogue.

Do not hand-edit anything under `docs/reference/cli/` — `docsgen
check` byte-compares it against the clap tree and will reject the
change. Run `just docsgen-generate` instead, or, for the authored
paragraphs on its index, edit the constants in
`cli/docsgen/src/render.rs` and regenerate.
