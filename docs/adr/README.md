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

| #    | Title                                                                                                      | Status   |
|------|------------------------------------------------------------------------------------------------------------| -------- |
| 0000 | [Template](./0000-template.md)                                                                             | Template |
| 0001 | [FSL-1.1-MIT for the platform core, MIT for plugins](./0001-license-fsl-1-1-mit.md)                        | Accepted |
| 0002 | [Project codename "AppRafter"](./0002-codename-apprafter.md)                                               | Accepted |
| 0003 | [Custom Rust operator instead of Crossplane](./0003-rust-operator-over-crossplane.md)                      | Accepted |
| 0004 | [CUE as the configuration language](./0004-cue-over-pkl.md)                                                | Accepted |
| 0005 | [kine + NATS JetStream as control-plane storage](./0005-kine-nats-over-etcd.md)                            | Accepted |
| 0006 | [OpenBao as the secrets backend (Tier 2+)](./0006-openbao-over-vault.md)                                   | Accepted |
| 0007 | [SealedSecrets at Tier 1, OpenBao at Tier 2+](./0007-tier-1-sealedsecrets-tier-2-openbao.md)               | Accepted |
| 0008 | [HTTP-first notifications API](./0008-http-first-notifications-api.md)                                     | Accepted |
| 0009 | [Platform-only notification templates](./0009-platform-only-templates.md)                                  | Accepted |
| 0010 | [Dockerfile-first build pipeline](./0010-dockerfile-first-build.md)                                        | Accepted |
| 0011 | [Hybrid native-SDK + OpenTofu-shim infrastructure providers](./0011-hybrid-rust-sdk-tofu-shim.md)          | Superseded by ADR 0016 |
| 0012 | [MigrationPlan as a first-class concept](./0012-migrationplan-as-first-class.md)                           | Accepted |
| 0013 | [(Unused)](./0013-unused.md)                                                                               | Unused   |
| 0014 | [Why AppRafter, not Cozystack](./0014-why-apprafter-not-cozystack.md)                                      | Accepted |
| 0015 | [Tier 4 confidential stack — orthogonal opt-in](./0015-tier-4-confidential-orthogonal.md)                  | Accepted |
| 0016 | [Hetzner+AWS exclusive infra providers in v1](./0016-hetzner-aws-exclusive-providers.md)                   | Accepted |
| 0017 | [IPv6 strategy — dual-stack everywhere, no NAT64 in v1](./0017-ipv6-dual-stack.md)                         | Accepted |
| 0018 | [(Unused)](./0018-unused.md)                                                                               | Unused   |
| 0019 | [KEDA as official autoscaling backend in v1](./0019-keda-autoscaling.md)                                   | Accepted |
| 0020 | [Hubble defaults + staged UI strategy](./0020-hubble-defaults-staged-ui.md)                                | Accepted |
| 0021 | [Karpenter deferred to Phase 6.2 AWS native](./0021-karpenter-deferred.md)                                 | Accepted |
| 0022 | [Tier model clarification](./0022-tier-model-clarification.md)                                             | Accepted |
| 0023 | [Kamaji as hard multi-tenancy + Capsule policy layer](./0023-kamaji-multi-tenancy.md)                      | Accepted |
| 0024 | [Cluster-admin constrain bundle](./0024-cluster-admin-constrain.md)                                        | Accepted |
| 0025 | [GitOps control surface via in-cluster Argo CD Applications](./0025-gitops-control-surface.md)             | Draft    |
| 0026 | [PlatformStack CRD as platform version control plane](./0026-platformstack-crd.md)                         | Draft    |
| 0027 | [MigrationPlan unification with scope discriminator](./0027-migrationplan-unification.md)                  | Draft    |
| 0028 | [Platform-stack distribution — CUE source, dual-channel publishing](./0028-platform-stack-distribution.md) | Draft    |
| 0029 | [CUE compilation for user app repositories via Argo CD CMP](./0029-cue-cmp.md)                             | Draft    |
