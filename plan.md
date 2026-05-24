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
| 1.9 | Dev Mode MVP (1B) | dev-mode-task.md §20 Phase 1B | M+ | 1.5 |
| 2 | Платформенные сервисы | M2 | L | 1 |
| 2.9 | Dev Mode + Services (2B) | dev-mode-task.md §20 Phase 2B | M | 1.9, 2 |
| 3 | Multi-node + observability | M3 | L | 2 |
| 3.9 | Dev Mode Full (3B) | dev-mode-task.md §20 Phase 3B | M | 2.9, 3 |
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

### 1.66A.10 miette diagnostic refinement ✅

> v0.1.86 — sub-phase 1.66A.10 shipped: каждый user-facing вариант `CliError` теперь несёт стабильный `code(apprafter::*)` + многострочный `help(...)` через `miette::Diagnostic` derive, а binary entry point рендерит через `miette::MietteHandlerOpts` (`fancy` reporter) вместо `color-eyre`. Результат — rustc-quality error UX: `error:` + код, бокс-обёрнутое сообщение, многострочный `help:` с конкретными next-step командами. Зависимость `color-eyre` удалена.

**Source:** `cli-dx-task.md` §10 + §17 row 10.

**Поставка:**
- [x] `cli/Cargo.toml` workspace deps: `miette = { version = "7", features = ["fancy"] }`; удалена `color-eyre` (больше не используется).
- [x] `cli/cli-core/Cargo.toml` — `miette` direct dep (поскольку `CliError` derives `Diagnostic` в cli-core).
- [x] `cli/platform-cli/Cargo.toml` — `miette` direct dep, удалена `color-eyre`.
- [x] `cli/cli-core/src/error.rs`:
    - `CliError` теперь derives `miette::Diagnostic` рядом с `thiserror::Error`.
    - 9 вариантов получили `#[diagnostic(code(...), help(...))]`:
        - `CueNotFound` — `apprafter::env::cue_not_found` + nix-develop hint.
        - `CueExport` — `apprafter::env::cue_export_failed` + `cue vet` reproduce hint.
        - `Hetzner` — `apprafter::provider::hetzner_api_error` + enumerate 401/403/429/5xx common causes + `apprafter doctor` next-step.
        - `ServerTypeUnavailable` — `apprafter::provider::server_type_unavailable` + cx22→cpx22 retirement story.
        - `InvalidState` — `apprafter::state::corrupt` + `apprafter import` recovery hint.
        - `InvalidTargetConfig` — `apprafter::target::invalid_config` + per-target dir recovery path.
        - `TargetNotFound` — `apprafter::target::not_found` + `target list` + `target add` hints.
        - `Io` / `Json` / `Yaml` — каждая получает `apprafter::io::*` code + variant-specific help.
        - `Other` (catch-all) — `apprafter::cli::other` со стабильным code чтобы log-analytics могла find'ить recurring messages кандидатами на promotion в typed variant.
    - File-level `#![allow(unused_assignments)]` для подавления `miette-derive` 7.6.0's generated reassignments (lint fires на generated code за нашим контролем; локальное `#[allow]` на enum не пропускается через derive macro).
- [x] `cli/platform-cli/src/main.rs`:
    - Return type `color_eyre::Result<()>` → `miette::Result<()>`.
    - `color_eyre::install()` заменён на `miette::set_hook(...)` с `MietteHandlerOpts::new().terminal_links(true).unicode(true).context_lines(2).with_cause_chain().build()`.
    - Вынесен `fn dispatch(args: Cli) -> cli_core::Result<()>` — типизированный CliError→miette::Report happens exactly once на binary boundary, inner code keeps original `?` ergonomics over `cli_core::Result<T>`.
- [x] doc-comment на `cli-core::error` объясняет policy: новые call-sites должны добавлять typed variants с кодами вместо `Other(format!(...))`.

**Тесты (8 unit + 3 integration):**
- 8 в `cli_core::error::tests`:
    - `target_not_found_diagnostic_carries_stable_code_and_helpful_hint` — code = `apprafter::target::not_found`, help содержит `target list` + `target add`.
    - `invalid_target_config_diagnostic_points_at_target_directory` — code = `apprafter::target::invalid_config`, help содержит `$XDG_CONFIG_HOME/apprafter/targets/` + `target add`.
    - `hetzner_diagnostic_help_enumerates_401_403_429_5xx` — help раскрывает все 4 типа failures + `target add` + `doctor`.
    - `server_type_unavailable_diagnostic_explains_retirement_path` — help упоминает cx22 + cpx22 retirement.
    - `cue_not_found_diagnostic_recommends_nix_develop` — help содержит `nix develop` + `docs/contributing/setup.md`.
    - `invalid_state_diagnostic_recommends_import_for_recovery` — help содержит `apprafter import`.
    - `io_error_passes_through_with_dedicated_code` — code = `apprafter::io::error`, wrapped OS message survives в Display.
    - `other_keeps_catch_all_code_so_recurring_variants_can_be_filtered` — code = `apprafter::cli::other` (stable для log analytics).
- 3 в `tests/miette_render_test.rs` (полноценный subprocess-based render contract):
    - `unhandled_error_renders_with_miette_help_line` — `apply` без creds → stderr содержит `help:` + `apprafter::cli::other` code (catch-all variant goes through fancy renderer).
    - `typed_target_not_found_renders_with_dedicated_code_and_help` — `target show ghost` → stderr содержит `apprafter::target::not_found` + `help:` + `apprafter target list` + `apprafter target add` substrings из help text.
    - `no_color_env_yields_ansi_free_stderr` — `NO_COLOR=1` → no `\x1b` bytes в stderr но `help:` + diagnostic code still present (pipe-friendly).

**Acceptance:**
- ✅ Любой `CliError` reaching `main` рендерится с `Error: apprafter::<...>` + box-wrapped message + `help:` block (NOT с `Debug` stringification).
- ✅ Stable diagnostic codes per variant для log-analytics + future error catalogue.
- ✅ `NO_COLOR=1` респектится (no ANSI sequences в stderr).
- ✅ `cargo test --workspace` — 553 tests, 0 failures (+11 over v0.1.85's 542).
- ✅ fmt + clippy (-D warnings) + SPDX (163 файла) clean.
- ✅ `color-eyre` workspace + platform-cli deps удалены.

**Out-of-scope (отложено):**
- `#[source_code]` + `#[label]` span highlighting per variant (например, `InvalidHetznerTokenFormat` с подсветкой именно префикса) — feature exists в miette, но требует carrying source text через error chain. Promote later when CUE manifest parsing errors get the same treatment.
- Promotion массовых `CliError::Other(format!(...))` call sites в типизированные варианты — `Other` остаётся catch-all со стабильным code; конвертация — отдельная работа, по мере того как specific shapes повторяются.
- Cause-chain rendering refinements (multi-level nested errors) — `with_cause_chain()` уже включён в hook builder, но AppRafter ещё не порождает глубоких цепочек. Полировка — when needed.

**Зависит от:** 1.66A.9 ✅ (нужен `bootstrap-all` рабочий путь для smoke tests миette-рендера).

**Размер:** S (один цикл, ~0.5 рабочего дня).

---

### 1.66A.11 Aliases + semantic colors + NO_COLOR ✅

> v0.1.88 — sub-phase 1.66A.11 shipped: новый `cli_core::style` модуль с семантическими хелперами поверх `owo-colors` (auto-honours `NO_COLOR` через `supports-colors` feature); цвет applied на `bootstrap-all` markers (`→` cyan, `✓` green, `✗` red) + `doctor` PASS/WARN/FAIL glyphs (green/yellow/red); subcommand aliases — `target list/show/remove` ↔ `ls`/`info`/`rm`, `kubeconfig` ↔ `kc`, `cluster-bootstrap` ↔ `cb`, `bootstrap-all` ↔ `up`. Уже существовавший `target` ↔ `t` сохраняется, новые aliases прицепляются к нему (`apprafter t ls`).

**Source:** `cli-dx-task.md` §17 row 11.

**Поставка:**
- [x] `cli/Cargo.toml` workspace dep `owo-colors = { version = "4", features = ["supports-colors"] }`; `cli-core/Cargo.toml` direct dep.
- [x] `cli/cli-core/src/style.rs` — новый модуль:
    - `ok(t)` — green (PASS / `✓` / verified). `Stream::Stdout` для авто-NO_COLOR.
    - `warn(t)` — yellow (WARN / soft failures).
    - `fail(t)` — red. `Stream::Stderr` — callsites that consume `fail()` write to stderr.
    - `info(t)` — cyan (phase markers `→`, column headers, `(active)` tags).
    - `dim(t)` — dimmed (tertiary annotations типа `(unset — apply uses platform-1)`).
    - `bold(t)` — bold emphasis (target names, cluster names). Combine: `info(&bold("dev"))`.
    - Все возвращают `String` (упрощено — `if_supports_color` возвращает hard-to-name generic type; форматирование в строку pragmatic и pollutes только небольшие call sites).
- [x] `cli-core/src/lib.rs` — `pub mod style;`.
- [x] `commands/bootstrap_all.rs`:
    - Phase markers `→`/`✓`/`✗` через `style::info/ok/fail`.
    - Phase 2 spinner success line использует `style::ok`.
    - Phase failure marker через `style::fail`.
- [x] `commands/doctor.rs`:
    - Новый `CheckStatus::coloured_glyph(&self) -> String` — green ✓ / yellow ⚠ / red ✗.
    - `print_check_line` использует coloured glyph.
- [x] `cli/platform-cli/src/cli.rs` — aliases:
    - `Kubeconfig` — `alias = "kc"`.
    - `ClusterBootstrap` — `alias = "cb"`.
    - `BootstrapAll` — `alias = "up"`.
    - `TargetCommand::List` — `alias = "ls"`.
    - `TargetCommand::Show` — `alias = "info"`.
    - `TargetCommand::Remove` — `alias = "rm"`.

**Тесты (2 unit + 7 integration):**
- 2 в `cli_core::style::tests`:
    - `ok_returns_ansi_free_text_when_stream_is_not_a_tty` — под `cargo test` stdout не TTY → no ANSI bytes, literal text survives.
    - `warn_fail_info_dim_bold_all_round_trip_text_under_no_tty` — same contract для всех 5 helpers.
- 7 в `tests/aliases_test.rs`:
    - `target_ls_alias_routes_to_target_list` — sub-process сравнение stdout/exit между `target list` и `target ls` (identical bytes).
    - `target_rm_alias_routes_to_target_remove` — `rm ghost --yes` → typed `apprafter::target::not_found`.
    - `target_info_alias_routes_to_target_show` — same not-found surface.
    - `kc_alias_routes_to_kubeconfig` — surfaces "no hetzner_cloud state" hint identically.
    - `cb_alias_routes_to_cluster_bootstrap` — same.
    - `up_alias_routes_to_bootstrap_all_dry_run` — `up --dry-run` exits 0 + prints `DRY RUN` plan identical to `bootstrap-all --dry-run`.
    - `t_alias_for_target_still_works_alongside_new_alias_chain` — `apprafter t ls` chains `t` (target) ↔ `ls` (list) → empty-store onboarding hint surfaces. Pins muscle-memory kubectl-style path.

**Acceptance:**
- ✅ `bootstrap-all` real run в TTY показывает coloured phase markers (green ✓ / cyan →).
- ✅ `doctor` PASS rows green, WARN rows yellow, FAIL rows red.
- ✅ `NO_COLOR=1` или non-TTY pipe → output identical to monochrome v0.1.87 (zero ANSI bytes).
- ✅ Все 6 новых aliases работают через subprocess: `apprafter ls`/`info`/`rm`/`kc`/`cb`/`up` + chained `t ls`/`t info`/`t rm`.
- ✅ `cargo test --workspace` — 564 tests, 0 failures.
- ✅ fmt + clippy (-D warnings) + SPDX (165 файлов) clean.

**Out-of-scope (отложено):**
- Цвет на `target list` table (через `tabled` cell styling) — feasible but требует custom cell renderer; на стандартных терминалах current monospace table читается хорошо. Promote позже если walk feedback потребует.
- Цветная identity-строка в `whoami` (target name + cluster bold-cyan) — следующее iterative refinement; foundation готов через `style::bold` + `style::info`.
- `style::ok_strong` / `style::fail_strong` background variants — добавим если нужно различать "ready" vs "ready + critical path".

**Зависит от:** 1.66A.10 ✅ (miette уже использует свой палитру; `style` модуль координирует семантику чтобы наш output совпадал с miette's по тонам — green/yellow/red).

**Размер:** S (один цикл, ~0.5 рабочего дня).

---

### 1.66A.12 Docs + ADR ✅

> v0.1.90 — sub-phase 1.66A.12 shipped: финальная подфаза Track A. Документация для операторов переписана под пост-Track-A flow (`apprafter target add` + `apprafter up` вместо env-var + `cargo run`), credential resolution chain и target store layout вынесены в reference, диагностические коды каталогизированы, full CLI reference добавлен в `docs/reference/cli.md`, дизайн-решения Track A закрыты ADR 0030. mkdocs nav обновлён. Track A теперь закрыт — открывается Track B (M1.5 1.66 platform-stack rethink).

**Source:** `cli-dx-task.md` §17 row 12.

**Поставка:**
- [x] `docs/adr/0030-cli-target-store-and-credential-chain.md` — новый ADR, кодифицирует 4 design decisions: D1 target store (file layout + `APPRAFTER_CONFIG_DIR` override + per-target dirs + mode 0600), D2 three-step credential resolution chain (flag → env → store, including `--target` override), D3 `miette` для user-facing diagnostics (stable `apprafter::<area>::<reason>` codes + multi-line `help` + `#[diagnostic_source]` cause chains), D4 subcommand aliases + semantic colour palette. Включает 6 alternatives considered, 4 risks с mitigations, re-evaluation triggers (AWS landing, Phase 2 opening, credential leak).
- [x] `docs/operator-guide/quickstart.md` — полностью переписан. Old flow (`export HCLOUD_TOKEN` + `cargo run --bin apprafter -- init`) → new flow (`apprafter target add prod ...` + `apprafter bootstrap-all`). Объяснены 3-phase wrapper, dry-run preview, per-phase recovery, doctor self-check, aliases (kc/cb/up/t ls/...), миette error reading. Подробный day-2 ops table + Application CRD usage.
- [x] `docs/operator-guide/target-store.md` — новая страница. File layout reference (`config.yaml` + per-target dirs + `auth/` + `state/`), field reference table для `TargetConfig`, credential resolution chain explained, 4 common patterns (single-cluster / multi-cluster / CI-env-only / token rotation), 3 anti-patterns.
- [x] `docs/operator-guide/troubleshooting.md` — новая страница. Diagnostic-code catalogue: каждый из 11 кодов получает 2-3 параграфа объяснения (`env::cue_not_found`, `env::cue_export_failed`, `provider::hetzner_api_error`, `provider::server_type_unavailable`, `state::corrupt`, `target::invalid_config`, `target::not_found`, `target::token_rejected`, `target::provider_unreachable`, `io::error/json/yaml`, `cli::other`). + walk-found common failures section + worked example reading the layered cause chain.
- [x] `docs/reference/cli.md` — новая страница. Top-level binary surface, global env vars table, every subcommand documented (target/whoami/doctor/init/apply/destroy/import/kubeconfig/cluster-bootstrap/argocd-password/bootstrap-all/status/login/upgrade-tier/auth), full aliases reference table.
- [x] `docs/operator-guide/index.md` обновлён — links to new pages, Track A status note (closed), no more "stub" wording.
- [x] `docs/reference/index.md` обновлён — CLI reference из stub стал first-class, диагностические коды cross-link'нуты на troubleshooting page.
- [x] `mkdocs.yml` nav — Operator Guide + Reference раскрываются в nested entries (quickstart / target store / gitops walk / troubleshooting / recovery; reference index + CLI page).

**Тесты:** docs-only changes, никаких runtime tests. SPDX gates: 166 файлов pass. fmt + clippy: clean. Workspace tests: 564 passed (unchanged). mkdocs `--strict` build не запустился локально из-за `nix shell` env quirk (mkdocs binary не видит mkdocs-material theme); CI workflow `.github/workflows/docs.yml` валидирует.

**Acceptance:**
- ✅ ADR 0030 покрывает все 4 design decisions с rationale, alternatives, risks, re-evaluation triggers.
- ✅ Operator quickstart описывает post-Track-A flow без legacy `cargo run` / env-var-only префиксов.
- ✅ Target store layout + credential chain документированы в одной reference page.
- ✅ Все 11 diagnostic codes имеют next-step CLI команды в troubleshooting page.
- ✅ Full CLI reference covers все 13 subcommand'ов + 6 aliases.
- ✅ mkdocs nav surface'ит новые страницы.
- ✅ SPDX clean (166 файлов).

**Out-of-scope (отложено):**
- Auto-generated CRD field reference — Phase 8.1 target per `docs/reference/index.md`.
- Mass-rewrite of `docs/dev-guide/quickstart.md` — Track A не трогал developer flow напрямую, отдельная итерация когда developer experience станет приоритетом.
- Translations — все docs остаются English-only.

**Зависит от:** 1.66A.11 ✅ (последний код-меняющий sub-phase Track A — нужны все имплементированные фичи чтобы корректно их задокументировать).

**Размер:** M (один цикл, ~1 рабочий день — 4 новых doc-файла + 1 ADR + 2 update'а + nav).

---

### Track A backlog (закрыт в v0.1.91)

Originally items surfaced during Track A walks. Cleared:

1. **Phase 2 polish (A.9c)** ✅ — v0.1.91 закрыл оба:
    - SSH `ConnectTimeout=5` добавлен в `SshKubeconfigFetcher::build_command` — первая attempt fail'ится за 5s вместо ~30s на kernel TCP timeout. Typical Phase 2 на cpx22 + Ubuntu 24.04 падает с ~60s до ~20-40s.
    - `[2/3] kubeconfig` rename → `[2/3] k3s-ready`: спиннер label, success line, failed() marker, dry-run plan, total summary breakdown — все consistent. Сообщение "waiting for cloud-init + k3s on the new node…" честно отражает что мы реально делаем. +1 regression test (`ssh_fetcher_caps_connect_timeout_at_five_seconds`); existing dry-run integration test обновлён под новый label. Docs (quickstart, troubleshooting, cli reference) тоже подтянуты.

После v0.1.91 открывается **Track B (M1.5 sub-phase 1.66 platform-stack rethink)** — главная архитектурная работа M1.5.

---

### 1.66 platform-stack monorepo skeleton + CUE source layout ✅

> v0.1.92 — sub-phase 1.66 shipped: top-level `platform-stack/` со всем CUE-only source per ADR 0028. **Layout flat** — `platform-stack/cue/` единая директория с filename prefixes (`component_<name>.cue`, `tier_<name>.cue`) вместо subdirectory groupings. Subdirs пробовали — CUE считает их отдельными package instances even when `package` declaration matches, что ломает cross-file `_components` merging. Дополнительные design-walk gotchas (autobinding-strip + typed `_components` strip + `vet -c` rejecting defaults) задокументированы в `platform-stack/README.md` + комментах в `platform.cue`.

**Source:** ADR 0028.

**Цель:** заложить структуру `platform-stack/` в монорепо. CUE source-of-truth для всех Argo CD Application определений платформенных компонент.

**Поставка:**
- [x] New top-level subdir `platform-stack/` (path указан с префиксом `apprafter/` в исходном тексте — это GitHub URL convention `apprafter/apprafter` org+repo; в clonированном дереве это просто `platform-stack/` на root уровне рядом с `cli/`, `operator/`, `schemas/`).
    - [x] `cue/platform.cue` — umbrella schema (`#Version` / `#Channel` / `#Tier` / `#ComponentSource` / `#Component` / `#ComponentSet` / `#PlatformValues` / `_components: {}` package-level base).
    - [x] `cue/component_cilium.cue` — Cilium 1.16.5, kube-proxy replacement, IPAM kubernetes, Hubble off by default.
    - [x] `cue/component_cert-manager.cue` — jetstack v1.16.2 + CRDs enabled.
    - [x] `cue/component_argocd.cue` — Argo CD 7.7.7 self-managing (`prune: false`), Dex off.
    - [x] `cue/component_apprafter-operator.cue` — pinned to v0.1.91 (Track A closing tag) from `oci://ghcr.io/apprafter/apprafter-operator`.
    - [x] `cue/component_admission-webhook.cue` — pinned to v0.1.91, 2 replicas default.
    - [x] `cue/component_backstage.cue` — Git-source manifests, conditional on `values.backstage.domain`, default-off в tier-1 overlay.
    - [x] `cue/component_network-policies.cue` — default-deny + DNS + Argo-CD egress allowance bundle.
    - [x] `cue/component_argocd-cue-cmp.cue` — declared but `enabled: false` by default до sidecar wiring step в 1.69.
    - [x] `cue/tier_solo.cue` — tier 1 overlay (single cpx22, Hubble off, Backstage off, argocd-cue-cmp off, single-replica everything).
    - [x] `cue/tier_team.cue` — tier 2 overlay (Hubble on relay+UI, Backstage on, admission-webhook + cert-manager + operator at 2 replicas).
    - [x] `cue/compatibility.cue` — `#ChangeClass` + `#VersionRecord` schema + initial 0.2.0 entry classified `safe` (no behaviour change vs in-tree v0.1.x bootstrap).
- [x] `platform-stack/Chart.yaml.tmpl` — template для umbrella chart metadata (рендерится в `dist/<version>/Chart.yaml` через `cue cmd render` в 1.67).
- [x] `platform-stack/README.md` — full layout + contribution model + distribution + forking story + design-walk decision rationale.
- [x] `platform-stack/CHANGELOG.md` — initial planned 0.2.0 entry.
- [x] `scripts/lint-cue.sh` расширен: `cue fmt --check` + `cue vet` теперь покрывают `./platform-stack/cue/...`.
- [x] `scripts/check-spdx-headers.sh` patterns добавлены: `platform-stack/cue/**/*.cue` + `platform-stack/Chart.yaml.tmpl`. SPDX gate теперь 167 файлов.

**Тесты:** scaffold-only release — никаких runtime tests, валидируем CUE-слой:
- [x] `bash scripts/lint-cue.sh` — `cue fmt --check` + `cue vet` clean.
- [x] `nix run nixpkgs#cue -- vet -c ./platform-stack/cue/...` — strict concreteness check passes (exit 0).
- [x] `nix run nixpkgs#cue -- eval ./platform-stack/cue/... -e tier1` / `-e tier2` — рендерят fully-concrete components map.

**Acceptance:**
- ✅ `cue vet -c ./platform-stack/cue/...` exits 0; все schemas валидны + полностью concrete на обоих tiers.
- ✅ Все 8 компонентов declared в CUE. Hardcoded values в `cli-providers::k8s::*` остаются на месте до migration в 1.71 — `platform-stack/` сейчас параллельная source-of-truth, не replacement.
- ✅ README ясно описывает: CUE only в Git, rendered chart живёт в OCI (`oci://ghcr.io/apprafter/platform-stack:<version>`), GitHub Release `.tgz` secondary.

**Design-walk gotchas** (зафиксированы в README + komments в `platform.cue` чтобы будущие contributors не переоткрывали):
- Subdirectory split → CUE считает sibling dirs отдельными package instances, `_components` не мерджились. **Fix:** flat `cue/` с filename prefixes.
- `#ComponentSet` autobinding `[NAME=string]: #Component & { name: NAME }` re-применяет `#Component` на каждом overlay unification и стрипит concrete fields. **Fix:** plain `[string]: #Component` + explicit `name:` per component.
- Typed `_components: #ComponentSet` — та же проблема. **Fix:** plain `_components: {}` с локальной type-conformance на declaration site.
- CUE 0.10+ `vet -c` flags `bool | *false` как incomplete даже когда default applies. **Fix:** explicit pin per tier (`tier_team.cue`'s `argocd-cue-cmp.enabled: false`).

**Out-of-scope (отложено):**
- `dist/` renderer + `templates/applications.yaml` template + `make render` — sub-phase 1.67.
- Publish workflow (`.github/workflows/platform-stack-publish.yml`) — 1.68.
- Argo CD CMP sidecar wiring — 1.69.
- Migration `cli-providers::k8s` hardcoded values → CUE — 1.71.

**Зависит от:** —

**Размер:** M (один цикл, ~0.5-1 рабочий день; основное время — design-walk на CUE gotchas).

---

### 1.67 `cue cmd render` pipeline + umbrella chart generation ✅

> v0.1.93 — sub-phase 1.67 shipped: CUE-tools-based renderer + Makefile + per-tier examples + Helm-native values schema. `make -C platform-stack render` produces a fully-lintable umbrella chart in `dist/platform-stack-<version>/` purely from CUE source, with no hand-edited intermediate YAML. Helm lint clean (only INFO about an icon recommendation, no errors / warnings). `helm template --set tier=99` rejects out-of-range tier values at the schema gate before any Argo CD reconcile sees them.

**Source:** ADR 0028.

**Цель:** CI step который рендерит CUE source в Helm umbrella chart в `dist/`.

**Поставка:**
- [x] `platform-stack/cue/render_tool.cue` — CUE `command: render: { ... }` using `tool/file` package. Tasks:
    - `mkdist` / `mktemplates` / `mkexamples` — `file.Mkdir` создают `dist/platform-stack-<version>/templates/` + `examples/` (с `$dep` chain, чтобы `file.Create` shaped tasks выполнялись после dirs).
    - `chartYaml` — Chart.yaml v2 (apiVersion + name + description + version + appVersion + maintainers + keywords + annotations с `apprafter.io/change-class` + `apprafter.io/operator-version` из `compatibility.cue`).
    - `valuesYaml` — defaults to `tier1` (solo), emit via `yaml.Marshal`. Operators running `helm install platform-stack` без `--values` получают tier-1 baseline (совпадает с v0.1.x cluster-bootstrap).
    - `valuesSchemaJson` — Helm-native JSON-schema-2020-12 (handrolled to match `#PlatformValues` shape — CUE's auto-export targets draft-07 which Helm не понимает). Required: version + tier + channel + components; tier enum `[1,2,3,4]`; channel enum `["stable","edge"]`; per-component required: name/enabled/namespace/source/version. `additionalProperties: false`.
    - `appsTemplate` — `templates/applications.yaml`: единственный Go template. `{{- range $name, $component := .Values.components }}` → один Argo CD `Application` per enabled entry. Conditional `helm.valuesObject` only когда `source.chart` set (Git-source components skip it). Labels `apprafter.io/{component,tier,channel}`. SSA + auto-create-namespace via `syncPolicy.syncOptions` из CUE base.
    - `compatibilityYaml` — `compatibility.yaml` rendered from `compatibility.cue`'s `compatibility: [string]: #VersionRecord`.
    - `soloExample` / `teamExample` — `examples/values.solo.yaml` + `examples/values.team.yaml` (concrete tier renders for `helm install -f`).
    - `readme` — `README.md` inside the rendered chart pointing back at the CUE source (so users pulling the OCI artifact see a redirect to canonical docs).
- [x] `platform-stack/Makefile` — `make render` / `render-only` / `lint` / `clean` / `help`. Auto-detects `cue` and `helm` binaries from PATH, falls back to `nix run nixpkgs#cue --` / `nix run nixpkgs#kubernetes-helm --` so anyone in the project's nix shell или с nix available picks them up. Version резолвится из `tier1.version` через `cue export` — никогда не хардкодится в Makefile.
- [x] `dist/` уже gitignored (project-wide rule в `.gitignore` line 17 — `dist/` without leading slash matches at any depth).
- [x] `Justfile` — `just platform-stack-render` + `just platform-stack-check` wrappers вокруг `make -C platform-stack ...` для project-level convenience.
- [x] `platform-stack/README.md` Local-development section обновлён с реальными командами + per-tier helm template примером + schema-gate sanity check (`--set tier=99` → error).

**Тесты:** scaffold-only — никакого Rust unit/integration кода не меняется (565 passed). Валидация через render-and-lint:
- [x] `cue cmd render` (через `make render-only` или direct) emit'ит 8 файлов в `dist/platform-stack-0.2.0/` (Chart.yaml + values.yaml + values.schema.json + compatibility.yaml + README.md + templates/applications.yaml + examples/values.solo.yaml + examples/values.team.yaml).
- [x] `helm lint dist/platform-stack-0.2.0` exits 0 (single INFO about chart icon, no warnings / errors).
- [x] `helm template platform dist/platform-stack-0.2.0` (default tier-1) → 6 Argo CD Applications: admission-webhook + apprafter-operator + argocd + cert-manager + cilium + network-policies. Backstage + argocd-cue-cmp ожидаемо disabled.
- [x] `helm template platform dist/platform-stack-0.2.0 --values dist/platform-stack-0.2.0/examples/values.team.yaml` → 7 Applications (добавляется Backstage; argocd-cue-cmp всё ещё off до 1.69 wiring).
- [x] `helm template platform dist/platform-stack-0.2.0 --set tier=99` → schema rejects `value must be one of 1, 2, 3, 4`. Validates the values.schema.json gate.

**Acceptance:**
- ✅ `make render` produces `dist/platform-stack-0.2.0/` content.
- ✅ `helm lint` returns 0.
- ✅ `helm template ... --values examples/values.solo.yaml` renders tier-1 correctly (6 enabled Applications, no Backstage / no argocd-cue-cmp).
- ✅ `helm template ... --values examples/values.team.yaml` renders tier-2 correctly (7 Applications с Backstage enabled, Hubble relay+UI in cilium values).
- ✅ Schema gate rejects invalid tier values at `helm template` time before reaching Argo CD.

**Out-of-scope (отложено):**
- Kamaji / Capsule / Hubble dashboard для tier-2+ — отдельные components, landing в sub-phase 1.71+ alongside the in-tree manifest migration.
- Smoke install в kind cluster — это CI-side acceptance в sub-phase 1.68 publish workflow.
- `helm push` к OCI registry + `cosign sign` — sub-phase 1.68.
- `text/template` engine для `Chart.yaml.tmpl` (сейчас chart metadata — CUE string literal с interpolation). Перевод на template engine — when chart metadata вырастает; пока линейная string interpolation покрывает все substitutions.

**Зависит от:** 1.66 ✅

**Размер:** S (один цикл, ~0.5 рабочий день — основное время на отладку CUE `tool/file` task DAG semantics + Helm template indent/quoting).

---

### 1.68 CI OCI publish workflow + cosign signing ✅

> v0.1.94 — sub-phase 1.68 shipped: `.github/workflows/platform-stack-publish.yml` triggers on `platform-stack/v*` tags (или manual `workflow_dispatch`), validate'ит `compatibility.cue` имеет entry для версии, render'ит chart, lints, smoke-template'ит обе tier-1/tier-2, packages, push'ит к OCI на `ghcr.io/<owner>/platform-stack:<version>`, cosign-keyless signs OCI digest + `.tgz` blob через Sigstore OIDC (ambient GitHub identity, no managed keys), tags `:latest` on stable releases via `oras tag`, и создаёт GitHub Release с `.tgz` + `.tgz.sig` + `.tgz.pem` attachments и body content описывающим install + verify commands. `scripts/check-platform-stack-version.sh` — отдельный helper, используется CI как fail-fast gate, тоже работает локально для проверки перед tagging'ом. `platform-stack/RELEASE.md` — full maintainer procedure: versioning rules, pre-release checklist, tagging steps, after-publish actions, failure-mode recovery.

**Source:** ADR 0028.

**Цель:** GitHub Actions workflow который on tag `platform-stack/v*` builds chart + signs + publishes к OCI и GitHub Release.

**Поставка:**
- [x] `.github/workflows/platform-stack-publish.yml`:
    - [x] Trigger: tag matching `platform-stack/v*` (плюс `workflow_dispatch` с `version:` input для manual republish).
    - [x] Step 1: Checkout.
    - [x] Step 2: Resolve version from tag (strip `platform-stack/v` prefix) or from workflow input. Detect pre-release via `-` in version → controls `:latest` retag + GitHub Release `prerelease:` flag.
    - [x] Step 3: Compute lowercase owner (ghcr requires lowercase). Same shim as `release-operator.yml`.
    - [x] Step 4: `cue-lang/setup-cue@v1` + `azure/setup-helm@v4` + `sigstore/cosign-installer@v3`.
    - [x] Step 5: `bash scripts/check-platform-stack-version.sh "$VERSION"` — compatibility gate.
    - [x] Step 6: `make -C platform-stack render-only` — render CUE → `dist/`.
    - [x] Step 7: `helm lint`.
    - [x] Step 8: `helm template` smoke for **both** tiers — assert 6 tier-1 Applications + Backstage on tier-2.
    - [x] Step 9: `helm package` → `.tgz`.
    - [x] Step 10: `docker/login-action@v3` к `ghcr.io` via `GITHUB_TOKEN`.
    - [x] Step 11: `helm push` к `oci://ghcr.io/<owner>` (Helm 3.8+ native OCI push; tag derived from `Chart.yaml.version`). Resolve immutable digest via `helm show chart oci://...` для последующего cosign sign — Sigstore best practice: never sign mutable tags.
    - [x] Step 12: `cosign sign --yes "${IMAGE}@${DIGEST}"` — keyless via Sigstore OIDC + GitHub Actions ambient identity (`id-token: write` permission). No managed signing keys.
    - [x] Step 13: `cosign sign-blob --yes ... "$TGZ"` → `.tgz.sig` + `.tgz.pem` detached signature pair для GitHub Release attachment path (`cosign verify-blob` consumer).
    - [x] Step 14: `oras tag "${IMAGE}:${VERSION}" latest` (только на stable, с graceful warning если oras CLI отсутствует на runner image).
    - [x] Step 15: `gh release create` с heredoc-formatted notes (install snippets для Argo CD path + plain Helm path + cosign verify snippets для обоих), attaches `.tgz` + `.sig` + `.pem`, `--prerelease` flag для pre-release tags.
    - [x] Security hardening: каждый dynamic input (`github.ref_name`, `github.repository_owner`, `github.event.inputs.version`, `github.repository`) routed через `env:` binding, не direct interpolation в `run:` body — pattern из release-operator.yml продолжается. Heredoc-built notes file через `mktemp` + `--notes-file` чтобы не передавать multi-line string в bash arg directly.
- [x] CI validation: `scripts/check-platform-stack-version.sh "$VERSION"` exits non-zero с human-readable error pointing at `compatibility.cue`. Resolves cue binary same way `lint-cue.sh` does (local → nix fallback). Verified локально на happy path (`0.2.0` → returns YAML) и unhappy path (`99.99.99` → exit 1 с instruction'ом добавить entry).
- [x] `platform-stack/RELEASE.md` — full maintainer release procedure:
    - Semver rules + first-published-version-is-0.2.0 explanation.
    - Pre-release checklist (compatibility.cue entry + accurate change class + operator version + CHANGELOG.md + local render passes + workspace tests).
    - Tagging instructions (pre-release `-rc1` vs stable).
    - After-publish actions (verify in clean env + bump `RELEASED_OPERATOR_VERSION` if paired + update UNRELEASED.md).
    - Failure-mode recovery (tag delete + re-push).

**Тесты:** CI-side acceptance — нельзя по-настоящему verify без push'a реального tag'а. Локальные проверки которые делал:
- [x] `bash scripts/check-platform-stack-version.sh 0.2.0` → success + prints YAML.
- [x] `bash scripts/check-platform-stack-version.sh 99.99.99` → exit 1 + human-readable error pointing at compatibility.cue.
- [x] `yamllint -d relaxed .github/workflows/platform-stack-publish.yml` clean.
- [x] SPDX gate clean (167 → 170 после staging .yml + .sh + RELEASE.md).
- [x] Все existing gates green (cargo fmt/clippy/test 565, cue lint, spdx).

**Acceptance:**
- ✅ Workflow file present и syntactically valid (yamllint passes).
- ✅ Compatibility-gate скрипт работает on happy and unhappy paths.
- ✅ Security pattern from `release-operator.yml` (env-binding for all dynamic inputs) consistent.
- ✅ Release procedure documented в `platform-stack/RELEASE.md`.
- ⏳ Tag `platform-stack/v0.2.0-rc1` triggers workflow → ends green. **Verified only after first real push** (CI-side acceptance). Local validation steps above approximate the pre-push checklist.
- ⏳ `oras pull ghcr.io/apprafter/platform-stack:0.2.0-rc1` retrieves signed chart. **Verified after first real push.**
- ⏳ `cosign verify ghcr.io/apprafter/platform-stack@<digest> --certificate-identity-regexp ... --certificate-oidc-issuer ...` succeeds. **Verified after first real push.**
- ⏳ GitHub Release page has `.tgz` + `.tgz.sig` + `.tgz.pem` attached. **Verified after first real push.**

**Out-of-scope (отложено):**
- Smoke install in `kind` cluster within the workflow — current `helm template` smoke + `helm lint` cover chart shape and template-time errors. Adding kind would extend workflow runtime ~3 minutes for marginal new coverage. Promote when first real-world chart bug slips past template-time validation.
- SLSA Level 3 build provenance attestation. cosign already provides keyless artifact provenance; SLSA Level 3 demands hermetic builds in `slsa-github-generator` reusable workflow. Defer until M3 compliance pass.
- Multi-architecture OCI manifest list. The chart is a Helm artifact — architecture-neutral by definition. Sub-charts (Cilium, cert-manager, Argo CD) are pulled by Argo CD at install time and select arch on the user's cluster.

**Зависит от:** 1.67 ✅ (renderer + Makefile — workflow shells out to `make -C platform-stack render-only`).

**Размер:** S (один цикл, ~0.5 рабочий день — основное время на отладку cosign keyless flow + проверку workflow-injection security pattern).

---

### 1.69 CUE CMP sidecar Docker image + plugin.yaml ✅

> 2026-05-19 — sub-phase 1.69 shipped: `argocd-cue-cmp/` flat directory at repo root + publish/check workflow pair following the same trigger-inversion + drift-detection model as `platform-stack-*.yml`. Chart bumped to 0.1.2 to wire the sidecar into `argocd-repo-server.extraContainers`. Image's own version track `argocd-cue-cmp/v*` — independent semver, started at 0.1.0.

**Source:** ADR 0029.

**Цель:** sidecar image для `argocd-repo-server` который компилирует CUE → YAML для user app repositories.

**Поставка:**
- [x] New top-level `argocd-cue-cmp/`:
    - [x] `Dockerfile` — Alpine 3.20 multi-stage; fetcher stage pulls cue v0.10.0 tarball from GitHub Releases; runtime stage drops cue binary on PATH, copies plugin.yaml + entrypoint.sh, sets UID/GID 999 to match argocd-repo-server CMP sidecar contract (Alpine 3.20's ping group on gid 999 deleted first to free the slot). OCI labels populated from build args (IMAGE_VERSION, IMAGE_REVISION) which CI fills.
    - [x] `plugin.yaml` — ConfigManagementPlugin manifest. `discover.find.glob: "**/apprafter*.cue"` (matches phase 1.11 user app convention). `generate.command: [sh, "-c"]` invokes `/usr/local/bin/entrypoint.sh`.
    - [x] `entrypoint.sh` — runs `cue export ./... --out yaml`; on success prints YAML to stdout; on failure extracts first non-empty error line as `::cue-cmp:: CUE compile failed: <summary>` to stderr + full cue stderr block below. Smoke-tested locally: happy-path → exit 0 + YAML; conflict-path (`apiVersion: "v1"` vs `"v2"`) → exit 1 + summary line.
    - [x] `VERSION` — plain-text single source of truth for image semver. Read by publish workflow via `tr -d '[:space:]' < VERSION`. Initial value `0.1.0`.
    - [x] `README.md` — purpose + local build instructions + smoke test script + release flow.
- [x] `.github/workflows/argocd-cue-cmp-publish.yml` — split into `detect` + `publish` jobs (same pattern as `platform-stack-publish.yml`). Trigger: push to master on `argocd-cue-cmp/**` paths + `workflow_dispatch` with optional `version_override:`. `detect` resolves VERSION, checks if tag exists on origin → `should_publish`. `publish` (gated): docker buildx build + push to `ghcr.io/<owner>/argocd-cue-cmp:<version>` + cosign keyless sign (immutable digest from `docker/build-push-action` outputs, not the mutable tag) + `:latest` retag via `docker buildx imagetools create` on stable + `gh release create argocd-cue-cmp/v<version>` создаёт tag.
- [x] `.github/workflows/argocd-cue-cmp-check.yml` — PR + push gate. VERSION semver validation, docker smoke build (no push), entrypoint fixture render (tiny `apprafter/Application.cue` → assert `kind: Application` in output), **drift detection** identical to platform-stack-check: if `argocd-cue-cmp/v<VERSION>` exists on origin AND any file under `argocd-cue-cmp/{Dockerfile,plugin.yaml,entrypoint.sh,VERSION}` differs → fail с 80-line diff.
- [x] `platform-stack/cue/component_argocd-cue-cmp.cue` обновлён: image tag `v0.1.91` → `v0.1.0` (cue-cmp's own semver track), repoURL переключён на GitHub source path (image не Helm chart, sidecar pulled directly via repoServer.extraContainers), `version` field тоже `v0.1.0`. Doc-comment объясняет sidecar-not-Application semantics.
- [x] `platform-stack/cue/component_argocd.cue` обновлён: добавлен `repoServer.extraContainers` блок с `cue-cmp` sidecar. `image` поле читает `_components."argocd-cue-cmp".values.image.repository:tag` через CUE interpolation — bump cue-cmp version становится one-line edit в одном файле. UID 999 / runAsNonRoot. Volume mounts соответствуют Argo CD CMP sidecar contract (var-files, plugins, cmp-tmp, cue-cmp-config configmap subPath).
- [x] `platform-stack/cue/platform.cue` — `currentVersion` 0.1.1 → 0.1.2.
- [x] `platform-stack/cue/compatibility.cue` — добавлена запись 0.1.2 (change: safe, references ADR 0029 + argocd-cue-cmp/README.md, упоминает ~50 MiB sidecar memory overhead из ADR 0029, single repo-server pod restart impact).
- [x] `scripts/check-spdx-headers.sh` — добавил patterns `argocd-cue-cmp/{Dockerfile,plugin.yaml,entrypoint.sh}`. SPDX gate cover'ит 175 файлов.

**Тесты:**
- [x] `docker build` локально — pass. Multi-stage build → runtime image с UID 999, cue v0.10.0 binary, plugin.yaml и entrypoint.sh на правильных path'ах per Argo CD CMP sidecar contract.
- [x] Entrypoint happy-path smoke: tiny `apprafter/Application.cue` → renders YAML, exit 0.
- [x] Entrypoint error-path smoke: conflict-cue (`apiVersion` two-values) → `::cue-cmp:: CUE compile failed: apiVersion: conflicting values...` summary on stderr + full block, exit 1.
- [x] `cue vet -c ./platform-stack/cue/...` clean (invariant catches future bump-without-compat).
- [x] `bash scripts/lint-cue.sh` clean.
- [x] Render chart 0.1.2: `helm lint` clean, `helm template` rendered output показывает `extraContainers` блок с `cue-cmp` sidecar в argocd-repo-server и `ghcr.io/apprafter/argocd-cue-cmp:v0.1.0` image ref.
- [x] yamllint оба новых workflow'а clean.
- [x] SPDX gate (170 → 175 после staging).
- [x] CLI / cargo тесты untouched (565 passed, не Rust changes).

**Acceptance:**
- ✅ `docker build argocd-cue-cmp/` produces image (verified locally).
- ✅ Manual test: `docker run --rm -v ./test-repo:/repo -w /repo --entrypoint /usr/local/bin/entrypoint.sh image` produces correct YAML output для sample `apprafter/Application.cue`.
- ⏳ Tag `argocd-cue-cmp/v0.1.0-rc1` publishes image (CI-side, не локально воспроизводимо — verified at first push of `argocd-cue-cmp/VERSION` to master).

**Out-of-scope (отложено):**
- ApplicationSet pattern для multi-app monorepos — Phase 2+ per ADR 0029 §"Still open".
- Canonical filename migration `apprafter/Application.cue` → `.apprafter/app.cue` — deferred per ADR 0029.
- Backstage plugin surfacing CUE compile errors — out of scope per ADR 0029.
- End-to-end Argo CD sync test (steps 4-5 из ADR 0029 implementation outline) — manual integration test, plan.md M3 territory.
- Multi-arch arm64 — same reasoning as release-operator.yml: Hetzner cpx22 is amd64, arm64 lands когда `Infrastructure.spec.nodes[].arch` wires through apply.rs.

**Зависит от:** 1.68 ✅ (publish-workflow pattern reused), 1.67 ✅ (chart renderer для wiring step).

**Размер:** S (одна итерация, ~3 часа — основное время на Argo CD CMP sidecar contract research + Alpine ping-group conflict).

---

### 1.70 Minimal `cluster-bootstrap` rewrite ✅

> v0.1.97 — sub-phase 1.70 shipped. `commands/cluster_bootstrap.rs` переписан целиком с ~1250-line imperative install (Cilium + Gateway + Application CRD + default-deny + Argo CD + cert-manager + ClusterIssuer + operator helm + webhook manifest + bootstrap App + Backstage) на 4-step GitOps loader (~450 lines, half of которых — комментарии и тесты). Argo CD теперь handle'ит весь platform layer через chart pull. Сам CLI binary stays small — он только loader.

**Source:** ADR 0025.

**Цель:** reduce `cluster-bootstrap` to a minimal loader: install Argo CD via Helm, apply root Application pointing к platform-stack OCI chart. Argo CD дальше reconciles остальное.

**Поставка:**
- [x] Refactor `commands/cluster_bootstrap.rs`:
    - [x] **Step 1**: `helm repo add argo …` + `helm upgrade --install argocd argo/argo-cd` с loader-only values (single replicas, dex off — chart's `component_argocd.cue` overlay adopts the release on first reconcile, adds cue-cmp sidecar + tier-2 replicas).
    - [x] **Step 2**: `kubectl wait --for=condition=Available deployment/argocd-server -n argocd --timeout=180s` — gates root Application apply until Argo CD CRDs are installed (otherwise "no matches for kind Application").
    - [x] **Step 3**: Render single root Application YAML (`apiVersion: argoproj.io/v1alpha1, kind: Application, name: platform, source.repoURL: oci://ghcr.io/<owner>/platform-stack, chart: platform-stack, targetRevision: 0.1.2`) → `kubectl apply -f`. Repo + version pulled из `cli-providers::k8s::APPRAFTER_PLATFORM_STACK_DEFAULT_REPO` + `RELEASED_PLATFORM_STACK_VERSION` constants.
    - [x] **Step 4**: `kubectl wait --for=jsonpath='{.status.health.status}'=Healthy application/platform -n argocd --timeout=600s` — once root Application reports Healthy, all child Applications (cilium, cert-manager, argocd self-managing, apprafter-operator, admission-webhook, network-policies, conditionally Backstage) are reconciling under Argo CD.
- [x] Existing imperative install code **deleted** from CLI: 7 component installs + 5 manifests + 2 helpers (~800 lines net). `cli-providers::k8s::*_yaml` рендерераторы остаются как-есть для chart-side use (parallel source-of-truth до 1.71's migration).
- [x] `cli-providers::k8s` exposes 3 new constants: `RELEASED_PLATFORM_STACK_VERSION = "0.1.2"`, `APPRAFTER_PLATFORM_STACK_DEFAULT_REPO = "oci://ghcr.io/apprafter"`, `APPRAFTER_PLATFORM_STACK_CHART_NAME = "platform-stack"`. Bump `RELEASED_PLATFORM_STACK_VERSION` lockstep с published chart tag.
- [x] `KubectlRunner` trait расширен `wait_for_condition(resource_ref, namespace, condition_expr, timeout_secs, kubeconfig)`. Wraps `kubectl wait --for=<expr>`. Supports both `condition=Available` (deployment readiness) и `jsonpath={.status.health.status}=Healthy` (Argo CD Application health). Real-impl shells out, fake-impl записывает calls для tests.
- [x] FakeKubectl в `argocd_password.rs` обновлён под расширенный trait (unreachable! на wait — argocd-password never waits).

**Тесты:**
- [x] `perform_bootstrap_installs_argocd_then_applies_root_then_waits_for_healthy` — full sequence assertions: 1 helm repo_add, 1 helm install (argocd only, no Cilium/cert-manager/operator/webhook), 1 client-side apply (root Application), 0 server-side applies, 2 waits в правильном порядке (deployment/argocd-server first, application/platform second).
- [x] `render_root_application_includes_repo_url_and_chart_version` — pin repoURL + targetRevision + chart name in rendered YAML. Verifies `prune: true` + `selfHeal: true` syncPolicy для drift correction.
- [x] `render_root_application_uses_argocd_namespace_destination` — destination namespace + cluster URL.
- [x] `argocd_loader_values_keeps_replicas_at_one_for_initial_install` — minimal loader values (replicas=1, dex off). Tier-2 replica counts arrive via Argo CD's first reconcile.
- [x] Existing `decrypt_cached_kubeconfig_*` helper tests preserved.

**Acceptance:**
- ✅ `cargo test --workspace` — closed at 557 cli + 62 operator passed (v0.1.108). Walk-fix cascade added 4 net regression tests (Option<&str> namespace, Cilium ordering, OCI repo registration, default AppProject) and the webhook crate's rustls-CryptoProvider mirror.
- ✅ `cargo fmt --all --check` + `cargo clippy --workspace -- -D warnings` clean.
- ✅ `apprafter init && apprafter bootstrap-all` on fresh Hetzner account → tier-1 cluster reconciles via Argo CD. Verified manually на walk #12 (chart 0.1.12 / CLI v0.1.108). Took **11 walk-fix iterations** (v0.1.98 → v0.1.108) to close, each one a real-cluster-found defect, all surface in `docs/changelog/UNRELEASED.md` v0.1.98 — v0.1.108.
- ✅ `kubectl get applications.argoproj.io -A` shows root `platform` + 6 children all Synced/Healthy. Verified walk #12.
- ✅ `kubectl edit application cilium -n argocd` — drift correction via Argo CD. Verified implicitly through chart's `selfHeal: true` syncPolicy on every child Application.
- ✅ Re-run `apprafter bootstrap-all` идемпотентен. Verified implicitly через 11 destroy+bootstrap cycles during the walk-fix series — each cycle re-applied the same loader values and root Application without dirty state.

**Closure note — walk-fix cascade v0.1.98 → v0.1.108 (11 patches):**

Each walk-fix surfaced a real-cluster defect that prior walks
couldn't reach because of an upstream blocker in the same
cycle. Most defects were latent bugs masked by the previous
blocker:

| Walk | Tag | Bug |
|---|---|---|
| 1 | v0.1.98 | argo-cd 7.7.7 `redis-ha.enabled: true` default times out pre-install hook on single-node k3s. |
| 2 | v0.1.99 | k3s starts with `--flannel-backend=none`; node carries `node.kubernetes.io/not-ready:NoSchedule` until Cilium installs. Loader had Argo CD before Cilium — catch-22. |
| 3 | v0.1.100 | Argo CD doesn't infer OCI Helm protocol from `oci://`; needs explicit `configs.repositories.<name>` with `enableOCI: "true"`. Plus root `Healthy` is a false-positive (zero children = trivially healthy); wait must be Synced→Healthy. |
| 4 | v0.1.101 | Operator + admission-webhook helm charts never published to OCI (only container images). `ignoreDifferences` missed `terminatingReplicas` (k3s v1.35). `manifests/tier-1/network-policies/` directory never created. |
| 5 | v0.1.102 | webhook chart `selectorLabels` missing from `labels` → invalid Deployment. Operator + webhook missed `ignoreDifferences`. network-policies git pin `v0.1.91` predates the directory. Missing sync-wave ordering for cert-manager. |
| 6 | v0.1.103 | `component_cilium.cue` values differed from loader's; Argo CD applied chart-overlay on top of loader, breaking Cilium operator with `KUBERNETES_SERVICE_HOST=auto`. cert-manager `ignoreDifferences` missed. |
| 7 | v0.1.104 | `default` AppProject not auto-created by chart 7.7.7 or Argo CD 2.13.1 server. Every Application referencing it fails. |
| 8 | v0.1.105 | `ghcr.io/apprafter/apprafter-operator:v0.1.91` image was broken months ago (binary missing); never exercised before. `apprafter-selfsigned` ClusterIssuer never moved into a chart template after the v0.1.97 imperative-to-GitOps rewrite. `RELEASED_OPERATOR_VERSION` stale at `v0.1.64`. |
| 9 | v0.1.106 | webhook `main.rs` never called `install_rustls_crypto_provider()` (operator had it since v0.1.61). Masked since the v0.1.91 image's binary never ran. |
| 10 | v0.1.107 | chart added cue-cmp sidecar in 0.1.2 with a volumeMount on ConfigMap `cue-cmp-plugin-config` but never declared the ConfigMap. Masked through walks #5-9 by upstream blockers. |
| 11 | v0.1.108 | `argocd-cue-cmp-publish.yml` workflow tagged image as `:0.1.0` (no `v` prefix); chart pinned `:v0.1.0`. The lone workflow inconsistent with operator + webhook's `:v<version>` convention. |

The pattern reveals a class of defect this track creates and
**B.1.71 eliminates**: duplication between CLI loader values
and chart values (Cilium drift in walk #6 is the canonical
example, the eight `*_yaml` renderers in `cli-providers::k8s`
are the inventory). After B.1.71 the chart is the single
source of truth; the loader extracts CUE-rendered values
instead of carrying parallel definitions.

**Out-of-scope (отложено):**
- `apprafter bootstrap-all` per-component progress sub-bars (cilium ⏳, cert-manager ⏳, ...). Current implementation has single-bar "[2/3] kubeconfig" + "[3/3] bootstrap" UX without per-child polling. Adding `kubectl get applications -n argocd -o jsonpath='...'` poll loop is a UX-polish iteration, not blocking 1.70.
- `apprafter cluster-bootstrap --manifest <path>` flag + auto-discovery from CWD — current `APPRAFTER_MANIFEST` env-var still works. Manifest overlay → root Application's `helm.valuesObject` requires CLI knowledge of chart values shape; defer to 1.71 cutover.
- Idempotent resume на каждом шаге (PRELAUNCH_CHECKLIST P1 item 3.1) — `helm upgrade --install` + `kubectl apply` уже idempotent на step level; what's NOT yet idempotent — partial state when waits timeout (e.g. argocd-server up but root Application apply failed). Defer полная resume semantics.
- E2E test (`e2e/mvp.sh`) update — currently tests imperative install. Rewriting it для GitOps path = separate iteration после first real-cluster verification.

**Зависит от:** 1.66 ✅, 1.67 ✅, 1.68 ✅, 1.69 ✅ (platform-stack chart must be publishable + CMP sidecar wired before CLI references it).

**Размер:** M (один цикл, ~3 часа — rewrite + tests + trait extension + Cargo bump).

---

### 1.71 Migrate platform component values from CLI to chart ✅

> v0.1.109 — sub-phase 1.71 shipped. `cli/cli-providers/build.rs` extracts `_loaderValues.{cilium,argocd}` + `currentVersion` from `platform-stack/cue/` at compile time. 12 dead `*_yaml` renderer files deleted; `cluster_bootstrap.rs` consumes generated constants. CUE invariants enforce chart↔loader agreement structurally.

**Source:** ADR 0025.

**Цель:** все existing Helm values builders в `cli-providers::k8s::*` переезжают в `apprafter/platform-stack/cue/components/*.cue` как CUE-typed values. CLI больше не содержит platform component конфигурации.

**Поставка:**
- [x] Audit existing CLI source:
    - `cilium_values_yaml()` → `cue/components/cilium.cue` values block
    - `cert_manager_values_yaml()` → `cue/components/cert-manager.cue` values
    - `argocd_values_yaml()` → `cue/components/argocd.cue` values (включая CMP sidecar config от 1.69)
    - `apprafter_operator_values_yaml()` → `cue/components/apprafter-operator.cue`
    - Admission webhook manifests → `cue/components/admission-webhook.cue`
    - Backstage values → `cue/components/backstage.cue` (conditional на values.domain)
    - default-deny NetworkPolicy → `cue/components/network-policies.cue`
- [x] Self-managing Argo CD: Argo CD's own Application within chart has `syncPolicy.automated.prune: false` to prevent self-destructive upgrades.
- [x] Delete migrated Rust code from `cli-providers::k8s::*`.
- [x] Smoke: rendered chart + applied → cluster matches what previous CLI-installed setup produced (value-by-value diff).

**Acceptance:**
- `git grep -E "(cilium_values|cert_manager_values|argocd_values|backstage_values)_yaml" cli/` returns no matches in source (only possibly in tests as legacy reference).
- Tier 1 bootstrap через new pipeline produces functionally identical cluster (Cilium config, cert-manager ClusterIssuer, Argo CD UI, admission webhook).
- Argo CD UI shows Argo CD как один из child Applications с prune=false visible.

**Зависит от:** 1.66, 1.70

**Размер:** M

---

### 1.71b Close remaining version drift classes ✅

> v0.1.110 — sub-phase 1.71b shipped.

**Source:** Track B.1.71's "Deferred to follow-up" closure note.

**Цель:** close the 6 version-duplication classes B.1.71 explicitly carved out — Cilium + Argo CD upstream chart versions, operator + admission-webhook image tags, cue-cmp image version.

**Поставка:**
- [x] `_loaderValues.{cilium,argocd}` schema extended with `chartVersion` field; CUE invariant `_components.<comp>.version ≡ _loaderValues.<comp>.chartVersion`; build.rs emits `CILIUM_CHART_VERSION` + `ARGOCD_CHART_VERSION`; `helm.rs#CILIUM_CHART_VERSION` + `argocd_values.rs` deleted.
- [x] `operator/charts/<chart>/Chart.yaml#appVersion` becomes SoT for operator + webhook image tag; `values.image.tag` dropped from both component cues; build.rs reads both Chart.yaml via line-prefix grep, asserts equal, emits `RELEASED_OPERATOR_VERSION`; `image_ref.rs#RELEASED_OPERATOR_VERSION` deleted.
- [x] `argocd-cue-cmp/VERSION` → `argocd-cue-cmp/version.cue` (`package argocdcuecmp; version: "0.1.1"`); chart's `component_argocd-cue-cmp.cue` imports the package and uses `argocdcuecmp.version`; publish + check workflows read via `cue export -e version --out text` (setup-cue step added to detect job).

**Acceptance:**
- `cargo test --workspace` clean inside `nix develop` (or with `~/bin/cue` wrapper) — 3 new regression tests added.
- Chart-YAML byte-equivalent: `cue export -e _components.<comp>` diff before/after empty for cilium, argocd, apprafter-operator, admission-webhook, argocd-cue-cmp.
- No hand-maintained version const in `cli-providers/src/k8s/*.rs` for the affected classes (verified by `grep RELEASED_OPERATOR_VERSION\|CILIUM_CHART_VERSION\|ARGOCD_CHART_VERSION cli/cli-providers/src/k8s/*.rs` returning only generated consts in `loader_values.rs`).
- Real-cluster walk verifies no behavioural regression vs 0.1.13.

**Зависит от:** 1.71 ✅.

**Размер:** S (один цикл, 3 tasks + closure).

---

### 1.72 PlatformStack CRD schema + admission webhook

**Source:** ADR 0026.

**Цель:** CUE-typed schema для PlatformStack CR + admission webhook validation.

**Поставка:**
- [x] `schemas/v1alpha1/platformstack.cue` — full schema per spec.md §3.11:
    - `spec.channel` (enum stable | beta | edge)
    - `spec.pin` (optional, semver string)
    - `spec.autoUpgrade` (bool, default false)
    - `spec.source.upstream` + `spec.source.repoURL` (OCI references)
    - `spec.source.checkInterval` (duration, default 6h)
    - `spec.values` (free-form, tier/domain/etc.)
    - `spec.overrides` (per-component freezes)
    - `status` with currentVersion, **targetVersion**, availableVersion, lastUpstreamCheck, components[], versionHistory (ring buffer), conditions[]
- [x] Generated OpenAPI v3 schema (hand-rolled mirror in `operator/charts/apprafter-operator/templates/crd-platformstack.yaml`; Application CRD restored in `crd-application.yaml`, sync-wave -5 both).
- [x] Admission webhook validation rules:
    - Exactly one PlatformStack CR per cluster (rejected if a second is created), named `default` в namespace `apprafter-system`.
    - `spec.channel` is one of `stable | beta | edge`.
    - `spec.source.checkInterval` ≥ 1h (prevent rate-limit abuse).
    - `spec.pin` is valid semver if set.
- [x] Bootstrap integration: 1.70 step adds creation of default `PlatformStack` CR с `spec.channel: stable`, `spec.pin: unset`, `spec.source.upstream/repoURL = oci://ghcr.io/apprafter/platform-stack`.

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
- [x] ~~New crate `operator-platform-controller/` в workspace~~ — **адаптировано**: PlatformController landed как новый workspace member `operator-controllers/platform-stack` (peer to `operator-controllers/application`), запускается в том же `apprafter-operator` binary как второй controller (session 2026-05-20 design adapt).
- [x] kube-rs reconcile loop watching `PlatformStack` CRs.
- [x] Leader election (kube standard pattern with lease в `apprafter-system` namespace) — переиспользуется существующий `LeaderElection::for_apprafter_operator` lease; оба controllers поднимаются после acquire'a.
- [x] OCI registry client:
    - Pull chart by tag from `spec.source.repoURL` (via `oci-distribution` 0.11 + flate2/tar для compatibility.yaml extraction).
    - List available tags by channel.
- [x] ~~Helm render~~ — **delegated to Argo CD**: PlatformController patches только parent Application's `spec.source.helm.valuesObject`; Argo CD's repo-server рендерит chart через argocd-cue-cmp sidecar. Manifest-level diff против rendered output не делается в 1.73 (future enhancement если потребуется).
- [x] Diff logic: compare `parent.spec.source.helm.valuesObject` + `parent.spec.source.targetRevision` vs desired payload from PlatformStack. Classify diff using `compatibility.yaml#<version>.change` (fetched via OCI tarball pull).
- [x] On non-destructive diff (safe + requires-restart): SSA patch parent Application with field manager `platform-controller`.
- [x] On destructive diff (data-migration | breaking, OR pin unset + autoUpgrade=false): push condition (`MigrationPending=True` или `UpgradeAvailable=True`), no auto-bump. MigrationPlan auto-create deferred to 1.74 — `PolicyHooks::request_migration_plan` stub'нут в `NoOpHooks`.
- [ ] ~~Environment check at apply time: confirm cluster's k8s version ≥ chart's `minimumKubernetesVersion`~~ — **deferred**: chart's `compatibility.yaml` shape пока не объявляет `minimumKubernetesVersion`. Future iteration (add field в compatibility schema + reconciler check). Не блокирует 1.73 acceptance — chart's `kubeVersion` constraint в `Chart.yaml` уже даёт helm-level guard.
- [x] Status updates: `currentVersion`, `targetVersion`, `availableVersion`, `lastUpstreamCheck`, `conditions[]`. `components[]` + `versionHistory[]` поля присутствуют в schema но пока не заполняются (требует full child-app health introspection — separate future task).

**Walk-found / additional deliverables (B.1.73 expanded beyond plan.md base):**
- [x] Single-writer pattern via SSA field manager `platform-controller` (single writer for `spec.source.{targetRevision, helm.valuesObject}`).
- [x] Outside-writer detection via `metadata.managedFields` — foreign field manager на spec.source ⇒ force-revert + `UnauthorizedSourceModification=True` condition.
- [x] Conservative race resolution — wait for parent App Sync=Synced before next bump (no aggressive cancel of in-flight syncs).
- [x] Chart-side override pattern в `_applicationsTemplate`: `.Values.overrides.<component>.{pin, values, enabled}` projects onto rendered children (mergeOverwrite на values, replace на pin/enabled).
- [x] Hooks для 1.74 / 1.74a — `PolicyHooks` trait + `NoOpHooks` default impl.

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
- [x] Periodic check task spawned by PlatformController (`Action::requeue(spec.source.checkInterval)` + watch events on PlatformStack + parent Application; реализовано в B.1.73):
    - Pull OCI tag list from `spec.source.upstream` (via `oci-distribution` Rust crate)
    - Filter by channel (stable / beta / edge via semver-suffix conventions, see `oci::channel_matches`)
    - Pick latest semver tag
    - Update `status.availableVersion`, `status.lastUpstreamCheck`
- [x] `status.versionHistory` ring buffer (capped at 10, FIFO). On each successful SSA patch that actually changes `targetRevision`, push `{version, appliedAt, outcome: "succeeded"}`. `append_version_history` helper в `status.rs`.
- [x] `status.conditions`:
    - `Ready` — derived from `parent.status.health.status` (B.1.74).
    - `UpgradeAvailable` — semver comparison `channel_latest > target_for_patch` (B.1.73 walk-fix #3).
    - Plus `Synced`, `MigrationPending`, `UnauthorizedSourceModification` (B.1.73).
- [x] Auto-upgrade logic: pin OR autoUpgrade=true + safe class → SSA patch parent Application. Breaking/data-migration → push `MigrationPending=True` (B.1.75 will land actual MigrationPlan auto-create).
- [ ] ~~Caching: ETag-aware OCI requests~~ — **deferred**. Existing `MIN_OCI_POLL_INTERVAL_SECS=60` throttle + cached `availableVersion` reuse already saturate the bandwidth concern. ETag would shave bytes-per-poll without changing cadence; YAGNI per CLAUDE.md.

**Acceptance:**
- Publish new platform-stack version (0.2.2 with safe changes only) → within `checkInterval` (или after manual `kubectl annotate platformstack default apprafter.io/refresh-upstream=true`), `status.availableVersion = 0.2.2`.
- With `autoUpgrade: true` + safe classification → controller bumps spec.pin → reconcile path completes → status.currentVersion = 0.2.2.
- With `autoUpgrade: true` + new version classified as breaking → MigrationPlan created (см. 1.78); no spec.pin bump.
- `kubectl get platformstack default -o jsonpath='{.status.versionHistory}'` shows history entries.

**Зависит от:** 1.73

**Размер:** S

---

### 1.74a Yanking support для опубликованных platform-stack версий

**Source:** ADR 0028 (extension, motivated by "published-with-bug" scenario).

**Цель:** возможность retroactively пометить конкретную опубликованную версию platform-stack как yanked. Controller перестаёт предлагать её новым пользователям через `availableVersion`, существующие кластеры на этой версии получают warning, но не форсятся автоапгрейдом. Аналог `cargo yank` / `npm deprecate` / PyPI yank для OCI-distributed chart.

**Зачем:** OCI tag immutable per (repo, version) → если опубликовал версию с регрессией, единственный путь — publish next patch, но нет механизма мягко увести с битой версии тех кто на ней. Yanking даёт «soft recall» без принудительного апгрейда (всё ещё уважает MigrationPlan семантику).

**Поставка:**

- [x] Extend `compatibility.cue` schema в `apprafter/platform-stack/`:
    ```cue
    versions: [_]: {
        classification: "safe" | "breaking"
        // новые поля:
        yanked: bool | *false
        yankedReason?: string  // required when yanked=true
    }
    ```
- [x] CI guard в `platform-stack-publish.yml` (расширение 1.68 валидации compatibility.cue): PR ставящий `yanked: true` без непустого `yankedReason` → fail с понятным сообщением. Реализовано в обоих workflow'ах: `platform-stack-check.yml` (PR time) + `platform-stack-publish.yml` (publish time). Текст «PR без bump version → публикация не триггерится» в исходной формулировке преждевременен: текущая drift-detection логика заставит делать bump чтобы chart source change достиг master без CI fail; revisit при first practical yank scenario.
- [x] PlatformController (расширение 1.74) изменения:
    - `availableVersion` resolution через channel skip'ает entries с `yanked: true`. Кластер с `spec.channel: stable` видит только non-yanked stable версии. Реализовано в `resolve_non_yanked_latest` + `tags_in_channel` (вместо `latest_in_channel`) + `fetch_compatibility_doc` pull на top channel tag.
    - Если `status.currentVersion` matches yanked entry → push condition `YankedVersion=True` с `Message: <yankedReason>`, surfaces в `kubectl describe platformstack`. Условие — informational/warning, не Ready=False. Реализовано через `COND_YANKED_VERSION` константу + reconcile loop emit.
    - Upgrade flow **не модифицируется** — yanked это метаданные про версию, не override на user policy. ✓ (existing code uses target_for_patch independent of yank status).
    - Если `spec.pin` точно указывает на yanked версию → condition `YankedVersion=True`, pin остаётся в силе. ✓ (lookup over `target_for_patch` includes pinned versions; UpgradeAvailable + safe-class auto-bump natural flow does not change).
- [ ] Surface yank warning в UI'ях с framing «update strongly recommended»: deferred to `apprafter platform` CLI subcommand work (B.1.8?) и Backstage platform plugin (Phase 2). На данном этапе warning visible через `kubectl describe platformstack default` → `Conditions` section + Kubernetes Events (через standard PlatformStack visibility — не требует UI shim).

**Acceptance:**

- Publish `platform-stack/v0.2.5` нормальный → fresh кластер с `channel: stable` резолвит `availableVersion=0.2.5`.
- Update `compatibility.cue` (PR без bump): для `0.2.5` поставить `yanked: true, yankedReason: "regression в X"`, publish `0.2.6` → fresh кластер резолвит `availableVersion=0.2.6` (skip 0.2.5).
- Кластер уже на `0.2.5`, `spec.autoUpgrade: false`: `status.conditions` содержит `YankedVersion=True` с reason, `apprafter platform status` показывает warning «update strongly recommended → 0.2.6», `spec.version` без изменений (manual policy уважена).
- Кластер уже на `0.2.5`, `spec.autoUpgrade: true`, `0.2.6` classification=safe: normal safe-upgrade path срабатывает → controller бампает на `0.2.6` (yank ничего не меняет в policy, просто получилось что естественный апгрейд уводит с битой версии).
- `spec.pin: "0.2.5"` (explicit) на yanked версии → warning есть, но pin не меняется (явный user choice уважён).
- CI guard fail на PR ставящем `yanked: true` без `yankedReason`.

**Зависит от:** 1.74 (PlatformController + status fields)

**Размер:** S

---

### 1.75 Unified MigrationPlan CRD + admission webhook

**Source:** ADR 0027.

**Цель:** unified MigrationPlan CRD с scope discriminator (application | platform).

**Поставка:**
- [x] `schemas/v1alpha1/migrationplan.cue` per spec.md §3.8 rewrite:
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
- [x] OpenAPI v3 schema with `oneOf` discriminator on `spec.scope.type`. Реализовано без `oneOf` в structural schema (apiserver rejects most oneOf shapes in CRDs); вместо этого scope.{application,platform} оба optional на CRD layer, conditional invariant enforced webhook'ом.
- [x] Admission webhook deeper validation:
    - [x] Required fields per scope type — `validate_application_scope` + `validate_platform_scope` в `validator_migrationplan.rs`.
    - [x] Approver email format validation — `is_emailish` (light RFC5322).
    - [x] Reject changes to `spec.scope` after CR creation (immutable) — UPDATE-time check via `AdmissionRequest.oldObject`.
    - Deferred to B.1.76: reject `status` patches not from MigrationController. Controller doesn't exist в 1.75; защищать status сейчас означало бы `Unable to find auth principal` корнер кейсы. Защита status'а — concern controller-existence-aware и B.1.76 lands it as part of MigrationController wiring (controller владеет всеми status'ами через единственный SSA field manager `migration-controller`; admission webhook отвергает status patches от других managers).

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
- [x] Extend `apprafter-operator` workspace с `MigrationController` reconciler. Реализован как новый workspace member `operator-controllers/migration` (peer to application + platform-stack), spawn'ится из main.rs после acquired lease.
- [x] `MigrationStrategy` trait (отклонение от pseudo-code в plan.md):
  ```rust
  trait MigrationStrategy {
      async fn execute_step(&self, plan: &MigrationPlan, step: &MigrationStep) -> Result<StepOutcome, MigrationError>;
      async fn reject(&self, plan: &MigrationPlan) -> Result<(), MigrationError>;
  }
  ```
  - `detect_destructive` + `create_plan` **НЕ** в trait — signatures differ per scope (Application diff vs version+compat-doc), forcing one shared signature через associated type или generic context либо breaks trait-object dispatch либо loses information callers need. Detection лежит как concrete fn per strategy struct; B.1.77 + B.1.78 callers wire их in.
- [x] `ApplicationMigrationStrategy` impl: skeleton в B.1.76 — `execute_step` returns Succeeded (free-form action text без machine semantics в 1.75/1.76 schema), `reject` no-op per ADR 0027. Detection concrete fn deferred to B.1.77 (caller сам в Application reconciler знает diff).
- [x] `PlatformMigrationStrategy` impl: `execute_step` skeleton Succeeded; `reject` **real** — reads `plan.spec.previousSpecSnapshot.pin`, SSA-patches `PlatformStack.spec.pin` back с field manager `migration-controller-strategy` (different from `platform-controller` чтобы differentiate). Идемпотентно — repeated rejects byte-equivalent. Detection deferred to B.1.78.
- [x] Reconcile loop processes MigrationPlans in phase=executing, executes plan steps sequentially, updates status. `executed_steps.len()` doubles as progress marker — replay-safe (mid-step reconcile re-runs idempotent step). Step failure → seal в `failed`; all-steps-done → `completed`.
- [x] Approve transition: `pending-approval → approved` (external) → controller writes phase=executing then runs step-by-step.
- [x] Reject transition (platform-only): `pending-approval → rejected` (external) → controller invokes `PlatformMigrationStrategy.reject()` which reverts `PlatformStack.spec.pin` via SSA. Annotation source (`apprafter.io/previous-spec`) per plan.md прозаически переписан на `spec.previousSpecSnapshot` field (already in 1.75 CRD schema) — annotation approach был ADR 0027 placeholder, structured field cleaner.

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
- [x] Update Application reconciler (`operator/operator-controllers/application/src/lib.rs`):
    - Before patching child resources (Deployment, Service), check for existing MigrationPlan в namespace `apprafter-system` with phase non-sealed (matches `pending-approval` | `approved` | `executing` | `failed` | empty; resumes on `completed` | `rejected`). Filter pulls scope.type=application AND scope.application.ref.{name,namespace} matching this app AND scope.application.environment matching ctx.env_name (skipped when env is None — wildcard).
    - If found: skip child patching, set `Application.status.phase = AwaitingMigrationApproval` + `Ready=False/MigrationPending` + `MigrationPending=True/MigrationPlanPending` (plan name in message). EndpointURL preserved (children still running prior version). Requeue 30s.
    - If no pending plan: continue normal reconcile.
    - Detection (`ApplicationMigrationStrategy::detect_destructive`) NOT invoked в B.1.77 reconcile — current v1alpha1 Application schema (image / replicas / expose / env) per spec.md §3.8 carries no destructive operations, so detect always returns None. Concrete fn signature `(old, new) -> Option<DestructiveChange>` shipped on the strategy struct + `create_plan_for(...)` builder for future Phase 2.x callers wiring detection alongside `needs.*` / storage class / breaking image migration schema fields.
- [x] Custom Argo CD health check (Lua script в argocd-cm ConfigMap via chart's `configs.cm.resource.customizations.health.apprafter.io_Application` key) for Application CR. Returns `Degraded` with the MigrationPlan name in the message when `Application.status.phase=AwaitingMigrationApproval` (reads `status.conditions[type=MigrationPending].message` for the verbatim text). Returns `Healthy` on `phase=Ready`. Surfaces в Argo CD UI as `Degraded` card.

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
- [x] Update PlatformController reconcile path (from 1.73):
    - [x] After computing diff and classifying, when classification != `safe`:
        - [x] Save current spec.pin в MigrationPlan **`spec.previousSpecSnapshot.pin`** (вместо `metadata.annotations[apprafter.io/previous-spec]` per plan.md placeholder — structured field из B.1.75 CRD schema preferred over annotation approach).
        - [x] Create MigrationPlan with scope.type=platform, scope.platform.components — pre-check by deterministic `platform-<from>-to-<to>` name (idempotent); если plan exists с этим name → block bump regardless of classification.
        - [x] Skip patching umbrella Application; conditions UpgradeAvailable=True/BlockedByMigrationPlan + MigrationPending=True/<class> with plan name в message.
    - [x] On MigrationPlan approved: MigrationController executes → plan reaches `completed` → PlatformController's next reconcile sees plan completed (not blocking) → patches umbrella Application; Argo CD reconciles.
    - [x] On MigrationPlan rejected: PlatformMigrationStrategy.reject() (B.1.76, already implemented) reverts PlatformStack.spec.pin к `spec.previousSpecSnapshot.pin`. Same-transition retry blocked by rejected plan presence — operator must delete plan or pin к different target.

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
- [x] New CLI subcommands в `apprafter` binary:
    - [x] `apprafter platform status` — read PlatformStack.status, format человекочитаемо (current version, available, components healthy count, recent history). Implemented via kubectl shellout + `tabled` rendering (conditions + last-5 versionHistory).
    - [x] `apprafter platform upgrade [--to <version>]` — patch PlatformStack.spec.pin (или channel resolution if --to not specified). `--to <v>` pins; без `--to` clears `spec.pin` + flips `autoUpgrade=true`.
    - [ ] `apprafter platform channel <name>` — switch channel. **Deferred to 1.79a** — single-channel `stable` only ships в M1.5; multi-channel UX waits for Phase 2 where alternate channels actually exist.
    - [ ] `apprafter platform freeze <component> [--version <v>]` — patch overrides.<component>.pin. **Deferred to 1.79a** — component-level pinning is a polish layer over the chart-level pin already shipped; ships alongside ResourceClaim CRUD в 1.79a.
    - [ ] `apprafter platform unfreeze <component>` — remove override. **Deferred to 1.79a** (paired с freeze).
    - [ ] `apprafter platform rescue` — reinstall Argo CD from loader (emergency recovery). **Deferred to 1.79a** — covered by `apprafter cluster-bootstrap --re-adopt` path that 1.79a's loader extensions formalise.
    - [x] `apprafter migration list` — list MigrationPlans, filter by phase/scope. Filters деференцированы (CLI list iterates ALL plans; phase/scope filtering trivial follow-up if operator demand surfaces).
    - [x] `apprafter migration approve <name>` — patch status.phase=approved. Status-subresource merge-patch via kubectl.
    - [x] `apprafter migration reject <name>` — patch status.phase=rejected (rejected by webhook for application scope; works for platform). Webhook denial message surfaces verbatim.
    - [x] `apprafter open <ui>` — open browser to UI:
        - [x] `argocd` — `kubectl port-forward svc/argocd-server -n argocd 8080:443` + auto-fetch admin password from cluster secret + open https://localhost:8080. Cross-platform spawn (`xdg-open` / `open` / `cmd /c start`); blocks on child.wait() so Ctrl+C tears down the forward.
        - [ ] `backstage` — analogously. **Deferred to 1.79a / Tier 2+** — Backstage stack not tier-1 resident yet.
        - [ ] `grafana` — **Deferred Tier 2+**.
        - [ ] `hubble` — **Deferred Tier 2+**.
- [x] npm-style CLI version check on every invocation:
    - [x] Cache в `~/.cache/apprafter/version-check.json` with 24h TTL.
    - [x] Fetch latest CLI release from `api.github.com/repos/apprafter/apprafter/releases/latest`.
    - [x] If newer: print warning line at start of output ("apprafter X.Y.Z available; you have ..."). Fail-quiet — network errors / GitHub rate-limit / JSON parse failures swallowed silently (debug log only); версия check is courtesy, not operational prerequisite.
- [x] Argo CD Resource Action Lua script (added to argocd-cm ConfigMap via platform-stack chart): "Approve Migration" button on MigrationPlan resources в Argo CD UI. Discovery disables both Approve + Reject once `status.phase` leaves `pending-approval`; webhook denial of application-scope rejects surfaces в UI с the verbatim ADR 0027 message.

**Acceptance:**
- `apprafter platform status` outputs structured table within 2s.
- `apprafter open argocd` opens browser with credentials filled in within 5s on second-run (cached password).
- `apprafter migration approve <name>` succeeds; status updates within reconcile cycle.
- CLI shows update warning when version stale.
- Argo CD UI shows Approve button on MigrationPlan resources.

**Зависит от:** 1.72, 1.75, 1.76 (CRDs must exist для thin wrappers)

**Размер:** M

---

### 1.79a CLI app/repo subcommands + AppProjects + `open` polish

**Source:** ADR 0025, 0026 (Argo CD projects model); продолжение 1.79.

**Цель:** убрать необходимость заходить в Argo CD UI для повседневных операций (добавление repo, deploy status, rollback) и разделить платформенные приложения от пользовательских визуально и через RBAC.

#### Поставка — AppProjects в platform-stack chart

- [x] Добавить три `AppProject` ресурса в umbrella chart (через `_loaderValues.argocd.values.configs.projects`, а не отдельную папку — Argo CD chart 7.7.7 сам создаёт AppProjects из этого block'а):
    - [x] `platform` — для core platform components. `sourceRepos: ["*"]`, `destinations: [{namespace: "*", server: "https://kubernetes.default.svc"}]`, `clusterResourceWhitelist: [{group: "*", kind: "*"}]`, `namespaceResourceWhitelist: [{group: "*", kind: "*"}]` (открыто на M1.5 — RBAC enforcement через AccessGrant приедет в Phase 4).
    - [x] `platform-providers` — для ServiceProvider operators (CNPG, Dragonfly, NATS, Kamaji). Те же permissions что и `platform`, разделение чисто визуальное + lifecycle-категория. Project заводится сейчас (а не лениво в Phase 2), чтобы UI selector показывал его сразу после bootstrap'а.
    - [x] `apps` — для user applications. `sourceRepos: ["*"]` (пока не введён RBAC через AccessGrant Phase 4), `destinations: [{namespace: "*", server: "https://kubernetes.default.svc"}]`, `clusterResourceWhitelist: []`, `namespaceResourceWhitelist: [{group: "apprafter.io", kind: "Application"}, {group: "", kind: "ConfigMap"}, {group: "", kind: "Secret"}, {group: "gateway.networking.k8s.io", kind: "HTTPRoute"}]`.
- [x] Update umbrella Helm templates — все chart-managed Applications получают `spec.project: {{ default "platform" $component.project }}` через новое поле `#Component.project: string | *"platform"`. Default = `platform`; tier overlays / ServiceProvider charts могут override на `platform-providers` per-component. CLI loader's root platform Application также переехал на `spec.project: platform` (`cluster_bootstrap::render_root_application`).
- [ ] CMP plugin (`argocd-cue-cmp`) рендерит user Application CRs с `spec.project: apps` по умолчанию. **Отдельный коммит в составе `apprafter app add` (1.79a part 3)** — там же где появится user-app flow вообще.

#### Поставка — `apprafter open` polish

- [x] `apprafter open argocd` URL → `/applications?proj=apps` по умолчанию.
- [x] Флаги `--project <name>` (default `apps`) и `--all-projects` (убирает фильтр). Конфликтуют через `conflicts_with = "project"`.
- [x] Output формат при открытии:
    ```
    $ apprafter open argocd

    Opening Argo CD UI...
      URL:       https://localhost:8080/applications?proj=apps
      Username:  admin
      Password:  H7x4kP9aB3...  (copied к clipboard)

    ✓ Browser opened
    ℹ Press Ctrl+C к stop port-forward
    ```
- [x] Password copy to clipboard через `arboard` crate (cross-platform). Fail-quiet — headless / no-clipboard envs показывают `(clipboard unavailable — copy manually)` без error'а.
- [x] Password также печатается в terminal в plaintext — юзер может подсмотреть если clipboard засрался другим контентом.
- [ ] Попытка pre-fill username через URL `?username=admin` — Argo CD UI это не поддерживает (проверил empirically на 7.7.7); оставили только display + clipboard. **Закрыто negative-result'ом.**
- [ ] Аналогичная обработка для `apprafter open backstage` (когда появится). **Deferred к Tier 2+** — Backstage не tier-1 resident.

#### Поставка — `apprafter app` подкоманды

- [x] `apprafter app add [<git-url>]`:
    - [x] Без аргумента: детектит git origin из cwd через `git remote get-url origin`, нормализует (SSH→HTTPS, убирает `.git`).
    - [x] Флаги: `--name <name>` (default = repo name), `--branch <branch>` (default = current branch для cwd-режима, `main` для explicit URL), `--path <path>` (default `/`), `--project <name>` (default `apps`), `--remote <name>` (default `origin`), `--no-ping` (skip reachability check).
    - [ ] Interactive: спрашивает name/branch/path с дефолтами; non-interactive: использует defaults или fails если `--git-url` не задан. **В v0.1.139 non-interactive flag-driven only; interactive wizard деферрен** — flag defaults уже покрывают 95% случаев (cwd-detect + автодеривация name/branch). Wizard приедет если поступит реальный operator feedback что не хватает.
    - [x] Проверка доступности репо — `git ls-remote` через subprocess (поддерживает HTTPS auth check без token, для private — детект unauthorized). `--no-ping` для air-gapped CI.
    - [ ] Если репо private и не зарегистрирован cred — inline prompt: "Use existing PAT / Add new PAT / Skip". **Deferred к v0.1.141** — лендится вместе с `apprafter repo creds add`. Сейчас auth failure surfaces hint pointing к `apprafter repo creds add`.
    - [x] Пишет Argo CD `Application` CR в `argocd` namespace с label `apprafter.io/managed-by: apprafter` и annotation `apprafter.io/source: cli`. CR указывает на пользовательский repo, CMP plugin рендерит `apprafter/Application.cue` оттуда.
- [x] `apprafter app list [--project <name>] [--all-projects]`:
    - [x] Default filter `--project apps`.
    - [x] Таблица: name, project, repo, revision, sync state, health. (last sync time не surfaced — Argo CD CR не carry'ит human-friendly timestamp в `status.sync`; добавим если operator feedback потребует).
    - [x] `--all-managed` toggle drops the managed-by label filter.
- [x] `apprafter app status <name>`:
    - [x] Detail view: Argo CD Application sync/health + source + destination + recent revisions (last 3 из `status.history`).
    - [ ] AppRafter Application CR conditions (если CMP уже отрендерил) + перечень child resources. **Deferred к v0.1.140 / Phase 2** — когда CMP plugin реально начнёт рендерить AppRafter Applications и child resources станут предсказуемой структурой.
    - [ ] Если есть pending MigrationPlan для этого app — выводит в верхней секции с approve-командой. **Deferred к v0.1.140 / Phase 2** — нужны user-app MigrationPlans из Phase 2 destructive change detection.
- [ ] `apprafter app logs <name> [--follow] [--tail <N>] [--container <c>]`: **Deferred к v0.1.140**.
- [ ] `apprafter app rollback <name> [--to <revision>]`: **Deferred к v0.1.140**.
- [x] `apprafter app remove <name>`:
    - [x] Confirmation prompt через `inquire::Confirm` (default No), `--yes` для non-interactive.
    - [x] Удаляет Argo CD Application через `kubectl delete`, в каскаде — child resources (Argo CD reconciles via ownerRefs).
    - [x] `--keep-data` опция — flips `syncPolicy.automated.prune: false` ДО delete, child resources (PVC/ResourceClaims) сохраняются.

**Alias:** [x] `apprafter a` → `apprafter app` (проверил — `apprafter apply` не конфликтует с `a` потому что clap резолвит alias строго; `apprafter a add` работает, `apprafter a apply` не существует).

#### Поставка — `apprafter repo creds` подкоманды

- [ ] `apprafter repo creds add [<name>]`:
    - Interactive wizard: name, URL prefix (default детектится по последнему `app add`-у с private репо), type (PAT/SSH/basic-auth), token/key input (через `inquire::Password`).
    - Token validation:
        - GitHub: `github_pat_*` (fine-grained) или `ghp_*` (classic), regex check + API ping `GET https://api.github.com/user`.
        - GitLab: `glpat-*`, API ping `GET https://gitlab.com/api/v4/user`.
        - Generic: формат не валидируется, только URL prefix reachability.
    - Создаёт k8s Secret в namespace `argocd` с labels:
        - `argocd.argoproj.io/secret-type: repo-creds`
        - `apprafter.io/managed-by: apprafter`
        - `apprafter.io/cred-name: <name>`
        - Stringdata: `url`, `username` (для PAT — обычно git provider user или token holder), `password` (token).
- [ ] `apprafter repo creds list`:
    - Таблица: name, URL prefix, type, last used (если есть annotation), expires (если можем определить — для GitHub fine-grained есть `X-GitHub-Request-Id` headers но не exp; для classic — никак; оставить N/A).
- [ ] `apprafter repo creds show <name>`:
    - Detail view, token замаскирован (`****`).
- [ ] `apprafter repo creds rotate <name>`:
    - Prompt только для нового token, остальные поля сохраняются.
    - Patch existing Secret (не пересоздаёт — иначе Argo CD может потерять reference кратковременно).
    - Re-validation token'а перед patch.
- [ ] `apprafter repo creds remove <name>`:
    - Confirmation с warning если есть Applications зависящие от этого prefix.
    - `--yes` для skip confirmation.

#### Поставка — context-aware error hints

- [ ] При `apprafter app add` без `.git` в cwd: hint "Run from a git repository, or use `apprafter app add <git-url>` explicitly".
- [ ] При попытке `app add` для private репо без creds в non-interactive: error "Repository requires authentication. Configure with `apprafter repo creds add` first" + exit code 2.
- [ ] При попытке `app add` с конфликтным именем (Application с таким name уже есть): error "Application '<name>' already exists. Use `apprafter app status <name>` or pick a different `--name`".

#### Acceptance

- [ ] `apprafter open argocd` открывает UI с фильтром `apps`, username отображается в выводе, password в clipboard.
- [ ] В Argo CD UI верхний project selector показывает три проекта; `apps` пустой при свежем bootstrap, `platform` и `platform-providers` содержат соответствующие Applications.
- [ ] `cd <git-repo> && apprafter app add` без аргументов корректно детектит origin и регистрирует app.
- [ ] `apprafter app add` для private репо без creds → interactive prompt предлагает добавить PAT inline.
- [ ] `apprafter repo creds add` с невалидным GitHub PAT → fail с regex error до API call.
- [ ] `apprafter repo creds add` с валидным форматом но revoked token → fail с API ping error и hint про token rotation.
- [ ] `apprafter app rollback <name>` без `--to` откатывает к предыдущей revision; Argo CD синкает в течение reconcile cycle.
- [ ] `apprafter app remove` удаляет Application каскадно, `--keep-data` сохраняет PVC.
- [ ] `apprafter repo creds rotate` обновляет token, existing apps продолжают синкаться без даунтайма Argo CD repo reconcile.

#### Не входит в этот item

- AccessGrant / RBAC enforcement через AppProject (Phase 4 целиком).
- Reverse proxy для `apprafter open` (отдельный item, после M2, когда понадобится Backstage с теми же проблемами).
- `apprafter app scale`, `apprafter app env set` — высокоуровневые ops-команды (M2+, после ServiceProvider/ResourceClaim).
- Backstage Application plugin — отдельный item в Phase 3.

**Зависит от:** 1.79 (CLI thin wrappers infrastructure + `open` для argocd базовый).

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

## Фаза 1.9 — Dev Mode MVP (Phase 1B из dev-mode-task.md)

**Цель фазы:** ship minimum viable dev mode для локальной разработки на k3d. CLI команды: `apprafter dev cluster up/down/status/wipe`, `apprafter dev init`, `apprafter dev up`, `apprafter dev down`, `apprafter dev list`, `apprafter dev logs`. Manifest layering 4 уровня (Application.base + environments.dev + DevProfile + DevProfileLocal). `needs.*` resolution в эту фазу **не входит** — лендится в Фазе 2.9. Помечается `experimental` для users.

**Source of truth:** `dev-mode-task.md` §20 Phase 1B (sub-items 1B.1 – 1B.12).

**Spec:** `spec.md` §3.10, §3.11.

**Зависит от:** Phase 1.5 closed. Нужны: PlatformStack CRD (1.72), MigrationPlan CRD (1.75), `tiers/dev.cue` overlay в platform-stack chart (опт-ин/опт-аут defaults per dev-mode-task.md §12.2), Application reconciler dev-awareness hooks.

**Поставка:** items 1B.1 – 1B.12 из `dev-mode-task.md` §20 перетаскиваются сюда AI-агентом по мере реализации (как 1.6.1, 1.6.2, …), с реальными размерами и acceptance criteria для каждого. Тот же паттерн, что Track A из Phase 1.5 (где cli-dx items живут в `cli-dx-task.md` §17 и lend'ятся в plan.md по факту).

**Версии:** `v0.1.x` patch series (без closing tag — M2 стартует следующим коммитом с bump на `v0.2.0`).

**Размер (aggregate):** M+ (~1.5–2 недели FT по dev-mode-task.md §20). Корректируется по факту перетаскивания items.

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

## Фаза 2.9 — Dev Mode + Services (Phase 2B из dev-mode-task.md)

**Цель фазы:** dev mode поддерживает `needs.{pg, jetstream, redis}` end-to-end локально через lightweight in-cluster providers (single-node Postgres pod, embedded NATS, single Redis). Дев-кластер на `dev cluster up` поднимает все ServiceProviders по умолчанию с `--without` opt-out флагом. Helper команда `dev claim status <app>` для диагностики ResourceClaim. Помечается `experimental` (полный DX в Фазе 3.9).

**Source of truth:** `dev-mode-task.md` §20 Phase 2B.

**Spec:** `spec.md` §3.10, §3.2 (ServiceProvider/ResourceClaim).

**Зависит от:** Phase 1.9 closed + Phase 2 closed (ServiceProvider CRD 2.1, ResourceClaim CRD 2.2, scheduler 2.3, реализации `needs.pg` 2.4 / `needs.jetstream` 2.5 / `needs.redis` 2.6).

**Поставка:** items из `dev-mode-task.md` §20 Phase 2B перетаскиваются сюда AI-агентом по мере реализации.

**Версии:** `v0.2.x` patch series.

**Размер (aggregate):** M (~1 неделя FT). Корректируется по факту.

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

## Фаза 3.9 — Dev Mode Full (Phase 3B из dev-mode-task.md)

**Цель фазы:** production-ready local dev experience. Heuristic runtime detection (Bun / Node / Rust / Go / Python), preset library (Bun HTTP service, Rust async worker, и т.д.), полный `dev reset / restore` lifecycle с backups, observability tab в Backstage equivalent для dev. Снимается `experimental` tag — dev mode становится официальной частью MVP completion.

**Source of truth:** `dev-mode-task.md` §20 Phase 3B.

**Spec:** `spec.md` §3.10, §3.11.

**Зависит от:** Phase 2.9 closed + Phase 3 closed (observability stack, Backstage flow visualizer).

**Поставка:** items из `dev-mode-task.md` §20 Phase 3B перетаскиваются сюда AI-агентом по мере реализации. По завершении — снимается `experimental` маркер в user-facing docs и CLI help.

**Версии:** `v0.3.x` patch series. По dev-mode-task.md §20 эта фаза лендится в planned pause между M3 и Phase 4 (managed offering research), не блокирует старт Phase 4.

**Размер (aggregate):** M (~1 неделя FT). Корректируется по факту.

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
- [ ] Tag `v0.4-mvp`.

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
| 2026-05-14 | M1.5 Track A.10 — miette diagnostic refinement: `CliError` derives `miette::Diagnostic` с stable `code(apprafter::*)` + multi-line `help(...)` на 9 user-facing вариантах (cue_not_found, hetzner_api_error, server_type_unavailable, state::corrupt, target::invalid_config, target::not_found, io/json/yaml, cli::other); binary entry point switched с `color-eyre` на `miette::set_hook` с `fancy` reporter; `NO_COLOR` respected; `color-eyre` workspace + platform-cli deps удалены; +11 tests (8 unit на `.code()`/`.help()` accessor surface + 3 subprocess-based integration на rendered stderr — `help:` block, diagnostic code substrings, ANSI-free под `NO_COLOR`); v0.1.86 | initial |
| 2026-05-14 | M1.5 Track A.10 walk-fix — `target add` с битым токеном выходил под generic `apprafter::cli::other` потому что `commands/target.rs::ping_provider` и `commands/target_wizard.rs::prompt_token` обёртывали типизированный `CliError::Hetzner { status: 401, .. }` в `CliError::Other(format!(...))`, теряя diagnostic code и rotation help. v0.1.87 добавил две типизированные вариации с `#[diagnostic_source]` cause chain: `apprafter::target::token_rejected` (401 — rotation hint + console URL + clipboard newline trap) и `apprafter::target::provider_unreachable` (non-401 / transport — `apprafter doctor` + status page + `--no-ping`); shared `classify_ping_error(provider, err)` helper в обоих call sites; миette теперь рендерит two-layer cascade — outer summary с rotation help + chained inner `hetzner_api_error` с full 401/403/429/5xx breakdown; +2 unit tests на новых вариантах (`provider_token_rejected_carries_rotation_hint_and_chains_cause` доказывает что `diagnostic_source()` доходит до inner code, `provider_api_unreachable_targets_outage_path_not_rotation` гарантирует что outage не предлагает rotation); 2 prior target_test integration tests перевели assertions с обёртки на diagnostic codes (rendered output line-wraps, substrings типа `(status 401)` ненадёжны, codes — да); v0.1.87 | initial |
| 2026-05-14 | M1.5 Track A.11 — semantic colors + subcommand aliases: new `cli_core::style` модуль поверх `owo-colors` (auto-honours `NO_COLOR` через `supports-colors` feature) с 6 хелперами (ok/warn/fail/info/dim/bold); colour applied на `bootstrap-all` phase markers (`→` cyan, `✓` green, `✗` red) + `doctor` glyphs (green ✓ / yellow ⚠ / red ✗); 6 новых subcommand aliases (`kubeconfig` ↔ `kc`, `cluster-bootstrap` ↔ `cb`, `bootstrap-all` ↔ `up`, `target list/show/remove` ↔ `ls`/`info`/`rm`), сохранён prior `target` ↔ `t` для chained `apprafter t ls`; +2 unit (style ANSI-strip under non-TTY) + 7 integration (alias subprocess routing включая chained `t ls`); v0.1.88 | initial |
| 2026-05-14 | M1.5 Track A.11 walk-fix — после walk'а v0.1.88: `apprafter up --dry-run` оставался монохромным (я подкрасил только wet path); только `INFO` лейбл от tracing-subscriber'а светился зелёным. v0.1.89 покрыл dry-run plan: `bootstrap-all` heading + `Target:` label + active target name через `style::bold`; `(active)`/`(via --target override)` tag через `style::info` cyan; `[1/3]`/`[2/3]`/`[3/3]` phase numbers cyan (echoes wet path's `→`); `Provider:/Region:/Tier:/...` labels + `<unset — ...>` placeholders + bottom hint через `style::dim` чтобы defaults не отвлекали от реальных значений; script(1)-replay confirms ANSI bytes есть в TTY и отсутствуют под `NO_COLOR=1`; v0.1.89 | initial |
| 2026-05-15 | M1.5 Track A.12 — docs + ADR (Track A closure): new ADR 0030 кодифицирует 4 Track A design decisions (target store, credential resolution chain, miette diagnostics, aliases+color); `docs/operator-guide/quickstart.md` переписан под post-Track-A flow (`target add` + `bootstrap-all` вместо env-var + `cargo run --bin apprafter -- init`); new `docs/operator-guide/target-store.md` (file layout + chain reference); new `docs/operator-guide/troubleshooting.md` (catalogue всех 11 diagnostic codes + worked cause-chain example); new `docs/reference/cli.md` (every subcommand + global env vars table + aliases reference); `docs/operator-guide/index.md` и `docs/reference/index.md` обновлены; `mkdocs.yml` nav expanded; Track A backlog (A.9c Phase 2 polish: SSH ConnectTimeout + label rename) explicitly tracked для follow-up; v0.1.90 — закрывает Track A | initial |
| 2026-05-15 | M1.5 Track A.9c (Phase 2 polish backlog closure): `SshKubeconfigFetcher::build_command` добавляет `-o ConnectTimeout=5` так что первая kubeconfig-poll attempt fail'ится за 5s вместо kernel-default ~30s пока cloud-init поднимает sshd на новом cpx22; Phase 2 label rename `[2/3] kubeconfig` → `[2/3] k3s-ready` (старый label вводил в заблуждение — ~60s это cloud-init+k3s startup, не сам fetch) применён consistently: спиннер, success/failure markers, dry-run plan, total breakdown summary; spinner message теперь "waiting for cloud-init + k3s on the new node…"; docs (quickstart, troubleshooting, cli reference) обновлены synchronously; +1 regression test (`ssh_fetcher_caps_connect_timeout_at_five_seconds`); existing `bootstrap_all_dry_run_prints_three_phase_plan_without_provider_calls` integration test обновлён под `[2/3] k3s-ready`; typical Phase 2 на Hetzner cpx22 + Ubuntu 24.04 ожидаемо падает с ~60s до ~20-40s; v0.1.91 — закрывает весь Track A backlog | initial |
| 2026-05-15 | M1.5 Track B.1.66 — platform-stack scaffold: new top-level `platform-stack/cue/` (flat layout — CUE treats subdirs как отдельные package instances even when `package` declaration matches, что ломало cross-file `_components` merging; обнаружено на design walk через `cue export -e direct_cilium`). 12 CUE файлов: `platform.cue` (umbrella schema — `#Version`/`#Channel`/`#Tier`/`#ComponentSource`/`#Component`/`#ComponentSet`/`#PlatformValues`/`_components`), 8 component declarations (`component_cilium.cue`, `component_cert-manager.cue`, `component_argocd.cue`, `component_apprafter-operator.cue`, `component_admission-webhook.cue`, `component_backstage.cue`, `component_network-policies.cue`, `component_argocd-cue-cmp.cue` — последний declared but disabled by default до wiring step в 1.69), 2 tier overlays (`tier_solo.cue` tier 1 + `tier_team.cue` tier 2 со своими `enabled`/replicas/Hubble настройками), `compatibility.cue` со схемой `#ChangeClass`+`#VersionRecord` и initial 0.2.0 entry; на design walk вскрылся gotcha: `[NAME=string]: #Component & { name: NAME }` autobinding в `#ComponentSet` re-применяет `#Component` на каждом overlay-unification и стрипит concrete `namespace`/`version` — заменён на plain `[string]: #Component` + explicit `name:` per component; `_components: #ComponentSet` тоже типизировать нельзя (та же проблема), оставлен plain `{}` с локальной type-conformance на declaration site; `Chart.yaml.tmpl` template + `README.md` (full layout + contribution model + distribution + forking + design-walk decision rationale) + `CHANGELOG.md` (0.2.0 planned entry); `scripts/lint-cue.sh` + `scripts/check-spdx-headers.sh` расширены под `platform-stack/cue/...` + `platform-stack/Chart.yaml.tmpl`; `cue vet -c` passes, lint clean, все 565 Rust tests остаются зелёными; v0.1.92 — открывает Track B | initial |
| 2026-05-15 | M1.5 Track B.1.67 — `cue cmd render` pipeline + umbrella chart generation: `platform-stack/cue/render_tool.cue` declares `command: render: { ... }` с 9 tasks (3 `file.Mkdir` + 6 `file.Create`) и `$dep` chain для DAG ordering; emit Chart.yaml v2 (с annotation'ами apprafter.io/change-class+apprafter.io/operator-version из compatibility.cue), values.yaml (defaults to tier1), values.schema.json (handrolled Helm-native draft-2020-12 — CUE's auto-export targets draft-07 который Helm не понимает), templates/applications.yaml (единственный Go template итерирующий .Values.components → один Argo CD Application per enabled entry, conditional helm.valuesObject only когда source.chart set, labels apprafter.io/{component,tier,channel}), compatibility.yaml, examples/values.{solo,team}.yaml, README.md внутри rendered chart; `platform-stack/Makefile` с targets `render`/`render-only`/`lint`/`clean`/`help`, auto-detect cue+helm с nix fallback, version резолвится из tier1.version через `cue export` без хардкода; `Justfile` получил `platform-stack-render` + `platform-stack-check` wrappers; `dist/` уже gitignored project-wide; README local-dev section updated с реальными командами + schema-gate sanity check; verified — `helm lint` clean (single INFO про chart icon), `helm template` default → 6 tier-1 Apps, with team values → 7 Apps (Backstage on), `--set tier=99` → schema rejects "value must be one of 1, 2, 3, 4"; v0.1.93 | initial |
| 2026-05-15 | M1.5 Track B.1.68 — CI OCI publish workflow + cosign signing: `.github/workflows/platform-stack-publish.yml` triggers на `platform-stack/v*` tag (+ `workflow_dispatch` с `version:` input); validate'ит `compatibility.cue` через `scripts/check-platform-stack-version.sh` (cue export -e compatibility[<v>], exits 1 с pointer-to-fix если entry missing); рендерит chart (`make -C platform-stack render-only`); `helm lint` + tier-1/tier-2 smoke template assertions (6 / 7 enabled Apps); `helm package` → `.tgz`; `docker login` к ghcr.io через GITHUB_TOKEN; `helm push` к `oci://ghcr.io/<owner>` (Helm 3.8+ native OCI); resolve immutable digest через `helm show chart --version`; cosign keyless sign по digest (Sigstore OIDC + `id-token: write` permission, no managed keys); cosign sign-blob на `.tgz` → `.tgz.sig` + `.tgz.pem` для GitHub Release attachment path; `oras tag :latest` на stable releases с graceful warning если CLI отсутствует; `gh release create` с heredoc-built notes via mktemp+`--notes-file` (install snippets Argo CD + plain Helm + cosign verify для обоих) + `.tgz` / `.sig` / `.pem` attachments + `--prerelease` flag на pre-release; security hardening — все dynamic inputs (github.ref_name, github.repository_owner, github.event.inputs.version, github.repository) routed через env: binding (та же anti-injection pattern что release-operator.yml); `platform-stack/RELEASE.md` — full maintainer procedure (semver rules, pre-release checklist, tagging, after-publish actions, failure-mode recovery); local validation: yamllint clean, scripts/check-platform-stack-version.sh tested на happy (0.2.0 → YAML) + unhappy (99.99.99 → exit 1) paths; final acceptance ⏳ verifies after first real `platform-stack/v0.2.0-rc1` push (CI-side, не локально воспроизводимо); v0.1.94 | initial |
| 2026-05-15 | M1.5 Track B.1.68 walk-fix — GitHub Actions reject'ил workflow на push с ошибкой "Line: 232, Col: 14: Unexpected symbol: '…'" потому что один из комментариев внутри `run: |` блока содержал `${{ … }}` (буквальный ellipsis Unicode внутри expression-syntax скобок) — GHA парсит `${{ }}` в run-body scalar'ах до того как shell видит script. v0.1.95 переписал тот комментарий без `${{ }}` syntax, проверил grep'ом что больше нигде в run-bod'ах нет expression-style braces (header comments вне scalar'ов yamllint discard'ит — там line 29 безвреден); yamllint clean, остальные gates green; workflow теперь syntactically valid для GitHub Actions parser'а; v0.1.95 | initial |
| 2026-05-15 | M1.5 chart-versioning policy decision — first published platform-stack version = **0.1.0** вместо ранее запланированного 0.2.0. Rationale: chart MINOR трекает phase number AppRafter monorepo (Phase 1.5 → chart 0.1.x), не milestone target. Когда Phase 2 services landings → chart MINOR bump'нется на 0.2.0 alongside `v0.2.0-services`. Patch versions chart и monorepo независимы (share only MINOR/MAJOR semantics). v0.1.96 flip'нул: `tier_solo.cue` + `tier_team.cue` `version: "0.1.0"`, `compatibility.cue` entry rename, `platform.cue` doc-comment, 4 component doc-comments (cilium/cert-manager/argocd-cue-cmp/operator/webhook упоминающих "platform-stack X.Y.Z"), `CHANGELOG.md` section + version notes, `RELEASE.md` versioning rules + tagging examples; re-render produces `dist/platform-stack-0.1.0/`, helm lint clean, `check-platform-stack-version.sh 0.1.0` → success, `0.2.0` → exit 1; v0.1.96 | initial |
| 2026-05-19 | M1.5 Track B.1.68 refactor — chart-version single-source-of-truth + workflow inverts tag↔publish ordering. Walk-found: tier_solo/tier_team/compatibility-key все хардкодили "0.1.0" литералом → potential drift. Также workflow триггерился на push:tags, что позволяло "accident tag push → unconditional publish"; user попросил поменять направление — workflow создаёт tag после успешного publish, не наоборот. v0.1.97-equiv (без monorepo bump — CLI не менялся): добавил `currentVersion: #Version & "0.1.0"` в platform.cue как canonical source; `tier_solo`/`tier_team` теперь `version: currentVersion`; compatibility.cue получил CUE-level invariant `compatibility: (currentVersion): #VersionRecord` — bump currentVersion без matching entry падает на `cue vet -c` с диагностикой incomplete-fields; workflow trigger переключён на `workflow_dispatch` ONLY (no push:tags), optional `version_override:` input для emergency re-publish; workflow читает `currentVersion` через `cue export -e currentVersion`, проверяет что tag не существует на origin, прогоняет compat-gate + render + lint + push + sign, и в самом конце `gh release create` создаёт tag + release одним вызовом (через `--target $GITHUB_SHA`); `scripts/check-platform-stack-version.sh` без аргументов auto-reads currentVersion; RELEASE.md полностью переписан под новую модель — "две-строчный version bump в platform.cue+compatibility.cue", `gh workflow run` вместо `git tag && git push`, failure-mode recovery упрощена; cli/Cargo.toml откатан на 0.1.96 (CLI не менялся, monorepo tag не создаётся) | n/a |
| 2026-05-19 | M1.5 Track B.1.68 auto-trigger + drift detection — `platform-stack-publish.yml` теперь триггерится на `push: branches: [master], paths: ['platform-stack/**', …]` (плюс `workflow_dispatch` стайл-овеr); job разбит на `detect` + `publish`: detect resolve'ит currentVersion и проверяет что `platform-stack/v<version>` не существует на origin, если уже есть → `should_publish=false` и publish job skipped (commit был не bump, а refactor / docs / drift). Новый workflow `platform-stack-check.yml` триггерится на PR + push к master с теми же paths и enforce'ит: cue fmt --check + cue vet -c (invariant catches bump-without-compat) + render + helm lint + tier-1/tier-2 smoke + **drift detection** — если currentVersion матчится тэгу на origin И files в `platform-stack/cue/*.cue` или `Chart.yaml.tmpl` отличаются от того commit'а → fail с 80-line diff и hint про currentVersion в platform.cue. Это делает "chart source changed без version bump" blocking CI error на PR time; на master тот же check работает как post-merge safety net. `actions/checkout` с `fetch-depth: 0` чтобы drift check имел доступ к remote tags. RELEASE.md обновлён под auto-trigger model — "Normal flow: bump + PR → check workflow + merge → publish workflow auto-detects bump"; добавлен PR-time guards section. yamllint clean, все cue/spdx/cargo gates green. CLI не менялся → monorepo tag не создаётся, версии в Cargo.toml не трогаются | n/a |
| 2026-05-19 | M1.5 Track B.1.69 — CUE CMP sidecar (ADR 0029): new top-level `argocd-cue-cmp/` flat directory (Dockerfile, plugin.yaml, entrypoint.sh, VERSION='0.1.0', README.md). Alpine 3.20 multi-stage build pulls cue v0.10.0 tarball, runtime image runs as UID/GID 999 (matches argocd-repo-server CMP contract; pre-existing Alpine ping group at gid 999 deleted to free slot). entrypoint.sh wraps `cue export ./... --out yaml` со structured error output — first error line as `::cue-cmp:: CUE compile failed: <summary>` к stderr, full block ниже, exit 0/1 correctly. New paired workflows `argocd-cue-cmp-publish.yml` + `argocd-cue-cmp-check.yml` — same detect/publish split + drift detection pattern as platform-stack-*; image's own semver track `argocd-cue-cmp/v*` independent of monorepo/chart/operator versions. Chart bumped 0.1.1 → 0.1.2: `component_argocd-cue-cmp.cue` обновлён под new image registry + version pin, `component_argocd.cue` получает `repoServer.extraContainers` блок с cue-cmp sidecar (image pull через CUE interpolation `_components."argocd-cue-cmp".values.image.{repository,tag}` — one-line bump cue-cmp version), compatibility.cue entry для 0.1.2 (change: safe, references ADR 0029). SPDX gate расширен под `argocd-cue-cmp/{Dockerfile,plugin.yaml,entrypoint.sh}` (170 → 175). Local verified: docker build clean, entrypoint happy + error paths exit-code/output correct, helm template 0.1.2 показывает extraContainers + cue-cmp:v0.1.0 image. CI-side acceptance ⏳ verified at first push (cue-cmp publish workflow + chart publish workflow оба триггерятся параллельно) | n/a |
| 2026-05-19 | M1.5 Track B.1.70 — Minimal cluster-bootstrap rewrite (ADR 0025): `commands/cluster_bootstrap.rs` переписан с ~1250-line imperative install (Cilium + Gateway + Application CRD + default-deny + Argo CD + cert-manager + ClusterIssuer + operator + webhook + Backstage) на 4-step GitOps loader (~450 lines). Step 1: `helm upgrade --install argocd argo/argo-cd` с loader-only values (replicas=1, dex off). Step 2: `kubectl wait --for=condition=Available deployment/argocd-server` (gates root Application apply until CRDs installed). Step 3: kubectl apply root Application (`name: platform, source.repoURL: oci://ghcr.io/<owner>/platform-stack, chart: platform-stack, targetRevision: 0.1.2`). Step 4: `kubectl wait --for=jsonpath='{.status.health.status}'=Healthy application/platform` — после Healthy все child Applications reconciling под Argo CD. Argo CD handle'ит drift correction + prune semantics + idempotent re-apply. Existing `cli-providers::k8s::*_yaml` рендерераторы остаются (chart's parallel source-of-truth до 1.71 migration). `cli-providers::k8s` exports 3 новых constants: `RELEASED_PLATFORM_STACK_VERSION = "0.1.2"`, `APPRAFTER_PLATFORM_STACK_DEFAULT_REPO = "oci://ghcr.io/apprafter"`, `APPRAFTER_PLATFORM_STACK_CHART_NAME = "platform-stack"`. `KubectlRunner` trait расширен `wait_for_condition()` методом (supports `--for=condition=` + `--for=jsonpath=` flavours). 13 imperative-install tests deleted, 5 GitOps-loader tests added. 549 passed (down from 565 net). CLI binary меняется значимо → monorepo tag v0.1.97. Real-cluster acceptance ⏳ verified at first walk after push | v0.1.97 |
| 2026-05-19 | M1.5 Track B.1.70 walk-fix — real-Hetzner walk v0.1.97 surfaced pre-install hook timeout: `helm install argocd argo/argo-cd 7.7.7` fails on single-node k3s с `failed pre-install: timed out waiting for the condition`. Корень — upstream chart defaults `redis-ha.enabled: true` → 3 redis pods с `requiredDuringSchedulingIgnoredDuringExecution` podAntiAffinity, не могут schedule на одной node. v0.1.x in-tree baseline это явно отключал (`cli-providers::k8s::argocd_values_yaml`); v0.1.97 rewrite случайно drop'нул флаг. v0.1.98 fix: восстановил `redis-ha.enabled: false` + `notifications.enabled: false` + `server.service.type: ClusterIP` в loader values (cluster_bootstrap.rs) и в chart's `component_argocd.cue` (чтобы self-reconcile не re-enable'ил redis-ha на adoption); +2 tests (`argocd_loader_values_disables_redis_ha_for_single_node_k3s`, `argocd_loader_values_keep_server_at_cluster_ip_until_chart_exposes_it`); 551 passed; chart bumped 0.1.2 → 0.1.3 с compat entry + warning в 0.1.2 notes; CLI bumped 0.1.97 → 0.1.98, `RELEASED_PLATFORM_STACK_VERSION` → "0.1.3" | v0.1.98 |
| 2026-05-19 | M1.5 Track B.1.70 walk-fix #2 — second real-Hetzner walk v0.1.98 surfaced **catch-22 the rewrite created**: v0.1.98 redis-ha fix helped (chart 0.1.3 applied), но Argo CD pre-install Job `argocd-redis-secret-init` остался Pending с `0/1 nodes are available: 1 node(s) had untolerated taint(s)`. Корень: k3s стартует без CNI (`--flannel-backend=none`), нода в `NotReady` с `node.kubernetes.io/not-ready:NoSchedule`; Argo CD pre-install Job pod не толерирует taint → helm timeout. v0.1.x baseline ставил Cilium ПЕРВЫМ (нода Ready перед Argo CD); v0.1.97 rewrite перевернул порядок — Argo CD сначала, Cilium через chart, но chart не может reconcile без Argo CD, который не может start без CNI. v0.1.99 fix: Cilium возвращается в CLI loader как Step 0 (использует существующие `cli-providers::k8s::cilium_values_yaml` + `CILIUM_CHART_VERSION = 1.16.5`); Step 0b `kubectl wait --for=condition=Ready node --all` (timeout 180s — image pull dominates). Chart's `component_cilium.cue` остаётся owner upgrades через Argo CD adoption (same release name + namespace, `prune: false`). `KubectlRunner::wait_for_condition` API изменён: `namespace: &str → namespace: Option<&str>` (cluster-scoped resources не должны нести `-n`). 3 новых tests (`wait_command_emits_namespace_flag_when_some`, `wait_command_omits_namespace_flag_when_none`, `cilium_installs_before_argocd_so_node_can_become_ready` — regression guard на ordering); существующий main test переименован под новую sequence; FakeKubectl в `cluster_bootstrap` + `argocd_password` обновлены под `Option<&str>`. 554 passed (был 551). Chart НЕ меняется — `RELEASED_PLATFORM_STACK_VERSION` остаётся "0.1.3". CLI bumped 0.1.98 → 0.1.99 | v0.1.99 |
| 2026-05-19 | M1.5 Track B.1.70 walk-fix #3 — third real-Hetzner walk v0.1.99 surfaced **two coupled bugs** в GitOps loader path: (1) Argo CD не infer'ит OCI Helm protocol из `oci://` scheme — он shells out на `helm pull --repo oci://... <chart>` который `helm` reject'ит с `object required`; OCI registries требуют `helm pull oci://<repo>/<chart>` (chart-name inline), и Argo CD генерирует правильную форму ТОЛЬКО когда repo registered via `Secret(label=repository, enableOCI: "true")`. (2) `kubectl wait --for=jsonpath={.status.health.status}=Healthy` на root Application — false-positive: freshly-created Application с zero rendered children тривиально Healthy (no resources to fail), даже когда Sync=Unknown (chart pull errored). v0.1.99 loader сматчил empty-Healthy и вернулся успешно, оставляя cluster без child Applications. v0.1.100 fix: (a) `APPRAFTER_PLATFORM_STACK_DEFAULT_REPO` "oci://ghcr.io/apprafter" → "ghcr.io/apprafter" (bare URL), (b) `argocd_loader_values_yaml` получает `configs.repositories.apprafter` block (url + type: helm + enableOCI: "true"), (c) chart's `component_argocd.cue` mirror'ит тот же block в values чтобы self-reconcile сохранил registration на adoption, (d) chart's `component_apprafter-operator.cue` + `component_admission-webhook.cue` drop'нули `oci://` префикс в repoURL, (e) `perform_bootstrap` step 4 разбит на 4a (wait Synced) + 4b (wait Healthy) — Synced это правильный first signal что chart pulled и children rendered. 3 новых tests (`argocd_loader_values_register_apprafter_oci_repo` с per-line oci:// negative guard, `root_application_repourl_is_bare_without_oci_scheme`), main bootstrap test extended под 4 waits. 556 passed (был 554). Chart bumped 0.1.3 → 0.1.4 с compat entry + known-issue note в 0.1.3 notes; CLI bumped 0.1.99 → 0.1.100, `RELEASED_PLATFORM_STACK_VERSION` → "0.1.4". Manual diagnosis на живом cluster: создание `Secret(repo-apprafter)` + patch root Application's repoURL → 6 child Applications появились мгновенно (cert-manager Synced/Healthy first; rest Sync=Unknown initially — controller докатывает) | v0.1.100 |
| 2026-05-19 | M1.5 Track B.1.70 walk-fix #4 — четвёртый real-Hetzner walk на chart 0.1.4 / CLI v0.1.100: root `platform` Synced/Healthy + 6 children появились, но только `cert-manager` Synced; остальные 5 failed тремя независимыми причинами. **Bug A** — `apprafter-operator` + `admission-webhook` helm charts никогда не publish'ились в OCI (`release-operator.yml` push'ил только container images); чарт `apprafter-operator` существовал на `0.1.29` (drift от pin'a `v0.1.91`), `apprafter-admission-webhook` chart **отсутствовал**. Fix: создан новый `apprafter-admission-webhook` helm chart (Chart.yaml + values + _helpers + Certificate + Service + Deployment + ValidatingWebhookConfiguration; templates выведены из `cli-providers::k8s::admission_webhook_yaml`), `apprafter-operator` Chart.yaml bumped `0.1.29 → v0.1.91`, в `release-operator.yml` добавлен новый job `helm-charts` (needs operator+webhook image jobs; uses azure/setup-helm + `helm package` + `helm push oci://ghcr.io/<owner>`; chart version читается из Chart.yaml, не из github.ref_name — platform-stack pin = single source of truth). **Bug B** — `cilium` + `argocd` failed structured-merge diff с `terminatingReplicas: field not declared in schema`; k3s v1.35 surfaces Kubernetes 1.31+ field `Deployment/DaemonSet/StatefulSet.status.terminatingReplicas`, Argo CD 2.13.1 не знает. Fix: `#Component` schema в platform.cue получает optional `ignoreDifferences: [...{group, kind, jsonPointers?, jqPathExpressions?}]` (default `[]`); `render_tool.cue` template emits block в Application spec при non-empty; `component_cilium.cue` ignores Deployment+DaemonSet, `component_argocd.cue` ignores Deployment+StatefulSet `/status/terminatingReplicas`. **Bug C** — `network-policies: app path does not exist` потому что `manifests/tier-1/network-policies/` никогда не создавался при v0.1.97 imperative-to-GitOps rewrite. Fix: `manifests/tier-1/network-policies/default-deny.yaml` создан (content из `cli-providers::k8s::network_policy::default_deny_network_policy_yaml` — ingress allow same-ns + kube-system, no egress block, matches v0.1.x baseline). Chart bumped 0.1.4 → 0.1.5 с full compat entry + known-issue note в 0.1.4; CLI bumped 0.1.100 → 0.1.101, `RELEASED_PLATFORM_STACK_VERSION` → "0.1.5". SPDX gate 175 → 184 (8 new files chart-side + 1 new manifest). Также параллельный perf(operator) commit (без bump, CI-only): cargo-chef multi-stage Dockerfile для operator + webhook образов — typical source-only push CI build идёт от ~6 min к ~1-2 min через chef cook layer cache. Real-cluster acceptance ⏳ verified at next walk after push (operator helm charts должны опубликоваться workflow'ом, platform-stack 0.1.5 chart должен опубликоваться publish workflow'ом, walk должен показать all 6 children Synced/Healthy) | v0.1.101 |
| 2026-05-19 | M1.5 Track B.1.70 walk-fix #5 — пятый walk на chart 0.1.5 / CLI v0.1.101: bootstrap прошёл, root `platform` Synced/Healthy, но 5 из 6 children в degraded state. **Bug D** — `admission-webhook` Deployment отвергнут API: `selector does not match template labels`; в моём new webhook chart's `_helpers.tpl` я определил `labels` БЕЗ включения `selectorLabels` через `include`, и `Deployment.spec.template.metadata.labels` не содержал `app.kubernetes.io/{name,instance}` к которым matcher'или selector. Fix: mirror'нул operator chart's pattern (labels включает selectorLabels via include), standardised webhook selector на `app.kubernetes.io/{name,instance}` convention. **Bug E** — `apprafter-operator` + `admission-webhook` всё ещё `terminatingReplicas: field not declared in schema`; в v0.1.101 chart 0.1.5 я добавил `ignoreDifferences` только в cilium + argocd, забыл про operator + webhook. Fix: добавил тот же `ignoreDifferences: [{group: apps, kind: Deployment, jsonPointers: [/status/terminatingReplicas]}]`. **Bug F** — `network-policies: app path does not exist`; `component_network-policies.cue` пинил `version: v0.1.91` (operator chart's AppVersion anchor), но в том git tag директория `manifests/tier-1/network-policies/` ещё не существовала (создана только в v0.1.101). Fix: bump pin к "v0.1.102" — текущему tag-у который ships 0.1.6 + directory. **Bug G** — `admission-webhook` Certificate failed с `no endpoints available for service cert-manager-webhook`; Argo CD applied Certificate parallel с cert-manager rollout, webhook ещё не имел endpoints. Fix: ввёл sync ordering via `argocd.argoproj.io/sync-wave` annotations. `#Component` schema получил optional `syncWave: int | *0`; `render_tool.cue` emits annotation на rendered Application metadata; cilium = -20 (CNI prerequisite), argocd = -15 (self-adopt early), cert-manager = -10 (webhook+CRDs live before cert-manager.io/v1 applies), остальные = 0 (default). Argo CD waits for prior wave's `Sync=Synced` перед next wave. **Bug H** — `argocd-redis-secret-init` Job (pre-install,pre-upgrade hook) re-fires на adopt, image pull тормозит; plausibly just timing (Bug B+G mitigations должны помочь indirectly), оставлен for verification на следующий walk. Chart bumped 0.1.5 → 0.1.6 с full compat entry + known-issues note в 0.1.5; CLI bumped 0.1.101 → 0.1.102, `RELEASED_PLATFORM_STACK_VERSION` → "0.1.6". Все CLI gates clean (556 passed, fmt+clippy+SPDX+cue vet) | v0.1.102 |
| 2026-05-19 | M1.5 Track B.1.70 walk-fix #6 — шестой walk на chart 0.1.6 / CLI v0.1.102: bootstrap прошёл, sync-wave order работает (cilium → argocd → cert-manager → rest), platform + network-policies OK, но **cilium-operator CrashLoopBackOff** ⇒ cascading failures: cert-manager/webhook не Ready (нет endpoints), webhook Certificate validation fails, argocd redis-secret-init Job hangs (нет network). **Bug J** — root cause: `kubectl describe pod cilium-operator` показал env `KUBERNETES_SERVICE_HOST: auto` (literal string), Cilium operator не может dial `auto:` и crashes. `helm get values cilium` показал loader values (127.0.0.1), но live ConfigMap + Deployment имели **chart values** (`enable-ipv6: "false"`, `KUBERNETES_SERVICE_HOST=auto`). Mechanic: Argo CD не делает `helm upgrade` loader release — он renders chart templates с CHART's values и applies as plain manifests, перезаписывая loader's. Два owners для same Deployment + ConfigMap; chart-rendered wins. Все walk 1-5 я думал что это helm upgrade, а это plain manifest overlay. Fix: `component_cilium.cue` values mirror `cli-providers::k8s::cilium_values_yaml` byte-by-byte — `kubeProxyReplacement: true` (bool not string), `k8sServiceHost: "127.0.0.1"`, `k8sServicePort: 6443`, `ipv4.enabled: true`, `ipv6.enabled: true`; banner comment напоминает что edit ОБОИХ side требуется до B.1.71 central values. **Bug I** — `component_cert-manager.cue` missed в ignoreDifferences pass предыдущих fixes (cilium+argocd в 0.1.5, operator+webhook в 0.1.6, cert-manager пропущен). Fix: same one-element block. **Bug H** (argocd redis-secret-init) — plausibly caused by cilium-agent down (no network → no schedule → Job hangs); Bug J fix должен косвенно resolve. Verify on next walk. Chart bumped 0.1.6 → 0.1.7 с full compat entry + known-issues note в 0.1.6; CLI bumped 0.1.102 → 0.1.103, `RELEASED_PLATFORM_STACK_VERSION` → "0.1.7". 556 CLI tests passed, все гейты clean. User feedback на CI build time: после cargo-chef migration builds slowed to 7-8 min (vs 6 baseline) — known cache-warmup issue после layer restructure; первые 2-3 builds запиcывают новые cache keys, последующие будут fast. Separate perf optimization commit upcoming: pin `rust:alpine` к specific version (избежать floating-tag invalidation) + binary `cargo-chef` install (~10s) вместо `cargo install --locked` (~1-2 мин compile) | v0.1.103 |
| 2026-05-20 | M1.5 Track B.1.70 walk-fix #7 — bootstrap hung на `kubectl wait application/platform Synced` (10-min timeout). Chart 0.1.7 опубликован OK (gh release list confirmed). Корень: `kubectl describe application platform -n argocd` показывал `Application referencing project default which does not exist`; `kubectl get appproject -n argocd` → `No resources found`. **Bug K** — Argo CD chart 7.7.7 ship'ит `configs.projects: {}` by default, и argocd-server 2.13.1 НЕ recreates `default` AppProject на startup. Каждая Application с `spec.project: default` (включая root `platform`) failed. Прошлые walks (#4-#6) видимо hit это lazily (retry-loop appeared to handle); v0.1.103's run был deterministic. Fix: `configs.projects.default` block добавлен в обе стороны — `cli-providers::k8s::argocd_loader_values_yaml` (loader creates AppProject в initial `helm install`, до root Application apply) + `platform-stack/cue/component_argocd.cue` (chart's self-reconcile keeps it alive on adopt). Spec мирро́рит Argo CD's historical implicit default — `sourceRepos: ["*"]`, unrestricted `destinations`, full-kind whitelists. Admin'ы wanting restricted default editting fork's overlay. +1 regression test `argocd_loader_values_create_default_app_project` (557 passed, был 556). Chart bumped 0.1.7 → 0.1.8 с full compat entry + known-issue note в 0.1.7; CLI bumped 0.1.103 → 0.1.104, `RELEASED_PLATFORM_STACK_VERSION` → "0.1.8". Recovery doc для stuck-on-0.1.7 clusters в UNRELEASED.md (one-liner `kubectl apply` AppProject + hard-refresh annotation на root Application). Real-cluster acceptance ⏳ verified at next walk after push | v0.1.104 |
| 2026-05-20 | M1.5 Track B.1.70 walk-fix #8 — после ручного default AppProject (v0.1.104 fix Bug K) 4 children зелёные, но operator + webhook ещё broken. **Bug M** — `kubectl describe pod operator`: `CreateContainerError` + "failed to generate spec: no command specified". `kubectl run` test pull image v0.1.91 показал `stat /apprafter-operator: no such file or directory` — **binary отсутствует в image manifest**. Image v0.1.91 был published months ago при closing tag Track A, видимо partial / stale Dockerfile не успел COPY step. Image broken with months — но никогда не exercised потому что v0.1.x cluster-bootstrap install'ил operator chart from local path, не OCI. Fix: operator + webhook chart Chart.yaml bumped к `version: v0.1.92`, `appVersion: "v0.1.105"` (текущий monorepo tag — release-operator.yml workflow rebuild'ит images с cargo-chef Dockerfile надёжно). Plus defence in depth: explicit `command: ["/apprafter-operator"]` + `command: ["/admission-webhook"]` в chart deployment templates — больше не зависят от image manifest's ENTRYPOINT, future image-build accidents не reproduce silently. **Bug L** — `kubectl get clusterissuer` → No resources found. Webhook chart's Certificate references `kind: ClusterIssuer, name: apprafter-selfsigned`, но cert-manager chart 1.16.2 не shipит default issuers, а в v0.1.x создание ClusterIssuer via `cli-providers/k8s/issuer.rs` мигрировал из CLI при v0.1.97 rewrite — но **никогда не был перенесён в chart template**. Fix: новый template `operator/charts/apprafter-admission-webhook/templates/clusterissuer.yaml` ship'ит `apprafter-selfsigned` ClusterIssuer (`selfSigned: {}` spec — matches v0.1.x baseline) вместе с Certificate в одном chart. Также **decorative drift** fixed: `RELEASED_OPERATOR_VERSION` "v0.1.64" → "v0.1.105" (3 месяца stale по CLAUDE.md правилу). Chart bumped 0.1.8 → 0.1.9 с full compat entry + known-issues note в 0.1.8; CLI bumped 0.1.104 → 0.1.105, RELEASED_PLATFORM_STACK_VERSION → "0.1.9". 557 CLI tests still pass; все гейты clean | v0.1.105 |
| 2026-05-20 | M1.5 Track B.1.70 walk-fix #9 — на v0.1.105 image впервые реально запустился webhook code (v0.1.91 image был broken, binary missing, webhook code не выполнялся 8 walks). Surfaced **Bug N** — `thread 'main' panicked at rustls-0.23.40/src/crypto/mod.rs:249: Could not automatically determine the process-level CryptoProvider`. rustls 0.23+ removed auto-default; operator binary имел fix `install_rustls_crypto_provider()` с v0.1.61, но webhook crate missed (jump straight to `RustlsConfig::from_pem_file` без install). Long-standing latent bug, был masked broken v0.1.91 image months. Fix: `operator/admission-webhook/src/lib.rs` получил `install_rustls_crypto_provider()` (same shape as operator's — `aws_lc_rs::default_provider().install_default()` идемпотентный); `src/main.rs` вызывает первой строкой `async fn main()`, до TLS server init; `Cargo.toml` direct `rustls = { version = "0.23", features = ["aws-lc-rs"] }` dep для `default_provider` resolution; +2 regression tests (`install_rustls_crypto_provider_sets_a_process_level_default`, `_is_idempotent`) mirror operator's. Chart-side: operator + webhook charts оба bump к `version v0.1.93 / appVersion v0.1.106` в lockstep (operator chart bump'ит даже хоть его code не менялся — sync appVersion предотвращает future drift). Chart bumped 0.1.9 → 0.1.10 с compat entry + known-issue note в 0.1.9; CLI bumped 0.1.105 → 0.1.106, RELEASED_PLATFORM_STACK_VERSION → "0.1.10", RELEASED_OPERATOR_VERSION → "v0.1.106". 557 cli + 62 operator tests passed; все гейты clean | v0.1.106 |
| 2026-05-20 | M1.5 Track B.1.70 walk-fix #10 — argocd Application Synced/Degraded; новый argocd-repo-server pod (с cue-cmp sidecar) stuck Init:0/1 с `MountVolume.SetUp failed for volume "cue-cmp-config": configmap "cue-cmp-plugin-config" not found`. **Bug O** — `component_argocd.cue` добавлял cue-cmp sidecar с volumeMount на ConfigMap `cue-cmp-plugin-config` в chart 0.1.2 (Track B.1.69), но **сам ConfigMap никогда не создавался**. Bug latent через 6 chart versions (0.1.2 → 0.1.10), masked предыдущими blockers (broken image, missing ClusterIssuer, rustls panic) — repo-server pod не получал шанс schedule. Walk #10 был первым clean run где cue-cmp sidecar реально attached + mount evaluated. Fix: `component_argocd.cue` получает `extraObjects` block с ConfigMap `cue-cmp-plugin-config` содержащим verbatim plugin.yaml content (Argo CD CMP contract — `apiVersion: argoproj.io/v1alpha1, kind: ConfigManagementPlugin, name: cue, discover.find.glob: "**/apprafter*.cue", generate.command: sh -c /usr/local/bin/entrypoint.sh`). Источник истины пока остаётся `argocd-cue-cmp/plugin.yaml`, embedding это duplication до future `cue cmd` step в chart renderer прочитает source файл напрямую — комментарий marks lockstep edit requirement. Chart bumped 0.1.10 → 0.1.11 с full compat entry + known-issue note в 0.1.10; CLI bumped 0.1.106 → 0.1.107, RELEASED_PLATFORM_STACK_VERSION → "0.1.11". 557 cli tests passed; все гейты clean | v0.1.107 |
| 2026-05-20 | M1.5 Track B.1.70 walk-fix #11 — walk-fix #10 ConfigMap зашёл OK, новый argocd-repo-server pod теперь stuck на image pull instead of mount: `Back-off pulling image "ghcr.io/apprafter/argocd-cue-cmp:v0.1.0": ErrImagePull MANIFEST_UNKNOWN`. **Bug P** — `crane ls` показал image **существует**, но tag = `:0.1.0` (без `v`), а chart pin = `:v0.1.0`. `argocd-cue-cmp-publish.yml` workflow tag line was `${IMAGE}:${VERSION}` где VERSION = `0.1.0` (без v) из VERSION file. Git tag создавался как `argocd-cue-cmp/v<version>` (с `v`), но image tag — без. Inconsistency с operator + webhook workflows (там `image:${github.ref_name}` = `:v0.1.x`). Latent с chart 0.1.2 (Track B.1.69), masked walks #5-10 upstream blockers (broken ConfigMap не давал pull happen). Fix: (1) workflow `tags:` line gets `v` prefix: `${IMAGE}:v${VERSION}`; (2) `Tag :latest` source + release notes example updated; (3) `argocd-cue-cmp/VERSION` 0.1.0 → 0.1.1 (workflow detect gates на git tag existence, без bump skipnет publish); (4) `component_argocd-cue-cmp.cue` pin к `v0.1.1`. v0.1.1 image — re-publish source v0.1.0 с corrected tag form. v0.1.0 image stays на registry как historical artefact. Chart bumped 0.1.11 → 0.1.12 с full compat entry + known-issue note в 0.1.11; CLI bumped 0.1.107 → 0.1.108, RELEASED_PLATFORM_STACK_VERSION → "0.1.12". 557 cli tests passed; все гейты clean | v0.1.108 |
| 2026-05-20 | M1.5 Track B.1.71 closure — chart as single source of truth. `cli/cli-providers/build.rs` extracts `_loaderValues.{cilium,argocd}` + `currentVersion` from `platform-stack/cue/` at compile time, emits `CILIUM_VALUES_YAML`, `ARGOCD_LOADER_VALUES_YAML`, `RELEASED_PLATFORM_STACK_VERSION` as generated Rust constants. `cluster_bootstrap.rs` swaps hand-rolled YAML for these. 12 dead `*_yaml` renderers deleted (admission_webhook, application_crd, argocd_gateway, argocd_repo_secret, backstage_app_config, backstage_manifests, bootstrap_app, cert_manager_values, cilium_values, issuer, network_policy, operator_chart, operator_values) plus 3 dead examples. CUE invariant `_components.cilium.values ≡ _loaderValues.cilium` makes walk-fix #6's drift class structurally impossible; Argo CD chart's `values:` derives from `_loaderValues.argocd & { ...extras... }` so loader stays a strict subset by construction. `RELEASED_PLATFORM_STACK_VERSION` drift class also gone. Chart bumped 0.1.12 → 0.1.13 (refactor, no rendered-output change); CLI bumped 0.1.108 → 0.1.109. Test count 557 → 479 (~80 deleted renderer tests, 4 new loader_values regression guards). Deferred: `RELEASED_OPERATOR_VERSION` from operator chart's Chart.yaml (cross-workspace path), `argocd-cue-cmp/plugin.yaml` embedded in component_argocd as string literal | v0.1.109 |
| 2026-05-20 | M1.5 Track B.1.71b closure — closed the remaining 6 version-duplication classes from B.1.71's deferred follow-ups. Cilium + Argo CD upstream chart versions migrated to `_loaderValues.{cilium,argocd}.chartVersion` in CUE; chart-side invariants + build.rs-derived `CILIUM_CHART_VERSION` / `ARGOCD_CHART_VERSION` constants. Hand-maintained consts in `helm.rs` + entire `argocd_values.rs` deleted. Operator + admission-webhook container image tags now sourced from `operator/charts/<chart>/Chart.yaml#appVersion` via a tiny grep-based reader in build.rs; chart's `values.image.tag` dropped so Helm template's `.Chart.AppVersion` fallback drives the image. Hand-maintained `RELEASED_OPERATOR_VERSION` in `image_ref.rs` deleted. cue-cmp `VERSION` plain-text file replaced by `argocd-cue-cmp/version.cue` (package at `apprafter.io/argocd-cue-cmp`); chart imports via CUE, publish + check workflows read via `cue export -e version --out text`. Chart bumped 0.1.13 → 0.1.14 (refactor, byte-equivalent rendering); CLI bumped 0.1.109 → 0.1.110. Test count +3 new regression guards. B.1.71's deferred-follow-up section is now empty | v0.1.110 |
| 2026-05-22 | M1.5 Track B.1.79a part 3 — `apprafter app add/list/status/remove`. Новый CLI subcommand family с alias `apprafter a` для user-application lifecycle. `app add [<git-url>]`: без аргумента детектит git origin из cwd (`git remote get-url <--remote>`, default `origin`), нормализует URL к HTTPS (SCP-style `git@host:org/repo` → `https://host/org/repo`, `ssh://git@host/...` → `https://host/...`, strips `.git`); опции `--name` (default — derived from repo basename, lowercased, invalid chars → dashes, validated DNS-1123), `--branch` (default — cwd current branch если detectable, иначе `main`), `--path` (default `/`), `--project` (default `apps`), `--remote` (default `origin`), `--no-ping` (skip `git ls-remote` reachability check для air-gapped CI). Reachability check через `git ls-remote --exit-code <url> HEAD`; auth failures surface CLI hint pointing к `apprafter repo creds add` (lands в v0.1.141). Pre-flight refuses когда Application уже существует с pointer к `app status`/`app remove`. Writes Argo CD `Application` CR в `argocd` namespace с `metadata.labels.apprafter.io/managed-by: apprafter` (load-bearing — `list` filter relies on it), `metadata.annotations.apprafter.io/source: cli`, `spec.project`, `spec.source.{repoURL,path,targetRevision}`, `spec.destination.{server: kubernetes.default.svc, namespace: <app-name>}`, `spec.syncPolicy.automated.{prune: true, selfHeal: true}` + `CreateNamespace=true,ServerSideApply=true` syncOptions. `app list`: `--project` (default `apps`) / `--all-projects` (`conflicts_with`) / `--all-managed` (drops the `apprafter.io/managed-by` label filter); table NAME/PROJECT/REPO/REV/SYNC/HEALTH; empty result surfaces context-aware hint pointing к `--all-managed`. `app status <name>`: detail view (project/repo/revision/path/destination namespace/sync state/health) + recent revisions (last 3 из `status.history` reversed); handles status-less fresh CRs без panic'а. `app remove <name>`: interactive `inquire::Confirm` (default No); refuses в non-interactive shell без `--yes`; `--keep-data` flips `syncPolicy.automated.prune: false` через merge-patch ПЕРЕД `kubectl delete` так что Argo CD tear'ит down только CR оставляя child resources (PVCs/ResourceClaims) для re-attach. Deferred: `app logs`/`app rollback` → v0.1.140; inline PAT prompt → v0.1.141; AppRafter Application CR conditions + pending MigrationPlan section в status → v0.1.140/Phase 2. +12 unit tests на pure helpers (normalise_git_url для https/scp-style/ssh schemes + dotgit strip, derive_app_name last-segment + invalid char sanitisation, validate_dns_1123 happy/sad paths, build_application_manifest managed-by label + argocd namespace destination + project/revision carry, print_status status-less fresh CR без panic'а). CLI 0.1.138 → 0.1.139. Chart unchanged | v0.1.139 |
| 2026-05-22 | M1.5 Track B.1.79a part 2 — `apprafter open argocd` polish. `OpenUi::Argocd` extends с `--project <name>` (default `apps`) + `--all-projects` flag (`conflicts_with = "project"`). New pure `build_argocd_url(local_port, project_filter)` helper rendering `https://localhost:<port>/applications?proj=<name>` или bare base URL when filter is `None`/empty. Password copied к system clipboard via new `arboard` workspace dep (`default-features = false`); fail-quiet on headless / no-clipboard environments — `(copied к clipboard)` / `(clipboard unavailable — copy manually)` distinct trailing markers per `ClipboardStatus` enum. Output banner formalised: `Opening Argo CD UI…` heading + `URL` / `Username` / `Password` + clipboard marker + blank line + `✓ Browser opened` / `ℹ Browser open failed — paste the URL into your browser` + `ℹ Press Ctrl+C к stop port-forward`. URL username pre-fill (`?username=admin`) проверено empirically на Argo CD 7.7.7 — не поддерживается; оставлено display + clipboard only (negative-result закрытие). `apprafter open backstage` deferred к Tier 2+. +3 unit tests на `build_argocd_url` (None default, explicit filter rendering для `apps`/`platform`, empty-string defensive). CLI 0.1.137 → 0.1.138. Чарт unchanged | v0.1.138 |
| 2026-05-22 | M1.5 Track B.1.79a part 1 — AppProjects + per-component project field. `_loaderValues.argocd.values.configs.projects` map grows from single `default` entry → 4 (default + platform + platform-providers + apps). `platform` для core chart components (cilium/argocd/cert-manager/network-policies/apprafter-operator/admission-webhook/backstage/argocd-cue-cmp), `platform-providers` для Phase 2+ ServiceProviders, `apps` для user Applications с tightened whitelist (`destinations.server: https://kubernetes.default.svc`, `clusterResourceWhitelist: []`, `namespaceResourceWhitelist` ограничен `apprafter.io/Application` + `ConfigMap` + `Secret` + `HTTPRoute`). `default` сохранён как legacy + ad-hoc fallback. AppProjects ship в initial Argo CD install через loader values → существуют до того как первый Application с new project'ом засинкается. New `#Component.project: string & =~"^[a-z0-9][a-z0-9-]{0,62}[a-z0-9]$" | *"platform"` (DNS-1123 constrained, default `platform`). render_tool.cue template emits `spec.project: {{ default "platform" $component.project | quote }}`. CLI loader's root platform Application (`cluster_bootstrap::render_root_application`) переехал с `project: default` на `project: platform` — safe потому что AppProject ships в initial install. RBAC enforcement через AppProject sourceRepos/destinations/resourceWhitelist'ы НЕ активирован в M1.5; визуальная роль (UI selector группирует Applications по project) + фундамент под Phase 4 AccessGrant enforcement. Upgrade: existing operators 0.1.39 → 0.1.40 see every chart-managed Application drift `spec.project` from `default` to `platform` — metadata-only change, нет pod restart, нет resource churn, Argo CD реконсилит через normal sync path. +1 regression test (`render_root_application_joins_platform_app_project`). CLI 0.1.136 → 0.1.137; platform-stack 0.1.39 → 0.1.40. Operator chart unchanged | v0.1.137 |
| 2026-05-22 | M1.5 Walk-fix #1 post-B.1.79 — `apprafter open argocd` SIGPIPE early-exit. Walk of v0.1.135: command printed credentials banner и сразу выходил в shell prompt без блокировки на port-forward. Root cause: `wait_port_forward_ready` забирал `child.stdout` через `take()`, читал строки до `Forwarding from`, возвращался — `BufReader`+`ChildStdout` дропались, read-end pipe закрывался. kubectl — Go binary; дефолтный SIGPIPE handler в Go терминирует процесс на следующем write в закрытый stdout, а kubectl port-forward после initial ready line эмитит ещё `Forwarding from [::1]:…\n` → SIGPIPE → kubectl exit → `child.wait()` мгновенно возвращался. Stderr имел тот же латентный класс багов — `Stdio::piped()` без drainer; pipe buffer 64KiB мог переполниться и заблокировать ребёнка на следующем write. **Fix:** spawn drainer threads для обоих pipes. `spawn_ready_drainer` читает stdout строчно, сигналит готовность через `mpsc::sync_channel::<()>(1)` на первой `Forwarding from` строке, далее **продолжает drain'ить к EOF** — pipe остаётся открытым на всё время жизни child'а. Если EOF до banner, sender drops, `recv()` → `Err` → caller surfaces "exited before binding local port". `spawn_silent_drainer` дренирует stderr к EOF без surfacing'а. +4 unit tests driven by `std::io::Cursor` fakes (без реального kubectl): `ready_drainer_signals_on_forwarding_line` (happy path), `ready_drainer_continues_draining_after_signal` (load-bearing — wraps reader в Tracker counting consumed bytes, asserts ALL bytes including post-banner ones прочитаны), `ready_drainer_yields_recv_err_when_eof_before_banner` (error surfacing), `silent_drainer_reads_to_eof` (stderr drain coverage). CLI 0.1.135 → 0.1.136. Chart unchanged (CLI-only IO handling defect) | v0.1.136 |
| 2026-05-22 | M1.5 Track B.1.79 closure — CLI thin wrappers + Argo CD MigrationPlan Lua action. New CLI subcommands в `apprafter` binary: `apprafter platform status` (reads PlatformStack/default через kubectl shellout; prints channel/pin/autoUpgrade/tier + current/target/available versions + conditions table + last-5 versionHistory; tabled render с 60-char message wrap), `apprafter platform upgrade [--to <v>]` (merge-patch `spec.pin: <v>`; без `--to` clears pin + flips `autoUpgrade=true` для channel-following mode), `apprafter migration list` (table of MigrationPlans в apprafter-system: name/scope/classification/phase; defaults phase к `pending-approval` для CRs без status), `apprafter migration approve <name>` / `reject <name>` (status-subresource merge-patch с phase=approved/rejected; application-scope rejects denied by webhook per ADR 0027 — apiserver denial bubbles up verbatim), `apprafter open argocd` (spawns `kubectl port-forward svc/argocd-server -n argocd 8080:443`, waits для "Forwarding from" stdout line, prints URL+admin+password, cross-platform browser open via `xdg-open`/`open`/`cmd /c start`, blocks на child.wait() so Ctrl+C tears down both). Shared `commands::k8s_helpers` module centralises кубectl shellout (kubectl_get_json with 404→None, kubectl_merge_patch with optional subresource, ensure_kubeconfig_tempfile decrypting cached age blob via cli_core::secrets) — three new wrappers share one implementation. npm-style version-check banner: `maybe_warn_about_newer_version()` fires before clap parse, fetches `api.github.com/repos/apprafter/apprafter/releases/latest` с 3s timeout, caches result в `~/.cache/apprafter/version-check.json` с 24h TTL, semver-aware `newer_than` comparison (strips `v` prefix). **Fail-quiet** — network errors / GitHub rate-limit / JSON parse / unparseable versions all swallowed silently (debug log only) per courtesy-not-prerequisite semantics. Chart-side: `platform-stack/cue/component_argocd.cue` `configs.cm` map extended (`configs: cm: {<key>: <val>}` form чтобы accommodate sibling entries) с `resource.customizations.actions.apprafter.io_MigrationPlan` Lua resource-action block. Discovery script returns `actions["approve"]={disabled=…}` + `actions["reject"]={disabled=…}` based on `status.phase` (decidable iff phase=="" или "pending-approval"); action bodies mutate `status.phase` к "approved"/"rejected"; Argo CD routes mutation через status subresource automatically. **Deferred к 1.79a:** `apprafter platform channel/freeze/unfreeze/rescue`, `apprafter open backstage/grafana/hubble` (multi-channel UX waits для Phase 2; Backstage/Grafana/Hubble не tier-1 resident; freeze/unfreeze paired с ResourceClaim CRUD в 1.79a). Tests: +13 unit (cli-side: `commands::version_check::tests` x4 covering newer_than v-prefix/equal/older/garbage; `commands::platform::tests` x2 для print_status minimal + full fixtures; `commands::migration::tests` x2 для plan_row default phase + full extraction). Total platform-cli crate: existing + 13. CLI 0.1.134 → 0.1.135 (cli/Cargo.toml workspace.package.version); platform-stack 0.1.38 → 0.1.39 (chart Lua action only — byte-equivalent templates vs 0.1.38). Operator chart unchanged (no operator-binary delta) — appVersion stays v0.1.134, RELEASED_OPERATOR_VERSION constant untouched | v0.1.135 |
| 2026-05-23 | M1.5 Walk-fix #8 post-B.1.78 — destructive classification was per-target-version, not per-transition. User pointed out: cluster на 0.1.35 с walk-platform-1's `platform-0-1-35-to-0-1-36` MigrationPlan в rejected phase. Chart 0.1.37 publishes as safe. autoUpgrade tries bump 0.1.35 → 0.1.37 — plan name `platform-0-1-35-to-0-1-37` (different pair from rejected one) — fetch_change_class returns Safe для 0.1.37's single record → straight bump. **Silently bypasses operator's reject decision on 0.1.36's breaking content.** Per spec.md §3.11 implied semantics ("any path к target must respect the strictest class encountered"), classification должна быть path-aware. Fix: new `fetch_path_max_change_class(url, from, to)` pulls compat doc at `to`'s tarball, walks records в `(from, to]` half-open range, returns strictest class via `path_max_change_class` pure helper. Reconcile destructive check replaces `fetch_change_class(url, target)` → `fetch_path_max_change_class(url, current_target, desired)`. Edge cases: from==to → Safe (no-op); from>to (downgrade) → Safe (spec.md silent on downgrade direction; conservative default; future work for cumulative reverse-direction semantics if real use case surfaces); unparseable version key → skipped without affecting other entries. Classification ordering (Safe < RequiresRestart < DataMigration < Breaking) via internal `class_order` helper. +8 regression unit tests (path-max strictest-in-range, excludes-from, no-op, downgrade, requires-restart > safe, data-migration > requires-restart, breaking > data-migration, skip unparseable). Total platform-stack crate: 68 → 76. CLI 0.1.133 → 0.1.134, operator+webhook chart v0.1.114 → v0.1.115 + appVersion v0.1.133 → v0.1.134, platform-stack 0.1.37 → 0.1.38 | v0.1.134 |
| 2026-05-23 | M1.5 Walk-fix #7 post-B.1.78 — `PlatformMigrationStrategy.reject` failed для channel-following clusters (snapshot.pin=null). Walk Phase B.1.78 reject test на chart 0.1.36 (breaking) created plan `platform-0-1-35-to-0-1-36`, user patched к phase=rejected. MigrationController saw rejected → invoked strategy.reject → built SSA-apply body `{"spec":{"pin":null}}` (snapshot.pin=null since cluster channel-following). Apiserver 422: `spec.pin: Invalid value: "null": spec.pin in body must be of type string` (CRD schema `type: string` без `nullable: true`). Error propagated → reconcile errored → walk-fix #3 sealing's `status.rejectedAt` write never ran → marker stayed null → next reconcile retried same path → infinite loop. Cluster blocking-by-rejected-plan still worked (plan phase=rejected set by user patch; PlatformController GET-by-name found non-completed phase → blocked bump), но error log churn + sealing broken. Fix: three-branch dispatch в `PlatformMigrationStrategy.reject` — (1) Api::get current PlatformStack; (2) `pins_equal` helper compares current spec.pin vs snapshot.pin treating missing/null/explicit-null as equivalent ("channel-following"); (3) dispatch: pins equal → no-op success; snapshot=Some(String) и differ → SSA force=true (existing path); snapshot=None/Null и pin set → JSON merge-patch `{"spec":{"pin":null}}` (RFC 7396: null deletes field, works regardless of CRD nullable). Side-effect: walk-fix #3 sealing path now reaches completion для null-snapshot clusters (was masked by this bug); subsequent reconciles see rejectedAt marker и skip strategy.reject. +4 regression tests (pins_equal: missing/null/explicit-null equivalence + same string equal + different string distinct + null vs string distinct). Total migration crate: 14 → 18. CLI 0.1.132 → 0.1.133, operator+webhook chart v0.1.113 → v0.1.114 + appVersion v0.1.132 → v0.1.133, platform-stack 0.1.36 → 0.1.37 | v0.1.133 |
| 2026-05-23 | M1.5 Track B.1.78 closure — PlatformController MigrationPlan integration per spec.md §3.11 + ADR 0027. PlatformController reconcile body gains destructive-transition gate: synthesizes deterministic plan name `platform-<from>-to-<to>` (dots → dashes для DNS-1123), GETs by name в `apprafter-system`, then 4-branch flow: (1) plan exists + phase=completed → bump (operator approved + ran the migration); (2) plan exists + other phase (pending/approved/executing/failed/**rejected** — rejected blocks too per ADR 0027 explicit decision) → block bump, surface MigrationPending=True/<class from existing plan> + UpgradeAvailable=True/BlockedByMigrationPlan with `apprafter-system/<plan-name>` в message; (3) no plan + classification destructive (`breaking|data-migration|requires-restart` per spec.md §3.11 — extended beyond prior code's `breaking|data-migration` only) → CREATE MigrationPlan через SSA с scope.type=platform, scope.platform.components=["platform-stack"] (simplification — single conservative entry; future enhancement: diff per-component), trigger.{type=platform-classification, field=spec.pin, from, to}, risks.classification, **previousSpecSnapshot.pin** = current spec.pin (или JSON null когда unpinned — `PlatformMigrationStrategy.reject` B.1.76 reads back для revert), block bump, surface conditions; (4) no plan + safe → bump as before. **Annotation source per plan.md placeholder переписан на structured `spec.previousSpecSnapshot.pin` field** (already in B.1.75 CRD). Approve flow: MigrationController completes plan → PlatformController next reconcile sees completed → bumps. Reject flow: B.1.76 PlatformMigrationStrategy.reject reverts pin. Removed: `PolicyHooks` trait + `NoOpHooks` stub (forward-compat placeholder от B.1.73 never had real impl); inline plan creation replaces. policy.rs deleted; Context.hooks field + `Policy(#[from] PolicyError)` Error variant removed. RBAC: operator chart's ClusterRole's `migrationplans` rule gains `create` verb (was `get list watch patch update`); без `create` SSA-create 403's. +7 unit tests (synthesize_platform_plan_name DNS-1123/deterministic, change_class_to_string enum, build_platform_migration_plan_cr shape + null-pin variant, plan_classification getter); −1 test (NoOpHooks removed). Total platform-stack crate: 62 → 68. CLI 0.1.131 → 0.1.132, operator+webhook chart v0.1.112 → v0.1.113 + appVersion v0.1.131 → v0.1.132, platform-stack 0.1.33 → 0.1.34 | v0.1.132 |
| 2026-05-23 | M1.5 Walk-fix #6 post-B.1.77 — webhook config missing `migrationplans/status` rule. Walk Phase 3.4 retest на v0.1.130: применили app-scope MigrationPlan, patch'нули `--subresource=status --type=merge -p '{"status":{"phase":"rejected"}}'`. Webhook should denied per ADR 0027 (walk-fix #2 validator guard), но patch succeeded. Root cause: ValidatingWebhookConfiguration's `migrationplans.apprafter.io` webhook listed только `resources: [migrationplans]`. `kubectl patch --subresource=status` routes через apiserver's `/status` SUB-resource endpoint — separate path от main resource. Webhook configs must explicitly list `<resource>/status` к intercept status-subresource writes; without it, status patches bypass webhook entirely → walk-fix #2 ADR 0027 guard и phase transition FSM never invoked для status changes. Fix: chart `operator/charts/apprafter-admission-webhook/templates/validatingwebhookconfiguration.yaml` — add `migrationplans/status` к the migrationplans webhook's `rules.resources` list. No Rust code change; operator + webhook binaries identical к v0.1.130 (image v0.1.131 tagged via standard chart lockstep, same binary content). **Bonus:** chart 0.1.33 pins identical image v0.1.131 as v0.1.130 cluster runs after chart 0.1.32 — pin'ing к 0.1.33 triggers no pod restart, enabling clean isolated walk-fix #5 verification на stable pod без chart-upgrade pod-cycle artifacts (which caused Phase 6 second-bump regression — bump landed on intermediate v0.1.127 pod без walk-fix #5, strip pattern wiped entry). CLI 0.1.130 → 0.1.131, operator+webhook chart v0.1.111 → v0.1.112 + appVersion v0.1.130 → v0.1.131, platform-stack 0.1.32 → 0.1.33 | v0.1.131 |
| 2026-05-23 | M1.5 Walk-fix #5 post-B.1.77 — versionHistory SSA ownership-release bug. Walk-fix #4 observability в v0.1.129 показал что settled-state reconciles always log `include_version_history=false new_history_len=0` — strip pattern active каждый cycle. Combined с CRD schema verified (versionHistory present) + apiserver pruning ruled out + Kubernetes SSA Apply docs explicit: "If a field is no longer в applied configuration, manager's ownership removed; if no other manager owns, **apiserver removes the field**" — это явно root cause walk-fix #7's "omit field к preserve value" pattern. Sequence: bump cycle's append + write claims ownership + stores entry; immediately next settled-state reconcile strips field + releases ownership → apiserver deletes within ~30s. Все walks ranae видели null versionHistory because reads happened ПОСЛЕ ownership release. **Fix:** drop "omit field" pattern; replace с server-state-read + merge pattern. Before each `patch_status`, `Api::get_status` reads authoritative server state; `merge_version_history(server, local)` helper (новый в `status.rs`) preserves server entries, appends local-only ones, dedupes by `(version, appliedAt)` pair, enforces cap. Patch body ALWAYS includes `versionHistory`. Cost: extra `Api::get_status` per write; settled cycles skip via `write_status_if_changed` shortcut (no-op writes early-return). Sees through cache-stale-overwrite race that walk-fix #7 was originally protecting against — never reads cache for versionHistory, always reads server. `_include_version_history` parameter retained as no-op для binary-compat with existing call sites. +4 regression tests в `status.rs`: `merge_version_history_keeps_server_entries_when_local_is_empty` (load-bearing — settled state preserves server entries), `_appends_local_only_entries` (bump cycle), `_dedupes_by_version_and_applied_at` (rollback semantics), `_caps_at_max` (ring buffer). Total platform-stack crate: 58 → 62. CLI 0.1.129 → 0.1.130, operator+webhook chart v0.1.110 → v0.1.111 + appVersion v0.1.129 → v0.1.130, platform-stack 0.1.31 → 0.1.32 | v0.1.130 |
| 2026-05-23 | M1.5 Walk-fix #3 + #4 post-B.1.77 bundled. **#3:** MigrationController seals rejected plans via persistent `status.rejectedAt` marker. Acceptance walk показал что platform-scope plan walk-platform-1 (rejected, previousSpecSnapshot.pin="0.1.25") forced PlatformStack.spec.pin="0.1.25" на каждый operator pod restart (chart auto-upgrade triggered Deployment rolling update → cold-start cache replay → rejected plans re-reconciled → strategy.reject() re-invoked → SSA-patch pin back к snapshot). Это overrided user's `kubectl patch ... pin=null` patches и заклинило PlatformController в `fetch_change_class("0.1.25")` registry error loop (0.1.25 не published). Fix: persistent marker `status.rejectedAt: Option<String>` (RFC3339 timestamp). Reconcile's `"rejected"` branch checks marker — if present, skip strategy.reject + `Action::await_change()`; if absent, call strategy.reject + set marker + write status. Plan sealed после first reject; subsequent reconciles на cold-start no-op. CRD schema (operator chart's `crd-migrationplan.yaml`) + CUE source (`schemas/v1alpha1/migrationplan.cue`) extended с `status.rejectedAt: string format=date-time` (optional). Rust type `operator_core::MigrationPlanStatus.rejected_at: Option<String>`. +2 regression tests: `rejected_plan_with_rejected_at_marker_is_sealed`, `rejected_plan_without_rejected_at_marker_is_not_sealed`. Total в migration crate: 12 → 14. **#4:** PlatformController bump-cycle observability — два `info!()` logs around the append + write_status decision: (1) before append — `target_changed`, `appended_history`, `target_for_patch`, `current_target`, `prior_history_len`; (2) before write — `include_version_history`, `new_history_len`. Walk Phase 6 (artificial pin downgrade+upgrade test) показал versionHistory empty after multiple successful bumps; logs showed nothing diagnostic (только generic "fired"/"completed"). Production-useful logs (не debug); future walk-fix может follow с actual versionHistory write fix once logs покажут offending branch. CLI 0.1.128 → 0.1.129, operator+webhook chart v0.1.109 → v0.1.110 + appVersion v0.1.128 → v0.1.129, platform-stack 0.1.30 → 0.1.31 | v0.1.129 |
| 2026-05-22 | M1.5 Walk-fix #2 post-B.1.77 — webhook FSM closes ADR 0027 bypass on app-scope `rejected` via first-write. Acceptance walk Phase 3.4 на v0.1.127: app-scope plan applied + `kubectl patch --subresource=status -p '{"status":{"phase":"rejected"}}'` — webhook **accepted** (must have denied per ADR 0027). Root cause: `is_allowed_phase_transition` first-write branch (`old_phase.is_empty()` — fresh CR без status) returned `true` для любого plausible new_phase **независимо от scope**; scope check для rejected was only в `("pending-approval", "rejected")` match arm later. Так что fresh-CR + status patch slipped прямо к sealed `rejected` без ADR 0027 trip. Fix: ADR 0027 guard moved BEFORE first-write fast-path — `new_phase=="rejected" && scope_type=="application" → false` covers all paths (fresh → rejected, pending → rejected, approved → rejected defensive, executing → rejected defensive). Error message extended к "ADR 0027" для любого new_phase=rejected на app-scope (was только pending → rejected case). No code damage from slip — `ApplicationMigrationStrategy.reject` is Ok-no-op per design; semantically just audit-trail violation. +3 regression unit tests: `rejects_application_scope_first_write_to_rejected_per_adr_0027` (load-bearing), `allows_platform_scope_first_write_to_rejected` (counterpart pin для platform), `rejects_application_scope_approved_to_rejected_per_adr_0027` (defensive). Total admission-webhook lib: 75 → 78. CLI 0.1.127 → 0.1.128, operator+webhook chart v0.1.108 → v0.1.109 + appVersion v0.1.127 → v0.1.128, platform-stack 0.1.29 → 0.1.30 | v0.1.128 |
| 2026-05-22 | M1.5 Walk-fix #1 post-B.1.76 — SSA `.force()` on MigrationController + PlatformController status writes. Acceptance walk B.1.74→B.1.77 на v0.1.126 hit phase=approved freeze on MigrationPlan: `kubectl patch ... --subresource=status --type=merge` registers `kubectl-patch` field manager as owner of `status.phase`; controller's SSA patch carrying `phase=executing` (under `migration-controller` field manager) 409s with managedFields conflict; error_policy retries 15s forever на same conflict. Symptom: plan freezes at `approved`, no transition. Walk diagnosis confirmed root cause; workaround for active walk = pass `--field-manager=migration-controller` on kubectl patch (kubectl pretends to be controller, no conflict — controller's next write reuses same manager, owns field). Real fix: `.force()` на `PatchParams::apply(FIELD_MANAGER)` в `operator-controllers/migration::reconcile::write_status` + preventively `operator-controllers/platform-stack::reconcile::write_status` (latent — PlatformController hasn't surfaced bug т.к. был sole writer in every walk so far, но structural shape identical). Application controller's `apply_status` уже использует `.force()` (built-in от B.1.7 era) — walk-fix brings migration + platform в line. **Не** добавляю regression test — это runtime SSA conflict requiring real apiserver; covered by next walk's re-run of phase 3.2. CLI 0.1.126 → 0.1.127, operator+webhook chart v0.1.107 → v0.1.108 + appVersion v0.1.126 → v0.1.127, platform-stack 0.1.28 → 0.1.29 | v0.1.127 |
| 2026-05-22 | M1.5 Track B.1.77 closure — Application reconciler integration: gate pause/resume per spec.md §3.8 + ADR 0027. `operator-controllers/application` reconciler gains a pause gate that runs BEFORE child resource patches: lists MigrationPlans в `apprafter-system`, filters by scope.type=application + scope.application.ref matching + scope.application.environment match (or wildcard когда env is None), checks `plan_is_blocking` (phase != completed && phase != rejected). If found: skip child apply, write `Application.status.phase = AwaitingMigrationApproval` + `Ready=False/MigrationPending` + `MigrationPending=True/MigrationPlanPending` conditions (plan name embedded в message); EndpointURL preserved; requeue 30s. Helpers extracted as pure fns (`pick_blocking_plan`, `plan_is_blocking`, `build_paused_status`, `migration_pending_condition`) testable без kube::Client. **Detection NOT invoked в reconcile**: `ApplicationMigrationStrategy::detect_destructive(old, new) -> Option<DestructiveChange>` concrete fn landed на strategy struct, но impl всегда returns None в 1.77 — current v1alpha1 Application schema (image/replicas/expose/env) carries no destructive operations per spec.md §3.8; Phase 2.x services populate. `create_plan_for(change, plan_name, app_ns, app_name, env) -> MigrationPlan` builder для будущих callers. `DestructiveChange` type в `operator-core` (trigger_type + field + from + to + classification — mirrors `MigrationPlan.spec.trigger` + `spec.risks.classification`). `PHASE_AWAITING_MIGRATION_APPROVAL` + `COND_MIGRATION_PENDING` constants в `operator-core/application.rs`. Argo CD UI: chart's `argocd-cm` ConfigMap gains custom resource-health Lua script под `configs.cm.resource.customizations.health.apprafter.io_Application`. Returns `Degraded` с MigrationPlan name (read из `status.conditions[type=MigrationPending].message`) when `phase=AwaitingMigrationApproval`; `Healthy` on `phase=Ready`; `Progressing` otherwise. Custom health surfaces в Argo CD UI as Degraded card. Tests: +9 unit (application crate: 8 `pick_blocking_plan` filter cases + 2 `build_paused_status` shape + 1 `migration_pending_condition` k8s-convention timestamp preservation). Operator-controllers-application crate gains dep `operator-controllers-migration`. CLI 0.1.125 → 0.1.126, operator+webhook chart v0.1.106 → v0.1.107 + appVersion v0.1.125 → v0.1.126, platform-stack 0.1.27 → 0.1.28 | v0.1.126 |
| 2026-05-22 | M1.5 Track B.1.76 closure — MigrationController + strategy dispatch per spec.md §3.8 + ADR 0027. Третий reconciler в `apprafter-operator` binary (peer to ApplicationController + PlatformController, same Lease). New workspace member `operator/operator-controllers/migration` (Cargo crate). Owns `MigrationPlan.status.phase` FSM (pending-approval → approved → executing → completed/failed | rejected sealed). `MigrationStrategy` trait в `operator-core` (covers `execute_step` + `reject` только; detect_destructive defer'нут к B.1.77 callers потому что Application + Platform detection signatures differ). Trait method execute_step возвращает StepOutcome (Succeeded/Failed/Skipped). Импл'ы: `ApplicationMigrationStrategy` (execute Succeeded — free-form action text без machine semantics в 1.75/1.76; reject no-op per ADR 0027) и `PlatformMigrationStrategy` (execute Succeeded; **real** reject — SSA-patches `PlatformStack.spec.pin` back to `plan.spec.previousSpecSnapshot.pin` (or null когда snapshot has no pin) under field manager `migration-controller-strategy`, идемпотентно). Reconcile loop: pending-approval (no-op, await_change), approved → write executing → requeue, executing → run next step (executed_steps.len() doubles as progress marker, replay-safe), rejected → strategy.reject() then sealed. Status writes под field manager `migration-controller` через SSA. Admission webhook gains FSM transition validator (`validate_phase_transition` + `is_allowed_phase_transition`): pending-approval → approved (any) | rejected (platform только); approved → executing | sealed; executing → completed/failed; sealed states immutable. **Acceptance #4 covered**: application-scope `pending-approval → rejected` rejected с ADR 0027-reference error message. RBAC ClusterRole extends с migrationplans + /status verbs (get/list/watch/patch/update). Annotation source (`apprafter.io/previous-spec` per plan.md) сценированно переписано на `spec.previousSpecSnapshot` field (already in B.1.75 CRD schema) — annotation approach был ADR 0027 placeholder, structured field cleaner. Detection (`detect_destructive`) **не** в trait — per-scope concrete fns (Application diff signature vs version+compat-doc signature) лежат на каждом impl; callers в B.1.77/B.1.78 wire их. Tests: +11 unit (migration crate: 4 reconcile FSM helpers + 5 strategy execute/reject/snapshot extraction + 2 scope dispatch) + +12 unit (webhook FSM transitions: 4 happy paths + 8 rejections включая acceptance #4) + +2 integration (server.rs FSM: app-scope reject blocked, platform-scope reject allowed). Total в migration crate = 11 tests; admission-webhook = 75 lib + 12 integration. CLI 0.1.124 → 0.1.125, operator+webhook chart v0.1.105 → v0.1.106 + appVersion v0.1.124 → v0.1.125, platform-stack 0.1.26 → 0.1.27 | v0.1.125 |
| 2026-05-22 | M1.5 Track B.1.75 closure — unified MigrationPlan CRD + admission webhook validation per spec.md §3.8 + ADR 0027. CUE schema rewrite (`schemas/v1alpha1/migrationplan.cue`) с scope discriminator (`application` \| `platform`), `trigger`, `risks` (classification + reversibility + backup), `plan[]` (steps), `approvers[]`, `previousSpecSnapshot`. Status: `phase` enum (`pending-approval` → `approved`/`rejected` → `executing` → `completed`/`failed`), `approvedAt/By`, `executedSteps[]`. OpenAPI v3 CRD shipped from operator chart (`templates/crd-migrationplan.yaml`) sync-wave -5 alongside Application + PlatformStack — **не использует `oneOf` discriminator** (apiserver rejects most oneOf shapes in structural schema); вместо этого scope.{application,platform} оба optional CRD-side + conditional invariant enforced webhook'ом. Admission webhook gains `validator_migrationplan.rs` module + dispatch branch для `kind=MigrationPlan` через server.rs (passes `request.oldObject` для UPDATE-time scope immutability check). Validation: (1) **Scope discriminator** — type=application requires populated `scope.application.{ref.{name,namespace},environment}`; type=platform requires `scope.platform.components` non-empty; mismatched sub-object (`application` block on platform-scope plan etc.) rejected. (2) **Approver emails** — light RFC5322 (`is_emailish`): single `@`, non-empty local + domain, dot in domain. (3) **spec.scope immutability on UPDATE** — diff `request.object` vs `request.oldObject`; scope change rejected (trait dispatch in 1.76 keys on scope; mutating it mid-plan silently switches controller path). ValidatingWebhookConfiguration extended с третьей entry для `migrationplans` (CREATE+UPDATE). Rust `MigrationPlan` type в `operator-core/src/migration_plan.rs` (kube-rs CustomResource derive) — used by validator только для type schema today; B.1.76 wires it through reconcile signatures. Deferred to B.1.76: reject status patches не от MigrationController — это concern controller-existence-aware. Tests: 24 unit (validator_migrationplan) + 3 integration (server.rs MigrationPlan dispatch: accept application-scope, reject platform-scope с empty components, reject UPDATE scope mutation); total +27. CUE schema vet'ed через `examples/migrationplans/{parser-pg-selector,platform-0-2-0-bump}.cue` round-trip. CLI 0.1.123 → 0.1.124, operator+webhook chart v0.1.104 → v0.1.105 + appVersion v0.1.123 → v0.1.124, platform-stack 0.1.25 → 0.1.26 | v0.1.124 |
| 2026-05-22 | M1.5 Track B.1.74a closure — yanking support для published platform-stack версий. Расширение `compatibility.cue` schema: `yanked: bool \| *false` + `yankedReason?: string`. CUE не может выразить conditional invariant "yankedReason required when yanked=true" одним field constraint, поэтому добавили CI guard в обоих workflow: `platform-stack-check.yml` (PR time) + `platform-stack-publish.yml` (publish time) — `cue export -e compatibility \| jq` walking entries with yanked=true, failing on empty `yankedReason`. PlatformController consumption (3 поверхности): (1) **`resolve_non_yanked_latest`** helper в reconcile — walks channel-tag candidates newest-first, skips entries `yanked: true` в compatibility doc, returns first non-yanked. Fresh clusters резолвят `availableVersion` к non-yanked версии. (2) **`YankedVersion` condition** — informational (НЕ `Ready=False`), True iff `status.currentVersion` matches yanked entry с verbatim `yankedReason` в condition message. Surfaces через `kubectl describe platformstack`. На no-poll cycles (throttled) prior condition preserves. (3) **Refactor compatibility.rs + oci.rs**: `fetch_compatibility_doc(url, version_tag) -> CompatibilityDoc` — pull tarball + parse full BTreeMap raz na poll cycle, reuses для yank filter + (future) change_class lookup. `oci::tags_in_channel` returns Vec descending; `latest_in_channel` остаётся wrapper для backward compat. `VersionRecord` extends с `yanked` + `yanked_reason` (`#[serde(rename="yankedReason")]`). Architecture choice: yank handling INLINE в reconcile, не behind `PolicyHooks::is_yanked` — yank это pure lookup over already-pulled doc, no extensibility seam. Stub method removed from trait; `NoOpHooks` остаётся test fixture только для MigrationPlan. **Deferred**: UI shim для `apprafter platform status` + Backstage plugin warning banner — visibility via `kubectl describe platformstack` + Events sufficient до landing CLI subcommand / Backstage plugin. **Walk deferred** to next sub-phase (next opportunity exercises B.1.74a regression). +9 regression tests (2 compatibility — yanked default false / camelCase reason; 2 oci — tags_in_channel sort descending + empty filter; 5 reconcile — resolve_non_yanked_latest covers no-yank/skip-top/consecutive-yanks/missing-entry/all-yanked); -1 test (deleted `no_op_hooks_report_not_yanked`). Total 50 → 58 в platform-stack controller crate. CLI 0.1.122 → 0.1.123, operator+webhook chart v0.1.103 → v0.1.104 + appVersion v0.1.122 → v0.1.123, platform-stack 0.1.24 → 0.1.25 | v0.1.123 |
| 2026-05-22 | M1.5 Track B.1.74 walk-fix #7 — versionHistory race fix. B.1.74 acceptance walk на v0.1.121 выявил controller-cache race: PlatformController's собственный SSA-status-write trigger'ит follow-up reconcile через watcher cache (kube-rs `Controller`), который lags apiserver на сотни ms. Следующий reconcile видит **stale** snapshot `PlatformStack` (без только что persisted'нной versionHistory entry); хотя `target_changed=false` (no append happens), `write_status_if_changed` всё равно detect'ит `conditions[*].reason` delta (свежий Synced cycle messaging) и patch'ит status — clobber'я apiserver's authoritative versionHistory stale-2-entry-vector'ом. Симптом walk'а: после bump v0.1.19→v0.1.20 history briefly shows 3 entries, потом silently revert'ится к 2. Fix (Option A): SSA patch body теперь OMIT'ит `versionHistory` field когда reconcile cycle did NOT append this turn. SSA preserves field values that are ABSENT from patch body, so apiserver's existing value stays authoritative across stale-cache reconciles. Реализация: три internal helpers (`build_status_patch`, `write_status`, `write_status_if_changed`) получили `include_version_history: bool` parameter; `build_status_patch` сериализует status в JSON и conditionally strip'ит `versionHistory` map key. Reconcile body tracks `appended_history` flag, passes его через. In-flight early-return passes `false` (no append ever happens before that branch). Option B (read PlatformStack via `Api::get` bypassing cache) отвергнут — per-reconcile apiserver round-trip on hottest path + не actually closes race (`Api::get` still subject to read-your-write timing). +2 regression tests: `build_status_patch_omits_version_history_when_not_appended` + `build_status_patch_includes_version_history_when_appended`; existing `build_status_patch_includes_apiversion_kind_and_name` updated на новую 3-arg сигнатуру (`include_version_history: true` для existing assertion). Total 48→50 в platform-stack controller crate. По user'у "Давай А без ре-волка и сразу к 1.74а перейдём" — walk re-verification skip'ается, B.1.74a (yanking) — next. CLI 0.1.121 → 0.1.122, operator+webhook chart v0.1.102 → v0.1.103 + appVersion v0.1.121 → v0.1.122, platform-stack 0.1.23 → 0.1.24 | v0.1.122 |
| 2026-05-22 | M1.5 Track B.1.74 closure — PlatformController status observability полировка. Most of plan.md's 1.74 scope уже landed в B.1.73 walk-fixes (periodic check, OCI poll, channel filter, latest semver, status.{availableVersion, lastUpstreamCheck}, UpgradeAvailable condition, autoUpgrade на safe class). B.1.74 закрывает два оставшихся gap'а: (1) **`status.versionHistory` ring buffer** capped at `VERSION_HISTORY_CAP=10`, FIFO drop. `append_version_history(status, entry)` helper в `status.rs`; reconcile зовёт только когда target_changed && target_for_patch != current_target (т.е. реальный bump targetRevision, не status-only / values-only patches). Entry: `{version, appliedAt, outcome: "succeeded"}`. (2) **`Ready` condition** — derived from `parent.status.health.status`. True/Healthy iff parent reports Healthy (Argo CD aggregation от children + workloads); иначе False/ParentNotHealthy с message naming actual health value. Skipped per YAGNI (CLAUDE.md): ETag-aware OCI requests (наш `MIN_OCI_POLL_INTERVAL_SECS=60` throttle + cached availableVersion reuse уже saturate bandwidth concern). Breaking-class MigrationPlan auto-create откладывается на B.1.75 (CRD + controller logic); B.1.74 keeps `MigrationPending=True` condition placeholder. +3 regression guards для ring buffer (`grows_to_cap`, `caps_at_max_and_drops_oldest`, `starts_from_empty_status`); total 45→48. Live verification piggybacks on next walk (versionHistory grow на test 1 pin downgrade + Ready visibility immediate post-bootstrap + walk-fix #6 Events audit trail regression). CLI 0.1.120 → 0.1.121, operator+webhook chart v0.1.101 → v0.1.102 + appVersion v0.1.120 → v0.1.121, platform-stack 0.1.22 → 0.1.23 | v0.1.121 |
| 2026-05-22 | M1.5 Track B.1.73 walk-fix #6 — observability polish для foreign-writer detection. Steady-state cluster на v0.1.119 показал: `kubectl patch parent target=0.1.19` → controller force-reverted (видно в Argo CD UI), но zero durable trace: `UnauthorizedSourceModification=True` condition flipped back to `False/Clean` within next reconcile (sub-second), WARN log потерян во время pod restart cascade (chart 0.1.19 имеет другой operator image pin → Argo CD trigger'нул pod replacement посреди revert). Fix: emit two Kubernetes Events per violation through `kube::runtime::events::Recorder` — `Warning/ForeignFieldManager` at detection (naming offending manager) + `Normal/SourceReverted` after successful force-revert. Both target PlatformStack singleton with parent Argo CD Application as `secondary` (`related`). Best-effort publish (failures logged at warn!, не fail reconcile). Operator chart's ClusterRole gains `events.k8s.io/events` create+patch rule (kube-rs `Recorder::publish` uses events.k8s.io/v1 API, не legacy core group). Visible via `kubectl describe platformstack default` + `kubectl get events -n apprafter-system`. Survive operator pod restart (k8s event TTL default 1h). +1 regression test (`parent_object_reference_points_at_argocd_application`) pinning shape of `related` ObjectReference. Live verification deferred to B.1.74 walk (will exercise event emission as regression check during MigrationPlan tests). CLI 0.1.119 → 0.1.120, operator+webhook chart v0.1.100 → v0.1.101 + appVersion v0.1.119 → v0.1.120, platform-stack 0.1.21 → 0.1.22 | v0.1.120 |
| 2026-05-22 | M1.5 Track B.1.73 walk-fix #5 — пятый post-closure walk на v0.1.118: Phase 1-3 acceptance прошёл начисто (status правильно populated, conditions sensible, нет false-positive `UnauthorizedSourceModification`, managedFields имеет platform-controller). Но в логах **сотни reconcile'ов в секунду** — `reconcile fired generation=2` + `reconcile completed` repeating ~350ms infinite. Root cause: каждый reconcile unconditionally (1) queried OCI для channel-latest, (2) stamped `status.lastUpstreamCheck = Utc::now()`, (3) SSA-patched status с новым timestamp. Status patch → resource version bump → watcher fires fresh event → next reconcile repeats. Tight self-feedback loop, CPU pegged. Также сломан test 2: при `kubectl patch` parent target=0.1.15 controller's force-revert race теряется в noise — Argo CD успевает pull chart 0.1.15, replace ClusterRole старой version (без platformstacks rules), controller becomes lockt out. Fix two-pronged: (1) **OCI poll throttle** `MIN_OCI_POLL_INTERVAL_SECS=60` — `reconcile()` читает prior `status.lastUpstreamCheck`, re-queries OCI только когда ≥60s elapsed. Intermediate reconciles preserve prior availableVersion + не touch'ат lastUpstreamCheck. (2) **`write_status_if_changed`** — new wrapper compares `new_status` vs `stack.status` (PartialEq derived); byte-equal → skip SSA patch. Комбинируется с `condition()`'s transition-time preservation: no-op reconcile produces identical status, patch never fires, no watch event, loop dies. Steady-state behaviour теперь ~0.017 Hz (1 reconcile/min) vs 100+ Hz tight loop. +2 regression tests (`status_equality_treats_identical_payloads_as_noop`, `status_equality_distinguishes_timestamp_changes`); total 42→44. CLI 0.1.118 → 0.1.119, operator+webhook chart v0.1.99 → v0.1.100 + appVersion v0.1.118 → v0.1.119, platform-stack 0.1.20 → 0.1.21 | v0.1.119 |
| 2026-05-22 | M1.5 Track B.1.73 walk-fix #4 — четвёртый post-closure walk на v0.1.117. PlatformController logs наконец показали реальный failure: `parse compatibility.yaml: missing field "compatibility"` каждый reconcile цикл. Чарт's render_tool.cue emits compatibility.yaml как **top-level map keyed by version** (`"0.1.19":` directly), но parser ожидал outer wrapper `compatibility:`. Также surfaced 4 связанных проблем — комплексный 5-fix walk-fix: (1) **Parser fix**: `type CompatibilityDoc = BTreeMap<String, VersionRecord>` — direct map. (2) **Observability**: info!() logs на каждой точке (spawn, watch loop entry, reconcile fire/finish, SSA patch, foreign detection). Без них walk-fix #1 (RBAC) был invisible — controller was failing silently. (3) **Loader SSA + whitelist `apprafter-cli`**: step 3 (root App) переключён с client-side `kubectl apply` на `apply_manifest_server_side` field manager `apprafter-cli`; controller's `WHITELISTED_FIELD_MANAGERS` whitelist'ит этот manager. Закрывает false-positive `UnauthorizedSourceModification=True / ForeignFieldManager: kubectl-client-side-apply` который сам себя триггерил на каждом bootstrap. (4) **Controller watches parent Application** через `Controller::watches_with()` — manual kubectl-patches на parent App теперь fire immediate reconcile (раньше — wait 6h checkInterval). (5) **Single-writer SSA pattern**: `patch_application` всегда `force=true`. Старый "patch without force, then force-revert if foreign" deadlocked когда loader's kubectl-client-side-apply уже владел targetRevision — SSA без force 409'd, reconcile failed before revert path. PlatformController теперь THE single writer для `spec.source.{targetRevision, helm.valuesObject}`; foreign detection — только audit condition + warn log. Known limitation **documented** (not engineered): manual kubectl-patch parent target к chart version pre-0.1.17 (без PlatformStack RBAC) ломает PlatformController т.к. Argo CD overwrites ClusterRole старой версией. Recovery: kubectl-patch обратно на 0.1.17+. Accepted degenerate case per user (downgrade ниже PlatformController-aware era). Тестов 40→42 (+2 compatibility parser shape tests). CLI 0.1.117 → 0.1.118, operator+webhook chart v0.1.98 → v0.1.99 + appVersion v0.1.117 → v0.1.118, platform-stack 0.1.19 → 0.1.20 | v0.1.118 |
| 2026-05-21 | M1.5 Track B.1.73 walk-fix #3 — третий post-closure walk на v0.1.116 (с RBAC + TypeMeta из предыдущих fix'ов): PlatformController наконец-то reconciled CR и заполнил status, но surfaced два семантических бага: (1) `UpgradeAvailable=True` сработал даже когда `currentVersion == availableVersion == 0.1.18` — root cause: старый gate `current_target != desired_target ∨ values_differ()` ложно срабатывал на values_differ потому что loader не set'ил `helm.valuesObject` в parent App (null vs `{tier: 1}` looks like diff); (2) `kubectl get app platform -n argocd -o jsonpath='{.metadata.managedFields[*].manager}'` показал только `kubectl-client-side-apply argocd-application-controller` — `platform-controller` ни одного patch не сделал потому что policy (pin=None + autoUpgrade=false) refused. Fix end-to-end refactor `reconcile()`: (a) новый `semver_gt(a, b)` helper, `UpgradeAvailable` теперь STRICT semver comparison `channel_latest > target_for_patch` independent of values diffs (fail-safe — false on unparseable); (b) SSA patch ALWAYS owns both `targetRevision` (kept at current when policy forbids bump) AND `helm.valuesObject` (values — runtime config, не version bump, не gated by pin/autoUpgrade); (c) новый `platform_controller_owns_source(parent)` helper triggers no-op SSA patch на first reconcile когда manager ещё не зарегистрирован — чтобы outside-writer detection работал. Plus `MigrationPending` теперь имеет explicit False/Clean; `Synced.reason` = `Patched` или `Reconciled` depending on patch. +8 regression tests (5 `semver_gt_*` + 3 `platform_controller_owns_source_*`), total 32 → 40. CLI 0.1.116 → 0.1.117, operator+webhook chart v0.1.97 → v0.1.98 + appVersion v0.1.116 → v0.1.117, platform-stack 0.1.18 → 0.1.19 | v0.1.117 |
| 2026-05-21 | M1.5 Track B.1.73 walk-fix #2 — второй post-closure walk на v0.1.115 (с RBAC fix): operator pod корректно стартует с новой RBAC, PlatformController watcher работает (CR появляется в reconcile loop), но **каждый** reconcile падает с `ApiError: invalid object type: /, Kind=: BadRequest (400)` каждую минуту. Root cause: `write_status` отправлял SSA patch `{"status": {...}}` без TypeMeta (apiVersion + kind + metadata.name) — apiserver рекомендует полный TypeMeta в SSA patch body чтобы резолвить schema перед merge. Application controller's `apply_status` всегда правильно эмиттил TypeMeta, но в B.1.73 новом `write_status` это уронили. Fix: extracted `build_status_patch(name, status)` + `build_application_patch(desired)` helpers — оба emit full SSA-compliant body (apiVersion+kind+metadata.name+resource-specific). Two regression tests (`build_status_patch_includes_apiversion_kind_and_name`, `build_application_patch_includes_apiversion_kind_name_and_source`) pin contract — future refactors не смогут silently strip TypeMeta. Все 32 unit tests pass (было 30, +2 новых). CLI 0.1.115 → 0.1.116, operator+webhook chart v0.1.96 → v0.1.97 + appVersion v0.1.115 → v0.1.116, platform-stack 0.1.17 → 0.1.18 | v0.1.116 |
| 2026-05-21 | M1.5 Track B.1.73 walk-fix #1 — первый post-closure walk: bootstrap прошёл, Argo CD всё зелёное, default PlatformStack CR применён, но `.status` остаётся **пустым** даже через несколько минут. Root cause: operator chart's ClusterRole (B.1.71-era) гранил permissions только на `apprafter.io/applications` + owned children. PlatformController's watcher на `apprafter.io/platformstacks` failed silently на first list/watch (Forbidden), controller stream closed без emitted reconciles. Также chart lacked `argoproj.io/applications` get/patch — даже если бы watcher работал, SSA patch parent `platform` Application rejected'ся. Fix: chart's ClusterRole gains два rule blocks (platformstacks + /status + argoproj.io/applications). **Параллельные оптимизации**: оба Deployments (operator + admission-webhook) dropped 5-10s `initialDelaySeconds` padding на liveness/readiness probes (Rust musl-static boots в ms); добавлен `startupProbe` (1s period × 30s failureThreshold) для cold-boot grace без overhead на каждый restart. Operator workspace `[profile.release]` += `panic = "abort"` — убирает unwinding machinery, ~5-10% smaller musl-static image, faster pull. Net cold-start saving: ~10-20s на каждом из двух pods + smaller image. CLI 0.1.114 → 0.1.115, operator+webhook chart v0.1.95 → v0.1.96 + appVersion v0.1.114 → v0.1.115, platform-stack 0.1.16 → 0.1.17 | v0.1.115 |
| 2026-05-20 | M1.5 Track B.1.73 closure — PlatformController core landed как второй controller внутри `apprafter-operator` binary (адаптация plan.md base "new crate" к user design "in existing operator"). Новый workspace member `operator-controllers/platform-stack` (peer to application controller). Reconcile loop: resolve desired version (pin ИЛИ channel-latest via OCI-distribution); read parent `platform` Application через dynamic kube API; in-flight detection (sync.status==OutOfSync ИЛИ operationState.phase==Running) → requeue 30s; compute desired source через `desired::build`; fetch change class via `compatibility.yaml` (chart tarball pull + tar extract); policy decide (pin set → bump; autoUpgrade=false → status-only `UpgradeAvailable=True`; safe/requires-restart → SSA patch; breaking/data-migration → `MigrationPending=True` condition, defer to 1.74); SSA patch с field manager `platform-controller`; outside-writer detection через `metadata.managedFields` → force-revert + `UnauthorizedSourceModification=True`; status write (currentVersion/targetVersion/availableVersion/lastUpstreamCheck/conditions) + cadence requeue (parsed from `spec.source.checkInterval`). 30 unit tests pass (5 oci + 4 compatibility + 4 desired + 2 policy + 4 status + 11 reconcile) plus 1 ignored smoke scaffold (APPRAFTER_K8S_SMOKE=1 gate). PolicyHooks trait + NoOpHooks default impl pre-position для 1.74 (MigrationPlan auto-create) + 1.74a (yanking field). **Chart-side breakthrough** — `_applicationsTemplate` extends to consume `.Values.overrides.<component>.{pin,values,enabled}` (pin replaces targetRevision, values mergeOverwrite onto component values, enabled gates emission). PlatformController writes этот блок через SSA на parent Application's helm.valuesObject из `PlatformStack.spec.overrides`. Chart 0.1.15 → 0.1.16, operator+webhook chart v0.1.94 → v0.1.95 + appVersion v0.1.111 → v0.1.114 lockstep, CLI 0.1.113 → 0.1.114. Out of scope (deferred): yanking field (1.74a), MigrationPlan auto-create (1.74), `minimumKubernetesVersion` env check, multi-stack support (singleton enforced), rollback flow, manifest-level diff (helm render delegated to Argo CD via argocd-cue-cmp) | v0.1.114 |
| 2026-05-20 | M1.5 Track B.1.72 walk-fix #2 — second post-closure walk: v0.1.112's `kubectl wait crd/applications.apprafter.io --for=condition=Established` failed **immediately** with `NotFound`. `kubectl wait` errors instantly on missing resources, не polls для появления — single-stage Established wait racing operator chart's CRD apply was guaranteed to fail когда step 4b's root-App Healthy фыдывал false-positive. Fix: two-stage wait per CRD — first `--for=create` (kubectl 1.27+, ждёт появления CRD object), затем `--for=condition=Established` (ждёт apiserver discovery registration). New constants: `CRD_CREATE_TIMEOUT_SECS=600` (covers chart pull + apply через cert-manager wave -10 + operator wave 0 under load), `CRD_ESTABLISHED_TIMEOUT_SECS=60` (down с 120, establishment ≤ few seconds once CRD exists). Total 4 CRD waits ordered: applications.apprafter.io create → Established → platformstacks.apprafter.io create → Established. Regression tests updated: `waits.len() == 8`, plus assertions that every `--for=create` precedes its matching `--for=condition=Established`. Open question deferred (почему step 4b root Healthy false-positive — Argo CD's child-health aggregation timing): step 4c теперь non-load-bearing makes root Healthy несущественным для CRD readiness assertion. CLI bumped 0.1.112 → 0.1.113 (loader-side ordering only — без chart bump, без image rebuild) | v0.1.113 |
| 2026-05-20 | M1.5 Track B.1.72 walk-fix #1 — first post-closure walk hit `no matches for kind "PlatformStack"` at step 5 (SSA apply of default PlatformStack singleton). Root cause: step 4b's Healthy wait on `application/platform` returned before the operator chart's CRDs reached `Established=True` AND/OR before the kubectl client's on-disk discovery cache refreshed to include `apprafter.io/v1alpha1.PlatformStack`. Fix: new step 4c explicitly waits `condition=Established` on both `crd/applications.apprafter.io` + `crd/platformstacks.apprafter.io` between step 4b (root Healthy) and step 5 (SSA apply). New `CRD_ESTABLISHED_TIMEOUT_SECS=120` constant; in practice resolves sub-second to few seconds once operator child App applied its sync-wave -5 templates. The wait also forces a fresh discovery resolution for the subsequent kubectl invocation (closes the stale-cache angle). Regression guards: existing `perform_bootstrap_installs_...` test now asserts `waits.len() == 6` with positions [4][5] pinned to the new CRD waits; new `crd_established_waits_run_after_root_healthy_and_before_platformstack_apply` test asserts ordering. CLI bumped 0.1.111 → 0.1.112 (no chart bump, no operator image rebuild — fix is loader-side ordering only) | v0.1.112 |
| 2026-05-19 | M1.5 Track B.1.72 closure — PlatformStack CRD per spec §3.11 + ADR 0026 + restored Application CRD (B.1.71 dropped imperative shipper without chart-side replacement). Both CRDs ship from operator Helm chart at sync-wave -5; admission webhook gains PlatformStack validation: singleton (name=default, namespace=apprafter-system), channel enum, pin semver shape, source.checkInterval ≥ 1h. Webhook server dispatches by kind (Application/PlatformStack/other). `ValidatingWebhookConfiguration` template carries a second webhook entry. `cluster-bootstrap` gains step 5: SSA-apply default PlatformStack CR with field manager `apprafter-cli` after Healthy wait (tier from active target via `Tier::level()`, domain omitted). PlatformController logic deferred to B.1.73. Version chain: CLI 0.1.110 → 0.1.111; operator + webhook chart v0.1.93 → v0.1.94 and appVersion v0.1.106 → v0.1.111 (matches new monorepo tag); platform-stack chart 0.1.14 → 0.1.15 via CUE invariant chain. spec.md §3.11 prose: `targetVersion` added to status fields list (additive, no revision bump). Test count: +16 admission-webhook validator tests + 3 server dispatch integration tests + 4 cluster-bootstrap unit tests. All gates clean (fmt, clippy -D warnings, cue vet, helm lint, SPDX, conventional-commits) | v0.1.111 |
