// SPDX-License-Identifier: FSL-1.1-Apache-2.0

// Image version for the argocd-cue-cmp sidecar. SoT for
// three consumers:
//
//   1. `.github/workflows/argocd-cue-cmp-publish.yml`
//      reads this via `cue export -e version --out text` to
//      decide whether to publish a new image (compared against
//      the existing `argocd-cue-cmp/v<version>` git tag).
//   2. `.github/workflows/argocd-cue-cmp-check.yml` reads
//      it for the PR drift check.
//   3. `platform-stack/cue/component_argocd-cue-cmp.cue`
//      imports `apprafter.io/argocd-cue-cmp` and uses
//      `argocdcuecmp.version` for both the chart's
//      `_components."argocd-cue-cmp".version` and the
//      `values.image.tag` (the chart Application uses this
//      to pin the sidecar image at render time).
//
// Bumping this value alone is enough to trigger a new
// release; the workflow's `detect` job sees the git tag
// doesn't exist for the new version and publishes.
package argocdcuecmp

version: "0.1.13"
