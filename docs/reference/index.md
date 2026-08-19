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
- **Custom resources** — the objects the platform installs into a
  cluster, [listed below](#custom-resources). Source of truth:
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

## Custom resources

The operator's Helm chart installs each of these, which is what puts
them in a running cluster. Some are written by the platform rather
than by a person: you read those, you do not author them.

| Object | Who writes it | What it is for |
| ------ | ------------- | -------------- |
| `Application` | Developer | The unit of deployment — one image, its environment, and what it needs. |
| `ServiceProvider` | Operator | A backend implementation that a declared need can resolve to. |
| `SourceCredential` | Operator | One private git or registry credential. It holds no secret material itself: the material stays sealed and is referenced. |
| `SharedVolume` | Operator | A persistent volume that several applications mount at once. |
| `PlatformStack` | Operator | The one object per cluster that pins the platform version and its cluster-wide settings. |
| `ResourceClaim` | The platform | One application's claim on one backing service, generated from a declared need. |
| `RetainedClaim` | The platform | The snapshot taken when a claim is deleted, so what stood behind it can still be recovered. |
| `MigrationPlan` | The platform | The approval gate a destructive change waits behind. |

Other schemas under `schemas/v1alpha1/` describe objects the platform
does **not** install — `AccessGrant`, `ExternalSurface`,
`ServiceProviderPlugin` and `InfrastructureProviderPlugin`. No CRD
ships for any of them, so they cannot be created in a cluster today.

`Infrastructure` is a schema that is deliberately not a cluster object
at all: it describes the substrate the platform runs on, and
`apprafter` reads it from disk rather than from the cluster.
`APPRAFTER_MANIFEST` names the path — see
[environment variables](environment.md).
