// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The back-off across runs (D.4b B12, B74, B88; GOTCHA-326): at most one failed
//! account-password check per ten minutes and two per hour, for this account, across every
//! AppRafter process.
//!
//! The record lives where the environment cannot move it: Linux `/run/user/<uid>` (logind
//! makes it for a login session, 0700, the account's), macOS the per-user temporary directory
//! (`confstr(_CS_DARWIN_USER_TEMP_DIR)`, which follows neither `HOME` nor `TMPDIR`). A place
//! that is missing, not a directory, a symlink, another uid's, or writable by its group or
//! others is no place for it: `Unavailable::NoBackoffRecord`, and no check runs — never an
//! unbounded one (B88). On Linux that is also the case under `su`, `sudo` or ssh without a
//! logind session.
//!
//! A check is counted before PAM is asked: [`Record::begin`] takes the record's exclusive lock
//! (another process's check is waited for at most [`BUSY_WAIT`], then refused
//! `AnotherCheckRunning`), re-reads it, refuses while a wait is in force, and writes the attempt
//! down as a failure, synced, before it returns. [`Attempt::not_counted`] takes it back when PAM
//! reported no failure; a crash, a failed write and every other end keep it. Every failure PAM
//! reports may count toward `pam_faillock` (GOTCHA-326), so the back-off fails closed; a
//! success does not erase earlier failures — they age out.
//!
//! The file: a header line, then one `failure <unix seconds>` line per failure of the last
//! hour. A time later than now counts as now (a clock set back). A record this crate did not
//! write — longer than [`RECORD_LIMIT`], not text, not its format — counts as one failure at its
//! last write (now, when that is later or unknown): it ages out like any failure, so it never
//! refuses every run (critic round). A program running as the person can delete it: the back-off
//! bounds a typo's and an agent's checks, it is no barrier against that program (ADR 0069
//! Risks).

use std::fmt::Write as _;
use std::fs::File;
use std::io::{self, Read, Seek, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::account::Account;
use crate::outcome::{BackoffState, BackoffWait, Unavailable, WaitWhy};

/// The record's file name in its directory.
pub const RECORD_FILE: &str = "apprafter-account-password-backoff";
/// After a failure, the next check waits this long.
pub const AFTER_A_FAILURE: Duration = Duration::from_secs(10 * 60);
/// At most [`FAILURES_PER_HOUR`] failures in this window.
pub const HOUR: Duration = Duration::from_secs(60 * 60);
pub const FAILURES_PER_HOUR: usize = 2;
/// How long [`Record::begin`] waits for another process's check to end.
pub const BUSY_WAIT: Duration = Duration::from_secs(10);

const HEADER: &str = "apprafter account-password back-off v1";
/// A record longer than this was not written by this crate: it counts as damaged.
const RECORD_LIMIT: u64 = 4096;

/// One account's record of failed checks, in a directory that is that account's alone.
#[derive(Debug, Clone)]
pub(crate) struct Record {
    path: PathBuf,
    uid: u32,
    busy_wait: Duration,
}

/// Why a check could not begin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Begin {
    Wait(BackoffWait),
    Unavailable(Unavailable),
}

/// One check, counted as a failure until [`Attempt::not_counted`] takes it back. Holds the
/// record's lock until it is dropped.
#[derive(Debug)]
pub(crate) struct Attempt {
    file: File,
    kept: Vec<u64>,
}

impl Record {
    /// This account's record in its system place (module docs).
    pub(crate) fn locate(account: &Account) -> Result<Record, Unavailable> {
        let dir = system_dir(account)?;
        Record::in_dir(&dir, account.uid)
    }

    /// The record in `dir`, when `dir` is a directory `uid` owns that no one else can write.
    pub(crate) fn in_dir(dir: &Path, uid: u32) -> Result<Record, Unavailable> {
        if !private_dir(dir, uid) {
            return Err(Unavailable::NoBackoffRecord {
                dir: dir.to_path_buf(),
            });
        }
        Ok(Record {
            path: dir.join(RECORD_FILE),
            uid,
            busy_wait: BUSY_WAIT,
        })
    }

    /// The back-off at `now`, read without the lock and without writing (a plan's read, B74). A
    /// record that is a link or not this account's file counts as one failure now.
    pub(crate) fn state(&self, now: SystemTime) -> BackoffState {
        let now = seconds(now);
        let failures = match read_record(&self.path, self.uid) {
            Ok(None) => Vec::new(),
            Ok(Some(on_disk)) => failures_in(&on_disk, now),
            Err(_) => vec![now],
        };
        match wait(&failures, now) {
            None => BackoffState::Clear,
            Some((until, why)) => BackoffState::Wait(BackoffWait {
                until: Some(time(until)),
                why,
            }),
        }
    }

    /// Counts one check before PAM is asked (module docs).
    pub(crate) fn begin(&self, now: SystemTime) -> Result<Attempt, Begin> {
        let no_place = || Begin::Unavailable(Unavailable::NoBackoffRecord { dir: self.dir() });
        let mut file = open_record(&self.path, self.uid).map_err(|_| no_place())?;
        if !lock(&file, self.busy_wait) {
            return Err(Begin::Wait(BackoffWait {
                until: None,
                why: WaitWhy::AnotherCheckRunning,
            }));
        }
        let now = seconds(now);
        let kept: Vec<u64> = failures_in(&read_on_disk(&file), now)
            .into_iter()
            .map(|t| t.min(now))
            .filter(|&t| now - t < HOUR.as_secs())
            .collect();
        if let Some((until, why)) = wait(&kept, now) {
            return Err(Begin::Wait(BackoffWait {
                until: Some(time(until)),
                why,
            }));
        }
        let mut counted = kept.clone();
        counted.push(now);
        write_record(&mut file, &counted).map_err(|_| no_place())?;
        Ok(Attempt { file, kept })
    }

    fn dir(&self) -> PathBuf {
        self.path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_default()
    }
}

impl Attempt {
    /// PAM reported no failure: the record as it was before this check. A failed write leaves
    /// the failure counted — a longer wait, never a shorter one.
    pub(crate) fn not_counted(mut self) {
        let _ = write_record(&mut self.file, &self.kept);
    }
}

/// When the next check may run, given the failures (seconds since the epoch) and `now`: the
/// later of the ten-minute and the hourly bound, or `None` when neither holds.
pub(crate) fn wait(failures: &[u64], now: u64) -> Option<(u64, WaitWhy)> {
    let mut recent: Vec<u64> = failures
        .iter()
        .map(|&t| t.min(now))
        .filter(|&t| now - t < HOUR.as_secs())
        .collect();
    recent.sort_unstable();
    let short = recent
        .last()
        .map(|&t| t + AFTER_A_FAILURE.as_secs())
        .filter(|&until| until > now)
        .map(|until| (until, WaitWhy::RecentFailure));
    let long = (recent.len() >= FAILURES_PER_HOUR).then(|| {
        let oldest = recent[recent.len() - FAILURES_PER_HOUR];
        (oldest + HOUR.as_secs(), WaitWhy::TwoInAnHour)
    });
    match (short, long) {
        (Some(a), Some(b)) if b.0 > a.0 => Some(b),
        (Some(a), _) => Some(a),
        (None, b) => b,
    }
}

/// The failures a record holds; `None` when it does not parse. A new, empty file holds none.
fn parse(text: &str) -> Option<Vec<u64>> {
    if text.is_empty() {
        return Some(Vec::new());
    }
    let mut lines = text.lines();
    if lines.next()? != HEADER {
        return None;
    }
    lines
        .map(|line| line.strip_prefix("failure ")?.parse().ok())
        .collect()
}

/// A record as read: its bytes (`None` when longer than [`RECORD_LIMIT`]: not this crate's)
/// and its last write.
struct OnDisk {
    bytes: Option<Vec<u8>>,
    modified: Option<SystemTime>,
}

/// The record behind `file`, from the position it is at (the start: the file was just opened).
fn read_on_disk(file: &File) -> OnDisk {
    let modified = file.metadata().and_then(|m| m.modified()).ok();
    let mut bytes = Vec::new();
    let fits = file
        .take(RECORD_LIMIT + 1)
        .read_to_end(&mut bytes)
        .is_ok_and(|n| n as u64 <= RECORD_LIMIT);
    OnDisk {
        bytes: fits.then_some(bytes),
        modified,
    }
}

/// The failures a record holds (module docs): one this crate did not write counts as one
/// failure at its last write, or now when that is later or unknown (critic round).
fn failures_in(on_disk: &OnDisk, now: u64) -> Vec<u64> {
    on_disk
        .bytes
        .as_deref()
        .and_then(|bytes| std::str::from_utf8(bytes).ok())
        .and_then(parse)
        .unwrap_or_else(|| vec![on_disk.modified.map_or(now, |at| seconds(at).min(now))])
}

/// The whole record, in place (the lock is on this file), synced.
fn write_record(file: &mut File, failures: &[u64]) -> io::Result<()> {
    let mut text = format!("{HEADER}\n");
    for failure in failures {
        let _ = writeln!(text, "failure {failure}");
    }
    file.set_len(0)?;
    file.rewind()?;
    file.write_all(text.as_bytes())?;
    file.sync_all()
}

fn seconds(at: SystemTime) -> u64 {
    at.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

fn time(seconds: u64) -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(seconds)
}

/// The record's exclusive lock, waited for at most `patience`.
fn lock(file: &File, patience: Duration) -> bool {
    let started = Instant::now();
    loop {
        match file.try_lock() {
            Ok(()) => return true,
            Err(std::fs::TryLockError::WouldBlock) if started.elapsed() < patience => {
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(_) => return false,
        }
    }
}

#[cfg(target_os = "macos")]
fn system_dir(_account: &Account) -> Result<PathBuf, Unavailable> {
    crate::macos::darwin_user_temp_dir().ok_or_else(|| Unavailable::NoBackoffRecord {
        dir: PathBuf::from("(the per-user temporary directory)"),
    })
}

#[cfg(all(unix, not(target_os = "macos")))]
fn system_dir(account: &Account) -> Result<PathBuf, Unavailable> {
    Ok(PathBuf::from(format!("/run/user/{}", account.uid)))
}

#[cfg(not(unix))]
fn system_dir(_account: &Account) -> Result<PathBuf, Unavailable> {
    Err(Unavailable::NotOnThisOs)
}

/// A directory (not a link) `uid` owns that neither its group nor others can write.
#[cfg(unix)]
fn private_dir(dir: &Path, uid: u32) -> bool {
    use std::os::unix::fs::MetadataExt;
    std::fs::symlink_metadata(dir)
        .is_ok_and(|m| m.is_dir() && m.uid() == uid && (m.mode() & 0o022) == 0)
}

#[cfg(not(unix))]
fn private_dir(_dir: &Path, _uid: u32) -> bool {
    false
}

/// The record for a plan's read: never through a link, refused unless it is a file `uid`
/// owns; `None` when there is none yet.
#[cfg(unix)]
fn read_record(path: &Path, uid: u32) -> io::Result<Option<OnDisk>> {
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    let mut options = std::fs::OpenOptions::new();
    options.read(true).custom_flags(libc::O_NOFOLLOW);
    let file = match options.open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    let meta = file.metadata()?;
    if !meta.is_file() || meta.uid() != uid {
        return Err(io::Error::other(
            "the back-off record is not this account's file",
        ));
    }
    Ok(Some(read_on_disk(&file)))
}

#[cfg(not(unix))]
fn read_record(_path: &Path, _uid: u32) -> io::Result<Option<OnDisk>> {
    Err(io::Error::from(io::ErrorKind::Unsupported))
}

/// The record, created 0600 if new, never through a link, and refused unless it is a file
/// `uid` owns.
#[cfg(unix)]
fn open_record(path: &Path, uid: u32) -> io::Result<File> {
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    let meta = file.metadata()?;
    if !meta.is_file() || meta.uid() != uid {
        return Err(io::Error::other(
            "the back-off record is not this account's file",
        ));
    }
    Ok(file)
}

#[cfg(not(unix))]
fn open_record(_path: &Path, _uid: u32) -> io::Result<File> {
    Err(io::Error::from(io::ErrorKind::Unsupported))
}

#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::fs::{symlink, PermissionsExt};

    use super::*;

    const T0: u64 = 1_800_000_000;

    fn at(seconds: u64) -> SystemTime {
        time(seconds)
    }

    fn uid() -> u32 {
        // SAFETY: getuid cannot fail and touches no memory.
        unsafe { libc::getuid() }
    }

    /// A directory this account alone can write, as logind makes `/run/user/<uid>`.
    fn private(dir: &Path) {
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    }

    fn scratch() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        private(dir.path());
        dir
    }

    fn record(dir: &Path) -> Record {
        Record::in_dir(dir, uid()).unwrap()
    }

    fn recent(until: u64) -> BackoffWait {
        BackoffWait {
            until: Some(at(until)),
            why: WaitWhy::RecentFailure,
        }
    }

    #[test]
    fn no_failure_no_wait() {
        assert_eq!(wait(&[], T0), None);
    }

    #[test]
    fn one_failure_waits_ten_minutes() {
        assert_eq!(wait(&[T0], T0), Some((T0 + 600, WaitWhy::RecentFailure)));
        assert_eq!(
            wait(&[T0], T0 + 599),
            Some((T0 + 600, WaitWhy::RecentFailure))
        );
        assert_eq!(wait(&[T0], T0 + 600), None);
    }

    #[test]
    fn two_failures_in_an_hour_wait_for_the_older_to_age() {
        let two = [T0, T0 + 700];
        assert_eq!(
            wait(&two, T0 + 700),
            Some((T0 + 3600, WaitWhy::TwoInAnHour))
        );
        assert_eq!(
            wait(&two, T0 + 1400),
            Some((T0 + 3600, WaitWhy::TwoInAnHour))
        );
        assert_eq!(wait(&two, T0 + 3600), None);
        assert_eq!(
            wait(&[T0 - 4000, T0], T0 + 600),
            None,
            "older than an hour: gone"
        );
    }

    #[test]
    fn a_failure_dated_after_now_counts_as_now() {
        assert_eq!(
            wait(&[T0 + 86_400], T0),
            Some((T0 + 600, WaitWhy::RecentFailure))
        );
    }

    #[test]
    fn a_new_record_allows_the_first_check() {
        let dir = scratch();
        let r = record(dir.path());
        assert_eq!(r.state(at(T0)), BackoffState::Clear);
        r.begin(at(T0)).unwrap().not_counted();
        assert_eq!(r.state(at(T0)), BackoffState::Clear);
    }

    #[test]
    fn an_attempt_is_counted_before_pam_and_kept_on_drop() {
        let dir = scratch();
        let r = record(dir.path());
        let attempt = r.begin(at(T0)).unwrap();
        assert_eq!(
            r.state(at(T0)),
            BackoffState::Wait(recent(T0 + 600)),
            "on disk while PAM runs"
        );
        drop(attempt);
        assert_eq!(r.state(at(T0 + 1)), BackoffState::Wait(recent(T0 + 600)));
        assert_eq!(
            r.begin(at(T0 + 1)).unwrap_err(),
            Begin::Wait(recent(T0 + 600))
        );
    }

    #[test]
    fn an_attempt_not_counted_leaves_the_record_as_it_was() {
        let dir = scratch();
        let r = record(dir.path());
        drop(r.begin(at(T0)).unwrap());
        r.begin(at(T0 + 700)).unwrap().not_counted();
        let text = std::fs::read_to_string(dir.path().join(RECORD_FILE)).unwrap();
        assert_eq!(text, format!("{HEADER}\nfailure {T0}\n"));
        assert_eq!(r.state(at(T0 + 700)), BackoffState::Clear);
    }

    /// Sets a record's last write: a damaged record's failure is dated by it.
    fn written_at(path: &Path, seconds: u64) {
        let file = std::fs::OpenOptions::new().write(true).open(path).unwrap();
        file.set_modified(at(seconds)).unwrap();
    }

    #[test]
    fn a_record_that_does_not_parse_counts_as_one_failure_at_its_last_write() {
        let dir = scratch();
        let path = dir.path().join(RECORD_FILE);
        std::fs::write(&path, "something else\n").unwrap();
        written_at(&path, T0);
        let r = record(dir.path());
        assert_eq!(r.state(at(T0)), BackoffState::Wait(recent(T0 + 600)));
        assert_eq!(
            r.begin(at(T0 + 1)).unwrap_err(),
            Begin::Wait(recent(T0 + 600))
        );
        assert_eq!(
            r.state(at(T0 + 600)),
            BackoffState::Clear,
            "it ages out like any failure"
        );
        r.begin(at(T0 + 600)).unwrap().not_counted();
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            text,
            format!("{HEADER}\nfailure {T0}\n"),
            "written back in the record's form"
        );
        std::fs::write(&path, "damaged again\n").unwrap();
        written_at(&path, T0 + 86_400);
        let now_counts = BackoffState::Wait(recent(T0 + 600));
        assert_eq!(
            r.state(at(T0)),
            now_counts,
            "a last write after now counts as now"
        );
    }

    #[test]
    fn a_runtime_dir_others_can_write_is_no_place_for_the_record() {
        let dir = scratch();
        let no_place = Unavailable::NoBackoffRecord {
            dir: dir.path().to_path_buf(),
        };
        for mode in [0o720, 0o702, 0o777] {
            std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(mode)).unwrap();
            assert_eq!(
                Record::in_dir(dir.path(), uid()).unwrap_err(),
                no_place,
                "{mode:o}"
            );
        }
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(
            Record::in_dir(dir.path(), uid()).is_ok(),
            "readable by others is fine"
        );
        assert!(Record::in_dir(&dir.path().join("missing"), uid()).is_err());
        assert!(
            Record::in_dir(dir.path(), uid() + 1).is_err(),
            "another account's directory"
        );
    }

    #[test]
    fn a_runtime_dir_that_is_a_symlink_is_no_place() {
        let dir = scratch();
        let real = dir.path().join("real");
        std::fs::create_dir(&real).unwrap();
        private(&real);
        let link = dir.path().join("link");
        symlink(&real, &link).unwrap();
        assert!(Record::in_dir(&real, uid()).is_ok());
        assert!(Record::in_dir(&link, uid()).is_err());
    }

    #[test]
    fn a_record_that_is_a_symlink_is_refused() {
        let dir = scratch();
        let elsewhere = dir.path().join("elsewhere");
        std::fs::write(&elsewhere, "").unwrap();
        symlink(&elsewhere, dir.path().join(RECORD_FILE)).unwrap();
        let no_place = Unavailable::NoBackoffRecord {
            dir: dir.path().to_path_buf(),
        };
        let r = record(dir.path());
        assert_eq!(r.begin(at(T0)).unwrap_err(), Begin::Unavailable(no_place));
        assert_eq!(
            r.state(at(T0)),
            BackoffState::Wait(recent(T0 + 600)),
            "the plan's read fails closed"
        );
    }

    #[test]
    fn a_record_too_long_to_be_ours_counts_as_a_failure_at_its_last_write() {
        let dir = scratch();
        let long = format!("{HEADER}\n{}", "failure 1\n".repeat(500));
        std::fs::write(dir.path().join(RECORD_FILE), long).unwrap();
        written_at(&dir.path().join(RECORD_FILE), T0);
        let r = record(dir.path());
        assert_eq!(r.state(at(T0)), BackoffState::Wait(recent(T0 + 600)));
        assert_eq!(r.begin(at(T0)).unwrap_err(), Begin::Wait(recent(T0 + 600)));
    }

    #[test]
    fn a_second_attempt_while_one_runs_waits_then_is_refused() {
        let dir = scratch();
        let mut r = record(dir.path());
        r.busy_wait = Duration::from_millis(300);
        let running = r.begin(at(T0)).unwrap();
        let started = Instant::now();
        let running_elsewhere = BackoffWait {
            until: None,
            why: WaitWhy::AnotherCheckRunning,
        };
        assert_eq!(r.begin(at(T0)).unwrap_err(), Begin::Wait(running_elsewhere));
        assert!(
            started.elapsed() >= Duration::from_millis(300),
            "it waited first"
        );
        running.not_counted();
        assert!(r.begin(at(T0)).is_ok(), "the lock goes with the attempt");
    }
}
