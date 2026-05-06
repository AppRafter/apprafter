# ADR 0012: MigrationPlan as a first-class concept

## Status

`Accepted`. Date: 2026-05-06.

## Context

The most common cause of production outages in declarative-GitOps
systems is "I changed one line and the operator did something I did
not expect." Selector changes for stateful platform services
(Postgres, ClickHouse) and major-version upgrades are inherently
destructive — they require data migration, downtime, and explicit
risk acceptance.

Auto-applying these silently is worse than no automation at all: it
creates a false sense of safety while exposing the platform to data
loss.

## Decision

When a reconciler detects a destructive change (selector change for
stateful claims, major version upgrade, storage class change, or any
field marked `destructive: true` in the ServiceProvider schema), it
creates a `MigrationPlan` resource instead of applying the change
immediately.

The `MigrationPlan` records the trigger, risk breakdown
(`estimatedDowntime`, `dataVolume`, `reversible`,
`requiresFullBackup`), and a step-by-step plan. Backstage shows a
prominent "Pending migration approval — production at risk" banner.
The named approver(s) review, then approve / reject / edit (e.g., add
a maintenance window). On approval, a dedicated migration runner
executes the plan with progress reporting.

Non-destructive changes (replica counts, expose rules, env-var
additions, image updates) auto-apply as before.

## Consequences

Positive:

- The implicit "this might blow up" becomes the explicit "this will
  require approval, here is the risk breakdown".
- Approval is human-in-the-loop by design, not by accident.
- Same model that made `terraform plan / apply` successful.

Negative:

- Approver fatigue is a real risk. Mitigated by limiting the trigger
  set to genuinely destructive changes.
- Adds a new CRD and a new reconciler path; more code to maintain.

## Alternatives considered

- **Auto-apply with rollback.** Rejected: rollback is not always
  possible for stateful resources.
- **Disable destructive changes via admission.** Rejected: operators
  do need to perform these changes; the question is how.
- **External CI gate.** Viable but loses the declarative model.

## Risks

- Approvers may rubber-stamp without reading. Mitigated by surfacing
  the risk breakdown prominently and recording the approval (with
  approver identity) in the audit log.

## Owner

Operator maintainers.

## Re-evaluation

Revisit at v2.x as we ship more automated migration runners (PG
`tier: integrated → managed-aws`, ClickHouse major upgrade, etc.).

## References

- `spec.md` §3.8, §8 ("Why MigrationPlan as a first-class concept").
