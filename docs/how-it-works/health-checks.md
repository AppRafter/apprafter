---
description: "How health checks reach a running pod: the two probes the platform supplies that no manifest contains, why their numbers live in the operator rather than in the stored object, and what the rollout on upgrade is."
---

# Health checks on a running application

The recipe is [Health checks](../dev-guide/health-checks.md). This page is the
mechanism, and the two behaviours that are easiest to misread from the
outside. An application developer does not need any of it.

The reasoning behind each decision is [ADR
0065](../adr/0065-probes-and-jetstream-tuning.md) §1.

## The form is chosen by one field

A probe is an HTTP GET when it carries a `path` and a TCP connect when it does
not. There is no nesting to say which — the Kubernetes shape has one because
it carries five kinds of action, and this surface carries two.

Timings are whole seconds under Kubernetes' own field names. That is not
cosmetic either: the underlying check is specified in whole seconds, so a
duration string would let a manifest ask for `500ms` and the platform would
then have to refuse it. It is better that the wrong thing cannot be written
down.

## Two probes exist that no manifest contains

**A readiness probe on every exposed workload.** An application with an
`expose.port` and no declared readiness check gets a TCP connect on that port.
Nothing is guessed: the port is the manifest's own, and a TCP connect asserts
only what the `Service` in front of it already assumes.

Without it every rolling update has a window in which the new instance counts
as ready while its process is still binding, and the requests routed there in
that window fail.

**A startup probe derived from a declared liveness probe.** When a manifest
declares `liveness` and no `startup`, the platform renders a startup probe
against the same endpoint with a five-minute budget.

Its cadence is **not** taken from the liveness probe. A liveness period
answers "how quickly should a hang be noticed"; a startup budget answers "how
long may this take to come up". They are opposite questions, so inheriting the
first would mean that tightening a liveness probe — the responsible thing to
do — silently shortens the time an application is allowed to start.

A derived startup probe cannot make a working application fail. It can only
postpone the first liveness kill.

## The numbers are in the operator, not in the object

The platform's defaults — a ten-second period, a two-second timeout, three
failures — are applied when the workload is rendered. They are not defaults on
the custom resource, and this is deliberate rather than incidental: a
schema-level default is written into the stored object when it is admitted, so
it freezes at whatever the platform believed on the day that object was last
written. Revising a default would then leave every existing application
carrying the old number, invisibly, and two applications deployed a month
apart would disagree about what "the default" is.

The generator enforces this repository-wide — no default of any kind reaches a
custom resource definition — so the behaviour belongs to the operator and
moves when the operator does.

The consequence is that neither the manifest nor `kubectl get application -o
yaml` shows the effective numbers. `apprafter app status` is where they are,
and it marks the probes that came from the platform rather than from the file.

## One deviation from the Kubernetes defaults

Every timing matches Kubernetes' own except the timeout, which is two seconds
rather than one. On the smallest tier a node has one or two shared CPUs, and a
one-second budget for a request that never leaves the machine turns an
ordinary garbage-collection pause into a failed check.

This is a judgement, not a measurement, and it is recorded as one: if a
one-second timeout turns out to produce no false failures under load, the
platform takes Kubernetes' number.

## What the upgrade does

The default readiness probe changes the pod template of every application that
exposes a port, so **every such Deployment rolls once** when the operator is
upgraded. One rollout, not a repeating one.

The second-order effect is the one to expect a report about: an application
whose `expose.port` does not match the port its process listens on has been
running with a `Service` pointing at nothing, and it now shows as not ready.
Nothing broke at that moment — it was already broken, and the platform stopped
concealing it.

## Where each rule is enforced

| Rule | Enforced by |
| --- | --- |
| port range, positive periods and thresholds, `scheme` values | the custom resource definition, before anything else runs |
| `path` begins with `/` | the definition **and** the admission webhook |
| `scheme` or `headers` without a `path` | the admission webhook |
| a probe resolves a port, its own or inherited | the admission webhook |
| `successThreshold` is 1 on liveness and startup | the admission webhook |
| `timeoutSeconds` below `periodSeconds` | the admission webhook |

The duplication on `path` is intentional. The definition is the gate that
always runs; the webhook covers a cluster whose definition predates the rule,
which is exactly the gap an operator upgrade arriving ahead of its chart
opens.

## What a probe edit does not trigger

Nothing is gated. A probe change destroys no data and moves no security
boundary, so it is applied like any other spec change — its worst outcome is a
pod restart, which an image-tag change already causes without ceremony.
