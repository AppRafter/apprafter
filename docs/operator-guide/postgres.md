---
description: "Watching a declared Postgres dependency all the way through: claim, provisioning, credential binding, and the grace window after removal."
---

# Postgres from a declared dependency

This guide walks a Tier-1 operator through the full `needs.pg` chain:
an Application that declares `spec.base.needs.pg` gets a Postgres
database provisioned on demand, its connection details published in a
`Secret` the app binds to an env-var of its choosing, and — when the
dependency is removed — the database retained for a 7-day grace period
and then physically dropped.

## The chain in one paragraph

You declare `needs: { pg: {} }` on an Application. The operator
generates a `ResourceClaim` and **pauses** the Application
(`status.phase=AwaitingResourceClaim`). The scheduler matches the
claim to a `ServiceProvider` (the seeded `pg-integrated`) and marks it
`Scheduled=True`. The provisioner lazily creates the shared
CloudNativePG `Cluster` (first claim only), a per-claim Postgres role +
database + password Secret, and a connection `Secret` carrying the
decomposed fields `url`, `user`, `pass`, `host`, `port`, `db`; it marks
the claim `ready=true`. The Application then **resumes** to `Ready`, and
the rendered Deployment carries a `secretKeyRef` for every claim field
the manifest referenced. When you delete the claim (or remove
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
  apprafter kubeconfig --refresh > /tmp/kc && export KUBECONFIG=/tmp/kc
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

## Commands used on this page

Every step below runs through the `apprafter` CLI. The `kubectl` and
`psql` reads are there to show you the machine state behind each one;
you never need them to drive the chain.

| Stage | Command |
| ----- | ------- |
| Identity + target | `apprafter target list`, `apprafter target add …`, `apprafter whoami` |
| Self-diagnostic | `apprafter doctor` |
| Provision | `apprafter bootstrap-all` (then re-run `apprafter cluster-bootstrap` once to prove idempotency) |
| Cluster access | `apprafter kubeconfig --refresh`, `apprafter argocd-password` |
| Cluster + platform health | `apprafter status`, `apprafter platform status` |
| Author the manifest | `apprafter app scaffold --needs pg` |
| Register the app | `apprafter app add` (public repo); `apprafter repo creds add/list/show` for the private-repo variant |
| Inspect the app | `apprafter app status`, `apprafter app logs` |
| Portal | `apprafter open argocd` |
| Cleanup | `apprafter app remove --keep-data`, `apprafter destroy --yes` |

### Author the manifest with `apprafter app scaffold`

```sh
apprafter app scaffold --name parser --namespace demo --needs pg
```

> The repeatable `--needs pg` flag emits the `spec.base.needs` block for
> you; an unknown type is a clear error. The key set is closed —
> `pg`, `jetstream`, `clickhouse`, `redis`, `s3`, `notifications` and
> `disk` — and of those, providers ship today for `pg`, `redis` and
> `disk`, each with its own guide in this section.
> The generated `apprafter/Application.cue` carries:
>
> ```cue
> spec: base: {
>     // ... image / replicas / expose ...
>     needs: {
>         pg: { selector: { tier: "integrated" }, size: "small" }
>     }
> }
> ```
>
> `cue vet ./apprafter/...` validates it locally before you push.

`needs.pg` provisions the database and publishes its connection
`Secret`, but injects nothing into your container (ADR 0046). Bind the
env-var you want to the claim field yourself:

```cue
spec: base: {
    // ... image / replicas / expose / needs ...
    env: {
        DATABASE_URL: claim.pg.url
    }
}
```

The env-var name is yours to pick; `claim.pg.url` names the field in the
connection `Secret`. The `claim` binding is generated from your own
`needs` block by `apprafter app validate` and by the CUE CMP at render
time, so referencing a field you did not provision fails to compile
rather than at runtime. The rest of this guide assumes that binding is in
place.

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

> **Naming — the `parser` you query is the apprafter.io `metadata.name`,
> not the `apprafter app add` name.** The steps address
> `application.apprafter.io parser` and `resourceclaim.apprafter.io
> parser-pg`: the kinds are group-qualified on purpose, because bare
> `application` also matches Argo CD's `argoproj.io` Application and bare
> `resourceclaim` matches the k8s 1.32+ DRA `resource.k8s.io`
> ResourceClaim. The `parser` name is the apprafter.io Application's
> `metadata.name` from the repo's `apprafter/Application.cue` — also the
> rendered Deployment name — and it can legitimately **differ** from the
> name you passed to `apprafter app add` (which names the *Argo CD*
> Application). If `kubectl get application.apprafter.io <name>` returns
> "not found", you likely used the `app add` name; use the
> `Application.cue` `metadata.name` instead.

**1. The Application carries the dependency.**

```sh
kubectl -n demo get application.apprafter.io parser \
  -o jsonpath='{.spec.base.needs.pg.selector.tier}{"\n"}'   # -> integrated
```

**2. The operator generated a ResourceClaim.**

```sh
kubectl -n demo get resourceclaim.apprafter.io parser-pg \
  -o jsonpath='type={.spec.type} tier={.spec.selector.tier} size={.spec.size}{"\n"}'
# -> type=pg tier=integrated size=small
```

**3. The Application is gated.**

```sh
kubectl -n demo get application.apprafter.io parser -o jsonpath='{.status.phase}{"\n"}'
# -> AwaitingResourceClaim
kubectl -n demo get application.apprafter.io parser -o \
  jsonpath='{.status.conditions[?(@.type=="ResourceClaimPending")].status}{"\n"}'
# -> True
```

**4. The scheduler matched a provider.**

```sh
kubectl -n demo get resourceclaim.apprafter.io parser-pg \
  -o jsonpath='{.status.provider}{"\n"}'                    # -> pg-integrated
kubectl -n demo get resourceclaim.apprafter.io parser-pg -o \
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
kubectl -n demo get resourceclaim.apprafter.io parser-pg -o jsonpath='{.status.ready}{"\n"}'
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
kubectl -n demo get resourceclaim.apprafter.io parser-pg \
  -o jsonpath='{.status.connectionSecretRef}{"\n"}'         # -> parser-pg-conn
kubectl -n demo get secret parser-pg-conn \
  -o jsonpath='{.data.url}' | base64 -d; echo
# -> postgresql://claim_demo_parser_pg:…@platform-postgres-rw.cnpg-system.svc:5432/claim_demo_parser_pg
kubectl -n demo get secret parser-pg-conn \
  -o jsonpath='{.data}{"\n"}' | tr ',' '\n' | cut -d'"' -f2
# -> one key per line, and alphabetically, because jsonpath
#    JSON-marshals the map and Go sorts map keys:
#      db
#      host
#      pass
#      port
#      url
#      user
#    (the decomposed fields, ADR 0046 — `url` is the composed DSN
#    kept alongside them, not a seventh component)
kubectl -n demo get secret parser-pg-conn \
  -o jsonpath='{.metadata.ownerReferences[0].kind}{"\n"}'   # -> ResourceClaim
```

**8. The Application resumed.**

```sh
kubectl -n demo get application.apprafter.io parser -o jsonpath='{.status.phase}{"\n"}'
# -> Ready
```

**9. The declared claim reference resolved in the Deployment.** The
env-var the manifest bound to `claim.pg.url` renders as a `secretKeyRef`
pointing at the connection Secret's `url` key:

```sh
kubectl -n demo get deployment parser -o \
  jsonpath='{.spec.template.spec.containers[0].env[?(@.name=="DATABASE_URL")].valueFrom.secretKeyRef.name}{"\n"}'
# -> parser-pg-conn
kubectl -n demo get deployment parser -o \
  jsonpath='{.spec.template.spec.containers[0].env[?(@.name=="DATABASE_URL")].valueFrom.secretKeyRef.key}{"\n"}'
# -> url
```

**10. (Optional) connect from a consumer pod.** With a
`postgres:16`-style image you can prove the DSN actually connects:

```sh
kubectl -n demo run dsn-check --rm -it --restart=Never \
  --image=postgres:16 \
  --env="DATABASE_URL=$(kubectl -n demo get secret parser-pg-conn \
    -o jsonpath='{.data.url}' | base64 -d)" \
  -- psql "$DATABASE_URL" -tAc "SELECT 1"          # -> 1
```

**11. Inspect the app via the CLI.**

```sh
apprafter app status parser
apprafter app logs parser --tail 50
```

> `apprafter app status parser` now surfaces, by default, the AppRafter
> phase, the workload Pods and Services with live status, and the
> ResourceClaim provisioning state (provider / ready / Scheduled /
> connection Secret); add `--resources` for the full Argo CD resource
> tree. While the chain is in flight the claim shows `ready=false` until
> the provisioner finishes. The raw view is still:
>
> ```sh
> kubectl -n demo get resourceclaim.apprafter.io parser-pg
> ```

**12. Delete the dependency — the claim is retained.** Removing the
`needs.pg` block (and re-syncing) deletes the claim; for a direct
check:

```sh
kubectl -n demo delete resourceclaim.apprafter.io parser-pg
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
re-create it with a past `retainUntil` — the kubeconfig you are using is
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

The GC fires immediately, but the drop is **phased** — it is no longer
single-shot. CloudNativePG drops a managed role only via an
`ensure: absent` entry, and it cannot drop a role that still owns a
database, so the GC drains in ordered stages across a few
`ROLE_DROP_REQUEUE` (~15s) cycles:

1. the `Database` CR flips to `ensure: absent` and the role entry flips
   to `ensure: absent` (kept, **not** pruned) — the `RetainedClaim`
   **persists** here;
2. CloudNativePG drops the database, then the role, reporting the role
   in `status.managedRolesStatus.byStatus.reconciled`;
3. only after CloudNativePG confirms the role is dropped does the GC
   prune the entry, delete the password Secret, and delete the snapshot.

So **poll** these across a few ~15s cycles rather than expecting the
final pruned / `NotFound` state immediately:

```sh
# Stage 1 (within the first cycle) — both flip to absent; the role entry
# is KEPT as ensure:absent (CNPG drops it), and the snapshot PERSISTS:
kubectl -n cnpg-system get database.postgresql.cnpg.io claim-demo-parser-pg \
  -o jsonpath='{.spec.ensure}{"\n"}'               # -> absent
kubectl -n cnpg-system get cluster.postgresql.cnpg.io platform-postgres -o \
  jsonpath='{.spec.managed.roles[?(@.name=="claim_demo_parser_pg")].ensure}{"\n"}' # -> absent

# Stage 3 (after a few ~15s cycles, once CNPG confirms the drop) — the
# entry is pruned, the password Secret and the snapshot are gone:
kubectl -n cnpg-system get cluster.postgresql.cnpg.io platform-postgres -o \
  jsonpath='{.spec.managed.roles[?(@.name=="claim_demo_parser_pg")].name}{"\n"}'   # -> (empty)
kubectl -n cnpg-system get secret claim-demo-parser-pg-pw           # -> NotFound
kubectl -n apprafter-system get retainedclaim claim-demo-parser-pg  # -> NotFound
```

If the entry is still `ensure: absent` and the snapshot is still present,
that is expected mid-drain — give it a few more ~15s cycles. (A drop that
stays wedged for many cycles is surfaced in the operator logs as a
`role drop BLOCKED (CNPG cannotReconcile)` warning, e.g. the role still
owns a database with live connections.)

## Confirm the database is physically gone

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

CloudNativePG's `ensure: absent` reconcile lags the CR patch, and the GC
drains the DB then the role over a few ~15s cycles (see the phased drop
above), so give these queries a few reconcile cycles before trusting the
result — the database row clears first, then the role row. **If a row is
still present after several cycles, CloudNativePG did NOT honor
`ensure: absent`. Stop here and
[report it](https://github.com/apprafter/apprafter/issues) — the platform
is not doing what it promises, and data you were told was dropped is still
on disk.**

## Checklist — did it work?

A complete run exercises both surfaces AppRafter ships. Check every
box.

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
- [ ] The `claim.pg.url` reference renders as a `secretKeyRef` on the
      Deployment, keyed `url` (step 9).
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
| `status.ready` never `true` | shared CNPG Cluster not Ready, or a provisioner error | `kubectl -n cnpg-system get cluster.postgresql.cnpg.io platform-postgres`; check the operator logs: `kubectl -n apprafter-system logs deploy/apprafter-operator`. |
| `kubectl apply` of the Application rejected | admission webhook: an unknown `needs` key | Use a key from the closed set — `pg`, `jetstream`, `clickhouse`, `redis`, `s3`, `notifications`, `disk`. Providers ship for `pg`, `redis` and `disk`. |
| App starts but the DSN env-var is empty or absent | the manifest declares `needs.pg` but never binds it — nothing is auto-injected | Add `env: { DATABASE_URL: claim.pg.url }` (see *Author the manifest*). |
| Database still present after the GC | CloudNativePG has not reconciled `ensure: absent` yet | Re-run the psql query after a short wait; check the CNPG operator logs in `cnpg-system`. If it persists, stop and [report it](https://github.com/apprafter/apprafter/issues) — data you were told was dropped is still on disk. |

## For contributors

Nothing in this section is needed to run Postgres — it is here for people
changing the platform itself.

**The automated end-to-end script.** It needs a checkout of the AppRafter
repository, which nothing above does. `e2e/needs-pg-walk.sh`
exercises this identical chain on a local k3d cluster, and is the cheap
gate to clear before anyone spends a real one on it:

```sh
bash e2e/needs-pg-walk.sh        # green in ~8-12 min on k3d
```

If that is red, fix it before continuing: working through this guide
by hand only adds value once the automated chain is green.

**One assertion to re-run by hand if you touch the claim controllers.**
Two of them write this claim's status — the scheduler records which
provider it picked, the provisioner records the database it created — and
each writes through server-side apply (SSA) under its own field manager.
Get a field manager wrong in either one and it silently erases the other's
verdict, which no step above would notice. After provisioning, the
scheduler's condition must still read `True`:

```sh
kubectl -n demo get resourceclaim.apprafter.io parser-pg -o \
  jsonpath='{.status.conditions[?(@.type=="Scheduled")].status}{"\n"}'   # -> True
```

## Cleanup

```sh
apprafter app remove parser --keep-data    # leave any retained data in place
apprafter destroy --yes                     # tear down the Tier-1 cluster
```
