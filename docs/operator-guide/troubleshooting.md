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

**Fix.** Enter the project's Nix dev shell (`nix develop` from the
repo root). If you don't use Nix, follow `docs/contributing/setup.md`
for direct-install options.

### `apprafter::env::cue_export_failed`

A `cue export` call rejected the manifest with non-zero exit code.

**Fix.** Run `cue vet <manifest>` against the same file to
reproduce the parse / type error locally. The captured stderr in
the diagnostic body usually points at the offending expression.

### `apprafter::provider::hetzner_api_error`

The Hetzner Cloud API returned a non-2xx response.

**Fix.** Read the inner help — it enumerates the four most common
failure families:

- **401 unauthorized** — the stored token was rotated or revoked.
  Run `apprafter target add <name> --renew --token <new>` to
  refresh it.
- **403 forbidden** — the token's project lacks permission for
  this resource type.
- **429 rate limit** — back off and retry; if persistent, the
  project may need a quota increase.
- **5xx** — provider-side outage. Check
  https://status.hetzner.com/.

After fixing the root cause, run `apprafter doctor` to confirm
reachability.

### `apprafter::provider::server_type_unavailable`

Pre-flight rejection: the requested Hetzner server type isn't
available in the requested region (or has been retired entirely
— e.g. `cx22` was retired in early 2026, replaced by `cpx22`).

**Fix.** Pick one of the alternatives suggested in the error
body, or set `APPRAFTER_MANIFEST` to a manifest with a different
`nodes[0].kind`. Once `--server-type` lands, that flag will be the
recommended override.

### `apprafter::state::corrupt`

`<cwd>/.apprafter/state.json` failed to parse. The file was
written by a previous `apply` / `import` and may have been
hand-edited or copied across incompatible CLI versions.

**Fix.** If the file looks salvageable, edit it by hand. Otherwise
delete `.apprafter/` and run `apprafter import` — the
`apprafter=true` Hetzner label is the canonical idempotency
anchor, so `import` rebuilds state from live API objects.

### `apprafter::target::invalid_config`

A YAML file under `$XDG_CONFIG_HOME/apprafter/` failed to parse.
Either hand-edited or written by an incompatible CLI version.

**Fix.** Either fix the YAML by hand (these files are small), or
nuke just the offending target's directory under
`$XDG_CONFIG_HOME/apprafter/targets/<name>/` and re-create with
`apprafter target add <name> --provider hetzner-cloud …`. The
global `config.yaml` is the only file shared across targets;
treat it as the last line of defence.

### `apprafter::target::not_found`

A subcommand asked for a target that isn't in the store.
Variants:

- `--target ghost` against a populated store with no `ghost`
  target.
- Any subcommand reading the active pointer when no target has
  been created yet.

**Fix.** `apprafter target list` shows what's in the store.
`apprafter target add <name> …` creates a new one; the first add
on a fresh store auto-activates it. If `available: ` shows
nothing, you're seeing the empty-store first-run case.

### `apprafter::target::token_rejected`

The provider's read-only credential check returned 401 /
unauthorized. **Distinct from** the generic Hetzner API error —
this fires only on the explicit `target add` ping path, so the
help text targets the rotation flow specifically.

**Fix.** Read the layered help:

- Verify the token at https://console.hetzner.cloud/projects →
  Security → API Tokens. It must say `Read & Write` next to
  the project.
- Copy the token again — the most common cause is a trailing
  newline from a clipboard manager (Hetzner tokens are 64
  ASCII chars, no prefix).
- If you're rotating, use `apprafter target add <name> --renew
  --token <new>` instead of re-creating the target.
- For offline / CI seeding, pass `--no-ping` to skip the
  network round-trip and save the target anyway.

### `apprafter::target::provider_unreachable`

The credential check ping failed for a **non-401** reason —
transport error (`connection refused`, DNS failure), 5xx
provider-side outage, or 429 rate limit. The token may still
be valid once the API recovers — the help text intentionally
avoids any rotation suggestion that would misdirect operators.

**Fix.**

- `apprafter doctor` to confirm DNS + reachability.
- Check the provider's status page
  (https://status.hetzner.com/ for hetzner-cloud).
- VPN / corporate proxy: ensure `https://api.hetzner.cloud/`
  is reachable.
- `--no-ping` to save the target offline and verify later.

### `apprafter::io::error` / `apprafter::io::json` / `apprafter::io::yaml`

Low-level filesystem / network IO error, or
encode/decode error on `state.json` (JSON) /
`config.yaml` / `credentials.yaml` (YAML).

**Fix.** The captured OS message names the failing path or
socket. Common cases: missing directory, wrong permissions
(`chmod 0600` on credentials), full disk. For decode failures on
target-store files, re-create the offending target with
`apprafter target add <name> --force`.

### `apprafter::cli::other`

Catch-all for messages that haven't been promoted to a typed
variant yet. The error code itself is stable — log-analytics
piped through this code surface as candidates for typed
variants in future releases.

**Fix.** Read the message text. If you see the same wording often,
file an issue — recurring catch-all messages should be promoted
to a typed variant with its own help text.

## Common walk-found failures

### "state has no provider — run `apprafter init` first"

Before v0.1.83 this fired after `target add` because the
operational commands only consulted `state.json` for provider /
region. v0.1.83 wired the active target's `config.yaml` as a
fallback. If you still see this on v0.1.83+, your store's
`config.yaml` for the active target is missing the `provider`
field — fix by hand or recreate the target.

### Phase 2 of `bootstrap-all` always takes ~1 minute

Tracked in plan.md (Track A.9 backlog). Hetzner cloud-init takes
90–180 s; the first SSH attempt blocks on the kernel's TCP
connect timeout (~30 s) until sshd is up. With the current
`KUBECONFIG_POLL_INTERVAL = 10 s`, the typical run hits
30 + 10 + 10 ≈ 50–60 s before attempt 3 succeeds.

Not a bug; will be reduced when the SSH wrapper learns to pass
`-o ConnectTimeout=5`.

### Token rotation accepted silently with the same value

Fixed in v0.1.78. `apprafter target add <name> --renew --token
<x>` byte-compares the new value against the stored one; identical
input errors with a hint pointing at the Hetzner Cloud Console.

### `apprafter target add` printed plaintext to stderr

Walk-found in v0.1.78. The wizard's "✓ Token verified" line is
fine; the prior "(token bytes: <value>)" debug line is gone. If
you see the value echoed anywhere in v0.1.78+, file an issue.

### Cilium CNI did not roll over to v6 after Cilium chart update

Track B 1.70 territory. The Cilium chart `upgrade-install` does
not trigger a DaemonSet restart on every config change; manually
`kubectl -n kube-system rollout restart ds/cilium` after the
chart bump.

## Reading the rendered output

A worked example. After `apprafter target add bad --token
"$(python -c 'print("a"*64)')" …`:

```text
Error: apprafter::target::token_rejected

  × provider `hetzner-cloud` rejected the supplied token
  ╰─▶ apprafter::provider::hetzner_api_error

        × hetzner-cloud GET /v1/locations failed (status 401):
        │ unauthorized: the token you have provided is invalid
        help: The Hetzner Cloud API returned a non-2xx response. …
              • 401 unauthorized — …
              • 403 forbidden — …
              • 429 rate limit — …
              • 5xx — …

  help: The provider's read-only credential check returned 401 /
        unauthorized. …
        • Verify the token at https://console.hetzner.cloud/projects → …
        • Copy the token again …
        • If you're rotating, run `apprafter target add <name>
        --renew --token <new>` …
        • Pass `--no-ping` to skip the check …
```

The OUTER `Error:` line names the operator-facing scenario. The
OUTER `help:` block walks the rotation flow. The INNER `╰─▶`
arrow opens the provider's underlying view: API call, status,
generic-API help.

For grep'ping CI logs: stable codes survive renames. Group your
runbooks by code, not by the human-readable summary.

## See also

- [`quickstart.md`](./quickstart.md) — happy-path setup.
- [`target-store.md`](./target-store.md) — credential resolution
  chain reference.
- [`docs/reference/cli.md`](../reference/cli.md) — every
  subcommand + flag.
- [ADR 0030](../adr/0030-cli-target-store-and-credential-chain.md)
  — design rationale for the diagnostic-code scheme.
