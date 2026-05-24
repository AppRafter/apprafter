// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Resolve the latest published platform-stack chart version
//! from the upstream GitHub Releases stream at runtime.
//!
//! Walk-fix #9 post-B.1.79a (v0.1.159).
//!
//! ## Why
//!
//! Up through v0.1.158 the loader (`apprafter cluster-bootstrap`)
//! always pinned the root `Application platform` to the baked-in
//! `RELEASED_PLATFORM_STACK_VERSION` constant. The constant
//! tracks `platform.cue` `currentVersion` via `cli-providers/
//! build.rs` at compile time. Result: every chart-only bump
//! forced a CLI rebuild + release to keep fresh installs
//! current, otherwise new operators would bootstrap onto the
//! previous chart version and only reach the latest after a
//! reconcile cycle.
//!
//! New flow: the loader fetches the latest `platform-stack/v*`
//! tag from the upstream GitHub Releases API at bootstrap time
//! and uses that for the initial pin. On network failure / parse
//! failure / no matching tags, falls back to the baked
//! constant — so air-gapped and firewalled installs still work
//! against the version the CLI binary itself shipped against.
//!
//! ## Channel semantics
//!
//! "Latest" here means "highest semver among `platform-stack/v*`
//! tags on the GitHub repo". The platform-stack chart's
//! compatibility metadata also carries a `channel` field
//! (`stable` / `edge`), but we don't filter by channel at the
//! CLI loader level — the assumption is that the upstream
//! tag stream IS the stable channel by construction.
//! Edge-channel publishes (if they ever ship) would need to
//! carry a `-edge.N` prerelease suffix on the semver, which
//! `Version::parse` distinguishes from stable releases via
//! the `.pre` field.

use std::time::Duration;

use semver::Version;
use serde_json::Value;
use tracing::debug;

use crate::k8s::loader_values::RELEASED_PLATFORM_STACK_VERSION;

const RELEASES_URL: &str = "https://api.github.com/repos/apprafter/apprafter/releases?per_page=100";
const HTTP_TIMEOUT: Duration = Duration::from_secs(3);
const TAG_PREFIX: &str = "platform-stack/v";

/// Resolve the chart version the loader should pin the root
/// Application to. Best-effort:
///
/// 1. GET `/releases?per_page=100` (same surface as
///    `cli/platform-cli/src/commands/version_check.rs`).
/// 2. Filter `tag_name` to entries starting `platform-stack/v`.
/// 3. Strip the prefix, parse as semver.
/// 4. Pick the highest; reject prereleases unless every
///    candidate is a prerelease.
/// 5. Return as string for the SSA-apply YAML body.
///
/// Any failure path returns the baked-in
/// `RELEASED_PLATFORM_STACK_VERSION` so air-gapped /
/// firewalled installs still bootstrap with a known-good
/// version.
pub fn resolve_latest_platform_stack_version() -> String {
    match fetch_latest() {
        Ok(v) => {
            debug!(version = %v, "resolved platform-stack version from upstream");
            v
        }
        Err(e) => {
            debug!(
                error = %e,
                fallback = %RELEASED_PLATFORM_STACK_VERSION,
                "channel-latest fetch failed; using baked-in fallback"
            );
            RELEASED_PLATFORM_STACK_VERSION.to_string()
        }
    }
}

fn fetch_latest() -> Result<String, String> {
    let agent = ureq::AgentBuilder::new().timeout(HTTP_TIMEOUT).build();
    let body: Value = agent
        .get(RELEASES_URL)
        .set("User-Agent", "apprafter-cli")
        .set("Accept", "application/vnd.github+json")
        .call()
        .map_err(|e| format!("HTTP: {e}"))?
        .into_json()
        .map_err(|e| format!("JSON: {e}"))?;
    pick_latest_platform_stack_tag(&body)
        .ok_or_else(|| "no platform-stack/v* releases found".to_string())
}

/// Pure helper: scan a `/releases` array, filter to
/// `platform-stack/v*` tags, return the highest-semver value
/// (or `None` if none match). Splits out from `fetch_latest`
/// so tests can drive every branch with fixture JSON.
///
/// Prefers stable releases over prereleases: when both
/// `0.1.45` and `0.1.46-rc.1` are present, returns `0.1.45`.
/// Only if every candidate is a prerelease does it return one.
pub fn pick_latest_platform_stack_tag(releases: &Value) -> Option<String> {
    let arr = releases.as_array()?;
    let candidates: Vec<Version> = arr
        .iter()
        .filter_map(|r| r.get("tag_name").and_then(Value::as_str))
        .filter_map(|tag| tag.strip_prefix(TAG_PREFIX))
        .filter_map(|v| Version::parse(v).ok())
        .collect();

    if candidates.is_empty() {
        return None;
    }

    // Stable preferred: filter out prereleases first. If
    // nothing left (all candidates were prereleases — common
    // on a fresh channel before the first stable lands),
    // fall back to the prerelease pool.
    let stable: Vec<&Version> = candidates.iter().filter(|v| v.pre.is_empty()).collect();
    let pool: Vec<&Version> = if !stable.is_empty() {
        stable
    } else {
        candidates.iter().collect()
    };

    pool.into_iter().max().map(|v| v.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn picks_highest_semver_from_mixed_streams() {
        // The releases list carries every tag stream
        // simultaneously — canonical CLI tags `v0.1.x`,
        // chart tags `platform-stack/v0.1.x`, cue-cmp tags
        // `argocd-cue-cmp/v0.1.x`, landing tags
        // `landing-v0.1.x`. Only platform-stack/v* should
        // match.
        let releases = json!([
            { "tag_name": "v0.1.158" },
            { "tag_name": "platform-stack/v0.1.43" },
            { "tag_name": "platform-stack/v0.1.45" },
            { "tag_name": "platform-stack/v0.1.44" },
            { "tag_name": "argocd-cue-cmp/v0.1.4" },
            { "tag_name": "landing-v0.1.2" },
        ]);
        assert_eq!(
            pick_latest_platform_stack_tag(&releases),
            Some("0.1.45".to_string())
        );
    }

    #[test]
    fn returns_none_when_no_platform_stack_tags() {
        // Operator might fork the repo without re-tagging
        // any platform-stack releases (or be running before
        // the first publish). Resolver returns None →
        // caller falls back to baked constant.
        let releases = json!([
            { "tag_name": "v0.1.158" },
            { "tag_name": "argocd-cue-cmp/v0.1.4" },
        ]);
        assert_eq!(pick_latest_platform_stack_tag(&releases), None);
    }

    #[test]
    fn ignores_malformed_release_entries() {
        // Defensive: missing or non-string `tag_name`
        // entries, unparseable semver, and empty tag values
        // all get skipped. The good entry still wins.
        let releases = json!([
            { "name": "no tag_name field" },
            { "tag_name": null },
            { "tag_name": "platform-stack/vGARBAGE" },
            { "tag_name": "platform-stack/" },
            { "tag_name": "platform-stack/v0.1.45" },
        ]);
        assert_eq!(
            pick_latest_platform_stack_tag(&releases),
            Some("0.1.45".to_string())
        );
    }

    #[test]
    fn returns_none_for_non_array() {
        // Pre-fix release_url shape returned a single
        // object (`/releases/latest`). New endpoint
        // returns an array; defensive coverage for a
        // future schema change.
        let single = json!({ "tag_name": "platform-stack/v0.1.45" });
        assert_eq!(pick_latest_platform_stack_tag(&single), None);
    }

    #[test]
    fn prefers_stable_over_prerelease_when_both_present() {
        // Edge-channel releases would carry a prerelease
        // suffix (`-rc.1`, `-edge.7`). Loader on stable
        // path should ignore them when a stable release
        // is also published.
        let releases = json!([
            { "tag_name": "platform-stack/v0.1.45" },
            { "tag_name": "platform-stack/v0.1.46-rc.1" },
            { "tag_name": "platform-stack/v0.1.46-rc.2" },
        ]);
        assert_eq!(
            pick_latest_platform_stack_tag(&releases),
            Some("0.1.45".to_string())
        );
    }

    #[test]
    fn falls_back_to_prerelease_when_no_stable_exists() {
        // Fresh chart channel before its first stable cut —
        // we still want to surface SOMETHING for bootstrap
        // testing rather than returning None.
        let releases = json!([
            { "tag_name": "platform-stack/v0.1.46-rc.2" },
            { "tag_name": "platform-stack/v0.1.46-rc.1" },
        ]);
        assert_eq!(
            pick_latest_platform_stack_tag(&releases),
            Some("0.1.46-rc.2".to_string())
        );
    }

    #[test]
    fn handles_two_digit_patch_versions_correctly() {
        // Regression guard: `0.1.10` > `0.1.9` semver-wise
        // but `<` lexically. Make sure we use semver
        // comparison, not string comparison.
        let releases = json!([
            { "tag_name": "platform-stack/v0.1.9" },
            { "tag_name": "platform-stack/v0.1.10" },
            { "tag_name": "platform-stack/v0.1.8" },
        ]);
        assert_eq!(
            pick_latest_platform_stack_tag(&releases),
            Some("0.1.10".to_string())
        );
    }
}
