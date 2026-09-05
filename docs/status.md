---
description: "Every user-facing feature, its status, and the phase it lands in."
# This page lists roadmap features by name, so it cites commands and a
# schema type that do not ship today — the drift gate resolves those
# against what ships and (correctly) finds nothing. Exempted here with
# `since=` set to the current release so the entry ages out and forces
# its own removal the day the feature lands and its row flips ✅.
cli-check-ignore:
  - span: "apprafter cluster register --token"
    reason: known-broken
    since: v0.2.61
    note: roadmap row (Managed track, ☐) — the hosted cluster-register command is not built yet
  - span: "apprafter migrate-to-tier --to team"
    reason: known-broken
    since: v0.2.61
    note: roadmap row (PL1, ☐) — the Tier-1→Tier-2 migration command is not built yet
schema-check-ignore:
  - path: "needs.jetstream"
    reason: known-broken
    since: v0.2.61
    note: out-of-scope list — the schema declares this need but no provider ships it
---

# Feature status

> **Purpose:** a living status list of user-facing features, ordered by the launch build sequence (the SR markers in `plan.md`).
> **Reading the two axes:**
> - **order N** — build sequence from the SR markers (`> 🏁 SR: … order N`). This is the order in which work happens.
> - **Checkpoint** — the milestone at which a feature first becomes demoable/usable. Build order and user-visible availability do not always coincide: e.g. `4.1`/`4.1a` are built in order 5 but appear in the Tier-1 demo (`*`).
> - The **managed (hosted) track** is not part of the `plan.md` SR markers by design — it is a parallel track and is listed in its own section.

**Status:** `☐` not started · `🚧` in progress · `✅` done
**Checkpoints:** `CP1` Tier-1 demo · `CP2` Tier-1 demo+ · `CP3` Tier-2 demo (full OSS core) · `CP4` MVP (managed) · `CP5` MVP+ (Tier-1→Tier-2 migration)

> `✅` means delivered **and** verified (a passing live walk / e2e), not merely "code committed." `🚧` means partially landed. Where a capability is delivered but a part of it (e.g. its portal surface) is deferred, the row carries a note. Committed-vs-pushed is not tracked here — the tracker ships in the same push as the code it describes.

> **Two indexes, one file.** The leading **Phase** column is the public product phase each feature lands in (the same phase names the landing roadmap and the subscribe form use). `order`/`CP` are the internal build-sequence columns this file is sectioned around.

---

## Baseline — Phase 0–1 (shipped)

The foundation everything else builds on. Closed in `plan.md` across the `v0.1.x` releases.

| Phase | Status | Feature | plan.md | order/CP |
|---|---|---|---|---|
| Shipped | ✅ | CLI `apprafter`: provision + lifecycle for a Hetzner Tier-1 cluster (`init` → `apply` → `up` → `destroy`) | 1.1–1.3 | baseline |
| Shipped | ✅ | One-command cluster bootstrap (k3s + Cilium + Gateway API + cert-manager + Argo CD + Backstage) | 1.4–1.5 | baseline |
| Shipped | ✅ | Deploy applications via a CUE `Application` manifest + GitOps (Argo CD), including tag→digest auto-deploy and rollback | 1.6–1.9, 1.15, 2.4h, 2.22e | baseline |
| Shipped | ✅ | Typed config + composition (CUE + admission-webhook validation) | 1.5, 1.14 | baseline |
| Shipped | ✅ | One manifest for dev/prod (per-environment expansion) | 1.9c | baseline |
| Shipped | 🚧 | Backstage developer portal: app status view + golden-path template (scaffold a Bun HTTP service) | 1.10–1.11 | baseline |
| Shipped | ✅ | App scaffolding (`app open` / `app new`) | 1.79b | baseline |

> **Backstage 🚧, corrected 2026-09-05.** The portal deploys as an opt-in
> component of the platform-stack chart, and the golden-path template is in
> the repository — but there is no supported path to it: `apprafter open`
> handles Argo CD only and says so, and the portal work (the encrypt wizard,
> the MigrationPlan plugin) is deferred to the post-launch bundle. The row
> read `✅` while the CLI reference read "not wired up yet"; one of the two
> had to move, and this is the one that was ahead of the product.

---

## order 1 — M1.5 Track B subset

| Phase | Status | Feature | plan.md | order/CP |
|---|---|---|---|---|
| Shipped | ✅ | Platform self-updates via GitOps (self-reconcile; substrate for managed updates) | 1.66–1.83 subset | order 1 · CP1 |

> Self-update is live — every Phase-2 release lands on test clusters by GitOps convergence, no re-bootstrap. The M1.5 §6 milestone box still awaits the first green e2e CI run (the sandbox can't run k3d); that is bookkeeping, not feature availability.

---

## order 2 — agentic safety primitive

| Phase | Status | Feature | plan.md | order/CP |
|---|---|---|---|---|
| Shipped | ✅ | MCP-native safety gate: destructive operations are paused (`MigrationPlan` CRD), approve/reject via CLI | 1.72–1.78 condensed, 2.16b, 2.16b-sec | order 2 · CP1 |

> Delivered as a full vertical: the `MigrationPlan` CRD, its reconciler, the admission webhook, and `apprafter migration list/approve/reject`. At launch, approval is available from the CLI and from Argo CD; a dedicated Backstage plugin lands post-launch. **App-scope** detection (ADR 0051) gates a destructive edit to a user `Application` — removing a `needs.*`, scale-to-zero, an image-repository, domain or network change, an env-reference removal — by creating a plan in the app's own namespace and pausing the app until it is approved; softer edits emit an Event instead, and a reject is a Git revert. The **security axis** (ADR 0052) extends the gate to edits that *add or escalate* rather than remove: a `security-boundary` class above `data-migration`, covering `secret:` reference changes, going public, gaining a public hostname, retargeting a public port, and relaxing `imagePolicy.resolve`. Approval is bound to a content hash, so a plan re-gates if the spec drifts under it. The full trigger table is on [Migration plans](operator-guide/migration-plans.md); the release history is in the changelog.

---

## order 3 — Phase-2 minimum + secrets + private repos

| Phase | Status | Feature | plan.md | order/CP |
|---|---|---|---|---|
| Shipped | ✅ | On-demand Postgres (`needs.pg`, CloudNativePG) | 2.1–2.4 | order 3 · CP2 |
| Shipped | ✅ | On-demand Redis cache (`needs.redis`, Dragonfly) | 2.6 | order 3 · CP2 |
| Shipped | ✅ | On-demand block storage (`needs.disk`) | 2.6b | order 3 · CP2 |
| Shipped | ✅ | Cross-app shared volumes (`SharedVolume` + `needs.disk.ref`, `apprafter volume`) + disk capacity-signal | 2.6c | order 3 · CP2 |
| Shipped | ✅ | Data export + backup/restore (`apprafter export` / `backup` / `restore`, restic engine, local-pull default) | 2.6d | order 3 · CP2 |
| Shipped | ✅ | Reference secrets/claims directly in `Application.env` (`secret()` / `claim.*`) | 2.12 | order 3 · CP2 |
| Shipped | ✅ | Secrets management — SealedSecrets (encrypted secrets in Git) | 2.11 | order 3 · CP2 |
| Shipped | ✅ | Deploy from private repos (`SourceCredential`: git + registry credentials from one source) | 1.79c | order 3 · CP2 |

> Invisible infrastructure in order 3 (auto-`NetworkPolicy` derivation from `needs`, 2.10) is not a tracker row — it shipped alongside `needs.*` as a security default.
> **Secrets** ship the seal capability and `apprafter secret seal`; the portal encrypt-wizard is post-launch. **`SourceCredential`** is live end to end: narrowing a credential's coverage while an application depends on it gates the change and pauses both derived Secrets, leaving the wider ones in place so in-flight applications keep cloning and pulling. **Shared volumes** carry the cross-application path — two applications referencing one volume, the mount surviving a rolling update, `volume rm` refused while referenced; the disk capacity signal is verified on real hardware. **Backup and restore** are verified on real Hetzner in every mode, including a rebuild from nothing (`restore --reprovision`) and a scheduled off-site push to S3, and a `persistent: true` redis claim travels with them as a whole-instance snapshot. Ephemeral caches stay out by declaration. What each command captures, and what it does not, is on [Back up a cluster](operator-guide/backup-restore.md).

---

## order 3.7 — Tier-1 substrate hardening (pull-ups, pre-launch)

| Phase | Status | Feature | plan.md | order/CP |
|---|---|---|---|---|
| Shipped | ✅ | Live `(region × SKU)` machine-picker matrix — explicit server-type choice at provision time (`apprafter target machine`, `--server-type`, `APPRAFTER_SERVER_TYPE`; no implicit `cpx22` default) | 2.16h + 2.16h-a | order 3.7 · CP1 |

> **2.16h/2.16h-a:** released **cli v0.2.43** (monorepo tag `v0.2.43`, CLI-only). Real-Hetzner walk **GREEN 2026-08-12** (`e2e/machine-picker-walk.sh`, non-interactive legs: no-type→error + 0 resources, explicit-type→provisions the exact SKU + records the fact, legacy self-heal backfill, `target machine` refused on a provisioned cluster; swept to zero). Interactive matrix table confirmed live by the owner; the manual-acceptance pass drove three UX corrections (`target machine` refuses on a provisioned cluster → backup + `restore --reprovision`, dead deferred-intent guard removed, stale `run target machine` hint dropped).
>
> The machine picker (`2.16h`) and the no-implicit-default breaking change (`2.16h-a` / Decision 0) ship together in one CLI release. BREAKING: `apprafter up` / `apply` / `restore --reprovision` without an explicit type on the create path now errors `apprafter::provider::server_type_not_selected`. **Migration:** existing clusters self-heal on the first `apply` after upgrade (type backfilled from the live server). Fresh targets need a type via `apprafter target machine`, `--server-type`, `APPRAFTER_SERVER_TYPE`, or `nodes[0].kind` in the manifest. See ADR 0056.

---

## order 3.7b — documentation as a product surface

| Phase | Status | Feature | plan.md | order/CP |
|---|---|---|---|---|
| Shipped | ✅ | Public documentation site (`docs.apprafter.dev`) — generated CLI reference + guides, kept true to the code by a drift gate in `just lint` and CI | 2.19 | order 3.7b · CP1 |
| Shipped | ✅ | Presentation-walk program — phase registry + PhaseChip across landing/docs/README; id-derived roadmap anchors (no `phase-phase`); published `/status/` feature page with Phase column; per-phase subscribe control; SYS-3 content gate (live-smoke + schema-test + Payload validate); Tier-A point fixes (operator-quickstart CTA, product-path README, absolute doc links, MVP wording, `llms-guides.txt`, version footer, ADR-index relabel) | 2.19 (order-3.7b) | order 3.7b · CP1 |

> Both rows flipped `✅` on 2026-09-05: `https://docs.apprafter.dev/` serves, and the phase registry, roadmap anchors and per-phase subscribe controls are live on the landing. The build history is in ADR 0057 and the changelog.

---

## order 3.9 — day-2 operability

The 2.20/2.22 program ran as defect fixes rather than as tracked subphases, so
nothing in the phase loop forced a row for any of it. One row per user-visible
capability, sourced from `docs/changelog/plan-history.md`.

| Phase | Status | Feature | plan.md | order/CP |
|---|---|---|---|---|
| Shipped | ✅ | A failing reconcile is visible without reading the operator log — `status.recentProblems[]` and the yellow lines `apprafter app status` prints from it, deduplicated on reason and self-ageing | 2.22h | order 3.9 · CP1 |
| Shipped | ✅ | `apprafter status` — the cluster roll-up: target, platform version and available upgrade, unhealthy conditions, applications reporting problems, applications held at a digest, MigrationPlans awaiting approval. Degrades to labelled lines rather than failing when the cluster is unreachable | 2.23a | order 3.9 · CP1 |
| Shipped | ✅ | Sealed-secret disclosure — `apprafter secret list` names what is sealed where, and `apprafter secret remove` retires it | 2.22c | order 3.9 · CP1 |
| Shipped | ✅ | The operator deletes the children an Application no longer declares, so removing a `needs.*` releases its claim onto the documented seven-day retention path | 2.22b | order 3.9 · CP1 |
| Shipped | ✅ | Node disk-pressure signal — sampled from the kubelet, surfaced as a condition and a CLI banner, with recovery | 2.22d | order 3.9 · CP1 |
| Shipped | ✅ | Digest-pinned rollback — `apprafter app rollback` returns to the previously resolved image and holds the application there until `apprafter app unpin` | 2.22e | order 3.9 · CP1 |
| Shipped | ✅ | Dragonfly ACL durability — a restart no longer drops every claim's credential | 2.22f | order 3.9 · CP1 |
| Shipped | ✅ | Backup schedule surface — `apprafter backup enable` takes the schedule, the retention and the integrity check as flags rather than a cron string | 2.22g | order 3.9 · CP1 |
| Shipped | ✅ | A one-line installer that verifies its own checksum (`apprafter.dev/install.sh`), and a download page. The documented install command previously resolved the version through an endpoint that returns the newest release across all five of this monorepo's tag series | 2.23b | order 3.9 · CP1 |
| Shipped | ✅ | CI reports which pinned external components are behind, and — where a gate can honestly fail — whether the candidate version still passes it | 2.23g | order 3.9 · CP1 |

---

## order 4 — Tier-2 substrate

| Phase | Status | Feature | plan.md | order/CP |
|---|---|---|---|---|
| Phase 3 | ☐ | Tier-2: 3-node HA cluster (k3s + kube-vip + embedded etcd) | 3.1 | order 4 · CP3 |
| Phase 3 | ☐ | The same manifest runs on Tier-1 and Tier-2 (tier chosen at provision time) | 3.1 + 1.9c | order 4 · CP3 |
| Phase 3 | ☐ | Workload mTLS between services (Cilium) | 3.3 | order 4 · CP3 |

> The landing roadmap surfaces these as **Phase 3 — Production multi-node + observability** (the public-facing name for order 4 + the observability slice of order 5); Tier 2 is presented as roadmap/waitlist, not available, until these ship.

---

## order 5 — external surface + observability (pulled from Phase 4)

| Phase | Status | Feature | plan.md | order/CP |
|---|---|---|---|---|
| Shipped | ✅ | Automatic public URL + HTTPS on the app's domain (HTTPRoute auto-generated from `Application.expose`) | 4.1a | order 5 · CP1 `*` |
| Phase 3 | ☐ | Declarative external surface (`ExternalSurface` CRD) | 4.1 | order 5 · CP1 `*` |
| Phase 3 | ☐ | Automatic DNS (external-dns + `DNSZone`) | 4.4a | order 5 · CP3 |
| Shipped | ✅ | Automatic backups to external S3 (opt-in scheduled off-site push; AppRafter CronJob-restic, `PlatformStack.spec.backup`) | 2.6d-4 / 4.12 | order 5 · CP3 |
| Phase 3 | ☐ | Built-in observability: metrics/traces/logs (OTel + Tempo + Prometheus/Grafana) + network flow (Hubble UI) | 3.4 + 3.7a subset | order 5 · CP3 |

> `*` `4.1`/`4.1a` are built in order 5 but are included in the Tier-1 demo (CP1). Build order is 5; the checkpoint is CP1.
> The opt-in **Cloudflare origin firewall** (1.83d — `Infrastructure.spec.firewall.cloudflareOrigin`, restricts the node's 80/443 to Cloudflare IP ranges so an orange-cloud proxy isn't bypassable via the node IP) is not a tracker row — like the auto-`NetworkPolicy` note above it is a security-posture hardening of the public-URL capability, shipped opt-in alongside the ingress path, not a standalone launch feature.

---

## Managed (hosted) track — parallel

Not part of the `plan.md` SR markers by design — a separate, parallel track that lands around MVP (CP4). The launch tier is **Hosted Services**: only the UI/MCP layer is hosted, while the cluster remains a standalone OSS install on the customer's own infrastructure.

| Phase | Status | Feature | plan.md | order/CP |
|---|---|---|---|---|
| Phase 4 | ☐ | Hosted account and sign-up | — | CP4 |
| Phase 4 | ☐ | Cluster registration via an outbound agent (`apprafter cluster register --token`) | — | CP4 |
| Phase 4 | ☐ | Hosted Backstage portal at `<customer>.apprafter.dev` | — | CP4 |
| Phase 4 | ☐ | Hosted MCP endpoint (`mcp.apprafter.app`): AI clients connect and are proxied to the cluster | — | CP4 |
| Phase 4 | ☐ | Billing view in the portal | — | CP4 |
| Phase 4 | ☐ | Cancel anytime — the cluster keeps running as a standalone OSS install (no migration required) | — | CP4 |
| Phase 4 | ☐ | Minimal data exposure: the hosted side sees metadata only, not the application data plane | — | CP4 |

---

## Post-launch first bundle (PL1)

| Phase | Status | Feature | plan.md | order/CP |
|---|---|---|---|---|
| Post-launch | ☐ | Tier-1→Tier-2 migration (`apprafter migrate-to-tier --to team`) | 3.10 | CP5 |
| Post-launch | ☐ | MigrationPlan approval UI in Backstage (approve/reject in the portal, not only the CLI) | 4.16 | CP5 |

---

### Deferred and out of current scope

Listed so that "not done" is not confused with "not planned."

- **Deferred (added on demand):** KEDA autoscaling · SPIRE + OpenBao · kine+NATS storage · ClickHouse / VictoriaMetrics (observability depth) · Kamaji hard multi-tenancy (Tier-2 opt-in, ADR 0038) · Cilium Egress Gateway + static IPs · AccessGrant + OIDC SSO · Trivy / SBOM scanning · cost view.
- **Out of current scope:** Dev Mode (local bootstrap) · `needs.jetstream` · notifications service · self-hosted Forgejo / Harbor / GitLab · Headscale / Tailscale · Tier 3 (Talos / LINSTOR / Kata) · Tier 4 (confidential containers) · plugin ecosystem.
