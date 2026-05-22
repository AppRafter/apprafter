# AppRafter landing

Source of `apprafter.dev` — Astro 5 static site (`web/`) plus a self-hosted
Payload 3 CMS (`cms/`), wired together as a Bun workspace.

> **Status:** implementation in progress.
> See `docs/2026-05-22-landing-implementation-plan.md` for the full plan
> (the executable scope, phase-by-phase, with checkboxes).

## Layout

```
landing/
├── web/                # Astro 5 + Svelte 5 islands — the public site
├── cms/                # Payload 3 + Next 15 + Postgres — admin at /admin
├── docs/               # Plan, design source-of-truth, archived briefs
├── scripts/            # Local SPDX check etc.
├── docker-compose.yml  # Postgres for cms dev
├── package.json        # Workspace root
├── tsconfig.base.json
└── biome.json          # Lint + format
```

## Quickstart

> Requires Bun ≥ 1.1, Docker (or a local Postgres ≥ 16).

```sh
cd landing
bun install
docker compose up -d postgres
cp cms/.env.example cms/.env
# edit cms/.env: set PAYLOAD_SECRET to a 32+ char random string

# 1) Start CMS (creates first admin via UI at http://localhost:3000/admin)
bun run dev:cms

# 2) In a second terminal, seed content from local fallback JSON
bun --filter @apprafter/landing-cms run seed

# 3) Start the site
bun run dev:web        # http://localhost:4321
```

Astro proxies `/api/*` to the CMS in dev (`astro.config.ts`), so the waitlist
form works against `localhost:3000` without CORS noise.

## Editing content

Almost every string on the site is a Payload **Global**. Open
`http://localhost:3000/admin`, sign in, edit, refresh the site. Code-level
content (CUE syntax, design tokens, SVG logo) is in the source.

## Lint / typecheck / build

```sh
bun run lint           # Biome (no .astro — those are covered by astro check)
bun run typecheck      # astro check (web) + tsc --noEmit (cms)
bun run check:spdx     # local SPDX-header check restricted to landing/
bun run build          # cms build first, then web build
```

## License

`FSL-1.1-Apache-2.0` per
[`docs/adr/0032-license-fsl-1-1-apache-2-0.md`](../docs/adr/0032-license-fsl-1-1-apache-2-0.md).
Every source file declares its SPDX identifier on the top five lines.

## Deployment

See `DEPLOY.md`.
