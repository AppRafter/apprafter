// SPDX-License-Identifier: FSL-1.1-Apache-2.0

// Platform-scope MigrationPlan example. PlatformController
// detected that the desired platform-stack version (0.2.0)
// carries a `breaking` compatibility classification vs the
// currently-deployed 0.1.x line — it created this plan
// instead of patching the parent platform Application
// directly. Reject reverts `PlatformStack.spec.pin` to the
// value stored in `spec.previousSpecSnapshot.pin`.
//
// Used as a vet-time fixture: `cue vet ./examples/...`
// confirms the rewritten v1alpha1.#MigrationPlan schema
// (B.1.75) admits the platform-scope shape and the
// `previousSpecSnapshot` annotation round-trip.
package examples

import v1alpha1 "apprafter.io/schemas/v1alpha1"

platformBumpMigration: v1alpha1.#MigrationPlan & {
	metadata: {
		name:      "platform-stack-0-2-0-bump"
		namespace: "apprafter-system"
	}
	spec: {
		scope: {
			type: "platform"
			platform: {
				components: ["apprafter-operator", "admission-webhook", "argocd"]
			}
		}
		trigger: {
			type:  "platform-classification"
			field: "spec.pin"
			from:  "0.1.25"
			to:    "0.2.0"
		}
		risks: {
			classification:     "breaking"
			estimatedDowntime:  "10–20 minutes"
			reversible:         true
			requiresFullBackup: false
		}
		plan: [
			{step: 1, action: "Snapshot Argo CD application list to S3", estimatedDuration: "30s", reversible: true},
			{step: 2, action: "Patch parent platform Application targetRevision to 0.2.0", estimatedDuration: "10s", reversible: true},
			{step: 3, action: "Wait for Argo CD reconciliation + all child Applications Healthy", estimatedDuration: "8m", reversible: true},
		]
		approvers: ["alice@company.com", "bob@company.com"]
		previousSpecSnapshot: {
			pin:         "0.1.25"
			autoUpgrade: false
		}
	}
}
