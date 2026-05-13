# ADR 0028: Platform-stack distribution — CUE source, dual-channel publishing

## Status

Draft.

## Context

ADR 0026 establishes that the platform stack is fetched as a Helm chart from an OCI registry. ADR 0025 establishes that the user does not host these manifests themselves. This leaves the questions: where do the platform manifests live in source form, how are they distributed, and how does a power user fork the stack for customization?

Two earlier proposals were rejected during design discussion:

1. **User-side platform Git repo (submodule or fork at init time).** Rejected because it pollutes user projects with platform machinery, requires the user to manage Git tokens for our infrastructure repository, and makes upstream updates awkward for private user forks.
2. **Committed rendered YAML alongside CUE source in our repo.** Rejected because it pollutes Git history with CI-generated artifacts and contradicts the "CUE as configuration language" positioning by giving YAML equal prominence.

The correct pattern is: CUE source in our public Git monorepo (transparent, community-contributable), rendered Helm chart published to OCI on tag (standard Kubernetes ecosystem pattern), with a secondary publish to GitHub Release assets as an escape hatch.

## Decision

The platform-stack source lives in the AppRafter monorepo at `apprafter/platform-stack/`, containing only CUE. No `templates/`, no `values.yaml`, no rendered manifests committed to Git.

On Git tags matching `platform-stack/v*`, CI:

1. Runs `cue cmd render` to produce an umbrella Helm chart in `dist/` (gitignored).
2. Validates the chart with `helm lint` and a smoke install in a kind cluster.
3. Signs the chart with `cosign`.
4. Publishes to two channels in parallel:
    - **Primary:** `oras push ghcr.io/apprafter/platform-stack:<version>` — the OCI artifact that Argo CD pulls.
    - **Secondary:** GitHub Release attachment — the rendered `.tgz` for users who want to use Helm directly without involving AppRafter components.

The umbrella chart template iterates over components declared in `values.components`, producing one Argo CD `Application` per component:

```yaml
# templates/applications.yaml — the only template
{{- range $name, $component := .Values.components }}
{{- if $component.enabled }}
apiVersion: argoproj.io/v1alpha1
kind: Application
metadata:
  name: {{ $name }}
  namespace: argocd
spec:
  source:
    repoURL: {{ $component.source.repoURL }}
    chart: {{ $component.source.chart }}
    targetRevision: {{ $component.version }}
    helm:
      valuesObject: {{ toYaml $component.values | nindent 8 }}
  destination:
    server: https://kubernetes.default.svc
    namespace: {{ $component.namespace }}
  syncPolicy:
    automated: { prune: true, selfHeal: true }
{{- end }}{{- end }}
```

PlatformController patches a single Argo CD Application (the umbrella) via PlatformStack reconciliation; Argo CD's rendering produces the N child Application CRs.

A `apprafter platform fork` CLI command provides the bootstrap flow for power users who want to maintain a private fork:

```
$ apprafter platform fork --to ghcr.io/myorg
✓ Forked github.com/AppRafter/apprafter → github.com/myorg/apprafter (private)
✓ Added .github/workflows/platform-stack-publish.yml to the fork
✓ Triggered initial publish: ghcr.io/myorg/platform-stack:0.2.0
✓ Patched PlatformStack/default with new source.repoURL
```

The fork retains `spec.source.upstream` pointing at our public registry by default, so the user's controller still sees `status.availableVersion` updates from upstream. They can independently sync their fork from upstream when ready.

## Rationale

### CUE only in Git

The repository contains exactly the artifacts the user inspects and modifies: CUE source, schema definitions, compatibility metadata. Generated artifacts live in CI output and OCI, not in version control. This keeps the history readable and prevents the confusion of "is the YAML or the CUE the source of truth?"

### OCI as primary distribution

OCI is the standard distribution channel in the Kubernetes ecosystem. Argo CD pulls OCI Helm charts natively. Cosign signing produces verifiable artifacts. Forking is `crane copy` to a different registry — one command.

### GitHub Release as escape hatch

A user who wants to deploy the rendered chart directly with `helm install`, without involving the AppRafter CLI, operator, or any of our control plane, can download the `.tgz` from the GitHub Release and use it as a vanilla Helm chart. They lose PlatformStack management, MigrationPlan gating, and CLI integration, but the underlying components still work. This is an honest fallback for skeptics and a reference implementation for those who want to learn from the chart structure.

For us, this costs nothing: the chart is already rendered for the OCI publish; attaching the tarball is one line of CI configuration.

### Umbrella chart pattern

A single template that iterates over components, rather than one template per component, has several benefits:

- PlatformController patches a single Argo CD Application instead of N. Diff logic is simpler.
- Adding a new component is a CUE addition with no template changes.
- Override mechanism is uniform: all per-component customization flows through `values.components.<name>`.

### Fork via OCI, not Git

Forking the chart distribution is fundamentally an OCI operation (copy an OCI artifact to a different registry namespace). The Git repository is the source of upstream changes; the OCI is the distribution channel. A user who forks both has full autonomy. A user who forks only the OCI (without Git) gets distribution control but no contribution channel.

The dual-source pattern (`spec.source.upstream` vs `spec.source.repoURL`) lets users track upstream availability while pulling from their own registry.

### Compatibility metadata as part of the chart

Every release ships a `compatibility.yaml` (rendered from `compatibility.cue`) classifying changes per component:

```yaml
version: "0.3.0"
minimumKubernetesVersion: "1.30"
changes:
  - component: cilium
    classification: requires-restart
    from: "1.16.5"
    to: "1.17.0"
    notes: "Updated node-init image; pods reschedule on rollout."
  - component: cert-manager
    classification: safe
    from: "1.16.2"
    to: "1.16.3"
```

Classification taxonomy (four levels, simple enough to apply consistently):

| Class | Semantics | Default action under autoUpgrade |
|---|---|---|
| `safe` | Patch versions, value tweaks, no resource recreate | Bumped automatically |
| `requires-restart` | Pod restart, brief disruption, no data movement | Bumped automatically with notification |
| `data-migration` | Schema or storage change, requires migration steps | Gated through MigrationPlan |
| `breaking` | Manual intervention required, documentation link mandatory | Gated through MigrationPlan |

CI validates that every new release includes a `compatibility.cue` update; pull requests without it fail the build.

## Implementation outline

| Step | Description | Size |
|---|---|---|
| 1 | CUE source layout: `apprafter/platform-stack/cue/` with `platform.cue`, `components/*.cue`, `tiers/*.cue`, `compatibility.cue` | M |
| 2 | `cue cmd render` pipeline producing umbrella Helm chart in `dist/` | S |
| 3 | GitHub Actions workflow: render → lint → smoke test → cosign sign → OCI publish + GitHub Release attachment | S |
| 4 | Cosign key generation, secure storage, key rotation procedure documented | XS |
| 5 | `apprafter platform fork` CLI command: GitHub fork API, workflow template, OCI publish setup, CR patch | M |
| 6 | CI validation: `compatibility.cue` must update on each version tag; missing → red | S |
| 7 | Documentation: maintainer release procedure; user fork procedure | XS |

## Consequences

**Positive:**
- CUE positioning preserved end to end.
- Distribution via standard OCI is signed, cacheable, and ecosystem-aligned.
- Fork is one CLI command for users; manual via `crane` for power users.
- Compatibility metadata is enforced by CI rather than hoped for.

**Negative:**
- CUE-to-Helm-chart rendering is custom build infrastructure that must be maintained.
- Cosign keys require management (rotation, backup, revocation procedures).
- The `apprafter platform fork` CLI command depends on GitHub API; it does not work for non-GitHub Git hosts in v1 (GitLab fork support is a Phase 2+ addition).

## Risk

**Main risk:** GitHub Container Registry rate limits on public reads. **Mitigation:** PlatformController caches resolved version lists (ADR 0026) and pulls only on actual version change. Managed-fleet contexts (ADR 0026 re-evaluation triggers) can switch to webhook-driven update notifications if rate limits become a blocker.

**Secondary risk:** Cosign key compromise or loss. **Mitigation:** keys live in GitHub Actions secrets with restricted access; rotation procedure documented; users can configure `cosign verify` against a published list of valid keys in the project's GitHub Releases.

## Owner

Core platform team plus release engineering.

## Re-evaluation triggers

- If the CUE-to-Helm pipeline accumulates fragility (recursive imports not rendering, unexpected differences between local and CI output), evaluate Timoni as a CUE-native package manager replacement. Timoni would eliminate the Helm rendering step but currently requires a CMP for Argo CD support and is not yet upstream-production-grade.
- If GitHub OCI rate limits become a chronic blocker, migrate primary publishing to a self-hosted Harbor registry, keeping GitHub Release as a secondary mirror.

## Still open

- **Compatibility metadata authoring tooling.** A future tool that analyses changelogs of upstream components (Cilium, cert-manager, etc.) and pre-fills the `compatibility.cue` classification. Out of scope for v1; CI enforces presence but classification remains a human decision.
- **Non-GitHub fork support.** `apprafter platform fork` for GitLab and other Git hosts. Phase 2+ depending on user demand.

## References

- ADR 0025 (GitOps control surface).
- ADR 0026 (PlatformStack CRD).
- [Helm OCI artifact distribution](https://helm.sh/docs/topics/registries/).
- [Cosign Helm chart signing](https://docs.sigstore.dev/cosign/installation/).
