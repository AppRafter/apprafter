---
description: "The three steps outside the repository that put this documentation site on docs.apprafter.dev — confirming the zone, the Cloudflare record, and the sync that follows."
---

# Publish the documentation site

This is the runbook for putting **this site — the one you are reading —**
on `https://docs.apprafter.dev/`. The repository ships the image, the
manifest and the release workflow; three steps are outside it and belong
to whoever runs the cluster and holds the Cloudflare account. This page
is those three steps, in order, with how to tell each one worked.

It doubles as the worked example of **adding an application on a
subdomain of a zone you have already connected**. If that is what you
came for, [Connect a domain](connect-a-domain.md) is the general
procedure and everything below is one instance of it — the only thing
special about this instance is that the application happens to be the
documentation.

**Prerequisites:** a bootstrapped cluster with the `apprafter` CLI
pointed at it (`apprafter target list` shows it active), the
`apprafter.dev` zone already connected per [Connect a
domain](connect-a-domain.md), and the site image published.

## What the repository already ships

| Path | What it is |
| --- | --- |
| `docs-site/Dockerfile` | Copies a **prebuilt** `site/` into `caddy:2-alpine`. The site is built outside the image, under the pinned toolchain, so the published bytes and the checked bytes come from one build. |
| `docs-site/Caddyfile` | Static serving on `:80`, the cache headers, and the content types for the plain-text exports. |
| `docs-site/apprafter/Application.cue` | The deployable manifest: two replicas, `port: 80`, public on `docs.apprafter.dev`. |
| `.github/workflows/release-docs.yml` | Builds under Nix and pushes `ghcr.io/apprafter/docs`, on a documentation change merged to the default branch. |

Nothing below edits any of them.

### Confirm the image exists first

Every step after this assumes there is something to deploy. The release
workflow publishes two tags — a rolling `:latest`, which the manifest
watches, and an immutable `:<git sha>` per build.

**The authoritative check is the workflow's own run history**: if
`release-docs` has never completed green, nothing has been published and
the rest of this page has nothing to point at.

A registry probe is a tempting shortcut and a poor one. GHCR answers
`403 Forbidden` both for a package you are not entitled to read *and*
for one that does not exist, so a failure cannot tell you which — and
`docker manifest inspect` is a Docker-specific verb that a Podman
`docker` shim does not answer the same way. Read the workflow run.

If the package is private, the cluster needs a pull credential of its
own before any of this deploys — see [Private repos and
registries](../dev-guide/private-repos-and-registries.md).

## Step 1 — Confirm the zone covers `docs.`

```sh
apprafter target domain list
```

`apprafter.dev` must be listed. **That is the whole requirement — a
subdomain needs no registration of its own, no second zone and no second
certificate.** Registering an apex gives the platform Gateway a *pair*
of `:443` listeners, the apex and a wildcard, both terminating TLS from
the one imported certificate, and the certificate is minted for `<zone>`
and `*.<zone>` together.

### Check that yourself rather than taking it on trust

Two commands, each reading the live cluster:

```sh
kubectl get gateway platform -n apprafter-system \
  -o jsonpath='{range .spec.listeners[*]}{.name}{"\t"}{.hostname}{"\n"}{end}'
```

Expect an apex listener and a wildcard listener for the zone, alongside
the plain `http` one that redirects to HTTPS:

```text
https-apex-apprafter-dev	apprafter.dev
https-wild-apprafter-dev	*.apprafter.dev
http
```

Then the names the certificate actually carries, which the CLI records
on the Secret when it imports one:

```sh
kubectl get secret cf-origin-cert-apprafter-dev -n apprafter-system \
  -o jsonpath='{.metadata.annotations.apprafter\.io/cert-sans}{"\n"}'
```

Expect both names — `apprafter.dev,*.apprafter.dev`. A certificate
carrying only the apex leaves the wildcard listener presenting something
that does not match `docs.`, and Cloudflare answers **526**.

**The wildcard is one level deep, in both layers.**
`docs.apprafter.dev` is a single label under the apex and is covered.
`a.b.apprafter.dev` is two, and is covered by neither the listener nor
the certificate — it would need its own zone and its own certificate.

### If the zone is not listed

Mint and import the certificate, then register the apex — the full
procedure is [Cloudflare Origin CA
certificate](cloudflare-origin-cert.md), and it is one certificate per
registrable zone:

```sh
apprafter target cert import cf-origin-cert-apprafter-dev \
  --cert ./origin.pem --key ./origin.key
apprafter target domain add apprafter.dev --cert cf-origin-cert-apprafter-dev
```

Two things that catch people here:

- **Register the apex, not the subdomain.** `apprafter target domain
  add` takes a registrable domain and refuses a `*.` prefix; the
  wildcard listener is generated from the apex. Adding
  `docs.apprafter.dev` as a zone of its own is not the way to get a
  subdomain served.
- **Mint the certificate for both names.** Cloudflare's Origin CA form
  takes a hostname list; enter `apprafter.dev` **and** `*.apprafter.dev`.
  Rotating a certificate that already covers both is
  `apprafter target cert import … --replace`.

## Step 2 — The DNS record

This step is in the Cloudflare dashboard, in the account that holds the
zone. Nothing in the repository can do it for you.

In **DNS → Records** for `apprafter.dev`, add:

| Type | Name | Value | Proxy status |
| --- | --- | --- | --- |
| CNAME | `docs` | `apprafter.dev` | **Proxied** (orange cloud) |

Pointing the subdomain at the apex keeps the node's address in one
record, so a re-provision that changes the IP is a single edit. The
explicit alternative is an `A` + `AAAA` pair carrying the values
`apprafter target ip` prints — equivalent, and two more places to update.

**Proxied is not optional here.** If the cluster has the origin firewall
on (`apprafter target firewall cloudflare-origin enable`), the node
accepts `80`/`443` only from Cloudflare's ranges, so a DNS-only
record — grey cloud — resolves to an address that will refuse the
connection. Leave the zone's SSL/TLS mode at **Full (strict)**, which
[Connect a domain](connect-a-domain.md) sets once per zone.

### Verify it

```sh
dig +short docs.apprafter.dev
```

A proxied record answers with **Cloudflare anycast addresses, not the
node's IP** — that is the tell that the orange cloud is on. Seeing the
address `apprafter target ip` prints means the record is grey-clouded;
go back and proxy it.

Cloudflare serves the edge as soon as the record is saved, so there is
no nameserver-propagation wait at this point — that wait happened when
the zone was first connected.

## Step 3 — Register the application, and watch the sync

Registering it once is all that is needed; everything after that is
automatic.

```sh
apprafter app add https://github.com/AppRafter/apprafter \
  --name docs --path docs-site --branch master
```

Three flags, each load-bearing:

- **`--path docs-site`** scopes what gets rendered. The default is the
  repository root, and from there the plugin walks the whole tree for
  manifests. This repository carries a good many besides this one — the
  landing site's, the packaged examples, the end-to-end fixtures — and a
  root registration sweeps in every one of them.
- **`--branch master`** because that is this repository's default branch.
  With an explicit URL and no local checkout to read a branch from, the
  CLI assumes `main`, which does not exist here.
- **`--name docs`** matches the name in the manifest, so every later
  command takes the same word.

The destination namespace defaults to `apprafter`, which is what the
manifest declares — no flag needed. **Do not pass `--env dev`**: the
manifest's `dev` environment is deliberately one replica and *internal*,
with no public route, so a `dev` registration deploys a site nobody
outside the cluster can reach.

### What happens on its own

Argo CD fetches the repository, the CUE plugin renders
`docs-site/apprafter/Application.cue` into an AppRafter `Application`
resource, and the operator turns that into a Deployment, a Service and
an HTTPRoute attached to the Gateway's `:443` listeners. You do not
apply any of those by hand.

Watch it:

```sh
apprafter app status docs --resources
```

`--resources` is the flag worth reaching for: it lists the workload pods
with their states, which is where an image-pull or crash-loop failure
shows up. The application-level "Healthy" can go green before the pods
are actually running.

For the AppRafter resource itself, **spell the group out**. Argo CD
installs a resource also called `applications` in the same cluster, and
a bare `kubectl get applications` does not report the collision — it
silently resolves to one of the two groups and hides everything in the
other. Which one it picks is not worth relying on;
`applications.apprafter.io` and `applications.argoproj.io` always mean
what they say:

```sh
kubectl get applications.apprafter.io docs -n apprafter \
  -o jsonpath='{.status.phase}{"\n"}'
```

### The public-route verdict

The operator records what became of the public route, and this is the
single most useful line when the site does not answer:

```sh
kubectl get applications.apprafter.io docs -n apprafter \
  -o jsonpath='{range .status.conditions[?(@.type=="PublicRouteReady")]}{.status}{"\t"}{.reason}{"\t"}{.message}{"\n"}{end}'
```

| Reason | What it means | What to do |
| --- | --- | --- |
| `Accepted` | The Gateway accepted the route and resolved its backend. This is the good one. | Nothing. |
| `Pending` | The route was applied; the Gateway has not accepted it yet. | Wait a few seconds and re-read. If it persists, check the Gateway exists — it is only created once a zone is registered. |
| `NoMatchingZone` | The hostname is under no registered zone. | Step 1. |

It is informational and never blocks: the route is written either way,
so registering the zone afterwards attaches it with no need to
re-register the application.

### Then the site itself

```sh
curl -sSI https://docs.apprafter.dev/ | grep -Ei '^(HTTP|server):'
```

A `200` on a response whose `server:` is `cloudflare` means all three
steps landed: DNS resolves through the edge, the edge trusts the origin
certificate, and the Gateway routed the host to these pods.

## Later documentation changes deploy themselves

Once this is done, publishing is not a step anyone takes. A
documentation change merged to the default branch republishes
`:latest`, and the operator re-resolves that tag to its current registry
digest on every reconcile ([ADR 0040](../adr/0040-image-digest-resolution.md)),
so a moved tag rolls the Deployment without a manifest edit, a
re-registration, or a visit to this page.

To hold the site at a known build instead, point
`Application.spec.base.image` at the immutable `:<git sha>` the same
workflow publishes, and move it deliberately.

## When it does not work

| Symptom | Where to look |
| --- | --- |
| `PublicRouteReady` says `NoMatchingZone` | Step 1 — the zone is not registered, or the hostname is more than one label deep. |
| Cloudflare **521**/**522**, or the origin times out | The origin firewall and the record's proxy status: [Troubleshooting → public domain](troubleshooting.md#dns). |
| Cloudflare **526** | The certificate does not cover `*.apprafter.dev`, or SSL/TLS mode is not Full (strict). Step 1's second check. |
| Pods stuck on `ImagePullBackOff` | The image is private and the cluster has no pull credential: [Troubleshooting → registry auth](troubleshooting.md#registry-auth). |
| Applications nobody asked for appeared alongside it | The registration was made without `--path docs-site`, so the whole repository was rendered. Remove it with `apprafter app remove` and register again. |
| Argo CD never syncs at all | The repository connection: [Connect a Git repository](connect-a-git-repository.md). |

## For contributors

The one-label rule above is not a convention this page invented; it is
asserted in two places, and both carry tests:

- `cli/platform-cli/src/commands/target_domain.rs` decides which
  applications a zone covers, for `apprafter target domain list` and for
  the check that blocks removing a zone still in use.
- `operator/operator-controllers/application/src/lib.rs` decides the
  `PublicRouteReady` verdict, and is what emits `NoMatchingZone`.

The listener pair itself is generated by the platform chart, in
`platform-stack/cue/render_tool.cue` — one apex listener and one
wildcard listener per registered zone, both referencing the same
imported certificate.

Before changing the manifest, validate it the way the cluster will:

```sh
apprafter app validate docs-site/apprafter/Application.cue
```

The publication decision itself — why the site is built outside the
Dockerfile, and why these three steps stayed manual — is
[ADR 0057](../adr/0057-documentation-system.md).
