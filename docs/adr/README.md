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

- `Proposed` — raised for discussion; the decision is not yet made.
- `Draft` — the decision is made and written, but not yet ratified.
- `Accepted` — ratified; implementations should follow it.
- `Deprecated` — no longer applies, but kept for history.
- `Superseded by ADR NNNN` — replaced by a newer decision.
- `Template` — the ADR template itself, not a decision.
- `Unused` — a reserved number that was never used; kept to preserve sequential numbering.

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
| 0030 | [CLI target store and credential resolution chain](./0030-cli-target-store-and-credential-chain.md)        | Accepted |
| 0031 | [`apprafter-agent` ↔ hosted-bus protocol — gRPC streaming with Rust agent](./0031-apprafter-agent-protocol.md) | Accepted |
| 0032 | [Migrate core license base from FSL-1.1-MIT to FSL-1.1-Apache-2.0](./0032-license-fsl-1-1-apache-2-0.md)   | Accepted |
| 0033 | [Tenant security configuration — strictMode and confidential as orthogonal switches](./0033-tenant-security-configuration.md) | Accepted |
| 0034 | [Managed offering model and terminology — hosted-management layer, hardware tier vs managed plan](./0034-managed-offering-model.md) | Accepted |
| 0035 | [Minimal Data Exposure — managed services see metadata only, never customer data](./0035-minimal-data-exposure.md) | Accepted |
| 0036 | [MCP server and agentic-safety model — structural enforcement at the platform boundary](./0036-mcp-agentic-safety.md) | Accepted |
| 0037 | [Managed control plane infrastructure — dogfooded on AppRafter, rescue-cluster recovery](./0037-managed-control-plane-infrastructure.md) | Accepted |
| 0038 | [Tier 2 default is an HA substrate only; hard multi-tenancy via Kamaji is opt-in](./0038-tier2-kamaji-opt-in.md) | Accepted |
| 0039 | [SourceCredential — per-repo image-registry credential binding](./0039-source-credential.md) | Accepted |
| 0040 | [Image tag-to-digest resolution and automatic rollout](./0040-image-digest-resolution.md) | Accepted |
| 0041 | [Channel-tag resolver — O(1) fast path for channel-latest version resolution](./0041-channel-tag-version-resolution.md) | Accepted |
| 0042 | [needs.redis → Dragonfly, per-database isolation, acl_reconcile](./0042-needs-redis-dragonfly.md) | Accepted |
| 0043 | [needs.disk → persistent block storage, named multi-claim `needs` format](./0043-needs-disk-named-claims.md) | Accepted |
| 0044 | [Per-environment deploy via a deploy-time, per-Application env selector](./0044-per-environment-deploy.md) | Accepted |
| 0045 | [needs → CiliumNetworkPolicy egress auto-derivation, config-driven cluster egress profile](./0045-needs-networkpolicy-egress.md) | Accepted |
| 0046 | [`Application.env` value references — claim and secret sources](./0046-env-value-references.md) | Accepted |
| 0047 | [CRD codegen — CUE as the single source; generated CRD, gated Rust, typed webhook](./0047-crd-codegen-from-cue.md) | Accepted |
| 0048 | [Argo CD platform-upgrade approval surface](./0048-argo-platform-upgrade-approval-surface.md) | Accepted |
| 0049 | [cross-app SharedVolume](./0049-cross-app-sharedvolume.md) | Accepted |
| 0050 | [backup, export, and restore — restic engine, local-pull default](./0050-backup-restore.md) | Accepted |
| 0051 | [application-scope destructive-change detection and gating](./0051-app-scope-migration.md) | Accepted |
| 0052 | [application-migration security axis — additive/escalation gating and structural hardening](./0052-migration-security-axis.md) | Accepted |
| 0053 | [resource governance — QoS classes, node reservations, app-seed Burstable, backends Guaranteed](./0053-resource-governance.md) | Accepted |
| 0054 | [Vertical autoscaling of application requests via VPA (InPlace)](./0054-vpa-vertical-autoscaling.md) | Accepted |
| 0055 | [node swap policy — provisioned host swap + pod NoSwap, Tier-1](./0055-node-swap-policy.md) | Accepted |
| 0056 | [machine-picker — live (region × SKU) matrix and no implicit server-type default](./0056-machine-picker.md) | Accepted |
| 0057 | [documentation system — MkDocs-Material retained, generated CLI reference, content-detected drift gate](./0057-documentation-system.md) | Accepted |
