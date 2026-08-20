---
description: "The on-disk layout of target configuration, the credential resolution chain, the multi-target patterns operators actually use, and the inspect / rename / remove lifecycle — including why removing a target does not remove its servers."
---

# Target store reference

> Reference companion to [`quickstart.md`](./quickstart.md). The
> quickstart covers the happy path; this page documents the
> on-disk layout, the credential resolution chain, the
> multi-target patterns operators actually use, and the rest of a
> target's life — inspecting one, renaming one, and removing one
> without stranding the servers it provisioned.
>
> Authoritative design rationale: [ADR
> 0030](../adr/0030-cli-target-store-and-credential-chain.md).

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
[renaming a target](#inspecting-renaming-and-removing-a-target) moves
it and removing one deletes it.

### `config.yaml` (global)

```yaml
schema_version: 1
active_target: prod
```

Exactly one `active_target`. Empty string == "no active target",
which makes most operational commands fail-loud with an
onboarding hint pointing at `apprafter target add`.

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
```

Field reference:

| Field          | Required | Notes                                                              |
| -------------- | -------- | ------------------------------------------------------------------ |
| `provider`     | yes      | Today: `hetzner-cloud`. AWS and OpenBao are not implemented yet.   |
| `region`       | no       | Provider-specific (Hetzner: `nbg1`, `fsn1`, `hel1`, …).            |
| `default_tier` | no       | `solo` / `team` / `prod` / `regulated`. Hint for `init` / `up`.    |
| `cluster_name` | no       | Falls back to `platform-1`.                                        |
| `ssh_key_path` | no       | Path (not body). Source-of-truth stays in `~/.ssh/`.               |

### `targets/<name>/credentials.yaml` (per-target, **mode 0600**)

```yaml
hetzner_token: hxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx
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
`kubeconfig --refresh`, the `k3s-ready` step of `bootstrap-all`)
resolves the
Hetzner token in this order:

1. **`--flag` value** (where the subcommand exposes one — today
   only `target add --token` does). Wins everything.
2. **Environment variable `HCLOUD_TOKEN`**. Wins over the store.
3. **Active target's `credentials.yaml`** — or `--target <name>`
   override for one-off runs against a non-active target.

SSH public key resolution is analogous (`APPRAFTER_SSH_PUBLIC_KEY`
env → target store's `ssh_key_path` → read the file).

When nothing resolves, the typed error lists **all three** paths.
End-to-end runs confirmed this is what operators read first:

```text
Error: apprafter::cli::other
  × no Hetzner token configured. Resolution chain tried:
  │  1. --token flag — not set
  │  2. HCLOUD_TOKEN env — not set
  │  3. active target — no target store at /home/op/.config/apprafter/
```

## Common patterns

### Single-cluster operator

```sh
apprafter target add prod \
    --provider hetzner-cloud \
    --token "$HCLOUD_TOKEN" \
    --region nbg1 \
    --tier solo \
    --ssh-key ~/.ssh/id_ed25519.pub
```

First target is auto-active. Every subsequent command resolves
through the store. `HCLOUD_TOKEN` can be unset after `target add`.

### Multi-cluster operator (dev + prod)

```sh
apprafter target add prod ... --region nbg1
apprafter target add dev  ... --region fsn1

apprafter target list
# Active │ Name  │ Provider       │ Region │ Tier
#   *    │ prod  │ hetzner-cloud  │ nbg1   │ solo
#        │ dev   │ hetzner-cloud  │ fsn1   │ solo

apprafter target use dev          # switch active
apprafter up --dry-run            # → shows Target: dev (active)

# Or one-off override without switching active:
apprafter apply --target dev
apprafter destroy --target dev --yes
```

### CI / scripted setup (env-var only)

`HCLOUD_TOKEN` from a secret manager remains step 2 in the chain:

```sh
export HCLOUD_TOKEN="$(vault read -field=token secret/apprafter/hcloud)"
export APPRAFTER_SSH_PUBLIC_KEY="$(cat ./deploy-key.pub)"

apprafter init --provider hetzner-cloud --tier solo --region nbg1
apprafter up
```

No target store touched. Backwards-compat with the pre-v0.1.69
flow is preserved.

### Token rotation (no target re-creation)

```sh
apprafter target add prod --renew --token "$NEW_TOKEN"
```

`--renew` updates only the credentials half of an existing
target. Fails if the target doesn't exist (`--force` would
replace the entire target). The wizard pings the new token
before saving; pass `--no-ping` to skip the round-trip.

The token bytes are byte-compared against the stored value —
identical input is rejected with a hint pointing at the Hetzner
Cloud Console to confirm rotation actually happened.

### Per-machine target with stricter perms

```sh
# Lock the entire store dir to your user
chmod 700 ~/.config/apprafter
# Already enforced per-file, but mode-700 on the dir blocks
# even directory listings by other users on the box.
```

`doctor` will continue to pass; only the directory above is
tightened.

## Inspecting, renaming and removing a target

The patterns above create targets and switch between them. These three
commands finish the lifecycle. All of them are **local** — they read
and write the store on this machine and never call your cloud
provider, which is also why `target remove` is the one to be careful
with.

### Inspect one — `apprafter target show`

With no argument, `show` reports the **active** target. Pass a name to
read another one without switching to it:

```sh
apprafter target show            # the active target
apprafter target show dev        # another one; the active target is unchanged
```

```text
Target: prod (active)
  Provider:    hetzner-cloud
  Region:      nbg1
  Server type: not set
  Default tier: solo
  Cluster name: not set
  SSH key:     not set
  Hetzner token: set (64 chars; read credentials.yaml for the raw value)

Config:      /home/operator/.config/apprafter/targets/prod/config.yaml
Credentials: /home/operator/.config/apprafter/targets/prod/credentials.yaml (mode 0600)
```

How to read it:

- **`(active)`** is printed only when the target you asked about is the
  active one, so `show` with no argument doubles as "which target am I
  on".
- **`not set`** is an empty optional field, not a fault. `Cluster name:
  not set` means provisioning will use the `platform-1` default;
  `Server type: not set` means this target contributes nothing to the
  server-type resolution chain, and `apprafter target machine` is what
  fills it in — see [Choosing the
  machine](choosing-the-machine.md#which-one-wins) for the rest of that
  chain.
- **The token line reports presence and length only.** The bytes are
  never printed, here or in `apprafter whoami`. Read
  `credentials.yaml` directly if a script needs the value.
- **The two paths at the foot** are the files every field above was
  read from — useful when you are not sure which store an
  `APPRAFTER_CONFIG_DIR` in your shell is pointing at.

Two refusals are worth recognising. On an empty store, or after the
active pointer has been cleared:

```text
× no active target and no name supplied. Run `apprafter target list` to see
│ configured targets, or `apprafter target add` to create one.
```

and on a name that is not in the store — note that it lists what *is*:

```text
Error: apprafter::target::not_found

  × target `ghost` not found (available: dev, prod)
```

### Rename one — `apprafter target rename`

```sh
apprafter target rename prod production
```

```text
target renamed: `prod` → `production` (active pointer updated)
```

The parenthetical appears only when the renamed target was the active
one; the pointer is moved for you, so you are still on the same target
under its new name.

**A rename is safe on a target with a live cluster.** Configuration,
credentials and the per-target state directory all move together, so
the next `apprafter apply`, `apprafter kubeconfig` or `apprafter
destroy` finds exactly the state it had before, under the new name.

What does *not* move is anything outside the store that names the old
target: a `--target prod` in a CI job, a shell alias, a runbook. Those
are yours to update.

The destination name follows the same rule as `target add`:
alphanumeric and `-`, not starting or ending with `-`, at most 64
characters. Three things are refused outright, each before anything is
written:

- **the destination already exists.** Rename never merges two targets;
  the error suggests picking a different name, or removing the existing
  target first.
- **the source does not exist.** The same `not_found` error as above,
  listing the targets that do.
- **the two names are identical.** Nothing to rename, and the CLI says
  so rather than performing a no-op.

### Remove one — `apprafter target remove`

Read this before you run it.

> **`target remove` deletes the local record, not the servers.** The
> per-target state directory goes with the target, and that directory
> is where the CLI keeps the IDs of the server, network, firewall and
> floating IPs it provisioned. Remove a target whose cluster is still
> running and the machines keep running — and keep billing — with
> nothing left on your machine pointing at them.

So the order is destroy, then remove:

```sh
apprafter destroy --yes --target prod
apprafter target remove prod --yes
```

If you did it the other way round, you have lost the record and not
the cluster. Recreate the target with the same token and region, then
rebuild the record from the account itself:

```sh
apprafter target add prod --provider hetzner-cloud --token "$HCLOUD_TOKEN" --region nbg1
apprafter import
```

`import` is read-only: it lists the resources labelled `apprafter=true`
in the account and writes them back into the target's state, creating
and deleting nothing. The cached kubeconfig is not among them, because
it never lived in your cloud account — `apprafter kubeconfig --refresh`
fetches a fresh one over SSH.

**Confirmation is not optional.** At a terminal you are prompted, and
the prompt defaults to *no*. Without a terminal — a CI job, a pipeline,
a scripted teardown — there is no prompt, and no silent deletion
either:

```text
× non-interactive invocation: pass `--yes` to confirm removing target `dev`
│ (refusing silent destruction)
```

**The active pointer is repaired for you.** Removing a target that was
not active just removes it. Removing the active one moves the pointer
to the alphabetically first target that remains:

```text
target `prod` removed; active switched to `dev` (alphabetically next)
```

and removing the last target clears the pointer entirely, returning the
store to its fresh state — the next `apprafter target add` greets you
as a first run and auto-activates what it creates:

```text
target `dev` removed; no targets left, active pointer cleared
```

## Anti-patterns

- **Committing `credentials.yaml` to a dotfiles repo.** Plaintext
  token. Use env-var + secret manager instead.
- **Pre-creating `credentials.yaml` by hand.** The atomic-write
  helpers guarantee mode 0600 — hand-edited files may end up
  group-readable.
- **Symlinking `~/.config/apprafter/targets/*/credentials.yaml`
  to a shared file.** Each target needs its own; sharing breaks
  `apprafter target use` semantics.

## See also

- [`troubleshooting.md`](./troubleshooting.md) — diagnostic-code
  catalogue, including every credential-chain failure surface.
- [`docs/reference/cli/`](../reference/cli/index.md) — full
  `apprafter target …` subcommand reference.
- [ADR 0030](../adr/0030-cli-target-store-and-credential-chain.md)
  — design rationale.
