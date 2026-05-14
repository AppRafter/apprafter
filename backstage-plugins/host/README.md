# Backstage host app for AppRafter tier-1

Operators of an AppRafter cluster who want a Backstage UI run a
short scaffold-build-push loop *once*, then point
`spec.backstage.image` in their `Infrastructure` manifest at the
resulting image. From there `apprafter cluster-bootstrap`
applies the v0.1.18 manifest set
(`manifests/tier-1/backstage/example.yaml`) into the `backstage`
namespace and the UI shows up at `https://<spec.backstage.domain>`.

This directory ships the build-side helpers — a reference
multi-stage Dockerfile, a matching `.dockerignore`, and a one-shot
`scripts/scaffold.sh` wrapper around `@backstage/create-app`.
We deliberately don't vendor the Backstage app itself: it lives
in your bootstrap repo (the one named in
`spec.argocd.bootstrapRepo`), and we leave you free to upgrade
Backstage on your cadence.

## Requirements

- Node.js 20+ on `$PATH` (Backstage 1.x minimum).
- `npm`/`npx` (or `yarn`) on `$PATH`.
- Network access to npm.
- Docker (or `nerdctl`/`podman` with a Docker-compat alias) for
  the build step.

## Workflow

```sh
# 1) Scaffold the Backstage app (from anywhere — relative paths
#    are fine). The script refuses to overwrite a non-empty target.
./scripts/scaffold.sh ./host-app

# 2) Install deps with the package manager you prefer:
cd host-app
yarn install
#   or:
#   npm install

# 3) Build the image. The Dockerfile + .dockerignore the script
#    dropped expect the build context to be the app root.
docker build -t ghcr.io/<your-org>/backstage:0.1.0 .

# 4) Push to your registry:
docker push ghcr.io/<your-org>/backstage:0.1.0

# 5) Wire it into your Infrastructure manifest:
#    spec.backstage:
#      domain: backstage.example.com
#      image:  ghcr.io/<your-org>/backstage:0.1.0

# 6) Apply via cluster-bootstrap (or commit to the bootstrap repo
#    so Argo CD picks it up):
APPRAFTER_MANIFEST=examples/infrastructure/tier-1-hetzner.cue \
  apprafter cluster-bootstrap
```

## Where the pieces live

| Artefact | Path |
| --- | --- |
| Multi-stage build | [`Dockerfile`](./Dockerfile) |
| Build-context filter | [`.dockerignore`](./.dockerignore) |
| Scaffold wrapper | [`scripts/scaffold.sh`](./scripts/scaffold.sh) |
| Cluster-side YAML (rendered) | [`manifests/tier-1/backstage/example.yaml`](../../manifests/tier-1/backstage/example.yaml) |
| Cluster-side YAML (Rust builder) | [`cli/cli-providers/src/k8s/backstage_manifests.rs`](../../cli/cli-providers/src/k8s/backstage_manifests.rs) |

## What's still missing

OAuth lands in v0.1.20: until then the Backstage UI runs with
`auth.providers.guest.dangerouslyAllowOutsideDevelopment: true`
or whatever the create-app default ships. The
`manifests/tier-1/backstage/example.yaml` doesn't yet mount an
`app-config.yaml` ConfigMap — you bake your own config into the
image for now.
