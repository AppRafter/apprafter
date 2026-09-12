---
description: "Give an application a JetStream account by declaring it in the manifest: how to declare it, declare a stream, consume a neighbour's, connect correctly, and remove it safely."
---

# JetStream from a declared dependency

An application declares that it needs JetStream, and the platform provisions a
NATS account for it, publishes the connection details, and binds them to the
env-vars you name. Streams and durable consumers are declared in the same
manifest rather than created by hand. Retiring the application keeps the streams
for seven days before reclaiming them.

**The account belongs to the namespace, not to the application.** Every
application in one namespace shares it, and they are kept apart by the subjects
each is allowed to publish and subscribe to. Put applications that must not
reach each other's messages in different namespaces.

> **Declaring the dependency also opens the network path to it**, the same way
> it does for Postgres and for the cache — on a cluster running Cilium, a
> `needs.jetstream` application is allowed through to the message server on
> port 4222, and to nothing in-cluster it has not declared. See
> [Egress policy](egress-policy.md).

## Prerequisites

- A Tier-1 cluster provisioned with `apprafter up` (see the
  [Quickstart](quickstart.md)), operator **≥ v0.2.49** — the release that ships
  the NATS platform component, the `jetstream-integrated` provider, and the
  JetStream backend in the claim controllers.
- For the verification blocks below only, a kubeconfig:

  ```sh
  apprafter kubeconfig --refresh > /tmp/kc && export KUBECONFIG=/tmp/kc
  ```

## Declare the dependency

`apprafter app scaffold --needs` does not take `jetstream` today, so scaffold
the manifest and add the block by hand:

```sh
apprafter app scaffold --name feeder --namespace demo
```

Then edit `apprafter/Application.cue`:

```cue
spec: base: {
    // ... image / replicas / expose ...
    needs: {
        jetstream: { selector: { tier: "integrated" }, size: "small" }
    }
}
```

`size` is the storage the account may hold, one of `nano`, `small`, `medium`,
`large` and `xlarge`. It is a **namespace** budget: the sizes of every claim in
the namespace are added together and clamped by a per-namespace ceiling, so a
second application arriving is what makes the number bite rather than your own
declaration alone.

**Declaring the dependency does not inject anything into your container.** Bind
the env-vars you want:

```cue
spec: base: {
    // ... image / replicas / expose / needs ...
    env: {
        NATS_URL:            claim.jetstream.url
        NATS_SUBJECT_PREFIX: claim.jetstream.subjectPrefix
        NATS_INBOX_PREFIX:   claim.jetstream.inboxPrefix
    }
}
```

The env-var names are yours. `claim.jetstream.<field>` names a field of the
connection Secret the platform will publish — `url`, `host`, `port`, `user`,
`pass`, `account`, `subjectPrefix` and `inboxPrefix` are all available. `url`
already carries the user and password, so it is the only one most clients need
— but bind all three above, because the other two are not optional to use
correctly and the next section is why.

Check it before you push:

```sh
apprafter app validate
```

Use that rather than a bare `cue vet ./apprafter/...`: the scaffold does not
vendor the schema next to your manifest, so `cue` refuses the import outright.
`app validate` lays the bundled schema and the generated `claim` binding into a
temporary workspace first, which is what makes `claim.jetstream.*` resolve the
same way it will at sync time.

### Declare a stream

A stream is declared, not created at runtime:

```cue
needs: {
    jetstream: {
        selector: { tier: "integrated" }
        size:     "medium"
        streams: [
            {
                name:      "orders"
                subjects:  ["feeder.orders.>"]
                storage:   "file"
                retention: "workqueue"
                maxAge:    "24h"
                maxBytes:  "512Mi"
            },
        ]
    }
}
```

`maxBytes` is **required** — a stream without one silently claims the whole
account budget — and it is reserved out of the budget the namespace shares,
alongside every stream your neighbours declare. Ask for more than is left and
the claim reports `QuotaExceeded`, naming the stream and the numbers, before
anything is created.

`name` must be a DNS-1123 label (lowercase letters, digits and hyphens): the
platform composes it with your application's name into the identifier the access
rules are keyed on, and an underscore there would make that composition
ambiguous.

The platform creates the stream and holds the application back until it exists.
That wait is the point — an application that started first would be publishing
into nothing.

### Consume another application's stream

A durable consumer is declared by the application that **reads**, naming the
application that owns the stream:

```cue
needs: {
    jetstream: {
        selector: { tier: "integrated" }
        consume: [
            { from: "feeder", stream: "orders", durable: "indexer" },
        ]
    }
}
```

Omit `from` for a stream your own application declares. Naming **another**
application shares that stream between the two, so the first such entry for each
`(from, stream)` pair is held for approval — see
[Widening edits are held](#widening-edits-are-held) below.

The durable carries the consuming application's name, so two applications may
each declare a durable called `indexer` on one shared stream without silently
sharing a cursor.

### Collect subjects from several producers

A declared stream may name subjects **outside** your application's own prefix,
which is how you build a shared work queue several producers write into. Each
producer still publishes only under its own prefix; it is the collecting
application's permissions that widen, which is why the declaration belongs in
the collector's manifest and not in theirs.

```cue
streams: [
    {
        name:     "orders"
        subjects: ["shop.orders.>", "billing.orders.>"]   // other applications' prefixes
        maxBytes: "512Mi"
    },
]
```

Adding a foreign subject is held for approval.

## Three rules for using the connection

This is the whole of what an application author needs to know, and the second
one is the one that bites.

**Subjects: you must prefix them.** Your application may publish only under
`claim.jetstream.subjectPrefix` — the value is your application's name and a
dot. This is enforced, not advisory: a publish outside the prefix is refused.
Subscribing is narrower still — that same prefix and your own reply inbox, and
nothing else. Reading a stream you do not own works through the durable you
declared, which delivers into that inbox, which is the next rule's business.

### Set the inbox prefix, it is not optional

**Give your client `claim.jetstream.inboxPrefix` as its inbox prefix.** Most
NATS clients call this a custom inbox prefix or a reply prefix; every client
library has the setting, and the platform cannot set it for you.

The shared reply tree every NATS client uses by default carries the replies of
*every* application in the account, so no application is allowed to subscribe to
it. Each claim gets a private one instead.

**Leaving it unset costs the acknowledgement, never the write — which is worse
than a refusal.** A client without it connects perfectly well and looks healthy.
What it loses is every reply: the request is published and processed normally,
and only the answer is refused. Measured on a live cluster, a `nats stream add`
from such a client returned `context deadline exceeded` after **10.2 seconds**
— **and the stream was created.** Nothing in that message mentions a permission.

The natural response to a timeout is to retry, and the retry creates a second
stream. So if a client of yours is timing out on operations that appear not to
happen, check the inbox prefix before you check anything else, and look at what
actually exists before you retry.

### Match your client to the acknowledgement format

The server is configured for the current (v2) acknowledgement and flow-control
subjects. A client library that only understands the older form still delivers
messages, but reads message metadata — stream sequence, delivery count,
timestamps — off the wrong positions and returns wrong values silently.

`nats.js` and the Go client both handle the current form. If you are on another
language, confirm your client does before you rely on message metadata.

## Register the application

```sh
apprafter app add https://github.com/your-org/feeder.git \
  --name feeder --namespace demo --project apps
```

If the repository is private, register the credential first:

```sh
apprafter repo creds add feeder-creds \
  --url-prefix https://github.com/your-org \
  --type pat --token "$YOUR_PAT"
apprafter repo creds list
```

## Watch it come up

```sh
apprafter app status feeder
```

The first `needs.jetstream` in a fresh cluster is the slow one: the message
server is not running until something asks for it, so the first claim starts it
and later claims join the account that already exists. While that runs,
`app status` reports the claim as not ready and the application as
`AwaitingResourceClaim` — the workload is held back rather than started against
a server that does not exist yet.

A claim that declares streams stays unready a little longer still, until those
streams are reported live. When it finishes you should see the application
`Ready`, and the claim with a provider, `ready`, and a connection Secret.
`apprafter app logs feeder` shows whether your application picked the connection
up.

??? note "Verify independently with kubectl"

    ```sh
    kubectl -n demo get resourceclaim.apprafter.io feeder-jetstream -o \
      jsonpath='provider={.status.provider} ready={.status.ready}{"\n"}'
    # -> provider=jetstream-integrated ready=true

    kubectl -n demo get application.apprafter.io feeder -o jsonpath='{.status.phase}{"\n"}'
    # -> Ready

    kubectl -n demo get secret feeder-jetstream-conn -o jsonpath='{.data.subjectPrefix}' | base64 -d; echo
    kubectl -n demo get secret feeder-jetstream-conn -o jsonpath='{.data.inboxPrefix}' | base64 -d; echo
    ```

    The claim also carries what the platform last observed in the account —
    which of your streams it can see, and which streams it cannot account for.
    The classification, and what it deliberately does not act on, are in
    [How it works](../how-it-works/needs-jetstream.md#what-the-platform-can-see-and-what-it-cannot).

## Widening edits are held

Three edits to a `needs.jetstream` block do not take effect on push. Each widens
what your application can reach, so the platform holds it: the application
pauses at `AwaitingMigrationApproval`, the previously-applied spec keeps
running, and nothing changes until a human approves it.

```sh
apprafter migration list
apprafter migration approve <plan-name>
```

- Adding a `consume` entry that names **another** application.
- Turning `dynamicStreams` on.
- Adding a declared subject outside your own prefix, or setting `allowPurge` on
  a stream that already collects one.

Taking any of them back is a narrowing and clears the gate instead of asking
again. The approval gate itself is covered on
[Migration plans](migration-plans.md).

**`dynamicStreams: true` is the one to think about before asking for it.** It
lets your application create streams at runtime, which sounds narrow and is not:
a stream can be created that copies another stream, and the server performs that
copy on the account's behalf without consulting the permissions of whoever asked
for it. So the flag means, in plain words, *this application can read every
stream in its namespace, and drain every work queue in it*. Leave it off and
declare your streams; if you need key-value buckets or object stores, those are
streams underneath and the flag is what enables them.

When one application in a namespace holds the flag, the owner of a work-queue
stream beside it is told so on its own claim. That is a report, not a fault.

## Per-environment overrides replace the whole block

An environment override replaces a `needs` entry wholesale rather than merging
into it — the platform-wide rule, described in
[Per-environment deploy](../how-it-works/per-environment-deploy.md#how-an-override-merges-onto-the-base).
For jetstream that is sharper than elsewhere, because the block holds your
stream and consumer contract as well as the size:

```cue
environments: prod: needs: jetstream: { size: "large" }   // refused: drops streams AND consume
```

That edit is **refused at admission**, not applied quietly. The message names
the streams and the consumers the override was about to drop:

```text
prod: needs.jetstream omits "streams" while base declares 2 of them
("orders", "events") — a per-environment override REPLACES the whole
needs.jetstream block, so this environment would lose them entirely.
Repeat them here, or write "streams: []" to state that this environment
deliberately has none.
```

So there are three ways forward, and each says what you mean: repeat the whole
block in the override, leave the size alone, or write `streams: []` /
`consume: []` when the environment really is meant to have none.

## Remove the dependency

**Dropping the `needs.jetstream` block from the manifest does not take effect on
push.** Removing a declared dependency is classified as a destructive change, so
the platform holds it the same way: the application pauses at
`AwaitingMigrationApproval` and nothing is torn down until you approve the
change.

Remove the `env` bindings in the same edit — `claim.jetstream.*` is generated
from the `needs` block, so a binding left behind fails `apprafter app validate`.

**To retire the application and start the retention clock, delete it:**

```sh
apprafter app remove feeder
```

That deletes the Argo CD Application, which prunes the AppRafter `Application`
CR and the claim with it. The connection Secret goes; **the streams and their
messages are kept for seven days**, then reclaimed automatically. A neighbour's
streams in the same account are untouched, and the account itself survives until
its last claim goes.

That grace window is the point: retiring an application by accident, or on a
branch, does not destroy messages. There is no supported way to shorten it, and
the retention record is deliberately not hand-editable — it is the work order
the reclaim executes, so a hand-written substitute is a way to delete the wrong
streams.

`--keep-data` does something different, despite the name: it strips the cascade
first, so **only** the Argo CD object is deleted and the workload, its
`Application` CR and its claim all keep running. Use it to hand an application
off or re-register it elsewhere, not to retire one.

??? note "Verify independently with kubectl"

    Right after `app remove` — the snapshot exists and the connection Secret
    is gone, while the streams are kept:

    ```sh
    kubectl -n apprafter-system get retainedclaim claim-demo-feeder-jetstream -o \
      jsonpath='until={.spec.retainUntil}{"\n"}'
    # -> until=<RFC3339, ~7 days out>

    kubectl -n demo get secret feeder-jetstream-conn        # -> NotFound (cascaded)
    ```

    Confirming the reclaim itself needs the namespace's management credential
    and an unrestricted client against an account that holds other
    applications' streams, so the guide does not walk it. What the reclaim
    sweeps, and the one rule in it that is a heuristic rather than a guarantee,
    are in
    [How it works](../how-it-works/needs-jetstream.md#the-grace-window-and-the-reclaim).

## How it works

[Declared JetStream dependencies](../how-it-works/needs-jetstream.md) covers why
the account belongs to the namespace, what the subject prefix actually enforces,
what the platform can and cannot detect inside one account, how object names are
derived, and the reclaim after the grace window.

## Troubleshooting

| Symptom | Likely cause | Fix |
| ------- | ------------ | --- |
| The application connects and gets its credentials, but every operation times out from inside the pod | on a Cilium cluster, the egress rule for the message server is missing from the application's policy | Read it back: `kubectl get ciliumnetworkpolicy <application>-egress -n <namespace> -o yaml` should carry a rule for namespace `nats-system` on port 4222. If it does not, the operator is older than the release that added it — see [Egress policy](egress-policy.md). |
| A client times out after about ten seconds on operations that turn out to have happened anyway | the inbox prefix is not set | Set it from `claim.jetstream.inboxPrefix`, and check what already exists before retrying — a retry is what creates duplicates. See *Set the inbox prefix* above. |
| The application stays at `AwaitingResourceClaim` and the claim never gets a provider | the `needs.jetstream.selector` matches no provider | Confirm the selector reads `tier=integrated`: `kubectl get serviceprovider jetstream-integrated -n apprafter-system -o yaml`. |
| The claim stays unready with `AwaitingStreamCreation` | a declared stream has not been created yet, or could not be | Give it a few cycles, then read the claim's `Ready` condition message — it names the object that is not live. |
| The claim stays unready with `QuotaExceeded` | the namespace's declared streams add up to more than the account may hold | The message names the stream and the numbers. Lower a `maxBytes`, raise a `size`, or move the application to its own namespace. |
| The claim stays unready with `NatsMemoryBudgetExceeded` | the namespaces using JetStream jointly reserve more memory on the shared message server than it has — roughly three fit on a Solo node | The message names the reservation and the budget. Nothing in this application's own declaration will change it: drop JetStream from a namespace that no longer needs it, or put these applications in a namespace that already has an account. Namespaces already on the server are unaffected and keep running. |
| The manifest is rejected on sync | `persistent` or `name` on the jetstream need, a stream without `maxBytes` or without subjects, a name that is not a DNS-1123 label, or a durable whose name collides with one of your own streams | Each is refused by name in the error. `persistent` does not apply here — durability is a property of each stream's `storage`, not of the claim. |
| The application's env-vars are empty or missing | the manifest declares `needs.jetstream` but binds nothing — nothing is injected automatically | Add the bindings, as above. |
| An override is rejected for omitting `streams` or `consume` | an environment override replaces the whole `needs.jetstream` block, and the omission would drop what `base` declares | Repeat the block in the override, or write `streams: []` / `consume: []` if the environment is meant to have none. See *Per-environment overrides* above. |
| A stream the platform reports as unattributed appeared under your prefix | a neighbour holding `dynamicStreams` created it — this cannot be prevented, only reported | Find out which application in the namespace holds the flag and why it was approved. The platform does not delete the stream. |
| Message metadata (sequence numbers, delivery counts) is wrong but messages arrive | the client library reads the older acknowledgement format | Use a client that handles the current form — see *Match your client* above. |

## Cleanup

```sh
apprafter app remove feeder   # the streams enter their seven-day window
apprafter destroy --yes       # every apprafter=true resource in the token's project
```
