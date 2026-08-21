---
description: "Why AppRafter never picks a server type for you, how to read the live machine catalogue, the ways to supply a type and which one wins, and why changing the machine of a running cluster is a rebuild."
---

# Choosing the machine

A single-node cluster is one machine, and AppRafter never chooses it for
you. There is **no built-in default server type**: a run that is about to
create a machine with no type chosen stops and says so, before it creates
anything at all. Provisioning is where the spending starts, so the
machine is yours to name.

This page covers how to choose a type, the three ways to supply one, what
the error means when you supply none, what happens to a cluster created
before the default was removed, and why the machine of a running cluster
cannot be changed in place — with what to do instead.

The design rationale is in [ADR 0056](../adr/0056-machine-picker.md).

> **This changed in `v0.2.43`.** Earlier releases fell back to one
> hard-coded machine type when you named none, so a cluster could be
> provisioned — and billed — on a machine nobody chose. That fallback is
> gone. Existing clusters are unaffected and need no action; see [a cluster
> created before the default was removed](#older-clusters).

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
later `apprafter apply` or `apprafter bootstrap-all` reads it from there.
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
apprafter bootstrap-all --server-type <sku>
apprafter restore <repo> --reprovision --server-type <sku>
```

The same value can come from the environment instead, which suits a
runner with no saved target store:

```sh
APPRAFTER_SERVER_TYPE=<sku> apprafter bootstrap-all
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

The reason is that a different machine **is** a different machine: the
cluster is rebuilt on new hardware and its data is replayed into it. The
same is true of the region — a running machine cannot move between
regions either.

Both routes below need a backup that lives somewhere other than the
cluster you are about to take apart, and both have downtime. Read
[Backup and restore](backup-restore.md) first — it covers what a backup
captures, the passphrase you must keep, and the extra `--credential-file`
an `s3:` repository needs.

!!! danger "`apprafter destroy` empties a provider project, not a cluster"

    Both routes run `apprafter destroy`, and it is wider than its name
    suggests. It deletes **every** resource labelled `apprafter=true` in
    the Hetzner project the token belongs to — servers, floating IPs,
    firewalls, networks and SSH keys — and it never looks at a cluster
    name. `--target <name>` chooses only which state file it reads and
    which token it uses, never which cluster is removed. One AppRafter
    cluster per Hetzner project and the command means exactly what you
    expect; **two clusters in one project, and destroying either one
    destroys both.**

    `HCLOUD_TOKEN` exported in your shell also outranks the target's
    stored token, so an environment variable — not `--target` — would
    decide which project is emptied.

### Route A — same target, one machine at a time

Cheapest, and the machine is gone while the new one comes up.

```sh
apprafter backup create                                    # to an off-cluster repository
apprafter destroy --yes                                    # releases the machine
apprafter target machine                                   # pick the new one
apprafter restore <repo> --reprovision --server-type <sku> # rebuild, then replay
```

`apprafter destroy` clears the recorded cluster, which is what makes
`apprafter target machine` available again — it is the same "target with
no cluster yet" state a freshly registered target is in. Note that the
target keeps the **old** type as its saved preference, so either pick a
new one first or pass `--server-type` to the restore; otherwise the
rebuild reproduces the machine you were trying to leave.

[Move a cluster onto a bigger
machine](backup-restore.md#substrate-upgrade) is this route from the
backup side: the same four commands with an off-site (`s3:`) variant, what
to check once the new machine is up, how long the outage runs, and two
defects worth planning around. It is validated end to end on real Hetzner.

`destroy` names the machine it removed on its way out, so keep the line
if you may want to go back to it:

```text
  (destroyed server: type=<sku> region=<region> — note for restore --reprovision)
```

### Route B — a second target in a second Hetzner project, cut over, then remove the old one

More expensive for as long as both run, and the old cluster stays up
until you are satisfied with the new one. It asks for one thing Route A
does not, and the whole route rests on it: **the new target needs its own
Hetzner project, with an API token issued in that project.**

That is not tidiness. It is what makes the last step — destroying the old
cluster — a thing you can do at all, per the scope box above: with both
clusters in one project, `apprafter destroy --target <old-name>` would
take the new one with it, and nothing in the command or its flags can
narrow it. A second project is also why the new target needs no
`--cluster-name` juggling: `platform-1` in the new project is a different
machine from `platform-1` in the old one.

Create the project in the Hetzner Cloud Console, issue an API token in it
(Security → API Tokens), and register the new target with that token:

```sh
apprafter backup create
apprafter target add <new-name> --provider hetzner-cloud --token <new-project-token> --region <region> --server-type <sku>
apprafter restore <repo> --reprovision --target <new-name> --server-type <sku>
```

Then move DNS to the new cluster (see [Connect a
domain](connect-a-domain.md)), confirm it, and empty the old project:

```sh
apprafter destroy --yes --target <old-name>
```

That is safe here for one reason and you should be able to state it: the
old target's stored token belongs to the old project, and the old project
now holds nothing you want. Check that `HCLOUD_TOKEN` is **not** exported
in the shell you run it in — it outranks the stored token and would
redirect the command at whichever project it names.

`apprafter target use <name>` switches which target the commands without
`--target` act on.

!!! warning "If the two clusters must share one Hetzner project"

    Then `apprafter destroy` is not the teardown for this: it has no flag
    that narrows it to one cluster, and running it removes both. Delete
    the old machine **by ID in the Hetzner Cloud Console** instead — the
    server first, then its floating IP, firewall and network if nothing
    else uses them — and then `apprafter target remove <old-name> --yes`
    to drop the local record that now points at nothing.

    A shared project also brings back the cluster-name collision: the new
    target needs a **name of its own**, because provisioning looks for a
    machine by cluster name across the whole project and a second target
    left on the same name would find the first target's machine and
    reconcile it instead of creating anything. Read the old name off
    `apprafter target show` and pass a different one as
    `--cluster-name <new-cluster>`.

> **What will not work:** running `apprafter restore --reprovision` while
> a machine under the same cluster name is still there **in the same
> project**. Provisioning finds that machine, reconciles it, and creates
> nothing — the `--server-type` you passed is never used and the machine
> does not change. What makes a rebuild real is that no machine in the
> project answers to the name: either the old one is gone (Route A), or
> the new cluster is in a project of its own (Route B).

## Related

- [Operator quickstart](quickstart.md) — registering a target and
  provisioning the cluster, of which the machine choice is one step.
- [Target store reference](target-store.md) — where the saved type lives
  on disk, alongside the region and the credentials.
- [Node preparation](node-prep.md) — the control-plane headroom and swap
  the machine you chose then gets configured with.
- [Backup and restore](backup-restore.md) — the backup and replay both
  rebuild routes depend on.
- [Troubleshooting](troubleshooting.md) — every diagnostic code the CLI
  emits, including both server-type errors above.
- [`apprafter target machine`](../reference/cli/target.md), [`apprafter
  apply`](../reference/cli/apply.md) — the generated flag reference.
