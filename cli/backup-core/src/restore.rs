// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! 2.6d restore: ordered step-decision state machine (pure, unit-testable).

use chrono::{DateTime, Utc};
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
    /// For `latest`: the runs NEWER than this one that it passed over because
    /// they never completed ([`UnfinishedRun`]). Always empty for a snapshot
    /// named by id.
    pub passed_over: Vec<UnfinishedRun>,
}

/// A backup run no snapshot of which carries `manifest.json`: a sequential run
/// stopped before its commit snapshot (a SIGINT, a kill, a Job's deadline), or
/// one a backup is still writing — from the repository alone the two look the
/// same.
///
/// `latest` never resolves to one ([`resolve_run_snapshots`]), and reports the
/// ones newer than the run it chose, so an operator is not left believing the
/// newest backup is the one being restored.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnfinishedRun {
    /// The run tag its snapshots share.
    pub tag: String,
    /// How many snapshots it holds.
    pub snapshots: usize,
    /// The newest of their times, as restic reports it (RFC 3339, with the
    /// writer's own UTC offset). "Newest" is by the clock, not the string.
    pub newest: String,
}

/// What `latest` means for a caller that wants one snapshot rather than a
/// whole run ([`resolve_latest_snapshot`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LatestSnapshot {
    /// The snapshot carrying the newest complete run's `manifest.json`.
    pub id: String,
    /// The unfinished runs newer than it, as in [`RunSnapshots::passed_over`].
    pub passed_over: Vec<UnfinishedRun>,
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
/// * This cluster has snapshots carrying its OWN UID → newest of those plus any
///   legacy ones ([`crate::cluster::owned_by_this_cluster`]). This is the
///   rollback case: restoring a cluster from its own history.
/// * Nothing carrying our UID, but the repository holds at most ONE identified
///   cluster → newest of EVERYTHING, legacy and identified alike. This is
///   disaster recovery and the upgrade shape: the target is a freshly
///   provisioned cluster with a brand-new UID, and there is nothing to confuse
///   it with.
///
///   Note what the first rule must NOT be: legacy snapshots count as owned, so
///   testing ownership alone reports a history this cluster does not have. A
///   repository holding legacy runs plus one new identified run would then
///   collapse the pool to the legacy ones and restore the newest of THOSE,
///   silently passing over the run just taken — observed in the field after an
///   upgrade.
/// * Nothing of ours and MORE THAN ONE other cluster present → refuse, naming
///   them. Picking one would be a guess about which cluster the operator meant
///   to restore, and the wrong guess replays a stranger's secrets.
///
/// `this_cluster_uid` is `None` when the caller has no cluster to ask (a
/// repository inspected with no target). `latest` then falls back to the
/// single-cluster / refuse-if-ambiguous rule, which is the safe half.
///
/// # `latest` is the newest COMPLETE run
///
/// Inside that pool, `latest` is the newest snapshot that completes its run —
/// the one carrying `manifest.json`, by the rule the prune plans with
/// ([`crate::prune::derive_manifest`]). It used to be the newest snapshot of
/// any kind, and after a sequential backup was stopped between its claims
/// (a SIGINT, a kill, a Job's deadline) that was a per-claim snapshot of a run
/// with no manifest: `backup show` then said the repository held something
/// other than a backup, and a restore with no `--snapshot` failed on the
/// missing manifest until a newer complete run landed — so the restore right
/// after a backup died, the one disaster recovery needs, was the one that
/// could not run. The prune already treats such a run as unfinished and never
/// as a run to keep; `latest` now agrees with it. The unfinished runs newer
/// than the one chosen are returned in [`RunSnapshots::passed_over`], so the
/// caller says so rather than skipping them silently.
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

    // `latest` is the newest snapshot that completes its run — which is what
    // the sequential writer makes the commit point: it is written LAST — and
    // only ever within ONE cluster's snapshots (E2).
    let (commit, passed_over) = if requested == "latest" {
        let latest = choose_latest(&snaps, this_cluster_uid)?;
        (latest.commit, latest.passed_over)
    } else {
        let named = snaps
            .iter()
            .find(|s| {
                let id = id_of(s);
                id == requested
                    || id.starts_with(requested)
                    || s.get("short_id").and_then(Value::as_str) == Some(requested)
            })
            .ok_or_else(|| format!("no snapshot matching `{requested}` in this repository"))?;
        (named, Vec::new())
    };

    let commit_id = id_of(commit);
    let commit_tags = crate::cluster::snapshot_tags(commit);

    // An untagged snapshot cannot be grouped, and must not silently drag in
    // every other untagged snapshot in the repository.
    let mut claims: Vec<(Option<DateTime<Utc>>, String)> = Vec::new();
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
                claims.push((instant_of(s), id));
            }
        }
    }
    // Oldest first by the clock: the strings of one run need not order as
    // its instants do (a run across the autumn clock change).
    claims.sort();

    Ok(RunSnapshots {
        commit: commit_id,
        claims: claims.into_iter().map(|(_, id)| id).collect(),
        passed_over,
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
) -> Result<LatestSnapshot, String> {
    let snaps = parse_snapshot_list(snapshots_json)?;
    let latest = choose_latest(&snaps, this_cluster_uid)?;
    Ok(LatestSnapshot {
        id: id_of(latest.commit),
        passed_over: latest.passed_over,
    })
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

/// A snapshot's time as restic reports it, or the empty string. For display
/// only: compare [`instant_of`].
fn time_of(s: &Value) -> String {
    s.get("time")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

/// A snapshot's time as an instant, which is what every comparison here uses.
///
/// restic records the time with the WRITER's UTC offset: the in-cluster
/// runner writes `…Z`, a `backup create` on a workstation writes its local
/// offset (`…+01:00`), and one repository can hold both. Their strings do not
/// order as their instants do — `2026-09-24T04:00:03+01:00` is half an hour
/// BEFORE `2026-09-24T03:30:00Z` — so comparing them picked the older run as
/// `latest` and hid a newer unfinished one.
///
/// `None` — no time, or one that does not parse — orders before every
/// instant, so such a snapshot never wins a `max_by_key`.
fn instant_of(s: &Value) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s.get("time").and_then(Value::as_str)?)
        .ok()
        .map(|t| t.with_timezone(&Utc))
}

/// A snapshot's `paths`, as restic reports them.
fn paths_of(s: &Value) -> Vec<String> {
    s.get("paths")
        .and_then(Value::as_array)
        .map(|p| {
            p.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// `latest` as [`choose_latest`] resolves it.
struct Latest<'a> {
    commit: &'a Value,
    passed_over: Vec<UnfinishedRun>,
}

/// The newest snapshot of [`latest_pool`] that completes its run — THE
/// definition of `latest` here, in one place so `restore` and `backup show`
/// cannot drift apart — and the unfinished runs of the pool newer than it.
///
/// Which snapshot completes its run is the prune's rule
/// ([`crate::prune::derive_manifest`]), applied to the run as
/// [`resolve_run_snapshots`] groups it: the snapshots sharing a tag, an
/// untagged snapshot being a run of its own. A snapshot that completes
/// nothing, in a run where nothing else does either, belongs to an
/// [`UnfinishedRun`].
///
/// "Newest" is by the clock, never by the time string ([`instant_of`]).
fn choose_latest<'a>(
    snaps: &'a [Value],
    this_cluster_uid: Option<&str>,
) -> Result<Latest<'a>, String> {
    let pool = latest_pool(snaps, this_cluster_uid)?;

    // How many snapshots of the whole listing carry each tag: a snapshot is
    // alone in its run when none of its tags is carried by another.
    let mut carried: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for s in snaps {
        for t in crate::cluster::snapshot_tags(s) {
            *carried.entry(t).or_default() += 1;
        }
    }
    let completes = |s: &Value| {
        let alone = crate::cluster::snapshot_tags(s)
            .iter()
            .all(|t| carried.get(t).copied().unwrap_or(0) <= 1);
        crate::prune::derive_manifest(&paths_of(s), alone)
    };

    let (complete, rest): (Vec<&Value>, Vec<&Value>) = pool.into_iter().partition(|s| completes(s));
    let commit = complete.iter().copied().max_by_key(|s| instant_of(s));

    // The other snapshots, by run. A complete run's own per-claim snapshots
    // are among them, but all are older than its commit — written last — so
    // none is newer than the run `latest` chose, which is all that is
    // reported; and with no complete run there are none.
    //
    // Each run carries its newest INSTANT beside the string it displays.
    let mut unfinished: std::collections::BTreeMap<String, (UnfinishedRun, Option<DateTime<Utc>>)> =
        std::collections::BTreeMap::new();
    for s in rest {
        let tag = crate::cluster::snapshot_tags(s)
            .first()
            .cloned()
            .unwrap_or_default();
        // An untagged snapshot is a run of its own.
        let key = if tag.is_empty() {
            id_of(s)
        } else {
            tag.clone()
        };
        let at = instant_of(s);
        let (run, newest_at) = unfinished.entry(key).or_insert_with(|| {
            (
                UnfinishedRun {
                    tag,
                    snapshots: 0,
                    newest: time_of(s),
                },
                at,
            )
        });
        run.snapshots += 1;
        if at > *newest_at {
            *newest_at = at;
            run.newest = time_of(s);
        }
    }

    let Some(commit) = commit else {
        let newest = unfinished
            .values()
            .max_by_key(|(_, at)| *at)
            .map(|(r, _)| r.newest.as_str());
        return Err(format!(
            "no complete backup run to choose: {} run(s) here, and none has the snapshot that \
             completes it, the one carrying manifest.json{} — each was interrupted before its \
             last snapshot, or is still being written. A sequential backup writes that \
             snapshot last. Wait for a backup in progress to finish, or take one \
             (`apprafter backup run`); `apprafter backup list` shows what the repository holds.",
            unfinished.len(),
            newest
                .map(|t| format!(" (the newest written at {t})"))
                .unwrap_or_default(),
        ));
    };
    let chosen = instant_of(commit);
    let mut passed_over: Vec<(UnfinishedRun, Option<DateTime<Utc>>)> = unfinished
        .into_values()
        .filter(|(_, at)| *at > chosen)
        .collect();
    passed_over.sort_by(|(_, a), (_, b)| b.cmp(a));
    Ok(Latest {
        commit,
        passed_over: passed_over.into_iter().map(|(r, _)| r).collect(),
    })
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
        // A pool of ONLY legacy snapshots is not a history of our own.
        //
        // `owned_by_this_cluster` is true for pre-identity snapshots as well as
        // ours — the accepted widening, and the right rule for prune, which must
        // not orphan them. Used as the gate HERE it reports a history this
        // cluster does not have: a `--reprovision` into a fresh target has no
        // snapshots carrying its own UID, so the pool collapsed to the legacy
        // ones and `latest` picked the newest of THOSE — silently passing over
        // the identified snapshot the operator had just taken. Found on a live
        // repository that held legacy snapshots plus one new identified run.
        //
        // So require at least one snapshot actually carrying our UID before
        // treating the pool as ours. Legacy still joins it once we have a
        // history — it just may not constitute one. When we have none, the
        // branch below is already correct: `cluster_uids_in` ignores legacy, so
        // a repository holding legacy plus ONE identified cluster is
        // unambiguous and every snapshot, legacy and identified alike, competes
        // for newest.
        let have_our_own = snaps.iter().any(|s| {
            matches!(
                crate::cluster::classify_snapshot(&crate::cluster::snapshot_tags(s), uid),
                crate::cluster::SnapshotOrigin::ThisCluster
            )
        });
        if have_our_own {
            return Ok(snaps
                .iter()
                .filter(|s| {
                    crate::cluster::owned_by_this_cluster(&crate::cluster::snapshot_tags(s), uid)
                })
                .collect());
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
        //
        // The newer run is a sequential one, spelled with the paths the writer
        // gives it: a two-snapshot run whose paths name neither a claim nor
        // the commit completes nothing, and `latest` never picks such a run.
        let two_runs = r#"[
          {"id":"old1","short_id":"old1","time":"2026-09-01T10:00:00Z","tags":["platform-run-0"],"paths":["/a/data"]},
          {"id":"new1","short_id":"new1","time":"2026-09-02T10:00:00Z","tags":["platform-run-1"],"paths":["/b/claim-0"]},
          {"id":"new2","short_id":"new2","time":"2026-09-02T10:00:01Z","tags":["platform-run-1"],"paths":["/b/commit"]}
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
        let id = resolve_latest_snapshot(&shared_listing(), Some(MINE))
            .unwrap()
            .id;
        assert_eq!(id, "mine1", "the newest in the repository is theirs");
    }

    /// DOES NOT FIRE: from the other side the same listing resolves to the
    /// other run — so the test above is not passing on a resolver that simply
    /// returns the oldest snapshot.
    #[test]
    fn resolve_latest_snapshot_from_the_other_side_resolves_to_that_cluster() {
        let id = resolve_latest_snapshot(&shared_listing(), Some(THEIRS))
            .unwrap()
            .id;
        assert_eq!(id, "theirs1");
    }

    /// The two entry points agree BY CONSTRUCTION — the property that makes
    /// `show` a trustworthy input to a `restore` decision. A second rule would
    /// let an operator inspect one snapshot and replay another.
    #[test]
    fn show_and_restore_resolve_latest_to_the_same_snapshot() {
        for uid in [Some(MINE), Some(THEIRS), None] {
            let shown = resolve_latest_snapshot(&shared_listing(), uid).map(|l| l.id);
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

    /// A repository holding legacy snapshots plus ONE identified cluster's is
    /// unambiguous, and `latest` means the newest of all of them.
    ///
    /// This is the upgrade shape, and it was wrong in the field: a cluster
    /// upgrades, takes a new (identified) backup, then `restore --reprovision`
    /// into a fresh target — which has no snapshots of its own — silently
    /// restored the newest LEGACY snapshot and passed over the run the operator
    /// had just taken. The pool collapsed to legacy-only because legacy counts
    /// as owned, and a legacy-only pool was being read as a history of our own.
    ///
    /// The test that stood here asserted the defect as the contract, which is
    /// why nothing caught it.
    #[test]
    fn latest_prefers_the_identified_run_over_an_older_legacy_one() {
        let mixed = format!(
            r#"[
              {{"id":"old","short_id":"old","time":"2026-09-01T03:00:00Z",
                "tags":["platform-2026-09-01T03:00:00Z"],"paths":["/s/data"]}},
              {{"id":"theirs","short_id":"theirs","time":"2026-09-02T03:00:00Z",
                "tags":["{THEIRS}-2026-09-02T03:00:00Z"],"paths":["/s/data"]}}
            ]"#
        );
        let r = resolve_run_snapshots(&mixed, "latest", Some(MINE)).unwrap();
        assert_eq!(
            r.commit, "theirs",
            "a fresh target must restore the newest run in an unambiguous \
             repository, not the newest pre-identity one"
        );
    }

    /// The other direction, so the fix cannot be read as "legacy never wins":
    /// once this cluster HAS a history of its own, legacy snapshots are still
    /// in the pool and a legacy run that is genuinely newest still wins. That
    /// is the accepted widening, and only the "legacy alone constitutes a
    /// history" reading was wrong.
    #[test]
    fn legacy_still_wins_for_a_cluster_that_has_its_own_history() {
        let mixed = format!(
            r#"[
              {{"id":"mine","short_id":"mine","time":"2026-09-01T03:00:00Z",
                "tags":["{MINE}-2026-09-01T03:00:00Z"],"paths":["/s/data"]}},
              {{"id":"old","short_id":"old","time":"2026-09-02T03:00:00Z",
                "tags":["platform-2026-09-02T03:00:00Z"],"paths":["/s/data"]}},
              {{"id":"theirs","short_id":"theirs","time":"2026-09-03T03:00:00Z",
                "tags":["{THEIRS}-2026-09-03T03:00:00Z"],"paths":["/s/data"]}}
            ]"#
        );
        let r = resolve_run_snapshots(&mixed, "latest", Some(MINE)).unwrap();
        assert_eq!(
            r.commit, "old",
            "legacy stays selectable; the foreign run is still excluded"
        );
    }

    // -----------------------------------------------------------------------
    // `latest` is the newest COMPLETE run: an unfinished one is passed over.
    // -----------------------------------------------------------------------

    /// A complete sequential run, then a NEWER one stopped after two of its
    /// claims — the shape a SIGINT, a kill or a deadline leaves: per-claim
    /// snapshots and no commit snapshot, so no `manifest.json` anywhere in it.
    fn interrupted_listing() -> String {
        let done = format!("{MINE}-2026-09-23T03:00:00Z");
        let cut = format!("{MINE}-2026-09-24T03:00:00Z");
        format!(
            r#"[
              {{"id":"d-claim0","short_id":"d-claim0","time":"2026-09-23T03:00:01Z",
                "tags":["{done}"],"paths":["/staging/apprafter-backup-a/claim-0"]}},
              {{"id":"d-claim1","short_id":"d-claim1","time":"2026-09-23T03:00:02Z",
                "tags":["{done}"],"paths":["/staging/apprafter-backup-a/claim-1"]}},
              {{"id":"d-commit","short_id":"d-commit","time":"2026-09-23T03:00:03Z",
                "tags":["{done}"],"paths":["/staging/apprafter-backup-a/commit"]}},
              {{"id":"c-claim0","short_id":"c-claim0","time":"2026-09-24T03:00:01Z",
                "tags":["{cut}"],"paths":["/tmp/apprafter-backup-b/claim-0"]}},
              {{"id":"c-claim1","short_id":"c-claim1","time":"2026-09-24T03:00:02Z",
                "tags":["{cut}"],"paths":["/tmp/apprafter-backup-b/claim-1"]}}
            ]"#
        )
    }

    /// FIRES: the newest snapshot belongs to a run that never wrote its commit
    /// snapshot. `latest` used to resolve to it, so `backup show` said the
    /// repository "holds something else" and a restore with no `--snapshot`
    /// found no manifest — until a newer complete run landed, which is the
    /// worst time to be told that: right after a backup died.
    #[test]
    fn latest_is_the_newest_complete_run_not_a_newer_unfinished_one() {
        let r = resolve_run_snapshots(&interrupted_listing(), "latest", Some(MINE)).unwrap();
        assert_eq!(r.commit, "d-commit", "the unfinished run has no manifest");
        assert_eq!(
            r.claims,
            vec!["d-claim0", "d-claim1"],
            "and the complete run's own claims come with it, not the unfinished run's"
        );
        let id = resolve_latest_snapshot(&interrupted_listing(), Some(MINE))
            .unwrap()
            .id;
        assert_eq!(id, "d-commit", "`backup show` resolves the same run");
    }

    /// …and the run it passed over is reported, not skipped in silence: the
    /// operator has to know the newest backup is not the one being restored.
    /// Only the unfinished run — the complete run's own claim snapshots are
    /// not "unfinished" merely for carrying no manifest themselves.
    #[test]
    fn latest_reports_the_newer_unfinished_run_it_passed_over() {
        let cut = UnfinishedRun {
            tag: format!("{MINE}-2026-09-24T03:00:00Z"),
            snapshots: 2,
            newest: "2026-09-24T03:00:02Z".into(),
        };
        let r = resolve_run_snapshots(&interrupted_listing(), "latest", Some(MINE)).unwrap();
        assert_eq!(r.passed_over, vec![cut.clone()]);
        let shown = resolve_latest_snapshot(&interrupted_listing(), Some(MINE)).unwrap();
        assert_eq!(
            shown.passed_over,
            vec![cut],
            "show and restore say the same"
        );
    }

    /// An unfinished run OLDER than the one chosen is not news: `latest` did
    /// not pass over it, and a note about last week's dead run on every
    /// restore is a note nobody reads.
    #[test]
    fn an_older_unfinished_run_is_not_reported() {
        let dead = format!("{MINE}-2026-09-20T03:00:00Z");
        let done = format!("{MINE}-2026-09-23T03:00:00Z");
        let listing = format!(
            r#"[
              {{"id":"x-claim0","time":"2026-09-20T03:00:01Z","tags":["{dead}"],
                "paths":["/s/a/claim-0"]}},
              {{"id":"mono","time":"2026-09-23T03:00:00Z","tags":["{done}"],
                "paths":["/s/b/data"]}}
            ]"#
        );
        let r = resolve_run_snapshots(&listing, "latest", Some(MINE)).unwrap();
        assert_eq!(r.commit, "mono");
        assert!(r.passed_over.is_empty(), "{:?}", r.passed_over);
    }

    /// A sequential run of ONE claim stopped before its commit is a lone
    /// `claim-0` snapshot. Alone in its run it still completes nothing — the
    /// prune's rule — so a monolithic run before it stays `latest`.
    #[test]
    fn a_lone_claim_snapshot_is_not_a_complete_run() {
        let done = format!("{MINE}-2026-09-23T03:00:00Z");
        let cut = format!("{MINE}-2026-09-24T03:00:00Z");
        let listing = format!(
            r#"[
              {{"id":"mono","time":"2026-09-23T03:00:00Z","tags":["{done}"],
                "paths":["/s/a/data"]}},
              {{"id":"lone","time":"2026-09-24T03:00:01Z","tags":["{cut}"],
                "paths":["/s/b/claim-0"]}}
            ]"#
        );
        let r = resolve_run_snapshots(&listing, "latest", Some(MINE)).unwrap();
        assert_eq!(r.commit, "mono");
        assert_eq!(r.passed_over.len(), 1);
        assert_eq!(r.passed_over[0].snapshots, 1);
    }

    /// The rule is the prune's, whole: in a run of several snapshots only the
    /// one under `commit` completes it, so a run whose paths name neither a
    /// claim nor the commit completes nothing, and the older run is `latest`.
    #[test]
    fn a_run_of_several_snapshots_with_no_commit_path_completes_nothing() {
        let done = format!("{MINE}-2026-09-23T03:00:00Z");
        let odd = format!("{MINE}-2026-09-24T03:00:00Z");
        let listing = format!(
            r#"[
              {{"id":"mono","time":"2026-09-23T03:00:00Z","tags":["{done}"],"paths":["/s/a/data"]}},
              {{"id":"odd1","time":"2026-09-24T03:00:01Z","tags":["{odd}"],"paths":["/s/b/data"]}},
              {{"id":"odd2","time":"2026-09-24T03:00:02Z","tags":["{odd}"],"paths":["/s/c/data"]}}
            ]"#
        );
        let r = resolve_run_snapshots(&listing, "latest", Some(MINE)).unwrap();
        assert_eq!(r.commit, "mono");
        assert_eq!(r.passed_over.len(), 1, "{:?}", r.passed_over);
    }

    /// Nothing complete at all — a first backup that died — is an error that
    /// says what the repository holds and what to do, not "no manifest".
    #[test]
    fn latest_with_no_complete_run_says_every_run_is_unfinished() {
        let cut = format!("{MINE}-2026-09-24T03:00:00Z");
        let listing = format!(
            r#"[
              {{"id":"c0","time":"2026-09-24T03:00:01Z","tags":["{cut}"],"paths":["/s/claim-0"]}},
              {{"id":"c1","time":"2026-09-24T03:00:02Z","tags":["{cut}"],"paths":["/s/claim-1"]}}
            ]"#
        );
        for err in [
            resolve_run_snapshots(&listing, "latest", Some(MINE)).unwrap_err(),
            resolve_latest_snapshot(&listing, Some(MINE)).unwrap_err(),
        ] {
            assert!(err.contains("no complete backup run"), "{err}");
            assert!(err.contains("1 run(s)"), "{err}");
            assert!(err.contains("manifest.json"), "{err}");
            assert!(err.contains("2026-09-24T03:00:02Z"), "{err}");
            assert!(err.contains("still being written"), "{err}");
        }
    }

    /// A snapshot named by id is honoured as it always was, and `latest`'s
    /// note does not follow it: nothing was passed over.
    #[test]
    fn a_named_snapshot_passes_nothing_over() {
        let r = resolve_run_snapshots(&interrupted_listing(), "d-commit", Some(MINE)).unwrap();
        assert_eq!(r.commit, "d-commit");
        assert!(r.passed_over.is_empty());
    }

    // -----------------------------------------------------------------------
    // Times are instants: restic keeps the WRITER's UTC offset.
    // -----------------------------------------------------------------------

    /// One repository, two writers. The in-cluster runner writes `…Z`; a
    /// `backup create` from a workstation an hour east of UTC writes
    /// `…+01:00` — the offset restic recorded in the field. The CLI's run
    /// here committed at `04:00:03.1+01:00`, which is 03:00:03.1 UTC: half
    /// an hour BEFORE the runner's 03:30 run, and the later string.
    fn cli_run(tag_time: &str) -> String {
        let tag = format!("{MINE}-{tag_time}");
        format!(
            r#"{{"id":"cli-claim0","time":"2026-09-24T04:00:01.1+01:00","tags":["{tag}"],
                 "paths":["/tmp/apprafter-backup-c/claim-0"]}},
               {{"id":"cli-commit","time":"2026-09-24T04:00:03.1+01:00","tags":["{tag}"],
                 "paths":["/tmp/apprafter-backup-c/commit"]}}"#
        )
    }

    /// FIRES: the newest complete run is the runner's, by the clock. Compared
    /// as strings, `2026-09-24T04…` beat `2026-09-24T03…`, and `latest`
    /// restored the CLI's older run.
    #[test]
    fn latest_orders_runs_by_instant_not_by_the_writers_offset() {
        let listing = format!(
            r#"[
              {{"id":"runner-mono","time":"2026-09-24T03:30:00.1Z",
                "tags":["{MINE}-2026-09-24T03:30:00Z"],"paths":["/staging/apprafter-backup-r/data"]}},
              {}
            ]"#,
            cli_run("2026-09-24T03:00:00Z")
        );
        let r = resolve_run_snapshots(&listing, "latest", Some(MINE)).unwrap();
        assert_eq!(r.commit, "runner-mono", "03:30Z is after 04:00:03+01:00");
        assert!(r.claims.is_empty(), "{:?}", r.claims);
        assert!(r.passed_over.is_empty(), "{:?}", r.passed_over);
        let shown = resolve_latest_snapshot(&listing, Some(MINE)).unwrap();
        assert_eq!(
            shown.id, "runner-mono",
            "`backup show` resolves the same run"
        );
    }

    /// FIRES: a runner run cut at 03:30Z is newer than the CLI run `latest`
    /// chose, and has to be named. Compared as strings it was "older" and
    /// silently dropped; and two passed-over runs are listed newest first by
    /// the clock, not by the string.
    #[test]
    fn a_newer_unfinished_run_is_reported_whatever_offset_either_writer_used() {
        let listing = format!(
            r#"[
              {}
              ,{{"id":"r-claim0","time":"2026-09-24T03:30:00.1Z",
                "tags":["{MINE}-2026-09-24T03:29:00Z"],"paths":["/staging/apprafter-backup-r/claim-0"]}}
              ,{{"id":"k-claim0","time":"2026-09-24T04:20:00+01:00",
                "tags":["{MINE}-2026-09-24T03:19:00Z"],"paths":["/tmp/apprafter-backup-k/claim-0"]}}
            ]"#,
            cli_run("2026-09-24T03:00:00Z")
        );
        let r = resolve_run_snapshots(&listing, "latest", Some(MINE)).unwrap();
        assert_eq!(r.commit, "cli-commit");
        let newest: Vec<&str> = r.passed_over.iter().map(|u| u.newest.as_str()).collect();
        assert_eq!(
            newest,
            vec!["2026-09-24T03:30:00.1Z", "2026-09-24T04:20:00+01:00"],
            "both are newer than 03:00:03Z, and 03:30Z is newer than 03:20Z"
        );
        let shown = resolve_latest_snapshot(&listing, Some(MINE)).unwrap();
        assert_eq!(
            shown.passed_over, r.passed_over,
            "show and restore say the same"
        );
    }

    /// The autumn clock change inside one run: `claim-0` at 02:59:50 CEST is
    /// 00:59:50 UTC, `claim-1` at 02:00:10 CET is 01:00:10 UTC. The claims
    /// come oldest first by the clock, and an unfinished run's newest time is
    /// the later instant, whatever its string says.
    fn across_the_clock_change(done: &str, run: &str, last: &str) -> String {
        format!(
            r#"{{"id":"{run}-claim0","time":"2026-10-25T02:59:50+02:00","tags":["{done}"],
                 "paths":["/tmp/apprafter-backup-{run}/claim-0"]}},
               {{"id":"{run}-claim1","time":"2026-10-25T02:00:10+01:00","tags":["{done}"],
                 "paths":["/tmp/apprafter-backup-{run}/claim-1"]}}{last}"#
        )
    }

    #[test]
    fn a_runs_claims_come_oldest_first_by_the_clock() {
        let tag = format!("{MINE}-2026-10-25T00:59:00Z");
        let commit = format!(
            r#",{{"id":"x-commit","time":"2026-10-25T02:00:20+01:00","tags":["{tag}"],
                  "paths":["/tmp/apprafter-backup-x/commit"]}}"#
        );
        let listing = format!("[{}]", across_the_clock_change(&tag, "x", &commit));
        let r = resolve_run_snapshots(&listing, "latest", Some(MINE)).unwrap();
        assert_eq!(r.commit, "x-commit");
        assert_eq!(r.claims, vec!["x-claim0", "x-claim1"]);
    }

    #[test]
    fn an_unfinished_runs_newest_time_is_the_later_instant() {
        let tag = format!("{MINE}-2026-10-25T00:59:00Z");
        let listing = format!(
            r#"[
              {{"id":"mono","time":"2026-10-24T03:00:00Z","tags":["{MINE}-2026-10-24T03:00:00Z"],
                "paths":["/s/a/data"]}},
              {}
            ]"#,
            across_the_clock_change(&tag, "y", "")
        );
        let r = resolve_run_snapshots(&listing, "latest", Some(MINE)).unwrap();
        assert_eq!(r.commit, "mono");
        assert_eq!(r.passed_over.len(), 1, "{:?}", r.passed_over);
        assert_eq!(r.passed_over[0].newest, "2026-10-25T02:00:10+01:00");
    }

    /// With nothing complete, the error names the newest unfinished run's
    /// time by the clock too.
    #[test]
    fn the_no_complete_run_error_names_the_newest_time_by_the_clock() {
        let listing = format!(
            r#"[
              {{"id":"r-claim0","time":"2026-09-24T03:30:00.1Z",
                "tags":["{MINE}-2026-09-24T03:29:00Z"],"paths":["/s/r/claim-0"]}},
              {{"id":"k-claim0","time":"2026-09-24T04:20:00+01:00",
                "tags":["{MINE}-2026-09-24T03:19:00Z"],"paths":["/s/k/claim-0"]}}
            ]"#
        );
        let err = resolve_run_snapshots(&listing, "latest", Some(MINE)).unwrap_err();
        assert!(
            err.contains("the newest written at 2026-09-24T03:30:00.1Z"),
            "{err}"
        );
    }

    /// A snapshot whose time does not parse is older than every one that
    /// does — as a missing time always was. As a string, `yesterday` sorted
    /// after every date and won.
    #[test]
    fn a_time_that_does_not_parse_never_wins_latest() {
        for bad in [r#""time":"yesterday","#, ""] {
            let listing = format!(
                r#"[
                  {{"id":"good","time":"2026-09-24T03:00:00Z",
                    "tags":["{MINE}-2026-09-24T03:00:00Z"],"paths":["/s/a/data"]}},
                  {{"id":"bad",{bad}"tags":["{MINE}-2026-09-25T03:00:00Z"],"paths":["/s/b/data"]}}
                ]"#
            );
            let r = resolve_run_snapshots(&listing, "latest", Some(MINE)).unwrap();
            assert_eq!(r.commit, "good", "bad time: {bad:?}");
        }
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
