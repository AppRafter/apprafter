# ADR 0066: `SharedDatabase` — one database, one credential per consumer

## Status

`Accepted` (2026-09-15). ADR for subphase 2.29 (`plan.md` §2.29).

Released in the same pack as [ADR 0065](0065-probes-and-jetstream-tuning.md)
(subphase 2.28): both change the `Application` CRD, and two CRD upgrades back
to back at a live cluster is risk with no upside.

Follows [ADR 0049](0049-cross-app-sharedvolume.md) (`SharedVolume`) section by
section — an explicitly created, namespaced resource with its own lifecycle,
bound through a `ref` in the claim grammar, whose delete is refused while
referenced. It departs from that record in exactly two places, §2 and §3, and
both departures are argued rather than assumed.

**Amends** [ADR 0052](0052-migration-security-axis.md) with one new trigger
(§5), and takes up [ADR 0043](0043-needs-disk-named-claims.md)'s claim grammar
on the `ref` axis that ADR 0049 §2 already named as the intended cross-cutting
generalisation.

The CNPG schemas quoted in §4 were read out of the pinned chart's CRDs
(`cloudnative-pg` 0.28.2, operator 1.29.1) on 2026-09-15. The operand-image
fact in §4.2 was measured on the owner's live cluster the same day.

## Context

Two applications in one namespace could not share one Postgres database or one
Redis logical DB, and this was established by reading the tree rather than
assumed:

- `#ServiceNeed` has no `ref` shape. Only `#DiskClaim` does, and it binds a
  `SharedVolume`.
- Claim identity is `(application, type, name)` all the way down. The claim
  object is `<app>-<type>[-<name>]`, the Postgres role and database are
  `pg_identifier(ns, claim)`, the Redis ACL user is `claim_<ns>_<claim>_redis`
  with its own `$N`. There is no shared point anywhere in the chain.
- The workaround that does work — declare `needs.pg` solely to open the egress
  CiliumNetworkPolicy rule, then read the neighbour's connection Secret
  through an `env` `secret:` reference — shares one credential with no
  per-application revocation, and breaks when the owning claim is deleted,
  because the Secret is owner-referenced to it.

Separately, **no extension could be enabled on any database at all.** The
provisioner's `Database` builder emits `cluster` / `name` / `owner` / `ensure`
and nothing else; its `Cluster` builder emits `instances` / `storage` /
`resources` / `shared_buffers` and nothing else. There was no manifest field,
no CRD field and no provider-seed knob, so pgvector was unreachable.

The two land together because they are the same file, the same CNPG surface
and the same release, and because a shared database needs an extension story
anyway: the extension belongs to the database, which is now a thing that
outlives its consumers.

## Decision

### 1. A `SharedDatabase` CRD (namespaced, `apprafter.io/v1alpha1`)

```yaml
spec:
  type: pg | redis
  size: small                  # optional; tier-aware default
  selector: {tier: integrated} # optional
  persistent: true             # redis only; rejected on pg
  extensions:                  # pg only — §4
    - {name: vector}
status:
  ready: true
  refCount: 2                  # rm is refused above 0
  database: shd_apps_orders    # pg: the database in the shared cluster
  instance: platform-redis-persistent-000
  dbnum: 7                     # redis: the $N every consumer is pinned to
  conditions: [...]
```

CLI: `apprafter db create | list | status | rm`, mirroring `apprafter volume`.
`rm` is refused while `status.refCount > 0`, naming the applications that
still bind it.

**It deliberately carries no `connectionSecretRef`, and that is the first
departure from `SharedVolume`.** That CRD publishes a `pvcRef` because every
consumer mounts the identical volume. Here every consumer gets its own
credential (§3), so a single shared Secret in the status would be exactly the
thing this design exists to avoid. The status reports what was provisioned,
not how to reach it.

Same-namespace invariant, as ADR 0049 §5, for a different reason: not RWO
semantics but the connection Secret, which a consumer's pod reads from its own
namespace, plus the egress policy, which is written per application namespace.

### 2. `ref` and `access` on the claim grammar

```cue
needs: {
    pg: {ref: "orders-db"}                  // rw by default
    redis: {ref: "cache", access: "ro"}
}
```

`ref` names a `SharedDatabase` in the application's own namespace and is
mutually exclusive with `size`, `persistent` and `selector` — the
`SharedDatabase` already made those decisions. The webhook enforces the
disjunction, mirroring `#DiskClaim`'s owned/ref shapes.

`access` is `rw` (default) or `ro`, and it is the privilege level of **this
application's own credential**. A third `owner` level was considered and
rejected in §Alternatives.

Both fields go on the shared `#ServiceNeed` rather than into a per-type union,
which is what ADR 0049 §2 said it intended — "future need types could adopt a
`ref` field under the same semantics". The webhook rejects `ref` on the need
types with no `SharedDatabase` implementation (`clickhouse`, `s3`,
`notifications`), so the syntax being available to them costs nothing.

**A reference need still generates a `ResourceClaim`, and this is the second
departure from ADR 0049 — the decision that keeps the feature cheap.** A
reference *disk* generates none: the renderer reads `status.pvcRef` and mounts
it. A database reference cannot do that, because something per-consumer must
actually be provisioned — a role, a password, a Secret. So the claim is
generated exactly as today, keeps its `type` and its name, and carries two new
spec fields, `sharedRef` and `access`. Every consequence is "unchanged":
`claim.pg.url` resolves through the existing `needs_secrets` map keyed on
`(type, name)`; the egress rule is keyed on the need type; the 2.4d
`AwaitingResourceClaim` gate is untouched; and GC drops the consumer's role and
Secret while never touching the shared database.

The provisioner branches on `spec.sharedRef`: present means *bind a consumer
to an existing database*, absent means *provision a new one*, which is today's
path verbatim.

### 3. One credential per consumer

#### 3.1 Postgres

On `SharedDatabase` creation: a `NOLOGIN` group role `shd_<ns>_<name>` that
owns the database, a second `NOLOGIN` group `shd_<ns>_<name>_ro` for readers,
and a CNPG `Database` CR owned by the first and carrying `extensions` (§4).

On each consumer bind: a `LOGIN` role `claim_<ns>_<app>_pg` with its own
password, created through CNPG's declarative `managed.roles` — which supports
`inRoles`, so **membership is declarative** — a member of the owner group for
`rw` or the reader group for `ro`. Then, for `rw`, one statement:

```sql
ALTER ROLE <consumer> IN DATABASE <db> SET ROLE <owner group>;
```

and for `ro`, a fixed idempotent block: `GRANT CONNECT`, `GRANT USAGE ON
SCHEMA public`, `GRANT SELECT ON ALL TABLES IN SCHEMA public`, and `ALTER
DEFAULT PRIVILEGES FOR ROLE <owner group> IN SCHEMA public GRANT SELECT ON
TABLES`.

**The `SET ROLE` line is the whole design, not a detail.** Without it a table
created by application A's migration is owned by A's own role, and application
B — a fellow member of the same group — cannot read it. That is the classic
Postgres shared-database failure, and it surfaces as "my neighbour's migration
broke my app" rather than as a permission error anyone recognises. With it,
every object any consumer creates belongs to the group, so membership alone is
sufficient and the single `ALTER DEFAULT PRIVILEGES` line covers every future
table no matter who created it.

**Cost, stated plainly: the provisioner needs a PostgreSQL client.** Today it
reaches Postgres only through CNPG CRs, while already performing imperative
I/O against Redis and NATS — so this is a third instance of an existing
pattern rather than a new kind of thing (ADR 0061 already recorded "the
provisioner performs imperative NATS I/O" as an accepted consequence). The
statement set is small, fixed and idempotent, and that is what makes it
acceptable; if it ever needs to grow into arbitrary SQL, that is the signal to
revisit this decision rather than to extend it.

It connects as a dedicated platform `LOGIN` role created through
`managed.roles` with membership in each shared owner group — membership is
what `ALTER DEFAULT PRIVILEGES FOR ROLE <owner>` requires. **Not the CNPG
superuser**: `enableSuperuserAccess` is off by default, and turning it on to
run four statements would be a far larger grant than the job needs. §7(a)
verifies the privilege set is actually sufficient before this is built.

#### 3.2 Redis

The `SharedDatabase` owns one `$N` on a pool instance, persistent or ephemeral
per `spec.persistent`. Each consumer gets its own ACL user pinned to that same
`$N` — the existing `acl_setuser_args` shape with two changes:

- **The channel pattern becomes the shared one** (`&shd_<ns>_<name>:*`)
  instead of `&<user>:*`. This is not cosmetic. Channels are not DB-scoped, so
  the per-user prefix that correctly isolates *owned* claims would leave two
  consumers of one shared cache unable to see each other's pub/sub — the
  feature would look provisioned and not work. `claim.redis.channelPrefix`
  reports the shared prefix to every consumer.
- **`ro` drops the write categories**: `+@all -@write -@admin -@dangerous
  -move -copy -pubsub +sort_ro`, plus an explicit denial of `publish` /
  `spublish`, because a publish is not a member of `@write`.

Everything else — the `$N` pin, `~*`, `resetkeys` / `resetchannels`, the
`-move` / `-copy` cross-DB denials and the `-pubsub` disclosure denial — is
inherited unchanged from [ADR 0042](0042-needs-redis-dragonfly.md), including
its lesson: **category membership is verified, never assumed** (§7(b)).

### 4. PostgreSQL extensions

A new field on both the owned need and the shared database:

```cue
extensions?: [...#PgExtension]
#PgExtension: {name: string, version?: string, schema?: string}
```

It maps 1:1 onto CNPG's `Database.spec.extensions[]`, whose schema — `name`
(required), `ensure` (`present` | `absent`, default `present`), `version`,
`schema` — was read from the pinned chart's CRD. `ensure` is **not** exposed:
removing an entry from the list means `absent`, and a manifest that says
`ensure: absent` while still listing the extension is a shape with two ways to
say one thing.

One list form, no bare-string sugar. `extensions: ["vector"]` would be terser,
but an element-level `string | #PgExtension` union collapses the item schema to
`x-kubernetes-preserve-unknown-fields` — the route `expose.hostname` had to
take — and losing structural validation of `version` / `schema` to save six
characters is a bad trade.

#### 4.1 The allow list is mandatory, and is not a policy preference

`platform-postgres` is **one cluster shared by every tenant in the Kubernetes
cluster**, and CNPG executes `CREATE EXTENSION` with superuser rights.
Enabling an extension on a manifest's say-so therefore hands the application a
privilege its own role categorically does not have. Refused, permanently:

| extension class | what it grants |
|---|---|
| `dblink`, `postgres_fdw` | outbound network from the database server — egress the application's own CiliumNetworkPolicy does not govern |
| `file_fdw`, `adminpack` | the server's filesystem, which holds every tenant's data files |
| `plpython3u`, `plperlu`, `pltclu` | arbitrary code as the `postgres` OS user |

The allow list lives in the `pg-integrated` ServiceProvider seed
(`config.allowedExtensions`, with a code fallback) so a platform operator can
extend it without a code change — the shape `instances` and `resources`
already use. The webhook enforces it at admission so the refusal names the
field, and the provisioner re-checks it, because a `ResourceClaim` can also be
written directly.

Extensions requiring `shared_preload_libraries` (`pg_cron`,
`pg_stat_statements`) are refused separately and with a different message:
they are server-wide configuration on a cluster serving every tenant, plus a
restart. pgvector is not in that class — it is an ordinary per-database
extension, which is why it is reachable at all.

#### 4.2 The operand image — measured, and left unpinned

**Measured on the owner's live cluster, 2026-09-15.** The running operand
image is `ghcr.io/cloudnative-pg/postgresql:18.3-system-trixie`, and it
already provides `vector` 0.8.2, `pg_trgm` 1.6 and `pgcrypto` 1.4. So
`Database.spec.extensions` is the **entire** mechanism: pgvector needs one
field in the manifest and nothing else. The `Cluster.spec.postgresql.extensions[]`
image-volume route — verified to exist in the pinned CRD — is not needed and
is not built. The three measured extensions seed the allow list with names
known to resolve rather than hoped to.

CNPG still chooses that image from the operator's compiled-in default: nothing
in this repository sets `imageName` or `imageCatalogRef`, and the only mention
of either is a seed comment calling an unpinned default a drift hazard.
**Pinning it is deliberately out of scope here** — it gates nothing in this
record, and the hazard it covers is a different one (§Risks).

What this subphase does owe is detection, and it is nearly free because §3.1
already puts a PostgreSQL client in the provisioner: on reconcile, compare each
database's declared extensions against `pg_available_extensions` and raise a
claim condition `ExtensionUnavailable` naming the extension and the running
image. That covers both directions — a name that never resolves, and a name
that resolved until an operand image changed under a live database. CNPG's own
`Database.status.extensions[]` is read as the second source; neither alone
should leave a claim sitting `Ready=False` with nothing saying why.

### 5. Gating

Binding an application to a `SharedDatabase` for the first time — one gate per
`(application, SharedDatabase)` pair — creates a MigrationPlan and pauses for
approval. A new ADR 0052 trigger, structurally the mirror of #14
(`jetstream-consume-add`): under that record's inverted threat model the
attacker is whoever can write the manifest, and without the gate one line in
an application's own file attaches it to a neighbour's data.

Not gated: a re-bind of a pair that already passed, a narrowing from `rw` to
`ro`, and the creation of the `SharedDatabase` itself — that is already a
human act, and gating it would gate the wrong end. Gated: `ro` → `rw` on an
existing binding, as an escalation.

Unbinding is a `needs.*` removal, which the existing app-scope classifier
already treats as destructive, so it inherits that gate with no new rule.

### 6. Lifecycle

- `SharedDatabase` delete with `refCount > 0` → refused by the webhook, naming
  the binders. At `refCount == 0` the provisioner drops the database (pg) or
  clears the `$N` and releases it to the allocator (redis), then the groups.
- A consumer's claim GC drops that consumer's role and Secret only. The shared
  database and its data are never touched by a consumer's lifecycle — the
  property the whole CRD exists to provide.
- `refCount` is DERIVED from the live claims carrying the `sharedRef` and
  recomputed on reconcile, never incremented and decremented: that is the
  counter that drifts.
- Backup ([ADR 0050](0050-backup-restore.md)) sees a shared database as a
  database like any other. The per-consumer roles are not data and are
  re-derived on restore from the bindings — recorded because a restore that
  silently dropped bindings would present as an outage with no error.

## Consequences

- **Easier.** Two applications can share state without sharing a password; a
  read-only consumer is expressible; revocation is per application; and an
  extension is one manifest field instead of impossible.
- **Harder.** The provisioner gains a third imperative client and a reconcile
  that now depends on the database being reachable, not only on a CR apply
  succeeding. The shared Postgres cluster gains a privileged code path that
  executes SQL on a tenant's behalf, which is what §4.1's allow list bounds.
- **Visible.** The first bind of each pair pauses for approval. Intended, and
  it will be reported as a bug at least once.

## Alternatives considered

- **One shared role per `SharedDatabase`**, every consumer reading one
  connection Secret. Zero new machinery and available immediately. Rejected:
  revocation is all-or-nothing, there is no per-application audit, and the
  platform would deliver exactly what a manual `secret:` reference to a
  neighbour's Secret already delivers — the thing this record exists to
  replace.
- **Per-consumer credentials for redis only**, pg keeping a shared role until
  later. Rejected: the pg migration would be a breaking DSN change for live
  consumers, which is a worse thing to schedule than the client dependency it
  defers.
- **An `owner` access level** distinguishing "may migrate" from "may write".
  Rejected for now: `rw` members already perform DDL through the group role,
  and separating the two needs a privilege split whose most likely outcome is
  a migration that fails at 3am for a reason no error message explains.
  Revisit with a concrete case.
- **Reading declarations from the `Application` rather than the claim.**
  Rejected for the reason ADR 0061 §6 gives: the provisioner already watches
  `ResourceClaim`, and a second Kind means new RBAC, new event plumbing, and a
  resolution whose failure mode is provisioning computed from a stale read.
- **A schema per consumer inside one database** instead of one shared schema
  with group ownership. Rejected: it makes the common case (two services, one
  set of tables) the hard case, and `search_path` becomes a per-consumer
  contract nothing enforces.
- **Arbitrary SQL from the manifest** (schemas, grants to non-platform roles,
  seed data). Rejected: the statement set staying small and fixed is what
  makes §3.1 acceptable at all.

## Risks

- **The shared cluster is a single blast radius.** Per-consumer roles reduce
  it and do not remove it: every tenant's database still lives in one
  `platform-postgres`. Extensions sharpen that, which is what §4.1 exists for,
  and is why the allow list is a refusal rather than a warning.
- **`SET ROLE` is invisible until it is missing.** Everything works in a
  single-consumer test; the failure appears only when a second consumer reads
  the first's tables. The walk must create the table from one application and
  read it from another, in that order, or it proves nothing.
- **The operand image stays unpinned, and this record does not close it.** Two
  failure modes ride on that, and only the smaller one is detected here. (1) A
  CNPG bump changes the default image and an extension a live database uses
  disappears — caught by §4.2's `ExtensionUnavailable` condition, visible and
  recoverable. (2) A CNPG bump moves the default **major** under an existing
  data directory; nothing here detects it, and a live cluster does not survive
  its data directory and its binaries disagreeing. The correct fix is the pin
  plus the bump duty that comes with it — an unbumped pin ages into an
  unpatched Postgres, so it is not free either. Filed as its own item rather
  than carried here.
- **A new imperative failure surface in the provisioner.** Mitigated the same
  way the NATS path is: failures become claim conditions, never a silent
  partial provision.

## Owner

Andrey Ryahovskiy.

## Re-evaluation

- Revisit the `owner` access level if a concrete migration-permission case
  arrives.
- Revisit §3.1's "small, fixed statement set" the first time someone needs a
  statement that is not in it — that is the boundary this decision rests on.
- Revisit §4.2 when the operand image is pinned, at which point
  `ExtensionUnavailable` stops being the only defence.

## 7. Measurements owed before implementation

- **(a)** That a `managed.roles`-created role with membership in the owner
  group, and without superuser, can execute the §3.1 statement set — in
  particular `ALTER DEFAULT PRIVILEGES FOR ROLE <owner>` and `ALTER ROLE … IN
  DATABASE … SET ROLE`. Measured against CNPG 1.29.1 on `kind`.
- **(b)** Dragonfly's category membership for `publish` / `spublish` and for
  the write commands, before `ro` is written as an ACL string. ADR 0042 §2 is
  the precedent: `MOVE` and `COPY` were assumed to be in `@dangerous` and are
  not, which would have left a cross-DB escape.
- **(c)** That `Database.spec.extensions` with `ensure: absent` drops the
  extension rather than erroring when dependent objects exist — it is how a
  removed manifest entry behaves.

## References

- [ADR 0049](0049-cross-app-sharedvolume.md) — `SharedVolume`, the structural
  template; §1 and §2 name the two places this record departs from it.
- [ADR 0043](0043-needs-disk-named-claims.md) — the `(type, name)` claim
  identity this extends on the `ref` axis.
- [ADR 0042](0042-needs-redis-dragonfly.md) — the Dragonfly ACL model §3.2
  inherits, including its verify-don't-assume lesson.
- [ADR 0052](0052-migration-security-axis.md) — the security axis; §5 adds one
  trigger in the shape of #14.
- [ADR 0061](0061-needs-jetstream-nats.md) — the precedent for imperative I/O
  in the provisioner, and for declarations riding the claim.
- [ADR 0046](0046-env-value-references.md) — the `claim.*` / `secret:`
  reference model the workaround in §Context abuses.
- `cloudnative-pg` 0.28.2 / operator 1.29.1 — the `Database.spec.extensions`
  and `Cluster.spec.postgresql.extensions` schemas quoted in §4.
