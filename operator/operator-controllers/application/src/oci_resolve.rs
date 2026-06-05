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

const DOCKER_HUB_HOST: &str = "registry-1.docker.io";

/// Split an image string into host / repository / reference. A
/// component before the first `/` is the host iff it contains `.` or
/// `:` or equals `localhost` (the Docker reference grammar); else the
/// host is Docker Hub and a single-segment name gets the `library/`
/// prefix. A missing tag defaults to `latest`.
pub fn parse_image_ref(image: &str) -> Result<ImageRef, OciResolveError> {
    let image = image.trim();
    if image.is_empty() {
        return Err(OciResolveError::InvalidReference(image.to_string()));
    }
    let (host, remainder) = match image.split_once('/') {
        Some((first, rest))
            if first.contains('.') || first.contains(':') || first == "localhost" =>
        {
            (first.to_string(), rest.to_string())
        }
        _ => (DOCKER_HUB_HOST.to_string(), image.to_string()),
    };
    // Digest form: repo@sha256:...
    if let Some((repo, digest)) = remainder.split_once('@') {
        return Ok(ImageRef {
            host: host.clone(),
            repository: normalize_repo(&host, repo),
            reference: digest.to_string(),
            is_digest: true,
        });
    }
    // Tag form: split on the LAST ':' that is in the final path segment
    // (so a host:port already consumed above can't be mistaken for a tag).
    let (repo, tag) = match remainder.rsplit_once(':') {
        Some((r, t)) if !t.contains('/') => (r.to_string(), t.to_string()),
        _ => (remainder.clone(), "latest".to_string()),
    };
    Ok(ImageRef {
        host: host.clone(),
        repository: normalize_repo(&host, &repo),
        reference: tag,
        is_digest: false,
    })
}

fn normalize_repo(host: &str, repo: &str) -> String {
    if host == DOCKER_HUB_HOST && !repo.contains('/') {
        format!("library/{repo}")
    } else {
        repo.to_string()
    }
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
        let r = parse_image_ref("nginx").unwrap();
        assert_eq!(r.host, "registry-1.docker.io");
        assert_eq!(r.repository, "library/nginx");
        assert_eq!(r.reference, "latest");
    }

    #[test]
    fn parse_image_ref_dockerhub_namespaced() {
        let r = parse_image_ref("bitnami/redis:7").unwrap();
        assert_eq!(r.host, "registry-1.docker.io");
        assert_eq!(r.repository, "bitnami/redis");
        assert_eq!(r.reference, "7");
    }

    #[test]
    fn parse_image_ref_already_digest_is_flagged() {
        let r = parse_image_ref("ghcr.io/acme/web@sha256:abc").unwrap();
        assert!(r.is_digest);
        assert_eq!(r.reference, "sha256:abc");
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
