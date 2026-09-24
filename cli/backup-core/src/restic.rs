// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Pure restic argv builders and passphrase resolution.
//!
//! The passphrase is NEVER passed on argv — it is always injected via the
//! `RESTIC_PASSWORD` environment variable at call time by the invoking layer.

use chrono::{DateTime, Utc};

/// `restic init` argv (RESTIC_PASSWORD passed via env, never argv).
pub fn restic_init_argv(repo: &str) -> Vec<String> {
    vec!["init".into(), "--repo".into(), repo.into()]
}

/// `restic backup` argv: snapshot the staging dir into the repo, tagged.
/// `--json` is included so the caller can parse the structured summary line.
///
/// `host`: when `Some(h)`, passes `--host h`, so the snapshot carries a
/// fixed, stable host — the cluster's name — rather than the pod or machine
/// name (spec §Retention M-r3-1a). `None` leaves restic to use the machine's
/// hostname.
pub fn restic_backup_argv(
    repo: &str,
    staging_dir: &str,
    tag: &str,
    host: Option<&str>,
) -> Vec<String> {
    let mut argv = vec![
        "backup".into(),
        "--repo".into(),
        repo.into(),
        "--tag".into(),
        tag.into(),
        "--json".into(),
    ];
    if let Some(h) = host {
        argv.push("--host".into());
        argv.push(h.into());
    }
    argv.push(staging_dir.into());
    argv
}

/// `restic snapshots --json` argv.
pub fn restic_snapshots_argv(repo: &str) -> Vec<String> {
    vec![
        "snapshots".into(),
        "--repo".into(),
        repo.into(),
        "--json".into(),
    ]
}

/// A time `restic snapshots --json` lists (a snapshot's `time`, or its
/// `summary.backup_end`) as an instant — which is what every comparison of
/// two snapshot times must use: which run is `latest`
/// ([`crate::restore`]), which run is the newest of its day, and how old a
/// run is ([`crate::prune`]).
///
/// restic records the time with the WRITER's UTC offset: the in-cluster
/// runner writes `…Z`, a `backup create` on a workstation writes its local
/// offset (`…+01:00`), and one repository can hold both. Their strings do not
/// order as their instants do — `2026-09-24T04:00:03+01:00` is half an hour
/// BEFORE `2026-09-24T03:30:00Z` — and one writer's strings do not either in
/// the hour a clock goes back (`02:30+02:00` comes before `02:10+01:00`). Nor
/// does a string's date say which day the run was on, anywhere but at its
/// writer.
///
/// `None` when the time does not parse as RFC 3339. `Option`'s order puts it
/// before every instant, so such a snapshot never wins a `max_by_key`.
pub fn snapshot_instant(time: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(time)
        .ok()
        .map(|t| t.with_timezone(&Utc))
}

/// `restic stats --json` argv, in `raw-data` mode.
///
/// `raw-data` is the mode that answers "what does this repository cost" —
/// bytes actually stored after dedup and compression. The default
/// `restore-size` mode answers a different question (what a restore would
/// write) and reports a number several times larger for a repository with
/// any history, which as a "size" line is just misleading.
///
/// `snapshot` narrows it to one snapshot; `None` covers the repository.
pub fn restic_stats_argv(repo: &str, snapshot: Option<&str>) -> Vec<String> {
    let mut argv = vec![
        "stats".into(),
        "--repo".into(),
        repo.into(),
        "--json".into(),
        "--mode".into(),
        "raw-data".into(),
    ];
    if let Some(id) = snapshot {
        argv.push(id.into());
    }
    argv
}

/// `restic ls --json <snapshot>` argv — one JSON object per line.
pub fn restic_ls_argv(repo: &str, snapshot: &str) -> Vec<String> {
    vec![
        "ls".into(),
        "--repo".into(),
        repo.into(),
        "--json".into(),
        snapshot.into(),
    ]
}

/// `restic dump <snapshot> <path>` argv — one file to stdout.
pub fn restic_dump_argv(repo: &str, snapshot: &str, path: &str) -> Vec<String> {
    vec![
        "dump".into(),
        "--repo".into(),
        repo.into(),
        snapshot.into(),
        path.into(),
    ]
}

/// `restic forget <ids...>` argv (retention, spec §M-r3-1b): remove exactly
/// these snapshots and nothing else. The run-aware planner in
/// [`crate::prune`] decides the set; [`restic_prune_argv`] reclaims the space
/// afterwards, as a SEPARATE command.
///
/// Never `--prune` here. restic 0.18.1 exits 0 from a `forget` whose deletes
/// the store refused (it prints `unable to remove snapshot/<id>` and goes
/// on), and with `--prune` it then prunes as if those snapshots were gone:
/// the blobs only they reference count as unused. Under a key that may not
/// delete snapshots, that prune writes a new index without them; under one
/// that may delete packs but not snapshots, it deletes data a snapshot still
/// in the repository needs. Measured on MinIO with a scoped key: a
/// `forget <id> --prune` left the snapshot in place and wrote a new index
/// object before failing, where a bare `forget <id>` changed nothing. So
/// [`crate::prune::run_prune`] forgets, checks the listing, and prunes only
/// once every snapshot it forgot is really gone. RESTIC_PASSWORD is passed
/// via env, never argv.
pub fn restic_forget_argv(repo: &str, ids: &[String]) -> Vec<String> {
    let mut argv = vec!["forget".into(), "--repo".into(), repo.into()];
    argv.extend(ids.iter().cloned());
    argv
}

/// `restic prune` argv: remove the data no snapshot in the repository refers
/// to any more. Safe on its own whatever `forget` did before it: it counts
/// as used everything the snapshots actually listed refer to.
pub fn restic_prune_argv(repo: &str) -> Vec<String> {
    vec!["prune".into(), "--repo".into(), repo.into()]
}

/// How much of the data a `restic check` reads, besides the structure.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CheckDepth {
    /// Structure and metadata only (`restic check`): reads no pack data.
    Structure,
    /// Every pack (`--read-data`).
    Full,
    /// A part of the packs (`--read-data-subset=<spec>`): `10%`, `n/t`, or a
    /// byte size, in restic's own grammar.
    Subset(String),
}

impl CheckDepth {
    /// The depth the chart's two knobs select: a full read wins over a
    /// subset, as `checkReadData` wins over `checkReadDataSubset`, and an
    /// empty subset means structure only.
    pub fn from_knobs(read_data: bool, subset: &str) -> Self {
        if read_data {
            CheckDepth::Full
        } else if subset.trim().is_empty() {
            CheckDepth::Structure
        } else {
            CheckDepth::Subset(subset.trim().to_string())
        }
    }
}

impl std::fmt::Display for CheckDepth {
    /// How a log line names the depth.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CheckDepth::Structure => f.write_str("structure only"),
            CheckDepth::Full => f.write_str("every pack read"),
            CheckDepth::Subset(spec) => write!(f, "a {spec} subset of the packs read"),
        }
    }
}

/// `restic check` argv at `depth`. RESTIC_PASSWORD is passed via env, never
/// argv.
pub fn restic_check_depth_argv(repo: &str, depth: &CheckDepth) -> Vec<String> {
    let mut argv = vec!["check".into(), "--repo".into(), repo.into()];
    match depth {
        CheckDepth::Structure => {}
        CheckDepth::Full => argv.push("--read-data".into()),
        CheckDepth::Subset(spec) => argv.push(format!("--read-data-subset={spec}")),
    }
    argv
}

/// What `restic stats --json --mode raw-data` says about a whole repository.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RepoStats {
    /// Bytes stored, after deduplication and compression.
    pub total_size: u64,
    /// Blobs the snapshots refer to — the number the restic index, and so the
    /// memory of every restic command that loads it, grows with.
    pub blob_count: Option<u64>,
    /// Snapshots in the repository.
    pub snapshots: Option<u64>,
}

/// Parse `restic stats --json --mode raw-data`. `None` when it is not JSON or
/// carries no size: a stats call that said nothing useful must not read as
/// an empty repository.
pub fn parse_repo_stats(raw: &str) -> Option<RepoStats> {
    let v: serde_json::Value = serde_json::from_str(raw.trim()).ok()?;
    Some(RepoStats {
        total_size: v.get("total_size")?.as_u64()?,
        blob_count: v
            .get("total_blob_count")
            .and_then(serde_json::Value::as_u64),
        snapshots: v.get("snapshots_count").and_then(serde_json::Value::as_u64),
    })
}

/// Does restic's stderr say the object store REFUSED a delete for lack of
/// permission? S3 answers `AccessDenied`, which restic prints as the
/// minio-go message `Access Denied.` (`Remove(<snapshot/…>) failed:
/// client.RemoveObject: Access Denied.`); a store that says `403 Forbidden`
/// instead is read the same way. Anything else — a timeout, a lost
/// connection — is not a refusal, and must not be reported as one.
pub fn delete_was_denied(stderr: &str) -> bool {
    let s = stderr.to_ascii_lowercase();
    s.contains("access denied") || s.contains("accessdenied") || s.contains("403 forbidden")
}

/// `restic restore <snapshot> --target <out>` argv.
pub fn restic_restore_argv(repo: &str, snapshot: &str, out: &str) -> Vec<String> {
    vec![
        "restore".into(),
        "--repo".into(),
        repo.into(),
        snapshot.into(),
        "--target".into(),
        out.into(),
    ]
}

/// `restic check` argv: verify the repository's structural integrity.
///
/// With `read_data = false` (the default) restic checks metadata + pack
/// consistency only — fast, no data download. With `read_data = true` it adds
/// `--read-data`, downloading and re-hashing every pack to catch bit-rot at the
/// cost of a full repo read (a deep, opt-in verify). RESTIC_PASSWORD is passed
/// via env, never argv.
pub fn restic_check_argv(repo: &str, read_data: bool) -> Vec<String> {
    let mut argv = vec!["check".into(), "--repo".into(), repo.into()];
    if read_data {
        argv.push("--read-data".into());
    }
    argv
}

/// `restic unlock` argv: removes only stale locks (restic's default behaviour —
/// no `--remove-all` flag, which would also kill live locks held by concurrent
/// backup runs).
pub fn restic_unlock_argv(repo: &str) -> Vec<String> {
    vec!["unlock".into(), "--repo".into(), repo.into()]
}

/// Passphrase precedence: explicit arg → env. Returns None when neither is set
/// (the caller then prompts on a TTY, or errors on non-TTY — the repo holds
/// decrypted secrets so an empty passphrase is NEVER allowed). The resolved
/// passphrase is passed to restic via the RESTIC_PASSWORD env at call time,
/// never on argv.
pub fn resolve_passphrase(arg: Option<&str>, env: Option<&str>) -> Option<String> {
    arg.or(env).map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backup_argv_targets_repo_and_tag() {
        // tag is opaque to argv here — generated by backup_tag() (T9) as
        // cluster-id + timestamp, NOT namespace-based.
        let a = restic_backup_argv("/repo", "/stage", "k3d-demo-2026-06-20T00:00:00Z", None);
        assert_eq!(a[0], "backup");
        assert!(a.contains(&"--repo".to_string()));
        assert!(a.contains(&"/repo".to_string()));
        assert!(a.contains(&"--tag".to_string()));
        assert!(a.contains(&"k3d-demo-2026-06-20T00:00:00Z".to_string()));
        assert!(a.contains(&"/stage".to_string()));
        assert!(a.contains(&"--json".to_string()));
    }

    #[test]
    fn backup_argv_sets_fixed_host_when_given_one() {
        let a = restic_backup_argv("s3:repo", "/stage", "tag", Some("apprafter-backup"));
        let i = a
            .iter()
            .position(|x| x == "--host")
            .expect("--host present");
        assert_eq!(a[i + 1], "apprafter-backup");
    }

    #[test]
    fn backup_argv_omits_host_when_none() {
        let a = restic_backup_argv("/local/repo", "/stage", "tag", None);
        assert!(
            !a.iter().any(|x| x == "--host"),
            "no host given, no --host (restic uses the machine's hostname): {a:?}"
        );
    }

    #[test]
    fn init_argv_and_restore_argv() {
        assert_eq!(restic_init_argv("/repo")[0], "init");
        let r = restic_restore_argv("/repo", "latest", "/out");
        assert_eq!(r[0], "restore");
        assert!(r.contains(&"latest".to_string()));
        assert!(r.windows(2).any(|w| w == ["--target", "/out"]));
    }

    #[test]
    fn forget_argv_lists_ids_and_never_prunes() {
        let ids = vec!["aaaa".to_string(), "bbbb".to_string()];
        let a = restic_forget_argv("s3:repo", &ids);
        assert_eq!(a, vec!["forget", "--repo", "s3:repo", "aaaa", "bbbb"]);
        // The prune is its own command, run only once the listing shows the
        // forgotten snapshots gone (see the argv's doc comment).
        assert!(!a.iter().any(|x| x == "--prune"), "{a:?}");
    }

    #[test]
    fn prune_argv_is_a_bare_prune() {
        assert_eq!(
            restic_prune_argv("s3:repo"),
            vec!["prune", "--repo", "s3:repo"]
        );
    }

    #[test]
    fn check_depth_follows_the_charts_two_knobs() {
        assert_eq!(CheckDepth::from_knobs(false, ""), CheckDepth::Structure);
        assert_eq!(CheckDepth::from_knobs(false, "  "), CheckDepth::Structure);
        assert_eq!(
            CheckDepth::from_knobs(false, "10%"),
            CheckDepth::Subset("10%".into())
        );
        // A full read wins, as checkReadData wins over checkReadDataSubset.
        assert_eq!(CheckDepth::from_knobs(true, "10%"), CheckDepth::Full);
        assert_eq!(
            restic_check_depth_argv("s3:x", &CheckDepth::Structure),
            vec!["check", "--repo", "s3:x"]
        );
        assert_eq!(
            restic_check_depth_argv("s3:x", &CheckDepth::Full),
            vec!["check", "--repo", "s3:x", "--read-data"]
        );
        assert_eq!(
            restic_check_depth_argv("s3:x", &CheckDepth::Subset("1/12".into())),
            vec!["check", "--repo", "s3:x", "--read-data-subset=1/12"]
        );
        assert_eq!(
            CheckDepth::Subset("10%".into()).to_string(),
            "a 10% subset of the packs read"
        );
    }

    #[test]
    fn repo_stats_read_the_raw_data_document_restic_prints() {
        // Verbatim from restic 0.18.1 against MinIO.
        let raw = r#"{"total_size":1202562,"total_uncompressed_size":1205256,"compression_ratio":1.0022402171364138,"compression_progress":100,"compression_space_saving":0.22352097811585425,"total_blob_count":12,"snapshots_count":4}"#;
        assert_eq!(
            parse_repo_stats(raw),
            Some(RepoStats {
                total_size: 1_202_562,
                blob_count: Some(12),
                snapshots: Some(4)
            })
        );
        assert_eq!(parse_repo_stats("not json"), None);
        assert_eq!(parse_repo_stats(r#"{"snapshots_count":4}"#), None);
    }

    #[test]
    fn a_refused_delete_is_told_apart_from_any_other_failure() {
        // Verbatim from restic 0.18.1 `forget <id>` under a MinIO policy that
        // denies DeleteObject outside locks/ (exit status 0).
        let refused = "Remove(<snapshot/ecd0be3219>) failed: client.RemoveObject: Access Denied.\n\
                       unable to remove snapshot/ecd0be3219c6a9 from the repository\n";
        assert!(delete_was_denied(refused));
        assert!(delete_was_denied(
            "Remove(<snapshot/x>) failed: AccessDenied"
        ));
        assert!(delete_was_denied(
            "unexpected HTTP response (403 Forbidden)"
        ));
        for other in [
            "Remove(<snapshot/x>) failed: dial tcp 10.0.0.1:443: i/o timeout",
            "unable to remove snapshot/x from the repository",
            "",
        ] {
            assert!(!delete_was_denied(other), "{other:?}");
        }
    }

    #[test]
    fn unlock_argv_targets_repo_and_removes_only_stale() {
        let a = restic_unlock_argv("s3:repo");
        assert_eq!(a[0], "unlock");
        assert!(a.iter().any(|x| x == "--repo") && a.iter().any(|x| x == "s3:repo"));
        assert!(
            !a.iter().any(|x| x == "--remove-all"),
            "must NOT remove live locks"
        );
    }

    #[test]
    fn check_argv_basic() {
        assert_eq!(
            restic_check_argv("s3:x", false),
            vec!["check", "--repo", "s3:x"]
        );
    }

    #[test]
    fn check_argv_read_data() {
        assert!(restic_check_argv("s3:x", true).contains(&"--read-data".to_string()));
    }

    #[test]
    fn passphrase_precedence_arg_then_env_then_none() {
        assert_eq!(
            resolve_passphrase(Some("p1"), Some("p2")),
            Some("p1".to_string())
        );
        assert_eq!(resolve_passphrase(None, Some("p2")), Some("p2".to_string()));
        assert_eq!(resolve_passphrase(None, None), None);
    }
}
