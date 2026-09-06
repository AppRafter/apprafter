---
description: "How a pushed .cue file becomes running Kubernetes objects: what Argo CD detects, what the CUE plugin sidecar does to your checkout, and why each manifest is exported on its own."
schema-check-ignore:
  - path: "spec.source.path"
    reason: external-tool
    since: v0.2.61
    note: Argo CD's Application CR, whose field set AppRafter does not model
---

# GitOps and the CUE plugin

What happens between `git push` and a running Deployment. The recipe is
[Writing Application.cue](../dev-guide/application-cue.md); none of this is
needed to write one.

Read it when a manifest you expected to be compiled was not, when a `claim`
reference resolves to something you did not declare, or when a compile error
mentions a file you did not write.

## The chain

Argo CD clones your repository and runs a **discovery probe** on it. If the
probe matches, Argo CD hands the checkout to the CUE
Config-Management-Plugin — a sidecar named `cue-cmp` inside the
`argocd-repo-server` pod — which compiles your CUE to Kubernetes YAML and
returns it to Argo CD's sync pipeline. Argo CD applies the result; the
AppRafter operator then reconciles the `Application` CR it finds into a
Deployment and a Service.

There is no rendered-output branch and no local pre-commit step. The design
rationale is [ADR 0029](../adr/0029-cue-cmp.md).

## What makes a repository "CUE"

Place your manifest at `apprafter/Application.cue` in the root
of your repository (or the repository path you registered with
`apprafter app add`). The CMP decides whether a repository is CUE by
running a shell probe from `spec.source.path` and checking whether it
printed anything:

```yaml
discover:
  find:
    command:
      - sh
      - -c
      - |
        if [ "$(basename "$PWD")" = "apprafter" ]; then
          find . -maxdepth 1 -type f -name '*.cue' -print -quit
        else
          find . -type f -name '*.cue' \( -path '*/apprafter/*' -o -name 'apprafter*.cue' \) -print -quit
        fi
```

So a file is picked up when **either** it sits anywhere under an
`apprafter/` directory — whatever it is called, which is why
`apprafter/Application.cue` works — **or** its own filename starts
with `apprafter`. The special case at the top handles a
`spec.source.path` that already points *at* the `apprafter/`
directory: there, any `.cue` file directly inside it matches.

The recommended layout is `apprafter/Application.cue`. Bear in mind
that every `.cue` file under `apprafter/` is compiled, not just the
one named `Application.cue`, and that a stray `apprafter-notes.cue`
elsewhere in the repo also matches the second branch.

The probe runs from `spec.source.path`, and it prints at most one match — Argo
CD only needs to know *whether* this repository is ours, not how many manifests
it holds.

## What the sidecar does to your checkout

## How the CUE CMP works

The CUE plugin runs in a sidecar container named `cue-cmp`, inside
the Argo CD `argocd-repo-server` pod. When Argo CD clones a repository
and the discovery probe above prints a match, the sidecar runs its
`entrypoint.sh`, which does three things worth knowing about:

1. **It changes directory into `apprafter/`** when `spec.source.path`
   pointed at a parent. Everything below runs from the package
   directory.
2. **It writes the schema and the `claim` binding into your checkout**
   — a workspace-local CUE module holding the exact
   `apprafter.io/schemas/v1alpha1` the sidecar image ships, plus a
   generated `apprafter_claim_gen.cue` that defines the `claim` value
   your `env` references resolve against
   ([ADR 0046](../adr/0046-env-value-references.md)). Both are inject-wins: they overwrite anything you vendored. This is why you
   do not vendor the schema yourself, and why bare
   `claim.pg.url` selectors resolve without you declaring them.
3. **It exports each manifest separately.** A single
   `cue export ./... --out yaml` would emit your named top-level values
   (`app: …`, `web: …`) as keys of one YAML document, which Argo CD
   would reject as a manifest with no `apiVersion`. So the sidecar
   exports to JSON, enumerates the top-level values that look like
   Kubernetes objects (`apiVersion` + `kind` present), and re-exports
   each one on its own with `cue export ./... -e <key> --out yaml`,
   separated by `---`. Top-level helper values that are not Kubernetes
   objects are skipped rather than emitted.

The sidecar is installed and kept up to date by the platform-stack
chart; no manual sidecar configuration is required on your part.

The second of those three is the one that surprises people: the schema and the
`claim` binding are written **into your working copy** at compile time, and
they overwrite anything of the same name. That is why a vendored copy of the
schema is not merely unnecessary but actively pointless, and why
`claim.pg.url` resolves in your manifest without you having declared `claim`
anywhere.

## Reading it when it goes wrong

??? note "Reading the same thing from a shell"

    ```sh
    # Argo CD Application sync state.
    kubectl get applications.argoproj.io <app-name> -n argocd \
        -o jsonpath='{.status.conditions}'

    # The CMP sidecar's log, for the full cue output. The container is
    # named `cue-cmp` — `argocd-cue-cmp` is the image, not the container.
    kubectl logs -n argocd deploy/argocd-repo-server -c cue-cmp --tail=50
    ```

The container is `cue-cmp`; `argocd-cue-cmp` is the image. Getting that pair
the wrong way round is the usual reason `kubectl logs -c` reports no such
container.

## See also

- [Writing Application.cue](../dev-guide/application-cue.md) — the manifest
  this compiles, and what to do about a compile error.
- [Image iteration](../dev-guide/image-iteration.md) — what happens after the
  objects exist and the tag they name moves.
