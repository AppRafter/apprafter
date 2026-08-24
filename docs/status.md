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
| Shipped | ✅ | Deploy applications via a CUE `Application` manifest + GitOps (Argo CD) | 1.6–1.9, 1.15 | baseline |
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
> Caveats: **2.11** ships the seal capability + `apprafter secret seal`; the Backstage encrypt-wizard is deferred to PL1. **1.79c** is `✅` — S0–S4 landed the vertical (CRD + controller + webhook + `apprafter repo creds`), and the S5 acceptance #4 live-wiring shipped as **2.16b-sc**: a destructive coverage-narrowing of a `SourceCredential` (removing a `git.repoPrefixes` / `registry.hosts` entry while a matched app depends on it) now auto-creates a `sourcecredential`-scope `MigrationPlan` in the cred's namespace, pauses BOTH derived-Secret derivations (old wider-coverage Secrets stay in place so in-flight apps keep access), and resumes only on approve (`apprafter migration approve` / Argo node) — actor-agnostic (raw `kubectl edit` trips it too). Walk-verified GREEN on kind+podman (`e2e/sourcecredential-migration-walk.sh`, incl. the kubectl-edit variant); released op/webhook v0.2.36 / cue-cmp v0.1.17 / platform-stack 0.2.45 / cli v0.2.36. **2.6c** — the cross-app SharedVolume path (two apps ref one volume, rolling-update mount survival, `volume rm` refused while referenced) is walk-verified GREEN on kind+podman; the disk **capacity-signal** Warning/condition live-read (kubelet `nodes/stats`) is best-effort; SOFT-skipped on kind but **live-validated GREEN on real Hetzner** (2026-07-16, `e2e/capacity-kubelet-probe-hetzner.sh`: kubelet `node.fs` present, `node_free_fraction≈0.887`). Cross-ns / multi-node shared volumes + intra-app `shareMode: shared` remain T2. **2.6d** is ✅ **verified**: `apprafter export`, `backup` (encrypted restic repo, local-pull default), and `restore` in ALL modes — `--into <fresh-cluster>` + `--data-only` (kind+podman two-cluster walk, run twice) AND `--reprovision` (mode a, clone-to-new — provision a fresh cluster as part of restore) **live-validated end-to-end on real Hetzner** (2026-07-16: provision → backup → destroy → `restore --reprovision` → fresh box, data + secret intact; `e2e/restore-reprovision-hetzner.sh`), which also serves the full-DR (`restore` in <1h) drill. The 2.6d follow-on **automated S3 push** (scheduled off-site backup to a remote bucket) **shipped as 2.6d-4** (opt-in AppRafter CronJob-restic on `PlatformStack.spec.backup`; cli/runner v0.2.33 / operator v0.2.33 / platform-stack 0.2.42 / cue-cmp v0.1.14 / runner image `apprafter-backup:v0.2.33`; 0.2.40+0.2.41 yanked). **Both file AND S3 backup/restore validated GREEN on real Hetzner** — the S3 path took two live-walk fix rounds (1 release-coordination miss + 7 runner bugs — RBAC verb/resource gaps + a singular-`secret` resolution + stderr visibility — all of which passed unit/CRD/review; the full backup wrote a snapshot to Hetzner Object Storage + the restic repo restored cleanly). Row above is `✅` — the confirmation walk on the **published** 0.2.42 ran GREEN end-to-end on real Hetzner (2026-07-17: provision on 0.2.42 → seal scoped Secret → `backup enable` → CronJob backup Job Completed → snapshot in Hetzner Object Storage → `check`+`prune` → `destroy` → `restore --reprovision` from S3 into a fresh box → data + re-sealed secret intact; zero server leak). The scoped-creds security model (V2 branch (a) + V7) is verified on Hetzner OS (see backup-restore.md). local-pull stays the default.

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

> **🚧 — nine of ten subphases closed (a–i), and the only one of them a user can see today is `apprafter --help`.** The machinery half has landed: a validating strict site build (**2.19a**), a generated reference over every command path that is byte-compared against the clap tree (**2.19b**), a content-detected drift gate over the in-scope guides that resolves documented invocations, schema paths and CUE manifests against what ships (**2.19c**), its xref/health half — repository paths, ADR citations, obligations that survive unfencing a block, and a committed corpus census whose obligation counts may not fall (**2.19d**) — and the two mkdocs build hooks: CUE now renders as code, every page carries a one-line `description`, and the site publishes `llms.txt`, `llms-full.txt` and a markdown twin per page (**2.19e**).
>
> **2.19f** settled the structure and the URLs while that was still free — the last subphase in which moving a page cost nothing. The two stub sections that held top-level nav slots were retired (each existed only to say it did not exist yet); five guides named after this repository's word for an end-to-end script are now named for what a reader gets; the two public-ingress pages folded into the operator section, directory and all; and the documentation stopped citing internal phases, subphase codes and a roadmap file the site does not carry. The redirect map the design called for was deliberately **not** built: no URL this site serves has ever been public, so there is nothing to redirect from — the trigger is the first page that moves after publication, and `mkdocs-redirects` is already installed.
>
> **2.19g built the whole publication path and the site is still unserved — the distinction is the point.** Shipped: an image serving a prebuilt site out of Caddy (the site is built in the workflow under the one pinned nix toolchain, never in the Dockerfile, so the published bytes and the gated bytes come from the same build), a deployable `Application.cue` on `docs.apprafter.dev`, and a release workflow that runs the documentation gate **itself** rather than trusting that a separate workflow ran on the same commit. What it could not do is anything outside the repository, and that is what stands between here and a reader: the push, making the new GHCR package pullable by the cluster (a first publish under an org is **private**, and no `SourceCredential` on the cluster covers `ghcr.io/apprafter/`), confirming the zone, a Cloudflare DNS record, one `apprafter app add` run from `docs-site/`, and finally the landing's Docs link. Those are five numbered steps in `docs/operator-guide/publish-the-docs-site.md`, verified against the live cluster rather than written from the design.
>
> **The landing switch is an operator step, not a held-back commit, and the reason is worth carrying forward to every other landing change.** It was first written as a commit of its own on the premise that a commit can be dropped from a push. It cannot: `.github/workflows/landing-autotag.yml` fires on **any** push to the default branch whose diff touches `landing/**`, auto-bumps the newest `landing-v*` tag and dispatches the release, which publishes `landing-web:latest` — the tag the landing's own manifest watches and the operator re-resolves to a digest every reconcile. Merging such a commit **is** deploying it. The commit was removed from the branch; the ordering (site first, link second) is the operator's and is written down in the runbook. The same mechanism is why the landing's own soft-404 bug, found while measuring this one, is recorded rather than drive-by fixed.
>
> **2.19h is the first subphase of this track a user can actually see, because `--help` needs no website.** Every one of the 75 visible leaf commands now ends its help with worked examples (124 lines), there is an `apprafter completion <shell>` for all five shells `clap_complete` supports, and both are held in place by two guards: one resolves every example's command path and flag names against the clap tree, closing a blind spot where the byte-compare proved the pages matched the source while the source could be wrong; the other pins seven help properties that rot without a diff reviewer noticing — including that the aliases the guides already type still reach their command.
>
> **What 2.19h did *not* fix is the defect the whole track was named for, because that was already fixed.** The request that opened this work cited `apprafter secret seal` as not describing `--namespace`. It does, and re-measuring at the closing commit over what clap itself prints: **187 arguments across 97 help pages, none without a description, and no command without an `about`**. Earlier walk-fixes closed that. Five items this subphase's own scope list named are also unbuilt and are listed by name in `plan.md` with a probe for each rather than quietly dropped: `secret list`, a namespace picker for `secret seal`, `status.conditions[]` with remediation in `app status`, `requires = "select"` on `--namespace`, and `app open --yes`.
>
> **2.19i wrote the guides, and its first finding was that there were far fewer of them than this row has been claiming.** "Roughly fifteen missing guides" was an estimate made before any of the machinery existed and re-derived by nobody for seven subphases; measured against the shipped command tree it is **five**, closing **eight** leaf commands taught nowhere. That eight is a measurement rather than a subtraction, and the command is the durable form of it: the inventory in ADR 0057's 2.19i amendment returns 10 untaught leaves at `0a2ba48` and 2 at the close. (A first pass said seven, by subtracting three deliberate absences from a list holding only two of them — `upgrade-tier` was already mentioned in the operator index at `0a2ba48`, so it was never in the 10.) Three commands are deliberately left undocumented and named as such rather than filed as a gap: `apprafter login` and `apprafter upgrade-tier` say "NOT IMPLEMENTED" in their own help, and `apprafter plan` is a skeleton — a guide for any of them would document the future. The five: what an application asks for in CPU and memory and the right-sizing that happens on top of it; sealing an application secret, whose default namespace is the wrong one for that job and fails silently; running one manifest as two environments; choosing the machine, for a capability whose introduction was a breaking change and which had one mention in a troubleshooting entry; and the rest of a target's life — inspect, rename, remove — plus rolling a bad deploy back. The inventory also caught `apprafter plan` promising a diff it never computes, fixed rather than documented. **Three reviews of the landed pages then found four defects the gate cannot see, all repaired here** — the sharpest being that `apprafter destroy` is scoped to a *Hetzner project*, not to a cluster (it filters on the `apprafter=true` label alone; `--target` picks only the state file and token), so the machine guide's two-cluster cut-over route was telling readers to destroy the cluster they had just built; the scope is now stated in `operator-guide/target-store.md#destroy-scope` and linked from every page that instructs the command. The other three: `app list` was named as showing the logical name (its NAME column is `<app>-<env>`), `app status` was named as reporting a webhook rejection (it reports neither `status.conditions` nor `status.operationState`), and the corrected figure above was itself wrong.
>
> **2.19j is closed and the row stays 🚧, which is a deliberate reading of its own rule.** The rule said the row flips `✅` at 2.19j; the *reason* it gave was that a documentation site nobody can open is not a delivered feature. That reason outlives its trigger. All ten subphases are done and the site still answers nothing, because the five operator steps in `docs/operator-guide/publish-the-docs-site.md` are the user's to take — DNS, registry visibility, app registration. **The row flips when `https://docs.apprafter.dev/` returns a page**, not when the last subphase merges.
>
> What the closing walk cost, and why it was worth its length: four walkers read every guide with the release binary while a fifth read the built site, and a merger re-verified each finding independently. **35 findings, 30 blocking, and 23 of them the gate structurally could not catch** — nine subphases of green gates said nothing about them, because the gate resolves names and never truth. Two were instructions that destroy data: a teardown whose scope is the whole provider project rather than one cluster, and an environment change whose recipe drops the database with a seven-day window nobody is told about. One page (298 lines) documented a capability removed in v0.1.97 and passed every gate for nine subphases because a vestigial schema field kept its identifiers resolving. And the walk found a **product** defect on the live cluster: the VPA controllers have been crash-looping since the component shipped, so autoscaling produces recommendations and applies none.
>
> No release and no monorepo tag rides **a–f**. `docsgen` is a build-time crate that ships in no release artefact; a–b changed no shipped behaviour (2.19b added a lib target and a narrow two-item `docs_api` facade; 2.19a and the doc-comment audit touched help text), 2.19c widened the schema bundle `apprafter app validate` embeds, and 2.19d, 2.19e and 2.19f touched the shipped binary not at all.
>
> **2.19g does carry a CLI patch release, `v0.2.45`, and the reversal is worth recording rather than quietly correcting.** The subphase was written and closed on the finding that it touched no `cli/` file; a review then showed the manifest it ships is the first in the repository the CLI's own parser cannot read — `ApplicationExpose.port` was a required serde field, so `apprafter app add` printed a parse warning and hid the environment picker on a manifest `cue vet`, `app validate`, the webhook and the operator all accept. Making it optional is a `cli-core` change, `cli-core` links into the shipped binary, and `apprafter app add` behaves differently as a result — so the earlier claim is false and is replaced rather than amended. `commands.json` moved by its `cli_version` line and was regenerated in the same commit. 2.19g's container image, `ghcr.io/apprafter/docs`, is still versioned by commit SHA and introduces no tag stream of its own.
>
> **2.19h carries the next CLI patch release, `v0.2.46`**, and needs no reversal to say so: it changes the shipped binary twice over — a new `completion` command and an `Examples:` section in the help of 75 commands. `cli/Cargo.toml` is bumped in the same commit and `commands.json` moves by its `cli_version` line, regenerated alongside. Still no chart, operator or cue-cmp artefact.
>
> **2.19i carries `v0.2.47`, and not for the guides.** Documentation ships in no binary; what moves is two `about` texts. `apprafter plan --help` no longer promises a diff against live infrastructure that the command never computes. And `apprafter destroy --help` now opens with the scope: it deletes every `apprafter=true`-labelled resource in the token's **project**, not one cluster — the sentence it used to open with ("infrastructure managed by this state") asserted exactly the narrower reading that led a guide in this same subphase to tell a reader to destroy the cluster they had just built. The prose sweep that fixed eight pages had missed the sentence a reader of a destructive command meets first, which is also the reference page's title, its description and what `--help` prints. Same rule, same shape: `cli/Cargo.toml` bumped and `commands.json` regenerated in the same commit, no chart, operator or cue-cmp artefact.

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
