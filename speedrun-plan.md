# AppRafter — Managed Launch Speedrun

> **Источник:** сессия 2026-05-17 — разбор кратчайшего пути до managed launch.
> **Назначение:** working roadmap для managed-first launch. Ссылается на `plan.md` по точным подфазам, на `spec.md` по разделам, на `MANAGED_STRATEGY.md` / `KILLER_FEATURES_MATRIX.md` по контексту.
> **Scope:** managed-track + минимальный OSS-core substrate для него.
> **Статус:** draft, требует ADR-decisions перед стартом managed-track (см. §6).
> **Дата сборки:** 2026-05-17.

---

## 0. Контекст

### 0.1 Главный insight сессии

**Managed = product 2 поверх OSS-core, не его продолжение.** Это меняет всё: OSS-roadmap до Phase 8 строит платформу под все 4 tier'а × 9 personas; managed нужно меньше — один substrate (Hetzner cpx-class), одна аудитория (Lisa / Дима / early Маша), focused MCP-first story.

Из этого следует: **OSS закрывается ровно до точки где он становится substrate для managed**. Многое из `plan.md` не отменяется, а **переносится в приоритизированный post-launch backlog** и приоритизируется по сигналам от реальных платящих.

### 0.2 Зачем speedrun

Текущий `plan.md` → Phase 3 closure ≈ 14-18 месяцев FT solo. Managed launch естественно tied к Phase 4 в `KILLER_FEATURES_MATRIX.md` §2.4. Speedrun даёт **~7.5-11 месяцев календарно** до managed launch — экономия 6-10 месяцев.

### 0.3 Target persona на launch

Per recalibration сессии 2026-05-17: AI-augmented users (Lisa-class) — поправка x0.5 к моим первоначальным friction-оценкам. Они компетентны через AI, могут справиться с self-host barrier'ами. Но для managed-first launch остаются target ровно потому что:
- Их primary loop = AI-agent деплоит за них → MCP-native managed закрывает loop
- Self-host через managed они получают в момент когда им нужно («Export to self-host» — Phase 4+ post-launch feature)

Tier 1 prod-ready на launch. Tier 2 beta (наша managed-side ставит, customers могут upgrade).

### 0.4 Velocity baseline

Per memory: AppRafter ≈ 12k LOC prod/week FT. Distributed-systems penalty (-30%) **НЕ применяется** к этому speedrun'у — нет Kamaji / SPIRE / kine+NATS / multi-tenancy в pre-launch scope. Standard kube-rs controllers + standard SaaS scaffolding.

Frontend-heavy work в managed-track (Bun + Svelte portal) может иметь lower LOC-density — recalibrate когда первый frontend код landит.

### 0.5 Managed offering tier на launch — Hosted Services (Tier 1 из трёх)

Из трёх managed tiers per `MANAGED_STRATEGY.md` §3 — **на launch только Hosted Services** (самый лёгкий первый уровень).

| Tier | Что мы хостим | Customer's responsibility | Hetzner relationship | Cash flow exposure |
|---|---|---|---|---|
| **1. Hosted Services** (launch) | Backstage portal + плагины, Account UI, MCP server | Весь кластер на их Hetzner, sам `apprafter init`, sам Hetzner billing, sам abuse handling | Customer's direct | Zero (нет, мы не передаём compute через нас) |
| **2. Managed Operations** (Phase 4/4.5) | Всё из Tier 1 + abuse email parsing + cost monitoring + backup orchestration + API token health checks | Sам платит Hetzner; не управляет Hetzner UI | Customer's account + наш operational access (API token) | Низкая |
| **3. Turnkey Cloud** (Phase 5+) | Всё, включая Hetzner relationship | Только платит нам | Наш | Существенная (юр.лицо, VAT, DPA chain, abuse process) |

(Hosted **notifications service** в `MANAGED_STRATEGY.md` §3.1 list'ится для Tier 1, но scope = AccessGrant lifecycle emails. AccessGrant CRD в bucket C post-launch, поэтому hosted notifications **drop из launch scope**. Transactional emails — direct через Postmark/Sendgrid, не через notifications service abstraction. См. §7.5.)

**Hosted Services architecture (launch tier):**

- **Customer's cluster полностью автономен.** AppRafter operator + kine + NATS + все данные — на их Hetzner. Если они отключают наши hosted services — OSS-кластер продолжает работать.
- **Подключение через outbound + agent.** В customer's cluster живёт `apprafter-agent` который establishes outbound connection к нашему hosted MCP/Backstage. Inbound listener на customer side не нужен — никакой firewall config.
- **Registration token** заменяет наше владение customer credentials. Customer issues token к их cluster через `apprafter` CLI, paste'ит в Account UI signup, agent connects.
- **Что мы видим:** только metadata (per `MANAGED_STRATEGY.md` §2 Minimal Data Exposure principle) — manifest applies, status events, log streams (если customer opt'инт), audit events. **Не видим:** customer data в PG/Redis, secret values, в общем-то ничего из application data plane.

**Implications для speedrun:**

1. **Killer feature #2 (Open Core + Export) closes structurally с дня launch'а** — customer's cluster — это their OSS install. «Cancel» = revoke registration token → naszej hosted services отключаются → OSS-кластер продолжает работать без изменений. Даже не нужен Export CLI Phase 4.
2. **Killer feature #5 (Minimal Data Exposure) closes с launch** — by architecture мы literally видим только metadata. Hosted Services design = MDE ADR pre-launch enforceable.
3. **Compliance positioning сильнейшее из 3 tiers** — мы НЕ touch customer data plane, мы НЕ sub-processor для compute, мы НЕ оператор customer infra. Group C-friendly architecture с дня 1.
4. **Юр.поверхность minimal** — свой ToS, простой DPA (metadata only), нет sub-processor chain для compute.
5. **Hetzner reseller risk (`MANAGED_STRATEGY.md` §4) — NOT APPLICABLE** на launch tier (это Tier 3 concern).
6. **Onboarding journey — 4 шага** (Hetzner setup → CLI install → `target add` + `bootstrap-all` → managed signup + register). Friction acceptable если Account UI signup walkthrough hood. Soft wrapper `curl | sh` orchestrator — fallback option если в beta friction blocking. См. §7.6.

**Tier 2 / Tier 3 ladder up** post-launch когда:
- Tier 2 (Managed Ops): customer ask «уберите меня из Hetzner UI» становится consistent. Adds abuse parsing + cost monitoring + token health
- Tier 3 (Turnkey): customer ask «не хочу регистрироваться в Hetzner» + track record для юр.exposure. Phase 5+ territory

---

## 1. Killer features на managed launch

Per `KILLER_FEATURES_MATRIX.md`:

### 1.1 Закрываются на launch (sufficient для primary persona)

- **#1** Same-manifest T1→T2 vertical scaling — **T1+T2 ready at signup**, T1→T2 migration в post-launch first bundle (см. §6). Demo: customer выбирает tier на signup, обе работают
- **#2** Open Core + Export — **structural** через Hosted Services architecture (customer cluster autonomous; cancel = revoke token, OSS-кластер продолжает работать)
- **#3** MCP-native + agentic safety — **real CRD-based gate** (не soft API gate). Approvals через CLI на launch, Backstage plugin в post-launch first bundle
- **#5** Minimal Data Exposure — **structural** through Hosted Services (мы видим только metadata, не data plane). ADR pre-launch.
- **#7** MigrationPlan primitive — **full CRD + reconciler** embedded в apprafter-operator. Application + platform scope, destructive change classification, gate pause/resume
- **#13** Tier 1 simpler defaults — keep
- **#14** Typed config + composition (CUE + admission webhook) — keep
- **#17** No-VC structural compliance — always
- **#18** Cost-anchored transparent pricing — €10/mo published
- **#19** HTTPRoute auto-gen from Application.expose — pulled up 4.1a

### 1.2 Partial / в roadmap с explicit ETA в pitch

- **#6** One-time migration toolkit (Product 1) — partial: Supabase + Railway basic helpers. Full Product 1 — Phase 4-5
- **#9** Six platform services — **2 из 6**: `needs.pg` (CloudNativePG) + `needs.redis` (Dragonfly). Остальные on-demand: `needs.jetstream` bucket D, ClickHouse/S3 bucket C
- **#15** Same manifest dev/prod — partial: identical CUE manifest works. Dev mode CLI (Track C) bucket D на launch
- **#16** Sub-processors как bounded list — minimal (Stripe + email + analytics)
- **#22** Out-of-band rescue cluster — partial (depends на §5.5 host infrastructure decision)

### 1.3 НЕ закрываются на launch

- **#1 (Tier 3+4 aspects)** — Phase 5/6
- **#4** Cross-cluster MigrationPlan (Product 2) — Phase 8 territory
- **#8** Tier 4 confidential containers — Phase 6
- **#10** Cluster-admin constrain 8-layer — Phase 4+ (но Hosted Services architecture даёт #3 layer structurally — мы external, не оперируем customer cluster-admin)
- **#11** Hard multi-tenancy via Kamaji — post-launch backlog (Tier 2+ opt-in feature, не default на launch — spec deviation requires ADR §5.7)
- **#12** Vertical integration audit/identity — depends on SPIRE (post-launch)
- **#20** Plugin Migration Interface — Phase 5+
- **#21** kine+NATS replayable audit log — etcd на launch, kine+NATS post-launch когда audit replayability нужна как differentiator
- **#23** Live platform demo via AccessGrant — depends on Kamaji + AccessGrant + Karpenter (all bucket C)

### 1.4 Marketing implication

Launch claims следуют **strict KILLER_FEATURES_MATRIX §2.4 ruleset**: говорим только то что работает.

Honest launch claims:
- ✅ «Sign up for T1 single-node или T2 3-node HA — both available today»
- ✅ «T1→T2 migration coming Q3 2026» (post-launch first bundle, ~1 месяц после launch)
- ✅ «MCP-enabled с CRD-based agentic safety gate. Approvals via CLI, Backstage UI plugin Q3»
- ✅ «Cancel anytime — your cluster keeps working as OSS. Zero migration required»
- ✅ «We see metadata, not your data. Minimal Data Exposure by architecture»
- ❌ Не заявляем «replayable audit log» (kine+NATS post-launch)
- ❌ Не заявляем «Tier 3/4» (далеко)
- ❌ Не заявляем «hard multi-tenancy» (Kamaji opt-in post-launch)

---

## 2. OSS-core scope — категоризация items `plan.md`

| Bucket | Meaning |
|---|---|
| **A** | Keep as-is, в pre-launch critical path |
| **B** | Pull up из later phase в pre-launch |
| **C** | Defer to prioritized post-launch backlog (with trigger condition) |
| **D** | Drop from active scope (re-add только on explicit signal) |

### 2.1 Bucket A — keep as-is

| `plan.md` ref | Item | Reasoning |
|---|---|---|
| **1.66-1.83 subset** (M1.5 Track B) | ADR 0025 + 0028 + 0029 only | Platform reconciles itself через Argo CD; substrate для managed updates |
| **1.72-1.78 condensed** | PlatformController + MigrationPlan CRD (embedded в apprafter-operator) | ~1 нед FT condensed implementation. Closes #3 и #7 killer features через real CRD primitive. T1↔T2 диff небольшой; gate работает для tier change + Application destructive ops. CLI approve commands included. **Backstage MigrationPlan plugin (4.16) остаётся в bucket C** — bundles с migrate-to-tier post-launch |
| **2.1-2.4** | ServiceProvider + ResourceClaim + selector + `needs.pg` (CloudNativePG) | Без БД managed launch = пустая demo |
| **2.6** | `needs.redis` (Dragonfly) | Pull from bucket C: closes 2 из 6 platform services (#9), dogfooded by Account UI session storage / rate limiting |
| **2.6b** | `needs.disk` (block storage) | Launch storage primitive alongside pg+redis; добавлен в launch-scope (не было в ревизиях ≤5). |
| **2.10** | `needs` → NetworkPolicy auto-derivation | Free win, исключает класс security mistakes managed-side |
| **2.11** | SealedSecrets интеграция | Default secrets mechanism Tier 1 до SPIRE+OpenBao landing |
| **2.12** | `secret()` и `claim.*` references в `Application.env` | Без этого DX manifest неполный |
| **3.1** | HA-bootstrap (k3s 3-node + kube-vip + joins + dual-stack) | Pull from bucket C: T2 substrate. Standard k3s pattern, embedded etcd для HA storage. **NOT kine+NATS** — etcd handles HA по default |
| **3.3** | Cilium mTLS between workloads | Pull from bucket C: T2 substrate. Helm flag + validation tests |

### 2.2 Bucket B — pull up

| `plan.md` ref | Item | Reasoning |
|---|---|---|
| **4.1** | ExternalSurface CRD | Declarative external surface на launch |
| **4.1a** | HTTPRoute auto-generation | Без этого «Application deployed» ≠ «доступен на домене». Самая дешёвая UX win в плане |
| **4.4a** | external-dns + DNSZone CRD | Закрывает DNS friction (PRELAUNCH §3.2) автоматизацией вместо хелперов |
| **4.12** | Backups в external S3 (default ON Tier 1) | Lost DB = blame storm. PRELAUNCH §4.1 P2 |
| **3.7a** subset | Hubble enable + Hubble UI standalone | ~3-5 дней FT; high ROI на customer support + marketing claim |
| **3.4** minimal subset | OTel Collector + Tempo/Jaeger one-node + Prometheus/Grafana defaults | ~1.5 нед FT vs full 3.4 (L); НЕ включает auto-injection sidecar и full ClickHouse provider (Phase 3.5 → bucket C) |

### 2.3 Bucket C — defer to prioritized post-launch backlog

Каждый item имеет **trigger condition** — сигнал, при котором item re-activates. **Order должен resolve'иться на основании реальных customer signals**, не a priori.

| `plan.md` ref | Item | Trigger condition |
|---|---|---|
| **2.6a** | KEDA install + ScaledObject | First autoscaling customer signal |
| **2.7-2.8** | SPIRE + credential injection | First Tier 2 customer requesting OpenBao-grade secrets, OR compliance-focused customer ask |
| **3.2** | kine + NATS как control-plane storage | Когда audit replayability (#21 killer feature) станет marketing-critical, OR scale ceiling hit (etcd HA tolerates обычно достаточно для launch-scale) |
| **3.5** | ClickHouse provider (full obs backend) | Маша-class signal «нужны long-term traces/logs retention» |
| **3.6** | VictoriaMetrics integration | Same as 3.5 |
| **3.7b** | Backstage flow visualizer plugin | Depends on Hubble + Backstage UX priority |
| **3.8/3.8a** | Kamaji + Capsule + Tenant CRD (opt-in для T2, default для T3+) | First hard-mt customer ask, OR MSP scenario (Андрей-A) signal. **Spec deviation требует ADR (§5.7)** |
| **3.9** | Cilium Egress Gateway + family-aware static IPs | Customer signal «нужен static IP для third-party API» |
| **3.10 + post-launch first bundle** | `apprafter migrate-to-tier --to team` CLI tool + **4.16 Backstage MigrationPlan plugin** | **Post-launch first bundle (~2-4 недели после launch)**. Customer-side CLI tool (мы не управляем клиентскими кластерами) + Backstage plugin bundled because second user-visible MigrationPlan case lands then |
| **3.11/3.12** | OpenBao 3-node HA + SealedSecrets migration | Depends on SPIRE (2.7-2.8) landed |
| **4.5/4.5a** | AccessGrant + JIT cluster-admin | Team-of-3+ customer (Маша expansion). Solo handled portal auth |
| **4.6-4.9** | OIDC SSO + magic-link + auto-revocation | Same as 4.5 |
| **4.10** | Audit log cluster-admin tagging | Group C compliance signal |
| **4.13/4.14** | Trivy/Grype/SBOM + Build Report | Маша/Group A Phase 5+ feature |
| **4.15** | Cost view в Backstage | Заменён managed portal billing view на launch; OSS-side feature post-launch |

#### Post-launch priority (draft, требует пересмотра на 10-customer mark)

1. **`apprafter migrate-to-tier --to team` CLI + Backstage MigrationPlan plugin bundle** (~1.5-2.5 нед FT) — **first post-launch deliverable**, closes #1 killer feature fully + delivers Backstage UI для approvals
2. SPIRE + OpenBao migration (compliance unlock Tier 2)
3. Hubble→Backstage flow visualizer + full ClickHouse provider (obs depth для Маши)
4. Kamaji + Tenant CRD opt-in (hard mt для scale, ADR spec deviation)
5. AccessGrant + OIDC SSO (team features)
6. kine+NATS migration (audit replayability для #21 killer feature)
7. `needs.jetstream` + Notifications service (event-driven workflows)
8. Build pipeline Trivy/Grype/SBOM
9. Tier 3 (Talos/LINSTOR/Kata) — Phase 5
10. Confidential containers — Phase 6
11. Plugin ecosystem — Phase 7

### 2.4 Bucket D — drop from active scope

| `plan.md` ref | Item | Reasoning |
|---|---|---|
| **2.5** | `needs.jetstream` (NATS account/stream) | Lisa не event-driven. Re-activate на 2+ explicit requests |
| **2.13-2.16** | Notifications service (4 sub-phases L+M+M+M) | «Без notifications service все живут — у них своё решение». Managed-side notifications (billing/deploy) handled в portal. ~3 нед FT экономия |
| **4.2-4.3** | Forgejo/Harbor self-hosted | GitHub + ghcr.io достаточно. Re-activate для self-hosted compliance customers (Group C) |
| **4.4** | Headscale + Tailscale Operator | Managed portal auth заменяет VPN-thinking. Re-activate если self-host customers просят |
| **4.11** | Synthetic monitoring (Uptime Kuma external) | External SaaS (UptimeRobot etc.) на launch. Re-activate если customers просят in-cluster |
| **4.15a** | Cilium FQDN policies для external | Advisory only сейчас per `spec.md` Known limitations. Re-activate с Tier 2 security ask |
| **1.9** (Phase 1B Dev Mode) | Phase 1B Minimum Viable Dev Mode | Managed users не запускают local cluster bootstrap. Re-activate после managed traction → перед OSS-self-host marketing push |
| **Phase 5 full** | Tier 3 (Talos/LINSTOR/Kata) | Post-launch territory |
| **Phase 6 full** | Tier 4 (CoCo/AWS) | Compliance/sovereignty customer signal |
| **Phase 7 full** | Plugin ecosystem | Community contribution timeframe |

### 2.5 Velocity estimate — OSS-core до managed launch

| Block | Estimate (нед FT) |
|---|---|
| M1.5 Track B subset (ADR 0025/0028/0029) | 3-4 |
| PlatformController + MigrationPlan CRD (embedded, condensed 1.72-1.78) | 1 |
| Phase 2 minimum (2.1-2.4, 2.6 redis, 2.10-2.12) | 3.5-4.5 |
| T2 substrate (3.1 HA-bootstrap + 3.3 Cilium mTLS) | 1-1.5 |
| Pulled Phase 4 (4.1, 4.1a, 4.4a, 4.12) | 3-4 |
| Hubble enable (3.7a subset) | 0.5-1 |
| OTel minimal (3.4 subset) | 1.5 |
| **OSS-core total** | **13.5-17 нед FT** |

---

## 3. Managed-specific трек

Items **отсутствующие в `plan.md`** — это правильно, это product 2 work.

### 3.1 Hosted multi-tenant SaaS scaffolding

**Что:** наш host-кластер с N hosted services. Customer's clusters register к нам через agent, connect outbound. Мы не держим customer's Hetzner credentials или kubeconfigs.

**Surface (наш hosted side):**
- **API gateway / auth layer** — registration token validation, customer session auth, MCP token auth
- **Customer registry** — состояние per customer: registered clusters + their connection state + billing + team
- **Event/metadata bus** — receive events от customer agents (status updates, audit events, deploy notifications)
- **Connection multiplexer** — establish и hold long-lived connections от agents к нам

**КЛЮЧЕВОЕ ОТЛИЧИЕ от Managed Operations:** мы получаем **metadata от customer cluster через outbound agent connection**, не делаем reverse-direction API calls в customer's kubeapi с сохранённым kubeconfig. Customer cluster autonomous.

**Acceptance:** customer paste's registration token в Account UI → agent в их кластере connects → их cluster appears в Backstage list → status updates streaming live → revoke token → graceful disconnect.

**Размер:** M (~3-4 нед FT)

**Risks:**
- Hosted services compromise → potential man-in-the-middle для MCP operations (но **не** для customer data — она в их кластере)
- Agent reliability — flaky connection = customer sees stale state. Mitigation: agent reconnect logic + status cache

### 3.2 `apprafter-agent` + cluster registration

**Что:** новая компонента в customer's `apprafter` operator — `apprafter-agent` pod/sidecar который establishes outbound connection к нашему hosted bus.

**Flow:**
1. Customer создаёт Hetzner Cloud project, issues SSH key, sets API token env var
2. Customer runs `apprafter init --provider hetzner-cloud --tier solo --region nbg1` → standard OSS flow
3. Customer runs `apprafter apply` + `cluster-bootstrap` → standard OSS flow, working cluster
4. Customer signs up на нашем Account UI → gets registration token
5. Customer runs `apprafter cluster register --token <token>` → agent deployed в их cluster, connects к нашему hub
6. Cluster appears в их Backstage / Account UI / MCP server scope

**Customer полностью контролирует steps 1-3** — это OSS flow без managed dependency. Steps 4-6 add managed services layer.

**Acceptance:** customer goes from «pasted token» → «cluster live в Account UI» за <2 минут (assuming OSS cluster уже работает).

**Размер:** S-M (~2-3 нед FT — agent code + registration handshake + state sync)

**Note:** agent code будет жить в OSS repo (FSL-1.1-MIT) — без него customer cluster fully functional. Это enforce'ит «можешь отключить нас в любой момент» promise.

### 3.2a Customer offboarding (structural killer feature #2 на launch)

**Что:** customer clicks «cancel» в Account UI:
1. Registration token revoked на нашей стороне
2. Agent в customer cluster получает disconnect signal
3. Customer's `apprafter` operator continues running OSS-only
4. Email с инструкциями «вот как продолжать через OSS Backstage если хотите self-host portal» + ссылка на docs

**Что customer теряет:** hosted Backstage UI, hosted MCP server, notifications service, Account UI billing view.
**Что customer сохраняет:** **весь кластер**, все applications, все данные, OSS Backstage option (если они захотят self-host UI), `apprafter` CLI для управления.

**Acceptance:** cancel → access revoked → OSS-кластер продолжает обслуживать traffic без любой interruption → их Hetzner billing продолжается, наш subscription fee останавливается.

**Это закрывает killer feature #2 (Open Core + Export to Self-host) полностью на launch** — даже сильнее Phase 4 Export CLI, потому что **никакой миграции не требуется** (customer cluster уже autonomous).

**Размер:** XS (~0.5 нед FT — revoke logic + email template)

### 3.3 Hosted Backstage с нашими плагинами

**Что:** **namespace-per-customer Backstage** в нашем host-кластере + per-customer subdomain `<customer>.apprafter.dev`. Каждый customer — отдельный Backstage deployment в своём namespace с собственным Ingress на `<customer>.apprafter.dev`.

**Multi-tenancy через k8s namespace isolation:**
- Customer A → namespace `customer-a` → Backstage pods + Postgres + plugins
- Customer B → namespace `customer-b` → separate Backstage pods
- Ingress / external-dns маршрутизирует `<customer>.apprafter.dev` → правильный namespace
- Auth — простой: Account UI SSO на subdomain, no cross-tenant complexity

**Преимущества этого подхода:**
- Backstage не имеет built-in multi-tenancy → namespace isolation решает это **простой стандартной k8s pattern'ой**
- Plugin updates можно rollout постепенно по customers (canary deploys per namespace)
- Resource isolation: noisy customer не affects других (k8s resource limits)
- Customer cancel → drop namespace, всё чисто

**Tradeoff:** N customer Backstages = N pods. Не super efficient для resource utilization, но **для launch-scale (десятки клиентов) acceptable**. Optimization (multi-tenant Backstage с shared infra) — post-launch when traction valid'ирует investment.

**Spec §7 #6 reminder:** Backstage stays for v1.0. Custom portal — post-1.0 option если Lisa UX feedback signal негативный.

**Acceptance:** customer signup → namespace creates → Backstage deploys → `<customer>.apprafter.dev` resolves → SSO redirects → их зарегистрированный cluster appears с deploys и status.

**Размер:** M (~2-4 нед FT — уменьшилось vs original 3-5 потому что namespace-per-customer pattern проще чем shared multi-tenant Backstage с auth gymnastics. Backstage плагины сами — уже в `plan.md` 1.10 + 2.16, не дополнительная работа здесь)

### 3.4 MCP server (hosted endpoint + customer agent passthrough)

**Что:** наш hosted MCP endpoint. AI клиент (Cursor/Claude) connects к `mcp.apprafter.app` с token. Запрос proxies через `apprafter-agent` в нужный customer cluster.

Tools per `MANAGED_STRATEGY.md` §8.2 risk taxonomy:
- **Safe** (без ограничений): `list_apps`, `get_logs`, `get_status`, `get_metrics`
- **Reversible write** (audit log): `scale_app`, `restart_app`, `redeploy_app`
- **Bounded write** (heuristic + approval): `create_dev_env`, `deploy_to_staging`, `add_secret_to_dev`
- **Destructive** (через gate, см. §3.5): `delete_app`, `drop_claim`, `change_tier`

MCP token derivation per `MANAGED_STRATEGY.md` §8.5 — derived из customer Account UI auth (на launch упрощённо; full AccessGrant integration post-launch).

**Acceptance:** Cursor с MCP token → `list_apps` returns customer's apps в их зарегистрированном cluster → `scale_app` reaches агента → выполняется в их кластере → audit log на нашей стороне записывает.

**Размер:** M (~2-3 нед FT)

### 3.5 Destructive-op gate (via real MigrationPlan CRD)

**Что:** managed control plane hooks MCP/portal writes к **real MigrationPlan CRD primitive** (landed в OSS-core per §2.1 — PlatformController + MigrationPlan condensed). API middleware:
1. Classifies operation per `MANAGED_STRATEGY.md` §8.2 risk taxonomy
2. Safe/reversible → execute directly with audit log
3. Bounded write → check heuristics (blast radius, time window, rate limit, diff size, environment-aware), then either execute or escalate
4. Destructive → **create MigrationPlan CRD instance** в customer cluster через agent → pending approval

Heuristics для bounded writes per `MANAGED_STRATEGY.md` §8.3:
- Blast radius (refuse если > N apps)
- Time-window (вне рабочих часов heuristic ужесточается)
- Rate limit per MCP session
- Diff size (refuse «переделай всё разом»)
- Environment-aware (prod порог выше dev)

**Approval flow на launch — CLI + Argo CD approve/reject кнопки:**
- Customer runs `apprafter migration list` → видит pending
- `apprafter migration approve <id>` или `migration reject <id>`
- Argo CD UI exposes approve/reject Resource Actions для MigrationPlan на launch alongside the CLI — customer может approve/reject прямо из Argo CD интерфейса без CLI
- MigrationPlan CRD status updates → MigrationController в customer's cluster executes or aborts
- Email notification от Account UI о pending migrations (через transactional email, не Notifications service)

**Backstage MigrationPlan plugin остаётся post-launch (first bundle)** — bundled с `apprafter migrate-to-tier` CLI (см. §6 post-launch first bundle). На launch approvals закрыты через CLI + Argo CD approve/reject кнопки.

**Closes** killer feature #3 integrity (agentic safety) и #7 (MigrationPlan primitive) полностью через real CRD. AI agent through MCP не может bypass — CRD admission webhook enforces classification.

**Acceptance:** AI agent через MCP пытается `delete_app prod` → managed API creates MigrationPlan CRD в customer cluster → email notification → customer runs `apprafter migration approve <id>` → MigrationController executes → audit log via k8s events.

**Размер:** S (~0.5-1 нед FT — теперь thinner потому что MigrationPlan CRD primitive уже landed в OSS-core. Managed-side это просто hooking MCP/portal writes к CRD creation + classification)

### 3.6 Subdomain delegation для customer apps

**Что:** customer получает целый subdomain tree `*.<customer>.apprafter.dev` который **направлен на их cluster** через DNS NS delegation. Customer's `external-dns` (managed внутри их кластера, OSS-side per pulled-up 4.4a) создаёт A/AAAA records внутри этой зоны.

**Flow:**
1. Customer signup → namespace `<customer>` создан → DNS zone `<customer>.apprafter.dev` создаётся в нашей managed DNS (Hetzner DNS / Cloudflare / etc.)
2. NS records этой зоны делегируют на customer cluster's external-dns endpoint (через DNSZone CRD)
3. Customer deploys app `parser` → `external-dns` создаёт `parser.<customer>.apprafter.dev` → cert-manager в их cluster issues TLS
4. App доступен на `parser.<customer>.apprafter.dev` с TLS через ~60 секунд

**Custom domains — optional, opt-in:**
- Customer хочет `parser.acme-corp.com` вместо нашего subdomain → они настраивают их DNS provider sами (CNAME → их cluster Gateway endpoint)
- Мы даём инструкции / docs / возможно validation helper в Account UI («введите домен, мы покажем какой CNAME поставить»)
- Не наша responsibility поддерживать N DNS providers — стандартный external-dns workflow

**Backstage subdomain отдельно:** `<customer>.apprafter.dev` (без wildcard) → наш host-кластер → namespace customer's Backstage. То есть split:
- `<customer>.apprafter.dev` → нам, Backstage
- `*.<customer>.apprafter.dev` (everything else) → их cluster, apps

**Acceptance:** customer signup → namespace + DNS zone delegated automatically → app deploys → app URL доступен с TLS без manual DNS configuration.

**Размер:** S (~1 нед FT — наш part: zone provisioning automation + NS delegation + wildcard cert split. External-dns в customer cluster — уже OSS-core pulled-up 4.4a, не дополнительная работа здесь)

### 3.7 Stripe subscription + 14-day trial (no free tier)

**Per `PRICING_AND_LAUNCH_NOTES.md` §2:**

| Линия | Price |
|---|---|
| **OSS self-host** | €0 (но без managed UI/MCP) |
| **Hosted Services** | **€10/mo per cluster** |
| Annual | Monthly × 10 («save 2 months») = €100/year per cluster |
| Trial | 14 дней без cc, once per account |

**Что покрывает €10/mo:**
- Hosted Backstage с плагинами
- MCP server endpoint
- Account UI (billing, team, multi-cluster view)
- Notifications service (hosted)
- 90-day audit retention
- SSO
- Template DPA

**Что НЕ покрывает (customer pays themselves):**
- Hetzner compute (~€4.49 cpx22 → €24+ Tier 2 nodes → etc.)
- Customer's domain + DNS
- Customer's email / external integrations

**Prepaid model** per `PRICING_AND_LAUNCH_NOTES.md` §3.4. **Нет metered usage**, **нет free tier** (OSS играет эту роль).

**Trial scope:** 14 days full Hosted Services access без credit card. **Once per account** — abuse vector mitigation. После trial — credit card обязателен или access expires. Per `PRICING_AND_LAUNCH_NOTES.md` Section 1 принцип: «Hosted free tier — money sink без conversion path».

**Acceptance:** customer signup без cc → 14-day trial activated → trial expiry → access expires (cluster продолжает работать как OSS, но без hosted UI/MCP) → customer adds card → access resumes.

**Размер:** S (~1-2 нед FT — Stripe subscription + trial period logic + access gating. Прямолинейно потому что нет metered billing, нет free tier limits, нет abuse infrastructure)

### 3.8 Migration helpers (light — не Product 1 full)

**Что:**
- Documented Supabase → AppRafter migration: PG dump + restore + connection string rewrite (~1 нед FT)
- Documented Railway → AppRafter migration: env vars import + Dockerfile detection (~0.5 нед FT)

**Не Product 1 full** — это Phase 4-5 (bucket C post-launch). Light версия достаточна для launch claim «migrate from Supabase в один день».

**Acceptance:** customer запускает migration command → PG schema + data копируется в managed PG → connection strings обновляются → app deployed.

**Размер:** S (~1-2 нед FT for basic Supabase scenario)

### 3.9 ~~Free tier abuse prevention~~ — DROPPED (no free tier, no compute responsibility)

**Why dropped в Hosted Services tier:**
1. **No free tier** на managed — OSS plays the free role per `PRICING_AND_LAUNCH_NOTES.md` §1
2. **14-day trial без cc** — abuse window mitigation: max 14 days × N email accounts; once-per-account check prevents serial abuse
3. **Compute не наш** — crypto mining etc. — это customer's Hetzner account problem, Hetzner abuse process handles это
4. **Hosted services usage минимален per customer** — Backstage views, MCP calls. Rate-limit-able через standard middleware (~1 day work, included в §3.1), не отдельная subphase

**Re-activate trigger:** Tier 3 Turnkey Cloud landing (post-launch, post-PMF) — когда compute on our account, abuse prevention становится критично.

**Размер:** DROPPED

### 3.10 Internal customer support tooling

**Что:** internal CLI/UI для нашего team — list customers, inspect cluster, escalation flow. С audit log per access.

**Acceptance:** support engineer отвечает customer ticket с context.

**Размер:** S (~1-2 нед FT)

### 3.11 Velocity estimate — managed-specific (Hosted Services launch)

| Block | Estimate (нед FT) | Note |
|---|---|---|
| 3.1 Hosted multi-tenant SaaS scaffolding | 3-4 | Auth + customer registry + agent connection bus |
| 3.2 `apprafter-agent` + registration | 2-3 | Agent code + handshake + state sync |
| 3.2a Customer offboarding (revoke только) | 0.5 | Cluster уже автономен, нет self-host kit shipping |
| 3.3 Hosted Backstage (namespace-per-customer + `<customer>.apprafter.dev`) | 2-4 | Standard k8s namespace isolation, Backstage plugins сами уже в `plan.md` |
| 3.4 MCP server (hosted endpoint + agent passthrough) | 2-3 | |
| 3.5 Destructive-op gate (uses real MigrationPlan CRD) | 0.5-1 | Thinner: hooking writes → MigrationPlan creation. Primitive landed в OSS-core |
| 3.6 Subdomain delegation `*.<customer>.apprafter.dev` | 1 | Zone provisioning + NS delegation + wildcard cert split |
| 3.7 Stripe subscription + 14-day trial | 1-2 | No metered usage, no free tier |
| 3.8 Migration helpers (Supabase basic) | 1-2 | |
| ~~3.9 Abuse prevention~~ | DROPPED | Hetzner's responsibility |
| 3.10 Internal support tooling | 1-2 | |
| **Optional: soft wrapper `curl \| sh` orchestrator** | +0.5-1 | §7.6 mitigation, не launch-blocker |
| **Managed-specific total** | **13-22.5 нед FT** | (without optional wrapper) |
| **+ optional wrapper** | **13.5-23.5 нед FT** | |

---

## 4. Phasing / ordering

### 4.1 Sequential mode (solo development, per memory)

Per memory: user работает sequentially through tracks, parallel confuses + risks dropped items.

```
OSS-core (13.5-17 нед FT) → Managed-specific (13-22.5 нед FT) → Launch readiness
                       ▲
                       └─ Decision point: ADRs §5.5, §5.6 resolved здесь
```

**Sequential total: 26.5-39.5 нед FT ≈ 6.5-9.5 месяцев календарно**

(С optional soft wrapper orchestrator: +0.5-1 нед → 27-40.5 нед FT.)

**Post-launch first bundle (~2-4 недели после launch, separate PR-волна):**
- `apprafter migrate-to-tier --to team` CLI: ~1-1.5 нед FT
- Backstage MigrationPlan plugin (4.16): ~0.5-1 нед FT
- Bundle total: ~1.5-2.5 нед FT

### 4.2 Order within OSS-core

1. M1.5 Track B subset (in progress, ~50% done) — ADR 0025 + 0028 + 0029
2. **PlatformController + MigrationPlan CRD** (embedded в apprafter-operator, condensed 1.72-1.78 implementation) — closes #3 + #7 killer features
3. Phase 2 minimum — 2.1, 2.2, 2.3, 2.4, **2.6 redis**, 2.10, 2.11, 2.12
4. **T2 substrate** — 3.1 HA-bootstrap + 3.3 Cilium mTLS
5. Pulled Phase 4 + obs minimal — 4.1, 4.1a, 4.4a, 4.12, 3.7a subset, 3.4 subset

### 4.3 Order within managed-specific

Внутри managed-track ordering определяется dependencies + customer-facing readiness:

1. **Hosted SaaS foundation:** 3.1 hosted multi-tenant scaffolding → 3.2 agent + registration → 3.2a offboarding
2. **Surface pass:** 3.3 Hosted Backstage + 3.6 subdomain delegation (parallel within pass)
3. **API pass:** 3.4 MCP server + 3.5 destructive-op gate (теперь thinner — uses MigrationPlan CRD)
4. **Commerce pass:** 3.7 Stripe subscription + 14-day trial
5. **Onboarding polish:** 3.8 migration helpers + 3.10 internal support
6. Polish + soft launch (closed beta → invite waves → public)

### 4.4 Sequential reality check

Sequential 6.5-9.5 месяцев календарно — это **на 5-9 месяцев меньше** чем full plan to Phase 4 closure (~14-18 месяцев). Trade-off acceptable.

Если в какой-то момент появится contributor / future team member — managed-track перевести в параллель к остаткам OSS-core, сэкономив ещё ~3-4 месяца. Но это **планирование на ситуацию которая может не наступить** — speedrun считается solo.

---

## 5. Open decisions требующие resolve перед start managed-track

### 5.1 ~~ADR: custom portal vs Backstage~~ — RESOLVED via spec §7 #6

**Status:** spec §7 open question #6 уже резолвлен: «Stay on Backstage for v1.0».
**Speedrun choice:** Hosted Backstage с custom плагинами per spec. Custom portal на Bun+Svelte/OneBun — **post-1.0 option** если Lisa-feedback на Backstage UX окажется слабым.
**My previous draft был wrong** про custom portal на launch — это противоречит spec и `PRICING_AND_LAUNCH_NOTES.md` §2 («Hosted Backstage portal с нашими плагинами»).
**Action:** ничего. Спека уже зафиксировала.

### 5.2 ~~ADR: namespace-per-customer + Capsule vs Kamaji~~ — N/A в Hosted Services

**Status:** **resolved by tier choice (§0.5)**. Hosted Services tier = customer cluster полностью autonomous на customer's Hetzner. Multi-tenancy isolation вопрос не возникает.

**Re-opens когда:** Tier 3 Turnkey Cloud landит post-launch — там мы держим многих customers' compute, hard mt vs soft mt становится релевантен.

### 5.3 Pricing — fixed per `PRICING_AND_LAUNCH_NOTES.md` §2

**No open question — values уже зафиксированы:**

| Линия | Цена |
|---|---|
| OSS self-host | €0 |
| Hosted Services | €10/mo per cluster |
| Trial | 14 дней без cc, once per account |
| Annual | Monthly × 10 («save 2 months») = €100/year per cluster |

**No free tier.** No per-user pricing. No per-workspace base fee. **Prepaid model.**

**Phase 4 measure (§9 `PRICING_AND_LAUNCH_NOTES.md`):**
- COGS per cluster (actual cost)
- Trial-to-paid conversion rate
- Annual vs monthly mix

**Action:** ничего pre-launch. Reassess после первых 10-50 paying customers.

### 5.4 Sub-processors disclosure — minimal в Hosted Services

**Status:** dramatically simpler в Hosted Services tier (vs Managed Operations / Turnkey). Мы **не sub-processor для compute**, **не sub-processor для customer data plane** (она в customer's cluster, мы её не видим per Minimal Data Exposure architecture).

**Что в disclosure включаем:**
- Stripe (payments processor для subscription fee)
- Email provider (Postmark/Sendgrid для transactional email)
- Sentry/PostHog (если используются для error tracking / product analytics — disclose explicitly)
- (Optional) DNS provider для `apprafter.app` parent domain

**Что НЕ включаем (customer's relationships, not ours):**
- Hetzner (customer has direct relationship)
- Customer's domain DNS

**Action:** draft перед public launch. ~1 день — самая лёгкая disclosure из трёх tiers.

### 5.5 Managed control plane infrastructure decisions

- **Domain:** `apprafter.dev` (existing) или новый `apprafter.app` для managed services? `PRICING_AND_LAUNCH_NOTES.md` использует `apprafter.app` — likely уже decided
- **Где хостить:** **dogfooding plan — наш host = AppRafter на собственном Hetzner account** (Tier 2 substrate теперь в OSS-core). Account UI backend → k8s API → spawns customer namespaces as standard AppRafter Application instances. Marketing claim «we host AppRafter on AppRafter». customer's tokens мы НЕ держим, KMS choice не критичен — но host security всё ещё важен для customer metadata
- **Backup и DR plan для control plane itself** — important, потому что control plane outage = customers потеряют UI/MCP access (clusters сами продолжают работать, но это плохой experience). Через standard 4.12 backups к external S3
- **Rescue cluster setup (#22 killer feature)** — отдельный кластер для emergency host access к нашему host-кластеру. Это **dogfooding signal** для marketing — «у нас тоже рассе есть»

**Action:** ADR before 3.1 starts. ~2-3 дня.

### 5.6 ADR для `apprafter-agent` protocol

**Что:** какой протокол использовать для agent → hosted bus connection?
- Options: gRPC streaming / WebSocket / NATS client connecting к нашему hosted JetStream
- NATS is natural fit (kine+NATS in OSS roadmap, audit log story works через тот же transport)
- gRPC более standard для k8s ecosystem (Headlamp-style agent communication)

**Implications:**
- Choice влияет на 3.1 hosted scaffolding shape
- Influences killer feature #21 narrative (replayable audit log) — NATS-based agent natively integrates
- Recommend: **gRPC streaming на launch** (standard, well-supported, no extra infra). NATS transition можно сделать при kine+NATS migration post-launch

**Action:** ADR before 3.1 starts. ~1-2 дня.

### 5.7 ADR для Kamaji opt-in spec deviation

**Status:** spec §4.1 говорит «Tier 2: Hard multi-tenancy via Kamaji default». Speedrun отклоняется — **Tier 2 = HA substrate только, Kamaji opt-in feature через `PlatformStack.spec.values.multitenancy: true`**, default off.

**Reasoning:**
- Большинство Tier 2 customers на launch — solo team, не MSP. Kamaji overkill
- Kamaji landing = 2-3 нед FT с distributed systems complexity. Откладывание saves ~2-3 нед
- Multi-tenancy via standard k8s namespaces достаточно для single-org Tier 2 customer
- Hard mt становится default только на Tier 3+ где compliance/scale demand actually есть

**Re-activate trigger:** first MSP customer signal (Андрей-A persona) или multi-org customer ask hard isolation.

**Action:** write ADR. ~1 день. **Spec.md §4.1 update required перед launch announcement.**

---

## 6. Post-launch backlog (recap §2.3)

Draft priority — re-iterate на 10-customer mark когда реальные signals доступны:

1. **`apprafter migrate-to-tier --to team` CLI + Backstage MigrationPlan plugin bundle** (~1.5-2.5 нед FT) — **first post-launch deliverable**, lands ~2-4 недели после launch, closes #1 killer feature fully + delivers Backstage UI для approvals
2. SPIRE + OpenBao migration (Tier 2 security depth) — `plan.md` 2.7, 2.8, 3.11, 3.12
3. Hubble→Backstage flow visualizer + full ClickHouse provider (obs depth для Маши) — `plan.md` 3.5, 3.7b
4. Kamaji + Tenant CRD opt-in (hard mt для scale, requires §5.7 ADR landed) — `plan.md` 3.8, 3.8a
5. AccessGrant + OIDC SSO (team features для Маши expansion) — `plan.md` 4.5-4.9
6. kine+NATS migration (closes #21 killer feature replayable audit log) — `plan.md` 3.2
7. `needs.jetstream` + Notifications service (event-driven workflows) — `plan.md` 2.5, 2.13-2.16
8. Build pipeline Trivy/Grype/SBOM + Build Report — `plan.md` 4.13, 4.14
9. Tier 3 trail (Talos/LINSTOR/Kata) — `plan.md` Phase 5
10. Confidential containers — `plan.md` Phase 6
11. Plugin ecosystem — `plan.md` Phase 7

---

## 7. Honest risks / known weaknesses этого спидрана

### 7.1 ~~«Tier 1 only на launch»~~ — RESOLVED. T1+T2 ready, T3+T4 на roadmap

T2 substrate pulled up в OSS-core. Customer может signup на T1 или T2 directly. T1→T2 migration в post-launch first bundle.

Marketing framing: «T1 single-node and T2 3-node HA available today. T1→T2 migration coming Q3 2026. T3 bare metal and T4 confidential containers on roadmap.»

### 7.2 `apprafter-agent` compromise risk

В Hosted Services модели **мы не держим customer credentials**, поэтому API token storage risk из предыдущей revision не применим. Но появляется **новый risk**:

**Agent в customer cluster имеет outbound trust к нашему hub.** Если agent compromised (supply-chain attack на наши binaries) → attacker может execute MCP-style operations против customer cluster через legitimate-looking agent channel.

**Mitigation layers:**
- Agent binaries signed via cosign (per OSS-core release pipeline)
- Agent permissions narrow по design — operates через AppRafter CRDs, не cluster-admin
- Customer can revoke registration token любое время → agent disconnects → cluster autonomous
- Audit log на agent operations виден customer'у через их Backstage (transparency)

**Severity:** Lower than Managed Operations API-token-holding (compromise там = compromise N customer Hetzner accounts). Здесь compromise = limited to operations через AppRafter abstraction layer.

### 7.3 ~~Custom portal vs Backstage~~ — N/A

Spec §7 #6 уже резолвлен в favour of Backstage на v1.0. Мой previous draft про custom portal был wrong. Post-1.0 re-evaluation если Lisa-feedback signal негативный — отдельная decision.

### 7.4 ~~MCP killer feature без full agentic safety~~ — RESOLVED

PlatformController + MigrationPlan CRD теперь в OSS-core (§2.1). Real CRD-based destructive-op gate works через MigrationController в customer cluster. Approvals через CLI на launch, Backstage plugin в post-launch first bundle.

Closes killer feature #3 (MCP agentic safety) + #7 (MigrationPlan primitive) **fully**.

Remaining gap: LLM-reviewer для MigrationPlan approval — Phase 4+ feature, не critical для launch.

### 7.5 Notifications service drop — confirmed для launch

Hosted notifications service в `MANAGED_STRATEGY.md` §3.1 listed as part of Hosted Services. Но **scope hosted notifications — это AccessGrant lifecycle emails** (magic link, expiry warnings, revocation notifications per `spec.md` §3.4).

AccessGrant CRD сам **в bucket C post-launch** (см. §2.3 — `plan.md` 4.5/4.5a, trigger condition «team-of-3+ customer signal»). Без AccessGrant — hosted notifications service на launch **не нужен**.

**Что remains hosted на launch:**
- Transactional emails (signup confirmation, billing receipts, trial expiry warning) — через standard email provider direct (Postmark/Sendgrid), не через AppRafter notifications service abstraction

**Когда landит hosted notifications:** одновременно с AccessGrant landing в post-launch backlog #5.

### 7.6 «Bring your own Hetzner» — onboarding journey

Lisa-class customer проходит **4 шага** (по `apprafter` infrastructure после M1.5 Track A `bootstrap-all` orchestrator):

1. **Hetzner setup:** signup / login → create project → issue API token
2. **Install `apprafter` CLI:** одна команда (`curl | sh` / brew / cargo install)
3. **Bootstrap cluster:** `apprafter target add` + `apprafter bootstrap-all` (M1.5 Track A.9 это уже **один orchestrated flow**, не отдельные команды)
4. **Connect to managed:** signup на Account UI → login из CLI → register cluster (token-based)

Сравнение: Vercel signup → deploy ≈ 3 шага. Наш path = 4. **Acceptable если Account UI signup walkthrough хороший.**

**Mitigations уже встроенные:**
- `apprafter bootstrap-all` orchestrator (M1.5 Track A.9 ✅ closed) — закрывает step 3 в одну команду с staged output
- `apprafter doctor` (Track A.7 ✅) — preflight checks если что-то не так
- miette diagnostics (Track A.10 ✅) — clear error messages

**Mitigations для launch:**
- **Account UI onboarding walkthrough — primary mitigation.** При signup показываем все 4 шага со screenshots, copy-pasteable commands, real-time status check («connection from your CLI detected ✓»). Это **наша core UX work** в §3.3 Backstage / Account UI scope
- llms.txt + AI-friendly docs (per `PRELAUNCH_CHECKLIST.md` §3.3) — AI агенты могут guide
- 90-second video walkthrough
- Affiliate link Hetzner signup (заработать % per `MANAGED_STRATEGY.md`)

**Soft wrapper option — если в beta friction окажется high:**

```bash
curl -fsSL https://apprafter.dev/launch | sh -- --account-token <managed-token>
```

Делает за customer:
1. Prompts Hetzner API token (still customer step — мы не получаем токен)
2. Installs CLI
3. Runs `target add` + `bootstrap-all`
4. Registers cluster в managed через `--account-token`

Это **~3-5 дней FT доп. работы** (соft wrapper + Account UI flow для генерации account-token). Не launch-blocker, но **сильный UX win** если первые beta customers стругают на шагах.

**Если onboarding friction systematic blocker в beta** — это сигнал перейти к Tier 2 (Managed Operations) для Lisa-segment быстрее чем Phase 4/4.5. Beta period для именно этого signal.

### 7.7 Solo execution risk

Sequential 6.5-9.5 месяцев — это **window для конкурентов** (Coolify добавляет MCP, Vercel/Railway добавляют agentic safety, новый entrant появляется). Mitigation: public roadmap + early developer mindshare через AI-discoverability (llms.txt, public presence per `PRELAUNCH_CHECKLIST.md` §3.3-3.4) даже до launch.

### 7.8 T1→T2 migration delay risk

T1→T2 migration deliverable отложен на post-launch first bundle (~2-4 недели после launch). Customer signing up на T1 в первые ~4 недели имеет **stuck path** к T2 без manual workarounds.

**Mitigations:**
- Marketing совершенно clear: «Sign up T1 if starting small, T2 if scale needed now. Migration coming Q3»
- Если customer оказывается blocked — support response: «create fresh T2 cluster, redeploy Application manifest, migrate data via `apprafter migrate-data` (через 3.8 helper) или PG dump/restore»
- Migration tool ETA публичен в roadmap
- Beta customers с этой потребностью могут получить hands-on помощь от founder в первые недели

**Severity:** Low if migration tool ships в planned ~4 weeks window. Higher если slippage до 2+ месяцев — может damage trust для early signups. **Strict timeline на post-launch first bundle важна.**

---

## 8. Changelog

- **2026-05-29 (revision 6)** — sync with the docs-actualization wave:
    - **2.6b needs.disk** added to launch bucket A (block-storage primitive alongside pg+redis).
    - **Argo CD approve/reject buttons** confirmed at launch for MigrationPlan approval (alongside CLI); Backstage plugin remains post-launch.
    - Managed strategy + tiers cemented in **ADR 0034–0038** (+ 0031 ratified); spec.md actualized to **Rev 8**; plan.md change-history extracted to `docs/changelog/plan-history.md`; speedrun buckets now annotated as `> 🏁 SR:` markers in plan.md.

- **2026-05-17 (revision 5)** — T2 substrate + full MigrationPlan primitive в OSS-core:
    - **T2 substrate pulled up** в bucket A: 3.1 HA-bootstrap (k3s 3-node + kube-vip + embedded etcd, **NOT kine+NATS**) + 3.3 Cilium mTLS. Total ~1-1.5 нед FT. Customer может signup на T1 или T2 directly
    - **PlatformController + MigrationPlan CRD condensed** в bucket A (M1.5 Track B 1.72-1.78 condensed implementation, ~1 нед FT embedded в apprafter-operator). Real CRD-based gate replaces soft API gate. Closes #3 + #7 killer features fully
    - **needs.redis pulled up** в bucket A (Phase 2.6). Closes 2 из 6 platform services. Dogfooded by Account UI session/rate limiting
    - **§3.5 destructive-op gate thinner** (~0.5-1 нед FT вместо 1-2) — managed-side это просто hooking writes → MigrationPlan CRD creation, primitive уже landed
    - **Approval flow split:** CLI approve на launch, Backstage MigrationPlan plugin bundled с migrate-to-tier в post-launch first bundle
    - **NEW §5.7 ADR**: Kamaji opt-in spec deviation (spec §4.1 says default on T2, мы делаем opt-in via `PlatformStack.spec.values.multitenancy: true`). Spec.md update required
    - **§5.5 dogfooding clarified:** наш host = AppRafter на собственном T2 substrate. Marketing claim «we host AppRafter on AppRafter»
    - **§5.6 protocol recommendation:** gRPC streaming на launch, NATS transition с kine+NATS migration post-launch
    - **§6 post-launch backlog #1**: `apprafter migrate-to-tier` CLI + Backstage MigrationPlan plugin bundle (~1.5-2.5 нед FT, lands 2-4 weeks после launch)
    - **§7.1 RESOLVED:** T1+T2 ready, не «T1 only»
    - **§7.4 RESOLVED:** real CRD-based agentic safety, не soft gate
    - **§7.8 NEW:** T1→T2 migration delay risk (first 4 weeks early customers без upgrade path)
    - **Killer features upgrade:** #1 partial → full ready, #3 basic → full CRD-based, #7 partial → full CRD, #9 1/6 → 2/6
    - **Velocity:** 26.5-39.5 нед FT sequential ≈ 6.5-9.5 месяцев календарно (was 24.5-37 / 6-9). Net +2-2.5 нед FT for **three killer features fully closed**

- **2026-05-17 (revision 4)** — refinements per user corrections:
    - **§0.5 + §7.6 onboarding journey:** 4 шага, не 7 (Hetzner setup → CLI install → `target add` + `bootstrap-all` orchestrator → managed signup + register). M1.5 Track A.9 уже закрыл steps 4-5 предыдущей revision в один command
    - **§3.3 Hosted Backstage:** namespace-per-customer + `<customer>.apprafter.dev` subdomain. Standard k8s namespace isolation pattern вместо shared multi-tenant Backstage. Size 3-5 → 2-4 нед FT
    - **§3.6 Subdomain delegation:** целиком `*.<customer>.apprafter.dev` zone делегируется на customer cluster через DNS NS records. Custom domains opt-in с инструкциями. Cleaner separation: `<customer>.apprafter.dev` → Backstage у нас; `*.<customer>.apprafter.dev` → их apps
    - **§7.5 Notifications service** confirmed dropped для launch: hosted notifications scope = AccessGrant lifecycle, AccessGrant в bucket C post-launch
    - **§7.6 mitigation expanded:** Account UI signup walkthrough — primary mitigation. Soft wrapper `curl | sh` orchestrator — optional fallback (0.5-1 нед FT) если в beta friction blocking
    - **§3.11 velocity:** 13.5-23 нед FT (was 14.5-24). Net -1 нед FT через Backstage + subdomain simplifications
    - **§4.1 sequential total:** 24.5-37 нед FT ≈ 6-9 месяцев календарно

- **2026-05-17 (revision 3)** — Managed offering tier ladder correction:
    - Recognized 3-tier ladder per `MANAGED_STRATEGY.md` §3: Hosted Services / Managed Operations / Turnkey Cloud
    - **Hosted Services** (lightest) — launch tier. Previous revision incorrectly identified Managed Operations as launch.
    - 3.1-3.4 переписаны radically: hosted multi-tenant SaaS scaffolding + `apprafter-agent` для outbound connection vs «control plane operating customer clusters»
    - **3.2a simpler:** customer offboarding это просто revoke registration token — OSS-кластер уже автономен, нет необходимости в self-host kit shipping
    - **3.3 переосмыслен — Hosted Backstage**, не custom portal. Spec §7 #6 уже резолвлен в favour of Backstage, мой previous draft был wrong
    - **3.7 fixed pricing:** €10/mo per cluster, 14-day trial без cc once per account, NO free tier (OSS plays free role per `PRICING_AND_LAUNCH_NOTES.md` §1), prepaid only, annual = save 2 months
    - **5.1 RESOLVED** (custom vs Backstage) — spec already decided
    - **5.5 KMS не критичен** — мы не держим customer credentials в Hosted Services
    - **5.6 NEW:** ADR для `apprafter-agent` protocol (gRPC/WebSocket/NATS client)
    - **7.2 reframed:** agent compromise risk (lower severity than API token holding in Managed Operations)
    - **7.5 reconciled:** Notifications service в OSS plan (drop) ≠ Notifications service в Hosted Services (hosted, simplified version included)
    - **7.6 stronger:** «bring your own Hetzner» 7-step onboarding journey — largest friction point launch'а. OneClick-style `curl | sh` orchestrator mitigation добавлен
    - **Velocity:** 25.5-38 нед FT sequential ≈ 6-9 месяцев календарно (was 25.5-39 / 6-9.5). Marginal refinement, architecture cleaner
    - **Tier 2 / Tier 3** explicitly post-launch с trigger conditions

- **2026-05-17 (revision 2)** — Managed offering tier correction (superseded by revision 3):
    - Incorrectly identified Managed Operations as launch tier
    - 3.1 reframed as control plane operating N customer clusters via stored Hetzner API tokens
    - Risk model focused on API token compromise
    - Velocity 25.5-39 нед FT
    - Superseded once realized Hosted Services is the lighter starting tier

- **2026-05-17** — first draft from speedrun session. Решения:
    - SPIRE/Kamaji/OpenBao/kine+NATS → bucket C post-launch backlog (not drop) — это **advanced security/scale features**, не launch-critical
    - MigrationPlan split: soft gate в managed API на launch (closes #3 killer feature integrity), full CRD post-launch
    - OTel minimal + Hubble added back to OSS-core (cost-effective obs story, ~2 weeks FT)
    - Phase 2 reduced to `needs.pg` minimum (`needs.jetstream` drop, `needs.redis` defer, Notifications service drop)
    - Phase 3 mostly deferred с explicit trigger conditions
    - Phase 4 partial pull-up (HTTPRoute auto-gen, ExternalSurface, external-dns, backups)
    - Phase 5/6/7 deferred entirely
    - Velocity estimate: 31-45 нед FT sequential ≈ 7.5-11 месяцев календарно (superseded by revision 2)
    - Risk: window для конкурентов 7.5-11 месяцев — mitigated через early AI-discoverability presence
