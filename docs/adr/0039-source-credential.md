# ADR 0039: Private-repo credentials via a `SourceCredential` CRD — operator-derived, config-only, sealed material

## Status

Accepted (2026-05-30).

## Context

Deploying a customer application from a **private** repository needs two distinct accesses: Argo CD must read the repo to render and sync manifests (git-read), and the kubelet must pull the workload image (registry-pull). The flow shipped in plan item 1.79a handles neither cleanly:

- `apprafter repo creds add` writes a **raw Argo CD `repo-creds` Secret** (`stringData` url/username/password) directly into the `argocd` namespace.
- `apprafter app add` writes a **raw Argo CD `Application` CR** directly into `argocd`.
- No image-pull secret is derived at all.

Three defects follow. **(1)** The CLI creates raw resources that bypass the admission webhook and the operator — they are not gated, and in particular a credential change is not classified as a potentially-destructive operation the way every other change is (the MCP security checklist already flags that the destructive-op gate must be actor-agnostic and must not be bypassable by raw resource creation). **(2)** Credential material sits as a plaintext Kubernetes Secret, which is incompatible with the SealedSecrets posture that is the Tier-1 default (plan item 2.11) and with ADR 0024 Layer 2 (secrets accessed via workload identity, never `kubectl get secret`; tokens must not be scavenge-able). **(3)** The registry pull-secret is missing, so any application whose image lives in a private registry simply does not start.

This is **not** a managed-only concern. The open-source self-host path with a private repo hits all three defects immediately; the managed Hosted Services launch needs exactly the same flow. The decision is taken OSS-first.

### The GitHub credential reality shaping the design

The credential type is constrained by what GitHub actually accepts, which differs sharply between the git half and the registry half:

- **GHCR (registry-pull)** accepts **only** a classic PAT with `read:packages` (plus `repo` when the package inherits visibility from a private repo), or `GITHUB_TOKEN` inside Actions (not applicable to a kubelet at runtime). **Fine-grained PATs have no packages permission** and fail with 403. **GitHub App installation tokens are not accepted by `ghcr.io`** (they work for the API and git, not the registry).
- A classic PAT's `repo` scope is **all-or-nothing** — there is no read-only-private variant — so a single GHCR-capable GitHub credential over-grants (write to every repo the owner can reach) unless package access is managed explicitly at the package level (then a `read:packages`-only PAT, no `repo`, suffices).
- **GitLab / Forgejo / Gitea** have proper narrow read-only scopes (`read_repository`, `read_registry`), so a single narrow token covers both halves there.

The platform cannot fix GitHub's coarse scopes; it can only store the credential securely and offer least-privilege paths to those who want them.

## Decision

Introduce **`SourceCredential`**, an AppRafter CRD that is a **config / reference object carrying zero secret material**. The operator owns all derivation; the CLI becomes a thin front-end; the credential material lives sealed.

### Shape

```cue
apiVersion: apprafter.io/v1alpha1
kind: SourceCredential
spec: {
    // At least one of git / registry. Both halves are independent;
    // a single classic PAT is simply the same backend in both.
    git?: {
        backend:      #Backend          // sealedSecretRef | openBaoPath
        repoPrefixes: [...string]       // e.g. ["github.com/myorg/"]
    }
    registry?: {
        backend: #Backend
        hosts:   [...string]            // e.g. ["ghcr.io/myorg/"]
    }
}
// #Backend = { sealedSecretRef: {...} } | { openBaoPath: string }
// No token, no base64 material, ever, in the spec.
```

`status` carries per-half `conditions` with states **`Present` / `Valid` / `Invalid` / `Unverified`**, the covered prefixes/hosts, and `lastValidated`.

### Single source of truth; operator derivation

The operator derives both materializations from the one `SourceCredential` + its sealed material:

- `git` → a **prefix-matched** Argo CD `repo-creds` Secret in `argocd` (Argo selects the credential for a clone by URL-prefix match against registered `repo-creds`; nothing inside the app repo is consulted).
- `registry` → a **static `dockerconfigjson` pull-secret** in the workload namespace, **auto-attached** to the workload ServiceAccount / `Deployment.imagePullSecrets` by **registry-host match** against the rendered image (`image: ghcr.io/myorg/...` → the `SourceCredential` covering host `ghcr.io/myorg/...`).

These derived Secrets are **operator outputs, never hand-managed**. There is therefore no two-sources-of-truth synchronisation problem: one input, N derived outputs, reconciliation keeps them consistent — the standard operator pattern. Rotating the material re-derives both halves automatically.

### Config-only CRD + sealed material — not in the spec, not in the app repo

The material lives in a **SealedSecret (Tier 1)** or **OpenBao (Tier 2)**, sealed client-side by the CLI. It is never in the CRD spec (a CRD is not a Secret; material in `spec` would be plaintext-at-rest with weaker handling than a Secret and would be scavenge-able, violating ADR 0024). It is never in the app repo, for a hard ordering reason: **Argo needs the git credential to clone the private repo, so a credential living inside that repo cannot be read to obtain it (chicken-and-egg).** The credential definition and material live in a platform-scoped location; the application repository carries nothing about credentials — host/prefix auto-match removes any need for a per-application reference.

SealedSecrets is asymmetric: the CLI seals with the controller's **public** cert (pinned or fetched over the TLS-authenticated kube API to avoid cert substitution); only the in-cluster **private** key decrypts. The sealed blob is therefore safe in transit, at rest, and in Git — which is what makes the GitOps delivery mode below safe.

### Validation and status

Validation splits across the two places it belongs:

- **CLI** keeps a cheap **shape check** (the existing PAT-shape regex) to catch copy-paste typos before pushing — fast fail, good UX.
- **The operator** performs **real validity** (reachability/auth: `git ls-remote` for git, a registry token-exchange/HEAD for registry) on change and periodically with backoff, and writes the result into `status`. This is strictly better than the CLI doing an API ping (which 1.79a rejected for flakiness and rate-limiting): the operator is in-cluster, retries, and reports continuously.

Where the operator has no egress to the host (air-gapped / restricted), validity is **`Unverified`, not `Invalid`**, and the coverage gate is configurable between `present` and `confirmed` (mirroring the 1.79a `--no-ping` philosophy).

### CLI as a thin front-end

`apprafter repo creds` stops being a Secret manager and becomes a front-end over `SourceCredential`: `add` = shape-check + seal + create/update CR; `list` / `show` = read `status` (coverage + validity), **never** the material; `rotate` = re-seal; `remove` = delete the CR with a reverse-dependency gate (reusing `find_apps_matching_prefix` from 1.79a). `app add`'s coverage check reads `status` (is there a `Valid` cred covering this prefix?) instead of guessing.

The CLI **cannot read existing material** — not merely by RBAC but **cryptographically**: it cannot decrypt a SealedSecret without the cluster's private key, and `list`/`show` expose only status. This realises ADR 0024's "tokens do not sit where an agent can find them" by construction. It also pre-defines the scoped CLI role for day-2 hardening: read `SourceCredential` (+status) and write `SourceCredential` / `SealedSecret`, with **no read on the derived Secrets**.

### Gating: MigrationPlan on destructive credential change

`SourceCredential` is an AppRafter CRD, so registration mutations pass through the admission webhook and the MigrationPlan gate (ADR 0027). A `SourceCredentialMigrationStrategy` implements `detect_destructive(old, new) -> Option<DestructiveChange>`: rotating material to an equivalent valid credential is **non-destructive (`None`)**; **removing a covered repo-prefix or registry-host, deleting a credential while applications match it, or narrowing scope** are destructive → MigrationPlan. The gate is **actor-agnostic** — it catches a human fat-fingering a credential and the CLI alike.

### Credential type at launch; split-ready schema

The launch default is a **single classic PAT** (`repo` + `read:packages`) used in both halves. The breadth is GitHub's limitation, not the platform's choice, and the platform stores it sealed regardless. The schema supports **independent git / registry backends from day one** (this is the only least-privilege path on GitHub today): the narrow paths — deploy-key or fine-grained PAT for git plus a `read:packages`-only classic PAT for registry (with package-level access configured), or a single `read_repository`+`read_registry` token on GitLab — are **opt-in via the CLI wizard, not the default**, and the wizard's credential-type chooser is deferred until operator feedback warrants it.

### Delivery is mode-agnostic

Because this is purely "how does the CR reach the cluster", both modes fall out of the existing design with no extra concept: **CLI→cluster** (`kubectl apply` SealedSecret + CR) or **config-repo** (commit sealed material + CR to the optional infra repo; Argo syncs). The material is sealed in both, so it is safe in Git. In a pure-GitOps mode with no cluster read, the CLI matches on *declared* prefixes from the config-repo and validity is observed in Backstage.

### Relationship to managed and minimal data exposure (ADR 0034 / 0035)

The managed Hosted Services path uses the **same** flow on the customer's own cluster. The hosted side never receives credential material — it cannot, since the material is sealed in the customer cluster and the operator is the only holder. This reinforces the Minimal Data Exposure guarantee (ADR 0035) by construction rather than by policy.

### Tier relationship

The flow is **identical on T1 and T2**: only the `backend` changes (SealedSecrets on T1, OpenBao on T2), behind the same CRD. The pull-secret lands in workload namespaces; when hard multi-tenancy is enabled (ADR 0038), those namespaces are tenant-scoped, and derivation is unchanged.

## Consequences

- **`repo creds` becomes a front-end; the Argo `repo-creds` Secret and the pull-secret become derived outputs.** No hand-managed credential Secret, no synchronisation between a legacy Secret and the new CRD — single source of truth.
- **All three defects close.** Registration is gated (defect 1); material is sealed, never plaintext (defect 2); the registry pull-secret is derived and attached (defect 3).
- **A scoped CLI RBAC role is now defined** (read `SourceCredential`+status, write `SourceCredential`/`SealedSecret`, no read on derived Secrets) — a ready seed for the Phase-4 scoped-identity hardening, without changing the everyday CLI today.
- **Cross-phase dependency.** This item depends on the **SealedSecrets controller + CLI-seal slice of 2.11** (which the speedrun already places in bucket A) and on the **MigrationPlan CRD (1.72–1.78)** for the destructive-gating sub-part. The SealedSecrets controller slice must precede this item in execution order even though its phase number is higher; see the speedrun reconciliation.
- **GitHub over-grant is accepted by default.** A single classic PAT carries `repo` write. Least-privilege is documented and offered as an opt-in split; it is not forced, and the platform's own handling is sealed regardless.
- **Spec update required.** `spec.md` needs: a new section for the `SourceCredential` CRD; a `§4.5` addition to the Application Operator responsibilities ("derive prefix-matched Argo repo-cred + host-matched workload pull-secret from `SourceCredential`; validate and report status"); a `§4.7` External Surface note on the credential model; and the updated `repo creds` CLI semantics. Until those edits land, this ADR governs.

## Alternatives considered

### Keep the CLI creating raw Argo resources (1.79a as shipped)

Rejected. It is ungated (bypasses the admission webhook and MigrationPlan), stores material as a plaintext Secret, has two would-be sources of truth once the CRD exists, and never derives a pull-secret. It is exactly the flow being repaired.

### Put the credential (or its config) in the app repo's `Application.cue`

Rejected on a hard technical ground, not aesthetics: Argo needs the git credential to clone the private repo, so a credential whose source of truth lives inside that repo cannot be read to obtain it (chicken-and-egg). It also mixes platform credentials into application source. Host/prefix auto-match gives the desired "it just works for a new app under the same org" UX without putting anything credential-related in the app repo.

### Material inside the `SourceCredential` spec (base64 / plaintext)

Rejected. A CRD is not a Secret; material in `spec` is plaintext-at-rest with weaker handling than a Kubernetes Secret and is scavenge-able, directly contradicting ADR 0024.

### GitHub App as the canonical credential

Rejected for this scope. `ghcr.io` does not accept App installation tokens, so the registry half stays unsolved; and the problem is OSS-first with no managed yet. A GitHub App may return as a **managed-era** refinement for the git half, paired with a machine-user PAT for the registry half.

### CLI performs API-ping validation

Superseded. Validity moves to the operator, where it is robust and continuous and reported in `status`; the CLI keeps only the cheap shape check it already has.

### Two separate CRDs (`RepoCredential` + `RegistryCredential`)

Rejected. It is one origin with two materializations (most visibly in the single-PAT case). One CRD with optional `git` / `registry` halves avoids duplication while still expressing the two-token least-privilege split.

## Risks

- **Cross-phase ordering.** The new 1.79c (Phase-1 label) depends on 2.11 (SealedSecrets, Phase-2 label). The speedrun SR order must place the 2.11 controller+seal slice before 1.79c even though 2.11's phase number is higher. Mitigation: the reconciliation instruction; SR order, not phase number, governs execution.
- **Validity latency at `add`.** The operator must reconcile before validity is known. Mitigation: the CLI briefly polls `status`; otherwise it returns "submitted, validity pending" and `list` shows it shortly.
- **Operator egress required for validation.** Restricted-egress clusters surface `Unverified`, not `Invalid`; the coverage gate is configurable (`present` vs `confirmed`).
- **Raw SealedSecret edit bypasses the gate.** A raw edit changes only the *value* (a rotate, low-risk); a raw edit to a *wrong* value breaks apps with no gate — the known raw-kube bypass residual (ADR 0024 + core-resource coverage), not solved here. The destructive-relevant surface (coverage / removal) is on the gated CRD.
- **GitHub over-grant by default.** The single-PAT path carries `repo` write. Mitigation: least-privilege split documented and offered; sealed handling regardless.
- **PAT expiry under org policy.** If the PAT expires unrotated, both git and pull break. Mitigation: an optional user-declared `expiresAt` drives a status warning, and operator validity surfaces revoked/expired credentials; `rotate` re-derives both halves.

## Owner

Core platform team. Andrey Ryahovskiy (`remryahirev@gmail.com`) convenes reviews and approves amendments. Lands as plan item **1.79c** (the next free suffix). The previous 1.79b — the already-shipped `app open` + scaffolding sub-phase (releases v0.1.161–v0.1.174) — keeps its number, so no released changelog or git history is renumbered. Depends on **2.11** (SealedSecrets controller + CLI-seal slice) and **1.72–1.78** (MigrationPlan CRD).

## Re-evaluation

Re-evaluate when:

- The **first non-GitHub-host customer at scale**, or the **first customer demanding least-privilege git access**, arrives — prioritise the split-credential wizard path.
- A **short-lived-token registry** enters scope (a managed-era GitHub App on the git half plus a machine-user, or a cloud registry such as ECR) — revisit credential rotation (a refresher controller or a kubelet credential-provider plugin), currently unnecessary for a non-expiring classic PAT against GHCR.
- **OpenBao lands** (2.7–2.8 / 3.11) — `SourceCredential.backend` gains the OpenBao path on T2; verify the backend abstraction holds with no CRD change.
- **GitHub ships fine-grained packages support or App-token GHCR support** — revisit the classic-PAT-forced default, which would permit a narrower default credential.

Otherwise no scheduled re-evaluation.

## References

- Plan item **1.79a** — CLI app/repo subcommands + `repo creds` (the flow this refactors into a front-end; `find_apps_matching_prefix` reused for the remove gate).
- Plan item **2.11** — SealedSecrets integration (material backend on T1; the controller + CLI-seal slice is a dependency, pulled ahead of the rest of 2.11).
- Plan items **1.72–1.78** — PlatformController + MigrationPlan CRD (the destructive-credential-change gate; this CRD adds a `MigrationStrategy.detect_destructive`).
- **ADR 0024** — cluster-admin constraint; Layer 2 secrets accessed via workload identity, not `kubectl get secret`; tokens must not be scavenge-able.
- **ADR 0027** — MigrationPlan as the destructive-change gate (actor-agnostic; this CRD plugs into the same strategy framework).
- **ADR 0025 / 0028 / 0029** — GitOps control surface, platform-stack distribution, CUE CMP (the config-repo delivery mode; Argo's URL-prefix `repo-creds` matching).
- **ADR 0034 / 0035** — managed offering model; Minimal Data Exposure (the managed path uses the same flow and never receives credential material).
- **ADR 0023 / 0038** — Kamaji / tenant scoping (the pull-secret lands in workload namespaces; tenant-scoped when hard MT is enabled on T2).
- `spec.md` §4.5 (Application Operator responsibilities — derivation addition), §4.7 (External Surface — git host / container registry credential), §4.4 (OpenBao / SPIFFE — T2 backend), §3.8 (MigrationPlan), §3.11 (PlatformStack control surface).
- GHCR auth constraints: classic PAT `read:packages` (+`repo` for repo-inherited package visibility); fine-grained PATs unsupported for packages; App installation tokens not accepted by `ghcr.io`.
