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
- **Environment variables** — [Environment variables](environment.md).
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

## Custom resources

The operator's Helm chart installs each of these, which is what puts
them in a running cluster. Some are written by the platform rather
than by a person: you read those, you do not author them. The "who
writes it" column names the command or the component that creates
each one, so it can be checked rather than believed.

| Object | Who writes it | What it is for |
| ------ | ------------- | -------------- |
| `Application` | You, as a developer — the CUE manifest in your repository, rendered into the cluster by Argo CD | The unit of deployment — one image, its environment, and what it needs. |
| `ServiceProvider` | The platform — the platform chart seeds `pg-integrated`, `redis-integrated`, `disk-local` and `shared-local`. No command creates one. | A backend implementation that a declared need can resolve to. |
| `SourceCredential` | You, as a cluster operator — `apprafter repo creds add` | One private git or registry credential. It holds no secret material itself: the material stays sealed and is referenced. |
| `SharedVolume` | You, as a cluster operator — `apprafter volume create` | A persistent volume that several applications mount at once. |
| `PlatformStack` | The platform creates it during `apprafter cluster-bootstrap`; you edit it through `apprafter platform` | The one object per cluster that pins the platform version and its cluster-wide settings. |
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
