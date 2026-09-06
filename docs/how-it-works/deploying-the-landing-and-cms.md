---
description: "A worked example of a two-application deployment with a database and a human promotion step: what builds each image, how content reaches production without going through Git, and which parts are deliberately not automated."
---

# Worked example: deploying the landing site and its CMS

Two applications on one cluster, sharing a namespace: a static marketing site
and the Payload CMS that supplies its content. It is a more instructive example
than [the documentation site](deploying-the-docs-site.md) because it has the
three things that one does not — a declared database, a second copy of the same
application at a different hostname, and a change path that does not go through
Git at all.

## What is responsible for what

| Link in the chain | Where it lives | What it decides |
| --- | --- | --- |
| The site source | `landing/web/` | Astro pages, components, and a fallback copy of every CMS-backed field |
| The CMS source | `landing/cms/` | Payload collections and globals — the schema the editor edits against |
| The code build | `.github/workflows/release-landing.yml` | builds and pushes both images when the *code* changes |
| The content build | `.github/workflows/landing-preview-build.yml` | rebuilds the site when the *content* changes, triggered by Payload itself on save |
| The promotion | `.github/workflows/landing-promote-to-prod.yml` | retags the inspected preview image to `:prod` and `:latest` — the human step |
| The content gate | `.github/workflows/landing-content-check.yml` | asserts the live site agrees with the tracked fallback |
| The version tag | `.github/workflows/landing-autotag.yml` | bumps a `landing-v*` tag on every push touching `landing/**` |
| The web deployment | `landing/web/apprafter/Application.cue` | `apprafter.dev`, two replicas in prod |
| The preview deployment | `landing/web/apprafter/Application-preview.cue` | the same application, one replica, `network: internal` |
| The CMS deployment | `landing/cms/apprafter/Application.cue` | `cms.apprafter.dev`, one replica, and a `needs.pg` |
| The database | declared, not deployed | `needs: pg: {}` — the provisioner creates it, and the connection details arrive as a claim |

## Content does not travel through Git

This is the part worth studying. The documentation site is git-resident: every
published byte passed the gate on the commit that published it, so `master` is
the promoted state and there is nothing left to decide.

The landing is not. An editor saves a global in the Payload admin, and that
save is a change to production content that no CI run has ever seen. So the
chain has a step the documentation site does not need:

1. Payload's `afterChange` hook triggers the preview build.
2. The preview image is deployed to an **internal** host — reachable from the
   cluster, not from the internet.
3. A human looks at it.
4. Payload's `promoteToProd` hook triggers the promotion, which **retags the
   image bytes already inspected** rather than rebuilding.

Step four is why the promotion retags rather than rebuilds: a rebuild could
pick up a content change made between the inspection and the promotion, and
then what reaches production is not what anyone looked at.

## The database is declared, not configured

```cue
needs: {
    pg: {}
}
env: {
    DATABASE_URL: claim.pg.url
}
```

That is the whole of it. The provisioner creates the database and a role for
it, writes the connection details into a Secret, and the `claim.pg.url`
reference becomes a `secretKeyRef` in the rendered Deployment — so no credential
appears in this manifest or in Git. The mechanism is
[Declared Postgres dependencies](needs-pg.md).

## The manifests, in full

The production site:

```cue
--8<-- "landing/web/apprafter/Application.cue"
```

The CMS:

```cue
--8<-- "landing/cms/apprafter/Application.cue"
```

## What each one deliberately does not have

**The CMS runs one replica, and that is a constraint rather than a choice.**
Payload holds session state in the process, so a second replica without shared
session storage would sign editors out at random. Making it highly available is
real work, not a `replicas: 2`.

**The web application has no `needs`.** It is static at serve time: the content
is fetched at *build* time and baked into the image, which is why a content
change requires a rebuild rather than a restart, and why the site keeps serving
if the CMS is down.

**The preview host is `network: internal`.** It has no public route, which is
what makes it safe to point at unreviewed content.

## See also

- [Worked example: deploying this documentation site](deploying-the-docs-site.md)
  — the simpler shape, with no database and no promotion step.
- [Declared Postgres dependencies](needs-pg.md) — what `needs: pg: {}` sets in
  motion.
- [The image digest](the-image-digest.md) — why retagging is enough to make a
  deployment roll.
