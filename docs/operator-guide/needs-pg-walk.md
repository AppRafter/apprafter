# needs.pg manual walk — Postgres from a declared dependency

This guide walks a Tier-1 operator through the full `needs.pg` chain:
an Application that declares `spec.base.needs.pg` gets a Postgres
database provisioned on demand, its connection string injected as
`DATABASE_URL`, and — when the dependency is removed — the database
retained for a 7-day grace period and then physically dropped.

## The chain in one paragraph

You declare `needs: { pg: {} }` on an Application. The operator
generates a `ResourceClaim` and **pauses** the Application
(`status.phase=AwaitingResourceClaim`). The scheduler matches the
claim to a `ServiceProvider` (the seeded `pg-integrated`) and marks it
`Scheduled=True`. The provisioner lazily creates the shared
CloudNativePG `Cluster` (first claim only), a per-claim Postgres role +
database + password Secret, and a connection `Secret` carrying
`DATABASE_URL`; it marks the claim `ready=true`. The Application then
**resumes** to `Ready`, and the rendered Deployment mounts
`DATABASE_URL` from that Secret. When you delete the claim (or remove
the dependency), a finalizer snapshots an immutable `RetainedClaim`
(deletion + 7 days) and the connection Secret cascades away — but the
role and database survive the grace window. Once `retainUntil` passes,
the GC controller drops the role, flips the `Database` CR to
`ensure: absent` (CloudNativePG physically drops the database), deletes
the password Secret, and removes the snapshot.

## Prerequisites

- A real Tier-1 cluster provisioned with `apprafter bootstrap-all`
  (see [`quickstart.md`](quickstart.md)), operator **≥ v0.2.10** (the
  release that ships the always-on CloudNativePG operator, the
  `pg-integrated` ServiceProvider, and the ResourceClaim
  scheduler / provisioner / GC controllers).
- `kubectl` bound to the cluster:

  ```sh
  export KUBECONFIG="$(apprafter kubeconfig --refresh)"
  ```

- Pre-flight: the seeded provider, the CNPG operator, and the
  admission webhook are all Ready:

  ```sh
  kubectl get serviceprovider pg-integrated -n apprafter-system \
    -o jsonpath='{.metadata.labels.tier}{"\n"}'           # -> integrated
  kubectl -n cnpg-system rollout status \
    deploy -l app.kubernetes.io/name=cloudnative-pg        # -> available
  kubectl -n apprafter-system rollout status \
    deploy admission-webhook                               # -> available
  ```

## Step 0 — run the k3d e2e first (cheap gate)

Before spending a real Tier-1 cluster, run the automated walk on a
local k3d cluster. It exercises the identical chain and is the
pre-manual-walk gate:

```sh
bash e2e/needs-pg-walk.sh        # green in ~8-12 min on k3d
```

If that is red, fix it before continuing — the manual walk only adds
value once the automated chain is green.

## Platform-CLI coverage

This walk exercises every shipped `platform-cli` subcommand. Raw
`kubectl` / `psql` are sanity-only supplements that confirm the
machine state behind each CLI surface.

| Stage | Command |
| ----- | ------- |
| Identity + target | `apprafter target list`, `apprafter target add …`, `apprafter whoami` |
| Self-diagnostic | `apprafter doctor` |
| Provision | `apprafter bootstrap-all` (then re-run `apprafter cluster-bootstrap` once to prove idempotency) |
| Cluster access | `apprafter kubeconfig --refresh`, `apprafter argocd-password` |
| Cluster + platform health | `apprafter status`, `apprafter platform status` |
| Author the manifest | `apprafter app scaffold` (see **Known gap** below) |
| Register the app | `apprafter app add` (public repo); `apprafter repo creds add/list/show` for the private-repo variant |
| Inspect the app | `apprafter app status` (see **Known gap** below), `apprafter app logs` |
| Portal | `apprafter open argocd` |
| Cleanup | `apprafter app remove --keep-data`, `apprafter destroy --yes` |

### Author the manifest with `apprafter app scaffold`

```sh
apprafter app scaffold --name parser --namespace demo
```

> **Known gap → Phase 2.5.** The scaffold template emits
> `spec.base.{image,replicas,expose}` but **no `needs` block** — there
> is no `--needs pg` flag yet. Until Phase 2.5 ships scaffold support
> for declared dependencies, hand-add the block under `spec: base:` in
> the generated `apprafter/Application.cue`:
>
> ```cue
> spec: base: {
>     // ... image / replicas / expose left as scaffolded ...
>
>     needs: {
>         pg: {
>             selector: tier: "integrated"
>             size: "small"
>         }
>     }
> }
> ```
>
> `cue vet ./apprafter/...` validates it locally before you push.

### Register the app with `apprafter app add`

Public repo:

```sh
apprafter app add https://github.com/your-org/parser.git \
  --name parser --namespace demo --project apps
```

Private repo — register the git credential first:

```sh
apprafter repo creds add parser-creds \
  --url-prefix https://github.com/your-org \
  --type pat --token "$YOUR_PAT"
apprafter repo creds list           # confirm it is registered
apprafter repo creds show parser-creds
```

## Happy chain — steps 1 to 13

The numbered steps below assume the Application CR is in the cluster
(via Argo CD sync after `app add`, or applied directly for a quick
check). The `kubectl` lines are the machine-readable proof behind each
CLI surface; the constants (`demo` namespace, `parser` app,
`parser-pg` claim, role `claim_demo_parser_pg`, object
`claim-demo-parser-pg`) are derived deterministically from the claim's
`(namespace, name)`.

**1. The Application carries the dependency.**

```sh
kubectl -n demo get application parser \
  -o jsonpath='{.spec.base.needs.pg.selector.tier}{"\n"}'   # -> integrated
```

**2. The operator generated a ResourceClaim.**

```sh
kubectl -n demo get resourceclaim parser-pg \
  -o jsonpath='type={.spec.type} tier={.spec.selector.tier} size={.spec.size}{"\n"}'
# -> type=pg tier=integrated size=small
```

**3. The Application is gated.**

```sh
kubectl -n demo get application parser -o jsonpath='{.status.phase}{"\n"}'
# -> AwaitingResourceClaim
kubectl -n demo get application parser -o \
  jsonpath='{.status.conditions[?(@.type=="ResourceClaimPending")].status}{"\n"}'
# -> True
```

**4. The scheduler matched a provider.**

```sh
kubectl -n demo get resourceclaim parser-pg \
  -o jsonpath='{.status.provider}{"\n"}'                    # -> pg-integrated
kubectl -n demo get resourceclaim parser-pg -o \
  jsonpath='{.status.conditions[?(@.type=="Scheduled")].status}{"\n"}'   # -> True
```

**5. The shared CNPG Cluster was lazily created.** The first claim
creates `platform-postgres`; later claims reuse it.

```sh
kubectl -n cnpg-system get cluster.postgresql.cnpg.io
# exactly one Cluster: platform-postgres
```

**6. The claim is provisioned.** (The lazy Cluster boot makes this the
slow step.)

```sh
kubectl -n demo get resourceclaim parser-pg -o jsonpath='{.status.ready}{"\n"}'
# -> true
kubectl -n cnpg-system get database.postgresql.cnpg.io claim-demo-parser-pg \
  -o jsonpath='ensure={.spec.ensure} owner={.spec.owner}{"\n"}'
# -> ensure=present owner=claim_demo_parser_pg
kubectl -n cnpg-system get cluster.postgresql.cnpg.io platform-postgres -o \
  jsonpath='{.spec.managed.roles[?(@.name=="claim_demo_parser_pg")].name}{"\n"}'
# -> claim_demo_parser_pg
kubectl -n cnpg-system get secret claim-demo-parser-pg-pw \
  -o jsonpath='{.type}{"\n"}'                               # -> kubernetes.io/basic-auth
```

**7. The connection Secret carries the DSN.**

```sh
kubectl -n demo get resourceclaim parser-pg \
  -o jsonpath='{.status.connectionSecretRef}{"\n"}'         # -> parser-pg-conn
kubectl -n demo get secret parser-pg-conn \
  -o jsonpath='{.data.DATABASE_URL}' | base64 -d; echo
# -> postgresql://claim_demo_parser_pg:…@platform-postgres-rw.cnpg-system.svc:5432/claim_demo_parser_pg
kubectl -n demo get secret parser-pg-conn \
  -o jsonpath='{.metadata.ownerReferences[0].kind}{"\n"}'   # -> ResourceClaim
```

**SSA-split guard.** The provisioner's status write must NOT clobber
the scheduler's verdict — `Scheduled` is still `True` after
provisioning:

```sh
kubectl -n demo get resourceclaim parser-pg -o \
  jsonpath='{.status.conditions[?(@.type=="Scheduled")].status}{"\n"}'   # -> True
```

**8. The Application resumed.**

```sh
kubectl -n demo get application parser -o jsonpath='{.status.phase}{"\n"}'
# -> Ready
```

**9. DATABASE_URL is injected into the Deployment.**

```sh
kubectl -n demo get deployment parser -o \
  jsonpath='{.spec.template.spec.containers[0].env[?(@.name=="DATABASE_URL")].valueFrom.secretKeyRef.name}{"\n"}'
# -> parser-pg-conn
```

**10. (Optional) connect from a consumer pod.** With a
`postgres:16`-style image you can prove the DSN actually connects:

```sh
kubectl -n demo run dsn-check --rm -it --restart=Never \
  --image=postgres:16 \
  --env="DATABASE_URL=$(kubectl -n demo get secret parser-pg-conn \
    -o jsonpath='{.data.DATABASE_URL}' | base64 -d)" \
  -- psql "$DATABASE_URL" -tAc "SELECT 1"          # -> 1
```

**11. Inspect the app via the CLI.**

```sh
apprafter app status parser
apprafter app logs parser --tail 50
```

> **Known gap → Phase 2.5.** `apprafter app status` reports Argo CD
> sync / health and workload pods, but does **not** surface
> ResourceClaim / provisioning state. While the chain is in flight
> (e.g. the app sitting at `AwaitingResourceClaim`), fall back to:
>
> ```sh
> kubectl -n demo get resourceclaim parser-pg
> ```

**12. Delete the dependency — the claim is retained.** Removing the
`needs.pg` block (and re-syncing) deletes the claim; for a direct
check:

```sh
kubectl -n demo delete resourceclaim parser-pg
kubectl -n apprafter-system get retainedclaim claim-demo-parser-pg -o \
  jsonpath='claim={.spec.claimRef.name} role={.spec.role} until={.spec.retainUntil}{"\n"}'
# -> claim=parser-pg role=claim_demo_parser_pg until=<RFC3339, ~7 days out>
kubectl -n demo get secret parser-pg-conn          # -> NotFound (cascaded)
```

The role and database survive the grace floor:

```sh
kubectl -n cnpg-system get cluster.postgresql.cnpg.io platform-postgres -o \
  jsonpath='{.spec.managed.roles[?(@.name=="claim_demo_parser_pg")].name}{"\n"}'
# -> claim_demo_parser_pg   (still present)
kubectl -n cnpg-system get database.postgresql.cnpg.io claim-demo-parser-pg \
  -o jsonpath='{.spec.ensure}{"\n"}'               # -> present
```

**13. Force the GC (without a 7-day wait).** The `RetainedClaim` is
immutable (a CEL `self == oldSelf` rule), so an in-place
`kubectl patch` of `retainUntil` is **rejected**. Delete it and
re-create it with a past `retainUntil` — your e2e/walk kubeconfig is
`system:masters`, which the operator-only webhook permits to CREATE:

```sh
kubectl -n apprafter-system delete retainedclaim claim-demo-parser-pg

kubectl apply -f - <<'YAML'
apiVersion: apprafter.io/v1alpha1
kind: RetainedClaim
metadata:
  name: claim-demo-parser-pg
  namespace: apprafter-system
spec:
  claimRef:
    name: parser-pg
    namespace: demo
  provider: pg-integrated
  backend: cloudnative-pg
  cnpgCluster: platform-postgres
  cnpgNamespace: cnpg-system
  role: claim_demo_parser_pg
  database: claim_demo_parser_pg
  databaseObjectName: claim-demo-parser-pg
  passwordSecretName: claim-demo-parser-pg-pw
  retainUntil: "2000-01-01T00:00:00Z"
YAML
```

The GC fires immediately:

```sh
# role dropped from the shared Cluster:
kubectl -n cnpg-system get cluster.postgresql.cnpg.io platform-postgres -o \
  jsonpath='{.spec.managed.roles[?(@.name=="claim_demo_parser_pg")].name}{"\n"}'   # -> (empty)
# Database flipped to absent (CNPG drops it):
kubectl -n cnpg-system get database.postgresql.cnpg.io claim-demo-parser-pg \
  -o jsonpath='{.spec.ensure}{"\n"}'               # -> absent
# password Secret + the snapshot gone:
kubectl -n cnpg-system get secret claim-demo-parser-pg-pw           # -> NotFound
kubectl -n apprafter-system get retainedclaim claim-demo-parser-pg  # -> NotFound
```

## MANDATORY: psql DROP assertion

**The `ensure: absent` flip is the operator's intent, not proof.** You
must confirm the database is **physically gone from Postgres** — not
merely that the `Database` CR reports `absent`. Exec the CloudNativePG
primary and query the catalog directly:

```sh
PRIMARY=$(kubectl -n cnpg-system get pod \
  -l cnpg.io/cluster=platform-postgres,role=primary \
  -o jsonpath='{.items[0].metadata.name}')

kubectl exec "$PRIMARY" -n cnpg-system -- \
  psql -U postgres -tAc \
  "SELECT 1 FROM pg_database WHERE datname='claim_demo_parser_pg'"
# -> (EMPTY — the database is physically dropped)

kubectl exec "$PRIMARY" -n cnpg-system -- psql -U postgres -c '\l' \
  | grep claim_demo_parser_pg
# -> (no match — \l does not list it)

kubectl exec "$PRIMARY" -n cnpg-system -- \
  psql -U postgres -tAc \
  "SELECT 1 FROM pg_roles WHERE rolname='claim_demo_parser_pg'"
# -> (EMPTY — the role is dropped too)
```

CloudNativePG's `ensure: absent` reconcile lags the CR patch, so give
it a few retries. **If the row is present after the GC, CloudNativePG
did NOT honor `ensure: absent` — STOP, this is a closure-blocking
bug.**

## DoD checklist

The walk must exercise both shipped surfaces. Check every box.

**Surface 1 — Argo CD UI (`apprafter open argocd`):**

- [ ] The bootstrap / platform Application is **Synced** + **Healthy**.
- [ ] The `pg-integrated` ServiceProvider resource shows green.
- [ ] The `platform-postgres` CNPG Cluster shows green with
      `instances = 1`.

**Surface 2 — kubectl / psql assertions:**

- [ ] ResourceClaim `parser-pg` reaches `status.ready=true`
      (steps 2, 4, 6).
- [ ] Application `parser` transitions `AwaitingResourceClaim → Ready`
      (steps 3, 8).
- [ ] `DATABASE_URL` is injected into the Deployment (step 9).
- [ ] Deleting the claim cascades the connection Secret + writes a
      `RetainedClaim`; the role/database survive the grace floor
      (step 12).
- [ ] Forcing the GC drops the role, the `Database` flips to `absent`,
      and the password Secret + snapshot are removed (step 13).
- [ ] **The psql DROP assertion passes — `pg_database` and `pg_roles`
      have no row for `claim_demo_parser_pg`.**

## Troubleshooting

| Symptom | Likely cause | Fix |
| ------- | ------------ | --- |
| Claim stuck `Scheduled` absent / Application stuck `AwaitingResourceClaim` | `needs.pg.selector` does not match any provider's `metadata.labels` | Confirm `selector.tier=integrated` and that `pg-integrated` carries `tier=integrated`: `kubectl get serviceprovider pg-integrated -n apprafter-system -o yaml`. |
| `status.ready` never `true` | shared CNPG Cluster not Ready, or a provisioner error | `kubectl -n cnpg-system get cluster platform-postgres`; check the operator logs: `kubectl -n apprafter-system logs deploy/apprafter-operator`. |
| `kubectl apply` of the Application rejected | admission webhook: an unknown `needs` key, or a literal `DATABASE_URL` colliding with the injected one | Use only the closed `needs` key set (`pg`); do not declare a literal `env.DATABASE_URL` alongside `needs.pg`. |
| Database still present after the GC | CloudNativePG has not reconciled `ensure: absent` yet | Re-run the psql query after a short wait; check the CNPG operator logs in `cnpg-system`. If it persists, this is a closure-blocking bug. |

## Cleanup

```sh
apprafter app remove parser --keep-data    # leave any retained data in place
apprafter destroy --yes                     # tear down the Tier-1 cluster
```
