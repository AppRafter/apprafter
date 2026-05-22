// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! OCI registry poll: list tags from an upstream chart repo,
//! filter to semver-valid + channel-matching, return the latest.
//!
//! Anonymous reads against ghcr.io's OCI Distribution Spec
//! endpoint. The `oci-distribution` crate handles auth handshake
//! and pagination behind a single `list_tags` call.

use oci_distribution::client::ClientConfig;
use oci_distribution::secrets::RegistryAuth;
use oci_distribution::{Client, Reference};
use semver::Version;
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Channel {
    Stable,
    Beta,
    Edge,
}

impl Channel {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "stable" => Some(Self::Stable),
            "beta" => Some(Self::Beta),
            "edge" => Some(Self::Edge),
            _ => None,
        }
    }
}

#[derive(Debug, Error)]
pub enum OciError {
    #[error("invalid reference {0:?}: {1}")]
    InvalidReference(String, String),
    #[error("registry IO: {0}")]
    Registry(String),
    #[error("no channel-matching versions found at {0:?}")]
    NoVersions(String),
}

/// Strip an `oci://` scheme prefix if present; the
/// `oci-distribution` `Reference` parser does not accept it.
fn strip_oci_scheme(url: &str) -> &str {
    url.strip_prefix("oci://").unwrap_or(url)
}

/// Decide whether a parsed semver belongs in the channel.
/// `stable` = no pre-release identifier. `beta` accepts
/// `-rc.*` and `-beta.*` (and stable). `edge` accepts everything
/// including arbitrary pre-release suffixes.
pub fn channel_matches(version: &Version, channel: Channel) -> bool {
    match channel {
        Channel::Stable => version.pre.is_empty(),
        Channel::Beta => {
            let pre = version.pre.as_str();
            pre.is_empty()
                || pre.starts_with("rc.")
                || pre.starts_with("beta.")
                || pre == "rc"
                || pre == "beta"
        }
        Channel::Edge => true,
    }
}

/// Fetch all tags from `upstream_url` and return every
/// channel-matching semver in descending order (newest first).
///
/// B.1.74a callers consume the full list to apply the yank
/// filter — they pull `compatibility.yaml` from the top tag,
/// then walk this Vec popping yanked entries off the front
/// until a non-yanked candidate is found.
///
/// Returns `Err(OciError::NoVersions)` when no tag in the
/// channel parses to a valid semver. Callers that just want
/// "newest in channel without any yank-awareness" use
/// `latest_in_channel`, which is a thin wrapper picking the
/// first entry.
pub async fn tags_in_channel(
    upstream_url: &str,
    channel: Channel,
) -> Result<Vec<Version>, OciError> {
    let bare = strip_oci_scheme(upstream_url);
    let reference: Reference = bare.parse().map_err(|e: oci_distribution::ParseError| {
        OciError::InvalidReference(bare.to_string(), e.to_string())
    })?;

    let client = Client::new(ClientConfig::default());
    let tag_response = client
        .list_tags(&reference, &RegistryAuth::Anonymous, None, None)
        .await
        .map_err(|e| OciError::Registry(e.to_string()))?;

    let versions = sort_tags_descending(tag_response.tags, channel);
    if versions.is_empty() {
        return Err(OciError::NoVersions(upstream_url.to_string()));
    }
    Ok(versions)
}

/// Pure helper for `tags_in_channel`'s sort/filter logic.
/// Pulled out so unit tests cover the ordering invariant
/// without a network call.
fn sort_tags_descending(tags: Vec<String>, channel: Channel) -> Vec<Version> {
    let mut versions: Vec<Version> = tags
        .into_iter()
        .filter_map(|t| Version::parse(t.trim_start_matches('v')).ok())
        .filter(|v| channel_matches(v, channel))
        .collect();
    versions.sort();
    versions.reverse();
    versions
}

/// Fetch all tags from `upstream_url` and return the latest
/// semver in the requested channel. `upstream_url` examples:
/// `oci://ghcr.io/apprafter/platform-stack` or
/// `ghcr.io/apprafter/platform-stack`.
///
/// This function is yank-unaware — the B.1.74a reconcile loop
/// uses `tags_in_channel` + a compatibility-doc walk instead.
pub async fn latest_in_channel(upstream_url: &str, channel: Channel) -> Result<Version, OciError> {
    let mut versions = tags_in_channel(upstream_url, channel).await?;
    Ok(versions.remove(0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_stable_rejects_prerelease() {
        assert!(channel_matches(&"1.0.0".parse().unwrap(), Channel::Stable));
        assert!(!channel_matches(
            &"1.0.0-rc.1".parse().unwrap(),
            Channel::Stable
        ));
    }

    #[test]
    fn channel_beta_accepts_rc_and_beta_and_stable() {
        assert!(channel_matches(&"1.0.0".parse().unwrap(), Channel::Beta));
        assert!(channel_matches(
            &"1.0.0-rc.2".parse().unwrap(),
            Channel::Beta
        ));
        assert!(channel_matches(
            &"1.0.0-beta.5".parse().unwrap(),
            Channel::Beta
        ));
        assert!(!channel_matches(
            &"1.0.0-alpha.1".parse().unwrap(),
            Channel::Beta
        ));
    }

    #[test]
    fn channel_edge_accepts_everything() {
        assert!(channel_matches(&"1.0.0".parse().unwrap(), Channel::Edge));
        assert!(channel_matches(
            &"1.0.0-rc.2".parse().unwrap(),
            Channel::Edge
        ));
        assert!(channel_matches(
            &"1.0.0-alpha.1".parse().unwrap(),
            Channel::Edge
        ));
        assert!(channel_matches(
            &"1.0.0-experimental.7".parse().unwrap(),
            Channel::Edge
        ));
    }

    #[test]
    fn strip_oci_scheme_removes_prefix() {
        assert_eq!(strip_oci_scheme("oci://ghcr.io/x/y"), "ghcr.io/x/y");
        assert_eq!(strip_oci_scheme("ghcr.io/x/y"), "ghcr.io/x/y");
    }

    #[test]
    fn channel_parse_round_trips_known_values() {
        assert_eq!(Channel::parse("stable"), Some(Channel::Stable));
        assert_eq!(Channel::parse("beta"), Some(Channel::Beta));
        assert_eq!(Channel::parse("edge"), Some(Channel::Edge));
        assert_eq!(Channel::parse("nightly"), None);
    }

    #[test]
    fn sort_tags_descending_orders_newest_first_and_strips_v_prefix() {
        // B.1.74a contract: the yank-aware walk pops the front
        // of the returned Vec, so the ordering MUST be newest
        // first. Mixed `v`-prefixed and bare tags, plus a
        // non-semver tag, plus a tag the channel filter
        // rejects — all flow through one assertion.
        let tags = vec![
            "v0.1.20".to_string(),
            "0.1.19".to_string(),
            "v0.1.21-rc.1".to_string(), // stable channel rejects rc
            "v0.1.22".to_string(),
            "garbage".to_string(),
            "v0.1.18".to_string(),
        ];
        let sorted = sort_tags_descending(tags, Channel::Stable);
        let strs: Vec<String> = sorted.iter().map(|v| v.to_string()).collect();
        assert_eq!(
            strs,
            vec![
                "0.1.22".to_string(),
                "0.1.20".to_string(),
                "0.1.19".to_string(),
                "0.1.18".to_string(),
            ]
        );
    }

    #[test]
    fn sort_tags_descending_returns_empty_when_channel_rejects_all() {
        let tags = vec!["v0.1.0-rc.1".to_string(), "0.1.0-rc.2".to_string()];
        let sorted = sort_tags_descending(tags, Channel::Stable);
        assert!(sorted.is_empty());
    }
}
