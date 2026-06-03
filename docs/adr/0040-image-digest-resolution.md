# ADR 0040: Image tag → digest resolution & auto-rollout — the operator owns the image zone

## Status

Accepted (2026-06-03).

## Context

AppRafter deploys a customer workload from `Application.spec.base.image`, a string the user writes — in practice a mutable tag such as `ghcr.io/acme/app:latest` produced by the customer's CI from a protected branch. Argo CD syncs the `Application` CR from Git; the operator renders the `Deployment`.

Re-pushing the image under the same tag does **not** roll the workload. The rendered `Deployment` references the tag verbatim, the operator sets no `imagePullPolicy` and stamps no image-derived annotation, and nothing watches the registry — so a same-tag re-push leaves the pod template byte-identical and Kubernetes never rolls. The `Application` CR in Git is also unchanged, so Argo records no event either.

This produces a **fake consistency**: Git says `:latest`, the cluster says `:latest`, the two *look* equal, but they are different bytes. GitOps's core promise — "what is in Git is what is running" — is already void under mutable tags, silently. The operator cannot answer "what image is actually running" from the manifest, and `git revert` is not a true rollback of the image.

Two forces shape the decision:

- **The industry-standard client flow is mutable-tag-as-deploy.** A customer's CI builds from a protected branch (commonly `master`) and pushes `:latest`; "the image built from master changed" is *intended* to mean "deploy it." Forcing every user onto unique immutable tags plus a manual manifest bump is the opposite of the push-and-it-deploys experience an opinionated PaaS exists to provide.
- **The credential and rendering infrastructure already exists.** ADR 0039 added `SourceCredential.spec.registry` (host prefixes + a sealed `dockerconfigjson`) and `operator-controllers/application/src/pull_secret.rs::pick_pull_credential(image, &creds)`, which already matches a rendered image's registry host to the credential that produces the kubelet pull-secret. `operator-rendering` is a pure `Application → Vec<k8s objects>` function; the controller is the I/O layer.

The responsibility split is the existing one: Argo owns the **manifest zone** (Git → `Application` CR), and the cluster/operator owns the **image zone** (a manifest can already arrive from Argo whose image the kubelet cannot pull — different zones of responsibility).

## Decision

We will have the **Application controller resolve the referenced image tag to its current registry digest** on each reconcile, render the `Deployment` pinned to `repo@sha256:<digest>`, and record the resolution in the `Application` status. A moved tag yields a new digest, which changes the pod template, which triggers a **normal rolling update**. The platform thereby owns the **pull half of push-pull**: the customer's CI pushes an image, and the platform auto-deploys it.

Specifics:

- **Locus — controller, not renderer.** The controller performs the OCI registry lookup (a manifest `HEAD`/`GET` reading `Docker-Content-Digest`) and passes the resolved digest *string* into `render`. `operator-rendering` stays pure; the network I/O lives in the controller, where I/O already lives.
- **Auth — reuse ADR 0039.** `pick_pull_credential(image, &source_credentials)` selects the covering `SourceCredential`; its `dockerconfigjson` authenticates the manifest request. An image with no covering credential resolves **anonymously** (public registries).
- **Cadence.** Re-resolve on the existing reconcile requeue (~60 s — aligned to the current controller cycle, no second timer), using a conditional request keyed on the recorded digest so an unchanged tag is a no-op.
- **Pin form — digest in `image:`.** The resolved digest is rendered into the container `image:` (`repo@sha256:…`) so the running pod is *exactly* the resolved digest, with no time-of-check/time-of-use gap if the tag moves again between resolve and pull. The human-readable tag remains in `spec.base.image` (Git) and in status.
- **Status — record the truth.** The `Application` status carries `image.tag` (as written), `image.resolved` (`repo@sha256:…`), and `image.resolvedAt`. `apprafter app status` surfaces the running digest, restoring real auditability: Git shows the tag, status shows what is running.
- **Opt-out — `spec.base.imagePolicy.resolve: "digest" | "off"`,** default `"digest"`, on **all tiers**. `"off"` renders the image reference verbatim (today's behaviour) and performs **no** registry poll; the user manages their own reference (e.g. a hand-pinned digest for Regulated immutability). Opt-out does **not** force the user to write a digest — it merely disables resolution.
- **No migration gate.** An image change — whether a newly resolved digest or a manual reference edit — does **not** create a `MigrationPlan` or pause the workload. An auto-deploy that paused for approval on every push would break the entire UX. (A gate would make more sense for the digest-pinned / Regulated mode, but those customers carry their own change-control regime; we ship no image-change gate for now.)
- **Argo — unchanged.** Argo syncs the `Application` CR from Git; the tag in Git is unchanged, so the app stays `Synced`. The operator updates its own owned child `Deployment` out-of-band; Argo does not manage the `Deployment` spec and will not revert the digest.
- **Degradation — never block.** If resolution fails (registry unreachable, no covering credential for a private image, malformed reference), the controller renders the **verbatim tag** (today's behaviour) and sets a status condition `ImageResolved=False` with a reason. Resolution is best-effort; it never blocks the rollout.

In scope: tag→digest resolution for public and private (via `SourceCredential.registry`) registries, the opt-out knob, the status fields, and the `app status` surfacing. Out of scope: registry push webhooks (poll only), an image-change approval gate, registry mirroring, and a first-class `apprafter app redeploy` command (the escape hatch remains `kubectl rollout restart`; a redeploy verb is a separate small follow-up if wanted).

## Consequences

- **Easier:** push-and-it-deploys (Hosted PaaS-like) on the customer's existing mutable-tag CI; real auditability (status carries the running digest); immutable running pods (digest-pinned); no new credential infrastructure (reuses ADR 0039); the renderer stays pure and unit-testable.
- **Harder / neutral:** the controller now performs registry network I/O on the reconcile path (latency and registry rate-limit exposure — mitigated by conditional requests and the existing cadence); a moved tag rolls within roughly the reconcile interval, not instantly; private-image resolution depends on a covering `SourceCredential`, else it falls back to the tag.

## Alternatives considered

- **Force immutable tags + Git-bump** (`apprafter app deploy <tag|digest>` edits `spec.base.image` and commits; or Argo CD Image Updater writes the digest back to Git). GitOps-pure — the deploy is a Git event — but it pushes work onto the user (unique tags + manifest bumps) and is the *opposite* of the push→deploy value. Rejected as the default; users may still pin immutable references and opt out.
- **Annotation-trigger variant** (keep `image: :latest` + `imagePullPolicy: Always` + a digest annotation that changes to force a rollout). Simpler, but pods pull the *tag* at pull time, which may have moved again since the resolve → the running image need not match the recorded digest (TOCTOU). Rejected in favour of digest-pinning `image:`.
- **Argo CD Image Updater as the mechanism.** An extra component with Git write-back; heavier (write credentials, a second controller, a commit loop) and still couples deploys to a Git write. May be offered as an opt-in later; operator-side resolution is the default.
- **Resolve inside `operator-rendering`.** Rejected — it would break the pure-rendering invariant. The lookup lives in the controller; the renderer receives a resolved string.

## Risks

- **Registry rate-limits** (GHCR, Docker Hub) at fleet scale × apps × ~60 s. Mitigation: conditional requests (a no-op when the digest is unchanged), reuse of the existing requeue (no extra polling), room for per-registry backoff.
- **Reconcile-path latency** from registry I/O. Mitigation: a bounded timeout with graceful fallback to the tag; resolution is best-effort and never blocks reconcile.
- **A genuinely bad image auto-deploys** — the flip side of push→deploy. Accepted: this is the customer's CI contract (tagging a protected branch = intent to deploy); recovery is `kubectl rollout undo` / re-push, and the resolved digest is recorded for forensics. Regulated workloads opt out.
- **Private-image resolution requires a covering `SourceCredential`;** absent one, there is no auto-deploy (it falls back to the tag). Accepted and surfaced via the `ImageResolved=False` status condition.

## Owner

Platform / operator team.

## Re-evaluation

Revisit if reconcile p99 latency attributable to registry I/O exceeds ~200 ms, if registry rate-limits bite at fleet scale, or when a first-class immutable-deploy / approval flow is required for a regulated customer.

## References

- ADR 0039 — `SourceCredential` (the registry pull-secret seam; `pick_pull_credential`).
- `operator/operator-controllers/application/src/pull_secret.rs`; `operator/operator-rendering/` (pure render).
- The 2.4g `needs.pg` CMS manual walk, where the mutable-tag asymmetry surfaced.
- `spec.md` (application image / deployment), `plan.md` Phase 2.
