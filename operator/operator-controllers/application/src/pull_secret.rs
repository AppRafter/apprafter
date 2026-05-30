// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Registry-host matching for the SourceCredential pull-secret attach
//! (Seam A, 1.79c S3 / ADR 0039).
//!
//! Pure logic: given a rendered image reference and the cluster's
//! `SourceCredential`s, decide which one (if any) covers the image's
//! registry, so the Application reconcile can project its derived
//! `dockerconfigjson` into the workload namespace and attach it to the
//! Deployment's `imagePullSecrets`.

use operator_core::SourceCredential;

/// The repository path of an image reference, with any tag or digest
/// stripped: `"ghcr.io/acme/app:v1"` → `"ghcr.io/acme/app"`,
/// `"ghcr.io/acme/app@sha256:…"` → `"ghcr.io/acme/app"`.
pub fn image_repo_path(image: &str) -> String {
    let no_digest = image.split('@').next().unwrap_or(image);
    let last_slash = no_digest.rfind('/');
    let tag_colon = no_digest.rfind(':');
    let cut = match (last_slash, tag_colon) {
        // A ':' after the last '/' is a tag separator; one before it is
        // a registry port (e.g. localhost:5000/app) and is NOT a tag.
        (Some(ls), Some(tc)) if tc > ls => Some(tc),
        (None, Some(tc)) => Some(tc),
        _ => None,
    };
    match cut {
        Some(c) => no_digest[..c].to_string(),
        None => no_digest.to_string(),
    }
}

/// Does a SourceCredential registry-host prefix cover this image repo
/// path? Prefix match after normalising the trailing slash:
/// `"ghcr.io/acme/"` covers `"ghcr.io/acme/app"`.
pub fn host_covers(host_prefix: &str, repo_path: &str) -> bool {
    let prefix = host_prefix.trim_end_matches('/');
    if prefix.is_empty() {
        return false;
    }
    repo_path == prefix || repo_path.starts_with(&format!("{prefix}/"))
}

/// Pick the SourceCredential whose registry covers `image`, if any.
/// Coverage match only (the `present` gate) — the caller separately
/// confirms the derived pull-secret exists before attaching.
pub fn pick_pull_credential<'a>(
    image: &str,
    creds: &'a [SourceCredential],
) -> Option<&'a SourceCredential> {
    let repo_path = image_repo_path(image);
    creds.iter().find(|cred| {
        cred.spec
            .registry
            .as_ref()
            .map(|r| r.hosts.iter().any(|h| host_covers(h, &repo_path)))
            .unwrap_or(false)
    })
}

/// Name of the per-workload pull-secret copy projected into the app
/// namespace (distinct from the canonical `…-dockercfg` in
/// apprafter-system).
pub fn app_pull_secret_name(cred_name: &str) -> String {
    format!("srccred-{cred_name}-pull")
}

#[cfg(test)]
mod tests {
    use super::*;
    use operator_core::{SealedSecretRef, SourceBackend, SourceCredentialSpec, SourceRegistry};

    fn cred(name: &str, hosts: &[&str]) -> SourceCredential {
        SourceCredential::new(
            name,
            SourceCredentialSpec {
                git: None,
                registry: Some(SourceRegistry {
                    backend: SourceBackend {
                        sealed_secret_ref: Some(SealedSecretRef {
                            name: format!("srccred-{name}-material"),
                            namespace: None,
                        }),
                        open_bao_path: None,
                    },
                    hosts: hosts.iter().map(|s| s.to_string()).collect(),
                }),
            },
        )
    }

    #[test]
    fn image_repo_path_strips_tag() {
        assert_eq!(image_repo_path("ghcr.io/acme/app:v1"), "ghcr.io/acme/app");
    }

    #[test]
    fn image_repo_path_strips_digest() {
        assert_eq!(
            image_repo_path("ghcr.io/acme/app@sha256:abc"),
            "ghcr.io/acme/app"
        );
    }

    #[test]
    fn image_repo_path_keeps_registry_port() {
        assert_eq!(image_repo_path("localhost:5000/app"), "localhost:5000/app");
        assert_eq!(
            image_repo_path("localhost:5000/app:v1"),
            "localhost:5000/app"
        );
    }

    #[test]
    fn host_covers_prefix_match() {
        assert!(host_covers("ghcr.io/acme/", "ghcr.io/acme/app"));
        assert!(host_covers("ghcr.io/acme", "ghcr.io/acme/app"));
        assert!(!host_covers("ghcr.io/other/", "ghcr.io/acme/app"));
        // A prefix must align on a path boundary, not mid-segment.
        assert!(!host_covers("ghcr.io/acme", "ghcr.io/acmecorp/app"));
    }

    #[test]
    fn pick_pull_credential_matches_covering_org() {
        let creds = vec![
            cred("other", &["ghcr.io/other/"]),
            cred("acme", &["ghcr.io/acme/"]),
        ];
        let picked = pick_pull_credential("ghcr.io/acme/app:v1", &creds).unwrap();
        assert_eq!(picked.metadata.name.as_deref(), Some("acme"));
    }

    #[test]
    fn pick_pull_credential_none_for_public_image() {
        let creds = vec![cred("acme", &["ghcr.io/acme/"])];
        assert!(pick_pull_credential("nginx:1.27", &creds).is_none());
        assert!(pick_pull_credential("docker.io/library/redis:7", &creds).is_none());
    }

    #[test]
    fn app_pull_secret_name_is_deterministic() {
        assert_eq!(app_pull_secret_name("acme"), "srccred-acme-pull");
    }
}
