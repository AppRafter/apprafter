---
description: "Catalogue of the diagnostic codes the CLI emits, what each one means, and the exact command to run next."
---

# Troubleshooting

> Catalogue of `apprafter::<area>::<reason>` diagnostic codes,
> what each one means, and the exact next-step command(s).
>
> Each error renders with the rustc-style miette block:
>
> ```text
> Error: apprafter::<area>::<reason>
>
>   × <one-line summary>
>   help: <multi-line context + next-step commands>
> ```
>
> Set `NO_COLOR=1` (or pipe stdout to a file) for ANSI-free
> output — miette honours both contracts.

## How to read the diagnostic

Three things matter:

1. **The code** (`apprafter::<area>::<reason>`). Stable across
   patch releases. Group log analytics by this string.
2. **The boxed summary line** (after the `×`). One-line restatement
   of what failed, with relevant identifiers (target name, status
   code, endpoint).
3. **The `help:` block**. Multi-line, walks you through root cause
   + the exact next-step CLI command. Always read this first.

For chained causes (typed wrappers carrying an inner error), miette
renders both layers via `╰─▶`. The OUTER help addresses the
operator-facing scenario ("rotation"); the INNER help addresses the
provider-side detail ("401/403/429/5xx breakdown").

## Diagnostic-code catalogue

### `apprafter::env::cue_not_found`

The `cue` binary isn't on `PATH`.

**Fix.** Install CUE 0.10 or newer — from
<https://cuelang.org/docs/install/> or your package manager — and put it
on `PATH`. If you keep it somewhere off `PATH`, point `CUE_BIN` at the
binary instead; see
[Environment variables](../reference/environment.md).

Only the local commands need it — `apprafter app validate` and the
manifest parse inside `apprafter app add`. The cluster validates every
change server-side regardless, so nothing you deploy goes unchecked.

If you are working from a checkout of the AppRafter repository, the
[contributor setup page](../contributing/setup.md) covers the Nix dev
shell, which puts `cue` on `PATH` for you.

### `apprafter::env::cue_export_failed`

A `cue export` call rejected the manifest with non-zero exit code.

**Fix.** Run `cue vet <manifest>` against the same file to
reproduce the parse / type error locally. The captured stderr in
the diagnostic body usually points at the offending expression.

### `apprafter::provider::hetzner_api_error`

The Hetzner Cloud API refused the request. The credential check
reports this code too, for any status but 401
(`apprafter::target::token_rejected`).

**Fix.** Read the inner help. It lists what each status means:

- **401 unauthorized** — the token is wrong, or it was revoked or
  rotated. For a target's stored token,
  `apprafter target add <name> --renew` replaces it: at a terminal it
  asks for the new token without showing it; in a script, pass `--token`.
  An `HCLOUD_TOKEN` in the environment outranks the stored token.
- **403 forbidden** — the token lacks Read & Write (it is
  Read-only), or the project forbids the call, for example at one
  of its limits.
- **429 rate limit** — too many requests: wait, then try again.
- **5xx** — an outage at the provider. Check
  https://status.hetzner.com/.

After fixing the root cause, run `apprafter doctor` to confirm
reachability.

### `apprafter::provider::server_type_not_selected`

Provisioning a new machine requires an explicit server type, and
none was found in the resolution chain (`--server-type` flag >
`spec.nodes[0].type` in the manifest > the type recorded in state > the
type in the target store > `APPRAFTER_SERVER_TYPE` env). The error fires only on the **create
path** (a new machine is about to be provisioned); `apply` on an
already-running cluster does not require the type.

**Fix.** Choose the method that matches your workflow:

- **Interactive / wizard:** run `apprafter target machine` to open
  the live `(region × SKU)` picker and write the chosen type into the
  active target. Subsequent `up` / `apply` calls use it automatically.
- **Non-interactive / CI:** pass `--server-type <sku>` on the
  provisioning command (`target add`, `apply`, `up`, `restore
  --reprovision`), or export `APPRAFTER_SERVER_TYPE=<sku>` in the
  runner environment. This is the recommended path for pipelines.
- **Declarative (manifest):** set `type: "<sku>"` on the entry in
  `spec.nodes` in your `Infrastructure.cue` manifest and point
  `APPRAFTER_MANIFEST` at it.
  The manifest rung sits above the target store, so a committed
  manifest pins the type even if the target default changes.
- **No saved target (env-credential run):** when provisioning via
  `HCLOUD_TOKEN` + no target store (the ephemeral CI path), there is no
  store to persist the fact or backfill into. Pass `--server-type` or
  `APPRAFTER_SERVER_TYPE` explicitly; use `apprafter target add` for
  repeatable provisioning with a stored type.

**Migration (existing clusters):** an existing cluster whose state
predates this field does **not** trigger this error on `apply`
(the reconcile path never requires the type). The first `apply`
after upgrading the CLI backfills the type from the live Hetzner
server automatically.

### `apprafter::provider::server_type_unavailable`

Pre-flight rejection: the requested `(region × SKU)` pair isn't
valid or available. The error body names the exact kind:

- **`Unknown`** — the SKU was not found in the Hetzner catalog at
  all. The alternatives list in the error body shows the nearest
  known SKUs in the requested region.
- **`NotOfferedInRegion`** — the SKU exists but is not offered in
  the requested region. The alternatives list shows which regions
  stock it, and which alternative SKUs are available in the
  requested region.
- **`Retired`** — the SKU's end-of-sale date has passed
  (`unavailable_after <= today`). The alternatives list shows the
  recommended replacements in the requested region. (A SKU whose
  retirement date is still in the future is selectable in the picker
  with a `!` badge — it is not an error.)
- **`OutOfCapacity`** — the SKU is offered in the region but
  Hetzner currently has no available capacity. This is transient;
  try again later or pick a different type. The picker hides
  sold-out rows by default; toggle the `Show sold-out` option to
  see them.

**Fix.** Run `apprafter target machine` to open the live picker and
choose an available `(region × SKU)` row. For non-interactive
runs, consult the alternatives list in the error body and pass
`--server-type <alternative>` with `--region <region>`.

### `apprafter::state::corrupt`

A target's `state.json` failed to parse. The file was written by a
previous `apply` / `import` and may have been hand-edited or copied
across incompatible CLI versions.

**Where it is.** State is per **target**, not per directory:

```text
<config-root>/state/<target>/.apprafter/state.json
```

where `<config-root>` is `APPRAFTER_CONFIG_DIR` if set, else
`$XDG_CONFIG_HOME/apprafter` (`~/.config/apprafter`). The error's own
summary line prints the exact path — `state file at <path>: <message>`
— and that path is the one to act on.

Two stale pointers to ignore. The diagnostic's `help:` text still
describes the pre-v0.1.154 layout ("the local `.apprafter/state.json`
file"), and so did this page; that per-cwd file is now only a legacy
artefact, moved into the per-target slot on first use and never written
again. Deleting a `.apprafter/` directory in your project fixes
nothing — the file that failed to parse is under the config root.

**Fix.** If the file looks salvageable, edit it by hand. Otherwise
delete that target's state directory and run `apprafter import` — the
`apprafter=true` Hetzner label is the canonical idempotency anchor, so
`import` rebuilds state from live API objects:

```sh
rm -rf "${APPRAFTER_CONFIG_DIR:-${XDG_CONFIG_HOME:-$HOME/.config}/apprafter}/state/<target>"
apprafter import --target <target>
```

### `apprafter::target::invalid_config`

A YAML file in the target store (`$XDG_CONFIG_HOME/apprafter/`, or
`APPRAFTER_CONFIG_DIR`) failed to parse: it was edited by hand or
written by an incompatible CLI version. The message and the help name
the file. For a target's `credentials.yaml` the message gives only the
line and column where parsing stopped, never the file's text, because
that text is the API token.

**Fix.** Fix the file by hand (these files are small), or restore it
from a backup. Otherwise, it depends on whose file it is:

- **A target's `config.yaml` or `credentials.yaml`**: remove the
  target with `apprafter target remove <name>` and add it again with
  `apprafter target add <name> --provider hetzner-cloud …`, its token
  included. `target remove` takes a target it cannot read: it warns
  which file cannot be read and deletes the target's files without
  reading them. The removal also deletes the target's local state:
  the record of its server, the cached kubeconfig and the Argo CD
  password. If a server is recorded, fix or restore the file first:
  the warning names the server, which keeps running, and
  `apprafter destroy --target <name>` cannot read the token until the
  files are fixed. If you remove it anyway, add the target again with
  the same token and region, and
  `apprafter import --target <name>` rebuilds the record from the
  provider; the kubeconfig and the Argo CD password are fetched again
  on first use.
- **The store's own `config.yaml`**: no target's removal or re-add
  repairs it. It records only which target is the default, so you can
  delete it and choose the default again with
  `apprafter target use <name>`.

### `apprafter::target::no_active`

A command needs a target, none was named with `--target`, and the
store has no active target: none has been created yet, or the last
one was removed.

**Fix.** `apprafter target add <name> …` creates one; the first add
on a fresh store makes it active. With targets in the store,
`apprafter target use <name>` picks one, or pass `--target <name>`
for a single run.

### `apprafter::target::not_found`

A subcommand asked for a target that isn't in the store — for
example `--target ghost` against a store with no `ghost` target.

**Fix.** `apprafter target list` shows what's in the store.
`apprafter target add <name> …` creates a new one; the first add
on a fresh store auto-activates it. If `available: ` shows
nothing, you're seeing the empty-store first-run case.

### `apprafter::target::token_rejected`

The provider's read-only credential check returned 401
unauthorized. **Distinct from** the generic Hetzner API error —
this fires only on the explicit `target add` ping path, so the
help text targets the rotation flow specifically.

The token was mistyped, or it was revoked or rotated, or its
project was deleted. Its permissions are not the cause: the check
only reads, so a Read-only token passes it. A token with stray
whitespace never reaches the check either; the format check
refuses it first (`apprafter::target::invalid_token`).

**Fix.**

- A token is shown only once, when it is created: paste it again
  from where you saved it, or create a new one with Read & Write
  permission in the Hetzner Console (open the project, then
  Security → API tokens).
- If you're rotating, use `apprafter target add <name> --renew`
  instead of re-creating the target. At a terminal it asks for the new
  token without showing it; in a script, pass `--token`.

### `apprafter::target::provider_unreachable`

The provider's API gave **no answer**: the connection was
refused, the name did not resolve, or the request timed out. Any
request reports this code, whether it was the credential check, a
machine catalogue read or `apply`. A provider that answered is
never reported this way: a 5xx outage or a 429 rate limit is
`apprafter::provider::hetzner_api_error` with its status, and an
answer AppRafter cannot read is `apprafter::provider::request_failed`.
The token may still be valid once the API recovers — the help text
intentionally avoids any rotation suggestion that would misdirect
operators.

**Fix.**

- `apprafter doctor` to confirm DNS + reachability.
- Check the provider's status page
  (https://status.hetzner.com/ for hetzner-cloud).
- VPN / corporate proxy: ensure `https://api.hetzner.cloud/`
  is reachable.
- `--no-ping` (on `target add`, also with `--renew`, and on
  `target machine`) to save offline and verify later.

### `apprafter::io::error` / `apprafter::io::json` / `apprafter::io::yaml`

Low-level filesystem / network IO error, or
encode/decode error on `state.json` (JSON) /
`config.yaml` / `credentials.yaml` (YAML).

**Fix.** The captured OS message names the failing path or
socket. Common cases: missing directory, wrong permissions
(`chmod 0600` on credentials), full disk. When the message is
`cannot read <path>` and the path is the target store's own — its
root, `targets/` or a folder in it, its `config.yaml` — this account
cannot read it: most often its permissions, or those of a folder above
it, do not let it, and `ls -ld` on each shows them. A folder another
account owns or can change is
[`store_not_private`](#store-not-private) instead. For decode failures on
target-store files, fix the YAML by hand (they are small files), or
restore the file from a backup. Otherwise remove that target with
`apprafter target remove <name>`, which takes a target it cannot
read, and add it again with `apprafter target add`. The removal
also deletes the target's local state: the record of its server,
the cached kubeconfig and the Argo CD password. If a server is
recorded, fix or restore the file first. After adding the target
again, `apprafter import --target <name>` rebuilds the record from
the provider; the kubeconfig and the Argo CD password are fetched
again on first use. `target add --force` cannot rewrite such a
target: it keeps the stored values, so it needs a readable config.

### `apprafter::backup::job_active`

`apprafter backup run` (or the first backup `apprafter backup enable`
takes) found a backup Job or a check Job that has not finished, and
started nothing beside it. The lines it printed just before the error name
that Job and say what it is doing, as `apprafter backup status` would:
`Running`, `Pending, cannot be scheduled: …`, or `Retrying after 1 failed
attempt`. Two runs at once do not both finish: two backups need the same
helper pods, and a backup and a check each fail on the other's repository
lock. `enable` does not fail on this: backup is enabled, and it skips only
its first backup.

**Fix.** Wait until `apprafter backup status` shows that Job finished, then
run `apprafter backup run` again. A Job that cannot start may hold on
until its deadline; the printed lines then include the command that
deletes it, and [the backup runner's pod cannot be
scheduled](backup-restore.md#runner-unschedulable) explains why it cannot
start.

### `apprafter::backup::runner_unschedulable`

`apprafter backup run` (or the first backup `apprafter backup enable`
takes) created the backup Job, but no node took its pod: for two minutes
no node had room for it, or for ten minutes a condition of the node, such
as memory pressure, kept it off. The command deleted the Job and stopped.
The lines it printed just before the error give the scheduler's reason
(`0/1 nodes are available: 1 Insufficient memory. …`) and, for a lack of
room, what the runner asks for. When `enable` took this backup, backup is
enabled all the same: the error's help says so, and `enable` need not run
again.

**Fix.** For a lack of room, `apprafter top` shows how much of each node
is requested, and by what. Free enough for the runner, or move to a
bigger machine, then run `apprafter backup run` again. The scheduled
backup asks for the same room, so it cannot start either until then. A
node condition such as `node.kubernetes.io/memory-pressure` lifts on its
own once the condition ends; run `apprafter backup run` again then. The
whole case, including how to tell whether tonight's scheduled backup is
stuck: [the backup runner's pod cannot be
scheduled](backup-restore.md#runner-unschedulable).

### `apprafter::secrets::age_key_missing` {#age-key-missing}

The age key that the cached kubeconfig and Argo CD password are
encrypted under is not at the path the error names —
`APPRAFTER_AGE_KEY`, or `~/.config/apprafter/age.key` by default (see
[Environment variables](../reference/environment.md)). A command that
reads a cache never creates a key in its place: a new key cannot
decrypt anything cached under the old one. `apprafter doctor` reports
the same on its **Kube API reachable** row.

On a target store that `apprafter lock on` has locked, this does not occur:
its caches are encrypted to the store's own key, kept in its vault, and
`APPRAFTER_AGE_KEY` does not apply there.

A locked store whose vault is lost is the exception. Once its vault and
`store.yaml` are moved aside ([every way into a locked store is
lost](#locked-store-lost)), the store is in files again and looks for the
shared key, which never held those caches: they were encrypted to the
vault's own key. The error, and doctor's row, say so: the cached secrets
were encrypted to the key of a target store vault that is no longer here,
not to the age key at that path, and no key put there opens them. If you
still have the vault and its `store.yaml`, put them back and unlock;
otherwise fetch the kubeconfig again under a new key, as below.

**Fix.** If you still have the key, put it back at that path, or set
`APPRAFTER_AGE_KEY` to where it is. Nothing else is needed.

If the key is lost, fetch the kubeconfig again under a new key:

```sh
apprafter kubeconfig --refresh --target <name>
```

It reads `/etc/rancher/k3s/k3s.yaml` from the target's node over SSH,
as the first fetch did, and changes nothing on the node or in the
cluster. Before it creates the key it lists everything else cached
under the lost key, which the new key cannot read, and asks; without a
terminal it refuses unless you pass `--yes`. Answering no, or pressing
Esc or Ctrl-C, creates, fetches and writes nothing: the command exits 1
with `apprafter::secrets::new_key_declined` and prints nothing on
stdout, so `apprafter kubeconfig > kc && …` stops there. The key is
created only once the node has answered, so a failed fetch leaves none
behind.

Only the targets of the current config root (`APPRAFTER_CONFIG_DIR`)
are checked for secrets cached under the lost key. The key is shared
by every config root on the machine, so the list leaves out the targets
of another root, and in a root whose targets cache nothing the first
fetch creates the key without asking, as on a first use. Their caches
then fail with
[`apprafter::secrets::cache_undecryptable`](#cache-undecryptable).

What the new key leaves behind, and the way back for each:

| Cached under the lost key | After the new key |
| --- | --- |
| The target's kubeconfig | Fetched again by the command itself. |
| The target's Argo CD admin password | Dropped from the cache: `apprafter argocd-password --target <name>` fetches it from the cluster again. It reads `argocd-initial-admin-secret`; if you deleted that secret after changing the password, use the password you set. |
| Another target's kubeconfig | `apprafter kubeconfig --refresh --target <other>` fetches it again under the new key, without asking: the key exists now. |
| Another target's Argo CD admin password | `apprafter argocd-password --refresh --target <other>` fetches it again, once that target's kubeconfig is fetched. |

`apprafter up` and `apprafter restore --reprovision` never create a
key in place of a lost one. They check for it before they provision
anything: when the key is missing and anything in the config root is
cached under it, they stop with this error, and nothing is created.
The help names a target that has something cached under the lost key.
Put the key back, or run the command above for that target: the target
being provisioned has no server yet to fetch a kubeconfig from. Then
run the same `up` or `restore` command again. With no key and nothing
cached in the config root, there is no lost key: `up` goes on, and
creates the key when it caches the new kubeconfig.

If the key goes missing while the server is being created, they stop
later, in the `k3s-ready` step, with the same error. The server then
exists and Hetzner bills for it, but cluster-bootstrap has not run.
Put the key back, or run the command above for the target being
provisioned (the node may need a few minutes after provisioning before
it answers). Then run the same `up` or `restore` command again: it
finds the server it provisioned instead of creating another, and goes
on with cluster-bootstrap.

### `apprafter::secrets::age_key_identity_count` {#age-key-identity-count}

The age key file the error names does not hold exactly one key. AppRafter
reads it as `age-keygen -o <file>` writes it: lines that start with `#`
(age-keygen's `# created:` and `# public key:` lines) and blank lines are
skipped, and exactly one other line must be the key,
`AGE-SECRET-KEY-1…`. The message says whether the file holds no such line
or several. AppRafter itself writes the key line alone. The file is never
used and never replaced: a command that would create a key on a first use
stops here instead.

**Fix.** Keep only the key the cached secrets were encrypted with in that
file, or set `APPRAFTER_AGE_KEY` to a file that holds only that key. A key
line that is cut short or mistyped is reported as `apprafter::cli::other`,
with the words `parse age key` and the file's path.

### `apprafter::secrets::cache_undecryptable` {#cache-undecryptable}

A cached kubeconfig or Argo CD password does not decrypt with the age
key there is. It was cached under another key — one that was lost and
replaced, or one `APPRAFTER_AGE_KEY` no longer points at — or the
cached copy is damaged. For a kubeconfig, `apprafter doctor` reports
the same on its **Kube API reachable** row.

**Fix.** If you have the key it was cached under, set
`APPRAFTER_AGE_KEY` to it. Otherwise fetch it again under the key
there is: `apprafter kubeconfig --refresh --target <name>` for a
kubeconfig, `apprafter argocd-password --refresh --target <name>` for
an Argo CD admin password. Neither asks, since the key they cache under
exists.

On a target store that `apprafter lock on` has locked, the key is the
store's own, in its vault: a cache it does not open was written by an older
AppRafter or by another program, or the copy is damaged, and whoever holds
the key it was written for can read it until it is replaced. Replace it
with the same `--refresh` commands; on a locked store they ask for the lock
password, unless an unlock window is open in this terminal.
`APPRAFTER_AGE_KEY` is refused there.

### `apprafter::secrets::store_locked` {#store-locked}

The target store is locked (`apprafter lock on`), the command needs one of
its secrets, and nothing here can open the vault: no unlock window is open in
this terminal, and either the command has no terminal to ask at — a script, a
CI job, an AI agent's tool — or nobody answered the prompt within five
minutes. Nothing was changed. When `apprafter up` or
`apprafter restore --reprovision` stops this way, it stops before
provisioning, so no server was created.

**Fix.** In your own terminal run `apprafter unlock`, then run the command
from that terminal while the window is open. A command started without a
terminal cannot ask for the lock password, by design.

In AppRafter the same code says that AppRafter holds no key for the store:
AppRafter locked, or the store was locked from a terminal while AppRafter
was open. Unlock AppRafter with the lock password or the recovery key, then
try again.
How a locked store is opened, and in what order:
[How the store lock works](../how-it-works/the-store-lock.md#opening).

### `apprafter::secrets::override_refused` {#override-refused}

`HCLOUD_TOKEN` or `APPRAFTER_AGE_KEY` is set, and the target store is
locked. A locked store uses only the token and the age key in its vault, so a
value from the environment would pass the lock unnoticed; the command refuses
before it asks for anything. Commands that need neither still run, and
`target add` still stores `HCLOUD_TOKEN` as the target's new token (a renewal
at a terminal asks for it instead).

**Fix.** Unset the variable; the command then asks for the lock password at
its terminal. To work with a value from the environment on purpose — in CI,
say — use a target store of its own that is not locked, with its own
`APPRAFTER_CONFIG_DIR` — [Give an agent a store of its
own](protecting-secrets-from-agents.md#playground). `apprafter lock off` is the one command that takes
`APPRAFTER_AGE_KEY` on a locked store: as the file the shared age key is
written to, when a cached secret goes back under it.

### `apprafter::secrets::unlock_failed` {#unlock-failed}

Three answers at the prompt opened nothing. The prompt takes the lock
password or the recovery key, either one.

**Fix.** Run the command again with the lock password, or with the recovery
key if the password is lost. If neither opens the store, see [Every way into
a locked store is lost](#locked-store-lost).

### `apprafter::secrets::lock_incomplete` {#lock-incomplete}

An earlier `apprafter lock on` or `apprafter lock off` stopped before it
finished — Ctrl-C, a closed terminal, a crash. Until one finishes or backs
out, commands that need the store's secrets refuse, because some may be only
in the vault. Nothing was lost: no copy in clear is removed before the vault
has been written and checked.

**Fix.** For an interrupted `lock on`, run `apprafter lock on` again. If it
had written the vault, it asks for both the lock password and the recovery
key it showed and finishes; if not, it starts over, and the recovery key
shown before is not valid. Without one of the two, `apprafter lock off`
backs out with the other, and a new `apprafter lock on` starts over. For an
interrupted `lock off`, run `apprafter lock off` again. `apprafter doctor`
names the state.

### `apprafter::secrets::files_not_kept` {#files-not-kept}

Turning the lock off writes each secret back to its file, and reads each one
back before it deletes the vault. A file did not hold what was written — the
disk may be full, or another program changed the file — so the run stopped
with the vault kept. Nothing was lost.

**Fix.** Run `apprafter lock off` again, or choose **Finish…** on the desktop's
Security page: either finishes from where the run stopped. If it stops here
again, check the disk the target store is on.

### `apprafter::secrets::store_changed` {#store-changed}

Another AppRafter process turned this store's lock on or off, or replaced
its vault, while this command ran. Nothing was written.

**Fix.** Run the command again: it reads the store as it is now.

### `apprafter::secrets::store_file_unreadable` {#store-file-unreadable}

`store.yaml`, at the root of the target store, says whether its secrets are
locked, and it cannot be read — or it says something no AppRafter writes: a
newer format, an unknown state, a lock without an id, or secrets in files
with a `vault` beside it. A `vault` with no `store.yaml` beside it is
refused the same way. AppRafter then treats the store as locked and uses
none of its secrets.

**Fix.** If a newer AppRafter wrote it, upgrade. Otherwise fix it by hand —
`secrets:` is one of `files`, `locking`, `locked` or `unlocking` — or restore
it from a backup. Never delete it while the store's `vault` exists.

### `apprafter::secrets::vault_unreadable` {#vault-unreadable}

The vault is missing, is not an AppRafter vault, was written in a newer
format, fails its integrity check, or belongs to another target store. The
message says which.

**Fix.** A newer format: upgrade AppRafter. Any other case: restore the
store's `vault` from a backup — with its `store.yaml`, when it belongs to
another store. Never delete the vault while `store.yaml` says the store is
locked: nothing else holds these secrets. Without a backup, see [Every way
into a locked store is lost](#locked-store-lost).

### `apprafter::secrets::vault_unwritable` {#vault-unwritable}

A write to the vault did not happen, and the vault is as it was. The vault
would grow past its limit (the tokens of a few thousand targets), the input
cannot be stored (a token that is not one line, a target name a vault entry
cannot hold, a name already taken), or the filesystem cannot create a file
without risking replacing one (a filesystem without hard links, such as FAT
or exFAT).

**Fix.** The help names the case. For a filesystem without hard links, keep
the target store elsewhere: `APPRAFTER_CONFIG_DIR` moves it.

### `apprafter::secrets::vault_needs_write_lock` {#vault-needs-write-lock}

The store's filesystem gives no file locks, and a vault is never written
without the store's write lock: two writers racing could overwrite the only
copy of every token.

**Fix.** Keep the target store on a local filesystem with file locks;
`APPRAFTER_CONFIG_DIR` moves it.

### `apprafter::secrets::slot_refused` {#slot-refused}

The vault's lock-password slot uses key-derivation settings this AppRafter
refuses — weaker than it accepts, or beyond what it computes — or none of the
vault's slots is one this AppRafter knows. `apprafter lock status` names a
refused slot.

**Fix.** Open the store with its recovery key instead — every prompt takes
it — then run `apprafter lock off` and `apprafter lock on`, which sets a new
lock password with current settings. Between the two the store's secrets
are in files, in clear; `lock on` shows a new recovery key, and the old one
stops working. For slots a newer AppRafter made, upgrade.

### `apprafter::secrets::cache_not_sealed` {#cache-not-sealed}

A target's cached kubeconfig in a locked store is not encrypted: an older
AppRafter or another program wrote it. A locked store never uses it, and
never encrypts it later — it cannot be told from a forgery.

**Fix.** Fetch it again from the node, over SSH, which caches it encrypted
to the store's own key: `apprafter kubeconfig --refresh --target <name>`.

### `apprafter::secrets::token_outside_vault` {#token-outside-vault}

A target of a locked store has a token in clear in its `credentials.yaml`,
written by an older AppRafter's `target add` or by another program. A locked
store takes tokens only from its vault, so this one is never used.
`apprafter doctor` fails that target's token row, and warns for any other
target with one.

**Fix.** At a terminal, run `apprafter target add <name> --renew`: it asks
for the new token without showing it (in a script, `--token`), then for the
lock password, stores the token in the vault and puts the vault's marker
back in the file. That token was on disk in clear: if it is a real one,
delete it in the Hetzner Console once the new one works.
`apprafter lock off` refuses while such a file exists.

### `apprafter::secrets::token_in_another_vault` {#token-in-another-vault}

The target's `credentials.yaml` names the vault of another target store: the
target's files were copied from it, or this store's `vault` and `store.yaml`
were moved aside. Its token is not here.

**Fix.** At a terminal, run `apprafter target add <name> --renew`: it asks
for the token without showing it (in a script, `--token`) and stores it
here. The old token stays in the other store's vault.

### `apprafter::secrets::token_missing_from_vault` {#token-missing-from-vault}

The target's `credentials.yaml` says its token is in this store's vault, and
the vault has no entry for it. An older AppRafter renames a target's files
without its vault entry.

**Fix.** If an older AppRafter renamed it, rename it back with this
AppRafter (`apprafter target rename <new> <old>`), then rename it again.
Otherwise store its token again: at a terminal, run
`apprafter target add <name> --renew`, which asks for it without showing
it (in a script, `--token`).

### `apprafter::secrets::random_unavailable` {#random-unavailable}

The operating system's random number generator failed, so no key was made
and nothing was written. AppRafter makes keys only from it, with no
fallback.

**Fix.** Run the command again. If it keeps failing, the system's source of
randomness is broken — a container or virtual machine without one, for
example.

### `apprafter::secrets::runtime_dir_foreign` {#runtime-dir-foreign}

On Windows, the runtime directory — where a command writes the decrypted
kubeconfig it hands to `kubectl` — belongs to another account. That account
can give itself access to the directory whatever its permissions say, and
replace the kubeconfig before `kubectl` reads it, pointing `kubectl` at a
server of its choosing. AppRafter writes nothing there. The message names
the directory: `<store>\run` when `APPRAFTER_CONFIG_DIR` is set, a folder
under `%LOCALAPPDATA%\apprafter\run` when it is not; the app's own is in its
local data folder. A directory your account, an administrator or the system
owns is used.

**Fix.** Remove the directory: AppRafter makes it again, owned by you. If
Windows refuses, an administrator can remove it. When the store is in a
folder other accounts can write to, set `APPRAFTER_CONFIG_DIR` to a folder
only you can write. On Linux and macOS such a directory is refused too, as
an I/O error: its mode cannot be set.

### `apprafter::secrets::store_not_private` {#store-not-private}

A folder of the target store belongs to another account, or other accounts
can write to it. Whoever can replace a file there can put a Hetzner token, a
kubeconfig or a vault of their own in place of yours and send your next
deploy to a project or a cluster of their choosing, so AppRafter neither uses
nor writes any of the store's secrets until the folder is yours alone. The
message names the folder: the store itself, `targets` or a target's folder in
it, `state` or a folder under it. A command refused here is refused before
it asks for anything at its terminal: a lock password, a Hetzner or
repository token, a backup passphrase. Commands that need no secret still run —
`apprafter target list`, `apprafter target show`, `apprafter lock status` —
and [`apprafter doctor`](#store-folder-foreign) lists every folder and secret
file concerned. A folder another account owns is often one your account
cannot open either: a command that has to read a file in it, `apprafter
target list`, `apprafter target show` and doctor's `Config file readable`
row included, then gives this message, naming the folder, rather than a bare
permission error or an empty list of targets.

AppRafter creates every folder of the store your account's alone, whatever
the umask, and never changes a folder that is already there. So the folder
the message names was there before: created by hand, by another program, or
by an earlier AppRafter release under a umask such as 002.

On Linux and macOS a folder counts as writable by others when its mode lets
others write to it — a sticky bit does not change that, since they can still
create files in it — or lets its group write, unless that group is your
account's own: named after your account, your primary group, with no other
member. On Windows it does when its permissions let another account create,
change or delete files in it, or change its permissions or its owner, for
the folder itself or for what is created in it. An owner other than your
account counts too, except, on Windows, the Administrators group and the
system.

**Fix.** Keep the store in a folder only you can write: move it there and
set `APPRAFTER_CONFIG_DIR` to it. Or fix the folder the message names. On
Linux and macOS, `chmod go-w <folder>` removes the other accounts' write
access, and an administrator can give a folder back to your account with
`sudo chown`. On Windows, remove their write access in the folder's
Properties › Security; an administrator can make your account its owner in
Properties › Security › Advanced. Then run the command again.

### `apprafter::lock::no_window` {#no-window}

`apprafter unlock` cannot open an unlock window in this terminal: there is
no private directory for its socket, the socket's path would be too long,
the terminal's session has no leader, or — on Windows — there is no console
to bind it to. For these it refuses before asking for anything. The same
code ends an `unlock` whose new window did not start after the lock
password was accepted: a holder already running for this terminal does not
answer, or a new one did not start within five seconds.

**Fix.** `apprafter lock status` says why. Every command in this terminal
still works: each asks for the lock password itself. On Linux and macOS,
setting `XDG_RUNTIME_DIR` to a directory only you can open, with a short
path, usually gives the window a place. A holder that does not answer is
ended by `apprafter lock now`.

### `apprafter::lock::not_ended` {#not-ended}

`apprafter lock now` found this terminal's unlock window but could not end
it: its holder does not answer, and it could not be stopped from here. The
window is still open: until that process ends, it keeps the store's key.

**Fix.** Stop the process the message names: `kill -9 <pid>` on Linux and
macOS, `taskkill /F /PID <pid>` on Windows. Meanwhile every command asks
for the lock password at its prompt.

### `apprafter::cluster::k3s_not_ready` {#k3s-not-ready}

`apprafter up` or `apprafter restore --reprovision` provisioned the
server, then waited five minutes in its `k3s-ready` step for the
node's kubeconfig, and the node did not get there: SSH did not reach
it, k3s had not written the file yet, or the apiserver did not accept
connections. The message ends with the last of those errors. The
server exists and Hetzner bills for it, but cluster-bootstrap has not
run.

**Fix.** Run the same `up` or `restore` command again. It finds the
server it provisioned instead of creating another, waits for the node
again, and goes on with cluster-bootstrap. `apprafter kubeconfig
--refresh` alone fetches the kubeconfig but bootstraps nothing. If the
node still does not answer, see [The `k3s-ready` step of `up` takes
longer than
expected](#the-k3s-ready-step-of-bootstrap-all-takes-longer-than-expected).

### `apprafter::cli::other`

Catch-all for messages that haven't been promoted to a typed
variant yet. The error code itself is stable — log-analytics
piped through this code surface as candidates for typed
variants in future releases.

**Fix.** Read the message text. If you see the same wording often,
file an issue — recurring catch-all messages should be promoted
to a typed variant with its own help text.

## Doctor: a secret file other accounts may open {#secret-file-wide}

`apprafter doctor`, and the app's Doctor, warn about each of the target
store's secret files that another account on this computer may be able to
open: a target's `credentials.yaml` (on its own row for the target checked)
and `state.json`, the age key, and the vault of a locked store. They read
the file's permissions only, never its contents, and change nothing.

- **Linux and macOS:** `<file> mode 0600 (got 644)`, with the
  `chmod 600 <path>` that fixes it. AppRafter writes these files 0600; a
  wider mode comes from a copy, a restore or a hand edit.
- **Windows:** `<file> not restricted to your account`. A file AppRafter
  wrote before it gave these files a DACL of their own, or one widened
  since, keeps what its folder grants until AppRafter next reads it; each
  read then restricts it to your account. Run a command that reads it —
  with the target active, `apprafter whoami` reads its token and
  `apprafter status` its state file and the age key; `apprafter lock
  status` reads the vault — or use the app, and the warning goes. When the
  fix says your account may not change who can open the file, another
  account owns it, or its permissions keep even its owner from changing
  them: an administrator can make your account its owner with full
  control, and AppRafter restricts it at its next read.

## Doctor: a store folder or a secret file another account owns or can write {#store-folder-foreign}

`apprafter doctor`, and the app's Doctor, warn about each folder of the
target store that another account owns or can write to — the store itself,
`targets` and each target's folder, `state` and each folder under it — and
about each secret file another account owns: a target's `credentials.yaml`
and `state.json`, the age key, and on Windows the vault. They read owners and
permissions only, and change nothing.

- `Target store folder belongs to another account`, or
  ``Target store folder `targets/prod` belongs to another account``.
- ``Target store folder `targets` can be changed by other accounts``, with
  the folder's mode on Linux and macOS (`(mode 777)`).
- ``Credentials file of target `prod` belongs to another account``, and the
  same for a state file, the age key or the vault file.
- ``Target store folder `state/prod`: who can change the folders in it is
  unknown``, when the folders in it cannot be listed: one another account
  owns is usually closed to you too. The store's secrets are not used until
  they can be.

While a folder is listed, the store's secrets are not used: the token and
Kubernetes API rows are skipped with the refusal
[`store_not_private`](#store-not-private), so doctor neither sends a token
another account may have put there to the provider nor runs a kubeconfig it
may have put there. The fix for a folder is that refusal's; a file another
account owns can be given back to your account by an administrator, or
replaced with a copy of your own.

## Every way into a locked store is lost {#locked-store-lost}

AppRafter cannot open a locked store without its lock password or its
recovery key, and the recovery key opens nothing without the vault file.
There is no reset. If you have a backup of the store's `vault` and either
secret, restore the vault and unlock. Otherwise rebuild the store's secrets
from their sources; the clusters themselves keep running. How to keep this
from happening: [Back it up](protecting-secrets-from-agents.md#backup).

1. Move the vault and `store.yaml` aside. Keep them: a password found later
   still opens them. `STORE` is the target store's root, which the
   `Target store:` line of `apprafter lock status` names (write a leading `~`
   as `$HOME`: inside quotes the shell does not expand it):

   ```sh
   STORE="$HOME/.config/apprafter"   # the root `apprafter lock status` names
   mkdir -p ~/apprafter-locked-store
   mv "$STORE/vault" "$STORE/store.yaml" ~/apprafter-locked-store/
   ```

   The store is in files again, and each target's `credentials.yaml` still
   names the old vault, so its token is not used.

2. For each target, create a new API token in the Hetzner Console (open the
   project, then Security → API tokens), and at a terminal store it:

   ```sh
   apprafter target add prod --renew
   ```

   It asks for the token without showing it (in a script, `--token`). The
   old token was only in the vault; delete it in the Console once the new
   one works. Until step 4, the new tokens are in files, in clear.

3. For each target, fetch the kubeconfig again from its node — this needs SSH
   access to the node, with the key the target was provisioned with — and
   then the Argo CD password from the cluster:

   ```sh
   apprafter kubeconfig --refresh --target prod
   apprafter argocd-password --refresh --target prod
   ```

   With no shared age key on this computer yet, the first refresh asks
   before it creates one, as
   [`age_key_missing`](#age-key-missing) describes. It says the cached
   secrets were encrypted to the key of the vault you moved aside, which no
   key at the shared key's path opens: answer `y`.

4. Lock the store again with `apprafter lock on`: a new lock password, and a
   new recovery key to keep somewhere other than this computer. A cached
   secret still encrypted to the vault you moved aside — one whose refresh
   did not run — is named so in its plan and dropped; its refresh fetches it
   again.

## Common failures found in end-to-end runs

### "state has no provider — run `apprafter init` first"

The operational commands read the active target's `config.yaml` as a
fallback when `state.json` carries no provider, so seeing this means that
`config.yaml` is missing its `provider` field — fix it by hand or recreate
the target.

### The `k3s-ready` step of `up` takes longer than expected {#the-k3s-ready-step-of-bootstrap-all-takes-longer-than-expected}

The `k3s-ready` phase is **waiting for cloud-init + k3s on the
new node**, not the kubeconfig fetch itself — that's why the
phase was renamed from `kubeconfig` to `k3s-ready` in v0.1.91.
Typical duration on Hetzner `cpx22` + Ubuntu 24.04 is 20–40 s,
of which the trailing 1–2 s is the actual SCP. Pre-v0.1.91 the
phase consistently stabilised at ~60 s because the first SSH
attempt blocked on the kernel's default TCP connect timeout
(~30 s) while sshd was still coming up; v0.1.91 added
`ConnectTimeout=5` to the SSH wrapper so the retry loop's
10-second sleep absorbs the wait instead.

If you see `> 60 s` consistently on `k3s-ready`:

- **Read the spinner — the reason is already on it.** Every failed
  attempt rewrites the progress line to `attempt <n> — k3s not ready
  yet (<error>); next retry in <s>s`, and logs the same at `WARN`,
  which the default log level already shows. The most common one is
  `cat: /etc/rancher/k3s/k3s.yaml: No such file` — SSH works but k3s
  hasn't written the file yet; just wait. The other is `apiserver
  <addr> not accepting connections yet`, which is the second gate: the
  file exists but `:6443` isn't up.
- For more than that, raise the level **by crate**:
  `RUST_LOG=apprafter=debug,cli_providers=debug apprafter up`. Those
  are the target names the CLI logs under (`apprafter`, `cli_core`,
  `cli_state`, `cli_providers`); a directive naming anything else —
  `kubeconfig=trace`, say — matches no target and silently enables
  nothing, and because `RUST_LOG` **replaces** the default filter
  rather than adding to it, such a directive on its own turns the
  logging off instead of up.
- Confirm the Hetzner Cloud Firewall has port 22 open:
  `apprafter doctor`'s **`Node reachable over SSH`** check connects
  to port 22 of the node's public IPv4, and FAILs naming the address
  when nothing accepts the connection.
- Read the node's boot log in the Hetzner Console: its Web Console
  gives you out-of-band access to it.
- If the step gives up after five minutes, it fails with
  [`apprafter::cluster::k3s_not_ready`](#k3s-not-ready): the server
  exists and bills, and cluster-bootstrap has not run. Once the node
  is up, run the same `up` command again. It finds the server instead
  of creating another and goes on with cluster-bootstrap.

### Cilium pods did not pick up a config change

They do now, and no manual roll is needed. The platform stack sets
`rollOutCiliumPods`, `operator.rollOutPods` and `envoy.rollOutPods` on the
Cilium chart, which stamps a config checksum onto each pod template — so a values change rolls the agent, the
operator and Envoy on its own.

If a Cilium pod is genuinely stuck, that is a different problem from a config
change not landing: read the agent's logs before restarting anything, because a
roll will hide the reason.

### Provisioning fails on a Hetzner quota or server-type limit {#quota}

`apprafter up` / `apply` surfaces the provider's error directly.
The common ones:

- `apprafter::provider::server_type_not_selected` — no server type
  was supplied. See the section above for the fix options.
- `apprafter::provider::server_type_unavailable` — the requested
  `(region × SKU)` pair is invalid or unavailable. See the section
  above for the four `UnavailableKind` variants and their fixes.
- A `403 forbidden` with a quota message — your Hetzner project
  has hit its server / IP / volume limit. Request a higher limit
  from the [limits overview](https://console.hetzner.com/limits) in
  the Hetzner Console (Request change → Limit increase) or free up
  resources, then re-run. `apprafter destroy --yes` clears any half-built
  resources before you retry — but it removes **every** resource
  tagged `apprafter=true` in that project, running clusters included,
  so only reach for it when the project holds nothing else
  ([scope](target-store.md#destroy-scope)).

### SSH key rejected or `ssh-key path` FAIL {#ssh-key}

Point `--ssh-key` at a real **public** key file (e.g.
`~/.ssh/id_ed25519.pub`, not the private key). The key is injected into
the node at provision time, so a change only takes effect on the next
`apply` / `up` — set it first with
`apprafter target add <name> --renew --ssh-key <path>`, which changes
the key and keeps everything else, the token included.

**What checks what, exactly**

The key file is sent to Hetzner as it is, so every place that takes it
checks that it holds one OpenSSH public key line, `<type> <base64>
[comment]`, of type `ssh-ed25519`, `ssh-rsa`, an `ecdsa-sha2` curve, or
a security-key (`sk-`) type. A private key is refused by name, and its
contents are never printed.

- `apprafter target add --ssh-key <path>` (and `--renew --ssh-key`)
  **refuses at add time** if the path does not exist (`SSH key path
  '<path>' does not exist`, code `apprafter::target::ssh_key_unreadable`)
  or if the file is not a public key (`SSH key '<path>' is a private
  key: …` or `… is not an OpenSSH public key`, code
  `apprafter::target::ssh_key_not_public`). So neither a bad path nor a
  private key gets stored in the first place.
- `apprafter apply` / `up` check the key body again before anything is
  sent to the provider: the stored path's file, an inline
  `APPRAFTER_SSH_PUBLIC_KEY`, or a manifest's `sshKeys` entry. A target
  stored before this check existed can still name a private key; `apply`
  then stops with `apprafter::target::ssh_key_not_public` and sends
  nothing.
- `apprafter doctor`'s **`SSH key readable`** row FAILs when the stored
  path has disappeared, cannot be read, or does not hold a public key (a
  private key is named as one), and otherwise PASSes, printing the key
  type:

    ```text
      ✓ SSH key readable (/home/you/.ssh/id_ed25519.pub (ssh-ed25519))
    ```

- With no key configured at all the check is a **WARN**, `SSH key path
  configured`, not a FAIL — provisioning is what refuses.

A private key's public half is the `.pub` file next to it; `ssh-keygen
-y -f <private key>` prints it again if the `.pub` is lost.

Fix the key where the message says it came from, as its help does. A
manifest's `sshKeys[i]`: replace that entry's `public_key` with the
public line. `APPRAFTER_SSH_PUBLIC_KEY`: set it to the public line, or
unset it. A target's key file:
`apprafter target add <name> --renew --ssh-key <path>.pub`, or, on a
first `target add`, the same command with the `.pub`. `apply` takes the
manifest's keys first, then the variable, then the target's key, so
changing one further down does not change what is sent.

### App stuck on `ImagePullBackOff` (registry auth) {#registry-auth}

The Deployment can't pull your image. Almost always a private
registry without (or with the wrong) credentials:

- For **GHCR**, the pull token must be a **classic** PAT with
  `read:packages` (plus `repo` if the package inherits a private
  repo's visibility). Fine-grained PATs and GitHub App tokens are
  **not** accepted by `ghcr.io` — see
  [private repos & registries](../dev-guide/private-repos-and-registries.md).
- Confirm the image ref in `apprafter/Application.cue` matches what
  you pushed (`apprafter app status <name>` shows the rendered
  image), and that the tag actually exists in the registry.
- A public image with a typo'd path fails the same way — check the
  registry path before assuming an auth problem.

### Public domain doesn't resolve or returns 5xx through Cloudflare {#dns}

The public-domain path has several moving parts; work outward:

- **DNS not resolving** — give Cloudflare time to propagate the
  nameserver change, and confirm the A/AAAA records match
  `apprafter target ip`. Records must be **Proxied** (orange
  cloud) for the origin-firewall lock to make sense.
- **521 / 522 from Cloudflare** — the origin is unreachable.
  Check the firewall toggle (`apprafter target firewall
  cloudflare-origin enable` allows only Cloudflare ranges) and
  that the zone is registered (`apprafter target domain list`).
- **526 (invalid certificate)** — SSL/TLS mode isn't **Full
  (strict)** or the imported Origin CA cert doesn't cover the
  host. Re-check `apprafter target cert import` and the zone's
  apex + wildcard coverage.

Full runbook: [connect a domain](connect-a-domain.md).

### Commands print `Node disk: …` {#node-disk}

The node's root filesystem has less than **15% free**, and the platform says
so above every `apprafter` command that reaches beyond your machine until it
does not. `--help`, `--version` and the commands that only work with local
files, such as `completion`, `target list` and `app validate`, neither print
it nor read the cluster to find out. The banner carries the figure and what
shares that filesystem:

```text
warn: Node disk: the node's filesystem is 88% full (12% free). Every workload
on this node shares it — local-path volumes, database storage, snapshots,
container images and logs.
```

It is one filesystem for all of it, which is why the warning is loud: a
database, a volume and the image store fill the same disk, and the first thing
to stop working will not be the thing that filled it.

Free space, and the banner clears itself — the condition is re-sampled and goes
back to `SufficientSpace` on its own, with nothing to acknowledge. The usual
recoveries, cheapest first: retire snapshots you no longer need
([Backup retention](backup-maintenance.md)), remove applications you have
stopped using so their volumes are released, or
[move to a bigger machine](moving-to-a-bigger-machine.md) if the workload has
simply outgrown the one it is on.

The sample is best-effort. If the node cannot be reached the previous verdict
stands rather than flipping to a reassuring one, so a banner that neither
appears nor clears is a reason to check the node itself.

### Node shows `NotReady` after bootstrap {#node-not-ready}

A freshly provisioned node stays `NotReady` until the CNI is up.
If `kubectl get nodes` doesn't reach `Ready` within a couple of
minutes of `up` completing, the Cilium agent is usually the
culprit — `kubectl -n kube-system get pods -l k8s-app=cilium` and,
if it's crash-looping, check its logs. This is distinct from the
[`k3s-ready` step](#the-k3s-ready-step-of-bootstrap-all-takes-longer-than-expected),
which is the node coming up at all, before the CNI install.

### A status says a reconcile did not finish {#reconcile-timed-out}

The operator gives every reconcile a deadline. A reconcile still running when
its deadline passes is abandoned and tried again, so one request that the
Kubernetes API never answers can no longer hold a resource without a word. A
Postgres call has a shorter limit of its own, so a Postgres server that does
not answer at all does not end up here (a shared database's `Ready` condition
gives the reason `AwaitingCluster`, which `apprafter db status` prints only
while `Ready` is false; see [When the server is slow to
answer](../how-it-works/shared-databases.md#when-the-server-is-slow-to-answer));
one that answers slowly, call after call, can. Source
credentials, migration plans and the clean-up of retained claims have
deadlines too (90 s, 190 s and 120 s), but their timeouts appear only as a
warning in the operator's log and on the metric at the end of this section.
For everything else, an abandoned reconcile shows:

| Resource | Deadline | Where it shows |
| --- | --- | --- |
| Application | 120 s | `apprafter app status`, as `ReconcileTimedOut` among its recent problems |
| A dependency's claim, while it is scheduled or provisioned | 120 s | `apprafter app status`, as a `Reconcile:` entry under the claims table |
| Shared volume | 60 s | `apprafter volume status`, as a `Reconcile:` line |
| Shared database | 120 s | `apprafter db status`, as a `Reconcile:` line |
| The platform | 120 s | `apprafter platform status` and `apprafter status`, as `ReconcileStalled=True` |

The `Reconcile:` line of `apprafter volume status` reads, for example:

```text
  Reconcile:   last timed out 3 minutes ago (did not finish within 60s); the operator retries on its own
```

Apart from what the table lists, a timeout writes nothing to the resource's
status, so the rest of the status these commands print can be older than the
timeout. What they read live from the cluster, such as an application's pods,
is current. The `Reconcile:` line comes from a `Warning` Event with reason
`ReconcileTimedOut`, which `kubectl describe` also shows. The line goes once a
later reconcile of the same controller changes the resource's status, in a
later second than the Event (for a claim, its size and capacity figures do not
count), or once the Event expires, an hour by default, whichever comes first.
A timeout that keeps coming back writes no status but leaves a new Event on
each retry, so its line stays and its age stays short. A reconcile that
finishes without changing the status leaves no record that it finished, so
after such a recovery the line stays until the Event expires, and its age
keeps growing: a timeout whose age keeps growing has not come back.
`ReconcileStalled` goes once a later platform reconcile finishes without an
error; [How the platform upgrades
itself](../how-it-works/platform-upgrades.md#when-the-controllers-own-reconcile-stalls)
says what it does and does not point at.

A single timeout needs nothing. One that repeats points at what that
reconcile waits on: the Kubernetes API itself (a `kubectl get` of the same
resource is slow or does not answer), or the Postgres cluster for a shared
database or a `needs.pg` claim. A node's kubelet that does not answer never
causes a timeout: a shared volume keeps its last capacity figure, and its
`CapacityWarning` condition says the figure was not re-measured. `apprafter
volume status` shows that note only while the volume is warning; otherwise the
kept figure prints as an ordinary line. Every abandoned
reconcile is also counted on the operator's
`apprafter_reconcile_timeouts_total` metric, by kind of resource. A healthy
operator never increments it, so the metric does not appear at all until the
first timeout.

## Reading the rendered output

A worked example. After `apprafter target add bad --token
"$(python -c 'print("a"*64)')" …`:

```text
Error: apprafter::target::token_rejected

  × provider `hetzner-cloud` rejected the supplied token
  ╰─▶ apprafter::provider::hetzner_api_error

        × hetzner-cloud GET /v1/locations failed (status 401):
        │ unauthorized: the token you have provided is invalid
        help: The Hetzner Cloud API refused the request:
              • 401 unauthorized — …
              • 403 forbidden — …
              • 429 rate limit — …
              • 5xx — …

  help: The provider's read-only credential check returned 401
        unauthorized: the token was mistyped, or it was revoked or
        rotated, or its project was deleted.
        • A token is shown only once, when it is created: …
        • If you're rotating, run `apprafter target add <name> --renew`
        instead of re-creating the target …
```

The OUTER `Error:` line names the operator-facing scenario. The
OUTER `help:` block walks the rotation flow. The INNER `╰─▶`
arrow opens the provider's underlying view: API call, status,
generic-API help.

For grep'ping CI logs: stable codes survive renames. Group your
runbooks by code, not by the human-readable summary.

## See also

- [Operator quickstart](./quickstart.md) — happy-path setup.
- [The target store on disk](../how-it-works/the-target-store.md) — where a
  credential-chain failure comes from, and the decision behind the chain.
- [CLI reference](../reference/cli/index.md) — every
  subcommand + flag.
