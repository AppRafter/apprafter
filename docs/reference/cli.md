# `apprafter` CLI reference

> Every subcommand of the `apprafter` binary, its flags, and the
> alias that routes to it. Authoritative source: clap definitions
> in `cli/platform-cli/src/cli.rs`. Run `apprafter <subcommand>
> --help` for the same information formatted for the shell.

## Top-level

```text
apprafter [OPTIONS] <SUBCOMMAND>
```

Help / version:

```sh
apprafter --help                 # top-level subcommand list
apprafter <subcmd> --help        # per-subcommand flag reference
apprafter --version              # workspace.package.version (sync'd with git tag)
```

Global env vars:

| Variable                       | Purpose                                                                 |
| ------------------------------ | ----------------------------------------------------------------------- |
| `APPRAFTER_CONFIG_DIR`         | Override target-store root (default `~/.config/apprafter/`).            |
| `HCLOUD_TOKEN`                 | Step 2 of the credential resolution chain. Wins over the target store.  |
| `APPRAFTER_SSH_PUBLIC_KEY`     | Step 2 for the SSH public key body. Wins over the target store path.   |
| `APPRAFTER_SSH_PUBLIC_KEY_PATH`| Same as above but a path; CLI reads the file body.                     |
| `APPRAFTER_AGE_KEY`            | Path to the age identity used for state-file encryption (default `~/.config/apprafter/age.key`). |
| `APPRAFTER_HCLOUD_BASE_URL`    | Hetzner Cloud API base URL override (tests + mockito).                  |
| `APPRAFTER_MANIFEST`           | Path to an `Infrastructure.cue` manifest used to overlay `apply` defaults. |
| `APPRAFTER_NO_PING`            | Skip credential-validation API ping. Honours `1` / `true` / `yes`.     |
| `NO_COLOR`                     | Strip all ANSI escapes (miette + the `cli_core::style` helpers).        |
| `RUST_BACKTRACE`               | Set to `1` to enable miette's backtrace rendering (off by default).     |

## Subcommands

### `target`

Manage deployment targets. Alias: `t`.

```text
apprafter target <add|list|use|show|rename|remove>
```

#### `target add <NAME>`

Add (or update with `--renew`) a target. On a TTY, missing flags
trigger an interactive wizard; `--no-interactive` makes the
command purely flag-driven.

```text
--provider <hetzner-cloud>          # required for non-interactive runs
--token <STRING>                    # env: HCLOUD_TOKEN
--ssh-key <PATH>                    # env: APPRAFTER_SSH_PUBLIC_KEY_PATH
--region <STRING>                   # provider-specific (Hetzner: nbg1, fsn1, …)
--tier <solo|team|prod|regulated>   # default tier hint
--cluster-name <STRING>             # default cluster name; falls back to platform-1
--force                             # overwrite existing target (mutually exclusive with --renew)
--renew                             # update credentials only (target must exist)
--no-interactive                    # disable the wizard, error on missing required flags
--no-ping                           # env: APPRAFTER_NO_PING. Skip API validation ping.
```

The wizard validates the Hetzner token against
`GET /v1/locations` before saving. The `apprafter=true` Hetzner
label is the canonical idempotency anchor for `apply` / `destroy`
/ `import`.

#### `target list` (alias: `ls`)

Print every configured target in a table, marking the active one.

#### `target use <NAME>`

Switch the active target. Errors if `<NAME>` doesn't exist.

#### `target show [NAME]` (alias: `info`)

Print one target's details (defaults to the active target). The
stored token renders as `set` / `not set`, never the value.

#### `target rename <FROM> <TO>`

Rename a target. Moves both config + credentials + state cache.
Updates the active pointer if needed.

#### `target remove <NAME>` (alias: `rm`)

Remove a target. Interactive runs prompt for confirmation;
non-interactive runs require `--yes` (no silent destruction).

```text
--yes      # skip the confirmation prompt
```

### `whoami`

One-line identity + active-target summary with optional
provider-API ping. Honours `APPRAFTER_NO_PING=1`.

```text
--no-ping  # skip the Hetzner API ping (HEAD /v1/locations)
```

### `doctor`

Self-diagnostic. Walks the active target's stored config,
credentials, reachability checks, plus the surrounding shell
environment (`kubectl`, `helm`, `ssh`, DNS). PASS / WARN / FAIL
per check; exits 1 on FAIL.

```text
--target <NAME>  # inspect a non-active target
--no-ping        # env: APPRAFTER_NO_PING. Skip API ping.
```

### `init`

Legacy one-shot. Seeds `<cwd>/.apprafter/state.json` with
provider + tier + region. Not required after `target add`;
kept for scripted setups.

```text
--provider <hetzner-cloud>          # required
--tier <solo|team|prod|regulated>   # required
--region <STRING>                   # required
```

### `apply`

Provision (or update) infrastructure for the active target.
Idempotent — re-runs reconcile to the desired state without
recreating live resources tagged `apprafter=true`.

```text
--target <NAME>  # override the active target for the credential chain
```

### `destroy`

Tear down provisioned infrastructure.

```text
--yes            # required for non-interactive runs (no silent destruction)
--target <NAME>  # override the active target for the credential chain
```

### `import`

Read-only rebuild of local state from live Hetzner Cloud
resources tagged `apprafter=true`. Never deletes or creates
provider-side.

```text
--force          # overwrite an already-populated state.hetzner_cloud
--dry-run        # print what would be imported without writing state
--target <NAME>  # override the active target for the credential chain
```

### `kubeconfig` (alias: `kc`)

Print the cached k3s kubeconfig (decrypted). Cold-fetches over
SSH on first use; subsequent calls decrypt the age-encrypted
cache in `.apprafter/state.json`.

```text
--refresh        # force re-fetch over SSH even with a cached value
--target <NAME>  # override the active target for the credential chain
```

Pipe target:

```sh
KUBECONFIG=/dev/stdin kubectl get nodes
```

### `cluster-bootstrap` (alias: `cb`)

Install Cilium + Gateway API CRDs + Application CRD + default-deny
NetworkPolicy + Argo CD + cert-manager + the self-signed
ClusterIssuer + apprafter-operator + admission-webhook into the
cluster pointed to by the cached kubeconfig.

No flags today.

### `argocd-password`

Print the Argo CD admin password (decrypted). Cold-fetches from
the cluster on first use; subsequent calls decrypt the
age-encrypted cache.

```text
--refresh  # force re-fetch from the cluster Secret
```

### `bootstrap-all` (alias: `up`)

One-command convenience wrapper that chains `apply` →
`k3s-ready` poll → `cluster-bootstrap` under a unified progress
UX (cyan `→` phase markers, green `✓` completions, red `✗` on
failure, per-phase elapsed timing).

```text
--target <NAME>  # override the active target for all three phases
--dry-run        # print the resolved target + phase plan, exit 0
```

`--dry-run` is provider-side-safe — no API calls, no state
mutation. Use it to confirm which target + region + cluster_name
the wrapper would resolve against before committing.

### `status`

Print the current cluster status. Stub today; M3 target.

### `login`

Obtain an OIDC-backed kubeconfig. Stub today; Phase 3 target.

### `upgrade-tier --to <NAME>`

Upgrade the cluster from one tier to the next. Stub today; M3
target.

### `auth` (hidden)

Reserved for AppRafter Cloud (Managed) authentication. Hidden
from `--help` until Managed lands. Subcommands today print a
friendly redirect to `apprafter target add`.

```text
apprafter auth login
apprafter auth logout
apprafter auth status
```

## Aliases reference

| Canonical               | Alias  | Notes                                  |
| ----------------------- | ------ | -------------------------------------- |
| `target`                | `t`    | Chains with subcommand aliases below.  |
| `target list`           | `ls`   | `apprafter t ls`                       |
| `target show`           | `info` | `apprafter t info dev`                 |
| `target remove`         | `rm`   | `apprafter t rm bad --yes`             |
| `kubeconfig`            | `kc`   | `apprafter kc --refresh`               |
| `cluster-bootstrap`     | `cb`   | `apprafter cb`                         |
| `bootstrap-all`         | `up`   | `apprafter up --dry-run`               |

The pre-existing `target` ↔ `t` alias chains with every
subcommand alias above, so muscle memory like `apprafter t ls /
apprafter t info / apprafter t rm` all work.

## See also

- [`quickstart.md`](../operator-guide/quickstart.md) — happy-path
  walkthrough.
- [`target-store.md`](../operator-guide/target-store.md) —
  on-disk layout + credential resolution chain.
- [`troubleshooting.md`](../operator-guide/troubleshooting.md) —
  diagnostic-code catalogue.
- [ADR 0030](../adr/0030-cli-target-store-and-credential-chain.md)
  — design rationale.
