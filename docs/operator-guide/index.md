# Operator Guide

> **Status:** Track A (M1.5 Phase 1.5) CLI rework is closed —
> CLI quickstart, target-store reference, troubleshooting
> catalogue, and per-tier guidance are live. Full handbook for
> tiers 2+ and disaster recovery lands incrementally as Phase 3
> / Phase 4 features stabilise.

Tasks for operators (anyone running an AppRafter cluster):

- **Provisioning** — `apprafter target add` (recommended) +
  `apprafter bootstrap-all`. See
  [`quickstart.md`](quickstart.md) for the tier-1 walkthrough.
  The legacy `apprafter init` one-shot stays available for
  scripted setups.
- **Target store** — multi-target setups, credential rotation,
  the resolution chain. See
  [`target-store.md`](target-store.md).
- **Troubleshooting** — diagnostic-code catalogue + the
  walk-found common failures. See
  [`troubleshooting.md`](troubleshooting.md).
- **Tier upgrades** — `apprafter upgrade-tier 1 → 2 → 3 → 4`,
  with safety semantics from `MigrationPlan`. Stub today;
  M3 target.
- **External surface** — wiring git, registry, OIDC SSO,
  Headscale/Tailscale, synthetic monitoring, backups.
- **Day-2** — debugging with k9s / Headlamp / Hubble; reading
  audit logs from JetStream; managing AccessGrants.
- **Recovery from a wedged VM** — see
  [`recovery.md`](recovery.md) for the Hetzner Rescue Mode
  runbook (the VM is key-only and the Hetzner web console is
  unusable for emergency access; rescue mode + chroot is the
  documented escape hatch).
- **Disaster recovery** — restoring from `s3://`-backups,
  cluster rebuild, `DisasterRecoveryPlan` runbooks.

Canonical references:

- [`quickstart.md`](quickstart.md) — tier-1 walkthrough.
- [`target-store.md`](target-store.md) — file layout + credential
  resolution chain.
- [`troubleshooting.md`](troubleshooting.md) — diagnostic codes.
- [`gitops-walk.md`](gitops-walk.md) — Argo CD + repo-creds walk.
- [`needs-pg-walk.md`](needs-pg-walk.md) — `needs.pg` Postgres
  provisioning + GC walk.
- [`recovery.md`](recovery.md) — Hetzner rescue-mode runbook.
- [`docs/reference/cli/`](../reference/cli/index.md) — every
  subcommand + flag.
- [ADR 0030](../adr/0030-cli-target-store-and-credential-chain.md)
  — Track A design rationale.
- [Reference](../reference/index.md) — the generated CLI pages, the
  environment variables, and where each CRD's field list lives.
- [ADR index](../adr/README.md) — the decision behind each behaviour.
  An ADR describes the world as it was when it was ratified, so read
  it for *why*, and the pages above for *what ships*.
