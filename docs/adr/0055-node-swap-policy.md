# ADR 0055: node swap policy (provisioned host swap + pod NoSwap, Tier-1)

## Status

`Accepted` (2026-08-11).

ADR for subphase 2.16g (`plan.md` §2.16g). Records the node-swap policy for
Tier-1 single-node clusters: provision host swap so a control-plane memory
spike is absorbed instead of OOM-killed, while application and backend pods do
**not** swap. This supersedes in part ADR 0053 §3 — the retrofit command that
applied node reservations is now `apprafter node prep`, which also provisions
swap. Ships as a CLI + k3s-installer change (no operator, schema, or
platform-stack component change), carried by a CLI bump + a monorepo tag.

## Context

A live Tier-1 production observation motivated this work. The k3s control
plane runs in `system.slice`, **outside** the `kubepods` cgroup, and on the
supported ~4GB solo node the `k3s.service` footprint peaked at roughly **2.8G**
against roughly **788Mi** of remaining headroom. When a transient
control-plane spike exceeds that headroom on a swap-free node, the kernel OOM
killer is the only relief valve, and on a single-node cluster there is no HA
peer to fail over to — an OOM of an apiserver, kine, or a data-critical pod is
a full outage.

ADR 0053 addressed the *steady-state* budget (QoS classes, `system-reserved`,
`eviction-hard`). It did **not** give the node any elastic room for a
*transient* spike above the reservation: once `system.slice` exceeds its
budget on a swap-free node, the only outcome is a kill. This ADR adds that
elastic room in the form of host swap, deliberately scoped so that only the
node's own `system.slice` pressure is absorbed — pods keep the ADR-0053
memory-pressure semantics unchanged.

Two upstream facts frame the decision:

- Kubernetes NodeSwap reached GA in v1.30 for the node-level plumbing, and the
  `memorySwap.swapBehavior` values (`LimitedSwap`, `NoSwap`) are only honored
  by a kubelet at or above **v1.34** with cgroup v2. Below that the drop-in is
  ineffective, so the swap step must gate on kubelet version and refuse (with
  an upgrade hint) rather than silently apply an inert config.
- Upstream guidance is to keep control-plane nodes swap-free. That guidance
  assumes an HA control plane where a swapping, latency-degraded member can be
  removed. The Tier-1 single node has no such fallback (see *Alternatives
  considered*).

The observed k3s versions clear the gate: the dogfood cluster runs
`v1.35.5+k3s1` and the current stable channel is `v1.36.3+k3s1` — both above
the ≥1.34 requirement, so the Tier-1 retrofit proceeds directly with no k3s
upgrade prerequisite.

## Decision

### 1. Provision host swap on Tier-1 nodes, sized `min(RAM, 8Gi)`

The CLI/k3s-installer provisions a `/swapfile` of size `min(MemTotal, 8Gi)`
(a T1 4GB node gets 4G), enables it with `swappiness=10`, and records it in
`/etc/fstab` as `/swapfile none swap sw,nofail 0 0`. This is applied at
bootstrap for new clusters and re-appliable to existing clusters via
`apprafter node prep`.

The `min(RAM, 8Gi)` cap is deliberate: a **larger** swap area is not safer. A
bigger swap file gives the node a longer thrashing runway before the kernel
gives up, which under a runaway spike means a **longer** period of degraded,
high-latency operation before recovery — a longer outage, not a shorter one.
The cap bounds worst-case thrash time while still covering the observed spike.

### 2. Pods do NOT swap — `NoSwap` protects the node, not workloads

The kubelet drop-in sets, in k3s's managed
`kubelet.conf.d` directory:

- `failSwapOn: false` (unconditional — allows the kubelet to start on a node
  that has swap enabled),
- `memorySwap.swapBehavior: NoSwap` (only on kubelet ≥1.34 with cgroup v2).

`NoSwap` means **pod** cgroups get `memory.swap.max = 0` — no pod (app or
backend) may use swap. The elastic room is available **only** to
`system.slice` (the k3s control plane), which is exactly the process set that
the ~2.8G spike lives in and that has no `kubepods` reservation of its own.

The precise semantics — recorded here because they are easy to get wrong:

- **`NoSwap` is a non-regression for pods, not a pod benefit.** Application
  and backend pods behave exactly as they did on the swap-free node. They
  cannot swap; swap does not soften their limits or their eviction behaviour.
- **`NoSwap` does not change the `memory.available` eviction signal.** The
  ADR-0053 `eviction-hard: memory.available<100Mi` still fires on **RAM**
  availability. Swap capacity does not enter the kubelet's `memory.available`
  computation, so pod eviction is unchanged; the cushion helps only
  `system.slice` survive a spike that would otherwise OOM. (This is the point
  labelled N17 in the frozen design — the cushion is a node-survival mechanism,
  not an eviction-threshold change.)

### 3. `GOMEMLIMIT=2GiB` on `k3s.service`

A `k3s.service.d/oom.conf` drop-in sets `Environment=GOMEMLIMIT=2GiB`.

The framing matters (point P6 in the design). `GOMEMLIMIT` is a soft limit on
the **Go heap** — it makes the Go garbage collector run more aggressively as
the live heap approaches the limit. It bounds the *Go-heap component* of the
k3s footprint only; it does **not** bound cgo allocations, `mmap`'d regions,
or the sqlite/kine page cache. Those non-Go residuals are precisely what swap
covers. This gives a coherent story against ADR 0053's
`system-reserved=1500Mi`: the Go heap is throttled toward its limit, the
non-Go residual is what pushes the footprint past the reservation, and the
swap cushion absorbs that residual (the roughly ~500Mi overshoot the
reservation cannot pre-size for). It is **not** framed as a 2GiB RSS ceiling —
RSS includes the residual that `GOMEMLIMIT` cannot bound.

The exact start value is measured and tuned rather than pinned by this ADR;
acceptance is only that `GOMEMLIMIT` is set and non-empty, with the chosen
value recorded under `docs/measurements/`.

### 4. `apprafter node prep` — the retrofit umbrella; `NoSwap` at container creation

`apprafter node prep` is the idempotent umbrella command that applies node
reservations (moved in from ADR 0053's `reserve-headroom`) and the swap step
over a single k3s restart, behind an SSH connection and a confirmation prompt
(`--yes` to skip). The swap **step** is gated (not the whole command): on a
node below kubelet 1.34, or without cgroup v2, the command applies the
reservations and refuses the swap step with an "upgrade k3s first" hint —
never a silent skip.

`swapBehavior: NoSwap` is honored by the CRI **at container creation** (point
N18). Containers that already existed before `prep` ran keep
`memory.swap.max = max` until they are recreated. So `node prep` additionally
sets `memory.swap.max = 0` inline on the existing `kubepods` cgroups (a
depth-agnostic walk, tolerant of the pods/QoS/pod-level hierarchy) to close
that window immediately, and **warns** the operator to roll the affected
workloads so the CRI re-applies `NoSwap` on the next container creation.

The roll must go through the workload's managed path — **never
`kubectl delete` a CNPG primary or a Dragonfly pod**. Deleting a CNPG primary
triggers a failover and an unclean shutdown. Use the CNPG restart annotation
(`cnpg.io/restart`) / `cnpg` plugin, the Dragonfly operator's restart path, or
`kubectl rollout restart` for plain app Deployments.

### 5. `apprafter node status` — read live state, degrade gracefully

`apprafter node status` reports swap and reservation state. It reads the
**live** `swapon --show` for the active swap size — not `/etc/fstab`, because
`sw,nofail` means a swapfile that is missing at boot silently does not
activate, so fstab is not authoritative (point P9). Fields span two sources
(the kube-API and SSH), and the command degrades gracefully: it labels each
field's source and marks SSH-derived fields unknown if SSH is unreachable,
still printing the kube-API-derived fields (`swapBehavior` via the kubelet
`configz` endpoint, swap capacity from the Node object).

It distinguishes these states clearly:

- swap active with its size;
- `swap skipped by env` (the undocumented test hook was set);
- `ineligible (<1.34 — upgrade k3s)`;
- `eligible, not applied — run apprafter node prep`;
- `swap step failed at provision` (a bootstrap breadcrumb, see §7);
- an orphaned `/swapfile` (present but not active and not in fstab).

### 6. Tier fork — Tier-1 only; Tier-2+ deferred

This policy is **Tier-1 only** (kine/sqlite control-plane storage).

- **T1 (kine/sqlite) is swap-safe.** sqlite tolerates page-cache pressure and
  paging without a correctness or availability hazard, so `swappiness=10`
  (mild) is appropriate.
- **T2 (etcd) is swap-risky.** etcd is latency-sensitive: paging etcd's
  working set adds disk latency to its fsync/consensus path, which can miss a
  Raft heartbeat and trigger leader churn — trading an OOM for a different
  availability failure. Tier-2+ is therefore **deferred**, and when it is
  taken up it will use `swappiness=1` (paging only as an absolute last resort)
  plus dedicated etcd-protection research, targeted at Phase 3.1.

### 7. Applied at bootstrap (fail-soft) + via `node prep` (retrofit)

New clusters get swap at bootstrap through an `INSTALL_K3S_SKIP_START` install
flow: the k3s binary and unit are installed but not started, the version/cgroup
gate runs against the just-installed binary, the config + kubelet drop-in +
swapfile are written, and only then is k3s started. The swap-writing block is
**fail-soft**: on any error it logs, drops a breadcrumb marker (which
`node status` surfaces as `swap step failed at provision`), and does **not**
abort — k3s is started unconditionally, so a swap failure leaves a working
cushionless node, never an unstarted cluster.

The retrofit path (`node prep`) applies the same steps to an existing cluster
atomically, with a whole-step rollback if the k3s restart does not come back
(`/readyz` never returns). The rollback removes only `swapBehavior` first
(keeping `failSwapOn: false`, so the kubelet still starts with swap on),
`swapoff`s under a timeout, and only removes the swapfile if `swapoff`
succeeded — under the memory pressure that motivated swap, `swapoff` itself can
hang or fail, and the recovery must never leave a node that cannot start.

## Consequences

Positive:

- A transient control-plane spike above the ADR-0053 reservation is absorbed
  by swap instead of OOM-killing k3s or a data pod. On the single-node Tier-1
  cluster that is the difference between a latency blip and a full outage.
- Pods are unaffected — `NoSwap` is a strict non-regression, and the
  `memory.available` eviction behaviour is byte-identical to before.
- `GOMEMLIMIT` gives the Go-heap component a soft ceiling, so the swap cushion
  only has to cover the smaller non-Go residual.
- The retrofit is idempotent and re-runnable; `node status` makes the node's
  swap posture observable without SSH guesswork.

Negative / neutral:

- `node prep` restarts k3s (~30s single-node API outage; workloads survive via
  containerd; Argo CD logs a transient sync failure) — behind a confirmation
  prompt.
- A swapping node is a degraded node: while `system.slice` is paging, the
  control plane is slower. The `min(RAM, 8Gi)` cap bounds how long that
  degradation can last before the kernel gives up.
- Pre-existing containers keep `memory.swap.max = max` until rolled; `node prep`
  closes the window inline and warns, but a full guarantee needs a workload
  roll.
- A VPA in-place resize (present on every Tier-1 node since 2.16e) may rewrite
  a pod's `memory.swap.max` when it resizes; because the pod stays `NoSwap`,
  the correct value it converges to is still `0`. The acceptance walk verifies
  a forced resize does not leave a pod swap-enabled; if it reverts the inline
  value, the workload roll is mandatory rather than a window-closer.
- Because pods stay `NoSwap`, their Summary-API working-set numbers are
  unaffected, so VPA recommendations do not drift — there is **no** interaction
  with ADR 0054's VPA right-sizing.

## Alternatives considered

- **Keep the node swap-free (the upstream default for control-plane nodes).**
  Rejected for Tier-1. Upstream's guidance assumes an HA control plane, where a
  swapping, latency-degraded member can be drained and replaced. The Tier-1
  single node has no HA peer — a control-plane OOM is a full outage with no
  fallback. Given that, controlled degradation via swap is strictly better than
  a hard OOM. This is a conscious, tier-scoped deviation from the upstream
  recommendation, justified only by the absence of an HA fallback; it does not
  apply to Tier-2+ (see Decision §6).
- **Let pods swap (`LimitedSwap`).** Rejected. Pod swapping reintroduces exactly
  the memory-pressure indeterminacy ADR 0053 removed, muddies the
  `memory.available` eviction signal, and would let a runaway app degrade the
  whole node. `NoSwap` keeps the cushion scoped to `system.slice`.
- **A larger (or RAM-proportional uncapped) swap area.** Rejected — a bigger
  swap area is a longer thrash runway, i.e. a longer degraded outage, not a
  safer node. `min(RAM, 8Gi)` bounds worst-case thrash time.
- **`GOMEMLIMIT` as an RSS ceiling.** Rejected as a framing — `GOMEMLIMIT`
  bounds only the Go heap, not cgo/mmap/sqlite page cache, so it cannot cap
  RSS. It is used as a Go-heap throttle, and swap covers the non-Go residual.
- **Encrypt the swap area with a random per-boot key.** Deferred (see Risks) —
  the reachable exposure is a snapshot, not a normal decommission, and
  random-key encrypted swap adds boot-time and recovery complexity for a
  residual risk that a snapshot-time `swapoff` closes more simply.
- **Apply swap on all tiers now.** Rejected — etcd (T2+) is latency-sensitive
  and swap can induce leader churn; deferred with a stricter `swappiness=1` plan
  and dedicated research (Decision §6).

## Risks

- **Encrypted-swap residual — snapshot exposure (point N11).** The `/swapfile`
  can contain paged-out apiserver memory, including Kubernetes Secret material.
  The reachable exposure is **not** node decommission (the swapfile is on the
  node's own disk) but a **Hetzner snapshot** that copies `/swapfile` into an
  image. *Mitigation:* any future CLI snapshot flow **must** run
  `swapoff -a && rm -f /swapfile` before taking the snapshot. Random per-boot
  key encryption of swap is deferred; recorded here so the snapshot flow, when
  it lands, does not overlook it.
- **`sw,nofail` silent non-activation (point P9).** With `sw,nofail`, a
  swapfile that is absent at boot silently does not activate and does not fail
  the boot. *Mitigation:* `node status` reads the **live** `swapon`, never
  fstab, so a silently-inactive swapfile surfaces as such instead of being
  reported active from a stale fstab line.
- **Pre-existing containers keep `memory.swap.max = max` (point N18).**
  `NoSwap` applies at container creation, so containers created before `prep`
  are not covered until recreated. *Mitigation:* `node prep` sets
  `memory.swap.max = 0` inline on existing cgroups and warns the operator to
  roll the affected workloads through their managed restart path (never
  `kubectl delete` a CNPG/Dragonfly pod).
- **Retrofit outage / failed restart.** The k3s restart is a brief single-node
  API outage; a bad drop-in could stop k3s from restarting. *Mitigation:*
  confirmation prompt; whole-step rollback that removes `swapBehavior` first,
  `swapoff`s under a timeout, and only removes the swapfile if `swapoff`
  succeeded — so recovery never leaves an unstartable node.
- **VPA in-place resize reverting the inline swap value.** *We accept this* —
  the pod is `NoSwap`, so the value it converges to is `0` regardless; the walk
  verifies a forced resize does not leave a pod swap-enabled, and mandates the
  workload roll if the inline set is reverted.

## Owner

Andrey Ryahovskiy.

## Re-evaluation

- **At Phase 3.1 (Tier-2 / etcd):** take up the deferred Tier-2+ swap policy —
  `swappiness=1` plus etcd-latency-protection research; do not simply extend
  the Tier-1 `swappiness=10` policy to an etcd control plane.
- **When a CLI snapshot flow is added:** the snapshot path must
  `swapoff -a && rm -f /swapfile` first; revisit random-key encrypted swap at
  that point.
- **If the supported Tier-1 minimum node changes:** re-derive the `min(RAM,
  8Gi)` sizing and the `GOMEMLIMIT` start value against a fresh baseline.
- **If a kubelet upgrade changes `swapBehavior` semantics or the ≥1.34 gate:**
  re-verify that the drop-in still yields `NoSwap` at container creation.

## References

- `docs/superpowers/specs/2026-08-11-2.16g-node-swap-design.md` — the frozen
  design (locked decisions, the R1–R4 / N / P / Q review points, and the
  real-Hetzner acceptance walk).
- ADR 0053 — resource governance (QoS, `system-reserved`, `eviction-hard`).
  This ADR **supersedes its §3 retrofit in part**: the `reserve-headroom`
  command is replaced by `apprafter node prep`, which also provisions swap.
- ADR 0054 — VPA vertical autoscaling (no interaction: pods stay `NoSwap`, so
  Summary-API working-set and VPA recommendations are unaffected).
- ADR 0044 — per-environment override-wins model.
- `cli/cli-providers/src/hetzner_cloud/user_data.rs` — the swap builder, the
  `GOMEMLIMIT` OOM drop-in, and the `INSTALL_K3S_SKIP_START` gate → write →
  start fail-soft bootstrap flow.
- `cli/platform-cli/src/commands/` — `apprafter node prep` (umbrella,
  version/cgroup2 gate, atomic apply, whole-step rollback, inline swap.max set)
  and `apprafter node status`.
- `docs/operator-guide/node-prep.md` — the operator-facing runbook.
- `plan.md` §2.16g — the deliverable decomposition and acceptance.
