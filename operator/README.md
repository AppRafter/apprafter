# operator/

Custom Rust operator built on `kube-rs`. Implements controllers for the `Application`, `ResourceClaim`, `AccessGrant`, and `MigrationPlan` CRDs in a single reconcile loop (no Crossplane composition layer).
