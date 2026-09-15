# ADR 0065: Two gaps in the manifest — probes, and JetStream tuning

## Status

`Accepted` (2026-09-15). ADR for subphase 2.28 (`plan.md` §2.28).

Two unrelated capabilities in one record because they ship as one release:
both change the `Application` CRD, and two CRD upgrades back to back at a
live cluster is risk with no upside. §1 is the probe surface; §2 is the
JetStream tuning surface and **amends [ADR 0061](0061-needs-jetstream-nats.md)
§6**, the user-facing contract. Neither part changes ADR 0061 §3's allow
list or deny vector.

The provider field sets quoted in §2 were read out of the pinned `nack`
0.35.0 chart's CRDs (`jetstream.nats.io/v1beta2`) on 2026-09-15, not
recalled. The facts §2.4 depends on are **owed measurements**, listed in §5,
and are to be taken on nats-server v2.14.3 — the image
`platform-stack/cue/component_nats.cue` pins deliberately below its own
chart's default.

## Context

**Probes.** AppRafter workloads had no probe surface at all.
`#ApplicationSpec` carried no field for one, and `render_deployment`
(`operator/operator-rendering/src/lib.rs`) built its `Container` from
`name`, `image`, `env`, `ports`, `volumeMounts` and `resources` — nothing
else. Every application therefore ran with the kubelet's default posture: a
pod is Ready the instant its container process starts, and a hung process is
never restarted. Two consequences followed from that and were both live.
First, every rolling update had a window in which the new pod was Ready and
receiving traffic while its process was still binding. Second, an
application whose `expose.port` was wrong "worked", because nothing ever
checked that anything was listening on it.

The only trace of the feature in the tree was a deferred scaffold-template
field (`spec.healthcheck`, noted in 1.79b Part 4), which was never a schema.

**JetStream.** `consumer_object`
(`operator-controllers/resourceclaim-provisioner/src/nats.rs`) hard-coded
`ackPolicy: explicit` and `deliverPolicy: all` and carried nothing else. A
manifest could name a durable and its stream; it could not say how long to
wait for an acknowledgement, how many times to redeliver, how many messages
may be in flight, or where a message goes when redelivery is exhausted. Six
stream fields were exposed and the rest were not.

This is not merely a missing convenience. An application *can* create and
update its own consumers — `$JS.API.CONSUMER.CREATE.>` and
`CONSUMER.DURABLE.CREATE.>` are in the allow list — but NACK owns a declared
durable and re-asserts its own configuration, so a client-side `ackWait` is
reverted on the controller's next pass. The setting was therefore neither
client-owned nor git-owned: it was unreachable.

## Decision

### 1. Probes

#### 1.1 The shape

A `probes` block on `#ApplicationSpec` and on `#ApplicationEnvOverride`,
carrying `liveness`, `readiness` and `startup`, each a `#Probe`.

**The probe FORM is discriminated by `path`**: present means an HTTP GET,
absent means a TCP connect. There is no `httpGet` / `tcpSocket` nesting —
the Kubernetes shape exists to carry five action kinds and this surface
carries two. HTTP and TCP only: gRPC probes require the image to serve
`grpc.health.v1`, which most do not, and `exec` runs an arbitrary command in
the container on every period, which is a reliable way to spend a Tier-1
node's CPU. Both remain additive later.

**Timings are integer seconds under Kubernetes' own field names**, not the
duration strings `#JetStreamStream.maxAge` uses. The inconsistency is
deliberate: a Kubernetes probe is specified in whole seconds, so a string
field would accept `"500ms"` and we would then have to reject it. Better
that the wrong thing cannot be written down.

`enabled: false` keeps a declaration in git while taking the probe off the
pod. Declaring it alongside a full probe body is **not** a contradiction —
it is the reason the field exists.

#### 1.2 Defaults, and the two layers they live in

Effective values for a declared probe that omits them: `periodSeconds` 10,
`timeoutSeconds` 2, `failureThreshold` 3, `successThreshold` 1,
`initialDelaySeconds` 0, `port` = the effective `expose.port`, `scheme`
`http`.

One deviation from Kubernetes' own defaults: the timeout is 2 seconds, not
1. On Tier 1 the node has one or two shared vCPUs, and a one-second budget
for an in-pod HTTP round trip turns an ordinary GC pause into a probe
failure. This is recorded as a **choice, not a measurement** — §5 owes the
measurement, and if a 1-second timeout shows no false failures under load we
take Kubernetes' number.

**Every one of those defaults is applied by the renderer, and none of them
reaches the CRD.** That is not a choice made for probes; it is this
repository's standing rule, and `crdgen` enforces it: `structural::resolve`
strips a CUE `*x` default and the R4-M2 assertion
(`operator/crdgen/src/check.rs`) fails the build if any `default:` survives
into a rendered CRD, on the grounds that behaviour belongs to the renderer
and not to the apiserver ([ADR 0047](0047-crd-codegen-from-cue.md)). The
`*true` on `#Probe.enabled` and the `*"http"` on `#Probe.scheme` therefore
document intent to a reader of the schema and to `cue vet`; they are not a
second mechanism, and `cue export` does not materialise them either, because
an optional CUE field with a default is simply omitted.

The rule is worth keeping for exactly the reason it would matter here if it
did not exist: a CRD default is stamped into the stored object at admission,
so it would freeze at whatever the operator believed on the day that object
was written, and revising `periodSeconds` later would leave every existing
`Application` carrying the old number invisibly. The consequence to plan
around is that neither the manifest nor the stored object shows the
effective numbers — which is what the `apprafter app status` probes line is
for.

*(Corrected 2026-09-15, before release: an earlier draft of this section
claimed booleans and enums do reach the CRD as `default:` while the numeric
knobs deliberately do not. They do not either — checked by generating the
CRD and finding zero `default:` keys in it, and then finding the gate that
forbids them. The design outcome is unchanged; the reason for it is
repository-wide rather than specific to this field set.)*

#### 1.3 A default readiness probe, and what it costs

**When `probes.readiness` is absent and `expose.port` is present, the
renderer emits a TCP connect probe on that port.**
`probes: {readiness: {enabled: false}}` opts out.

Nothing is guessed: the port is the one the manifest already declares, and a
TCP connect asserts only what the `Service` already assumes. Without it,
every rolling update has the 502 window described in Context.

Two consequences, both breaking, both named in the release notes rather than
discovered:

1. **Every existing Deployment rolls once** on the operator upgrade, because
   the pod spec changes.
2. **An application whose `expose.port` is wrong becomes NotReady for the
   first time.** Surfacing it is a fix, and it will be reported as a
   regression.

#### 1.4 A derived startup probe

When `liveness` is declared and enabled and `startup` is absent, the
renderer derives a startup probe against the same target with
`periodSeconds: 5` and `failureThreshold: 60` — five minutes to come up,
after which liveness takes over. An explicit `probes.startup` wins outright;
there is no field-level merge between the two.

The derived cadence is deliberately **not** inherited from the liveness
probe's own period: a liveness period is tuned for how fast a hang should be
caught, which is the opposite question from how long a start should be
tolerated.

This is the one place the platform adds something the manifest did not ask
for, and it is there because the failure it prevents is the common one: a
user adds a single liveness probe, the application takes forty seconds to
warm a cache, and the kubelet kills it at thirty, forever. A derived startup
probe cannot make a working application fail; it can only postpone the first
liveness kill.

#### 1.5 Validation, by layer

CUE and the CRD carry what they can state structurally: the port range, the
positive thresholds and periods, the `scheme` enum, and — as a CRD
`pattern`, not a CUE regex stub, per the validation policy — `path`'s
leading `/`.

The admission webhook carries the five rules they cannot:

1. `path` starts with `/` (restated, so a cluster whose CRD predates the
   patch is still covered);
2. `scheme` / `headers` without `path` are rejected rather than ignored;
3. a probe resolves a port — its own, or the scope's `expose.port`;
4. `successThreshold` is 1 on liveness and startup, because Kubernetes
   requires it and rejecting here names the field instead of failing later
   on the Deployment;
5. `timeoutSeconds < periodSeconds`. Kubernetes permits the overlap; we do
   not, because overlapping probe attempts are always a mistake and the
   refusal costs one edit.

#### 1.6 No migration gate, on either axis

A probe edit destroys no data ([ADR 0051](0051-app-scope-migration.md)) and
moves no security boundary ([ADR 0052](0052-migration-security-axis.md)).
Its worst outcome is a pod restart, which every image-tag change already
causes ungated. Recorded as an explicit decision so it is not re-argued.

### 2. JetStream tuning

#### 2.1 Stream fields

`#JetStreamStream` gains, all optional, each mapping 1:1 onto the NACK
`Stream.spec` field of the same name: `maxMsgs`, `maxMsgsPerSubject`,
`maxMsgSize`, `maxConsumers`, `discard` (`old` default | `new`),
`discardPerSubject`, `duplicateWindow`, `compression` (`none` default |
`s2`), `allowDirect`, `allowRollup`, `consumerLimits`, `description`.

`allowDirect` defaults to `false`, which is today's behaviour, even though
`$JS.API.DIRECT.GET.>` is already granted: the grant is inert until a stream
opts in, and flipping the default would change every existing stream on
upgrade for no asked-for reason.

`allowRollup` takes the same shape as `allowPurge`. On a stream carrying
only its owner's own subjects it is destructive-to-self and governed by ADR
0051's axis with no security trigger; on a fan-in stream carrying a
neighbour's subjects it deletes a neighbour's data, and is ADR 0052 trigger
#16 exactly as `allowPurge` is.

#### 2.2 Consumer fields

`#JetStreamConsume` gains `ackPolicy` (`explicit` default | `all` | `none`),
`ackWait`, `maxDeliver`, `backoff`, `maxAckPending`, `filterSubject`,
`filterSubjects`, `deliverPolicy` with `optStartSeq` / `optStartTime`,
`replayPolicy`, `maxWaiting`, `maxRequestBatch`, `maxRequestExpires`,
`maxRequestMaxBytes`, `inactiveThreshold`, `rateLimitBps`, `headersOnly`,
`memStorage`, `sampleFreq`, `description`.

Cross-field rules, webhook-enforced: `filterSubject` and `filterSubjects`
are mutually exclusive; `optStartSeq` requires `deliverPolicy:
by_start_sequence` and `optStartTime` requires `by_start_time`, either one
under any other policy being rejected rather than ignored; and `backoff`'s
length is bounded by `maxDeliver` per §5(c).

A filter only ever narrows what a durable sees, including on a foreign
stream, so no filter field touches the permission model.

#### 2.3 What stays closed, and why the schema declares seven of them

Permanently closed, with the reason attached:

| field(s) | reason |
|---|---|
| `sources`, `mirror` | ADR 0061 §4.1: **measured** to defeat the read half of the deny vector |
| `republish`, `subjectTransform` | same class — re-emit a stream's traffic under a subject the application could not publish to |
| `deliverSubject`, `deliverGroup`, `flowControl`, `heartbeatInterval` | push delivery is performed by the server, outside the application's publish permissions: a write channel into a neighbour's prefix |
| `placement`, `replicas` | clustered topology, Tier 2+ |
| `sealed`, `preventDelete`, `preventUpdate`, `denyDelete`, `denyPurge` | platform-owned lifecycle; the last two are already expressed positively through the allow list |
| `account`, `creds`, `nkey`, `servers`, `tls`, `tlsFirst` | provisioner-owned connection plumbing |
| `jsDomain` | ADR 0061's design constraint: a domain shifts every pattern in §3 by one token and silently unmatches the whole deny vector |
| `pauseUntil`, `priorityGroups`, `priorityPolicy`, `pinnedTtl`, `allowMsgTtl`, `allowMsgCounter`, `allowAtomicPublish`, `allowMsgSchedules`, `allowBatched`, `persistMode`, `firstSequence`, `mirrorDirect`, `noAck`, `metadata` | deferred, not refused — the pinned server's support for each is unverified, and `allowMsgSchedules` needs the same foreign-subject analysis `republish` got before it could be considered |

**Seven of these — `sources`, `mirror`, `republish`, `subjectTransform`,
`deliverSubject`, `placement`, `replicas` — are declared in the CUE type in
order to be rejected by the webhook**, the pattern ADR 0061 §6 established
for `#JetStreamNeed.name` / `persistent`. The reason is unchanged and still
load-bearing: a structural schema *prunes* unknown fields before a
validating webhook runs, so a manifest setting `mirror:` would have it
vanish silently and appear to work. These seven are the ones a user
plausibly writes, because they are prominent in NATS's own documentation.
The remainder are provider plumbing nobody types into an application
manifest, and pruning them costs nothing.

#### 2.4 Dead-letter routing

NATS has no dead-letter queue. What it has is an advisory published when a
consumer exhausts `maxDeliver`, carrying the stream, the consumer, the
stream sequence and the delivery count — **metadata, not the message body**.
This decision makes that advisory addressable and says so plainly wherever
it is documented.

```yaml
consume:
  - from: feeder
    stream: blocks-head
    durable: indexer
    maxDeliver: 5
    ackWait: "30s"
    deadLetter: {stream: indexer-dlq, maxBytes: "128Mi", maxAge: "168h"}
```

**`deadLetter` materialises an ordinary declared stream owned by the
application** — it enters the provisioner's `ClaimView.streams` exactly as
an entry of `streams[]` does, so the allow list, the deny vector, the quota
pre-flight, the capture detector and the GC treat it identically, with no
new code path and no new permission. The only difference is that its
`subjects` are composed by the platform rather than written by the user: a
single, exact advisory subject for this `(composed stream, composed
durable)` pair, never a wildcard, so it can carry nothing but this
application's own failures. `retention` is fixed to `limits` and `storage`
to `file`: a DLQ is a log to be read, not a workqueue to be drained.

Reading it is an ordinary `consume` entry naming the DLQ stream. The
permission chain already exists end to end and is written out here because
each link is a separate grant: the stream is filled by the **server** (a
stream's subjects are not permission-checked against the application), the
application pulls with `$JS.API.CONSUMER.MSG.NEXT.<stream>.<durable>` and
acknowledges under `$JS.ACK.>` — both in the allow list — and the pulled
messages are delivered into its own inbox prefix, the one subtree besides
its own it may subscribe to. The application never subscribes to the
advisory subject directly, and could not.

Recovering the original body is `$JS.API.STREAM.MSG.GET` by `stream_seq`,
already granted, **if the message is still in the stream**. Under
`retention: workqueue` it may not be — §5(b) settles it, and the answer
becomes a documented limitation rather than a feature.

`deadLetter` without `maxDeliver > 0` on the same entry is **rejected**:
without a delivery ceiling the advisory never fires and the DLQ is
permanently empty, which is the exact failure class — a manifest that looks
like it works and says nothing — that the declare-to-reject pattern exists
for. `maxBytes` is required because the DLQ counts against the namespace
quota like any other declared stream.

No gate: the DLQ carries one pair's advisories, both ends of which belong to
the declaring application, including when the source stream is a
neighbour's.

## Consequences

- **Easier.** A workload can state its own health contract; a rolling update
  stops serving traffic from a pod that is not listening; a JetStream
  consumer's redelivery behaviour becomes a reviewable line in git instead
  of a client-side setting NACK reverts; a failed message has somewhere to
  go.
- **Harder.** The `Application` CRD grows a lot of optional surface, seven
  fields of it existing only to be rejected. The counterweight is that every
  one of those seven would be a security-relevant surprise if pruned
  silently.
- **Visible, and breaking.** Every existing Deployment rolls once on
  upgrade, and an application with a misdeclared `expose.port` goes NotReady
  for the first time. Both are intended; both will be reported at least
  once.
- **A stale CLI rejects a valid manifest locally.** The schema is compiled
  into `apprafter app validate` via `include_str!`, so a CLI older than the
  cluster refuses the new fields. Unchanged property, restated so it is not
  rediscovered.

## Alternatives considered

- **Mirroring the Kubernetes probe shape** (`httpGet` / `tcpSocket` /
  `exec` / `grpc` / `tcpSocket`). Rejected: four of the five action kinds are
  out of scope, and the nesting exists to discriminate between them.
- **Duration strings for probe timings**, for consistency with `maxAge`.
  Rejected: it would make `"500ms"` writable and unimplementable.
- **CRD defaults for the probe timings.** Not available: §1.2's rule is
  enforced by `crdgen`, so choosing this would mean changing a
  repository-wide invariant for one field set. The freezing behaviour
  described there is why that invariant exists.
- **No default readiness probe** — probes strictly opt-in, as first
  proposed. Rejected: the 502 window is real, closing it guesses nothing
  (the port is declared), and an opt-out field costs one line.
- **Deriving the startup probe's cadence from the liveness probe's.**
  Rejected: the two answer opposite questions.
- **Exposing the full NACK field set** and gating the dangerous fields
  behind a MigrationPlan instead of refusing them. Rejected: each such field
  would need its own detector and its own proof that the gate cannot be
  bypassed, to buy capabilities ADR 0061 §4 measured as isolation-breaking.
- **An application-side dead-letter convention only** — the application
  republishes to `<app>.dlq.>` after N attempts, with an ordinary declared
  stream collecting it. Not rejected: it keeps the message body and stays
  available, and the documentation carries it as the recipe for when the
  body matters. It is not a substitute, because retries are then the
  application's business and the manifest says nothing about them.

## Risks

- **The fleet-wide rollout on upgrade.** Mitigated by making it exactly one
  rollout, asserting convergence on the walk, and naming it as breaking. We
  accept the second-order effect: a misdeclared port surfacing as NotReady.
- **The 2-second timeout is unmeasured.** §5(d) settles it; until then it is
  recorded as a choice, not a finding.
- **The DLQ carries metadata, not bodies, and users will expect bodies.**
  Mitigated by documentation that leads with the limitation, and by §5(b)
  deciding whether body recovery can be promised at all under `workqueue`.
- **A deferred NACK field is read as a refused one.** Mitigated by the last
  row of §2.3 being explicitly "deferred, not refused" with the reason.

## Owner

Andrey Ryahovskiy.

## Re-evaluation

- Revisit §1.2's timeout default once the Tier-1 measurement in §5(d) lands.
- Revisit §2.3's deferred row when the pinned nats-server moves: several of
  those fields exist in the NACK CRD ahead of the server we run.
- Revisit §2.4 if NATS grows a first-class dead-letter mechanism, at which
  point the advisory composition becomes an implementation detail to
  replace rather than a contract.

## 5. Measurements owed before implementation

Each is a claim this record depends on that no gate in this repository
would catch if it were wrong. Recorded under `docs/measurements/`.

- **(a)** The exact advisory subject for exhausted `maxDeliver` and its
  payload fields, on nats-server v2.14.3. §2.4's DLQ subject is composed
  from it.
- **(b)** Whether a message that exhausts `maxDeliver` under `retention:
  workqueue` remains fetchable by `STREAM.MSG.GET` at the advised sequence.
- **(c)** The server's actual constraint between `backoff` length and
  `maxDeliver`, before it is written as a webhook rule.
- **(d)** Whether a 1-second probe timeout produces false failures on a
  Tier-1 node under load.
- **(e)** Which of `compression`, `consumerLimits`, `discardPerSubject` and
  `allowDirect` the pinned server honours. The NACK CRD carrying a field
  does not mean the server implements it, and a field that silently does
  nothing is worse than an absent one.

## References

- [ADR 0061](0061-needs-jetstream-nats.md) — `needs.jetstream` → NATS; §6 is
  the contract this amends, §3/§4 the permission model it does not touch.
- [ADR 0052](0052-migration-security-axis.md) — the security axis; triggers
  #14/#16 are the shape §2.1 reuses for `allowRollup`.
- [ADR 0051](0051-app-scope-migration.md) — the destructive axis; §1.6
  records that probes sit on neither.
- [ADR 0053](0053-resource-governance.md) — "no pod is BestEffort"; §1.3 is
  its sibling property, "no exposed pod is unchecked".
- [ADR 0047](0047-crd-codegen-from-cue.md) — why the CRD is generated from
  the CUE and what the drift gate covers.
- `nack` 0.35.0 (`platform-stack/cue/component_nack.cue`) — the provider
  field sets in §2.
