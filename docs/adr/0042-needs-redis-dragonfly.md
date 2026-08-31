# ADR 0042: needs.redis → Dragonfly — per-database isolation (`$N` ACL) on a pool of lazy shared instances

## Status

`Accepted` (2026-06-05). **§7 (connection contract) superseded in part by
[ADR 0046](0046-env-value-references.md)** (2026-06-10): the connection
Secret carries decomposed keys (`url`, `host`, `port`, `user`, `pass`, `db`,
`channelPrefix`) instead of the composed `REDIS_URL`/`REDIS_CHANNEL_PREFIX`
keys described below, and apps bind them via explicit `claim.redis.<field>`
env references rather than platform auto-injection. §§1–6 and §8 (pool
architecture, `$N` ACL isolation, DB-number allocation/recycling, the
reconcile loop, scripting policy, GC) are unaffected and remain in effect.
**Extended by §9** (2026-08-21): shared-instance reaping and unconditional
volume preservation.

## Context

Phase 2.6 ships `Application.spec.base.needs.redis` — a developer declares a Redis dependency and gets a working connection string, the same way `needs.pg` (Phase 2.4, decomposed in `plan.md` 2.4a–g) yields a Postgres `DATABASE_URL`. The generic machinery 2.4 built is reused as-is: the Application controller generates a child `ResourceClaim` and pauses on `AwaitingResourceClaim` (2.4d); the 2.3 scheduler matches a `ServiceProvider`; the `resourceclaim-provisioner` controller provisions per-claim resources and writes `status.ready`/`connectionSecretRef` (2.4c); `operator-rendering` injects the connection Secret as env (2.4e); a finalizer snapshots a `RetainedClaim` and a 7-day-grace GC reclaims it (2.4f). The provisioner already dispatches on `ServiceProvider.spec.backend` (`Backend::from_spec_backend`); `cloudnative-pg` is wired, a `dragonfly` arm is the slot.

Redis differs from Postgres in ways that force genuinely new decisions:

- **Isolation is weak by default.** Logical DBs (`SELECT n`) are not a security boundary in stock Redis — any authenticated client can `SELECT` any DB — and `requirepass` is a single **server-global** password. So the `plan.md` 2.6 sketch ("requirepass per-claim, DB-namespace per claim") cannot isolate as written. Real isolation needs **Redis ACL users** (`ACL SETUSER`).
- **Dragonfly closes the DB gap.** Dragonfly has a non-standard ACL **database selector `$N`** that pins a user to one logical DB: `SELECT` of any other DB returns `NOPERM`. Combined with numbered databases, this gives **complete keyspace separation per DB** — each claim owns a whole DB, apps use ordinary key names (no prefix), and `SCAN` is naturally confined to the selected DB (no cross-tenant key-name enumeration). This is strictly cleaner than key-prefix isolation (see Alternatives).
- **Two boundaries, not one.** Keys are DB-scoped, but **pub/sub channels are not** — `PUBLISH`/`SUBSCRIBE` are server-wide. So channel isolation still needs an ACL channel-pattern (`&prefix:*`), and on Dragonfly/Redis 7+ channels default to *restrictive* (no access), so a user with no channel rule cannot use pub/sub at all.
- **ACL on a shared instance is runtime state, not declarative.** Neither a bare Dragonfly nor the `dragonfly-operator` exposes a declarative per-ACL-user resource (the `Dragonfly` CR manages *instances*; its `aclFromSecret` mounts a static ACL **file**, with conflicting signals on whether that file can carry key/channel patterns). Per-claim users must be created **imperatively** over the Redis protocol and **re-asserted after any instance restart**, because runtime `ACL SETUSER` users live only in memory and are lost when the pod reloads from its file/flags.
- **DB numbers are finite and recycled.** The number of logical DBs is the `--dbnum` flag (default 16). Unlike an unbounded prefix space, DB numbers are a small finite pool per instance that must be **allocated and recycled**, and `--dbnum` is very likely **not runtime-mutable** (Dragonfly documents that only some flags are `CONFIG SET`-able, and `dbnum` is not among the known-mutable set), so growing it means restarting the instance.
- **Persistence and backup are whole-instance.** RDB snapshots and AOF cover the entire dataset (all DBs); there is no per-DB snapshot/backup/restore. So persistence is an **instance-level** property, and per-tenant durability/restore isolation requires a **separate instance** — numbered DBs do not change this.

Dragonfly is a single-process, multi-threaded Redis-compatible server with first-class ACL (commands, key patterns, channel patterns, `$N` DB selector), an embedded Lua 5.4 interpreter that **requires keys to be declared** and rejects undeclared-key access by default, and an official Kubernetes operator (`dragonflydb/dragonfly-operator`) handling instance lifecycle, auth (`passwordFromSecret`/`aclFromSecret`), replication, and snapshots (PVC and S3). On Dragonfly, `ACL SETUSER` propagates to already-authenticated connections and `ACL DELUSER` closes the user's connections.

## Decision

We will provision `needs.redis` onto a **pool of lazy shared Dragonfly instances**, isolating each claim in its **own numbered logical DB** via a **`$N`-pinned ACL user** (plus a channel-prefix for pub/sub), with claim→`(instance, dbnum)` allocation, a reconcile loop for ACL durability, and `FLUSHDB`-based GC.

### 1. Deployment — operator + a pool of lazy shared instances

`platform-stack` installs the `dragonfly-operator` as an always-on component (mirroring the always-on CNPG operator) and seeds a `redis-integrated` `ServiceProvider` (`type: redis`, `backend: dragonfly`). The provisioner manages a **pool** of shared `Dragonfly` CRs per persistence class in `dragonfly-system` (`platform-redis-ephemeral-<NNN>`, `platform-redis-persistent-<NNN>`, the persistent ones with `snapshot.persistentVolumeClaimSpec` + cron). Instances are created lazily; a solo cluster with no Redis apps runs no Dragonfly pod. **Initially the pool may be a single instance per class** — but the allocator (§3) is written to span N instances, so adding capacity later is a config change, not a redesign. Higher tiers get HA/replication via the same CR (`replicas: N`).

Instance-level invariants the provisioner sets on creation:
- `--dbnum = 1024` — **measured 2026-06-05 (see Pre-merge verification).** `dbnum` is the hard max (`kMaxDbId = 1024`; `1025` exits with *dbnum is too big*), and a high `dbnum` is **free at idle**: Dragonfly allocates per-DB structures lazily on first write, so empty databases cost nothing (idle RSS is flat across `dbnum` 16→1024). We set `dbnum` to its ceiling 1024 once. It is **immutable at runtime** (`CONFIG SET` → READONLY) — restart-only — so capacity grows **horizontally** (a new pool instance) at 1024 claims/instance, never by bumping `dbnum`.
- `--num_shards = 1` (default, **all tiers**) — **measured.** Each *active* (key-bearing) database costs a fixed **~280 kB × `num_shards`** DashTable floor, independent of key count, because the per-DB table is allocated per shard. The dragonfly-operator sets **neither** `--dbnum` **nor** `--num_shards`, so an unpinned instance auto-shards to the node's vCPU count and per-tenant memory would scale with cores. We therefore pin `--num_shards=1` on the pool-instance template (via the CR `spec.args`) on every tier, minimising per-claim density cost (a 1-shard instance holds its full 1024-claim cap for ~287 MB of structural overhead). The shard count is an **operator-tunable override on the platform manifest** — exposed on the `redis-integrated` `ServiceProvider` seed / platform values (the same surface that already carries tier-aware backend sizing, e.g. CNPG `instances`) — so a deployment wanting more Redis throughput raises it by hand without a code change; the per-claim-instance tier remains the escape hatch for a single throughput-bound tenant. **Verify the pin takes effect in-cluster** — Dragonfly historically ignored `--proactor_threads` under containerd (issue #4251); bound via pod CPU limits if `num_shards` is similarly ignored.
- `--default_lua_flags` left at default (declared-keys enforced); `allow-undeclared-keys` must **never** be set (it is the in-script isolation guarantee — §5).
- `maxmemory-policy noeviction` (so queue/lock libraries that assert it, e.g. BullMQ, are satisfied); eviction-tolerant cache use is handled at the app layer.

### 2. Per-claim isolation — numbered DB + `$N` ACL user

The provisioner assigns the claim a DB number `N` on its instance (§3) and creates a `$N`-pinned ACL user:

```
ACL SETUSER claim_<ns>_<app>_redis \
  on >\<generated-password\> \
  $<N> \
  resetkeys ~* \
  resetchannels &claim_<ns>_<app>_redis:* \
  +@all -@admin -@dangerous \
  +info \
  +client|setname +client|setinfo +client|getname +client|id \
  +sort_ro
```

- `$<N>` pins the user to DB N — `SELECT` of any other DB is `NOPERM`. This is the hard keyspace boundary.
- `~*` then means "all keys **in DB N**" — the claim owns its whole DB, **with no key prefix on the app side**. `SCAN`/`RANDOMKEY` are confined to DB N, so the cross-tenant key-name enumeration leak that key-prefix isolation suffers does not exist here.
- `&claim_<ns>_<app>_redis:*` — pub/sub channel isolation (channels are not DB-scoped, so they still need an explicit prefix). `resetchannels` first, since the default is no channels.
- `+@all -@admin -@dangerous` — blocks server-wide / admin / data-destroying commands on the shared instance (`FLUSHALL`, `SWAPDB`, `CONFIG`, `KEYS`, `DEBUG`, `REPLICAOF`, `SHUTDOWN`, `MIGRATE`, `RESTORE`, …). `+sort_ro` restores read-only `SORT`. **Corrected 2026-08-31 (§10):** this bullet used to list `CLIENT KILL/LIST/NO-EVICT` among the blocked commands and to say `+info` was re-granted. Neither is accurate. `CLIENT` is registered `SLOW | CONNECTION` (`server_family.cc:4294, :4326`) with no `ADMIN`/`DANGEROUS` bit and no subcommand granularity, so `-@admin -@dangerous` does not touch it and the whole command survives — see the recorded risk in §11. `+info` was dropped in §11. Scripting (`@scripting`) is intentionally retained (§5).
- **Optional self-service knob:** because `$N` confines the user to DB N, it is *safe* to re-grant `+flushdb` — the user's `FLUSHDB` can only ever clear DB N. This lets an app "clear my own cache", which key-prefix isolation can never allow (there `FLUSHDB` would wipe the shared DB 0). Off by default; enable per-tier if wanted.

The provisioner holds the instance admin credential (the `passwordFromSecret` Secret **it creates** when it lazily creates the `Dragonfly` CR; the operator consumes it) and runs `ACL SETUSER` over a Redis client (a new provisioner dependency).

### 3. DB-number allocation — claim → `(instance, dbnum)`

- **Authoritative assignment lives in `ResourceClaim.status`** (`instance`, `dbnum`) — a k8s object, owner-ref'd, GC'd by the existing RetainedClaim machinery, and it survives instance restarts (unlike Dragonfly's runtime ACL state). The provisioner writes it there and into the connection Secret.
- **Allocation** picks the lowest free `dbnum < D` on an instance of the requested class. A per-instance free/used index makes this race-free; it is a cache rebuilt from live `ResourceClaim` statuses on reconcile (single source of truth = the claims).
- **Capacity:** if no instance in the class has a free slot, that is the signal to provision a new pool instance (horizontal growth). Until pool auto-growth is implemented, the claim stays unready with a clear `InsufficientCapacity` condition rather than silently overflowing.
- **Recycling (safety-critical):** DB numbers are reused, so a freed number returns to the pool for a future claim. To prevent a new tenant inheriting a dead tenant's keys if a prior cleanup ever failed, the provisioner **`FLUSHDB`s on allocation** (not only on teardown) and always creates a fresh ACL user. This recycle-safety invariant has no analogue in the prefix model (prefixes are unique and never reused).

### 4. ACL reconciliation — the provisioner owns runtime ACL state

Per-claim users are runtime state (in-memory, not reliably persisted via `--aclfile`, lost on pod reload). A **reconcile loop** keeps them alive:
- It watches the pool instances' pod readiness / `status` generation. On a (re)start transition it enumerates every live `ResourceClaim` bound to that instance and **re-applies** each `ACL SETUSER` (idempotent via `resetkeys`/`resetchannels`), re-pinning the user to the `dbnum` recorded in the claim's status.
- It also re-asserts on a periodic resync.
- This extends `resourceclaim-provisioner` (which already owns the Redis client, the secrets, and the allocation index) rather than adding a separate controller.
- `ACL SETUSER` propagates to live connections, so re-asserting a recovering instance restores access without forcing app reconnects.

The loop is mandatory: it is what makes per-claim users durable across instance restarts.

### 5. Scripting policy — allowed, gated on declared-keys

`needs.redis` **allows** Lua scripting (`EVAL`/`EVALSHA`/`FUNCTION`) because Redis-as-queue/lock is a primary use case (BullMQ, Redlock, rate-limiters). In-script isolation rests on Dragonfly's **default declared-keys enforcement** (a script may only touch keys passed in `KEYS[]`, undeclared access errors out) plus the user's `$N` DB pin, which confines scripts to DB N. The `allow-undeclared-keys` flag (§1) staying unset is therefore a security invariant. (Lua write-scripts fail on read-only replicas; the operator service targets the master, so the primary path is fine, but read-replica fan-out is not script-safe.)

Conservative fallback (if the pre-merge check fails): add `-@scripting` and document `needs.redis` as cache/pub-sub only, routing queue/lock workloads to a per-claim instance tier.

### 6. Per-claim opt-in persistence

`#ServiceNeed` gains `persistent?: bool` (default `false`), threaded into the `ResourceClaim` (4 mirrors: CUE, kube-rs, OpenAPI CRD, webhook). `needs.redis: {}` allocates a DB on an *ephemeral* pool instance; `needs.redis: {persistent: true}` allocates on a *persistent* pool instance.

Persistence in Dragonfly is **instance-scoped**: an RDB snapshot captures the whole instance (all DBs) and a restore replays the whole instance — there is no per-DB snapshot/backup/restore. So per-claim persistence is expressed as **instance placement**, not per-DB config, and a restore reverts *all* DBs on that instance to the snapshot point. A claim needing *isolated* durability (its own RPO, snapshot lifecycle, independent restore) requires a dedicated instance (the per-claim-instance tier).

### 7. Connection contract — DB-pinned URL, channel-only prefix

The per-claim connection Secret (claim namespace, owner-ref'd → cascades on delete) carries:
- `REDIS_URL = redis://claim_<ns>_<app>_redis:\<pass\>@\<instance\>.dragonfly-system.svc:6379/<N>` — the `/<N>` selects the claim's DB; combined with the `$N` ACL the app is hard-pinned to DB N and uses **ordinary key names, no prefix**.
- `REDIS_CHANNEL_PREFIX = claim_<ns>_<app>_redis:` — applies to **pub/sub channel names only** (the `&` ACL enforces it); apps not using pub/sub ignore it. Keys need no prefix.

The renderer's needs→env table (2.4e) generalises to a list of `(env-var, secret-key)` pairs: `pg → [(DATABASE_URL, DATABASE_URL)]`; `redis → [(REDIS_URL, REDIS_URL), (REDIS_CHANNEL_PREFIX, REDIS_CHANNEL_PREFIX)]`. The webhook's reserved-env guard rejects an app literally setting `REDIS_URL`/`REDIS_CHANNEL_PREFIX` when `needs.redis` is present (mirrors the `DATABASE_URL` guard).

Keyspace notifications, previously out of scope, become **viable per-tenant**: because they are DB-scoped (`__keyevent@N__` / `__keyspace@N__`), an instance can enable `notify-keyspace-events` and a tenant on DB N can be granted `&__keyevent@<N>__:* &__keyspace@<N>__:*` safely (those channels carry only DB N's events). Off by default; a per-tier knob.

> **Superseded in part by [ADR 0046](0046-env-value-references.md).** The
> connection Secret's shape described above — a composed `REDIS_URL` plus a
> separate `REDIS_CHANNEL_PREFIX`, auto-injected by the renderer's
> needs→env table — is replaced by decomposed keys (`url`, `host`, `port`,
> `user`, `pass`, `db`, `channelPrefix`) with no auto-injection: an app binds
> whichever field it needs via an explicit `claim.redis.<field>` (or
> `claim.redis.<name>.<field>`) reference in its own `env` map. `acl_reconcile`
> reads the `pass` key directly instead of parsing it out of the `REDIS_URL`
> DSN. The reserved-env guard rejecting a literal `REDIS_URL`/
> `REDIS_CHANNEL_PREFIX` is also gone (see ADR 0046 §5). The DB-pinning and
> channel-prefix isolation mechanics themselves are unchanged — only the
> Secret's key names and how an app gets to them moved.

### 8. GC — per-DB `FLUSHDB`

The whole-instance snapshot format does **not** constrain GC, because GC operates on the live keyspace, not the snapshot file. After the 7-day grace floor, the provisioner connects with the admin credential and: `SELECT <N>; FLUSHDB ASYNC` (drops the claim's entire DB in one command — far simpler than the prefix model's `SCAN MATCH | UNLINK`), `ACL DELUSER claim_<ns>_<app>_redis` (which also closes the user's live connections), returns `N` to the instance's free pool, then deletes the connection Secret. The next scheduled snapshot simply captures the now-empty DB. During the grace window the number stays **reserved** (not reused) so a recreate-within-7-days can reattach to retained data on a persistent instance; ephemeral claims hold no data, so their number may be freed immediately on deletion.

### Scope

In scope: pooled lazy shared instances (per persistence class), per-DB `$N` isolation, channel-prefix pub/sub isolation, claim→`(instance, dbnum)` allocation + recycling, the reconcile loop, scripting (declared-keys-gated), the DB-pinned connection contract, `FLUSHDB` GC. Out of scope (for now): per-DB *resource* isolation / noisy-neighbour controls (accepted within a cluster; see Risks), automatic pool auto-**growth** (adding an instance when a class runs out of slots — manual/triggered initially; pool **shrink** is in scope and specified in §9), HA/replication topology tuning, Redis Cluster sharding, multi-tenant *adversarial* isolation, and a `needs.redis` size/eviction surface beyond the MVP.

## Consequences

- **Easier:** apps use ordinary key names (no prefix); `SCAN` is DB-confined so the key-name leak is gone; GC is a single `FLUSHDB`; keyspace notifications become usable; `needs.redis` still reuses the entire 2.4 claim → schedule → provision → inject → GC pipeline. The dragonfly-operator handles instance lifecycle, auth, replication, and snapshots, and Dragonfly already ships `$N` — no engine swap needed.
- **Operationally visible (§9, 2026-08-21):** a shared instance with no tenants
  is now **reaped**, returning its Guaranteed reservation (256Mi CNPG / 320Mi
  Dragonfly) — but reaping a CNPG cluster **leaves its PVC behind**, because the
  reaper strips the volume's `ownerReference` before deleting the `Cluster` and
  does so unconditionally (§9.4). Expect `kubectl get pvc` to show volumes with
  no owning cluster; that is the design, not a leak, and it does not accumulate
  — the next provision under the same instance name adopts the same volume
  (§9.5). A reaped persistent Dragonfly instance also comes back with its
  snapshot contents intact (§9.6), which `FLUSHDB`-on-allocation (§3) is what
  makes safe.
- **Harder / neutral:** a **finite, recycled DB-number namespace** to allocate (claim→`(instance, dbnum)` state, an allocation index, capacity handling, and a **recycle-safety `FLUSHDB`-on-allocation** invariant). `--dbnum` is very likely **restart-only**, so the default must be set generously up front (and measured — see below) and capacity grown by adding pool instances. Pub/sub still needs a channel prefix (keys don't). The provisioner performs imperative, continuously-reconciled Redis I/O. Scripting is allowed but pinned to one instance flag staying unset.

## Alternatives considered

- **Key-prefix ACL on a shared DB 0** (`~claim_…:* &claim_…:*`, no `$N`). The earlier shape. Works, but: every key carries a prefix (app-side friction and RAM), `SCAN`/`RANDOMKEY` leak other claims' key *names* (ACL is checked on access, not enumeration — and `SCAN` cannot be scoped to a prefix), and GC needs a `SCAN MATCH | UNLINK` sweep. The `$N` model removes all three. **Retained only as a fallback** if a single instance's DB-number ceiling is exhausted at extreme density before pool growth is available.
- **Dragonfly Namespaces (experimental).** `NAMESPACE:` flag — complete separation with an unbounded namespace count and the same zero-prefix/scoped-SCAN benefits as `$N`, without the DB-count ceiling. Rejected for now: **experimental, no replication or defrag**, incompatible with the higher-tier HA story; revisit if it gains replication.
- **Valkey 9.1 (numbered DB + per-database ACL).** The *same* mechanism on a different engine — Valkey 9.1 added DB-scoped ACLs and numbered DBs as lightweight namespaces (with near-zero overhead for unused DBs, a nicer ceiling story than Dragonfly), under a permissive BSD licence. Rejected the **engine swap**: Dragonfly already provides `$N`, and the dragonfly-operator (GA: snapshots to PVC/S3, replication, auth/ACL) is markedly more mature than the Valkey operator landscape (the official `valkey-io/valkey-operator` is WIP/not-production). Kept as a **re-evaluation trigger** (if the dragonfly-operator/namespaces disappoint, if the BSL licence becomes a problem, or for a leaner per-tenant-instance tier).
- **Per-claim Dragonfly instance.** Hard, even adversarial isolation; isolated persistence/restore; no noisy neighbour; no DB-number ceiling. Rejected for low tiers (cost / contradicts lazy-shared). **Reserved as the higher-tier / Regulated upgrade** and the escape hatch for isolated durability and adversarial tenants.
- **Single global `requirepass`, DB-number isolation without ACL.** The `plan.md` sketch — every app shares one credential and can `SELECT` any DB. No isolation. Rejected.
- **Bare Dragonfly `Deployment` (no operator)** and **always-on shared instance in platform-stack.** Rejected — lose HA/auth/snapshot management and the lazy/Solo-cost behaviour respectively.
- **Declarative per-claim users via `aclFromSecret`.** Would avoid the reconcile loop, but Dragonfly's ACL-file support for key/channel patterns is ambiguous; the reconcile loop is robust regardless. Revisit if file-based ACLs are officially confirmed.

## Risks

- **Per-DB overhead — measured (2026-06-05).** Idle `dbnum` cost is ~0 (lazy alloc); each active DB ≈ ~280 kB × `num_shards`; hard ceiling 1024 DBs/instance. Density is known: a low-shard shared instance holds its full 1024-claim cap for a few hundred MB of structural overhead (Pre-merge #1). Residual: the floor scales with `num_shards`, so `num_shards` MUST be pinned low (§1) — and the pin verified to take effect in-cluster (issue #4251).
- **`--dbnum` is restart-only (confirmed).** Immutable at runtime (Pre-merge #2), so we set it to the 1024 ceiling once and grow the pool horizontally; we never need a `dbnum` change (1024 is the max), avoiding the rolling restart it would require (ephemeral data wipe / persistent downtime + RPO gap).
- **DB-number exhaustion / pool management.** Finite, recycled namespace per instance. Mitigation: generous (measured) `dbnum`, the allocation index + `InsufficientCapacity` signal, and pool growth as the relief valve.
- **Recycle leak.** Reusing a number after a failed cleanup could expose a dead tenant's keys. Mitigation: `FLUSHDB`-on-allocation + fresh ACL user (recycle-safety invariant).
- **Cross-DB command escapes.** Verify that under `$N`, commands with a DB-index argument (`MOVE`, `COPY … DB`, `SWAPDB`) are denied (`SWAPDB` is already blocked via `-@dangerous`); explicitly `-move`/restrict `copy` if not (Pre-merge #3).
- **Pub/sub channel friction.** Channels are not DB-scoped, so an app using pub/sub must apply `REDIS_CHANNEL_PREFIX` or get `NOPERM`. Mitigation: inject + document; keyspace notifications are now DB-scoped (an improvement).
- **Imperative ACL fragility + restart durability.** Mitigation: idempotent `ACL SETUSER`, the reconcile loop re-pinning users from claim status on (re)start and resync, requeue on error, the 2.4d gate holding the app until the user first exists. `DELUSER` closes the user's connections.
- **Scripting on a shared instance.** Mitigation: declared-keys default + `$N` confinement, `allow-undeclared-keys` unset, no admin, per-claim-instance upgrade for stricter tiers, Pre-merge #4.
- **Snapshot data-loss / ephemeral total loss.** Persistent instances lose writes since the last snapshot; ephemeral instances lose *all* DBs on any restart. Persistence/backup is whole-instance. Mitigation: documented; sensible snapshot interval; zero-loss durability → Postgres.
- **Noisy neighbour (no resource isolation).** Numbered DBs isolate logically, not by resources — one claim can saturate CPU/memory/IO for all DBs on its instance. Accepted within a cluster for now; horizontal pool growth and the per-claim-instance tier are the levers, and per-DB resource controls can be added later.
- **`INFO` exposure.** ~~Re-granting `+info` lets a claim read aggregate server stats on the shared instance. Accepted (trusted single-tenant).~~ **Superseded by §11 (2026-08-31).** The "trusted single-tenant" premise stopped describing the deployment the moment `POOL_INSTANCE_INDEX` was hard-coded to `0`: one ephemeral and one persistent instance serve every tenant in the cluster, across unrelated namespaces. `+info` is dropped and `-pubsub` added.
- **ACL-user / DB growth.** Many claims → many users + DBs per instance. Accepted at Tier-1 scale; pool growth when an instance's footprint hits its measured limit.

## Pre-merge verification

1. ✅ **RESOLVED 2026-06-05 — Per-DB overhead → `dbnum = 1024`, pin `num_shards` low.** Measured on Dragonfly v1.38.1 (podman, process VmRSS): idle RSS is **flat** across `dbnum` 16→1024 (~21 MB @1 shard, ~25 MB @4 shards) — per-DB structures are lazy (`DbSlice` ctor materialises only db0; `ActivateDb→CreateDb` on first write). Each **active** DB costs a fixed **~280 kB × num_shards** (1 shard → ~280 kB, 4 shards → ~1114 kB; independent of key count — the `kInitSegmentLog=3` → 8-segment DashTable floor, ~32 KiB mimalloc good-size class per segment). Hard ceiling 1024 DBs/instance. Source-corroborated against v1.38.1 (`common_types.h:24 kMaxDbId=1024`, `db_slice.cc`, `table.cc kInitSegmentLog`).
2. ✅ **RESOLVED 2026-06-05 — `dbnum` is immutable (restart-only).** `CONFIG SET dbnum 64` → `ERR … can't set immutable config`. Source: registered non-mutable (`main_service.cc:909`; `config_registry` `is_mutable=false` → READONLY). No online vertical growth — horizontal pool growth is the only capacity lever, as §1/§3 assume.
3. **Cross-DB command containment under `$N`.** Confirm `MOVE` / `COPY … DB` / `SWAPDB` are denied for a `$N`-pinned user; add explicit `-move` / `copy` restrictions if not.
4. **In-script ACL + DB confinement.** Confirm `EVAL` cannot touch keys outside DB N (declared-keys + `$N`); `ACL DRYRUN` + a live `EVAL`. If not enforced, take the §5 fallback.
5. **Client-library init under the ACL.** ioredis / node-redis / BullMQ connect, health, and `maxmemory-policy` checks pass; if a client probes `CONFIG GET maxmemory-policy`, decide `+config|get` vs disabling the check — do not widen to `+@dangerous`. **Amended 2026-08-31 (§11):** with `+info` dropped, BullMQ's version probe is unconditional and its `init()` rejects, so a BullMQ tenant needs `skipVersionCheck: true` — one line, tenant-side. ioredis ≥ 5 has a purpose-built `NOPERM` branch and degrades to a warning; node-redis, redis-py, go-redis, lettuce and jedis never issue `INFO` on connect.
6. **Restart durability + recycle-safety.** Kill a pool pod; confirm the reconcile loop re-pins all live claims to the correct `dbnum`s. Allocate a recycled number; confirm `FLUSHDB`-on-allocation leaves no inherited keys.

## §9 — Shrinking the pool (2026-08-21, extends Decision)

Added after acceptance; a continuation of §§1–8, which live as subsections of
Decision above. §1 states that "instances are created lazily; a solo cluster
with no Redis apps runs no Dragonfly pod." The lazy-create half is real. The
other half — giving an instance back when its last tenant leaves — was never
built. This section specifies it.

The gap spans **both** shared backends the `resourceclaim-provisioner` creates
lazily, so §9 governs both: the shared CNPG cluster (`plan.md` 2.4, applied by
`provision_cloudnativepg`) and the Dragonfly pool instances
(`provision_dragonfly`). Tenant-level reclamation exists for each —
`remove_managed_role` / `remove_database` for Postgres, `FLUSHDB` +
`ACL DELUSER` for Redis (`gc_drop_dragonfly`) — but across the whole crate
`delete` is called only on PVCs, Secrets and `RetainedClaim`s, never on a
`Cluster` or a `Dragonfly`. A tenantless instance therefore keeps its full
Guaranteed reservation: **256Mi** for the CNPG cluster, **320Mi** for a
Dragonfly pool instance ([ADR 0053](0053-resource-governance.md), 2.16d). On
the ~4 GB Tier-1 node those are not rounding errors — an idle Dragonfly
holding 320Mi was a material part of the node-saturation incident of
2026-08-21, which is what prompted this section.

Symbols named in this section live in
`operator/operator-controllers/resourceclaim-provisioner/src/` (`reconcile.rs`,
`gc.rs`, `dragonfly.rs`, `grace.rs`). They are cited by name rather than by
line, because the reaper this section specifies is implemented in those same
functions.

### 9.1 The liveness predicate — three vetoes

A shared instance is reapable only when nothing in the cluster can be pointing
at it. Three vetoes, each sufficient on its own to keep the instance:

- **ALLOCATED** — a live `ResourceClaim` names the instance in
  `status.instance`. A tenant exists and is connected.
- **INTENT** — an unallocated claim of the matching type and persistence class
  exists. It has not named an instance yet, but it is on its way to this one.
- **RETAINED** — a `RetainedClaim` snapshot names the instance (with one
  exception, §9.6).

**INTENT is not a precaution; it closes a real window in the provision path.**
`provision_dragonfly` SSA-applies the `Dragonfly` CR as its first step and does
not write `status.instance`/`status.dbnum` until it calls `patch_allocation`,
several steps later. Between those two points the instance exists and *no
object in the cluster refers to it*. A predicate built on ALLOCATED and
RETAINED alone, evaluated inside that window, reaps the instance the
provisioner is in the middle of populating. `provision_cloudnativepg` has the
same shape around its `Cluster` apply. The window is short, the reaper's poll
is not synchronised with it, and a race that is merely unlikely is still a
race.

### 9.2 Teardown is not symmetric — measured, 2026-08-21

Measured on kind + podman against **the versions this platform pins**: CNPG
chart `cloudnative-pg-0.28.2` (`app_version 1.29.1`, per
`platform-stack/cue/component_cloudnative-pg.cue`) and `dragonfly-operator
v1.5.0`. The versions are recorded inline because §9.3's safety argument is a
claim about a specific operator's reconcile behaviour: were a future chart to
start re-adding the ownerReference, the strip would quietly stop protecting
anything. **Re-measure on a CNPG chart bump** — and that instruction is
enforced rather than left as prose by the pg e2e walk
(`e2e/needs-pg-walk.sh`) **extended in this same change**, which asserts the
PVC is still `Bound` after a reap and that the same PV is re-adopted, so a CNPG
version that starts re-adding the ownerReference makes the reap cascade and
**fails the walk**. These are observations, not expectations.

**CNPG owns the volume, so a plain delete takes it — measured, not inferred.**
On chart 0.28.2 the shared cluster's PVC is created carrying a controller
ownerReference:

```
[{"apiVersion":"postgresql.cnpg.io/v1","controller":true,"kind":"Cluster",
  "name":"platform-postgres","uid":"dcd48d90-5e54-4afd-850f-4d0857bd06a3"}]
```

and the `Cluster` carries **no finalizers** (`finalizers=` empty). With that
reference left **intact** — a live cluster on PV
`pvc-03e87227-ea32-4eb2-ba1f-2de5813b56a4`, marker row `42` written — deleting
the `Cluster` destroyed the database:

```
PVCs in cnpg-system:            No resources found in cnpg-system namespace.
PVs still bound to that claim:  0
```

The reasoning belongs beside the observation, because it says what would have
to change for the result to change: a `controller: true` ownerReference with no
finalizer to intervene makes this the apiserver garbage collector's doing, not
CNPG's — there is no CNPG-side deletion hook to negotiate with and nothing to
switch off. **The strip in §9.3 is therefore a requirement, not a precaution.**

This counterfactual is recorded here because it is the one observation the e2e
walk can never make — running it destroys a database, so no test may perform
it, and this ADR is its only record.

**Dragonfly does not.** A persistent pool instance's snapshot PVC has **empty**
`ownerReferences`, and the StatefulSet the operator creates ships
`persistentVolumeClaimRetentionPolicy: {"whenDeleted":"Retain","whenScaled":"Retain"}`.
After deleting the `Dragonfly`, the PVC remains `Bound`. An **ephemeral**
instance has no `volumeClaimTemplates` at all — StatefulSet, Service and pod
all cascade and nothing is left behind. So the Dragonfly arm of the reaper is
just the delete; the asymmetry to engineer around is CNPG's alone.

**`--cascade=orphan` was tested and rejected.** It is the obvious way to keep a
volume, and it does not work here. Control experiment: with orphan propagation
the Postgres pod was still `Running` **29 seconds** after the `Cluster` was
gone. Orphan does not select what it orphans — it detaches the entire owned
subtree, pod included — so it preserves the volume by preserving everything and
reclaims **no memory at all**, which is the only thing the reaper exists to do.

### 9.3 The reaper strips the CNPG PVC's ownerReference before deleting

Immediately before deleting a CNPG `Cluster`, the reaper removes the
controller `ownerReference` from each PVC that `Cluster` owns. This converts an
unrecoverable cascade into a preserved volume. Measured on chart 0.28.2:
`after delete: PVC SURVIVED`, with `pods remaining: none` — so the workload is
gone and the memory genuinely returned, while the volume stays.

**The strip holds.** After stripping, the reference was observed empty at
t = 30 s, 60 s and 90 s; a genuine reconcile was then forced with a real spec
change (`shared_buffers` 32MB → 48MB) and the reference was still empty 45 s
past it. CNPG **never re-added it** to a running cluster. The forced reconcile
matters: without it the observation would only show that CNPG had not happened
to reconcile, not that reconciling leaves the strip in place.

Ordering is strip-then-delete, and it fails safe in that direction. If the
reaper dies between the two steps, what remains is a live, healthy cluster
whose PVC has no owner — a volume that would survive a later delete instead of
being cleaned up. The opposite ordering has no safe failure at all.

The strip is re-applied on every reap, because CNPG re-adds the reference when
it adopts the volume back (§9.5). The stripped state is transient by design.

### 9.4 Volume preservation is unconditional

Preservation is **not** conditional on the reap looking correct. The tempting
refinement — preserve the volume when the predicate ran against a complete
picture, let the cascade run otherwise — cannot be built, and the reason is
worth stating precisely rather than asserting.

The failure the refinement would guard against is a predicate computed off a
**truncated LIST**: a list call that returns fewer objects than exist. Such a
LIST does not truncate selectively. It omits live `ResourceClaim`s and
`RetainedClaim`s with the same silence, and hands back no signal that either
set was short. So the exact condition under which the reap is wrong is the
condition under which a "does this reap look clean?" test also reports clean.
The test would pass precisely in the case it exists to catch. **A safety net
that fails in the same instant as the thing it is meant to catch is not a
safety net** — it is a second copy of the assumption that already failed.

Preservation is therefore paid on every reap, sound or not. The price is a
`Bound` PVC; the alternative price is a tenant's data. §9.5 shows the price is
smaller than it first looks.

### 9.5 The leak is continuity, not waste

Both operators re-adopt a preserved volume by name, with the data intact.
Re-provisioning under the same CR name after a reap, measured:

- **CNPG** (chart 0.28.2) — the re-provisioned cluster **adopted the same PV**,
  `pvc-c8c53a3f-7441-425c-929e-da342401cbab`; `SELECT id FROM reap_marker`
  returned **`42`**, the row written before the reap; the cluster reached
  `READY 1` with `Cluster in healthy state`; no PVC-conflict events; and CNPG
  **re-added its ownerReference** on adoption.
- **Dragonfly, persistent** — the same PV before and after
  (`pvc-c8d14acb-d8bb-4152-b635-597345dd91b1`); the pod came up `Ready`.

Because the PVC name is derived deterministically from the CR name, and the
reaper's counterpart recreates the CR under that same name (§1's pool naming),
the preserved volume is exactly what the next provision picks up. The "leak" a
preserved PVC represents is therefore **continuity, not accumulation**: a
reap/re-provision cycle reuses one volume rather than stranding one and
allocating a second. What is left behind is bounded by the number of pool
instance *names*, not by the number of reaps.

### 9.6 Remanence — a reaped instance does not come back empty

Preserving the volume preserves what is on it, and for a persistent Dragonfly
instance that is a snapshot RDB which is **not synchronised with GC**. §8
reclaims a tenant with `SELECT <N>; FLUSHDB ASYNC` — in memory — while the
snapshot is written on the instance's cron. A snapshot taken before the flush
and never superseded (the instance is reaped before the next cron tick)
outlives the tenant whose keys it holds. A future cohort adopting that volume
starts an instance that loads those bytes back into those DB numbers.

This is not a new exposure. It is §3's exposure with a longer arm. §3 already
requires `FLUSHDB` **on allocation**, not only on teardown, precisely because a
prior cleanup may have failed — so every DB number a new tenant is handed is
cleared before that tenant can read it, whether the stale bytes arrived from a
failed GC or from a resurrected snapshot. The recycle-safety invariant is keyed
on allocation, not on instance continuity, so it survives the instance being
deleted and recreated underneath the pool unchanged. What §3 did not consider
is that the instance could be recreated at all; that is the only thing §9 adds
here.

The residue is confined to DB numbers no new tenant takes: those retain stale
bytes on disk until something allocates them, and allocation is the thing that
clears them. Recorded plainly so no future reader assumes a reaped instance
returns empty — it does not. (Ephemeral instances have no volume at all, §9.2,
so they have no remanence.)

### 9.7 `RetainedClaim` vetoes two of the three, and reclaim latency splits

The RETAINED veto applies to the shared CNPG cluster and to persistent
Dragonfly instances, and **not** to ephemeral Dragonfly instances. §8 already
draws that line in principle: "ephemeral claims hold no data, so their number
may be freed immediately on deletion." A snapshot naming an ephemeral instance
has nothing to reattach to, so it has nothing to protect.

> **That §8 sentence describes intent, not shipped behaviour, and the
> difference matters here.** Nothing in the code frees an ephemeral claim's DB
> number early: `GRACE_PERIOD` (`grace.rs`) is an unconditional
> `7 * 24 * 60 * 60` with no persistence branch; `snapshot_retained_claim`
> writes a `RetainedClaim` for every Dragonfly claim regardless of class,
> deliberately, "so the 7-day `RetainedClaim` lifecycle is uniform"; and
> `used_dbnums` reserves *every* `RetainedClaim` matching the instance, with no
> class filter. So the veto asymmetry above is about the **instance**, not the
> **slot**: an ephemeral instance is reaped roughly one dwell after its last
> claim goes, while that claim's DB number stays reserved for the full seven
> days either way. §9 neither changes §8's slot behaviour nor depends on §8's
> "immediately" ever becoming true — it only declines to keep a *pod* alive for
> a snapshot that has no data in it.

The consequence is a **split in reclaim latency**: an ephemeral pool instance
comes back roughly one reaper dwell after its last claim goes away, while the
CNPG shared cluster and persistent instances are held for at least the 7-day
grace floor of the last snapshot naming them. That is the correct trade — the
grace window's whole promise is that a recreate within it reattaches to
retained data — but it means the memory win on a persistent class is a slow one
and should not be expected to show up during a walk.

**Slot reservation is unaffected either way.** `used_dbnums` (`dragonfly.rs`)
derives the reserved set from live claims ∪ `RetainedClaim`s and **never** from
the running instance. A `RetainedClaim`'s `(instance, dbnum)` reservation
therefore survives the instance vanishing entirely: the number stays
unavailable for the whole grace window whether or not a pod exists to hold it,
and a recreated instance starts with exactly those slots already spoken for.
The reaper cannot cause a recycled-number collision by removing the instance,
because the instance was never the source of truth for which numbers are taken.

**Follow-up (deferred, not rejected).** §9 is backend-agnostic in substance —
the predicate, the strip and the preservation rule govern the shared CNPG
cluster as much as the Dragonfly pool — so it arguably belongs in its own ADR
rather than inside a Redis-titled one. Deferred deliberately: promoting it
before the reaper exists would churn the record for no gain. Revisit once the
implementation lands.

## §10 — File-backed ACL durability (2026-08-31, extends Decision §4)

### The revisit condition is met

§4 and the Alternatives list rejected file-based ACLs twice, the second time
with a named condition: *"Dragonfly's ACL-file support for key/channel patterns
is ambiguous; the reconcile loop is robust regardless. Revisit if file-based
ACLs are officially confirmed."*

At the pinned server there is no ambiguity. `ParseAclSetUser` has **one
definition** (`acl_family.cc:1056`) and exactly **two call sites**: runtime
`ACL SETUSER` (`AclFamily::SetUser`, `:142`) and the file loader
(`LoadToRegistryFromFile`, `:318`). Both file entry points — startup `Load()`
and the runtime `ACL LOAD` command — route through it. Every token this
platform emits (`$N`, `~*`, `&user:*`, `resetkeys`, `resetchannels`, the
category grants) is parsed by the same code on both paths, and upstream's own
suite round-trips key and channel patterns through `ACL SAVE` / `ACL LOAD`.

So §4's "not reliably persisted via `--aclfile`" is **retracted**.

§4's reconcile loop is **not** retracted. It becomes the file's sole writer and
remains the backstop for a claim created inside one tick.

Also retracted honestly: §4 described a loop that "watches the pool instances'
pod readiness / `status` generation" and re-applies on a restart transition.
Only the periodic half was ever built. This amendment does not add the watch —
it adds durability, which makes the watch unnecessary rather than overdue.

### The `default` line is a security gate, and its failure is the opposite of what was assumed

The working note that preceded this amendment said a wrong `default` line risks
locking the provisioner out of its own instance. **Omitting it does the
opposite, and the opposite is far worse.**

`AclFamily::Init` early-returns (`acl_family.cc:626-627`) whenever `--aclfile`
loads, so `UserRegistry::Init` — the only consumer of the `requirepass` value
the dragonfly-operator injects from `passwordFromSecret` — never runs.
`LoadToRegistryFromFile` then synthesises `default` with **`nopass = true`**,
`+@all ~* &*`. Verified live on a kind cluster with the real operator, a real
`authentication.passwordFromSecret`, and a tenant-only ACL Secret:
unauthenticated `PING` returned `PONG`, `ACL WHOAMI` returned `default`, and an
unauthenticated `SET` succeeded.

**A file that parses successfully but omits `default` silently disables
authentication on an instance serving every tenant in the cluster.**

The lockout is real but has a different cause: `User::is_active_` defaults
false, and the synthesised default's `is_active = true` applies only when the
file omits `default` — so a `default` line missing the `on` token yields an
inactive default nobody can authenticate as.

Both failures are closed by the same rule: **the builder refuses to emit a file
without `USER default on >…`**, and the walk asserts the boundary in both
directions — unauthenticated access denied, a wrong password rejected, the
admin password accepted.

### Grammar, corrected

`MaterializeFileContents` splits lines on `\n` and tokens with
`absl::StrSplit(command, ' ', absl::SkipEmpty())`. **`SkipEmpty` tolerates
double spaces** — upstream tests this deliberately. The working note claimed
single-space joining was a correctness constraint; it is not. It survives only
as a byte-stability requirement, so that a re-derived file compares equal to
the live one and the loop can skip the write.

What is fatal: a tab or `\r` (glued into a token rather than splitting it), any
first token that is not `USER` (there is no comment syntax — a `#` header
rejects the whole file), fewer than four tokens on a line, and a whitespace-only
line.

Failure is all-or-nothing and, importantly, **safe**: one bad line yields zero
users loaded, a `LOG(WARNING)`, and the server starts anyway — falling back to
the `requirepass`-derived default. **A malformed file degrades to exactly
today's post-restart behaviour, no worse.** That is the strongest argument that
this change is safe to make.

### `ACL LOAD` is prohibited

The runtime path evicts every connection whose authenticated user is not
`default` and then **clears the registry** (`acl_family.cc:326-334`), destroying
every user the incoming file does not name. It does not diff against the new
file, so even a file perfectly in sync with memory drops every tenant
connection. Combined with the kubelet's projected-Secret refresh lag, an
`ACL LOAD` fired straight after a Secret write can load the *previous* file and
delete users the provisioner has just created.

It is never added to the operator's Redis seam. The startup path is safe by
contrast — it passes no reply builder, so it neither clears nor evicts.

### Two further prohibitions

- **Never `CONFIG SET requirepass` on an `--aclfile` instance.** Several
  `UserRegistry` tables are assigned only in the skipped `Init`, and the
  `requirepass` config hook is the one path that dereferences them.
- **Rotation ordering (for 2.16a).** If credential rotation ever reaches the
  admin password, the ACL file must be rewritten **before** the
  `<instance>-admin` Secret is rotated. A restart in the reverse window locks
  out everyone, including the operator.

### Break-glass

Two in-band recoveries from a bad file, neither losing data:

1. Correct the `<instance>-acl` Secret and delete the pod — startup re-loads
   the fixed file.
2. Detach the field entirely:
   `kubectl -n dragonfly-system patch dragonfly <inst> --type=json -p '[{"op":"remove","path":"/spec/aclFromSecret"}]'`.
   The operator rolls the StatefulSet without `--aclfile`, and the default user
   is reseeded from the injected admin password.

Note that the pod healthcheck cannot detect a broken ACL configuration: it
probes the admin port, which is exempt from authentication.

### The server image is pinned here

The platform pinned no Dragonfly server image: the CR carried no `image`, the
chart's default was empty, so the tag came from the operator's compiled-in
default. Every ACL fact above is a fact about **v1.37.0**, and an operator-chart
bump would have changed it silently. The provisioner now sets `spec.image`
explicitly.

## §11 — `+info` and `PUBSUB` are revoked (2026-08-31)

`INFO KEYSPACE` on this server emits a line for every **non-empty** database
regardless of which one the connection selected — a live enumeration of which
tenants currently hold data, with their key counts, expiries and hit ratios.
The ACL grammar cannot scope `INFO` to a section or a database: a
`+info|keyspace` token fails to resolve, and enforcement is whole-command. It
is `+info` on or off.

`PUBSUB` is worse and was not in the original finding. It is registered `SLOW`
only, so it survives `-@admin -@dangerous`, and it takes the generic ACL path —
the `&{user}:*` channel patterns constrain what a tenant may subscribe to, not
what `PUBSUB CHANNELS` *returns*. Channel names are
`claim_<namespace>_<app>_redis:`, so `PUBSUB CHANNELS '*'` returns **the
Kubernetes namespace and application name of every pub/sub-using tenant in
plaintext**. Shipping the `INFO` fix alone would have let us claim tenants
cannot see each other while they still could, through a channel that names them.

**Both are dropped.** Pub/sub itself is untouched — `PUBSUB` is a distinct
command from `SUBSCRIBE`/`PUBLISH` — and the safe `CLIENT` subcommands stay
granted, so client handshakes are unaffected.

Waiting for upstream was not a live option: the `INFO KEYSPACE` loop is
byte-identical on `main` three minor releases past our pin, with no issue and no
PR. Doing it now costs two tokens in a rule vector this subphase is already
rewriting; doing it later costs a forced re-pin of every user **and** a rewrite
of a persisted file.

### Recorded risk, NOT fixed here

`CLIENT` is granted, so `CLIENT LIST` returns peer addresses, DB indices and
per-connection command counts for every tenant, and `CLIENT KILL` / `CLIENT
PAUSE` let any tenant sever another tenant's connections or stall the shared
instance. That is a **cross-tenant denial of service and a larger disclosure
than the one this section closes.** It is not fixed here because `-client`
cannot be scoped to subcommands at this version and would break the
`CLIENT SETNAME`/`SETINFO` handshake every client performs. Recorded rather than
inherited; the real remedy is per-tenant instances (already the higher-tier
answer) or upstream subcommand granularity.

## Owner

Platform / operator team.

## Re-evaluation

Revisit if: per-DB overhead (Pre-merge #1) makes a generous `dbnum` too costly (→ smaller `dbnum`, earlier pool growth, or reconsider per-claim instances); `dbnum` proves runtime-mutable (→ optional online vertical growth); Dragonfly Namespaces gain replication (→ drop the DB-count ceiling); Valkey's operator matures or the BSL licence becomes a problem (→ reconsider the engine); adversarial isolation or isolated durability is required (→ per-claim instances); or noisy-neighbour contention forces per-DB resource controls.

## References

- `plan.md` Phase 2.6; ADR 0026 (PlatformStack); the 2.4 needs.pg decomposition (`plan.md` 2.4a–g) and `resourceclaim-provisioner` (the generic backend-dispatch this extends).
- `operator/operator-controllers/resourceclaim-provisioner/`, `operator/operator-rendering/src/lib.rs` (needs→env table), `schemas/v1alpha1/application.cue` (`#ServiceNeed`).
- Dragonfly ACL (`ACL SETUSER`; `$N` DB selector → `NOPERM` on other DBs; command/key/channel rules; `SETUSER` propagates to live connections, `DELUSER` closes them); Dragonfly `--dbnum` (max DBs for `SELECT`, default 16); `CONFIG SET` (runtime reconfig, but not all flags are mutable); Dragonfly `SCAN` (iterates the selected DB), `FLUSHDB` (clears the selected DB only); Lua (declared-keys default, `allow-undeclared-keys`, no scripts on read-only replicas); Namespaces (experimental, no replication).
- Redis/Valkey persistence (RDB/AOF are whole-dataset; no per-DB snapshot/backup); Redis 7+ restrictive pub/sub default; Valkey 9.0/9.1 (numbered DBs as lightweight namespaces, per-database ACL, near-zero overhead for unused DBs) as the engine-swap alternative.
- `dragonflydb/dragonfly-operator` (`Dragonfly` CR — instances, `passwordFromSecret`/`aclFromSecret`, replication, snapshots to PVC/S3; service targets the master).
- **Shared-instance reaping — §9 covers the shared CNPG cluster as well as the Dragonfly pool** (instance liveness predicate, CNPG PVC `ownerReference` stripping, unconditional volume preservation, snapshot remanence). Sizing of the reserved memory it reclaims: [ADR 0053](0053-resource-governance.md) (Guaranteed stateful backends) and `platform-stack/cue/service_providers.cue`; CNPG chart pin: `platform-stack/cue/component_cloudnative-pg.cue`.
