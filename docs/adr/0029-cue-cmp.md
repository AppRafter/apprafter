# ADR 0029: CUE compilation for user app repositories via Argo CD CMP

## Status

Draft.

## Context

ADRs 0025–0028 address the platform-stack side: platform manifests originate from our CUE source, are rendered into a Helm chart, distributed via OCI, and pulled by Argo CD without user-side rendering. CUE is invisible to users on the platform side.

User application repositories are different. The golden-path template (phase 1.11) generates `apprafter/Application.cue` in the user's app repository. Spec §5 designates CUE as the configuration language. When Argo CD synchronizes a user app repository, it must compile `*.cue` files into the `Application` CR YAML that Kubernetes accepts. Argo CD does not understand CUE natively; it supports Helm, Kustomize, Jsonnet, and raw YAML directories. Without a compilation step, GitOps deployment of user apps does not work.

## Decision

Provide an Argo CD Config Management Plugin (CMP) for CUE, shipped as a sidecar container in the `argocd-repo-server` Deployment. The CMP detects `*.cue` files in synced repositories, runs `cue export ./... --out yaml`, and passes the rendered manifests to Argo CD's standard sync pipeline.

The plugin configuration (`plugin.yaml` inside the sidecar):

```yaml
apiVersion: argoproj.io/v1alpha1
kind: ConfigManagementPlugin
metadata:
  name: cue
spec:
  discover:
    find:
      glob: "**/apprafter*.cue"
  generate:
    command: [sh, "-c"]
    args:
      - cue export ./... --out yaml
```

The sidecar image (`ghcr.io/apprafter/argocd-cue-cmp:<version>`) is built and published in our CI pipeline alongside the platform-stack chart. The platform-stack chart configures Argo CD's `repoServer.extraContainers` to include this sidecar by default.

For monorepos with multiple apps and shared CUE schemas, Argo CD Applications use the `argocd.argoproj.io/manifest-generate-paths` annotation to control which paths trigger a re-render and what subset of the repository is available to the CMP at render time.

## Rationale

### CMP is the native Argo CD mechanism

CMPs are an official, supported Argo CD extension point used upstream for Kustomize variants, Jsonnet, Tanka, and others. They run in a sidecar with controlled filesystem access, integrate with Argo CD's caching layer, and are not invasive — they extend Argo CD without modifying its core.

### Server-side compilation preserves user experience

Users write CUE, commit it, push to their repository. They do not run a render step locally, they do not configure a pre-commit hook, they do not maintain a separate "rendered output" branch. Argo CD's repo-server sees the repository and produces the rendered manifest internally.

### Dependencies on shared schemas work via annotation

A monorepo with structure:

```
my-services/
├── schemas/
│   └── v1alpha1/                # CUE schemas shared across services
├── parser/
│   ├── src/
│   └── apprafter/Application.cue
└── gateway/
    ├── src/
    └── apprafter/Application.cue
```

The Argo CD Application for `parser` uses `manifest-generate-paths: /parser,/schemas`. The CMP renders only when those paths change, and the rendered Application sees the schemas as imports.

### Optional for users who prefer raw YAML

Users who do not want CUE can simply put YAML files in their repository. Argo CD's directory source handles them directly without CMP involvement. The CMP only activates when `*.cue` files are present.

## Implementation outline

| Step | Description | Size |
|---|---|---|
| 1 | Dockerfile for `argocd-cue-cmp`: Alpine base + `cue` binary + `plugin.yaml` + entrypoint wrapper | XS |
| 2 | Entrypoint wrapper: structured error output, friendly diagnostics for common CUE errors | S |
| 3 | Integration into platform-stack chart: `repoServer.extraContainers` populated with the sidecar | XS |
| 4 | End-to-end test: app repository with `apprafter/Application.cue` + `schemas/` imports, Argo CD sync produces correct Application CR | S |
| 5 | Documentation in `docs/dev-guide/`: writing `apprafter/Application.cue` for GitOps deployment, troubleshooting CUE compile errors as surfaced by Argo CD | S |

## Consequences

**Positive:**
- CUE↔Argo CD compilation gap is closed without compromising the "CUE as configuration language" positioning.
- User workflow is standard GitOps: edit, commit, push.
- The sidecar is self-contained and does not affect Argo CD core stability.
- Users who prefer raw YAML are not forced into CUE — the CMP activates conditionally.

**Negative:**
- The sidecar adds approximately 50 MB of memory overhead to `argocd-repo-server`.
- A custom Docker image must be built, signed, and published alongside platform-stack releases.
- CMP discovery scanning can be slow on large monorepos; mitigation via `manifest-generate-paths` annotation requires user awareness.

## Risk

**Main risk:** upstream Argo CD changes its CMP API (breaking `plugin.yaml` schema). **Mitigation:** the channel system from ADR 0028 catches breaking changes — `beta` deployments reveal CMP regressions before `stable` ships them.

**Secondary risk:** CUE compilation errors are surfaced through Argo CD's standard error reporting (Application sync status with stderr). Verbose CUE errors may not display well in the Argo CD UI. **Mitigation:** the entrypoint wrapper script post-processes `cue` output into structured, single-line-summary form with details available in the full sync log.

## Owner

Core platform team.

## Re-evaluation triggers

- If sidecar overhead becomes noticeable on large clusters running many Argo CD instances (10+), consider a pre-render approach: a CI job in user repositories renders CUE to YAML in a separate branch, Argo CD reads from that branch. Trades repository pollution for resource savings.
- If upstream Argo CD adds native CUE support, retire the CMP. This is unlikely in the foreseeable future based on current Argo CD roadmap.

## Still open

- **Canonical filename for user app CUE.** Phase 1.11 uses `apprafter/Application.cue`. Alternatives include `apprafter.cue` at repository root or `.apprafter/app.cue`. Settling on one choice early prevents fragmentation. Recommendation: keep `apprafter/Application.cue` as used in phase 1.11.
- **Multi-app monorepo strategy.** One Argo CD Application per service, or one ApplicationSet with a Git generator discovering service paths? ApplicationSet is the standard pattern but adds indirection. Decision deferred until multi-app monorepo use cases mature in Phase 2+.
- **CUE compile error display in Backstage.** A future Backstage plugin could surface CUE compile errors directly in the Application view, alongside Argo CD sync status. Out of scope for this ADR.

## References

- ADR 0025 (GitOps control surface).
- ADR 0028 (Distribution — the CMP sidecar ships as part of the platform-stack chart).
- [Argo CD Config Management Plugins](https://argo-cd.readthedocs.io/en/stable/operator-manual/config-management-plugins/).
- Phase 1.11 (golden-path template for `apprafter/Application.cue`).
