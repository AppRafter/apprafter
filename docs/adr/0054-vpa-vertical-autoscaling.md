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
- **Targeting:** `spec.targetRef` → the app's Deployment; `containerPolicies[0].containerName: "*"` (wildcard — a name mismatch would silently apply no policy; the platform renders exactly one container with no init containers, so the wildcard is safe and drift-proof); `controlledValues: RequestsOnly`; `minAllowed {cpu 25m, memory 32Mi}` (the seed floor); `maxAllowed.memory ≤ 512Mi` (the seed limit); `updatePolicy.minReplicas: 1` (overrides the chart's global `--min-replicas=2`, which would otherwise silently skip every single-replica app).
- **Cluster knob (`PlatformStack.spec.resources.autoscale.mode`), three-valued, default `full`:** `full` (InPlace, bidirectional), `up-only` (InPlace + `evictionRequirements: TargetHigherThanRequests` — raises but never reclaims), `off` (`updateMode: Off` — recommender still learns and the mirror still populates; pods untouched). The operator reads the knob **with fallback to compiled-in tier defaults and never writes `PlatformStack.spec`**. `apprafter platform autoscale set|show` flips it (merge-patch).
- **Read-only mirror:** the operator reflects `VerticalPodAutoscaler.status.recommendation` (`target` + `uncappedTarget`) into `Application.status.recommendedResources` in the single status apply payload, surfaced by `apprafter app status`.
- **Pro-mode opt-out (implicit, per-env):** an app with explicit `resources` (effective spec) gets no VPA CR; the operator prunes any existing CR when the app transitions to pro-mode.
- **Chart config:** self-managed webhook cert (`certGen` + `registerWebhook: false`, the CNPG precedent — no cert-manager dependency); `admissionController.mutatingWebhookConfiguration.failurePolicy: Ignore` (chart default); `--feature-gates=InPlaceOrRecreate=true` on updater + admission controller (in-place is an upstream **alpha** gate — off by default, and the webhook rejects in-place-mode VPA objects when it is off); recommender tuned for weekly peaks (`--memory-aggregation-interval=24h` ×14, `--memory-histogram-decay-half-life=168h`, `--memory-saver=true`); `--in-place-skip-disruption-budget=true`; syncWave -4.

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

## Owner

Platform team (AppRafter operator + platform-stack).

## Re-evaluation

Revisit when the VPA chart is bumped (re-confirm the in-place feature-gate name and alpha/beta/GA status), or at platform milestone M2 close, or if the post-saturation node-headroom measurement shows the T1 budget no longer closing.

## References

- Design spec: `docs/superpowers/specs/2026-08-08-2.16e-vpa-design.md` (folds 4 external review rounds).
- ADR 0053 (resource governance — the seed + node reservations this builds on), ADR 0044 (per-env deploy), ADR 0045 (egress profile — the read-with-fallback knob precedent), ADR 0040 (image digest rollout — orthogonal template mutation).
- Upstream: `github.com/kubernetes/autoscaler` (vertical-pod-autoscaler), the in-place-resize KEP.
