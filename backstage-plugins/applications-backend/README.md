# @apprafter/applications-backend

Backstage backend plugin that surfaces `apprafter.io/v1alpha1`
Applications. v0.1.33 ships the TypeScript scaffold + types + pure
handler stubs; v0.1.34 wires them to the kube apiserver via the
in-cluster service-account token; v0.1.35 ships the React frontend
that consumes the JSON; v0.1.36 adds per-environment tabs and
closes plan.md sub-phase 1.10.

## Layout

```
backstage-plugins/applications-backend/
├── package.json
├── tsconfig.json
├── bun.lock
└── src/
    ├── index.ts          # public re-exports
    ├── types.ts          # TS mirror of operator-core::Application
    ├── router.ts         # pure handler fns + ApplicationStore interface
    ├── types.test.ts
    └── router.test.ts
```

## Develop

```sh
cd backstage-plugins/applications-backend
bun install
bun test            # 10 unit tests (5 types + 5 router)
bun run lint        # tsc --noEmit
```

The Bun-based toolchain matches the project-wide CI pattern
(`.github/workflows/test.yml` discovers every depth-≤3
`package.json` and runs `bun install + bun test` in each).

## Public surface (v0.1.33)

- `Application`, `ApplicationSpec`, `ApplicationBaseSpec`,
  `ApplicationStatus`, `ApplicationCondition`,
  `ApplicationExpose`, `ObjectMeta` — types mirroring
  `operator-core::application` (Rust). Hand-synced — change here
  when you change there.
- `isApplication(unknown): obj is Application` — minimal shape
  guard.
- `ApplicationStore { list, get }` — storage abstraction; v0.1.33
  ships only `StubApplicationStore` (always empty / null).
- `listApplicationsHandler(store, namespace?)` →
  `{ items: Application[] }`.
- `getApplicationHandler(store, namespace, name)` →
  `{ application: Application | null, notFound: boolean }`.

## Use the kube store (v0.1.34)

```ts
import {
  inClusterConfig,
  KubeApplicationStore,
  listApplicationsHandler,
} from '@apprafter/applications-backend';

const store = new KubeApplicationStore(await inClusterConfig());
const { items } = await listApplicationsHandler(store);
```

`inClusterConfig()` reads:

| Source                                                            | Field        |
| ----------------------------------------------------------------- | ------------ |
| `KUBERNETES_SERVICE_HOST` env var (set by k8s on every pod)       | `apiServer`  |
| `KUBERNETES_SERVICE_PORT_HTTPS` env var (defaults to `443`)       | `apiServer`  |
| `/var/run/secrets/kubernetes.io/serviceaccount/token`             | `token`      |
| `/var/run/secrets/kubernetes.io/serviceaccount/ca.crt`            | `caCert`     |

For local dev / tests, build the config by hand and inject a mock
`fetchImpl`:

```ts
const store = new KubeApplicationStore({
  apiServer: 'https://localhost:6443',
  token: 'dev-token',
  fetchImpl: myMockFetch,
});
```

The store implements the `ApplicationStore` interface from v0.1.33,
so the existing `listApplicationsHandler` / `getApplicationHandler`
pure handlers consume it without changes.

## What's missing

- Backstage backend plugin glue (`createBackendPlugin`,
  `apiRouter.use(...)`, etc.) → v0.1.35 (sub-phase 1.10c) along
  with the React frontend.
- React frontend table + drilldown → v0.1.35 (sub-phase 1.10c).
- Per-environment tabs → v0.1.36 (sub-phase 1.10d, closes phase 1.10).
- Watch streams for live status updates → phase 2 polish.
- kubeconfig parsing for local dev → not planned; use env-var
  config or a mock `fetchImpl` instead.
