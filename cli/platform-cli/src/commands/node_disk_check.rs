// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! "Your node is nearly out of disk" banner. 2.22d / D8.
//!
//! Runs at the start of every `apprafter` invocation, on the model of
//! [`super::version_check`], because the thing it warns about is not
//! specific to any one command: the node's root filesystem carries every
//! local-path volume, the database's data directory, snapshot files, the
//! container image store and the logs. When it fills, everything on that
//! node stops writing at once, and the operator finds out from whichever
//! symptom happens to surface first.
//!
//! # Why a banner and not a status line
//!
//! The signal already existed — the provisioner computed it and stamped a
//! `CapacityWarning` on the `SharedVolume` being reconciled. Two problems
//! with that, both recorded as D8. It was only computed while reconciling a
//! `SharedVolume`, so a cluster with none was never warned at all. And no
//! CLI surface read it, so even where it fired it reached nobody.
//!
//! A node running out of disk is not something you go and look for. It is
//! something that should interrupt whatever you are doing, which is exactly
//! what the version banner's shape is for.
//!
//! # Cost
//!
//! One `kubectl get platformstack` behind a five-minute file cache, so a
//! busy operator does not pay a cluster round-trip per shell command. Every
//! failure — no target, no kubeconfig, no cluster, no such CRD, a parse
//! error — is swallowed and negatively cached for the same window, so an
//! offline or half-configured machine costs one attempt per five minutes
//! rather than one per command.

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tracing::debug;

const CACHE_TTL: Duration = Duration::from_secs(5 * 60);

#[derive(Debug, Serialize, Deserialize)]
struct CachedPressure {
    /// The condition message, or `None` when the cluster reported no
    /// pressure OR could not be reached. The two are deliberately the same
    /// on this path: a banner that said "could not check your disk" on
    /// every command from a laptop with no cluster would be noise, and the
    /// place that must distinguish them is `apprafter platform status`,
    /// which does.
    message: Option<String>,
    fetched_at_secs: u64,
}

/// Print a warning when the cluster reports `NodeDiskPressure=True`.
///
/// Best-effort throughout: this must never delay or fail a command. It is
/// the same contract the sampler in the operator holds — a decorative read
/// that fails is silence, not an error.
pub fn maybe_warn_about_node_disk() {
    if let Some(message) = resolve_pressure() {
        eprintln!(
            "{}",
            cli_core::style::warn(&format!("Node disk: {message}"))
        );
    }
}

fn resolve_pressure() -> Option<String> {
    let path = cache_path();
    resolve_pressure_with(
        || read_fresh_cache(&path),
        probe_cluster,
        |entry| {
            let _ = write_cache(&cache_path(), entry);
        },
        now_secs,
    )
}

/// Seconds since the epoch, or 0 when the clock predates it.
fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// The cache-or-probe decision, with every impure part injected.
///
/// # Why this seam exists — the same reason as `version_check`, one file later
///
/// This function read a five-minute disk cache, shelled out to a real
/// `kubectl`, and stamped `SystemTime::now()` inline. That made the measured
/// coverage of this file depend on whether a previous run had warmed the cache:
/// two consecutive workspace measurements reported 89.89% and 66.85% for it,
/// with nothing changed in between, because the second run started inside the
/// TTL and never executed the probe or the write at all.
///
/// `version_check.rs` had a byte-for-byte identical defect and was fixed first.
/// This one was left standing, which is worth recording: the lesson was
/// learned as a fact about a FILE rather than as a rule about a SHAPE — a
/// decision welded to a disk cache, a subprocess and a clock — and the sibling
/// went unnoticed until an independent measurement disagreed with itself.
///
/// The ordering is asserted, not assumed: a fresh cache must short-circuit
/// BEFORE the probe, or every command pays a `kubectl` round trip.
fn resolve_pressure_with(
    read_cache: impl FnOnce() -> Option<CachedPressure>,
    probe: impl FnOnce() -> Option<String>,
    write: impl FnOnce(&CachedPressure),
    now: impl FnOnce() -> u64,
) -> Option<String> {
    if let Some(cached) = read_cache() {
        return cached.message;
    }
    let message = probe();
    write(&CachedPressure {
        message: message.clone(),
        fetched_at_secs: now(),
    });
    message
}

/// Read `NodeDiskPressure` off the PlatformStack singleton.
///
/// Uses the ambient kubeconfig rather than resolving the CLI's own target
/// store: this runs before any command has decided which target it is
/// operating on, and reaching into the state store here would both cost
/// more and be wrong for a command that carries an explicit `--target`.
fn probe_cluster() -> Option<String> {
    let out = Command::new("kubectl")
        .args([
            "get",
            "platformstack",
            "default",
            "-n",
            "apprafter-system",
            "-o",
            "json",
        ])
        .output()
        .ok()?;
    if !out.status.success() {
        debug!("node-disk check: kubectl get platformstack did not succeed");
        return None;
    }
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
    pressure_message(&v)
}

/// The `NodeDiskPressure` message when the condition is `True`.
///
/// Pure, so the shape is tested without a cluster. Returns `None` for any
/// other status — including a missing condition, which is what an older
/// operator or a cluster whose kubelet could not be sampled reports.
/// Silence on "unknown" is deliberate: an unfounded disk warning is the
/// kind of banner people learn to scroll past, and then miss the real one.
pub fn pressure_message(stack: &serde_json::Value) -> Option<String> {
    let conds = stack.pointer("/status/conditions")?.as_array()?;
    let c = conds
        .iter()
        .find(|c| c.get("type").and_then(serde_json::Value::as_str) == Some("NodeDiskPressure"))?;
    if c.get("status").and_then(serde_json::Value::as_str) != Some("True") {
        return None;
    }
    c.get("message")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
}

fn cache_path() -> PathBuf {
    let base = dirs::cache_dir().unwrap_or_else(std::env::temp_dir);
    base.join("apprafter").join("node-disk-check.json")
}

fn read_fresh_cache(path: &PathBuf) -> Option<CachedPressure> {
    let raw = fs::read_to_string(path).ok()?;
    let cached: CachedPressure = serde_json::from_str(&raw).ok()?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    (now.saturating_sub(cached.fetched_at_secs) < CACHE_TTL.as_secs()).then_some(cached)
}

fn write_cache(path: &PathBuf, value: &CachedPressure) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, serde_json::to_string(value).unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    // -----------------------------------------------------------------
    // The cache-or-probe decision, with no kubectl and no clock
    // -----------------------------------------------------------------

    #[test]
    fn a_fresh_cache_short_circuits_before_kubectl_runs() {
        // This hook runs before EVERY command. If a warm cache does not
        // short-circuit, every invocation pays a `kubectl get platformstack`
        // — which is both slow and, on a machine with no cluster, a guaranteed
        // failure on the critical path of commands that need no cluster at all.
        let probed = Cell::new(false);
        let wrote = Cell::new(false);
        let got = super::resolve_pressure_with(
            || {
                Some(super::CachedPressure {
                    message: Some("the node's filesystem is 91% full".into()),
                    fetched_at_secs: 1,
                })
            },
            || {
                probed.set(true);
                None
            },
            |_| wrote.set(true),
            || 42,
        );
        assert_eq!(got.as_deref(), Some("the node's filesystem is 91% full"));
        assert!(!probed.get(), "a fresh cache must not shell out to kubectl");
        assert!(!wrote.get(), "nothing to rewrite when the cache was used");
    }

    #[test]
    fn a_cached_absence_is_still_an_answer() {
        // `message: None` means "checked, and the node is fine". It must be
        // honoured as a cache HIT, not re-probed — otherwise the healthy case,
        // which is the common one, never benefits from the cache at all.
        let probed = Cell::new(false);
        let got = super::resolve_pressure_with(
            || {
                Some(super::CachedPressure {
                    message: None,
                    fetched_at_secs: 1,
                })
            },
            || {
                probed.set(true);
                Some("stale warning".into())
            },
            |_| {},
            || 42,
        );
        assert_eq!(got, None);
        assert!(!probed.get(), "a cached healthy verdict must not re-probe");
    }

    #[test]
    fn a_cache_miss_probes_and_records_the_verdict_with_the_time() {
        let wrote: Cell<Option<(Option<String>, u64)>> = Cell::new(None);
        let got = super::resolve_pressure_with(
            || None,
            || Some("the node's filesystem is 90% full".into()),
            |e| wrote.set(Some((e.message.clone(), e.fetched_at_secs))),
            || 1_700_000_000,
        );
        assert_eq!(got.as_deref(), Some("the node's filesystem is 90% full"));
        assert_eq!(
            wrote.take(),
            Some((
                Some("the node's filesystem is 90% full".to_string()),
                1_700_000_000
            ))
        );
    }

    #[test]
    fn a_healthy_probe_is_cached_too_so_the_next_command_stays_quiet() {
        // Caching only the WARNING would make every command on a healthy
        // cluster pay the probe — the expensive half of the decision, spent on
        // the outcome that has nothing to say.
        let wrote = Cell::new(false);
        let got = super::resolve_pressure_with(|| None, || None, |_| wrote.set(true), || 7);
        assert_eq!(got, None);
        assert!(wrote.get(), "a healthy verdict must be cached as well");
    }

    use super::*;
    use serde_json::json;

    fn stack_with(status: &str, message: &str) -> serde_json::Value {
        json!({ "status": { "conditions": [
            { "type": "Ready", "status": "True" },
            { "type": "NodeDiskPressure", "status": status, "message": message }
        ]}})
    }

    #[test]
    fn it_reports_the_message_when_the_condition_is_true() {
        let m = pressure_message(&stack_with("True", "the node's filesystem is 91% full"));
        assert_eq!(m.as_deref(), Some("the node's filesystem is 91% full"));
    }

    #[test]
    fn a_healthy_node_produces_no_banner() {
        assert!(pressure_message(&stack_with("False", "plenty of room")).is_none());
    }

    #[test]
    fn an_absent_condition_is_silence_not_a_warning() {
        // An older operator, or one whose kubelet could not be sampled,
        // reports no condition at all. Warning on that would be an
        // unfounded banner, and unfounded banners are what teach people to
        // scroll past the founded ones.
        let no_cond = json!({ "status": { "conditions": [{ "type": "Ready", "status": "True" }] }});
        assert!(pressure_message(&no_cond).is_none());
        assert!(pressure_message(&json!({ "status": {} })).is_none());
        assert!(pressure_message(&json!({})).is_none());
    }

    #[test]
    fn a_true_condition_with_no_message_stays_silent() {
        // Printing "Node disk: " with nothing after it is worse than
        // printing nothing: it tells the reader something is wrong and
        // refuses to say what.
        let v = json!({ "status": { "conditions": [
            { "type": "NodeDiskPressure", "status": "True" }
        ]}});
        assert!(pressure_message(&v).is_none());
    }
}
