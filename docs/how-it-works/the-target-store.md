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
├── config.yaml                      # GlobalConfig
├── targets/
│   ├── default/
│   │   ├── config.yaml              # TargetConfig (non-secret)
│   │   └── credentials.yaml         # TargetCredentials, mode 0600
│   └── work/
│       ├── config.yaml
│       └── credentials.yaml
├── auth/                            # reserved for `apprafter auth` (Managed; stub)
│   └── .keep
└── state/
    └── <target>/
        └── .apprafter/
            ├── state.json           # provisioned resource IDs + cached kubeconfig
            └── known_hosts          # per-cluster SSH known_hosts
```

The `state/<target>/` half is not a scratch cache. It is the only
local record of what a target provisioned — the Hetzner server,
network, firewall and floating-IP IDs, plus the age-encrypted
kubeconfig. Everything under it is keyed by target name, which is why
[renaming a target](../operator-guide/target-store.md#inspecting-renaming-and-removing-a-target) moves
it and removing one deletes it.

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

Mode 0600 is enforced on every write. The CLI redacts the value
in `target show` / `whoami` output; read this file directly if
you need the raw bytes for a one-off script.

**Do not commit this file to dotfiles repos.** The token is a
plaintext credential — the security boundary is filesystem
permissions. ADR 0030's R1 mitigation: `apprafter doctor` warns
on mode != 0600.

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

SSH public key resolution is analogous (`APPRAFTER_SSH_PUBLIC_KEY`
env → target store's `ssh_key_path` → read the file).

There is **no single "chain tried 1/2/3" error**. Each rung fails with
its own message, and which one you get tells you where you actually
are. All three are `apprafter::cli::other`; the `help:` footer is
omitted here.

On an empty store you never reach the token chain at all — resolving
the per-target state directory refuses first, so this is the message a
first run produces:

```text
  × no active target — run `apprafter target add <name> --provider hetzner-
  │ cloud …` first, or supply `--target <name>` to point at a specific one
```

With a target configured but no token in its `credentials.yaml`:

```text
  × target `prod` has no Hetzner Cloud token stored. Run `apprafter target add
  │ prod --renew --token <X>` to add one, or pass `--token`/`HCLOUD_TOKEN` for
  │ this invocation.
```

And with `--target` pointing at a name that is not in the store — note
that it lists what is:

```text
  × target `ghost` not found (available: prod). Pass `--target <name>` with a
  │ configured name, or `apprafter target use <name>` to switch the active
  │ pointer.
```

Each names the rung that failed and the next thing to type, which is
what you need; what none of them does is enumerate the other two, so
do not go looking for a rung-by-rung report.

## See also

- [Managing targets](../operator-guide/target-store.md) — the commands that
  write what this page describes.
- [Environment variables](../reference/environment.md) — every variable the
  chain above consults, with its default and the code that reads it.
