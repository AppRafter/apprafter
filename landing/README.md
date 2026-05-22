# AppRafter landing

Source of `apprafter.dev` — an Astro 5 static site (`web/`) plus a
self-hosted Payload 3 CMS (`cms/`), wired together as a Bun workspace.
Designed and built end-to-end per
[`docs/2026-05-22-landing-implementation-plan.md`](docs/2026-05-22-landing-implementation-plan.md).

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

## Architecture (TL;DR)

- **Astro 5 SSG.** The whole page is statically rendered at build time;
  three small Svelte 5 islands cover the only interactive bits
  (ThemeToggle, the hero CUE-snippet copy button, the inline managed-
  launch waitlist form). Total client JS budget: **~24 KB gzipped**.
- **Payload 3 + Postgres.** 14 globals hold every editable string on
  the page (Hero, ValueProps, ScalingJourney, TierLadder, Comparison,
  LandingTransparency, BoringTech, Advantages, Roadmap, BootstrapStrip,
  Footer, WaitlistFormCopy, Booking, SiteSettings). One collection
  (`WaitlistSignups`) stores managed-launch signups; an `afterChange`
  hook sends a Calendly invitation email when the user opts in.
- **Single source of truth for dev ports.** `LANDING_CMS_PORT` drives
  the Next.js port, the Astro `/api` proxy upstream, Payload `serverURL`,
  and the CORS allowlist. Change once → everything follows.
- **Fail-loud production builds.** `lib/cms.ts` throws when Payload
  is unreachable during a prod build, so a half-rendered page never
  ships. In dev the same fetch falls back to JSON in
  `web/src/data/fallback/` so you can work offline.

## Quickstart

> Requires Bun ≥ 1.1 and Docker (or a local Postgres ≥ 16).

```sh
cd landing
bun install
docker compose up -d postgres

# 1) Bootstrap CMS env
cp cms/.env.example cms/.env
# Generate a 32+ char secret:
#   openssl rand -base64 48
# Put it in cms/.env as PAYLOAD_SECRET=...

# 2) Start Payload (creates first admin via UI on first boot)
bun run dev:cms
# Open http://localhost:3000/admin and register.

# 3) Seed the 13 content globals from web/src/data/fallback/*.json
bun --filter @apprafter/landing-cms run seed

# 4) Start the site (second terminal)
bun run dev:web                 # http://localhost:4321
```

Astro proxies `/api/*` to the CMS in dev (`astro.config.ts`), so the
waitlist form fetches relative `/api/waitlist-signups` with zero CORS
noise. Edits to globals in the admin reflect on the next reload — the
dev server hits the live API per request.

### Env-driven ports

All five vars default to the quickstart values above; override any
of them when ports collide with something else on your machine.

| Var | Default | Used by |
|---|---|---|
| `LANDING_WEB_PORT` | `4321` | `bun run dev:web` |
| `LANDING_WEB_PREVIEW_PORT` | `4322` | `bun run preview:web` |
| `LANDING_CMS_PORT` | `3000` | `bun run dev:cms` / `bun start:cms` |
| `LANDING_CMS_URL` | `http://localhost:${LANDING_CMS_PORT}` | Astro dev `/api` proxy upstream + `lib/cms.ts` SSR fetch base |
| `LANDING_CMS_CORS_ORIGINS` | derived from web ports + `apprafter.dev` | Payload CORS allowlist |

Examples:

```sh
LANDING_CMS_PORT=3100 bun run dev:cms   # CMS on 3100
LANDING_CMS_PORT=3100 bun run dev:web   # proxy + lib/cms.ts follow
LANDING_CMS_PORT=3100 bun run build:web # prod build hits CMS on 3100
```

Full surfaces in
[`web/.env.example`](web/.env.example) and
[`cms/.env.example`](cms/.env.example).

## Editing content

Almost every string on the site is a Payload **Global**. Open
`http://localhost:3000/admin`, sign in, edit, refresh the site.
The hardcoded bits are:

- Brand SVG + wordmark (`web/src/components/brand/`)
- Design tokens (`web/src/styles/tokens.css`)
- Tier-ladder stair + ScalingJourney node-grid visuals
- Phase-id slug formula (Comparison cross-link target)

For the waitlist hook to actually email the Calendly link, set the
SMTP_* env vars (see `cms/.env.example`); without them the hook
logs a warning and the signup still persists.

## Commands

| Command | What it does |
|---|---|
| `bun run dev:web` | Astro dev server (4321 by default) |
| `bun run dev:cms` | Payload + Next.js dev server (3000 by default) |
| `bun --filter @apprafter/landing-cms run seed` | Upsert every global from the fallback JSONs |
| `bun --filter @apprafter/landing-cms run generate:types` | Regenerate `cms/payload-types.ts` after editing a collection / global |
| `bun run build` | Build both workspaces — `next build` then `astro build` |
| `bun run preview:web` | Serve the production build locally (`bun run build:web` first) |
| `bun run lint` | Biome — TypeScript + JSON + Svelte |
| `bun run typecheck` | `astro check` for web + `tsc --noEmit` for cms |
| `bun run check:spdx` | Local SPDX-header check restricted to `landing/` |
| `bun test` | Smoke tests (40 invariants over the scaffold + content shape) |

## Tests

`bun test` from `landing/` runs three smoke-test files:

- `landing.smoke.test.ts` — workspace shape, shared configs, design
  source-of-truth files, SPDX identifier.
- `web/web.smoke.test.ts` — Astro/Svelte configs, Roboto WOFF2 set,
  design tokens, theme init, hero copy, content sections, page
  shell + SEO + JSON-LD, robots/sitemap/404/privacy/terms, cms.ts
  getters, fallback JSONs, cue-highlight tokenizer behaviour.
- `cms/cms.smoke.test.ts` — Payload + Next routes, Postgres adapter,
  all 13 + Booking globals, seed-script slug mapping, Users +
  WaitlistSignups collections, sendDiscoveryEmail hook + mailer.

40 tests / 197 expect-calls today. New invariants (e.g. a new
Payload global) should add one assertion here per slug so CI
catches regressions.

## License

`FSL-1.1-Apache-2.0` per
[`docs/adr/0032-license-fsl-1-1-apache-2-0.md`](../docs/adr/0032-license-fsl-1-1-apache-2-0.md).
Every source file declares its SPDX identifier on the top five lines.
Roboto + Roboto Mono attribution lives in
[`web/public/fonts/LICENSE-Roboto.txt`](web/public/fonts/LICENSE-Roboto.txt)
(Apache 2.0).

## Deployment

See [`DEPLOY.md`](DEPLOY.md).
