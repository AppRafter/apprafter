---
description: "How a declared needs.jetstream dependency becomes a NATS account: one account per namespace, the subject prefix that isolates applications inside it, what dynamicStreams grants, and the reclaim after the grace window."
---

# Declared JetStream dependencies

The recipe is [JetStream](../operator-guide/jetstream.md). This page is what
happens behind it — the tenancy model, what actually keeps two applications in
one namespace apart, and the shape of the teardown. None of it is needed to use
`needs.jetstream`.

The backend is [NATS](https://nats.io/) with JetStream enabled, run as a
platform component that is **off until the first claim asks for it**. The
design, the measurements behind the permission model, and the alternatives that
were rejected are in [ADR 0061](../adr/0061-needs-jetstream-nats.md).

## The chain

You declare `needs: { jetstream: {} }` on an Application.

1. The operator generates a `ResourceClaim` and **pauses** the Application at
   `status.phase=AwaitingResourceClaim`. Nothing is deployed against a message
   server that does not exist yet.
2. The scheduler matches the claim to the seeded `jetstream-integrated`
   `ServiceProvider` and marks it `Scheduled=True`.
3. The provisioner writes the account file the server will read, **then** turns
   the NATS component on, waits for the server, and **verifies by connecting as
   the new user** rather than by waiting a fixed time. The order is not
   cosmetic: the server pulls the account file in by reference, and a pod
   mounting a file that is not there yet never starts.
4. Streams and durable consumers the manifest declares are applied as their own
   resources, and the claim stays unready — `AwaitingStreamCreation` — until
   they are reported live. A claim that went ready first would be telling an
   application its stream exists when it does not.
5. It publishes a connection `Secret` carrying `url`, `host`, `port`, `user`,
   `pass`, `account`, `subjectPrefix` and `inboxPrefix`, and marks the claim
   `ready=true`.
6. The Application resumes to `Ready`, and the rendered Deployment carries a
   `secretKeyRef` for every claim field the manifest referenced.

Nothing is injected into your container automatically ([ADR
0046](../adr/0046-env-value-references.md)) — an env-var exists because the
manifest bound one.

## The account is the namespace

Every application in one namespace shares **one** NATS account, named after the
namespace. The account owns a subject space, a JetStream store, and the storage
quota; the quota is the sum of that namespace's live claims' `size`, clamped by
a per-namespace ceiling.

**Applications inside the account are separated by subject permissions, not by
accounts of their own.** That is a deliberate trade rather than an oversight.
An account per application would be the harder boundary — but two applications
sharing a stream would then need cross-account exports and imports of a mapped
slice of the JetStream API, which is a much larger construct than the sharing it
buys. Account-per-application is kept as the escape hatch for a tenant that
needs real isolation, not as the launch model.

The consequence to hold on to: **the namespace is the trust boundary.** Put
applications that must not reach each other's messages in different namespaces.

Accounts live in a configuration file the provisioner is the sole writer of,
re-derived **whole** from the live set of claims on every change — one
application's permissions depend on what its neighbours declared, so any change
anywhere invalidates the rest. The server picks the new file up without
restarting, and connections that were open across the rewrite stay open.

## The prefix, and the one sanctioned exception

Each application owns the subject prefix `<app>.` and may publish only there.
This is enforced by the server, not by convention: a publish outside the prefix
is refused.

**A declared stream is the exception.** A stream declared in the manifest may
collect subjects *outside* its owner's prefix, which is what makes fan-in
possible — several producers writing under their own prefixes into one
collecting stream. Producers are unaffected; only the collector's permissions
change, which is why the declaration sits in the collector's manifest rather
than in theirs.

That reversal is only safe because **a declared stream cannot be altered from
the application side**. Otherwise an application could declare an innocent
subject set, pass the approval gate, and add a neighbour's prefix afterwards
with one update. The two decisions stand or fall together — and adding a
foreign subject, or `allowPurge` on a stream that already carries one, is
[gated for approval](the-approval-gate.md).

## What `dynamicStreams: true` grants

By default an application may create consumers but not streams. The flag adds
the create, update and delete verbs for streams — and that is a wider grant
than it reads as.

A stream can be created with another stream as its *source*. The server
performs that copy **in the account's context**: the creating user's own
permissions are never consulted. So an application that can create a stream can
read any stream in its namespace, whatever the subject rules say. Against a
`workqueue` origin the copy is worse than a read — a workqueue drops a message
once it is acknowledged, so the copy **drains** the original.

In plain words, `dynamicStreams: true` means *this application can read every
stream in its namespace and drain every workqueue in it*. That is why the
default is `false`, why turning it on is classified a security-boundary change
and [held for approval](the-approval-gate.md), and why the owner of a workqueue
stream is told — `NamespaceDrainRisk` — when a neighbour holds the flag.

Two smaller consequences:

- Key-value buckets and object stores are streams underneath, so the default
  mode disables them too.
- Turning the flag back **off** does not undo a capture that already happened.
  The stream that was created keeps existing; the reclaim described below is
  what eventually removes it.

## What the platform can see, and what it cannot

**A stream's subjects are not checked when it is created.** They travel in the
request body, which the server never inspects while deciding permissions. So a
stream created over a neighbour's prefix cannot be *prevented* — only observed.
Detection exists because prevention is not available.

On each 60-second resync the provisioner lists the account's streams and writes
what it observed onto the claim, classified three ways:

- **declared** — this application's own declared streams, seen live;
- **dynamic** — undeclared streams whose subjects lie wholly under `<app>.`;
- **unattributed** — streams touching `<app>.` that no declaration in the
  namespace accounts for, plus streams that carry no subjects at all.

`apprafter app status` renders the bytes those streams hold. Unattributed bytes
are deliberately left out of that figure: they belong to nobody the platform can
name, and charging them to whichever claim shares a prefix would make one
tenant's number move when another's stream grew.

An unattributed stream raises `ForeignSubjectCapture` on the **victim's** claim,
with an Event beside it, and the shipped policy **reports without deleting**.
Two limits are worth knowing:

- A stream built from a *source* carries no subjects of its own, so it touches
  no prefix by any reading and is never flagged as a capture. It is listed as
  unattributed, which is the whole difference between reporting and acting — a
  detector that deleted on no evidence would be a data-loss primitive.
- For an application that holds `dynamicStreams: true`, its own dynamic streams
  are indistinguishable from a capture. The approval gate on turning the flag on
  is the entire control there.

One condition looks backwards until you see what it covers. A declared subject
naming an application that does not exist yet passes the gate, because narrowing
the check to applications that are live would let someone position a stream
ahead of an arrival. When that application finally arrives, `PrefixPreCaptured`
on its claim tells it that its prefix is already inside somebody else's stream.

## The inbox is per application

Inside one account, the default reply-inbox tree carries every reply to every
application's requests — API responses, and the message payloads an application
fetches through a pull consumer. A neighbour would not need to defeat anything
to read another application's data; it would read it in flight.

So the shared tree is in nobody's subscribe list, and each claim is given an
inbox prefix of its own in its connection Secret. A client that does not use it
connects perfectly well and then loses every reply — a failure that reads as a
timeout rather than as a refusal, and whose cost is described where a reader
meets it, in
[Set the inbox prefix](../operator-guide/jetstream.md#set-the-inbox-prefix-it-is-not-optional).

## The names are derived, not chosen

For an app `web` in namespace `demo`, declaring a stream `orders` and a durable
`reader`:

| Object | Name |
| --- | --- |
| `ResourceClaim` | `web-jetstream` |
| Account | `ns_demo` |
| Claim user | `claim_demo_web_jetstream` |
| Subject prefix | `web.` |
| Inbox prefix | `_INBOX_demo_web` |
| Stream, on the server | `web_orders` |
| Durable, on the server | `web_reader` |
| Connection `Secret` | `web-jetstream-conn` |
| `RetainedClaim` snapshot | `claim-demo-web-jetstream` |

**The underscore is load-bearing.** Declared stream and durable names must be
DNS-1123 labels, whose alphabet excludes `_`, so the join can never be read two
ways — which matters because the permission rules key on the composed name, and
two applications handed the same one would decide each other's access. It is
also why a durable carries the *consuming* application's name: two applications
may each declare a durable called `reader` on one shared stream without silently
sharing a delivery cursor.

## Watching it happen

```sh
# 1 — a claim was generated, and the Application is gated on it
kubectl -n demo get resourceclaim.apprafter.io web-jetstream \
  -o jsonpath='type={.spec.type} dynamic={.spec.jetstream.dynamicStreams}{"\n"}'
kubectl -n demo get application.apprafter.io web \
  -o jsonpath='{.status.phase}{"\n"}'          # -> AwaitingResourceClaim, then Ready

# 2 — the scheduler picked a provider, and the claim provisioned
kubectl -n demo get resourceclaim.apprafter.io web-jetstream \
  -o jsonpath='provider={.status.provider} ready={.status.ready}{"\n"}'
# -> provider=jetstream-integrated ready=true

# 3 — the account, the prefix and the inbox the claim was given.
#     These live in the connection Secret, not on the claim
for f in account subjectPrefix inboxPrefix; do
  kubectl -n demo get secret web-jetstream-conn -o jsonpath="{.data.$f}" | base64 -d; echo
done
# -> ns_demo / web. / _INBOX_demo_web

# 4 — what the platform last observed in the account
kubectl -n demo get resourceclaim.apprafter.io web-jetstream \
  -o jsonpath='{.status.streams}{"\n"}'

# 5 — the conditions, including the ones that are reports rather than faults
kubectl -n demo get resourceclaim.apprafter.io web-jetstream \
  -o jsonpath='{range .status.conditions[*]}{.type}={.status} {.reason}{"\n"}{end}'
```

The server itself runs in `nats-system`, and so do the stream and consumer
objects the provisioner applies. Only the connection `Secret` lands in the
application's namespace, owned by the `ResourceClaim`, which is what makes it
cascade away when the claim goes.

## The grace window, and the reclaim

**Deleting the Application deletes the claim** — the `ResourceClaim` carries a
controlling `ownerReference` back to it — and a finalizer writes an immutable
`RetainedClaim` snapshot with `retainUntil` at deletion + 7 days. The connection
Secret cascades away; the streams and their contents survive the window.

The stream and consumer objects live in `nats-system` while the claim lives in
the application's namespace, and **Kubernetes forbids a cross-namespace
`ownerReference`** — so nothing cascades to them and the provisioner deletes them
explicitly, the same pattern `needs.disk` uses for its standalone volume.

Once `retainUntil` passes, the reclaim runs as the namespace's management
identity: the application's declared streams and their objects go, its user is
dropped, the account file is re-derived without it, and the connection Secret is
deleted. When the namespace's last claim goes, the account goes with its store.

**Dynamic streams are swept by subject, never by name.** A stream whose subjects
lie wholly under the departing application's prefix is removed with it;
mixed-subject streams are left in place and go on being reported as
unattributed. That rule is close to an invariant for a producer, because
publishing is permission-enforced — but it is **a heuristic, not an invariant**,
and it is wrong in both directions: a capture stream over a neighbour's subjects
is missed, and a capture stream over the departing application's prefix is swept
along with it.

Editing the manifest is a different path. Dropping a `needs.<type>` key is
classified a destructive change, so it is gated behind a MigrationPlan and the
Application pauses at `AwaitingMigrationApproval`. The retention path runs on
Application deletion, not on a manifest edit.

## Limits in force today

Three properties of this design are not yet delivered, and each is something a
reader would otherwise assume from the pages around it.

- **A declared dependency carries no egress rule for the message server.** A
  declaration normally also opens the path to the thing declared
  ([Egress policy](egress-policy.md)) — a `needs.pg` application is allowed
  through to Postgres, a `needs.redis` one to the cache. `needs.jetstream` is
  given credentials and no route, so on a cluster running the default network
  profile an application's own pod is refused the connection its claim was
  given. The guide says so where a reader meets it.
- **A per-environment override replaces the whole `needs.jetstream` block**, as
  it does for every other dependency
  ([Per-environment deploy](per-environment-deploy.md)). For jetstream that is
  sharper than elsewhere, because the block holds the stream and consumer
  contract as well as the size: an override setting one field drops the rest.
- **The storage ceiling is per namespace, not per cluster.** Two namespaces each
  sized near the ceiling can jointly promise more than the single shared volume
  behind them holds. Nothing refuses that today; it surfaces as an allocation
  failure in whichever tenant asks last.

## For contributors

`e2e/needs-jetstream-walk.sh` exercises the whole chain on a local cluster,
including the parts no unit test can reach because each is a claim about what a
real server does: that the account file reaches the running server and is
reloaded without a restart, that a connection open across that rewrite survives
it, that a neighbour is refused another application's subjects, that a capture
is detected on the victim's claim and left in place, and that a departed
application's streams are reclaimed while its neighbour's are not.

Two things about that walk are worth knowing before trusting a green run. It
substitutes a hand-applied render of the NATS charts for the Argo CD sync of a
component that is not published yet, and it says so at the substitution's own
call site. And it runs without Cilium, which is why the egress gap named above
could not have turned it red.
