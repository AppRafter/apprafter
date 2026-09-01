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
    ensure_kubeconfig_tempfile, kubectl_apply_server_side, kubectl_get_json,
    kubectl_get_json_by_selector, kubectl_get_json_showing_managed_fields, kubectl_merge_patch,
};
use cli_providers::k8s::kubectl::APPRAFTER_CLI_EGRESS_FIELD_MANAGER;

const PLATFORMSTACK_NAME: &str = "default";
const PLATFORMSTACK_NAMESPACE: &str = "apprafter-system";

/// Annotation the CLI stamps to ask the operator for an immediate
/// upstream OCI re-poll (instead of waiting for the operator's 6h
/// cadence). Contract shared with the operator: the operator sees
/// this RFC3339 timestamp is newer than `status.lastUpstreamCheck`,
/// does an immediate poll, and then stamps
/// `status.lastUpstreamCheck = now` (> the request ts). The CLI's
/// "recheck completed" signal is `status.lastUpstreamCheck` parsing
/// to a moment STRICTLY AFTER the request ts.
const RECHECK_REQUESTED_ANNOTATION: &str = "apprafter.io/recheck-requested";

/// How long `status` / `update` wait for the operator to honour a
/// recheck before falling back to the last-known status. Kept short
/// — the operator's poll is a single OCI HEAD; a longer wait would
/// only punish operators whose cluster runs a binary that predates
/// the recheck contract (it ignores the annotation, so we'd wait the
/// full budget every time).
const RECHECK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Poll interval while waiting for `status.lastUpstreamCheck` to
/// advance past the request timestamp.
const RECHECK_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(1);

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

pub fn status(cached: bool) -> Result<()> {
    let kc = ensure_kubeconfig_tempfile()?;

    // Unless the operator opted into the last-known snapshot, ask the
    // operator for a fresh upstream re-check first so the displayed
    // `available` / `lastCheck` / `UpgradeAvailable` reflect the most
    // recent OCI poll rather than the operator's (up to 6h stale) cadence.
    if !cached {
        force_recheck_and_wait(kc.path(), Utc::now)?;
    }

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

    // The platform's own state prints FIRST, before the two cluster-wide
    // reads below. It is what this command is named for, it is already in
    // hand, and making it wait behind two more round-trips would delay output
    // that used to appear immediately.
    print_status(&json, Utc::now());

    // ONE cluster-wide application read, shared by both roll-ups. Two
    // independent fetches would let the two sections describe two different
    // instants, and would double the cost on a large cluster for nothing.
    //
    // The Results are kept rather than unwrapped: the pinned section stays
    // silent on a read failure (it is decorative — see its doc comment), and
    // the problem section must NOT, for the reason stated on its printer.
    let apps = kubectl_get_json_by_selector("application.apprafter.io", "", None, kc.path());
    let argo = kubectl_get_json_by_selector(
        "application.argoproj.io",
        "",
        Some(crate::commands::app::ARGOCD_NAMESPACE),
        kc.path(),
    );

    if let Ok(items) = &apps {
        print_pinned_applications(items);
    }
    print_problem_applications(apps.as_deref(), argo.as_deref(), &Utc::now());
    Ok(())
}

/// The cluster's application-health verdict (2.22h / D16), printed LAST.
///
/// Placed after everything else because it is the answer to "is anything
/// wrong?", and the tail of the output is what a terminal leaves on screen.
///
/// THREE DELIBERATE INVERSIONS of the pinned roll-up this sits next to. That
/// one is decorative and says so; this one is a health signal, and a health
/// signal has the opposite failure asymmetry:
///
///  1. It prints when there is nothing to report. `app status` deliberately
///     prints no problem section for a healthy application, because there the
///     surrounding output already proves the command ran. Here it does not:
///     an absent section is indistinguishable from "the check did not run",
///     "this CLI is too old to have the check", and "everything is fine". The
///     question being asked is whether anything is wrong, and to that question
///     silence is not an answer.
///  2. It is LOUD on a read failure. Copying the precedent's silent
///     `else { return; }` would render an RBAC denial, an apiserver timeout or
///     a missing CRD as a clean bill of health — the one output this section
///     must never produce.
///  3. It names applications the way the READER must type them: the logical
///     name `app status` takes, not the CR's `metadata.name`. An application
///     the roll-up cannot resolve that way is still listed, and labelled as
///     unresolvable, rather than dropped.
fn print_problem_applications(
    crs: std::result::Result<&[Value], &CliError>,
    argo: std::result::Result<&[Value], &CliError>,
    now: &DateTime<Utc>,
) {
    println!();
    // ONLY the application read can silence this section. The problem data
    // lives entirely on the AppRafter CRs; the Argo CD read supplies NAMES.
    // Discarding a complete problem list because the naming lookup failed
    // would report "unknown" about state we successfully read, and would make
    // this section newly dependent on read access to the argocd namespace.
    let crs = match crs {
        Ok(c) => c,
        Err(e) => {
            println!(
                "{}",
                cli_core::style::warn(&format!(
                    "Applications: could not read ({e}) — problem state unknown."
                ))
            );
            return;
        }
    };
    let (argo, naming_failed) = match argo {
        Ok(a) => (a, false),
        Err(_) => (&[][..], true),
    };

    let live = live_applications(crs);
    let rows = problem_app_rows(&live, argo, now);
    if rows.is_empty() {
        println!(
            "Applications: {} checked, none reporting problems ({}).",
            live.len(),
            crate::commands::app::problem_window_label()
        );
        return;
    }

    let shown = rows.len().min(PROBLEM_ROW_CAP);
    println!(
        "{}",
        cli_core::style::warn(&problem_heading(live.len(), rows.len(), PROBLEM_ROW_CAP))
    );
    for row in rows.iter().take(PROBLEM_ROW_CAP) {
        println!("{}", cli_core::style::warn(&format!("  {row}")));
    }
    if rows.len() > shown {
        println!(
            "{}",
            cli_core::style::warn(&format!("  … and {} more", rows.len() - shown))
        );
    }
    if naming_failed {
        println!(
            "{}",
            cli_core::style::warn(
                "  note: could not read Argo CD registrations — applications are named by their CR"
            )
        );
    }
    // Printed ONCE, not per row — `format_problem_lines` appends its own
    // advisory per call, which is right for one application and a wall for N.
    println!("  run `apprafter app status <name>` for the full ledger");
}

/// The applications a roll-up may speak about: everything not on its way out.
///
/// Deletion-marked CRs are excluded from BOTH the tally and the rows. The
/// Application controller evicts the in-memory ledger and returns before it
/// ever flushes again for a dying object, so whatever entries such a CR still
/// carries can never be updated or cleared — they would be permanent phantom
/// rows for something already being deleted.
fn live_applications(crs: &[Value]) -> Vec<&Value> {
    crs.iter()
        .filter(|c| c.pointer("/metadata/deletionTimestamp").is_none())
        .collect()
}

/// List every application currently held at an image digest (ADR 0059).
///
/// Best-effort and silent on failure: this is a decorative addition to a
/// command whose job is the platform's own version state, and an unreadable
/// application list must not turn `platform status` into an error.
///
/// It exists because a pin is invisible to a reader of the Git repository, so
/// without a cluster-wide view an operator would have to run `app status` per
/// application to discover which ones have stopped receiving builds.
fn print_pinned_applications(items: &[Value]) {
    // `items` is the caller's single cluster-wide read (`-A`, empty selector —
    // `kubectl_get_json` with no namespace does NOT pass `-A` and would
    // silently list only the kubeconfig's default namespace, which reads as
    // "nothing is pinned"). It is shared with the problem roll-up so both
    // sections describe one instant.
    let rows = pinned_app_rows(items);
    if rows.is_empty() {
        return;
    }
    println!();
    println!(
        "{}",
        cli_core::style::warn(&format!(
            "Pinned applications ({}) — held at a digest, NOT following their tag:",
            rows.len()
        ))
    );
    for row in rows {
        println!("{}", cli_core::style::warn(&format!("  {row}")));
    }
}

/// Pure: the heading above the problem rows.
///
/// States the TRUE total, never the number of lines about to be printed. The
/// cap governs display only, and a heading that silently reports the cap both
/// contradicts the "… and N more" line directly beneath it and understates the
/// blast radius at exactly the moment the number gets quoted into an incident
/// channel.
pub(crate) fn problem_heading(checked: usize, total: usize, cap: usize) -> String {
    let scope = if total > cap {
        format!(" (showing the {cap} most recent)")
    } else {
        " (most recent first)".to_string()
    };
    format!("Applications: {checked} checked, {total} reporting problems{scope}:")
}

/// Most problem rows printed before the tail is summarised. A roll-up that
/// scrolls the terminal at exactly the moment something is wrong is one the
/// reader stops using.
const PROBLEM_ROW_CAP: usize = 10;

/// Pure: one row per application carrying a problem the reader should see.
///
/// Takes BOTH lists because the identity a reader can act on lives on the Argo
/// CD side. The join is the one `app status` already performs, run backwards:
/// `find_apprafter_app_name` reads the inner CR's name out of an Argo
/// Application's `status.resources[]`, and `spec.destination.namespace` gives
/// the namespace — so `(namespace, cr-name)` maps to the logical name
/// `apprafter.io/application` and the environment.
///
/// An application with problems that does NOT resolve through that join is
/// still listed, marked unresolvable. Dropping it would hide exactly the
/// applications most likely to be broken, and `app status` cannot render those
/// either — saying so is the honest output.
pub(crate) fn problem_app_rows(crs: &[&Value], argo: &[Value], now: &DateTime<Utc>) -> Vec<String> {
    // (namespace, inner CR name) -> display identity.
    let mut index: std::collections::HashMap<(String, String), String> =
        std::collections::HashMap::new();
    for a in argo {
        let Some(ns) = a
            .pointer("/spec/destination/namespace")
            .and_then(Value::as_str)
        else {
            continue;
        };
        let Some(inner) = crate::commands::app_open::find_apprafter_app_name(a) else {
            continue;
        };
        let logical = a
            .pointer("/metadata/labels/apprafter.io~1application")
            .and_then(Value::as_str)
            .or_else(|| a.pointer("/metadata/name").and_then(Value::as_str))
            .unwrap_or("<unknown>");
        let env = a
            .pointer("/metadata/labels/apprafter.io~1environment")
            .and_then(Value::as_str);
        let display = match env {
            Some(e) if !e.is_empty() => format!("{logical} ({e})"),
            _ => logical.to_string(),
        };
        index.insert((ns.to_string(), inner), display);
    }

    let mut rows: Vec<(i64, String)> = Vec::new();
    for cr in crs {
        let problems = crate::commands::app::live_problems(cr, now);
        // The FRESHEST entry, explicitly — never `problems.first()`. The
        // operator writes `recentProblems` sorted by `firstSeen` ASCENDING
        // (`ProblemLedger::snapshot`), so element 0 is the problem that
        // started earliest, which says nothing about what is burning now. A
        // row built from it would name a failure that stopped hours ago and
        // hide the live one behind "(+N more)" — and, because the same entry
        // is the sort key, would file the whole application in the wrong place
        // under a heading that promises "most recent first".
        let Some(newest) = problems.iter().min_by_key(|p| p.age) else {
            continue;
        };
        let ns = cr
            .pointer("/metadata/namespace")
            .and_then(Value::as_str)
            .unwrap_or("<unknown>");
        let name = cr
            .pointer("/metadata/name")
            .and_then(Value::as_str)
            .unwrap_or("<unknown>");
        let (identity, suffix) = match index.get(&(ns.to_string(), name.to_string())) {
            Some(display) => (display.clone(), String::new()),
            // Deliberately not "not registered with Argo CD": an application
            // that IS registered but has not synced yet has an empty
            // `status.resources[]`, so the join misses it too. Say what is
            // actually known — the name could not be resolved — rather than
            // asserting a cause that may be false.
            None => (
                format!("{ns}/{name}"),
                " — logical name unresolved; `app status` may not find it under this name"
                    .to_string(),
            ),
        };
        // The newest surviving entry carries the row; the rest are counted.
        // A row per entry would put five lines on one application and bury
        // the other applications that are also broken.
        let more = if problems.len() > 1 {
            format!(" (+{} more)", problems.len() - 1)
        } else {
            String::new()
        };
        rows.push((
            newest.age,
            format!(
                "{identity}  {} ({}{}): {}{more}{suffix}",
                newest.reason,
                newest.when(now),
                newest.times(),
                newest.message
            ),
        ));
    }
    // Most recent first; ties broken by the rendered text so the order is
    // stable across runs rather than dependent on map iteration.
    rows.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
    rows.into_iter().map(|(_, r)| r).collect()
}

/// Pure: one line per pinned application in a `kubectl get -o json` list.
///
/// Reads `status.image.pinned`, which the operator writes only when the pin
/// is HONOURED — a rejected pin must not appear here, or an operator would
/// chase an application that is in fact still following its tag.
pub(crate) fn pinned_app_rows(items: &[Value]) -> Vec<String> {
    items
        .iter()
        .filter_map(|app| {
            let reference = app
                .pointer("/status/image/pinned/resolved")
                .and_then(Value::as_str)?;
            let name = app
                .pointer("/metadata/name")
                .and_then(Value::as_str)
                .unwrap_or("<unknown>");
            let ns = app
                .pointer("/metadata/namespace")
                .and_then(Value::as_str)
                .unwrap_or("<unknown>");
            Some(format!("{ns}/{name}  {reference}"))
        })
        .collect()
}

/// Stamp the recheck-request annotation on the singleton
/// PlatformStack, then POLL `status.lastUpstreamCheck` until it
/// advances past the timestamp we wrote (the operator completed an
/// immediate poll) or the [`RECHECK_TIMEOUT`] budget elapses.
///
/// GRACEFUL on timeout: a cluster whose operator predates this
/// contract simply ignores the annotation and never advances
/// `lastUpstreamCheck` in response — we must NOT hard-fail (status
/// still has to render the last-known data). On timeout we print a
/// one-line `note:` and return `Ok(())`; the caller then reads +
/// renders whatever the cluster currently reports.
///
/// `now` is injected (a `Fn() -> DateTime<Utc>`) so the request
/// timestamp and the elapsed-budget check use the same clock and
/// tests can drive the comparison deterministically.
fn force_recheck_and_wait<F: Fn() -> DateTime<Utc>>(
    kubeconfig_path: &std::path::Path,
    now: F,
) -> Result<()> {
    let requested = now();
    let body = recheck_annotation_patch_body(&requested.to_rfc3339());
    kubectl_merge_patch(
        "platformstack",
        PLATFORMSTACK_NAME,
        Some(PLATFORMSTACK_NAMESPACE),
        None,
        &body,
        kubeconfig_path,
    )?;

    let deadline = std::time::Instant::now() + RECHECK_TIMEOUT;
    loop {
        // Re-read just the status each cycle. A transient read error
        // here shouldn't abort the whole command — treat it like a
        // not-yet-fresh poll and keep waiting until the deadline.
        let last_check = kubectl_get_json(
            "platformstack",
            Some(PLATFORMSTACK_NAME),
            Some(PLATFORMSTACK_NAMESPACE),
            kubeconfig_path,
        )
        .ok()
        .flatten()
        .and_then(|j| {
            j.pointer("/status/lastUpstreamCheck")
                .and_then(Value::as_str)
                .map(str::to_string)
        });

        if recheck_completed(last_check.as_deref(), requested) {
            return Ok(());
        }

        if std::time::Instant::now() >= deadline {
            println!(
                "note: upstream re-check did not complete within {}s (operator may predate this \
                 feature); showing last-known data",
                RECHECK_TIMEOUT.as_secs()
            );
            return Ok(());
        }
        std::thread::sleep(RECHECK_POLL_INTERVAL);
    }
}

/// RFC 7396 merge-patch body that stamps the recheck-request
/// annotation. Pure fn — the timestamp string is the caller's; the
/// builder just wraps it in the metadata/annotations envelope so it
/// can be unit-tested without a cluster.
pub(crate) fn recheck_annotation_patch_body(ts: &str) -> String {
    // serde_json so the timestamp is JSON-escaped correctly (RFC3339
    // has no quotes/backslashes, but route it through the encoder
    // rather than hand-splicing to stay safe).
    serde_json::json!({
        "metadata": { "annotations": { RECHECK_REQUESTED_ANNOTATION: ts } }
    })
    .to_string()
}

/// The "recheck completed" predicate: did the operator stamp a
/// `status.lastUpstreamCheck` STRICTLY AFTER the request ts we
/// wrote? Pure fn so every branch is unit-testable.
///
/// - `None` (status carries no `lastUpstreamCheck`) → not completed.
/// - Unparseable timestamp → not completed (defensive; the operator
///   always writes RFC3339, but a mid-write CR shouldn't read as done).
/// - Parsed but `<= requested` → the value is stale (operator hasn't
///   polled since our request, OR predates the contract) → not done.
/// - Parsed and `> requested` → fresh → done.
pub(crate) fn recheck_completed(
    last_upstream_check: Option<&str>,
    requested: DateTime<Utc>,
) -> bool {
    let Some(raw) = last_upstream_check else {
        return false;
    };
    match DateTime::parse_from_rfc3339(raw) {
        Ok(parsed) => parsed.with_timezone(&Utc) > requested,
        Err(_) => false,
    }
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
/// patches `PlatformStack.spec.overrides.<component>.pin`.
/// Without `--version` resolves the current effective version
/// through a fallback chain so the verb is always actionable
/// no matter which signals the cluster currently surfaces.
///
/// Resolution chain (walk-fix #3 post-B.1.79a / v0.1.147):
///
///   1. **PlatformStack.status.componentVersions.<component>**
///      — the operator's canonical version dial when present.
///      M1.5 ships this populated only on bump cycles though,
///      so it can be absent on steady state.
///   2. **Argo CD `Application argocd/<component>.spec.source.
///      targetRevision`** — the version Argo CD is actively
///      reconciling against. Always present for a chart-managed
///      component, since the umbrella's `templates/applications.
///      yaml` template emits it. This is the new fallback.
///   3. Hard error pointing the operator at `--version <v>`.
pub fn freeze(component: &str, version: Option<&str>) -> Result<()> {
    let kc = ensure_kubeconfig_tempfile()?;
    let stack = kubectl_get_json(
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
        None => {
            // Try the Argo CD Application as fallback when
            // PlatformStack.status doesn't carry the version
            // yet. 404 on the lookup is fine — we'll pass
            // `None` along to the resolver and let it surface
            // a clean error pointing at `--version <v>`.
            let app = kubectl_get_json(
                "application.argoproj.io",
                Some(component),
                Some("argocd"),
                kc.path(),
            )?;
            resolve_effective_pin(&stack, app.as_ref(), component)?
        }
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

/// Resolve the effective pin for `component` through the
/// PlatformStack-then-Argo-CD fallback chain. Pure fn — tests
/// drive every branch with fixture JSON instead of needing a
/// live cluster.
///
/// Returns the rendered string verbatim. Caller threads it
/// into the merge-patch body. None Argo CD source is allowed
/// (caller may have 404'd on the lookup); the resolver only
/// errors when BOTH signals are absent.
pub(crate) fn resolve_effective_pin(
    stack: &Value,
    argocd_app: Option<&Value>,
    component: &str,
) -> Result<String> {
    if let Some(v) = stack
        .pointer(&format!("/status/componentVersions/{component}"))
        .and_then(Value::as_str)
    {
        return Ok(v.to_string());
    }
    if let Some(app) = argocd_app {
        if let Some(v) = app
            .pointer("/spec/source/targetRevision")
            .and_then(Value::as_str)
        {
            return Ok(v.to_string());
        }
    }
    Err(CliError::Other(format!(
        "No effective version known for component '{component}' — \
         PlatformStack.status.componentVersions.{component} is empty and \
         Argo CD Application argocd/{component} carries no spec.source.targetRevision. \
         Pass `--version <v>` to set the pin explicitly, or run `apprafter platform status` \
         to inspect the list of known components."
    )))
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

pub fn upgrade(to: Option<&str>, cached: bool) -> Result<()> {
    let kc = ensure_kubeconfig_tempfile()?;

    // Force a fresh upstream re-check first (unless `--cached`) so the
    // upgrade decision — and the operator's subsequent channel-latest
    // resolution when clearing the pin — acts on the most recent
    // availableVersion rather than the operator's up-to-6h-stale poll.
    if !cached {
        force_recheck_and_wait(kc.path(), Utc::now)?;
    }

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

/// The three valid egress profiles, in order of decreasing
/// breadth. Single source of truth for both the validator and the
/// `set` error message; mirrors the operator/webhook enum.
const EGRESS_PROFILES: [&str; 3] = ["internet", "internal", "strict"];

/// Validate an egress profile string against the
/// `internet|internal|strict` enum. Pure fn — client-side guard so
/// a typo (`open`) is rejected with a clear message instead of
/// degrading into an admission-webhook rejection. Mirrors
/// `validator_platformstack.rs`'s enum (ADR 0045 §Decision #3/#4).
fn validate_egress_profile(profile: &str) -> Result<()> {
    if EGRESS_PROFILES.contains(&profile) {
        return Ok(());
    }
    Err(CliError::Other(format!(
        "egress profile '{profile}' is invalid; expected one of internet|internal|strict \
         (internet = DNS + same-ns + world + needs; internal = DNS + same-ns + needs; \
         strict = DNS + needs)."
    )))
}

/// Pure formatter for `apprafter platform egress show`. Reads
/// `/spec/network/egress/profile` from the PlatformStack JSON and
/// renders the active profile plus the three-line legend. An
/// absent field reports the documented operator default
/// (`internet`), flagged as unset so it's not mistaken for an
/// explicit `set`. Pulled out so tests drive it with a fixture
/// JSON without a cluster (mirrors `print_status`).
fn format_egress_profile(json: &Value) -> String {
    let active = json
        .pointer("/spec/network/egress/profile")
        .and_then(Value::as_str);
    let header = match active {
        Some(p) => format!("Egress profile: {p}"),
        None => "Egress profile: internet (default — field unset)".to_string(),
    };
    format!(
        "{header}\n\
         \n\
         Profiles:\n\
         \u{2022} internet  DNS + same-namespace + world (external internet) + declared needs\n\
         \u{2022} internal  DNS + same-namespace + declared needs (no external internet)\n\
         \u{2022} strict    DNS + declared needs (same-namespace egress also denied)\n\
         \n\
         Set with: apprafter platform egress set <internet|internal|strict>"
    )
}

/// `apprafter platform egress show` — read the singleton
/// PlatformStack and print the current egress profile + legend.
pub fn egress_show() -> Result<()> {
    let kc = ensure_kubeconfig_tempfile()?;
    // Managed-fields variant: `egress_field_appears_git_managed` reads
    // `metadata.managedFields`, which kubectl strips from `get -o json`
    // unless asked. Shipped in 2.10 on the plain getter, so the warning
    // below had never once been reachable.
    let json = kubectl_get_json_showing_managed_fields(
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

    println!("{}", format_egress_profile(&json));
    if egress_field_appears_git_managed(&json) {
        println!(
            "\n⚠ The egress profile field appears to be managed by Argo CD (an infra-repo \
             declares spec.network.egress.profile). `apprafter platform egress set` will be \
             reverted on the next sync — change it in git instead."
        );
    }
    Ok(())
}

/// `apprafter platform egress set <profile>` — server-side apply
/// the profile onto the singleton PlatformStack under the dedicated
/// field manager `apprafter-cli-egress`. SSA (not merge-patch) so the
/// value survives Argo CD self-heal: the platform-stack chart does not
/// declare this field, so there is no conflicting owner to revert it
/// (ADR 0045 §Decision #4 / design §E).
///
/// The manager is deliberately DISTINCT from `cluster-bootstrap`'s
/// `apprafter-cli` (which owns the REQUIRED `spec.source` + `spec.values`):
/// re-applying this partial object under that same manager would make SSA
/// prune source/values and the apiserver would reject the PlatformStack
/// (`Required value`). See [`APPRAFTER_CLI_EGRESS_FIELD_MANAGER`].
pub fn egress_set(profile: &str) -> Result<()> {
    validate_egress_profile(profile)?;
    let kc = ensure_kubeconfig_tempfile()?;

    // Best-effort: if an infra-repo already owns the field via
    // Argo CD, warn that git will win on the next sync. A 404 /
    // unparseable managedFields is non-fatal — fall through to the
    // unconditional advisory below.
    let existing = kubectl_get_json(
        "platformstack",
        Some(PLATFORMSTACK_NAME),
        Some(PLATFORMSTACK_NAMESPACE),
        kc.path(),
    )?;
    let git_managed = existing
        .as_ref()
        .map(egress_field_appears_git_managed)
        .unwrap_or(false);

    let manifest = format!(
        "apiVersion: apprafter.io/v1alpha1\n\
         kind: PlatformStack\n\
         metadata:\n\
        \x20 name: {PLATFORMSTACK_NAME}\n\
        \x20 namespace: {PLATFORMSTACK_NAMESPACE}\n\
         spec:\n\
        \x20 network:\n\
        \x20   egress:\n\
        \x20     profile: {profile}\n"
    );
    kubectl_apply_server_side(&manifest, APPRAFTER_CLI_EGRESS_FIELD_MANAGER, kc.path())?;

    println!(
        "✓ Egress profile set to '{profile}' (field manager '{APPRAFTER_CLI_EGRESS_FIELD_MANAGER}')."
    );
    println!(
        "The operator's ApplicationController re-derives each app's egress CiliumNetworkPolicy \
         on its next reconcile; run `apprafter platform egress show` to confirm."
    );
    if git_managed {
        println!(
            "⚠ This field appears to be declared in an infra-repo Argo CD reconciles — git \
             wins on the next sync and this live value will be reverted. Change it in git."
        );
    } else {
        println!(
            "Note: the platform-stack chart does not declare this field, so this value persists \
             across Argo CD syncs. If you later opt into an infra-repo that declares \
             spec.network.egress.profile, git becomes authoritative and wins on the next sync."
        );
    }
    Ok(())
}

/// Shown after every `platform env` output so operators aren't misled: the
/// default env is a CLI convenience, not a rendering gate (ADR 0044).
const SOFT_ENV_NOTE: &str =
    "(soft default — preselects the `apprafter app add` env picker; it does NOT \
     change rendering. An app added without `--env` is still base-only.)";

/// Trim + reject empty/whitespace. Pure (unit-tested without a cluster).
fn validate_env(env: &str) -> Result<&str> {
    let trimmed = env.trim();
    if trimmed.is_empty() {
        return Err(CliError::Other("environment must not be empty".into()));
    }
    Ok(trimmed)
}

/// The path-scoped JSON merge-patch body for `spec.defaultEnvironment`. Pure.
fn default_environment_patch_body(env: &str) -> String {
    serde_json::json!({ "spec": { "defaultEnvironment": env } }).to_string()
}

/// `apprafter platform env show` — print the cluster's default environment.
pub fn env_show() -> Result<()> {
    let kc = ensure_kubeconfig_tempfile()?;
    let stack = kubectl_get_json(
        "platformstack",
        Some(PLATFORMSTACK_NAME),
        Some(PLATFORMSTACK_NAMESPACE),
        kc.path(),
    )?;
    let current = stack
        .as_ref()
        .and_then(|s| s.pointer("/spec/defaultEnvironment"))
        .and_then(Value::as_str);
    match current {
        Some(env) => println!("Default environment: {env}"),
        None => println!("Default environment: (unset)"),
    }
    println!("{SOFT_ENV_NOTE}");
    Ok(())
}

/// `apprafter platform env set <env>` — set the cluster's default environment.
pub fn env_set(env: &str) -> Result<()> {
    let env = validate_env(env)?;
    let kc = ensure_kubeconfig_tempfile()?;
    let body = default_environment_patch_body(env);
    kubectl_merge_patch(
        "platformstack",
        PLATFORMSTACK_NAME,
        Some(PLATFORMSTACK_NAMESPACE),
        None,
        &body,
        kc.path(),
    )?;
    println!("✓ Default environment set to '{env}'.");
    println!("{SOFT_ENV_NOTE}");
    Ok(())
}

/// The path-scoped JSON merge-patch body for
/// `spec.resources.autoscale.mode`. Pure (unit-tested without a cluster).
fn autoscale_patch_body(mode: &str) -> String {
    serde_json::json!({ "spec": { "resources": { "autoscale": { "mode": mode } } } }).to_string()
}

/// Validate the autoscale mode string client-side so the user gets a
/// clear rejection rather than a raw apiserver/webhook error.
fn validate_autoscale_mode(mode: &str) -> Result<&str> {
    match mode {
        "full" | "up-only" | "off" => Ok(mode),
        other => Err(CliError::Other(format!(
            "invalid autoscale mode '{other}' (expected full|up-only|off)"
        ))),
    }
}

/// `apprafter platform autoscale show` — print the current VPA autoscale mode.
pub fn autoscale_show() -> Result<()> {
    let kc = ensure_kubeconfig_tempfile()?;
    let stack = kubectl_get_json(
        "platformstack",
        Some(PLATFORMSTACK_NAME),
        Some(PLATFORMSTACK_NAMESPACE),
        kc.path(),
    )?;
    let mode = stack
        .as_ref()
        .and_then(|s| s.pointer("/spec/resources/autoscale/mode"))
        .and_then(Value::as_str)
        .unwrap_or("full (default)");
    println!("Autoscale mode: {mode}");
    println!(
        "\nModes:\n\
         \u{2022} full      VPA applies both CPU and memory recommendations (up and down)\n\
         \u{2022} up-only   VPA scales resources up but never below the platform seed\n\
         \u{2022} off       VPA recommendations recorded but NOT applied to pods\n\
         \n\
         Set with: apprafter platform autoscale set <full|up-only|off>"
    );
    Ok(())
}

/// `apprafter platform autoscale set <mode>` — set the cluster-wide VPA
/// autoscale mode via merge-patch on the singleton PlatformStack. Uses
/// merge-patch (not SSA) for the same reason as `env set` / `target domain`:
/// the path is nested under `spec.resources` which may contain other fields
/// the CLI doesn't own; a scoped merge-patch touches only the leaf.
pub fn autoscale_set(mode: &str) -> Result<()> {
    let mode = validate_autoscale_mode(mode)?;
    let kc = ensure_kubeconfig_tempfile()?;
    let body = autoscale_patch_body(mode);
    kubectl_merge_patch(
        "platformstack",
        PLATFORMSTACK_NAME,
        Some(PLATFORMSTACK_NAMESPACE),
        None,
        &body,
        kc.path(),
    )?;
    println!("✓ Autoscale mode set to '{mode}'.");
    if mode == "off" {
        println!(
            "⚠ off freezes live pods but does NOT restore them: the next deploy/recreation \
             reverts each pod to the platform seed (32Mi). Set explicit `resources` on apps \
             you want to keep at their current sizing."
        );
    }
    println!(
        "Note: if an infra-repo declares spec.resources.autoscale, Argo CD becomes \
         authoritative and wins on the next sync."
    );
    Ok(())
}

/// Best-effort: does any `metadata.managedFields` entry owned by a
/// manager OTHER than `apprafter-cli` whose name looks like Argo CD
/// (`argocd`, `argo-cd-*`, `application-controller`) carry the
/// `spec.network.egress` subtree? Argo CD's managed-fields entry
/// records `f:spec → f:network → f:egress` when the field is
/// git-declared. Conservative: parse failures / absence → `false`
/// (we then fall back to the unconditional advisory in `set`).
fn egress_field_appears_git_managed(json: &Value) -> bool {
    let Some(entries) = json
        .pointer("/metadata/managedFields")
        .and_then(Value::as_array)
    else {
        return false;
    };
    entries.iter().any(|e| {
        let manager = e.get("manager").and_then(Value::as_str).unwrap_or("");
        let is_argo = manager.contains("argocd")
            || manager.contains("argo-cd")
            || manager.contains("application-controller");
        if !is_argo {
            return false;
        }
        e.pointer("/fieldsV1/f:spec/f:network/f:egress").is_some()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    // The bootstrap field manager — only referenced from the
    // distinct-manager regression test below, so it lives here rather than
    // at module scope (where it would be an unused import in non-test builds).
    use cli_providers::k8s::kubectl::APPRAFTER_CLI_FIELD_MANAGER;
    use serde_json::json;

    use chrono::TimeZone;

    // ---- ADR 0059: pinned-application roll-up ----

    #[test]
    fn pinned_app_rows_lists_only_honoured_pins() {
        // The middle app carries a pin ANNOTATION the operator rejected, so
        // it has no `status.image.pinned` and is still following its tag.
        // Listing it would send an operator chasing an application that is
        // not actually held.
        let items = vec![
            json!({ "metadata": { "name": "web", "namespace": "demo" },
                    "status": { "image": { "pinned": { "resolved": "ghcr.io/acme/web@sha256:aaa" }}}}),
            json!({ "metadata": { "name": "api", "namespace": "demo" },
                    "status": { "image": { "tag": "ghcr.io/acme/api:latest" }}}),
            json!({ "metadata": { "name": "worker", "namespace": "other" },
                    "status": { "image": { "pinned": { "resolved": "ghcr.io/acme/worker@sha256:bbb" }}}}),
        ];
        let rows = pinned_app_rows(&items);
        assert_eq!(rows.len(), 2);
        assert!(rows[0].contains("demo/web"), "{:?}", rows);
        assert!(rows[0].contains("sha256:aaa"), "{:?}", rows);
        assert!(rows[1].contains("other/worker"), "{:?}", rows);
    }

    #[test]
    fn pinned_app_rows_is_empty_when_nothing_is_pinned() {
        assert!(pinned_app_rows(&[]).is_empty());
        assert!(pinned_app_rows(&[json!({ "metadata": { "name": "x" }})]).is_empty());
    }

    // ---- 2.22h / D16: the cluster-wide problem roll-up ----

    fn t(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    /// An AppRafter CR carrying `n` problem entries, all at `last_seen`.
    fn cr_with_problems(ns: &str, name: &str, reasons: &[&str], last_seen: &str) -> Value {
        let entries: Vec<Value> = reasons
            .iter()
            .map(|r| {
                json!({ "reason": r, "message": "forbidden: cannot delete resourceclaims",
                        "firstSeen": "2026-09-01T10:00:00+00:00",
                        "lastSeen": last_seen, "count": 7 })
            })
            .collect();
        json!({
            "metadata": { "namespace": ns, "name": name },
            "status": { "recentProblems": entries }
        })
    }

    /// The Argo CD Application that registers `ns/cr_name` as logical `logical`.
    fn argo_app(logical: &str, env: &str, ns: &str, cr_name: &str) -> Value {
        json!({
            "metadata": {
                "name": format!("{logical}-{env}"),
                "labels": { "apprafter.io/application": logical,
                            "apprafter.io/environment": env }
            },
            "spec": { "destination": { "namespace": ns } },
            "status": { "resources": [
                { "group": "apprafter.io", "kind": "Application",
                  "name": cr_name, "namespace": ns, "version": "v1alpha1" }
            ]}
        })
    }

    #[test]
    fn problem_roll_up_names_the_application_the_way_app_status_takes_it() {
        // The row must be typeable into the command it points at. The CR's
        // own metadata.name is author-chosen CUE and is NOT that argument —
        // printing it would send the reader to a command that errors.
        let cr = cr_with_problems(
            "demo",
            "parser-cr",
            &["ClaimPruneFailed"],
            "2026-09-01T10:05:00+00:00",
        );
        let argo = vec![argo_app("parser", "prod", "demo", "parser-cr")];
        let rows = problem_app_rows(&[&cr], &argo, &t("2026-09-01T10:06:00+00:00"));
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert!(rows[0].starts_with("parser (prod)"), "{rows:?}");
        assert!(rows[0].contains("ClaimPruneFailed"), "{rows:?}");
        assert!(rows[0].contains("(now, 7x)"), "{rows:?}");
    }

    #[test]
    fn an_unregistered_application_is_listed_and_labelled_rather_than_dropped() {
        // These are the ones most likely to be broken. Dropping them would
        // make the roll-up quietest exactly where it should be loudest.
        let cr = cr_with_problems(
            "demo",
            "orphan",
            &["ReconcileFailed"],
            "2026-09-01T10:05:00+00:00",
        );
        let rows = problem_app_rows(&[&cr], &[], &t("2026-09-01T10:06:00+00:00"));
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert!(rows[0].starts_with("demo/orphan"), "{rows:?}");
        assert!(rows[0].contains("logical name unresolved"), "{rows:?}");
    }

    /// A CR carrying two entries with DIFFERENT `lastSeen`, in the order the
    /// operator actually writes them: sorted by `firstSeen` ascending, so the
    /// entry that started earliest comes first regardless of what is burning.
    fn cr_two_problems(ns: &str, name: &str) -> Value {
        json!({
            "metadata": { "namespace": ns, "name": name },
            "status": { "recentProblems": [
                { "reason": "StoppedAgesAgo", "message": "this one ended",
                  "firstSeen": "2026-09-01T09:00:00+00:00",
                  "lastSeen": "2026-09-01T09:10:00+00:00", "count": 4 },
                { "reason": "BurningNow", "message": "this one is live",
                  "firstSeen": "2026-09-01T11:30:00+00:00",
                  "lastSeen": "2026-09-01T12:00:00+00:00", "count": 2 }
            ]}
        })
    }

    #[test]
    fn the_row_names_the_live_failure_not_the_one_that_started_first() {
        // The operator writes `recentProblems` sorted by firstSeen ASCENDING
        // (`ProblemLedger::snapshot`), so taking element 0 names whatever
        // broke earliest — here a failure that stopped three hours ago —
        // and buries the live one behind "(+1 more)".
        let cr = cr_two_problems("demo", "web");
        let rows = problem_app_rows(&[&cr], &[], &t("2026-09-01T12:00:00+00:00"));
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert!(rows[0].contains("BurningNow"), "{rows:?}");
        assert!(!rows[0].contains("StoppedAgesAgo"), "{rows:?}");
        assert!(rows[0].contains("(now, 2x)"), "{rows:?}");
    }

    #[test]
    fn ordering_ranks_applications_by_their_freshest_failure() {
        // Same trap one level up: `web` must outrank `other` on the strength
        // of its LIVE entry, not be filed under its oldest one.
        let web = cr_two_problems("demo", "web");
        let other = cr_with_problems("demo", "other", &["X"], "2026-09-01T11:00:00+00:00");
        let rows = problem_app_rows(&[&other, &web], &[], &t("2026-09-01T12:00:00+00:00"));
        assert_eq!(rows.len(), 2, "{rows:?}");
        assert!(rows[0].starts_with("demo/web"), "{rows:?}");
    }

    #[test]
    fn an_application_being_deleted_is_neither_counted_nor_listed() {
        // Its ledger can never be flushed again — the controller evicts and
        // returns before the flush — so any entry it still carries would be a
        // permanent phantom.
        let mut dying = cr_with_problems("demo", "dying", &["X"], "2026-09-01T11:55:00+00:00");
        dying["metadata"]["deletionTimestamp"] = json!("2026-09-01T11:00:00+00:00");
        let healthy = json!({ "metadata": { "namespace": "demo", "name": "ok" }});
        let all = vec![dying, healthy];
        let live = live_applications(&all);
        assert_eq!(live.len(), 1);
        assert!(problem_app_rows(&live, &[], &t("2026-09-01T12:00:00+00:00")).is_empty());
    }

    #[test]
    fn the_heading_states_the_true_total_not_the_number_of_lines_shown() {
        // 13 broken applications, 10 rows printed, "… and 3 more" beneath.
        // A heading saying "10 reporting problems" contradicts that line and
        // understates the incident by three applications.
        let capped = problem_heading(40, 13, 10);
        assert!(capped.contains("13 reporting problems"), "{capped}");
        assert!(capped.contains("showing the 10 most recent"), "{capped}");
        let uncapped = problem_heading(40, 3, 10);
        assert!(uncapped.contains("3 reporting problems"), "{uncapped}");
        assert!(uncapped.contains("most recent first"), "{uncapped}");
    }

    #[test]
    fn many_problems_on_one_application_collapse_to_one_row() {
        // Five reasons on one app must not bury the four other apps that are
        // also broken. The count rides the row; the detail is one command away.
        let cr = cr_with_problems(
            "demo",
            "parser-cr",
            &["A", "B", "C"],
            "2026-09-01T10:05:00+00:00",
        );
        let argo = vec![argo_app("parser", "prod", "demo", "parser-cr")];
        let rows = problem_app_rows(&[&cr], &argo, &t("2026-09-01T10:06:00+00:00"));
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert!(rows[0].contains("(+2 more)"), "{rows:?}");
    }

    #[test]
    fn a_healthy_cluster_yields_no_rows() {
        assert!(problem_app_rows(&[], &[], &t("2026-09-01T10:06:00+00:00")).is_empty());
        let healthy = json!({ "metadata": { "namespace": "demo", "name": "web" }});
        assert!(problem_app_rows(&[&healthy], &[], &t("2026-09-01T10:06:00+00:00")).is_empty());
    }

    #[test]
    fn rows_are_ordered_most_recent_first() {
        let old = cr_with_problems("demo", "old", &["X"], "2026-09-01T08:00:00+00:00");
        let fresh = cr_with_problems("demo", "fresh", &["Y"], "2026-09-01T10:05:00+00:00");
        let rows = problem_app_rows(&[&old, &fresh], &[], &t("2026-09-01T10:06:00+00:00"));
        assert_eq!(rows.len(), 2, "{rows:?}");
        assert!(rows[0].starts_with("demo/fresh"), "{rows:?}");
    }

    #[test]
    fn the_roll_up_and_app_status_never_disagree() {
        // THE invariant. A roll-up whose whole job is to send the reader to
        // `app status` must never name an application whose `app status`
        // prints nothing, and must never stay silent about one that would.
        // Both surfaces share `live_problems`; this asserts the sharing holds
        // through both renderings, across every filter rule.
        let now = t("2026-09-01T12:00:00+00:00");
        let cases = vec![
            // live
            cr_with_problems("demo", "a", &["R"], "2026-09-01T11:55:00+00:00"),
            // past the 24h render horizon
            cr_with_problems("demo", "b", &["R"], "2026-08-30T10:00:00+00:00"),
            // unparseable lastSeen
            cr_with_problems("demo", "c", &["R"], "not-a-timestamp"),
            // no problems array at all
            json!({ "metadata": { "namespace": "demo", "name": "d" }}),
            // present but empty
            json!({ "metadata": { "namespace": "demo", "name": "e" },
                    "status": { "recentProblems": [] }}),
        ];
        for cr in &cases {
            let in_app_status = !crate::commands::app::format_problem_lines(cr, &now).is_empty();
            let in_roll_up = !problem_app_rows(&[cr], &[], &now).is_empty();
            assert_eq!(
                in_app_status,
                in_roll_up,
                "surfaces disagree for {:?}",
                cr.pointer("/metadata/name")
            );
        }
    }

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
    fn resolve_effective_pin_prefers_platformstack_status() {
        // Both sources present — PlatformStack status wins
        // because it's the operator's canonical version dial
        // and the Argo CD targetRevision may lag during a
        // bump cycle (in-flight reconcile shows the OLD
        // version on app.spec.source.targetRevision until
        // the umbrella patches it).
        let stack = json!({
            "status": { "componentVersions": { "cilium": "1.16.5" } }
        });
        let app = json!({
            "spec": { "source": { "targetRevision": "1.15.0" } }
        });
        let pin = resolve_effective_pin(&stack, Some(&app), "cilium").unwrap();
        assert_eq!(pin, "1.16.5");
    }

    #[test]
    fn resolve_effective_pin_falls_back_to_argocd_target_revision() {
        // Operator hits `freeze` on a cluster where the
        // operator binary doesn't (yet) write
        // componentVersions — this is the M1.5 default state.
        // The Argo CD Application's targetRevision is THE
        // authoritative version Argo CD is actively
        // reconciling against, so fall back to it instead
        // of erroring.
        let stack = json!({ "status": {} });
        let app = json!({
            "spec": { "source": { "targetRevision": "v1.16.5" } }
        });
        let pin = resolve_effective_pin(&stack, Some(&app), "cilium").unwrap();
        assert_eq!(pin, "v1.16.5");
    }

    #[test]
    fn resolve_effective_pin_errors_when_both_sources_empty() {
        // Neither PlatformStack.status nor Argo CD Application
        // carries a version — likely an unknown component
        // name OR a half-bootstrapped cluster. Error message
        // must point operators at the `--version <v>` escape
        // hatch and `platform status` for the canonical
        // component list.
        let stack = json!({ "status": {} });
        let err = resolve_effective_pin(&stack, None, "ghost-component")
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("ghost-component"),
            "must surface component name: {err}"
        );
        assert!(err.contains("--version"), "must hint at --version: {err}");
        assert!(
            err.contains("platform status"),
            "must hint at platform status: {err}"
        );
    }

    #[test]
    fn resolve_effective_pin_handles_argocd_app_without_target_revision() {
        // Defensive: malformed Argo CD CR (e.g. mid-edit
        // with empty spec.source) is treated as "no signal"
        // — equivalent to passing None.
        let stack = json!({ "status": {} });
        let app = json!({ "spec": { "source": {} } });
        assert!(resolve_effective_pin(&stack, Some(&app), "x").is_err());
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

    #[test]
    fn validate_egress_profile_accepts_three_and_rejects_other() {
        // 2.10: the only valid presets are internet|internal|strict
        // (mirrors the webhook enum). Anything else — e.g. "open" —
        // is rejected client-side with a clear message rather than
        // degrading into an apiserver/webhook rejection.
        assert!(validate_egress_profile("internet").is_ok());
        assert!(validate_egress_profile("internal").is_ok());
        assert!(validate_egress_profile("strict").is_ok());

        let err = validate_egress_profile("open").unwrap_err().to_string();
        assert!(err.contains("open"), "must echo the bad value: {err}");
        assert!(
            err.contains("internet") && err.contains("internal") && err.contains("strict"),
            "must list the valid presets: {err}"
        );
    }

    #[test]
    fn format_egress_profile_reports_explicit_value() {
        let obj = json!({
            "spec": { "network": { "egress": { "profile": "strict" } } }
        });
        let s = format_egress_profile(&obj);
        assert!(s.contains("strict"), "must surface the set profile: {s}");
        // The legend lists all three presets regardless of which is active.
        assert!(s.contains("internet"), "legend must list internet: {s}");
        assert!(s.contains("internal"), "legend must list internal: {s}");
        assert!(s.contains("needs"), "legend must mention needs: {s}");
    }

    #[test]
    fn format_egress_profile_falls_back_to_internet_default_when_unset() {
        // Field absent (the common case — CLI-bootstrap-seeded CR
        // ships without the field) → report the documented operator
        // default `internet`, flagged as unset so it's not mistaken
        // for an explicit set.
        let obj = json!({ "spec": {} });
        let s = format_egress_profile(&obj);
        assert!(s.contains("internet"), "must default to internet: {s}");
        assert!(
            s.to_lowercase().contains("default") || s.to_lowercase().contains("unset"),
            "must flag the value as the unset default: {s}"
        );
    }

    #[test]
    fn egress_field_git_managed_detects_argocd_owner() {
        // Argo CD owns the egress subtree → git-managed.
        let owned_by_argo = json!({
            "metadata": {
                "managedFields": [
                    {
                        "manager": "argocd-application-controller",
                        "fieldsV1": { "f:spec": { "f:network": { "f:egress": {} } } }
                    }
                ]
            }
        });
        assert!(egress_field_appears_git_managed(&owned_by_argo));
    }

    #[test]
    fn egress_field_git_managed_false_when_only_cli_owns_or_absent() {
        // apprafter-cli's own SSA ownership must NOT count as
        // git-managed (else `set` would always warn after the
        // first run). And a CR with no managedFields at all → false.
        let owned_by_cli = json!({
            "metadata": {
                "managedFields": [
                    {
                        "manager": "apprafter-cli",
                        "fieldsV1": { "f:spec": { "f:network": { "f:egress": {} } } }
                    }
                ]
            }
        });
        assert!(!egress_field_appears_git_managed(&owned_by_cli));
        assert!(!egress_field_appears_git_managed(
            &json!({ "metadata": {} })
        ));
        // Argo CD owns OTHER fields but not egress → false.
        let argo_other = json!({
            "metadata": {
                "managedFields": [
                    {
                        "manager": "argocd-application-controller",
                        "fieldsV1": { "f:spec": { "f:channel": {} } }
                    }
                ]
            }
        });
        assert!(!egress_field_appears_git_managed(&argo_other));
    }

    #[test]
    fn recheck_annotation_patch_body_is_well_formed_merge_patch() {
        // The body must be a valid RFC-7396 merge patch nesting the
        // request timestamp under metadata.annotations[<annotation>].
        let ts = "2026-06-11T12:00:00+00:00";
        let body = recheck_annotation_patch_body(ts);
        let parsed: Value = serde_json::from_str(&body).expect("valid JSON");
        assert_eq!(
            parsed.pointer(&format!(
                "/metadata/annotations/{}",
                RECHECK_REQUESTED_ANNOTATION.replace('/', "~1")
            )),
            Some(&Value::String(ts.to_string())),
            "body must stamp the request ts under the recheck annotation: {body}"
        );
    }

    #[test]
    fn recheck_completed_true_only_when_last_check_strictly_after_request() {
        let requested = Utc.with_ymd_and_hms(2026, 6, 11, 12, 0, 0).unwrap();

        // Strictly after → the operator polled in response → done.
        assert!(recheck_completed(Some("2026-06-11T12:00:01Z"), requested));
        // A minute later, different-but-equivalent zone form → done.
        assert!(recheck_completed(
            Some("2026-06-11T13:01:00+01:00"),
            requested
        ));
    }

    #[test]
    fn recheck_completed_false_when_stale_equal_absent_or_unparseable() {
        let requested = Utc.with_ymd_and_hms(2026, 6, 11, 12, 0, 0).unwrap();

        // Older than the request — pre-existing 6h-cadence value the
        // operator hasn't refreshed yet.
        assert!(!recheck_completed(Some("2026-06-11T11:59:59Z"), requested));
        // Exactly equal must NOT count (operator stamps strictly later).
        assert!(!recheck_completed(Some("2026-06-11T12:00:00Z"), requested));
        // No lastUpstreamCheck at all (fresh CR / pre-contract operator).
        assert!(!recheck_completed(None, requested));
        // Mid-write garbage reads as not-yet-done, not a crash.
        assert!(!recheck_completed(Some("not-a-timestamp"), requested));
    }

    #[test]
    fn validate_env_trims_and_accepts() {
        assert_eq!(validate_env("staging").unwrap(), "staging");
        assert_eq!(validate_env("  prod ").unwrap(), "prod");
    }

    #[test]
    fn validate_env_rejects_empty_and_whitespace() {
        assert!(validate_env("").is_err());
        assert!(validate_env("   ").is_err());
    }

    #[test]
    fn default_environment_patch_body_shape() {
        let body = default_environment_patch_body("staging");
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["spec"]["defaultEnvironment"], "staging");
        assert_eq!(v["spec"].as_object().unwrap().len(), 1);
    }

    #[test]
    fn egress_set_field_manager_is_distinct_from_bootstrap_and_not_argo_shaped() {
        // Walk-found regression (2.10 Phase 7): `cluster-bootstrap` seeds the
        // singleton PlatformStack (with the REQUIRED spec.source + spec.values)
        // under APPRAFTER_CLI_FIELD_MANAGER. `egress set` applies ONLY
        // {spec.network.egress.profile}; if it shared that manager, server-side
        // apply would PRUNE source/values and the apiserver would reject the CR
        // ("Required value"). The two managers MUST differ.
        assert_ne!(
            APPRAFTER_CLI_EGRESS_FIELD_MANAGER, APPRAFTER_CLI_FIELD_MANAGER,
            "egress set must not reuse the bootstrap field manager (SSA would prune source/values)"
        );
        // And the egress manager must NOT look Argo-CD-shaped, or
        // egress_field_appears_git_managed would mis-flag it as git-owned and
        // every `set` would print the spurious "git wins" advisory.
        for needle in ["argocd", "argo-cd", "application-controller"] {
            assert!(
                !APPRAFTER_CLI_EGRESS_FIELD_MANAGER.contains(needle),
                "egress field manager must not contain Argo-shaped substring {needle:?}"
            );
        }
    }

    #[test]
    fn autoscale_patch_body_is_path_scoped() {
        // The merge-patch must touch ONLY the autoscale.mode leaf — a
        // shallow `spec: { mode }` body would clobber other spec fields
        // on the PlatformStack that the CLI doesn't own.
        assert_eq!(
            autoscale_patch_body("up-only"),
            r#"{"spec":{"resources":{"autoscale":{"mode":"up-only"}}}}"#
        );
    }

    #[test]
    fn autoscale_validates_mode() {
        // All three documented presets must pass; anything else is
        // rejected client-side before touching the apiserver.
        assert!(validate_autoscale_mode("full").is_ok());
        assert!(validate_autoscale_mode("up-only").is_ok());
        assert!(validate_autoscale_mode("off").is_ok());
        assert!(validate_autoscale_mode("bogus").is_err());
        // Error must echo the bad value.
        let err = validate_autoscale_mode("auto").unwrap_err().to_string();
        assert!(err.contains("auto"), "must echo bad value: {err}");
        assert!(
            err.contains("full") && err.contains("up-only") && err.contains("off"),
            "must list valid presets: {err}"
        );
    }
}
