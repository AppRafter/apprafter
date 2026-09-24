// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Status ConfigMap payload builder and in-cluster upsert for the backup runner.

use crate::orchestrate::RunOutcome;

// ---------------------------------------------------------------------------
// Merge helper
// ---------------------------------------------------------------------------

/// Merge the new run's status fields onto the old ConfigMap `data` object.
///
/// The caller passes the `data` object from the *live* CM (if any) as `old`,
/// and the `data` section this run built as `new`. The result starts from
/// `old` and overlays every key from `new`, so that:
///
/// * A success run overlays `lastSuccess` + `lastRunFormat` and CLEARS
///   `lastError` (empty string) so a prior failure's message doesn't linger as
///   if current (E2); the `lastFailure` timestamp is kept as history.
/// * A failure run overlays `lastFailure` + `lastError` + `lastRunFormat`
///   while keeping `lastSuccess` from a prior success.
/// * A check run's `lastCheck*`, `lastPrune*` and `repo*` keys leave the
///   backup's alone, and the other way round.
/// * New repository figures (`repoStatsAt`) move the previous ones to
///   `repoPrev*`, so that one record says how much the repository grew
///   between two checks — when both readings are of the same repository
///   (`repoStatsRepo`). Pointed at a new bucket, the old figures are
///   dropped rather than read as growth.
///
/// Both arguments must be JSON objects; non-object inputs fall back to the
/// new data alone.
pub fn merge_status_data(old: &serde_json::Value, new: &serde_json::Value) -> serde_json::Value {
    let mut merged = match old.as_object() {
        Some(o) => o.clone(),
        None => return new.clone(),
    };
    if let Some(n) = new.as_object() {
        if n.contains_key(REPO_STATS_AT) {
            // The figures are one reading: all of the old one moves, and a
            // figure the new reading lacks is not left standing beside it.
            // A reading of another repository is not a previous one.
            let same_repo = merged.get(REPO_STATS_REPO).is_some()
                && merged.get(REPO_STATS_REPO) == n.get(REPO_STATS_REPO);
            for (key, prev) in REPO_FIGURES.iter().zip(REPO_PREV_FIGURES) {
                match merged.remove(*key) {
                    Some(v) if same_repo => merged.insert(prev.to_string(), v),
                    _ => merged.remove(prev),
                };
            }
        }
        for (k, v) in n {
            merged.insert(k.clone(), v.clone());
        }
    }
    serde_json::Value::Object(merged)
}

/// When the repository figures were read.
pub const REPO_STATS_AT: &str = "repoStatsAt";
/// Which repository they were read from (its URL, which holds no secret).
pub const REPO_STATS_REPO: &str = "repoStatsRepo";
/// The repository figures a check records, and where the previous ones move.
const REPO_FIGURES: [&str; 4] = [REPO_STATS_AT, "repoSnapshots", "repoBlobs", "repoBytes"];
const REPO_PREV_FIGURES: [&str; 4] = [
    "repoPrevStatsAt",
    "repoPrevSnapshots",
    "repoPrevBlobs",
    "repoPrevBytes",
];

// ---------------------------------------------------------------------------
// What a check run records
// ---------------------------------------------------------------------------

/// How a `restic check` ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CheckResult {
    Passed,
    /// Failed, with restic's words (or the stop's).
    Failed(String),
}

/// What became of a prune the runner ran (or was stopped in).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PruneRecord {
    /// It ran to an outcome: pruned, nothing to prune, or not permitted.
    Done(backup_core::prune::PruneOutcome),
    /// It failed, with the reason.
    Failed(String),
}

impl PruneRecord {
    /// The `lastPruneResult` value.
    pub fn result(&self) -> &'static str {
        use backup_core::prune::PruneOutcome as O;
        match self {
            PruneRecord::Done(O::Pruned { .. }) => "pruned",
            PruneRecord::Done(O::NothingToPrune { .. }) => "nothing-to-prune",
            PruneRecord::Done(O::NotPermitted { .. }) => "not-permitted",
            PruneRecord::Failed(_) => "failed",
        }
    }

    /// The `lastPruneDetail` value: what was forgotten, or why not.
    pub fn detail(&self) -> String {
        match self {
            PruneRecord::Done(outcome) => outcome.describe(),
            PruneRecord::Failed(error) => error.clone(),
        }
    }
}

/// `lastCheck` (when), `lastCheckResult` (`passed` / `failed`) and
/// `lastCheckError` (empty on a pass, so an old failure's words do not sit
/// beside a new pass).
pub fn check_record(result: &CheckResult, now: &str) -> serde_json::Value {
    let (verdict, error) = match result {
        CheckResult::Passed => ("passed", String::new()),
        CheckResult::Failed(e) => ("failed", e.clone()),
    };
    serde_json::json!({
        "lastCheck": now,
        "lastCheckResult": verdict,
        "lastCheckError": error,
    })
}

/// `lastPrune` (when), `lastPruneResult` (`pruned`, `nothing-to-prune`,
/// `not-permitted`, `failed`), `lastPruneDetail`, and `lastPruneBy`: `check`
/// for the weekly check Job, `backup` for a backup Job under
/// `enforce: cluster`.
pub fn prune_record(record: &PruneRecord, by: &str, now: &str) -> serde_json::Value {
    serde_json::json!({
        "lastPrune": now,
        "lastPruneResult": record.result(),
        "lastPruneDetail": record.detail(),
        "lastPruneBy": by,
    })
}

/// The repository's size and counts, from `restic stats --mode raw-data`,
/// and the repository they are of. A figure restic did not report is left
/// out rather than written as zero.
pub fn stats_record(
    stats: &backup_core::restic::RepoStats,
    repo: &str,
    now: &str,
) -> serde_json::Value {
    let mut data = serde_json::json!({
        REPO_STATS_AT: now,
        REPO_STATS_REPO: repo,
        "repoBytes": stats.total_size.to_string(),
    });
    if let Some(n) = stats.snapshots {
        data["repoSnapshots"] = serde_json::Value::String(n.to_string());
    }
    if let Some(n) = stats.blob_count {
        data["repoBlobs"] = serde_json::Value::String(n.to_string());
    }
    data
}

// ---------------------------------------------------------------------------
// Kube upsert
// ---------------------------------------------------------------------------

/// Write (create-or-update) the `apprafter-backup-status` ConfigMap in
/// `apprafter-system` with one backup run's outcome, merged with whatever the
/// live CM already holds ([`write_status_data`]).
pub async fn write_status(
    client: &kube::Client,
    outcome: &crate::orchestrate::RunOutcome,
    format: &str,
    now: &str,
) -> cli_core::Result<()> {
    write_status_data(client, &status_configmap(outcome, format, now)["data"]).await
}

/// Write `data` (a JSON object of string values) into the
/// `apprafter-backup-status` ConfigMap, merged with whatever the live CM
/// already holds ([`merge_status_data`]).
///
/// Uses server-side apply with field manager `apprafter-backup` so the write
/// is idempotent even under concurrent runners.  Returns the kube-rs error as
/// a [`cli_core::CliError`] — the caller decides whether to propagate it or
/// treat it as best-effort.
pub async fn write_status_data(
    client: &kube::Client,
    data: &serde_json::Value,
) -> cli_core::Result<()> {
    use k8s_openapi::api::core::v1::ConfigMap;
    use kube::api::{Api, Patch, PatchParams};

    const CM_NAME: &str = "apprafter-backup-status";
    const NS: &str = "apprafter-system";

    let api: Api<ConfigMap> = Api::namespaced(client.clone(), NS);

    // Read the live CM (if it already exists) and merge so that BOTH
    // lastSuccess and lastFailure survive across alternating runs, and a
    // check's record survives a backup's.
    let merged_data = match api
        .get_opt(CM_NAME)
        .await
        .map_err(|e| cli_core::CliError::Other(format!("get status CM {NS}/{CM_NAME}: {e}")))?
    {
        Some(live_cm) => {
            let old_data = live_cm
                .data
                .map(|m| {
                    let obj: serde_json::Map<_, _> = m
                        .into_iter()
                        .map(|(k, v)| (k, serde_json::Value::String(v)))
                        .collect();
                    serde_json::Value::Object(obj)
                })
                .unwrap_or(serde_json::Value::Object(Default::default()));
            merge_status_data(&old_data, data)
        }
        None => data.clone(),
    };

    // Rebuild the full patch document with the merged data.
    let patch_doc = serde_json::json!({
        "apiVersion": "v1",
        "kind": "ConfigMap",
        "metadata": {
            "name": CM_NAME,
            "namespace": NS,
        },
        "data": merged_data,
    });

    let pp = PatchParams::apply("apprafter-backup").force();
    api.patch(CM_NAME, &pp, &Patch::Apply(patch_doc))
        .await
        .map_err(|e| cli_core::CliError::Other(format!("SSA status CM {NS}/{CM_NAME}: {e}")))?;

    Ok(())
}

// ---------------------------------------------------------------------------
// Pure CM builder
// ---------------------------------------------------------------------------

/// Build a Kubernetes ConfigMap JSON payload that records the outcome of one
/// backup run.
///
/// The returned value is a partial patch document — it only includes the fields
/// for *this* run.  The caller (a future `write_status` function) will merge it
/// with the live ConfigMap so that the previous run's fields survive (e.g., a
/// failure run keeps `lastSuccess` from the prior successful run).
///
/// # Fields emitted
/// Always: `lastRunFormat`.
/// On success: `lastSuccess` (set to `now`).
/// On failure: `lastFailure` (set to `now`) + `lastError`.
pub fn status_configmap(outcome: &RunOutcome, format: &str, now: &str) -> serde_json::Value {
    let mut data = serde_json::json!({
        "lastRunFormat": format,
    });

    match outcome {
        RunOutcome::Success { .. } => {
            data["lastSuccess"] = serde_json::Value::String(now.to_string());
            // CLEAR a prior run's `lastError` (E2): the merge preserves keys the
            // new run omits, so without this an old failure's error string
            // lingers next to a fresh `lastSuccess` and `backup status` reads as
            // "currently erroring". Emit an empty string so the merge overwrites
            // it. `lastFailure` (a timestamp) is intentionally kept as "when it
            // last failed" history — only the misleading message is cleared.
            data["lastError"] = serde_json::Value::String(String::new());
        }
        RunOutcome::Failure { error } => {
            data["lastFailure"] = serde_json::Value::String(now.to_string());
            data["lastError"] = serde_json::Value::String(error.clone());
        }
    }

    serde_json::json!({
        "apiVersion": "v1",
        "kind": "ConfigMap",
        "metadata": {
            "name": "apprafter-backup-status",
            "namespace": "apprafter-system",
        },
        "data": data,
    })
}

// ---------------------------------------------------------------------------
// Tests (written first — TDD red phase)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- merge_status_data ----------------------------------------------------

    #[test]
    fn success_run_keeps_lastfailure_ts_but_clears_lasterror() {
        // Mirrors what `status_configmap` emits on Success (E2): lastError="".
        let old = serde_json::json!({
            "lastFailure": "t0",
            "lastError": "boom",
            "lastRunFormat": "monolithic"
        });
        let new = serde_json::json!({
            "lastSuccess": "t1",
            "lastError": "",
            "lastRunFormat": "monolithic"
        });
        let m = merge_status_data(&old, &new);
        assert_eq!(m["lastSuccess"], "t1");
        assert_eq!(m["lastFailure"], "t0"); // kept as history
        assert_eq!(m["lastError"], ""); // stale error cleared
        assert_eq!(m["lastRunFormat"], "monolithic");
    }

    #[test]
    fn merge_overwrites_same_keys() {
        let old = serde_json::json!({"lastSuccess": "t0"});
        let new = serde_json::json!({"lastSuccess": "t1", "lastRunFormat": "seq"});
        let m = merge_status_data(&old, &new);
        assert_eq!(m["lastSuccess"], "t1");
        assert_eq!(m["lastRunFormat"], "seq");
    }

    #[test]
    fn merge_keeps_prior_lastsuccess_on_a_failure_run() {
        let old = serde_json::json!({
            "lastSuccess": "t0",
            "lastRunFormat": "sequential"
        });
        let new = serde_json::json!({
            "lastFailure": "t1",
            "lastError": "oops",
            "lastRunFormat": "sequential"
        });
        let m = merge_status_data(&old, &new);
        assert_eq!(m["lastSuccess"], "t0"); // preserved
        assert_eq!(m["lastFailure"], "t1");
        assert_eq!(m["lastError"], "oops");
    }

    #[test]
    fn merge_with_empty_old_returns_new() {
        let old = serde_json::json!({});
        let new = serde_json::json!({"lastSuccess": "t1"});
        let m = merge_status_data(&old, &new);
        assert_eq!(m["lastSuccess"], "t1");
    }

    #[test]
    fn a_check_record_leaves_the_backups_alone_and_the_other_way_round() {
        let backup = serde_json::json!({
            "lastSuccess": "t0", "lastError": "", "lastRunFormat": "monolithic"
        });
        let check = check_record(&CheckResult::Passed, "t1");
        let m = merge_status_data(&backup, &check);
        assert_eq!(m["lastSuccess"], "t0");
        assert_eq!(m["lastCheck"], "t1");
        assert_eq!(m["lastCheckResult"], "passed");
        let back = merge_status_data(
            &m,
            &status_configmap(
                &crate::orchestrate::RunOutcome::Failure { error: "x".into() },
                "monolithic",
                "t2",
            )["data"],
        );
        assert_eq!(
            back["lastCheck"], "t1",
            "a backup's record keeps the check's"
        );
        assert_eq!(back["lastFailure"], "t2");
    }

    #[test]
    fn a_passing_check_clears_the_last_failed_checks_words() {
        let failed = check_record(&CheckResult::Failed("pack 3f… damaged".into()), "t0");
        assert_eq!(failed["lastCheckResult"], "failed");
        assert_eq!(failed["lastCheckError"], "pack 3f… damaged");
        let m = merge_status_data(&failed, &check_record(&CheckResult::Passed, "t1"));
        assert_eq!(m["lastCheckResult"], "passed");
        assert_eq!(m["lastCheckError"], "");
    }

    #[test]
    fn every_prune_outcome_has_its_own_result_and_says_what_happened() {
        use backup_core::prune::PruneOutcome as O;
        for (record, result, says) in [
            (
                PruneRecord::Done(O::Pruned {
                    forgot_snapshots: 3,
                    forgot_runs: 2,
                    kept_runs: 7,
                    unfinished_runs: 0,
                }),
                "pruned",
                "forgot 3 snapshot(s) of 2 run(s)",
            ),
            (
                PruneRecord::Done(O::NothingToPrune {
                    kept_runs: 4,
                    unfinished_runs: 0,
                }),
                "nothing-to-prune",
                "all 4 run(s)",
            ),
            (
                PruneRecord::Done(O::NotPermitted {
                    snapshot: "ecd0be3219c6a9adb39e".into(),
                    restic_said: "Remove(<snapshot/ecd0be3219>) failed: client.RemoveObject: \
                                  Access Denied.\nunable to remove snapshot/ecd0 from the \
                                  repository"
                        .into(),
                    would_forget_snapshots: 5,
                    would_forget_runs: 5,
                }),
                "not-permitted",
                "refused to delete snapshot ecd0be32 (Remove(<snapshot/ecd0be3219>) failed: \
                 client.RemoveObject: Access Denied.)",
            ),
            (
                PruneRecord::Failed("restic prune: exit status 1".into()),
                "failed",
                "restic prune: exit status 1",
            ),
        ] {
            let data = prune_record(&record, "check", "t1");
            assert_eq!(data["lastPruneResult"], result);
            assert_eq!(data["lastPrune"], "t1");
            assert_eq!(data["lastPruneBy"], "check");
            let detail = data["lastPruneDetail"].as_str().unwrap();
            assert!(detail.contains(says), "{result}: {detail}");
        }
    }

    #[test]
    fn new_repository_figures_keep_the_previous_ones_so_growth_is_visible() {
        use backup_core::restic::RepoStats;
        let first = stats_record(
            &RepoStats {
                total_size: 1000,
                blob_count: Some(10),
                snapshots: Some(2),
            },
            "s3:repo",
            "t0",
        );
        assert_eq!(first["repoBytes"], "1000");
        assert_eq!(first["repoBlobs"], "10");
        assert_eq!(first["repoSnapshots"], "2");
        let m = merge_status_data(&serde_json::json!({}), &first);
        assert!(
            m.get("repoPrevStatsAt").is_none(),
            "nothing before the first"
        );
        let second = stats_record(
            &RepoStats {
                total_size: 1500,
                blob_count: Some(14),
                snapshots: Some(3),
            },
            "s3:repo",
            "t1",
        );
        let m = merge_status_data(&m, &second);
        assert_eq!(m["repoStatsAt"], "t1");
        assert_eq!(m["repoBytes"], "1500");
        assert_eq!(m["repoPrevStatsAt"], "t0");
        assert_eq!(m["repoPrevBytes"], "1000");
        assert_eq!(m["repoPrevBlobs"], "10");
        assert_eq!(m["repoPrevSnapshots"], "2");
        // A record without figures (a backup's, a check's result) moves
        // nothing.
        let m = merge_status_data(&m, &check_record(&CheckResult::Passed, "t2"));
        assert_eq!(m["repoPrevStatsAt"], "t0");
        // A figure restic did not report is not carried as an old one.
        let third = stats_record(
            &RepoStats {
                total_size: 1600,
                blob_count: None,
                snapshots: None,
            },
            "s3:repo",
            "t3",
        );
        let m = merge_status_data(&m, &third);
        assert_eq!(m["repoPrevBlobs"], "14");
        assert!(
            m.get("repoBlobs").is_none(),
            "an old count must not stand beside a new reading: {m}"
        );
        let fourth = stats_record(
            &RepoStats {
                total_size: 1700,
                blob_count: Some(20),
                snapshots: Some(4),
            },
            "s3:repo",
            "t4",
        );
        let m = merge_status_data(&m, &fourth);
        assert_eq!(m["repoPrevStatsAt"], "t3");
        assert_eq!(m["repoPrevBytes"], "1600");
        assert!(m.get("repoPrevBlobs").is_none(), "t3 had no count: {m}");
        // Pointed at another bucket: its first reading has no "before".
        let other = stats_record(
            &RepoStats {
                total_size: 10,
                blob_count: Some(1),
                snapshots: Some(1),
            },
            "s3:elsewhere",
            "t5",
        );
        let m = merge_status_data(&m, &other);
        assert_eq!(m["repoStatsRepo"], "s3:elsewhere");
        for prev in REPO_PREV_FIGURES {
            assert!(m.get(prev).is_none(), "{prev} from another repository: {m}");
        }
    }

    // -- status_configmap -----------------------------------------------------

    #[test]
    fn status_cm_success_fields() {
        let cm = status_configmap(
            &crate::orchestrate::RunOutcome::Success {
                snapshot: Some("s".into()),
            },
            "monolithic",
            "2026-07-17T03:00:00Z",
        );
        assert_eq!(cm["metadata"]["name"], "apprafter-backup-status");
        assert_eq!(cm["metadata"]["namespace"], "apprafter-system");
        assert_eq!(cm["kind"], "ConfigMap");
        assert_eq!(cm["data"]["lastSuccess"], "2026-07-17T03:00:00Z");
        assert_eq!(cm["data"]["lastRunFormat"], "monolithic");
        assert_eq!(cm["data"]["lastError"], ""); // cleared on success (E2)
        assert!(cm["data"].get("lastFailure").is_none());
    }

    #[test]
    fn status_cm_failure_fields() {
        let cm = status_configmap(
            &crate::orchestrate::RunOutcome::Failure {
                error: "boom".into(),
            },
            "sequential",
            "2026-07-17T03:05:00Z",
        );
        assert_eq!(cm["data"]["lastFailure"], "2026-07-17T03:05:00Z");
        assert_eq!(cm["data"]["lastError"], "boom");
        assert!(cm["data"].get("lastSuccess").is_none());
        assert_eq!(cm["data"]["lastRunFormat"], "sequential");
    }

    // -- write_status against a stub apiserver --------------------------------
    //
    // `write_status` decides "create" vs "merge" on how the apiserver answers
    // its GET, through kube-rs' `get_opt` — which reads a `Status` whose REASON
    // is `NotFound` as absent. kube 3 replaced `ErrorResponse` with `Status`
    // under that call, so the three answers that matter are pinned here: absent,
    // present, and forbidden (which must NOT read as absent — the SSA would then
    // overwrite the recorded history with this run's fields alone).

    use std::convert::Infallible;
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::{Arc, Mutex};
    use std::task::{Context, Poll};

    use http::{Request, Response};
    use kube::client::Body;
    use serde_json::{json, Value};
    use tower_service::Service;

    const CM_PATH: &str = "/api/v1/namespaces/apprafter-system/configmaps/apprafter-backup-status";

    /// One request as the stub saw it: method, full URI, JSON body (if any).
    type Seen = (String, String, Option<Value>);

    /// Answers `GET` on the status ConfigMap with a fixed `(code, body)` and
    /// every `PATCH` with the patch body echoed back; records every request
    /// WITH its body, so a test can assert what the SSA actually sent.
    #[derive(Clone)]
    struct StubApiServer {
        get: (u16, Value),
        seen: Arc<Mutex<Vec<Seen>>>,
    }

    impl Service<Request<Body>> for StubApiServer {
        type Response = Response<Body>;
        type Error = Infallible;
        type Future = Pin<Box<dyn Future<Output = Result<Response<Body>, Infallible>> + Send>>;

        fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Infallible>> {
            Poll::Ready(Ok(()))
        }

        fn call(&mut self, req: Request<Body>) -> Self::Future {
            let this = self.clone();
            Box::pin(async move {
                let (parts, body) = req.into_parts();
                let bytes = body.collect_bytes().await.expect("read the request body");
                let sent: Option<Value> = serde_json::from_slice(&bytes).ok();
                let method = parts.method.as_str().to_string();
                this.seen.lock().unwrap().push((
                    method.clone(),
                    parts.uri.to_string(),
                    sent.clone(),
                ));
                let (code, answer) = match (method.as_str(), parts.uri.path()) {
                    ("GET", CM_PATH) => this.get.clone(),
                    ("PATCH", CM_PATH) => (200, sent.unwrap_or(Value::Null)),
                    (_, path) => (
                        404,
                        json!({"kind": "Status", "apiVersion": "v1", "status": "Failure",
                               "reason": "NotFound", "message": format!("{path} not found"),
                               "code": 404}),
                    ),
                };
                Ok(Response::builder()
                    .status(code)
                    .header("content-type", "application/json")
                    .body(Body::from(answer.to_string().into_bytes()))
                    .expect("build stub response"))
            })
        }
    }

    /// Run `write_status` for a FAILURE outcome at `t1` against a stub whose
    /// GET answers `get`; return the result and every request the stub saw.
    fn write_failure_against(get: (u16, Value)) -> (cli_core::Result<()>, Vec<Seen>) {
        let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
        let seen = Arc::new(Mutex::new(Vec::new()));
        let svc = StubApiServer {
            get,
            seen: Arc::clone(&seen),
        };
        let client = {
            let _guard = rt.enter();
            kube::Client::new(svc, "default")
        };
        let outcome = crate::orchestrate::RunOutcome::Failure {
            error: "boom".into(),
        };
        let res = rt.block_on(write_status(&client, &outcome, "monolithic", "t1"));
        let seen = seen.lock().unwrap().clone();
        (res, seen)
    }

    fn status_body(code: u16, reason: &str) -> Value {
        json!({"kind": "Status", "apiVersion": "v1", "status": "Failure",
               "reason": reason, "message": "stub apiserver rejection", "code": code})
    }

    #[test]
    fn write_status_reads_a_notfound_cm_as_absent_and_applies_this_runs_fields() {
        let (res, seen) = write_failure_against((404, status_body(404, "NotFound")));
        res.expect("an absent status CM is created, not an error");

        assert_eq!(seen.len(), 2, "one GET, then one apply: {seen:?}");
        assert_eq!(seen[0].0, "GET");
        let (method, uri, body) = &seen[1];
        assert_eq!(method, "PATCH");
        assert!(
            uri.contains("fieldManager=apprafter-backup") && uri.contains("force=true"),
            "the write must be a forced SSA under the apprafter-backup manager: {uri}"
        );
        assert_eq!(
            body.as_ref().expect("an apply body")["data"],
            json!({"lastFailure": "t1", "lastError": "boom", "lastRunFormat": "monolithic"})
        );
    }

    #[test]
    fn write_status_merges_this_run_onto_the_live_cm() {
        let live = json!({
            "apiVersion": "v1", "kind": "ConfigMap",
            "metadata": {"name": "apprafter-backup-status", "namespace": "apprafter-system"},
            "data": {"lastSuccess": "t0", "lastRunFormat": "sequential"}
        });
        let (res, seen) = write_failure_against((200, live));
        res.expect("merge onto the live CM");

        assert_eq!(
            seen[1].2.as_ref().expect("an apply body")["data"],
            json!({
                "lastSuccess": "t0",
                "lastFailure": "t1",
                "lastError": "boom",
                "lastRunFormat": "monolithic"
            }),
            "the prior success must survive a failure run"
        );
    }

    #[test]
    fn write_status_does_not_read_a_forbidden_cm_as_absent() {
        let (res, seen) = write_failure_against((403, status_body(403, "Forbidden")));
        assert!(res.is_err(), "a forbidden read must be an error");
        assert_eq!(
            seen.iter().map(|s| s.0.as_str()).collect::<Vec<_>>(),
            vec!["GET"],
            "a forbidden read must not be followed by a history-erasing apply"
        );
    }
}
