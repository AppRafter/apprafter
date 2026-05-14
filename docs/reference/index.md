# Reference

Field-by-field reference for everything the platform exposes:

- **`apprafter` CLI** — [`cli.md`](cli.md) covers every
  subcommand, every flag, every alias, plus the global env var
  surface. Authoritative source remains
  `cli/platform-cli/src/cli.rs`; this page is the
  human-readable view.
- **CRDs** — `Application`, `ServiceProvider`, `ResourceClaim`,
  `AccessGrant`, `ExternalSurface`, `MigrationPlan`,
  `Infrastructure`, `ServiceProviderPlugin`,
  `InfrastructureProviderPlugin`. Source of truth:
  `schemas/v1alpha1/` (CUE). Auto-generated field reference lands
  in phase 8.1.
- **Notifications HTTP API** — request/response shapes, error
  codes, idempotency keys. Stub today; lands with the M3
  notification stack.
- **gRPC plugin contracts** — `ServiceProviderInterface`,
  `InfrastructureProviderInterface`. Stub today; lands in
  Phase 7 (plugin sidecar lifecycle).

For the immediate operator-facing surface, [`cli.md`](cli.md) is
canonical. Cross-reference with `apprafter <subcmd> --help` for
shell-formatted output, and
[`operator-guide/troubleshooting.md`](../operator-guide/troubleshooting.md)
for the diagnostic-code catalogue.
