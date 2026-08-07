// SPDX-License-Identifier: FSL-1.1-Apache-2.0

package v1alpha1

// SourceCredential is a config-only reference object for one private
// source credential — a git-read half (for Argo CD) and/or a
// registry-pull half (for the kubelet). It carries ZERO secret
// material: the material lives sealed (SealedSecret on Tier 1,
// OpenBao on Tier 2), referenced by `backend`. The operator derives
// both materialisations — a prefix-matched Argo `repo-creds` Secret
// and a host-matched `dockerconfigjson` pull-secret — from the one
// CR + its sealed material. See ADR 0039 / plan.md 1.79c.
//
// Cross-field invariants the structural CRD schema cannot express are
// enforced by the admission webhook, NOT here (keeping CUE free of
// half-measure stubs, per the project's CUE-validation philosophy):
//   - at least one of `git` / `registry` is present;
//   - each present `backend` is exactly one of
//     `sealedSecretRef` / `openBaoPath`.
#SourceCredential: {
	#TypeMeta
	kind:     "SourceCredential"
	metadata: #ObjectMeta

	spec:    #SourceCredentialSpec
	status?: #SourceCredentialStatus
}

// Where the sealed material lives. Tier 1 → a SealedSecret reference;
// Tier 2 → an OpenBao path (reserved now, wired in 2.7–2.8 / 3.11).
#SourceCredentialBackend: {
	sealedSecretRef: {
		name:       string
		namespace?: string
	}
} | {
	openBaoPath: string
}

#SourceCredentialSpec: {
	// git-read half. `repoPrefixes` are URL prefixes Argo CD
	// matches a clone against (e.g. "github.com/myorg/").
	git?: {
		backend: #SourceCredentialBackend
		// Non-empty — enforced by the CRD `minItems` + webhook.
		repoPrefixes: [...string]
	}

	// registry-pull half. `hosts` are registry-host prefixes the
	// operator matches a rendered image against (e.g.
	// "ghcr.io/myorg/").
	registry?: {
		backend: #SourceCredentialBackend
		hosts: [...string]
	}
}

#SourceCredentialStatus: {
	// Per-half conditions: `GitPresent` / `GitValid` and
	// `RegistryPresent` / `RegistryValid`. `status: "Unknown"` with
	// reason `Unverified` is used where the operator has no egress
	// to validate (air-gapped / restricted), distinct from an
	// `Invalid` auth failure.
	conditions?: [...{
		type:               string
		status:             "True" | "False" | "Unknown"
		reason?:            string
		message?:           string
		lastTransitionTime: string
	}]

	// The prefixes / hosts the operator currently covers (echoed
	// from spec after validation) — read by the CLI coverage check.
	coveredRepoPrefixes?: [...string]
	coveredHosts?: [...string]

	// RFC3339 timestamp of the last validity probe.
	lastValidated?: string

	// `AwaitingMigrationApproval` while a coverage-removal MigrationPlan
	// gates this credential; otherwise absent. Shares the phase string with
	// the Application scope (2.16b-sc).
	phase?: string

	// Full spec snapshot the operator stamps after a successful derivation
	// (2.16b-sc). The SourceCredential-scope migration classifier diffs the
	// incoming spec against this baseline. Free-form JSON here — the whole
	// `status` node is emitted x-kubernetes-preserve-unknown-fields by crdgen,
	// but a bare `type: object` child under that root is a STRUCTURAL PRUNING
	// BOUNDARY at the apiserver, so the crdmeta `statusSchemaPatches` re-marks
	// THIS node preserve-unknown (same as Application's `lastAppliedSpec`).
	lastAppliedSpec?: {...}
}
