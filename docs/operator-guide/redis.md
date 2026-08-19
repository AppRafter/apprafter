---
description: "Watching a declared Redis dependency all the way through, including the per-claim logical-database isolation on a shared Dragonfly pool."
---

# Redis from a declared dependency

This guide walks a Tier-1 operator through the full `needs.redis` chain:
an Application that declares `spec.base.needs.redis` gets an isolated,
Redis-compatible connection provisioned on demand, its connection
details published in a `Secret` the app binds to env-vars of its
choosing, and — when the dependency is removed — the data retained for a
7-day grace period and then physically flushed.

The backend is [Dragonfly](https://www.dragonflydb.io/), a single-process
Redis-compatible server. Each claim gets its **own numbered logical DB**
on a shared pool instance, pinned by a per-claim `$N` ACL user, so apps
use ordinary key names with complete keyspace separation. The full design
and the measured per-DB overhead are in
[ADR 0042](https://github.com/apprafter/apprafter/blob/master/docs/adr/0042-needs-redis-dragonfly.md).

## The chain in one paragraph

You declare `needs: { redis: {} }` on an Application. The operator
generates a `ResourceClaim` and **pauses** the Application
(`status.phase=AwaitingResourceClaim`). The scheduler matches the claim
to a `ServiceProvider` (the seeded `redis-integrated`) and marks it
`Scheduled=True`. The provisioner lazily creates a shared Dragonfly
instance (first claim only), allocates the claim a numbered logical DB
(`status.dbnum`), creates a `$N`-pinned ACL user over the Redis protocol,
and writes a connection `Secret` carrying the decomposed fields `url`,
`user`, `pass`, `host`, `port`, `db`, `channelPrefix`; it marks the claim
`ready=true`. The Application then **resumes** to `Ready`, and the
rendered Deployment carries a `secretKeyRef` for every claim field the
manifest referenced. When you delete the claim (or remove the
dependency), a finalizer snapshots an immutable `RetainedClaim`
(deletion + 7 days, `backend: dragonfly`) and the connection Secret
cascades away — but the ACL user and the DB's data survive the grace
window. Once `retainUntil` passes, the GC controller runs `FLUSHDB`
(empties the DB) + `ACL DELUSER` (drops the user), deletes the connection
Secret (belt-and-suspenders, it usually cascaded already), and removes
the snapshot.

## How an app uses the connection

- **Keys:** use the DSN (`claim.redis.url`) with **ordinary key names** — no
  prefix. The `$N` ACL pins the user to one logical DB, so `SET foo`,
  `GET foo`, `SCAN` etc. are naturally confined to that DB and cannot
  see, enumerate, or collide with any other tenant's keys.
- **Pub/sub channels:** channels are **server-wide** (not DB-scoped), so
  the app **must** prefix every channel name with the claim's
  `channelPrefix` field (`claim.redis.channelPrefix`, whose value is
  `<aclUser>:`). The ACL enforces it — `PUBLISH`/`SUBSCRIBE` to an
  unprefixed channel returns `NOPERM`.
- **Persistence:** add `persistent: true` to the need to route the claim
  onto a *persistent* pool instance (whole-instance snapshot → PVC)
  instead of the default ephemeral one. Persistence is an instance-level
  property in Redis, so persistent claims land on a separate instance.

## Prerequisites

- A real Tier-1 cluster provisioned with `apprafter bootstrap-all` (see
  [`quickstart.md`](quickstart.md)), operator **≥ v0.2.18** (the release
  that ships the always-on dragonfly-operator, the `redis-integrated`
  ServiceProvider, and the Dragonfly backend in the ResourceClaim
  provisioner / GC controllers).
- `kubectl` bound to the cluster:

  ```sh
  export KUBECONFIG="$(apprafter kubeconfig --refresh)"
  ```

- Pre-flight: the seeded provider, the dragonfly-operator, and the
  admission webhook are all Ready:

  ```sh
  kubectl get serviceprovider redis-integrated -n apprafter-system \
    -o jsonpath='{.metadata.labels.tier} {.spec.backend}{"\n"}'  # -> integrated dragonfly
  kubectl -n dragonfly-system rollout status \
    deploy -l app.kubernetes.io/name=dragonfly-operator           # -> available
  kubectl -n apprafter-system rollout status \
    deploy admission-webhook                                      # -> available
  ```

## Commands used on this page

Every step below runs through the `apprafter` CLI. The `kubectl` and
`redis-cli` reads are there to show you the machine state behind each
one; you never need them to drive the chain.

| Stage | Command |
| ----- | ------- |
| Identity + target | `apprafter target list`, `apprafter target add …`, `apprafter whoami` |
| Self-diagnostic | `apprafter doctor` |
| Provision | `apprafter bootstrap-all` (then re-run `apprafter cluster-bootstrap` once to prove idempotency) |
| Cluster access | `apprafter kubeconfig --refresh`, `apprafter argocd-password` |
| Cluster + platform health | `apprafter status`, `apprafter platform status` |
| Author the manifest | `apprafter app scaffold --needs redis` |
| Register the app | `apprafter app add` (public repo); `apprafter repo creds add/list/show` for the private-repo variant |
| Inspect the app | `apprafter app status`, `apprafter app logs` |
| Portal | `apprafter open argocd` |
| Cleanup | `apprafter app remove --keep-data`, `apprafter destroy --yes` |

### Author the manifest with `apprafter app scaffold`

```sh
apprafter app scaffold --name web --namespace demo --needs redis
```

> The repeatable `--needs redis` flag emits the `spec.base.needs` block
> for you; an unknown type is a clear error. The generated
> `apprafter/Application.cue` carries:
>
> ```cue
> spec: base: {
>     // ... image / replicas / expose ...
>     needs: {
>         redis: { selector: { tier: "integrated" } }
>     }
> }
> ```
>
> `cue vet ./apprafter/...` validates it locally before you push.

`needs.redis` provisions the logical DB and publishes its connection
`Secret`, but injects nothing into your container (ADR 0046). Bind the
env-vars you want to the claim fields yourself:

```cue
spec: base: {
    // ... image / replicas / expose / needs ...
    env: {
        REDIS_URL:            claim.redis.url
        REDIS_CHANNEL_PREFIX: claim.redis.channelPrefix
    }
}
```

The env-var names are yours to pick; `claim.redis.<field>` names a field
in the connection `Secret`. The rest of this guide assumes both bindings
are in place.

### Register the app with `apprafter app add`

Public repo:

```sh
apprafter app add https://github.com/your-org/web.git \
  --name web --namespace demo --project apps
```

Private repo — register the git credential first:

```sh
apprafter repo creds add web-creds \
  --url-prefix https://github.com/your-org \
  --type pat --token "$YOUR_PAT"
apprafter repo creds list           # confirm it is registered
apprafter repo creds show web-creds
```

## Happy chain — steps 1 to 12

The numbered steps below assume the Application CR is in the cluster (via
Argo CD sync after `app add`, or applied directly for a quick check). The
`kubectl` lines are the machine-readable proof behind each CLI surface;
the constants (`demo` namespace, `web` app, `web-redis` claim, ACL user
`claim_demo_web-redis_redis`, connection Secret `web-redis-conn`) are
derived deterministically from the claim's `(namespace, name)`.

> **Naming — the `web` you query is the apprafter.io `metadata.name`, not
> the `apprafter app add` name.** The steps address
> `application.apprafter.io web` and `resourceclaim.apprafter.io
> web-redis`: the kinds are group-qualified on purpose, because bare
> `application` also matches Argo CD's `argoproj.io` Application and bare
> `resourceclaim` matches the k8s 1.32+ DRA `resource.k8s.io`
> ResourceClaim. If `kubectl get application.apprafter.io <name>` returns
> "not found", you likely used the `app add` name; use the
> `Application.cue` `metadata.name` instead.

**1. The Application carries the dependency.**

```sh
kubectl -n demo get application.apprafter.io web \
  -o jsonpath='{.spec.base.needs.redis.selector.tier}{"\n"}'   # -> integrated
```

**2. The operator generated a ResourceClaim.**

```sh
kubectl -n demo get resourceclaim.apprafter.io web-redis \
  -o jsonpath='type={.spec.type} tier={.spec.selector.tier}{"\n"}'
# -> type=redis tier=integrated
```

**3. The Application is gated.**

```sh
kubectl -n demo get application.apprafter.io web -o jsonpath='{.status.phase}{"\n"}'
# -> AwaitingResourceClaim
kubectl -n demo get application.apprafter.io web -o \
  jsonpath='{.status.conditions[?(@.type=="ResourceClaimPending")].status}{"\n"}'
# -> True
```

**4. The scheduler matched a provider.**

```sh
kubectl -n demo get resourceclaim.apprafter.io web-redis \
  -o jsonpath='{.status.provider}{"\n"}'                    # -> redis-integrated
kubectl -n demo get resourceclaim.apprafter.io web-redis -o \
  jsonpath='{.status.conditions[?(@.type=="Scheduled")].status}{"\n"}'   # -> True
```

**5. The shared Dragonfly instance was lazily created.** The first
ephemeral claim creates `platform-redis-ephemeral-000`; later ephemeral
claims reuse it (each on its own numbered DB). A `persistent: true` claim
lands on `platform-redis-persistent-000` instead.

```sh
kubectl -n dragonfly-system get dragonfly.dragonflydb.io
# exactly one instance for the ephemeral class: platform-redis-ephemeral-000
```

**6. The claim is provisioned.** (The lazy instance boot makes this the
slow step.)

```sh
kubectl -n demo get resourceclaim.apprafter.io web-redis -o \
  jsonpath='ready={.status.ready} instance={.status.instance} db={.status.dbnum}{"\n"}'
# -> ready=true instance=platform-redis-ephemeral-000 db=<0..1023>
```

The `$N` ACL user exists on the instance (exec the Dragonfly pod as the
admin user — the admin password is in the per-instance admin Secret):

```sh
ADMIN_PW=$(kubectl -n dragonfly-system get secret \
  platform-redis-ephemeral-000-admin -o jsonpath='{.data.password}' | base64 -d)
kubectl -n dragonfly-system exec deploy/platform-redis-ephemeral-000 -- \
  redis-cli -a "$ADMIN_PW" --no-auth-warning \
  ACL GETUSER claim_demo_web-redis_redis
# -> the user's rules, including the `$<dbnum>` selector and `&claim_demo_web-redis_redis:*` channel pattern
```

**7. The connection Secret carries the decomposed fields.**

```sh
kubectl -n demo get resourceclaim.apprafter.io web-redis \
  -o jsonpath='{.status.connectionSecretRef}{"\n"}'         # -> web-redis-conn
kubectl -n demo get secret web-redis-conn \
  -o jsonpath='{.data.url}' | base64 -d; echo
# -> redis://claim_demo_web-redis_redis:…@platform-redis-ephemeral-000.dragonfly-system.svc:6379/<dbnum>
kubectl -n demo get secret web-redis-conn \
  -o jsonpath='{.data.channelPrefix}' | base64 -d; echo
# -> claim_demo_web-redis_redis:
kubectl -n demo get secret web-redis-conn \
  -o jsonpath='{.metadata.ownerReferences[0].kind}{"\n"}'   # -> ResourceClaim
```

**SSA-split guard.** The provisioner's status write must NOT clobber the
scheduler's verdict — `Scheduled` is still `True` after provisioning:

```sh
kubectl -n demo get resourceclaim.apprafter.io web-redis -o \
  jsonpath='{.status.conditions[?(@.type=="Scheduled")].status}{"\n"}'   # -> True
```

**8. The Application resumed.**

```sh
kubectl -n demo get application.apprafter.io web -o jsonpath='{.status.phase}{"\n"}'
# -> Ready
```

**9. Both declared claim references resolved in the Deployment.**

```sh
kubectl -n demo get deployment web -o \
  jsonpath='{.spec.template.spec.containers[0].env[?(@.name=="REDIS_URL")].valueFrom.secretKeyRef.name}{"\n"}'
# -> web-redis-conn
kubectl -n demo get deployment web -o \
  jsonpath='{.spec.template.spec.containers[0].env[?(@.name=="REDIS_CHANNEL_PREFIX")].valueFrom.secretKeyRef.name}{"\n"}'
# -> web-redis-conn
```

**10. (Optional) prove the DSN connects and is DB-isolated.** With the
`redis-cli` baked into the Dragonfly image you can run as the claim's
own user from inside the cluster:

```sh
DSN=$(kubectl -n demo get secret web-redis-conn \
  -o jsonpath='{.data.url}' | base64 -d)
kubectl -n dragonfly-system exec deploy/platform-redis-ephemeral-000 -- \
  redis-cli -u "$DSN" --no-auth-warning SET hello world   # -> OK
kubectl -n dragonfly-system exec deploy/platform-redis-ephemeral-000 -- \
  redis-cli -u "$DSN" --no-auth-warning GET hello         # -> world
```

A *second* claim's user is denied this claim's DB (the `$N` pin is a hard
wall — the isolation proof). Provision a second app (`api`) the same way,
then:

```sh
# As the api user, SELECT of web's DB number returns NOPERM:
DSN2=$(kubectl -n demo get secret api-redis-conn -o jsonpath='{.data.url}' | base64 -d)
WEB_DB=$(kubectl -n demo get resourceclaim.apprafter.io web-redis -o jsonpath='{.status.dbnum}')
kubectl -n dragonfly-system exec deploy/platform-redis-ephemeral-000 -- \
  redis-cli -u "$DSN2" --no-auth-warning SELECT "$WEB_DB"
# -> NOPERM ... (api cannot reach web's DB)
```

**11. Inspect the app via the CLI.**

```sh
apprafter app status web
apprafter app logs web --tail 50
```

> `apprafter app status web` surfaces, by default, the AppRafter phase,
> the workload Pods and Services with live status, and the ResourceClaim
> provisioning state (provider / ready / Scheduled / connection Secret);
> add `--resources` for the full Argo CD resource tree. While the chain
> is in flight the claim shows `ready=false` until the provisioner
> finishes.

**12. Delete the dependency — the data is retained.** Removing the
`needs.redis` block (and re-syncing) deletes the claim; for a direct
check:

```sh
kubectl -n demo delete resourceclaim.apprafter.io web-redis
kubectl -n apprafter-system get retainedclaim claim-demo-web-redis -o \
  jsonpath='claim={.spec.claimRef.name} backend={.spec.backend} user={.spec.aclUser} until={.spec.retainUntil}{"\n"}'
# -> claim=web-redis backend=dragonfly user=claim_demo_web-redis_redis until=<RFC3339, ~7 days out>
kubectl -n demo get secret web-redis-conn          # -> NotFound (cascaded)
```

The ACL user (and the DB's data) survive the grace floor — GC has not
fired:

```sh
ADMIN_PW=$(kubectl -n dragonfly-system get secret \
  platform-redis-ephemeral-000-admin -o jsonpath='{.data.password}' | base64 -d)
kubectl -n dragonfly-system exec deploy/platform-redis-ephemeral-000 -- \
  redis-cli -a "$ADMIN_PW" --no-auth-warning ACL GETUSER claim_demo_web-redis_redis
# -> still present
```

## Confirm the data and the ACL user are physically gone

The `RetainedClaim` is immutable (a CEL `self == oldSelf` rule), so an
in-place `kubectl patch` of `retainUntil` is **rejected**. Delete it and
re-create it with a past `retainUntil` — the kubeconfig you are using is
`system:masters`, which the operator-only webhook permits to CREATE:

```sh
WEB_DB=$(kubectl -n apprafter-system get retainedclaim claim-demo-web-redis \
  -o jsonpath='{.spec.dbnum}')   # capture before deleting the snapshot
kubectl -n apprafter-system delete retainedclaim claim-demo-web-redis

kubectl apply -f - <<YAML
apiVersion: apprafter.io/v1alpha1
kind: RetainedClaim
metadata:
  name: claim-demo-web-redis
  namespace: apprafter-system
spec:
  claimRef:
    name: web-redis
    namespace: demo
  provider: redis-integrated
  backend: dragonfly
  instance: platform-redis-ephemeral-000
  dbnum: ${WEB_DB}
  aclUser: claim_demo_web-redis_redis
  connectionSecretRef: web-redis-conn
  connectionSecretNamespace: demo
  retainUntil: "2000-01-01T00:00:00Z"
YAML
```

The GC fires immediately. It runs `FLUSHDB` on the claim's DB and
`ACL DELUSER` on the claim's user, then deletes the snapshot. Confirm the
user is **physically gone from the instance** — not merely that the
snapshot was removed:

```sh
ADMIN_PW=$(kubectl -n dragonfly-system get secret \
  platform-redis-ephemeral-000-admin -o jsonpath='{.data.password}' | base64 -d)

# The ACL user is dropped:
kubectl -n dragonfly-system exec deploy/platform-redis-ephemeral-000 -- \
  redis-cli -a "$ADMIN_PW" --no-auth-warning ACL GETUSER claim_demo_web-redis_redis
# -> (empty / nil — the user no longer exists)

# The DB is empty (FLUSHDB ran):
kubectl -n dragonfly-system exec deploy/platform-redis-ephemeral-000 -- \
  redis-cli -a "$ADMIN_PW" --no-auth-warning -n "$WEB_DB" DBSIZE
# -> 0

# The snapshot is gone:
kubectl -n apprafter-system get retainedclaim claim-demo-web-redis   # -> NotFound
```

The freed DB number returns to the pool implicitly — the next allocation
scan reads only LIVE claims' `status.dbnum`, so this DB is reusable.
**If the ACL user is still present after the snapshot is gone, the GC did
NOT run `ACL DELUSER` — STOP, this is a closure-blocking bug.**

## Checklist — did it work?

A complete run exercises both surfaces AppRafter ships. Check every
box.

**Surface 1 — Argo CD UI (`apprafter open argocd`):**

- [ ] The bootstrap / platform Application is **Synced** + **Healthy**.
- [ ] The `redis-integrated` ServiceProvider resource shows green.
- [ ] The `platform-redis-ephemeral-000` Dragonfly instance shows green
      (created lazily on the first claim).

**Surface 2 — kubectl / redis-cli assertions:**

- [ ] ResourceClaim `web-redis` reaches `status.ready=true` with a
      `status.instance` + `status.dbnum` (steps 2, 4, 6).
- [ ] Application `web` transitions `AwaitingResourceClaim → Ready`
      (steps 3, 8).
- [ ] The `claim.redis.url` **and** `claim.redis.channelPrefix`
      references render as `secretKeyRef`s on the Deployment (step 9).
- [ ] **Isolation: a second claim's user gets `NOPERM` on the first
      claim's DB** (step 10).
- [ ] Deleting the claim cascades the connection Secret + writes a
      `RetainedClaim` (`backend: dragonfly`); the ACL user survives the
      grace floor (step 12).
- [ ] **Forcing the GC runs `FLUSHDB` + `ACL DELUSER` — the user is gone
      and the DB is empty — and the snapshot is removed.**

## Troubleshooting

| Symptom | Likely cause | Fix |
| ------- | ------------ | --- |
| Claim stuck `Scheduled` absent / Application stuck `AwaitingResourceClaim` | `needs.redis.selector` does not match any provider's `metadata.labels` | Confirm `selector.tier=integrated` and that `redis-integrated` carries `tier=integrated`: `kubectl get serviceprovider redis-integrated -n apprafter-system -o yaml`. |
| `status.ready` never `true` | shared Dragonfly instance not Ready, or a provisioner error reaching it over the Redis protocol | `kubectl -n dragonfly-system get dragonfly.dragonflydb.io`; check the operator logs: `kubectl -n apprafter-system logs deploy/apprafter-operator`. |
| `kubectl apply` of the Application rejected | admission webhook: an unknown `needs` key | Use only the closed `needs` key set. |
| App starts but the Redis env-vars are empty or absent | the manifest declares `needs.redis` but never binds it — nothing is auto-injected | Add `env: { REDIS_URL: claim.redis.url }` (see *Author the manifest*). |
| App gets `NOPERM` on a pub/sub channel | the app published/subscribed to an unprefixed channel | Prefix every channel name with the claim's `channelPrefix`; keys need no prefix. |
| ACL user missing after a Dragonfly pod restart | runtime ACL users are in-memory and lost on reload | The reconcile loop re-pins them on instance readiness; if it lingers, check the operator logs for the ACL reconcile task. |

## For contributors — the automated end-to-end script

Optional, and it needs a checkout of the AppRafter repository — nothing
above does. If you are changing the platform, `e2e/needs-redis-walk.sh`
exercises this identical chain on a local k3d cluster (including the
`$N`-ACL isolation proof and the `FLUSHDB`/`DELUSER` GC proof), and is
the cheap gate to clear before anyone spends a real one on it:

```sh
bash e2e/needs-redis-walk.sh        # green in ~8-12 min on k3d
```

If that is red, fix it before continuing: working through this guide
by hand only adds value once the automated chain is green.

## Cleanup

```sh
apprafter app remove web --keep-data    # leave any retained data in place
apprafter destroy --yes                  # tear down the Tier-1 cluster
```
