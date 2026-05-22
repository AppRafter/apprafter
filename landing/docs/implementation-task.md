# AppRafter Landing — Implementation Brief

> **For:** Claude Code
> **Input:** Handoff bundle from Claude Design (HTML/CSS/markup with design tokens)
> **Output:** Production-ready Astro landing page integrated with Payload CMS, deployable
> **Companion document:** `LANDING_DESIGN_BRIEF.md` (design spec, already executed)

---

## 0. Overview

You are implementing the AppRafter landing page based on a design handoff from Claude Design. The handoff includes HTML/CSS, design tokens (CSS custom properties), and component structure identified.

Your job is to translate that into a production system using Astro + Svelte (islands) + Payload CMS, with theme switching, i18n readiness, and clean code that can evolve over time.

You are **not** designing anything. Visual decisions are already made. Stick to the handoff. If something seems missing, replicate the closest existing pattern from the handoff rather than inventing.

---

## 1. Stack and constraints

### Required

- **Astro 5.x** — primary framework, static-first
- **Svelte 5.x** — for interactive islands only (Astro `client:*` directives)
- **Payload CMS 3.x** — headless CMS, self-hosted
- **Bun 1.x+** — runtime and package manager (no Node, no npm, no pnpm)
- **TypeScript** — strict mode, no `any` without explicit justification
- **PostgreSQL** — for Payload (use the project's existing platform Postgres if integrated; otherwise a standalone instance for landing dev)

### Not allowed without explicit approval

- React (Svelte covers all interactivity)
- Tailwind (use vanilla CSS with custom properties from design tokens; if a utility layer becomes necessary later, that's a separate decision)
- CSS-in-JS libraries
- Third-party UI kits (shadcn, MUI, Chakra, etc.)
- Storybook (overkill for a landing of this size)
- Heavy build tooling additions beyond Astro defaults

### Encouraged

- Native CSS features: nesting, custom properties, layers, container queries where appropriate
- Astro view transitions for the optional theme toggle / language switch animations
- ViewTransitions API where it cleanly applies
- Image optimization via Astro's built-in `<Image>` component

---

## 2. Monorepo structure

The landing fits into an existing Bun monorepo. Add:

```
apps/
  landing/                  # Astro site
    src/
      components/           # Shared Astro/Svelte components
      layouts/              # Page layouts
      pages/                # Routes (file-based)
        index.astro
        [...slug].astro     # Catch-all for blog/legal pages from CMS
      content/              # Local fallback content (collections schema)
      styles/               # Global CSS, design tokens
      lib/                  # Helpers, CMS client
      i18n/                 # i18n config and helpers
    public/                 # Static assets (logo SVGs, favicon, fonts)
    astro.config.ts
    tsconfig.json
    package.json
  cms/                      # Payload CMS instance
    src/
      collections/          # Payload collection definitions
      globals/              # Payload globals (site settings, etc.)
      payload.config.ts
    package.json
packages/
  ui/                       # If shared components emerge between landing and other apps
  config/                   # Shared tsconfig, eslint, etc.
```

Use Bun workspaces (`workspaces` field in root `package.json`). Both apps run independently in dev (`bun run dev` in each).

---

## 3. Design token integration

The handoff bundle from Claude Design includes CSS custom properties for colors, typography, spacing. Map these to:

```
src/styles/
  tokens.css           # All :root and [data-theme="dark"] custom properties
  reset.css            # Modern CSS reset (Josh Comeau or similar)
  global.css           # Base typography, focus styles, link styles
  fonts.css            # @font-face declarations for Roboto self-hosted
```

Token structure:

- `--color-*` — colors (background, surface, foreground, muted-foreground, accent, border)
- `--text-*` — typography scale (hero, h1, h2, h3, body, small, mono)
- `--space-*` — spacing scale (1 through 8 or so)
- `--radius-*` — border radius (sharp design, mostly 0 with small exceptions)
- `--shadow-*` — minimal shadows
- `--container-*` — content max-widths

Theme switching: `[data-theme="light"]` / `[data-theme="dark"]` on `<html>`. No JS-based color manipulation — pure CSS overrides on the data attribute.

---

## 4. Theme switching

Implementation:

1. **Initial theme detection (no flash of wrong theme):**
    - Inline `<script>` in `<head>` (before any rendering) reads `localStorage.theme`, falls back to `matchMedia('(prefers-color-scheme: dark)')`, defaults to `dark` if undetectable
    - Sets `data-theme` attribute on `<html>` synchronously

2. **Toggle component (Svelte island):**
    - Sun/moon icon button in header
    - Click cycles: system → light → dark → system
    - Persists choice to `localStorage`
    - Respects `prefers-color-scheme` change events when set to "system"
    - Use `client:load` directive (small island, immediate interactivity needed)

3. **CSS:**
    - All colors via custom properties — no hardcoded hex values in component styles
    - Single set of components, two sets of token values

---

## 5. Internationalization

**Initial state:** English only. Setup must accommodate adding languages later without restructuring.

Use Astro's built-in i18n routing (`astro.config.ts → i18n` field):

```ts
i18n: {
  defaultLocale: 'en',
  locales: ['en'],
  routing: {
    prefixDefaultLocale: false,  // /en/... not required for English
  },
}
```

Content strategy:

- All hardcoded strings extracted to `src/i18n/en.json`
- Helper: `t(key)` function reading from current locale JSON
- CMS-managed content (hero copy, feature blocks) is multi-locale via Payload's localization (configure but don't enable additional locales yet)
- Language switcher in header (Svelte island): currently shows `EN` only, dropdown disabled. Component must be ready to show 2-3 languages with no further work when locales are added.

**Anti-RTL:** do not implement RTL. Layout assumes LTR. If RTL is added later, that's a re-architecture, not a configuration change.

---

## 6. Component architecture

From the handoff, identify and create these Astro components:

```
components/
  layout/
    Header.astro
    Footer.astro
    ThemeToggle.svelte         # Svelte island
    LanguageSwitcher.svelte    # Svelte island, disabled dropdown initially
  hero/
    Hero.astro
    HeroCodeSample.astro       # The CUE manifest preview, if included
  sections/
    Positioning.astro          # 3-block "what it is"
    HowItWorks.astro           # Technical depth section
    SelfHostedVsManaged.astro
    StackPhilosophy.astro      # Optional, if designer included it
  primitives/
    CodeBlock.astro            # Syntax-styled code, used in HowItWorks and Hero
    FeatureBlock.astro         # Single block for Positioning section
    ComparisonCard.astro       # Used in SelfHostedVsManaged
    Badge.astro                # "Pre-MVP", "Coming soon" badges
    Container.astro            # Layout container with max-width
  brand/
    Logo.astro                 # Inline SVG with theme-aware fills
    Wordmark.astro
```

**Astro vs Svelte split:**

- **Astro components:** all static rendering (almost everything)
- **Svelte islands:** only for interactivity (theme toggle, language switcher, any future hover-state code samples or animations)
- **Hydration directives:** prefer `client:idle` or `client:visible` for non-critical islands, `client:load` only for theme toggle (must be interactive immediately)

---

## 7. Payload CMS integration

### Collections to define

```ts
// In apps/cms/src/collections/

LandingHero          // Single, global
  - headline: text (localized)
  - subheadline: textarea (localized)
  - primaryCTALabel: text (localized)
  - primaryCTAUrl: text
  - secondaryCTALabel: text (localized)
  - secondaryCTAUrl: text
  - statusBadge: text (localized) — e.g. "Pre-MVP · in active development"

PositioningBlocks    // Array, ordered
  - title: text (localized)
  - body: richText (localized)
  - icon: text (optional, identifier for designer-provided icon set)

HowItWorksBlocks     // Array, ordered
  - title: text (localized)
  - body: richText (localized)
  - codeSample: code (optional, with language field)
  - codeLanguage: select (cue, yaml, bash, typescript, rust)

OfferingBlocks       // Self-hosted + managed
  - type: select (self-hosted | managed)
  - title: text (localized)
  - description: richText (localized)
  - status: text (localized)  // e.g. "Coming soon"
  - ctaLabel: text (localized)
  - ctaUrl: text
  - ctaDisabled: checkbox

StackItems           // Array, ordered (optional section)
  - name: text (localized)
  - description: text (localized)
  - link: text (optional)

FooterLinks          // Grouped links
  - column: text (localized) — column header
  - links: array of {label: localized text, url: text}

LegalPages           // For Privacy / Terms / License pages later
  - slug: text (unique)
  - title: text (localized)
  - body: richText (localized)
  - publishedAt: date

BlogPosts            // For future blog
  - slug: text (unique)
  - title: text (localized)
  - excerpt: text (localized)
  - body: richText (localized)
  - publishedAt: date
  - draft: checkbox
```

All localized fields use Payload's built-in localization. Configure for `en` only initially; document how to add `ru`, `de`, etc.

### Globals

```ts
SiteSettings         // One global
  - defaultTheme: select (system | light | dark) — default "system"
  - githubUrl: text
  - specUrl: text
  - language: select (en) — for now only EN, expandable
```

### Auth

Simple: admin user only (the project owner). No public registration. Payload's default auth is sufficient.

### CMS dev experience

- Payload runs at `http://localhost:3000/admin` in dev
- Astro fetches content from Payload's REST or GraphQL API at build time (static generation)
- For dev: Astro fetches on demand so content edits show up immediately
- For production: full static rebuild on content change (webhook from Payload to redeploy, or manual rebuild — implementer's call based on hosting)

---

## 8. Content fetching strategy

**Static-first.** All landing page content fetched at build time. No client-side data fetching for first paint.

```
apps/landing/src/lib/cms.ts
  - getLandingHero()
  - getPositioningBlocks()
  - getHowItWorksBlocks()
  - getOfferings()
  - getStackItems()
  - getFooterLinks()
  - getSiteSettings()
  - getLegalPage(slug)
  - getBlogPosts({ draft: false })
  - getBlogPost(slug)
```

Each function:
- Uses Bun's native `fetch` to hit Payload API
- Returns typed result (TypeScript types generated from Payload collection schemas — Payload supports this natively)
- Throws on error during build (fail loud, not silent fallback to nothing)
- Has a local fallback fixture (JSON file in `src/content/fallback/`) used when CMS is unreachable AND we're in dev mode (helps when Payload isn't running)

---

## 9. Pages

### Routes

- `/` — landing page (static, prebuilt)
- `/legal/[slug]` — privacy, terms, license (one page per slug from CMS)
- `/blog` — blog index (placeholder, list from CMS)
- `/blog/[slug]` — individual blog post

For pre-launch, only `/` matters. Other routes should exist but can render a placeholder or 404 gracefully if collections are empty.

### Catch-all route

`src/pages/[...slug].astro` handles arbitrary slugs from CMS (legal pages mostly). Falls through to 404 if no match.

---

## 10. Performance and quality requirements

### Performance

- **Lighthouse score 95+** on all four categories (Performance, Accessibility, Best Practices, SEO)
- **First Contentful Paint < 1s** on simulated 3G
- **No Cumulative Layout Shift** — fonts must be preloaded, images must have explicit dimensions
- **JavaScript shipped to client < 30KB** total (gzipped) — only theme toggle and language switcher

### Accessibility

- All interactive elements keyboard-navigable
- Focus states clearly visible (designer should have provided; if not, use 2px solid accent outline)
- Color contrast: WCAG AA minimum (AAA preferred for body text)
- Theme toggle has visible label or `aria-label`
- Code blocks have `aria-label` describing language
- All images have `alt` attributes (decorative ones empty `alt=""`)
- Skip-to-content link in header

### SEO

- Meta tags: `title`, `description`, `og:image`, `og:title`, `og:description`, `twitter:card`
- OG image generated once (1200×630) showing the logo + tagline — design will provide
- Sitemap.xml generated by Astro's sitemap integration
- robots.txt allows all (no need to hide pre-launch — page itself says "pre-MVP")
- Structured data (JSON-LD) for `Organization` and `SoftwareApplication`

### SEO content

- `<title>`: "AppRafter — Opinionated PaaS for solo founders to enterprises"
- `<meta description>`: from Hero subheadline (CMS-driven)
- Default OG image: hero with logo, tagline, dark theme

---

## 11. Fonts

Roboto self-hosted, **not** loaded from Google Fonts.

Steps:

1. Download Roboto from Fontsource or Google Fonts archive (Apache 2.0 license, no attribution needed in UI)
2. Place WOFF2 files in `apps/landing/public/fonts/`
3. Subset to Latin only (use `glyphhanger` or similar) — saves ~70% size
4. Weights: 400, 500, 700 (regular, medium, bold)
5. Roboto Mono for code: 400 only, also subsetted
6. `@font-face` declarations in `src/styles/fonts.css`
7. `<link rel="preload">` for Roboto Regular in document head

Document the license inclusion requirement: copy `Apache-2.0.txt` into `apps/landing/public/fonts/LICENSE-Roboto.txt` as required by Apache 2.0 attribution.

---

## 12. Deployment

**Implementer's choice based on what fits the existing project infrastructure.** Recommendation hierarchy:

1. **Self-host on the same Hetzner VPS that runs other project services** — Astro static output served by Caddy or nginx, Payload as a Docker/Podman container behind reverse proxy. This is most consistent with project's "no vendor lock-in" philosophy.
2. **Cloudflare Pages + separate Payload host** — easy static hosting for Astro, Payload self-hosted elsewhere. Acceptable.
3. **Vercel / Netlify** — only if implementer has strong reason. Notes the irony for an anti-vendor-lock project.

Whichever chosen, document the deployment in `apps/landing/DEPLOY.md` so future maintainer (you) can rebuild.

CI/CD:
- GitHub Actions on push to `main`: typecheck, build, lint, deploy
- No tests required for v1 (it's a landing, content-driven, low logic) — focus on type safety and lint instead

---

## 13. Code quality

- **Strict TypeScript:** `strict: true`, `noUncheckedIndexedAccess: true`, `exactOptionalPropertyTypes: true`
- **ESLint** with `@astrojs/eslint-plugin-astro` and standard TS rules
- **Prettier** with project-wide config
- **No `any`** without an explicit `// eslint-disable-next-line` and a comment explaining why
- **Components < 200 lines** — if longer, refactor into sub-components
- **CSS in component scope** by default (Astro's scoped styles), global CSS only for tokens, reset, fonts

---

## 14. What you should NOT do

- Don't add testimonials, fake metrics, "trusted by" sections — design has none, don't introduce
- Don't add a waitlist form, signup, or any conversion mechanism
- Don't add analytics in v1 (Plausible or self-hosted analytics later, but not now)
- Don't add a chatbot, live chat, or feedback widget
- Don't introduce React, Vue, or other frontend frameworks
- Don't switch to Tailwind without explicit ask
- Don't generate marketing copy — all editorial content comes from the project owner via CMS, not invented by you
- Don't add testimonial fixtures even as placeholders — leave actual blank states
- Don't deploy to production without owner review

---

## 15. Deliverables checklist

- [ ] Astro project scaffolded in `apps/landing/`
- [ ] Payload CMS scaffolded in `apps/cms/` with all collections defined
- [ ] All design tokens from handoff translated into CSS custom properties
- [ ] Theme switching (light/dark, system default, dark fallback)
- [ ] All page sections implemented as Astro components
- [ ] Two Svelte islands: ThemeToggle, LanguageSwitcher
- [ ] Roboto self-hosted, subsetted
- [ ] CMS integration with build-time fetching
- [ ] Local fallback content for offline dev
- [ ] i18n setup (English only, ready for more locales)
- [ ] Lighthouse 95+ verified
- [ ] Type-safe end-to-end (CMS types generated from Payload)
- [ ] `DEPLOY.md` documenting how to deploy
- [ ] `README.md` in `apps/landing/` documenting local dev

---

## 16. Questions to ask before starting

If anything below is unclear, ask the project owner before implementing — these affect architecture:

1. Is there an existing Postgres instance for Payload, or should we provision a fresh one?
2. Where will the site be deployed? (Affects build output mode and env config)
3. Is there a domain ready (`apprafter.dev` registered) and DNS access?
4. Should Payload's admin be at `apprafter.dev/admin` (same domain) or `cms.apprafter.dev` (subdomain)?
5. Are SVG logo files available in the project, or do they need to be created from the visual reference?

If owner is unavailable, default to: provision fresh Postgres, deploy plan TBD (build to static output for now), assume `apprafter.dev` will exist, put admin on subdomain `cms.apprafter.dev`, request SVG files explicitly.

---

## 17. Out of scope (explicit non-goals)

- Authentication beyond Payload admin
- User accounts on the landing site
- Search functionality
- Comments or interactive content
- E-commerce or pricing pages with checkout
- Email capture or newsletter
- A/B testing infrastructure
- Multi-tenancy in Payload
- Mobile app version
- Browser notification subscriptions
