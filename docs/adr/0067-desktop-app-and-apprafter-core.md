# ADR 0067: AppRafter Desktop — the CLI's GUI twin over a shared `apprafter-core`

## Status

`Accepted` (2026-10-08). ADR for `plan.md` Phase D (sub-phase D.0).

## Context

The `apprafter` CLI is the operator's interface to the platform. Its 86 visible leaf commands
cover the target store, provisioning, bootstrap, applications, approvals, data services,
networking, platform versions, nodes, backups and restore. The project owner asked for a
desktop application for Windows, Linux and macOS that covers all of that functionality, plus a
small set of additions taken from an approved interface design: tabs per cluster, live refresh,
a notification inbox with OS notifications and a tray badge, an application lock, and a
stronger confirmation for destructive operations.

Three earlier positions bear on this:

- spec §1.1 commits to **one developer portal, Backstage**, and Appendix B lists "compete with
  Backstage as a portal" as a non-goal;
- spec §7 #6 keeps a custom portal on OneBun + Svelte/React + Tauri as a post-1.0 option, and
  §7 #10 leaves the "UI layer beyond Backstage" open;
- `plan.md` CLI-hardening B (B.7) expects a GUI confirmation gesture to live on a shared
  `apprafter-core` domain crate used by the CLI and a GUI. No such crate exists.

The CLI as it stands cannot be driven by a GUI:

- **No machine-readable output.** No command has `--json`; command logic is interleaved with
  terminal printing (`println!`, `tabled`, `indicatif`) and interactive prompts (`inquire`).
  The library target exports only `run()` and a documentation facade (`docs_api`).
- **Process-global behaviour.** Signal handlers installed by the library, a process-wide
  "interrupted" flag that is never reset, `process::exit` calls, and configuration read from
  the environment. The active target is a single pointer in the shared `config.yaml`, and most
  cluster commands can act only on it.
- **Unsafe concurrent use.** The state file is written without an atomic rename or a lock; the
  target store is written atomically but without a lock. A GUI and a terminal session running
  at once can corrupt or lose state.
- **Unix only.** The release matrix builds Linux x86_64 and macOS; library code calls `libc`
  signal APIs without a `cfg(unix)` guard, tool lookup ignores `.exe`, and the age key path is
  derived from `$HOME`.
- **Secrets at rest.** The kubeconfig and Argo CD password caches are age-encrypted, but the
  age identity sits in a file next to them under the same user, and provider tokens are stored
  in plain text with mode 0600 (ADR 0030 placed their encryption out of scope).

## Decision

### 1. Positioning

We will build **AppRafter Desktop**, a local operator console that is the **GUI twin of the
CLI**. It works on the same target store, the same clusters, and the same code. Everything the
CLI can do, the desktop can do, with two exceptions: shell completion, and the legacy `init`,
which only seeds a local state file and is superseded by `target add` followed by `apply`.

It is **not** a second portal — Backstage remains the developer portal (catalogue, templates,
self-service), and spec §1.1 and Appendix B stand. It is **not** a managed-offering client —
account login stays a stub until the managed track ships it. It does **not** replace the Argo
CD UI, which it opens on request.

### 2. `apprafter-core`

We will add a crate, `cli/apprafter-core`, that holds the whole CLI domain. The CLI and the
desktop are its two clients:

- the core never prints, prompts, exits the process, installs signal handlers, or reads the
  environment; everything the CLI reads from the environment today becomes a field of an
  explicit `Context`, which the CLI builds from the environment and the desktop from its own
  settings. Overrides that redirect credentials — `HCLOUD_TOKEN`, `APPRAFTER_AGE_KEY` and the
  SSH key variables — are read by the CLI only: inherited by a desktop process, `HCLOUD_TOKEN`
  would point every cluster tab at one provider project;
- every operation takes an explicit target. Only `target use` exists to move the CLI's
  active-target pointer; `target add`, `rename` and `remove` keep moving it as a side effect, as
  they do today, and report that to the user. Opening or switching a desktop tab never moves it;
- reads return typed reports; mutations are split into **plan → confirm → execute**, and a plan
  carries a class — reversible, bounded, or destructive — that each client turns into its own
  confirmation;
- progress and captured tool output flow through a reporter interface instead of the terminal;
- cancellation is a per-operation token, and each operation owns the helper pods it must clean
  up; the CLI binds its signals to the token;
- errors carry a stable code, help text, a cause chain and structured fields, with a
  serialisable projection for the GUI;
- cluster access goes through a `Kube` trait with **one** implementation over `kubectl`,
  preserving today's field managers and semantics; a second implementation (for example on
  kube-rs) is out of scope, and if ever introduced it replaces the first behind parity tests
  rather than running beside it;
- state and target-store writes are atomic and taken under an advisory lock.

The CLI keeps clap, its wizards and its rendering. When a command family moves into the core,
its terminal output must stay byte-identical, proven by golden snapshots taken before the move.

### 3. Coverage contract

Every visible leaf command in `docs/reference/cli/commands.json` maps to a desktop action, or
carries an explicit marker: `cli-only` (shell completion), `unavailable` (commands the CLI
itself documents as not implemented, shown the same way in the GUI) or `superseded` (`init`). A
CI gate fails when a new leaf has no mapping.

### 4. Platforms and tools

The desktop and the CLI ship for **Windows, Linux and macOS**. Windows *workstations* become
supported clients; Windows *cluster nodes* remain a non-goal (Appendix B). The in-cluster backup
runner stays a Linux-only binary: it runs only in the backup Job's container. External tools
(`kubectl`, `helm`, `restic`, `cue`, `git`, `ssh`) are **detected, not bundled**: the resolver
handles `.exe` lookup on Windows and the login-shell `PATH` on macOS, and the desktop shows
per-OS install instructions for anything missing.

### 5. Security

- The desktop locks behind **OS authentication** (Windows Hello, Touch ID or the account
  password, polkit or PAM on Linux), on start, on sleep, on idle, and on demand.
- Approvals and destructive operations require the full plan to be shown and then an **OS
  authentication gesture**. This is the GUI form of CLI-hardening B.7.
- **Nothing outside the running app can trigger an action**: no URL scheme, local server, or
  single-instance argument approves or mutates anything (ADR 0036).
- The age identity and provider tokens move into the **OS keyring** (Keychain, Credential
  Manager, Secret Service) through the core, shared by both clients. Entries are namespaced by
  target store, so a scratch store can never read or overwrite another store's secrets. New
  installs write to the keyring; existing stores migrate only on an explicit command, which
  deletes the files after a verified read-back. Where no keyring exists, files remain and
  `doctor`/`status` report it. `APPRAFTER_AGE_KEY` and `HCLOUD_TOKEN` keep working as explicit
  CLI overrides. On macOS the keyring becomes the default only once the CLI is signed, because
  Keychain access is tied to the binary's signature. This supersedes the "encryption out of
  scope" note of ADR 0030 for these two secrets only.

### 6. Repository and releases

- The desktop lives in `desktop/`, a separate Cargo workspace (Tauri 2) plus a bun package
  (React, TypeScript), licensed FSL-1.1-Apache-2.0 as platform core (ADR 0032). It depends on
  the core by path.
- It has its own version stream, `desktop/v*`, whose tags are created only by its publish
  workflow. It is a third human-chosen version number beside the CLI and the platform-stack
  chart.
- Consumers of the GitHub releases list that look for CLI releases (the CLI update check, the
  installer script, the download page) must select by tag prefix, so desktop releases cannot
  push the newest CLI release out of their window.

### 7. Delivery

The work is delivered in vertical slices: one command family at a time moves into the core, its
CLI output is proven unchanged, its desktop screens land, and a walk on a disposable cluster
verifies it. Each slice that changes the core is a CLI release.

### 8. Impact on the specification

Spec §1.1 ("one developer portal: Backstage") and Appendix B's portal non-goal stand: the
desktop is not a portal. Appendix B gains that Windows workstations are supported clients while
Windows cluster nodes remain a non-goal. §7 #10 ("UI layer beyond Backstage — TBD") is closed by
this ADR. §7 #6 is about a custom *portal* and is unaffected. The architecture edits — the UX
layer, the repository structure with `apprafter-core` and `desktop/`, and the technology stack —
are made when the desktop track closes, once both exist.

## Consequences

- The CLI gains a typed domain layer, typed errors and graceful cancellation on every OS — the
  CLI-hardening work (B.1–B.7) and future surfaces (an MCP server, a portal backend) can build on
  the same layer instead of on terminal text.
- Windows users get a CLI as a side effect of the core compiling there.
- Concurrent use of the CLI and the desktop becomes safe.
- Every slice touches the CLI and so carries a CLI release; the refactor of the largest modules
  (backup, restore) is the main regression risk and is guarded by golden snapshots.
- A third version stream and a third release workflow exist, with code-signing prerequisites on
  macOS and Windows.
- `kubectl` remains a hard prerequisite of both clients.

## Alternatives considered

- **Run the CLI binary as a sidecar with a new JSON-lines output mode.** Rejected: every print
  site still has to be rewritten; without prompts, the GUI re-implements every wizard's
  validation; CLI-hardening B.1 (confirmations read from `/dev/tty` only, no `--yes`) would make
  destructive operations unreachable from a sidecar by design; Windows has no graceful
  equivalent of SIGINT for the backup/restore cleanup path; each dashboard poll would cost an
  extra process spawn and a kubeconfig decryption on top of the `kubectl` calls themselves.
- **Hybrid: in-process reads on a new kube-rs client, writes through a sidecar.** Rejected: two
  different paths to the cluster (managed-field and field-manager behaviour already caused
  defects here), and a sidecar contract that is thrown away once everything moves in-process.
- **Extract everything first, build the GUI afterwards; or build the whole GUI on mocks first.**
  Rejected in favour of vertical slices: the first defers any verification of the pattern until
  the end; the second derives the data contract from a mock-up instead of from the data the CLI
  actually has.
- **Bundle the external tools, or download pinned tool versions on demand.** Rejected by the
  owner in favour of detection with instructions.
- **Keep secrets as they are and lock the UI only.** Rejected by the owner: the lock screen
  would promise protection the storage does not provide.
- **Put the desktop crate inside the `cli/` workspace.** Rejected: the Tauri dependency graph
  would enter the lock file the CLI release builds from, and every UI change would require a CLI
  version bump. The cost of a separate workspace — a second lock file — is covered by a gate that
  keeps shared dependency versions equal.
- **Generate IPC bindings with `tauri-specta`.** Rejected for now: release-candidate only, with a
  known code-generation break; `ts-rs` generates the types from the core's report structs.

## Risks

- **Regression while moving large modules.** Mitigated by golden snapshots of CLI output per
  family, taken before the move, and by a live walk per slice.
- **macOS Keychain and unsigned binaries.** Keychain access is tied to the code signature, so an
  unsigned CLI would prompt after every update. Mitigation: sign the macOS CLI with the same
  Developer ID as the desktop.
- **Linux OS authentication.** The candidate cross-platform library marks Linux as incomplete,
  and AppImage cannot install a polkit policy. Mitigation: a spike before the lock ships, with a
  PAM password check as the AppImage path.
- **Dependency advisories.** Tauri pulls the GTK3 binding stack on Linux, which advisory
  databases flag as unmaintained. Accepted with reasoned, documented ignores.
- **Signing prerequisites.** Apple and Windows signing credentials are owner-provided; until they
  exist, builds are unsigned CI artefacts and nothing is published.
- **Follow-up outside this ADR.** Recording *how* a MigrationPlan was approved (`approvedVia`)
  needs a CRD change and stays with CLI-hardening B.6.

## Owner

Project maintainers (AppRafter core). Amendments go through a follow-up ADR.
