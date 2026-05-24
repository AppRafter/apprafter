// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! npm-style "newer CLI available" check. Track B.1.79.
//!
//! Runs at the start of every `apprafter` invocation. Caches
//! the latest-release lookup in `~/.cache/apprafter/version-
//! check.json` with a 24-hour TTL — busy operators don't pay
//! for a GitHub API round-trip on every shell command. The
//! warning prints once per shell session (cache hit on
//! subsequent calls).
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

const RELEASE_URL: &str = "https://api.github.com/repos/apprafter/apprafter/releases/latest";
const CACHE_TTL: Duration = Duration::from_secs(24 * 60 * 60);
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
        .get(RELEASE_URL)
        .set("User-Agent", "apprafter-cli")
        .set("Accept", "application/vnd.github+json")
        .call()
        .map_err(|e| format!("HTTP: {e}"))?
        .into_json()
        .map_err(|e| format!("JSON: {e}"))?;

    body.get("tag_name")
        .and_then(serde_json::Value::as_str)
        .map(|s| s.trim_start_matches('v').to_string())
        .ok_or_else(|| "missing tag_name".into())
}

fn cache_path() -> PathBuf {
    let cache_dir = dirs::cache_dir().unwrap_or_else(|| PathBuf::from("."));
    cache_dir.join("apprafter").join("version-check.json")
}

fn read_fresh_cache(path: &std::path::Path) -> Option<CachedCheck> {
    let raw = fs::read_to_string(path).ok()?;
    let parsed: CachedCheck = serde_json::from_str(&raw).ok()?;
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
}
