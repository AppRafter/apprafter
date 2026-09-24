// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Real-restic proof that the stop's signal, passed on to a running restic,
//! gets its repository lock removed — and that without it (restic SIGKILLed
//! along with the runner, as before) the lock stays. The unit tests in
//! `src/restic_child.rs` drive fake children; only a real restic shows what
//! restic does with the signal.
//!
//! Skipped by default. Opt in with the restic the runner image ships (0.18):
//!
//! ```text
//! APPRAFTER_RESTIC_BIN=/path/to/restic \
//!     cargo test -p apprafter-backup --test restic_signal_test -- --ignored
//! ```
//!
//! Local repository in a temporary directory; no network. A few seconds.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use apprafter_backup::restic_child::{release_restic, ForwardingRestic, Released};
use backup_core::ResticRunner;

const PASSWORD: &str = "restic-signal-test";

fn restic_bin() -> PathBuf {
    // Explicitly opted in, so a missing binary is a FAILURE, not a skip.
    PathBuf::from(
        std::env::var_os("APPRAFTER_RESTIC_BIN")
            .expect("run with APPRAFTER_RESTIC_BIN=<path to a restic 0.18 binary>"),
    )
}

fn locks(bin: &Path, repo: &Path) -> Vec<String> {
    let out = Command::new(bin)
        .args(["list", "locks", "--no-lock", "--repo"])
        .arg(repo)
        .env("RESTIC_PASSWORD", PASSWORD)
        .output()
        .expect("run restic list locks");
    assert!(out.status.success(), "{out:?}");
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::to_string)
        .collect()
}

/// Start a `restic backup` that holds its lock for half a minute (it backs
/// up the output of a `sleep`), and wait until the lock is in the repository.
fn start_locked_backup(
    bin: &Path,
    repo: &Path,
) -> (
    apprafter_backup::restic_child::LiveResticChildren,
    std::thread::JoinHandle<cli_core::Result<Option<String>>>,
) {
    let r = ForwardingRestic::new(bin);
    let live = r.live_children();
    let argv: Vec<String> = [
        "backup",
        "--repo",
        repo.to_str().unwrap(),
        "--json",
        "--stdin-from-command",
        "--",
        "sleep",
        "30",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    let run = std::thread::spawn(move || r.run_backup(&argv, PASSWORD));
    let deadline = Instant::now() + Duration::from_secs(20);
    while locks(bin, repo).is_empty() {
        assert!(Instant::now() < deadline, "the backup never took its lock");
        std::thread::sleep(Duration::from_millis(100));
    }
    assert_eq!(live.running(), 1);
    (live, run)
}

#[test]
#[ignore = "needs a restic binary: APPRAFTER_RESTIC_BIN=<path>"]
fn the_stops_signal_gets_restics_lock_removed_and_a_kill_leaves_it() {
    let bin = restic_bin();
    let version = Command::new(&bin)
        .arg("version")
        .output()
        .expect("restic version");
    eprintln!("{}", String::from_utf8_lossy(&version.stdout).trim());
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().join("repo");
    let init = Command::new(&bin)
        .args(["init", "--repo"])
        .arg(&repo)
        .env("RESTIC_PASSWORD", PASSWORD)
        .output()
        .expect("restic init");
    assert!(init.status.success(), "{init:?}");
    let rt = tokio::runtime::Runtime::new().unwrap();

    // --- the stop passes SIGTERM on: restic removes its lock ----------------
    let (live, run) = start_locked_backup(&bin, &repo);
    let started = Instant::now();
    let released = rt.block_on(release_restic(
        &live,
        libc::SIGTERM,
        apprafter_backup::stop::RESTIC_RELEASE_BOUND,
    ));
    let took = started.elapsed();
    assert_eq!(released, Released::Exited(1));
    let err = run.join().unwrap().expect_err("a stopped backup fails");
    assert!(
        err.to_string().contains("signal terminated received"),
        "restic's own words: {err}"
    );
    assert_eq!(
        locks(&bin, &repo),
        Vec::<String>::new(),
        "restic removed its lock"
    );
    eprintln!("SIGTERM passed on: restic exited and removed its lock in {took:?}");

    // --- before: restic SIGKILLed with the runner — the lock stays ----------
    let (live, run) = start_locked_backup(&bin, &repo);
    assert_eq!(live.kill_remaining(), 1);
    assert!(run.join().unwrap().is_err());
    let left = locks(&bin, &repo);
    assert_eq!(left.len(), 1, "a killed restic leaves its lock: {left:?}");
    eprintln!("SIGKILL (the old behaviour): lock {} left behind", left[0]);
}
