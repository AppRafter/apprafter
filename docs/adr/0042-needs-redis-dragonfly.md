# ADR 0042: needs.redis → Dragonfly — per-database isolation (`$N` ACL) on a pool of lazy shared instances

## Status

Accepted (2026-06-05).

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
  resetchannels &claim_<ns>_<app>:* \
  +@all -@admin -@dangerous \
  +info \
  +client|setname +client|setinfo +client|getname +client|id \
  +sort_ro
```

- `$<N>` pins the user to DB N — `SELECT` of any other DB is `NOPERM`. This is the hard keyspace boundary.
- `~*` then means "all keys **in DB N**" — the claim owns its whole DB, **with no key prefix on the app side**. `SCAN`/`RANDOMKEY` are confined to DB N, so the cross-tenant key-name enumeration leak that key-prefix isolation suffers does not exist here.
- `&claim_<ns>_<app>:*` — pub/sub channel isolation (channels are not DB-scoped, so they still need an explicit prefix). `resetchannels` first, since the default is no channels.
- `+@all -@admin -@dangerous` — blocks server-wide / admin / data-destroying commands on the shared instance (`FLUSHALL`, `SWAPDB`, `CONFIG`, `KEYS`, `DEBUG`, `REPLICAOF`, `SHUTDOWN`, `MIGRATE`, `RESTORE`, `CLIENT KILL/LIST/NO-EVICT`, …). `+info` and the safe `CLIENT` subcommands are re-granted (otherwise `-@dangerous` strips them and breaks client init / pool health checks); `+sort_ro` restores read-only `SORT`. Scripting (`@scripting`) is intentionally retained (§5).
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
- `REDIS_CHANNEL_PREFIX = claim_<ns>_<app>:` — applies to **pub/sub channel names only** (the `&` ACL enforces it); apps not using pub/sub ignore it. Keys need no prefix.

The renderer's needs→env table (2.4e) generalises to a list of `(env-var, secret-key)` pairs: `pg → [(DATABASE_URL, DATABASE_URL)]`; `redis → [(REDIS_URL, REDIS_URL), (REDIS_CHANNEL_PREFIX, REDIS_CHANNEL_PREFIX)]`. The webhook's reserved-env guard rejects an app literally setting `REDIS_URL`/`REDIS_CHANNEL_PREFIX` when `needs.redis` is present (mirrors the `DATABASE_URL` guard).

Keyspace notifications, previously out of scope, become **viable per-tenant**: because they are DB-scoped (`__keyevent@N__` / `__keyspace@N__`), an instance can enable `notify-keyspace-events` and a tenant on DB N can be granted `&__keyevent@<N>__:* &__keyspace@<N>__:*` safely (those channels carry only DB N's events). Off by default; a per-tier knob.

### 8. GC — per-DB `FLUSHDB`

The whole-instance snapshot format does **not** constrain GC, because GC operates on the live keyspace, not the snapshot file. After the 7-day grace floor, the provisioner connects with the admin credential and: `SELECT <N>; FLUSHDB ASYNC` (drops the claim's entire DB in one command — far simpler than the prefix model's `SCAN MATCH | UNLINK`), `ACL DELUSER claim_<ns>_<app>_redis` (which also closes the user's live connections), returns `N` to the instance's free pool, then deletes the connection Secret. The next scheduled snapshot simply captures the now-empty DB. During the grace window the number stays **reserved** (not reused) so a recreate-within-7-days can reattach to retained data on a persistent instance; ephemeral claims hold no data, so their number may be freed immediately on deletion.

### Scope

In scope: pooled lazy shared instances (per persistence class), per-DB `$N` isolation, channel-prefix pub/sub isolation, claim→`(instance, dbnum)` allocation + recycling, the reconcile loop, scripting (declared-keys-gated), the DB-pinned connection contract, `FLUSHDB` GC. Out of scope (for now): per-DB *resource* isolation / noisy-neighbour controls (accepted within a cluster; see Risks), automatic pool auto-scaling (manual/triggered initially), HA/replication topology tuning, Redis Cluster sharding, multi-tenant *adversarial* isolation, and a `needs.redis` size/eviction surface beyond the MVP.

## Consequences

- **Easier:** apps use ordinary key names (no prefix); `SCAN` is DB-confined so the key-name leak is gone; GC is a single `FLUSHDB`; keyspace notifications become usable; `needs.redis` still reuses the entire 2.4 claim → schedule → provision → inject → GC pipeline. The dragonfly-operator handles instance lifecycle, auth, replication, and snapshots, and Dragonfly already ships `$N` — no engine swap needed.
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
- **`INFO` exposure.** Re-granting `+info` lets a claim read aggregate server stats on the shared instance. Accepted (trusted single-tenant).
- **ACL-user / DB growth.** Many claims → many users + DBs per instance. Accepted at Tier-1 scale; pool growth when an instance's footprint hits its measured limit.

## Pre-merge verification

1. ✅ **RESOLVED 2026-06-05 — Per-DB overhead → `dbnum = 1024`, pin `num_shards` low.** Measured on Dragonfly v1.38.1 (podman, process VmRSS): idle RSS is **flat** across `dbnum` 16→1024 (~21 MB @1 shard, ~25 MB @4 shards) — per-DB structures are lazy (`DbSlice` ctor materialises only db0; `ActivateDb→CreateDb` on first write). Each **active** DB costs a fixed **~280 kB × num_shards** (1 shard → ~280 kB, 4 shards → ~1114 kB; independent of key count — the `kInitSegmentLog=3` → 8-segment DashTable floor, ~32 KiB mimalloc good-size class per segment). Hard ceiling 1024 DBs/instance. Source-corroborated against v1.38.1 (`common_types.h:24 kMaxDbId=1024`, `db_slice.cc`, `table.cc kInitSegmentLog`).
2. ✅ **RESOLVED 2026-06-05 — `dbnum` is immutable (restart-only).** `CONFIG SET dbnum 64` → `ERR … can't set immutable config`. Source: registered non-mutable (`main_service.cc:909`; `config_registry` `is_mutable=false` → READONLY). No online vertical growth — horizontal pool growth is the only capacity lever, as §1/§3 assume.
3. **Cross-DB command containment under `$N`.** Confirm `MOVE` / `COPY … DB` / `SWAPDB` are denied for a `$N`-pinned user; add explicit `-move` / `copy` restrictions if not.
4. **In-script ACL + DB confinement.** Confirm `EVAL` cannot touch keys outside DB N (declared-keys + `$N`); `ACL DRYRUN` + a live `EVAL`. If not enforced, take the §5 fallback.
5. **Client-library init under the ACL.** ioredis / node-redis / BullMQ connect, health, and `maxmemory-policy` checks pass; if a client probes `CONFIG GET maxmemory-policy`, decide `+config|get` vs disabling the check — do not widen to `+@dangerous`.
6. **Restart durability + recycle-safety.** Kill a pool pod; confirm the reconcile loop re-pins all live claims to the correct `dbnum`s. Allocate a recycled number; confirm `FLUSHDB`-on-allocation leaves no inherited keys.

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
