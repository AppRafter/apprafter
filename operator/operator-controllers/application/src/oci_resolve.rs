// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Resolve an OCI image tag to its current registry digest
//! (`repo@sha256:…`) via a manifest `HEAD` reading
//! `Docker-Content-Digest`. Pure parsers + an injected HTTP seam
//! (`RegistryHttp`) so the bearer-token flow is unit-testable
//! without a live registry — the I/O lives in the controller.

// The parsers + error type are wired into the controller's resolve
// flow in the following 2.4h-a/2.4h-d tasks; the whole module reads as
// dead code until those call sites land.
#![allow(dead_code)]

use oci_distribution::Reference;
use thiserror::Error;

/// Registry host + repository path + reference (tag or digest)
/// split out of an image string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageRef {
    pub host: String,
    pub repository: String,
    pub reference: String,
    pub is_digest: bool,
}

#[derive(Debug, Error)]
pub enum OciResolveError {
    #[error("invalid image reference {0:?}")]
    InvalidReference(String),
    #[error("registry HTTP error: {0}")]
    Http(String),
    #[error("registry returned status {status} for {url}")]
    Status { status: u16, url: String },
    #[error("registry response missing Docker-Content-Digest")]
    NoDigestHeader,
    #[error("could not authenticate to registry: {0}")]
    Auth(String),
}

/// Split an image string into host / repository / reference by
/// delegating to `oci_distribution::Reference`, the same validated
/// parser `platform-stack::oci` uses — so the OCI reference grammar
/// (lowercase repo names, `sha256:` digest format/length, `host:port`
/// vs tag disambiguation, the `library/` Docker-Hub default) is
/// enforced in exactly one place rather than re-derived here (and
/// rather than re-using `pull_secret::image_repo_path`, which is a
/// looser host/tag-split heuristic only meant to strip a tag).
///
/// `host` is `Reference::resolve_registry()`, which maps the
/// `docker.io` alias to the canonical `index.docker.io` manifest
/// endpoint the registry HEAD wants. The reference is a digest when
/// `digest()` is present, else the tag (the crate defaults a missing
/// tag to `latest`). A malformed input — e.g. `repo@sha256:abc` with a
/// too-short digest, or an uppercase repo name — is rejected here.
pub fn parse_image_ref(image: &str) -> Result<ImageRef, OciResolveError> {
    let image = image.trim();
    let reference: Reference = image
        .parse()
        .map_err(|_| OciResolveError::InvalidReference(image.to_string()))?;
    let (text, is_digest) = match (reference.digest(), reference.tag()) {
        (Some(digest), _) => (digest.to_string(), true),
        (None, Some(tag)) => (tag.to_string(), false),
        // `Reference` parsing guarantees one of tag/digest is set
        // (a bare name is defaulted to `:latest`), so this is
        // unreachable; fall back defensively rather than panic.
        (None, None) => ("latest".to_string(), false),
    };
    Ok(ImageRef {
        host: reference.resolve_registry().to_string(),
        repository: reference.repository().to_string(),
        reference: text,
        is_digest,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_image_ref_ghcr_with_tag() {
        let r = parse_image_ref("ghcr.io/acme/web:1.2.3").unwrap();
        assert_eq!(r.host, "ghcr.io");
        assert_eq!(r.repository, "acme/web");
        assert_eq!(r.reference, "1.2.3");
        assert!(!r.is_digest);
    }

    #[test]
    fn parse_image_ref_dockerhub_official_library_default_tag() {
        // No host => Docker Hub; single name => library/<name>; no tag => latest.
        // `resolve_registry()` maps the docker.io alias to its canonical
        // manifest endpoint.
        let r = parse_image_ref("nginx").unwrap();
        assert_eq!(r.host, "index.docker.io");
        assert_eq!(r.repository, "library/nginx");
        assert_eq!(r.reference, "latest");
    }

    #[test]
    fn parse_image_ref_dockerhub_namespaced() {
        let r = parse_image_ref("bitnami/redis:7").unwrap();
        assert_eq!(r.host, "index.docker.io");
        assert_eq!(r.repository, "bitnami/redis");
        assert_eq!(r.reference, "7");
    }

    #[test]
    fn parse_image_ref_already_digest_is_flagged() {
        let digest = format!("sha256:{}", "a".repeat(64));
        let r = parse_image_ref(&format!("ghcr.io/acme/web@{digest}")).unwrap();
        assert!(r.is_digest);
        assert_eq!(r.reference, digest);
    }

    #[test]
    fn parse_image_ref_malformed_digest_is_rejected() {
        // The hand-rolled splitter silently accepted a 3-char digest;
        // delegating to oci_distribution validates the sha256 length.
        assert!(parse_image_ref("repo@sha256:abc").is_err());
    }

    #[test]
    fn parse_image_ref_uppercase_repo_is_rejected() {
        // OCI repository names must be lowercase — the validated parser
        // enforces this where the hand-rolled one did not.
        assert!(parse_image_ref("ghcr.io/Acme/Web:1.0").is_err());
    }

    #[test]
    fn parse_image_ref_localhost_port_is_a_registry_host() {
        let r = parse_image_ref("localhost:5000/team/app:dev").unwrap();
        assert_eq!(r.host, "localhost:5000");
        assert_eq!(r.repository, "team/app");
    }

    #[test]
    fn parse_image_ref_empty_is_error() {
        assert!(parse_image_ref("").is_err());
    }
}
