# ADR 0063: Manifest discovery is convention plus content, and a bundle may span directories

## Status

`Accepted` (2026-09-14).

Amends [ADR 0029](0029-cue-cmp.md), whose `discover` block no longer matches
what ships. ADR for subphase 2.27 (`plan.md` §2.27), alongside
[ADR 0062](0062-manifest-package-is-a-bundle.md).

Every layout claim in this record was **measured**, not predicted: the
discover snippet was extracted from `argocd-cue-cmp/plugin.yaml` with the
same `awk` program the regression test uses
(`argocd-cue-cmp/test-discover.sh:37-58`), run against fixture trees under
both host `bash` and **busybox ash 1.36.1 (`alpine:3.20` under podman)** —
the sidecar's actual shell — and the render column was produced by the real
sidecar image built from `argocd-cue-cmp/Dockerfile`.

## Context

ADR 0029 specified discovery as `glob: "**/apprafter*.cue"`. That is not
what ships. `argocd-cue-cmp/plugin.yaml:58-68` is a shell snippet with two
branches: if the current directory's basename is `apprafter`, match any
`.cue` at depth 1; otherwise match any `.cue` under a `*/apprafter/*` path or
named `apprafter*.cue`. The snippet is judged by its **stdout**, not its exit
code — walk-fix #10 exists because a `grep -q` in the pipeline exited 0 while
printing nothing, and Argo CD silently fell back to directory mode.

Three things about this are wrong, and all three are reachable today.

**Layouts that should work do not, and fail destructively.** Measured:

| layout | `spec.source.path` | outcome today |
|---|---|---|
| `apprafter/{api,web}/App.cue` | `apprafter` | discover misses → Argo falls back to directory mode → renders nothing → **`prune: true` removes the workloads** |
| `apprafter/{api,web}/App.cue` | `.` | `cue export ./... -e api` evaluates against both package instances and fails `reference "api" not found` → **red sync** |
| repo-root `apprafter/` **and** `services/web/apprafter/` | `.` | the single `cd` at `entrypoint.sh:84-88` is greedy — renders 1 document of 2, `rc=0`, **no warning** |
| two unwrapped (Style A) package directories | `.` | `is_top_level_manifest` evaluates to the two-line string `"yes\nyes"` which is not `"yes"`, so both fall into the Style-B branch; `jq … select(has("apiVersion"))` selects nothing because `apiVersion` is a string, `keys` is empty, and `entrypoint.sh:470-477` exits 0 with **empty stdout** — pruning the whole bundle |

"No match" is not a safe default once an Application exists: Argo CD's
directory mode reads only `.yaml`/`.yml`/`.json`, so it renders an empty
manifest set into a registration carrying `prune: true`.

**The filename convention is not a content check.** A directory that merely
happens to be called `apprafter` matches (measured: a fixture
`apprafter/settings.cue` containing `owner: "team-apprafter"` is claimed),
and after one render the sidecar's own artefacts self-match forever —
`inject_schema_and_claim` writes `cue.mod/pkg/apprafter.io/schemas/v1alpha1/*.cue`
and `apprafter_claim_gen.cue` into the checkout (`entrypoint.sh:200-243`),
and both carry the marker the naive content check would look for.

**The entrypoint mutates the checkout in a way that poisons later renders.**
A nested `cue.mod/` is a CUE module boundary, so once the sidecar has
rendered a subdirectory, a subsequent render from a parent gets `"./..."
matched no packages`. Argo CD's repo-server reuses a per-repo checkout across
Applications, so this is reachable whenever two Applications share a
repository.

Finally, ADR 0029's monorepo story — `argocd.argoproj.io/manifest-generate-paths`
— **never shipped**. The annotation appears nowhere in the tree.

## Decision

### 1. The gate is convention AND content, always

> A repository is ours iff the **repo-relative** path to a `.cue` file
> carries an `apprafter` **directory** component, or the file's name starts
> with `apprafter` — **and** that file contains an AppRafter manifest
> marker, matched as `apprafter\.io/(schemas/)?v1alpha1`.

The convention half restores ADR 0029's original intent. The content half is
new and is what makes the convention safe to widen. A bare `apprafter.io`
substring is deliberately insufficient; the anchored form covers both
manifest shapes — Style A's `apiVersion: "apprafter.io/v1alpha1"` and Style
B's `import v1alpha1 "apprafter.io/schemas/v1alpha1"` — and rejects a `.cue`
that merely mentions the domain in a string.

The sidecar's own artefacts are excluded by path (`cue.mod/`) and by name
(`apprafter_claim_gen.cue`), so a rendered checkout cannot self-match.

**A registered path that merely contains a `.cue` file is not enough.**
`apps/api/App.cue` registered at `--path apps/api` does **not** match. The
convention gate applies at every depth, including the registered directory
itself.

### 2. The signal is `ARGOCD_APP_SOURCE_PATH`, never the absolute `$PWD`

The part of the path at or above the working directory is the registered
`spec.source.path`, which `argocd-repo-server` hands the discover command as
`ARGOCD_APP_SOURCE_PATH` (verified in Argo CD v2.13.1 — the version this
chart pins via `platform-stack/cue/loader_values.cue:77` → `argo-cd 7.7.7` —
at `cmpserver/plugin/plugin.go:353`, fed by `newEnv`
`reposerver/repository/repository.go:1513-1527` and by `getPluginEnvs`
`:1924-1959` on the explicit-plugin path that fires whenever our CLI passed
`--env`).

We will **not** glob the absolute `$PWD`. The CMP working directory is
`${ARGOCD_CMP_WORKDIR:-os.TempDir()}/_cmp_server/<uuidv4>/<source.path>`, and
its base is environment-controlled by **two** variables — `ARGOCD_CMP_WORKDIR`
and, because Argo calls `os.TempDir()`, also `TMPDIR`. Measured: with a base
containing an `apprafter` component, a `$PWD` glob claims `apps/api/App.cue`,
which the rule above must reject.

Scope, stated precisely so the comment in the code does not overstate it: a
contaminated base flips only the **convention** half, and only for
repositories that already carry the marker outside the convention — the
content grep is the first-order gate and is unaffected. In the shipped chart
the vector is unreachable (`component_argocd.cue:417-484` gives the sidecar
no `env:` and mounts a `cmp-tmp` emptyDir at `/tmp`). The live vector is
off-cluster: a checkout is conventionally named after its repository.

Absence of the variable is unambiguous rather than lucky: `environ()`
(`plugin.go:155-163`) drops empty values, so unset ⟺ `source.path` empty ⟺
the working directory **is** the repository root ⟺ there is nothing above it
to match. The rule therefore fails closed.

Two hardenings ride with it. A `..` component in the variable is rejected
first — `argopath.Path` cleans `apprafter/../apps/api` via `filepath.Join`
and accepts it, so the raw `..` survives only in the environment variable.
And `${PWD##*/}` — exactly one path component, which can never be
contaminated by the base — is kept as the direct-invocation arm for
off-cluster runs.

### 3. A bundle may span directories; ambiguity is loud

The entrypoint's single greedy `cd` is replaced by an enumeration that
mirrors the discover predicate and renders each matched package directory in
turn. This is what makes "split into separate directories" — the remedy
[ADR 0062](0062-manifest-package-is-a-bundle.md) prints in its refusals — an
actually supported layout.

**When manifests are found in more than one directory under one registered
path, the render fails with a legible message and a non-zero exit.** It does
not render the first and discard the rest. The two states this replaces are a
silent empty render and a cryptic `reference "api" not found`; neither tells
the reader what happened. Per [ADR 0062](0062-manifest-package-is-a-bundle.md)
one registration is one package, so more than one package under one path is a
registration mistake, and the CMP is the only layer that can see it.

Rendering each directory in its own working directory also retires the
module-boundary poisoning entirely: `cue export ./...` is never again invoked
from a parent.

### 4. `spec.source.path` pointing at a file is not supported

Argo CD rejects it. `util/app/path/path.go` at v2.13.1 returns
`"%s: app path is not a directory"`, called on the plugin manifest path at
`reposerver/repository/repository.go:444` and `:620`. The failure lands at
manifest-generation time, so the apiserver **accepts** the Application and it
then sits in `ComparisonError` permanently.

Today `normalise_argocd_source_path` (`cli/platform-cli/src/commands/app.rs:3728-3738`)
only trims whitespace and strips leading slashes, and `--path`
(`cli/platform-cli/src/cli.rs:816-824`) has no directory check — so
`apprafter app add --path apps/api/App.cue` succeeds locally and creates
exactly that permanently broken object. **The CLI will reject it**, naming
the Argo CD constraint.

### 5. Intra-bundle consistency is enforced here, not at the webhook

The whole package is already in one JSON document at
`entrypoint.sh:395-402`, so every cross-workload check is a `jq` program over
data in hand — no extra `cue` invocation, no cluster access. `apprafter app
validate` runs the same checks locally.

| inconsistency | enforced at | why not elsewhere |
|---|---|---|
| workloads declare different `metadata.namespace` | cue-cmp `exit 1` + `validate` | cross-object; the webhook cannot see a sibling |
| duplicate `(namespace, name)` | cue-cmp `exit 1` + `validate` | below the render layer the two documents are **one** apiserver identity — the webhook sees CREATE-then-UPDATE and cannot tell it from an edit |
| mixed Style A and Style B in one package | cue-cmp `exit 1` + `validate` | the **only** possible layer: the discarded document never becomes an API object |
| workloads declare different `spec.environment` | cue-cmp `exit 1`, **before** `inject_env` | after `entrypoint.sh:140` sets `.spec.environment = $e` the divergence is erased |
| two workloads claim one `expose.hostname` | operator soft condition; cue-cmp warns, exit 0 | hostname uniqueness is a **cluster** property — two bundles from two repositories collide identically, and only the operator sees that |
| a workload references a `claim.<type>` it did not declare | admission webhook — **already implemented** (`operator/admission-webhook/src/validator.rs:2171-2179`) | a pure per-object rule, the one cross-check the webhook can make |

The webhook is not given a cluster client. It receives one object per
`AdmissionReview` — a fixed `admission.k8s.io/v1` contract — and Argo CD
applies each rendered manifest as its own call, so the workloads of a bundle
arrive as unrelated reviews in nondeterministic order. Even with a client the
result would be wrong: the first workload sees no siblings and is admitted
while the rest are rejected, so a bundle lands half-applied with the error
attributed to innocent members, and a legitimate whole-bundle namespace move
deadlocks with every member rejecting every other. The availability half is
worse: `failurePolicy: Fail` with a 10s timeout and no `namespaceSelector`
means a slow apiserver rejects every `apprafter.io` write cluster-wide,
including the `platformstacks` writes needed to recover. This repository has
already paid for the weaker version of that mistake — [ADR 0048](0048-argo-platform-upgrade-approval-surface.md)'s
decorative ConfigMap GET froze reconcile and deadlocked GitOps. The
webhook's own source states the doctrine three times, e.g. `validator.rs:1199-1206`:
*"this function does not, and must not, reach for a Kubernetes client."*

Making the cue-cmp errors visible where the reader already is costs one CLI
change: `status_detail_lines` (`app.rs:3824-3869`) does not read the Argo CD
Application's `status.conditions[]`, so a `ComparisonError` renders as
`sync state: Unknown` and nothing else.

## Consequences

**Easier.** A monorepo can put each bundle in its own directory and register
each, which is what [ADR 0062](0062-manifest-package-is-a-bundle.md) tells
users to do. Four layouts that silently pruned or reddened now work. A
rendered checkout stops self-matching. Intra-bundle mistakes surface as one
legible line on the Argo CD tile instead of as a missing workload.

**Harder.** The rule now depends on a repo-relative path supplied by Argo CD
rather than on the working directory alone. Off-cluster invocation loses
exactly one case — an ancestor-only convention such as `--path apprafter/api`
— which cannot occur in-cluster because the variable is always present there.

**Neutral, and a decision rather than an accident:** "an `apprafter`
directory anywhere in the repo-relative path" means a Helm or Kustomize
repository that merely ships an AppRafter example under `examples/apprafter/`
is claimed by the plugin **when registered at the repository root**.
Registering at any non-root path is unaffected. `examples/` cannot be
special-cased without inventing a second convention. Accepted.

**A monorepo re-renders every registered bundle on every push**, because
`manifest-generate-paths` was never wired. Multi-directory rendering does not
cause this, but it raises the cost. Recorded here as the open item it is.

## Alternatives considered

**Widen the glob only** (the literal `**/apprafter*.cue` / `**/apprafter/*.cue`).
Rejected: it makes things worse on its own. The layouts that currently go
unclaimed would become claimed and then fail — `services/{api,web}/apprafter/`
turns "not claimed" into "red sync", and two Style-A directories turn "not
claimed" into "pruned". The glob and the entrypoint must change in one commit
or not at all. It also cannot see the content, so a directory named
`apprafter` that holds no manifest still matches.

**Pure content matching** (any `.cue` mentioning `apprafter.io`). Rejected in
the other direction: measured false positives on a `.cue` containing
`vendor: "apprafter.io"`, and a Helm repository keeping a manifest anywhere
under its root would have the root claimed.

**Keep `basename "$PWD"` and add an upward walk.** Rejected: the walk has no
bound, because the entrypoint does not know where the checkout root is, and
any bound that reaches past the registered path re-opens the contamination
vector.

**An explicit escape hatch for a single-file path.** The only shape Argo CD
permits is a plugin environment variable
(`spec.source.plugin.env` → `ARGOCD_ENV_APPRAFTER_MANIFEST`), which is
self-consistent because declaring plugin env is exactly what makes
`ExplicitType()` return `Plugin`. Not shipped: it is more surface than the
convention buys, and nobody has needed it.

## Risks

**The `plugin.yaml` mirror has no automated guard.**
`platform-stack/cue/component_argocd.cue:546-551` holds a copy of the snippet
in a ConfigMap that is mounted **over** the image's baked copy, so the mirror
is what actually runs. Nothing in `scripts/` or `.github/workflows/` compares
the two; the only protection is prose. Editing `argocd-cue-cmp/plugin.yaml`
alone passes `test-discover.sh`, passes both drift guards after the version
bumps, publishes an image — and ships the old snippet to every cluster.
Mitigation: a `scripts/check-cue-cmp-mirror.sh` wired into `just lint`, in
this same change. Note the CUE `"""` block requires every backslash doubled,
including `\;`, which is otherwise an `unknown escape sequence`.

**The chart publish does not wait for the sidecar image.**
`.github/workflows/platform-stack-publish.yml:207-245` waits for
`apprafter-operator` and `apprafter-admission-webhook` but not for
`argocd-cue-cmp`, even though the chart pins its tag. If the chart wins the
race, clusters get a repo-server sidecar in `ImagePullBackOff`, which
presents as "Argo CD stopped syncing" rather than as a bad release.
Mitigation: add it to the wait loop in this change — this is the first
cue-cmp bump where correctness depends on it.

**Existing `test-discover.sh` fixtures are not manifests.** Fixtures 1–3
(`test-discover.sh:126-147`) are `package app` plus a `metadata.name` with no
`apiVersion` and no import. Under the content gate they correctly stop
matching, which will read as a regression. Mitigation: rewrite them as real
manifests in the same commit, and add the contaminated-workdir case as the
mutation test for the whole change — reverting to a `$PWD` glob must flip it
from `nomatch` to `match`.

**Accepted:** the ambiguity hard-fail turns a repository that today renders
one workload of two into a red sync. That is the intent — a silent partial
render is the worse state — but it is delivered by chart sync, to every
cluster, with no opt-in.

## Owner

Andrey Ryahovskiy.

## Re-evaluation

Revisit if Argo CD changes how the CMP working directory or the plugin
environment is constructed (the contract read here is v2.13.1), or if the
`examples/apprafter/` false positive is reported by a real user rather than
predicted.

## References

- `plan.md` §2.27.
- [ADR 0029](0029-cue-cmp.md) (amended: the `discover` block and the
  `manifest-generate-paths` monorepo story),
  [ADR 0062](0062-manifest-package-is-a-bundle.md),
  [ADR 0046](0046-env-value-references.md) (the `claim` binding this
  enumeration must keep generating per directory),
  [ADR 0048](0048-argo-platform-upgrade-approval-surface.md).
- `argocd-cue-cmp/{plugin.yaml,entrypoint.sh,test-discover.sh,version.cue}`,
  `platform-stack/cue/component_argocd.cue`,
  `cli/platform-cli/src/commands/app.rs` (`normalise_argocd_source_path`),
  `operator/admission-webhook/src/validator.rs`.
- Argo CD v2.13.1: `cmpserver/plugin/plugin.go`,
  `reposerver/repository/repository.go`, `util/app/path/path.go`,
  `util/io/files/util.go`, `common/common.go`.
