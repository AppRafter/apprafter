// SPDX-License-Identifier: FSL-1.1-Apache-2.0

// Application-scope MigrationPlan example. The application
// `parser` has switched its `needs.pg.selector` from
// `tier: integrated` to `tier: managed-aws`, which is a
// data-migration class change — a MigrationPlan gates the
// transition until an approver signs off.
//
// Used as a vet-time fixture: `cue vet ./examples/...`
// confirms the rewritten v1alpha1.#MigrationPlan schema
// (B.1.75) admits the application-scope shape.
package examples

import v1alpha1 "apprafter.io/schemas/v1alpha1"

parserPgSelectorMigration: v1alpha1.#MigrationPlan & {
	metadata: {
		name:      "parser-pg-selector-2026-05-22"
		namespace: "apprafter-system"
	}
	spec: {
		scope: {
			type: "application"
			application: {
				ref: {
					name:      "parser"
					namespace: "demo"
				}
				environment: "prod"
			}
		}
		trigger: {
			type:  "selector-change"
			field: "needs.pg.selector"
			from: {tier: "integrated"}
			to: {tier: "managed-aws"}
		}
		risks: {
			classification:     "data-migration"
			estimatedDowntime:  "5–15 minutes"
			dataVolume:         "12 GB"
			reversible:         false
			requiresFullBackup: true
		}
		plan: [
			{step: 1, action: "Snapshot source DB to S3", estimatedDuration: "2m", reversible: true},
			{step: 2, action: "Provision target RDS instance", estimatedDuration: "5m", reversible: true},
			{step: 3, action: "Logical-replicate source → target", estimatedDuration: "3m", reversible: true},
			{step: 4, action: "Cutover application traffic", estimatedDuration: "30s", reversible: false},
			{step: 5, action: "Retain source DB read-only for 7d", estimatedDuration: "—", reversible: false},
		]
		approvers: ["alice@company.com"]
	}
}
