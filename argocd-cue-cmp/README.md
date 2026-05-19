# argocd-cue-cmp

> Argo CD Config Management Plugin sidecar for compiling user
> application CUE manifests into Kubernetes-compatible YAML at
> Argo CD sync time. See [ADR 0029](../docs/adr/0029-cue-cmp.md)
> for the design rationale.
>
> **Status:** scaffold landed in plan.md sub-phase 1.69. Image
> publishes to `ghcr.io/<owner>/argocd-cue-cmp:<version>` via
> `.github/workflows/argocd-cue-cmp-publish.yml`.

## What it does

Argo CD does not understand CUE natively — it speaks Helm,
Kustomize, Jsonnet, and raw YAML directories. When a user
application repository contains `apprafter*.cue`, this sidecar
takes over the render step: `cue export ./... --out yaml`
runs inside the sidecar, the YAML stream feeds into Argo CD's
standard sync pipeline.

Users see the same workflow as raw YAML: edit, commit, push.
The CMP activates conditionally — repositories without `*.cue`
files are unaffected.

## Layout

```
argocd-cue-cmp/
├── VERSION         # single source of truth for image version
├── Dockerfile      # Alpine + cue + plugin.yaml + entrypoint
├── plugin.yaml     # ConfigManagementPlugin manifest
├── entrypoint.sh   # cue-export wrapper with structured errors
└── README.md       # you are here
```

The image is published as `ghcr.io/<owner>/argocd-cue-cmp:<version>`
where `<version>` is the contents of `VERSION` (single line,
no leading `v`).

## Local build + smoke test

```sh
# Build with the current VERSION file's tag:
docker build \
    --build-arg IMAGE_VERSION="$(cat argocd-cue-cmp/VERSION)" \
    --build-arg IMAGE_REVISION="$(git -C . rev-parse --short HEAD)" \
    -t argocd-cue-cmp:dev \
    argocd-cue-cmp/

# Sanity: cue binary present and runnable.
docker run --rm --entrypoint /usr/local/bin/cue argocd-cue-cmp:dev version

# Smoke: feed a tiny CUE document and verify YAML output.
mkdir -p /tmp/cue-smoke/apprafter
cat > /tmp/cue-smoke/apprafter/Application.cue <<'EOF'
package app
apiVersion: "apprafter.io/v1alpha1"
kind:       "Application"
metadata: {
    name:      "hello"
    namespace: "default"
}
spec: image: "nginxdemos/hello:plain-text"
EOF

docker run --rm \
    -v /tmp/cue-smoke:/repo \
    -w /repo \
    --entrypoint /usr/local/bin/entrypoint.sh \
    argocd-cue-cmp:dev
```

Expected stdout: well-formed YAML with `apiVersion:
apprafter.io/v1alpha1`, `kind: Application`, etc.

To exercise the error path, introduce a CUE error
(e.g. `spec: image: 42`) and re-run; the script prints
`::cue-cmp:: CUE compile failed: …` to stderr with a
single-line summary, then dumps the full cue stderr below.

## Releasing

The image follows its own semver track (`argocd-cue-cmp/v*`
git tags) independent of the AppRafter monorepo (`v0.x.y`)
and the platform-stack chart (`platform-stack/v*`):

1. Bump `argocd-cue-cmp/VERSION` (single-line edit).
2. Open PR → `argocd-cue-cmp-check.yml` lints the Dockerfile,
   builds the image as a smoke test, runs the entrypoint
   against a fixture, refuses to merge if `argocd-cue-cmp/v<VERSION>`
   already exists on origin AND any file under
   `argocd-cue-cmp/` differs from that tag (drift).
3. Merge to master → `argocd-cue-cmp-publish.yml` detects
   the bump, builds + pushes + cosign-signs the image, then
   creates the `argocd-cue-cmp/v<VERSION>` tag on origin
   via `gh release create` (same trigger-inversion model as
   `platform-stack-publish.yml`).

The platform-stack chart's
[`component_argocd-cue-cmp.cue`](../platform-stack/cue/component_argocd-cue-cmp.cue)
pins the cue-cmp image tag the chart will inject as the
argocd-repo-server sidecar. Bumping cue-cmp without bumping
the chart means existing chart releases still reference the
older sidecar tag — that's the contract; chart releases
choose explicitly which sidecar version they ship.

## Related

- [ADR 0029](../docs/adr/0029-cue-cmp.md) — design rationale.
- [`platform-stack/`](../platform-stack/) — the umbrella
  chart that wires this sidecar into argocd-repo-server's
  `extraContainers`.
- Phase 1.11 (golden-path template) — generates
  `apprafter/Application.cue` in new user app repositories,
  which the CMP then renders at Argo CD sync time.
