---
description: "Map of the developer-facing tasks — scaffolding, manifest authoring, platform services, private repositories, image iteration — and where each is documented."
---

# Developer Guide

Tasks for application developers (anyone deploying an `Application`
to AppRafter):

- **Quickstart** — scaffold a service, register it with
  `apprafter app add`, and watch it deploy. See
  [Quickstart](quickstart.md).
- **Authoring `Application`** — `image`, `expose`, `resources`,
  `needs`, and per-environment overrides via `spec.environments`.
  See [Writing Application.cue](application-cue.md).
- **Working with platform services** — declaring `needs.{pg, redis,
  disk}` and binding the provisioned credentials into env-vars with
  `claim` references (ADR 0046). The provisioning guides live in the
  operator guide: [Postgres](../operator-guide/postgres.md),
  [Redis](../operator-guide/redis.md),
  [persistent disk](../operator-guide/persistent-disk.md).
- **Private repos and registries** — source credentials for a private
  Git repository and image-pull secrets for a private registry. See
  [Private repos and registries](private-repos-and-registries.md).
- **Iterating on an image** — pushing a moved tag and having the
  cluster resolve and roll out the new digest. See
  [Image iteration](image-iteration.md).
- **Egress** — a declared `need` is also what opens your pods' network
  path to that backend; an undeclared reach is denied. See the
  [egress guide](../operator-guide/egress-policy.md).

Canonical references:

- [The CLI reference](../reference/cli/index.md) — every
  subcommand + flag.
- [`schemas/v1alpha1/application.cue`](https://github.com/apprafter/apprafter/blob/master/schemas/v1alpha1/application.cue)
  — the authoritative `Application` field list.
- [`examples/applications/parser.cue`](https://github.com/apprafter/apprafter/blob/master/examples/applications/parser.cue)
  — a worked multi-environment manifest.
- [ADR 0046](../adr/0046-env-value-references.md) — how env values
  reference claim fields and sealed secrets.
- [Reference](../reference/index.md) — the generated CLI pages, the
  environment variables, and where each CRD's field list lives.
- [ADR index](../adr/README.md) — the decision behind each behaviour.
  An ADR describes the world as it was when it was ratified, so read
  it for *why*, and the pages above for *what ships*.
