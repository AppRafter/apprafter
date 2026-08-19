---
description: "The on-disk layout of target configuration, the credential resolution chain, and the multi-target patterns operators actually use."
---

# Target store reference

> Reference companion to [`quickstart.md`](./quickstart.md). The
> quickstart covers the happy path; this page documents the
> on-disk layout, the credential resolution chain, and the
> multi-target patterns operators actually use.
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
    └── <target>/                    # per-target runtime cache. Reserved.
```

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
| `provider`     | yes      | Today: `hetzner-cloud`. AWS / OpenBao / Managed land in M2+.       |
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
