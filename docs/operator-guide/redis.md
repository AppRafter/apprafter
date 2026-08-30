---
description: "Give an application a Redis-compatible cache or store by declaring it in the manifest: how to declare it, bind it, use it correctly, and remove it safely."
---

# Redis from a declared dependency

An application declares that it needs Redis, and the platform provisions an
isolated, Redis-compatible connection for it, publishes the connection details,
and binds them to the env-vars you name. Retiring the application retains the
data for seven days before flushing it.

Each application gets its own logical database on a shared pool, walled off
from every other by the credential it is given. You do not create an instance,
a user, or a password.

## Declare the dependency

```sh
apprafter app scaffold --name web --namespace demo --needs redis
```

That writes a `needs` block into `apprafter/Application.cue`:

```cue
spec: base: {
    // ... image / replicas / expose ...
    needs: {
        redis: { selector: { tier: "integrated" } }
    }
}
```

Add `persistent: true` if the data must survive a restart. Persistence in Redis
is a property of the whole server rather than of one database, so a persistent
claim is placed on a separate persistent instance — a difference in where the
claim lands, not in how you use it. Without it you get a cache: fast, shared,
and empty after a restart.

**Declaring the dependency does not inject anything into your container.** Bind
the env-vars you want:

```cue
spec: base: {
    // ... image / replicas / expose / needs ...
    env: {
        REDIS_URL:            claim.redis.url
        REDIS_CHANNEL_PREFIX: claim.redis.channelPrefix
    }
}
```

The env-var names are yours. `claim.redis.<field>` names a field of the
connection Secret the platform will publish — `url`, `user`, `pass`, `host`,
`port`, `db` and `channelPrefix` are all available.

Check it before you push:

```sh
apprafter app validate
```

Use that rather than a bare `cue vet ./apprafter/...`: the scaffold does not
vendor the schema next to your manifest, so `cue` refuses the import outright.
`app validate` lays the bundled schema and the generated `claim` binding into a
temporary workspace first, which is what makes `claim.redis.*` resolve the same
way it will at sync time.

## Two rules for using the connection

This is the whole of what an application author needs to know, and the second
one is the one that bites.

**Keys: use ordinary names.** No prefix, no namespacing of your own. Your
credential is pinned to your own logical database, so `SET foo` cannot see or
collide with another application's `foo`. Isolation is enforced by the server,
not by a convention you have to remember.

**Channels: you must prefix them.** Redis pub/sub is server-wide rather than
per-database, so there is no equivalent pin for channels. Prefix every channel
name with `claim.redis.channelPrefix` — that is what the second env-var above
is for. This is enforced, not advisory: publishing or subscribing to an
unprefixed channel returns `NOPERM`.

## Register the application

```sh
apprafter app add https://github.com/your-org/web.git \
  --name web --namespace demo --project apps
```

If the repository is private, register the credential first:

```sh
apprafter repo creds add web-creds \
  --url-prefix https://github.com/your-org \
  --type pat --token "$YOUR_PAT"
apprafter repo creds list
```

## Watch it come up

```sh
apprafter app status web
```

The first `needs.redis` in a fresh cluster is the slow one: the platform
creates the shared instance on first use, and later claims reuse it. While that
runs, `app status` reports the claim as not ready and the application as
`AwaitingResourceClaim` — the workload is held back rather than started against
a connection that does not exist yet.

When it finishes you should see the application `Ready`, and the claim with a
provider, `ready`, and a connection Secret. `apprafter app logs web` shows
whether your application picked the connection up.

??? note "Verify independently with kubectl"

    ```sh
    kubectl -n demo get resourceclaim.apprafter.io web-redis -o \
      jsonpath='ready={.status.ready} instance={.status.instance} db={.status.dbnum}{"\n"}'
    # -> ready=true instance=platform-redis-ephemeral-000 db=<0..1023>

    kubectl -n demo get application.apprafter.io web -o jsonpath='{.status.phase}{"\n"}'
    # -> Ready

    kubectl -n demo get secret web-redis-conn -o jsonpath='{.data.url}' | base64 -d; echo
    kubectl -n demo get secret web-redis-conn -o jsonpath='{.data.channelPrefix}' | base64 -d; echo
    ```

    Reaching for the instance itself has two sharp edges — the exec target is a
    StatefulSet pod rather than a Deployment, and `redis-cli -u` silently
    authenticates as the wrong user on the bundled client version. Both are in
    [How it works](../how-it-works/needs-redis.md#watching-it-happen).

## Remove the dependency

**Dropping the `needs.redis` block from the manifest does not take effect on
push.** Removing a declared dependency is classified as a destructive change,
so the platform holds it: the application pauses at
`AwaitingMigrationApproval`, the previously-applied spec keeps running, and
nothing is torn down until you approve the change.

```sh
apprafter migration list
apprafter migration approve <plan-name>
```

The approval gate is covered on [Migration plans](migration-plans.md). Remove
the `env` bindings in the same edit — `claim.redis.*` is generated from the
`needs` block, so a binding left behind fails `apprafter app validate`.

!!! warning "Known gap: the claim is left behind"

    Approving the change does not currently release the data. Nothing deletes
    the claim once its need is undeclared, so the seven-day window never
    starts and the backing database and its credential stays live — and keeps the shared backend
    from scaling down. **Retire the application with `apprafter app remove`
    instead** when you want the retention path; that is the route the platform
    actually implements today. Tracked as a defect.


**To retire the application and start the retention clock, delete it:**

```sh
apprafter app remove web
```

That deletes the Argo CD Application, which prunes the AppRafter `Application`
CR and the claim with it. The connection Secret goes; **the data and the
credential are kept for seven days**, then flushed automatically.

That grace window is the point: retiring an application by accident, or on a
branch, does not destroy data. There is no supported way to shorten it, and the
retention record is deliberately not hand-editable — here it is also what
reserves the database number, so removing it by hand can hand another
application a database that still holds retained data.

`--keep-data` does something different, despite the name: it strips the cascade
first, so **only** the Argo CD object is deleted and the workload, its
`Application` CR and its claim all keep running. Use it to hand an application
off or re-register it elsewhere, not to retire one.

??? note "Verify independently with kubectl"

    Right after `app remove` — the snapshot exists and the connection Secret
    is gone, while the data and the credential are kept:

    ```sh
    kubectl -n apprafter-system get retainedclaim claim-demo-web-redis -o \
      jsonpath='backend={.spec.backend} user={.spec.aclUser} until={.spec.retainUntil}{"\n"}'
    # -> backend=dragonfly user=claim_demo_web-redis_redis until=<RFC3339, ~7 days out>

    kubectl -n demo get secret web-redis-conn        # -> NotFound (cascaded)
    ```

    Confirming the flush itself needs the shared pool's admin credential and an
    unrestricted client against an instance that hosts other applications'
    databases, so the guide does not walk it — `e2e/needs-redis-walk.sh`
    asserts it instead. The reasoning is in
    [How it works](../how-it-works/needs-redis.md#the-grace-window-and-the-flush).

## How it works

[Declared Redis dependencies](../how-it-works/needs-redis.md) covers the shared
pool, what the per-claim credential actually enforces, why channels are the one
exception, how object names are derived, and the teardown after the grace
window.

## Troubleshooting

| Symptom | Likely cause | Fix |
| ------- | ------------ | --- |
| The application stays at `AwaitingResourceClaim` and the claim never gets a provider | the `needs.redis.selector` matches no provider | Confirm the selector reads `tier=integrated`: `kubectl get serviceprovider redis-integrated -n apprafter-system -o yaml`. |
| The claim never reaches `ready` | the shared instance is not up, or the provisioner errored | Check the pod in `dragonfly-system`, then the operator log: `kubectl -n apprafter-system logs deploy/apprafter-operator`. |
| `NOPERM` on publish or subscribe | the channel name is not prefixed | Prefix it with `claim.redis.channelPrefix`. Channels are server-wide, so the prefix is what scopes them — see *Two rules* above. |
| `NOPERM` on an ordinary key operation | the client connected to database 0 instead of the claim's | The DSN carries the database number; a client that ignores it lands on 0, where the credential has no rights. |
| Every application on the pool starts failing `WRONGPASS` or `NOPERM` at once, shortly after a restart | claim credentials are runtime state on the instance and do not survive one | Nothing to do. The platform re-asserts every claim's credential automatically, within about five minutes. If it persists well past that, check the operator log: `kubectl -n apprafter-system logs deploy/apprafter-operator`. |
| The application starts but its Redis env-vars are empty or missing | the manifest declares `needs.redis` but binds nothing — nothing is injected automatically | Add both bindings, as above. |
| Data vanished after a restart | the claim is ephemeral, which is the default | Add `persistent: true` to the need. The claim moves to a persistent instance. |

## Prerequisites

- A Tier-1 cluster provisioned with `apprafter bootstrap-all` (see the
  [Quickstart](quickstart.md)), operator **≥ v0.2.18** — the release that ships
  the always-on dragonfly-operator, the `redis-integrated` provider, and the
  Dragonfly backend in the claim controllers.
- For the verification blocks above only, a kubeconfig:

  ```sh
  apprafter kubeconfig --refresh > /tmp/kc && export KUBECONFIG=/tmp/kc
  ```

## Cleanup

```sh
apprafter app remove web   # the data enters its seven-day window
apprafter destroy --yes    # every apprafter=true resource in the token's project
```
