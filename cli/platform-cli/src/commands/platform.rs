// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! `apprafter platform …` thin wrappers. Track B.1.79.
//!
//! `status` reads `PlatformStack/default` from the cluster and
//! prints a human-readable summary (current/target/available
//! versions, conditions, recent history). `upgrade --to <v>`
//! patches `spec.pin`. Both shell out to `kubectl` rather than
//! pulling in kube-rs's Tokio runtime for the synchronous CLI
//! binary.

use chrono::{DateTime, Utc};
use cli_core::{CliError, Result};
use serde_json::Value;
use tabled::settings::{object::Columns, Modify, Width};
use tabled::{Table, Tabled};

use crate::commands::k8s_helpers::{
    ensure_kubeconfig_tempfile, kubectl_get_json, kubectl_merge_patch,
};

const PLATFORMSTACK_NAME: &str = "default";
const PLATFORMSTACK_NAMESPACE: &str = "apprafter-system";

#[derive(Tabled)]
struct ConditionRow {
    #[tabled(rename = "TYPE")]
    type_: String,
    #[tabled(rename = "STATUS")]
    status: String,
    #[tabled(rename = "REASON")]
    reason: String,
    #[tabled(rename = "MESSAGE")]
    message: String,
}

#[derive(Tabled)]
struct HistoryRow {
    #[tabled(rename = "APPLIED AT")]
    applied_at: String,
    #[tabled(rename = "VERSION")]
    version: String,
    #[tabled(rename = "OUTCOME")]
    outcome: String,
}

pub fn status() -> Result<()> {
    let kc = ensure_kubeconfig_tempfile()?;
    let json = kubectl_get_json(
        "platformstack",
        Some(PLATFORMSTACK_NAME),
        Some(PLATFORMSTACK_NAMESPACE),
        kc.path(),
    )?
    .ok_or_else(|| {
        CliError::Other(format!(
            "PlatformStack {PLATFORMSTACK_NAMESPACE}/{PLATFORMSTACK_NAME} not found in cluster — \
             is `apprafter cluster-bootstrap` complete?"
        ))
    })?;

    print_status(&json, Utc::now());
    Ok(())
}

/// Pure formatter — pulled out so unit tests can drive with a
/// fixture JSON without a cluster. `now` lets tests pin "now"
/// for deterministic relative-date formatting; production
/// callers use `Utc::now()`.
fn print_status(json: &Value, now: DateTime<Utc>) {
    let spec = json.get("spec").cloned().unwrap_or(Value::Null);
    let status = json.get("status").cloned().unwrap_or(Value::Null);

    let channel = spec
        .pointer("/channel")
        .and_then(Value::as_str)
        .unwrap_or("stable");
    let pin = spec
        .pointer("/pin")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| "(unpinned)".to_string());
    let auto_upgrade = spec
        .pointer("/autoUpgrade")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let tier = spec
        .pointer("/values/tier")
        .and_then(Value::as_i64)
        .unwrap_or(0);

    let current = status
        .pointer("/currentVersion")
        .and_then(Value::as_str)
        .unwrap_or("(unset)");
    let target = status
        .pointer("/targetVersion")
        .and_then(Value::as_str)
        .unwrap_or("(unset)");
    let available = status
        .pointer("/availableVersion")
        .and_then(Value::as_str)
        .unwrap_or("(unset)");
    let last_check_raw = status.pointer("/lastUpstreamCheck").and_then(Value::as_str);
    let last_check = last_check_raw
        .map(|s| format_timestamp_with_relative(s, now))
        .unwrap_or_else(|| "(never)".to_string());

    println!("PlatformStack {PLATFORMSTACK_NAMESPACE}/{PLATFORMSTACK_NAME} — tier {tier}");
    println!("  channel:     {channel}");
    println!("  pin:         {pin}");
    println!("  autoUpgrade: {auto_upgrade}");
    println!();
    println!("Versions:");
    println!("  current:   {current}");
    println!("  target:    {target}");
    println!("  available: {available}");
    println!("  lastCheck: {last_check}");
    println!();

    let conditions: Vec<ConditionRow> = status
        .pointer("/conditions")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .map(|c| ConditionRow {
                    type_: c
                        .get("type")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                    status: c
                        .get("status")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                    reason: c
                        .get("reason")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                    message: c
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                })
                .collect()
        })
        .unwrap_or_default();

    if conditions.is_empty() {
        println!("Conditions: (none)");
    } else {
        println!("Conditions:");
        println!("{}", render_conditions_table(&conditions));
    }
    println!();

    let history: Vec<HistoryRow> = collect_history_rows(&status, now, 5);

    if history.is_empty() {
        println!("Recent history: (none)");
    } else {
        println!("Recent history (last {}):", history.len());
        println!("{}", Table::new(&history));
    }
}

/// Render the conditions table sized to the operator's
/// terminal. Without sizing the `MESSAGE` column dominates
/// (some operator-controller messages run hundreds of
/// characters); previous heuristic of a flat 60-char wrap
/// blew out narrow terminals (80 cols → table sprawled at
/// 130-ish cols). Compute a budget that subtracts the other
/// three columns' max widths plus separator overhead, then
/// wrap MESSAGE to that budget. Falls back to a sane 60 when
/// stdout isn't a TTY (CI, pipes — width unknown).
fn render_conditions_table(conditions: &[ConditionRow]) -> String {
    let terminal_width = terminal_width_or_default();
    // Compute the visible width each non-message column will
    // claim: max(header, cells). Plus 3 separators (` | `) of
    // 3 chars each × column gaps.
    let type_w = column_width(conditions, |r| &r.type_, "TYPE");
    let status_w = column_width(conditions, |r| &r.status, "STATUS");
    let reason_w = column_width(conditions, |r| &r.reason, "REASON");
    // Tabled adds borders / paddings; budget 12 char overhead
    // empirically (4 columns × 3-char gap + outer borders).
    let overhead = 12usize;
    let used = type_w + status_w + reason_w + overhead;
    let message_budget = terminal_width.saturating_sub(used).max(20);
    let mut t = Table::new(conditions);
    t.with(Modify::new(Columns::single(3)).with(Width::wrap(message_budget)));
    t.to_string()
}

fn column_width<F: Fn(&ConditionRow) -> &str>(
    rows: &[ConditionRow],
    field: F,
    header: &str,
) -> usize {
    rows.iter()
        .map(|r| field(r).len())
        .max()
        .unwrap_or(0)
        .max(header.len())
}

/// Look up the operator's terminal width with sane fallbacks
/// — `terminal_size` for TTYs, 100 when stdout is piped or
/// the lookup fails. 100 keeps tables readable on common CI
/// log capture without surprising sprawl.
fn terminal_width_or_default() -> usize {
    match terminal_size::terminal_size() {
        Some((terminal_size::Width(w), _)) => w as usize,
        None => 100,
    }
}

/// Collect the most recent history entries, sorted by
/// `appliedAt` desc. Falls back to declaration order when the
/// timestamp is missing/unparseable (puts unparseable last so
/// they don't dominate the visible head of the table).
fn collect_history_rows(status: &Value, now: DateTime<Utc>, take: usize) -> Vec<HistoryRow> {
    let mut entries: Vec<&Value> = status
        .pointer("/versionHistory")
        .and_then(Value::as_array)
        .map(|arr| arr.iter().collect())
        .unwrap_or_default();
    // Stable sort by parsed `appliedAt` desc; entries without
    // a parseable timestamp sink to the bottom (they're either
    // corrupt CRs or freshly-created records still missing the
    // field).
    entries.sort_by(|a, b| {
        let ta = a
            .get("appliedAt")
            .and_then(Value::as_str)
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok());
        let tb = b
            .get("appliedAt")
            .and_then(Value::as_str)
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok());
        match (ta, tb) {
            (Some(a), Some(b)) => b.cmp(&a),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => std::cmp::Ordering::Equal,
        }
    });
    entries
        .into_iter()
        .take(take)
        .map(|e| {
            let raw_at = e.get("appliedAt").and_then(Value::as_str).unwrap_or("");
            HistoryRow {
                applied_at: if raw_at.is_empty() {
                    String::new()
                } else {
                    format_timestamp_with_relative(raw_at, now)
                },
                version: e
                    .get("version")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                outcome: e
                    .get("outcome")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
            }
        })
        .collect()
}

/// Render an RFC3339 timestamp as `2026-05-24 14:30 UTC
/// (2 hours ago)` style. Operators don't think in raw RFC3339
/// — the relative suffix surfaces "is this recent?" at a
/// glance, the absolute prefix keeps the exact moment
/// available for audit.
///
/// Returns the original string verbatim when parsing fails so
/// we never lose information operators may need; the only cost
/// of a parse failure is the missing relative suffix.
pub(crate) fn format_timestamp_with_relative(raw: &str, now: DateTime<Utc>) -> String {
    let Ok(parsed) = DateTime::parse_from_rfc3339(raw) else {
        return raw.to_string();
    };
    let utc = parsed.with_timezone(&Utc);
    let absolute = utc.format("%Y-%m-%d %H:%M UTC");
    let delta = now.signed_duration_since(utc);
    let relative = humanise_relative(delta);
    format!("{absolute} ({relative})")
}

/// Render a signed duration relative to "now" as a short
/// English phrase: `just now`, `2 minutes ago`, `3 hours ago`,
/// `5 days ago`, `in 3 minutes`, etc. Granularity matches
/// what operators actually care about — sub-minute precision
/// is noise on platform-level events.
fn humanise_relative(delta: chrono::Duration) -> String {
    let secs = delta.num_seconds();
    let abs = secs.unsigned_abs();
    let in_past = secs >= 0;

    let (unit, value) = if abs < 45 {
        return if in_past {
            "just now".to_string()
        } else {
            "in a few seconds".to_string()
        };
    } else if abs < 90 {
        ("minute", 1u64)
    } else if abs < 60 * 60 {
        ("minute", (abs as f64 / 60.0).round() as u64)
    } else if abs < 60 * 60 * 2 {
        ("hour", 1u64)
    } else if abs < 60 * 60 * 24 {
        ("hour", (abs as f64 / 3600.0).round() as u64)
    } else if abs < 60 * 60 * 24 * 2 {
        ("day", 1u64)
    } else if abs < 60 * 60 * 24 * 30 {
        ("day", (abs as f64 / 86_400.0).round() as u64)
    } else if abs < 60 * 60 * 24 * 60 {
        ("month", 1u64)
    } else if abs < 60 * 60 * 24 * 365 {
        ("month", (abs as f64 / 2_592_000.0).round() as u64)
    } else if abs < 60 * 60 * 24 * 365 * 2 {
        ("year", 1u64)
    } else {
        ("year", (abs as f64 / 31_536_000.0).round() as u64)
    };

    let plural = if value == 1 { "" } else { "s" };
    if in_past {
        format!("{value} {unit}{plural} ago")
    } else {
        format!("in {value} {unit}{plural}")
    }
}

/// `apprafter platform freeze <component> [--version <v>]`
/// patches PlatformStack.spec.overrides.<component>.pin.
/// Without `--version` reads the current effective component
/// version from `status.componentVersions.<component>` and
/// locks that.
pub fn freeze(component: &str, version: Option<&str>) -> Result<()> {
    let kc = ensure_kubeconfig_tempfile()?;
    let json = kubectl_get_json(
        "platformstack",
        Some(PLATFORMSTACK_NAME),
        Some(PLATFORMSTACK_NAMESPACE),
        kc.path(),
    )?
    .ok_or_else(|| {
        CliError::Other(format!(
            "PlatformStack {PLATFORMSTACK_NAMESPACE}/{PLATFORMSTACK_NAME} not found"
        ))
    })?;

    let pin = match version {
        Some(v) => v.to_string(),
        None => json
            .pointer(&format!("/status/componentVersions/{component}"))
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| {
                CliError::Other(format!(
                    "No effective version found for component '{component}' in \
                     status.componentVersions. Pass `--version <v>` explicitly or run \
                     `apprafter platform status` to inspect the list of known components."
                ))
            })?,
    };

    let body = format!(r#"{{"spec":{{"overrides":{{"{component}":{{"pin":"{pin}"}}}}}}}}"#);
    kubectl_merge_patch(
        "platformstack",
        PLATFORMSTACK_NAME,
        Some(PLATFORMSTACK_NAMESPACE),
        None,
        &body,
        kc.path(),
    )?;

    println!("✓ Component '{component}' frozen at version '{pin}'.");
    println!(
        "The PlatformController reconcile cycle will apply the override; the umbrella \
         chart's curated pin for '{component}' is ignored as long as the override is set."
    );
    println!("To revert: `apprafter platform unfreeze {component}`.");
    Ok(())
}

/// `apprafter platform unfreeze <component>` — RFC 7396
/// merge-patch with a `null` value removes the override entry.
pub fn unfreeze(component: &str) -> Result<()> {
    let kc = ensure_kubeconfig_tempfile()?;

    // RFC 7396: null deletes the field. Patches the
    // `overrides.<component>` entry as a whole — strips both
    // `pin` and `values` overrides. If the operator wants to
    // keep a partial override (e.g. unfreeze the pin but keep
    // values overrides), they should patch manually; `unfreeze`
    // is the "fully revert to the chart's curated state" verb.
    let body = format!(r#"{{"spec":{{"overrides":{{"{component}":null}}}}}}"#);
    kubectl_merge_patch(
        "platformstack",
        PLATFORMSTACK_NAME,
        Some(PLATFORMSTACK_NAMESPACE),
        None,
        &body,
        kc.path(),
    )?;
    println!("✓ Component '{component}' unfrozen. Chart's curated pin restored.");
    Ok(())
}

/// `apprafter platform rescue` — emergency recovery wrapper
/// over `apprafter cluster-bootstrap`. Re-applies the loader's
/// Cilium + Argo CD + CRDs + operator chain against the active
/// target. Useful when Argo CD itself is unable to self-adopt
/// and a regular upgrade flow won't reach the right reconcile
/// state.
pub fn rescue(yes: bool) -> Result<()> {
    if !yes {
        use std::io::IsTerminal;
        if !std::io::stdin().is_terminal() {
            return Err(CliError::Other(
                "non-interactive shell — pass `--yes` to skip the confirmation prompt".into(),
            ));
        }
        println!(
            "Emergency rescue: re-run the loader's cluster-bootstrap path against the active \
             target. This will apply the upstream Cilium / Argo CD / CRDs / operator manifests \
             as in the initial bootstrap — all AppRafter-managed Applications will lose their \
             current Sync/Healthy state for a few reconcile cycles."
        );
        let confirmed = inquire::Confirm::new("Confirm?")
            .with_default(false)
            .prompt()
            .map_err(|e| CliError::Other(format!("confirmation prompt: {e}")))?;
        if !confirmed {
            println!("Cancelled.");
            return Ok(());
        }
    }
    println!("Re-running cluster-bootstrap chain...");
    crate::commands::cluster_bootstrap::run()
}

pub fn upgrade(to: Option<&str>) -> Result<()> {
    let kc = ensure_kubeconfig_tempfile()?;
    let body = match to {
        Some(v) => format!(r#"{{"spec":{{"pin":"{v}"}}}}"#),
        None => r#"{"spec":{"pin":null,"autoUpgrade":true}}"#.to_string(),
    };
    kubectl_merge_patch(
        "platformstack",
        PLATFORMSTACK_NAME,
        Some(PLATFORMSTACK_NAMESPACE),
        None,
        &body,
        kc.path(),
    )?;
    match to {
        Some(v) => println!("Pinned PlatformStack/{PLATFORMSTACK_NAME} to {v}"),
        None => println!(
            "Cleared pin; autoUpgrade=true. PlatformController will resolve to channel-latest."
        ),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    use chrono::TimeZone;

    fn frozen_now() -> DateTime<Utc> {
        // A fixed reference moment so relative-date tests
        // don't drift with wall-clock — picks a known
        // RFC3339 timestamp the helpers can subtract from.
        Utc.with_ymd_and_hms(2026, 5, 24, 14, 30, 0).unwrap()
    }

    #[test]
    fn print_status_handles_minimal_object() {
        // PlatformStack with only spec, no status (fresh CR).
        // Must not panic; should gracefully print "(unset)" /
        // "(none)" placeholders.
        let obj = json!({
            "spec": { "channel": "stable", "values": { "tier": 1 } }
        });
        print_status(&obj, frozen_now());
    }

    #[test]
    fn print_status_renders_full_object() {
        // Smoke test for the happy path — all sections populated.
        let obj = json!({
            "spec": {
                "channel": "stable",
                "autoUpgrade": true,
                "pin": "0.1.35",
                "values": { "tier": 1 }
            },
            "status": {
                "currentVersion": "0.1.35",
                "targetVersion": "0.1.35",
                "availableVersion": "0.1.38",
                "lastUpstreamCheck": "2026-05-23T22:30:00Z",
                "conditions": [
                    { "type": "Ready", "status": "True", "reason": "Healthy", "message": "ok" }
                ],
                "versionHistory": [
                    { "appliedAt": "2026-05-23T21:00:00Z", "version": "0.1.34", "outcome": "succeeded" }
                ]
            }
        });
        print_status(&obj, frozen_now());
    }

    #[test]
    fn format_timestamp_renders_absolute_and_relative() {
        // 90 minutes before the frozen `now` reference. The
        // absolute prefix is the parsed UTC moment; the
        // relative suffix is rounded to the most relevant
        // unit ("2 hours ago" matches the 1.5h → 2h round).
        let ts = "2026-05-24T13:00:00Z";
        let s = format_timestamp_with_relative(ts, frozen_now());
        assert!(s.starts_with("2026-05-24 13:00 UTC"), "got: {s}");
        assert!(s.contains("ago"), "expected past tense: {s}");
        assert!(s.contains("hour"), "expected hour unit: {s}");
    }

    #[test]
    fn format_timestamp_handles_unparseable_input() {
        // Unparseable input must surface verbatim so operators
        // don't lose information. Audit value > prettiness.
        let s = format_timestamp_with_relative("not-a-date", frozen_now());
        assert_eq!(s, "not-a-date");
    }

    #[test]
    fn humanise_relative_uses_just_now_under_45_seconds() {
        // Sub-minute precision is noise for platform events.
        // Up to 45s past or future renders as "just now" /
        // "in a few seconds" — keeps the output uncluttered.
        let now = frozen_now();
        let ten_seconds_ago = now - chrono::Duration::seconds(10);
        let s = format_timestamp_with_relative(&ten_seconds_ago.to_rfc3339(), now);
        assert!(s.contains("just now"), "got: {s}");
    }

    #[test]
    fn humanise_relative_handles_minutes_hours_days_months_years() {
        // Span coverage across every unit branch — guards
        // against an accidental thresholding regression that
        // could surface "60 minutes ago" instead of "1 hour
        // ago".
        let now = frozen_now();
        let cases = [
            (chrono::Duration::minutes(5), "5 minutes ago"),
            (chrono::Duration::hours(3), "3 hours ago"),
            (chrono::Duration::days(2), "2 days ago"),
            (chrono::Duration::days(45), "1 month ago"),
            (chrono::Duration::days(400), "1 year ago"),
        ];
        for (delta, expected_suffix) in cases {
            let ts = (now - delta).to_rfc3339();
            let s = format_timestamp_with_relative(&ts, now);
            assert!(
                s.contains(expected_suffix),
                "for delta {delta:?}, expected '{expected_suffix}' in '{s}'"
            );
        }
    }

    #[test]
    fn collect_history_rows_sorts_by_applied_at_desc() {
        // Source data deliberately out-of-order to assert the
        // sort actually fires (instead of merely preserving
        // declaration order which happens to be sorted).
        let status = json!({
            "versionHistory": [
                { "appliedAt": "2026-05-20T10:00:00Z", "version": "0.1.30", "outcome": "succeeded" },
                { "appliedAt": "2026-05-24T10:00:00Z", "version": "0.1.40", "outcome": "succeeded" },
                { "appliedAt": "2026-05-22T10:00:00Z", "version": "0.1.35", "outcome": "succeeded" }
            ]
        });
        let rows = collect_history_rows(&status, frozen_now(), 10);
        let versions: Vec<&str> = rows.iter().map(|r| r.version.as_str()).collect();
        assert_eq!(versions, vec!["0.1.40", "0.1.35", "0.1.30"]);
    }

    #[test]
    fn collect_history_rows_puts_unparseable_timestamps_last() {
        // Corrupt / mid-write CRs (timestamp not yet stamped)
        // shouldn't dominate the visible head of the table.
        let status = json!({
            "versionHistory": [
                { "appliedAt": "garbage", "version": "0.0.0", "outcome": "succeeded" },
                { "appliedAt": "2026-05-24T10:00:00Z", "version": "0.1.40", "outcome": "succeeded" }
            ]
        });
        let rows = collect_history_rows(&status, frozen_now(), 10);
        // First row must be the parseable one; the unparseable
        // entry sinks to the bottom.
        assert_eq!(rows.first().map(|r| r.version.as_str()), Some("0.1.40"));
        assert_eq!(rows.get(1).map(|r| r.version.as_str()), Some("0.0.0"));
    }

    #[test]
    fn collect_history_rows_caps_at_take() {
        let entries: Vec<_> = (0..10)
            .map(|i| {
                json!({
                    "appliedAt": format!("2026-05-{:02}T10:00:00Z", 10 + i),
                    "version": format!("0.1.{i}"),
                    "outcome": "succeeded"
                })
            })
            .collect();
        let status = json!({ "versionHistory": entries });
        let rows = collect_history_rows(&status, frozen_now(), 3);
        assert_eq!(rows.len(), 3);
    }
}
