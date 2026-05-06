# Reference

> **Status:** stub. Auto-generated CRD reference and `platform-cli`
> reference land in phase 8.1 / 8.2.

Field-by-field reference for everything the platform exposes:

- **CRDs** — `Application`, `ServiceProvider`, `ResourceClaim`,
  `AccessGrant`, `ExternalSurface`, `MigrationPlan`,
  `Infrastructure`, `ServiceProviderPlugin`,
  `InfrastructureProviderPlugin`. Generated from CUE schemas in
  `schemas/v1alpha1/`.
- **`platform-cli`** — every subcommand, every flag.
- **Notifications HTTP API** — request/response shapes, error codes,
  idempotency keys.
- **gRPC plugin contracts** — `ServiceProviderInterface`,
  `InfrastructureProviderInterface`.

Until the auto-generated reference exists, the source of truth is
`schemas/v1alpha1/` (CUE) and the `platform-cli --help` output.
