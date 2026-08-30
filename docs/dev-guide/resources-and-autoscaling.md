---
description: "How much CPU and memory an application gets — the values the platform applies when you set none, how to set your own request and limit, the in-place right-sizing that runs on your behalf, how to read what it decided, and how to switch it off."
---

# Resources and autoscaling

How much CPU and memory your application asks for, who decides it, and
how to take that decision back.

There are two modes, and which one you are in turns on a single thing:
whether your manifest carries a `resources` block.

| Your manifest | What the container asks for | Right-sizing |
| --- | --- | --- |
| **no** `resources` | the platform's values: requests `cpu: 25m`, `memory: 32Mi`; limits `memory: 512Mi`; no CPU limit | **observed and applied in place**, from platform-stack 0.2.56; see the note below if your cluster is older |
| **any** `resources` | exactly what you wrote, and nothing else | **off** for that application in that environment — no autoscaler object at all |

The second row is the part that surprises people: the block is not a
set of hints the platform refines. Writing it is how you say "I own
these numbers", and the automatic correction stops for that
application.

!!! note "On a cluster older than platform-stack 0.2.56, nothing is applied"

    Recommendations were computed and reported, but the two components that
    change a pod never started — so every reading surface on this page worked
    while nothing acted on it. `apprafter platform status` tells you which
    side of 0.2.56 your cluster is on.

    On anything earlier, read the recommendation and act on it yourself with
    an explicit `resources` block, and do not plan capacity on the assumption
    that a request will be corrected upward for you. The defect, and how to
    tell a dead controller from a healthy quiet one, are in
    [How it works](../how-it-works/resources-and-autoscaling.md#the-defect-this-diagnosis-exists-for).

The design behind the automatic half is
[ADR 0054](../adr/0054-vpa-vertical-autoscaling.md);
the values it starts from are
[ADR 0053](../adr/0053-resource-governance.md).

## If you set nothing

Every container the platform renders gets requests and a memory limit
even when your manifest is silent:

```text
requests:  cpu: 25m       memory: 32Mi
limits:                   memory: 512Mi
```

Three things follow from that, and all three are deliberate:

- **A container with no requests at all is the first thing the kubelet
  evicts** when the node runs short of memory. Nothing the platform
  deploys is in that class.
- **There is no CPU limit.** CPU is compressible — a throttled
  container is slow, not dead — so an application is free to use idle
  CPU on the node and is only guaranteed its 25m share when the node is
  busy.
- **The memory limit is 512Mi**, and it is the same on every machine
  size. It does not grow when you provision a larger node. If your
  application's working set is above it, read *[When 512Mi is the
  problem](#when-512mi-is-the-problem)* below — this is the one case
  where the default is not enough and you have to act.

The 32Mi request is a starting point, not a verdict: the platform
watches the pod and works out what it actually needs, and from
platform-stack 0.2.56 it corrects the running pod to that number in
place. On anything earlier the number is *reported* rather than applied
(the warning above): there, treat it as a measurement you act on
yourself, because the live pod keeps its 32Mi until you write a
`resources` block.

## Setting an explicit request and limit

`resources` sits beside `image` and `expose`, under `spec.base` (and
under any `spec.environments.<env>` override):

```cue
// apprafter/Application.cue
package apprafter

import v1alpha1 "apprafter.io/schemas/v1alpha1"

app: v1alpha1.#Application & {
    // No `metadata.namespace`: this manifest declares an environment
    // override, so each deployment takes the namespace passed to
    // `apprafter app add --namespace`.
    metadata: name: "reporting"
    spec: {
        base: {
            image:    "ghcr.io/my-org/reporting:1.4.0"
            replicas: 1
            expose: {
                port:    8080
                network: "internal"
            }
            resources: {
                requests: {
                    cpu:    "250m"
                    memory: "512Mi"
                }
                limits: {
                    memory: "1Gi"
                }
            }
        }
        // Staging runs the same image on a smaller footprint.
        environments: staging: resources: requests: memory: "256Mi"
    }
}
```

The rules:

- **Keys are resource names** — `cpu`, `memory`, `ephemeral-storage` —
  and **values are Kubernetes quantities**: `"250m"`, `"0.5"`, `"512Mi"`,
  `"1Gi"`. They are strings, quotes included.
- **`requests` is what the scheduler reserves for you; `limits` is where
  the kernel stops you.** Exceeding a memory limit is an OOM kill;
  exceeding a CPU limit is throttling.
- **Nothing is filled in around what you write.** The platform's values
  apply only when the merged manifest has no `resources` at all, so a
  block with `requests` and no `limits` gives you a container with no
  memory ceiling. If you take the block, take both halves.
- **Per environment, `requests` and `limits` merge key by key**, so the
  `staging` override above keeps the base `cpu` request and the base
  `limits.memory` and changes only the memory request. This is looser
  than most of the manifest — `image` and `replicas` replace outright.
- **An environment can opt out on its own.** If base has no `resources`
  and one environment does, that environment is yours to size and every
  other environment stays on the platform's values. Registering the
  same manifest as two deployments is
  [Deploying more than one environment](environments.md).

### What is checked, and where

The admission webhook rejects two things when the cluster admits your
change:

- a value that is not a quantity — `resources.limits.memory value "12x"
  must be a Kubernetes quantity (e.g. "256Mi", "100m", "1Gi")`, named on
  the field it came from (`spec.base.resources.limits.memory`);
- a request above its own limit — `requests.memory "1Gi" >
  limits.memory "512Mi"; a resource request must not exceed its limit`.

`apprafter app validate` will **not** catch either. It runs the same CUE
pipeline the cluster runs, and to CUE these maps are strings keyed by
strings — every quantity rule lives one layer later, at admission. So a
typo here surfaces as a failed sync rather than as a local error:

```sh
apprafter app validate                 # shape: passes on "12x"
```

After the push, `apprafter app status` tells you *that* it failed — the
deployment sits at `sync state: OutOfSync` and never leaves it:

```sh
apprafter app status reporting
```

```text
Application argocd/reporting
  project:       apps
  repo:          https://github.com/acme/reporting.git
  revision:      HEAD
  path:          apprafter
  destination:   reporting
  environment:   (base)
  sync state:    OutOfSync
  health:        Missing

(workload detail unavailable — app not synced yet)
```

It does not tell you *why*. The block above is everything
`apprafter app status` prints — it reads Argo CD's sync and health
summary and the workload underneath, and neither the sync conditions nor
the operation result are among them, so the rejection text appears
nowhere in it.

That text is on the Argo CD Application, which the CLI does not read today.
Reading it takes one `kubectl` command against the sync conditions —
[How it works](../how-it-works/resources-and-autoscaling.md#reading-a-rejected-manifest)
has it, with the naming rule for environment deployments.

## What the platform does when you leave it alone

An application with no `resources` gets a `VerticalPodAutoscaler`
alongside its Deployment, and it is configured narrowly. Read as
"what may change" — with the standing caveat that on anything before
platform-stack 0.2.56 the applying half is down, so nothing in the first
row happens there:

| Setting | Value | What it means for you |
| --- | --- | --- |
| update mode | `InPlace` | The request on the **running pod** would be changed where it stands. No eviction, no restart, no rollout. If the node cannot fit the new request the change is deferred and retried rather than forced. (Requires the updater, which runs from platform-stack 0.2.56.) |
| controlled values | `RequestsOnly` | Only **requests** are ever touched. Your limits — or the platform's `512Mi` — are never rewritten. |
| controlled resources | `cpu`, `memory` | Nothing else is autoscaled, `ephemeral-storage` included. |
| floor | `cpu: 25m`, `memory: 32Mi` | The platform's own starting values: the request is never corrected below them. |
| ceiling | `cpu: 1`, `memory: 512Mi` | The request is never corrected above them — and for memory, never above the container's own limit either, which is the tighter bound. See below. |
| minimum replicas | `1` | Single-replica applications are right-sized too, which is the common case here. |

Two consequences worth carrying:

**The Deployment keeps showing the starting values forever.** The
platform renders `32Mi` into the Deployment template and the autoscaler
edits the *pod*; they are different fields and neither reverts the
other. `kubectl describe deployment` is therefore the wrong place to
look — read the pod, as below. (While the updater is down — anything
before 0.2.56 — the pod reads `32Mi` too; the difference between the two
only becomes visible once the applying half runs.)

**It only runs where the autoscaler is installed**, and a cluster can have
the component rendered while the controllers that apply recommendations are
down — which looks identical from here, because a recommendation still
appears. If the numbers below never move, that is the thing to rule out:
[How it works](../how-it-works/resources-and-autoscaling.md#is-the-applying-half-actually-running)
has the two readings that separate them.

## Seeing what it decided

The reading that needs no `kubectl` is the application's own status:

```sh
apprafter app status reporting
```

Among its lines, when a recommendation exists:

```text
  VPA reco:      limits.cpu: 180m, limits.memory: 340Mi
```

Read those as **requests** — the autoscaler here only ever changes
requests, so the `limits.` prefix on that line is a mislabel in the
output rather than a second measurement. When a recommendation is being
held down by the ceiling, the line says so and names what to raise:

```text
  VPA reco:      limits.memory: 512Mi · uncapped 900Mi — raise `resources.limits.memory`
```

`uncapped` is what the application would be given if nothing capped it
— the single most useful number on this page when an application is
being OOM-killed.

**No such line at all** means one of three things, in order of
likelihood: the application has an explicit `resources` block, so it
has no autoscaler; the pod has not been observed long enough for a
recommendation to exist yet; or the autoscaler is not installed on the
cluster.

??? note "Reading the live pod instead"

    `app status` reports the recommendation. What the scheduler has actually
    reserved is on the pod, and — until the applying half runs — is still the
    starting request the Deployment asked for. Both raw readings, and why the
    Deployment permanently disagrees with the pod, are in
    [How it works](../how-it-works/resources-and-autoscaling.md#the-deployment-and-the-pod-disagree-permanently).

## When 512Mi is the problem {#when-512mi-is-the-problem}

An application whose working set exceeds `512Mi` and which has no
`resources` block will be OOM-killed and stay OOM-killed. The
recommendation will sit at `512Mi` with a larger `uncapped` figure
beside it, which is what makes the case diagnosable rather than
mysterious.

The fix is to take ownership of the numbers:

```cue
resources: {
    requests: {
        cpu:    "250m"
        memory: "768Mi"
    }
    limits: {
        memory: "1Gi"
    }
}
```

**Know the trade.** That block is also the opt-out: the application no
longer has an autoscaler, its existing one is removed, and its requests
are what you wrote until you change them. For an application that needs
more than 512Mi that is the right trade — but it is a trade, not a
free upgrade.

**Raising the cluster ceiling instead does not work for memory.** The
floors and ceilings in the table above are cluster-wide, live at
`PlatformStack.spec.resources.autoscale.minAllowed` and
`PlatformStack.spec.resources.autoscale.maxAllowed`, and have no CLI —
they are edited on the resource. But a request is never raised above
the container's own memory limit, and for an application with no
`resources` block that limit is the platform's `512Mi`, fixed. Lifting
`maxAllowed.memory` above it changes nothing. CPU is the exception:
there is no CPU limit to bind first, so `maxAllowed.cpu` really is the
CPU ceiling.

They are an operator-side knob with three sharp edges, and editing them is
covered in
[How it works](../how-it-works/resources-and-autoscaling.md#the-cluster-wide-floors-and-ceilings).
For the case on this page it is the wrong lever regardless.

## Turning the automatic behaviour off

Per application, the lever is the one above: a `resources` block, which
takes effect for that application in that environment only.

Cluster-wide, it is one command. Read the current posture first:

```sh
apprafter platform autoscale show
```

which prints the mode, the three available modes, and how to set one:

```text
Autoscale mode: full (default)
```

`full (default)` means the field has never been set and the platform is
using its default. The three modes:

| Mode | What the autoscaler does |
| --- | --- |
| `full` (default) | Corrects requests **up and down**, in place — an application that spiked once does not hold that memory forever. |
| `up-only` | Corrects **up** only. A request that has been raised is never brought back down on a running pod, so nothing is reclaimed. |
| `off` | Keeps observing and keeps reporting recommendations; changes no pod. |

From platform-stack 0.2.56 all three do exactly what the table says. On
anything earlier all three behave like `off`, because the two components
that change pods do not start (the note at the top of this page) —
setting the mode correctly is still worth doing there, since it is what
the cluster will do the moment it is upgraded.

```sh
apprafter platform autoscale set up-only
apprafter platform autoscale set off
```

An invalid mode is rejected before the cluster is contacted:

```text
× invalid autoscale mode 'bogus' (expected full|up-only|off)
```

> **`off` freezes, it does not restore.** Pods already corrected keep
> the sizes they have; the next deploy or pod recreation puts each one
> back to the platform's `32Mi` starting request, because that is what
> the Deployment template has always said. If you are switching to `off`
> to keep applications at their current sizing, give each one an
> explicit `resources` block **first** — otherwise the next push
> silently undersizes them.

The command writes the mode onto the cluster's `PlatformStack`. If you
run an infrastructure repository that declares
`PlatformStack.spec.resources.autoscale`, Git is authoritative and wins
on the next sync — change the mode there instead.

## What this does not do

- **It does not add or remove replicas.** `replicas` is a number you
  set and nothing changes it for you; there is no horizontal
  autoscaling on this platform today. Vertical means one pod getting
  bigger, not more pods.
- **It does not size platform services.** A Postgres or Redis instance
  from a `needs` declaration is sized by the platform independently of
  everything on this page.
- **It never touches limits.** Whatever bound you set — or the
  platform's `512Mi` — is exactly the bound you keep.

## Related

- [Writing Application.cue](application-cue.md) — every field of the
  manifest, and the per-environment merge rules in full.
- [Troubleshooting](../operator-guide/troubleshooting.md) — reading a
  crash-looping or unschedulable pod.
- [Platform management](../operator-guide/platform-management.md) — the
  `PlatformStack` resource the cluster-wide mode lives on.
- [ADR 0053](../adr/0053-resource-governance.md)
  and
  [ADR 0054](../adr/0054-vpa-vertical-autoscaling.md)
  — why the starting values are what they are, and why the correction
  happens in place.
