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
    since: v0.2.50
    note: roadmap row (Managed track, ☐) — the hosted cluster-register command is not built yet
  - span: "apprafter migrate-to-tier --to team"
    reason: known-broken
    since: v0.2.50
    note: roadmap row (PL1, ☐) — the Tier-1→Tier-2 migration command is not built yet
schema-check-ignore:
  - path: "needs.jetstream"
    reason: known-broken
    since: v0.2.50
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
| Shipped | ✅ | CLI `apprafter`: provision + lifecycle for a Hetzner Tier-1 cluster (`init` → `apply` → `bootstrap-all` → `destroy`) | 1.1–1.3 | baseline |
| Shipped | ✅ | One-command cluster bootstrap (k3s + Cilium + Gateway API + cert-manager + Argo CD + Backstage) | 1.4–1.5 | baseline |
| Shipped | ✅ | Deploy applications via a CUE `Application` manifest + GitOps (Argo CD), including tag→digest auto-deploy and rollback | 1.6–1.9, 1.15, 2.4h, 2.22e | baseline |
| Shipped | ✅ | Typed config + composition (CUE + admission-webhook validation) | 1.5, 1.14 | baseline |
| Shipped | ✅ | One manifest for dev/prod (per-environment expansion) | 1.9c | baseline |
| Shipped | ✅ | Backstage developer portal: app status view + golden-path template (scaffold a Bun HTTP service) | 1.10–1.11 | baseline |
| Shipped | ✅ | App scaffolding (`app open` / `app new`) | 1.79b | baseline |

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

> Delivered as a full vertical: `MigrationPlan` CRD + reconciler + admission webhook + `apprafter migration list/approve/reject`. The **hosted MCP endpoint** itself is in the managed section. At launch, approval is available via the CLI and Argo CD buttons; a dedicated Backstage MigrationPlan plugin lands post-launch (PL1). **2.16b (ADR 0051)** turns on **app-scope** auto-detection: a destructive edit to a user `Application` (removing `needs.*`, scale-to-zero, image-repository/domain/network change, env-reference removal) auto-creates an app-scope `MigrationPlan` in the app's own namespace and pauses the app until approved; soft edits emit a `SoftDestructiveChange` Event instead; reject is via Git revert. Validated by a GREEN two-env kind+Argo e2e walk. **2.16b-sec (ADR 0052)** extends the gate along the **security axis** — the inverted threat model, where an actor with manifest write access *adds and escalates* rather than removes: a new `security-boundary` class (severity above `data-migration`) now gates additive/escalation edits (`secret:` env-ref add / downgrade / retarget, `expose.network` escalation to public, public-hostname add, public-port retarget, `imagePolicy.resolve` relaxation; `image-path-change` reclassified to `security-boundary`), and the plan carries a full `classifications[]`/`changes[]` rollup so a dangerous op can't be laundered behind a benign primary. Hardened against disarming: approval is bound to a content hash (`spec.trigger.approvedSpecHash`, re-gates on drift), `Application.status` is write-protected to the operator's SSA field manager, and `spec.environment` is immutable on UPDATE.

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
> Caveats: **2.11** ships the seal capability + `apprafter secret seal`; the Backstage encrypt-wizard is deferred to PL1. **1.79c** is `✅` — S0–S4 landed the vertical (CRD + controller + webhook + `apprafter repo creds`), and the S5 acceptance #4 live-wiring shipped as **2.16b-sc**: a destructive coverage-narrowing of a `SourceCredential` (removing a `git.repoPrefixes` / `registry.hosts` entry while a matched app depends on it) now auto-creates a `sourcecredential`-scope `MigrationPlan` in the cred's namespace, pauses BOTH derived-Secret derivations (old wider-coverage Secrets stay in place so in-flight apps keep access), and resumes only on approve (`apprafter migration approve` / Argo node) — actor-agnostic (raw `kubectl edit` trips it too). Walk-verified GREEN on kind+podman (`e2e/sourcecredential-migration-walk.sh`, incl. the kubectl-edit variant); released op/webhook v0.2.36 / cue-cmp v0.1.17 / platform-stack 0.2.45 / cli v0.2.36. **2.6c** — the cross-app SharedVolume path (two apps ref one volume, rolling-update mount survival, `volume rm` refused while referenced) is walk-verified GREEN on kind+podman; the disk **capacity-signal** Warning/condition live-read (kubelet `nodes/stats`) is best-effort; SOFT-skipped on kind but **live-validated GREEN on real Hetzner** (2026-07-16, by a since-removed capacity-kubelet probe walk: kubelet `node.fs` present, `node_free_fraction≈0.887`. That probe was a diagnostic asking whether a real k3s kubelet reports `node.fs` at all; it was deleted on 2026-09-04 once the same question was answered again by `e2e/node-disk-pressure-hetzner.sh`, which asserts the whole signal chain — sample, condition, CLI banner, recovery — rather than merely observing that the field exists). Cross-ns / multi-node shared volumes + intra-app `shareMode: shared` remain T2. **2.6d** is ✅ **verified**: `apprafter export`, `backup` (encrypted restic repo, local-pull default), and `restore` in ALL modes — `--into <fresh-cluster>` + `--data-only` (kind+podman two-cluster walk, run twice) AND `--reprovision` (mode a, clone-to-new — provision a fresh cluster as part of restore) **live-validated end-to-end on real Hetzner** (2026-07-16: provision → backup → destroy → `restore --reprovision` → fresh box, data + secret intact; `e2e/restore-reprovision-hetzner.sh`), which also serves the full-DR (`restore` in <1h) drill. The 2.6d follow-on **automated S3 push** (scheduled off-site backup to a remote bucket) **shipped as 2.6d-4** (opt-in AppRafter CronJob-restic on `PlatformStack.spec.backup`; cli/runner v0.2.33 / operator v0.2.33 / platform-stack 0.2.42 / cue-cmp v0.1.14 / runner image `apprafter-backup:v0.2.33`; 0.2.40+0.2.41 yanked). **Both file AND S3 backup/restore validated GREEN on real Hetzner** — the S3 path took two live-walk fix rounds (1 release-coordination miss + 7 runner bugs — RBAC verb/resource gaps + a singular-`secret` resolution + stderr visibility — all of which passed unit/CRD/review; the full backup wrote a snapshot to Hetzner Object Storage + the restic repo restored cleanly). Row above is `✅` — the confirmation walk on the **published** 0.2.42 ran GREEN end-to-end on real Hetzner (2026-07-17: provision on 0.2.42 → seal scoped Secret → `backup enable` → CronJob backup Job Completed → snapshot in Hetzner Object Storage → `check`+`prune` → `destroy` → `restore --reprovision` from S3 into a fresh box → data + re-sealed secret intact; zero server leak). The scoped-creds security model (V2 branch (a) + V7) is verified on Hetzner OS (see backup-restore.md). local-pull stays the default. **T12 (persistent-redis backup+restore) — SHIPPED 2026-08-28 (cli v0.2.51):** `export`/`backup` capture a `persistent: true` `needs.redis` claim as a whole-instance Dragonfly snapshot and `restore` (`--into` + `--data-only`) live-loads it back via `DFLY LOAD` (no scale, so the claim provisioner never re-provisions/FLUSHes it); `e2e/backup-restore-walk.sh` GREEN on both restore paths. This closes the redis leg the 2.19j walk flagged (the backup table promised a snapshot no code produced). Ephemeral (`persistent: false`) caches stay out by declaration.

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
| Shipped | 🚧 | Public documentation site (`docs.apprafter.dev`) — generated CLI reference + guides, kept true to the code by a drift gate in `just lint` and CI | 2.19 | order 3.7b · CP1 |
| Shipped | 🚧 | Presentation-walk program — phase registry + PhaseChip across landing/docs/README; id-derived roadmap anchors (no `phase-phase`); published `/status/` feature page with Phase column; per-phase subscribe control; SYS-3 content gate (live-smoke + schema-test + Payload validate); Tier-A point fixes (operator-quickstart CTA, product-path README, absolute doc links, MVP wording, `llms-guides.txt`, version footer, ADR-index relabel) | 2.19 (order-3.7b) | order 3.7b · CP1 |

> **Presentation-walk 🚧:** delivered on `feat/presentation-walk` (PRES-01…09 + SYS-1/2/3); awaiting push + prod promote + live-smoke green.

> **🚧 — the docs machinery is complete; the site is not yet publicly served.** All ten 2.19 subphases landed: a strict site build, a generated CLI reference drift-gated against the clap tree, `llms.txt` / a per-page markdown twin, and worked `--help` examples + shell completion. The row flips `✅` when `https://docs.apprafter.dev/` returns a page — the remaining steps (DNS, GHCR package visibility, one `apprafter app add`) are operator actions, documented in `docs/operator-guide/publish-the-docs-site.md`. The subphase-by-subphase build history and the closing documentation walk are recorded in ADR 0057 and the changelog.

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
