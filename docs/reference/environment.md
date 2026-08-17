---
title: "Environment variables"
description: "Every environment variable the apprafter CLI reads, what it changes, its default, and where it is read."
audience: reference
status: stable
---

# Environment variables

Everything the `apprafter` binary reads from the process environment.
This page is **hand-written**, unlike the
[CLI reference](cli/index.md), which is generated from the clap tree —
only four of the variables below are declared as flag fallbacks
(`#[arg(env = …)]`); the rest are read with `std::env::var` inside
command code, or by a dependency (miette, `owo-colors`,
`tracing-subscriber`), and so are invisible to the generator.

Every row names the file and function that reads it, so a claim here
can be checked against the source without a search. Paths are relative
to `cli/`.

Precedence, wherever a variable competes with a flag or with stored
state, is stated per row — the CLI has no single global rule.

## Target store and credentials

| Variable | What it does | Default | Read at |
| --- | --- | --- | --- |
| `APPRAFTER_CONFIG_DIR` | Target-store root. Used **verbatim** — no `apprafter/` component is appended, so it can point straight at a scratch directory. Honoured ahead of `dirs::config_dir()`, which matters on macOS where that returns `~/Library/Application Support/`. An empty value is ignored. | `dirs::config_dir()/apprafter` (`~/.config/apprafter` on Linux) | `cli-core/src/target.rs`, `default_config_root()` |
| `APPRAFTER_SSH_PUBLIC_KEY` | SSH public-key **body** inline (the literal `ssh-ed25519 AAAA… you@host` string), for CI that has no key file on disk. Highest rung of the SSH-key chain: it wins over the active target's stored `ssh_key_path`. An empty value is ignored. | unset — the target's stored key path is used, and provisioning is allowed to proceed with no key at all | `cli-core/src/credentials.rs`, `resolve_hetzner_ssh_public_key()` |
| `APPRAFTER_SSH_PRIVATE_KEY` | Path to the **private** key used for every SSH round-trip to the node: `apprafter kubeconfig`'s cold fetch of the k3s kubeconfig, and `apprafter node prep`. | `$HOME/.ssh/id_ed25519` | `cli-providers/src/hetzner_cloud/kubeconfig.rs`, `default_ssh_identity_path()` |
| `APPRAFTER_AGE_KEY` | Path to the age identity that encrypts the cached kubeconfig and Argo CD password in `.apprafter/state.json`. The file is created (mode 0600) on first use if absent. Note this default is **not** derived from `APPRAFTER_CONFIG_DIR` and does not go through `dirs::config_dir()` — it is built from `$HOME` directly. | `$HOME/.config/apprafter/age.key` | `cli-core/src/secrets.rs`, `default_age_key_path()` |

## Provisioning

| Variable | What it does | Default | Read at |
| --- | --- | --- | --- |
| `APPRAFTER_SERVER_TYPE` | Server-type SKU (e.g. `cx22`) for non-interactive provisioning. **Lowest** rung of the chain: `--server-type` flag → manifest `nodes[0].kind` → saved state → target preference → this variable. There is no built-in default below it — a provision with no rung set fails with `apprafter::provider::server_type_not_selected`. An empty value counts as unset. | unset | `platform-cli/src/commands/apply.rs`, `env_server_type()` |
| `APPRAFTER_MANIFEST` | Path to an `Infrastructure.cue` manifest whose values overlay `apply`'s built-in defaults (region, node kind, network ranges). Resolved **relative to the current directory**, not to the state file. | unset — `apply` uses its built-in defaults | `platform-cli/src/commands/apply.rs`, `run()` |
| `APPRAFTER_HCLOUD_BASE_URL` | Hetzner Cloud API base URL. Exists so integration tests can point the CLI at a local `mockito` server; it is not a production knob. | the upstream Hetzner Cloud API | `platform-cli/src/commands/hcloud.rs`, `hcloud_base_url()` |

## Backup and restore credentials

These are read only by the backup and restore verbs. They are **not**
clap `env` declarations — the generated pages mention them inside the
help text of `--passphrase` and `--credential-file`, which is why they
do not appear there as `Env:` rows. The full workflow is in
[Backup & restore](../operator-guide/backup-restore.md).

| Variable | What it does | Default | Read at |
| --- | --- | --- | --- |
| `RESTIC_PASSWORD` | The restic repository passphrase. Two independent uses: (1) fallback for `--passphrase` on [`backup create`](cli/backup.md#apprafter-backup-create), [`backup list`](cli/backup.md#apprafter-backup-list) and [`restore`](cli/restore.md) against a local repo — flag first, then this variable, then an interactive prompt on a TTY; (2) a **required** key of the operator credential set below. | unset — prompted for on a TTY, an error otherwise | `platform-cli/src/commands/backup.rs`, `run_backup()` / `run_backup_list()`; `platform-cli/src/commands/restore.rs`, `run_restore()` |
| `S3_ACCESS_KEY_ID` `S3_SECRET_ACCESS_KEY` `S3_REGION` | Operator-side S3 credentials for a remote (`s3:`/`b2:`/`gs:`/`azure:`) restic repo, used by [`backup check`](cli/backup.md#apprafter-backup-check), [`backup prune`](cli/backup.md#apprafter-backup-prune), [`backup unlock`](cli/backup.md#apprafter-backup-unlock) and [`restore`](cli/restore.md). Consulted only when `--credential-file` is absent; the file, when given, wins outright. `S3_ACCESS_KEY_ID`, `S3_SECRET_ACCESS_KEY` and `RESTIC_PASSWORD` are required, `S3_REGION` optional. These creds are read locally and never from the cluster. | unset | `platform-cli/src/commands/backup.rs`, `resolve_operator_s3_creds()` |
| `AWS_ACCESS_KEY_ID` `AWS_SECRET_ACCESS_KEY` `AWS_DEFAULT_REGION` | Accepted **aliases** for the three `S3_*` names above, normalised to the canonical `S3_*` form on read. If both a canonical name and its alias are set, the canonical one wins. | unset | `platform-cli/src/commands/backup.rs`, `normalize_s3_creds()` |

[`backup enable`](cli/backup.md#apprafter-backup-enable) is the
exception: it takes credentials from `--credential-file` or from an
existing in-cluster Secret named by `--credential`, and does **not**
fall back to the environment.

## Tooling, output and diagnostics

| Variable | What it does | Default | Read at |
| --- | --- | --- | --- |
| `CUE_BIN` | Path to the `cue` binary the CLI shells out to. Used by [`app validate`](cli/app.md#apprafter-app-validate) and by the manifest parse inside [`app add`](cli/app.md#apprafter-app-add), and by the `Infrastructure.cue` parse that `APPRAFTER_MANIFEST` triggers. For a custom or non-`PATH` install. | `cue`, resolved through `PATH` | `cli-core/src/cue.rs`, `cue_bin()`; `platform-cli/src/commands/app_validate.rs`, `cue_bin()` |
| `RUST_LOG` | `tracing-subscriber` filter directive. Logs go to **stderr**, so raising the level does not corrupt a piped `apprafter kubeconfig` or `argocd-password`. `RUST_LOG=apprafter=debug` is the useful setting; it is what surfaces why the version-check banner stayed quiet, for example. | `warn,apprafter=info,cli_core=info,cli_state=info,cli_providers=info` | `cli-core/src/logging.rs`, `init()` |
| `NO_COLOR` | Set it (see [no-color.org](https://no-color.org/)) and every styled string drops its ANSI escapes — `doctor` rows, `bootstrap-all` phase markers, `target list`, `whoami`, and miette's rendered errors. Colour is also dropped automatically when the stream is not a TTY, so a plain pipe needs no opt-out. | unset — colour when the stream is a terminal | `cli-core/src/style.rs` (via `owo-colors`' `supports-colors`), and miette |
| `RUST_BACKTRACE` | `RUST_BACKTRACE=1` restores backtraces in rendered errors. They are off by default because the miette `help:` line is more actionable for an operator; this is a development knob. | unset — no backtrace, just the diagnostic block | `platform-cli/src/lib.rs`, `run()` (miette handler) |
| `USER` | Stamped as the `addedBy` attribution on [`target domain add`](cli/target.md#apprafter-target-domain-add) when `--added-by` is omitted. Falls back to the literal `unknown`. | unset — recorded as `unknown` | `platform-cli/src/commands/target_domain.rs`, `resolve_added_by()` |

## Declared through clap

These four are flag fallbacks. They are also documented on the
generated pages, in the flag table of the flag that owns them, as
`Env: <NAME>` — that is the authoritative place, because it is
projected from the parser.

| Variable | Owning flag | Notes |
| --- | --- | --- |
| `HCLOUD_TOKEN` | [`target add --token`](cli/target.md#apprafter-target-add) | Also read directly, one rung below an explicit `--token`, by the credential chain every Hetzner-touching command uses (`apply`, `destroy`, `import`, `kubeconfig`, `node prep`, `target machine`, `target firewall`) — so it **wins over the token saved in the target store**. Prefer the flag or the target store interactively; the env fallback exists for CI. Declared `hide_env_values`, so clap never prints the value in help. |
| `APPRAFTER_SSH_PUBLIC_KEY_PATH` | [`target add --ssh-key`](cli/target.md#apprafter-target-add) | A **path**; the CLI reads the file's body. Contrast `APPRAFTER_SSH_PUBLIC_KEY` above, which carries the body itself. Also prefills the interactive wizard, which labels the source so you can see where the value came from. |
| `APPRAFTER_NO_PING` | `--no-ping` on [`whoami`](cli/whoami.md), [`doctor`](cli/doctor.md), [`target add`](cli/target.md#apprafter-target-add) and [`target machine`](cli/target.md#apprafter-target-machine) | Skips the Hetzner API validation ping. Parsed by clap's `BoolishValueParser`, so the value must be one of `y` `yes` `t` `true` `on` `1` (enable) or `n` `no` `f` `false` `off` `0` (disable), case-insensitive. Any other value — including the empty string — is a parse **error**, not a silent no-op. |
| `APPRAFTER_REPO_TOKEN` | `--token` on [`repo creds add`](cli/repo.md#apprafter-repo-creds-add) and [`repo creds rotate`](cli/repo.md#apprafter-repo-creds-rotate) | Git-provider PAT for a private repo. Declared `hide_env_values`. Without it and without the flag, an interactive shell prompts with masked entry; a non-interactive one errors. |

## Not read: `KUBECONFIG`

`apprafter` does **not** read `KUBECONFIG`. Every cluster-touching
command decrypts the kubeconfig cached in `.apprafter/state.json`,
writes it to a temporary file, and sets `KUBECONFIG` on the `kubectl` /
`helm` subprocesses it spawns. Exporting `KUBECONFIG` in your shell
therefore changes nothing about which cluster `apprafter` talks to —
switch clusters with `apprafter target use <name>`, and pipe the
kubeconfig out with `apprafter kubeconfig` when you want `kubectl`
itself pointed somewhere.

## Not product surface

These exist in the source and are deliberately **not** supported
knobs. They drive the end-to-end harness, may change or disappear in
any release, and none is covered by a compatibility promise. They are
listed here so that finding one in the source does not read as an
undocumented feature.

| Variable | Why it exists |
| --- | --- |
| `APPRAFTER_SKIP_NODE_SWAP` | Forces swap ineligible (any value, including empty) so the e2e walk can provision a deliberately cushionless node and exercise the `node prep` retrofit path. Intentionally absent from `--help`. |
| `APPRAFTER_NODE_SWAP_FORCE_INVALID_DROPIN` | Makes `node prep` write a deliberately malformed systemd drop-in, so the walk can assert the failure path. |
| `APPRAFTER_CILIUM_IPV4_ONLY` | Disables Cilium IPv6 during bootstrap. For the k3d e2e, whose ULA IPv6 does not route; production Tier 1 is dual-stack per ADR 0017. |
| `APPRAFTER_BOOTSTRAP_SKIP_CILIUM` | Leaves the cluster's existing CNI alone during bootstrap. k3d-e2e only. |
| `APPRAFTER_HCLOUD_E2E`, `APPRAFTER_K8S_SMOKE`, `APPRAFTER_E2E_*` | Gates for `#[ignore]`d integration tests and for `e2e/*.sh`. Read by the test harness, never by the shipped binary. |

The `APPRAFTER_BACKUP_*` variables are also not CLI surface: they
configure the separate in-cluster `apprafter-backup` runner through its
CronJob's container environment, which the platform-stack chart writes.

## See also

- [CLI reference](cli/index.md) — every command, flag and default.
- [Target store](../operator-guide/target-store.md) — the on-disk
  layout and the full credential-resolution chain these variables
  slot into.
- [Backup & restore](../operator-guide/backup-restore.md) — where the
  restic and S3 credentials are used end to end.
- [Troubleshooting](../operator-guide/troubleshooting.md) — the
  diagnostic-code catalogue.
