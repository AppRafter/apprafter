# AppRafter platform-stack

> Curated bundle of Cilium, cert-manager, Argo CD,
> apprafter-operator, admission-webhook, and (optionally)
> Backstage. Rendered as Argo CD `Application` resources from
> CUE source. The single source of truth for "what makes up
> the AppRafter platform layer".
>
> **Status:** scaffold landed in `v0.1.92` (plan.md sub-phase
> 1.66). Renderer + publishing pipeline land in 1.67–1.68.

## Why this exists

ADR 0026 establishes that the platform stack is fetched as a
Helm chart from an OCI registry. ADR 0028 establishes that
the *source* of that chart is CUE in this repository, not
hand-edited YAML and not a Git submodule. This directory is
the on-disk expression of that decision.

Quick mental model:

- `cue/` — every artifact under version control. CUE only.
- `dist/` — gitignored. Output of `cue cmd render`, consumed
  by `helm package` and `oras push`.
- `Chart.yaml.tmpl` — the only non-CUE source file. Rendered
  at chart-build time with the current version substituted.

You never edit `dist/`. Edit `cue/`. Run `make render` (lands
in 1.67) to regenerate `dist/`.

## Layout

```
platform-stack/
├── cue/
│   ├── platform.cue                          # umbrella schema (types, #Version, #Component, …)
│   ├── component_cilium.cue                  # CNI + kube-proxy replacement
│   ├── component_cert-manager.cue            # TLS controllers + ClusterIssuer
│   ├── component_argocd.cue                  # Argo CD self-managing
│   ├── component_apprafter-operator.cue      # Application reconciler
│   ├── component_admission-webhook.cue       # Application validation
│   ├── component_backstage.cue               # Backstage portal (conditional)
│   ├── component_network-policies.cue        # default-deny bundle
│   ├── component_argocd-cue-cmp.cue          # ADR 0029 CMP sidecar
│   ├── tier_solo.cue                         # tier 1 — single cpx22, no Hubble, no Backstage
│   ├── tier_team.cue                         # tier 2 — multi-node, Hubble on, Backstage on
│   └── compatibility.cue                     # change classification per version
├── Chart.yaml.tmpl                           # rendered to dist/<version>/Chart.yaml
├── README.md                                 # you are here
└── CHANGELOG.md                              # operator-facing release notes
```

The flat layout is deliberate: every `.cue` file lives directly
under `cue/` so they share the same CUE package instance
(`platformstack`). CUE treats subdirectories as separate
package instances even when the `package` declaration matches,
which prevented `_components` from being populated across
files. Filename prefixes (`component_`, `tier_`) keep the
mental grouping that subdirectories would have given.

## Contribution model

1. **Adding a component.** Drop a new
   `cue/component_<name>.cue`, declare a `_components.<name>:
   #Component & {...}` value. Set defaults. The umbrella
   chart's `templates/applications.yaml` (lands in 1.67)
   iterates over `components` automatically — no manifest
   plumbing needed beyond the CUE declaration.

2. **Tightening a default.** Edit the relevant
   `cue/component_<name>.cue`. Tier overlays (`cue/tier_<name>.cue`)
   may then override the new default per tier. Run `cue eval
   ./platform-stack/cue/...` locally to confirm the
   unification still succeeds.

3. **Major version bump (component MAJOR or API shape).** Add
   a new entry in `cue/compatibility.cue` describing the
   change and its operator-facing impact. CI gates publish on
   the entry being present.

4. **Adding a tier.** New `cue/tier_<name>.cue` with a
   `tierN: #PlatformValues & { tier: N, ... }` value. The
   renderer picks up new tier names from the values
   `tier: N` key automatically.

The CLI never writes here directly. `apprafter cluster-bootstrap`
(today) and the upcoming `PlatformController` (Phase 2+)
consume the *published* OCI artifact, not the source CUE.
Local development overrides during chart-rendering go via
`make render TIER=team` (lands in 1.67) without touching
this tree.

## Distribution (ADR 0028)

- **Primary:** `oci://ghcr.io/apprafter/platform-stack:<version>`.
  Argo CD pulls from here on every PlatformStack reconcile.
- **Secondary:** GitHub Release `.tgz` attachment. Lets users
  use Helm directly without involving AppRafter components.
- **No** rendered manifests in this repository.

Tags shaped `platform-stack/v<version>` trigger the publish
workflow (lands in 1.68). Maintainer release procedure: see
`RELEASE.md` (lands alongside 1.68).

## Local development

```sh
# Validate CUE source (already wired into the project lint).
just lint                     # under the hood: cue vet ./platform-stack/cue/...

# Render to dist/ (lands in 1.67):
make render                   # → dist/platform-stack-<version>/

# Local sanity install (lands in 1.67):
helm template dist/platform-stack-<version>
helm lint dist/platform-stack-<version>
```

## Forking the stack

Power users who maintain a private fork run:

```sh
apprafter platform fork --to ghcr.io/myorg
```

(Command lands in plan.md sub-phase 1.74.) The fork retains
`spec.source.upstream` pointing at our public registry by
default; the user's controller still sees
`status.availableVersion` updates from upstream and can
independently sync when ready.

## Related design docs

- [ADR 0028](../docs/adr/0028-platform-stack-distribution.md)
  — CUE source + dual-channel publishing.
- [ADR 0026](../docs/adr/0026-platformstack-crd.md) —
  PlatformStack CRD + version pinning.
- [ADR 0029](../docs/adr/0029-cue-cmp.md) — Argo CD CMP for
  user CUE repositories.
- [plan.md](../plan.md) sub-phase 1.66 (this scaffold), 1.67
  (renderer + Helm chart shape), 1.68 (publish workflow),
  1.69 (CUE CMP wiring), 1.70 (chart cluster-bootstrap
  rewrite), 1.71 (in-tree manifests migration), 1.74 (`apprafter
  platform fork`).
