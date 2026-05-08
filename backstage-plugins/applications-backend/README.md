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

## What's missing

- Real kube proxy → v0.1.34 (sub-phase 1.10b).
- Backstage backend plugin glue (`createBackendPlugin`,
  `apiRouter.use(...)`, etc.) → v0.1.34 once the kube store is
  in.
- React frontend table + drilldown → v0.1.35 (sub-phase 1.10c).
- Per-environment tabs → v0.1.36 (sub-phase 1.10d, closes phase 1.10).
