---
cli-check-ignore:
  - span: "apprafter node reserve-headroom"
    reason: historical
    since: v0.2.44
    note: names the removed command so scripts calling it can be migrated
---

# Node preparation — reservations and swap

`apprafter node prep` prepares a Tier-1 node for resilient operation: it
reserves headroom for the k3s control plane and provisions host swap so a
transient control-plane spike is absorbed instead of OOM-killing k3s or a
data-critical pod. `apprafter node status` reports the resulting posture.

The design decisions and trade-offs are in
[ADR 0055](https://github.com/apprafter/apprafter/blob/master/docs/adr/0055-node-swap-policy.md)
(swap policy) and
[ADR 0053](https://github.com/apprafter/apprafter/blob/master/docs/adr/0053-resource-governance.md)
(the reservations).

> **`apprafter node reserve-headroom` was removed.** The old command that only
> applied reservations is replaced by `apprafter node prep`, which applies the
> same reservations *and* provisions swap. If you have a script that calls
> `reserve-headroom`, switch it to `node prep`.

## What `node prep` does

`node prep` connects to the node over SSH, prompts for confirmation
(`--yes` skips the prompt), and applies two things over a single k3s restart:

1. **Node reservations** (from ADR 0053) — `system-reserved`, `kube-reserved`,
   and `eviction-hard`, so `k3s.service` (which runs in `system.slice`,
   outside the pod cgroup) keeps guaranteed headroom.
2. **Host swap** — a `/swapfile` sized `min(RAM, 8Gi)` with `swappiness=10`,
   plus a kubelet drop-in (`failSwapOn: false`, `memorySwap.swapBehavior:
   NoSwap`) and `GOMEMLIMIT=2GiB` on `k3s.service`.

The command is **idempotent** — re-running it is safe. If it finds an orphaned
`/swapfile` (present but neither active nor in `/etc/fstab`) it reports and
reconciles it rather than failing.

```sh
apprafter node prep            # prompts before restarting k3s
apprafter node prep --yes      # no prompt (for scripted runs)
```

### Pods do not swap

Swap is provisioned for the **node**, not for your workloads. `NoSwap` sets
`memory.swap.max = 0` on every pod cgroup, so no application or backend pod can
use swap. The swap cushion is available only to the k3s control plane in
`system.slice`.

This is a **non-regression** for pods: they behave exactly as they did without
swap. In particular the pod memory-eviction signal (`memory.available`) is
unchanged — eviction still fires on RAM, never on swap availability — so swap
does not soften any pod's limit or eviction behaviour.

### The ~30-second k3s restart

Applying reservations and swap restarts k3s. On a single-node cluster this is a
brief (~30s) Kubernetes API outage:

- Running workloads keep running — containerd holds them through the restart.
- Argo CD may log one transient sync failure and self-heals afterwards.

The restart is why the command prompts for confirmation. If the restart does
not come back within the readiness timeout, `node prep` **rolls the whole step
back** and returns the cluster to its pre-`prep` state, so a bad run never
leaves an unstartable node.

### Kubelet version requirement (≥ 1.34)

The `NoSwap` pod behaviour needs a kubelet at **v1.34 or newer** with cgroup v2.
`node prep` gates the **swap step** on this:

- On a node **≥ 1.34 with cgroup v2**: reservations *and* swap are applied.
- On a node **below 1.34** (or without cgroup v2): the reservations are still
  applied, but the swap step is **refused with an "upgrade k3s first" hint** —
  it is never silently skipped.

Current k3s clears the gate: the dogfood cluster runs `v1.35.5+k3s1` and the
stable channel is `v1.36.3+k3s1`, both above 1.34.

### Rolling existing workloads after prep

`NoSwap` is applied by the container runtime **when a container is created**.
Containers that already existed before you ran `node prep` keep their old swap
setting until they are recreated. `node prep` closes this window immediately by
setting `memory.swap.max = 0` inline on the existing pod cgroups, and **warns**
you to roll the affected workloads so the runtime re-applies `NoSwap` cleanly on
the next start.

Roll workloads through their managed restart path — **do not
`kubectl delete` a stateful backend pod**:

- **Postgres (CNPG):** use the `cnpg.io/restart` annotation or the `cnpg`
  plugin. Deleting a CNPG primary directly triggers a failover and an unclean
  shutdown.
- **Redis (Dragonfly):** use the Dragonfly operator's restart path.
- **Application Deployments:** `kubectl rollout restart deployment/<name>`.

## Reading `node status`

`apprafter node status` reports the node's swap and reservation posture. It
reads the **live** swap state (`swapon`), not `/etc/fstab` — the fstab entry
uses `nofail`, so a swapfile that is missing at boot silently does not activate,
and only the live reading is authoritative.

```sh
apprafter node status
```

Some fields come from the Kubernetes API (`swapBehavior`, swap capacity) and
some from SSH (`swapon`, `swappiness`, `GOMEMLIMIT`). If SSH is unreachable, the
command still prints the API-derived fields and marks the SSH-derived ones
unknown, labelling each field's source.

The swap step reports one of these states:

| State | Meaning |
|-------|---------|
| Active with size | Swap is on; size and behaviour shown. |
| `eligible, not applied` | Node is ≥1.34 / cgroup v2 but swap was never provisioned — run `apprafter node prep`. |
| `ineligible (<1.34 — upgrade k3s)` | Kubelet is too old for `NoSwap` — upgrade k3s, then re-run `node prep`. |
| `swap skipped by env` | Swap was intentionally skipped at provision time (test hook). |
| `swap step failed at provision` | Bootstrap ran but the swap step failed; the node is up and cushionless. Re-run `node prep`. |
| orphaned `/swapfile` | A `/swapfile` exists but is neither active nor in fstab; `node prep` reconciles it. |

## Tier scope

This applies to **Tier-1** (single-node, kine/sqlite control plane) only. sqlite
tolerates paging, so mild swap (`swappiness=10`) is safe there. Tier-2 and above
use etcd, which is latency-sensitive — paging etcd can trigger leader churn — so
swap on those tiers is deferred and will use a stricter policy. See
[ADR 0055](https://github.com/apprafter/apprafter/blob/master/docs/adr/0055-node-swap-policy.md).

## Where to look next

- [ADR 0055](https://github.com/apprafter/apprafter/blob/master/docs/adr/0055-node-swap-policy.md)
  — node swap policy and rationale.
- [ADR 0053](https://github.com/apprafter/apprafter/blob/master/docs/adr/0053-resource-governance.md)
  — QoS, reservations, and the `reserve-headroom` → `node prep` history.
- [`platform-management.md`](./platform-management.md) — managing the platform
  stack.
- [`recovery.md`](./recovery.md) — the Hetzner rescue-mode runbook if a node is
  wedged.
