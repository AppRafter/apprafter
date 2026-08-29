---
description: "How a declared needs.pg dependency becomes a Postgres database: claim, scheduling, lazy shared cluster, connection Secret, and the phased drop after the grace window."
---

# Declared Postgres dependencies

The recipe is [Postgres](../operator-guide/postgres.md). This page is what
happens behind it — the state machine, the names it derives, and the shape of
the teardown. None of it is needed to use `needs.pg`.

## The chain

You declare `needs: { pg: {} }` on an Application.

1. The operator generates a `ResourceClaim` and **pauses** the Application at
   `status.phase=AwaitingResourceClaim`, with a `ResourceClaimPending`
   condition. Nothing is deployed against a database that does not exist yet.
2. The scheduler matches the claim to a `ServiceProvider` — the seeded
   `pg-integrated` — and marks it `Scheduled=True`.
3. The provisioner creates the shared CloudNativePG `Cluster`
   (`platform-postgres`) **lazily**: the first claim in the cluster creates it,
   every later claim reuses it. This is why the first `needs.pg` in a fresh
   cluster is slow and the second is not.
4. It then creates a per-claim Postgres role, a database, and a password
   Secret, and publishes a connection `Secret` carrying the decomposed fields
   `url`, `user`, `pass`, `host`, `port` and `db`. `url` is the composed DSN
   kept alongside the components, not a seventh component.
5. The claim goes `ready=true`, the Application **resumes** to `Ready`, and the
   rendered Deployment carries a `secretKeyRef` for every claim field the
   manifest referenced.

Nothing is injected into your container automatically ([ADR
0046](../adr/0046-env-value-references.md)) — an env-var exists because the
manifest bound one.

## The names are derived, not chosen

Every object above is named deterministically from the claim's
`(namespace, name)`. For an app `parser` in namespace `demo`:

| Object | Name |
| --- | --- |
| `ResourceClaim` | `parser-pg` |
| Postgres role and database | `claim_demo_parser_pg` |
| CloudNativePG `Database` object | `claim-demo-parser-pg` |
| Password `Secret` | `claim-demo-parser-pg-pw` |
| Connection `Secret` | `parser-pg-conn` |
| `RetainedClaim` snapshot | `claim-demo-parser-pg` |

Underscores in Postgres identifiers, hyphens in Kubernetes object names.

**The `parser` in these names is the `apprafter.io` Application's
`metadata.name`** — the one in your repository's `apprafter/Application.cue`,
which is also the rendered Deployment name. It can legitimately differ from the
name passed to `apprafter app add`, which names the *Argo CD* Application. A
lookup that returns "not found" is usually this: the `app add` name was used
where the manifest name belongs.

Kinds are group-qualified for a reason when you do reach for `kubectl`: a bare
`application` also matches Argo CD's `argoproj.io` Application, and a bare
`resourceclaim` matches the Kubernetes 1.32+ DRA `resource.k8s.io` kind.

## Watching it happen

The stages above are observable, one read each. This is the long form of the
guide's verification block — useful when a claim is stuck and you want to know
*which* stage it stopped at.

```sh
# 1 — the Application carries the dependency
kubectl -n demo get application.apprafter.io parser \
  -o jsonpath='{.spec.base.needs.pg.selector.tier}{"\n"}'        # -> integrated

# 2 — a claim was generated, and the Application is gated on it
kubectl -n demo get resourceclaim.apprafter.io parser-pg \
  -o jsonpath='type={.spec.type} tier={.spec.selector.tier} size={.spec.size}{"\n"}'
kubectl -n demo get application.apprafter.io parser \
  -o jsonpath='{.status.phase}{"\n"}'                            # -> AwaitingResourceClaim

# 3 — the scheduler picked a provider
kubectl -n demo get resourceclaim.apprafter.io parser-pg \
  -o jsonpath='{.status.provider}{"\n"}'                         # -> pg-integrated

# 4 — the shared cluster exists (exactly one, however many claims)
kubectl -n cnpg-system get cluster.postgresql.cnpg.io

# 5 — the claim is provisioned and the DSN is published
kubectl -n demo get resourceclaim.apprafter.io parser-pg \
  -o jsonpath='{.status.ready}{"\n"}'                            # -> true
kubectl -n demo get secret parser-pg-conn -o jsonpath='{.data.url}' | base64 -d; echo

# 6 — the binding resolved into the Deployment
kubectl -n demo get deployment parser -o jsonpath\
='{.spec.template.spec.containers[0].env[?(@.name=="DATABASE_URL")].valueFrom.secretKeyRef.key}{"\n"}'
# -> url
```

The connection Secret is owned by the `ResourceClaim`
(`metadata.ownerReferences[0].kind` is `ResourceClaim`), which is what makes it
cascade away when the claim goes.

## The grace window, and the phased drop

Removing the dependency deletes the claim. A finalizer then writes an immutable
`RetainedClaim` snapshot with `retainUntil` set to **deletion + 7 days**, and
the connection Secret cascades away — but the role and the database survive the
window. Deleting a dependency does not delete data.

The snapshot is immutable by a CEL `self == oldSelf` rule, and the admission
webhook accepts a CREATE only from the operator's ServiceAccount:
[RetainedClaims are written by the provisioner's finalizer, never by
hand](https://github.com/apprafter/apprafter/blob/master/operator/admission-webhook/src/validator_retainedclaim.rs).
There is no supported way to shorten the window, and the guide does not teach
one: the snapshot carries the entire work order the GC will execute — role,
database, password Secret, cluster and namespace — so a hand-written substitute
is a way to drop the wrong thing.

Once `retainUntil` passes, the GC drains in **ordered stages**, not in one shot.
CloudNativePG drops a managed role only through an `ensure: absent` entry, and
it cannot drop a role that still owns a database:

1. the `Database` CR flips to `ensure: absent` and the role entry flips to
   `ensure: absent` — kept, not pruned. The `RetainedClaim` still exists here;
2. CloudNativePG drops the database, then the role, reporting it under
   `status.managedRolesStatus.byStatus.reconciled`;
3. only once CloudNativePG confirms the role is gone does the GC prune the
   entry, delete the password Secret, and delete the snapshot.

Each stage is a `ROLE_DROP_REQUEUE` cycle, roughly 15 seconds. An entry still
reading `ensure: absent` with the snapshot still present is mid-drain, not
stuck. A drop that stays wedged across many cycles is reported in the operator
log as `role drop BLOCKED (CNPG cannotReconcile)` — most often a role that
still owns a database with live connections.

**`ensure: absent` is the platform's intent, not proof.** What proves the data
is gone is the catalog, and the automated walk asserts exactly that: after the
GC completes, `pg_database` and `pg_roles` carry no row for the claim's
identifiers. If you are verifying this yourself on a real cluster, that is the
query to run — and a row still present after several cycles is a
[bug worth reporting](https://github.com/apprafter/apprafter/issues), because
data you were told was dropped is still on disk.

## For contributors

**Two controllers write this claim's status** — the scheduler records the
provider it picked, the provisioner records the database it created — and each
writes through server-side apply under its own field manager. Get a field
manager wrong in either and it silently erases the other's verdict, which no
observation in the chain above would notice. The assertion that catches it is
that the scheduler's condition still reads `True` *after* provisioning:

```sh
kubectl -n demo get resourceclaim.apprafter.io parser-pg -o \
  jsonpath='{.status.conditions[?(@.type=="Scheduled")].status}{"\n"}'   # -> True
```

`e2e/needs-pg-walk.sh` exercises the whole chain on a local k3d cluster in
about 8–12 minutes, including the catalog assertion above. It needs a checkout
of the AppRafter repository. Clear it before spending a real cluster on the
same ground.
