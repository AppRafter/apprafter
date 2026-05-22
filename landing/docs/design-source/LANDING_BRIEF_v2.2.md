# AppRafter Landing — Brief v2.2

> **Версия:** 2.2
> **Дата:** 2026-05-14
> **Заменяет:** `LANDING_DESIGN_BRIEF.md` v1 (полностью), brief v2 / v2.1
> **Связан с:** `LANDING_IMPLEMENTATION_BRIEF.md` (technical stack — см. §11; требует minor patch для waitlist)
> **Входы:** `KILLER_FEATURES_MATRIX.md`, `PRELAUNCH_CHECKLIST.md`, `MANAGED_STRATEGY.md`, `PRICING_AND_LAUNCH_NOTES.md`, `PLATFORM_SPEC.md` (tier model, Application CUE schema)
> **Статус:** working draft для дизайн-работы. Финализируется до старта вёрстки.

---

## 0. Соглашения документа

- **Website copy** (всё что попадает на страницу: заголовки, value props, описания, CTA-лейблы, badges, button text) — **строго на английском**. Это анкорный язык лендинга на старте. RU-локаль не делаем (i18n-ready, но контролов и переводов нет).
- **Brief commentary** (обоснования, calibration, design rationale) — на русском. Это для внутреннего пользования.
- Все sample-цитаты ниже — это **готовые copy strings** для CMS / handoff. Не подменять при имплементации.

---

## 1. Что изменилось vs v1, v2 и v2.1

Старый бриф (v1) писался до проработки killer features matrix и трёхступенчатого managed roadmap. Промежуточная v2 не учитывала:
- ограничение поставки Hetzner-only на launch,
- честную модель видимости данных в Turnkey,
- KFM #21 caveats для Tier 1 (kine+sqlite, не NATS),
- business-model alignment как structural argument,
- discovery call opt-in для waitlist.

v2.1 — финализация выше.

v2.2 — точечные правки по anti-overcommit дисциплине (§5/§6) и factual accuracy после сессии review:

- **§4.3 Block 3** — softened "When you want managed — you'll get it" (implied certainty). Также убрана "there will be a CLI command for that" с настоящим временем.
- **§4.5 pricing table** — добавлен footnote под таблицей с честной формулировкой FSL: разрешено любое использование кроме предоставления AppRafter как managed service сторонним лицам.
- **§4.5.1 Pricing transparency** — убрана конкретная цена Tier 4 floor (€2,500/mo). Phase 6+ feature, конкретный anchor создавал expectation до того как ship'нется.
- **§4.5.2 Anti-vendor-lock** — Cross-cluster MigrationPlan: "is shipped tooling" → "planned as shipped tooling (Phase 8+)". Direct conflict с §5 disciplined. Также softened "AWS pretends migration is possible" — слишком partisan для основного брифа.
- **§4.6** — "kine + NATS adapter" удалён из "What we wrote ourselves": в реальности community kine-nats adapter упаковывается/настраивается, но не пишется. Заменён на честный "NATS-based audit log layer on top of kine" с пометкой что эта надстройка — наша работа.
- **§4.7 Block 1** — caveat про Tier 3/4 поднят из italic footnote в основной текст. Scan-reader может пропустить курсив, claim "runs in" в настоящем времени про не-shipped tiers — risk.
- **§4.7 Block 2** — "touches everything above it" → "a major architectural commitment". kine API etcd-compatible, "everything above" overstated.
- **§4.7 Block 3** — "unwinding their core business model" → "would conflict with their incentive structure". Architecturally могут, но incentive misaligned — точнее формулировка.
- **§4.9 footer strip** — "not against them" → "not at their expense". Менее partisan, factually точнее.
- **§5** — добавлен общий принцип: claims в настоящем времени только для Phase 1-3 features, future tense для Phase 4+.
- **Value props block 2 (§4.3)** — лёгкое усиление angle vertical-ceiling vs pricing-led. Architecture как leading angle, pricing — следствие (см. discussion 2026-05-14).

CUE snippet в hero, tier ladder, structural advantages framework — без изменений.

---

## 2. Цели и аудитория

### 2.1 Цели

Две цели одновременно, разрешаются через **порядок секций**, не через две версии страницы:

1. **Информационная (друзья и комьюнити).** Показать масштаб и продуманность архитектуры. Дать evaluator-у достаточно глубины чтобы оценить серьёзность продукта. Без агрессивного маркетингового тона.
2. **Минимально-маркетинговая (T1/T2 audience).** Привлечь self-host users, собрать waitlist на managed для тех, кто ещё не готов хостить сам. Не пугать сложностью реализации. Намекнуть что под рост есть готовый путь.

Honest framing: 5 waitlist signups до managed launch — уже успех. Strategic ambition: 50-200 signups к moment of managed shipping. Никакого fake-urgency, никаких «joined 487 others».

### 2.2 Аудитория

**Primary T1/T2 conversion targets** (KFM §2.3):
- **Маша** — tech lead малого SaaS, PaaS-refugee, готова к self-host если DX хороший.
- **Андрей-A** — соло-консультант / MSP с 3 малыми клиентами.

**Secondary readers** (не конвертятся прямо сейчас, но влияют на бренд через word-of-mouth):
- **Дима** — solo backend, side-projects на VPS.
- **Лиза** — vibe-coder PM, придёт когда managed launched.
- Друзья-инженеры — оценят глубину, пошарят технические посты.

**НЕ primary на этой странице** (Phase 5+):
- Group A/C/D — sales cycle и track record requirements не выполнены.
- Андрей-B (enterprise) — нужен compliance package, не лендинг.

### 2.3 Тон

**Serious, industrial, technical.** Не «hipster startup», не «enterprise corporate», не «AI-buzzword».

References для тона:
- Tailscale's marketing — technical, confident, no-nonsense.
- Linear's early site — clean, minimal, trusted dev tool.
- Cloudflare's product pages — specifications front and center.

Anti-references:
- Vercel marketing (too consumer-polished, gradient-heavy).
- Notion (too soft, too rounded).
- Анимированные градиенты, hand-drawn illustrations, любая emoji.
- Любой «AI for X» landing template.

---

## 3. Brand identity

### 3.1 Logo

Stylized stepped platform viewed at corner from ground level. Передаёт: foundation, layered scaling, structural support, technical precision.

Варианты:
1. **Two-tone primary** — dark slate с teal accent на верхнем tier'е (default для обеих тем).
2. **Horizontal solid** — single-color когда teal недоступен.
3. **Monochrome** — для single-color контекстов.
4. **Vertical lockup** — mark над wordmark для square containers.
5. **Favicon 16×16** — simplified silhouette.

Logo files — SVG.

### 3.2 Wordmark

Typeset как **AppRafter** без пробела. Two-tone variant тинтит «Rafter» в teal. Wordmark на **Roboto**.

### 3.3 Цветовая палитра

**Light theme:**
- Background: `#fafafa` (near-white, slight warmth)
- Surface: `#ffffff`
- Foreground: `#0f172a` (dark slate)
- Muted foreground: `#64748b`
- Accent: `#14b8a6` (teal)
- Border: `#e2e8f0`

**Dark theme:**
- Background: `#0a0e1a` (deep navy)
- Surface: `#111827` (slate-900)
- Foreground: `#f1f5f9`
- Muted foreground: `#94a3b8`
- Accent: `#14b8a6` (тот же teal — работает на обеих темах)
- Border: `#1e293b`

### 3.4 Типографика

- **Весь текст:** Roboto. Weights: Regular (400), Medium (500), Bold (700).
- Headings — Bold; body — Regular; emphasis — Medium.
- Code samples — `Roboto Mono` (или system-ui mono fallback).

Обоснование Roboto: industrial, utilitarian, нейтральный. Противоположность «yet another Inter/Geist site».

### 3.5 Визуальный язык

- **Sharp edges**, без скруглений на карточках (4px max только для кнопок/инпутов).
- **Generous whitespace**, плотные блоки только в технических секциях (код, таблицы).
- **Grid-based**, предсказуемый ритм.
- **Без gradients** для primary surfaces.
- **Flat elevations**, max — одна subtle shadow.
- **Geometric shapes** для декорации.
- **Без иллюстраций**, mascot'ов, hand-drawn anything.
- **Code snippets** — first-class visual элемент, как product screenshots.

### 3.6 Themes

- Light и dark обе must-be-полированы.
- Default — `prefers-color-scheme`.
- Fallback — dark (target audience defaults to dark).
- Manual toggle в header (sun/moon).

---

## 4. Структура страницы

Single-page лендинг. Восемь секций по убыванию маркетингового веса и возрастанию technical depth.

```
Header (sticky)
├── 4.1  Hero
├── 4.2  Value props (3 блока) — для T1/T2 founder
├── 4.3  Tier ladder visual — scale story
├── 4.4  Self-host vs Managed vs Turnkey — comparison table + pricing transparency
├── 4.5  Boring tech, opinionated glue — smoothing implementation framing
├── 4.6  Structural advantages — S-features per-claim
├── 4.7  Roadmap — phases, no dates
└── 4.8  Footer + bootstrap-without-VC strip
```

Идея: T1/T2 founder получит ответ на первых 3-4 секциях и пойдёт пробовать. Technical evaluator/друг прочитает дальше и оценит глубину. **Один путь, прогрессивное углубление.**

### 4.1 Header

- Logo (horizontal lockup, left-aligned).
- Right-aligned nav: `Docs` (placeholder с «Soon» если ещё не готовы), `Spec` (links to GitHub spec), `GitHub` (icon), theme toggle.
- **Без language switcher** — EN-only на launch, i18n-ready на уровне разметки, но контролов нет.
- Subtle border-bottom on scroll.

### 4.2 Hero

**Layout:** wordmark + headline + subhead + CTAs слева, CUE manifest snippet справа (или ниже на mobile).

**Headline (final):**

> **One manifest. From a €5 VPS to production. Open source.**

**Subhead (1-2 предложения):**

> AppRafter is an opinionated PaaS on Kubernetes. Describe your applications in a single CUE manifest — the same one runs from a single VDS to a multi-node production cluster. Open source (FSL-1.1-MIT). A managed version is coming for those who'd rather not run ops themselves.

**CTAs:**
- **Primary** (filled, teal): `Try self-host` → `/docs/quickstart` (или GitHub README quickstart anchor).
- **Secondary** (outlined): `Notify me on managed launch` → раскрывает inline waitlist форму (см. §7).
- **Tertiary** (text link с GitHub icon): `View on GitHub` → repo URL.

**Status badge** (под CTAs, subtle):

> `v0.[N] · MVP shipped on Tier 1 and Tier 2 · managed in development`

Без дат, без quarter'ов. Точный текст финализируется при публикации в зависимости от actual milestone state. Точный номер версии — в момент публикации.

**Hero visual — CUE manifest snippet.**

Согласован со спекой (`PLATFORM_SPEC.md` §3.1, после v0.1.25 schema refactor). Этот sample — **готовый текст для размещения, не подменять**:

```cue
apiVersion: apprafter.io/v1alpha1
kind: Application
metadata: name: "billing-api"

spec: {
    base: {
        image: "ghcr.io/me/billing:v1.4.2"

        expose: {
            port:     8080
            public:   true
            hostname: "api.example.com"
        }

        needs: {
            pg:        {size: small}
            jetstream: {streams: ["events"]}
        }

        env: DATABASE_URL: from: claim.pg.uri
    }

    environments: {
        dev:  base & {replicas: 1}
        prod: base & {replicas: 3}
    }
}
```

**Snippet UX:** Svelte island с copy-to-clipboard кнопкой в углу. Click → текст копируется, кнопка переключается в `Copied` на 1.5s, потом возвращается. Без notifications/toasts.

**Обоснование snippet'а:** 23 строки, демонстрирует kind/metadata/spec структуру, `needs.*` с claim-injection (`from: claim.pg.uri`), per-environment unification (`dev: base & {...}`). Достаточно чтобы оценить выразительность, но не overwhelming для первого знакомства.

### 4.3 Value props (3 блока)

Три блока × (заголовок + 2-3 предложения + опциональная иконка).

**Block 1 — Deploy with a single manifest.**

> Describe your application in CUE: `kind: Application`, declare dependencies through `needs.pg` / `needs.jetstream` / `needs.redis`. The platform handles the rest — Postgres clusters with backups, NATS streams with retention, Redis instances. No 400-line `values.yaml`. No drift between dev and prod.

**Block 2 — From one VDS to production, no scaling ceiling.**

> The same manifest runs on a €5 Hetzner VDS (single node, Tier 1) and scales horizontally to an HA cluster of any size and node mix (Tier 2). When you grow, you add nodes — you don't migrate to a different platform when you hit your provider's vertical ceiling. No rewriting applications between dev and prod.

**Block 3 — Open source, no vendor lock-in.**

> FSL-1.1-MIT, auto-converts to MIT in two years. Everything runs on your hardware or in your cloud. The managed version is on the way for those who'd rather not run ops themselves. And when you want back to self-host from it — there'll be a CLI command for that. Not a philosophy, an architecture.

**Обоснование набора:** три блока коррелируют с тремя реальными болями T1/T2 audience — config-сложность, страх scale-out (включая страх упереться в потолок), страх vendor-lock. Каждое утверждение поддержано конкретным фактом (CUE, T1+T2 shipped на Hetzner, FSL→MIT, managed→self-host tooling). Block 2 reframed на vertical-ceiling angle — это leading differentiator, pricing differential следует из architecture (см. discussion 2026-05-14, PRICING §7.1).

### 4.4 Tier ladder — visual scale story

**Визуал:** горизонтальная stairway-диаграмма или Sankey-style flow.

**Section heading:**

> **One Application manifest. The backing changes — not your app.**

**Tier cards** (4 ступени, plus orthogonal note):

| Tier | Description | Status |
|---|---|---|
| **Tier 1** | A single VDS with sane simplifications (no HA quorum, single-node database, no multi-tenancy). For side-projects and solo founders. Hetzner Cloud at launch. From €5/mo. | **Available now** |
| **Tier 2** | Production. 3 or more nodes of any size and any count — grows with your project. Hetzner Cloud at launch. | **Available now** |
| **Tier 3** | Bare metal. Dedicated EPYC servers. For when you need a performance ceiling above VPS. | Roadmap |
| **Tier 4** | Hyperscalers (AWS / GCP / Azure). Primarily for cases where regulation requires these specific providers. | Roadmap |

**Под таблицей, отдельной строкой:**

> **Confidential containers** are not a tier — they're an orthogonal capability available on any hardware that supports TDX or SEV-SNP.

**Обоснование:** killer feature #1 (KFM) — central structural moat. Tier-маркеры honest о том, что доступно сейчас vs roadmap, без дат. Heterogeneity подсвечена явно: T2 — это reality production-deployment-а любого размера, не «3 слабых узла на Хецнере». Hetzner-only на launch проговорен явно — мы не overcommit'им интеграции которые ещё не сделаны.

### 4.5 Self-host vs Managed vs Turnkey — comparison + transparency

**Section heading:**

> **Three ways to run AppRafter.**

**Comparison table** (трёхколоночная):

| | **Self-host** | **Managed (waitlist)** | **Turnkey (roadmap)** |
|---|---|---|---|
| **Price** | Free<sup>†</sup>, FSL-1.1-MIT | €10/mo per cluster (Hosted Services)<br>+ €10/mo per cluster (Operations add-on) | From €30/mo for solo (Tier 1)<br>Server cost + reseller markup + €20/mo per cluster |
| **Who runs the infrastructure** | You | You (your Hetzner account) | We do |
| **Who runs the UI / Backstage / MCP** | You (optional) | We do | We do |
| **Where your data lives** | With you | With you — your cluster, your control plane, your databases | With us, in our Hetzner account |
| **What we have visibility into** | Nothing | Only metadata — architectural guarantee from cluster ownership | Metadata by design (Minimal Data Exposure architecture), but the account is ours, so the guarantee is policy-level rather than structural |
| **Exit when you stop paying** | n/a | Your cluster keeps running. OSS takes over management. UI/Backstage you can self-host (we ship the tooling). You only lose the thin cloud-native premium layer — AI insights, cross-cluster aggregator, smart bill analysis | Cross-cluster MigrationPlan tooling (Phase 8+) for orchestrated migration |
| **Status** | **Available now** on Tier 1 and Tier 2 | **Waitlist** | **Roadmap** |

<sup>†</sup> *FSL-1.1-MIT allows any use — personal, internal business, commercial workloads — except offering AppRafter itself as a managed service to third parties. After two years, the license auto-converts to plain MIT and that restriction lifts.*

**Под таблицей — три коротких блока:**

#### 4.5.1 Pricing transparency

> **Everything you pay for is in this table.** No hidden tiers, no per-seat upsells, no "free tier that ends once you outgrow it". Prepaid model (like ChatGPT, Claude, Cursor — the AI-tool standard, no surprises). Annual billing optional, with a "save two months" discount. 14-day trial without a card, once per account.

**Обоснование изменения:** убрана упоминание Tier 4 floor (€2,500/mo для 3-node TDX baseline). Tier 4 — Phase 6+ feature; конкретный price anchor сейчас создавал expectation до того, как feature shipped. Принцип «no hidden tiers / no contact-sales» сохранён, конкретная цифра по T4 уходит во внутренний PRICING_AND_LAUNCH_NOTES и проявится на лендинге когда Phase 6 будет close.

#### 4.5.2 Anti-vendor-lock — architectural, not promised

> In Managed, your cluster physically lives on your infrastructure. We host the UI and operations layer on top. Cancel the subscription — the cluster keeps running, you lose the premium layer (which you can mostly self-host yourself; we ship the tooling for it). This is an **architectural fact**, not a service-level promise.
>
> For Turnkey, where we host the infrastructure too, we invest engineering cycles into your **exit path**, not just onboarding. Cross-cluster MigrationPlan is **planned as shipped tooling** (Phase 8+) — `apprafter migration plan` — for moving from Turnkey to self-host or another provider. Most cloud providers leave exit as an exercise for the customer. We're building it into a CLI command.

**Обоснование изменения:** "is shipped tooling" → "planned as shipped tooling (Phase 8+)" — direct conflict с §5 anti-overcommit. "AWS pretends migration is possible" заменено на neutral "Most cloud providers leave exit as an exercise for the customer" — без partisan vibe и без называния конкретного vendor в pejorative context.

#### 4.5.3 Our business model is aligned with your growth — structurally

> Per-cluster billing means our revenue grows when your deployments grow. We don't make more by locking you in or up-selling seats. We make more when you scale — more clusters, more workloads, more compute. When you stay small, we stay small with you. When you grow, we grow with you. This is an **incentive structure, not a marketing slogan** — it's encoded in how we charge.

**Обоснование:** прозрачная сетка с honest framing «что мы видим» (Managed = архитектурная гарантия, Turnkey = policy-level) лучше нечестного «we never look at your data» которое evaluator вскроет за минуту. Anti-vendor-lock + business-model alignment вместе формируют пакет «наши интересы структурно совпадают с вашими». Это и есть «showing the depth of architecture» without preaching.

### 4.6 Boring tech, opinionated glue

**Section heading:**

> **Boring tech, opinionated glue.**

**Opening paragraph:**

> We don't reinvent the Kubernetes control plane. Under the hood — well-known, proven components. The real work is opinionated composition and a thin layer of code where no ready solution exists.

**Component list** (с краткими пояснениями):

- **Talos Linux** — immutable OS, API-driven. Fewer snowflakes than a regular Linux node.
- **k3s / Cilium** — k8s core + eBPF networking. Cilium provides network policies, observability via Hubble, egress gateway.
- **NATS JetStream** — event/messaging backbone and control plane storage (via the community kine-nats adapter).
- **CloudNativePG** — Postgres operator with replication, backups, point-in-time recovery.
- **Dragonfly** — Redis-compatible, scales better on a single node.
- **ClickHouse** — logs, traces, application analytics.
- **OpenBao** — secrets management (HashiCorp Vault fork, BSL-free).
- **Backstage** — developer portal with TypeScript plugins.
- **Kamaji + Capsule** — hard multi-tenancy (Phase 5+).
- **cert-manager, external-dns, KEDA** — standard k8s add-ons.

**What we wrote ourselves:**

- **Rust operator on kube-rs** — reconciles `Application`, `ResourceClaim`, `ServiceProvider`, `MigrationPlan`, `Tenant`, `AccessGrant`.
- **CUE-based admission webhook** — type-safe validation with line-level errors before apply.
- **`apprafter` CLI** — bootstrap, manifest workflow, dev mode, migration tooling.
- **NATS-based audit log layer on top of kine** — turning the control plane's NATS backing store into a replayable platform event log (Tier 2+; opt-in upgrade on Tier 1).
- **MigrationPlan reconciler** — destructive-change gating with explicit approval.
- **ResourceClaim / ServiceProvider primitives** — typed contract for platform services.

**Closing paragraph:**

> This is a thin layer **on top of** proven components — only where no ready solution exists. Boring, and that's intentional. Boring tech is easier to debug, easier to hire for, easier to keep running.

**Обоснование изменения:** kine + NATS adapter — это **upstream community work** (kine-nats backend), AppRafter упаковывает и настраивает его, но не пишет. Поэтому в "Under the hood" честно указан как "via the community kine-nats adapter". Replayable audit log layer **поверх** этого adapter — это работа AppRafter (community adapter из коробки этого не предоставляет), и она перенесена в "What we wrote ourselves" с точной формулировкой. Это устраняет miscredit (overclaim про "we wrote kine+NATS adapter") при сохранении видимости реального differentiator-а.

**Обоснование секции в целом:** прямо адресует concern «один человек пишет всю платформу». Делает invisible work visible (правильная компоновка — это и есть ценность). Trust signal для technical evaluator-а: можно взять этот список и проверить каждый компонент отдельно.

### 4.7 Structural advantages

**Section heading:**

> **What the architecture gives you.**

Четыре блока. Каждый — claim + одно предложение **почему это следует из архитектуры**, не из maintenance promise.

#### Block 1 — Manifest portability across tiers

> **The same Application manifest is designed to run from local dev (k3d) through a single VDS, multi-node clusters, bare metal, and hyperscalers — without rewrites.** This isn't "we tested in a few environments" — it's a structural property: one CUE schema, one operator, one ResourceClaim contract. Only the backing changes.
>
> **Today it works on local dev, Tier 1 (single VDS), and Tier 2 (multi-node).** Tier 3 (bare metal) is in Phase 5+, Tier 4 (hyperscalers) is in Phase 6+.
>
> *KFM #1.*

**Обоснование изменения:** caveat про Tier 3/4 поднят из italic footnote в основной body — scan-reader мог потерять курсив, claim "runs in" в настоящем времени про не-shipped tiers создавал risk overclaim. Honest framing разделяет архитектурный design ("is designed to run") и текущее состояние ("today it works on").

#### Block 2 — Replayable audit log out of the box

> **The control plane runs on kine + NATS JetStream, which means every platform operation can be replayed from the event log.** Not a separate audit pipeline, not an add-on to etcd. Compliance-friendly **architecturally**, not by promise. A competitor would have to swap out their control plane storage layer to match this — a major architectural commitment.
>
> *KFM #21 — works today on Tier 2 and above. On Tier 1 the control plane uses kine with SQLite for simplicity; replayable log is available as an opt-in upgrade.*

**Обоснование изменения:** "touches everything above it" → "a major architectural commitment". kine API etcd-compatible, поэтому formal "everything above changes" — slight overclaim. Replacing control plane storage — серьёзная архитектурная стоимость без необходимости преувеличивать.

#### Block 3 — Managed → self-host: trivial exit

> **An architectural consequence of how we run Managed: only the UI and operations layer is hosted by us. Your cluster always stays with you.** AWS, Vercel, and Railway don't offer this pattern — workloads live with them, and replicating our model would conflict with their incentive structure: workloads-live-with-us is how they monetize.
>
> *KFM #2 — full CLI in Phase 4, principle in place from Phase 1.*

**Обоснование изменения:** "unwinding their core business model" → "would conflict with their incentive structure: workloads-live-with-us is how they monetize". Architecturally они **могли бы** offer "your cluster, our managed layer" — они не делают потому что conflicts с EKS / Vercel-platform monetization. Это **точнее** distinction (incentive misalignment, не architectural impossibility) и меньше partisan vibe.

#### Block 4 — Six platform services through a typed primitive

> **Not a Helm wrapper over Postgres.** A standardized CRD contract via ResourceClaim and ServiceProvider, which means a drop-in replacement (Postgres → AlloyDB, Redis → Valkey, S3 → Garage) is a container-level swap of the ServiceProvider — not an application rewrite. Competitors who expose services through Helm values would have to change their resource model from the ground up.
>
> *KFM #9 — works today: Postgres, JetStream, Redis (Phase 2); ClickHouse, S3, Notifications (Phase 3).*

**Обоснование:** «показать масштаб и продуманность» — но каждый claim обоснован архитектурно, не повторён как мантра. Структурная сложность повторения подсвечена явно (что нужно перепилить конкуренту), без overclaim «impossible». Это работает и для technical evaluator-а («here's why this is hard to copy») и для T1/T2 founder-а («OK, this won't be a 6-month rewrite-the-app project when I grow»).

### 4.8 Roadmap

**Section heading:**

> **Roadmap.**

Не «coming soon!!!», а спокойный timeline по KFM §2.2, без дат и quarter'ов.

#### Phase 4 — Managed offering launch

- Export-to-self-host CLI (fully functional)
- MCP-native managed (full integration)
- Minimal Data Exposure ADR + audit
- Hosted Services + Managed Operations tiers shipped

#### Phase 5+ — Production Tier 3 + multi-tenancy

- Tier 3 (Talos + LINSTOR on bare metal)
- Hard multi-tenancy via Kamaji + Capsule
- Turnkey Cloud launches (Tier 1-3)
- Live platform demo via self-hosted AccessGrant
- One-time migration toolkit (Product 1: cloud-foreign → AppRafter)

#### Phase 6+ — Confidential workloads

- Tier 4 confidential containers (Kata-CC) in opinionated wrapper
- Full T1 → T4 manifest portability complete

#### Phase 8+ — Cross-cluster federation

- Cross-cluster MigrationPlan (Product 2): sub-second cutover between clusters
- DR failover as an orchestrated operation
- Region migration within Turnkey — invisible to the customer

**Closing line:**

> Roadmap is driven by shipped features, not PR dates. Each phase is a finished product on its own, not "an MVP we'll polish later".

**Обоснование:** «уже всё будет когда понадобится» — это и есть roadmap. Honest delivery (фазы, не quarters) уважает technical audience и страхует от deadline-pressure (это hobby-проект соло-фаундера).

### 4.9 Footer + bootstrap-without-VC strip

**Опциональная strip над футером** (subtle, one line, separator borders):

> *AppRafter is a bootstrap project. No VC funding, no exit pressure — we grow with our customers, not at their expense.*

**Обоснование изменения:** "not against them" → "not at their expense". Less partisan, factually точнее. AWS / Vercel etc. — не "active enemies" of customers, у них просто misaligned incentives при scale. "At their expense" — это observable phenomenon (egress charges, vertical scaling cliffs), не moral judgment.

**Обоснование секции:** compliance differentiator для Group C (см. `MANAGED_STRATEGY` §11.6), и trust signal для T1/T2 friends («устойчивый проект, не Series-C-горящий»). «Легонько» — означает small text, muted foreground color, одна строка, не блок.

**Footer:**

- Logo (small).
- Columns:
  - **Project:** Spec (link to GitHub spec), GitHub (repo link), Roadmap (anchor to §4.7), Docs (placeholder).
  - **Legal:** License (FSL-1.1-MIT), Privacy Policy (placeholder until launch task), Terms (placeholder).
  - **Author:** link to creator's site / GitHub.
- Bottom row: copyright, license note (`FSL-1.1-MIT, auto-converts to MIT after 2 years`), founder line.

---

## 5. Запрещённые claims (анти-overcommit)

К моменту публикации лендинга **нельзя**:

- ❌ «Cancel subscription with zero downtime» — Phase 8 для full version.
- ❌ «Migrate between clouds in under 2 minutes» — Phase 8, только для AppRafter → AppRafter direction.
- ❌ «Confidential containers built-in» — Phase 6.
- ❌ «MCP-native PaaS» как primary claim — Phase 4 full version.
- ❌ «Multi-tenancy for MSP» — Phase 5+.
- ❌ «One-click AWS migration» — Phase 4-5, и **honest framing — это hours not seconds**.
- ❌ Любые «100% uptime», «99.99% SLA», testimonials, «trusted by 500+ companies».
- ❌ **Никаких конкретных чисел про reseller markup** на лендинге.
- ❌ **Никаких конкретных Tier 4 price anchors** до Phase 6+ closure (см. §4.5.1 rationale).

**Общий принцип формулировки (v2.2):** claims в **настоящем времени** ("X works", "is shipped") — только для Phase 1-3 features. Для Phase 4+ — **future tense** ("will be", "is planned"). Это правило в первую очередь касается structural advantages (§4.7) и anti-vendor-lock framing (§4.5.2) где easy скатиться в overcommit.

**Calibration claims (можно с уточнением):**

- ✅ «From €5 VPS to production» — works today (T1 + T2 shipped on Hetzner).
- ✅ «Replayable audit log on every cluster» — works on Tier 2+, opt-in upgrade on Tier 1. **Не «production-proven» до scale validation.**
- ✅ «Managed exit — your cluster keeps running» — только для Hosted Services / Operations tiers.
- ✅ «Turnkey exit through cross-cluster migration tooling» — обязательно с пометкой Phase 8+.

**Apple meta-frame** (см. `MANAGED_STRATEGY` §11.7):
- Claims «на нашем поле» (AppRafter → AppRafter) можно делать с конкретными числами **когда они появятся**.
- Claims «на чужом поле» (AWS → AppRafter, bare Linux → AppRafter) — только с honest framing limitations.

---

## 6. Anchor claims for current phase

К моменту публикации лендинга (после M3 минимум) можно стоять за:

1. **«One Application manifest from a €5 VPS to a 3+ node production cluster.»** (KFM #1, T1+T2 shipped on Hetzner)
2. **«Six platform services through a typed ResourceClaim. Postgres, NATS JetStream, Redis, ClickHouse, S3, Notifications.»** (KFM #9, Phase 2-3 shipped)
3. **«Replayable audit log on Tier 2+.»** (KFM #21, with Tier 1 opt-in caveat)
4. **«The same manifest in local dev and in prod. `apprafter dev cluster up` brings up local k3d with the same CRDs.»** (KFM #15, Phase 2B+ shipped — **verify shipped before publication**, иначе snять anchor)
5. **«Sane defaults on Tier 1 — single VDS, SealedSecrets in place of OpenBao, containerd, SMTP for notifications.»** (KFM #13)
6. **«Open source FSL-1.1-MIT, auto-converts to MIT in two years.»**

---

## 7. Waitlist scope

**Minimal viable form** — не воронка, а способ дать interested people знать. Раскрывается inline под secondary CTA в hero (не отдельная страница, не модалка).

### 7.1 Fields

- **Email** — required.
- **What's your use case?** (optional, single-line) — sanity check на quality of leads, не gate.
- **`[ ] I'd like a short call to discuss my use case.`** — checkbox, default unchecked. Если поставлен — после submit отправляется отдельный email с calendar link.

### 7.2 Copy (рядом с формой)

> The managed version of AppRafter is in development. Drop your email — we'll let you know when it's ready. One email, no newsletter, no marketing drip.

### 7.3 Behaviour

- Submit → success state: `We'll be in touch.` Без дополнительных promises.
- Если `wantsCall: true` — после signup user получает второй email с calendar booking link (Cal.com / Calendly / etc. — конкретный сервис настраивается через Payload Globals).
- Никаких subsequent drip-emails, никаких subscription preferences pages — этот канал используется один раз для launch announcement плюс опционально один раз для discovery booking.

### 7.4 Что НЕ делаем

- Нет «join 487 others».
- Нет countdown timer'а.
- Нет «get early access» с pressure-tactics.
- Нет автоматических reminder'ов.

### 7.5 Storage и privacy

- Payload `WaitlistSignups` collection — см. §11 для технических деталей.
- Минимальная privacy policy page — email используется для одного launch announcement (плюс опционально discovery booking link если opt-in), не передаётся третьим сторонам, можно отписаться в любой момент.
- Privacy policy текст — отдельный мини-таск перед публикацией.

---

## 8. Internationalization readiness (без локализации)

- **Без language switcher** в header на launch.
- **EN-only website copy** на старте — все strings жёстко в EN.
- **i18n-ready на уровне разметки** — Payload поля с локализованными значениями включены, но второй язык не заполняется.
- Если RU/другой язык появятся — это re-launch decision, не configuration change.
- LTR only — RTL не в scope.
- Без флагов где-либо — даже когда переводы появятся.

---

## 9. Editable via CMS

Для Claude Code integration с Payload — что должно быть editable из CMS, что hardcoded:

**Editable (Payload collections):**

- Hero — `LandingHero` global: headline, subhead, status badge text, CUE snippet text (richText с code language).
- Value props — `ValuePropsBlocks` array (title, body).
- Tier ladder cards — `TierLadderCards` array (tier number, title, description, status badge text).
- Comparison table — `OfferingTable` array (column rows with cells).
- Pricing/anti-lock/business-model transparency blocks — три отдельных rich-text fields в `LandingTransparency` global.
- Boring tech — два списка (`UnderTheHoodComponents`, `WhatWeWroteOurselves`), оба array с (name, description).
- Structural advantages — `StructuralAdvantagesBlocks` array (title, body, phase tag).
- Roadmap phases — `RoadmapPhases` array (phase number, title, items).
- Bootstrap-without-VC strip — single rich-text field.
- Footer columns — `FooterColumns` array.
- Waitlist copy — `WaitlistFormCopy` global (form copy, success state copy, calendar link для discovery opt-in).

**Hardcoded:**

- Layout, visuals, colors, typography.
- Tier ladder visual diagram (SVG component, не CMS-editable).
- Logo / wordmark SVG.

---

## 10. Anti-patterns to avoid

- «Get Started» секции с fake signup формами.
- Testimonials (нет users yet).
- «Trusted by» логотипы (нет users yet).
- Stats которые не существуют («10,000 deployments», «99.99% uptime»).
- Generic stock photography.
- 3D illustrations облаков, серверов, «cyber».
- Animated background gradients.
- Sticky chat bubble в углу.
- Newsletter capture popup.
- Cookie consent banner с marketing language (минимальный legal — OK).
- Emoji в любом месте сайта.
- **Конкретные имена конкурентов в pejorative context** (см. §4.5.2 v2.2 rationale, §4.7 Block 3) — можно говорить «AWS / Vercel / Railway» neutrally, нельзя «AWS pretends X», «Vercel rip-off», и подобное.

---

## 11. Implementation notes

**Technical stack остаётся как в `LANDING_IMPLEMENTATION_BRIEF.md` v1:** Astro 5 + Svelte 5 islands + Payload CMS 3 + Bun + PostgreSQL.

**Patches к v1 implementation brief:**

### 11.1 WaitlistSignups collection

`apps/cms/src/collections/waitlist.ts`:

```ts
{
  slug: 'waitlist',
  fields: [
    { name: 'email', type: 'email', required: true, unique: true },
    { name: 'useCase', type: 'text', required: false },
    { name: 'wantsCall', type: 'checkbox', defaultValue: false },
    { name: 'source', type: 'text' }, // populated from referrer
    { name: 'callEmailSentAt', type: 'date', admin: { readOnly: true } },
  ],
  hooks: {
    afterChange: [sendCallInvitationIfWanted],
  },
}
```

`afterChange` hook отправляет email с calendar link если `wantsCall === true` и `callEmailSentAt === null`, потом ставит `callEmailSentAt = now()` (защита от повторных отправок).

### 11.2 Calendar link через Payload Globals

`apps/cms/src/globals/booking.ts`:

```ts
{
  slug: 'booking',
  fields: [
    { name: 'discoveryCallUrl', type: 'text', required: true,
      admin: { description: 'Cal.com / Calendly / similar — sent to opt-in waitlist signups' } },
    { name: 'discoveryCallEmailTemplate', type: 'richText' },
  ],
}
```

### 11.3 Waitlist Svelte island

`apps/landing/src/components/waitlist/WaitlistForm.svelte`:

- Inline expansion при click на secondary CTA в hero.
- Email validation client-side (HTML5 + simple regex) + server-side в Payload.
- Optional `useCase` text input.
- `wantsCall` checkbox с label `I'd like a short call to discuss my use case`.
- POST to `/api/waitlist` endpoint (Payload's auto-generated REST endpoint).
- Success state: `We'll be in touch.` Без drip-email promises.

### 11.4 CUE snippet copy button

`apps/landing/src/components/hero/HeroCodeSample.svelte`:

- Svelte island (single button needs hydration, остальное — static syntax-highlighted code).
- Copy button в правом верхнем углу snippet'а.
- На click — `navigator.clipboard.writeText()` + переключение текста кнопки в `Copied` на 1.5s, потом обратно.
- Без notifications/toasts/global state.

### 11.5 Privacy policy placeholder

`apps/landing/src/pages/privacy.astro`:

- Stub файл с placeholder контентом для launch (либо «Privacy policy is in preparation»).
- Editable через Payload `LegalPages` collection (отдельная для privacy, terms — заполняем при готовности).

### 11.6 Anchored sections для roadmap-status linking

В comparison table (§4.5) ссылка на `#roadmap-phase-8` для Turnkey exit feature. Slug anchors стабильные, не меняются между релизами.

### 11.7 FSL footnote rendering

Footnote под pricing table (§4.5) рендерится через Payload field `pricingTableFootnote` (richText) в `LandingTransparency` global. Точный текст в брифе §4.5 — готовый для CMS.

---

## 12. Открытые вопросы

Все ключевые decisions финализированы в v2.2. Остаются три tail-таска перед публикацией:

1. **Финальный номер версии в status badge** (`v0.[N] · MVP shipped on Tier 1 and Tier 2 · managed in development`) — заполнится в момент публикации.
2. **Privacy policy текст** — отдельный мини-таск ближе к релизу. Может потребовать minimal legal review.
3. **Discovery call booking system** — конкретный сервис (Cal.com / Calendly / etc.) выбирается до публикации, URL прописывается в Payload `booking` global.
4. **Verify §6 Anchor claim #4** — `apprafter dev cluster up` зависит от Phase 2B+ shipped (dev-mode-task.md). Перед публикацией landing подтвердить что фича действительно shipped, иначе **удалить** этот anchor claim или переместить в roadmap (§4.8).

---

## 13. Изменения

- **2026-05-14 v2.2** — anti-overcommit и factual accuracy pass. Точечные правки:
  - **§4.3 Block 3** softened ("when you want managed — you'll get it" → "managed version is on the way... when you want back to self-host — there'll be a CLI command for that"). Block 2 reframed на vertical-ceiling angle (architecture-led, pricing — следствие).
  - **§4.5 pricing table** — footnote под таблицей с честной формулировкой FSL (разрешено всё кроме предоставления AppRafter как managed service сторонним).
  - **§4.5.1** — убрана конкретная цена Tier 4 floor (€2,500/mo); Phase 6+ feature, anchor создавал premature expectation.
  - **§4.5.2** — "is shipped tooling" → "planned as shipped tooling (Phase 8+)"; "AWS pretends migration is possible" → neutral "Most cloud providers leave exit as an exercise for the customer".
  - **§4.6** — kine + NATS adapter перенесён из "What we wrote ourselves" в "Under the hood" с пометкой "via the community kine-nats adapter" (это upstream work); в "What we wrote ourselves" — точная формулировка про **audit log layer on top of kine** как AppRafter работа.
  - **§4.7 Block 1** — caveat про Tier 3/4 поднят из italic в основной body, разделение design vs current state.
  - **§4.7 Block 2** — "touches everything above it" → "a major architectural commitment".
  - **§4.7 Block 3** — "unwinding their core business model" → "would conflict with their incentive structure: workloads-live-with-us is how they monetize". Точнее distinction (incentive vs architectural impossibility), меньше partisan.
  - **§4.9 strip** — "not against them" → "not at their expense" (less partisan, factually точнее).
  - **§5** добавлен общий принцип: present tense только для Phase 1-3 features.
  - **§10** добавлен anti-pattern: pejorative naming конкурентов.
  - **§12** добавлен open question про verification anchor claim #4 dev mode.
  - **§11.7** добавлена implementation note про FSL footnote rendering.
- **2026-05-14 v2.1** — финальная итерация. Все правки от сессии 2026-05-14: EN-only website copy, hero headline зафиксирован (`One manifest. From a €5 VPS to production. Open source.`), CUE snippet согласован с актуальной spec.md (post v0.1.25 schema refactor), Hetzner-only deployment на launch (без DigitalOcean / «any VPS provider»), honest framing «what we see» для Turnkey (policy-level vs structural guarantee), softened «Managed exit» framing (cluster keeps running, premium layer is mostly self-hostable, мы даём tooling), KFM #21 clarification для Tier 1 (kine+SQLite, opt-in upgrade для NATS event log), business-model alignment мини-блок добавлен в pricing transparency, no reseller markup percentage exposed publicly (only final prices and `from €30/mo solo` anchor), waitlist + discovery-call opt-in checkbox через Payload afterChange hook, bootstrap-without-VC strip kept as light footer note.
- **2026-05-14 v2** — переработка из v1: цели страницы, status story, tier ladder с гетерогенностью, три новые секции, CTA hierarchy, pricing transparency, anti-vendor-lock через active tooling.
