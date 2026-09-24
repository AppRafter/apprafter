// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The staging volume's size limit, enforced by the runner itself.
//!
//! The chart mounts an `emptyDir` at `/staging` with
//! `sizeLimit: spec.backup.stagingSizeLimit`, points `TMPDIR` at it, and
//! passes the same limit in `APPRAFTER_BACKUP_STAGING_SIZE_LIMIT`. The run's
//! staging directory, restic's temporary files and restic's cache for the run
//! all live on that volume.
//!
//! The kubelet enforces the limit too, by evicting the pod, but late and
//! without saying why to anyone who reads the backup's status. It refreshes a
//! volume's usage once a minute by default and checks it every ten seconds,
//! so the dumps go on growing past the limit for up to a minute, and a run
//! shorter than that can finish over the limit unnoticed. Measured on kind
//! (WI-386): a run that staged 652 MiB against a 300Mi limit succeeded in 17 s
//! with no eviction, and a pod that stayed over its limit was evicted 32 s
//! after passing it and killed 2 s later, its own 90 s grace period ignored.
//! The most the runner could record in those two seconds is that Kubernetes
//! stopped it and its pod was "deleted or evicted".
//!
//! So the runner measures the volume itself, every [`POLL`], counting what
//! the kubelet counts ([`usage_bytes`]). When the volume holds more than the
//! limit it stops the run as a deadline would ([`crate::stop::stop_run_with`]):
//! the helper pods deleted, restic signalled so it removes its lock, and
//! `lastError` and the failure webhook given [`overrun_message`], which names
//! the limit and what to change. The kubelet's eviction stays as the
//! backstop.
//!
//! The check Job mounts the same volume with the same limit: there it holds
//! restic's cache and temporary files for the check and the prune after it,
//! and an overrun is recorded against the step it stopped
//! ([`check_overrun_message`]).
//!
//! # Exit code
//!
//! A run stopped this way exits [`EXIT_OVER_LIMIT`], not the 1 of any other
//! failure. The next attempt stages the same claims into the same limit, so
//! a retry cannot succeed: it dumps the databases and volumes again, holds
//! the node's disk and memory again, and posts the failure webhook again,
//! up to the Job's backoff limit — measured on kind, 7 attempts and 12
//! minutes against a 300Mi limit. Both Jobs carry a `podFailurePolicy` rule
//! that fails the Job on this exit code, so the first overrun ends it.

use std::collections::HashSet;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use backup_core::StagingMode;
use cli_core::quantity::humanise_bytes;

/// How often the runner measures its staging volume. Well inside the
/// kubelet's own cadence (usage refreshed each minute, checked every ten
/// seconds), so the runner is the one that stops an overrun, and a walk of
/// the volume — a few dump files and restic's cache — costs next to nothing.
pub const POLL: Duration = Duration::from_secs(2);

/// The exit code of a run stopped because its staging volume outgrew its
/// limit. The chart's `podFailurePolicy` on both Jobs fails the Job on it
/// (`FailJob`), so no attempt follows: see the module docs. Asserted against
/// the rendered chart by `scripts/check-backup-render.sh`.
pub const EXIT_OVER_LIMIT: i32 = 3;

/// Bytes the volume at `root` uses, counted the way the kubelet counts an
/// `emptyDir` against its `sizeLimit`: every entry's allocated blocks (512
/// bytes each, so a sparse file counts what it occupies), an inode with more
/// than one link once, and nothing on another device. An entry that vanishes
/// while it is being read counts nothing: the run deletes files as it goes.
pub fn usage_bytes(root: &Path) -> u64 {
    let Ok(top) = std::fs::symlink_metadata(root) else {
        return 0;
    };
    let device = top.dev();
    let mut linked: HashSet<u64> = HashSet::new();
    let mut total: u64 = 0;
    let mut pending = vec![root.to_path_buf()];
    while let Some(path) = pending.pop() {
        let Ok(meta) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        if meta.dev() != device {
            continue;
        }
        if meta.nlink() > 1 && !linked.insert(meta.ino()) {
            continue;
        }
        total = total.saturating_add(meta.blocks().saturating_mul(512));
        if meta.is_dir() {
            if let Ok(entries) = std::fs::read_dir(&path) {
                pending.extend(entries.flatten().map(|e| e.path()));
            }
        }
    }
    total
}

/// Whether a volume using `used` bytes is over a `limit` of that many:
/// strictly more, as the kubelet compares them. A volume exactly at its
/// limit fits.
pub fn is_over(used: u64, limit: u64) -> bool {
    used > limit
}

/// Measure `root` every `poll` until it holds more than `limit` bytes, and
/// return what it held then. Does not return while the volume fits.
pub async fn wait_for_overrun(root: PathBuf, limit: u64, poll: Duration) -> u64 {
    loop {
        tokio::time::sleep(poll).await;
        let dir = root.clone();
        let used = tokio::task::spawn_blocking(move || usage_bytes(&dir))
            .await
            .unwrap_or(0);
        if is_over(used, limit) {
            return used;
        }
    }
}

/// The `lastError` and webhook text for a run stopped because its staging
/// volume held `used` bytes, more than its `limit`.
pub fn overrun_message(used: u64, limit: u64, mode: StagingMode) -> String {
    let way_out = match mode {
        StagingMode::Monolithic => {
            "This run stages every claim's data before restic reads any of it; `apprafter \
             backup set staging-mode sequential` stages and uploads one claim at a time, so \
             only the largest has to fit. Otherwise raise spec.backup.stagingSizeLimit on the \
             PlatformStack"
        }
        StagingMode::Sequential => {
            "This run already stages one claim at a time, so the largest claim alone does not \
             fit: raise spec.backup.stagingSizeLimit on the PlatformStack"
        }
    };
    format!(
        "the staging volume held {}, more than its limit of {} (spec.backup.stagingSizeLimit), \
         so the run was stopped before Kubernetes evicts its pod. The volume holds the dumps \
         of the claims being backed up and restic's cache for the run. {way_out}, and check \
         that the node's disk has that much room",
        humanise_bytes(saturating_i64(used)),
        humanise_bytes(saturating_i64(limit)),
    )
}

/// [`overrun_message`] for the check Job, whose volume holds no dumps:
/// restic's cache for the check and the prune after it (the repository's
/// index and its tree packs) and the pack files a prune repacks.
pub fn check_overrun_message(used: u64, limit: u64) -> String {
    format!(
        "the staging volume held {}, more than its limit of {} (spec.backup.stagingSizeLimit), \
         so the run was stopped before Kubernetes evicts its pod. In the check Job the volume \
         holds restic's cache for the check and the prune after it, which grows with the \
         repository's index and trees rather than with its data, and the pack files a prune \
         rewrites. Raise spec.backup.stagingSizeLimit on the PlatformStack, and check that \
         the node's disk has that much room",
        humanise_bytes(saturating_i64(used)),
        humanise_bytes(saturating_i64(limit)),
    )
}

fn saturating_i64(n: u64) -> i64 {
    i64::try_from(n).unwrap_or(i64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    fn write_file(path: &Path, bytes: usize) {
        let mut f = std::fs::File::create(path).unwrap();
        f.write_all(&vec![0x5a; bytes]).unwrap();
        f.sync_all().unwrap();
    }

    /// Allocated blocks of one path, as `usage_bytes` counts them.
    fn blocks_of(path: &Path) -> u64 {
        std::fs::symlink_metadata(path).unwrap().blocks() * 512
    }

    #[test]
    fn it_counts_every_file_and_directory_under_the_root() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("apprafter-backup-x/data/pg/demo")).unwrap();
        std::fs::create_dir_all(root.join("restic-cache")).unwrap();
        let dump = root.join("apprafter-backup-x/data/pg/demo/shop.dump");
        let index = root.join("restic-cache/index-1");
        write_file(&dump, 3 * 1024 * 1024);
        write_file(&index, 200 * 1024);

        let used = usage_bytes(root);
        // Every entry is counted, not only the files: this is what the
        // kubelet sums, and the difference is the directories' own blocks.
        let mut want = 0;
        for p in [
            root.to_path_buf(),
            root.join("apprafter-backup-x"),
            root.join("apprafter-backup-x/data"),
            root.join("apprafter-backup-x/data/pg"),
            root.join("apprafter-backup-x/data/pg/demo"),
            dump.clone(),
            root.join("restic-cache"),
            index.clone(),
        ] {
            want += blocks_of(&p);
        }
        assert_eq!(used, want);
        assert!(used >= 3 * 1024 * 1024 + 200 * 1024, "{used}");
    }

    #[test]
    fn a_hard_linked_file_counts_once() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a");
        write_file(&a, 1024 * 1024);
        let once = usage_bytes(dir.path());
        std::fs::hard_link(&a, dir.path().join("b")).unwrap();
        assert_eq!(usage_bytes(dir.path()), once);
    }

    #[test]
    fn a_sparse_file_counts_what_it_occupies_not_its_length() {
        let dir = tempfile::tempdir().unwrap();
        let f = std::fs::File::create(dir.path().join("sparse")).unwrap();
        f.set_len(512 * 1024 * 1024).unwrap();
        drop(f);
        assert!(
            usage_bytes(dir.path()) < 1024 * 1024,
            "{}",
            usage_bytes(dir.path())
        );
    }

    #[test]
    fn a_symlink_is_not_followed() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        write_file(&outside.path().join("big"), 4 * 1024 * 1024);
        let root = dir.path().join("root");
        std::fs::create_dir(&root).unwrap();
        std::os::unix::fs::symlink(outside.path(), root.join("link")).unwrap();
        assert!(usage_bytes(&root) < 1024 * 1024, "{}", usage_bytes(&root));
    }

    #[test]
    fn a_missing_root_is_zero() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(usage_bytes(&dir.path().join("gone")), 0);
    }

    #[test]
    fn a_volume_exactly_at_its_limit_fits_and_one_byte_more_does_not() {
        assert!(!is_over(1024, 1024));
        assert!(is_over(1025, 1024));
        assert!(!is_over(0, 1024));
    }

    #[tokio::test]
    async fn the_watch_returns_once_the_volume_passes_the_limit_and_not_before() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let limit = 2 * 1024 * 1024;
        let watch = tokio::spawn(wait_for_overrun(
            root.clone(),
            limit,
            Duration::from_millis(20),
        ));

        // Under the limit: still watching, however long.
        write_file(&root.join("one"), 1024 * 1024);
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(!watch.is_finished(), "returned while the volume fit");

        // Past it: the watch reports what the volume held.
        write_file(&root.join("two"), 2 * 1024 * 1024);
        let used = tokio::time::timeout(Duration::from_secs(5), watch)
            .await
            .expect("the watch did not see the overrun")
            .unwrap();
        assert!(used > limit, "{used}");
        assert_eq!(used, usage_bytes(&root));
    }

    #[test]
    fn the_message_names_the_sizes_the_limit_and_the_way_out_for_each_mode() {
        let gi = 1024 * 1024 * 1024;
        let m = overrun_message(gi + gi / 2, gi, StagingMode::Monolithic);
        assert!(m.contains("held 1.5Gi"), "{m}");
        assert!(m.contains("limit of 1.0Gi"), "{m}");
        assert!(m.contains("spec.backup.stagingSizeLimit"), "{m}");
        assert!(
            m.contains("`apprafter backup set staging-mode sequential`"),
            "{m}"
        );
        assert!(m.contains("before Kubernetes evicts its pod"), "{m}");

        let s = overrun_message(3 * gi, 2 * gi, StagingMode::Sequential);
        assert!(s.contains("held 3.0Gi"), "{s}");
        assert!(s.contains("largest claim alone does not fit"), "{s}");
        // A sequential run is not told to switch to what it already is.
        assert!(!s.contains("staging-mode sequential"), "{s}");
    }

    /// The check Job stages no claims: its message is about restic's cache,
    /// and does not send anyone to a staging mode the check does not have.
    #[test]
    fn the_check_jobs_message_names_the_cache_and_the_limit() {
        let mi = 1024 * 1024;
        let m = check_overrun_message(320 * mi, 300 * mi);
        assert!(m.contains("held 320Mi"), "{m}");
        assert!(m.contains("limit of 300Mi"), "{m}");
        assert!(m.contains("spec.backup.stagingSizeLimit"), "{m}");
        assert!(
            m.contains("restic's cache for the check and the prune"),
            "{m}"
        );
        assert!(!m.contains("staging-mode"), "{m}");
        assert!(!m.contains("claim"), "{m}");
    }

    /// The overrun's exit code is its own: a plain failure (1) is retried by
    /// the Job and must stay so, a precondition error is 2, and a code above
    /// 128 reads as a signal.
    #[test]
    fn the_overrun_exits_with_a_code_of_its_own() {
        assert!(![0, 1, 2].contains(&EXIT_OVER_LIMIT));
        assert!((3..128).contains(&EXIT_OVER_LIMIT));
    }
}
