# Day-2 follow-ups — product defects the 2.19j walk documented rather than fixed

Found while correcting the day-2 operations pages (2.19j). Each one is a
**product change**, not a documentation change: the pages now describe
the behaviour as it actually is, and these entries record what would
have to move for the description to go back to the intended one.

Internal working data — this tree is excluded from the site
(`exclude_docs` in `mkdocs.yml`) and from the documentation gate
(`docs/measurements/` is in `EXCLUDED`, `cli/docsgen/src/scan.rs:96`).

## VPA in-place right-sizing has never run: wrong feature-gate name

**Opened:** 2026-08-20 (2.19j, correcting
`docs/dev-guide/resources-and-autoscaling.md`).
**Status:** open — deliberately NOT fixed here; the fix changes live
cluster behaviour and is the cluster owner's call.
**Severity:** the whole applying half of ADR 0054 is inert, silently.

### What is wrong

`platform-stack/cue/component_vpa.cue` passes

```text
--feature-gates=InPlaceOrRecreate=true
```

to the VPA **admission controller** (line 58) and the **updater**
(line 87). The chart is pinned at `vertical-pod-autoscaler` 0.11.0,
whose `appVersion` is `1.7.1` (confirmed with `helm show chart`), and
that version does not have a gate by that name. Reproduced locally
against the released images, no cluster involved:

```console
$ podman run --rm registry.k8s.io/autoscaling/vpa-updater:1.7.1 \
    --feature-gates=InPlaceOrRecreate=true
invalid argument "InPlaceOrRecreate=true" for "--feature-gates" flag: unrecognized feature gate: InPlaceOrRecreate

$ podman run --rm registry.k8s.io/autoscaling/vpa-admission-controller:1.7.1 \
    --feature-gates=InPlaceOrRecreate=true
invalid argument "InPlaceOrRecreate=true" for "--feature-gates" flag: unrecognized feature gate: InPlaceOrRecreate
```

The gate 1.7.1 *does* recognise is **`InPlace`** — from the same
binary's own `--help`:

```text
--feature-gates mapStringBool   A set of key=value pairs … Options are:
                                AllAlpha=true|false (ALPHA - default=false)
                                AllBeta=true|false (BETA - default=false)
                                CPUStartupBoost=true|false (ALPHA - default=false)
                                InPlace=true|false (ALPHA - default=false)
```

So both Deployments crash-loop on startup, and have since the component
shipped — the chart version is pinned, so there was never a window in
which it worked. Confirmed on the live cluster by the 2.19j walk: both
pods `CrashLoopBackOff`, nine days.

### Why it is silent

- The **recommender** carries no such flag, so it runs: recommendations
  are produced and mirrored onto `Application.status.recommendedResources`.
  Every read path (`apprafter app status`'s `VPA reco:` line, the VPA
  object's `status.recommendation`) keeps working.
- The mutating webhook is registered `failurePolicy: Ignore`
  (deliberately, and correctly — it stops a down admission pod from
  deadlocking pod creation), so nothing fails when it is absent.
- `Application.status.recommendedResources.notApplied` is set **only**
  for node-capacity infeasibility
  (`operator-controllers/application/src/lib.rs:1372`), never for "no
  updater". So the status reads exactly as it would if everything were
  working.
- The installed-check the docs previously offered —
  `kubectl get crd verticalpodautoscalers.autoscaling.k8s.io` — passes
  in this state. It answers "was the component rendered", not "is the
  autoscaler working".

Net effect: recommendations are computed and reported; **no pod request
is ever changed**. Applications keep the rendered `32Mi` forever.

### The decision needed

Renaming the gate to `InPlace` is a one-word change — and it flips live
behaviour from "nothing happens" to "the updater starts resizing running
pods". On a cluster that has been running for months at `32Mi` with
recommendations accumulating, that is a fleet-wide resize on the first
reconcile after the upgrade, not a no-op. It needs an owner's decision
and a maintenance window, which is why 2.19j documented it instead.

Options, in the order they should be considered:

1. **Rename to `InPlace` and ship it**, with the resize wave called out
   in the platform-stack changelog entry and the compatibility
   classification set accordingly (this is at least `requires-restart`
   for anything the updater cannot resize in place).
2. **Drop the gate and pin `updateMode: Off`-equivalent behaviour**
   explicitly, i.e. make "observe and report only" the documented,
   intended posture rather than an accident. Then
   `apprafter platform autoscale set` needs to say what it really does.
3. **Leave as-is.** Only acceptable while this file exists and the
   dev-guide page keeps saying so.

Whichever is chosen: **re-read the gate name on every chart bump.** The
component's own header comment already says so and it still drifted —
the name changed upstream and nothing here noticed, because a
crash-looping controller in a namespace nobody watches produces no
signal that reaches an operator.

### Guard worth adding

Nothing in `just lint`, `just crd-validate` or the e2e asserts that the
VPA controllers reach `Running`. A component whose pods never start is
invisible to every gate we have. The cheapest guard is an e2e assertion
that every Deployment in the `vpa` namespace reports
`availableReplicas >= 1` after the platform converges.

## `apprafter backup enable --cron` / `--check-cron` accept a value the apiserver rejects

**Opened:** 2026-08-20 (2.19j, correcting
`docs/operator-guide/backup-restore.md`).
**Status:** open — small, self-contained CLI fix.

### What is wrong

Both flags are `Option<String>` with no validation
(`cli/platform-cli/src/cli.rs`, `check_cron` / `cron`). The value is
threaded into `spec.backup.schedule` / `spec.backup.checkSchedule` —
declared `string` in `schemas/v1alpha1/platformstack.cue:87` with no
pattern — and rendered verbatim into the CronJob's `schedule:` field
(`platform-stack/cue/render_tool.cue:538`).

A non-cron value therefore travels all the way to the apiserver before
anything objects, and when it does object it takes the platform-stack
sync down with it. Verified against a real apiserver (kind):

```console
$ kubectl apply -f apprafter-backup-check.yaml     # schedule: "off"
The CronJob "apprafter-backup-check" is invalid: spec.schedule: Invalid value: "off": expected exactly 5 fields, found 1: [off]
```

This is not hypothetical wording: `--check-cron off` was offered three
times in `backup-restore.md` as the way to disable the in-cluster check.
It is not implemented anywhere — no `"off"` literal exists in
`commands/backup.rs` — so every operator who followed that instruction
broke their platform sync. The page now documents a never-firing
schedule (`0 6 31 2 *`, verified accepted) instead.

### The fix

A clap `value_parser` on both flags that rejects anything that is not
five whitespace-separated fields, alongside the existing client-side
enum checks for `--enforce` and `--staging-mode`
(`commands/backup.rs:1732-1745`) — cron is the odd one out among the
validated options. Rejecting at the CLI keeps a typo from reaching the
CR at all.

Optionally, and separately: decide whether "disable the in-cluster
check" deserves a first-class spelling. If it does, it is a
`checkSchedule: ""`-means-omit-the-CronJob branch in the chart plus a
`--check-cron off` that maps onto it — **not** a string that reaches
`schedule:`.

## Two diagnostics whose `help:` text describes a layout that moved

**Opened:** 2026-08-20 (2.19j, correcting
`docs/operator-guide/troubleshooting.md`).
**Status:** open — text-only, but it is the text an operator reads at
the exact moment they are stuck.

Both live in `cli/cli-core/src/error.rs`. Left alone here because
changing a shipped diagnostic's help is a CLI release, and because the
troubleshooting page now states the correct answer beside the stale one.

1. **`apprafter::state::corrupt`** (error.rs:152-155) says "The local
   `.apprafter/state.json` file … delete `.apprafter/`". State moved to
   `<config-root>/state/<target>/.apprafter/state.json` in **v0.1.154**
   (`cli/cli-state/src/state.rs:10`, `commands/state_paths.rs`); the
   per-cwd file is a legacy artefact that is migrated once. An operator
   following the help deletes a directory that is not the problem, and
   the error keeps firing. The summary line does print the real path, so
   the fix is to make the help point at it rather than at a fixed
   location.

2. **`apprafter::provider::server_type_not_selected`** (error.rs:142)
   says "set `nodes[0].kind` in your Infrastructure manifest". There is
   no `kind` field on a node: the manifest schema declares
   `spec.nodes[*].type` (`schemas/v1alpha1/infrastructure.cue:29`) and
   `kind` is only the Rust field name, renamed because `type` is a
   keyword (`cli/cli-core/src/manifest.rs:106-113`). Every other surface
   gets this right — `apprafter restore --server-type` and
   `bootstrap-all --server-type` both say `spec.nodes[0].type` — so this
   one string is the outlier, and it names a field that a manifest
   author cannot set.
