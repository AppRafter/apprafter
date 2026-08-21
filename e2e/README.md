# e2e/

End-to-end smoke harness for AppRafter. Uses real Hetzner Cloud
credentials — every run costs a small amount of euros (one CX22
hour + a Floating IP if you use one). Set `APPRAFTER_E2E_SKIP_DESTROY=1`
to leave the cluster up after the run; otherwise the script tears
it down.

## mvp.sh

Provisions a fresh single-node cluster, bootstraps the in-cluster
stack (Cilium + Gateway API CRDs + Application CRD + Argo CD +
cert-manager + ClusterIssuer), applies a plain `Deployment` +
`Service` running `nginxdemos/hello:plain-text`, verifies the
endpoint via an in-cluster `curl` pod, applies an `Application` CR
and asserts the operator reconciles it to `Ready`, and destroys.

### Usage

```sh
export HCLOUD_TOKEN=...                          # Hetzner API token
export APPRAFTER_SSH_PUBLIC_KEY="$(cat ~/.ssh/id_ed25519.pub)"
./e2e/mvp.sh                                     # green in ~6-8 min
```

### Env vars

| Var                              | Required | Default | Purpose                                             |
| -------------------------------- | -------- | ------- | --------------------------------------------------- |
| `HCLOUD_TOKEN`                   | yes      | —       | Hetzner Cloud API token (Read+Write).               |
| `APPRAFTER_SSH_PUBLIC_KEY`       | yes      | —       | SSH public key (single-line OpenSSH format).        |
| `APPRAFTER_E2E_REGION`           | no       | `nbg1`  | Hetzner region.                                     |
| `APPRAFTER_E2E_SKIP_DESTROY`     | no       | unset   | When `1`, keep the cluster up at the end.           |

### Phases + timing

| Phase                                    | Typical | Cumulative |
| ---------------------------------------- | ------- | ---------- |
| 1. provision (Hetzner CX22 + cloud-init) | ~30s    | 30s        |
| 2. wait for k3s                          | 3-5 min | 4-6 min    |
| 3. cluster-bootstrap                     | 1-2 min | 5-8 min    |
| 4. apply hello-world                     | <5s     | 5-8 min    |
| 5. wait for pod ready                    | 10-30s  | 5-9 min    |
| 6. verify endpoint                       | <10s    | 5-9 min    |
| 6.5. apply Application CR + reconcile    | <60s    | 6-9 min    |
| 7. destroy                               | ~30s    | 6-10 min   |

Target time-to-first-Application: **< 30 minutes** (plan.md §1.12).
The smoke usually lands in 6-9 minutes wall-clock; the budget is
generous.

### What's missing

- CI nightly auto-trigger lives in `.github/workflows/nightly.yml`
  (shipped in v0.1.40 / sub-phase 1.12b).
- Image-build + registry-push integration — out of scope for the
  smoke; operators handle this via their own CI.

### Cleanup if the script crashes mid-run

```sh
cd cli && cargo run --bin apprafter -- destroy --yes
```

`destroy` is idempotent and label-driven — it'll clean up
whatever's tagged `apprafter=true` regardless of the local state
file.

## needs-pg-walk.sh

Local-k3d smoke for the full Phase-2.4 `needs.pg` ResourceClaim chain
(no cloud spend, no secrets). It bootstraps the platform stack —
including the always-on CloudNativePG operator and the seeded
`pg-integrated` ServiceProvider — then walks:

```
generate -> schedule -> provision -> resume + explicit DSN ref ->
delete + RetainedClaim snapshot -> force-GC -> psql DROP proof
```

Since Phase 2.12 (ADR 0046) the app binds its DSN by an **explicit**
`env: {DATABASE_URL: {claim: "pg.url"}}` ref — the 2.4e implicit
`DATABASE_URL` injection and the connection-Secret's composed
`DATABASE_URL` key are removed (the Secret now carries the decomposed
keys `url user pass host port db`). While 2.12 is **unreleased** this walk
therefore **requires `APPRAFTER_E2E_LOCAL_OPERATOR=1`**: it builds +
side-loads the working-tree operator/webhook and applies the branch CRDs
(Phase 1b), because the published image/CRD predate the `env` value node.

The final phase execs the CloudNativePG primary and asserts the
per-claim database + role are **physically dropped from Postgres**
(`pg_database` / `pg_roles` empty), not merely that the `Database` CR
reports `ensure: absent`.

### Usage

```sh
# 2.12 is unreleased, so build + side-load the working-tree operator (Phase 1b):
APPRAFTER_E2E_LOCAL_OPERATOR=1 bash e2e/needs-pg-walk.sh
APPRAFTER_E2E_LOCAL_OPERATOR=1 APPRAFTER_E2E_SKIP_DESTROY=1 bash e2e/needs-pg-walk.sh   # keep the cluster
```

Without `APPRAFTER_E2E_SKIP_DESTROY` the script tears the k3d cluster
down on both success and failure (diagnostics are dumped before
teardown on failure). It is the **pre-manual-walk gate** for the
Tier-1 [Postgres guide](../docs/operator-guide/postgres.md)
and runs nightly via
[`.github/workflows/e2e-pg-nightly.yml`](../.github/workflows/e2e-pg-nightly.yml)
(cron 05:00 UTC, `workflow_dispatch`). It is deliberately **not** part
of the per-push `e2e-k3d.yml` gate (booting a Postgres pod is heavier)
and is not a dependency of `just e2e`.

## substrate-upgrade-hetzner.sh

Real-Hetzner proof that a cluster can be moved onto a **bigger machine**
via backup → destroy → `restore --reprovision --server-type`, with the
workload and all Postgres data intact. A *planned substrate upgrade*
(same target, same project, same region — `cx23` → `cx33` in `hel1`),
not disaster recovery.

The central assertion is a **deterministic digest of the CMS application
database**: for every user table, its row count plus a content hash over
the rows in an explicit `ORDER BY ... COLLATE "C"`. (`pg_dump` output is
unordered and hashing it produces false failures.) The digest is computed
twice on the unchanged source and asserted identical *before* it is
relied on — an unreproducible digest is either a false alarm or a false
pass, and you cannot tell which from the outcome. Phase 7 then asserts it
is byte-identical after the upgrade, with row counts matching table for
table, alongside: server type is now the big SKU **read from the Hetzner
API**, server id changed, and node allocatable memory grew.

Two legs share every phase — one switch, no forked script:

```sh
set -a; . ./backup-test.env; set +a          # exports OLD_CLUSTER_TOKEN + S3_*

# Leg A — host-local restic repo (`backup create`). Needs only the token.
./e2e/substrate-upgrade-hetzner.sh

# Leg B — real off-site S3 (`backup enable` + the in-cluster CronJob runner).
APPRAFTER_SUBSTRATE_BACKEND=s3 ./e2e/substrate-upgrade-hetzner.sh
```

The project baseline **must be zero resources**: the walk refuses to
start otherwise, because its teardown sweeps the project. Teardown is
armed before the first provision and API-verifies zero at the end; a leak
is a failure, not a note. Cost is two short-lived boxes, ~EUR 0.02.

A missing precondition exits 2 *before* anything is provisioned — this
walk never returns green for a gate that did not run. Judge a run by
reading the log: the `FAILED:` marker, each phase's `ok:` lines, and the
final GREEN banner.

Note: the CMS initialises **lazily** — its Payload adapter runs
`prodMigrations` and the global seed on the first request that reaches a
payload route, so a pod that is merely `Ready` has written nothing to the
database. Phase 3 wakes it from inside the pod and waits for
`payload_migrations` to appear, otherwise the "CMS database" fingerprint
would cover nothing but the walk's own seed rows.

## Nightly CI

`.github/workflows/nightly.yml` runs this script every night at
04:00 UTC against a real Hetzner project. It also exposes a
`workflow_dispatch` trigger so operators can kick a run manually
from the Actions tab.

### Repository secrets

Set these in **Settings → Secrets and variables → Actions** before
the first scheduled run:

| Secret                       | Purpose                                                  |
| ---------------------------- | -------------------------------------------------------- |
| `HCLOUD_TOKEN`               | Hetzner Cloud API token (Read+Write).                    |
| `APPRAFTER_SSH_PUBLIC_KEY`   | Single-line OpenSSH public key — `ssh-ed25519 AAAA…`.    |

Without those secrets the workflow runs but `mvp.sh` exits 2 on
the precondition check, leaving a clear failure in the Actions UI.
The runner doesn't touch Hetzner.

### Cost

Each successful run provisions one CX22 for ~10 minutes (plus
tear-down). Hetzner bills hourly — typical cost is single-digit
cents per night. The `mvp.sh` `EXIT` trap destroys the cluster on
failure, so crashes don't leave servers idling.

### Closure criterion (plan.md §1.12)

> Acceptance: nightly green 5 times in a row; a manual run per docs
> works for a new person.

Operators flip the §1.12 ✅ box once five consecutive nightly runs
are green and one new operator has walked the manual quickstart
end-to-end. Both criteria are judgment calls — there's no
automated tracking.
