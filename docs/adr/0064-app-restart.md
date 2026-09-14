# ADR 0064: `apprafter app restart` — an explicit roll, because the platform cannot know when a rotation is finished

## Status

`Accepted` (2026-09-14).

**Retracts a recorded non-goal.** `app restart` was listed as out of scope in
three places; §Context explains why that rejection was correctly scoped and
does not cover this case. ADR for subphase 2.27 (`plan.md` §2.27).

The mechanism claims in this record were **measured** on a throwaway
`kind` v1.35 cluster (podman provider, created and destroyed), using the
actual apply payload printed from `operator-rendering` rather than a
hand-written approximation.

## Context

Environment values that reference a secret render to
`valueFrom.secretKeyRef` (`operator/operator-rendering/src/lib.rs:519-532`,
test at `:2036`). Kubernetes reads those once at pod start and does **not**
roll a Deployment when the referenced Secret changes. Nothing in the operator
compensates: the Application controller is
`Controller::new(apps).owns(claims).owns(plans)`
(`operator/operator-controllers/application/src/lib.rs:202-205`) — no Secret
watch — and the renderer emits no annotation on any object (`grep -c
annotations operator/operator-rendering/src/lib.rs` is 0), so there is no
checksum in the pod template either.

So after rotating a credential the workload keeps serving the old value until
something replaces the pods. The platform **sees** this and says so: the
controller already computes a content digest of the resolved secret material
(`env_config_digest`, `application/src/lib.rs:2052`), writes
`status.envConfig.{digest,changedAt}` at `:1081-1093`, and the CLI marks
affected pods `← old config` (`app.rs:1544`, `:1766-1793`). Visibility
shipped in 2.22c. Action never did.

The gap is admitted in the shipped corpus. `docs/operator-guide/secrets.md:180-186`
carries, since v0.2.51, a dated exemption:

```
<!-- docs: check=none reason=known-broken since=v0.2.51 — no first-class verb
     rolls a workload yet; the platform now SHOWS the drift … but cannot act on it -->
kubectl -n shop rollout restart deployment -l apprafter.io/application=checkout
```

and `app.rs:1792` — the product's own stale-pod warning — ends
*"…restarting the workload is what picks up the new one"*, pointing at a verb
that does not exist. [ADR 0040](0040-image-digest-resolution.md) independently
parked *"a first-class `apprafter app redeploy` command (the escape hatch
remains `kubectl rollout restart`)"*.

### On the prior rejection

`plan.md:4253` and `docs/superpowers/specs/2026-08-29-guides-are-recipes-design.md:329`
both list `app restart` as out of scope. Both are the **scope fence of a
documentation subphase (2.20)**, not a product ruling, and the same document
says so at `:259-264`: the owner's position is that a restart is *"normally a
symptom of bad probes, **which this case is not**"*, and whether the platform
should own a deliberate roll is *"a decision to take deliberately rather than
inside a documentation subphase."*

`docs/measurements/day2-followups.md:936-941` then retracts the objection by
name for exactly this case: *"the objection that an imperative restart verb is
usually a symptom does not apply here — a coordinated credential cutover is
not a symptom, it is the operation."*

This ADR is that deliberate decision.

## Decision

**Ship `apprafter app restart <application> [--workload <name>] [--env <e>] [--yes]`.**

It replaces the pods of the named application's workloads with a rolling
update, preserving the currently applied pod template. It changes nothing
else.

**The roll is explicit, never automatic.** The platform cannot know where a
developer's editing sequence ends. The 60-second requeue can fire between a
first and a second seal, deploying a state that was never an intended one —
`day2-followups.md:876-885` prices this as *"production Sentry with a
development Stripe key … nothing about a timer can fix this, because the
timer does not know where the developer's sequence ends."*

**Mechanism: the CLI writes a pod-template annotation via server-side apply
under a dedicated field manager, and does not touch anything else.** It does
not send a full Deployment. Measured: a partial SSA apply under
`apprafter-cli-restart` owns exactly `f:annotations` → its one key, leaving
`replicas` and the operator's image untouched, whereas `kubectl rollout
restart` registers as an `Update` claiming the whole annotations map
including its `"."` entry.

**The restart record lives on the CR, the roll lives on the Deployment.**
This follows [ADR 0059](0059-application-image-pin.md), where
`pin_manifest()` writes a CR annotation under
`APPRAFTER_CLI_PIN_FIELD_MANAGER`. The split is not stylistic: the one-time
`selector_needs_migration` delete-and-recreate (`application/src/lib.rs:1353-1366`)
drops the Deployment's annotations, which is harmless for a roll and fatal
for a record.

### Scope under the bundle model

Per [ADR 0062](0062-manifest-package-is-a-bundle.md) the positional argument
is the registration, so `app restart <application>` rolls **every** workload
in the bundle; `--workload` narrows. It prompts by default, naming what it
will touch, and refuses on a non-TTY without `--yes` — the
`rollback_to_revision` pattern (`app.rs:2732-2750`). It replaces running
pods; it does not get a free pass for being "safe".

### Three behaviours worth stating

- **A paused `MigrationPlan` does not block it.** The roll uses the already
  applied template, so it cannot push the gated change through — which is the
  only thing `day2-followups.md:951-961` asks it to respect. It prints one
  line saying so.
- **Zero replicas is refused, not reported as success.** `restore --data-only`
  sets `spec.base.replicas: 0` plus `PRE_RESTORE_REPLICAS_ANNOTATION`
  (`restore.rs:2754-2762`). Rolling zero pods and printing "restarted" is a
  false claim; the refusal names the cause.
- **Registrations whose git renders a Deployment directly are refused.** For
  the AppRafter path the Deployment is excluded from Argo CD's managed set
  twice over (below), but a raw-YAML, Helm or Kustomize registration puts the
  Deployment in `targetObjs`, Argo CD owns it, and self-heal **would** revert
  the annotation. The boundary is nearly self-enforcing — such a registration
  has no `apprafter.io/Application` in `status.resources` — but the refusal is
  explicit rather than incidental.

## Consequences

**Easier.** A credential cutover becomes one command instead of a documented
`kubectl` escape hatch. The `known-broken` exemption at
`docs/operator-guide/secrets.md:180-186` is deleted, and three other places
that print `kubectl rollout restart` stop doing so — the public corpus
shrinks. The stale-pod warning at `app.rs:1792` starts pointing at something
real. No walk currently rotates a secret (`day2-followups.md:964`); the one
this verb needs closes that hole too.

**Harder.** Users will reach for `app restart` as a remedy for failing probes
— the thing the original rejection was about. Nothing in the design prevents
it; only the output can bias against it.

**Neutral.** CLI-only. No operator change, no chart bump, no CRD change, so
it does not have to ride the pack release even though it will.

## Alternatives considered

**An automatic roll on a secret-content change** (stamp `env_config_digest`
into the pod template). Measured to work and nearly free — the digest already
exists. Rejected as a substitute for three recorded reasons and one new
measurement:

1. It is wrong for the case this verb is for. Automatic is the failure mode
   when the platform cannot see the end of the sequence (above).
2. The blast radius is unknowable to the person acting. N applications may
   reference one `secret:"thirdparty/sentry-dsn"`; re-sealing would roll all
   N, and no reverse index exists (`day2-followups.md:915-923`).
3. A per-application `rollout: manual` opt-out has no declarative home —
   secrets have no such surface (`:924-930`).
4. **New, measured:** `env_config_digest` returns `None` on empty material
   (`application/src/lib.rs:2300-2301`). When the operator re-applies a
   payload that omits its own annotation, SSA prunes the key it owns and
   bumps the generation — a **spurious roll**. Here the repository's
   "omission prunes" lesson does bite, and any such implementation must stamp
   an explicit sentinel rather than omit.

Kept as a possible opt-in follow-up for the leaked-credential case, already
deferred to Tier 2+ at `day2-followups.md:941-949`.

**A CR annotation that the operator translates into a pod-template stamp.**
Rejected: the controller returns before applying any child on `Paused`/`NoOp`
(`application/src/lib.rs:419-441`), so a restart requested during a gated
migration would report success and do nothing — the one behaviour
`day2-followups.md:954-961` explicitly forbids. It also needs the whole
operator-plus-chart release chain to do what the CLI already can.

**`redeploy` as the name** (ADR 0040's word). Rejected: it reads like a
git-revision operation and collides with `rollback`.

## Risks

**The annotation could be pruned or reverted.** Refuted by measurement, on
both paths, and worth recording because the intuition points the other way.

*Operator SSA does not prune it.* `metadata.annotations` is a granular map —
SSA ownership is per key, not per map — and the operator's fieldset contains
no `f:annotations` at all, because the renderer emits none
(`operator-rendering/src/lib.rs:639-642` builds `ObjectMeta { labels: Some(..),
..Default::default() }`). Measured over three consecutive forced re-applies
plus one real image change: both the CLI's annotation and a `kubectl`-written
one survived, and the generation stayed pinned between identical re-applies.
`PatchParams::apply(FIELD_MANAGER).force()` (`application/src/lib.rs:278`)
resolves conflicts only on fields inside its own payload. The repository's
`feedback_ssa_omit_prunes` lesson is about a manager dropping a field it
previously **owned**; the operator has never owned pod-template annotations.

*Argo CD does not revert it,* because it does not manage the Deployment —
excluded twice: gitops-engine `pkg/cache/cluster.go:1166-1170` requires
`len(o.OwnerRefs) == 0` and the operator sets an ownerReference
(`operator-rendering/src/lib.rs:614`), and `make_labels` (`:474-490`) never
writes `app.kubernetes.io/instance` or `argocd.argoproj.io/tracking-id`. The
repository's own real `status.resources` fixture (`app_open.rs:450-468`) is
`[Namespace, apprafter.io/Application]` — no Deployment.

**Mitigated:** the raw-YAML boundary above is exactly where those two
exclusions stop holding, which is why it is a refusal and not a warning.

**Accepted:** probe-remedy misuse. The output names what it touches and says
what it does not fix; that is the whole mitigation.

## Owner

Andrey Ryahovskiy.

## Re-evaluation

Revisit if the automatic-digest roll is shipped as an opt-in — at that point
the verb's role narrows to the multi-secret cutover and the deliberate roll,
and its prompt should say so. Also revisit if restart usage in the field
correlates with probe failures rather than with `status.envConfig.changedAt`
movement, which would mean the original objection had been right after all.

## References

- `plan.md` §2.27; `docs/measurements/day2-followups.md` D6 (§§876-885,
  915-930, 936-941, 951-964);
  `docs/superpowers/specs/2026-08-29-guides-are-recipes-design.md:112-116,
  259-264, 329`.
- [ADR 0062](0062-manifest-package-is-a-bundle.md),
  [ADR 0059](0059-application-image-pin.md) (the CR-annotation + dedicated
  field-manager precedent), [ADR 0040](0040-image-digest-resolution.md),
  [ADR 0046](0046-env-value-references.md),
  [ADR 0051](0051-app-scope-migration.md) (the pause this verb must survive).
- `operator/operator-controllers/application/src/lib.rs`,
  `operator/operator-rendering/src/lib.rs`,
  `cli/platform-cli/src/commands/{app.rs,app_open.rs,restore.rs}`,
  `docs/operator-guide/secrets.md`.
- gitops-engine `pkg/cache/cluster.go`; Argo CD
  `controller/cache/cache.go`, `util/argo/resource_tracking.go`.
