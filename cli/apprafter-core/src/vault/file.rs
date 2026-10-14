// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Reading and writing the vault file (D.4a T27, T28; crypto brief §9). Every path is an
//! argument; nothing here knows where a store lives.
//!
//! - [`read`] reads at most [`VAULT_FILE_MAX`] + 1 bytes; a missing file is `Missing`, never an
//!   empty vault.
//! - [`write_new`] writes the first vault (`lock on`) and never replaces one. Unix links the
//!   finished temp file to the vault's name (`link(2)` fails when the name exists), removes the
//!   temp name and fsyncs the directory — an fsync error fails the write, because clear data is
//!   deleted only after it. A filesystem without hard links (FAT, exFAT) is the typed refusal
//!   `NoReplaceUnsupported`, never a generic I/O error. Windows moves the temp file with
//!   `MoveFileExW(MOVEFILE_WRITE_THROUGH)` and without `MOVEFILE_REPLACE_EXISTING`: a
//!   same-volume move keeps the temp file's DACL and fails when the name exists.
//! - [`replace`] writes every later vault by `std::fs::rename` (never `ReplaceFileW`, which keeps
//!   the replaced file's DACL); the directory fsync is best effort.
//! - The temp file is owner-only before its first byte ([`cli_core::owner_only::create_new`]):
//!   mode 0600 at `open` on Unix (never a chmod afterwards); on Windows a protected DACL with
//!   one ACE, full access for the process token's user (`D:P(A;;FA;;;<user SID>)`), set on the
//!   open handle before anything is written. A wider mode or a foreign owner found later is a
//!   doctor warning, not a refusal: the contents are authenticated.
//! - Every write needs the target store's write lock, held (`WriteLockNotHeld`), and first
//!   removes the `.vault.*.tmp` files an interrupted writer left (they hold ciphertext only).

use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use cli_core::StoreLock;

use super::error::{MalformedWhat, VaultError, VaultResult};
use super::format::VAULT_FILE_MAX;
use super::rng::{OsRandom, VaultRng};

/// A temp vault's name: `.vault.<12 hex>.tmp`, beside the vault.
const TEMP_PREFIX: &str = ".vault.";
const TEMP_SUFFIX: &str = ".tmp";

#[cfg(all(test, unix))]
thread_local! {
    /// Tests: the next link fails with this errno instead of calling `link(2)`.
    static LINK_FAILS_WITH: std::cell::Cell<Option<i32>> = const { std::cell::Cell::new(None) };
    /// Tests: the next directory fsync fails.
    static SYNC_DIR_FAILS: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// Tests: the last temp file's mode, read from its open handle before its first byte.
    static TEMP_MODE_AT_OPEN: std::cell::Cell<Option<u32>> = const { std::cell::Cell::new(None) };
}

#[cfg(test)]
thread_local! {
    /// Tests: the next `replace` stops after its temp file is written and synced, as a crash
    /// between the fsync and the rename would, leaving the temp file behind.
    static STOP_BEFORE_RENAME: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// The vault file's bytes, at most [`VAULT_FILE_MAX`]; `Missing` when there is none.
pub fn read(path: &Path) -> VaultResult<Vec<u8>> {
    under_temp(path);
    let file = match File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return Err(VaultError::Missing {
                path: path.to_path_buf(),
            })
        }
        Err(e) => return Err(io_error(path, e)),
    };
    let mut bytes = Vec::new();
    file.take(VAULT_FILE_MAX as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| io_error(path, e))?;
    if bytes.len() > VAULT_FILE_MAX {
        return Err(VaultError::Malformed {
            what: MalformedWhat::TooLarge,
            offset: VAULT_FILE_MAX,
        });
    }
    Ok(bytes)
}

/// Writes the first vault at `path`; fails, and leaves what is there, when a file exists. An
/// `Err` after the link (a failed directory fsync) leaves the new vault in place: a caller
/// treats any `Err` as "the vault may exist" and reads the directory again (`lock on` resumes).
pub fn write_new(path: &Path, bytes: &[u8], held: &StoreLock) -> VaultResult<()> {
    under_temp(path);
    let dir = prepare(path, held)?;
    let tmp = write_temp(&dir, bytes)?;
    if let Err(e) = link_no_replace(&tmp, path) {
        let _ = fs::remove_file(&tmp);
        if no_hard_links(&e) {
            return Err(VaultError::NoReplaceUnsupported {
                fs: filesystem_name(&dir),
                dir,
            });
        }
        return Err(io_error(path, e));
    }
    #[cfg(unix)]
    {
        // The vault holds the bytes now; a temp name left by a failed removal is swept by the
        // next writer.
        let _ = fs::remove_file(&tmp);
        sync_dir(&dir).map_err(|e| io_error(&dir, e))?;
    }
    Ok(())
}

/// Replaces the vault at `path` with `bytes` in one rename.
pub fn replace(path: &Path, bytes: &[u8], held: &StoreLock) -> VaultResult<()> {
    under_temp(path);
    let dir = prepare(path, held)?;
    let tmp = write_temp(&dir, bytes)?;
    #[cfg(test)]
    if STOP_BEFORE_RENAME.with(|stop| stop.replace(false)) {
        return Err(io_error(path, io::Error::from(io::ErrorKind::Interrupted)));
    }
    if let Err(e) = fs::rename(&tmp, path) {
        let _ = fs::remove_file(&tmp);
        return Err(io_error(path, e));
    }
    #[cfg(unix)]
    let _ = sync_dir(&dir);
    Ok(())
}

/// The lock check and the sweep every write starts with; the vault's directory.
fn prepare(path: &Path, held: &StoreLock) -> VaultResult<PathBuf> {
    if !held.is_held() {
        return Err(VaultError::WriteLockNotHeld {
            path: path.to_path_buf(),
        });
    }
    let dir = match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
        _ => PathBuf::from("."),
    };
    remove_leftovers(&dir);
    Ok(dir)
}

/// Removes every `.vault.*.tmp` in `dir`, best effort: they hold ciphertext only, and the
/// caller holds the store's write lock, so none belongs to a write in progress.
fn remove_leftovers(dir: &Path) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if name.starts_with(TEMP_PREFIX) && name.ends_with(TEMP_SUFFIX) {
            let _ = fs::remove_file(entry.path());
        }
    }
}

/// An owner-only temp file in `dir` holding `bytes`, synced and closed.
fn write_temp(dir: &Path, bytes: &[u8]) -> VaultResult<PathBuf> {
    let (tmp, mut file) = create_temp(dir)?;
    #[cfg(all(test, unix))]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = file.metadata().ok().map(|m| m.permissions().mode() & 0o777);
        TEMP_MODE_AT_OPEN.with(|at_open| at_open.set(mode));
    }
    let written = file.write_all(bytes).and_then(|()| file.sync_all());
    drop(file);
    if let Err(e) = written {
        let _ = fs::remove_file(&tmp);
        return Err(io_error(&tmp, e));
    }
    Ok(tmp)
}

fn create_temp(dir: &Path) -> VaultResult<(PathBuf, File)> {
    for _ in 0..8 {
        let mut suffix = [0u8; 6];
        OsRandom.fill(&mut suffix)?;
        let hex: String = suffix.iter().map(|b| format!("{b:02x}")).collect();
        let path = dir.join(format!("{TEMP_PREFIX}{hex}{TEMP_SUFFIX}"));
        match cli_core::owner_only::create_new(&path) {
            Ok(file) => return Ok((path, file)),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(io_error(&path, e)),
        }
    }
    Err(io_error(
        dir,
        io::Error::new(io::ErrorKind::AlreadyExists, "no free temporary vault name"),
    ))
}

#[cfg(unix)]
fn link_no_replace(tmp: &Path, dest: &Path) -> io::Result<()> {
    #[cfg(test)]
    if let Some(errno) = LINK_FAILS_WITH.with(|fail| fail.take()) {
        return Err(io::Error::from_raw_os_error(errno));
    }
    fs::hard_link(tmp, dest)
}

#[cfg(windows)]
fn link_no_replace(tmp: &Path, dest: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{MoveFileExW, MOVEFILE_WRITE_THROUGH};
    let wide = |p: &Path| {
        p.as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect::<Vec<u16>>()
    };
    let (from, to) = (wide(tmp), wide(dest));
    // SAFETY: both are NUL-terminated UTF-16 paths that outlive the call. Without
    // MOVEFILE_REPLACE_EXISTING the move fails when `dest` exists.
    if unsafe { MoveFileExW(from.as_ptr(), to.as_ptr(), MOVEFILE_WRITE_THROUGH) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// `link(2)`'s answer on a filesystem that has no hard links: `EPERM` (Linux vfat, exFAT),
/// `EOPNOTSUPP` / `ENOTSUP`. The temp file is a regular file this process created and owns, so
/// no other reason for those applies.
#[cfg(unix)]
fn no_hard_links(e: &io::Error) -> bool {
    let code = e.raw_os_error();
    code == Some(libc::EPERM) || code == Some(libc::EOPNOTSUPP) || code == Some(libc::ENOTSUP)
}

/// A no-replace move needs no hard links.
#[cfg(windows)]
fn no_hard_links(_: &io::Error) -> bool {
    false
}

/// `fsync(2)` on the directory: what makes a link or a rename in it durable. Not
/// `File::sync_all`, which is `fcntl(F_FULLFSYNC)` on Apple targets, an operation a directory
/// descriptor need not accept; `EINTR` is retried, as std does.
#[cfg(unix)]
fn sync_dir(dir: &Path) -> io::Result<()> {
    use std::os::fd::AsRawFd;
    #[cfg(test)]
    if SYNC_DIR_FAILS.with(|fail| fail.replace(false)) {
        return Err(io::Error::other("injected directory fsync failure"));
    }
    let dir = File::open(dir)?;
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

/// The filesystem holding `dir`, for the refusal's message: `statfs(2)`'s magic number on
/// Linux, its type name on macOS.
#[cfg(target_os = "linux")]
fn filesystem_name(dir: &Path) -> Option<String> {
    use std::os::unix::ffi::OsStrExt;
    let path = std::ffi::CString::new(dir.as_os_str().as_bytes()).ok()?;
    let mut stat = std::mem::MaybeUninit::<libc::statfs>::zeroed();
    // SAFETY: `path` is NUL-terminated and `stat` is a valid place for one `statfs`.
    if unsafe { libc::statfs(path.as_ptr(), stat.as_mut_ptr()) } != 0 {
        return None;
    }
    // SAFETY: `statfs` returned 0, so it filled `stat`.
    let stat = unsafe { stat.assume_init() };
    #[allow(clippy::unnecessary_cast)] // `f_type`'s integer type differs between libcs.
    let magic = stat.f_type as u64;
    Some(match magic {
        0x4d44 => "vfat".to_string(),
        0x2011_bab0 => "exfat".to_string(),
        0x6573_5546 => "fuseblk".to_string(),
        other => format!("filesystem type {other:#x}"),
    })
}

#[cfg(target_os = "macos")]
fn filesystem_name(dir: &Path) -> Option<String> {
    use std::os::unix::ffi::OsStrExt;
    let path = std::ffi::CString::new(dir.as_os_str().as_bytes()).ok()?;
    let mut stat = std::mem::MaybeUninit::<libc::statfs>::zeroed();
    // SAFETY: `path` is NUL-terminated and `stat` is a valid place for one `statfs`.
    if unsafe { libc::statfs(path.as_ptr(), stat.as_mut_ptr()) } != 0 {
        return None;
    }
    // SAFETY: `statfs` returned 0, so it filled `stat`.
    let stat = unsafe { stat.assume_init() };
    let name: Vec<u8> = stat
        .f_fstypename
        .iter()
        .take_while(|c| **c != 0)
        .map(|c| *c as u8)
        .collect();
    Some(String::from_utf8_lossy(&name).into_owned())
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn filesystem_name(_: &Path) -> Option<String> {
    None
}

fn io_error(path: &Path, source: io::Error) -> VaultError {
    VaultError::Io {
        path: path.to_path_buf(),
        source,
    }
}

/// D.4a T6: in the core's own tests a vault file opens only under the temp dir — the deepest
/// existing part of `path`, canonicalised — so no test reaches a real store (the twin of
/// `secrets::files::under_temp`). Compiled out of every other build.
#[cfg(test)]
fn under_temp(path: &Path) {
    let tmp = fs::canonicalize(std::env::temp_dir()).unwrap();
    let existing = path
        .ancestors()
        .find(|a| a.exists())
        .and_then(|a| fs::canonicalize(a).ok());
    assert!(
        existing.is_some_and(|p| p.starts_with(&tmp)),
        "a vault test opened {path:?}, outside the temp dir (D.4a T6)"
    );
}

#[cfg(not(test))]
fn under_temp(_: &Path) {}

#[cfg(test)]
mod tests {
    use super::*;
    use cli_core::TargetStorePaths;

    /// A temp store root, its write lock (held), and the vault path in it.
    fn store() -> (tempfile::TempDir, StoreLock, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let lock =
            StoreLock::exclusive(&TargetStorePaths::for_root(dir.path().to_path_buf())).unwrap();
        assert!(lock.is_held());
        let vault = dir.path().join("vault");
        (dir, lock, vault)
    }

    /// The `.vault.*.tmp` names left in `dir`.
    fn temps(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(dir)
            .unwrap()
            .filter_map(|e| e.ok()?.file_name().into_string().ok())
            .filter(|n| n.starts_with(TEMP_PREFIX) && n.ends_with(TEMP_SUFFIX))
            .collect();
        names.sort();
        names
    }

    #[test]
    fn a_new_vault_is_written_once_and_never_replaced() {
        let (dir, lock, vault) = store();
        write_new(&vault, b"first", &lock).unwrap();
        assert_eq!(read(&vault).unwrap(), b"first");
        match write_new(&vault, b"second", &lock) {
            Err(VaultError::Io { source, .. }) => {
                assert_eq!(source.kind(), io::ErrorKind::AlreadyExists)
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(
            read(&vault).unwrap(),
            b"first",
            "the first vault is untouched"
        );
        assert!(temps(dir.path()).is_empty());
    }

    #[test]
    fn replace_swaps_the_whole_file() {
        let (dir, lock, vault) = store();
        write_new(&vault, b"first", &lock).unwrap();
        replace(&vault, b"second, longer", &lock).unwrap();
        assert_eq!(read(&vault).unwrap(), b"second, longer");
        assert!(temps(dir.path()).is_empty());
    }

    #[test]
    fn a_missing_vault_is_missing_and_a_huge_one_is_not_read() {
        let (dir, _lock, vault) = store();
        assert!(matches!(read(&vault), Err(VaultError::Missing { path }) if path == vault));
        fs::write(&vault, vec![0u8; VAULT_FILE_MAX + 1]).unwrap();
        assert!(matches!(
            read(&vault),
            Err(VaultError::Malformed {
                what: MalformedWhat::TooLarge,
                ..
            })
        ));
        drop(dir);
    }

    #[test]
    fn an_interrupted_replace_keeps_the_old_vault_and_the_next_writer_sweeps() {
        let (dir, lock, vault) = store();
        write_new(&vault, b"old", &lock).unwrap();
        STOP_BEFORE_RENAME.with(|stop| stop.set(true));
        assert!(matches!(
            replace(&vault, b"new", &lock),
            Err(VaultError::Io { .. })
        ));
        assert_eq!(read(&vault).unwrap(), b"old");
        assert_eq!(temps(dir.path()).len(), 1, "the crash left its temp file");
        fs::write(dir.path().join(".vault-notes"), b"not ours").unwrap();
        replace(&vault, b"new", &lock).unwrap();
        assert_eq!(read(&vault).unwrap(), b"new");
        assert!(temps(dir.path()).is_empty());
        assert!(
            dir.path().join(".vault-notes").exists(),
            "only .vault.*.tmp is swept"
        );
    }

    #[test]
    fn a_first_write_sweeps_leftovers_too() {
        let (dir, lock, vault) = store();
        fs::write(dir.path().join(".vault.abcdef.tmp"), b"ciphertext").unwrap();
        write_new(&vault, b"first", &lock).unwrap();
        assert!(temps(dir.path()).is_empty());
    }

    #[test]
    #[should_panic(expected = "outside the temp dir")]
    fn a_vault_test_never_reaches_outside_the_temp_dir() {
        let outside = std::env::temp_dir()
            .parent()
            .expect("the temp dir has a parent")
            .join("apprafter-not-in-temp")
            .join("vault");
        let _ = read(&outside);
    }

    /// Mode 0600 from `open`: read from the temp file's handle before its first byte (a chmod
    /// after writing would pass a check of the final mode alone), and on the vault after each
    /// write.
    #[cfg(unix)]
    #[test]
    fn the_vault_is_owner_only_from_its_first_byte() {
        use std::os::unix::fs::PermissionsExt;
        let mode = |path: &Path| fs::metadata(path).unwrap().permissions().mode() & 0o777;
        let (_dir, lock, vault) = store();
        write_new(&vault, b"first", &lock).unwrap();
        assert_eq!(
            TEMP_MODE_AT_OPEN.with(std::cell::Cell::take),
            Some(0o600),
            "write_new's temp file before its first byte"
        );
        assert_eq!(mode(&vault), 0o600);
        replace(&vault, b"second", &lock).unwrap();
        assert_eq!(
            TEMP_MODE_AT_OPEN.with(std::cell::Cell::take),
            Some(0o600),
            "replace's temp file before its first byte"
        );
        assert_eq!(mode(&vault), 0o600);
    }

    /// A store whose root this process cannot write holds no lock; the vault is then never
    /// written, wherever it is. Fails, never skips, where a 0555 directory binds nothing (root).
    #[cfg(unix)]
    #[test]
    fn writes_need_the_store_lock_held() {
        use std::os::unix::fs::PermissionsExt;
        let readonly = tempfile::tempdir().unwrap();
        fs::set_permissions(readonly.path(), fs::Permissions::from_mode(0o555)).unwrap();
        let unheld =
            StoreLock::exclusive(&TargetStorePaths::for_root(readonly.path().to_path_buf()))
                .unwrap();
        fs::set_permissions(readonly.path(), fs::Permissions::from_mode(0o755)).unwrap();
        assert!(
            !unheld.is_held(),
            "a 0555 store root was writable: run the tests as a user a directory mode binds"
        );
        let elsewhere = tempfile::tempdir().unwrap();
        let vault = elsewhere.path().join("vault");
        for write in [write_new, replace] {
            assert!(matches!(
                write(&vault, b"x", &unheld),
                Err(VaultError::WriteLockNotHeld { .. })
            ));
        }
        assert!(!vault.exists());
        assert!(temps(elsewhere.path()).is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn a_filesystem_without_hard_links_is_a_typed_refusal() {
        for errno in [libc::EPERM, libc::EOPNOTSUPP, libc::ENOTSUP] {
            let (dir, lock, vault) = store();
            LINK_FAILS_WITH.with(|fail| fail.set(Some(errno)));
            match write_new(&vault, b"first", &lock) {
                Err(VaultError::NoReplaceUnsupported { dir: at, fs }) => {
                    assert_eq!(at, dir.path());
                    assert!(fs.is_some_and(|name| !name.is_empty()), "errno {errno}");
                }
                other => panic!("errno {errno}: {other:?}"),
            }
            assert!(!vault.exists());
            assert!(temps(dir.path()).is_empty());
        }
        // Any other link failure is an I/O error, not this refusal.
        let (_dir, lock, vault) = store();
        LINK_FAILS_WITH.with(|fail| fail.set(Some(libc::EACCES)));
        assert!(matches!(
            write_new(&vault, b"first", &lock),
            Err(VaultError::Io { .. })
        ));
    }

    /// The write fails, and the linked vault stays: a caller reads the directory again and
    /// resumes (D.4a §3.9), so nothing may remove it on this error.
    #[cfg(unix)]
    #[test]
    fn a_first_write_fails_when_its_directory_cannot_be_synced() {
        let (dir, lock, vault) = store();
        SYNC_DIR_FAILS.with(|fail| fail.set(true));
        assert!(matches!(
            write_new(&vault, b"first", &lock),
            Err(VaultError::Io { .. })
        ));
        assert_eq!(read(&vault).unwrap(), b"first", "the linked vault stays");
        assert!(temps(dir.path()).is_empty());
    }

    #[cfg(windows)]
    #[test]
    fn a_vault_has_a_protected_dacl_with_one_ace_for_this_user() {
        use cli_core::owner_only::testing;
        let (_dir, lock, vault) = store();
        write_new(&vault, b"first", &lock).unwrap();
        testing::of_path(&vault)
            .unwrap()
            .assert_owner_only_file("the vault after write_new");
        replace(&vault, b"second", &lock).unwrap();
        testing::of_path(&vault)
            .unwrap()
            .assert_owner_only_file("the vault after replace");
    }
}
