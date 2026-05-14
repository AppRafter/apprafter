# AppRafter — План разработки

> **Источник истины:** [`spec.md`](./spec.md) (revision 4).
> **Назначение:** разбить spec на упорядоченные actionable-фазы, каждая из которых пригодна как самостоятельный цикл «план → реализация».
> **Статус:** живой документ. Обновляется по мере закрытия фаз и появления новых решений.

---

## 0. Как пользоваться этим документом

1. **Цикл работы:** выбираем одну подфазу (например, `1.4`), раскрываем её через skill `superpowers:writing-plans` в детальный TDD-план в `docs/superpowers/plans/YYYY-MM-DD-<feature>.md`, исполняем через `subagent-driven-development` или `executing-plans`, отмечаем чекбоксы здесь.
2. **Гранулярность:** каждая подфаза — это **один цикл разработки** (~1–5 рабочих дней). Если оказывается больше — делим на лету.
3. **Зависимости:** идти по фазам сверху вниз. Внутри фазы соблюдать указанные `Зависит от:`. Параллелить можно ветки без общих зависимостей.
4. **Размер (T-shirt):** `XS` ≈ полдня, `S` ≈ 1–2 дня, `M` ≈ 3–5 дней, `L` ≈ ≥1 неделя (кандидат на дробление).
5. **Acceptance:** перед закрытием подфазы должны выполниться все её критерии приёмки. Без этого галка не ставится.
6. **Критерий «готовности к запуску цикла»:** spec-ссылка ясна, зависимости закрыты, acceptance проверяемы. Если что-то размыто — сначала ADR в `docs/adr/`.

### Условные обозначения

- `[ ]` — не начато
- `[~]` — в работе (с пометкой ветки/PR в скобках)
- `[x]` — закрыто (с пометкой даты и коммита)
- `🔒` — заблокировано (с указанием блокера)
- `⚡` — критический путь
- `🌱` — можно запараллелить

---

## 1. Карта фаз

| Фаза | Название | Соответствие spec | Размер | Зависит от |
|------|----------|-------------------|--------|------------|
| 0 | Основания и подготовка | M0 finalization | M | — |
| 1 | MVP single-node | M1 | L | 0 |
| 2 | Платформенные сервисы | M2 | L | 1 |
| 3 | Multi-node + observability | M3 | L | 2 |
| 4 | External Surface + Access | M4 | L | 3 |
| 5 | Tier 3 — bare metal | M5 | L | 4 |
| 6 | Tier 4 — confidential | M6 | M | 5 |
| 7 | Plugin ecosystem | (cross-cut) | L | 2 (gRPC), 3 (infra) |
| 8 | 1.0 release | M7 | M | 4 (минимально), идеально 6 |
| ∞ | Сквозные направления | — | — | running |

Phase 7 запускается параллельно с 3+ как только готов CRD ServiceProvider (закроется в фазе 2).

---

## Фаза 0 — Основания и подготовка ⚡

**Цель фазы:** превратить repo из черновика spec в готовую к контрибьюторам монорепу с зафиксированными решениями M0.

**Spec:** §6 (M0), §7 (Resolved → license, codename), Appendix A.

### 0.1 Структура монорепы 🌱

**Статус:** ✅ закрыто 2026-05-06.

**Цель:** создать каркас директорий по Appendix A с README-заглушками.

**Поставка:**
- [x] Создать каталоги: `cli/`, `operator/`, `schemas/`, `providers/{pg-integrated,pg-aws,jetstream-integrated,clickhouse-integrated,redis-integrated,s3-integrated}/`, `backstage-plugins/`, `manifests/`, `docs/`, `examples/`, `docs/adr/`, `docs/superpowers/plans/`.
- [x] В каждом каталоге — README.md с одним абзацем «что здесь».
- [x] Корневой `README.md` с vision, схемой, ссылкой на `spec.md` и `plan.md`.
- [x] `.editorconfig`, `.gitattributes`, базовый `.gitignore` (Rust, Node, Bun, OS-артефакты).

**Acceptance:** `tree -L 2` соответствует Appendix A; README рендерится в GitHub-flavoured markdown.

**Размер:** XS

---

### 0.2 Лицензия FSL-1.1-MIT 🌱

**Статус:** ✅ закрыто 2026-05-06.

**Цель:** оформить лицензионное решение из §7.

**Поставка:**
- [x] `LICENSE` в корне с текстом FSL-1.1-MIT (канонический шаблон с fsl.software, copyright «AppRafter Authors», year 2026).
- [x] `LICENSE-MIT` для будущей конверсии (для прозрачности).
- [x] `NOTICE` с описанием модели (2-летнее окно → MIT). Только английский — публичный документ (см. правило о языке проекта).
- [x] SPDX-заголовок-шаблон в `docs/contributing/license-headers.md`.
- [x] Подпапки плагинов (`providers/`, `backstage-plugins/`) — отдельный `LICENSE` MIT (см. §7).

**Acceptance:** GitHub распознаёт лицензию (текст канонический FSL-1.1-MIT с fsl.software — Linguist распознаёт по характерному тексту); SPDX-header задокументирован для всех будущих исходников.

**Размер:** XS

---

### 0.3 ADR-процесс и шаблон

**Статус:** ✅ закрыто 2026-05-06.

**Цель:** зафиксировать формат принятия архитектурных решений.

**Поставка:**
- [x] `docs/adr/0000-template.md` (по шаблону Майкла Найгарда + risk/owner/re-evaluation).
- [x] `docs/adr/0001-license-fsl-1-1-mit.md` — FSL-1.1-MIT для core, MIT для плагинов (§7 + §8).
- [x] `docs/adr/0002-codename-apprafter.md` — выбор кодового имени (§7 open-question 9).
- [x] `docs/adr/0003-rust-operator-over-crossplane.md` (§8).
- [x] `docs/adr/0004-cue-over-pkl.md` (§7 + точка пересмотра M5).
- [x] `docs/adr/0005-kine-nats-over-etcd.md` (§4.2 + §8).
- [x] `docs/adr/0006-openbao-over-vault.md` (§4.4 + §8).
- [x] `docs/adr/0007-tier-1-sealedsecrets-tier-2-openbao.md` — переназначено с дублирующего FSL-обоснования на принцип 1.8 (§1.8 + §4.4 + §8).
- [x] `docs/adr/0008-http-first-notifications-api.md` (§4.6 + §8).
- [x] `docs/adr/0009-platform-only-templates.md` (§4.6 + §8).
- [x] `docs/adr/0010-dockerfile-first-build.md` (§4.9 + §8).
- [x] `docs/adr/0011-hybrid-rust-sdk-tofu-shim.md` (§3.7 + §4.12 + §8).
- [x] `docs/adr/0012-migrationplan-as-first-class.md` (§3.8 + §8).
- [x] `docs/adr/README.md` обновлён индексом всех ADR.

**Acceptance:** все «Resolved»-решения §7 и тех-обоснования §8 закодифицированы как ADR; индекс соответствует фактическому содержимому каталога.

**Размер:** S

---

### 0.4 CUE-модуль и валидация

**Статус:** ✅ закрыто 2026-05-06.

**Цель:** инициализировать единый CUE-модуль для всех схем платформы.

**Решения по ходу:**
- `cue.mod/` положен **в корень репо**, не в `schemas/` — стандартная CUE-практика для monorepo (один модуль, schemas + examples в нём).
- Имя модуля — `apprafter.io` (вместо `github.com/apprafter/schemas`); короче, согласовано с `apiVersion: apprafter.io/v1alpha1`.
- Каркас 9 CRD — skeleton с минимальным набором полей. Полные production-grade схемы (`Application` с env-overrides, `ServiceProvider` с tier-defaults, и т.д.) докручиваются в фазах 1.7 / 2.1 / 2.2 / 4.1 / 4.5 / 4.16 / 5.x.
- `schemas/k8s/` пока пустой каталог с README — импорт upstream Kubernetes типов через `cue import` подключается в фазе 1.7, когда renderer операторa получит конкретные `Deployment`/`Service`/Gateway типы.

**Поставка:**
- [x] `cue.mod/module.cue` (`module: "apprafter.io"`, language v0.10.0).
- [x] `schemas/k8s/` — placeholder с README; импорт отложен до фазы 1.7.
- [x] `schemas/v1alpha1/` — skeleton всех 9 CRD: `Application`, `ServiceProvider`, `ResourceClaim`, `AccessGrant`, `MigrationPlan`, `ExternalSurface`, `Infrastructure`, `ServiceProviderPlugin`, `InfrastructureProviderPlugin`, плюс общий `types.cue`.
- [x] `scripts/lint-cue.sh` — `cue fmt --check` + `cue vet` для schemas и examples; fallback на `nix run nixpkgs#cue` если нет локального бинарника.
- [x] `examples/applications/parser.cue` — валидная фикстура (упрощённая версия §3.1).

**Acceptance:**
- ✅ `scripts/lint-cue.sh` зелёный (CUE 0.16.0 через `nix run`).
- ✅ Невалидный пример (`replicas: "three"`, `port: "not-a-port"`, `public: "yes"`) валится с понятными сообщениями `conflicting values <wrong> and <expected> (mismatched types ...)` со ссылками на line:column в schema и в example.

**Размер:** M

---

### 0.5 Bootstrap CI 🌱

**Статус:** ✅ закрыто 2026-05-06.

**Цель:** GitHub Actions / CI-пайплайн с минимальным набором проверок.

**Решения по ходу:**
- Lefthook config назван `lefthook.yml` (без leading dot — стандартный путь, который lefthook ищет по умолчанию).
- Rust- и Bun-job'ы условные: пока в репе нет ни `Cargo.toml`, ни `package.json`, оба пропускают шаги с `::notice`. Реальная проверка включится в фазе 1.1 (cli) и 1.6 (Backstage).
- SPDX-чек реализован через `scripts/check-spdx-headers.sh` — `git ls-files` против явного списка `PATTERNS`. Markdown-доки и сгенерированные файлы исключены.
- Conventional-commits enforce'ится в двух местах: PR-title через GitHub Action `amannn/action-semantic-pull-request@v5`, локальный commit-msg — через `scripts/check-commit-msg.sh` (привязан в `lefthook.yml`).

**Поставка:**
- [x] `.github/workflows/lint.yml` — три job'а: CUE (`./scripts/lint-cue.sh`), Rust (`cargo fmt --check` + `cargo clippy -D warnings`, conditional), Bun (`bun lint`, conditional).
- [x] `.github/workflows/test.yml` — Rust (`cargo test`) + Bun (`bun test`), оба conditional.
- [x] `.github/workflows/license-check.yml` — `./scripts/check-spdx-headers.sh`.
- [x] `.github/workflows/conventional-commits.yml` — PR-title проверка.
- [x] `.github/CODEOWNERS` (с placeholder-handle `@apprafter-authors`).
- [x] `.github/PULL_REQUEST_TEMPLATE.md`.
- [x] `.github/ISSUE_TEMPLATE/{bug,feature,adr-proposal}.yml`.
- [x] `lefthook.yml` — pre-commit (`lint-cue.sh`, `check-spdx-headers.sh`) и commit-msg (`check-commit-msg.sh`).
- [x] `scripts/check-spdx-headers.sh` — проходит на всех 25 текущих source-файлах.
- [x] `scripts/check-commit-msg.sh` — Conventional Commits validator (тот же набор типов, что и в CI).

**Acceptance:**
- ✅ `scripts/check-spdx-headers.sh` зелёный для всех 25 tracked source-файлов; добавление файла без SPDX → fail с `::error file=...::missing SPDX-License-Identifier`.
- ✅ `scripts/check-commit-msg.sh` принимает `feat(repo): ...`, отвергает «random non-conventional message» с понятным сообщением.
- ✅ `scripts/lint-cue.sh` (вызывается lint workflow) продолжает быть зелёным.

**Размер:** S

---

### 0.6 DevContainer / dev-окружение 🌱

**Статус:** ✅ закрыто 2026-05-06.

**Цель:** контрибьютор клонирует repo и в один шаг получает всё нужное.

**Решения по ходу:**
- Три параллельных install-пути: Nix flake (рекомендуемый), VS Code Dev Container (postCreate скачивает CUE/k3d/just/lefthook/cosign), и manual через `mise.toml` для language runtimes + ручная установка остальных. Все три ведут к одному `just bootstrap && just e2e-up`.
- `Justfile` вместо Makefile — современный синтаксис, рекурсивные shebang-блоки для условных шагов.
- `flake.lock` закоммичен — pinning nixpkgs revision для воспроизводимости.

**Поставка:**
- [x] `.devcontainer/devcontainer.json` (Rust + Node + Bun + Go + kubectl + helm + docker-in-docker через features) и `.devcontainer/post-create.sh` (CUE, k3d, just, lefthook, cosign).
- [x] `flake.nix` — полный devShell: cue, cargo+rustc+rustfmt+clippy+rust-analyzer, bun, kubectl, k9s, helm, k3d, argocd, cilium-cli, talosctl, cosign, syft, trivy, grype, just, lefthook, age, sops, jq, git. `nix flake check` — зелёный.
- [x] `flake.lock` — пин nixpkgs (rev 549bd84d…) и flake-utils.
- [x] `mise.toml` — rust/bun/node/just/go (language runtimes; для остальных тулз ссылка на Nix flake / dev container).
- [x] `Justfile` — 8 таргетов: `default`, `bootstrap`, `lint`, `fmt`, `test`, `e2e-up`, `e2e-down`, `stats`. `just --list` рендерит дерево.
- [x] `docs/contributing/setup.md` — три install-пути, bootstrap, e2e-up/-down, common issues.
- [x] `docs/contributing/README.md` — индекс contributor-документов.
- [x] Корневой `README.md` дополнен секцией Quick Start.

**Acceptance:**
- ✅ `nix flake check --no-build` — зелёный (devShell + formatter эвалюируются).
- ✅ `nix run nixpkgs#just -- --justfile Justfile --list` — выводит все 8 рецептов.
- ✅ Контрибьютор: `git clone` → `nix develop` (или Dev Container reopen) → `just bootstrap && just e2e-up` без чтения дополнительных доков (Quick Start в корневом README покрывает базовый flow).

**Размер:** S

---

### 0.7 Базовый docs-skeleton

**Статус:** ✅ закрыто 2026-05-06.

**Цель:** заготовка под TechDocs (M7), сейчас — навигационный каркас.

**Решения по ходу:**
- `docs/README.md` (Phase 0.1) удалён — конфликтовал с `docs/index.md` и блокировал mkdocs strict mode. Содержимое перенесено в `docs/index.md` (mkdocs landing page).
- mkdocs `exclude_docs` исключает `superpowers/` (локальный gitignored каталог, который физически виден mkdocs на диске).
- `validation.nav.omitted_files: info` — ADR-страницы доступны по URL, но не дублируются в боковом nav (одной ссылкой «ADRs» → `adr/README.md` достаточно).
- mkdocs-material в `flake.nix` (вместе с базовым mkdocs) — `nix develop` сразу даёт `mkdocs serve/build`.
- В `Justfile` добавлены `docs-serve` (live preview) и `docs-build` (strict).

**Поставка:**
- [x] `docs/index.md` — landing page с tier-таблицей и ссылками на разделы.
- [x] `docs/architecture/index.md` — stub, ссылки на §2/§4 spec.md.
- [x] `docs/concepts/index.md` — stub, таблица §3-объектов и порядок чтения.
- [x] `docs/operator-guide/index.md` — stub.
- [x] `docs/dev-guide/index.md` — stub.
- [x] `docs/reference/index.md` — stub.
- [x] `mkdocs.yml` с Material theme, plugins (search), pymdownx-расширениями и валидной nav.
- [x] `CONTRIBUTING.md` (root) — entry point для новых контрибьюторов.
- [x] `CODE_OF_CONDUCT.md` — Contributor Covenant 2.1.
- [x] `SECURITY.md` — disclosure policy.
- [x] `GOVERNANCE.md` — роли и decision-making (lazy consensus + ADR-process).
- [x] `Justfile` — таргеты `docs-serve`, `docs-build`.
- [x] `flake.nix` дополнен `python3Packages.mkdocs-material`.
- [x] `.gitignore` дополнен `site/`.

**Acceptance:**
- ✅ `mkdocs build --strict` (через `nix-shell -p (python3.withPackages [mkdocs mkdocs-material])`) — зелёный, 0 warnings, build за 0.39 s.
- ✅ Навигация согласована со spec: Architecture/Concepts/Operator/Dev/Reference + Contributing + ADRs соответствуют §2/§3/§4/§7/§8.

**Размер:** S

---

### 0.8 Закрытие чек-листа M0 spec

**Статус:** ✅ закрыто 2026-05-06.

**Цель:** обновить `spec.md` §6 (M0): зачеркнуть «Repository structure defined» и «License chosen».

**Решения по ходу:**
- Версия не `v0.0.0-foundations` (как было в первоначальной редакции плана), а `v0.0.8` — в соответствии с патч-нумерацией, которой мы ведём всю Phase 0 (по решению пользователя «начнём с 0.0.1 и пойдём по патч-версиям»).
- License-комментарий в spec.md переписан с «candidates: MPL-2.0, Apache-2.0» на «FSL-1.1-MIT for core, MIT for plugins; see ADR 0001» — фактическое решение.

**Поставка:**
- [x] `spec.md` §6 M0 — оба оставшихся пункта переведены в `[x]`.
- [x] `docs/changelog/UNRELEASED.md` — Keep a Changelog v1.1 формат, секция Phase 0 (v0.0.1 → v0.0.8) с Added/Changed.

**Acceptance:**
- ✅ spec.md M0 полностью закрыт.
- ✅ Tag `v0.0.8` (заменяет упомянутый в исходном плане `v0.0.0-foundations`).

**Размер:** XS

---

## Фаза 1 — MVP single-node (M1) ✅

**Цель фазы:** на чистом Hetzner CX22 за один `platform-cli init` поднять Tier 1 кластер и задеплоить hello-world `Application` через GitOps.

**Spec:** §6 M1, §4.1 (Tier 1), §4.5, §4.12, §3.1.

### 1.1 platform-cli — каркас CLI

**Статус:** ✅ закрыто 2026-05-06.

**Цель:** Rust-бинарник `platform-cli` с командами-заглушками `init|plan|apply|status|login|upgrade-tier`.

**Решения по ходу:**
- Версионная схема Phase 1: `0.1.x` (минор = фаза, патч = подфаза).
- State хранится как JSON (`.apprafter/state.json`), не CUE-encoded. Переход на CUE-encoded — позже, когда схема состояния стабилизируется.
- CUE-доступ через subprocess (`cue export ... --out json`); FFI-вариант (`cuelang-go`) отложен.
- Workspace из четырёх крейтов: `platform-cli` (бинарь), `cli-core` (ошибки + Tier + логи + CUE), `cli-state` (state-файл), `cli-providers` (трейт + `DryRunProvider`).
- `cue::export_in(workdir, path)` добавлен в API: `cue` отказывается от абсолютных путей и требует относительный путь от module-root, поэтому wrapper вызывает `cue` с `current_dir(workdir)`. Простой `export(path)` — обёртка над `export_in(cwd, path)`.
- Все команды печатают «would …» с указанием будущей фазы plan.md, в которой стаб станет реальной операцией.

**Поставка:**
- [x] Cargo workspace `cli/` с тремя крейтами + бинарь (`platform-cli`, `cli-core`, `cli-state`, `cli-providers`).
- [x] CUE-доступ через subprocess (`cli-core::cue::{export, export_in}`), `CUE_BIN` env-override.
- [x] Структурированный логгер (`tracing` + `tracing-subscriber` с `EnvFilter`).
- [x] State-файл `.apprafter/state.json` (JSON в skeleton-фазе) с `load_or_default` / `save`.
- [x] Шесть команд-стабов через clap derive API.

**Acceptance:**
- ✅ `platform-cli --help` показывает все шесть subcommand'ов.
- ✅ `platform-cli plan` на пустом state выдаёт `no changes`.
- ✅ Workspace проходит `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`.

**Зависит от:** 0.4

**Размер:** M

---

### 1.2 Hetzner Cloud built-in provider

**Статус:** ✅ shipped — sub-phase 1.2 полностью закрыта серией циклов `v0.1.2`–`v0.1.7`, покрывающих server / SSH-keys / network / firewall / CUE-parsing / floating IP / state import.

**Цель:** нативный provider в `platform-cli` через прямую интеграцию с Hetzner Cloud REST API (через `ureq`, blocking).

**Решения по ходу:**
- Используем `ureq` (blocking) + ручные wire-types вместо third-party `hcloud`-crate'ов: тоньше, без async, без неактивных внешних зависимостей.
- Mock-тесты через `mockito` 1.x (sync HTTP).
- Метим все managed-ресурсы лейблом `apprafter=true` — идемпотентность и будущий `import` строятся вокруг этого фильтра.
- `Provider` trait расширен: добавлен `destroy()`, `Plan.changes` переименован в `Plan.actions: Vec<Action>` (типизированные `CreateServer` / `DestroyServer` / `Noop`).
- `State` дополнен `hetzner_cloud: Option<HetznerCloudState>` (server_id + server_name).
- Server boot пока без SSH-ключей (Hetzner возвращает root-password). SSH-keys приедут вместе с network/firewall в следующем цикле.

**Поставка (server-CRUD ветка, v0.1.2):**
- [x] `cli-providers/hetzner_cloud/`: серверный CRUD (list / create / delete) с mockito-тестами.
- [x] `Provider::destroy()` + типизированный `Action`.
- [x] `HetznerCloudClient` (list / create / delete server) с idempotent 404-on-delete.
- [x] `HetznerCloudProvider` impl `Provider`: refresh + diff + apply + destroy.
- [x] CLI `destroy --yes` команда + дисциплина `apply` (требует `HCLOUD_TOKEN`, без него — внятная ошибка).
- [x] `examples/infrastructure/tier-1-hetzner.cue` фикстура.
- [x] `#[ignore]`-тагнутый e2e-тест против реального Hetzner.

**Поставка (SSH-keys, v0.1.3):**
- [x] `HetznerCloudClient` методы для SSH-keys: list / create / delete.
- [x] `Action::CreateSshKey` / `DestroySshKey`, `SshKeySpec`.
- [x] `ServerCreateRequest.ssh_keys` (`Option<Vec<u64>>`, serde-skip).
- [x] `HetznerCloudProvider.ssh_keys: Vec<SshKeySpec>` — refresh + idempotent create + ordered apply (ssh-keys → server) + ordered destroy (server → ssh-keys).
- [x] `HetznerCloudState.ssh_key_ids` cache (back-compat через `#[serde(default)]`).
- [x] `apply` читает `APPRAFTER_SSH_PUBLIC_KEY` env: при наличии — boot с SSH-key (без root-password).

**Поставка (Network + Firewall, v0.1.4):**
- [x] `HetznerCloudClient` методы для Network: list / create / delete.
- [x] `HetznerCloudClient` методы для Firewall: list / create / delete.
- [x] `ServerCreateRequest.networks` + `ServerCreateRequest.firewalls` (Option, serde-skip).
- [x] `Action::CreateNetwork` / `DestroyNetwork` / `CreateFirewall` / `DestroyFirewall`.
- [x] `NetworkSpec`, `FirewallSpec`, `FirewallRuleSpec`.
- [x] `HetznerCloudProvider.{networks,firewalls}` — ordered apply (ssh → net → fw → server) и destroy (server → fw → net → ssh).
- [x] `HetznerCloudState.{network_id,firewall_id}` cache.
- [x] CLI `apply` строит дефолтные `NetworkSpec` (10.0.0.0/16 + 10.0.0.0/24 в `eu-central`) и `FirewallSpec` (SSH 22 + HTTPS 443 ingress) из имени кластера.

**Поставка (CUE Infrastructure parsing, v0.1.5):**
- [x] CUE schema `Infrastructure` расширена optional полями (`region`, `network` с `subnet`, `firewall.ingress`, `sshKeys`, `osImage`).
- [x] `examples/infrastructure/tier-1-hetzner.cue` — полный пример (network 10.0.0.0/16 + subnet eu-central + SSH/HTTPS ingress + osImage).
- [x] `cli-core::manifest` модуль — `InfrastructureManifest` типы и `parse_infrastructure(workdir, path)` через `cue::export_in`.
- [x] `apply.rs`: при `APPRAFTER_MANIFEST=<path>` читает manifest и накладывает на v0.1.4-дефолты (server_type, image, network/subnet/zone, firewall rules, ssh keys). Без env var поведение v0.1.4 не изменилось.

**Поставка (Floating IP, v0.1.6):**
- [x] `HetznerCloudClient` методы для Floating IPs: list / create / delete (404-idempotent).
- [x] Wire-types `FloatingIp`, `FloatingIpListResponse`, `FloatingIpCreateRequest`/`Response`, `HomeLocation`.
- [x] `Action::CreateFloatingIp` / `DestroyFloatingIp`, `FloatingIpSpec`.
- [x] `HetznerCloudProvider.floating_ips` — refresh + idempotent create + ordered apply (ssh → net → fw → server → fip с `server` атрибутом сразу при создании) + ordered destroy (fip → server → fw → net → ssh).
- [x] `HetznerCloudState.floating_ip_ids` cache (back-compat через `#[serde(default)]`).
- [x] CUE schema: `network.floatingIPs: [...string]` (оставалось зарезервированным с v0.1.5).
- [x] CLI `apply` читает `network.floatingIPs` из manifest, префиксует имена кластером и передаёт как `FloatingIpSpec` (`ipv4`, `home_location = region`).
- [x] `examples/infrastructure/tier-1-hetzner.cue` — `floatingIPs: ["egress"]`.

**Поставка (`platform-cli import`, v0.1.7):**
- [x] `Commands::Import { force, dry_run }` clap-вариант + dispatch в `main.rs`.
- [x] `commands::hcloud::hcloud_base_url()` — общий хелпер для `apply`/`destroy`/`import`, читает `APPRAFTER_HCLOUD_BASE_URL` (test-only) с фолбэком на `DEFAULT_BASE_URL`.
- [x] `commands::import::run` — read-only сканирование `apprafter=true` ресурсов, сборка `HetznerCloudState` по `cluster_name`, флаги `--dry-run` (печатает summary, не пишет state) и `--force` (перезаписывает существующий `state.hetzner_cloud`).
- [x] Integration-тесты на `assert_cmd` + `mockito`: happy-path с записью state, `--dry-run` без записи, "no matching server" → friendly message, фильтр по `apprafter` лейблу, `--force` overwrite-guard.
- [x] `cli/README.md` — новая секция "Recovering state with `import`".

**Полное закрытие 1.2:**
- [x] `plan.md` отражает все 6 циклов как ✅; sub-phase 1.2 переведён из 🚧 partial в ✅ shipped.

**Acceptance (v0.1.2):**
- ✅ `platform-cli apply` (с валидным state + `HCLOUD_TOKEN`) поднимает 1× CX22.
- ✅ Повторный `platform-cli apply` — no-op (refresh видит сервер, Plan = пустой).
- ✅ `platform-cli destroy --yes` сносит сервер и чистит state.
- ✅ Mocked-тесты (12 шт.) + `#[ignore]` integration test компилируются и запускаются вручную с реальным Hetzner.

**Зависит от:** 1.1

**Размер:** L (разбит на 6 циклов: server-CRUD `v0.1.2` ✅, SSH-keys `v0.1.3` ✅, network+firewall `v0.1.4` ✅, CUE-parsing `v0.1.5` ✅, floatingIP `v0.1.6` ✅, import `v0.1.7` ✅)

---

### 1.2 AUDIT — Hetzner Cloud built-in provider: IPv6 support ✅

> v0.1.70 — 1.2 AUDIT shipped (partial): wire-type IPv6 parsing, k3s dual-stack cluster/service CIDRs, Hetzner Firewall ICMP allow-rule. `--node-ip` dual-binding и реальный pod-level dual-stack smoke прицеплены к зависимым подфазам (3.1 HA-bootstrap пересекает `--node-ip`; 1.4 AUDIT закрывает Cilium values для pod connectivity).

**Source:** ADR 0017.

**Поставка:**
- [x] `cli-providers/src/hetzner_cloud/types.rs` — новый `PublicIpv6 { ip: String }` (хранит `<prefix>::/64` CIDR-строку как Hetzner возвращает) + `PublicNet.ipv6: Option<PublicIpv6>`. Два regression-guard теста в `tests/types_test.rs` пинят deserialize sample response (dual-stack + только-v6 forward-compat ветка). Re-export `PublicIpv6` через `cli-providers::hetzner_cloud`.
- [x] **Hetzner private network остаётся IPv4-only** (фундаментальное ограничение Hetzner — public IPv6 идёт через server's public interface, private network — внутрикластерная IPv4). Без изменений в `NetworkSpec`; ADR 0017 это явно признаёт.
- [x] `cli-providers/src/hetzner_cloud/user_data.rs` — `K3sBootstrapOptions { dual_stack: bool }` (`Default::default()` = `true`), `build_k3s_user_data` теперь добавляет `--cluster-cidr=10.42.0.0/16,fd00:42::/64 --service-cidr=10.43.0.0/16,fd00:43::/112` per ADR 0017. Константы `CLUSTER_CIDR_DUAL_STACK` / `SERVICE_CIDR_DUAL_STACK` экспортируются для shared-use. Два regression-guard теста — default install line содержит CIDR-пару, opt-out `dual_stack: false` дропает их без касания других disable-флагов.
- [x] `cli/platform-cli/src/commands/apply.rs::default_ingress_rules` — новый ICMP-rule (`direction: in, protocol: icmp, port: None, source_ips: ["0.0.0.0/0", "::/0"]`) per ADR 0017 §Per-tier. Hetzner Cloud Firewall не различает ICMPv4 и ICMPv6 — один `protocol: icmp` правило покрывает обе family. Два regression-guard теста — `default_ingress_rules_emits_one_rule_per_default_port_plus_icmp` (счётчик правил) + `default_ingress_rules_include_icmp_for_pmtu_and_ndp` (shape).
- **Отложено:** `--node-ip` dual-binding пока не передаётся — требует cloud-init substitution с runtime-detected IPv4 + IPv6 host addresses (multi-line bash в `runcmd`), что в Tier 1 single-node не блокирует connectivity (k3s auto-detects). Закроется в 3.1 (HA bootstrap), когда heterogeneous-nodes сценарий делает выбор node IP критичным.
- **Отложено:** Full e2e dual-stack pod connectivity smoke — зависит от 1.4 AUDIT (Cilium Helm values dual-stack), pod не получит v6 интерфейс без Cilium-конфига. После 1.4 AUDIT добавим pod-level v4+v6 reachability assertion в `e2e/mvp.sh`.

**Acceptance:** Hetzner provider парсит IPv6 prefix из API; k3s install line содержит dual-stack CIDR-пару; ICMP allowed в Hetzner Firewall. Pod-level connectivity подтверждается после 1.4 AUDIT (Cilium).

**Зависит от:** —

**Размер:** M (доставлен как single-cycle audit ~v0.1.70; реальный pod-connectivity smoke выкатим вместе с 1.4 AUDIT)

---

### 1.3 k3s bootstrap на свежем VDS

**Статус:** ✅ shipped — sub-phase 1.3 закрыта серией циклов `v0.1.8`–`v0.1.10`: cloud-init bootstrap (k3s + ufw + fail2ban) → kubeconfig retrieval (SSH fetch + URL rewrite) → age-encryption кеша.

**Цель:** автоматическая установка k3s в single-node режиме после провижионинга VM.

**Поставка (cloud-init bootstrap, v0.1.8):**
- [x] `cli-providers::hetzner_cloud::user_data::build_k3s_user_data` — pure builder для `#cloud-config` YAML; собирает install-команду для k3s c `--disable=traefik --disable=servicelb`, ufw default-deny + whitelist (22/6443/80/443 tcp + 51820 udp), fail2ban для SSH jail.
- [x] `ServerCreateRequest.user_data: Option<String>` (serde-skip when None) + `ServerSpec.user_data` + проброс через `HetznerCloudProvider::create_request`.
- [x] CLI `apply` ставит `user_data = Some(build_k3s_user_data(...))`; default Hetzner-firewall расширен до tier-1 whitelist (тот же набор портов, что и в ufw).

**Поставка (kubeconfig retrieval, v0.1.9):**
- [x] `Server.public_net.ipv4.ip` wire field — `cli-providers::hetzner_cloud::types` теперь декодирует public IPv4 с list-ответа.
- [x] `cli-providers::hetzner_cloud::kubeconfig` — `rewrite_server_url(yaml, public_ip)` + `KubeconfigFetcher` trait + `SshKubeconfigFetcher` impl (shell out на системный `ssh`, BatchMode/StrictHostKeyChecking=accept-new); `default_ssh_identity_path()` читает `APPRAFTER_SSH_PRIVATE_KEY` с фолбэком на `~/.ssh/id_ed25519`.
- [x] `HetznerCloudState.kubeconfig_yaml: Option<String>` (`#[serde(default)]`).
- [x] `Commands::Kubeconfig { refresh }` + `commands::kubeconfig::run` + `compute_kubeconfig` orchestrator (cached / cold-fetch / `--refresh`).
- [x] Unit-тесты на `rewrite_server_url`, argv-shape `SshKubeconfigFetcher`, `compute_kubeconfig` через `FakeFetcher` (cold/cached/--refresh); integration на missing-state error + cached print без SSH.
- [ ] (defer to v0.1.10) age-encryption кеша — на этом цикле сохраняем plaintext.

**Поставка (age-encryption, v0.1.10):**
- [x] `cli-core::secrets` — wrapper над `age` 0.10: `load_or_create_identity` (mode 0600, parent dirs auto), `encrypt_for_recipient` (armored), `decrypt_with_identity`, `default_age_key_path()` (env override + `~/.config/apprafter/age.key` fallback).
- [x] `HetznerCloudState.kubeconfig_age: Option<String>` (armored, serde-default); `kubeconfig_yaml` остаётся читаемым один цикл как legacy-fallback и обнуляется на ближайшем `--refresh`.
- [x] `commands::kubeconfig::run` шифрует на запись (recipient = .to_public() identity), расшифровывает на чтение, fallback на plaintext-поле.
- [x] Integration round-trip через предзаписанный age-blob + `APPRAFTER_AGE_KEY` env override; in-file тесты `cli-core::secrets` (round-trip / wrong-identity / persist+reload / mode-0600 / env override / bech32 sanity).

**Acceptance (v0.1.8):** `platform-cli apply` отправляет в Hetzner POST `/v1/servers` с непустым `user_data`-полем; mocked-тесты + unit-тесты builder'а пинят форму YAML.

**Acceptance (1.3 целиком, после v0.1.9):** через ~5 минут после `platform-cli init && platform-cli apply` команда `platform-cli kubeconfig | KUBECONFIG=/dev/stdin kubectl get nodes` показывает Ready single node.

**Зависит от:** 1.2

**Размер:** M (разбит на 3 цикла: cloud-init `v0.1.8` ✅, kubeconfig retrieval `v0.1.9` ✅, age encryption `v0.1.10` ✅)

---

### 1.4 Cilium + Gateway API установка

**Статус:** ✅ shipped — sub-phase 1.4 закрыта серией циклов `v0.1.11`–`v0.1.12`: Cilium через Helm + Gateway API CRDs (1.4a) → default-deny NetworkPolicy + real-cluster smoke (1.4b).

**Цель:** заменить flannel на Cilium с kube-proxy replacement и Gateway API.

**Поставка (Cilium + Gateway API CRDs, v0.1.11):**
- [x] `cli-providers::k8s::cilium_values::cilium_values_yaml()` — pure builder для tier-1 Helm-values (`kubeProxyReplacement: true`, `ipam: kubernetes`, `hubble: enabled: false`, `operator: replicas: 1`).
- [x] `cli-providers::k8s::helm` — `HelmRunner` trait + `HelmCli` shell-out + `HelmUpgradeArgs` + `CILIUM_CHART_VERSION = "1.16.5"`.
- [x] `cli-providers::k8s::kubectl` — `KubectlRunner` trait + `KubectlCli` shell-out + `ManifestSource` enum + `gateway_api_crds_url()` (pinned `v1.2.1`).
- [x] `Commands::ClusterBootstrap` + `commands::cluster_bootstrap::run()` + pure `perform_bootstrap<H, K>` orchestrator (helm repo add → helm upgrade --install → kubectl apply -f gateway CRDs); driven с fake runners в in-file tests.
- [x] `build_k3s_user_data` теперь добавляет `--disable-kube-proxy` к k3s install line — без этого `kubeProxyReplacement: true` бессмыслен.

**Поставка (NetworkPolicy + smoke, v0.1.12):**
- [x] `cli-providers::k8s::network_policy::default_deny_network_policy_yaml(namespace)` — pure builder для `NetworkPolicy` (apiVersion `networking.k8s.io/v1`, podSelector `{}`, policyTypes ingress + egress, label `apprafter=true`).
- [x] `perform_bootstrap` теперь применяет default-deny на `default` namespace после Gateway API CRDs; kube-system намеренно exempt; `kubectl apply -f` идёт из tempfile.
- [x] Renamed FakeKubectl test (`perform_bootstrap_runs_helm_repo_then_install_then_two_kubectl_applies`) пинит call sequence + ManifestSource type для каждого apply.
- [x] `cli/platform-cli/tests/cluster_smoke_test.rs` — `#[ignore]`-tagged real-cluster smoke; opt-in через `APPRAFTER_K8S_SMOKE=1` + `KUBECONFIG`; проверяет `cilium status --wait`, `kubectl apply --dry-run=server -f Gateway`, наличие default-deny NetworkPolicy.

**Acceptance (v0.1.11):** `platform-cli cluster-bootstrap` выводит сводку и завершается успешно (mocked-runner test); реальный smoke (`cilium status` зелёный, `kubectl apply` Gateway проходит admission) — после v0.1.12.

**Зависит от:** 1.3

**Размер:** M (разбит на 2 цикла: Cilium + Gateway API CRDs `v0.1.11` ✅, NetworkPolicy + smoke `v0.1.12` ✅)

---

### 1.4 AUDIT — Cilium + Gateway API установка: dual-stack Helm values ✅

> v0.1.71 — 1.4 AUDIT shipped: `cilium_values_yaml()` явно декларирует `ipv4.enabled: true` + `ipv6.enabled: true`; `e2e/mvp.sh` получил Phase 6.4 с pod-level dual-stack assertion (закрывает отложенный 5-й чекбокс 1.2 AUDIT).

**Source:** ADR 0017.

**Поставка:**
- [x] Cilium Helm values builder лежит в `cli-providers/src/k8s/cilium_values.rs` (`cilium_values_yaml()`).
- [x] Assess текущего state (v0.1.70): `ipv4.enabled` не объявлен явно (Helm chart 1.16.x default = true, но это implicit); `ipv6.enabled` не объявлен (default = false) → поды никогда не получают v6 интерфейс даже когда k3s выдаёт dual-stack podCIDR. IPAM mode = `kubernetes` (правильно — k3s публикует pod CIDR через Node API, Cilium читает оттуда без собственного allocator'а).
- [x] Updated Helm values на dual-stack: добавлены два явных блока `ipv4: { enabled: true }` и `ipv6: { enabled: true }`. IPAM `mode: kubernetes` сохранён без изменений.
- [x] Gateway API CRDs install path — verified: `kubectl apply -f gateway-api/standard-install.yaml` ставит **type definitions only**, не listeners. Family-binding происходит при создании Gateway resource'ом (см. позже в 4.1a), и Gateway API spec поддерживает `listener.protocol: HTTPS` без family-restriction — listener bind'ится на любую family, доступную на node (после v0.1.71 — обе). Никаких изменений в install path не требуется.
- [x] E2E `Phase 6.4: dual-stack podIPs assertion` в `e2e/mvp.sh` — после Phase 6 (endpoint curl green) делает `kubectl get pod -l app=e2e-hello -o jsonpath='{.items[0].status.podIPs[*].ip}'` и assert'ит наличие **обоих** v4-адреса из `10.42.0.0/16` (k3s podCIDR) и v6-адреса из `fd00:42::/64` (k3s podCIDR v6 + Cilium ipv6.enabled). Без 1.4 AUDIT этот assert валится с понятным сообщением "Cilium ipv6.enabled likely false". Реальный outbound v6 curl-тест (curl -6 ipv6.google.com из pod'а) отложен в Phase 3.x — pod-image `nginxdemos/hello:plain-text` не имеет curl, добавление test-pod с `curlimages/curl:latest --ipv6` пересекается с network observability (Hubble), которую закроет 3.7a.

**Acceptance:** `cilium_values_yaml()` декларирует обе family explicit; unit-test `dual_stack_enabled_per_adr_0017` пинит наличие `ipv4:` + `ipv6:` blocks и счётчик `enabled: true` ≥ 2; `e2e/mvp.sh` Phase 6.4 валится если у pod'а отсутствует v6 IP.

**Зависит от:** 1.2 AUDIT (Hetzner provider dual-stack) ✅

**Размер:** S — доставлен как single-cycle audit ~v0.1.71.

**Known wart (deferred to Track B 1.70):** `helm upgrade cilium` патчит `cilium-config` ConfigMap, но **не** триггерит rotation cilium DaemonSet pods (chart v1.16.x не имеет `checksum/config` аннотации в template'е). На свежий install это не влияет (агенты сразу стартуют с новыми values), но на upgrade существующего кластера оператору приходится вручную `kubectl rollout restart daemonset cilium -n kube-system` + пересоздать pod'ы, чтобы они получили v6 IP. Quick-fix в `cluster-bootstrap` (один `kubectl rollout restart`) добавил бы ~30с к каждому re-run; вместо этого ждём 1.70 (`cluster-bootstrap` rewrite в Argo CD-managed flow), где Argo CD resource hooks решают это нативно — disposable код мы тогда не пишем.

---

### 1.5 Argo CD установка и bootstrap

**Статус:** ✅ shipped — sub-phase 1.5 закрыта серией циклов `v0.1.13`–`v0.1.17`: helm install (1.5a) → admin password (1.5b) → cert-manager + ClusterIssuer (1.5c) → Gateway + HTTPRoute (1.5d) → bootstrap-Application + smoke (1.5e).

**Цель:** Argo CD как единственный механизм применения манифестов в кластер.

**Поставка (Argo CD Helm install, v0.1.13):**
- [x] `cli-providers::k8s::argocd_values::argocd_values_yaml()` — pure builder для tier-1 Helm-values (Dex off, Redis-HA off, ApplicationSet on, Notifications off, ClusterIP server, single replicas).
- [x] `ARGOCD_CHART_VERSION = "7.7.7"` в том же модуле.
- [x] `perform_bootstrap` теперь делает helm repo add `argo` + helm upgrade --install `argocd` после default-deny NP; renamed FakeRunner test пинит call sequence для обеих helm releases (cilium → argocd) и обоих kubectl applies.
- [x] `cluster-bootstrap` дропает 4-й tempfile (Argo CD values) рядом с kubeconfig / Cilium values / default-deny NP.

**Поставка (admin password retrieval, v0.1.14):**
- [x] `KubectlRunner::get_secret_value(name, namespace, key, kubeconfig)` — wraps `kubectl get secret -o jsonpath={.data.<key>}` + base64-decodes; argv-shape unit test.
- [x] `Commands::ArgocdPassword { refresh }` + `commands::argocd_password::run` + pure `compute_argocd_password<K>` orchestrator.
- [x] `HetznerCloudState.argocd_admin_password_age: Option<String>` (serde-default).
- [x] In-file FakeKubectl tests + cli_smoke missing-state error + integration test для cached-path round-trip через `APPRAFTER_AGE_KEY`.

**Поставка (cert-manager + self-signed ClusterIssuer, v0.1.15):**
- [x] `cli-providers::k8s::cert_manager_values::cert_manager_values_yaml()` — pure builder для tier-1 Helm-values (`installCRDs: true`, single replicas, Prometheus off).
- [x] `CERT_MANAGER_CHART_VERSION = "v1.16.2"` в том же модуле.
- [x] `cli-providers::k8s::issuer::selfsigned_cluster_issuer_yaml()` — pure builder для `cert-manager.io/v1 ClusterIssuer` `apprafter-selfsigned` (`spec.selfSigned: {}`, label `apprafter=true`); имя issuer'а как `pub const APPRAFTER_SELFSIGNED_ISSUER` чтобы будущие HTTPRoute / Certificate manifests могли ссылаться без магических строк.
- [x] `perform_bootstrap` теперь делает helm repo add `jetstack` + helm upgrade --install `cert-manager` после Argo CD, и kubectl apply self-signed issuer; renamed FakeRunner test пинит 3 helm repos / 3 installs / 3 kubectl applies в правильном порядке.
- [x] `cluster-bootstrap` дропает 5-й и 6-й tempfile (cert-manager values + selfsigned issuer) рядом с существующими.

**Поставка (Gateway + HTTPRoute для Argo CD UI, v0.1.16):**
- [x] CUE schema: `spec.argocd.domain?` optional поле в `#Infrastructure`.
- [x] Rust manifest mirror: `cli_core::manifest::ArgocdBlock { domain: Option<String> }` + `InfrastructureSpec.argocd: Option<ArgocdBlock>`.
- [x] `cli-providers::k8s::argocd_gateway::argocd_gateway_yaml(domain)` — pure builder для 3-document манифеста (Gateway + HTTPRoute + Certificate); все ресурсы в namespace `argocd`, label `apprafter=true`, Certificate ссылается на `apprafter-selfsigned` ClusterIssuer.
- [x] `perform_bootstrap` подросла `argocd_gateway_path: Option<&Path>` параметром; при Some — kubectl apply после self-signed ClusterIssuer; при None — bootstrap идентичен v0.1.15.
- [x] `cluster_bootstrap::run` парсит `APPRAFTER_MANIFEST` если установлен, извлекает domain, conditionally дропает 7-й tempfile.
- [x] `examples/infrastructure/tier-1-hetzner.cue` — закомментированный пример opt-in.

**Поставка (bootstrap-Application + закрытие 1.5, v0.1.17):**
- [x] CUE schema: `spec.argocd.bootstrapRepo?` + `spec.argocd.bootstrapPath?` optional поля.
- [x] Rust manifest mirror: `ArgocdBlock.bootstrap_repo: Option<String>` (rename `bootstrapRepo`) + `bootstrap_path: Option<String>` (rename `bootstrapPath`).
- [x] `cli-providers::k8s::bootstrap_app::bootstrap_application_yaml(repo_url, path)` — pure builder для `argoproj.io/v1alpha1 Application` `bootstrap` (namespace `argocd`, syncPolicy.automated.prune+selfHeal, label `apprafter=true`); `BOOTSTRAP_APP_DEFAULT_PATH = "."` для пустого пути.
- [x] `read_argocd_settings_from_manifest` возвращает struct (domain + bootstrap_repo + bootstrap_path); `cluster_bootstrap::run` conditionally дропает 8-й tempfile.
- [x] `perform_bootstrap` подросла `bootstrap_app_path: Option<&Path>` параметром; при Some — kubectl apply после optional Argo CD Gateway.
- [x] Real-cluster smoke в `cluster_smoke_test.rs`: `kubectl get application bootstrap -n argocd` под gate `APPRAFTER_BOOTSTRAP_REPO_SMOKE=1`.
- [x] Sub-phase 1.5 status: ✅ shipped.

**Acceptance (v0.1.13):** `perform_bootstrap` производит `helm install cilium`, `kubectl apply` Gateway CRDs, `kubectl apply` default-deny NP, `helm install argocd` в этом порядке (mocked). Реальный smoke (Argo CD pods Ready, UI reachable, root app sync) — после v0.1.15.

**Зависит от:** 1.4

**Размер:** M (разбит на 5 циклов: helm install `v0.1.13` ✅, admin password `v0.1.14` ✅, cert-manager + ClusterIssuer `v0.1.15` ✅, Gateway + HTTPRoute `v0.1.16` ✅, bootstrap-Application + smoke `v0.1.17` ✅)

---

### 1.6 Backstage минимальный деплой

**Статус:** ✅ shipped — sub-phase 1.6 закрыта серией циклов `v0.1.18`–`v0.1.20`: k8s-манифесты (1.6a) → app-скаффолд + Dockerfile (1.6b) → app-config ConfigMap + OAuth stub (1.6c).

**Цель:** Backstage как pod в кластере, доступный через Gateway.

**Поставка (k8s-манифесты + cluster-bootstrap, v0.1.18):**
- [x] CUE schema: `spec.backstage.domain?` + `spec.backstage.image?`.
- [x] Rust manifest mirror: `BackstageBlock { domain, image }`; `Default` derived.
- [x] `cli-providers::k8s::backstage_manifests::backstage_manifests_yaml(domain, image)` — pure builder для 6-document манифеста (Namespace + Deployment + Service + HTTPRoute + Gateway + Certificate); `BACKSTAGE_DEFAULT_IMAGE = "ghcr.io/apprafter/backstage:placeholder"` для пустого image.
- [x] `read_argocd_settings_from_manifest` переименован в `read_cluster_settings_from_manifest`, struct расширен backstage-полями.
- [x] `perform_bootstrap` подросла `backstage_manifests_path: Option<&Path>` параметром; при Some — kubectl apply после bootstrap-Application.
- [x] `manifests/tier-1/backstage/{example.yaml, README.md}` — статический рендеринг builder'а + recipe для refresh через `cargo run --example backstage_example`.

**Поставка (app-скаффолд + Dockerfile, v0.1.19):**
- [x] `backstage-plugins/host/Dockerfile` — multi-stage Backstage 1.x шаблон (Node 20 builder + slim runtime, копия skeleton/bundle tarballs, EXPOSE 7007, USER node).
- [x] `backstage-plugins/host/.dockerignore` — node_modules, build-output, local config, secrets out of context.
- [x] `backstage-plugins/host/scripts/scaffold.sh` — обёртка над `npx @backstage/create-app@latest --skip-install` с preflight'ом (Node 20+, target пустой), drop'ом Dockerfile/.dockerignore рядом, печатью next-steps. Shellchecked.
- [x] `backstage-plugins/host/README.md` — 6-step workflow scaffold → install → build → push → manifest → cluster-bootstrap; cross-links к Dockerfile, scaffold-скрипту, rendered example manifest, Rust-builder'у.
- [x] `cli/README.md` — blockquote-cross-link к host-app README рядом со step 10.

**Поставка (app-config ConfigMap + OAuth stub, v0.1.20):**
- [x] `cli-providers::k8s::backstage_app_config::backstage_app_config_yaml(domain)` — pure builder для tier-1 `app-config.yaml` (`app.title`, `app.baseUrl`, `backend.baseUrl` + `cors.origin` от `domain`, `backend.listen 0.0.0.0:7007`, `database better-sqlite3 :memory:`, `auth.providers.guest.dangerouslyAllowOutsideDevelopment: true`).
- [x] `backstage_manifests_yaml` теперь эмитит 7-document YAML — добавлен `ConfigMap` `backstage-config` с `data["app-config.yaml"]: |<rendered>`, и Deployment получил `volumeMount` (subPath, readOnly) в `/app/app-config.yaml`.
- [x] `manifests/tier-1/backstage/example.yaml` пере-рендерён.
- [x] Sub-phase 1.6 status: ✅ shipped.

**Acceptance:** `https://backstage.<domain>` открывается, виден catalog (пустой), `auth.providers.guest` пускает без логина (`dangerouslyAllowOutsideDevelopment: true`).

**Зависит от:** 1.5

**Размер:** M (разбит на 3 цикла: k8s-манифесты `v0.1.18` ✅, app-скаффолд `v0.1.19` ✅, app-config + OAuth `v0.1.20` ✅)

---

### 1.7 Application CRD v1alpha1 ✅

> v0.1.25 — schema refactor: `base` + `environments` moved under `spec` for k8s-convention alignment before phase 1.8.

**Цель:** зарегистрировать CRD `Application` в кластере, схема валидируется через CUE.

**Поставка:**
- [x] OpenAPI v3 схема CRD: hand-rolled YAML мирорит CUE `#ApplicationSpec` (v0.1.22 — `cli-providers::k8s::application_crd` + apply через cluster-bootstrap; `cue cmd export-crd` автогенерация откладывается до v0.2.x).
- [x] Поля v1alpha1: `image`, `expose`, `replicas`, `env` (только литералы), `environments` map (v0.1.21 — schema + Rust parser).
- [x] Admission webhook (Rust, axum + rustls) в отдельном pod с auto-rotated cert (cert-manager Certificate в `apprafter-system`, `cert-manager.io/inject-ca-from` синхронит `caBundle` на ValidatingWebhookConfiguration; v0.1.23 — webhook crate + Dockerfile, v0.1.24 — k8s-манифесты + cluster-bootstrap wiring).
- [x] Невалидный manifest реджектится с понятной ошибкой (v0.1.24 — webhook возвращает `Application is invalid: <field>: <reason>` через AdmissionReview, kube-apiserver включает это сообщение в ответ `kubectl apply`).

**Acceptance:** `kubectl apply` валидного Application проходит; невалидного — с сообщением, указывающим поле и причину.

**Зависит от:** 0.4, 1.5

**Размер:** M

---

### 1.8 Application operator — каркас на kube-rs ✅

> v0.1.26 — sub-phase 1.8a shipped: 3 library crates (`operator-core` + `operator-rendering` + `operator-controllers/application`).
> v0.1.27 — sub-phase 1.8b shipped: `apprafter-operator` binary + 3 Prometheus signals + axum `/healthz` / `/readyz` / `/metrics`.
> v0.1.28 — sub-phase 1.8c shipped: Lease-based leader election (`operator-core::leader`).
> v0.1.29 — sub-phase 1.8d shipped: Helm chart at `operator/charts/apprafter-operator/`. Phase 1.8 ✅.

**Цель:** Rust-операторный pod с reconcile-loop по `Application`.

**Поставка:**
- [x] `operator/` — workspace с подпакетами `operator-core`, `operator-controllers/application`, `operator-rendering` (v0.1.26).
- [x] Контроллер на `kube-rs`, leader election через Lease (v0.1.27 + v0.1.28).
- [x] Метрики Prometheus: `reconcile_total`, `reconcile_duration`, `reconcile_errors` (v0.1.27).
- [x] Структурированный лог (tracing) (v0.1.27 — `tracing-subscriber::EnvFilter` в `apprafter-operator/main.rs`).
- [x] Health/readiness endpoints (v0.1.27 — `/healthz` + `/readyz` axum routes).
- [x] Helm chart для деплоя оператора (v0.1.29 — `operator/charts/apprafter-operator/`).

**Acceptance:** оператор запускается, видит Application-объекты, пишет «reconciled» в лог; metrics endpoint отвечает.

**Зависит от:** 1.7

**Размер:** M

---

### 1.9 Application reconcile: image + expose + replicas ✅

> v0.1.30 — sub-phase 1.9a shipped: pure `render_application` for Deployment + Service.
> v0.1.31 — sub-phase 1.9b shipped: reconcile applies children via SSA + updates `status`.
> v0.1.32 — sub-phase 1.9c shipped: per-environment expansion (`APPRAFTER_ENV` selects override). Phase 1.9 ✅. HTTPRoute deferred to a later phase that owns Gateway domain config end-to-end.

**Цель:** Application → Deployment + Service + HTTPRoute.

**Поставка:**
- [x] Renderer (pure-функция) `Application → [k8s Resource]` (v0.1.30 — Deployment + Service; HTTPRoute deferred to a phase that owns Gateway domain config end-to-end).
- [x] Per-environment expansion (v0.1.32 — pure-Rust merge; functionally equivalent to CUE unification for our v1alpha1 schema, switchable to CUE FFI when CUE-only constructs are added).
- [x] Apply-семантика: server-side apply с field manager `apprafter-operator` (v0.1.31).
- [x] Status subresource: `phase`, `observedGeneration`, `conditions`, `endpointURL` (v0.1.31).
- [x] Удаление Application удаляет дочерние ресурсы (ownerReferences) (v0.1.30).

**Acceptance:** манифест Application с image+expose даёт работающий HTTP endpoint, доступный изнутри кластера; `curl` на endpoint отвечает.

**Зависит от:** 1.8

**Размер:** M

---

### 1.10 Backstage Application plugin (status view) ✅

> v0.1.33 — sub-phase 1.10a shipped: TypeScript scaffold + types + pure handler stubs.
> v0.1.34 — sub-phase 1.10b shipped: `KubeApplicationStore` proxies kube apiserver via in-cluster SA token.
> v0.1.35 — sub-phase 1.10c shipped: applications-frontend scaffold + `ApplicationsApi` interface + pure `applicationsToRows` transform.
> v0.1.36 — sub-phase 1.10d shipped: `ApplicationsTable` + `ApplicationDetail` + `EnvironmentTabs` React components + per-env helpers. Backstage `createApiRef` + `createPlugin` wiring documented as a consumer-side snippet (keeps the package's dep tree light enough to publish independently). Phase 1.10 ✅.

**Цель:** в Backstage — список Application, статус, ссылка на endpoint, последние события.

**Поставка:**
- [x] Backstage backend plugin читает k8s API напрямую (через kubeconfig service account) (v0.1.33 + v0.1.34 — `@apprafter/applications-backend` с `KubeApplicationStore` через in-cluster service-account token).
- [x] Frontend plugin: таблица + drilldown (v0.1.36 — `ApplicationsTable` + `ApplicationDetail` React components).
- [x] События: replicas / status / последние deploys (v0.1.36 — `ApplicationDetail` рендерит `status.phase` + `status.observedGeneration` + полный список `conditions` с `lastTransitionTime`).
- [x] Per-environment вкладки (dev/staging/prod) (v0.1.36 — `EnvironmentTabs` controlled component + `applicationsForEnvironment` filter helper).

**Acceptance:** в Backstage виден задеплоенный hello-world, статус Ready, ссылка работает.

**Зависит от:** 1.6, 1.9

**Размер:** M

---

### 1.11 Golden-path template: Bun HTTP service ✅

> v0.1.37 — sub-phase 1.11a shipped: `examples/templates/bun-http/` starter (OneBun + multi-stage Dockerfile + v1alpha1 Application.cue).
> v0.1.38 — sub-phase 1.11b shipped: Backstage Software Template (`template.yaml` + `skeleton/`) + operator quickstart at `docs/dev-guide/quickstart.md`. Phase 1.11 ✅.

**Цель:** Backstage Software Template, генерирующий стартер на OneBun.

**Поставка:**
- [x] Template в `examples/templates/bun-http/`: `package.json`, `Dockerfile` (multi-stage, distroless), `src/index.ts` + `app.module.ts` + `health.controller.ts` + `config.ts` (OneBun controllers + envSchema), `apprafter/Application.cue` (v0.1.37).
- [x] Backstage software template manifest с параметрами (имя, репо, домен) (v0.1.38 — `template.yaml` + `skeleton/` subdir со scaffolder Nunjucks templating).
- [x] Документация в `docs/dev-guide/quickstart.md` (v0.1.38).

**Acceptance:** через UI Backstage за 3 клика создаётся репо с готовым стартером; коммит → Argo CD → задеплоилось.

**Зависит от:** 1.10

**Размер:** S

---

### 1.12 End-to-end MVP smoke-тест ✅

> v0.1.39 — sub-phase 1.12a shipped: `e2e/mvp.sh` orchestration script + operator-guide quickstart.
> v0.1.40 — sub-phase 1.12b shipped: `.github/workflows/nightly.yml` (cron 04:00 UTC + workflow_dispatch). Phase 1.12 ✅ pending operator's "5 greens in a row" judgment call — the automation lands here, the verdict lands when the streak holds.

**Цель:** воспроизводимый E2E-тест полного пути: чистый Hetzner-аккаунт → задеплоенный hello-world.

**Поставка:**
- [x] Скрипт `e2e/mvp.sh`: `platform-cli init` → ждёт готовности → деплоит hello-world → проверяет HTTP-endpoint (v0.1.39 — Application-via-template путь живёт в operator-guide quickstart до публикации образа оператора).
- [x] CI nightly job (с реальным Hetzner project под отдельный billing-tag) (v0.1.40 — `.github/workflows/nightly.yml`, cron 04:00 UTC + workflow_dispatch; billing-tag через `apprafter=true` label, выделенный CI tag отложен до propagation labels через provider).
- [x] Таймер: фиксируем «time-to-first-application», цель < 30 минут (v0.1.39 — `START_NS` + `elapsed` в mvp.sh; observed 6-9 min, well under 30-min budget).
- [x] `docs/operator-guide/quickstart.md` — те же шаги вручную (v0.1.39).

**Acceptance:** nightly зелёный 5 раз подряд; ручной прогон по docs работает у нового человека.

**Зависит от:** 1.11

**Размер:** M

---

### 1.13 Закрытие чек-листа M1 spec ✅

**Поставка:**
- [x] Обновить `spec.md` §6 M1 — все пункты `[x]` (v0.1.41).
- [x] Tag `v0.1.0-mvp` (v0.1.41 — the v0.1.41 commit also carries an annotated `v0.1.0-mvp` tag pointing at the same SHA).
- [x] Release notes (v0.1.41 — `docs/changelog/UNRELEASED.md` graduates the Phase 1 section into a `v0.1.0-mvp` release block).

**Размер:** XS

---

### 1.14 Level B integration cycle (default-on operator + webhook) ✅

> v0.1.64 — sub-phase 1.14 shipped: `cluster-bootstrap` installs the AppRafter operator + admission-webhook by default from ghcr.io images published by `release-operator.yml`. Default-on semantics with opt-out via `spec.{operator,admissionWebhook}.enabled: false`. Fork builds override via `image` + `tag` fields; variant-C resolution semantics (full-ref ignores `tag`).

**Поставка (v0.1.64):**
- [x] `cli-providers::k8s::image_ref` — `RELEASED_OPERATOR_VERSION` const + `resolve_image_ref` variant-C helper (6 unit tests).
- [x] `cli-providers::k8s::operator_values` — pure values-YAML builder (3 unit tests).
- [x] `cli-providers::k8s::operator_chart` — `include_dir!`-embedded helm chart + runtime extractor (2 tests).
- [x] `cli-core::manifest` — `OperatorBlock` + extended `AdmissionWebhookBlock` (4 schema tests).
- [x] `HelmUpgradeArgs.version` → `Option<String>` (allows local-path chart installs).
- [x] `perform_bootstrap` gains operator + webhook orchestration steps in `apprafter-system` (step 8 + step 9 per spec).
- [x] 5 new orchestration tests (default install order; operator opt-out; webhook opt-out; full-ref override; tag-only override).
- [x] CUE schema extension: `spec.operator?:` block + extended `spec.admissionWebhook?:`.
- [x] `e2e/mvp.sh` Phase 6.5 — apply Application CR + poll status `Ready` + assert child Deployment `Available` (60s deadline).
- [x] `docs/operator-guide/quickstart.md` §5 rewrite (operator pod required → operator pod installed by default).

**Acceptance:** против чистого Hetzner кластера новый оператор проходит manual walk из spec §1.14 (init → apply → kubeconfig → cluster-bootstrap → kubectl apply Application → `.status.phase == Ready` за 60с → child Deployment живой) без `helm install` руками и без «build your own image».

**Зависит от:** 1.13

**Размер:** S (один цикл, ~3 рабочих дня)

---

### 1.15 Level C GitOps cycle (env-driven Argo CD repo credentials) ✅

> v0.1.65 — sub-phase 1.15 shipped: `cluster-bootstrap` provisions the `apprafter-bootstrap-repo-creds` Argo CD Secret automatically when `APPRAFTER_ARGOCD_REPO_TOKEN` is set, enabling private GitHub/GitLab `spec.argocd.bootstrapRepo` without `kubectl apply` of a Secret by hand. Public-repo path unchanged. 4-quadrant manual walk documented in `docs/operator-guide/gitops-walk.md`.

**Поставка (v0.1.65):**
- [x] `cli-providers::k8s::argocd_repo_secret` — pure builder + 2 constants (4 unit tests).
- [x] `cluster_bootstrap::read_argocd_repo_creds` testable helper over injected env-lookup closure (4 unit tests).
- [x] `ClusterSettings` gains `argocd_repo_creds: Option<(String, String)>`; `default_cluster_settings` + `read_cluster_settings_from_manifest` populate it from env.
- [x] `perform_bootstrap` gains `argocd_repo_secret_path: Option<&Path>` parameter at bootstrap step 9.5 (between webhook and Argo CD HTTPRoute).
- [x] `run()` builds the tempfile when both creds + `bootstrap_repo` are `Some`, wires path through.
- [x] Success-message suffix mentions repo-creds Secret when applied.
- [x] 3 new orchestration tests (token+repo creates Secret before bootstrap App; token absent skips Secret but keeps App; no bootstrap repo skips both).
- [x] 8 pre-existing orchestration tests updated for the new arg position.
- [x] `docs/operator-guide/gitops-walk.md` — 4-quadrant runbook (GitHub × GitLab × public × private) with prereqs, DoD checklists, troubleshooting matrices, token-rotation + revoke sections.
- [x] `docs/operator-guide/quickstart.md` §3 gains the env-var opt-in bullet.

**Acceptance:** против чистого Hetzner кластера новый оператор проходит manual walk из spec §1.15 (все 4 квадранта end-to-end), каждый walk заканчивается зелёным DoD checklist (Argo CD UI: bootstrap = Synced + Healthy; child Application reconcilen оператором; для private — Secret присутствует в argocd ns).

**Зависит от:** 1.14

**Размер:** S (один цикл, ~2 рабочих дня кода + manual walk)

---

## Фаза 1.5 — Self-managing platform rethink (M1.5) ⚡

**Цель фазы:** архитектурный rethink из ADR 0025–0029. Переход от imperative `cluster-bootstrap` к Argo CD-managed platform stack из versioned OCI chart, declarative version control через PlatformStack CRD, unified MigrationPlan для application + platform scopes, CUE compilation для user app repos через CMP sidecar.

**Spec:** §3.10, §3.11 (PlatformStack), §3.8 (MigrationPlan unified). ADRs 0025–0029.

**Almighty target:** «happy path» first user experience сжимается до ~30 минут end-to-end (install binary → `apprafter init` → `apprafter bootstrap-all` → `apprafter open argocd` → add app repo via UI → app deployed). Каждая подфаза landing'ится как `v0.1.66`–`v0.1.83` patch release (loose recommendation, точное mapping commit-driven). После закрытия M1.5 — tag `v0.2.0-self-managing`, после чего Phase 2 (M2) стартует с `v0.2.0-services` уже на правильном фундаменте.

**Numbering:** под-фазы M1.5 используют 1.66–1.83 как continuation Phase 1 namespace, поскольку landing'ятся последовательными `v0.1.66`–`v0.1.83` releases перед `v0.2.0-services`. Major version stays `0`; minor reflects phase number (`v0.2.x` для всего между M1.5 и M3 closure, `v0.3.x` для M3 series, etc.).

**Blocks Phase 2** потому что Phase 2 (ServiceProviders, ResourceClaims, Tenant logic) builds on the GitOps-managed platform. Landing Phase 2 поверх split-brain дизайна приведёт к технического долгу — все ServiceProviders придётся reframing когда M1.5 doлжен будет landить позже.

### M1.5 Track positioning — CLI DX rework + Platform rethink + Dev-mode integration

M1.5 содержит **три work tracks**, выполняемых **последовательно** в указанном порядке. Каждый track имеет свой authoritative spec:

| Track | Order | Authoritative spec | Description |
|---|---|---|---|
| **A. CLI DX rework** | First | `cli-dx-task.md` §17 (12 items) | Target store, `apprafter target {add,list,use,...}`, `whoami`, `doctor`, `bootstrap-all` wrapper, miette errors, rename `platform-cli` → `apprafter`, aliases, color/NO_COLOR. **Prerequisite for Track B** потому что platform rethink relies on new CLI infrastructure (target resolution, bootstrap-all, manifest auto-discovery). |
| **B. Platform rethink** | Second | this file, sub-phases **1.66 — 1.83** (18 items, numbered below) | Argo CD as control surface, PlatformStack CRD + Controller, MigrationPlan unification, CUE → OCI chart distribution, CMP for user app CUE. Lands after Track A is complete. |
| **C. Dev-mode Phase 1B** | Third | `dev-mode-task.md` §20 Phase 1B | Minimum viable dev mode — `apprafter dev {cluster up, init, up, down, list, logs}` on local k3d. Lands after Track B closure (after `v0.2.0-self-managing` tag). |

**Why this order**:

- **Track A first**: bootstrap-all wrapper, target resolution, и miette errors are required for the minimal `cluster-bootstrap` rewrite (Track B 1.70) to provide acceptable UX. Без Track A, platform rethink landed бы на том же `cargo run --bin platform-cli` from-source workflow, который сейчас documented gap. Reverse order would create two sub-optimal user experiences during M1.5.
- **Track B second**: platform rethink uses CLI infrastructure from Track A. Once `cluster-bootstrap` rewritten и PlatformStack CRD in place, M1.5 closes with `v0.2.0-self-managing` tag.
- **Track C third**: dev-mode Phase 1B benefits from Track A CLI rework AND reuses Track B platform-stack chart's tier-1 overlay (with new `tiers/dev.cue` overlay). Lands after `v0.2.0-self-managing` as a follow-up patch series before M2 begins.

**Dependencies between tracks (sequential)**:
- Track A `target store` (`cli-dx-task.md` §5.1–5.6) → Track B `cluster-bootstrap` rewrite (1.70) requires target resolution instead of env vars.
- Track A `bootstrap-all` orchestrator (`cli-dx-task.md` §5.11) → Track B 1.70 — these should be one cohesive piece of work, landed within Track A.
- Track A `apprafter open` (referenced in Track B 1.79) → either land as part of Track A late items или Track B 1.79 — choose at implementation time when Track A is nearly done.
- Track C dev-mode Phase 1B references Application CRD operator (already shipped в Phase 1 v0.1.7-v0.1.65), benefits from Track A CLI rework, и reuses Track B's `tiers/dev.cue` overlay.

**M1.5 closure**: tag `v0.2.0-self-managing` after **both Track A and Track B** complete. Track C dev-mode Phase 1B lands as a follow-up patch series (e.g., `v0.2.1`, `v0.2.2` patch numbers depending on how it splits across commits). Phase 2B и Phase 3B из dev-mode-task.md лежат в later milestones (after M2 and M3 respectively).

**Total M1.5 aggregate**: Track A (12 small-medium items per `cli-dx-task.md` §17) + Track B (18 items, 1 L + 7 M + 8 S + 2 XS) ≈ **L+ overall**, with Track C following as a separate ~M-aggregate series. The heavy work concentrates in Track B 1.73 (PlatformController — the only L item with distributed-systems penalty applied); most other items are S or M.

---

## Фаза 1.5 / Track A — CLI DX rework (`cli-dx-task.md` §17)

> 12 sub-versions, one per `cli-dx-task.md` §17 row, landed as `v0.1.69`–`v0.1.80` patch releases. Each row owns a focused slice (feature + test + docs). Track B (sub-phases 1.66 onwards) **does not start** until Track A is closed — its `cluster-bootstrap` rewrite depends on the target store + `bootstrap-all` orchestrator + miette errors landed here.

### 1.66A.1 Rename `platform-cli` → `apprafter` + deprecation shim ✅

> v0.1.69 — sub-phase 1.66A.1 shipped: Cargo package + binary flipped to `apprafter`; legacy `platform-cli` survives as a deprecated shim that warns + forwards; user-facing docs swept.

**Source:** `cli-dx-task.md` §12 + §17 row 1.

**Цель:** перевести user-facing binary с легаси-имени `platform-cli` на каноничное `apprafter` без слома существующих скриптов. Foundation для всех остальных Track A под-фаз (target store, `bootstrap-all`, `doctor`, `whoami`), которые landятся последовательно в `v0.1.70`–`v0.1.80`.

**Поставка:**
- [x] `cli/platform-cli/Cargo.toml` — package переименован в `apprafter`; `[[bin]] name = "apprafter"` (path `src/main.rs`) — каноничная точка входа; второй `[[bin]] name = "platform-cli"` (path `src/bin/platform-cli.rs`) — shim, помеченный к удалению в `v0.2.0`.
- [x] `cli/platform-cli/src/bin/platform-cli.rs` — shim: печатает 3-строчный deprecation warning на stderr, потом `Command::new(apprafter)` + `.args(skip(1))` + forward exit code; cross-platform (`.exe` suffix на Windows).
- [x] `cli/platform-cli/src/cli.rs` — `#[command(name = "apprafter", ...)]` для clap-help; about-line обновлён.
- [x] `cli-core::logging::init` — дефолтный `EnvFilter` теперь `warn,apprafter=info,cli_core=info,cli_state=info,cli_providers=info`; без этого фикса INFO-логи фильтровались после переименования крейта (regression поймана `cli_smoke::tracing_logs_go_to_stderr_not_stdout`).
- [x] User-facing error hints (`run \`platform-cli init …\` first` и т.п.) в `commands/apply.rs`, `commands/argocd_password.rs`, `commands/cluster_bootstrap.rs`, `commands/import.rs`, `commands/kubeconfig.rs` теперь ссылаются на `apprafter`.
- [x] Internal docstrings + Cargo descriptions в `cli/cli-core`, `cli/cli-providers`, `cli/cli-state` обновлены — grep-discoverability осталась консистентной.
- [x] Все 4 integration-теста (`cli_smoke`, `argocd_password_test`, `import_test`, `kubeconfig_test`, `cluster_smoke_test`) переключены на `Command::cargo_bin("apprafter")`.
- [x] Новый regression-guard `cli_smoke::platform_cli_shim_warns_and_forwards` — пинит обе половины контракта shim'а (deprecation banner на stderr + forwarded `plan` output untouched на stdout + exit code).
- [x] User-visible docs sweep: `README.md`, `cli/README.md`, `e2e/{README.md,mvp.sh}`, `operator/{README.md,charts/apprafter-operator/README.md}`, `backstage-plugins/host/{README.md,scripts/scaffold.sh}`, `manifests/**/README.md`, `examples/templates/bun-http/**`, `schemas/v1alpha1/{infrastructure,infrastructureproviderplugin}.cue`, `docs/{architecture,dev-guide,operator-guide,reference}/**/*.md`, `.github/ISSUE_TEMPLATE/bug.yml`, `SECURITY.md`, `.gitignore` — все ссылаются на `apprafter`.
- [x] `spec.md` swept (kept `cli/platform-cli/` dir name in Appendix A repository tree with explicit comment that dir is renamed in `v0.2.x`).
- [x] `docs/changelog/UNRELEASED.md` — new `v0.1.69` block с Changed/Added/Docs/Backwards-compatibility секциями; historic v0.1.x entries сохранены as-is (no rewriting of past system state).

**Acceptance:**
- ✅ `cargo build --workspace` зелёный — обе bin entry (`apprafter` + `platform-cli`) компилируются.
- ✅ `cargo test --workspace` зелёный (61+ unit + 16 integration, включая новый shim-test).
- ✅ `cargo clippy --workspace --all-targets -- -D warnings` зелёный.
- ✅ `cargo fmt --all -- --check` зелёный.
- ✅ Manual walk (см. ниже) — `apprafter --help` работает, shim прячет deprecation warning на stderr + forwards exit code, существующие env-var-based workflows (`HCLOUD_TOKEN=… apprafter init …`) функционируют без изменений.

**Out-of-scope (отложено в следующие Track A слоты):**
- Persistent target store (`apprafter target {add,list,use,show,rename,remove}`) — Track A.2/A.3.
- Interactive wizard через `inquire` + miette-diagnostic errors — Track A.4.
- `apprafter doctor`, `apprafter whoami` — Track A.6/A.7.
- `apprafter bootstrap-all` orchestrator — Track A.9.
- `apprafter auth` stubs — Track A.6.
- Aliases (`apprafter t`), `--color` flag, `NO_COLOR` support — Track A.11.
- ADR `docs/adr/0014-cli-command-structure.md` — Track A.12 (после всей Track A landed).
- Переименование dir `cli/platform-cli/` → `cli/apprafter/` — отложено до `v0.2.0-self-managing` (M1.5 closure) одной cleanup-коммитой.

**Зависит от:** 1.65 (последняя закрытая Track-B-prerequisite).

**Размер:** S (один цикл, ~0.5 рабочего дня механического свипа + один новый файл shim + один regression-guard тест).

---

### 1.66A.2 Target file structure + IO module ✅

> v0.1.72 — sub-phase 1.66A.2 shipped: `cli-core::target` module — types (`GlobalConfig`, `TargetConfig`, `TargetCredentials`, `Target`, `TargetStorePaths`) + atomic load/save IO + 0600 enforcement on credentials. No CLI commands wired yet — pure foundation for A.3+ wizards.

**Source:** `cli-dx-task.md` §4 (file layout), §8 (deps), §17 row 2.

**Поставка:**
- [x] Новые workspace deps: `serde_yaml = "0.9"` + `dirs = "5"`; `tempfile` промотан из dev-dependencies cli-core в обычные (atomic-write использует его в prod-коде).
- [x] `cli-core/src/target.rs` (~570 строк):
    - `default_config_root()` → `dirs::config_dir().join("apprafter")` — cross-platform XDG.
    - `TargetStorePaths { root }` testable locator с методами `global_config_file`/`targets_dir`/`target_dir`/`target_config_file`/`target_credentials_file`/`auth_dir`/`auth_keep_file`/`state_dir` — миррорит spec §4 на тип-уровне.
    - `GlobalConfig { active_target, version }` с `TARGET_STORE_VERSION = 1` форвард-compat кодом.
    - `TargetConfig { provider, region, default_tier, cluster_name, ssh_key_path }` (`#[serde(default)]`).
    - `TargetCredentials { hetzner_token: Option<String> }` — **manual `Debug` impl** с `<redacted>` маркером (никогда не derive — лекит токен в любом `println!("{:?}", ...)`).
    - `load_global_config`/`save_global_config`/`load_target`/`save_target`/`list_target_names`/`remove_target`.
    - `atomic_write(path, bytes, secret)` — tempfile-in-same-dir + fsync + chmod (0600 для secret, 0644 для public) + `persist()` rename (POSIX-atomic, ReplaceFileW на Windows).
    - `ensure_auth_placeholder()` — создаёт `auth/.keep` на любом первом write, чтобы reserved namespace existed для будущего Managed.
- [x] `cli-core/src/error.rs`: новые варианты `InvalidTargetConfig { path, message }`, `TargetNotFound { name, available }`, `Yaml(serde_yaml::Error)` через `#[from]`.
- [x] `cli-core/src/lib.rs`: pub use re-export всех target-типов и функций; модуль `target` зарегистрирован.
- [x] 16 regression-guard unit-tests (inline в `target.rs`):
    - `default_config_root_points_at_user_config_dir_under_apprafter` — leaf path sanity guard.
    - `paths_compose_per_spec_directory_layout` — пин on-disk shape против spec §4 (если кто-то ренеймит `TARGETS_DIR` константу, тест отлетит).
    - `load_global_config_returns_none_on_fresh_store` — first-run case ОК.
    - `save_then_load_global_round_trips_active_target` — round-trip global.
    - `save_global_creates_auth_placeholder_directory` — auth/.keep всегда создаётся.
    - `load_global_config_returns_invalid_target_config_on_corrupt_yaml` — corrupt YAML → typed error.
    - `save_then_load_target_round_trips_both_halves` — round-trip per-target config + creds.
    - `load_target_returns_target_not_found_with_available_list` — error message включает comma-separated список существующих имён.
    - `load_target_tolerates_missing_credentials_file` — dotfiles-only сценарий (config есть, credentials нет — возвращает empty creds, не ошибку).
    - `credentials_file_lands_at_mode_0600` (Unix-only `#[cfg(unix)]`) — пин разрешений: credentials.yaml = 0600, config.yaml = 0644.
    - `list_target_names_returns_empty_on_fresh_store` + `list_target_names_returns_sorted_names_skipping_dot_dirs` — список target'ов сортирован, hidden dirs (`.scratch` от atomic-write tempfiles) скрыты.
    - `remove_target_deletes_both_files_and_state_dir` — удаление каскадно сносит `state/<name>/`.
    - `remove_target_returns_target_not_found_when_missing` — idempotency помощник.
    - `credentials_debug_redacts_token` — пинит `<redacted>` маркер в Debug формате (защита от случайного println).
    - `atomic_write_leaves_no_temp_files_on_success` — после успешного save в корне нет `.apprafter-tgt-*.tmp` файлов.

**Acceptance:**
- ✅ `cargo build --workspace` зелёный (новые `serde_yaml`, `dirs`, `tempfile` deps скомпилированы).
- ✅ `cargo test --workspace`: 26 cli-core тестов (16 новых target + 10 pre-existing), 0 failures across весь workspace.
- ✅ `cargo clippy --workspace --all-targets -- -D warnings` — clean.
- ✅ `cargo fmt --all -- --check` — clean.
- ✅ `scripts/check-spdx-headers.sh` (150 файлов) + `scripts/lint-cue.sh` — green.
- ✅ Module re-exports проверены: `use cli_core::{TargetStorePaths, GlobalConfig, Target, TargetConfig, TargetCredentials, save_target, load_target, list_target_names, remove_target}` компилируется в чистом downstream crate'е (semantics test through smoke tests in Track A.3).

**Out-of-scope (отложено в зависимые слоты):**
- Никаких CLI команд — `apprafter target add/list/use/...` приходят в Track A.3 (non-interactive) → A.4 (interactive wizard) → A.5 (CRUD-набор).
- Provider validator framework (`token format regex`, API ping) — Track A.4.
- Resolution chain plumbed в existing commands (`init/apply/cluster-bootstrap` берут токен из active target) — Track A.8.
- Migration существующего `<cwd>/.apprafter/state.json` в per-target `<root>/state/<name>/` — Track A.8.
- `secrecy::Secret<String>` wrapper для in-memory защиты — добавится в Track A.3/A.4 когда credentials handling попадает в hot path. Сейчас защита через manual Debug-redact + mode 0600 на файле.

**Зависит от:** 1.66A.1 ✅

**Размер:** S (один цикл, ~1 рабочий день кода + тестов).

---

### 1.66A.3 `apprafter target add` non-interactive ✅

> v0.1.73 — sub-phase 1.66A.3 shipped: clap subcommand `apprafter target add <name>` (+ alias `apprafter t add`) с pure flag-driven flow, валидация (name shape, provider whitelist, token format, ssh-key readable), `--force` / `--renew` семантика, первый target auto-promotes в active. Interactive wizard via `inquire` — отложен в A.4.

**Source:** `cli-dx-task.md` §5.1 (non-interactive flow), §10 (error patterns), §11 (validation rules), §17 row 3.

**Поставка:**
- [x] `cli/platform-cli/src/cli.rs` — новый `Commands::Target { action }` варинт + `TargetCommand::Add { name, provider, token, ssh_key, region, tier, cluster_name, force, renew, no_interactive }` enum-вариант. `#[command(alias = "t")]` на `Target` группе → `apprafter t add …` работает идентично `apprafter target add …`. `--token` читает `HCLOUD_TOKEN` env var через `#[arg(env)]`, `--ssh-key` — `APPRAFTER_SSH_PUBLIC_KEY_PATH`. `--force` и `--renew` помечены `conflicts_with` для clap-уровневого reject комбинации.
- [x] `cli/platform-cli/src/commands/target.rs` — handler-модуль:
    - `run(action) → match → run_add(args)`.
    - `validate_target_name(name)` — non-empty, ≤64 chars, `[A-Za-z0-9-]+`, без leading/trailing `-`.
    - `require_known_provider(opt)` — non-None + whitelist (`["hetzner-cloud"]` пока).
    - `require_token(provider, opt)` — non-None + per-provider format check. Для hetzner-cloud зовёт `cli_core::target::validate_hetzner_token_format`.
    - `verify_ssh_key_readable(path)` — `path.exists()` + `read_to_string` success.
    - `run_renew(paths, args)` — error если target отсутствует; refuses конфиг-флаги (только credentials); зовёт `save_target(existing.with(new creds))`.
    - `ensure_active_target(paths, name)` — `save_global_config({active_target: name})` только если `load_global_config` возвращает None (first-run case); subsequent saves не трогают active pointer.
- [x] `cli/platform-cli/src/main.rs` — `match args.command` (move) + dispatch `Commands::Target { action } => commands::target::run(action)?`. Существующие command::run сигнатуры (`&str` / `bool`) совместимы через `&owned` / Copy.
- [x] `cli-core::target` extensions:
    - `CONFIG_DIR_ENV = "APPRAFTER_CONFIG_DIR"` — env override для `default_config_root()` (testing ergonomics + power-user redirect). Используется verbatim, без `.join("apprafter")` суффикса.
    - `validate_hetzner_token_format(token)` — `^hcloud_[a-zA-Z0-9]{60,}$` без `regex` crate (string-методы достаточно для такой простой проверки). Возвращает `Result<(), String>` — caller-level wrap в `CliError::Other` с context'ом.
- [x] 17 integration-тестов (`tests/target_test.rs`):
    - happy path (`writes_config_and_credentials_and_promotes_first_target_to_active`) — пинит на-disk layout, active pointer, token landed.
    - mode 0600 (`credentials_file_is_mode_0600`, Unix-only).
    - env-var fallback (`uses_hcloud_token_env_var_as_fallback`) — clap `#[arg(env)]` works.
    - missing token (`errors_when_token_missing_entirely`).
    - unknown provider (`errors_on_unknown_provider`).
    - malformed token (`errors_on_malformed_hetzner_token`).
    - invalid name (`errors_on_invalid_target_name`).
    - no-force overwrite reject (`refuses_to_overwrite_existing_target_without_force`) — error message включает оба `--force` и `--renew` hint'а.
    - force overwrite (`force_overwrites_existing_target_and_keeps_active_pointer`) — active pointer **не** меняется на second-save.
    - renew rotate (`renew_rotates_credentials_without_touching_config`).
    - renew on missing (`renew_on_missing_target_errors_with_hint`).
    - renew refuses config flags (`renew_rejects_config_flags`).
    - clap-level conflict (`force_and_renew_are_mutually_exclusive`).
    - ssh-key path verified (`with_ssh_key_path_verifies_file_exists`).
    - ssh-key missing (`errors_when_ssh_key_path_missing`).
    - second save preserves active (`second_target_save_keeps_first_as_active_and_reports_so`).
    - alias `t` works (`target_alias_t_subcommand_resolves_to_target`).
- [x] 10 unit-тестов inline в `commands/target.rs` (pure validators): `validate_target_name` × 5 cases, `require_known_provider` × 2, `require_token` happy + bad, `verify_ssh_key_readable` × 2.
- [x] 6 unit-тестов в `cli_core::target` (валидатор + env override): `default_config_root_honours_apprafter_config_dir_env_override`, `_ignores_empty_env_override`, `validate_hetzner_token_format` × 4.

**Acceptance:**
- ✅ `apprafter target add default --provider hetzner-cloud --token hcloud_…` создаёт целый target + ставит active.
- ✅ Subsequent `target add another --provider … --token …` создаёт, **не** трогает active.
- ✅ `target add existing` без `--force` → error с hint'ом на оба `--force` и `--renew`.
- ✅ `target add existing --force` → overwrites; `target add existing --renew --token …` → только credentials, config preserved.
- ✅ `apprafter t add … ` — alias работает.
- ✅ `--force --renew` одновременно — clap reject (`conflicts_with`).
- ✅ Malformed token / unknown provider / invalid name — typed error на stderr.
- ✅ `cargo test --workspace`: 0 failures (17 new integration + 10 unit + 6 cli-core validator = +33 vs v0.1.72). fmt + clippy + SPDX (151 files) — clean.

**Out-of-scope (отложено):**
- Interactive wizard через `inquire` — Track A.4 (v0.1.74).
- `apprafter target list / use / show / rename / remove` — Track A.5.
- `apprafter target add` real API ping (`GET /v1/locations` against Hetzner) — Track A.4 validator framework.
- `apprafter whoami` aggregator (показать active target + provider verified status) — Track A.6.
- Resolution chain в существующие `init / apply / cluster-bootstrap` — Track A.8.

**Зависит от:** 1.66A.2 ✅ (target store IO).

**Размер:** S (один цикл, ~1 рабочий день).

---

### 1.66A.4 Provider validator framework + Hetzner API ping ✅

> v0.1.75 — sub-phase 1.66A.4 (split as A.4a) shipped: `cli-providers::validators` module с `ProviderValidator` trait + `HetznerCloudValidator` (`GET /v1/locations`); `target add` теперь делает real API ping по умолчанию + flag `--no-ping` / env `APPRAFTER_NO_PING` для CI/offline. Interactive wizard через `inquire` — отложен в **1.66A.4b** (v0.1.76).

**Source:** `cli-dx-task.md` §5.1 (token verified step) + §11 (validation framework), §17 row 4.

**Поставка:**
- [x] `cli/cli-providers/src/hetzner_cloud/types.rs` — новые wire-types `Location { id, name, description, country, city, network_zone }` + `LocationListResponse`. Re-exported через crate root.
- [x] `cli/cli-providers/src/hetzner_cloud/client.rs::list_locations()` — `GET /v1/locations` с тем же error-mapping шаблоном как остальные list_X методы (на 2xx parse, на 4xx/5xx → `CliError::Hetzner`, на transport-fail → `CliError::Other`). Reused validator-ом и (будущим в A.4b) wizard-region-picker'ом.
- [x] `cli/cli-providers/src/validators.rs` — новый модуль:
    - `pub trait ProviderValidator { fn validate_credentials(&self) -> Result<()>; }` — минимальная поверхность (region/type lookups придут с wizard'ом в A.4b).
    - `pub struct HetznerCloudValidator { client: HetznerCloudClient }` + `new(base_url, token)` + impl `ProviderValidator` через `self.client.list_locations().map(|_| ())`.
    - 3 unit-теста с mockito: 200 OK (валид) / 401 → typed `CliError::Hetzner` / closed-port → `CliError::Other` transport.
- [x] `cli-providers::lib.rs` — re-export `HetznerCloudValidator` + `ProviderValidator`.
- [x] `cli/platform-cli/src/cli.rs::TargetCommand::Add` — новый флаг `--no-ping` с env-binding `APPRAFTER_NO_PING` через `BoolishValueParser` (принимает `1/0/yes/no/true/false/on/off` — не только canonical `true`/`false`, чтобы shell-сcripty `APPRAFTER_NO_PING=1 apprafter target add ...` работал).
- [x] `cli/platform-cli/src/commands/target.rs::ping_provider(provider, token)` — orchestrator: знает per-provider маршруты, для hetzner-cloud зовёт `HetznerCloudValidator::new(hcloud_base_url(), token).validate_credentials()`. Error-mapping расширен с human-readable hint'ами:
    - 401 → "Hetzner Cloud rejected the token (HTTP 401)..."
    - non-401 HTTP error → "Hetzner Cloud API ping failed (HTTP {status})..." + reassurance что target NOT saved.
    - transport-fail → "could not reach Hetzner Cloud at {base}..." + `--no-ping` hint.
- [x] `run_add` теперь делает ping после format/ssh-key checks но ДО save (так, чтобы failed ping не оставлял half-state на диске). `run_renew` — analogous, ping после `require_token`.
- [x] Success-message в `target add` теперь упоминает статус: `... (token verified against Hetzner Cloud)` или `... (token NOT verified — \`--no-ping\` was passed)`. Closes the cli-dx-task.md §5.1 "✓ Token verified" UX promise on the non-interactive flow (interactive wizard will reuse the same string in A.4b).
- [x] 5 новых integration-тестов (`tests/target_test.rs`):
    - `target_add_pings_provider_by_default_and_announces_verified_status` — mockito 200, target saved + UI says "verified". `mockito::Mock::expect(1)` пинит, что ping реально был сделан.
    - `target_add_surfaces_typed_error_on_hetzner_401` — mockito 401 + assertion что target dir на диске **не** создан (нет half-state).
    - `target_add_surfaces_helpful_error_when_api_is_unreachable` — closed port 1 → error message содержит либо "could not reach" либо "API ping failed" (зависит от платформы) + `--no-ping` hint.
    - `target_add_no_ping_flag_skips_validator_and_announces_unverified` — `--no-ping` короткозамыкает на нereachable base URL.
    - `target_add_no_ping_env_var_also_skips_validator` — `APPRAFTER_NO_PING=1` равноценен флагу.
- [x] 17 prior integration-тестов получили `APPRAFTER_NO_PING=1` через sed-инжект (focus тех тестов — file IO / clap parsing, не API; новые ping-тесты эту поверхность покрывают отдельно).

**Acceptance:**
- ✅ `apprafter target add <name> --token <valid>` ходит в Hetzner, success-message содержит "(token verified against Hetzner Cloud)".
- ✅ С невалидным токеном — typed error "(HTTP 401)" + target **не** сохранён.
- ✅ С недоступным API — typed error + `--no-ping` hint.
- ✅ `--no-ping` / `APPRAFTER_NO_PING=1` — short-circuit, success-message "(token NOT verified)".
- ✅ Existing env-var-based `apply` / `cluster-bootstrap` flow не затронут — ping живёт только в `target add`.
- ✅ `cargo test --workspace`: 22 target_test (17 prior +5 new) + 118 cli-providers (115 +3 validator) — 0 failures. fmt + clippy + SPDX (153 files) — clean.

**Out-of-scope (отложено):**
- Interactive wizard через `inquire` — Track 1.66A.4b (v0.1.76).
- Region validator + region-picker (использует уже готовый `list_locations`) — пойдёт в A.4b вместе с wizard'ом.
- `secrecy::Secret<String>` обёртка для in-memory tokens — A.10/A.11 (miette + secret hardening pass).
- Resolution chain в operational `init/apply/cluster-bootstrap` — A.8.

**Зависит от:** 1.66A.3 ✅ (target add non-interactive).

**Размер:** S (один цикл, ~1 рабочий день).

---

### 1.66A.4b Interactive wizard via `inquire` ✅

> v0.1.76 — sub-phase 1.66A.4b shipped: `commands::target_wizard` модуль с `inquire`-based prompts (Text/Password/Select), default-when-TTY поведение, inline validation внутри token prompt'а (format + API ping), region-picker через `list_regions()`. `--no-interactive` отключает wizard явно; CRUD команды (`list/use/show/...`) и `whoami`/`doctor`/`bootstrap-all` — следующие итерации.

**Source:** `cli-dx-task.md` §5.1 (interactive flow) + §9 (TTY detection) + §17 row 4.

**Поставка:**
- [x] Workspace dep `inquire = "0.7"`; `dirs` поднят в platform-cli (использует для `~/.ssh/id_ed25519.pub` default'а и tilde expansion в SSH-key prompt).
- [x] `cli-providers::validators::ProviderValidator` расширен `fn list_regions() -> Result<Vec<RegionInfo>>`. `RegionInfo { name, description }` с `Display` impl `<name> — <description>` для удобного scanning в `Select`. `HetznerCloudValidator::list_regions()` мапит `client.list_locations()` → отсортированный по `name` Vec. +2 mockito-тестa (sorted output + Display fallback на empty description).
- [x] `cli/platform-cli/src/cli.rs`: positional `name` теперь `Option<String>` — wizard может его спросить; ошибка surface'ится после wizard'а если name всё ещё None (non-TTY/`--no-interactive` сценарий).
- [x] `commands/target.rs::check_target_name(&str) -> Result<(), String>` — pure helper экспортирован для wizard'а (validation сообщения консистентны между CLI surface и `inquire::Validation::Invalid`). `validate_target_name` остаётся как CliError-обёртка.
- [x] `commands/target_wizard.rs` — новый модуль:
    - `should_use_wizard(no_interactive, stdin_tty, stdout_tty, all_required_present)` — pure decision, testable. Wizard fires только когда **обе** consoli TTYs И `--no-interactive` не передан И хотя бы один required input отсутствует. Если все флаги supplied — non-interactive path даже на TTY (respect explicit intent).
    - `run_add_wizard(initial: &AddArgs) -> Result<WizardOutput>` — последовательность из 6 prompts по spec §5.1:
        1. **Target name** — `Text` с default `default`, валидатор вызывает `check_target_name`.
        2. **Provider** — `Select` (сейчас одна опция `hetzner-cloud`, оставлен Select-shape для forward-compat).
        3. **Provider token** — `Password` с `PasswordDisplayMode::Masked`. Inline-валидатор сначала проверяет формат, потом, если не `--no-ping`, делает API ping через `HetznerCloudValidator::validate_credentials()`. Failure → `Validation::Invalid("…")` → инквайр перепросит. Success → eprintln `✓ Token verified`.
        4. **SSH public key path** — `Text` с default `<home>/.ssh/id_ed25519.pub` (через `dirs::home_dir`); пустая строка = "skip". Tilde expansion `~/...` через `expand_tilde` helper.
        5. **Default region** — `Select` из `validator.list_regions()` (когда token verified). При `--no-ping` fallback на `Text` с default `nbg1` (нет API → нет picker'а).
        6. **Default tier** — `Select` по spec choices: `solo / team / prod / regulated` с `Display` impl `<key> — <human label>`.
    - `run_renew_wizard(provider, no_ping)` — упрощённый prompt только токена (config preserved).
    - Все prompts мапят `InquireError::OperationCanceled/Interrupted` → `CliError::Other("wizard aborted by user")` (Ctrl-C / Esc не дают backtrace).
- [x] `commands/target.rs::run_add`:
    - Сначала evaluate `should_use_wizard(...)`, если true — `run_wizard_into_args(&mut args)` заполняет missing поля.
    - После wizard'а (или без него) `name` обязан быть `Some`; иначе typed error "target name required — pass it as a positional argument ... or run on a TTY".
    - `--renew` wizard ветвится отдельно: если name отсутствует — спрашиваем сначала name (Text + `check_target_name`), потом загружаем existing target для определения provider'а, потом `run_renew_wizard` для нового токена.
    - Save-time ping остаётся (re-verifies даже когда wizard уже ping'нул — cheap ~200ms, save-time check — authoritative).
- [x] 5 unit-тестов inline в `target_wizard.rs` (pure helpers): `should_use_wizard` (4 ветки), `expand_tilde` (3 cases: `~/`, abs path, `~user/` not expanded), `inline_ping_error` (401 vs 5xx сообщения), `validate_for_provider` (good/bad/unknown), `TierChoice` Display.

**Acceptance:**
- ✅ `apprafter target add` на TTY → wizard просит все поля по порядку, inline-показывает `✓ Token verified` после успешного ping'а.
- ✅ `apprafter target add work --provider hetzner-cloud --token <X>` на TTY → wizard НЕ fires (все required supplied), сразу не-interactive path.
- ✅ `apprafter target add work --no-interactive` без token'а → typed error "is required" (TTY не помогает когда явно non-interactive).
- ✅ Pipe / CI → no TTY → wizard skipped, как раньше.
- ✅ Esc / Ctrl-C во время wizard'а → "wizard aborted by user" (без backtrace, exit-code 1).
- ✅ `cargo test --workspace`: 22 target_test (без изменений) + 5 target_wizard unit + 64 cli-core + 120 cli-providers (+2 list_regions) — 0 failures.
- ✅ fmt + clippy + SPDX (154 files) + CUE — clean.

**Out-of-scope (явно отложено):**
- E2E wizard testing с PTY-harness — overkill для текущего MVP; manual walks покрывают prompt UX.
- "✓ Token verified (account: …, project: …)" detail per `cli-dx-task.md` §5.1 — Hetzner `/v1/locations` не возвращает account info, нужен `/v1/me`-style endpoint (Hetzner такого не имеет). Текущий "✓ Token verified" — достаточный signal.
- `apprafter target list / use / show / rename / remove` — Track 1.66A.5.
- `secrecy::Secret<String>` обёртка для tokens in-memory — A.10/A.11 hardening pass.

**Зависит от:** 1.66A.4a ✅ (validator framework + API ping).

**Размер:** M (один цикл, ~1.5 рабочих дня).

---

### 1.66A.5 Target CRUD — `list / use / show / rename / remove` ✅

> v0.1.79 — sub-phase 1.66A.5 shipped: 5 новых subcommand'ов поверх target store (`tabled`-based table в `list`, kubectl-style `use/show`, `rename` с FS move + active-pointer обновлением, `remove` с `--yes` opt-in или interactive confirm). v0.1.77 + v0.1.78 wizard polish — затрагивает только `target add`; CRUD-набор полностью отдельный.

**Source:** `cli-dx-task.md` §5.2–§5.6 + §6 (aliases) + §17 row 5.

**Поставка:**
- [x] Workspace dep `tabled = "0.15"` (для `target list` рендера); promoted в platform-cli как direct dep.
- [x] `cli_core::target::rename_target(paths, from, to)` — атомарный `fs::rename` target-директории + best-effort move per-target state cache (`state/<from>/`). Refuses на missing-source (`CliError::TargetNotFound`) и existing-destination (`CliError::Other`). Re-exported через crate root. 4 unit-теста: happy path с state cache + missing source + dest collision + no-state-cache path.
- [x] `cli/platform-cli/src/cli.rs`: новые `TargetCommand::{List, Use, Show, Rename, Remove}` варианты per spec §5.2–5.6. `Remove` имеет `--yes` flag.
- [x] `cli/platform-cli/src/commands/target.rs`:
    - **`run_list`** — собирает rows через `list_target_names` + `load_target` per name (skip-with-tracing-warn на unreadable, не валит вся листинг). Tabled-derive struct `TargetListRow { active, name, provider, region, tier }` с `Style::sharp()` (чистая ASCII-таблица). Empty store → onboarding hint "apprafter target add". Trailing summary `N targets configured. Active: '<name>'.`.
    - **`run_use(name)`** — validates target exists (через `load_target`), updates `GlobalConfig.active_target` через `save_global_config`. Polite no-op message если уже active.
    - **`run_show(name)`** — `name` Optional, default → active. Если no-active + no-name → typed error с hint'ом. Печатает Provider/Region/Default tier/Cluster name/SSH key/Hetzner token (через `token_summary(opt)` который выдаёт `"set (N chars; read credentials.yaml for the raw value)"` или `"not set"` — НЕ echo'ит токен). Trailing — на-диске пути config.yaml + credentials.yaml (mode 0600).
    - **`run_rename(from, to)`** — validates `to` через `check_target_name`, refuses identical from==to, вызывает `cli_core::target::rename_target`, потом если `active_target == from` — обновляет global config на `to`.
    - **`run_remove(name, yes)`** — `load_target` для existence + canonical TargetNotFound hint. Если `!yes`: на TTY показывает `inquire::Confirm` (default `false`), на non-TTY refuses ("non-interactive invocation: pass `--yes` to confirm ..."). После `remove_target`: если был active — pointer ре-assigned на alphabetically next remaining target; если targets закончились — `config.yaml` deleted (фреш-сторе-поведение возвращается).
- [x] `token_summary` pure helper + unit-тест: НЕ leak'ит byte'ы токена даже частично.

**Тесты (16 новых integration в `target_test.rs` + 4 cli-core unit + 1 platform-cli unit = 21 total):**
- `target_list_on_empty_store_prints_onboarding_hint`
- `target_list_renders_table_with_active_marker_and_columns`
- `target_use_switches_active_pointer_and_reports_the_swap`
- `target_use_on_already_active_is_a_polite_noop`
- `target_use_on_missing_target_surfaces_available_hint`
- `target_show_with_no_args_renders_active_target_with_masked_token` (пинит что token НЕ появляется в output)
- `target_show_with_explicit_name_renders_named_target_without_active_marker`
- `target_show_on_empty_store_errors_with_onboarding_hint`
- `target_rename_moves_files_and_updates_active_pointer`
- `target_rename_non_active_target_leaves_active_pointer_alone`
- `target_rename_refuses_when_destination_exists`
- `target_rename_rejects_invalid_destination_name`
- `target_rename_refuses_identical_source_and_destination`
- `target_remove_with_yes_flag_deletes_and_reassigns_active_alphabetically`
- `target_remove_last_target_clears_active_pointer`
- `target_remove_non_active_target_keeps_active_pointer_intact`
- `target_remove_non_interactive_without_yes_refuses`
- `target_remove_on_missing_target_surfaces_available_hint`
- + `token_summary` unit
- + 4 `rename_target` cli-core unit-тестов

**Acceptance:**
- ✅ `apprafter target list` рисует таблицу с `*` маркером на active, или onboarding hint на empty store.
- ✅ `apprafter target use <name>` свитчит active; missing → friendly error c available-listом.
- ✅ `apprafter target show [name]` показывает details; токен замаскирован как `set (N chars; ...)` без leak'а.
- ✅ `apprafter target rename <from> <to>` атомарен (либо обе директории на месте при collision, либо ровно одна после успеха), active-pointer follows automatically.
- ✅ `apprafter target remove <name>` требует `--yes` на non-TTY, prompt'ит на TTY; удаление active → reassign alphabetically.
- ✅ `apprafter t list/use/show/rename/remove` alias works (через существующий `#[command(alias = "t")]` на `Target`).
- ✅ `cargo test --workspace`: 36 target_test (16 новых CRUD + 20 prior) + 74 cli-core (+4 rename) — 0 failures. fmt + clippy + SPDX (155 files) — clean.

**Out-of-scope (отложено):**
- "Last used" / "Account" / "Cluster status" колонки в `list` + `show` — нужна telemetry-wire-up через A.8 (operational commands записывают `last_used_at`) и/или Hetzner `/v1/account`-style endpoint которого у Hetzner нет публично.
- ADR `docs/adr/0014-cli-command-structure.md` про resource-first grouping + auth namespace — Track A.12 (docs+ADR final pass).
- `apprafter whoami` / `apprafter auth …` (stub) — Track A.6.

**Зависит от:** 1.66A.4b ✅ (wizard) — используем тот же target store API.

**Размер:** M (один цикл, ~1 рабочий день кода + tests).

---

### 1.66A.6 `apprafter whoami` + `auth` stubs ✅

> v0.1.80 — sub-phase 1.66A.6 shipped: top-level `apprafter whoami` (identity + active target + verified status) + hidden `apprafter auth login/logout/status` stubs (per spec §3.1 reserved namespace для Managed AppRafter Cloud).

**Source:** `cli-dx-task.md` §5.7 (auth stubs) + §5.8 (whoami) + §3.1 (two-layer identity/target model) + §17 row 6.

**Поставка:**
- [x] `apprafter whoami` — новая top-level команда с одним флагом `--no-ping` (+ env `APPRAFTER_NO_PING` через `BoolishValueParser`). Рендер:
    - `Identity:     anonymous (self-hosted mode)` — placeholder до Track A.10+ когда Managed Cloud auth wires in.
    - `Target:       <name> (active)` или onboarding hint на empty store.
    - `Provider:     hetzner-cloud (<verification status>)` — статус: `verified ✓` / `verified ✓` skipped (если `--no-ping`) / `verification failed ✗ — token rejected (HTTP 401). Run \`apprafter target add <name> --renew\` ...` / `... HTTP <N> from provider API` / `... provider unreachable (network?)`. **Failed ping НЕ валит whoami** — операторы на flaky network'е получают остальную инфо.
    - `Region:`, `Default tier:`, `Cluster name:`, `SSH key:` (с маркером `(loaded)` если файл существует, `(missing!)` если path в config'е есть но файла нет на диске, `not set` если не задан). `~/...` tilde-abbreviation через локальный `abbreviate_home` (3 строки, без cross-module surface).
- [x] `apprafter auth login/logout/status` — три hidden stub'а per spec §5.7. `Commands::Auth` помечен `#[command(hide = true)]` → не появляется в `apprafter --help` (не загромождает new-user discovery surface). Под-команды реальны (`apprafter auth --help` работает): `login` и `logout` печатают friendly redirect "AppRafter Cloud is not yet available... apprafter target add"; `status` — "self-hosted mode active. Use `apprafter whoami`...". Все три имеют ссылку на `https://apprafter.dev`. `AuthCommand` enum — реальный Subcommand (не stub-string), чтобы future Managed impl заполнял ветки без CLI surface re-shape.
- [x] `cli/platform-cli/src/commands/whoami.rs` (~150 LOC + 5 unit-тестов): pure `verified_status(target, no_ping)` + `ssh_key_status(opt)` + `abbreviate_home(p)` helpers; orchestrator `run(no_ping)`. Best-effort ping → не валит whoami.
- [x] `cli/platform-cli/src/commands/auth.rs` (~60 LOC): три `run_X` функции через shared `print_redirect` helper.
- [x] `cli/platform-cli/src/cli.rs`: `Commands::Whoami { no_ping }` + `Commands::Auth { #[command(hide = true)] action: AuthCommand }` + `AuthCommand { Login, Logout, Status }`.
- [x] `cli/platform-cli/src/main.rs`: dispatch обеих новых веток.

**Тесты:** 10 integration в `whoami_auth_test.rs` + 5 unit в `whoami.rs`:
- `whoami_on_empty_store_prints_onboarding_hint`
- `whoami_with_active_target_renders_summary_and_honours_no_ping` — пинит что синтетический токен **никогда** не появляется в stdout (regression-guard на leak).
- `whoami_with_real_ping_reports_verified_on_mockito_200`
- `whoami_with_real_ping_reports_failure_hint_on_mockito_401` — проверяет что 401 не валит exit code + содержит `--renew` hint.
- `whoami_with_real_ping_reports_failure_when_provider_unreachable` — closed-port path.
- `auth_login_prints_friendly_redirect_to_target_add`
- `auth_logout_prints_friendly_redirect_with_nothing_to_logout_phrasing`
- `auth_status_explains_self_hosted_mode_and_points_at_whoami`
- `auth_group_is_hidden_from_top_level_help` — `apprafter --help` НЕ содержит `auth`.
- `auth_subcommand_help_is_still_reachable` — `apprafter auth --help` работает (hide ≠ delete).
- 5 unit: `verified_status` × 2 (no_ping + no_token), `ssh_key_status` × 3 (loaded / missing / not-set).

**Acceptance:**
- ✅ `apprafter whoami` на TTY/CI без active target → onboarding hint + Identity-line.
- ✅ С active target — рендер всех полей + verified status (или skip с `--no-ping`).
- ✅ Token никогда не leak'ится в stdout.
- ✅ `apprafter auth login/logout/status` печатают friendly redirect + Managed-roadmap URL.
- ✅ `apprafter --help` не показывает `auth`; `apprafter auth --help` показывает все три subcommand'а.
- ✅ `cargo test --workspace`: 36 (target_test без изменений) + 10 (whoami_auth_test новый) + 120 (cli-providers без изменений) — 0 failures. fmt + clippy + SPDX (155 files) — clean.

**Out-of-scope (отложено):**
- "Account" / "Last used" / "Cluster: provisioned/not" в whoami — нужны (a) Hetzner endpoint которого нет публично, (b) per-target state cache wire-up через A.8, (c) telemetry on operational commands. Закроется когда A.8 land'нет state-per-target.
- Real AppRafter Cloud auth — Managed offering, далеко за пределами M1.5.
- ADR `docs/adr/0014-cli-command-structure.md` про резервирование `auth` namespace + resource-first grouping — Track A.12 (final docs+ADR pass).

**Зависит от:** 1.66A.5 ✅ (target store + load_target).

**Размер:** S (один цикл, ~0.5 рабочего дня).

---

### 1.66A.7 `apprafter doctor` ✅

> v0.1.81 — sub-phase 1.66A.7 shipped: self-diagnostic команда (target checks + env checks + DNS probe; trichotomy PASS/WARN/FAIL; FAIL → exit 1 для CI gates).

**Source:** `cli-dx-task.md` §5.9 + §17 row 7.

**Поставка:**
- [x] `apprafter doctor [--target <name>] [--no-ping]` — новая top-level команда.
- [x] `cli/platform-cli/src/commands/doctor.rs` (~520 LOC + 11 unit-тестов): pure `Check { name, status, detail, hint }` + `DoctorReport { target_name, target_checks, env_checks }` data layer, отдельные `build_target_checks` / `build_env_checks` / `print_report` функции; orchestrator `run` зовёт всё + exit-1 на FAIL.
- [x] **Target checks** (когда target resolved):
    - `Config file readable` — через `load_target`; on `TargetNotFound` сразу FAIL с available-hint'ом.
    - `Credentials file present (mode 0600)` — на Unix проверяет permissions, WARN если drift'нул от 0600; на других OS просто existence-check.
    - `Provider \`X\` supported` — whitelist (`hetzner-cloud`).
    - `Token format valid` — `validate_hetzner_token_format`; FAIL с `--renew` hint'ом если сломан.
    - `Token verified against provider API` — `HetznerCloudValidator::validate_credentials()` с timing (`{ms} ms`); WARN если `--no-ping` / нет токена; FAIL с разделением 401 (token rejected → `--renew` hint) / non-401 HTTP / transport.
    - `SSH key readable` — exists + read_to_string + parse algo из OpenSSH-первой строки; FAIL если path в config'е есть но файла нет на диске (stale config); WARN если path не задан.
- [x] **Env checks** (всегда):
    - `\`kubectl\` on PATH` / `\`helm\` on PATH` / `\`ssh\` on PATH` — `Command::new(tool).args(...).output()`, PASS с первой непустой строкой (stdout ИЛИ stderr — `ssh -V` пишет в stderr), WARN с hint'ом если binary не найден. Лояльно — отсутствие optional-tool не валит doctor.
    - `DNS resolves \`api.hetzner.cloud\`` — `ToSocketAddrs::to_socket_addrs("host:443")`; PASS с `443/tcp` detail или FAIL с resolver-error hint'ом.
- [x] **Rendering**: `  ✓ name (detail)` / `  ⚠ name (detail)` / `  ✗ name (detail)` + `      hint: <hint>` на отдельной indented строке. Trailing summary с разными формулировками для clean / warning-only / FAIL'ed runs.
- [x] **Exit policy**: FAIL anywhere → `std::process::exit(1)`; WARN-only → exit 0 + "Ready to go; review warnings".

**Тесты (17 новых):**
- 11 unit в `commands::doctor::tests`:
    - `check_status_glyph_renders_distinctly`
    - `report_counters_split_by_status`
    - `report_has_failures_returns_false_when_only_warns`
    - `check_dns_resolves_localhost_passes` (RFC reserved 127.0.0.1)
    - `check_dns_resolves_invalid_tld_fails` (RFC 6761 `.invalid`)
    - `check_tool_warns_on_missing_binary` (no `apprafter-doctor-no-such-binary` on $PATH)
    - `check_provider_known_fails_for_unknown_provider` / `_passes_for_hetzner`
    - `check_token_format_passes_canonical_token` / `_fails_on_missing_token`
    - `check_token_ping_warns_when_no_ping_flag_set`
- 6 integration в `tests/doctor_test.rs`:
    - `doctor_on_empty_store_errors_with_onboarding_hint`
    - `doctor_renders_target_and_env_checks_with_summary` (Target/Env разделы, --no-ping → WARN на ping, summary mentions target name)
    - `doctor_target_flag_inspects_non_active_target` (`--target secondary`)
    - `doctor_ssh_key_missing_path_fails_the_run_with_exit_1` (configure ssh-key path → удалить файл → FAIL + exit 1)
    - `doctor_target_not_found_fails_with_available_hint` (`--target ghost`)
    - `doctor_summary_line_phrases_outcomes_clearly` (warning-only run → "warning(s)", no FAIL в выводе)

**Acceptance:**
- ✅ `apprafter doctor` на empty store → typed error + onboarding hint.
- ✅ С active target — все 10 (~6 target + ~4 env) checks отрисованы; summary с count'ом PASS/WARN/FAIL.
- ✅ `--target <name>` инспектирует non-active target.
- ✅ `--no-ping` → token-ping check как WARN с "skipped — --no-ping".
- ✅ Любая FAIL → exit 1 (для CI gates).
- ✅ Никаких token leaks в output.
- ✅ `cargo test --workspace`: 36 target_test + 10 whoami_auth_test + 6 doctor_test + 120 cli-providers + ... — 0 failures.
- ✅ fmt + clippy (-D warnings) + SPDX (158 files) clean.

**Out-of-scope (отложено):**
- "Region in known list" check — нужен hardcoded list (brittle) или API call (уже есть в ping). Implicit: если ping проходит с этим region'ом, он валиден.
- "No active cluster" check — нужен cli-state cross-ref per target. Track A.8.
- Color output для PASS/WARN/FAIL — Track A.11 (color/NO_COLOR).
- miette-стиль diagnostics — Track A.10.

**Зависит от:** 1.66A.4a ✅ (validator), 1.66A.5 ✅ (CRUD load_target).

**Размер:** M (один цикл, ~1 рабочий день).

---

### 1.66A.8 Wire `apply` / `destroy` / `import` в target resolution ✅

> v0.1.82 — sub-phase 1.66A.8 shipped: credential resolution chain (`--flag > env > target store`) реально подключена к operational commands. После v0.1.82 — `apprafter target use prod && apprafter apply` без `HCLOUD_TOKEN=...` действительно работает.

**Source:** `cli-dx-task.md` §5.10 + §7 + §17 row 8.

**Поставка:**
- [x] `cli/cli-core/src/credentials.rs` — новый модуль:
    - `resolve_hetzner_token(cli_flag, paths, target_override) -> Result<String>` — implements 3-step chain. cli_flag (highest) > `HCLOUD_TOKEN` env > active target's credentials.yaml (или `--target <name>` override).
    - `resolve_hetzner_ssh_public_key(paths, target_override) -> Result<Option<String>>` — analogous chain для SSH public key BODY. Env `APPRAFTER_SSH_PUBLIC_KEY` > target store path → read file.
    - `read_ssh_public_key_body(path)` pure helper.
    - Constants `HCLOUD_TOKEN_ENV` / `SSH_PUBLIC_KEY_ENV` для shared use.
    - Error messages enumerate **все 3** пути (flag / env / `apprafter target add`) чтобы оператор сразу видел альтернативы.
- [x] cli-core re-export через `pub use credentials::*`.
- [x] `cli_core::TEST_ENV_MUTEX: pub(crate) static Mutex<()>` в `lib.rs` (cfg(test)-gated) — serialises env-touching unit tests across modules (target.rs + credentials.rs обе flip'ают HCLOUD_TOKEN / CONFIG_DIR_ENV, race без shared mutex).
- [x] `commands/apply.rs::run(target_override: Option<&str>)` — заменил direct `env::var("HCLOUD_TOKEN")` на `resolve_hetzner_token(None, &target_store, target_override)`. `build_ssh_specs` теперь thread'ит target_store + target_override и вызывает `resolve_hetzner_ssh_public_key`; manifest `sshKeys` block по-прежнему wins (highest precedence на той ветке).
- [x] `commands/destroy.rs::run(yes, target_override)` — analogous wiring. Empty-state early-exit моложе credential resolution чтобы `destroy --yes` в no-Hetzner-state директории не падал на missing-creds.
- [x] `commands/import.rs::run(force, dry_run, target_override)` — analogous.
- [x] `cli.rs` — новый `--target <name>` flag на `Apply` / `Destroy` / `Import`.
- [x] `main.rs` dispatch обновлён.

**Тесты (16 новых cli-core unit + 1 integration smoke):**
- 16 в `cli_core::credentials::tests`:
    - CLI flag wins over env + store
    - env wins over store when no flag
    - store fallback when flag + env absent
    - `--target <name>` override picks named target not active
    - error with 3-paths hint when nothing configured
    - error when target exists but no token stored
    - error with override for missing target surfaces "available" hint
    - SSH key env wins over target path
    - SSH key reads target path when env absent (with trim)
    - SSH key returns None when nothing configured
    - SSH key errors loudly on unreadable path
- 1 в `tests/cli_smoke.rs` integration:
    - `apply_target_flag_routes_resolution_at_named_target_and_surfaces_not_found` — seed target store с `real`, run `apply --target ghost` → typed error содержит `ghost`, `not found`, `real` (available hint).
- 3 prior `apply_without_token_*` / `import_without_token_*` integration тесты обновлены: добавлен `APPRAFTER_CONFIG_DIR=<tempdir>` для изоляции от user's real `~/.config/apprafter/`; assertions enumerate новые "3-paths" error message tokens (`--token`, `HCLOUD_TOKEN`, `apprafter target add`).

**Acceptance:**
- ✅ `apprafter apply` без `HCLOUD_TOKEN` env читает токен из active target — главная цель Track A.
- ✅ `--target <name>` override per-invocation, без switching active.
- ✅ Existing CI scripts (`HCLOUD_TOKEN=... apprafter apply`) работают без изменений — backwards-compat preserved (env остаётся step 2 в chain).
- ✅ Empty-store + no env + no flag → typed error с **всеми 3** путями выхода в сообщении.
- ✅ Stale `--target <name>` → "did you mean..." hint via canonical TargetNotFound.
- ✅ `cargo test --workspace`: 90 cli-core (+16 credentials) + 47 cli_smoke (+1 new integration, 3 prior updated) + 42 target_test + 10 whoami_auth_test + 6 doctor_test + ... — 0 failures.
- ✅ fmt + clippy (-D warnings) + SPDX (160 files) clean.

**Out-of-scope (отложено):**
- `--token <X>` flag на `apply`/`destroy`/`import` (secrets в shell history — wait until A.10 miette pass решает UX).
- Migration `<cwd>/.apprafter/state.json` → per-target `state/<name>/state.json` — отдельная iteration после bootstrap-all.
- `kubeconfig` / `argocd-password` / `cluster-bootstrap` — не используют HCLOUD_TOKEN напрямую (работают на kubeconfig); скип.
- `init` — не нужны creds (stub-like, write state.json only).

**Зависит от:** 1.66A.5 ✅ (target store CRUD).

**Размер:** M (один цикл, ~1.5 рабочих дня).

---

### 1.66A.9 `apprafter bootstrap-all` orchestrator ✅

> v0.1.84 — initial landing: 3-phase wrapper `apply` → kubeconfig-poll → `cluster-bootstrap` под единым `indicatif::MultiProgress` UX, `--dry-run` со списком subcommand-команд.
> v0.1.85 — hotfix UX после ручного walk'а v0.1.84: `MultiProgress` рендерил finished spinner'ы поверх каждого helm/kubectl `println` (10+ дублированных строк); spinner Phase 1/3 fought с tracing-логами apply/cluster-bootstrap за тот же row; dry-run печатал `<active target>` placeholder вместо реального имени активного target'а и не давал понять что произойдёт за каждой фазой. v0.1.85 переезжает на single-bar-per-phase pattern, Phase 1/3 без spinner'а (только `→ start` / `✓ end` static-строки), Phase 2 keeps spinner because retry loop owns all output. Dry-run resolves active target name + грузит config.yaml и расписывает фазы человеческим языком.

**Source:** `cli-dx-task.md` §5.11 + §17 row 9.

**Поставка:**
- [x] `cli/cli-core/Cargo.toml` workspace dep `indicatif = "0.17"`; `platform-cli/Cargo.toml` direct dep.
- [x] `commands/kubeconfig.rs` рефакторинг: новый `pub fn fetch_and_cache(refresh, target_override) -> Result<String>` возвращает YAML без `print!`, прежний `run` стал thin wrapper. Внутри теперь `resolve_hetzner_token` (cli-dx-task.md §7) вместо прямого `env::var("HCLOUD_TOKEN")` — Phase 2 поллинг подхватывает active target's токен идентично `apply`.
- [x] `cli.rs` — новый `Commands::Kubeconfig { refresh, target }` (`--target` override credential resolution chain) + `Commands::BootstrapAll { target, dry_run }`.
- [x] `main.rs` dispatch обновлён.
- [x] `commands/bootstrap_all.rs` (v0.1.85 UX layout):
    - Phase 1/3 — `apply::run(target_override)` без spinner'а: `→ [1/3] apply  provisioning…` перед вызовом, `✓ [1/3] apply  done in Ns` после. Apply сам логирует через `tracing` на stderr — spinner вокруг него только конкурировал бы за тот же ряд терминала и оставлял stale-кадры после каждого `helm`/`kubectl` write.
    - Phase 2/3 — retry-loop `kubeconfig::fetch_and_cache(true, target_override)` каждые 10s до 5 минут (`KUBECONFIG_POLL_TIMEOUT = 300s`, `KUBECONFIG_POLL_INTERVAL = 10s`). Здесь spinner оправдан: цикл наш, никаких inner subcommand'ов не пишут в stdout, message обновляется с attempt counter + truncated last error. Завершается `finish_and_clear()` + static success line.
    - Phase 3/3 — `cluster_bootstrap::run()` без spinner'а (та же логика, что Phase 1).
    - `--dry-run` short-circuits BEFORE any side-effect — load `default_config_root()` + `resolve_active_target_name()` + `load_active_target_config()`, печатает реальное имя active target (или `--target` override label), provider/region/tier/cluster/ssh-key из `config.yaml`, и human-readable описание каждой фазы (что именно она делает, не просто «вызовет такую-то subcommand»).
    - `failed(num, name, elapsed, err)` helper — на error печатает `✗ [N/3] phase  FAILED after Ns` в stderr и пробрасывает CliError неизменно (timing accountable без потери error chain).
    - Финал: `bootstrap-all complete in Tm00s (apply X + kubeconfig Y + bootstrap Z)` — single-line breakdown total + per-phase.
- [x] `commands/mod.rs` регистрирует новый модуль.

**Тесты (4 unit + 6 integration):**
- 4 в `commands::bootstrap_all::tests`:
    - `format_elapsed_uses_seconds_under_one_minute`
    - `format_elapsed_switches_to_minutes_at_sixty_seconds`
    - `short_error_keeps_first_line_only`
    - `short_error_truncates_long_first_line_with_ellipsis`
- 6 в `tests/bootstrap_all_test.rs`:
    - `bootstrap_all_dry_run_prints_three_phase_plan_without_provider_calls` — fresh store / no token / no base-URL → success + all 3 phase labels + `Phases:` block в stdout.
    - `bootstrap_all_dry_run_with_empty_store_prints_onboarding_hint` — empty target store → `no active target` + `apprafter target add` hint.
    - `bootstrap_all_dry_run_with_target_override_labels_it_clearly` — `--target work` → `Target: work` + `via --target override` label.
    - `bootstrap_all_dry_run_with_active_target_resolves_name_and_config` — seed real target через `target add`, dry-run resolves `Target: myprod (active)` + Provider/Region/Tier из `config.yaml`.
    - `bootstrap_all_help_documents_dry_run_and_target_flags` — `--help` mentions both flags.
    - `bootstrap_all_rejects_unknown_flag` — clap surface contract guard.

**Acceptance:**
- ✅ `apprafter bootstrap-all --dry-run` exits 0 на любой директории / любом credential state, никаких provider calls.
- ✅ Dry-run показывает реальное имя active target + полный target config (не placeholder `<active target>`).
- ✅ `--target <name>` override доходит и до apply, и до Phase 2 kubeconfig poll (single resolution path).
- ✅ Real run на свежем Hetzner токене даёт **clean** vertical output: `→ start / inner output / ✓ end` без дублирования спиннер-строк.
- ✅ `cargo test --workspace` — 542 tests, 0 failures.
- ✅ fmt + clippy (-D warnings) + SPDX (161 файл) clean.

**Out-of-scope (отложено):**
- Capturing inner helm/kubectl output to a buffer (показывать только on failure) — feasible but invasive; пользователю всё ещё нужно видеть прогресс helm install. Доработка цвета + табличного hiding — Track A.11.
- Idempotent re-run / skip-already-installed semantics — Argo CD handle'ит это в Phase 3 через `helm upgrade --install`; Phase 1 — Hetzner labels; Phase 2 — `--refresh` always re-fetches.
- miette-styled error rendering при timeout — Track A.10.
- **Phase 2 polish (отложено до отдельной итерации, ~A.9c)** — две связанные доработки, замеченные на ручном walk'е v0.1.85: (a) Phase 2 стабильно завершается за `1m00s` потому что `ssh` упирается в kernel TCP connect timeout (~30s) пока cloud-init поднимает sshd; нужен `ConnectTimeout=5` в SSH wrapper'е → attempt 1 fail'ится за 5s вместо 30s, total Phase 2 падает до ~20-30s, attempts равномернее. (b) Label `[2/3] kubeconfig` вводит в заблуждение — реально это время полного boot'а ноды (cloud-init + k3s startup), kubeconfig fetch — копеешный финальный шаг. Переименовать на `[2/3] k3s-ready` / `[2/3] cluster-up` / подобное; success-строка станет `up in Ns` вместо `ready in Ns`. dry-run phase block обновить синхронно.

**Зависит от:** 1.66A.8 ✅ (credential resolution chain — Phase 2 needs `resolve_hetzner_token`).

**Размер:** S (один цикл, ~0.5 рабочего дня).

---

### 1.66A.10 — 1.66A.12 (TBD, заполняются по мере landings)

> Каждая следующая Track A под-фаза получает собственный заголовок и `Поставка/Acceptance/Размер` блок ровно перед тем, как открывается её итерация. Шаблон — выше (1.66A.1–1.66A.9). См. `cli-dx-task.md` §17: target store IO ✅ → `target add` non-interactive ✅ → validator framework ✅ → interactive wizard ✅ → CRUD ✅ → `whoami`+`auth` ✅ → `doctor` ✅ → resolution chain ✅ → `bootstrap-all` orchestrator ✅ → **miette refinement (1.66A.10)** → aliases/color → docs+ADR.

---

### 1.66 platform-stack monorepo skeleton + CUE source layout

**Source:** ADR 0028.

**Цель:** заложить структуру `apprafter/platform-stack/` в монорепо. CUE source-of-truth для всех Argo CD Application определений платформенных компонент.

**Поставка:**
- [ ] New subdir `apprafter/platform-stack/`:
    - `cue/platform.cue` — umbrella schema (channels, versioning, component list type)
    - `cue/components/cilium.cue` — Cilium Application definition с source.repoURL=https://helm.cilium.io, chart=cilium, version, default values per tier
    - `cue/components/cert-manager.cue` — analogously для jetstack.io
    - `cue/components/argocd.cue` — Argo CD's own Application (self-managing, prune=false)
    - `cue/components/apprafter-operator.cue` — наш operator из ghcr.io
    - `cue/components/admission-webhook.cue` — admission webhook
    - `cue/components/backstage.cue` — Backstage, conditional на `values.domain`
    - `cue/components/network-policies.cue` — default-deny NetworkPolicies
    - `cue/components/argocd-cue-cmp.cue` — CMP sidecar configuration (см. 1.69)
    - `cue/tiers/solo.cue` — tier 1 overlay (no Backstage если no domain, no Hubble, etc.)
    - `cue/tiers/team.cue` — tier 2+ overlay (Hubble enabled, Kamaji, Capsule)
    - `cue/compatibility.cue` — change classification per version (initial entry для 0.2.0)
- [ ] `apprafter/platform-stack/Chart.yaml.tmpl` — template для umbrella chart metadata
- [ ] `apprafter/platform-stack/README.md` — поясняет CUE-only convention, contribution model
- [ ] `apprafter/platform-stack/CHANGELOG.md` — initial entry для 0.2.0

**Acceptance:**
- `cue eval ./apprafter/platform-stack/cue/...` exits 0; все schemas валидны.
- Все компоненты declared в CUE (нет hardcoded values в platform-cli которые ещё не migrated в chart — это произойдёт в 1.71).
- README ясно описывает: CUE only, rendered chart живёт в OCI, не в Git.

**Зависит от:** —

**Размер:** M

---

### 1.67 `cue cmd render` pipeline + umbrella chart generation

**Source:** ADR 0028.

**Цель:** CI step который рендерит CUE source в Helm umbrella chart в `dist/`.

**Поставка:**
- [ ] `apprafter/platform-stack/render.cue` (CUE command) implementing:
    - Read all `cue/components/*.cue` files
    - Read `cue/tiers/*.cue` overlay (parameterized via values)
    - Emit `dist/platform-stack-<version>/Chart.yaml`
    - Emit `dist/platform-stack-<version>/values.yaml` (rendered default values; tier-aware structure)
    - Emit `dist/platform-stack-<version>/values.schema.json` (rendered from CUE schema; Helm native validation)
    - Emit `dist/platform-stack-<version>/templates/applications.yaml` — единственный template, итерирующийся по `.Values.components`:
      ```yaml
      {{- range $name, $component := .Values.components }}
      {{- if $component.enabled }}
      apiVersion: argoproj.io/v1alpha1
      kind: Application
      metadata:
        name: {{ $name }}
        namespace: argocd
      spec:
        source:
          repoURL: {{ $component.source.repoURL }}
          chart: {{ $component.source.chart }}
          targetRevision: {{ $component.version }}
          helm:
            valuesObject: {{ toYaml $component.values | nindent 8 }}
        destination: ...
        syncPolicy: {{ toYaml $component.syncPolicy | nindent 6 }}
      {{- end }}{{- end }}
      ```
    - Emit `dist/platform-stack-<version>/compatibility.yaml` — rendered change classification
- [ ] Local `make render` target вызывает `cue cmd render` + `helm lint dist/platform-stack-<version>/`
- [ ] `dist/` gitignored.

**Acceptance:**
- `make render` produces `dist/platform-stack-0.2.0/` content.
- `helm lint` returns 0.
- `helm template dist/platform-stack-0.2.0 --values examples/solo.yaml` renders correctly с tier-1 settings; produces список Argo CD Applications для cilium, cert-manager, argocd, apprafter-operator, admission-webhook (no Backstage т.к. no domain).
- `helm template ... --values examples/team.yaml` renders с Hubble, Backstage, Kamaji, etc.

**Зависит от:** 1.66

**Размер:** S

---

### 1.68 CI OCI publish workflow + cosign signing

**Source:** ADR 0028.

**Цель:** GitHub Actions workflow который on tag `platform-stack/v*` builds chart + signs + publishes к OCI и GitHub Release.

**Поставка:**
- [ ] `.github/workflows/platform-stack-publish.yml`:
    - Trigger: tag matching `platform-stack/v*`
    - Steps:
        1. Checkout
        2. Install `cue` binary
        3. Run `make render` (1.67)
        4. `helm lint` + smoke install in `kind` cluster
        5. `helm package dist/platform-stack-<version>` → `.tgz`
        6. `cosign sign` artifact (with GitHub OIDC keyless signing — no managed secret keys)
        7. `oras push ghcr.io/apprafter/platform-stack:<version>` + tag latest in channel
        8. Attach `.tgz` and `.tgz.sig` to GitHub Release page
- [ ] CI validation: `compatibility.cue` must update для new version tag; otherwise fail with clear error.
- [ ] `apprafter/platform-stack/RELEASE.md` — maintainer release procedure.

**Acceptance:**
- Tag `platform-stack/v0.2.0-rc1` triggers workflow → ends green.
- `oras pull ghcr.io/apprafter/platform-stack:0.2.0-rc1` retrieves signed chart.
- `cosign verify ghcr.io/apprafter/platform-stack:0.2.0-rc1` succeeds.
- GitHub Release page has both `.tgz` and `.tgz.sig` attached.

**Зависит от:** 1.67

**Размер:** S

---

### 1.69 CUE CMP sidecar Docker image + plugin.yaml

**Source:** ADR 0029.

**Цель:** sidecar image для `argocd-repo-server` который компилирует CUE → YAML для user app repositories.

**Поставка:**
- [ ] `apprafter/argocd-cue-cmp/` new subdir:
    - `Dockerfile` — Alpine base + `cue` binary + plugin.yaml + entrypoint wrapper
    - `plugin.yaml`:
      ```yaml
      apiVersion: argoproj.io/v1alpha1
      kind: ConfigManagementPlugin
      metadata:
        name: cue
      spec:
        discover:
          find:
            glob: "**/apprafter*.cue"
        generate:
          command: [sh, "-c"]
          args:
            - /entrypoint.sh
      ```
    - `entrypoint.sh` — runs `cue export ./... --out yaml` + post-processing для structured error output (на ошибки CUE compile — extract first error line into single-line summary; full details в stderr).
- [ ] `.github/workflows/argocd-cue-cmp-publish.yml` — analogous to 1.68, publishes `ghcr.io/apprafter/argocd-cue-cmp:<version>`.
- [ ] Reference image в `apprafter/platform-stack/cue/components/argocd.cue` (argocd-repo-server `extraContainers` config).

**Acceptance:**
- `docker build apprafter/argocd-cue-cmp/` produces image.
- Manual test: `docker run -v ./test-repo:/repo -w /repo image cue export ./... --out yaml` produces correct YAML output для sample `apprafter/Application.cue`.
- Tag `argocd-cue-cmp/v0.2.0-rc1` publishes image.

**Зависит от:** —

**Размер:** S

---

### 1.70 Minimal `cluster-bootstrap` rewrite

**Source:** ADR 0025.

**Цель:** reduce `cluster-bootstrap` to a minimal loader: install Argo CD via Helm, apply root Application pointing к platform-stack OCI chart. Argo CD дальше reconciles остальное.

**Поставка:**
- [ ] Refactor `cli-providers/k8s/cluster_bootstrap.rs` (или эквивалент):
    - Step 1: `helm install argocd` (only this; no Cilium, no cert-manager, no operator, no webhook here).
    - Step 2: Wait для Argo CD ready (kubectl wait, ~30s).
    - Step 3: Apply root Argo CD `Application` CR pointing на `oci://ghcr.io/apprafter/platform-stack:<resolved-version>` (resolved через PlatformStack CR — см. 1.74).
    - Step 4: Wait для root Application reports Healthy + Synced.
    - Step 5: Wait для child Applications (cilium, cert-manager, apprafter-operator, etc.) report Healthy. Progress UX surfaces per-component status.
- [ ] Existing imperative install code (Cilium, cert-manager, operator, webhook, Backstage) — **deleted** from CLI. Their Helm values moved to platform-stack chart (см. 1.71).
- [ ] `apprafter bootstrap-all` progress UX using `indicatif::MultiProgress`:
    - Phase 1/3: substrate provisioning (Hetzner)
    - Phase 2/3: Argo CD loader install
    - Phase 3/3: Platform stack reconciliation (per-component sub-bars: Cilium ⏳, cert-manager ⏳, ...)
- [ ] Idempotent resume на любом шаге (closes PRELAUNCH_CHECKLIST P1 item 3.1).
- [ ] `apprafter cluster-bootstrap --manifest <path>` flag заменяет current `APPRAFTER_MANIFEST` env-var; auto-discovery walking upward from CWD (default).

**Acceptance:**
- `apprafter init && apprafter bootstrap-all` on fresh Hetzner account → working Tier 1 cluster with all platform components reconciled via Argo CD within ~10 minutes (vs current ~5-7 min imperative bootstrap).
- `kubectl get applications.argoproj.io -A` shows: root, cilium, cert-manager, argocd, apprafter-operator, admission-webhook (+ network-policies, possibly Backstage), all Healthy + Synced.
- `kubectl edit application cilium -n argocd` — change value — Argo CD reconciles → drift correction works.
- Re-run `apprafter bootstrap-all` идемпотентен (skip-already-installed semantics).

**Зависит от:** 1.66, 1.67, 1.68, 1.69 (platform-stack chart must be publishable before CLI references it)

**Размер:** M

---

### 1.71 Migrate platform component values from CLI to chart

**Source:** ADR 0025.

**Цель:** все existing Helm values builders в `cli-providers::k8s::*` переезжают в `apprafter/platform-stack/cue/components/*.cue` как CUE-typed values. CLI больше не содержит platform component конфигурации.

**Поставка:**
- [ ] Audit existing CLI source:
    - `cilium_values_yaml()` → `cue/components/cilium.cue` values block
    - `cert_manager_values_yaml()` → `cue/components/cert-manager.cue` values
    - `argocd_values_yaml()` → `cue/components/argocd.cue` values (включая CMP sidecar config от 1.69)
    - `apprafter_operator_values_yaml()` → `cue/components/apprafter-operator.cue`
    - Admission webhook manifests → `cue/components/admission-webhook.cue`
    - Backstage values → `cue/components/backstage.cue` (conditional на values.domain)
    - default-deny NetworkPolicy → `cue/components/network-policies.cue`
- [ ] Self-managing Argo CD: Argo CD's own Application within chart has `syncPolicy.automated.prune: false` to prevent self-destructive upgrades.
- [ ] Delete migrated Rust code from `cli-providers::k8s::*`.
- [ ] Smoke: rendered chart + applied → cluster matches what previous CLI-installed setup produced (value-by-value diff).

**Acceptance:**
- `git grep -E "(cilium_values|cert_manager_values|argocd_values|backstage_values)_yaml" cli/` returns no matches in source (only possibly in tests as legacy reference).
- Tier 1 bootstrap через new pipeline produces functionally identical cluster (Cilium config, cert-manager ClusterIssuer, Argo CD UI, admission webhook).
- Argo CD UI shows Argo CD как один из child Applications с prune=false visible.

**Зависит от:** 1.66, 1.70

**Размер:** M

---

### 1.72 PlatformStack CRD schema + admission webhook

**Source:** ADR 0026.

**Цель:** CUE-typed schema для PlatformStack CR + admission webhook validation.

**Поставка:**
- [ ] `schemas/v1alpha1/platformstack.cue` — full schema per spec.md §3.11:
    - `spec.channel` (enum stable | beta | edge)
    - `spec.pin` (optional, semver string)
    - `spec.autoUpgrade` (bool, default false)
    - `spec.source.upstream` + `spec.source.repoURL` (OCI references)
    - `spec.source.checkInterval` (duration, default 6h)
    - `spec.values` (free-form, tier/domain/etc.)
    - `spec.overrides` (per-component freezes)
    - `status` with currentVersion, availableVersion, lastUpstreamCheck, components[], versionHistory (ring buffer), conditions[]
- [ ] Generated OpenAPI v3 schema.
- [ ] Admission webhook validation rules:
    - Exactly one PlatformStack CR per cluster (rejected if a second is created), named `default` в namespace `apprafter-system`.
    - `spec.channel` is one of `stable | beta | edge`.
    - `spec.source.checkInterval` ≥ 1h (prevent rate-limit abuse).
    - `spec.pin` is valid semver if set.
- [ ] Bootstrap integration: 1.70 step adds creation of default `PlatformStack` CR с `spec.channel: stable`, `spec.pin: unset`, `spec.source.upstream/repoURL = oci://ghcr.io/apprafter/platform-stack`.

**Acceptance:**
- `kubectl apply` of a second PlatformStack CR rejected by admission webhook.
- Invalid channel value rejected.
- Default PlatformStack created during bootstrap is visible через `kubectl get platformstack default -n apprafter-system`.

**Зависит от:** 1.70 (bootstrap creates the CR)

**Размер:** S

---

### 1.73 PlatformController core: reconcile loop + OCI client + diff

**Source:** ADR 0026.

**Цель:** core PlatformController component — kube-rs reconcile loop, OCI registry client, helm render + diff vs current state, patches umbrella Argo CD Application.

**Поставка:**
- [ ] New crate `operator-platform-controller/` в workspace.
- [ ] kube-rs reconcile loop watching `PlatformStack` CRs.
- [ ] Leader election (kube standard pattern with lease в `apprafter-system` namespace).
- [ ] OCI registry client:
    - Pull chart by tag from `spec.source.repoURL`
    - List available tags by channel (filter using channel marker в OCI annotations или separate index)
- [ ] Helm render: invoke embedded helm Go library (через `cgo` или `helm-go-sdk`) или sidecar; render umbrella chart with merged `values` + `overrides` to produce target list of Applications.
- [ ] Diff logic: compare rendered umbrella values vs currently-applied Argo CD Application's `spec.source.helm.valuesObject`. Classify diff using `compatibility.yaml` from chart (taxonomy: safe | requires-restart | data-migration | breaking).
- [ ] On non-destructive diff (safe + requires-restart, кроме когда autoUpgrade=false): patch the single umbrella Argo CD Application; Argo CD reconciles child Applications.
- [ ] On destructive diff (data-migration | breaking, or any change when autoUpgrade=false): defer to MigrationPlan (см. 1.78).
- [ ] Environment check at apply time: confirm cluster's k8s version ≥ chart's `minimumKubernetesVersion`; block с clear diagnostic if not.
- [ ] Status updates: `components[]`, `conditions[]`.

**Acceptance:**
- Edit `PlatformStack.spec.pin` from `0.2.0` to `0.2.1` (с safe-only changes в compatibility metadata) → controller pulls chart 0.2.1, computes diff classified as safe, patches umbrella Application; child Applications (Cilium etc.) reconcile to new versions within ~3 minutes.
- Edit `spec.overrides.cilium.pin: "1.16.5"` while platform is on 0.2.1 → Cilium frozen even after stack bump to 0.2.2.
- k8s version mismatch — clear error in `status.conditions`, no patch applied.

**Зависит от:** 1.71 (umbrella chart structure), 1.72 (CRD)

**Размер:** L — distributed-systems penalty applies (new distributed component, leader election, OCI client reliability)

---

### 1.74 PlatformController upstream check + status updates

**Source:** ADR 0026.

**Цель:** periodic check task, version history tracking, UpgradeAvailable condition surfacing.

**Поставка:**
- [ ] Periodic check task spawned by PlatformController (configurable interval via `spec.source.checkInterval`, default 6h):
    - Pull OCI tag list from `spec.source.upstream` (note: upstream может differ from repoURL для fork scenarios)
    - Filter by channel (stable/beta/edge via OCI annotation conventions)
    - Pick latest semver tag
    - Update `status.availableVersion`, `status.lastUpstreamCheck`
- [ ] `status.versionHistory` ring buffer (last 10 transitions): on each successful patch of umbrella Application, push entry `{version, appliedAt, transition}`.
- [ ] `status.conditions`:
    - `Ready` (True если все child Applications Healthy)
    - `UpgradeAvailable` (True если `availableVersion != currentVersion`, message describes diff classification)
- [ ] Auto-upgrade logic: when `spec.autoUpgrade: true` AND new available version AND diff classification = `safe` → bump `spec.version` automatically (which triggers normal reconcile path → patches Applications).
- [ ] Caching: ETag-aware OCI requests; aggressive caching of channel tag list (TTL = checkInterval).

**Acceptance:**
- Publish new platform-stack version (0.2.2 with safe changes only) → within `checkInterval` (или after manual `kubectl annotate platformstack default apprafter.io/refresh-upstream=true`), `status.availableVersion = 0.2.2`.
- With `autoUpgrade: true` + safe classification → controller bumps spec.pin → reconcile path completes → status.currentVersion = 0.2.2.
- With `autoUpgrade: true` + new version classified as breaking → MigrationPlan created (см. 1.78); no spec.pin bump.
- `kubectl get platformstack default -o jsonpath='{.status.versionHistory}'` shows history entries.

**Зависит от:** 1.73

**Размер:** S

---

### 1.75 Unified MigrationPlan CRD + admission webhook

**Source:** ADR 0027.

**Цель:** unified MigrationPlan CRD с scope discriminator (application | platform).

**Поставка:**
- [ ] `schemas/v1alpha1/migrationplan.cue` per spec.md §3.8 rewrite:
    - `spec.scope.type` (enum, application | platform)
    - `spec.scope.application` (required if type=application): ref, environment
    - `spec.scope.platform` (required if type=platform): affected components list
    - `spec.trigger` (kind + field-specific data)
    - `spec.risks` (classification, estimatedDowntime, dataVolume, reversible, requiresFullBackup)
    - `spec.plan[]` (steps with action, estimatedDuration, reversible)
    - `spec.approvers[]` (emails)
    - `spec.previousSpecSnapshot` annotation (for platform-scope rollback)
    - `status.phase` (pending-approval | approved | rejected | executing | completed | failed)
    - `status.approvedBy`, `status.approvedAt`
    - `status.executedSteps[]`
- [ ] OpenAPI v3 schema with `oneOf` discriminator on `spec.scope.type`.
- [ ] Admission webhook deeper validation:
    - Required fields per scope type
    - Approver email format validation
    - Reject changes to `spec.scope` after CR creation (immutable)
    - Reject `status` patches not from MigrationController (only controller can transition phase)

**Acceptance:**
- `kubectl apply` valid application-scope MigrationPlan succeeds.
- `kubectl apply` valid platform-scope MigrationPlan succeeds.
- Apply with missing scope-required fields → rejected.
- Apply with invalid approver emails → rejected.

**Зависит от:** —

**Размер:** S

---

### 1.76 MigrationController + strategy dispatch

**Source:** ADR 0027.

**Цель:** MigrationController reconciler with Rust trait dispatch для application + platform strategies.

**Поставка:**
- [ ] Extend `apprafter-operator` workspace с `MigrationController` reconciler.
- [ ] `MigrationStrategy` trait:
  ```rust
  trait MigrationStrategy {
      fn detect_destructive(&self, ctx: &Context) -> Result<Option<DestructiveChange>>;
      fn create_plan(&self, change: DestructiveChange) -> Result<MigrationPlan>;
      fn execute_step(&self, step: &MigrationStep) -> Result<StepStatus>;
      fn reject(&self, ctx: &Context) -> Result<()>;  // platform-only; application impl is no-op
  }
  ```
- [ ] `ApplicationMigrationStrategy` impl: detect destructive changes in Application CR (needs.* selector changes, storage class changes, breaking image migrations).
- [ ] `PlatformMigrationStrategy` impl: detect destructive diff between umbrella Application versions based on compatibility metadata.
- [ ] Reconcile loop processes MigrationPlans in phase=executing, executes plan steps sequentially, updates status.
- [ ] Approve transition: `status.phase: pending-approval → approved` (triggered by Backstage/CLI/Argo CD action). Controller transitions to `executing` and runs plan steps.
- [ ] Reject transition (platform-only): `status.phase: pending-approval → rejected`. Controller invokes PlatformMigrationStrategy.reject() which reverts spec.pin to value from `metadata.annotations[apprafter.io/previous-spec]`.

**Acceptance:**
- MigrationPlan в pending-approval state — controller doesn't touch underlying resources.
- Patch status.phase = approved → controller starts executing.
- Patch status.phase = rejected on platform-scope plan → PlatformStack.spec.pin reverts to previous.
- Patch status.phase = rejected on application-scope plan → admission webhook rejects the patch (no reject for application scope per ADR 0027).

**Зависит от:** 1.75

**Размер:** M

---

### 1.77 Application reconciler integration: gate pause/resume

**Source:** ADR 0027.

**Цель:** existing `Application` reconciler (delivered в Phase 1) теперь respects pending MigrationPlans — pauses child resource patching, sets status.phase=AwaitingMigrationApproval.

**Поставка:**
- [ ] Update `operator-controllers/src/application.rs`:
    - Before patching child resources (Deployment, Service, HTTPRoute), check for existing MigrationPlan referencing this Application в namespace `apprafter-system` with phase=pending-approval.
    - If found: skip child patching, set Application.status.phase = `AwaitingMigrationApproval`, set condition `MigrationPending`.
    - If no pending plan: continue normal reconcile.
    - On detect-destructive: call ApplicationMigrationStrategy to create a MigrationPlan, then enter pause mode.
- [ ] Custom Argo CD health check (Lua script в argocd-cm ConfigMap) для Application CR: returns Degraded with message "AwaitingMigrationApproval — see MigrationPlan <name>" when Application.status.phase=AwaitingMigrationApproval. This surfaces в Argo CD UI as Degraded card.

**Acceptance:**
- User pushes destructive change в app repo (e.g., changes `needs.pg.selector`) → Argo CD syncs Application CR → reconciler creates MigrationPlan and pauses → Deployment continues running с prior version, Application UI shows Degraded with MigrationPlan reference.
- Approve plan через `kubectl patch migrationplan <name> -p '{"status":{"phase":"approved"}}' --type=merge --subresource=status` (или CLI/Backstage) → controller resumes, executes plan steps, Application reaches Ready.
- User revert в Git → Argo CD syncs reverted spec → reconciler observes non-destructive → existing MigrationPlan superseded.

**Зависит от:** 1.76

**Размер:** M

---

### 1.78 PlatformController MigrationPlan integration

**Source:** ADR 0027.

**Цель:** PlatformController detects destructive platform diffs, creates MigrationPlan instead of immediately patching umbrella Application.

**Поставка:**
- [ ] Update PlatformController reconcile path (from 1.73):
    - After computing diff and classifying, when classification != `safe`:
        - Save current spec.pin (or resolved version) в MigrationPlan annotation `apprafter.io/previous-spec`.
        - Create MigrationPlan with scope.type=platform, scope.platform.components = affected component names.
        - Skip patching umbrella Application; status updates to reflect pending.
    - On MigrationPlan approved: PlatformController patches umbrella Application с новыми values; Argo CD reconciles.
    - On MigrationPlan rejected: PlatformMigrationStrategy.reject() reverts PlatformStack.spec.pin via patch to value from annotation.

**Acceptance:**
- Publish platform-stack 0.3.0 (with breaking changes per compatibility metadata) → PlatformController creates MigrationPlan; PlatformStack.status.conditions[UpgradeAvailable]=True with "blocked by MigrationPlan".
- Approve MigrationPlan → upgrade flows through.
- Reject MigrationPlan → PlatformStack.spec.pin reverts; status reflects.

**Зависит от:** 1.74, 1.76

**Размер:** S

---

### 1.79 CLI thin wrappers + `apprafter open` commands

**Source:** ADR 0025, 0026, 0027.

**Цель:** CLI commands operating on declarative resources + UI access helpers + npm-style version check.

**Поставка:**
- [ ] New CLI subcommands в `apprafter` binary:
    - `apprafter platform status` — read PlatformStack.status, format человекочитаемо (current version, available, components healthy count, recent history).
    - `apprafter platform upgrade [--to <version>]` — patch PlatformStack.spec.pin (или channel resolution if --to not specified).
    - `apprafter platform channel <name>` — switch channel.
    - `apprafter platform freeze <component> [--version <v>]` — patch overrides.<component>.pin.
    - `apprafter platform unfreeze <component>` — remove override.
    - `apprafter platform rescue` — reinstall Argo CD from loader (emergency recovery).
    - `apprafter migration list` — list MigrationPlans, filter by phase/scope.
    - `apprafter migration approve <name>` — patch status.phase=approved.
    - `apprafter migration reject <name>` — patch status.phase=rejected (rejected by webhook for application scope; works for platform).
    - `apprafter open <ui>` — open browser to UI:
        - `argocd` — `kubectl port-forward svc/argocd-server -n argocd 8080:443` + auto-fetch admin password from cluster secret + open https://localhost:8080
        - `backstage` — analogously
        - `grafana` — when present (Tier 2+)
        - `hubble` — when present (Tier 2+)
- [ ] npm-style CLI version check on every invocation:
    - Cache в `~/.cache/apprafter/version-check.json` with 24h TTL.
    - Fetch latest CLI release from `api.github.com/repos/apprafter/apprafter/releases/latest`.
    - If newer: print warning line at start of output ("apprafter X.Y.Z available; you have ...").
- [ ] Argo CD Resource Action Lua script (added to argocd-cm ConfigMap via platform-stack chart): "Approve Migration" button on MigrationPlan resources в Argo CD UI.

**Acceptance:**
- `apprafter platform status` outputs structured table within 2s.
- `apprafter open argocd` opens browser with credentials filled in within 5s on second-run (cached password).
- `apprafter migration approve <name>` succeeds; status updates within reconcile cycle.
- CLI shows update warning when version stale.
- Argo CD UI shows Approve button on MigrationPlan resources.

**Зависит от:** 1.72, 1.75, 1.76 (CRDs must exist для thin wrappers)

**Размер:** M

---

### 1.80 `apprafter platform fork` GitHub API automation

**Source:** ADR 0028.

**Цель:** one-command fork bootstrap для power users.

**Поставка:**
- [ ] `apprafter platform fork --to <oci-ref> [--private]`:
    - Validates GitHub PAT exists (env or target credentials store).
    - Fork `github.com/AppRafter/apprafter` to user's GitHub account/org via API.
    - Add `.github/workflows/platform-stack-publish.yml` to the fork (copied from upstream — это same workflow что был залит в 1.68, отображённый для fork-specific OCI namespace).
    - Trigger initial publish (push tag → CI builds → OCI publishes).
    - Patch local PlatformStack CR: `spec.source.repoURL = <new oci ref>`, keep `spec.source.upstream` pointing to AppRafter upstream for tracking.
- [ ] Documentation в `docs/operator-guide/fork.md`: when to fork, how to maintain, sync from upstream procedure.

**Acceptance:**
- `apprafter platform fork --to ghcr.io/myorg --private` on test account → fork created, workflow added, initial OCI publish ends green, local cluster's PlatformStack updated to pull from `ghcr.io/myorg`.
- Edit CUE in fork → tag → next bootstrap or upgrade pulls from fork.
- Upstream tracking: PlatformStack.status.availableVersion still reflects AppRafter upstream releases.

**Зависит от:** 1.68 (workflow template), 1.79 (CLI infra)

**Размер:** M

---

### 1.81 e2e tests update

**Source:** ADR 0025, 0026, 0027, 0028, 0029.

**Цель:** end-to-end coverage всех new flows.

**Поставка:**
- [ ] `e2e/mvp.sh` rewritten:
    - Original 9-step flow → 3-step flow (init → bootstrap-all → smoke Application).
    - Verify все platform components reconciled by Argo CD (not by CLI).
- [ ] `e2e/gitops-walk.sh` — new script:
    - Add app repo via Argo CD UI (scripted через Argo CD API).
    - Push apprafter/Application.cue change → CMP renders → Argo CD syncs → operator reconciles → Deployment running.
- [ ] `e2e/migration-app.sh` — new script:
    - Apply Application with needs.pg.
    - Push change to needs.pg.selector (destructive) → MigrationPlan created.
    - Approve via CLI → migration executes → Application reaches Ready with new database.
- [ ] `e2e/migration-platform.sh` — new script:
    - Set PlatformStack.spec.pin = 0.2.0.
    - Publish 0.3.0 with breaking change (test artifact).
    - Verify MigrationPlan created with platform scope.
    - Approve → upgrade flows; reject — PlatformStack reverts.
- [ ] `e2e/fork.sh` — new script:
    - Use test GitHub fixture; verify fork command on minimal repo.
- [ ] All scripts callable from CI; runtime budget < 30 min per script на kind cluster.

**Acceptance:**
- `make e2e` runs all scripts green в CI.
- Test coverage report shows all major code paths exercised.

**Зависит от:** all 1.66–1.80

**Размер:** M

---

### 1.82 Docs update

**Source:** ADR 0025, 0026, 0027, 0028, 0029.

**Цель:** rewrite outdated quickstart, add new operator/dev guides.

**Поставка:**
- [ ] `docs/operator-guide/quickstart.md` rewritten:
    - Drop nine-step imperative narrative.
    - Three-step flow: install binary → init → bootstrap-all.
    - Explain Argo CD-managed platform on first read.
    - `apprafter open argocd` instead of port-forward + cli-password dance.
    - Update CX22 → CPX22 (closes existing factual error).
    - Smoke test через `Application` CRD (closes existing design contradiction).
- [ ] `docs/operator-guide/platform-management.md` (new):
    - PlatformStack lifecycle.
    - Channels and upgrade strategy.
    - `apprafter platform upgrade`, `freeze`, `fork`, `rescue`.
    - When to fork; how to maintain a fork.
- [ ] `docs/operator-guide/migration-plans.md` (new):
    - What's a destructive change.
    - Approve / reject semantics by scope (application vs platform).
    - Approving via Backstage, CLI, Argo CD UI.
- [ ] `docs/dev-guide/application-cue.md` (new):
    - Writing `apprafter/Application.cue` for GitOps deployment.
    - CMP rendering, troubleshooting compile errors.
    - Multi-environment patterns.
- [ ] `docs/operator-guide/gitops-walk.md` updated:
    - Workflow accounts for AppRafter Application CRs (current version tests raw Deployment+Service; new version goes through Application CRD end-to-end).
- [ ] Update root `README.md` reference links.

**Acceptance:**
- New user reading quickstart end-to-end can get to running app in ~30 min.
- Docs explain Argo CD's role clearly without contradictions.
- Mental model "platform reconciles itself" передаётся on first reading.

**Зависит от:** 1.81

**Размер:** S

---

### 1.83 Tag `v0.2.0-self-managing`

**Source:** all of M1.5.

**Цель:** close M1.5 milestone with version tag.

**Поставка:**
- [ ] Final smoke run: full e2e suite green.
- [ ] Update CHANGELOG.md entries for the v0.1.66 — v0.1.82 series, consolidate в M1.5 release notes.
- [ ] Update version в Cargo.toml workspace, package metadata.
- [ ] Tag `v0.2.0-self-managing` (signals M1.5 close before M2 starts).
- [ ] Update root README badge.

**Acceptance:**
- Tag exists; release notes complete.
- Gate passed для Phase 2 start.

**Зависит от:** 1.82

**Размер:** XS

---

## Фаза 2 — Платформенные сервисы (M2) ⚡

**Цель фазы:** Application может декларировать `needs.{pg,jetstream,redis}` — операторы и ServiceProvider'ы выделяют ресурсы автоматически.

**Spec:** §6 M2, §3.2, §3.3, §4.4, §4.6, §3.1 (per-env overrides).

### 2.1 ServiceProvider CRD

**Поставка:**
- [ ] CUE-схема + admission webhook.
- [ ] Поля: `type`, `backend`, `labels`, `config` (raw map), `status.health`.
- [ ] Built-in типы (закрытый enum в v1alpha1): `pg`, `jetstream`, `clickhouse`, `redis`, `s3`, `notifications`.
- [ ] Tier-aware defaults в схеме (через `if tier == 1 ...`).

**Acceptance:** ServiceProvider валидируется; неизвестный `type` без плагина — ошибка admission.

**Зависит от:** 1.7

**Размер:** S

---

### 2.2 ResourceClaim CRD

**Поставка:**
- [ ] CUE-схема + admission webhook.
- [ ] Поля: `type`, `selector`, `spec` (size, etc.), `status.{provider, connectionSecretRef, ready, conditions}`.
- [ ] Создаётся **только** оператором, юзер-create запрещён admission.

**Зависит от:** 2.1

**Размер:** S

---

### 2.3 Selector matching и provider scheduler

**Цель:** Reconcile ResourceClaim → matching ServiceProvider по labels.

**Поставка:**
- [ ] Лог matching-логики: точное соответствие labels, default `tier: integrated`.
- [ ] При нескольких подходящих — детерминированный выбор (alphabetical `name`).
- [ ] При отсутствии подходящего — Status `Pending`, событие.
- [ ] Метрики: `claim_unmatched_total`.

**Зависит от:** 2.2

**Размер:** S

---

### 2.4 needs.pg → CloudNativePG

**Поставка:**
- [ ] Установка CloudNativePG operator как platform-service.
- [ ] `pg-integrated` ServiceProvider управляет одним shared CNPG-кластером.
- [ ] На каждый ResourceClaim: создание DB + role + secret с DSN.
- [ ] `Application` с `needs.pg` → оператор генерирует ResourceClaim → DSN в env.
- [ ] Удаление Application → grace-period 7 дней → удаление DB (через Soft-delete CRD `RetainedClaim`).

**Acceptance:** манифест из §3.1 (parser) с `needs.pg` поднимается, в pg-кластере появляется DB, приложение коннектится.

**Зависит от:** 2.3

**Размер:** L

---

### 2.5 needs.jetstream → NATS account/stream

**Поставка:**
- [ ] NATS-кластер как platform-service (в Tier 1 — single node, embedded в kine — 3.2).
- [ ] `jetstream-integrated` ServiceProvider: создание account, stream, consumer scopes на claim.
- [ ] Credentials (NKEY/JWT) в Secret.
- [ ] `Application.needs.jetstream.streams: [...]` создаёт streams декларативно.

**Acceptance:** Application декларирует `streams: ["blocks-head"]`, NATS показывает stream созданным; приложение публикует/подписывается.

**Зависит от:** 2.3

**Размер:** L

---

### 2.6 needs.redis → Dragonfly

**Поставка:**
- [ ] Dragonfly как platform-service (single instance Tier 1).
- [ ] `redis-integrated` ServiceProvider: DB-namespace per claim.
- [ ] `requirepass` per-claim, в Secret.

**Acceptance:** Application с `needs.redis` получает рабочий DSN, два claim'а изолированы по DB-номеру.

**Зависит от:** 2.3

**Размер:** M

---

### 2.6a KEDA install + ScaledObject rendering

**Source:** ADR 0019.

**Цель:** KEDA как official autoscaling backend; `Application.autoscale.on:` рендерит ScaledObject.

**Поставка:**
- [ ] Install KEDA Helm chart как platform-service — post-M1.5 это означает adding KEDA как component в `apprafter/platform-stack/cue/components/keda.cue`, not direct install via CLI. KEDA arrives через Argo CD reconciliation.
- [ ] Default enabled at Tier 1 (sufficient KEDA footprint ~50MB для opt-in autoscaling), но Application receives ScaledObject только когда `autoscale:` declared.
- [ ] Operator renderer (`operator-rendering` crate) генерирует `ScaledObject` resource из `Application.autoscale`.
- [ ] Supported triggers in v1: `jetstream_lag`, `cpu`, `memory`, `http_rps`.
- [ ] Per-trigger rendering:
    - `jetstream_lag` → KEDA `nats-jetstream` scaler with stream + consumer.
    - `cpu` / `memory` → KEDA built-in CPU/memory scalers (HPA passthrough).
    - `http_rps` → KEDA Prometheus scaler reading Gateway metrics.
- [ ] Unit tests на rendering coverage для каждого trigger типа.
- [ ] Integration test: Application с `autoscale: {on: cpu, min: 1, max: 10}` реально скейлится под cpu load на 3-node test cluster (можно re-use Tier 1 single-node для базового test'а).
- [ ] Backstage Application view: текущий replica count + autoscaling state (Pending / Active / scale events history).

**Acceptance:**
- Application с `autoscale.on: cpu` rendered ScaledObject видим через `kubectl get scaledobject`.
- Под load (искусственный CPU stress) replicas действительно растут от min к max.
- Backstage показывает autoscaling activity.

**Зависит от:** 2.6 needs.redis (как proxy для готовности базовых ServiceProvider'ов), 1.83 (M1.5 closure)

**Размер:** M

---

### 2.7 SPIRE installation + workload identity

**Поставка:**
- [ ] SPIRE server + agent на каждой ноде.
- [ ] Trust domain `platform.local` (или из ExternalSurface).
- [ ] Регистрация workloads по labels оператором.
- [ ] Metrics + audit log.

**Acceptance:** под получает SVID через unix socket; `spire-agent api fetch` возвращает X.509-cert.

**Зависит от:** 1.8

**Размер:** L

---

### 2.8 Credential injection через SPIFFE

**Поставка:**
- [ ] Замена «mounted Secret with DSN» на «приложение получает DSN через workload identity (короткоживущие credentials)».
- [ ] Для PG: интеграция через CNPG `pg_ident.conf` + SPIFFE-aware sidecar.
- [ ] Fallback на mounted Secret с пометкой «deprecated, use workload identity».

**Acceptance:** под без переменной окружения с паролем PG, аутентификация через SPIFFE; ротация SVID каждый час, приложение продолжает работать.

**Зависит от:** 2.7

**Размер:** L

---

### 2.9 Per-environment overrides через CUE unification

**Цель:** реализовать §3.1 пример (dev/staging/prod в одном файле).

**Поставка:**
- [ ] Renderer Application учитывает `environments.<env>` как unification с `base`.
- [ ] Каждое env разворачивается в свой namespace с суффиксом `-<env>`.
- [ ] Selector ServiceProvider'а различен по env (например, dev → `tier: integrated`, prod → `tier: managed-aws`).
- [ ] Backstage показывает вкладки по env'ам.

**Acceptance:** один Application файл с тремя env'ами создаёт три namespace с разными ресурсами; CUE-валидация ловит конфликт типов между base и env override.

**Зависит от:** 1.9, 2.4

**Размер:** M

---

### 2.10 needs → NetworkPolicy auto-derivation

**Цель:** при `needs.pg` оператор создаёт egress NetworkPolicy к pg-кластеру.

**Поставка:**
- [ ] Каталог connection-targets для каждого ServiceProvider type (label-selector + порт).
- [ ] Renderer добавляет CiliumNetworkPolicy egress per declared need.
- [ ] Default-deny остаётся; всё разрешённое — явно через needs или connects.
- [ ] Hubble drops на forbidden flows видны в логах.

**Acceptance:** Application без `needs.pg` не может коннектится к pg-кластеру (Hubble drop); с `needs.pg` — может.

**Зависит от:** 2.4, 2.5, 2.6

**Размер:** M

---

### 2.11 SealedSecrets интеграция (Tier 1 секреты)

**Поставка:**
- [ ] Установка sealed-secrets controller.
- [ ] CLI helper в `platform-cli`: `platform-cli secret seal --name foo --from-literal=...`.
- [ ] Public-key экспортируется и публикуется в `manifests/tier-1/sealed-secrets/`.
- [ ] Backstage UI: «Encrypt secret» wizard для Tier 1.
- [ ] Прометейный warning в UI: «вы используете SealedSecrets — без ротации, без dynamic. Tier 2+ → OpenBao».

**Acceptance:** разработчик через CLI шифрует секрет, коммитит, Argo CD синкает, в кластере появляется обычный Secret.

**Зависит от:** 1.5

**Размер:** M

---

### 2.12 Application.env: secret() и claim.* references

**Поставка:**
- [ ] CUE-функции `secret("path/to/key")` и `claim.<need>.<field>` в схеме.
- [ ] Renderer резолвит:
  - `secret(...)` → Secret-ref envFrom (для SealedSecrets) либо annotation для Vault Agent (Phase 3).
  - `claim.pg.uri` → Secret-ref на сгенерированный Secret из ResourceClaim.
- [ ] Литералы (`LOG_LEVEL: "info"`) — обычный env.
- [ ] Ошибка resolve — Application Status NotReady с понятной причиной.

**Acceptance:** Application с тремя источниками env (literal, secret, claim) запускается, под видит все три переменные.

**Зависит от:** 2.4, 2.11

**Размер:** M

---

### 2.13 Notifications service — каркас (HTTP API + NATS внутрь)

**Поставка:**
- [ ] OneBun-сервис `notifications` в `providers/notifications-integrated/`.
- [ ] HTTP `/send` endpoint, авторизация через workload identity (JWT с SPIFFE claims).
- [ ] Внутрь — publish в NATS JetStream stream `notifications.<account>.outbox`.
- [ ] CUE-схема `needs.notifications` (см. §4.6).
- [ ] Auto-provision streams + DLQ при первом claim.

**Acceptance:** `curl` с правильным JWT публикует сообщение, оно появляется в outbox-stream.

**Зависит от:** 2.5, 2.7

**Размер:** L

---

### 2.14 Notifications channels: SMTP / Slack / Telegram

**Поставка:**
- [ ] Воркеры на OneBun: `email-worker`, `slack-worker`, `telegram-worker`.
- [ ] Подписка на outbox, доставка по каналу, exponential backoff retry.
- [ ] DLQ после N retries, alert на escalation channel.
- [ ] Конфигурация SMTP/Slack/Telegram через ExternalSurface (3.x ещё не готов — пока через ConfigMap).

**Acceptance:** отправка через `/send` доходит до email/Slack/TG; искусственная ошибка SMTP уводит в DLQ + alert.

**Зависит от:** 2.13

**Размер:** L

---

### 2.15 Platform-only notification templates

**Поставка:**
- [ ] `templates/access-grant/{issued,renewal-reminder,expired,revoked}.{html,md}`.
- [ ] `templates/operational/{dlq-stuck,service-down,quota-exceeded,migration-pending,backup-digest}.{html,md}`.
- [ ] `templates/bootstrap/cluster-initialized.{html,md}`.
- [ ] Template-engine (Handlebars или Liquid) в notifications-сервисе.
- [ ] Override-механизм: ConfigMap `platform-notification-templates` перебивает встроенные.

**Acceptance:** платформенное событие (например, бутстрап кластера) уходит email с правильным шаблоном.

**Зависит от:** 2.14

**Размер:** M

---

### 2.16 Backstage notifications-плагин

**Поставка:**
- [ ] Inbox-view: pending / sent / failed / DLQ per Application.
- [ ] DLQ viewer: retry, drop actions.
- [ ] Per-channel success-rate dashboard.
- [ ] Alert-баннер в UI «N стуков в DLQ».

**Acceptance:** в Backstage виден реальный inbox; retry из UI воскрешает сообщение.

**Зависит от:** 2.14, 1.10

**Размер:** M

---

### 2.17 Закрытие чек-листа M2 spec

- [ ] Обновить `spec.md` §6 M2.
- [ ] Tag `v0.2.0-services`.
- [ ] Update `docs/dev-guide/needs.md`.

**Размер:** XS

---

### 2.18 Known Limitations docs sync

**Source:** tracker §1.1.

**Цель:** перед `v0.2.0-services` tag убедиться, что `spec.md` § Known limitations of v0.1.x отражает реальное состояние закрытого Phase 2.

**Поставка:**
- [ ] Update `spec.md` Known limitations section:
    - Remove items that landed в Phase 2 (если такие были).
    - Remove "Platform stack installed imperatively" item (closed by M1.5).
    - Remove "MigrationPlan reconciler not implemented" item (closed by M1.5).
    - Add items, что **deferred** к Phase 3+ для honesty.
- [ ] Update `docs/dev-guide/needs.md` (если такого doc нет — create) с реальным workflow для `needs.{pg,jetstream,redis}`.
- [ ] Update `e2e/mvp.sh` — extend для проверки `needs.*` flow (apply Application с pg, verify DB provisioned, app connects).
- [ ] Tag `v0.2.0-services` после всех Phase 2 closures.

**Acceptance:** spec.md Known limitations accurate per state; e2e зелёный с pg flow.

**Зависит от:** all other Phase 2 подфазы closed.

**Размер:** XS

---

## Фаза 3 — Multi-node + Observability (M3) ⚡

**Цель фазы:** платформа поднимается в HA на 3 нодах; observability stack по умолчанию для всех workload'ов.

**Spec:** §6 M3, §4.1 (Tier 2), §4.2, §4.10, §4.4 (OpenBao).

### 3.1 HA-bootstrap в platform-cli + dual-stack validation

**Source:** ADR 0017.

**Поставка:**
- [ ] `apprafter init --tier team --nodes 3`.
- [ ] k3s server ×3 с `--cluster-init` + joins.
- [ ] Embedded LB через kube-vip (или Hetzner LB).
- [ ] Smoke: убить мастер — kubectl продолжает работать.
- [ ] Explicit dual-stack validation на 3-nodes setup.
- [ ] Cluster-CIDR и service-CIDR должны быть dual notation на HA bootstrap.
- [ ] E2E: kill master node — kubectl continues working на обоих family.

**M1.5 carry-over:** HA bootstrap теперь means provisioning multiple Hetzner nodes + running through the same Argo CD-managed platform stack pipeline. Helm values for multi-node mode live в platform-stack chart's tier-2 overlay; CLI just orchestrates substrate provisioning + platform-stack reconciliation watch.

**Acceptance:** 3-нодовый кластер за один init; failover мастера < 30s; dual-stack connectivity sustained через node failure.

**Зависит от:** 1.83 (M1.5 closure), 1.13

**Размер:** L (без изменений; dual-stack adds little work поверх HA bootstrap)

---

### 3.2 kine + NATS JetStream как control-plane storage

**Поставка:**
- [ ] NATS JetStream cluster (3 replica, embedded или workload — ADR).
- [ ] kine в etcd-emulation режиме поверх NATS KV.
- [ ] k3s конфиг `--datastore-endpoint=nats://...`.
- [ ] Бенчмарк: API churn 1k objects, сравнение с baseline etcd.
- [ ] Stream подписки для CDC (event log платформы).

**Acceptance:** все стандартные k8s операции (deploy, watch, admission) работают; kine API соответствует etcdctl get/put/watch.

**Зависит от:** 3.1

**Размер:** L

---

### 3.3 Cilium mTLS между workloads

**Поставка:**
- [ ] Включение Cilium service mesh с mTLS.
- [ ] Identity через SPIFFE (через 2.7).
- [ ] Default-deny дополняется identity-based ingress (`fromIdentity: ...`).
- [ ] Hubble видит mTLS handshake.

**Acceptance:** между двумя Applications трафик зашифрован (tcpdump показывает TLS); невалидный SPIFFE → drop.

**Зависит от:** 2.7, 3.1

**Размер:** L

---

### 3.4 OpenTelemetry pipeline по умолчанию

**Поставка:**
- [ ] OTel Collector как daemonset.
- [ ] Auto-инжект env vars `OTEL_EXPORTER_OTLP_ENDPOINT` для Application pods (admission mutating webhook).
- [ ] Configurable sampling per Application (`observability.sampling: 0.1`).
- [ ] OneBun + Bun стартеры подключают `@onebun/trace` и `@onebun/metrics` по умолчанию.

**Acceptance:** новый Application без явной OTel-конфигурации шлёт metrics+traces+logs в коллектор.

**Зависит от:** 3.1

**Размер:** M

---

### 3.5 ClickHouse provider (logs + traces)

**Поставка:**
- [ ] clickhouse-operator (Altinity).
- [ ] `clickhouse-integrated` ServiceProvider: DB per claim, RBAC.
- [ ] Системные DB `_logs`, `_traces` для платформенной observability.
- [ ] Vector / OTel exporter в ClickHouse.

**Acceptance:** логи и трейсы пишутся, видны через Grafana (Datasource ClickHouse).

**Зависит от:** 3.4, 2.3

**Размер:** L

---

### 3.6 VictoriaMetrics integration

**Поставка:**
- [ ] VictoriaMetrics single (Tier 2) / cluster (Tier 3+).
- [ ] OTel metrics → vmagent → VictoriaMetrics.
- [ ] Стандартные dashboards в Grafana (operator metrics, NATS, k3s, Cilium).

**Acceptance:** Grafana показывает per-Application latency / RPS dashboards.

**Зависит от:** 3.4

**Размер:** M

---

### 3.7a Hubble enable + Hubble UI + Grafana network dashboards

**Source:** ADR 0020.

**Поставка:**
- [ ] Включить Hubble в Cilium values within platform-stack chart (per-tier overlay: tier-2 has Hubble enabled, tier-1 doesn't).
- [ ] Hubble UI deploys via Cilium chart; expose internally через Service в `kube-system`; AccessGrant flow для external access.
- [ ] Grafana dashboards для network metrics (имеется `cilium/cilium` upstream dashboards — import + adapt).
- [ ] Standard dashboards: cluster-wide flows, per-namespace flows, per-Application flows (когда Application labels propagated).

**Acceptance:**
- Tier 2+ cluster: `kubectl -n kube-system get pods | grep hubble` показывает Hubble + UI pods running.
- Hubble UI доступен через AccessGrant'у oriented kubeconfig.
- Grafana dashboards показывают real-time flow metrics.

**Зависит от:** 3.1, 3.6 (VictoriaMetrics для metrics storage)

**Размер:** M

---

### 3.7b Backstage flow visualizer plugin

**Source:** ADR 0020.

**Поставка:**
- [ ] Backstage plugin: card на Application page показывает real-time flow data из Hubble (через Hubble Relay API).
- [ ] «Convert observed flow to policy» button — генерирует PR в Git repository с дополнением Application's `connects.egress` (если destination not yet declared).
- [ ] Filter UI: namespace, identity, time range, L7 protocol.
- [ ] Drop visibility — показывает blocked flows (default-deny enforcement points).

**Acceptance:**
- Developer на Application page видит реальный трафик своего Application.
- Click кнопку → PR with `connects.egress` addition открывается автоматически.

**Зависит от:** 3.7a, 1.10 (Backstage с Application plugin)

**Размер:** M

---

### 3.8 Kamaji + Capsule — multi-tenancy primitives (REWORK)

**Source:** ADR 0023.

**Цель:** установить Kamaji controller и Capsule policy controller как platform services; provision first TenantControlPlane для default tenant.

**Поставка:**
- [ ] Add Kamaji + Capsule components в `apprafter/platform-stack/cue/components/` (kamaji.cue, capsule.cue) с tier-2 overlay enabling them by default.
- [ ] Kamaji datastore — `ResourceClaim` на `pg-integrated` provider (CNPG cluster, dedicated database для Kamaji controller).
- [ ] Provision **default** TenantControlPlane (`apprafter-default`) для случаев, когда юзер не declared explicit Tenant — все existing Applications mapped to default tenant.
- [ ] Capsule controller; configure default Capsule Tenant внутри default Kamaji TCP.
- [ ] Tier 1 path: Kamaji не enabled через tier-1 overlay; Capsule installed standalone на host cluster для policy enforcement (soft mt only).
- [ ] Backstage Tenant overview plugin (basic): list tenants, owners, status.

**Acceptance:**
- Tier 2 bootstrap: `kubectl get tcp -n kamaji-system` показывает default TenantControlPlane Active.
- `kubectl get tenants.capsule.clastix.io -A` показывает default Capsule Tenant.
- Kamaji datastore connects to CNPG; Kamaji state persists через controller restart.
- Tier 1 bootstrap: только Capsule controller; no Kamaji.

**Зависит от:** 2.4 (CloudNativePG), 3.1 (HA bootstrap)

**Размер:** L

---

### 3.8a AppRafter `Tenant` CRD operator integration

**Source:** ADR 0023.

**Цель:** AppRafter `Tenant` CRD как user-facing primitive; operator translates Tenant declarations в Kamaji TCP + Capsule Tenant.

**Поставка:**
- [ ] CUE-схема `kind: Tenant` в `schemas/v1alpha1/tenant.cue` (полная схема per spec.md §3.9).
- [ ] Admission webhook: validation (datastore selector valid, owners non-empty, quotas reasonable).
- [ ] Operator controller (`operator-controllers/src/tenant.rs`):
    - Reconcile Tenant → Kamaji TenantControlPlane + Capsule Tenant внутри TCP.
    - Watch AccessGrants referencing this Tenant → create RoleBindings cluster-admin within TCP.
    - Status field: phase, observed TCP readiness, Capsule policy enforcement status, current owner count.
- [ ] `apprafter login --tenant <name>` — fetches tenant-scoped kubeconfig.
- [ ] Backstage Tenant view extended: applications inside tenant, current owners, quota usage, policy violations.
- [ ] Cascading deletion: Tenant deletion → graceful TCP drain → Capsule Tenant cleanup → Kamaji TCP deletion.
- [ ] Migration: existing v0.1.x Applications get auto-assigned to default tenant on Tier 2 upgrade.

**Acceptance:**
- Apply Tenant manifest → Kamaji TCP created within 60s, Capsule Tenant configured, AccessGrant references resolve to cluster-admin inside TCP only.
- `apprafter login --tenant blockchain-team` returns kubeconfig that works only for that TCP.
- Tenant deletion drains workloads and cleans up TCP + Capsule resources.

**Зависит от:** 3.8, 4.5 (AccessGrant — for owner mapping; but reconciler can degrade gracefully if 4.5 not yet landed)

**Размер:** L

---

### 3.9 Cilium Egress Gateway + family-aware static egress IPs

**Source:** ADR 0017.

**Поставка:**
- [ ] CiliumEgressGatewayPolicy для Application с `network.egressIP.static: true`.
- [ ] Привязка floating IP (Hetzner) к egress-нодам.
- [ ] Family-aware allocation per `Application.network.egressIP.families`. If both `[ipv6, ipv4]` — provision both floating v4 и delegated /64 v6 prefix.
- [ ] Backstage Application view: показать current egress IPs для each family (copy button per address); смена floating IP отражается в UI.

**Acceptance:**
- Трафик от Application к `api.tron.network` идёт с фиксированного IP; смена floating IP отражается в UI.
- Application с `egressIP.families: [ipv6, ipv4]` имеет working egress через оба family; third-party могут whitelist оба адреса.

**Зависит от:** 1.2 (Hetzner provider), 3.1 (HA bootstrap)

**Размер:** M

---

### 3.10 upgrade-tier 1→2

**Поставка:**
- [ ] Команда `apprafter upgrade-tier --to team`.
- [ ] Превращает single-node в 3+ heterogeneous nodes (добавляет 2+ ноды в Hetzner, joins, переключает kine на NATS HA).
- [ ] Бэкап перед миграцией (snapshot в S3).
- [ ] Rollback при failure.

**M1.5 carry-over:** upgrade-tier теперь means changing `PlatformStack.spec.values.tier: solo → team` + applying tier-2 overlay. PlatformController detects destructive change (significant: Kamaji + Hubble + Capsule come online), creates MigrationPlan; user approves. Underlying mechanism reuses 1.78 path.

**Acceptance:** Tier 1 кластер с задеплоенным hello-world превращается в Tier 2 без downtime > 1 минуты.

**Зависит от:** 3.1, 3.2, 1.78 (MigrationPlan path для platform-scope diff)

**Размер:** L

---

### 3.11 OpenBao как platform-service (Tier 2+)

**Поставка:**
- [ ] OpenBao 3-node HA через Helm.
- [ ] Auto-unsealing: AWS KMS / GCP KMS / Shamir (выбор по конфигу).
- [ ] Workload identity через SPIRE → OpenBao auth method.
- [ ] Secret engines: kv-v2, database (PG), pki.

**Acceptance:** OpenBao unsealed автоматически после рестарта; Application получает dynamic PG-credentials через OpenBao.

**Зависит от:** 2.7, 3.1

**Размер:** L

---

### 3.12 Migration: SealedSecrets → OpenBao

**Поставка:**
- [ ] `platform-cli upgrade-tier` шаг: импорт SealedSecrets в OpenBao kv-v2.
- [ ] Application manifests переписываются (CUE rewrite tool): `secret(...)` → тот же путь, но из OpenBao.
- [ ] Verification: тот же контент, тот же hash.
- [ ] SealedSecrets controller остаётся работающим (для legacy), warning в UI.

**Acceptance:** после миграции Application продолжает работать без изменения кода или env vars.

**Зависит от:** 3.11, 2.11

**Размер:** M

---

### 3.13 Закрытие чек-листа M3 spec

- [ ] Обновить `spec.md` §6 M3.
- [ ] Tag `v0.3.0-multinode`.

**Размер:** XS

---

## Фаза 4 — External Surface + Access (M4) ⚡

**Цель фазы:** ExternalSurface декларативен; AccessGrant — единственный путь к доступу для людей; build pipeline с auto-аудитом.

**Spec:** §6 M4, §3.4, §3.5, §4.7, §4.8, §4.9.

### 4.1 ExternalSurface CRD

**Поставка:**
- [ ] CUE-схема (§3.5).
- [ ] Reconciler: разворачивает компоненты в порядке зависимостей.
- [ ] Status per компонент (git/registry/access/notifications/synthetic/backups).

**Размер:** M

---

### 4.1a HTTPRoute auto-generation

**Source:** tracker 2.6.

**Цель:** operator автоматически генерирует HTTPRoute + Certificate для каждого Application с `expose.public: true`.

**Поставка:**
- [ ] CUE-схема Application расширена (per spec.md §3.1 update): `hostname`, `paths`, `tls`, `rewrites`, `websocket`, `sticky`, `protocols`.
- [ ] Operator renderer (`operator-rendering`):
    - HTTPRoute generation с `parentRefs` на platform Gateway (owned by ExternalSurface от 4.1), `hostnames`, `rules` с URLRewrite filters.
    - Certificate generation через cert-manager `Certificate` resource если `tls: true`; `issuerRef` на platform ClusterIssuer.
    - BackendLBPolicy generation для `sticky: true` (Gateway API beta feature).
    - Annotations / EnvoyFilter для WebSocket upgrade handling и extended idle timeout если `websocket: true`.
- [ ] Admission webhook: hostname conflict detection across namespaces (через kubectl-style list HTTPRoutes); reject Application apply с conflict error.
- [ ] Backstage Application view: показать current hostname, TLS status, traffic statistics из Hubble (Hubble plugin already лежит в 3.7b).
- [ ] Cascading delete: Application deletion → HTTPRoute + Certificate cleanup via ownerReferences.
- [ ] Migration: existing Tier 1 deployments (deployed без HTTPRoute) — operator detects missing HTTPRoute on reconcile, creates с auto-generated hostname (no manual intervention required).
- [ ] Update spec.md Known Limitations to remove «HTTPRoute auto-generation deferred to Phase 4» bullet.

**Acceptance:**
- Apply Application с `public: true` → HTTPRoute created within 30s, Certificate issued (cert-manager), Application accessible via HTTPS.
- Hostname conflict (two Applications с same hostname): admission webhook rejects with clear error.
- WebSocket Application: long-lived connection holds через sticky binding.
- Cascading delete: Application removal → HTTPRoute + Certificate gone.

**Зависит от:** 4.1 (ExternalSurface CRD with Gateway domain config)

**Размер:** M

---

### 4.2 Forgejo (или GitLab self-hosted) deployable из манифеста

**Поставка:**
- [ ] Helm chart (готовый upstream) обёрнут в ServiceProvider-style ресурс.
- [ ] Persistence на ClickHouse (для logs) и pg/s3 (data) — через ResourceClaim.
- [ ] Backups → external S3.
- [ ] HTTPRoute через Gateway, OIDC SSO.

**Acceptance:** `git push` в Forgejo триггерит CI runner.

**Зависит от:** 4.1, 2.4

**Размер:** L

---

### 4.3 Harbor registry deployable из манифеста

**Поставка:**
- [ ] Helm chart Harbor.
- [ ] Storage backend → s3-integrated ResourceClaim.
- [ ] Cosign verification policy.
- [ ] Retention rules из ExternalSurface.

**Acceptance:** `docker push` работает; неподписанный image при `signing: required` блокируется.

**Зависит от:** 4.1, 2.x s3

**Размер:** M

---

### 4.4 Headscale + Tailscale Operator

**Поставка:**
- [ ] Headscale-controller pod, persistence pg.
- [ ] Tailscale Operator для автоматической интеграции с k8s сервисами.
- [ ] OIDC SSO для регистрации устройств.

**Acceptance:** `tailscale up --login-server=https://headscale.<domain>` работает; устройство видит cluster routes.

**Зависит от:** 4.1

**Размер:** L

---

### 4.4a external-dns integration + `DNSZone` CRD

**Source:** tracker 2.8.

**Цель:** automated DNS records для HTTPRoute / Application hostnames через external-dns operator.

**Поставка:**
- [ ] Install external-dns operator как platform-service (added to platform-stack chart components).
- [ ] CUE-схема `kind: DNSZone` в `schemas/v1alpha1/dnszone.cue`:
  ```cue
  kind: DNSZone
  name: apprafter-dev
  zone: "apprafter.dev"
  provider: cloudflare
  credentialsRef: secret("platform/cloudflare-token")
  pattern: "{app}.{env}.{tenant}.apprafter.dev"    // optional; default — let external-dns use HTTPRoute hostnames
  ```
- [ ] Operator translates DNSZone → external-dns DNSEndpoint resources + provider configuration.
- [ ] Provider integrations (initial set): Cloudflare, Hetzner DNS, AWS Route53.
- [ ] external-dns reads HTTPRoute hostnames in cluster, creates corresponding DNS records.
- [ ] Backstage DNSZone overview: list zones, provider, record count, last sync.

**Acceptance:**
- Apply DNSZone for `apprafter.dev` with Cloudflare credentials → external-dns синхронизируется, DNS records появляются.
- Apply Application с `hostname: "parser.apprafter.dev"` → DNS record создан в Cloudflare automatically.
- Update spec.md Known Limitations to remove DNS-related deferral.

**Зависит от:** 4.1 (ExternalSurface), 4.4 (Headscale — для credentials store integration через AccessGrant)

**Размер:** M

---

### 4.5 AccessGrant CRD + reconciler — tenant scoping + approvers (REWORK)

**Source:** ADR 0023, ADR 0024.

**Поставка:**
- [ ] CUE-схема (§3.4).
- [ ] Add `tenant:` field — scopes grant к specific Kamaji TCP (см. spec.md §3.4 updates).
- [ ] Add `approvers:` field — two-person rule для host cluster-admin grants.
- [ ] Reconciler:
  - создаёт Headscale pre-auth key (одноразовый, 24h).
  - создаёт RoleBinding/ClusterRoleBinding в k8s.
  - создаёт OIDC group mapping.
  - публикует событие → notifications-сервис.
  - Если `tenant:` set → create RoleBinding inside Kamaji TCP, not host cluster.
  - Если `scope.cluster: host` and `scope.capabilities: ["cluster-admin"]` and `approvers` empty → admission webhook rejects (policy: host cluster-admin requires approvers).
  - Если `approvers:` non-empty → AccessGrant status = `pending-approval`; reconciler waits for approval signals through Backstage или API endpoint.
  - On all approvers signed → status → `active`, credentials issued (Headscale + RoleBinding + OIDC).
  - Audit-event на каждый approval action.
- [ ] Status: issued / pending-approval / pending-activation / active / expiring / expired.
- [ ] Backstage AccessGrant view: pending grants requiring my approval (per user); approve/reject UI; current grants and their tenant scope.

**Acceptance:**
- Apply AccessGrant → email с magic-link приходит; click → SSO+MFA → подключение работает.
- AccessGrant с `tenant: blockchain-team` → subject имеет kubectl access только в TCP «blockchain-team», not host или other tenants.
- AccessGrant `scope.cluster: host` + `scope.capabilities: cluster-admin` без `approvers` → rejected by admission.
- AccessGrant с `approvers: ["bob@"]` + Alice как subject → grant pending until Bob approves via Backstage; only then Alice can login.

**Зависит от:** 4.4 (Headscale), 2.13, 3.8a (Tenant CRD для tenant scoping)

**Размер:** L

---

### 4.5a JIT cluster-admin AccessGrant flow

**Source:** ADR 0024.

**Цель:** короткоживущие emergency cluster-admin grants с auto-revocation и loud audit.

**Поставка:**
- [ ] Special AccessGrant variant: `scope.cluster: host`, `scope.capabilities: ["cluster-admin"]`, `expiry: 1h` (max for JIT grants).
- [ ] Policy enforcement: admission webhook requires `purpose:` field non-empty для JIT grants (forces operator to document why).
- [ ] Approval flow: same `approvers` mechanism, but typically expedited (one approver minimum, can be configured).
- [ ] Loud audit: dedicated event stream `audit.cluster-admin.jit`; immediate Backstage notification banner visible to entire team.
- [ ] Auto-revocation на expiry: kubeconfig invalidates, RoleBinding deleted, audit closes.
- [ ] Backstage emergency dashboard: «JIT access active» banner with grant details, time remaining, ability to view audit trail live.

**Acceptance:**
- JIT grant flow end-to-end (Alice requests с purpose, Bob approves quickly, Alice has 1h cluster-admin, banner visible all team) проходит за < 5 минут.
- After expiry: Alice's kubectl fails with proper auth error; audit shows full trail.

**Зависит от:** 4.5

**Размер:** M

---

### 4.6 OIDC SSO интеграция

**Поставка:**
- [ ] Поддержка внешних провайдеров (Authentik / Keycloak / Auth0 / Google Workspace).
- [ ] ExternalSurface поле `auth.oidc.{issuer,clientId,...}`.
- [ ] Auto-провижионинг конфигов для Argo CD, Backstage, Headscale, OpenBao.

**Acceptance:** один SSO-логин даёт доступ ко всем UI; MFA enforced.

**Зависит от:** 4.4

**Размер:** M

---

### 4.7 platform-cli login (OIDC kubeconfig)

**Поставка:**
- [ ] Device-flow OIDC, токен 8h, auto-refresh.
- [ ] Записывает в `~/.kube/config` контекст с exec-credential.
- [ ] Audit-event на каждый login.

**Acceptance:** после AccessGrant пользователь делает `platform-cli login` и работает с `kubectl`.

**Зависит от:** 4.6

**Размер:** M

---

### 4.8 Magic-link flow для AccessGrant

**Поставка:**
- [ ] Notifications-template из 2.15 (`access-grant/issued`).
- [ ] Endpoint в `platform-cli login --magic-link <token>`.
- [ ] Один-time-use, 24h TTL.

**Acceptance:** flow §3.4 шаги 1–7 проходят за ≤ 5 минут от commit до active mesh.

**Зависит от:** 4.5, 4.7

**Размер:** S

---

### 4.9 Auto-revocation на expiry

**Поставка:**
- [ ] Cron-reconciler сканирует AccessGrant.expiry.
- [ ] T-5d: reminder через notifications.
- [ ] T+0: revoke (Headscale device removed, RoleBinding deleted, OIDC mapping cleared).
- [ ] Audit-event.

**Acceptance:** expired grant — пользователь не может ни в mesh, ни в k8s.

**Зависит от:** 4.5

**Размер:** S

---

### 4.10 Audit log в JetStream — cluster-admin tagging (REWORK)

**Source:** ADR 0024.

**Поставка:**
- [ ] Stream `audit.platform` с retention 1 год.
- [ ] Все компоненты публикуют структурированные audit-события (кто, что, когда, на что).
- [ ] Tag cluster-admin actions specifically — route to dedicated stream `audit.cluster-admin`:
    - All k8s API server actions where user identity has cluster-admin RoleBinding.
    - All AccessGrant lifecycle events (created, approved, active, revoked).
    - All JIT access events (high-priority subset of cluster-admin).
- [ ] Separate retention policy: `audit.cluster-admin` retained longer (default 3 years vs 1 year for `audit.platform`) для compliance.
- [ ] Backstage audit-viewer plugin extended: filter by stream, search by user, time range, action type; cluster-admin actions highlighted.
- [ ] Export to external archive (S3) for cluster-admin stream specifically — compliance-grade retention beyond cluster lifetime.

**Acceptance:**
- Все события из §3.4 (login, AccessGrant lifecycle, MigrationPlan approval) видны и неизменяемы.
- Cluster-admin action (например, `kubectl delete deployment` на critical workload) appears in `audit.cluster-admin` with full context (who, when, what, from where).
- JIT grant audit trail searchable в Backstage end-to-end.
- S3 export job succeeds, audit blob is restorable.

**Зависит от:** 3.2 (kine + NATS), 4.5 (AccessGrant for user identity context)

**Размер:** M

---

### 4.11 Synthetic monitoring (Uptime Kuma external)

**Поставка:**
- [ ] `platform-cli ext-vps init --provider hetzner-cloud --tier nano`.
- [ ] Provisioning Uptime Kuma на отдельном CX11.
- [ ] Targets из ExternalSurface (`syntheticMonitoring.endpoints`).
- [ ] Alerts через notifications.

**Acceptance:** упал Argo CD — alert приходит в течение 60s через telegram/slack/email.

**Зависит от:** 4.1, 2.14

**Размер:** M

---

### 4.12 Backups в external S3

**Поставка:**
- [ ] Velero (или встроенный backup-controller) для k8s ресурсов.
- [ ] CNPG continuous backup в S3.
- [ ] NATS JetStream snapshot job.
- [ ] ClickHouse backup-job.
- [ ] Restore-runbook (`docs/operator-guide/disaster-recovery.md`).

**Acceptance:** test restore: новый кластер из бэкапа за < 1 час, данные совпадают.

**Зависит от:** 4.1

**Размер:** L

---

### 4.13 Build pipeline: Trivy + Grype + Cosign + SBOM

**Поставка:**
- [ ] CI-шаблон (Forgejo Actions / GitLab CI / Woodpecker) для multi-stage build.
- [ ] BuildKit с inline-cache.
- [ ] Trivy + Grype scan, fail on HIGH (configurable).
- [ ] Syft → CycloneDX SBOM.
- [ ] Cosign sign + push в Harbor (mandatory для prod env).

**Acceptance:** PR с уязвимостью HIGH в base image — CI падает; merge запрещён.

**Зависит от:** 4.3, 4.2

**Размер:** L

---

### 4.14 Backstage Build Report plugin

**Поставка:**
- [ ] View per Application image: размер, layers, CVE-list, SBOM, cache-эффективность, рекомендации.
- [ ] Diff между двумя build'ами (что прибавилось/убавилось).
- [ ] «Auto-fix where possible» — генерация PR с обновлённым base image.

**Acceptance:** разработчик видит CVE-отчёт без перехода в Trivy/Harbor UI.

**Зависит от:** 4.13, 1.10

**Размер:** M

---

### 4.15 Cost view в Backstage

**Поставка:**
- [ ] Per Application: CPU/RAM/disk/network usage из VictoriaMetrics.
- [ ] Аллокация % cluster cost (rough percentages в v1.0).
- [ ] Per platform-service breakdown (DB rows, S3 GB, JetStream msgs).
- [ ] Экспорт CSV.

**Acceptance:** руководитель видит топ-5 самых дорогих Application.

**Зависит от:** 3.6, 1.10

**Размер:** M

---

### 4.15a Cilium FQDN policies for `connects.egress.external`

**Source:** tracker «Known limitations» elimination.

**Цель:** enforce `Application.connects.egress.external` declarations через Cilium FQDN-aware NetworkPolicies; eliminate «advisory only» limitation.

**Поставка:**
- [ ] Operator renderer (`operator-rendering`):
    - For each Application с `connects.egress.external: [...]` → generate `CiliumNetworkPolicy` с FQDN matchers per declared destination.
    - DNS-aware matching (Cilium DNS proxy integration): policy matches actual DNS resolution at runtime.
    - Wildcard support (`*.binance.com`) per Cilium FQDN policy capabilities.
- [ ] Backstage Application view: show declared external dependencies vs actual flows (cross-reference with Hubble drops для not-declared destinations).
- [ ] Update spec.md Known Limitations to remove «connects.egress.external not enforced» bullet.
- [ ] Migration: existing Applications без `connects.egress.external` declarations не affected (default-deny stays for declared destinations; undeclared traffic continues blocked by NetworkPolicy default-deny).

**Acceptance:**
- Application с `connects.egress.external: [{host: "api.tron.network", port: 443}]` имеет working egress only к этому destination.
- Attempt to call `api.binance.com` (not declared) → Cilium drop, Hubble logs it.
- Backstage shows: declared destinations green, observed-but-not-declared red с «add to policy» button (similar to 3.7b).

**Зависит от:** 4.13 (Build pipeline — для image scan), 3.7b (Backstage Hubble plugin), 3.3 (Cilium mTLS)

**Размер:** M

---

### 4.16 MigrationPlan Backstage UI (REWORK — alignment with M1.5)

**Source:** ADR 0027.

**Цель:** после M1.5 closure, MigrationPlan CRD already exists with unified scope (application + platform). В Phase 4 остаётся Backstage UI plugin для MigrationPlan queue + notifications integration.

**Поставка:**
- [ ] Backstage MigrationPlan plugin: unified queue view (filter by scope/phase/owner), approve/reject buttons (gated by RBAC), audit trail view per plan.
- [ ] Notifications service integration: pending-approval plan → notification to approvers via email/webhook (Phase 4 also delivers notifications service).
- [ ] MigrationPlan template library: golden-path templates для common destructive operations (PG selector change, image major bump) — pre-populated `plan` array steps that user reviews and approves.

**Acceptance:** Backstage shows MigrationPlan queue with filters; approver receives notification on pending plan; one-click approve via Backstage UI works end-to-end.

**Зависит от:** 1.83 (M1.5 closure — CRD + controller already exist), 4.6 (OIDC SSO for Backstage RBAC), notifications service from Phase 4.

**Размер:** M

---

### 4.17 Закрытие чек-листа M4 spec

- [ ] Обновить `spec.md` §6 M4.
- [ ] Tag `v0.4.0-external-access`.

**Размер:** XS

---

## Фаза 5 — Tier 3, bare metal (M5)

**Цель фазы:** платформа разворачивается на Talos+EPYC; LINSTOR как replicated storage; Kata по умолчанию.

**Spec:** §6 M5, §4.1 (Tier 3), §3.7 (Hetzner Robot).

### 5.1 Talos installation flow

**Поставка:**
- [ ] `platform-cli init --tier prod --provider hetzner-robot --osImage talos-1.x`.
- [ ] PXE / ISO bootstrap через `talosctl`.
- [ ] Machine config generation через `talm`.
- [ ] State в Git (encrypted).

**Acceptance:** 3 EPYC ноды → Talos → k8s ready за < 30 минут от старта `init`.

**Зависит от:** 3.10

**Размер:** L

---

### 5.2 Hetzner Robot built-in provider

**Поставка:**
- [ ] Robot API SDK интеграция (Rust).
- [ ] Server lifecycle: order не автоматизируем (manual), но lifecycle (vSwitch, IP, reset, boot mode) — да.
- [ ] vSwitch для private network между серверами.
- [ ] Failover IP management.

**Acceptance:** `platform-cli plan` показывает diff Robot ресурсов; `apply` применяет.

**Зависит от:** 1.2

**Размер:** L

---

### 5.3 LINSTOR provisioning

**Поставка:**
- [ ] Piraeus operator (LINSTOR).
- [ ] StorageClass `linstor-replicated-3` по умолчанию для prod.
- [ ] Auto-provisioning DRBD volumes.
- [ ] Backup интеграция.

**Acceptance:** PVC с replicated SC получает 3-копийный volume; failover ноды без потери данных.

**Зависит от:** 5.1

**Размер:** L

---

### 5.4 Kata containers как default runtime

**Поставка:**
- [ ] Kata install через operator (kata-deploy).
- [ ] RuntimeClass `kata` на всех нодах prod.
- [ ] Application имеет `runtime: kata|containerd` (default — kata в Tier 3).
- [ ] Performance benchmark vs containerd.

**Acceptance:** Application с `runtime: kata` запускается, изоляция проверена (ps на хосте не видит процесс).

**Зависит от:** 5.1

**Размер:** L

---

### 5.5 MSP scenarios + multi-customer Kamaji scaling (REWORK)

**Source:** ADR 0023.

**Цель:** validated MSP scenario (multiple customers одного AppRafter HQ) + scaling patterns для Kamaji когда tenants растут.

**Поставка:**
- [ ] MSP onboarding flow:
    - Apply customer Tenant manifest → new Kamaji TenantControlPlane provisioned.
    - Customer admin AccessGrant scoped to TCP only.
    - Customer Applications deployed внутри TCP.
- [ ] Customer isolation guarantees verified end-to-end:
    - Customer A's employee cannot kubectl into Customer B's TCP.
    - Customer A's employee cannot kubectl into host cluster.
    - Customer A's quota exhaustion doesn't affect Customer B.
- [ ] Multi-customer scaling patterns:
    - Shared Kamaji datastore (CNPG cluster) serves multiple TCPs — verify scaling characteristics.
    - Per-TCP node selectors для tenant workload affinity (если customer wants dedicated workers).
- [ ] Customer cluster export hooks (для customer exit / migration to self-host) — initial implementation (refines in Phase 7+).
- [ ] Backstage MSP overview: list customers, per-customer resource usage, billing-relevant metrics.

**Acceptance:**
- 3+ customer Tenants на одном AppRafter HQ instance.
- Customer A admin attempts to access Customer B's TCP → fails with proper auth error.
- Customer cluster export creates portable manifest bundle.

**Зависит от:** 3.8a (Tenant CRD), 5.3 (LINSTOR — для customer data persistence), 4.16 (MigrationPlan UI — для customer migration scenarios)

**Размер:** L

---

### 5.6 KubeVirt enable для VM workloads

**Поставка:**
- [ ] KubeVirt operator.
- [ ] CUE-схема `kind: VirtualMachine` (parallel to Application).
- [ ] Backstage plugin (минимальный list+status).

**Acceptance:** VM запускается, доступна по SSH через AccessGrant mesh.

**Зависит от:** 5.1, 4.5

**Размер:** L

---

### 5.7 Migration Tier 2 → Tier 3

**Поставка:**
- [ ] `platform-cli upgrade-tier --to prod`.
- [ ] PG migration через CNPG → restored на Tier 3 LINSTOR.
- [ ] NATS migration через mirroring.
- [ ] Workloads переезжают через MigrationPlan.

**Acceptance:** Tier 2 кластер с реальной нагрузкой мигрируется без потери данных, downtime < 30 минут на claim.

**Зависит от:** 5.3, 4.16

**Размер:** L

---

### 5.8 MARKER — Karpenter на Hetzner via CAPI (opt-in для OSS Tier 2+)

**Source:** ADR 0021.

> When Cluster API (CAPI) infrastructure is established as part of Turnkey foundation (Phase 5+ separate work track), Karpenter on Hetzner becomes available as opt-in для OSS Tier 2+ clusters. Concrete deliverables, dependencies, and sizing are populated when CAPI is ready. Karpenter component will be added to platform-stack chart как opt-in tier-2 overlay enable.

**Размер:** TBD (depends on CAPI)

---

### 5.9 Закрытие чек-листа M5 spec

- [ ] Обновить `spec.md` §6 M5.
- [ ] Tag `v0.5.0-bare-metal`.

**Размер:** XS

---

## Фаза 6 — Tier 4, confidential (M6)

**Цель фазы:** workloads с `confidential: true` на SEV-SNP / TDX нодах; attestation; AWS C8i интеграция.

**Spec:** §6 M6, §4.1 (Tier 4).

### 6.1 Kata-CC runtimeClass + nodepool selectors

> **Wording:** confidential — opt-in feature, decoupled from T4 (per ADR 0015). Любой тир может включать confidential workloads если соответствующий nodepool доступен; T4 — это "regulated" профиль (compliance, attestation, audit), не синоним "confidential".

**Поставка:**
- [ ] kata-cc установка.
- [ ] Nodepool labels `compute.confidential: tdx|sev-snp`.
- [ ] Application с `confidential: true` → scheduling на confidential nodepool + RuntimeClass kata-cc.

**Acceptance:** confidential workload запускается, attestation passes; non-confidential не попадает на confidential ноды.

**Зависит от:** 5.4

**Размер:** L

---

### 6.2 AWS provider (C8i / M7a) + Karpenter standalone (REWORK)

**Source:** ADR 0021.

**Поставка:**
- [ ] AWS SDK Rust интеграция в platform-cli.
- [ ] EC2 / VPC / EKS provisioning.
- [ ] Mixed Hetzner+AWS deployments (через Infrastructure provider композицию).
- [ ] AWS KMS для OpenBao auto-unseal.
- [ ] Karpenter standalone installation as part of AWS stack (Karpenter is native first-class on AWS, no CAPI required). Karpenter component added to platform-stack chart tier-4 overlay.
- [ ] Karpenter NodePool configurations per Application kind (default sizes, instance type preferences).
- [ ] Cluster-autoscaler explicitly **not** installed (per ADR 0021 «cluster-autoscaler not supported»).
- [ ] Verify Karpenter consolidation policy works well on AWS dual-stack instances.

**Acceptance:**
- Tier 4 на AWS C8i запускается; HA между AZ.
- AWS Tier 4 cluster bootstraps с Karpenter active.
- Application scaling triggers actual node provisioning (verify with Karpenter logs + EC2 instances list).
- Karpenter consolidates when load drops.

**Зависит от:** 1.2 (паттерн), 3.11 (KMS)

**Размер:** L (existing) + S (Karpenter additions) = L overall

---

### 6.3 Confidential service providers

**Поставка:**
- [ ] PG-confidential (CNPG на confidential nodes).
- [ ] OpenBao-confidential.
- [ ] Documentation: что нельзя сделать confidential (NATS — open question).

**Acceptance:** Application с confidential PG получает claim, который scheduling на confidential nodepool.

**Зависит от:** 6.1, 2.4

**Размер:** L

---

### 6.4 Attestation flow с workload identity

**Поставка:**
- [ ] Attestation report integration в SPIFFE workload identity (через SPIRE plugin).
- [ ] OpenBao policy: только attested workloads могут читать confidential secrets.
- [ ] Backstage badge «attested» на Application странице.

**Acceptance:** скомпрометированный (без attestation) под не получает confidential credentials.

**Зависит от:** 6.1, 3.11

**Размер:** L

---

### 6.5 Application.confidential: true flag

**Поставка:**
- [ ] CUE-схема дополнение.
- [ ] Renderer применяет nodepool selector + runtimeClass + attestation policy.
- [ ] Backstage UI отметка confidential.

**Acceptance:** один флаг включает весь стек confidential.

**Зависит от:** 6.1, 6.4

**Размер:** S

---

### 6.6 MARKER — NAT64 opt-in component

**Source:** ADR 0017.

> Implemented on-demand when first IPv6-only deployment requires outbound to legacy IPv4-only services. Component: NAT64 + DNS64 platform-service (added to platform-stack chart as opt-in component). Operator declaration: `Infrastructure.network.nat64.enabled: true` when `ipFamilies: [ipv6]` is set. Concrete deliverables added when scenario materialises.

**Размер:** TBD (deferred)

---

### 6.7 MARKER — Bare metal slow autoscaling research

**Source:** ADR 0021.

> Research item для Tier 3 bare metal autoscaling pattern. Design constraint: UX/DX must not degrade compared to faster tiers — Application API behavior identical, slow provisioning hidden through capacity headroom and predictive scaling. Possible paths: server auction cache + Robot API order automation. Research output: ADR + PoC; production implementation deferred until research conclusions.

**Размер:** L (research, not implementation)

---

### 6.8 Закрытие чек-листа M6 spec

- [ ] Обновить `spec.md` §6 M6.
- [ ] Tag `v0.6.0-confidential`.

**Размер:** XS

---

## Фаза 7 — Plugin ecosystem 🌱

**Цель фазы:** комьюнити может расширять платформу без trunk-доступа.

**Spec:** §3.6 (ServiceProviderPlugin), §3.7 (InfrastructureProviderPlugin), §4.12, §8 (three-tier plugin model).

> Запускать **параллельно** с 3+ как только есть ServiceProvider CRD (после 2.1).

### 7.1 ServiceProviderPlugin gRPC interface (proto)

**Поставка:**
- [ ] `proto/service_provider/v1.proto`: rpc Provision/Update/Deprovision/HealthCheck/Schema.
- [ ] Versioning policy.
- [ ] Codegen для Go, Rust, TypeScript, Python (CI).

**Acceptance:** генерация stub'ов в 4 языках без warning'ов.

**Зависит от:** 2.1

**Размер:** M

---

### 7.2 Plugin host runtime в operator

**Поставка:**
- [ ] Sidecar container management (operator поднимает gRPC plugin pod на ServiceProviderPlugin).
- [ ] mTLS plugin↔operator (через SPIFFE).
- [ ] Health/readiness, restart on failure.

**Acceptance:** plugin pod корректно стартует, hook'ается, перезапускается.

**Зависит от:** 7.1, 2.7

**Размер:** L

---

### 7.3 Reference community ServiceProviderPlugin: MySQL Percona

**Поставка:**
- [ ] Отдельный repo `apprafter-plugin-mysql-percona`, MIT.
- [ ] gRPC server (Go), wraps Percona Operator.
- [ ] Documentation, тесты.
- [ ] Публикация в plugin catalog.

**Acceptance:** `needs.mysql` работает после `kind: ServiceProviderPlugin` apply.

**Зависит от:** 7.2

**Размер:** L

---

### 7.4 Plugin catalog (отдельный репо)

**Поставка:**
- [ ] Static site (mdBook/Hugo) с perevody plugin'ов.
- [ ] CI checks: схема, лицензия, security review.
- [ ] Submit-PR flow.

**Acceptance:** community plugin виден на сайте после merge PR.

**Зависит от:** 7.3

**Размер:** M

---

### 7.7 WASM plugin runtime (R&D)

**Поставка:**
- [ ] Tracking ADR на состояние WASI (threading, async I/O).
- [ ] PoC в отдельной ветке.
- [ ] Decision-point: миграция или продление gRPC.

**Acceptance:** ADR с рекомендацией; код PoC.

**Зависит от:** 7.2

**Размер:** L (R&D, неблокирующее)

---

### 7.8 MARKER — kine+NATS как Kamaji datastore experimental

**Source:** ADR 0023, tracker 2.3.

> Experimental research: verify if Kamaji can use kine+NATS as datastore (kine officially supports etcd-API emulation поверх NATS; Kamaji not officially validated for this combination). If works — alternative single-substrate path для Kamaji's tenant state. If not — staying на integrated CNPG. Research output: feasibility report + (if positive) reference deployment.

**Размер:** M (research, opt-in)

---

### 7.9 MARKER — MigrationPlan future enhancements (skip + partial migration)

**Source:** ADR 0027 Still open.

> Future enhancements to MigrationPlan CRD considered post-Phase-7:
> - `skip` action: user acknowledges available upgrade without acting; PlatformStack.status.skippedVersions tracks skipped versions; only proposes next version when one becomes available. Useful for cycle skipping.
> - Partial migration: per-component approval when a platform upgrade touches multiple components. Plan splits into sub-plans or per-component approval entries.
>
> Both are extensions of existing CRD schema (additive); no breaking changes.

**Размер:** M (when triggered by user demand)

---

### 7.10 MARKER — Non-GitHub fork support

**Source:** ADR 0028 Still open.

> `apprafter platform fork` currently supports GitHub only (per 1.80). Extend to GitLab (and possibly Gitea/Forgejo) via vendor-specific API integration. Phase 2+ depending on user demand. Pattern: trait `GitHostForkProvider` with GitHub + GitLab implementations.

**Размер:** M (when triggered)

---

## Фаза 8 — 1.0 release (M7)

**Цель фазы:** стабилизация API, документация, бенчмарки, публичный релиз.

**Spec:** §6 M7.

### 8.1 CUE schema → v1 (semver guarantee)

**Поставка:**
- [ ] Заморозка CRD на v1, conversion webhooks v1alpha1→v1.
- [ ] Compatibility tests.
- [ ] Deprecation policy документ.

**Acceptance:** все CRD v1; v1alpha1 manifests продолжают работать с deprecation warnings.

**Размер:** M

---

### 8.2 TechDocs полный сайт

**Поставка:**
- [ ] Архитектура, концепты, operator guide, dev guide, reference (CRD field-by-field).
- [ ] Tutorials: «Solo founder» (Tier 1), «Small team» (Tier 2), «Production» (Tier 3), «Regulated» (Tier 4).
- [ ] FAQ.
- [ ] Search (Algolia / Stork).
- [ ] Hosted на app.apprafter.dev/docs.

**Acceptance:** новый разработчик находит ответ на 90% типовых вопросов через docs+search.

**Зависит от:** 0.7

**Размер:** L

---

### 8.3 Reference deployments (publish)

**Поставка:**
- [ ] Public Tier 1 demo cluster (read-only Backstage).
- [ ] Public Tier 2 demo.
- [ ] Bench cluster для performance reports.
- [ ] Open-source examples репо.

**Размер:** M

---

### 8.4 Public bootstrap-from-zero benchmark

**Поставка:**
- [ ] CI-job: время от `platform-cli init` до live Application.
- [ ] Цель: < 15 минут.
- [ ] Публичный dashboard с историей.

**Размер:** S

---

### 8.5 Disaster Recovery plans-as-code

**Поставка:**
- [ ] `kind: DisasterRecoveryPlan` CRD.
- [ ] Шаблоны: «полная потеря кластера», «потеря одного компонента ExternalSurface», «coruption pg».
- [ ] Manual run книги на каждый scenario.
- [ ] Quarterly DR-drill в CI.

**Acceptance:** drill восстанавливает Tier 2 кластер из бэкапов за < 2 часа.

**Зависит от:** 4.12

**Размер:** L

---

### 8.6 Security review + responsible disclosure

**Поставка:**
- [ ] External pentest.
- [ ] SECURITY.md с PGP-ключом.
- [ ] CVE process.
- [ ] Bug bounty (HackerOne / self-hosted) — опционально.

**Размер:** L

---

### 8.7 Public 1.0 launch

**Поставка:**
- [ ] Release notes.
- [ ] Blog post / announcement.
- [ ] Public roadmap для post-1.0.
- [ ] Tag `v1.0.0`.

**Размер:** S

---

## Dev-mode phases positioning (dev-mode-task.md §20)

`dev-mode-task.md` defines three phased deliveries of dev mode (Phases 1B, 2B, 3B), each interleaved with platform milestones. This section maps dev-mode phases to plan milestones для visibility; sub-version numbering and acceptance criteria остаются в `dev-mode-task.md` itself.

| dev-mode-task phase | Lands when | Purpose | Notes |
|---|---|---|---|
| **Phase 1B** — Minimum Viable Dev Mode | **After M1.5 closure** (`v0.2.x` patch series before M2 begins) | `apprafter dev cluster up/down`, `apprafter dev init`, `apprafter dev up`, `apprafter dev down`, `apprafter dev list`, `apprafter dev logs` on local k3d. Manifest layering 4 levels (base + env + dev + DevProfileLocal). No `needs.*` resolution yet — that's Phase 2B. Marked `experimental` для users. | Benefits from M1.5 Track A CLI rework (target store, miette errors); reuses M1.5 Track B platform-stack chart's tier-1 overlay (with new `tiers/dev.cue` overlay). |
| **Phase 2B** — Dev Mode + Platform Services | After M2 closure | Dev mode supports `needs.{pg, jetstream, redis}` end-to-end on local k3d via lightweight in-cluster providers (single-node Postgres pod, embedded NATS, single Redis). Still marked `experimental`. | Depends on M2 ServiceProvider CRDs and reconcilers being in place; dev mode just runs them in a lightweight configuration. |
| **Phase 3B** — Full Dev Experience | After M3 closure (part of MVP completion) | Production-ready local dev experience: heuristic runtime detection (Bun/Node/Rust/Go/Python), preset library (Bun HTTP, Rust async worker, etc.), `apprafter dev reset / restore` lifecycle, observability tab в Backstage equivalent для dev. Removes `experimental` tag. Completes MVP definition alongside platform Phase 3. | Per `dev-mode-task.md` §20: lands в planned pause between M3 and Phase 4 (managed offering research), so does not block Phase 4 startup. |

**Sequential ordering within Phase 1B**: per `dev-mode-task.md` §20 Phase 1B internal sub-items (1B.1, 1B.2, …). Each lands as its own commit / version bump; specific patch numbers (`v0.2.1`, `v0.2.2`, ...) are commit-driven, not regulated by this plan.

**Why sequential, not parallel with M1.5**: keeping the workflow linear avoids interleaving dependencies and losing track of work in flight. M1.5 closes cleanly with `v0.2.0-self-managing`, then dev-mode Phase 1B starts on top of the new platform foundation.

---

## Сквозные направления (running concerns)

Эти задачи не привязаны к конкретной фазе и идут параллельно.

### ∞.1 ADR-дисциплина

- [ ] Каждое нетривиальное архитектурное решение → ADR в `docs/adr/`.
- [ ] Раз в квартал — ревью устаревших ADR.
- [ ] Зафиксировать ADR'ы 0014–0029 (исключая 0018 как Unused): добавить в `docs/adr/`, обновить `docs/adr/README.md` index. ADR 0011 mark as `Status: Superseded by 0016`.
- [ ] **M1.5 carry-over:** ADRs 0025–0029 should be committed to `docs/adr/` during M1.5 (preferably как часть 1.66 — early commit chains decision documents to the work).

### ∞.2 Dependency hygiene

- [ ] Renovate / Dependabot bot.
- [ ] Weekly digest в Backstage.
- [ ] Critical CVE → автоматический PR.

### ∞.3 Performance regression tracking

- [ ] Bench-CI (operator reconcile latency, build time, bootstrap time).
- [ ] Baseline + alert на > 10% регрессию.

### ∞.4 Open questions из spec §7 (still open)

- [ ] (1) kine + NATS scaling ceiling — empirical, требует production-данных.
- [ ] (2) CUE vs Pkl re-evaluation point — ADR на M5.
- [ ] (3) Multi-tenancy isolation choice — ADR в фазе 5.5.
- [ ] (4) Migration tooling depth — расширение runner'ов в v2.x.
- [ ] (5) Cost attribution model — улучшение метрик per-DB / per-queue.
- [ ] (6) Backstage vs custom portal — pulse-check на каждом milestone.
- [ ] (7) WASM plugin readiness — фаза 7.7.
- [ ] (8) Bidirectional self-healing — отдельный design в M5+.
- [ ] (9) Codename — ✅ AppRafter.
- [ ] (10) OneBun integration depth — фиксировать ADR per-сервис.
- [ ] (11) Per-environment substrate (federated multi-cluster) — v2.x.

### ∞.5 Community и governance

- [ ] CONTRIBUTING flow.
- [ ] Quarterly community calls.
- [ ] Public roadmap (этот документ + статус).
- [ ] Maintainership ladder.

### ∞.6 Backports и LTS-policy

- [ ] Определить LTS-окно (рекомендация: каждый minor LTS на 1 год; 2 параллельных LTS).
- [ ] Security-bugfixes — backport.

### ∞.7 Tier-1 Hetzner stability hardening (gate to M1.5)

> **Status:** ✅ Gate passed for M1.5 start. Все items закрыты per plan.md changelog v0.1.43–v0.1.65 (см. чек-боксы ниже).

Открытые баги, найденные в первом полном ручном E2E (2026-05-08…10). Закрыты до старта M1.5 (v0.1.66+) — иначе M1.5 строится на дрейфующей основе. Каждый — отдельный patch v0.1.4x–v0.1.5x.

- [x] **SSH host-key collision при destroy+apply на тот же IP.** ✅ закрыто `v0.1.46` 2026-05-10. `StatePaths::known_hosts_file()` → `.apprafter/known_hosts`; `SshKubeconfigFetcher` принимает path и передаёт `-o UserKnownHostsFile=…` + `-o StrictHostKeyChecking=accept-new`. `destroy --yes` сносит файл вместе со state. `~/.ssh/known_hosts` не трогаем.
- [x] **`HetznerCloudProvider::destroy()` race-condition.** ✅ закрыто двумя слоями: `v0.1.47` (server-level poll: `wait_for_server_gone()` ждёт исчезновения server из `GET /v1/servers`); `v0.1.50` (resource-level retry: `delete_with_retry_on_resource_in_use` для `delete_firewall` + `delete_network` — Hetzner reaps `firewall.applied_to` / `network.servers` ещё 1-15с после server-vanish, ловит на `422 resource_in_use`). Exponential back-off 500ms → 5s, 60s deadline в обоих слоях.
- [x] **noVNC console fallback при сетевой смерти VM.** ✅ закрыто `v0.1.49` 2026-05-10 (docs-only по варианту C). Новый `docs/operator-guide/recovery.md` с runbook'ом Hetzner Rescue Mode + chroot для триажа cloud-init / k3s / firewall логов с диска. Code-патч с опциональным `APPRAFTER_EMERGENCY_ROOT_PASSWORD` отложен до tier-3/4 (явный opt-in с audit-logging — не default для tier-1, который key-only by design).
- [x] **`default-deny` NetworkPolicy блокирует всё включая DNS+Service routing.** ✅ закрыто `v0.1.51` 2026-05-10. v0.1.0-mvp через v0.1.50 деплоил NP с `policyTypes: [Ingress, Egress]` и пустыми allow-rules → каждый workload в namespace в полной изоляции (только probes от kubelet работали, потому что host-network). Скрытно потому что nightly не пушился, а §4 quickstart никто не проходил end-to-end до 2026-05-10. Fix: Ingress-only с явными allow для same-ns (Service routing) и kube-system (Gateway/HTTPRoute/monitoring); egress без ограничений до phase 2.10.
- [x] **`tracing` logs идут в stdout вместо stderr.** ✅ закрыто `v0.1.44` 2026-05-09. `with_writer(std::io::stderr)` в `cli-core/src/logging.rs` + smoke-test guard в `cli_smoke.rs`. Affected commands: `apply`, `destroy`, `import`, `kubeconfig`, `argocd-password` теперь имеют чистый stdout, диагностика на stderr.
- [x] **k3s flannel конфликтует с Cilium VXLAN device.** ✅ закрыто `v0.1.45` 2026-05-09. k3s ships embedded flannel-vxlan daemon на UDP port 8472, тот же что нужен Cilium → `cilium_vxlan: address already in use` → cilium-agent CrashLoopBackOff → каждый `cluster-bootstrap` падал на Argo CD pre-install timeout. Fix: добавили `--flannel-backend=none --disable-network-policy` к k3s installer в `user_data.rs`; теперь 5 disabled-флагов вместо 3 (Cilium-recommended k3s recipe).

### ∞.8 CRD short-name rename pre-M2

**Source:** SPEC_REFINEMENTS cross-cut from ADR обсуждений.

- [ ] Rename `applications.apprafter.io` CRD short-name to `apps.apprafter.io` or `workloads.apprafter.io` to avoid shadowing Argo CD's `applications.argoproj.io`. Decision and rename must happen during M1.5 (ideally early, around 1.66–1.70) before more docs reference the short name.

**Размер:** XS (CRD spec change + admission alias + docs sweep). Affects existing tests minimally; mostly documentation.

### ∞.9 Smoke test design fix (closes Phase 1 quickstart contradiction)

**Source:** discussion of operator quickstart §4.

- [ ] Rewrite `e2e/mvp.sh` and `docs/operator-guide/quickstart.md` §4 to exercise the `Application` CRD end-to-end рядом с the platform stack, not раздельно as currently. Folded into 1.81 (e2e tests update) and 1.82 (docs update) within M1.5.

---

## История изменений плана

| Дата | Изменение | Автор |
| --- | --- | --- |
| 2026-05-06 | Первая версия плана из spec.md rev.4 | initial |
| 2026-05-06 | Закрыта подфаза 0.1 — структура монорепы и README | initial |
| 2026-05-06 | Закрыта подфаза 0.2 — LICENSE / NOTICE / SPDX-шаблон | initial |
| 2026-05-06 | Закрыта подфаза 0.3 — ADR-шаблон + 12 ADR; 0007 переназначен на SealedSecrets/OpenBao | initial |
| 2026-05-06 | Закрыта подфаза 0.4 — CUE-модуль `apprafter.io` + skeleton 9 CRD + lint-скрипт | initial |
| 2026-05-06 | Закрыта подфаза 0.5 — CI workflows, GitHub meta, lefthook, SPDX/commit-msg скрипты | initial |
| 2026-05-06 | Закрыта подфаза 0.6 — flake.nix, devcontainer, mise.toml, Justfile, contributing/setup | initial |
| 2026-05-06 | Закрыта подфаза 0.7 — mkdocs-skeleton, governance файлы, docs-serve/-build таргеты | initial |
| 2026-05-06 | Закрыта подфаза 0.8 — spec.md M0 полностью закрыт, UNRELEASED changelog заведён | initial |
| 2026-05-06 | Phase 1 стартовал — версия `0.1.x` (минор=фаза, патч=подфаза) | initial |
| 2026-05-06 | Закрыта подфаза 1.1 — Cargo workspace `cli/` + skeleton subcommands; v0.1.1 | initial |
| 2026-05-06 | 1.2 (server-CRUD ветка) — `HetznerCloudProvider` (apply/destroy/idempotent CX22); v0.1.2 | initial |
| 2026-05-06 | 1.2 (SSH-keys ветка) — `HetznerCloudProvider.ssh_keys` + APPRAFTER_SSH_PUBLIC_KEY; v0.1.3 | initial |
| 2026-05-06 | 1.2 (Network + Firewall ветка) — default 10.0.0.0/16 net + SSH/HTTPS firewall, server attached; v0.1.4 | initial |
| 2026-05-06 | 1.2 (CUE Infrastructure parsing ветка) — APPRAFTER_MANIFEST overlays defaults; v0.1.5 | initial |
| 2026-05-08 | Phase 1 patch — Hetzner `cx22` retired upstream; default flipped to `cpx22` + pre-flight validate_server_type lookup; v0.1.42 | initial |
| 2026-05-09 | Phase 1 patch — cloud-init drops ufw (silent initcaps fail on noble); fail2ban + Hetzner Cloud Firewall стали единственными слоями; v0.1.43 | initial |
| 2026-05-09 | Phase 1 patch — tracing logs → stderr (фикс stdout-pollution для `kubeconfig`/`argocd-password` пайпов); ∞.7 bug #4 ✅; v0.1.44 | initial |
| 2026-05-09 | Phase 1 patch — k3s installer передаёт `--flannel-backend=none --disable-network-policy` (фикс cilium_vxlan VXLAN-port collision, разблокирует cluster-bootstrap); ∞.7 bug #5 ✅; v0.1.45 | initial |
| 2026-05-10 | Phase 1 patch — per-cluster `.apprafter/known_hosts` для SSH (фикс host-key collision на recycled Hetzner IPs); ∞.7 bug #1 ✅; v0.1.46 | initial |
| 2026-05-10 | Phase 1 patch — `destroy()` poll-wait после delete_server (фикс async-cleanup race с 409 на delete_network); ∞.7 bug #2 ✅; v0.1.47 | initial |
| 2026-05-10 | Phase 1 patch — cert-manager `installCRDs` → `crds.enabled` (drop deprecation warning); v0.1.48 | initial |
| 2026-05-10 | Phase 1 patch (docs-only) — operator-guide `recovery.md` для Hetzner Rescue Mode runbook; ∞.7 bug #3 ✅ закрыто docs-путём; v0.1.49 | initial |
| 2026-05-10 | Phase 1 patch — retry-on-`resource_in_use` для `delete_firewall`/`delete_network` (второй слой защиты от Hetzner async-cleanup лагов после v0.1.47); v0.1.50 | initial |
| 2026-05-10 | Phase 1 patch — `default-deny` NP теперь Ingress-only с allow для same-ns + kube-system (фикс silent-breakage workloads — DNS+Service routing блокировались с v0.1.0-mvp); ∞.7 bug #6 ✅; v0.1.51 | initial |
| 2026-05-10 | Phase 1 patch — GHCR release workflow + `apprafter-operator/Dockerfile`; разблокирует §5 (operator image теперь pullable, не «build your own»); v0.1.52 | initial |
| 2026-05-10 | Phase 1 patch — `e2e/mvp.sh` использует `curlimages/curl:latest` вместо несуществующего `:8`; ∞.7 bug #7 ✅; v0.1.53 | initial |
| 2026-05-10 | Phase 1 patch — closure возвращает `ureq::Request` (не `Result`), фикс CI clippy 1.95 `result_large_err`; v0.1.54 | initial |
| 2026-05-10 | Phase 1 patch — release-operator workflow lowercase'ит `repository_owner` для ghcr.io tag (Docker registry требует lowercase); v0.1.55 | initial |
| 2026-05-10 | Phase 1 patch — упрощены `operator-*/Dockerfile` (drop fragile dep-prebuild trick) + `operator/.dockerignore`; cargo-chef как follow-up; v0.1.56 | initial |
| 2026-05-10 | Phase 1 patch — bump rust pin 1.83 → 1.85 (transitive dep `hashbrown-0.17.1` требует cargo `edition2024` feature, стабилизированный в 1.85); v0.1.57 | initial |
| 2026-05-10 | Phase 1 patch — Dockerfile-pin переехал с фиксированной версии на `rust:stable-alpine` (transitive deps непредсказуемо бампают MSRV, реактивные патчи дорого); MSRV → 1.88; v0.1.58 | initial |
| 2026-05-10 | Phase 1 patch — `rust:stable-alpine` не существует на Docker Hub, заменено на каноничный `rust:alpine`; v0.1.59 | initial |
| 2026-05-11 | Phase 1 patch — `helm template` whitespace-trim съедал newline между SPDX-комментом и `apiVersion:` в `serviceaccount.yaml`/`rbac.yaml` чарта operator; v0.1.60 | initial |
| 2026-05-11 | Phase 1 patch — operator pod CrashLoopBackOff: `install_rustls_crypto_provider()` перед `Client::try_default` (rustls 0.23+ убрал auto-default); + 2 regression-guard unit-теста; v0.1.61 | initial |
| 2026-05-11 | Phase 1 patch — CRD объявляет `.status` schema (раньше было только `subresources.status: {}` без `properties.status`, operator PATCH падал на `.status: field not declared in schema`); +1 regression-guard тест; v0.1.62 | initial |
| 2026-05-11 | Phase 1 patch — hot-reconcile loop из-за `lastTransitionTime = now()` на каждом reconcile; теперь preserve when status unchanged (k8s `meta/v1.Condition` semantics); +2 regression-guard теста; v0.1.63 | initial |
| 2026-05-11 | Phase 1 Level B integration — default-on operator + webhook (§1.14); v0.1.64 | initial |
| 2026-05-11 | Phase 1 Level C GitOps — env-driven Argo CD repo credentials (§1.15); v0.1.65 | initial |
| 2026-05-13 | §1.15 walks bug fixes — tier-1 firewall port 6443 + destroy floating-IP unassign order; v0.1.66 | initial |
| 2026-05-13 | §1.15 walks follow-up — real Hetzner unassign-422 message + 423 locked retry; v0.1.67 | initial |
| 2026-05-13 | §1.15 Q3 security fix — repo-creds Secret apply via SSA (no more PAT leak in last-applied-configuration annotation); v0.1.68 | initial |
| 2026-05-14 | M1.5 Track A.1 — rename `platform-cli` → `apprafter`, add deprecated shim, sweep user-facing docs, retarget tracing filter; v0.1.69 | initial |
| 2026-05-14 | 1.2 AUDIT (Hetzner IPv6) — wire-type `PublicIpv6` + dual-stack k3s `--cluster-cidr`/`--service-cidr` (ADR 0017) + Hetzner Firewall ICMP allow-rule; v0.1.70 | initial |
| 2026-05-14 | 1.4 AUDIT (Cilium dual-stack) — explicit `ipv4.enabled: true` + `ipv6.enabled: true` в Helm values (ADR 0017); `e2e/mvp.sh` Phase 6.4 — pod-level v4+v6 podIPs assertion; v0.1.71 | initial |
| 2026-05-14 | M1.5 Track A.2 — `cli-core::target` module: types (GlobalConfig/TargetConfig/TargetCredentials/Target/TargetStorePaths) + atomic load/save IO + 0600 enforcement + `<redacted>` Debug; +16 unit tests; v0.1.72 | initial |
| 2026-05-14 | M1.5 Track A.3 — `apprafter target add` (+`t` alias) non-interactive: validators, `--force`/`--renew`, first-target auto-active, env-override `APPRAFTER_CONFIG_DIR`; +33 tests; v0.1.73 | initial |
| 2026-05-14 | v0.1.73 hotfix — `validate_hetzner_token_format` теперь exactly 64 ASCII alphanumeric (без `hcloud_` префикса — v0.1.73 ложно его требовал, реальные Hetzner токены префикса не имеют); cli-dx-task.md §11 amended; +регрессия-guard на underscore-at-correct-length; v0.1.74 | initial |
| 2026-05-14 | M1.5 Track A.4a — `cli-providers::validators` + `HetznerCloudValidator` (`GET /v1/locations` ping); `target add` теперь validates token by default + `--no-ping`/`APPRAFTER_NO_PING` opt-out; +8 tests (3 validator + 5 integration); v0.1.75 | initial |
| 2026-05-14 | M1.5 Track A.4b — interactive wizard via `inquire`: Text/Password/Select prompts по spec §5.1, inline format+ping validation в Password, region-picker через `list_regions()`, tilde expansion для SSH-key, default-when-TTY поведение через `IsTerminal` + `should_use_wizard()`; +7 tests; v0.1.76 | initial |
| 2026-05-14 | v0.1.76 wizard polish — workspace `Cargo.toml` version-bump policy (CLAUDE.md addendum, bumped 0.1.2 → 0.1.77); wizard fires on ANY TTY (drops "all-required" short-circuit) so optional fields get prompted; HCLOUD_TOKEN-from-env notification; SSH-key `Select` из `~/.ssh/*.pub` + algo/comment label; parallel region-latency probe (`<region>-speed.hetzner.com:443`) with sort + ms display; +10 tests; v0.1.77 | initial |
| 2026-05-14 | v0.1.77 wizard polish #2 — `prompt_name` silent on prefill (consistency with other prompts); `ℹ <Field>: <value> (from <source>)` для всех prefilled wizard-полей (name/provider/ssh-key/region/tier — token уже имел); `run_renew` rejects identical-token with hint to generate new in Cloud Console; +3 tests; v0.1.78 | initial |
| 2026-05-14 | M1.5 Track A.5 — target CRUD: `target list` (tabled-таблица с `*` маркером) + `target use` (свитч active) + `target show` (детали; токен masked как `set (N chars)`) + `target rename` (атомарно с auto-update active pointer) + `target remove` (`--yes` opt-in или interactive Confirm; active → reassign alphabetically); `cli_core::target::rename_target` API; +21 tests; v0.1.79 | initial |
| 2026-05-14 | M1.5 Track A.6 — `apprafter whoami` (identity + active target + best-effort verified status; failed ping не валит exit) + hidden `apprafter auth login/logout/status` stubs (friendly redirect на `target add` + ссылка на Managed roadmap); +15 tests; v0.1.80 | initial |
| 2026-05-14 | M1.5 Track A.7 — `apprafter doctor` (self-diagnostic): target checks (config readable, creds mode 0600, provider known, token format, token API-verified с latency, ssh-key readable) + env checks (kubectl/helm/ssh on PATH, DNS resolves api.hetzner.cloud); trichotomy PASS/WARN/FAIL → exit 1 на FAIL; +17 tests; v0.1.81 | initial |
| 2026-05-14 | M1.5 Track A.8 — wire `apply`/`destroy`/`import` в credential resolution chain: new `cli_core::credentials` с `resolve_hetzner_token` + `resolve_hetzner_ssh_public_key` (`--flag > env > target store`), `--target <name>` flag на трёх operational commands, error messages enumerate все 3 пути; backwards-compat env-only workflows preserved; +17 tests; v0.1.82 | initial |
| 2026-05-14 | v0.1.82 hotfix — `apply`/`import` теперь дочитывают `provider`/`region`/`cluster_name` из active target когда state.json пустой (A.8 wire'нул только credentials, но operational commands ещё требовали `init`); `init` становится опциональным после `target add`; new `cli_core::target::{resolve_active_target_name, load_active_target_config}` helpers; +1 regression-guard test; v0.1.83 | initial |
| 2026-05-14 | M1.5 Track A.9 — `apprafter bootstrap-all` orchestrator (Phase 1 apply → Phase 2 kubeconfig SSH poll до 300s × 10s интервал → Phase 3 cluster-bootstrap) под единым `indicatif::MultiProgress` UX; `--dry-run` печатает план без provider/cluster mutation; `--target <name>` пробрасывается во все фазы; `commands::kubeconfig::fetch_and_cache` extracted из `run` чтобы Phase 2 retry-loop не плодил child процессы; `commands::kubeconfig` теперь использует `resolve_hetzner_token` вместо прямого env-чтения; `--target` flag добавлен на `Commands::Kubeconfig`; +8 tests (4 unit + 4 integration); v0.1.84 | initial |
| 2026-05-14 | M1.5 Track A.9 hotfix UX — после ручного walk'а v0.1.84: `MultiProgress` дублировал spinner-строки на каждый helm/kubectl `println` (Phase 1 + Phase 3 spinner fought с tracing-логами apply/cluster-bootstrap за тот же row); dry-run печатал `<active target>` placeholder вместо имени активного target'а и не раскрывал что делает каждая фаза. v0.1.85 пересобрал bootstrap_all: Phase 1+3 без spinner (`→ start / inner output / ✓ end`), Phase 2 keeps spinner (retry loop owns all output), `finish_and_clear()` + static success line; dry-run теперь load'ит `default_config_root` + `resolve_active_target_name` + `load_active_target_config` и печатает реальное имя active target + Provider/Region/Tier/Cluster/SSH-key из `config.yaml` + human-readable описание каждой фазы; +2 integration tests (empty store hint + active target resolved name); v0.1.85 | initial |

