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
    /// Re-apply the imported TLS certificates captured under `certs/` — as the
    /// plain `kubernetes.io/tls` Secrets they were, labels and annotations
    /// intact (A1).
    ///
    /// Ordered BEFORE `ApplyPlatformStack` because that step is what replays
    /// `gateway.allowedDomains`, and the chart renders the Gateway's
    /// `tls.certificateRefs` straight out of those domains. Landing the
    /// certificate first means the reference is never dangling, not even for
    /// the seconds between the two steps.
    ApplyImportedCerts,
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
///
/// # `latest` in a SHARED repository (E2)
///
/// Two clusters can legitimately write to one repository, so "the newest
/// snapshot" was able to be another cluster's run — which restore would then
/// replay, PlatformStack, secrets, applications and all. `this_cluster_uid` is
/// the restore TARGET's `kube-system` UID, and `latest` now resolves like this:
///
/// * Snapshots this cluster owns ([`crate::cluster::owned_by_this_cluster`] —
///   its own UID, plus legacy snapshots that carry no UID) → newest of those.
///   This is the rollback case: restoring a cluster from its own history.
/// * Nothing of ours, but the repository holds exactly ONE cluster's snapshots
///   → newest of those. This is disaster recovery: the target is a freshly
///   provisioned cluster with a brand-new UID, and there is nothing to confuse
///   it with.
/// * Nothing of ours and MORE THAN ONE other cluster present → refuse, naming
///   them. Picking one would be a guess about which cluster the operator meant
///   to restore, and the wrong guess replays a stranger's secrets.
///
/// `this_cluster_uid` is `None` when the caller has no cluster to ask (a
/// repository inspected with no target). `latest` then falls back to the
/// single-cluster / refuse-if-ambiguous rule, which is the safe half.
///
/// An EXPLICIT snapshot id is always honoured, whatever cluster it belongs to:
/// naming an id is the operator saying which run they mean, and it is the
/// escape hatch the refusal above points at.
pub fn resolve_run_snapshots(
    snapshots_json: &str,
    requested: &str,
    this_cluster_uid: Option<&str>,
) -> Result<RunSnapshots, String> {
    let snaps = parse_snapshot_list(snapshots_json)?;

    // `latest` is restic's own spelling for "newest by time", which is exactly
    // what the sequential writer makes the commit point: it is written LAST —
    // but only ever within ONE cluster's snapshots (E2).
    let commit = if requested == "latest" {
        choose_latest(&snaps, this_cluster_uid)?
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
    let commit_tags = crate::cluster::snapshot_tags(commit);

    // An untagged snapshot cannot be grouped, and must not silently drag in
    // every other untagged snapshot in the repository.
    let mut claims: Vec<(String, String)> = Vec::new();
    if !commit_tags.is_empty() {
        for s in &snaps {
            let id = id_of(s);
            if id == commit_id {
                continue;
            }
            if crate::cluster::snapshot_tags(s)
                .iter()
                .any(|t| commit_tags.contains(t))
            {
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

/// The snapshot id `latest` means for THIS cluster, for a caller that wants
/// one snapshot rather than a whole run.
///
/// Deliberately the SAME rule as [`resolve_run_snapshots`] — both go through
/// [`choose_latest`] — because `backup show latest` feeds an operator's
/// restore decision, and a second rule would let the two disagree about which
/// snapshot `latest` is. `show` inspects; `restore` replays; they must be
/// looking at the same thing (E2).
pub fn resolve_latest_snapshot(
    snapshots_json: &str,
    this_cluster_uid: Option<&str>,
) -> Result<String, String> {
    let snaps = parse_snapshot_list(snapshots_json)?;
    Ok(id_of(choose_latest(&snaps, this_cluster_uid)?))
}

/// Parse `restic snapshots --json` into a non-empty snapshot list.
fn parse_snapshot_list(snapshots_json: &str) -> Result<Vec<Value>, String> {
    let snaps: Vec<Value> = serde_json::from_str(snapshots_json)
        .map_err(|e| format!("parsing `restic snapshots --json`: {e}"))?;
    if snaps.is_empty() {
        return Err("the repository has no snapshots".to_string());
    }
    Ok(snaps)
}

/// A snapshot's restic id, or the empty string when the document has none.
fn id_of(s: &Value) -> String {
    s.get("id")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

/// A snapshot's RFC-3339 time, or the empty string — which sorts first, so an
/// undated snapshot never wins a `max_by_key`.
fn time_of(s: &Value) -> String {
    s.get("time")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

/// The newest snapshot of [`latest_pool`] — THE definition of `latest` here,
/// in one place so `restore` and `backup show` cannot drift apart.
fn choose_latest<'a>(
    snaps: &'a [Value],
    this_cluster_uid: Option<&str>,
) -> Result<&'a Value, String> {
    latest_pool(snaps, this_cluster_uid)?
        .into_iter()
        .max_by_key(|s| time_of(s))
        .ok_or_else(|| "no snapshots to choose from".to_string())
}

/// The snapshots `latest` is allowed to choose between — see
/// [`resolve_run_snapshots`] for the rule and why each branch exists.
///
/// Separated out so the DECISION (which cluster's history is `latest` drawn
/// from) is a single readable function rather than a condition threaded
/// through the selection.
fn latest_pool<'a>(
    snaps: &'a [Value],
    this_cluster_uid: Option<&str>,
) -> Result<Vec<&'a Value>, String> {
    if let Some(uid) = this_cluster_uid {
        let ours: Vec<&Value> = snaps
            .iter()
            .filter(|s| {
                crate::cluster::owned_by_this_cluster(&crate::cluster::snapshot_tags(s), uid)
            })
            .collect();
        if !ours.is_empty() {
            return Ok(ours);
        }
    }

    // Nothing of ours (or no cluster to ask). Every remaining snapshot carries
    // a foreign UID — if they all carry the SAME one there is no ambiguity to
    // resolve, and this is the ordinary disaster-recovery shape: a fresh
    // cluster restoring the only history the repository holds.
    let others = crate::cluster::cluster_uids_in(snaps);
    if others.len() > 1 {
        return Err(format!(
            "this repository holds snapshots from {} different clusters ({}), and none of them \
             are this cluster's — so `latest` cannot say which one you meant. \
             `apprafter backup list --all-clusters` shows every snapshot with the cluster it \
             belongs to; then name the run explicitly — `--snapshot <id>` for a restore, \
             `apprafter backup show <id>` to look inside one first.",
            others.len(),
            others.join(", ")
        ));
    }
    Ok(snaps.iter().collect())
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
        ApplyImportedCerts,
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

    /// The restore target's `kube-system` UID, and a co-tenant's.
    const MINE: &str = "11111111-2222-3333-4444-555555555555";
    const THEIRS: &str = "99999999-8888-7777-6666-555555555555";

    // ---- D26: a sequential run is a SET of snapshots, not one ----
    //
    // These listings deliberately keep the LEGACY tag shape (`platform-run-N`,
    // no cluster UID): they are what a repository written before cluster
    // identity existed holds, and the stated rule is that those snapshots stay
    // restorable as this cluster's. Passing `Some(MINE)` against them is the
    // legacy path, exercised on every one of these cases.

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
        let r = resolve_run_snapshots(seq_listing(), "latest", Some(MINE)).unwrap();
        assert_eq!(r.commit, "ccc3", "the commit point is written LAST");
        assert_eq!(r.claims, vec!["aaa1", "bbb2"], "oldest first");
    }

    #[test]
    fn a_monolithic_run_has_no_claim_snapshots() {
        let one = r#"[{"id":"solo","short_id":"solo","time":"2026-09-02T19:00:00Z",
                       "tags":["platform-run-9"],"paths":["/tmp/x"]}]"#;
        let r = resolve_run_snapshots(one, "latest", Some(MINE)).unwrap();
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
        let r = resolve_run_snapshots(two_runs, "latest", Some(MINE)).unwrap();
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
        let r = resolve_run_snapshots(seq_listing(), "ccc3", Some(MINE)).unwrap();
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
        let r = resolve_run_snapshots(untagged, "latest", Some(MINE)).unwrap();
        assert_eq!(r.commit, "u2");
        assert!(r.claims.is_empty());
    }

    #[test]
    fn an_unknown_request_and_an_empty_repo_both_error() {
        assert!(resolve_run_snapshots(seq_listing(), "zzz9", Some(MINE)).is_err());
        assert!(resolve_run_snapshots("[]", "latest", Some(MINE)).is_err());
    }

    // -----------------------------------------------------------------------
    // E2: `latest` in a repository two clusters share.
    // -----------------------------------------------------------------------

    /// A shared repository: the co-tenant wrote LAST, so an unfiltered
    /// `max_by_key(time)` picks their run — their PlatformStack, their
    /// secrets, their applications.
    fn shared_listing() -> String {
        format!(
            r#"[
              {{"id":"mine1","short_id":"mine1","time":"2026-09-02T03:00:00Z",
                "tags":["{MINE}-2026-09-02T03:00:00Z"],"paths":["/s/data"]}},
              {{"id":"theirs1","short_id":"theirs1","time":"2026-09-02T04:00:00Z",
                "tags":["{THEIRS}-2026-09-02T04:00:00Z"],"paths":["/s/data"]}}
            ]"#
        )
    }

    /// FIRES: `latest` must stay inside this cluster's history even when the
    /// newest snapshot in the repository belongs to the neighbour.
    #[test]
    fn latest_never_crosses_into_another_clusters_run() {
        let r = resolve_run_snapshots(&shared_listing(), "latest", Some(MINE)).unwrap();
        assert_eq!(
            r.commit, "mine1",
            "the newest snapshot is theirs; `latest` must still be OURS"
        );
        assert!(r.claims.is_empty(), "and must not drag their run in");
    }

    /// DOES NOT FIRE: the same listing from the OTHER side resolves to the
    /// other run. Without this the test above would also pass on a resolver
    /// that always returned the oldest snapshot.
    #[test]
    fn latest_from_the_other_clusters_side_resolves_to_that_clusters_run() {
        let r = resolve_run_snapshots(&shared_listing(), "latest", Some(THEIRS)).unwrap();
        assert_eq!(r.commit, "theirs1");
    }

    /// Disaster recovery: a freshly provisioned target has a UID that has
    /// never written a snapshot. With ONE cluster in the repository there is
    /// nothing to confuse, so `latest` still works — a filter that returned
    /// "no snapshots" here would break every DR restore.
    #[test]
    fn a_fresh_target_still_gets_latest_when_the_repo_holds_one_cluster() {
        let only_theirs = format!(
            r#"[
              {{"id":"t1","short_id":"t1","time":"2026-09-01T03:00:00Z",
                "tags":["{THEIRS}-2026-09-01T03:00:00Z"],"paths":["/s/data"]}},
              {{"id":"t2","short_id":"t2","time":"2026-09-02T03:00:00Z",
                "tags":["{THEIRS}-2026-09-02T03:00:00Z"],"paths":["/s/data"]}}
            ]"#
        );
        let fresh = "abcdabcd-0000-0000-0000-abcdabcdabcd";
        let r = resolve_run_snapshots(&only_theirs, "latest", Some(fresh)).unwrap();
        assert_eq!(r.commit, "t2");
    }

    /// …but with TWO foreign clusters and nothing of ours, `latest` is a
    /// guess. Refuse, and say how to choose.
    #[test]
    fn a_fresh_target_refuses_latest_when_the_repo_holds_two_clusters() {
        let fresh = "abcdabcd-0000-0000-0000-abcdabcdabcd";
        let err = resolve_run_snapshots(&shared_listing(), "latest", Some(fresh))
            .expect_err("ambiguous `latest` must refuse, not guess");
        assert!(err.contains("2 different clusters"), "{err}");
        assert!(err.contains("--snapshot"), "{err}");
    }

    /// The escape hatch the refusal names: an explicit id is always honoured,
    /// whatever cluster wrote it.
    #[test]
    fn an_explicit_id_reaches_another_clusters_run_on_purpose() {
        let r = resolve_run_snapshots(&shared_listing(), "theirs1", Some(MINE)).unwrap();
        assert_eq!(r.commit, "theirs1");
    }

    // -----------------------------------------------------------------------
    // `backup show latest` resolves through the SAME rule (E2, loose end 2).
    // -----------------------------------------------------------------------

    /// FIRES: the read-only inspector must not display the co-tenant's newest
    /// snapshot — it is what an operator reads before deciding what to
    /// restore, so a foreign answer here becomes a foreign restore.
    #[test]
    fn resolve_latest_snapshot_stays_inside_this_clusters_history() {
        let id = resolve_latest_snapshot(&shared_listing(), Some(MINE)).unwrap();
        assert_eq!(id, "mine1", "the newest in the repository is theirs");
    }

    /// DOES NOT FIRE: from the other side the same listing resolves to the
    /// other run — so the test above is not passing on a resolver that simply
    /// returns the oldest snapshot.
    #[test]
    fn resolve_latest_snapshot_from_the_other_side_resolves_to_that_cluster() {
        let id = resolve_latest_snapshot(&shared_listing(), Some(THEIRS)).unwrap();
        assert_eq!(id, "theirs1");
    }

    /// The two entry points agree BY CONSTRUCTION — the property that makes
    /// `show` a trustworthy input to a `restore` decision. A second rule would
    /// let an operator inspect one snapshot and replay another.
    #[test]
    fn show_and_restore_resolve_latest_to_the_same_snapshot() {
        for uid in [Some(MINE), Some(THEIRS), None] {
            let shown = resolve_latest_snapshot(&shared_listing(), uid);
            let restored =
                resolve_run_snapshots(&shared_listing(), "latest", uid).map(|r| r.commit);
            assert_eq!(shown, restored, "uid={uid:?}");
        }
    }

    /// Ambiguity refuses here too, and names both spellings of "say which one"
    /// — the inspector's is a positional argument, not `--snapshot`.
    #[test]
    fn resolve_latest_snapshot_refuses_an_ambiguous_repository() {
        let fresh = "abcdabcd-0000-0000-0000-abcdabcdabcd";
        let err = resolve_latest_snapshot(&shared_listing(), Some(fresh))
            .expect_err("two foreign clusters and none of ours is a guess");
        assert!(err.contains("2 different clusters"), "{err}");
        assert!(err.contains("--snapshot"), "{err}");
        assert!(err.contains("apprafter backup show <id>"), "{err}");
    }

    #[test]
    fn resolve_latest_snapshot_reports_an_empty_repository() {
        assert!(resolve_latest_snapshot("[]", Some(MINE)).is_err());
    }

    /// Legacy snapshots are ours by the stated assumption, so they are what
    /// `latest` picks even when an identified foreign run is newer.
    #[test]
    fn legacy_snapshots_count_as_ours_for_latest() {
        let mixed = format!(
            r#"[
              {{"id":"old","short_id":"old","time":"2026-09-01T03:00:00Z",
                "tags":["platform-2026-09-01T03:00:00Z"],"paths":["/s/data"]}},
              {{"id":"theirs","short_id":"theirs","time":"2026-09-02T03:00:00Z",
                "tags":["{THEIRS}-2026-09-02T03:00:00Z"],"paths":["/s/data"]}}
            ]"#
        );
        let r = resolve_run_snapshots(&mixed, "latest", Some(MINE)).unwrap();
        assert_eq!(r.commit, "old");
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

    /// A1: the certificate has to be in the cluster before the domains that
    /// reference it are, or the chart renders a Gateway whose
    /// `tls.certificateRefs` names a Secret that is not there yet.
    #[test]
    fn imported_certs_are_applied_before_the_domains_that_reference_them() {
        for mode in [RestoreMode::IntoRunning, RestoreMode::Reprovision] {
            let steps = restore_steps(mode, false);
            let i = |s| steps.iter().position(|x| *x == s).unwrap();
            assert!(
                i(RestoreStep::RestoreArtifact) < i(RestoreStep::ApplyImportedCerts),
                "the certificate is read off the restored artifact ({mode:?})"
            );
            assert!(
                i(RestoreStep::ApplyImportedCerts) < i(RestoreStep::ApplyPlatformStack),
                "the certificate must land BEFORE gateway.allowedDomains ({mode:?})"
            );
        }
    }

    /// `--data-only` replays no config at all — no CRs, no secrets, and so no
    /// certificate either. Applying one there would be a config write from the
    /// mode that exists precisely to make none.
    #[test]
    fn a_data_only_restore_applies_no_imported_certs() {
        let steps = restore_steps(RestoreMode::IntoRunning, true);
        assert!(!steps.contains(&RestoreStep::ApplyImportedCerts));
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
