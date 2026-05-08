# @apprafter/applications-frontend

Backstage frontend plugin that surfaces `apprafter.io/v1alpha1`
Applications. v0.1.36 (sub-phase 1.10d, closes phase 1.10) ships
the React components — `ApplicationsTable`, `ApplicationDetail`,
`EnvironmentTabs` — alongside the v0.1.35 types + transforms.
Components are pure props-driven and carry no Backstage runtime
deps; operators wire them into a Backstage page in their host app.

## Layout

```
backstage-plugins/applications-frontend/
├── package.json
├── tsconfig.json
├── bun.lock
└── src/
    ├── index.ts                  # public re-exports
    ├── types.ts                  # re-declared mirror of backend types
    ├── api.ts                    # ApplicationsApi interface + apiRef id
    ├── transforms.ts             # applicationsToRows + applicationToRow
    ├── transforms.test.ts
    ├── helpers.ts                # environmentsOf + applicationsForEnvironment
    ├── helpers.test.ts
    └── components/
        ├── ApplicationsTable.tsx
        ├── ApplicationDetail.tsx
        └── EnvironmentTabs.tsx
```

The `Application` types are **re-declared** from the backend so each
package can be published independently. Hand-sync changes between
`@apprafter/applications-backend/src/types.ts` and this package's
`src/types.ts`.

## Develop

```sh
cd backstage-plugins/applications-frontend
bun install
bun test            # 9 unit tests (5 transforms + 4 helpers)
bun run lint        # tsc --noEmit (also type-checks the .tsx)
```

React rendering isn't covered by `bun test` — Bun lacks a built-in
DOM and adding `happy-dom` / `jsdom` isn't worth the test value
for v0.1.36's tier-1 scope. Components are statically type-checked
by `tsc`; rendering verifies on a real Backstage host.

## Public surface (v0.1.36)

- Types — `Application`, `ApplicationSpec`, `ApplicationBaseSpec`,
  `ApplicationStatus`, `ApplicationCondition`,
  `ApplicationExpose`, `ObjectMeta`.
- `ApplicationsApi { listApplications, getApplication }`.
- `applicationsApiRefId` (string id; consumer wraps it via
  `createApiRef<ApplicationsApi>` — see snippet below).
- `ApplicationRow` + `applicationToRow` + `applicationsToRows` —
  display-ready row shape + transforms.
- `environmentsOf(apps)` + `applicationsForEnvironment(apps, env)` —
  pure helpers for the per-env tab strip.
- `ApplicationsTable` (props: `rows`, optional `onSelect`).
- `ApplicationDetail` (props: `application`).
- `EnvironmentTabs` (props: `environments`, `selected`, `onSelect`)
  — controlled component; consumer owns the selection state.

## Wire it into a Backstage host

In your scaffolded Backstage app:

```ts
// packages/app/src/api.ts
import { createApiRef } from '@backstage/core-plugin-api';
import {
  applicationsApiRefId,
  type ApplicationsApi,
} from '@apprafter/applications-frontend';

export const applicationsApiRef = createApiRef<ApplicationsApi>({
  id: applicationsApiRefId,
});

// packages/app/src/apis.ts (or the equivalent registration site)
import { applicationsApiRef } from './api';
import { fetchApiRef, configApiRef } from '@backstage/core-plugin-api';

createApiFactory({
  api: applicationsApiRef,
  deps: { fetchApi: fetchApiRef, configApi: configApiRef },
  factory: ({ fetchApi, configApi }) => ({
    listApplications: async (namespace?: string) => {
      const url = `${configApi.getString('backend.baseUrl')}/api/applications${
        namespace ? `?namespace=${namespace}` : ''
      }`;
      const res = await fetchApi.fetch(url);
      const { items } = await res.json();
      return items;
    },
    getApplication: async (namespace, name) => {
      const url = `${configApi.getString('backend.baseUrl')}/api/applications/${namespace}/${name}`;
      const res = await fetchApi.fetch(url);
      if (res.status === 404) return null;
      const { application } = await res.json();
      return application;
    },
  }),
});

// packages/app/src/components/applications/ApplicationsPage.tsx
import { useEffect, useState } from 'react';
import { useApi } from '@backstage/core-plugin-api';
import {
  applicationsToRows,
  environmentsOf,
  applicationsForEnvironment,
  ApplicationsTable,
  EnvironmentTabs,
  type Application,
} from '@apprafter/applications-frontend';
import { applicationsApiRef } from '../../api';

export function ApplicationsPage() {
  const api = useApi(applicationsApiRef);
  const [apps, setApps] = useState<Application[]>([]);
  const [env, setEnv] = useState<string | null>(null);

  useEffect(() => {
    api.listApplications().then(setApps);
  }, [api]);

  const filtered = env ? applicationsForEnvironment(apps, env) : apps;
  return (
    <>
      <EnvironmentTabs
        environments={environmentsOf(apps)}
        selected={env}
        onSelect={setEnv}
      />
      <ApplicationsTable rows={applicationsToRows(filtered)} />
    </>
  );
}
```

The matching backend wiring (operators register the v0.1.34
`KubeApplicationStore` behind `/api/applications`) is documented
in [`@apprafter/applications-backend`](../applications-backend/README.md).
