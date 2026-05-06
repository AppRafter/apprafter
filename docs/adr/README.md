# docs/adr/

Architecture Decision Records — numbered documents (`NNNN-title.md`)
capturing non-trivial architectural decisions, their context, and
consequences. Use [`0000-template.md`](./0000-template.md) as the
starting point for new ADRs.

## Naming

`NNNN-kebab-case-slug.md`, where `NNNN` is the next free four-digit
number. Numbers are assigned sequentially and never reused, even if
an ADR is later superseded.

## Statuses

- `Proposed` — under discussion, not yet ratified.
- `Accepted` — ratified; implementations should follow it.
- `Deprecated` — no longer applies, but kept for history.
- `Superseded by ADR NNNN` — replaced by a newer decision.

ADRs are never deleted; supersession preserves the historical record.

## Index

| #    | Title                                                                                           | Status   |
| ---- | ----------------------------------------------------------------------------------------------- | -------- |
| 0000 | [Template](./0000-template.md)                                                                  | Template |
| 0001 | [FSL-1.1-MIT for the platform core, MIT for plugins](./0001-license-fsl-1-1-mit.md)             | Accepted |
| 0002 | [Project codename "AppRafter"](./0002-codename-apprafter.md)                                    | Accepted |
| 0003 | [Custom Rust operator instead of Crossplane](./0003-rust-operator-over-crossplane.md)           | Accepted |
| 0004 | [CUE as the configuration language](./0004-cue-over-pkl.md)                                     | Accepted |
| 0005 | [kine + NATS JetStream as control-plane storage](./0005-kine-nats-over-etcd.md)                 | Accepted |
| 0006 | [OpenBao as the secrets backend (Tier 2+)](./0006-openbao-over-vault.md)                        | Accepted |
| 0007 | [SealedSecrets at Tier 1, OpenBao at Tier 2+](./0007-tier-1-sealedsecrets-tier-2-openbao.md)    | Accepted |
| 0008 | [HTTP-first notifications API](./0008-http-first-notifications-api.md)                          | Accepted |
| 0009 | [Platform-only notification templates](./0009-platform-only-templates.md)                       | Accepted |
| 0010 | [Dockerfile-first build pipeline](./0010-dockerfile-first-build.md)                             | Accepted |
| 0011 | [Hybrid native-SDK + OpenTofu-shim infrastructure providers](./0011-hybrid-rust-sdk-tofu-shim.md) | Accepted |
| 0012 | [MigrationPlan as a first-class concept](./0012-migrationplan-as-first-class.md)                | Accepted |
