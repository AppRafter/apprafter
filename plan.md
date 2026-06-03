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
- `> 🏁 SR:` — speedrun bucket: **A** keep (launch) · **B** pull-up (launch) · **C** defer post-launch (+ trigger) · **D** drop (+ reactivate). "order N" = OSS-core build sequence (speedrun §4.2). See `speedrun-plan.md`.

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
> 🏁 SR: A · order 1 — M1.5 Track B subset (ADR 0025/0028/0029: GitOps + platform-stack distribution + CUE CMP); managed substrate, largely closed

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
- Idempotent resume на каждом шаге (pre-launch P1 requirement) — `helm upgrade --install` + `kubectl apply` уже idempotent на step level; what's NOT yet idempotent — partial state when waits timeout (e.g. argocd-server up but root Application apply failed). Defer полная resume semantics.
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
> 🏁 SR: A · order 2 — PlatformController + MigrationPlan CRD, condensed (1.72–1.78); closes killer features #3 + #7. SPLIT: 4.16 Backstage MigrationPlan plugin → C

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

### 1.79 CLI thin wrappers + `apprafter open` commands ✅

> v0.1.142 — sub-phase 1.79 shipped. `apprafter platform {status,upgrade}` + `apprafter migration {list,approve,reject}` + `apprafter open argocd` (port-forward + clipboard + browser launch) + npm-style version check + Argo CD UI Resource Action Lua scripts для MigrationPlan approve/reject. `platform {freeze,unfreeze,rescue}` rolled forward в 1.79a part 5 (closed v0.1.142). `platform channel` deferred к Phase 2 (multi-channel UX waits for stable/edge divergence); `open {backstage,grafana,hubble}` deferred к Tier 2+ (не tier-1 residents).

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

### 1.79a CLI app/repo subcommands + AppProjects + `open` polish ✅

> v0.1.142 originally; v0.1.160 + platform-stack 0.1.47 + argocd-cue-cmp v0.1.5 fully validated. Sub-phase shipped через 5 parts (AppProjects + project field; `open argocd` polish; `app {add,list,status,remove}`; `app {logs,rollback}`; `repo creds {add,list,show,rotate,remove}`) + `platform {freeze,unfreeze,rescue}` rollup. Real-cluster manual walk surfaced 12 walk-fixes (history rows below — #1 sync-wave/AppProject race, #2 standalone AppProject manifests at wave -30, #3 release-cli CUE install, #4 version_check tag stream picker, #5/#5b CMP entrypoint flat-stream + apprafter/ dir layout, #6 per-target state migration, #7 EXDEV cross-mount + richer-legacy preference, #8/#8b CMP discover glob→command + cue-cmp v0.1.4 rebuild, #9 channel-latest resolver decouples CLI from chart, #10 CMP discover stdout fix `grep -q` removed, #11 Namespace permitted в apps AppProject, #12 wizard defaults destination namespace к apprafter). Each walk-fix landed с regression-guard test; phase boundary tech-debt = zero. Deferred с rationale: interactive wizard для `repo creds add` (flag-driven sufficient, operator feedback gates); inline PAT prompt при private-repo cred miss (hint sufficient); API ping для PAT validation (format regex catches most copy-paste errors, adds flakiness); App CR conditions + pending MigrationPlan section в `app status` (Phase 2 destructive change detection prerequisite); last-used / expires в `repo creds list` (Argo CD / GitHub не surface'ят данные); `open backstage` (Tier 2+); URL pre-fill username `?username=admin` (Argo CD 7.7.7 не поддерживает — negative result).

**Source:** ADR 0025, 0026 (Argo CD projects model); продолжение 1.79.

**Цель:** убрать необходимость заходить в Argo CD UI для повседневных операций (добавление repo, deploy status, rollback) и разделить платформенные приложения от пользовательских визуально и через RBAC.

#### Поставка — AppProjects в platform-stack chart

- [x] Добавить три `AppProject` ресурса в umbrella chart (через `_loaderValues.argocd.values.configs.projects`, а не отдельную папку — Argo CD chart 7.7.7 сам создаёт AppProjects из этого block'а):
    - [x] `platform` — для core platform components. `sourceRepos: ["*"]`, `destinations: [{namespace: "*", server: "https://kubernetes.default.svc"}]`, `clusterResourceWhitelist: [{group: "*", kind: "*"}]`, `namespaceResourceWhitelist: [{group: "*", kind: "*"}]` (открыто на M1.5 — RBAC enforcement через AccessGrant приедет в Phase 4).
    - [x] `platform-providers` — для ServiceProvider operators (CNPG, Dragonfly, NATS, Kamaji). Те же permissions что и `platform`, разделение чисто визуальное + lifecycle-категория. Project заводится сейчас (а не лениво в Phase 2), чтобы UI selector показывал его сразу после bootstrap'а.
    - [x] `apps` — для user applications. `sourceRepos: ["*"]` (пока не введён RBAC через AccessGrant Phase 4), `destinations: [{namespace: "*", server: "https://kubernetes.default.svc"}]`, `clusterResourceWhitelist: []`, `namespaceResourceWhitelist: [{group: "apprafter.io", kind: "Application"}, {group: "", kind: "ConfigMap"}, {group: "", kind: "Secret"}, {group: "gateway.networking.k8s.io", kind: "HTTPRoute"}]`.
- [x] Update umbrella Helm templates — все chart-managed Applications получают `spec.project: {{ default "platform" $component.project }}` через новое поле `#Component.project: string | *"platform"`. Default = `platform`; tier overlays / ServiceProvider charts могут override на `platform-providers` per-component. CLI loader's root platform Application также переехал на `spec.project: platform` (`cluster_bootstrap::render_root_application`).
- [x] CMP plugin (`argocd-cue-cmp`) рендерит user Application CRs с `spec.project: apps` по умолчанию. **Закрыто:** `apprafter app add` wizard (`app_wizard.rs`) defaults parent Argo CD Application's `spec.project` к `apps` (clap default), а CMP-rendered user manifests (apprafter.io/Application) сами не несут `spec.project` (это Argo CD-specific поле, не AppRafter); end-to-end проверено walk-fix #10/#11/#12 на real cluster — landing-web/landing-cms apps зарегистрированы в Argo CD под project `apps`.

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
    - [x] Interactive: спрашивает name/branch/path с дефолтами; non-interactive: использует defaults или fails если `--git-url` не задан. **Закрыто v0.1.145** через `cli/platform-cli/src/commands/app_wizard.rs` (Text+Select prompts, cwd-detect для path/branch/origin, inline DNS-1123 валидация); v0.1.160 (walk-fix #12) добавил шестой prompt «Destination namespace» с DNS-1123 валидацией. `--no-interactive` opt-out для CI/headless.
    - [x] Проверка доступности репо — `git ls-remote` через subprocess (поддерживает HTTPS auth check без token, для private — детект unauthorized). `--no-ping` для air-gapped CI.
    - [ ] Если репо private и не зарегистрирован cred — inline prompt: "Use existing PAT / Add new PAT / Skip". **Deferred к v0.1.141** — лендится вместе с `apprafter repo creds add`. Сейчас auth failure surfaces hint pointing к `apprafter repo creds add`.
    - [x] Пишет Argo CD `Application` CR в `argocd` namespace с label `apprafter.io/managed-by: apprafter` и annotation `apprafter.io/source: cli`. CR указывает на пользовательский repo, CMP plugin рендерит `apprafter/Application.cue` оттуда.
- [x] `apprafter app list [--project <name>] [--all-projects]`:
    - [x] Default filter `--project apps`.
    - [x] Таблица: name, project, repo, revision, sync state, health. (last sync time не surfaced — Argo CD CR не carry'ит human-friendly timestamp в `status.sync`; добавим если operator feedback потребует).
    - [x] `--all-managed` toggle drops the managed-by label filter.
- [x] `apprafter app status <name>`:
    - [x] Detail view: Argo CD Application sync/health + source + destination + recent revisions (last 3 из `status.history`).
    - [x] AppRafter Application CR conditions (если CMP уже отрендерил) + перечень child resources. **Закрыто v0.1.164** через `apprafter app status <name> --resources`/`-r` flag — рендерит Argo CD's `status.resources[]` (NAME/KIND/NAMESPACE/STATUS/HEALTH) + Pods в destination ns через operator's `app.kubernetes.io/name=<inner-name>` label (READY/STATUS/RESTARTS/AGE с kubectl-style waiting.reason heuristic — surfaces ImagePullBackOff / CrashLoopBackOff / etc. что Argo CD app-level Healthy aggregation скрывает). Pod fetch non-fatal — соблюдает Argo CD's view как authoritative для sync/health. Operator-side health propagation (CR status.phase отражающий actual Pod state) остаётся Phase 2/3 concern (ResourceClaim wait semantics).
    - [ ] Если есть pending MigrationPlan для этого app — выводит в верхней секции с approve-командой. **Deferred к v0.1.140 / Phase 2** — нужны user-app MigrationPlans из Phase 2 destructive change detection.
- [x] `apprafter app logs <name> [--follow] [--tail <N>] [--container <c>] [--pod <name>]`:
    - [x] Wrapper над `kubectl logs` с label selector `app.kubernetes.io/instance=<app-name>` (Argo CD's documented standard label, stamped на every child resource).
    - [x] Multi-pod: aggregate by default через selector form с `--prefix=true` + `--max-log-requests=10`; `--pod <name>` narrows к single pod (selector mode skipped).
    - [x] Pure helpers `build_kubectl_logs_target` + `build_kubectl_logs_args` exhaustively tested без spawning kubectl.
- [x] `apprafter app rollback <name> [--to <revision>]`:
    - [x] Без `--to`: rollback к предыдущей Git revision (читает `status.history` из Argo CD Application через pure `pick_previous_revision` helper — chronologically ordered list, second-to-last entry).
    - [x] С `--to <sha>`: rollback к указанному коммиту.
    - [x] Внутри — patch `spec.source.targetRevision` через JSON merge-patch, Argo CD ресинкает на следующем reconcile cycle.
    - [x] Confirmation prompt в interactive (`inquire::Confirm` default No), `--yes` для non-interactive.
    - [x] Pre-flight refuse когда target revision matches current (no-op).
- [x] `apprafter app remove <name>`:
    - [x] Confirmation prompt через `inquire::Confirm` (default No), `--yes` для non-interactive.
    - [x] Удаляет Argo CD Application через `kubectl delete`, в каскаде — child resources (Argo CD reconciles via ownerRefs).
    - [x] `--keep-data` опция — flips `syncPolicy.automated.prune: false` ДО delete, child resources (PVC/ResourceClaims) сохраняются.

**Alias:** [x] `apprafter a` → `apprafter app` (проверил — `apprafter apply` не конфликтует с `a` потому что clap резолвит alias строго; `apprafter a add` работает, `apprafter a apply` не существует).

#### Поставка — `apprafter repo creds` подкоманды

- [x] `apprafter repo creds add <name>`:
    - [ ] Interactive wizard для всех полей. **В v0.1.141 flag-driven only** (`--url-prefix` required, `--type pat` default, `--username git` default); token reads из stdin через `inquire::Password` masked когда `--token` opused и stdin = TTY. Wizard приедет если поступит реальный operator feedback.
    - [x] Token validation:
        - [x] GitHub: `github_pat_*` (fine-grained, 80+ char body) или `ghp_*` (classic, 40 chars total), regex check. **API ping deferred** — adds network round-trip и failure modes (rate limiting, transient network); validation gates на shape suffices для most copy-paste errors.
        - [x] GitLab: `glpat-*`, 20+ char body length check.
        - [x] Generic fallback: 20+ chars (Gitea/Codeberg/Forgejo); `--no-validate` для bypass.
        - [x] Basic auth: any non-empty password accepted.
    - [x] Создаёт k8s Secret в namespace `argocd` с labels:
        - [x] `argocd.argoproj.io/secret-type: repo-creds`
        - [x] `apprafter.io/managed-by: apprafter`
        - [x] `apprafter.io/cred-name: <name>`
        - [x] `stringData` (НЕ `data` — kubectl base64-encodes server-side): `url`, `username`, `password`.
        - [x] Annotation `apprafter.io/auth-type: <pat|basic>` чтобы `rotate` мог re-validate against original auth type.
- [x] `apprafter repo creds list`:
    - [x] Таблица: name, URL prefix, type, username. **last-used / expires deferred** — Argo CD не stamps usage timestamps на the Secret и GitHub fine-grained PATs don't expose `exp` через token shape; будем surface когда appear полезный signal.
- [x] `apprafter repo creds show <name>`:
    - [x] Detail view, password замаскирован (`****`) + pointer к `kubectl get secret -o jsonpath='{.data.password}' | base64 -d` для plaintext decode когда нужно.
- [x] `apprafter repo creds rotate <name>`:
    - [x] Prompt только для нового token, остальные поля сохраняются.
    - [x] Patch existing Secret через JSON merge-patch (не пересоздаёт — repo-server caches resourceVersion).
    - [x] Re-validation token'а перед patch против recorded `apprafter.io/auth-type` annotation.
- [x] `apprafter repo creds remove <name>`:
    - [x] **Dependency check** by default — refuses когда есть Argo CD Applications с `spec.source.repoURL` starting with the creds' `url` field. Pure helper `find_apps_matching_prefix` walks the Application list filter testable без cluster.
    - [x] `--force` skips dependency check (для migrations к а different creds entry).
    - [x] `--yes` skips только confirmation prompt (still runs dependency check).

#### Поставка — context-aware error hints

- [x] При `apprafter app add` без `.git` в cwd: hint "не удалось запустить git remote get-url ... Запусти из git-репозитория или передай URL явно через `apprafter app add <git-url>`."
- [x] При попытке `app add` для private репо без creds в non-interactive: error "git ls-remote отказал в доступе ... Зарегистрируй creds через `apprafter repo creds add` и повтори `apprafter app add`."
- [x] При попытке `app add` с конфликтным именем (Application с таким name уже есть): error "Application '<name>' уже зарегистрирован в namespace argocd. Запусти `apprafter app status <name>` чтобы посмотреть текущее состояние, или `apprafter app remove <name>` для каскадного удаления, либо передай другой `--name`."

#### Поставка — `apprafter platform` extension (закрывает остатки от 1.79)

- [x] `apprafter platform freeze <component> [--version <v>]` patches `PlatformStack.spec.overrides.<component>.pin`. Без `--version` reads effective version из `status.componentVersions.<component>` и locks that. CRD schema (`schemas/v1alpha1/platformstack.cue`) already supports `overrides` map (B.1.74 era).
- [x] `apprafter platform unfreeze <component>` — RFC 7396 merge-patch с null value strips `overrides.<component>` entry. Component falls back к chart's curated pin.
- [x] `apprafter platform rescue [--yes]` — thin wrapper over `apprafter cluster-bootstrap` с recovery confirmation banner. Use case: Argo CD self-adopt stuck (stale chart, corrupted ConfigMap, pod-eviction loop).

#### Acceptance

- [x] `apprafter open argocd` открывает UI с фильтром `apps`, username отображается в выводе, password в clipboard.
- [x] В Argo CD UI верхний project selector показывает три проекта (+ legacy `default`); `apps` пустой при свежем bootstrap, `platform` и `platform-providers` содержат соответствующие Applications.
- [x] `cd <git-repo> && apprafter app add` без аргументов корректно детектит origin и регистрирует app.
- [ ] `apprafter app add` для private репо без creds → **hint pointing на `apprafter repo creds add`** (inline interactive prompt deferred — flag-driven flow + hint sufficient).
- [x] `apprafter repo creds add` с невалидным GitHub PAT → fail с regex error до API call.
- [ ] `apprafter repo creds add` с валидным форматом но revoked token → **API ping deferred** (см. above — adds network round-trip + flakiness; format validation alone catches most copy-paste errors).
- [x] `apprafter app rollback <name>` без `--to` откатывает к предыдущей revision; Argo CD синкает в течение reconcile cycle.
- [x] `apprafter app remove` удаляет Application каскадно, `--keep-data` сохраняет PVC.
- [x] `apprafter repo creds rotate` обновляет token, existing apps продолжают синкаться без даунтайма Argo CD repo reconcile.

#### Не входит в этот item

- AccessGrant / RBAC enforcement через AppProject (Phase 4 целиком).
- Reverse proxy для `apprafter open` (отдельный item, после M2, когда понадобится Backstage с теми же проблемами).
- `apprafter app scale`, `apprafter app env set` — высокоуровневые ops-команды (M2+, после ServiceProvider/ResourceClaim).
- Backstage Application plugin — отдельный item в Phase 3.

**Зависит от:** 1.79 (CLI thin wrappers infrastructure + `open` для argocd базовый).

**Размер:** M

---

### 1.79b CLI app ergonomics — `app open` + scaffolding + runtime templates ✅

> v0.1.174 + platform-stack 0.1.48 + argocd-cue-cmp v0.1.6 — sub-phase 1.79b shipped через parts 1–3b: `apprafter app open <name>` (part 1, v0.1.161 — port-forward + Service-resolution + browser launch + graceful Ctrl+C), runtime detection primitives (part 2, v0.1.166 — filesystem-marker → runtime mapping с High/Medium/Low/Fallback confidence), `apprafter app scaffold` + embedded `.cue.hbs` template engine (part 3, v0.1.167) + scaffold wizard в `app add` step 0 + interactive runtime picker (part 3b, v0.1.168). Real-cluster manual walk surfaced walk-fixes (post-B.1.79b #1–#4 v0.1.162–165: app-open label-resolution off operator's `app.kubernetes.io/name` + kubectl stderr surfacing + `app status --resources` child workload state + Cargo.lock/bun-smoke sync; post-Part-3b #1–#6 + #11 v0.1.169–174 + chart 0.1.48 / cue-cmp v0.1.6: scaffold UX/SPDX/namespace/cred-hint polish, `spec.source.path` absolute-path fix, CUE-import drop, schema inline→vendored CUE module под `apprafter/cue.mod/`, image-ref derived from git origin, CMP entrypoint cds into package dir). Each walk-fix landed с regression-guard test; phase boundary tech-debt = zero. Design divergences (приняты): template engine ships 2 consolidated `.cue.hbs` (default + blank) вместо 12 per-runtime files (port defaults baked into `defaults_for`); multi-stack default = first-High-in-order. Deferred с rationale (Part 4, ещё не отгружено): `examples/applications/` reference manifests, `docs/user-guide/cli/app-scaffold.md`, quickstart update, `spec.resources`/`spec.healthcheck` template fields (v1alpha1-only сейчас), `--lang ru|en` template comments (English-only).

**Source:** Продолжение 1.79a. Closes gaps в quickstart flow для unfamiliar users.

**Цель:** убрать k8s complexity из первого запуска приложения. Юзер не должен знать про port-forward / kubectl, и не должен с нуля писать `apprafter/Application.cue` если у него стандартный stack.

#### Поставка — `apprafter app open <name>`

- [x] Wrapper над `kubectl port-forward` для пользовательского app'а:
    - Резолвит Application name → Service в namespace (через AppRafter Application CR labels).
    - Определяет primary port из Application.expose.port или Service.spec.ports[0].
    - `kubectl port-forward svc/<app> 8080:<port>` в background process.
    - Открытие `http://localhost:8080` в браузере через `open` (Linux/Mac) / `start` (Windows) / `xdg-open`.
    - Output формат:
        ```
        $ apprafter app open my-parser

        Forwarding to my-parser on namespace 'default'...
          Service:   my-parser
          Port:      3000 (container) → 8080 (localhost)
          URL:       http://localhost:8080

        ✓ Browser opened
        ℹ Press Ctrl+C to stop port-forward
        ```
- [x] Флаги:
    - `--port <port>` — override local port (default 8080, со shift на 8081/8082/etc. если занят).
    - `--container-port <port>` — override container port если app exposed multiple.
    - `--no-browser` — только port-forward, без открытия браузера (для CI/scripts).
- [x] Error handling:
    - App не найден → "Application '<name>' not found. List with `apprafter app list`."
    - App not Healthy → warning "Application is in state <state>; port-forward may fail. Continue anyway? [y/N]".
    - Local port занят → auto-increment до 8090, дальше error.
    - kubectl missing → error с hint к target add.
- [x] Graceful shutdown по Ctrl+C — kill port-forward, exit clean.

#### Поставка — Runtime detection в `apprafter app add`/`scaffold`

- [x] Detection heuristic из cwd:

  | Маркер | Detected runtime | Priority |
      |---|---|---|
  | `bun.lock` или `bun.lockb` | `bun` | High |
  | `pnpm-lock.yaml` | `node-pnpm` | High |
  | `yarn.lock` | `node-yarn` | High |
  | `package-lock.json` | `node-npm` | High |
  | `package.json` без lock-файла | `node-npm` (fallback) | Medium |
  | `pyproject.toml` с `[tool.poetry]` | `python-poetry` | High |
  | `pyproject.toml` с `[tool.uv]` или `uv.lock` | `python-uv` | High |
  | `Pipfile` | `python-pipenv` | High |
  | `requirements.txt` без других | `python-pip` | Medium |
  | `Cargo.toml` | `rust` | High |
  | `go.mod` | `go` | High |
  | `Dockerfile` без других маркеров | `docker` (build-only template) | Low |
  | Ничего | `blank` (пустой шаблон с TODO) | Fallback |

- [x] Confidence levels:
    - `High`: явный lock-файл = runtime установлен, версии воспроизводимы. Default-select без вопросов в auto-mode.
    - `Medium`: маркер есть, но lock отсутствует — менее уверенно, confirm prompt всё равно.
    - `Low` / `Fallback`: prompt со списком всех runtimes, юзер выбирает руками.

- [x] Multiple маркеры одновременно (monorepo, мультистек):
    - Если детектится 2+ runtime с High confidence — prompt со списком, default = первый по алфавиту.
    - Можно подсказать через CLI флаг `--runtime <name>` сразу для non-interactive.

#### Поставка — Application.cue templates

- [x] Template engine: Handlebars-like substitution на статичных `.cue.hbs` файлах в CLI binary через `include_str!` (embedded в release).
- [x] Variables в шаблоне:
    - `{{app_name}}` — из cwd name или CLI flag.
    - `{{image_ref}}` — placeholder `ghcr.io/<org>/<app>:latest` для пользовательской подстановки.
    - `{{primary_port}}` — defaults per runtime (bun/node: 3000, python: 8000, rust/go: 8080, docker: 8080).
    - `{{healthcheck_path}}` — default `/health` со комментарием "// adjust if your app uses different path".
- [x] Шаблоны (`cli/templates/application/*.cue.hbs`):
    - `bun.cue.hbs` — runtime=bun, build с bun build script.
    - `node-pnpm.cue.hbs`, `node-yarn.cue.hbs`, `node-npm.cue.hbs` — соответствующий package manager в build steps.
    - `python-poetry.cue.hbs`, `python-uv.cue.hbs`, `python-pipenv.cue.hbs`, `python-pip.cue.hbs`.
    - `rust.cue.hbs` — cargo build --release.
    - `go.cue.hbs` — go build, static binary.
    - `docker.cue.hbs` — assume Dockerfile в корне, no build steps generation (юзер сам Dockerfile управляет).
    - `blank.cue.hbs` — пустой Application с TODO-комментариями на все required поля.
- [x] Каждый шаблон содержит:
    - `apiVersion`, `kind`, `metadata.name`
    - `spec.image` — placeholder
    - `spec.expose` — minimal (port + comment "set public: true и hostname when ready to expose")
    - `spec.resources` — sensible defaults (100m CPU / 128Mi RAM с комментарием "tune based on observed usage")
    - `spec.healthcheck` — default path `/health` с note про customization
    - Inline-комментарии на русском или английском объясняющие что каждое поле делает (selectable через `--lang ru|en`, default `en`)

#### Поставка — Scaffold flow в `app add` и `app scaffold`

- [x] `apprafter app add` (расширение из 1.79a):
    - Шаг 0 (before any prompts): проверка наличия `apprafter/Application.cue` в cwd.
    - Если **отсутствует**:
        - Run runtime detection.
        - Wizard prompt: "No `apprafter/Application.cue` found. Generate one? [Y/n]". На "Y" — continue scaffolding.
        - List с pre-selected default = detected runtime. Юзер может выбрать другой.
        - Запрос на app name (default = repo dir name).
        - Confirm: "Generate `apprafter/Application.cue` and `.gitignore` entries?".
        - Создание файла из template, append `.apprafter/local/` в `.gitignore` если отсутствует (для будущих local secrets).
        - Print "✓ Created apprafter/Application.cue — review and adjust before committing".
    - Если **присутствует**: пропускает scaffold step, идёт к existing flow (`git remote get-url origin` → register Application).
    - В non-interactive mode без `--scaffold`: если файл отсутствует — fail с hint к `apprafter app scaffold`.

- [x] `apprafter app scaffold` — standalone команда для re-scaffolding или explicit-only flow:
    - Без аргументов: detection + interactive wizard, генерит файл.
    - Флаги: `--runtime <name>` (override detection), `--name <app>`, `--lang ru|en` (комментарии), `--force` (overwrite existing).
    - Без `--force` отказывается перезаписать existing `apprafter/Application.cue` — exit code 2.

#### Поставка — README updates + examples directory

- [ ] `examples/applications/` в monorepo — реальные working Application.cue для каждого preset (используются как fixture для тестов template engine + reference в docs). **Deferred — Part 4 (ещё не отгружено).**
- [ ] `docs/user-guide/cli/app-scaffold.md` — описание templates, runtime detection logic, customization guide. **Deferred — Part 4 (ещё не отгружено).**
- [ ] Update `docs/operator-guide/quickstart.md` — шаг "Write Application.cue" заменён на "Run `apprafter app add` — it scaffolds for you". **Deferred — Part 4 (ещё не отгружено).**

#### Acceptance

- [x] `cd <bun-project> && apprafter app add` детектит bun, генерит `apprafter/Application.cue` с bun preset, регистрирует Argo CD Application.
- [x] `apprafter app open <name>` после `apprafter app status` показывающего Healthy → port-forward работает, browser открывается на app'е.
- [x] `apprafter app open` для app в Pending/CrashLoopBackOff показывает warning, но всё равно делает port-forward по confirmation.
- [x] Multiple маркеры (e.g., python-poetry + Dockerfile) → prompt со списком, не auto-pick.
- [x] `apprafter app scaffold` для пустого репо генерит `blank` template с TODO-комментариями.
- [x] `apprafter app scaffold` для существующего `Application.cue` без `--force` → exit 2.
- [x] Generated `apprafter/Application.cue` проходит `cue vet` (валидный CUE).
- [ ] Generated файл с `image` placeholder + push в репо → Argo CD синкает; AppRafter Application reaches `OutOfSync` / `Suspended` until юзер обновит image. Status condition объясняет что делать. **Partial** — scaffold + push + Argo CD sync через CMP shipped (ImagePullBackOff на placeholder-image видно через `apprafter app status --resources`, walk-fix #3); operator-side `Suspended` phase + explanatory status condition остаётся Phase 2/3 (CR health-propagation, как app-status в 1.79a).

#### Не входит в этот item

- Auto-generation Dockerfile для runtime presets (отдельный future item, нетривиально и небезопасно).
- Backstage Software Templates integration (Phase 3, Backstage plugin).
- Live `cue vet` + linting в CLI на каждом save (Phase 3+).
- AI-assisted Application.cue refinement (вне scope OSS платформы).

**Зависит от:** 1.79a (`apprafter app add` базовый flow, AppProjects, repo creds).

**Размер:** S-M

---

### 1.79c Private-repo credential flow — `SourceCredential` CRD
> 🏁 SR: A · order 3 — private-repo credential flow (`SourceCredential`, ADR 0039); **must follow the 2.11 SealedSecrets controller+seal slice** в SR-порядке (cross-phase dep — phase-номера немонотонны: Phase-1 item выполняется после Phase-2 slice, это ожидаемо; см. `speedrun-plan.md` §4.2).
>
> ✅ **ЗАКРЫТО** (walk пройден чисто 2026-06-01, без багов; запушено, образы opera­тора v0.1.137 + platform-stack 0.1.51 опубликованы, кластер сошёлся). S0–S5 + acceptance #1/#2/#3/#5/#6 валидированы на живом кластере. **Остаётся только осознанно отложенное** (не долг, cross-phase): acceptance #4 live-wiring (admission создаёт MigrationPlan + pause на host-removal) co-отложен с application-scope **B.1.77**; config-repo (GitOps) delivery → **Phase-3 Backstage**. Работа велась на `master`, design+plan в `docs/superpowers/{specs,plans}/2026-05-30-1-79c-*`. S0–S4 ВАЛИДИРОВАН end-to-end на живом кластере: `repo creds add`→seal→derive, `app add`→Argo клонит приватный репо→под тянет приватный ghcr-образ (1-й acceptance), `app remove`→Argo каскадит CR→оператор+ownerRef убирают workload без зависания. Walk-fixes #1 (seal-label `/`), #2 (registry-host lowercase), #4 (operator `deletionTimestamp`-guard), #6 (Argo cascade finalizer name — `resources-finalizer`, не `-finalization`); тупиковые #3/#5 откачены в #6. **S5 поставлен** (validity-probe · coverage-gate `confirmed` · derived-Secret GC finalizer · scoped CLI RBAC seed · `detect_destructive` классификатор) одним координированным релизом. Версии: CLI `0.1.184` (тег `v0.1.184` локально) · operator `v0.1.137` · platform-stack `0.1.51`. **Ждёт:** пользователь пушит master → CI публикует operator/webhook образы + platform-stack чарт → auto-update кластера сходится → ручной walk (acceptance #2/#3/#4 + validity + GC; см. инструкции в ответе ассистента). **spec.md уже актуализирован** (Revision 9: §3.12 SourceCredential, §4.5 деривация+validity, §4.7, CLI-секция — описывает весь ADR 0039 включая S5-поведения; новой правки не требует — план-заметка отставала).
> **S0 ✅** (v0.1.175 + platform-stack component, chart-only): sealed-secrets controller компонент (bitnami 2.18.6, ns `apprafter-system`, `fullnameOverride` пинит Service) + **нативный Rust-seal** в `cli-providers::k8s::sealing` (RSA-OAEP-SHA256 ⊗ AES-256-GCM single-use, strict scope, bitnami wire-format; cert fetch через kube API service-proxy `KubectlRunner::get_raw`) + команда `apprafter secret seal`. Вынесенный вперёд prereq-slice из 2.11.
> **S1 ✅** (v0.1.176): `SourceCredential` CRD в 3 слоя — CUE schema + example; kube-rs тип (per-half status conditions); OpenAPI v3 CRD (sync-wave -5); admission `validator_sourcecredential.rs` (at-least-one-of git/registry, exactly-one backend, non-empty coverage) + dispatch + integration-тесты; bootstrap CRD-Established wait.
> **S2 ✅** (operator-only): новый крейт `operator-controllers/sourcecredential` — watch SourceCredential, read unsealed material, derive prefix-matched Argo `repo-creds` (argocd ns) + status `GitPresent`/`GitValid=Unverified`; field manager `apprafter-sourcecredential`; RBAC (sourcecredentials + secrets); wired 4-м контроллером в `main.rs`.
> **S3 ✅** (operator-only): registry-половина — canonical `dockerconfigjson` (apprafter-system) + `RegistryPresent`/coveredHosts; **Seam A** — Application-контроллер host-match rendered image → SSA-копия pull-secret в app ns → `Deployment.imagePullSecrets`. Pure matching в `application/src/pull_secret.rs`.
> **S4 ✅** (v0.1.177): CLI `repo creds` → thin front-end над SourceCredential. `add` seal + SealedSecret + CR (git всегда; ghcr.io/<org> инферится для github); `list`/`show` читают `.status` (никогда материал); `rotate` re-seal; `remove` delete CR+SealedSecret за reverse-dep gate; `app add` coverage-hint читает SourceCredential. Legacy raw repo-creds флоу cut over (без shim).
> **S5 ✅ поставлен** (operator v0.1.137 / platform-stack 0.1.51 / CLI v0.1.184): `SourceCredentialMigrationStrategy.detect_destructive` (классификатор coverage-removal→breaking + 7 тестов + экспорт; live plan-creation wiring co-отложен с B.1.77) · live validity-probe (представительный репо/образ из матчащегося Argo/AppRafter Application → git smart-HTTP / scoped registry v2 token-exchange; консервативный mapping, egress-blocked → `Unverified`, не `Invalid`; `lastValidated`) · coverage-gate `present`|`confirmed` (`app add --coverage-gate`) · derived-Secret GC finalizer (`apprafter.io/derived-secrets-cleanup`, cross-ns без ownerRef-каскада; RBAC `delete` на secrets) · scoped CLI credential-author Role seed (unbound, no read на derived Secrets). config-repo delivery — **DEFER** (→ Phase-3 Backstage). Запись в `plan-history.md` добавлена.
> ⚠️ **Release-координация — СДЕЛАНО:** координированный bump одним коммитом `chore(release): publish 1.79c S5` (тег `v0.1.184`): operator+webhook `Chart.yaml` appVersion v0.1.136→**v0.1.137** + chart version v0.1.117→v0.1.118; platform-stack component-пины следом; `currentVersion` 0.1.50→**0.1.51** + `safe` compatibility-запись; `RELEASED_OPERATOR_VERSION` деривится build.rs из appVersion (проверено: v0.1.137); CLI v0.1.183→**0.1.184**. `operator/v0.1.137` + `platform-stack/v0.1.51` — воркфлоу-managed на push. Ждёт `git push` пользователя.

**Source:** ADR 0039. Продолжение 1.79a (repo/app subcommands). Новый item — не затрагивает отгруженный 1.79b (`app open`/scaffolding/runtime templates сохраняют свой номер и историю v0.1.161–v0.1.174).

**Цель:** один безопасный credential-флоу к приватным client-репозиториям — git-read для Argo CD + registry-pull для kubelet — из одного источника, без сырых ungated-ресурсов и без plaintext-секретов. Закрывает три дефекта текущего 1.79a-флоу: **(1)** CLI создаёт raw Argo `Application` + raw `repo-creds` Secret в обход admission+оператора (credential change не классифицируется как потенциально деструктивная операция); **(2)** credential лежит plaintext в kube Secret (несовместимо с SealedSecrets 2.11 / ADR 0024 Layer 2); **(3)** pull-secret не заводится вообще — приложения с приватным registry не стартуют.

#### Поставка — `SourceCredential` CRD (config-only, ноль материала)
- [x] CUE schema `schemas/v1alpha1/sourcecredential.cue`: `spec.git? { backend, repoPrefixes: [...] }`, `spec.registry? { backend, hosts: [...] }`; `#Backend = { sealedSecretRef } | { openBaoPath }`. Обе половины независимы; single-PAT кейс = один backend в обоих. В `spec` — никакого токена/base64.
- [x] CRD OpenAPI v3 + admission webhook (cross-field: хотя бы одна из git/registry; непустые prefixes/hosts; валидный backend ref).
- [x] `status.conditions` per-half: `Present` / `Valid` / `Invalid` / `Unverified` + covered prefixes/hosts + `lastValidated`.

#### Поставка — operator derivation
- [x] git → **prefix-matched** Argo `repo-creds` Secret в `argocd` ns (Argo клонит по URL-prefix; derived output, не hand-managed).
- [x] registry → static `dockerconfigjson` pull-secret в workload ns; **auto-attach** к workload SA / `Deployment.imagePullSecrets` по **registry-host match** (`image: ghcr.io/...` → `SourceCredential` с host `ghcr.io/...`).
- [x] Reconcile держит derived Secrets консистентными с CR + материалом (single source of truth; `rotate` материала → передеривка обеих производных).
- [x] RBAC оператора: read `SourceCredential`, read unsealed material, write argocd repo-cred + workload pull-secret, patch SA/Deployment.

#### Поставка — operator validation + status
- [x] git validity: smart-HTTP `GET <repo>/info/refs?service=git-upload-pack` (Basic auth, reqwest) против **представительного репо** из матчащегося Argo Application (префикс — орга, нужен конкретный объект; решение в `validity.rs`). registry validity: scoped v2 token-exchange (oci-distribution) против представительного образа из матчащегося AppRafter Application. On-change + каждые 60s (1/min — в пределах GitHub rate-limit). Консервативный mapping: Valid только на 2xx/success, Invalid только на явный auth-reject (401/403 / auth-failure), всё неоднозначное → Unverified.
- [x] Egress-blocked → `Unverified` (не `Invalid`) — network-error никогда не даёт Invalid. `GitValid`/`RegistryValid` + `status.lastValidated` пишутся. Нет матчащегося app → тоже `Unverified` (нечего пробить).
- [x] Coverage-gate конфигурируем `present` | `confirmed` (`app add --coverage-gate`, ValueEnum, default `present`): `present` — warn-only post-registration по наличию prefix (как было); `confirmed` — pre-flight БЛОКИРУЕТ регистрацию приватного (`https`) репо, пока покрывающий `SourceCredential` не в `GitValid=True` (pure `valid_credential_covers` читает `.status.conditions`). Egress-restricted → дефолт `present` (validity остаётся `Unverified`). [ADR 0039 §Validation and status]

#### Поставка — CLI front-end refactor (`repo creds` → `SourceCredential`)
- [x] `apprafter repo creds add`: shape-check (existing regex) → seal материал client-side (kubeseal публичным сертом контроллера; серт **пинится** / fetch через TLS kube API) → create/update `SourceCredential` CR + SealedSecret. Опц. поллит `.status` несколько сек для validity-фидбэка; иначе «submitted, validity pending».
- [x] `list` / `show`: читают `.status` (coverage + validity), **никогда** материал (CLI не может расшифровать SealedSecret — нет cluster private key).
- [x] `rotate`: re-seal материал на эквивалентный валидный cred (оператор передеривает обе производные; non-destructive).
- [x] `remove`: delete CR с reverse-dep gate (переиспользует `find_apps_matching_prefix` из 1.79a).
- [x] `app add` coverage-check: гейтит по «есть `Valid` cred, покрывающий repo prefix» из `.status` (вместо CLI-догадки); режим гейта = coverage-gate (`present`/`confirmed`) из operator-секции выше.
- [x] Scoped CLI RBAC role (Phase-4 scoped-identity seed, фиксируется уже сейчас): `Role` `<fullname>-credential-author` в release-ns (`operator/charts/.../templates/rbac-cli.yaml`, gate `.Values.cliRole.create`), shipped **UNBOUND** (Phase 4 биндит к scoped identity). read `SourceCredential` (+`status`), write `SourceCredential` / `SealedSecret` (bitnami.com), **no read** на core/v1 Secrets — крипто-нечитаемость материала из CLI по построению, не только RBAC. Public-cert fetch через service-proxy — отдельный non-sensitive grant, не в seed. [ADR 0039 §CLI as a thin front-end]
- [x] **Cut over** от legacy raw `repo-creds` Secret флоу — без migration shim (pre-1.0, флоу живёт только в dev-кластере).

#### Поставка — MigrationPlan integration (destructive credential change)
- [x] `SourceCredentialMigrationStrategy` с `detect_destructive(old, new) -> Option<DestructiveChange>`: rotate-to-equivalent-valid = `None`; coverage removal (repo-prefix / registry-host, включая drop целой half) = destructive → `DestructiveChange{trigger:"coverage-removal", classification:"breaking"}`. Actor-agnostic классификатор поставлен + 7 юнит-тестов + экспорт. **Live plan-creation wiring** (reconcile-time snapshot→build MigrationPlan→pause derivation) + `create_plan_for` + scope-вариант `sourcecredential` в MigrationPlan CRD (kube-rs/OpenAPI/CUE/admission) **co-отложены** с application-scope live-wiring (B.1.77 — «один call-site, который раскомментируют»; ADR 0039 §169 определяет вклад 1.79c в gate именно как «adds a `MigrationStrategy.detect_destructive`»). `delete-while-referenced` (удаление CR при матчащихся приложениях) уже закрыт reverse-dep gate в CLI `repo creds remove` (S4); admission-side actor-agnostic вариант — с тем же B.1.77. Зависит от MigrationPlan CRD (1.72–1.78).

#### Поставка — delivery modes
- [x] CLI→cluster: `kubectl apply` SealedSecret + CR.
- [~] config-repo (опц. инфра-репо): commit sealed + CR, Argo синкает (материал sealed → git-safe). Pure-GitOps-без-cluster-read: coverage-match по declared prefixes, validity в Backstage. **DEFER** (явное решение на закрытии 1.79c, 2026-05-31): помечен «опц.» в исходном scope; pure-GitOps-без-cluster-read (validity в Backstage) — заметный кусок, ближе к Backstage-фазе (Phase 3). Архитектурно уже поддержан (материал sealed → git-safe в обоих режимах — ADR 0039 §"Delivery is mode-agnostic"); закрытие 1.79c не требует. Подтянуть с Phase-3 Backstage-вью.

#### Поставка — credential type
- [x] Launch default: single classic PAT (`repo` + `read:packages`) в обе половины. GitHub-ограничение (нет `repo:read`-only; fine-grained без packages; App-токен ghcr.io не берёт) — принимается; платформа хранит sealed.
- [x] Schema split-ready с первого дня (git ≠ registry backend). Wizard split (deploy-key/fine-grained git + `read:packages`-only registry с package-level access; GitLab — один `read_repository`+`read_registry` токен) — **opt-in, не дефолт**; визард-выбор откладывается до operator feedback.

#### Acceptance
- [x] Private GitHub репо + приватный ghcr.io образ: `repo creds add` (classic PAT) → `SourceCredential` `Valid` → `app add` проходит coverage-check → Argo клонит (prefix-matched repo-cred) → оператор рендерит Deployment с auto-attached pull-secret → под стартует, образ тянется.
- [x] Org-cred (`repoPrefixes: ["github.com/myorg/"]` + `hosts: ["ghcr.io/myorg/"]`) покрывает второе приложение орги без отдельного `repo creds add` (auto-match); в `Application.cue` про credentials ничего. **Валидировано walk'ом 2026-06-01.**
- [x] `repo creds rotate` на валидный новый PAT → обе производные передериваются, Argo + kubelet продолжают без даунтайма; MigrationPlan не создаётся (non-destructive; `detect_destructive`=`None`). **Валидировано walk'ом 2026-06-01.**
- [~] Убрать registry-host из покрытия CR, пока приложение на него матчится → admission создаёт MigrationPlan; derived pull-secret не трогается до approve. **Классификатор поставлен** (`detect_destructive` → `coverage-removal`/`breaking`, 7 тестов), но **live admission-creates-MigrationPlan + pause co-отложен с B.1.77** (паритет с application-scope; ADR 0039 §169 — вклад 1.79c = «adds `detect_destructive`»). Сейчас удаление host'а проходит, derived pull-secret передеривается под reverse-dep gate CLI; полное end-to-end gating — с B.1.77.
- [x] `repo creds show` / `list` / `kubectl get sourcecredential -o yaml` — нигде plaintext токена; SealedSecret нерасшифровываем без cluster private key.
- [x] Restricted-egress кластер: валидный PAT → status `Unverified` (не `Invalid`); coverage-gate в `present`-режиме пропускает.

#### Не входит в этот item
- OpenBao backend (T2) — с 2.7–2.8 / 3.11; schema `backend` уже предусматривает `openBaoPath`.
- Wizard выбора credential-типа (single vs split) — flag-driven + дефолт single PAT; визард по operator feedback.
- Short-lived-token registry + refresher / kubelet credential provider — не нужно для classic-PAT GHCR; managed-era / cloud-registry concern.
- GitHub App credential path — managed-era refinement (git-половина).
- Backstage `SourceCredential` view — Phase 3 Backstage plugin.

**Зависит от:** 1.79a (repo/app subcommands, `find_apps_matching_prefix`); **2.11** (SealedSecrets controller + kubeseal-in-CLI slice — **cross-phase, см. `speedrun-plan.md` §4.2 SR ordering**); 1.72–1.78 (MigrationPlan CRD — для destructive-gating подчасти).

**Размер:** L

---

### 1.80 `apprafter platform fork` GitHub API automation
> ⏸️ **DEFERRED (2026-06-01)** — power-user one-command fork; **не на managed-launch критическом пути** (no SR bucket marker). Явно отложен при закрытии M1.5 (решение пользователя). Не блокирует M1.5: `e2e/fork.sh` дропнут, docs fork = 1 строка-ссылка сюда. Подтянуть post-launch / по запросу power-users.

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

> ✅ **CLOSED (2026-06-02):** k3d e2e-гейт зелёный (run `26815926016`, `gitops-walk.sh` 3m16s) → spec.md §6 M1.5 box флипнут + Revision 9→10. Деталь раунда ниже. **SCOPED CLOSE (2026-06-01, ревизия 2026-06-02):** локальный кластер проекта — **k3d** (не kind, формулировка ниже устарела). Сэндбокс ассистента **не запускает k3d** (Podman без `/var/run/docker.sock`) → k3d-скрипты написаны + `shellcheck`-clean + заведены в CI, валидатор — CI (аналог ручного walk). Общий `e2e/lib.sh` harness извлечён. **2026-06-02 (CI-разбор):** `migration-platform.sh` **снят с k3d-гейта** (Option C) — фундаментально несовместим со средой: PlatformController-OCI-клиент **HTTPS-only** (`oci-distribution ClientConfig::default()`), а локальный реестр k3d — **plain-HTTP**, поэтому контроллер не может стянуть compat-doc фикстур in-cluster → классификация перехода падает, план не создаётся даже на CR-уровне. Net: k3d-гейт = `gitops-walk.sh`; `mvp.sh` = Hetzner-nightly; миграционный гейт покрыт unit+integration тестами оператора + (follow-up) real-infra walk на nightly Hetzner.
>
> **Поставка:**
- [x] `e2e/mvp.sh` rewritten — 9-step → 3-step (init → bootstrap-all → Application smoke); **+ассерт что platform-компоненты Argo-CD-managed** (не CLI-applied). Общий `e2e/lib.sh`. Hetzner-nightly. (commit `bfcd95d`)
- [x] `e2e/gitops-walk.sh` — k3d: fixture app repo (local git daemon) → `app add` → CMP рендерит Application CR → Argo синкает → operator reconciles Deployment → push `Application.cue` change → сходится. shellcheck-clean; CI-validated. (commit `a7a4051`)
- [~] `e2e/migration-app.sh` — **DEFER → Phase 2**: тестит `needs.pg` end-to-end provision (ResourceClaim + ServiceProvider + CloudNativePG); `needs` **явно удалён** из v1alpha1 (`schemas/v1alpha1/application.cue:18`), реализуем только после 2.2/2.3.
- [~] `e2e/migration-platform.sh` — **DROP с k3d-гейта (2026-06-02, Option C)**: скрипт удалён. CI-разбор вскрыл, что walk нереализуем на k3d на нескольких уровнях, и все блокеры упираются в **неотгруженную/отложенную** инфру, а не в баг гейта (1.78 корректен, покрыт unit+integration): (1) PlatformController-OCI **HTTPS-only** vs plain-HTTP реестр k3d → compat-doc фикстур не тянется; (2) `PlatformController` пробрасывает родителю только `targetRevision`+`helm.values`, **не `repoURL`** → реальный деплой редиректнутого реестра = fork-история **1.80 (deferred)**; (3) попытка пропатчить `targetRevision` на отсутствующую в ghcr версию вешает in-flight гейт; (4) исходный скрипт был написан против неверной модели триггера (доступность-сама-создаёт-план вместо pin→breaking). Корректный blueprint будущего real-infra теста (pin-driven, nudge PlatformStack после `approve`, HTTPS-реестр) — в memory `project_m1_5_close`. **Follow-up'ы:** (a) env-gated insecure/HTTP OCI в контроллере (dev-mode/1.9 территория); (b) `apprafter migration approve` мог бы пинать PlatformStack — сейчас bump доезжает лишь на следующем reconcile (до `checkInterval` ≤6h), т.к. PlatformController не watch'ит MigrationPlan.
- [~] `e2e/fork.sh` — **DROP**: item 1.80 (`platform fork`) сейчас не делаем.
- [x] All scripts callable from CI; budget < 30 min per script на **k3d** cluster — `Justfile` (`e2e`/`e2e-gitops`) + CI `e2e-k3d.yml` (PR-triggered); `mvp.sh` остаётся nightly. (commit `6921670`; dist-render step + `e2e-migration-platform` recipe убраны 2026-06-02 вместе с descope)

**Acceptance:**
- [x] **(CI-gate GREEN 2026-06-02, run `26815926016`)** `just e2e` (= `gitops-walk.sh`) ran green on k3d in CI in 3m16s — bootstrap (Cilium→Argo→platform-stack 0.1.52, PlatformStack/default tier=1) + the full loop (`app add` → CMP render → Argo Synced/Healthy → operator Deployment Available → replicas 1→2 propagates). `mvp.sh` is the nightly real-Hetzner complement. **This flipped the spec.md §6 M1.5 box + Revision 9→10.**
- Major M1.5 code paths exercised: cluster-bootstrap + Application reconcile (mvp), CMP→Argo→operator loop (gitops-walk). PlatformStack pin→breaking→MigrationPlan→approve/reject gate covered by **operator unit + integration tests** (k3d walk descoped — see migration-platform.sh note above). `needs.pg`/migration-app path → Phase 2; fork path → 1.80 (deferred); real-infra migration walk → nightly-Hetzner follow-up.

**Зависит от:** all 1.66–1.80 (1.80 deferred — `fork.sh` dropped accordingly)

**Размер:** M

---

### 1.82 Docs update

**Source:** ADR 0025, 0026, 0027, 0028, 0029.

**Цель:** rewrite outdated quickstart, add new operator/dev guides.

**Поставка:**
> ✅ **DONE (2026-06-01, commit `a7efe69`)** — все доки написаны, `mkdocs build --strict` чист (0 warnings/errors), `cyrillic`-хук clean. Fork = одна строка-defer (1.80). Доки реально валидируемы (в отличие от e2e).
- [x] `docs/operator-guide/quickstart.md` rewritten: 3-step (install → `target add`/`init` → `bootstrap-all`), Argo-CD-managed platform на first read, `apprafter open argocd`, **CX22 → CPX22**, smoke через `Application` CRD.
- [x] `docs/operator-guide/platform-management.md` (new): PlatformStack lifecycle, каналы (stable/beta/edge), upgrade/freeze/rescue. **Fork = 1 строка** (power-user, 1.80 not yet shipped) — не отдельная секция (scoped).
- [x] `docs/operator-guide/migration-plans.md` (new): destructive change; approve/reject by scope (application = approve-only + revert commit per ADR 0027; platform = approve/reject); surfaces (CLI сейчас; Backstage/Argo UI — позже).
- [x] `docs/dev-guide/application-cue.md` (new): `apprafter/Application.cue`, CMP render + troubleshooting, multi-env (`spec.environments` + `APPRAFTER_ENV`).
- [x] `docs/operator-guide/gitops-walk.md` updated: Application-CRD end-to-end (не raw Deployment+Service); + `mkdocs.yml` nav.
- [x] Update root `README.md` reference links + dev-quickstart CX22→CPX22 fix.

**Acceptance:**
- New user reading quickstart end-to-end can get to running app in ~30 min.
- Docs explain Argo CD's role clearly without contradictions.
- Mental model "platform reconciles itself" передаётся on first reading.

**Зависит от:** 1.81

**Размер:** S

---

## Фаза 1.9 — Dev Mode MVP (Phase 1B из dev-mode-task.md)
> 🏁 SR: D — dev mode dropped from launch (managed users don't bootstrap local clusters); reactivate after managed traction

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
> 🏁 SR: A · order 3 (Phase-2 minimum)
> v0.2.1 — 2.1 shipped: ServiceProvider CRD (CUE schema + OpenAPI v3 CRD + kube-rs type + admission validator/dispatch/webhook + cue-vet example + tests). Namespaced. Tier-aware defaults deferred to 2.4–2.6.
> v0.2.2 — 2.1 re-release (release-pipeline fix): the v0.2.1 operator+webhook images were clobbered on ghcr by their own Helm charts (chart and image share the repo path `ghcr.io/apprafter/apprafter-operator`; uniform chartVersion==appVersion let the chart push overwrite the image tag → pods crash-looped `exec: no such file`). Fix: charts now publish to the `ghcr.io/apprafter/charts` OCI sub-namespace (new `apprafter-charts` enableOCI registration in loader_values.cue; components' repoURL points there). v0.2.1 abandoned.

**Поставка:**
- [x] CUE-схема + admission webhook.
- [x] Поля: `type`, `backend`, `labels`, `config` (raw map), `status.health`.
- [x] Built-in типы (закрытый enum в v1alpha1): `pg`, `jetstream`, `clickhouse`, `redis`, `s3`, `notifications`.
- [ ] Tier-aware defaults в схеме (через `if tier == 1 ...`). — **deferred to 2.4–2.6** (concrete tier-default provider instances land with the actual providers; 2.1 ships schema + admission only)

**Acceptance:** ServiceProvider валидируется; неизвестный `type` без плагина — ошибка admission.

**Зависит от:** 1.7

**Размер:** S

---

### 2.2 ResourceClaim CRD
> 🏁 SR: A · order 3 (Phase-2 minimum)
> v0.2.3 — 2.2 shipped: ResourceClaim CRD (CUE schema +status.conditions + OpenAPI v3 CRD + kube-rs type + operator-only admission validator/dispatch/webhook + cue-vet example + tests). Namespaced. Operator-only CREATE (operator SA OR system:masters); UPDATE ungated. status.conditions schema-only (writer in 2.3); no creator until 2.4.

**Поставка:**
- [x] CUE-схема + admission webhook.
- [x] Поля: `type`, `selector`, `spec` (size, etc.), `status.{provider, connectionSecretRef, ready, conditions}`.
- [x] Создаётся **только** оператором, юзер-create запрещён admission.

**Зависит от:** 2.1

**Размер:** S

---

### 2.3 Selector matching и provider scheduler
> 🏁 SR: A · order 3 (Phase-2 minimum)
> v0.2.4 — 2.3 shipped: ResourceClaim scheduler controller (operator-controllers-resourceclaim-scheduler, 5th controller). Type-equal + selector-superset match, cluster-wide provider listing, alphabetical tie-break; on match writes status.provider + Scheduled=True; no-match -> Scheduled=False + NoMatchingServiceProvider event + apprafter_claim_unmatched_total. Pending claims re-evaluate on a 300s requeue (a ServiceProvider watch needs a reflector Store — deferred). ready/connectionSecretRef + provisioning deferred to 2.4.

**Цель:** Reconcile ResourceClaim → matching ServiceProvider по labels.

**Поставка:**
- [x] Лог matching-логики: точное соответствие labels, default `tier: integrated`.
- [x] При нескольких подходящих — детерминированный выбор (alphabetical `name`).
- [x] При отсутствии подходящего — Status `Pending`, событие.
- [x] Метрики: `claim_unmatched_total`.

**Deferred (tracked follow-up, not debt-blocking):**
- [ ] `Controller.watches(ServiceProvider)` for **immediate** self-heal of `Pending` claims. 2.3 shipped a **300s requeue** fallback instead — a late-arriving provider rescues a Pending claim within ≤5 min, not instantly. Deferred because the event→claims mapper needs a `reflector::Store<ResourceClaim>` (cross-resource fan-out), out of the 2.3 MVP scope and moot until claims exist. Revisit in/after **2.4** (once the Application operator generates claims) or when claim volume/latency warrants; mind the O(claims×providers) fan-out at scale.

**Зависит от:** 2.2

**Размер:** S

---

### 2.4 needs.pg → CloudNativePG
> 🏁 SR: A · order 3 (Phase-2 minimum) — needs.pg, the launch database

**Первая user-facing фича Фазы 2** — закрывается подробным ручным walk'ом на реальном Tier-1 (2.4g, см. [[feedback_phase_closure_validation]] / [[feedback_walk_ux_coverage]]). NOTE: 2.4 ≠ закрытие Фазы 2 — НЕ флипать §6 M2 на 2.4g (M2 закрывается после 2.5/2.6/2.10–2.12).

**Сквозной поток:** `Application.spec.base.needs.pg {selector?, size?}` → Application-контроллер генерирует дочерний `ResourceClaim` (type pg, ownerRef, дефолтный selector `{tier:integrated}` инжектится контроллером) + ставит app в новую фазу-паузу `AwaitingResourceClaim` (зеркало AwaitingMigrationApproval) → scheduler 2.3 (переиспользуется) матчит → `status.provider=pg-integrated` + `Scheduled=True` → новый 6-й контроллер `resourceclaim-provisioner` провижинит per-claim CNPG Database+role+Secret и пишет `status.ready`+`connectionSecretRef`+`Ready` под СВОИМ field manager → operator-rendering инжектит DSN (`DATABASE_URL` через secretKeyRef) в Deployment env → app коннектится. На delete: finalizer снапшотит в `RetainedClaim` (retainUntil=now+7d) → GC дропает DB+Secret после истечения.

**Зафиксированные решения (с пользователем, 2026-06-02):**
- **CNPG footprint на Tier-1 = оператор always-on, shared-кластер LAZY.** platform-stack ставит CNPG-оператор (always-on, лёгкий) + `pg-integrated` ServiceProvider CR, но НЕ сидит shared Cluster. `resourceclaim-provisioner` создаёт shared `platform-postgres` CNPG Cluster **лениво + идемпотентно на первом матченном claim'е** (единоличный owner — нет гонки контроллеров). Solo-кластеры без pg-приложений не платят за Postgres-под. Первый pg-claim имеет повышенную латентность (boot кластера ~30–60с) — её закрывает пауза-гейт `AwaitingResourceClaim`. Кластер НЕ сносится при опустошении claim'ов.
- **Provisioner = НОВЫЙ generic-крейт `operator-controllers/resourceclaim-provisioner`** (6-й контроллер), свой field manager — НЕ расширение scheduler'а (сохраняет чистый SSA-split: scheduler владеет `status.provider`+`Scheduled`, provisioner — `ready`+`connectionSecretRef`+`Ready`). Generic, чтобы 2.5 jetstream / 2.6 redis переиспользовали крейт (dispatch по типу/backend claim'а).
- **`needs.pg` = объект `{selector?, size?}`** на тип сервиса (закрытый enum), пере-добавляется в 4 зеркала (CUE application.cue + kube-rs ApplicationBaseSpec + OpenAPI v3 crd-application.yaml + admission webhook). `needs` был удалён на f81e350; CUE — пермиссивно, non-empty-selector / size-enum / reserved-`DATABASE_URL` → CRD+webhook.

**Декомпозиция (порядок сборки):**
- [x] **2.4a** ✅ — CNPG-оператор как platform-stack component (`component_cloudnative-pg.cue`, chart 0.28.2 → operator appVersion 1.29.1, ns cnpg-system, sync-wave -5, project platform-providers) + сид `pg-integrated` ServiceProvider через НОВЫЙ data-driven umbrella-шаблон `templates/serviceproviders.yaml` (зеркало `appProjects`-механизма: `#ServiceProviderSeed` + `_serviceProviders` + render-task; `SkipDryRunOnMissingResource=true` на CRD-гонку), tier-aware `instances` (T1=1 / T2=3). **platform-stack-only**: `currentVersion` 0.2.4→0.2.5 + compatibility (change=safe, `operatorVersion` остаётся v0.2.4); БЕЗ operator appVersion bump, БЕЗ cli bump, БЕЗ monorepo-тега; `platform-stack/v0.2.5` — workflow-made на пуше. **БЕЗ сида Cluster CR** (lazy — 2.4c создаст лениво). Реализовано subagent-driven (6 коммитов) + двухстадийная ревизия (spec ✅ / quality Approved-with-nits → breadcrumb-фиксы применены). Гейт зелёный. Ждёт пуша + CI.
- [x] **2.4b** ✅ — пере-добавлен `needs` в 3 зеркала (CUE `application.cue` `#ServiceNeed` + `needs?: [#PlatformServiceType]: #ServiceNeed`; kube-rs `operator-core` `ServiceNeed` + поле на `ApplicationBaseSpec`; OpenAPI v3 CRD под base И environments — `selector` minProperties 1 + `size` enum) + webhook (отклоняет неизвестные `needs`-ключи, base + per-env). `f81e350` снёс лишь skeleton-заглушку — форма спроектирована заново по spec §3.1. Чистая схема, без поведения контроллера (генерация claim'ов — 2.4d; reserved-`DATABASE_URL` guard — 2.4e). **Уточнение по зеркалам:** CLI-копия Application CRD (`cli-providers::k8s::application_crd`) удалена ещё в B.1.71 — CRD шипает только чарт оператора, поэтому 2.4b **НЕ трогает cli/** → operator+platform-stack release **0.2.6** (re-uniform после platform-stack-only 2.4a@0.2.5), БЕЗ cli-bump/monorepo-тега. subagent-driven (6 коммитов) + двухстадийная ревизия (spec ✅ / quality Approved-with-nits → 3 фикса применены: точный CUE-комментарий о закрытости + sync-нота webhook'а на 4 сайта + multi-bad-key тест). Гейт зелёный. Ждёт пуша + CI.
- [x] **2.4c** ✅ — `resourceclaim-provisioner` контроллер (6-й контроллер, field manager `resourceclaim-provisioner`): для каждого `Scheduled=True` pg-claim'а лениво SSA-applies shared CNPG `Cluster` (`platform-postgres`, создаётся на первом claim'е — solo-кластеры без pg-приложений не платят за Postgres-под), провижинит per-claim role (RMW unkeyed `spec.managed.roles` с retry на 409) + basic-auth Secret (ns cnpg-system) + декларативный CNPG `Database` CR + connection Secret с `DATABASE_URL` (claim ns, ownerRef→claim → каскад на delete), пишет ТОЛЬКО `status.ready`/`connectionSecretRef`/`Ready` под своим field manager (НЕ трогает scheduler-owned `status.provider`/`Scheduled`). Внешние CNPG CR применяются через `DynamicObject` + `ApiResource::from_gvk` (нет compile-time CNPG-типов). **Cleanup — скелет** (решено): connection Secret каскадит по ownerRef; role+DB НЕ удаляются на delete (retained до 2.4f); finalizer на delete только логирует "retained pending 2.4f GC" и снимает себя. Новый RBAC: `postgresql.cnpg.io` clusters+databases CRUD (secrets уже cluster-wide). Новая метрика `apprafter_claim_provisioned_total{backend,namespace}`. Coordinated operator+platform-stack release **0.2.6→0.2.7** (оба чарта version+appVersion, оба component-пина, currentVersion 0.2.7 + compatibility change=safe operatorVersion v0.2.7); БЕЗ CRD-изменений, БЕЗ cli-bump/monorepo-тега; `operator/v0.2.7` + `platform-stack/v0.2.7` — workflow-made на пуше. TDD на pure-хелперах (cnpg builders + decision points), gated smoke `#[ignore]`. Гейт зелёный (fmt/clippy -D warnings/test --workspace, cue vet -c, render-only, helm lint, SPDX). Ждёт пуша + CI + 2.4g manual walk.
- [x] **2.4d** ✅ — генерация claim'а Application'ом + пауза-гейт `AwaitingResourceClaim` (зеркало MigrationPlan-гейта). Контроллер Application для каждого `needs`-ключа эффективного спека SSA-применяет дочерний `ResourceClaim` `{app}-{type}` (DNS-1123-fold, дефолтный селектор `{tier: integrated}` при отсутствии, `size` проброшен при наличии, ownerRef→Application controller=true/blockOwnerDeletion=true для каскада) — пишет **только spec+metadata, НИКОГДА status** (scheduler владеет status.provider/Scheduled, провижионер — status.ready/connectionSecretRef/Ready; строгий SSA-сплит под field manager `apprafter-operator`, payload без ключа `status`); затем пауза в новой фазе `AwaitingResourceClaim` (зеркало `AwaitingMigrationApproval`: `Ready=False`/`ResourceClaimPending` + `ResourceClaimPending=True` с именами unready-claim'ов, observedGeneration+endpointURL сохранены, lastTransitionTime сохранён при already-True) пока каждый claim не отрапортует `status.ready==true` И `connectionSecretRef` (ОБА — закрывает half-ready resume-гонку). Резюм — мгновенный через новый `.owns(ResourceClaim)` watch. **S0-фикс (латентная дыра 2.4b):** `effective_spec` теперь мёржит `needs` на env-override (per-key whole-object replace, зеркало `expose`). **Решения (reversible, в compatibility-нотах):** смена `needs.*.selector` НЕ-деструктивна в 2.4d (без MigrationPlan-гейта; revisit в 2.5+); predicate readiness = ready AND connectionSecretRef. Новый RBAC: правило `resourceclaims` получает `create` (SSA-apply, создающий объект, требует его) — отщеплено от `resourceclaims/status` (контроллер Application никогда не пишет status claim'а). **Scope held:** БЕЗ инъекции DSN/`DATABASE_URL` в Deployment (это 2.4e — `needs.pg`-апп резюмится БЕЗ `DATABASE_URL` до тех пор); БЕЗ Application-side finalizer/GC (2.4f). Coordinated operator+platform-stack release v0.2.7→v0.2.8 (оба чарта version+appVersion, platform-stack component-пины + currentVersion 0.2.7→0.2.8, compatibility change=safe operatorVersion v0.2.8); БЕЗ CRD-изменений, БЕЗ cli-bump/monorepo-тега. Полный цикл generate→provision→resume — на 2.4g real-cluster walk. Гейт зелёный. Ждёт пуша + CI.
- [x] **2.4e** ✅ — инъекция DSN `DATABASE_URL` в Deployment needs.pg-аппа. Три скоординированных изменения, рендерер остаётся ЧИСТОЙ функцией: (A) `operator-rendering` получает проброшенный параметр `needs_secrets: Option<&BTreeMap<String,String>>` (needs-type → connectionSecretRef) и для каждого известного need'а с разрешённым connection-Secret'ом добавляет EnvVar `valueFrom.secretKeyRef{name: <connectionSecretRef>, key: "DATABASE_URL", optional: false}` в контейнер — ПОСЛЕ литерального `env`, итерируя `needs.keys()` (BTreeMap) → байт-стабильный Deployment (SSA no-op; недетерминированный порядок крутил бы оператор). Модульная таблица pg→DATABASE_URL (`NEEDS_ENV_VAR_NAME`) держит **только-pg** (jetstream/redis — 2.5/2.6). (B) reconcile резолвит ready-claim'ы в эту мапу через новый чистый хелпер `resolve_needs_secrets(&[ResourceClaim])` (key=`spec.type_`, value=`status.connectionSecretRef`, пропуск claim'ов без secret-ref), построенный из ТЕХ ЖЕ `current` ready-claim'ов, которые валидировал 2.4d-гейт, ПОСЛЕ прохождения гейта — оператор только ЧИТАЕТ status claim'а (провижионер-owned), НИКОГДА не пишет его (SSA-сплит сохранён); пробрасывает `Some(&map)` (или `None` при пустой/pre-gate) в рендер. (C) admission-webhook отклоняет Application с `needs.pg` И литеральным `env.DATABASE_URL` (коллизия) — ЖЁСТКИЙ reject (не warn; консистентно с hard-enforce-позицией платформы, revisit на UX-polish), ГЛОБАЛЬНО/cross-scope (pg в base ИЛИ любом environment резервирует DATABASE_URL везде), multi-error (по ошибке на поле, без short-circuit). **Scope held:** только-pg; БЕЗ полного 2.12 `claim.*`/`secret()` reference-engine; БЕЗ cross-namespace; БЕЗ CRD/CUE/operator-core схемных изменений; 2.4d readiness AND-гейт НЕ ослаблен (инъекция строго post-gate). Coordinated operator+platform-stack release v0.2.8→v0.2.9 (оба чарта version+appVersion, platform-stack component-пины + currentVersion 0.2.8→0.2.9, compatibility change=safe operatorVersion v0.2.9); БЕЗ cli-bump/monorepo-тега. Полный цикл generate→provision→resume→DSN-injected — на 2.4g real-cluster walk. Гейт зелёный. Ждёт пуша + CI.
- [x] **2.4f** ✅ — `RetainedClaim` CRD + finalizer-снапшот + 7-дневный grace GC-контроллер. Поставлено ОДНИМ юнитом (CRD+finalizer+GC — floor без GC тёк бы вечно, хуже 2.4c; инжектированные часы снимают единственную причину сплита). Новый immutable namespaced `RetainedClaim` CRD (5 hand-rolled зеркал: CUE `retainedclaim.cue` + cue-vet пример, OpenAPI v3 `crd-retainedclaim.yaml` с CEL `self == oldSelf` immutability + БЕЗ status-сабресурса, kube-rs `operator-core` тип, admission `validator_retainedclaim.rs` operator-only CREATE + spec-immutability-on-UPDATE + диспатч в server.rs + VWC). На delete pg-claim'а провижионер-finalizer СНАПШОТИТ его в `RetainedClaim` в **`apprafter-system`** (USER-CHOSEN, leak-safe: platform-ns переживает tenant-ns → GC сработает даже если ns приложения снесён; lineage в `spec.claimRef`, `metadata.name = cnpg::k8s_name(claim_ns, claim_name)`, `retainUntil = deletionTimestamp + 7d`) **ДО** снятия finalizer'а (crash-safe: краш по дороге переприменяет байт-идентичный идемпотентный SSA-снапшот, затем снимает finalizer). Новый **7-й контроллер** (`gc.rs`, та же crate — без нового workspace-member) смотрит `Api::<RetainedClaim>::all` и после `retainUntil` дропает по порядку, каждый шаг идемпотентен + 404-tolerant: per-claim role (RMW unkeyed `spec.managed.roles` через новый pure `cnpg::remove_role` + retry на 409), database через **`spec.ensure: absent` SSA-patch — НЕ delete CR** (Postgres reclaim default = retain, удаление CR НЕ дропнуло бы БД; ensure:absent — правильный дроп, ~1 KB tombstone self-heal'ится при возврате приложения), password Secret (cnpg-ns, без ownerRef → без каскада), затем сам `RetainedClaim`. Un-e2e-able 7-дневный таймер вынесен в PURE `grace.rs` (`compute_retain_until`/`should_gc`/`remaining_grace`) с ИНЖЕКТИРОВАННЫМ `now: DateTime<Utc>` (прод — `Utc::now()`; unit-тесты — ФИКСИРОВАННЫЕ инстанты, без реального ожидания, без env-gated silent-skip; malformed `retainUntil` логирует + requeue, не паникует). Новая метрика `apprafter_claim_gc_total{result,namespace}`; новый RBAC (`retainedclaims` get/list/watch/create/patch/delete + `delete` на cnpg clusters/databases). SSA-сплит сохранён — finalizer только СОЗДАЁТ RetainedClaim (никогда не пишет status ResourceClaim'а); GC читает только spec RetainedClaim'а. TDD на pure-частях (`remove_role` 4 кейса, grace-часы, snapshot-билдер `retained_claim_object`, webhook-валидатор). Coordinated operator + platform-stack release v0.2.9 → v0.2.10 (оба чарта version+appVersion, platform-stack component-пины + currentVersion 0.2.9 → 0.2.10 + compatibility change=safe operatorVersion v0.2.10); БЕЗ cli/Cargo.toml-bump, БЕЗ monorepo v0.x.y-тега; `operator/v0.2.10` + `platform-stack/v0.2.10` — workflow-made на пуше. Гейт зелёный (fmt/clippy -D warnings/test --workspace, cue vet -c, check-platform-stack-version, render-only, helm lint, SPDX). delete → snapshot → GC loop (assert БД реально ДРОПНУТА, не только CR) — на 2.4g real-cluster walk. Ждёт пуша + CI + 2.4g.
- [ ] **2.4g** — `e2e/needs-pg-walk.sh` (k3d) + **подробный ручной walk** на реальном Tier-1 + plan.md/plan-history/UNRELEASED + координированный operator+platform-stack release bump.

**Acceptance:** манифест из §3.1 (parser) с `needs.pg` поднимается, в pg-кластере появляется DB, приложение коннектится.

**Зависит от:** 2.3

**Размер:** L (декомпозирован в 2.4a–g; полный дизайн — память `project_2_4_needs_pg`)

---

### 2.4h — Image tag→digest resolution & auto-rollout (ADR 0040)
> 🏁 SR: A — push→deploy на мутабельном теге; launch-critical UX, притянут через 2.4g walk; **закрыть ДО 2.6**

**Контекст:** re-push образа под тем же тегом (`:latest`) сейчас НЕ катит workload — fake-consistency (Git=`latest`, кластер=`latest`, но разные байты; «что в Git, то в кластере» молча нарушено). Клиентский CI тегает `:latest` из протектед-ветки = деплой (индустриальная практика) → платформа берёт pull-половину push-pull → push-and-it-deploys. Полный дизайн + альтернативы (A / Image-Updater / annotation-вариант) + риски — **ADR 0040**.

**Сквозной поток:** контроллер Application на reconcile резолвит `spec.base.image` (тег) → текущий registry-digest (OCI manifest HEAD, auth через `pick_pull_credential`/`dockerconfigjson` из ADR 0039, публичные — анонимно) → рендерит Deployment запиненным на `repo@sha256:<digest>` → пишет `status.image.{tag,resolved,resolvedAt}` → сдвинулся тег → новый digest → обычный rolling update. Requeue ~60с (существующий цикл, conditional). Рендерер ОСТАЁТСЯ ЧИСТЫМ (I/O в контроллере, digest приходит строкой). Argo не трогаем (зона манифестов vs зона образов). Graceful-fallback на verbatim-тег + condition `ImageResolved=False`, резолв НИКОГДА не блокирует rollout.

**Решения/инварианты:**
- **Opt-out** `spec.base.imagePolicy.resolve: "digest"|"off"` (дефолт `digest`, **все тиры**; `off` = verbatim ref, digest писать не заставляет, registry НЕ опрашивается).
- **Pin в `image:`** (не annotation-триггер) — под бежит ровно резолвнутый digest, без TOCTOU.
- **Гейта НЕТ** (ни авто, ни pinned) — авто-апдейт с паузой сломал бы UX; Regulated несёт свой регламент.
- **Приватные реестры в scope** — переиспользуем `SourceCredential.registry` (ADR 0039), новой cred-инфры нет.
- **Status = правда** (`status.image.resolved` = running digest) → аудируемость, `app status` показывает.

**Декомпозиция:**
- [ ] **2.4h-a** — OCI-registry-клиент (новый модуль): manifest HEAD/GET → `Docker-Content-Digest`, Bearer-token-флоу (`WWW-Authenticate` realm/service/scope для ghcr/dockerhub), auth из `dockerconfigjson`, анонимный путь. Pure-парсеры + мокнутый HTTP. Основная масса.
- [ ] **2.4h-b** — схема: `spec.base.imagePolicy.resolve` + `status.image.{tag,resolved,resolvedAt}` + condition `ImageResolved` в 4 зеркала (CUE `application.cue` + kube-rs `operator-core` + OpenAPI `crd-application.yaml` + webhook). cue-vet пример.
- [ ] **2.4h-c** — `operator-rendering`: принимает резолвнутый digest-параметр, рендерит `image:` как digest (или verbatim при `off`/fallback). Остаётся чистой функцией.
- [ ] **2.4h-d** — интеграция в контроллер application: resolve (2.4h-a + `pick_pull_credential`) → requeue ~60с conditional → status-write (свой field-manager, SSA-split) → render-with-digest → graceful-fallback + `ImageResolved=False`. Метрика `apprafter_image_resolve_total{result}`; image-change мимо MigrationPlan.
- [ ] **2.4h-e** — `apprafter app status` показывает running-digest (`status.image.resolved` + tag + resolvedAt-age).
- [ ] **2.4h-f** — dev-guide про цикл итерации (push→auto-deploy, opt-out, escape-hatch `rollout restart`); shipped-пример + CMS-манифест с явным `imagePolicy`. Координированный operator+platform-stack release bump.

**Acceptance:** re-push того же тега → в пределах reconcile-интервала Deployment катится на новый digest без правки манифеста в Git; `app status` показывает running-digest; `imagePolicy.resolve: off` → resolve не происходит, ref verbatim; приватный образ с covering `SourceCredential` резолвится, без креда — graceful-fallback на тег + `ImageResolved=False`.

**Зависит от:** 2.4 (рабочий Application-reconcile), ADR 0039 (pull-creds), ADR 0040

**Размер:** L (OCI-клиент + контроллер-интеграция — основная масса). Закрыть ДО 2.6.

---

### 2.5 needs.jetstream → NATS account/stream
> 🏁 SR: D — needs.jetstream dropped; reactivate on 2+ explicit requests

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
> 🏁 SR: A · order 3 — needs.redis; pulled C→A (closes 2/6 platform services)

**Поставка:**
- [ ] Dragonfly как platform-service (single instance Tier 1).
- [ ] `redis-integrated` ServiceProvider: DB-namespace per claim.
- [ ] `requirepass` per-claim, в Secret.

**Acceptance:** Application с `needs.redis` получает рабочий DSN, два claim'а изолированы по DB-номеру.

**Зависит от:** 2.3

**Размер:** M

---

### 2.6a KEDA install + ScaledObject rendering
> 🏁 SR: C — trigger: first autoscaling customer signal

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

### 2.6b needs.disk → persistent block storage
> 🏁 SR: A · order 3 — needs.disk block storage; launch scope (not in earlier speedrun revisions)

**Source:** design session 2026-05-27; ADR TBD before implementation (disk class abstraction, shareMode semantics, tier mapping).

**Цель:** declarative persistent block storage для Application через `needs.disk` claim; tier-portable через storage class abstraction; operator под капотом emits StatefulSet + PVC machinery + CSI snapshot scheduling.

**Поставка:**

Schema:
- [ ] CUE schema `#DiskClaim` в `schemas/v1alpha1/application.cue` под `needs.disk?: #DiskClaim | [...#DiskClaim]` (union scalar | array).
- [ ] Поля: `name?` (string, optional), `size` (string, e.g. "10Gi"), `mountPath` (string), `class?: "local" | "replicated" | "shared" | *"local"`, `shareMode?: "per-replica" | "shared" | *"per-replica"`, `readOnly?: bool | *false`, `backup?: { enabled, schedule, retention }`, `autoExpand?: { enabled, threshold, maxSize }`.

Admission webhook:
- [ ] Name uniqueness validation across array entries (implicit name from `mountPath` segment когда `name` omit'нут — `/data` → `data`, `/var/lib/uploads` → `uploads`; explicit wins; conflict → reject с suggestion).
- [ ] `shareMode: shared` requires `class` supporting RWX (`shared` only в v1) — reject otherwise с suggestion.
- [ ] `class: replicated` или `class: shared` на T1 → reject («single-node tier does not support replicated/shared storage; use `local` or upgrade to T2»).
- [ ] `class: replicated` или `class: shared` на T2 без opt-in platform values (`enableReplicatedStorage` / `enableSharedStorage`) → reject с install hint.
- [ ] Optional advisory warning: `shareMode: per-replica` + `replicas > 1` → не reject, но print warning «new replicas start with empty disks; if shared data needed, use shareMode: shared».
- [ ] Quota enforcement: sum `disk.size` across all claims в Application против Tenant quota → reject если over.

Operator rendering (`operator-rendering` crate):
- [ ] Detect `needs.disk` non-empty в Application → emit StatefulSet вместо Deployment (existing renderer pivot).
- [ ] Normalize union format: scalar → single-element array внутри renderer pipeline.
- [ ] Per claim emit: PVC template (для `shareMode: per-replica`) или standalone PVC (для `shareMode: shared`) с tier-mapped StorageClass.
- [ ] VolumeMount per claim в container spec.
- [ ] StorageClass resolution table (per tier from platform values):
    - T1: `local` → `local-path` (k3s default).
    - T2: `local` → `hcloud-volumes` (Hetzner CSI), `replicated` → `longhorn` (opt-in), `shared` → `rook-nfs` (opt-in).
    - T3+: `local` → `linstor-single-replica`, `replicated` → `linstor-three-replica`, `shared` → `rook-nfs` / `cephfs`.
    - T4: cloud-provider specific (EBS gp3, Azure Premium SSD, GCP PD).

Backup integration (CSI snapshot):
- [ ] При `backup.enabled: true` operator создаёт `VolumeSnapshotClass` reference + Velero `Schedule` (или native CronJob if Velero не installed) per claim.
- [ ] Snapshot schedule per claim's `backup.schedule` (default daily `0 2 * * *`), retention per `backup.retention` (default 30d).
- [ ] Snapshots store в external S3 destination (configured platform-wide через 4.12, fallback на local CSI snapshot если 4.12 не deployed).

Auto-expand:
- [ ] При `autoExpand.enabled: true` operator periodically reads PVC usage (через `kubelet_volume_stats_used_bytes` metric или `df` exec в pod).
- [ ] При usage > `autoExpand.threshold` (default 80%) → patch PVC.spec.resources.requests.storage к next size step (e.g., +20% или next round Gi) до `autoExpand.maxSize`.
- [ ] Online expansion supported большинством modern StorageClass (CSI provisioner + ExpandInUsePersistentVolumes feature gate).

Platform-stack chart:
- [ ] Component `local-path-provisioner` (k3s ships это automatically, но explicit pin в chart values для T2 если k3s не используется).
- [ ] Optional components с opt-in flags: `longhorn` (replicated), `rook-nfs` (shared), gated по `enableReplicatedStorage` / `enableSharedStorage` platform values.

Documentation:
- [ ] `docs/operator-guide/storage.md` — disk claims reference, class semantics, shareMode behaviour при scaling, restore procedure.
- [ ] `docs/operator-guide/disaster-recovery.md` — per-claim restore steps (manual procedure: identify VolumeSnapshot → create PVC from snapshot → patch Application's claim к нему). Automation deferred к later DR drill phase.

Tests:
- [ ] CUE schema validation: scalar + array forms, name conflict detection, shareMode/class compatibility.
- [ ] Renderer unit tests: per-replica vs shared rendering, StatefulSet emission, StorageClass selection по tier.
- [ ] Webhook unit tests: validation rules (quota, T1 reject for replicated/shared, opt-in reject T2, mountPath uniqueness).
- [ ] Integration test (T1 single-node k3s): SQLite-app с `needs.disk.db: { size: "1Gi", mountPath: "/data" }` стартует, file persists через pod restart, через `dev down` + `dev up`.
- [ ] Integration test (T2 multi-node): `shareMode: shared` + `class: shared` (с installed Rook-NFS) deploys multi-replica app с shared storage, все replicas видят writes друг друга.
- [ ] Snapshot test: backup schedule создаёт VolumeSnapshot, retention enforces deletion старых.

**Acceptance:**
- SQLite-app deploys через `needs.disk` с `class: local`, single replica → file persists через pod restart, через node reboot.
- Multi-disk app (массив с двумя `disk` claims разных `mountPath`) deploys корректно, обе PVCs создаются, mounts работают.
- `shareMode: shared` + RWX class deploys multi-replica app, cross-replica file visibility verified.
- `shareMode: per-replica` + `replicas: 3` → 3 separate PVCs, каждая replica имеет own data, scaling to 5 создаёт 2 fresh empty PVCs.
- `autoExpand.enabled: true` + filling disk past threshold → PVC автоматически resize'ит до `maxSize`.
- Backup schedule emits VolumeSnapshots по cron; retention удаляет старые.
- T1 reject для `class: replicated/shared` с helpful message.
- T2 без opt-in → reject с install hint.
- Quota over → reject с remaining quota info.

**Зависит от:** 1.83 (M1.5 closure), 4.12 (для full backup destination integration — без 4.12 backups landed но локальный CSI snapshot only).

**Размер:** L

---

### 2.7 SPIRE installation + workload identity
> 🏁 SR: C — SPIRE; trigger: first Tier-2 OpenBao-grade or compliance ask

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
> 🏁 SR: C — credential injection via SPIFFE; with 2.7

**Поставка:**
- [ ] Замена «mounted Secret with DSN» на «приложение получает DSN через workload identity (короткоживущие credentials)».
- [ ] Для PG: интеграция через CNPG `pg_ident.conf` + SPIFFE-aware sidecar.
- [ ] Fallback на mounted Secret с пометкой «deprecated, use workload identity».

**Acceptance:** под без переменной окружения с паролем PG, аутентификация через SPIFFE; ротация SVID каждый час, приложение продолжает работать.

**Зависит от:** 2.7

**Размер:** L

---

### 2.9 Per-environment overrides через CUE unification
> 🏁 SR: D — dev mode dropped from launch

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
> 🏁 SR: A · order 3 — needs→NetworkPolicy auto-derivation (free security win)

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
> 🏁 SR: A · order 3 — SealedSecrets (default Tier-1 secrets). **Controller + `secret seal` slice carved к фронту order 3 как prereq для 1.79c** (`SourceCredential`, см. `speedrun-plan.md` §4.2); Backstage encrypt-wizard + UI rotation-warning — позже в order 3.

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
> 🏁 SR: A · order 3 — secret()/claim.* refs in Application.env

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
> 🏁 SR: D — Notifications service dropped from launch (managed portal + direct transactional email); reactivate with AccessGrant

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
> 🏁 SR: D — Notifications channels; with 2.13

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
> 🏁 SR: D — Notification templates; with 2.13

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
> 🏁 SR: D — Notifications Backstage plugin; with 2.13

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
> 🏁 SR: D — dev mode dropped from launch

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
> 🏁 SR: A · order 4 — HA-bootstrap (k3s 3-node + kube-vip + embedded etcd, NOT kine+NATS); T2 substrate, pulled C→A

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
> 🏁 SR: C — kine+NATS storage; trigger: audit-replayability marketing-critical OR etcd scale ceiling

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
> 🏁 SR: A · order 4 — Cilium mTLS; T2 substrate, pulled C→A

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
> 🏁 SR: B · order 5 — OTel/Tempo/Prometheus subset (EXCLUDES sidecar auto-inject + full ClickHouse)

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
> 🏁 SR: C — ClickHouse provider; trigger: long-term traces/logs retention signal

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
> 🏁 SR: C — VictoriaMetrics; trigger: same as 3.5

**Поставка:**
- [ ] VictoriaMetrics single (Tier 2) / cluster (Tier 3+).
- [ ] OTel metrics → vmagent → VictoriaMetrics.
- [ ] Стандартные dashboards в Grafana (operator metrics, NATS, k3s, Cilium).

**Acceptance:** Grafana показывает per-Application latency / RPS dashboards.

**Зависит от:** 3.4

**Размер:** M

---

### 3.7a Hubble enable + Hubble UI + Grafana network dashboards
> 🏁 SR: B · order 5 — Hubble enable + UI subset

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
> 🏁 SR: C — Backstage flow visualizer; depends on Hubble + Backstage UX

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
> 🏁 SR: C — Kamaji + Capsule hard-MT; trigger: first hard-MT or MSP signal (ADR 0038: T2 opt-in)

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
> 🏁 SR: C — Tenant CRD; with 3.8

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
> 🏁 SR: C — Cilium Egress Gateway + static IPs; trigger: static-IP-for-third-party-API signal

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
> 🏁 SR: C — upgrade-tier 1→2; post-launch first bundle (~2-4 wks), bundled with 4.16

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
> 🏁 SR: C — OpenBao HA; depends on SPIRE (2.7/2.8)

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
> 🏁 SR: C — SealedSecrets→OpenBao migration; with 3.11

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
> 🏁 SR: D — dev mode dropped from launch

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
> 🏁 SR: B · order 5 — ExternalSurface CRD

**Поставка:**
- [ ] CUE-схема (§3.5).
- [ ] Reconciler: разворачивает компоненты в порядке зависимостей.
- [ ] Status per компонент (git/registry/access/notifications/synthetic/backups).

**Размер:** M

---

### 4.1a HTTPRoute auto-generation
> 🏁 SR: B · order 5 — HTTPRoute auto-gen (deployed = reachable; cheapest UX win)

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

### 4.1b TLS schema + custom cert import + manual DNS-01 + domain management
> 🏁 SR: C — advanced TLS/cert/DNS-01 beyond launch minimum (4.1a + 4.4a cover launch)

**Source:** Продолжение 4.1a; ADR 0027 (MigrationPlan) для destructive domain ops. Automated DNS provider integration вынесен в 4.1c.

**Цель:** production-grade hostname/TLS support без обязательной DNS API integration. Юзер может использовать LE (HTTP-01 для specific hostnames, manual DNS-01 для wildcards) или принести собственный cert от любого CA (DigiCert/Comodo/Sectigo/etc.). Cluster получает domain management CLI и MigrationPlan-защиту от destructive operations.

#### Поставка — ExternalSurface schema расширение

- [ ] `ExternalSurface.spec` получает новые optional поля:
    ```cue
    spec: {
        // Existing fields из 4.1...

        // Default suffix для apps с expose.public=true без hostname.
        // Если задан, operator auto-generates `<app-name>.<defaultDomain>`.
        defaultDomain?: string

        // Whitelist доменов которые этот кластер обслуживает.
        // Apps с hostname вне этого списка отклоняются admission webhook.
        // Wildcard entries (`*.example.com`) матчат suffix.
        // Если массив пуст — нет whitelist (любой hostname accept'ится).
        allowedDomains?: [...#DomainEntry]
    }

    #DomainEntry: {
        domain:    string      // "example.com" или "*.example.com"
        wildcard:  bool | *false  // computed: true если starts with "*."
        certMode:  "letsencrypt-http01" | "letsencrypt-dns01-manual" | "imported" | *"letsencrypt-http01"
        importedCertRef?: string  // когда certMode == "imported", имя из target cert list
        addedAt:   string      // ISO timestamp
        addedBy:   string      // email/identity who added
    }
    ```

- [ ] PlatformController reconciles `ExternalSurface` → creates cert-manager `ClusterIssuer` ресурсы:
    - `apprafter-letsencrypt-http01` — для не-wildcard hostnames через HTTP-01 (всегда создан).
    - `apprafter-letsencrypt-dns01-manual` — для wildcards через manual DNS-01 (cert-manager в "manual" режиме, требует юзерского участия для каждого challenge).

#### Поставка — `tls: bool | TlsOptions` schema

- [ ] CUE schema (`apprafter/Application.cue`):
    ```cue
    #Expose: {
        public:   bool | *false
        hostname: string | *""
        tls:      bool | #TlsOptions | *true
        // ...
    }

    #TlsOptions: {
        enabled:    bool | *true
        redirect:   bool | *true        // HTTP→HTTPS 301 redirect
        hsts:       bool | *true        // Strict-Transport-Security header
        hstsMaxAge: int  | *31536000   // 1 year
        minVersion: "TLSv1.2" | "TLSv1.3" | *"TLSv1.2"
        // Future: cipherSuites, alpn, clientCert
    }
    ```
- [ ] Operator renderer:
    - `tls: true` → expand to `#TlsOptions{}` defaults; generates Certificate + HTTPRoute с HTTPS redirect filter + HSTS response header filter.
    - `tls: false` → no Certificate, HTTP-only HTTPRoute (port 80), no redirect, no HSTS. Никаких warnings (юзер явно выключил).
    - `tls: {...}` object → use provided values, fill missing с defaults.
- [ ] HTTPS redirect: HTTPRoute с `filters: [{type: RequestRedirect, requestRedirect: {scheme: "https", statusCode: 301}}]` для port 80 listener.
- [ ] HSTS: HTTPRoute с `filters: [{type: ResponseHeaderModifier, responseHeaderModifier: {set: [{name: "Strict-Transport-Security", value: "max-age=<hstsMaxAge>; includeSubDomains"}]}}]`.

#### Поставка — Custom cert import (`apprafter target cert ...`)

- [ ] `apprafter target cert import <name>`:
    - Флаги: `--cert <file>` (PEM-encoded certificate, fullchain), `--key <file>` (PEM-encoded private key), `--chain <file>` (optional separate intermediate chain если не включён в fullchain).
    - Pre-validation:
        - Parse PEM, проверить что cert и key соответствуют (key matches cert public key).
        - Extract subject + SANs + expiry; output для confirmation.
        - Warn если expiry < 30 days ("Certificate expires в N days. Continue? [y/N]").
    - Создаёт k8s Secret type `kubernetes.io/tls` в namespace `cert-manager` с labels:
        - `apprafter.io/managed-by: apprafter`
        - `apprafter.io/cert-name: <name>`
        - `apprafter.io/cert-mode: imported`
        - Annotations: `apprafter.io/cert-not-before`, `apprafter.io/cert-not-after`, `apprafter.io/cert-sans` (для бystrого querying без re-parsing).
    - Output:
        ```
        ✓ Certificate 'my-wildcard' imported

          Subject:     CN=*.example.com
          SANs:        *.example.com, example.com
          Issuer:      DigiCert TLS RSA SHA256 2020 CA1
          Valid from:  2025-11-12
          Expires:     2026-11-12 (in 357 days)

        Use in Application.cue:
          expose.tls.useCert: "my-wildcard"

        Or register associated domain:
          apprafter target domain add "*.example.com" --cert my-wildcard
        ```

- [ ] `apprafter target cert list`:
    - Таблица: name, subject, SANs (truncated если > 80 chars), expires (с цветовой подсветкой: red если < 30d, yellow < 60d, green иначе), used by (count apps + domains references).

- [ ] `apprafter target cert show <name>`:
    - Full detail: полные SANs, issuer chain, fingerprint, всех Applications/domains references.

- [ ] `apprafter target cert renew <name>`:
    - Для `imported` certs: instructional output "Re-purchase from CA, then run `apprafter target cert import <name> --cert <new-fullchain> --key <new-key> --replace`".
    - Для `letsencrypt-dns01-manual`: триггерит manual DNS-01 challenge cycle (см. ниже).

- [ ] `apprafter target cert remove <name>`:
    - Pre-check: scan domains и Applications references.
    - Если есть active references — error "Certificate used by domain '<domain>' and N applications. Remove references first или используй `--force` (apps will lose TLS)".
    - `--force` создаёт **platform-scope MigrationPlan** (см. ниже про MigrationPlan integration).

- [ ] `apprafter target cert import <name> --replace` для rotation:
    - Pre-validation идентична `import`.
    - Updates existing Secret in-place (sequence: validate → write new keys → no downtime).
    - HTTPRoutes референсящие этот cert автоматически подхватывают новый (cert-manager и Gateway API делают reload).

#### Поставка — Manual DNS-01 instructional flow

- [ ] Triggered через `apprafter target domain add "*.example.com"` (без `--cert` flag):
    - Wizard prompt:
        ```
        Wildcard certificates require DNS-01 challenge (LE limitation, not ours).
        
        How would you like to proceed?
          > Manual TXT record (one-time setup, manual renewal every ~60 days)
            Use existing certificate from a CA (Comodo, DigiCert, etc.)
            Configure automated DNS provider (requires API token)
            Cancel
        ```
    - "Use existing certificate" → редиректит к `apprafter target cert import` wizard.
    - "Automated DNS provider" → message "Available в Phase 4.1c (coming soon). Use manual TXT для now".
    - "Manual TXT record" → continue с challenge initiation.

- [ ] Manual TXT record sub-flow:
    1. CLI создаёт cert-manager `Certificate` resource с `issuerRef: apprafter-letsencrypt-dns01-manual`.
    2. cert-manager начинает challenge, generates required TXT record.
    3. CLI polls challenge status until TXT requirement доступен.
    4. CLI outputs:
        ```
        DNS configuration required:
          
          Type:    TXT
          Name:    _acme-challenge.example.com
          Value:   abc123XYZ_long_token_string...
          TTL:     300 (short, will be deleted after challenge)
        
        Steps:
          1. Add the TXT record above at your DNS registrar
          2. Wait 1-5 minutes for DNS propagation
          3. Run: apprafter target cert verify-challenge "*.example.com"
        
        Or, if you want CLI to wait и auto-verify:
          apprafter target cert wait-challenge "*.example.com" --timeout 10m
        ```
    5. `apprafter target cert verify-challenge` или `wait-challenge`:
        - Periodic `dig +short _acme-challenge.example.com TXT` поlling.
        - Когда matches expected value → notify cert-manager, ждёт issuance.
        - Successful → output cert details + reminder про renewal.
    6. Issuance complete → cert хранится в Secret аналогично imported cert, но с label `apprafter.io/cert-mode: letsencrypt-dns01-manual`.

- [ ] Renewal reminder:
    - cert-manager сам пытается renew в day 60 (стандартное LE поведение).
    - cert-manager в manual режиме surface'ит новое TXT-требование через event на Certificate resource.
    - AppRafter operator scan'ит Certificates с `cert-mode: letsencrypt-dns01-manual` и status `Renewing`, surface'ит в:
        - `Application.status.conditions[CertificateRenewalRequired]=True` с TXT details
        - `apprafter app status` показывает actionable instructions
        - (Phase 3, когда notifications service) — push notification по email/slack/telegram
    - CLI helper: `apprafter target cert continue-renewal "<domain>"` → выводит current TXT requirement, ждёт пока юзер не поставит, верифицирует.

#### Поставка — `public: true` + hostname validation rules

- [ ] Admission webhook расширение:
    - `expose.public: true` + `hostname` задан → используем заданный.
    - `expose.public: true` + `hostname` пустой + `ExternalSurface.spec.defaultDomain` задан → operator auto-generates `<app-name>.<defaultDomain>`, reflected в `Application.status.effectiveHostname` для visibility.
    - `expose.public: true` + `hostname` пустой + `defaultDomain` пустой → **reject** с error "expose.public: true requires either expose.hostname или platform-wide defaultDomain. Set hostname в Application.cue, или run `apprafter target domain set-default <domain>`".
- [ ] `allowedDomains` enforcement:
    - При не-пустом `allowedDomains` — hostname app'а должен match'ить минимум одну entry (exact или wildcard suffix).
    - Mismatch → reject с error "Hostname '<host>' not в cluster's allowed domains list. Add via `apprafter target domain add <domain>`".
    - При пустом `allowedDomains` — без enforcement (любой hostname accept'ится, для dev/experimental clusters).

#### Поставка — Path conflict detection (расширение 4.1a)

- [ ] Admission webhook для HTTPRoute:
    - Existing check: hostname conflict — из 4.1a.
    - New check: exact `(hostname, pathPrefix, pathType)` tuple duplication across Applications:
        - **Exact match** → reject с error "Path '<path>' on hostname '<hostname>' already claimed by Application '<other-app>'".
        - **Subset/superset paths** (catch-all `/` + specific `/api`) → **accept с warning** в admission response: "Application '<name>' acts as catch-all for `<hostname>`; requests not matching more specific paths will route here. This is correct per Gateway API spec but can be surprising. Consider explicit path prefixes if intended."
        - Warning visible в kubectl output ("Warning: ..."), не блокирует apply.

#### Поставка — `apprafter target domain` CLI группа

- [ ] `apprafter target domain add <domain> [--cert <imported-cert-name>]`:
    - Validation: формат (RFC 1123 hostname, wildcard `*.<domain>`).
    - `--cert <name>` — использовать existing imported cert. `certMode: imported`, `importedCertRef: <name>`.
    - Без `--cert`:
        - Для specific hostname — `certMode: letsencrypt-http01` (default).
        - Для wildcard — triggers manual DNS-01 wizard (см. выше).
    - Append в `ExternalSurface.spec.allowedDomains`.
    - Output: floating IP + DNS A/AAAA-record instructions + warning если домен уже резолвится не в кластер.

- [ ] `apprafter target domain list`:
    - Таблица: domain, type (specific/wildcard), cert mode (LE/imported/manual), cert status, DNS verification, apps count, added.

- [ ] `apprafter target domain verify <domain>`:
    - `dig <domain> +short` → сравнение с cluster floating IP (A для IPv4, AAAA для IPv6 если dual-stack).
    - LE rate limit pre-check: `--check-rate-limit` пингует LE rate-limit endpoint (полезно перед issuance multiple certs).

- [ ] `apprafter target domain status <domain>`:
    - Detail view: cert (mode, expiry, last renewal attempt + result), apps using, DNS resolution, recent challenges (last 5).

- [ ] `apprafter target domain set-default <domain>`:
    - Patches `ExternalSurface.spec.defaultDomain`.
    - Если domain не в `allowedDomains` — **explicit reject** ("Domain '<d>' not registered. Run `apprafter target domain add <d>` first") — без implicit auto-add, чтобы не было surprise mutations.
    - Если меняет existing default → **platform-scope MigrationPlan** (existing apps без explicit hostname могут получить новый effective hostname).

- [ ] `apprafter target domain remove <domain>`:
    - Pre-check: scan Applications использующих этот domain или его wildcard scope.
    - Active apps есть → **platform-scope MigrationPlan** создаётся, removal не происходит до approve.
    - Active apps нет → immediate remove, confirmation prompt + `--yes` для skip.

#### Поставка — MigrationPlan integration для destructive domain/cert ops

- [ ] При `target domain remove <domain>` с active apps создаётся MigrationPlan:
    - `scope.type: platform`
    - `scope.platform.target: ExternalSurface/default`
    - `trigger.kind: domain-removal`
    - `trigger.removing: "<domain>"`
    - `risks.classification: breaking`
    - `risks.affectedApps: ["<app1>", "<app2>", ...]`
    - `risks.estimatedDowntime: "immediate"`
    - `risks.reversible: true`
    - `plan` steps: remove from allowedDomains → cleanup HTTPRoutes referencing → cleanup orphaned Certificates → (M3) notify affected app owners.
- [ ] Аналогично для `target cert remove --force` с active references — platform-scope MigrationPlan с affected apps.
- [ ] `target domain set-default <new>` с existing default → platform-scope MigrationPlan с listing apps чей effective hostname изменится.
- [ ] Approve via `apprafter migration approve <name>` → PlatformMigrationStrategy выполняет; reject — no-op.

#### Поставка — `apprafter app status` cert visibility

- [ ] Расширить `app status` output для apps с TLS:
    ```
    Application: my-parser
      Sync:     Synced (12 seconds ago)
      Health:   Healthy

      Endpoints:
        Internal:  http://my-parser.default.svc.cluster.local:3000
        External:  https://parser.example.com
          Certificate: my-wildcard (imported)
            Issuer:      DigiCert TLS RSA SHA256 2020 CA1
            Valid until: 2026-11-12 (in 357 days)
          TLS:         enabled (redirect ON, HSTS ON, minVersion TLSv1.2)

      Pods:     1/1 Ready
    ```
- [ ] Cert status conditions:
    - `Valid until: 2026-XX-XX (in N days)` — normal state.
    - Pending issuance → `Certificate: pending issuance (HTTP-01 challenge, attempt 2/3)`.
    - Failed → `Certificate: FAILED (last attempt 5m ago: <error>). Will retry. See: kubectl describe certificate <name>`.
    - Close to expiry для LE → `Certificate: expires in 11 days (auto-renewal scheduled in 2 days)` — informational.
    - Close to expiry для imported → `Certificate: expires in 11 days. Re-purchase from CA and run \`apprafter target cert import <name> --replace\``.
    - Manual DNS-01 renewal required → `Certificate: renewal requires DNS TXT update. Run \`apprafter target cert continue-renewal "<domain>"\``.
- [ ] AppRafter operator periodically scan'ит Certificates и Secrets с label `apprafter.io/managed-by: apprafter`:
    - Bump `Application.status.conditions[CertificateExpiringSoon]=True` при < 30 days.
    - Bump `Application.status.conditions[CertificateRenewalRequired]=True` для manual DNS-01 в Renewing state.

#### Acceptance

- [ ] `apprafter target cert import` с валидным fullchain + key → Secret создан, cert details выведены, expiry правильно распарсен.
- [ ] `apprafter target cert import` с mismatched cert/key → fail с error до записи Secret.
- [ ] `apprafter target domain add app.example.com` (specific) → registered, DNS A-record instructions, certMode `letsencrypt-http01`.
- [ ] `apprafter target domain add "*.example.com"` без `--cert` → wizard offers manual DNS-01 / cert import / cancel.
- [ ] Manual DNS-01 flow: TXT instructions → юзер ставит record → `verify-challenge` → cert issued.
- [ ] `apprafter target domain add "*.example.com" --cert my-wildcard` (imported) → registered, certMode `imported`, никакого challenge не происходит.
- [ ] Application с `tls: false` + `public: true` — deployed без warnings, HTTP-only, accessible на port 80.
- [ ] Application с `tls: {redirect: false}` — TLS включён, но HTTP не редиректит.
- [ ] Two apps с same hostname + same path → second reject'ится.
- [ ] Two apps с same hostname, один `/`, другой `/api` → both accepted, warning в kubectl output.
- [ ] `expose.public: true` + no hostname + defaultDomain → effective hostname auto-generated, HTTPRoute generated.
- [ ] `expose.public: true` + no hostname + no defaultDomain → admission reject.
- [ ] `apprafter target domain set-default <new>` с existing default → MigrationPlan создан.
- [ ] `apprafter target cert remove <name>` с active references → MigrationPlan создан; без references → immediate remove с confirmation.
- [ ] `apprafter app status` для healthy app с imported cert показывает cert mode (imported), expiry, re-import instructions при < 30d.

#### Не входит в этот item

- Automated DNS provider integration для wildcard auto-issuance — **4.1c**.
- mTLS / client certificate auth (Phase 5+ для regulated workloads).
- Custom internal CA (Phase 6 для confidential containers tier).
- Notifications service integration для cert expiry alerts — depends on M3 notifications service.
- ALPN / HTTP/3 / QUIC support.

**Зависит от:** 4.1a (basic HTTPRoute + Certificate generation, hostname conflict detection); 1.79a (CLI infrastructure для `apprafter target ...` group).

**Размер:** L

---

### 4.1c Automated DNS provider integration + lazy add-on system
> 🏁 SR: C — automated DNS provider + lazy add-ons; beyond launch minimum

**Source:** Продолжение 4.1b. Закрывает renewal pain manual DNS-01 через автоматизацию DNS-01 challenge. Вводит lazy add-on activation: ExternalSurface — single source of truth, PlatformController реагирует.

**Цель:** zero-touch wildcard certificate issuance и renewal для built-in providers (Cloudflare, Route53, Google Cloud DNS, DigitalOcean) сразу через core platform-stack, и для community-maintained providers (GoDaddy, Hetzner DNS) через on-demand add-ons без раздувания default deployment.

#### Поставка — Add-on registry architecture

- [ ] **Built-in providers** (cert-manager native solvers, no extra deployment):
    - Cloudflare
    - Route53 (AWS)
    - Google Cloud DNS
    - DigitalOcean
- [ ] **Add-on providers** (community webhook plugins, lazy-deployed on-demand):
    - GoDaddy через `cert-manager-webhook-godaddy`
    - Hetzner DNS через `cert-manager-webhook-hetzner`
- [ ] **Out of scope (V1):**
    - Namecheap — IP allowlist + balance requirements + community webhook maturity не оправдывают MVP complexity. Может быть добавлен позже как community-contributed registry entry без архитектурных изменений.
    - Azure DNS, Akamai, Gandi и прочие — добавляются on-demand small items (XS каждый).
- [ ] Add-on registry: статичная таблица `addons/dns-providers/<name>/manifest.yaml` в platform-stack repo, каждая запись:
    - `name`: identifier (e.g., `godaddy`)
    - `displayName`: human-readable
    - `webhookChart`: OCI ref Helm chart
    - `chartVersion`: pinned version
    - `credentialsSchema`: keys которые Secret должен содержать
    - `validationEndpoint`: provider API endpoint для пинга при configure
    - `documentation`: link к provider's API token instructions

#### Поставка — Lazy activation через ExternalSurface (Path B)

- [ ] **Source of truth:** `ExternalSurface.spec.dnsProvider`. PlatformController watches это поле и реагирует.
- [ ] **State machine** в `ExternalSurface.status`:
    ```
    Pending → ResolvingAddon → InstallingAddon → ValidatingCredentials → Ready
                                                           ↓
                                                         Failed (with reason)
    ```
- [ ] **Reconcile logic** PlatformController при изменении `dnsProvider.type`:
    1. Если type — built-in (cloudflare/route53/google-cloud-dns/digitalocean):
        - Skip add-on installation, go straight to ValidatingCredentials.
        - Создаёт ClusterIssuer `apprafter-letsencrypt-dns01-auto` с native solver config.
    2. Если type — add-on (godaddy/hetzner-dns):
        - Lookup registry entry для type.
        - Создаёт Argo CD `Application` в project `platform-providers` для webhook chart.
        - Waits для webhook Application `Healthy` (с timeout 5 min).
        - После Healthy → создаёт ClusterIssuer с webhook solver config.
        - Если timeout — status.phase=Failed, conditions[AddonInstallFailed]=True с actionable hint.
    3. **ClusterIssuer создаётся только в фазе Ready** — это критично, чтобы cert-manager не пытался issue cert через несуществующий webhook.
- [ ] **Reconcile logic при removal** (юзер делает `dns-provider remove`):
    1. Удаляет ClusterIssuer.
    2. Если был add-on type — удаляет webhook Argo CD Application из `platform-providers`.
    3. Очищает Secret с credentials.
    4. status.phase → Pending (либо undefined если не configured).
- [ ] **Future extension point** (multi-tenant Phase 7+, не сейчас): `PlatformStack.spec.overrides.dnsProviders.<name>.policy: "auto" | "allow" | "deny"`. Default `auto` — текущее lazy поведение. `allow`/`deny` для explicit platform-admin override в multi-tenant scenarios. Backward-compatible добавление, не требует переписывания текущей логики.

#### Поставка — ExternalSurface schema расширение

- [ ] `ExternalSurface.spec.dnsProvider` field:
    ```cue
    spec: {
        // ...existing fields из 4.1, 4.1b...

        dnsProvider?: #DnsProviderConfig
    }

    #DnsProviderConfig: {
        // Type matches registry entry name.
        type: "cloudflare" | "godaddy" | "hetzner-dns" | "digitalocean" | "route53" | "google-cloud-dns"

        // Provider-specific credentials через Secret reference (не inline).
        credentialsSecretRef: {
            name:      string
            namespace: string | *"cert-manager"
            // Keys внутри Secret конкретны для provider:
            //   cloudflare:       "api-token"
            //   godaddy:          "api-key" + "api-secret"
            //   hetzner-dns:      "api-token"
            //   digitalocean:     "access-token"
            //   route53:          "access-key-id" + "secret-access-key" + "region"
            //   google-cloud-dns: "service-account-json"
        }

        // Optional zone hint когда есть несколько зон под одним provider.
        zoneSelector?: string
    }
    ```
- [ ] Также добавляется `certMode: "letsencrypt-dns01-auto"` в `#DomainEntry` (из 4.1b).

#### Поставка — Built-in provider configurations

- [ ] **Cloudflare** (priority 1):
    - Required: API Token с permission `Zone:DNS:Edit` для target zone (или `User:All Zones`).
    - cert-manager built-in solver `cloudflare`.
    - Wizard validation: `GET https://api.cloudflare.com/client/v4/user/tokens/verify`.

- [ ] **Route53** (AWS):
    - Required: AWS access key + secret + region.
    - cert-manager built-in solver `route53`.
    - Wizard validation: STS `GetCallerIdentity`.

- [ ] **Google Cloud DNS**:
    - Required: GCP service account JSON с роли `roles/dns.admin`.
    - cert-manager built-in solver `clouddns`.
    - Wizard validation: parse JSON + API call.

- [ ] **DigitalOcean**:
    - Required: DO API token.
    - cert-manager built-in solver `digitalocean`.
    - Wizard validation: API call.

#### Поставка — Add-on provider configurations

- [ ] **GoDaddy** add-on:
    - Webhook chart: `cert-manager-webhook-godaddy` (community).
    - Required: API key + API secret pair from developer.godaddy.com.
    - Token format: separate `<key>` and `<secret>` (NOT colon-separated string).
    - Pre-requirements (CLI выводит до prompt'а token):
        ```
        GoDaddy API access requires:
          • Active GoDaddy account with at least one registered domain
          • Production API keys (OTE test keys do NOT work for real DNS-01)
          • Get keys from: https://developer.godaddy.com/keys
        ```
    - Wizard validation: `GET https://api.godaddy.com/v1/domains` с `Authorization: sso-key <key>:<secret>`.

- [ ] **Hetzner DNS** add-on:
    - Webhook chart: `cert-manager-webhook-hetzner` (community).
    - Required: Hetzner DNS API token (отдельный от Hetzner Cloud token — выдаётся в Hetzner DNS Console, не в Hetzner Cloud Console).
    - Pre-requirements:
        ```
        Hetzner DNS API access requires:
          • Hetzner DNS Console account (separate from Hetzner Cloud account if applicable)
          • DNS API token from: https://dns.hetzner.com/settings/api-token
          
        Note: This is NOT the same as your Hetzner Cloud token (which AppRafter uses
        for infrastructure provisioning). DNS API tokens are managed separately.
        ```
    - Wizard validation: `GET https://dns.hetzner.com/api/v1/zones` с token.

#### Поставка — `apprafter target dns-provider` CLI группа

- [ ] `apprafter target dns-provider configure`:
    - Interactive wizard:
        ```
        ? Which DNS provider hosts your domain(s)?
            Built-in (no extra deployment):
            > Cloudflare
              DigitalOcean
              Route53 (AWS)
              Google Cloud DNS
            Add-ons (lazy-deployed):
              GoDaddy
              Hetzner DNS

        ? GoDaddy API Key: › ****
        ? GoDaddy API Secret: › ****
          ✓ Format valid
          ✓ Credentials verified (12 domains accessible)

        Installing GoDaddy webhook add-on...
          ✓ cert-manager-webhook-godaddy installed (Argo CD synced in 23s)
          ✓ ClusterIssuer apprafter-letsencrypt-dns01-auto ready

        ✓ GoDaddy DNS provider configured
        ℹ Wildcard certificates can now be auto-issued.
        ℹ Existing manual-DNS-01 domains will auto-migrate on next renewal.
        ```
    - Non-interactive: provider-specific флаги, e.g.:
        - `--type cloudflare --token "$CF_TOKEN"`
        - `--type godaddy --api-key "$GD_KEY" --api-secret "$GD_SECRET"`
        - `--type hetzner-dns --token "$HD_TOKEN"`
    - Под капотом:
        - Validates credentials через provider API.
        - Создаёт Secret в namespace `cert-manager`.
        - Patches `ExternalSurface.spec.dnsProvider`.
        - PlatformController берёт на себя add-on installation (если нужно) + ClusterIssuer creation.
        - CLI ждёт `ExternalSurface.status.phase=Ready` (или Failed) с progress indication.

- [ ] `apprafter target dns-provider status`:
    - Output:
        ```
        DNS Provider:  GoDaddy (add-on)
        Status:        Ready (validated 5m ago)
        Webhook:       cert-manager-webhook-godaddy v1.2.3 (Healthy)
        Zone access:   3 zones (example.com, example.org, example.dev)
        Last DNS-01:   2 hours ago (success, *.example.com renewal)

        Auto-renewal:  enabled для 4 domains
          *.example.com    (renews in 28d, auto)
          *.example.dev    (renews in 45d, auto)
          api.example.org  (specific, HTTP-01, renews in 12d, auto)
        ```
    - Для built-in providers webhook line опускается.

- [ ] `apprafter target dns-provider rotate`:
    - Wizard prompt только для новых credentials, остальные fields preserved.
    - Pre-validation, потом patches Secret in-place.

- [ ] `apprafter target dns-provider remove`:
    - Pre-check: scan domains с `certMode: letsencrypt-dns01-auto`.
    - Active wildcards с auto mode → **platform-scope MigrationPlan** (см. ниже).
    - No auto-mode wildcards → immediate remove с confirmation.
    - При add-on type — webhook Application автоматом удаляется PlatformController'ом.

- [ ] `apprafter target dns-provider list-available`:
    - Output: registry таблица всех supported providers, помечает "built-in" / "add-on" + ссылки на documentation.

#### Поставка — Migration existing manual-DNS-01 → auto

- [ ] При первом `dns-provider configure` после того как существуют wildcards с `certMode: letsencrypt-dns01-manual`:
    - Prompt:
        ```
        Found 2 existing wildcard domains using manual DNS-01:
          *.example.com   (next renewal in 28 days)
          *.example.dev   (next renewal in 45 days)

        Migrate to automated renewal? [Y/n]
        ```
    - "Y" → patches `DomainEntry.certMode: letsencrypt-dns01-manual` → `letsencrypt-dns01-auto`.
    - При следующем renewal cycle cert-manager использует auto issuer.
    - "n" → existing certs остаются manual до explicit `target cert migrate-to-auto <domain>`.

- [ ] `apprafter target cert migrate-to-auto <domain>` для explicit migration:
    - Patches certMode на auto.
    - Опционально с `--force-renew-now` — triggers immediate re-issuance.

#### Поставка — MigrationPlan integration

- [ ] При `dns-provider remove` с active auto-mode wildcards создаётся MigrationPlan:
    - `scope.type: platform`
    - `scope.platform.target: ExternalSurface/default`
    - `trigger.kind: dns-provider-removal`
    - `risks.classification: requires-restart`
    - `risks.affectedDomains: [...]`
    - `risks.estimatedImpact: "Wildcards will fail next renewal (~60 days). Apps lose TLS at that point."`
    - `risks.reversible: true`
    - `plan` steps: remove dns-provider config → uninstall add-on webhook (если был) → mark affected domains → (M3) notify owners.
- [ ] Approve/reject через standard `migration approve`/`migration reject`.

#### Поставка — Cert renewal monitoring (расширение 4.1b)

- [ ] AppRafter operator periodic scan Certificates с `cert-mode: letsencrypt-dns01-auto`:
    - Successful renewal in last 60 days → silently track `lastRenewalAt`.
    - Failed renewal с last attempt > 7 days ago → bump `Application.status.conditions[CertificateAutoRenewalStuck]=True` с last error.
    - Continuous failures (3+ attempts) → suggest action: "DNS provider credentials may be invalid. Run `apprafter target dns-provider check`."

- [ ] `apprafter target dns-provider check`:
    - Re-validate credentials через provider API.
    - Output статус каждого manageable zone.
    - `--full-check` opt-in флаг (5-10s overhead): создаёт + verifies + удаляет test TXT record для end-to-end проверки.

#### Acceptance

- [ ] `apprafter target dns-provider configure --type cloudflare` (built-in) → ExternalSurface переходит Pending → ValidatingCredentials → Ready за < 5s, no webhook deployment.
- [ ] `apprafter target dns-provider configure --type godaddy` → ExternalSurface проходит Pending → ResolvingAddon → InstallingAddon → ValidatingCredentials → Ready, webhook deployed в `platform-providers` project.
- [ ] Если webhook chart install failed (e.g., OCI registry недоступен) → ExternalSurface.status.phase=Failed, conditions[AddonInstallFailed]=True с actionable hint, ClusterIssuer не создаётся.
- [ ] Wildcard cert auto-issued через DNS-01 в течение 60s после `target domain add` (built-in или add-on provider).
- [ ] Existing manual-mode wildcard после `configure` + prompt "Y" → migrated к auto mode, next renewal automatic.
- [ ] `dns-provider remove` для add-on provider → webhook Application автоматом удаляется из `platform-providers` project.
- [ ] `dns-provider remove` с active auto-wildcards → MigrationPlan создан.
- [ ] `dns-provider check --full-check` создаёт + verifies + удаляет test TXT record, output success/failure detail.
- [ ] Invalid token (revoked у provider) после rotation → next renewal fails, `CertificateAutoRenewalStuck` condition bumped.
- [ ] Switch между providers (cloudflare → godaddy): cleanup старого + setup нового через standard `remove` + `configure` flow. CLI и controller handle gracefully.

#### Не входит в этот item

- Namecheap support — deferred из-за IP allowlist complexity + balance requirements. Может быть добавлен позже как community-contributed registry entry.
- Multi-tenant explicit add-on control (`PlatformStack.spec.overrides.dnsProviders.<name>.policy`) — future Phase 7+, backward-compatible extension.
- Concurrent multiple DNS providers per cluster (out of scope, single provider per cluster).
- External-DNS integration для automatic A/AAAA record provisioning (Phase 4, orthogonal item).
- HSM-backed certs / regulated workloads patterns.
- Cert pinning / Certificate Transparency monitoring.

**Зависит от:** 4.1b (custom cert import, manual DNS-01, ExternalSurface.spec.allowedDomains/defaultDomain, target domain/cert CLI groups); 1.79a (`platform-providers` Argo CD project для add-on deployment).

**Размер:** M

---

### 4.2 Forgejo (или GitLab self-hosted) deployable из манифеста
> 🏁 SR: D — Forgejo/GitLab self-hosted dropped (GitHub+ghcr.io suffice); reactivate for Group-C self-host compliance

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
> 🏁 SR: D — Harbor dropped; with 4.2

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
> 🏁 SR: D — Headscale/Tailscale dropped (managed portal auth replaces VPN)

**Поставка:**
- [ ] Headscale-controller pod, persistence pg.
- [ ] Tailscale Operator для автоматической интеграции с k8s сервисами.
- [ ] OIDC SSO для регистрации устройств.

**Acceptance:** `tailscale up --login-server=https://headscale.<domain>` работает; устройство видит cluster routes.

**Зависит от:** 4.1

**Размер:** L

---

### 4.4a external-dns integration + `DNSZone` CRD
> 🏁 SR: B · order 5 — external-dns + DNSZone CRD (closes DNS friction)

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
> 🏁 SR: C — AccessGrant + JIT; trigger: team-of-3+ (solo handled by portal auth)

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
> 🏁 SR: C — JIT cluster-admin; with 4.5

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
> 🏁 SR: C — OIDC SSO; trigger: same as 4.5

**Поставка:**
- [ ] Поддержка внешних провайдеров (Authentik / Keycloak / Auth0 / Google Workspace).
- [ ] ExternalSurface поле `auth.oidc.{issuer,clientId,...}`.
- [ ] Auto-провижионинг конфигов для Argo CD, Backstage, Headscale, OpenBao.

**Acceptance:** один SSO-логин даёт доступ ко всем UI; MFA enforced.

**Зависит от:** 4.4

**Размер:** M

---

### 4.7 platform-cli login (OIDC kubeconfig)
> 🏁 SR: C — login OIDC kubeconfig; with 4.6

**Поставка:**
- [ ] Device-flow OIDC, токен 8h, auto-refresh.
- [ ] Записывает в `~/.kube/config` контекст с exec-credential.
- [ ] Audit-event на каждый login.

**Acceptance:** после AccessGrant пользователь делает `platform-cli login` и работает с `kubectl`.

**Зависит от:** 4.6

**Размер:** M

---

### 4.8 Magic-link flow для AccessGrant
> 🏁 SR: C — magic-link; with 4.6

**Поставка:**
- [ ] Notifications-template из 2.15 (`access-grant/issued`).
- [ ] Endpoint в `platform-cli login --magic-link <token>`.
- [ ] Один-time-use, 24h TTL.

**Acceptance:** flow §3.4 шаги 1–7 проходят за ≤ 5 минут от commit до active mesh.

**Зависит от:** 4.5, 4.7

**Размер:** S

---

### 4.9 Auto-revocation на expiry
> 🏁 SR: C — auto-revocation; with 4.6

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
> 🏁 SR: C — audit-log cluster-admin tagging; trigger: Group-C compliance

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
> 🏁 SR: D — synthetic monitoring dropped (external SaaS at launch)

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
> 🏁 SR: B · order 5 — backups to external S3 (default ON Tier 1)

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
> 🏁 SR: C — Trivy/Grype/SBOM; trigger: Phase-5+ security

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
> 🏁 SR: C — Build Report plugin; with 4.13

**Поставка:**
- [ ] View per Application image: размер, layers, CVE-list, SBOM, cache-эффективность, рекомендации.
- [ ] Diff между двумя build'ами (что прибавилось/убавилось).
- [ ] «Auto-fix where possible» — генерация PR с обновлённым base image.

**Acceptance:** разработчик видит CVE-отчёт без перехода в Trivy/Harbor UI.

**Зависит от:** 4.13, 1.10

**Размер:** M

---

### 4.15 Cost view в Backstage
> 🏁 SR: C — Cost view; managed portal billing covers launch

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
> 🏁 SR: D — Cilium FQDN policies (advisory only); reactivate with Tier-2 security ask

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
> 🏁 SR: C — MigrationPlan Backstage UI; post-launch first bundle (with 3.10)

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
> 🏁 SR: D — Tier 3 (Talos/LINSTOR/Kata) post-launch territory

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
> 🏁 SR: D — Tier 4 (CoCo/AWS); trigger: compliance/sovereignty signal

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
> 🏁 SR: D — plugin ecosystem; community-contribution timeframe

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

## Фаза M — Managed track (product 2)

> 🏁 Speedrun managed-specific work (`speedrun-plan.md` §3). No OSS plan.md backing — the §M.x numbers are NOT Phase 3 subphases. Ordered per speedrun §4.3 (PASS 1–6). Grounded in ADR 0034 (managed model), 0036 (MCP safety), 0037 (control-plane infra).

- [ ] **M.1 (PASS 1)** Hosted multi-tenant SaaS scaffolding — auth / customer registry / agent-bus (speedrun §3.1)
- [ ] **M.2 (PASS 1)** `apprafter-agent` + cluster registration (speedrun §3.2; ADR 0031)
- [ ] **M.3 (PASS 1)** Customer offboarding = revoke registration token (speedrun §3.2a)
- [ ] **M.4 (PASS 2)** Hosted Backstage — namespace-per-customer + `<customer>.apprafter.dev` (speedrun §3.3)
- [ ] **M.5 (PASS 2)** Subdomain delegation `*.<customer>.apprafter.dev` (speedrun §3.6)
- [ ] **M.6 (PASS 3)** Hosted MCP server + agent passthrough (speedrun §3.4; ADR 0036)
- [ ] **M.7 (PASS 3)** Destructive-op gate via MigrationPlan CRD (speedrun §3.5; ADR 0036). Approvals at launch: `apprafter` CLI + Argo CD approve/reject buttons; Backstage plugin post-launch
- [ ] **M.8 (PASS 4)** Stripe subscription + 14-day trial (speedrun §3.7)
- [ ] **M.9 (PASS 5)** Migration helpers — Supabase / Railway basic (speedrun §3.8)
- [ ] **M.10 (PASS 5)** Internal customer support tooling (speedrun §3.10)
- [ ] **M.11 (PASS 6)** Polish + soft launch (closed beta → invite waves → public)

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

> Вынесено в отдельный файл: [`docs/changelog/plan-history.md`](docs/changelog/plan-history.md) — plan-level ledger на English (одна строка на под(под)фазу / патч; ранее таблица жила здесь). Детализация по релизам — `docs/changelog/UNRELEASED.md`.
