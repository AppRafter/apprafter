---
description: "Why the platform runs SQL to build a shared database's roles instead of declaring them, why one statement is what makes sharing work at all, what a reference count gates, and what a delete does in which order."
---

# Shared databases on a running cluster

The recipe is [Shared databases](../operator-guide/shared-databases.md). This
page is the mechanism and the parts that are easy to misread from outside. An
application developer needs none of it.

The reasoning behind each decision is [ADR
0066](../adr/0066-shared-database.md).

## Why the platform executes SQL at all

Everything else the platform provisions is declared on an object and left to a
controller. A shared database's roles are not: the platform connects and runs
statements.

That is forced, and it was measured rather than assumed. The database
operator's declarative role support creates exactly one role here — the
platform's own. It cannot create the rest, because a role with permission to
create roles, but without superuser, may administer only the roles it created
itself. With the groups created by somebody else, both of the statements this
design needs are refused outright.

So the platform role creates the groups, grants itself membership in them, and
creates each consumer's role. The alternative — making the platform role a
superuser — would also mean extension creation ran for anything a manifest
named, rather than only for what the allow list permits.

## One statement is the whole design

When a consumer connects, its session assumes the group identity for that
database. Everything it creates is therefore owned by the group.

Without it, a table created by one consumer's migration belongs to that
consumer, and the next consumer cannot read it. That is the classic
shared-database failure, and it presents as "my neighbour's migration broke my
app" rather than as a permission error anyone recognises.

With it, membership alone is enough, and a single grant covers every future
table no matter which consumer creates it.

## Each consumer's credential is its own

The database's status publishes what was provisioned — the database name, or
the cache instance and its keyspace number. It publishes no connection secret,
deliberately. A shared volume can publish one reference because every consumer
mounts the identical object; here every consumer has a different login, and a
shared secret in the status would be the thing this whole mechanism replaces.

Each consumer's password is read back from its own secret rather than
regenerated on each pass. An owned database can regenerate freely, because one
component owns both the server's copy and the secret and writes them together.
Here they are two steps, and between them is a window in which the server has
the new password and the secret still has the old one — an application
restarting in that window would fail to authenticate on a reconcile that
changed nothing it asked for.

## The reference count is derived, and it gates a delete

It is recomputed from the live bindings on every reconcile, never incremented
and decremented. An incremented counter drifts, and a drifted zero here
deletes a database two applications are using.

A binding already being removed does not count. Its credential is on its way
out, and holding the database hostage to a removal that has started would read
as a flaky command rather than as a guard.

**Two gates cover each other.** The cluster refuses the delete by reading the
count off the object; the platform's own cleanup step recounts from the live
bindings before dropping anything. A stale non-zero count refuses a delete
that would have been fine, and you retry. A stale zero gets past the first
gate and is caught by the second. The expensive direction has the accurate
gate.

Without the first gate the delete would be admitted and the cleanup step would
hold the object indefinitely — a command that appeared to succeed followed by
an object that never goes away, which is worse than a refusal because nothing
says why.

## What a delete does, in order

For a Postgres database: the database is declared absent, and only then are
the groups dropped. The order is forced — the owning group owns the database,
and Postgres refuses to drop a role that owns one. A first pass that cannot
yet drop the groups is expected; the next one completes it.

For a cache: the keyspace is flushed. The number itself needs no release,
because the allocator derives which numbers are taken from the objects that
exist.

Both resolve where the backing lives from the provider rather than assuming
it, so moving the shared cluster or the cache pool does not silently orphan a
database while its object disappears.

## Why a shared cache shares a channel prefix

Keys in a cache are confined to the keyspace number each credential is pinned
to. Channels are not — the channel namespace spans the whole instance, so a
credential's channel range has to be stated separately.

An owned cache gets a range named after its own user. A shared one gets a
range named after the **database**, so its consumers can publish to each
other. A per-consumer range would pass every check and leave two consumers of
one cache unable to hear each other, which is most of what sharing a cache is
for.

## What the count does not protect

A shared cache's keyspace number is held in the database's own status and in
no binding. An allocator reading only bindings would hand the same number out
again and put two unrelated tenants in one keyspace, where either one's
cleanup wipes the other. The allocator therefore reads shared databases as a
third source alongside live and retained bindings, and that source is a
required input rather than an optional one — so a future caller has to
consider it rather than omit it.

## Where each rule is enforced

| Rule | Enforced by |
| --- | --- |
| the database type, the extension-name alphabet | the custom resource definition |
| `ref` is incompatible with `size` / `persistent` / `selector` | the admission webhook |
| `access` without `ref` | the admission webhook |
| the extension allow list | the webhook **and** the platform, which re-checks |
| a delete while bindings exist | the webhook **and** the cleanup step |
| read-only actually being read-only | the database server |

The duplication is intentional in each case. The webhook covers what the
definition cannot express; the platform re-checks the allow list because a
binding object can be written directly; and the last row is the one that
matters most — an access level the platform merely recorded, rather than
enforced, would be an application believing itself read-only and not being.
