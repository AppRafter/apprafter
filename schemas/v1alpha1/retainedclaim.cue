// SPDX-License-Identifier: FSL-1.1-Apache-2.0

package v1alpha1

// RetainedClaim is the immutable snapshot the
// resourceclaim-provisioner finalizer writes when a ResourceClaim is
// deleted, BEFORE it removes its finalizer (Phase 2.4f). It always
// lives in `apprafter-system` — a platform namespace that outlives
// tenant namespaces, so the 7-day-grace GC always fires even if the
// app's namespace is torn down. The original claim's lineage is
// preserved in `spec.claimRef`.
//
// Operator-only + immutable: admission gates CREATE to the operator
// (the finalizer writes them) and rejects any spec mutation on UPDATE;
// the CRD layers the same immutability via an `x-kubernetes-validations`
// CEL rule. Non-empty / RFC3339 invariants live in the CRD + webhook,
// not here (CUE-validation philosophy — no half-measure regex stubs).
// No `status` subresource — the GC reads `spec` only.
#RetainedClaim: {
	#TypeMeta
	kind:     "RetainedClaim"
	metadata: #ObjectMeta
	spec: {
		// Lineage of the deleted ResourceClaim this snapshot stands in
		// for. The snapshot lives in apprafter-system; claimRef keeps
		// the original (name, namespace).
		claimRef: {
			name:      string
			namespace: string
		}
		// ServiceProvider name the deleted claim was matched to.
		provider: string
		// Provider spec.backend (e.g. "cloudnative-pg" or "dragonfly").
		backend: string

		// --- CNPG-backend allocation (optional; absent for dragonfly) ---
		// Shared CNPG Cluster + its namespace (also the password Secret
		// namespace).
		cnpgCluster?:   string
		cnpgNamespace?: string
		// Postgres role + database name (the same identifier).
		role?:     string
		database?: string
		// DNS-1123 metadata.name of the CNPG Database CR.
		databaseObjectName?: string
		// metadata.name of the basic-auth password Secret in the CNPG
		// namespace.
		passwordSecretName?: string

		// --- Dragonfly-backend allocation (optional; absent for CNPG; ADR 0042) ---
		// The shared pool instance + numbered logical DB ($N) the claim held,
		// the $N-pinned ACL username, and the connection Secret coordinates
		// the GC FLUSHDBs + DELUSERs + deletes.
		instance?:                  string
		dbnum?:                     int & >=0 & <1024
		aclUser?:                   string
		connectionSecretRef?:       string
		connectionSecretNamespace?: string

		// --- Disk-backend allocation (optional; absent for CNPG/dragonfly; 2.6b) ---
		// The unowned RWO PVC the claim provisioned (status.volumeClaimRef)
		// + its namespace. The GC deletes this PVC once retainUntil passes.
		volumeClaimRef?:       string
		volumeClaimNamespace?: string

		// --- NATS/jetstream-backend allocation (optional; absent for every
		// other backend; 2.5e, ADR 0061 §8) ---
		// Where nats-system is (the accounts Secret, the mgr_<ns>
		// credential and the NACK CRs all live there), the DECLARING
		// application's name, and the stream/durable names it declared.
		//
		// The application name is stored rather than derived: the GC needs
		// it for the `<app>.` subject prefix the dynamic-stream sweep keys
		// on, and re-deriving it from claimRef.name by stripping a
		// "-jetstream" suffix would be a second, silently-drifting copy of
		// the application controller's own claim_name() join.
		//
		// The declared names are stored because the GC deletes the NACK
		// Stream/Consumer CRs for them (object names <ns>-<app>-<declared>)
		// and EXCLUDES them from the subject-prefix sweep — a declared
		// stream is not a dynamic one, whatever its subjects look like.
		natsNamespace?: string
		natsApp?:       string
		natsDeclaredStreams?: [...string]
		natsDeclaredConsumers?: [...string]

		// RFC3339 instant after which the GC drops the backend resources +
		// password/connection Secret (deletion + 7-day grace).
		retainUntil: string
	}
}
