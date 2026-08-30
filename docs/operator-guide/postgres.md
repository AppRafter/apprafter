---
description: "Give an application a Postgres database by declaring it in the manifest: how to declare it, bind it, check it, and remove it safely."
---

# Postgres from a declared dependency

An application declares that it needs Postgres, and the platform provisions a
database for it, publishes the connection details, and binds them to the
env-var you name. Retiring the application retains the data for seven days
before dropping it.

You do not create a database, a user, or a password. You do not connect
anything to anything.

## Declare the dependency

Scaffold the manifest with the dependency in place:

```sh
apprafter app scaffold --name parser --namespace demo --needs pg
```

`--needs` is repeatable, and today it accepts the three types a provider ships
for — `pg`, `redis` and `disk` — each with a guide in this section. Anything
else is a clear error naming the supported set. The manifest schema itself
allows a wider closed set (`jetstream`, `clickhouse`, `s3`, `notifications` as
well), so a hand-written need of one of those validates but has no provider to
schedule it.

That writes a `needs` block into `apprafter/Application.cue`:

```cue
spec: base: {
    // ... image / replicas / expose ...
    needs: {
        pg: { selector: { tier: "integrated" }, size: "small" }
    }
}
```

**Declaring the dependency does not inject anything into your container.** Bind
the env-var you want to the field you want:

```cue
spec: base: {
    // ... image / replicas / expose / needs ...
    env: {
        DATABASE_URL: claim.pg.url
    }
}
```

The env-var name is yours. `claim.pg.url` names a field of the connection
Secret the platform will publish — `url`, `user`, `pass`, `host`, `port`, `db`
are all available. The `claim` binding is generated from your own `needs`
block, so referencing a field you did not provision fails to compile rather
than at runtime.

Check it before you push:

```sh
apprafter app validate
```

Use that rather than a bare `cue vet ./apprafter/...`: the scaffold does not
vendor the schema next to your manifest, so `cue` refuses the import outright
with `imports are unavailable because there is no cue.mod/module.cue file`.
`app validate` lays the bundled schema and the generated `claim` binding into a
temporary workspace first — which is also what makes `claim.pg.url` resolve the
same way it will at sync time.

## Register the application

```sh
apprafter app add https://github.com/your-org/parser.git \
  --name parser --namespace demo --project apps
```

If the repository is private, register the credential first:

```sh
apprafter repo creds add parser-creds \
  --url-prefix https://github.com/your-org \
  --type pat --token "$YOUR_PAT"
apprafter repo creds list
```

## Watch it come up

```sh
apprafter app status parser
```

The first `needs.pg` in a fresh cluster is the slow one: the platform creates
the shared Postgres cluster on first use, and later claims reuse it. While that
runs, `app status` reports the claim as not ready and the application as
`AwaitingResourceClaim` — the workload is deliberately held back rather than
started against a database that does not exist yet.

When it finishes you should see the application `Ready`, and the claim with a
provider, `ready`, and a connection Secret. `apprafter app logs parser` shows
whether your application picked the DSN up. Add `--resources` to `app status`
for the full Argo CD resource tree.

??? note "Verify independently with kubectl"

    The CLI reads the same objects; these are the raw ones, if you would
    rather see them yourself.

    ```sh
    kubectl -n demo get resourceclaim.apprafter.io parser-pg \
      -o jsonpath='provider={.status.provider} ready={.status.ready}{"\n"}'
    # -> provider=pg-integrated ready=true

    kubectl -n demo get application.apprafter.io parser \
      -o jsonpath='{.status.phase}{"\n"}'
    # -> Ready

    kubectl -n demo get secret parser-pg-conn -o jsonpath='{.data.url}' | base64 -d; echo
    # -> postgresql://claim_demo_parser_pg:…@platform-postgres-rw.cnpg-system.svc:5432/…
    ```

    The object names are derived from the claim's namespace and name; the
    derivation is in [How it works](../how-it-works/needs-pg.md#the-names-are-derived-not-chosen).

## Remove the dependency

**Dropping the `needs.pg` block from the manifest does not take effect on
push.** Removing a declared dependency is classified as a destructive change,
so the platform holds it: the application pauses at
`AwaitingMigrationApproval`, the previously-applied spec keeps running, and
nothing is torn down until you approve the change.

```sh
apprafter migration list
apprafter migration approve <plan-name>
```

The approval gate is covered on [Migration plans](migration-plans.md). Remove
the `env` binding in the same edit — `claim.pg.url` is generated from the
`needs` block, so a binding left behind fails `apprafter app validate`.

!!! warning "Known gap: the claim is left behind"

    Approving the change does not currently release the data. Nothing deletes
    the claim once its need is undeclared, so the seven-day window never
    starts and the backing database stays live — and keeps the shared backend
    from scaling down. **Retire the application with `apprafter app remove`
    instead** when you want the retention path; that is the route the platform
    actually implements today. Tracked as a defect.


**To retire the application and start the retention clock, delete it:**

```sh
apprafter app remove parser
```

That deletes the Argo CD Application, which prunes the AppRafter `Application`
CR and the claim with it. The connection Secret goes; **the database and its
role are kept for seven days**, then dropped automatically.

That grace window is the point: removing an application by accident, or on a
branch, does not destroy data. There is no supported way to shorten it, and the
retention record is deliberately not hand-editable — it carries the whole work
order the platform will execute, so a hand-written substitute is a way to drop
the wrong database.

`--keep-data` does something different, despite the name: it strips the cascade
first, so **only** the Argo CD object is deleted and the workload, its
`Application` CR and its claim all keep running. Use it to hand an application
off or re-register it elsewhere, not to retire one.

??? note "Verify independently with kubectl"

    Right after `app remove` — the snapshot exists, the connection Secret is
    gone, and the database survives:

    ```sh
    kubectl -n apprafter-system get retainedclaim claim-demo-parser-pg -o \
      jsonpath='role={.spec.role} until={.spec.retainUntil}{"\n"}'
    # -> role=claim_demo_parser_pg until=<RFC3339, ~7 days out>

    kubectl -n demo get secret parser-pg-conn        # -> NotFound (cascaded)

    kubectl -n cnpg-system get database.postgresql.cnpg.io claim-demo-parser-pg \
      -o jsonpath='{.spec.ensure}{"\n"}'             # -> present
    ```

    After the window passes, the drop runs in stages over a few cycles rather
    than at once, and the snapshot is the last thing to go. The staging, and
    what proves the data is physically gone rather than merely marked absent,
    are in [How it works](../how-it-works/needs-pg.md#the-grace-window-and-the-phased-drop).

## How it works

[Declared Postgres dependencies](../how-it-works/needs-pg.md) covers the claim
and scheduling machinery, the lazily-created shared cluster, how object names
are derived, the ordered teardown after the grace window, and the assertion
that proves a database is physically dropped.

## Troubleshooting

| Symptom | Likely cause | Fix |
| ------- | ------------ | --- |
| The application stays at `AwaitingResourceClaim` and the claim never gets a provider | the `needs.pg.selector` matches no provider | Confirm the selector reads `tier=integrated`: `kubectl get serviceprovider pg-integrated -n apprafter-system -o yaml`. |
| The claim never reaches `ready` | the shared Postgres cluster is not up, or the provisioner errored | `kubectl -n cnpg-system get cluster.postgresql.cnpg.io platform-postgres`, then the operator log: `kubectl -n apprafter-system logs deploy/apprafter-operator`. |
| The manifest is rejected on sync | an unknown `needs` key | Use one of `pg`, `jetstream`, `clickhouse`, `redis`, `s3`, `notifications`, `disk`. Providers ship for `pg`, `redis` and `disk`. |
| The application starts but its DSN env-var is empty or missing | the manifest declares `needs.pg` but binds nothing — nothing is injected automatically | Add `env: { DATABASE_URL: claim.pg.url }`, as above. |
| The database is still there long after the grace window | CloudNativePG has not reconciled the drop yet | Give it a few cycles and check the CNPG operator log in `cnpg-system`. If it persists, stop and [report it](https://github.com/apprafter/apprafter/issues) — data you were told was dropped is still on disk. |

## Prerequisites

- A Tier-1 cluster provisioned with `apprafter bootstrap-all` (see the
  [Quickstart](quickstart.md)), operator **≥ v0.2.10** — the release that ships
  the always-on CloudNativePG operator, the `pg-integrated` provider, and the
  claim controllers.
- For the verification blocks above only, a kubeconfig:

  ```sh
  apprafter kubeconfig --refresh > /tmp/kc && export KUBECONFIG=/tmp/kc
  ```

## Cleanup

```sh
apprafter app remove parser   # the database enters its seven-day window
apprafter destroy --yes       # every apprafter=true resource in the token's project
```
