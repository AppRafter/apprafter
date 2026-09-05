---
description: "Moving a healthy cluster onto a different machine: why it is a rebuild, the two project topologies it can take, the sequence for each backup backend, and what it costs."
---

# Moving to a bigger machine

The node under a healthy cluster has become too small — it is out of
allocatable memory and the scheduler has started refusing pods — and you want
the same cluster, with the same data, on a bigger machine. There is no
in-place resize and no region change either, so the move is a rebuild: take a
backup, release the machine, provision a bigger one, replay the backup.

`apprafter target machine` refuses outright once a target has a provisioned
cluster, rather than saving a preference that would never take effect. That
refusal is what sends you here.

It runs the same `--reprovision` command as disaster recovery, and it is not
the same operation. In a disaster the source cluster is already gone, the
backup is whatever the schedule last managed to take, and you are recovering
from a position you did not choose. Here the source is healthy and in your
hands: you take the backup yourself and know it is current, you drain what
will not survive, and you pick the hour. If your cluster is already dead, read
[Restore from a backup](restore.md) instead — everything below assumes a
cluster that still works.

Read [Back up a cluster](backup-restore.md) first if you have not: both routes
need a backup that lives somewhere other than the cluster you are about to take
apart, and both have downtime.

## Before you start

- **Persistent redis comes across; ephemeral caches do not.** A restore
  live-loads each `persistent: true` `needs.redis` claim's Dragonfly snapshot
  back into place — see the redis note on [Back up a
  cluster](backup-restore.md). A `persistent: false`
  claim is re-provisioned empty (it is a cache by declaration). A planned
  upgrade is the good case for the latter: drain a queue or accept an empty
  cache before you take the backup. In a disaster you get no such chance.
- **A bigger node does not raise per-application limits.** The default memory
  limit is 512Mi on every machine size and does not grow with the node — see
  [Resources and
  autoscaling](../dev-guide/resources-and-autoscaling.md#when-512mi-is-the-problem).
  If one application is the thing hitting a ceiling, a bigger machine may not
  be the fix you need.
- **Do not resize in the provider console instead.** It looks cheaper and it
  leaves the cluster's recorded machine type disagreeing with the live one,
  which `apprafter apply` then warns about on every run until you reconcile it
  with `apprafter import --force`. The route below keeps the record and the
  machine in step.

## It is one cluster, not two

`apprafter destroy` clears the recorded cluster from local state; it does not
touch the target. The name, the token, the region and the SSH key all survive,
which is exactly what lets `restore --reprovision --target <same>` provision
back into the same target and the same Hetzner project. You register no second
target, issue no second token, and cut nothing over: at the end there is one
cluster in one project, on a bigger machine.

That is what separates it from Route B below, which deliberately runs two
clusters at once — safer, and more expensive for as long as both are up.

## `apprafter destroy` empties a provider project, not a cluster

!!! danger "Read this before either route"

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


## Route A — same target, one machine at a time

Cheapest, and the machine is gone while the new one comes up.

```sh
apprafter backup create                                    # to an off-cluster repository
apprafter destroy --yes                                    # releases the machine
apprafter restore <repo> --reprovision --server-type <sku> # rebuild, then replay
```

`apprafter destroy` clears the recorded cluster, which is what makes
`apprafter target machine` available again — it is the same "target with
no cluster yet" state a freshly registered target is in. You do not need
it here: `--server-type` on the restore names the machine to build, and
the target adopts what it actually provisioned, so a later rebuild that
names no type reproduces the new machine rather than the old one.

[The sequence — host-local repository](#the-sequence-host-local-repository)
below is these three commands in full, with an off-site (`s3:`) variant beside
it; it is validated end to end on real Hetzner.

`destroy` names the machine it removed on its way out, so keep the line
if you may want to go back to it:

```text
  (destroyed server: type=<sku> region=<region> — note for restore --reprovision)
```


## Route B — a second target in a second Hetzner project, cut over, then remove the old one

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


## The sequence — host-local repository

```sh
# 1 — back up, into a repository that is not on the cluster
RESTIC_PASSWORD=<passphrase> apprafter backup create --repo /backups/prod-repo

# 2 — confirm the snapshot is really there, before anything is destroyed
RESTIC_PASSWORD=<passphrase> apprafter backup list --repo /backups/prod-repo

# 3 — release the machine (read the destroy-scope warning on the machine page)
apprafter destroy --yes --target prod

# 4 — provision the bigger machine and replay the backup into it
RESTIC_PASSWORD=<passphrase> apprafter restore /backups/prod-repo \
    --reprovision --server-type cx33 --target prod
```

`--server-type` on the restore is the whole answer: the target records the
machine it actually provisioned, so a later rebuild that names no type
reproduces the new machine rather than the old one.

## The sequence — off-site (S3) repository

When the cluster already pushes an encrypted repository off-site on a schedule
([Back up off-site, on a schedule](backup-restore.md#back-up-off-site-on-a-schedule)), the artifact you
restore from is already there. Step 1 becomes a check that it is current and
intact rather than a fresh backup.

```sh
# 1 — confirm the off-site repository is current and sound
apprafter backup status
apprafter backup check --repo s3:<endpoint>/<bucket>/<prefix> \
    --credential-file ./operator-s3.env

# 2 — release the machine
apprafter destroy --yes --target prod

# 3 — provision the bigger machine and replay from off-site
apprafter restore s3:<endpoint>/<bucket>/<prefix> \
    --reprovision --server-type cx33 --target prod \
    --credential-file ./operator-s3.env
```

Step 1 is worth doing before the destroy regardless — a repository you verify
while the cluster is still up is one you can still fall back to — but it is not
a constraint: `backup check` runs offline from `--repo` and the operator's
credentials, with or without a cluster.

The credentials in `--credential-file` are the **operator's**, read from your
own machine: between steps 2 and 4 there is no cluster left to read a
credential from. That is the [two-tier credential
model](backup-restore.md#back-up-off-site-on-a-schedule) doing the exact job it exists for, and a
substrate upgrade is a cheap way to find out whether yours actually works —
if the operator-side credentials cannot reach the repository while the cluster
is down, neither could a real recovery.

Scheduled backup survives the move. `PlatformStack.spec.backup` is part of the
captured configuration, so the rebuilt cluster comes back with the same
bucket, schedule and retention, and `apprafter backup status` reports it
enabled again without you re-running `apprafter backup enable`.

## What to verify afterwards

`restore` reports success once it has replayed the artifact. That is not the
same as the upgrade having worked. Six checks, each earning its place:

1. **The server type, read from the provider.** Take it from the Hetzner Cloud
   Console or the provider API rather than from local records: only the
   provider says what it actually got. `apprafter target show` should agree —
   the target adopts the machine it provisioned — and a disagreement between
   the two is itself the finding.
2. **The server id changed.** A new id is what proves a genuinely new machine
   rather than a local record rewritten around the old one. If the id is the
   same, no rebuild happened — the most likely cause is the cluster-name
   collision described under [Route B](#route-b-a-second-target-in-a-second-hetzner-project-cut-over-then-remove-the-old-one),
   where provisioning
   finds the existing machine, reconciles it, and never uses the
   `--server-type` you passed.
3. **Node allocatable memory grew.** This is the number the scheduler budgets
   against, and it is the reason you did any of this — a bigger SKU whose
   allocatable did not move has bought you nothing. Across the two
   validation runs it went from 1963 MiB to 5895 MiB on a `cx23` → `cx33`
   move.

    ??? note "Reading it with kubectl"

        ```sh
        kubectl get node -o jsonpath='{.items[0].status.allocatable.memory}'
        ```

4. **The workload reached `Ready` on its own.** Not "the pods exist" — the
   application's own readiness, arrived at without you intervening. The
   restore resumes workloads only after the data is loaded, so an application
   that reaches `Ready` did so against restored state.
5. **A sealed secret decrypts to its original value.** SealedSecrets are bound
   to the cluster that sealed them, so every one of them was re-sealed against
   the new cluster's key during the replay ([Secrets are re-sealed for the
   target](restore.md#secrets-are-re-sealed-for-the-target)). Read one back and compare
   it to what you put in. The failure this catches is an application sitting
   at `Ready=False` with `EnvSecretMissing`.
6. **The data, compared properly.** A marker row is not evidence. A single row
   you planted and read back proves only that *a* restore happened; it would
   still read back if every other table had been truncated. Use the stronger
   check the walk uses:

    - **per-table row counts**, compared table for table, so "rows went
      missing" is a distinguishable failure from "the same rows, different
      bytes"; and
    - **a content digest** over every user table, hashed with an explicit,
      deterministic `ORDER BY` — otherwise the digest depends on the order
      rows happen to come back in and tells you nothing.

    Compute both on the source before the backup and again on the upgraded
    cluster. Compute each **twice on each side**: if the two computations on
    one side disagree, that database is still being written to and any
    comparison across the upgrade is noise rather than a result — a failure you
    want to be able to tell apart from real data loss. Both validation runs
    did exactly this against a real CMS database — 73 tables and 701 rows in
    the first — and the digest came back byte-identical each time.

## The downtime

The cluster is destroyed and rebuilt. This is not a live migration and there
is no overlap: from `apprafter destroy` until the workloads are back up on the
new machine, the cluster does not exist and nothing it served is reachable.

Two full runs of the automated walk on real Hetzner took **29m35s** and
**21m9s** wall-clock. Read that as an order of magnitude, not a budget — it
covers the whole walk, including standing the original cluster up and seeding
it, which you are not doing. Your outage is the `destroy` → workloads-`Ready`
span inside it.

What dominates that span is the rebuild: provisioning the machine and running
the full bootstrap — Cilium, Argo CD, then the platform stack syncing its
components — not the data load, which was small at this size and scales with
your data rather than with anything AppRafter controls. So plan for tens of
minutes rather than seconds, and measure your own before you commit to a
maintenance window. The walk's phase banners carry an elapsed clock, which
makes the Phase 5 → Phase 7 span the number to read off a run of your own.

## The executable version

`e2e/substrate-upgrade-hetzner.sh` is this procedure written as a script, and
it is the specification the section above describes: it provisions a `cx23`,
deploys a real application with a `needs.pg` claim and a `secret:` reference,
fingerprints the database, upgrades to a `cx33` through the sequence above,
and asserts every item in *What to verify afterwards*. One switch
(`APPRAFTER_SUBSTRATE_BACKEND=local|s3`) selects the storage backend, so both
sequences are the same script. Its Phase 7 is where the verification list came
from.

Unlike the local-cluster harnesses cited elsewhere on this site, it runs
against **real Hetzner** and spends real money — two short-lived machines, on
the order of a couple of euro cents — and it calls `apprafter destroy`, which
empties the whole project its token belongs to. Point it at a project you are
willing to lose.


## See also

- [Choosing the machine](choosing-the-machine.md) — reading the catalogue and
  the three rungs a type can be supplied on, which is the decision this page
  acts on.
- [Back up a cluster](backup-restore.md) — the artifact both routes depend on.
- [Restore from a backup](restore.md) — the unplanned version of the same
  `--reprovision`.
