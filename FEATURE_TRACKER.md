# AppRafter — Feature Roadmap

> **Purpose:** a living status list of user-facing features, ordered by the launch build sequence (the SR markers in `plan.md`).
> **Reading the two axes:**
> - **order N** — build sequence from the SR markers (`> 🏁 SR: … order N`). This is the order in which work happens.
> - **Checkpoint** — the milestone at which a feature first becomes demoable/usable. Build order and user-visible availability do not always coincide: e.g. `4.1`/`4.1a` are built in order 5 but appear in the Tier-1 demo (`*`).
> - The **managed (hosted) track** is not part of the `plan.md` SR markers by design — it is a parallel track and is listed in its own section.

**Status:** `☐` not started · `🚧` in progress · `✅` done
**Checkpoints:** `CP1` Tier-1 demo · `CP2` Tier-1 demo+ · `CP3` Tier-2 demo (full OSS core) · `CP4` MVP (managed) · `CP5` MVP+ (Tier-1→Tier-2 migration)

> `✅` means delivered **and** verified (a passing live walk / e2e), not merely "code committed." `🚧` means partially landed. Where a capability is delivered but a part of it (e.g. its portal surface) is deferred, the row carries a note. Committed-vs-pushed is not tracked here — the tracker ships in the same push as the code it describes.

---

## Baseline — Phase 0–1 (shipped)

The foundation everything else builds on. Closed in `plan.md` across the `v0.1.x` releases.

| Status | Feature | plan.md | Checkpoint |
|---|---|---|---|
| ✅ | CLI `apprafter`: provision + lifecycle for a Hetzner Tier-1 cluster (`init` → `apply` → `bootstrap-all` → `destroy`) | 1.1–1.3 | baseline |
| ✅ | One-command cluster bootstrap (k3s + Cilium + Gateway API + cert-manager + Argo CD + Backstage) | 1.4–1.5 | baseline |
| ✅ | Deploy applications via a CUE `Application` manifest + GitOps (Argo CD) | 1.6–1.9, 1.15 | baseline |
| ✅ | Typed config + composition (CUE + admission-webhook validation) | 1.5, 1.14 | baseline |
| ✅ | One manifest for dev/prod (per-environment expansion) | 1.9c | baseline |
| ✅ | Backstage developer portal: app status view + golden-path template (scaffold a Bun HTTP service) | 1.10–1.11 | baseline |
| ✅ | App scaffolding (`app open` / `app new`) | 1.79b | baseline |

---

## order 1 — M1.5 Track B subset

| Status | Feature | plan.md | Checkpoint |
|---|---|---|---|
| ✅ | Platform self-updates via GitOps (self-reconcile; substrate for managed updates) | 1.66–1.83 subset | CP1 |

> Self-update is live — every Phase-2 release lands on test clusters by GitOps convergence, no re-bootstrap. The M1.5 §6 milestone box still awaits the first green e2e CI run (the sandbox can't run k3d); that is bookkeeping, not feature availability.

---

## order 2 — agentic safety primitive

| Status | Feature | plan.md | Checkpoint |
|---|---|---|---|
| ✅ | MCP-native safety gate: destructive operations are paused (`MigrationPlan` CRD), approve/reject via CLI | 1.72–1.78 condensed | CP1 |

> Delivered as a full vertical: `MigrationPlan` CRD + reconciler + admission webhook + `apprafter migration list/approve/reject`. The **hosted MCP endpoint** itself is in the managed section. At launch, approval is available via the CLI and Argo CD buttons; a dedicated Backstage MigrationPlan plugin lands post-launch (PL1).

---

## order 3 — Phase-2 minimum + secrets + private repos

| Status | Feature | plan.md | Checkpoint |
|---|---|---|---|
| ✅ | On-demand Postgres (`needs.pg`, CloudNativePG) | 2.1–2.4 | CP2 |
| ✅ | On-demand Redis cache (`needs.redis`, Dragonfly) | 2.6 | CP2 |
| ✅ | On-demand block storage (`needs.disk`) | 2.6b | CP2 |
| ✅ | Reference secrets/claims directly in `Application.env` (`secret()` / `claim.*`) | 2.12 | CP2 |
| ✅ | Secrets management — SealedSecrets (encrypted secrets in Git) | 2.11 | CP2 |
| 🚧 | Deploy from private repos (`SourceCredential`: git + registry credentials from one source) | 1.79c | CP2 |

> Invisible infrastructure in order 3 (auto-`NetworkPolicy` derivation from `needs`, 2.10) is not a tracker row — it shipped alongside `needs.*` as a security default.
> Caveats: **2.11** ships the seal capability + `apprafter secret seal`; the Backstage encrypt-wizard is deferred to PL1. **1.79c** is `🚧` — S0–S4 land the vertical (CRD + controller + webhook + `apprafter repo creds`); S5 + the manual walk remain.

---

## order 4 — Tier-2 substrate

| Status | Feature | plan.md | Checkpoint |
|---|---|---|---|
| ☐ | Tier-2: 3-node HA cluster (k3s + kube-vip + embedded etcd) | 3.1 | CP3 |
| ☐ | The same manifest runs on Tier-1 and Tier-2 (tier chosen at provision time) | 3.1 + 1.9c | CP3 |
| ☐ | Workload mTLS between services (Cilium) | 3.3 | CP3 |

---

## order 5 — external surface + observability (pulled from Phase 4)

| Status | Feature | plan.md | Checkpoint |
|---|---|---|---|
| ☐ | Automatic public URL + HTTPS on the app's domain (HTTPRoute auto-generated from `Application.expose`) | 4.1a | CP1 `*` |
| ☐ | Declarative external surface (`ExternalSurface` CRD) | 4.1 | CP1 `*` |
| ☐ | Automatic DNS (external-dns + `DNSZone`) | 4.4a | CP3 |
| ☐ | Automatic backups to external S3 (default ON for Tier-1) | 4.12 | CP3 |
| ☐ | Built-in observability: metrics/traces/logs (OTel + Tempo + Prometheus/Grafana) + network flow (Hubble UI) | 3.4 + 3.7a subset | CP3 |

> `*` `4.1`/`4.1a` are built in order 5 but are included in the Tier-1 demo (CP1). Build order is 5; the checkpoint is CP1.

---

## Managed (hosted) track — parallel

Not part of the `plan.md` SR markers by design — a separate, parallel track that lands around MVP (CP4). The launch tier is **Hosted Services**: only the UI/MCP layer is hosted, while the cluster remains a standalone OSS install on the customer's own infrastructure.

| Status | Feature | plan.md | Checkpoint |
|---|---|---|---|
| ☐ | Hosted account and sign-up | — | CP4 |
| ☐ | Cluster registration via an outbound agent (`apprafter cluster register --token`) | — | CP4 |
| ☐ | Hosted Backstage portal at `<customer>.apprafter.dev` | — | CP4 |
| ☐ | Hosted MCP endpoint (`mcp.apprafter.app`): AI clients connect and are proxied to the cluster | — | CP4 |
| ☐ | Billing view in the portal | — | CP4 |
| ☐ | Cancel anytime — the cluster keeps running as a standalone OSS install (no migration required) | — | CP4 |
| ☐ | Minimal data exposure: the hosted side sees metadata only, not the application data plane | — | CP4 |

---

## Post-launch first bundle (PL1)

| Status | Feature | plan.md | Checkpoint |
|---|---|---|---|
| ☐ | Tier-1→Tier-2 migration (`apprafter migrate-to-tier --to team`) | 3.10 | CP5 |
| ☐ | MigrationPlan approval UI in Backstage (approve/reject in the portal, not only the CLI) | 4.16 | CP5 |

---

### Deferred and out of current scope

Listed so that "not done" is not confused with "not planned."

- **Deferred (added on demand):** KEDA autoscaling · SPIRE + OpenBao · kine+NATS storage · ClickHouse / VictoriaMetrics (observability depth) · Kamaji hard multi-tenancy (Tier-2 opt-in, ADR 0038) · Cilium Egress Gateway + static IPs · AccessGrant + OIDC SSO · Trivy / SBOM scanning · cost view.
- **Out of current scope:** Dev Mode (local bootstrap) · `needs.jetstream` · notifications service · self-hosted Forgejo / Harbor / GitLab · Headscale / Tailscale · Tier 3 (Talos / LINSTOR / Kata) · Tier 4 (confidential containers) · plugin ecosystem.
