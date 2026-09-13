---
description: "What a registered zone puts on the Gateway, how an application's expose block becomes a route that attaches to it, and what the PublicRouteReady verdict and the origin firewall each mean."
---

# The public route

What stands between `apprafter target domain add` and a browser reaching your
application. The recipe is
[Connect a domain](../operator-guide/connect-a-domain.md), with
[Cloudflare Origin CA certificate](../operator-guide/cloudflare-origin-cert.md)
for the certificate half and
[Publish the documentation site](../operator-guide/publish-the-docs-site.md) as
one worked instance; none of them needs any of this.

Read it when a hostname does not answer and nothing looks wrong. That symptom
has a specific shape here: `apprafter app status` reports the application
`Ready`, because the route's own verdict is a separate, deliberately soft
condition that never gates it.

Public ingress has no architecture decision record of its own — what follows is
the shipped implementation, named against the code that implements it. Two
neighbouring decisions do govern parts of it, and are cited where they bite.

## What registering a zone creates

`apprafter target domain add <zone> --cert <name>` does not touch the Gateway.
It first checks that the named Secret in `apprafter-system` carries the label
`apprafter.io/cert-mode: imported` — the label `apprafter target cert import`
writes — so an arbitrary Secret in that namespace, a cert-manager intermediate
say, cannot be made to back a listener. Then it merge-patches one entry onto
`PlatformStack.spec.values.gateway.allowedDomains` on the cluster's singleton
`PlatformStack` (`default`, in `apprafter-system`), recording the domain, the
Secret name, and who added it when. The patch is scoped to exactly that path
because `PlatformStack.spec.values` is a preserve-unknown blob carrying required
siblings such as `tier`, which a whole-object write could prune
(`cli/platform-cli/src/commands/target_domain.rs`).

Everything after that is the ordinary GitOps path. The platform controller
projects those values into the umbrella Argo CD Application's Helm values, and
the platform-stack chart renders the Gateway from them
([how the platform reconciles itself](platform-upgrades.md);
[ADR 0025](../adr/0025-gitops-control-surface.md) is why the CLI is not in that
path).

The whole Gateway template is guarded on that list being non-empty, so a freshly
bootstrapped cluster has **no Gateway at all** — it materialises with the first
registered zone. That is worth remembering when a route sits at `Pending`
forever.

Per zone the Gateway named `platform` in `apprafter-system` gains two HTTPS
listeners (`platform-stack/cue/render_tool.cue`):

| Listener | Hostname | TLS |
| --- | --- | --- |
| `https-apex-<zone-with-dashes>` | `<zone>` | `Terminate`, `certificateRefs` → the imported Secret |
| `https-wild-<zone-with-dashes>` | `*.<zone>` | the same Secret |

Both are `port: 443` with `allowedRoutes.namespaces.from: All`, which is what
lets a route in an application's own namespace attach to them. The names carry
the zone with its dots replaced by dashes, because a listener name is a
Kubernetes section name and cannot hold a dot.

**Why two.** A Gateway API listener carries exactly one hostname, and a
wildcard hostname never matches the apex: `*.example.com` matches
`app.example.com` — and `a.b.example.com` — but not `example.com` itself. One listener therefore cannot cover both a zone
and the names under it. It is also why the certificate has to be minted for
`<zone>` **and** `*.<zone>` — both listeners reference the same Secret, so a
certificate covering only the apex leaves the wildcard listener presenting a
name that does not match the request, which the edge surfaces as a 526.

Alongside the per-zone pairs there is one plain `http` listener on port 80 and a
`platform-http-redirect` HTTPRoute in `apprafter-system` bound to it by
`sectionName: http`, whose only rule is a request redirect to `https` with
status 301. Nothing else is served on `:80`.

The Gateway runs `gatewayClassName: cilium` in host-network mode: Cilium's Envoy
binds the node's ports 80 and 443 directly, so there is no LoadBalancer Service
and no address to allocate. The node's own IP is the address — which is what
`apprafter target ip` prints for your DNS records, and what makes the node's
cloud firewall the place where the origin can be restricted at all.

## How an expose block becomes a route

The operator renders an HTTPRoute for an application only when its **effective**
spec — `spec.base` with the deployment's environment override merged onto it,
see [Per-environment deploy](per-environment-deploy.md) — has `expose.network`
set to `public` **and** at least one `expose.hostname`. Anything else renders no
route, and the reconcile then deletes any route left from a previous spec, so
flipping an application back to `internal` withdraws it
(`operator/operator-rendering/src/lib.rs`,
`operator/operator-controllers/application/src/lib.rs`).

The route is named after the `Application` resource and lives in the
application's own namespace. It carries:

- `parentRefs[0]` → the `platform` Gateway in `apprafter-system`, `port: 443`,
  and deliberately **no `sectionName`**;
- `hostnames` → every entry of `expose.hostname`, a bare string normalised to a
  one-element list;
- one rule with no `matches`, so the apiserver's default match-all `/`, whose
  `backendRefs` is the application's own Service on port 80. The Service
  publishes 80 and targets `expose.port`, so the port in your manifest is the
  container's and never appears on the route.

**What attaches it to a listener** is that missing `sectionName` plus hostname
intersection. Gateway API matches the route's hostnames against each `:443`
listener's, so `example.com` lands on the apex listener and `app.example.com` on
the wildcard, and nothing in the application names a zone, a listener or a
certificate. Naming `port: 443` rather than a section is also what keeps the
route off the `:80` listener, leaving the platform's redirect the only thing
there.

Before applying, the controller injects the namespace and an `ownerReference`
back to the Application, then server-side applies it under the operator's field
manager — so the route is re-asserted on every reconcile and cascades away when
the application is deleted.

One guard sits in front of all of it: the apply runs only when the cluster
serves `httproutes.gateway.networking.k8s.io`. The operator probes for that CRD
once at startup and skips the apply otherwise, rather than 404-ing every
reconcile on a cluster without Gateway API.

The admission webhook enforces the pairing the renderer assumes
(`operator/admission-webhook/src/validator.rs`): `network: public` without a
hostname is rejected, and a hostname without `network: public` is rejected —
that second rule is what catches a mistyped `network` value. Each hostname must
be a DNS-1123 subdomain and a concrete host: the wildcard belongs to the
listener, not to the route. `expose.tls` at `false` on a public application is
rejected because this route attaches to `:443` only, and `network: vpn` is
reserved and rejected outright.

## Which applications a zone covers

A zone covers a hostname when the hostname **is** the zone, or is exactly one
label under it. `example.com` and `app.example.com` are covered by a
registration of `example.com`; `a.b.example.com` is not, and needs a zone of its
own — the certificate's wildcard and the listener's wildcard are both one level
deep, and the platform's own rule matches them.

That rule is implemented on both sides of the cluster boundary, and each side
answers a different question.

**Which applications use a zone** is the CLI's answer.
`apprafter target domain list` reads the registered entries off the
`PlatformStack`, then lists every `Application` in the cluster and counts those
whose `spec.base.expose.hostname` — **or** the `expose.hostname` of any
`Application.spec.environments` entry — falls under the zone. The `Apps` column
is that count, not a list of names
(`cli/platform-cli/src/commands/target_domain.rs`). The same count gates
`apprafter target domain remove`, which refuses outright while it is non-zero
and downgrades to one warning per application under `--force`.

**Whether a zone covers this application** is the operator's answer, and it is
where `NoMatchingZone` comes from. On each reconcile of a public application the
controller reads the registered zones off the singleton `PlatformStack` and
applies the same coverage rule to every hostname. The first hostname under no
registered zone produces the verdict below, with that hostname quoted in the
message.

## The PublicRouteReady verdict

`PublicRouteReady` is set by the Application controller on each successful
reconcile, and only for an application whose effective spec is public with at
least one hostname; other applications carry no such condition. It has two
inputs: the registered zones read from the `PlatformStack`, and the live
HTTPRoute's own `status.parents[].conditions`.

| Status / reason | What produced it |
| --- | --- |
| `True` / `Accepted` | Every hostname is covered by a registered zone, **and** some parent of the route reports `Accepted=True` and some parent reports `ResolvedRefs=True` — the two are read independently across parents, though today there is only one. |
| `False` / `NoMatchingZone` | At least one hostname is under no registered zone. Register it — the route is already written, so it attaches with no re-registration of the application. |
| `False` / `Pending` | The hostnames are covered but the route reports no acceptance yet: it was just applied, the Gateway has not settled, or there is no Gateway at all because no zone is registered. It is also what a cluster without the Gateway-API CRDs reports, since there is then no route status to read. |

Two properties are easy to misread.

**It is soft, and it never gates `Ready`.** The route is applied whatever the
verdict, and the application reconciles to `Ready` regardless. So a green
`apprafter app status` says nothing about whether the hostname is served.

**A read failure looks like `NoMatchingZone`.** The `PlatformStack` read is
best-effort: a missing CR or a read error yields an empty zone list, and an
empty list covers nothing.

No CLI command surfaces the condition today — `app status` reads the `Ready`
condition and nothing else off `status.conditions`, so this one is read with
`kubectl`:

```sh
kubectl get applications.apprafter.io <app> -n <namespace> \
  -o jsonpath='{range .status.conditions[?(@.type=="PublicRouteReady")]}{.status}{"\t"}{.reason}{"\t"}{.message}{"\n"}{end}'
```

Separately, the application's `status.endpointURL` becomes
`https://<first hostname>/` for a public application, and the in-cluster
Service URL for a non-public one that exposes a port. An application with no
`expose` block has no Service and carries no `endpointURL` at all. That is derived from the spec, not from the route's
fate: it says where the application *would* answer.

## What the origin firewall buys, and what it does not

`apprafter target firewall cloudflare-origin enable` does three things, in this
order. It records the toggle on the active target in the local target store
first, so the intent survives even if everything after it fails
([the target store on disk](the-target-store.md)); it records the same intent
in the cluster, as `PlatformStack.spec.firewall.cloudflareOrigin`; then it
reconciles the live cloud firewall.

The two records answer different questions. The target store is what
`apprafter apply` reads when it builds the node's firewall, at a moment when
there may be no cluster to ask. The CR is what a **backup** can read: the
firewall is a cloud object, so nothing about it would otherwise appear in a
snapshot, and a restore onto a new machine would bring the node up with
`80`/`443` open while the source had them shut. `PlatformStack/default` is the
first object every backup captures — including the scheduled in-cluster
runner's, which has no target store to read — so that is where the intent has
to live for a restore to find it. The cluster write is best-effort: it is
legitimate to set the toggle before a cluster exists, so an unreachable
apiserver warns (naming what a backup would then be missing) rather than
failing the command.

The reconcile fetches Cloudflare's published ranges from
`https://www.cloudflare.com/ips-v4` and `https://www.cloudflare.com/ips-v6` at
the moment you run the command. There are no fallback CIDRs compiled in: an
unreachable endpoint, or an empty answer for either family, aborts the command
rather than writing a firewall from a stale list
(`cli/cli-providers/src/cloudflare.rs`).

It then replaces the entire rule set of the firewall `apply` provisioned: the
id cached in the state file when there is one, and otherwise the firewall
matched by BOTH the name `<cluster>-fw` and the `apprafter=true` ownership
label, since name alone could rewrite an unrelated firewall in the same cloud
project. Of that rule set
exactly two rules change: the sources of `tcp/80` and `tcp/443` become the
Cloudflare set. SSH on 22, the Kubernetes API on 6443, WireGuard on `51820/udp`
and the ICMP rule keep `0.0.0.0/0` and `::/0`
(`cli/platform-cli/src/commands/firewall_spec.rs`). 6443 is deliberately left
open: Cloudflare does not proxy the apiserver, so gating it would break
`kubectl`.

Disabling restores the open default set and never calls out to Cloudflare — the
moment you most need to reopen 80/443 is the one where that endpoint may be the
thing that is broken. Two further mechanics: if the resolved cluster has no
firewall yet, the toggle is saved and the command warns rather than failing,
taking effect on the next `apprafter apply`; and at apply time an explicit
`cloudflareOrigin` in the infrastructure manifest's `firewall` block wins over
the stored toggle, the manifest being the stated intent and the toggle the
fallback.

**What it buys.** With the node's HTTP ports reachable only from the CDN's
ranges, a request cannot skip the edge. Whatever the edge is doing for
you — terminating the browser's TLS, rate limiting, filtering, caching — stops
being optional for anyone who has learned the node's address, and that address
is not a secret: it is in DNS history and in any scan of the provider's ranges.

**What it does not buy.** It is an IP allow-list, not authentication. The
allowed set is the CDN's entire published range, shared by every customer of
that CDN, so what it establishes is that a connection arrived *through
Cloudflare* — not that it arrived through *your* zone. Closing that gap is an
origin-authentication feature on the CDN side, and this platform configures
nothing in your Cloudflare account: it holds no Cloudflare API credential and
makes exactly the two range fetches above. Nor does the rule set do anything for
the other open ports, encrypt the edge-to-origin hop — the imported certificate
and **Full (strict)** do that — or inspect a single request. It is a cloud
firewall rule set and nothing more.

## What is held before any of this runs

Making an application public, and giving a public application a hostname it did
not have, are both classified as escalations on the security axis: the operator
cuts a `MigrationPlan` and holds the application at `AwaitingMigrationApproval`
while the previous spec keeps serving. Retargeting the port of an
already-public application is the third. The reverse direction gates too, as an
availability change — withdrawing `public`, or dropping a hostname a public
application was serving.

[How the approval gate works](the-approval-gate.md) is the mechanism;
[ADR 0052](../adr/0052-migration-security-axis.md) is why the additive direction
is the one classified as a security boundary.

## See also

- [Connect a domain](../operator-guide/connect-a-domain.md) — the recipe this
  page is behind.
- [Cloudflare Origin CA certificate](../operator-guide/cloudflare-origin-cert.md) —
  minting and rotating the certificate both listeners reference.
- [Publish the documentation site](../operator-guide/publish-the-docs-site.md) —
  the same procedure carried out once, on a subdomain of an existing zone.
- [Per-environment deploy](per-environment-deploy.md) — the merge that decides
  which `expose` block is in force.
- [How the approval gate works](the-approval-gate.md) — why going public waits.
- [Worked example: the documentation site](deploying-the-docs-site.md) and
  [the landing site and its CMS](deploying-the-landing-and-cms.md) — one host on
  one application, and two hosts on two.
- [ADR 0025](../adr/0025-gitops-control-surface.md) and
  [ADR 0052](../adr/0052-migration-security-axis.md) — the two decisions this
  page leans on: why the CLI is not in the reconcile path, and why gaining a
  public address is gated in the additive direction. **There is no ADR for
  public ingress itself.** The listener pair, the one-label rule and the route
  rendering were built without one, so the code is the only record of why they
  take the shape they do — worth knowing before changing any of them.
