// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Filesystem helpers for the files the CLI and AppRafter Desktop both
//! rewrite: the target store and `state.json`.

use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// Replace `dest` with `bytes` atomically: write a temp file next to it
/// (`<prefix>XXXXXX.tmp`), fsync it, then rename it over `dest` with
/// `std::fs::rename` — which on Windows retries a refused `MoveFileExW`
/// with POSIX rename semantics, so a reader holding `dest` open does not
/// make the replace fail (tempfile's `persist` does not retry). On Unix the
/// parent directory is fsynced afterwards (errors ignored). On error the
/// temp file is removed, on Windows too: the owner-only temp file's handle
/// shares removal by name ([`crate::owner_only::create_new`]).
///
/// `mode` is the file's mode on Unix, applied to the temp file before the
/// rename; `None` keeps tempfile's 0600. A file that is owner-only on Unix
/// (`None`, or a mode that grants nothing to group or others: `credentials.yaml`,
/// `state.json`) is owner-only on Windows too: its temp file gets the
/// protected one-ACE DACL ([`crate::owner_only`]) on its open handle before
/// its first byte, and the same-volume rename keeps it, so a store in a
/// folder other users can read keeps them out. A wider mode (0644) leaves
/// the Windows file what it inherits from its directory.
///
/// A reader sees the old file or the new one, never a partial write, and
/// never the new one under a wider mode than `mode`.
pub fn atomic_replace(
    dest: &Path,
    bytes: &[u8],
    prefix: &str,
    mode: Option<u32>,
) -> io::Result<()> {
    let parent = parent_of(dest)?;
    let path = synced_temp(parent, bytes, prefix, mode)?;
    if let Err(e) = std::fs::rename(&path, dest) {
        let _ = std::fs::remove_file(&path);
        return Err(e);
    }

    // The rename is durable once the directory entry is: fsync the parent.
    // Best-effort — a filesystem that cannot fsync a directory still did
    // the replace.
    #[cfg(unix)]
    if let Ok(dir) = std::fs::File::open(parent) {
        let _ = dir.sync_all();
    }
    Ok(())
}

/// [`atomic_replace`], durable before it returns (D.4b B78): for the files a
/// power cut must never roll back to a key the person was told is void — the
/// vault (`apprafter_core::vault::file::replace`, through [`rename_durable`])
/// and the `state.json` files a key change re-encrypts. The temp file is
/// written and synced as [`atomic_replace`]'s, owner-only from its first byte
/// under the same rule; then [`rename_durable`] moves it. An `Err` after the
/// move leaves `dest` holding the new bytes, not known to be durable: a caller
/// treats every `Err` as "either file may be there" and reports nothing done.
pub fn replace_durable(
    dest: &Path,
    bytes: &[u8],
    prefix: &str,
    mode: Option<u32>,
) -> io::Result<()> {
    let parent = parent_of(dest)?;
    let tmp = synced_temp(parent, bytes, prefix, mode)?;
    rename_durable(&tmp, dest)
}

/// Moves `tmp`, a synced file beside `dest`, over `dest` and returns once the
/// move is durable (D.4b B78): `rename(2)`, then `fsync(2)` on the directory —
/// an fsync error fails the call. When the rename fails, `tmp` is removed.
#[cfg(unix)]
pub fn rename_durable(tmp: &Path, dest: &Path) -> io::Result<()> {
    if let Err(e) = std::fs::rename(tmp, dest) {
        let _ = std::fs::remove_file(tmp);
        return Err(e);
    }
    sync_dir(parent_of(dest)?)
}

/// How long [`rename_durable`] retries a destination another program holds
/// open, on Windows: a holder's look or the desktop's read holds the vault for
/// a moment only.
#[cfg(windows)]
pub const BUSY_RETRY_FOR: std::time::Duration = std::time::Duration::from_secs(2);

/// Moves `tmp`, a synced file beside `dest`, over `dest` and returns once the
/// move is durable (D.4b B78): `MoveFileExW(MOVEFILE_REPLACE_EXISTING |
/// MOVEFILE_WRITE_THROUGH)`, which keeps `tmp`'s DACL. A destination another
/// program holds open — a sharing, access or lock violation, which a replacing
/// move meets even when that program shares deletion — is retried for
/// [`BUSY_RETRY_FOR`], then the call fails: never `std::fs::rename`, whose
/// fall-back for that case renames without write-through. When the move
/// fails, `tmp` is removed.
#[cfg(windows)]
pub fn rename_durable(tmp: &Path, dest: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::{
        ERROR_ACCESS_DENIED, ERROR_LOCK_VIOLATION, ERROR_SHARING_VIOLATION,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
    };
    const BUSY: [u32; 3] = [
        ERROR_SHARING_VIOLATION,
        ERROR_ACCESS_DENIED,
        ERROR_LOCK_VIOLATION,
    ];
    let wide = |p: &Path| {
        p.as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect::<Vec<u16>>()
    };
    let (from, to) = (wide(tmp), wide(dest));
    let flags = MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH;
    let until = std::time::Instant::now() + BUSY_RETRY_FOR;
    loop {
        #[cfg(any(test, feature = "test-support"))]
        testing::MOVES.with(|moves| moves.borrow_mut().push(flags));
        // SAFETY: both are NUL-terminated UTF-16 paths that outlive the call.
        if unsafe { MoveFileExW(from.as_ptr(), to.as_ptr(), flags) } != 0 {
            return Ok(());
        }
        let e = io::Error::last_os_error();
        let busy = e
            .raw_os_error()
            .is_some_and(|code| BUSY.contains(&(code as u32)));
        if !busy || std::time::Instant::now() >= until {
            let _ = std::fs::remove_file(tmp);
            return Err(e);
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

/// `dest`'s directory: `.` for a bare name.
fn parent_of(dest: &Path) -> io::Result<&Path> {
    match dest.parent() {
        Some(p) if p.as_os_str().is_empty() => Ok(Path::new(".")),
        Some(p) => Ok(p),
        None => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "{} has no parent directory to write next to",
                dest.display()
            ),
        )),
    }
}

/// A temp file `<prefix>XXXXXX.tmp` in `parent` holding `bytes`, synced and
/// closed, owner-only under [`atomic_replace`]'s rule: its path. On an error
/// nothing is left behind.
fn synced_temp(
    parent: &Path,
    bytes: &[u8],
    prefix: &str,
    mode: Option<u32>,
) -> io::Result<PathBuf> {
    let mut builder = tempfile::Builder::new();
    builder.prefix(prefix).suffix(".tmp");
    #[cfg(windows)]
    let mut tmp = if mode.is_none_or(|mode| mode & 0o077 == 0) {
        builder.make_in(parent, crate::owner_only::create_new)?
    } else {
        builder.tempfile_in(parent)?
    };
    #[cfg(not(windows))]
    let mut tmp = builder.tempfile_in(parent)?;
    #[cfg(all(test, windows))]
    crate::owner_only::testing::record_before_first_byte(tmp.as_file());
    tmp.write_all(bytes)?;
    #[cfg(test)]
    if WRITE_FAILS.with(std::cell::Cell::take) {
        return Err(io::Error::other("a forced write error"));
    }
    #[cfg(unix)]
    if let Some(mode) = mode {
        use std::os::unix::fs::PermissionsExt;
        tmp.as_file()
            .set_permissions(std::fs::Permissions::from_mode(mode))?;
    }
    tmp.as_file().sync_all()?;

    // Until here a failure drops `tmp`, which removes the temp file. From
    // here on the temp file is ours to remove: it is closed first, because
    // Windows will not rename a file this process still has open.
    let (file, path) = tmp.keep().map_err(|e| e.error)?;
    drop(file);
    Ok(path)
}

/// `fsync(2)` on the directory `dir`: what makes a rename in it durable. Not
/// `File::sync_all`, which is `fcntl(F_FULLFSYNC)` on Apple targets, an
/// operation a directory descriptor need not accept; `EINTR` is retried.
#[cfg(unix)]
fn sync_dir(dir: &Path) -> io::Result<()> {
    use std::os::fd::AsRawFd;
    #[cfg(any(test, feature = "test-support"))]
    if testing::DIR_SYNC_FAILS.with(|fail| fail.replace(false)) {
        return Err(io::Error::other("injected directory fsync failure"));
    }
    let dir = std::fs::File::open(dir)?;
    loop {
        // SAFETY: `dir` is an open descriptor for the duration of the call.
        if unsafe { libc::fsync(dir.as_raw_fd()) } == 0 {
            return Ok(());
        }
        let e = io::Error::last_os_error();
        if e.kind() != io::ErrorKind::Interrupted {
            return Err(e);
        }
    }
}

/// The seams of a durable replace's tests — this crate's, and through the
/// `test-support` feature the core's (`vault::file`) — never in a shipped
/// binary (`test-support` is enabled only from `[dev-dependencies]`). Per
/// thread: a test arms them and calls in the same thread.
#[cfg(any(test, feature = "test-support"))]
pub mod testing {
    #[cfg(unix)]
    use std::cell::Cell;
    #[cfg(windows)]
    use std::cell::RefCell;

    #[cfg(unix)]
    thread_local! {
        pub(super) static DIR_SYNC_FAILS: Cell<bool> = const { Cell::new(false) };
    }

    #[cfg(windows)]
    thread_local! {
        pub(super) static MOVES: RefCell<Vec<u32>> = const { RefCell::new(Vec::new()) };
    }

    /// This thread's next directory fsync of a durable replace fails (Unix).
    #[cfg(unix)]
    pub fn fail_next_dir_sync() {
        DIR_SYNC_FAILS.with(|fail| fail.set(true));
    }

    /// The flags of every `MoveFileExW` this thread's durable replaces made
    /// since the last call (Windows).
    #[cfg(windows)]
    pub fn take_moves() -> Vec<u32> {
        MOVES.with(|moves| std::mem::take(&mut *moves.borrow_mut()))
    }
}

#[cfg(test)]
thread_local! {
    /// A test's forced write error: this thread's next [`atomic_replace`] fails once its temp
    /// file holds the bytes, as a write to a full disk does.
    static WRITE_FAILS: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Names in `dir` other than `keep`.
    fn others(dir: &Path, keep: &str) -> Vec<String> {
        std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n != keep)
            .collect()
    }

    #[test]
    fn it_creates_the_file_and_then_replaces_it() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("f.yaml");
        atomic_replace(&dest, b"one", ".f.yaml.", None).unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), b"one");
        atomic_replace(&dest, b"two", ".f.yaml.", None).unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), b"two");
    }

    #[test]
    fn it_leaves_no_temp_file_behind() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("f.yaml");
        atomic_replace(&dest, b"one", ".f.yaml.", None).unwrap();
        atomic_replace(&dest, b"two", ".f.yaml.", None).unwrap();
        assert!(others(dir.path(), "f.yaml").is_empty());
    }

    #[test]
    fn a_failed_rename_removes_the_temp_file() {
        // `dest` is a non-empty directory: the temp file is written, and
        // the rename over it is refused on every platform.
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("occupied");
        std::fs::create_dir(&dest).unwrap();
        std::fs::write(dest.join("inside"), b"x").unwrap();
        atomic_replace(&dest, b"new", ".occupied.", None).expect_err("cannot replace a directory");
        assert!(
            others(dir.path(), "occupied").is_empty(),
            "temp file left behind"
        );
    }

    /// WI-474: a write that fails leaves no temp file. On Windows the owner-only temp file
    /// (`None`, 0600) is removed by tempfile's cleanup while its handle is still open, which
    /// that handle must share; a wider mode's temp file is tempfile's own.
    #[test]
    fn a_failed_write_leaves_no_temp_file() {
        let dir = tempfile::tempdir().unwrap();
        for (name, mode) in [("none", None), ("0600", Some(0o600)), ("0644", Some(0o644))] {
            let dest = dir.path().join(name);
            WRITE_FAILS.with(|fails| fails.set(true));
            let err = atomic_replace(&dest, b"secret", ".f.", mode).expect_err(name);
            assert_eq!(err.to_string(), "a forced write error", "{name}");
            let left = others(dir.path(), "");
            assert!(left.is_empty(), "{name}: left behind: {left:?}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn the_mode_is_applied_and_none_keeps_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;

        let open = dir.path().join("open");
        atomic_replace(&open, b"x", ".open.", Some(0o644)).unwrap();
        assert_eq!(mode(&open), 0o644);

        let secret = dir.path().join("secret");
        std::fs::write(&secret, b"old").unwrap();
        std::fs::set_permissions(&secret, std::fs::Permissions::from_mode(0o644)).unwrap();
        atomic_replace(&secret, b"x", ".secret.", None).unwrap();
        assert_eq!(mode(&secret), 0o600, "None must keep tempfile's 0600");
    }

    /// B78: a durable replace writes, then replaces, and leaves no temp file.
    #[test]
    fn a_durable_replace_creates_then_replaces_and_leaves_no_temp_file() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("vault");
        replace_durable(&dest, b"one", ".vault.", None).unwrap();
        replace_durable(&dest, b"two", ".vault.", None).unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), b"two");
        assert!(others(dir.path(), "vault").is_empty());
    }

    /// B78: a move that fails leaves no temp file.
    #[test]
    fn a_failed_durable_move_removes_the_temp_file() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("occupied");
        std::fs::create_dir(&dest).unwrap();
        std::fs::write(dest.join("inside"), b"x").unwrap();
        replace_durable(&dest, b"new", ".occupied.", None).expect_err("cannot replace a directory");
        assert!(
            others(dir.path(), "occupied").is_empty(),
            "temp file left behind"
        );
    }

    /// B78 on Unix: the rename is durable once its directory is synced, and a failed directory
    /// fsync fails the call; the new bytes are in place, not known to be durable.
    #[cfg(unix)]
    #[test]
    fn a_failed_directory_sync_fails_the_durable_replace() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("vault");
        replace_durable(&dest, b"old", ".vault.", None).unwrap();
        testing::fail_next_dir_sync();
        let err =
            replace_durable(&dest, b"new", ".vault.", None).expect_err("the directory sync failed");
        assert_eq!(err.to_string(), "injected directory fsync failure");
        assert_eq!(std::fs::read(&dest).unwrap(), b"new");
        assert!(others(dir.path(), "vault").is_empty());
    }

    /// The rule `atomic_replace` follows: owner-only without a mode.
    #[cfg(unix)]
    #[test]
    fn a_durable_replace_is_owner_only_without_a_mode() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("state.json");
        replace_durable(&dest, b"{}", ".state.json.", None).unwrap();
        let mode = std::fs::metadata(&dest).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    /// B78 on Windows: every move asks for write-through.
    #[cfg(windows)]
    #[test]
    fn a_durable_replace_moves_with_write_through() {
        use windows_sys::Win32::Storage::FileSystem::{
            MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
        };
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("vault");
        let _ = testing::take_moves();
        replace_durable(&dest, b"one", ".vault.", None).unwrap();
        replace_durable(&dest, b"two", ".vault.", None).unwrap();
        assert_eq!(
            testing::take_moves(),
            [MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH; 2]
        );
        assert_eq!(std::fs::read(&dest).unwrap(), b"two");
    }

    /// B78 on Windows: a destination another program holds open is retried for
    /// `BUSY_RETRY_FOR`, then the call fails and leaves the file as it was — whether that
    /// program shares nothing but reading, or deletion too (`File::open`'s sharing, where
    /// `std::fs::rename` would have gone on without write-through).
    #[cfg(windows)]
    #[test]
    fn a_destination_another_program_holds_fails_the_durable_replace_after_retrying() {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_SHARE_READ, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
        };
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("vault");
        std::fs::write(&dest, b"old").unwrap();
        for holder_kind in 0..2 {
            let holder = if holder_kind == 0 {
                std::fs::OpenOptions::new()
                    .read(true)
                    .share_mode(FILE_SHARE_READ)
                    .open(&dest)
                    .unwrap()
            } else {
                std::fs::File::open(&dest).unwrap()
            };
            let _ = testing::take_moves();
            let start = std::time::Instant::now();
            let err = replace_durable(&dest, b"new", ".vault.", None).expect_err("held open");
            let moves = testing::take_moves();
            assert!(
                start.elapsed() >= BUSY_RETRY_FOR,
                "holder {holder_kind}: the bound"
            );
            assert!(moves.len() > 1, "holder {holder_kind}: {moves:?}");
            assert!(
                moves
                    .iter()
                    .all(|f| *f == MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH),
                "holder {holder_kind}: {moves:?}"
            );
            assert!(
                matches!(err.raw_os_error(), Some(5 | 32 | 33)),
                "holder {holder_kind}: {err}"
            );
            drop(holder);
            assert_eq!(
                std::fs::read(&dest).unwrap(),
                b"old",
                "holder {holder_kind}"
            );
            assert!(
                others(dir.path(), "vault").is_empty(),
                "holder {holder_kind}"
            );
        }
    }

    /// WI-472: an owner-only file (`None`, or a mode that grants group and others nothing) gets
    /// the protected one-ACE DACL on its temp file before the first byte, keeps it through the
    /// rename, and replaces a file that inherited its folder's DACL.
    #[cfg(windows)]
    #[test]
    fn an_owner_only_file_has_the_owner_only_dacl_from_its_first_byte() {
        use crate::owner_only::testing;
        let dir = tempfile::tempdir().unwrap();
        for (name, mode) in [("none", None), ("0600", Some(0o600)), ("0400", Some(0o400))] {
            let dest = dir.path().join(name);
            std::fs::write(&dest, b"old, inherited").unwrap();
            let _ = testing::take_before_first_byte();
            atomic_replace(&dest, b"secret", ".f.", mode).unwrap();
            testing::take_before_first_byte()
                .expect("the write recorded its temp file's DACL")
                .unwrap()
                .assert_owner_only_file(&format!("{name}: the temp file before its first byte"));
            testing::of_path(&dest)
                .unwrap()
                .assert_owner_only_file(&format!("{name}: the file after the rename"));
            assert_eq!(std::fs::read(&dest).unwrap(), b"secret");
        }
        assert!(others(dir.path(), "").iter().all(|n| !n.ends_with(".tmp")));
    }

    /// A mode that lets group or others read is not a secret's: on Windows the file keeps what
    /// its directory hands down.
    #[cfg(windows)]
    #[test]
    fn a_wider_mode_keeps_what_the_directory_hands_down() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("config.yaml");
        atomic_replace(&dest, b"x", ".config.yaml.", Some(0o644)).unwrap();
        assert!(
            !crate::owner_only::testing::of_path(&dest)
                .unwrap()
                .protected
        );
    }

    /// The reason this helper exists: tempfile's `persist` fails with
    /// "Access is denied" when a reader holds the destination open.
    #[cfg(windows)]
    #[test]
    fn a_reader_holding_the_destination_open_does_not_stop_the_replace() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("f.yaml");
        std::fs::write(&dest, b"old").unwrap();
        let reader = std::fs::File::open(&dest).unwrap();
        atomic_replace(&dest, b"new", ".f.yaml.", None).unwrap();
        drop(reader);
        assert_eq!(std::fs::read(&dest).unwrap(), b"new");
    }
}
