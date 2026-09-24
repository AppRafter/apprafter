---
description: "Why AppRafter never picks a server type for you, how much a 4 GB machine holds, how to read the live machine catalogue, the ways to supply a type and which one wins, and why changing the machine of a running cluster is a rebuild."
---

# Choosing the machine

A single-node cluster is one machine, and AppRafter never chooses it for
you. There is **no built-in default server type**: a run that is about to
create a machine with no type chosen stops and says so, before it creates
anything at all. Provisioning is where the spending starts, so the
machine is yours to name.

This page covers how much memory the machine needs, how to choose a type,
the three ways to supply one, what the error means when you supply none,
what happens to a cluster created before the default was removed, and why
the machine of a running cluster cannot be changed in place — with what to
do instead.


> There is no default machine type. Naming none is an error, not a
> fallback — provisioning is a spending decision and the platform will not
> make it for you. A cluster that predates this rule needs no action; see
> [a cluster created before the default was removed](#older-clusters).

## Two constraints before you open the catalogue

### The machine must be x86 {#x86-only}

Every AppRafter platform image — the operator, the admission webhook, the
configuration plugin Argo CD runs, the backup runner — is published for
`linux/amd64` only. There is no arm64 build today.

The catalogue the picker shows is the provider's whole catalogue, so it
includes arm machines, and nothing in the picker stops you selecting one.
A cluster on an arm machine will not run the platform. **Choose an x86
machine** — the picker's filter box takes `arch:x86`, and the `arch`
column shows what each row is.

### The machine's disk is the cluster's storage

On the single-node path there is no separate storage service: the node's
own disk holds the container images, the shared Postgres and Redis data,
and every volume an application claims — Tier 1 binds those through the
in-cluster `local-path` provisioner (see [Persistent
disk](persistent-disk.md)). The same node also runs the Kubernetes
control plane alongside your workloads, and
[`apprafter node prep`](node-prep.md) reserves memory headroom for it.

So read the `cores/ram/disk` figures twice over: the disk is everything
the cluster will ever store, and the RAM covers the control plane, the
platform's own components and your applications together.

## How much memory {#how-much-memory}

On a single-node cluster the node's memory is shared by its control plane, the
platform's own components, the backends your applications declare, and the
applications themselves. Kubernetes places a pod only if the memory it
*requests* still fits in what the node has left, so the figure to size by is
what everything requests, not what it happens to use.

A machine with **4 GB of RAM** holds, once [node
preparation](node-prep.md) has reserved the control plane's share:

- the platform's own components;
- the shared PostgreSQL behind `needs.pg`, and one Dragonfly instance behind a
  persistent `needs.redis`;
- the nightly off-site backup, while it runs;
- about four small applications at the platform's default request.

That is its limit. More applications, an application whose request its
recommendation has raised, or a second environment of one leaves no room for
the backup: the applications keep running, but the nightly backup cannot
start. `apprafter backup status` then shows its Job as `Pending, cannot be
scheduled`, and `apprafter top` shows how much memory the node has left to
give in its `SCHEDULABLE` column. [The backup runner's pod cannot be
scheduled](backup-restore.md#runner-unschedulable) is the recipe for that
state.

When room is short, the backup is what gives way, never the platform or your
applications: every other pod waiting for room is placed before it, and a pod
that needs the room a running backup holds stops that backup, which runs again
once there is room.

A further backend instance does not fit on the node at all, whether or not a
backup is running. An ephemeral `needs.redis` class runs a second Dragonfly
instance and `needs.jetstream` a NATS server, and each asks for more memory
than the node has left once the shared PostgreSQL and the first Dragonfly
instance are placed. The backend's pod stays `Pending`, and the application
that declared it waits at `AwaitingResourceClaim` without starting. [Node
reservations and
swap](../how-it-works/node-reservations-and-swap.md#what-a-4-gb-node-holds)
shows the arithmetic for both.

If you expect to run more than that, choose a machine with more RAM from the
start. Moving a running cluster to a bigger machine is a rebuild from a
backup ([Moving to a bigger machine](moving-to-a-bigger-machine.md)).

## Reading the catalogue

Which machine types exist, which regions offer them, what they cost and
what is in stock right now are the provider's facts, and they change
without any release of AppRafter. **This guide deliberately names no
machine type**: the list to choose from is the one the picker prints when
you run it, not one written down here months earlier.

Open it on a target that has not provisioned yet:

```sh
apprafter target machine
```

The command asks how to sort first — nearest first (the default when
latency could be measured), cheapest first, most cores, most RAM, most
disk, or by location — and then prints the matrix under this legend:

```text
  location  latency  sku      cores/ram/disk   arch  EUR/mo(net,excl.VAT)  [*]recommended [!]retiring
```

Every row below it is one **region × machine type** offer, in that column
order. Availability and price are per region, which is why a row carries
both: selecting one writes the region *and* the machine type onto the
target in a single step, so you cannot assemble a pair the provider does
not sell.

| Column | What it is |
| --- | --- |
| `location` | The provider region the offer is in. |
| `latency` | Round-trip time measured from **your machine** at the moment you opened the picker, not from your users. Best-effort — `n/a` when it could not be measured. |
| `sku` | The provider's name for the machine type. This is the value `--server-type` takes. |
| `cores/ram/disk` | vCPU count, RAM in GB, disk in GB. |
| `arch` | `x86` or `arm`. [Choose `x86`](#x86-only). |
| `EUR/mo` | The monthly price for that type in that region, net of VAT. |

Two badges appear at the end of a row. `*` marks an offer the provider
recommends. `!` marks one with a retirement date already announced: it is
still orderable, and picking it is a decision to move again later.

Type in the box to narrow the matrix. Tokens combine with AND:

| Token | Matches |
| --- | --- |
| `cpu>=4`, `cores=2` | vCPU count. `>=`, `>`, `<=`, `<`, `=` all work. |
| `ram>=16` | RAM in GB, same operators. |
| `disk>=160` | Disk in GB, same operators. |
| `arch:x86`, `arch:arm` | Architecture, exact match. |
| `cpu:shared`, `cpu:dedicated` | Whether the vCPUs are shared with other tenants. |
| `loc:hel`, `sku:cp` | Substring of the region or of the type name. |
| anything else | Substring of the region, type, architecture and CPU class together. |

Price and latency are sort axes, not filter tokens — a `price<10` token
is read as free text and matches nothing.

Rows the provider currently has no capacity for are hidden. When there
are any, a `-- show sold-out --` entry sits at the bottom of the list;
choosing it redraws the matrix with them included, and selecting one
tells you it is out of capacity and asks again. Being sold out is
temporary — the same row may be selectable an hour later.

`Ctrl-C` leaves the picker without writing anything.

## The three ways to supply a type

### 1. Save it on the target — the usual answer

The picker above writes its selection onto the active target, and every
later `apprafter apply` or `apprafter up` reads it from there.
Use `--target <name>` to set it on a target other than the active one:

```sh
apprafter target machine --target <name>
```

You can also supply it when you first register the target, which skips
the picker (the registration wizard asks for the rest — the API token,
the SSH key, the region):

```sh
apprafter target add <name> --provider hetzner-cloud --server-type <sku>
```

On a terminal the registration wizard shows the same matrix, so the type
is normally chosen there. A scripted run (`--no-interactive`, or any
non-terminal shell) that names no type does **not** fail: the target is
saved without one and the failure arrives later, at provisioning time.

### 2. Pass it per command — CI and one-offs

```sh
apprafter apply --server-type <sku>
apprafter up --server-type <sku>
apprafter restore <repo> --reprovision --server-type <sku>
```

The same value can come from the environment instead, which suits a
runner with no saved target store:

```sh
APPRAFTER_SERVER_TYPE=<sku> apprafter up
```

The environment variable is deliberately the **weakest** source (see the
table below), so a variable left in a shell cannot quietly override a
committed manifest or an explicit flag.

### 3. Commit it to the Infrastructure manifest — declarative setups

If you keep an `Infrastructure` manifest and point `APPRAFTER_MANIFEST`
at it, the first node entry's `spec.nodes[0].type` is the machine type:

```cue
spec: nodes: [{
    role: "control-plane"
    // The provider's name for the machine — the picker's `sku` column.
    type:  "<sku>"
    count: 1
}]
```

The manifest outranks anything saved on the target, so a committed file
pins the machine even if someone changes the target's saved preference.

### Which one wins

The first source that has a value is used; there is nothing below the
last row.

| Order | Source | Printed as |
| --- | --- | --- |
| 1 | `--server-type` on the command | `--server-type flag` |
| 2 | `spec.nodes[0].type` in the manifest at `APPRAFTER_MANIFEST` | `manifest` |
| 3 | The machine AppRafter recorded when it provisioned this cluster — a fact, not a setting | `state` |
| 4 | The type saved on the target | `target` |
| 5 | `APPRAFTER_SERVER_TYPE` | `env` |
| — | **No default.** Nothing is assumed | — |

`apprafter apply` prints which one it used before it touches the
provider, so you never have to guess:

```text
  server type: <sku> (target)
  region: nbg1
```

To read what is saved without running anything, use `apprafter target
show` (or `apprafter whoami` for the short version) — both print a
`Server type` line, or `not set`.

### A type is checked against the live catalogue when you save it

`apprafter target add --server-type` and `apprafter target machine
--server-type` both call the provider and reject a type that is not
orderable, before saving it. The region they check against is the one the
command knows about at that moment: `--region` for `target add` (falling
back to the default region when you pass none) and the target's saved
region for `target machine`. A type sold only in another region is
rejected there — pass `--region` too, or pick the row in the picker,
which cannot produce a mismatched pair.

Passing `--no-ping` (or setting `APPRAFTER_NO_PING`) skips that call and
saves the value unchecked:

```sh
apprafter target machine --server-type <sku> --no-ping
```

```text
server type set to `<sku>` on target `<name>` — NOT validated (--no-ping)
```

Nothing is lost except the early warning: an unsellable type is rejected
again at provisioning time, when the machine was about to be created.
`--no-ping` on its own, with no `--server-type`, is an error rather than a
silent no-op — the picker needs the provider to draw the matrix.

## When you supply none {#no-type-selected}

A run that is about to create a machine and finds nothing in any of the
five rows above stops there. The check runs ahead of every other step, so
no SSH key, network, firewall or machine is left behind:

```text
Error: apprafter::provider::server_type_not_selected

  × no server type selected
  help: No server type selected. Choose one:
        • interactive: `apprafter target machine` (opens the machine picker)
        • non-interactive / CI: `--server-type <sku>` or
        `APPRAFTER_SERVER_TYPE`
        • declaratively: set `nodes[0].kind` in your Infrastructure manifest
```

Two things about that message are worth knowing. The manifest field it
names is written `type:` in the file — `spec.nodes[0].type`, as in the
example above. And the check fires **only when a machine is about to be
created**: `apprafter apply` against a cluster that already exists
reconciles the firewall, the network and the kubeconfig without needing a
type at all, which is why upgrading the CLI never breaks a running
cluster.

Before the failing run, `apply` says the same thing in one line:

```text
  server type: (not selected — will fail on provision; run `apprafter target machine`)
```

A type that *is* set but cannot be ordered fails differently, with
`apprafter::provider::server_type_unavailable` and one of four reasons —
the type does not exist, it is not offered in that region, it has been
retired, or it is temporarily out of stock. Each carries a list of live
alternatives. [Troubleshooting](troubleshooting.md) tells the four apart.

## A cluster created before the default was removed {#older-clusters}

**Nothing to do.** A cluster provisioned by an older release has no
recorded machine type, and the first `apprafter apply` after the upgrade
reads it back from the running machine and records it:

```text
  server type baseline established: <sku> (recorded from live server — run `apprafter target machine` to change)
```

That run creates no machine, so it needs no type from you; it adopts the
one you are already running. The region is adopted the same way when the
target has none saved. From then on the cluster is indistinguishable from
one provisioned after the change.

### "the running machine is … but AppRafter recorded …"

```text
warning: the running machine is `<live>` but AppRafter recorded `<recorded>` — it was changed outside AppRafter
```

This says the machine was resized in the provider's own console, not
through AppRafter. It is a warning, not a failure: the cluster keeps
running and `apply` finishes. AppRafter deliberately does not overwrite
what it recorded at provisioning time, so the line repeats on every
`apply` until you tell it the new machine is the intended one:

```sh
apprafter import --force
```

`import` rebuilds the local record from the live infrastructure, the new
machine included. It also clears the cached kubeconfig and Argo CD
password, which the next `apprafter kubeconfig` and `apprafter
argocd-password` fetch again.

## The machine of a running cluster cannot be changed {#changing-the-machine}

There is no in-place resize. `apprafter target machine` refuses outright
once the target has a provisioned cluster, rather than saving a
preference that would never take effect:

```text
Error: apprafter::cli::other

  × `<name>` already runs a provisioned cluster — its machine type cannot be
  │ changed in place. To move to a different machine, rebuild from a backup:
  │
  │     apprafter backup create
  │     apprafter restore --reprovision --server-type <sku>
  │
  │ (`target machine` only sets the type on a target that has NOT provisioned
  │ yet.)
```


The rebuild itself — the two project topologies it can take, the sequence for
each backup backend, what to verify afterwards and what the outage costs — is
[Moving to a bigger machine](moving-to-a-bigger-machine.md).

The region is the same story: a running machine cannot move between regions
either, and the move is the same rebuild.

## Related

- [Operator quickstart](quickstart.md) — registering a target and
  provisioning the cluster, of which the machine choice is one step.
- [Managing targets](target-store.md) — where the saved type lives
  on disk, alongside the region and the credentials.
- [The target store on disk](../how-it-works/the-target-store.md) — where the
  type you pick is recorded, and how the rungs of the resolution chain above
  are read at provision time.
- [Node preparation](node-prep.md) — the control-plane headroom and swap
  the machine you chose then gets configured with.
- [Back up a cluster](backup-restore.md) — the backup and replay both
  rebuild routes depend on.
- [Troubleshooting](troubleshooting.md) — every diagnostic code the CLI
  emits, including both server-type errors above.
- [`apprafter target machine`](../reference/cli/target.md), [`apprafter
  apply`](../reference/cli/apply.md) — the generated flag reference.
