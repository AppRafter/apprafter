<!-- SPDX-License-Identifier: FSL-1.1-Apache-2.0 -->
# ADR 0053: resource governance (requests/limits, QoS, node reservations)

## Status

`Accepted` (2026-08-08). **§3 (node reservations) superseded in part by
[ADR 0055](0055-node-swap-policy.md)** (2026-08-11): the retrofit command is
now `apprafter node prep`, which applies these reservations *and* provisions
host swap. The reservation values and rationale below stand unchanged; only
the command name and its now-expanded scope moved.

ADR for subphase 2.16d (`plan.md` §2.16d). Records the resource-governance
model — pod QoS strategy, node reservations, and what is deferred — since
2.16e (recommendation-based right-sizing) builds on it. Ships as a
coordinated operator + admission-webhook + schema + platform-stack + CLI
release (the CLI/k3s-installer half carries the node reservations).

## Context

A live Tier-1 production observation motivated this work: the kernel OOM
killer evicted a stateful Postgres mid-write when every pod on the node was
**BestEffort** QoS. With no requests or limits set, the kernel's per-process
`oom_score_adj` for every pod sat near the top of the kill list, and a memory
spike anywhere on the node could take out an unrelated, data-critical
process. This is not a scheduling problem (the node was not overcommitted at
request time) — it is a QoS-class problem: BestEffort pods are the first
victims of node memory pressure regardless of what they are doing.

At the start of 2.16d **no** AppRafter-rendered app container, provisioned
stateful backend, or platform component set resource requests or limits, so
every pod was BestEffort. A separate but related problem: `k3s.service` runs
in `system.slice`, **outside** the `kubepods` cgroup, so per-pod requests do
not reserve any headroom for the control plane itself — a full kubepods
allocation can starve k3s.

The values here are not guesswork. A baseline-measurement walk on a real
Hetzner solo-tier node (`docs/measurements/2.16d-baseline-2026-08-08.md`)
captured per-container steady-state RSS (kubelet `/stats/summary`
`workingSetBytes`) for the full platform inventory plus a representative app
with `needs.pg` + `needs.redis`, and resolved a budget-close check (D2) on
the supported Tier-1 minimum (~4GB solo node).

## Decision

### 1. Pod QoS by workload class — measured, never BestEffort

No pod (platform or application) is BestEffort. QoS class is chosen per
workload role:

- **Stateful backends → Guaranteed** (`requests == limits`). CloudNativePG
  and Dragonfly pods get equal requests/limits so they are the *last*, not
  the first, evicted under node memory pressure — the exact failure the
  live incident showed. Guaranteed is only safe if the in-process memory
  budget is **coherent** with the cgroup limit, so the same change emits the
  matching DB parameters: CNPG `spec.postgresql.parameters.shared_buffers`
  (32MB, below the 256Mi limit) and Dragonfly `--maxmemory` (256mb, below
  the 320Mi limit), plus `ephemeral-storage`. An init container without
  resources silently demotes the whole pod, so the acceptance walk asserts
  `status.qosClass == Guaranteed` on the live backend pods, not just the CR
  spec.

- **App container + platform components → Burstable** (measured). The app
  seed is a low memory request (good scheduling + a lower `oom_score_adj`
  than BestEffort neighbours) plus a **generous-but-PRESENT** memory limit,
  and a CPU request with **no** CPU limit. Concretely: `requests {cpu: 25m,
  memory: 32Mi}`, `limits {memory: 512Mi}`. Platform components take the
  asymmetric pattern (generous memory request ≈ measured RSS, tighter memory
  limit, modest CPU request, no CPU limit) with values from the measurement
  table. An explicit `Application.spec.resources` (base or per-environment)
  is honored verbatim — the seed applies only when the field is omitted.

  The app memory limit is **present on purpose**. An *absent* limit was
  rejected: it breaks render determinism (2.4e byte-stability), Tier-2
  well-formedness (tenant apps are not node-pinned), and local↔in-cluster
  parity (2.12). A *low* limit (e.g. 128Mi) was also rejected: it
  deterministically OOMKills Node/JVM/Python runtimes. The seed is therefore
  generous-but-present. If a truly unbounded app limit is ever wanted it
  belongs in the declarative config layer, never threaded through the node.

  CPU has a request but no limit because CPU is compressible — the scheduler
  throttles under contention, it never kills — so a CPU limit only adds
  latency cliffs with no safety benefit.

### 2. No PriorityClass

We do **not** introduce a PriorityClass to protect backends. The kernel OOM
killer selects victims by `oom_score_adj`, which the kubelet derives from
**QoS class**, not from Pod priority. Pod priority affects scheduling and
*preemption* (which pod the scheduler evicts to make room), not which
process the kernel kills under live memory pressure. Guaranteed QoS already
gives the backends the lowest `oom_score_adj`; a PriorityClass would add a
knob that does not address the incident.

### 3. Node reservations (kube/system-reserved, eviction-hard, OOMScoreAdjust)

Per-pod requests cannot protect `k3s.service` because it lives in
`system.slice`, outside `kubepods`. The CLI/k3s-installer therefore reserves
node headroom directly, applied at bootstrap for new clusters via the k3s
config and re-appliable to existing clusters via a retrofit command:

- `system-reserved`: `memory=1500Mi` (covers the observed ~1.5G
  `k3s.service` footprint in `system.slice`),
- `kube-reserved`: `memory=256Mi`, `cpu=100m`,
- `eviction-hard`: `memory.available<100Mi`,
- k3s systemd unit `OOMScoreAdjust=-999` (verify the shipped unit doesn't
  already set it before adding the drop-in).

These are an independent improvement with no coupling gate — no
reservation-observation loop (a single controlled cluster; and a
`capacity − allocatable ≈ eviction-hard` default check would misfire).

> **Superseded in part by [ADR 0055](0055-node-swap-policy.md).** The retrofit
> command named `apprafter node reserve-headroom` above is replaced by
> `apprafter node prep`, which applies these same reservations *and*
> provisions host swap over one k3s restart. The reservation values are
> unchanged — only the command moved. See ADR 0055 for the swap policy and
> the `reserve-headroom` → `prep` removal.

### 4. LimitRange deferred until the Capsule policy layer

We do **not** ship a namespace `LimitRange` in 2.16d. A LimitRange becomes
mandatory only alongside a `ResourceQuota`, which arrives with the Capsule
tenant-policy layer. Until then the renderer and provisioner set resources
directly on every AppRafter-owned pod, which already satisfies the "no
BestEffort" acceptance without a cluster-default fallback.

### 5. `resources` deep-merge = MAP-of-map leaf semantics

The new `resources` field (base + per-environment, on 2.16c's
`#ApplicationEnvOverride`) deep-merges one level deeper than 2.16c's
struct-level fields: `requests` and `limits` merge **independently, key by
key** (like the `env` map), so a per-environment override of `limits.memory`
does not drop the base `limits.cpu`. This refines ADR 0044's override-wins
model; it does not need its own ADR.

### 6. Measure-first

The values baked into the code are constants derived from a real solo-node
baseline walk (`docs/measurements/2.16d-baseline-2026-08-08.md`), not a live
measurement and not guesswork. Platform requests ≈ measured RSS × 0.8;
backend Guaranteed limits ≈ load-sized (not idle RSS) with coherent DB
params; the app seed ≈ p99 small-app RSS with a generous present limit. A
pre-registered D2 budget-close ladder (pull 2.16f Argo tuning forward → trim
optional components → raise the Tier-1 minimum) was on standby; the budget
closed with headroom on the ~4GB solo node, so no ladder step was taken.

## Consequences

Positive:

- No pod is BestEffort. Stateful backends are the last evicted under node
  memory pressure — the live OOM incident cannot recur in the same form.
- `k3s.service` keeps guaranteed headroom independent of workload density.
- Backend memory limits are coherent with in-process budgets (no
  Guaranteed-but-crashlooping backend from a limit below `shared_buffers` /
  `--maxmemory`).
- The app seed is invisible to users who do not care and fully overridable
  (verbatim) by users who do (pro-mode).

Negative / neutral:

- Re-rendering adds resources to existing Deployments → a one-time pod roll.
- The node-reservation retrofit restarts k3s (~30s API outage on a
  single-node cluster; workloads survive via containerd, Argo logs a
  transient sync failure) — gated behind a confirmation prompt.
- Density on the smallest supported node is now bounded by the reservations
  + backend Guaranteed reserves; the D2 measurement confirms the ~4GB solo
  node still closes the budget with headroom.
- A generous app memory limit (512Mi) is deliberately loose — it prevents
  runtime OOMKills at the cost of not tightly capping a runaway app. That
  tightening is a user override or a future recommendation (2.16e), not a
  platform default.

## Alternatives considered

- **Leave everything BestEffort.** Rejected — it is exactly the failure
  mode the live incident exposed.
- **Backends Burstable instead of Guaranteed.** A Burstable backend with
  `request < limit` still gets a higher `oom_score_adj` than a Guaranteed
  one; under memory pressure it can be killed before less-critical pods.
  Rejected for stateful data planes.
- **App container Guaranteed.** Would require a tight, correct per-app memory
  limit the platform cannot know for an arbitrary user runtime; a wrong
  Guaranteed limit OOMKills deterministically. Burstable with a generous
  present limit is the safe default.
- **Absent app memory limit.** Rejected — breaks render determinism (2.4e),
  Tier-2 well-formedness, and local↔in-cluster parity (2.12).
- **PriorityClass for backends.** Rejected — the kernel OOM score is
  QoS-derived, not priority-derived (see Decision 2).
- **Ship a LimitRange now.** Rejected — a LimitRange without a ResourceQuota
  adds a cluster-default that the direct renderer/provisioner resources
  already make redundant; deferred to the Capsule layer where a
  ResourceQuota makes it mandatory.
- **VPA (Vertical Pod Autoscaler) for all workloads now.** Deferred to
  2.16e and constrained there — see Risks / Re-evaluation.

## Risks

- **Backend limit mis-sizing.** A too-tight Guaranteed limit crashloops the
  backend; a too-loose one wastes the small node's budget. *Mitigation:*
  limits are load-sized from the measurement (not idle RSS) with coherent DB
  params, and the acceptance walk asserts `qosClass == Guaranteed` on live
  pods (init containers included). Right-sizing feedback is 2.16e.
- **App memory limit too generous.** A runaway app can consume up to 512Mi
  before the limit bites, which on the smallest node is a meaningful slice.
  *We accept this* as the safe default (a lower limit is lethal to common
  runtimes); users override verbatim, and 2.16e will recommend per-app
  tightening.
- **Node-reservation retrofit outage.** The k3s restart is a brief
  single-node API outage. *Mitigation:* confirmation prompt describing the
  outage; workloads survive via containerd; Argo self-heals the transient
  sync failure.
- **VPA-vs-backends (2.16e non-goal, recorded here).** VPA mutates app Pods
  cleanly, but for CNPG / Dragonfly the resources live on the **CR**, not on
  the Pod — VPA reverts pod-level edits every reconcile. Therefore backend
  right-sizing in 2.16e will be **recommendation-only** into
  `ServiceProvider.spec.config`, never a direct VPA target. Recording this
  now so 2.16e does not re-derive it.

## Owner

Andrey Ryahovskiy.

## Re-evaluation

- At the Capsule policy layer: take up the deferred `LimitRange` +
  `ResourceQuota` per-namespace defaults.
- At 2.16e (right-sizing): wire VPA in recommendation mode for app Pods and a
  recommendation-only path into `ServiceProvider.spec.config` for backends
  (never a direct VPA target on the backend CR).
- At 2.16f (Argo footprint tuning): the platform-stack request table can be
  re-tightened once Argo's own footprint is reduced (Argo dominates the D2
  budget).
- If the supported Tier-1 minimum node changes: re-run the baseline walk and
  re-check the D2 budget-close.

## References

- `docs/superpowers/specs/2026-08-08-2.16d-resource-baseline-respec.md` —
  the re-spec (locked R1–R4 decisions + four implementation sites).
- `docs/measurements/2.16d-baseline-2026-08-08.md` — the real solo-node
  RSS baseline, the chosen value table, and the D2 budget-close.
- ADR 0044 — per-environment override-wins model (`resources` MAP-of-map
  merge refines it).
- ADR 0042 — Dragonfly per-DB `$N` isolation (its ~287MB structural floor
  sets the Dragonfly Guaranteed lower bound).
- `operator/operator-rendering/src/lib.rs` — app-container `resources`
  render, `effective_spec` `merge_resources`, and the app seed.
- `operator/operator-controllers/resourceclaim-provisioner/src/{cnpg,dragonfly}.rs`
  — Guaranteed backend resources + coherent DB params.
- `operator/admission-webhook/src/validator.rs` — `validate_resources`
  (quantity validity + `request <= limit`).
- `cli/cli-providers/src/hetzner_cloud/user_data.rs` +
  `cli/platform-cli/src/commands/` — k3s node reservations at bootstrap and
  the `apprafter node reserve-headroom` retrofit (the retrofit is now
  `apprafter node prep` — see ADR 0055).
- [ADR 0055](0055-node-swap-policy.md) — node swap policy; supersedes §3's
  retrofit command (`reserve-headroom` → `node prep`) and adds host swap.
- `plan.md` §2.16d — the deliverable decomposition and acceptance.
