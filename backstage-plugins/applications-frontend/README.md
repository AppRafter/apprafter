# @apprafter/applications-frontend

Backstage frontend plugin that surfaces `apprafter.io/v1alpha1`
Applications. v0.1.35 ships the TypeScript scaffold + types +
`ApplicationsApi` interface + the pure `applicationsToRows` data
transform. v0.1.36 (sub-phase 1.10d, closes phase 1.10) wraps it
in a Backstage `createPlugin` extension with a React table page,
drilldown view, status / events panel, and per-environment tabs.

## Layout

```
backstage-plugins/applications-frontend/
├── package.json
├── tsconfig.json
├── bun.lock
└── src/
    ├── index.ts          # public re-exports
    ├── types.ts          # re-declared mirror of backend types
    ├── api.ts            # ApplicationsApi interface + apiRef id
    ├── transforms.ts     # applicationsToRows pure data transform
    └── transforms.test.ts
```

The `Application` types are **re-declared** here rather than
imported across package boundaries — each plugin can be published
independently, at the cost of hand-syncing the types between
`@apprafter/applications-backend/src/types.ts` and this package's
`src/types.ts`.

## Develop

```sh
cd backstage-plugins/applications-frontend
bun install
bun test            # 5 unit tests against transforms.ts
bun run lint        # tsc --noEmit
```

## Public surface (v0.1.35)

- `Application`, `ApplicationSpec`, `ApplicationBaseSpec`,
  `ApplicationStatus`, `ApplicationCondition`,
  `ApplicationExpose`, `ObjectMeta` — mirror types.
- `ApplicationsApi { listApplications, getApplication }` — the
  contract a Backstage backend impl satisfies.
- `applicationsApiRefId: 'apprafter.applications'` — stable ID;
  v0.1.36 wraps it in a real `ApiRef<ApplicationsApi>` via
  `@backstage/core-plugin-api::createApiRef`.
- `ApplicationRow { name, namespace, image, replicas, phase,
  endpointURL, ready }` — display-ready row shape.
- `applicationToRow`, `applicationsToRows` — pure transforms.

## What's missing

- React component (table + drilldown) → v0.1.36 (sub-phase 1.10d).
- Backstage `createPlugin` glue + routable extension factory →
  v0.1.36.
- Per-environment tabs → v0.1.36 (closes phase 1.10).
