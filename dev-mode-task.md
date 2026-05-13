# AppRafter Dev Mode — Spec

> **Scope:** Local development experience for AppRafter applications and the platform itself.
> **Status:** Spec for implementation. Owner reviews, agent executes.
> **Revision:** 2 (Self-managing platform integration — alignment with ADR 0025–0029 and spec.md rev.7).
> **Target phases:** 1B (Minimum Viable Dev Mode), 2B (Platform Services integration), 3B (Full Dev Experience). See §21.
> **Companion documents:** `spec.md` §1.8, `cli-dx-task.md` (reused libraries and conventions), `plan.md` (phase map).

---

## 0. Context and motivation

AppRafter today jumps from "no platform" to Tier 1 (single VPS). The fastest iteration loop available is: edit → commit → push → Argo CD reconcile on Tier 1 → observe. That cycle is minutes per iteration, which is fatal for two audiences:

1. **App developers** writing services for AppRafter. They are the platform's primary users. If iteration is slow, they default to Fly.io / Railway / Render where local CLI dev is table stakes.
2. **The platform team itself** (currently solo). Phase 2 (`needs.*`, ServiceProviders, SPIRE) and Phase 3 (multi-node, observability, kine+NATS) involve heavy reconciler work that benefits massively from fast local iteration.

Principle 1.8 of `spec.md` states "enterprise practices must not block solo-tier adoption". Dev mode extends this one rung lower: **Tier 1 must not block local-development adoption**. The same Application manifest a solo founder uses on their €5 VPS should work on their laptop in seconds.

### Why "mode", not "Tier 0"

Tiers in AppRafter denote production scale (Tier 1: solo prod → Tier 4: confidential prod). Local development is a different axis — iteration speed and developer ergonomics, not substrate scale or compliance. Conflating the two would invite questions like "can I run production on Tier 0?" (no) and dilute the tier model.

Dev mode is a **mode of operating the same Application manifest**, optimised for fast iteration on a single machine. It is not a tier.

### Dogfood argument

This spec deliberately delivers in three phases (1B, 2B, 3B), interleaved between platform phases 1, 2, 3. Early phases are minimal — primarily to accelerate platform development, with end-user value as a side effect. Full end-user value lands in Phase 3B, which forms part of MVP completion alongside Phase 3.

---

## 1. Goals

### Primary

1. **Sub-minute iteration loop.** File save → running code in local cluster: under 10 seconds for code changes, under 60 seconds for manifest changes.
2. **Manifest portability.** The same `apprafter.cue` works in dev mode and in production. Differences are expressed as DevProfile overlays, not as separate manifests.
3. **`needs.*` support locally.** Applications declaring `needs.pg`, `needs.redis`, etc., get those dependencies provisioned in the local cluster automatically through the same ServiceProviders used in production.
4. **Hot reload by default.** Where the runtime supports it (Bun, Node, Python, etc.), code changes apply without pod restart. Where it doesn't (Rust, Go), the platform performs a fast rolling restart.
5. **Persistent state by default.** Cluster restarts and machine reboots do not destroy developer data. Explicit `reset` / `wipe` commands are required to clear state.
6. **Shared vs personal manifest separation.** Team-shared dev settings live in Git; personal paths, debug ports, feature flags do not.

### Secondary

7. Interactive `dev init` wizard with runtime auto-detection.
8. Workspace support for monorepos (single command runs multiple services).
9. Staged diagnostic output that localises failures by lifecycle stage.
10. Debugger port forwarding with sensible defaults per runtime.
11. Honest documentation of hot reload semantics per runtime (some are sub-second, some are seconds-long restart).

---

## 2. Non-goals

- **Tier 0 as a production tier.** Dev mode is for development only; production deployments target Tier 1+.
- **Full Tier 1+ stack reproduction locally.** No Argo CD, no Backstage, no OpenBao, no observability stack, no SPIFFE/SPIRE. Dev mode runs the minimum required to execute Application reconciliation against `needs.*`.
- **Shared dev clusters between developers.** Each developer has their own local cluster. Team collaboration on dev state is out of scope.
- **Cloud-synced dev profiles.** Personal `.local.cue` files do not sync between machines; copy via dotfiles if needed.
- **Cross-platform Windows-native support.** Windows users run via WSL2. No native Hyper-V integration.
- **Production-grade observability locally.** `dev logs` and `dev exec` are the diagnostic surface; full OpenTelemetry/VictoriaMetrics stack is out of scope.
- **Full MigrationPlan semantics.** Dev mode uses a simplified destructive-change detector. The full MigrationPlan CRD lands in Phase 4.16.
- **Multi-cluster dev environments.** Exactly one local cluster, fixed name `apprafter-dev`. Future expansion deferred.
- **Web/GUI configuration.** CLI-only.
- **Anonymous telemetry of dev mode usage.** No metrics shipped to platform team without opt-in.
- **Public preset registry / marketplace.** Bundled presets only; community sharing happens via copy-paste of `.cue` files (gist, repo, wherever).

---

## 3. Conceptual model

### 3.1 DevProfile

A CUE kind that declares how a single Application should run in dev mode. DevProfile is **opt-in**: applications without one use defaults from runtime detection.

Two layers per Application:

- **DevProfile (shared)** — committed to Git in `.apprafter/dev.cue`. Contains team-agreed defaults: runtime preset, build commands, watch rules, restart triggers, default ports for tooling. Same across all team members.
- **DevProfileLocal (personal)** — gitignored at `.apprafter/dev.local.cue`. Contains per-developer settings: absolute source paths, debugger ports, personal env overrides, alternative service backends (e.g., point `needs.pg` at a shared team DB instead of local).

### 3.2 Layered manifest unification

When `apprafter dev up` resolves an application, it produces an effective manifest by CUE unification of four layers, in order:

1. `Application.base` (production base from `apprafter.cue`)
2. `Application.environments.dev` if declared (production-side dev overrides)
3. `DevProfile` (shared dev settings from `.apprafter/dev.cue`)
4. `DevProfileLocal` (personal dev settings from `.apprafter/dev.local.cue`)

CUE unification rules apply: conflicting concrete values cause compile errors. The personal layer can refine but not contradict the shared layer for overlapping fields. Dev-only fields (sources, debugger, hot reload config) live in a namespace that production layers don't touch, so a missing DevProfile is a non-error.

### 3.3 Workspace

In monorepos, an `apprafter.workspace.cue` at repo root lists known applications by path. Workspace commands (`dev up --all`, `dev down --all`) operate across the listed set. The CLI walks up from cwd to find the nearest workspace file. Without one, the CLI assumes single-application mode and operates on the nearest `apprafter.cue` above cwd.

### 3.4 Lifecycle stages

Every `apprafter dev up` invocation passes through a fixed pipeline of stages, in order. Each stage has a clear success/failure boundary, and the CLI reports its outcome explicitly. This is the basis for staged diagnostics (§14).

Stages:
1. Manifest discovery and parsing
2. Manifest validation (CUE compile + admission rules)
3. Effective manifest synthesis (layered unification)
4. Cluster reachability check
5. Dependency resolution (ResourceClaim setup for `needs.*`)
6. Source sync (bind mount setup)
7. Pre-build steps (init containers with cache check)
8. Pod startup
9. Readiness probe pass

A failure at stage N skips N+1 onward. Diagnostic output (§14) identifies the failed stage and shows the relevant logs.

> **Note on stage 5 (Dependency resolution):** in production, a destructive change to `needs.*` (e.g., switching `needs.pg.selector` from `tier: integrated` to `tier: managed-aws`) creates a `MigrationPlan` (spec.md §3.8) and pauses reconciliation until approved. **The same is true in dev mode by default** — silent destructive changes that wipe local test data are more annoying than the friction of approving a plan. `apprafter dev up` blocks with a clear message when a pending `MigrationPlan` exists for the target application:
>
> ```
> Pending MigrationPlan for parser: needs.pg.selector change (data-migration)
> Approve via:    apprafter migration approve <plan-name>
> Or revert the change in your manifest and re-run dev up.
> ```
>
> An opt-in `autoApprove` setting is available для developers who explicitly want auto-approve behaviour in their local cluster (see §12.4). It's off by default.

### 3.5 File system conventions

```
<project-root>/
├── apprafter.cue                    # Application manifest (single-service repo)
├── apprafter.workspace.cue          # Optional: workspace registry (monorepo)
├── services/                        # Monorepo layout
│   └── parser/
│       ├── apprafter.cue            # Per-service Application
│       └── .apprafter/
│           ├── dev.cue              # Shared DevProfile (in Git)
│           └── dev.local.cue        # Personal DevProfile (gitignored)
└── .apprafter/                      # Single-service repo layout
    ├── dev.cue                      # Shared DevProfile
    ├── dev.local.cue                # Personal DevProfile (gitignored)
    └── backups/                     # Auto-created on --backup-first
        └── <app>/<timestamp>.dump
```

The CLI auto-appends `*.local.cue` and `.apprafter/backups/` to `.gitignore` on first `dev init` if missing.

### 3.6 Two reconciliation paths

Dev mode runs two distinct reconciliation loops simultaneously, and understanding the boundary between them is essential for predicting cluster behaviour:

**Platform reconciliation (Argo CD):** the local k3d cluster, after `apprafter dev cluster up`, has Argo CD running as the sole control surface for **platform components** — Cilium, the AppRafter operator, admission webhook, default NetworkPolicies, Argo CD itself. These come from the same OCI chart (`oci://ghcr.io/apprafter/platform-stack`) as production, just with the `tier: dev` overlay enabled (see §12 below). Platform changes flow through `PlatformStack` CR edits → PlatformController → Argo CD sync.

**User application reconciliation (direct apply):** `apprafter dev up` does **not** push application manifests through Argo CD. Instead, it applies the resolved `Application` CR directly to the cluster via kubectl-equivalent API calls. The AppRafter operator's Application reconciler then creates child resources (Deployment, Service, PVC) for the user's app. Hot reload (`apprafter dev up --watch`) rebuilds container images and patches the Deployment directly.

**Why two paths:** Argo CD's reconcile cycle (~30s default) is incompatible with hot reload UX expectations (sub-second iteration). Argo CD also requires a Git source as the truth, while dev mode treats local files (including uncommitted changes) as truth. Going through Argo CD for user apps would break the core dev experience.

**Boundary between paths:**

| Resource scope | Path | Frequency |
|---|---|---|
| Platform components (Cilium, operator, webhook, CRDs) | Argo CD reconciles from OCI chart | On chart version bump |
| `PlatformStack` CR | PlatformController watches | Continuous |
| `ServiceProvider` instances | Operator reconciles (lazy provisioning) | On `needs.*` reference |
| User `Application` CR | Direct apply via `apprafter dev up` | On user command / file change |
| User app child resources (Deployment, Service) | Operator Application reconciler | On Application CR change; hot reload bypasses with direct patch |

**No `apprafter.io/dev-mode: true` annotation needed.** The operator detects dev mode by examining the effective manifest: presence of DevProfile or DevProfileLocal layers (per §3.2 unification) means dev mode. In dev mode, the Application reconciler:
1. Creates child resources on first apply (Deployment, Service, etc.) as usual.
2. Does **not** enforce drift correction on child resource fields commonly touched by hot reload — image, command, env-from, source bind mounts.
3. Still enforces drift on identity-level fields (selectors, ownerReferences, resource quotas) so a stuck dev cluster doesn't accumulate orphaned resources.

See spec.md §3.10 (Platform stack management via Argo CD) and §3.11 (PlatformStack CRD) for the platform side; this section documents how dev mode reuses that machinery while keeping user-app iteration fast.

---

## 4. CLI command structure

Two command groups, mirroring the Docker `engine` vs `run` distinction.

### 4.1 Cluster commands (machine-wide, one cluster total)

- `apprafter dev cluster up [--without <providers>]`
- `apprafter dev cluster down`
- `apprafter dev cluster status`
- `apprafter dev cluster wipe`

### 4.2 Application commands (per-project, run from any directory)

- `apprafter dev init [--preset <name>] [--no-interactive]`
- `apprafter dev up [<name>...] [--all] [--watch/--no-watch]`
- `apprafter dev down <name> | --all | --glob <pattern> | --workspace <path>`
- `apprafter dev list [--all-workspaces]`
- `apprafter dev logs <name> [--follow] [--tail <N>] [--since <duration>]`
- `apprafter dev exec <name> -- <command...>`
- `apprafter dev reset <name> | --all [--backup-first]`
- `apprafter dev restore <name> <backup-file>`

### 4.3 Aliases

- `apprafter d` → `apprafter dev`

Subcommands inside `dev` have unique enough names; no further aliases. Documented in `--help` but not advertised in first-time UX.

---

## 5. Subcommand specification

For each command: purpose, behaviour, key options, example output.

### 5.1 `apprafter dev cluster up`

**Purpose:** create local k3d cluster, install Argo CD as the bootstrap loader, and let it reconcile the rest of the platform stack from the OCI chart with `tier: dev` overlay.

**Behaviour:**
- Verify k3d is installed; if missing, point to mise/nix recipe and fail clearly.
- Verify Docker daemon is reachable; fail clearly if not.
- Create a single-node k3d cluster, name fixed at `apprafter-dev`. Mount Docker socket (optional, for image cache reuse).
- Merge kubeconfig into `~/.kube/config` with context `apprafter-dev`. Existing context with same name is overwritten with confirmation.
- **Bootstrap loader (per spec.md §3.10):** install Argo CD via Helm. This is the only direct install — every other component comes through Argo CD reconciliation.
- **Apply root Application:** apply a single Argo CD `Application` resource pointing to `oci://ghcr.io/apprafter/platform-stack:<resolved-version>` with `values.tier: dev`.
- **Wait for tier-dev components to reach Healthy:** Cilium, AppRafter operator, admission webhook, default NetworkPolicies, integrated ServiceProvider operators (pg / redis / jetstream / notifications operators only — no instances). Argo CD shows progress per-component.
- **Create default `PlatformStack` CR** with `spec.values.tier: dev`, `spec.channel: stable`, `spec.source.checkInterval: 24h`. The CR is the declarative platform-version control surface in dev as in production.
- **`MigrationController` behaves as in production by default**: destructive Application or platform changes create `MigrationPlan` CRs requiring approval. Opt-in `autoApprove` setting (see §12.4) is available для developers who explicitly want auto-approval. `apprafter dev cluster status` and `apprafter dev up` surface pending plans prominently.

Real ServiceProvider instances are **not** created at this stage — lazy provisioning applies as in production (spec §4.6).

**Options:**
- `--without <providers>` — comma-separated list to skip (e.g., `--without pg,redis`).
- `--port-mapping <host>:<container>` — extra mappings (default: 80, 443 from host).

**Example output (success):**

```
Creating local cluster...
  ✓ k3d binary found (v5.7.4)
  ✓ Docker daemon reachable
  ✓ Created cluster apprafter-dev (1 node)                    (28s)
  ✓ Merged kubeconfig context: apprafter-dev

Installing platform stack...
  ✓ Argo CD installed (bootstrap loader)                      (32s)
  ✓ Root platform Application applied
  Reconciling components (tier: dev):
    ✓ cilium                                                  (45s)
    ✓ apprafter-operator                                      (18s)
    ✓ admission-webhook                                       (12s)
    ✓ network-policies                                        (3s)
    ✓ pg-operator                                             (15s)
    ✓ redis-operator                                          (14s)
    ✓ jetstream-operator                                      (16s)
    ✓ notifications                                           (8s)
  ✓ PlatformStack/default created (channel: stable, v0.3.0)

Cluster ready. Memory footprint: ~1.1 GB (Argo CD + operators).

Next:
    cd <your-app> && apprafter dev up
    apprafter open argocd   # inspect platform components
```

> **Memory note:** dev mode footprint is now ~1.1 GB (vs ~720 MB in pre-self-managing draft) due to Argo CD overhead. Users on memory-constrained laptops should allocate at least 4 GB to Docker Desktop / Colima.

**Example output (failure — Docker not running):**

```
Creating local cluster...
  ✓ k3d binary found (v5.7.4)
  ✗ Docker daemon unreachable

  Error: cannot connect to Docker daemon at unix:///var/run/docker.sock
  
  Hint: start Docker Desktop (macOS/Windows) or run `systemctl start docker` (Linux).
        Verify with: docker ps
```

**Example output (failure — offline / OCI registry unreachable on first run):**

```
Creating local cluster...
  ✓ k3d binary found
  ✓ Docker daemon reachable
  ✓ Created cluster apprafter-dev (1 node)                    (28s)
  ✓ Argo CD installed (bootstrap loader)                      (32s)
  ✓ Root platform Application applied
  Reconciling components (tier: dev):
    ✗ Argo CD cannot pull platform-stack chart

  Error: failed to pull oci://ghcr.io/apprafter/platform-stack
  Cause: connection refused / DNS resolution failed

  Hint: First `dev cluster up` requires internet access to pull the platform chart.
        Subsequent restarts work offline (Argo CD uses Docker image cache).
        Verify connectivity: curl -v https://ghcr.io
        Or set an alternative source: kubectl edit platformstack default
                                      → spec.source.repoURL
```

> **Offline operation after first successful bootstrap:** once `dev cluster up` has completed once on this machine, subsequent invocations work offline. k3d uses cached container images for cluster creation; Argo CD's chart-cache and Docker image cache cover platform components. Argo CD marks Applications `Degraded` when unable to reach upstream for update checks, but existing workloads continue running normally. This is standard Argo CD + Kubernetes behaviour — no additional offline-mode code is required.

### 5.2 `apprafter dev cluster down`

**Purpose:** stop the cluster. State is preserved.

**Behaviour:**
- Stop k3d cluster (containers paused/removed, volumes retained).
- Kubeconfig context preserved (cluster will be reachable on next `up`).
- Local cluster state file remembers last-known state for fast restart.

### 5.3 `apprafter dev cluster status`

**Purpose:** show current cluster state, platform stack version, and pending migrations.

**Example output:**

```
Cluster: apprafter-dev (running)
  Created:        2026-05-11 14:32:00
  Uptime:         3h 12m
  Memory:         1.2 GB / 4 GB allocated
  Disk:           340 MB used / 10 GB
  Nodes:          1

Platform stack:
  Version:        0.3.0 (channel: stable)
  Last check:     6 hours ago
  Status:         all components Healthy
  Update:         0.3.1 available (safe) — run `apprafter platform upgrade` to bump

Pending migrations: 1
  ⚠ parser-pg-migration-2026-05-15  (data-migration, awaiting approval)
    Approve:  apprafter migration approve parser-pg-migration-2026-05-15

Installed providers (lazy-provisioned):
  ✓ pg              CNPG v1.24 (operator only, 0 instances)
  ✓ redis           Dragonfly v1.21 (operator only, 0 instances)
  ✓ jetstream       NATS v2.10 (operator only, 0 streams)
  ✓ notifications   built-in (running)

Running applications: 2
  parser            apprafter-dev    up 2h 14m  ⚠ blocked by migration
  gateway           apprafter-dev    up 2h 12m
```

**Notes:**
- The `Update: X.Y.Z available` line shows when local dev platform stack version has fallen behind the stable channel. Suggested practice: bump dev periodically so it stays close to what production runs, avoiding "works on prod, not on dev" surprises.
- **Pending migrations section** is shown only when count > 0. Developers don't typically browse the Argo CD UI during local iteration — surfacing pending plans in `dev cluster status` is the primary visibility channel. Each pending plan has a copy-paste-ready approve command.
- The `⚠ blocked by migration` marker on running applications indicates that the current Application reconciler is paused for that app due to a pending MigrationPlan; child resources continue running with the previous spec.

### 5.4 `apprafter dev cluster wipe`

**Purpose:** destroy the cluster and all persistent volumes.

**Behaviour:**
- Interactive confirmation listing what will be lost (apps, claim data sizes).
- `--yes` skips confirmation.
- Removes kubeconfig context.

### 5.5 `apprafter dev init`

**Purpose:** initialise DevProfile files in the current project.

**Behaviour (interactive, default when TTY):**
- Detect runtime via heuristics (§7).
- If detected: confirm with the developer ("Detected: bun. Use this? [Y/n]"); allow override.
- If not detected: present list of supported runtimes, pick one.
- Generate `.apprafter/dev.cue` from selected preset (§8) with reasonable defaults.
- Generate `.apprafter/dev.local.cue` with developer-specific bits (absolute source path, debugger port).
- Add `*.local.cue` and `.apprafter/backups/` to `.gitignore` if missing.
- Print next step ("Run `apprafter dev up` to start.").

**Behaviour (non-interactive):**
- Require `--preset <name>` and `--no-interactive`.
- Fail clearly if any required field cannot be auto-determined.

**Example output (interactive):**

```
$ apprafter dev init

Detecting project type...
  Found: package.json (bun.lock present)
  Detected runtime: bun (confidence: high)

? Use bun preset? [Y/n] › Y
? Source path: › ./src
? Debugger port: › 9229
? Add .apprafter/* to .gitignore? [Y/n] › Y

✓ Created .apprafter/dev.cue
✓ Created .apprafter/dev.local.cue
✓ Updated .gitignore

Next:
    apprafter dev up
```

### 5.6 `apprafter dev up`

**Purpose:** start application(s) in the local cluster.

**Behaviour:**
- Find nearest `apprafter.cue` upward from cwd (or `apprafter.workspace.cue` for `--all`).
- Run the full lifecycle pipeline (§3.4); abort and report on first failure.
- Apply effective manifest via AppRafter operator in `apprafter-dev` namespace.
- Set up bind mount of source path into pod.
- Set up file watcher (unless `--no-watch`).
- Stream initial logs until pod is `Ready`.
- Detach (keep watcher running in background); show how to follow logs / attach debugger.

**Options:**
- `<name>...` — explicit application names (resolved against workspace).
- `--all` — all apps from workspace (requires `apprafter.workspace.cue`).
- `--watch` / `--no-watch` — enable/disable file watcher (default: enabled).
- `--apply-only` — render and apply manifest, skip watcher and log stream.
- `--auto-recreate-on-destructive` — auto-confirm destructive changes (for CI).
- `--backup-first` — back up data before destructive change.

**Example output (success):**

```
Starting application: parser

  ✓ Manifest discovered: ./apprafter.cue                        (0.0s)
  ✓ Manifest valid                                              (0.1s)
  ✓ Effective manifest synthesised (base + dev + dev.local)     (0.1s)
  ✓ Cluster reachable (apprafter-dev)                           (0.2s)
  ✓ Dependencies ready: pg (claim parser-pg, 12 MB)             (1.4s)
  ✓ Sources synced (847 files, 12 MB)                           (0.6s)
  ✓ Pre-build: install-deps (cache hit)                         (0.4s)
  ✓ Pod started: parser-7d4f9c-xyz                              (3.1s)
  ✓ Ready                                                       (1.8s)

Application running.
  URL:       http://localhost:8080 (port-forwarded from pod)
  Logs:      apprafter dev logs parser --follow
  Debugger:  attach on localhost:9229 (Node Inspector)
  Stop:      apprafter dev down parser
```

**Example output (failure at pod startup, see §14 for full diagnostic format):**

```
Starting application: parser

  ✓ Manifest discovered                                         (0.0s)
  ✓ Manifest valid                                              (0.1s)
  ✓ Effective manifest synthesised                              (0.1s)
  ✓ Cluster reachable                                           (0.2s)
  ✓ Dependencies ready: pg                                      (1.4s)
  ✓ Sources synced                                              (0.6s)
  ✓ Pre-build: install-deps                                     (8.2s)
  ✗ Pod startup failed                                          (after 30s)

  Pod: parser-7d4f9c-xyz
  Status: CrashLoopBackOff (3 restarts)
  Last exit code: 1
  
  Container logs (last 50 lines):
  ────────────────────────────────────────
  Error: connect ECONNREFUSED 127.0.0.1:5432
      at Object.callback (/app/node_modules/pg/lib/connection.js:127:14)
      at /app/src/db.ts:14:5
  ────────────────────────────────────────
  
  Init container logs: ok
  
  Pod events (last 5):
    10:32:01  Pulled    Image already present
    10:32:02  Created   Started container parser
    10:32:08  BackOff   Back-off restarting failed container
  
  Hint: app started but exited. Check container logs above.
        Common causes: missing env, unreachable dependencies, startup probe failure.
        DATABASE_URL points to 127.0.0.1 — should be the claim hostname.
  
  More:
    apprafter dev logs parser --tail 200
    apprafter dev exec parser -- sh
```

### 5.7 `apprafter dev down`

**Purpose:** stop application(s). Volumes are retained.

**Behaviour:**
- Resolve target apps from arguments (single name, `--all`, `--glob`, `--workspace`).
- Delete Deployment + Service + bind mounts; preserve PVCs and ResourceClaims.
- File watcher process (if running for that app) is terminated.

### 5.8 `apprafter dev list`

**Purpose:** show running dev applications.

**Behaviour:**
- Query cluster by label `apprafter.io/dev=true`.
- Local cache (§15.1) provides fast path; refresh from cluster on mismatch.
- `--all-workspaces` shows apps from all workspaces (default: current workspace only).

**Example output:**

```
NAME      WORKSPACE                STATUS    UPTIME   NEEDS
parser    ~/code/myrepo            running   2h 14m   pg, redis
gateway   ~/code/myrepo            running   2h 12m   pg
worker    ~/code/myrepo            crashed   5m       pg, jetstream

3 applications.
```

### 5.9 `apprafter dev logs`

**Purpose:** tail logs for an application.

**Behaviour:**
- Thin wrapper over `kubectl logs` against `apprafter-dev` namespace, label-selected.
- Supports `--follow`, `--tail`, `--since` (parses Go duration: `5m`, `2h`).
- Multi-pod apps: aggregate by default; `--pod <name>` for specific.

### 5.10 `apprafter dev exec`

**Purpose:** run a command in an app pod.

**Behaviour:**
- Thin wrapper over `kubectl exec -it`.
- Useful for `apprafter dev exec parser -- sh` or `dev exec parser -- bun repl`.

### 5.11 `apprafter dev reset`

**Purpose:** clear application state (drops PVCs and ResourceClaim data).

**Behaviour:**
- Interactive confirmation listing what will be lost (claim names, data sizes).
- `--all` resets all dev apps' state, leaves cluster intact.
- `--backup-first` runs `pg_dump` / `BGSAVE` / stream backup before drop.
- `--yes` skips confirmation.

### 5.12 `apprafter dev restore`

**Purpose:** restore an application's state from a backup file.

**Behaviour:**
- Detects backup type from extension or file header (pg_dump vs redis vs jetstream).
- Routes restore through the appropriate ServiceProvider mechanism.
- Refuses if app is currently running (require `dev down` first).

---

## 6. Manifest layering rules

### 6.1 Unification order

```
effective = Application.base
          & (Application.environments.dev | {})
          & (DevProfile           | {})
          & (DevProfileLocal      | {})
```

Each layer is optional. `Application.base` is the only required input.

### 6.2 Conflict semantics

CUE unification: conflicting concrete values → compile error with line/column of both sides.

Examples:
- `Application.base.replicas: 3` + `DevProfile.replicas: 1` → both concrete, but unification works because `replicas: 1` is "more specific" only if `base.replicas` is `*3 | int` (open default). Spec recommends production fields use open defaults so dev can refine.
- `Application.base.image: "ghcr.io/me/parser:v1"` + `DevProfile.image: "oven/bun:1-alpine"` → CUE error if both concrete strings. Resolution: DevProfile should specify `image` only when intentionally overriding (e.g., dev image with full runtime); base should leave `image` open if dev is expected to override.

CLI surfaces conflicts with miette-style diagnostics showing both manifest locations.

### 6.3 Dev-only fields

The following fields exist only in DevProfile / DevProfileLocal and are not part of `Application`:

- `sources`: bind mount config (host path, container path, exclude patterns)
- `build`: dev-time build steps with cache directives
- `watch`: file watcher rules
- `restartOn`: file patterns that trigger pod restart
- `debugger`: debugger port and config
- `hotReload`: explicit on/off (default: detected from runtime preset)
- `runtime`: preset selector (`bun-generic`, `rust-generic`, etc.) or full custom command spec

These do not conflict with production fields; an Application without DevProfile simply lacks these.

### 6.4 Personal layer scope

`DevProfileLocal` should be used for:
- Absolute host paths (developer's filesystem layout differs)
- Debugger ports (developer's preference / IDE config)
- Personal env overrides (feature flags, local test data identifiers)
- Alternative service backends (point `needs.pg` at a personal external DB)

It should **not** be used for things that affect correctness of the app under test for the whole team. Those belong in shared `dev.cue` so behaviour is reproducible across team.

The CLI does not enforce this distinction; it's documentation guidance.

---

## 7. Heuristic runtime detection

### 7.1 Supported runtimes (Phase 3B initial set)

1. **node / bun** (treated as one family, preset distinguishes)
2. **rust**
3. **python**
4. **go**

### 7.2 Detection signals

| Runtime | Strong signals | Weak signals |
|---|---|---|
| bun | `bun.lock`, `bunfig.toml` | `package.json` with bun-specific scripts |
| node | `package-lock.json`, `pnpm-lock.yaml`, `yarn.lock` | `package.json` only |
| rust | `Cargo.lock`, `Cargo.toml`, `target/` | `*.rs` files only |
| python | `pyproject.toml`, `poetry.lock`, `requirements.txt`, `Pipfile.lock` | `*.py` files only |
| go | `go.mod`, `go.sum` | `*.go` files only |

### 7.3 Confidence rating

- **High** — lock file present and at least one strong signal. Suggest as default.
- **Medium** — manifest file present (`package.json`, `Cargo.toml`) but no lock file. Suggest with caveat.
- **Low** — only source files. Don't auto-select; list all runtimes and let user pick.

Confidence is shown in interactive `dev init` output. Non-interactive mode requires `--preset` regardless of detection.

### 7.4 Multi-runtime detection

If multiple runtimes detected (e.g., `package.json` + `Cargo.toml` in same directory):
- Interactive: list both, let user pick.
- Non-interactive: error with hint to specify `--preset`.

---

## 8. Bundled presets

### 8.1 Embedded in CLI

Presets ship as embedded strings in the CLI binary — no separate download, no network access required.

### 8.2 Initial preset list (Phase 3B)

- `bun-generic` — generic Bun app with `bun --hot`
- `node-generic` — generic Node app with `tsx watch` or `nodemon` (detected from deps)
- `rust-generic` — generic Rust binary with `cargo watch -x run`
- `python-generic` — generic Python app with `uvicorn --reload` if FastAPI detected, else plain `python -m`
- `go-generic` — generic Go binary with `air` (or fallback to manual rebuild)

Optional sixth preset if cleanly extractable from feedback: `nestjs` (NestJS-specific structure with `bun nest start --watch`).

### 8.3 Preset structure

Each preset defines:

- Dev image (full-runtime variant, e.g., `oven/bun:1` not `oven/bun:1-alpine`)
- Default command and arguments
- Default watch include / exclude patterns
- Default restart-trigger files
- Default debugger port and protocol
- Build steps with cache keys/outputs

### 8.4 Customisation

Generated `.apprafter/dev.cue` references a preset by name and then overrides specific fields:

```cue
kind: DevProfile
application: parser
preset: "bun-generic"

build: {
    steps: [{
        name: "install-deps"
        command: ["bun", "install", "--frozen-lockfile"]
        cacheKey: ["bun.lock", "package.json"]
        cacheOutputs: ["node_modules"]
    }]
}

// preset defaults below can be overridden as needed
watch: {
    exclude: ["**/*.test.ts"]  // appended to preset defaults
}
```

### 8.5 Community sharing

Community-authored presets are plain `.cue` files. No registry, no signing, no auto-discovery. Sharing happens via gist / repo / copy-paste. The CLI does **not** ship infrastructure for community preset distribution.

---

## 9. Build and cache semantics

### 9.1 Build step declaration

A DevProfile can declare zero or more build steps, executed sequentially before the main container starts.

```cue
build: {
    steps: [
        {
            name: "install-deps"
            command: ["bun", "install", "--frozen-lockfile"]
            cacheKey: ["bun.lock", "package.json"]
            cacheOutputs: ["node_modules"]
        },
        {
            name: "generate-types"
            command: ["bun", "run", "codegen"]
            cacheKey: ["schema.graphql", "package.json"]
            cacheOutputs: ["src/generated"]
        }
    ]
}
```

### 9.2 Cache invalidation

Each step's cache is keyed by SHA-256 of the files in `cacheKey`. If hash matches the value stored in the named volume, the step is skipped. If different, the command runs and the new hash is stored.

Hash storage location: `.apprafter-cache-key` file at the root of the named volume for that step.

### 9.3 Implementation approach (not prescriptive)

Init containers in the Pod spec, one per build step. Each init container:
1. Mounts its `cacheOutputs` directory as a named docker volume.
2. Compares hash of `cacheKey` files against stored hash.
3. If different (or first run): execute `command`, write new hash.
4. If same: skip.

Main container then mounts the same named volumes.

Operator generates the Pod spec from CUE declarations — developers do not write shell scripts manually.

### 9.4 Cache scope

Cache volumes are **per-application**, not shared across applications in a workspace. Each app gets its own `node_modules`, `target/`, etc. Cross-application caching (workspace-level hoisting) is delegated to the package manager (pnpm/bun workspaces, cargo workspaces) and is not a platform concern.

---

## 10. File watcher rules

### 10.1 Code change handling

Files matching `watch.include` (minus `watch.exclude` and ignore-file unions) trigger:
- **Hot reload via runtime**: file change visible inside container; runtime's watcher (`bun --hot`, `tsx watch`, etc.) handles reload.
- **No platform action**: platform does not restart the pod. Reload speed is the runtime's responsibility.

The platform's role is to make the file visible inside the container quickly (bind mount sync). This is essentially zero-latency on Linux; macOS/Windows users may see seconds of delay on large projects (VirtIO-FS limitation, documented honestly).

### 10.2 Manifest change handling

Files `apprafter.cue`, `.apprafter/dev.cue`, `.apprafter/dev.local.cue`, `apprafter.workspace.cue` are **not** auto-applied on change.

Reasoning: manifest edits are often partial during refactors. Auto-applying mid-refactor produces broken states. Developer explicitly re-runs `apprafter dev up` when ready.

When the file watcher detects a manifest change, it prints a one-line notice:

```
[14:32:51] apprafter.cue changed — run `apprafter dev up` to apply.
```

### 10.3 Restart-trigger files

Some files require a full pod restart even when other files only need hot reload. Examples:
- `package.json` (new dependency added)
- `tsconfig.json` (compiler config change)
- `.env` files
- `Cargo.toml` (for Rust)

These are listed in DevProfile `restartOn`:

```cue
restartOn: ["package.json", "tsconfig.json", ".env"]
```

Change to a `restartOn` file → CLI performs `kubectl rollout restart deployment/<app>` automatically.

### 10.4 Ignore file precedence

Effective ignore set (files **not** synced into container and **not** watched):

```
ignored = (.gitignore patterns)
       ∪ (.dockerignore patterns)
       ∪ (DevProfile.sources.excludeFromSync)
       ∪ (DevProfile.watch.exclude)
```

`.npmignore`, `.helmignore`, etc., are intentionally **not** consulted — they describe publishing semantics, not deployment.

### 10.5 Force-include override

`DevProfile.sources.includeForce` is an explicit allow-list that overrides ignores:

```cue
sources: {
    includeForce: ["dist/generated"]   // gitignored but needed at runtime
}
```

Used for the rare case of gitignored generated content that must be present in the container.

---

## 11. needs.* integration in dev mode

### 11.1 ServiceProvider tier: "dev"

Each integrated ServiceProvider supports `tier: dev` as a valid value, with overlays that:
- Reduce replica count to 1 (no HA in dev)
- Reduce memory/CPU requests to minimum viable
- Use local-path-provisioner as `storageClassName`
- Disable backup schedules (dev backups are manual via `dev reset --backup-first`)

ServiceProvider operators run in `apprafter-dev` namespace. Each `needs.*` claim provisions an instance in this namespace.

### 11.2 Lazy provisioning

ServiceProvider operators are installed during `dev cluster up`. Real service instances (PG clusters, NATS streams, Dragonfly pods) are created only when an Application's `needs.*` triggers a ResourceClaim. This matches spec §4.6 `scaleOnDemand`.

Empty cluster (after `dev cluster up`, before any `dev up`): operators only, ~720 MB total memory footprint.

### 11.3 Persistent volumes

PVCs created for `needs.*` claims use `Retain` reclaim policy. Volumes survive:
- Pod restarts (always)
- `dev down <app>` (always)
- `dev cluster down` + `dev cluster up` (always)
- `dev cluster wipe` — **destroys volumes**, with confirmation

Explicit cleanup via `dev reset <app>` or `dev cluster wipe`.

### 11.4 Credential injection

Dev mode uses Kubernetes Secrets directly, not SPIFFE/OpenBao (those are Phase 2+ production-tier features). The operator generates Secrets from ResourceClaim outputs and mounts them as env in the Pod spec.

Selector matching for credentials follows the production Application reconcile logic — same code path, simpler backing.

> **Why dev mode does not use SealedSecrets or OpenBao directly:** in production tier-1, secrets flow through SealedSecrets in Git or OpenBao at higher tiers (spec §4.4). In dev mode, the layered resolution (process env > `.env` > DevProfileLocal > warning + empty) is sufficient for local iteration and avoids the friction of sealing/unsealing keys for ephemeral dev secrets. The `tier: dev` platform overlay (see §12) disables OpenBao and treats SealedSecrets as opt-in for users who specifically want to test sealing-key flow locally.

### 11.5 NetworkPolicy in dev namespace

Default-deny is **disabled** in `apprafter-dev` namespace. Debugging is hostile when arbitrary services can't reach each other for ad-hoc testing. Production-style policies appear in `dev list` output as a reminder that this differs from prod.

`needs.*`-derived policies (production behaviour) are still generated as ConfigMaps for inspection but not enforced.

### 11.6 External backend override

`DevProfileLocal` can redirect a `needs.*` claim to an external backend:

```cue
overrides: {
    needs: {
        pg: {
            external: "postgres://shared-dev.team.internal:5432/parser_alice"
        }
    }
}
```

When `external` is set, the operator skips local provisioning and injects the external connection string. Used for shared team dev databases, personal external PG, etc.

---

## 12. Self-managing platform integration

Dev mode runs the same Argo CD-managed platform stack as production, configured via the `tier: dev` overlay in the platform-stack chart. This section documents the differences from production behaviour.

### 12.1 Bootstrap path

`apprafter dev cluster up` uses the same minimal bootstrap loader as production (spec.md §3.10): create k3d cluster → install Argo CD via Helm → apply root Application pointing at `oci://ghcr.io/apprafter/platform-stack:<v>` with `values.tier: dev`. All platform components arrive through Argo CD reconciliation, not direct CLI install.

This unifies the bootstrap code path between local k3d, single-VDS Tier 1, and multi-node Tier 2+ deployments — only the tier overlay differs.

### 12.2 The `tier: dev` overlay

The `tier: dev` overlay in `platform-stack/cue/tiers/dev.cue` differs from `tier: solo` (Tier 1) in only two ways:

1. **Structurally unavailable components** — same as Tier 1. Kamaji (hard multi-tenancy), HA replicas, and similar require multiple nodes; both single-node deployments share the same structural constraints.
2. **Opt-in vs opt-out defaults** — components that Tier 1 enables by default may be opt-in in dev (and vice versa), reflecting the difference between "production-tier solo deployment" and "single-developer local iteration". **Developers can opt into anything that's available in Tier 1 to make their local environment as close to production as desired.**

There is **no resource-scaling difference** between tier:solo and tier:dev. Both run on a single node with single-replica components (Argo CD single replica, no Redis HA, etc.). The minimal footprint principle applies to Tier 1 as well — solo founders pay for one VDS, not three. The footprint-minimisation work for Tier 1 (Phase 1 deliverables) automatically applies to dev mode through the shared chart.

The second point — opt-in vs opt-out defaults — is the key framing: **dev is not "Tier 1 with things removed"**. It's "Tier 1 with different default-on/default-off choices". A developer who wants OpenBao, Hubble, KEDA, full observability, and Backstage running locally can flip them on in `PlatformStack.spec.overrides.*.enabled` and run a tier-1-equivalent local cluster. The defaults exist to favor faster iteration for the common single-developer workflow.

| Component | tier: solo (Tier 1) default | tier: dev default | Structural availability | Why different default |
|---|---|---|---|---|
| Cilium CNI | required | required | available | CNI is mandatory |
| AppRafter operator | required | required | available | Reconciler needed |
| Admission webhook | required | required | available | CRD validation |
| Argo CD | required | required | available | Platform control surface |
| Default NetworkPolicies | default | default | available | Security baseline applies everywhere |
| cert-manager | conditional on domain | conditional on domain | available | Same logic — no domain → no need |
| Backstage | default (when domain set) | opt-in | available | Single-developer doesn't need portal by default; opt-in if learning Backstage flows |
| Hubble | opt-in | opt-in | available | Same — both opt-in |
| Kamaji | n/a | n/a | **structurally impossible** (single-node) | Hard multi-tenancy requires separate worker nodes |
| Capsule policy | default (opt-out) | opt-in | available | Policy enforcement noise in single-developer context; opt-in for testing |
| OpenBao | opt-in (with KMS) | opt-in | available | Same — both opt-in; dev secrets via .env / DevProfileLocal by default |
| SealedSecrets | default | default | available | Defaults align; layered .env still works as user-supplied secret resolution |
| Workload identity (SPIFFE/SPIRE) | opt-in | opt-in | available | Same — both opt-in |
| KEDA (workload autoscaling) | default | opt-in | available | Local single-developer rarely needs autoscaling; opt-in for testing |
| argocd-cue-cmp sidecar | default | **disabled (design choice)** | available but unused | User apps in dev don't go through Argo CD (see §3.6 and §12.5); CMP wouldn't serve anything |
| OpenTelemetry pipeline | opt-in | opt-in | available | Same — both opt-in |
| ClickHouse / VictoriaMetrics | opt-in | opt-in | available | Same — both opt-in; high memory cost mostly avoided in dev defaults |

**Opting in is just editing `PlatformStack`:**

```yaml
spec:
  values:
    tier: dev
  overrides:
    hubble:
      enabled: true       # I want network observability locally
    keda:
      enabled: true       # I'm testing autoscale behavior
    backstage:
      enabled: true
      values:
        domain: dev.local # local domain or use port-forward
```

This keeps the production / dev mental model unified: same chart, same CR, same controls — different defaults.

### 12.3 `PlatformStack` CR in dev

`PlatformStack/default` is created automatically by `dev cluster up` with:

```yaml
spec:
  channel: stable               # or beta if dev should track beta
  pin: null                     # let channel resolve to latest
  autoUpgrade: false            # explicit upgrade preferred, manual command
  source:
    upstream: oci://ghcr.io/apprafter/platform-stack
    repoURL: oci://ghcr.io/apprafter/platform-stack
    checkInterval: 24h          # rare upstream checks; dev users don't need constant nagging
  values:
    tier: dev
```

The 24h checkInterval is a deliberate choice. Frequent checks (production default 6h) would surface "update available" notifications too often in a local cluster, becoming noise. A daily check is rare enough to stay quiet but frequent enough that a developer who hasn't bumped their dev platform in a week will see the gap.

Manual upgrade via `apprafter platform upgrade` works the same as production. Recommended cadence: bump dev to current stable when starting work on a new feature, or when production has been bumped recently.

### 12.4 `MigrationController` behaviour in dev

**By default, `MigrationController` in dev behaves identically to production**: destructive Application or platform changes create `MigrationPlan` CRs (spec.md §3.8) and pause reconciliation until approved. Auto-approve is not enabled by default because silent destructive changes that wipe local test data are more annoying than the friction of approving a plan.

**Visibility of pending plans in dev**: a developer typically does not browse the Argo CD UI during local iteration. The CLI surfaces pending plans prominently in several places:

1. **`apprafter dev cluster status`** lists pending plans in a dedicated section (count + first few entries with details).
2. **`apprafter dev up`** blocks with a clear, actionable message when a pending plan exists for the target application:
   ```
   ✗ Cannot continue: pending MigrationPlan for parser
     Plan:           parser-pg-migration-2026-05-15
     Trigger:        needs.pg.selector → tier: managed-aws (data-migration)
     Estimated:      5–15 min downtime, 12 GB data movement

     Approve via:    apprafter migration approve parser-pg-migration-2026-05-15
     Or revert:      git checkout apprafter.cue   # back out the change
   ```
3. **`apprafter migration list`** (existing Track A command) shows all pending plans cluster-wide.

This makes the migration gate **visible** in dev workflows without forcing the developer into the Argo CD UI.

**Opt-in `autoApprove` setting**: developers who explicitly want auto-approval (e.g., for rapid iteration on schema changes where they don't care about preserving test data) can opt in via:

- **Per-cluster setting** (most common):
  ```bash
  apprafter dev cluster config set migration.autoApprove true
  ```
  Persists in local cluster state file; applies to all subsequent destructive changes.

- **Per-command flag** (one-off):
  ```bash
  apprafter dev up --auto-approve-migrations
  ```

- **DevProfileLocal entry** (per-developer default):
  ```cue
  // .apprafter/dev.local.cue
  migrations: autoApprove: true
  ```

When `autoApprove` is on, the controller still:
1. Creates the `MigrationPlan` CR (for visibility and audit).
2. Logs a console-visible warning at approval time: `[dev mode] auto-approved MigrationPlan <name>: <classification>; X GB data may be lost`.
3. Executes the plan.

`kubectl get migrationplans -A` shows the audit trail. If a developer enabled `autoApprove` and regrets a destructive change, they can recover by restoring from `--backup-first` snapshots (`apprafter dev restore`).

**Rationale for the default-off choice:**
- The friction of one `apprafter migration approve <name>` invocation is minor; the cost of silent data loss is high.
- Dev mode often hosts test data that took non-trivial setup time (seed scripts, fixtures, manual exploration). Wiping it silently because an upgrade reclassified a `needs.*` change as destructive is a worse experience than a 10-second approval step.
- Opt-in is a one-line setting for developers who genuinely want fast iteration over data preservation.

### 12.5 User apps do not go through Argo CD

The argocd-cue-cmp sidecar (spec.md §3.10, ADR 0029) renders user app CUE manifests to Kubernetes YAML in production, allowing Argo CD to sync user app repositories. In dev mode, this is disabled in the `tier: dev` overlay because:

- User apps in dev come from local files, not Git repositories.
- `apprafter dev up` applies the resolved Application CR directly to the cluster (operator reconciler handles the rest).
- Hot reload patches Deployment image directly — going through Argo CD's sync cycle would add 30s+ latency per iteration.

The AppRafter operator's Application reconciler still creates child resources (Deployment, Service, etc.) as in production, but recognises dev-mode manifests (those with DevProfile or DevProfileLocal layers present) and:

- **Does not enforce drift** on fields commonly touched by hot reload: container image, command/args, env vars sourced from `.env`, source mount points.
- **Does enforce drift** on identity-level fields: ownerReferences, selectors, resource quotas — so resources can be cleaned up reliably.

This split is what makes the same Application CRD work in both production (strict reconciliation) and dev (lenient on hot-reload-friendly fields).

### 12.6 Offline behaviour

The first `dev cluster up` on a machine requires internet access to pull:
- k3d container images (k3s base, system images)
- Argo CD Helm chart
- Platform-stack OCI chart
- Per-component images referenced by chart (Cilium, operators, etc.)

Subsequent operations work offline because:
- Docker daemon caches all pulled images locally.
- Kubernetes / k3d restarts use cached images.
- Argo CD reconciliation marks Applications `Degraded` if it can't reach upstream for updates, but existing workloads continue running based on already-applied manifests.

No custom offline-mode code is implemented. Standard Docker + Kubernetes + Argo CD caching handles this out of the box.

`apprafter doctor` (per `cli-dx-task.md` §5.9) can include an "Argo CD upstream reachability" check that reports offline state when relevant — this is a UX courtesy, not a functional requirement.

### 12.7 Cross-referencing production behaviour

Dev mode reuses production spec definitions wherever possible:
- `Application` CRD — same schema (spec.md §3.1), with effective manifest including dev layers per §3.2 of this spec.
- `PlatformStack` CRD — same schema (spec.md §3.11), with `tier: dev` values.
- `MigrationPlan` CRD — same schema (spec.md §3.8), with auto-approve enabled via opt-in.
- Argo CD bootstrap loader — same pattern (spec.md §3.10).
- Platform-stack OCI chart — same artifact (spec.md §3.10, ADR 0028), `tier: dev` overlay selects subset.

The principle is: dev mode is **production with selected components disabled and selected behaviours relaxed**, not a separate stack. This keeps coverage between dev iteration and production deployment honest.

---

## 13. Destructive change detection

### 13.1 Classification

Dev mode uses a simplified detector compared to production's MigrationPlan (Phase 4.16).

**Destructive (prompts for confirmation):**
- Removing a `needs.*` claim from the manifest entirely
- Major version upgrade of a platform service (e.g., `needs.pg.version: 15 → 16`)
- Changing `storageClassName` (rare in dev — only one storage class exists, but detector covers the case)

**Non-destructive (applies silently):**
- Replica count changes
- Env additions / removals
- Expose / port changes
- `needs.*.size` changes (grow or shrink) — PVCs resize via local-path; data may be evicted on shrink but that's tolerable in dev
- Image changes
- Command changes

### 13.2 Prompt UX

Interactive (default when TTY):

```
Detected destructive change in parser:

  Change: removing needs.pg (claim parser-pg will be deleted)
  Data:   12 MB across 4 tables
  Last backup: never

  This action cannot be undone without a backup.

Continue? [y/N] › 
```

Flags:
- `--auto-recreate-on-destructive` — skip prompt, proceed (for CI / scripts)
- `--backup-first` — run backup before drop, store in `.apprafter/backups/<app>/<ISO-timestamp>.dump`

### 13.3 Backup mechanics

Backup commands run via `kubectl exec` into the ServiceProvider's data pod:
- pg: `pg_dump` to local file, then `kubectl cp` to host
- redis: `BGSAVE` then copy `dump.rdb`
- jetstream: `nats stream backup` then copy

Stored format: native dump format per service, with a small JSON metadata header (`apprafter-backup-meta.json`) recording app name, claim type, version, timestamp.

`apprafter dev restore <app> <backup-file>` reverses the operation, refusing if the app is currently running (require `dev down` first).

---

## 14. Diagnostic output

### 14.1 Staged pipeline format

Every `dev up` output follows the same shape: each stage gets a ✓ / ✗ marker with timing. On failure, the failing stage's details are shown inline with relevant context.

### 14.2 Stage-specific failure detail

Each stage has a defined set of context to collect on failure:

| Stage | Context on failure |
|---|---|
| Manifest discovery | search path, missing file hint |
| Manifest validation | CUE compile error with line:col |
| Effective manifest synthesis | unification conflict location (both sides) |
| Cluster reachability | API server URL, last connection attempt |
| Dependency resolution | which claim, which provider, reconcile event log |
| Source sync | last sync error, file permission state |
| Pre-build | init container logs, cache key diff |
| Pod startup | container logs (last 50 lines), pod events (last 5), exit code |
| Readiness | probe config, last probe response |

### 14.3 Hints

Each stage failure includes a `Hint:` section with the most likely cause. Hints are static (not generated per failure) and based on common patterns. Examples:

- Manifest validation: "Hint: run `cue vet apprafter.cue` to debug locally."
- Dependency resolution: "Hint: check `dev cluster status` — provider operator may be down."
- Pod startup, exit code 1: "Hint: app started but exited. Common causes: missing env, unreachable dependencies, config error."
- Pod startup, OOMKilled: "Hint: app exceeded memory limit. Adjust `Application.resources.memory` or pod sizing."

### 14.4 Verbose mode

`--verbose` flag adds:
- Full effective manifest output before apply
- All k8s events for the app's pods (not just last 5)
- Operator reconcile log filtered to this app

### 14.5 Format flag

`--format text` (default) — human-readable output as shown.
`--format json` — structured JSON for tooling consumption. Schema documented in `docs/dev-guide/output-format.md`.

---

## 15. State management

### 15.1 Local cluster state file

Path: `$XDG_CONFIG_HOME/apprafter/dev-cluster.json` (Linux/Mac: `~/.config/apprafter/dev-cluster.json`).

Contents:
- Cluster name (always `apprafter-dev`)
- k3d version used to create
- Created timestamp
- Last activity timestamp
- Installed providers list
- Port mappings

Used for fast `dev cluster status` without round-trip to cluster.

### 15.2 Per-app state in cluster

Source of truth for running apps is the cluster itself, queried by label selector `apprafter.io/dev=true`.

Labels on every dev-mode resource:
- `apprafter.io/dev=true`
- `apprafter.io/app=<name>`
- `apprafter.io/workspace-root=<absolute-host-path>` (hashed if too long)
- `apprafter.io/managed-by=apprafter-dev`

### 15.3 Local app cache

Path: `$XDG_CACHE_HOME/apprafter/dev-state.json`.

Contains last-known list of running apps, refreshed on every `dev list` / `dev up` / `dev down`. Used for tab completion and fast list output. Stale-tolerant; cluster is source of truth.

### 15.4 Backup directory

Path: `<workspace-root>/.apprafter/backups/<app>/<ISO-timestamp>.dump`.

Per-workspace, per-app. Auto-created on first `--backup-first`. Gitignored automatically.

### 15.5 Kubeconfig context management

Context name fixed at `apprafter-dev`. Always merged into `~/.kube/config`, never replaces it.

`dev cluster wipe` removes the context. `dev cluster down` leaves the context (will resolve again on next `up`).

If a context named `apprafter-dev` already exists for a different cluster (unlikely but possible), `dev cluster up` warns and asks for confirmation before overwriting.

---

## 16. Library and dependency choices

Reuse where possible from `cli-dx-task.md` to keep operator overhead low.

### 16.1 Reused from cli-dx-task.md

- `clap` — command parsing
- `inquire` — interactive prompts (used in `dev init`)
- `indicatif` — progress bars (used in `dev cluster up`, `dev up`)
- `miette` — diagnostic errors (all stage failures)
- `owo-colors` — terminal colours with `NO_COLOR` support
- `tabled` — `dev list`, `dev cluster status` output
- `dirs` — cross-platform config/cache directory resolution
- `serde` + `serde_yaml` / `serde_json` — config files
- `secrecy` — secret string wrappers (where applicable)

### 16.2 New for dev mode

- **k3d binary** — invoked via subprocess. No Rust SDK exists; the CLI shells out. Version pinning enforced via mise/nix recipe.
- **notify** crate — file watcher (cross-platform: inotify on Linux, FSEvents on macOS, ReadDirectoryChangesW on Windows).
- **kube-rs** — already in operator; CLI uses it for cluster queries (`dev list`, `dev logs` via kubectl-style API client).
- **sha2** — cache key hashing for build steps.

### 16.3 Hot reload mechanics

The platform does **not** ship custom hot-reload tooling. Each runtime preset uses the canonical tool for that ecosystem:
- Bun: `bun --hot`
- Node + TypeScript: `tsx watch`
- Node + JS: `nodemon`
- Rust: `cargo watch -x run`
- Python (FastAPI): `uvicorn --reload`
- Python (other): manual restart on file change (no universal tool)
- Go: `air` if available, else manual restart

---

## 17. TTY, colour, non-interactive

Same rules as `cli-dx-task.md` §9.

- TTY detection via `std::io::IsTerminal`.
- Colour policy: `--color={never,always,auto}` > `NO_COLOR` > `CLICOLOR_FORCE` > TTY auto.
- `--no-interactive` flag disables prompts and progress bars; required for CI usage.

`dev init` and destructive-change prompts require either TTY or explicit flag (`--preset` for init, `--auto-recreate-on-destructive` for destructive changes).

---

## 18. Testing requirements

### 18.1 Unit tests

- Manifest layering (`Application.base & dev & DevProfile & DevProfileLocal`) — fixture-based, including conflict cases.
- Heuristic runtime detector — fixture directories with various combinations.
- Ignore-file union computation (`.gitignore` + `.dockerignore` + DevProfile excludes).
- Cache key hashing (deterministic, file-order-independent).
- Destructive change detector (classification of manifest diffs).
- State file load/save round-trips.

### 18.2 Integration tests

- `dev init` interactive flow with mocked stdin (`inquire` test utilities).
- `dev up` against fake cluster client (no real k3d).
- `dev down` / `dev list` with mocked cluster state.
- Backup/restore round-trip with mocked ServiceProvider client.
- Workspace discovery (walk-up to nearest `apprafter.workspace.cue`).
- Manifest unification error reporting (line:col references correct).

### 18.3 Smoke tests (env-gated, run manually or in dedicated CI)

- `APPRAFTER_DEV_SMOKE=1` — full `dev cluster up` + `dev up` + `dev down` + `dev cluster wipe` on real k3d. Asserts memory footprint < 1.5 GB, total wall-clock < 5 minutes for full cycle.
- `APPRAFTER_DEV_NEEDS_SMOKE=1` — Application with `needs.pg` deployed, basic SQL query succeeds.

### 18.4 Manual acceptance criteria per phase

See §22.

---

## 19. Documentation deliverables

### Phase 1B
- `docs/dev-guide/local-dev.md` — basic walkthrough, marked `experimental`.
- `docs/adr/0014-dev-mode-architecture.md` — k3d choice, kubeconfig merge, namespace strategy, deferred features.

### Phase 2B
- `docs/dev-guide/local-dev.md` — updated with `needs.*` section, real examples.
- `docs/dev-guide/needs-in-dev.md` — `needs.*` semantics in dev (persistent, lazy, override patterns).
- `docs/adr/0015-dev-mode-needs-integration.md` — one provider with tier overrides, persistent by default, destructive change semantic.
- `examples/applications/parser-with-pg.cue` — working `needs.pg` example.

### Phase 3B
- `docs/dev-guide/local-dev.md` — final version, `experimental` tag removed.
- `docs/dev-guide/dev-profile.md` — DevProfile reference (all fields).
- `docs/dev-guide/presets.md` — bundled presets reference + how to customise.
- `docs/dev-guide/diagnostics.md` — staged output interpretation.
- `docs/dev-guide/output-format.md` — JSON output schema.
- `docs/adr/0016-dev-mode-full-experience.md` — DevProfile, heuristics, presets, watch semantics, staged diagnostics.
- `examples/dev-profiles/` — one working profile per bundled preset.
- `spec.md` — new §4.13 "Dev mode" or extension of §4.1.

---

## 20. Out of scope (revisited for emphasis)

Reiterating §2 with phase context, to prevent scope creep during implementation:

- **No OS keyring / age encryption** for dev mode credentials — local plain configs, machine-scoped (deferred follow-up if real demand).
- **No remote dev environments** (Codespaces-style) — local k3d only.
- **No GUI / TUI** — CLI plus structured output.
- **No automatic IDE integration beyond optional config generation** — generating `.vscode/launch.json` and `.idea/runConfigurations/*` is opt-in; deeper integration (LSP plugin, editor extensions) is out of scope.
- **No "cloud mode"** — when AppRafter Cloud (Managed) lands, dev mode remains local. Cloud-hosted dev environments are a separate product decision.
- **No automated migration from prior dev setups** (docker-compose, kind, minikube) — users translate manually.
- **No multi-cluster dev** — exactly one local cluster per machine.

---

## 21. Implementation phasing

### Phase 1B — Minimum Viable Dev Mode (between platform Phase 1 and Phase 2)

**Purpose:** dogfood tooling for accelerating platform Phase 2 and 3 development. Marked `experimental` in user docs.

**Deliverables:**
- `apprafter dev cluster {up,down,status}` against k3d.
- Kubeconfig merge with context `apprafter-dev`.
- Basic `apprafter dev up`: discover `apprafter.cue`, render via operator, bind mount sources, rollout-restart on manifest change.
- `apprafter dev {list,down,logs}` via label selectors.
- Local cluster state file + label conventions on all dev resources.
- ADR 0014.
- Experimental-marked user guide.

**Out of scope for this phase:**
- DevProfile / DevProfileLocal (manifest is just `apprafter.cue`).
- Runtime heuristics / presets / `dev init`.
- File watcher (manifest changes require explicit re-run).
- `needs.*` (Phase 2 prerequisite).
- Persistent state (not meaningful until `needs.*` exists).
- Destructive change detection (no destructive resources yet).
- Staged diagnostic format (basic error output sufficient).
- Workspace.
- Debugger forwarding.
- **CMP sidecar in dev mode:** disabled by tier:dev overlay; not part of Phase 1B deliverables.
- **PlatformStack channel switching beyond stable:** dev defaults to stable; beta/edge channel switching can be added in a later sub-phase if needed.
- **`apprafter platform fork` in dev mode:** dev clusters can fork via the same CLI command as production; no dev-specific fork flow.
- **Offline detection in CLI:** `apprafter doctor` extension for offline detection is a UX courtesy, deferred to a follow-up Track A item.

**Sub-phase suggestion:**
- 1B.1 — Add `tier: dev` overlay to platform-stack chart (`platform-stack/cue/tiers/dev.cue`): opt-in/opt-out defaults per §12.2 table, 24h checkInterval. Inherits single-replica configuration from `tier: solo`. Test rendering via `cue cmd render --values tier=dev`.
- 1B.2 — `dev cluster up` calls bootstrap loader + creates `PlatformStack/default` with tier:dev values. Reuses Track A's bootstrap-all orchestrator and Track B's loader implementation.
- 1B.3 — `dev cluster down`, `dev cluster status` with platform-stack version line and pending-migrations section per §5.3 update.
- 1B.4 — `dev cluster wipe` (full reset).
- 1B.5 — **Opt-in `autoApprove` mechanism**: per-cluster config storage (in local cluster state file), `apprafter dev cluster config set migration.autoApprove true|false` command, `--auto-approve-migrations` flag on `dev up`, DevProfileLocal `migrations.autoApprove` field. MigrationController reads this config and gates `pending-approval → approved` transition accordingly. Default: false (production-like behaviour). When on, controller logs `[dev mode] auto-approved ...` with classification details.
- 1B.6 — Application reconciler dev-mode awareness: detect dev layers (DevProfile/DevProfileLocal presence), skip drift correction on image/command/env fields, retain drift on identity-level fields.
- 1B.7 — **`dev up` MigrationPlan blocking UX**: before resolving stage 5 (dependency resolution), check for pending MigrationPlan referencing the target application. If present, fail with actionable error showing plan details and approve command (per §3.4 update).
- 1B.8 — Basic `dev up`: discover apprafter.cue, render via operator, bind mount sources, rollout-restart on manifest change. No watch yet.
- 1B.9 — `dev {list,down,logs}` via label selectors. `dev list` includes "blocked by migration" marker on applications with pending plans.
- 1B.10 — Local cluster state file + label conventions on all dev resources.
- 1B.11 — ADR (consolidated dev-mode + Argo CD integration ADR, or reference existing ADRs 0025-0029).
- 1B.12 — Experimental-marked user guide; cross-references to spec.md §3.10 and §3.11.

**Size:** M+ (still in the ~1.5–2 weeks FT range; operator reconciler awareness в 1B.6 adds work, но tier-dev overlay (1B.1) is small CUE additions and bootstrap reuse (1B.2) is essentially configuration).

### Phase 2B — Dev Mode + Platform Services (after platform Phase 2)

**Purpose:** dev mode supports `needs.*` end-to-end. Still marked `experimental` (full DX in Phase 3B).

**Deliverables:**
- ServiceProvider deployment in `dev cluster up` (all by default, `--without` opt-out).
- ResourceClaim integration via existing operator code paths.
- `tier: dev` overlay for each integrated provider.
- Persistent PVCs (`Retain` policy).
- `dev reset` and `dev cluster wipe` commands.
- Destructive change detector (simplified: remove `needs.*`, major version upgrade only).
- `--backup-first` + `dev restore` for pg / redis / jetstream.
- ADR 0015.
- Updated user guide with `needs.*` section.

**Out of scope for this phase:**
- DevProfile (still using only `apprafter.cue`).
- Heuristics / presets.
- File watcher.
- Workspace.
- Debugger forwarding.
- Staged diagnostic format.

**Sub-phase suggestion:**
- 2B.1 — Dev-sized ServiceProvider deployment
- 2B.2 — ResourceClaim + Application with `needs.*` in dev mode
- 2B.3 — Persistent state + reset/wipe
- 2B.4 — Destructive change prompt + backup/restore
- 2B.5 — ADR + docs

**Size:** M (~1–1.5 weeks FT).

### Phase 3B — Full Dev Experience (after platform Phase 3, part of MVP)

**Purpose:** production-ready local development experience for end users. Removes `experimental` tag. Completes MVP definition together with platform Phase 3.

**Deliverables:**
- DevProfile + DevProfileLocal CRDs and CUE schemas.
- Layered manifest unification in operator.
- Heuristic runtime detection (4 stacks).
- `apprafter dev init` interactive + non-interactive.
- 4–6 bundled presets embedded in CLI.
- Build steps with cache via init containers.
- File watcher with code/manifest/restart-trigger semantics.
- Workspace support (`apprafter.workspace.cue`).
- Staged diagnostic output (all stages, hints, JSON format).
- Debugger port forwarding + optional IDE config generation.
- `dev exec` command.
- ADR 0016.
- Full user documentation, examples per preset.

**Sub-phase suggestion:**
- 3B.1 — DevProfile + layered manifests
- 3B.2 — Runtime heuristics + `dev init`
- 3B.3 — Bundled presets
- 3B.4 — Build steps with cache
- 3B.5 — File watcher
- 3B.6 — Workspace
- 3B.7 — Staged diagnostics + JSON format
- 3B.8 — Debugger forwarding + IDE configs
- 3B.9 — ADR + final docs

**Size:** L (~2–3 weeks FT). Falls inside the planned pause between platform Phase 3 and Phase 4 (managed offering research), so it does not block Phase 4 startup.

---

## 22. Acceptance summary

### Phase 1B complete when:

- [ ] `apprafter dev cluster up` succeeds on Linux + macOS hosts with Docker installed, completing in < 60s.
- [ ] `kubectl --context apprafter-dev get nodes` shows `Ready` after `dev cluster up`.
- [ ] `apprafter dev up` from a project with minimal `apprafter.cue` (Bun HTTP service from Phase 1.11 template) shows running pod, reachable via port-forward.
- [ ] `apprafter dev list` from any directory shows running apps.
- [ ] `apprafter dev down parser` from any directory stops `parser`.
- [ ] `apprafter dev cluster down` + `dev cluster up` cycle works; state files survive.
- [ ] User documentation present, marked `experimental`.
- [ ] ADR 0014 written and indexed.

### Phase 2B complete when:

- [ ] Bun template extended with `needs.pg: {}` deploys via `apprafter dev up`, connects to local CNPG instance, basic SQL query succeeds.
- [ ] Adding `needs.redis: {}` works without reconfiguration of cluster.
- [ ] `apprafter dev cluster down` + `dev cluster up` preserves DB data.
- [ ] `apprafter dev reset parser` clears DB; `dev up parser` rebuilds it empty.
- [ ] Removing `needs.pg` from manifest triggers destructive-change prompt.
- [ ] `--backup-first` creates dump file; `dev restore` reverses.
- [ ] Total memory footprint of empty cluster (operators only) < 1.5 GB.
- [ ] Documentation updated with real examples.
- [ ] ADR 0015 written and indexed.

### Phase 3B complete when:

- [ ] `apprafter dev init` correctly detects runtime in projects for each of: bun, node, rust, python, go.
- [ ] Generated `.apprafter/dev.cue` and `.apprafter/dev.local.cue` are valid CUE; `dev.local.cue` is auto-gitignored.
- [ ] Code save in a bun-generic project visible inside container in < 2s on Linux (< 5s on macOS).
- [ ] Hot reload functional for: bun, node + tsx, python + uvicorn, rust + cargo-watch, go + air. Honest restart-time documentation for each.
- [ ] `apprafter dev up --all` from monorepo workspace starts all listed services.
- [ ] Staged diagnostic output on a deliberately broken manifest shows correct stage of failure with helpful hint.
- [ ] Build cache hit: re-running `dev up` after small code change does not re-run `bun install`.
- [ ] Debugger port-forward functional with `--debugger` flag; VS Code attaches successfully.
- [ ] `apprafter dev up --format json` produces parseable JSON with documented schema.
- [ ] User documentation complete, no `experimental` markers remain.
- [ ] ADR 0016 written and indexed.
- [ ] All bundled presets have working examples in `examples/dev-profiles/`.
