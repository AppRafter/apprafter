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

## Source of truth

The chart version lives in exactly **one** place:

- `platform-stack/cue/platform.cue` →
  `currentVersion: #Version & "<version>"`

Everything downstream derives from there:
- `tier_solo.cue` + `tier_team.cue` reference it via
  `version: currentVersion`.
- `compatibility.cue` enforces a CUE-level invariant
  `compatibility: (currentVersion): #VersionRecord` so the
  current version MUST have a matching entry.
- The renderer (`make -C platform-stack render-only`) reads
  `tier1.version` (which equals `currentVersion`) for the
  `dist/<chart>-<version>/` path.
- The publish workflow reads `currentVersion` directly via
  `cue export ./platform-stack/cue/... -e currentVersion`.

A version bump is a **two-line edit**:

1. `platform-stack/cue/platform.cue` — bump
   `currentVersion: #Version & "0.1.0"` → `"0.1.1"`
   (or `"0.2.0"`, etc).
2. `platform-stack/cue/compatibility.cue` — add the matching
   entry `compatibility: "0.1.1": { change: …, … }`.

If you skip step 2, `cue vet -c ./platform-stack/cue/...`
fails at edit time with `compatibility."0.1.1".change:
incomplete value …`, before you can even commit. CI then
runs the same check on push as a belt-and-suspenders.

## Pre-release checklist

Before running the publish workflow, confirm:

- [ ] `currentVersion` in `platform-stack/cue/platform.cue`
      reflects the version you want to publish.
- [ ] `platform-stack/cue/compatibility.cue` has the matching
      entry. (`cue vet -c` will yell at you if it doesn't.)
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
- [ ] `bash scripts/check-platform-stack-version.sh` (no args
      — auto-reads `currentVersion`) → success.
- [ ] Changes committed and pushed to the branch you want to
      release from.

## Publishing

Releases are triggered by **running the workflow**, not by
pushing a tag. The workflow itself writes the
`platform-stack/v<version>` tag at the end of a successful
publish, so a half-baked chart never lands.

```sh
# Normal flow — workflow reads currentVersion from CUE source.
gh workflow run platform-stack-publish.yml --ref <branch-or-sha>

# Watch the run.
gh run watch
```

Or via the GitHub UI: `Actions → platform-stack-publish → Run
workflow → branch: <branch> → Run`.

The optional `version_override:` input is for emergency /
debug re-publishes against a specific commit (the value MUST
still have a `compatibility.cue` entry). Normal flow leaves
it empty.

On success the workflow runs:

1. Reads `currentVersion` from CUE source (or uses the
   `version_override` input).
2. Refuses to proceed if `platform-stack/v<version>` already
   exists on `origin`.
3. Validates `compatibility.cue` has a matching entry via
   `scripts/check-platform-stack-version.sh`.
4. Renders the chart from CUE
   (`make -C platform-stack render-only`).
5. Runs `helm lint` + tier-1 / tier-2 smoke templates.
6. Packages → `platform-stack-<version>.tgz`.
7. `helm push` to
   `oci://ghcr.io/<owner>/platform-stack:<version>`.
8. `cosign sign` the immutable OCI digest (keyless via
   Sigstore OIDC + the workflow's ambient GitHub identity).
9. `cosign sign-blob` the `.tgz` → `.tgz.sig` + `.tgz.pem`.
10. On stable (`<version>` without a `-`-suffix): `oras tag`
    the artifact as `:latest`.
11. `gh release create platform-stack/v<version>` — this
    creates BOTH the GitHub Release AND the underlying git
    tag in one shot, pointing at the workflow's checkout
    SHA. `--prerelease` is set when the version contains
    `-` (e.g. `0.1.0-rc1`).

The `id-token: write` permission is what makes keyless
signing work — never remove it from `permissions:` block.

The `contents: write` permission is what lets `gh release
create` write the tag back to the repo.

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

Tag-creation happens only as the workflow's final step
(`gh release create`), so a failure partway through leaves
neither tag nor release on `origin`. Recovery:

1. Inspect the workflow logs to find the failing step.
2. Fix the underlying issue (most often: missing
   `compatibility.cue` entry — but `cue vet -c` already
   catches that at edit time; if it slipped past, fix and
   commit).
3. Re-run the workflow against the fixed commit:
   ```sh
   gh workflow run platform-stack-publish.yml --ref <branch>
   ```

The OCI registry may have a partial artifact if `helm push`
succeeded but `cosign sign` failed. Re-running pushes the
same tag again (Helm OCI is overwrite-by-tag), then signs
fresh. The pre-flight "tag does not exist on origin" guard
only checks the git side — so a partial-OCI / no-git-tag
state is recoverable by another workflow run.

If the failure happened AFTER `gh release create` (very
unlikely — it's the last step), you'll have a tag + release
without a signature attachment. To clean up:

```sh
gh release delete platform-stack/v<version> --yes --cleanup-tag
```

Then re-run the workflow.
