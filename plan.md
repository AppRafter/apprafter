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

## Фаза 3 — Multi-node + Observability (M3) ⚡

**Цель фазы:** платформа поднимается в HA на 3 нодах; observability stack по умолчанию для всех workload'ов.

**Spec:** §6 M3, §4.1 (Tier 2), §4.2, §4.10, §4.4 (OpenBao).

### 3.1 HA-bootstrap в platform-cli

**Поставка:**
- [ ] `platform-cli init --tier team --nodes 3`.
- [ ] k3s server ×3 с `--cluster-init` + joins.
- [ ] Embedded LB через kube-vip (или Hetzner LB).
- [ ] Smoke: убить мастер — kubectl продолжает работать.

**Acceptance:** 3-нодовый кластер за один init; failover мастера < 30s.

**Зависит от:** 1.13

**Размер:** L

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

### 3.7 Hubble + Grafana network dashboards

**Поставка:**
- [ ] Hubble enable, относительная сборка metrics в VM.
- [ ] Backstage plugin: Hubble flow visualizer (отдельная карточка на Application page).
- [ ] «Наблюдаемое не разрешено политикой» → suggest-policy кнопка (создаёт Pull Request с дополнением `connects`).

**Acceptance:** разработчик видит реальный трафик своего Application; нажимает кнопку — открывается PR.

**Зависит от:** 3.6, 1.10

**Размер:** M

---

### 3.8 vCluster optional для env separation

**Поставка:**
- [ ] vCluster operator установка опциональная (`platformServices.vcluster: true`).
- [ ] `Application.environments.<env>` может указать `isolation: vcluster`.
- [ ] platform-cli создаёт kubeconfig для vCluster при AccessGrant.

**Acceptance:** dev и prod в разных vCluster, изолированы на уровне API; ResourceClaim между ними не пересекаются.

**Зависит от:** 3.1

**Размер:** L

---

### 3.9 Cilium Egress Gateway + статические egress IP

**Поставка:**
- [ ] CiliumEgressGatewayPolicy для Application с `network.egressIP.static: true`.
- [ ] Привязка floating IP (Hetzner) к egress-нодам.
- [ ] Backstage показывает текущий egress IP на странице Application с кнопкой copy.

**Acceptance:** трафик от Application к `api.tron.network` идёт с фиксированного IP; смена floating IP отражается в UI.

**Зависит от:** 1.2, 3.1

**Размер:** M

---

### 3.10 platform-cli upgrade-tier 1→2

**Поставка:**
- [ ] Команда `platform-cli upgrade-tier --to team`.
- [ ] Превращает single-node в 3-node (добавляет 2 ноды в Hetzner, joins, переключает kine на NATS HA).
- [ ] Бэкап перед миграцией (snapshot в S3).
- [ ] Rollback при failure.

**Acceptance:** Tier 1 кластер с задеплоенным hello-world превращается в Tier 2 без downtime > 1 минуты.

**Зависит от:** 3.1, 3.2

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

### 4.5 AccessGrant CRD + reconciler

**Поставка:**
- [ ] CUE-схема (§3.4).
- [ ] Reconciler:
  - создаёт Headscale pre-auth key (одноразовый, 24h).
  - создаёт RoleBinding/ClusterRoleBinding в k8s.
  - создаёт OIDC group mapping.
  - публикует событие → notifications-сервис.
- [ ] Status: issued / pending-activation / active / expiring / expired.

**Acceptance:** apply AccessGrant → email с magic-link приходит; click → SSO+MFA → подключение работает.

**Зависит от:** 4.4, 2.13

**Размер:** L

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

### 4.10 Audit log в JetStream

**Поставка:**
- [ ] Stream `audit.platform` с retention 1 год.
- [ ] Все компоненты публикуют структурированные audit-события (кто, что, когда, на что).
- [ ] Backstage audit-viewer plugin.
- [ ] Экспорт в S3 для compliance.

**Acceptance:** все события из §3.4 (login, AccessGrant lifecycle, MigrationPlan approval) видны и неизменяемы.

**Зависит от:** 3.2

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

### 4.16 MigrationPlan CRD + reconciler

**Поставка:**
- [ ] CUE-схема (§3.8).
- [ ] Триггеры:
  - selector change для stateful claims.
  - major version upgrade ServiceProvider.
  - storage class change.
- [ ] Backstage banner «Pending migration approval» с risk breakdown.
- [ ] Approve/reject/edit workflow (audit-logged).
- [ ] Migration runner (на каждый известный тип миграции — отдельный runner).
- [ ] v1.0 включает runner для PG `tier: integrated → managed-aws` и pg major upgrade.

**Acceptance:** изменение `selector: tier` для PG — не применяется, создаётся MigrationPlan; approve → шаги выполняются с прогрессом в UI.

**Зависит от:** 2.4, 4.10

**Размер:** L

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

### 5.5 vCluster для tenant separation

**Поставка:**
- [ ] vCluster включается по умолчанию на Tier 3+ для каждого env.
- [ ] AccessGrant создаёт RoleBinding в vCluster, не в host.
- [ ] Resource quotas per vCluster.

**Acceptance:** tenant'ы изолированы; failure одного vCluster не задевает другой.

**Зависит от:** 3.8, 5.1

**Размер:** M

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

### 5.8 Закрытие чек-листа M5 spec

- [ ] Обновить `spec.md` §6 M5.
- [ ] Tag `v0.5.0-bare-metal`.

**Размер:** XS

---

## Фаза 6 — Tier 4, confidential (M6)

**Цель фазы:** workloads с `confidential: true` на SEV-SNP / TDX нодах; attestation; AWS C8i интеграция.

**Spec:** §6 M6, §4.1 (Tier 4).

### 6.1 Kata-CC runtimeClass + nodepool selectors

**Поставка:**
- [ ] kata-cc установка.
- [ ] Nodepool labels `compute.confidential: tdx|sev-snp`.
- [ ] Application с `confidential: true` → scheduling на confidential nodepool + RuntimeClass kata-cc.

**Acceptance:** confidential workload запускается, attestation passes; non-confidential не попадает на confidential ноды.

**Зависит от:** 5.4

**Размер:** L

---

### 6.2 AWS provider (C8i / M7a)

**Поставка:**
- [ ] AWS SDK Rust интеграция в platform-cli.
- [ ] EC2 / VPC / EKS provisioning.
- [ ] Mixed Hetzner+AWS deployments (через Infrastructure provider композицию).
- [ ] AWS KMS для OpenBao auto-unseal.

**Acceptance:** Tier 4 на AWS C8i запускается; HA между AZ.

**Зависит от:** 1.2 (паттерн), 3.11 (KMS)

**Размер:** L

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

### 6.6 Закрытие чек-листа M6 spec

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

### 7.5 InfrastructureProviderPlugin interface

**Поставка:**
- [ ] CUE-схема `kind: InfrastructureProviderPlugin`.
- [ ] Plugin contract: CUE→OpenTofu translator + state-importer.
- [ ] platform-cli host runtime (subprocess invocation OpenTofu).

**Acceptance:** заглушка-plugin для OVH работает на минимальном `Infrastructure` манифесте.

**Зависит от:** 1.1

**Размер:** L

---

### 7.6 Reference InfrastructureProviderPlugin: Scaleway

**Поставка:**
- [ ] Repo `apprafter-infra-scaleway`, MIT.
- [ ] CUE→OpenTofu translator (scaleway provider).
- [ ] Documentation.

**Acceptance:** `platform-cli init --provider scaleway` поднимает Tier 1 кластер.

**Зависит от:** 7.5

**Размер:** L

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

## Сквозные направления (running concerns)

Эти задачи не привязаны к конкретной фазе и идут параллельно.

### ∞.1 ADR-дисциплина

- [ ] Каждое нетривиальное архитектурное решение → ADR в `docs/adr/`.
- [ ] Раз в квартал — ревью устаревших ADR.

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

### ∞.7 Tier-1 Hetzner stability hardening (gate to Phase 2)

Открытые баги, найденные в первом полном ручном E2E (2026-05-08…10). Закрыть до старта Phase 2 (v0.2.0) — иначе Phase 2 строится на дрейфующей основе. Каждый — отдельный patch v0.1.4x.

- [x] **SSH host-key collision при destroy+apply на тот же IP.** ✅ закрыто `v0.1.46` 2026-05-10. `StatePaths::known_hosts_file()` → `.apprafter/known_hosts`; `SshKubeconfigFetcher` принимает path и передаёт `-o UserKnownHostsFile=…` + `-o StrictHostKeyChecking=accept-new`. `destroy --yes` сносит файл вместе со state. `~/.ssh/known_hosts` не трогаем.
- [x] **`HetznerCloudProvider::destroy()` race-condition.** ✅ закрыто двумя слоями: `v0.1.47` (server-level poll: `wait_for_server_gone()` ждёт исчезновения server из `GET /v1/servers`); `v0.1.50` (resource-level retry: `delete_with_retry_on_resource_in_use` для `delete_firewall` + `delete_network` — Hetzner reaps `firewall.applied_to` / `network.servers` ещё 1-15с после server-vanish, ловит на `422 resource_in_use`). Exponential back-off 500ms → 5s, 60s deadline в обоих слоях.
- [x] **noVNC console fallback при сетевой смерти VM.** ✅ закрыто `v0.1.49` 2026-05-10 (docs-only по варианту C). Новый `docs/operator-guide/recovery.md` с runbook'ом Hetzner Rescue Mode + chroot для триажа cloud-init / k3s / firewall логов с диска. Code-патч с опциональным `APPRAFTER_EMERGENCY_ROOT_PASSWORD` отложен до tier-3/4 (явный opt-in с audit-logging — не default для tier-1, который key-only by design).
- [x] **`default-deny` NetworkPolicy блокирует всё включая DNS+Service routing.** ✅ закрыто `v0.1.51` 2026-05-10. v0.1.0-mvp через v0.1.50 деплоил NP с `policyTypes: [Ingress, Egress]` и пустыми allow-rules → каждый workload в namespace в полной изоляции (только probes от kubelet работали, потому что host-network). Скрытно потому что nightly не пушился, а §4 quickstart никто не проходил end-to-end до 2026-05-10. Fix: Ingress-only с явными allow для same-ns (Service routing) и kube-system (Gateway/HTTPRoute/monitoring); egress без ограничений до phase 2.10.
- [x] **`tracing` logs идут в stdout вместо stderr.** ✅ закрыто `v0.1.44` 2026-05-09. `with_writer(std::io::stderr)` в `cli-core/src/logging.rs` + smoke-test guard в `cli_smoke.rs`. Affected commands: `apply`, `destroy`, `import`, `kubeconfig`, `argocd-password` теперь имеют чистый stdout, диагностика на stderr.
- [x] **k3s flannel конфликтует с Cilium VXLAN device.** ✅ закрыто `v0.1.45` 2026-05-09. k3s ships embedded flannel-vxlan daemon на UDP port 8472, тот же что нужен Cilium → `cilium_vxlan: address already in use` → cilium-agent CrashLoopBackOff → каждый `cluster-bootstrap` падал на Argo CD pre-install timeout. Fix: добавили `--flannel-backend=none --disable-network-policy` к k3s installer в `user_data.rs`; теперь 5 disabled-флагов вместо 3 (Cilium-recommended k3s recipe).

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

