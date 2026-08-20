# Schema follow-ups — fields the CUE still declares and no code reads

A standing ledger of `schemas/v1alpha1` fields that outlived the
capability they configured. Each one is a **product decision**, not a
cleanup chore, which is why they are recorded rather than deleted by
whoever trips over them.

They matter beyond tidiness: the documentation gate resolves schema
identifiers against the CUE, so a field the CUE still declares makes
every page naming it pass — including a page describing a capability
that no longer exists. That is not a hypothetical, and the entry below
is the case that proved it.

## `spec.argocd.bootstrapRepo` / `spec.argocd.bootstrapPath`

**Opened:** 2026-08-20 (2.19j, withdrawing
`docs/operator-guide/connect-a-git-repository.md`).
**Status:** open — needs a product decision.

### What is dead

The bootstrap-repository capability was removed in **v0.1.97**, when
`cluster-bootstrap` became the minimal GitOps loader (ADR 0025): Cilium,
Argo CD, and one root `Application` pointing at the platform-stack OCI
chart. Its own module docs list "the bootstrap Application" under *What
deliberately went away vs. v0.1.x*.

Re-verified at `bd6185b`, all three returning zero hits outside `docs/`
and the historical ledgers (`plan.md`, `docs/changelog/`):

- `APPRAFTER_ARGOCD_REPO_TOKEN`
- `APPRAFTER_ARGOCD_REPO_USERNAME`
- `apprafter-bootstrap-repo-creds`

`cluster-bootstrap` reads no manifest at all — `parse_infrastructure` has
exactly one caller, `apply`, and nothing downstream of it reads the
`argocd` block's bootstrap fields.

### What is still declared

- `schemas/v1alpha1/infrastructure.cue` declares `bootstrapRepo?` and
  `bootstrapPath?` under `spec.argocd`.
- `cli/cli-core/src/manifest.rs` deserialises them into
  `ArgocdBlock::bootstrap_repo` / `bootstrap_path`.

The Rust fields have **no live reader**: the only things touching
`bootstrap_repo` are a parse test
(`cli/cli-core/tests/manifest_test.rs`) and the documentation gate's own
field-set harvest. A manifest setting either field parses, validates,
and is silently ignored.

### Why it is a documentation problem and not only dead code

`docs/operator-guide/connect-a-git-repository.md` documented the removed
flow for 298 lines and passed **nine subphases of green gates**. Every
identifier on it resolved, because resolution asks the CUE whether a
name exists and never asks the code whether anything reads it. The gate
sees names, not truth.

So the fields are load-bearing for a false pass: while they stand,
nothing stops the page being written again. The hole is documented at
`FieldSet::from_repo` in `cli/docsgen/src/identifier.rs`, and
`a_cue_only_schema_still_backs_the_docs_that_name_it` in
`cli/docsgen/tests/identifier_test.rs` pins it as a live reproduction.

### The decision needed

Not taken here — removing a schema field is a compatibility change and
belongs with the person who owns the manifest surface.

1. **Remove from the CUE** (and the Rust struct). A manifest still
   setting them then fails `cue vet` instead of being ignored, which is
   the honest outcome but breaks any checked-in `Infrastructure.cue`
   carrying a field that has done nothing since v0.1.97.
2. **Keep and mark deprecated** in the CUE docstring. Cheap, keeps old
   manifests parsing — but leaves the gate's false pass exactly as it
   is, so it needs the docstring to say *removed in v0.1.97*, not
   merely *deprecated*.
3. **Reinstate the capability.** Nothing suggests demand; recorded only
   so the option is not lost by omission.

Whoever takes it: `cli/docsgen/tests/identifier_test.rs` asserts
`spec.argocd.bootstrapRepo` resolves and is the test that fails under
option 1. Swap it for another CUE-only path — the assertion is about
harvesting CUE-only schemas, not about this field.

## `spec.operator.enabled` / `spec.admissionWebhook.enabled`

**Opened:** 2026-08-20 (2.19j, correcting
`docs/operator-guide/quickstart.md`).
**Status:** open — needs a product decision.

### What is dead

`enabled: false` was an opt-out from a direct `helm install` that
`cluster-bootstrap` performed. That install was removed: the operator
and the admission webhook now arrive as components of the platform-stack
chart, reconciled by Argo CD. `cluster_bootstrap.rs`'s module docs list
"Direct `helm install` of cert-manager, the operator, the
admission-webhook" under *What deliberately went away vs. v0.1.x*, and
`cluster-bootstrap` reads no `Infrastructure.cue` at all — so nothing
can consult the flag even in principle.

### What is still declared

- `schemas/v1alpha1/infrastructure.cue` declares `operator?` and
  `admissionWebhook?`, each `{enabled?, image?, tag?}`. Both docstrings
  still describe the removed behaviour ("`cluster-bootstrap` installs
  the operator in `apprafter-system` by default").
- `cli/cli-core/src/manifest.rs` deserialises both blocks.

The only consumer of either block is
`cli-providers::k8s::image_ref::resolve_image_ref`, which has **no
production caller** — a fact `CLAUDE.md` already records for the
sibling `RELEASED_OPERATOR_VERSION` constant. So all three fields
(`enabled`, `image`, `tag`) parse and are ignored.

### Why it is the same class as the entry above

Setting `operator.enabled: false` is *worse* than a no-op that errors:
an operator reads the documented field, sets it, sees `cue vet` pass,
and gets the operator installed anyway. The page documenting it passed
every gate for the same reason the bootstrap-repository page did — the
identifiers resolve against the CUE.

### The decision needed

Same three options as above, with one difference: `image` / `tag` were
the fork/dev-build override knobs. If the intent is to keep a
fork-build escape hatch, it now belongs on the **chart** side
(`component_apprafter-operator.cue`), not on `Infrastructure.cue` —
so option 2 should say *moved*, not merely *deprecated*.
