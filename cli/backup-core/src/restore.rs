// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! 2.6d restore: ordered step-decision state machine (pure, unit-testable).

use serde_json::Value;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RestoreMode {
    /// Restore into an already-running, bootstrapped target (modes a-into-running / b).
    IntoRunning,
    /// Re-provision a fresh cluster in the current target first (mode a).
    Reprovision,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RestoreStep {
    Reprovision,
    RestoreArtifact,
    ApplyPlatformStack,
    /// Create every namespace named in the backup manifest (idempotent SSA of a
    /// bare `Namespace` object). A fresh restore target has only the platform
    /// namespaces; the app namespaces (e.g. `apprafter`) do NOT exist yet, so
    /// the first namespaced apply below would fail `namespaces "<ns>" not found`.
    EnsureNamespaces,
    ApplySourceCredentials,
    /// Apply Argo Apps with `syncPolicy.automated` stripped + AppRafter
    /// `Application` CRs with replicas=0 — claims provision, NO workload pod (H2).
    ApplyAppsGated,
    WaitClaimsBound,
    LoadData,
    ReSealUserSecrets,
    /// Patch Application replicas back to the backed-up values + re-enable Argo
    /// auto-sync — workloads come up on already-loaded data (H2).
    ResumeWorkloads,
    /// `--data-only`: scale the existing app's workload to 0 (+ disable its Argo
    /// auto-sync) so the load doesn't race a running pod.
    SuspendWorkloads,
}

/// The snapshots that together make up ONE backup run.
///
/// A `monolithic` run is a single snapshot carrying everything, so `claims` is
/// empty. A `sequential` run is N per-claim snapshots plus a final commit-point
/// snapshot that carries `crs/`, `secrets/` and `manifest.json` — all sharing
/// one `run-<id>` tag, which is the only thing that groups them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunSnapshots {
    /// The snapshot carrying `manifest.json` — the run's commit point, and the
    /// one a restore must read first.
    pub commit: String,
    /// The per-claim snapshots of the same run, oldest first. Empty for a
    /// monolithic backup.
    pub claims: Vec<String>,
}

/// Group a `restic snapshots --json` listing into the run the caller asked for.
///
/// WHY THIS EXISTS. `restore` used to fetch exactly one snapshot and read the
/// per-claim dumps out of it. For a monolithic backup that is right — one
/// snapshot holds everything. For a SEQUENTIAL backup the payloads live in the
/// other snapshots of the run, so the restore extracted only `crs/`, `secrets/`
/// and `manifest.json`, found no `data/pg`, loaded nothing, and reported
/// success over an empty database (D26).
///
/// The grouping key is the run tag, deliberately, and not a new manifest field:
/// the tag is already written by the backup engine and is therefore present on
/// backups ALREADY IN REPOSITORIES. A manifest flag would only have fixed runs
/// taken after the fix — which is no use to anyone holding a sequential backup
/// today.
///
/// `requested` is the snapshot the user asked to restore: `latest`, or an id /
/// short-id prefix. The commit point is that snapshot; its siblings are every
/// other snapshot sharing at least one tag with it.
pub fn resolve_run_snapshots(
    snapshots_json: &str,
    requested: &str,
) -> Result<RunSnapshots, String> {
    let snaps: Vec<Value> = serde_json::from_str(snapshots_json)
        .map_err(|e| format!("parsing `restic snapshots --json`: {e}"))?;
    if snaps.is_empty() {
        return Err("the repository has no snapshots".to_string());
    }

    let id_of = |s: &Value| {
        s.get("id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    };
    let time_of = |s: &Value| {
        s.get("time")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    };
    let tags_of = |s: &Value| -> Vec<String> {
        s.get("tags")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default()
    };

    // `latest` is restic's own spelling for "newest by time", which is exactly
    // what the sequential writer makes the commit point: it is written LAST.
    let commit = if requested == "latest" {
        snaps
            .iter()
            .max_by_key(|s| time_of(s))
            .ok_or_else(|| "no snapshots to choose from".to_string())?
    } else {
        snaps
            .iter()
            .find(|s| {
                let id = id_of(s);
                id == requested
                    || id.starts_with(requested)
                    || s.get("short_id").and_then(Value::as_str) == Some(requested)
            })
            .ok_or_else(|| format!("no snapshot matching `{requested}` in this repository"))?
    };

    let commit_id = id_of(commit);
    let commit_tags = tags_of(commit);

    // An untagged snapshot cannot be grouped, and must not silently drag in
    // every other untagged snapshot in the repository.
    let mut claims: Vec<(String, String)> = Vec::new();
    if !commit_tags.is_empty() {
        for s in &snaps {
            let id = id_of(s);
            if id == commit_id {
                continue;
            }
            if tags_of(s).iter().any(|t| commit_tags.contains(t)) {
                claims.push((time_of(s), id));
            }
        }
    }
    claims.sort();

    Ok(RunSnapshots {
        commit: commit_id,
        claims: claims.into_iter().map(|(_, id)| id).collect(),
    })
}

/// Decide the ordered restore steps for a mode + `--data-only`.
pub fn restore_steps(mode: RestoreMode, data_only: bool) -> Vec<RestoreStep> {
    use RestoreStep::*;
    if data_only {
        // recover a volume/DB into an existing cluster: suspend the running
        // workload, load, resume — no CR/secret replay (H2 race avoidance).
        return vec![RestoreArtifact, SuspendWorkloads, LoadData, ResumeWorkloads];
    }
    let mut steps = Vec::new();
    if mode == RestoreMode::Reprovision {
        steps.push(Reprovision);
    }
    steps.extend([
        RestoreArtifact,
        ApplyPlatformStack,
        EnsureNamespaces,
        ApplySourceCredentials,
        ApplyAppsGated,
        WaitClaimsBound,
        LoadData,
        ReSealUserSecrets,
        ResumeWorkloads,
    ]);
    steps
}

/// Gate an AppRafter `Application` CR so its claims provision but NO workload
/// pod comes up: set `spec.base.replicas = 0` and every
/// `spec.environments.<env>.replicas = 0` (the latter is set EVEN WHEN the env
/// did not previously carry a `replicas` field, so an env that inherited a
/// non-zero base replica count can't sneak a pod up before `LoadData`).
///
/// This is the load-bearing H2 transform of the restore-into-running flow: the
/// app's ResourceClaims must regenerate (so the fresh connection Secret + PVCs
/// exist for `LoadData`), but the workload must stay down until the data is
/// loaded — `ResumeWorkloads` then patches the recorded replica counts back.
///
/// Returns a fresh `Value`; the input is not mutated. Pure — the unit-tested
/// seam of the gated apply.
pub fn zero_replicas(app_cr: &Value) -> Value {
    let mut out = app_cr.clone();

    // spec.base.replicas = 0 (create base if the CR somehow lacks it; an
    // AppRafter Application always has spec.base, but be defensive).
    {
        let spec = ensure_object(&mut out, "spec");
        let base = ensure_child_object(spec, "base");
        base.insert("replicas".to_string(), Value::from(0));
    }

    // spec.environments.<env>.replicas = 0 for EVERY env key — set even when
    // the env had no replicas field, so an inherited base count can't leak a
    // pod up. Only touch envs that are objects (a non-object env value is
    // schema-invalid and left untouched for the apply to surface).
    if let Some(envs) = out
        .pointer_mut("/spec/environments")
        .and_then(Value::as_object_mut)
    {
        for (_name, env) in envs.iter_mut() {
            if let Some(env_obj) = env.as_object_mut() {
                env_obj.insert("replicas".to_string(), Value::from(0));
            }
        }
    }

    out
}

/// Borrow (creating if absent) the named child object of a JSON object value.
fn ensure_object<'a>(v: &'a mut Value, key: &str) -> &'a mut serde_json::Map<String, Value> {
    if !v.is_object() {
        *v = Value::Object(serde_json::Map::new());
    }
    let obj = v.as_object_mut().expect("just ensured object");
    ensure_child_object(obj, key)
}

/// Borrow (creating if absent) the named child object of a JSON map.
fn ensure_child_object<'a>(
    obj: &'a mut serde_json::Map<String, Value>,
    key: &str,
) -> &'a mut serde_json::Map<String, Value> {
    obj.entry(key.to_string())
        .or_insert_with(|| Value::Object(serde_json::Map::new()));
    obj.get_mut(key)
        .and_then(Value::as_object_mut)
        .expect("entry just inserted as object")
}

#[cfg(test)]
mod tests {

    // ---- D26: a sequential run is a SET of snapshots, not one ----

    fn seq_listing() -> &'static str {
        // Two per-claim snapshots then the commit point, all one run tag —
        // the exact shape run_backup_sequential_with_summary writes.
        r#"[
          {"id":"aaa1","short_id":"aaa1","time":"2026-09-02T19:11:29Z","tags":["platform-run-1"],
           "paths":["/tmp/apprafter-backup-x/claim-0"]},
          {"id":"bbb2","short_id":"bbb2","time":"2026-09-02T19:11:30Z","tags":["platform-run-1"],
           "paths":["/tmp/apprafter-backup-x/claim-1"]},
          {"id":"ccc3","short_id":"ccc3","time":"2026-09-02T19:11:31Z","tags":["platform-run-1"],
           "paths":["/tmp/apprafter-backup-x/commit"]}
        ]"#
    }

    #[test]
    fn latest_is_the_commit_point_and_the_rest_are_its_claims() {
        let r = resolve_run_snapshots(seq_listing(), "latest").unwrap();
        assert_eq!(r.commit, "ccc3", "the commit point is written LAST");
        assert_eq!(r.claims, vec!["aaa1", "bbb2"], "oldest first");
    }

    #[test]
    fn a_monolithic_run_has_no_claim_snapshots() {
        let one = r#"[{"id":"solo","short_id":"solo","time":"2026-09-02T19:00:00Z",
                       "tags":["platform-run-9"],"paths":["/tmp/x"]}]"#;
        let r = resolve_run_snapshots(one, "latest").unwrap();
        assert_eq!(r.commit, "solo");
        assert!(
            r.claims.is_empty(),
            "nothing to merge for a single-snapshot run"
        );
    }

    #[test]
    fn a_different_run_is_never_dragged_in() {
        // THE isolation rule: two runs in one repository must not blend, or a
        // restore would load another backup's data over this one's.
        let two_runs = r#"[
          {"id":"old1","short_id":"old1","time":"2026-09-01T10:00:00Z","tags":["platform-run-0"],"paths":["/a"]},
          {"id":"new1","short_id":"new1","time":"2026-09-02T10:00:00Z","tags":["platform-run-1"],"paths":["/b"]},
          {"id":"new2","short_id":"new2","time":"2026-09-02T10:00:01Z","tags":["platform-run-1"],"paths":["/c"]}
        ]"#;
        let r = resolve_run_snapshots(two_runs, "latest").unwrap();
        assert_eq!(r.commit, "new2");
        assert_eq!(
            r.claims,
            vec!["new1"],
            "the older RUN must not be pulled in"
        );
    }

    #[test]
    fn an_explicit_snapshot_id_selects_its_own_run() {
        // Restoring an older run by id must bring that run's claims, not the
        // newest one's.
        let r = resolve_run_snapshots(seq_listing(), "ccc3").unwrap();
        assert_eq!(r.commit, "ccc3");
        assert_eq!(r.claims, vec!["aaa1", "bbb2"]);
    }

    #[test]
    fn an_untagged_snapshot_groups_with_nothing() {
        // Without a tag there is no run to reconstruct, and guessing would
        // merge unrelated backups.
        let untagged = r#"[
          {"id":"u1","short_id":"u1","time":"2026-09-02T10:00:00Z","tags":[],"paths":["/a"]},
          {"id":"u2","short_id":"u2","time":"2026-09-02T10:00:01Z","tags":[],"paths":["/b"]}
        ]"#;
        let r = resolve_run_snapshots(untagged, "latest").unwrap();
        assert_eq!(r.commit, "u2");
        assert!(r.claims.is_empty());
    }

    #[test]
    fn an_unknown_request_and_an_empty_repo_both_error() {
        assert!(resolve_run_snapshots(seq_listing(), "zzz9").is_err());
        assert!(resolve_run_snapshots("[]", "latest").is_err());
    }
    use super::*;

    #[test]
    fn zero_replicas_gates_base_and_every_env() {
        let app = serde_json::json!({"spec":{"base":{"image":"x","replicas":3},
            "environments":{"dev":{"replicas":2},"prod":{"image":"y"}}}});
        let z = zero_replicas(&app);
        assert_eq!(z["spec"]["base"]["replicas"], 0);
        assert_eq!(z["spec"]["environments"]["dev"]["replicas"], 0);
        assert_eq!(z["spec"]["environments"]["prod"]["replicas"], 0); // set even if absent
                                                                      // Non-replica fields are preserved untouched.
        assert_eq!(z["spec"]["base"]["image"], "x");
        assert_eq!(z["spec"]["environments"]["prod"]["image"], "y");
    }

    #[test]
    fn zero_replicas_handles_app_with_no_environments() {
        let app = serde_json::json!({"spec":{"base":{"image":"x","replicas":5}}});
        let z = zero_replicas(&app);
        assert_eq!(z["spec"]["base"]["replicas"], 0);
        // No environments key invented.
        assert!(z["spec"].get("environments").is_none());
    }

    #[test]
    fn full_restore_into_running_target_gates_workloads_until_after_load() {
        let steps = restore_steps(RestoreMode::IntoRunning, false);
        assert!(!steps.contains(&RestoreStep::Reprovision));
        assert_eq!(steps.first(), Some(&RestoreStep::RestoreArtifact));
        let i = |s| steps.iter().position(|x| *x == s).unwrap();
        // Namespaces must be created after the PlatformStack but BEFORE any
        // namespaced apply (source credentials, apps) — else the first apply
        // fails `namespaces "<ns>" not found` on a fresh target.
        assert!(i(RestoreStep::ApplyPlatformStack) < i(RestoreStep::EnsureNamespaces));
        assert!(i(RestoreStep::EnsureNamespaces) < i(RestoreStep::ApplySourceCredentials));
        assert!(i(RestoreStep::ApplySourceCredentials) < i(RestoreStep::ApplyAppsGated));
        assert!(i(RestoreStep::ApplyAppsGated) < i(RestoreStep::WaitClaimsBound));
        assert!(i(RestoreStep::WaitClaimsBound) < i(RestoreStep::LoadData));
        assert!(i(RestoreStep::LoadData) < i(RestoreStep::ReSealUserSecrets));
        assert_eq!(steps.last(), Some(&RestoreStep::ResumeWorkloads));
        assert!(i(RestoreStep::LoadData) < i(RestoreStep::ResumeWorkloads));
    }

    #[test]
    fn reprovision_mode_prepends_reprovision() {
        let steps = restore_steps(RestoreMode::Reprovision, false);
        assert_eq!(steps.first(), Some(&RestoreStep::Reprovision));
        assert_eq!(steps.last(), Some(&RestoreStep::ResumeWorkloads));
    }

    #[test]
    fn data_only_suspends_loads_resumes() {
        let steps = restore_steps(RestoreMode::IntoRunning, true);
        assert_eq!(
            steps,
            vec![
                RestoreStep::RestoreArtifact,
                RestoreStep::SuspendWorkloads,
                RestoreStep::LoadData,
                RestoreStep::ResumeWorkloads,
            ]
        );
    }
}
