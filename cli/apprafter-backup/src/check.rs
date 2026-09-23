// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The weekly check Job: `apprafter-backup check`.
//!
//! One run, three steps, each recorded in the status ConfigMap as soon as it
//! is known:
//!
//! 1. `restic check` at the configured depth (after a non-fatal `restic
//!    unlock`, as the backup opens). A check that fails ends the run: exit 1,
//!    `lastCheckResult: failed` with restic's words, the failure webhook. A
//!    repository whose integrity is in doubt is never pruned.
//! 2. Under `retention.enforce: check` (the platform default), and only after
//!    a check that passed: the same run-aware prune `apprafter backup prune`
//!    and `enforce: cluster` run ([`backup_core::prune::run_prune`]), scoped
//!    to this cluster's snapshots. How far it gets is up to the cluster's S3
//!    key. A key that may delete prunes; the scoped key ADR 0050 recommends
//!    may not, and the prune is recorded `not-permitted` with nothing deleted
//!    — the first delete is refused and nothing more is asked of the store.
//!    Neither that nor a prune that fails fails the check: the check passed,
//!    and retention is reported on its own (the `BackupRetention` condition).
//! 3. The repository's size and counts (`restic stats --mode raw-data`), so
//!    that a repository nothing prunes is seen growing. Best-effort.
//!
//! Under `enforce: cluster` the backup Job prunes, and under `operator`
//! nothing in the cluster does: the check Job then checks and measures only.

use std::sync::{Arc, Mutex};

use backup_core::prune::{run_prune, RetentionPolicy};
use backup_core::restic::{
    parse_repo_stats, restic_check_depth_argv, restic_stats_argv, restic_unlock_argv, CheckDepth,
    RepoStats,
};
use backup_core::ResticRunner;
use cli_core::Result;

use crate::config::Enforce;
use crate::status::{check_record, prune_record, stats_record, CheckResult, PruneRecord};

/// The step a check run is in: what a stop records its failure against.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum CheckPhase {
    /// The unlock and the check. A stop here is a check that did not pass.
    #[default]
    Check,
    /// The prune after a passing check. A stop here is a prune that failed;
    /// the check's pass is already recorded.
    Prune,
    /// Reading the repository's figures. The check and the prune are
    /// recorded; a stop here has nothing of theirs to add.
    Stats,
}

/// The step under way, shared between the run and its stop.
#[derive(Clone, Debug, Default)]
pub struct PhaseCell(Arc<Mutex<CheckPhase>>);

impl PhaseCell {
    pub fn get(&self) -> CheckPhase {
        *self.0.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn set(&self, phase: CheckPhase) {
        *self.0.lock().unwrap_or_else(|p| p.into_inner()) = phase;
    }
}

/// What the check run is asked to do.
pub struct CheckPlan<'a> {
    pub repo: &'a str,
    pub passphrase: &'a str,
    pub depth: &'a CheckDepth,
    pub enforce: Enforce,
    pub retention: &'a RetentionPolicy,
}

/// What the check run did.
#[derive(Debug, PartialEq, Eq)]
pub struct CheckRun {
    pub check: CheckResult,
    /// `None` when this run did not prune: the check failed, or retention is
    /// not this Job's to enforce.
    pub prune: Option<PruneRecord>,
    /// `None` when the figures were not read (a failed check) or could not be.
    pub stats: Option<RepoStats>,
}

impl CheckRun {
    /// 1 when the check did not pass, else 0 — whatever the prune did. The
    /// Job's own ending is what `BackupHealthy` reads, and it is about the
    /// repository's integrity; retention has its own condition.
    pub fn exit_code(&self) -> i32 {
        match self.check {
            CheckResult::Passed => 0,
            CheckResult::Failed(_) => 1,
        }
    }

    /// The failure to post to the failure webhook, as `(phase, error)`: a
    /// failed check, or a prune that failed. A prune the key does not permit
    /// is the recommended setup working as designed and posts nothing.
    pub fn failure(&self) -> Option<(&'static str, String)> {
        match (&self.check, &self.prune) {
            (CheckResult::Failed(e), _) => Some(("check", e.clone())),
            (CheckResult::Passed, Some(PruneRecord::Failed(e))) => Some(("prune", e.clone())),
            _ => None,
        }
    }
}

/// Run the check, the prune and the figures (see the module docs).
///
/// `cluster_uid` reads this cluster's `kube-system` UID, which scopes the
/// prune to its own snapshots; it is called only when a prune is due.
/// `record` receives each step's status fields as soon as they are known,
/// and `now` gives the time each is recorded at.
pub fn run_check(
    r: &dyn ResticRunner,
    plan: &CheckPlan,
    phase: &PhaseCell,
    cluster_uid: &mut dyn FnMut() -> Result<String>,
    now: &dyn Fn() -> String,
    record: &mut dyn FnMut(serde_json::Value),
) -> CheckRun {
    phase.set(CheckPhase::Check);
    // A stale lock from a run that was killed would fail the check. Not
    // fatal: a repository with no lock answers with an error too.
    if let Err(e) = r.run(&restic_unlock_argv(plan.repo), plan.passphrase) {
        eprintln!("unlock (non-fatal): {e}");
    }
    let check = match r.run(
        &restic_check_depth_argv(plan.repo, plan.depth),
        plan.passphrase,
    ) {
        Ok(()) => {
            eprintln!("check passed ({})", plan.depth);
            CheckResult::Passed
        }
        Err(e) => {
            eprintln!("check failed: {e}");
            CheckResult::Failed(e.to_string())
        }
    };
    record(check_record(&check, &now()));
    if check != CheckResult::Passed {
        return CheckRun {
            check,
            prune: None,
            stats: None,
        };
    }

    let prune = if plan.enforce == Enforce::Check {
        phase.set(CheckPhase::Prune);
        let outcome = cluster_uid()
            .and_then(|uid| run_prune(r, plan.repo, plan.passphrase, plan.retention, &uid));
        let pruned = match outcome {
            Ok(o) => PruneRecord::Done(o),
            Err(e) => PruneRecord::Failed(e.to_string()),
        };
        eprintln!("prune: {}", pruned.detail());
        record(prune_record(&pruned, "check", &now()));
        Some(pruned)
    } else {
        eprintln!(
            "prune: not this Job's (retention.enforce is {}), so nothing was pruned",
            plan.enforce.as_str()
        );
        None
    };

    phase.set(CheckPhase::Stats);
    let stats = match r.run_stdout(&restic_stats_argv(plan.repo, None), plan.passphrase) {
        Ok(out) => {
            let parsed = parse_repo_stats(&out);
            if parsed.is_none() {
                eprintln!(
                    "stats (non-fatal): restic printed no figures: {}",
                    out.trim()
                );
            }
            parsed
        }
        Err(e) => {
            eprintln!("stats (non-fatal): {e}");
            None
        }
    };
    if let Some(s) = &stats {
        eprintln!(
            "repository: {} bytes stored, {} snapshot(s), {} blob(s)",
            s.total_size,
            s.snapshots.map_or("?".to_string(), |n| n.to_string()),
            s.blob_count.map_or("?".to_string(), |n| n.to_string())
        );
        record(stats_record(s, plan.repo, &now()));
    }
    CheckRun {
        check,
        prune,
        stats,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use backup_core::prune::PruneOutcome;
    use backup_core::ResticOutput;
    use cli_core::CliError;
    use std::cell::RefCell;

    const MINE: &str = "11111111-2222-3333-4444-555555555555";

    /// A repository restic is run against, with the answers the test sets.
    struct FakeRepo {
        check_fails: bool,
        deletes_denied: bool,
        stats: Option<&'static str>,
        snapshots: RefCell<Vec<serde_json::Value>>,
        calls: RefCell<Vec<String>>,
    }

    impl FakeRepo {
        /// Three daily runs of this cluster: with keep-daily 1, two are past
        /// the policy.
        fn new() -> Self {
            let snaps = ["2026-09-18", "2026-09-19", "2026-09-20"]
                .iter()
                .enumerate()
                .map(|(i, day)| {
                    serde_json::json!({
                        "id": format!("snap{i}-0123456789abcdef"),
                        "time": format!("{day}T03:00:00Z"),
                        "tags": [format!("{MINE}-{day}T03:00:00Z")],
                        "paths": ["/staging/x/data"],
                    })
                })
                .collect();
            Self {
                check_fails: false,
                deletes_denied: false,
                stats: Some(r#"{"total_size":4096,"total_blob_count":12,"snapshots_count":3}"#),
                snapshots: RefCell::new(snaps),
                calls: RefCell::new(Vec::new()),
            }
        }

        fn verbs(&self) -> Vec<String> {
            self.calls.borrow().clone()
        }
    }

    impl ResticRunner for FakeRepo {
        fn run(&self, argv: &[String], p: &str) -> Result<()> {
            self.run_capture(argv, p).map(|_| ())
        }
        fn run_stdout(&self, argv: &[String], p: &str) -> Result<String> {
            self.run_capture(argv, p).map(|o| o.stdout)
        }
        fn run_backup(&self, _: &[String], _: &str) -> Result<Option<String>> {
            unreachable!("a check run takes no backup")
        }
        fn run_capture(&self, argv: &[String], _: &str) -> Result<ResticOutput> {
            let verb = argv[0].clone();
            self.calls.borrow_mut().push(match verb.as_str() {
                "check" => argv[3..]
                    .iter()
                    .fold(verb.clone(), |a, b| format!("{a} {b}")),
                _ => verb.clone(),
            });
            let mut out = ResticOutput::default();
            match verb.as_str() {
                "unlock" | "prune" => {}
                "check" if self.check_fails => {
                    return Err(CliError::Other(
                        "restic check: pack 5e1f… contains 1 error: blob damaged".into(),
                    ))
                }
                "check" => {}
                "snapshots" => {
                    out.stdout = serde_json::to_string(&*self.snapshots.borrow()).unwrap()
                }
                "forget" if self.deletes_denied => {
                    out.stderr = format!(
                        "Remove(<snapshot/{}>) failed: client.RemoveObject: Access Denied.\n",
                        &argv[3][..10]
                    )
                }
                "forget" => self
                    .snapshots
                    .borrow_mut()
                    .retain(|s| !argv[3..].iter().any(|id| s["id"] == id.as_str())),
                "stats" => match self.stats {
                    Some(s) => out.stdout = s.to_string(),
                    None => return Err(CliError::Other("stats: Access Denied".into())),
                },
                other => panic!("a check run never runs restic {other}"),
            }
            Ok(out)
        }
    }

    struct Run {
        run: CheckRun,
        records: Vec<serde_json::Value>,
        uid_reads: usize,
    }

    fn run(r: &FakeRepo, enforce: Enforce, depth: CheckDepth) -> Run {
        let retention = RetentionPolicy {
            keep_daily: 1,
            keep_weekly: 0,
            keep_monthly: 0,
        };
        let plan = CheckPlan {
            repo: "s3:repo",
            passphrase: "pw",
            depth: &depth,
            enforce,
            retention: &retention,
        };
        let mut records = Vec::new();
        let mut uid_reads = 0;
        let run = run_check(
            r,
            &plan,
            &PhaseCell::default(),
            &mut || {
                uid_reads += 1;
                Ok(MINE.to_string())
            },
            &|| "t".to_string(),
            &mut |v| records.push(v),
        );
        Run {
            run,
            records,
            uid_reads,
        }
    }

    #[test]
    fn a_passing_check_is_followed_by_the_prune_and_the_figures() {
        let r = FakeRepo::new();
        let got = run(&r, Enforce::Check, CheckDepth::Subset("10%".into()));
        assert_eq!(got.run.check, CheckResult::Passed);
        assert_eq!(
            got.run.prune,
            Some(PruneRecord::Done(PruneOutcome::Pruned {
                forgot_snapshots: 2,
                forgot_runs: 2,
                kept_runs: 1
            }))
        );
        assert_eq!(
            r.verbs(),
            vec![
                "unlock",
                "check --read-data-subset=10%",
                "snapshots",
                "forget",
                "snapshots",
                "forget",
                "snapshots",
                "prune",
                "stats"
            ]
        );
        assert_eq!(got.uid_reads, 1);
        // Recorded step by step: the check first, so a run killed during
        // the prune still says the check passed.
        assert_eq!(got.records.len(), 3);
        assert_eq!(got.records[0]["lastCheckResult"], "passed");
        assert_eq!(got.records[1]["lastPruneResult"], "pruned");
        assert_eq!(got.records[1]["lastPruneBy"], "check");
        assert_eq!(got.records[2]["repoBlobs"], "12");
        assert_eq!(got.run.exit_code(), 0);
        assert_eq!(got.run.failure(), None);
    }

    /// The rule the owner set: a check that did not pass never prunes.
    #[test]
    fn a_failed_check_prunes_nothing_and_fails_the_run() {
        let r = FakeRepo {
            check_fails: true,
            ..FakeRepo::new()
        };
        let got = run(&r, Enforce::Check, CheckDepth::Structure);
        assert!(matches!(got.run.check, CheckResult::Failed(ref e) if e.contains("damaged")));
        assert_eq!(got.run.prune, None);
        assert_eq!(r.verbs(), vec!["unlock", "check"], "no prune, no figures");
        assert_eq!(got.uid_reads, 0);
        assert_eq!(got.records.len(), 1);
        assert_eq!(got.records[0]["lastCheckResult"], "failed");
        assert!(got.records[0]["lastCheckError"]
            .as_str()
            .unwrap()
            .contains("damaged"));
        assert_eq!(got.run.exit_code(), 1);
        assert_eq!(got.run.failure().map(|f| f.0), Some("check"));
    }

    /// The scoped key: the check passes, the prune is not permitted,
    /// nothing is deleted, and the run still succeeds — loudly recorded,
    /// not failed.
    #[test]
    fn a_key_that_may_not_delete_leaves_the_check_passed_and_the_prune_not_permitted() {
        let r = FakeRepo {
            deletes_denied: true,
            ..FakeRepo::new()
        };
        let got = run(&r, Enforce::Check, CheckDepth::Structure);
        assert_eq!(got.run.check, CheckResult::Passed);
        assert!(
            matches!(
                got.run.prune,
                Some(PruneRecord::Done(PruneOutcome::NotPermitted { .. }))
            ),
            "{:?}",
            got.run.prune
        );
        assert_eq!(r.snapshots.borrow().len(), 3, "nothing deleted");
        assert!(!r.verbs().contains(&"prune".to_string()), "{:?}", r.verbs());
        assert_eq!(got.records[1]["lastPruneResult"], "not-permitted");
        assert!(got.records[1]["lastPruneDetail"]
            .as_str()
            .unwrap()
            .contains("Access Denied"));
        // The figures are still read: they are what shows the growth.
        assert_eq!(got.records[2]["repoSnapshots"], "3");
        assert_eq!(got.run.exit_code(), 0);
        assert_eq!(
            got.run.failure(),
            None,
            "the recommended setup posts nothing"
        );
    }

    #[test]
    fn retention_that_is_not_this_jobs_is_not_pruned_here() {
        for enforce in [Enforce::Cluster, Enforce::Operator] {
            let r = FakeRepo::new();
            let got = run(&r, enforce, CheckDepth::Full);
            assert_eq!(got.run.check, CheckResult::Passed);
            assert_eq!(got.run.prune, None, "{enforce:?}");
            assert_eq!(
                r.verbs(),
                vec!["unlock", "check --read-data", "stats"],
                "{enforce:?}"
            );
            assert_eq!(got.uid_reads, 0, "{enforce:?}");
            assert_eq!(got.records.len(), 2, "{enforce:?}");
            assert!(got.records.iter().all(|r| r.get("lastPrune").is_none()));
        }
    }

    #[test]
    fn a_cluster_identity_that_cannot_be_read_fails_the_prune_not_the_check() {
        let r = FakeRepo::new();
        let retention = RetentionPolicy::default();
        let depth = CheckDepth::Structure;
        let plan = CheckPlan {
            repo: "s3:repo",
            passphrase: "pw",
            depth: &depth,
            enforce: Enforce::Check,
            retention: &retention,
        };
        let mut records = Vec::new();
        let got = run_check(
            &r,
            &plan,
            &PhaseCell::default(),
            &mut || {
                Err(CliError::Other(
                    "namespaces \"kube-system\" is forbidden".into(),
                ))
            },
            &|| "t".to_string(),
            &mut |v| records.push(v),
        );
        assert_eq!(got.check, CheckResult::Passed);
        assert!(matches!(got.prune, Some(PruneRecord::Failed(ref e)) if e.contains("forbidden")));
        assert_eq!(records[1]["lastPruneResult"], "failed");
        assert_eq!(got.exit_code(), 0);
        assert_eq!(got.failure().map(|f| f.0), Some("prune"));
        assert!(!r.verbs().contains(&"forget".to_string()));
    }

    #[test]
    fn figures_that_cannot_be_read_record_nothing_and_fail_nothing() {
        let r = FakeRepo {
            stats: None,
            ..FakeRepo::new()
        };
        let got = run(&r, Enforce::Check, CheckDepth::Structure);
        assert_eq!(got.run.stats, None);
        assert_eq!(got.records.len(), 2, "the check and the prune only");
        assert_eq!(got.run.exit_code(), 0);
    }

    #[test]
    fn the_phase_follows_the_run() {
        let r = FakeRepo::new();
        let phase = PhaseCell::default();
        assert_eq!(phase.get(), CheckPhase::Check);
        let retention = RetentionPolicy::default();
        let depth = CheckDepth::Structure;
        let plan = CheckPlan {
            repo: "s3:repo",
            passphrase: "pw",
            depth: &depth,
            enforce: Enforce::Check,
            retention: &retention,
        };
        let seen = RefCell::new(Vec::new());
        run_check(
            &r,
            &plan,
            &phase,
            &mut || {
                seen.borrow_mut().push(phase.get());
                Ok(MINE.to_string())
            },
            &|| "t".to_string(),
            &mut |_| seen.borrow_mut().push(phase.get()),
        );
        assert_eq!(
            *seen.borrow(),
            vec![
                CheckPhase::Check,
                CheckPhase::Prune,
                CheckPhase::Prune,
                CheckPhase::Stats
            ]
        );
    }
}
