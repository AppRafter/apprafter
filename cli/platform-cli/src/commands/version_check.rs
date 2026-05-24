// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! npm-style "newer CLI available" check. Track B.1.79.
//!
//! Runs at the start of every `apprafter` invocation. Caches
//! the latest-release lookup in `~/.cache/apprafter/version-
//! check.json` with a 6-hour TTL — busy operators don't pay
//! for a GitHub API round-trip on every shell command. The
//! warning prints once per shell session (cache hit on
//! subsequent calls).
//!
//! Walk-fix #4 post-B.1.79a (v0.1.151): the `/releases/latest`
//! endpoint returns the most recently CREATED release across
//! every tag series the repo ships, not just the canonical CLI
//! tag stream. The monorepo also publishes `platform-stack/v*`,
//! `landing-v*`, `argocd-cue-cmp/v*` releases — these often
//! land out of step with CLI tags and were silently
//! short-circuiting the banner. Fix: GET `/releases?per_page=30`
//! and walk newest-first looking for the first tag whose
//! stripped form parses as a canonical semver
//! (`X.Y.Z[-prerelease]`). All four prefix-tagged series fail
//! `Version::parse` and get skipped.
//!
//! TTL also dropped from 24h → 6h: the project is in active
//! daily development, so a 6h dial keeps operators current
//! without paying the GitHub API on every command.
//!
//! Failures (network down, GitHub rate-limited, malformed
//! response, parse error) are swallowed silently — version
//! check is a courtesy banner, not an operational
//! prerequisite. Logging at `debug` level surfaces what went
//! wrong via `RUST_LOG=apprafter=debug`.

use std::fs;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tracing::debug;

const RELEASES_URL: &str = "https://api.github.com/repos/apprafter/apprafter/releases?per_page=30";
const CACHE_TTL: Duration = Duration::from_secs(6 * 60 * 60);
const HTTP_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Debug, Serialize, Deserialize)]
struct CachedCheck {
    latest_tag: String,
    fetched_at_secs: u64,
}

/// Check for a newer published CLI version and print a warning
/// line to stderr if one exists. Best-effort — failures
/// swallowed silently (logged at debug level).
pub fn maybe_warn_about_newer_version() {
    let current = env!("CARGO_PKG_VERSION");
    if let Some(latest) = resolve_latest_tag() {
        if newer_than(&latest, current) {
            eprintln!(
                "apprafter {latest} is available; you're on {current}. \
                 Upgrade: https://github.com/apprafter/apprafter/releases/latest"
            );
        }
    }
}

fn resolve_latest_tag() -> Option<String> {
    let cache_path = cache_path();
    if let Some(cached) = read_fresh_cache(&cache_path) {
        return Some(cached.latest_tag);
    }

    let fetched = match fetch_latest_tag() {
        Ok(tag) => tag,
        Err(e) => {
            debug!(error = %e, "version check fetch failed");
            return None;
        }
    };

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let _ = write_cache(
        &cache_path,
        &CachedCheck {
            latest_tag: fetched.clone(),
            fetched_at_secs: now,
        },
    );

    Some(fetched)
}

fn fetch_latest_tag() -> Result<String, String> {
    let agent = ureq::AgentBuilder::new().timeout(HTTP_TIMEOUT).build();
    let body: serde_json::Value = agent
        .get(RELEASES_URL)
        .set("User-Agent", "apprafter-cli")
        .set("Accept", "application/vnd.github+json")
        .call()
        .map_err(|e| format!("HTTP: {e}"))?
        .into_json()
        .map_err(|e| format!("JSON: {e}"))?;
    pick_canonical_cli_tag(&body)
        .ok_or_else(|| "no canonical CLI release tag in the most recent 30 releases".into())
}

/// Pull the first canonical CLI tag from a `/releases` array.
/// GitHub returns releases newest-first, so walking in order
/// yields the latest CLI release.
///
/// "Canonical CLI tag" = `tag_name` whose stripped form
/// (leading `v` removed) parses as a `semver::Version`. The
/// monorepo's other tag series (`platform-stack/v*`,
/// `landing-v*`, `argocd-cue-cmp/v*`) all fail this filter
/// because the prefix isn't strippable by `trim_start_matches`
/// and the resulting string isn't valid semver.
///
/// Pure fn — tests exercise every prefix permutation without
/// needing the network.
pub(crate) fn pick_canonical_cli_tag(releases: &serde_json::Value) -> Option<String> {
    let arr = releases.as_array()?;
    for release in arr {
        let tag = release
            .get("tag_name")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("");
        if let Some(stripped) = canonicalise_cli_tag(tag) {
            return Some(stripped);
        }
    }
    None
}

/// Strip a leading `v` from a tag and validate it as semver.
/// Returns `Some(stripped)` only for canonical CLI tags
/// (`v0.1.150` → `0.1.150`); returns `None` for prefixed tags
/// (`platform-stack/v0.1.38`, `landing-v0.1.0`,
/// `argocd-cue-cmp/v0.1.1`) and for non-semver garbage.
fn canonicalise_cli_tag(tag: &str) -> Option<String> {
    if tag.is_empty() {
        return None;
    }
    let stripped = tag.trim_start_matches('v').to_string();
    if semver::Version::parse(&stripped).is_ok() {
        Some(stripped)
    } else {
        None
    }
}

fn cache_path() -> PathBuf {
    let cache_dir = dirs::cache_dir().unwrap_or_else(|| PathBuf::from("."));
    cache_dir.join("apprafter").join("version-check.json")
}

fn read_fresh_cache(path: &std::path::Path) -> Option<CachedCheck> {
    let raw = fs::read_to_string(path).ok()?;
    let parsed: CachedCheck = serde_json::from_str(&raw).ok()?;
    // Defensive: a cached `latest_tag` that doesn't parse as
    // semver is leftover garbage from a prior version (e.g.
    // pre-walk-fix-#4 cache holding `platform-stack/v0.1.38`).
    // Treat it as a cache miss so the next fetch overwrites
    // the bad value with a canonical CLI tag.
    if semver::Version::parse(parsed.latest_tag.trim_start_matches('v')).is_err() {
        debug!(
            cached = %parsed.latest_tag,
            "ignoring non-semver cached tag — forcing fresh fetch"
        );
        return None;
    }
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    if now.saturating_sub(parsed.fetched_at_secs) > CACHE_TTL.as_secs() {
        return None;
    }
    Some(parsed)
}

fn write_cache(path: &std::path::Path, entry: &CachedCheck) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let body = serde_json::to_string(entry).map_err(std::io::Error::other)?;
    fs::write(path, body)
}

/// Semver-aware "is `candidate` > `current`?" comparison.
/// Tolerates `v` prefix on either side. Falls back to false
/// (no warning) on unparseable input — fail-quiet.
fn newer_than(candidate: &str, current: &str) -> bool {
    use semver::Version;
    let cand = Version::parse(candidate.trim_start_matches('v')).ok();
    let cur = Version::parse(current.trim_start_matches('v')).ok();
    matches!((cand, cur), (Some(c), Some(s)) if c > s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn newer_than_strips_v_prefix() {
        assert!(newer_than("v0.1.135", "0.1.134"));
        assert!(newer_than("0.1.135", "v0.1.134"));
    }

    #[test]
    fn newer_than_returns_false_for_equal() {
        assert!(!newer_than("0.1.134", "0.1.134"));
    }

    #[test]
    fn newer_than_returns_false_for_older() {
        assert!(!newer_than("0.1.130", "0.1.134"));
    }

    #[test]
    fn newer_than_returns_false_for_garbage() {
        // Fail-quiet — don't warn the operator on a stale
        // cache OR malformed GitHub response.
        assert!(!newer_than("not-a-version", "0.1.134"));
        assert!(!newer_than("0.1.134", "garbage"));
    }

    #[test]
    fn canonicalise_cli_tag_strips_v_prefix_for_canonical_semver() {
        assert_eq!(
            canonicalise_cli_tag("v0.1.150"),
            Some("0.1.150".to_string())
        );
        assert_eq!(canonicalise_cli_tag("v1.0.0"), Some("1.0.0".to_string()));
        // Pre-release suffix is valid semver and a legitimate
        // CLI release shape.
        assert_eq!(
            canonicalise_cli_tag("v0.2.0-mvp"),
            Some("0.2.0-mvp".to_string())
        );
    }

    #[test]
    fn canonicalise_cli_tag_rejects_platform_stack_prefix() {
        // Load-bearing: this is the exact case that broke the
        // banner on the v0.1.149 walk — `releases/latest`
        // returned `platform-stack/v0.1.38` (chronologically
        // latest), and the trim_start_matches('v') strip
        // didn't touch it, so semver::parse failed silently
        // and the banner never showed.
        assert_eq!(canonicalise_cli_tag("platform-stack/v0.1.38"), None);
    }

    #[test]
    fn canonicalise_cli_tag_rejects_landing_prefix() {
        // `landing-v0.1.0` doesn't start with `v` so the
        // strip is a no-op; the whole string fails semver
        // parse.
        assert_eq!(canonicalise_cli_tag("landing-v0.1.0"), None);
    }

    #[test]
    fn canonicalise_cli_tag_rejects_argocd_cue_cmp_prefix() {
        assert_eq!(canonicalise_cli_tag("argocd-cue-cmp/v0.1.1"), None);
    }

    #[test]
    fn canonicalise_cli_tag_rejects_empty_and_garbage() {
        assert_eq!(canonicalise_cli_tag(""), None);
        assert_eq!(canonicalise_cli_tag("not-a-version"), None);
        // Defensive: a stripped tag that's still partial
        // semver (no patch component) is rejected.
        assert_eq!(canonicalise_cli_tag("v0.1"), None);
    }

    #[test]
    fn pick_canonical_cli_tag_skips_prefixed_tags() {
        // Real-world walk-fix #4 scenario: the most recent
        // releases are a mix of platform-stack, landing, and
        // CLI tags. The CLI tag must surface even when it's
        // not literally the newest entry in the list.
        let releases = json!([
            { "tag_name": "platform-stack/v0.1.41" },
            { "tag_name": "landing-v0.1.2" },
            { "tag_name": "v0.1.150" },
            { "tag_name": "v0.1.149" },
            { "tag_name": "argocd-cue-cmp/v0.1.1" }
        ]);
        assert_eq!(
            pick_canonical_cli_tag(&releases),
            Some("0.1.150".to_string())
        );
    }

    #[test]
    fn pick_canonical_cli_tag_returns_none_when_no_canonical_in_window() {
        // Pathological case: every recent release is a
        // platform-stack or landing tag (CLI hasn't shipped
        // anything new in а while). Resolver returns None,
        // banner stays silent — same as before fix, no
        // surprise.
        let releases = json!([
            { "tag_name": "platform-stack/v0.1.41" },
            { "tag_name": "landing-v0.1.2" }
        ]);
        assert_eq!(pick_canonical_cli_tag(&releases), None);
    }

    #[test]
    fn pick_canonical_cli_tag_handles_malformed_release_objects() {
        // Defensive: some releases may omit `tag_name`
        // entirely (corrupt CRs, partial publishes); the
        // walker just skips them and continues.
        let releases = json!([
            { "name": "no tag_name field" },
            { "tag_name": null },
            { "tag_name": "" },
            { "tag_name": "v0.1.150" }
        ]);
        assert_eq!(
            pick_canonical_cli_tag(&releases),
            Some("0.1.150".to_string())
        );
    }

    #[test]
    fn pick_canonical_cli_tag_returns_none_for_non_array() {
        // Pre-fix-#4 endpoint returned а single object
        // (the latest release). Defensive: the new endpoint
        // returns an array, but if а future schema change
        // breaks that contract the resolver fails quiet.
        let single_object = json!({ "tag_name": "v0.1.150" });
        assert_eq!(pick_canonical_cli_tag(&single_object), None);
    }
}
