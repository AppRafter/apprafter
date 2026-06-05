# ADR 0042: needs.redis → Dragonfly — keyspace-isolated ACL users on lazy shared instances

## Status

Accepted (2026-06-05).

## Context

Phase 2.6 ships `Application.spec.base.needs.redis` — a developer declares a Redis dependency and gets a working connection string, the same way `needs.pg` (Phase 2.4, ADR-less but decomposed in `plan.md` 2.4a–g) yields a Postgres `DATABASE_URL`. The generic machinery 2.4 built is reused as-is: the Application controller generates a child `ResourceClaim` and pauses on `AwaitingResourceClaim` (2.4d); the 2.3 scheduler matches a `ServiceProvider`; the `resourceclaim-provisioner` controller provisions per-claim resources and writes `status.ready`/`connectionSecretRef` (2.4c); `operator-rendering` injects the connection Secret as env (2.4e); a finalizer snapshots a `RetainedClaim` and a 7-day-grace GC reclaims it (2.4f). The provisioner already dispatches on `ServiceProvider.spec.backend` (`Backend::from_spec_backend`); `cloudnative-pg` is wired, a `dragonfly` arm is the slot.

Redis differs from Postgres in ways that force genuinely new decisions:

- **Isolation is weak by construction.** Logical DBs (`SELECT n`) are not a security boundary — any authenticated client can `SELECT` any DB. `requirepass` is a single **server-global** password, not per-tenant. So the `plan.md` 2.6 sketch ("requirepass per-claim, DB-namespace per claim") cannot provide real per-claim isolation as written. Real isolation needs **Redis ACL users** (`ACL SETUSER`), and the only hard keyspace boundary ACL offers is a **key-pattern** restriction (`~prefix:*`).
- **No declarative per-user API.** Neither a bare Dragonfly nor the `dragonfly-operator` (its `Dragonfly` CR manages *instances*, not users) exposes a declarative per-ACL-user resource. Per-claim users must be created **imperatively** over the Redis protocol — unlike CloudNativePG, where the provisioner applies `Database`/role CRs and CNPG reconciles them.

Dragonfly is a single-process, multi-threaded Redis-compatible server with first-class Redis ACL support and an official Kubernetes operator (`dragonflydb/dragonfly-operator`) whose `Dragonfly` CR handles instances, auth, replication, and snapshot persistence.

## Decision

We will provision `needs.redis` onto **lazy shared Dragonfly instances**, isolating claims by **per-claim ACL users restricted to a key prefix**, driven **imperatively** by the provisioner.

- **Deployment — operator + lazy shared instances.** `platform-stack` installs the `dragonfly-operator` as an always-on component (mirroring the always-on CNPG operator) and seeds a `redis-integrated` `ServiceProvider` (`type: redis`, `backend: dragonfly`). The provisioner lazily creates up to **two** shared `Dragonfly` CRs in `dragonfly-system`: `platform-redis` (ephemeral) and `platform-redis-persistent` (snapshot → PVC, the PVC created with it). Each is created on the first claim that needs it; solo clusters with no Redis apps run no Dragonfly pod. Higher tiers get HA/replication via the same CR.
- **Per-claim isolation — keyspace-prefix ACL user.** For each claim the provisioner creates a Dragonfly ACL user `claim_<ns>_<app>_redis` with `on >\<generated-password\> ~claim_<ns>_<app>_redis:* +@all -@dangerous -@admin -@scripting` — hard keyspace isolation (cannot read or write another claim's keys) plus blocking server-wide / admin / Lua commands on the shared instance. The provisioner holds the instance's admin credential (from the operator-managed auth Secret) and runs `ACL SETUSER` over a Redis client (a new provisioner dependency).
- **Per-claim opt-in persistence.** `#ServiceNeed` gains `persistent?: bool` (default `false`), threaded into the `ResourceClaim` (4 mirrors: CUE, kube-rs, OpenAPI CRD, webhook). `needs.redis: {}` routes to the ephemeral instance; `needs.redis: {persistent: true}` routes to the persistent instance (lazily created with its PVC on first use). The operator supports both; the app opts in.
- **Connection contract — two keys.** The per-claim connection Secret (claim namespace, owner-ref'd to the claim → cascades on delete) carries `REDIS_URL = redis://claim_<ns>_<app>_redis:\<pass\>@\<instance\>.dragonfly-system.svc:6379/0` **and** `REDIS_PREFIX = claim_<ns>_<app>_redis:`. The renderer's needs→env table (2.4e) generalises from one env var per need to a **list of `(env-var, secret-key)` pairs**: `pg → [(DATABASE_URL, DATABASE_URL)]`; `redis → [(REDIS_URL, REDIS_URL), (REDIS_PREFIX, REDIS_PREFIX)]`. The webhook's reserved-env guard rejects an app literally setting `REDIS_URL`/`REDIS_PREFIX` when `needs.redis` is present (mirrors the `DATABASE_URL` guard).
- **GC — imperative cleanup.** The generic RetainedClaim GC (2.4f) gains a Dragonfly path: after the grace floor, connect with the admin credential, `ACL DELUSER claim_<ns>_<app>_redis`, then `SCAN MATCH claim_<ns>_<app>_redis:* | UNLINK` to drop the claim's keys (a shared instance cannot be `FLUSHDB`'d), then delete the connection Secret. The shared instances persist (like the shared CNPG cluster).

In scope: Tier-1 single-shared-instance (per persistence class) provisioning, keyspace-prefix isolation, opt-in persistence, the DSN+prefix injection, the GC. Out of scope: HA/replication topology tuning (the operator supports it; tier defaults are a later concern), Redis Cluster sharding, multi-tenant *adversarial* isolation (see Risks), and a `needs.redis` size/eviction-policy surface beyond the MVP.

## Consequences

- **Easier:** `needs.redis` reuses the entire 2.4 claim → schedule → provision → inject → GC pipeline; only a backend arm + a schema knob + a renderer generalisation are new. Isolation is a *hard* keyspace boundary (stronger than logical-DB). Persistence is opt-in and lazy, so ephemeral cache users and `€5 Solo` clusters pay nothing. The dragonfly-operator handles instance lifecycle, auth, and snapshots.
- **Harder / neutral:** the provisioner now performs **imperative Redis I/O** (a Redis client dependency, `ACL SETUSER`/`DELUSER`, `SCAN`/`UNLINK`) — a different failure surface than declarative CR apply (network to the instance, command errors, retries). **Apps must use their key prefix** — a client writing unprefixed keys gets `NOPERM`; the platform injects `REDIS_PREFIX` and documents it, transparent only for clients with a `keyPrefix` option. Two shared CRs (per persistence class) instead of one.

## Alternatives considered

- **Logical-DB-number isolation + per-claim ACL user (soft).** Each claim a DB number + an ACL user. Simpler DSN, no key-prefix burden on the app — but DB numbers are not a boundary (ACL cannot restrict `SELECT`), so a claim can read another's data. Rejected: we chose a hard boundary over the convenience.
- **Single global `requirepass`, DB-number isolation.** The `plan.md` sketch. Every app shares one credential and can reach every DB — no isolation. Rejected.
- **Per-claim Dragonfly instance.** A pod (+ PVC if persistent) per claim → hard, even adversarial isolation. Rejected for Tier 1: contradicts the single-shared-instance / lazy-shared CNPG decision and the Solo cost target. Reserved as a higher-tier / Regulated upgrade.
- **Bare Dragonfly `Deployment` (no operator).** Lighter, but the operator gives HA, auth, and snapshot management for free and keeps the deploy model uniform with CNPG; instance lifecycle is the operator's job either way. Rejected in favour of the operator.
- **Always-on shared Dragonfly in platform-stack.** No lazy logic, but pays for a pod on every cluster including Redis-less ones. Rejected — inconsistent with the CNPG lazy-shared decision.

## Risks

- **Key-prefix adoption friction.** An app that does not apply `REDIS_PREFIX` gets `NOPERM` and breaks. Mitigation: inject the prefix + document it prominently; note the `keyPrefix`-capable clients where it is transparent. Accepted as the cost of a hard boundary.
- **Imperative ACL fragility.** A failed `ACL SETUSER` mid-provision, or a Redis-client/network error, must not corrupt state. Mitigation: idempotent ACL operations (re-run safe), provisioner requeue on error, and the claim stays unready until the user exists (the 2.4d gate already holds the app).
- **Snapshot data-loss window.** Dragonfly persistence is point-in-time snapshots, not a WAL — a crash loses writes since the last snapshot. Mitigation: documented; a sensible snapshot interval; apps needing zero-loss durability use Postgres.
- **Shared-instance blast radius / ACL escapes.** A bug letting a claim run `-@dangerous`/`-@admin`, or a future Dragonfly ACL gap, would breach isolation on a shared instance. Mitigation: the deny-list ACL, no admin/scripting, and the per-claim-instance upgrade path for Regulated. The model is *trusted single-tenant* (a customer's own apps), not adversarial multi-tenant.
- **ACL-user growth.** Many claims → many ACL users on one instance. Accepted at Tier-1 scale; revisit if a single instance's user count or memory becomes a limit (→ shard onto more instances).

## Owner

Platform / operator team.

## Re-evaluation

Revisit if: multi-tenant adversarial isolation is required (→ per-claim instances), Redis-Cluster sharding or HA tuning is needed, the key-prefix friction blocks adoption (→ reconsider soft DB-number isolation or a prefixing proxy), or a single shared instance's ACL-user / memory footprint hits a limit.

## References

- `plan.md` Phase 2.6; ADR 0026 (PlatformStack), the 2.4 needs.pg decomposition (`plan.md` 2.4a–g) and `resourceclaim-provisioner` (the generic backend-dispatch this extends).
- `dragonflydb/dragonfly-operator` (`Dragonfly` CR — instances, auth, snapshots); Redis ACL (`ACL SETUSER`, keyspace patterns).
- `operator/operator-controllers/resourceclaim-provisioner/`, `operator/operator-rendering/src/lib.rs` (the needs→env table), `schemas/v1alpha1/application.cue` (`#ServiceNeed`).
