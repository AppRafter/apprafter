# ADR 0052: application-migration security axis — additive/escalation gating and structural hardening

## Status

`Accepted` (2026-08-06).

ADR-first for subphase 2.16b-sec (`plan.md` §2.16b-sec — "App-migration
security axis"). It extends the application-scope destructive classifier
established in ADR 0051 along a second, orthogonal axis, and closes three
structural gaps that let the gate itself be disarmed. It ships as an
operator + admission-webhook release plus the `spec.md` §3.8 actualization;
it adds two CRD fields to `MigrationPlan` (a `spec.risks.classifications[]`
rollup and a `spec.changes[]` drill-in) and one Argo CD health-Lua extension,
but introduces no new CRD.

## Context

ADR 0051 enabled application-scope destructive-change detection. Its threat
model is the **availability / data-loss** axis: a *removal* or *reduction*
(dropping a `needs.*` dependency, scaling to zero, removing a public route,
removing an env reference) breaks a running service, so it is gated behind
explicit approval. Every trigger in ADR 0051 fires on something being **taken
away**.

That axis is the correct model for an operator editing their own manifest by
hand. It is the **wrong** model for the threat that motivates the gate in the
first place (ADR 0036, MCP agentic-safety): an actor — human or automated
agent — who has gained **write access to the manifest** and wants to exfiltrate
data or widen the blast radius. Such an actor does not remove things. They
**add and escalate**:

- add a `secret:` env reference to read a credential the workload never needed,
- flip `expose.network` from `internal` to `public`,
- add a public hostname or retarget the public port to expose an internal
  admin/debug surface,
- relax `imagePolicy.resolve` from `off` back to `digest` so a mutated tag can
  serve different content without a further manifest edit,
- point the image at a different repository entirely.

Under ADR 0051 the entire "soft" column is soft **precisely because it is
additive** — and additive is the attack direction. So the same property that
keeps approver fatigue low on the availability axis leaves the security axis
completely ungated.

A parallel review also confirmed three structural gaps that let the gate be
disarmed *regardless of the trigger set*:

1. **Approval was transferable across a spec change.** The consume-time match
   compared only the `(trigger_type, field)` tuple, never the `from`/`to`
   content. So an approval for a benign-looking `replicas 2→0` could be
   consumed against a *different* `replicas 1→0`, or — for a security-boundary
   op — an approval of a benign `from→to` could be consumed after the payload
   was swapped.
2. **`Application.status` was unprotected.** The admission webhook registered
   `applications` but omitted `applications/status`. Any subject holding
   `patch applications/status` RBAC could rewrite `status.lastAppliedSpec` to
   `{}`, making the very next edit diff against an empty baseline and disarm the
   gate silently.
3. **`spec.environment` was mutable on UPDATE.** A single UPDATE flipping
   `spec.environment` swaps the *entire* effective spec (image / replicas /
   expose / env / needs all resolve through the per-environment override), so
   the environment field itself is a laundering vector.

## Decision

Extend the application-migration classifier with a **security axis** and close
the three structural gaps. Concretely:

### 1. A new class `security-boundary` (severity 4)

Add `security-boundary` to the migration classification vocabulary, ranked
**above** `data-migration` (severity 3 > `breaking` 2 > `requires-restart` 1 >
`safe` 0). The ordering is deliberate and settled: a data-loss change is
*coverable* (restic backups can recover the data, ADR 0050), but a leaked
credential or a widened exposure is **irreversible and uncoverable** — a leaked
secret cannot be un-leaked. When a single edit carries several destructive ops,
`security-boundary` therefore wins the plan's primary headline.

`image-path-change` (moving the image to a different repository / pull source)
is **reclassified** from `requires-restart` to `security-boundary`: a different
repository can serve entirely different content, so it is a pull-source /
content-provenance change, not a mere restart.

### 2. Seven additive / escalation triggers (all `security-boundary`)

All seven fire on an **addition or escalation** over the effective spec, and all
gate for approval (none is a webhook reject). `from`/`to` carry only string
sentinels — never a literal env value (see §1.4 below).

| # | `type` | `field` | condition | `from` → `to` |
|---|---|---|---|---|
| 7 | `env-secret-ref-add` | `env.<KEY>` | key absent in baseline, present in new, value is a `secret:` ref (NOT `claim.*`) | `"(absent)"` → `"secret:name/key"` |
| 8 | `env-ref-downgrade` | `env.<KEY>` | key on both sides; baseline is a `Ref`, new is a literal | `"claim.pg.url"`\|`"secret:n/k"` → `"(literal)"` |
| 9 | `env-secret-ref-retarget` | `env.<KEY>` | key on both sides; both `secret:` but different | `"secret:a/k"` → `"secret:b/k"` |
| 10 | `network-visibility-escalation` | `expose.network` | old effective `!= "public"` && new effective `== "public"` | `"internal"` → `"public"` |
| 11 | `public-hostname-add` | `expose.hostname` | new effective is public && the public hostname set gains a member | `"(none)"` → `"<host>"` |
| 12 | `public-port-retarget` | `expose.port` | new effective is public && the port differs | `"8080"` → `"9090"` |
| 13 | `image-policy-relaxation` | `imagePolicy.resolve` | old `resolve == "off"` && new `!= "off"` && the effective image is not already a digest | `"off"` → `"digest"` |

Deliberate carve-outs, so the security axis does not create approver fatigue on
benign edits:

- **#7 is secret-only.** A `claim.*` ref is self-scoped (ADR 0046 — the value is
  provider-derived and namespace-bound), so the common `claim.pg.url` flow is
  never an exfiltration primitive and is not gated.
- **#10 and #11 co-fire** under the webhook's bidirectional hostname↔public
  coupling (an `internal → public` flip forces a hostname add). The rollup
  (§3) carries both in `changes[]`; they are deliberately **not merged**,
  because a hostname can also be added to an already-public app without an #10
  escalation.
- **#12 is public-only.** A port change on a non-public app is inert externally;
  the risk is exposing a debug / metrics / actuator port that dumps environment
  (i.e. credentials).
- **#13 is conditional.** An already-digest image is a no-op (`off` and `digest`
  both render the verbatim digest); `digest → off` is *hardening* (pinning the
  tag), which stays soft.

### 3. A rollup: `spec.risks.classifications[]` + `spec.changes[]` (anti-laundering)

ADR 0051's `pick_primary` still selects one headline `spec.trigger` (backward
compatible). Two additive fields now record the **full** blast radius so a
dangerous op cannot hide behind a benign-looking primary:

- `spec.risks.classifications[]` — the distinct classification classes present
  across every detected candidate, sorted severity-descending then name-ascending.
- `spec.changes[]` — every detected candidate, each carrying `type`, `field`,
  `classification`, `severity`, `from`, `to`. (The wire field for the trigger
  kind is `type`; the Rust struct field `MigrationChange.trigger` is
  `#[serde(rename = "type")]`, so drill-in consumers — the Argo health-Lua,
  the CLI — read `.spec.changes[*].type`.)

The rollup structurally defeats approve-laundering: classes no longer compete
for a single slot, so an attacker cannot ride a `security-boundary` op along a
`data-migration` primary and have it disappear from the plan. The approval
content hash (§4) covers the whole `changes[]` set, so attaching any extra op
changes the hash and re-gates.

### 4. S-4 — approval bound to a content hash

Stamp `spec.trigger.approvedSpecHash` at plan creation: a stable SHA-256 over
the **full candidate set** (each change's `type|field|from|to`), collision-free
(the hash input is length-prefixed / structured JSON, not a naive separator
join). At consume time the reconciler recomputes the hash of the currently
detected change set; if it differs from the plan's stored hash the completed
plan is demoted to a relic and the edit is **re-gated** as a fresh
pending-approval plan. This binds an approval to the *exact* change it was
approved for — approve-X-can-no-longer-apply-Y — and re-gates on any drift.

An app-scope plan **requires a non-empty `approvedSpecHash` to consume**: a
missing or empty stamped hash is treated as *no match* and re-gates, never
applies. App-scope migration is brand-new and was never shipped without the
hash, so there are no legacy hashless plans to protect — and treating a
hashless completed plan as consumable would hand a forger a free bypass. The
operator's `plan_state`/`plan_hash_matches` therefore bucket a completed,
trigger-matching plan with no hash as a `Relic` (re-gate), and `warn!`-log it
as a forged or pre-2.16b-sec artifact. So a forged or pre-2.16b-sec hashless
plan can **never** apply a destructive change.

The approval hash binds the **gated candidate-set** (the `type|field|from|to`
of each detected *gated* change), **not the full effective spec**; soft
(non-gated) changes may accompany the execution of an approved plan and do not
trigger a re-gate — by definition a soft change applies without approval, so
there is nothing to bind. Binding a soft change into the hash would only
manufacture spurious re-gates without adding any security guarantee.

### 5. S-1 — `Application.status` write protection

Add `applications/status` to the applications admission rule, and in the
validator reject any status-subresource write whose SSA fieldManager is not
`apprafter-operator` (mirroring the `migrationplans/status` protection).

Recorded authority model (**S-2**): the SSA fieldManager is an **ownership
label, not an authentication token** — it says "the operator owns this field",
not "this request is the operator". Its integrity therefore rests on RBAC:
`patch applications/status` must be scoped to the operator's ServiceAccount, so
that no other subject can present that fieldManager. A **manifest-write or
status-write credential must NOT equal approve authority.** The authoritative
approve path is the Kubernetes `kubectl patch migrationplan … --subresource=status`
(→ `phase: approved`), governed by RBAC on the MigrationPlan status subresource;
the Argo CD resource-action "Approve" button is a convenience layered on top of
Argo CD's own RBAC. Git write access alone must never approve a plan.

### 6. §7.2 — `spec.environment` immutable on UPDATE

Thread the old object into the Application validator on UPDATE and reject any
change to `spec.environment` (`spec.environment is immutable on UPDATE (was
'X', cannot change to 'Y')`). Changing the environment is a *different*
deployment (`<name>-<env>` per ADR 0044), not an edit to an existing CR — so
this closes the "flip the environment to swap the entire effective spec"
laundering vector at the webhook, before the classifier ever runs.

### 7. A needs.selector multi-provider tripwire

A `needs.*.selector` change stays soft (ADR 0051 — a single integrated provider
is the only option at launch), but it becomes *dangerous* the moment a second
`ServiceProvider` of the same type exists (a selector flip then moves data and
data-residency). The pure classifier must not read the cluster, so the tripwire
lives in the application controller: on a soft `needs.*.selector` change, it
best-effort lists `ServiceProvider`s of that type; if more than one is
registered it emits a loud error log, a Warning Kubernetes Event, and increments
`apprafter_soft_destructive_total`. It is list-failure tolerant (never fails the
reconcile). This is a coded tripwire, not a new classifier trigger — the gate
decision for a genuine multi-provider selector migration is deferred to a
separate plan-item (ADR 0051 already flagged the re-evaluation).

### 8. Soft-destructive observability

Three soft ops — `needs.*.selector` change, env-literal removal, and scale-down
N→M — now emit an `EventType::Warning` Kubernetes Event (they were
`EventType::Normal`); every soft op increments a new
`apprafter_soft_destructive_total{trigger,namespace}` counter. A separate
`apprafter_claim_retained_total{backend,namespace}` counter ends the previously
silent `RetainedClaim` creation (review S-3 observability; the *gate* for the
reattach-by-name adoption vector is a separate plan-item — the metric ships, the
finalizer does not).

## Consequences

Positive:

- The gate now covers the **attack direction** (additive / escalation edits),
  not just accidental removals — closing the axis the availability model left
  open while keeping the same human-in-the-loop guarantee for agent authors.
- The plan is **tamper-evident and non-launderable**: the full `changes[]` set
  plus the content-hash binding mean an approval cannot be transferred to a
  different edit and a dangerous op cannot hide behind a benign primary.
- Two independent disarm paths are closed at the webhook (status-write
  protection; environment immutability), each defense-in-depth on top of the
  RBAC scoping.

Negative / neutral:

- Two new (optional, additive) CRD fields on `MigrationPlan` and one Argo
  health-Lua extension; `crd-validate` confirms the fields reach `Established`.
- The security triggers slightly widen the set of edits that pause for approval.
  This is intended: an escalation edit is exactly the class that should pause.
  The carve-outs (#7 secret-only, #12 public-only, #13 conditional, `claim.*`
  never gated) keep the common developer flows ungated.
- The `approvedSpecHash` re-gates a completed plan on any drift; a benign edit
  layered on a pending approval therefore requires a fresh approval. This is the
  intended anti-transfer behaviour, not a regression.

## Alternatives considered

- **Merge #10 and #11 into one "public exposure" trigger.** Rejected: a hostname
  can be added to an already-public app without a network escalation, so
  collapsing them would lose a real signal; the rollup carries both cleanly.
- **Gate `claim.*` env-ref adds as well as `secret:`.** Rejected: `claim.*`
  values are provider-derived and self-scoped (ADR 0046), not an exfiltration
  primitive; gating them would fire on the common `claim.pg.url` flow and create
  approver fatigue for no security gain.
- **Authenticate the status write by userInfo instead of fieldManager.**
  Rejected for the status path: the SSA fieldManager is the operator's root of
  trust for its own writes and is already the mechanism `migrationplans/status`
  uses; the userInfo gate is reserved for the ResourceClaim create path, where
  no single SSA manager is authoritative. S-2 records that the fieldManager's
  integrity nonetheless rests on RBAC scoping, not on the label itself.
- **A cluster-reading classifier for the selector tripwire.** Rejected:
  `detect_destructive(old, new)` must stay pure and deterministic (ADR 0051), so
  the multi-provider check lives in the controller as a best-effort tripwire, not
  in the classifier.

## Risks

- **A misclassification could gate a benign escalation or miss a real one.**
  Mitigated by an exhaustive unit table over every new trigger including its
  negatives (claim-ref add not gated; literal→literal not gated; non-public port
  change not gated; `digest→off` not gated), plus the invariant tests on the
  rollup, plus a live kind + Argo walk before release.
- **The content-hash could over-gate** if the hash input is not stable across
  serialization. Mitigated by hashing a structured, length-prefixed
  representation of the full candidate set and by a regression test asserting a
  stale approval re-gates rather than consumes.
- **Explicitly out of scope (recorded so they are not mistaken for gaps).** The
  MigrationPlan protects the git→cluster path only. It does **not** gate: an
  image-*tag* change (ADR 0040 tag→digest auto-rollout owns it); replicas-up /
  autoscale / quota (that is `ResourceQuota`'s job); a registry or CI compromise
  (that is image-signing + `SourceCredential`, ADR 0039); an in-cluster
  `kubectl` / RBAC subject acting directly on child resources; and
  cross-tenancy (Kamaji / Capsule, ADR 0023). These are deliberately ungated in
  §6 of `spec.md` §3.8.

## Owner

Operator maintainers.

## Re-evaluation

- Revisit the `needs.*.selector` tripwire → a full gate when a second
  (non-integrated) `needs.*` provider ships (the tripwire's metric already
  measures the exposure; the gate is a separate plan-item).
- Revisit the deliberately-ungated §6 set if the MigrationPlan scope is ever
  extended to cover the in-cluster or cross-tenancy paths.
- Revisit the Application-delete reattach-by-name vector when the delete
  finalizer plan-item (S-3) lands.

## References

- ADR 0051 (application-scope destructive-change detection and gating) — the
  availability-axis classifier this ADR extends along the security axis.
- ADR 0046 (`Application.env` value references) — the `claim.*` vs `secret:`
  distinction that makes #7 secret-only.
- ADR 0027 (unified `MigrationPlan` with scope discriminator), ADR 0048 (Argo CD
  platform-upgrade approval surface) — the plan CRD and approval surface reused
  here.
- ADR 0036 (MCP agentic-safety) — the agent-author threat model that motivates
  gating the additive/escalation direction.
- ADR 0040 (image tag-to-digest resolution), ADR 0039 (SourceCredential),
  ADR 0050 (backup/restore), ADR 0023 (Kamaji multi-tenancy) — the boundaries of
  the deliberately-ungated §6 set.
- `spec.md` §3.8 (MigrationPlan — security axis + the S-2 / §6 decisions).
- `docs/superpowers/specs/2026-08-06-2.16b-security-axis-design.md`.
