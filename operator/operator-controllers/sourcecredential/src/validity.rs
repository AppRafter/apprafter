// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Live validity probe for a `SourceCredential` (1.79c S5 / ADR 0039).
//!
//! Coverage in a `SourceCredential` is a **prefix** (`github.com/acme/`,
//! `ghcr.io/acme/`) — org-level, not a single repo. A live reachability
//! probe, however, needs a *concrete* object: git smart-HTTP validates a
//! repository, and a registry token exchange only reliably rejects bad
//! credentials when scoped to a concrete image. So the probe finds a
//! **representative** object actually covered by the prefix — a git repo
//! from a matching Argo CD `Application`, an image from a matching
//! AppRafter `Application` — and probes that. Validity therefore means
//! "the credential can serve the applications that depend on it."
//!
//! When no application references a covered prefix yet, there is nothing
//! concrete to probe and the half is reported `Unverified` (`status:
//! Unknown`), exactly like the restricted-egress case — the `present`
//! coverage gate accepts both by design.
//!
//! Mapping is deliberately conservative: a half is `Invalid` only on an
//! explicit auth rejection (HTTP 401/403 on git; an auth failure on the
//! registry token exchange), `Valid` only on a clean success, and
//! `Unverified` for everything ambiguous (404, 5xx, DNS/connect errors,
//! no representative object). It never declares `Invalid` from a network
//! error, so a blocked egress can never look like a bad credential.

use std::time::Duration;

use kube::api::{Api, ListParams};
use kube::core::{DynamicObject, GroupVersionKind};
use kube::discovery::ApiResource;
use kube::Client;
use oci_distribution::client::ClientConfig;
use oci_distribution::secrets::RegistryAuth;
use oci_distribution::{Client as OciClient, Reference};
use operator_core::{Application, REASON_AUTH_REJECTED, REASON_REACHABLE, REASON_UNVERIFIED};
use tracing::debug;

/// Per-probe wall-clock ceiling, so a hung host never stalls reconcile.
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);

/// Outcome of a validity probe. Maps onto a k8s condition `(status,
/// reason)` pair: `Valid → ("True", Reachable)`, `Invalid → ("False",
/// AuthRejected)`, `Unverified → ("Unknown", Unverified)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Validity {
    Valid,
    Invalid,
    Unverified,
}

impl Validity {
    /// `(status, reason)` for the `*Valid` condition.
    pub fn condition_parts(self) -> (&'static str, &'static str) {
        match self {
            Validity::Valid => ("True", REASON_REACHABLE),
            Validity::Invalid => ("False", REASON_AUTH_REJECTED),
            Validity::Unverified => ("Unknown", REASON_UNVERIFIED),
        }
    }
}

/// Aggregate per-representative probe results into one half verdict.
/// Any explicit `Invalid` wins (a single rejected credential must
/// surface); otherwise any `Valid` makes the half `Valid`; if nothing
/// could be concluded, `Unverified`.
pub fn aggregate(results: &[Validity]) -> Validity {
    if results.contains(&Validity::Invalid) {
        Validity::Invalid
    } else if results.contains(&Validity::Valid) {
        Validity::Valid
    } else {
        Validity::Unverified
    }
}

/// Git smart-HTTP discovery URL for a concrete repo URL.
fn git_info_refs_url(repo_url: &str) -> String {
    format!(
        "{}/info/refs?service=git-upload-pack",
        repo_url.trim_end_matches('/')
    )
}

/// Does an Argo `Application` `repoURL` fall under the normalised
/// credential prefix? Path-boundary aware so `…/acme` does not match
/// `…/acme-evil`.
fn repo_url_matches_prefix(repo_url: &str, normalized_prefix: &str) -> bool {
    let repo = repo_url.trim_end_matches('/');
    let prefix = normalized_prefix.trim_end_matches('/');
    repo == prefix || repo.starts_with(&format!("{prefix}/"))
}

/// Does an image reference fall under a registry host prefix
/// (`ghcr.io/acme/`)? Path-boundary aware.
fn image_matches_host(image: &str, host_prefix: &str) -> bool {
    let prefix = host_prefix.trim_end_matches('/');
    image == prefix || image.starts_with(&format!("{prefix}/"))
}

/// Map an HTTP status to a `Validity` (git smart-HTTP). 2xx → Valid,
/// 401/403 → Invalid (auth rejected), anything else → Unverified.
fn git_validity_from_status(code: u16) -> Validity {
    match code {
        200..=299 => Validity::Valid,
        401 | 403 => Validity::Invalid,
        _ => Validity::Unverified,
    }
}

/// Probe one concrete git repo over smart-HTTP with Basic auth. Any
/// transport error degrades to `Unverified` (never `Invalid`).
async fn probe_git(repo_url: &str, username: &str, password: &str) -> Validity {
    let client = match reqwest::Client::builder().timeout(PROBE_TIMEOUT).build() {
        Ok(c) => c,
        Err(e) => {
            debug!(%e, "reqwest client build failed; reporting Unverified");
            return Validity::Unverified;
        }
    };
    let url = git_info_refs_url(repo_url);
    match client
        .get(&url)
        .basic_auth(username, Some(password))
        .send()
        .await
    {
        Ok(resp) => git_validity_from_status(resp.status().as_u16()),
        Err(e) => {
            debug!(%e, %repo_url, "git probe transport error; Unverified");
            Validity::Unverified
        }
    }
}

/// Probe one concrete image via a scoped registry v2 token exchange.
/// `oci-distribution` performs the `/v2/` challenge → token-endpoint
/// exchange with the supplied Basic credentials; an auth failure means
/// the credential is rejected, any other error is inconclusive.
async fn probe_registry(image: &str, username: &str, password: &str) -> Validity {
    let reference: Reference = match image.parse() {
        Ok(r) => r,
        Err(e) => {
            debug!(%e, %image, "unparsable image ref; Unverified");
            return Validity::Unverified;
        }
    };
    let auth = RegistryAuth::Basic(username.to_string(), password.to_string());
    let client = OciClient::new(ClientConfig::default());
    let probe = client.list_tags(&reference, &auth, Some(1), None);
    match tokio::time::timeout(PROBE_TIMEOUT, probe).await {
        Ok(Ok(_)) => Validity::Valid,
        Ok(Err(oci_distribution::errors::OciDistributionError::AuthenticationFailure(_)))
        | Ok(Err(oci_distribution::errors::OciDistributionError::UnauthorizedError { .. })) => {
            Validity::Invalid
        }
        Ok(Err(e)) => {
            debug!(%e, %image, "registry probe inconclusive; Unverified");
            Validity::Unverified
        }
        Err(_) => {
            debug!(%image, "registry probe timed out; Unverified");
            Validity::Unverified
        }
    }
}

/// Collect every Argo CD `Application` `repoURL` (single `source` and
/// multi-`sources`) covered by `normalized_prefix`. Cluster-wide list;
/// the operator's ClusterRole already grants `argoproj.io/applications`
/// read.
async fn representative_repos(client: &Client, normalized_prefix: &str) -> Vec<String> {
    let ar = ApiResource::from_gvk(&GroupVersionKind {
        group: "argoproj.io".into(),
        version: "v1alpha1".into(),
        kind: "Application".into(),
    });
    let api: Api<DynamicObject> = Api::all_with(client.clone(), &ar);
    let Ok(list) = api.list(&ListParams::default()).await else {
        return Vec::new();
    };
    let mut repos = Vec::new();
    for app in list {
        let spec = app.data.get("spec");
        let mut candidates = Vec::new();
        if let Some(u) = spec
            .and_then(|s| s.get("source"))
            .and_then(|s| s.get("repoURL"))
            .and_then(|u| u.as_str())
        {
            candidates.push(u.to_string());
        }
        if let Some(sources) = spec
            .and_then(|s| s.get("sources"))
            .and_then(|s| s.as_array())
        {
            for src in sources {
                if let Some(u) = src.get("repoURL").and_then(|u| u.as_str()) {
                    candidates.push(u.to_string());
                }
            }
        }
        for repo in candidates {
            if repo_url_matches_prefix(&repo, normalized_prefix) {
                repos.push(repo);
            }
        }
    }
    repos
}

/// Collect every AppRafter `Application` image (base + environments)
/// covered by `host_prefix`.
async fn representative_images(client: &Client, host_prefix: &str) -> Vec<String> {
    let api: Api<Application> = Api::all(client.clone());
    let Ok(list) = api.list(&ListParams::default()).await else {
        return Vec::new();
    };
    let mut images = Vec::new();
    for app in list {
        let mut candidates = Vec::new();
        if let Some(base) = &app.spec.base {
            if let Some(img) = &base.image {
                candidates.push(img.clone());
            }
        }
        if let Some(envs) = &app.spec.environments {
            for env in envs.values() {
                if let Some(img) = &env.image {
                    candidates.push(img.clone());
                }
            }
        }
        for img in candidates {
            if image_matches_host(&img, host_prefix) {
                images.push(img);
            }
        }
    }
    images
}

/// Probe the git half: for each normalised prefix, find a representative
/// repo and probe it, then aggregate. `(Validity, message)`.
pub async fn probe_git_half(
    client: &Client,
    normalized_prefixes: &[String],
    username: &str,
    password: &str,
) -> (Validity, String) {
    let mut results = Vec::new();
    let mut probed = 0usize;
    for prefix in normalized_prefixes {
        for repo in representative_repos(client, prefix).await {
            results.push(probe_git(&repo, username, password).await);
            probed += 1;
        }
    }
    let verdict = aggregate(&results);
    let message = match verdict {
        Validity::Valid => format!("git smart-HTTP reachable for {probed} representative repo(s)"),
        Validity::Invalid => "git host rejected the credential (HTTP 401/403)".to_string(),
        Validity::Unverified if probed == 0 => {
            "no Argo Application references a covered repoPrefix yet; coverage gated by presence"
                .to_string()
        }
        Validity::Unverified => {
            "git host unreachable (restricted egress); coverage gated by presence".to_string()
        }
    };
    (verdict, message)
}

/// Probe the registry half: for each host prefix, find a representative
/// image and probe it, then aggregate. `(Validity, message)`.
pub async fn probe_registry_half(
    client: &Client,
    host_prefixes: &[String],
    username: &str,
    password: &str,
) -> (Validity, String) {
    let mut results = Vec::new();
    let mut probed = 0usize;
    for host in host_prefixes {
        for image in representative_images(client, host).await {
            results.push(probe_registry(&image, username, password).await);
            probed += 1;
        }
    }
    let verdict = aggregate(&results);
    let message = match verdict {
        Validity::Valid => {
            format!("registry token exchange succeeded for {probed} representative image(s)")
        }
        Validity::Invalid => "registry rejected the credential (auth failure)".to_string(),
        Validity::Unverified if probed == 0 => {
            "no Application renders an image under a covered host yet; coverage gated by presence"
                .to_string()
        }
        Validity::Unverified => {
            "registry unreachable (restricted egress); coverage gated by presence".to_string()
        }
    };
    (verdict, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn info_refs_url_appends_service_and_trims_slash() {
        assert_eq!(
            git_info_refs_url("https://github.com/acme/landing"),
            "https://github.com/acme/landing/info/refs?service=git-upload-pack"
        );
        assert_eq!(
            git_info_refs_url("https://github.com/acme/landing/"),
            "https://github.com/acme/landing/info/refs?service=git-upload-pack"
        );
    }

    #[test]
    fn repo_url_match_is_path_boundary_aware() {
        let prefix = "https://github.com/acme";
        assert!(repo_url_matches_prefix(
            "https://github.com/acme/landing",
            prefix
        ));
        assert!(repo_url_matches_prefix("https://github.com/acme", prefix));
        assert!(repo_url_matches_prefix("https://github.com/acme/", prefix));
        // boundary: a sibling org sharing the prefix string must NOT match
        assert!(!repo_url_matches_prefix(
            "https://github.com/acme-evil/x",
            prefix
        ));
        assert!(!repo_url_matches_prefix(
            "https://github.com/other/x",
            prefix
        ));
    }

    #[test]
    fn image_match_is_path_boundary_aware() {
        let host = "ghcr.io/acme/";
        assert!(image_matches_host("ghcr.io/acme/landing:latest", host));
        assert!(image_matches_host("ghcr.io/acme", host)); // exact (rare)
        assert!(!image_matches_host("ghcr.io/acme-evil/x:1", host));
        assert!(!image_matches_host("docker.io/acme/x:1", host));
    }

    #[test]
    fn git_status_mapping_is_conservative() {
        assert_eq!(git_validity_from_status(200), Validity::Valid);
        assert_eq!(git_validity_from_status(204), Validity::Valid);
        assert_eq!(git_validity_from_status(401), Validity::Invalid);
        assert_eq!(git_validity_from_status(403), Validity::Invalid);
        // 404 (org prefix, no repo) and 5xx are NOT a credential verdict
        assert_eq!(git_validity_from_status(404), Validity::Unverified);
        assert_eq!(git_validity_from_status(500), Validity::Unverified);
    }

    #[test]
    fn aggregate_prefers_invalid_then_valid_then_unverified() {
        assert_eq!(aggregate(&[]), Validity::Unverified);
        assert_eq!(aggregate(&[Validity::Unverified]), Validity::Unverified);
        assert_eq!(
            aggregate(&[Validity::Valid, Validity::Unverified]),
            Validity::Valid
        );
        assert_eq!(
            aggregate(&[Validity::Valid, Validity::Invalid, Validity::Unverified]),
            Validity::Invalid
        );
    }

    #[test]
    fn condition_parts_map_each_variant() {
        assert_eq!(
            Validity::Valid.condition_parts(),
            ("True", REASON_REACHABLE)
        );
        assert_eq!(
            Validity::Invalid.condition_parts(),
            ("False", REASON_AUTH_REJECTED)
        );
        assert_eq!(
            Validity::Unverified.condition_parts(),
            ("Unknown", REASON_UNVERIFIED)
        );
    }
}
