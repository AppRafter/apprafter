// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Pure resolution of `(image, tag)` override pairs from
//! `Infrastructure.spec.operator` / `Infrastructure.spec.admissionWebhook`
//! into the effective `repo:tag` reference helm + kubectl manifests
//! consume.
//!
//! Semantics (variant C from the §1.14 design):
//! - If `image` contains `:` it is a full ref; `tag` is ignored.
//! - Otherwise `image` is a bare repo; effective ref is
//!   `format!("{image}:{tag}")` using the supplied or default tag.
//! - Both arguments default when `None`.

/// Single source of truth for the operator + admission-webhook image
/// tag installed by `cluster-bootstrap` by default. Bump in lockstep
/// with the `release-operator.yml` workflow run that publishes the
/// matching `apprafter-operator` / `apprafter-admission-webhook`
/// container images to ghcr.io.
pub const RELEASED_OPERATOR_VERSION: &str = "v0.1.64";

/// Single source of truth for the platform-stack OCI Helm chart
/// version `cluster-bootstrap` points the root Argo CD Application
/// at. Updated whenever a new `platform-stack/v*` is published.
///
/// Bump in lockstep with `platform-stack/cue/platform.cue`'s
/// `currentVersion`. The chart at this version MUST exist in OCI
/// (`oci://<owner>/platform-stack:<version>`) before a bumped CLI
/// ships, otherwise `apprafter cluster-bootstrap`'s root
/// Application reconcile fails with "pull failed: not found".
pub const RELEASED_PLATFORM_STACK_VERSION: &str = "0.1.6";

/// Default OCI registry path the chart is published to.
///
/// Stored **without** the `oci://` scheme prefix. Two reasons:
///
///   - Argo CD repository registrations carry a bare URL +
///     `enableOCI: "true"` flag; the scheme is implicit. Both
///     the loader values in
///     `cli-providers::k8s::argocd_loader_values_yaml` and the
///     chart's `component_argocd.cue` register this URL
///     verbatim under `configs.repositories.apprafter.url`.
///   - The root Application's `spec.source.repoURL` MUST match
///     the registration URL byte-for-byte for Argo CD to
///     resolve it. Passing `oci://ghcr.io/apprafter` would
///     trigger a malformed `helm pull --repo oci://... chart`
///     command (walk-found bug, v0.1.99 → v0.1.100).
///
/// Fork users (`apprafter platform fork --to ghcr.io/myorg`,
/// plan.md 1.74) override by passing `ghcr.io/myorg` here.
/// `helm push` workflows independently prepend `oci://` when
/// invoking `helm push`.
pub const APPRAFTER_PLATFORM_STACK_DEFAULT_REPO: &str = "ghcr.io/apprafter";

/// Chart name under the registry path. The renderer
/// (`platform-stack/cue/render_tool.cue`) hardcodes this as the
/// chart name in `Chart.yaml`; both sides MUST agree.
pub const APPRAFTER_PLATFORM_STACK_CHART_NAME: &str = "platform-stack";

/// Default ghcr.io repository for the operator container image.
pub const APPRAFTER_OPERATOR_DEFAULT_IMAGE: &str = "ghcr.io/apprafter/apprafter-operator";

/// Default ghcr.io repository for the admission-webhook container image.
pub const APPRAFTER_ADMISSION_WEBHOOK_DEFAULT_IMAGE: &str =
    "ghcr.io/apprafter/apprafter-admission-webhook";

/// Compose a fully-qualified `repo:tag` reference from the
/// manifest-level `image` / `tag` overrides and the spec defaults.
pub fn resolve_image_ref(
    image: Option<&str>,
    tag: Option<&str>,
    default_image: &str,
    default_tag: &str,
) -> String {
    let image = image.unwrap_or(default_image);
    if image.contains(':') {
        image.to_string()
    } else {
        let tag = tag.unwrap_or(default_tag);
        format!("{image}:{tag}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_compose_repo_and_tag() {
        let got = resolve_image_ref(
            None,
            None,
            APPRAFTER_OPERATOR_DEFAULT_IMAGE,
            RELEASED_OPERATOR_VERSION,
        );
        assert_eq!(
            got,
            format!("{APPRAFTER_OPERATOR_DEFAULT_IMAGE}:{RELEASED_OPERATOR_VERSION}"),
        );
    }

    #[test]
    fn tag_override_alone_keeps_default_registry() {
        let got = resolve_image_ref(
            None,
            Some("v0.1.65"),
            APPRAFTER_OPERATOR_DEFAULT_IMAGE,
            RELEASED_OPERATOR_VERSION,
        );
        assert_eq!(got, format!("{APPRAFTER_OPERATOR_DEFAULT_IMAGE}:v0.1.65"));
    }

    #[test]
    fn image_override_alone_keeps_default_tag() {
        let got = resolve_image_ref(
            Some("ghcr.io/my-fork/apprafter-operator"),
            None,
            APPRAFTER_OPERATOR_DEFAULT_IMAGE,
            RELEASED_OPERATOR_VERSION,
        );
        assert_eq!(
            got,
            format!("ghcr.io/my-fork/apprafter-operator:{RELEASED_OPERATOR_VERSION}"),
        );
    }

    #[test]
    fn full_ref_in_image_field_ignores_tag_field() {
        let got = resolve_image_ref(
            Some("ghcr.io/my-fork/apprafter-operator:dev"),
            Some("ignored"),
            APPRAFTER_OPERATOR_DEFAULT_IMAGE,
            RELEASED_OPERATOR_VERSION,
        );
        assert_eq!(got, "ghcr.io/my-fork/apprafter-operator:dev");
    }

    #[test]
    fn image_field_without_colon_combines_with_tag_field() {
        let got = resolve_image_ref(
            Some("ghcr.io/my-fork/apprafter-operator"),
            Some("dev"),
            APPRAFTER_OPERATOR_DEFAULT_IMAGE,
            RELEASED_OPERATOR_VERSION,
        );
        assert_eq!(got, "ghcr.io/my-fork/apprafter-operator:dev");
    }

    #[test]
    fn webhook_defaults_reuse_released_operator_version() {
        let got = resolve_image_ref(
            None,
            None,
            APPRAFTER_ADMISSION_WEBHOOK_DEFAULT_IMAGE,
            RELEASED_OPERATOR_VERSION,
        );
        assert_eq!(
            got,
            format!("{APPRAFTER_ADMISSION_WEBHOOK_DEFAULT_IMAGE}:{RELEASED_OPERATOR_VERSION}"),
        );
    }
}
