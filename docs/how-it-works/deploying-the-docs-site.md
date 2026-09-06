---
description: "A worked example of one application on AppRafter: what builds the documentation site's image, what deploys it, where its hostname and certificate come from, and what its manifest deliberately leaves out."
---

# Worked example: deploying this documentation site

The site you are reading runs as an ordinary AppRafter application on an
AppRafter cluster. Nothing about it is privileged — no operator support, no
special case in the platform — so it is a complete example of the shape a
static web application takes here, with every part named by its real path in
this repository.

Read it beside your own deployment. The
[operator quickstart](../operator-guide/quickstart.md) and
[Add your application](../dev-guide/build-and-push.md) are the recipes; this is
one instance of them, assembled.

## What is responsible for what

| Link in the chain | Where it lives | What it decides |
| --- | --- | --- |
| The content | `docs/**` | every published page, plus `mkdocs.yml` for nav, theme and the build hooks |
| The build | `.github/workflows/release-docs.yml` | runs `mkdocs build --strict` under the flake's pinned toolchain, so the published bytes come from one environment |
| The gate | the same workflow, before the build | the drift gate and the artefact check run on the very commit that publishes; a red gate publishes nothing |
| The image | `docs-site/Dockerfile` | `caddy:2-alpine` plus the built `site/` tree and a `Caddyfile`. `caddy validate` runs at build time, so a broken config fails the build rather than the pod |
| The serving config | `docs-site/Caddyfile` | cache headers, content types, and the `:80` the container listens on |
| The registry | `ghcr.io/apprafter/docs` | two streams: `:latest`, which the manifest watches, and `:<git sha>`, immutable, for a rollback or a pinned rollout |
| The deployment | `docs-site/apprafter/Application.cue` | image, replicas, the port, and the hostname |
| The reconcile | Argo CD, registered once with `apprafter app add` | pulls the manifest, compiles it, applies it |
| The rollout | the AppRafter operator | resolves `:latest` to its current digest each reconcile, so a re-pushed tag rolls with no manifest edit |
| TLS and the hostname | the platform Gateway | `docs.apprafter.dev` is a subdomain of an already-registered zone, so it needs no new certificate |
| DNS | the operator's Cloudflare account | the one part of the chain that is not in this repository |

## The manifest, in full

```cue
--8<-- "docs-site/apprafter/Application.cue"
```

## The serving config

```caddyfile
--8<-- "docs-site/Caddyfile"
```

## What it deliberately does not have

The absences are the more instructive half of a worked example, because each
one is a decision someone could copy without meaning to.

**No `needs`.** The site is static: Caddy serves a tree baked into the image
and nothing is read or written at runtime. There is no database, cache, bucket
or disk to declare, so declaring one would provision a backend that nothing
connects to.

**No `env`.** Nothing is configurable at runtime either. The single build-time
input — `site_url` — is baked in by `mkdocs build`, because it is compiled into
every absolute URL in `sitemap.xml` and in the LLM artefacts.

**No `resources`.** Not an oversight and not a default worth copying: there is
no measurement to set them from. A request invented here would be a number
nobody re-derived. Take one off a running pod first — see
[Right-sizing an application's requests](resources-and-autoscaling.md).

**No `imagePolicy`.** Its default already resolves the tag to a digest on every
reconcile, which is exactly the wanted behaviour. Restating a default is a
second place to change it.

**No `:prod` stream.** The landing has one, because a human promotes CMS
content that never passed CI. Documentation is git-resident and gated on the
same commit that publishes it, so master *is* the promoted state and a second
stream would be a manual step with nothing left to decide.

## The one string that is copied

The workflow's image base and the manifest's `image` are two copies of one
value. A mismatch between them deploys nothing and reports success — the
workflow pushes, Argo CD syncs a manifest pointing elsewhere, and every status
is green. They are compared literally whenever either moves, and that is the
failure mode to watch for in your own deployment too.

## See also

- [The image digest](the-image-digest.md) — why a re-pushed `:latest` rolls at
  all.
- [Connect a domain](../operator-guide/connect-a-domain.md) — registering the
  zone whose wildcard listener serves this subdomain.
- [Publishing this site](../operator-guide/publish-the-docs-site.md) — the
  one-time steps that first put it on the internet.
