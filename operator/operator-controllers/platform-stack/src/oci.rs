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

/// Fetch all tags from `upstream_url` and return the latest
/// semver in the requested channel. `upstream_url` examples:
/// `oci://ghcr.io/apprafter/platform-stack` or
/// `ghcr.io/apprafter/platform-stack`.
pub async fn latest_in_channel(
    upstream_url: &str,
    channel: Channel,
) -> Result<Version, OciError> {
    let bare = strip_oci_scheme(upstream_url);
    let reference: Reference = bare
        .parse()
        .map_err(|e: oci_distribution::ParseError| {
            OciError::InvalidReference(bare.to_string(), e.to_string())
        })?;

    let client = Client::new(ClientConfig::default());
    let tag_response = client
        .list_tags(&reference, &RegistryAuth::Anonymous, None, None)
        .await
        .map_err(|e| OciError::Registry(e.to_string()))?;

    let mut versions: Vec<Version> = tag_response
        .tags
        .into_iter()
        .filter_map(|t| Version::parse(t.trim_start_matches('v')).ok())
        .filter(|v| channel_matches(v, channel))
        .collect();
    versions.sort();
    versions
        .pop()
        .ok_or_else(|| OciError::NoVersions(upstream_url.to_string()))
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
}
