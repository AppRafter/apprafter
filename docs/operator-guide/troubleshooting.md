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

The Hetzner Cloud API refused the request.

**Fix.** Read the inner help. It lists what each status means:

- **401 unauthorized** — the token is wrong, or it was revoked or
  rotated. For a target's stored token,
  `apprafter target add <name> --renew --token <new>` replaces it.
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

- **A target's `config.yaml` or `credentials.yaml`**: delete that
  target's directory (`targets/<name>/`, named in the help) and add
  the target again with
  `apprafter target add <name> --provider hetzner-cloud …`, its token
  included. `apprafter target remove` refuses a target it cannot read.
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

- The Hetzner Cloud Console shows a token only once, when it is
  created: paste it again from where you saved it, or create a new
  one in the project under Security → API tokens (AppRafter needs
  Read & Write).
- If you're rotating, use `apprafter target add <name> --renew
  --token <new>` instead of re-creating the target.

### `apprafter::target::provider_unreachable`

The provider's API could not serve a request, for a reason no
other token fixes. Any request that gets **no answer** — the
connection is refused, the name does not resolve, the request
times out — reports this code, whether it was the credential
check, a machine catalogue read or `apply`. The credential check
also reports it for any other **non-401** failure: a 5xx
provider-side outage or a 429 rate limit. The token may still be
valid once the API recovers — the help text intentionally avoids
any rotation suggestion that would misdirect operators.

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
(`chmod 0600` on credentials), full disk. For decode failures on
target-store files, fix the YAML by hand (they are small files),
or delete that target's directory under the target store and add
it again with `apprafter target add`. `target add --force` cannot
rewrite such a target: it keeps the stored values, so it needs a
readable config.

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
- Try `apprafter kubeconfig --refresh` once the cluster reports
  ready in the Hetzner Cloud Console (the Web Console gives you
  out-of-band access to the boot log).

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
        • The Hetzner Cloud Console shows a token only once, …
        • If you're rotating, run `apprafter target add <name>
        --renew --token <new>` …
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
