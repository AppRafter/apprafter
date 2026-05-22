# AppRafter Landing — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use `superpowers:subagent-driven-development` (recommended) or `superpowers:executing-plans` to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking. Update this file in place as you complete steps.

**Дата создания:** 2026-05-22
**Автор:** Claude Code (sonnet/opus в режиме `/effort max`)
**Source-of-truth bundle:** `landing/.design-bundle/apprafter-v2/` (gzip из Claude Design)

**Goal:** Собрать продакшен-готовый лендинг AppRafter по утверждённому дизайну: Astro 5 static-first + Svelte 5 islands + Payload CMS 3 + Bun, с self-hosted Roboto, темами light/dark, CUE-snippet с copy-to-clipboard, inline waitlist-формой и Lighthouse 95+.

**Architecture (TL;DR):**

- `landing/` — самостоятельный Bun-workspace внутри монорепо AppRafter, отдельно от Rust workspaces (`cli/`, `operator/`). Внутри две npm-package'и: `landing/web` (Astro-сайт, статическая сборка) и `landing/cms` (Payload 3, headless self-hosted).
- Контент почти весь редактируется через Payload (collections + globals); локальные JSON-фолбэки (`landing/web/src/data/fallback/*.json`) с точным текстом из `LANDING_BRIEF v2.2` используются (а) когда Payload недоступен в dev и (б) как seed для первого `payload migrate` в проде.
- Только три Svelte-острова: `ThemeToggle`, `HeroCodeBlock` (copy-кнопка), `WaitlistForm` (inline-форма с email + opt-in на discovery call). CUE-подсветка делается build-time в Astro компоненте — никакой JS-tokenizer в рантайме.
- Ноль React, ноль Tailwind, ноль сторонних UI-китов. Vanilla CSS на custom properties, ровно как в `styles.css` из бандла.

**Tech Stack:**

- Astro `^5.x`, Svelte `^5.x`, `@astrojs/svelte`, `@astrojs/sitemap`
- Payload CMS `^3.x` (Next.js-app), PostgreSQL (для Payload), Sharp (для медиа)
- Bun `1.x+` (runtime + package manager + workspaces)
- TypeScript strict (`noUncheckedIndexedAccess`, `exactOptionalPropertyTypes`)
- Self-hosted Roboto (400/500/700) + Roboto Mono (400/500) — Fontsource Latin subsets
- `@biomejs/biome` для lint+format (быстрее ESLint+Prettier, один deps, конфиг проще; вписывается в Bun-only TS-стек). Для `.astro` оставляем `astro check` (TS-валидация) — Biome не парсит `.astro` файлы.

---

## 0. Source-of-truth reconciliation

В проекте лежат **четыре частично пересекающихся документа** + дизайн-бандл. Эта секция фиксирует, какой документ выигрывает в каком конфликте, чтобы исполнитель не залип в противоречиях.

### 0.1 Входящие документы

| Файл | Версия | Статус | Используем |
|---|---|---|---|
| `landing/.design-bundle/apprafter-v2/project/uploads/LANDING_BRIEF.md` | v2.2 (2026-05-14) | **Финальный бриф, к которому привязан дизайн** | ✅ Источник всего копирайта и section-структуры |
| `landing/.design-bundle/apprafter-v2/project/sections.jsx` | финальный JSX | Дизайн-выгрузка | ✅ Источник точной разметки и поведения каждой секции |
| `landing/.design-bundle/apprafter-v2/project/styles.css` | финальный CSS | Дизайн-выгрузка | ✅ Источник дизайн-токенов, классов и адаптива (порт 1:1) |
| `landing/.design-bundle/apprafter-v2/project/logo.jsx` | финальный JSX | Дизайн-выгрузка | ✅ Источник SVG логотипа и его вариантов |
| `landing/docs/implementation-task.md` | v1 (older) | Технический бриф для Claude Code | ⚠️ Используем для стека и DX-конвенций; **игнорируем устаревшие места** (см. §0.2) |
| `landing/docs/design-breif.md` | v1 | Старый дизайн-бриф (заменён v2.2) | ❌ Игнорируем, кроме общего описания продукта |
| `landing/docs/design-fix-1.md` | v1 refinement | Промежуточные правки | ❌ Уже учтены в финальном дизайне |
| `landing/docs/steps.md` | — | Walkthrough для оператора, **не относится к лендингу** | ❌ Игнорируем |

### 0.2 Точки конфликта и решения

| Что | implementation-task.md (v1) | LANDING_BRIEF v2.2 + дизайн | Решение |
|---|---|---|---|
| **Language switcher в хедере** | ✅ Есть, EN-only с disabled dropdown | ❌ Нет switcher'а (§8) | Не делаем. Payload localized-поля настраиваем (i18n-ready), но в хедере switcher не рендерим. |
| **Waitlist / signup форма** | ❌ Запрещена («Don't add a waitlist form, signup, or any conversion mechanism») | ✅ Есть inline-форма на secondary CTA + Payload-коллекция + discovery-call opt-in | Делаем (бриф новее; пользователь явно сказал «по контенту дизайн актуален»). |
| **Theme toggle: цикл** | system→light→dark→system (3 состояния) | Light↔Dark (2 состояния), `localStorage` + `prefers-color-scheme` как первый запуск | Делаем 2-state как в дизайне. `prefers-color-scheme` используется только при отсутствии `localStorage`, потом — явный персистент. |
| **Структура apps/** | `apps/landing/` + `apps/cms/` | — | Перекладываем в `landing/web/` + `landing/cms/`. Проект корнем не использует `apps/` (там `cli/`, `operator/`, `providers/` etc.), а каталог `landing/` уже есть и стоит держать самодостаточным. |
| **License в копии** | FSL-1.1-MIT (исторический) | FSL-1.1-Apache-2.0 (в дизайне и в ADR 0032 от 2026-05-19) | Используем `FSL-1.1-Apache-2.0` — это **реальная лицензия проекта** после ADR 0032 (`docs/adr/0032-license-fsl-1-1-apache-2-0.md`). Все SPDX-заголовки в `landing/` ставим `FSL-1.1-Apache-2.0`. Плагины (если бы они тут были) — MIT, как и раньше. |
| **Tweaks panel** | — | Есть в `app.jsx`/`tweaks-panel.jsx` | Не портируем. Это design-time tool, в проде не нужен. Аналогично — accent-presets кроме teal. |
| **CMS-коллекции** | Перечислены 8 шт (LandingHero, PositioningBlocks, …) | В v2.2 §9 — расширенный список (12+) с глобалами `LandingTransparency`, `Booking`, `WaitlistFormCopy` | Берём список из v2.2 §9. Имена нормализуем (см. §3.3). |
| **Code highlighting в hero** | Astro `CodeBlock.astro` (статика) | React-tokenizer в `sections.jsx` (наивный regex, классы `.tok-*`) | Портируем алгоритм 1:1 в build-time TypeScript-функцию `cueTokenize()` в `landing/web/src/lib/cue-highlight.ts`. Никакого Shiki — палитра кастомная (`.tok-key`/`.tok-str`/`.tok-num`/`.tok-kw`/`.tok-ident`/`.tok-acc`/`.tok-cmt`), CSS уже стилизует. Output — pre-rendered HTML, копи-кнопка — единственный Svelte-остров. |
| **CTAs в hero** | "Read the spec" + "View on GitHub" | "Try self-host" → GitHub README quickstart; "Notify me on managed launch" → раскрывает waitlist; "View on GitHub" → repo | Используем дизайн. |
| **Tier описания** | Tier 1: VPS, Tier 4: confidential bare metal | Tier 1-2: VDS / 3+ nodes (Hetzner), Tier 3: bare metal, Tier 4: hyperscalers + orthogonal confidential note | Используем дизайн (более актуальное разделение). |
| **Roadmap** | — | 4 фазы (4, 5+, 6+, 8+) | Берём из дизайна. |

### 0.3 Что переносим в `landing/docs/` и что архивируем

После создания плана:

1. **Оставляем** в `landing/docs/`:
   - `2026-05-22-landing-implementation-plan.md` — этот файл (рабочий план).
   - `implementation-task.md` — пригождается как ссылка на стек/DX/non-goals.
2. **Архивируем** в `landing/docs/archive/`:
   - `design-breif.md` → `archive/design-brief-v1.md`
   - `design-fix-1.md` → `archive/design-fix-1.md`
   - `steps.md` → удалить или переместить в проектный root (не относится к лендингу).
3. **Бандл** `landing/.design-bundle/` — оставляем как есть, добавляем в корневой `.gitignore` чтобы не коммитить 2 МБ assets, и **копируем три файла** в `landing/docs/design-source/` для git-versioning:
   - `sections.jsx` (read-only reference)
   - `styles.css` (read-only reference — фактический порт ляжет в `landing/web/src/styles/`)
   - `LANDING_BRIEF v2.2.md` (читаемый source-of-truth для копирайта)

---

## 1. Directory structure

```
landing/
├── .design-bundle/                    # gitignored, локальная распаковка дизайна (см. §0.3)
├── docs/
│   ├── 2026-05-22-landing-implementation-plan.md   # этот файл
│   ├── implementation-task.md
│   ├── design-source/                  # из бандла, для reference
│   │   ├── LANDING_BRIEF_v2.2.md
│   │   ├── sections.jsx
│   │   └── styles.css
│   └── archive/
│       ├── design-brief-v1.md
│       └── design-fix-1.md
├── package.json                        # workspace root, "name": "@apprafter/landing"
├── tsconfig.base.json                  # shared strict TS config
├── biome.json                          # lint + format config
├── .gitignore
├── README.md
├── DEPLOY.md
├── docker-compose.yml                  # Postgres для cms в dev
│
├── web/                                # Astro 5 сайт
│   ├── package.json                    # "name": "@apprafter/landing-web"
│   ├── astro.config.ts
│   ├── svelte.config.js
│   ├── tsconfig.json                   # extends ../tsconfig.base.json
│   ├── env.d.ts
│   ├── public/
│   │   ├── fonts/                      # Roboto + Roboto Mono WOFF2 (Latin subset)
│   │   │   ├── roboto-400.woff2
│   │   │   ├── roboto-500.woff2
│   │   │   ├── roboto-700.woff2
│   │   │   ├── roboto-mono-400.woff2
│   │   │   ├── roboto-mono-500.woff2
│   │   │   └── LICENSE-Roboto.txt      # Apache-2.0 attribution
│   │   ├── favicon.svg
│   │   ├── favicon-16.png
│   │   ├── apple-touch-icon.png
│   │   ├── og-image.png                # 1200×630, build-generated в Phase I
│   │   └── robots.txt
│   ├── src/
│   │   ├── styles/
│   │   │   ├── tokens.css              # все --color-*, --t-*, --space-*
│   │   │   ├── reset.css               # Josh Comeau-style modern reset
│   │   │   ├── fonts.css               # @font-face Roboto + Roboto Mono
│   │   │   ├── global.css              # base typography, focus, ::selection
│   │   │   └── index.css               # @layer-импорт всего вышеперечисленного
│   │   ├── lib/
│   │   │   ├── cms.ts                  # клиент к Payload REST (+ fallback)
│   │   │   ├── cue-highlight.ts        # build-time CUE-tokenizer (порт из sections.jsx)
│   │   │   ├── theme-init.ts           # генератор inline-script для head
│   │   │   └── types.ts                # ре-экспорт типов из Payload
│   │   ├── content/
│   │   │   └── fallback/
│   │   │       ├── landingHero.json
│   │   │       ├── valueProps.json
│   │   │       ├── scalingJourney.json
│   │   │       ├── tierLadder.json
│   │   │       ├── comparison.json
│   │   │       ├── transparency.json
│   │   │       ├── boringTech.json
│   │   │       ├── advantages.json
│   │   │       ├── roadmap.json
│   │   │       ├── bootstrapStrip.json
│   │   │       ├── footer.json
│   │   │       ├── waitlistCopy.json
│   │   │       └── siteSettings.json
│   │   ├── components/
│   │   │   ├── brand/
│   │   │   │   ├── LogoMark.astro      # SVG из logo.jsx, с variant: twoTone|mono
│   │   │   │   ├── Wordmark.astro
│   │   │   │   └── Brand.astro         # LogoMark + Wordmark, ссылка #top
│   │   │   ├── primitives/
│   │   │   │   ├── Container.astro
│   │   │   │   ├── Eyebrow.astro
│   │   │   │   ├── SectionHead.astro   # eyebrow + h2 + опциональный lede
│   │   │   │   └── StatusPill.astro    # live | waitlist | roadmap
│   │   │   ├── layout/
│   │   │   │   ├── BaseLayout.astro    # <html> + <head> + slot
│   │   │   │   ├── Header.astro
│   │   │   │   ├── Footer.astro
│   │   │   │   ├── ThemeToggle.svelte  # client:load
│   │   │   │   └── header-scroll.ts    # tiny vanilla scroll-listener
│   │   │   ├── hero/
│   │   │   │   ├── Hero.astro
│   │   │   │   ├── HeroCodeBlock.astro # build-time подсветка, обёртка
│   │   │   │   ├── CodeCopyButton.svelte # client:idle, copy-кнопка
│   │   │   │   └── WaitlistForm.svelte # client:load, inline-форма
│   │   │   └── sections/
│   │   │       ├── ValueProps.astro
│   │   │       ├── ScalingJourney.astro
│   │   │       ├── TierLadder.astro
│   │   │       ├── Comparison.astro
│   │   │       ├── BoringTech.astro
│   │   │       ├── Advantages.astro
│   │   │       ├── Roadmap.astro
│   │   │       └── BootstrapStrip.astro
│   │   └── pages/
│   │       ├── index.astro             # лендинг
│   │       ├── 404.astro
│   │       ├── privacy.astro           # stub, CMS-driven
│   │       ├── terms.astro             # stub, CMS-driven
│   │       └── [...slug].astro         # catch-all для CMS-driven legal/blog
│   └── astro-integrations/             # (если нужны проектные интеграции; обычно не нужно)
│
├── cms/                                # Payload 3 (Next.js app)
│   ├── package.json                    # "name": "@apprafter/landing-cms"
│   ├── tsconfig.json
│   ├── next.config.mjs
│   ├── Dockerfile                      # для деплоя
│   ├── .env.example
│   ├── src/
│   │   ├── payload.config.ts
│   │   ├── collections/
│   │   │   ├── Users.ts                # default-auth админ
│   │   │   ├── WaitlistSignups.ts      # email + useCase + wantsCall + callEmailSentAt
│   │   │   ├── LegalPages.ts           # slug + title + body + publishedAt
│   │   │   └── BlogPosts.ts            # для будущего блога (Phase I)
│   │   ├── globals/
│   │   │   ├── SiteSettings.ts         # defaultTheme + githubUrl + specUrl + repoUrl
│   │   │   ├── LandingHero.ts          # headline, subhead, badge, statusBadge, cueSnippet, cueFilename, primaryCTA, secondaryCTA, tertiaryCTA
│   │   │   ├── ValueProps.ts           # array из 3 блоков
│   │   │   ├── ScalingJourney.ts       # eyebrow, h2, lede, leftStage + rightStage + caveat
│   │   │   ├── TierLadder.ts           # eyebrow, h2, cards array, orthogonalNote
│   │   │   ├── Comparison.ts           # heading, table rows array, footnote, columns
│   │   │   ├── LandingTransparency.ts  # 3 transparency-cards
│   │   │   ├── BoringTech.ts           # underHood[], ourCode[], opening, closing
│   │   │   ├── Advantages.ts           # blocks[] с featured-флагом
│   │   │   ├── Roadmap.ts              # phases[]
│   │   │   ├── BootstrapStrip.ts       # body
│   │   │   ├── FooterContent.ts        # columns[], copyright, license
│   │   │   ├── WaitlistFormCopy.ts     # form copy, success copy, calendar link
│   │   │   └── Booking.ts              # discoveryCallUrl + emailTemplate
│   │   ├── hooks/
│   │   │   └── sendDiscoveryEmail.ts   # afterChange hook для WaitlistSignups
│   │   ├── lib/
│   │   │   └── mailer.ts               # nodemailer / Resend SDK обёртка
│   │   └── seed/
│   │       └── seed.ts                 # CLI: `bun run seed` — заливает дефолты из ../web/src/data/fallback/
│   └── public/                         # admin assets если нужно
```

### 1.1 Конвенция именования

- Astro-компоненты: PascalCase (`Hero.astro`).
- Svelte-острова: PascalCase + `.svelte` (`ThemeToggle.svelte`).
- TS-модули: kebab-case (`cms.ts`, `cue-highlight.ts`).
- Collections в Payload — единственное число где смысл единичный (`Booking`), множественное где это набор (`WaitlistSignups`). Globals = singular.
- CSS-классы — kebab-case, точно как в `styles.css` бандла.

---

## 2. Stack decisions

### 2.1 Почему Astro 5 + Svelte 5 (не SvelteKit и не Next)

- Лендинг — static-first. Astro статически рендерит 100% контента; острова появляются только там, где нужна интерактивность (3 шт).
- Svelte 5 в режиме runes — самый компактный SSR-/CSR-runtime (~5 КБ gzipped) среди современных frameworks. Меньше JS на странице.
- SvelteKit и Next были бы избыточны: оба ассумят SSR-сервер; нам это не нужно (Payload отдельный процесс).

### 2.2 Почему Payload 3 (а не Strapi/Directus/Sanity)

- Self-hosted, Postgres backend, TypeScript-конфиг, локализация из коробки, Next.js-app (можно деплоить как обычный Node-process за Caddy).
- Payload 3 нативно генерирует TS-типы из collections — мы получаем end-to-end типобезопасность без кодгена-«велосипедов».
- Совместим с Bun runtime (есть подтверждение в issues; deps работают через Bun's Node-compat).

### 2.3 Почему Biome (не ESLint + Prettier)

- Один deps, один конфиг-файл, в 10-25× быстрее ESLint+Prettier.
- Поддерживает TS/JS/JSON; `.astro` мы покрываем через `astro check` (TS-валидация).
- Если в проекте в будущем понадобится плагин-специфичный rule, который Biome не поддерживает, — переезд на ESLint можно сделать одним PR'ом.

### 2.4 Почему vanilla CSS (не Tailwind, не CSS-in-JS)

- Дизайн уже определён в `styles.css` с custom properties — Tailwind тут только добавит абстракции без пользы.
- Astro scoped styles + `<style is:global>` для tokens — идиоматично.
- Дизайн-токены через CSS custom properties — единственный способ сделать переключение темы без JS-перерисовки компонентов.

### 2.5 Версии (фиксируем в `package.json`)

| Пакет | Версия | Зачем |
|---|---|---|
| `astro` | `^5.0` | Static SSG, content collections, view transitions |
| `@astrojs/svelte` | `^7.0` | Интеграция Svelte 5 |
| `@astrojs/sitemap` | `^3.0` | Auto sitemap.xml |
| `svelte` | `^5.0` | Svelte 5 runes |
| `typescript` | `^5.6` | Strict + `noUncheckedIndexedAccess` |
| `payload` | `^3.0` | CMS ядро |
| `@payloadcms/db-postgres` | `^3.0` | Postgres-адаптер |
| `@payloadcms/next` | `^3.0` | Next.js wrapper |
| `@payloadcms/richtext-lexical` | `^3.0` | Lexical editor |
| `next` | `^15.0` | Хост для Payload-админки |
| `pg` | `^8.13` | Postgres-драйвер для Payload |
| `sharp` | `^0.33` | Image processing |
| `@fontsource/roboto` | `^5.1` | Self-hosted Roboto Latin |
| `@fontsource/roboto-mono` | `^5.1` | Self-hosted Roboto Mono Latin |
| `@biomejs/biome` | `^1.9` | Lint+format |
| `nodemailer` | `^6.9` | Discovery-call email (можно заменить на Resend SDK позже) |

**Полная актуальность версий не критична** — на момент исполнения плана возьмите `latest` для major-версии каждого пакета, зафиксируйте `bun.lockb` в коммит.

---

## 3. CMS schema reference

Все коллекции и глобалы перечислены в `LANDING_BRIEF v2.2 §9`. Ниже — финальный список с TypeScript-сигнатурами для удобства Phase H.

### 3.1 Collections

```ts
// Users — Payload default auth, только админ.
{ slug: 'users', auth: true, fields: [{ name: 'name', type: 'text' }] }

// WaitlistSignups — pre-launch list для managed.
{
  slug: 'waitlist-signups',
  fields: [
    { name: 'email', type: 'email', required: true, unique: true },
    { name: 'useCase', type: 'text' },
    { name: 'wantsCall', type: 'checkbox', defaultValue: false },
    { name: 'source', type: 'text', admin: { readOnly: true } },
    { name: 'callEmailSentAt', type: 'date', admin: { readOnly: true } },
  ],
  hooks: { afterChange: [sendDiscoveryEmail] },
}

// LegalPages — privacy, terms, license (для catch-all маршрута).
{
  slug: 'legal-pages',
  fields: [
    { name: 'slug', type: 'text', required: true, unique: true, index: true },
    { name: 'title', type: 'text', required: true, localized: true },
    { name: 'body', type: 'richText', localized: true },
    { name: 'publishedAt', type: 'date' },
  ],
}

// BlogPosts — заготовка для будущего блога. Минимум для v1.
{
  slug: 'blog-posts',
  fields: [
    { name: 'slug', type: 'text', required: true, unique: true, index: true },
    { name: 'title', type: 'text', required: true, localized: true },
    { name: 'excerpt', type: 'text', localized: true },
    { name: 'body', type: 'richText', localized: true },
    { name: 'publishedAt', type: 'date' },
    { name: 'draft', type: 'checkbox', defaultValue: true },
  ],
}
```

### 3.2 Globals

```ts
SiteSettings {
  defaultTheme: 'system' | 'light' | 'dark' (default: 'system' — в JS трактуем как dark если matchMedia не сработал)
  githubUrl: text                  // https://github.com/AppRafter/apprafter
  specUrl: text                    // .../spec.md
  docsUrl: text                    // .../README.md (с пометкой Soon в нав-меню если пустой)
}

LandingHero {
  headline: richText (localized)   // только bold/accent inline
  subhead: textarea (localized)
  statusBadge: text (localized)    // "v0.13 · M3 · MVP shipped on Tier 1 and Tier 2 · managed in development"
  cueFilename: text                // "billing-api.cue"
  cueSnippet: code (language: cue)
  primaryCTA: { label, href, openInNewTab }
  secondaryCTA: { label }           // для waitlist-кнопки — без href
  tertiaryCTA: { label, href, openInNewTab }
}

ValueProps {
  blocks: array of {
    title: text (localized)
    body: richText (localized)
    iconName: select [grid|chart|lock]   // указатель на встроенный SVG в Astro
  }
}

ScalingJourney {
  eyebrow, h2, lede: localized
  leftStage: { eyebrow, fileLOC, caption }
  rightStage: { eyebrow, fileLOC, caption }
  footerKickers: array of text (localized)  // "No rewrite.", "No migration tool.", "One CUE manifest."
  caveat: richText (localized)
}

TierLadder {
  eyebrow, h2: localized
  cards: array of {
    num, title, price, desc, status: 'live'|'roadmap', statusText
  }
  orthogonalNote: richText (localized)
}

Comparison {
  eyebrow, h2: localized
  columns: [self, managed, turnkey] — каждая column { label, badge: 'available'|'waitlist'|'roadmap', badgeText }
  rows: array of { label, self: richText, managed: richText, turnkey: richText }
  footnote: richText (localized)
}

LandingTransparency {
  blocks: array of {
    kicker, title: localized
    body: richText (localized)
  } // ровно 3 шт: Pricing, Anti-lock, Alignment
}

BoringTech {
  eyebrow, h2, lede, closing: localized
  underHood: array of { name, desc: localized }
  ourCode: array of { name, desc: localized }
}

Advantages {
  eyebrow, h2, lede: localized
  blocks: array of {
    title: richText (localized)
    lead: richText (localized)
    detail: text (localized)
    phaseTag: text (localized)
    featured: checkbox
  }
}

Roadmap {
  eyebrow, h2, lede, closing: localized
  phases: array of { num, title, items: array of text } (localized)
}

BootstrapStrip {
  body: text (localized)
}

FooterContent {
  brandDesc: text (localized)
  columns: array of { heading, links: array of { label, href, soon: checkbox } } (localized)
  copyright: text (localized)
  licenseNote: text (localized)
  founderNote: text (localized)
}

WaitlistFormCopy {
  formIntro: richText (localized)
  emailLabel, useCaseLabel, callLabel: text (localized)
  submitLabel: text (localized)
  successMessage: text (localized)
  successWithCall: text (localized)
  storageNote: text (localized)
}

Booking {
  discoveryCallUrl: text
  discoveryCallEmailSubject: text (localized)
  discoveryCallEmailBody: richText (localized)
}
```

### 3.3 Локализация

- `i18n.locales: ['en']`, `i18n.defaultLocale: 'en'`. Все `localized: true` поля в схеме включены сразу, чтобы добавить язык было редактированием конфига, не миграцией данных.
- Astro `i18n.routing.prefixDefaultLocale: false` — EN на корне `/`.

---

## 4. Progress checklist (high-level)

- [x] Phase A — Bootstrap (A1–A6) — commits 667a807, f2e062b, fe31c5b
- [x] Phase B — Style foundation (B1–B7) — commit be484af
- [x] Phase C — Primitives (C1–C5) — commit 1f21a68
- [x] Phase D — Header & Footer (D1–D2) — commit 0076b3c
- [x] Phase E — Hero (E1–E6) — commit a63c09e
- [x] Phase F — Sections (F1–F8) — commit ceb5ce9
- [x] Phase G — Composition (G1–G4) — commit e2ee391
- [x] Phase H — CMS — part 1 commit b544b7a, part 2 (13 content globals + cms.ts client + section refactor + seed) commit 623f286
- [x] Phase I — Polish (I1–I3 robots+404+legal stubs) — commit d1fb48e. (I4 OG image, I5 axe-core scan, I6 Lighthouse, I7 mobile walkthrough — visual-verification tasks, run before first deploy.)
- [x] Phase J — Docs/CI — commit 44aebe0. (CI workflow integration done in commits 45f3bce + dca51b5.)

---

## Phase A — Bootstrap

**Цель:** Создать структуру каталогов, инициализировать оба package'а, поднять Astro dev-server и Payload admin локально.

### Task A1: Архивация старых документов и подготовка docs/

**Files:**
- Create: `landing/docs/archive/`, `landing/docs/design-source/`
- Move: `landing/docs/design-breif.md` → `landing/docs/archive/design-brief-v1.md`
- Move: `landing/docs/design-fix-1.md` → `landing/docs/archive/design-fix-1.md`
- Delete: `landing/docs/steps.md` (нерелевантно, оператор-walkthrough)
- Copy: `landing/.design-bundle/apprafter-v2/project/uploads/LANDING_BRIEF.md` → `landing/docs/design-source/LANDING_BRIEF_v2.2.md`
- Copy: `landing/.design-bundle/apprafter-v2/project/sections.jsx` → `landing/docs/design-source/sections.jsx`
- Copy: `landing/.design-bundle/apprafter-v2/project/styles.css` → `landing/docs/design-source/styles.css`

- [ ] **Step 1: Создать архивные подкаталоги**
  ```bash
  mkdir -p landing/docs/archive landing/docs/design-source
  ```
- [ ] **Step 2: Переместить старые брифы и удалить steps.md**
  ```bash
  git mv landing/docs/design-breif.md landing/docs/archive/design-brief-v1.md
  git mv landing/docs/design-fix-1.md landing/docs/archive/design-fix-1.md
  rm landing/docs/steps.md
  ```
- [ ] **Step 3: Скопировать source-of-truth из бандла**
  ```bash
  cp landing/.design-bundle/apprafter-v2/project/uploads/LANDING_BRIEF.md landing/docs/design-source/LANDING_BRIEF_v2.2.md
  cp landing/.design-bundle/apprafter-v2/project/sections.jsx landing/docs/design-source/sections.jsx
  cp landing/.design-bundle/apprafter-v2/project/styles.css landing/docs/design-source/styles.css
  ```
- [ ] **Step 4: Игнорировать `.design-bundle/` через `landing/.gitignore`** (создаётся в Task A3 — фиксируем требование). Не трогаем корневой `.gitignore` per §5.1.
- [ ] **Step 5: Commit**
  ```bash
  git add landing/docs landing/docs/2026-05-22-landing-implementation-plan.md
  git commit -m "docs(landing): archive v1 briefs, copy v2.2 design source, add implementation plan"
  ```

### Task A2: SPDX headers

Project требует SPDX в `landing/` будущих исходниках, см. CLAUDE.md «Repository conventions». Проверить, что `scripts/check-spdx-headers.sh` (если он покрывает `landing/`) корректно реагирует. Если нет — landing/ можно временно исключить.

- [ ] **Step 1: Проверить, какие пути покрыты `scripts/check-spdx-headers.sh`**
  ```bash
  cat scripts/check-spdx-headers.sh
  ```
- [ ] **Step 2: Принять решение (с учётом ограничения §5.1):**
  - Корневой `scripts/check-spdx-headers.sh` **не трогаем**, только читаем для понимания формата.
  - Применяем SPDX-заголовок `FSL-1.1-Apache-2.0` ко всему исходному коду в `landing/web/src/` и `landing/cms/src/`. Конфиги/JSON/`.astro` HTML части — на усмотрение скрипта (если он их игнорирует, оставляем без заголовков).
  - Если `landing/` не покрыт корневым скриптом — создаём `landing/scripts/check-spdx.sh` (свой локальный, повторяет логику корневого, но ограниченный `landing/`).
- [ ] **Step 3: Создать helper `landing/.spdx-header.txt`** для единообразного использования при создании файлов:
  ```
  // SPDX-FileCopyrightText: 2026 AppRafter contributors
  // SPDX-License-Identifier: FSL-1.1-Apache-2.0
  ```

  Для CSS/HTML вариант:
  ```
  /* SPDX-FileCopyrightText: 2026 AppRafter contributors */
  /* SPDX-License-Identifier: FSL-1.1-Apache-2.0          */
  ```

  Astro/Svelte fenced top-of-file:
  ```
  ---
  // SPDX-FileCopyrightText: 2026 AppRafter contributors
  // SPDX-License-Identifier: FSL-1.1-Apache-2.0
  ---
  ```

### Task A3: Создать корневой workspace

**Files:**
- Create: `landing/package.json`, `landing/tsconfig.base.json`, `landing/biome.json`, `landing/.gitignore`, `landing/README.md` (stub), `landing/DEPLOY.md` (stub)

- [ ] **Step 1: `landing/package.json`**
  ```json
  {
    "name": "@apprafter/landing",
    "private": true,
    "version": "0.1.0",
    "workspaces": ["web", "cms"],
    "scripts": {
      "dev:web": "bun --filter '@apprafter/landing-web' dev",
      "dev:cms": "bun --filter '@apprafter/landing-cms' dev",
      "build:web": "bun --filter '@apprafter/landing-web' build",
      "build:cms": "bun --filter '@apprafter/landing-cms' build",
      "build": "bun run build:cms && bun run build:web",
      "lint": "biome check .",
      "lint:fix": "biome check --write .",
      "format": "biome format --write .",
      "typecheck": "bun --filter '*' run typecheck"
    },
    "devDependencies": {
      "@biomejs/biome": "^1.9.0",
      "typescript": "^5.6.0"
    },
    "engines": { "bun": ">=1.1.0" }
  }
  ```
- [ ] **Step 2: `landing/tsconfig.base.json`** — общая база, расширяется обоими подпроектами
  ```json
  {
    "compilerOptions": {
      "target": "ES2022",
      "module": "ESNext",
      "moduleResolution": "Bundler",
      "strict": true,
      "noUncheckedIndexedAccess": true,
      "exactOptionalPropertyTypes": true,
      "noImplicitReturns": true,
      "noFallthroughCasesInSwitch": true,
      "isolatedModules": true,
      "esModuleInterop": true,
      "skipLibCheck": true,
      "forceConsistentCasingInFileNames": true,
      "resolveJsonModule": true,
      "verbatimModuleSyntax": true
    }
  }
  ```
- [ ] **Step 3: `landing/biome.json`**
  ```json
  {
    "$schema": "https://biomejs.dev/schemas/1.9.0/schema.json",
    "files": {
      "include": ["**/*.ts", "**/*.tsx", "**/*.js", "**/*.jsx", "**/*.json"],
      "ignore": ["**/dist", "**/.astro", "**/.next", "**/node_modules", "**/.design-bundle", "docs/design-source"]
    },
    "linter": { "enabled": true, "rules": { "recommended": true, "style": { "useNamingConvention": "off" } } },
    "formatter": {
      "enabled": true,
      "indentStyle": "space",
      "indentWidth": 2,
      "lineWidth": 100,
      "lineEnding": "lf"
    },
    "javascript": { "formatter": { "quoteStyle": "single", "trailingCommas": "all", "semicolons": "always" } }
  }
  ```
- [ ] **Step 4: `landing/.gitignore`**
  ```
  node_modules/
  .design-bundle/
  dist/
  .astro/
  .next/
  .env
  .env.local
  *.log
  .DS_Store
  bun.lockb.bak
  ```
  (`bun.lockb` КОММИТИМ, не игнорируем — это lockfile.)
- [ ] **Step 5: `landing/README.md`** и `landing/DEPLOY.md` — пока stub'ы, заполняем в Phase J. Сейчас один абзац «Implementation in progress, see docs/2026-05-22-landing-implementation-plan.md».
- [ ] **Step 6: Commit**
  ```bash
  git add landing/package.json landing/tsconfig.base.json landing/biome.json landing/.gitignore landing/README.md landing/DEPLOY.md
  git commit -m "feat(landing): init Bun workspace root for landing monorepo"
  ```

### Task A4: Astro scaffold

**Files:**
- Create: `landing/web/package.json`, `landing/web/astro.config.ts`, `landing/web/svelte.config.js`, `landing/web/tsconfig.json`, `landing/web/env.d.ts`, `landing/web/src/pages/index.astro` (минимальный hello-world пока), `landing/web/public/.gitkeep`

- [ ] **Step 1: Создать каталоги**
  ```bash
  mkdir -p landing/web/src/{pages,components,styles,lib,content/fallback}
  mkdir -p landing/web/public/fonts
  ```
- [ ] **Step 2: `landing/web/package.json`**
  ```json
  {
    "name": "@apprafter/landing-web",
    "private": true,
    "version": "0.1.0",
    "type": "module",
    "scripts": {
      "dev": "astro dev --port 4321",
      "build": "astro build",
      "preview": "astro preview --port 4322",
      "typecheck": "astro check"
    },
    "dependencies": {
      "astro": "^5.0.0",
      "@astrojs/svelte": "^7.0.0",
      "@astrojs/sitemap": "^3.0.0",
      "svelte": "^5.0.0",
      "@fontsource/roboto": "^5.1.0",
      "@fontsource/roboto-mono": "^5.1.0"
    },
    "devDependencies": {
      "@astrojs/check": "^0.9.0",
      "typescript": "^5.6.0"
    }
  }
  ```
- [ ] **Step 3: `landing/web/astro.config.ts`**
  ```ts
  import { defineConfig } from 'astro/config';
  import svelte from '@astrojs/svelte';
  import sitemap from '@astrojs/sitemap';

  export default defineConfig({
    site: 'https://apprafter.dev',
    integrations: [svelte(), sitemap()],
    i18n: {
      defaultLocale: 'en',
      locales: ['en'],
      routing: { prefixDefaultLocale: false },
    },
    build: { inlineStylesheets: 'auto' },
    vite: {
      server: {
        // Запросы к /api/* проксируем на Payload в dev
        proxy: {
          '/api': { target: 'http://localhost:3000', changeOrigin: true },
        },
      },
    },
  });
  ```
- [ ] **Step 4: `landing/web/svelte.config.js`** (Svelte 5)
  ```js
  export default {
    compilerOptions: {
      // runes-mode по умолчанию в Svelte 5
    },
  };
  ```
- [ ] **Step 5: `landing/web/tsconfig.json`**
  ```json
  {
    "extends": ["../tsconfig.base.json", "astro/tsconfigs/strict"],
    "include": ["src", ".astro", "env.d.ts"],
    "exclude": ["dist", "node_modules"]
  }
  ```
- [ ] **Step 6: `landing/web/env.d.ts`**
  ```ts
  /// <reference path="../.astro/types.d.ts" />
  /// <reference types="astro/client" />

  interface ImportMetaEnv {
    readonly PUBLIC_CMS_URL: string;
    readonly CMS_API_KEY: string | undefined;
  }
  interface ImportMeta {
    readonly env: ImportMetaEnv;
  }
  ```
- [ ] **Step 7: Минимальный `landing/web/src/pages/index.astro`** (пройти dev-старт):
  ```astro
  ---
  ---
  <!doctype html>
  <html lang="en">
    <head><meta charset="utf-8" /><title>AppRafter</title></head>
    <body><h1>AppRafter landing — bootstrapping…</h1></body>
  </html>
  ```
- [ ] **Step 8: Установить зависимости**
  ```bash
  cd landing && bun install
  ```
- [ ] **Step 9: Проверить dev-сервер**
  ```bash
  cd landing && bun run dev:web
  # Ожидается: Astro v5.x, локально http://localhost:4321
  # Открыть в браузере, увидеть "AppRafter landing — bootstrapping…"
  ```
- [ ] **Step 10: Commit**
  ```bash
  git add landing/web landing/bun.lockb
  git commit -m "feat(landing): scaffold Astro 5 + Svelte 5 app"
  ```

### Task A5: Payload scaffold

**Files:**
- Create: `landing/cms/package.json`, `landing/cms/tsconfig.json`, `landing/cms/next.config.mjs`, `landing/cms/.env.example`, `landing/cms/src/payload.config.ts`, `landing/cms/src/collections/Users.ts`, `landing/docker-compose.yml`

- [ ] **Step 1: Создать каталоги**
  ```bash
  mkdir -p landing/cms/src/{collections,globals,hooks,lib,seed}
  ```
- [ ] **Step 2: `landing/docker-compose.yml`** (Postgres для dev)
  ```yaml
  services:
    postgres:
      image: postgres:16
      restart: unless-stopped
      environment:
        POSTGRES_DB: apprafter_cms
        POSTGRES_USER: apprafter
        POSTGRES_PASSWORD: apprafter_dev
      ports: ['5432:5432']
      volumes: ['cms-pg-data:/var/lib/postgresql/data']
  volumes:
    cms-pg-data:
  ```
- [ ] **Step 3: `landing/cms/.env.example`**
  ```
  PAYLOAD_SECRET=replace-me-32-chars-min-randomstring
  DATABASE_URI=postgres://apprafter:apprafter_dev@localhost:5432/apprafter_cms
  PAYLOAD_PUBLIC_SERVER_URL=http://localhost:3000

  # mail (для discovery-call afterChange hook)
  SMTP_HOST=
  SMTP_PORT=587
  SMTP_USER=
  SMTP_PASS=
  SMTP_FROM=noreply@apprafter.dev
  ```
- [ ] **Step 4: `landing/cms/package.json`**
  ```json
  {
    "name": "@apprafter/landing-cms",
    "private": true,
    "version": "0.1.0",
    "type": "module",
    "scripts": {
      "dev": "next dev --port 3000",
      "build": "next build",
      "start": "next start --port 3000",
      "typecheck": "tsc --noEmit",
      "payload": "payload",
      "seed": "bun run src/seed/seed.ts"
    },
    "dependencies": {
      "payload": "^3.0.0",
      "@payloadcms/db-postgres": "^3.0.0",
      "@payloadcms/next": "^3.0.0",
      "@payloadcms/richtext-lexical": "^3.0.0",
      "next": "^15.0.0",
      "react": "^19.0.0",
      "react-dom": "^19.0.0",
      "pg": "^8.13.0",
      "sharp": "^0.33.0",
      "nodemailer": "^6.9.0"
    },
    "devDependencies": {
      "@types/node": "^22.0.0",
      "@types/nodemailer": "^6.4.0",
      "@types/pg": "^8.11.0",
      "@types/react": "^19.0.0",
      "typescript": "^5.6.0"
    }
  }
  ```
- [ ] **Step 5: `landing/cms/tsconfig.json`**
  ```json
  {
    "extends": "../tsconfig.base.json",
    "compilerOptions": {
      "jsx": "preserve",
      "lib": ["DOM", "ES2022"],
      "incremental": true,
      "plugins": [{ "name": "next" }]
    },
    "include": ["src/**/*", "next-env.d.ts", ".next/types/**/*.ts"],
    "exclude": ["node_modules"]
  }
  ```
- [ ] **Step 6: `landing/cms/next.config.mjs`**
  ```js
  /** @type {import('next').NextConfig} */
  const nextConfig = {
    experimental: {},
    serverExternalPackages: ['sharp'],
  };
  export default nextConfig;
  ```
- [ ] **Step 7: `landing/cms/src/collections/Users.ts`** (минимум для админ-логина)
  ```ts
  import type { CollectionConfig } from 'payload';

  export const Users: CollectionConfig = {
    slug: 'users',
    auth: true,
    admin: { useAsTitle: 'email' },
    fields: [{ name: 'name', type: 'text' }],
  };
  ```
- [ ] **Step 8: `landing/cms/src/payload.config.ts`** (минимальный, расширяется в Phase H)
  ```ts
  import { buildConfig } from 'payload';
  import { postgresAdapter } from '@payloadcms/db-postgres';
  import { lexicalEditor } from '@payloadcms/richtext-lexical';
  import path from 'node:path';
  import { fileURLToPath } from 'node:url';
  import { Users } from './collections/Users';

  const dirname = path.dirname(fileURLToPath(import.meta.url));

  export default buildConfig({
    secret: process.env.PAYLOAD_SECRET ?? '',
    db: postgresAdapter({ pool: { connectionString: process.env.DATABASE_URI } }),
    editor: lexicalEditor(),
    collections: [Users],
    globals: [],
    typescript: { outputFile: path.resolve(dirname, '../payload-types.ts') },
    localization: {
      locales: ['en'],
      defaultLocale: 'en',
      fallback: true,
    },
    cors: ['http://localhost:4321', 'https://apprafter.dev'],
    admin: { user: Users.slug },
  });
  ```
- [ ] **Step 9: `landing/cms/src/app/(payload)/admin/[[...segments]]/page.tsx`, layout.tsx, not-found.tsx** — стандартный Payload-next-template. Скопировать из официального шаблона (`bunx create-payload-app@latest`) или из docs (~30 строк). См. https://payloadcms.com/docs/getting-started/installation#payload-with-nextjs (используем как референс).

  > Если у Payload 3 уже есть автогенератор файлов (`payload generate:next`), запустить его. Иначе — создать руками четыре файла (layout, page, not-found, api route handler) по docs.

- [ ] **Step 10: Запустить Postgres + Payload**
  ```bash
  cd landing && docker compose up -d postgres
  cp landing/cms/.env.example landing/cms/.env
  # отредактировать PAYLOAD_SECRET на любую длинную строку
  bun --filter @apprafter/landing-cms dev
  # Ожидается: открыть http://localhost:3000/admin, страница регистрации первого админа
  ```
- [ ] **Step 11: Создать первого админа через UI**, потом сразу выйти.
- [ ] **Step 12: Commit**
  ```bash
  git add landing/cms landing/docker-compose.yml landing/bun.lockb
  git commit -m "feat(landing/cms): scaffold Payload 3 with Postgres adapter"
  ```

### Task A6: SPDX bootstrap pass

- [ ] **Step 1: Применить SPDX header** ко всем созданным `.ts`/`.js`/`.astro`/`.svelte`/`.css` файлам в `landing/web/src/` и `landing/cms/src/`. Файлы конфигов (`*.config.{ts,js,mjs}`) включаем, JSON/lockfile — нет.
- [ ] **Step 2: Прогнать `scripts/check-spdx-headers.sh`** из корня проекта, исправить пропуски.
- [ ] **Step 3: Commit**
  ```bash
  git add landing
  git commit -m "chore(landing): add SPDX headers to bootstrap files"
  ```

---

## Phase B — Style foundation

**Цель:** Переложить дизайн-токены и базовые стили из `styles.css` бандла в `landing/web/src/styles/`, подключить self-hosted Roboto, реализовать инициализацию темы без FOUC.

### Task B1: Дизайн-токены (`tokens.css`)

**Files:**
- Create: `landing/web/src/styles/tokens.css`

Скопировать из `landing/docs/design-source/styles.css` блок `:root { … }` и `[data-theme="light"] { … }`. Не включать пресеты accent (`[data-accent="blue"]` etc.) — они только для design-time tweaks-панели и нам не нужны.

- [ ] **Step 1: Создать файл** с содержимым:
  ```css
  /* SPDX-FileCopyrightText: 2026 AppRafter contributors */
  /* SPDX-License-Identifier: FSL-1.1-Apache-2.0                 */

  /* ============================================================
     AppRafter Landing — design tokens
     Source: design-source/styles.css (Claude Design v2)
     ============================================================ */

  :root {
    /* Dark theme tokens — default */
    --bg:           #0a0e1a;
    --surface:      #111827;
    --surface-2:    #161f30;
    --fg:           #f1f5f9;
    --fg-muted:     #94a3b8;
    --fg-faint:     #64748b;
    --accent:       #14b8a6;
    --accent-2:     #0d9488;
    --accent-fg:    #03110e;
    --border:       #1e293b;
    --border-strong:#2a3a55;
    --code-bg:      #0c1322;
    --code-border:  #1e293b;

    --font-sans: 'Roboto', system-ui, -apple-system, 'Segoe UI', sans-serif;
    --font-mono: 'Roboto Mono', ui-monospace, 'SF Mono', Menlo, Consolas, monospace;

    --section-pad-y: 96px;
    --container-w: 1200px;
    --container-pad-x: 32px;

    --t-display: 64px;
    --t-h2:      40px;
    --t-h3:      22px;
    --t-body:    17px;
    --t-small:   14px;
    --t-mono:    14px;
  }

  [data-theme='light'] {
    --bg:           #fafafa;
    --surface:      #ffffff;
    --surface-2:    #f1f5f9;
    --fg:           #0f172a;
    --fg-muted:     #475569;
    --fg-faint:     #64748b;
    --border:       #e2e8f0;
    --border-strong:#cbd5e1;
    --code-bg:      #0f172a;
    --code-border:  #1e293b;
  }
  ```
- [ ] **Step 2: Commit**

### Task B2: Modern CSS reset (`reset.css`)

**Files:**
- Create: `landing/web/src/styles/reset.css`

Используем Josh Comeau modern reset, адаптированный к нашим токенам.

- [ ] **Step 1: Создать файл** (≈40 строк):
  ```css
  /* SPDX-FileCopyrightText: 2026 AppRafter contributors */
  /* SPDX-License-Identifier: FSL-1.1-Apache-2.0                 */

  *, *::before, *::after { box-sizing: border-box; }
  * { margin: 0; }
  html, body { height: 100%; }
  body {
    line-height: 1.55;
    -webkit-font-smoothing: antialiased;
    text-rendering: optimizeLegibility;
  }
  img, picture, video, canvas, svg { display: block; max-width: 100%; }
  input, button, textarea, select { font: inherit; color: inherit; }
  p, h1, h2, h3, h4, h5, h6 { overflow-wrap: break-word; }
  #root, #__next { isolation: isolate; }
  ```

### Task B3: Шрифты (`fonts.css` + WOFF2 в `public/fonts/`)

**Files:**
- Create: `landing/web/src/styles/fonts.css`
- Create: `landing/web/public/fonts/roboto-{400,500,700}.woff2`, `roboto-mono-{400,500}.woff2`, `LICENSE-Roboto.txt`

Используем `@fontsource/roboto` и `@fontsource/roboto-mono` (уже в deps Task A4). Эти пакеты дают Latin subsets WOFF2.

- [ ] **Step 1: Скопировать WOFF2 в `public/fonts/`**
  ```bash
  cp landing/web/node_modules/@fontsource/roboto/files/roboto-latin-400-normal.woff2 landing/web/public/fonts/roboto-400.woff2
  cp landing/web/node_modules/@fontsource/roboto/files/roboto-latin-500-normal.woff2 landing/web/public/fonts/roboto-500.woff2
  cp landing/web/node_modules/@fontsource/roboto/files/roboto-latin-700-normal.woff2 landing/web/public/fonts/roboto-700.woff2
  cp landing/web/node_modules/@fontsource/roboto-mono/files/roboto-mono-latin-400-normal.woff2 landing/web/public/fonts/roboto-mono-400.woff2
  cp landing/web/node_modules/@fontsource/roboto-mono/files/roboto-mono-latin-500-normal.woff2 landing/web/public/fonts/roboto-mono-500.woff2
  ```
  Если subsets называются иначе после установки конкретной версии — посмотреть `landing/web/node_modules/@fontsource/roboto/files/` и взять верные filenames.

- [ ] **Step 2: Записать Apache-2.0 attribution** в `landing/web/public/fonts/LICENSE-Roboto.txt`. Скопировать стандартный Apache-2.0 текст + строку «Roboto and Roboto Mono are licensed under the Apache License, Version 2.0. https://fonts.google.com/specimen/Roboto».
- [ ] **Step 3: `landing/web/src/styles/fonts.css`**
  ```css
  /* SPDX-FileCopyrightText: 2026 AppRafter contributors */
  /* SPDX-License-Identifier: FSL-1.1-Apache-2.0                 */

  @font-face {
    font-family: 'Roboto';
    font-style: normal;
    font-weight: 400;
    font-display: swap;
    src: url('/fonts/roboto-400.woff2') format('woff2');
    unicode-range: U+0000-00FF, U+0131, U+0152-0153, U+02BB-02BC, U+02C6, U+02DA, U+02DC, U+0304, U+0308, U+0329, U+2000-206F, U+2074, U+20AC, U+2122, U+2191, U+2193, U+2212, U+2215, U+FEFF, U+FFFD;
  }
  @font-face {
    font-family: 'Roboto';
    font-style: normal;
    font-weight: 500;
    font-display: swap;
    src: url('/fonts/roboto-500.woff2') format('woff2');
    unicode-range: /* same Latin range */;
  }
  @font-face {
    font-family: 'Roboto';
    font-style: normal;
    font-weight: 700;
    font-display: swap;
    src: url('/fonts/roboto-700.woff2') format('woff2');
    unicode-range: /* same */;
  }
  @font-face {
    font-family: 'Roboto Mono';
    font-style: normal;
    font-weight: 400;
    font-display: swap;
    src: url('/fonts/roboto-mono-400.woff2') format('woff2');
    unicode-range: /* same */;
  }
  @font-face {
    font-family: 'Roboto Mono';
    font-style: normal;
    font-weight: 500;
    font-display: swap;
    src: url('/fonts/roboto-mono-500.woff2') format('woff2');
    unicode-range: /* same */;
  }
  ```
  > Полный `unicode-range` для Latin берётся из `@fontsource/roboto/index.css` — скопировать.

- [ ] **Step 4: Добавить `<link rel="preload">` для regular 400 шрифта** в `BaseLayout.astro` (см. Phase G).
- [ ] **Step 5: Commit**

### Task B4: Global typography & base (`global.css`)

**Files:**
- Create: `landing/web/src/styles/global.css`

Скопировать из `design-source/styles.css` всё, что **не** относится к секциям (Header, Hero, Tier ladder и т.д.) — а именно `html, body`, `::selection`, `a`, `.container`, `section`, `h1-h4`, `p`, `.eyebrow`, `.muted`, `.faint`, `.mono` (всё что между строкой `* { box-sizing: border-box; }` и началом `Header`).

- [ ] **Step 1: Создать файл, скопировать соответствующий блок 1:1** (≈40 строк).
- [ ] **Step 2: Удалить дублирующий `* { box-sizing }`** — он уже в `reset.css`.

### Task B5: Объединяющий `index.css`

**Files:**
- Create: `landing/web/src/styles/index.css`

```css
/* SPDX-FileCopyrightText: 2026 AppRafter contributors */
/* SPDX-License-Identifier: FSL-1.1-Apache-2.0                 */

@import url('./fonts.css');
@import url('./reset.css');
@import url('./tokens.css');
@import url('./global.css');
```

Все сложные section-стили из `design-source/styles.css` будем класть в scoped-стили компонентов (Phases C-F).

### Task B6: Theme init script (no-FOUC)

**Files:**
- Create: `landing/web/src/lib/theme-init.ts`

Скрипт сериализуется в `<head>` и выполняется до первого пейнта. Логика:

1. Прочитать `localStorage.apprafter-theme` (`'dark'`/`'light'`).
2. Если пусто — спросить `matchMedia('(prefers-color-scheme: light)')`. Light → `light`, иначе `dark` (fallback).
3. Установить `documentElement.setAttribute('data-theme', theme)`.

```ts
// SPDX-FileCopyrightText: 2026 AppRafter contributors
// SPDX-License-Identifier: FSL-1.1-Apache-2.0

export const THEME_INIT_SCRIPT = `
(function(){
  try {
    var stored = localStorage.getItem('apprafter-theme');
    var theme = stored || (window.matchMedia('(prefers-color-scheme: light)').matches ? 'light' : 'dark');
    document.documentElement.setAttribute('data-theme', theme);
  } catch (e) {
    document.documentElement.setAttribute('data-theme', 'dark');
  }
})();
`;
```

Этот скрипт вставляется через `<script is:inline set:html={THEME_INIT_SCRIPT} />` в `BaseLayout.astro` (см. Phase G1) перед любыми стилями.

### Task B7: Verify dev-сборка с подключёнными стилями

- [ ] **Step 1: Импортировать `index.css`** в первой странице (временно), запустить dev-сервер.
  - В `landing/web/src/pages/index.astro` добавить `import '../styles/index.css';` в front-matter.
  - Открыть http://localhost:4321, убедиться: Roboto подключился (`Inspect → Network → fonts/`), `data-theme="dark"` есть на `<html>`, фон тёмно-синий `#0a0e1a`.
- [ ] **Step 2: Переключить вручную в DevTools** `<html data-theme="light">` → фон становится `#fafafa`.
- [ ] **Step 3: Commit**
  ```bash
  git commit -am "feat(landing/web): tokens, fonts, reset, global styles + theme init script"
  ```

---

## Phase C — Primitives & brand components

**Цель:** Подготовить переиспользуемые низкоуровневые компоненты: логотип, Wordmark, Brand-обёртку, Container, Eyebrow/SectionHead, StatusPill. И первый Svelte-остров — ThemeToggle.

### Task C1: `LogoMark.astro`

**Files:**
- Create: `landing/web/src/components/brand/LogoMark.astro`

Портируем SVG из `landing/docs/design-source/sections.jsx → logo.jsx`. Поддерживаем varianты `twoTone` (default) и `mono`. В `twoTone` accent-path использует `fill="var(--accent)"`, остальные — `currentColor`.

- [ ] **Step 1: Создать файл:**
  ```astro
  ---
  // SPDX-FileCopyrightText: 2026 AppRafter contributors
  // SPDX-License-Identifier: FSL-1.1-Apache-2.0
  type Props = { size?: number; variant?: 'twoTone' | 'mono' };
  const { size = 26, variant = 'twoTone' } = Astro.props;
  const accentFill = variant === 'mono' ? 'currentColor' : 'var(--accent)';
  ---
  <svg
    xmlns="http://www.w3.org/2000/svg"
    viewBox="0 0 200 200"
    width={size}
    height={size}
    shape-rendering="geometricPrecision"
    aria-label="AppRafter"
    style="display:block;flex-shrink:0"
  >
    <g transform="translate(0 -5)">
      <path d="…" fill="currentColor" fill-rule="evenodd" /> <!-- из logo.jsx -->
      <path d="…" fill="currentColor" fill-rule="evenodd" />
      <path d="…" fill="currentColor" fill-rule="evenodd" />
      <path d={'M 50.870 71.474 L 94.735 52.855 L 88.838 67.002 L 50.031 83.474 Z M 105.265 52.855 L 149.130 71.474 L 149.969 83.474 L 111.162 67.002 Z'} fill={accentFill} fill-rule="evenodd" />
    </g>
  </svg>
  ```
  > Все четыре `d` path'а взять побайтно из `landing/docs/design-source/sections.jsx` (там в `logo.jsx` бандла, но всё одинаково).

### Task C2: `Wordmark.astro`

**Files:**
- Create: `landing/web/src/components/brand/Wordmark.astro`

```astro
---
// SPDX-FileCopyrightText: 2026 AppRafter contributors
// SPDX-License-Identifier: FSL-1.1-Apache-2.0
type Props = { variant?: 'twoTone' | 'mono' };
const { variant = 'twoTone' } = Astro.props;
---
{variant === 'mono' ? (
  <span class="wordmark">AppRafter</span>
) : (
  <span class="wordmark">App<span class="accented">Rafter</span></span>
)}

<style>
  .wordmark { display: inline-flex; gap: 0; }
  .wordmark .accented { color: var(--accent); }
</style>
```

### Task C3: `Brand.astro`

**Files:**
- Create: `landing/web/src/components/brand/Brand.astro`

```astro
---
// SPDX-FileCopyrightText: 2026 AppRafter contributors
// SPDX-License-Identifier: FSL-1.1-Apache-2.0
import LogoMark from './LogoMark.astro';
import Wordmark from './Wordmark.astro';
type Props = { variant?: 'twoTone' | 'mono'; size?: number };
const { variant = 'twoTone', size = 26 } = Astro.props;
---
<a href="#top" class="brand" aria-label="AppRafter home">
  <LogoMark size={size} variant={variant} />
  <Wordmark variant={variant} />
</a>

<style>
  .brand {
    display: inline-flex;
    align-items: center;
    gap: 10px;
    font-weight: 700;
    font-size: 17px;
    letter-spacing: -0.01em;
    color: inherit;
    text-decoration: none;
  }
</style>
```

### Task C4: `Container.astro`, `Eyebrow.astro`, `SectionHead.astro`, `StatusPill.astro`

**Files:**
- Create: `landing/web/src/components/primitives/Container.astro`
- Create: `landing/web/src/components/primitives/Eyebrow.astro`
- Create: `landing/web/src/components/primitives/SectionHead.astro`
- Create: `landing/web/src/components/primitives/StatusPill.astro`

- [ ] **Step 1: `Container.astro`**
  ```astro
  ---
  // SPDX-FileCopyrightText: 2026 AppRafter contributors
  // SPDX-License-Identifier: FSL-1.1-Apache-2.0
  type Props = astroHTML.JSX.HTMLAttributes;
  const { class: className, ...rest } = Astro.props;
  ---
  <div class:list={['container', className]} {...rest}><slot /></div>
  ```
  (Класс `.container` уже задан в `global.css`.)

- [ ] **Step 2: `Eyebrow.astro`** — обёртка над `<div class="eyebrow">/ {text}</div>`. Default — text-параметр.
- [ ] **Step 3: `SectionHead.astro`** — `<div class="section-head">` с slot'ами `eyebrow`, `default` (h2), `lede`.
- [ ] **Step 4: `StatusPill.astro`** — `<span class="status-pill is-{kind}">{label}</span>` где kind in `live|waitlist|roadmap`. Все стили классов уже в `design-source/styles.css` — копируем нужные в scoped.

### Task C5: `ThemeToggle.svelte` (первый остров)

**Files:**
- Create: `landing/web/src/components/layout/ThemeToggle.svelte`

```svelte
<!-- SPDX-FileCopyrightText: 2026 AppRafter contributors -->
<!-- SPDX-License-Identifier: FSL-1.1-Apache-2.0 -->
<script lang="ts">
  import { onMount } from 'svelte';

  let theme = $state<'dark' | 'light'>('dark');

  onMount(() => {
    const current = document.documentElement.getAttribute('data-theme');
    theme = current === 'light' ? 'light' : 'dark';
  });

  function toggle() {
    theme = theme === 'dark' ? 'light' : 'dark';
    document.documentElement.setAttribute('data-theme', theme);
    try { localStorage.setItem('apprafter-theme', theme); } catch {}
  }
</script>

<button
  class="icon-btn"
  onclick={toggle}
  aria-label={theme === 'dark' ? 'Switch to light theme' : 'Switch to dark theme'}
  title={theme === 'dark' ? 'Light theme' : 'Dark theme'}
>
  {#if theme === 'dark'}
    <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
      <circle cx="12" cy="12" r="4" />
      <path d="M12 2v2M12 20v2M4.9 4.9l1.4 1.4M17.7 17.7l1.4 1.4M2 12h2M20 12h2M4.9 19.1l1.4-1.4M17.7 6.3l1.4-1.4" />
    </svg>
  {:else}
    <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
      <path d="M21 12.8A9 9 0 1 1 11.2 3 7 7 0 0 0 21 12.8z" />
    </svg>
  {/if}
</button>

<style>
  .icon-btn {
    display: inline-flex; align-items: center; justify-content: center;
    width: 36px; height: 36px;
    background: transparent;
    border: 1px solid var(--border);
    color: var(--fg-muted);
    cursor: pointer;
    transition: all 150ms ease;
  }
  .icon-btn:hover { color: var(--fg); border-color: var(--border-strong); }
</style>
```

Использование (в Header): `<ThemeToggle client:load />`.

- [ ] **Commit:** `feat(landing/web): brand + primitives + theme toggle island`

---

## Phase D — Header & Footer

**Цель:** Sticky header с brand, нав-меню, GitHub-иконкой и `ThemeToggle`; footer с колонками, copyright, license. Скролл-поведение хедера — vanilla-script (без острова).

### Task D1: `Header.astro`

**Files:**
- Create: `landing/web/src/components/layout/Header.astro`
- Create: `landing/web/src/components/layout/header-scroll.ts`

Скопировать структуру из `sections.jsx → Header`. Three nav-links: Docs (с `soon` бейджем), Spec, GitHub (icon). Theme toggle справа.

- [ ] **Step 1: `header-scroll.ts`**
  ```ts
  // SPDX-FileCopyrightText: 2026 AppRafter contributors
  // SPDX-License-Identifier: FSL-1.1-Apache-2.0
  const header = document.querySelector('.site-header');
  if (header) {
    const onScroll = () => header.classList.toggle('scrolled', window.scrollY > 8);
    onScroll();
    window.addEventListener('scroll', onScroll, { passive: true });
  }
  ```

- [ ] **Step 2: `Header.astro`**
  ```astro
  ---
  // SPDX-FileCopyrightText: 2026 AppRafter contributors
  // SPDX-License-Identifier: FSL-1.1-Apache-2.0
  import Brand from '../brand/Brand.astro';
  import Container from '../primitives/Container.astro';
  import ThemeToggle from './ThemeToggle.svelte';
  import { getSiteSettings } from '../../lib/cms';
  const settings = await getSiteSettings();
  ---
  <header class="site-header" id="top">
    <Container class="row">
      <Brand variant="twoTone" size={26} />
      <nav class="nav" aria-label="Primary">
        {settings.docsUrl
          ? <a href={settings.docsUrl} target="_blank" rel="noreferrer">Docs</a>
          : <a href={settings.githubUrl + '#readme'} target="_blank" rel="noreferrer" class="soon">Docs</a>
        }
        <a href={settings.specUrl} target="_blank" rel="noreferrer">Spec</a>
        <a href={settings.githubUrl} target="_blank" rel="noreferrer" aria-label="GitHub">
          <svg width="18" height="18" viewBox="0 0 24 24" fill="currentColor" aria-hidden="true">
            <path d="…" /> <!-- octocat — из sections.jsx Header -->
          </svg>
        </a>
        <ThemeToggle client:load />
      </nav>
    </Container>
  </header>

  <script>
    import '../../components/layout/header-scroll.ts';
  </script>

  <style>
    /* Скопировать .site-header, .site-header.scrolled, .nav, .nav a, .nav .soon::after из styles.css */
  </style>
  ```

  > Полные стили `.site-header`, `.nav`, `.soon::after` — из `design-source/styles.css` строки 108-176.

### Task D2: `Footer.astro`

**Files:**
- Create: `landing/web/src/components/layout/Footer.astro`

Структура из `sections.jsx → Footer`: brand-колонка с desc + три колонки (Project, Legal, Author) + footer-bottom с copyright, license note, founder line.

- [ ] **Step 1: Создать компонент**, забирая данные через `getFooterContent()` (CMS-клиент; пока вернёт fallback JSON).
- [ ] **Step 2: Стили `.site-footer`, `.footer-grid`, `.footer-grid h4`, `.footer-grid ul`, `.footer-brand .desc`, `.footer-bottom`** — из `design-source/styles.css` строки 1088-1123.
- [ ] **Commit:** `feat(landing/web): header + footer Astro components with scroll behavior`

---

## Phase E — Hero

**Цель:** Главная секция — заголовок + subhead + 3 CTA + status badge слева, CUE-сниппет с copy-button справа. Вторая CTA раскрывает inline `WaitlistForm` (Svelte-остров с локальным state).

### Task E1: `cue-highlight.ts` — build-time подсветка

**Files:**
- Create: `landing/web/src/lib/cue-highlight.ts`

Портируем функцию `renderCue` из `sections.jsx` в TypeScript-функцию, возвращающую HTML-строку. Используется на этапе SSR в Astro (build-time), HTML кладётся в `<pre set:html={…} />` — никакого JS в браузере.

Используем `String.prototype.matchAll` вместо ручной итерации регэкспом — на одну строку короче и не страдает от глобального state.

- [ ] **Step 1: Скопировать алгоритм 1:1** из `sections.jsx:91-122`. Преобразовать React-фрагменты в строки с `<span class="tok-…">`. Эскейпить HTML-сущности (`<`, `>`, `&`, `"`).

  ```ts
  // SPDX-FileCopyrightText: 2026 AppRafter contributors
  // SPDX-License-Identifier: FSL-1.1-Apache-2.0

  const TOKEN_RE = /("(?:[^"\\]|\\.)*")|(\b\d+\b)|(\b(?:apiVersion|kind|metadata|spec|base|environments|expose|env|image|replicas|port|public|network|name|namespace|needs|from|claim|size)\b)|(&|\||\?|:|\.|\{|\}|\[|\])|([A-Za-z_][A-Za-z0-9_-]*)/g;

  function esc(s: string): string {
    return s.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;').replace(/"/g, '&quot;');
  }

  export function cueTokenize(src: string): string {
    return src.split('\n').map(line => {
      const leadMatch = line.match(/^(\s+)/);
      const lead = leadMatch ? leadMatch[1] : '';
      const rest = leadMatch ? line.slice(lead.length) : line;
      if (rest.startsWith('//')) {
        return `<div>${esc(lead)}<span class="tok-cmt">${esc(rest)}</span></div>`;
      }
      let out = esc(lead);
      let last = 0;
      for (const m of rest.matchAll(TOKEN_RE)) {
        const idx = m.index ?? 0;
        if (idx > last) out += esc(rest.slice(last, idx));
        if (m[1]) out += `<span class="tok-str">${esc(m[1])}</span>`;
        else if (m[2]) out += `<span class="tok-num">${esc(m[2])}</span>`;
        else if (m[3]) out += `<span class="tok-key">${esc(m[3])}</span>`;
        else if (m[4]) out += `<span class="tok-kw">${esc(m[4])}</span>`;
        else if (m[5]) out += `<span class="tok-ident">${esc(m[5])}</span>`;
        last = idx + m[0].length;
      }
      if (last < rest.length) out += esc(rest.slice(last));
      return `<div style="min-height:1.65em">${out || '&nbsp;'}</div>`;
    }).join('');
  }
  ```

- [ ] **Step 2: Sanity test** — на сниппете из брифа `apiVersion: apprafter.io/v1alpha1` ключи `apiVersion`, `kind` должны попасть в `tok-key`, строки `"…"` в `tok-str`, числа `8080`, `3` в `tok-num`. Вручную через `bun --print "import('./src/lib/cue-highlight').then(m => console.log(m.cueTokenize('apiVersion: \"foo\" replicas: 3')))"`.

### Task E2: `HeroCodeBlock.astro`

**Files:**
- Create: `landing/web/src/components/hero/HeroCodeBlock.astro`

```astro
---
// SPDX-FileCopyrightText: 2026 AppRafter contributors
// SPDX-License-Identifier: FSL-1.1-Apache-2.0
import { cueTokenize } from '../../lib/cue-highlight';
import CodeCopyButton from './CodeCopyButton.svelte';

type Props = { filename: string; snippet: string };
const { filename, snippet } = Astro.props;
const html = cueTokenize(snippet);
---
<div class="codeblock" aria-label="Application manifest example">
  <div class="codeblock-header">
    <span class="filename">{filename}</span>
    <CodeCopyButton client:idle snippet={snippet} />
  </div>
  <pre set:html={html} />
</div>

<style>
  /* Скопировать стили .codeblock, .codeblock-header, .filename, .copy-btn (без .copied — он в Svelte), pre, .tok-* из design-source/styles.css строки 267-333 */
</style>
```

### Task E3: `CodeCopyButton.svelte`

**Files:**
- Create: `landing/web/src/components/hero/CodeCopyButton.svelte`

Кнопка копирования, единственная интерактивная часть code-блока.

```svelte
<!-- SPDX-FileCopyrightText: 2026 AppRafter contributors -->
<!-- SPDX-License-Identifier: FSL-1.1-Apache-2.0 -->
<script lang="ts">
  type Props = { snippet: string };
  let { snippet }: Props = $props();
  let copied = $state(false);
  let timer: ReturnType<typeof setTimeout> | null = null;

  function copy() {
    navigator.clipboard.writeText(snippet).then(() => {
      copied = true;
      if (timer) clearTimeout(timer);
      timer = setTimeout(() => (copied = false), 1500);
    }).catch(() => {});
  }
</script>

<button class={'copy-btn' + (copied ? ' copied' : '')} onclick={copy} aria-live="polite">
  {#if copied}
    <svg width="11" height="11" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="3" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><polyline points="20 6 9 17 4 12" /></svg>
    Copied
  {:else}
    <svg width="11" height="11" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><rect x="9" y="9" width="13" height="13" rx="1" /><path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1" /></svg>
    Copy
  {/if}
</button>
```

> Стили `.copy-btn`, `.copy-btn:hover`, `.copy-btn.copied` уже в `HeroCodeBlock.astro` scoped — Svelte-кнопка ходит в эти классы через global selectors. Или дублируем `<style>` внутри Svelte (Svelte по умолчанию scoped, но в данном случае хочется, чтобы кнопка стилизовалась из обёртки — проще скопировать стили внутрь Svelte компонента).

### Task E4: `WaitlistForm.svelte`

**Files:**
- Create: `landing/web/src/components/hero/WaitlistForm.svelte`

Inline-форма из `sections.jsx → WaitlistForm`. Поля: email, useCase, wantsCall checkbox. Submit POST на `/api/waitlist-signups` (Payload автогенеренный endpoint). При успехе — показать `successMessage` (если `wantsCall` — `successWithCall`).

- [ ] **Step 1: Скопировать структуру JSX** из `sections.jsx:209-272`, переписать на Svelte 5 runes.
- [ ] **Step 2: Подключить fetch к Payload endpoint:**
  ```ts
  async function submit(e: SubmitEvent) {
    e.preventDefault();
    if (!email || !/^[^\s@]+@[^\s@]+\.[^\s@]+$/.test(email)) return;
    const res = await fetch(import.meta.env.PUBLIC_CMS_URL + '/api/waitlist-signups', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({
        email,
        useCase: useCase || undefined,
        wantsCall,
        source: typeof document !== 'undefined' ? document.referrer || 'direct' : 'direct',
      }),
    });
    if (res.ok) submitted = true;
    else error = await res.text().catch(() => 'Submit failed');
  }
  ```
- [ ] **Step 3: Props:**
  ```ts
  type Props = {
    formIntro: string;
    emailLabel: string;
    useCaseLabel: string;
    callLabel: string;
    submitLabel: string;
    successMessage: string;
    successWithCall: string;
    storageNote: string;
  };
  ```
  Все строки приходят из `WaitlistFormCopy` global. Передаются из `Hero.astro` пропсами.
- [ ] **Step 4: Стили** — из `design-source/styles.css` строки 1006-1072 (`.waitlist`, `.field`, `.checkbox-row`, `.waitlist-actions`, `.waitlist-success`).

### Task E5: `Hero.astro`

**Files:**
- Create: `landing/web/src/components/hero/Hero.astro`

Композиция: левая колонка (headline + subhead + CTAs + status-badge + опционально WaitlistForm), правая колонка (HeroCodeBlock). Layout — `.hero-grid`.

Особенность: вторая CTA — обычный `<button>` с `onclick` на крошечный inline-скрипт, который тоглит `display` `WaitlistForm`'а. Или сделать всю Hero левую часть Svelte-островом? Проще:

- `WaitlistForm.svelte` всегда рендерится с `hidden` атрибутом + у себя следит за `waitlistOpen` через `addEventListener('waitlist:toggle', …)`.
- В `Hero.astro` кнопка делает `dispatchEvent(new CustomEvent('waitlist:toggle'))`.

Это даёт нам statless Astro hero без раздувания острова на весь блок.

```astro
---
// SPDX-FileCopyrightText: 2026 AppRafter contributors
// SPDX-License-Identifier: FSL-1.1-Apache-2.0
import Container from '../primitives/Container.astro';
import HeroCodeBlock from './HeroCodeBlock.astro';
import WaitlistForm from './WaitlistForm.svelte';
import { getLandingHero, getWaitlistCopy } from '../../lib/cms';
const hero = await getLandingHero();
const waitlistCopy = await getWaitlistCopy();
---
<section class="hero" aria-label="Hero">
  <Container class="hero-grid">
    <div>
      <h1 set:html={hero.headlineHtml} />
      <p class="subhead">{hero.subhead}</p>
      <div class="ctas">
        <a class="btn btn-primary" href={hero.primaryCTA.href} target="_blank" rel="noreferrer">
          {hero.primaryCTA.label}
          <svg><!-- arrow icon --></svg>
        </a>
        <button id="waitlist-toggle" class="btn btn-secondary" aria-controls="waitlist-form" aria-expanded="false">
          {hero.secondaryCTA.label}
        </button>
        <a class="btn btn-text" href={hero.tertiaryCTA.href} target="_blank" rel="noreferrer">
          <svg><!-- github --></svg> {hero.tertiaryCTA.label}
        </a>
      </div>
      <div class="status-badge">
        <span class="dot" />
        <span>{hero.statusBadge}</span>
      </div>
      <WaitlistForm client:load {...waitlistCopy} />
    </div>
    <HeroCodeBlock filename={hero.cueFilename} snippet={hero.cueSnippet} />
  </Container>
</section>

<script>
  const btn = document.getElementById('waitlist-toggle');
  btn?.addEventListener('click', () => {
    const open = btn.getAttribute('aria-expanded') === 'true';
    btn.setAttribute('aria-expanded', String(!open));
    document.dispatchEvent(new CustomEvent('waitlist:toggle', { detail: !open }));
  });
</script>

<style>
  /* Скопировать .hero, .hero-grid, .hero h1 .accented, .hero .subhead, .hero .ctas, .status-badge, .status-badge .dot, .btn-*, .btn из styles.css строки 222-261 и 182-217 */
</style>
```

В `WaitlistForm.svelte` добавить:
```ts
let open = $state(false);
$effect(() => {
  const onToggle = (e: Event) => { open = (e as CustomEvent).detail; };
  document.addEventListener('waitlist:toggle', onToggle);
  return () => document.removeEventListener('waitlist:toggle', onToggle);
});
```
И обернуть всю форму в `{#if open}…{/if}`.

> **Headline-HTML:** в CMS `headline` хранится как richText. `getLandingHero()` конвертирует Lexical-JSON в санитайз-HTML через `@payloadcms/richtext-lexical` build-time renderer. Локально (fallback) `landingHero.json` уже отдаёт готовый HTML-фрагмент `"One manifest. From a <span class='accented'>€5 VPS</span> to production. Open source."`.

### Task E6: Включить hero в `index.astro` и smoke-проверить

- [ ] **Step 1:** В `landing/web/src/pages/index.astro` импортировать `Hero.astro`, обернуть в `BaseLayout` (заглушка). Все остальные секции пока не вставляем.
- [ ] **Step 2:** `bun run dev:web`, открыть http://localhost:4321:
  - Hero виден.
  - Code-блок отрендерен.
  - Click "Copy" → текст в буфере (проверить вручную).
  - Click "Notify me on managed launch" → форма раскрылась.
  - Submit с валидным email → должен лететь POST (т.к. WaitlistSignups коллекция ещё не настроена, Payload вернёт 404 — это OK, фиксим в Phase H). Пока проверяем только UI.
- [ ] **Step 3: Commit** `feat(landing/web): hero with code snippet + waitlist island`

---

## Phase F — Content sections

**Цель:** Перевести каждую секцию из `sections.jsx` в отдельный Astro-компонент в `landing/web/src/components/sections/`. Контент пока — fallback JSON; CMS-обвязку добавим в Phase H. Каждая секция самодостаточна: scoped-стили из соответствующего блока в `design-source/styles.css`, props из `lib/cms.ts`.

### Принцип портирования (общий для всех F-tasks)

- Один Astro-файл на секцию (`<Section>.astro`).
- Структура HTML — 1:1 с JSX (только React-фрагменты → Astro slot/markup, `className` → `class`, `htmlFor` → `for`).
- Стили — `<style>` в нижней части файла, копия соответствующего блока из `design-source/styles.css`. Никаких глобальных селекторов кроме тех, что уже в `global.css`.
- Контент — из `await getXxx()` импорта из `lib/cms.ts`. В Phase F клиент возвращает только локальный JSON.
- Адаптив — media-queries копируются в каждый компонент в соответствующей части (или внизу `<style>` для всего файла, как сделано в design-source).

### Task F1: `ValueProps.astro`

**Files:**
- Create: `landing/web/src/components/sections/ValueProps.astro`
- Create: `landing/web/src/data/fallback/valueProps.json`

**Source:** `design-source/sections.jsx:277-345`.

- [ ] **Step 1: JSON fallback** — три объекта `{ title, body, iconName }`. `body` хранится как HTML-строка (включая `<code class="mono">` инлайны), потому что в JSX там JSX-фрагмент с `<code>` тегами.
- [ ] **Step 2: Astro-компонент** — `<section>` с `aria-label`, `data-screen-label`, внутри `<Container>`, `<SectionHead>`, `<div class="value-grid">` с тремя `.value-card`.
- [ ] **Step 3:** Иконки — три встроенных `<svg>` в `IconSet.astro` (или просто три hard-coded SVG в компоненте по `iconName`).
- [ ] **Step 4: Стили `.section-head`, `.value-grid`, `.value-card`, `.value-card .index`, `.value-card .icon`, h3/p внутри** — из `design-source/styles.css` строки 338-388.

### Task F2: `ScalingJourney.astro`

**Files:**
- Create: `landing/web/src/components/sections/ScalingJourney.astro`
- Create: `landing/web/src/data/fallback/scalingJourney.json`

**Source:** `sections.jsx:597-666` + стили строки 749-905.

Самая сложная секция визуально — диаграмма с solo-нодом → стрелка → кластер. Структура:

```html
<section><Container>
  <SectionHead>…</SectionHead>
  <div class="journey">
    <div class="journey-side">
      <div class="journey-eyebrow">Tier 1 · €5 VPS</div>
      <div class="journey-stage">
        <div class="journey-node solo"><span class="node-label">node-1</span></div>
        <div class="journey-file">…</div>
      </div>
      <div class="journey-caption">…</div>
    </div>
    <div class="journey-arrow">
      <div class="arrow-bar"><span class="arrow-label">Tier upgrade</span></div>
      <svg><!-- arrow-head --></svg>
    </div>
    <div class="journey-side">…</div>
  </div>
  <div class="journey-footer">
    <span class="footer-kicker">No rewrite.</span>
    …
  </div>
  <div class="journey-caveat">…</div>
</Container></section>
```

- [ ] **Step 1: JSON fallback** — leftStage (eyebrow, file, caption), rightStage (eyebrow, cluster=[cp/w nodes], file, caption), footerKickers, caveat (HTML).
- [ ] **Step 2: Astro-компонент** — портировать структуру.
- [ ] **Step 3: Стили** — из styles.css строки 749-905 (большой блок).
- [ ] **Step 4: Адаптив `@media (max-width: 1080px)` для `.journey` — превратиться в одну колонку, стрелка поворачивается на 90°. Уже описано в design-source.

### Task F3: `TierLadder.astro`

**Files:**
- Create: `landing/web/src/components/sections/TierLadder.astro`
- Create: `landing/web/src/data/fallback/tierLadder.json`

**Source:** `sections.jsx:350-423` + стили 392-504.

- [ ] **Step 1: JSON fallback** — `cards: [{ num, title, price, desc, status, statusText }]` (4 шт), `orthogonalNote` (HTML).
- [ ] **Step 2: Astro-компонент** — `.ladder-wrap`, внутри `.stair` (4 разной высоты степени, `aria-hidden`), `.tier-cards` (grid 4 cols), `.orthogonal-note`.
- [ ] **Step 3: Стили** — строки 392-504. Адаптив: `@media (max-width: 1080px)` — `.tier-cards` 2 cols, `.stair` `display: none`. `@media (max-width: 720px)` — `.tier-cards` 1 col.

### Task F4: `Comparison.astro`

**Files:**
- Create: `landing/web/src/components/sections/Comparison.astro`
- Create: `landing/web/src/data/fallback/comparison.json`
- Create: `landing/web/src/data/fallback/transparency.json`

**Source:** `sections.jsx:428-525` + стили 510-662.

Таблица 3 колонки + footnote. Под таблицей — 3 transparency-card'а.

- [ ] **Step 1: JSON fallbacks** — comparison.rows (6 шт с HTML в ячейках), columns с status-pill'ами; transparency.blocks (3 шт с kicker, title, body-HTML).
- [ ] **Step 2: Astro-компонент** — `<table class="compare-table">` с `<thead>`, `<tbody>`. Последняя строка — `class="status-row"` с тремя `<StatusPill kind={…} />`.
- [ ] **Step 3: Стили** — строки 510-662 (compare-table, status-pill, footnote, transparency-grid, transparency-card). Адаптив 720px переворачивает таблицу в стек.

### Task F5: `BoringTech.astro`

**Files:**
- Create: `landing/web/src/components/sections/BoringTech.astro`
- Create: `landing/web/src/data/fallback/boringTech.json`

**Source:** `sections.jsx:530-592` + стили 668-713.

- [ ] **Step 1: JSON fallback** — underHood (массив `[name, desc]`), ourCode (массив), opening, closing.
- [ ] **Step 2: Astro-компонент** — `.boring-grid` 2 колонки, каждая колонка — `<h3>`-шапка + `<ul class="tech-list">`. В ourCode `.name` обёрнуто `<span class="ours">{name}</span>` (teal).
- [ ] **Step 3: Стили** — строки 668-713.

### Task F6: `Advantages.astro`

**Files:**
- Create: `landing/web/src/components/sections/Advantages.astro`
- Create: `landing/web/src/data/fallback/advantages.json`

**Source:** `sections.jsx:671-741` + стили 717-746 и 906-941.

- [ ] **Step 1: JSON fallback** — 5 блоков с `title`, `lead` (HTML включая `<strong>` и inline `<code>`), `detail`, `phaseTag`, `featured: bool`. 5-й блок (KFM #3) featured=true.
- [ ] **Step 2: Astro-компонент** — `<div class="advantages">` grid 2 cols, последний блок (featured) `grid-column: 1 / -1`.
- [ ] **Step 3: Стили** — строки 717-746 + 906-941. Адаптив 1080px — 1 col.

### Task F7: `Roadmap.astro`

**Files:**
- Create: `landing/web/src/components/sections/Roadmap.astro`
- Create: `landing/web/src/data/fallback/roadmap.json`

**Source:** `sections.jsx:746-814` + стили 947-1004.

- [ ] **Step 1: JSON fallback** — 4 фазы с `num`, `title`, `items[]`. Closing line.
- [ ] **Step 2: Astro-компонент** — `<div class="roadmap">` → `<div class="roadmap-phase" id={'roadmap-phase-…'}>` × 4. Каждая фаза — meta (sticky) + ul.
- [ ] **Step 3: Anchor stability** — `id="roadmap-phase-8"` для линковки из Comparison секции. Slug-формула из JSX: `p.num.toLowerCase().replace(/\W+/g, "-")`.
- [ ] **Step 4: Стили** — строки 947-1004.

### Task F8: `BootstrapStrip.astro`

**Files:**
- Create: `landing/web/src/components/sections/BootstrapStrip.astro`
- Create: `landing/web/src/data/fallback/bootstrapStrip.json`

**Source:** `sections.jsx:819-825` + стили 1078-1085.

Простая полоска поверх футера. Один абзац italic с copy «AppRafter is a bootstrap project…».

- [ ] **Commit после Phase F:** `feat(landing/web): all content sections ported from design`

---

## Phase G — Page composition

**Цель:** Собрать `BaseLayout.astro` со всем `<head>`, объединить все секции на `index.astro`, добавить SEO-метатеги и JSON-LD.

### Task G1: `BaseLayout.astro`

**Files:**
- Create: `landing/web/src/components/layout/BaseLayout.astro`

```astro
---
// SPDX-FileCopyrightText: 2026 AppRafter contributors
// SPDX-License-Identifier: FSL-1.1-Apache-2.0
import { THEME_INIT_SCRIPT } from '../../lib/theme-init';
import '../../styles/index.css';

type Props = {
  title: string;
  description: string;
  canonical?: string;
  ogImage?: string;
};
const { title, description, canonical = Astro.url.href, ogImage = '/og-image.png' } = Astro.props;
---
<!doctype html>
<html lang="en">
  <head>
    <meta charset="utf-8" />
    <meta name="viewport" content="width=device-width, initial-scale=1" />
    <title>{title}</title>
    <meta name="description" content={description} />
    <link rel="canonical" href={canonical} />

    <meta property="og:site_name" content="AppRafter" />
    <meta property="og:type" content="website" />
    <meta property="og:url" content={canonical} />
    <meta property="og:title" content={title} />
    <meta property="og:description" content={description} />
    <meta property="og:image" content={new URL(ogImage, Astro.url).href} />

    <meta name="twitter:card" content="summary_large_image" />
    <meta name="twitter:title" content={title} />
    <meta name="twitter:description" content={description} />
    <meta name="twitter:image" content={new URL(ogImage, Astro.url).href} />

    <link rel="icon" type="image/svg+xml" href="/favicon.svg" />
    <link rel="icon" type="image/png" sizes="16x16" href="/favicon-16.png" />
    <link rel="apple-touch-icon" sizes="180x180" href="/apple-touch-icon.png" />

    <link rel="preload" href="/fonts/roboto-400.woff2" as="font" type="font/woff2" crossorigin />

    <script is:inline set:html={THEME_INIT_SCRIPT} />

    <slot name="head" />
  </head>
  <body>
    <slot />
  </body>
</html>
```

### Task G2: `index.astro` — собрать всё

**Files:**
- Modify: `landing/web/src/pages/index.astro`

```astro
---
import BaseLayout from '../components/layout/BaseLayout.astro';
import Header from '../components/layout/Header.astro';
import Footer from '../components/layout/Footer.astro';
import Hero from '../components/hero/Hero.astro';
import ValueProps from '../components/sections/ValueProps.astro';
import ScalingJourney from '../components/sections/ScalingJourney.astro';
import TierLadder from '../components/sections/TierLadder.astro';
import Comparison from '../components/sections/Comparison.astro';
import BoringTech from '../components/sections/BoringTech.astro';
import Advantages from '../components/sections/Advantages.astro';
import Roadmap from '../components/sections/Roadmap.astro';
import BootstrapStrip from '../components/sections/BootstrapStrip.astro';

const title = 'AppRafter — One manifest. From a €5 VPS to production. Open source.';
const description = 'AppRafter is an opinionated PaaS on Kubernetes. Describe your applications in a single CUE manifest — the same one runs from a single VDS to a multi-node production cluster. Open source (FSL-1.1-Apache-2.0).';
---
<BaseLayout {title} {description}>
  <Fragment slot="head">
    <script type="application/ld+json" set:html={JSON.stringify({
      '@context': 'https://schema.org',
      '@type': 'Organization',
      name: 'AppRafter',
      url: 'https://apprafter.dev',
      logo: 'https://apprafter.dev/favicon.svg',
      sameAs: ['https://github.com/AppRafter/apprafter'],
    })} />
    <script type="application/ld+json" set:html={JSON.stringify({
      '@context': 'https://schema.org',
      '@type': 'SoftwareApplication',
      name: 'AppRafter',
      operatingSystem: 'Linux',
      applicationCategory: 'DeveloperApplication',
      offers: { '@type': 'Offer', price: '0', priceCurrency: 'EUR' },
      description,
    })} />
  </Fragment>

  <Header />
  <main>
    <Hero />
    <ValueProps />
    <ScalingJourney />
    <TierLadder />
    <Comparison />
    <BoringTech />
    <Advantages />
    <Roadmap />
    <BootstrapStrip />
  </main>
  <Footer />
</BaseLayout>
```

### Task G3: Smoke-проверка собранной страницы

- [ ] **Step 1: `bun run dev:web`**, открыть `/`. Ожидается полная страница с темой по умолчанию, всеми 9 секциями, ThemeToggle работает, copy-кнопка в hero работает, кнопка waitlist раскрывает форму.
- [ ] **Step 2: Pixel-проверка против дизайн-бандла:** открыть `landing/.design-bundle/apprafter-v2/project/index.html` в отдельной вкладке (это React-prototype, работает напрямую) → сравнить визуально с локальным `/`. Ожидаются мелкие отклонения (например, шрифты — Google CDN vs. self-hosted), но layout, цвета, размеры должны совпадать.
- [ ] **Step 3:** Проверить в обеих темах и на ширине 1080/720/400.

### Task G4: Commit

```bash
git commit -am "feat(landing/web): full page composition with all sections + SEO meta"
```

---

## Phase H — CMS integration

**Цель:** Реализовать все Payload collections и globals из §3, типизированный клиент `cms.ts`, fallback к JSON, hook отправки discovery-call email'а, seed-скрипт.

### Task H1: Расширить `payload.config.ts`

**Files:**
- Modify: `landing/cms/src/payload.config.ts`
- Create: 4 файла в `landing/cms/src/collections/` (Users, WaitlistSignups, LegalPages, BlogPosts) — Users уже есть из Phase A.
- Create: 14 файлов в `landing/cms/src/globals/` (см. §3.2)

- [ ] **Step 1: Создать каждый файл коллекции/глобала** с типизированной конфигурацией (~30-60 строк каждый). Используем `import type { CollectionConfig, GlobalConfig } from 'payload'`.
- [ ] **Step 2:** В `payload.config.ts` импортировать все и зарегистрировать в `collections: […]` и `globals: […]`.
- [ ] **Step 3: `cors`** — добавить `http://localhost:4321` (dev) и `https://apprafter.dev` (prod).

### Task H2: Hook `sendDiscoveryEmail`

**Files:**
- Create: `landing/cms/src/hooks/sendDiscoveryEmail.ts`
- Create: `landing/cms/src/lib/mailer.ts`

- [ ] **Step 1: `mailer.ts`** — обёртка nodemailer transporter, читает SMTP_* env vars.
- [ ] **Step 2: `sendDiscoveryEmail.ts`** — `afterChange` hook на `WaitlistSignups`. Если `doc.wantsCall === true && doc.callEmailSentAt === null`:
  1. Загрузить `Booking` global → `discoveryCallUrl`, шаблон.
  2. Послать email через `mailer.ts`.
  3. Обновить документ `callEmailSentAt = new Date()` через `req.payload.update(...)`.
- [ ] **Step 3:** Обрабатывать ошибки логированием, не падать в throw (иначе создание signup упадёт, что плохо).

### Task H3: TypeScript типы из Payload

- [ ] **Step 1: Запустить `bun --filter @apprafter/landing-cms run payload generate:types`** — создаст `landing/cms/payload-types.ts`.
- [ ] **Step 2:** Создать re-export в `landing/web/src/lib/types.ts`:
  ```ts
  // SPDX-FileCopyrightText: 2026 AppRafter contributors
  // SPDX-License-Identifier: FSL-1.1-Apache-2.0
  export type { LandingHero, ValueProps, ScalingJourney, TierLadder, Comparison,
    LandingTransparency, BoringTech, Advantages, Roadmap, BootstrapStrip,
    FooterContent, WaitlistFormCopy, Booking, SiteSettings, LegalPage, BlogPost,
    WaitlistSignup } from '../../../cms/payload-types';
  ```

### Task H4: CMS клиент `cms.ts`

**Files:**
- Create: `landing/web/src/lib/cms.ts`

```ts
// SPDX-FileCopyrightText: 2026 AppRafter contributors
// SPDX-License-Identifier: FSL-1.1-Apache-2.0
import type * as Types from './types';

const CMS_URL = import.meta.env.PUBLIC_CMS_URL || 'http://localhost:3000';

async function fetchOrFallback<T>(path: string, fallbackName: string): Promise<T> {
  try {
    const res = await fetch(`${CMS_URL}/api${path}`);
    if (!res.ok) throw new Error(`CMS ${path} → ${res.status}`);
    return (await res.json()) as T;
  } catch (err) {
    if (import.meta.env.PROD) throw err;
    // dev fallback
    const mod = await import(`../data/fallback/${fallbackName}.json`);
    return mod.default as T;
  }
}

export const getSiteSettings = () => fetchOrFallback<Types.SiteSettings>('/globals/site-settings', 'siteSettings');
export const getLandingHero = () => fetchOrFallback<Types.LandingHero>('/globals/landing-hero', 'landingHero');
export const getValueProps = () => fetchOrFallback<Types.ValueProps>('/globals/value-props', 'valueProps');
export const getScalingJourney = () => fetchOrFallback<Types.ScalingJourney>('/globals/scaling-journey', 'scalingJourney');
export const getTierLadder = () => fetchOrFallback<Types.TierLadder>('/globals/tier-ladder', 'tierLadder');
export const getComparison = () => fetchOrFallback<Types.Comparison>('/globals/comparison', 'comparison');
export const getTransparency = () => fetchOrFallback<Types.LandingTransparency>('/globals/landing-transparency', 'transparency');
export const getBoringTech = () => fetchOrFallback<Types.BoringTech>('/globals/boring-tech', 'boringTech');
export const getAdvantages = () => fetchOrFallback<Types.Advantages>('/globals/advantages', 'advantages');
export const getRoadmap = () => fetchOrFallback<Types.Roadmap>('/globals/roadmap', 'roadmap');
export const getBootstrapStrip = () => fetchOrFallback<Types.BootstrapStrip>('/globals/bootstrap-strip', 'bootstrapStrip');
export const getFooterContent = () => fetchOrFallback<Types.FooterContent>('/globals/footer-content', 'footer');
export const getWaitlistCopy = () => fetchOrFallback<Types.WaitlistFormCopy>('/globals/waitlist-form-copy', 'waitlistCopy');

export async function getLegalPage(slug: string): Promise<Types.LegalPage | null> {
  try {
    const res = await fetch(`${CMS_URL}/api/legal-pages?where[slug][equals]=${encodeURIComponent(slug)}&limit=1`);
    if (!res.ok) return null;
    const data = await res.json() as { docs: Types.LegalPage[] };
    return data.docs[0] ?? null;
  } catch {
    return null;
  }
}
```

> В проде (`import.meta.env.PROD`) fallback не используется — если Payload недоступен, build падает. Это правильное поведение per implementation-task §8 «Throws on error during build».

### Task H5: Lexical → HTML render

**Files:**
- Create: `landing/web/src/lib/lexical-to-html.ts`

Для полей `richText` (headline, body advantages, transparency etc.) Payload отдаёт Lexical JSON. Конвертируем в санитайз-HTML на build-time.

- [ ] **Step 1: Установить `@payloadcms/richtext-lexical/server`** или использовать минимальный walker над node tree.
- [ ] **Step 2: Реализовать функцию `lexicalToHtml(node)`** обходом дерева. Поддерживаем: text (с format-битмаской → strong/em), paragraph, link, code (inline и block), heading.

  Альтернатива: используем `@payloadcms/richtext-lexical` HTML serializer (если есть в v3) — это путь меньшего сопротивления.

### Task H6: Wire каждой секции к CMS

- [ ] **Step 1: Header** → `getSiteSettings()` (уже сделано в Phase D).
- [ ] **Step 2: Hero** → `getLandingHero()` + `getWaitlistCopy()` (уже сделано в Phase E).
- [ ] **Step 3: ValueProps, ScalingJourney, TierLadder, Comparison, Transparency, BoringTech, Advantages, Roadmap, BootstrapStrip, Footer** — заменить hardcoded на `await getXxx()`.
- [ ] **Step 4:** Где в дизайне есть inline-HTML (`<strong>`, `<code>` и т.п.) — рендерим через `set:html` с предварительной фильтрацией через lexical-to-html.

### Task H7: Seed-скрипт

**Files:**
- Create: `landing/cms/src/seed/seed.ts`

```ts
// SPDX-FileCopyrightText: 2026 AppRafter contributors
// SPDX-License-Identifier: FSL-1.1-Apache-2.0
import { getPayload } from 'payload';
import config from '../payload.config';
import fs from 'node:fs/promises';
import path from 'node:path';

async function main() {
  const payload = await getPayload({ config });
  const fallbackDir = path.resolve(__dirname, '../../../web/src/content/fallback');
  const files = await fs.readdir(fallbackDir);
  for (const file of files) {
    if (!file.endsWith('.json')) continue;
    const slug = file.replace(/\.json$/, '');
    const data = JSON.parse(await fs.readFile(path.join(fallbackDir, file), 'utf8'));
    // map filename → global slug (siteSettings.json → site-settings)
    const globalSlug = slug.replace(/([A-Z])/g, '-$1').toLowerCase().replace(/^-/, '');
    await payload.updateGlobal({ slug: globalSlug, data });
    console.log(`seeded global ${globalSlug}`);
  }
  process.exit(0);
}

main().catch(e => { console.error(e); process.exit(1); });
```

- [ ] **Step 1:** Создать скрипт.
- [ ] **Step 2: Прогнать локально**: `bun --filter @apprafter/landing-cms run seed`.
- [ ] **Step 3: Проверить в Payload admin** (`http://localhost:3000/admin`) — все globals заполнены данными из fallback'ов.

### Task H8: E2E прогон Astro ← Payload

- [ ] **Step 1: Запустить параллельно** Payload (`bun run dev:cms`) и Astro (`bun run dev:web`).
- [ ] **Step 2:** Открыть `/` — текст должен прийти из CMS, не из fallback. Проверить через изменение текста в admin → reload `/`.
- [ ] **Step 3:** Остановить Payload, перезагрузить `/` в dev — должен показаться fallback.
- [ ] **Step 4: Build-режим:** `bun run build:web` без запущенного Payload → expected: build падает. С запущенным Payload → проходит.

### Task H9: Тестировать `/api/waitlist-signups`

- [ ] **Step 1:** Отправить тестовый signup через UI hero waitlist.
- [ ] **Step 2:** Проверить в Payload admin → запись появилась.
- [ ] **Step 3:** Поставить `wantsCall=true` — проверить, что `callEmailSentAt` обновился (потребует SMTP credentials; можно протестировать с MailHog: `docker run -p 1025:1025 -p 8025:8025 mailhog/mailhog`).

### Task H10: Commit

```bash
git commit -am "feat(landing): full CMS integration with Payload globals + waitlist + seed"
```

---

## Phase I — Polish

### Task I1: sitemap + robots.txt

- [ ] **Step 1:** Astro sitemap-плагин уже подключен в `astro.config.ts` (Phase A4). На build он автогенерит `/sitemap-index.xml`. Проверить `bun run build:web && ls landing/web/dist/sitemap-*.xml`.
- [ ] **Step 2: `landing/web/public/robots.txt`**
  ```
  User-agent: *
  Allow: /
  Sitemap: https://apprafter.dev/sitemap-index.xml
  ```

### Task I2: 404 + [...slug] catch-all

**Files:**
- Create: `landing/web/src/pages/404.astro` — простая страница с brand + "Page not found" + ссылкой на `/`.
- Create: `landing/web/src/pages/[...slug].astro` — пытается `getLegalPage(slug)`, если null → `Astro.redirect('/404')`. Иначе рендерит `<BaseLayout>` с `<Container>` и `set:html={lexicalToHtml(page.body)}`.

### Task I3: Privacy + Terms stubs

**Files:**
- Create: `landing/web/src/pages/privacy.astro`
- Create: `landing/web/src/pages/terms.astro`

Каждая страница:
```astro
---
import BaseLayout from '../components/layout/BaseLayout.astro';
import { getLegalPage } from '../lib/cms';
import { lexicalToHtml } from '../lib/lexical-to-html';
const page = await getLegalPage('privacy');
---
<BaseLayout title={page?.title ?? 'Privacy'} description="…">
  …
</BaseLayout>
```

Если в CMS пусто — показать «Privacy policy is in preparation. See LICENSE for now.»

### Task I4: OG image

**Files:**
- Create: `landing/web/public/og-image.png` (1200×630, dark theme, logo + tagline)

- [ ] **Step 1:** Использовать Playwright/satori для генерации на build-time. Или сделать вручную в Figma и закоммитить как PNG. Для v1 — ручной PNG, чтобы не плодить deps.

### Task I5: Accessibility audit

- [ ] **Step 1: axe-core CLI** (`bunx @axe-core/cli http://localhost:4321`) — пройти, исправить ошибки.
- [ ] **Step 2: Keyboard nav** — все интерактивные элементы должны быть достижимы Tab'ом. Focus styles визуально различимы (2px solid var(--accent)). Добавить в `global.css`:
  ```css
  :focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }
  ```
- [ ] **Step 3: Skip-to-content link** в BaseLayout (как первый элемент `<body>`).

### Task I6: Lighthouse 95+

- [ ] **Step 1: Build & preview:** `bun run build:web && bun run --filter @apprafter/landing-web preview`.
- [ ] **Step 2: Chrome → Lighthouse → Mobile + Performance/A11y/Best/SEO**. Цель 95+ во всех четырёх.
- [ ] **Step 3:** Типичные проблемы:
  - LCP > 2.5s → проверить preload Roboto, размер hero image (если он есть как раster).
  - CLS > 0 → fonts уже preload'ены, но проверить `font-display: swap` vs `optional`. При swap CLS может быть; рассмотреть `font-display: optional` для Roboto Mono.
  - Unused JS — проверить, что Svelte-острова action'ятся только нужным `client:*`.

### Task I7: Smoke на мобильных размерах

- [ ] **Step 1:** Chrome DevTools → iPhone SE (375px), iPhone 14 (390px), iPad (768px), Desktop (1280px, 1920px).
- [ ] **Step 2:** Все секции читаемы, нет горизонтального скролла, hero stack'ается, comparison-таблица в "карточки" режиме.

### Task I8: Commit

```bash
git commit -am "feat(landing): sitemap, 404, legal stubs, a11y, Lighthouse pass"
```

---

## Phase J — CI + Docs + Deploy

### Task J1: GitHub Actions workflow — **template only** (per §5.1, не активируем)

**Files:**
- Create: `landing/ci/landing-ci.example.yml` (template, не активный)
- Create: `landing/ci/README.md` (как активировать)

Per §5.1 нам запрещено создавать `/projects/omnixal/apprafter/.github/workflows/`. Кладём workflow-template как `landing/ci/landing-ci.example.yml`; пользователь по своей готовности скопирует его в `.github/workflows/landing.yml` или попросит отдельно.

```yaml
# Copy this to /.github/workflows/landing.yml at the repo root to enable.
name: Landing CI

on:
  push:
    branches: [master, main]
    paths: ['landing/**']
  pull_request:
    paths: ['landing/**']

jobs:
  ci:
    runs-on: ubuntu-latest
    defaults:
      run: { working-directory: landing }
    steps:
      - uses: actions/checkout@v4
      - uses: oven-sh/setup-bun@v1
        with: { bun-version: latest }
      - run: bun install
      - run: bun run lint
      - run: bun run typecheck
      - run: bun run build
```

> Path-filter ограничивает CI только когда меняется `landing/`. Постгрес для cms-build НЕ нужен — `next build` не подключается к БД на build-time.

### Task J2: README.md

**Files:**
- Modify: `landing/README.md`

Заполнить:
- Что это (landing for apprafter.dev)
- Структура (`web/`, `cms/`, `docs/`)
- Quickstart (clone → `bun install` → `docker compose up postgres` → `cp cms/.env.example cms/.env` → `bun run dev:cms` → `bun run dev:web` → создать админа → `bun run seed`)
- Edit content (где в Payload что лежит)
- Production build
- Ссылки на: implementation plan, DEPLOY.md, LANDING_BRIEF v2.2

### Task J3: DEPLOY.md

**Files:**
- Modify: `landing/DEPLOY.md`

Recommendation per implementation-task §12 — self-host на Hetzner VPS того же проекта.

Описать:
- **Topology:** Caddy reverse proxy → `apprafter.dev` (статика web/dist) + `cms.apprafter.dev` (Payload Node-процесс на :3000) + Postgres контейнер.
- **Build artifacts:** `landing/web/dist/` — static, кладётся в `/var/www/landing` и раздаётся Caddy. `landing/cms/.next/` — собранное Next-приложение, запускается `next start` в systemd-unit или podman-контейнере.
- **Caddyfile** (snippet):
  ```
  apprafter.dev {
    root * /var/www/landing
    file_server
    encode gzip zstd
  }
  cms.apprafter.dev {
    reverse_proxy localhost:3000
  }
  ```
- **Environment vars** для cms прода (DATABASE_URI, PAYLOAD_SECRET, SMTP_*, PAYLOAD_PUBLIC_SERVER_URL=https://cms.apprafter.dev).
- **Static rebuild trigger:** Payload webhook на change → GitHub Actions → rebuild `landing/web/` → `rsync` в VPS. Или ручной `bun run build:web && rsync dist/ vps:/var/www/landing/`.
- **Postgres backup:** `pg_dump` cron, target — стандартный backup'ный pipeline проекта.

### Task J4: Final smoke + manual UX walk

- [ ] **Step 1: Поднять локально полный стек** (postgres + cms + web), пройти весь лендинг:
  - Все секции рендерятся.
  - Light/dark toggle работает в обе стороны, состояние сохраняется.
  - CUE-сниппет копируется.
  - Waitlist принимает email, спам-валидация работает (двойной submit не плодит signups — Payload `unique: true` на email возвращает 400).
  - При `wantsCall=true` — email уходит в MailHog (если SMTP настроен).
  - Navigation links не битые, anchor `#roadmap-phase-8` ведёт куда нужно.
  - Footer.legal: ссылка на License открывает GitHub LICENSE, Privacy/Terms ведут на свои страницы.
- [ ] **Step 2:** Проверить, что lint+typecheck+build проходят на чистом checkout:
  ```bash
  rm -rf landing/{web,cms}/node_modules landing/node_modules
  cd landing && bun install && bun run lint && bun run typecheck && bun run build
  ```
- [ ] **Step 3:** Сделать финальный коммит.

### Task J5: Commit

```bash
git commit -am "feat(landing): CI workflow + README + DEPLOY"
```

---

## 5. Open questions — resolved 2026-05-22

| ID | Вопрос | Ответ пользователя | Применено в плане |
|---|---|---|---|
| Q1 | Domain | `apprafter.dev` | `site:` в `astro.config.ts`, `canonical` в `BaseLayout`, JSON-LD, OG-meta — все уже зашиты в план |
| Q2 | Postgres для prod cms | Standalone, через `docker compose` | `landing/docker-compose.yml` + `landing/cms/.env.example` уже описаны в Task A5; в Phase J3 DEPLOY.md распишет тот же образ на VPS |
| Q3 | Discovery-call calendar | Calendly | URL хранится в Payload `Booking.discoveryCallUrl` (см. §3.2) — без релиза правится |
| Q4 | SMTP provider | `nodemailer` пока | Task H2 `mailer.ts` — обёртка над nodemailer с SMTP_* env vars |
| Q5 | Аналитика — Plausible | **Нужна, но позже** (после v1) | НЕ ставим в v1, см. **Deferred §5.1** ниже. В Payload `SiteSettings` добавляем поле `plausibleDomain: text` сразу — чтобы потом включить одной правкой в админке |
| Q6 | SPDX для `landing/` | `FSL-1.1-Apache-2.0` (см. ADR 0032) | Все SPDX-заголовки в плане **уже обновлены** на `FSL-1.1-Apache-2.0`. В Task A2 — проверка `scripts/check-spdx-headers.sh` только на чтение, без модификаций. |
| Q7 | Status badge | `v0.3 · Phase 3 · MVP shipped on Tier 1 and Tier 2 · managed in development` (хвост из дизайна) | Дефолт в `landingHero.json` fallback. CMS-поле остаётся редактируемым. |
| Q8 | License в копирайте | FSL-1.1-Apache-2.0 (ADR 0032 переключил core с FSL-1.1-MIT, плагины остаются на MIT) | Все упоминания «FSL-1.1-MIT» в footer / pricing footnote / стек заменены на «FSL-1.1-Apache-2.0». Источник истины — `/projects/omnixal/apprafter/docs/adr/0032-license-fsl-1-1-apache-2-0.md` (read-only reference). |

### 5.1 Жёсткое ограничение объёма правок

**Пользователь явно запретил трогать что-либо за пределами `landing/`** (2026-05-22). Что это меняет относительно изначального плана:

- ❌ **Не трогаем** корневой `/projects/omnixal/apprafter/.gitignore`. Вместо этого `.design-bundle/` игнорируем через `landing/.gitignore` (он и так создаётся в Task A3).
- ❌ **Не создаём** `/projects/omnixal/apprafter/.github/workflows/landing.yml` в Phase J. Вместо этого кладём `landing/ci/landing-ci.example.yml` со словом «example» в имени и блок-комментарием «move to `.github/workflows/landing.yml` to enable». Пользователь сам перенесёт.
- ❌ **Не правим** `scripts/check-spdx-headers.sh`, даже если `landing/` там не покрыт. Если выяснится, что не покрыт — в Task A2 кладём `landing/.spdx-check.sh` для локального запуска, который применит ту же проверку только к landing-tree.
- ✅ **Делаем** всё внутри `landing/`: подкаталоги, конфиги, исходники, документация. ADR `docs/adr/0032-…` — только чтение для справки.

### 5.2 Перенесённые в v1+1 решения (Deferred)

- **Plausible analytics** (Q5) — отдельная мини-итерация после первого деплоя. Поле `plausibleDomain` в `SiteSettings` global уже зарезервировано в §3.2. Включается через инжект скрипта в `BaseLayout.astro` под `{settings.plausibleDomain && ...}`. **Не делаем в Phase G**, ставим напоминание в `Open questions` нового плана v1+1.

---

## 6. Принципы исполнения (для будущих сессий)

- **Бите-sized шаги.** Каждый чекбокс — 2-5 минут работы. Если шаг занимает дольше — разделить.
- **Один коммит на task.** Завершил Task X → коммит. Это даёт rollback на любой уровень и читаемую историю.
- **Сверка с design-source.** При любых сомнениях о пикселях/копирайте — открыть `landing/docs/design-source/sections.jsx` или `LANDING_BRIEF_v2.2.md`. Не изобретать.
- **Не трогать `tweaks-panel.jsx`** и не вытаскивать оттуда тюнинги (accent presets, density modes). Это design-time только.
- **Lint/typecheck перед каждым коммитом.** `cd landing && bun run lint && bun run typecheck` (см. memory `feedback_cue_bin_local.md` — пользователь явно требует прогон чек-листа перед commit).
- **SPDX в каждом исходнике** (см. Task A2, A6 и `scripts/check-spdx-headers.sh`).
- **Push policy.** Агент коммитит **локально**. `git push` делает пользователь, или явный запрос (см. `feedback_push_policy.md`).

---

## 7. История исполнения

> Заполняется по мере прогресса. Формат: `YYYY-MM-DD HH:MM — Task X.Y — описание — commit-hash`.

| Дата/Время | Task | Описание | Commit |
|---|---|---|---|
| 2026-05-22 03:30 | — | Plan created | — |
| 2026-05-22 03:43 | A1 | Archive v1 briefs, copy v2.2 design-source | 667a807 |
| 2026-05-22 03:48 | A2+A3 | Workspace root + SPDX helpers | f2e062b |
| 2026-05-22 03:57 | A4+A5+A6 | Astro + Payload scaffolds, both typecheck clean, web build green | fe31c5b |
| 2026-05-22 04:03 | B1–B7 | tokens/reset/fonts/global/index CSS + theme-init script | be484af |
| 2026-05-22 04:03 | — | `src/content/fallback` renamed → `src/data/fallback` (Astro auto-collection avoidance) | (rolled into fe31c5b) |
| 2026-05-22 04:06 | C1–C5 | LogoMark/Wordmark/Brand/Container/Eyebrow/SectionHead/StatusPill + ThemeToggle island | 1f21a68 |
| 2026-05-22 04:10 | D1–D2 | Header (sticky, scroll-border, nav) + Footer (3 cols, FSL note) | 0076b3c |
| 2026-05-22 — | — | docs: progress checkboxes ticked A-D | 14a9e56 |
| 2026-05-22 — | CI | per-workspace lint scripts + smoke tests; workflow ignores Bun workspace children | 45f3bce, dca51b5 |
| 2026-05-22 — | — | env-driven ports for web + cms (LANDING_*_PORT, LANDING_CMS_URL, LANDING_CMS_CORS_ORIGINS) | 2e488e8 |
| 2026-05-22 — | E1–E6 | cue-highlight build-time tokenizer + HeroCodeBlock + CodeCopyButton + WaitlistForm + Hero composition | a63c09e |
| 2026-05-22 — | F1–F8 | All eight content sections ported from design 1:1 | ceb5ce9 |
| 2026-05-22 — | G1–G4 | BaseLayout extracted; index uses named head slot; JSON-LD Organization + SoftwareApplication; full SEO surface | e2ee391 |
| 2026-05-22 — | H part 1 | WaitlistSignups collection + Booking global + sendDiscoveryEmail hook + mailer.ts; live POST verified end-to-end through Vite proxy | b544b7a |
| 2026-05-22 — | H part 2 | 13 content globals + cms.ts client (typed getters + import.meta.glob fallback + fail-loud in prod) + 13 fallback JSONs + seed script + 11-component refactor to `await getXxx()` | 623f286 |
| 2026-05-22 — | I | robots.txt + 404.astro + privacy/terms stubs; sitemap auto-generated; prod build 4 pages, client JS ~24 KB gzipped | d1fb48e |
| 2026-05-22 — | J | Full README + DEPLOY.md (Caddy + systemd + Postgres container recipe per Q1/Q2) | 44aebe0 |
