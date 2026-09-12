---
description: "Every user-facing feature, its status, and the phase it lands in."
# No exemptions, and that is the rule rather than luck: an unbuilt row
# names the CAPABILITY, never the command or the schema key that would
# deliver it. A reader cannot run a command that does not exist, and a
# `needs` type no provider ships is a claim a manifest cannot make. Both
# exemptions this page used to carry were retired by applying that rule
# to the rows that needed them.
---

# Feature status

What AppRafter can do today, what is being built, and which phase each thing lands in.

**Delivery:** `☐` not started · `🚧` partially landed · `✅` delivered **and** verified — a passing
live walk or end-to-end run, not merely committed.

**Properties and policies:** `◆` in force today · `◇` not in force yet. These are architectural
guarantees and published commitments rather than features. They have no ship date, so their Phase
cell is empty: there is no release to subscribe to.

**Phase** links to the roadmap entry on the landing page, where you can subscribe to hear when that
phase ships. Two kinds of row are not links: those already shipped, and those marked `Post-launch` —
post-launch work is not a roadmap phase and has no subscribe control.

---

## Deploying an application

| Phase | Status | Feature | Documentation |
|---|---|---|---|
| Shipped | ✅ | Provision and run a cluster from one CLI (`init` → `apply` → `up` → `destroy`) | [Quickstart](operator-guide/quickstart.md) |
| Shipped | ✅ | One-command bootstrap: k3s, Cilium, Gateway API, cert-manager and Argo CD | [Quickstart](operator-guide/quickstart.md) |
| Shipped | ✅ | Deploy from a CUE `Application` manifest through GitOps, including tag→digest auto-deploy | [Writing Application.cue](dev-guide/application-cue.md) |
| Shipped | ✅ | Typed configuration with composition — CUE plus admission-webhook validation | [Writing Application.cue](dev-guide/application-cue.md) |
| Shipped | ✅ | One manifest serves several environments | [Per-environment deploy](how-it-works/per-environment-deploy.md) |
| Shipped | ✅ | Scaffold a starter manifest for an application (`apprafter app scaffold`) | [Developer quickstart](dev-guide/quickstart.md) |
| Shipped | ✅ | Roll back to the previously resolved image and hold there until released | [Rolling back a bad deploy](dev-guide/rollback.md) |
| Shipped | 🚧 | Backstage developer portal — application status and a golden-path template | — |

> **Backstage is partial, and further from ready than "opt-in" suggests.** The portal's source and
> a golden-path template are in the repository, and the platform chart carries a component for it —
> but the component is off on the only shipped tier, no release publishes a portal image, the
> template is not registered in any catalogue the platform installs, and `apprafter open` reaches
> Argo CD only and says so. The portal work — the secret-encryption wizard and the approval plugin —
> is in the post-launch bundle.

---

## Data and storage

| Phase | Status | Feature | Documentation |
|---|---|---|---|
| Shipped | ✅ | On-demand Postgres (`needs.pg`) | [Postgres](operator-guide/postgres.md) |
| Shipped | ✅ | On-demand Redis-compatible cache (`needs.redis`) | [Redis](operator-guide/redis.md) |
| Shipped | ✅ | Message streams and durable consumers, declared in the manifest (`needs.jetstream`) | [JetStream](operator-guide/jetstream.md) |
| Shipped | ✅ | On-demand block storage (`needs.disk`) | [Persistent disks](operator-guide/persistent-disk.md) |
| Shipped | ✅ | One volume shared between applications, with removal refused while it is referenced | [Shared volumes](operator-guide/shared-volumes.md) |
| Shipped | ✅ | Back up and restore a whole cluster, or export one dependency's data | [Back up a cluster](operator-guide/backup-restore.md) |
| Shipped | ✅ | Scheduled off-site backups to S3, opt-in | [Back up a cluster](operator-guide/backup-restore.md) |
| Shipped | ✅ | A backup schedule set from flags — the interval, the retention and the integrity check | [Backup maintenance](operator-guide/backup-maintenance.md) |
| Shipped | ✅ | A cache credential survives a restart of the cache | [Redis](operator-guide/redis.md) |
| [Phase 3](https://apprafter.dev/#roadmap-phase-tier2) | ☐ | An analytics store and object storage, declared the same way as the dependencies above | — |
| [Phase 3](https://apprafter.dev/#roadmap-phase-tier2) | ☐ | An event backbone for control-plane operations. A replayable audit log on top of it is exploratory, and available on request | — |

> A cache is ephemeral by declaration and stays out of a backup unless you mark it otherwise. What
> each command captures, and what it does not, is on the backup page.

> A JetStream account belongs to the **namespace**, and applications inside it are separated by
> subject permissions rather than by an account each — so the namespace is the trust boundary. An
> application whose subject prefix is already collected by a neighbour's declared stream is told so
> when it arrives; collecting a neighbour's subjects is a legitimate, approval-gated way to express
> fan-in, so this is a warning rather than a refusal and the application starts normally.
> A namespace's memory budget is reserved on the shared message server when its first application
> arrives, and the server has a fixed amount to give — roughly three JetStream namespaces fit on a
> Solo node today. The reservations are summed against that limit: a namespace that would not fit is
> refused and told so on its own dependency, naming the budget, while every namespace already on the
> server keeps running untouched. A larger per-tier budget is planned.
> JetStream stores are out of scope for backup and restore.

---

## Networking and domains

| Phase | Status | Feature | Documentation |
|---|---|---|---|
| Shipped | ✅ | A public URL over HTTPS on your own domain, generated from the manifest | [Connect a domain](operator-guide/connect-a-domain.md) |
| Shipped | ✅ | Egress policy derived from what an application declares it needs | [Egress policy](operator-guide/egress-policy.md) |
| [Phase 3](https://apprafter.dev/#roadmap-phase-tier2) | ☐ | Declarative external surface (`ExternalSurface`) | — |
| [Phase 3](https://apprafter.dev/#roadmap-phase-tier2) | ☐ | Automatic DNS records | — |
| [Phase 3](https://apprafter.dev/#roadmap-phase-tier2) | ☐ | Mutual TLS between your services | — |

> Two hardening defaults ship with the public path rather than as features of their own: a network
> policy is derived from each application's `needs`, and the node's HTTP ports can be restricted to
> your CDN's address ranges so the origin is not reachable around it.

---

## Secrets and credentials

| Phase | Status | Feature | Documentation |
|---|---|---|---|
| Shipped | ✅ | Encrypted secrets committed to Git, decrypted only in the cluster | [Secrets](operator-guide/secrets.md) |
| Shipped | ✅ | Reference a secret or a dependency's credential straight from `env` | [Writing Application.cue](dev-guide/application-cue.md) |
| Shipped | ✅ | Deploy from private repositories and registries with one credential | [Private repositories and registries](dev-guide/private-repos-and-registries.md) |
| Shipped | ✅ | See what is sealed and where, and retire one when it is no longer needed | [Secrets](operator-guide/secrets.md) |
| Post-launch | ☐ | Defence in depth against a compromised cluster administrator | — |
| Post-launch | ☐ | End-to-end identity propagation, so an audit trail names the workload and not just the node | — |
| [Phase 3](https://apprafter.dev/#roadmap-phase-tier2) | ☐ | A managed secret store on multi-node clusters, in place of sealed secrets | — |

---

## Day-2 operations

| Phase | Status | Feature | Documentation |
|---|---|---|---|
| Shipped | ✅ | One command for the state of the cluster — version, upgrades, unhealthy conditions, applications in trouble, changes awaiting approval | [CLI reference](reference/cli/status.md) |
| Shipped | ✅ | A failing reconcile is visible without reading the operator log | [Troubleshooting](operator-guide/troubleshooting.md) |
| Shipped | ✅ | Disk pressure on the node is surfaced before it becomes an outage, and clears on its own | [Troubleshooting](operator-guide/troubleshooting.md#node-disk) |
| Shipped | ✅ | Removing a dependency from the manifest releases it onto the documented retention path | [Troubleshooting](operator-guide/troubleshooting.md) |
| Shipped | ✅ | A one-line installer that verifies its own checksum, and a download page | [Quickstart](operator-guide/quickstart.md) |
| [Phase 3](https://apprafter.dev/#roadmap-phase-tier2) | ☐ | Built-in metrics, traces, logs and network-flow visibility | — |
| Post-launch | ☐ | A rescue path that does not depend on the platform being healthy | — |
| [Phase 3](https://apprafter.dev/#roadmap-phase-tier2) | ☐ | Notifications sent by the platform | — |

---

## The platform itself

| Phase | Status | Feature | Documentation |
|---|---|---|---|
| Shipped | ✅ | The platform updates itself through GitOps — no re-bootstrap for a new release | [Platform upgrades](how-it-works/platform-upgrades.md) |
| Shipped | ✅ | Destructive changes are held until a human approves them, from the CLI or from Argo CD | [When a change needs approval](dev-guide/when-a-change-needs-approval.md) |
| Shipped | ✅ | A documentation site kept true to the code by a gate that runs on every commit | [Publishing this site](operator-guide/publish-the-docs-site.md) |
| Shipped | 🚧 | The landing, this site and the README name the same phases, and every roadmap phase has a subscribe control | — |
| Post-launch | ☐ | Community migration plugins for platforms we do not cover ourselves | — |

> **Phase naming is `🚧`.** The registry, this site, the README and every
> tracked landing file agree, and a test holds them there. Three names on the
> landing still resolve to no registry entry — a fractional `Phase 4.5` for an
> add-on with no roadmap card, and `Phase 1` / `Phase 2` used as era names for
> work already shipped. Each is a known exception with its reason recorded
> beside the check, not a drift nobody noticed.

---

## More machines, and bigger ones

| Phase | Status | Feature | Documentation |
|---|---|---|---|
| Shipped | ✅ | Choose the machine at provision time from a live catalogue of regions and sizes | [Choosing the machine](operator-guide/choosing-the-machine.md) |
| [Phase 3](https://apprafter.dev/#roadmap-phase-tier2) | ☐ | A three-node cluster with a highly available control plane | — |
| [Phase 3](https://apprafter.dev/#roadmap-phase-tier2) | ☐ | The same manifest runs on a single node and on a cluster | — |
| [Phase 8](https://apprafter.dev/#roadmap-phase-federation) | ☐ | Move a running application between clusters with a sub-second cutover | — |
| Post-launch | ☐ | Grow a single-node cluster into a multi-node one in place | — |
| Post-launch | ☐ | Approve or reject a held change from the portal, not only the CLI | — |
| [Phase 5](https://apprafter.dev/#roadmap-phase-tier3) | ☐ | A one-time import from another platform, checked before it commits | — |
| [Phase 5](https://apprafter.dev/#roadmap-phase-tier3) | ☐ | Bare metal, for workloads that want the whole machine | — |
| [Phase 5](https://apprafter.dev/#roadmap-phase-tier3) | ☐ | Hard multi-tenancy — many customers on one cluster, isolated from each other | — |
| [Phase 6](https://apprafter.dev/#roadmap-phase-tier4) | ☐ | Confidential computing — workloads the machine's operator cannot read | — |

---

## The managed offering

The launch plan is **Hosted Services**: the portal and the AI-facing layer are hosted, and the
cluster stays an ordinary AppRafter install on your own infrastructure.

| Phase | Status | Feature | Documentation |
|---|---|---|---|
| [Phase 4](https://apprafter.dev/#roadmap-phase-managed) | ☐ | A hosted account and sign-up | — |
| [Phase 4](https://apprafter.dev/#roadmap-phase-managed) | ☐ | Register a cluster with an outbound agent — nothing inbound to open | — |
| [Phase 4](https://apprafter.dev/#roadmap-phase-managed) | ☐ | A hosted portal on your own subdomain | — |
| [Phase 4](https://apprafter.dev/#roadmap-phase-managed) | ☐ | A hosted endpoint for AI clients, proxied to your cluster | — |
| [Phase 4](https://apprafter.dev/#roadmap-phase-managed) | ☐ | A billing view in the portal | — |
| [Phase 5](https://apprafter.dev/#roadmap-phase-tier3) | ☐ | A live playground for trying the platform without installing it | — |

---

## Architecture and policy

| Phase | Status | Feature | Documentation |
|---|---|---|---|
| — | ◆ | Open core with no exit cost — cancelling a hosted plan leaves the cluster running, with no migration to perform | [License](license.md) |
| — | ◇ | Minimal data exposure — the hosted side sees manifests, status and audit events, never the data your applications hold | — |
| — | ◇ | A short, published list of sub-processors | — |
| — | ◆ | Independent ownership — the roadmap is set by the people running the platform | — |
| — | ◆ | Pricing anchored to cost and published in full | — |

---

## Not on the roadmap

Everything above is either built or planned. These are neither, and they are
listed so that "no row" is not read as "nobody thought about it". Nothing here
has a phase or a subscribe control, because nothing here has a date.

**Wanted, and waiting for someone to want it.** Each is understood and would be
built on demand; none is scheduled, and asking is what would move one:

- Horizontal autoscaling driven by a queue depth or a custom metric, rather
  than by CPU.
- A fixed outbound IP for your applications, for allow-lists on the other side.
- Access grants and single sign-on for the cluster's own surfaces.
- Image vulnerability scanning and a published bill of materials.
- A cost view in the portal.

**Deliberately not built.** These are decisions rather than gaps:

- **A local development mode.** The platform targets a real cluster; running it
  on a laptop would be a second product with its own failure modes.
- **Self-hosted Git, registry or VPN as platform components.** You bring your
  own, and the platform integrates with them.
- **A plugin ecosystem.** Extension exists at the service-provider boundary
  only, and opening more of it would make "one way to do things" untrue.
