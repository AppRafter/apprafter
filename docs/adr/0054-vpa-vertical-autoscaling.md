# ADR 0054: Vertical autoscaling of application requests via VPA (InPlace)

## Status

`Accepted`

Date: 2026-08-08.

## Context

The 2.16d resource baseline (ADR 0053) gave every app container a conservative Burstable seed (`requests {cpu 25m, memory 32Mi}`, `limits {memory 512Mi}`) so no pod is BestEffort. That closed *safety* but not *right-sizing*: the 32Mi request under-provisions real workloads (the scheduler over-packs a narrow node), and a seed cannot know an arbitrary app's true footprint — only observation can. AppRafter targets a Vercel-like "it just works" experience on a single ~4GB T1 node (measured allocatable ~1958Mi after reservations), so the platform, not the user, should correct requests to observed usage.

Kubernetes in-place pod resize is GA cluster-side on the pinned k8s v1.35 (`InPlacePodVerticalScaling`), containerd is ≥2.0 (CRI `UpdateContainerResources`), cgroup v2 — so a request change can be applied to a live pod without eviction, which removes the single-node eviction objection that otherwise makes vertical autoscaling hostile to a one-replica app.

## Decision

We will adopt the **upstream Vertical Pod Autoscaler** (official `kubernetes/autoscaler` Helm chart `vertical-pod-autoscaler` 0.11.0 / VPA appVersion 1.7.1) — we do not build a recommender. The operator emits **one `VerticalPodAutoscaler` per managed app-env**; VPA owns the live-pod resources, the operator owns only the constant 2.16d seed *template*.

- **Update path (Option A):** `updatePolicy.updateMode: InPlace` (never evicts; an infeasible resize is deferred and retried — strictly better than `InPlaceOrRecreate`, which evicts a single-replica app). The operator keeps rendering the seed as the Deployment template `resources`; VPA changes the live pod via the resize subresource + CRI. These are disjoint fields — the Deployment controller rolls only on template-hash change and never reverts live-pod resources, so there is no fight. App Deployments are operator-owned children (not Argo-synced), so there is no Argo drift on resources.
- **Targeting:** `spec.targetRef` → the app's Deployment; `containerPolicies[0].containerName: "*"` (wildcard — a name mismatch would silently apply no policy; the platform renders exactly one container with no init containers, so the wildcard is safe and drift-proof); `controlledValues: RequestsOnly`; `minAllowed {cpu 25m, memory 32Mi}` (the seed floor) and `maxAllowed.memory ≤ 512Mi` (the seed limit) — these are **clamps applied to the recommender's output after its own minimum-recommendation floors**, so `minAllowed` only binds if the recommender's floor is pinned at or below it, see the second amendment; `updatePolicy.minReplicas: 1` (overrides the chart's global `--min-replicas=2`, which would otherwise silently skip every single-replica app).
- **Cluster knob (`PlatformStack.spec.resources.autoscale.mode`), three-valued, default `full`:** `full` (InPlace, bidirectional), `up-only` (InPlace + `evictionRequirements: TargetHigherThanRequests` — raises but never reclaims), `off` (`updateMode: Off` — recommender still learns and the mirror still populates; pods untouched). The operator reads the knob **with fallback to compiled-in tier defaults and never writes `PlatformStack.spec`**. `apprafter platform autoscale set|show` flips it (merge-patch).
- **Read-only mirror:** the operator reflects `VerticalPodAutoscaler.status.recommendation` (`target` + `uncappedTarget`) into `Application.status.recommendedResources` in the single status apply payload, surfaced by `apprafter app status`.
- **Pro-mode opt-out (implicit, per-env):** an app with explicit `resources` (effective spec) gets no VPA CR; the operator prunes any existing CR when the app transitions to pro-mode.
- **Chart config:** self-managed webhook cert (`certGen` + `registerWebhook: false`, the CNPG precedent — no cert-manager dependency); `admissionController.mutatingWebhookConfiguration.failurePolicy: Ignore` (chart default); `--feature-gates=InPlace=true` on updater + admission controller (in-place is an upstream **alpha** gate — off by default, and the webhook rejects in-place-mode VPA objects when it is off; **the gate was named `InPlaceOrRecreate` when this ADR was written and upstream renamed it — see the amendment**); recommender tuned for weekly peaks (`--memory-aggregation-interval=24h` ×14, `--memory-histogram-decay-half-life=168h`, `--memory-saver=true`) and pinned off upstream's own minimum-recommendation floors (`--pod-recommendation-min-memory-mb=32`, `--pod-recommendation-min-cpu-millicores=25` — **added in `0.2.56`; unset, upstream's 250Mi default made `minAllowed` unreachable, see the second amendment**); `--in-place-skip-disruption-budget=true`; syncWave -4.

Out of scope (documented follow-ups): backend right-sizing (CNPG/Dragonfly resources live on the CR, not the pod — a recommendation-only path into `ServiceProvider.spec.config`); the KEDA/HPA exclusion guard (KEDA is not shipped and `autoscale` is removed from v1alpha1 — the 2.6a KEDA subphase owns both sides of the exclusion); git/template writeback (Phase 3, MCP-agent approval-gate).

## Consequences

- Apps are right-sized to observed usage in-place without restarts; the platform reclaims idle capacity on the tight node (the `full` default) and the mirror gives per-app usage visibility for capacity study.
- **The 2.16d D2 budget-close is superseded:** apps stop requesting the 32Mi seed and request their real working set (up to 512Mi each after saturation). Node capacity planning is now driven by observed requests, not the seed. Post-saturation headroom is re-measured into `docs/measurements/`; if thin, the ADR 0053 pre-registered ladder (pull 2.16f forward → trim optional components → raise the T1 minimum) is the response.
- **Memory corrects only within `[32Mi, 512Mi]`:** under `RequestsOnly`, VPA hard-caps the request at the container's own limit, and the seed `limits.memory: 512Mi` is operator-owned + constant (and tier-invariant, because `seed_app_resources()` is a constant). An app whose working set exceeds 512Mi keeps OOMKilling while its request saturates at the ceiling — surfaced via `uncappedTarget` so it is diagnosable, not silent. Raising the ceiling is a user override or a follow-up (needs a tier-aware seed first).
- `change: requires-restart`: installing a cluster-wide mutating admission webhook on pod CREATE + an updater that mutates live pods is not a `safe` auto-sync.
- Durability is native — the recommendation lives in `status.recommendation` + a `VerticalPodAutoscalerCheckpoint`; nothing is lost on recommender restart, app-pod recreation, or node reboot (as long as the datastore survives).

## Alternatives considered

- **Build a recommender** — rejected; VPA's decaying-histogram recommender (cross-restart, percentile, memory-never-below-peak) is mature. The differentiator is the application path, not the algorithm.
- **`InPlaceOrRecreate`** — rejected (D1); it falls back to *evicting* the pod on an infeasible resize, a full outage for a single-replica app, silently breaking the RESTARTS=0 invariant. `InPlace` defers-and-retries instead.
- **`up-only` as the default** — rejected; downward reclaim is the point of the subphase, and a request stuck high after a one-off spike permanently eats capacity on the ~1958Mi node. `up-only` remains available as the between-`full`-and-`off` lever.
- **Recommendation-only + operator/git writeback into the manifest** — rejected for 2.16e; each writeback is a template change → rollout → restart, violating the acceptance, and git writeback needs a config-repo + approval-gate deferred to Phase 3.
- **A custom "start Off, flip after N minutes of history" gate** — rejected; upstream already handles cold-start (no history → no mutation; a confidence multiplier that widens bounds on thin history; a 12h pod-age gate). A wall-clock timer persisted in status lies across restarts, and the one declarative signal (`LowConfidence`) is declared but never set by the recommender.
- **cert-manager-issued webhook cert** — rejected; the self-signed `ClusterIssuer` is shipped by the admission-webhook chart at syncWave 0, so a cert-manager Certificate at wave -4 would reference a not-yet-existing issuer. Self-managed `certGen` avoids the ordering trap and the runtime-patched-`caBundle` Argo drift.

## Risks

- **In-place is an upstream ALPHA feature enabled fleet-wide by default.** Bug density is higher and semantics moved between VPA minors. Mitigation: pin the version explicitly, enable the feature gate deliberately, validate the CR is accepted in the real-Hetzner walk, and re-read the gate name on every chart bump. The `minAllowed` floor bounds the blast radius.
- **Checkpoint loss → depressed recommendation → a downward resize** (the one residual risk of the bidirectional default). Bounded by `minAllowed` (over-commit, not an outage), but it compounds on a single node where the recommender is itself OOM-killable. Mitigation: VPACheckpoint persistence is mandatory and the recommender's own `resources` block is load-bearing (measured, not guessed).
- **Silent-infeasible upward resize:** an upward resize the node cannot satisfy is retried forever with no user-visible error. Mitigation: surface it as a "recommendation not applied — node capacity" signal on `Application.status`, distinct from the `uncappedTarget` own-limit case.
- **`mode: off` is not symmetric:** it freezes live pods but the next pod recreation admits at the seed (32Mi) — a deferred, decoupled capacity change. Mitigation: `autoscale set off` warns at the point of action; the walk pins the behaviour.
- **metrics-server is an undeclared prerequisite** (the recommender reads `metrics.k8s.io`; k3s installs it by default). Mitigation: recorded here; the walk asserts `kubectl top pod` responds.
- **`vpa_available` startup probe can go stale-true** if an infra overlay disables the component while the operator runs. We accept this (the `cilium_available` precedent has the same property); a live cluster is the only thing that catches it.

## Amendment — the gate name, and the risk this ADR named coming true (2026-08-21)

This ADR's Risks section says, of in-place being an upstream alpha feature:
"semantics moved between VPA minors. Mitigation: pin the version explicitly,
enable the feature gate deliberately, **validate the CR is accepted in the
real-Hetzner walk**, and **re-read the gate name on every chart bump**."

Both mitigations were written down and neither was performed. Upstream renamed
the gate to `InPlace` — matching the `updateMode` this ADR already chose — and
VPA 1.7.1 rejects the old name by refusing to start. So from the day the
component shipped in 2.16e until 2026-08-21, the updater and the admission
controller were in `CrashLoopBackOff` on every cluster carrying it. The
recommender was unaffected: recommendations accrued, and nothing applied them.
**Vertical autoscaling has never run.**

Fixed in platform-stack `0.2.55`: one string, in two `extraArgs` lists. The
decision itself is unchanged and was correct — the CRD accepts `InPlace`
(`["Off","Initial","Recreate","InPlaceOrRecreate","InPlace","Auto"]`, read off
a live cluster), and D1's reasoning for preferring it over `InPlaceOrRecreate`
still holds.

### Why it stayed invisible for months, which is the part worth carrying forward

Three properties combined, and each is defensible alone:

- **`failurePolicy: Ignore`** on the admission webhook is correct — it stops a
  down admission pod deadlocking cluster-wide pod creation. It also means a
  dead webhook admits VPA objects unmutated instead of failing loudly.
- **The CRDs install from the chart independently of the controllers.** Every
  "is VPA installed?" probe that checks for the CRD passes while nothing runs.
  The documentation page written for this feature used exactly that check.
- **Nothing asserts that a recommendation was ever applied.** The mirror
  populates from the recommender, so `apprafter app status` shows a VPA
  recommendation on a pod whose requests have never moved.

The tell is `kubectl -n vpa get pods`, not the CRD. It is now in the component's
comment, in the compatibility note and on the operator guide.

### What would have caught it

Not a documentation gate: every identifier resolved and every command existed.
The closing walk of the documentation track found it by **reading the live
cluster while checking a page's claims** — which is also the only reason the
CRD-vs-controllers gap surfaced. The durable lesson matches the one that ADR
recorded for itself and did not act on: an alpha upstream feature needs a check
that the feature *did something*, not that its objects exist.

## Second amendment — the floors underneath the floors (2026-08-21)

The amendment above was written earlier the same day. This one is the same
component, the same day, and the same failure class: an unread upstream default
on an alpha component. The lesson the first one recorded did not prevent the
second one.

### What this ADR believed `minAllowed`/`maxAllowed` were

The Decision reads `minAllowed {cpu 25m, memory 32Mi}` as "the seed floor" and
`maxAllowed.memory ≤ 512Mi` as "the seed limit" — a band inside which the
recommendation is free to move. That is not what they are. They are **clamps
the recommender applies to its own output, after its own minimum-recommendation
floors have already been applied.** Upstream ships those floors on by default:

- `--pod-recommendation-min-memory-mb`, default **250**;
- `--pod-recommendation-min-cpu-millicores`, default **25**.

Both are registered in `pkg/recommender/config/config.go` at tag
`vertical-pod-autoscaler-1.7.1`, the version this ADR pins. Neither was set
anywhere in this repository.

Because `32 < 250 < 512`, `minAllowed: 32Mi` could not bind under any input:
every recommendation entered the clamp already at or above 250Mi, so the lower
bound had nothing left to raise. It was **structurally dead code**, shadowed by
an invisible floor 7.8× higher.

`maxAllowed: 512Mi` is a different case, and the distinction is worth stating
precisely because an earlier reading got it wrong: the ceiling still binds
whenever a recommendation exceeds it. It simply never did here, because every
recommendation sat at the floor. It was **unexercised, not inert** — the
`[32Mi, 512Mi]` correction range in the Consequences above is real, and only
its lower half was dead.

**Two of this ADR's own mitigations rested on that dead clamp**, and so were not
in force for the component's entire life: the Risks section offers "the
`minAllowed` floor bounds the blast radius" against in-place being alpha, and
against checkpoint loss "bounded by `minAllowed` (over-commit, not an outage)".
Neither could hold while `minAllowed` could not bind — a recommendation
depressed by a lost checkpoint would have landed on upstream's 250Mi, not our
32Mi. Both mitigations are true again as of `0.2.56`, and only as of `0.2.56`.

### The observable signature

On a live single-node T1 cluster, all five `VerticalPodAutoscaler` objects
reported an **identical** `250Mi / 25m` target — across workloads whose real
working sets ranged from 6Mi to 82Mi. Identical targets across a 14× spread of
actual usage are not a recommendation; they are a constant, and the constant is
the tell.

The node is why anyone looked at all: ~1952Mi of 1959Mi allocatable memory
booked in requests, a Postgres instance `Pending` for four days with no running
copy, platform infrastructure holding 86.9% of the requests against user
application code's 13.1%.

The counterfactual is what makes this the more dangerous of the day's two bugs.
Had vertical autoscaling been repaired by the first amendment's one-string fix,
with the shipped default `mode: full` and these floors still unset, every one of
those pods would have been admitted at 250Mi instead of the 32Mi seed. A VPA
targets a *Deployment*, and every pod of that Deployment is admitted at the
recommendation — so the five objects above cover five Deployments totalling
**eight pods**, because three of the five applications run two replicas.
`8 × (250 − 32) = ` **+1744Mi on a node with 7Mi free.** At a 250Mi floor the
admissible application count on this tier is zero. The fix for the first bug
would have saturated the node.

### The fix, and the coupling it creates

Both floors are now pinned on the recommender's `extraArgs` in
`platform-stack/cue/component_vpa.cue`:

- memory to **32Mi**, the 2.16d seed — the value `minAllowed` already carried,
  so the clamp becomes reachable instead of decorative;
- CPU to **25m**, which is the value it already defaults to. Pinning a current
  default deliberately: the pin is not for today's number, it is immunity from
  a silent upstream default change on the next chart bump — the failure mode
  that produced this bug and the gate-name bug both.

**The coupling this creates is the most valuable thing to record here.** The
floor is now hard-pinned in chart source, so `minAllowed` is authoritative only
**upward**. Raise it above 32Mi and it binds normally. Lower it below 32Mi —
per cluster through `PlatformStack.spec.resources.autoscale.minAllowed`, or in
the compiled-in default (`default_autoscale_config` in
`operator/operator-controllers/application/src/lib.rs`) — and the chart pin
silently overrides it. That is exactly the failure mode this change fixes,
relocated from 250Mi to 32Mi. **The two must move together:** any change to the
`minAllowed` memory floor is also an edit to `component_vpa.cue`.

### Why the CPU column is why nobody read the memory column

`--pod-recommendation-min-cpu-millicores` defaults to 25m, and the 2.16d
application seed requests 25m. So the CPU column of `kubectl get vpa` printed,
on every object, exactly what a correct system would print — and the memory
column beside it was assumed correct by association. One axis reading right is
a strong and entirely unjustified signal about the other.

### The change classification

Platform-stack `0.2.55` shipped `change: safe`. This ADR had already argued
otherwise in its Consequences — "installing a cluster-wide mutating admission
webhook on pod CREATE + an updater that mutates live pods is not a `safe`
auto-sync" — and `0.2.55` was the release in which that machinery first ran at
all. `0.2.55` is yanked. `0.2.56` carries `change: requires-restart`, which is
the classification this ADR asked for and did not get.

### What would have caught it

Not a review: `32Mi` is the right value, in the right field, and nothing about
the object or the chart reads as wrong. Not the CRD either — the object is
valid and was accepted. The only thing that distinguishes a bound clamp from a
dead one is the recommender's output, so the check has to be on the output:
that a recommendation was ever **applied**, and — the half this bug adds — that
the recommendations across a fleet are **not all identical**.

The first half was already written down. The amendment above closes with it:
"an alpha upstream feature needs a check that the feature *did something*, not
that its objects exist." It was recorded without being implemented, in the same
shape as this ADR's original "re-read the gate name on every chart bump" — a
mitigation written into a document and then not performed. That is twice in one
day on one component, which is why both halves are stated here as a check to
build rather than a lesson to remember.

## Owner

Platform team (AppRafter operator + platform-stack).

## Re-evaluation

Revisit when the VPA chart is bumped (re-confirm the in-place feature-gate name and alpha/beta/GA status, **and re-read the recommender's `--pod-recommendation-min-*` defaults** — the pins fix the behaviour, but the upstream numbers quoted in this ADR and in `component_vpa.cue` go stale silently), or at platform milestone M2 close, or if the post-saturation node-headroom measurement shows the T1 budget no longer closing.

## References

- Design spec: `docs/superpowers/specs/2026-08-08-2.16e-vpa-design.md` (folds 4 external review rounds).
- ADR 0053 (resource governance — the seed + node reservations this builds on), ADR 0044 (per-env deploy), ADR 0045 (egress profile — the read-with-fallback knob precedent), ADR 0040 (image digest rollout — orthogonal template mutation).
- Upstream: `github.com/kubernetes/autoscaler` (vertical-pod-autoscaler), the in-place-resize KEP.
