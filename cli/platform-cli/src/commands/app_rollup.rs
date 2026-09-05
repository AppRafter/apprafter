// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The cluster-wide application roll-up: which applications are
//! reporting problems, and which are held at an image digest.
//!
//! # Why this is its own module (2.23a)
//!
//! Both roll-ups were built in `commands::platform` because
//! `platform status` was, at the time, the only command with a
//! cluster-wide view. That is a reason to have built them, and never
//! was a reason to file them under the `platform` verb: neither reads
//! the PlatformStack. They are driven by a cluster-wide
//! `application.apprafter.io` list and an Argo CD list, and they answer
//! a question about workloads.
//!
//! `apprafter platform status` now describes the platform stack and
//! nothing else; `apprafter status` composes this module with the
//! platform's version and condition summary into the one place a reader
//! asks "is anything wrong?".
//!
//! Nothing here renders differently from the way it rendered under
//! `platform status`. The move is a move.

use chrono::{DateTime, Utc};
use cli_core::{CliError, Result};
use serde_json::Value;

use crate::commands::k8s_helpers::kubectl_get_json_by_selector;

/// Most problem rows printed before the tail is summarised. A roll-up that
/// scrolls the terminal at exactly the moment something is wrong is one the
/// reader stops using.
const PROBLEM_ROW_CAP: usize = 10;

/// The two cluster-wide reads both roll-ups share, taken ONCE.
///
/// One read, not two: the pinned list and the problem list are printed
/// as one report, and two independent fetches would let the two
/// sections describe two different instants — and would double the cost
/// on a large cluster for nothing.
///
/// Each side keeps its own `Result` rather than being unwrapped here,
/// because the two sections must fail differently: the pinned section
/// is decorative and stays silent, and the problem section must NOT
/// (see [`print_problem_applications`]).
pub(crate) struct ClusterApplications {
    pub(crate) apps: Result<Vec<Value>>,
    pub(crate) argo: Result<Vec<Value>>,
}

impl ClusterApplications {
    /// Read both lists through an already-resolved kubeconfig path.
    pub(crate) fn read(kubeconfig: &std::path::Path) -> Self {
        Self {
            apps: kubectl_get_json_by_selector("application.apprafter.io", "", None, kubeconfig),
            argo: kubectl_get_json_by_selector(
                "application.argoproj.io",
                "",
                Some(crate::commands::app::ARGOCD_NAMESPACE),
                kubeconfig,
            ),
        }
    }
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
pub(crate) fn print_problem_applications(
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
pub(crate) fn live_applications(crs: &[Value]) -> Vec<&Value> {
    crs.iter()
        .filter(|c| c.pointer("/metadata/deletionTimestamp").is_none())
        .collect()
}

/// List every application currently held at an image digest (ADR 0059).
///
/// Best-effort and silent on failure: this is a decorative section beside a
/// health signal, and an unreadable application list must not turn the
/// command into an error. The health signal it sits next to is the one that
/// speaks up about a failed read.
///
/// It exists because a pin is invisible to a reader of the Git repository, so
/// without a cluster-wide view an operator would have to run `app status` per
/// application to discover which ones have stopped receiving builds.
pub(crate) fn print_pinned_applications(items: &[Value]) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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
        //
        // It moved here from `commands::platform` with the code it guards
        // (2.23a). The guarantee is unchanged: what it proves is that the
        // roll-up and `app status` read the same filter, and that is a
        // property of these two renderings, not of the module they sit in.
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
}
