# ADR 0044: Per-environment deploy via a deploy-time, per-Application env selector

## Status

`Accepted`

Date: 2026-06-07.

## Context

The Application schema already carries `spec.environments.<env>` overrides,
and the operator already unifies a selected environment onto `spec.base`
via `effective_spec(app, env_name)` (`image`/`replicas`/`expose` replace,
the `env` map merges override-wins). But `env_name` is sourced
**cluster-wide** from the operator's `APPRAFTER_ENV` environment variable
(`apprafter-operator/src/main.rs` → `Context.env_name`), which has **no
configuration surface** — no chart value, no CLI flag, no manifest field —
and Argo CD self-heals any manual `kubectl set env`. The mechanism is
therefore inert: every Application renders `base`, and every
`environments.<env>` block is dead code. This was found during the 2.4g
walk.

The launch goal (plan.md §2.9, spec §3.1 example) is to run multiple
environments of an application — dev + staging, or dev + prod — co-resident
on Tier-1 hardware. The key realisation that reframes the work: **whether a
given deployment is "dev" or "prod" is a per-deployment runtime fact, not a
property of the manifest and not a cluster-global setting.** The same
artifact must be deployable to different clusters and/or to one cluster in
different namespaces, each deployment with its own environment.

## Decision

We will make environment selection a **deploy-time decision carried
per-Application**, replacing the cluster-wide `APPRAFTER_ENV`:

1. **`Application.spec.environment`** (optional string) selects which
   `environments.<env>` override unifies onto `base` for *this* Application
   CR. The operator's `effective_spec` reads this field; the cluster-wide
   `APPRAFTER_ENV` read and `Context.env_name` plumbing are removed. Absent
   field ⇒ `base` (unchanged behaviour).
2. The same env-agnostic manifest is deployed per environment via
   `apprafter app add --env <env>` — **one Argo CD Application per
   env-deployment** (the *scalar* model), each fully self-contained in its
   own namespace so the existing same-namespace `ownerReferences` cascade is
   unchanged. We will **not** fan one CR out across namespaces.
3. **Namespace is orthogonal to environment** — chosen freely by the user
   at `app add` (interactive: pick an existing namespace or enter a new one;
   or `--namespace`). The environment never derives or constrains the
   namespace.
4. A **soft per-cluster default environment** lives on
   **`PlatformStack.spec.defaultEnvironment`** — a CLI pre-selection
   convenience, not a hard gate; `--env` always overrides.
5. `spec.environment` is **injected by the CUE CMP** from the Argo
   Application's `spec.source.plugin.env`, keeping the source manifest
   env-agnostic; the admission webhook requires the value (when present) to
   be a declared `environments` key. The env-deployments of one logical app
   are grouped by an `apprafter.io/app=<name>` label (the Argo Application
   is named `<name>-<env>`; the AppRafter CR keeps `<name>` in each
   namespace).

In scope for launch: the scalar model, the per-CR field, and the CLI /
PlatformStack / CMP surface. Out of scope (deferred, not precluded):
single-CR multi-env fan-out, dev-mode forcing (the 1.9/2.9/3.9 dev slices),
Backstage env tabs, and a dedicated env-migration command.

## Consequences

- **+** The inert `APPRAFTER_ENV` gains a real, GitOps-native surface;
  `environments.<env>` blocks become usable.
- **+** Each env-deployment is self-contained — the current operator model
  (same-namespace `ownerReferences` cascade, RetainedClaim GC) is unchanged;
  per-env `needs` overrides yield per-env claims **for free** because claim
  generation already runs on the effective spec (`web-pg` in `web-dev` vs
  `web-pg` in `web-prod`, no collision, full data isolation).
- **+** The same artifact deploys to many clusters/namespaces; the
  service-deployment logic stays in the operator while Argo CD only syncs
  the single, env-agnostic repo.
- **−** There are N Argo Applications per logical app (one per env), grouped
  by label, rather than a single entry — the Argo UI shows N rows.
- **−** The operator keeps the env-unification logic (it already had it),
  and the running spec is computed (base ⊕ selected override), not literal
  in the CR.
- **−** The CMP image, the operator, and the CLI all change ⇒ a coordinated
  release across three publish streams.

## Alternatives considered

- **CMP-time pre-resolution (operator env-agnostic):** have the CMP unify
  and emit a concrete single-env CR, letting the operator drop env logic.
  Rejected: the override-wins merge is **custom Rust** (not pure CUE
  unification — unify is intersection and would conflict on the replace
  fields), so it would have to be re-implemented inside the CMP plugin
  (duplication + a CLI/plugin call) for a marginal transparency gain.
  Operator-time reuses the tested `effective_spec`.
- **Single-CR multi-env fan-out** (one Argo app + an array of
  `{env, namespace}`; the operator materialises N namespaces): rejected for
  launch because k8s forbids cross-namespace `ownerReferences`, so one CR
  owning children in N namespaces forces hand-managed cross-namespace child
  + claim lifecycle (finalizers) and status aggregation — meaningfully more
  operator work and risk for a single-Argo-entry UX win. Left as a clean,
  non-precluded fast-follow.
- **Cluster-global environment (status quo `APPRAFTER_ENV`):** one env per
  cluster. Rejected — it is the inert mechanism being replaced; it cannot
  express dev + prod co-resident, and "the cluster's environment" is the
  wrong unit (environment is per-deployment).
- **Imperative CLI selection not written to git:** rejected — Argo CD
  self-heals it (the exact failure mode of the inert `APPRAFTER_ENV`); the
  selector must be durable in the Argo Application (plugin env) that the CMP
  reads on every sync.

## Risks

- **Cross-stream release skew** (operator + argocd-cue-cmp image + CLI): a
  partial publish could mismatch the CMP injection against the operator
  field. *Mitigation:* the change is CRD-additive and the operator treats an
  absent `spec.environment` as `base`, so the streams are order-independent;
  bump the CMP + operator + CLI in one coordinated series.
- **Namespace / CR-name collision** — two environments of one app into one
  namespace would collide the `<name>` CR. *Mitigation:* the CLI rejects and
  guides toward distinct namespaces per env; this is a CLI concern, not a
  webhook one.
- **Removing the cluster-wide `APPRAFTER_ENV`** could in principle break a
  deployment relying on it. *Mitigation:* it is inert today (no config
  surface), so nothing effective depends on it; absent selector = `base`
  (unchanged).

## Owner

AppRafter core (Andrey Ryahovskiy).

## Re-evaluation

Revisit when either the single-CR multi-env fan-out (one Argo app; operator
materialises N namespaces) or the dev-mode slices (1.9/2.9/3.9) are pulled
into scope — both build directly on this per-CR `spec.environment` selector.

## References

- Design: `docs/superpowers/specs/2026-06-07-2.9-per-env-deploy-design.md`
- `plan.md` §2.9; memory `project_env_model` (the inert `APPRAFTER_ENV`
  finding).
- Reuses the rendering pipeline (1.9) and the needs/claim machinery
  (ADR 0042 / ADR 0043) unchanged.
