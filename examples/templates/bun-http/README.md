# bun-http-starter

AppRafter golden-path starter — minimal HTTP service built on
[OneBun](https://github.com/RemRyahirev/onebun) (Bun.js +
Effect.ts, decorator-driven DI). Ships with a health controller,
typed env config, an `apprafter/Application.cue` v1alpha1 manifest,
and a multi-stage Dockerfile that produces a distroless runtime
image.

## What's in the box

| File / dir                        | Purpose                                                             |
| --------------------------------- | ------------------------------------------------------------------- |
| `src/index.ts`                    | `OneBunApplication` bootstrap (binds routes, metrics, tracing).      |
| `src/app.module.ts`               | Root `@Module` — register controllers + providers here.              |
| `src/health.controller.ts`        | `@Controller('/api')` with `/health` + `/ready`.                     |
| `src/config.ts`                   | `envSchema` + `InferConfigType` module augmentation.                 |
| `Dockerfile`                      | Multi-stage `oven/bun:1-debian` builder → `distroless/nodejs:nonroot`. |
| `apprafter/Application.cue`       | v1alpha1 Application manifest — drop into your bootstrap repo.        |

## Develop

```sh
cd examples/templates/bun-http
bun install
bun run dev          # starts on http://localhost:3000
curl http://localhost:3000/api/health
# → {"success":true,"result":{"status":"healthy","timestamp":"…"}}
```

`PORT` (default 3000) and `HOST` (default 0.0.0.0) override the
listener.

## Build the container

```sh
docker build -t ghcr.io/your-org/bun-http-starter:0.1.0 .
docker push ghcr.io/your-org/bun-http-starter:0.1.0
```

The Dockerfile bundles to a Node-compatible CommonJS file via
`bun build --target node` so the runtime layer is
`distroless/nodejs20-debian12:nonroot` — no Bun shipped in the
image, ~30 MB final size.

## Deploy via AppRafter

1. Replace the placeholder `image` in `apprafter/Application.cue`
   with the tag you just pushed.
2. Commit the file into the Git repo Argo CD watches (the one you
   set as `Infrastructure.spec.argocd.bootstrapRepo` when running
   `platform-cli cluster-bootstrap`).
3. Argo CD picks the change up; the operator reconciles the
   Application; `/api/health` becomes reachable cluster-internally
   at `http://bun-http-starter.default.svc.cluster.local:80`.

## Per-environment overrides

`spec.environments.prod.replicas: 3` in the example manifest pins
3 replicas in production. The AppRafter operator picks the active
environment from `APPRAFTER_ENV` (set via the operator's chart
values or pod env). Add `staging`, `dev`, etc. by adding more keys
under `spec.environments`.

## What's still pending

- Backstage Software Template (`template.yaml`) so operators can
  scaffold this starter from the Backstage UI — lands in v0.1.38
  (sub-phase 1.11b, closes phase 1.11).
- Quickstart doc at `docs/dev-guide/quickstart.md` — also v0.1.38.
