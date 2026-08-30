---
description: "How a declared needs.redis dependency becomes an isolated logical database: the shared Dragonfly pool, the $N ACL pin, the channel prefix, and the flush after the grace window."
---

# Declared Redis dependencies

The recipe is [Redis](../operator-guide/redis.md). This page is what happens
behind it — the pooling model, how one tenant is walled off from another, and
the shape of the teardown. None of it is needed to use `needs.redis`.

The backend is [Dragonfly](https://www.dragonflydb.io/), a single-process
Redis-compatible server. The full design and the measured per-database overhead
are in [ADR 0042](../adr/0042-needs-redis-dragonfly.md).

## The chain

You declare `needs: { redis: {} }` on an Application.

1. The operator generates a `ResourceClaim` and **pauses** the Application at
   `status.phase=AwaitingResourceClaim`.
2. The scheduler matches the claim to the seeded `redis-integrated`
   `ServiceProvider` and marks it `Scheduled=True`.
3. The provisioner creates a shared Dragonfly instance **lazily** — the first
   claim creates it, later claims reuse it — allocates the claim a numbered
   logical database (`status.dbnum`), and creates an ACL user pinned to that
   database over the Redis protocol.
4. It publishes a connection `Secret` carrying the decomposed fields `url`,
   `user`, `pass`, `host`, `port`, `db` and `channelPrefix`, and marks the claim
   `ready=true`.
5. The Application resumes to `Ready`, and the rendered Deployment carries a
   `secretKeyRef` for every claim field the manifest referenced.

## Why one instance, and what keeps tenants apart

A Dragonfly instance hosts many claims. Each claim gets its own numbered
logical database, and its ACL user carries a `$N` selector pinning it to that
one. That pin is the wall:

- **Keys are naturally confined.** `SET foo` from one claim cannot see,
  enumerate or collide with another claim's `foo`. Applications use ordinary
  key names; there is no prefix to remember and none to get wrong.
- **A second claim's user is refused this claim's database.** `SELECT <other
  dbnum>` returns `NOPERM`. It is an authorisation failure, not a convention.
- **Channels are the exception, because Redis pub/sub is server-wide rather
  than database-scoped.** There is no `$N` equivalent for channels, so the ACL
  instead constrains the user to a channel pattern, and the claim publishes the
  prefix it must use as `channelPrefix` (its value is `<aclUser>:`). Publishing
  or subscribing to an unprefixed channel returns `NOPERM`.

That asymmetry is the one thing an application author has to know, and it is
why the recipe binds two env-vars rather than one.

**Credentials are runtime state on the instance, not stored configuration.** A
Dragonfly restart drops every claim's ACL user, so all of that pool's tenants
fail authentication at once until the provisioner's reconcile re-pins them —
about five minutes. It is self-healing and needs no operator action, but it is
worth recognising, because a simultaneous `WRONGPASS` across unrelated
applications looks like something far worse than a pod that restarted.

**Persistence is an instance-level property**, not a per-database one: Redis
snapshots the whole dataset. So `persistent: true` routes the claim onto a
*separate* persistent instance rather than turning persistence on for one
database of the shared ephemeral one.

## The names are derived, not chosen

For an app `web` in namespace `demo`:

| Object | Name |
| --- | --- |
| `ResourceClaim` | `web-redis` |
| ACL user | `claim_demo_web-redis_redis` |
| Channel prefix | `claim_demo_web-redis_redis:` |
| Connection `Secret` | `web-redis-conn` |
| `RetainedClaim` snapshot | `claim-demo-web-redis` |
| Shared instance | `platform-redis-ephemeral-000` |

## Watching it happen

```sh
# the claim is provisioned, on which instance, in which database
kubectl -n demo get resourceclaim.apprafter.io web-redis -o \
  jsonpath='ready={.status.ready} instance={.status.instance} db={.status.dbnum}{"\n"}'
# -> ready=true instance=platform-redis-ephemeral-000 db=<0..1023>

# the DSN and the channel prefix
kubectl -n demo get secret web-redis-conn -o jsonpath='{.data.url}' | base64 -d; echo
kubectl -n demo get secret web-redis-conn -o jsonpath='{.data.channelPrefix}' | base64 -d; echo
```

Two shapes catch people out when reaching for the instance directly:

- **The exec target is a pod, not a Deployment.** The dragonfly-operator backs
  each instance with a StatefulSet, so the pod is the instance name plus an
  ordinal — `platform-redis-ephemeral-000-0` — and
  `kubectl exec deploy/platform-redis-ephemeral-000` reports
  `deployments.apps … not found`. That StatefulSet also uses the `OnDelete`
  update strategy, so `kubectl rollout status` is unavailable; wait on the pod
  instead.
- **Do not hand `redis-cli` the DSN with `-u`.** The DSN in `claim.redis.url` is
  correct and a real client library will use it, but the redis-cli bundled in
  the Dragonfly image is 6.0.16, whose URL parser drops the userinfo *username*
  and authenticates as `default`. `-u` therefore fails with `NOAUTH` instead of
  logging in as the claim's user — which would make an isolation check "pass"
  for the wrong reason. Use explicit `--user` / `--pass`, and `-n <dbnum>`:
  without the database number redis-cli operates on database 0, where a
  `$N`-pinned user has no rights, so even a correct credential gets `NOPERM`.

## The grace window, and the flush

**Deleting the Application deletes the claim** — the `ResourceClaim` carries an
ownerRef back to it, so the cascade is what starts this. A finalizer writes an
immutable `RetainedClaim` snapshot with `retainUntil` set to deletion + 7 days,
and the connection Secret cascades away, but the ACL user and the database's
contents survive the window.

Editing the manifest is a different path. Dropping a `needs.<type>` key is a
destructive `data-migration` change, so it is gated behind a MigrationPlan and
the Application pauses at `AwaitingMigrationApproval`. Even after approval
nothing deletes the claim: the render path applies claims for *declared* needs
and skips the block when there are none. The retention path runs on Application
deletion, not on a manifest edit.

Once `retainUntil` passes, the GC runs `FLUSHDB` on the claim's database and
`ACL DELUSER` on its user, then removes the snapshot.

**The snapshot is not a knob.** It is immutable by a CEL `self == oldSelf` rule,
and the admission webhook restricts CREATE to the operator's ServiceAccount,
with a deliberate cluster-admin break-glass (`system:masters`,
`kubeadm:cluster-admins`) — so hand-writing one is unsupported rather than
impossible. What makes it a bad idea here, in a way the Postgres equivalent is
not, is what it costs: [ADR
0042](../adr/0042-needs-redis-dragonfly.md) derives the allocator's reserved
database set from live claims **∪** `RetainedClaim`s and never from the running
instance, so deleting a snapshot frees a database number that still holds
another tenant's retained data — and the next claim can be handed it.

There is therefore no supported way to shorten the window, and the guide does
not teach one. What proves the flush actually happened is asserted by
`e2e/needs-redis-walk.sh`, which has the cluster-admin standing to check it;
doing the same by hand means extracting the shared pool's admin password and
running an unrestricted `redis-cli` against an instance that hosts other
tenants' databases, which is a larger risk than the question is worth.

## For contributors

`e2e/needs-redis-walk.sh` exercises the whole chain on a local k3d cluster in
about 8–12 minutes — including the cross-tenant isolation proof (a second
claim's user gets `NOPERM` on the first claim's database) and the post-GC
assertion that the ACL user is physically gone. It needs a checkout of the
AppRafter repository. Clear it before spending a real cluster on the same
ground.

Judge the isolation assertions by the **reply text**, not the exit status, and
fold stderr in so the reply is visible: `NOPERM` is the pass, while `NOAUTH` or
`WRONGPASS` means the login itself failed and the database pin was never
exercised — the check proved nothing.
