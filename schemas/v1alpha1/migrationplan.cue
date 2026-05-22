// SPDX-License-Identifier: FSL-1.1-Apache-2.0

package v1alpha1

// MigrationPlan gates destructive changes — to user Applications
// or to the platform stack — behind explicit approval. The same
// CRD covers both scopes via a discriminator field on
// `spec.scope.type`; per-scope behaviour is implemented through
// a Rust trait dispatch in `MigrationController` (lands in
// B.1.76, see plan.md). B.1.75 ships the CRD schema, admission
// webhook validation, and OpenAPI v3 CRD definition; no
// controller logic.
//
// See spec.md §3.8 and ADR 0027.
//
// Per the project's CUE schema validation philosophy
// (CLAUDE.md), this file declares structural invariants only:
// shapes, enums, required-vs-optional. Cross-field invariants
// (scope-required-fields, approver email format, immutability)
// live in the admission webhook
// (`operator/admission-webhook/src/validator_migrationplan.rs`),
// not as regex stubs here.
#MigrationPlan: {
	#TypeMeta
	kind:     "MigrationPlan"
	metadata: #ObjectMeta
	spec:     #MigrationPlanSpec
	status?:  #MigrationPlanStatus
}

// Spec carries the static description of the migration:
// what's changing, what's at risk, the plan steps, and who
// must approve. Once the plan is created, the spec is
// immutable apart from `approvers` additions (the controller
// + webhook enforce this in B.1.76; webhook only rejects
// scope mutation in 1.75 because that's the field that
// breaks the discriminator and the strategy dispatch).
#MigrationPlanSpec: {
	scope:   #MigrationPlanScope
	trigger: #MigrationTrigger
	risks?:  #MigrationRisks
	plan?: [...#MigrationStep]
	approvers?: [...string]

	// Snapshot of the previous PlatformStack spec (or
	// Application spec for application-scope) at the time
	// the plan was created. Used by the controller to
	// revert on reject (platform scope only) and by audit
	// surfaces to show the diff being approved. Free-form
	// JSON — the controller round-trips it without
	// interpretation in 1.75; B.1.76 + B.1.78 wire it
	// into reject flow.
	previousSpecSnapshot?: {...}
}

// Scope discriminator. `type` selects which of the
// scope-specific sub-objects MUST be set. The CUE schema
// expresses the enum + the optionality; the webhook
// enforces "if type=X, scope.X is present and non-empty"
// because CUE cannot express a conditional-required-field
// in a way that round-trips through OpenAPI v3 cleanly.
#MigrationPlanScope: {
	type: "application" | "platform"

	application?: {
		ref: {
			name:      string & =~"^[a-z0-9][a-z0-9-]{0,62}[a-z0-9]$"
			namespace: string & =~"^[a-z0-9][a-z0-9-]{0,62}[a-z0-9]$"
		}
		environment: string & =~"^[a-z0-9][a-z0-9-]{0,62}[a-z0-9]$"
	}

	platform?: {
		// Names of the platform-stack components affected by
		// this migration. Empty list is rejected by the
		// admission webhook — a platform-scope plan that
		// touches zero components is nonsensical.
		components: [...string]
	}
}

// What triggered the plan. The `type` field names the
// concrete trigger; per-type payload lives in `field` +
// `from` / `to`. Open string set — new trigger types are
// added by reconcilers without needing to bump the CRD
// schema (CUE/webhook simply require non-empty strings).
#MigrationTrigger: {
	// Examples: "selector-change", "major-version-upgrade",
	// "storage-class-change", "destructive-field",
	// "platform-classification".
	type: string

	// JSON Pointer-ish path of the field whose change
	// triggered the plan. Free-form string; example values:
	// "needs.pg.selector", "base.image", "spec.pin".
	field: string

	// Old / new values. Free-form JSON — schema does not
	// constrain shape because triggers cover heterogeneous
	// field types.
	from?: _
	to?:   _
}

// Risk classification + impact metadata. Operators read
// this to decide whether to approve.
#MigrationRisks: {
	// Mirrors the platform-stack change-class enum from
	// `platform-stack/cue/compatibility.cue` so users see
	// a consistent vocabulary across application + platform
	// migrations.
	classification: "safe" | "requires-restart" | "data-migration" | "breaking"

	// Free-form text — human estimate, not a parseable
	// duration. Reconciler-supplied or webhook-supplied;
	// no machine consumes it in 1.75.
	estimatedDowntime?: string

	// Free-form text. Example: "12 GB", "~5M rows".
	dataVolume?: string

	// Whether the change can be undone after execution
	// (true for selector flips back; false for destructive
	// data drops).
	reversible?: bool

	// Whether the operator should take a full backup before
	// approving. Surfaces in approval UI as a "you must
	// have a backup" checkbox.
	requiresFullBackup?: bool
}

// One step in the execution plan. Steps run sequentially;
// MigrationController updates `status.executedSteps[]` as
// each completes.
#MigrationStep: {
	// 1-based step number. Required so YAML order doesn't
	// silently change semantics.
	step: int & >0

	// Imperative description shown to the approver. Plain
	// English; no machine semantics in 1.75.
	action: string

	// Free-form text — human estimate, see
	// `MigrationRisks.estimatedDowntime`.
	estimatedDuration?: string

	// Whether this individual step can be reversed mid-plan.
	// Used by the controller's rollback logic (B.1.76).
	reversible?: bool
}

// Status is owned by `MigrationController` (lands in 1.76).
// 1.75 ships the schema only — controller fills it in.
//
// Phase transitions:
//
//   - `pending-approval` (initial; created by detection
//     code in Application reconciler or PlatformController).
//   - `approved` → `executing` → `completed`.
//   - `approved` → `executing` → `failed` (steps errored).
//   - `pending-approval` → `rejected` (platform scope only;
//     application scope has no reject — operator reverts
//     the Git commit instead).
#MigrationPlanStatus: {
	phase?: "pending-approval" | "approved" | "rejected" | "executing" | "completed" | "failed"

	// RFC3339 timestamp of approval. Populated alongside
	// the `pending-approval` → `approved` transition.
	approvedAt?: string

	// Approver identity. Free-form string; the
	// admission webhook + Backstage UI fill it from the
	// authenticated principal.
	approvedBy?: string

	// Step-by-step audit trail. One entry per executed
	// step; appended in order.
	executedSteps?: [...#ExecutedStep]
}

#ExecutedStep: {
	step:        int & >0
	startedAt:   string
	finishedAt?: string
	outcome:     "succeeded" | "failed" | "skipped"
	// Free-form note populated on failures.
	message?: string
}
