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
