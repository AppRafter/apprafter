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
//! member at all — an ORPHAN) is swept entirely, once it is old enough that no
//! backup can still be writing it ([`unfinished_run_window`]).
//!
//! [`plan_prune`] is the pure, deterministic JUDGMENT (what to forget);
//! [`run_prune`] is the thin impure executor that derives the snapshot metadata
//! from `restic snapshots --json`, calls [`plan_prune`], forgets the resulting
//! id set, checks that it is gone, and only then runs `restic prune`.

use std::collections::BTreeMap;
use std::time::Duration;

use chrono::{DateTime, Datelike, NaiveDate, Utc};
/// The zone type of [`RetentionPolicy::zone`], re-exported so that a caller
/// names a zone without a dependency of its own on the zone database.
pub use chrono_tz::Tz;
use cli_core::{CliError, Result};
use serde_json::Value;

use crate::restic::{
    restic_forget_argv, restic_prune_argv, restic_snapshots_argv, snapshot_instant,
};

/// How long past a run's newest snapshot, beyond the backup deadline, a run
/// with no manifest is still left alone ([`unfinished_run_window`]): the pod's
/// 90 s grace period after the deadline, and room for two nodes' clocks to
/// disagree.
pub const UNFINISHED_RUN_MARGIN: Duration = Duration::from_secs(3600);

/// How long after its newest snapshot was written — the end of that
/// snapshot's `restic backup` ([`SnapshotMeta::ended`]), or its start when
/// restic did not record the end — a run with no manifest may still be
/// being written, given the cluster's backup run deadline
/// (`spec.backup.activeDeadlineSeconds`): that deadline, never less than six
/// hours ([`crate::helper_pod::helper_keep_alive`]), plus
/// [`UNFINISHED_RUN_MARGIN`].
///
/// A sequential run writes one snapshot per claim, hours before the commit
/// snapshot that makes it complete, and holds no restic lock while it stages
/// the next claim. Nothing stops a prune from starting in that time — the
/// weekly check's, `apprafter backup prune`, a backup Job's under `enforce:
/// cluster` beside a `backup create` — and a run with no manifest looks the
/// same whether it died or is still going. Swept as an orphan, the run's
/// claims are deleted under it, and the backup then writes its commit
/// snapshot and succeeds without them.
///
/// The window is what makes the two tell apart:
///
/// * A scheduled run is stopped by its Job's deadline, which counts from the
///   Job's start — before the run's first snapshot, so before its newest. A
///   run whose newest snapshot is older than the deadline (plus the grace
///   period) has ended.
/// * The six-hour floor covers a run started before the deadline was lowered
///   (under the default six hours, or anything shorter), and `apprafter backup
///   create`, which has no Job deadline: each claim's dump runs in a helper
///   pod that lives the same `max(deadline, 6h)` ([`crate::helper_pod`]), so
///   its next snapshot starts within that of the last one's end.
pub fn unfinished_run_window(run_deadline: Duration) -> Duration {
    crate::helper_pod::helper_keep_alive(run_deadline).saturating_add(UNFINISHED_RUN_MARGIN)
}

/// Metadata for one restic snapshot (as [`run_prune`] derives from
/// `restic snapshots --json`).
#[derive(Clone, Debug)]
pub struct SnapshotMeta {
    pub id: String,
    /// The shared `run-<id>` tag (== the backup tag).
    pub run_tag: String,
    /// When its `restic backup` started. RFC 3339 with the WRITER's offset
    /// — `…Z` from the runner, the workstation's own from `backup create` —
    /// so NOT sortable as a string: compared as an instant
    /// ([`snapshot_instant`]). The keep policy files a run by the start its
    /// tag carries, and by this only when the tag carries none
    /// ([`run_instant`]).
    pub time: String,
    /// When its `restic backup` ended (`summary.backup_end`, restic 0.17+),
    /// RFC 3339, the same way; `None` from an older restic. A large claim's
    /// upload lies between `time` and this, and a run is alive until it
    /// ([`unfinished_run_window`]).
    pub ended: Option<String>,
    /// True iff this snapshot carries `manifest.json` (the run representative).
    pub is_manifest: bool,
    /// Every tag restic reports for this snapshot — what
    /// [`crate::cluster::classify_snapshot`] reads to decide whether this
    /// cluster may forget it.
    pub tags: Vec<String>,
}

/// How many representatives to keep per calendar day / ISO week / calendar
/// month, and in which zone those days, weeks and months are counted.
#[derive(Clone, Copy, Debug)]
pub struct RetentionPolicy {
    pub keep_daily: u32,
    pub keep_weekly: u32,
    pub keep_monthly: u32,
    /// The zone whose calendar days, ISO weeks and months the policy counts:
    /// the backup schedule's own, `spec.backup.timeZone` ([`policy_zone`]),
    /// and UTC on a cluster that names none.
    pub zone: Tz,
}

impl Default for RetentionPolicy {
    fn default() -> Self {
        Self {
            keep_daily: 7,
            keep_weekly: 4,
            keep_monthly: 6,
            zone: Tz::UTC,
        }
    }
}

/// The zone the keep policy counts its days, ISO weeks and months in, from
/// `spec.backup.timeZone` — the zone both backup CronJobs run their
/// schedules in. The CLI's `apprafter backup prune` reads it off the
/// PlatformStack; the chart gives the runner the same value as
/// `APPRAFTER_BACKUP_TIME_ZONE`. Both parse it here, against the one zone
/// database compiled into both, so the two prunes bucket alike.
///
/// # Why the schedule's zone, and not UTC
///
/// A daily CronJob in zone Z starts one run per calendar day OF Z, every day
/// of the year, and a run is filed under its start ([`run_instant`]).
/// Counted in Z, `keepDaily: 7` is therefore the last seven scheduled runs
/// as long as each run starts on the day it was scheduled for (a runner
/// that waits past midnight for a node starts on the next), the weekly
/// bucket is Z's Monday to Sunday and the monthly one Z's month. Counted in
/// any other zone, UTC included, the day boundary falls at a different hour
/// of Z's clock, and moves by an hour when Z changes to or from summer time:
/// a schedule within an hour of that boundary then puts two runs in one
/// counted day and none in the next, and `keepDaily` forgets one of the
/// two. A nightly `--at 01:30 --timezone
/// Europe/Berlin` runs at 00:30 UTC on the Sunday of the spring change and
/// at 23:30 UTC the same day for Monday, so a UTC count throws Sunday's run
/// away. The writer's own offset is no zone at all: the runner writes UTC
/// and a workstation its local time, and a run's day then depended on who
/// took it.
///
/// An empty value is a cluster whose schedules run in the
/// kube-controller-manager's zone, which neither the CLI nor the runner can
/// read; UTC stands in for it. A name this database does not know is an
/// `Err` naming it, and the caller counts in UTC and says so: the zone only
/// decides which run is the newest of its day, never how many are kept, and
/// a prune must not stop over it.
pub fn policy_zone(name: &str) -> std::result::Result<Tz, String> {
    let name = name.trim();
    if name.is_empty() {
        return Ok(Tz::UTC);
    }
    name.parse::<Tz>().map_err(|_| {
        format!(
            "spec.backup.timeZone {name:?} is not a zone this build knows, so the keep policy \
             counts its days, weeks and months in UTC"
        )
    })
}

/// The set of snapshot ids to `restic forget`.
#[derive(Debug, PartialEq)]
pub struct PrunePlan {
    pub forget_ids: Vec<String>,
    /// How many runs those ids make up: rotated-out runs and orphans.
    pub forget_runs: usize,
    /// How many of this cluster's runs the keep policy keeps.
    pub kept_runs: usize,
    /// Runs with no manifest that a backup may still be writing
    /// ([`unfinished_run_window`]): left alone, whatever the policy.
    pub unfinished_runs: usize,
}

/// A run: its representative (if any) plus every member snapshot id.
struct Run {
    /// The manifest-bearing snapshot, if the run has one. `None` ⇒ the run
    /// is unfinished: an orphan, or still being written.
    representative: Option<SnapshotMeta>,
    /// Every snapshot id in the run (representative + members).
    ids: Vec<String>,
    /// The newest start or end of a member that parses: the run's last sign
    /// of life.
    newest: Option<DateTime<Utc>>,
}

/// Decide which snapshot ids to forget, format-aware (spec §Retention M-r3-1b).
///
/// 0. **Drop every snapshot that belongs to ANOTHER cluster** (E3). A restic
///    repository can legitimately be shared — the documented "move to a bigger
///    machine" runbook has two clusters alive at once writing to one bucket —
///    and before this filter existed the planner grouped them all together and
///    forgot the neighbour's runs. The exclusion lives HERE, in the pure
///    judgment, rather than only in [`run_prune`]: this is the function that
///    decides what gets deleted, so every route into it must be covered, not
///    just the one route that happens to filter first.
///
///    "Belongs to another cluster" is decided by
///    [`crate::cluster::owned_by_this_cluster`]: a snapshot whose run tag
///    carries a DIFFERENT `kube-system` UID. Legacy snapshots — written before
///    cluster identity existed, carrying no UID at all — are kept in scope by
///    the stated, accepted widening, so a repository does not accumulate
///    snapshots nothing can ever reclaim.
///
/// 1. Group the remaining snapshots by `run_tag`.
/// 2. Each group's REPRESENTATIVE is its `is_manifest == true` member. A group
///    with NO manifest member is UNFINISHED: a sequential run that died before
///    its commit snapshot, or one a backup is still writing. While its newest
///    snapshot is younger than [`unfinished_run_window`] of `run_deadline`
///    (at `now`) it is left alone and counted in `unfinished_runs`; older,
///    it is an ORPHAN → ALL its snapshot ids are forgotten. (A monolithic run
///    is a single snapshot with `is_manifest == true` → it is its own
///    representative.)
/// 3. Apply the keep policy to the set of representatives by their run's
///    start ([`run_instant`]: the start the run tag carries, else the
///    representative's `time`), read as an instant and placed in the
///    policy's zone ([`RetentionPolicy::zone`]): keep the newest
///    representative per distinct calendar day up to `keep_daily`, per
///    distinct ISO week up to `keep_weekly`, per distinct calendar month up
///    to `keep_monthly`. A representative kept by ANY of the three is kept
///    (union). One with no instant belongs to no day, week or month, and is
///    kept.
/// 4. For every representative NOT kept → forget ALL its group's snapshot ids.
/// 5. Representatives that ARE kept → keep all their group's members.
///
/// Pure + deterministic. Empty input ⇒ empty `forget_ids` (never panics).
pub fn plan_prune(
    snapshots: &[SnapshotMeta],
    policy: &RetentionPolicy,
    this_cluster_uid: &str,
    now: DateTime<Utc>,
    run_deadline: Duration,
) -> PrunePlan {
    // 0. Another cluster's snapshots are never ours to forget (E3).
    let snapshots: Vec<&SnapshotMeta> = snapshots
        .iter()
        .filter(|s| crate::cluster::owned_by_this_cluster(&s.tags, this_cluster_uid))
        .collect();

    // 1. Group by run_tag. BTreeMap keeps grouping deterministic; the final
    //    forget_ids order is derived from a stable sort below regardless.
    let mut runs: BTreeMap<String, Run> = BTreeMap::new();
    for s in snapshots {
        let run = runs.entry(s.run_tag.clone()).or_insert_with(|| Run {
            representative: None,
            ids: Vec::new(),
            newest: None,
        });
        run.ids.push(s.id.clone());
        for seen in std::iter::once(&s.time).chain(&s.ended) {
            if let Some(t) = snapshot_instant(seen) {
                run.newest = Some(run.newest.map_or(t, |n| n.max(t)));
            }
        }
        if s.is_manifest {
            // If (pathologically) more than one member claims to be the
            // manifest, the newest by the clock wins as the representative —
            // its `time` is what the keep policy falls back on when the run
            // tag carries no start ([`run_instant`]). A tie is broken
            // by id, as in the keep policy's walk, so the choice does not
            // depend on the order restic lists in.
            match &run.representative {
                Some(cur) if newest_first(cur, s).is_le() => {}
                _ => run.representative = Some(s.clone()),
            }
        }
    }

    let mut forget_ids: Vec<String> = Vec::new();
    let mut forget_runs = 0;
    let mut unfinished_runs = 0;
    let window = unfinished_run_window(run_deadline);

    // 2. No manifest member: left alone while a backup may still be writing
    //    the run, an orphan to forget whole once none can be.
    // Collect the representatives of complete runs for the keep policy.
    let mut representatives: Vec<(&SnapshotMeta, &Vec<String>)> = Vec::new();
    for run in runs.values() {
        match &run.representative {
            None if may_still_be_written(run.newest, now, window) => unfinished_runs += 1,
            None => {
                forget_ids.extend(run.ids.iter().cloned());
                forget_runs += 1;
            }
            Some(rep) => representatives.push((rep, &run.ids)),
        }
    }

    // 3. Apply the keep policy to the representatives.
    let kept = select_kept(&representatives, policy);

    // 4. Forget every group whose representative was not kept.
    let mut kept_runs = 0;
    for (rep, ids) in &representatives {
        if kept.contains(&rep.id) {
            kept_runs += 1;
        } else {
            forget_ids.extend(ids.iter().cloned());
            forget_runs += 1;
        }
    }

    // Deterministic output: sort + dedup (a snapshot id appears once).
    forget_ids.sort();
    forget_ids.dedup();

    PrunePlan {
        forget_ids,
        forget_runs,
        kept_runs,
        unfinished_runs,
    }
}

/// May a backup still be writing a run with no manifest, whose newest
/// snapshot is `newest`, at `now`? Until `window` has passed since that
/// snapshot ([`unfinished_run_window`]) — and always when no member's time
/// parses, since then nothing shows the run has ended.
fn may_still_be_written(
    newest: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
    window: Duration,
) -> bool {
    let Some(newest) = newest else {
        return true;
    };
    match chrono::TimeDelta::from_std(window) {
        Ok(window) => now.signed_duration_since(newest) < window,
        // A window past chrono's range (hundreds of millennia) never ends.
        Err(_) => true,
    }
}

/// Newest first by the clock ([`snapshot_instant`]), a time that does not
/// parse after every one that does, and a tie broken by id — so neither the
/// writer's offset nor the order restic lists in changes which run comes
/// first.
fn newest_first(a: &SnapshotMeta, b: &SnapshotMeta) -> std::cmp::Ordering {
    snapshot_instant(&b.time)
        .cmp(&snapshot_instant(&a.time))
        .then_with(|| a.id.cmp(&b.id))
}

/// The instant the keep policy files a run under: when the run STARTED,
/// which its run tag carries ([`crate::cluster::tag_run_start`]) — else,
/// for a tag with no start in it (every tag written before cluster
/// identity), its representative's own time.
///
/// Not the representative's time first. That is when the commit snapshot's
/// `restic backup` began, and a sequential run writes its commit snapshot
/// only after the last claim is dumped, hours after the run started. Filed
/// by it, a nightly run that starts at 22:00 and commits at 00:10 is the
/// next day's, beside that day's own run, and `keepDaily` forgets one of
/// the two.
fn run_instant(rep: &SnapshotMeta) -> Option<DateTime<Utc>> {
    crate::cluster::tag_run_start(&rep.run_tag).or_else(|| snapshot_instant(&rep.time))
}

/// The keep policy's walk order: newest first by [`run_instant`], the one
/// the runs' days are counted by, a run with none after every one that has
/// one, and a tie broken as [`newest_first`] breaks it.
///
/// It has to be the same instant. The walk keeps the first run of each day
/// it meets and stops once it has kept `keep` days: walked by another time,
/// a run that started on Monday and committed on Tuesday came before
/// Tuesday's own, took the one day `keepDaily: 1` allows, and Tuesday's run
/// was forgotten.
fn newest_run_first(a: &SnapshotMeta, b: &SnapshotMeta) -> std::cmp::Ordering {
    run_instant(b)
        .cmp(&run_instant(a))
        .then_with(|| newest_first(a, b))
}

/// Return the set of representative ids kept by the union of the three buckets.
///
/// A run's day is the date its start ([`run_instant`]) falls on in the
/// policy's zone ([`RetentionPolicy::zone`]) — not the date its writer's
/// clock showed, and not the day its commit snapshot was written. For each
/// bucket kind, walk the runs newest-first by that start
/// ([`newest_run_first`]) and keep the newest one per distinct period (day /
/// ISO week / month) until `keep_*` distinct periods have been kept. A
/// representative kept by ANY bucket is kept.
///
/// So is one with no instant at all: no start in its tag and a time that
/// does not parse. It has no day, week or month to be counted in, and
/// restic always writes a time, so it is not a run this planner
/// understands: it is kept for someone to look at rather than deleted, and
/// it takes no period's slot from a run that has one.
fn select_kept(
    representatives: &[(&SnapshotMeta, &Vec<String>)],
    policy: &RetentionPolicy,
) -> std::collections::HashSet<String> {
    use std::collections::HashSet;

    let mut reps: Vec<&SnapshotMeta> = representatives.iter().map(|(r, _)| *r).collect();
    reps.sort_by(|a, b| newest_run_first(a, b));

    let mut kept: HashSet<String> = HashSet::new();
    let mut dated: Vec<(&str, NaiveDate)> = Vec::with_capacity(reps.len());
    for rep in reps {
        match run_instant(rep) {
            Some(at) => dated.push((&rep.id, at.with_timezone(&policy.zone).date_naive())),
            None => {
                kept.insert(rep.id.clone());
            }
        }
    }

    keep_by_period(&dated, policy.keep_daily, day_of, &mut kept);
    keep_by_period(&dated, policy.keep_weekly, iso_week_of, &mut kept);
    keep_by_period(&dated, policy.keep_monthly, month_of, &mut kept);

    kept
}

/// Keep the newest representative per distinct period, up to `keep` periods.
///
/// `dated` (each representative's id and its date in the policy's zone) MUST
/// already be sorted newest-first. Adds kept ids into `kept`.
fn keep_by_period(
    dated: &[(&str, NaiveDate)],
    keep: u32,
    period_of: fn(NaiveDate) -> (i32, u32),
    kept: &mut std::collections::HashSet<String>,
) {
    if keep == 0 {
        return;
    }
    let mut seen_periods: Vec<(i32, u32)> = Vec::new();
    for (id, date) in dated {
        let period = period_of(*date);
        if seen_periods.contains(&period) {
            continue; // already kept the newest representative in this period
        }
        if seen_periods.len() as u32 >= keep {
            break; // kept enough distinct periods for this bucket
        }
        seen_periods.push(period);
        kept.insert(id.to_string());
    }
}

/// A calendar day: its year and day of the year.
fn day_of(date: NaiveDate) -> (i32, u32) {
    (date.year(), date.ordinal())
}

/// An ISO 8601 week: its ISO week-year and week number (chrono `IsoWeek`, so
/// a run on 2026-12-31 and one on 2027-01-01 land in the same week when they
/// should).
fn iso_week_of(date: NaiveDate) -> (i32, u32) {
    let week = date.iso_week();
    (week.year(), week.week())
}

/// A calendar month: its year and month.
fn month_of(date: NaiveDate) -> (i32, u32) {
    (date.year(), date.month())
}

// ---------------------------------------------------------------------------
// Executor (impure) — thin restic interaction around the pure planner.
// ---------------------------------------------------------------------------

/// What [`run_prune`] did to the repository.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PruneOutcome {
    /// Every complete run of this cluster is inside the keep policy: nothing
    /// was forgotten, and restic was not asked to delete anything.
    NothingToPrune {
        kept_runs: usize,
        /// Runs a backup may still be writing, left alone
        /// ([`PrunePlan::unfinished_runs`]).
        unfinished_runs: usize,
    },
    /// Snapshots forgotten, and the data only they referred to removed.
    Pruned {
        forgot_snapshots: usize,
        forgot_runs: usize,
        kept_runs: usize,
        /// Runs a backup may still be writing, left alone
        /// ([`PrunePlan::unfinished_runs`]).
        unfinished_runs: usize,
    },
    /// The credential may not delete snapshots: the store refused the first
    /// delete, the snapshot is still listed, and NOTHING was deleted or
    /// written — the first `forget` names one snapshot, and no prune runs.
    /// The scoped key ADR 0050 recommends for the cluster answers this way
    /// by design.
    NotPermitted {
        /// The snapshot whose delete was refused.
        snapshot: String,
        /// What restic said about it (its stderr, trimmed).
        restic_said: String,
        /// Snapshots and runs the policy would have forgotten.
        would_forget_snapshots: usize,
        would_forget_runs: usize,
    },
}

impl PruneOutcome {
    /// One sentence on what happened, for a status record or a terminal.
    pub fn describe(&self) -> String {
        match self {
            PruneOutcome::NothingToPrune {
                kept_runs,
                unfinished_runs,
            } => format!(
                "nothing to prune: all {kept_runs} run(s) of this cluster are inside the keep \
                 policy{}",
                left_alone(*unfinished_runs)
            ),
            PruneOutcome::Pruned {
                forgot_snapshots,
                forgot_runs,
                kept_runs,
                unfinished_runs,
            } => format!(
                "forgot {forgot_snapshots} snapshot(s) of {forgot_runs} run(s) and pruned the \
                 data only they used; {kept_runs} run(s) kept{}",
                left_alone(*unfinished_runs)
            ),
            PruneOutcome::NotPermitted {
                snapshot,
                restic_said,
                would_forget_snapshots,
                would_forget_runs,
            } => format!(
                "not permitted: the storage refused to delete snapshot {} ({}), so nothing was \
                 deleted; {would_forget_snapshots} snapshot(s) of {would_forget_runs} run(s) \
                 are past the keep policy",
                short_id(snapshot),
                first_line(restic_said)
            ),
        }
    }
}

/// The clause [`PruneOutcome::describe`] adds for runs a backup may still be
/// writing: empty when there are none.
fn left_alone(unfinished_runs: usize) -> String {
    if unfinished_runs == 0 {
        return String::new();
    }
    format!(
        "; {unfinished_runs} unfinished run(s) left alone, as a backup may still be writing \
         them"
    )
}

/// restic's eight-character short form of a snapshot id.
fn short_id(id: &str) -> &str {
    id.get(..8).unwrap_or(id)
}

/// The first line of restic's stderr that says something, for a one-line
/// record.
fn first_line(stderr: &str) -> &str {
    stderr
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("restic gave no reason")
}

/// `restic snapshots --json` → derive [`SnapshotMeta`] → [`plan_prune`] →
/// forget the resulting id set → `restic prune`.
///
/// The JUDGMENT (what to forget) is entirely in the pure [`plan_prune`]; this
/// only marshals restic I/O. A no-op prune (nothing to forget) runs neither
/// `forget` nor `prune`.
///
/// # Forget, look, then prune
///
/// restic's own `forget <ids> --prune` is not safe to run with a credential
/// that may not delete: restic 0.18.1 exits 0 from a `forget` whose deletes
/// were refused, and then prunes as if those snapshots were gone (see
/// [`crate::restic::restic_forget_argv`]). So this:
///
/// 1. forgets ONE snapshot of the plan, and lists the repository again. If
///    it is still there, nothing has been deleted and nothing else is tried:
///    [`PruneOutcome::NotPermitted`] when restic says the store refused the
///    delete, an error otherwise;
/// 2. forgets the rest, and lists again: a snapshot still there is an error,
///    and no prune runs;
/// 3. only then runs `restic prune`, which counts as used everything the
///    listed snapshots refer to.
///
/// `this_cluster_uid` is the caller's `kube-system` namespace UID. The listing
/// is repository-WIDE (`restic snapshots` has no prefix filter, and the tag
/// carrying the identity is per-run, so there is no exact `--tag` value to ask
/// restic for — and legacy snapshots carry no matchable tag at all), so the
/// narrowing happens in [`plan_prune`], which is where the delete decision is
/// made and therefore the only place that can be complete.
///
/// `now` and `run_deadline` (the cluster's backup run deadline,
/// `spec.backup.activeDeadlineSeconds`) decide which runs with no manifest a
/// backup may still be writing ([`unfinished_run_window`]); those are left
/// alone.
pub fn run_prune(
    r: &dyn crate::ResticRunner,
    repo: &str,
    pass: &str,
    policy: &RetentionPolicy,
    this_cluster_uid: &str,
    now: DateTime<Utc>,
    run_deadline: Duration,
) -> Result<PruneOutcome> {
    let json = r.run_stdout(&restic_snapshots_argv(repo), pass)?;
    let snapshots = parse_snapshots(&json)?;
    let plan = plan_prune(&snapshots, policy, this_cluster_uid, now, run_deadline);
    let Some((probe, rest)) = plan.forget_ids.split_first() else {
        return Ok(PruneOutcome::NothingToPrune {
            kept_runs: plan.kept_runs,
            unfinished_runs: plan.unfinished_runs,
        });
    };

    // 1. One snapshot first: under a key that may not delete, this is the
    //    only request that reaches the store, and it changes nothing.
    let said = r.run_capture(&restic_forget_argv(repo, std::slice::from_ref(probe)), pass)?;
    if listed_ids(r, repo, pass)?.contains(probe) {
        let restic_said = said.stderr.trim().to_string();
        if crate::restic::delete_was_denied(&restic_said) {
            return Ok(PruneOutcome::NotPermitted {
                snapshot: probe.clone(),
                restic_said,
                would_forget_snapshots: plan.forget_ids.len(),
                would_forget_runs: plan.forget_runs,
            });
        }
        return Err(CliError::Other(format!(
            "restic forget exited 0 but snapshot {probe} is still in the repository, so \
             nothing was forgotten and the repository was not pruned. restic said: {}",
            first_line(&restic_said)
        )));
    }

    // 2. The rest of the plan.
    if !rest.is_empty() {
        let said = r.run_capture(&restic_forget_argv(repo, rest), pass)?;
        let listed = listed_ids(r, repo, pass)?;
        let left: Vec<&String> = rest.iter().filter(|id| listed.contains(*id)).collect();
        if !left.is_empty() {
            return Err(CliError::Other(format!(
                "restic forget removed {} of the {} snapshot(s) past the keep policy, and {} \
                 (first {}) are still in the repository, so the repository was not pruned. \
                 restic said: {}",
                plan.forget_ids.len() - left.len(),
                plan.forget_ids.len(),
                left.len(),
                left[0],
                first_line(said.stderr.trim())
            )));
        }
    }

    // 3. Every forgotten snapshot is gone: reclaim the data only they used.
    r.run(&restic_prune_argv(repo), pass)?;
    Ok(PruneOutcome::Pruned {
        forgot_snapshots: plan.forget_ids.len(),
        forgot_runs: plan.forget_runs,
        kept_runs: plan.kept_runs,
        unfinished_runs: plan.unfinished_runs,
    })
}

/// The full ids of every snapshot the repository lists now.
fn listed_ids(
    r: &dyn crate::ResticRunner,
    repo: &str,
    pass: &str,
) -> Result<std::collections::BTreeSet<String>> {
    let json = r.run_stdout(&restic_snapshots_argv(repo), pass)?;
    Ok(parse_snapshots(&json)?.into_iter().map(|s| s.id).collect())
}

/// Parse `restic snapshots --json` (an array of snapshot objects) into
/// [`SnapshotMeta`], deriving `run_tag` + `is_manifest` (see [`derive_manifest`]).
///
/// `pub(crate)` for one reader outside this module: the test that holds
/// [`crate::restore`]'s `latest` to this grouping.
pub(crate) fn parse_snapshots(json: &str) -> Result<Vec<SnapshotMeta>> {
    let value: Value = serde_json::from_str(json)
        .map_err(|e| CliError::Other(format!("parse restic snapshots JSON: {e}")))?;
    let arr = value.as_array().cloned().unwrap_or_default();

    // First pass: id, run_tag, time, paths.
    struct Raw {
        id: String,
        run_tag: String,
        time: String,
        ended: Option<String>,
        paths: Vec<String>,
        tags: Vec<String>,
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
        let tags: Vec<String> = s
            .pointer("/tags")
            .and_then(Value::as_array)
            .map(|t| {
                t.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        // Each backup snapshot carries exactly one `--tag` (the shared run tag);
        // take the first tag as the run_tag.
        let run_tag = tags.first().cloned().unwrap_or_default();
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
        let ended = s
            .pointer("/summary/backup_end")
            .and_then(Value::as_str)
            .map(str::to_string);
        raws.push(Raw {
            id,
            run_tag,
            time,
            ended,
            paths,
            tags,
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
                ended: r.ended,
                tags: r.tags,
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
/// * A per-claim (`claim-<i>`) snapshot is never a representative. Even ALONE
///   in its group it is not a monolithic run: a sequential run of one claim
///   still ends with its commit snapshot, so a lone claim snapshot is the
///   first of a run that has not finished — still being written, or dead.
///   Counted as complete, it took its day's keep slot from a run that was.
/// * Any other snapshot ALONE in its run_tag group is a monolithic run →
///   representative, whatever its path.
/// * In a MULTI-snapshot group, the representative is the one whose path is the
///   commit/manifest dir (basename `commit`).
///
/// Also THE rule for which run `latest` means
/// ([`crate::restore::resolve_run_snapshots`]): a run the prune would sweep as
/// an orphan is not one a restore may pick.
pub(crate) fn derive_manifest(paths: &[String], alone: bool) -> bool {
    let base = |p: &String| -> String {
        p.trim_end_matches('/')
            .rsplit('/')
            .next()
            .unwrap_or(p)
            .to_string()
    };
    if paths.iter().any(|p| is_claim_dir(&base(p))) {
        return false;
    }
    alone || paths.iter().any(|p| base(p) == "commit")
}

/// Is `base` a per-claim staging directory the engine writes: `claim-<i>`?
fn is_claim_dir(base: &str) -> bool {
    base.strip_prefix("claim-")
        .is_some_and(|i| !i.is_empty() && i.bytes().all(|b| b.is_ascii_digit()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// This cluster's `kube-system` UID, and a co-tenant's.
    const MINE: &str = "11111111-2222-3333-4444-555555555555";
    const THEIRS: &str = "99999999-8888-7777-6666-555555555555";

    /// The chart's default backup deadline.
    const SIX_HOURS: Duration = crate::helper_pod::DEFAULT_RUN_DEADLINE;

    fn at(t: &str) -> DateTime<Utc> {
        t.parse().unwrap()
    }

    /// Long after every run of the older listings below: no backup can still
    /// be writing any of them.
    fn later() -> DateTime<Utc> {
        at("2026-09-27T12:00:00Z")
    }

    /// Build a sequential run: `claims` per-claim snapshots (is_manifest:false)
    /// plus 1 manifest/commit snapshot (is_manifest:true), all sharing `tag`,
    /// at `{day}T03:00:00Z`.
    ///
    /// `tag` is a LABEL for readable ids; the actual run tag is the real
    /// production shape (`<uid>-<time>-<label>`) so the cluster filter sees
    /// what it sees in a repository.
    fn seq_run_of(uid: &str, tag: &str, day: &str, claims: usize) -> Vec<SnapshotMeta> {
        let time = format!("{day}T03:00:00Z");
        let run_tag = format!("{uid}-{time}-{tag}");
        let mut out = Vec::new();
        for i in 0..claims {
            out.push(SnapshotMeta {
                id: format!("{tag}-claim-{i}"),
                run_tag: run_tag.clone(),
                time: time.clone(),
                is_manifest: false,
                ended: None,
                tags: vec![run_tag.clone()],
            });
        }
        out.push(SnapshotMeta {
            id: format!("{tag}-manifest"),
            run_tag: run_tag.clone(),
            time,
            is_manifest: true,
            ended: None,
            tags: vec![run_tag],
        });
        out
    }

    fn seq_run(tag: &str, day: &str, claims: usize) -> Vec<SnapshotMeta> {
        seq_run_of(MINE, tag, day, claims)
    }

    /// Build a monolithic run: one snapshot (is_manifest:true) at `{day}T03:00:00Z`.
    fn mono_run_of(uid: &str, tag: &str, day: &str) -> Vec<SnapshotMeta> {
        let time = format!("{day}T03:00:00Z");
        let run_tag = format!("{uid}-{time}-{tag}");
        vec![SnapshotMeta {
            id: format!("{tag}-mono"),
            run_tag: run_tag.clone(),
            time,
            is_manifest: true,
            ended: None,
            tags: vec![run_tag],
        }]
    }

    fn mono_run(tag: &str, day: &str) -> Vec<SnapshotMeta> {
        mono_run_of(MINE, tag, day)
    }

    /// A run in the PRE-IDENTITY format: `<release-name>-<time>`, no UID.
    fn legacy_run(tag: &str, day: &str) -> Vec<SnapshotMeta> {
        let time = format!("{day}T03:00:00Z");
        let run_tag = format!("platform-{time}-{tag}");
        vec![SnapshotMeta {
            id: format!("{tag}-legacy"),
            run_tag: run_tag.clone(),
            time,
            is_manifest: true,
            ended: None,
            tags: vec![run_tag],
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
            zone: Tz::UTC,
        };
        let plan = plan_prune(&snaps, &policy, MINE, later(), SIX_HOURS);
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
            zone: Tz::UTC,
        };
        let plan = plan_prune(&snaps, &policy, MINE, later(), SIX_HOURS);
        assert_eq!(plan.forget_ids, vec!["m-a-mono".to_string()]);
    }

    #[test]
    fn orphan_set_without_a_manifest_is_swept_entirely() {
        // One run_tag group of 2 claim snapshots, both is_manifest:false → an
        // interrupted run: both ids forgotten regardless of policy.
        let run_tag = format!("{MINE}-2026-07-12T03:00:00Z");
        let snaps = vec![
            SnapshotMeta {
                id: "orphan-0".into(),
                run_tag: run_tag.clone(),
                time: "2026-07-12T03:00:00Z".into(),
                is_manifest: false,
                ended: None,
                tags: vec![run_tag.clone()],
            },
            SnapshotMeta {
                id: "orphan-1".into(),
                run_tag: run_tag.clone(),
                time: "2026-07-12T03:00:00Z".into(),
                is_manifest: false,
                ended: None,
                tags: vec![run_tag],
            },
        ];
        // Even a generous policy sweeps the orphan (no representative to keep).
        let plan = plan_prune(
            &snaps,
            &RetentionPolicy::default(),
            MINE,
            later(),
            SIX_HOURS,
        );
        assert_eq!(
            plan.forget_ids,
            vec!["orphan-0".to_string(), "orphan-1".to_string()]
        );
    }

    // -----------------------------------------------------------------------
    // A run with no manifest that a backup may still be writing.
    // -----------------------------------------------------------------------

    /// A sequential run's claim snapshots, as `restic snapshots --json` lists
    /// them — each at its own time, under one run tag that started at
    /// `start` — with no commit snapshot yet.
    fn claims_only(label: &str, start: &str, times: &[&str]) -> Vec<SnapshotMeta> {
        let run_tag = format!("{MINE}-{start}");
        times
            .iter()
            .enumerate()
            .map(|(i, t)| SnapshotMeta {
                id: format!("{label}-claim-{i}"),
                run_tag: run_tag.clone(),
                time: t.to_string(),
                is_manifest: false,
                ended: None,
                tags: vec![run_tag.clone()],
            })
            .collect()
    }

    /// The reported failure: the Sunday 03:00 sequential backup is still
    /// dumping at 06:00, when the weekly check prunes. Its two claim
    /// snapshots have no commit snapshot yet — and must not be swept as an
    /// orphan, whatever the policy.
    #[test]
    fn a_run_a_backup_is_still_writing_is_left_alone() {
        let mut snaps = seq_run("sat", "2026-09-26", 1);
        snaps.extend(claims_only(
            "sun",
            "2026-09-27T03:00:00Z",
            &["2026-09-27T03:40:00Z", "2026-09-27T04:30:00Z"],
        ));
        let plan = plan_prune(
            &snaps,
            &RetentionPolicy::default(),
            MINE,
            at("2026-09-27T06:00:00Z"),
            SIX_HOURS,
        );
        assert_eq!(
            plan,
            PrunePlan {
                forget_ids: vec![],
                forget_runs: 0,
                kept_runs: 1,
                unfinished_runs: 1,
            }
        );
    }

    /// The window runs from the run's NEWEST snapshot: the backup deadline,
    /// never less than six hours, plus an hour. At the window a run is an
    /// orphan and is swept as before; a second earlier it is left alone.
    #[test]
    fn a_run_with_no_manifest_is_an_orphan_once_its_newest_snapshot_is_past_the_window() {
        let snaps = claims_only(
            "dead",
            "2026-09-27T03:00:00Z",
            &["2026-09-27T03:40:00Z", "2026-09-27T04:30:00Z"],
        );
        let newest = at("2026-09-27T04:30:00Z");
        let seconds = |s: u64| chrono::TimeDelta::seconds(s as i64);
        for (deadline, window) in [
            // The default: six hours, plus the hour.
            (SIX_HOURS, 7 * 3600),
            // A deadline lowered for a frequent schedule keeps the floor.
            (Duration::from_secs(600), 7 * 3600),
            // A longer one moves the window with it.
            (Duration::from_secs(12 * 3600), 13 * 3600),
        ] {
            assert_eq!(unfinished_run_window(deadline).as_secs(), window);
            let spared = plan_prune(
                &snaps,
                &RetentionPolicy::default(),
                MINE,
                newest + seconds(window - 1),
                deadline,
            );
            assert!(spared.forget_ids.is_empty(), "{deadline:?}: {spared:?}");
            assert_eq!(spared.unfinished_runs, 1, "{deadline:?}");
            let swept = plan_prune(
                &snaps,
                &RetentionPolicy::default(),
                MINE,
                newest + seconds(window),
                deadline,
            );
            assert_eq!(
                swept.forget_ids,
                vec!["dead-claim-0".to_string(), "dead-claim-1".to_string()],
                "{deadline:?}"
            );
            assert_eq!((swept.forget_runs, swept.unfinished_runs), (1, 0));
        }
    }

    /// A snapshot's `time` is when its `restic backup` STARTED; restic 0.17+
    /// also records when it ended (`summary.backup_end`), and a large claim's
    /// upload lies between the two. The run was alive until the end, so the
    /// window counts from there. The times are in the form restic 0.18.1
    /// writes them: nanoseconds and the host's offset.
    #[test]
    fn a_run_is_alive_until_its_newest_snapshot_ended_not_until_it_started() {
        let json = format!(
            r#"[
              {{"id":"big-c0","time":"2026-09-26T23:00:00.109384903+01:00",
                "tags":["{MINE}-2026-09-26T21:55:00+00:00"],"paths":["/staging/b/claim-0"],
                "summary":{{"backup_start":"2026-09-26T23:00:00.109384903+01:00",
                            "backup_end":"2026-09-27T00:30:00.161944354+01:00"}}}},
              {{"id":"big-c1","time":"2026-09-26T23:10:00.5+01:00",
                "tags":["{MINE}-2026-09-26T21:55:00+00:00"],"paths":["/staging/b/claim-1"]}}
            ]"#
        );
        let snaps = parse_snapshots(&json).unwrap();
        // 23:00+01:00 is 22:00Z: eight hours before Sunday 06:00Z, past the
        // seven-hour window; the upload ended at 23:30Z, six and a half.
        let plan = plan_prune(
            &snaps,
            &RetentionPolicy::default(),
            MINE,
            at("2026-09-27T06:00:00Z"),
            SIX_HOURS,
        );
        assert!(plan.forget_ids.is_empty(), "{plan:?}");
        assert_eq!(plan.unfinished_runs, 1);
        // Once seven hours have passed since the end, it is an orphan.
        let plan = plan_prune(
            &snaps,
            &RetentionPolicy::default(),
            MINE,
            at("2026-09-27T06:30:01Z"),
            SIX_HOURS,
        );
        assert_eq!(plan.forget_ids, vec!["big-c0", "big-c1"]);
    }

    /// A run whose snapshots carry no time that parses cannot be shown to
    /// have ended, so it is not deleted.
    #[test]
    fn a_run_with_no_manifest_and_no_readable_time_is_left_alone() {
        let snaps = claims_only("odd", "2026-07-12T03:00:00Z", &["", "yesterday"]);
        let plan = plan_prune(
            &snaps,
            &RetentionPolicy::default(),
            MINE,
            later(),
            SIX_HOURS,
        );
        assert!(plan.forget_ids.is_empty(), "{plan:?}");
        assert_eq!(plan.unfinished_runs, 1);
    }

    /// With ONE claim snapshot so far, an unfinished sequential run used to
    /// pass for a complete monolithic run ("alone in its group"), and as the
    /// newest "run" of the day it took the day's keep slot from the run that
    /// had completed: that one was forgotten.
    #[test]
    fn a_first_claim_snapshot_alone_does_not_take_a_complete_runs_slot() {
        let json = format!(
            r#"[
              {{"id":"done","time":"2026-09-27T01:00:00Z",
                "tags":["{MINE}-2026-09-27T01:00:00Z"],"paths":["/staging/x/data"]}},
              {{"id":"going-claim-0","time":"2026-09-27T02:10:00Z",
                "tags":["{MINE}-2026-09-27T02:00:00Z"],"paths":["/staging/y/claim-0"]}}
            ]"#
        );
        let snaps = parse_snapshots(&json).unwrap();
        let plan = plan_prune(
            &snaps,
            &KEEP_ONE_DAY,
            MINE,
            at("2026-09-27T02:30:00Z"),
            SIX_HOURS,
        );
        assert_eq!(
            plan,
            PrunePlan {
                forget_ids: vec![],
                forget_runs: 0,
                kept_runs: 1,
                unfinished_runs: 1,
            }
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
        let plan = plan_prune(&[], &RetentionPolicy::default(), MINE, later(), SIX_HOURS);
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
            zone: Tz::UTC,
        };
        let plan = plan_prune(&snaps, &policy, MINE, later(), SIX_HOURS);
        assert_eq!(plan.forget_ids, vec!["w-old-mono".to_string()]);

        // Now a monthly bucket rescues an older run in a different month.
        let mut snaps = Vec::new();
        snaps.extend(mono_run("prev-month", "2026-06-15")); // June
        snaps.extend(mono_run("this-month", "2026-07-15")); // July
        let policy = RetentionPolicy {
            keep_daily: 1,
            keep_weekly: 0,
            keep_monthly: 2,
            zone: Tz::UTC,
        };
        let plan = plan_prune(&snaps, &policy, MINE, later(), SIX_HOURS);
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
            zone: Tz::UTC,
        };
        let plan = plan_prune(&snaps, &policy, MINE, later(), SIX_HOURS);
        let mut expected = vec![
            "m-a-mono".to_string(),
            "s-b-claim-0".to_string(),
            "s-b-claim-1".to_string(),
            "s-b-manifest".to_string(),
        ];
        expected.sort();
        assert_eq!(plan.forget_ids, expected);
    }

    // -----------------------------------------------------------------------
    // Times written at different offsets, and the zone the policy counts in.
    // -----------------------------------------------------------------------

    /// A complete monolithic run whose one snapshot restic lists with `time`
    /// exactly as its writer recorded it: `…Z` from the runner, the
    /// workstation's own offset from `apprafter backup create`.
    fn run_at(label: &str, time: &str) -> SnapshotMeta {
        let run_tag = format!("{MINE}-{label}");
        SnapshotMeta {
            id: label.to_string(),
            run_tag: run_tag.clone(),
            time: time.to_string(),
            is_manifest: true,
            ended: None,
            tags: vec![run_tag],
        }
    }

    fn keep_days(keep_daily: u32, zone: Tz) -> RetentionPolicy {
        RetentionPolicy {
            keep_daily,
            keep_weekly: 0,
            keep_monthly: 0,
            zone,
        }
    }

    fn berlin() -> Tz {
        policy_zone("Europe/Berlin").unwrap()
    }

    /// The ids `plan_prune` forgets, with the listing in the order given
    /// and reversed: the plan must not depend on the order restic lists in.
    fn forgotten(snaps: &[SnapshotMeta], policy: &RetentionPolicy) -> Vec<String> {
        let plan = plan_prune(snaps, policy, MINE, later(), SIX_HOURS);
        let mut reversed = snaps.to_vec();
        reversed.reverse();
        let again = plan_prune(&reversed, policy, MINE, later(), SIX_HOURS);
        assert_eq!(plan, again, "the plan depends on the listing's order");
        plan.forget_ids
    }

    /// The reported failure: in one repository the runner (UTC) took a run
    /// at 03:30:00Z, and a workstation an hour ahead of UTC had taken one at
    /// 04:00:03+01:00, which is 03:00:03Z — half an hour EARLIER. Compared
    /// as strings the workstation's was the newer, and the day's one kept
    /// run was the older of the two.
    #[test]
    fn the_newer_run_of_a_day_is_kept_whichever_offset_its_writer_used() {
        let json = format!(
            r#"[
              {{"id":"runner","time":"2026-09-24T03:30:00.1Z",
                "tags":["{MINE}-2026-09-24T03:30:00Z"],"paths":["/s/r/data"]}},
              {{"id":"cli","time":"2026-09-24T04:00:03.1+01:00",
                "tags":["{MINE}-2026-09-24T03:00:00Z"],"paths":["/s/c/data"]}}
            ]"#
        );
        let snaps = parse_snapshots(&json).unwrap();
        for zone in [Tz::UTC, berlin(), policy_zone("Europe/London").unwrap()] {
            assert_eq!(
                forgotten(&snaps, &keep_days(1, zone)),
                vec!["cli"],
                "{zone}"
            );
        }
        assert_eq!(
            forgotten(&snaps, &RetentionPolicy::default()),
            vec!["cli"],
            "the default 7/4/6 keeps one run a day too"
        );
    }

    /// A run's day is its day in the policy's zone, whoever wrote it — not
    /// the date its writer's clock showed. Around midnight in Berlin (in
    /// September two hours ahead of UTC), around midnight UTC, and around
    /// midnight in New York (four hours behind).
    #[test]
    fn a_run_just_before_and_one_just_after_midnight_are_on_the_zones_two_days() {
        // Berlin: a workstation there at 23:59 on the 23rd, the runner at
        // 22:01Z — two minutes later, and 00:01 on the 24th in Berlin.
        let snaps = vec![
            run_at("older", "2026-09-22T10:00:00Z"),
            run_at("berlin-2359", "2026-09-23T23:59:00+02:00"),
            run_at("runner-2201z", "2026-09-23T22:01:00Z"),
        ];
        assert_eq!(forgotten(&snaps, &keep_days(2, berlin())), vec!["older"]);
        // In UTC both are on the 23rd, and the later of them is its run.
        assert_eq!(
            forgotten(&snaps, &keep_days(2, Tz::UTC)),
            vec!["berlin-2359"]
        );

        // UTC: the Berlin workstation at 01:59 on the 25th is 23:59Z on the
        // 24th; the runner two minutes later is on the 25th.
        let snaps = vec![
            run_at("older", "2026-09-23T12:00:00Z"),
            run_at("berlin-0159", "2026-09-25T01:59:00+02:00"),
            run_at("runner-0001z", "2026-09-25T00:01:00Z"),
        ];
        assert_eq!(forgotten(&snaps, &keep_days(2, Tz::UTC)), vec!["older"]);
        // In Berlin both are on the 25th.
        assert_eq!(
            forgotten(&snaps, &keep_days(2, berlin())),
            vec!["berlin-0159"]
        );

        // New York: the runner at 03:30Z is 23:30 on the 23rd there; a
        // workstation in New York at 00:10 on the 24th is 04:10Z.
        let new_york = policy_zone("America/New_York").unwrap();
        let snaps = vec![
            run_at("older", "2026-09-22T12:00:00Z"),
            run_at("runner-0330z", "2026-09-24T03:30:00Z"),
            run_at("ny-0010", "2026-09-24T00:10:00-04:00"),
        ];
        assert_eq!(forgotten(&snaps, &keep_days(2, new_york)), vec!["older"]);
        assert_eq!(
            forgotten(&snaps, &keep_days(2, Tz::UTC)),
            vec!["runner-0330z"]
        );
    }

    /// Spring: a nightly `--at 01:30 --timezone Europe/Berlin` runs at
    /// 00:30Z until Sunday 29 March 2026, the day the clocks go forward,
    /// and at 23:30Z the day BEFORE from Monday. Counted in Berlin, every
    /// run is its own day and `keepDaily: 7` is the last seven runs.
    /// Counted in UTC, Sunday's and Monday's runs share the 29th, and
    /// Sunday's is forgotten while an eighth-oldest run is kept.
    #[test]
    fn a_daily_schedule_is_one_run_a_day_across_the_spring_clock_change() {
        let snaps: Vec<SnapshotMeta> = [
            ("tue-24", "2026-03-24T00:30:00Z"),
            ("wed-25", "2026-03-25T00:30:00Z"),
            ("thu-26", "2026-03-26T00:30:00Z"),
            ("fri-27", "2026-03-27T00:30:00Z"),
            ("sat-28", "2026-03-28T00:30:00Z"),
            ("sun-29", "2026-03-29T00:30:00Z"),
            ("mon-30", "2026-03-29T23:30:00Z"),
            ("tue-31", "2026-03-30T23:30:00Z"),
        ]
        .iter()
        .map(|(label, time)| run_at(label, time))
        .collect();
        assert_eq!(forgotten(&snaps, &keep_days(7, berlin())), vec!["tue-24"]);
        assert_eq!(forgotten(&snaps, &keep_days(7, Tz::UTC)), vec!["sun-29"]);
    }

    /// Autumn: on 25 October 2026 Berlin's clocks go from 03:00 back to
    /// 02:00, so a workstation there writes 02:30+02:00 (00:30Z) and, forty
    /// minutes later, 02:10+01:00 (01:10Z). The later run's string is the
    /// smaller: one writer's times do not sort as strings either.
    #[test]
    fn the_later_of_two_runs_in_the_repeated_autumn_hour_is_the_days_run() {
        let snaps = vec![
            run_at("first", "2026-10-25T02:30:00+02:00"),
            run_at("second", "2026-10-25T02:10:00+01:00"),
        ];
        for zone in [berlin(), Tz::UTC] {
            assert_eq!(
                forgotten(&snaps, &keep_days(1, zone)),
                vec!["first"],
                "{zone}"
            );
        }
    }

    /// Weeks and months are the zone's too: 22:30Z on Sunday 27 September
    /// is 00:30 on Monday in Berlin (ISO week 40), and 22:30Z on 30 September
    /// is 1 October there.
    #[test]
    fn weeks_and_months_are_counted_in_the_policy_zone() {
        let weeks = RetentionPolicy {
            keep_daily: 0,
            keep_weekly: 2,
            keep_monthly: 0,
            zone: berlin(),
        };
        let snaps = vec![
            run_at("w38", "2026-09-16T10:00:00Z"),
            run_at("sun-w39", "2026-09-27T10:00:00Z"),
            run_at("mon-w40-berlin", "2026-09-27T22:30:00Z"),
        ];
        assert_eq!(forgotten(&snaps, &weeks), vec!["w38"]);
        let utc_weeks = RetentionPolicy {
            zone: Tz::UTC,
            ..weeks
        };
        assert_eq!(forgotten(&snaps, &utc_weeks), vec!["sun-w39"]);

        let months = RetentionPolicy {
            keep_daily: 0,
            keep_weekly: 0,
            keep_monthly: 2,
            zone: berlin(),
        };
        let snaps = vec![
            run_at("august", "2026-08-15T10:00:00Z"),
            run_at("september", "2026-09-30T10:00:00Z"),
            run_at("october-berlin", "2026-09-30T22:30:00Z"),
        ];
        assert_eq!(forgotten(&snaps, &months), vec!["august"]);
        let utc_months = RetentionPolicy {
            zone: Tz::UTC,
            ..months
        };
        assert_eq!(forgotten(&snaps, &utc_months), vec!["september"]);
    }

    /// A run with two manifest members (which the engine never writes) is
    /// represented by the later of them BY THE CLOCK: here 03:30Z, not the
    /// 04:00:03+01:00 (03:00:03Z) whose string is the greater. That decides
    /// whether it is the newer run of its day than one at 04:15+01:00
    /// (03:15Z).
    #[test]
    fn the_later_manifest_member_by_the_clock_represents_its_run() {
        let run_tag = format!("{MINE}-two-manifests");
        let member = |id: &str, time: &str| SnapshotMeta {
            id: id.to_string(),
            run_tag: run_tag.clone(),
            time: time.to_string(),
            is_manifest: true,
            ended: None,
            tags: vec![run_tag.clone()],
        };
        let snaps = vec![
            member("m-0300z", "2026-09-24T04:00:03+01:00"),
            member("m-0330z", "2026-09-24T03:30:00Z"),
            run_at("other-0315z", "2026-09-24T04:15:00+01:00"),
        ];
        assert_eq!(
            forgotten(&snaps, &keep_days(1, Tz::UTC)),
            vec!["other-0315z"]
        );
    }

    /// A complete run whose time does not parse cannot be placed in a day,
    /// a week or a month — and restic always writes one, so it is not a run
    /// this planner understands. It is kept, whatever the policy; it does not
    /// take a day's slot from a run that has a time.
    #[test]
    fn a_complete_run_whose_time_does_not_parse_is_never_forgotten() {
        let snaps = vec![
            run_at("mon", "2026-09-21T03:00:00Z"),
            run_at("tue", "2026-09-22T03:00:00Z"),
            run_at("no-time", ""),
            run_at("odd-time", "yesterday"),
            run_at("wed", "2026-09-23T03:00:00Z"),
        ];
        assert_eq!(
            forgotten(&snaps, &keep_days(1, Tz::UTC)),
            vec!["mon", "tue"]
        );
        let plan = plan_prune(&snaps, &keep_days(1, Tz::UTC), MINE, later(), SIX_HOURS);
        assert_eq!((plan.forget_runs, plan.kept_runs), (2, 3), "{plan:?}");
        let none = keep_days(0, Tz::UTC);
        assert_eq!(forgotten(&snaps, &none), vec!["mon", "tue", "wed"]);
    }

    /// A run as the engine writes it: a claim snapshot at its `start`, and
    /// the commit snapshot, begun at `commit`, all under the real run tag,
    /// which carries the start ([`crate::cluster::run_tag`]).
    fn run_started(label: &str, start: &str, commit: &str) -> Vec<SnapshotMeta> {
        let run_tag = crate::cluster::run_tag(MINE, start, &[]);
        let member = |id: String, time: &str, is_manifest: bool| SnapshotMeta {
            id,
            run_tag: run_tag.clone(),
            time: time.to_string(),
            is_manifest,
            ended: None,
            tags: vec![run_tag.clone()],
        };
        vec![
            member(format!("{label}-claim"), start, false),
            member(label.to_string(), commit, true),
        ]
    }

    /// A nightly run started at 22:00 in Berlin whose commit snapshot began
    /// after midnight is Tuesday's run, not Wednesday's. Filed by the commit
    /// snapshot's time, Tuesday's and Wednesday's runs shared Wednesday and
    /// `keepDaily: 7` forgot one of the last four runs.
    #[test]
    fn a_run_is_filed_under_the_day_it_started_not_the_day_it_committed() {
        let snaps: Vec<SnapshotMeta> = [
            ("mon", "2026-09-14T20:00:00.5Z", "2026-09-14T21:50:00Z"),
            ("tue", "2026-09-15T20:00:00.5Z", "2026-09-15T22:10:00Z"),
            ("wed", "2026-09-16T20:00:00.5Z", "2026-09-16T21:55:00Z"),
            ("thu", "2026-09-17T20:00:00.5Z", "2026-09-17T21:40:00Z"),
        ]
        .iter()
        .flat_map(|(label, start, commit)| run_started(label, start, commit))
        .collect();
        assert_eq!(
            forgotten(&snaps, &keep_days(7, berlin())),
            Vec::<String>::new()
        );
        assert_eq!(
            forgotten(&snaps, &keep_days(3, berlin())),
            vec!["mon", "mon-claim"]
        );
    }

    /// The walk is newest-first by the same start the days are counted by.
    /// Monday's run started at 23:00 in Berlin and committed at 02:00 on
    /// Tuesday; a manual run started at 00:30 on Tuesday and committed at
    /// 01:00. Walked by commit time, Monday's run came first, took the one
    /// day `keepDaily: 1` allows, and the newest day's run was forgotten.
    #[test]
    fn the_run_that_started_last_is_the_newest() {
        let mut snaps = run_started("mon", "2026-09-14T21:00:00Z", "2026-09-15T00:00:00Z");
        snaps.extend(run_started(
            "tue-manual",
            "2026-09-14T22:30:00Z",
            "2026-09-14T23:00:00Z",
        ));
        assert_eq!(
            forgotten(&snaps, &keep_days(1, berlin())),
            vec!["mon", "mon-claim"]
        );
    }

    /// A tag with no start in it — every one written before cluster
    /// identity — leaves the representative's own time to file the run by;
    /// and a representative whose time does not parse is filed by its tag's
    /// start rather than kept regardless.
    #[test]
    fn a_run_with_no_start_in_its_tag_is_filed_by_its_time_and_the_reverse() {
        let legacy = |label: &str, start: &str, time: &str| {
            let run_tag = format!("platform-{start}");
            SnapshotMeta {
                id: label.to_string(),
                run_tag: run_tag.clone(),
                time: time.to_string(),
                is_manifest: true,
                ended: None,
                tags: vec![run_tag],
            }
        };
        let snaps = vec![
            legacy("mon", "2026-09-14T20:00:00Z", "2026-09-14T22:10:00Z"),
            legacy("tue", "2026-09-15T20:00:00Z", "2026-09-15T21:00:00Z"),
        ];
        assert_eq!(forgotten(&snaps, &keep_days(1, berlin())), vec!["mon"]);

        let mut snaps = run_started("mon", "2026-09-14T20:00:00Z", "");
        snaps.extend(run_started("tue", "2026-09-15T20:00:00Z", "yesterday"));
        assert_eq!(
            forgotten(&snaps, &keep_days(1, berlin())),
            vec!["mon", "mon-claim"]
        );
    }

    #[test]
    fn the_policy_zone_is_the_schedules_or_utc() {
        assert_eq!(policy_zone("").unwrap(), Tz::UTC);
        assert_eq!(policy_zone("  ").unwrap(), Tz::UTC);
        assert_eq!(policy_zone("UTC").unwrap(), Tz::UTC);
        assert_eq!(
            policy_zone("Europe/Berlin").unwrap().name(),
            "Europe/Berlin"
        );
        let why = policy_zone("Mars/Olympus_Mons").unwrap_err();
        assert!(why.contains("\"Mars/Olympus_Mons\""), "{why}");
        assert!(why.contains("in UTC"), "{why}");
        assert_eq!(RetentionPolicy::default().zone, Tz::UTC);
    }

    // --- is_manifest derivation (run_prune's impure seam) ---

    #[test]
    fn derive_manifest_lone_snapshot_is_representative_any_path() {
        // A monolithic run (alone in its group) is a representative even under
        // the `.../data` staging path.
        assert!(derive_manifest(&["/stage/data".into()], true));
    }

    /// A sequential run's per-claim snapshot is never its run's
    /// representative, not even as the only snapshot so far: a sequential
    /// run of one claim still ends with a commit snapshot.
    #[test]
    fn derive_manifest_a_lone_claim_snapshot_is_not_a_representative() {
        assert!(!derive_manifest(&["/stage/claim-0".into()], true));
        assert!(!derive_manifest(
            &["/tmp/apprafter-backup-x/claim-12/".into()],
            true
        ));
        // Only the per-claim directory the engine writes: a monolithic run
        // staged anywhere else stays its own representative.
        for monolithic in [
            "/stage/data",
            "/stage/claim-",
            "/stage/claim-x",
            "/claims-0",
        ] {
            assert!(derive_manifest(&[monolithic.into()], true), "{monolithic}");
        }
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

    // -----------------------------------------------------------------------
    // E3: a shared repository. The planner must never forget a neighbour's run.
    // -----------------------------------------------------------------------

    /// FIRES: the neighbour's runs are older and the policy would drop them,
    /// yet not one of their ids may appear in the forget set.
    #[test]
    fn another_clusters_runs_are_never_forgotten_however_old() {
        // The exact shape of the documented "move to a bigger machine" window:
        // the source (THEIRS) has three days of history, the clone (MINE) has
        // just taken its first snapshot, and the clone prunes right after its
        // own backup — so every one of the source's runs is older than the
        // clone's in its day ⊂ week ⊂ month.
        let mut snaps = Vec::new();
        snaps.extend(mono_run_of(THEIRS, "t-a", "2026-07-10"));
        snaps.extend(seq_run_of(THEIRS, "t-b", "2026-07-11", 2));
        snaps.extend(mono_run_of(THEIRS, "t-c", "2026-07-12"));
        snaps.extend(mono_run_of(MINE, "m-new", "2026-07-13"));
        let policy = RetentionPolicy {
            keep_daily: 1,
            keep_weekly: 0,
            keep_monthly: 0,
            zone: Tz::UTC,
        };
        let plan = plan_prune(&snaps, &policy, MINE, later(), SIX_HOURS);
        assert!(
            plan.forget_ids.is_empty(),
            "a co-tenant's snapshots are not ours to delete, and our own single \
             run is kept by keep_daily=1 — got {:?}",
            plan.forget_ids
        );
        for id in ["t-a-mono", "t-b-manifest", "t-b-claim-0", "t-c-mono"] {
            assert!(
                !plan.forget_ids.contains(&id.to_string()),
                "foreign snapshot {id} must never be forgotten"
            );
        }
    }

    /// DOES NOT FIRE: the same policy, the same ages — but the runs are OURS,
    /// so they are forgotten exactly as before. Without this pair the test
    /// above would also pass on a planner that forgot nothing at all.
    #[test]
    fn our_own_old_runs_are_still_forgotten_by_the_same_policy() {
        let mut snaps = Vec::new();
        snaps.extend(mono_run_of(MINE, "t-a", "2026-07-10"));
        snaps.extend(seq_run_of(MINE, "t-b", "2026-07-11", 2));
        snaps.extend(mono_run_of(MINE, "t-c", "2026-07-12"));
        snaps.extend(mono_run_of(MINE, "m-new", "2026-07-13"));
        let policy = RetentionPolicy {
            keep_daily: 1,
            keep_weekly: 0,
            keep_monthly: 0,
            zone: Tz::UTC,
        };
        let plan = plan_prune(&snaps, &policy, MINE, later(), SIX_HOURS);
        let mut expected = vec![
            "t-a-mono".to_string(),
            "t-b-claim-0".to_string(),
            "t-b-claim-1".to_string(),
            "t-b-manifest".to_string(),
            "t-c-mono".to_string(),
        ];
        expected.sort();
        assert_eq!(plan.forget_ids, expected);
    }

    /// A foreign snapshot must not even be able to influence OUR retention by
    /// occupying a day bucket: it is dropped before grouping, not scored and
    /// then spared.
    #[test]
    fn a_foreign_run_does_not_consume_one_of_our_keep_slots() {
        let mut snaps = Vec::new();
        snaps.extend(mono_run_of(MINE, "m-old", "2026-07-11"));
        snaps.extend(mono_run_of(THEIRS, "t-new", "2026-07-12"));
        // keep_daily 1: if the foreign run were scored it would take the only
        // slot (it is newest) and our run would be forgotten.
        let policy = RetentionPolicy {
            keep_daily: 1,
            keep_weekly: 0,
            keep_monthly: 0,
            zone: Tz::UTC,
        };
        let plan = plan_prune(&snaps, &policy, MINE, later(), SIX_HOURS);
        assert!(
            plan.forget_ids.is_empty(),
            "our only run must keep the daily slot: {:?}",
            plan.forget_ids
        );
    }

    /// The stated, accepted widening: a pre-identity snapshot is treated as
    /// ours, so a repository does not accumulate unreclaimable history.
    #[test]
    fn legacy_snapshots_stay_prunable_as_ours() {
        let mut snaps = Vec::new();
        snaps.extend(legacy_run("l-old", "2026-07-10"));
        snaps.extend(mono_run_of(MINE, "m-new", "2026-07-12"));
        let policy = RetentionPolicy {
            keep_daily: 1,
            keep_weekly: 0,
            keep_monthly: 0,
            zone: Tz::UTC,
        };
        let plan = plan_prune(&snaps, &policy, MINE, later(), SIX_HOURS);
        assert_eq!(
            plan.forget_ids,
            vec!["l-old-legacy".to_string()],
            "a legacy run is in scope and rotates out like any other"
        );
    }

    // -----------------------------------------------------------------------
    // run_prune: the impure seam, against a fake repository that deletes (or
    // refuses to) the way restic 0.18.1 does.
    // -----------------------------------------------------------------------

    /// How the fake store answers a delete.
    #[derive(Clone, Copy)]
    enum Deletes {
        /// Every delete goes through.
        Allowed,
        /// Every delete is refused as S3 refuses it: restic prints the
        /// refusal on stderr and still exits 0 (measured, restic 0.18.1).
        Denied,
        /// Every delete fails for another reason, again with exit 0.
        TimesOut,
        /// The first `forget` goes through; every later one is refused.
        FirstOnly,
    }

    /// A repository restic is run against: `snapshots` lists what is left,
    /// `forget` removes (or not), `prune` is recorded.
    struct FakeRepo {
        snapshots: std::cell::RefCell<Vec<Value>>,
        deletes: Deletes,
        forgets: std::cell::Cell<usize>,
        calls: std::cell::RefCell<Vec<Vec<String>>>,
    }

    impl FakeRepo {
        fn new(listing: &str, deletes: Deletes) -> Self {
            Self {
                snapshots: std::cell::RefCell::new(serde_json::from_str(listing).unwrap()),
                deletes,
                forgets: Default::default(),
                calls: Default::default(),
            }
        }

        fn verbs(&self) -> Vec<String> {
            self.calls.borrow().iter().map(|c| c[0].clone()).collect()
        }

        fn left(&self) -> Vec<String> {
            self.snapshots
                .borrow()
                .iter()
                .map(|s| s["id"].as_str().unwrap().to_string())
                .collect()
        }

        /// The ids a `forget` argv names: everything after `--repo <repo>`.
        fn forget_ids(argv: &[String]) -> Vec<String> {
            argv[3..].to_vec()
        }
    }

    impl crate::ResticRunner for FakeRepo {
        fn run(&self, argv: &[String], p: &str) -> Result<()> {
            self.run_capture(argv, p).map(|_| ())
        }
        fn run_stdout(&self, argv: &[String], p: &str) -> Result<String> {
            self.run_capture(argv, p).map(|o| o.stdout)
        }
        fn run_backup(&self, _argv: &[String], _p: &str) -> Result<Option<String>> {
            unreachable!("prune never takes a backup")
        }
        fn run_capture(&self, argv: &[String], _p: &str) -> Result<crate::ResticOutput> {
            self.calls.borrow_mut().push(argv.to_vec());
            let mut out = crate::ResticOutput::default();
            match argv[0].as_str() {
                "snapshots" => {
                    out.stdout = serde_json::to_string(&*self.snapshots.borrow()).unwrap()
                }
                "forget" => {
                    let n = self.forgets.get();
                    self.forgets.set(n + 1);
                    let ids = Self::forget_ids(argv);
                    let refuse = |why: &str| {
                        ids.iter()
                            .map(|id| {
                                format!(
                                    "Remove(<snapshot/{}>) failed: client.RemoveObject: {why}\n\
                                     unable to remove snapshot/{id} from the repository\n",
                                    &id[..id.len().min(10)]
                                )
                            })
                            .collect::<String>()
                    };
                    match (self.deletes, n) {
                        (Deletes::Allowed, _) | (Deletes::FirstOnly, 0) => self
                            .snapshots
                            .borrow_mut()
                            .retain(|s| !ids.iter().any(|id| s["id"] == id.as_str())),
                        (Deletes::Denied, _) | (Deletes::FirstOnly, _) => {
                            out.stderr = refuse("Access Denied.")
                        }
                        (Deletes::TimesOut, _) => {
                            out.stderr = refuse("dial tcp 10.0.0.1:443: i/o timeout")
                        }
                    }
                }
                "prune" => {}
                other => panic!("prune never runs restic {other}"),
            }
            Ok(out)
        }
    }

    /// Our two old runs (one sequential, one monolithic), our newest run,
    /// and a co-tenant's old run.
    fn shared_listing() -> String {
        format!(
            r#"[
              {{"id":"theirs-old","time":"2026-07-10T03:00:00Z",
                "tags":["{THEIRS}-2026-07-10T03:00:00Z"],"paths":["/s/data"]}},
              {{"id":"mine-old-claim","time":"2026-07-11T03:00:00Z",
                "tags":["{MINE}-2026-07-11T03:00:00Z"],"paths":["/s/claim-0"]}},
              {{"id":"mine-old-commit","time":"2026-07-11T03:00:05Z",
                "tags":["{MINE}-2026-07-11T03:00:00Z"],"paths":["/s/commit"]}},
              {{"id":"mine-mid","time":"2026-07-12T03:00:00Z",
                "tags":["{MINE}-2026-07-12T03:00:00Z"],"paths":["/s/data"]}},
              {{"id":"mine-new","time":"2026-07-13T03:00:00Z",
                "tags":["{MINE}-2026-07-13T03:00:00Z"],"paths":["/s/data"]}}
            ]"#
        )
    }

    const KEEP_ONE_DAY: RetentionPolicy = RetentionPolicy {
        keep_daily: 1,
        keep_weekly: 0,
        keep_monthly: 0,
        zone: Tz::UTC,
    };

    #[test]
    fn run_prune_forgets_only_our_ids_from_a_shared_repository_then_prunes() {
        let r = FakeRepo::new(&shared_listing(), Deletes::Allowed);
        let outcome = run_prune(&r, "s3:repo", "pw", &KEEP_ONE_DAY, MINE, later(), SIX_HOURS)
            .expect("prune runs");
        assert_eq!(
            outcome,
            PruneOutcome::Pruned {
                forgot_snapshots: 3,
                forgot_runs: 2,
                kept_runs: 1,
                unfinished_runs: 0
            }
        );
        assert_eq!(r.left(), vec!["theirs-old", "mine-new"]);
        // One snapshot first, the listing checked, the rest, the listing
        // checked, and only then the prune.
        assert_eq!(
            r.verbs(),
            vec![
                "snapshots",
                "forget",
                "snapshots",
                "forget",
                "snapshots",
                "prune"
            ]
        );
        let calls = r.calls.borrow();
        assert_eq!(FakeRepo::forget_ids(&calls[1]).len(), 1, "{:?}", calls[1]);
        for c in calls.iter().filter(|c| c[0] == "forget") {
            assert!(!c.iter().any(|a| a == "--prune"), "{c:?}");
            assert!(
                !c.iter().any(|a| a == "theirs-old" || a == "mine-new"),
                "{c:?}"
            );
        }
    }

    /// The scoped key ADR 0050 recommends: the first delete is refused, and
    /// nothing else is asked of the store — no second forget, no prune.
    #[test]
    fn a_key_that_may_not_delete_prunes_nothing_and_says_so() {
        let r = FakeRepo::new(&shared_listing(), Deletes::Denied);
        let outcome = run_prune(&r, "s3:repo", "pw", &KEEP_ONE_DAY, MINE, later(), SIX_HOURS)
            .expect("not an error");
        let PruneOutcome::NotPermitted {
            snapshot,
            restic_said,
            would_forget_snapshots,
            would_forget_runs,
        } = &outcome
        else {
            panic!("expected NotPermitted, got {outcome:?}");
        };
        assert_eq!(snapshot, "mine-mid");
        assert!(restic_said.contains("Access Denied"), "{restic_said}");
        assert_eq!((*would_forget_snapshots, *would_forget_runs), (3, 2));
        assert_eq!(r.verbs(), vec!["snapshots", "forget", "snapshots"]);
        assert_eq!(r.left().len(), 5, "every snapshot is still there");
        let said = outcome.describe();
        assert!(said.starts_with("not permitted"), "{said}");
        assert!(said.contains("Access Denied"), "{said}");
        assert!(said.contains("nothing was deleted"), "{said}");
    }

    /// A delete that failed for any other reason is not "not permitted": it
    /// is an error, and still nothing more is asked of the store.
    #[test]
    fn a_delete_that_failed_otherwise_is_an_error_and_prunes_nothing() {
        let r = FakeRepo::new(&shared_listing(), Deletes::TimesOut);
        let err =
            run_prune(&r, "s3:repo", "pw", &KEEP_ONE_DAY, MINE, later(), SIX_HOURS).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("still in the repository"), "{msg}");
        assert!(msg.contains("i/o timeout"), "{msg}");
        assert_eq!(r.verbs(), vec!["snapshots", "forget", "snapshots"]);
    }

    /// The first forget went through and a later one did not: the prune
    /// that would count the survivors' data as unused is never run.
    #[test]
    fn a_forget_that_left_snapshots_behind_is_never_followed_by_a_prune() {
        let r = FakeRepo::new(&shared_listing(), Deletes::FirstOnly);
        let err =
            run_prune(&r, "s3:repo", "pw", &KEEP_ONE_DAY, MINE, later(), SIX_HOURS).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("removed 1 of the 3"), "{msg}");
        assert!(msg.contains("not pruned"), "{msg}");
        assert!(!r.verbs().contains(&"prune".to_string()), "{:?}", r.verbs());
    }

    #[test]
    fn run_prune_on_a_repository_of_only_foreign_snapshots_forgets_nothing() {
        let listing = format!(
            r#"[{{"id":"theirs","time":"2026-07-10T03:00:00Z",
                  "tags":["{THEIRS}-2026-07-10T03:00:00Z"],"paths":["/s/data"]}}]"#
        );
        let r = FakeRepo::new(&listing, Deletes::Allowed);
        let policy = RetentionPolicy {
            keep_daily: 0,
            keep_weekly: 0,
            keep_monthly: 0,
            zone: Tz::UTC,
        };
        let outcome =
            run_prune(&r, "s3:repo", "pw", &policy, MINE, later(), SIX_HOURS).expect("prune runs");
        assert_eq!(
            outcome,
            PruneOutcome::NothingToPrune {
                kept_runs: 0,
                unfinished_runs: 0
            }
        );
        assert_eq!(
            r.verbs(),
            vec!["snapshots"],
            "with nothing of ours to forget, neither forget nor prune runs"
        );
    }

    /// The reported failure, end to end: last night's complete sequential
    /// run and tonight's, still being written — two claim snapshots, no
    /// commit yet — when the weekly check prunes at 06:00. Nothing may be
    /// forgotten, and restic is not asked to delete anything.
    #[test]
    fn run_prune_leaves_a_run_a_backup_is_still_writing_in_the_repository() {
        let listing = format!(
            r#"[
              {{"id":"done-c0","time":"2026-09-26T03:10:00Z",
                "tags":["{MINE}-2026-09-26T03:00:00Z"],"paths":["/s/claim-0"]}},
              {{"id":"done-cm","time":"2026-09-26T03:20:00Z",
                "tags":["{MINE}-2026-09-26T03:00:00Z"],"paths":["/s/commit"]}},
              {{"id":"going-c0","time":"2026-09-27T03:40:00Z",
                "tags":["{MINE}-2026-09-27T03:00:00Z"],"paths":["/s/claim-0"]}},
              {{"id":"going-c1","time":"2026-09-27T04:30:00Z",
                "tags":["{MINE}-2026-09-27T03:00:00Z"],"paths":["/s/claim-1"]}}
            ]"#
        );
        let r = FakeRepo::new(&listing, Deletes::Allowed);
        let outcome = run_prune(
            &r,
            "s3:repo",
            "pw",
            &RetentionPolicy::default(),
            MINE,
            at("2026-09-27T06:00:00Z"),
            SIX_HOURS,
        )
        .expect("prune runs");
        assert_eq!(
            outcome,
            PruneOutcome::NothingToPrune {
                kept_runs: 1,
                unfinished_runs: 1
            }
        );
        assert_eq!(r.left(), vec!["done-c0", "done-cm", "going-c0", "going-c1"]);
        assert_eq!(r.verbs(), vec!["snapshots"]);
        let said = outcome.describe();
        assert!(
            said.ends_with(
                "; 1 unfinished run(s) left alone, as a backup may still be writing them"
            ),
            "{said}"
        );
    }

    #[test]
    fn nothing_past_the_policy_is_nothing_to_prune() {
        let r = FakeRepo::new(&shared_listing(), Deletes::Allowed);
        let outcome = run_prune(
            &r,
            "s3:repo",
            "pw",
            &RetentionPolicy::default(),
            MINE,
            later(),
            SIX_HOURS,
        )
        .unwrap();
        assert_eq!(
            outcome,
            PruneOutcome::NothingToPrune {
                kept_runs: 3,
                unfinished_runs: 0
            }
        );
        assert_eq!(r.verbs(), vec!["snapshots"]);
    }
}
