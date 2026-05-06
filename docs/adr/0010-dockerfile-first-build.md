# ADR 0010: Dockerfile-first build pipeline

## Status

`Accepted`. Date: 2026-05-06.

## Context

Two paths were available for the build pipeline:

1. Heroku-style Buildpacks that auto-detect the language and produce
   an image without a Dockerfile.
2. A Dockerfile written by the application author, with the platform
   adding analysis and policy.

Buildpacks are great when they work; failure modes are opaque and
hard to debug. Most backend developers already know Dockerfile and
`.dockerignore`.

## Decision

The default build pipeline expects a **Dockerfile** in the
application repository. The platform adds:

- Multi-stage build via BuildKit / Buildah / Kaniko (auto-detected).
- Vulnerability scan with Trivy and Grype; CI fails on HIGH severity
  by default (configurable per Application).
- SBOM generation (CycloneDX) via Syft.
- Mandatory Cosign signing for production environments.
- Layer-size and cache-efficiency analysis surfaced in Backstage.

Buildpacks remain available as an opt-in for applications that
prefer them.

## Consequences

Positive:

- Application authors retain full control over what is in the image.
- Failure modes are familiar (it is `docker build` underneath).
- The platform's value-add is transparency (CVEs, SBOM, sizes,
  signatures), not magic.

Negative:

- Authors who would prefer "no Dockerfile" must opt into Buildpacks.
  Acceptable: opt-in is one line in the manifest.

## Alternatives considered

- **Buildpacks-default.** Rejected because of opaque failure modes.
- **Custom platform builder.** Rejected: rebuilding the entire
  ecosystem is not justified by the marginal UX win.

## Risks

- Some Dockerfiles will produce inefficient images. Mitigated by the
  Backstage build report's recommendations (multi-stage, base-image
  upgrades, etc.).

## Owner

Build-pipeline maintainers.

## Re-evaluation

Revisit if Buildpacks evolve to expose the same level of analysis
natively.

## References

- `spec.md` §4.9 and §8 ("Why Dockerfile-first build pipeline (not
  Buildpacks-default)").
