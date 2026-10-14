---
description: "How the CLI persists a target on disk, what each file holds and in what form, and the order the credential chain is resolved in."
---

# The target store on disk

What `apprafter target add` writes, where, and in what form — and the order
every later command resolves a credential in. The recipe is
[Managing targets](../operator-guide/target-store.md); none of this is needed
to use one.

It is worth reading when a target resolves to something you did not expect,
when you are moving a store between machines, or when you are deciding whether
a value belongs in a file, an environment variable or a flag.

The design rationale — why a store at all, and why the chain is ordered this
way — is
[ADR 0030](../adr/0030-cli-target-store-and-credential-chain.md).

## File layout

```text
$XDG_CONFIG_HOME/apprafter/          # ~/.config/apprafter on Linux
├── .lock                            # the store's write lock; always empty
├── config.yaml                      # GlobalConfig
├── store.yaml                       # whether the secrets are locked (after apprafter lock on)
├── vault                            # a locked store's secrets, encrypted; mode 0600
├── targets/
│   ├── default/
│   │   ├── config.yaml              # TargetConfig (non-secret)
│   │   └── credentials.yaml         # TargetCredentials, mode 0600
│   └── work/
│       ├── config.yaml
│       └── credentials.yaml
├── auth/                            # reserved for `apprafter auth` (Managed; stub)
│   └── .keep
├── state/
│   └── <target>/
│       └── .apprafter/
│           ├── state.json           # provisioned resource IDs + cached kubeconfig, mode 0600
│           └── known_hosts          # per-cluster SSH known_hosts
└── run/                             # private: decrypted kubeconfigs while a command uses them
```

The `state/<target>/` half is not a scratch cache. It is the only
local record of what a target provisioned — the Hetzner server,
network, firewall and floating-IP IDs, plus the age-encrypted
kubeconfig. Everything under it is keyed by target name, which is why
[renaming a target](../operator-guide/target-store.md#inspecting-renaming-and-removing-a-target) moves
it and removing one deletes it.

A store that `apprafter lock on` has locked keeps every token and its own
age key in `vault`, and each `credentials.yaml` holds a marker naming it
instead of a token: [How the store lock works](the-store-lock.md#layout).

Every command that changes the store takes its write lock, `.lock`,
while it reads, changes and saves, so two processes editing one store
never lose each other's change. One that finds the lock held waits for
it, and says so on stderr:

```text
waiting for another AppRafter process to finish writing the target store (/home/operator/.config/apprafter/.lock)…
```

A store whose write lock cannot be taken at all — a read-only directory, a
filesystem without locks — is used without it, after a
`warning: cannot take the target store's write lock (…): …; continuing without it` line.

The store is used only while it is your account's alone. An account that
owns one of its folders — the store itself, `targets` and each target's
folder, `state` and each folder under it — or can write to one could put
its own token, kubeconfig or vault there. So before a command uses or
writes any of the store's secrets, and before any change under the write
lock, AppRafter reads who owns each of those folders and who may write to
it, and refuses the store if another account does
([`store_not_private`](../operator-guide/troubleshooting.md#store-not-private)).
`apprafter doctor` names each such folder, and each secret file another
account owns, and changes nothing
([troubleshooting](../operator-guide/troubleshooting.md#store-folder-foreign)).

Every folder AppRafter creates in the store — the store itself, `targets`
and each target's folder, `auth`, `state` and each folder under it, `run` —
is your account's alone from the moment it exists, whatever the umask: mode
0700 on Linux and macOS, so a shared group's umask such as 002 does not make
it group-writable, and on Windows a DACL that admits only your account and
takes nothing from the folders above. A store AppRafter creates therefore
passes the check above, and `apprafter target add` on a fresh store is not
refused after it asked for a token. The folders the store sits in, such as
`~/.config` when AppRafter has to create it, are created with your umask as
any program creates them, and are not checked. A folder that is already
there is never changed: one another account can change is refused, not
fixed.
`cli/cli-core/src/store_privacy.rs`, `cli/cli-core/src/owner_only.rs`.
Each file is replaced whole: a save writes a temporary file
beside it and renames that into place, so a reader sees the old version
or the new one, never a partial write.

### `config.yaml` (global)

```yaml
active_target: prod
version: 1
```

Exactly one `active_target`. Empty string == "no active target",
which makes most operational commands fail-loud with an
onboarding hint pointing at `apprafter target add`. `version` is the
on-disk format revision (`1` today). It is spelled `version`, not
`schema_version`: a hand-written file using the latter is rejected as
`apprafter::target::invalid_config` with `missing field "version"`.

Override the entire store root via `APPRAFTER_CONFIG_DIR`:

```sh
APPRAFTER_CONFIG_DIR=/tmp/sandbox-store apprafter target add ...
```

The env value is used verbatim — no `apprafter/` suffix appended.
Primarily for tests; power users may use it for compartmentalised
experimentation.

### `targets/<name>/config.yaml` (per-target, non-secret)

```yaml
provider: hetzner-cloud
region: nbg1
default_tier: solo
cluster_name: platform-1
ssh_key_path: /home/operator/.ssh/id_ed25519.pub
firewall: null
server_type: null
```

Every key is written on every save — an unset optional is serialised as
`null`, not omitted, so a freshly created target's file is the block
above with `cluster_name`, `ssh_key_path`, `firewall` and `server_type`
all `null`.

Field reference:

| Field          | Required | Notes                                                              |
| -------------- | -------- | ------------------------------------------------------------------ |
| `provider`     | yes      | Today: `hetzner-cloud`. AWS and OpenBao are not implemented yet.   |
| `region`       | no       | Provider-specific (Hetzner: `nbg1`, `fsn1`, `hel1`, …).            |
| `default_tier` | no       | `solo` / `team` / `prod` / `regulated`. Hint for `init` / `up`.    |
| `cluster_name` | no       | Falls back to `platform-1`.                                        |
| `ssh_key_path` | no       | Path (not body). Source-of-truth stays in `~/.ssh/`.               |
| `firewall`     | no       | Cloud-firewall toggles for this target, written by `apprafter target firewall`. |
| `server_type`  | no       | Preferred Hetzner server type (`cx22`, `ccx23`, …), written by `apprafter target machine`. This is the "target preference" rung of the server-type chain and the field `target show` reports as `Server type:`. |

### `targets/<name>/credentials.yaml` (per-target, **mode 0600**)

```yaml
hetzner_token: hxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx
```

Mode 0600 is enforced on every write. On Windows every write
gives it instead a DACL that admits only your account and takes
nothing from the folders above, so a store kept in a folder other
accounts can read keeps the token from them. `state.json` and the
vault get the same on every write, and the age key when it is
created, along with its folder when AppRafter creates that too
(`cli/cli-core/src/owner_only.rs`). The age key is written whole to
a temporary file beside it and takes its name only if no key is there yet,
so a write that fails leaves no part of a key behind, and two commands that
make it at once end with the same key; a key file that does not parse is
an error that names it, never replaced (`cli/cli-core/src/secrets.rs`). On
Windows a file written
before AppRafter gave it that DACL — or widened since — gets it the next
time AppRafter reads the file: a command, or the app. `apprafter doctor`
names any of these files another account may open — a mode wider than
0600 on Linux and macOS, a DACL that is not yours alone on Windows — and
changes nothing itself
([troubleshooting](../operator-guide/troubleshooting.md#secret-file-wide)).
The CLI redacts the value
in `target show` / `whoami` output; read this file directly if
you need the raw bytes for a one-off script.

**Do not commit this file to dotfiles repos.** In a store in files the
token is a plaintext credential, and the security boundary is filesystem
permissions; `apprafter lock on` moves it into an encrypted vault, leaving
a marker here. The R1 mitigation in
[ADR 0030](../adr/0030-cli-target-store-and-credential-chain.md): `apprafter doctor`
warns on mode != 0600.

## Credential resolution chain

Every operational command (`apply`, `destroy`, `import`,
`kubeconfig --refresh`, the `k3s-ready` step of `up`)
resolves the
Hetzner token in this order:

1. **`--flag` value** (where the subcommand exposes one — today
   only `target add --token` does). Wins everything.
2. **Environment variable `HCLOUD_TOKEN`**. Wins over the store.
3. **Active target's `credentials.yaml`** — or `--target <name>`
   override for one-off runs against a non-active target.

On a locked target store, rung 2 is refused — `HCLOUD_TOKEN` set is
`apprafter::secrets::override_refused` — and rung 3 is the store's vault,
opened as [How the store lock works](the-store-lock.md#opening) describes.

SSH public key resolution is analogous (`APPRAFTER_SSH_PUBLIC_KEY`
env → target store's `ssh_key_path` → read the file).

There is **no single "chain tried 1/2/3" error**. Each rung fails with
its own message, and which one you get tells you where you actually
are. Their codes are `apprafter::target::no_active`,
`apprafter::cli::other` and `apprafter::target::not_found`, in the
order below; the `help:` footer is omitted here.

On an empty store you never reach the token chain at all — resolving
the per-target state directory refuses first, so this is the message a
first run produces:

```text
  × no active target — run `apprafter target add <name> --provider hetzner-
  │ cloud …` first, or supply `--target <name>` to point at a specific one
```

With a target configured but no token in its `credentials.yaml`:

```text
  × target `prod` has no provider token stored
  help: Run `apprafter target add prod --renew` to store one (at a terminal it
        asks for the new token without showing it; in a script, `--token`), or
        set `HCLOUD_TOKEN` for this invocation.
```

And with `--target` pointing at a name that is not in the store — note
that it lists what is:

```text
  × target `ghost` not found (available: prod)
```

Each names the rung that failed and, in its message or its help, the
next thing to type, which is what you need; what none of them does is
enumerate the other two, so do not go looking for a rung-by-rung
report.

## See also

- [Managing targets](../operator-guide/target-store.md) — the commands that
  write what this page describes.
- [Environment variables](../reference/environment.md) — every variable the
  chain above consults, with its default and the code that reads it.
- [How the store lock works](the-store-lock.md) — what a locked store
  keeps where, and how it is opened.
