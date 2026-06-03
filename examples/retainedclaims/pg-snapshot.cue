// SPDX-License-Identifier: FSL-1.1-Apache-2.0

// Example RetainedClaim — the immutable snapshot the
// resourceclaim-provisioner finalizer writes into apprafter-system when
// a `needs.pg` ResourceClaim is deleted (Phase 2.4f). retainUntil =
// deletion + 7-day grace; the GC drops the role/DB/password Secret +
// this snapshot once it passes. The metadata.name is the deterministic
// `claim-<ns>-<name>` (cnpg::k8s_name) encoding the deleted claim's
// origin; the lineage is in spec.claimRef. Used as a `cue vet` fixture.
package examples

import v1alpha1 "apprafter.io/schemas/v1alpha1"

retainedClaimPgSnapshot: v1alpha1.#RetainedClaim & {
	metadata: {
		name:      "claim-demo-demo-web-pg"
		namespace: "apprafter-system"
	}
	spec: {
		claimRef: {
			name:      "demo-web-pg"
			namespace: "demo"
		}
		provider:           "pg-integrated"
		backend:            "cloudnative-pg"
		cnpgCluster:        "platform-postgres"
		cnpgNamespace:      "cnpg-system"
		role:               "claim_demo_demo_web_pg"
		database:           "claim_demo_demo_web_pg"
		databaseObjectName: "claim-demo-demo-web-pg"
		passwordSecretName: "claim-demo-demo-web-pg-pw"
		retainUntil:        "2026-06-10T00:00:00+00:00"
	}
}
