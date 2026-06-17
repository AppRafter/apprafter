# ${{ values.name }}

${{ values.description }}

Generated from the AppRafter `bun-http` golden-path template
([source](https://github.com/apprafter/apprafter/tree/master/examples/templates/bun-http)).
Built on [OneBun](https://github.com/RemRyahirev/onebun).

## Develop

```sh
bun install
bun run dev          # http://localhost:3000
curl http://localhost:3000/api/health
```

## Build the container

```sh
docker build -t ${{ values.image }} .
docker push ${{ values.image }}
```

## Deploy

`apprafter/Application.cue` is already wired with your
`${{ values.image }}` image. Commit this whole repo into the Git
repo Argo CD watches (set as
`Infrastructure.spec.argocd.bootstrapRepo` when running
`apprafter cluster-bootstrap`); Argo CD will sync, the
AppRafter operator will reconcile the manifest into a Deployment +
Service, and `${{ values.name }}` becomes reachable cluster-internally
at `http://${{ values.name }}.${{ values.namespace }}.svc.cluster.local:80`.

## What's next

- Add controllers under `src/` and register them in `src/app.module.ts`.
- Extend `src/config.ts` with new env keys; the typed `this.config.get()`
  surface picks them up automatically.
- Add per-environment overrides in `apprafter/Application.cue`'s
  `spec.environments` map (image / replicas / env vary per env;
  the AppRafter operator's `APPRAFTER_ENV` env var picks the
  active one).
