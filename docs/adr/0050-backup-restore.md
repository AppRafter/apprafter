# ADR 0050: backup, export, and restore — restic engine, local-pull default

## Status

`Accepted` (2026-06-21). **Amended 2026-07-17** — the Phase-4 follow-on
"automated off-site S3 push" was pulled up and delivered as **2.6d-4**; the
open scheduled-runner decision (K8up vs AppRafter CronJob-restic) is now
**resolved in favour of an AppRafter CronJob-restic runner** (see the
off-site-push section under Decision + the K8up entry under Alternatives).

## Context

AppRafter runs stateful workloads — PostgreSQL (CNPG), Dragonfly (redis),
owned and shared persistent volumes — provisioned through the `needs.*`
ResourceClaim pipeline (ADRs 0042, 0043, 0049), with user secrets sealed via
the SealedSecrets controller (Phase 2.11) and private-source credentials bound
through `SourceCredential` CRs (ADR 0039). Operators will not trust production
to the platform without a working, demonstrable restore.

Two distinct needs were apparent:

1. **Inspect / migrate out** — pull the live native data to the operator's
   machine in an open, self-contained form, with no platform involvement to
   read it. A debugging convenience and an anti-lock-in escape hatch.
2. **Disaster recovery** — capture enough to rebuild a cluster (data + the
   config and app custom resources + the user secrets) into one portable
   artifact, and replay it.

The forces:

- **Zero forced purchase in the default path.** The Tier-1 operator runs a €5
  VDS; forcing them to buy an object-storage bucket just to take a backup is
  product friction. The default must be a local pull onto the operator's
  machine.
- **The artifact contains decrypted secrets**, so it must be encrypted at
  rest, and the key must stay with the operator.
- **SealedSecrets are cluster-bound** — sealed material only unseals as
  `<namespace>/<name>` under the controller key of the cluster that sealed it,
  so a restore cannot copy them; it must re-seal for the target.
- **GitOps owns the workloads.** Argo CD pulls app source from Git, so a
  backup need not carry source code — but it must carry the Argo `Application`
  CRs so a restore re-registers the apps (no manual re-add).
- **The provisioning pipeline must not deadlock the restore**, and a restore
  must not race a live workload onto half-loaded data.

## Decision

We will ship a **restic-based, CLI-orchestrated** backup/restore engine with a
**local-filesystem default** backend, exposed as three commands.

### `export` (Kind 1) and `backup` (Kind 2)

`apprafter export` pulls native data only (pg `pg_dump -Fc`, volume tars,
redis snapshots) to a plain folder plus a `manifest.json` — no CRs, no
secrets, no encryption. `apprafter backup create` runs the same extraction and
**additionally** serializes the config/app CRs and the decrypted user secrets,
then wraps everything into an encrypted restic repository. `apprafter backup
list` enumerates a repo's snapshots. Both extraction paths share one engine
(`cli-providers::backup`); native data is pulled through ephemeral helper pods
that stream `pg_dump` / `tar` over `kubectl exec`.

### Cluster scope by default; user-vs-platform discrimination (H1)

Both commands default to the **whole cluster** — the *app-namespace set*
derived from `kubectl get applications.apprafter.io -A`, never from
`kubectl get ns` (which would sweep platform/system namespaces we must never
replay). `--namespace`/`--select` narrow it.

The backup captures **user** material only — the load-bearing **H1**
discrimination:

- Argo `Application`s are filtered to those carrying
  `apprafter.io/managed-by=apprafter`. The platform umbrella and per-component
  Argo Applications lack the label and are never serialized, so a restore
  never double-owns the target's own bootstrap Applications.
- Config CRs are captured by **kind**: `PlatformStack/default` and every
  `SourceCredential` (cluster-wide). There is no in-cluster `Infrastructure`
  CR in M2 — the infrastructure topology is the **local manifest**, and its
  essentials ride `manifest.platformVersion`. A missing
  `infrastructures.apprafter.io` listing is expected, never an error.

### The serialized config-CR set and the two secret-capture paths

The captured config CRs are a fixed set: `PlatformStack` →
`SourceCredential` (follow-the-reference) → user `Application`s (gated) → user
`ArgoApplication`s → `SharedVolume`s. Secrets are captured by **two distinct
paths**, because the SealedSecret-backed app-namespace sweep cannot see
everything:

- **(a) app user secrets** — for each in-scope namespace, a Secret is carried
  only when a SealedSecret of the same name exists. Derived secrets the
  operator re-creates (connection Secrets, pull-secrets) are skipped.
- **(b) `SourceCredential` material** — the material lives in
  `apprafter-system`, outside the app-ns set, so the sweep misses it. The
  backup instead **follows the reference**: it resolves each
  `SourceCredential`'s `spec.git.backend.sealedSecretRef` and
  `spec.registry.backend.sealedSecretRef` and reads the underlying unsealed
  material directly (a cluster-wide path).

### `restore` — into a running target; modes (a) and (b)

`apprafter restore <repo> --target <name>` replays a backup into a **running,
already-bootstrapped** target cluster. The git/registry token used to pull
private workloads after restore comes from the **target's own configuration**
(its re-applied `SourceCredential`), not from a flag — a two-source restore
(repo artifact + target config).

- **(a) restore-into-running** (default): replay config CRs → gate apps → wait
  for fresh claims → load data → re-seal secrets → resume workloads.
- **(b) data-only** (`--data-only`): reload only the native data — scale the
  existing apps to zero, load, resume. No CR/secret replay.
- **(c) clone-to-new** (`--reprovision`): the "source is dead, rebuild from
  nothing" path. A prepended `Reprovision` step provisions + bootstraps a fresh
  cluster in the target (`bootstrap_all::run` — topology + cloud token from the
  target's local config, R2, exactly as `apprafter up`), then replays as (a).
  The kubeconfig is resolved lazily, after the fresh cluster exists; version
  alignment rides the backup's `PlatformStack` (applied in `ApplyPlatformStack`)
  plus the cross-version `version_warning`. `--reprovision` + `--data-only` are
  mutually exclusive and rejected up front. **Real-Hetzner only** (kind has no
  cloud provider). Validated end-to-end 2026-07-16 (`e2e/restore-reprovision-
  hetzner.sh`: provision → backup → destroy → `restore --reprovision` → fresh
  box, data + secret intact). It fulfils the Phase-4 external-S3 DR drill
  (`plan.md`, "test restore: a new cluster from backup in < 1 hour") and the
  Phase-8.5 `DisasterRecoveryPlan` resource.

### Restore ordering and the two invariants (H2, R1)

The full restore is a fixed sequence:

```
RestoreArtifact -> ApplyPlatformStack -> ApplySourceCredentials ->
ApplyAppsGated -> WaitClaimsBound -> LoadData -> ReSealUserSecrets ->
ResumeWorkloads
```

- **H2 — workload gating.** Apps are applied with `replicas: 0` and their user
  Argo Applications have `syncPolicy.automated` stripped, so the operator
  provisions fresh claims but **no pod runs** during the load. Data is loaded
  into the empty backends, then workloads are resumed at their original
  replica count and Argo auto-sync is re-enabled. This is what lets a
  framework-style tracked migration see the restored state and **skip** on
  boot rather than race the load.
- **R1 — wait for the claim, not the volume bind.** `WaitClaimsBound` polls
  each regenerated `ResourceClaim` for `status.ready == true`, **not** for the
  PVC to be `Bound`. A 2.6b disk claim reports ready as soon as its
  `volumeClaimRef` is set; on a `WaitForFirstConsumer` StorageClass the PVC
  binds only when its first consumer schedules — and the restore's own load
  helper is that first consumer. Waiting for `Bound` would deadlock.
  *Caveat:* on a Tier-2 multi-node cluster, an RWO PVC may bind to a node
  other than the one the load helper lands on; the restore pins the load
  helper to the same node the workload will use, but this is the area to watch
  as multi-node lands.

### Data load mechanics and secret roundtrip

- **pg**: a helper pod pipes the dump on stdin to `pg_restore --no-owner
  --clean --if-exists`. `--no-owner` is **assumed** because the target role is
  the newly-provisioned one, not whatever owned the source objects. Credentials
  come from the claim's **fresh** `status.connectionSecretRef`, never the
  backed-up ones.
- **volumes**: a helper pod streams the tar on stdin to `tar x` into the fresh
  PVC mounted read-write.
- **secrets**: the source SealedSecrets are not copied. The restore reads the
  decrypted material from the backup and **re-seals** it against the target's
  controller public key, round-tripping the Kubernetes secret **type** (a
  `kubernetes.io/tls` secret stays TLS). App user secrets re-seal into their
  app namespace; `SourceCredential` material re-seals into `apprafter-system`.

### Storage engine: restic pull (T1) vs CSI snapshot (T2+)

For Tier 1, the data is pulled natively (logical dumps / tars) through helper
pods and stored in restic — engine-agnostic and portable. CSI
volume-snapshot-based backup (storage-level, faster for large volumes) is the
Tier-2+ path and is out of scope here.

### Version-aligned target

The default restore targets a cluster bootstrapped at the same platform-stack
version as the backup (`manifest.platformVersion`). A cross-version restore is
not blocked but **warns** — a different target version may re-render
components, so the operator verifies after restoring.

### Off-site scheduled S3 push (2.6d-4)

The local-pull `backup` above is the default and stays so. **2.6d-4** adds an
**opt-in** scheduled push of the same encrypted restic repo to a
user-configured external S3 bucket, so a Tier-1 operator who *wants* off-site
DR gets it without the platform forcing a bucket purchase on anyone else.

- **Runner = an AppRafter CronJob running the `apprafter-backup` binary**, not
  a third-party operator. The binary reuses the *same* `backup-core` engine as
  the local-pull `backup` (the KubeExec + ResticRunner traits, `pg_dump -Fc` +
  ephemeral restic), so there is one backup code path, not two. Delivery is
  chart-owned: the platform-stack chart's `templates/backup.yaml` (guarded
  entirely by `{{- if .Values.backup.enabled }}`) emits a scoped
  ServiceAccount + ClusterRole/-Binding, a nightly backup CronJob, a weekly
  `restic check` CronJob, and a CiliumNetworkPolicy pinning the runner's
  egress. The operator projects `PlatformStack.spec.backup` onto
  `.Values.backup`; the runner image is its own `apprafter-backup/v*` tag
  stream, pinned in the chart via `.Values.backup.image`.
- **Config lives on `PlatformStack.spec.backup`** (GitOps-native, opt-in,
  default off). The CLI verbs `apprafter backup enable/disable/status` are
  declarative merge-patches of that block after a fail-closed preflight
  (restic ≥ 0.14, the sealed credential Secret exists, repo reachability, an
  explicit DR-credentials-saved confirmation).
- **Staging mode** — `monolithic` (default): stage every namespace's native
  data at once, one snapshot per run (manifest format v1). `sequential`
  (opt-in): stage + snapshot one claim at a time, writing the manifest
  snapshot **last** as the commit-point, so peak staging disk is bounded by
  the largest single claim rather than the sum. Restore auto-detects the
  format via the manifest version.
- **Retention is restic's own, host+format-aware.** The runner backs up under
  a fixed `--host apprafter-backup` so restic's `forget` grouping is stable
  (pod-name hosts would make every snapshot its own group and silently retain
  everything). The pure `plan_prune` keeps daily/weekly/monthly
  representatives (default 7/4/6) computed over the *manifest* snapshots and
  deletes whole run-sets by tag + sweeps orphans, then forgets by **explicit
  id** — never trusting restic's own keep-policy grouping. Prune is the
  operator-side `apprafter backup prune` verb (full creds, outside the
  cluster) by default; it stamps `apprafter.io/last-prune`. **Bucket
  lifecycle rules are the wrong tool** — restic packs many snapshots into
  content-addressed pack files, so an object-age lifecycle rule would delete
  still-referenced packs and corrupt the repo; retention MUST flow through
  `restic forget --prune`.
- **Two-tier scoped credentials.** The operator owns the *full* S3 creds
  outside the cluster (for prune + DR restore). The in-cluster Secret, in the
  default `enforce: operator` model, should carry creds scoped to
  Put/Get/List + Delete only on `locks/*` — so a cluster compromise can
  rotate/stale-lock but cannot erase backup history (integrity + availability
  of the history survive). `enforce: cluster` instead puts full creds in the
  cluster Secret and runs `forget --prune` in the Job. **Confidentiality
  caveat:** scoped-delete protects integrity/availability, NOT confidentiality
  — anyone with the cluster creds can still *read* every snapshot; restic
  encryption means confidentiality rests on the passphrase, which is why the
  passphrase stays off the cluster and must be saved out-of-band.
- **Credentials never live in chart values** — only `credentialRef.name`
  names a Secret in `apprafter-system`, mounted via `envFrom: secretRef`. The
  runner self-reports into a non-chart-owned `apprafter-backup-status`
  ConfigMap (so Argo CD won't reconcile it away); `apprafter backup status`
  reads it plus the Job outcomes.

## Consequences

- **Easier:** a Tier-1 operator can take an encrypted backup and restore it
  with zero cloud spend; the artifact is a plain restic repo readable with
  stock `restic` (no lock-in); apps auto-register on restore (no manual
  re-add); secrets survive a cluster rebuild.
- **Harder / accepted:** restore ordering is intricate (the H2/R1 invariants
  are the crux); the default path requires the target to be bootstrapped first
  (clone-to-new is deferred); the operator owns the passphrase and any cloud
  token, and losing the passphrase makes the backup unrecoverable.
- **Neutral:** restic becomes a runtime dependency of the CLI (resolved on the
  operator machine; the e2e harness wraps a `nix run nixpkgs#restic` fallback).

## Alternatives considered

- **Velero — rejected.** Velero supports only object storage as a backup
  location (BSL); its restic/file-system backup is not stand-alone (still
  needs a bucket), and a local-artifact + restore-from-file flow is an open,
  unclosed feature request. Forcing a bucket purchase into the default path is
  product friction, so Velero is unfit for the default. (It may reappear as a
  Tier-2+ option alongside CSI snapshots.)
- **CNPG Barman / continuous PITR for pg — not for export.** Barman is
  in-cluster continuous backup; `export`/`backup` deliberately use logical
  `pg_dump -Fc` so the artifact is self-contained, openable off-cluster, and
  uniform with the volume/redis dumps.
- **An always-on object-storage default — rejected** for the same
  zero-forced-purchase reason. Automated S3 push stays **opt-in** (below).
- **K8up as the scheduled runner — rejected (2.6d-4).** K8up is a mature
  CNCF restic-based backup operator with scheduling, prune/check, and an
  app-aware `backupcommand`. It was the earlier lean, but adopting it means a
  whole new in-cluster operator + its CRDs + its own scheduling/credential
  model running *alongside* the `backup-core` engine we already ship and
  control — two backup code paths, more RBAC surface, and a dependency the
  "one way to do things" principle resists. An AppRafter CronJob invoking the
  same `apprafter-backup` binary reuses the existing `pg_dump`+ephemeral-restic
  engine, keeps retention format-aware in code we own, and adds no operator
  dependency. K8up may still fit a Tier-2+ managed profile later.

## Risks

- **Lost passphrase ⇒ unrecoverable backup.** Mitigation: the command refuses
  an empty passphrase and documents the operator's responsibility; no silent
  weak-key path exists.
- **RWO multi-node bind (R1 caveat).** On Tier-2 multi-node, an RWO PVC could
  bind to the wrong node for the load helper. Mitigation: the load helper is
  pinned to the workload's node; re-evaluate when multi-node lands.
- **Cross-version restore re-render.** Mitigation: warn on a version mismatch;
  the operator verifies after restore. Cross-version DR is explicitly deferred.
- **pg major mismatch on `pg_restore`.** The helper image is major-matched to
  the source CNPG image where visible; a mismatch surfaces as a `pg_restore`
  error, not silent corruption.

## Owner

Platform CLI / storage maintainers.

## Re-evaluation

Revisit when the Phase-4 external-S3 DR drill lands (clone-to-new + automated
S3 push), or at platform M-tier-2 (CSI snapshot path + multi-node RWO), or if
restic proves an unacceptable operator-machine dependency.

## References

- `plan.md` § 2.6d (data export + backup/restore), § 4.x (external-S3 DR),
  § 8.5 (`DisasterRecoveryPlan`).
- ADR 0039 (SourceCredential), 0042 (needs.redis), 0043 (needs.disk),
  0046 (env value references), 0049 (cross-app SharedVolume).
- `docs/operator-guide/backup-restore.md` — the operator how-to.
- `cli/platform-cli/src/commands/{backup,restore}.rs`,
  `cli/cli-providers/src/backup/` — the implementation.
- `e2e/backup-restore-walk.sh` — the end-to-end validation harness.
