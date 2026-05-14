# Developer Guide

> **Status:** stub. Quickstart materials land in phase 1.11 along
> with the first golden-path Backstage Software Template.

Tasks for application developers (anyone deploying an `Application`
to AppRafter):

- **Quickstart** — clone a golden-path template, push, watch it
  deploy.
- **Authoring `Application`** — `image`, `expose`, `needs`,
  `connects`, per-environment overrides via CUE unification.
- **Working with platform services** — declaring `needs.{pg,
  jetstream, redis, clickhouse, s3, notifications}` and consuming
  generated credentials through workload identity.
- **Build pipeline** — Dockerfile + auto-analysis (CVE scan, SBOM,
  layer report) surfaced in Backstage.
- **Promotion** — `dev → staging → prod` via `apprafter promote`.
- **Notifications** — sending via the platform HTTP API with
  channel routing and DLQ.

Until each topic has its own page, the canonical references are:

- `spec.md` §3.1 (Application), §4.6 (Platform Services), §4.9
  (Build Pipeline).
- `examples/applications/parser.cue` for a minimal manifest.
