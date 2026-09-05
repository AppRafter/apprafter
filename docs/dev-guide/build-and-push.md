---
description: "What the platform requires of your container image, a worked multi-stage Dockerfile, and the CI job that pushes the moving tag a deployment follows."
---

# From source to a running image

Two steps of the developer flow that had no page: containerising your
application, and pushing the tag the cluster follows. They are one continuous
story — the image is the input the platform takes, and the tag is how a new
build reaches it — so they are one page.

Neither half is AppRafter's to own. [ADR 0010](../adr/0010-dockerfile-first-build.md)
makes the image the user's artefact and the platform's input; your CI is your
forge's. What AppRafter owns is the contract between them, which is short, and
this page is mostly about being explicit where it has been implicit.

## What the platform requires of an image

Four things, and nothing else:

| Requirement | Why | What happens if not |
| --- | --- | --- |
| One listening port, matching `expose.port` in your manifest | The Service and any HTTPRoute target that port by number | Traffic reaches a port nothing is listening on; the pod is Running and the app is unreachable |
| Runs as a non-root user | Pod security, and the platform sets no `runAsUser` for you | Refused on a restricted namespace, or runs with more privilege than it needs |
| No build-time secrets baked into layers | An image layer is not a secret store, and a registry copy is a permanent copy | The secret is readable by anyone who can pull the image, including after you rotate it |
| A tag your CI re-pushes, or an immutable one you name explicitly | The rolling deploy watches the tag ([Image iteration](image-iteration.md)) | Nothing rolls: a tag that never moves has nothing for the platform to act on |

AppRafter does **not** build your image. `apprafter app scaffold` detects your
runtime to write a manifest; it writes no Dockerfile, and there is no build
step in the platform. That boundary is deliberate — the alternative is
maintaining an opinion about ten ecosystems' build tooling.

## A worked example

This is the Dockerfile from `examples/templates/bun-http`, included from the
file the repository actually ships rather than copied — so it cannot drift
from the template it claims to be:

```dockerfile
--8<-- "examples/templates/bun-http/skeleton/Dockerfile"
```

Three things in it generalise past Bun:

- **Multi-stage.** The builder carries the toolchain; the runtime carries the
  output and nothing else. It is the difference between a ~1 GB image and a
  ~30 MB one, and the smaller one has less in it to have a vulnerability.
- **Lockfile and manifest copied before sources.** The dependency layer is then
  cached across every commit that does not change dependencies, which is most
  of them.
- **A non-root runtime user.** `distroless/*:nonroot` gives you this by
  default; on a `FROM alpine` or `FROM debian` base you write the `USER` line
  yourself.

For other runtimes, follow the ecosystem's own guidance rather than a recipe
here — a Dockerfile that has drifted a year behind upstream advice is worse
than a link:

| Runtime | Where the current advice lives |
| --- | --- |
| Node | [Docker's Node.js guide](https://docs.docker.com/guides/nodejs/) |
| Python | [Docker's Python guide](https://docs.docker.com/guides/python/) |
| Go | [Docker's Go guide](https://docs.docker.com/guides/golang/) |
| Rust | [Docker's Rust guide](https://docs.docker.com/guides/rust/) |
| Java | [Docker's Java guide](https://docs.docker.com/guides/java/) |

## Pushing from CI

The platform pulls; your CI pushes. The whole contract is that a tag your
manifest names gets a new digest, and the rest follows on its own.

Push **two** tags for every build:

- a **moving** tag your manifest names — `latest`, or a branch name. Re-pushing
  it is what triggers the rollout.
- an **immutable** tag nothing else will ever point at — the commit SHA. This
  is what you roll back *to*, and a build with no immutable tag cannot be
  returned to once the moving tag has passed it.

GitHub Actions, as the one worked instance:

```yaml
name: build
on:
  push:
    branches: [main]

permissions:
  contents: read
  packages: write

jobs:
  image:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: docker/login-action@v3
        with:
          registry: ghcr.io
          username: ${{ github.actor }}
          password: ${{ secrets.GITHUB_TOKEN }}
      - uses: docker/build-push-action@v6
        with:
          context: .
          push: true
          # The moving tag the manifest names, and the immutable one a
          # rollback returns to. Both, every build.
          tags: |
            ghcr.io/${{ github.repository }}:latest
            ghcr.io/${{ github.repository }}:${{ github.sha }}
```

The same three steps — log in, build, push both tags — are what any runner
needs; nothing above depends on GitHub beyond the login and the `GITHUB_TOKEN`.

**A private registry needs credentials in the cluster**, registered once with
`apprafter repo creds add`. See
[Private repos & registries](private-repos-and-registries.md#ghcr-token-scopes)
for which token scopes a pull needs, which differ from the ones a Git clone
needs.

## What happens after the push

Nothing you have to do. The cluster resolves the moving tag to its digest,
notices the digest changed, and rolls the deployment — see
[Image iteration](image-iteration.md) for how to confirm it happened and how to
opt out. If the new build is wrong,
[Rolling back a bad deploy](rollback.md) is the way back, and it is the
immutable tag above that makes it possible.

## See also

- [Writing Application.cue](application-cue.md) — the manifest that names the
  tag this page pushes.
- [Image iteration](image-iteration.md) — what the platform does with it.
- [Private repos & registries](private-repos-and-registries.md) — credentials
  for a private source repository and a private registry.
