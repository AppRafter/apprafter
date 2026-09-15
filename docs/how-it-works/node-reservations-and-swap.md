---
description: "Why a Tier-1 node gets control-plane headroom and host swap, why pods never swap, the kubelet version the behaviour depends on, and why the tier decides the policy."
---

# Node reservations and swap

Why a Tier-1 node is prepared the way it is. The recipe is
[Node preparation](../operator-guide/node-prep.md); none of this is needed to
run it.

Read it when `apprafter node status` reports something you did not expect, when
you are deciding whether a workload is affected, or when you are wondering why
the same treatment is not applied on a larger tier.

The decisions behind it are [ADR 0055](../adr/0055-node-swap-policy.md) (the
swap policy) and [ADR 0053](../adr/0053-resource-governance.md) (the
reservations).

## Two problems, one restart

`k3s.service` runs in `system.slice`, outside the pod cgroup, so nothing in
Kubernetes' own accounting protects it from a workload that grows. The
reservations — `system-reserved`, `kube-reserved`, `eviction-hard` — are what
give it guaranteed headroom.

Swap is the second half of the same problem, for the case the reservations
cannot cover: a transient control-plane spike that would otherwise OOM-kill
k3s or a data-critical pod. A `/swapfile` sized `min(RAM, 8Gi)` with
`swappiness=10` absorbs it.

## Pods do not swap

Swap is provisioned for the **node**, not for your workloads. `NoSwap` sets
`memory.swap.max = 0` on every pod cgroup, so no application or backend pod can
use swap. The swap cushion is available only to the k3s control plane in
`system.slice`.

This is a **non-regression** for pods: they behave exactly as they did without
swap. In particular the pod memory-eviction signal (`memory.available`) is
unchanged — eviction still fires on RAM, never on swap availability — so swap
does not soften any pod's limit or eviction behaviour.


## Kubelet version requirement (≥ 1.34)

The `NoSwap` pod behaviour needs a kubelet at **v1.34 or newer** with cgroup v2.
`node prep` gates the **swap step** on this:

- On a node **≥ 1.34 with cgroup v2**: reservations *and* swap are applied.
- On a node **below 1.34** (or without cgroup v2): the reservations are still
  applied, but the swap step is **refused with an "upgrade k3s first" hint** —
  it is never silently skipped.

Current k3s clears the gate: the dogfood cluster runs `v1.35.5+k3s1` and the
stable channel is `v1.36.3+k3s1`, both above 1.34.

## Why an existing container keeps its old setting

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
- **Application Deployments:** `apprafter app restart <name>`.


## Live state, not intended state

`apprafter node status` reads the **live** swap state (`swapon`) rather than
`/etc/fstab`. The fstab entry uses `nofail`, so a swapfile that is missing at
boot silently does not activate — and only the live reading is authoritative
about a cushion that is either there or is not.

Some fields come from the Kubernetes API (`swapBehavior`, swap capacity) and
some over SSH (`swapon`, `swappiness`, `GOMEMLIMIT`). Each is labelled with the
source that answered it, so an unreachable node degrades to a partial reading
rather than to a wrong one.

## Tier scope

This applies to **Tier-1** (single-node, kine/sqlite control plane) only. sqlite
tolerates paging, so mild swap (`swappiness=10`) is safe there. Tier-2 and above
use etcd, which is latency-sensitive — paging etcd can trigger leader churn — so
swap on those tiers is deferred and will use a stricter policy.

## See also

- [Node preparation](../operator-guide/node-prep.md) — the command that applies
  all of this, and the state table `node status` prints.
- [Right-sizing an application's requests](resources-and-autoscaling.md) — the
  other half of the same resource governance, on the pod side.
