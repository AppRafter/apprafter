# Operator Guide

> **Status:** stub. Full operator handbook lands incrementally as
> Phase 1, Phase 3, and Phase 4 features stabilise; consolidated in
> phase 8.2.

Tasks for operators (anyone running an AppRafter cluster):

- **Provisioning** — `platform-cli init` per tier (Hetzner Cloud,
  Hetzner Robot, AWS, OpenTofu plugins).
- **Tier upgrades** — `platform-cli upgrade-tier 1 → 2 → 3 → 4`,
  with safety semantics from `MigrationPlan`.
- **External surface** — wiring git, registry, OIDC SSO,
  Headscale/Tailscale, synthetic monitoring, backups.
- **Day-2** — debugging with k9s / Headlamp / Hubble; reading
  audit logs from JetStream; managing AccessGrants.
- **Disaster recovery** — restoring from `s3://`-backups,
  cluster rebuild, `DisasterRecoveryPlan` runbooks.

Until each topic has its own page, the canonical references are:

- `spec.md` §4.7 (External Surface), §4.8 (Access Plane), §4.12
  (`platform-cli`).
- `plan.md` for the implementation order.
