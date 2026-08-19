---
description: "The five steps outside the repository that put this documentation site on docs.apprafter.dev — registry access, the zone, the Cloudflare record, the one registration, and the link on the front door."
---

# Publish the documentation site

This is the runbook for putting **this site — the one you are reading —**
on `https://docs.apprafter.dev/`. The repository ships the image, the
manifest and the release workflow; five steps are outside it and belong
to whoever runs the cluster, holds the Cloudflare account and edits the
public site. This page is those five steps, in order, with how to tell
each one worked.

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
| `.github/workflows/release-docs.yml` | Builds under Nix, smoke-tests the image, and pushes `ghcr.io/apprafter/docs`, on a documentation change merged to the default branch. |

Nothing below edits any of them. Step 5 is the one step that does edit
the repository, and it edits the **landing**, not this list.

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

## Step 1 — Make sure the cluster can pull the image

**A package published under an organisation for the first time is
private.** Nothing in the release workflow makes it public, and a
private package with no matching credential on the cluster deploys as
`ResolveFailed` on the Application followed by `ImagePullBackOff` on the
pods — after every other step on this page has been done correctly,
which is what makes it worth its own step rather than a footnote.

Ask the only question that decides it: **can an anonymous puller read
this package?** That is exactly the question the cluster's kubelet asks
when it has no credential.

```sh
repo=apprafter/docs   # <org>/<package>, lowercased
tok=$(curl -s "https://ghcr.io/token?service=ghcr.io&scope=repository:${repo}:pull" |
        sed -e 's/.*"token":"\([^"]*\)".*/\1/')
curl -s -o /dev/null -w '%{http_code}\n' \
  -H "Authorization: Bearer ${tok}" \
  -H 'Accept: application/vnd.oci.image.index.v1+json' \
  "https://ghcr.io/v2/${repo}/manifests/latest"
```

- **`200`** — anonymously pullable. Nothing to do; this is how
  `apprafter/landing-web` already runs on this cluster.
- **anything else** (GHCR answers `403` for *private* and for *absent*
  alike, which is why the workflow run is the check for existence and
  this is only the check for reachability) — the cluster cannot pull it
  as it stands. Take one of two actions:

    - **Make the package public**, in the org's *Packages* settings, to
      match `landing-web`. Re-run the probe; it must answer `200`.
    - **Or register a pull credential** covering the registry path:

      ```sh
      apprafter repo creds add apprafter \
          --url-prefix https://github.com/AppRafter
      ```

      For a GitHub org the registry host is inferred
      (`github.com/AppRafter` → `ghcr.io/apprafter`), so one credential
      covers both the clone and the pull — see [Private repos and
      registries](../dev-guide/private-repos-and-registries.md) for the
      token type, which is not a free choice.

Confirm what the cluster actually holds, rather than what you meant to
register — a credential whose prefix does not match this image covers
nothing, and there is no error that says so:

```sh
kubectl get sourcecredentials.apprafter.io -n apprafter-system \
  -o custom-columns=NAME:.metadata.name,HOSTS:.spec.registry.hosts
```

One of the listed `HOSTS` prefixes must be a prefix of
`ghcr.io/apprafter/docs`. A cluster serving other AppRafter properties
will already list credentials for *their* orgs; those do not cover this
one.

## Step 2 — Confirm the zone covers `docs.`

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
on the Secret when it imports one.

**The Secret's name is yours, not the platform's** — you chose it at
`apprafter target cert import <name>`, and the chart only copies it into
both listeners. So read the name off the cluster instead of typing one:
the `Cert` column of the `apprafter target domain list` above prints it,
and this derives it from the wildcard listener that will actually serve
`docs.`:

```sh
cert=$(kubectl get gateway platform -n apprafter-system \
  -o jsonpath='{.spec.listeners[?(@.hostname=="*.apprafter.dev")].tls.certificateRefs[0].name}')
echo "${cert:-<no wildcard listener for this zone>}"

kubectl get secret "$cert" -n apprafter-system \
  -o jsonpath='{.metadata.annotations.apprafter\.io/cert-sans}{"\n"}'
```

**Covered** looks like a name printed for `$cert` and a SAN list holding
both the apex and the wildcard, in either order:

```text
cf-origin-cert-apprafter
*.apprafter.dev,apprafter.dev
```

**Not covered** has two distinguishable shapes, and they need different
fixes:

- `$cert` prints the placeholder and the second command errors on an
  empty name — there is **no wildcard listener**, so the zone is not
  registered. That is the "If the zone is not listed" case below, and
  `PublicRouteReady` will say `NoMatchingZone`.
- `$cert` prints a name but the SAN list holds only the apex — the zone
  is registered against a certificate **minted for one name**. The
  wildcard listener then presents something that does not match `docs.`
  and Cloudflare answers **526**. Re-mint for both names and rotate with
  `apprafter target cert import … --replace`; nothing about the zone
  registration needs to change.

A missing `apprafter.io/cert-sans` annotation is a third, milder case: a
Secret imported by hand rather than through the CLI carries no SAN
record, so the command prints nothing and tells you only that this check
cannot answer. Read the certificate itself — `kubectl get secret "$cert"
-o jsonpath='{.data.tls\.crt}' | base64 -d | openssl x509 -noout -text` —
or re-import it through the CLI, which writes the annotation.

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
apprafter target cert import cf-origin-cert-apprafter \
  --cert ./origin.pem --key ./origin.key
apprafter target domain add apprafter.dev --cert cf-origin-cert-apprafter
```

The Secret name here is an example and yours to choose; whatever you
pass to `import` is what you pass to `--cert`, and what the check above
reads back off the Gateway.

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

## Step 3 — The DNS record

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

## Step 4 — Register the application, and watch the sync

Registering it once is all that is needed; everything after that is
automatic.

**Run this from the `docs-site` directory of a checkout**, not from
anywhere else:

```sh
git clone https://github.com/AppRafter/apprafter   # or use a checkout you have
cd apprafter/docs-site

apprafter app add https://github.com/AppRafter/apprafter \
  --name docs --path docs-site --branch master --no-interactive
```

The working directory is part of the step, not a detail. Before it does
anything else, `apprafter app add` looks for
`apprafter/Application.cue` **in the current directory** — and reacts to
its absence in a way that depends on whether it has a terminal. From a
directory without one it either refuses outright (no TTY:
`× apprafter/Application.cue not found in …`) or, on a TTY, opens the
scaffold wizard and writes **a new, unrelated manifest named after the
directory you happened to be standing in**. Run from a repository root,
that means a stray manifest committed into someone's tree.

`--no-interactive` is what makes the printed command behave as this page
describes: it takes every value from the flags and asks nothing.
Without it, on a terminal, the same command opens a wizard — which is
fine, but then the flags are defaults to confirm rather than the whole
input. Drop the flag if you would rather step through it.

`--path docs-site` stays as written: it is relative to the **repository
root**, which is what Argo CD renders from, not to your shell's cwd.

The other flags, each load-bearing:

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
| `NoMatchingZone` | The hostname is under no registered zone. | Step 2. |

It is informational and never blocks: the route is written either way,
so registering the zone afterwards attaches it with no need to
re-register the application.

### Then the site itself

```sh
curl -sSI https://docs.apprafter.dev/ | grep -Ei '^(HTTP|server):'
```

A `200` on a response whose `server:` is `cloudflare` means the first
four steps landed: DNS resolves through the edge, the edge trusts the
origin certificate, and the Gateway routed the host to these pods.

## Step 5 — Point the landing page at it

**Do this last, and only once the `curl` above answers `200`.** The
landing page's nav carries a Docs item that renders as a disabled "Soon"
badge while its `docsUrl` is empty and as a live link once it is set.
Setting it before the site answers points the front door at a host that
does not resolve — a worse failure than the badge, because it looks
shipped.

There are **two** values, because the landing renders from two sources:

| Value | Feeds | Set it |
| --- | --- | --- |
| `docsUrl` in the CMS *Site Settings* global | the preview build | in the CMS admin UI |
| `docsUrl` in `landing/web/src/data/fallback/siteSettings.json` | the **released** image, which builds with `LANDING_USE_FALLBACK=1` | as a repository change |

Set both to `https://docs.apprafter.dev/` — the value is not a matter of
taste: it is `mkdocs.yml`'s `site_url`, the origin baked into every
canonical link, sitemap entry and `llms.txt` URL this site publishes,
and its host is `expose.hostname` in the manifest. Seeding the CMS from
the file happens only when the global is empty, so an existing global is
never overwritten and must be edited by hand.

!!! warning "Any commit touching `landing/**` publishes the landing"

    This step is **not** shipped as a held-back commit on the branch that
    added everything else, and could not be. `.github/workflows/landing-autotag.yml`
    fires on any push to the default branch whose diff touches
    `landing/**`; it bumps the patch of the newest `landing-v*` tag,
    pushes it, and dispatches `release-landing.yml`, which publishes
    `ghcr.io/apprafter/landing-web` at both `:<tag>` and `:latest` — and
    the landing's own manifest watches `:latest`, which the operator
    re-resolves to its current digest on every reconcile. So the edit
    **is** the deploy, with no further approval, the moment it merges.
    There is no ordering the repository can enforce on your behalf, which
    is why this is a step here rather than a commit somebody remembers not
    to push.

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
| `PublicRouteReady` says `NoMatchingZone` | Step 2 — the zone is not registered, or the hostname is more than one label deep. |
| Cloudflare **521**/**522**, or the origin times out | The origin firewall and the record's proxy status: [Troubleshooting → public domain](troubleshooting.md#dns). |
| Cloudflare **526** | The certificate does not cover `*.apprafter.dev`, or SSL/TLS mode is not Full (strict). Step 2's second check. |
| Pods stuck on `ImagePullBackOff`, or the Application says `ResolveFailed` | Step 1 — the package is private and the cluster has no credential covering it. Also [Troubleshooting → registry auth](troubleshooting.md#registry-auth). |
| The site answers but the landing still badges Docs "Soon" | Step 5, and note there are two values: the CMS global feeds the preview build, the tracked fallback JSON feeds the released image. |
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

Before changing the manifest, validate it the way the cluster will —
**from the repository root**, since the path is written relative to it
(Step 4 works from `docs-site/`, where the same check is
`apprafter app validate` with no argument at all):

```sh
apprafter app validate docs-site/apprafter/Application.cue
```

The serving layer has its own two checks, and neither is part of
`just lint` — the image build runs `caddy validate`, and
`scripts/docs-site-smoke.sh <image>` starts a built image and probes it:

```sh
nix develop --command mkdocs build --strict --site-dir docs-site/site
docker build -t docs-local -f docs-site/Dockerfile docs-site/
scripts/docs-site-smoke.sh docs-local
```

The publication decision itself — why the site is built outside the
Dockerfile, and why these five steps stayed manual — is
[ADR 0057](../adr/0057-documentation-system.md).
