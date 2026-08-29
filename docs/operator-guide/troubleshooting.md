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

### `apprafter::provider::server_type_not_selected`

Provisioning a new machine requires an explicit server type, and
none was found in the resolution chain (`--server-type` flag >
`spec.nodes[0].type` in the manifest > `HetznerCloudState.server_type`
in state > `TargetConfig.server_type` in the target store >
`APPRAFTER_SERVER_TYPE` env). The error fires only on the **create
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

## Common failures found in end-to-end runs

### "state has no provider — run `apprafter init` first"

Before v0.1.83 this fired after `target add` because the
operational commands only consulted `state.json` for provider /
region. v0.1.83 wired the active target's `config.yaml` as a
fallback. If you still see this on v0.1.83+, your store's
`config.yaml` for the active target is missing the `provider`
field — fix by hand or recreate the target.

### The `k3s-ready` step of `bootstrap-all` takes longer than expected

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
- Confirm the Hetzner Cloud Firewall has port 22 open
  (`apprafter doctor` sanity-checks DNS + reachability, not
  port-level connectivity).
- Try `apprafter kubeconfig --refresh` once the cluster reports
  ready in the Hetzner Cloud Console (the Web Console gives you
  out-of-band access to the boot log).

### Token rotation accepted silently with the same value

Fixed in v0.1.78. `apprafter target add <name> --renew --token
<x>` byte-compares the new value against the stored one; identical
input errors with a hint pointing at the Hetzner Cloud Console.

### `apprafter target add` printed plaintext to stderr

Found in an end-to-end run of v0.1.78. The wizard's "✓ Token verified" line is
fine; the prior "(token bytes: <value>)" debug line is gone. If
you see the value echoed anywhere in v0.1.78+, file an issue.

### Cilium pods did not pick up a config change

They do now, and no manual roll is needed. The platform stack sets
`rollOutCiliumPods`, `operator.rollOutPods` and `envoy.rollOutPods` on the
Cilium chart (`platform-stack/cue/component_cilium.cue`), which stamps a config
checksum onto each pod template — so a values change rolls the agent, the
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
  has hit its server / IP / volume limit. Raise the limit in the
  Hetzner Cloud Console (Project → Limits) or free up resources,
  then re-run. `apprafter destroy --yes` clears any half-built
  resources before you retry — but it removes **every** resource
  tagged `apprafter=true` in that project, running clusters included,
  so only reach for it when the project holds nothing else
  ([scope](target-store.md#destroy-scope)).

### SSH key rejected or `ssh-key path` FAIL {#ssh-key}

Point `--ssh-key` at a real **public** key file (e.g.
`~/.ssh/id_ed25519.pub`, not the private key). The key is injected into
the node at provision time, so a change only takes effect on the next
`apply` / `up` — re-add the target with `--force --ssh-key <path>`
first.

**What checks what, exactly** — because the obvious check does less
than it looks:

- `apprafter target add --ssh-key <path>` **refuses at add time** if
  the path does not exist: `SSH key path '<path>' does not exist`
  (code `apprafter::cli::other`). So a bad path never gets stored in
  the first place.
- `apprafter doctor`'s check is named **`SSH key readable`** — not
  "SSH public key readable" — and that is all it verifies. It FAILs
  when the stored path has since disappeared or cannot be read, and
  otherwise PASSes, printing the file's first whitespace-delimited
  token as the algorithm. It does **not** validate that the file is a
  public key: pointed at a file containing `not-a-key` it reports

    ```text
      ✓ SSH key readable (/path/to/fake.pub (not-a-key))
    ```

    and pointed at a *private* key it passes just as happily, printing
    `✓ SSH key readable (…/id_ed25519 (-----BEGIN))`. A green tick here
    means "a file is there", not "the key is right".
- With no key configured at all the check is a **WARN**, `SSH key path
  configured`, not a FAIL — provisioning is what refuses.

So if provisioning fails on SSH and `doctor` is green, compare the
stored path's contents against the key the node actually has: `head -c
20 <path>` should start with `ssh-ed25519` / `ssh-rsa` / `ecdsa-`, not
`-----BEGIN`.

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

The step-5 public path has several moving parts; work outward:

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

### Node shows `NotReady` after bootstrap {#node-not-ready}

A freshly provisioned node stays `NotReady` until the CNI is up.
If `kubectl get nodes` doesn't reach `Ready` within a couple of
minutes of `up` completing, the Cilium agent is usually the
culprit — `kubectl -n kube-system get pods -l k8s-app=cilium` and,
if it's crash-looping, check its logs. This is distinct from the
[`k3s-ready` step](#the-k3s-ready-step-of-bootstrap-all-takes-longer-than-expected),
which is the node coming up at all, before the CNI install.

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
- [`docs/reference/cli/`](../reference/cli/index.md) — every
  subcommand + flag.
- [ADR 0030](../adr/0030-cli-target-store-and-credential-chain.md)
  — design rationale for the diagnostic-code scheme.
