---
description: "Why a Tier-1 node gets control-plane headroom and host swap, why pods never swap, the kubelet version the behaviour depends on, why the tier decides the policy, and what a 4 GB node holds once the reservations are taken."
---

# Node reservations and swap

Why a Tier-1 node is prepared the way it is. The recipe is
[Node preparation](../operator-guide/node-prep.md); none of this is needed to
run it.

Read it when `apprafter node status` reports something you did not expect, when
you are deciding whether a workload is affected, when you are wondering why
the same treatment is not applied on a larger tier, or when you want to know
how much a 4 GB node can run.

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

## What a 4 GB node holds {#what-a-4-gb-node-holds}

The reservations take their share of the machine before any pod is placed. On
a machine with 4 GB of RAM, which the kernel reports as 3814Mi, the 1500Mi
system reservation, the 256Mi kube reservation and the 100Mi eviction threshold
leave **1958Mi of allocatable memory**. The scheduler places a pod only while
the memory *requests* of the pods on the node, the new one included, fit
inside that figure. What the pods actually use does not enter into it.

Measured on such a machine, the requests add up like this:

| What runs | Memory requested |
| --- | --- |
| The platform: Argo CD, Cilium, cert-manager, the AppRafter operator and admission webhook, sealed-secrets, the PostgreSQL and Dragonfly operators, the vertical pod autoscaler, and the cluster's DNS and metrics server | 1120Mi |
| The shared PostgreSQL instance behind every `needs.pg` | 256Mi |
| One Dragonfly instance, behind a persistent `needs.redis` | 320Mi |
| The nightly backup runner, while it runs | 128Mi |
| Each application at the platform's default request | 32Mi |

So a 4 GB node holds the platform, the shared PostgreSQL, one Dragonfly
instance, the backup runner and **about four small applications**: 1958Mi less
the first four rows leaves 134Mi. Two such clusters, one with three
applications and one with six, some above the default request, had requested
1792Mi and 1824Mi before the backup runner, which leaves 38Mi and 6Mi once it
is placed.

It does not hold more beside the backup runner:

- **A further backend instance.** An ephemeral `needs.redis` class runs a
  second Dragonfly instance (320Mi), and `needs.jetstream` requests 384Mi.
  Neither fits at all once PostgreSQL and a persistent Dragonfly instance are
  there.
- **More applications, or larger ones.** A second environment of an
  application is a second set of its pods. An application's request can also
  rise above 32Mi when its [recommendation](resources-and-autoscaling.md) says
  so, and every such rise comes out of the same room.

A node past that point keeps running its applications, but the nightly backup
cannot start: its pod asks for 128Mi and no node has it. It is not silent.
`apprafter backup run` and `apprafter backup status` report the pod as one the
scheduler cannot place, with the scheduler's reason, and `apprafter top` shows
what is left in its `SCHEDULABLE` column. The recipe is [the backup runner's
pod cannot be scheduled](../operator-guide/backup-restore.md#runner-unschedulable).

The runner's 128Mi is measured, not guessed: restic's memory follows the size
of the repository's index and the CPUs it is allowed, not the size of the
data, so the platform holds restic to two CPUs and a 96 MiB heap target, and
limits the runner to 384Mi. The decision and the measurements are in [ADR
0053](../adr/0053-resource-governance.md#amendment-the-backup-runner-in-the-tier-1-budget-2026-09-23).

## A rollout releases before it asks

A node with no spare memory can also freeze a rollout, and the freeze is silent.

Left to Kubernetes' own default, a Deployment rolls with
`maxSurge: 25%, maxUnavailable: 25%`. Those percentages are resolved by rounding
surge **up** and unavailable **down**, so at one, two or three replicas they
become `maxSurge: 1, maxUnavailable: 0`. A zero `maxUnavailable` forbids any old
pod from terminating until its replacement is Available — the rollout has to
*acquire* capacity before it may *release* any. On a node already at its
allocatable ceiling the replacement stays `Pending`, nothing is ever released,
and the rollout sits in a stable deadlock. The old pods keep running and
serving, so the application looks healthy from the outside while every further
change to it is frozen.

For applications below four replicas the platform therefore pins the inverse —
`maxSurge: 0, maxUnavailable: 1` — so a rollout releases before it asks and
completes regardless of headroom. Four is where the default stops being a
problem on its own (it is the first count at which the 25% floor reaches one
whole pod), so from four replicas upward the Kubernetes default is left alone.

The cost is asymmetric and worth knowing:

- At **one replica** the old pod goes away before its replacement is ready,
  which leaves a brief window with nothing serving. A single replica on a single
  node has no availability guarantee to lose, and the alternative is a rollout
  that never finishes.
- At **two or three replicas** there is no such window: one pod is replaced at a
  time while the others keep serving.

An application that mounts its own disk is unaffected — it already rolls with
`Recreate`, because a read-write-once volume cannot be held by two pods at once.

## See also

- [Node preparation](../operator-guide/node-prep.md) — the command that applies
  all of this, and the state table `node status` prints.
- [Right-sizing an application's requests](resources-and-autoscaling.md) — the
  other half of the same resource governance, on the pod side.
- [Choosing the machine](../operator-guide/choosing-the-machine.md#how-much-memory)
  — the same arithmetic, when the machine is still to be picked.
