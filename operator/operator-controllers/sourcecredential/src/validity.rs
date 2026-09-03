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

/// Map a failed registry token exchange to a `Validity`. Only an explicit
/// auth rejection is a credential verdict (`Invalid`); every other
/// failure — 5xx, an unparsable manifest, a transport error — is
/// inconclusive, so restricted egress can never look like a bad PAT.
fn registry_validity_from_error(err: &oci_distribution::errors::OciDistributionError) -> Validity {
    match err {
        oci_distribution::errors::OciDistributionError::AuthenticationFailure(_)
        | oci_distribution::errors::OciDistributionError::UnauthorizedError { .. } => {
            Validity::Invalid
        }
        _ => Validity::Unverified,
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
        Ok(Err(e)) => {
            let verdict = registry_validity_from_error(&e);
            if verdict == Validity::Unverified {
                debug!(%e, %image, "registry probe inconclusive; Unverified");
            }
            verdict
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
        repos.extend(matching_repo_urls(app.data.get("spec"), normalized_prefix));
    }
    repos
}

/// Pure: the `repoURL`s ONE Argo CD `Application` spec contributes as
/// representatives of `normalized_prefix` — the single `source.repoURL`
/// plus every `sources[].repoURL` (Argo's multi-source form), each kept
/// only when it actually falls under the prefix. Split out of
/// [`representative_repos`] so the (fiddly, optional-at-every-level)
/// extraction is testable without a cluster; the caller only does the
/// list.
fn matching_repo_urls(spec: Option<&serde_json::Value>, normalized_prefix: &str) -> Vec<String> {
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
    candidates
        .into_iter()
        .filter(|repo| repo_url_matches_prefix(repo, normalized_prefix))
        .collect()
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
        images.extend(matching_images(&app, host_prefix));
    }
    images
}

/// Pure: the images ONE AppRafter `Application` contributes as
/// representatives of `host_prefix` — `base.image` plus every
/// `environments[*].image` override (a per-environment image can live in a
/// covered registry while the base does not, and vice versa), each kept
/// only when it falls under the prefix. Split out of
/// [`representative_images`] so the traversal is testable without a
/// cluster.
fn matching_images(app: &Application, host_prefix: &str) -> Vec<String> {
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
    candidates
        .into_iter()
        .filter(|img| image_matches_host(img, host_prefix))
        .collect()
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
    (verdict, git_half_message(verdict, probed))
}

/// Pure: the `GitValid` condition message for a verdict. The two
/// `Unverified` messages are NOT interchangeable — they are the operator's
/// only way to tell "nothing to probe yet" (no Argo Application references
/// a covered prefix) from "probed and could not reach the host"
/// (restricted egress), and both are the normal steady state on a
/// network-less cluster.
fn git_half_message(verdict: Validity, probed: usize) -> String {
    match verdict {
        Validity::Valid => format!("git smart-HTTP reachable for {probed} representative repo(s)"),
        Validity::Invalid => "git host rejected the credential (HTTP 401/403)".to_string(),
        Validity::Unverified if probed == 0 => {
            "no Argo Application references a covered repoPrefix yet; coverage gated by presence"
                .to_string()
        }
        Validity::Unverified => {
            "git host unreachable (restricted egress); coverage gated by presence".to_string()
        }
    }
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
    (verdict, registry_half_message(verdict, probed))
}

/// Pure: the `RegistryValid` condition message for a verdict. Same
/// split as [`git_half_message`]: `Unverified` with `probed == 0` means
/// "no Application renders a covered image yet", `Unverified` after a
/// probe means the registry was unreachable.
fn registry_half_message(verdict: Validity, probed: usize) -> String {
    match verdict {
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
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // A `Client` aimed at a closed local port — see its definition for why
    // the rustls provider has to be installed first.
    use crate::tests::unreachable_client;
    use operator_core::{ApplicationBaseSpec, ApplicationEnvOverride, ApplicationSpec};

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

    /// An Argo CD `Application` may declare its repo either as a single
    /// `spec.source` or as a multi-`spec.sources` list; a representative
    /// repo hides in either, so BOTH shapes must be walked. Regression
    /// guard: an extractor that reads only `source` silently reports
    /// `Unverified` forever for every multi-source app.
    #[test]
    fn matching_repo_urls_walks_single_source_and_multi_sources() {
        let spec = serde_json::json!({
            "source": { "repoURL": "https://github.com/acme/landing" },
            "sources": [
                { "repoURL": "https://github.com/acme/api" },
                { "repoURL": "https://github.com/other/thing" },
            ]
        });
        assert_eq!(
            matching_repo_urls(Some(&spec), "https://github.com/acme"),
            vec![
                "https://github.com/acme/landing".to_string(),
                "https://github.com/acme/api".to_string(),
            ]
        );
    }

    /// Every level of the Argo spec is optional and untyped (`DynamicObject`),
    /// so a malformed entry must be SKIPPED, not abort the walk: a
    /// `sources` element with no `repoURL` (or a null/non-string one) sits
    /// happily next to the real representative, and dropping the rest of the
    /// list on hitting it would silently make the half `Unverified`.
    #[test]
    fn matching_repo_urls_skips_malformed_entries_without_aborting_the_walk() {
        assert!(matching_repo_urls(None, "https://github.com/acme").is_empty());
        let no_source = serde_json::json!({ "destination": { "namespace": "x" } });
        assert!(matching_repo_urls(Some(&no_source), "https://github.com/acme").is_empty());
        let junk_then_good = serde_json::json!({
            "source": { "repoURL": 42 },
            "sources": [
                { "path": "." },
                { "repoURL": null },
                { "repoURL": "https://github.com/acme/api" },
            ]
        });
        assert_eq!(
            matching_repo_urls(Some(&junk_then_good), "https://github.com/acme"),
            vec!["https://github.com/acme/api".to_string()]
        );
    }

    /// A per-environment `image` override can point at a covered registry
    /// while `base.image` does not (and vice versa), so both are candidate
    /// representatives. Pins that the environment map is walked and that the
    /// host filter is applied to each candidate independently.
    #[test]
    fn matching_images_walks_base_and_environment_overrides() {
        let app = Application::new(
            "landing",
            ApplicationSpec {
                base: Some(ApplicationBaseSpec {
                    image: Some("docker.io/library/nginx:1".into()),
                    ..ApplicationBaseSpec::default()
                }),
                environments: Some(std::collections::BTreeMap::from([
                    (
                        "prod".to_string(),
                        ApplicationEnvOverride {
                            image: Some("ghcr.io/acme/landing:v2".into()),
                            ..ApplicationEnvOverride::default()
                        },
                    ),
                    (
                        "stage".to_string(),
                        ApplicationEnvOverride::default(), // no image override
                    ),
                ])),
                environment: None,
            },
        );
        // base is on docker.io → not covered; only the prod override is.
        assert_eq!(
            matching_images(&app, "ghcr.io/acme/"),
            vec!["ghcr.io/acme/landing:v2".to_string()]
        );
        // …and the base IS returned when the prefix covers it.
        assert_eq!(
            matching_images(&app, "docker.io/library/"),
            vec!["docker.io/library/nginx:1".to_string()]
        );
    }

    /// An Application whose images all live OUTSIDE the covered host
    /// contributes no representative — probing it would exercise a
    /// credential the SourceCredential does not claim to cover. Same for an
    /// Application with no image at all; both leave the half `Unverified`
    /// (nothing to probe), never `Invalid`.
    #[test]
    fn matching_images_returns_nothing_when_no_image_is_covered() {
        let elsewhere = Application::new(
            "elsewhere",
            ApplicationSpec {
                base: Some(ApplicationBaseSpec {
                    image: Some("docker.io/library/nginx:1".into()),
                    ..ApplicationBaseSpec::default()
                }),
                environments: Some(std::collections::BTreeMap::from([(
                    "prod".to_string(),
                    ApplicationEnvOverride {
                        image: Some("quay.io/acme/api:1".into()),
                        ..ApplicationEnvOverride::default()
                    },
                )])),
                environment: None,
            },
        );
        assert!(matching_images(&elsewhere, "ghcr.io/acme/").is_empty());
        let imageless = Application::new("empty", ApplicationSpec::default());
        assert!(matching_images(&imageless, "ghcr.io/acme/").is_empty());
    }

    /// The two `Unverified` git messages are the operator's only way to tell
    /// "no application references a covered prefix yet" from "probed and the
    /// host was unreachable" — both are normal on a network-less cluster, so
    /// they must not collapse into one string.
    #[test]
    fn git_half_message_distinguishes_nothing_probed_from_unreachable() {
        let nothing = git_half_message(Validity::Unverified, 0);
        let unreachable = git_half_message(Validity::Unverified, 2);
        assert_ne!(nothing, unreachable);
        assert!(nothing.contains("no Argo Application references"));
        assert!(unreachable.contains("unreachable"));
        // The Valid message reports how many representatives were probed.
        assert!(git_half_message(Validity::Valid, 3).contains('3'));
        assert!(git_half_message(Validity::Invalid, 1).contains("401/403"));
    }

    /// Registry half: same `Unverified` split as the git half.
    #[test]
    fn registry_half_message_distinguishes_nothing_probed_from_unreachable() {
        let nothing = registry_half_message(Validity::Unverified, 0);
        let unreachable = registry_half_message(Validity::Unverified, 2);
        assert_ne!(nothing, unreachable);
        assert!(nothing.contains("no Application renders an image"));
        assert!(unreachable.contains("unreachable"));
        assert!(registry_half_message(Validity::Valid, 4).contains('4'));
        assert!(registry_half_message(Validity::Invalid, 1).contains("auth failure"));
    }

    /// Only an explicit auth rejection may condemn the credential. A 5xx, a
    /// malformed manifest or any transport failure is inconclusive — mapping
    /// one of those to `Invalid` would make a blocked egress look like a
    /// revoked PAT, which is exactly what this controller must never do.
    #[test]
    fn registry_error_maps_only_auth_failures_to_invalid() {
        use oci_distribution::errors::OciDistributionError as E;
        assert_eq!(
            registry_validity_from_error(&E::AuthenticationFailure("bad token".into())),
            Validity::Invalid
        );
        assert_eq!(
            registry_validity_from_error(&E::UnauthorizedError {
                url: "https://ghcr.io/v2/acme/landing/tags/list".into()
            }),
            Validity::Invalid
        );
        assert_eq!(
            registry_validity_from_error(&E::ServerError {
                code: 503,
                url: "https://ghcr.io/v2/".into(),
                message: "unavailable".into()
            }),
            Validity::Unverified
        );
        assert_eq!(
            registry_validity_from_error(&E::RegistryNoDigestError),
            Validity::Unverified
        );
        assert_eq!(
            registry_validity_from_error(&E::GenericError(Some("dns failure".into()))),
            Validity::Unverified
        );
    }

    /// THE load-bearing safety property of this module: on a cluster the
    /// controller cannot reach, both halves must come back `Unverified`,
    /// never `Invalid`. `present` coverage is gated on the validity
    /// condition not being `False`, so a network fault reported as
    /// `AuthRejected` would fail every dependent Application on a
    /// restricted-egress cluster. Here the apiserver list itself fails, so
    /// no representative object is ever found and nothing is probed.
    #[tokio::test]
    async fn unreachable_cluster_leaves_both_halves_unverified() {
        let client = unreachable_client();
        let (verdict, message) = probe_git_half(
            &client,
            &["https://github.com/acme".to_string()],
            "git",
            "ghp_x",
        )
        .await;
        assert_eq!(verdict, Validity::Unverified);
        assert!(message.contains("no Argo Application references"));

        let (verdict, message) =
            probe_registry_half(&client, &["ghcr.io/acme/".to_string()], "git", "ghp_x").await;
        assert_eq!(verdict, Validity::Unverified);
        assert!(message.contains("no Application renders an image"));
    }

    /// A half whose coverage list is empty probes nothing at all — the loop
    /// never touches the cluster — and still reports `Unverified` with the
    /// "nothing to probe" message rather than a verdict it did not earn.
    #[tokio::test]
    async fn empty_coverage_probes_nothing_and_stays_unverified() {
        let client = unreachable_client();
        assert_eq!(
            probe_git_half(&client, &[], "git", "ghp_x").await.0,
            Validity::Unverified
        );
        assert_eq!(
            probe_registry_half(&client, &[], "git", "ghp_x").await.0,
            Validity::Unverified
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
