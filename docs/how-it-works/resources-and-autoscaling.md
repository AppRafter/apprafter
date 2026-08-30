---
description: "How right-sizing works: the VerticalPodAutoscaler the platform renders, why the Deployment and the pod disagree, the cluster-wide floors and ceilings, and how to tell whether the applying half is running."
---

# Right-sizing an application's requests

The recipe is [Resources and
autoscaling](../dev-guide/resources-and-autoscaling.md). This page is the
mechanism, and the cluster-side diagnosis that goes with it. An application
developer does not need any of it.

Why the starting values are what they are, and why the correction is vertical
and in place, is in [ADR 0053](../adr/0053-resource-governance.md) and [ADR
0054](../adr/0054-vpa-vertical-autoscaling.md).

## The object the platform renders

An application with **no** `resources` block gets a `VerticalPodAutoscaler`
alongside its Deployment, configured narrowly:

| Setting | Value | Consequence |
| --- | --- | --- |
| update mode | `InPlace` | The request on the running pod is changed where it stands — no eviction, no restart, no rollout. If the node cannot fit the new request the change is deferred and retried rather than forced. |
| controlled values | `RequestsOnly` | Only requests are touched. Limits — the application's own, or the platform's `512Mi` — are never rewritten. |
| controlled resources | `cpu`, `memory` | Nothing else, `ephemeral-storage` included. |
| floor | `cpu: 25m`, `memory: 32Mi` | The request is never corrected below the platform's own starting values. |
| ceiling | `cpu: 1`, `memory: 512Mi` | Never corrected above these — and for memory, never above the container's own limit either, which is the tighter bound. |
| minimum replicas | `1` | Single-replica applications are right-sized too, which is the common case. |

An application **with** a `resources` block gets no autoscaler at all, and an
existing one is removed. Writing the block is the opt-out.

## The Deployment and the pod disagree, permanently

The platform renders `32Mi` into the Deployment template; the autoscaler edits
the **pod**. They are different fields and neither reverts the other, so the
Deployment shows the starting values forever no matter how large the
application has grown.

`kubectl describe deployment` is therefore the wrong place to look. Read the
pod:

```sh
kubectl -n apprafter get pod -l apprafter.io/application=reporting \
  -o jsonpath='{.items[0].spec.containers[0].resources}'; echo
```

and the raw recommendation, including the uncapped figure, from the autoscaler
object:

```sh
kubectl -n apprafter get vpa -l apprafter.io/application=reporting \
  -o jsonpath='{.items[0].status.recommendation.containerRecommendations[0]}'; echo
```

Both selectors take the name from the manifest's `metadata.name`; replace
`apprafter` with the application's namespace.

The same consequence explains why `off` freezes rather than restores: a pod
already corrected keeps its size, and the next deploy or pod recreation puts it
back to `32Mi`, because that is what the template has always said.

## Is the applying half actually running?

"Installed" is two questions, and only the second one is load-bearing.

```sh
# 1. Is the component rendered at all?
kubectl get crd verticalpodautoscalers.autoscaling.k8s.io

# 2. Are its controllers up? This is the one that tells the truth.
kubectl -n vpa get pods
```

Expect **three** `Running` pods — recommender, updater, admission controller.
Anything less means recommendations are accruing and nothing is applying them.
The CRD check passes in exactly that state, which is why it cannot be the test.

```sh
kubectl -n vpa logs -l app.kubernetes.io/component=updater --tail=5
```

### The defect this diagnosis exists for

From the day the component shipped until platform-stack **0.2.56**, the
applying half never ran. The updater and the admission controller were passed a
feature gate named `InPlaceOrRecreate`; upstream had renamed it to `InPlace`,
and the pinned version rejects an unknown gate by refusing to start rather than
warning. Both Deployments crash-looped.

The recommender was unaffected, so recommendations were computed, written to the
`VerticalPodAutoscaler` and mirrored onto each application's status — every
reading surface worked, and reported numbers nothing was applying.

Nothing failed loudly, and that is the part worth carrying. The mutating webhook
is registered `failurePolicy: Ignore` — correct, because a down admission pod
must not deadlock pod creation cluster-wide — so a dead controller is
indistinguishable from a healthy one that had nothing to say. `kubectl -n vpa
get pods` is the only reading that separates them.

On a cluster older than 0.2.56, every autoscale mode behaves like `off`.

## The cluster-wide floors and ceilings

They live on the `PlatformStack` singleton at
`spec.resources.autoscale.minAllowed` and `.maxAllowed`, have no CLI, and are
edited on the resource:

```sh
kubectl -n apprafter-system patch platformstack default --type=merge \
  -p '{"spec":{"resources":{"autoscale":{"mode":"full","maxAllowed":{"cpu":"2","memory":"512Mi"}}}}}'
```

**Raising `maxAllowed.memory` does not raise the memory ceiling.** A request is
never lifted above the container's own memory limit, and for an application with
no `resources` block that limit is the platform's `512Mi`, fixed. The clamp
above it is unreachable. CPU is the exception — there is no CPU limit to bind
first, so `maxAllowed.cpu` really is the CPU ceiling.

Three further traps if you do edit them:

- `mode` is required on that object, so it has to travel in the same patch;
- each map replaces the built-in one wholesale, so write every key you mean to
  keep;
- **`minAllowed` is authoritative only upward.** The autoscaler applies its own
  minimum-recommendation floor before this clamp, and the platform pins that
  floor to the same `32Mi` — so raising `minAllowed.memory` above `32Mi` works,
  while lowering it below `32Mi` changes nothing and reports no error.
  Recommendations stop at `32Mi` either way.

That last one is a fix, not a quirk: the floor used to sit unpinned at the
autoscaler's own default of 250Mi, which put it *above* `minAllowed` and made
the clamp unreachable in the other direction — every application recommending an
identical 250Mi. platform-stack 0.2.56 pinned it to the seed.

## Reading a rejected manifest

When the admission webhook refuses a manifest, the message names the field and
the value — but it lands on the Argo CD Application rather than anywhere the
CLI reads today. Ask for the sync conditions by the **Argo CD application
name**, which is `<app>` for a base-only deployment and `<app>-<env>` for an
environment one:

```sh
kubectl -n argocd get application.argoproj.io reporting \
    -o jsonpath='{range .status.conditions[*]}{.type}: {.message}{"\n"}{end}'
```

The `SyncError` line wraps the webhook's own sentence.
`{.status.operationState.message}` carries the same one behind an apply-level
prefix, and is what to reach for when the conditions list is empty.
