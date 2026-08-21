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
| **no** `resources` | the platform's values: requests `cpu: 25m`, `memory: 32Mi`; limits `memory: 512Mi`; no CPU limit | **observed and reported**; see the note below — the *applying* half does not run on the shipped release |
| **any** `resources` | exactly what you wrote, and nothing else | **off** for that application in that environment — no autoscaler object at all |

The second row is the part that surprises people: the block is not a
set of hints the platform refines. Writing it is how you say "I own
these numbers", and the automatic correction stops for that
application.

!!! warning "Before platform-stack 0.2.55, right-sizing observed but never applied"

    Check which side of this you are on before reading further:

    ```sh
    kubectl -n vpa get pods          # all three Running → it applies
    apprafter platform status        # the platform version this cluster is on
    ```

    The autoscaler ships in three parts. The **recommender** always ran:
    recommendations were computed, written to the `VerticalPodAutoscaler`
    and mirrored onto your application's status, so everything under
    [Seeing what it decided](#seeing-what-it-decided) worked. The
    **updater** and the **admission controller** — the two that actually
    change a pod — did not start at all. They were passed a feature gate
    named `InPlaceOrRecreate`; upstream renamed it to `InPlace`, and the
    pinned version rejects an unknown gate by refusing to start rather
    than warning. Both Deployments crash-looped from the day the component
    shipped.

    Nothing failed loudly, and that is the part worth remembering. The
    mutating webhook is registered `failurePolicy: Ignore` — correct, so a
    down admission pod cannot deadlock pod creation cluster-wide — so a
    dead webhook admits pods unchanged instead of rejecting them. The CRDs
    install from the chart independently of the controllers, so every
    "is the autoscaler installed?" check that looks for the CRD passed the
    whole time. Pods were created with the Deployment's seed request and
    kept it, beside a recommendation saying otherwise.

    **On 0.2.55 and later** the updater applies recommendations in place,
    without eviction — see
    [What the platform does when you leave it alone](#what-the-platform-does-when-you-leave-it-alone).
    **On anything earlier**, read the recommendation and act on it yourself
    with an explicit `resources` block, and do not plan capacity on the
    assumption that a request will be corrected upward for you.

The design behind the automatic half is
[ADR 0054](https://github.com/apprafter/apprafter/blob/master/docs/adr/0054-vpa-vertical-autoscaling.md);
the values it starts from are
[ADR 0053](https://github.com/apprafter/apprafter/blob/master/docs/adr/0053-resource-governance.md).

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
watches the pod and works out what it actually needs. On the shipped
release that number is *reported* rather than applied (the warning
above), so treat it as a measurement you act on — the live pod keeps
its 32Mi until you write a `resources` block.

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

That text is on the Argo CD Application, and `kubectl` is what reads it
today. Point it at the cluster with `apprafter kubeconfig` if you have
not already, then ask for the sync conditions — by the **Argo CD
application name**, which is `<app>` for a base-only deployment and
`<app>-<env>` for an environment one:

```sh
kubectl -n argocd get application.argoproj.io reporting \
    -o jsonpath='{range .status.conditions[*]}{.type}: {.message}{"\n"}{end}'
```

The `SyncError` line it prints wraps the webhook's own sentence — the one
quoted above, naming the field and the value: `resources.limits.memory
value "12x" must be a Kubernetes quantity …`.
`{.status.operationState.message}` carries the same sentence behind the
apply-level prefix, and is the one to reach for when the conditions list
is empty.

## What the platform does when you leave it alone

An application with no `resources` gets a `VerticalPodAutoscaler`
alongside its Deployment, and it is configured narrowly. Read as
"what may change" — with the standing caveat that the applying half is
down on the shipped release, so today nothing in the first row happens:

| Setting | Value | What it means for you |
| --- | --- | --- |
| update mode | `InPlace` | The request on the **running pod** would be changed where it stands. No eviction, no restart, no rollout. If the node cannot fit the new request the change is deferred and retried rather than forced. (Requires the updater, which runs from platform-stack 0.2.55.) |
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
look — read the pod, as below. (While the updater is down the pod reads
`32Mi` too; the difference between the two only becomes visible once
the applying half runs.)

**It only runs where the autoscaler is installed** — and "installed"
is not one question but two. The CRD being present says the component
was rendered; it says nothing about whether the three controllers are
up. Ask both:

```sh
# 1. Is the component installed at all?
kubectl get crd verticalpodautoscalers.autoscaling.k8s.io

# 2. Are its controllers actually running? This is the load-bearing one.
kubectl -n vpa get pods
```

The second command is the one that tells the truth, and the first is the
trap. Before platform-stack 0.2.55 the `…-recommender` pod is `Running`
while the `…-updater` and `…-admission-controller` pods sit in
`CrashLoopBackOff` — the gate defect in the warning at the top of this
page. The CRD check passes in exactly that state and always did, which
is why it cannot be the test. Expect three `Running` pods; anything less
means recommendations are accruing and nothing is applying them.

```sh
# Why a controller is down, when it is. The label is stable; the
# Deployment name carries the Helm release prefix.
kubectl -n vpa logs -l app.kubernetes.io/component=updater --tail=5
```

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

To see what is actually in effect right now — the request on the live
pod, which is what the scheduler reserves for it (and which, until the
updater runs, is still the `32Mi` the Deployment asked for):

```sh
kubectl -n apprafter get pod -l apprafter.io/application=reporting \
  -o jsonpath='{.items[0].spec.containers[0].resources}'; echo
```

And the raw recommendation, including the uncapped figure, from the
autoscaler object itself:

```sh
kubectl -n apprafter get vpa -l apprafter.io/application=reporting \
  -o jsonpath='{.items[0].status.recommendation.containerRecommendations[0]}'; echo
```

Both selectors take the name from your manifest's `metadata.name`.
Replace `apprafter` with the namespace the application deploys to if it
is not the default one.

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

If you do edit them, two traps: `mode` is required on that object, so
send it in the same patch; and each map replaces the built-in one
wholesale, so write every key you mean to keep.

```sh
kubectl -n apprafter-system patch platformstack default --type=merge \
  -p '{"spec":{"resources":{"autoscale":{"mode":"full","maxAllowed":{"cpu":"2","memory":"512Mi"}}}}}'
```

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

On the shipped release all three behave like `off`, because the two
components that change pods do not start (the warning at the top of
this page). The mode is still worth setting correctly — it is what the
cluster will do the moment that is fixed — but changing it today
changes nothing you can observe.

```sh
apprafter platform autoscale set up-only
apprafter platform autoscale set off
```

An invalid mode is rejected before the cluster is contacted:

```text
× invalid autoscale mode 'bogus' (expected full|up-only|off)
```

> **`off` freezes, it does not restore.** (Read this for the day the
> applying half runs; with no pod ever corrected, there is nothing to
> freeze today.) Pods already corrected keep
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
- [ADR 0053](https://github.com/apprafter/apprafter/blob/master/docs/adr/0053-resource-governance.md)
  and
  [ADR 0054](https://github.com/apprafter/apprafter/blob/master/docs/adr/0054-vpa-vertical-autoscaling.md)
  — why the starting values are what they are, and why the correction
  happens in place.
