---
description: "Letting two applications use one Postgres database or one Redis keyspace without sharing a password: creating the database, binding an application to it read-write or read-only, and what the platform refuses."
---

# Shared databases

Two applications that need the same rows have three options, and only one of
them is good.

They can share a connection string — one password, no way to revoke either
side, no way to make one of them read-only. They can each own a database and
copy data between them. Or one database can exist in its own right, with each
application holding its own credential to it.

A `SharedDatabase` is the third. It works for a PostgreSQL database and for a
Redis keyspace.

## The database is created first, and separately

```bash
apprafter db create orders --type pg -n shop
```

It is not declared in any application's manifest, and that is the point: a
database created by the first application to mention it would go away when
that application does, taking its neighbours' data along. A shared database
outlives every application bound to it, so it is created and deleted as its
own thing.

```bash
apprafter db list -n shop
```

```text
 NAME     TYPE   READY   BOUND   BACKING
 orders   pg     true    0       shd_shop_orders
```

## Binding an application to it

One line in the application's `needs`:

```cue
spec: base: {
    image: "ghcr.io/example/web:v1"
    needs: pg: ref: "orders"
    env: DATABASE_URL: {claim: "pg.url"}
}
```

`DATABASE_URL` resolves to **this application's own credential** for the
shared database. Same rows, different login. Revoking one application's access
is removing its binding; nothing else changes.

A read-only consumer says so:

```cue
    needs: pg: {
        ref:    "orders"
        access: "ro"
    }
```

`ro` is enforced by the database server, not by agreement. A read-only
consumer's `INSERT` is refused by PostgreSQL itself.

## The first binding needs approval

Attaching an application to a shared database is a one-line edit with a large
consequence — it reaches a neighbour's data, and the neighbour is the one who
bears it. So the first binding of each (application, database) pair pauses for
approval, the same gate destructive changes use:

```bash
apprafter migration list
apprafter migration approve <plan>
```

`migration list` spans every namespace, and `approve` resolves the plan's
namespace from its name.

Not gated: re-deploying an application that is already bound, and narrowing a
binding from read-write to read-only. Gated: widening read-only to
read-write, which is a different exposure from the one that was approved.

Also not gated, and worth knowing: a **brand-new** application whose first
version already declares the `ref`. The gate watches for an application
acquiring reach it did not have, and a new application has no previous state
to compare against. Creating one still requires the ability to deploy into the
namespace that holds the data.

## What sharing a Postgres database actually gives you

A table created by one consumer belongs to a **group** that every consumer of
that database is a member of — not to whichever application happened to run
the migration. Without that, the usual shared-database failure appears as "my
neighbour's migration broke my app" rather than as a permission error anyone
recognises.

It means you can run migrations from either side, and both sides keep working.

## Extensions

Extensions belong to the database, so they are declared on it rather than on a
consumer — two consumers could otherwise ask for different sets:

```bash
apprafter db create orders --type pg --extension vector --extension pg_trgm -n shop
```

The list is bounded. `CREATE EXTENSION` runs with superuser rights on a
cluster every tenant shares, so anything that reaches the network, the
server's filesystem, or runs untrusted code is refused. `vector`, `pg_trgm`,
`pgcrypto`, `citext`, `hstore`, `uuid-ossp`, `unaccent`, `btree_gin`,
`btree_gist`, `intarray` and `ltree` are available.

An extension the running database image does not provide is reported on the
database, naming it — rather than leaving you with a dependency that never
becomes ready.

## A shared cache

```bash
apprafter db create events --type redis -n shop
```

```cue
    needs: redis: ref: "events"
    env: {
        REDIS_URL:            {claim: "redis.url"}
        REDIS_CHANNEL_PREFIX: {claim: "redis.channelPrefix"}
    }
```

Both consumers land on the same keyspace **and the same channel prefix**, so
they can publish to each other. Prefix your channel names with
`REDIS_CHANNEL_PREFIX`; that is the range your credential is allowed to use.

`--persistent` keeps the data across the database's own deletion. It applies
to a cache only — on a Postgres database it is refused rather than ignored,
because a silently dropped durability request is the kind you discover after a
restart.

## Deleting one

Refused while anything is bound, by the CLI and by the cluster both:

```text
shared database 'orders' still has 2 bound application(s): reporter-pg, web-pg.
Remove `ref` from each application's `needs` block first — deleting the
database would take their data with it.
```

Removing a consumer — deleting the application, or dropping the `ref` line —
drops **that consumer's credential and nothing else**. The database and its
data are never touched by a consumer's lifecycle. That is the property the
whole thing exists for, and it holds whether the consumer left tidily or was
deleted outright.

Once nothing is bound, `apprafter db rm orders -n shop` deletes the database
and its data. Nothing is retained.

## What is not available

- **Sharing across namespaces.** A `ref` that names another namespace is
  refused with its own message. The namespace is the trust boundary.
- **A third access level between `ro` and `rw`.** Ownership of the shared
  objects stays with the platform's group.
- **Arbitrary SQL from a manifest.** Extensions are the bounded exception.

---

The mechanism — how the roles are built, why the platform executes SQL at all,
and what happens on delete — is [Shared databases on a running
cluster](../how-it-works/shared-databases.md).
