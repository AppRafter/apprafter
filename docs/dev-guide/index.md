# Developer Guide

Tasks for application developers (anyone deploying an `Application`
to AppRafter):

- **Quickstart** — scaffold a service, register it with
  `apprafter app add`, and watch it deploy. See
  [`quickstart.md`](quickstart.md).
- **Authoring `Application`** — `image`, `expose`, `resources`,
  `needs`, and per-environment overrides via `spec.environments`.
  See [`application-cue.md`](application-cue.md).
- **Working with platform services** — declaring `needs.{pg, redis,
  disk}` and binding the provisioned credentials into env-vars with
  `claim` references (ADR 0046). The provisioning walks live in the
  operator guide: [needs.pg](../operator-guide/needs-pg-walk.md),
  [needs.redis](../operator-guide/needs-redis-walk.md),
  [needs.disk](../operator-guide/needs-disk-walk.md).
- **Private repos and registries** — source credentials for a private
  Git repository and image-pull secrets for a private registry. See
  [`private-repos-and-registries.md`](private-repos-and-registries.md).
- **Iterating on an image** — pushing a moved tag and having the
  cluster resolve and roll out the new digest. See
  [`image-iteration.md`](image-iteration.md).
- **Egress** — a declared `need` is also what opens your pods' network
  path to that backend; an undeclared reach is denied. See the
  [needs.networkpolicy walk](../operator-guide/needs-networkpolicy-walk.md).

Canonical references:

- [`docs/reference/cli/`](../reference/cli/index.md) — every
  subcommand + flag.
- [`schemas/v1alpha1/application.cue`](https://github.com/apprafter/apprafter/blob/master/schemas/v1alpha1/application.cue)
  — the authoritative `Application` field list.
- [`examples/applications/parser.cue`](https://github.com/apprafter/apprafter/blob/master/examples/applications/parser.cue)
  — a worked multi-environment manifest.
- [ADR 0046](../adr/0046-env-value-references.md) — how env values
  reference claim fields and sealed secrets.
- `spec.md` §3.1 (Application), §4.6 (Platform Services).
