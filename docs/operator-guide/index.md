---
description: "A map of every page in the operator guide, grouped by the job you came to do — set a cluster up, put it on the internet, give an application a dependency, and day-2 work."
---

# Operator Guide

You are in the right place if you run the cluster: you own the machine,
the platform on it, and the backups. If you are shipping an application
onto a cluster somebody else runs, the [Developer
Guide](../dev-guide/index.md) is the shorter path.

> **Status:** every guide below is written for the single-node Tier-1
> path and verified on it. Tiers 2 to 4 are not documented yet — where a
> page says something tier-specific, it says Tier 1 unless it names
> another. The pages listed under "not documented yet" at the foot of
> this page are exactly that; nothing else on this page is a promise.

## Set a cluster up

- [Operator quickstart](quickstart.md) — from a blank Hetzner account to
  a self-managing cluster, with `apprafter target add` and then
  `apprafter up`. Start here. (The older one-shot
  `apprafter init` still works and suits scripted setups.)
- [Managing targets](target-store.md) — running more than one, and the
  rest of a target's life: inspecting, renaming, and removing one
  without stranding the servers it provisioned.
- [Private repos & registries](../dev-guide/private-repos-and-registries.md)
  — `apprafter repo creds add` registers one credential that Argo CD
  clones private repositories with and the node pulls private images
  with, for GitHub, GitLab and self-hosted Gitea or Forgejo. A public
  repository needs none of this: registering an application with
  `apprafter app add` is the whole step, and the
  [developer quickstart](../dev-guide/quickstart.md) walks it.

## Put it on the internet

- [Connect a domain](connect-a-domain.md) — the once-per-cluster
  preparation and the per-zone steps that put an application behind
  Cloudflare on HTTPS.
- [Cloudflare Origin CA certificate](cloudflare-origin-cert.md) —
  minting the certificate Cloudflare's edge trusts, importing it, and
  rotating it.
- [Publish the documentation site](publish-the-docs-site.md) — the two
  pages above worked through end to end on one application: confirming a
  registered zone covers a subdomain, the Cloudflare record, and the sync
  that follows. The instance is this site, on `docs.apprafter.dev`.

## Make it survivable

- [Back up a cluster](backup-restore.md) — a copy on your own machine,
  and a copy off-site in S3 on a schedule. **Set this up before you need
  it**, which is why it is here and not under Day 2.
- [Restore from a backup](restore.md) — the three target modes, the
  ordering a restore is safe under, and the disaster-recovery runbook
  for an off-site repository.
- [Backup retention, integrity and credentials](backup-maintenance.md) —
  how long snapshots live, the integrity check and its locks, and how
  narrow the in-cluster credentials can be. Not a first read.

## Give an application a dependency

An application declares what it needs; the platform provisions it,
binds the credentials, and opens exactly the egress that need implies.

- [Postgres](postgres.md) — a `needs.pg` declaration end to end: claim,
  provisioning, credential binding, and the grace window after removal.
- [Redis](redis.md) — a `needs.redis` declaration end to end, including
  the per-claim logical-database isolation on the shared pool.
- [Persistent disk](persistent-disk.md) — a `needs.disk` declaration end
  to end, and the single-writer constraint it puts on the workload.
- [Shared volumes](shared-volumes.md) — one directory mounted by several
  applications in a namespace, and when to reach for an owned disk
  instead.
- [Egress policy](egress-policy.md) — how a declaration is also what
  opens the path to the thing declared, how to watch an undeclared reach
  get dropped, and the cluster-wide profile knob.

## Day 2, and getting out of trouble

- [Choosing the machine](choosing-the-machine.md) — reading the live
  machine catalogue and the three ways to supply a server type. The
  `target add` wizard already makes this choice, so this is the
  reference for the exceptions.
- [Moving to a bigger machine](moving-to-a-bigger-machine.md) — a
  planned rebuild when the node has become too small.
- [Node preparation](node-prep.md) — a cluster provisioned by
  `apprafter apply` already has the control-plane reservations and the
  host swap; this is the retrofit for a node that does not, and how to
  read `apprafter node status`.
- [Platform management](platform-management.md) — how the platform
  upgrades itself: the `PlatformStack` resource, channels and pins,
  component freezes, and the CLI that edits them.
- [Resources and autoscaling](../dev-guide/resources-and-autoscaling.md)
  — what every application requests by default, the in-place
  right-sizing the platform does on top, and the cluster-wide
  `apprafter platform autoscale` mode. Written for application authors;
  the mode is yours.
- [Migration plans](migration-plans.md) — what counts as a destructive
  change, how the platform pauses one behind an approval, and the CLI
  that approves it.
- [Troubleshooting](troubleshooting.md) — the diagnostic codes the CLI
  emits, what each means, and the exact command to run next.
- [Recovery and emergency console access](recovery.md) — getting back
  into a VM that no longer answers SSH, via Hetzner Rescue Mode, and
  when to rebuild instead.

## Not documented yet

Named so you can tell a gap from an oversight. None of these has a page
on this site today.

- **Tier upgrades.** `apprafter upgrade-tier` is **not implemented in
  this release**: it validates `--to` and prints the move it would
  make, and changes no infrastructure and no platform configuration.
  The tier is chosen when the cluster is first created. There is
  nothing to document until it lands, so this is a missing capability
  rather than a missing page.
- **Tiers 2, 3 and 4 generally.** Every page above is a Tier-1
  procedure.
- **Single sign-on, private networking, and synthetic monitoring** —
  OIDC, Headscale or Tailscale, and uptime checks.
- **Managing `AccessGrant`s**, and reading audit logs out of JetStream.
- **Day-2 debugging with k9s, Headlamp and Hubble.**
- **Disaster-recovery runbooks.** [Restore from a
  backup](restore.md) documents the commands, including rebuilding a
  cluster from a backup; what is missing is the drill around them.

## Where else to look

- [Reference](../reference/index.md) — the generated CLI pages, the
  environment variables, and the custom resources the platform
  installs. The [CLI reference](../reference/cli/index.md) covers
  every subcommand and flag.
- [ADR index](../adr/README.md) — the decision behind each behaviour,
  including [ADR 0030](../adr/0030-cli-target-store-and-credential-chain.md)
  for the target store and its credential chain. An ADR describes the
  world as it was when it was ratified, so read it for *why*, and the
  pages above for *what ships*.
