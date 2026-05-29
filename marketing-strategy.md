# AppRafter — Маркетинговая стратегия (managed offering)

> **Назначение:** консолидированный продуктово-маркетинговый документ — managed-предложение, аудитория, killer features, тиры, pricing, позиционирование, миграция, go-to-market, открытые вопросы.
> **Происхождение:** объединяет ранее разрозненные `MANAGED_STRATEGY.md`, `KILLER_FEATURES_MATRIX.md`, `PRELAUNCH_CHECKLIST.md`, `PRICING_AND_LAUNCH_NOTES.md` (теперь удалены — их содержимое здесь).
> **Статус:** внутренний рабочий документ (не публичный, не в `docs/` — допустимы персоны, цены, messaging). Архитектура зацементирована в ADR 0034–0038 + 0031; здесь — продуктово-маркетинговый слой.
> **Согласовано с:** `speedrun-plan.md` (Hosted Services = launch managed plan; hardware-tier T1+T2 в OSS-core на launch). `MCP_CHECKLIST.md` остаётся отдельно — операционный security-чеклист для MCP.
> **Дата сборки:** 2026-05-29.

---

## Содержание

1. Обзор и тезис
2. Аудитория и персоны
3. Killer features
4. Managed-модель и тиры
5. Pricing
6. Позиционирование и messaging
7. Миграция и anti-vendor-lock
8. Go-to-market и launch
9. Открытые вопросы и риски

---

## Обзор и тезис

### Главный тезис: managed — это product 2 поверх open-source core

AppRafter — это сначала полнофункциональная open-source платформа, и только затем — managed offering. Managed **не продолжение** OSS-roadmap и **не его premium-форк**: это отдельный product 2, построенный поверх того же OSS-core как над substrate. OSS-кластер клиента полностью автономен; managed добавляет тонкий слой premium QoL и операционных сервисов, которые мы делаем за клиента.

Из этого insight'а вытекает всё остальное: OSS закрывается ровно до точки, где становится substrate для managed, а managed-track строится как обычный multi-tenant SaaS поверх стабильной платформы. Архитектура решения зацементирована в ADR — управленческая часть тезиса не «живёт в этом документе», а в **ADR 0034** (managed offering model и терминология), на которую опираются остальные секции.

### Структурный анти-вендор-лок by design

Ключевой архитектурный выбор — **Option B**: master nodes, kine, NATS, AppRafter operator и **все данные клиента** остаются в его кластере на его инфраструктуре. Мы хостим только управленческую «крышу» — Backstage, Account UI, cross-cluster aggregator, MCP server, white-glove services (зацементировано в **ADR 0034** и **ADR 0037** — managed control-plane infra).

Прямое следствие, которое AWS EKS / GCP GKE / Vercel / Railway предложить архитектурно **не могут**:

> **Cancel = revoke registration token. OSS-кластер продолжает работать.**

Подключение customer-кластера к нашим hosted services идёт через `apprafter-agent`, который держит outbound-соединение к нашему hub (протокол агента — **ADR 0031**). Отписка = отзыв registration token → agent получает disconnect → кластер продолжает обслуживать трафик как обычный OSS-install, без миграции и без downtime. Клиент теряет только premium QoL слой (hosted UI, MCP endpoint, Account UI), но не сам кластер, приложения или данные. Это не теоретический «Export to self-host», а **structural offboarding с дня launch** — кластер уже автономен.

Усиливает анти-лок ещё два структурных факта, детали — в соответствующих секциях:
- **Minimal Data Exposure** (зацементировано в **ADR 0035**): managed-сервисы видят только metadata, никогда не customer data. Мы не sub-processor для compute и не для data plane.
- **Agentic safety через MigrationPlan CRD** (**ADR 0036**): тот же human-in-the-loop gate, что защищает людей от destructive-операций, работает для AI-агентов без модификаций.

### Open Core split

Модель — Open Core по аналогии с GitLab CE/Premium, Sentry self-host/SaaS, Grafana OSS/Cloud. Принцип классификации: **в OSS попадает всё, что нужно платформе чтобы работать; в managed-only остаётся то, что улучшает работу, но не блокирует её, плюс то, что технически невозможно в single-tenant контексте.**

- **OSS-core (одинаков для self-host и managed):** operator + все CRDs + ServiceProviders, Application / DevProfile / AccessGrant / MigrationPlan, Backstage с core-плагинами, CLI, базовая observability, ExternalSurface, platform services, базовый MCP server. Сам `apprafter-agent` тоже OSS (FSL-1.1) — без него кластер fully functional.
- **Split (basic в OSS, enhanced в managed):** cost monitoring, backup orchestration, abuse handling, AccessGrant lifecycle.
- **Managed-only (premium QoL):** cross-cluster aggregator, AI-powered insights, smart bill optimization, Hosted MCP cloud endpoint, Hetzner relationship management.

Community-replicated managed-фичи — **bounded risk**, не cannibalization: целевая аудитория managed и DIY-сообщество — разные сегменты. Это нормальная Open Core dynamics, бороться не нужно.

### Три managed plan'а — c Hosted Services как launch-плана

Terminology per **ADR 0034**. Важно различать **hardware tier** (T1–T4 — характеристика железа/изоляции) и **managed plan** (что именно мы делаем за клиента). T1 и T2 оба входят в OSS-core на launch; managed-plan — отдельная ось.

| Managed plan | Что хостим / делаем | Hetzner relationship | Когда |
|---|---|---|---|
| **Hosted Services** (€10/mo per cluster) | Backstage + плагины, Account UI, MCP endpoint, SSO, 90-day audit, template DPA. Customer-кластер автономен | Customer's direct (cash flow exposure: zero) | **LAUNCH plan** |
| **Managed Operations** (+€10/mo per cluster) | + abuse parsing, cost monitoring, backup orchestration, API token health checks | Customer's account + наш operational access | Post-launch |
| **Turnkey Cloud** (Hetzner × 1.25 + €20/mo, от €30/mo) | Всё, включая Hetzner relationship — один invoice | Наш | Post-launch (track record + юр.exposure) |
| **Enterprise** | TBD | TBD | Когда придёт первый Андрей-Б |

**Hosted Services — это плана launch'а, а не «managed = Phase 4»** (предыдущая формулировка устарела до speedrun). На launch self-host остаётся €0 и играет роль free tier — managed free tier'а нет; для evaluation есть 14-day trial без credit card, once per account.

Детали managed-модели, distribution responsibilities и тиров — в секции «Managed-модель и тиры»; персоны и сегменты — в секции «Аудитория». Эта секция — только высокоуровневый тезис.

---

## Аудитория и персоны

Этот раздел — единственный owner определений персон и сегментов. Все остальные разделы ссылаются на них по имени (Дима, Маша, Андрей-A, Андрей-B, Лиза; Group A/B/C/D). Терминология выровнена по текущему состоянию: «hardware tier» (T1–T4 — substrate, ADR 0022) и «managed plan» (Hosted Services → Managed Operations → Turnkey Cloud, ADR 0034) — это **две ортогональные оси**, не одно и то же. Любой customer на любом hardware tier выбирает любой managed plan, который этот tier поддерживает. Архитектура managed-предложения зацементирована в ADR 0034 (модель и терминология), 0035 (Minimal Data Exposure), 0036 (MCP & agentic safety), 0037 (managed control-plane infra), 0038 (Tier-2 Kamaji opt-in), 0031 (apprafter-agent) — детали там, здесь только аудиторная карта.

Важно для калибровки: персоны изначально жили в двух источниках с разной оптикой. Dev-персоны описывают **individual user journey** на self-host/managed. Группы A/B/C/D описывают **company-level сегменты** для heavier managed plans на более поздних phase. Ниже они сведены в одну карту: dev-персоны — это «кто сидит за клавиатурой», группы — «какая компания платит». Они частично пересекаются (например, Маша как individual входит в Group A как company), и это нормально — оптики дополняют друг друга, а не конкурируют.

### Калибровка модели пользователя (применима ко всем персонам)

- **AI-augmented baseline.** Современный target — это `пользователь + AI-copilot + onboarding-опыт от 5–10 SaaS`. У них уже есть аккаунты везде, SSH-ключ, привязанная карта, AI как постоянный спутник для любого незнакомого термина. Поправка **x0.5** к первичным оценкам friction. Реальные блокеры — структурные gap'ы (нет нужного managed plan, нет migration tool), не когнитивные (CUE, +1 конфиг, новый CLI). При оценке любого friction спрашивать: «это структурный gap или когнитивная преграда, которую AI снимает?».
- **Primary loop Lisa-класса = AI-agent деплоит за неё.** Поэтому MCP-native managed закрывает её петлю целиком (ADR 0036), и именно она — target №1 на launch, несмотря на то что формально это «техническая» аудитория.

### Dev-персоны (individual user journey)

#### Дима — SSH-deployer
- **Профиль:** backend-разработчик ~5 лет, NestJS, 1–3 side-project'а на CX22. Деплоит `ssh + git pull + pm2`.
- **Боли:** ручной деплой, нет rollback, нет DNS/TLS из коробки, непонятен backup destination, страх «упало на середине bootstrap — что делать».
- **Value-prop:** автоматизация без выгорания от k8s; opinionated defaults снимают решения (backup, TLS).
- **Когда конвертируется:** на self-host сразу при первом успешном bootstrap (idempotent resume + DNS/TLS hints критичны). В managed заходит «+ tech upgrade» к Hosted Services, когда хочет portal/MCP поверх своего кластера.

#### Маша — PaaS-refugee
- **Профиль:** tech lead в малом SaaS-стартапе (~3 разработчика). Сейчас Railway + Vercel + Supabase, bill ~$300/мес и растёт. Знает Docker, читала про k8s.
- **Боли:** растущий bill, hyperscaler-style непредсказуемость, нет миграционного пути с PaaS, env-vars UX (OpenBao выглядит как downgrade против «одного окна» Railway), нужен preview-per-PR.
- **Value-prop:** self-host/managed с DX уровня Railway, прозрачный prepaid pricing, T2 для роста команды.
- **Когда конвертируется:** target Hosted Services с launch (signup на T1 или T2 напрямую). Углубляется в Managed Operations, когда хочет уйти из Hetzner UI; в team-фичи (AccessGrant, SSO, preview env) при росте до 3+.

#### Андрей-A — Ansible-DIY консультант
- **Профиль:** senior DevOps, ~8 лет, 3 малых клиента через свои ansible-playbook'и + ~15 helm charts на каждого, устал мейнтенить ~45 деплоев.
- **Боли:** хочет стандартизации; mTLS/SPIFFE хочет уже на T1; концепция отдельного оператора и kine+NATS at scale — непривычны.
- **Value-prop:** один config × один config вместо N×M; Open Core (можно self-host без managed вообще).
- **Когда конвертируется:** **это DIY-сегмент, который сам не пойдёт в managed** — он целевая аудитория OSS-core и потенциальный MSP-кейс (один control panel на нескольких клиентов; Kamaji opt-in, ADR 0038, активируется по его сигналу). Важная asymmetry: Андрей-A и managed-аудитория (Лиза, занятая Маша) — **разные сегменты, которые не пересекаются по причине выбора**, поэтому community-replicated managed-фичи — естественное расслоение, не каннибализация revenue.

#### Андрей-B — Enterprise platform engineer
- **Профиль:** платформенный инженер в средней/крупной компании с compliance-требованиями. DR runbook, multi-tenancy, audit/identity.
- **Боли:** нужен coherent end-to-end audit/identity, confidential workloads, доказуемая sovereignty.
- **Value-prop:** узкая killer-комбинация (confidential containers Kata-CC + OpenBao + obs-стек + Headscale) + vertical-integration audit-аргумент. **Не конкурируем с Deckhouse в массовом enterprise** — целимся в 5–10 правильных клиентов.
- **Когда конвертируется:** поздняя аудитория. На launch нерелевантен; заходит через Turnkey Cloud / T4 на Phase 5–6. Тяжёлых DevOps в принципе сложно перетащить — придут только когда наберётся популярность на малых командах.

#### Лиза — Vibe-coder PM
- **Профиль:** PM в early-stage стартапе, навайбкодила MVP через Cursor/v0/Lovable. Stack Next.js + Supabase, платит $50–100/мес SaaS, bill пугает. AI-copilot — постоянный спутник.
- **Боли:** терминал-фобия, не пойдёт в spec.md; нужен soft-onboarding отдельно от технической документации.
- **Value-prop:** MCP-native managed (нулевой когнитивный overhead — AI агент деплоит за неё), prepaid (нормализовано AI-tool'ами), «Don't know Kubernetes? You shouldn't have to».
- **Канал захода:** через AI-tool которым пишет код, либо через знакомых-вайбкодеров (ссылка в активной conversation, где AI уже консультируется).
- **Когда конвертируется:** **primary target на launch** через Hosted Services. Self-host для неё критичен только опосредованно (llms.txt + beginner's track). По мере роста ladder up в Managed Operations и далее Turnkey Cloud (когда не хочет вообще регистрироваться в Hetzner).

### Company-level сегменты (heavier managed plans, post-launch)

Это аудитория для Managed Operations и Turnkey Cloud на более поздних phase. Описание — профиль / боли / value-prop / sales-cycle / LTV / phase.

#### Group A — Hands-on growing tech company
- **Профиль:** 20–150 человек, tech-heavy (50%+ engineers), сейчас на Hetzner Cloud / DO / Linode, CTO лично знает архитектуру.
- **Боли:** performance ceilings VPS, noisy neighbors для DB, растущий cost per GB, появляющиеся compliance-требования. Сами в bare metal не пошли — provisioning + networking + hardware failures = новая ops-нагрузка.
- **Value-prop:** T3 (Talos + LINSTOR) без operational шока, тот же Application manifest что в dev, DR/BC built-in.
- **Sales cycle:** 1–3 месяца, решает CTO. **LTV:** $500–2k/mo MRR. **Phase:** 5+ (после готового T3).

#### Group B — Mid-market wants managed (без hyperscaler lock-in)
- **Профиль:** 100–500 человек, business-focused, растущий AWS bill, нет ресурсов на свою platform team.
- **Value-prop:** Turnkey Cloud без hyperscaler-цен, прозрачный pricing, Open Core safety net (Export to Self-host).
- **Caveat:** нужна **track record** — case studies, годы uptime, references. В Phase 4–5 не пойдут.
- **Sales cycle:** 3–6 месяцев. **LTV:** $2–10k/mo MRR. **Phase:** 7+ (после track record от Group A).

#### Group C — EU data sovereignty / Schrems II refugees
- **Профиль:** EU-based (DE/FR/NL/AT/CH), часто работают с personal data EU residents.
- **Боли:** post-Schrems II legal risk, customer demand «EU-only», подозрительные для регуляторов DPA-цепочки с US-провайдерами.
- **Value-prop:** AppRafter на Hetzner DE / OVH FR / Scaleway = native EU-only, полная data sovereignty, audit log полностью наш, T4 confidential containers для sensitive data, Open Core safety net. Minimal Data Exposure (ADR 0035) делает нас compliance-friendly by architecture.
- **Sales cycle:** 4–6 месяцев (legal team involved). **LTV:** $2–8k/mo MRR. **Phase:** 5+ — окно открыто сейчас (2026), через 2–4 года сегмент уходит к established players.
- **Notable:** **архитектурный driver** — дизайн под Group C поднимает планку для всех (см. ordering ниже).

#### Group D — Cost-driven cloud refugees
- **Профиль:** mid-size с AWS bill $10k–50k/mo, 30–200 человек, tech know-how есть, bare metal сами не разворачивают.
- **Боли:** AWS bill растёт быстрее revenue, optimization выжата, egress charges особенно болезненны.
- **Value-prop:** Hetzner 5–10x дешевле AWS для compute, egress практически free, one-time migration toolkit (Product 1).
- **Sales cycle:** 1–4 месяца (financial pressure ускоряет). **LTV:** $1–5k/mo MRR. **Phase:** 5–6 (после migration toolkit).

### Стратегический порядок захода: Group A → Group C

**Group A — easiest path to first managed customers**, и **~70% требований Group C = подмножество требований Group A** (T3, DR/BC docs, audit-полнота, Backstage observability, cost monitoring, `needs.*.external`). Только ~30% требований Group C специфичны (ISO 27001 / SOC 2, compliance posture page, DPA template, one-time migration tools).

Путь:
1. **Phase 4–5: build for Group A** — T3, DR docs, Backstage maturity, audit log, cost monitoring.
2. **Phase 5 параллельно: Group C-specific** — стартовать SOC 2 / ISO 27001, draft DPA, design migration toolkit.
3. **Phase 5–6: первые Group A customers** становятся references для Group C.
4. **Phase 6: первые Group C deals** при наличии references + certifications + migration toolkit.

Идти через A → C — **в ~2x быстрее**, чем независимо строить под Group C. Group C как architectural driver поднимает крышу для всех: Minimal Data Exposure становится default-архитектурой (ADR 0035), cross-cluster migration — core capability, audit log — полным, Open Core безопасность подкреплена Export feature. Каждый из этих фич улучшает и Group A / D опыт.

### Launch-аудитория (current state — что реально на старте)

Реконсиляция со speedrun (ADR 0034, speedrun-plan §0.3, §0.5): на launch **Hosted Services — единственный managed plan**, и hardware tiers **T1 и T2 оба в OSS-core**. Поэтому launch-аудитория — это AI-augmented individuals, а не company-сегменты:

- **Primary:** Лиза (vibe-coder, через MCP-native loop) и Маша (PaaS-refugee, signup на T1 или T2 напрямую).
- **Secondary:** Дима (SSH-deployer «+ tech upgrade» к Hosted Services поверх своего кластера).
- **Не на launch:** Андрей-B (Phase 5–6, Turnkey/T4), company-группы A/B/C/D (heavier managed plans позже). Андрей-A — целевая аудитория OSS-core, в managed по природе сегмента не идёт.

Group C-friendly позиционирование при этом доступно **с дня launch структурно**: Hosted Services не трогает customer data plane (Minimal Data Exposure by architecture, ADR 0035), мы не sub-processor для compute и не оператор customer-инфраструктуры (ADR 0034) — то есть compliance-история готова раньше, чем сам сегмент конвертируется.

---

## Killer features

Этот раздел воспроизводит killer-features matrix и реконсилит колонку «available from» с актуальным launch scope из `speedrun-plan.md`. Личные имена персон (Дима, Маша, Андрей-A, Андрей-B, Лиза, Group A–D) определены в разделе «Аудитория» — здесь они только упоминаются. Архитектура managed-offering закреплена в ADR 0034–0038 (+ 0031 для `apprafter-agent`); этот раздел ссылается на них там, где «available from» сдвинулся относительно pre-speedrun матрицы.

### Легенда

**Type — тип differentiator'а:**

- **S — Structural moat:** конкуренты архитектурно не могут реплицировать без переписывания stack'а. Самые ценные, устойчивы к copy by funded competitor.
- **D — Strong differentiator:** конкуренты *могли бы* реплицировать, но не сделали (yet). Менее defensible long-term, но real today.
- **E — Enabler:** не killer alone, но необходимый foundation для других killer features.

**Weight** («если убрать фичу, AppRafter всё ещё AppRafter?»): Critical / High / Med / Low.
**Influence** (impact на purchase decision): Critical / High / Med / Low.
**Killer alone?** (закрывает ли deal одна фича): **Y** — sufficient by itself для primary persona; **N** — supports другие, alone недостаточно.

### Матрица killer features

> **Колонка «Available from» дана в реконсилированном виде:** где speedrun меняет дату относительно pre-speedrun матрицы — указано **«launch (HS)»** для Hosted Services launch с пометкой источника (ADR / speedrun-bucket). Где фича остаётся на исходной фазе — указана фаза. Hosted Services — это launch managed plan (НЕ Phase 4), T1 и T2 оба в OSS-core на launch.

| # | Feature | Type | Weight | Influence | Available from (реконсилировано) | Primary value | Secondary value | Killer alone? |
|---|---------|------|--------|-----------|----------------------------------|---------------|-----------------|---------------|
| 1 | Same-manifest вертикальное масштабирование по hardware-tier'ам | **S** | Critical | High (от T3+) | **T1+T2 на launch (HS)**; T1→T2 migration ~Q3 2026 (post-launch first bundle); T3/T4 на roadmap | Маша, Group A | Андрей-A, Лиза, Group C/D | **Y** (с T3+ полностью) |
| 2 | Open Core + Export to Self-host | **S** | Critical | Critical (compliance-aware) | **structural на launch (HS)** — customer cluster autonomous; cancel = revoke token, OSS-кластер живёт; даже Export CLI не нужен | Group C, Group A | Маша, Андрей-B, Group B | **Y** |
| 3 | MCP-native + agentic safety через MigrationPlan CRD | **S** | Critical | High (rising) | **real CRD-based gate на launch (HS)** — ADR 0036; approvals via CLI + Argo CD кнопки; Backstage plugin post-launch | Лиза, Group A forward-looking | Маша, Андрей-B | **Y** для AI-tools сегмента |
| 4 | Cross-cluster MigrationPlan (Product 2, sub-second cutover) | **S** | Critical | High | Phase 8+ (далеко post-launch) | Group B, Group D | Turnkey customers, Group A | **Y** (когда ships) |
| 5 | Minimal Data Exposure architecture | **S** | High | Critical (compliance) | **structural на launch (HS)** — ADR 0035; by architecture видим только metadata, не data plane | Group C, Андрей-B | Group A с GDPR concerns | **Y** для HIPAA/GDPR scenarios |
| 6 | One-time migration toolkit (Product 1) | **D** | Med | High | partial на launch (Supabase + Railway light helpers); full Product 1 post-launch | Group D, Group C | Маша (Railway/Render exit) | **Y** для AWS-cost-pressured |
| 7 | MigrationPlan primitive (destructive change gate) | **D** | High | Med | **full CRD + reconciler на launch (HS)**, embedded в apprafter-operator | Group A, Андрей-A | Андрей-B, всё что прод | N (enabler для #3, #4) |
| 8 | T4 confidential containers в opinionated PaaS | **S** | High | Critical (narrow) | Phase 6 (post-launch) | Group C, regulated niche | T4 enterprise | **Y** для confidential niche |
| 9 | Шесть platform services через ResourceClaim/ServiceProvider | **D** | High | High (day-1 DX) | **2 из 6 на launch (HS)** — `needs.pg` (CloudNativePG) + `needs.redis` (Dragonfly); остальные on-demand post-launch | Маша, Дима, Андрей-A | Group A, Лиза | **Y** для «Render-class DX self-hostable» |
| 10 | Cluster-admin constrain — 8-layer defense bundle | **D** | Med-High | Critical (regulated) | post-launch (но HS-архитектура даёт layer #3 structurally — мы external, не оперируем customer cluster-admin) | Group C, Андрей-B | Group A security-conscious | N alone (technical buyer recognition) |
| 11 | Hard multi-tenancy через Kamaji + Capsule | **D** | Med | Critical (MSP) | post-launch — **opt-in feature** через `PlatformStack.spec.values.multitenancy: true`, НЕ default на T2 (ADR 0038) | Андрей-A (MSP) | Group B managed | **Y** для MSP-specific |
| 12 | Vertical integration audit / identity propagation | **S** | Med | High (security-conscious) | post-launch (depends on SPIRE) | Group C, T4 | Андрей-B | N alone (supports compliance story) |
| 13 | T1 simpler defaults (SealedSecrets, containerd, SMTP) | **E** | Med | High (solo adoption) | **на launch (HS)** | Дима, Лиза | Маша first-time | **Y** для «30-min bootstrap» promise |
| 14 | Typed config + composition (CUE + admission webhook) | **D** | Med | Med-High (capability framing) | **на launch (HS)** | Андрей-A (Helm-tired), Group A engineers | Маша, Дима | N alone (substantive value bypasses CUE bikeshedding) |
| 15 | Dev mode — same Application manifest dev/prod | **D** | Med | Med-High | partial на launch (identical CUE manifest works); dev-mode CLI post-launch | Маша, Андрей-A | Дима, Group A | N alone (powerful enabler) |
| 16 | Sub-processors как bounded list (4-7 items) | **D** | Med | Med | **на launch (HS)** — minimal (Stripe + email + analytics), самая лёгкая disclosure из 3 plans (ADR 0034) | Group C | Андрей-B, compliance teams | N alone (supports compliance story) |
| 17 | No-VC bootstrap alignment / structural compliance | **D** | Low-Med | Med (risk-aware) | always | Group C lawyers/DPOs | Group A risk-cautious | N alone |
| 18 | Cost-anchored transparent pricing (no markup magic) | **D** | Med | Med (trust signal) | **на launch (HS)** — €10/mo per cluster published | Маша, Дима, Group D | Андрей-A, Group A | N alone |
| 19 | HTTPRoute auto-gen от `Application.expose` | **D** | Low-Med | Med (DX win) | **на launch (HS)** — pulled up (speedrun bucket B, 4.1a) | All deploying | — | N |
| 20 | Plugin Migration Interface (community migration plugins) | **D** | Low | Med (community-driven) | Phase 5+ (post-launch) | Андрей-A, community | Group D (long-tail migrations) | N (ecosystem play) |
| 21 | kine + NATS JetStream как control-plane storage (replayable audit log) | **S** | Med | Med (buyer) / High (evaluator) | **etcd на launch (HS)**; kine+NATS post-launch когда audit replayability нужна как differentiator | Group C compliance, Group A engineers | Андрей-B, Андрей-A | N alone (foundation для #5 #12; risk: scale not yet validated) |
| 22 | Out-of-band rescue cluster (dogfooding mitigation) | **E** | Low | Med (trust signal) | partial на launch (ADR 0037 — dogfooded host); зависит от host infra decision | All managed customers indirectly | — | N (operational hygiene) |
| 23 | Live platform demo через self-hosted AccessGrant (ephemeral playground) | **S** | Med-High | High (evaluators) | post-launch (требует Kamaji + AccessGrant + Karpenter — all post-launch backlog) | Маша, Group A, technical evaluators | Андрей-A, Андрей-B, Лиза | **Y** для «try before commit» flow |

### Что сдвинулось относительно pre-speedrun матрицы

Pre-speedrun матрица привязывала managed-defining moats (#2, #3, #5) к Phase 4. Speedrun-архитектура (Hosted Services как launch plan, закреплено в ADR 0034) делает три из них **structural с дня launch'а** — это сильнее, чем Phase 4 Export CLI:

- **#2 Open Core + Export** — структурно закрыт на launch, потому что customer cluster полностью autonomous на их Hetzner. «Cancel» = revoke registration token → hosted services отключаются → OSS-кластер продолжает работать без миграции. Export CLI из Phase 4 не нужен вовсе.
- **#5 Minimal Data Exposure** — структурно закрыт на launch (ADR 0035). By architecture мы видим только metadata (manifest applies, status events, opt-in log streams, audit events), но не application data plane: ни данных в PG/Redis, ни secret values.
- **#3 MCP-safety** — на launch это **real CRD-based gate** (ADR 0036), не soft API gate. MigrationPlan primitive (#7) embedded в apprafter-operator, destructive ops проходят через MigrationController в customer's cluster, admission webhook enforces классификацию — AI agent через MCP bypass'нуть не может. Approvals на launch через CLI + Argo CD approve/reject кнопки; Backstage MigrationPlan plugin — post-launch first bundle.

Прочие сдвиги: #1 теперь **T1+T2 ready на launch** (не «Phase 1 demo» / «Phase 5 full») — оба hardware-tier'а в OSS-core, T1→T2 migration в post-launch first bundle ~Q3 2026; #9 — 2/6 platform services на launch (`needs.pg` + `needs.redis`); #11 hard multi-tenancy — **opt-in, не default на T2** (ADR 0038); #21 kine+NATS — etcd на launch, миграция post-launch. Архитектурные обоснования всех этих решений живут в ADR 0031 и 0034–0038.

> **Различай «hardware tier» и «managed plan».** Hardware tier (T1 single-node → T4 confidential) — это substrate, на котором крутится customer cluster. Managed plan (Hosted Services → Managed Operations → Turnkey Cloud, + Enterprise TBD) — это сколько эксплуатации мы берём на себя поверх OSS-core. На launch: managed plan = Hosted Services; hardware tiers = T1 и T2. Терминология — ADR 0034.

### Pattern observations

**Распределение по типам.** 9 Structural moats (#1, 2, 3, 4, 5, 8, 12, 21, 23) — это defensible long-term portfolio; если хотя бы 5-6 реально ship, конкуренты на той же территории (Cozystack, Coolify, Kuberns) не догоняют без переписывания stack'а. Большинство Strong differentiators (#6, 7, 9, 10, 11, 14, 15-20) реплицируемы при достаточной инвестиции, но today никто их вместе не делает. Enablers (#13, 22) deal'ы не закрывают, но без них Structural/Strong не работают.

**Про #21 (upgrade D→S).** kine+NATS изначально классифицировался как D с low weight. Пересмотр: NATS JetStream как event log даёт **replayable audit history** для control-plane operations — capability, которой etcd structurally не имеет (требует отдельный audit pipeline). Это architectural foundation для #5 (MDE) и #12 (vertical integration audit). Risk остаётся в execution (scale validation), не в design. На launch — etcd (HA через embedded etcd в k3s 3-node T2 substrate); kine+NATS включается post-launch, когда audit replayability становится marketing-critical.

**Persona × density.** Маша и Андрей-A — primary launch targets (несколько killer features alone-sufficient уже на launch — #9, #2, #3 для Маши). Group A — лучший stretch target post-launch (включая #23 live demo). Group C — самая «invested» persona (#2, #5, #16 уже на launch), но sales cycle 4-6 месяцев + legal review = они **не early customers** даже когда features ready. Group B/D — sparse coverage до Product 2 (#4, Phase 8). Дима и Лиза получают по сути 1 killer feature каждый — это OSS/conversion ramp и (для Лизы) MCP-loop, не paying-heavy segments. После launch Маша/Андрей-A expansion идёт через **organic tier-upgrade** (T1→T2→T3) и Turnkey migration, не через feature-upsell — structurally cleaner expansion model, но требует, чтобы эти персоны продолжали расти для MRR growth.

### Marketing-claim rules per plan (что можно / нельзя говорить)

Launch claims следуют strict ruleset: говорим только то, что работает на текущем managed plan и текущем hardware-tier scope.

**На Hosted Services launch МОЖНО:**

- «Sign up for T1 single-node или T2 3-node HA — both available today».
- «Same manifest от €5 VPS до production HA — works today» (T1+T2 demonstrated). Полная T1→T4 vertical claim («to confidential bare metal») — additional bullet для compliance-aware, не above-the-fold.
- «Cancel anytime — your cluster keeps working as OSS. Zero migration required» (#2, structural).
- «We see metadata, not your data. Minimal Data Exposure by architecture» (#5, ADR 0035).
- «MCP-enabled with CRD-based agentic safety gate. Approvals via CLI / Argo CD, Backstage UI plugin coming» (#3, ADR 0036).
- «Same CUE manifest на local dev и production» (#15 partial — identical manifest works).
- «Postgres + Redis на самохостинге через одну строку `needs`» (#9, 2/6).
- «Configuration that won't deploy if it won't run» — capability framing для #14, **не** «we use CUE» (избегаем bikeshedding по поводу DSL).
- «T1→T2 migration coming Q3 2026» — честный roadmap-bullet (post-launch first bundle, ~2-4 недели после launch).

**На Hosted Services launch НЕЛЬЗЯ говорить yet:**

- ❌ «Replayable audit log» как production capability — kine+NATS post-launch (#21). До scale validation допустимо максимум «design enables replayable audit log», не «production-proven».
- ❌ «Tier 3 / Tier 4», confidential containers — Phase 5/6 (#8).
- ❌ «Hard multi-tenancy» как default — это opt-in feature, не default на T2 (#11, ADR 0038).
- ❌ «Cancel subscription без downtime» в Product 2 смысле (cross-cluster cutover) — Phase 8 (#4). На launch «cancel keeps cluster running» работает structurally, но это offboarding, не live cutover.
- ❌ «Full agentic safety с LLM-reviewer» — на launch heuristic + CRD gate; LLM-reviewer post-launch.
- ❌ «One-time migration from AWS» (full Product 1) — на launch только light Supabase/Railway helpers (#6 partial).

**Общее правило для landing copy:** above-the-fold = только features с Influence ≥ High и available ≤ текущий scope (launch / Hosted Services); below-the-fold roadmap — всё остальное с честным «coming Q3» / «on roadmap». Для discovery calls: не продавать features, которые не ship'ятся в ближайшие ~2 шага — credibility hit.

---

## Managed-модель и тиры

Managed offering AppRafter — это **управляемая надстройка над open-source платформой**, а не отдельный продукт и не замена self-host. Self-host остаётся полностью функциональным; managed добавляет premium QoL и операционные сервисы, которые мы делаем за клиента. Здесь — продуктовое framing модели; архитектура зацементирована в ADR (см. ниже), и спорные инварианты живут там, не в этом документе.

Важно держать в голове два независимых измерения, которые легко спутать (терминология по **ADR 0034**):

- **Hardware tier** (T1–T4) — это *что за инфраструктура* под кластером: T1 single-node VDS, T2 3-node HA, T3 bare metal (Talos + LINSTOR), T4 confidential bare metal. T1 и T2 — оба в OSS-core с launch: customer выбирает tier прямо на signup, обе работают. T3/T4 — post-launch roadmap.
- **Managed plan** (Hosted Services / Managed Operations / Turnkey Cloud, + Enterprise TBD) — это *сколько операционной ответственности мы берём на себя* поверх любого hardware tier.

Это ортогональные оси: T1-кластер можно вести на Hosted Services, T2-кластер тоже на Hosted Services, и тот же T2 позже поднять до Managed Operations без смены железа.

### Option B — hosted UX/ops layer над автономным customer-кластером

Архитектурное решение (зацементировано в **ADR 0034**): **master nodes, kine/etcd, NATS, AppRafter operator и все данные клиента остаются в customer-кластере на customer-инфраструктуре.** Мы хостим только управленческую «крышу» — Backstage portal с нашими плагинами, Account UI (multi-cluster, billing, team), cross-cluster aggregator, hosted MCP endpoint, white-glove services.

Почему именно так:

- **Trivial exit.** Customer отписался → теряет hosted UI/aggregation/white-glove → **кластер продолжает работать как обычный OSS install.** AWS EKS / GCP GKE такого предложить не могут архитектурно. Это структурный differentiator, не маркетинговое обещание.
- **Архитектурная чистота.** Control plane и workers — в одной приватной сети, без cross-internet API traffic.
- **T1-совместимость.** В single-node режиме control plane и workers — одна нода; split control plane на нашей стороне был бы невозможен. Option B работает на любом tier одинаково.
- **Лучшая SaaS unit economics.** Наша инфра растёт с базой клиентов медленнее, чем per-cluster.

Подключение — **outbound, без inbound listener** на customer side. В кластере живёт `apprafter-agent`, который устанавливает исходящее соединение к нашему hosted bus; customer issue'ит registration token через CLI и paste'ит его в Account UI. Никакой firewall-конфигурации, никаких сохранённых у нас kubeconfig'ов или Hetzner credentials. Протокол агента и его trust-модель зафиксированы в **ADR 0031**.

**Metadata-only — hard constraint, не feature.** Hosted services видят *только* metadata: applies манифестов, status events, audit events, по opt-in — log streams. Мы **не видим** customer data в PG/Redis, не видим secret values, не касаемся application data plane вообще. При дизайне любой managed-фичи первый вопрос — «что именно идёт от customer cluster к нам?»; если ответ «customer data» — redesign до «metadata only». Это и есть основной compliance-аргумент (мы не sub-processor для compute, для Group C — fast-pass на review), полностью описан в **ADR 0035 (Minimal Data Exposure)**; в маркетинговой секции он раскрыт как killer feature, здесь — только архитектурная рамка.

### Responsibility split по слоям

| Слой | Self-host (OSS) | Hosted Services | Managed Operations | Turnkey Cloud |
|---|---|---|---|---|
| OS / node provisioning | client | client | client | **нам** |
| k8s control plane | client | **client** | **client** | **client** (на нашем железе) |
| AppRafter operator + CRDs | client | **client** | **client** | **client** |
| Workloads + customer data | client | **client** | **client** | **client** |
| Storage / backups (bytes) | client | client | orchestration нам, bytes client-side | orchestration нам, bytes client-side |
| Backstage portal | client (опц.) | **нам** | **нам** | **нам** |
| Account UI (multi-cluster, billing, team) | n/a | **нам** | **нам** | **нам** |
| Hosted MCP endpoint | client (свой) | **нам** | **нам** | **нам** |
| Abuse handling | client | client | **нам** (auto-suspend) | **нам** |
| Cost monitoring + bill alerts | client (basic) | client | **нам** | **нам** |
| Hetzner billing / relationship | client | **client (direct)** | client платит, мы оперируем UI | **нам (один invoice)** |
| Cash flow exposure для нас | — | **нет** | низкая | существенная |

Ключевой инвариант через все планы: control plane, operator, CRDs и customer data **всегда** остаются client-side. Меняется только ширина hosted-слоя и того, чем мы оперируем за клиента.

### Три managed plan и триггеры

**Hosted Services — это LAUNCH-план** (не Phase 4, не «когда-нибудь»). Самый лёгкий уровень: мы хостим Backstage portal, Account UI и MCP endpoint; всё остальное — на customer-кластере. Hetzner billing и abuse — на стороне клиента, наша cash flow exposure нулевая, юр.поверхность минимальна (свой ToS + простой metadata-only DPA). Target-аудитория на launch — AI-augmented Lisa-class пользователи, ранние Дима/Маша. Открывается с T1 и T2 одновременно.

**Managed Operations — post-launch.** Всё из Hosted Services плюс автоматизация операционных задач: abuse email parsing → match на клиента → notify + auto-suspend в пределах Hetzner grace period, cost monitoring с bill-anomaly alerts, backup orchestration с retention policies, API token health checks, snapshot management через наш UI. UI здесь **opinionated, не реплика Hetzner-консоли** — показываем только то, что нужно платформе для автономной работы. **Триггер активации:** customer ask «уберите меня из Hetzner UI» становится consistent. Юр.поверхность та же, что у Hosted Services; ценовая дельта — небольшая надбавка за реальную operational нагрузку.

**Turnkey Cloud — post-launch, дальше всего.** Hetzner relationship целиком на нашей стороне, один invoice (infra + Hosted Services + Managed Operations), prepaid model. **Триггер:** customer ask «не хочу вообще регистрироваться в Hetzner» **плюс** наличие track record под существенную юр./финансовую exposure (юр.лицо обязательно, VAT compliance, DPA chain, abuse-process). Это Phase 5+ territory.

**Почему именно staged.** Operational опыт нарабатывается *до того*, как берётся юридическая и финансовая ответственность за чужие инстансы. К Turnkey уже отработаны billing, abuse-flow и support — на более лёгких планах за меньшие риски.

### Hetzner reseller risk — забота только Turnkey Cloud

Этот риск **не применим** к Hosted Services и Managed Operations: там Hetzner-аккаунт остаётся у клиента, и abuse-цепочка Hetzner идёт к нему напрямую. Релевантен он только когда compute — на *нашем* аккаунте, т.е. на Turnkey Cloud. Сводка:

- Hetzner работает по принципу **notify-first, suspend-after** (6–48h на ответ в зависимости от типа нарушения), блокируют offending IP, а не аккаунт целиком — при отсутствии ответа.
- **Pattern-of-complaints cancellation** реальна: несколько resolved abuse-incidents подряд → могут cancel весь аккаунт. Mitigation — жёсткий ToS + терминация клиентов с 2+ нарушениями.
- **Fake abuse reports** проходят level-1 support; downtime во время доказательства подделки уже идёт. Mitigation — template-ответы с requirement of evidence, эскалация в L2.
- **Segmentation** — несколько Hetzner-проектов (риск общий) либо несколько юр.лиц (риск изолирован, но дороже). На Phase 6+ exit — собственный IP block + ASN убирает Hetzner из abuse chain полностью.

Auto-handling abuse полностью автоматизируется (parser abuse-email → matcher IP→cluster → notify + auto-suspend за N часов, где N меньше Hetzner grace period → statement Hetzner'у закрывает тикет) и в соло-формате справляется. Но до Turnkey строить это не нужно.

### Где живёт архитектура

Это продуктовое framing. Спецификация и инварианты зацементированы в ADR — на них и ссылаться при реализации:

- **ADR 0034** — managed offering model и терминология (hosted-management layer, hardware tier vs managed plan).
- **ADR 0035** — Minimal Data Exposure (metadata-only как hard constraint).
- **ADR 0031** — `apprafter-agent` ↔ hosted-bus протокол (gRPC streaming, Rust-агент).
- **ADR 0037** — managed control-plane infrastructure (dogfood «AppRafter on AppRafter» на собственном T2 substrate, rescue-cluster recovery).
- **ADR 0038** — Tier 2 = HA substrate; hard multi-tenancy via Kamaji opt-in, не default.
- **ADR 0036** — MCP server и agentic-safety модель (структурное enforcement на границе платформы).

---

## Pricing

> **Статус:** draft baseline. Сетка зафиксирована «по ощущениям» до измерения реального unit economics — её необходимо re-validate на actual cost per cluster после первых 10–50 paying customers (метрики измерения — ниже, в подразделе «Что меряем»). Reconciled со speedrun-моделью (`speedrun-plan.md`).

Pricing — это не отдельный документ-в-вакууме, а прямое следствие двух решений, зацементированных в архитектуре: managed-offering модель и терминология трёх managed plans (**Hosted Services → Managed Operations → Turnkey Cloud**, + Enterprise TBD) живут в **ADR 0034**; принцип «мы видим только metadata, не data plane» (Minimal Data Exposure) — в **ADR 0035**; managed control-plane инфраструктура — в **ADR 0037**. Сетка цен ниже — это денежная проекция этой модели, а не самостоятельная конструкция.

Важное разграничение, которое pricing обязан держать чисто: **«hardware tier» (T1–T4) ≠ «managed plan»**. Tier — это про железо и его confidential-свойства. Plan — это про то, что из операционной поверхности хостим мы. На launch в OSS-core входят **оба hardware tier T1 и T2** (T2 — HA-substrate, Kamaji opt-in per **ADR 0038**), а из managed plans запускается ровно **один — Hosted Services**. То есть customer на launch может выбрать T1 single-node или T2 3-node HA, и поверх любого из них взять Hosted Services. Managed Operations и Turnkey Cloud — post-launch ladder-up по реальным customer-сигналам, но их экономику фиксируем заранее, чтобы цены T1-сегмента не упирались в потолок без запаса.

### Принципы

- **Per-cluster как единственный primary cost driver.** Реальные трудозатраты на платформу масштабируются от количества кластеров, не от приложений и не от пользователей. Pricing отражает это напрямую — bill считается по кластерам.
- **No per-user fee, no per-workspace base fee.** Нагрузка на платформу — функция железа кластера, а не software-policy. Импорт per-seat модели из retail SaaS архитектурно не оправдан. Топология безразлична к нашему cost: 1 workspace × N clusters и N workspaces × 1 cluster дают одинаковый bill.
- **No free tier на managed.** Роль free-опции играет OSS self-host (€0). Hosted free tier — money sink без conversion path: Дима либо остаётся на OSS, либо дорастает до Маши через value, а не через freemium-воронку. Evaluation-кейс закрывает 14-day trial без credit card, once per account.
- **Prepaid, без metered usage.** Никакого usage-based billing на launch — фиксированная подписка per cluster. Это сознательно упрощает и Stripe-интеграцию, и customer-facing предсказуемость bill.
- **Transparent public pricing, EUR primary, round numbers.** AppRafter EU-based, primary валюта €. «Contact sales» — antipattern для нашей аудитории; единственные исключения — Tier 4 (показываем floor + «contact us» для нестандартных размеров) и Enterprise (TBD до прихода первого Андрея-Б). Charm-pricing вида $9/$19 в developer-tools контексте выглядит manipulative; round numbers — confidence-сигнал (траектория Railway/Vercel/Tailscale).
- **Один paid plan на launch, без feature-ladder.** Большинство «Business»-фич (SSO, multi-workspace, DPA, audit retention) имеют marginal cost ≈ 0 для платформы. Tier-laddering — это сигнал-driven решение, а не launch-фича.

### Сетка (launch baseline)

| Линия | Цена | Что включено |
|---|---|---|
| **OSS self-host** | €0 | Полная платформа (Open Core), но без hosted UI/MCP |
| **Hosted Services** *(launch plan)* | €10/mo per cluster | Hosted Backstage + MCP endpoint + Account UI + SSO + 90-day audit retention + template DPA |
| **Managed Operations** *(post-launch, add-on к HS)* | +€10/mo per cluster (includes 1 Hetzner account) | Abuse parsing, cost monitoring, backup orchestration, API token health checks |
| **Turnkey Cloud T1–T3** *(post-launch)* | Hetzner × 1.25 + €20/mo per cluster (HS + Ops bundled) | от ~€26/mo solo (T1) |
| **Turnkey Cloud T4** *(post-launch)* | from €2,500/mo (3-node TDX baseline) | Confidential setup + T4 Operations + dedicated support; «contact us» для нестандартных размеров |
| **Enterprise** | «contact us» (TBD) | Custom SLA / DPA / dedicated support — открывается с первым Андреем-Б |
| **Annual** | Monthly × 10 («save 2 months», ~17%) | На все HS / Managed Operations линии |
| **Trial** | 14 дней без cc, once per account | На любой Hosted Services-tier |

Разграничение plan vs hardware tier здесь видно буквально: строка **Hosted Services €10** не зависит от того, T1 или T2 у customer под капотом — €10 это plan-fee, а стоимость железа customer платит Hetzner напрямую (на launch-плане Hosted Services compute не проходит через нас — cash flow exposure нулевой, что и делает этот plan самым лёгким юридически: мы не sub-processor для compute, см. ADR 0034/0035). В Turnkey Cloud, наоборот, hardware tier попадает в нашу формулу (Hetzner × 1.25), потому что там железо — наше.

Примеры расчёта по персонам (полные сценарии и unit-economics — в смежных разделах; здесь только иллюстрация формулы):

- **Маша** (1 cluster T2, growing): HS €10 + (post-launch) Operations €10 + own Hetzner T2 ~€24 = **€44/mo**.
- **Андрей-A** (MSP, 4 clusters в 4 Hetzner-аккаунтах): HS €10 × 4 (+ Operations €10 × 4 post-launch) = **€40–80/mo** managed; топология не меняет bill.
- **Лиза** на Turnkey T1 (post-launch): Hetzner CX22 €4.49 × 1.25 + €20 = **~€26/mo**.

### Обоснование цен

**Industry floor — категория «managed UI для self-managed серверов».** Прямые конкуренты по бизнес-модели: **Coolify Cloud** ($5/mo за 2 servers + $3/server) и **Dokploy Cloud** ($4.50/mo за 1 server + $3.50/server). Это floor рынка. Наши €10/mo per cluster — **1.5–2x premium**, и он объективно оправдан категориальной разницей: Coolify/Dokploy — Docker-обёртка над VPS, тогда как Hosted Services даёт MCP-native Kubernetes-платформу с CRD-примитивами (ServiceProvider/ResourceClaim/MigrationPlan), Backstage с кастомными плагинами и CRD-based agentic safety gate (per ADR 0036). 1.5–2x — sweet spot: достаточно далеко от Coolify, чтобы не выглядеть «то же самое», и достаточно низко, чтобы не отпугнуть solo-сегмент.

**Markup-логика для Turnkey (post-launch).** 25% на Hetzner pass-through — внутри industry reseller baseline (15–30%) и оставляет запас на повышение. Для T1-solo это даёт высокий ratio (~5.7x против €4.49 VPS), но абсолютная цена низкая, и для не-технической Лизы это «не хочу регистрироваться в Hetzner» — оправданная convenience-надбавка. T4 идёт по другой логике — value pricing на функцию, которой нет у конкурентов: compute-markup 40–50% + dedicated T4 Operations fee, floor from €2,500/mo. **Annual = «save 2 months» (~17%)** — industry-standard, не воспринимается как desperate pre-pay grab; agressive-discount (>20%) сознательно избегаем.

**Что цена сознательно НЕ делает:** per-user / per-workspace fee (cost driver не масштабируется от них), free tier на managed (OSS), contact-sales blackbox для T1–T3 (только T4 floor), «higher API rate limits» как paid-фича (marketing-driven, не cost-justified), и явный Hetzner-account add-on в публичной сетке (при правиле «1 account included per cluster» доплата практически не триггерится).

### Per-segment competitive positioning

- **Дима** (solo, side-project). Альтернативы: Hetzner CX22 + Docker self-managed / Coolify self-host (€4.49), Railway Hobby / Fly.io ($5–20). Вердикт: Диму таргетим **через OSS, не через managed** — Hosted Services за €14.49 (€4.49 VPS + €10) проигрывает self-managed и не должен пытаться выиграть. Это conversion-ramp сегмент, не revenue.
- **Маша** (1 cluster, growing, ~5 apps) — **сильнейший value-for-money сегмент**. Альтернативы: Render ~€155/mo, Railway Pro ~€73, Fly.io ~€55–90, Coolify Cloud + own Hetzner ~€29 (но функционально слабее). AppRafter €44/mo: **~3.5x дешевле Render**, comparable с Railway/Fly при заметно большей функциональности.
- **Андрей-A** (mid-size, 4 clusters, T3-железо) — **самый рациональный value-prop**, он уже считает деньги. Альтернатива EKS: 4×$73 control + 4×$500 EC2 ≈ €2,300/mo. AppRafter (own Hetzner + HS) — порядок €320/mo managed-side, то есть полноценная PaaS поверх k8s за долю стоимости отдельной platform-team. (Это post-launch сегмент — Managed Operations ladder.)
- **Андрей-Б / Group D** (AWS refugees, compliance) — Phase 7+ / post-launch сегмент, экономику готовим заранее. AWS EKS prod ≈ $5–15k/mo против Turnkey T3 prod ~€1,280/mo (4 clusters) = **3–10x дешевле**. Caveat: без track record цена не убедит — это conversion-target поздних фаз.
- **Лиза** (Turnkey, solo/small) — post-launch (Turnkey — Phase 5+). Heroku Basic $25–50, Render Pro $50 против Turnkey T1 ~€26 — **не dramatically дешевле**, хук другой: scaling trajectory без re-platforming. «Same manifest from €5 VDS to confidential bare metal» — там, где Heroku/Render требуют rebuild на масштабе, у нас тот же manifest на T1→T2→T3.

### Что меряем (re-validation на real unit economics)

Сетка остаётся **draft baseline** до первых 3–6 месяцев managed-customers. Ключевые метрики, после которых пересматриваем цены: actual **COGS per cluster** (infra hosted-control-plane / active clusters + ops-attention в часах/неделю); **trial-to-paid conversion** (если <10% — trial слишком короткий или scope не закрывает evaluation); **annual vs monthly mix**; demand на Hetzner-account add-on; price-sensitivity Managed Operations (если actual cost <€5/cluster — держим €10 и кладём в development; если >€7 — review-точка для bump до €15). До этих данных любые числа здесь — обоснованная гипотеза, не финал.

---

## Позиционирование и messaging

Этот раздел консолидирует стратегическое позиционирование и готовый messaging. Терминология выровнена по текущему состоянию: managed-уровни — **Hosted Services → Managed Operations → Turnkey Cloud** (+ Enterprise TBD), per ADR 0034; «hardware tier» (T1–T4) и «managed plan» — это разные оси, не путать. Архитектура каждого аргумента ниже цементирована в ADR — детали реализации там, здесь только позиционирование:

- **ADR 0034** — managed offering model и terminology (три плана + Enterprise).
- **ADR 0035** — Minimal Data Exposure (база для compliance- и sub-processor-аргументов).
- **ADR 0036** — MCP & agentic safety (база для MCP-native differentiator'а).
- **ADR 0037** — managed control-plane infrastructure.
- **ADR 0038** — Tier-2 Kamaji opt-in (hard multi-tenancy как post-launch opt-in, не launch-default).
- **ADR 0031** — apprafter-agent protocol.

Важная поправка к pre-speedrun источникам: **Hosted Services — это LAUNCH managed plan**, а не Phase-4-фича. Hardware tiers **T1 и T2 оба входят в OSS core на launch** (T1 prod-ready, T2 beta). Соответственно часть differentiator'ов, которые старые документы маркировали «Phase 4», на самом деле закрываются **структурно с дня launch'а** — потому что Hosted Services по архитектуре external к customer cluster (мы видим только metadata, customer cluster автономен). Конкретно: killer feature #2 (Open Core + Export) и #5 (Minimal Data Exposure) — structural at launch; #3 (MCP-native + agentic safety) — через real CRD-gate at launch.

### Дисциплина claims — «play on own field» vs «play on others' field»

Это базовая рамка, которая управляет силой любого claim (Apple-мета-рамка, см. также калибровочные заметки):

- **На нашем поле** (AppRafter ↔ AppRafter): vertical scaling T1→T2 по тому же манифесту, cross-cluster DR, region migration внутри Turnkey, multi-cluster federation, cross-cluster MigrationPlan (Product 2). Здесь архитектура даёт **structural advantage**, и claims могут быть **сильными и измеримыми с конкретными числами** («AppRafter→AppRafter migration: 1m 47s»).
- **На чужом поле** (foreign-cloud → AppRafter): AWS/GCP/Azure → AppRafter, bare Linux → AppRafter, existing k8s → AppRafter. Здесь максимум — **подстелить соломку**: tooling, docs, refactoring templates, pre-flight checks. Honest framing: «predictable maintenance window, engineering prep required, we minimize pain not eliminate it» («AWS→AppRafter: planned window 2–8h depending on scale»).

Правило при формулировке любого claim: спросить — «это на нашем поле или на чужом?». Anti-pattern — смешивать поля: «AppRafter migration is fast» слишком vague, читатель применит к своему AWS-departure use case и разочаруется. Это дисциплина против oversell.

Поверх этого — **strict launch-claims ruleset**: на лендинг выносим только то, что работает **сегодня**. Above-the-fold = только features с Influence ≥ High **и** Available ≤ current phase; below-fold roadmap = всё остальное с честным «Phase X / coming Q…». Что **нельзя** заявлять на launch: «replayable audit log» (kine+NATS — post-launch, etcd на launch), «hard multi-tenancy» (Kamaji opt-in post-launch, ADR 0038), Tier 3/4, full «cancel без downtime» для Turnkey (Product 2 = Phase 8; на launch «cancel без downtime» работает для Hosted Services **структурно**, через revoke registration token → OSS-кластер продолжает работать).

### Differentiator'ы — по сегментам

**MCP-native managed (ADR 0036).** Большинство managed PaaS (Vercel, Railway, Fly, Render) не имеют MCP. У нас Cursor/Claude/Copilot нативно управляют деплоями — структурное преимущество, особенно для vibe-coder сегмента (нулевой когнитивный overhead). На launch это real CRD-based gate (не soft API gate), approvals через CLI; Backstage plugin — post-launch first bundle.

**Vertical integration как audit/identity аргумент.** Обычно vertical integration защищают через UX-coherence, но **coherent end-to-end audit/identity** — отдельный технический аргумент для security-conscious аудитории. Каждое звено цепочки (CLI → API → operator → workload) — наш код, identity propagation вшивается без compromise'ов. Out-of-tree платформы так не могут: control plane один, ingress другой, runtime третий, у каждого свой лог. (Полная реализация зависит от SPIRE — post-launch; на launch это roadmap-claim, не launch-claim.)

**Sub-processors как bounded list (compliance differentiator).** В отличие от типичного SaaS с 30+ sub-processors, наш список реально короткий: hosting (Hetzner DE / OVH FR / Scaleway — всё EU), payment (Stripe / Paddle), transactional email (Postmark EU / Mailjet EU), error tracking (можно self-hosted Sentry на своём же стеке). Optional с customer opt-in — AI providers (OpenAI / Anthropic), и к ним уходят только metrics и structural metadata, **не customer data** (Minimal Data Exposure, ADR 0035); alternative для compliance-sensitive — self-hosted LLMs на EU GPU. Architectural reason: opinionated стек **уменьшает** dependency surface (нет CDN-as-a-service, нет external analytics, нет third-party monitoring). Compliance implication — sub-processor questionnaire короткая страница, не книга; 30-day notification process простой; customer compliance review — fast pass. Это **active differentiator** против AWS-managed-services, где список sub-processors недоступно длинный и непрозрачный.

**No-VC bootstrap — compliance + alignment story.** Bootstrapping from revenue без VC: sustainable от первого клиента, roadmap не hockey-stick-driven, alignment на customer retention (не на VC exit), no «pivot risk», Open Core устойчив (нет инвесторов, требующих закрытия исходников). Это **structural fact**, а не puffery: lawyer на legal review спрашивает «что если вы пропадёте» — ответ «we don't depend on VC pivot decisions, our survival is correlated with customer base, not investment cycles» серьёзнее стандартного SaaS-ответа. В комбинации с Open Core + Export to Self-host — triple safety net для customer continuity. Communicate: compliance posture page, about-us / founder story, vendor evaluation questionnaire, sales-разговоры с EU data-sovereignty сегментом.

**Enterprise / Tier-4 — узкая killer-комбинация.** Не играть с Deckhouse в их сегменте. Целиться в специфическую комбинацию, которую нигде не собрали воедино: Confidential containers (Kata-CC, практически нигде нет в PaaS-обёртке), OpenBao + obs-стек + Headscale + ExternalSurface как **когерентная история**, cost-arbitrage против EKS (особенно egress), cross-cluster MigrationPlan как уникальный primitive. 5–10 правильных enterprise-клиентов окупают продукт; массовый enterprise — не пытаться. (Это далеко post-launch — Tier 4 не заявляем на launch.)

**Open Core dynamics — natural segmentation, не cannibalization.** Целевая аудитория managed и DIY-сообщество — разные сегменты, не пересекаются по причинам выбора. Community-replicated features существуют параллельно managed offering'у, не конкурируют напрямую. Следствие: open source как ценность сам по себе **не подрывает** managed revenue — можно открывать больше кода без страха каннибализации.

### Per-persona hooks

Готовые фразы для лендинга / pitch decks / outreach. Использовать по правилу matrix §5: на discovery-call'е поднимать только killer features, доступные ≤ текущей фазы для этой персоны.

- **Дима** (SSH-deployer, OSS/conversion ramp): «Start free on AppRafter OSS — single Hetzner VPS, full platform. Outgrow it? Move to managed near-zero-downtime: same manifest, same APIs, orchestrated via cross-cluster MigrationPlan. AppRafter → AppRafter, not a re-platforming.» (наше поле; concrete claim появляется с Product 2.)
- **Маша** (PaaS-refugee): «Render-level DX, 3–4x cheaper, no vendor lock-in, MCP-native.»
- **Андрей-A** (Ansible-DIY консультант / MSP): «Save €2k/mo per cluster vs EKS, get production platform UX without hiring SRE team.»
- **Андрей-B / EU-cost-refugees** (enterprise platform engineer, compliance): «Transparent bill instead of AWS runaway. EU sovereignty built-in. Confidential containers on request.»
- **Лиза** (vibe-coder PM): «Same manifest from €5 VDS to confidential bare metal. No re-platforming at any growth stage.»

### Готовые messaging snippets (curated, de-duped)

Единый curated-список — overlapping snippets из источников схлопнуты. Применять по дисциплине claims выше; рядом — что honest заявлять **на launch** vs что роадмап.

Hero / lendinг (honest сегодня):
- **«Opinionated platform for the solo founder who'll outgrow it.»** — основной hero.
- **«Don't know Kubernetes? You shouldn't have to.»** — sub-headline / badge для non-DevOps аудитории.
- Hardware tiers на лендинге формулировать **с примерностью** («starting from ~€5/mo, choose your specific machines and counts»), не как закрытый список из 4 вариантов.

Launch claims (работают **сегодня**, honest):
- «Sign up for T1 single-node or T2 3-node HA — both available today.»
- «Cancel anytime — your cluster keeps working as OSS. Zero migration required.» (structural через Hosted Services; для Turnkey full-версия — Phase 8.)
- «We see metadata, not your data. Minimal Data Exposure by architecture.» (ADR 0035.)
- «MCP-enabled with CRD-based agentic safety gate. Approvals via CLI, Backstage UI plugin coming Q3.» (ADR 0036.)
- «T1→T2 migration coming Q3 2026.» (явный roadmap-claim, post-launch first bundle.)

Managed-positioning (часть — roadmap, помечено):
- **«MCP-native PaaS with built-in agentic safety.»** — для технической аудитории.
- **«AppRafter has built-in agentic safety because we built guardrails for humans first.»** — managed-launch messaging.
- **«Единственный managed PaaS, где cancel-subscription button не приводит к downtime. Ваш кластер остаётся вашим — теряете только premium QoL слой.»** — для Hosted Services / Managed Operations (на launch верно структурно).
- **«Управляемое облако, но без AWS-style вендор-лока. На руках всегда есть полный экспорт для миграции куда угодно.»** — для Turnkey Cloud (roadmap).
- **«Единственный managed PaaS, где переход на self-host — управляемая операция через MigrationPlan, не миграция руками с риском downtime.»** — для Turnkey Cloud, после реализации cross-cluster MigrationPlan (Phase 5+ / Product 2 = Phase 8). Roadmap, не launch.
- **«Migrate between clouds in under 2 minutes. Verifiable. Open scripts.»** — только после synthetic test suite, с конкретными числами. Roadmap, не launch.

### Distribution-замечание

AI discoverability — самостоятельный distribution channel: для vibe-coder и значительного non-DevOps сегмента **первичный читатель docs — AI-ассистенты**, не сами пользователи. Следствия для messaging — llms.txt обязателен, AI-предсказуемые CLI-команды, public presence как долгоиграющая инвестиция; на pre-launch стадии канал друзей-знакомых эффективнее публичного broadcast'а. (Operational детали content-стратегии и adoption sequence — в соседних разделах, не дублируются здесь.)

---

## Миграция и anti-vendor-lock

Anti-vendor-lock — не побочный комфорт, а структурный differentiator, который AWS / GCP / Vercel / Railway **архитектурно не могут предложить**. У них cancel-subscription = downtime + полная миграция приложения. У нас весь смысл в обратном: кластер, приложения, манифесты и данные всегда переносимы — это core promise, а не маркетинговая фигура речи. Эта секция описывает две независимые механики, на которых держится promise: (1) **Export to Self-host** как exit-семантика по managed-плану, и (2) **cross-cluster MigrationPlan** как два отдельных продукта (one-time toolkit и active federation). Сам primitive `MigrationPlan` зацементирован в ADR 0012 (MigrationPlan as a first-class concept) и ADR 0027 (unification with scope discriminator) — он же одновременно служит agentic-safety gate (детали MCP-стороны живут в секции про MCP & agentic safety, здесь только пересечение).

> **Sync-note по сравнению с pre-speedrun источниками.** В исходных файлах Export подавался как «Phase 4 feature внутри managed launch». Per `speedrun-plan.md` это пересмотрено: managed-плагин на запуске — **Hosted Services**, и для него Export закрывается **структурно с дня launch'а** — отдельный Export CLI Phase 4 для этого плана не требуется (см. ниже). Различай два понятия: **hardware tier** (T1–T4, hardware substrate; T1 и T2 оба в OSS-core на launch) и **managed plan** (Hosted Services → Managed Operations → Turnkey Cloud + Enterprise TBD, терминология per ADR 0034). Exit-семантика ниже привязана к managed-плану, не к hardware tier.

### 1. Export to Self-host — first-class anti-lock feature

Export to Self-host генерирует полный пакет для переноса под собственное управление: `Infrastructure` manifest текущей инфры, `ExternalSurface` manifest (git, registry, monitoring, backups), все Application-манифесты + DevProfile'ы, список AccessGrant'ов, зашифрованный пакет секрет-store состояния под клиентским master key, runbook «как развернуть OSS-версию на своей инфре», bootstrap-скрипт для подцепки к существующему кластеру и — отдельно важно — документ **«что вы теряете при переходе»** (явный список managed-only фич с OSS-альтернативами там, где они есть).

Точная формулировка anti-lock promise: при переходе managed → self-host клиент получает **всю платформу целиком** — Rust operator с CRDs, шесть platform services (PG / JetStream / ClickHouse / Redis / S3 / Notifications), Backstage с custom-плагинами, CLI со всеми subcommand'ами, observability stack, MCP server, dev mode. На managed-стороне остаётся узкий слой premium QoL (AI insights, cross-cluster aggregator, smart bill optimization, premium-интеграции). По объёму это несравнимо — массивная платформа уезжает к клиенту, тонкая cloud-надстройка остаётся у нас. (Сам водораздел OSS vs managed-only — предмет отдельной Open-Core-секции, здесь не дублируется.)

**Exit-семантика по managed-плану:**

| Managed plan | Серверы клиента | Exit без downtime? |
|---|---|---|
| **Hosted Services** (launch) | Клиентский Hetzner-аккаунт | **Да, структурно.** Cancel = revoke registration token → hosted services отключаются → OSS-кластер продолжает работать без изменений. Кластер клиента уже автономен (operator + kine/etcd + NATS + все данные на его Hetzner), миграция вообще не требуется |
| **Managed Operations** (Phase 4/4.5) | Клиентский Hetzner-аккаунт | **Да.** Отписался — кластер работает; к клиенту возвращается ручная обработка abuse / cost / backup |
| **Turnkey Cloud** (Phase 5+) | **Наш** Hetzner-аккаунт | **На launch плана:** «easy exit» с миграцией инфры (manifests + data export на руках). **Позже** (cross-cluster MigrationPlan Product 2, §2) — near-zero-downtime через orchestrated migration |

Ключевой сдвиг относительно pre-speedrun источников: поскольку launch-плагин — Hosted Services, а в нём customer's cluster — это его собственный OSS-install, **killer feature «Open Core + Export» закрывается structurally с дня запуска**, причём сильнее, чем через гипотетический Phase 4 Export CLI, потому что никакой миграции не требуется. На launch это сводится к honest claim: «Cancel anytime — your cluster keeps working as OSS. Zero migration required.» Export CLI как богатый пакет-генератор (см. список артефактов выше) актуален в основном для Turnkey Cloud, где инфра живёт на нашем аккаунте.

Честное сравнение downside для клиента, которое можно класть в маркетинг как есть:

- AWS / Vercel / Railway: «ваше приложение перестанет работать, нужна полная миграция».
- AppRafter Hosted Services / Managed Operations: «вы потеряете умные алёрты на bill и AI-инсайты, кластер продолжает работать».
- AppRafter Turnkey Cloud: «нужно перенести серверы, но у вас на руках manifests + data export, миграция направленная».

### 2. Cross-cluster MigrationPlan — два продукта

После анализа реальных migration patterns Group C / D это не один продукт, а два, с разным horizon'ом и разным маркетинговым framing'ом.

#### Product 1 — One-time migration toolkit (Phase 4-5)

Покрывает realistic-паттерн: customer переходит с AWS / GCP / Azure на AppRafter одним направлением через **planned maintenance window**, а не near-zero-downtime exit. Наша роль — **minimize pain и provide tooling**, не «eliminate downtime». Честный framing: «predictable 2-8 hour maintenance window в зависимости от объёма данных + advance engineering work для service-paradigm changes; мы снижаем боль через tooling и documentation, не обещаем zero downtime». (Synthetic-числа вида «Hetzner → AWS round-trip: 1m 47s» — это маркетинг Product 2, **не** Product 1.)

**Service-mapping reality для AWS → AppRafter** — это сердце честного позиционирования: часть сервисов мигрирует 1:1, часть требует рефактора кода клиента.

| AWS service | AppRafter equivalent | Сложность |
|---|---|---|
| RDS PostgreSQL | CNPG | **Easy** (mostly 1:1) |
| ElastiCache Redis | Dragonfly | **Easy** (~95% совместимо) |
| S3 | MinIO / Garage | **Easy** (S3 API одинаков) |
| SES | Notifications service | **Easy** (SMTP relay) |
| Secrets Manager | OpenBao | **Easy** (export + re-import) |
| EBS volumes | LINSTOR / local-path | **Easy** (standard volumes) |
| SQS | JetStream | **Medium** — другая paradigm, **рефач кода клиента** |
| EventBridge | JetStream subjects | **Medium** — customer-side refactor |
| IAM | AccessGrant + SPIFFE | **Medium** — другая identity-модель |
| Aurora-specific | CNPG | **Medium** — принять ограничения CNPG |
| Lambda | Containers сейчас / WASM edge-functions (Phase 7+) | **Hard** сейчас (re-containerize), **Medium** в будущем |
| DynamoDB | PG / external NoSQL | **Hard** — нет NoSQL в стеке |
| CloudFront | External (Cloudflare) | **External** — нет CDN |
| Cognito | External / build own | **External** — нет auth-as-a-service |

Реальный customer journey: pre-migration assessment (1-3 недели) → app code adjustments (недели-месяцы, рефактор SQS→JetStream, re-containerize Lambda, DynamoDB-specific code) → staging migration (1-2 недели) → production maintenance window (часы) → post-migration validation (1-4 недели).

Состав toolkit:
- Per-service one-time migration tools: CNPG через `pg_dump`/`pg_restore` или logical replication; JetStream через `nats stream backup/restore`; Dragonfly через `BGSAVE` + RDB; ClickHouse через `BACKUP`/`RESTORE` + S3 intermediate; MinIO/Garage через `mc mirror`; OpenBao export + re-encrypt; PV через `restic`/`Velero`.
- **redpanda-connect (бывший Benthos) как universal data-movement engine** для сервисов без native streaming-replication tooling. Паттерн: bulk snapshot заранее → redpanda-connect читает source с lag-detection и догоняет delta → в cutover-окне pause writes, ждём lag → 0, switch traffic → decommission source. Это значительно сокращает cutover-окно для больших datasets vs full dump+restore. redpanda-connect уже в нашей экосистеме (интегрируется с JetStream).
- **Plugin Migration Interface** — архитектурное решение, зафиксированное в Phase 4: каждый platform service (built-in или community plugin) реализует `MigrationSupport` contract (`snapshot` / `restore` / опциональный `incremental_sync` / `preflight_check` / `estimate`). Это превращает toolkit из «наш набор для наших сервисов» в **extensible platform primitive** — community-плагин для MongoDB / Cassandra / Neo4j реализует тот же interface и работает без модификаций toolkit'а, открывая сценарии, которые мы сами не покрываем (Mongo Atlas → self-hosted, Snowflake → ClickHouse и т.д.). Совпадает с философией ServiceProviderPlugin.
- **Pre-flight checks tool** (`apprafter migrate preflight --plan …`) — checklist перед выполнением: source healthy, target bootstrapped, все Application-манифесты валидны в target, все `needs.*` resolved, backup-destination настроен, DNS TTL подготовлен. Каждый issue идёт с suggestion. Превращает migration day-of из stressful event в predictable checklist. Must-have для обоих продуктов.
- Service-mapping documentation (AWS → AppRafter cheatsheet), refactoring templates (SQS→JetStream примеры, Lambda→container шаблоны), migration project tracker (Backstage view).

Объём Product 1 — 2-3 месяца FT в Phase 4-5. На launch (per speedrun) присутствует только **light-версия**: documented Supabase → AppRafter (PG dump/restore + connection-string rewrite) и Railway → AppRafter (env-import + Dockerfile detection), достаточно для claim «migrate from Supabase в один день». Full Product 1 — post-launch bucket C.

#### Product 2 — Active federation (Phase 8+)

Bidirectional cluster federation с sub-second cutover. Покрывает: **Turnkey Cloud → Self-host** (exit без downtime — killer anti-vendor-lock feature), self-host → Turnkey Cloud onboarding, DR failover (active-active multi-region), region-миграции внутри Turnkey, self-host A → B (cross-cloud, например Hetzner → AWS), M&A integration, cluster splitting по нагрузке.

Механика — оркестрация уже существующего в стеке инструментария, а не написание с нуля: CNPG streaming replication, JetStream stream mirroring, Dragonfly master-replica, ClickHouse через Keeper, S3 bucket replication, cross-cluster SPIFFE federation, sub-second cutover orchestration. Workflow — последовательность этапов `MigrationPlan` с pause-for-approval gate на каждом: export configs → bootstrap пустого target → setup federation (mTLS/SPIFFE bridge) → start replication → wait lag → 0 → cutover (sub-second writes-pause, promote target, switch traffic) → drain/decommission source.

Маркетинговая дисциплина критична для избежания over-promise: claim — **«near-zero-downtime»** / «orchestrated migration with sub-second cutover», не «zero-downtime» (HTTP writes — sub-second blackout; apps с broken connection handling видят retry-able errors; real-time apps типа трейдинга почувствуют). Доказываются числа **измеряемыми open-source synthetic-тестами** (например «Hetzner → AWS migration: 1m 47s unavailability, reproducible»), которые одновременно работают как marketing material + CI regression test + sales demo + trust builder.

Почему «no one else built this»: не «никто не захотел», а **никто не был в архитектурной позиции** сделать это без massive standardization tax. Cozystack / Deckhouse / OpenShift поддерживают N×M комбинаций (любой PG → любой PG через любой storage); у нас **1×1** — наш CNPG → наш CNPG, identical operator versions. Разница в порядки сложности — это прямое следствие opinionated-stack «one way to do things».

**Validation перед коммитом на Product 2:** Phase 4-5 — Product 1 даёт real customer data о том, как клиенты мигрируют; Phase 8 — если спрос валидирован (>10% Turnkey-клиентов либо явный enterprise pull), делаем full Product 2, иначе остаёмся на Product 1 + assisted. Чтобы опция не закрылась, в Phase 4 закладываются дешёвые design constraints: глобально-уникальные ResourceClaim IDs (не cluster-local), federation-ready audit log (timestamp ordering, identity propagation), cluster-scope в AccessGrant, отсутствие cluster-local assumptions в operator state.

### 3. Связка с agentic safety

Тот же `MigrationPlan`, который защищает от человеческих ошибок при destructive-операциях, без модификаций работает как gate для AI-агентов: AI через MCP создаёт `MigrationPlan` со статусом pending-approval → человек одобряет (на launch — через CLI `apprafter migration approve/reject` + approve/reject Resource Actions в Argo CD UI; Backstage MigrationPlan plugin — post-launch first bundle) → MigrationController в кластере клиента исполняет или прерывает. Это **real CRD-based gate**, а не soft API gate: admission webhook enforce-ит классификацию, AI agent не может bypass. Messaging: «AppRafter has built-in agentic safety because we built guardrails for humans first». Полная risk-taxonomy и identity-модель MCP-токенов — в секции MCP & agentic safety (ADR 0036); здесь зафиксировано только то, что migration-primitive и agentic-gate — это один и тот же зацементированный механизм.

### 4. Где живёт архитектура

Решения по этой теме зацементированы в ADR и являются source of truth, на который ссылается данная стратегия (а не наоборот): **ADR 0012** и **ADR 0027** — `MigrationPlan` как first-class primitive с scope discriminator; **ADR 0034** — managed offering model и разделение hardware tier / managed plan; **ADR 0035** — Minimal Data Exposure (фундамент structural-exit на Hosted Services: мы видим только metadata, не data plane); **ADR 0036** — MCP & agentic-safety model; **ADR 0037** — managed control-plane infrastructure; **ADR 0038** — Tier-2 Kamaji opt-in; **ADR 0031** — `apprafter-agent` ↔ hosted-bus протокол (канал, по которому managed control plane создаёт `MigrationPlan` CRD в кластере клиента).

---

## Go-to-market и launch

Go-to-market у AppRafter строится вокруг одного launch managed plan — **Hosted Services** (€10/mo per cluster, самый лёгкий из трёх уровней Hosted Services → Managed Operations → Turnkey Cloud, терминология и архитектура зацементированы в ADR 0034). Это не «managed придёт в Phase 4»: managed-first launch — это и есть запуск. Hardware tiers T1 (single-node) и T2 (3-node HA) оба входят в OSS core на launch, поэтому customer на signup выбирает hardware tier, а managed plan на старте один. Дальше — последовательность действий до launch (pre-launch action items), модель adoption без раннего платного маркетинга, dogfooding и content-marketing. Pricing-цифры, обоснование premium и per-persona hooks — в разделе про pricing; здесь только то, как мы выходим на рынок.

### Launch action items по приоритетам

Pre-launch action items написаны в pre-speedrun рамке («managed = coming soon»), поэтому их «non-managed self-host launch» пункты нужно читать как **то, что делает onboarding на Hosted Services не-болезненным**: customer всё равно проходит OSS-flow (`init` → `apply` → `bootstrap-all`) на собственном Hetzner, прежде чем register'ит cluster в managed (onboarding journey — 4 шага, см. раздел про onboarding/UX). Эти пункты остаются критичными именно потому, что cluster — это их autonomous OSS-install.

**P1 — обязательно до launch:**

- **Bootstrap idempotent resume (§3.1).** `bootstrap-all` resumable при сбое любой стадии — падение на шаге 3 из 4 продолжает с того же места, не с начала. Первый опыт «падает и непонятно как чинить» = customer ушёл ещё до managed signup. M1.5 Track A.9 уже свёл bootstrap в один orchestrated flow со staged output; idempotence — это polish поверх него.
- **DNS/TLS verbal hints (§3.2).** Конкретные подсказки в provisioning: «создайте A-запись `myapp.dev → 1.2.3.4`», «для HTTP01 порт 80 публично / DNS01 требует API token регистратора». Снижают молчаливое требование, об которое спотыкаются все persona. Частично закрывается автоматизацией — pulled-up `external-dns` + DNSZone CRD (speedrun bucket B 4.4a) убирает ручной DNS-step для customer apps на managed subdomain.
- **llms.txt + AI-friendly docs (§3.3).** `llms.txt` в корне docs-сайта (опыт переноса с onebun.dev/ai-docs уже есть), все ADR и guides под AI-traversal: чёткие якоря, таблицы, типизированные code-blocks, минимум скриншотов в core docs, AI-предсказуемые CLI-команды. **Первичный читатель docs — AI-ассистент, не сам пользователь** — для Lisa-class target persona (AI-augmented, primary loop = AI-agent деплоит за неё) это не nice-to-have, а основной distribution channel. Делать с первого дня docs-сайта.
- **Beginner's blog series (§3.5).** Отдельный layer от core docs: «как зарегистрировать Hetzner и получить API token», «как настроить домен и DNS», «что такое SSH-ключ», «деплой первого приложения за 30 минут». Lisa и Маша не пойдут в spec.md — им нужен soft-onboarding. Наполнение к launch.
- **Migration docs из нейтральных форматов (§3.6).** `docker-compose.yml` → Application manifest, `ecosystem.config.js` (pm2) → Application manifest, возможно `Procfile`. Косвенно покрывает Railway/Fly/Render (они работают с compose), не играя в их сторону напрямую — прямой migration-wizard под конкретного конкурента это плохой look для opinionated platform. На launch это дополняется managed-side migration helpers (Supabase basic: PG dump+restore+connection rewrite; Railway basic: env import + Dockerfile detection) — light-версия для claim «migrate from Supabase в один день», не Product 1 full.
- **Landing messaging (§3.7).** Badge/sub-headline «Don't know Kubernetes? You shouldn't have to.» — формализованный страх Lisa. Tier-формулировка как **примерная** («starting from ~€5/mo, you choose specific machines and counts»), не как закрытый список из 4 вариантов — жёсткая гранулярность отпугивает Машу ощущением «только эти options».

**Landing messaging — reconciliation для launch.** Hero «Opinionated platform for the solo founder who'll outgrow it» остаётся. Но раз managed запускается сразу, лендинг несёт **обе** offering: OSS self-host (€0, играет роль free tier) и Hosted Services (€10/mo per cluster). Честные launch claims следуют strict launch-claims ruleset — говорим только то, что работает:

- «Sign up for T1 single-node or T2 3-node HA — both available today.»
- «T1→T2 migration coming Q3 2026» (post-launch first bundle, ~2–4 недели после launch — см. §7.8 speedrun про delay risk; timeline strict).
- «Cancel anytime — your cluster keeps working as OSS. Zero migration required.» (structural killer feature #2, закрыт архитектурой Hosted Services).
- «We see metadata, not your data» (Minimal Data Exposure by architecture, ADR 0035).
- «MCP-enabled with CRD-based agentic safety gate. Approvals via CLI and Argo CD; Backstage UI plugin Q3» (ADR 0036).
- НЕ заявляем: replayable audit log (kine+NATS post-launch), Tier 3/4, hard multi-tenancy (Kamaji opt-in post-launch, ADR 0038).

Vendor-lock / agentic-safety / data-exposure messaging принадлежит соответствующим разделам (managed positioning) — здесь только то, как формулируем на лендинге для launch.

**P2 — polish к или сразу после launch:** backup destination opinionated default (Hetzner Storage Box one-click для Hetzner-юзеров; иначе «потом настрою» → потеря данных), `apprafter logs/status/exec/restart` как first-class CLI (Backstage остаётся главным порталом, но CLI покрывает 80% дневных нужд), OpenBao-vs-Railway-env explainer (почему «сложнее» ≠ «хуже»: rotation, ACL, audit, dynamic secrets), QoL-команды (`ssh-key generate`, `doctor`), monorepo-detector для `dev init`. Часть из них пересекается с post-launch backlog speedrun'а — не блокируют первичный запуск.

### Adoption sequence — без раннего платного маркетинга

Adoption: **sequential curve вместо big-bang launch**:

1. **Self / founder dogfooding** — основная масса bugs ловится тут, до входа первого customer.
2. **Solo founders / mini-teams** — organic discovery, расширение coverage.
3. **Group A organically** — через HN/Reddit/aggregators, по technical content (см. ниже).
4. **Group A active outreach** — Phase 5–7, когда есть track record.

**Marketing timing principle:** активный платный маркетинг раньше Phase 5 — wasted budget. Solo founders/mini-teams находят продукт organically; Group A не доверяет до track record. Paid acquisition на launch (Phase 4) не нужен. **Но content marketing ≠ paid acquisition** — технические посты с launch это slow-compound asset, окупающийся на Phase 5–7. На pre-launch стадии канал друзей-знакомых эффективнее публичного broadcast'а: ссылка, переданная в активной conversation, попадает туда, где AI-ассистент уже консультируется — и это же работает на попадание в AI training data к моменту, когда Group A начнёт искать. Solo-execution даёт окно для конкурентов (6.5–9.5 месяцев календарно по speedrun) — mitigation именно через раннюю AI-discoverability и public roadmap ещё до launch.

### Dogfooding — «AppRafter on AppRafter»

**ADR 0037** (где архитектура managed control plane зацементирована окончательно) задаёт dogfooding-модель: managed control plane — Account UI backend, hosted Backstage, hosted MCP endpoint, agent-bus, терминирующий `apprafter-agent` outbound-соединения (протокол — gRPC streaming на launch, ADR 0031) — работает **как стандартные AppRafter `Application` workloads на нашем собственном Hetzner hardware-tier-2 substrate**. Публичная формулировка: «AppRafter Cloud runs on AppRafter Platform». Managed domain — **`apprafter.dev`** (ADR 0037 разрешил прежний дрейф между двумя кандидатами; customer app subdomains `*.<customer>.apprafter.dev` делегируются отдельно).

Triple benefit dogfooding'а: COGS-экономика (Hetzner-based COGS в разы ниже AWS-based у конкурентов), trust signal и structural moat (конкурентам на AWS этот pattern архитектурно недоступен из-за recursive billing/IAM headaches). Для Group A («если эти ребята положили собственную business-continuity на свой же продукт — там что-то работает») сигнал читается сильнее SOC 2 report'а. Concrete GTM-actions:

- Публичный status page: «AppRafter Cloud runs on AppRafter Platform since [date]» + uptime metrics.
- Одна строка на лендинге со ссылкой на uptime.

**Release engineering как hard requirement к launch (ADR 0037, §7.5):** цена ошибки асимметрична — customer, которому продали «we eat our own dog food», не прощает выяснения, что собака так себе. Поэтому к launch обязательны:

- **Staging-AppRafter** — тоже AppRafter-managed, изолирован от customer prod; synthetic load generators гоняют heavy scenarios (stateful, high-write Postgres) до входа первого Group A customer.
- **Canary rollout** на 5–10% customer кластеров перед all.
- **Rescue cluster** (killer feature #22) — отдельный bare-k8s кластер с прямым kubectl access, breakglass credentials и backup orchestration, **не** managed через AppRafter UI/MCP. Закрывает recursive-dependency risk: bug в платформе ломает customer clusters И нашу способность их чинить, поскольку tooling живёт на той же платформе. Это отдельный recovery-runbook именно для AppRafter Cloud (cross-ref ADR 0037), и одновременно dogfooding-signal для маркетинга — «у нас тоже rescue есть». Tooling tax платится один раз; downside от откладывания — potentially destructive для всего managed offering на самой ранней стадии.

### Content marketing — категории и cadence

Content marketing: контент, который компаундится. **Cadence — один пост в месяц** across категорий, устойчиво без burnout; за год — 12-piece техническая библиотека.

- **Категории:** architecture decisions (наш spec / ADR-серия — half-готовый материал для разворота), trade-off retrospectives («почему kine+NATS вместо etcd, что узнали через N месяцев»), deep dives in primitives (ResourceClaim, MigrationPlan execution flow), postmortems по факту инцидентов.
- **Post order для максимального design-signal'а** (experienced commenters дают gold-фидбек): (1) kine+NATS replacing etcd, (2) MigrationPlan + destructive-change semantics, (3) ResourceClaim/ServiceProvider (Crossplane-crowd придёт сравнивать), (4) CUE choice + tier-uniform manifest (ждать bikeshedding, но и пару полезных pointer'ов).
- **Postmortem culture** — Cloudflare-стиль (technical depth, blameless framing, honest about gaps, concrete remediation timeline). Читают religiously **в evaluation moments**: когда Group A CTO оценивает AppRafter, лезет в архив incidents. Признание слабости paradoxically повышает доверие.
- **Anti-bikeshedding framing для emotional topics** (license FSL, Rust-vs-Go): «Here's our reasoning. Design closed, обсуждение welcome но решение не меняется.» Эти посты делают brand-building, не design refinement.
- **Calibration:** HN-тред на 200 комментариев — 5–10 действительно ценных. Фильтр: experience-grounded > theoretical. Public discussion ≠ public veto — solo founder держит decision rights.

### Reconciliation-заметка

Исходные pre-launch action items и adoption-модель (привязка к «Phase 4 launch») написаны до speedrun'а. Действующая модель: managed (Hosted Services) запускается на launch, T1+T2 в OSS core, managed control plane dogfooded на `apprafter.dev` per ADR 0037. Пункты, помеченные «Phase 4» / «coming soon», читать через speedrun-bucket-маппинг (`speedrun-plan.md` §2–§3) и зацементированную архитектуру в ADR 0034–0038.

---

## Открытые вопросы и риски

Этот раздел сводит open questions и риски из исходных черновиков с поправкой на текущее состояние: значительная часть «вопросов» уже закрыта speedrun-планом и ADR-серией 0031–0038. Архитектурные решения здесь не повторяются — они зацементированы в ADR; ниже остаются только genuinely-open вопросы, риски launch и метрики, которые надо снять на первых платящих.

### 1. Закрыто ADR / speedrun (снято с повестки)

Эти пункты фигурировали как «открытые» в pre-speedrun черновиках, но решены. Перечислены, чтобы не возвращаться к ним как к open questions:

- **Backstage vs custom portal на v1.0.** RESOLVED — остаёмся на hosted Backstage с нашими плагинами для launch (spec §7 #6, подтверждено speedrun §5.1). Custom portal на OneBun/Svelte — это post-1.0 option, не launch-вопрос; re-evaluate только если Lisa-feedback по Backstage UX окажется слабым. Снимает прежний тройной выбор (a/b/c) portal-стратегии.
- **Архитектура managed (Option B, Minimal Data Exposure).** Зацементировано в ADR 0034 (managed offering model, hardware tier vs managed plan) и ADR 0035 (Minimal Data Exposure — мы видим metadata, не customer data plane). Hosted Services как launch-plan делает MDE structural, не аспирационным.
- **Протокол `apprafter-agent`.** RESOLVED — gRPC streaming с Rust-агентом (ADR 0031, speedrun §5.6). NATS-transition отложен до kine+NATS миграции post-launch. Вопрос «какой транспорт» закрыт.
- **MCP & agentic safety.** Зацементировано в ADR 0036 — structural enforcement на platform boundary через real MigrationPlan CRD, не soft API gate. Killer features #3 и #7 закрываются на launch (approvals через CLI; Backstage-плагин в post-launch first bundle). LLM-reviewer для approval — Phase 4+, не launch-critical.
- **Managed control plane — где хостить.** RESOLVED — dogfooding: наш host = AppRafter на собственном Hetzner-аккаунте (ADR 0037, speedrun §5.5). Account UI backend спавнит customer namespaces как обычные AppRafter Application instances. Marketing-claim «we host AppRafter on AppRafter» + rescue-cluster recovery.
- **Multi-tenancy / Kamaji.** RESOLVED — Tier 2 = HA substrate, hard multi-tenancy через Kamaji = opt-in (`PlatformStack.spec.values.multitenancy: true`), default off (ADR 0038, speedrun §5.7). В Hosted Services tier вопрос namespace-per-customer / Capsule vs Kamaji вообще N/A — customer cluster полностью автономен. Re-opens только когда landit Turnkey Cloud.
- **Pricing-сетка.** Зафиксирована: Hosted Services €10/mo per cluster, annual = monthly × 10 («save 2 months») = €100/year, 14-day trial без cc once per account, no free tier, prepaid (speedrun §5.3). Открытым остаётся только эмпирическая калибровка после первых customers — см. метрики ниже.
- **Notifications service на launch.** RESOLVED как drop — hosted notifications завязаны на AccessGrant lifecycle emails, а AccessGrant CRD в post-launch backlog. Transactional email на launch идёт напрямую через Postmark/Sendgrid (speedrun §7.5). Notifications landit вместе с AccessGrant.
- **«Tier 1 only на launch».** RESOLVED — T1 и T2 оба в OSS-core на launch, customer может signup сразу на любой (speedrun §7.1). Это hardware-tier вопрос, не managed-plan.

> Терминология: «hardware tier» (T1–T4) и «managed plan» (Hosted Services → Managed Operations → Turnkey Cloud + Enterprise TBD) — разные оси, per ADR 0034. Ниже «tier» = железо, «plan» = managed-уровень.

### 2. Genuinely-open вопросы

Это то, что осталось нерешённым и требует либо решения перед launch, либо явного отнесения к post-launch с trigger-условием.

- **Sub-processors disclosure — финализация списка.** Состав минимальный для Hosted Services (мы не sub-processor для compute и не для customer data plane): Stripe, email provider, опционально Sentry/PostHog, опционально DNS-провайдер parent-домена. Hetzner и customer DNS в список НЕ входят (это customer's relationships). Сам документ ещё не написан — нужен draft перед public launch (~1 день, самая лёгкая disclosure из трёх plan'ов).
- **Домен managed-сервисов.** RESOLVED — `apprafter.dev` (ADR 0037; подтверждено пользователем).
- **Backup/DR для самого control plane.** Control-plane outage = customers теряют UI/MCP access (их кластеры продолжают работать, но experience плохой). План — standard backups в external S3, но конкретику DR control plane нужно дописать (часть ADR 0037 scope).
- **Soft-wrapper `curl | sh` orchestrator — нужен ли на launch.** Onboarding journey = 4 шага (Hetzner setup → CLI install → `target add` + `bootstrap-all` → managed signup + register), против ~3 у Vercel. Открытый вопрос: достаточно ли Account UI walkthrough + 90-секундного видео, или нужен soft wrapper (~3–5 дней FT). Решается по beta-сигналу, не proactively. Если onboarding friction окажется systematic blocker — это сигнал ускорить Managed Operations plan для Lisa-сегмента, а не латать Hosted Services (speedrun §7.6).
- **`apprafter` для не-Hetzner провайдеров.** Спека упоминает AWS как built-in и community plugins для остального. Когда AWS-builtin становится приоритетным — вопрос конкретного спроса, не launch-блокер (для managed launch single substrate = Hetzner cpx-class достаточно).
- **kine+NATS scaling ceiling.** Эмпирический вопрос, отвечается в production. На launch — etcd; kine+NATS в post-launch backlog (закрывает killer feature #21 replayable audit log, когда replayability понадобится как differentiator).
- **CUE vs Pkl re-evaluation.** Spec §7 — Phase 5 как formal review milestone. Не managed-launch вопрос, но держим в виду.
- **Cross-cluster MigrationPlan automation (Product 2).** Когда переходить от manual toolkit к full automation — зависит от real demand из Group B/C. Phase 8 territory, далеко за launch. Не дублируем содержимое из раздела про MigrationPlan / два продукта.
- **Hetzner reseller — отдельное юр.лицо vs личное ИП.** Релевантно только для Turnkey Cloud plan (Phase 5+). На launch (Hosted Services) reseller risk NOT APPLICABLE — customer держит Hetzner-relationship напрямую. Решать с юристом ближе к Turnkey.
- **Web-UI managed onboarding для Лизы-сегмента.** Частично закрывается Account UI walkthrough (часть §3.3 scope), но финальный объём onboarding UX — открытый design-вопрос, итерируем по beta-feedback.

### 3. Риски launch и mitigations

- **Solo execution risk.** Sequential 6.5–9.5 месяцев = window для конкурентов (Coolify добавляет MCP, Vercel/Railway — agentic safety, новый entrant). Mitigation: public roadmap + early developer mindshare через AI-discoverability (llms.txt, public presence) ещё до launch (speedrun §7.7).
- **`apprafter-agent` compromise risk.** В Hosted Services мы не держим customer credentials, поэтому token-storage risk из ранних ревизий неприменим. Новый risk: agent имеет outbound trust к нашему hub; compromised agent (supply-chain на наши binaries) → MCP-style операции против customer cluster через legitimate-looking channel. Mitigations: cosign-подпись binaries, narrow permissions (через AppRafter CRD, не cluster-admin), customer может revoke registration token в любой момент, audit log агентских операций виден customer'у. Severity ниже, чем у API-token-holding Managed Operations plan (speedrun §7.2, ADR 0031).
- **T1→T2 migration delay risk.** Migration deliverable отложен на post-launch first bundle (~2–4 недели после launch). Customer, подписавшийся на T1 в первые недели, имеет stuck path к T2 без manual workaround. Mitigations: marketing честно говорит «start T1 if small, T2 if scale needed now, migration coming Q3»; support fallback (fresh T2 + redeploy manifest + PG dump/restore через §3.8 helper); founder hands-on помощь beta-customers. Severity Low если migration tool выходит в planned ~4-недельное окно; Higher при slippage до 2+ месяцев — strict timeline на post-launch first bundle критична (speedrun §7.8).
- **Recursive dogfooding dependency.** Мы хостим managed на собственном AppRafter — bug в core может уронить и наш host, и customer-facing managed-слой одновременно. Mitigation — rescue cluster (#22 killer feature) + DR plan control plane (ADR 0037). Это же одновременно trust-signal: «у нас тоже есть rescue».

### 4. Что измеряем на первых 3–6 месяцах платящих

Pre-launch калибровка завершена; следующая reassess-точка — после первых 10–50 paying customers. Метрики:

- **Реальный COGS per cluster.** Infra-cost hosted Backstage/MCP/notifications + DR, поделённый на active clusters, плюс per-cluster ops-attention в часах/неделю. Decision rule: actual cost <€5/cluster → оставляем €10, маржу в development; actual cost >€7/cluster → review-точка для price bump до €15 на следующем plan-уровне.
- **Trial-to-paid conversion rate.** % из 14-day trial в paid. Если <10% — trial либо слишком short, либо feature scope не закрывает evaluation needs.
- **Annual vs monthly mix.** Launch скорее всего monthly-heavy. Когда annual mix дорастёт до 30–40% — это track-record / customer-comfort signal.
- **Hetzner-account add-on demand.** Сколько customer'ов реально использует >1 Hetzner-account per workspace. Частый pattern → возможно явная sub-pricing.
- **Managed-plan ladder-up signal.** Consistency запросов «уберите меня из Hetzner UI» (→ Managed Operations) и «не хочу регистрироваться в Hetzner» (→ Turnkey). Эти сигналы переводят следующий managed-plan из roadmap в active scope; MSP-сигнал (Андрей-A) или multi-org hard-isolation ask — trigger для Kamaji opt-in (ADR 0038).

---

