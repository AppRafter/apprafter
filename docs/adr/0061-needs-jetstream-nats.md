# ADR 0061: `needs.jetstream` → NATS — account per namespace, subject isolation, and what a deny vector can and cannot hold

## Status

`Accepted` (2026-09-11).

ADR for subphase 2.5 (`plan.md` §2.5). The subphase is pulled forward out of
speedrun bucket D: an external project needs JetStream on AppRafter, which is
the "2+ explicit requests" condition the bucket marker itself names. **`plan.md`
§3.2 (kine + NATS as control-plane storage) is not required by this and remains
deferred** — the two were coupled only by `plan.md:3136`'s note that a Tier-1
NATS could be embedded in kine, and this design does not take that route.

This record carries measurements taken on **nats-server v2.14.3** (podman,
2026-09-11) inline rather than by reference. They are not illustration: four of
them are the sole justification for decisions that look arbitrary otherwise, and
two of them contradict what the documentation implies. No gate in this
repository would catch their absence.

## Context

Phase 2.4 and 2.6 built the generic claim machinery — an `Application` declares
`needs.<type>`, the controller generates a `ResourceClaim` and pauses on
`AwaitingResourceClaim`, the 2.3 scheduler matches a `ServiceProvider`, the
`resourceclaim-provisioner` dispatches on `spec.backend`, a connection Secret is
resolved through `claim.<type>.<field>` references ([ADR 0046](0046-env-value-references.md)),
and a `RetainedClaim` with a seven-day grace reclaims it. `needs.jetstream` is
the fourth backend to use that machinery, and the first whose isolation model
does not follow from the previous ones.

Three facts separate it from `needs.pg` and `needs.redis`:

**There is no lifecycle operator.** `nats-io/nats-operator` (CRD `NatsCluster`)
is archived. The supported path is the official `nats` Helm chart — an ordinary
StatefulSet — plus **NACK** (`nats-io/nack`), which manages JetStream *objects*
(`Stream`, `Consumer`, `Account`) and not the server. NACK's `Account` CR is
connection configuration for `Stream`/`Consumer`, **not** an account — a reading
that produces a completely wrong implementation if taken the other way. So the
shape CNPG and Dragonfly use — an always-on operator plus a lazily created
instance CR — does not assemble.

**The account is a stronger boundary than Dragonfly's `$N`.** A NATS account
owns its subject namespace, its JetStream store, and native per-account quotas
(`max_mem`, `max_file`, `max_streams`, `max_consumers`). The noisy-neighbour
exposure [ADR 0042](0042-needs-redis-dragonfly.md) had to accept as a standing
risk is configurable here.

**Persistence is a property of the stream, not of the instance.** Dragonfly's
RDB/AOF covers a whole instance, which is why ADR 0042 §6 expressed per-claim
persistence as instance *placement* and ran two pools. A NATS account carries
both a memory and a file quota and each stream picks `storage: file | memory`,
so one server serves both classes and `persistent` does not apply to
`needs.jetstream` at all.

And one constraint that shapes everything else: **NATS subject wildcards are
whole-token.** `*` cannot match a prefix *within* a token, and a stream name is
a single token (dots are not legal in stream names). `$JS.API.STREAM.CREATE.<app>-*`
is therefore inexpressible: an application cannot be confined to streams whose
*names* it owns. Neither are a stream's *subjects* checked when it is created —
they travel in the request body, which NATS never inspects while evaluating
permissions. **Nothing about stream creation is constrainable; only publishing
is.**

## Decision

### 1. Deployment — the official chart, as a lazily enabled component

`platform-stack` gains `component_nats.cue` and `component_nack.cue`, both
shipping `enabled: false`. On the first matched jetstream claim the provisioner
merge-patches `PlatformStack.spec.overrides.nats.enabled = true`. This is not a
new mechanism: `#ComponentOverride.enabled` is a shipped field that
`PlatformController` already threads onto the child Argo CD Application, and the
CLI already merge-patches sibling keys in the same block.

The provisioner stamps `PlatformStack` with `apprafter.io/nats-auto-enabled:
"true"` when it enables the component, and disables again only while that
annotation is present, clearing it in the same patch — so an operator who turns
NATS on by hand keeps it regardless of the reaping predicate.

Using the chart rather than rendering a StatefulSet in provisioner code is what
makes §2 affordable: the chart owns the JetStream `fileStore` PVC, the Service,
the upgrade path, and — load-bearing — the **config-reloader sidecar**. The
account model needs a SIGHUP on config change, and `exec` into a pod from an
operator is not available to us.

**Ordering.** The naive order (enable, await readiness, then write the accounts
file) deadlocks: the server pulls the accounts file in via `include`, an
`include` of a missing file is fatal, and a pod mounting an absent Secret stays
in `ContainerCreating`. So: `platform-stack` ships the `nats-system` namespace
**unconditionally** (a bare namespace costs no pods and no budget, and shipping
it avoids granting the provisioner cluster-scoped `Namespace` create, which
[ADR 0024](0024-cluster-admin-constrain.md)'s posture would otherwise have to
justify for one fixed name) → the provisioner writes the `nats-accounts` Secret
→ flips the override → awaits readiness → verifies. Shipping a default accounts
file inside the chart is **not** the fix; it would make chart and provisioner
two owners of one Secret.

**How the accounts file reaches the reloader — measured on chart `nats` 2.14.6.** The
reloader is not given a directory to watch; it is given explicit `-config <file>` flags,
by default just `/etc/nats-config/nats.conf`, and there is no `reloader.extraArgs`. Three
chart-native values compose to close this without forking anything, and the chain was
verified by rendering:

- `config.merge` with a key **ending in `$include`** (e.g.
  `accounts$include: ./accounts-secret/accounts.conf`) emits both the `include` directive
  in `nats.conf` *and* an extra `-config /etc/nats-config/accounts-secret/accounts.conf`
  in the reloader's argv — the chart's `nats.reloaderConfig` helper walks the whole
  `config` tree for exactly this purpose;
- `podTemplate.patch` (JSON Patch, `add` on `/spec/volumes/-`) adds the
  provisioner-owned Secret as a volume;
- `container.patch` (`add` on `/volumeMounts/-`) mounts it at
  `/etc/nats-config/accounts-secret`, and `reloader.natsVolumeMountPrefixes` — default
  `["/etc/"]` — mirrors that mount into the reloader container automatically, with no
  `reloader.patch` needed.

**The mount must be a whole directory, never a `subPath`.** A `subPath` mount of a Secret
does not receive updates when the Secret changes — kubelet re-syncs whole-directory
mounts only (`kubernetes/kubernetes#50345`). Mounting the file directly at
`/etc/nats-config/accounts.conf` with `subPath: accounts.conf` reads as the tidier
arrangement and would defeat live reload **silently**: the file would simply never
change. The nested-directory form is the one that works, and the reason is recorded here
because nothing in the chart or in this design would otherwise explain the indirection.

The `jetstream.nats.io` CRDs arrive with the `nack` component, so stream
application is gated on the CRD being `Established`. Argo CD's
`SkipDryRunOnMissingResource` does not help — it governs Argo CD's dry-run, not
an operator's server-side apply.

**Namespaces.** The server lives in `nats-system`. It is the only object here
that application pods reach over the network, so its coordinates land in the
needs-derived egress policy ([ADR 0045](0045-needs-networkpolicy-egress.md)) as
a `(namespace, podSelector, port)` triple; naming a platform namespace there
would widen every jetstream application's egress into the namespace holding the
operator, webhook and provider seeds. The accounts Secret, the `mgr_<ns>`
credential and the NACK CRs live there too. Only the per-claim **connection
Secret** goes to the application's namespace, owner-ref'd to the
`ResourceClaim`.

**Reaping** follows ADR 0042 §9.1's predicate unchanged: no live claim
(ALLOCATED), no unallocated claim of the type (INTENT), no `RetainedClaim`
(RETAINED).

### 2. Tenancy — one account per namespace, in a config file

The account is `ns_<namespace>` (`-` → `_`; `_` is not legal in a DNS-1123
namespace name, so the fold is injective). It is a shared, lazily created
resource in exactly the sense the shared CNPG `Cluster` and the Dragonfly pool
instance already are — the "instance" is a block of configuration rather than a
pod, and the pool key is the namespace rather than a persistence class.

Accounts are **config-file accounts, not JWT/operator mode**. The chart renders
the base `nats.conf`; the provisioner owns a separate `nats-accounts` Secret
pulled in by an `include`. Two owners, two files — structurally the arrangement
Dragonfly's `aclFromSecret` already has (ADR 0042 §10). This keeps the
connection contract comparable to `needs.redis`: config accounts carry
`user`/`password`, which ADR 0046's env-reference model consumes directly.

The provisioner is the file's **sole writer**, and the file is re-derived
**whole** from the live claim set on every write, never patched. This is
required rather than stylistic: an application's deny vector depends on *other*
applications' declarations, so any claim change anywhere invalidates other
users' rules. Output is byte-stable, so an unchanged derivation is skipped.
Writes carry a `resourceVersion` precondition; leader election is real (the
operator runs Lease-based election and the provisioner's reconcile is serialised
under it).

The builder **refuses to emit** a file carrying `no_auth_user` or an account
without `users`. ADR 0042 §10's finding was that the dangerous failure of a
credential file is not lockout but *silently disabling authentication*; these
are the equivalents.

### 3. Isolation — a prefix, a deny vector, and one flag

Each application owns the subject prefix `<app>.` unconditionally. The claim
user `claim_<ns>_<app>_jetstream` gets:

**The allow list enumerates verbs, and that is the security boundary.** `deny` only
carves holes in what `allow` already admits; it is not, and cannot be, a boundary of its
own (§4.2). An operation this platform has not listed does not work — which is the
direction a platform's failures should point.

```
publish   allow: <app>.>
                 $JS.ACK.>    $JS.FC.>
                 $JS.API.INFO
                 $JS.API.STREAM.NAMES
                 $JS.API.STREAM.INFO.>          $JS.API.STREAM.MSG.GET.>
                 $JS.API.DIRECT.GET.>
                 $JS.API.CONSUMER.CREATE.>      $JS.API.CONSUMER.DURABLE.CREATE.>
                 $JS.API.CONSUMER.INFO.>        $JS.API.CONSUMER.DELETE.>
                 $JS.API.CONSUMER.LIST.>        $JS.API.CONSUMER.NAMES.>
                 $JS.API.CONSUMER.MSG.NEXT.>    $JS.API.CONSUMER.PAUSE.>
                 $JS.API.CONSUMER.RESET.>
                 $JS.API.STREAM.PURGE.<own stream declared allowPurge: true>   (§6)
          when `dynamicStreams: true`, additionally:
                 $JS.API.STREAM.CREATE.>        $JS.API.STREAM.UPDATE.>
                 $JS.API.STREAM.DELETE.>        $JS.API.STREAM.MSG.DELETE.>

  deny (B) every declared stream S in the namespace, INCLUDING THE APP'S OWN —
           only reachable at all when `dynamicStreams: true`, since the mutating
           verbs are otherwise absent from the allow list:
                        $JS.API.STREAM.UPDATE.S   .DELETE.S   .MSG.DELETE.S

  deny (C) every declared stream S the app neither owns nor consumes —
           BY POSITION, never by verb name:
                        $JS.API.*.*.S        $JS.API.*.*.S.>
                        $JS.API.*.*.*.S      $JS.API.*.*.*.S.>
                        $JS.ACK.S.>          $JS.ACK.*.*.S.>
                        $JS.FC.S.>           $JS.FC.*.*.S.>

  deny (D) every declared CONSUMER D on a stream S owned by another application:
                        $JS.API.CONSUMER.*.S.D    $JS.API.CONSUMER.*.*.S.D
                        $JS.ACK.S.D.>             $JS.ACK.*.*.S.D.>
                        $JS.FC.S.D.>              $JS.FC.*.*.S.D.>

subscribe allow: <app>.>, <inboxPrefix>.>
```

**There is no `subscribe.deny`, and no standalone "always deny" class.** An explicit
`allow` list is exhaustive — anything absent is denied — so `_INBOX.>`,
`$JS.API.STREAM.LIST`, `$JS.API.STREAM.SNAPSHOT.>`, `$JS.API.STREAM.RESTORE.>` and
`$JS.API.STREAM.LEADER.STEPDOWN.>` are all excluded by *not appearing*. Restating them as
denies would suggest the allow list is not exhaustive, which is the misreading most likely
to cause someone to widen it.

**`dynamicStreams` is an allow-list decision, not a deny-list one.** The constrained mode
simply does not receive `STREAM.CREATE`/`UPDATE`/`DELETE`/`MSG.DELETE`. An earlier draft
expressed it as a blanket deny; that was a worse formulation of the same rule, and it
concealed that the flag's whole effect is four entries in one list.

There is no `subscribe.deny`: an explicit `allow` list is already exhaustive.
`_INBOX.>` is excluded by *not appearing*, deliberately and load-bearingly.

`mgr_<ns>` is `publish: ">"`, `subscribe: ">"` within the account, including
`_INBOX.>` — NACK sets no custom inbox prefix and could otherwise not make a
single request.

**`dynamicStreams`** (per application, default `false`) is what moves (E). Its
primary value is not the reduced surface but that it makes §5's capture
detection exact for every opted-out application. Enabling it is a manifest edit,
therefore visible in git and gated (§7). KV buckets and object stores are
streams (`KV_<bucket>`, `OBJ_<bucket>`) created through `$JS.API.STREAM.CREATE`,
so the default mode disables them — a §"Scope" non-goal either way, but a
user-visible consequence of a flag whose name says "streams", documented as
such.

### 4. The measurements

#### 4.1 `sources` and `mirror` defeat the read half of any deny vector

A user denied every `$JS.API.*` subject addressing a stream — `INFO`, `MSG.GET`,
`DIRECT.GET`, the whole `CONSUMER.*` family, `ACK` — created a second stream
with `sources: [{name: <the denied stream>}]` and read the denied stream's
payload out of it, carrying the server's own
`Nats-Stream-Source: streama 1 > > a.test` header naming what had been copied.
Direct access remained denied throughout, before and after. The server performs
the copy **in the account's context**; the creating user's permissions are not
consulted.

**Against a `retention: workqueue` origin the same move is destructive.** A
workqueue stream deletes a message once acknowledged, so a sourcing stream
*drains* the origin: measured, an origin holding three messages, a denied user
creating a `sources` stream against it, **no pre-created origin consumer of any
kind**, and afterwards the origin reports `Messages: 0` with `First Sequence: 4`
while all three payloads are readable from the attacker's stream. The upstream
hint that sourcing "can use explicitly configured consumers" is a recommendation
and does not close the path. **`mirror` drains identically** — measured
separately, because once the effect is destruction rather than a copy, "the
single-source special case of the same mechanism" stops being a safe
generalisation.

This is a cross-tenant **data-destruction and denial-of-service** primitive
inside a namespace, reachable by any application that can create a stream,
against the headline JetStream use case.

**The mitigation was measured and holds.** With `$JS.API.STREAM.CREATE.>` and
`UPDATE.>` denied, the attempt fails. Two controls: consumer creation still
works, so the constrained mode does not break ordinary consumption; and a
pre-existing sourcing stream stays readable, so **flipping `dynamicStreams` from
`true` to `false` does not undo an existing capture** — §5's `delete` step is
what handles residue.

So: **the deny vector is a boundary on mutating declared streams and on forging
acknowledgements for everyone, and a boundary on reading only for applications
that cannot create streams.** `dynamicStreams: true` means, in plain words, *this
application can read every stream in its namespace and drain every workqueue in
it* — which is why enabling it is a `security-boundary` change and not a
convenience.

#### 4.2 Deny by position, never by verb name

An earlier draft expressed (C) as a list of verb names. A verb list is complete
only until the next NATS release. Both models were built and probed against the
same surface:

| subject | verb enumeration | position patterns |
|---|---|---|
| `STREAM.SNAPSHOT.S` | **allowed** | denied |
| `DIRECT.GET.S.<subject>` | **allowed** | denied |
| `CONSUMER.MSG.NEXT.S.<c>` | **allowed** | denied |
| `CONSUMER.RESET.S.<c>` (new in 2.14) | **allowed** | denied |
| `STREAM.LEADER.STEPDOWN.S` | **allowed** | denied |
| `STREAM.INFO.S`, `MSG.GET.S`, `CONSUMER.CREATE.S[.<c>[.<f>]]` | denied | denied |

Five forms leaked — including one that appears in no review of this design, and
one introduced in the very release it pins.

**The naming scheme makes the position patterns provably safe — but only because
the join separator is reserved.** The obvious objection is that
`$JS.API.*.*.*.S` also matches a *consumer* named `S`. It cannot bite: stream
names are `<owner>_<name>` and durables are `<consumer app>_<durable>`, and
because `_` is illegal in both components (§6), a durable can collide with a
stream name only when the two applications are the **same** one — and an
application's own streams are never in class (C). The webhook closes the
remainder by rejecting a declared stream name that collides with a declared
durable name within one application.

**This argument was broken until 2026-09-11 and the fix is in §6.** With the
earlier `-` join, a durable of application `a-b` composed to the same string as a
stream of application `a`, the two applications were different, and the
within-one-application webhook check did not reach it. The separator is not
formatting; it is what makes the sentence above true.

**But the position patterns are NOT complete against arbitrary depth, and no finite
set can be — measured.** Each pattern pair covers exactly one token depth. Probing
past the documented surface:

| subject | tokens | four patterns | six patterns |
|---|---|---|---|
| every documented shape (`STREAM.INFO.S` … `CONSUMER.MSG.NEXT.S.c`) | 5–6 | denied | denied |
| `$JS.API.STREAM.SOME.FUTURE.THING.S` | 7 | **Published** | denied |
| `$JS.API.A.B.C.D.E.S` | 8 | **Published** | **Published** |

Adding a depth-7 pair closes depth 7 and nothing else. `>` matches only as a trailing
token, so NATS has no way to say "any number of tokens, then literally `S`". **Extending
the set does not generalise; it moves the edge.**

**The resolution is that the deny vector does not have to be complete against all of
NATS — only against the allow list.** That is what §3's enumerated allow list buys. The
stream token sits at depth 5 or 6 in *every* operation this platform admits
(`STREAM.INFO.S` and `CONSUMER.CREATE.S` at 5; `STREAM.MSG.GET.S`,
`STREAM.MSG.DELETE.S`, `CONSUMER.DURABLE.CREATE.S.c` and `CONSUMER.MSG.NEXT.S.c` at 6),
so the four patterns are **provably complete against a closed set** — and that
completeness is a unit test, not an argument: for every subject the allow list admits,
assert the deny patterns match it when the stream is forbidden.

A future NATS verb at depth 7 does not leak, because it is not in the allow list and
therefore does not work at all. **The maintenance obligation moves from the deny side to
the allow side, where its failure is loud**: an application reaching for a new operation
gets `NOPERM` and says so, rather than silently acquiring reach. The component upgrade
gate's job changes accordingly — it no longer hunts for verbs the deny list forgot, it
reports verbs the allow list has not yet granted.

**The naming-collision argument above still holds** for the four patterns, and the
webhook check that closes it stands.

#### 4.3 The allow list is the control — twice over

Snapshot and restore move their *payload* over `$JS.SNAPSHOT.>`, a tree
`$JS.API.>` does not cover. Measured: a user carrying only the (E) blanket
published the restore **request** successfully, but the restore timed out and
**created nothing** (`No Streams defined` afterwards) because the chunk upload
had nowhere to go. `RESTORE` is denied in (A) as defence in depth rather than as
the fix for a live bypass — but the general inference stands: **widening the
allow list to `$JS.>` "to make something work" would hand back a capability
nothing else in this design blocks.**

`SNAPSHOT` was a live bypass, because its *delivery* subject is chosen by the
caller, who simply picks one under its own prefix. Measured: the same user
backed up a stream it had no read right to, and the payload was recoverable from
the resulting file. Snapshot is `mgr_<ns>`'s job under
[ADR 0050](0050-backup-restore.md); applications have no use for it.

These two findings arrived from opposite directions and say the same thing.
`$JS.SNAPSHOT.>` was out of reach because it was **outside the allow list**;
`STREAM.SNAPSHOT` was reachable because it was **inside** it, under `$JS.API.>`. §4.2's
depth measurement then showed that no deny list can be made complete on its own. All
three converge on one rule, which is the most portable thing in this record:

> **The allow list is the boundary. The deny list only carves holes in it.** Anything
> not listed does not work, and that is the failure direction a platform wants.

Widening the allow list is therefore never a local change, and a reviewer seeing one
should treat it as a security change regardless of what prompted it.

#### 4.4 The v1 → v2 acknowledgement subject migration

NATS 2.14 introduced v2 ack and flow-control subjects carrying a domain and an
account hash; from 2.15 they become the default. Observed form:

```
$JS.ACK._.Oq5NrhBh.lim2.push1.1.1.1.1789124646408750901.0
        ^ ^        ^
        | |        stream — token 5 in v2, token 3 in v1
        | account hash
        domain, degenerating to "_" when unset — but always present
```

Measured in both directions, by publishing to each shape as a user carrying each
rule:

| deny rule | v1-shaped publish | v2-shaped publish |
|---|---|---|
| `$JS.ACK.<stream>.>` only | violation | **Published** |
| `$JS.ACK.<stream>.>` + `$JS.ACK.*.*.<stream>.>` | violation | violation |

So the deny generator emits **both** forms for every stream, always, and **the
account hash never needs deriving** — domain and hash are exactly one token each
and both are always present, so `*.*` covers them. The catch-all *allow* forms
match both shapes and need no migration; only the deny vector is
version-sensitive.

The form is pinned by configuration rather than by version.
**`js_ack_fc_v2` is not a top-level key and not a key inside `jetstream { }`** —
both are rejected with `unknown field` — but the string is present in the
v2.14.3 binary and the server accepts it inside a top-level `feature_flags { }`
block, while `features` and `server_features` are rejected. That negative
control is what makes this a positive identification rather than a silent no-op.
`component_nats.cue` sets **`js_ack_fc_v2: true`**.

`true` rather than `false` because the value does not affect the deny vector at
all — both forms are emitted — but decides what **clients parse**: message
metadata is extracted from the ack subject's tokens and v2 shifts them by two, so
a client that knows only v1 returns silently wrong metadata. The feature has zero
users today, so taking v2 now costs nothing and means never migrating anyone.

The condition on that choice was the client, not the server. **`@onebun/nats` —
and so `nats.js`, the primary client for applications on this platform — parses
v2** (platform owner, 2026-09-11); the Go client does too, observed on the bench.
Applications in other languages remain the tenant's concern, and the operator
guide states the requirement rather than leaving it to be discovered.

The flag is undocumented, so it is trusted only as far as it is observed: the
component-level verification asserts once per start that the *observed* ack form
matches the configured one.

#### 4.5 Why `_INBOX.>` is excluded

Inside one account, subscribing to `_INBOX.>` yields every reply to every
application's requests — JetStream API responses *and the message payloads an
application fetches through a pull consumer or a direct get*. A neighbour does
not need to defeat the deny vector to read another application's data; it reads
it in flight. Each claim therefore gets its own `inboxPrefix`, delivered in the
connection Secret and set by the client through `CustomInboxPrefix`.

The provisioner's own verify-then-ready client must set it too. Without that, a
default client subscribes to `_INBOX.>`, takes a permissions violation, and the
provisioner concludes "not ready" — forever, on every claim.

### 5. Detection, and what it cannot see

A stream whose subjects touch `<app>.` is legitimate **iff** it is a declared
stream of *some* application in the namespace, or a dynamic stream of an
application whose subjects lie wholly under that application's own prefix.
Anything else was created by someone who should not have created it. Defining
legitimacy as "the victim's own declared streams" would have the detector flag a
neighbour's approved fan-in stream (§6) and, at the `delete` step, have the
platform destroy a live gated stream.

Detection runs on the provisioner resync that already lists the account's
streams. Policy ladder, `report` being the default: `report` (a
`ForeignSubjectCapture` condition plus an event), `delete` (remove the offending
stream; exposure bounded by the resync interval), and `quarantine` (revoke the
culprit's `STREAM.CREATE`/`UPDATE`) — which needs **attribution** and is
therefore not promised until a live `$JS.EVENT.ADVISORY.API` subscription is
verified to identify the requesting user. NATS records no creator on a stream;
`StreamConfig.metadata` is client-supplied and unenforceable.

**For an application holding `dynamicStreams: true` there is neither prevention
nor detection inside the namespace** — its own dynamic streams are
indistinguishable from a capture without attribution. The gate on turning the
flag on is the whole control.

### 6. The user-facing contract

`needs.jetstream` leaves the `#ServiceNeed | [...#ServiceNeed]` grammar
[ADR 0043](0043-needs-disk-named-claims.md) declares for every
`#PlatformServiceType` — an explicit amendment to that record — and takes its own
type: `{selector?, size?, dynamicStreams?, streams?, consume?}`, plus `name?` and
`persistent?` which exist **only to be rejected**. Declaring them is deliberate:
a structural schema *prunes* unknown fields before a validating webhook runs, so
omitting them would make `persistent: true` disappear silently and the manifest
appear to work.

```yaml
needs:
  jetstream:
    size: small
    dynamicStreams: false
    streams:
      - name: orders
        subjects: ["shop.orders.>", "billing.orders.>"]   # foreign token → gated
        storage: file
        retention: workqueue
        maxAge: "24h"
        maxBytes: "1Gi"        # required
        allowPurge: true       # default false
    consume:
      - from: feeder
        stream: blocks-head
        durable: indexer
```

**Fan-in.** Publishing is prefix-locked, so a stream confined to its owner's
prefix can only ever collect one application's traffic, and a shared workqueue
would be inexpressible. A declared stream may therefore name subjects **outside**
its owner's prefix. Producers are unaffected — each still publishes only under
its own prefix — so only the *collecting* application's permissions change, and
the declaration sits in the manifest of the application whose permissions change,
the same organising principle as `consume`. A symmetric `produce` was rejected: it
would have producers publishing into a foreign prefix, so an application's own
subject tree would stop being a complete picture of what it emits.

That reversal is only safe because **declared streams cannot be mutated from the
application side** — (B) above. Otherwise an application could declare an innocent
subject set, pass the gate, and add a neighbour's prefix at runtime with one
`STREAM.UPDATE`. The two decisions stand or fall together.

**Quota** is the sum of the namespace's live claims' `size` (an enum mapped to
bytes in the `jetstream-integrated` seed, tier-aware), with the tier ceiling
clamping the sum rather than each term. `maxBytes` is required on every declared
stream and the namespace total is checked before creation, surfacing
`QuotaExceeded` rather than an opaque server error through NACK. `max_mem` is
seeded conservatively at Tier 1: memory-storage streams live in the server's RSS.

**A non-zero account `max_mem` requires the chart to enable the server's memory
store, and the two are fatal apart — measured 2026-09-11.** The chart's default is
`memoryStore.enabled: false`, which renders `max_memory_store: 0`. With that, a
server whose *account block* requests any non-zero `max_mem` does not start: it
**exits 1 at JetStream startup**. Measured on 2.14.3 — explicit `0` exits, `192M`
reaches `Server is ready`. Note the distinction that makes this easy to
mis-reproduce: *omitting* `max_memory_store` lets the server pick its own default
and is harmless; the chart supplies an explicit zero.

So the two halves of this design are individually correct and lethal together. The
account-level fix — give `max_mem` a real value, because `storage: memory` was
advertised by the CUE, the CRD and the webhook's own error message while being
impossible in practice — turns an opaque per-stream `10028` into a **whole-server
crash-loop on the first jetstream claim** unless `component_nats.cue` enables the
memory store in the same change. Recorded because the coupling is invisible from
either side: a later reader looking at the chart sees a memory store nothing
obviously uses and removes it.

**The ceilings are per-namespace, and nothing yet clamps the global sum.** The
`#Size` mapping and its tier ceiling bound one namespace's account. N namespaces
can therefore oversubscribe both the server's `max_memory_store` and the shared
JetStream PVC. NATS does not crash on this — accounts simply compete for a total
that was promised twice — which is worse than a refusal because it surfaces as an
unattributable allocation failure in whichever tenant asks last. The provisioner is
the right place to close it: it already derives the accounts file from the **whole**
live claim set, so it can compute the global sum and refuse or clamp before
writing. Open until part 2 does so.

**Names** are collision-free by construction, in two distinct namespaces:

| | NATS-side | Kubernetes object |
|---|---|---|
| account | `ns_<namespace>` | `ns-<namespace>` |
| claim user | `claim_<ns>_<app>_jetstream` | — |
| management user | `mgr_<ns>` | — |
| stream | `<app>_<name>` | `<ns>-<app>-<name>` |
| durable | `<app>_<durable>` | `<ns>-<app>-<durable>` |
| subject prefix | `<app>.` | — |
| inbox prefix | `_INBOX_<ns>_<app>` | — |

The durable carries the **consuming** application's name: two applications may
each hold a durable called `indexer` on one shared stream, and without the prefix
the second would silently attach to the first's cursor and halve its delivery.

**The NATS-side join is `_`, and the separator is load-bearing — corrected
2026-09-11.** An earlier draft of this record joined with `-` and asserted
collision-freedom anyway. That was **false**, and measured so: `-` is legal in a
Kubernetes object name and there was no format rule on `#JetStreamStream.name` or
`consume[].durable` at all, so `("a", "b-c")` and `("a-b", "c")` both composed to
`a-b-c`. Two applications in one namespace could be handed the same NATS stream
name, which is not a cosmetic clash: the deny vector keys on that name, so the
collision decides who may read what.

The fix is the one that already makes `account()` sound — **reserve a character
that cannot appear in either component.** Declared stream and durable names are
constrained to DNS-1123 labels, whose alphabet is `[a-z0-9-]`, so `_` cannot occur
in them and the join is injective. `_` is legal in a NATS stream name, in a
consumer name and in a subject token — verified on v2.14.3, since the whole scheme
would be worthless if the composed name were unusable.

This also repairs §4.2's safety argument for the position patterns, which had
fallen with it: under a `-` join a durable of application `a-b` could equal a
stream of application `a`, so "a durable can only collide with a stream name of
the **same** application" was untrue and the webhook's within-one-application
check did not cover it. Under `_` the collision again requires `app == owner`, and
that check covers it exactly.

**Connection Secret** — `#ClaimFieldsFor.jetstream` gains
`["url", "host", "port", "user", "pass", "account", "subjectPrefix", "inboxPrefix"]`.

**The declarations ride the `ResourceClaim`, which is new.** `ResourceClaimSpec`
carries five generic fields — `type`, `name`, `selector`, `size`, `persistent` —
and deliberately nothing else: `needs.disk`'s `mountPath` lives on the
`Application` and is read by the renderer, never copied onto the claim. That split
is "the claim carries what the **provisioner** needs; app-side wiring stays on the
Application", and it has held for every backend so far.

jetstream is the first type where it does not hold, because `streams`, `consume`
and `dynamicStreams` are not app-side wiring — they are the **input to the
permission model**. Without them the provisioner cannot build an allow list or a
deny vector at all, and `JetStreamNeed::as_service_need()`, the only conversion
toward a claim, discards all three by design.

So `ResourceClaimSpec` gains a **typed, jetstream-scoped sub-block** rather than
three loose fields, mirroring the way `#Needs` gives `disk` its own `#DiskClaim`
value type instead of widening `#ServiceNeed`. The generic five stay generic.

Putting them on the claim rather than having the provisioner read `Application`s
is deliberate: the provisioner **watches** `ResourceClaim`s, so a declaration edit
updates the claim and triggers a reconcile through the path that already exists.
Reading declarations from a second Kind would mean new RBAC, new event plumbing,
and a claim→application resolution whose failure mode is a permission vector
computed from stale declarations.

**Per-environment overrides replace the whole slot, and for jetstream that is
loud rather than silent.** Every `needs.<type>` key is replaced wholesale by an
environment override — [ADR 0044](0044-per-environment-deploy.md)'s override-wins
model, which 2.16c extended to subfield deep-merge for `expose` and `imagePolicy`
while deferring `needs` to 2.16i. jetstream inherits that, and the consequence is
sharper than for any other need: `environments.prod.needs.jetstream: {size: large}`
drops `base`'s entire `streams` and `consume` block, i.e. the whole producer /
consumer contract, for one field's sake.

Leaving that silent was rejected. The webhook therefore **refuses** an environment
override of `needs.jetstream` that omits `streams` or `consume` while `base`
declares them, and says what was about to be lost. Special-casing jetstream into a
deep merge was also rejected: it would pre-empt 2.16i and make one need behave
unlike the other six, which is a worse thing to carry than a rule that tells you
to repeat yourself. When 2.16i lands the deep merge generally, this rule retires
with it.

### 7. Gating — three new ADR 0052 triggers

[ADR 0052](0052-migration-security-axis.md) carve-out #7 justifies not gating
`claim.*` references on the grounds that such a reference is *categorically*
self-scoped. `needs.jetstream` is the first claim-scoped construct that reaches
another application's data, so that wording is narrowed rather than left to be
read as still true.

| # | `type` | condition |
|---|---|---|
| 14 | `jetstream-consume-add` | a `consume` entry is added whose `from` is another application |
| 15 | `jetstream-dynamic-streams-enable` | `dynamicStreams` goes from effective `false`/absent to `true` |
| 16 | `jetstream-foreign-subject` | a declared subject is added whose first token is not the application's own name, **or** `allowPurge` is set on a stream whose subjects already contain one |

Trigger #15's approval text states the measured consequence — *read every stream
in the namespace, drain every workqueue in it* — not the field name. Trigger #16
fires on any foreign first token, including inert ones; narrowing it to tokens
matching a live application would need a sibling lookup the webhook does not have
and would reopen pre-positioned capture, so the breadth is deliberate and a
`PrefixPreCaptured` condition covers what it lets through.

`allowPurge` on a stream carrying only the application's own subjects takes no
trigger: it is destructive-to-self, which [ADR 0051](0051-app-scope-migration.md)
already governs. On a fan-in stream it destroys *other* applications' messages,
which is why #16 covers it — the argument "it reaches strictly its own stream" is
true of the object and false of the data.

### 8. Lifecycle

Provisioning follows §1's ordering, then: derive and write the accounts file →
**verify by connecting as the new user** (never a timer; this is what absorbs the
kubelet's projected-Secret refresh lag) → write `status.account`,
`subjectPrefix`, `inboxPrefix` → apply the NACK CRs once the CRDs are
`Established` → apply the connection Secret, owner-ref'd, in the claim's
namespace → ready.

`Stream` and `Consumer` live in `nats-system` while the `ResourceClaim` lives in
the application's namespace, and **Kubernetes forbids a cross-namespace
`ownerReference`**, so there is no cascade: the provisioner deletes them
explicitly, the pattern `needs.disk` already uses for its standalone PVC.

**GC**, after the seven-day grace, as `mgr_<ns>`: delete the application's
declared streams and its NACK CRs; delete **dynamic** streams by a rule that keys
on **subjects, never on names** — remove streams whose subjects lie wholly under
`<app>.`, leaving mixed-subject streams reported as `unattributed` (excluding
declared streams, whatever their owner). That rule is close to an invariant for a
producer, since publishing is permission-enforced, but it is **a heuristic, not an
invariant**, in two directions: a capture stream over a neighbour's subjects is
missed, and a capture stream over the *departing* application's prefix is swept
with it, reassigning ownership. Then drop the user, re-derive the file, delete the
connection Secret; when the namespace's last claim goes, the account goes with its
store.

**Clearing on allocation — and why ADR 0042's form does not transfer.** A reaped
component leaves its PVC, so re-enabling brings the store back with its old
streams (ADR 0042 §9.6). That ADR's answer was to clear on allocation. Applied
verbatim here it would **destroy live data**: its reuse unit was the *claim* (one
numbered database, one `FLUSHDB`), while here it is the *namespace account* shared
by N applications, and the accounts file is derived whole with no "did this account
exist before" state — so "clear the account on creation" would fire when the
**second** application in a namespace arrives. The clear is therefore predicated on
the same live-claim set: clear only when the namespace has no other ALLOCATED claim
and no `RetainedClaim`.

## Scope

**In scope:** the lazily enabled chart component and its reaping; account per
namespace in a provisioner-owned config file; the subject prefix, the deny vector
(A)–(E), `dynamicStreams`, per-app inbox prefixes; declared streams and durable
consumers through NACK; one-sided `consume`; fan-in through foreign declared
subjects; capture detection at `report`/`delete`; quota; the connection contract;
GC and the allocation clear.

**Out of scope:** cross-namespace sharing; two-sided sharing; a `produce`
declaration; named jetstream claims; per-application quotas; HA/clustered
topology (Tier 2+); KV and Object Store; a `SharedStream` analogue of
[ADR 0049](0049-cross-app-sharedvolume.md); rendering the stream inventory in
`apprafter app status`; the `quarantine` step until attribution is verified;
backup and restore of JetStream stores.

**Design constraint:** `platform-stack` **never configures a JetStream domain**
at any tier this ADR covers. A domain prefixes every API subject
(`$JS.<domain>.API.…`) and shifts every pattern in §3 by one token, silently
unmatching the entire deny vector. It is our configuration, so it is ruled out by
construction; introducing one later is a change to §3, not a values tweak.

## Consequences

- **Easier:** `needs.jetstream` reuses the whole 2.4 pipeline; the account gives
  native per-tenant quotas that ADR 0042 could not have; one server serves both
  persistence classes; the config-file account model is durable across restarts
  by construction, so ADR 0042 §4's ACL reconcile loop has no analogue here.
- **Harder:** the deny vector is derived from the *whole namespace's*
  declarations, so any claim change anywhere re-derives it; the provisioner
  performs imperative NATS I/O; the enumerated half of the vector carries an
  upgrade-gate obligation; and the security properties rest on measurements that
  a server upgrade can invalidate.
- **Visible:** an application declaring `dynamicStreams: true` will pause for
  approval, and its neighbours may surface `NamespaceDrainRisk`. Both are
  intended; both will be reported as bugs at least once.
- **CLI is in the release chain.** `apprafter app validate` embeds the schema at
  compile time (`include_str!`), so the `#ClaimFieldsFor` change is baked into the
  binary. A stale CLI would reject a valid `claim.jetstream.url` reference locally
  while the cluster accepts it — a skew whose lesson to the developer is to stop
  trusting the local gate.

## Alternatives considered

- **Account per application.** Hard isolation, per-application quotas, dynamic
  streams genuinely private, and the only structural answer to an adversarial
  neighbour inside a namespace. Rejected for the MVP: sharing a stream then needs
  cross-account `exports`/`imports`, and for JetStream that means exporting a
  mapped slice of `$JS.API`. Kept as the higher-tier escape hatch.
- **Operator/JWT mode with the full resolver.** The native dynamic-tenancy path,
  no reload and no Secret-projection lag. Rejected: Ed25519/nkeys signing inside
  the provisioner, and a `jwt`+`seed` pair that fits ADR 0046's env model poorly.
  Migration would be a breaking change to the connection Secret's shape.
- **An always-on NATS component.** Aligned with `plan.md` 3.2, where kine + NATS
  makes the server permanent anyway. Rejected for launch: a permanent ~128Mi
  reservation on the Tier-1 node for a feature most clusters do not use regresses
  the [ADR 0053](0053-resource-governance.md) budget.
- **A provisioner-rendered StatefulSet.** Obliges us to reimplement the
  config-reloader sidecar the account model depends on.
- **Imperative stream creation without NACK.** Drift reconciliation, status and
  retry would all be ours.
- **No declarative streams at all** (the account is the whole deliverable, as
  `needs.pg` ships no tables). Rejected: durable consumers and producer/consumer
  contracts are the reason to reach for JetStream, and both want to be in git.
- **Confining declared subjects to the owner's prefix.** Removes foreign capture
  outright but forecloses fan-in permanently; replaced by trigger #16.
- **A namespace-wide or platform-wide `dynamicStreams` knob.** Rejected: it would
  be left permissive, returning the problem unchanged. The per-application flag
  earns its place through the exact detection it enables.
- **Hashing the subject prefix** so it exists only in the connection Secret. It
  does defeat blind capture. Rejected on cost: a permanent, non-rotatable cluster
  secret that must itself be backed up (ADR 0050); it destroys `nats sub 'app.>'`
  during triage, the inventory's readability and the GC rule; and a `consume`
  neighbour learns the prefix legitimately anyway.
- **Advisory capture into a stream for attribution.** Deferred: attribution would
  live only as long as the capture stream's retention, failing on exactly the old
  streams GC cares about, and the platform's replayable audit log (`spec.md` §4.10,
  `plan.md` 3.2) needs the same capture stream.

## Risks

- **Reading is not isolated inside a namespace for `dynamicStreams: true`
  applications, and workqueues are drainable.** Measured (§4.1). *Mitigation:*
  default `false`, trigger #15, `NamespaceDrainRisk` on both parties, detection at
  `report`/`delete`. *Accepted residual:* a compromised image under a legitimately
  granted flag passes no gate.
- **A server upgrade can silently unmatch the deny vector.** `CONSUMER.RESET`
  arrived in the pinned release; the v2 ack form shifts token positions.
  *Mitigation:* position patterns for the total-denial class, both ack forms
  always, image pinned, upgrade gate checks the verb set and the observed ack form.
- **`feature_flags` is undocumented.** A rename makes the setting a silent no-op.
  *Mitigation:* both deny forms are emitted regardless; the observed-vs-configured
  assertion turns silent drift into a condition.
- **Ephemeral and dynamically created consumers are unprotected.** Their names are
  unknowable, so (D) cannot name them. *Accepted*, alongside the dynamic stream
  layer.
- **`_INBOX` isolation depends on the client.** An application that does not set
  `CustomInboxPrefix` cannot connect at all — a loud failure, which is the right
  direction, but it is friction the guide must pre-empt.
- ~~**The reloader may not watch the included file.**~~ **Closed by measurement**
  (§1): the chart composes `config.merge`'s `$include`, `podTemplate.patch` and
  `container.patch`, and mirrors the mount into the reloader through
  `natsVolumeMountPrefixes`. The residual is narrower and is recorded in its place: a
  `subPath` mount would silently never update, so the whole-directory form is a
  correctness requirement rather than a style choice.
- **A widened allow list silently restores everything the deny vector cannot catch.**
  §4.2 measured that no finite deny set is complete against arbitrary token depth, so
  §3's enumerated allow list is what makes the vector complete. Adding an entry to that
  list — for a new NATS operation, or to unblock an application — is a security change
  wearing the clothes of a compatibility fix. *Mitigation:* the allow list is generated
  from one table in one file with a comment saying this; the upgrade gate reports
  ungranted verbs rather than hunting for forgotten denies; and a reviewer is told, in
  §4.3, to treat a widening as a security change regardless of what prompted it.
- **Quota exhaustion is namespace-wide**, since JetStream quotas are per account.
  *Accepted;* the escape hatch is account-per-application.
- **GC's dynamic-stream rule is a heuristic** in both directions (§8). *Accepted
  and recorded* rather than presented as an invariant.

## Pre-merge verification

1. ~~The reloader watches the included accounts file.~~ **Settled 2026-09-11 by
   measurement — §1.** The `config.merge` `$include` + `podTemplate.patch` +
   `container.patch` chain was verified by rendering chart 2.14.6. What remains is the
   *live* half: that a Secret update actually reaches the container and fires the SIGHUP
   on a real cluster, which part 2's first integration test covers.
2. ~~The remaining API surface against the position patterns.~~ **Settled 2026-09-11 by
   measurement — §4.2**, and it changed the design: the patterns are complete against
   depths 5–6 and leak at 7 and beyond, which is why §3's allow list enumerates verbs.
   What remains is the **completeness test against the allow list** — for every subject
   the allow list admits, assert the deny patterns match it when the stream is
   forbidden. That is a unit test, not a session.
3. ~~`nats.js` parses v2 ack metadata.~~ **Settled 2026-09-11 by the platform owner:
   `@onebun/nats` parses v2.** This was the only client-side item in the list and the
   last condition on §4.4's choice of `true`; recorded as an owner statement rather
   than a bench measurement, which is the appropriate standard for a fact about our own
   client stack. Other languages remain the tenant's concern and are named in the guide.
4. Reload on an invalid accounts file (expected: rejected, server keeps running)
   and startup on one; `no_auth_user` and an account without `users`.
5. PVC survival across component disable → enable, and the remanence §8 assumes.
6. A live `$JS.EVENT.ADVISORY.API` subscription identifies the requesting user,
   for §5's `quarantine` step.
7. NACK `Stream` against an `Account` authenticated by user/password connects and
   reconciles.

## Owner

Platform / operator team.

## Re-evaluation

Revisit if: adversarial isolation is required *inside* a namespace, or a tenant
needs a quota of its own (→ account per application); a NATS release changes ack
subject forms again or removes `feature_flags` (→ §4.4); sourcing begins checking
source permissions (→ §4.1's table becomes stricter than documented, which the
walk asserts in both directions); `plan.md` 3.2 lands and makes the server
permanent infrastructure (→ §1's lazy enablement becomes pointless); or the
`quarantine` step is wanted (→ verification item 6).

## References

- `plan.md` §2.5; the design spec
  `docs/superpowers/specs/2026-09-11-needs-jetstream-design.md` (untracked —
  `docs/superpowers` is gitignored, which is why the measurements live in this
  record rather than being cited from it).
- [ADR 0042](0042-needs-redis-dragonfly.md) (the structural sibling — §9 reaping
  predicate, §10 file-backed credentials), [ADR 0043](0043-needs-disk-named-claims.md)
  (amended: the `#ServiceNeed` grammar), [ADR 0045](0045-needs-networkpolicy-egress.md),
  [ADR 0046](0046-env-value-references.md), [ADR 0050](0050-backup-restore.md),
  [ADR 0051](0051-app-scope-migration.md), [ADR 0052](0052-migration-security-axis.md)
  (amended: carve-out #7 and three triggers), [ADR 0053](0053-resource-governance.md),
  [ADR 0024](0024-cluster-admin-constrain.md).
- `operator/operator-controllers/resourceclaim-provisioner/`,
  `operator/operator-rendering/src/egress.rs`,
  `schemas/v1alpha1/application.cue` (`#ClaimFieldsFor`),
  `cli/platform-cli/src/commands/app_validate.rs` (the compile-time schema embed),
  `platform-stack/cue/platform.cue` (`#ComponentOverride`),
  `cli/docsgen/src/shipped.rs`.
- NATS: accounts and per-account JetStream limits; whole-token subject wildcards;
  `deny` precedence over `allow`; stream sourcing and mirroring; the 2.14 v2
  ack/flow-control subjects; `nats-io/nack` (`Account.spec.user`).
