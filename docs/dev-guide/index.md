---
description: "Map of the developer-facing tasks — scaffolding, manifest authoring, environments, sizing, secrets, platform services, private repositories and image iteration — and where each is documented."
---

# Developer Guide

Tasks for application developers (anyone deploying an `Application`
to AppRafter). If you own the machine and the platform on it rather
than an application, the [Operator
Guide](../operator-guide/index.md) is the page you want.

## Get something running

- [Quickstart](quickstart.md) — scaffold a service, register it with
  `apprafter app add`, and watch it deploy. Start here.

## Describe your application

The manifest is one CUE file in your repository. These pages cover it
field by field and then the three things most people reach for while
filling it in.

- [Writing Application.cue](application-cue.md) — the field reference:
  `image`, `expose`, `resources`, `needs`, `env`, and the shape of a
  per-environment override.
- [Deploying more than one environment](environments.md) — running one
  manifest as a staging and a production deployment, how an override
  merges onto the base, which commands need to be told which one you
  mean, and the cluster's default environment.
- [Resources and autoscaling](resources-and-autoscaling.md) — what your
  container asks for when you set nothing, how to set an explicit
  request and limit, and the in-place right-sizing that runs on your
  behalf until you do.
- [Secrets](secrets.md) — sealing a value with `apprafter secret seal`
  and binding it to an env-var with a `secret:` reference. The
  namespace you seal into decides whether it works.

## Give it a dependency

- **Platform services** — declaring `needs.{pg, redis, disk}` and
  binding the provisioned credentials into env-vars with `claim`
  references (ADR 0046). The provisioning guides live in the operator
  guide: [Postgres](../operator-guide/postgres.md),
  [Redis](../operator-guide/redis.md),
  [persistent disk](../operator-guide/persistent-disk.md).
- [Egress](../operator-guide/egress-policy.md) — a declared `need` is
  also what opens your pods' network path to that backend; an
  undeclared reach is denied.

## Ship it, and keep shipping

- [Private repos and registries](private-repos-and-registries.md) —
  source credentials for a private Git repository and image-pull
  secrets for a private registry.
- [Image iteration](image-iteration.md) — pushing a moved tag and
  having the cluster resolve and roll out the new digest, and rolling a
  bad deploy back with `apprafter app rollback`.

## Canonical references

- [The CLI reference](../reference/cli/index.md) — every
  subcommand + flag.
- [`schemas/v1alpha1/application.cue`](https://github.com/apprafter/apprafter/blob/master/schemas/v1alpha1/application.cue)
  — the authoritative `Application` field list.
- [ADR 0046](../adr/0046-env-value-references.md) — how env values
  reference claim fields and sealed secrets.
- [Reference](../reference/index.md) — the generated CLI pages, the
  environment variables, and where each CRD's field list lives.
- [ADR index](../adr/README.md) — the decision behind each behaviour.
  An ADR describes the world as it was when it was ratified, so read
  it for *why*, and the pages above for *what ships*.
