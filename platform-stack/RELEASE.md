# platform-stack release procedure

> Maintainer-only. Operators **install** platform-stack via Argo
> CD or plain Helm — the procedure here is for the people
> publishing the chart from the AppRafter monorepo.
>
> See [ADR 0028](../docs/adr/0028-platform-stack-distribution.md)
> for the design rationale and [the publish workflow](../.github/workflows/platform-stack-publish.yml)
> for the CI plumbing this procedure drives.

## Versioning rules

`platform-stack` uses **semver** independently of the AppRafter
monorepo's `v0.1.x` patch stream:

- **MAJOR** — chart values shape changed, a component was
  removed, or the PlatformStack CRD payload it produces is
  incompatible. Operators MUST read `cue/compatibility.cue`
  before upgrading; `PlatformController` (Phase 2+) gates
  automated apply on `change != "breaking"`.
- **MINOR** — additive component changes (new component, new
  optional tier overlay, new optional value).
- **PATCH** — bug fixes / dependency version bumps within
  the same chart shape.

The first published version is **0.1.0**. Chart MINOR tracks
the AppRafter monorepo **phase** (Phase 1.5 → chart 0.1.x;
chart MINOR will bump to 0.2.0 alongside the `v0.2.0-services`
milestone when Phase 2 services land). Chart patch versions are
independent of the monorepo's `v0.1.x` patch stream — the two
share only MINOR/MAJOR semantics.

## Pre-release checklist

Before tagging a release, confirm:

- [ ] `platform-stack/cue/compatibility.cue` has an entry for
      the new version. The publish workflow's first step calls
      `scripts/check-platform-stack-version.sh <version>` and
      fails fast if the entry is missing.
- [ ] The entry's `change:` field accurately classifies the
      delta vs the previous version. Be conservative —
      misclassifying `breaking` as `safe` lets
      `PlatformController` auto-apply and break operator
      clusters.
- [ ] The entry's `operatorVersion:` matches the
      `apprafter-operator` + `admission-webhook` images
      referenced from `cue/component_apprafter-operator.cue` and
      `cue/component_admission-webhook.cue`.
- [ ] `platform-stack/CHANGELOG.md` has a section for the new
      version with operator-facing notes (the in-chart README
      shipped to the OCI artifact links here).
- [ ] Local sanity render passes:
      ```sh
      cd platform-stack && make render
      helm template platform dist/platform-stack-<version>
      helm template platform dist/platform-stack-<version> \
          --values dist/platform-stack-<version>/examples/values.team.yaml
      ```
- [ ] `bash scripts/lint-cue.sh` clean.
- [ ] `cargo test --workspace` clean (paranoia — platform-stack
      doesn't depend on Rust, but the workspace cross-impact
      catches accidental linting fallout).

## Tagging

Release tags are **prefixed**: `platform-stack/v<version>`.
The prefix scopes the publish workflow to chart releases only
— AppRafter monorepo releases (`v0.x.y` / `v1.x.y`) trigger
the operator + webhook publish workflow, not this one.

```sh
# Pre-release (-rc1, -rc2, ...) — pushes :<version> only,
# no :latest alias, GitHub Release marked `prerelease: true`.
git tag platform-stack/v0.1.0-rc1
git push origin platform-stack/v0.1.0-rc1

# Stable release — pushes :<version> AND retags :latest,
# GitHub Release marked stable.
git tag platform-stack/v0.1.0
git push origin platform-stack/v0.1.0
```

The push triggers `.github/workflows/platform-stack-publish.yml`.
Watch the run in the Actions tab; on success the workflow:

1. Validates `compatibility.cue` has the entry.
2. Renders the chart from CUE (`make -C platform-stack render-only`).
3. Runs `helm lint` + tier-1 / tier-2 smoke templates.
4. Packages → `platform-stack-<version>.tgz`.
5. `helm push` to `oci://ghcr.io/<owner>/platform-stack:<version>`.
6. `cosign sign` the immutable OCI digest (keyless via Sigstore
   OIDC + the workflow's ambient GitHub identity).
7. `cosign sign-blob` the `.tgz` → `.tgz.sig` + `.tgz.pem`.
8. On stable: `oras tag` the artifact as `:latest`.
9. Creates a GitHub Release with the three files attached and
   a body containing the install + verify snippets.

The `id-token: write` permission is what makes keyless signing
work — never remove it from `permissions:` block.

## After publish

- Verify the release in a clean environment:
  ```sh
  # OCI install via plain Helm:
  helm install platform oci://ghcr.io/<owner>/platform-stack \
      --version <version>

  # Cosign verify:
  cosign verify ghcr.io/<owner>/platform-stack@<digest> \
      --certificate-identity-regexp 'https://github.com/<owner>/<repo>/' \
      --certificate-oidc-issuer 'https://token.actions.githubusercontent.com'
  ```
- Bump the in-tree `RELEASED_OPERATOR_VERSION` constant in
  `cli/cli-providers/src/k8s/mod.rs` IF the new platform-stack
  release pulled in a new operator+webhook image pair. (The
  platform-stack chart and the CLI's hard-coded operator
  image tag remain coupled until sub-phase 1.71 lands the CLI
  cutover to consuming the published chart.)
- Update `docs/changelog/UNRELEASED.md` with a one-paragraph
  pointer to the new version. (The chart's own
  `platform-stack/CHANGELOG.md` already has the detailed
  entry.)

## Failure modes

If the workflow fails partway through (e.g. `helm push` after
`helm package`), GitHub keeps the tag but no Release is
created and no OCI artifact lands. Recovery:

1. Inspect the workflow logs to find the failing step.
2. Fix the underlying issue (most often: missing
   `compatibility.cue` entry → add and commit before
   re-tagging).
3. Delete the broken tag both locally and on the remote:
   ```sh
   git tag -d platform-stack/v<version>
   git push origin --delete platform-stack/v<version>
   ```
4. Re-tag and re-push.

The CI workflow is idempotent on a clean run: the second
attempt will skip steps that already completed (helm push
overwrites by tag), so partial states don't leak into the
registry as long as the tag is replayed cleanly.
