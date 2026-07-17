// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Format-aware retention prune PLANNER for the backup repo
//! (spec §Retention M-r3-1b).
//!
//! A backup repo holds two snapshot shapes:
//!
//! * A **monolithic** run = ONE snapshot that carries the dumps + `manifest.json`.
//! * A **sequential** run = SEVERAL snapshots sharing one `run-<id>` tag, where
//!   exactly one (the commit point) carries `manifest.json` and the others (the
//!   per-claim dumps) do not.
//!
//! Raw `restic forget --keep-daily N` on individual snapshots would rotate a
//! sequential run inconsistently (keep the manifest, drop a claim snapshot →
//! broken restore; or leave orphans). So retention must operate on RUNS,
//! deleting whole run-sets: it applies the keep policy to one REPRESENTATIVE
//! per run (the manifest-bearing snapshot) and forgets every member of a run
//! whose representative is not kept. An interrupted sequential run (no manifest
//! member at all — an ORPHAN) is swept entirely.
//!
//! [`plan_prune`] is the pure, deterministic JUDGMENT (what to forget);
//! [`run_prune`] is the thin impure executor that derives the snapshot metadata
//! from `restic snapshots --json`, calls [`plan_prune`], and runs
//! `restic forget --prune` on the resulting id set.

use std::collections::BTreeMap;

use cli_core::{CliError, Result};
use serde_json::Value;

use crate::restic::{restic_forget_argv, restic_snapshots_argv};

/// Metadata for one restic snapshot (as [`run_prune`] derives from
/// `restic snapshots --json`).
#[derive(Clone, Debug)]
pub struct SnapshotMeta {
    pub id: String,
    /// The shared `run-<id>` tag (== the backup tag).
    pub run_tag: String,
    /// RFC-3339; lexicographically sortable.
    pub time: String,
    /// True iff this snapshot carries `manifest.json` (the run representative).
    pub is_manifest: bool,
}

/// How many representatives to keep per calendar day / ISO week / calendar month.
#[derive(Clone, Copy, Debug)]
pub struct RetentionPolicy {
    pub keep_daily: u32,
    pub keep_weekly: u32,
    pub keep_monthly: u32,
}

impl Default for RetentionPolicy {
    fn default() -> Self {
        Self {
            keep_daily: 7,
            keep_weekly: 4,
            keep_monthly: 6,
        }
    }
}

/// The set of snapshot ids to `restic forget`.
#[derive(Debug, PartialEq)]
pub struct PrunePlan {
    pub forget_ids: Vec<String>,
}

/// A run: its representative (if any) plus every member snapshot id.
struct Run {
    /// The manifest-bearing snapshot, if the run has one. `None` ⇒ orphan.
    representative: Option<SnapshotMeta>,
    /// Every snapshot id in the run (representative + members).
    ids: Vec<String>,
}

/// Decide which snapshot ids to forget, format-aware (spec §Retention M-r3-1b).
///
/// 1. Group snapshots by `run_tag`.
/// 2. Each group's REPRESENTATIVE is its `is_manifest == true` member. A group
///    with NO manifest member is an ORPHAN (an interrupted sequential run) →
///    ALL its snapshot ids are forgotten. (A monolithic run is a single
///    snapshot with `is_manifest == true` → it is its own representative.)
/// 3. Apply the keep policy to the set of representatives by `time`: keep the
///    newest representative per distinct calendar day up to `keep_daily`, per
///    distinct ISO week up to `keep_weekly`, per distinct calendar month up to
///    `keep_monthly`. A representative kept by ANY of the three is kept (union).
/// 4. For every representative NOT kept → forget ALL its group's snapshot ids.
/// 5. Representatives that ARE kept → keep all their group's members.
///
/// Pure + deterministic. Empty input ⇒ empty `forget_ids` (never panics).
pub fn plan_prune(snapshots: &[SnapshotMeta], policy: &RetentionPolicy) -> PrunePlan {
    // 1. Group by run_tag. BTreeMap keeps grouping deterministic; the final
    //    forget_ids order is derived from a stable sort below regardless.
    let mut runs: BTreeMap<String, Run> = BTreeMap::new();
    for s in snapshots {
        let run = runs.entry(s.run_tag.clone()).or_insert_with(|| Run {
            representative: None,
            ids: Vec::new(),
        });
        run.ids.push(s.id.clone());
        if s.is_manifest {
            // If (pathologically) more than one member claims to be the
            // manifest, the newest by time wins as the representative — its
            // `time` is what the keep policy buckets on.
            match &run.representative {
                Some(cur) if cur.time >= s.time => {}
                _ => run.representative = Some(s.clone()),
            }
        }
    }

    let mut forget_ids: Vec<String> = Vec::new();

    // 2. Orphans (no manifest member) → forget the whole set.
    // Collect the representatives of complete runs for the keep policy.
    let mut representatives: Vec<(&SnapshotMeta, &Vec<String>)> = Vec::new();
    for run in runs.values() {
        match &run.representative {
            None => forget_ids.extend(run.ids.iter().cloned()),
            Some(rep) => representatives.push((rep, &run.ids)),
        }
    }

    // 3. Apply the keep policy to the representatives.
    let kept = select_kept(&representatives, policy);

    // 4. Forget every group whose representative was not kept.
    for (rep, ids) in &representatives {
        if !kept.contains(&rep.id) {
            forget_ids.extend(ids.iter().cloned());
        }
    }

    // Deterministic output: sort + dedup (a snapshot id appears once).
    forget_ids.sort();
    forget_ids.dedup();

    PrunePlan { forget_ids }
}

/// Return the set of representative ids kept by the union of the three buckets.
///
/// For each bucket kind, walk the representatives newest-first and keep the
/// newest one per distinct period (day / ISO-week / month) until `keep_*`
/// distinct periods have been kept. A representative kept by ANY bucket is kept.
fn select_kept(
    representatives: &[(&SnapshotMeta, &Vec<String>)],
    policy: &RetentionPolicy,
) -> std::collections::HashSet<String> {
    use std::collections::HashSet;

    // Newest-first; ties broken by id for determinism.
    let mut reps: Vec<&SnapshotMeta> = representatives.iter().map(|(r, _)| *r).collect();
    reps.sort_by(|a, b| b.time.cmp(&a.time).then_with(|| a.id.cmp(&b.id)));

    let mut kept: HashSet<String> = HashSet::new();

    keep_by_period(&reps, policy.keep_daily, period_day, &mut kept);
    keep_by_period(&reps, policy.keep_weekly, period_week, &mut kept);
    keep_by_period(&reps, policy.keep_monthly, period_month, &mut kept);

    kept
}

/// Keep the newest representative per distinct period, up to `keep` periods.
///
/// `reps` MUST already be sorted newest-first. Adds kept ids into `kept`.
fn keep_by_period(
    reps: &[&SnapshotMeta],
    keep: u32,
    period_of: fn(&str) -> Option<String>,
    kept: &mut std::collections::HashSet<String>,
) {
    if keep == 0 {
        return;
    }
    let mut seen_periods: Vec<String> = Vec::new();
    for rep in reps {
        let Some(period) = period_of(&rep.time) else {
            continue; // unparseable time — never keep it by this bucket
        };
        if seen_periods.contains(&period) {
            continue; // already kept the newest representative in this period
        }
        if seen_periods.len() as u32 >= keep {
            break; // kept enough distinct periods for this bucket
        }
        seen_periods.push(period);
        kept.insert(rep.id.clone());
    }
}

/// Calendar-day period key `YYYY-MM-DD` from an RFC-3339 time.
fn period_day(time: &str) -> Option<String> {
    parse_date(time).map(|(y, m, d)| format!("{y:04}-{m:02}-{d:02}"))
}

/// Calendar-month period key `YYYY-MM` from an RFC-3339 time.
fn period_month(time: &str) -> Option<String> {
    parse_date(time).map(|(y, m, _d)| format!("{y:04}-{m:02}"))
}

/// ISO-8601 week period key `YYYY-Www` from an RFC-3339 time (chrono
/// `IsoWeek` — correct ISO-week-year + week-number, so a run on 2026-12-31 and
/// one on 2027-01-01 land in the same ISO week when they should).
fn period_week(time: &str) -> Option<String> {
    use chrono::Datelike;
    let dt = chrono::DateTime::parse_from_rfc3339(time).ok()?;
    let iso = dt.date_naive().iso_week();
    Some(format!("{:04}-W{:02}", iso.year(), iso.week()))
}

/// Parse `(year, month, day)` from an RFC-3339 time. Uses chrono for a robust
/// parse (offsets, fractional seconds, `Z`), falling back to nothing on error.
fn parse_date(time: &str) -> Option<(i32, u32, u32)> {
    use chrono::Datelike;
    let dt = chrono::DateTime::parse_from_rfc3339(time).ok()?;
    let d = dt.date_naive();
    Some((d.year(), d.month(), d.day()))
}

// ---------------------------------------------------------------------------
// Executor (impure) — thin restic interaction around the pure planner.
// ---------------------------------------------------------------------------

/// `restic snapshots --json` → derive [`SnapshotMeta`] → [`plan_prune`] →
/// `restic forget --prune` the resulting id set.
///
/// The JUDGMENT (what to forget) is entirely in the pure [`plan_prune`]; this
/// only marshals restic I/O. A no-op prune (nothing to forget) skips the
/// `forget` call entirely.
pub fn run_prune(
    r: &dyn crate::ResticRunner,
    repo: &str,
    pass: &str,
    policy: &RetentionPolicy,
) -> Result<()> {
    let json = r.run_stdout(&restic_snapshots_argv(repo), pass)?;
    let snapshots = parse_snapshots(&json)?;
    let plan = plan_prune(&snapshots, policy);
    if plan.forget_ids.is_empty() {
        return Ok(());
    }
    r.run(&restic_forget_argv(repo, &plan.forget_ids), pass)?;
    Ok(())
}

/// Parse `restic snapshots --json` (an array of snapshot objects) into
/// [`SnapshotMeta`], deriving `run_tag` + `is_manifest` (see [`derive_manifest`]).
fn parse_snapshots(json: &str) -> Result<Vec<SnapshotMeta>> {
    let value: Value = serde_json::from_str(json)
        .map_err(|e| CliError::Other(format!("parse restic snapshots JSON: {e}")))?;
    let arr = value.as_array().cloned().unwrap_or_default();

    // First pass: id, run_tag, time, paths.
    struct Raw {
        id: String,
        run_tag: String,
        time: String,
        paths: Vec<String>,
    }
    let mut raws: Vec<Raw> = Vec::new();
    for s in &arr {
        let id = s
            .pointer("/id")
            .or_else(|| s.pointer("/short_id"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        if id.is_empty() {
            continue; // a snapshot with no id can't be forgotten by id
        }
        // Each backup snapshot carries exactly one `--tag` (the shared run tag);
        // take the first tag as the run_tag.
        let run_tag = s
            .pointer("/tags")
            .and_then(Value::as_array)
            .and_then(|t| t.iter().filter_map(Value::as_str).next())
            .unwrap_or_default()
            .to_string();
        let time = s
            .pointer("/time")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let paths = s
            .pointer("/paths")
            .and_then(Value::as_array)
            .map(|p| {
                p.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        raws.push(Raw {
            id,
            run_tag,
            time,
            paths,
        });
    }

    // Count snapshots per run_tag so a lone snapshot in its group (a monolithic
    // run) is a representative even though its path is `.../data` not
    // `.../commit`.
    let mut group_size: BTreeMap<String, usize> = BTreeMap::new();
    for r in &raws {
        *group_size.entry(r.run_tag.clone()).or_default() += 1;
    }

    Ok(raws
        .into_iter()
        .map(|r| {
            let alone = group_size.get(&r.run_tag).copied().unwrap_or(1) == 1;
            SnapshotMeta {
                is_manifest: derive_manifest(&r.paths, alone),
                id: r.id,
                run_tag: r.run_tag,
                time: r.time,
            }
        })
        .collect())
}

/// Derive `is_manifest` for a snapshot from its restic `paths` + whether it is
/// the ONLY snapshot in its run_tag group.
///
/// The engine (T9) stages a monolithic run under `<staging>/data`, a sequential
/// per-claim snapshot under `<staging>/claim-<i>`, and the sequential commit
/// (manifest) snapshot under `<staging>/commit`. So the robust rule is:
///
/// * A snapshot ALONE in its run_tag group is a monolithic run → representative.
/// * In a MULTI-snapshot group, the representative is the one whose path is the
///   commit/manifest dir (basename `commit`); the per-claim (`claim-<i>`)
///   snapshots are not.
///
/// `alone == true` short-circuits to the monolithic case regardless of path.
fn derive_manifest(paths: &[String], alone: bool) -> bool {
    if alone {
        return true;
    }
    paths.iter().any(|p| {
        let base = p.trim_end_matches('/').rsplit('/').next().unwrap_or(p);
        base == "commit"
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a sequential run: `claims` per-claim snapshots (is_manifest:false)
    /// plus 1 manifest/commit snapshot (is_manifest:true), all sharing `tag`,
    /// at `{day}T03:00:00Z`.
    fn seq_run(tag: &str, day: &str, claims: usize) -> Vec<SnapshotMeta> {
        let time = format!("{day}T03:00:00Z");
        let mut out = Vec::new();
        for i in 0..claims {
            out.push(SnapshotMeta {
                id: format!("{tag}-claim-{i}"),
                run_tag: tag.to_string(),
                time: time.clone(),
                is_manifest: false,
            });
        }
        out.push(SnapshotMeta {
            id: format!("{tag}-manifest"),
            run_tag: tag.to_string(),
            time,
            is_manifest: true,
        });
        out
    }

    /// Build a monolithic run: one snapshot (is_manifest:true) at `{day}T03:00:00Z`.
    fn mono_run(tag: &str, day: &str) -> Vec<SnapshotMeta> {
        vec![SnapshotMeta {
            id: format!("{tag}-mono"),
            run_tag: tag.to_string(),
            time: format!("{day}T03:00:00Z"),
            is_manifest: true,
        }]
    }

    #[test]
    fn keeps_recent_runs_and_drops_whole_run_sets() {
        // 3 sequential daily runs (2 claims each) on 3 distinct days;
        // keep_daily 2, keep_weekly 0, keep_monthly 0.
        let mut snaps = Vec::new();
        snaps.extend(seq_run("run-a", "2026-07-10", 2)); // oldest
        snaps.extend(seq_run("run-b", "2026-07-11", 2));
        snaps.extend(seq_run("run-c", "2026-07-12", 2)); // newest
        let policy = RetentionPolicy {
            keep_daily: 2,
            keep_weekly: 0,
            keep_monthly: 0,
        };
        let plan = plan_prune(&snaps, &policy);
        // The OLDEST run's ALL THREE snapshot ids are forgotten.
        let mut expected = vec![
            "run-a-claim-0".to_string(),
            "run-a-claim-1".to_string(),
            "run-a-manifest".to_string(),
        ];
        expected.sort();
        assert_eq!(plan.forget_ids, expected);
        // The two newest runs' snapshots are NOT forgotten.
        for id in [
            "run-b-claim-0",
            "run-b-claim-1",
            "run-b-manifest",
            "run-c-claim-0",
            "run-c-claim-1",
            "run-c-manifest",
        ] {
            assert!(
                !plan.forget_ids.contains(&id.to_string()),
                "recent run snapshot {id} must be kept, got {:?}",
                plan.forget_ids
            );
        }
    }

    #[test]
    fn monolithic_runs_pruned_by_the_same_representative_policy() {
        // 3 monolithic runs on 3 days, keep_daily 2 → the oldest single
        // snapshot id is forgotten.
        let mut snaps = Vec::new();
        snaps.extend(mono_run("m-a", "2026-07-10")); // oldest
        snaps.extend(mono_run("m-b", "2026-07-11"));
        snaps.extend(mono_run("m-c", "2026-07-12")); // newest
        let policy = RetentionPolicy {
            keep_daily: 2,
            keep_weekly: 0,
            keep_monthly: 0,
        };
        let plan = plan_prune(&snaps, &policy);
        assert_eq!(plan.forget_ids, vec!["m-a-mono".to_string()]);
    }

    #[test]
    fn orphan_set_without_a_manifest_is_swept_entirely() {
        // One run_tag group of 2 claim snapshots, both is_manifest:false → an
        // interrupted run: both ids forgotten regardless of policy.
        let snaps = vec![
            SnapshotMeta {
                id: "orphan-0".into(),
                run_tag: "run-x".into(),
                time: "2026-07-12T03:00:00Z".into(),
                is_manifest: false,
            },
            SnapshotMeta {
                id: "orphan-1".into(),
                run_tag: "run-x".into(),
                time: "2026-07-12T03:00:00Z".into(),
                is_manifest: false,
            },
        ];
        // Even a generous policy sweeps the orphan (no representative to keep).
        let plan = plan_prune(&snaps, &RetentionPolicy::default());
        assert_eq!(
            plan.forget_ids,
            vec!["orphan-0".to_string(), "orphan-1".to_string()]
        );
    }

    #[test]
    fn default_policy_is_7_4_6() {
        assert_eq!(RetentionPolicy::default().keep_daily, 7);
        assert_eq!(RetentionPolicy::default().keep_weekly, 4);
        assert_eq!(RetentionPolicy::default().keep_monthly, 6);
    }

    // --- extra coverage (not required, but guards the trickier corners) ---

    #[test]
    fn empty_input_is_a_no_op() {
        let plan = plan_prune(&[], &RetentionPolicy::default());
        assert!(plan.forget_ids.is_empty());
    }

    #[test]
    fn weekly_and_monthly_buckets_union_extends_retention() {
        // Two runs in the same ISO week but on different days; keep_daily 1,
        // keep_weekly 1 → daily keeps the newest, weekly also keeps that same
        // newest (same week) → the older is forgotten.
        let mut snaps = Vec::new();
        snaps.extend(mono_run("w-old", "2026-07-13")); // Mon, ISO 2026-W29
        snaps.extend(mono_run("w-new", "2026-07-15")); // Wed, same ISO week
        let policy = RetentionPolicy {
            keep_daily: 1,
            keep_weekly: 1,
            keep_monthly: 0,
        };
        let plan = plan_prune(&snaps, &policy);
        assert_eq!(plan.forget_ids, vec!["w-old-mono".to_string()]);

        // Now a monthly bucket rescues an older run in a different month.
        let mut snaps = Vec::new();
        snaps.extend(mono_run("prev-month", "2026-06-15")); // June
        snaps.extend(mono_run("this-month", "2026-07-15")); // July
        let policy = RetentionPolicy {
            keep_daily: 1,
            keep_weekly: 0,
            keep_monthly: 2,
        };
        let plan = plan_prune(&snaps, &policy);
        // daily keeps newest (July); monthly keeps newest-per-month for 2 months
        // → both months kept → nothing forgotten.
        assert!(
            plan.forget_ids.is_empty(),
            "monthly bucket must rescue the June run, got {:?}",
            plan.forget_ids
        );
    }

    #[test]
    fn zero_policy_keeps_nothing_and_forgets_every_complete_run() {
        // keep_daily/weekly/monthly all 0 → no representative kept → every run
        // (complete or orphan) is forgotten.
        let mut snaps = Vec::new();
        snaps.extend(mono_run("m-a", "2026-07-10"));
        snaps.extend(seq_run("s-b", "2026-07-11", 2));
        let policy = RetentionPolicy {
            keep_daily: 0,
            keep_weekly: 0,
            keep_monthly: 0,
        };
        let plan = plan_prune(&snaps, &policy);
        let mut expected = vec![
            "m-a-mono".to_string(),
            "s-b-claim-0".to_string(),
            "s-b-claim-1".to_string(),
            "s-b-manifest".to_string(),
        ];
        expected.sort();
        assert_eq!(plan.forget_ids, expected);
    }

    // --- is_manifest derivation (run_prune's impure seam) ---

    #[test]
    fn derive_manifest_lone_snapshot_is_representative_any_path() {
        // A monolithic run (alone in its group) is a representative even under
        // the `.../data` staging path.
        assert!(derive_manifest(&["/stage/data".into()], true));
    }

    #[test]
    fn derive_manifest_commit_dir_is_representative_in_multi_group() {
        assert!(derive_manifest(&["/stage/commit".into()], false));
        assert!(derive_manifest(&["/stage/commit/".into()], false));
        // A per-claim snapshot in a multi-snapshot group is NOT a representative.
        assert!(!derive_manifest(&["/stage/claim-0".into()], false));
        assert!(!derive_manifest(&["/stage/claim-1".into()], false));
    }

    #[test]
    fn parse_snapshots_derives_run_tag_and_manifest() {
        // A sequential run: one claim snapshot + one commit snapshot share a tag.
        let json = r#"[
            {"id":"aaa","time":"2026-07-12T03:00:00Z","tags":["run-t"],"paths":["/s/claim-0"]},
            {"id":"bbb","time":"2026-07-12T03:00:05Z","tags":["run-t"],"paths":["/s/commit"]},
            {"id":"ccc","time":"2026-07-11T03:00:00Z","tags":["run-mono"],"paths":["/s/data"]}
        ]"#;
        let metas = parse_snapshots(json).expect("parse ok");
        let by_id = |id: &str| metas.iter().find(|m| m.id == id).unwrap().clone();
        assert_eq!(by_id("aaa").run_tag, "run-t");
        assert!(!by_id("aaa").is_manifest, "claim snapshot is not a rep");
        assert!(by_id("bbb").is_manifest, "commit snapshot is the rep");
        // A lone snapshot in its group is a representative even under `.../data`.
        assert!(
            by_id("ccc").is_manifest,
            "monolithic snapshot is its own rep"
        );
    }

    #[test]
    fn parse_snapshots_empty_array_is_empty() {
        assert!(parse_snapshots("[]").unwrap().is_empty());
    }
}
